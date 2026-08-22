/*!
Lints for the shape of a branch, and not for what the branch computes.

Each lint here reads the code twice: once as the author wrote it, and once in
the form that says the same thing with less structure. The finding is the
difference between the two. Biome reports each of these shapes in JavaScript,
and Luau code grows them for the same reasons.
*/

use crate::lint::ctx::{Finding, LintCtx};
use crate::lints;
use crate::syntax::ast::*;

use super::correctness::{each_stmt, unwrap_parens};

lints! {
    ConstantCondition => "constant_condition", Correctness, Warn,
        "a condition that is a literal, so the branch is decided already";
    ElseAfterReturn => "else_after_return", Style, Allow,
        "an else after a branch that returns, breaks or continues";
    CollapsibleIf => "collapsible_if", Style, Allow,
        "an if that holds one if and nothing else";
    NegatedCondition => "negated_condition", Style, Allow,
        "an if with a negated condition and an else";
}

// --- constant_condition ----------------------------------------------------

impl ConstantCondition {
    /*
    `if true then`, `if nil then`, `if 0 then`, `if "cache" then`.

    A literal decides the branch before the program runs. Either a debug
    edit stayed in, or the author meant to name a value and wrote the value
    itself. The number case is the one that surprises people: Luau follows
    Lua, where only `nil` and `false` are false. Thus `if 0 then` runs the
    branch, and so does `if "" then`.

    Two loops are exempt. `while true do` is how Luau writes a loop that a
    `break` or a `return` ends, and `repeat ... until false` is that loop
    with the test at the bottom. Both forms are standard, and a report on
    them would teach the reader to ignore this lint.
    */
    fn check(ctx: &LintCtx<'_>, out: &mut Vec<Finding>) {
        each_stmt(ctx, out, |ctx, s, out| {
            let conds: Vec<&Expr> = match s {
                Stmt::If(n) => n.branches.iter().map(|(c, _)| c).collect(),

                // The exempt loops, which are the two standard forms.
                Stmt::While(n) if constant_truth(&n.cond) == Some(true) => return,

                Stmt::Repeat(n) if constant_truth(&n.cond) == Some(false) => return,

                Stmt::While(n) => vec![&n.cond],

                Stmt::Repeat(n) => vec![&n.cond],

                _ => return,
            };

            for cond in conds {
                let Some(truth) = constant_truth(cond) else {
                    continue;
                };

                let (verdict, help) = match truth {
                    true => (
                        "always true",
                        "only nil and false are false in Luau, so 0 and \"\" pass here",
                    ),

                    false => ("always false", "the code under it never runs"),
                };

                out.push(
                    Finding::new(
                        "constant_condition",
                        ctx.bytes(cond.span()),
                        format!("this condition is {verdict}"),
                    )
                    .with_help(help),
                );
            }
        });
    }
}

// --- else_after_return -----------------------------------------------------

impl ElseAfterReturn {
    /*
    `if x then return ... else ... end`.

    The else costs a level of indent and buys nothing, because the branch
    above it left the block already. The body of the else can sit after the
    `if`, at the level of the rest of the function.

    Every branch has to end in the jump, and not the first one alone. An
    `elseif` that falls through still needs the else: to drop it there would
    run the else body after that branch as well, which changes the program.
    */
    fn check(ctx: &LintCtx<'_>, out: &mut Vec<Finding>) {
        each_stmt(ctx, out, |ctx, s, out| {
            let Stmt::If(n) = s else {
                return;
            };

            let Some(block) = &n.else_block else {
                return;
            };

            if !n.branches.iter().all(|(_, b)| jumps(b)) {
                return;
            }

            out.push(
                Finding::new(
                    "else_after_return",
                    keyword_before(ctx, block),
                    "this else follows a branch that leaves the block",
                )
                .with_help("put its body after the if, which saves a level of indent"),
            );
        });
    }
}

// --- collapsible_if --------------------------------------------------------

impl CollapsibleIf {
    /*
    `if a then if b then ... end end`.

    The two tests are one `and` apart, and the nesting says that they are
    not. The reader of the outer test has to hold it while they read the
    inner one, and the body sits two levels deep for no reason.

    The lint reports one shape only: one branch each, no else on either, and
    nothing beside the inner `if`. With any other content the merge changes
    what runs, and with an else it changes which test guards which body.
    */
    fn check(ctx: &LintCtx<'_>, out: &mut Vec<Finding>) {
        each_stmt(ctx, out, |ctx, s, out| {
            let Stmt::If(outer) = s else {
                return;
            };

            if !one_plain_branch(outer) {
                return;
            }

            let [Stmt::If(inner)] = &outer.branches[0].1.stmts[..] else {
                return;
            };

            if !one_plain_branch(inner) {
                return;
            }

            out.push(
                Finding::new(
                    "collapsible_if",
                    ctx.bytes(inner.span),
                    "this if is the only thing inside another if",
                )
                .with_help("join the two conditions with and"),
            );
        });
    }
}

// --- negated_condition -----------------------------------------------------

impl NegatedCondition {
    /*
    `if not ready then a() else b() end`.

    The reader meets the negative case first and holds it while they read
    the positive one. To drop the `not` and swap the two bodies states the
    same thing in the order that the reader expects.

    One branch only. An `elseif` between the two makes the swap a rewrite of
    the whole chain, and the result is longer than what it replaces.
    */
    fn check(ctx: &LintCtx<'_>, out: &mut Vec<Finding>) {
        each_stmt(ctx, out, |ctx, s, out| {
            let Stmt::If(n) = s else {
                return;
            };

            if n.branches.len() != 1 || n.else_block.is_none() {
                return;
            }

            let cond = &n.branches[0].0;

            if !matches!(unwrap_parens(cond), Expr::Unary { op, .. } if ctx.text(*op) == "not") {
                return;
            }

            out.push(
                Finding::new(
                    "negated_condition",
                    ctx.bytes(cond.span()),
                    "this condition is negated and the if has an else",
                )
                .with_help("drop the not and swap the two bodies"),
            );
        });
    }
}

// --- shared ----------------------------------------------------------------

/// Returns what a literal condition decides. `None` when it is not a literal.
fn constant_truth(e: &Expr) -> Option<bool> {
    match unwrap_parens(e) {
        Expr::Nil(_) | Expr::False(_) => Some(false),

        // A number or a string is true in Luau, and that holds for 0 and "".
        Expr::True(_) | Expr::Number(_) | Expr::String(_) => Some(true),

        _ => None,
    }
}

/// Returns true if the last statement of a block leaves the block.
fn jumps(block: &Block) -> bool {
    block
        .stmts
        .iter()
        .rev()
        .find(|s| !matches!(s, Stmt::Empty(_)))
        .is_some_and(|s| matches!(s, Stmt::Return(_) | Stmt::Break(_) | Stmt::Continue(_)))
}

/// Returns true if an if is one test over one body, which a merge can hold.
fn one_plain_branch(n: &If) -> bool {
    n.branches.len() == 1 && n.else_block.is_none()
}

/*
Returns the byte range of the token that opens a block.

The AST holds no span for the `else` word, and the span of a block starts at
its first statement. Thus a report on the block points at the body, and the
word that the lint asks the author to remove stays unmarked.
*/
fn keyword_before(ctx: &LintCtx<'_>, block: &Block) -> (u32, u32) {
    match block.span.start.checked_sub(1) {
        Some(i) => ctx.bytes(TokSpan::new(i as usize, i as usize + 1)),

        None => ctx.bytes(block.span),
    }
}
