//! Tree-walking interpreter.

use crate::ast::*;
use crate::builtins::{self, Args};
use crate::error::*;
use crate::mounts::{normalize, Mounts, SYSTEM_SET};
use crate::pretty::show_expr;
use crate::value::Value;
use std::collections::HashMap;
use std::io::Write as _;
use std::process::{Command, Stdio};
use std::rc::Rc;

#[derive(Debug, Clone, Default)]
pub struct Options {
    pub confirm: bool,
    pub plan: bool,
    /// `--json` or `--out`: `print` goes to stderr.
    pub print_to_stderr: bool,
    pub deny_exec: Vec<String>,
}

/// One file's declarations, as the flows in it see them.
#[derive(Default)]
pub struct Unit {
    pub flows: HashMap<String, Rc<Flow>>,
    pub tools: HashMap<String, Rc<Tool>>,
    pub fns: HashMap<String, Rc<FnDecl>>,
    /// Flows imported with `use`: name -> (unit that defines it, flow).
    pub imports: HashMap<String, (usize, Rc<Flow>)>,
}

impl Unit {
    pub fn from_file(f: &File) -> Unit {
        let mut u = Unit::default();
        for d in &f.decls {
            match d {
                Decl::Flow(x) => {
                    u.flows.insert(x.name.clone(), Rc::new(x.clone()));
                }
                Decl::Tool(x) => {
                    u.tools.insert(x.name.clone(), Rc::new(x.clone()));
                }
                Decl::Fn(x) => {
                    u.fns.insert(x.name.clone(), Rc::new(x.clone()));
                }
                Decl::Test(_) => {}
            }
        }
        u
    }
}

#[derive(Debug, Clone)]
pub struct StepRec {
    pub name: String,
    /// "ok", "failed" or "skipped"
    pub status: &'static str,
    pub detail: Option<String>,
}

struct Slot {
    val: Value,
    mutable: bool,
}

pub struct Frame {
    unit: usize,
    scopes: Vec<HashMap<String, Slot>>,
    outs: Vec<(String, Ty, Value)>,
    loops: Vec<usize>,
}

impl Frame {
    fn new(unit: usize) -> Frame {
        Frame { unit, scopes: vec![HashMap::new()], outs: vec![], loops: vec![] }
    }

    fn bind(&mut self, name: &str, val: Value, mutable: bool) {
        self.scopes.last_mut().unwrap().insert(name.to_string(), Slot { val, mutable });
    }

    fn lookup(&self, name: &str) -> Option<&Value> {
        self.scopes.iter().rev().find_map(|s| s.get(name)).map(|s| &s.val)
    }

    fn slot_mut(&mut self, name: &str) -> Option<&mut Slot> {
        self.scopes.iter_mut().rev().find_map(|s| s.get_mut(name))
    }
}

/// The part of the sandbox a `within` block leaves visible.
#[derive(Clone, Debug)]
pub struct View {
    pub system: bool,
    pub paths: Vec<(String, Mode)>,
    pub line: u32,
}

fn is_under(path: &str, base: &str) -> bool {
    base == "/" || path == base || path.strip_prefix(base).is_some_and(|r| r.starts_with('/'))
}

fn narrower(a: Mode, b: Mode) -> Mode {
    if a == Mode::Ro || b == Mode::Ro {
        Mode::Ro
    } else {
        Mode::Rw
    }
}

impl View {
    /// Can `inside` be used (written, if `write`) from within this view?
    fn allows(&self, inside: &str, write: bool) -> bool {
        let p = normalize(inside);
        if self.system && !write && SYSTEM_SET.iter().any(|s| is_under(&p, s)) {
            return true;
        }
        self.paths.iter().any(|(base, mode)| is_under(&p, base) && (*mode == Mode::Rw || !write))
    }

    /// Does this view name the explicit mount at `mount` (or a path inside it)? Naming a parent folder does not.
    fn requests(&self, mount: &str) -> bool {
        self.paths.iter().any(|(base, _)| is_under(base, mount))
    }

    /// What a program started inside this view sees: this view, cut down to a tool's own `see` lines.
    /// An explicit mount is in the result only if both the view and the tool name it.
    fn narrow_to(&self, tool: Option<(Vec<(String, Mode)>, bool)>, explicit: &[String]) -> (Vec<(String, Mode)>, bool) {
        let Some((fpaths, fsystem)) = tool else {
            return (self.paths.clone(), self.system);
        };
        let both_name = |path: &str, fp: &str, vp: &str| {
            explicit.iter().all(|r| !is_under(path, r) || (is_under(fp, r) && is_under(vp, r)))
        };
        let mut out: Vec<(String, Mode)> = Vec::new();
        for (fp, fm) in &fpaths {
            for (vp, vm) in &self.paths {
                if is_under(vp, fp) && both_name(vp, fp, vp) {
                    out.push((vp.clone(), narrower(*fm, *vm)));
                } else if is_under(fp, vp) && both_name(fp, fp, vp) {
                    out.push((fp.clone(), narrower(*fm, *vm)));
                }
            }
        }
        out.sort();
        out.dedup();
        (out, fsystem && self.system)
    }
}

/// One `stub` line of a test: the answer for a tool, optionally only for calls with these arguments.
pub struct StubEntry {
    pub tool: String,
    pub when: Vec<(String, Value)>,
    pub value: Value,
    pub used: bool,
}

pub struct Interp {
    pub opts: Options,
    pub mounts: Mounts,
    pub units: Vec<Unit>,
    pub steps: Vec<StepRec>,
    path: Vec<String>,
    pub stubs: Vec<StubEntry>,
    pub test_mode: bool,
    /// The `within` blocks being run, innermost last.
    views: Vec<View>,
    /// Running inside the real sandbox, so `see` clauses can be enforced per program.
    pub sandboxed: bool,
    /// Per-program views (`see`, `within`) can be built even without the script's sandbox. False after an `auto` fallback.
    pub views_ok: bool,
    depth: usize,
    /// Text `print` wrote while in test mode.
    pub captured: Vec<String>,
}

fn attempt_record(ok: bool, code: i64, value: Value, message: &str) -> Value {
    Value::record(vec![
        ("ok", Value::Bool(ok)),
        ("code", Value::Int(code)),
        ("value", value),
        ("message", Value::str(message)),
    ])
}

fn truthy(v: &Value, what: &str) -> R<bool> {
    match v {
        Value::Bool(b) => Ok(*b),
        v => Err(type_error(format!("{what} must be a bool, found {}", v.type_name()))),
    }
}

/// Does `v` fit the declared type `ty`?
pub fn conforms(v: &Value, ty: &Ty) -> Result<(), String> {
    let bad = |want: &str| Err(format!("expected {want}, found {}", v.type_name()));
    match (ty, v) {
        (Ty::Opt(_), Value::Absent) => Ok(()),
        (Ty::Opt(t), v) => conforms(v, t),
        (_, Value::Absent) => Err(format!("expected {}, found absent", ty.show())),
        (Ty::Str, Value::Str(_)) | (Ty::Int, Value::Int(_)) | (Ty::Bool, Value::Bool(_)) => Ok(()),
        (Ty::Path, Value::Path(_)) | (Ty::Duration, Value::Dur(_)) | (Ty::Secret, Value::Secret(_)) => Ok(()),
        (Ty::Str, Value::Path(_)) => Err("expected string, found path; a path is never implicitly a string".into()),
        (Ty::Path, Value::Str(_)) => Err("expected path, found string; convert with `path(text: s)`".into()),
        (Ty::Enum(vals), Value::Str(s)) => {
            if vals.iter().any(|x| x == &**s) {
                Ok(())
            } else {
                Err(format!("expected one of {}, found {:?}", vals.join(", "), s))
            }
        }
        (Ty::List(t), Value::List(l)) => {
            for (i, x) in l.iter().enumerate() {
                conforms(x, t).map_err(|e| format!("element {i}: {e}"))?;
            }
            Ok(())
        }
        (Ty::Record(fields), Value::Record(r)) => {
            for (n, t) in fields {
                match r.iter().find(|(k, _)| k == n) {
                    Some((_, x)) => conforms(x, t).map_err(|e| format!("field `{n}`: {e}"))?,
                    None => return Err(format!("missing field `{n}`")),
                }
            }
            Ok(())
        }
        (Ty::Map(_, _), Value::Record(_)) => Ok(()),
        (Ty::Named(n), _) => Err(format!("unknown type `{n}`")),
        (t, _) => bad(&t.show()),
    }
}

