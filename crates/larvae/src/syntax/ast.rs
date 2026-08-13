/*!
The Luau syntax tree. Nodes hold token index ranges, not owned strings. So a
print of a node is a replay of its tokens and the source between them. The
tree keeps types as spans on purpose. Larvae parses types but does not
interpret them until a rule needs to.
*/

/// A half-open range of token indices.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TokSpan {
    pub start: u32,
    pub end: u32,
}

impl TokSpan {
    pub fn new(start: usize, end: usize) -> Self {
        Self {
            start: start as u32,
            end: end as u32,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.start == self.end
    }
}

#[derive(Debug)]
pub struct Chunk {
    pub block: Block,
}

#[derive(Debug)]
pub struct Block {
    pub stmts: Vec<Stmt>,
    pub span: TokSpan,
}

#[derive(Debug)]
pub enum Stmt {
    /// A stray `;`.
    Empty(TokSpan),
    Local(Local),
    Assign(Assign),
    /// A call used as a statement.
    Call(Expr, TokSpan),
    Do(DoBlock),
    While(While),
    Repeat(Repeat),
    If(If),
    NumericFor(NumericFor),
    GenericFor(GenericFor),
    Function(Function),
    LocalFunction(LocalFunction),
    Return(Return),
    Break(TokSpan),
    Continue(TokSpan),
    TypeAlias(TypeAlias),
}

impl Stmt {
    pub fn span(&self) -> TokSpan {
        match self {
            Stmt::Empty(s) | Stmt::Break(s) | Stmt::Continue(s) | Stmt::Call(_, s) => *s,

            Stmt::Local(n) => n.span,

            Stmt::Assign(n) => n.span,

            Stmt::Do(n) => n.span,

            Stmt::While(n) => n.span,

            Stmt::Repeat(n) => n.span,

            Stmt::If(n) => n.span,

            Stmt::NumericFor(n) => n.span,

            Stmt::GenericFor(n) => n.span,

            Stmt::Function(n) => n.span,

            Stmt::LocalFunction(n) => n.span,

            Stmt::Return(n) => n.span,

            Stmt::TypeAlias(n) => n.span,
        }
    }
}

/// `local a, b: T = x, y` or its `const` form.
#[derive(Debug)]
pub struct Local {
    /// The `local` or `const` keyword.
    pub keyword: TokSpan,
    pub is_const: bool,
    pub names: Vec<Binding>,
    pub values: Vec<Expr>,
    pub span: TokSpan,
}

#[derive(Debug)]
pub struct Binding {
    pub name: TokSpan,
    pub ty: Option<TokSpan>,
}

/// `a, b = x, y` and the compound forms, for example `a += 1`.
#[derive(Debug)]
pub struct Assign {
    pub targets: Vec<Expr>,
    /// The `=` or compound operator.
    pub op: TokSpan,
    pub values: Vec<Expr>,
    pub span: TokSpan,
}

#[derive(Debug)]
pub struct DoBlock {
    pub block: Block,
    pub span: TokSpan,
}

#[derive(Debug)]
pub struct While {
    pub cond: Expr,
    pub block: Block,
    pub span: TokSpan,
}

#[derive(Debug)]
pub struct Repeat {
    pub block: Block,
    pub cond: Expr,
    pub span: TokSpan,
}

#[derive(Debug)]
pub struct If {
    /// The `if` and every `elseif`. Each entry is a condition and a body.
    pub branches: Vec<(Expr, Block)>,
    pub else_block: Option<Block>,
    pub span: TokSpan,
}

#[derive(Debug)]
pub struct NumericFor {
    pub var: Binding,
    pub start: Expr,
    pub limit: Expr,
    pub step: Option<Expr>,
    pub block: Block,
    pub span: TokSpan,
}

#[derive(Debug)]
pub struct GenericFor {
    pub vars: Vec<Binding>,
    pub exprs: Vec<Expr>,
    pub block: Block,
    pub span: TokSpan,
}

/// `function a.b:c() end`.
#[derive(Debug)]
pub struct Function {
    pub attributes: Vec<TokSpan>,
    /// Every name token in the dotted path, with the `:` method name included.
    pub path: Vec<TokSpan>,
    pub is_method: bool,
    pub body: FunctionBody,
    pub span: TokSpan,
}

#[derive(Debug)]
pub struct LocalFunction {
    pub attributes: Vec<TokSpan>,
    /// `const function f()` instead of `local function f()`.
    pub is_const: bool,
    pub name: TokSpan,
    pub body: FunctionBody,
    pub span: TokSpan,
}

#[derive(Debug)]
pub struct FunctionBody {
    pub generics: Option<TokSpan>,
    pub params: Vec<Param>,
    pub ret_type: Option<TokSpan>,
    pub block: Block,
    pub span: TokSpan,
}

#[derive(Debug)]
pub struct Param {
    /// The name token, or the `...` token for a vararg param.
    pub name: TokSpan,
    pub is_vararg: bool,
    pub ty: Option<TokSpan>,
}

#[derive(Debug)]
pub struct Return {
    pub values: Vec<Expr>,
    pub span: TokSpan,
}

/// `type X<T> = ...`, `export type ...`, `type function f() ... end`.
#[derive(Debug)]
pub struct TypeAlias {
    pub exported: bool,
    pub name: TokSpan,
    pub span: TokSpan,
}

#[derive(Debug)]
pub enum Expr {
    Nil(TokSpan),
    True(TokSpan),
    False(TokSpan),
    Vararg(TokSpan),
    Number(TokSpan),
    String(TokSpan),
    InterpString(TokSpan),
    Name(TokSpan),
    Function {
        attributes: Vec<TokSpan>,
        body: Box<FunctionBody>,
        span: TokSpan,
    },

