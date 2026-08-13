/*!
Rules that rewrite declarations and assignments

Compound assignment and floor division both emit their target again. For
this reason, both rules reject each target that is not provably safe to
write twice.
*/

use super::support::{self, insert, tok_bytes};
use crate::rules::engine::{Edit, Flow, RuleCtx, Visit, walk_chunk};
use crate::syntax::ast::*;

/// The plain operator that matches a compound operator.
fn compound_op(op: &str) -> Option<&'static str> {
    Some(match op {
        "+=" => "+",

        "-=" => "-",

        "*=" => "*",

        "/=" => "/",

        "%=" => "%",

        "^=" => "^",

        "..=" => "..",

        "//=" => "//",

        _ => return None,
    })
}

/// Move back over the spaces in front of an offset, so no trailing blank remains.
fn trim_back(ctx: &RuleCtx, mut from: u32) -> u32 {
    let b = ctx.src.as_bytes();

    while from > 0 && matches!(b[from as usize - 1], b' ' | b'\t') {
        from -= 1;
    }

    from
}

/*
A compound assignment that the rule can rewrite. It has one target, one
value, and a target that is cheap to write twice. The function returns
the pieces that the callers need.
*/
fn compound_parts<'a>(ctx: &RuleCtx<'a>, a: &'a Assign) -> Option<(&'a str, &'a Expr, &'a str)> {
    if a.targets.len() != 1 || a.values.len() != 1 {
        return None;
    }

    let op = ctx.text(a.op);
    let plain = compound_op(op)?;
    let target = &a.targets[0];

    if !support::is_reemittable(target) {
        return None;
    }

    let target_text = ctx.text(target.span());
    // The second copy must not move code onto a new line.
    if target_text.contains('\n') {
        return None;
    }

    Some((target_text, &a.values[0], plain))
}

/// Wrap the right hand side when a direct join to an operator would re-associate it.
fn guard_value(ctx: &RuleCtx, value: &Expr, edits: &mut Vec<Edit>) {
    if support::is_atomic(value) {
        return;
    }

    let (a, b) = ctx.bytes(value.span());
    insert(a, "(", edits);
    insert(b, ")", edits);
}

/*
remove_compound_assignment: `x += 1` becomes `x = x + 1`. The rule writes
the target again, so the target must be a name or a path of names. When
remove_floor_division is on, that rule handles `//=`. Then the two rules
do not collide.
*/
pub fn remove_compound_assignment(ctx: &RuleCtx, edits: &mut Vec<Edit>, floor_division_on: bool) {
    struct V<'a, 'b> {
        ctx: &'a RuleCtx<'b>,
        edits: &'a mut Vec<Edit>,
        skip_floor: bool,
    }

    impl Visit for V<'_, '_> {
        fn stmt(&mut self, s: &Stmt) -> Flow {
            let Stmt::Assign(a) = s else {
                return Flow::Next;
            };

            if self.skip_floor && self.ctx.text(a.op) == "//=" {
                return Flow::Next;
            }

            let Some((target, value, plain)) = compound_parts(self.ctx, a) else {
                return Flow::Next;
            };

            let (oa, ob) = self.ctx.bytes(a.op);
            self.edits.push((oa, ob, format!("= {target} {plain}")));
            guard_value(self.ctx, value, self.edits);

            Flow::Next
        }
    }

    walk_chunk(
        ctx.chunk,
        &mut V {
            ctx,
            edits,
            skip_floor: floor_division_on,
        },
    );
}

