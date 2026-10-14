//! Command-line surface: `run`, `check`, `list`, `test`, `lock`, `sandbox`.

use crate::ast::*;
use crate::check::check_program;
use crate::diag::Diag;
use crate::error::Ctl;
use crate::interp::{conforms, ctl_location, describe_ctl, Interp, Options, StepRec, Unit};
use crate::json::Json;
use crate::loader::{load, Program};
use crate::mounts::{normalize, Mounts};
use crate::value::{parse_duration, Value};
use std::collections::HashMap;
use std::io::Read as _;
use std::rc::Rc;

/// Mirrors `revision` in Cargo.toml; scripts/build-deb.sh bumps both after each package build.
pub(crate) const BUILD_NUMBER: u32 = 3;

pub const EXIT_OK: i32 = 0;
pub const EXIT_FAIL: i32 = 1;
pub const EXIT_USAGE: i32 = 2;
pub const EXIT_CHECK: i32 = 3;
pub const EXIT_CAPABILITY: i32 = 4;

const USAGE: &str = "\
Weir: scripts that glue programs together, checked before they run

usage:
  weir run   <file> [flow] [--flags]    run a flow (the `main` flow by default)
  weir <file> [flow] [--flags]         the same, so a `#!/usr/bin/env weir` script can be executed directly
  weir check <file> [--json]            check a script without running anything
  weir list  <file> [--manifest]        list flows, their inputs and outputs
  weir test  [path]                     run the `test` blocks of a file or folder
  weir lock  [file]                     pin remote components (none are supported yet)
  weir sandbox suggest <file>           suggest mounts for a script's programs

run flags:
  --confirm            also run `confirmable` steps
  --plan               print what would happen and run nothing
  --json               print one JSON result document; `print` goes to stderr
  --out <name>         print one named output as raw text; `print` goes to stderr
  --ro <inside-path>   turn a rw mount into ro for this run
  --deny-exec <name>   refuse to start a program for this run
  --secret-file <name>=<path>   read a `secret` input from a file (mode 0600)
  --secret-stdin <name>         read a `secret` input from stdin
  --no-sandbox         run without the filesystem sandbox (see docs/DESIGN.md 7.6)
  --help               show the flags a script generates from its inputs

exit codes: 0 success, 1 flow failure, 2 usage error, 3 check error, 4 capability denied
";

pub fn main_entry() -> i32 {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        None | Some("help") | Some("--help") | Some("-h") => {
            print!("{USAGE}");
            EXIT_OK
        }
        Some("--version") | Some("-V") => {
            println!("weir {}-{}", env!("CARGO_PKG_VERSION"), BUILD_NUMBER);
            EXIT_OK
        }
        Some("run") => cmd_run(&args[1..]),
        Some("check") => cmd_check(&args[1..]),
        Some("list") => cmd_list(&args[1..]),
        Some("test") => cmd_test(&args[1..]),
        Some("lock") => {
            eprintln!("weir lock: nothing to pin. This build imports only local components, and local paths are not pinned.");
            EXIT_OK
        }
        Some("sandbox") => match args.get(1).map(String::as_str) {
            Some("suggest") => cmd_sandbox_suggest(&args[2..]),
            _ => {
                eprintln!("usage: weir sandbox suggest <file>");
                EXIT_USAGE
            }
        },
        // `weir script.weir ...` is `weir run script.weir ...`, which is what a
        // `#!/usr/bin/env weir` line makes the kernel execute.
        Some(file) if file.ends_with(".weir") || std::path::Path::new(file).is_file() => cmd_run(&args),
        Some(other) => {
            eprintln!("weir: unknown command `{other}`\n\n{USAGE}");
            EXIT_USAGE
        }
    }
}

// ---- shared helpers -------------------------------------------------------

fn print_diags(diags: &[Diag]) {
    for d in diags {
        eprintln!("{}", d.render());
    }
}

fn units_of(prog: &Program) -> Vec<Unit> {
    let mut units: Vec<Unit> = prog.files.iter().map(|l| Unit::from_file(&l.file)).collect();
    for (i, l) in prog.files.iter().enumerate() {
        let mut imports: HashMap<String, (usize, Rc<Flow>)> = HashMap::new();
        for imp in &l.imports {
            for fl in prog.files[imp.file].file.flows() {
                let wanted = imp.name.as_deref().map(|n| n == fl.name).unwrap_or(fl.vis != Vis::Private);
                if wanted {
                    imports.entry(fl.name.clone()).or_insert((imp.file, Rc::new(fl.clone())));
                }
            }
        }
        units[i].imports = imports;
    }
    units
}

