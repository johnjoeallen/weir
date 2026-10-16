//! `weir check`: names, arguments, structure, purity, sandbox declarations and lint rules.
//! It never runs a command. The one thing it reads beyond source files is `PATH`, to
//! confirm each program a filter names can be started inside the declared sandbox.
//!
//! Not done here (see the design notes): full type inference through pipelines, `secret`
//! tracking through `fn` returns and record fields, and catch-reachability analysis.

use crate::ast::*;
use crate::builtins::{self, Effect};
use crate::diag::Diag;
use crate::error::CATCHABLE;
use crate::loader::Program;
use crate::mounts::{normalize, SYSTEM_SET};
use std::collections::{HashMap, HashSet};

const NATIVE_STAGES: &[&str] = &["lines", "map", "filter", "take", "unique", "count", "count_by", "sort_by"];
const DESTRUCTIVE: &[&str] = &["rm", "mv", "dd", "chmod", "chown", "mkfs", "shred", "truncate"];
const SHELLS: &[&str] = &["bash", "sh", "zsh", "dash", "ksh", "fish"];

/// What the entry file's sandbox block statically declares.
#[derive(Default)]
struct StaticMounts {
    present: bool,
    /// (inside path, mode) for every mount whose inside path is a literal.
    known: Vec<(String, Mode)>,
    /// False if some mount's inside path is computed (a sandbox `in` value on the host side only).
    all_known: bool,
}

impl StaticMounts {
    fn find(&self, p: &str) -> Option<(&str, Mode)> {
        let p = normalize(p);
        self.known
            .iter()
            .filter(|(i, _)| i == "/" || p == *i || p.starts_with(&format!("{i}/")))
            .max_by_key(|(i, _)| i.len())
            .map(|(i, m)| (i.as_str(), *m))
    }
}

pub fn check_program(prog: &Program) -> Vec<Diag> {
    let mut diags: Vec<Diag> = prog.diags.clone();
    if prog.files.is_empty() {
        return diags;
    }
    let mounts = static_mounts(&prog.files[0].file);
    for (i, l) in prog.files.iter().enumerate() {
        let mut cx = Cx::new(prog, i, &mounts);
        cx.file();
        let mut found = cx.diags;
        let allowed = allow_pragmas(&l.src);
        found.retain(|d| !allowed.iter().any(|(line, code)| d.line == *line && &d.code == code));
        diags.extend(found);
    }
    diags.sort_by(|a, b| (a.file.as_str(), a.line, a.col).cmp(&(b.file.as_str(), b.line, b.col)));
    diags
}

/// `// weir:allow CODE` suppresses CODE on the next line.
fn allow_pragmas(src: &str) -> Vec<(u32, String)> {
    let mut out = Vec::new();
    for (i, line) in src.lines().enumerate() {
        if let Some(pos) = line.find("weir:allow") {
            if line[..pos].trim_end().ends_with("//") {
                for code in line[pos + "weir:allow".len()..].split_whitespace() {
                    out.push((i as u32 + 2, code.to_string()));
                }
            }
        }
    }
    out
}

fn literal_str(e: &Expr) -> Option<String> {
    match &e.kind {
        ExprKind::Str(segs) => {
            let mut s = String::new();
            for seg in segs {
                match seg {
                    StrSeg::Lit(l) => s.push_str(l),
                    StrSeg::Expr(_) => return None,
                }
            }
            Some(s)
        }
        // path(text: "...")
        ExprKind::Call { name, args } if name == "path" && args.len() == 1 => literal_str(&args[0].expr),
        _ => None,
    }
}

/// Does every path through the block end in `return`, `fail` or a thrown error from an `if`/`else` that does?
fn block_returns(b: &Block) -> bool {
    b.stmts.iter().any(|s| match &s.kind {
        StmtKind::Return(_) | StmtKind::Fail(_) => true,
        StmtKind::If { then, else_: Some(e), .. } => block_returns(then) && block_returns(e),
        StmtKind::IfLet { then, else_: Some(e), .. } => block_returns(then) && block_returns(e),
        _ => false,
    })
}

fn static_mounts(f: &File) -> StaticMounts {
    let mut m = StaticMounts { present: f.sandbox.is_some(), all_known: true, ..Default::default() };
    if let Some(sb) = &f.sandbox {
        for mt in &sb.mounts {
            match &mt.spec {
                MountSpec::System => {
                    for p in SYSTEM_SET {
                        m.known.push((p.to_string(), Mode::Ro));
                    }
                }
                MountSpec::Path { host, at, mode, .. } => {
                    let inside = match at {
                        Some(a) => literal_str(a),
                        None => literal_str(host),
                    };
                    match inside {
                        Some(i) => m.known.push((normalize(&i), *mode)),
                        None => m.all_known = false,
                    }
                }
            }
        }
        m.known.push(("/tmp".into(), Mode::Rw));
    }
    m
}

fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for i in 1..=a.len() {
        let mut cur = vec![i];
        for j in 1..=b.len() {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            cur.push((prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost));
        }
        prev = cur;
    }
    prev[b.len()]
}

fn suggest(name: &str, candidates: &[String]) -> Option<String> {
    candidates
        .iter()
        .map(|c| (levenshtein(name, c), c))
        .filter(|(d, c)| *d <= (name.len().max(c.len()) / 3).max(1))
        .min_by_key(|(d, _)| *d)
        .map(|(_, c)| c.clone())
}

#[derive(Clone)]
struct Binding {
    ty: Option<Ty>,
    mutable: bool,
}

struct ParamInfo {
    name: String,
    required: bool,
    ty: Option<Ty>,
    default: Option<Expr>,
}

#[derive(Clone, Copy, Default)]
struct Want {
    /// An absent-able value may be used here without unwrapping.
    opt_ok: bool,
    /// A secret may be used here.
    secret_ok: bool,
}

struct Cx<'a> {
    prog: &'a Program,
    fidx: usize,
    file: &'a File,
    mounts: &'a StaticMounts,
    diags: Vec<Diag>,
    flows: HashMap<String, &'a Flow>,
    filters: HashMap<String, &'a Filter>,
    fns: HashMap<String, &'a FnDecl>,
    scopes: Vec<HashMap<String, Binding>>,
    in_fn: bool,
    within_depth: usize,
    in_test: bool,
    in_requires: bool,
    stage_depth: u32,
    loop_depth: u32,
    steps_seen: HashSet<String>,
    used_filters: HashSet<String>,
    warned_no_sandbox: bool,
    sets_seen: HashSet<String>,
    /// Names an import was supposed to provide but could not (already reported).
    unresolved: HashSet<String>,
    /// A whole-file import failed, so any unknown callable may have come from it.
    unresolved_all: bool,
    /// Inside the arguments of a call to an unresolved name: do not report unknown names.
    quiet_names: u32,
    /// Outputs of the flow being checked (write-only).
    cur_outs: HashSet<String>,
}

