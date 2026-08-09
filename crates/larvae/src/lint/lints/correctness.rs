/*!
Lints for code that is wrong rather than untidy.

Everything here reports something that does not do what it looks like it does,
so all of them are on by default. The bar for adding one is that the reported
code has no reading under which it is what the author meant.
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

    The second line reads the `a` the first line just overwrote, so both names
    end up holding the old `b`. A real swap is `a, b = b, a`, which evaluates
    both sides before assigning either.
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

                // `a = a` is a different mistake, and not this one
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

    nan is not equal to anything, itself included, so a comparison against it
    is constant. The check people mean is `x ~= x`, which is the only thing
    true of nan alone.
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

/// `0/0`, which is how nan is written when it is written at all
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

    Tables compare by identity, and a literal on the right is a table nothing
    else can be, so the answer is known before the comparison runs. To ask
    whether a table is empty, look at `next(x)`.
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
            `0/0` is nan and is the idiom for writing it, so it is left to
            compare_nan rather than reported twice under two names.
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

    Only the last one survives, so an earlier entry is dead. Almost always a
    rename that missed one, or two merged tables that overlapped.
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

                    // only a literal key can be compared without running anything
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

/// A key that can be compared without evaluating anything
fn literal_key(ctx: &LintCtx<'_>, e: &Expr) -> Option<String> {
    match e {
        Expr::Number(s) => Some(ctx.text(*s).to_string()),

        // the quotes are part of the text, so strip them before comparing
        Expr::String(s) => {
            let text = ctx.text(*s);

            Some(text.get(1..text.len() - 1).unwrap_or(text).to_string())
        }

        _ => None,
    }
}

// --- ifs_same_cond ---------------------------------------------------------

impl IfsSameCond {
    /// An `elseif` testing what an earlier branch already tested can never run
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
    /// Two branches of one `if` doing the same thing means one of them is wrong
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
                // an empty branch is empty_if's business, not this one
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

    A numeric for steps by one unless told otherwise, so counting from a
    higher number to a lower one runs zero times. The author meant to write
    `-1` as the step.
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
            The step decides whether this is the bug or the correct spelling of
            a countdown, so a step that cannot be evaluated has to suppress the
            report rather than permit it: `for i = 10, 1, step do` with a
            negative `step` is right, and nothing here can see its value.
            */
            if let Some(step) = &n.step {
                match number(ctx, step) {
                    Some(value) if value >= 0.0 => {}

                    // negative, or not a literal, and either way not this bug
                    _ => return,
                }
            }

            // an explicit negative step is the correct spelling, not the bug
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

    The comparison happens first and hands `type` a boolean, so the call
    always returns "boolean" and the test always fails. The parenthesis is one
    character from where it was meant to be.
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

    Extra names get nil and extra values are discarded, both silently. A
    declaration with no values at all is the normal way to declare something
    before assigning it, so that is left alone.

    A call or a `...` in last position can produce any number of values, so a
    count that does not match is expected there and not reported.
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

/// Whether an expression can stand for any number of values
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

        // `-1` is a unary minus over a literal, not a negative literal
        Expr::Unary { op, operand, .. } if ctx.text(*op) == "-" => number(ctx, operand).map(|v| -v),

        _ => None,
    }
}

fn is_zero(ctx: &LintCtx<'_>, e: &Expr) -> bool {
    number(ctx, e) == Some(0.0)
}

/*
The three passes every lint here is written against.

Each iterates the nodes [`LintCtx`] collected once for the file rather than
walking the tree, so adding a lint costs a pass over a vector rather than
another traversal. They take a closure rather than each lint defining its own
visitor type, because a visitor per lint is thirty types that all do the same
thing and differ only in one match arm.
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
