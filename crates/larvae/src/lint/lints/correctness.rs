/*!
Lints for code that is wrong, not only untidy.

Every lint here reports code that does not do what it appears to do, so all
of them are on by default. The rule for a new lint here is this: the
reported code has no reading in which it is what the author meant.
*/

use crate::lint::ctx::{Finding, LintCtx};
use crate::lints;
use crate::syntax::ast::*;

lints! {
    AlmostSwapped => "almost_swapped", Warn,
        "two assignments that look like a swap but overwrite one of the values";
    CompareNan => "compare_nan", Warn,
        "comparing against nan, which is never equal to anything including itself";
    ConstantTableComparison => "constant_table_comparison", Warn,
        "comparing against a table literal, which compares identity and is always false";
    DivideByZero => "divide_by_zero", Warn,
        "dividing by a literal zero";
    DuplicateKeys => "duplicate_keys", Warn,
        "a table key written twice, where only the last one survives";
    IfsSameCond => "ifs_same_cond", Warn,
        "an elseif repeating a condition already tested, which can never run";
    IfSameThenElse => "if_same_then_else", Warn,
        "two branches of the same if with identical bodies";
    SuspiciousReverseLoop => "suspicious_reverse_loop", Warn,
        "a numeric for counting down without a negative step, which never runs";
    TypeCheckInsideCall => "type_check_inside_call", Warn,
        "a comparison inside type(), where it belongs outside";
    UnbalancedAssignments => "unbalanced_assignments", Warn,
        "more names than values, or more values than names";
}

// --- almost_swapped --------------------------------------------------------

impl AlmostSwapped {
    /*
    `a = b` followed by `b = a`.

    The second line reads the `a` that the first line overwrote. Thus both
    names hold the old `b`. A real swap is `a, b = b, a`, which evaluates
    both sides before it assigns either side.
    */
    fn check(ctx: &LintCtx<'_>, out: &mut Vec<Finding>) {
        each_block(ctx, out, |ctx, block, out| {
            for pair in block.stmts.windows(2) {
                let (Stmt::Assign(first), Stmt::Assign(second)) = (&pair[0], &pair[1]) else {
                    continue;
                };

                if !is_plain_assign(ctx, first) || !is_plain_assign(ctx, second) {
                    continue;
                }

                let (a, b) = (first.targets[0].span(), first.values[0].span());
                let (c, d) = (second.targets[0].span(), second.values[0].span());

                // `a = a` is a different mistake, and not this one.
                if ctx.same_text(a, b) {
                    continue;
                }

                if ctx.same_text(a, d) && ctx.same_text(b, c) {
                    let span = (ctx.bytes(first.span).0, ctx.bytes(second.span).1);

                    out.push(
                        Finding::new(
                            "almost_swapped",
                            span,
                            format!(
                                "this looks like a swap of {} and {} but is not",
                                ctx.text(a),
                                ctx.text(b)
                            ),
                        )
                        .with_help(format!(
                            "write {}, {} = {}, {}",
                            ctx.text(a),
                            ctx.text(b),
                            ctx.text(b),
                            ctx.text(a)
                        )),
                    );
                }
            }
        });
    }
}

fn is_plain_assign(ctx: &LintCtx<'_>, assign: &Assign) -> bool {
    assign.targets.len() == 1 && assign.values.len() == 1 && ctx.text(assign.op) == "="
}

// --- compare_nan -----------------------------------------------------------

impl CompareNan {
    /*
    `x == 0/0`.

    nan is not equal to anything, itself included, so a comparison against
    nan is constant. The check that users mean is `x ~= x`, which is the
    only test that is true for nan alone.
    */
    fn check(ctx: &LintCtx<'_>, out: &mut Vec<Finding>) {
        each_expr(ctx, out, |ctx, e, out| {
            let Expr::Binary { op, lhs, rhs, span } = e else {
                return;
            };

            if !matches!(ctx.text(*op), "==" | "~=") {
                return;
            }

            if !is_nan_literal(ctx, lhs) && !is_nan_literal(ctx, rhs) {
                return;
            }

            out.push(
                Finding::new("compare_nan", ctx.bytes(*span), "comparing against nan")
                    .with_help("nan is not equal to itself, test with x ~= x instead"),
            );
        });
    }
}