/*
remove_floor_division: `a // b` becomes `math.floor(a / b)`.

`/` and `//` have the same precedence. Thus an in-place swap of the
operator cannot re-associate the parse. For this reason, the rule inserts
only the wrapping call and no extra parentheses. The compound form emits
its target again, so it has the same restriction as
remove_compound_assignment.
*/
pub fn remove_floor_division(ctx: &RuleCtx, edits: &mut Vec<Edit>) {
    struct V<'a, 'b> {
        ctx: &'a RuleCtx<'b>,
        edits: &'a mut Vec<Edit>,
    }

    impl Visit for V<'_, '_> {
        fn expr(&mut self, e: &Expr) -> Flow {
            let Expr::Binary { op, span, .. } = e else {
                return Flow::Next;
            };

            if self.ctx.text(*op) != "//" {
                return Flow::Next;
            }

            let (a, b) = self.ctx.bytes(*span);
            insert(a, "math.floor(", self.edits);
            let (oa, ob) = self.ctx.bytes(*op);
            self.edits.push((oa, ob, "/".to_string()));
            insert(b, ")", self.edits);

            Flow::Next
        }

        fn stmt(&mut self, s: &Stmt) -> Flow {
            let Stmt::Assign(a) = s else {
                return Flow::Next;
            };

            if self.ctx.text(a.op) != "//=" {
                return Flow::Next;
            }

            let Some((target, value, _)) = compound_parts(self.ctx, a) else {
                return Flow::Next;
            };

            let (oa, ob) = self.ctx.bytes(a.op);
            self.edits
                .push((oa, ob, format!("= math.floor({target} /")));
            // The closing paren goes outside a guard that the value needs.
            let (va, vb) = self.ctx.bytes(value.span());

            if !support::is_atomic(value) {
                insert(va, "(", self.edits);
                insert(vb, ")", self.edits);
            }

            insert(vb, ")", self.edits);

            Flow::Next
        }
    }

    walk_chunk(ctx.chunk, &mut V { ctx, edits });
}

/// make_assignment_local: change a `const` declaration back into a `local` declaration.
pub fn make_assignment_local(ctx: &RuleCtx, edits: &mut Vec<Edit>) {
    struct V<'a, 'b> {
        ctx: &'a RuleCtx<'b>,
        edits: &'a mut Vec<Edit>,
    }

    impl Visit for V<'_, '_> {
        fn stmt(&mut self, s: &Stmt) -> Flow {
            let Stmt::Local(l) = s else { return Flow::Next };

            if !l.is_const {
                return Flow::Next;
            }

            self.ctx.replace(l.keyword, "local".to_string(), self.edits);

            Flow::Next
        }
    }

    walk_chunk(ctx.chunk, &mut V { ctx, edits });
}

/*
remove_nil_declaration: `local x = nil` becomes `local x`. The rule also
removes trailing nils in a multi binding. A const must keep its value, so
the rule skips it.
*/
pub fn remove_nil_declaration(ctx: &RuleCtx, edits: &mut Vec<Edit>) {
    struct V<'a, 'b> {
        ctx: &'a RuleCtx<'b>,
        edits: &'a mut Vec<Edit>,
    }

    impl Visit for V<'_, '_> {
        fn stmt(&mut self, s: &Stmt) -> Flow {
            let Stmt::Local(l) = s else { return Flow::Next };

            if l.is_const || l.values.is_empty() {
                return Flow::Next;
            }

            let mut keep = l.values.len();

            while keep > 0 && matches!(l.values[keep - 1], Expr::Nil(_)) {
                keep -= 1;
            }

            if keep == l.values.len() {
                return Flow::Next;
            }

            let last_end = self.ctx.bytes(l.values.last().unwrap().span()).1;
            let from = if keep == 0 {
                // Each value was nil. The `=` goes with them.
                let Some(eq) = l.values[0].span().start.checked_sub(1) else {
                    return Flow::Next;
                };

                if self.ctx.tok_text(eq) != "=" {
                    return Flow::Next;
                }

                trim_back(self.ctx, tok_bytes(self.ctx, eq).0)
            } else {
                let Some(comma) = l.values[keep].span().start.checked_sub(1) else {
                    return Flow::Next;
                };

                if self.ctx.tok_text(comma) != "," {
                    return Flow::Next;
                }

                tok_bytes(self.ctx, comma).0
            };

            self.ctx.delete_bytes_keep_lines(from, last_end, self.edits);

            Flow::Next
        }
    }

    walk_chunk(ctx.chunk, &mut V { ctx, edits });
}

/*
group_local_assignment: fold a run of adjacent `local` declarations into
one declaration.

The rule is conservative by design. Each statement in the run must bind
exactly as many values as names. The values must call nothing. A later
statement must not read a name that an earlier statement bound. Without
these checks, the merged form would read the outer binding instead. Type
annotations, const, comments, and blank space between the statements all
stop a run.
*/
pub fn group_local_assignment(ctx: &RuleCtx, edits: &mut Vec<Edit>) {
    struct V<'a, 'b> {
        ctx: &'a RuleCtx<'b>,
        edits: &'a mut Vec<Edit>,
    }

    impl Visit for V<'_, '_> {
        fn block(&mut self, b: &Block) -> Flow {
            let mut i = 0;

            while i < b.stmts.len() {
                let run = run_length(self.ctx, &b.stmts, i);

                if run >= 2 {
                    emit_group(self.ctx, &b.stmts[i..i + run], self.edits);
                    i += run;
                } else {
                    i += 1;
                }
            }

            Flow::Next
        }
    }

    walk_chunk(ctx.chunk, &mut V { ctx, edits });
}