fn has_errors(d: &[Diag]) -> bool {
    d.iter().any(Diag::is_error)
}

// ---- weir check -----------------------------------------------------------

fn cmd_check(args: &[String]) -> i32 {
    let mut json = false;
    let mut file = None;
    for a in args {
        match a.as_str() {
            "--json" => json = true,
            s if s.starts_with("--") => {
                eprintln!("weir check: unknown flag `{s}`");
                return EXIT_USAGE;
            }
            s => file = Some(s.to_string()),
        }
    }
    let Some(file) = file else {
        eprintln!("usage: weir check <file> [--json]");
        return EXIT_USAGE;
    };
    let prog = load(&file);
    let diags = check_program(&prog);
    if json {
        println!("{}", Json::Arr(diags.iter().map(Diag::to_json).collect()).pretty());
    } else {
        print_diags(&diags);
        let (e, w) = (diags.iter().filter(|d| d.is_error()).count(), diags.iter().filter(|d| !d.is_error()).count());
        if diags.is_empty() {
            println!("{file}: ok");
        } else {
            eprintln!("{e} error(s), {w} warning(s)");
        }
    }
    if has_errors(&diags) {
        EXIT_CHECK
    } else {
        EXIT_OK
    }
}

// ---- weir list ------------------------------------------------------------

fn cmd_list(args: &[String]) -> i32 {
    let mut manifest = false;
    let mut file = None;
    for a in args {
        match a.as_str() {
            "--manifest" => manifest = true,
            s if s.starts_with("--") => {
                eprintln!("weir list: unknown flag `{s}`");
                return EXIT_USAGE;
            }
            s => file = Some(s.to_string()),
        }
    }
    let Some(file) = file else {
        eprintln!("usage: weir list <file> [--manifest]");
        return EXIT_USAGE;
    };
    let prog = load(&file);
    if prog.files.is_empty() {
        print_diags(&prog.diags);
        return EXIT_CHECK;
    }
    let f = &prog.files[0].file;
    for fl in f.flows() {
        let vis = match fl.vis {
            Vis::Main => "main ",
            Vis::Private => "private ",
            Vis::Normal => "",
        };
        println!("{vis}flow {}", fl.name);
        for i in &fl.ins {
            let d = match (&i.default, i.ty.is_optional()) {
                (Some(d), _) => format!(" = {}", crate::pretty::show_expr(d)),
                (None, true) => " (optional)".into(),
                (None, false) => " (required)".into(),
            };
            println!("  in  {}: {}{d}", i.name, i.ty.show());
        }
        for o in &fl.outs {
            println!("  out {}: {}", o.name, o.ty.show());
        }
    }
    if manifest {
        println!();
        match build_mounts(f, &HashMap::new(), true) {
            Ok(m) => {
                println!("mounts (host -> inside):");
                print!("{}", m.describe());
            }
            Err(e) => println!("mounts: {e}"),
        }
        println!("programs a filter may start:");
        let mut progs: Vec<&str> = f.filters().map(|x| x.program.as_str()).collect();
        progs.sort();
        progs.dedup();
        for p in progs {
            println!("  {p}");
        }
        for fl in f.filters() {
            if let Some(sees) = &fl.see {
                let desc: Vec<String> = sees
                    .iter()
                    .map(|(s, _)| match s {
                        See::System => "system".to_string(),
                        See::Path { path, mode } => format!("{} {}", crate::pretty::show_expr(path), mode.word()),
                    })
                    .collect();
                println!("filter {} sees: {}", fl.name, desc.join(", "));
            }
        }
    }
    EXIT_OK
}

// ---- weir sandbox suggest -------------------------------------------------

fn cmd_sandbox_suggest(_args: &[String]) -> i32 {
    eprintln!("weir sandbox suggest: not implemented yet in this build");
    EXIT_USAGE
}

// ---- mounts ---------------------------------------------------------------

