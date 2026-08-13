/*!
Rules that remove whole call statements

Both rules have the same shape. Each rule removes a call statement whose
target has a known name. `preserve_arguments_side_effects` decides the
result when an argument can have a side effect. The option is on by
default, and then the call stays.
*/

use super::support;
use crate::rules::engine::{Edit, Flow, RuleCtx, Visit, walk_chunk};
use crate::syntax::ast::*;

/// remove_assertions: remove `assert(...)` statements.
pub fn remove_assertions(ctx: &RuleCtx, edits: &mut Vec<Edit>, preserve: bool) {
    drop_calls(
        ctx,
        edits,
        preserve,
        &|ctx, func| matches!(func, Expr::Name(s) if ctx.text(*s) == "assert"),
    );
}

/// remove_debug_profiling: remove `debug.profilebegin` and `debug.profileend`.
pub fn remove_debug_profiling(ctx: &RuleCtx, edits: &mut Vec<Edit>, preserve: bool) {
    drop_calls(ctx, edits, preserve, &|ctx, func| {
        let Expr::Index {
            object,
            key: IndexKey::Field(field),
            ..
        } = func
        else {
            return false;
        };

        let Expr::Name(base) = object.as_ref() else {
            return false;
        };

        ctx.text(*base) == "debug" && matches!(ctx.text(*field), "profilebegin" | "profileend")
    });
}

type Match = dyn Fn(&RuleCtx, &Expr) -> bool;

fn drop_calls(ctx: &RuleCtx, edits: &mut Vec<Edit>, preserve: bool, matches: &Match) {
    struct V<'a, 'b> {
        ctx: &'a RuleCtx<'b>,
        edits: &'a mut Vec<Edit>,
        preserve: bool,
        matches: &'a Match,
    }

    impl Visit for V<'_, '_> {
        fn stmt(&mut self, s: &Stmt) -> Flow {
            let Stmt::Call(e, span) = s else {
                return Flow::Next;
            };
            let Expr::Call {
                func, method, args, ..
            } = e
            else {
                return Flow::Next;
            };

            if method.is_some() || !(self.matches)(self.ctx, func) {
                return Flow::Next;
            }

            if self.preserve && args_might_do_something(args) {
                return Flow::Next;
            }

            self.ctx.delete_keep_lines(*span, self.edits);

            Flow::Next
        }
    }

    walk_chunk(
        ctx.chunk,
        &mut V {
            ctx,
            edits,
            preserve,
            matches,
        },
    );
}

fn args_might_do_something(args: &CallArgs) -> bool {
    match args {
        CallArgs::Paren(list) => list.iter().any(support::has_call),

        CallArgs::Table(t) => support::has_call(t),

        CallArgs::Str(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::super::testing::{assert_lines_kept, run};
    use super::*;

    fn assertions(ctx: &RuleCtx, edits: &mut Vec<Edit>) {
        remove_assertions(ctx, edits, true);
    }

    fn assertions_forced(ctx: &RuleCtx, edits: &mut Vec<Edit>) {
        remove_assertions(ctx, edits, false);
    }

    fn profiling(ctx: &RuleCtx, edits: &mut Vec<Edit>) {
        remove_debug_profiling(ctx, edits, true);
    }

    #[test]
    fn plain_assertions_go() {
        let src = "assert(x)\nprint(1)\n";
        let out = run(src, assertions);

        assert!(!out.contains("assert"), "{out}");
        assert!(out.contains("print(1)"), "{out}");
        assert_lines_kept(src, &out);
        assert!(!run("assert(a == b, \"boom\")\n", assertions).contains("assert"));
    }

    #[test]
    fn arguments_with_side_effects_keep_the_call() {
        let src = "assert(check())\n";
        assert_eq!(run(src, assertions), src);
        // With the option off, the rule removes the call.
        assert!(!run(src, assertions_forced).contains("assert"));
    }

    #[test]
    fn assertions_used_as_values_stay() {
        // The code binds the result, so this is not a bare statement.
        let src = "local x = assert(y)\n";
        assert_eq!(run(src, assertions), src);
        let src = "obj:assert(y)\n";
        assert_eq!(run(src, assertions), src);
    }

    #[test]
    fn debug_profiling_goes() {
        let src = "debug.profilebegin(\"x\")\nwork()\ndebug.profileend()\n";
        let out = run(src, profiling);

        assert!(!out.contains("profile"), "{out}");
        assert!(out.contains("work()"), "{out}");
        assert_lines_kept(src, &out);
    }

    #[test]
    fn other_debug_calls_stay() {
        let src = "debug.traceback()\n";
        assert_eq!(run(src, profiling), src);
        let src = "profilebegin(\"x\")\n";
        assert_eq!(run(src, profiling), src);
    }
}
