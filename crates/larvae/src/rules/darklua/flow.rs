/*!
Rules that prune or reshape control flow
*/

use super::eval;
use super::support::{self, insert, tok_bytes};
use crate::rules::engine::{Edit, Flow, RuleCtx, Visit, walk_chunk};
use crate::syntax::ast::*;

/// The last statement with an effect. A trailing `;` does not count.
fn last_real(b: &Block) -> Option<&Stmt> {
    b.stmts.iter().rev().find(|s| !matches!(s, Stmt::Empty(_)))
}

/*
filter_after_early_return: a `do ... return ... end` makes the rest of
the enclosing block unreachable, so the rule removes the rest.

A type alias among the dead statements stops the rule. An exported alias
is visible outside the block, so its removal would be observable.
*/
pub fn filter_after_early_return(ctx: &RuleCtx, edits: &mut Vec<Edit>) {
    struct V<'a, 'b> {
        ctx: &'a RuleCtx<'b>,
        edits: &'a mut Vec<Edit>,
    }

    impl Visit for V<'_, '_> {
        fn block(&mut self, b: &Block) -> Flow {
            for (i, s) in b.stmts.iter().enumerate() {
                let Stmt::Do(d) = s else { continue };

                if !matches!(last_real(&d.block), Some(Stmt::Return(_))) {
                    continue;
                }

                let rest = &b.stmts[i + 1..];

                if rest.is_empty() || rest.iter().any(|s| matches!(s, Stmt::TypeAlias(_))) {
                    return Flow::Next;
                }

                let from = self.ctx.bytes(rest[0].span()).0;
                let to = self.ctx.bytes(rest[rest.len() - 1].span()).1;

                self.ctx.delete_bytes_keep_lines(from, to, self.edits);

                return Flow::Next;
            }

            Flow::Next
        }
    }

    walk_chunk(ctx.chunk, &mut V { ctx, edits });
}

/// remove_unused_while: remove a loop whose condition is a constant false.
pub fn remove_unused_while(ctx: &RuleCtx, edits: &mut Vec<Edit>) {
    struct V<'a, 'b> {
        ctx: &'a RuleCtx<'b>,
        edits: &'a mut Vec<Edit>,
    }

    impl Visit for V<'_, '_> {
        fn stmt(&mut self, s: &Stmt) -> Flow {
            let Stmt::While(w) = s else { return Flow::Next };
            let Some(v) = eval::eval(self.ctx, &w.cond) else {
                return Flow::Next;
            };

            if v.truthy() {
                return Flow::Next;
            }

            self.ctx.delete_keep_lines(w.span, self.edits);

            Flow::Next
        }
    }

    walk_chunk(ctx.chunk, &mut V { ctx, edits });
}

/*
remove_continue: change a loop body into `repeat ... until true`. Then
the rule can write `continue` as `break`.

Only a loop whose own level has no `break` qualifies. With both present,
the inner repeat would capture the break. The flag variable that repairs
this needs new statements, and this retain-lines pass has no place for
them. The rule skips `repeat` loops completely. Their until condition can
read locals from the body, and a wrap would take those out of scope.
*/
pub fn remove_continue(ctx: &RuleCtx, edits: &mut Vec<Edit>) {
    struct V<'a, 'b> {
        ctx: &'a RuleCtx<'b>,
        edits: &'a mut Vec<Edit>,
    }

    impl Visit for V<'_, '_> {
        fn stmt(&mut self, s: &Stmt) -> Flow {
            let (block, span) = match s {
                Stmt::While(n) => (&n.block, n.span),

                Stmt::NumericFor(n) => (&n.block, n.span),

                Stmt::GenericFor(n) => (&n.block, n.span),

                _ => return Flow::Next,
            };

            rewrite(self.ctx, block, span, self.edits);

            Flow::Next
        }
    }

    walk_chunk(ctx.chunk, &mut V { ctx, edits });
}

fn rewrite(ctx: &RuleCtx, block: &Block, span: TokSpan, edits: &mut Vec<Edit>) {
    let mut continues = Vec::new();
    let mut has_break = false;

    scan_level(block, &mut continues, &mut has_break);

    if continues.is_empty() || has_break {
        return;
    }

    let Some(do_tok) = block.span.start.checked_sub(1) else {
        return;
    };

    let end_tok = span.end - 1;

    if ctx.tok_text(do_tok) != "do" || ctx.tok_text(end_tok) != "end" {
        return;
    }

    insert(tok_bytes(ctx, do_tok).1, " repeat", edits);
    insert(tok_bytes(ctx, end_tok).0, "until true ", edits);

    for c in continues {
        ctx.replace(c, "break".to_string(), edits);
    }
}

