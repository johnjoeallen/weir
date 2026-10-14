//! Built-in functions: their signatures (shared by the checker and interpreter)
//! and the pure implementations. Effectful ones live in the interpreter.

use crate::error::{throw, type_error, R};
use crate::value::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effect {
    /// No effect and no dependence on the outside world: allowed in a `fn`.
    Pure,
    Clock,
    Read,
    Write,
    Print,
    Exec,
}

pub struct Sig {
    pub name: &'static str,
    /// (parameter, required)
    pub params: &'static [(&'static str, bool)],
    pub effect: Effect,
}

const fn s(name: &'static str, params: &'static [(&'static str, bool)], effect: Effect) -> Sig {
    Sig { name, params, effect }
}

use Effect::*;

pub static SIGS: &[Sig] = &[
    s("print", &[("text", true)], Print),
    s("split", &[("text", true), ("on", true)], Pure),
    s("join", &[("items", true), ("with", true)], Pure),
    s("first", &[("of", true)], Pure),
    s("last", &[("of", true)], Pure),
    s("nth", &[("of", true), ("index", true)], Pure),
    s("count", &[("of", true)], Pure),
    s("contains", &[("items", true), ("item", true)], Pure),
    s("contains", &[("text", true), ("part", true)], Pure),
    s("starts_with", &[("text", true), ("prefix", true)], Pure),
    s("ends_with", &[("text", true), ("suffix", true)], Pure),
    s("trim", &[("text", true)], Pure),
    s("lower", &[("text", true)], Pure),
    s("upper", &[("text", true)], Pure),
    s("replace", &[("text", true), ("from", true), ("to", true)], Pure),
    s("int", &[("text", true)], Pure),
    s("string", &[("value", true)], Pure),
    s("path", &[("text", true)], Pure),
    s("duration", &[("seconds", true)], Pure),
    s("extension", &[("of", true)], Pure),
    s("file_name", &[("of", true)], Pure),
    s("parent", &[("of", true)], Pure),
    s("with_suffix", &[("path", true), ("text", true)], Pure),
    s("now_epoch", &[], Clock),
    s("exists", &[("path", true)], Read),
    s("read_text", &[("file", true)], Read),
    s("read_lines", &[("file", true)], Read),
    s("write", &[("file", true), ("text", true)], Write),
    s("write_lines", &[("file", true), ("lines", true)], Write),
    s("append_lines", &[("file", true), ("lines", true)], Write),
    s("copy", &[("from", true), ("to", true)], Write),
    s("move", &[("from", true), ("to", true)], Write),
    s("delete", &[("path", true)], Write),
    s("run", &[("cmd", true), ("args", false), ("ok_codes", false)], Exec),
];

pub fn is_builtin(name: &str) -> bool {
    SIGS.iter().any(|s| s.name == name)
}

pub fn names() -> Vec<&'static str> {
    let mut v: Vec<&'static str> = SIGS.iter().map(|s| s.name).collect();
    v.dedup();
    v
}

/// Pick the overload whose parameters match the given argument names.
pub fn find(name: &str, arg_names: &[&str]) -> Option<&'static Sig> {
    let all: Vec<&Sig> = SIGS.iter().filter(|s| s.name == name).collect();
    if all.len() <= 1 {
        return all.into_iter().next();
    }
    all.iter()
        .find(|s| arg_names.iter().all(|a| s.params.iter().any(|(p, _)| p == a)))
        .copied()
        .or_else(|| all.first().copied())
}

pub struct Args(pub Vec<(String, Value)>);

impl Args {
    pub fn get(&self, name: &str) -> Option<&Value> {
        self.0.iter().find(|(k, _)| k == name).map(|(_, v)| v)
    }

    fn req(&self, name: &str) -> R<&Value> {
        self.get(name).ok_or_else(|| type_error(format!("missing argument `{name}`")))
    }

    pub fn text(&self, name: &str) -> R<String> {
        match self.req(name)? {
            Value::Str(s) | Value::Path(s) => Ok(s.to_string()),
            Value::Secret(_) => Err(throw("CapabilityError", format!("argument `{name}` is a secret and cannot be used as text"))),
            v => Err(type_error(format!("argument `{name}` must be a string, found {}", v.type_name()))),
        }
    }

    pub fn int(&self, name: &str) -> R<i64> {
        match self.req(name)? {
            Value::Int(n) => Ok(*n),
            v => Err(type_error(format!("argument `{name}` must be an int, found {}", v.type_name()))),
        }
    }

    pub fn list(&self, name: &str) -> R<std::rc::Rc<Vec<Value>>> {
        match self.req(name)? {
            Value::List(l) => Ok(l.clone()),
            v => Err(type_error(format!("argument `{name}` must be a list, found {}", v.type_name()))),
        }
    }

    pub fn strings(&self, name: &str) -> R<Vec<String>> {
        self.list(name)?
            .iter()
            .map(|v| match v {
                Value::Str(s) | Value::Path(s) => Ok(s.to_string()),
                v => Err(type_error(format!("argument `{name}` must be a list of strings, found a {} element", v.type_name()))),
            })
            .collect()
    }
}

