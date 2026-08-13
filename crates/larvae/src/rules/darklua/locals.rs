/*!
Rules that need the use of each local

Both rules use the scope walk and not the tree alone. For this reason,
they waited on the scope walk. The rules are conservative where it
matters. A local whose value can have a side effect stays in place.
*/

use std::collections::HashMap;

use crate::rules::engine::{Edit, RuleCtx};
use crate::rules::scope::{self, Origin};
use crate::syntax::ast::*;

/*
remove_unused_variable: remove locals that no code reads.

The rule removes only whole statements. A partly removed `local a, b`
would change the number of values that the right hand side supplies. The
declaration must also be inert. `local x = f()` must still call f, even
when no code reads the result.
*/
pub fn remove_unused_variable(ctx: &RuleCtx, edits: &mut Vec<Edit>) {
    let names = scope::resolve(ctx);
    let dead: Vec<u32> = names
        .bindings
        .iter()
        .filter(|b| b.uses.is_empty())
        .map(|b| b.declared_at)
        .collect();

    if dead.is_empty() {
        return;
    }

    let mut v = Sweeper {
        ctx,
        dead,
        edits,
        origins: names
            .bindings
            .iter()
            .map(|b| (b.declared_at, b.origin))
            .collect(),
    };

    walk(ctx.chunk, &mut v);
}

struct Sweeper<'a, 'b> {
    ctx: &'a RuleCtx<'b>,
    dead: Vec<u32>,
    origins: HashMap<u32, Origin>,
    edits: &'a mut Vec<Edit>,
}

impl Sweeper<'_, '_> {
    fn unused(&self, span: TokSpan) -> bool {
        self.dead.contains(&span.start)
    }

    fn stmt(&mut self, s: &Stmt) {
        match s {
            Stmt::Local(n) => {
                let all_dead = n.names.iter().all(|b| self.unused(b.name))
                    && n.names
                        .iter()
                        .all(|b| self.origins.get(&b.name.start) == Some(&Origin::Local));

                if all_dead && n.values.iter().all(inert) {
                    self.ctx.delete_keep_lines(n.span, self.edits);
                }
            }

            // A function definition alone has no effect, so removal is always safe.
            Stmt::LocalFunction(n) if self.unused(n.name) => {
                self.ctx.delete_keep_lines(n.span, self.edits);
            }

            _ => {}
        }
    }
}

/*
rename_variables: give each local a short name.

Each binding gets its own name. The rule does not reuse a name across
scopes that do not overlap. The output is a little longer than darklua's
output, but the rule needs no shadowing analysis to be correct. The
difference only matters when the dense generator exists to make short
names useful.

The rule does not rename a name that appears anywhere in a type. Larvae
keeps types as raw spans, so `typeof(x)` would still hold the old name
after the rename.
*/
pub fn rename_variables(ctx: &RuleCtx, edits: &mut Vec<Edit>) {
    let names = scope::resolve(ctx);
    let in_types = type_text(ctx);
    let mut supply = Supply::new(&names.taken);

    for binding in &names.bindings {
        let old = ctx.tok_text(binding.declared_at);

        // A vararg has no name to take, and Luau passes self implicitly.
        if old == "self" || in_types.contains(old) {
            continue;
        }

        let Some(new) = supply.next_name() else {
            return;
        };

        for at in std::iter::once(&binding.declared_at).chain(binding.uses.iter()) {
            let tok = &ctx.toks[*at as usize];
            edits.push((tok.start, tok.end, new.clone()));
        }
    }
}

/// Short names in order. The supply skips each name that the file already uses.
struct Supply<'a> {
    taken: &'a std::collections::HashSet<String>,
    counter: usize,
}

impl<'a> Supply<'a> {
    fn new(taken: &'a std::collections::HashSet<String>) -> Self {
        Self { taken, counter: 0 }
    }

    fn next_name(&mut self) -> Option<String> {
        const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ";

        // After a few million names, the state is wrong. Stop without a report.
        while self.counter < 5_000_000 {
            let mut n = self.counter;
            self.counter += 1;
            let mut name = String::new();

            loop {
                name.push(ALPHABET[n % ALPHABET.len()] as char);
                n /= ALPHABET.len();

                if n == 0 {
                    break;
                }

                n -= 1;
            }

            // is_ident rejects the keywords, so the supply never emits `do` or `end`.
            if !self.taken.contains(&name) && crate::rules::native::is_ident(&name) {
                return Some(name);
            }
        }

        None
    }
}

/// Each identifier that appears inside a type annotation anywhere in the file.
fn type_text<'src>(ctx: &RuleCtx<'src>) -> std::collections::HashSet<&'src str> {
    let mut found = std::collections::HashSet::new();
    let mut v = TypeScan {
        ctx,
        found: &mut found,
    };

    walk_types(ctx.chunk, &mut v);

    found
}