/*
Collect the `continue` statements that belong to this loop, and record if
the loop owns a `break`. A nested loop owns its own statements, so the
walk stops at it.
*/
fn scan_level(b: &Block, continues: &mut Vec<TokSpan>, has_break: &mut bool) {
    for s in &b.stmts {
        match s {
            Stmt::Continue(span) => continues.push(*span),

            Stmt::Break(_) => *has_break = true,

            Stmt::Do(d) => scan_level(&d.block, continues, has_break),

            Stmt::If(n) => {
                for (_, body) in &n.branches {
                    scan_level(body, continues, has_break);
                }

                if let Some(e) = &n.else_block {
                    scan_level(e, continues, has_break);
                }
            }

            _ => {}
        }
    }
}

/*
remove_unused_if_branch: prune each branch whose condition is a known
constant.

The rule unwraps a branch to its contents when the branch always runs and
has no bindings of its own. A branch that declares locals becomes a `do`
block instead. Then the bindings keep their scope.
*/
pub fn remove_unused_if_branch(ctx: &RuleCtx, edits: &mut Vec<Edit>) {
    struct V<'a, 'b> {
        ctx: &'a RuleCtx<'b>,
        edits: &'a mut Vec<Edit>,
    }

    impl Visit for V<'_, '_> {
        fn stmt(&mut self, s: &Stmt) -> Flow {
            if let Stmt::If(n) = s {
                prune(self.ctx, n, self.edits);
            }

            Flow::Next
        }
    }

    walk_chunk(ctx.chunk, &mut V { ctx, edits });
}

/// The token index of the keyword that opens branch `i`.
fn branch_keyword(ctx: &RuleCtx, n: &If, i: usize) -> Option<u32> {
    if i == 0 {
        return (ctx.tok_text(n.span.start) == "if").then_some(n.span.start);
    }

    let idx = n.branches[i].0.span().start.checked_sub(1)?;

    (ctx.tok_text(idx) == "elseif").then_some(idx)
}

/// The token index of the `then` that closes the condition of branch `i`.
fn branch_then(ctx: &RuleCtx, n: &If, i: usize) -> Option<u32> {
    let idx = n.branches[i].1.span.start.checked_sub(1)?;

    (ctx.tok_text(idx) == "then").then_some(idx)
}

fn prune(ctx: &RuleCtx, n: &If, edits: &mut Vec<Edit>) {
    let mut falsy = vec![false; n.branches.len()];
    let mut truthy_at = None;

    for (i, (cond, _)) in n.branches.iter().enumerate() {
        match eval::eval(ctx, cond) {
            Some(v) if v.truthy() => {
                truthy_at = Some(i);
                break;
            }

            Some(_) => falsy[i] = true,
            None => {}
        }
    }

    let survivors: Vec<usize> = (0..n.branches.len())
        .filter(|&i| !falsy[i] && truthy_at.is_none_or(|t| i <= t))
        .collect();

    if survivors.len() == n.branches.len() && truthy_at.is_none() {
        // No condition is known. Keep the statement.
        return;
    }

    let end_tok = n.span.end - 1;

    if ctx.tok_text(end_tok) != "end" {
        return;
    }

    let if_start = tok_bytes(ctx, n.span.start).0;

    // Each branch is dead. Only the else block remains.
    if survivors.is_empty() {
        match &n.else_block {
            Some(block) => {
                let Some(else_tok) = block.span.start.checked_sub(1) else {
                    return;
                };

                if ctx.tok_text(else_tok) != "else" {
                    return;
                }

                unwrap_block(
                    ctx,
                    block,
                    if_start,
                    tok_bytes(ctx, else_tok).1,
                    end_tok,
                    edits,
                );
            }

            None => ctx.delete_keep_lines(n.span, edits),
        }

        return;
    }

    // Exactly one branch survives, and it always runs.
    if let Some(t) = truthy_at
        && survivors == [t]
        && let Some(then_tok) = branch_then(ctx, n, t)
    {
        unwrap_block(
            ctx,
            &n.branches[t].1,
            if_start,
            tok_bytes(ctx, then_tok).1,
            end_tok,
            edits,
        );

        return;
    }

    // In the other cases, remove the dead branches one at a time.
    for i in 0..n.branches.len() {
        if survivors.contains(&i) {
            continue;
        }

        let Some(kw) = branch_keyword(ctx, n, i) else {
            continue;
        };

        let to = ctx.bytes(n.branches[i].1.span).1;
        ctx.delete_bytes_keep_lines(tok_bytes(ctx, kw).0, to, edits);
    }

    if truthy_at.is_some()
        && let Some(block) = &n.else_block
        && let Some(else_tok) = block.span.start.checked_sub(1)
        && ctx.tok_text(else_tok) == "else"
    {
        let to = ctx.bytes(block.span).1;
        ctx.delete_bytes_keep_lines(tok_bytes(ctx, else_tok).0, to, edits);
    }

    // The first branch that remains must use the keyword `if`.
    if let Some(&first) = survivors.first()
        && first > 0
        && let Some(kw) = branch_keyword(ctx, n, first)
    {
        let (a, b) = tok_bytes(ctx, kw);
        edits.push((a, b, "if".to_string()));
    }
}

