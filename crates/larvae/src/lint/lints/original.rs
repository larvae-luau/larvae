/*!
Lints selene does not have.

Each one here earned its place by catching something in real Luau that no
existing rule catches, and each is cheap enough to run on a keystroke. They are
the reason to run larvae's linter rather than a port of somebody else's.
*/

use crate::lint::ctx::{Finding, LintCtx};
use crate::lints;
use crate::syntax::ast::*;

use super::correctness::{each_block, each_stmt};

lints! {
    SelfAssignment => "self_assignment", Warn,
        "assigning a value to itself, which does nothing";
    ShadowedLoopWork => "loop_invariant_call", Warn,
        "a call inside a loop whose result cannot change between iterations";
    StringConcatInLoop => "string_concat_in_loop", Warn,
        "building a string by concatenation in a loop, which is quadratic";
    UnreachableCode => "unreachable_code", Warn,
        "statements after a return, break or continue, which never run";
}

// --- unreachable_code ------------------------------------------------------

impl UnreachableCode {
    /*
    Anything after a `return`, `break` or `continue` in the same block.

    Luau's parser accepts a `return` only as the last statement of a block, so
    this fires on `break` and `continue`, and on `return` in the dialects that
    allow it. What it catches is an early exit added above code that was meant
    to keep running, which is invisible in review because the dead lines still
    look like the program.
    */
    fn check(ctx: &LintCtx<'_>, out: &mut Vec<Finding>) {
        each_block(ctx, out, |ctx, block, out| {
            let live: Vec<&Stmt> = block
                .stmts
                .iter()
                .filter(|s| !matches!(s, Stmt::Empty(_)))
                .collect();

            let Some(at) = live.iter().position(|s| terminates(s)) else {
                return;
            };

            let Some(dead) = live.get(at + 1..).filter(|rest| !rest.is_empty()) else {
                return;
            };

            let span = (
                ctx.bytes(dead[0].span()).0,
                ctx.bytes(dead[dead.len() - 1].span()).1,
            );

            let terminator = match live[at] {
                Stmt::Return(_) => "return",

                Stmt::Break(_) => "break",

                _ => "continue",
            };

            out.push(
                Finding::new(
                    "unreachable_code",
                    span,
                    format!("this cannot run, the {terminator} above always leaves the block"),
                )
                .with_help("remove it, or move it above the exit"),
            );
        });
    }
}

fn terminates(stmt: &Stmt) -> bool {
    matches!(stmt, Stmt::Return(_) | Stmt::Break(_) | Stmt::Continue(_))
}

// --- self_assignment -------------------------------------------------------

impl SelfAssignment {
    /*
    `x = x`, or `t.k = t.k`.

    Does nothing, so it is either a leftover from an edit or a line where one
    side was meant to be something else. The second is the reason this is worth
    reporting rather than shrugging at.

    A compound operator is excluded, since `x += x` does something. So is an
    index whose key is computed, `t[i] = t[i]`, because `i` may be a function
    of something and the two reads need not be the same element.
    */
    fn check(ctx: &LintCtx<'_>, out: &mut Vec<Finding>) {
        each_stmt(ctx, out, |ctx, s, out| {
            let Stmt::Assign(n) = s else {
                return;
            };

            if ctx.text(n.op) != "=" || n.targets.len() != n.values.len() {
                return;
            }

            for (target, value) in n.targets.iter().zip(&n.values) {
                if !is_stable(target) || !ctx.same_text(target.span(), value.span()) {
                    continue;
                }

                out.push(
                    Finding::new(
                        "self_assignment",
                        ctx.bytes(target.span()),
                        format!("{} is assigned to itself", ctx.text(target.span())),
                    )
                    .with_help("this does nothing, one side was probably meant to differ"),
                );
            }
        });
    }
}

/// Whether reading this twice is certain to give the same thing both times
fn is_stable(e: &Expr) -> bool {
    match e {
        Expr::Name(_) => true,

        Expr::Index {
            object,
            key: IndexKey::Field(_),
            ..
        } => is_stable(object),

        _ => false,
    }
}

// --- string_concat_in_loop -------------------------------------------------

