/*!
Lints for code that works but reads badly.

The bar here is different from correctness: the reported code does what the
author meant, and the complaint is that the next reader will have to work to
see it. That makes these easier to disagree with, so each one says what it
wants instead rather than only what it dislikes.
*/

use crate::lint::ctx::{Finding, LintCtx};
use crate::lints;
use crate::syntax::ast::*;

use super::correctness::{each_block, each_expr, each_stmt};

lints! {
    EmptyIf => "empty_if", Warn,
        "an if branch with nothing in it";
    EmptyLoop => "empty_loop", Warn,
        "a loop body with nothing in it";
    MixedTable => "mixed_table", Warn,
        "a table with both array entries and named keys";
    MultipleStatements => "multiple_statements", Allow,
        "more than one statement on a line";
    ParentheseConditions => "parenthese_conditions", Warn,
        "parentheses around a condition, which Luau does not need";
}

// --- empty_if --------------------------------------------------------------

impl EmptyIf {
    /*
    An empty branch is either a stub or a leftover.

    A branch holding only a comment is neither, since the comment is the point,
    so the check is for a branch with nothing in it at all.
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

    Legal, and two different data structures sharing one value. `#t` counts
    only the array part and `pairs` walks both, so which half a reader gets
    depends on how they ask, and the mistake is usually that they did not know
    there were two halves.
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
    Off by default, unlike the rest.

    `if x then return end` on one line is idiomatic Luau and this would report
    every one of them, so a project has to ask for it. It is here because for
    a project that has decided one statement per line, having it checked is
    worth more than the noise costs.
    */
    fn check(ctx: &LintCtx<'_>, out: &mut Vec<Finding>) {
        /*
        Compared within a block, not across the file.

        `if x then return end` has the `return` on the same line as the `if`,
        but in a different block, and reporting that would report the very
        idiom this lint is off by default to avoid arguing about. Two
        statements only count when they are siblings.
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

    A habit carried in from a language that requires them. Luau does not, and
    the parentheses hide where the condition actually ends when it grows.
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

/// Whether a comment sits inside this span, which makes an empty block deliberate
fn holds_comment(ctx: &LintCtx<'_>, span: TokSpan) -> bool {
    let (lo, hi) = match span.is_empty() {
        // an empty block has no tokens, so look between the ones around it
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
