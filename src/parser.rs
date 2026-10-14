//! Hand-written recursive-descent parser. It stops at the first error, so one
//! mistake never produces a cascade.

use crate::ast::*;
use crate::diag::Diag;
use crate::lexer::{lex, StrPart, Tok, Token};

pub const RESERVED: &[&str] = &[
    "absent", "and", "args", "as", "at", "catch", "confirmable", "else", "enum", "env", "expect", "false", "fail",
    "filter", "finally", "fixture", "flow", "fn", "for", "if", "in", "it", "json", "let", "lines", "lock", "main",
    "mount", "none", "not", "ok_codes", "or", "out", "private", "program", "requires", "return", "ro", "run", "rw",
    "sandbox", "secret", "see", "set", "stub", "system", "takes", "test", "text", "true", "try", "use", "var", "when", "within",
    "yields",
];

pub fn parse_file(path: &str, src: &str) -> Result<File, Diag> {
    let toks = lex(path, src)?;
    let mut p = Parser { file: path.to_string(), toks, pos: 0 };
    p.file()
}

type PResult<T> = Result<T, Diag>;

struct Parser {
    file: String,
    toks: Vec<Token>,
    pos: usize,
}

impl Parser {
    // ---- token helpers ----------------------------------------------------

    fn tok(&self) -> &Tok {
        &self.toks[self.pos.min(self.toks.len() - 1)].tok
    }

    fn tok_at(&self, n: usize) -> &Tok {
        &self.toks[(self.pos + n).min(self.toks.len() - 1)].tok
    }

    fn span(&self) -> Span {
        let t = &self.toks[self.pos.min(self.toks.len() - 1)];
        Span { line: t.line, col: t.col }
    }

    fn advance(&mut self) -> Tok {
        let t = self.tok().clone();
        if self.pos < self.toks.len() - 1 {
            self.pos += 1;
        }
        t
    }

    fn err_at(&self, span: Span, code: &str, msg: String, fix: Option<&str>) -> Diag {
        Diag::error(&self.file, span.line, span.col, code, msg, fix.map(String::from))
    }

    fn err_here(&self, code: &str, msg: String, fix: Option<&str>) -> Diag {
        self.err_at(self.span(), code, msg, fix)
    }

    fn unexpected(&self, expected: &str) -> Diag {
        self.err_here("E-SYNTAX", format!("expected {expected}, found {}", self.tok().describe()), None)
    }

    fn skip_nl(&mut self) {
        while *self.tok() == Tok::Newline {
            self.advance();
        }
    }

    fn is(&self, t: &Tok) -> bool {
        self.tok() == t
    }

    fn eat(&mut self, t: &Tok) -> bool {
        if self.is(t) {
            self.advance();
            true
        } else {
            false
        }
    }

    fn expect(&mut self, t: Tok) -> PResult<()> {
        if self.is(&t) {
            self.advance();
            Ok(())
        } else {
            Err(self.unexpected(&t.describe()))
        }
    }

    fn is_kw(&self, kw: &str) -> bool {
        matches!(self.tok(), Tok::Ident(s) if s == kw)
    }

    fn is_kw_at(&self, n: usize, kw: &str) -> bool {
        matches!(self.tok_at(n), Tok::Ident(s) if s == kw)
    }

    fn eat_kw(&mut self, kw: &str) -> bool {
        if self.is_kw(kw) {
            self.advance();
            true
        } else {
            false
        }
    }

    fn expect_kw(&mut self, kw: &str) -> PResult<()> {
        if self.eat_kw(kw) {
            Ok(())
        } else {
            Err(self.unexpected(&format!("`{kw}`")))
        }
    }

    /// Any identifier, reserved or not (argument names, field names).
    fn any_ident(&mut self, what: &str) -> PResult<String> {
        match self.tok().clone() {
            Tok::Ident(s) => {
                self.advance();
                Ok(s)
            }
            _ => Err(self.unexpected(what)),
        }
    }

    /// An identifier that is allowed as a binding or declaration name.
    fn name(&mut self, what: &str) -> PResult<String> {
        let span = self.span();
        let s = self.any_ident(what)?;
        if RESERVED.contains(&s.as_str()) {
            return Err(self.err_at(
                span,
                "E-RESERVED-WORD",
                format!("`{s}` is a reserved word and cannot be used as {what}"),
                Some(&format!("choose another name, for example `{s}_value`")),
            ));
        }
        Ok(s)
    }

    fn end_stmt(&mut self) -> PResult<()> {
        match self.tok() {
            Tok::Newline => {
                self.advance();
                Ok(())
            }
            Tok::RBrace | Tok::Eof => Ok(()),
            other => Err(self.err_here(
                "E-SYNTAX",
                format!("unexpected {} after the end of a statement", other.describe()),
                Some("statements end at a newline; put each statement on its own line"),
            )),
        }
    }

    // ---- file and declarations -------------------------------------------