impl StringConcatInLoop {
    /*
    `s = s .. x` inside a loop.

    Lua strings are immutable, so each round allocates a new one and copies
    everything built so far. A thousand iterations is half a million character
    copies, and the loop looks linear while being quadratic, which is why this
    survives review and then shows up as a frame time spike.

    The fix is to push into a table and `table.concat` once at the end.
    */
    fn check(ctx: &LintCtx<'_>, out: &mut Vec<Finding>) {
        each_stmt(ctx, out, |ctx, s, out| {
            let body = match s {
                Stmt::While(n) => &n.block,

                Stmt::Repeat(n) => &n.block,

                Stmt::NumericFor(n) => &n.block,

                Stmt::GenericFor(n) => &n.block,

                _ => return,
            };

            let outside = ctx.bytes(s.span()).0;

            for accumulator in accumulating_concats(ctx, body, outside) {
                out.push(
                    Finding::new(
                        "string_concat_in_loop",
                        ctx.bytes(accumulator),
                        format!(
                            "{} is grown by concatenation each iteration",
                            ctx.text(accumulator)
                        ),
                    )
                    .with_help(
                        "collect the pieces in a table and table.concat once after the loop",
                    ),
                );
            }
        });
    }
}

/*
Statements in this block that append to a name declared outside the loop.

Two things have to hold, and both were learned from what happens without them.
The target must be a bare name, because `child.Name = child.Name .. \"_old\"`
writes a different field each iteration and is linear. And it must be declared
before the loop, because a string built inside the body starts empty every time
around and is linear too.

Nested `if` and `do` blocks are searched, since guarding the append behind a
condition is the shape this appears in most often. A nested *loop* is not,
because it is reported when that loop is visited.
*/
fn accumulating_concats(ctx: &LintCtx<'_>, block: &Block, loop_start: u32) -> Vec<TokSpan> {
    let mut out = Vec::new();

    for stmt in &block.stmts {
        match stmt {
            Stmt::If(n) => {
                for (_, inner) in &n.branches {
                    out.extend(accumulating_concats(ctx, inner, loop_start));
                }

                if let Some(inner) = &n.else_block {
                    out.extend(accumulating_concats(ctx, inner, loop_start));
                }
            }

            Stmt::Do(n) => out.extend(accumulating_concats(ctx, &n.block, loop_start)),

            _ => {}
        }

        let Stmt::Assign(n) = stmt else {
            continue;
        };

        if n.targets.len() != 1 || n.values.len() != 1 {
            continue;
        }

        // a field write lands somewhere different each iteration, so it is linear
        let Expr::Name(name) = &n.targets[0] else {
            continue;
        };

        // a string declared inside the body starts empty every time around
        let outlives = ctx
            .names
            .read_of
            .get(&name.start)
            .and_then(|&b| ctx.names.bindings.get(b))
            .or_else(|| {
                ctx.names
                    .bindings
                    .iter()
                    .find(|b| b.writes.contains(&name.start))
            })
            .is_some_and(|b| {
                ctx.bytes(TokSpan::new(
                    b.declared_at as usize,
                    b.declared_at as usize + 1,
                ))
                .0 < loop_start
            });

        if !outlives {
            continue;
        }

        let target = *name;

        // `s ..= x`, the compound form, is the same cost
        if ctx.text(n.op) == "..=" {
            out.push(target);

            continue;
        }

        if ctx.text(n.op) != "=" {
            continue;
        }

        if concatenates(ctx, &n.values[0], target) {
            out.push(target);
        }
    }

    out
}

/// Whether this expression is a `..` chain with `target` somewhere in it
fn concatenates(ctx: &LintCtx<'_>, e: &Expr, target: TokSpan) -> bool {
    let Expr::Binary { op, lhs, rhs, .. } = e else {
        return false;
    };

    if ctx.text(*op) != ".." {
        return false;
    }

    [lhs, rhs]
        .into_iter()
        .any(|side| ctx.same_text(side.span(), target) || concatenates(ctx, side, target))
}

// --- loop_invariant_call ---------------------------------------------------

impl ShadowedLoopWork {
    /*
    `game:GetService("Players")` inside a loop.

    A service lookup returns the same object every time, so calling it per
    iteration pays for a lookup that could have happened once above the loop.
    The same is true of `require`, which is memoised but still costs a table
    lookup and a call.

    Deliberately narrow: only calls that are known to be pure and known to be
    constant for a fixed argument. A general purity analysis would either be
    wrong or would need types, and a lint that guesses at this is worse than
    one that only speaks when it is sure.
    */
    fn check(ctx: &LintCtx<'_>, out: &mut Vec<Finding>) {
        each_stmt(ctx, out, |ctx, s, out| {
            let body = match s {
                Stmt::While(n) => &n.block,

                Stmt::Repeat(n) => &n.block,

                Stmt::NumericFor(n) => &n.block,

                Stmt::GenericFor(n) => &n.block,

                _ => return,
            };

            let mut found = Vec::new();
            collect_invariant(ctx, body, &mut found);

            for (span, what) in found {
                out.push(
                    Finding::new(
                        "loop_invariant_call",
                        ctx.bytes(span),
                        format!("{what} returns the same thing every iteration"),
                    )
                    .with_help("hoist it above the loop"),
                );
            }
        });
    }
}

