//! Findings produced by the lexer, parser and checker.

use crate::json::Json;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Debug, Clone)]
pub struct Diag {
    pub file: String,
    pub line: u32,
    pub col: u32,
    pub code: String,
    pub msg: String,
    pub fix: Option<String>,
    pub severity: Severity,
}

impl Diag {
    pub fn error(file: &str, line: u32, col: u32, code: &str, msg: String, fix: Option<String>) -> Diag {
        Diag { file: file.into(), line, col, code: code.into(), msg, fix, severity: Severity::Error }
    }

    pub fn warning(file: &str, line: u32, col: u32, code: &str, msg: String, fix: Option<String>) -> Diag {
        Diag { file: file.into(), line, col, code: code.into(), msg, fix, severity: Severity::Warning }
    }

    pub fn is_error(&self) -> bool {
        self.severity == Severity::Error
    }

    /// `file:line:col: code: message`, then the fix hint on its own line.
    pub fn render(&self) -> String {
        let mut s = format!("{}:{}:{}: {}: {}", self.file, self.line, self.col, self.code, self.msg);
        if let Some(f) = &self.fix {
            s.push_str(&format!("\n    fix: {f}"));
        }
        s
    }

    pub fn to_json(&self) -> Json {
        let mut o = vec![
            ("file".to_string(), Json::Str(self.file.clone())),
            ("line".to_string(), Json::Int(self.line as i64)),
            ("col".to_string(), Json::Int(self.col as i64)),
            ("code".to_string(), Json::Str(self.code.clone())),
            ("msg".to_string(), Json::Str(self.msg.clone())),
            (
                "severity".to_string(),
                Json::Str(if self.is_error() { "error" } else { "warning" }.into()),
            ),
        ];
        if let Some(f) = &self.fix {
            o.push(("fix".to_string(), Json::Str(f.clone())));
        }
        Json::Obj(o)
    }
}
