//! Runtime values.

use crate::json::Json;
use std::cmp::Ordering;
use std::rc::Rc;

#[derive(Debug, Clone)]
pub enum Value {
    Absent,
    Bool(bool),
    Int(i64),
    Str(Rc<str>),
    Path(Rc<str>),
    /// Seconds.
    Dur(i64),
    List(Rc<Vec<Value>>),
    Record(Rc<Vec<(String, Value)>>),
    /// Never printed, interpolated or written.
    Secret(Rc<str>),
}

impl Value {
    pub fn str(s: impl AsRef<str>) -> Value {
        Value::Str(Rc::from(s.as_ref()))
    }

    pub fn path(s: impl AsRef<str>) -> Value {
        Value::Path(Rc::from(s.as_ref()))
    }

    pub fn list(v: Vec<Value>) -> Value {
        Value::List(Rc::new(v))
    }

    pub fn record(v: Vec<(&str, Value)>) -> Value {
        Value::Record(Rc::new(v.into_iter().map(|(k, v)| (k.to_string(), v)).collect()))
    }

    pub fn type_name(&self) -> &'static str {
        match self {
            Value::Absent => "absent",
            Value::Bool(_) => "bool",
            Value::Int(_) => "int",
            Value::Str(_) => "string",
            Value::Path(_) => "path",
            Value::Dur(_) => "duration",
            Value::List(_) => "list",
            Value::Record(_) => "record",
            Value::Secret(_) => "secret",
        }
    }

    #[allow(dead_code)]
    pub fn field(&self, name: &str) -> Option<Value> {
        match self {
            Value::Record(r) => r.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone()),
            _ => None,
        }
    }

    /// Text for `print` and `${}`. Secrets are refused by the caller; here they are redacted.
    pub fn display(&self) -> String {
        match self {
            Value::Absent => "absent".into(),
            Value::Bool(b) => b.to_string(),
            Value::Int(n) => n.to_string(),
            Value::Str(s) | Value::Path(s) => s.to_string(),
            Value::Dur(s) => fmt_duration(*s),
            Value::List(l) => format!("[{}]", l.iter().map(|v| v.display_quoted()).collect::<Vec<_>>().join(", ")),
            Value::Record(r) => format!(
                "{{{}}}",
                r.iter().map(|(k, v)| format!("{k}: {}", v.display_quoted())).collect::<Vec<_>>().join(", ")
            ),
            Value::Secret(_) => "<secret>".into(),
        }
    }

    fn display_quoted(&self) -> String {
        match self {
            Value::Str(s) | Value::Path(s) => format!("{:?}", s),
            v => v.display(),
        }
    }

    pub fn to_json(&self) -> Json {
        match self {
            Value::Absent => Json::Null,
            Value::Bool(b) => Json::Bool(*b),
            Value::Int(n) => Json::Int(*n),
            Value::Str(s) | Value::Path(s) => Json::Str(s.to_string()),
            Value::Dur(s) => Json::Int(*s),
            Value::List(l) => Json::Arr(l.iter().map(Value::to_json).collect()),
            Value::Record(r) => Json::Obj(r.iter().map(|(k, v)| (k.clone(), v.to_json())).collect()),
            Value::Secret(_) => Json::Str("<secret>".into()),
        }
    }

    pub fn from_json(j: &Json) -> Value {
        match j {
            Json::Null => Value::Absent,
            Json::Bool(b) => Value::Bool(*b),
            Json::Int(n) => Value::Int(*n),
            Json::Str(s) => Value::str(s),
            Json::Arr(a) => Value::list(a.iter().map(Value::from_json).collect()),
            Json::Obj(o) => Value::Record(Rc::new(o.iter().map(|(k, v)| (k.clone(), Value::from_json(v))).collect())),
        }
    }

    /// Ordering for `sort_by` and comparisons. `None` when the two cannot be compared.
    pub fn compare(&self, other: &Value) -> Option<Ordering> {
        match (self, other) {
            (Value::Int(a), Value::Int(b)) => Some(a.cmp(b)),
            (Value::Dur(a), Value::Dur(b)) => Some(a.cmp(b)),
            (Value::Str(a), Value::Str(b)) | (Value::Path(a), Value::Path(b)) => Some(a.cmp(b)),
            (Value::Bool(a), Value::Bool(b)) => Some(a.cmp(b)),
            _ => None,
        }
    }
}

impl PartialEq for Value {
    fn eq(&self, other: &Value) -> bool {
        match (self, other) {
            (Value::Absent, Value::Absent) => true,
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::Int(a), Value::Int(b)) => a == b,
            (Value::Str(a), Value::Str(b)) => a == b,
            (Value::Path(a), Value::Path(b)) => a == b,
            (Value::Dur(a), Value::Dur(b)) => a == b,
            (Value::List(a), Value::List(b)) => a == b,
            (Value::Record(a), Value::Record(b)) => a == b,
            _ => false,
        }
    }
}

/// `90s` -> `90s`, `3600s` -> `1h`: the largest unit that divides evenly.
pub fn fmt_duration(secs: i64) -> String {
    if secs != 0 {
        for (unit, size) in [('d', 86400), ('h', 3600), ('m', 60)] {
            if secs % size == 0 {
                return format!("{}{unit}", secs / size);
            }
        }
    }
    format!("{secs}s")
}

/// Parses `30s`, `15m`, `2h`, `21d`.
pub fn parse_duration(s: &str) -> Option<i64> {
    let (num, unit) = s.split_at(s.len().checked_sub(1)?);
    let n: i64 = num.parse().ok()?;
    let mult = match unit {
        "s" => 1,
        "m" => 60,
        "h" => 3600,
        "d" => 86400,
        _ => return None,
    };
    n.checked_mul(mult)
}
