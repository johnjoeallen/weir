//! The filesystem sandbox (design section 7.6): a mount namespace entered with `pivot_root`,
//! created through an unprivileged user namespace. No root is needed, and `chroot(2)` is not
//! used because it is not a security boundary.
//!
//! Only the filesystem is covered. The network, the clock and other kernel resources are not
//! isolated. Linux only; if the namespaces cannot be created the caller exits 4 rather than
//! running unconfined.
//!
//! Known limits of this implementation:
//! - The interpreter is PID 1 of the new PID namespace but only reaps the children it spawns
//!   itself; a grandchild orphaned onto it stays a zombie until the run ends.
//! - Programs are exec'd from the interpreter's own namespace stage; the binary is dynamically
//!   linked, so the host libc must stay mapped (it is, since it is loaded before the pivot).

#[cfg(target_os = "linux")]
pub use linux::{enter, narrow, probe};

#[cfg(not(target_os = "linux"))]
pub use other::{enter, narrow, probe};

/// No sandbox exists off Linux (design section 19.4): the caller exits 4 unless `--no-sandbox`.
#[cfg(not(target_os = "linux"))]
mod other {
    use crate::ast::Mode;
    use crate::mounts::Mounts;
    use std::process::Command;

    pub fn enter(_mounts: &Mounts) -> Result<Option<i32>, String> {
        Err("the filesystem sandbox needs Linux namespaces and is not available on this platform".into())
    }

    pub fn narrow(_cmd: &mut Command, _sees: Vec<(String, Mode)>, _system: bool, _explicit: Vec<(String, String)>) {}