/// Evaluate the sandbox block's mounts. `lenient` skips host-path existence concerns (for `list`).
pub fn build_mounts(f: &File, sandbox_ins: &HashMap<String, Value>, lenient: bool) -> Result<Mounts, String> {
    let mut m = Mounts::default();
    let Some(sb) = &f.sandbox else {
        m.add("/", "/", Mode::Rw, "unconfined");
        return Ok(m);
    };
    let mut it = Interp::new(Options::default(), Mounts::default(), vec![]);
    let binds: Vec<(String, Value)> = sandbox_ins.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    let mut all = binds;
    // defaults for sandbox `in` values not supplied
    for i in &sb.ins {
        if !all.iter().any(|(k, _)| k == &i.name) {
            if let Some(d) = &i.default {
                let mut v = it.eval_with(d, &all).map_err(|e| format!("sandbox input `{}`: {}", i.name, describe_ctl(&e)))?;
                // a host path may be written as a plain string in the sandbox block
                if let (Ty::Path, Value::Str(s)) = (strip_opt(&i.ty), &v) {
                    v = Value::path(absolutize(s));
                }
                all.push((i.name.clone(), v));
            } else if !lenient {
                return Err(format!("sandbox input `{}` has no value; pass --{}", i.name, i.name.replace('_', "-")));
            } else {
                all.push((i.name.clone(), Value::path(format!("<{}>", i.name))));
            }
        }
    }
    for mt in &sb.mounts {
        match &mt.spec {
            MountSpec::System => m.add_system(),
            MountSpec::Path { host, at, mode } => {
                let hv = it.eval_with(host, &all).map_err(|e| format!("line {}: {}", mt.span.line, describe_ctl(&e)))?;
                let host_s = match &hv {
                    Value::Str(s) | Value::Path(s) => s.to_string(),
                    v => return Err(format!("line {}: a mount path must be a path or string, found {}", mt.span.line, v.type_name())),
                };
                let inside_s = match at {
                    Some(a) => match it.eval_with(a, &all).map_err(|e| format!("line {}: {}", mt.span.line, describe_ctl(&e)))? {
                        Value::Str(s) | Value::Path(s) => s.to_string(),
                        v => return Err(format!("line {}: a mount's inside path must be a path or string, found {}", mt.span.line, v.type_name())),
                    },
                    None => host_s.clone(),
                };
                if !inside_s.starts_with('/') {
                    return Err(format!("line {}: the inside path {inside_s:?} must be absolute", mt.span.line));
                }
                let host_abs = absolutize(&host_s);
                m.add(&host_abs, &inside_s, *mode, "declared");
            }
        }
    }
    m.add_fixed();
    Ok(m)
}

fn absolutize(p: &str) -> String {
    if p.starts_with('/') {
        normalize(p)
    } else {
        let cwd = std::env::current_dir().map(|c| c.to_string_lossy().into_owned()).unwrap_or_else(|_| "/".into());
        normalize(&format!("{cwd}/{p}"))
    }
}

// ---- weir run -------------------------------------------------------------

#[derive(Default)]
struct RunFlags {
    confirm: bool,
    plan: bool,
    json: bool,
    out: Option<String>,
    no_sandbox: bool,
    ro: Vec<String>,
    deny_exec: Vec<String>,
    secret_files: Vec<(String, String)>,
    secret_stdin: Vec<String>,
    help: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Bool,
    One,
    Many,
}

struct Spec {
    name: String,
    ty: Ty,
    kind: Kind,
}

fn flag_kind(ty: &Ty) -> Kind {
    match ty {
        Ty::Bool => Kind::Bool,
        Ty::List(_) => Kind::Many,
        Ty::Opt(t) => flag_kind(t),
        _ => Kind::One,
    }
}

fn flag_name(name: &str) -> String {
    format!("--{}", name.replace('_', "-"))
}

fn norm_flag(s: &str) -> String {
    s.replace('_', "-")
}

