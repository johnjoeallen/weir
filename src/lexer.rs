//! Lexer. Produces tokens with 1-based line/column, then drops newlines that
//! continue an expression (inside `(`/`[`, after an operator or comma, or before
//! a line that starts with `.name`, `and` or `or`).

use crate::diag::Diag;

#[derive(Debug, Clone, PartialEq)]
pub enum StrPart {
    Lit(String),
    /// Source text of a `${...}` interpolation and its position in the file.
    Expr(String, u32, u32),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Tok {
    Ident(String),
    Int(i64),
    /// Seconds, and the literal as written.
    Dur(i64, String),
    Str(Vec<StrPart>),
    LBrace,
    RBrace,
    LParen,
    RParen,
    LBrack,
    RBrack,
    Comma,
    Colon,
    Dot,
    Ellipsis,
    Dollar,
    Eq,
    EqEq,
    NotEq,
    Lt,
    Le,
    Gt,
    Ge,
    Plus,
    Minus,
    Star,
    Slash,
    Question,
    Arrow,
    Newline,
    Eof,
}

impl Tok {
    pub fn describe(&self) -> String {
        match self {
            Tok::Ident(s) => format!("`{s}`"),
            Tok::Int(n) => format!("`{n}`"),
            Tok::Dur(_, s) => format!("`{s}`"),
            Tok::Str(_) => "a string".into(),
            Tok::LBrace => "`{`".into(),
            Tok::RBrace => "`}`".into(),
            Tok::LParen => "`(`".into(),
            Tok::RParen => "`)`".into(),
            Tok::LBrack => "`[`".into(),
            Tok::RBrack => "`]`".into(),
            Tok::Comma => "`,`".into(),
            Tok::Colon => "`:`".into(),
            Tok::Dot => "`.`".into(),
            Tok::Ellipsis => "`...`".into(),
            Tok::Dollar => "`$`".into(),
            Tok::Eq => "`=`".into(),
            Tok::EqEq => "`==`".into(),
            Tok::NotEq => "`!=`".into(),
            Tok::Lt => "`<`".into(),
            Tok::Le => "`<=`".into(),
            Tok::Gt => "`>`".into(),
            Tok::Ge => "`>=`".into(),
            Tok::Plus => "`+`".into(),
            Tok::Minus => "`-`".into(),
            Tok::Star => "`*`".into(),
            Tok::Slash => "`/`".into(),
            Tok::Question => "`?`".into(),
            Tok::Arrow => "`->`".into(),
            Tok::Newline => "end of line".into(),
            Tok::Eof => "end of file".into(),
        }
    }

