/*!
dedupe_requires: when two top level `local X = require(...)` statements
point at the same module, the rule folds them. The later local becomes an
alias of the first local.

An alias is better than a rename of each use. The first local is always
in scope at top level, so the rewrite stays local to one statement.
Require caching means that the second call did no work. Only the lookup
cost goes away.
*/

use std::collections::HashMap;

use crate::rules::engine::{Edit, RuleCtx};
use crate::rules::native::{name_text, newlines_in};
use crate::syntax::ast::{Expr, Stmt};

pub fn apply(ctx: &RuleCtx, edits: &mut Vec<Edit>) {
    let mut first: HashMap<&str, &str> = HashMap::new();

    for stmt in &ctx.chunk.block.stmts {
        let Some((name, value)) = simple_require_local(ctx, stmt) else {
            continue;
        };

        let (start, end) = ctx.bytes(value.span());
        // The site inside this value carries the form that the rewriter selected.
        let Some((_, form)) = ctx
            .require_forms
            .iter()
            .find(|(site, _)| site.tok_start >= start && site.tok_end <= end)
        else {
            continue;
        };

        let Some(&kept) = first.get(form.as_str()) else {
            first.insert(form.as_str(), name);
            continue;
        };

        // A second local with the same name would alias itself. Keep it.
        if kept == name {
            continue;
        }

        let padding = "\n".repeat(newlines_in(ctx, start, end));
        edits.push((start, end, format!("{kept}{padding}")));
    }
}

/*
Match `local NAME = require(...)` and nothing else. A type annotation or
a second binding changes the meaning of the alias. A const would make the
alias a constant of a value that is not constant.
*/
fn simple_require_local<'a, 'src>(
    ctx: &'a RuleCtx<'src>,
    stmt: &'a Stmt,
) -> Option<(&'src str, &'a Expr)> {
    let Stmt::Local(local) = stmt else {
        return None;
    };

    if local.is_const || local.names.len() != 1 || local.values.len() != 1 {
        return None;
    }

    let binding = &local.names[0];

    if binding.ty.is_some() {
        return None;
    }

    let value = &local.values[0];
    let Expr::Call {
        func, method: None, ..
    } = value
    else {
        return None;
    };

    if !matches!(&**func, Expr::Name(n) if name_text(ctx, *n) == "require") {
        return None;
    }

    Some((name_text(ctx, binding.name), value))
}

#[cfg(test)]
mod tests {
    use crate::rules::native::test_support::run;

    const ON: &str = "dedupe_requires = true";

    #[test]
    fn later_duplicates_alias_the_first() {
        assert_eq!(
            run(
                ON,
                "local A = require(\"@pkg/x\")\nlocal B = require(\"@pkg/x\")\nreturn A, B\n"
            ),
            "local A = require(\"@pkg/x\")\nlocal B = A\nreturn A, B\n"
        );
        // All three fold onto the first.
        assert_eq!(
            run(
                ON,
                "local A = require(\"./x\")\nlocal B = require(\"./x\")\nlocal C = require(\"./x\")\n"
            ),
            "local A = require(\"./x\")\nlocal B = A\nlocal C = A\n"
        );
        // A multi line duplicate keeps its line count.
        assert_eq!(
            run(
                ON,
                "local A = require(\"./x\")\nlocal B = require(\n\"./x\"\n)\nreturn B\n"
            ),
            "local A = require(\"./x\")\nlocal B = A\n\n\nreturn B\n"
        );
    }

    #[test]
    fn leaves_everything_it_cannot_prove_alone() {
        // Different modules.
        let two = "local A = require(\"./x\")\nlocal B = require(\"./y\")\n";
        assert_eq!(run(ON, two), two);
        // Not top level.
        let nested = "local A = require(\"./x\")\ndo\n    local B = require(\"./x\")\nend\n";
        assert_eq!(run(ON, nested), nested);
        // Annotated, const, and multi binding forms.
        let annotated = "local A = require(\"./x\")\nlocal B: any = require(\"./x\")\n";
        assert_eq!(run(ON, annotated), annotated);
        let konst = "local A = require(\"./x\")\nconst B = require(\"./x\")\n";
        assert_eq!(run(ON, konst), konst);
        let multi = "local A = require(\"./x\")\nlocal B, C = require(\"./x\"), 1\n";
        assert_eq!(run(ON, multi), multi);
        // The same name twice. An alias would have no effect.
        let shadow = "local A = require(\"./x\")\nlocal A = require(\"./x\")\n";
        assert_eq!(run(ON, shadow), shadow);
    }
}