/// `0/0`, which is the usual written form of nan.
fn is_nan_literal(ctx: &LintCtx<'_>, e: &Expr) -> bool {
    let Expr::Binary { op, lhs, rhs, .. } = unwrap_parens(e) else {
        return false;
    };

    ctx.text(*op) == "/" && is_zero(ctx, lhs) && is_zero(ctx, rhs)
}

// --- constant_table_comparison ---------------------------------------------

impl ConstantTableComparison {
    /*
    `x == {}`.

    Tables compare by identity, and a table literal is a new table that no
    other value can be. Thus the result is fixed before the comparison
    runs. To test if a table is empty, use `next(x)`.
    */
    fn check(ctx: &LintCtx<'_>, out: &mut Vec<Finding>) {
        each_expr(ctx, out, |ctx, e, out| {
            let Expr::Binary { op, lhs, rhs, span } = e else {
                return;
            };

            if !matches!(ctx.text(*op), "==" | "~=") {
                return;
            }

            let literal = [lhs, rhs]
                .into_iter()
                .any(|side| matches!(unwrap_parens(side), Expr::Table { .. }));

            if !literal {
                return;
            }

            let always = if ctx.text(*op) == "==" {
                "false"
            } else {
                "true"
            };

            out.push(
                Finding::new(
                    "constant_table_comparison",
                    ctx.bytes(*span),
                    format!("this comparison is always {always}"),
                )
                .with_help("tables compare by identity, use next(t) == nil to test for empty"),
            );
        });
    }
}

// --- divide_by_zero --------------------------------------------------------

impl DivideByZero {
    fn check(ctx: &LintCtx<'_>, out: &mut Vec<Finding>) {
        each_expr(ctx, out, |ctx, e, out| {
            let Expr::Binary { op, rhs, span, .. } = e else {
                return;
            };

            let op_text = ctx.text(*op);

            if !matches!(op_text, "/" | "//" | "%") || !is_zero(ctx, rhs) {
                return;
            }

            /*
            `0/0` is nan, and it is the usual written form of nan. Thus
            compare_nan handles it, and larvae does not report it twice
            under two names.
            */
            if op_text == "/" && matches!(e, Expr::Binary { lhs, .. } if is_zero(ctx, lhs)) {
                return;
            }

            out.push(
                Finding::new("divide_by_zero", ctx.bytes(*span), "dividing by zero")
                    .with_help("this is inf, or nan when the left side is also zero"),
            );
        });
    }
}

// --- duplicate_keys --------------------------------------------------------

impl DuplicateKeys {
    /*
    `{ a = 1, a = 2 }`.

    Only the last entry survives, so an earlier entry is dead. The cause is
    almost always a rename that missed one key, or two merged tables that
    overlapped.
    */
    fn check(ctx: &LintCtx<'_>, out: &mut Vec<Finding>) {
        each_expr(ctx, out, |ctx, e, out| {
            let Expr::Table { fields, .. } = e else {
                return;
            };

            let mut seen: Vec<(String, TokSpan)> = Vec::new();

            for field in fields {
                let (key, span) = match field {
                    TableField::Named { name, .. } => (ctx.text(*name).to_string(), *name),

                    // The lint can compare only a literal key without evaluation.
                    TableField::Computed { key, .. } => match literal_key(ctx, key) {
                        Some(text) => (text, key.span()),

                        None => continue,
                    },

                    TableField::Positional(_) => continue,
                };

                if let Some((_, first)) = seen.iter().find(|(k, _)| *k == key) {
                    out.push(
                        Finding::new(
                            "duplicate_keys",
                            ctx.bytes(span),
                            format!("the key {key} is set twice in this table"),
                        )
                        .with_help(format!(
                            "the one on line {} is discarded",
                            ctx.line(ctx.bytes(*first).0) + 1
                        )),
                    );
                } else {
                    seen.push((key, span));
                }
            }
        });
    }
}

/// A key that the lint can compare without evaluation.
fn literal_key(ctx: &LintCtx<'_>, e: &Expr) -> Option<String> {
    match e {
        Expr::Number(s) => Some(ctx.text(*s).to_string()),

        // The quotes are part of the text, so strip them before the comparison.
        Expr::String(s) => {
            let text = ctx.text(*s);

            Some(text.get(1..text.len() - 1).unwrap_or(text).to_string())
        }

        _ => None,
    }
}