fn cmd_run(args: &[String]) -> i32 {
    let Some(file) = args.first().filter(|a| !a.starts_with("--")) else {
        eprintln!("usage: weir run <file> [flow] [--flags]");
        return EXIT_USAGE;
    };
    let mut rest: &[String] = &args[1..];

    let prog = load(file);
    let diags = check_program(&prog);
    print_diags(&diags);
    if has_errors(&diags) || prog.files.is_empty() {
        eprintln!("weir: {file} has check errors; nothing was run");
        return EXIT_CHECK;
    }
    let entry = &prog.files[0].file;

    // Which flow?
    let mut flow_name: Option<String> = None;
    if let Some(first) = rest.first() {
        if !first.starts_with("--") {
            flow_name = Some(first.clone());
            rest = &rest[1..];
        }
    }
    let wants_help = rest.iter().any(|a| a == "--help");
    let flow = match &flow_name {
        Some(n) => match entry.flows().find(|f| &f.name == n) {
            Some(f) if f.vis == Vis::Private => {
                eprintln!("weir: flow `{n}` is private and cannot be run from the command line");
                return EXIT_USAGE;
            }
            Some(f) => f,
            None => {
                let names: Vec<&str> = entry.flows().filter(|f| f.vis != Vis::Private).map(|f| f.name.as_str()).collect();
                eprintln!("weir: {file} has no flow `{n}`; runnable flows: {}", names.join(", "));
                return EXIT_USAGE;
            }
        },
        None => {
            let runnable: Vec<&Flow> = entry.flows().filter(|f| f.vis != Vis::Private).collect();
            if let Some(m) = entry.flows().find(|f| f.vis == Vis::Main) {
                m
            } else if runnable.len() == 1 {
                runnable[0]
            } else {
                eprintln!(
                    "weir: {file} has no `main` flow; name one to run: {}",
                    runnable.iter().map(|f| f.name.as_str()).collect::<Vec<_>>().join(", ")
                );
                return EXIT_USAGE;
            }
        }
    };

    // Flag table: run flags, the sandbox block's inputs, and the flow's inputs.
    let mut specs: Vec<Spec> = Vec::new();
    let mut sandbox_specs: Vec<Spec> = Vec::new();
    if let Some(sb) = &entry.sandbox {
        for i in &sb.ins {
            sandbox_specs.push(Spec { name: i.name.clone(), ty: i.ty.clone(), kind: flag_kind(&i.ty) });
        }
    }
    for i in &flow.ins {
        specs.push(Spec { name: i.name.clone(), ty: i.ty.clone(), kind: flag_kind(&i.ty) });
    }
    if wants_help {
        print_flow_help(file, flow, entry);
        return EXIT_OK;
    }

    let mut flags = RunFlags::default();
    let mut given: HashMap<String, Vec<String>> = HashMap::new();
    let mut given_sandbox: HashMap<String, Vec<String>> = HashMap::new();
    let mut i = 0;
    let usage_err = |msg: String| -> i32 {
        eprintln!("weir: {msg}\n(see `weir run {file} --help`)");
        EXIT_USAGE
    };
    while i < rest.len() {
        let tok = &rest[i];
        i += 1;
        let Some(body) = tok.strip_prefix("--") else {
            return usage_err(format!("unexpected argument `{tok}`; flags start with `--`"));
        };
        let (name, inline): (String, Option<String>) = match body.split_once('=') {
            Some((n, v)) => (n.to_string(), Some(v.to_string())),
            None => (body.to_string(), None),
        };
        let name_n = norm_flag(&name);
        macro_rules! value {
            () => {{
                match inline.clone() {
                    Some(v) => v,
                    None => {
                        if i < rest.len() {
                            i += 1;
                            rest[i - 1].clone()
                        } else {
                            return usage_err(format!("`--{name}` needs a value"));
                        }
                    }
                }
            }};
        }
        match name_n.as_str() {
            "confirm" => flags.confirm = true,
            "plan" => flags.plan = true,
            "json" => flags.json = true,
            "no-sandbox" => flags.no_sandbox = true,
            "out" => flags.out = Some(value!()),
            "ro" => flags.ro.push(value!()),
            "deny-exec" => flags.deny_exec.push(value!()),
            "secret-file" => {
                let v = value!();
                match v.split_once('=') {
                    Some((n, p)) => flags.secret_files.push((n.to_string(), p.to_string())),
                    None => return usage_err("`--secret-file` takes <name>=<path>".into()),
                }
            }
            "secret-stdin" => flags.secret_stdin.push(value!()),
            "help" => flags.help = true,
            _ => {
                // an input of the flow, or of the sandbox block
                let (is_no, base) = match name_n.strip_prefix("no-") {
                    Some(b) if specs.iter().chain(sandbox_specs.iter()).any(|s| norm_flag(&s.name) == b && s.kind == Kind::Bool) => (true, b.to_string()),
                    _ => (false, name_n.clone()),
                };
                let (spec, table) = if let Some(s) = specs.iter().find(|s| norm_flag(&s.name) == base) {
                    (s, &mut given)
                } else if let Some(s) = sandbox_specs.iter().find(|s| norm_flag(&s.name) == base) {
                    (s, &mut given_sandbox)
                } else {
                    let mut valid: Vec<String> = ["--confirm", "--plan", "--json", "--out", "--ro", "--deny-exec", "--secret-file", "--secret-stdin", "--no-sandbox", "--help"]
                        .iter()
                        .map(|s| s.to_string())
                        .collect();
                    valid.extend(specs.iter().chain(sandbox_specs.iter()).map(|s| flag_name(&s.name)));
                    return usage_err(format!("unknown flag `--{name}`; valid flags: {}", valid.join(" ")));
                };
                if matches!(strip_opt(&spec.ty), Ty::Secret) {
                    return usage_err(format!(
                        "`--{name}` is a secret and is never accepted on the command line (it would show in `ps` and shell history); \
                         use the environment variable WEIR_SECRET_{}, `--secret-file {}=<path>`, or `--secret-stdin {}`",
                        spec.name.to_uppercase(),
                        spec.name,
                        spec.name
                    ));
                }
                let spec_name = spec.name.clone();
                let kind = spec.kind;
                let v = match kind {
                    Kind::Bool => match inline.clone() {
                        Some(v) => v,
                        None => if is_no { "false" } else { "true" }.to_string(),
                    },
                    _ => value!(),
                };
                let entry_v = table.entry(spec_name).or_default();
                if kind != Kind::Many && !entry_v.is_empty() {
                    return usage_err(format!("`--{name}` is given more than once"));
                }
                entry_v.push(v);
            }
        }
    }
    if flags.help {
        print_flow_help(file, flow, entry);
        return EXIT_OK;
    }

    // Sandbox-block inputs (host paths, evaluated before the sandbox exists).
    let mut sandbox_vals: HashMap<String, Value> = HashMap::new();
    for s in &sandbox_specs {
        if let Some(raw) = given_sandbox.get(&s.name) {
            match to_value(&s.ty, raw, None) {
                Ok(v) => {
                    sandbox_vals.insert(s.name.clone(), v);
                }
                Err(e) => return usage_err(format!("`{}`: {e}", flag_name(&s.name))),
            }
        }
    }
    let mut mounts = match build_mounts(entry, &sandbox_vals, false) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("weir: {file}: sandbox: {e}");
            return EXIT_USAGE;
        }
    };
    for r in &flags.ro {
        if !mounts.make_ro(r) {
            eprintln!("weir: --ro {r}: no mount has that inside path; mounts:\n{}", mounts.describe());
            return EXIT_USAGE;
        }
    }
    if flags.no_sandbox {
        if let Some(e) = mounts.entries.iter().find(|e| e.host != e.inside && e.origin == "declared") {
            eprintln!(
                "weir: --no-sandbox cannot be used here: the mount of {} at {} remaps a path, and without a sandbox every inside path is a host path",
                e.host, e.inside
            );
            return EXIT_USAGE;
        }
        eprintln!("weir: warning: running without the filesystem sandbox (--no-sandbox)");
    }

    // Flow inputs.
    let mut inputs: Vec<(String, Value)> = Vec::new();
    for s in &specs {
        let secret = matches!(strip_opt(&s.ty), Ty::Secret);
        if secret {
            match read_secret(&s.name, &flags) {
                Ok(Some(v)) => inputs.push((s.name.clone(), v)),
                Ok(None) => {
                    if !s.ty.is_optional() {
                        return usage_err(format!(
                            "the secret input `{}` has no value; set WEIR_SECRET_{}, or pass `--secret-file {}=<path>` or `--secret-stdin {}`",
                            s.name,
                            s.name.to_uppercase(),
                            s.name,
                            s.name
                        ));
                    }
                }
                Err(e) => return usage_err(e),
            }
            continue;
        }
        if let Some(raw) = given.get(&s.name) {
            match to_value(&s.ty, raw, Some(&mounts)) {
                Ok(v) => inputs.push((s.name.clone(), v)),
                Err(e) => return usage_err(format!("`{}`: {e}", flag_name(&s.name))),
            }
        } else if !s.ty.is_optional() && flow.ins.iter().find(|i| i.name == s.name).is_some_and(|i| i.default.is_none()) {
            return usage_err(format!("the required input `{}` is missing; pass {} <{}>", s.name, flag_name(&s.name), s.ty.show()));
        }
    }

    // Enter the sandbox (everything above needed host paths).
    let sandboxed = if entry.sandbox.is_some() && !flags.no_sandbox && !flags.plan {
        match crate::sandbox::enter(&mounts) {
            Ok(Some(code)) => return code, // the parent of the sandbox stage: child's exit status
            Ok(None) => {
                mounts = mounts.inside_view();
                true
            }
            Err(e) => {
                eprintln!("weir: the sandbox could not be created: {e}\n(pass --no-sandbox only on hosts where a sandbox cannot be created)");
                return EXIT_CAPABILITY;
            }
        }
    } else {
        false
    };

    let opts = Options {
        confirm: flags.confirm,
        plan: flags.plan,
        print_to_stderr: flags.json || flags.out.is_some(),
        deny_exec: flags.deny_exec.clone(),
    };
    if flags.plan {
        println!("plan for flow `{}`; nothing is executed. Commands report empty results, so branches that depend on them may differ from a real run.", flow.name);
        println!("mounts:\n{}", mounts.describe().trim_end());
        if entry.sandbox.is_some() && !flags.no_sandbox {
            println!("(sandbox: would be entered for a real run)");
        }
    }
    let mut interp = Interp::new(opts, mounts, units_of(&prog));
    interp.sandboxed = sandboxed;
    let flow_rc = Rc::new(flow.clone());
    let (outs, result) = interp.run_entry(0, &flow_rc, inputs);

    let (exit, error) = match &result {
        Ok(()) => (EXIT_OK, None),
        Err(c) => (EXIT_FAIL, Some(render_error(&entry.path, c, &flow.name))),
    };
    if let Some(msg) = &error {
        if !(flags.plan && matches!(&result, Err(c) if matches!(**c, Ctl::Fail { .. }))) {
            eprintln!("{msg}");
        } else {
            println!("plan: stops here: {msg}");
        }
    }
    let exit = if flags.plan { EXIT_OK } else { exit };
    if flags.json {
        println!("{}", result_json(exit, &outs, error.as_deref(), &interp.steps, sandboxed).pretty());
    } else if let Some(name) = &flags.out {
        match outs.iter().find(|(n, _)| n == name) {
            Some((_, v)) => match v {
                Value::List(l) => {
                    for x in l.iter() {
                        println!("{}", x.display());
                    }
                }
                Value::Absent => {}
                v => println!("{}", v.display()),
            },
            None => {
                eprintln!("weir: --out {name}: the flow has no output `{name}`; outputs: {}", outs.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>().join(", "));
                return EXIT_USAGE;
            }
        }
    }
    exit
}