    fn file(&mut self) -> PResult<File> {
        let mut f = File { path: self.file.clone(), ..Default::default() };
        self.skip_nl();
        while self.is_kw("use") {
            let span = self.span();
            self.advance();
            match self.advance() {
                Tok::Str(parts) => match parts.as_slice() {
                    [StrPart::Lit(s)] => f.uses.push(Use { spec: s.clone(), span }),
                    _ => {
                        return Err(self.err_at(span, "E-SYNTAX", "`use` takes a plain string, without `${}`".into(), None))
                    }
                },
                _ => return Err(self.err_at(span, "E-SYNTAX", "`use` must be followed by a string".into(), Some("write use \"./lib/net.weir\""))),
            }
            self.end_stmt()?;
            self.skip_nl();
        }
        if self.is_kw("sandbox") {
            f.sandbox = Some(self.sandbox_block()?);
            self.skip_nl();
        }
        while *self.tok() != Tok::Eof {
            if self.is_kw("use") {
                return Err(self.err_here(
                    "E-SYNTAX",
                    "`use` lines must come first in the file".into(),
                    Some("move this `use` above the `sandbox` block and all declarations"),
                ));
            }
            if self.is_kw("sandbox") {
                return Err(self.err_here(
                    "E-SYNTAX",
                    "the `sandbox` block must come before the declarations, and a file has at most one".into(),
                    Some("move it directly after the `use` lines"),
                ));
            }
            let d = self.declaration()?;
            f.decls.push(d);
            self.skip_nl();
        }
        Ok(f)
    }

    fn declaration(&mut self) -> PResult<Decl> {
        let span = self.span();
        let mut vis = Vis::Normal;
        if self.is_kw("main") {
            vis = Vis::Main;
            self.advance();
        } else if self.is_kw("private") {
            vis = Vis::Private;
            self.advance();
        }
        if self.is_kw("flow") {
            self.advance();
            return Ok(Decl::Flow(self.flow(vis, span)?));
        }
        if vis != Vis::Normal {
            return Err(self.unexpected("`flow` after `main`/`private`"));
        }
        if self.is_kw("filter") {
            self.advance();
            return Ok(Decl::Filter(self.filter(span)?));
        }
        if self.is_kw("fn") {
            self.advance();
            return Ok(Decl::Fn(self.fn_decl(span)?));
        }
        if self.is_kw("test") {
            self.advance();
            return Ok(Decl::Test(self.test(span)?));
        }
        Err(self.err_here(
            "E-SYNTAX",
            format!("expected a declaration (`flow`, `filter`, `fn` or `test`), found {}", self.tok().describe()),
            Some("a file's top level holds declarations only; put statements inside a flow"),
        ))
    }

    fn sandbox_block(&mut self) -> PResult<SandboxBlock> {
        let span = self.span();
        self.expect_kw("sandbox")?;
        self.expect(Tok::LBrace)?;
        let mut b = SandboxBlock { ins: vec![], mounts: vec![], span };
        self.skip_nl();
        while !self.is(&Tok::RBrace) {
            let sp = self.span();
            if self.is_kw("in") {
                b.ins.push(self.in_decl()?);
            } else if self.eat_kw("mount") {
                if self.eat_kw("system") {
                    b.mounts.push(Mount { spec: MountSpec::System, span: sp });
                } else {
                    let host = self.expr()?;
                    let at = if self.eat_kw("at") { Some(self.expr()?) } else { None };
                    let mode = self.mode()?;
                    b.mounts.push(Mount { spec: MountSpec::Path { host, at, mode }, span: sp });
                }
            } else {
                return Err(self.unexpected("`in` or `mount`"));
            }
            self.end_stmt()?;
            self.skip_nl();
        }
        self.expect(Tok::RBrace)?;
        Ok(b)
    }

    fn mode(&mut self) -> PResult<Mode> {
        if self.eat_kw("ro") {
            Ok(Mode::Ro)
        } else if self.eat_kw("rw") {
            Ok(Mode::Rw)
        } else {
            Err(self.err_here(
                "E-SYNTAX",
                format!("expected `ro` or `rw`, found {}", self.tok().describe()),
                Some("every mount and `see` names its mode, for example `mount \"/var/log\" ro`"),
            ))
        }
    }

    fn in_decl(&mut self) -> PResult<InDecl> {
        let span = self.span();
        self.expect_kw("in")?;
        let name = self.name("an input name")?;
        self.expect(Tok::Colon)?;
        let ty = self.ty()?;
        let default = if self.eat(&Tok::Eq) { Some(self.expr()?) } else { None };
        Ok(InDecl { name, ty, default, span })
    }

