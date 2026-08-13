/*!
inject_module_path: define a constant that holds the DataModel path of
this file. Then logging and telemetry can name the module without a
hardcoded path. A hardcoded path becomes stale when the file moves.

The constant appears only in files that reference it, and only when a
mount covers the file. A reference with no known path produces a warning.
The rule does not guess a path.
*/

use std::path::Path;

use crate::diag::Diag;
use crate::requires::resolve::lua_quote;
use crate::rules::engine::{self, Edit, Flow, RuleCtx, Visit};
use crate::rules::native::name_text;
use crate::syntax::ast::{Expr, Stmt, TokSpan};

pub fn apply(name: &str, ctx: &RuleCtx, edits: &mut Vec<Edit>, diags: &mut Vec<Diag>, path: &Path) {
    let mut usage = Usage {
        ctx,
        name,
        referenced: false,
        bound: false,
    };

    engine::walk_chunk(ctx.chunk, &mut usage);
    // The file already owns the name. The injected local would only shadow or be shadowed.
    if !usage.referenced || usage.bound {
        return;
    }

    let Some(dm_path) = ctx.dm_path else {
        diags.push(Diag::warning(
            path,
            format!(
                "{name} is referenced but no mount covers this file, so its DataModel path is unknown"
            ),
        ).with_help("add it to [requires.mounts] or mount it in the Rojo project file"));

        return;
    };

    let at = insert_at(ctx);
    edits.push((
        at,
        at,
        format!("local {name} = {}\n", lua_quote(dm_path, ctx.quote)),
    ));
}

/*
The constant goes after the leading directives and comments. The first
real token marks exactly that boundary. The rule uses the token's line
start when no other content shares the line.
*/
fn insert_at(ctx: &RuleCtx) -> u32 {
    let Some(first) = ctx.toks.first() else {
        return ctx.src.len() as u32;
    };

    let line_start = match ctx.src[..first.start as usize].rfind('\n') {
        Some(i) => i as u32 + 1,

        None => 0,
    };

    if ctx.src[line_start as usize..first.start as usize]
        .bytes()
        .all(|b| b == b' ' || b == b'\t')
    {
        line_start
    } else {
        first.start
    }
}

struct Usage<'a, 'src> {
    ctx: &'a RuleCtx<'src>,
    name: &'a str,
    referenced: bool,
    bound: bool,
}

impl Usage<'_, '_> {
    fn mark(&mut self, span: TokSpan) {
        if name_text(self.ctx, span) == self.name {
            self.bound = true;
        }
    }
}

impl Visit for Usage<'_, '_> {
    fn stmt(&mut self, stmt: &Stmt) -> Flow {
        match stmt {
            Stmt::Local(local) => {
                for binding in &local.names {
                    self.mark(binding.name);
                }
            }

            Stmt::LocalFunction(f) => {
                self.mark(f.name);

                for param in &f.body.params {
                    self.mark(param.name);
                }
            }

            Stmt::Function(f) => {
                if let Some(first) = f.path.first() {
                    self.mark(*first);
                }

                for param in &f.body.params {
                    self.mark(param.name);
                }
            }

            Stmt::NumericFor(n) => self.mark(n.var.name),

            Stmt::GenericFor(n) => {
                for var in &n.vars {
                    self.mark(var.name);
                }
            }

            _ => {}
        }

        Flow::Next
    }

    fn expr(&mut self, expr: &Expr) -> Flow {
        match expr {
            Expr::Name(span) => {
                if name_text(self.ctx, *span) == self.name {
                    self.referenced = true;
                }
            }

            Expr::Function { body, .. } => {
                for param in &body.params {
                    self.mark(param.name);
                }
            }

            _ => {}
        }

        Flow::Next
    }
}

#[cfg(test)]
mod tests {
    use crate::diag::Severity;
    use crate::rules::native::test_support::{run, run_full};

    const ON: &str = "inject_module_path = \"MODULE_PATH\"";
    const DM: Option<&str> = Some("@game/ReplicatedStorage/shared/util");

    #[test]
    fn injects_when_the_name_is_used() {
        assert_eq!(
            run_full(ON, "print(MODULE_PATH)\n", DM, &mut Vec::new()),
            "local MODULE_PATH = \"@game/ReplicatedStorage/shared/util\"\nprint(MODULE_PATH)\n"
        );
        // The constant goes after the directives and the header comments.
        assert_eq!(
            run_full(
                ON,
                "--!strict\n-- header\nreturn MODULE_PATH\n",
                DM,
                &mut Vec::new()
            ),
            "--!strict\n-- header\nlocal MODULE_PATH = \"@game/ReplicatedStorage/shared/util\"\nreturn MODULE_PATH\n"
        );
    }

    #[test]
    fn stays_out_of_files_that_do_not_use_it() {
        let src = "local a = 1\nreturn a\n";
        assert_eq!(run_full(ON, src, DM, &mut Vec::new()), src);
        // The file defines the name itself. The injected local would only be shadowed.
        let owned = "local MODULE_PATH = \"custom\"\nreturn MODULE_PATH\n";
        assert_eq!(run_full(ON, owned, DM, &mut Vec::new()), owned);
    }

    #[test]
    fn warns_when_the_path_is_unknown() {
        let mut diags = Vec::new();
        let src = "return MODULE_PATH\n";

        assert_eq!(run_full(ON, src, None, &mut diags), src);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].severity, Severity::Warning);
        assert!(diags[0].message.contains("no mount covers this file"));
        // With no reference, there is no warning.
        let mut quiet = Vec::new();
        run_full(ON, "return 1\n", None, &mut quiet);
        assert!(quiet.is_empty());
    }

    #[test]
    fn off_by_default() {
        let src = "return MODULE_PATH\n";
        assert_eq!(run("", src), src);
    }
}