/// True when a local declaration is simple enough to fold into its neighbours.
fn groupable(s: &Stmt) -> bool {
    let Stmt::Local(l) = s else { return false };

    if l.is_const || l.values.is_empty() || l.values.len() != l.names.len() {
        return false;
    }

    if l.names.iter().any(|n| n.ty.is_some()) {
        return false;
    }

    !l.values
        .iter()
        .any(|v| support::has_call(v) || matches!(v, Expr::Vararg(_)))
}

/// The count of statements from `from` that the rule can fold together.
fn run_length(ctx: &RuleCtx, stmts: &[Stmt], from: usize) -> usize {
    if !groupable(&stmts[from]) {
        return 0;
    }

    let mut bound: Vec<&str> = local_names(ctx, &stmts[from]);
    let mut end = from + 1;

    while end < stmts.len() {
        if !groupable(&stmts[end]) {
            break;
        }

        let Stmt::Local(l) = &stmts[end] else { break };
        // Only whitespace is permitted between two statements that the rule joins.
        let gap_from = ctx.bytes(stmts[end - 1].span()).1;
        let gap_to = ctx.bytes(stmts[end].span()).0;

        if !ctx.src[gap_from as usize..gap_to as usize]
            .bytes()
            .all(|c| c.is_ascii_whitespace())
        {
            break;
        }

        if l.values.iter().any(|v| reads_any(ctx, v, &bound)) {
            break;
        }

        let names = local_names(ctx, &stmts[end]);

        if names.iter().any(|n| bound.contains(n)) {
            break;
        }

        bound.extend(names);
        end += 1;
    }

    end - from
}

fn local_names<'a>(ctx: &RuleCtx<'a>, s: &Stmt) -> Vec<&'a str> {
    match s {
        Stmt::Local(l) => l.names.iter().map(|n| ctx.text(n.name)).collect(),

        _ => Vec::new(),
    }
}

/// True when this value reads a name that an earlier statement in the run bound.
fn reads_any(ctx: &RuleCtx, e: &Expr, names: &[&str]) -> bool {
    struct V<'a, 'b> {
        ctx: &'a RuleCtx<'b>,
        names: &'a [&'a str],
        found: bool,
    }

    impl Visit for V<'_, '_> {
        fn expr(&mut self, e: &Expr) -> Flow {
            if let Expr::Name(s) = e
                && self.names.contains(&self.ctx.text(*s))
            {
                self.found = true;
            }

            Flow::Next
        }
    }

    let mut v = V {
        ctx,
        names,
        found: false,
    };

    crate::rules::engine::walk_expr(e, &mut v);

    v.found
}

fn emit_group(ctx: &RuleCtx, run: &[Stmt], edits: &mut Vec<Edit>) {
    let from = ctx.bytes(run[0].span()).0;
    let to = ctx.bytes(run[run.len() - 1].span()).1;

    // The replacement would remove a comment inside the run.
    if support::has_comment_in(ctx, from, to) {
        return;
    }

    let mut names: Vec<&str> = Vec::new();
    let mut values: Vec<&str> = Vec::new();

    for s in run {
        let Stmt::Local(l) = s else { return };

        for n in &l.names {
            names.push(ctx.text(n.name));
        }

        for v in &l.values {
            values.push(ctx.text(v.span()));
        }
    }

    let text = format!("local {} = {}", names.join(", "), values.join(", "));
    support::replace_keep_lines(ctx, from, to, &text, edits);
}

#[cfg(test)]
mod tests {
    use super::super::testing::{assert_lines_kept, run};
    use super::*;

    fn compound(ctx: &RuleCtx, edits: &mut Vec<Edit>) {
        remove_compound_assignment(ctx, edits, false);
    }

    #[test]
    fn compound_assignment_expands() {
        assert_eq!(run("x += 1\n", compound), "x = x + 1\n");
        assert_eq!(run("x -= 1\n", compound), "x = x - 1\n");
        assert_eq!(run("x *= 2\n", compound), "x = x * 2\n");
        assert_eq!(run("x /= 2\n", compound), "x = x / 2\n");
        assert_eq!(run("x %= 2\n", compound), "x = x % 2\n");
        assert_eq!(run("x ^= 2\n", compound), "x = x ^ 2\n");
        assert_eq!(run("s ..= \"a\"\n", compound), "s = s .. \"a\"\n");
        assert_eq!(run("a.b.c += 1\n", compound), "a.b.c = a.b.c + 1\n");
        assert_eq!(run("t[k] += 1\n", compound), "t[k] = t[k] + 1\n");
    }