// --- ifs_same_cond ---------------------------------------------------------

impl IfsSameCond {
    /// An `elseif` that tests what an earlier branch already tested can never run.
    fn check(ctx: &LintCtx<'_>, out: &mut Vec<Finding>) {
        each_stmt(ctx, out, |ctx, s, out| {
            let Stmt::If(n) = s else {
                return;
            };

            for (i, (cond, _)) in n.branches.iter().enumerate() {
                for (earlier, _) in &n.branches[..i] {
                    if ctx.same_text(earlier.span(), cond.span()) {
                        out.push(
                            Finding::new(
                                "ifs_same_cond",
                                ctx.bytes(cond.span()),
                                "this condition was already tested above",
                            )
                            .with_help("this branch can never run"),
                        );

                        break;
                    }
                }
            }
        });
    }
}

// --- if_same_then_else -----------------------------------------------------

impl IfSameThenElse {
    /// When two branches of one `if` do the same thing, one of them is wrong.
    fn check(ctx: &LintCtx<'_>, out: &mut Vec<Finding>) {
        each_stmt(ctx, out, |ctx, s, out| {
            let Stmt::If(n) = s else {
                return;
            };

            let mut bodies: Vec<&Block> = n.branches.iter().map(|(_, b)| b).collect();

            if let Some(block) = &n.else_block {
                bodies.push(block);
            }

            for (i, body) in bodies.iter().enumerate() {
                // An empty branch belongs to empty_if, not to this lint.
                if body.stmts.is_empty() {
                    continue;
                }

                for earlier in &bodies[..i] {
                    if ctx.same_text(earlier.span, body.span) {
                        out.push(
                            Finding::new(
                                "if_same_then_else",
                                ctx.bytes(body.span),
                                "this branch does the same thing as an earlier one",
                            )
                            .with_help("merge them, or one of the two is not what was meant"),
                        );

                        break;
                    }
                }
            }
        });
    }
}

// --- suspicious_reverse_loop -----------------------------------------------

impl SuspiciousReverseLoop {
    /*
    `for i = 10, 1 do`.

    A numeric for steps by one unless the author sets a step. Thus a count
    from a higher number to a lower number runs zero times. The author
    meant to write `-1` as the step.
    */
    fn check(ctx: &LintCtx<'_>, out: &mut Vec<Finding>) {
        each_stmt(ctx, out, |ctx, s, out| {
            let Stmt::NumericFor(n) = s else {
                return;
            };

            let (Some(start), Some(limit)) = (number(ctx, &n.start), number(ctx, &n.limit)) else {
                return;
            };

            if start <= limit {
                return;
            }

            /*
            The step decides if this is the bug or the correct form of a
            countdown. Thus a step that the lint cannot evaluate must
            suppress the report, not permit it. `for i = 10, 1, step do`
            with a negative `step` is correct, and the lint cannot see the
            value of `step`.
            */
            if let Some(step) = &n.step {
                match number(ctx, step) {
                    Some(value) if value >= 0.0 => {}

                    // The step is negative or not a literal. In both cases, it is not this bug.
                    _ => return,
                }
            }

            // An explicit negative step is the correct form, not the bug.
            if let Some(step) = &n.step
                && number(ctx, step).is_some_and(|v| v < 0.0)
            {
                return;
            }

            out.push(
                Finding::new(
                    "suspicious_reverse_loop",
                    ctx.bytes(n.span),
                    "this loop counts down but steps up, so it never runs",
                )
                .with_help("add a step of -1"),
            );
        });
    }
}

// --- type_check_inside_call ------------------------------------------------