    Table {
        fields: Vec<TableField>,
        span: TokSpan,
    },

    Binary {
        op: TokSpan,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
        span: TokSpan,
    },

    Unary {
        op: TokSpan,
        operand: Box<Expr>,
        span: TokSpan,
    },

    Paren {
        inner: Box<Expr>,
        span: TokSpan,
    },

    /// `obj.field` or `obj[key]`.
    Index {
        object: Box<Expr>,
        key: IndexKey,
        span: TokSpan,
    },

    /// `f(args)`, `obj:method(args)`, `f<<T>>(args)`.
    Call {
        func: Box<Expr>,
        method: Option<TokSpan>,
        /*
        An explicit type instantiation: the `<<T>>` in `f<<T>>()`.

        The tree holds it instead of a discard, because each rebuild of
        source from the tree must put it back. A discard turned
        `charm.atom<<number>>()` into `charm.atom()`. That result still
        parses, but it no longer means the same thing. So the loss stayed
        invisible until some code printed the tree.
        */
        type_args: Option<TokSpan>,
        args: CallArgs,
        span: TokSpan,
    },

    /// `if c then a else b` as an expression.
    IfElse {
        branches: Vec<(Expr, Expr)>,
        else_value: Box<Expr>,
        span: TokSpan,
    },

    /// `expr :: T`.
    TypeAssert {
        expr: Box<Expr>,
        ty: TokSpan,
        span: TokSpan,
    },
}

impl Expr {
    pub fn span(&self) -> TokSpan {
        match self {
            Expr::Nil(s)
            | Expr::True(s)
            | Expr::False(s)
            | Expr::Vararg(s)
            | Expr::Number(s)
            | Expr::String(s)
            | Expr::InterpString(s)
            | Expr::Name(s) => *s,
            Expr::Function { span, .. }
            | Expr::Table { span, .. }
            | Expr::Binary { span, .. }
            | Expr::Unary { span, .. }
            | Expr::Paren { span, .. }
            | Expr::Index { span, .. }
            | Expr::Call { span, .. }
            | Expr::IfElse { span, .. }
            | Expr::TypeAssert { span, .. } => *span,
        }
    }
}

#[derive(Debug)]
pub enum IndexKey {
    /// `.name`.
    Field(TokSpan),
    /// `[expr]`.
    Computed(Box<Expr>),
}

#[derive(Debug)]
pub enum CallArgs {
    Paren(Vec<Expr>),
    /// `f "str"`.
    Str(TokSpan),
    /// `f {table}`.
    Table(Box<Expr>),
}

#[derive(Debug)]
pub enum TableField {
    /// `value`.
    Positional(Expr),
    /// `name = value`.
    Named { name: TokSpan, value: Expr },
    /// `[key] = value`.
    Computed { key: Expr, value: Expr },
}