    fn ty(&mut self) -> PResult<Ty> {
        let mut t = if self.eat(&Tok::LBrace) {
            let mut fields = Vec::new();
            self.skip_nl();
            while !self.is(&Tok::RBrace) {
                let n = self.any_ident("a field name")?;
                self.expect(Tok::Colon)?;
                fields.push((n, self.ty()?));
                self.skip_nl();
                if !self.eat(&Tok::Comma) {
                    break;
                }
                self.skip_nl();
            }
            self.skip_nl();
            self.expect(Tok::RBrace)?;
            Ty::Record(fields)
        } else {
            let span = self.span();
            let n = self.any_ident("a type")?;
            match n.as_str() {
                "string" => Ty::Str,
                "int" => Ty::Int,
                "bool" => Ty::Bool,
                "path" => Ty::Path,
                "duration" => Ty::Duration,
                "secret" => Ty::Secret,
                "list" => {
                    self.expect(Tok::Lt)?;
                    let t = self.ty()?;
                    self.expect(Tok::Gt)?;
                    Ty::List(Box::new(t))
                }
                "map" => {
                    self.expect(Tok::Lt)?;
                    let k = self.ty()?;
                    self.expect(Tok::Comma)?;
                    let v = self.ty()?;
                    self.expect(Tok::Gt)?;
                    Ty::Map(Box::new(k), Box::new(v))
                }
                "enum" => {
                    self.expect(Tok::LParen)?;
                    let mut vals = Vec::new();
                    loop {
                        vals.push(self.any_ident("an enum value")?);
                        if !self.eat(&Tok::Comma) {
                            break;
                        }
                    }
                    self.expect(Tok::RParen)?;
                    Ty::Enum(vals)
                }
                other => {
                    let _ = span;
                    Ty::Named(other.to_string())
                }
            }
        };
        while self.eat(&Tok::Question) {
            t = Ty::Opt(Box::new(t));
        }
        Ok(t)
    }

    fn flow(&mut self, vis: Vis, span: Span) -> PResult<Flow> {
        let name = self.name("a flow name")?;
        self.expect(Tok::LBrace)?;
        let mut fl = Flow {
            name,
            vis,
            ins: vec![],
            outs: vec![],
            requires: vec![],
            lock: None,
            finally: None,
            body: Block { stmts: vec![] },
            span,
        };
        self.skip_nl();
        while !self.is(&Tok::RBrace) {
            if self.is(&Tok::Eof) {
                return Err(self.err_at(span, "E-SYNTAX", format!("flow `{}` is never closed", fl.name), Some("add a closing `}`")));
            }
            let sp = self.span();
            if self.is_kw("in") && matches!(self.tok_at(1), Tok::Ident(_)) && *self.tok_at(2) == Tok::Colon {
                fl.ins.push(self.in_decl()?);
                self.end_stmt()?;
            } else if self.is_kw("out") && matches!(self.tok_at(1), Tok::Ident(_)) && *self.tok_at(2) == Tok::Colon {
                self.advance();
                let name = self.name("an output name")?;
                self.expect(Tok::Colon)?;
                let ty = self.ty()?;
                fl.outs.push(OutDecl { name, ty, span: sp });
                self.end_stmt()?;
            } else if self.is_kw("requires") {
                self.advance();
                fl.requires.push(self.expr()?);
                self.end_stmt()?;
            } else if self.is_kw("lock") {
                self.advance();
                if fl.lock.is_some() {
                    return Err(self.err_at(sp, "E-SYNTAX", "a flow has at most one `lock`".into(), None));
                }
                fl.lock = Some(self.expr()?);
                self.end_stmt()?;
            } else if self.is_kw("finally") && *self.tok_at(1) == Tok::LBrace {
                self.advance();
                if fl.finally.is_some() {
                    return Err(self.err_at(sp, "E-SYNTAX", "a flow has at most one `finally` block".into(), None));
                }
                fl.finally = Some(self.block()?);
                self.end_stmt()?;
            } else {
                let s = self.stmt()?;
                fl.body.stmts.push(s);
            }
            self.skip_nl();
        }
        self.expect(Tok::RBrace)?;
        Ok(fl)
    }