/*
Keep one block and remove the `if` structure around it. A block that
binds names becomes `do ... end`. Then those names keep their scope.
*/
fn unwrap_block(
    ctx: &RuleCtx,
    block: &Block,
    if_start: u32,
    head_end: u32,
    end_tok: u32,
    edits: &mut Vec<Edit>,
) {
    let body_end = ctx.bytes(block.span).1;
    let (end_a, end_b) = tok_bytes(ctx, end_tok);

    if support::declares_local(block) {
        support::replace_keep_lines(ctx, if_start, head_end, "do", edits);
        ctx.delete_bytes_keep_lines(body_end, end_a, edits);
    } else {
        ctx.delete_bytes_keep_lines(if_start, head_end, edits);
        ctx.delete_bytes_keep_lines(body_end, end_b, edits);
    }
}

/*
remove_empty_do: remove `do end` blocks that contain nothing.
*/
pub fn remove_empty_do(ctx: &RuleCtx, edits: &mut Vec<Edit>) {
    struct V<'a, 'b> {
        ctx: &'a RuleCtx<'b>,
        edits: &'a mut Vec<Edit>,
    }

    impl Visit for V<'_, '_> {
        fn stmt(&mut self, s: &Stmt) -> Flow {
            let Stmt::Do(d) = s else { return Flow::Next };

            if d.block.stmts.is_empty() {
                self.ctx.delete_keep_lines(d.span, self.edits);
            }

            Flow::Next
        }
    }

    walk_chunk(ctx.chunk, &mut V { ctx, edits });
}

#[cfg(test)]
mod tests {
    use super::super::testing::{assert_lines_kept, run};
    use super::*;

    #[test]
    fn empty_do_blocks_go() {
        assert_eq!(run("do end\nprint(1)\n", remove_empty_do), "\nprint(1)\n");
        // The definition is strict. A semicolon means that the author typed content.
        let src = "do ; end\n";
        assert_eq!(run(src, remove_empty_do), src);
        // Nested empty blocks lose one layer per pass. The outer block survives this run.
        assert_eq!(run("do do end end\n", remove_empty_do), "do  end\n");
    }

    #[test]
    fn dead_statements_after_an_early_return_go() {
        let src = "do return end\nprint(1)\nprint(2)\n";
        let out = run(src, filter_after_early_return);

        assert!(out.starts_with("do return end\n"), "{out}");
        assert!(!out.contains("print"), "{out}");
        assert_lines_kept(src, &out);
    }

    #[test]
    fn a_do_block_without_a_return_changes_nothing() {
        let src = "do print(0) end\nprint(1)\n";
        assert_eq!(run(src, filter_after_early_return), src);
        // No statement follows, so there is nothing to remove.
        let src = "do return end\n";
        assert_eq!(run(src, filter_after_early_return), src);
    }

    #[test]
    fn while_false_disappears() {
        let src = "while false do\n    print(1)\nend\nprint(2)\n";
        let out = run(src, remove_unused_while);

        assert!(!out.contains("print(1)"), "{out}");
        assert!(out.contains("print(2)"), "{out}");
        assert_lines_kept(src, &out);
        // nil is also falsy.
        assert!(!run("while nil do print(1) end\n", remove_unused_while).contains("print"));
    }

