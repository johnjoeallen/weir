//! Syntax tree.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Span {
    pub line: u32,
    pub col: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Ty {
    Str,
    Int,
    Bool,
    Path,
    Duration,
    Secret,
    List(Box<Ty>),
    Map(Box<Ty>, Box<Ty>),
    Enum(Vec<String>),
    Opt(Box<Ty>),
    Record(Vec<(String, Ty)>),
    /// A name the parser did not recognise as a built-in type (reported by the checker).
    Named(String),
}

impl Ty {
    pub fn is_optional(&self) -> bool {
        matches!(self, Ty::Opt(_))
    }

    pub fn show(&self) -> String {
        match self {
            Ty::Str => "string".into(),
            Ty::Int => "int".into(),
            Ty::Bool => "bool".into(),
            Ty::Path => "path".into(),
            Ty::Duration => "duration".into(),
            Ty::Secret => "secret".into(),
            Ty::List(t) => format!("list<{}>", t.show()),
            Ty::Map(k, v) => format!("map<{},{}>", k.show(), v.show()),
            Ty::Enum(v) => format!("enum({})", v.join(", ")),
            Ty::Opt(t) => format!("{}?", t.show()),
            Ty::Record(f) => format!(
                "{{{}}}",
                f.iter().map(|(n, t)| format!("{n}: {}", t.show())).collect::<Vec<_>>().join(", ")
            ),
            Ty::Named(n) => n.clone(),
        }
    }
}

#[derive(Debug, Clone)]
pub enum StrSeg {
    Lit(String),
    Expr(Expr),
}

#[derive(Debug, Clone)]
pub struct Arg {
    /// `None` only for a spread (`...$list`).
    pub name: Option<String>,
    pub expr: Expr,
    pub spread: bool,
    pub span: Span,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    Not,
    Neg,
}

#[derive(Debug, Clone)]
pub struct Expr {
    pub span: Span,
    pub kind: ExprKind,
}

#[derive(Debug, Clone)]
pub enum ExprKind {
    Int(i64),
    Bool(bool),
    Absent,
    Dur(i64, String),
    Str(Vec<StrSeg>),
    Ident(String),
    List(Vec<Arg>),
    Record(Vec<(String, Expr)>),
    /// `name(args)`: a flow, filter, fn or built-in.
    Call { name: String, args: Vec<Arg> },
    /// `recv.name(args)`: a pipeline stage, or a built-in applied to a value.
    Stage { recv: Box<Expr>, name: String, args: Vec<Arg> },
    /// `recv.name`
    Member { recv: Box<Expr>, name: String },
    Unary(UnOp, Box<Expr>),
    Binary(BinOp, Box<Expr>, Box<Expr>),
    /// `try expr`: yields an attempt record.
    Try(Box<Expr>),
    /// `run flow(args)` inside a test.
    RunFlow { name: String, args: Vec<Arg> },
    /// `if cond { expr } else { expr }`: each branch is a block whose last
    /// expression statement is its value.
    If { cond: Box<Expr>, then: Block, else_: Block },
}

#[derive(Debug, Clone)]
pub struct Block {
    pub stmts: Vec<Stmt>,
}

#[derive(Debug, Clone)]
pub struct Stmt {
    pub span: Span,
    pub kind: StmtKind,
}