fn strip_opt(t: &Ty) -> &Ty {
    match t {
        Ty::Opt(i) => strip_opt(i),
        t => t,
    }
}

fn render_error(file: &str, c: &Ctl, flow: &str) -> String {
    let (span, path) = ctl_location(c);
    let place = match span {
        Some(s) => format!("{file}:{}:{}", s.line, s.col),
        None => file.to_string(),
    };
    let step = match path {
        Some(p) => format!("{flow} > {p}"),
        None => flow.to_string(),
    };
    format!("{place}: error in {step}: {}", describe_ctl(c))
}

fn result_json(exit: i32, outs: &[(String, Value)], error: Option<&str>, steps: &[StepRec], sandbox: bool) -> Json {
    let mut doc = vec![
        ("ok".to_string(), Json::Bool(exit == 0)),
        ("exit".to_string(), Json::Int(exit as i64)),
        ("sandbox".to_string(), Json::Bool(sandbox)),
        ("outputs".to_string(), Json::Obj(outs.iter().map(|(k, v)| (k.clone(), v.to_json())).collect())),
    ];
    if let Some(e) = error {
        doc.push(("error".into(), Json::Obj(vec![("message".into(), Json::Str(e.to_string()))])));
    }
    doc.push((
        "steps".into(),
        Json::Arr(
            steps
                .iter()
                .map(|s| {
                    let mut o = vec![("name".to_string(), Json::Str(s.name.clone())), ("status".to_string(), Json::Str(s.status.to_string()))];
                    if let Some(d) = &s.detail {
                        o.push(("detail".into(), Json::Str(d.clone())));
                    }
                    Json::Obj(o)
                })
                .collect(),
        ),
    ));
    Json::Obj(doc)
}