struct TypeScan<'a, 'b> {
    ctx: &'a RuleCtx<'b>,
    found: &'a mut std::collections::HashSet<&'b str>,
}

impl<'b> TypeScan<'_, 'b> {
    fn span(&mut self, span: TokSpan) {
        for i in span.start..span.end {
            self.found.insert(self.ctx.tok_text(i));
        }
    }

    fn maybe(&mut self, span: &Option<TokSpan>) {
        if let Some(s) = span {
            self.span(*s);
        }
    }

    fn body(&mut self, f: &FunctionBody) {
        self.maybe(&f.generics);
        self.maybe(&f.ret_type);

        for p in &f.params {
            self.maybe(&p.ty);
        }

        self.block(&f.block);
    }

    fn block(&mut self, b: &Block) {
        for s in &b.stmts {
            self.stmt(s);
        }
    }

    fn stmt(&mut self, s: &Stmt) {
        match s {
            // A whole alias is type text. This includes each name that typeof reads.
            Stmt::TypeAlias(n) => self.span(n.span),

            Stmt::Local(n) => {
                for name in &n.names {
                    self.maybe(&name.ty);
                }

                for e in &n.values {
                    self.expr(e);
                }
            }

            Stmt::LocalFunction(n) => self.body(&n.body),

            Stmt::Function(n) => self.body(&n.body),

            Stmt::Assign(n) => {
                for e in n.targets.iter().chain(n.values.iter()) {
                    self.expr(e);
                }
            }

            Stmt::Call(e, _) => self.expr(e),

            Stmt::Do(n) => self.block(&n.block),

            Stmt::While(n) => {
                self.expr(&n.cond);
                self.block(&n.block);
            }

            Stmt::Repeat(n) => {
                self.block(&n.block);
                self.expr(&n.cond);
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
                self.maybe(&n.var.ty);
                self.block(&n.block);
            }

            Stmt::GenericFor(n) => {
                for v in &n.vars {
                    self.maybe(&v.ty);
                }

                self.block(&n.block);
            }

            Stmt::Return(n) => {
                for e in &n.values {
                    self.expr(e);
                }
            }

            _ => {}
        }
    }

    fn expr(&mut self, e: &Expr) {
        match e {
            Expr::TypeAssert { expr, ty, .. } => {
                self.span(*ty);
                self.expr(expr);
            }

            Expr::Function { body, .. } => self.body(body),

            Expr::Paren { inner, .. } => self.expr(inner),

            Expr::Unary { operand, .. } => self.expr(operand),

            Expr::Binary { lhs, rhs, .. } => {
                self.expr(lhs);
                self.expr(rhs);
            }

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

            _ => {}
        }
    }
}

fn walk_types(chunk: &Chunk, v: &mut TypeScan) {
    v.block(&chunk.block);
}

/// True when evaluation of the expression cannot have an observable effect.
fn inert(e: &Expr) -> bool {
    match e {
        Expr::Nil(_)
        | Expr::True(_)
        | Expr::False(_)
        | Expr::Number(_)
        | Expr::String(_)
        | Expr::Vararg(_)
        | Expr::Name(_)
        | Expr::Function { .. } => true,

        Expr::Paren { inner, .. } => inert(inner),

        Expr::Unary { operand, .. } => inert(operand),

        Expr::Binary { lhs, rhs, .. } => inert(lhs) && inert(rhs),

        Expr::Table { fields, .. } => fields.iter().all(|f| match f {
            TableField::Positional(e) => inert(e),

            TableField::Named { value, .. } => inert(value),

            TableField::Computed { key, value } => inert(key) && inert(value),
        }),

        Expr::TypeAssert { expr, .. } => inert(expr),

        // An index runs __index. A call runs any code. Interpolation runs tostring.
        Expr::Index { .. } | Expr::Call { .. } | Expr::InterpString(_) | Expr::IfElse { .. } => {
            false
        }
    }
}

/// A statement walk that reaches each nested block.
fn walk(chunk: &Chunk, v: &mut Sweeper) {
    walk_block(&chunk.block, v);
}

fn walk_block(b: &Block, v: &mut Sweeper) {
    for s in &b.stmts {
        v.stmt(s);
        descend(s, v);
    }
}

