/*!
Lints for Roblox's own data types.

Every one of these catches the same shape of mistake: a constructor that takes
numbers, where the numbers mean something the signature does not say. `Color3`
is on a zero to one scale and everyone thinks in 0 to 255; `UDim2` interleaves
scale and offset and nobody remembers the order. Neither is a Luau error, and
both produce something that renders wrong rather than throwing.

Off entirely when `std` is not `roblox`, since none of these names mean
anything in plain Luau and a project that defined its own `Color3` should not
be told about Roblox's.
*/

use crate::lint::config::StdLib;
use crate::lint::ctx::{Finding, LintCtx};
use crate::lints;
use crate::syntax::ast::*;

use super::correctness::each_expr;

lints! {
    RobloxIncorrectColor3NewBounds => "roblox_incorrect_color3_new_bounds", Warn,
        "Color3.new given a channel over 1, where the scale is 0 to 1";
    RobloxManualFromScaleOrOffset => "roblox_manual_fromscale_or_fromoffset", Warn,
        "a UDim2.new that fromScale or fromOffset says more clearly";
    RobloxSuspiciousUdim2New => "roblox_suspicious_udim2_new", Warn,
        "UDim2.new given two arguments, where it takes four";
}

// --- roblox_incorrect_color3_new_bounds ------------------------------------

impl RobloxIncorrectColor3NewBounds {
    /*
    `Color3.new(255, 0, 0)`.

    Reads as red and renders as white, because `Color3.new` is on a zero to
    one scale and clamps. `Color3.fromRGB` is the one that takes 0 to 255, and
    the two are one word apart.
    */
    fn check(ctx: &LintCtx<'_>, out: &mut Vec<Finding>) {
        if ctx.cfg.std != StdLib::Roblox {
            return;
        }

        each_expr(ctx, out, |ctx, e, out| {
            let Some((path, args, span)) = constructor(ctx, e) else {
                return;
            };

            if path != "Color3.new" {
                return;
            }

            let over = args.iter().filter_map(|a| number(ctx, a)).any(|v| v > 1.0);

            if !over {
                return;
            }

            out.push(
                Finding::new(
                    "roblox_incorrect_color3_new_bounds",
                    ctx.bytes(span),
                    "Color3.new takes channels from 0 to 1 and clamps the rest",
                )
                .with_help("use Color3.fromRGB for 0 to 255"),
            );
        });
    }
}

// --- roblox_suspicious_udim2_new -------------------------------------------

impl RobloxSuspiciousUdim2New {
    /*
    `UDim2.new(0.5, 0.5)`.

    `UDim2.new` takes four numbers, scale and offset for each axis, so two
    arguments sets the X scale and the X offset and leaves Y at zero. The
    author meant `UDim2.fromScale(0.5, 0.5)`, which is the constructor that
    takes two.
    */
    fn check(ctx: &LintCtx<'_>, out: &mut Vec<Finding>) {
        if ctx.cfg.std != StdLib::Roblox {
            return;
        }

        each_expr(ctx, out, |ctx, e, out| {
            let Some((path, args, span)) = constructor(ctx, e) else {
                return;
            };

            if path != "UDim2.new" || args.len() != 2 {
                return;
            }

            /*
            Which one they meant is decided by whether the numbers are
            fractions. Two values under one are scales, whole numbers are
            pixels, and anything else is left unguessed.
            */
            let suggestion = match args
                .iter()
                .filter_map(|a| number(ctx, a))
                .collect::<Vec<_>>()
            {
                values if values.len() == 2 && values.iter().all(|v| v.abs() <= 1.0) => {
                    "UDim2.fromScale"
                }

                values if values.len() == 2 => "UDim2.fromOffset",

                _ => "UDim2.fromScale or UDim2.fromOffset",
            };

            out.push(
                Finding::new(
                    "roblox_suspicious_udim2_new",
                    ctx.bytes(span),
                    "UDim2.new takes four numbers, this passes two",
                )
                .with_help(format!("{suggestion} is the one that takes two")),
            );
        });
    }
}

// --- roblox_manual_fromscale_or_fromoffset ---------------------------------

impl RobloxManualFromScaleOrOffset {
    /*
    `UDim2.new(a, 0, b, 0)`.

    Correct, and says the wrong thing: the two zeros are the offsets, and a
    reader has to count positions to see that. `UDim2.fromScale(a, b)` says it
    outright. The same in reverse for `UDim2.new(0, a, 0, b)`.
    */
    fn check(ctx: &LintCtx<'_>, out: &mut Vec<Finding>) {
        if ctx.cfg.std != StdLib::Roblox {
            return;
        }

        each_expr(ctx, out, |ctx, e, out| {
            let Some((path, args, span)) = constructor(ctx, e) else {
                return;
            };

            if path != "UDim2.new" {
                return;
            }

            let [x_scale, x_offset, y_scale, y_offset] = args else {
                return;
            };

            let zero = |e: &Expr| number(ctx, e) == Some(0.0);

            let (form, kept) = if zero(x_offset) && zero(y_offset) {
                ("fromScale", (x_scale, y_scale))
            } else if zero(x_scale) && zero(y_scale) {
                ("fromOffset", (x_offset, y_offset))
            } else {
                return;
            };

            // all four zero is UDim2.new(), which needs no advice
            if zero(kept.0) && zero(kept.1) {
                return;
            }

            out.push(
                Finding::new(
                    "roblox_manual_fromscale_or_fromoffset",
                    ctx.bytes(span),
                    format!("this is UDim2.{form}"),
                )
                .with_help(format!(
                    "write UDim2.{form}({}, {})",
                    ctx.text(kept.0.span()),
                    ctx.text(kept.1.span())
                )),
            );
        });
    }
}

// --- shared ----------------------------------------------------------------

/*
A call of the form `Type.name(...)` where `Type` is a global.

Rooted at a global for the same reason as everywhere else: a local named
`Color3` is somebody's own, and this has nothing to say about it.
*/
fn constructor<'e>(ctx: &LintCtx<'_>, e: &'e Expr) -> Option<(String, &'e [Expr], TokSpan)> {
    let Expr::Call {
        func,
        method: None,
        args: CallArgs::Paren(list),
        span,
        ..
    } = e
    else {
        return None;
    };

    let Expr::Index {
        object,
        key: IndexKey::Field(field),
        ..
    } = func.as_ref()
    else {
        return None;
    };

    let Expr::Name(base) = object.as_ref() else {
        return None;
    };

    if !ctx.names.is_global(base.start) {
        return None;
    }

    Some((
        format!("{}.{}", ctx.text(*base), ctx.text(*field)),
        list,
        *span,
    ))
}

fn number(ctx: &LintCtx<'_>, e: &Expr) -> Option<f64> {
    match e {
        Expr::Number(s) => ctx.text(*s).parse().ok(),

        Expr::Paren { inner, .. } => number(ctx, inner),

        Expr::Unary { op, operand, .. } if ctx.text(*op) == "-" => number(ctx, operand).map(|v| -v),

        _ => None,
    }
}