/// Implementations that need no interpreter state.
pub fn call_pure(name: &str, a: &Args) -> Option<R<Value>> {
    Some(match name {
        "split" => (|| {
            let (t, on) = (a.text("text")?, a.text("on")?);
            if on.is_empty() {
                return Err(type_error("`split` needs a non-empty `on`"));
            }
            Ok(Value::list(t.split(on.as_str()).map(Value::str).collect()))
        })(),
        "join" => (|| {
            let items = a.list("items")?;
            let with = a.text("with")?;
            let mut parts = Vec::with_capacity(items.len());
            for v in items.iter() {
                parts.push(match v {
                    Value::Secret(_) => return Err(throw("CapabilityError", "a secret cannot be joined into text")),
                    v => v.display(),
                });
            }
            Ok(Value::str(parts.join(&with)))
        })(),
        "first" => (|| {
            let l = a.list("of")?;
            l.first().cloned().ok_or_else(|| throw("ParseError", "`first` of an empty list"))
        })(),
        "last" => (|| {
            let l = a.list("of")?;
            l.last().cloned().ok_or_else(|| throw("ParseError", "`last` of an empty list"))
        })(),
        "nth" => (|| {
            let l = a.list("of")?;
            let i = a.int("index")?;
            usize::try_from(i)
                .ok()
                .and_then(|i| l.get(i).cloned())
                .ok_or_else(|| throw("ParseError", format!("index {i} is out of range for a list of {} element(s)", l.len())))
        })(),
        "count" => (|| match a.get("of") {
            Some(Value::List(l)) => Ok(Value::Int(l.len() as i64)),
            Some(Value::Str(s)) => Ok(Value::Int(s.chars().count() as i64)),
            Some(v) => Err(type_error(format!("`count` needs a list or string, found {}", v.type_name()))),
            None => Err(type_error("missing argument `of`")),
        })(),
        "contains" => (|| {
            if a.get("items").is_some() {
                let items = a.list("items")?;
                let item = a.get("item").ok_or_else(|| type_error("missing argument `item`"))?;
                Ok(Value::Bool(items.iter().any(|v| v == item)))
            } else {
                Ok(Value::Bool(a.text("text")?.contains(a.text("part")?.as_str())))
            }
        })(),
        "starts_with" => (|| Ok(Value::Bool(a.text("text")?.starts_with(a.text("prefix")?.as_str()))))(),
        "ends_with" => (|| Ok(Value::Bool(a.text("text")?.ends_with(a.text("suffix")?.as_str()))))(),
        "trim" => (|| Ok(Value::str(a.text("text")?.trim())))(),
        "lower" => (|| Ok(Value::str(a.text("text")?.to_lowercase())))(),
        "upper" => (|| Ok(Value::str(a.text("text")?.to_uppercase())))(),
        "replace" => (|| {
            let from = a.text("from")?;
            if from.is_empty() {
                return Err(type_error("`replace` needs a non-empty `from`"));
            }
            Ok(Value::str(a.text("text")?.replace(from.as_str(), &a.text("to")?)))
        })(),
        "int" => (|| match a.get("text") {
            Some(Value::Int(n)) => Ok(Value::Int(*n)),
            _ => {
                let t = a.text("text")?;
                t.trim()
                    .parse::<i64>()
                    .map(Value::Int)
                    .map_err(|_| throw("ParseError", format!("cannot read {t:?} as an int")))
            }
        })(),
        "string" => (|| match a.get("value") {
            Some(Value::Secret(_)) => Err(throw("CapabilityError", "a secret cannot be converted to a string")),
            Some(v @ (Value::Str(_) | Value::Path(_) | Value::Int(_) | Value::Bool(_) | Value::Dur(_))) => Ok(Value::str(v.display())),
            Some(v) => Err(type_error(format!("`string` cannot convert a {}", v.type_name()))),
            None => Err(type_error("missing argument `value`")),
        })(),
        "path" => (|| Ok(Value::path(a.text("text")?)))(),
        "duration" => (|| Ok(Value::Dur(a.int("seconds")?)))(),
        "extension" => (|| {
            let t = a.text("of")?;
            let name = t.rsplit('/').next().unwrap_or("");
            Ok(Value::str(match name.rfind('.') {
                Some(i) if i > 0 => &name[i + 1..],
                _ => "",
            }))
        })(),
        "file_name" => (|| Ok(Value::str(a.text("of")?.trim_end_matches('/').rsplit('/').next().unwrap_or(""))))(),
        "parent" => (|| {
            let t = a.text("of")?;
            let t = t.trim_end_matches('/');
            Ok(Value::path(match t.rfind('/') {
                Some(0) => "/",
                Some(i) => &t[..i],
                None => ".",
            }))
        })(),
        "with_suffix" => (|| Ok(Value::path(format!("{}{}", a.text("path")?, a.text("text")?))))(),
        _ => return None,
    })
}