impl Interp {
    pub fn new(opts: Options, mounts: Mounts, units: Vec<Unit>) -> Interp {
        Interp {
            opts,
            mounts,
            units,
            steps: vec![],
            path: vec![],
            stubs: Vec::new(),
            test_mode: false,
            views: vec![],
            sandboxed: false,
            views_ok: true,
            depth: 0,
            captured: vec![],
        }
    }

    // ---- paths and the mount table ---------------------------------------

    fn host_path(&self, inside: &str, write: bool, op: &str) -> R<String> {
        if !inside.starts_with('/') {
            return Err(throw(
                "IoError",
                format!("cannot {op} {inside:?}: paths inside the sandbox are absolute (a relative path has no working directory to resolve against)"),
            ));
        }
        match self.mounts.find(inside) {
            None => Err(throw(
                "IoError",
                format!("cannot {op} {inside}: it is outside every mount, so it does not exist in the sandbox\nmounts:\n{}", self.mounts.describe().trim_end()),
            )),
            Some((m, host)) => {
                if self.explicit_blocks(m) {
                    return Err(throw(
                        "CapabilityError",
                        format!(
                            "cannot {op} {inside}: it is on the explicit mount {}, which only a tool's `see` line or a `within` block that names it can reach",
                            m.inside
                        ),
                    ));
                }
                if let Some(v) = self.views.last() {
                    if !v.allows(inside, write) {
                        return Err(throw(
                            "CapabilityError",
                            format!(
                                "cannot {op} {inside}: it is outside the view of the enclosing `within` block (line {}){}",
                                v.line,
                                if write && v.allows(inside, false) { ", where it is read-only" } else { "" }
                            ),
                        ));
                    }
                }
                if write && m.mode == Mode::Ro {
                    return Err(throw(
                        "IoError",
                        format!(
                            "cannot {op} {inside}: it is under the read-only mount {} (ro){}",
                            m.inside,
                            if m.host != m.inside { format!(" of host path {}", m.host) } else { String::new() }
                        ),
                    ));
                }
                Ok(host)
            }
        }
    }

    fn plan_say(&self, s: String) {
        println!("plan: {s}");
    }

    // ---- flows ------------------------------------------------------------

    /// Run a flow as the entry point. Returns the outputs (set before any failure) and the result.
    pub fn run_entry(&mut self, unit: usize, flow: &Rc<Flow>, args: Vec<(String, Value)>) -> (Vec<(String, Value)>, R<()>) {
        self.execute_flow(unit, flow, args)
    }

    fn call_flow(&mut self, unit: usize, flow: &Rc<Flow>, args: Vec<(String, Value)>) -> R<Value> {
        let (outs, r) = self.execute_flow(unit, flow, args);
        r?;
        Ok(Value::Record(Rc::new(outs)))
    }

    fn execute_flow(&mut self, unit: usize, flow: &Rc<Flow>, args: Vec<(String, Value)>) -> (Vec<(String, Value)>, R<()>) {
        let mut frame = Frame::new(unit);
        for o in &flow.outs {
            frame.outs.push((o.name.clone(), o.ty.clone(), Value::Absent));
        }
        if self.depth >= 64 {
            return (vec![], Err(throw("TypeError", format!("flow `{}` recursed more than 64 levels deep", flow.name))));
        }
        self.depth += 1;
        let mut lock_file: Option<std::fs::File> = None;
        let body = self.flow_body(flow, args, &mut frame, &mut lock_file);
        let fin = match &flow.finally {
            Some(block) => self.exec_block(block, &mut frame),
            None => Ok(()),
        };
        self.depth -= 1;
        drop(lock_file);
        let result = match (body, fin) {
            (Err(e), Err(f)) => {
                eprintln!("weir: `finally` in flow `{}` also failed: {}", flow.name, describe_ctl(&f));
                Err(e)
            }
            (Err(e), Ok(())) => Err(e),
            (Ok(()), Err(f)) => Err(f),
            (Ok(()), Ok(())) => Ok(()),
        };
        let result = match result {
            Err(e) if matches!(*e, Ctl::Return(_)) => Ok(()),
            r => r,
        };
        let outs = frame.outs.into_iter().map(|(n, _, v)| (n, v)).collect();
        (outs, result)
    }

    fn flow_body(&mut self, flow: &Rc<Flow>, mut args: Vec<(String, Value)>, frame: &mut Frame, lock_file: &mut Option<std::fs::File>) -> R<()> {
        for a in &args {
            if !flow.ins.iter().any(|i| i.name == a.0) {
                return Err(type_error(format!("flow `{}` has no input `{}`", flow.name, a.0)));
            }
        }
        for i in &flow.ins {
            let v = match args.iter().position(|(n, _)| n == &i.name) {
                Some(p) => args.remove(p).1,
                None => match &i.default {
                    Some(d) => self.eval(d, frame)?,
                    None if i.ty.is_optional() => Value::Absent,
                    None => return Err(type_error(format!("flow `{}` is missing the input `{}`", flow.name, i.name))),
                },
            };
            if let Err(e) = conforms(&v, &i.ty) {
                return Err(type_error(format!("input `{}` of flow `{}`: {e}", i.name, flow.name)));
            }
            frame.bind(&i.name, v, false);
        }
        for r in &flow.requires {
            let ok = truthy(&self.eval(r, frame)?, "a `requires` condition")?;
            if !ok {
                return Err(Box::new(Ctl::Fail {
                    message: format!("precondition failed: requires {}", show_expr(r)),
                    span: Some(r.span),
                    path: None,
                }));
            }
        }
        if let Some(l) = &flow.lock {
            let p = self.eval(l, frame)?;
            let inside = match &p {
                Value::Str(s) | Value::Path(s) => s.to_string(),
                v => return Err(type_error(format!("`lock` needs a path, found {}", v.type_name()))),
            };
            if self.opts.plan {
                self.plan_say(format!("lock {inside}"));
            } else if self.test_mode {
                // tests touch no real state, and the script's lock path is not mounted in a test
            } else {
                let host = self.host_path(&inside, true, "lock")?;
                let f = std::fs::OpenOptions::new()
                    .create(true)
                    .truncate(false)
                    .write(true)
                    .open(&host)
                    .map_err(|e| throw("IoError", format!("cannot open lock file {inside}: {e}")))?;
                match f.try_lock() {
                    Ok(()) => *lock_file = Some(f),
                    Err(std::fs::TryLockError::WouldBlock) => {
                        return Err(Box::new(Ctl::Fail {
                            message: format!("the lock {inside} is already held by another run"),
                            span: Some(l.span),
                            path: None,
                        }))
                    }
                    Err(std::fs::TryLockError::Error(e)) => return Err(throw("IoError", format!("cannot lock {inside}: {e}"))),
                }
            }
        }
        self.exec_stmts(&flow.body.stmts, frame)
    }

    // ---- statements -------------------------------------------------------

    fn exec_block(&mut self, b: &Block, frame: &mut Frame) -> R<()> {
        frame.scopes.push(HashMap::new());
        let r = self.exec_stmts(&b.stmts, frame);
        frame.scopes.pop();
        r
    }

    fn exec_stmts(&mut self, stmts: &[Stmt], frame: &mut Frame) -> R<()> {
        for s in stmts {
            self.exec_stmt(s, frame).map_err(|mut e| {
                stamp_span(&mut e, s.span);
                e
            })?;
        }
        Ok(())
    }

    fn record_step(&mut self, name: &str, status: &'static str, detail: Option<String>) {
        let mut full = self.path.clone();
        full.push(name.to_string());
        self.steps.push(StepRec { name: full.join(" > "), status, detail });
    }