    fn filter(&mut self, span: Span) -> PResult<Filter> {
        let name = self.name("a filter name")?;
        self.expect(Tok::LBrace)?;
        let mut f = Filter {
            name,
            program: String::new(),
            program_span: span,
            ins: vec![],
            args: vec![],
            see: None,
            env: vec![],
            takes: IoKind::None,
            yields: IoKind::None,
            ok_codes: vec![0],
            span,
        };
        let mut have_program = false;
        self.skip_nl();
        while !self.is(&Tok::RBrace) {
            let sp = self.span();
            if self.eat_kw("program") {
                match self.advance() {
                    Tok::Str(parts) => match parts.as_slice() {
                        [StrPart::Lit(s)] => {
                            f.program = s.clone();
                            f.program_span = sp;
                            have_program = true;
                        }
                        _ => {
                            return Err(self.err_at(
                                sp,
                                "E-DYNAMIC-PROGRAM",
                                "`program` must be a plain string literal".into(),
                                Some("the checker infers which executables a flow may start, so the name cannot be computed"),
                            ))
                        }
                    },
                    _ => {
                        return Err(self.err_at(
                            sp,
                            "E-DYNAMIC-PROGRAM",
                            "`program` must be a string literal".into(),
                            Some("write program \"grep\""),
                        ))
                    }
                }
            } else if self.is_kw("in") {
                f.ins.push(self.in_decl()?);
            } else if self.eat_kw("args") {
                f.args = self.list_lit()?;
            } else if self.is_kw("see") {
                // `see` lines sit directly in the filter; the first one makes the view restricted.
                f.see.get_or_insert_with(Vec::new).push(self.see_item()?);
            } else if self.is_kw("sandbox") {
                return Err(self.err_here(
                    "E-FILTER-SANDBOX",
                    "a filter no longer has a `sandbox` block".into(),
                    Some("write the `see` lines directly in the filter, for example `see system` and `see \"/var/log\" ro`"),
                ));
            } else if self.eat_kw("env") {
                self.expect(Tok::LBrace)?;
                self.skip_nl();
                while !self.is(&Tok::RBrace) {
                    let n = self.any_ident("an environment variable name")?;
                    self.expect(Tok::Eq)?;
                    let e = self.expr()?;
                    f.env.push((n, e));
                    self.end_stmt()?;
                    self.skip_nl();
                }
                self.expect(Tok::RBrace)?;
            } else if self.eat_kw("takes") {
                f.takes = self.io_kind()?;
            } else if self.eat_kw("yields") {
                f.yields = self.io_kind()?;
            } else if self.eat_kw("ok_codes") {
                let items = self.list_lit()?;
                f.ok_codes = items
                    .iter()
                    .map(|a| match &a.expr.kind {
                        ExprKind::Int(n) => Ok(*n),
                        _ => Err(self.err_at(a.span, "E-SYNTAX", "`ok_codes` holds integer literals".into(), Some("write ok_codes [0, 1]"))),
                    })
                    .collect::<PResult<Vec<_>>>()?;
            } else {
                return Err(self.err_here(
                    "E-SYNTAX",
                    format!("expected a filter clause, found {}", self.tok().describe()),
                    Some("a filter has: program, in, args, see, env, takes, yields, ok_codes"),
                ));
            }
            self.end_stmt()?;
            self.skip_nl();
        }
        self.expect(Tok::RBrace)?;
        if !have_program {
            return Err(self.err_at(span, "E-FILTER-PROGRAM", format!("filter `{}` has no `program`", f.name), Some("add program \"<executable>\"")));
        }
        Ok(f)
    }

    /// `see system` or `see <path> ro|rw`: one entry of a restricted view.
    fn see_item(&mut self) -> PResult<(See, Span)> {
        self.expect_kw("see")?;
        self.view_item()
    }

    /// `system` or `<path> ro|rw`: an entry of a view, as in a filter's `see` line or a `within` header.
    fn view_item(&mut self) -> PResult<(See, Span)> {
        let ssp = self.span();
        if self.eat_kw("system") {
            Ok((See::System, ssp))
        } else {
            let path = self.expr()?;
            let mode = self.mode()?;
            Ok((See::Path { path, mode }, ssp))
        }
    }

    fn io_kind(&mut self) -> PResult<IoKind> {
        match self.tok() {
            Tok::Ident(s) if s == "none" => {
                self.advance();
                Ok(IoKind::None)
            }
            Tok::Ident(s) if s == "lines" => {
                self.advance();
                Ok(IoKind::Lines)
            }
            Tok::Ident(s) if s == "text" => {
                self.advance();
                Ok(IoKind::Text)
            }
            Tok::Ident(s) if s == "json" => {
                self.advance();
                Ok(IoKind::Json)
            }
            _ => Err(self.err_here(
                "E-SYNTAX",
                format!("expected one of `none`, `lines`, `text`, `json`, found {}", self.tok().describe()),
                None,
            )),
        }
    }

    fn fn_decl(&mut self, span: Span) -> PResult<FnDecl> {
        let name = self.name("a function name")?;
        self.expect(Tok::LParen)?;
        let mut params = Vec::new();
        while !self.is(&Tok::RParen) {
            let n = self.name("a parameter name")?;
            self.expect(Tok::Colon)?;
            params.push(Param { name: n, ty: self.ty()? });
            if !self.eat(&Tok::Comma) {
                break;
            }
        }
        self.expect(Tok::RParen)?;
        if !self.is(&Tok::Arrow) {
            return Err(self.err_here(
                "E-SYNTAX",
                format!("expected `->` and a return type, found {}", self.tok().describe()),
                Some("a fn always declares its return type: fn name(x: int) -> int { ... }"),
            ));
        }
        self.advance();
        let ret = self.ty()?;
        let body = self.block()?;
        Ok(FnDecl { name, params, ret, body, span })
    }

