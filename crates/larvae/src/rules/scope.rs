/*!
The split of names into locals and globals

A rule that rewrites a bare name must know if the source bound the name
first. A define substituted into `local DEBUG = f()` would corrupt the
code without a report. For this reason, the walk tracks each binding that
a block introduces. The walk reports only the name references that no
binding covers.

Lua's binding order matters, and the walk follows it. `local x = x` reads
the outer x on the right. A `local function f` can call itself. A for
variable exists only inside its own loop.

The parser takes a type for its extent and never reads inside it, so a
name that only a type uses is invisible to the tree. `type_refs` recovers
those references from the tokens, in the same way as the linter's
resolver. A rename that skipped them would leave `x :: jecs.Component`
behind while the `jecs` binding took a new name.
*/

use std::collections::HashSet;

use crate::rules::engine::RuleCtx;
use crate::syntax::ast::*;
use crate::syntax::lexer::TokKind;

/// One local, the point of its declaration, and each point where the code reads it.
#[derive(Debug)]
pub struct Binding {
    /// The token index of the name in its declaration.
    pub declared_at: u32,
    /// The token indexes of each reference to it. A recursive call counts.
    pub uses: Vec<u32>,
    /// The token indexes of references inside a type that read this value.
    pub type_uses: Vec<u32>,
    /// The statement kind that introduced it. Deletion depends on this.
    pub origin: Origin,
}

/// The construct that introduced a local. This decides if removal is safe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// `local x = ...`
    Local,
    /// `local function f() end`
    LocalFunction,
    /// A function parameter. Larvae never removes it on its own.
    Param,
    /// A `for` variable. Larvae never removes it on its own.
    Loop,
}

/// The full set of facts that the walk learned about names in one file.
#[derive(Debug, Default)]
pub struct Names {
    pub bindings: Vec<Binding>,
    /// The token indexes of references that the source did not bind.
    pub globals: HashSet<u32>,
    /// Each name that appears anywhere. A generated name can then avoid all of them.
    pub taken: HashSet<String>,
    /*
    Names that a type mentions where the walk cannot say what they mean.

    A bare name in a type is a type: `type T = Entity` names an alias and
    not the local `Entity` beside it. A rename must leave such a name in
    place, because the token to change is unknown. A rule reads this set
    and skips those bindings.
    */
    pub type_blocked: HashSet<String>,
}

/// The token indexes of each name reference that no enclosing scope bound.
pub fn globals(ctx: &RuleCtx) -> HashSet<u32> {
    resolve(ctx).globals
}

/// Walk the file and find the referent of each name.
pub fn resolve(ctx: &RuleCtx) -> Names {
    let mut b = Binder {
        ctx,
        scopes: vec![Vec::new()],
        out: Names::default(),
    };

    b.block(&ctx.chunk.block);

    b.out
}

struct Binder<'a, 'src> {
    ctx: &'a RuleCtx<'src>,
    /// Each scope holds the names that it introduced and their binding indexes.
    scopes: Vec<Vec<(&'src str, usize)>>,
    out: Names,
}

impl<'src> Binder<'_, 'src> {
    /// The innermost binding of a name. A reference means this binding.
    fn lookup(&self, name: &str) -> Option<usize> {
        self.scopes
            .iter()
            .rev()
            .find_map(|s| s.iter().rev().find(|(n, _)| *n == name).map(|(_, i)| *i))
    }

    fn bind(&mut self, span: TokSpan, origin: Origin) {
        let name = self.ctx.tok_text(span.start);
        let index = self.out.bindings.len();

        self.out.bindings.push(Binding {
            declared_at: span.start,
            uses: Vec::new(),
            type_uses: Vec::new(),
            origin,
        });
        self.out.taken.insert(name.to_string());
        self.scopes
            .last_mut()
            .expect("a scope is open")
            .push((name, index));
    }

    /// Record a name reference against its binding, or record it as a global.
    fn reference(&mut self, span: TokSpan) {
        let name = self.ctx.tok_text(span.start);
        self.out.taken.insert(name.to_string());

        match self.lookup(name) {
            Some(index) => self.out.bindings[index].uses.push(span.start),

            None => {
                self.out.globals.insert(span.start);
            }
        }
    }

    /*
    Recover the value names that a type references.

    A token in a type falls into one of three groups.

    A name in front of a dot reads a value: `jecs.Component` reads the
    module and asks it for a type, and `Foo<jecs.Entity>` does the same
    inside a generic argument. A name inside `typeof(...)` reads a value
    too, because the parentheses hold an expression. Both groups record a
    use, so a rename reaches them.

    A name behind a dot is a member of the thing in front of it, so it
    names nothing this scope holds. The walk drops it.

    Every other name is a type: an alias, a generic parameter, or a
    builtin such as `string`. The walk cannot tell those apart from a
    local of the same name, so it blocks that name from a rename instead
    of guessing.
    */
    fn type_refs(&mut self, span: TokSpan) {
        let mut depth = 0usize;
        // The paren depth that the innermost open `typeof(` started from.
        let mut typeof_at: Option<usize> = None;
        let mut i = span.start;

        while i < span.end {
            let kind = self.ctx.toks[i as usize].kind;

            match kind {
                TokKind::LParen => depth += 1,

                TokKind::RParen => {
                    depth = depth.saturating_sub(1);

                    if typeof_at == Some(depth) {
                        typeof_at = None;
                    }
                }

                TokKind::Ident => {
                    let name = self.ctx.tok_text(i);
                    self.out.taken.insert(name.to_string());

                    if name == "typeof" && self.kind_at(i + 1) == Some(TokKind::LParen) {
                        typeof_at.get_or_insert(depth);
                        i += 1;
                        continue;
                    }

                    self.type_ident(span, i, typeof_at.is_some());
                }

                _ => {}
            }

            i += 1;
        }
    }