    /// Run `f` as a step: guard, confirmation, identity and recording.
    fn run_step(
        &mut self,
        name: String,
        when: &Option<Expr>,
        confirmable: bool,
        frame: &mut Frame,
        f: impl FnOnce(&mut Interp, &mut Frame) -> R<()>,
    ) -> R<()> {
        if let Some(w) = when {
            let ok = truthy(&self.eval(w, frame)?, "a `when` guard")?;
            if !ok {
                self.record_step(&name, "skipped", Some("when: false".into()));
                if self.opts.plan {
                    self.plan_say(format!("step {} skipped (when: {})", self.full_name(&name), show_expr(w)));
                }
                return Ok(());
            }
        }
        if confirmable && !self.opts.confirm {
            if self.opts.plan {
                self.plan_say(format!("step {} is confirmable and runs only with --confirm; its effects follow", self.full_name(&name)));
            } else {
                self.record_step(&name, "skipped", Some("unconfirmed".into()));
                eprintln!("weir: step {}: skipped: unconfirmed", self.full_name(&name));
                return Ok(());
            }
        }
        self.path.push(name.clone());
        let r = f(self, frame);
        let path_now = self.path.join(" > ");
        self.path.pop();
        match r {
            Ok(()) => {
                self.record_step(&name, "ok", None);
                Ok(())
            }
            Err(mut e) => {
                if !matches!(*e, Ctl::Return(_)) {
                    set_path(&mut e, &path_now);
                    let d = describe_ctl(&e);
                    self.record_step(&name, "failed", Some(d));
                }
                Err(e)
            }
        }
    }

    fn full_name(&self, name: &str) -> String {
        let mut p = self.path.clone();
        p.push(name.to_string());
        p.join(" > ")
    }

    fn exec_stmt(&mut self, s: &Stmt, frame: &mut Frame) -> R<()> {
        match &s.kind {
            StmtKind::Let { name, ty, expr } | StmtKind::Var { name, ty, expr } => {
                let v = self.eval(expr, frame)?;
                if let Some(t) = ty {
                    conforms(&v, t).map_err(|e| type_error(format!("`{name}` is declared {}: {e}", t.show())))?;
                }
                frame.bind(name, v, matches!(s.kind, StmtKind::Var { .. }));
                Ok(())
            }
            StmtKind::Assign { name, expr } => {
                let v = self.eval(expr, frame)?;
                match frame.slot_mut(name) {
                    Some(slot) if slot.mutable => {
                        slot.val = v;
                        Ok(())
                    }
                    Some(_) => Err(type_error(format!("`{name}` is a `let` binding and cannot be assigned; declare it with `var`"))),
                    None => Err(type_error(format!("`{name}` is not defined"))),
                }
            }
            StmtKind::Set { name, expr } => {
                let v = self.eval(expr, frame)?;
                match frame.outs.iter_mut().find(|(n, _, _)| n == name) {
                    Some((_, ty, slot)) => {
                        conforms(&v, ty).map_err(|e| type_error(format!("output `{name}` is declared {}: {e}", ty.show())))?;
                        *slot = v;
                        Ok(())
                    }
                    None => Err(type_error(format!("there is no `out {name}` declared in this flow"))),
                }
            }
            StmtKind::If { cond, then, else_ } => {
                if truthy(&self.eval(cond, frame)?, "an `if` condition")? {
                    self.exec_block(then, frame)
                } else if let Some(e) = else_ {
                    self.exec_block(e, frame)
                } else {
                    Ok(())
                }
            }
            StmtKind::IfLet { name, expr, then, else_ } => {
                let v = self.eval(expr, frame)?;
                if matches!(v, Value::Absent) {
                    if let Some(e) = else_ {
                        self.exec_block(e, frame)?;
                    }
                    Ok(())
                } else {
                    frame.scopes.push(HashMap::new());
                    frame.bind(name, v, false);
                    let r = self.exec_block(then, frame);
                    frame.scopes.pop();
                    r
                }
            }
            StmtKind::For { var, iter, body } => {
                let items = match self.eval(iter, frame)? {
                    Value::List(l) => l,
                    v => return Err(type_error(format!("`for` needs a list, found {}", v.type_name()))),
                };
                for (i, item) in items.iter().enumerate() {
                    frame.scopes.push(HashMap::new());
                    frame.bind(var, item.clone(), false);
                    frame.loops.push(i);
                    let r = self.exec_block(body, frame);
                    frame.loops.pop();
                    frame.scopes.pop();
                    r?;
                }
                Ok(())
            }
            StmtKind::Try { body, catches } => match self.exec_block(body, frame) {
                Err(e) => {
                    if let Ctl::Throw(t) = &*e {
                        if let Some(c) = catches.iter().find(|c| c.err_ty == t.kind) {
                            let errv = Value::record(vec![
                                ("message", Value::str(&t.message)),
                                ("kind", Value::str(&t.kind)),
                                ("code", Value::Int(t.code.unwrap_or(-1))),
                            ]);
                            frame.scopes.push(HashMap::new());
                            frame.bind(&c.var, errv, false);
                            let r = self.exec_block(&c.body, frame);
                            frame.scopes.pop();
                            return r;
                        }
                    }
                    Err(e)
                }
                ok => ok,
            },
            StmtKind::Return(e) => {
                let v = self.eval(e, frame)?;
                Err(Box::new(Ctl::Return(v)))
            }
            StmtKind::Fail(e) => {
                let v = self.eval(e, frame)?;
                let message = match v {
                    Value::Secret(_) => return Err(throw("CapabilityError", "a secret cannot be used in a failure message")),
                    v => v.display(),
                };
                Err(Box::new(Ctl::Fail { message, span: Some(s.span), path: None }))
            }
            StmtKind::Expect(e) => {
                if truthy(&self.eval(e, frame)?, "an `expect` condition")? {
                    Ok(())
                } else {
                    Err(Box::new(Ctl::Fail { message: format!("expectation failed: {}", show_expr(e)), span: Some(s.span), path: None }))
                }
            }
            StmtKind::Step(step) => {
                let name = self.step_identity(&step.name, &step.as_label, frame)?;
                self.run_step(name, &step.when, step.confirmable, frame, |me, fr| me.exec_block(&step.body, fr))
            }
            StmtKind::Expr { expr, as_label, when, confirmable } => {
                let is_flow_call = match &expr.kind {
                    ExprKind::Call { name, .. } => self.resolve_flow(frame.unit, name).is_some(),
                    ExprKind::RunFlow { .. } => true,
                    _ => false,
                };
                if is_flow_call {
                    let base = match &expr.kind {
                        ExprKind::Call { name, .. } | ExprKind::RunFlow { name, .. } => name.clone(),
                        _ => unreachable!(),
                    };
                    let name = self.step_identity(&base, as_label, frame)?;
                    self.run_step(name, when, *confirmable, frame, |me, fr| me.eval(expr, fr).map(|_| ()))
                } else if when.is_some() || *confirmable {
                    let name = format!("line {}", s.span.line);
                    self.run_step(name, when, *confirmable, frame, |me, fr| me.eval(expr, fr).map(|_| ()))
                } else {
                    self.eval(expr, frame).map(|_| ())
                }
            }
        }
    }

    fn step_identity(&mut self, name: &str, label: &Option<Expr>, frame: &mut Frame) -> R<String> {
        match label {
            None => Ok(name.to_string()),
            Some(l) => {
                let v = self.eval(l, frame)?;
                let idx = frame.loops.last().copied();
                Ok(match idx {
                    Some(i) => format!("{name}[{i}] \"{}\"", v.display()),
                    None => format!("{name} \"{}\"", v.display()),
                })
            }
        }
    }

    // ---- name resolution --------------------------------------------------

    fn resolve_flow(&self, unit: usize, name: &str) -> Option<(usize, Rc<Flow>)> {
        let u = &self.units[unit];
        if let Some(f) = u.flows.get(name) {
            return Some((unit, f.clone()));
        }
        u.imports.get(name).cloned()
    }

    // ---- expressions ------------------------------------------------------

    pub fn eval(&mut self, e: &Expr, frame: &mut Frame) -> R<Value> {
        self.eval_inner(e, frame).map_err(|mut c| {
            stamp_span(&mut c, e.span);
            c
        })
    }

