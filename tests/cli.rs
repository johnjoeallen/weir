//! Integration tests: run the real `weir` binary.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

static N: AtomicUsize = AtomicUsize::new(0);

/// Scratch space under `target/`, so test runs leave nothing in `/tmp`.
fn scratch() -> PathBuf {
    PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
}

fn weir() -> Command {
    Command::new(env!("CARGO_BIN_EXE_weir"))
}

struct Out {
    code: i32,
    stdout: String,
    stderr: String,
}

fn run_cmd(mut c: Command) -> Out {
    let o = c.stdin(Stdio::null()).output().expect("failed to start weir");
    Out {
        code: o.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
    }
}

fn weir_args(args: &[&str]) -> Out {
    let mut c = weir();
    c.args(args);
    run_cmd(c)
}

/// Write a script to a fresh temp folder and return its path.
fn script(src: &str) -> PathBuf {
    let dir = scratch().join(format!("weir-it-{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("s.weir");
    std::fs::write(&p, src).unwrap();
    p
}

fn s(p: &Path) -> &str {
    p.to_str().unwrap()
}

fn manifest(rel: &str) -> String {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(rel).to_string_lossy().into_owned()
}

#[test]
fn help_and_version() {
    let o = weir_args(&["--help"]);
    assert_eq!(o.code, 0);
    assert!(o.stdout.contains("weir run"));
    assert_eq!(weir_args(&["--version"]).code, 0);
    assert_eq!(weir_args(&["frobnicate"]).code, 2);
}

#[test]
fn shipped_example_checks_and_its_tests_pass() {
    let ex = manifest("examples/dpkgwatch.weir");
    let o = weir_args(&["check", &ex]);
    assert_eq!(o.code, 0, "{}{}", o.stdout, o.stderr);
    let o = weir_args(&["test", &ex]);
    assert_eq!(o.code, 0, "{}{}", o.stdout, o.stderr);
    assert!(o.stdout.contains("3 passed, 0 failed"), "{}", o.stdout);
}

#[test]
fn design_document_examples_check_clean() {
    // The two worked examples in the design (sections 13 and 14) are the language's own spec.
    let doc = std::fs::read_to_string(manifest("docs/DESIGN.md")).unwrap();
    let mut found = 0;
    for block in doc.split("```weir\n").skip(1) {
        let body = block.split("```").next().unwrap();
        if !(body.starts_with("// ban_noisy_clients.weir") || body.starts_with("// check_certs.weir")) {
            continue;
        }
        let p = script(body);
        let o = weir_args(&["check", s(&p)]);
        assert_eq!(o.code, 0, "{}\n{}{}", body.lines().next().unwrap(), o.stdout, o.stderr);
        found += 1;
    }
    assert_eq!(found, 2);
}

#[test]
fn check_json_has_stable_fields() {
    let p = script("main flow a {\n  let x = nope(y: 1)\n}\n");
    let o = weir_args(&["check", s(&p), "--json"]);
    assert_eq!(o.code, 3);
    for key in ["\"file\"", "\"line\"", "\"col\"", "\"code\": \"E-UNKNOWN-CALL\"", "\"msg\"", "\"fix\""] {
        assert!(o.stdout.contains(key), "{key} missing in {}", o.stdout);
    }
}

#[test]
fn exit_codes_distinguish_failure_usage_and_check() {
    let ok = script("main flow a {\n  in n: int\n  out r: int\n  set r = n * 2\n}\n");
    let o = weir_args(&["run", s(&ok), "--n", "21", "--out", "r"]);
    assert_eq!((o.code, o.stdout.trim()), (0, "42"), "{}", o.stderr);
    // usage: bad value, missing input, unknown flag
    assert_eq!(weir_args(&["run", s(&ok), "--n", "abc"]).code, 2);
    assert_eq!(weir_args(&["run", s(&ok)]).code, 2);
    assert_eq!(weir_args(&["run", s(&ok), "--n", "1", "--bogus"]).code, 2);
    // flow failure
    let bad = script("main flow a {\n  fail message: \"no\"\n}\n");
    let o = weir_args(&["run", s(&bad)]);
    assert_eq!(o.code, 1);
    assert!(o.stderr.contains("no"));
    // check error: nothing runs
    let broken = script("main flow a {\n  print(text: undefined_name)\n}\n");
    assert_eq!(weir_args(&["run", s(&broken)]).code, 3);
}

#[test]
fn json_result_reports_partial_outputs_after_failure() {
    let p = script("main flow a {\n  out r: int\n  set r = 7\n  fail message: \"late\"\n}\n");
    let o = weir_args(&["run", s(&p), "--json"]);
    assert_eq!(o.code, 1);
    assert!(o.stdout.contains("\"r\": 7"), "{}", o.stdout);
    assert!(o.stdout.contains("\"ok\": false"));
}

#[test]
fn confirmable_steps_need_confirm() {
    let p = script("main flow a {\n  go {\n    print(text: \"ran\")\n  } confirmable\n}\n");
    let o = weir_args(&["run", s(&p)]);
    assert_eq!(o.code, 0);
    assert!(!o.stdout.contains("ran"));
    assert!(o.stderr.contains("skipped: unconfirmed"), "{}", o.stderr);
    let o = weir_args(&["run", s(&p), "--confirm"]);
    assert!(o.stdout.contains("ran"));
}

#[test]
fn plan_executes_nothing() {
    let dir = scratch().join(format!("weir-plan-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let target = dir.join("out.txt");
    let src = format!(
        "main flow a {{\n  write(file: path(text: \"{}\"), text: \"x\")\n}}\n",
        target.display()
    );
    let p = script(&src);
    let o = weir_args(&["run", s(&p), "--plan"]);
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert!(o.stdout.contains("plan: write"), "{}", o.stdout);
    assert!(!target.exists(), "--plan must not write");
    assert_eq!(weir_args(&["run", s(&p)]).code, 0);
    assert!(target.exists());
}

#[test]
fn secrets_are_never_accepted_on_argv() {
    let p = script("main flow a {\n  in tok: secret\n  out n: int\n  set n = count(of: \"abc\")\n}\n");
    let o = weir_args(&["run", s(&p), "--tok", "hunter2"]);
    assert_eq!(o.code, 2);
    assert!(o.stderr.contains("WEIR_SECRET_TOK"), "{}", o.stderr);
    let mut c = weir();
    c.args(["run", s(&p), "--out", "n"]).env("WEIR_SECRET_TOK", "hunter2");
    let o = run_cmd(c);
    assert_eq!((o.code, o.stdout.trim()), (0, "3"), "{}", o.stderr);
}

#[test]
fn no_sandbox_refuses_remapping_mounts() {
    let p = script("sandbox {\n  mount system\n  mount \"/tmp\" at \"/data\" ro\n}\nmain flow a {\n  print(text: \"x\")\n}\n");
    let o = weir_args(&["run", s(&p), "--no-sandbox"]);
    assert_eq!(o.code, 2, "{}", o.stderr);
    assert!(o.stderr.contains("remaps"), "{}", o.stderr);
}

#[test]
fn read_only_mount_error_names_the_mount() {
    let dir = scratch().join(format!("weir-ro-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let src = format!(
        "sandbox {{\n  mount \"{d}\" ro\n}}\nmain flow a {{\n  write(file: path(text: \"{d}/x\"), text: \"1\")\n}}\n",
        d = dir.display()
    );
    let p = script(&src);
    let o = weir_args(&["run", s(&p), "--no-sandbox"]);
    assert_eq!(o.code, 1);
    assert!(o.stderr.contains("read-only mount"), "{}", o.stderr);
    assert!(!dir.join("x").exists());
}

#[test]
fn sandbox_confines_programs_when_available() {
    let base = scratch().join(format!("weir-sbx-it-{}", std::process::id()));
    let dir = base.join("mounted");
    let sibling = base.join("sibling");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::create_dir_all(&sibling).unwrap();
    std::fs::write(dir.join("hello.txt"), "hello\n").unwrap();
    std::fs::write(sibling.join("secret.txt"), "host only\n").unwrap();
    let src = format!(
        r#"sandbox {{
  mount system
  mount "{d}" ro
}}
filter sh {{
  program "sh"
  in script: string
  args ["-c", script]
  yields lines
  ok_codes [0, 1, 2]
}}
main flow a {{
  out rootdir: list<string>
  out seen: list<string>
  out hidden: list<string>
  out write: list<string>
  set rootdir = sh(script: "ls /")
  set seen = sh(script: "cat {d}/hello.txt")
  set hidden = sh(script: "{{ cat {s}/secret.txt; }} 2>&1")
  set write = sh(script: "{{ echo x > {d}/new.txt; }} 2>&1")
}}
"#,
        d = dir.display(),
        s = sibling.display()
    );
    let p = script(&src);
    let o = weir_args(&["run", s(&p), "--json"]);
    if o.code == 4 {
        eprintln!("skipped: this host cannot create the sandbox: {}", o.stderr.trim());
        return;
    }
    assert_eq!(o.code, 0, "{}{}", o.stdout, o.stderr);
    // the root holds mount points only: no /root, no /var, no /srv
    for forbidden in ["\"root\"", "\"var\"", "\"srv\"", "\"mnt\""] {
        assert!(!o.stdout.contains(forbidden), "{forbidden} visible in the sandbox: {}", o.stdout);
    }
    // a mounted folder is readable, and its sibling on the host does not exist
    assert!(o.stdout.contains("\"hello\""), "{}", o.stdout);
    assert!(!o.stdout.contains("host only"), "a sibling of the mount leaked: {}", o.stdout);
    assert!(o.stdout.contains("No such file"), "{}", o.stdout);
    // a ro mount is read-only
    assert!(o.stdout.to_lowercase().contains("read-only"), "{}", o.stdout);
    assert!(!dir.join("new.txt").exists(), "a ro mount must not be writable");
    assert!(o.stdout.contains("\"sandbox\": true"));
}

#[test]
fn sandbox_exit_code_and_cleanup() {
    let p = script("sandbox {\n  mount system\n}\nmain flow a {\n  fail message: \"inside\"\n}\n");
    let o = weir_args(&["run", s(&p)]);
    if o.code == 4 {
        return; // no sandbox on this host
    }
    assert_eq!(o.code, 1);
    assert!(o.stderr.contains("inside"));
    let leftovers = std::fs::read_dir(std::env::temp_dir())
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with(&format!("weir-sandbox-{}", std::process::id())))
        .count();
    assert_eq!(leftovers, 0);
}

#[test]
fn imports_local_components_and_hides_private_flows() {
    let dir = scratch().join(format!("weir-use-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("lib.weir"),
        "flow double {\n  in n: int\n  out r: int\n  set r = helper(n: n).r\n}\nprivate flow helper {\n  in n: int\n  out r: int\n  set r = n * 2\n}\n",
    )
    .unwrap();
    let main = dir.join("main.weir");
    std::fs::write(&main, "use \"./lib.weir\"\nmain flow a {\n  out r: int\n  set r = double(n: 4).r\n}\n").unwrap();
    let o = weir_args(&["run", s(&main), "--out", "r"]);
    assert_eq!((o.code, o.stdout.trim()), (0, "8"), "{}", o.stderr);
    // the private flow is not importable
    std::fs::write(&main, "use \"./lib.weir#helper\"\nmain flow a {\n  print(text: \"x\")\n}\n").unwrap();
    let o = weir_args(&["check", s(&main)]);
    assert_eq!(o.code, 3);
    assert!(o.stderr.contains("E-USE-PRIVATE-FLOW"), "{}", o.stderr);
}

#[test]
fn remote_components_are_reported_once_without_a_cascade() {
    let p = script("use \"github:acme/x@v1#thing\"\nmain flow a {\n  let r = [1].thing(on_bad: skip)\n}\n");
    let o = weir_args(&["check", s(&p)]);
    assert_eq!(o.code, 3);
    assert_eq!(o.stderr.matches(": E-").count(), 1, "{}", o.stderr);
    assert!(o.stderr.contains("E-USE-REMOTE-UNSUPPORTED"));
}

#[test]
fn a_script_given_directly_does_nothing_without_run() {
    let marker = script("main flow a {\n  out r: int\n  fail message: \"ran\"\n}\n");
    let o = weir_args(&[s(&marker)]);
    assert_eq!(o.code, 2, "{}", o.stderr);
    assert!(o.stderr.contains("nothing was run"), "{}", o.stderr);
    assert!(!o.stderr.contains("ran\n"), "{}", o.stderr);
    assert_eq!(weir_args(&[s(&marker), "--run"]).code, 1);
    assert_eq!(weir_args(&[s(&marker), "--help"]).code, 0);
}

#[test]
fn a_leading_run_flag_before_the_file_works_as_a_shebang_line() {
    let p = script("main flow a {\n  out r: int\n  set r = 1\n}\n");
    assert_eq!(weir_args(&["--run", s(&p)]).code, 0);
    // other modes are subcommands, not flags
    assert_eq!(weir_args(&["check", s(&p)]).code, 0);
    assert_eq!(weir_args(&[s(&p), "--check"]).code, 2);
}

#[test]
fn a_read_only_mount_may_contain_the_system_files() {
    // `mount system` puts /etc/hosts, /etc/resolv.conf and friends under a read-only /etc.
    let p = script("sandbox {\n  mount system\n  mount \"/etc\" ro\n}\nmain flow a {\n  out r: int\n  set r = 1\n}\n");
    let o = weir_args(&["run", s(&p)]);
    if o.stderr.contains("could not be created") && !o.stderr.contains("mount point") {
        return; // this host cannot create the sandbox
    }
    assert_eq!(o.code, 0, "{}", o.stderr);
}

#[test]
fn a_filter_sees_only_the_paths_in_its_see_block() {
    let base = scratch().join(format!("weir-see-it-{}", std::process::id()));
    let (a, b) = (base.join("a"), base.join("b"));
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    std::fs::write(a.join("one.txt"), "visible\n").unwrap();
    std::fs::write(b.join("two.txt"), "narrowed away\n").unwrap();
    let src = format!(
        r#"sandbox {{
  mount system
  mount "{a}" ro
  mount "{b}" rw
}}
filter wide {{
  program "sh"
  in script: string
  args ["-c", script]
  yields lines
  ok_codes [0, 1, 2]
}}
filter narrow {{
  program "sh"
  in script: string
  args ["-c", script]
  sandbox {{
    see system
    see "{a}" ro
  }}
  yields lines
  ok_codes [0, 1, 2]
}}
main flow a {{
  out script_sees: list<string>
  out filter_sees: list<string>
  out filter_hidden: list<string>
  out filter_write: list<string>
  set script_sees = wide(script: "cat {b}/two.txt")
  set filter_sees = narrow(script: "cat {a}/one.txt")
  set filter_hidden = narrow(script: "{{ cat {b}/two.txt; }} 2>&1")
  set filter_write = narrow(script: "{{ echo x > {a}/new.txt; }} 2>&1")
}}
"#,
        a = a.display(),
        b = b.display()
    );
    let p = script(&src);
    let o = weir_args(&["run", s(&p), "--json"]);
    if o.code == 4 {
        eprintln!("skipped: this host cannot create the sandbox: {}", o.stderr.trim());
        return;
    }
    assert_eq!(o.code, 0, "{}{}", o.stdout, o.stderr);
    // the script's other programs still see the second mount
    assert!(o.stdout.contains("\"script_sees\": [\n      \"narrowed away\""), "{}", o.stdout);
    assert!(o.stdout.contains("\"filter_sees\": [\n      \"visible\""), "{}", o.stdout);
    // the narrowed filter cannot see it, although the script mounted it
    assert!(o.stdout.matches("narrowed away").count() == 1, "the narrowed filter read a path outside its `see`: {}", o.stdout);
    assert!(o.stdout.contains("No such file"), "{}", o.stdout);
    // and a ro `see` stays read-only
    assert!(o.stdout.to_lowercase().contains("read-only"), "{}", o.stdout);
    assert!(!a.join("new.txt").exists());
}
