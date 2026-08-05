/*!
The AST, flattened into something a worm can hold a handle to.

Our tree is `Box` and `Vec` of borrowed nodes, so it cannot cross a boundary:
mlua userdata has to be `'static`, and handing a guest a raw pointer into the
tree is undefined the moment it stashes one in a global. So each file's tree is
walked once into an owned table of records addressed by **pre-order index**,
which is the node id §4.1 of the plan reserved for exactly this.

A worm never receives our node types. It receives `(epoch, id)` pairs and calls
accessors, which is what lets us reshape the AST without breaking every pinned
worm. That trap is the one SWC fell into by making its tree the plugin ABI.
*/

use crate::syntax::ast::*;

/// What a node is, as the guest sees it. `filter` in `worm.toml` names these.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Block,
    // statements
    Local,
    Assign,
    Call,
    Do,
    While,
    Repeat,
    If,
    NumericFor,
    GenericFor,
    Function,
    LocalFunction,
    Return,
    Break,
    Continue,
    TypeAlias,
    Empty,
    // expressions
    Nil,
    True,
    False,
    Vararg,
    Number,
    String,
    InterpString,
    Name,
    FunctionExpr,
    Table,
    Binary,
    Unary,
    Index,
    CallExpr,
    Paren,
    IfElse,
    TypeAssert,
}

impl Kind {
    /// The name a worm writes in `filter`, and gets back from `node:kind()`
    pub fn name(self) -> &'static str {
        match self {
            Self::Block => "Block",
            Self::Local => "Local",
            Self::Assign => "Assign",
            Self::Call => "Call",
            Self::Do => "Do",
            Self::While => "While",
            Self::Repeat => "Repeat",
            Self::If => "If",
            Self::NumericFor => "NumericFor",
            Self::GenericFor => "GenericFor",
            Self::Function => "Function",
            Self::LocalFunction => "LocalFunction",
            Self::Return => "Return",
            Self::Break => "Break",
            Self::Continue => "Continue",
            Self::TypeAlias => "TypeAlias",
            Self::Empty => "Empty",
            Self::Nil => "Nil",
            Self::True => "True",
            Self::False => "False",
            Self::Vararg => "Vararg",
            Self::Number => "Number",
            Self::String => "String",
            Self::InterpString => "InterpString",
            Self::Name => "Name",
            Self::FunctionExpr => "FunctionExpr",
            Self::Table => "Table",
            Self::Binary => "Binary",
            Self::Unary => "Unary",
            Self::Index => "Index",
            Self::CallExpr => "CallExpr",
            Self::Paren => "Paren",
            Self::IfElse => "IfElse",
            Self::TypeAssert => "TypeAssert",
        }
    }

    /// Every kind, which `worm.d.luau` mirrors as a singleton union
    pub const ALL: &'static [Kind] = &[
        Kind::Block,
        Kind::Local,
        Kind::Assign,
        Kind::Call,
        Kind::Do,
        Kind::While,
        Kind::Repeat,
        Kind::If,
        Kind::NumericFor,
        Kind::GenericFor,
        Kind::Function,
        Kind::LocalFunction,
        Kind::Return,
        Kind::Break,
        Kind::Continue,
        Kind::TypeAlias,
        Kind::Empty,
        Kind::Nil,
        Kind::True,
        Kind::False,
        Kind::Vararg,
        Kind::Number,
        Kind::String,
        Kind::InterpString,
        Kind::Name,
        Kind::FunctionExpr,
        Kind::Table,
        Kind::Binary,
        Kind::Unary,
        Kind::Index,
        Kind::CallExpr,
        Kind::Paren,
        Kind::IfElse,
        Kind::TypeAssert,
    ];

    /// Parse a name a worm declared in `filter`, so a typo is caught at load
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|k| k.name() == name)
    }
}

/// One flattened node
#[derive(Debug, Clone)]
pub struct Node {
    pub kind: Kind,
    /// Byte range in the original source, half open
    pub span: (u32, u32),
    /// Pre-order index of the parent, absent only for the root
    pub parent: Option<u32>,
    /// Pre-order indexes of the direct children, in source order
    pub children: Vec<u32>,
}