    fn test(&mut self, span: Span) -> PResult<Test> {
        let name = match self.advance() {
            Tok::Str(p) => match p.as_slice() {
                [StrPart::Lit(s)] => s.clone(),
                _ => return Err(self.err_at(span, "E-SYNTAX", "a test name is a plain string".into(), None)),
            },
            _ => return Err(self.err_at(span, "E-SYNTAX", "`test` must be followed by a name string".into(), Some("write test \"describes the behaviour\" { ... }"))),
        };
        self.expect(Tok::LBrace)?;
        let mut items = Vec::new();
        self.skip_nl();
        while !self.is(&Tok::RBrace) {
            let sp = self.span();
            if self.eat_kw("fixture") {
                self.expect_kw_ident("dir")?;
                let dir = self.plain_string("a fixture directory path")?;
                self.expect(Tok::LBrace)?;
                let mut fitems = Vec::new();
                self.skip_nl();
                while !self.is(&Tok::RBrace) {
                    self.expect_kw_ident("file")?;
                    let fname = self.plain_string("a file name")?;
                    self.expect_kw("text")?;
                    self.expect(Tok::Colon)?;
                    let text = self.expr()?;
                    fitems.push(FixtureItem::File { name: fname, text });
                    self.end_stmt()?;
                    self.skip_nl();
                }
                self.expect(Tok::RBrace)?;
                items.push(TestItem::Fixture { dir, items: fitems, span: sp });
                self.end_stmt()?;
            } else if self.eat_kw("stub") {
                let filter = self.any_ident("a filter name")?;
                self.expect_kw("yields")?;
                self.io_kind()?;
                self.expect(Tok::Colon)?;
                let yields = self.expr()?;
                items.push(TestItem::Stub { filter, yields, span: sp });
                self.end_stmt()?;
            } else {
                items.push(TestItem::Stmt(self.stmt()?));
            }
            self.skip_nl();
        }
        self.expect(Tok::RBrace)?;
        Ok(Test { name, items })
    }

    fn expect_kw_ident(&mut self, kw: &str) -> PResult<()> {
        // like expect_kw, for words that are not reserved (`dir`, `file`)
        self.expect_kw(kw)
    }

    fn plain_string(&mut self, what: &str) -> PResult<String> {
        let sp = self.span();
        match self.advance() {
            Tok::Str(p) => match p.as_slice() {
                [StrPart::Lit(s)] => Ok(s.clone()),
                _ => Err(self.err_at(sp, "E-SYNTAX", format!("{what} must be a plain string"), None)),
            },
            other => Err(self.err_at(sp, "E-SYNTAX", format!("expected {what}, found {}", other.describe()), None)),
        }
    }

    // ---- statements -------------------------------------------------------

    fn block(&mut self) -> PResult<Block> {
        let open = self.span();
        if !self.is(&Tok::LBrace) {
            return Err(self.err_here(
                "E-SYNTAX",
                format!("expected `{{` to open a block, found {}", self.tok().describe()),
                Some("the body of `if`, `for`, `else`, `try`, `catch` and `finally` is always a `{ ... }` block"),
            ));
        }
        self.expect(Tok::LBrace)?;
        let mut stmts = Vec::new();
        self.skip_nl();
        while !self.is(&Tok::RBrace) {
            if self.is(&Tok::Eof) {
                return Err(self.err_at(open, "E-SYNTAX", "this block is never closed".into(), Some("add a closing `}`")));
            }
            stmts.push(self.stmt()?);
            self.skip_nl();
        }
        self.expect(Tok::RBrace)?;
        Ok(Block { stmts })
    }

    /// A step body: a block whose first statement may be `when <expr>`.
    fn step_block(&mut self) -> PResult<(Block, Option<Expr>)> {
        let open = self.span();
        self.expect(Tok::LBrace)?;
        let mut stmts = Vec::new();
        let mut when = None;
        self.skip_nl();
        if self.is_kw("when") {
            self.advance();
            when = Some(self.expr()?);
            self.end_stmt()?;
            self.skip_nl();
        }
        while !self.is(&Tok::RBrace) {
            if self.is(&Tok::Eof) {
                return Err(self.err_at(open, "E-SYNTAX", "this block is never closed".into(), Some("add a closing `}`")));
            }
            stmts.push(self.stmt()?);
            self.skip_nl();
        }
        self.expect(Tok::RBrace)?;
        Ok((Block { stmts }, when))
    }

    fn stmt(&mut self) -> PResult<Stmt> {
        let span = self.span();
        let kind = self.stmt_kind()?;
        self.end_stmt()?;
        Ok(Stmt { span, kind })
    }