    pub fn probe() -> Result<(), String> {
        Err("the filesystem sandbox needs Linux namespaces and is not available on this platform".into())
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use crate::ast::Mode;
    use crate::mounts::{Mounts, SYSTEM_SET};
    use std::ffi::CString;
    use std::io;
    use std::os::raw::{c_char, c_int, c_long, c_ulong, c_void};
    use std::os::fd::AsFd;
    use std::os::unix::process::CommandExt;
    use std::process::Command;

    const CLONE_NEWNS: c_int = 0x0002_0000;
    const CLONE_NEWUSER: c_int = 0x1000_0000;
    const CLONE_NEWPID: c_int = 0x2000_0000;

    const MS_RDONLY: c_ulong = 1;
    const MS_NOSUID: c_ulong = 2;
    const MS_NODEV: c_ulong = 4;
    const MS_NOEXEC: c_ulong = 8;
    const MS_REMOUNT: c_ulong = 32;
    const MS_NOATIME: c_ulong = 1024;
    const MS_NODIRATIME: c_ulong = 2048;
    const MS_BIND: c_ulong = 4096;
    const MS_REC: c_ulong = 16384;
    const MS_PRIVATE: c_ulong = 1 << 18;
    const MS_RELATIME: c_ulong = 1 << 21;
    const MNT_DETACH: c_int = 2;

    #[cfg(target_arch = "x86_64")]
    const SYS_PIVOT_ROOT: c_long = 155;
    #[cfg(target_arch = "aarch64")]
    const SYS_PIVOT_ROOT: c_long = 41;

    extern "C" {
        fn unshare(flags: c_int) -> c_int;
        fn fork() -> c_int;
        fn waitpid(pid: c_int, status: *mut c_int, options: c_int) -> c_int;
        fn mount(src: *const c_char, target: *const c_char, fstype: *const c_char, flags: c_ulong, data: *const c_void) -> c_int;
        fn umount2(target: *const c_char, flags: c_int) -> c_int;
        fn syscall(num: c_long, ...) -> c_long;
        fn getuid() -> u32;
        fn getgid() -> u32;
        fn getpid() -> c_int;
        fn pipe(fds: *mut c_int) -> c_int;
        fn read(fd: c_int, buf: *mut c_void, n: usize) -> isize;
        fn write(fd: c_int, buf: *const c_void, n: usize) -> isize;
        fn close(fd: c_int) -> c_int;
        fn chdir(path: *const c_char) -> c_int;
        fn kill(pid: c_int, sig: c_int) -> c_int;
        fn signal(sig: c_int, handler: usize) -> usize;
        fn _exit(code: c_int) -> !;
    }

    fn cstr(s: &str) -> io::Result<CString> {
        CString::new(s).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, format!("path contains a NUL byte: {s:?}")))
    }

    fn do_mount(src: Option<&str>, target: &str, fstype: Option<&str>, flags: c_ulong, data: Option<&str>) -> Result<(), String> {
        let s = src.map(cstr).transpose().map_err(|e| e.to_string())?;
        let t = cstr(target).map_err(|e| e.to_string())?;
        let f = fstype.map(cstr).transpose().map_err(|e| e.to_string())?;
        let d = data.map(cstr).transpose().map_err(|e| e.to_string())?;
        let r = unsafe {
            mount(
                s.as_ref().map_or(std::ptr::null(), |c| c.as_ptr()),
                t.as_ptr(),
                f.as_ref().map_or(std::ptr::null(), |c| c.as_ptr()),
                flags,
                d.as_ref().map_or(std::ptr::null(), |c| c.as_ptr() as *const c_void),
            )
        };
        if r == 0 {
            Ok(())
        } else {
            Err(format!("mount {} on {target}: {}", src.unwrap_or("none"), io::Error::last_os_error()))
        }
    }

    /// One thing visible in a new root.
    struct Bind {
        /// Path in the namespace we are building from.
        src: String,
        /// Path in the new root.
        inside: String,
        mode: Mode,
        /// A system-set path that does not exist on this host is skipped instead of failing.
        optional: bool,
    }

    fn parse_mountinfo_flags(opts: &str) -> c_ulong {
        let mut f = 0;
        for o in opts.split(',') {
            f |= match o {
                "nosuid" => MS_NOSUID,
                "nodev" => MS_NODEV,
                "noexec" => MS_NOEXEC,
                "noatime" => MS_NOATIME,
                "nodiratime" => MS_NODIRATIME,
                "relatime" => MS_RELATIME,
                _ => 0,
            };
        }
        f
    }

    fn unescape_mountinfo(s: &str) -> String {
        let b = s.as_bytes();
        let mut out = Vec::with_capacity(b.len());
        let mut i = 0;
        while i < b.len() {
            if b[i] == b'\\' && i + 3 < b.len() && b[i + 1..i + 4].iter().all(|c| (b'0'..=b'7').contains(c)) {
                let v = (b[i + 1] - b'0') * 64 + (b[i + 2] - b'0') * 8 + (b[i + 3] - b'0');
                out.push(v);
                i += 4;
            } else {
                out.push(b[i]);
                i += 1;
            }
        }
        String::from_utf8_lossy(&out).into_owned()
    }

    /// Make `target` and every mount below it read-only. A recursive bind keeps submounts of the
    /// source writable, which would break a `ro` declaration.
    fn make_tree_readonly(target: &str) -> Result<(), String> {
        let info = std::fs::read_to_string("/proc/self/mountinfo").map_err(|e| format!("cannot read /proc/self/mountinfo: {e}"))?;
        let mut points: Vec<(String, c_ulong)> = Vec::new();
        for line in info.lines() {
            let f: Vec<&str> = line.split(' ').collect();
            if f.len() < 6 {
                continue;
            }
            let mp = unescape_mountinfo(f[4]);
            if mp == target || mp.starts_with(&format!("{target}/")) {
                points.push((mp, parse_mountinfo_flags(f[5])));
            }
        }
        for (mp, flags) in points {
            do_mount(None, &mp, None, MS_BIND | MS_REMOUNT | MS_RDONLY | flags | MS_NOSUID | MS_NODEV, None)?;
        }
        Ok(())
    }

    fn make_dir_or_file_target(src: &str, dst: &str) -> Result<(), String> {
        let meta = std::fs::metadata(src).map_err(|e| format!("{src}: {e}"))?;
        // Already there: the destination lies inside an earlier mount of the same host tree,
        // possibly a read-only one, where creating it would fail.
        if std::fs::metadata(dst).map(|d| d.is_dir() == meta.is_dir()).unwrap_or(false) {
            return Ok(());
        }
        if meta.is_dir() {
            std::fs::create_dir_all(dst).map_err(|e| format!("cannot create mount point {dst}: {e}"))
        } else {
            if let Some(parent) = std::path::Path::new(dst).parent() {
                std::fs::create_dir_all(parent).map_err(|e| format!("cannot create mount point {}: {e}", parent.display()))?;
            }
            std::fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .write(true)
                .open(dst)
                .map(|_| ())
                .map_err(|e| format!("cannot create mount point {dst}: {e}"))
        }
    }

    /// Build a new root at `root` (an existing empty directory) and pivot into it.
    /// `dev_proc_from` is where `/dev` nodes and `/proc` come from in the current namespace.
    fn build_and_enter(root: &str, binds: &[Bind], fresh_proc: bool) -> Result<(), String> {
        do_mount(None, "/", None, MS_REC | MS_PRIVATE, None)?;
        do_mount(Some("tmpfs"), root, Some("tmpfs"), MS_NOSUID | MS_NODEV, Some("mode=755"))?;

        // Fixed entries come first so that a declared mount under /tmp lands on top of them:
        // a private writable /tmp, a minimal /dev and /proc.
        let tmp = format!("{root}/tmp");
        std::fs::create_dir_all(&tmp).map_err(|e| format!("cannot create /tmp: {e}"))?;
        do_mount(Some("tmpfs"), &tmp, Some("tmpfs"), MS_NOSUID | MS_NODEV, Some("mode=1777"))?;

        let dev = format!("{root}/dev");
        std::fs::create_dir_all(&dev).map_err(|e| format!("cannot create /dev: {e}"))?;
        do_mount(Some("tmpfs"), &dev, Some("tmpfs"), MS_NOSUID, Some("mode=755"))?;
        for node in ["null", "zero", "urandom", "tty"] {
            let host = format!("/dev/{node}");
            let dst = format!("{dev}/{node}");
            if !std::path::Path::new(&host).exists() {
                continue;
            }
            if std::fs::File::create(&dst).is_err() {
                continue;
            }
            // /dev/tty fails to bind when the process has no controlling terminal; that is fine.
            let r = do_mount(Some(&host), &dst, None, MS_BIND, None);
            if r.is_err() && node != "tty" {
                return r;
            }
        }
        for (link, target) in [("fd", "/proc/self/fd"), ("stdin", "/proc/self/fd/0"), ("stdout", "/proc/self/fd/1"), ("stderr", "/proc/self/fd/2")] {
            let _ = std::os::unix::fs::symlink(target, format!("{dev}/{link}"));
        }

        let procd = format!("{root}/proc");
        std::fs::create_dir_all(&procd).map_err(|e| format!("cannot create /proc: {e}"))?;
        if fresh_proc {
            do_mount(Some("proc"), &procd, Some("proc"), MS_NOSUID | MS_NODEV | MS_NOEXEC, None)?;
        } else {
            do_mount(Some("/proc"), &procd, None, MS_BIND | MS_REC, None)?;
        }

        let depth = |p: &str| p.split('/').filter(|c| !c.is_empty()).count();
        let mut sorted: Vec<&Bind> = binds.iter().collect();
        sorted.sort_by_key(|b| depth(&b.inside));
        for b in sorted {
            if !std::path::Path::new(&b.src).exists() {
                if b.optional {
                    continue;
                }
                return Err(format!("the host path {} does not exist, so it cannot be mounted at {}", b.src, b.inside));
            }
            let dst = format!("{root}{}", b.inside);
            make_dir_or_file_target(&b.src, &dst)?;
            do_mount(Some(&b.src), &dst, None, MS_BIND | MS_REC, None)?;
            if b.mode == Mode::Ro {
                make_tree_readonly(&dst)?;
            } else {
                // keep nosuid/nodev; preserve the source's locked flags
                let flags = mount_flags_of(&dst);
                do_mount(None, &dst, None, MS_BIND | MS_REMOUNT | flags | MS_NOSUID | MS_NODEV, None)?;
            }
        }

        // pivot_root(".", ".oldroot"), then drop the old root.
        let old = format!("{root}/.oldroot");
        std::fs::create_dir_all(&old).map_err(|e| format!("cannot create the old-root mount point: {e}"))?;
        let (r, o) = (cstr(root).map_err(|e| e.to_string())?, cstr(&old).map_err(|e| e.to_string())?);
        let rc = unsafe { syscall(SYS_PIVOT_ROOT, r.as_ptr(), o.as_ptr()) };
        if rc != 0 {
            return Err(format!("pivot_root: {}", io::Error::last_os_error()));
        }
        let slash = cstr("/").map_err(|e| e.to_string())?;
        if unsafe { chdir(slash.as_ptr()) } != 0 {
            return Err(format!("chdir /: {}", io::Error::last_os_error()));
        }
        let oldp = cstr("/.oldroot").map_err(|e| e.to_string())?;
        if unsafe { umount2(oldp.as_ptr(), MNT_DETACH) } != 0 {
            return Err(format!("unmounting the old root: {}", io::Error::last_os_error()));
        }
        let _ = std::fs::remove_dir("/.oldroot");
        // The root itself is an empty read-only tmpfs holding only mount points.
        do_mount(None, "/", None, MS_BIND | MS_REMOUNT | MS_RDONLY | MS_NOSUID | MS_NODEV, None)?;
        let tmpc = cstr("/tmp").map_err(|e| e.to_string())?;
        unsafe { chdir(tmpc.as_ptr()) };
        Ok(())
    }

    fn mount_flags_of(path: &str) -> c_ulong {
        let Ok(info) = std::fs::read_to_string("/proc/self/mountinfo") else { return 0 };
        let mut best: Option<(usize, c_ulong)> = None;
        for line in info.lines() {
            let f: Vec<&str> = line.split(' ').collect();
            if f.len() < 6 {
                continue;
            }
            let mp = unescape_mountinfo(f[4]);
            if (path == mp || path.starts_with(&format!("{mp}/"))) && best.map_or(true, |(l, _)| mp.len() >= l) {
                best = Some((mp.len(), parse_mountinfo_flags(f[5])));
            }
        }
        best.map_or(0, |(_, f)| f)
    }

    fn why_unshare_failed(e: &io::Error) -> String {
        let mut s = format!("{e}");
        let max = std::fs::read_to_string("/proc/sys/user/max_user_namespaces").unwrap_or_default();
        let clone = std::fs::read_to_string("/proc/sys/kernel/unprivileged_userns_clone").unwrap_or_default();
        if max.trim() == "0" {
            s.push_str("; user.max_user_namespaces is 0, so user namespaces are disabled on this host");
        } else if clone.trim() == "0" {
            s.push_str("; kernel.unprivileged_userns_clone is 0, so unprivileged user namespaces are disabled on this host");
        } else if std::path::Path::new("/proc/sys/kernel/apparmor_restrict_unprivileged_userns").exists()
            && std::fs::read_to_string("/proc/sys/kernel/apparmor_restrict_unprivileged_userns").map(|v| v.trim() == "1").unwrap_or(false)
        {
            s.push_str("; AppArmor restricts unprivileged user namespaces on this host (kernel.apparmor_restrict_unprivileged_userns=1)");
        } else {
            s.push_str("; the host or a container runtime forbids creating user namespaces (seccomp or a container without them)");
        }
        s
    }

    static mut CHILD_PID: c_int = 0;

    extern "C" fn forward_signal(sig: c_int) {
        // Async-signal-safe: only kill(2).
        unsafe {
            let pid = CHILD_PID;
            if pid > 0 {
                kill(pid, sig);
            }
        }
    }

    extern "C" fn init_exit(sig: c_int) {
        // PID 1 of the namespace: leaving takes every process in it down with us.
        unsafe { _exit(128 + sig) }
    }

    /// Enter the sandbox described by `mounts`. `Ok(None)`: this process is now inside it.
    /// `Ok(Some(code))`: this is the original process, which waited for the sandboxed one and
    /// should exit with `code`.
    pub fn enter(mounts: &Mounts) -> Result<Option<i32>, String> {
        #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
        {
            let _ = mounts;
            return Err("the sandbox is not implemented for this CPU architecture".into());
        }
        #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
        {
            let uid = unsafe { getuid() };
            let gid = unsafe { getgid() };

            // Everything that needs the host happens before the fork.
            let root = std::env::temp_dir().join(format!("weir-sandbox-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&root).map_err(|e| format!("cannot create {}: {e}", root.display()))?;
            let root_s = root.to_string_lossy().into_owned();

            let binds: Vec<Bind> = mounts
                .entries
                .iter()
                .filter(|e| e.origin != "fixed")
                .map(|e| Bind { src: e.host.clone(), inside: e.hidden.clone().unwrap_or_else(|| e.inside.clone()), mode: e.mode, optional: e.origin == "system" })
                .collect();

            use std::io::Write as _;
            let _ = std::io::stdout().flush();
            let _ = std::io::stderr().flush();

            // The mount namespace is created in the child only: `pivot_root` moves the root of every
            // process sharing the namespace, and the parent must stay on the host to clean up.
            let rc = unsafe { unshare(CLONE_NEWUSER | CLONE_NEWPID) };
            if rc != 0 {
                let e = io::Error::last_os_error();
                let _ = std::fs::remove_dir_all(&root);
                return Err(format!("unshare: {}", why_unshare_failed(&e)));
            }
            let setup = (|| -> Result<(), String> {
                std::fs::write("/proc/self/setgroups", "deny").map_err(|e| format!("setgroups: {e}"))?;
                std::fs::write("/proc/self/uid_map", format!("{uid} {uid} 1")).map_err(|e| format!("writing uid_map: {e}"))?;
                std::fs::write("/proc/self/gid_map", format!("{gid} {gid} 1")).map_err(|e| format!("writing gid_map: {e}"))?;
                Ok(())
            })();
            if let Err(e) = setup {
                let _ = std::fs::remove_dir_all(&root);
                return Err(e);
            }

            // The first fork after unshare(CLONE_NEWPID) becomes PID 1 of the new namespace.
            let pid = unsafe { fork() };
            if pid < 0 {
                let _ = std::fs::remove_dir_all(&root);
                return Err(format!("fork: {}", io::Error::last_os_error()));
            }
            if pid > 0 {
                // Parent: forward termination signals and wait for the sandboxed interpreter.
                unsafe {
                    CHILD_PID = pid;
                    for sig in [1, 2, 15] {
                        signal(sig, forward_signal as extern "C" fn(c_int) as usize);
                    }
                }
                let mut status: c_int = 0;
                loop {
                    let r = unsafe { waitpid(pid, &mut status, 0) };
                    if r == pid {
                        break;
                    }
                    if r < 0 && io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
                        break;
                    }
                }
                let _ = std::fs::remove_dir_all(&root);
                let code = if status & 0x7f == 0 { (status >> 8) & 0xff } else { 128 + (status & 0x7f) };
                return Ok(Some(code));
            }

            // Child: PID 1. Take a private mount namespace, build the root and pivot.
            if unsafe { unshare(CLONE_NEWNS) } != 0 {
                eprintln!("weir: the sandbox could not be created: unshare mount namespace: {}", io::Error::last_os_error());
                unsafe { _exit(4) }
            }
            unsafe {
                signal(2, init_exit as extern "C" fn(c_int) as usize);
                signal(15, init_exit as extern "C" fn(c_int) as usize);
                signal(1, init_exit as extern "C" fn(c_int) as usize);
            }
            if let Err(e) = build_and_enter(&root_s, &binds, true) {
                eprintln!("weir: the sandbox could not be created: {e}");
                unsafe { _exit(4) }
            }
            Ok(None)
        }
    }

    /// Report a probe failure to the parent through `fd` and leave.
    fn probe_fail(fd: c_int, msg: &str) -> ! {
        unsafe {
            write(fd, msg.as_ptr() as *const c_void, msg.len());
            _exit(1)
        }
    }

    /// Find out whether `enter` would succeed here, without changing this process: a forked child
    /// builds and pivots into an empty root the way `enter` does and exits. Used by `--sandbox auto`,
    /// which must decide before the interpreter takes a PID namespace it cannot leave.
    pub fn probe() -> Result<(), String> {
        #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
        {
            return Err("the sandbox is not implemented for this CPU architecture".into());
        }
        #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
        {
            let (uid, gid) = unsafe { (getuid(), getgid()) };
            let root = std::env::temp_dir().join(format!("weir-probe-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&root).map_err(|e| format!("cannot create {}: {e}", root.display()))?;
            let root_s = root.to_string_lossy().into_owned();
            let mut fds = [0 as c_int; 2];
            if unsafe { pipe(fds.as_mut_ptr()) } != 0 {
                let _ = std::fs::remove_dir_all(&root);
                return Err(format!("pipe: {}", io::Error::last_os_error()));
            }
            use std::io::Write as _;
            let _ = std::io::stdout().flush();
            let _ = std::io::stderr().flush();
            let pid = unsafe { fork() };
            if pid < 0 {
                let _ = std::fs::remove_dir_all(&root);
                return Err(format!("fork: {}", io::Error::last_os_error()));
            }
            if pid == 0 {
                let w = fds[1];
                unsafe { close(fds[0]) };
                if unsafe { unshare(CLONE_NEWUSER | CLONE_NEWPID) } != 0 {
                    probe_fail(w, &format!("unshare: {}", why_unshare_failed(&io::Error::last_os_error())));
                }
                let maps = (|| -> Result<(), String> {
                    std::fs::write("/proc/self/setgroups", "deny").map_err(|e| format!("setgroups: {e}"))?;
                    std::fs::write("/proc/self/uid_map", format!("{uid} {uid} 1")).map_err(|e| format!("writing uid_map: {e}"))?;
                    std::fs::write("/proc/self/gid_map", format!("{gid} {gid} 1")).map_err(|e| format!("writing gid_map: {e}"))?;
                    Ok(())
                })();
                if let Err(e) = maps {
                    probe_fail(w, &e);
                }
                let init = unsafe { fork() };
                if init < 0 {
                    probe_fail(w, &format!("fork: {}", io::Error::last_os_error()));
                }
                if init > 0 {
                    let mut status: c_int = 0;
                    unsafe { waitpid(init, &mut status, 0) };
                    unsafe { _exit(if status == 0 { 0 } else { 1 }) }
                }
                if unsafe { unshare(CLONE_NEWNS) } != 0 {
                    probe_fail(w, &format!("unshare mount namespace: {}", io::Error::last_os_error()));
                }
                if let Err(e) = build_and_enter(&root_s, &[], true) {
                    probe_fail(w, &e);
                }
                unsafe { _exit(0) }
            }
            unsafe { close(fds[1]) };
            let mut msg: Vec<u8> = Vec::new();
            let mut buf = [0u8; 512];
            loop {
                let n = unsafe { read(fds[0], buf.as_mut_ptr() as *mut c_void, buf.len()) };
                if n > 0 {
                    msg.extend_from_slice(&buf[..n as usize]);
                } else if n == 0 || io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
                    break;
                }
            }
            unsafe { close(fds[0]) };
            let mut status: c_int = 0;
            unsafe { waitpid(pid, &mut status, 0) };
            let _ = std::fs::remove_dir_all(&root);
            if status == 0 && msg.is_empty() {
                Ok(())
            } else if msg.is_empty() {
                Err("the test sandbox exited without a message".into())
            } else {
                Err(String::from_utf8_lossy(&msg).into_owned())
            }
        }
    }

    /// Narrow what one program sees (a filter's `see` lines, or the view of a `within` block): the program starts in
    /// a nested mount namespace holding only the listed inside paths. Call only inside the sandbox.
    pub fn narrow(cmd: &mut Command, sees: Vec<(String, Mode)>, system: bool, explicit: Vec<(String, String)>) {
        let mut binds: Vec<Bind> = Vec::new();
        if system {
            for p in SYSTEM_SET {
                binds.push(Bind { src: p.to_string(), inside: p.to_string(), mode: Mode::Ro, optional: true });
            }
        }
        for (p, m) in sees {
            // A explicit mount is held out of the root's view, so its source is the holding path.
            let src = explicit
                .iter()
                .find(|(inside, _)| p == *inside || p.strip_prefix(inside.as_str()).is_some_and(|r| r.starts_with('/')))
                .map_or_else(|| p.clone(), |(inside, hidden)| format!("{hidden}{}", &p[inside.len()..]));
            binds.push(Bind { src, inside: p, mode: m, optional: false });
        }
        // `pre_exec` can only carry an errno back to the parent, so the reason is written to a
        // copy of the original stderr (close-on-exec) taken before the command redirects its own.
        let report = std::io::stderr().as_fd().try_clone_to_owned().ok().map(std::fs::File::from);
        unsafe {
            cmd.pre_exec(move || {
                let fail = |msg: String, errno: Option<i32>| -> io::Error {
                    if let Some(mut f) = report.as_ref().and_then(|f| f.try_clone().ok()) {
                        use std::io::Write as _;
                        let _ = writeln!(f, "weir: could not narrow the program's view of the filesystem: {msg}");
                    }
                    match errno {
                        Some(e) => io::Error::from_raw_os_error(e),
                        None => io::Error::from_raw_os_error(22),
                    }
                };
                let pid = getpid();
                if unshare(CLONE_NEWNS) != 0 {
                    let e = io::Error::last_os_error();
                    return Err(fail(format!("unshare: {e}"), e.raw_os_error()));
                }
                let root = format!("/tmp/.weir-see-{pid}");
                if let Err(e) = std::fs::create_dir_all(&root) {
                    return Err(fail(format!("creating {root}: {e}"), e.raw_os_error()));
                }
                build_and_enter(&root, &binds, false).map_err(|e| fail(e, None))
            });
        }
    }
}