    #[test]
    fn live_while_loops_stay() {
        let src = "while true do print(1) end\n";
        assert_eq!(run(src, remove_unused_while), src);
        let src = "while cond do print(1) end\n";
        assert_eq!(run(src, remove_unused_while), src);
        // Zero is truthy in Lua.
        let src = "while 0 do print(1) end\n";
        assert_eq!(run(src, remove_unused_while), src);
    }

    #[test]
    fn continue_becomes_a_repeat_until_true() {
        let src = "for i = 1, 10 do\n    if skip then continue end\n    work(i)\nend\n";
        let out = run(src, remove_continue);

        assert!(out.contains("do repeat"), "{out}");
        assert!(out.contains("break"), "{out}");
        assert!(out.contains("until true end"), "{out}");
        assert!(!out.contains("continue"), "{out}");
        assert_lines_kept(src, &out);
    }

    #[test]
    fn a_loop_that_also_breaks_is_left_alone() {
        // The inner repeat would capture the break.
        let src = "for i = 1, 10 do\n    if a then continue end\n    if b then break end\nend\n";
        assert_eq!(run(src, remove_continue), src);
    }

    #[test]
    fn loops_without_continue_are_left_alone() {
        let src = "for i = 1, 10 do work(i) end\n";
        assert_eq!(run(src, remove_continue), src);
        // A repeat loop can read body locals in its condition.
        let src = "repeat\n    if a then continue end\nuntil done\n";
        assert_eq!(run(src, remove_continue), src);
    }

    #[test]
    fn a_nested_loop_keeps_its_own_continue() {
        let src = "for i = 1, 2 do\n    for j = 1, 2 do\n        if x then continue end\n    end\n    if y then break end\nend\n";
        let out = run(src, remove_continue);

        // The rule rewrites the inner loop. The outer loop has a break, so it stays.
        assert_eq!(out.matches("repeat").count(), 1, "{out}");
        assert!(out.contains("if y then break end"), "{out}");
    }

    #[test]
    fn a_constant_true_branch_unwraps() {
        let src = "if true then\n    print(1)\nend\n";
        let out = run(src, remove_unused_if_branch);

        assert!(out.contains("print(1)"), "{out}");
        assert!(!out.contains("if"), "{out}");
        assert_lines_kept(src, &out);
    }

    #[test]
    fn a_branch_with_locals_becomes_a_do_block() {
        // An unwrap would leak x into the enclosing scope.
        let src = "if true then\n    local x = 1\nend\n";
        let out = run(src, remove_unused_if_branch);

        assert!(out.starts_with("do\n"), "{out}");
        assert!(out.contains("local x = 1"), "{out}");
        assert!(out.trim_end().ends_with("end"), "{out}");
        assert_lines_kept(src, &out);
    }

    #[test]
    fn a_constant_false_branch_falls_through_to_else() {
        let src = "if false then\n    print(1)\nelse\n    print(2)\nend\n";
        let out = run(src, remove_unused_if_branch);

        assert!(!out.contains("print(1)"), "{out}");
        assert!(out.contains("print(2)"), "{out}");
        assert_lines_kept(src, &out);
    }

    #[test]
    fn a_false_if_with_no_else_disappears() {
        let src = "if false then\n    print(1)\nend\nprint(2)\n";
        let out = run(src, remove_unused_if_branch);

        assert!(!out.contains("print(1)"), "{out}");
        assert!(out.contains("print(2)"), "{out}");
        assert_lines_kept(src, &out);
    }

    #[test]
    fn a_dead_elseif_is_removed_and_the_rest_stands() {
        let src = "if a then\n    one()\nelseif false then\n    two()\nelse\n    three()\nend\n";
        let out = run(src, remove_unused_if_branch);

        assert!(out.contains("one()"), "{out}");
        assert!(!out.contains("two()"), "{out}");
        assert!(out.contains("three()"), "{out}");
        assert_lines_kept(src, &out);
    }

    #[test]
    fn a_leading_dead_branch_promotes_the_next_one() {
        let src = "if false then\n    one()\nelseif b then\n    two()\nend\n";
        let out = run(src, remove_unused_if_branch);

        assert!(!out.contains("one()"), "{out}");
        assert!(out.contains("if b then"), "{out}");
        assert!(out.contains("two()"), "{out}");
        assert_lines_kept(src, &out);
    }

    #[test]
    fn unknown_conditions_are_left_alone() {
        let src = "if a then\n    one()\nelseif b then\n    two()\nelse\n    three()\nend\n";
        assert_eq!(run(src, remove_unused_if_branch), src);
    }
}