/// Every node in one file, addressed by pre-order index
#[derive(Debug, Default)]
pub struct NodeTable {
    nodes: Vec<Node>,
}

impl NodeTable {
    /// Flatten a parsed chunk, resolving spans to bytes through `bytes`
    pub fn build(chunk: &Chunk, bytes: &impl Fn(TokSpan) -> (u32, u32)) -> Self {
        let mut table = Self::default();

        table.push_block(&chunk.block, None, bytes);

        table
    }

    /// How many nodes the file has
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// One node by its pre-order id
    pub fn get(&self, id: u32) -> Option<&Node> {
        self.nodes.get(id as usize)
    }

    /// Every node, in pre-order, which is also traversal order
    pub fn iter(&self) -> impl Iterator<Item = (u32, &Node)> {
        self.nodes
            .iter()
            .enumerate()
            .map(|(i, node)| (i as u32, node))
    }

    /// The source text a node covers
    pub fn text<'s>(&self, id: u32, src: &'s str) -> Option<&'s str> {
        let node = self.get(id)?;

        src.get(node.span.0 as usize..node.span.1 as usize)
    }

    /// Reserve a slot, returning its id, so children can name their parent
    fn open(&mut self, kind: Kind, span: (u32, u32), parent: Option<u32>) -> u32 {
        let id = self.nodes.len() as u32;

        self.nodes.push(Node {
            kind,
            span,
            parent,
            children: Vec::new(),
        });

        if let Some(parent) = parent {
            self.nodes[parent as usize].children.push(id);
        }

        id
    }

    fn push_block(
        &mut self,
        b: &Block,
        parent: Option<u32>,
        bytes: &impl Fn(TokSpan) -> (u32, u32),
    ) {
        let id = self.open(Kind::Block, bytes(b.span), parent);

        for stmt in &b.stmts {
            self.push_stmt(stmt, id, bytes);
        }
    }

    fn push_stmt(&mut self, s: &Stmt, parent: u32, bytes: &impl Fn(TokSpan) -> (u32, u32)) {
        let (kind, span) = match s {
            Stmt::Empty(span) => (Kind::Empty, *span),
            Stmt::Local(n) => (Kind::Local, n.span),
            Stmt::Assign(n) => (Kind::Assign, n.span),
            Stmt::Call(_, span) => (Kind::Call, *span),
            Stmt::Do(n) => (Kind::Do, n.span),
            Stmt::While(n) => (Kind::While, n.span),
            Stmt::Repeat(n) => (Kind::Repeat, n.span),
            Stmt::If(n) => (Kind::If, n.span),
            Stmt::NumericFor(n) => (Kind::NumericFor, n.span),
            Stmt::GenericFor(n) => (Kind::GenericFor, n.span),
            Stmt::Function(n) => (Kind::Function, n.span),
            Stmt::LocalFunction(n) => (Kind::LocalFunction, n.span),
            Stmt::Return(n) => (Kind::Return, n.span),
            Stmt::Break(span) => (Kind::Break, *span),
            Stmt::Continue(span) => (Kind::Continue, *span),
            Stmt::TypeAlias(n) => (Kind::TypeAlias, n.span),
        };

        let id = self.open(kind, bytes(span), Some(parent));

        // children, so a worm can walk into a statement rather than only past it
        match s {
            Stmt::Local(n) => {
                for e in &n.values {
                    self.push_expr(e, id, bytes);
                }
            }

            Stmt::Assign(n) => {
                for e in n.targets.iter().chain(&n.values) {
                    self.push_expr(e, id, bytes);
                }
            }

            Stmt::Call(e, _) => self.push_expr(e, id, bytes),

            Stmt::Do(n) => self.push_block(&n.block, Some(id), bytes),

            Stmt::While(n) => {
                self.push_expr(&n.cond, id, bytes);
                self.push_block(&n.block, Some(id), bytes);
            }

            Stmt::Repeat(n) => {
                self.push_block(&n.block, Some(id), bytes);
                self.push_expr(&n.cond, id, bytes);
            }

            Stmt::If(n) => {
                for (cond, block) in &n.branches {
                    self.push_expr(cond, id, bytes);
                    self.push_block(block, Some(id), bytes);
                }

                if let Some(block) = &n.else_block {
                    self.push_block(block, Some(id), bytes);
                }
            }

            Stmt::NumericFor(n) => {
                self.push_expr(&n.start, id, bytes);
                self.push_expr(&n.limit, id, bytes);

                if let Some(step) = &n.step {
                    self.push_expr(step, id, bytes);
                }

                self.push_block(&n.block, Some(id), bytes);
            }

            Stmt::GenericFor(n) => {
                for e in &n.exprs {
                    self.push_expr(e, id, bytes);
                }

                self.push_block(&n.block, Some(id), bytes);
            }

            Stmt::Function(n) => self.push_block(&n.body.block, Some(id), bytes),

            Stmt::LocalFunction(n) => self.push_block(&n.body.block, Some(id), bytes),

            Stmt::Return(n) => {
                for e in &n.values {
                    self.push_expr(e, id, bytes);
                }
            }

            Stmt::Empty(_) | Stmt::Break(_) | Stmt::Continue(_) | Stmt::TypeAlias(_) => {}
        }
    }

    fn push_expr(&mut self, e: &Expr, parent: u32, bytes: &impl Fn(TokSpan) -> (u32, u32)) {
        let (kind, span) = match e {
            Expr::Nil(s) => (Kind::Nil, *s),
            Expr::True(s) => (Kind::True, *s),
            Expr::False(s) => (Kind::False, *s),
            Expr::Vararg(s) => (Kind::Vararg, *s),
            Expr::Number(s) => (Kind::Number, *s),
            Expr::String(s) => (Kind::String, *s),
            Expr::InterpString(s) => (Kind::InterpString, *s),
            Expr::Name(s) => (Kind::Name, *s),
            Expr::Function { span, .. } => (Kind::FunctionExpr, *span),
            Expr::Table { span, .. } => (Kind::Table, *span),
            Expr::Binary { span, .. } => (Kind::Binary, *span),
            Expr::Unary { span, .. } => (Kind::Unary, *span),
            Expr::Index { span, .. } => (Kind::Index, *span),
            Expr::Call { span, .. } => (Kind::CallExpr, *span),
            Expr::Paren { span, .. } => (Kind::Paren, *span),
            Expr::IfElse { span, .. } => (Kind::IfElse, *span),
            Expr::TypeAssert { span, .. } => (Kind::TypeAssert, *span),
        };

        let id = self.open(kind, bytes(span), Some(parent));

        match e {
            Expr::Function { body, .. } => self.push_block(&body.block, Some(id), bytes),

            Expr::Table { fields, .. } => {
                for field in fields {
                    match field {
                        TableField::Positional(v) => self.push_expr(v, id, bytes),

                        TableField::Named { value, .. } => self.push_expr(value, id, bytes),

                        TableField::Computed { key, value } => {
                            self.push_expr(key, id, bytes);
                            self.push_expr(value, id, bytes);
                        }
                    }
                }
            }

            Expr::Binary { lhs, rhs, .. } => {
                self.push_expr(lhs, id, bytes);
                self.push_expr(rhs, id, bytes);
            }

            Expr::Unary { operand, .. } => self.push_expr(operand, id, bytes),

            Expr::Index { object, key, .. } => {
                self.push_expr(object, id, bytes);

                if let IndexKey::Computed(k) = key {
                    self.push_expr(k, id, bytes);
                }
            }

            Expr::Call { func, args, .. } => {
                self.push_expr(func, id, bytes);

                match args {
                    CallArgs::Paren(list) => {
                        for a in list {
                            self.push_expr(a, id, bytes);
                        }
                    }

                    CallArgs::Table(t) => self.push_expr(t, id, bytes),

                    CallArgs::Str(_) => {}
                }
            }

            Expr::Paren { inner, .. } => self.push_expr(inner, id, bytes),

            Expr::IfElse {
                branches,
                else_value,
                ..
            } => {
                for (cond, value) in branches {
                    self.push_expr(cond, id, bytes);
                    self.push_expr(value, id, bytes);
                }

                self.push_expr(else_value, id, bytes);
            }

            Expr::TypeAssert { expr, .. } => self.push_expr(expr, id, bytes),

            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax::lexer;
    use crate::syntax::parser;

    /// Flatten real source, the same way the pipeline will
    fn table(src: &str) -> (NodeTable, String) {
        let lexed = lexer::lex(src).expect("lexes");
        let chunk = parser::parse(src, &lexed.toks).expect("parses");
        let toks = lexed.toks.clone();

        let bytes = move |span: TokSpan| -> (u32, u32) {
            if span.is_empty() {
                let at = toks
                    .get(span.start as usize)
                    .map(|t| t.start)
                    .unwrap_or_default();

                return (at, at);
            }

            (
                toks[span.start as usize].start,
                toks[span.end as usize - 1].end,
            )
        };

        (NodeTable::build(&chunk, &bytes), src.to_owned())
    }

    fn kinds(t: &NodeTable) -> Vec<&'static str> {
        t.iter().map(|(_, n)| n.kind.name()).collect()
    }

    #[test]
    fn a_local_and_its_value_are_both_nodes() {
        let (t, _) = table("local x = 1\n");

        assert_eq!(kinds(&t), ["Block", "Local", "Number"]);
    }

    #[test]
    fn ids_are_pre_order_so_a_parent_precedes_its_children() {
        let (t, _) = table("local x = f(1)\n");

        for (id, node) in t.iter() {
            if let Some(parent) = node.parent {
                assert!(parent < id, "child {id} precedes parent {parent}");
            }
        }
    }

    #[test]
    fn children_point_back_at_their_parent() {
        let (t, _) = table("if a then b() else c() end\n");

        for (id, node) in t.iter() {
            for &child in &node.children {
                assert_eq!(t.get(child).unwrap().parent, Some(id));
            }
        }
    }

    #[test]
    fn a_span_covers_exactly_the_source_it_names() {
        let src = "local greeting = \"hello\"\n";
        let (t, src) = table(src);

        let string = t
            .iter()
            .find(|(_, n)| n.kind == Kind::String)
            .map(|(id, _)| id)
            .expect("a string node");

        assert_eq!(t.text(string, &src), Some("\"hello\""));
    }

    #[test]
    fn nested_blocks_nest_in_the_table_too() {
        let (t, _) = table("while true do local x = 1 end\n");
        let names = kinds(&t);

        assert!(names.contains(&"While"), "{names:?}");
        assert!(names.contains(&"Block"), "{names:?}");
        assert!(names.contains(&"Local"), "{names:?}");
    }

    #[test]
    fn a_call_exposes_its_function_and_arguments() {
        let (t, src) = table("print(\"a\", 2)\n");

        let call = t
            .iter()
            .find(|(_, n)| n.kind == Kind::CallExpr)
            .expect("a call");

        let args: Vec<_> = call
            .1
            .children
            .iter()
            .map(|&c| t.text(c, &src).unwrap())
            .collect();

        assert_eq!(args, ["print", "\"a\"", "2"]);
    }

    #[test]
    fn every_kind_name_round_trips() {
        let (t, _) = table("local a = 1\nfor i = 1, 2 do a += i end\n");

        for (_, node) in t.iter() {
            assert_eq!(Kind::from_name(node.kind.name()), Some(node.kind));
        }
    }

    #[test]
    fn an_unknown_filter_name_is_rejected() {
        assert_eq!(Kind::from_name("Cal"), None);
        assert_eq!(Kind::from_name("Call"), Some(Kind::Call));
    }

    #[test]
    fn an_empty_file_still_has_its_root_block() {
        let (t, _) = table("");

        assert_eq!(kinds(&t), ["Block"]);
        assert!(!t.is_empty());
    }
}
