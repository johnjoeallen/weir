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
fn stubs_match_arguments_and_answer_in_turn() {
    let dir = scratch();
    let f = dir.join("stubs.weir");
    std::fs::write(
        &f,
        r#"sandbox { mount system }
tool echo_it {
  program "echo"
  in word: string
  args [word]
  see system
  takes none
  yields lines
}
test "by argument and in turn" {
  stub echo_it(word: "a") yields lines: ["first"]
  stub echo_it(word: "a") yields lines: ["second"]
  stub echo_it(word: "b") yields lines: ["bee"]
  expect echo_it(word: "a") == ["first"]
  expect echo_it(word: "b") == ["bee"]
  expect echo_it(word: "a") == ["second"]
  expect echo_it(word: "a") == ["second"]
}
test "a call no stub matches fails" {
  stub echo_it(word: "a") yields lines: ["first"]
  expect echo_it(word: "z") == ["first"]
}
"#,
    )
    .unwrap();
    let o = weir_args(&["test", f.to_str().unwrap()]);
    assert_eq!(o.code, 1, "{}{}", o.stdout, o.stderr);
    assert!(o.stdout.contains("ok    by argument and in turn"), "{}", o.stdout);
    assert!(o.stdout.contains("no stub for `echo_it` matches this call (word: z)"), "{}", o.stdout);
    let bad = dir.join("bad.weir");
    std::fs::write(
        &bad,
        "sandbox { mount system }\ntool e {\n  program \"echo\"\n  in w: string\n  args [w]\n  takes none\n  yields lines\n}\ntest \"t\" {\n  stub e(x: 1) yields lines: []\n}\n",
    )
    .unwrap();
    let c = weir_args(&["check", bad.to_str().unwrap()]);
    assert!(c.stderr.contains("E-STUB-ARG") || c.stdout.contains("E-STUB-ARG"), "{}{}", c.stdout, c.stderr);
}

#[test]
fn secrets_come_from_a_file_a_feed_or_the_environment_in_that_order() {
    use std::io::Write as _;
    use std::os::unix::fs::PermissionsExt;
    let p = script(
        r#"tool show {
  program "sh"
  in a: secret
  in b: secret
  env {
    SA = a
    SB = b
  }
  args ["-c", "echo $SA $SB"]
  takes none
  yields lines
}
main flow f {
  in a: secret
  in b: secret
  out r: string
  set r = first(of: show(a: a, b: b))
}
"#,
    );
    let path = p.to_str().unwrap();
    let file = p.with_file_name("keys.env");
    std::fs::write(&file, "# keys\nexport A=\"file a\"\nOTHER=1\n").unwrap();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
    let run = |args: &[&str], feed: &str, env: &[(&str, &str)]| {
        let mut c = weir();
        c.args(["run", path, "f", "--sandbox", "off", "--out", "r"]).args(args);
        c.env_remove("WEIR_SECRET_A").env_remove("WEIR_SECRET_B");
        for (k, v) in env {
            c.env(k, v);
        }
        let mut child = c.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
        child.stdin.take().unwrap().write_all(feed.as_bytes()).unwrap();
        let o = child.wait_with_output().unwrap();
        (o.status.code().unwrap_or(-1), String::from_utf8_lossy(&o.stdout).into_owned(), String::from_utf8_lossy(&o.stderr).into_owned())
    };
    let f = file.to_str().unwrap();
    // file for one, environment for the other; the file wins over a feed and the environment
    let (code, out, err) = run(&["--secrets-file", f], "A=feed a\nB=feed b\n", &[("WEIR_SECRET_A", "env a"), ("WEIR_SECRET_B", "env b")]);
    assert_eq!((code, out.trim()), (0, "file a env b"), "{err}");
    // a feed beats the environment, and a name it lacks falls back to the environment
    let (code, out, err) = run(&["--secrets-stdin"], "A=feed a\n", &[("WEIR_SECRET_A", "env a"), ("WEIR_SECRET_B", "env b")]);
    assert_eq!((code, out.trim()), (0, "feed a env b"), "{err}");
    // a missing secret is a usage error that names the sources
    let (code, _, err) = run(&[], "", &[("WEIR_SECRET_A", "env a")]);
    assert_eq!(code, 2);
    assert!(err.contains("--secrets-file") && err.contains("WEIR_SECRET_B"), "{err}");
    // a bad line is reported by number, without its text
    let (code, _, err) = run(&["--secrets-stdin"], "A=1\nhunter2\n", &[]);
    assert_eq!(code, 2);
    assert!(err.contains("line 2") && !err.contains("hunter2"), "{err}");
    // a file others can read is refused
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
    let (code, _, err) = run(&["--secrets-file", f], "", &[]);
    assert_eq!(code, 2);
    assert!(err.contains("chmod 600"), "{err}");
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
fn sandbox_mode_must_be_required_auto_or_off() {
    let p = script("sandbox {\n  mount system\n}\nmain flow a {\n  print(text: \"x\")\n}\n");
    let o = weir_args(&["run", s(&p), "--sandbox", "maybe"]);
    assert_eq!(o.code, 2, "{}", o.stderr);
    assert!(o.stderr.contains("required, auto or off"), "{}", o.stderr);
    let mut c = weir();
    c.args(["run", s(&p)]).env("WEIR_SANDBOX", "maybe");
    let o = run_cmd(c);
    assert_eq!(o.code, 2, "{}", o.stderr);
    assert!(o.stderr.contains("WEIR_SANDBOX"), "{}", o.stderr);
}

