//! Unit tests: lexer, parser, mount table, JSON and the interpreter's expression semantics.

use crate::ast::{Mode, Vis};
use crate::check::check_program;
use crate::interp::{Interp, Options, Unit};
use crate::json::{self, Json};
use crate::lexer::{lex, Tok};
use crate::loader::{Loaded, Program};
use crate::mounts::{normalize, Mounts};
use crate::parser::parse_file;
use crate::value::{fmt_duration, parse_duration, Value};
use std::rc::Rc;

fn toks(src: &str) -> Vec<Tok> {
    lex("t.weir", src).unwrap().into_iter().map(|t| t.tok).collect()
}

fn program(src: &str) -> Program {
    let file = parse_file("t.weir", src).unwrap_or_else(|d| panic!("{}", d.render()));
    Program { files: vec![Loaded { file, src: src.to_string(), imports: vec![] }], ..Default::default() }
}

/// Run the `main` flow of a script with no inputs, outside any sandbox, returning its outputs.
fn run(src: &str) -> Result<Vec<(String, Value)>, String> {
    let prog = program(src);
    let diags = check_program(&prog);
    if let Some(d) = diags.iter().find(|d| d.is_error()) {
        return Err(d.render());
    }
    let flow = prog.files[0].file.flows().find(|f| f.vis == Vis::Main).expect("a main flow");
    let mut m = Mounts::default();
    m.add("/", "/", Mode::Rw, "unconfined");
    let mut it = Interp::new(Options::default(), m, vec![Unit::from_file(&prog.files[0].file)]);
    let (outs, r) = it.run_entry(0, &Rc::new(flow.clone()), vec![]);
    match r {
        Ok(()) => Ok(outs),
        Err(c) => Err(crate::interp::describe_ctl(&c)),
    }
}

fn out(src: &str, name: &str) -> Value {
    run(src).unwrap().into_iter().find(|(n, _)| n == name).map(|(_, v)| v).unwrap()
}

fn int_flow(body: &str) -> String {
    format!("main flow t {{\n  out r: int\n  {body}\n}}\n")
}

#[test]
fn lexer_keeps_newline_after_closing_angle_bracket() {
    let t = toks("out x: list<int>\nlet y = 1\n");
    assert!(t.iter().filter(|t| **t == Tok::Newline).count() >= 2, "{t:?}");
}

#[test]
fn lexer_joins_dot_and_operator_continuations() {
    let joined = toks("let a = b\n  .map(to: it)\n");
    assert!(!joined[..joined.len() - 2].contains(&Tok::Newline));
    let joined = toks("a\n  or b\n");
    assert!(!joined[..joined.len() - 2].contains(&Tok::Newline));
}

#[test]
fn lexer_durations_and_integers() {
    assert!(matches!(&toks("30s")[0], Tok::Dur(30, _)));
    assert!(matches!(&toks("2h")[0], Tok::Dur(7200, _)));
    assert!(matches!(&toks("21d")[0], Tok::Dur(1_814_400, _)));
    // a unit letter that starts a longer word is not a duration
    assert!(matches!(&toks("5days")[0], Tok::Int(5)));
}

#[test]
fn lexer_rejects_bash_habits() {
    for (src, code) in [("a | b", "E-LEX"), ("'x'", "E-LEX"), ("\"abc", "E-LEX-STRING"), ("\"\\q\"", "E-LEX-ESCAPE")] {
        assert_eq!(lex("t", src).unwrap_err().code, code, "{src}");
    }
}

#[test]
fn parser_reports_stable_codes() {
    let code = |src: &str| parse_file("t.weir", src).unwrap_err().code;
    assert_eq!(code("main flow a {\n  print(\"x\")\n}"), "E-POSITIONAL-ARG");
    assert_eq!(code("fn f(x: int) { return x }"), "E-SYNTAX");
    assert_eq!(code("main flow a {\n  let in = 1\n}"), "E-RESERVED-WORD");
    assert_eq!(code("main flow a {\n  try {\n  }\n}"), "E-TRY-NO-CATCH");
    assert_eq!(code("tool f {\n  in x: string\n}"), "E-TOOL-PROGRAM");
    assert_eq!(code("print(text: \"x\")"), "E-SYNTAX");
}

#[test]
fn parser_precedence_follows_the_design() {
    // `not` binds tighter than comparison, comparison tighter than `and`
    assert_eq!(out(&int_flow("set r = if 1 + 2 * 3 == 7 and not false { 1 } else { 0 }"), "r"), Value::Int(1));
    assert_eq!(out(&int_flow("set r = 7 - 2 - 1"), "r"), Value::Int(4));
    assert_eq!(out(&int_flow("set r = -2 * 3"), "r"), Value::Int(-6));
}