    fn eval_inner(&mut self, e: &Expr, frame: &mut Frame) -> R<Value> {
        match &e.kind {
            ExprKind::Int(n) => Ok(Value::Int(*n)),
            ExprKind::Bool(b) => Ok(Value::Bool(*b)),
            ExprKind::Absent => Ok(Value::Absent),
            ExprKind::Dur(n, _) => Ok(Value::Dur(*n)),
            ExprKind::Str(segs) => {
                let mut out = String::new();
                for s in segs {
                    match s {
                        StrSeg::Lit(l) => out.push_str(l),
                        StrSeg::Expr(x) => match self.eval(x, frame)? {
                            Value::Secret(_) => return Err(throw("CapabilityError", "a secret cannot be interpolated into a string")),
                            Value::Absent => {
                                return Err(type_error(format!(
                                    "`{}` is absent and cannot be interpolated; unwrap it with `if let`",
                                    show_expr(x)
                                )))
                            }
                            v => out.push_str(&v.display()),
                        },
                    }
                }
                Ok(Value::str(out))
            }
            ExprKind::Ident(n) => match frame.lookup(n) {
                Some(v) => Ok(v.clone()),
                None if n == "asc" || n == "desc" => Ok(Value::str(n)),
                None => Err(type_error(format!("`{n}` is not defined"))),
            },
            ExprKind::List(items) => Ok(Value::list(self.eval_list_items(items, frame)?)),
            ExprKind::Record(fields) => {
                let mut out = Vec::with_capacity(fields.len());
                for (n, x) in fields {
                    out.push((n.clone(), self.eval(x, frame)?));
                }
                Ok(Value::Record(Rc::new(out)))
            }
            ExprKind::Call { name, args } => self.eval_call(name, args, frame, e.span),
            ExprKind::RunFlow { name, args } => {
                let (u, flow) = self
                    .resolve_flow(frame.unit, name)
                    .ok_or_else(|| type_error(format!("there is no flow `{name}`")))?;
                let a = self.eval_named_args(args, frame)?;
                self.call_flow(u, &flow, a)
            }
            ExprKind::Stage { recv, name, args } => self.eval_stage(recv, name, args, frame),
            ExprKind::Member { recv, name } => {
                let v = self.eval(recv, frame)?;
                self.member(&v, name)
            }
            ExprKind::Unary(op, x) => {
                let v = self.eval(x, frame)?;
                match (op, v) {
                    (UnOp::Not, Value::Bool(b)) => Ok(Value::Bool(!b)),
                    (UnOp::Neg, Value::Int(n)) => Ok(Value::Int(-n)),
                    (UnOp::Neg, Value::Dur(n)) => Ok(Value::Dur(-n)),
                    (UnOp::Not, v) => Err(type_error(format!("`not` needs a bool, found {}", v.type_name()))),
                    (UnOp::Neg, v) => Err(type_error(format!("`-` needs an int or duration, found {}", v.type_name()))),
                }
            }
            ExprKind::Binary(op, l, r) => self.eval_binary(*op, l, r, frame),
            ExprKind::Try(inner) => match self.eval(inner, frame) {
                Ok(v) => Ok(attempt_record(true, 0, v, "")),
                Err(c) => match *c {
                    Ctl::Throw(t) => Ok(attempt_record(false, t.code.unwrap_or(-1), Value::Absent, &t.message)),
                    Ctl::Fail { message, .. } => Ok(attempt_record(false, -1, Value::Absent, &message)),
                    other => Err(Box::new(other)),
                },
            },
            ExprKind::If { cond, then, else_ } => {
                let c = truthy(&self.eval(cond, frame)?, "an `if` condition")?;
                self.eval_block_value(if c { then } else { else_ }, frame)
            }
            ExprKind::Within { items, body } => {
                let view = self.build_view(items, e.span.line, frame)?;
                self.views.push(view);
                let r = self.exec_block(body, frame);
                self.views.pop();
                match r {
                    Ok(()) => Ok(Value::Absent),
                    Err(c) => match *c {
                        Ctl::Return(v) => Ok(v),
                        other => Err(Box::new(other)),
                    },
                }
            }
        }
    }

    /// Is this explicit mount out of reach here, because no enclosing `within` block names it?
    fn explicit_blocks(&self, m: &crate::mounts::MountEntry) -> bool {
        m.explicit && !self.views.last().is_some_and(|v| v.requests(&m.inside))
    }

    fn explicit_insides(&self) -> Vec<String> {
        self.mounts.entries.iter().filter(|e| e.explicit).map(|e| e.inside.clone()).collect()
    }

    /// (inside path, where the sandbox holds it) for each explicit mount.
    fn explicit_pairs(&self) -> Vec<(String, String)> {
        self.mounts.entries.iter().filter(|e| e.explicit).map(|e| (e.inside.clone(), e.host.clone())).collect()
    }

    /// What a program sees when nothing narrows it further: the system set and every mount that is not explicit.
    fn default_view(&self) -> (Vec<(String, Mode)>, bool) {
        let paths = self
            .mounts
            .entries
            .iter()
            .filter(|e| e.origin == "declared" && !e.explicit)
            .map(|e| (e.inside.clone(), e.mode))
            .collect();
        (paths, self.mounts.is_system_mounted())
    }

    /// Evaluate a `within` header into a view, refusing anything the enclosing view does not have.
    fn build_view(&mut self, items: &[(See, Span)], line: u32, frame: &mut Frame) -> R<View> {
        let mut view = View { system: false, paths: vec![], line };
        for (see, _) in items {
            match see {
                See::System => {
                    let have = match self.views.last() {
                        Some(v) => v.system,
                        None => self.mounts.is_system_mounted(),
                    };
                    if !have {
                        return Err(throw("CapabilityError", "a `within` block needs `system`, but it is not in the enclosing view".to_string()));
                    }
                    view.system = true;
                }
                See::Path { path, mode } => {
                    let p = normalize(&self.eval(path, frame)?.display());
                    let write = *mode == Mode::Rw;
                    // An explicit mount can be requested from the script's top level, or from a view that already requested it.
                    if let Some((m, _)) = self.mounts.find(&p) {
                        if m.explicit && self.views.last().is_some_and(|v| !v.requests(&m.inside)) {
                            return Err(throw(
                                "CapabilityError",
                                format!("a `within` block asks for the explicit mount {}, which the enclosing view does not include", m.inside),
                            ));
                        }
                    }
                    let ok = match self.views.last() {
                        Some(v) => v.allows(&p, write),
                        None => matches!(self.mounts.find(&p), Some((m, _)) if !(m.mode == Mode::Ro && write)),
                    };
                    if !ok {
                        return Err(throw(
                            "CapabilityError",
                            format!("a `within` block needs {p} {}, which the enclosing view does not have in that mode", mode.word()),
                        ));
                    }
                    view.paths.push((p, *mode));
                }
            }
        }
        Ok(view)
    }

    fn eval_block_value(&mut self, b: &Block, frame: &mut Frame) -> R<Value> {
        frame.scopes.push(HashMap::new());
        let mut result = Ok(Value::Absent);
        let n = b.stmts.len();
        for (i, s) in b.stmts.iter().enumerate() {
            if i + 1 == n {
                if let StmtKind::Expr { expr, as_label: None, when: None, confirmable: false } = &s.kind {
                    result = self.eval(expr, frame);
                    break;
                }
            }
            if let Err(e) = self.exec_stmt(s, frame) {
                result = Err(e);
                break;
            }
        }
        frame.scopes.pop();
        result
    }

    fn eval_list_items(&mut self, items: &[Arg], frame: &mut Frame) -> R<Vec<Value>> {
        let mut out = Vec::with_capacity(items.len());
        for it in items {
            let v = self.eval(&it.expr, frame)?;
            if it.spread {
                match v {
                    Value::List(l) => out.extend(l.iter().cloned()),
                    v => return Err(type_error(format!("`...` spreads a list, found {}", v.type_name()))),
                }
            } else {
                out.push(v);
            }
        }
        Ok(out)
    }

    fn eval_named_args(&mut self, args: &[Arg], frame: &mut Frame) -> R<Vec<(String, Value)>> {
        let mut out = Vec::with_capacity(args.len());
        for a in args {
            match &a.name {
                Some(n) => {
                    if out.iter().any(|(k, _): &(String, Value)| k == n) {
                        return Err(type_error(format!("argument `{n}` is given twice")));
                    }
                    out.push((n.clone(), self.eval(&a.expr, frame)?));
                }
                None => return Err(type_error("`...` spreads are only allowed inside a list literal")),
            }
        }
        Ok(out)
    }

