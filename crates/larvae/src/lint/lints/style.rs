/*!
Lints for code that works but is hard to read.

The standard here differs from correctness. The reported code does what the
author meant. The complaint is that the next reader must work to see it.
Thus users disagree with these lints more easily, so each lint says what it
wants instead, and not only what it dislikes.
*/

use crate::lint::ctx::{Finding, LintCtx};
use crate::lints;
use crate::syntax::ast::*;

use super::correctness::{each_block, each_expr, each_stmt};

lints! {
    EmptyIf => "empty_if", Suspicious, Warn,
        "an if branch with nothing in it";
    EmptyLoop => "empty_loop", Suspicious, Warn,
        "a loop body with nothing in it";
    MixedTable => "mixed_table", Suspicious, Warn,
        "a table with both array entries and named keys";
    MultipleStatements => "multiple_statements", Style, Allow,
        "more than one statement on a line";
    ParentheseConditions => "parenthese_conditions", Style, Warn,
        "parentheses around a condition, which Luau does not need";
}

// --- empty_if --------------------------------------------------------------

impl EmptyIf {
    /*
    An empty branch is a stub or a leftover.

    A branch that holds only a comment is neither, because the comment is
    the purpose. Thus the check reports only a branch with no content at all.
    */
    fn check(ctx: &LintCtx<'_>, out: &mut Vec<Finding>) {
        each_stmt(ctx, out, |ctx, s, out| {
            let Stmt::If(n) = s else {
                return;
            };

            let bodies = n
                .branches
                .iter()
                .map(|(_, b)| b)
                .chain(n.else_block.as_ref());

            for body in bodies {
                if body.stmts.is_empty() && !holds_comment(ctx, body.span) {
                    out.push(
                        Finding::new("empty_if", ctx.bytes(body.span), "this branch is empty")
                            .with_help("remove it, or invert the condition"),
                    );
                }
            }
        });
    }
}

// --- empty_loop ------------------------------------------------------------

impl EmptyLoop {
    fn check(ctx: &LintCtx<'_>, out: &mut Vec<Finding>) {
        each_stmt(ctx, out, |ctx, s, out| {
            let block = match s {
                Stmt::While(n) => &n.block,

                Stmt::Repeat(n) => &n.block,

                Stmt::NumericFor(n) => &n.block,

                Stmt::GenericFor(n) => &n.block,

                _ => return,
            };

            if block.stmts.is_empty() && !holds_comment(ctx, block.span) {
                out.push(
                    Finding::new(
                        "empty_loop",
                        ctx.bytes(block.span),
                        "this loop body is empty",
                    )
                    .with_help("a loop that does nothing is either a stub or a spin"),
                );
            }
        });
    }
}

// --- mixed_table -----------------------------------------------------------

impl MixedTable {
    /*
    `{ 1, 2, a = 3 }`.

    This is legal, and it is two different data structures that share one
    value. `#t` counts only the array part, and `pairs` walks both parts.
    Thus the half that a reader gets depends on how they ask. The usual
    mistake is that the author did not know there were two halves.
    */
    fn check(ctx: &LintCtx<'_>, out: &mut Vec<Finding>) {
        each_expr(ctx, out, |ctx, e, out| {
            let Expr::Table { fields, span } = e else {
                return;
            };

            let array = fields
                .iter()
                .any(|f| matches!(f, TableField::Positional(_)));

            let keyed = fields
                .iter()
                .any(|f| matches!(f, TableField::Named { .. } | TableField::Computed { .. }));

            if array && keyed {
                out.push(
                    Finding::new(
                        "mixed_table",
                        ctx.bytes(*span),
                        "this table has both array entries and named keys",
                    )
                    .with_help("# and ipairs see only the array half, split them apart"),
                );
            }
        });
    }
}

// --- multiple_statements ---------------------------------------------------

impl MultipleStatements {
    /*
    This is Luau's SameLineStatement, and Luau reports it by default.

    An earlier larvae kept the lint off, because `if x then return end` on
    one line is normal Luau and the lint appeared to report every such line.
    That was a defect in the lint and not a reason to disable it: the lint
    compared every statement in the file, and the `return` there sits in its
    own block. The lint now compares siblings, Luau agrees with the result,
    and the lint is on.
    */
    fn check(ctx: &LintCtx<'_>, out: &mut Vec<Finding>) {
        /*
        The lint compares within a block, not across the file.

        `if x then return end` has the `return` on the same line as the
        `if`, but in a different block. To report that would report the
        exact form that keeps this lint off by default. Two statements
        count only when they are siblings.
        */
        each_block(ctx, out, |ctx, block, out| {
            let mut previous: Option<u32> = None;

            for stmt in &block.stmts {
                if matches!(stmt, Stmt::Empty(_)) {
                    continue;
                }

                let line = ctx.line(ctx.bytes(stmt.span()).0);

                if previous == Some(line) {
                    out.push(
                        Finding::new(
                            "multiple_statements",
                            ctx.bytes(stmt.span()),
                            "more than one statement on this line",
                        )
                        .with_help("put it on its own line"),
                    );
                }

                previous = Some(line);
            }
        });
    }
}

// --- parenthese_conditions -------------------------------------------------

impl ParentheseConditions {
    /*
    `if (x) then`.

    This is a habit from a language that requires the parentheses. Luau does
    not require them. When the condition grows, the parentheses hide where
    the condition ends.
    */
    fn check(ctx: &LintCtx<'_>, out: &mut Vec<Finding>) {
        each_stmt(ctx, out, |ctx, s, out| {
            let conds: Vec<&Expr> = match s {
                Stmt::If(n) => n.branches.iter().map(|(c, _)| c).collect(),

                Stmt::While(n) => vec![&n.cond],

                Stmt::Repeat(n) => vec![&n.cond],

                _ => return,
            };

            for cond in conds {
                if matches!(cond, Expr::Paren { .. }) {
                    out.push(
                        Finding::new(
                            "parenthese_conditions",
                            ctx.bytes(cond.span()),
                            "these parentheses are not needed",
                        )
                        .with_help("Luau conditions do not take parentheses"),
                    );
                }
            }
        });
    }
}

// --- shared ----------------------------------------------------------------

/// Returns true if a comment sits inside this span. A comment makes an empty block intentional.
fn holds_comment(ctx: &LintCtx<'_>, span: TokSpan) -> bool {
    let (lo, hi) = match span.is_empty() {
        // An empty block has no tokens, so look between the tokens around it.
        true => {
            let before = span
                .start
                .checked_sub(1)
                .map_or(0, |i| ctx.toks[i as usize].end);
            let after = ctx
                .toks
                .get(span.start as usize)
                .map_or(ctx.src.len() as u32, |t| t.start);

            (before, after)
        }

        false => ctx.bytes(span),
    };

    ctx.comments.iter().any(|&(s, _)| s >= lo && s < hi)
}