    fn stmt_kind(&mut self) -> PResult<StmtKind> {
        let span = self.span();
        let word = match self.tok() {
            Tok::Ident(s) => s.clone(),
            _ => return self.expr_stmt(),
        };
        match word.as_str() {
            "else" | "catch" => Err(self.err_here(
                "E-SYNTAX",
                format!("`{word}` has no matching `{}` before it on this line", if word == "else" { "if" } else { "try" }),
                Some(&format!("put `{word}` on the same line as the closing `}}` of the block it continues: `}} {word} ... {{`")),
            )),
            "let" | "var" => {
                self.advance();
                let name = self.name("a variable name")?;
                let ty = if self.eat(&Tok::Colon) { Some(self.ty()?) } else { None };
                self.expect(Tok::Eq)?;
                let expr = self.expr()?;
                Ok(if word == "let" { StmtKind::Let { name, ty, expr } } else { StmtKind::Var { name, ty, expr } })
            }
            "set" => {
                self.advance();
                let name = self.any_ident("an output name")?;
                self.expect(Tok::Eq)?;
                Ok(StmtKind::Set { name, expr: self.expr()? })
            }
            "if" => self.if_stmt(),
            "for" => {
                self.advance();
                let var = self.name("a loop variable")?;
                self.expect_kw("in")?;
                let iter = self.expr()?;
                let body = self.block()?;
                Ok(StmtKind::For { var, iter, body })
            }
            "try" if *self.tok_at(1) == Tok::LBrace => {
                self.advance();
                let body = self.block()?;
                let mut catches = Vec::new();
                while self.is_kw("catch") {
                    let csp = self.span();
                    self.advance();
                    let var = self.name("an error variable")?;
                    self.expect(Tok::Colon)?;
                    let err_ty = self.any_ident("an error type")?;
                    let body = self.block()?;
                    catches.push(Catch { var, err_ty, body, span: csp });
                }
                if catches.is_empty() {
                    return Err(self.err_at(
                        span,
                        "E-TRY-NO-CATCH",
                        "a `try` block needs at least one `catch`".into(),
                        Some("add `catch e: ExecError { ... }`, or use `try` before a single call to get an attempt record"),
                    ));
                }
                Ok(StmtKind::Try { body, catches })
            }
            "return" => {
                self.advance();
                Ok(StmtKind::Return(self.expr()?))
            }
            "fail" => {
                self.advance();
                if !self.is_kw("message") {
                    return Err(self.err_here("E-SYNTAX", format!("expected `message:` after `fail`, found {}", self.tok().describe()), Some("write fail message: \"what went wrong\"")));
                }
                self.advance();
                self.expect(Tok::Colon)?;
                Ok(StmtKind::Fail(self.expr()?))
            }
            "expect" => {
                self.advance();
                Ok(StmtKind::Expect(self.expr()?))
            }
            _ => {
                // assignment: `name = expr`
                if *self.tok_at(1) == Tok::Eq && !RESERVED.contains(&word.as_str()) {
                    self.advance();
                    self.advance();
                    return Ok(StmtKind::Assign { name: word, expr: self.expr()? });
                }
                // inline step: `name {` or `name as label {`
                if !RESERVED.contains(&word.as_str()) && (*self.tok_at(1) == Tok::LBrace || self.is_kw_at(1, "as")) {
                    // `name as label {` is a step; `name as` can never start an expression
                    self.advance();
                    let as_label = if self.eat_kw("as") { Some(self.expr()?) } else { None };
                    let (body, when_inside) = self.step_block()?;
                    let (when_after, confirmable) = self.modifiers()?;
                    let when = match (when_inside, when_after) {
                        (Some(a), Some(b)) => Some(Expr {
                            span: a.span,
                            kind: ExprKind::Binary(BinOp::And, Box::new(a), Box::new(b)),
                        }),
                        (a, b) => a.or(b),
                    };
                    return Ok(StmtKind::Step(Step { name: word, as_label, body, when, confirmable }));
                }
                self.expr_stmt()
            }
        }
    }

    fn if_stmt(&mut self) -> PResult<StmtKind> {
        self.expect_kw("if")?;
        if self.eat_kw("let") {
            let name = self.name("a variable name")?;
            self.expect(Tok::Eq)?;
            let expr = self.expr()?;
            let then = self.block()?;
            let else_ = self.else_part()?;
            return Ok(StmtKind::IfLet { name, expr, then, else_ });
        }
        let cond = self.expr()?;
        let then = self.block()?;
        let else_ = self.else_part()?;
        Ok(StmtKind::If { cond, then, else_ })
    }

    fn else_part(&mut self) -> PResult<Option<Block>> {
        if !self.eat_kw("else") {
            return Ok(None);
        }
        if self.is_kw("if") {
            let span = self.span();
            let kind = self.if_stmt()?;
            return Ok(Some(Block { stmts: vec![Stmt { span, kind }] }));
        }
        Ok(Some(self.block()?))
    }

    fn modifiers(&mut self) -> PResult<(Option<Expr>, bool)> {
        let mut when = None;
        let mut confirmable = false;
        loop {
            if self.is_kw("when") && when.is_none() {
                self.advance();
                when = Some(self.expr()?);
            } else if self.is_kw("confirmable") && !confirmable {
                self.advance();
                confirmable = true;
            } else {
                break;
            }
        }
        Ok((when, confirmable))
    }

    fn expr_stmt(&mut self) -> PResult<StmtKind> {
        let expr = self.expr()?;
        let as_label = if self.eat_kw("as") { Some(self.expr()?) } else { None };
        let (when, confirmable) = self.modifiers()?;
        Ok(StmtKind::Expr { expr, as_label, when, confirmable })
    }

