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
*/

use std::collections::HashSet;

use crate::rules::engine::RuleCtx;
use crate::syntax::ast::*;

/// One local, the point of its declaration, and each point where the code reads it.
#[derive(Debug)]
pub struct Binding {
    /// The token index of the name in its declaration.
    pub declared_at: u32,
    /// The token indexes of each reference to it. A recursive call counts.
    pub uses: Vec<u32>,
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

        for p in &f.params {
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
            Stmt::Empty(_) | Stmt::Break(_) | Stmt::Continue(_) | Stmt::TypeAlias(_) => {}

            // The walk reads the values before the names exist. `local x = x` sees the outer x.
            Stmt::Local(n) => {
                for e in &n.values {
                    self.expr(e);
                }

                for name in &n.names {
                    self.bind(name.name, Origin::Local);
                }
            }

            // The name binds first, so the function can call itself.
            Stmt::LocalFunction(n) => {
                self.bind(n.name, Origin::LocalFunction);
                self.body(&n.body);
            }

            Stmt::Function(n) => self.body(&n.body),

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

            Expr::Call { func, args, .. } => {
                self.expr(func);

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

            Expr::TypeAssert { expr, .. } => self.expr(expr),
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
