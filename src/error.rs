//! Runtime control flow: thrown errors, `fail`, and `return`.

use crate::ast::Span;
use crate::value::Value;

/// Error kinds a script can name in `catch`.
pub const CATCHABLE: &[&str] = &["ExecError", "CapabilityError", "IoError", "ParseError", "Timeout"];

#[derive(Debug, Clone)]
pub struct Throw {
    pub kind: String,
    pub message: String,
    /// Exit status for `ExecError`.
    pub code: Option<i64>,
    pub span: Option<Span>,
    /// The step path (`deploy > publish`) where the error first surfaced.
    pub path: Option<String>,
}

#[derive(Debug, Clone)]
pub enum Ctl {
    Throw(Throw),
    /// `fail message: ...`
    Fail { message: String, span: Option<Span>, path: Option<String> },
    Return(Value),
}

pub type R<T> = Result<T, Box<Ctl>>;

pub fn throw(kind: &str, message: impl Into<String>) -> Box<Ctl> {
    Box::new(Ctl::Throw(Throw { kind: kind.into(), message: message.into(), code: None, span: None, path: None }))
}

/// A script bug the checker should have caught (wrong type, unknown name).
pub fn type_error(message: impl Into<String>) -> Box<Ctl> {
    throw("TypeError", message)
}
