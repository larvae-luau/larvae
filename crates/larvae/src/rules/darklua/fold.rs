/*!
compute_expression

Fold constant expressions to their value. The evaluator rejects each
value that it cannot print exactly, so the rule itself has a small job.
The rule finds the outermost node that folds. It writes the value in the
node's place. It never looks inside a node that already folded.
*/

use super::eval;
use super::support;
use crate::rules::engine::{Edit, Flow, RuleCtx, Visit, walk_chunk};
use crate::syntax::ast::*;

pub fn compute_expression(ctx: &RuleCtx, edits: &mut Vec<Edit>) {
    struct V<'a, 'b> {
        ctx: &'a RuleCtx<'b>,
        edits: &'a mut Vec<Edit>,
        /// The ranges that already folded. The walk visits parents first,
        /// so each node inside one of these ranges is part of a value that
        /// the rule already wrote.
        done: Vec<(u32, u32)>,
    }

    impl Visit for V<'_, '_> {
        fn expr(&mut self, e: &Expr) -> Flow {
            // Only composite nodes are worth a fold. A literal is already its own value.
            if !matches!(
                e,
                Expr::Binary { .. } | Expr::Unary { .. } | Expr::Paren { .. }
            ) {
                return Flow::Next;
            }

            let (a, b) = self.ctx.bytes(e.span());

            if self.done.iter().any(|&(fa, fb)| a >= fa && b <= fb) {
                return Flow::Next;
            }

            let Some(value) = eval::eval(self.ctx, e) else {
                return Flow::Next;
            };

            let Some(mut text) = eval::print(&value, self.ctx.quote) else {
                return Flow::Next;
            };

            if text == self.ctx.src[a as usize..b as usize] {
                return Flow::Next;
            }

            /*
            A negative result directly after a minus would read as a
            comment. `a-(1-3)` must not become `a--2`.
            */
            if text.starts_with('-') && a > 0 && self.ctx.src.as_bytes()[a as usize - 1] == b'-' {
                text.insert(0, ' ');
            }

            if support::replace_keep_lines(self.ctx, a, b, &text, self.edits) {
                self.done.push((a, b));
            }

            Flow::Next
        }
    }

    walk_chunk(
        ctx.chunk,
        &mut V {
            ctx,
            edits,
            done: Vec::new(),
        },
    );
}

#[cfg(test)]
mod tests {
    use super::super::testing::{assert_lines_kept, run};
    use super::*;

    #[test]
    fn constant_arithmetic_folds() {
        assert_eq!(
            run("local x = 1 + 2\n", compute_expression),
            "local x = 3\n"
        );
        assert_eq!(
            run("local x = 2 * 3 + 4\n", compute_expression),
            "local x = 10\n"
        );
        assert_eq!(
            run("local x = 60 * 60 * 24\n", compute_expression),
            "local x = 86400\n"
        );
    }

    #[test]
    fn only_the_outermost_node_is_written() {
        // The inner 1 + 2 must not also produce an edit.
        let out = run("local x = (1 + 2) * 4\n", compute_expression);
        assert_eq!(out, "local x = 12\n");
    }

    #[test]
    fn comparisons_and_logic_fold() {
        assert_eq!(
            run("local x = 1 < 2\n", compute_expression),
            "local x = true\n"
        );
        assert_eq!(
            run("local x = not nil\n", compute_expression),
            "local x = true\n"
        );
        assert_eq!(
            run("local x = \"a\" .. \"b\"\n", compute_expression),
            "local x = \"ab\"\n"
        );
    }

    #[test]
    fn a_negative_result_never_becomes_a_comment() {
        assert_eq!(
            run("local x = a-(1-3)\n", compute_expression),
            "local x = a- -2\n"
        );
    }

    #[test]
    fn anything_not_constant_is_left_alone() {
        let src = "local x = a + 1\n";
        assert_eq!(run(src, compute_expression), src);
        let src = "local x = f() + 1\n";
        assert_eq!(run(src, compute_expression), src);
        // A fraction has no exact printed form that larvae accepts.
        let src = "local x = 10 / 4\n";
        assert_eq!(run(src, compute_expression), src);
        // The value is already folded, so there is no edit.
        let src = "local x = 3\n";
        assert_eq!(run(src, compute_expression), src);
    }

    #[test]
    fn folding_across_lines_keeps_the_line_count() {
        let src = "local x = 1 +\n    2\nreturn x\n";
        let out = run(src, compute_expression);

        assert!(out.starts_with("local x = 3\n"), "{out}");
        assert_lines_kept(src, &out);
    }
}
