/*!
Lints that selene does not have.

Each lint here catches a problem in real Luau code that no existing rule
catches, and each is cheap enough to run on a keystroke. These lints are the
reason to run larvae's linter and not a port of a different linter.
*/

use crate::lint::ctx::{Finding, LintCtx};
use crate::lints;
use crate::syntax::ast::*;

use super::correctness::{each_block, each_stmt};

lints! {
    NonConstRequire => "non_const_require", Allow,
        "a required module bound with local, where const says it never changes";
    SelfAssignment => "self_assignment", Warn,
        "assigning a value to itself, which does nothing";
    ShadowedLoopWork => "loop_invariant_call", Warn,
        "a call inside a loop whose result cannot change between iterations";
    StringConcatInLoop => "string_concat_in_loop", Warn,
        "building a string by concatenation in a loop, which is quadratic";
    UnreachableCode => "unreachable_code", Warn,
        "statements after a return, break or continue, which never run";
}

// --- non_const_require -----------------------------------------------------

impl NonConstRequire {
    /*
    `local Signal = require(...)`, where `const Signal` says more.

    Luau enforces `const` and does not treat it as decoration: to reassign
    one is a syntax error, `Variable 'X' is constant and may not be
    reassigned`. A module handle is the clearest case for it, because a
    rebind of that name to something else is almost always a mistake and not
    an intention.

    The lint is off by default. `const` is newer than most codebases, and a
    project that has not adopted it would get one warning per require on the
    first run. That is how a linter teaches people to ignore it. `larvae fmt`
    with `require_binding = "const"` does the whole conversion in one pass,
    which is the better way to arrive at it.

    The lint is narrow on purpose, and it matches the `const_requires`
    transform: one name, one value, no type annotation. A multi binding
    cannot be const one name at a time, and an annotated local states
    something that the author cared about.
    */
    fn check(ctx: &LintCtx<'_>, out: &mut Vec<Finding>) {
        each_stmt(ctx, out, |ctx, s, out| {
            let Stmt::Local(n) = s else {
                return;
            };

            if n.is_const || !binds_one_require(ctx, n) {
                return;
            }

            /*
            A name that something reassigns cannot be const. Advice to make
            it const produces `Variable 'X' is constant and may not be
            reassigned`. The formatter's `require_binding` skips the same
            case.
            */
            let reassigned = ctx
                .names
                .by_token
                .get(&n.names[0].name.start)
                .and_then(|&i| ctx.names.bindings.get(i))
                .is_none_or(|b| !b.writes.is_empty());

            if reassigned {
                return;
            }

            out.push(
                Finding::new(
                    "non_const_require",
                    ctx.bytes(n.keyword),
                    format!(
                        "{} is a required module, so it can be const",
                        ctx.text(n.names[0].name)
                    ),
                )
                .with_help("const is enforced, reassigning one is a syntax error"),
            );
        });
    }
}

/// `local X = require(...)`, with one name, one value, and no annotation.
fn binds_one_require(ctx: &LintCtx<'_>, local: &Local) -> bool {
    let ([binding], [value]) = (local.names.as_slice(), local.values.as_slice()) else {
        return false;
    };

    // An annotation is a statement of the author, so the lint leaves it alone.
    if binding.ty.is_some() {
        return false;
    }

    is_require_call(ctx, value)
}

/*
Reports if this expression calls `require`.

The name must be the global one. A local named `require` is the function of
somebody else and says nothing about modules.
*/
fn is_require_call(ctx: &LintCtx<'_>, e: &Expr) -> bool {
    let Expr::Call { func, method, .. } = e else {
        return false;
    };

    if method.is_some() {
        return false;
    }

    matches!(func.as_ref(), Expr::Name(n)
        if ctx.text(*n) == "require" && ctx.names.is_global(n.start))
}

// --- unreachable_code ------------------------------------------------------

impl UnreachableCode {
    /*
    Any statement after a `return`, `break` or `continue` in the same block.

    Luau's parser accepts a `return` only as the last statement of a block.
    Thus this lint reports `break` and `continue`, and `return` in the
    dialects that allow it. The lint catches an early exit that an author
    added above code that must keep running. A review does not see this,
    because the dead lines still look like the program.
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

    This does nothing. It is a leftover from an edit, or a line where the
    author meant one side to be different. The second case is the reason
    that the lint reports it.

    The lint excludes a compound operator, because `x += x` does something.
    It also excludes an index with a computed key, `t[i] = t[i]`. There,
    `i` can depend on something, and the two reads need not be the same
    element.
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

/// Returns true if two reads of this expression always give the same value.
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

    Lua strings are immutable. Thus each iteration allocates a new string
    and copies everything built so far. A thousand iterations is half a
    million character copies. The loop looks linear but is quadratic. For
    that reason, it survives review and then appears as a frame-time spike.

    The fix is to push the pieces into a table and `table.concat` once at
    the end.
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
The statements in this block that append to a name declared outside the loop.

Two conditions must hold, and each one exists because of a false report
without it. The target must be a bare name, because
`child.Name = child.Name .. \"_old\"` writes a different field each iteration
and is linear. The target must be declared before the loop, because a string
built inside the body starts empty on each iteration and is also linear.

The search enters nested `if` and `do` blocks, because an append behind a
condition is the most common shape. The search does not enter a nested loop,
because the lint reports that loop when it visits it.
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

        // A field write lands on a different place each iteration, so it is linear.
        let Expr::Name(name) = &n.targets[0] else {
            continue;
        };

        // A string declared inside the body starts empty on each iteration.
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

        // `s ..= x`, the compound form, has the same cost.
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

/// Returns true if this expression is a `..` chain that contains `target`.
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

    A service lookup returns the same object every time. Thus a call per
    iteration pays for a lookup that can happen once above the loop. The
    same applies to `require`, which is memoised but still costs a table
    lookup and a call.

    The lint is narrow by design. It reports only calls that are known pure
    and known constant for a fixed argument. A general purity analysis
    would be wrong, or would need types. A lint that guesses here is worse
    than a lint that speaks only when it is sure.
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

/// Returns the name of the call, when its result cannot change.
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
Visit the expressions in this statement, and do not descend into a nested
loop or function.

A call inside a nested loop belongs to that loop, and the lint reports it
when it visits that loop. A call inside a function literal runs when a
caller calls the function, and not once per iteration of this loop. Thus
this lint does not report it.
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
        // A function body does not run once per iteration.
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