    /// True if a newline after this token cannot end a statement. `<` and `>` are left
    /// out on purpose: a line may end in the `>` that closes a type such as `list<int>`.
    fn continues(&self) -> bool {
        match self {
            Tok::Comma
            | Tok::LParen
            | Tok::LBrack
            | Tok::Dot
            | Tok::Eq
            | Tok::EqEq
            | Tok::NotEq
            | Tok::Le
            | Tok::Ge
            | Tok::Plus
            | Tok::Minus
            | Tok::Star
            | Tok::Slash
            | Tok::Arrow
            | Tok::Colon => true,
            Tok::Ident(s) => matches!(s.as_str(), "and" | "or" | "not"),
            _ => false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Token {
    pub tok: Tok,
    pub line: u32,
    pub col: u32,
}

pub fn lex(file: &str, src: &str) -> Result<Vec<Token>, Diag> {
    Lexer::new(file, src).run()
}

struct Lexer<'a> {
    file: &'a str,
    chars: Vec<char>,
    i: usize,
    line: u32,
    col: u32,
    out: Vec<Token>,
}

impl<'a> Lexer<'a> {
    fn new(file: &'a str, src: &str) -> Self {
        Lexer { file, chars: src.chars().collect(), i: 0, line: 1, col: 1, out: Vec::new() }
    }

    fn err(&self, line: u32, col: u32, code: &str, msg: String, fix: Option<&str>) -> Diag {
        Diag::error(self.file, line, col, code, msg, fix.map(String::from))
    }

    fn peek(&self, k: usize) -> Option<char> {
        self.chars.get(self.i + k).copied()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.chars.get(self.i).copied()?;
        self.i += 1;
        if c == '\n' {
            self.line += 1;
            self.col = 1;
        } else {
            self.col += 1;
        }
        Some(c)
    }

    fn push(&mut self, tok: Tok, line: u32, col: u32) {
        self.out.push(Token { tok, line, col });
    }

    fn run(mut self) -> Result<Vec<Token>, Diag> {
        while let Some(c) = self.peek(0) {
            let (line, col) = (self.line, self.col);
            match c {
                ' ' | '\t' | '\r' => {
                    self.bump();
                }
                '\n' => {
                    self.bump();
                    self.push(Tok::Newline, line, col);
                }
                '#' if self.i == 0 && self.peek(1) == Some('!') => {
                    while self.peek(0).is_some_and(|c| c != '\n') {
                        self.bump();
                    }
                }
                '/' if self.peek(1) == Some('/') => {
                    while self.peek(0).is_some_and(|c| c != '\n') {
                        self.bump();
                    }
                }
                '"' => {
                    let parts = self.string()?;
                    self.push(Tok::Str(parts), line, col);
                }
                '0'..='9' => self.number(line, col)?,
                c if c.is_alphabetic() || c == '_' => {
                    let mut s = String::new();
                    while let Some(c) = self.peek(0) {
                        if c.is_alphanumeric() || c == '_' {
                            s.push(c);
                            self.bump();
                        } else {
                            break;
                        }
                    }
                    self.push(Tok::Ident(s), line, col);
                }
                '{' => self.single(Tok::LBrace, line, col),
                '}' => self.single(Tok::RBrace, line, col),
                '(' => self.single(Tok::LParen, line, col),
                ')' => self.single(Tok::RParen, line, col),
                '[' => self.single(Tok::LBrack, line, col),
                ']' => self.single(Tok::RBrack, line, col),
                ',' => self.single(Tok::Comma, line, col),
                ':' => self.single(Tok::Colon, line, col),
                '$' => self.single(Tok::Dollar, line, col),
                '+' => self.single(Tok::Plus, line, col),
                '*' => self.single(Tok::Star, line, col),
                '/' => self.single(Tok::Slash, line, col),
                '?' => self.single(Tok::Question, line, col),
                '.' => {
                    if self.peek(1) == Some('.') && self.peek(2) == Some('.') {
                        self.bump();
                        self.bump();
                        self.bump();
                        self.push(Tok::Ellipsis, line, col);
                    } else {
                        self.single(Tok::Dot, line, col);
                    }
                }
                '-' => {
                    self.bump();
                    if self.peek(0) == Some('>') {
                        self.bump();
                        self.push(Tok::Arrow, line, col);
                    } else {
                        self.push(Tok::Minus, line, col);
                    }
                }
                '=' => {
                    self.bump();
                    if self.peek(0) == Some('=') {
                        self.bump();
                        self.push(Tok::EqEq, line, col);
                    } else {
                        self.push(Tok::Eq, line, col);
                    }
                }
                '!' => {
                    self.bump();
                    if self.peek(0) == Some('=') {
                        self.bump();
                        self.push(Tok::NotEq, line, col);
                    } else {
                        return Err(self.err(
                            line,
                            col,
                            "E-LEX",
                            "`!` is not an operator".into(),
                            Some("use `not` for negation and `!=` for inequality"),
                        ));
                    }
                }
                '<' => {
                    self.bump();
                    if self.peek(0) == Some('=') {
                        self.bump();
                        self.push(Tok::Le, line, col);
                    } else {
                        self.push(Tok::Lt, line, col);
                    }
                }
                '>' => {
                    self.bump();
                    if self.peek(0) == Some('=') {
                        self.bump();
                        self.push(Tok::Ge, line, col);
                    } else {
                        self.push(Tok::Gt, line, col);
                    }
                }
                '\'' => {
                    return Err(self.err(
                        line,
                        col,
                        "E-LEX",
                        "single-quoted strings do not exist".into(),
                        Some("use double quotes: \"text\""),
                    ))
                }
                '|' | '&' | ';' | '`' => {
                    return Err(self.err(
                        line,
                        col,
                        "E-LEX",
                        format!("`{c}` is not part of Weir syntax"),
                        Some("chain stages with `.`, separate statements with newlines, and use `and`/`or`"),
                    ))
                }
                other => {
                    return Err(self.err(line, col, "E-LEX", format!("unexpected character `{other}`"), None))
                }
            }
        }
        let (line, col) = (self.line, self.col);
        self.push(Tok::Newline, line, col);
        self.push(Tok::Eof, line, col);
        Ok(fold_newlines(self.out))
    }

    fn single(&mut self, tok: Tok, line: u32, col: u32) {
        self.bump();
        self.push(tok, line, col);
    }

    fn number(&mut self, line: u32, col: u32) -> Result<(), Diag> {
        let mut digits = String::new();
        while let Some(c) = self.peek(0) {
            if c.is_ascii_digit() {
                digits.push(c);
                self.bump();
            } else {
                break;
            }
        }
        let n: i64 = digits.parse().map_err(|_| {
            self.err(line, col, "E-LEX", format!("integer `{digits}` does not fit in 64 bits"), None)
        })?;
        // Duration suffix, only when it is not the start of a longer identifier.
        if let Some(u @ ('s' | 'm' | 'h' | 'd')) = self.peek(0) {
            let next = self.peek(1);
            if !next.is_some_and(|c| c.is_alphanumeric() || c == '_') {
                self.bump();
                let mult = match u {
                    's' => 1,
                    'm' => 60,
                    'h' => 3600,
                    _ => 86400,
                };
                self.push(Tok::Dur(n * mult, format!("{n}{u}")), line, col);
                return Ok(());
            }
        }
        self.push(Tok::Int(n), line, col);
        Ok(())
    }

    fn string(&mut self) -> Result<Vec<StrPart>, Diag> {
        let (sl, sc) = (self.line, self.col);
        self.bump(); // opening quote
        let mut parts = Vec::new();
        let mut lit = String::new();
        loop {
            let (line, col) = (self.line, self.col);
            match self.bump() {
                None | Some('\n') => {
                    return Err(self.err(
                        sl,
                        sc,
                        "E-LEX-STRING",
                        "unterminated string".into(),
                        Some("close the string with a double quote on the same line; use \\n for a newline"),
                    ))
                }
                Some('"') => break,
                Some('\\') => match self.bump() {
                    Some('n') => lit.push('\n'),
                    Some('t') => lit.push('\t'),
                    Some('r') => lit.push('\r'),
                    Some('0') => lit.push('\0'),
                    Some('\\') => lit.push('\\'),
                    Some('"') => lit.push('"'),
                    Some('$') => lit.push('$'),
                    other => {
                        return Err(self.err(
                            line,
                            col,
                            "E-LEX-ESCAPE",
                            format!("unknown escape `\\{}`", other.map(String::from).unwrap_or_default()),
                            Some("valid escapes are \\n \\t \\r \\0 \\\\ \\\" \\$"),
                        ))
                    }
                },
                Some('$') if self.peek(0) == Some('{') => {
                    self.bump();
                    let (el, ec) = (self.line, self.col);
                    let mut depth = 1;
                    let mut src = String::new();
                    loop {
                        match self.bump() {
                            None | Some('\n') => {
                                return Err(self.err(
                                    line,
                                    col,
                                    "E-LEX-STRING",
                                    "unterminated `${` interpolation".into(),
                                    Some("close it with `}`"),
                                ))
                            }
                            Some('{') => {
                                depth += 1;
                                src.push('{');
                            }
                            Some('}') => {
                                depth -= 1;
                                if depth == 0 {
                                    break;
                                }
                                src.push('}');
                            }
                            Some('"') => {
                                // a nested string inside the interpolation
                                src.push('"');
                                loop {
                                    match self.bump() {
                                        None | Some('\n') => {
                                            return Err(self.err(
                                                line,
                                                col,
                                                "E-LEX-STRING",
                                                "unterminated string inside `${}`".into(),
                                                None,
                                            ))
                                        }
                                        Some('\\') => {
                                            src.push('\\');
                                            if let Some(c) = self.bump() {
                                                src.push(c);
                                            }
                                        }
                                        Some('"') => {
                                            src.push('"');
                                            break;
                                        }
                                        Some(c) => src.push(c),
                                    }
                                }
                            }
                            Some(c) => src.push(c),
                        }
                    }
                    if !lit.is_empty() {
                        parts.push(StrPart::Lit(std::mem::take(&mut lit)));
                    }
                    parts.push(StrPart::Expr(src, el, ec));
                }
                Some(c) => lit.push(c),
            }
        }
        if !lit.is_empty() || parts.is_empty() {
            parts.push(StrPart::Lit(lit));
        }
        Ok(parts)
    }
}

/// Remove newlines that do not end a statement.
fn fold_newlines(toks: Vec<Token>) -> Vec<Token> {
    let mut out: Vec<Token> = Vec::with_capacity(toks.len());
    // Stack of open brackets: true for `(`/`[` (newlines ignored), false for `{`.
    let mut stack: Vec<bool> = Vec::new();
    for (i, t) in toks.iter().enumerate() {
        match &t.tok {
            Tok::LParen | Tok::LBrack => stack.push(true),
            Tok::LBrace => stack.push(false),
            Tok::RParen | Tok::RBrack | Tok::RBrace => {
                stack.pop();
            }
            Tok::Newline => {
                if stack.last() == Some(&true) {
                    continue;
                }
                if let Some(prev) = out.last() {
                    if prev.tok.continues() || prev.tok == Tok::Newline {
                        continue;
                    }
                }
                // A following line that starts with `.name`, `and` or `or` continues this one.
                let mut j = i + 1;
                while toks.get(j).is_some_and(|t| t.tok == Tok::Newline) {
                    j += 1;
                }
                let cont = match toks.get(j).map(|t| &t.tok) {
                    Some(Tok::Dot) => matches!(toks.get(j + 1).map(|t| &t.tok), Some(Tok::Ident(_))),
                    Some(Tok::Ident(s)) => s == "and" || s == "or",
                    _ => false,
                };
                if cont {
                    continue;
                }
            }
            _ => {}
        }
        out.push(t.clone());
    }
    out
}