    /// Every name in the span blocks a rename. `type function` bodies use this.
    fn type_names_block(&mut self, span: TokSpan) {
        for i in span.start..span.end {
            if self.ctx.toks[i as usize].kind == TokKind::Ident {
                let name = self.ctx.tok_text(i);
                self.out.taken.insert(name.to_string());
                self.out.type_blocked.insert(name.to_string());
            }
        }
    }

    fn kind_at(&self, index: u32) -> Option<TokKind> {
        self.ctx.toks.get(index as usize).map(|t| t.kind)
    }

    /// Sort one name in a type into a use, a member, or a name to leave alone.
    fn type_ident(&mut self, span: TokSpan, index: u32, in_typeof: bool) {
        let prev = (index > span.start)
            .then(|| self.kind_at(index - 1))
            .flatten();

        // A member of the thing in front of the dot. It names nothing here.
        if prev == Some(TokKind::Dot) {
            return;
        }

        /*
        A name in front of a colon declares something: the field of a table
        type in `{ e: T }`, or the parameter of a function type in
        `(e: T) -> ()`. Neither reads a name, so neither blocks one.
        */
        if self.kind_at(index + 1) == Some(TokKind::Colon) {
            return;
        }

        /*
        `obj:method()` and `x :: T` both appear inside typeof. The name
        after either one is a method or a type, so only the name in front
        of a dot reads a value there.
        */
        let after_colon = prev == Some(TokKind::Colon)
            || (index > span.start && self.ctx.tok_text(index - 1) == "::");
        let reads_value =
            self.kind_at(index + 1) == Some(TokKind::Dot) || (in_typeof && !after_colon);
        let name = self.ctx.tok_text(index);

        if !reads_value {
            self.out.type_blocked.insert(name.to_string());
            return;
        }

        match self.lookup(name) {
            Some(binding) => self.out.bindings[binding].type_uses.push(index),

            /*
            A type alias is hoisted, so `type T = typeof(x)` can stand
            above the `local x` it reads. The walk has not bound the name
            yet at that point, so it blocks the name rather than miss the
            use and rename half of it.
            */
            None => {
                self.out.type_blocked.insert(name.to_string());
            }
        }
    }

    fn maybe_type(&mut self, span: &Option<TokSpan>) {
        if let Some(s) = span {
            self.type_refs(*s);
        }
    }

    fn open(&mut self) {
        self.scopes.push(Vec::new());
    }

    fn close(&mut self) {
        self.scopes.pop();
    }

    fn block(&mut self, b: &Block) {
        self.open();

        for s in &b.stmts {
            self.stmt(s);
        }

        self.close();
    }

    /// A function body owns its parameters, so they live in the body's scope.
    fn body(&mut self, f: &FunctionBody) {
        self.open();
        self.maybe_type(&f.generics);
        self.maybe_type(&f.ret_type);

        for p in &f.params {
            self.maybe_type(&p.ty);

            if !p.is_vararg {
                self.bind(p.name, Origin::Param);
            }
        }

        for s in &f.block.stmts {
            self.stmt(s);
        }

        self.close();
    }