    fn member(&self, v: &Value, name: &str) -> R<Value> {
        match v {
            Value::Record(r) => r
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.clone())
                .ok_or_else(|| type_error(format!("no field `{name}`; the record has: {}", r.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>().join(", ")))),
            Value::Dur(s) => match name {
                "seconds" => Ok(Value::Int(*s)),
                "minutes" => Ok(Value::Int(s / 60)),
                "hours" => Ok(Value::Int(s / 3600)),
                "days" => Ok(Value::Int(s / 86400)),
                _ => Err(type_error(format!("a duration has `seconds`, `minutes`, `hours` and `days`, not `{name}`"))),
            },
            v => Err(type_error(format!("a {} has no member `{name}`", v.type_name()))),
        }
    }

    fn eval_binary(&mut self, op: BinOp, l: &Expr, r: &Expr, frame: &mut Frame) -> R<Value> {
        if matches!(op, BinOp::And | BinOp::Or) {
            let lv = truthy(&self.eval(l, frame)?, "the left side of `and`/`or`")?;
            if (op == BinOp::And && !lv) || (op == BinOp::Or && lv) {
                return Ok(Value::Bool(lv));
            }
            return Ok(Value::Bool(truthy(&self.eval(r, frame)?, "the right side of `and`/`or`")?));
        }
        let a = self.eval(l, frame)?;
        let b = self.eval(r, frame)?;
        let bad = |a: &Value, b: &Value, sym: &str| {
            type_error(format!("`{sym}` cannot combine {} and {}", a.type_name(), b.type_name()))
        };
        match op {
            BinOp::Eq => Ok(Value::Bool(a == b)),
            BinOp::Ne => Ok(Value::Bool(a != b)),
            BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                let sym = match op {
                    BinOp::Lt => "<",
                    BinOp::Le => "<=",
                    BinOp::Gt => ">",
                    _ => ">=",
                };
                let ord = a.compare(&b).ok_or_else(|| bad(&a, &b, sym))?;
                Ok(Value::Bool(match op {
                    BinOp::Lt => ord.is_lt(),
                    BinOp::Le => ord.is_le(),
                    BinOp::Gt => ord.is_gt(),
                    _ => ord.is_ge(),
                }))
            }
            BinOp::Add => match (&a, &b) {
                (Value::Int(x), Value::Int(y)) => x.checked_add(*y).map(Value::Int).ok_or_else(|| type_error("integer overflow")),
                (Value::Str(x), Value::Str(y)) => Ok(Value::str(format!("{x}{y}"))),
                (Value::Dur(x), Value::Dur(y)) => Ok(Value::Dur(x + y)),
                (Value::List(x), Value::List(y)) => {
                    let mut v = (**x).clone();
                    v.extend(y.iter().cloned());
                    Ok(Value::list(v))
                }
                _ => Err(bad(&a, &b, "+")),
            },
            BinOp::Sub => match (&a, &b) {
                (Value::Int(x), Value::Int(y)) => x.checked_sub(*y).map(Value::Int).ok_or_else(|| type_error("integer overflow")),
                (Value::Dur(x), Value::Dur(y)) => Ok(Value::Dur(x - y)),
                _ => Err(bad(&a, &b, "-")),
            },
            BinOp::Mul => match (&a, &b) {
                (Value::Int(x), Value::Int(y)) => x.checked_mul(*y).map(Value::Int).ok_or_else(|| type_error("integer overflow")),
                (Value::Dur(x), Value::Int(y)) | (Value::Int(y), Value::Dur(x)) => Ok(Value::Dur(x * y)),
                _ => Err(bad(&a, &b, "*")),
            },
            BinOp::Div => match (&a, &b) {
                (Value::Int(_), Value::Int(0)) => Err(type_error("division by zero")),
                (Value::Int(x), Value::Int(y)) => Ok(Value::Int(x / y)),
                (Value::Path(x), Value::Str(y)) | (Value::Path(x), Value::Path(y)) => {
                    Ok(Value::path(format!("{}/{}", x.trim_end_matches('/'), y.trim_start_matches('/'))))
                }
                _ => Err(bad(&a, &b, "/")),
            },
            BinOp::And | BinOp::Or => unreachable!(),
        }
    }

    // ---- calls ------------------------------------------------------------

    fn eval_call(&mut self, name: &str, args: &[Arg], frame: &mut Frame, span: Span) -> R<Value> {
        let unit = frame.unit;
        if let Some(f) = self.units[unit].fns.get(name).cloned() {
            let a = self.eval_named_args(args, frame)?;
            return self.call_fn(unit, &f, a);
        }
        if let Some((u, flow)) = self.resolve_flow(unit, name) {
            let a = self.eval_named_args(args, frame)?;
            return self.call_flow(u, &flow, a);
        }
        if let Some(f) = self.units[unit].tools.get(name).cloned() {
            let a = self.eval_named_args(args, frame)?;
            return self.call_tool(unit, &f, a, None);
        }
        if builtins::is_builtin(name) {
            let a = if name == "run" { self.eval_named_args(args, frame)? } else { self.eval_named_args(args, frame)? };
            return self.call_builtin(name, a, span);
        }
        Err(type_error(format!("there is no flow, tool, fn or built-in named `{name}`")))
    }

    fn call_fn(&mut self, unit: usize, f: &Rc<FnDecl>, mut args: Vec<(String, Value)>) -> R<Value> {
        let mut frame = Frame::new(unit);
        for a in &args {
            if !f.params.iter().any(|p| p.name == a.0) {
                return Err(type_error(format!("fn `{}` has no parameter `{}`", f.name, a.0)));
            }
        }
        for p in &f.params {
            let pos = args
                .iter()
                .position(|(n, _)| n == &p.name)
                .ok_or_else(|| type_error(format!("fn `{}` is missing the argument `{}`", f.name, p.name)))?;
            let v = args.remove(pos).1;
            conforms(&v, &p.ty).map_err(|e| type_error(format!("argument `{}` of fn `{}`: {e}", p.name, f.name)))?;
            frame.bind(&p.name, v, false);
        }
        if self.depth >= 200 {
            return Err(type_error(format!("fn `{}` recursed more than 200 levels deep", f.name)));
        }
        self.depth += 1;
        let r = self.exec_block(&f.body, &mut frame);
        self.depth -= 1;
        let v = match r {
            Ok(()) => Value::Absent,
            Err(e) => match *e {
                Ctl::Return(v) => v,
                other => return Err(Box::new(other)),
            },
        };
        conforms(&v, &f.ret).map_err(|e| type_error(format!("fn `{}` returns {}: {e}", f.name, f.ret.show())))?;
        Ok(v)
    }

    fn call_builtin(&mut self, name: &str, args: Vec<(String, Value)>, span: Span) -> R<Value> {
        let names: Vec<&str> = args.iter().map(|(k, _)| k.as_str()).collect();
        let sig = builtins::find(name, &names).ok_or_else(|| type_error(format!("unknown built-in `{name}`")))?;
        for (n, _) in &args {
            if !sig.params.iter().any(|(p, _)| p == n) {
                return Err(type_error(format!(
                    "`{name}` has no argument `{n}`; its arguments are: {}",
                    sig.params.iter().map(|(p, _)| *p).collect::<Vec<_>>().join(", ")
                )));
            }
        }
        for (p, req) in sig.params {
            if *req && !names.contains(p) {
                return Err(type_error(format!("`{name}` is missing the argument `{p}`")));
            }
        }
        let a = Args(args);
        if let Some(r) = builtins::call_pure(name, &a) {
            return r;
        }
        let _ = span;
        match (sig.effect, name) {
            (_, "print") => {
                let t = match a.get("text") {
                    Some(Value::Secret(_)) => return Err(throw("CapabilityError", "a secret cannot be printed")),
                    Some(v) => v.display(),
                    None => String::new(),
                };
                if self.test_mode {
                    self.captured.push(t);
                } else if self.opts.print_to_stderr {
                    let _ = writeln!(std::io::stderr(), "{t}");
                } else {
                    let mut out = std::io::stdout().lock();
                    if writeln!(out, "{t}").is_err() {
                        // a closed pipe (for example `| head`) ends the script quietly
                        std::process::exit(0);
                    }
                }
                Ok(Value::Absent)
            }
            (_, "now_epoch") => {
                let secs = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0);
                Ok(Value::Int(secs))
            }
            (_, "exists") => {
                let p = a.text("path")?;
                // A path outside the mounts, or outside the enclosing `within` view, does not exist.
                let visible = self.views.last().is_none_or(|v| v.allows(&p, false));
                Ok(Value::Bool(match self.mounts.find(&p) {
                    Some((m, host)) if visible && !self.explicit_blocks(m) => std::path::Path::new(&host).exists(),
                    _ => false,
                }))
            }
            (_, "read_text") => {
                let p = a.text("file")?;
                let host = self.host_path(&p, false, "read")?;
                let bytes = std::fs::read(&host).map_err(|e| throw("IoError", format!("cannot read {p}: {e}")))?;
                Ok(Value::str(String::from_utf8_lossy(&bytes)))
            }
            (_, "read_lines") => {
                let p = a.text("file")?;
                let host = self.host_path(&p, false, "read")?;
                let bytes = std::fs::read(&host).map_err(|e| throw("IoError", format!("cannot read {p}: {e}")))?;
                Ok(split_lines(&String::from_utf8_lossy(&bytes)))
            }
            (_, "write") => {
                let p = a.text("file")?;
                let text = a.text("text")?;
                if self.opts.plan {
                    self.plan_say(format!("write {p} ({} bytes)", text.len()));
                    return Ok(Value::Absent);
                }
                let host = self.host_path(&p, true, "write")?;
                std::fs::write(&host, text).map_err(|e| throw("IoError", format!("cannot write {p}: {e}")))?;
                Ok(Value::Absent)
            }
            (_, "write_lines") | (_, "append_lines") => {
                let p = a.text("file")?;
                let lines = a.strings("lines")?;
                let append = name == "append_lines";
                if self.opts.plan {
                    self.plan_say(format!("{} {p} ({} line(s))", if append { "append to" } else { "write" }, lines.len()));
                    return Ok(Value::Absent);
                }
                let host = self.host_path(&p, true, "write")?;
                let mut body = String::new();
                for l in &lines {
                    body.push_str(l);
                    body.push('\n');
                }
                let r = if append {
                    std::fs::OpenOptions::new().create(true).append(true).open(&host).and_then(|mut f| f.write_all(body.as_bytes()))
                } else {
                    std::fs::write(&host, body)
                };
                r.map_err(|e| throw("IoError", format!("cannot write {p}: {e}")))?;
                Ok(Value::Absent)
            }
            (_, "copy") => {
                let (from, to) = (a.text("from")?, a.text("to")?);
                if self.opts.plan {
                    self.plan_say(format!("copy {from} to {to}"));
                    return Ok(Value::Absent);
                }
                let src = self.host_path(&from, false, "read")?;
                let dst = self.host_path(&to, true, "write")?;
                std::fs::copy(&src, &dst).map_err(|e| throw("IoError", format!("cannot copy {from} to {to}: {e}")))?;
                Ok(Value::Absent)
            }
            (_, "move") => {
                let (from, to) = (a.text("from")?, a.text("to")?);
                self.refuse_mount_point(&from, "move")?;
                if self.opts.plan {
                    self.plan_say(format!("move {from} to {to}"));
                    return Ok(Value::Absent);
                }
                let src = self.host_path(&from, true, "move")?;
                let dst = self.host_path(&to, true, "write")?;
                if std::fs::rename(&src, &dst).is_err() {
                    std::fs::copy(&src, &dst)
                        .and_then(|_| std::fs::remove_file(&src))
                        .map_err(|e| throw("IoError", format!("cannot move {from} to {to}: {e}")))?;
                }
                Ok(Value::Absent)
            }
            (_, "delete") => {
                let p = a.text("path")?;
                self.refuse_mount_point(&p, "delete")?;
                if self.opts.plan {
                    self.plan_say(format!("delete {p}"));
                    return Ok(Value::Absent);
                }
                let host = self.host_path(&p, true, "delete")?;
                let meta = std::fs::symlink_metadata(&host).map_err(|e| throw("IoError", format!("cannot delete {p}: {e}")))?;
                let r = if meta.is_dir() { std::fs::remove_dir_all(&host) } else { std::fs::remove_file(&host) };
                r.map_err(|e| throw("IoError", format!("cannot delete {p}: {e}")))?;
                Ok(Value::Absent)
            }
            (_, "run") => {
                let cmd = a.text("cmd")?;
                let argv = if a.get("args").is_some() { a.strings("args")? } else { vec![] };
                let ok_codes: Vec<i64> = match a.get("ok_codes") {
                    Some(Value::List(l)) => l.iter().filter_map(|v| if let Value::Int(n) = v { Some(*n) } else { None }).collect(),
                    _ => vec![0],
                };
                self.exec_program(&cmd, argv, vec![], None, &ok_codes, IoKind::Lines, None)
            }
            _ => Err(type_error(format!("built-in `{name}` is not implemented"))),
        }
    }

    /// `delete` and `move` refuse the sandbox root and any mount point itself.
    fn refuse_mount_point(&self, p: &str, op: &str) -> R<()> {
        if p.starts_with('/') && self.mounts.is_mount_point(p) {
            return Err(throw(
                "IoError",
                format!("refusing to {op} {}: it is {}", normalize(p), if normalize(p) == "/" { "the sandbox root" } else { "a mount point" }),
            ));
        }
        Ok(())
    }

    // ---- tools and programs --------------------------------------------

    fn call_tool(&mut self, unit: usize, f: &Rc<Tool>, mut args: Vec<(String, Value)>, stdin: Option<Value>) -> R<Value> {
        for a in &args {
            if !f.ins.iter().any(|i| i.name == a.0) {
                return Err(type_error(format!(
                    "tool `{}` has no input `{}`; its inputs are: {}",
                    f.name,
                    a.0,
                    if f.ins.is_empty() { "(none)".to_string() } else { f.ins.iter().map(|i| i.name.as_str()).collect::<Vec<_>>().join(", ") }
                )));
            }
        }
        let mut frame = Frame::new(unit);
        let mut bound: Vec<(String, Value)> = Vec::new();
        for i in &f.ins {
            let v = match args.iter().position(|(n, _)| n == &i.name) {
                Some(p) => args.remove(p).1,
                None => match &i.default {
                    Some(d) => self.eval(d, &mut frame)?,
                    None if i.ty.is_optional() => Value::Absent,
                    None => return Err(type_error(format!("tool `{}` is missing the input `{}`", f.name, i.name))),
                },
            };
            conforms(&v, &i.ty).map_err(|e| type_error(format!("input `{}` of tool `{}`: {e}", i.name, f.name)))?;
            if self.test_mode {
                bound.push((i.name.clone(), v.clone()));
            }
            frame.bind(&i.name, v, false);
        }

        if self.test_mode {
            // The stubs for this tool whose arguments match this call, in the order written.
            // Each is used once, in turn; the last one then answers every further call.
            let matching: Vec<usize> = (0..self.stubs.len())
                .filter(|&k| {
                    let s = &self.stubs[k];
                    s.tool == f.name && s.when.iter().all(|(n, v)| bound.iter().any(|(bn, bv)| bn == n && bv == v))
                })
                .collect();
            let pick = matching.iter().copied().find(|&k| !self.stubs[k].used).or(matching.last().copied());
            return match pick {
                Some(k) => {
                    self.stubs[k].used = true;
                    Ok(self.stubs[k].value.clone())
                }
                None if self.stubs.iter().any(|s| s.tool == f.name) => Err(throw(
                    "ExecError",
                    format!(
                        "no stub for `{}` matches this call ({}); the stubs are matched on the arguments they name",
                        f.name,
                        bound.iter().map(|(n, v)| format!("{n}: {}", v.display())).collect::<Vec<_>>().join(", ")
                    ),
                )),
                None => Err(throw(
                    "ExecError",
                    format!("tool `{}` has no stub: tests do not run real programs (add `stub {} yields {}: ...`)", f.name, f.name, f.yields.word()),
                )),
            };
        }

        // `see` clauses must be satisfied by the sandbox; they also narrow what the program sees.
        let mut narrow: Option<(Vec<(String, Mode)>, bool)> = None;
        if let Some(sees) = &f.see {
            let mut paths: Vec<(String, Mode)> = Vec::new();
            let mut system = false;
            for (see, _) in sees {
                match see {
                    See::System => {
                        system = true;
                        if !self.mounts.is_system_mounted() {
                            return Err(throw(
                                "CapabilityError",
                                format!("tool `{}` needs `see system`, but the script does not `mount system`", f.name),
                            ));
                        }
                    }
                    See::Path { path, mode } => {
                        let p = self.eval(path, &mut frame)?.display();
                        match self.mounts.find(&p) {
                            Some((m, _)) if m.mode == Mode::Ro && *mode == Mode::Rw => {
                                return Err(throw(
                                    "CapabilityError",
                                    format!("tool `{}` needs {p} rw, but the sandbox mounts {} ro", f.name, m.inside),
                                ))
                            }
                            Some(_) => paths.push((normalize(&p), *mode)),
                            None => {
                                return Err(throw(
                                    "CapabilityError",
                                    format!("tool `{}` needs {p} {}, which is not mounted in the sandbox", f.name, mode.word()),
                                ))
                            }
                        }
                    }
                }
            }
            narrow = Some((paths, system));
        }

        let mut argv: Vec<String> = Vec::new();
        for a in &f.args {
            let v = self.eval(&a.expr, &mut frame)?;
            if a.spread {
                match v {
                    Value::List(l) => {
                        for x in l.iter() {
                            argv.push(arg_text(x)?);
                        }
                    }
                    v => return Err(type_error(format!("`...` spreads a list, found {}", v.type_name()))),
                }
            } else {
                argv.push(arg_text(&v)?);
            }
        }
        let mut env = Vec::new();
        for (n, x) in &f.env {
            let v = self.eval(x, &mut frame)?;
            let text = match &v {
                Value::Secret(s) => s.to_string(),
                v => v.display(),
            };
            env.push((n.clone(), text));
        }

        if f.takes == IoKind::None && stdin.is_some() {
            return Err(type_error(format!("tool `{}` takes no input, so nothing can be chained into it", f.name)));
        }
        let stdin_text = match f.takes {
            IoKind::None => None,
            IoKind::Lines | IoKind::Text | IoKind::Json => match stdin {
                None => {
                    return Err(type_error(format!(
                        "tool `{}` takes {} on stdin, so it must follow another stage: `.{}(...)`",
                        f.name,
                        f.takes.word(),
                        f.name
                    )))
                }
                Some(Value::Str(s)) => Some(s.to_string()),
                Some(Value::List(l)) => {
                    let mut t = String::new();
                    for x in l.iter() {
                        match x {
                            Value::Str(s) | Value::Path(s) => t.push_str(s),
                            v => t.push_str(&v.display()),
                        }
                        t.push('\n');
                    }
                    Some(t)
                }
                Some(Value::Secret(s)) => Some(s.to_string()),
                Some(v) => return Err(type_error(format!("tool `{}` cannot take a {} on stdin", f.name, v.type_name()))),
            },
        };
        let program = f.program.clone();
        let ok = f.ok_codes.clone();
        self.exec_program(&program, argv, env, stdin_text, &ok, f.yields, narrow)
    }

    fn exec_program(
        &mut self,
        program: &str,
        argv: Vec<String>,
        env: Vec<(String, String)>,
        stdin: Option<String>,
        ok_codes: &[i64],
        yields: IoKind,
        narrow: Option<(Vec<(String, Mode)>, bool)>,
    ) -> R<Value> {
        let base = program.rsplit('/').next().unwrap_or(program);
        if self.opts.deny_exec.iter().any(|d| d == base || d == program) {
            return Err(throw("CapabilityError", format!("`{program}` is denied for this run (--deny-exec {base})")));
        }
        let shown = format!("{program} {}", argv.iter().map(|a| shell_quote(a)).collect::<Vec<_>>().join(" "));
        if self.opts.plan {
            self.plan_say(format!("run {}", shown.trim_end()));
            return Ok(match yields {
                IoKind::Lines => Value::list(vec![]),
                IoKind::Text => Value::str(""),
                _ => Value::Absent,
            });
        }
        let mut cmd = Command::new(program);
        cmd.args(&argv).env_clear();
        cmd.env("PATH", std::env::var("PATH").unwrap_or_else(|_| "/usr/local/bin:/usr/bin:/bin".into()));
        for k in ["LANG", "TZ"] {
            if let Ok(v) = std::env::var(k) {
                cmd.env(k, v);
            }
        }
        cmd.env("HOME", "/tmp");
        for (k, v) in &env {
            cmd.env(k, v);
        }
        cmd.current_dir(if std::path::Path::new("/tmp").is_dir() { "/tmp" } else { "/" });
        cmd.stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() });
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
        let narrow = match self.views.last() {
            Some(v) => Some(v.narrow_to(narrow, &self.explicit_insides())),
            None => narrow,
        };
        // With an explicit mount in the sandbox, every program is cut down to the default view, which leaves it out.
        let narrow = match narrow {
            None if self.sandboxed && self.mounts.has_explicit() => Some(self.default_view()),
            n => n,
        };
        let mut view_dir = None;
        if let (true, Some((sees, system))) = (self.sandboxed || self.views_ok, narrow) {
            crate::sandbox::narrow(&mut cmd, sees, system, self.explicit_pairs(), !self.sandboxed);
            view_dir = Some(!self.sandboxed);
        }
        let mut child = cmd.spawn().map_err(|e| {
            let why = match e.kind() {
                std::io::ErrorKind::NotFound => format!("`{program}` was not found; it must exist inside the sandbox (is `mount system` or a mount of its directory missing?)"),
                _ => format!("could not start `{program}`: {e}"),
            };
            throw("ExecError", why)
        })?;
        let writer = stdin.and_then(|text| {
            child.stdin.take().map(|mut si| {
                std::thread::spawn(move || {
                    let _ = si.write_all(text.as_bytes());
                })
            })
        });
        let child_pid = child.id();
        let out = child.wait_with_output().map_err(|e| throw("ExecError", format!("waiting for `{program}` failed: {e}")))?;
        if view_dir == Some(true) {
            // Without the script's sandbox the program's view was built under the host's /tmp; the folder stays behind.
            let _ = std::fs::remove_dir(format!("/tmp/.weir-see-{child_pid}"));
        }
        if let Some(w) = writer {
            let _ = w.join();
        }
        let stderr = String::from_utf8_lossy(&out.stderr);
        match out.status.code() {
            Some(c) if ok_codes.contains(&(c as i64)) => {
                if !stderr.trim().is_empty() {
                    eprint!("{stderr}");
                }
            }
            Some(c) => {
                let tail: Vec<&str> = stderr.trim_end().lines().rev().take(5).collect::<Vec<_>>().into_iter().rev().collect();
                let mut msg = format!("`{}` exited with code {c}", shown.trim_end());
                if !tail.is_empty() {
                    msg.push_str(&format!(": {}", tail.join(" | ")));
                }
                return Err(Box::new(Ctl::Throw(Throw { kind: "ExecError".into(), message: msg, code: Some(c as i64), span: None, path: None })));
            }
            None => {
                return Err(throw("ExecError", format!("`{}` was terminated by a signal", shown.trim_end())));
            }
        }
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        Ok(match yields {
            IoKind::None => Value::Absent,
            IoKind::Text => Value::str(text),
            IoKind::Lines => split_lines(&text),
            IoKind::Json => {
                let j = crate::json::parse(&text).map_err(|e| throw("ParseError", format!("`{program}` did not print valid JSON: {e}")))?;
                Value::from_json(&j)
            }
        })
    }

    // ---- stages -----------------------------------------------------------

    fn arg_expr<'a>(&self, args: &'a [Arg], name: &str, stage: &str) -> R<&'a Expr> {
        args.iter()
            .find(|a| a.name.as_deref() == Some(name))
            .map(|a| &a.expr)
            .ok_or_else(|| type_error(format!("stage `{stage}` is missing the argument `{name}`")))
    }

    fn per_item(&mut self, expr: &Expr, item: &Value, frame: &mut Frame) -> R<Value> {
        frame.scopes.push(HashMap::new());
        frame.bind("it", item.clone(), false);
        let r = self.eval(expr, frame);
        frame.scopes.pop();
        r
    }

    fn eval_stage(&mut self, recv: &Expr, name: &str, args: &[Arg], frame: &mut Frame) -> R<Value> {
        let input = self.eval(recv, frame)?;
        let native = matches!(name, "lines" | "map" | "filter" | "take" | "unique" | "count" | "count_by" | "sort_by");
        if native {
            for a in args {
                let ok = match name {
                    "map" => a.name.as_deref() == Some("to"),
                    "filter" => a.name.as_deref() == Some("where"),
                    "take" => a.name.as_deref() == Some("n"),
                    "unique" => a.name.as_deref() == Some("by"),
                    "count_by" => a.name.as_deref() == Some("key"),
                    "sort_by" => matches!(a.name.as_deref(), Some("key") | Some("order")),
                    _ => false,
                };
                if !ok {
                    return Err(type_error(format!("stage `{name}` has no argument `{}`", a.name.clone().unwrap_or_else(|| "...".into()))));
                }
            }
            if name == "lines" {
                return match input {
                    Value::Str(s) => Ok(split_lines(&s)),
                    l @ Value::List(_) => Ok(l),
                    v => Err(type_error(format!("`lines` needs text or a list, found {}", v.type_name()))),
                };
            }
            let items = match input {
                Value::List(l) => l,
                v => return Err(type_error(format!("stage `{name}` applies to a list, found {}", v.type_name()))),
            };
            return match name {
                "map" => {
                    let to = self.arg_expr(args, "to", name)?;
                    let mut out = Vec::with_capacity(items.len());
                    for it in items.iter() {
                        out.push(self.per_item(to, it, frame)?);
                    }
                    Ok(Value::list(out))
                }
                "filter" => {
                    let w = self.arg_expr(args, "where", name)?;
                    let mut out = Vec::new();
                    for it in items.iter() {
                        if truthy(&self.per_item(w, it, frame)?, "a `filter` condition")? {
                            out.push(it.clone());
                        }
                    }
                    Ok(Value::list(out))
                }
                "take" => {
                    let n = match self.eval(self.arg_expr(args, "n", name)?, frame)? {
                        Value::Int(n) if n >= 0 => n as usize,
                        v => return Err(type_error(format!("`take` needs a non-negative int, found {}", v.display()))),
                    };
                    Ok(Value::list(items.iter().take(n).cloned().collect()))
                }
                "unique" => {
                    let by = args.iter().find(|a| a.name.as_deref() == Some("by")).map(|a| &a.expr);
                    let mut seen: Vec<Value> = Vec::new();
                    let mut out = Vec::new();
                    for it in items.iter() {
                        let key = match by {
                            Some(b) => self.per_item(b, it, frame)?,
                            None => it.clone(),
                        };
                        if !seen.contains(&key) {
                            seen.push(key);
                            out.push(it.clone());
                        }
                    }
                    Ok(Value::list(out))
                }
                "count" => Ok(Value::Int(items.len() as i64)),
                "count_by" => {
                    let k = self.arg_expr(args, "key", name)?;
                    let mut keys: Vec<Value> = Vec::new();
                    let mut counts: Vec<i64> = Vec::new();
                    let mut index: HashMap<String, usize> = HashMap::new();
                    for it in items.iter() {
                        let key = self.per_item(k, it, frame)?;
                        let sig = format!("{}:{}", key.type_name(), key.display());
                        match index.get(&sig) {
                            Some(&i) => counts[i] += 1,
                            None => {
                                index.insert(sig, keys.len());
                                keys.push(key);
                                counts.push(1);
                            }
                        }
                    }
                    Ok(Value::list(
                        keys.into_iter().zip(counts).map(|(k, c)| Value::record(vec![("key", k), ("count", Value::Int(c))])).collect(),
                    ))
                }
                "sort_by" => {
                    let k = self.arg_expr(args, "key", name)?;
                    let desc = match args.iter().find(|a| a.name.as_deref() == Some("order")) {
                        Some(a) => match self.eval(&a.expr, frame)? {
                            Value::Str(s) if &*s == "asc" => false,
                            Value::Str(s) if &*s == "desc" => true,
                            v => return Err(type_error(format!("`order` must be `asc` or `desc`, found {}", v.display()))),
                        },
                        None => false,
                    };
                    let mut keyed: Vec<(Value, Value)> = Vec::with_capacity(items.len());
                    for it in items.iter() {
                        keyed.push((self.per_item(k, it, frame)?, it.clone()));
                    }
                    let mut err = None;
                    keyed.sort_by(|a, b| match a.0.compare(&b.0) {
                        Some(o) => {
                            if desc {
                                o.reverse()
                            } else {
                                o
                            }
                        }
                        None => {
                            err = Some(type_error(format!("`sort_by` cannot compare {} with {}", a.0.type_name(), b.0.type_name())));
                            std::cmp::Ordering::Equal
                        }
                    });
                    if let Some(e) = err {
                        return Err(e);
                    }
                    Ok(Value::list(keyed.into_iter().map(|(_, v)| v).collect()))
                }
                _ => unreachable!(),
            };
        }

        let unit = frame.unit;
        if let Some(f) = self.units[unit].tools.get(name).cloned() {
            let a = self.eval_named_args(args, frame)?;
            return self.call_tool(unit, &f, a, Some(input));
        }
        if builtins::is_builtin(name) {
            // A built-in applied to a value: the receiver is the first parameter.
            let mut a = self.eval_named_args(args, frame)?;
            let names: Vec<&str> = a.iter().map(|(k, _)| k.as_str()).collect();
            if let Some(sig) = builtins::find(name, &names) {
                if let Some((first, _)) = sig.params.first() {
                    a.insert(0, (first.to_string(), input));
                }
            }
            return self.call_builtin(name, a, recv.span);
        }
        let mut candidates: Vec<String> =
            ["lines", "map", "filter", "take", "unique", "count", "count_by", "sort_by"].iter().map(|s| s.to_string()).collect();
        candidates.extend(self.units[unit].tools.keys().cloned());
        candidates.sort();
        Err(type_error(format!("there is no stage `{name}`; the stages in scope are: {}", candidates.join(", "))))
    }
}