impl<'a> Cx<'a> {
    fn new(prog: &'a Program, fidx: usize, mounts: &'a StaticMounts) -> Cx<'a> {
        let file = &prog.files[fidx].file;
        let mut cx = Cx {
            prog,
            fidx,
            file,
            mounts,
            diags: vec![],
            flows: HashMap::new(),
            filters: HashMap::new(),
            fns: HashMap::new(),
            scopes: vec![],
            in_fn: false,
            within_depth: 0,
            in_test: false,
            in_requires: false,
            stage_depth: 0,
            loop_depth: 0,
            steps_seen: HashSet::new(),
            used_filters: HashSet::new(),
            warned_no_sandbox: false,
            sets_seen: HashSet::new(),
            unresolved: HashSet::new(),
            unresolved_all: false,
            quiet_names: 0,
            cur_outs: HashSet::new(),
        };
        for (f, n) in &prog.failed_imports {
            if *f == fidx {
                match n {
                    Some(n) => {
                        cx.unresolved.insert(n.clone());
                    }
                    None => cx.unresolved_all = true,
                }
            }
        }
        for f in file.flows() {
            cx.flows.insert(f.name.clone(), f);
        }
        for f in file.filters() {
            cx.filters.insert(f.name.clone(), f);
        }
        for f in file.fns() {
            cx.fns.insert(f.name.clone(), f);
        }
        for imp in &prog.files[fidx].imports {
            let other = &prog.files[imp.file].file;
            for fl in other.flows() {
                let wanted = imp.name.as_deref().map(|n| n == fl.name).unwrap_or(fl.vis != Vis::Private);
                if wanted {
                    cx.flows.entry(fl.name.clone()).or_insert(fl);
                }
            }
        }
        cx
    }

    fn err(&mut self, span: Span, code: &str, msg: String, fix: Option<String>) {
        let file = self.file.path.clone();
        self.diags.push(Diag::error(&file, span.line, span.col, code, msg, fix));
    }

    fn warn(&mut self, span: Span, code: &str, msg: String, fix: Option<String>) {
        let file = self.file.path.clone();
        self.diags.push(Diag::warning(&file, span.line, span.col, code, msg, fix));
    }

    // ---- scopes -----------------------------------------------------------

    fn push(&mut self) {
        self.scopes.push(HashMap::new());
    }

    fn pop(&mut self) {
        self.scopes.pop();
    }

    fn bind(&mut self, name: &str, ty: Option<Ty>, mutable: bool) {
        if let Some(s) = self.scopes.last_mut() {
            s.insert(name.to_string(), Binding { ty, mutable });
        }
    }

    fn lookup(&self, name: &str) -> Option<&Binding> {
        self.scopes.iter().rev().find_map(|s| s.get(name))
    }

    fn names_in_scope(&self) -> Vec<String> {
        let mut v: Vec<String> = self.scopes.iter().flat_map(|s| s.keys().cloned()).collect();
        v.sort();
        v.dedup();
        v
    }

    // ---- file -------------------------------------------------------------

    fn file(&mut self) {
        let f = self.file;
        // Imports.
        for imp in &self.prog.files[self.fidx].imports {
            let other = &self.prog.files[imp.file].file;
            if let Some(n) = &imp.name {
                match other.flows().find(|fl| &fl.name == n) {
                    None => {
                        let names: Vec<String> = other.flows().filter(|x| x.vis != Vis::Private).map(|x| x.name.clone()).collect();
                        self.diags.push(Diag::error(
                            &f.path,
                            imp.line,
                            imp.col,
                            "E-USE-UNKNOWN-FLOW",
                            format!("`{}` has no flow named `{n}`", imp.spec),
                            Some(format!("importable flows there: {}", if names.is_empty() { "(none)".into() } else { names.join(", ") })),
                        ));
                    }
                    Some(fl) if fl.vis == Vis::Private => {
                        self.diags.push(Diag::error(
                            &f.path,
                            imp.line,
                            imp.col,
                            "E-USE-PRIVATE-FLOW",
                            format!("flow `{n}` is `private` in its file and cannot be imported"),
                            Some("remove `private` in the component, or import a different flow".into()),
                        ));
                    }
                    _ => {}
                }
            }
        }

        // Sandbox block placement.
        if let Some(sb) = &f.sandbox {
            if self.fidx != 0 {
                self.err(
                    sb.span,
                    "E-SANDBOX-NOT-ENTRY",
                    "only the entry file may have a `sandbox` block".into(),
                    Some("remove it from this component; the entry file's sandbox applies to everything it imports".into()),
                );
            } else {
                self.sandbox_block(sb);
                self.sandbox_ins_in_flows(sb, f);
            }
        }

        // Declarations: duplicates and shadowing.
        let mut seen: HashMap<String, Span> = HashMap::new();
        let mut mains = 0;
        for d in &f.decls {
            let (name, span) = match d {
                Decl::Flow(x) => {
                    if x.vis == Vis::Main {
                        mains += 1;
                        if mains == 2 {
                            self.err(x.span, "E-MAIN-MULTIPLE", "a file has at most one `main` flow".into(), Some("keep `main` on the entry flow only".into()));
                        }
                    }
                    (x.name.clone(), x.span)
                }
                Decl::Filter(x) => (x.name.clone(), x.span),
                Decl::Fn(x) => (x.name.clone(), x.span),
                Decl::Test(_) => continue,
            };
            if seen.contains_key(&name) {
                self.err(span, "E-DUP-DECL", format!("`{name}` is declared twice"), Some("rename one of them".into()));
            }
            seen.insert(name.clone(), span);
            if builtins::is_builtin(&name) || NATIVE_STAGES.contains(&name.as_str()) {
                self.err(
                    span,
                    "E-SHADOWS-BUILTIN",
                    format!("`{name}` is a built-in and cannot be redeclared"),
                    Some(format!("rename it, for example `{name}_value`")),
                );
            }
        }

        for d in &f.decls {
            match d {
                Decl::Filter(x) => self.filter(x),
                Decl::Fn(x) => self.fn_decl(x),
                Decl::Flow(x) => self.flow(x),
                Decl::Test(x) => self.test(x),
            }
        }

        // Programs a used filter starts must be reachable in the sandbox.
        let used: Vec<String> = self.used_filters.iter().cloned().collect();
        for name in used {
            if let Some(fl) = self.filters.get(&name).copied() {
                self.check_program(&fl.program, fl.program_span);
            }
        }
    }

    /// Every sandbox `in` is also an `in` of each flow that can be run, so the flow's interface shows it.
    /// The flow receives the path as it appears inside the sandbox; the host path stays in the sandbox block.
    fn sandbox_ins_in_flows(&mut self, sb: &SandboxBlock, f: &File) {
        for d in &f.decls {
            let Decl::Flow(flow) = d else { continue };
            if flow.vis == Vis::Private {
                continue;
            }
            for si in &sb.ins {
                match flow.ins.iter().find(|i| i.name == si.name) {
                    None => self.err(
                        flow.span,
                        "E-SANDBOX-IN-NOT-IN-FLOW",
                        format!("the sandbox input `{}` is not declared in flow `{}`", si.name, flow.name),
                        Some(format!("add `in {}: {}` to the flow; it receives the path as it appears inside the sandbox", si.name, si.ty.show())),
                    ),
                    Some(fi) if fi.ty != si.ty => self.err(
                        fi.span,
                        "E-SANDBOX-IN-TYPE-MISMATCH",
                        format!("`{}` is `{}` in the sandbox block but `{}` in flow `{}`", si.name, si.ty.show(), fi.ty.show(), flow.name),
                        Some(format!("write `in {}: {}`", si.name, si.ty.show())),
                    ),
                    Some(fi) if fi.default.is_some() => self.err(
                        fi.span,
                        "E-SANDBOX-IN-DEFAULT",
                        format!("`{}` has its default in the sandbox block, not in flow `{}`", si.name, flow.name),
                        Some("remove the default here".into()),
                    ),
                    Some(_) => {}
                }
            }
        }
    }

    fn sandbox_block(&mut self, sb: &SandboxBlock) {
        self.push();
        for i in &sb.ins {
            self.in_decl_types(i, true);
            self.bind(&i.name, Some(i.ty.clone()), false);
            if i.ty != Ty::Path && i.ty != Ty::Opt(Box::new(Ty::Path)) {
                self.err(i.span, "E-SANDBOX-IN-TYPE", "a sandbox `in` value is a host path".into(), Some("declare it as `in name: path = \"/host/folder\"`".into()));
            }
        }
        for m in &sb.mounts {
            if let MountSpec::Path { host, at, .. } = &m.spec {
                self.expr(host, Want::default());
                if literal_str(host).is_some_and(|h| normalize(&h) == "/") {
                    self.err(
                        host.span,
                        "E-MOUNT-ROOT",
                        "a mount of the host's root folder would be the whole machine, and a sandbox that holds everything is no sandbox".into(),
                        Some("mount only the folders the script needs; a script that needs the whole machine runs with --sandbox off".into()),
                    );
                }
                if let Some(a) = at {
                    self.expr(a, Want::default());
                    if let Some(s) = literal_str(a) {
                        if !s.starts_with('/') {
                            self.err(a.span, "E-MOUNT-RELATIVE", format!("the inside path {s:?} must be absolute"), Some("write a path that starts with `/`".into()));
                        }
                    }
                } else if let Some(s) = literal_str(host) {
                    if !s.starts_with('/') {
                        self.err(host.span, "E-MOUNT-RELATIVE", format!("the mount path {s:?} must be absolute"), Some("write a path that starts with `/`".into()));
                    }
                }
            }
        }
        self.pop();
    }

    fn check_ty(&mut self, ty: &Ty, span: Span) {
        match ty {
            Ty::Named(n) => {
                let hint = if CATCHABLE.contains(&n.as_str()) {
                    "error types are only valid in `catch`".to_string()
                } else {
                    "types are: string, int, bool, path, duration, secret, list<T>, map<K,V>, enum(a, b), T? and {field: T}".to_string()
                };
                self.err(span, "E-UNKNOWN-TYPE", format!("unknown type `{n}`"), Some(hint));
            }
            Ty::List(t) | Ty::Opt(t) => self.check_ty(t, span),
            Ty::Map(k, v) => {
                self.check_ty(k, span);
                self.check_ty(v, span);
            }
            Ty::Record(f) => {
                for (_, t) in f {
                    self.check_ty(t, span);
                }
            }
            _ => {}
        }
    }

    fn in_decl_types(&mut self, i: &InDecl, host_path_input: bool) {
        self.check_ty(&i.ty, i.span);
        if let Some(d) = &i.default {
            if i.ty == Ty::Secret {
                self.err(i.span, "E-SECRET-DEFAULT", "a `secret` input cannot have a default value".into(), Some("remove the default; secrets arrive from the environment, a file or stdin".into()));
            }
            self.expr(d, Want::default());
            // A sandbox `in` is a host path written as a plain string, as in the design's own example.
            let host_string_ok = host_path_input && matches!(d.kind, ExprKind::Str(_));
            if let (Some((found, fix)), false) = (Self::literal_mismatch(d, &i.ty), host_string_ok) {
                self.err(
                    d.span,
                    "E-DEFAULT-TYPE",
                    format!("input `{}` is {}, but its default is a {found}", i.name, i.ty.show()),
                    Some(fix),
                );
            }
        }
    }

    // ---- filters and fns --------------------------------------------------

    fn filter(&mut self, f: &Filter) {
        self.push();
        let mut names = HashSet::new();
        for i in &f.ins {
            if !names.insert(i.name.clone()) {
                self.err(i.span, "E-DUP-DECL", format!("filter `{}` declares input `{}` twice", f.name, i.name), None);
            }
            self.in_decl_types(i, false);
            self.bind(&i.name, Some(i.ty.clone()), false);
        }
        for a in &f.args {
            self.expr(&a.expr, Want::default());
        }
        for (_, e) in &f.env {
            self.expr(e, Want { opt_ok: false, secret_ok: true });
        }
        if let Some(sees) = &f.see {
            self.check_view_items(&format!("filter `{}`", f.name), sees);
        }
        if f.takes == IoKind::None && f.yields == IoKind::None && f.args.is_empty() && f.ins.is_empty() {
            // nothing to say: a bare command is fine (e.g. `nginx -t` has args though)
        }
        self.pop();
        if DESTRUCTIVE.contains(&f.program.rsplit('/').next().unwrap_or("")) {
            self.warn(
                f.program_span,
                "W-DESTRUCTIVE-PROGRAM",
                format!("`{}` is a destructive tool and the built-in checks (mount-point refusal) do not apply to it", f.program),
                Some("use the built-in `delete`, `move`, `copy` or `write` instead".into()),
            );
        }
    }

    /// The entries of a filter's `see` lines or a `within` header, against the script's mounts.
    fn check_view_items(&mut self, label: &str, sees: &[(See, Span)]) {
        for (see, span) in sees {
            if let See::Path { path, mode } = see {
                self.expr(path, Want::default());
                if let Some(p) = literal_str(path) {
                    self.check_see(label, &p, *mode, *span);
                }
            } else if !self.mounts.present && self.fidx == 0 {
                // reported once below via W-NO-SANDBOX
            } else if self.mounts.present && self.mounts.all_known && !SYSTEM_SET.iter().all(|p| self.mounts.known.iter().any(|(i, _)| i == p)) {
                self.err(
                    *span,
                    "E-SEE-UNMOUNTED",
                    format!("{label} has `see system`, but the sandbox does not `mount system`"),
                    Some("add `mount system` to the sandbox block".into()),
                );
            }
        }
    }

    fn check_see(&mut self, filter: &str, path: &str, mode: Mode, span: Span) {
        if !self.mounts.present {
            return;
        }
        match self.mounts.find(path) {
            Some((m, Mode::Ro)) if mode == Mode::Rw => {
                self.err(
                    span,
                    "E-SEE-UNMOUNTED",
                    format!("{filter} needs {path} rw, but the sandbox mounts {m} ro"),
                    Some(format!("mount it rw in the sandbox block, or change the `see` to `{path} ro`")),
                );
            }
            Some(_) => {}
            None if self.mounts.all_known => {
                self.err(
                    span,
                    "E-SEE-UNMOUNTED",
                    format!("{filter} needs {path} {}, which is not mounted in the sandbox", mode.word()),
                    Some(format!("add `mount \"{path}\" {}` to the sandbox block", mode.word())),
                );
            }
            None => {}
        }
    }

    fn fn_decl(&mut self, f: &FnDecl) {
        self.push();
        self.in_fn = true;
        self.check_ty(&f.ret, f.span);
        for p in &f.params {
            self.check_ty(&p.ty, f.span);
            self.bind(&p.name, Some(p.ty.clone()), false);
        }
        self.block_noscope(&f.body);
        self.in_fn = false;
        self.pop();
    }

    // ---- flows ------------------------------------------------------------

    fn flow(&mut self, f: &Flow) {
        self.steps_seen.clear();
        self.sets_seen.clear();
        self.cur_outs = f.outs.iter().map(|o| o.name.clone()).collect();
        self.push();
        for i in &f.ins {
            self.in_decl_types(i, false);
            self.bind(&i.name, Some(i.ty.clone()), false);
        }
        let mut outs = HashSet::new();
        for o in &f.outs {
            self.check_ty(&o.ty, o.span);
            if !outs.insert(o.name.clone()) {
                self.err(o.span, "E-DUP-DECL", format!("output `{}` is declared twice", o.name), None);
            }
        }
        self.in_requires = true;
        for r in &f.requires {
            self.expr(r, Want::default());
        }
        self.in_requires = false;
        if let Some(l) = &f.lock {
            self.expr(l, Want::default());
            if let Some(p) = literal_str(l) {
                self.check_lock(&p, l.span);
            }
        }
        self.block_noscope(&f.body);
        if let Some(fin) = &f.finally {
            self.block(fin);
        }
        self.pop();
        for o in &f.outs {
            if !self.sets_seen.contains(&o.name) {
                self.warn(o.span, "W-OUT-NEVER-SET", format!("output `{}` is never assigned with `set`", o.name), Some(format!("add `set {} = ...` or remove the output", o.name)));
            }
        }
    }

    fn check_lock(&mut self, p: &str, span: Span) {
        if !self.mounts.present {
            return;
        }
        let found = self.mounts.find(p).map(|(m, mode)| (m.to_string(), mode));
        match found {
            Some((m, Mode::Rw)) if m != "/tmp" => {}
            None if !self.mounts.all_known => {}
            Some((m, mode)) => {
                let why = if m == "/tmp" && mode == Mode::Rw {
                    "it is under the private /tmp, so every run has its own lock file and nothing is excluded".to_string()
                } else {
                    format!("it is under the read-only mount {m}")
                };
                self.err(
                    span,
                    "E-SANDBOX-LOCK-UNSHARED",
                    format!("the lock path {p} cannot exclude other runs: {why}"),
                    Some("put the lock under a `rw` mount that is shared with the host, for example `mount \"/run/lock\" rw`".into()),
                );
            }
            None => {
                self.err(
                    span,
                    "E-SANDBOX-LOCK-UNSHARED",
                    format!("the lock path {p} cannot exclude other runs: it is not under any mount"),
                    Some("put the lock under a `rw` mount that is shared with the host, for example `mount \"/run/lock\" rw`".into()),
                );
            }
        }
    }

    // ---- statements -------------------------------------------------------

    fn block(&mut self, b: &Block) {
        self.push();
        self.block_noscope(b);
        self.pop();
    }

    fn block_noscope(&mut self, b: &Block) {
        for s in &b.stmts {
            self.stmt(s);
        }
    }

    fn declared_ty_check(&mut self, ty: &Option<Ty>, span: Span) {
        if let Some(t) = ty {
            self.check_ty(t, span);
        }
    }

    fn stmt(&mut self, s: &Stmt) {
        match &s.kind {
            StmtKind::Let { name, ty, expr } | StmtKind::Var { name, ty, expr } => {
                self.declared_ty_check(ty, s.span);
                self.expr(expr, Want { opt_ok: ty.as_ref().map(|t| t.is_optional()).unwrap_or(false), secret_ok: matches!(ty, Some(Ty::Secret)) });
                self.bind(name, ty.clone(), matches!(s.kind, StmtKind::Var { .. }));
            }
            StmtKind::Assign { name, expr } => {
                let b = self.lookup(name).cloned();
                self.expr(expr, Want::default());
                match b {
                    None => {
                        let cands = self.names_in_scope();
                        self.err(s.span, "E-UNKNOWN-NAME", format!("`{name}` is not defined"), suggest(name, &cands).map(|c| format!("did you mean `{c}`?")));
                    }
                    Some(b) if !b.mutable => {
                        self.err(
                            s.span,
                            "E-ASSIGN-IMMUTABLE",
                            format!("`{name}` is immutable and cannot be assigned"),
                            Some(format!("declare it with `var {name} = ...` where it is first defined")),
                        );
                    }
                    _ => {}
                }
            }
            StmtKind::Set { name, expr } => {
                self.sets_seen.insert(name.clone());
                self.expr(expr, Want { opt_ok: true, secret_ok: false });
                if self.in_fn {
                    self.err(s.span, "E-FN-IMPURE", "a `fn` has no outputs".into(), Some("return a value instead".into()));
                }
            }
            StmtKind::If { cond, then, else_ } => {
                self.expr(cond, Want::default());
                self.block(then);
                if let Some(e) = else_ {
                    self.block(e);
                }
            }
            StmtKind::IfLet { name, expr, then, else_ } => {
                // The source of an `if let` may be an absent-able value.
                self.expr(expr, Want { opt_ok: true, secret_ok: true });
                let ty = match &expr.kind {
                    ExprKind::Ident(n) => match self.lookup(n).and_then(|b| b.ty.clone()) {
                        Some(Ty::Opt(inner)) => Some(*inner),
                        Some(other) => {
                            self.warn(
                                expr.span,
                                "W-IF-LET-NOT-OPTIONAL",
                                format!("`{n}` is {}, which is never absent, so this `if let` always matches", other.show()),
                                Some("use the value directly".into()),
                            );
                            Some(other)
                        }
                        None => None,
                    },
                    _ => None,
                };
                self.push();
                self.bind(name, ty, false);
                self.block(then);
                self.pop();
                if let Some(e) = else_ {
                    self.block(e);
                }
            }
            StmtKind::For { var, iter, body } => {
                self.expr(iter, Want::default());
                self.push();
                self.bind(var, None, false);
                self.loop_depth += 1;
                self.block_noscope(body);
                self.loop_depth -= 1;
                self.pop();
            }
            StmtKind::Try { body, catches } => {
                self.block(body);
                for c in catches {
                    if !CATCHABLE.contains(&c.err_ty.as_str()) {
                        self.err(
                            c.span,
                            "E-CATCH-UNKNOWN-TYPE",
                            format!("`{}` is not an error type", c.err_ty),
                            Some(match suggest(&c.err_ty, &CATCHABLE.iter().map(|s| s.to_string()).collect::<Vec<_>>()) {
                                Some(s) => format!("did you mean `{s}`?"),
                                None => format!("the error types are: {}", CATCHABLE.join(", ")),
                            }),
                        );
                    }
                    self.push();
                    self.bind(&c.var, None, false);
                    self.block_noscope(&c.body);
                    self.pop();
                }
            }
            StmtKind::Return(e) => {
                if !self.in_fn && self.within_depth == 0 {
                    self.err(
                        s.span,
                        "E-RETURN-OUTSIDE-FN",
                        "`return` is only allowed inside a `fn` or a `within` block".into(),
                        Some("a flow reports results with `set <output> = ...`".into()),
                    );
                }
                self.expr(e, Want::default());
            }
            StmtKind::Fail(e) => {
                self.expr(e, Want::default());
            }
            StmtKind::Expect(e) => {
                if !self.in_test {
                    self.err(s.span, "E-EXPECT-OUTSIDE-TEST", "`expect` is only allowed inside a `test`".into(), None);
                }
                self.expr(e, Want::default());
            }
            StmtKind::Step(step) => {
                if self.in_fn {
                    self.err(s.span, "E-FN-IMPURE", "a `fn` cannot contain steps".into(), None);
                }
                self.step_identity(&step.name, step.as_label.is_some(), s.span);
                if let Some(l) = &step.as_label {
                    self.expr(l, Want::default());
                }
                if let Some(w) = &step.when {
                    self.expr(w, Want::default());
                }
                self.block(&step.body);
            }
            StmtKind::Expr { expr, as_label, when, .. } => {
                if let Some(w) = when {
                    self.expr(w, Want::default());
                }
                if let Some(l) = as_label {
                    self.expr(l, Want::default());
                }
                if matches!(expr.kind, ExprKind::Within { .. }) {
                    self.within(expr, false);
                } else {
                    self.expr(expr, Want::default());
                }
                let flow_call = match &expr.kind {
                    ExprKind::Call { name, .. } | ExprKind::RunFlow { name, .. } => self.flows.contains_key(name.as_str()) && !self.fns.contains_key(name.as_str()),
                    _ => false,
                };
                if flow_call && !self.in_test {
                    if let ExprKind::Call { name, .. } | ExprKind::RunFlow { name, .. } = &expr.kind {
                        let name = name.clone();
                        self.step_identity(&name, as_label.is_some(), expr.span);
                    }
                }
            }
        }
    }

    fn step_identity(&mut self, name: &str, labelled: bool, span: Span) {
        if self.loop_depth > 0 && !labelled {
            self.err(
                span,
                "E-STEP-LABEL",
                format!("step `{name}` repeats inside a loop and needs an `as` label"),
                Some(format!("write `{name} as <label> {{ ... }}` so each iteration has a stable identity")),
            );
        }
        if !labelled && !self.steps_seen.insert(name.to_string()) {
            self.err(
                span,
                "E-DUP-STEP",
                format!("step `{name}` appears more than once in this flow"),
                Some("step names are unique within a flow; give each a label with `as`".into()),
            );
        }
    }

    // ---- expressions ------------------------------------------------------

    fn expr(&mut self, e: &Expr, want: Want) {
        match &e.kind {
            ExprKind::Int(_) | ExprKind::Bool(_) | ExprKind::Absent | ExprKind::Dur(..) => {}
            ExprKind::Str(segs) => {
                for s in segs {
                    if let StrSeg::Expr(x) = s {
                        self.interp_expr(x);
                    }
                }
            }
            ExprKind::Ident(n) => self.ident(n, e.span, want),
            ExprKind::List(items) => {
                for a in items {
                    self.expr(&a.expr, Want::default());
                }
            }
            ExprKind::Record(fields) => {
                for (_, x) in fields {
                    self.expr(x, Want::default());
                }
            }
            ExprKind::Call { name, args } => {
                if let Some(f) = self.filters.get(name.as_str()).copied() {
                    if f.takes != IoKind::None && !self.fns.contains_key(name.as_str()) {
                        self.err(
                            e.span,
                            "E-FILTER-NEEDS-INPUT",
                            format!("filter `{name}` takes {} on stdin, so it cannot start a pipeline", f.takes.word()),
                            Some(format!("chain it after a stage that supplies the input: `source.{name}(...)`")),
                        );
                    }
                }
                self.call(name, args, e.span, None)
            }
            ExprKind::RunFlow { name, args } => {
                if !self.in_test {
                    self.err(e.span, "E-RUN-OUTSIDE-TEST", "`run <flow>(...)` is only allowed inside a `test`".into(), Some("call the flow like a function: `name(arg: value)`".into()));
                }
                self.call(name, args, e.span, None);
            }
            ExprKind::Stage { recv, name, args } => {
                self.expr(recv, Want::default());
                self.stage(recv, name, args, e.span);
            }
            ExprKind::Member { recv, .. } => self.expr(recv, Want::default()),
            ExprKind::Unary(_, x) => self.expr(x, Want::default()),
            ExprKind::Binary(op, l, r) => {
                // `x == absent` and `x != absent` are how an optional is tested directly.
                let against_absent = matches!(op, BinOp::Eq | BinOp::Ne)
                    && (matches!(l.kind, ExprKind::Absent) || matches!(r.kind, ExprKind::Absent));
                let w = Want { opt_ok: against_absent, secret_ok: false };
                self.expr(l, w);
                self.expr(r, w);
            }
            ExprKind::Try(x) => self.expr(x, Want::default()),
            ExprKind::If { cond, then, else_ } => {
                self.expr(cond, Want::default());
                self.block(then);
                self.block(else_);
            }
            ExprKind::Within { .. } => self.within(e, true),
        }
    }

    /// A `within` block. Used as a value (`let x = within ...`) it must return on every path.
    fn within(&mut self, e: &Expr, as_value: bool) {
        let ExprKind::Within { items, body } = &e.kind else { return };
        if self.in_fn {
            self.err(e.span, "E-FN-IMPURE", "a `fn` cannot contain a `within` block".into(), Some("a `fn` is pure; move this into a flow".into()));
        }
        self.check_view_items("a `within` block", items);
        self.within_depth += 1;
        self.block(body);
        self.within_depth -= 1;
        if as_value && !block_returns(body) {
            self.err(
                e.span,
                "E-WITHIN-NO-RETURN",
                "a `within` block used as a value must `return` on every path".into(),
                Some("end the block with `return <value>`, or use the block as a statement".into()),
            );
        }
    }

    fn interp_expr(&mut self, e: &Expr) {
        self.expr(e, Want::default());
    }

    fn ident(&mut self, n: &str, span: Span, want: Want) {
        if n == "it" {
            if self.stage_depth == 0 {
                self.err(
                    span,
                    "E-IT-OUTSIDE-STAGE",
                    "`it` only exists inside the expression of a pipeline stage".into(),
                    Some("use it in `map(to: ...)`, `filter(where: ...)`, `count_by(key: ...)`, `sort_by(key: ...)` or `unique(by: ...)`".into()),
                );
            }
            return;
        }
        if n == "asc" || n == "desc" {
            return;
        }
        match self.lookup(n).cloned() {
            None if self.quiet_names > 0 => {}
            None => {
                let mut cands = self.names_in_scope();
                cands.extend(self.flows.keys().cloned());
                let hint = if self.cur_outs.contains(n) {
                    Some(format!("`{n}` is an output: it can be assigned with `set {n} = ...` but not read; keep the value in a `var` and `set` it at the end"))
                } else if self.flows.contains_key(n) || self.filters.contains_key(n) || self.fns.contains_key(n) || builtins::is_builtin(n) {
                    Some(format!("`{n}` is a callable, not a value; call it with `{n}(...)`"))
                } else {
                    suggest(n, &self.names_in_scope()).map(|c| format!("did you mean `{c}`?")).or_else(|| {
                        let v = self.names_in_scope();
                        Some(if v.is_empty() { "nothing is in scope here".into() } else { format!("in scope: {}", v.join(", ")) })
                    })
                };
                self.err(span, "E-UNKNOWN-NAME", format!("`{n}` is not defined"), hint);
            }
            Some(b) => {
                match &b.ty {
                    Some(Ty::Opt(_)) if !want.opt_ok => {
                        self.err(
                            span,
                            "E-OPTIONAL-UNWRAPPED",
                            format!("`{n}` may be absent and must be unwrapped before use"),
                            Some(format!("write `if let {n}_value = {n} {{ ... }}` and use the unwrapped value inside")),
                        );
                    }
                    Some(Ty::Secret) if !want.secret_ok => {
                        self.err(
                            span,
                            "E-SECRET-LEAK",
                            format!("`{n}` is a secret and cannot be used here"),
                            Some("a secret may reach a program only through a filter's `env { NAME = ... }` or its stdin".into()),
                        );
                    }
                    _ => {}
                }
            }
        }
    }

    fn params_of_callee(&mut self, name: &str, arg_names: &[&str]) -> Option<(&'static str, Vec<ParamInfo>, Option<Effect>)> {
        if let Some(f) = self.fns.get(name).copied() {
            return Some((
                "fn",
                f.params.iter().map(|p| ParamInfo { name: p.name.clone(), required: true, ty: Some(p.ty.clone()), default: None }).collect(),
                None,
            ));
        }
        if let Some(f) = self.flows.get(name).copied() {
            return Some(("flow", in_params(&f.ins), None));
        }
        if let Some(f) = self.filters.get(name).copied() {
            self.used_filters.insert(name.to_string());
            return Some(("filter", in_params(&f.ins), None));
        }
        if let Some(sig) = builtins::find(name, arg_names) {
            return Some((
                "built-in",
                sig.params.iter().map(|(p, r)| ParamInfo { name: p.to_string(), required: *r, ty: None, default: None }).collect(),
                Some(sig.effect),
            ));
        }
        None
    }

    /// Check a call. `skip_first` is the receiver parameter already supplied by a stage.
    fn call(&mut self, name: &str, args: &[Arg], span: Span, recv_param: Option<&str>) {
        let arg_names: Vec<&str> = args.iter().filter_map(|a| a.name.as_deref()).collect();
        let Some((kind, params, effect)) = self.params_of_callee(name, &arg_names) else {
            if self.unresolved.contains(name) || self.unresolved_all {
                self.quiet_names += 1;
                for a in args {
                    self.expr(&a.expr, Want::default());
                }
                self.quiet_names -= 1;
                return;
            }
            let mut cands: Vec<String> = self.fns.keys().chain(self.flows.keys()).chain(self.filters.keys()).cloned().collect();
            cands.extend(builtins::names().iter().map(|s| s.to_string()));
            cands.sort();
            cands.dedup();
            let hint = match suggest(name, &cands) {
                Some(c) => format!("did you mean `{c}`?"),
                None => format!("in scope: {}", cands.join(", ")),
            };
            self.err(span, "E-UNKNOWN-CALL", format!("there is no flow, filter, fn or built-in named `{name}`"), Some(hint));
            for a in args {
                self.expr(&a.expr, Want::default());
            }
            return;
        };

        // Purity.
        if self.in_fn {
            let impure = match kind {
                "flow" | "filter" => true,
                "built-in" => effect != Some(Effect::Pure),
                _ => false,
            };
            if impure {
                self.err(
                    span,
                    "E-FN-IMPURE",
                    format!("a `fn` cannot call {kind} `{name}`: it is not pure"),
                    Some("move this into a flow, and pass what the `fn` needs as arguments".into()),
                );
            }
        }
        if self.in_requires && kind == "built-in" && !matches!(effect, Some(Effect::Pure) | Some(Effect::Read)) {
            self.err(span, "E-REQUIRES-IMPURE", format!("a `requires` condition cannot call `{name}`"), Some("preconditions are pure or read-only, such as `exists(path: ...)`".into()));
        }
        if self.in_requires && (kind == "flow" || kind == "filter") {
            self.err(span, "E-REQUIRES-IMPURE", format!("a `requires` condition cannot call {kind} `{name}`"), None);
        }

        // Starting programs.
        if kind == "filter" {
            self.note_exec(span);
        }
        if name == "run" && kind == "built-in" {
            self.note_exec(span);
            self.check_run(args);
        }

        // Arguments.
        let mut seen: HashSet<&str> = HashSet::new();
        for a in args {
            match &a.name {
                None => {
                    self.err(a.span, "E-SPREAD-ARG", "a `...` spread is only allowed inside a list literal".into(), Some("write `args: [...list]`".into()));
                    self.expr(&a.expr, Want::default());
                }
                Some(n) => {
                    if !seen.insert(n.as_str()) {
                        self.err(a.span, "E-DUP-ARG", format!("argument `{n}` is given twice"), Some(format!("remove one `{n}:`")));
                    }
                    match params.iter().find(|p| &p.name == n) {
                        None => {
                            if recv_param == Some(n.as_str()) {
                                self.err(a.span, "E-DUP-ARG", format!("`{n}` is already supplied by the value this stage is applied to"), Some(format!("remove `{n}:`")));
                            } else {
                                let valid: Vec<String> = params.iter().map(|p| p.name.clone()).filter(|p| Some(p.as_str()) != recv_param).collect();
                                self.err(
                                    a.span,
                                    "E-UNKNOWN-ARG",
                                    format!("{kind} `{name}` has no argument `{n}`"),
                                    Some(match suggest(n, &valid) {
                                        Some(s) => format!("did you mean `{s}`?"),
                                        None if valid.is_empty() => format!("`{name}` takes no arguments"),
                                        None => format!("its arguments are: {}", valid.join(", ")),
                                    }),
                                );
                            }
                            self.expr(&a.expr, Want::default());
                        }
                        Some(p) => {
                            let w = Want {
                                opt_ok: p.ty.as_ref().map(|t| t.is_optional()).unwrap_or(false),
                                secret_ok: matches!(p.ty, Some(Ty::Secret)) && kind != "built-in",
                            };
                            self.expr(&a.expr, w);
                            if let Some(t) = &p.ty {
                                self.literal_arg_type(a, t, name);
                            }
                            if let (Some(d), true) = (&p.default, kind != "built-in") {
                                if let (Some(x), Some(y)) = (const_key(&a.expr), const_key(d)) {
                                    if x == y {
                                        self.warn(
                                            a.span,
                                            "W-DEFAULT-ARG",
                                            format!("argument `{n}` is the same as the default of `{name}`"),
                                            Some(format!("remove `{n}:`; add `// weir:allow W-DEFAULT-ARG` on the line above if the explicit value is intended")),
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        for p in &params {
            if p.required && !seen.contains(p.name.as_str()) && recv_param != Some(p.name.as_str()) {
                self.err(
                    span,
                    "E-MISSING-ARG",
                    format!("{kind} `{name}` is missing the argument `{}`", p.name),
                    Some(format!("add `{}: <value>`", p.name)),
                );
            }
        }
    }

    /// `Some((kind found, fix))` when a literal can never be a value of `ty`.
    fn literal_mismatch(e: &Expr, ty: &Ty) -> Option<(&'static str, String)> {
        let found = match &e.kind {
            ExprKind::Str(_) => "string",
            ExprKind::Int(_) => "int",
            ExprKind::Bool(_) => "bool",
            ExprKind::Dur(..) => "duration",
            _ => return None,
        };
        match ty {
            Ty::Opt(inner) => Self::literal_mismatch(e, inner),
            Ty::Named(_) | Ty::Record(_) | Ty::Map(..) => None,
            Ty::Str if found == "string" => None,
            Ty::Int if found == "int" => None,
            Ty::Bool if found == "bool" => None,
            Ty::Duration if found == "duration" => None,
            Ty::Enum(vals) if found == "string" => match literal_str(e) {
                Some(s) if !vals.contains(&s) => Some((found, format!("{s:?} is not allowed; the values are: {}", vals.join(", ")))),
                _ => None,
            },
            Ty::Path if found == "string" => Some((found, "a path is never implicitly a string: write `path(text: \"...\")`".into())),
            t => Some((found, format!("write a {} value", t.show()))),
        }
    }

    fn literal_arg_type(&mut self, a: &Arg, ty: &Ty, callee: &str) {
        if let Some((found, fix)) = Self::literal_mismatch(&a.expr, ty) {
            self.err(
                a.span,
                "E-ARG-TYPE",
                format!("argument `{}` of `{callee}` is {}, but a {found} was given", a.name.clone().unwrap_or_default(), ty.show()),
                Some(fix),
            );
        }
    }

    fn note_exec(&mut self, span: Span) {
        if !self.mounts.present && !self.warned_no_sandbox {
            self.warned_no_sandbox = true;
            self.warn(
                span,
                "W-NO-SANDBOX",
                "this script starts external programs but the entry file declares no `sandbox`".into(),
                Some("add a `sandbox { mount system ... }` block at the top of the entry file".into()),
            );
        }
    }

    fn check_run(&mut self, args: &[Arg]) {
        let cmd = args.iter().find(|a| a.name.as_deref() == Some("cmd"));
        match cmd {
            Some(c) => match literal_str(&c.expr) {
                None => self.err(
                    c.span,
                    "E-DYNAMIC-CMD",
                    "`run` needs a literal `cmd`".into(),
                    Some("the checker infers which executables a flow may start, so the name cannot be computed; declare a `filter` with a fixed `program`".into()),
                ),
                Some(s) => {
                    let base = s.rsplit('/').next().unwrap_or("").to_string();
                    if SHELLS.contains(&base.as_str()) {
                        self.warn(
                            c.span,
                            "W-SHELL-STRING",
                            format!("`run` starts the shell `{base}`; a string passed to it is re-parsed, which is what Weir exists to avoid"),
                            Some("wrap the real program in a `filter` and pass arguments as a list".into()),
                        );
                    } else if DESTRUCTIVE.contains(&base.as_str()) {
                        self.warn(
                            c.span,
                            "W-DESTRUCTIVE-PROGRAM",
                            format!("`{base}` is destructive and the built-in checks (mount-point refusal) do not apply to it"),
                            Some("use the built-in `delete`, `move`, `copy` or `write` instead".into()),
                        );
                    }
                    self.check_program(&s, c.span);
                }
            },
            None => {}
        }
    }

    fn check_program(&mut self, program: &str, span: Span) {
        if !self.mounts.present || !self.mounts.all_known {
            return;
        }
        let Some(found) = resolve_program(program) else {
            self.warn(
                span,
                "W-PROGRAM-NOT-FOUND",
                format!("`{program}` was not found on this machine's PATH, so it cannot be checked against the sandbox"),
                Some("install it, or ignore this on a machine that is not the target".into()),
            );
            return;
        };
        let mut paths = vec![found.clone()];
        if let Ok(c) = std::fs::canonicalize(&found) {
            let c = c.to_string_lossy().into_owned();
            if c != found {
                paths.push(c);
            }
        }
        for p in paths {
            if self.mounts.find(&p).is_none() {
                self.err(
                    span,
                    "E-SANDBOX-EXEC-UNREACHABLE",
                    format!("`{program}` resolves to {p}, which is not under any mount in the sandbox"),
                    Some(format!("add `mount system`, or `mount \"{}\" ro`", p.rsplit_once('/').map(|(d, _)| if d.is_empty() { "/" } else { d }).unwrap_or("/"))),
                );
                break;
            }
        }
    }

    fn stage(&mut self, recv: &Expr, name: &str, args: &[Arg], span: Span) {
        if NATIVE_STAGES.contains(&name) {
            let allowed: &[&str] = match name {
                "map" => &["to"],
                "filter" => &["where"],
                "take" => &["n"],
                "unique" => &["by"],
                "count_by" => &["key"],
                "sort_by" => &["key", "order"],
                _ => &[],
            };
            let required: &[&str] = match name {
                "map" => &["to"],
                "filter" => &["where"],
                "take" => &["n"],
                "count_by" => &["key"],
                "sort_by" => &["key"],
                _ => &[],
            };
            let mut seen = HashSet::new();
            for a in args {
                let n = a.name.clone().unwrap_or_default();
                if !allowed.contains(&n.as_str()) {
                    self.err(
                        a.span,
                        "E-UNKNOWN-ARG",
                        format!("stage `{name}` has no argument `{n}`"),
                        Some(if allowed.is_empty() { format!("`{name}()` takes no arguments") } else { format!("its arguments are: {}", allowed.join(", ")) }),
                    );
                }
                if !seen.insert(n.clone()) {
                    self.err(a.span, "E-DUP-ARG", format!("argument `{n}` is given twice"), None);
                }
                let per_item = matches!(n.as_str(), "to" | "where" | "key" | "by");
                if per_item {
                    self.stage_depth += 1;
                    self.expr(&a.expr, Want::default());
                    self.stage_depth -= 1;
                } else {
                    self.expr(&a.expr, Want::default());
                }
            }
            for r in required {
                if !seen.contains(&r.to_string()) {
                    self.err(span, "E-MISSING-ARG", format!("stage `{name}` is missing the argument `{r}`"), Some(format!("add `{r}: <value>`")));
                }
            }
            return;
        }
        if let Some(f) = self.filters.get(name).copied() {
            if f.takes == IoKind::None {
                self.err(
                    span,
                    "E-FILTER-TAKES-NONE",
                    format!("filter `{name}` takes no input, so nothing can be chained into it"),
                    Some(format!("call it at the start of a pipeline: `{name}(...)`, or declare `takes lines` or `takes text` on the filter")),
                );
            }
            self.call(name, args, span, None);
            return;
        }
        if let Some(f) = self.fns.get(name) {
            let _ = f;
            self.err(
                span,
                "E-UNKNOWN-STAGE",
                format!("`{name}` is a `fn`, not a stage"),
                Some(format!("call it as `{name}(...)`, for example inside `map(to: {name}(... : it))`")),
            );
            return;
        }
        if let Some(sig) = builtins::find(name, &args.iter().filter_map(|a| a.name.as_deref()).collect::<Vec<_>>()) {
            let recv_param = sig.params.first().map(|(p, _)| *p);
            self.call(name, args, span, recv_param);
            let _ = recv;
            return;
        }
        if self.unresolved.contains(name) || self.unresolved_all {
            self.quiet_names += 1;
            for a in args {
                self.expr(&a.expr, Want::default());
            }
            self.quiet_names -= 1;
            return;
        }
        let mut cands: Vec<String> = NATIVE_STAGES.iter().map(|s| s.to_string()).collect();
        cands.extend(self.filters.keys().cloned());
        cands.sort();
        self.err(
            span,
            "E-UNKNOWN-STAGE",
            format!("there is no stage `{name}`"),
            Some(match suggest(name, &cands) {
                Some(s) => format!("did you mean `{s}`? stages in scope: {}", cands.join(", ")),
                None => format!("stages in scope: {}", cands.join(", ")),
            }),
        );
        for a in args {
            self.expr(&a.expr, Want::default());
        }
    }

    // ---- tests ------------------------------------------------------------

    fn test(&mut self, t: &Test) {
        self.in_test = true;
        self.push();
        self.steps_seen.clear();
        for item in &t.items {
            match item {
                TestItem::Fixture { dir, items, span } => {
                    if !dir.starts_with('/') {
                        self.err(*span, "E-FIXTURE-PATH", format!("fixture directory {dir:?} must be an absolute inside path"), None);
                    }
                    for FixtureItem::File { text, .. } in items {
                        self.expr(text, Want::default());
                    }
                }
                TestItem::Stub { filter, yields, span } => {
                    if !self.filters.contains_key(filter.as_str()) {
                        let cands: Vec<String> = self.filters.keys().cloned().collect();
                        self.err(
                            *span,
                            "E-STUB-UNKNOWN-FILTER",
                            format!("there is no filter named `{filter}` to stub"),
                            Some(match suggest(filter, &cands) {
                                Some(s) => format!("did you mean `{s}`?"),
                                None => format!("filters: {}", cands.join(", ")),
                            }),
                        );
                    }
                    self.expr(yields, Want::default());
                }
                TestItem::Stmt(s) => self.stmt(s),
            }
        }
        self.pop();
        self.in_test = false;
    }
}

fn in_params(ins: &[InDecl]) -> Vec<ParamInfo> {
    ins.iter()
        .map(|i| ParamInfo {
            name: i.name.clone(),
            required: i.default.is_none() && !i.ty.is_optional(),
            ty: Some(i.ty.clone()),
            default: i.default.clone(),
        })
        .collect()
}

/// A canonical key for constant expressions, so a call argument can be compared with a default.
fn const_key(e: &Expr) -> Option<String> {
    match &e.kind {
        ExprKind::Int(n) => Some(format!("i:{n}")),
        ExprKind::Bool(b) => Some(format!("b:{b}")),
        ExprKind::Dur(n, _) => Some(format!("d:{n}")),
        ExprKind::Str(_) => literal_str(e).map(|s| format!("s:{s}")),
        ExprKind::Call { name, .. } if name == "path" => literal_str(e).map(|s| format!("p:{s}")),
        _ => None,
    }
}

fn resolve_program(program: &str) -> Option<String> {
    use std::os::unix::fs::PermissionsExt;
    let is_exec = |p: &std::path::Path| p.is_file() && std::fs::metadata(p).map(|m| m.permissions().mode() & 0o111 != 0).unwrap_or(false);
    if program.contains('/') {
        let p = std::path::Path::new(program);
        return if is_exec(p) { Some(normalize(program)) } else { None };
    }
    let path = std::env::var("PATH").unwrap_or_else(|_| "/usr/local/bin:/usr/bin:/bin".into());
    for dir in path.split(':') {
        let dir = if dir.is_empty() { "." } else { dir };
        let cand = std::path::Path::new(dir).join(program);
        if is_exec(&cand) {
            return Some(cand.to_string_lossy().into_owned());
        }
    }
    None
}