    fn stmt(&mut self, s: &Stmt) {
        match s {
            Stmt::Empty(_) | Stmt::Break(_) | Stmt::Continue(_) | Stmt::Declare(_) => {}

            /*
            `type Foo = Types.Foo` reads the local `Types`.

            A `type function f() ... end` holds Luau that runs at check
            time, and that code cannot see a runtime local. A name in there
            that matches one is a different name, so the whole body blocks
            instead of renaming.
            */
            Stmt::TypeAlias(n) => {
                let is_type_function =
                    n.name.start > 0 && self.ctx.tok_text(n.name.start - 1) == "function";

                if is_type_function {
                    self.type_names_block(n.span);
                } else {
                    self.type_refs(n.span);
                }
            }

            // The walk reads the values before the names exist. `local x = x` sees the outer x.
            Stmt::Local(n) => {
                for e in &n.values {
                    self.expr(e);
                }

                for name in &n.names {
                    self.maybe_type(&name.ty);
                    self.bind(name.name, Origin::Local);
                }
            }

            // The name binds first, so the function can call itself.
            Stmt::LocalFunction(n) => {
                self.bind(n.name, Origin::LocalFunction);
                self.body(&n.body);
            }

            Stmt::Function(n) => self.body(&n.body),

            Stmt::Class(n) => {
                // The base name is a reference, or a rename would strand it.
                if let Some(base) = n.extends {
                    self.reference(base);
                }

                self.bind(n.name, Origin::Local);

                for member in &n.members {
                    match member {
                        ClassMember::Field { ty, .. } => self.maybe_type(ty),

                        ClassMember::Method(f) => self.body(&f.body),
                    }
                }
            }

            Stmt::Assign(n) => {
                for e in &n.targets {
                    self.expr(e);
                }

                for e in &n.values {
                    self.expr(e);
                }
            }

            Stmt::Call(e, _) => self.expr(e),

            Stmt::Do(n) => self.block(&n.block),

            Stmt::While(n) => {
                self.expr(&n.cond);
                self.block(&n.block);
            }

            // A repeat statement sees the locals of the loop body in its until condition.
            Stmt::Repeat(n) => {
                self.open();

                for s in &n.block.stmts {
                    self.stmt(s);
                }

                self.expr(&n.cond);
                self.close();
            }

            Stmt::If(n) => {
                for (cond, body) in &n.branches {
                    self.expr(cond);
                    self.block(body);
                }

                if let Some(e) = &n.else_block {
                    self.block(e);
                }
            }

            Stmt::NumericFor(n) => {
                self.expr(&n.start);
                self.expr(&n.limit);

                if let Some(step) = &n.step {
                    self.expr(step);
                }

                self.open();
                self.maybe_type(&n.var.ty);
                self.bind(n.var.name, Origin::Loop);

                for s in &n.block.stmts {
                    self.stmt(s);
                }

                self.close();
            }

            Stmt::GenericFor(n) => {
                for e in &n.exprs {
                    self.expr(e);
                }

                self.open();

                for v in &n.vars {
                    self.maybe_type(&v.ty);
                    self.bind(v.name, Origin::Loop);
                }

                for s in &n.block.stmts {
                    self.stmt(s);
                }

                self.close();
            }

            Stmt::Return(n) => {
                for e in &n.values {
                    self.expr(e);
                }
            }
        }
    }

    fn expr(&mut self, e: &Expr) {
        match e {
            Expr::Name(span) => self.reference(*span),

            Expr::Nil(_)
            | Expr::True(_)
            | Expr::False(_)
            | Expr::Vararg(_)
            | Expr::Number(_)
            | Expr::String(_)
            | Expr::InterpString(_) => {}

            Expr::Function { body, .. } => self.body(body),

            Expr::Table { fields, .. } => {
                for f in fields {
                    match f {
                        TableField::Positional(e) => self.expr(e),

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

            // A field key is not a name reference. `t.print` gives no fact about print.
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
                self.maybe_type(type_args);

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
                for (c, val) in branches {
                    self.expr(c);
                    self.expr(val);
                }

                self.expr(else_value);
            }

            Expr::TypeAssert { expr, ty, .. } => {
                self.expr(expr);
                self.type_refs(*ty);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax::{lexer, parser};

    /// The names that the walk marked as global, in source order.
    fn found(src: &str) -> Vec<String> {
        let lexed = lexer::lex(src).unwrap();
        let chunk = parser::parse(src, &lexed.toks).unwrap();
        let ctx = RuleCtx {
            src,
            toks: &lexed.toks,
            chunk: &chunk,
            comments: &lexed.comments,
            require_forms: &[],
            dm_path: None,
            quote: '"',
            defines: &Default::default(),
            globals: &Default::default(),
        };

        let mut idx: Vec<u32> = globals(&ctx).into_iter().collect();
        idx.sort_unstable();

        idx.iter().map(|i| ctx.tok_text(*i).to_string()).collect()
    }

    #[test]
    fn plain_globals() {
        assert_eq!(found("return DEBUG"), ["DEBUG"]);
        assert_eq!(found("print(DEBUG)"), ["print", "DEBUG"]);
    }

    #[test]
    fn locals_shadow() {
        assert!(found("local DEBUG = 1\nreturn DEBUG").is_empty());
        assert!(found("local function f(DEBUG) return DEBUG end").is_empty());
        assert!(found("for DEBUG = 1, 2 do return DEBUG end").is_empty());
        assert!(found("for _, DEBUG in t do return DEBUG end").contains(&"t".to_string()));
    }

    #[test]
    fn a_local_sees_the_outer_name_on_its_own_right_hand_side() {
        assert_eq!(found("local x = x"), ["x"]);
    }

    #[test]
    fn shadowing_ends_with_the_block() {
        assert_eq!(found("do local DEBUG = 1 end\nreturn DEBUG"), ["DEBUG"]);
    }

    #[test]
    fn a_field_is_not_a_name_reference() {
        assert_eq!(found("return t.DEBUG"), ["t"]);
        assert_eq!(found("return { DEBUG = 1 }"), Vec::<String>::new());
    }

    #[test]
    fn repeat_sees_its_body_in_the_condition() {
        assert!(found("repeat local ok = f() until ok").contains(&"f".to_string()));
        assert!(!found("repeat local ok = 1 until ok").contains(&"ok".to_string()));
    }
}
