/*!
Lints about names: what is declared, what is used, and what quietly escapes.

All of these read the resolution done once in [`crate::lint::scope`] rather
than walking the tree themselves, which is why they are cheap enough to run on
every keystroke.
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
    /// Report unused function parameters too
    pub parameters: bool,
    /// Report unused `for` variables too
    pub loop_variables: bool,
    /// Names exempted, as a regular expression
    pub ignore_pattern: String,
}

impl Default for UnusedOptions {
    fn default() -> Self {
        Self {
            /*
            Both off by default, and for the same reason.

            A parameter is part of a signature the caller decides, and a `for k,
            v` where only `k` is wanted is how the language is written. Neither
            is a mistake, so reporting them by default would train people to
            stop reading the linter.
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
        The default pattern is a prefix, and compiling a regex to test one is
        several times the cost of the whole rest of this lint. Only a project
        that changed it pays for the engine.
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

            // a method's implicit self has no name token and cannot be removed
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
            A binding written but never read is worth saying differently. It
            usually means the value is being computed and thrown away, which
            reads as a bug rather than as tidying.
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
    A name declared while another of the same name is still in scope.

    The outer one becomes unreachable for the rest of the inner scope, so a
    later edit meaning to touch it silently touches the inner one instead.

    A binding that immediately re-derives what it hides, `local x = f(x)`, is
    the deliberate form and is not reported: it reads the outer value on the
    way in, which is exactly the case where the shadowing is the point.
    */
    fn check(ctx: &LintCtx<'_>, out: &mut Vec<Finding>) {
        for binding in &ctx.names.bindings {
            let Some(hidden) = binding.shadows else {
                continue;
            };

            if binding.is_ignored() {
                continue;
            }

            // the implicit self of a method has no name token to point at
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
Whether the outer binding is read inside the declaration that hides it.

`local x = x + 1` reads the outer `x` on its own right hand side, which is the
deliberate form of shadowing and not worth reporting. `local x = 2` does not,
and hides a name the rest of the scope can no longer reach.
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
    A name nothing in the file declares and no standard library provides.

    Denied rather than warned, because unlike everything else here this one is
    not a matter of taste: the name is nil at runtime and the line that touches
    it will throw. It is the lint most worth having and the one most dependent
    on the globals list being right, so a project with its own globals should
    list them under `[lint] globals` rather than turn this off.
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

    One missing keyword is the difference between a value this file owns and
    one every script in the process shares, and nothing about the line says
    which was meant. Almost always the keyword was forgotten.
    */
    fn check(ctx: &LintCtx<'_>, out: &mut Vec<Finding>) {
        for &token in &ctx.names.global_writes {
            let name = ctx.tok(token);

            // assigning a known global is a deliberate act, not a slip
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

    A table shared with every other script in the process, so two of them
    picking the same key is a collision nothing warns about and nothing
    isolates. A module returning its values is the same thing without the
    shared namespace.
    */
    fn check(ctx: &LintCtx<'_>, out: &mut Vec<Finding>) {
        each_expr(ctx, out, |ctx, e, out| {
            let Expr::Name(span) = e else {
                return;
            };

            if ctx.text(*span) != "_G" {
                return;
            }

            // a local named _G is somebody's own table, not the shared one
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