fn collect_invariant(ctx: &LintCtx<'_>, block: &Block, out: &mut Vec<(TokSpan, String)>) {
    for stmt in &block.stmts {
        walk_exprs_shallow(stmt, &mut |e| {
            if let Some(what) = invariant_call(ctx, e) {
                out.push((e.span(), what));
            }
        });
    }
}

/// The name of the call, when it is one whose result cannot change
fn invariant_call(ctx: &LintCtx<'_>, e: &Expr) -> Option<String> {
    let Expr::Call {
        func, method, args, ..
    } = e
    else {
        return None;
    };

    let literal_arg = match args {
        CallArgs::Str(_) => true,

        CallArgs::Paren(list) => matches!(list.as_slice(), [Expr::String(_)]),

        CallArgs::Table(_) => false,
    };

    if !literal_arg {
        return None;
    }

    // `game:GetService("X")`
    if let Some(m) = method
        && ctx.text(*m) == "GetService"
    {
        return Some(format!("{}:GetService(...)", ctx.text(func.span())));
    }

    // `require("@pkg/x")`
    if method.is_none() && matches!(func.as_ref(), Expr::Name(n) if ctx.text(*n) == "require") {
        return Some("require(...)".to_string());
    }

    None
}

/*
Expressions in this statement, without descending into a nested loop or
function.

A call inside a nested loop belongs to that loop and is reported when it is
visited. A call inside a function literal runs when the function is called,
which is not once per iteration of this loop, so it is not this lint's business
at all.
*/
fn walk_exprs_shallow(stmt: &Stmt, f: &mut impl FnMut(&Expr)) {
    match stmt {
        Stmt::Local(n) => {
            for e in &n.values {
                walk_expr_shallow(e, f);
            }
        }

        Stmt::Assign(n) => {
            for e in n.targets.iter().chain(&n.values) {
                walk_expr_shallow(e, f);
            }
        }

        Stmt::Call(e, _) => walk_expr_shallow(e, f),

        Stmt::Return(n) => {
            for e in &n.values {
                walk_expr_shallow(e, f);
            }
        }

        Stmt::If(n) => {
            for (cond, block) in &n.branches {
                walk_expr_shallow(cond, f);

                for inner in &block.stmts {
                    walk_exprs_shallow(inner, f);
                }
            }

            if let Some(block) = &n.else_block {
                for inner in &block.stmts {
                    walk_exprs_shallow(inner, f);
                }
            }
        }

        Stmt::Do(n) => {
            for inner in &n.block.stmts {
                walk_exprs_shallow(inner, f);
            }
        }

        _ => {}
    }
}

fn walk_expr_shallow(e: &Expr, f: &mut impl FnMut(&Expr)) {
    f(e);

    match e {
        // a function body does not run once per iteration
        Expr::Function { .. } => {}

        Expr::Binary { lhs, rhs, .. } => {
            walk_expr_shallow(lhs, f);
            walk_expr_shallow(rhs, f);
        }

        Expr::Unary { operand, .. } => walk_expr_shallow(operand, f),

        Expr::Paren { inner, .. } => walk_expr_shallow(inner, f),

        Expr::TypeAssert { expr, .. } => walk_expr_shallow(expr, f),

        Expr::Index { object, key, .. } => {
            walk_expr_shallow(object, f);

            if let IndexKey::Computed(k) = key {
                walk_expr_shallow(k, f);
            }
        }

        Expr::Call { func, args, .. } => {
            walk_expr_shallow(func, f);

            match args {
                CallArgs::Paren(list) => list.iter().for_each(|a| walk_expr_shallow(a, f)),

                CallArgs::Table(t) => walk_expr_shallow(t, f),

                CallArgs::Str(_) => {}
            }
        }

        Expr::Table { fields, .. } => {
            for field in fields {
                match field {
                    TableField::Positional(v) => walk_expr_shallow(v, f),

                    TableField::Named { value, .. } => walk_expr_shallow(value, f),

                    TableField::Computed { key, value } => {
                        walk_expr_shallow(key, f);
                        walk_expr_shallow(value, f);
                    }
                }
            }
        }

        _ => {}
    }
}