    #[test]
    fn compound_assignment_parenthesises_the_value() {
        // `x = x - a - b` would be wrong. The value must stay one unit.
        assert_eq!(run("x -= a - b\n", compound), "x = x - (a - b)\n");
        assert_eq!(run("x /= a * b\n", compound), "x = x / (a * b)\n");
    }

    #[test]
    fn compound_assignment_skips_unsafe_targets() {
        // A call in the key would run twice.
        let src = "t[f()] += 1\n";
        assert_eq!(run(src, compound), src);
        // A plain assignment is not a target of this rule.
        let src = "x = 1\n";
        assert_eq!(run(src, compound), src);
    }

    #[test]
    fn floor_division_wraps_in_math_floor() {
        assert_eq!(
            run("local x = a // b\n", remove_floor_division),
            "local x = math.floor(a / b)\n"
        );
        // The precedence equals `/`, so the surrounding parse does not change.
        assert_eq!(
            run("local x = a + b // c\n", remove_floor_division),
            "local x = a + math.floor(b / c)\n"
        );
        assert_eq!(
            run("local x = a // b + c\n", remove_floor_division),
            "local x = math.floor(a / b) + c\n"
        );
    }

    #[test]
    fn nested_floor_division_nests_the_calls() {
        assert_eq!(
            run("local x = a // b // c\n", remove_floor_division),
            "local x = math.floor(math.floor(a / b) / c)\n"
        );
    }

    #[test]
    fn floor_division_handles_the_compound_form() {
        assert_eq!(
            run("x //= 2\n", remove_floor_division),
            "x = math.floor(x / 2)\n"
        );
        assert_eq!(
            run("x //= a + b\n", remove_floor_division),
            "x = math.floor(x / (a + b))\n"
        );
    }

    #[test]
    fn plain_division_is_left_alone() {
        let src = "local x = a / b\n";
        assert_eq!(run(src, remove_floor_division), src);
    }

    #[test]
    fn const_becomes_local() {
        assert_eq!(
            run("const X = require(\"./x\")\n", make_assignment_local),
            "local X = require(\"./x\")\n"
        );
        let src = "local X = 1\n";
        assert_eq!(run(src, make_assignment_local), src);
    }

    #[test]
    fn nil_declarations_shrink() {
        assert_eq!(run("local x = nil\n", remove_nil_declaration), "local x\n");
        assert_eq!(
            run("local a, b = 1, nil\n", remove_nil_declaration),
            "local a, b = 1\n"
        );
        assert_eq!(
            run("local a, b = nil, nil\n", remove_nil_declaration),
            "local a, b\n"
        );
    }

    #[test]
    fn nil_declarations_leave_the_rest_alone() {
        // A leading nil is positional. The rule cannot remove it.
        let src = "local a, b = nil, 1\n";
        assert_eq!(run(src, remove_nil_declaration), src);
        // A const must keep its value.
        let src = "const x = nil\n";
        assert_eq!(run(src, remove_nil_declaration), src);
        let src = "local x\n";
        assert_eq!(run(src, remove_nil_declaration), src);
    }

    #[test]
    fn adjacent_locals_group() {
        let src = "local a = 1\nlocal b = 2\n";
        let out = run(src, group_local_assignment);

        assert!(out.starts_with("local a, b = 1, 2\n"), "{out}");
        assert_lines_kept(src, &out);
    }

    #[test]
    fn grouping_stops_at_a_dependency() {
        // b reads a. A merge would make it read the outer a.
        let src = "local a = 1\nlocal b = a\n";
        assert_eq!(run(src, group_local_assignment), src);
        // A call could observe the order.
        let src = "local a = f()\nlocal b = 2\n";
        assert_eq!(run(src, group_local_assignment), src);
        // A merge would remove a comment between them.
        let src = "local a = 1\n-- note\nlocal b = 2\n";
        assert_eq!(run(src, group_local_assignment), src);
        // The rule does not change an annotated binding.
        let src = "local a: number = 1\nlocal b = 2\n";
        assert_eq!(run(src, group_local_assignment), src);
    }
}