fn split_lines(text: &str) -> Value {
    let mut v: Vec<Value> = text.split('\n').map(|l| Value::str(l.strip_suffix('\r').unwrap_or(l))).collect();
    if matches!(v.last(), Some(Value::Str(s)) if s.is_empty()) {
        v.pop();
    }
    Value::list(v)
}

fn arg_text(v: &Value) -> R<String> {
    match v {
        Value::Str(s) | Value::Path(s) => Ok(s.to_string()),
        Value::Int(n) => Ok(n.to_string()),
        Value::Bool(b) => Ok(b.to_string()),
        Value::Dur(s) => Ok(s.to_string()),
        Value::Secret(_) => Err(throw("CapabilityError", "a secret cannot be placed in a program's argument list; pass it through `env` or stdin")),
        Value::Absent => Err(type_error("an absent value cannot be a program argument; unwrap it with `if let`")),
        v => Err(type_error(format!("a {} cannot be a program argument", v.type_name()))),
    }
}

fn shell_quote(a: &str) -> String {
    if !a.is_empty() && a.chars().all(|c| c.is_alphanumeric() || "-_./=:,@+%".contains(c)) {
        a.to_string()
    } else {
        format!("'{}'", a.replace('\'', "'\\''"))
    }
}

fn stamp_span(c: &mut Ctl, span: Span) {
    match c {
        Ctl::Throw(t) if t.span.is_none() => t.span = Some(span),
        Ctl::Fail { span: s, .. } if s.is_none() => *s = Some(span),
        _ => {}
    }
}