impl TypeCheckInsideCall {
    /*
    `type(x == "number")`.

    The comparison runs first and gives `type` a boolean. Thus the call
    always returns "boolean", and the test always fails. The parenthesis
    is one character away from the intended position.
    */
    fn check(ctx: &LintCtx<'_>, out: &mut Vec<Finding>) {
        each_expr(ctx, out, |ctx, e, out| {
            let Expr::Call {
                func, args, span, ..
            } = e
            else {
                return;
            };

            if !matches!(func.as_ref(), Expr::Name(n) if matches!(ctx.text(*n), "type" | "typeof"))
            {
                return;
            }

            let CallArgs::Paren(list) = args else {
                return;
            };

            let [arg] = list.as_slice() else {
                return;
            };

            let Expr::Binary { op, .. } = arg else {
                return;
            };

            if !matches!(ctx.text(*op), "==" | "~=") {
                return;
            }

            let name = ctx.text(func.span());

            out.push(
                Finding::new(
                    "type_check_inside_call",
                    ctx.bytes(*span),
                    format!("the comparison is inside {name}(), so this is always \"boolean\""),
                )
                .with_help(format!(
                    "move the closing parenthesis, {name}(x) == \"...\""
                )),
            );
        });
    }
}

// --- unbalanced_assignments ------------------------------------------------

impl UnbalancedAssignments {
    /*
    `local a, b = 1`, or `a, b = 1, 2, 3`.

    Extra names get nil, and Lua discards extra values. Both cases are
    silent. A declaration with no values is the normal way to declare a
    name before an assignment, so the lint leaves it alone.

    A call or a `...` in the last position can produce any number of
    values. There, a count that does not match is expected, and the lint
    does not report it.
    */
    fn check(ctx: &LintCtx<'_>, out: &mut Vec<Finding>) {
        each_stmt(ctx, out, |ctx, s, out| {
            let (names, values, span) = match s {
                Stmt::Local(n) if !n.values.is_empty() => {
                    (n.names.len(), &n.values, ctx.bytes(n.span))
                }

                Stmt::Assign(n) if ctx.text(n.op) == "=" => {
                    (n.targets.len(), &n.values, ctx.bytes(n.span))
                }

                _ => return,
            };

            if names == values.len() || spreads(values.last()) {
                return;
            }

            let message = if names > values.len() {
                format!(
                    "{names} names but {} values, the rest are nil",
                    values.len()
                )
            } else {
                format!(
                    "{} values but {names} names, the rest are discarded",
                    values.len()
                )
            };

            out.push(Finding::new("unbalanced_assignments", span, message));
        });
    }
}

/// Returns true if an expression can stand for any number of values.
fn spreads(e: Option<&Expr>) -> bool {
    matches!(e, Some(Expr::Call { .. } | Expr::Vararg(_)))
}

// --- shared ----------------------------------------------------------------

fn unwrap_parens(e: &Expr) -> &Expr {
    match e {
        Expr::Paren { inner, .. } => unwrap_parens(inner),

        other => other,
    }
}

fn number(ctx: &LintCtx<'_>, e: &Expr) -> Option<f64> {
    match unwrap_parens(e) {
        Expr::Number(s) => ctx.text(*s).parse().ok(),

        // `-1` is a unary minus over a literal, not a negative literal.
        Expr::Unary { op, operand, .. } if ctx.text(*op) == "-" => number(ctx, operand).map(|v| -v),

        _ => None,
    }
}

fn is_zero(ctx: &LintCtx<'_>, e: &Expr) -> bool {
    number(ctx, e) == Some(0.0)
}

/*
The three passes that every lint here uses.

Each pass iterates the nodes that [`LintCtx`] collected once for the file,
and it does not walk the tree. Thus a new lint costs a pass over a vector,
not another traversal. Each pass takes a closure, and a lint does not define
its own visitor type. A visitor per lint is thirty types that all do the
same thing and differ only in one match arm.
*/
pub fn each_expr(
    ctx: &LintCtx<'_>,
    out: &mut Vec<Finding>,
    mut f: impl FnMut(&LintCtx<'_>, &Expr, &mut Vec<Finding>),
) {
    for e in &ctx.exprs {
        f(ctx, e, out);
    }
}

pub fn each_stmt(
    ctx: &LintCtx<'_>,
    out: &mut Vec<Finding>,
    mut f: impl FnMut(&LintCtx<'_>, &Stmt, &mut Vec<Finding>),
) {
    for s in &ctx.stmts {
        f(ctx, s, out);
    }
}

pub fn each_block(
    ctx: &LintCtx<'_>,
    out: &mut Vec<Finding>,
    mut f: impl FnMut(&LintCtx<'_>, &Block, &mut Vec<Finding>),
) {
    for b in &ctx.blocks {
        f(ctx, b, out);
    }
}