fn print_flow_help(file: &str, flow: &Flow, entry: &File) {
    println!("usage: weir run {file} {} [flags]\n", flow.name);
    println!("inputs:");
    let mut any = false;
    let mut show = |i: &InDecl, host: bool| {
        any = true;
        let n = flag_name(&i.name);
        let ty = match strip_opt(&i.ty) {
            Ty::Bool => String::new(),
            Ty::List(t) => format!(" <{}> (repeatable)", t.show()),
            Ty::Enum(v) => format!(" <{}>", v.join("|")),
            Ty::Secret => " (secret: never on the command line)".to_string(),
            t => format!(" <{}>", t.show()),
        };
        let extra = match (&i.default, i.ty.is_optional()) {
            (Some(d), _) => format!("  [default: {}]", crate::pretty::show_expr(d)),
            (None, true) => "  [optional]".into(),
            (None, false) => "  [required]".into(),
        };
        println!("  {n}{ty}{extra}{}", if host { "  (host path, sandbox)" } else { "" });
    };
    if let Some(sb) = &entry.sandbox {
        for i in &sb.ins {
            show(i, true);
        }
    }
    for i in &flow.ins {
        show(i, false);
    }
    if !any {
        println!("  (none)");
    }
    println!("\noutputs:");
    if flow.outs.is_empty() {
        println!("  (none)");
    }
    for o in &flow.outs {
        println!("  {}: {}", o.name, o.ty.show());
    }
    println!("\nrun flags: --confirm --plan --json --out <name> --ro <path> --deny-exec <name> --no-sandbox");
}