#[test]
fn json_round_trip() {
    let j = json::parse(r#"{"a": [1, 2, {"b": "x\ny"}], "c": null, "d": true, "e": 1.5}"#).unwrap();
    assert_eq!(json::parse(&j.encode()).unwrap(), j);
    assert!(matches!(j, Json::Obj(_)));
    assert!(json::parse("{").is_err());
}

#[test]
fn mounts_most_specific_wins() {
    let mut m = Mounts::default();
    m.add("/etc/nginx", "/etc/nginx", Mode::Ro, "declared");
    m.add("/etc/nginx/conf.d", "/etc/nginx/conf.d", Mode::Rw, "declared");
    let (e, host) = m.find("/etc/nginx/conf.d/deny.conf").unwrap();
    assert_eq!((e.mode, host.as_str()), (Mode::Rw, "/etc/nginx/conf.d/deny.conf"));
    assert_eq!(m.find("/etc/nginx/nginx.conf").unwrap().0.mode, Mode::Ro);
    assert!(m.find("/etc/passwd").is_none());
    // a sibling that only shares a name prefix is not under the mount
    assert!(m.find("/etc/nginx-old/x").is_none());
}

#[test]
fn mounts_translate_host_paths_and_normalise() {
    let mut m = Mounts::default();
    m.add("/srv/logs", "/logs", Mode::Ro, "declared");
    assert_eq!(m.host_to_inside("/srv/logs/a/../access.log").unwrap(), Some("/logs/access.log".into()));
    assert_eq!(m.host_to_inside("/srv/other").unwrap(), None);
    assert_eq!(normalize("/a//b/./c/../d"), "/a/b/d");
    assert_eq!(normalize("/.."), "/");
}

#[test]
fn durations_format_and_parse() {
    assert_eq!(fmt_duration(86400), "1d");
    assert_eq!(fmt_duration(7200), "2h");
    assert_eq!(fmt_duration(90), "90s");
    assert_eq!(parse_duration("15m"), Some(900));
    assert_eq!(parse_duration("x"), None);
}

#[test]
fn integer_division_and_path_join() {
    assert_eq!(out(&int_flow("set r = 7 / 2"), "r"), Value::Int(3));
    let src = "main flow t {\n  out r: path\n  set r = path(text: \"/a/b/\") / \"c.log\"\n}\n";
    assert_eq!(out(src, "r"), Value::path("/a/b/c.log"));
}

#[test]
fn pipelines_over_lists() {
    let src = r#"main flow t {
  out r: list<string>
  set r = ["b", "a", "b", "c", "a", "b"]
    .count_by(key: it)
    .sort_by(key: it.count, order: desc)
    .take(n: 2)
    .map(to: "${it.key}=${it.count}")
}"#;
    assert_eq!(out(src, "r"), Value::list(vec![Value::str("b=3"), Value::str("a=2")]));
}

#[test]
fn immutable_let_is_a_check_error_and_var_works() {
    let err = run(&int_flow("let a = 1\n  a = 2\n  set r = a")).unwrap_err();
    assert!(err.contains("E-ASSIGN-IMMUTABLE"), "{err}");
    assert_eq!(out(&int_flow("var a = 1\n  a = a + 1\n  set r = a"), "r"), Value::Int(2));
}

#[test]
fn optional_must_be_unwrapped() {
    let err = run("main flow t {\n  in x: string?\n  print(text: x)\n}").unwrap_err();
    assert!(err.contains("E-OPTIONAL-UNWRAPPED"), "{err}");
}

#[test]
fn try_returns_an_attempt_record_and_catch_matches_by_type() {
    let src = r#"tool boom {
  program "false"
  yields none
}
main flow t {
  out r: string
  let a = try boom()
  var kind = "none"
  try {
    boom()
  } catch e: ExecError {
    kind = e.kind
  }
  set r = "${a.ok}/${a.code}/${kind}"
}"#;
    // `false` exits 1; the checker warns about the missing sandbox but it is not an error
    assert_eq!(out(src, "r"), Value::str("false/1/ExecError"));
}

#[test]
fn fail_ends_the_flow_and_finally_still_runs() {
    let src = r#"main flow t {
  out r: int
  set r = 1
  fail message: "boom"
  finally {
    set r = 2
  }
}"#;
    let prog = program(src);
    let flow = prog.files[0].file.flows().next().unwrap();
    let mut m = Mounts::default();
    m.add("/", "/", Mode::Rw, "unconfined");
    let mut it = Interp::new(Options::default(), m, vec![Unit::from_file(&prog.files[0].file)]);
    let (outs, r) = it.run_entry(0, &Rc::new(flow.clone()), vec![]);
    assert_eq!(crate::interp::describe_ctl(&r.unwrap_err()), "boom");
    // an output assigned before the failure is reported, and `finally` could still assign
    assert_eq!(outs[0].1, Value::Int(2));
}

#[test]
fn confirmable_steps_are_skipped_without_confirm() {
    let src = r#"main flow t {
  out r: int
  var n = 0
  once {
    n = n + 1
  } confirmable
  set r = n
}"#;
    assert_eq!(out(src, "r"), Value::Int(0));
}

#[test]
fn builtin_errors_are_typed() {
    let src = r#"main flow t {
  out r: string
  let a = try int(text: "abc")
  let b = try nth(of: ["x"], index: 4)
  set r = "${a.ok} ${b.ok}"
}"#;
    assert_eq!(out(src, "r"), Value::str("false false"));
}

#[test]
fn if_let_unwraps_optionals() {
    let src = r#"main flow t {
  in x: string? = absent
  out r: string
  if let v = x {
    set r = v
  } else {
    set r = "none"
  }
}"#;
    assert_eq!(out(src, "r"), Value::str("none"));
}

#[test]
fn a_secret_cannot_be_interpolated() {
    let err = run("main flow t {\n  in tok: secret\n  print(text: \"${tok}\")\n}").unwrap_err();
    assert!(err.contains("E-SECRET-LEAK"), "{err}");
}

#[test]
fn checker_flags_cascade_free_unknown_names() {
    let err = run("main flow t {\n  let a = nope(x: 1)\n}").unwrap_err();
    assert!(err.contains("E-UNKNOWN-CALL"), "{err}");
}