#[test]
fn sandbox_off_runs_unconfined_with_a_warning() {
    let p = script("sandbox {\n  mount system\n}\nmain flow a {\n  print(text: \"x\")\n}\n");
    let o = weir_args(&["run", s(&p), "--sandbox", "off", "--json"]);
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert!(o.stderr.contains("without the filesystem sandbox"), "{}", o.stderr);
    assert!(o.stdout.contains("\"sandbox\": false"), "{}", o.stdout);
    let mut c = weir();
    c.args(["run", s(&p), "--json"]).env("WEIR_SANDBOX", "off");
    let o = run_cmd(c);
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert!(o.stdout.contains("\"sandbox\": false"), "{}", o.stdout);
}

#[test]
fn sandbox_auto_confines_when_it_can_and_warns_when_it_cannot() {
    let p = script("sandbox {\n  mount system\n}\nmain flow a {\n  print(text: \"x\")\n}\n");
    let o = weir_args(&["run", s(&p), "--sandbox", "auto", "--json"]);
    assert_eq!(o.code, 0, "{}", o.stderr);
    if o.stderr.contains("the sandbox could not be created") {
        assert!(o.stdout.contains("\"sandbox\": false"), "{}", o.stdout);
    } else {
        assert!(o.stdout.contains("\"sandbox\": true"), "{}", o.stdout);
    }
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
tool sh {{
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
fn explicit_mount_is_reached_only_by_request() {
    let base = scratch().join(format!("weir-explicit-it-{}", std::process::id()));
    let open = base.join("open");
    let vault = base.join("vault");
    std::fs::create_dir_all(&open).unwrap();
    std::fs::create_dir_all(&vault).unwrap();
    std::fs::write(open.join("a.txt"), "plain\n").unwrap();
    std::fs::write(vault.join("key.txt"), "vault-content\n").unwrap();
    let src = format!(
        r#"sandbox {{
  mount system
  mount "{o}" ro
  mount "{v}" at "{iv}" ro explicit
}}
tool sh_default {{
  program "sh"
  in script: string
  args ["-c", script]
  yields lines
  ok_codes [0, 1, 2]
}}
tool sh_asks {{
  program "sh"
  in script: string
  args ["-c", script]
  see system
  see "{iv}" ro
  yields lines
  ok_codes [0, 1, 2]
}}
main flow a {{
  out by_default: list<string>
  out asked: list<string>
  out within_run: string
  out builtin: string
  out exists_top: bool
  out parent_only: bool
  set by_default = sh_default(script: "cat {o}/a.txt 2>&1; cat {iv}/key.txt 2>&1; ls /.weir-explicit 2>&1")
  set asked = sh_asks(script: "cat {iv}/key.txt 2>&1")
  set within_run = within system, "{iv}" ro {{
    return first(of: run(cmd: "cat", args: ["{iv}/key.txt"]))
  }}
  set exists_top = exists(path: path(text: "{iv}/key.txt"))
  set parent_only = within system, "{o}" ro {{
    return exists(path: path(text: "{iv}/key.txt"))
  }}
  try {{
    read_text(file: path(text: "{iv}/key.txt"))
    set builtin = "read"
  }} catch e: CapabilityError {{
    set builtin = e.message
  }}
}}
"#,
        o = open.display(),
        v = vault.display(),
        iv = format!("{}/vault", open.display())
    );
    let p = script(&src);
    let o = weir_args(&["run", s(&p), "--json"]);
    if o.code == 4 {
        eprintln!("skipped: this host cannot create the sandbox: {}", o.stderr.trim());
        return;
    }
    assert_eq!(o.code, 0, "{}{}", o.stdout, o.stderr);
    // a program that asks for nothing sees the ordinary mount but not the explicit one, nor where it is held
    assert!(o.stdout.contains("plain"), "{}", o.stdout);
    assert_eq!(o.stdout.matches("vault-content").count(), 2, "only the two requests may read it: {}", o.stdout);
    assert!(o.stdout.contains("No such file"), "{}", o.stdout);
    assert!(o.stdout.contains("explicit mount"), "{}", o.stdout);
    assert!(o.stdout.contains("\"exists_top\": false"), "{}", o.stdout);
    assert!(o.stdout.contains("\"parent_only\": false"), "naming a parent must not request it: {}", o.stdout);
}

#[test]
fn a_sandbox_input_must_be_declared_by_the_flow() {
    let base = "sandbox {\n  in src: path = \"/var\"\n  mount system\n  mount src at \"/data\" ro\n}\n";
    let p = script(&format!("{base}main flow a {{\n  print(text: \"x\")\n}}\n"));
    let o = weir_args(&["check", s(&p)]);
    assert_eq!(o.code, 3, "{}", o.stderr);
    assert!(o.stderr.contains("E-SANDBOX-IN-NOT-IN-FLOW"), "{}", o.stderr);
    let p = script(&format!("{base}main flow a {{\n  in src: string\n  print(text: src)\n}}\n"));
    let o = weir_args(&["check", s(&p)]);
    assert!(o.stderr.contains("E-SANDBOX-IN-TYPE-MISMATCH"), "{}", o.stderr);
    let p = script(&format!("{base}main flow a {{\n  in src: path = path(text: \"/x\")\n  print(text: \"x\")\n}}\n"));
    let o = weir_args(&["check", s(&p)]);
    assert!(o.stderr.contains("E-SANDBOX-IN-DEFAULT"), "{}", o.stderr);
}

#[test]
fn a_flow_receives_its_sandbox_input_as_the_inside_path() {
    let dir = scratch().join(format!("weir-sbin-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let src = "sandbox {\n  in src: path = \"/var\"\n  mount system\n  mount src at \"/data\" ro\n}\nmain flow a {\n  in src: path\n  out where: string\n  set where = string(value: src)\n}\n";
    let p = script(src);
    let o = weir_args(&["run", s(&p), "--src", dir.to_str().unwrap(), "--out", "where"]);
    if o.code == 4 {
        eprintln!("skipped: this host cannot create the sandbox: {}", o.stderr.trim());
        return;
    }
    assert_eq!((o.code, o.stdout.trim()), (0, "/data"), "{}", o.stderr);
    // listed once in --help, as a host path
    let o = weir_args(&["run", s(&p), "--help"]);
    assert_eq!(o.stdout.matches("--src").count(), 1, "{}", o.stdout);
    assert!(o.stdout.contains("host path, sandbox"), "{}", o.stdout);
}

#[test]
fn mounting_the_host_root_is_an_error() {
    for mount in ["mount \"/\" ro", "mount \"/\" at \"/host\" ro", "mount \"/var/..\" rw"] {
        let p = script(&format!("sandbox {{\n  mount system\n  {mount}\n}}\nmain flow a {{\n  print(text: \"x\")\n}}\n"));
        let o = weir_args(&["check", s(&p)]);
        assert_eq!(o.code, 3, "{mount}: {}", o.stderr);
        assert!(o.stderr.contains("E-MOUNT-ROOT"), "{mount}: {}", o.stderr);
        let o = weir_args(&["run", s(&p)]);
        assert_eq!(o.code, 3, "{mount}: run checks first: {}", o.stderr);
        assert!(!o.stdout.contains('x'), "nothing may run: {}", o.stdout);
    }
    // a root chosen at run time through a sandbox input is refused as well
    let p = script("sandbox {\n  in dir: path = \"/var\"\n  mount system\n  mount dir ro\n}\nmain flow a {\n  in dir: path\n  print(text: \"x\")\n}\n");
    let o = weir_args(&["run", s(&p), "--dir", "/"]);
    assert_eq!(o.code, 2, "{}", o.stderr);
    assert!(o.stderr.contains("--sandbox off"), "{}", o.stderr);
}

#[test]
fn see_lines_and_within_blocks_work_without_the_sandbox() {
    let base = scratch().join(format!("weir-views-{}", std::process::id()));
    let shown = base.join("shown");
    let hidden = base.join("hidden");
    std::fs::create_dir_all(&shown).unwrap();
    std::fs::create_dir_all(&hidden).unwrap();
    std::fs::write(shown.join("a.txt"), "in-view\n").unwrap();
    std::fs::write(hidden.join("b.txt"), "out-of-view\n").unwrap();
    let src = format!(
        r#"tool sh_view {{
  program "sh"
  in script: string
  args ["-c", script]
  see system
  see "{s}" ro
  yields lines
  ok_codes [0, 1, 2]
}}
tool sh_all {{
  program "sh"
  in script: string
  args ["-c", script]
  yields lines
  ok_codes [0, 1, 2]
}}
main flow a {{
  out narrowed: list<string>
  out everything: list<string>
  out inside_within: list<string>
  set narrowed = sh_view(script: "cat {s}/a.txt 2>&1; cat {h}/b.txt 2>&1")
  set everything = sh_all(script: "cat {s}/a.txt 2>&1; cat {h}/b.txt 2>&1")
  set inside_within = within system, "{s}" ro {{
    return sh_all(script: "cat {s}/a.txt 2>&1; cat {h}/b.txt 2>&1")
  }}
}}
"#,
        s = shown.display(),
        h = hidden.display()
    );
    let p = script(&src);
    let o = weir_args(&["run", s(&p), "--json"]);
    if o.stderr.contains("could not narrow") {
        eprintln!("skipped: this host cannot create the per-program view: {}", o.stderr.trim());
        return;
    }
    assert_eq!(o.code, 0, "{}{}", o.stdout, o.stderr);
    // three runs read the shown file; only the unnarrowed one reads the other folder
    assert_eq!(o.stdout.matches("in-view").count(), 3, "{}", o.stdout);
    assert_eq!(o.stdout.matches("out-of-view").count(), 1, "only the program with no view may read it: {}", o.stdout);
    let left: Vec<_> = std::fs::read_dir("/tmp").unwrap().flatten().filter(|e| e.file_name().to_string_lossy().starts_with(".weir-see-")).collect();
    assert!(left.is_empty(), "view folders left behind in /tmp: {left:?}");
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
fn a_tool_sees_only_the_paths_in_its_see_block() {
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
tool wide {{
  program "sh"
  in script: string
  args ["-c", script]
  yields lines
  ok_codes [0, 1, 2]
}}
tool narrow {{
  program "sh"
  in script: string
  args ["-c", script]
  see system
  see "{a}" ro
  yields lines
  ok_codes [0, 1, 2]
}}
main flow a {{
  out script_sees: list<string>
  out tool_sees: list<string>
  out tool_hidden: list<string>
  out tool_write: list<string>
  set script_sees = wide(script: "cat {b}/two.txt")
  set tool_sees = narrow(script: "cat {a}/one.txt")
  set tool_hidden = narrow(script: "{{ cat {b}/two.txt; }} 2>&1")
  set tool_write = narrow(script: "{{ echo x > {a}/new.txt; }} 2>&1")
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
    assert!(o.stdout.contains("\"tool_sees\": [\n      \"visible\""), "{}", o.stdout);
    // the narrowed tool cannot see it, although the script mounted it
    assert!(o.stdout.matches("narrowed away").count() == 1, "the narrowed tool read a path outside its `see`: {}", o.stdout);
    assert!(o.stdout.contains("No such file"), "{}", o.stdout);
    // and a ro `see` stays read-only
    assert!(o.stdout.to_lowercase().contains("read-only"), "{}", o.stdout);
    assert!(!a.join("new.txt").exists());
}

#[test]
fn the_bundled_examples_check_and_their_tests_pass() {
    for name in ["commands.weir", "dpkgwatch.weir", "parse_access.weir", "s3_sync.weir", "pg_backup.weir", "docker_rollout.weir", "sign_release.weir"] {
        let f = manifest(&format!("examples/{name}"));
        let c = weir_args(&["check", &f]);
        assert_eq!(c.code, 0, "{name}: {}{}", c.stdout, c.stderr);
        let t = weir_args(&["test", &f]);
        assert_eq!(t.code, 0, "{name}: {}{}", t.stdout, t.stderr);
    }
}

#[test]
fn commands_example_runs_every_form_of_starting_a_program() {
    let o = weir_args(&["run", &manifest("examples/commands.weir")]);
    if o.code == 4 {
        eprintln!("skipped: this host cannot create the sandbox: {}", o.stderr.trim());
        return;
    }
    assert_eq!(o.code, 0, "{}{}", o.stdout, o.stderr);
    for expected in ["kernel: Linux", "words: one two three", "prints: 0", "false failed with return code 1", "cat failed as expected", "/var/log visible: false"] {
        assert!(o.stdout.contains(expected), "missing {expected:?}: {}", o.stdout);
    }
}

// ---- `within` blocks and `see` lines ----------------------------------------------

const WITHIN_HEAD: &str = "sandbox {\n  mount system\n  mount \"/etc\" ro\n  mount \"/var/log\" ro\n}\ntool cat_file {\n  program \"cat\"\n  in file: path\n  args [\"--\", file]\n  takes none\n  yields lines\n}\n";

fn within_script(flow_body: &str) -> PathBuf {
    script(&format!("{WITHIN_HEAD}main flow a {{\n  out r: int\n{flow_body}\n}}\n"))
}

#[test]
fn within_returns_a_value_and_its_lets_stay_inside() {
    let p = within_script(
        "  let info = within system, \"/etc\" ro {\n    let inner = 41\n    return { n: inner + 1 }\n  }\n  set r = info.n\n",
    );
    let o = weir_args(&["run", s(&p), "--json"]);
    if o.code == 4 {
        return;
    }
    assert_eq!(o.code, 0, "{}{}", o.stdout, o.stderr);
    assert!(o.stdout.contains("\"r\": 42"), "{}", o.stdout);
    let leaked = within_script("  within system, \"/etc\" ro {\n    let inner = 1\n  }\n  set r = inner\n");
    let c = weir_args(&["check", s(&leaked)]);
    assert_eq!(c.code, 3);
    assert!(c.stderr.contains("E-UNKNOWN-NAME"), "{}", c.stderr);
}

#[test]
fn within_hides_paths_from_programs_and_built_ins() {
    let p = within_script(
        "  let p = try within system, \"/etc\" ro {\n    return cat_file(file: path(text: \"/var/log/dpkg.log\"))\n  }\n  let b = try within system, \"/etc\" ro {\n    return read_text(file: path(text: \"/var/log/dpkg.log\"))\n  }\n  let e = within system, \"/etc\" ro {\n    return exists(path: path(text: \"/var/log\"))\n  }\n  let outside = exists(path: path(text: \"/var/log\"))\n  if p.ok or b.ok or e or not outside {\n    fail message: \"the view leaked or never ended\"\n  }\n  print(text: b.message)\n  set r = 1\n",
    );
    let o = weir_args(&["run", s(&p)]);
    if o.code == 4 {
        eprintln!("skipped: this host cannot create the sandbox: {}", o.stderr.trim());
        return;
    }
    assert_eq!(o.code, 0, "{}{}", o.stdout, o.stderr);
    assert!(o.stdout.contains("outside the view of the enclosing `within` block"), "{}", o.stdout);
}

#[test]
fn within_can_only_narrow() {
    // /var/log is mounted by the script, but not in the outer view
    let p = within_script(
        "  let w = try within system, \"/etc\" ro {\n    return within \"/var/log\" ro {\n      return 1\n    }\n  }\n  if w.ok {\n    fail message: \"widened\"\n  }\n  print(text: w.message)\n  set r = 1\n",
    );
    let o = weir_args(&["run", s(&p)]);
    if o.code == 4 {
        return;
    }
    assert_eq!(o.code, 0, "{}{}", o.stdout, o.stderr);
    assert!(o.stdout.contains("enclosing view does not have"), "{}", o.stdout);
    // a path the script never mounted is a check error
    let q = within_script("  within \"/srv\" ro {\n    print(text: \"x\")\n  }\n  set r = 1\n");
    let c = weir_args(&["check", s(&q)]);
    assert_eq!(c.code, 3);
    assert!(c.stderr.contains("E-SEE-UNMOUNTED"), "{}", c.stderr);
}

#[test]
fn within_check_rules() {
    let no_ret = within_script("  let x = within system {\n    print(text: \"hi\")\n  }\n  set r = 1\n");
    assert!(weir_args(&["check", s(&no_ret)]).stderr.contains("E-WITHIN-NO-RETURN"));
    let ok_stmt = within_script("  within system {\n    print(text: \"hi\")\n  }\n  set r = 1\n");
    assert_eq!(weir_args(&["check", s(&ok_stmt)]).code, 0);
    let both = within_script("  let x = within system {\n    if true {\n      return 1\n    } else {\n      return 2\n    }\n  }\n  set r = x\n");
    assert_eq!(weir_args(&["check", s(&both)]).code, 0);
    let outside = within_script("  return 1\n  set r = 1\n");
    assert!(weir_args(&["check", s(&outside)]).stderr.contains("E-RETURN-OUTSIDE-FN"));
    let in_fn = script("fn f() -> int {\n  return within system {\n    return 1\n  }\n}\nmain flow a {\n  print(text: \"x\")\n}\n");
    assert!(weir_args(&["check", s(&in_fn)]).stderr.contains("E-FN-IMPURE"));
}

#[test]
fn a_tool_no_longer_takes_a_sandbox_block() {
    let p = script("tool f {\n  program \"cat\"\n  sandbox {\n    see system\n  }\n}\nmain flow a {\n  print(text: \"x\")\n}\n");
    let o = weir_args(&["check", s(&p)]);
    assert_eq!(o.code, 3);
    assert!(o.stderr.contains("E-TOOL-SANDBOX") && o.stderr.contains("`see` lines directly"), "{}", o.stderr);
}