fn read_secret(name: &str, flags: &RunFlags) -> Result<Option<Value>, String> {
    use std::os::unix::fs::PermissionsExt;
    if let Some((_, path)) = flags.secret_files.iter().find(|(n, _)| n == name) {
        let meta = std::fs::metadata(path).map_err(|e| format!("--secret-file {name}={path}: {e}"))?;
        if meta.permissions().mode() & 0o077 != 0 {
            return Err(format!(
                "--secret-file {name}={path}: the file is readable by group or others (mode {:o}); run `chmod 600 {path}`",
                meta.permissions().mode() & 0o777
            ));
        }
        let text = std::fs::read_to_string(path).map_err(|e| format!("--secret-file {name}={path}: {e}"))?;
        return Ok(Some(Value::Secret(Rc::from(text.trim_end_matches('\n')))));
    }
    if flags.secret_stdin.iter().any(|n| n == name) {
        let mut s = String::new();
        std::io::stdin().read_to_string(&mut s).map_err(|e| format!("--secret-stdin {name}: {e}"))?;
        return Ok(Some(Value::Secret(Rc::from(s.trim_end_matches('\n')))));
    }
    if let Ok(v) = std::env::var(format!("WEIR_SECRET_{}", name.to_uppercase())) {
        return Ok(Some(Value::Secret(Rc::from(v.as_str()))));
    }
    Ok(None)
}

/// Convert raw flag text into a value of the declared type. `mounts` translates host paths;
/// `None` leaves path values as host paths (sandbox-block inputs).
fn to_value(ty: &Ty, raw: &[String], mounts: Option<&Mounts>) -> Result<Value, String> {
    match ty {
        Ty::Opt(t) => to_value(t, raw, mounts),
        Ty::List(t) => {
            let mut out = Vec::new();
            for r in raw {
                out.push(to_value(t, std::slice::from_ref(r), mounts)?);
            }
            Ok(Value::list(out))
        }
        _ => {
            let r = raw.first().ok_or("no value")?;
            match ty {
                Ty::Str => Ok(Value::str(r)),
                Ty::Int => r.trim().parse::<i64>().map(Value::Int).map_err(|_| format!("{r:?} is not an int")),
                Ty::Bool => match r.as_str() {
                    "true" => Ok(Value::Bool(true)),
                    "false" => Ok(Value::Bool(false)),
                    _ => Err(format!("{r:?} is not a bool (use true or false)")),
                },
                Ty::Duration => parse_duration(r).map(Value::Dur).ok_or_else(|| format!("{r:?} is not a duration; write 30s, 15m, 2h or 21d")),
                Ty::Enum(vals) => {
                    if vals.contains(r) {
                        Ok(Value::str(r))
                    } else {
                        Err(format!("{r:?} is not one of: {}", vals.join(", ")))
                    }
                }
                Ty::Path => {
                    let abs = absolutize(r);
                    match mounts {
                        None => Ok(Value::path(abs)),
                        Some(m) => match m.host_to_inside(&abs) {
                            Ok(Some(inside)) => Ok(Value::path(inside)),
                            Ok(None) => Err(format!("the host path {abs} is not under any mount, so the script cannot see it; mounts:\n{}", m.describe().trim_end())),
                            Err(c) => Err(format!("the host path {abs} is ambiguous; it maps to: {}", c.join(", "))),
                        },
                    }
                }
                other => Err(format!("a {} cannot be given on the command line", other.show())),
            }
        }
    }
}

// ---- weir test ------------------------------------------------------------

