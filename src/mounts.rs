//! The mount table: which host folders the script sees, where, and in which mode.

use crate::ast::Mode;

#[derive(Debug, Clone)]
pub struct MountEntry {
    pub host: String,
    pub inside: String,
    pub mode: Mode,
    /// "declared", "system" or "fixed"
    pub origin: &'static str,
    /// Not part of the default view: only a `see` line or a `within` header that names it reaches it.
    pub explicit: bool,
    /// Where a explicit mount is held inside the sandbox, out of every program's default view.
    pub hidden: Option<String>,
}

/// A explicit mount is bound under this folder in the sandbox root, which no program's view includes.
pub const EXPLICIT_DIR: &str = "/.weir-explicit";

#[derive(Debug, Clone, Default)]
pub struct Mounts {
    pub entries: Vec<MountEntry>,
}

pub const SYSTEM_SET: &[&str] = &[
    "/usr",
    "/lib",
    "/lib64",
    "/bin",
    "/sbin",
    "/etc/ssl",
    "/etc/resolv.conf",
    "/etc/hosts",
    "/etc/nsswitch.conf",
    "/etc/localtime",
];

/// Lexically normalise an absolute path: collapse `//`, `.` and `..`.
pub fn normalize(p: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for c in p.split('/') {
        match c {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            c => parts.push(c),
        }
    }
    format!("/{}", parts.join("/"))
}

fn under(path: &str, root: &str) -> bool {
    root == "/" || path == root || path.strip_prefix(root).is_some_and(|r| r.starts_with('/'))
}

impl Mounts {
    pub fn add(&mut self, host: &str, inside: &str, mode: Mode, origin: &'static str) {
        self.add_with(host, inside, mode, origin, false);
    }

    pub fn add_with(&mut self, host: &str, inside: &str, mode: Mode, origin: &'static str, explicit: bool) {
        let hidden = explicit.then(|| format!("{EXPLICIT_DIR}/{}", self.entries.iter().filter(|e| e.explicit).count()));
        self.entries.push(MountEntry { host: normalize(host), inside: normalize(inside), mode, origin, explicit, hidden });
    }

    pub fn has_explicit(&self) -> bool {
        self.entries.iter().any(|e| e.explicit)
    }

    pub fn add_system(&mut self) {
        for p in SYSTEM_SET {
            self.add(p, p, Mode::Ro, "system");
        }
    }

    /// The fixed private `/tmp` every sandbox has.
    pub fn add_fixed(&mut self) {
        if !self.entries.iter().any(|e| e.inside == "/tmp") {
            self.add("/tmp", "/tmp", Mode::Rw, "fixed");
        }
    }

    pub fn is_system_mounted(&self) -> bool {
        SYSTEM_SET.iter().all(|p| self.entries.iter().any(|e| e.origin == "system" && e.inside == *p))
    }

    /// The most specific mount covering an inside path, with the host path it maps to.
    pub fn find(&self, inside: &str) -> Option<(&MountEntry, String)> {
        let p = normalize(inside);
        let e = self
            .entries
            .iter()
            .filter(|e| under(&p, &e.inside))
            .max_by_key(|e| (e.inside.len(), e.inside != "/"))?;
        let rest = &p[e.inside.len().min(p.len())..];
        let host = if e.inside == "/" {
            format!("{}{}", e.host.trim_end_matches('/'), p)
        } else {
            format!("{}{}", e.host, rest)
        };
        Some((e, host))
    }

    /// True if `inside` is exactly a mount point.
    pub fn is_mount_point(&self, inside: &str) -> bool {
        let p = normalize(inside);
        p == "/" || self.entries.iter().any(|e| e.inside == p)
    }

    /// Translate a host path to the inside path through the mount table.
    /// `Err` holds the candidates when the translation is ambiguous.
    pub fn host_to_inside(&self, host: &str) -> Result<Option<String>, Vec<String>> {
        let h = normalize(host);
        let mut best: Vec<&MountEntry> = Vec::new();
        let mut best_len = 0;
        for e in self.entries.iter().filter(|e| e.origin != "fixed" || e.inside == "/tmp") {
            if under(&h, &e.host) {
                match e.host.len().cmp(&best_len) {
                    std::cmp::Ordering::Greater => {
                        best_len = e.host.len();
                        best = vec![e];
                    }
                    std::cmp::Ordering::Equal => best.push(e),
                    _ => {}
                }
            }
        }
        match best.as_slice() {
            [] => Ok(None),
            [e] => Ok(Some(normalize(&format!("{}{}", e.inside, &h[e.host.len().min(h.len())..])))),
            many => {
                let first = normalize(&format!("{}{}", many[0].inside, &h[many[0].host.len().min(h.len())..]));
                let all: Vec<String> = many
                    .iter()
                    .map(|e| normalize(&format!("{}{}", e.inside, &h[e.host.len().min(h.len())..])))
                    .collect();
                if all.iter().all(|a| *a == first) {
                    Ok(Some(first))
                } else {
                    Err(all)
                }
            }
        }
    }

    /// The table as seen from inside the sandbox, where every path is its own host path.
    pub fn inside_view(&self) -> Mounts {
        Mounts {
            entries: self
                .entries
                .iter()
                .map(|e| MountEntry {
                    host: e.hidden.clone().unwrap_or_else(|| e.inside.clone()),
                    inside: e.inside.clone(),
                    mode: e.mode,
                    origin: e.origin,
                    explicit: e.explicit,
                    hidden: e.hidden.clone(),
                })
                .collect(),
        }
    }

    pub fn describe(&self) -> String {
        let mut out = String::new();
        for e in &self.entries {
            let tag = if e.explicit { ", explicit" } else { "" };
            if e.host == e.inside || e.hidden.is_some() && e.hidden.as_deref() == Some(e.host.as_str()) {
                out.push_str(&format!("  {} {}  ({}{tag})\n", e.inside, e.mode.word(), e.origin));
            } else {
                out.push_str(&format!("  {} <- {} {}  ({}{tag})\n", e.inside, e.host, e.mode.word(), e.origin));
            }
        }
        if out.is_empty() {
            out.push_str("  (no mounts)\n");
        }
        out
    }

    /// Narrow a `rw` mount to `ro` (`--ro INSIDE_PATH`). False if no mount has that path.
    pub fn make_ro(&mut self, inside: &str) -> bool {
        let p = normalize(inside);
        let mut found = false;
        for e in self.entries.iter_mut().filter(|e| e.inside == p) {
            e.mode = Mode::Ro;
            found = true;
        }
        found
    }
}