fn set_path(c: &mut Ctl, path: &str) {
    match c {
        Ctl::Throw(t) if t.path.is_none() => t.path = Some(path.to_string()),
        Ctl::Fail { path: p, .. } if p.is_none() => *p = Some(path.to_string()),
        _ => {}
    }
}

pub fn describe_ctl(c: &Ctl) -> String {
    match c {
        Ctl::Throw(t) => format!("{}: {}", t.kind, t.message),
        Ctl::Fail { message, .. } => message.clone(),
        Ctl::Return(_) => "return outside a fn".into(),
    }
}

/// Used by the test runner to read a flow's path out of an error.
pub fn ctl_location(c: &Ctl) -> (Option<Span>, Option<String>) {
    match c {
        Ctl::Throw(t) => (t.span, t.path.clone()),
        Ctl::Fail { span, path, .. } => (*span, path.clone()),
        Ctl::Return(_) => (None, None),
    }
}

impl Interp {
    /// Evaluate an expression with the given bindings in scope (sandbox mounts, fixtures).
    pub fn eval_with(&mut self, e: &Expr, binds: &[(String, Value)]) -> R<Value> {
        let mut frame = Frame::new(0);
        for (n, v) in binds {
            frame.bind(n, v.clone(), false);
        }
        self.eval(e, &mut frame)
    }

    /// Evaluate a test body statement in a fresh frame. Used by `weir test`.
    pub fn test_frame(&mut self, unit: usize) -> Frame {
        Frame::new(unit)
    }

    pub fn test_stmt(&mut self, s: &Stmt, frame: &mut Frame) -> R<()> {
        self.exec_stmt(s, frame)
    }

    pub fn test_eval(&mut self, e: &Expr, frame: &mut Frame) -> R<Value> {
        self.eval(e, frame)
    }
}
