/*!
Lints for a conditional that stands where a value belongs.

Luau writes a conditional as a value in two forms: `cond and a or b`, and
`if cond then a else b`. Both are correct. The complaint is that the decision
hides inside an expression, where an `if` statement states one case per
branch.

The two lints report the two forms, and both are off by default. A project
that wants no conditional value at all turns both on. A project that wants
one form and not the other turns on the lint for the form it refuses.
*/

use crate::lint::ctx::{Finding, LintCtx};
use crate::lints;
use crate::syntax::ast::*;

use super::correctness::{each_expr, each_stmt, unwrap_parens};

lints! {
    AndOrConditional => "and_or_conditional", Style, Allow,
        "cond and a or b used as a value, where an if statement states each case";
    IfExpressionAssignment => "if_expression_assignment", Style, Allow,
        "if cond then a else b used as a value, where an if statement states each case";
}

// --- and_or_conditional ----------------------------------------------------

impl AndOrConditional {
    /*
    `local label = ok and "yes" or "no"`.

    The pattern is a conditional built from two operators, and it holds only
    while the middle value is truthy. With a false or nil middle, the `or`
    gives the last part for every input. `misleading_and_or` reports that
    defect where a literal makes it certain. This lint reports the shape
    itself, for a project that refuses the pattern.

    The lint is off by default, because the pattern is ordinary Lua and it
    is correct for every truthy middle value.
    */
    fn check(ctx: &LintCtx<'_>, out: &mut Vec<Finding>) {
        each_value(ctx, out, |ctx, e, out| {
            if !is_and_or(ctx, e) {
                return;
            }

            out.push(
                Finding::new(
                    "and_or_conditional",
                    ctx.bytes(e.span()),
                    "this and-or picks the value with a condition",
                )
                .with_help("an if statement gives one branch to each case"),
            );
        });
    }
}

/// Reports if this expression is `a and b or c`, which Luau groups as
/// `(a and b) or c`.
fn is_and_or(ctx: &LintCtx<'_>, e: &Expr) -> bool {
    let Expr::Binary { op, lhs, .. } = e else {
        return false;
    };

    if ctx.text(*op) != "or" {
        return false;
    }

    matches!(unwrap_parens(lhs), Expr::Binary { op: and, .. } if ctx.text(*and) == "and")
}

// --- if_expression_assignment ----------------------------------------------

impl IfExpressionAssignment {
    /*
    `local label = if ok then "yes" else "no"`.

    Luau's if expression carries none of the and-or trap, so the complaint
    here is style alone: the branch lives in the value and not in a
    statement.

    The lint is off by default, and it has to stay off. `misleading_and_or`
    names this exact form as the repair, so a default that reports would
    report the fix that larvae asked for.
    */
    fn check(ctx: &LintCtx<'_>, out: &mut Vec<Finding>) {
        each_value(ctx, out, |ctx, e, out| {
            if !matches!(e, Expr::IfElse { .. }) {
                return;
            }

            out.push(
                Finding::new(
                    "if_expression_assignment",
                    ctx.bytes(e.span()),
                    "this if expression picks the value with a condition",
                )
                .with_help("an if statement gives one branch to each case"),
            );
        });
    }
}

/*
Calls `f` on each expression that produces a value for something else.

A conditional in an `if` or `while` condition is what the language asks for
there, and it says nothing about style. So the walk visits four positions
only, the ones where an `if` statement writes the same code with one branch
per case: a local, an assignment, a return, and a call argument.

The walk reads the value and not the parts inside it. `f(c and a or b)` is a
conditional argument. `x = (c and a or b) + 1` is arithmetic over one, where
an `if` statement would repeat the addition in both branches.
*/
fn each_value(
    ctx: &LintCtx<'_>,
    out: &mut Vec<Finding>,
    mut f: impl FnMut(&LintCtx<'_>, &Expr, &mut Vec<Finding>),
) {
    each_stmt(ctx, out, |ctx, s, out| {
        let values = match s {
            Stmt::Local(n) => &n.values,

            Stmt::Assign(n) => &n.values,

            Stmt::Return(n) => &n.values,

            _ => return,
        };

        for value in values {
            f(ctx, unwrap_parens(value), out);
        }
    });

    /*
    An argument list arrives through the expression walk, because a call is
    an expression wherever it stands. A `f "str"` or a `f {t}` call carries
    one literal and can hold no conditional, so those forms need no work.
    */
    each_expr(ctx, out, |ctx, e, out| {
        let Expr::Call {
            args: CallArgs::Paren(args),
            ..
        } = e
        else {
            return;
        };

        for arg in args {
            f(ctx, unwrap_parens(arg), out);
        }
    });
}