    // ---- expressions ------------------------------------------------------

    pub fn expr(&mut self) -> PResult<Expr> {
        self.or_expr()
    }

    fn bin(&self, op: BinOp, l: Expr, r: Expr) -> Expr {
        Expr { span: l.span, kind: ExprKind::Binary(op, Box::new(l), Box::new(r)) }
    }

    fn or_expr(&mut self) -> PResult<Expr> {
        let mut l = self.and_expr()?;
        while self.is_kw("or") {
            self.advance();
            let r = self.and_expr()?;
            l = self.bin(BinOp::Or, l, r);
        }
        Ok(l)
    }

    fn and_expr(&mut self) -> PResult<Expr> {
        let mut l = self.cmp_expr()?;
        while self.is_kw("and") {
            self.advance();
            let r = self.cmp_expr()?;
            l = self.bin(BinOp::And, l, r);
        }
        Ok(l)
    }

    fn cmp_expr(&mut self) -> PResult<Expr> {
        let mut l = self.add_expr()?;
        loop {
            let op = match self.tok() {
                Tok::EqEq => BinOp::Eq,
                Tok::NotEq => BinOp::Ne,
                Tok::Lt => BinOp::Lt,
                Tok::Le => BinOp::Le,
                Tok::Gt => BinOp::Gt,
                Tok::Ge => BinOp::Ge,
                _ => break,
            };
            self.advance();
            let r = self.add_expr()?;
            l = self.bin(op, l, r);
        }
        Ok(l)
    }

    fn add_expr(&mut self) -> PResult<Expr> {
        let mut l = self.mul_expr()?;
        loop {
            let op = match self.tok() {
                Tok::Plus => BinOp::Add,
                Tok::Minus => BinOp::Sub,
                _ => break,
            };
            self.advance();
            let r = self.mul_expr()?;
            l = self.bin(op, l, r);
        }
        Ok(l)
    }

    fn mul_expr(&mut self) -> PResult<Expr> {
        let mut l = self.unary()?;
        loop {
            let op = match self.tok() {
                Tok::Star => BinOp::Mul,
                Tok::Slash => BinOp::Div,
                _ => break,
            };
            self.advance();
            let r = self.unary()?;
            l = self.bin(op, l, r);
        }
        Ok(l)
    }

    fn unary(&mut self) -> PResult<Expr> {
        let span = self.span();
        if self.is_kw("not") {
            self.advance();
            let e = self.unary()?;
            return Ok(Expr { span, kind: ExprKind::Unary(UnOp::Not, Box::new(e)) });
        }
        if self.is(&Tok::Minus) {
            self.advance();
            let e = self.unary()?;
            return Ok(Expr { span, kind: ExprKind::Unary(UnOp::Neg, Box::new(e)) });
        }
        self.postfix()
    }

    fn postfix(&mut self) -> PResult<Expr> {
        let mut e = self.primary()?;
        while self.is(&Tok::Dot) {
            let span = self.span();
            self.advance();
            let name = match self.tok().clone() {
                Tok::Ident(s) => {
                    self.advance();
                    s
                }
                _ => return Err(self.err_here("E-SYNTAX", format!("expected a name after `.`, found {}", self.tok().describe()), None)),
            };
            if self.is(&Tok::LParen) {
                let args = self.args()?;
                e = Expr { span: e.span, kind: ExprKind::Stage { recv: Box::new(e), name, args } };
            } else {
                e = Expr { span, kind: ExprKind::Member { recv: Box::new(e), name } };
            }
        }
        Ok(e)
    }

