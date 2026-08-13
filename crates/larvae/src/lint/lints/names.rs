/*!
Lints about names: what is declared, what is used, and what silently escapes.

All these lints read the resolution that [`crate::lint::scope`] computes
once, and they do not walk the tree themselves. For that reason, they are
cheap enough to run on every keystroke.
*/

use serde::Deserialize;

use crate::lint::ctx::{Finding, LintCtx};
use crate::lint::globals;
use crate::lint::scope::Origin;
use crate::lints;
use crate::syntax::ast::*;

use super::correctness::each_expr;

lints! {
    GlobalUsage => "global_usage", Warn,
        "reaching into _G, which is shared with every other script";
    Shadowing => "shadowing", Warn,
        "a name that hides another still in scope";
    UndefinedVariable => "undefined_variable", Deny,
        "a name nothing declares, which is nil at runtime";
    UnscopedVariables => "unscoped_variables", Warn,
        "an assignment with no local, which creates a global";
    UnusedVariable => "unused_variable", Warn,
        "a name declared and never read";
}

// --- unused_variable -------------------------------------------------------

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct UnusedOptions {
    /// Report unused function parameters too.
    pub parameters: bool,
    /// Report unused `for` variables too.
    pub loop_variables: bool,
    /// The names to exempt, as a regular expression.
    pub ignore_pattern: String,
}

impl Default for UnusedOptions {
    fn default() -> Self {
        Self {
            /*
            Both options are off by default, and for the same reason.

            A parameter is part of a signature that the caller decides. A
            `for k, v` where the author wants only `k` is normal Luau.
            Neither is a mistake. To report them by default would teach
            users to stop reading the linter.
            */
            parameters: false,
            loop_variables: false,
            ignore_pattern: "^_".to_string(),
        }
    }
}

impl UnusedVariable {
    fn check(ctx: &LintCtx<'_>, out: &mut Vec<Finding>) {
        let options: UnusedOptions = ctx.cfg.options_for("unused_variable");

        /*
        The default pattern is a prefix test. To compile a regex for it
        costs several times the whole rest of this lint. Thus only a
        project that changed the pattern pays for the regex engine.
        */
        let ignore = match options.ignore_pattern.as_str() {
            "^_" => None,

            pattern => regex::Regex::new(pattern).ok(),
        };

        for binding in &ctx.names.bindings {
            if !binding.reads.is_empty() {
                continue;
            }

            let wanted = match binding.origin {
                Origin::Local | Origin::LocalFunction => true,

                Origin::Param => options.parameters,

                Origin::Loop => options.loop_variables,
            };

            if !wanted {
                continue;
            }

            // A method's implicit self has no name token, so the user cannot remove it.
            if binding.name == "self" && binding.origin == Origin::Param {
                continue;
            }

            let ignored = match &ignore {
                Some(pattern) => pattern.is_match(binding.name),

                None => binding.is_ignored(),
            };

            if ignored {
                continue;
            }

            let span = TokSpan::new(
                binding.declared_at as usize,
                binding.declared_at as usize + 1,
            );

            /*
            A binding that is written but never read needs a different
            message. It usually means that the code computes a value and
            discards it, which is a possible bug and not only untidy code.
            */
            let (message, help) = if binding.writes.is_empty() {
                (
                    format!("{} is never used", binding.name),
                    "remove it, or prefix the name with _ to keep it".to_string(),
                )
            } else {
                (
                    format!("{} is assigned but never read", binding.name),
                    format!("{} writes go nowhere", binding.writes.len()),
                )
            };

            out.push(Finding::new("unused_variable", ctx.bytes(span), message).with_help(help));
        }
    }
}

// --- shadowing -------------------------------------------------------------

