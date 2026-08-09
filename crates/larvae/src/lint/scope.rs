/*!
What every name in a file refers to.

There is already a scope walk under `rules`, built for the transformer, which
answers one question: is this name a local. The lints need more than that.
`unused_variable` needs to know whether a binding was ever read, `shadowing`
needs to know what a binding hides, and `unscoped_variables` needs to tell a
write that lands on a local from one that quietly creates a global. Rather than
widen the transformer's walk and make every rule pay for it, this is a second
one that answers all of them at once.

Lua's binding order is the part worth getting right, and it is not obvious:

- `local x = x` reads the *outer* `x`, because the right side is evaluated
  before the name exists.
- `local function f` binds `f` before its own body, so it can call itself.
  `local f = function()` does not.
- a `for` variable exists only inside its loop.
- `repeat ... until c` evaluates `c` inside the block's scope, so it can see
  locals the block declared. It is the only place in the language where a
  scope outlives its block.
*/

use std::collections::HashMap;

use crate::syntax::ast::*;
use crate::syntax::lexer::Tok;

/// What introduced a binding
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// `local x = ...`
    Local,
    /// `local function f() end`
    LocalFunction,
    /// A function parameter
    Param,
    /// A `for` variable
    Loop,
}

/// One local, and everything that happened to it
#[derive(Debug)]
pub struct Binding<'a> {
    pub name: &'a str,
    /// Token index of the name where it was declared
    pub declared_at: u32,
    /// Token indexes of every place it is read
    pub reads: Vec<u32>,
    /// Token indexes of every place it is assigned after declaration
    pub writes: Vec<u32>,
    pub origin: Origin,
    /// The binding in an enclosing scope this one hides, if any
    pub shadows: Option<usize>,
    /*
    The statement that declared it.

    Kept so `shadowing` can tell `local x = x + 1` from `local x = 2`: the
    first reads the outer binding inside its own declaration, which is the
    deliberate form, and the difference is exactly whether the outer read
    falls inside this span.
    */
    pub declared_in: TokSpan,
}

impl Names<'_> {
    /// Whether this name reference is a global, meaning nothing in the file bound it
    pub fn is_global(&self, token: u32) -> bool {
        self.undefined_set.contains(&token)
    }
}

impl Binding<'_> {
    /// Whether a name starting with `_` is deliberately unused, by convention
    pub fn is_ignored(&self) -> bool {
        self.name.starts_with('_')
    }
}

/// Everything the walk learned
#[derive(Debug, Default)]
pub struct Names<'a> {
    pub bindings: Vec<Binding<'a>>,
    /// Token indexes of reads that no binding in the file explains
    pub undefined: Vec<u32>,
    /*
    The same set, for lookup.

    Several lints ask "is this name a global" once per expression, and a
    linear scan of `undefined` makes that quadratic in a file with many
    globals, which describes every Roblox script.
    */
    undefined_set: std::collections::HashSet<u32>,
    /// Token indexes of writes that create a global rather than set a local
    pub global_writes: Vec<u32>,
    /// Binding index for each declaration token, for a lint given a token
    pub by_token: HashMap<u32, usize>,
    /// Binding index for each read token, so a call site can find its callee
    pub read_of: HashMap<u32, usize>,
}

pub fn resolve<'a>(src: &'a str, toks: &'a [Tok], chunk: &'a Chunk) -> Names<'a> {
    let mut binder = Binder {
        src,
        toks,
        scopes: vec![Vec::new()],
        current: chunk.block.span,
        out: Names::default(),
    };

    binder.block(&chunk.block);
    binder.pop_scope();

    /*
    A name the file assigns as a global is defined for the whole file.

    The walk cannot know this as it goes, because `function onTouch()` at the
    top and `Connect(onTouch)` at the bottom are resolved in that order and the
    binding is never entered into a scope. Reading a global before the line
    that sets it is nil at runtime, but it is also how every Roblox script is
    written, so the lenient answer is the right one: collect what the file
    defines, then stop calling those undefined.
    */
    let defined: std::collections::HashSet<&str> = binder
        .out
        .global_writes
        .iter()
        .map(|&t| toks[t as usize].text(src))
        .collect();

    if !defined.is_empty() {
        binder
            .out
            .undefined
            .retain(|&t| !defined.contains(toks[t as usize].text(src)));

        binder
            .out
            .undefined_set
            .retain(|&t| !defined.contains(toks[t as usize].text(src)));
    }

    binder.out
}