fn descend(s: &Stmt, v: &mut Sweeper) {
    match s {
        Stmt::Do(n) => walk_block(&n.block, v),

        Stmt::While(n) => walk_block(&n.block, v),

        Stmt::Repeat(n) => walk_block(&n.block, v),

        Stmt::NumericFor(n) => walk_block(&n.block, v),

        Stmt::GenericFor(n) => walk_block(&n.block, v),

        Stmt::Function(n) => walk_block(&n.body.block, v),

        Stmt::LocalFunction(n) => walk_block(&n.body.block, v),

        Stmt::If(n) => {
            for (_, body) in &n.branches {
                walk_block(body, v);
            }

            if let Some(e) = &n.else_block {
                walk_block(e, v);
            }
        }

        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::darklua::testing::{assert_lines_kept, run};

    fn sweep(src: &str) -> String {
        let out = run(src, remove_unused_variable);
        assert_lines_kept(src, &out);

        out
    }

    #[test]
    fn drops_a_local_nothing_reads() {
        assert_eq!(sweep("local x = 1\nreturn 2\n"), "\nreturn 2\n");
    }

    #[test]
    fn keeps_one_that_is_read() {
        assert_eq!(sweep("local x = 1\nreturn x\n"), "local x = 1\nreturn x\n");
    }

    #[test]
    fn keeps_a_declaration_that_does_something() {
        // f must still run.
        assert_eq!(
            sweep("local x = f()\nreturn 1\n"),
            "local x = f()\nreturn 1\n"
        );
        // The metatable behind an index must also run.
        assert_eq!(
            sweep("local x = t.k\nreturn 1\n"),
            "local x = t.k\nreturn 1\n"
        );
    }

    #[test]
    fn leaves_a_partly_used_multi_binding() {
        // Removal of half would change the number of values that the right side supplies.
        assert_eq!(
            sweep("local a, b = 1, 2\nreturn a\n"),
            "local a, b = 1, 2\nreturn a\n"
        );
    }

    #[test]
    fn drops_an_unused_local_function() {
        assert_eq!(sweep("local function f() end\nreturn 1\n"), "\nreturn 1\n");
    }

    #[test]
    fn keeps_params_and_loop_variables() {
        assert_eq!(
            sweep("local function f(a) return 1 end\nreturn f\n"),
            "local function f(a) return 1 end\nreturn f\n"
        );
        assert_eq!(
            sweep("for i = 1, 3 do print(1) end\n"),
            "for i = 1, 3 do print(1) end\n"
        );
    }

    #[test]
    fn reaches_into_nested_blocks() {
        // The indent stays behind, the same as each other rule that deletes.
        assert_eq!(
            sweep("do\n    local x = 1\nend\nreturn 2\n"),
            "do\n    \nend\nreturn 2\n"
        );
    }

    #[test]
    fn a_recursive_call_counts_as_a_use() {
        let src = "local function f() return f() end\nreturn 1\n";
        assert_eq!(sweep(src), src);
    }
}

#[cfg(test)]
mod rename_tests {
    use super::*;
    use crate::rules::darklua::testing::{assert_lines_kept, run};

    fn rename(src: &str) -> String {
        let out = run(src, rename_variables);
        assert_lines_kept(src, &out);

        out
    }

    #[test]
    fn locals_get_short_names() {
        assert_eq!(
            rename("local counter = 1\nreturn counter\n"),
            "local a = 1\nreturn a\n"
        );
    }

    #[test]
    fn every_binding_gets_its_own_name() {
        assert_eq!(
            rename("local one = 1\nlocal two = 2\nreturn one + two\n"),
            "local a = 1\nlocal b = 2\nreturn a + b\n"
        );
    }

    #[test]
    fn globals_are_never_touched() {
        assert_eq!(
            rename("local x = print\nreturn print(x)\n"),
            "local a = print\nreturn print(a)\n"
        );
    }

    #[test]
    fn a_name_the_file_already_uses_is_not_handed_out() {
        // `a` is a global here, so no local can take it.
        let out = rename("local counter = a\nreturn counter\n");
        assert!(out.contains("= a\n"), "{out}");
        assert!(!out.starts_with("local a ="), "{out}");
    }

    #[test]
    fn params_and_loop_variables_rename_too() {
        assert_eq!(
            rename("local function f(value) return value end\nreturn f\n"),
            "local function a(b) return b end\nreturn a\n"
        );
    }

    #[test]
    fn a_name_used_in_a_type_is_left_alone() {
        // typeof would still hold the old name, because types are raw spans.
        let src = "local config = {}\ntype T = typeof(config)\nreturn config\n";
        assert_eq!(rename(src), src);
    }

    #[test]
    fn shadowing_still_resolves_to_the_inner_one() {
        assert_eq!(
            rename("local x = 1\ndo local x = 2\nreturn x end\n"),
            "local a = 1\ndo local b = 2\nreturn b end\n"
        );
    }
}