impl Shadowing {
    /*
    A name that is declared while another name of the same name is in scope.

    The outer name becomes unreachable for the rest of the inner scope. Thus
    a later edit that intends to touch the outer name silently touches the
    inner name instead.

    A binding that directly derives from what it hides, `local x = f(x)`, is
    the intentional form, and the lint does not report it. That form reads
    the outer value at the declaration, and there the shadowing is the
    purpose.
    */
    fn check(ctx: &LintCtx<'_>, out: &mut Vec<Finding>) {
        for binding in &ctx.names.bindings {
            let Some(hidden) = binding.shadows else {
                continue;
            };

            if binding.is_ignored() {
                continue;
            }

            // The implicit self of a method has no name token to point at.
            if binding.name == "self" && binding.origin == Origin::Param {
                continue;
            }

            let outer = &ctx.names.bindings[hidden];

            if reads_inside(outer, binding.declared_in) {
                continue;
            }

            let span = TokSpan::new(
                binding.declared_at as usize,
                binding.declared_at as usize + 1,
            );

            out.push(
                Finding::new(
                    "shadowing",
                    ctx.bytes(span),
                    format!("{} hides an outer name of the same name", binding.name),
                )
                .with_help("rename one of them, the outer one is unreachable from here"),
            );
        }
    }
}

/*
Returns true if the declaration that hides the outer binding also reads it.

`local x = x + 1` reads the outer `x` on its own right side. That is the
intentional form of shadowing, and the lint does not report it.
`local x = 2` does not read the outer `x`, and it hides a name that the rest
of the scope cannot reach.
*/
fn reads_inside(outer: &crate::lint::scope::Binding<'_>, declaration: TokSpan) -> bool {
    outer
        .reads
        .iter()
        .any(|&r| r >= declaration.start && r < declaration.end)
}

// --- undefined_variable ----------------------------------------------------

impl UndefinedVariable {
    /*
    A name that nothing in the file declares and no standard library provides.

    The default level is deny, not warn. Unlike the other lints here, this
    is not a matter of taste. The name is nil at runtime, and the line that
    touches it will throw.

    This lint has the most value, and it depends the most on a correct
    globals list. Thus a project with its own globals must list them under
    `[lint] globals` and must not turn this lint off.
    */
    fn check(ctx: &LintCtx<'_>, out: &mut Vec<Finding>) {
        for &token in &ctx.names.undefined {
            let name = ctx.tok(token);

            if globals::has(ctx.cfg.std, name) || ctx.cfg.globals.iter().any(|g| g == name) {
                continue;
            }

            let span = TokSpan::new(token as usize, token as usize + 1);

            out.push(
                Finding::new(
                    "undefined_variable",
                    ctx.bytes(span),
                    format!("{name} is not defined"),
                )
                .with_help("declare it with local, or add it to [lint] globals"),
            );
        }
    }
}

// --- unscoped_variables ----------------------------------------------------

impl UnscopedVariables {
    /*
    `counter = 1` with no `local`.

    One missing keyword separates a value that this file owns from a value
    that every script in the process shares. Nothing on the line says which
    one the author meant. Almost always, the author forgot the keyword.
    */
    fn check(ctx: &LintCtx<'_>, out: &mut Vec<Finding>) {
        for &token in &ctx.names.global_writes {
            let name = ctx.tok(token);

            // To assign a known global is an intentional act, not a mistake.
            if globals::has(ctx.cfg.std, name) || ctx.cfg.globals.iter().any(|g| g == name) {
                continue;
            }

            let span = TokSpan::new(token as usize, token as usize + 1);

            out.push(
                Finding::new(
                    "unscoped_variables",
                    ctx.bytes(span),
                    format!("{name} is assigned without local, so it becomes a global"),
                )
                .with_help(format!("write local {name} instead")),
            );
        }
    }
}

// --- global_usage ----------------------------------------------------------

impl GlobalUsage {
    /*
    `_G.thing`.

    `_G` is a table that every other script in the process shares. If two
    scripts pick the same key, they collide, and nothing warns about or
    isolates the collision. A module that returns its values gives the
    same result without the shared namespace.
    */
    fn check(ctx: &LintCtx<'_>, out: &mut Vec<Finding>) {
        each_expr(ctx, out, |ctx, e, out| {
            let Expr::Name(span) = e else {
                return;
            };

            if ctx.text(*span) != "_G" {
                return;
            }

            // A local named _G is the author's own table, not the shared one.
            if !ctx.names.is_global(span.start) {
                return;
            }

            out.push(
                Finding::new(
                    "global_usage",
                    ctx.bytes(*span),
                    "_G is shared with every script",
                )
                .with_help("return the value from a module instead"),
            );
        });
    }
}