struct Binder<'a> {
    src: &'a str,
    toks: &'a [Tok],
    /// Each scope holds the names it introduced and which binding they are
    scopes: Vec<Vec<(&'a str, usize)>>,
    /// The statement being walked, which is what any new binding belongs to
    current: TokSpan,
    out: Names<'a>,
}

impl<'a> Binder<'a> {
    fn tok(&self, index: u32) -> &'a str {
        self.toks[index as usize].text(self.src)
    }

    fn name_of(&self, span: TokSpan) -> &'a str {
        self.tok(span.start)
    }

    fn push_scope(&mut self) {
        self.scopes.push(Vec::new());
    }

    fn pop_scope(&mut self) {
        self.scopes.pop();
    }

    /// Innermost binding of a name, which is the one a reference means
    fn lookup(&self, name: &str) -> Option<usize> {
        self.scopes
            .iter()
            .rev()
            .find_map(|scope| scope.iter().rev().find(|(n, _)| *n == name))
            .map(|(_, i)| *i)
    }

    fn declare(&mut self, span: TokSpan, origin: Origin) {
        let name = self.name_of(span);
        let shadows = self.lookup(name);

        let index = self.out.bindings.len();
        self.out.bindings.push(Binding {
            name,
            declared_at: span.start,
            reads: Vec::new(),
            writes: Vec::new(),
            origin,
            shadows,
            declared_in: self.current,
        });

        self.out.by_token.insert(span.start, index);
        self.scopes
            .last_mut()
            .expect("there is always a scope")
            .push((name, index));
    }

    fn read(&mut self, span: TokSpan) {
        let name = self.name_of(span);

        match self.lookup(name) {
            Some(i) => {
                self.out.bindings[i].reads.push(span.start);
                self.out.read_of.insert(span.start, i);
            }

            None => {
                self.out.undefined.push(span.start);
                self.out.undefined_set.insert(span.start);
            }
        }
    }

    /*
    A bare name on the left of an assignment.

    If nothing bound it, this statement is what creates it, and it creates a
    global. That is the whole of `unscoped_variables`: in Lua the difference
    between a local and a global is one keyword, and leaving it out is silent.
    */
    fn write(&mut self, span: TokSpan) {
        let name = self.name_of(span);

        match self.lookup(name) {
            Some(i) => self.out.bindings[i].writes.push(span.start),

            None => self.out.global_writes.push(span.start),
        }
    }

    // --- the walk ----------------------------------------------------------

    fn block(&mut self, block: &'a Block) {
        for stmt in &block.stmts {
            self.stmt(stmt);
        }
    }

    /// A block with its own scope, which is every block except a repeat's
    fn scoped_block(&mut self, block: &'a Block) {
        self.push_scope();
        self.block(block);
        self.pop_scope();
    }

    fn stmt(&mut self, stmt: &'a Stmt) {
        let outer = std::mem::replace(&mut self.current, stmt.span());

        self.stmt_inner(stmt);
        self.current = outer;
    }

    fn stmt_inner(&mut self, stmt: &'a Stmt) {
        match stmt {
            Stmt::Empty(_) | Stmt::Break(_) | Stmt::Continue(_) => {}

            // `type Foo = Types.Foo` is a use of the local `Types`
            Stmt::TypeAlias(n) => self.type_reads(n.span),

            // the values are evaluated before the names exist, so `local x = x`
            // reads whatever `x` already meant
            Stmt::Local(n) => {
                for value in &n.values {
                    self.expr(value);
                }

                for binding in &n.names {
                    if let Some(ty) = binding.ty {
                        self.type_reads(ty);
                    }

                    self.declare(binding.name, Origin::Local);
                }
            }

            Stmt::Assign(n) => {
                for value in &n.values {
                    self.expr(value);
                }

                for target in &n.targets {
                    match target {
                        Expr::Name(span) => self.write(*span),

                        // `t.k = v` reads `t`, it does not bind anything
                        other => self.expr(other),
                    }
                }
            }

            Stmt::Call(e, _) => self.expr(e),

            Stmt::Do(n) => self.scoped_block(&n.block),

            Stmt::While(n) => {
                self.expr(&n.cond);
                self.scoped_block(&n.block);
            }

            /*
            `until` is evaluated inside the block's scope, so the scope is
            popped after the condition rather than before it. This is the only
            construct in the language that works this way.
            */
            Stmt::Repeat(n) => {
                self.push_scope();
                self.block(&n.block);
                self.expr(&n.cond);
                self.pop_scope();
            }

            Stmt::If(n) => {
                for (cond, block) in &n.branches {
                    self.expr(cond);
                    self.scoped_block(block);
                }

                if let Some(block) = &n.else_block {
                    self.scoped_block(block);
                }
            }

            Stmt::NumericFor(n) => {
                self.expr(&n.start);
                self.expr(&n.limit);

                if let Some(step) = &n.step {
                    self.expr(step);
                }

                self.push_scope();
                self.declare(n.var.name, Origin::Loop);
                self.block(&n.block);
                self.pop_scope();
            }

            Stmt::GenericFor(n) => {
                for e in &n.exprs {
                    self.expr(e);
                }

                self.push_scope();

                for var in &n.vars {
                    self.declare(var.name, Origin::Loop);
                }

                self.block(&n.block);
                self.pop_scope();
            }

            /*
            `function a.b.c()` does not bind anything, it reads `a` and writes
            through it. `function f()` with a bare name writes a global unless
            something already bound `f`.
            */
            Stmt::Function(n) => {
                if let Some(first) = n.path.first() {
                    if n.path.len() == 1 && !n.is_method {
                        self.write(*first);
                    } else {
                        self.read(*first);
                    }
                }

                self.function_body(&n.body, n.is_method);
            }

            // bound before its own body, so it can call itself
            Stmt::LocalFunction(n) => {
                self.declare(n.name, Origin::LocalFunction);
                self.function_body(&n.body, false);
            }

            Stmt::Return(n) => {
                for e in &n.values {
                    self.expr(e);
                }
            }
        }
    }

    fn function_body(&mut self, body: &'a FunctionBody, is_method: bool) {
        self.push_scope();

        /*
        A method's implicit `self` is a real binding, so `function t:m()` can
        use it without being told it is undefined. It is declared at the body's
        first token because it has no name token of its own, and marked as a
        parameter so nothing reports it unused.
        */
        if is_method {
            let index = self.out.bindings.len();
            self.out.bindings.push(Binding {
                name: "self",
                declared_at: body.span.start,
                reads: Vec::new(),
                writes: Vec::new(),
                origin: Origin::Param,
                shadows: self.lookup("self"),
                declared_in: body.span,
            });

            self.scopes
                .last_mut()
                .expect("just pushed")
                .push(("self", index));
        }

        if let Some(generics) = body.generics {
            self.type_reads(generics);
        }

        if let Some(ret) = body.ret_type {
            self.type_reads(ret);
        }

        for param in &body.params {
            if let Some(ty) = param.ty {
                self.type_reads(ty);
            }

            if !param.is_vararg {
                self.declare(
                    TokSpan::new(param.name.start as usize, param.name.end as usize),
                    Origin::Param,
                );
            }
        }

        self.block(&body.block);
        self.pop_scope();
    }

    fn expr(&mut self, e: &'a Expr) {
        match e {
            Expr::Nil(_)
            | Expr::True(_)
            | Expr::False(_)
            | Expr::Vararg(_)
            | Expr::Number(_)
            | Expr::String(_) => {}

            /*
            An interpolated string holds expressions, but the lexer keeps it as
            one opaque token, so the names inside it are invisible here. The
            consequence is that a local used only inside a backtick string
            looks unused, so those reads are recovered by scanning the token's
            text for identifiers rather than by parsing it.
            */
            Expr::InterpString(span) => self.interp_reads(*span),

            Expr::Name(span) => self.read(*span),

            Expr::Function { body, .. } => self.function_body(body, false),

            Expr::Table { fields, .. } => {
                for field in fields {
                    match field {
                        TableField::Positional(v) => self.expr(v),

                        // the key is a name, not a reference to one
                        TableField::Named { value, .. } => self.expr(value),

                        TableField::Computed { key, value } => {
                            self.expr(key);
                            self.expr(value);
                        }
                    }
                }
            }

            Expr::Binary { lhs, rhs, .. } => {
                self.expr(lhs);
                self.expr(rhs);
            }

            Expr::Unary { operand, .. } => self.expr(operand),

            Expr::Paren { inner, .. } => self.expr(inner),

            Expr::Index { object, key, .. } => {
                self.expr(object);

                if let IndexKey::Computed(k) = key {
                    self.expr(k);
                }
            }

            Expr::Call {
                func,
                args,
                type_args,
                ..
            } => {
                self.expr(func);

                if let Some(ty) = type_args {
                    self.type_reads(*ty);
                }

                match args {
                    CallArgs::Paren(list) => {
                        for a in list {
                            self.expr(a);
                        }
                    }

                    CallArgs::Table(t) => self.expr(t),

                    CallArgs::Str(_) => {}
                }
            }

            Expr::IfElse {
                branches,
                else_value,
                ..
            } => {
                for (cond, value) in branches {
                    self.expr(cond);
                    self.expr(value);
                }

                self.expr(else_value);
            }

            Expr::TypeAssert { expr, ty, .. } => {
                self.expr(expr);
                self.type_reads(*ty);
            }
        }
    }

    /*
    Names referenced from inside a type.

    The tree keeps types as token spans and never interprets them, so a local
    used only from a type, `local T = require(...)` then `type Foo = T.Foo`,
    would otherwise look unused. Walking the tokens recovers it.

    Approximate in the same direction as the interpolated string scan: a name
    matching a local counts as a read even when it was really a type of the
    same name. Over counting keeps a lint quiet where it might have spoken,
    which is far better than reporting a live variable as unused.
    */
    fn type_reads(&mut self, span: TokSpan) {
        for i in span.start..span.end {
            if !matches!(
                self.toks[i as usize].kind,
                crate::syntax::lexer::TokKind::Ident
            ) {
                continue;
            }

            // a name after a dot or a colon is a field, not a reference
            if i > span.start && matches!(self.tok(i - 1), "." | ":") {
                continue;
            }

            let name = self.tok(i);

            if let Some(b) = self.lookup(name) {
                self.out.bindings[b].reads.push(self.toks[i as usize].start);
            }
        }
    }

    /*
    Identifiers inside a `{...}` hole of an interpolated string.

    Approximate on purpose. A name here is counted as a read of whatever it
    resolves to, which can over count if the same word appears as a table key
    inside the hole. Over counting means a lint stays quiet where it might have
    spoken; under counting would mean reporting a live variable as unused,
    which is the error people actually notice.
    */
    fn interp_reads(&mut self, span: TokSpan) {
        let tok = &self.toks[span.start as usize];
        let text = tok.text(self.src);
        let base = tok.start;

        let bytes = text.as_bytes();
        let mut i = 0;
        let mut depth = 0usize;

        while i < bytes.len() {
            match bytes[i] {
                b'\\' => i += 2,

                b'{' => {
                    depth += 1;
                    i += 1;
                }

                b'}' => {
                    depth = depth.saturating_sub(1);
                    i += 1;
                }

                c if depth > 0 && (c.is_ascii_alphabetic() || c == b'_') => {
                    let start = i;

                    while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_')
                    {
                        i += 1;
                    }

                    // a name straight after a dot is a field, not a reference
                    if start > 0 && bytes[start - 1] == b'.' {
                        continue;
                    }

                    let name = &text[start..i];

                    if let Some(b) = self.lookup(name) {
                        self.out.bindings[b].reads.push(base + start as u32);
                    }
                }

                _ => i += 1,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax::{lexer, parser};

    struct Resolved {
        src: String,
        lexed: lexer::Lexed,
        chunk: Chunk,
    }

    fn parse(src: &str) -> Resolved {
        let lexed = lexer::lex(src).expect("lexes");
        let chunk = parser::parse(src, &lexed.toks).expect("parses");

        Resolved {
            src: src.to_string(),
            lexed,
            chunk,
        }
    }

    impl Resolved {
        fn names(&self) -> Names<'_> {
            resolve(&self.src, &self.lexed.toks, &self.chunk)
        }
    }

    #[test]
    fn a_local_is_bound_and_its_reads_counted() {
        let r = parse("local x = 1\nprint(x)\nprint(x)\n");
        let names = r.names();

        assert_eq!(names.bindings.len(), 1);
        assert_eq!(names.bindings[0].name, "x");
        assert_eq!(names.bindings[0].reads.len(), 2);
    }

    #[test]
    fn an_unread_local_has_no_reads() {
        let r = parse("local x = 1\n");

        assert!(r.names().bindings[0].reads.is_empty());
    }

    #[test]
    fn a_write_is_not_a_read() {
        let r = parse("local x = 1\nx = 2\n");
        let names = r.names();

        assert!(names.bindings[0].reads.is_empty());
        assert_eq!(names.bindings[0].writes.len(), 1);
    }

    /// The rule everyone gets wrong, the right side is evaluated first
    #[test]
    fn local_x_equals_x_reads_the_outer_x() {
        let r = parse("local x = 1\ndo\n\tlocal x = x\nend\n");
        let names = r.names();

        assert_eq!(names.bindings[0].reads.len(), 1, "the outer x was read");
        assert!(names.bindings[1].reads.is_empty(), "the inner x was not");
        assert!(names.undefined.is_empty());
    }

    #[test]
    fn a_local_function_can_call_itself() {
        let r = parse("local function f(n)\n\treturn f(n)\nend\n");
        let names = r.names();

        assert_eq!(names.bindings[0].name, "f");
        assert_eq!(names.bindings[0].reads.len(), 1);
        assert!(names.undefined.is_empty());
    }

    /// Unlike a local function, which is the difference worth having a test for
    #[test]
    fn a_local_assigned_a_function_cannot() {
        let r = parse("local f = function(n)\n\treturn f(n)\nend\n");

        assert_eq!(r.names().undefined.len(), 1, "f is not bound yet");
    }

    /// `undefined` holds every unbound name, globals included, so count only `i`
    #[test]
    fn a_loop_variable_does_not_escape_its_loop() {
        let r = parse("for i = 1, 10 do\n\tprint(i)\nend\nprint(i)\n");
        let names = r.names();

        assert_eq!(
            names.bindings[0].reads.len(),
            1,
            "read once, inside the loop"
        );

        let loose_i = names
            .undefined
            .iter()
            .filter(|&&t| r.lexed.toks[t as usize].text(&r.src) == "i")
            .count();

        assert_eq!(loose_i, 1, "the i after the loop is bound by nothing");
    }

    #[test]
    fn a_generic_for_binds_every_variable() {
        let r = parse("for k, v in pairs(t) do\n\tprint(k, v)\nend\n");
        let names = r.names();

        assert_eq!(names.bindings.len(), 2);
        assert!(names.bindings.iter().all(|b| b.reads.len() == 1));
    }

    /// The one place a scope outlives its block
    #[test]
    fn an_until_condition_sees_the_blocks_locals() {
        let r = parse("repeat\n\tlocal done = check()\nuntil done\n");
        let names = r.names();

        assert_eq!(names.bindings[0].reads.len(), 1);
        assert!(
            names
                .undefined
                .iter()
                .all(|&t| r.lexed.toks[t as usize].text(&r.src) != "done")
        );
    }

    #[test]
    fn shadowing_is_recorded_with_what_it_hides() {
        let r = parse("local x = 1\ndo\n\tlocal x = 2\n\tprint(x)\nend\n");
        let names = r.names();

        assert_eq!(names.bindings[1].shadows, Some(0));
        assert_eq!(names.bindings[0].shadows, None);
    }

    #[test]
    fn a_sibling_scope_does_not_shadow() {
        let r = parse("do\n\tlocal x = 1\nend\ndo\n\tlocal x = 2\nend\n");
        let names = r.names();

        assert!(names.bindings.iter().all(|b| b.shadows.is_none()));
    }

    #[test]
    fn parameters_are_bindings() {
        let r = parse("local function f(a, b)\n\treturn a\nend\n");
        let names = r.names();

        let a = names.bindings.iter().find(|b| b.name == "a").expect("a");
        let b = names.bindings.iter().find(|b| b.name == "b").expect("b");

        assert_eq!(a.origin, Origin::Param);
        assert_eq!(a.reads.len(), 1);
        assert!(b.reads.is_empty());
    }

    #[test]
    fn a_method_gets_its_implicit_self() {
        let r = parse("function t:m()\n\treturn self.x\nend\n");
        let names = r.names();

        let this = names
            .bindings
            .iter()
            .find(|b| b.name == "self")
            .expect("self");

        assert_eq!(this.reads.len(), 1);
        assert!(
            !names
                .undefined
                .iter()
                .any(|&t| r.lexed.toks[t as usize].text(&r.src) == "self")
        );
    }

    #[test]
    fn assigning_a_name_nothing_bound_creates_a_global() {
        let r = parse("counter = 1\n");
        let names = r.names();

        assert_eq!(names.global_writes.len(), 1);
        assert!(names.bindings.is_empty());
    }

    #[test]
    fn assigning_a_local_does_not() {
        let r = parse("local counter = 0\ncounter = 1\n");

        assert!(r.names().global_writes.is_empty());
    }

    #[test]
    fn a_field_assignment_reads_the_table_rather_than_binding() {
        let r = parse("local t = {}\nt.k = 1\n");
        let names = r.names();

        assert!(names.global_writes.is_empty());
        assert_eq!(names.bindings[0].reads.len(), 1);
    }

    #[test]
    fn a_table_key_is_not_a_reference() {
        let r = parse("local value = 1\nlocal t = { value = 2 }\n");

        assert!(
            r.names().bindings[0].reads.is_empty(),
            "the key `value` is not a use of the local"
        );
    }

    #[test]
    fn a_field_name_is_not_a_reference() {
        let r = parse("local name = 1\nlocal t = {}\nprint(t.name)\n");

        assert!(r.names().bindings[0].reads.is_empty());
    }

    /// The lexer keeps an interpolated string whole, so its reads are recovered
    #[test]
    fn a_name_used_only_inside_an_interpolation_still_counts_as_read() {
        let r = parse("local who = \"world\"\nprint(`hello {who}`)\n");

        assert_eq!(r.names().bindings[0].reads.len(), 1);
    }

    #[test]
    fn text_outside_the_holes_of_an_interpolation_is_not_a_reference() {
        let r = parse("local hello = 1\nprint(`hello there`)\n");

        assert!(r.names().bindings[0].reads.is_empty());
    }

    #[test]
    fn a_field_inside_an_interpolation_is_not_a_reference() {
        let r = parse("local name = 1\nlocal t = {}\nprint(`{t.name}`)\n");

        assert!(r.names().bindings[0].reads.is_empty());
    }

    #[test]
    fn an_underscore_name_is_marked_as_deliberately_unused() {
        let r = parse("local _unused = 1\nlocal used = 2\n");
        let names = r.names();

        assert!(names.bindings[0].is_ignored());
        assert!(!names.bindings[1].is_ignored());
    }
}