fn collect_weir_files(path: &str, out: &mut Vec<String>, depth: u32) {
    let p = std::path::Path::new(path);
    if p.is_file() {
        out.push(path.to_string());
    } else if p.is_dir() && depth < 6 {
        let mut entries: Vec<_> = std::fs::read_dir(p).map(|r| r.flatten().map(|e| e.path()).collect()).unwrap_or_default();
        entries.sort();
        for e in entries {
            let name = e.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name.starts_with('.') || name == "target" {
                continue;
            }
            if e.is_dir() {
                collect_weir_files(&e.to_string_lossy(), out, depth + 1);
            } else if e.extension().and_then(|x| x.to_str()) == Some("weir") {
                out.push(e.to_string_lossy().into_owned());
            }
        }
    }
}

fn cmd_test(args: &[String]) -> i32 {
    let target = args.first().cloned().unwrap_or_else(|| ".".into());
    let mut files = Vec::new();
    collect_weir_files(&target, &mut files, 0);
    if files.is_empty() {
        eprintln!("weir test: no .weir files under {target}");
        return EXIT_USAGE;
    }
    let (mut passed, mut failed, mut broken) = (0, 0, 0);
    for f in &files {
        let prog = load(f);
        let diags = check_program(&prog);
        print_diags(&diags);
        if has_errors(&diags) || prog.files.is_empty() {
            eprintln!("weir test: {f} has check errors; its tests were not run");
            broken += 1;
            continue;
        }
        let tests: Vec<&Test> = prog.files[0].file.tests().collect();
        if tests.is_empty() {
            continue;
        }
        println!("{f}");
        for t in tests {
            match run_test(&prog, t) {
                Ok(()) => {
                    passed += 1;
                    println!("  ok    {}", t.name);
                }
                Err(msg) => {
                    failed += 1;
                    println!("  FAIL  {}\n        {}", t.name, msg.replace('\n', "\n        "));
                }
            }
        }
    }
    println!("\n{passed} passed, {failed} failed{}", if broken > 0 { format!(", {broken} file(s) did not check") } else { String::new() });
    if broken > 0 {
        EXIT_CHECK
    } else if failed > 0 {
        EXIT_FAIL
    } else {
        EXIT_OK
    }
}

static TEST_COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

fn run_test(prog: &Program, t: &Test) -> Result<(), String> {
    let n = TEST_COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let root = std::env::temp_dir().join(format!("weir-test-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let mut mounts = Mounts::default();
    let result = (|| -> Result<(), String> {
        let mut k = 0;
        for item in &t.items {
            if let TestItem::Fixture { dir, items: files, .. } = item {
                let host = root.join(format!("m{k}"));
                k += 1;
                std::fs::create_dir_all(&host).map_err(|e| format!("cannot create the fixture folder: {e}"))?;
                mounts.add(&host.to_string_lossy(), dir, Mode::Rw, "declared");
                let mut it = Interp::new(Options::default(), Mounts::default(), vec![]);
                for FixtureItem::File { name, text } in files {
                    let body = it.eval_with(text, &[]).map_err(|e| describe_ctl(&e))?.display();
                    let p = host.join(name.trim_start_matches('/'));
                    if let Some(parent) = p.parent() {
                        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                    }
                    std::fs::write(&p, body).map_err(|e| format!("cannot write fixture file {name}: {e}"))?;
                }
            }
        }
        let mut interp = Interp::new(Options::default(), mounts, units_of(prog));
        interp.test_mode = true;
        let mut frame = interp.test_frame(0);
        for item in &t.items {
            if let TestItem::Stub { filter, yields, .. } = item {
                let v = interp.test_eval(yields, &mut frame).map_err(|e| describe_ctl(&e))?;
                let f = prog.files[0].file.filters().find(|f| &f.name == filter);
                if let Some(f) = f {
                    let expect = match f.yields {
                        IoKind::Lines => Some(Ty::List(Box::new(Ty::Str))),
                        IoKind::Text => Some(Ty::Str),
                        _ => None,
                    };
                    if let Some(ty) = expect {
                        conforms(&v, &ty).map_err(|e| format!("the stub for `{filter}` yields {}, so it must be a {}: {e}", f.yields.word(), ty.show()))?;
                    }
                }
                interp.stubs.insert(filter.clone(), v);
            }
        }
        for item in &t.items {
            if let TestItem::Stmt(s) = item {
                interp.test_stmt(s, &mut frame).map_err(|e| {
                    let (span, _) = ctl_location(&e);
                    match span {
                        Some(sp) => format!("line {}: {}", sp.line, describe_ctl(&e)),
                        None => describe_ctl(&e),
                    }
                })?;
            }
        }
        Ok(())
    })();
    let _ = std::fs::remove_dir_all(&root);
    result
}