#[derive(Debug, Clone)]
pub struct Catch {
    pub var: String,
    pub err_ty: String,
    pub body: Block,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum StmtKind {
    Let { name: String, ty: Option<Ty>, expr: Expr },
    Var { name: String, ty: Option<Ty>, expr: Expr },
    Assign { name: String, expr: Expr },
    Set { name: String, expr: Expr },
    If { cond: Expr, then: Block, else_: Option<Block> },
    IfLet { name: String, expr: Expr, then: Block, else_: Option<Block> },
    For { var: String, iter: Expr, body: Block },
    Try { body: Block, catches: Vec<Catch> },
    Return(Expr),
    Fail(Expr),
    /// An inline named block.
    Step(Step),
    /// A call or other expression in statement position, with step modifiers.
    Expr { expr: Expr, as_label: Option<Expr>, when: Option<Expr>, confirmable: bool },
    /// Test only.
    Expect(Expr),
}

#[derive(Debug, Clone)]
pub struct Step {
    pub name: String,
    pub as_label: Option<Expr>,
    pub body: Block,
    pub when: Option<Expr>,
    pub confirmable: bool,
}

#[derive(Debug, Clone)]
pub struct InDecl {
    pub name: String,
    pub ty: Ty,
    pub default: Option<Expr>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct OutDecl {
    pub name: String,
    pub ty: Ty,
    pub span: Span,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Vis {
    Normal,
    Main,
    Private,
}

#[derive(Debug, Clone)]
pub struct Flow {
    pub name: String,
    pub vis: Vis,
    pub ins: Vec<InDecl>,
    pub outs: Vec<OutDecl>,
    pub requires: Vec<Expr>,
    pub lock: Option<Expr>,
    pub finally: Option<Block>,
    pub body: Block,
    pub span: Span,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Ro,
    Rw,
}

impl Mode {
    pub fn word(self) -> &'static str {
        match self {
            Mode::Ro => "ro",
            Mode::Rw => "rw",
        }
    }
}

#[derive(Debug, Clone)]
pub enum MountSpec {
    System,
    Path { host: Expr, at: Option<Expr>, mode: Mode },
}

#[derive(Debug, Clone)]
pub struct Mount {
    pub spec: MountSpec,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct SandboxBlock {
    pub ins: Vec<InDecl>,
    pub mounts: Vec<Mount>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum See {
    System,
    Path { path: Expr, mode: Mode },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IoKind {
    None,
    Lines,
    Text,
    Json,
}

impl IoKind {
    pub fn word(self) -> &'static str {
        match self {
            IoKind::None => "none",
            IoKind::Lines => "lines",
            IoKind::Text => "text",
            IoKind::Json => "json",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Filter {
    pub name: String,
    pub program: String,
    pub program_span: Span,
    pub ins: Vec<InDecl>,
    pub args: Vec<Arg>,
    pub see: Option<Vec<(See, Span)>>,
    pub env: Vec<(String, Expr)>,
    pub takes: IoKind,
    pub yields: IoKind,
    pub ok_codes: Vec<i64>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct Param {
    pub name: String,
    pub ty: Ty,
}

#[derive(Debug, Clone)]
pub struct FnDecl {
    pub name: String,
    pub params: Vec<Param>,
    pub ret: Ty,
    pub body: Block,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum FixtureItem {
    File { name: String, text: Expr },
}

#[derive(Debug, Clone)]
pub enum TestItem {
    Fixture { dir: String, items: Vec<FixtureItem>, span: Span },
    Stub { filter: String, yields: Expr, span: Span },
    Stmt(Stmt),
}

#[derive(Debug, Clone)]
pub struct Test {
    pub name: String,
    pub items: Vec<TestItem>,
}

#[derive(Debug, Clone)]
pub struct Use {
    pub spec: String,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum Decl {
    Flow(Flow),
    Filter(Filter),
    Fn(FnDecl),
    Test(Test),
}

#[derive(Debug, Clone, Default)]
pub struct File {
    pub path: String,
    pub uses: Vec<Use>,
    pub sandbox: Option<SandboxBlock>,
    pub decls: Vec<Decl>,
}

impl File {
    pub fn flows(&self) -> impl Iterator<Item = &Flow> {
        self.decls.iter().filter_map(|d| if let Decl::Flow(f) = d { Some(f) } else { None })
    }
    pub fn filters(&self) -> impl Iterator<Item = &Filter> {
        self.decls.iter().filter_map(|d| if let Decl::Filter(f) = d { Some(f) } else { None })
    }
    pub fn fns(&self) -> impl Iterator<Item = &FnDecl> {
        self.decls.iter().filter_map(|d| if let Decl::Fn(f) = d { Some(f) } else { None })
    }
    pub fn tests(&self) -> impl Iterator<Item = &Test> {
        self.decls.iter().filter_map(|d| if let Decl::Test(f) = d { Some(f) } else { None })
    }
}