    fn primary(&mut self) -> PResult<Expr> {
        let span = self.span();
        let kind = match self.tok().clone() {
            Tok::Int(n) => {
                self.advance();
                ExprKind::Int(n)
            }
            Tok::Dur(n, s) => {
                self.advance();
                ExprKind::Dur(n, s)
            }
            Tok::Str(parts) => {
                self.advance();
                ExprKind::Str(self.interpolate(parts)?)
            }
            Tok::LBrack => {
                let items = self.list_lit()?;
                ExprKind::List(items)
            }
            Tok::LBrace => {
                self.advance();
                let mut fields = Vec::new();
                self.skip_nl();
                while !self.is(&Tok::RBrace) {
                    let n = self.any_ident("a field name")?;
                    self.expect(Tok::Colon)?;
                    self.skip_nl();
                    fields.push((n, self.expr()?));
                    self.skip_nl();
                    if !self.eat(&Tok::Comma) {
                        break;
                    }
                    self.skip_nl();
                }
                self.skip_nl();
                self.expect(Tok::RBrace)?;
                ExprKind::Record(fields)
            }
            Tok::LParen => {
                self.advance();
                let e = self.expr()?;
                self.expect(Tok::RParen)?;
                return Ok(e);
            }
            Tok::Ident(s) => match s.as_str() {
                "true" => {
                    self.advance();
                    ExprKind::Bool(true)
                }
                "false" => {
                    self.advance();
                    ExprKind::Bool(false)
                }
                "absent" => {
                    self.advance();
                    ExprKind::Absent
                }
                "try" => {
                    self.advance();
                    let inner = self.postfix()?;
                    ExprKind::Try(Box::new(inner))
                }
                "if" => {
                    self.advance();
                    let cond = self.expr()?;
                    let then = self.block()?;
                    if !self.is_kw("else") {
                        return Err(self.err_here(
                            "E-IF-EXPR-ELSE",
                            "an `if` used as a value needs an `else` branch".into(),
                            Some("add `else { ... }`, or use `if` as a statement"),
                        ));
                    }
                    self.advance();
                    let else_ = if self.is_kw("if") {
                        let sp = self.span();
                        let e = self.primary()?;
                        Block { stmts: vec![Stmt { span: sp, kind: StmtKind::Expr { expr: e, as_label: None, when: None, confirmable: false } }] }
                    } else {
                        self.block()?
                    };
                    ExprKind::If { cond: Box::new(cond), then, else_ }
                }
                "within" => {
                    self.advance();
                    let mut items = vec![self.view_item()?];
                    while self.eat(&Tok::Comma) {
                        items.push(self.view_item()?);
                    }
                    let body = self.block()?;
                    ExprKind::Within { items, body }
                }
                "run" if matches!(self.tok_at(1), Tok::Ident(_)) => {
                    self.advance();
                    let name = self.any_ident("a flow name")?;
                    let args = self.args()?;
                    ExprKind::RunFlow { name, args }
                }
                _ => {
                    self.advance();
                    if self.is(&Tok::LParen) {
                        let args = self.args()?;
                        ExprKind::Call { name: s, args }
                    } else {
                        ExprKind::Ident(s)
                    }
                }
            },
            other => {
                return Err(self.err_here(
                    "E-SYNTAX",
                    format!("expected an expression, found {}", other.describe()),
                    None,
                ))
            }
        };
        Ok(Expr { span, kind })
    }

    fn list_lit(&mut self) -> PResult<Vec<Arg>> {
        self.expect(Tok::LBrack)?;
        let mut items = Vec::new();
        self.skip_nl();
        while !self.is(&Tok::RBrack) {
            let span = self.span();
            if self.eat(&Tok::Ellipsis) {
                self.eat(&Tok::Dollar);
                let e = self.postfix()?;
                items.push(Arg { name: None, expr: e, spread: true, span });
            } else {
                items.push(Arg { name: None, expr: self.expr()?, spread: false, span });
            }
            self.skip_nl();
            if !self.eat(&Tok::Comma) {
                break;
            }
            self.skip_nl();
        }
        self.skip_nl();
        self.expect(Tok::RBrack)?;
        Ok(items)
    }

    fn args(&mut self) -> PResult<Vec<Arg>> {
        self.expect(Tok::LParen)?;
        let mut args = Vec::new();
        self.skip_nl();
        while !self.is(&Tok::RParen) {
            let span = self.span();
            if self.eat(&Tok::Ellipsis) {
                self.eat(&Tok::Dollar);
                let e = self.postfix()?;
                args.push(Arg { name: None, expr: e, spread: true, span });
            } else if matches!(self.tok(), Tok::Ident(_)) && *self.tok_at(1) == Tok::Colon {
                let name = self.any_ident("an argument name")?;
                self.advance();
                self.skip_nl();
                let expr = self.expr()?;
                args.push(Arg { name: Some(name), expr, spread: false, span });
            } else {
                return Err(self.err_here(
                    "E-POSITIONAL-ARG",
                    "every argument is named; there is no positional form".into(),
                    Some("write the argument as `name: value`"),
                ));
            }
            self.skip_nl();
            if !self.eat(&Tok::Comma) {
                break;
            }
            self.skip_nl();
        }
        self.skip_nl();
        self.expect(Tok::RParen)?;
        Ok(args)
    }

    fn interpolate(&mut self, parts: Vec<StrPart>) -> PResult<Vec<StrSeg>> {
        let mut out = Vec::new();
        for p in parts {
            match p {
                StrPart::Lit(s) => out.push(StrSeg::Lit(s)),
                StrPart::Expr(src, line, col) => {
                    let mut toks = lex(&self.file, &src)?;
                    for t in toks.iter_mut() {
                        if t.line == 1 {
                            t.col += col - 1;
                        }
                        t.line += line - 1;
                    }
                    let mut sub = Parser { file: self.file.clone(), toks, pos: 0 };
                    sub.skip_nl();
                    if sub.is(&Tok::Eof) {
                        return Err(self.err_at(Span { line, col }, "E-SYNTAX", "empty `${}` interpolation".into(), None));
                    }
                    let e = sub.expr()?;
                    sub.skip_nl();
                    if !sub.is(&Tok::Eof) {
                        return Err(sub.unexpected("the end of the interpolation"));
                    }
                    out.push(StrSeg::Expr(e));
                }
            }
        }
        Ok(out)
    }
}
