/*!
freeze_module: wrap the return value of a module in `table.freeze`. Then
an accidental write from another script fails with an error and does not
corrupt shared state.

A wrongly frozen module is a runtime error in a user's game. For this
reason, the rule applies only to the two shapes where no code can write
to the table later. A metatable pattern, a rebind, or a field write turns
the rule off.
*/

use crate::rules::engine::{self, Edit, Flow, RuleCtx, Visit};
use crate::rules::native::{dotted_path, name_text};
use crate::syntax::ast::{CallArgs, Expr, IndexKey, Stmt};

pub fn apply(ctx: &RuleCtx, edits: &mut Vec<Edit>) {
    let stmts = &ctx.chunk.block.stmts;
    // A trailing `;` is still a statement. The return is the target.
    let Some(Stmt::Return(ret)) = stmts.iter().rev().find(|s| !matches!(s, Stmt::Empty(_))) else {
        return;
    };

    if ret.values.len() != 1 {
        return;
    }

    let value = &ret.values[0];

    match value {
        // The module returns the literal directly, so no other code can reach it.
        Expr::Table { span, .. } => {
            let (start, end) = ctx.bytes(*span);
            edits.push((start, start, "table.freeze(".to_string()));
            edits.push((end, end, ")".to_string()));
        }

        Expr::Name(name_span) => {
            let name = name_text(ctx, *name_span);

            if !defined_once_as_table(ctx, name) || is_touched(ctx, name) {
                return;
            }

            ctx.replace(*name_span, format!("table.freeze({name})"), edits);
        }

        _ => {}
    }
}

/// True when the name has exactly one top level definition, and it is a table literal.
fn defined_once_as_table(ctx: &RuleCtx, name: &str) -> bool {
    let mut found = false;

    for stmt in &ctx.chunk.block.stmts {
        let Stmt::Local(local) = stmt else {
            continue;
        };

        if !local.names.iter().any(|b| name_text(ctx, b.name) == name) {
            continue;
        }

        // The name has a second definition, so the returned value can differ from the literal.
        if found {
            return false;
        }

        if local.names.len() != 1
            || local.values.len() != 1
            || !matches!(local.values[0], Expr::Table { .. })
        {
            return false;
        }

        found = true;
    }

    found
}

/// True when code writes into the table or gives it to a metatable.
fn is_touched(ctx: &RuleCtx, name: &str) -> bool {
    let mut watch = Watch {
        ctx,
        name,
        touched: false,
    };

    engine::walk_chunk(ctx.chunk, &mut watch);

    watch.touched
}

struct Watch<'a, 'src> {
    ctx: &'a RuleCtx<'src>,
    name: &'a str,
    touched: bool,
}

impl Watch<'_, '_> {
    /// True when the expression is the name, or an index chain rooted at the name.
    fn rooted_at_name(&self, expr: &Expr) -> bool {
        match expr {
            Expr::Name(span) => name_text(self.ctx, *span) == self.name,

            Expr::Index { object, .. } => self.rooted_at_name(object),

            Expr::Paren { inner, .. } => self.rooted_at_name(inner),

            _ => false,
        }
    }

    fn mentions_name(&self, expr: &Expr) -> bool {
        struct Mentions<'b, 'c, 'src> {
            watch: &'b Watch<'c, 'src>,
            found: bool,
        }

        impl Visit for Mentions<'_, '_, '_> {
            fn expr(&mut self, expr: &Expr) -> Flow {
                if let Expr::Name(span) = expr
                    && name_text(self.watch.ctx, *span) == self.watch.name
                {
                    self.found = true;
                }

                Flow::Next
            }
        }

        let mut mentions = Mentions {
            watch: self,
            found: false,
        };

        engine::walk_expr(expr, &mut mentions);

        mentions.found
    }
}

impl Visit for Watch<'_, '_> {
    fn stmt(&mut self, stmt: &Stmt) -> Flow {
        match stmt {
            // `M = x`, `M.field = x`, and `M[k] = x` all break under a freeze.
            Stmt::Assign(assign) => {
                if assign.targets.iter().any(|t| self.rooted_at_name(t)) {
                    self.touched = true;
                }
            }

            // `function M.foo()` is a field write in a different form.
            Stmt::Function(f) => {
                self.touched |= f
                    .path
                    .first()
                    .is_some_and(|p| name_text(self.ctx, *p) == self.name);
            }

            _ => {}
        }

        Flow::Next
    }

    fn expr(&mut self, expr: &Expr) -> Flow {
        match expr {
            // Metatable patterns. A freeze of either side changes their behavior.
            Expr::Call { func, args, .. } => {
                let is_setmetatable =
                    matches!(dotted_path(self.ctx, func).as_deref(), Some("setmetatable"));
                if is_setmetatable
                    && let CallArgs::Paren(list) = args
                    && list.iter().any(|a| self.mentions_name(a))
                {
                    self.touched = true;
                }
            }

            Expr::Index {
                object,
                key: IndexKey::Field(field),
                ..
            } => {
                self.touched |=
                    name_text(self.ctx, *field) == "__index" && self.rooted_at_name(object);
            }

            _ => {}
        }

        Flow::Next
    }
}

#[cfg(test)]
mod tests {
    use crate::rules::native::test_support::run;

    const ON: &str = "freeze_module = true";

    #[test]
    fn freezes_provably_safe_modules() {
        // A literal returned directly.
        assert_eq!(
            run(ON, "return {\n    a = 1,\n}\n"),
            "return table.freeze({\n    a = 1,\n})\n"
        );
        // A local table literal that no code writes to.
        assert_eq!(
            run(ON, "local M = {\n    a = 1,\n}\nreturn M\n"),
            "local M = {\n    a = 1,\n}\nreturn table.freeze(M)\n"
        );
        // A field read is safe.
        assert_eq!(
            run(ON, "local M = { a = 1 }\nprint(M.a)\nreturn M\n"),
            "local M = { a = 1 }\nprint(M.a)\nreturn table.freeze(M)\n"
        );
    }

    #[test]
    fn leaves_anything_writable_alone() {
        // The common method table. `function M.f` writes into M.
        let methods = "local M = {}\nfunction M.f() end\nreturn M\n";
        assert_eq!(run(ON, methods), methods);
        // A field assignment, at any depth.
        let field = "local M = {}\nM.count = 0\nreturn M\n";
        assert_eq!(run(ON, field), field);
        let nested = "local M = {}\ndo\n    M[1] = true\nend\nreturn M\n";
        assert_eq!(run(ON, nested), nested);
        // A rebind.
        let rebound = "local M = {}\nM = {}\nreturn M\n";
        assert_eq!(run(ON, rebound), rebound);
        // Metatable patterns.
        let meta = "local M = {}\nM.__index = M\nreturn M\n";
        assert_eq!(run(ON, meta), meta);
        let setmeta = "local M = {}\nlocal o = setmetatable({}, M)\nreturn M\n";
        assert_eq!(run(ON, setmeta), setmeta);
        // Not a table literal, and not defined in this file.
        let call = "local M = require(\"./x\")\nreturn M\n";
        assert_eq!(run(ON, call), call);
        let global = "return M\n";
        assert_eq!(run(ON, global), global);
        // Multiple return values.
        let two = "local M = {}\nreturn M, 1\n";
        assert_eq!(run(ON, two), two);
    }
}
