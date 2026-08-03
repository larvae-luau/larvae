/*!
Rules that rewrite a single expression in place
*/

use super::support::{self, tok_bytes};
use crate::rules::engine::{Edit, Flow, RuleCtx, Visit, walk_chunk};
use crate::syntax::ast::*;

/*
remove_if_expression, `if c then a else b` becomes `c and a or b`

The and/or trick only reproduces the if expression when the then value can
never be false or nil, otherwise the or arm would take over and the wrong
value comes out. darklua wraps those cases in a helper function, larvae
would rather leave them readable, so anything unprovable is skipped
*/
pub fn remove_if_expression(ctx: &RuleCtx, edits: &mut Vec<Edit>) {
    struct V<'a, 'b> {
        ctx: &'a RuleCtx<'b>,
        edits: &'a mut Vec<Edit>,
    }

    impl Visit for V<'_, '_> {
        fn expr(&mut self, e: &Expr) -> Flow {
            let Expr::IfElse {
                branches,
                else_value,
                span,
            } = e
            else {
                return Flow::Next;
            };

            let Some(text) = build_and_or(self.ctx, branches, else_value, 0) else {
                return Flow::Next;
            };

            let (a, b) = self.ctx.bytes(*span);
            support::replace_keep_lines(self.ctx, a, b, &text, self.edits);

            Flow::Next
        }
    }

    walk_chunk(ctx.chunk, &mut V { ctx, edits });
}

fn build_and_or(
    ctx: &RuleCtx,
    branches: &[(Expr, Expr)],
    else_value: &Expr,
    idx: usize,
) -> Option<String> {
    if idx == branches.len() {
        return Some(support::operand_text(ctx, else_value));
    }

    let (cond, value) = &branches[idx];

    if !support::is_never_falsy(value) {
        return None;
    }

    let tail = build_and_or(ctx, branches, else_value, idx + 1)?;
    // a nested chain has to stay one operand of the or above it
    let tail = if idx + 1 < branches.len() {
        format!("({tail})")
    } else {
        tail
    };

    Some(format!(
        "{} and {} or {}",
        support::operand_text(ctx, cond),
        support::operand_text(ctx, value),
        tail
    ))
}

/*
convert_index_to_field, `t["field"]` becomes `t.field`, and the same for a
table constructor key, only when the literal spells a name Luau accepts
after a dot
*/
pub fn convert_index_to_field(ctx: &RuleCtx, edits: &mut Vec<Edit>) {
    struct V<'a, 'b> {
        ctx: &'a RuleCtx<'b>,
        edits: &'a mut Vec<Edit>,
    }

    impl Visit for V<'_, '_> {
        fn expr(&mut self, e: &Expr) -> Flow {
            match e {
                Expr::Index {
                    key: IndexKey::Computed(k),
                    ..
                } => {
                    if let Some((from, to, name)) = bracket_swap(self.ctx, k) {
                        self.edits.push((from, to, format!(".{name}")));
                    }
                }

                Expr::Table { fields, .. } => {
                    for f in fields {
                        if let TableField::Computed { key, .. } = f
                            && let Some((from, to, name)) = bracket_swap(self.ctx, key)
                        {
                            self.edits.push((from, to, name.to_string()));
                        }
                    }
                }

                _ => {}
            }

            Flow::Next
        }
    }

    walk_chunk(ctx.chunk, &mut V { ctx, edits });
}

/// The `[` to `]` byte range around a string key, plus the name it spells
fn bracket_swap<'a>(ctx: &RuleCtx<'a>, key: &Expr) -> Option<(u32, u32, &'a str)> {
    let Expr::String(span) = key else { return None };
    let name = support::plain_string_value(ctx, *span)?;

    if !support::is_ident(name) {
        return None;
    }

    let open = span.start.checked_sub(1)?;
    let close = span.end;

    if ctx.tok_text(open) != "[" || ctx.toks.len() as u32 <= close || ctx.tok_text(close) != "]" {
        return None;
    }

    Some((tok_bytes(ctx, open).0, tok_bytes(ctx, close).1, name))
}

/*
convert_luau_number, spell binary literals in hex and drop digit separators,
both are Luau only syntax that plain Lua would reject
*/
pub fn convert_luau_number(ctx: &RuleCtx, edits: &mut Vec<Edit>) {
    struct V<'a, 'b> {
        ctx: &'a RuleCtx<'b>,
        edits: &'a mut Vec<Edit>,
    }

    impl Visit for V<'_, '_> {
        fn expr(&mut self, e: &Expr) -> Flow {
            let Expr::Number(span) = e else {
                return Flow::Next;
            };
            let text = self.ctx.text(*span);

            if let Some(new) = rewrite_number(text) {
                let (a, b) = self.ctx.bytes(*span);
                self.edits.push((a, b, new));
            }

            Flow::Next
        }
    }

    walk_chunk(ctx.chunk, &mut V { ctx, edits });
}

fn rewrite_number(text: &str) -> Option<String> {
    let cleaned: String = text.chars().filter(|&c| c != '_').collect();
    let lower = cleaned.to_ascii_lowercase();

    if let Some(bits) = lower.strip_prefix("0b") {
        if bits.is_empty() || !bits.chars().all(|c| c == '0' || c == '1') {
            return None;
        }

        let value = u64::from_str_radix(bits, 2).ok()?;

        return Some(format!("0x{value:X}"));
    }

    (cleaned != text).then_some(cleaned)
}

/*
remove_function_call_parens, `f("x")` becomes `f"x"` and `f({})` becomes
`f{}`, Luau's parenless call forms take exactly one string or table
*/
pub fn remove_function_call_parens(ctx: &RuleCtx, edits: &mut Vec<Edit>) {
    struct V<'a, 'b> {
        ctx: &'a RuleCtx<'b>,
        edits: &'a mut Vec<Edit>,
    }

    impl Visit for V<'_, '_> {
        fn expr(&mut self, e: &Expr) -> Flow {
            let Expr::Call { args, span, .. } = e else {
                return Flow::Next;
            };

            let CallArgs::Paren(list) = args else {
                return Flow::Next;
            };

            if list.len() != 1 {
                return Flow::Next;
            }

            let arg = &list[0];

            if !matches!(arg, Expr::String(_) | Expr::Table { .. }) {
                return Flow::Next;
            }

            let Some(open) = arg.span().start.checked_sub(1) else {
                return Flow::Next;
            };

            let close = span.end - 1;

            if self.ctx.tok_text(open) != "(" || self.ctx.tok_text(close) != ")" {
                return Flow::Next;
            }

            let (oa, ob) = tok_bytes(self.ctx, open);
            let (ca, cb) = tok_bytes(self.ctx, close);

            self.edits.push((oa, ob, String::new()));
            self.edits.push((ca, cb, String::new()));

            Flow::Next
        }
    }

    walk_chunk(ctx.chunk, &mut V { ctx, edits });
}

/*
convert_square_root_call, `math.sqrt(x)` becomes `(x ^ 0.5)`

The result is always parenthesised. A bare `x ^ 0.5` would re-associate as
the base of another power and would not survive being indexed or called, and
proving which of those applies costs more than the parentheses do
*/
pub fn convert_square_root_call(ctx: &RuleCtx, edits: &mut Vec<Edit>) {
    struct V<'a, 'b> {
        ctx: &'a RuleCtx<'b>,
        edits: &'a mut Vec<Edit>,
        /// Calls used as statements, an expression cannot replace those
        statement_calls: Vec<TokSpan>,
    }

    impl Visit for V<'_, '_> {
        fn stmt(&mut self, s: &Stmt) -> Flow {
            if let Stmt::Call(e, _) = s {
                self.statement_calls.push(e.span());
            }

            Flow::Next
        }

        fn expr(&mut self, e: &Expr) -> Flow {
            let Expr::Call {
                func,
                method,
                args,
                span,
            } = e
            else {
                return Flow::Next;
            };

            if method.is_some() || !is_math_sqrt(self.ctx, func) {
                return Flow::Next;
            }

            let CallArgs::Paren(list) = args else {
                return Flow::Next;
            };

            if list.len() != 1 || self.statement_calls.contains(span) {
                return Flow::Next;
            }

            let base = support::operand_text(self.ctx, &list[0]);
            let (a, b) = self.ctx.bytes(*span);

            support::replace_keep_lines(self.ctx, a, b, &format!("({base} ^ 0.5)"), self.edits);

            Flow::Next
        }
    }

    walk_chunk(
        ctx.chunk,
        &mut V {
            ctx,
            edits,
            statement_calls: Vec::new(),
        },
    );
}

fn is_math_sqrt(ctx: &RuleCtx, func: &Expr) -> bool {
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

    ctx.text(*base) == "math" && ctx.text(*field) == "sqrt"
}

#[cfg(test)]
mod tests {
    use super::super::testing::{assert_lines_kept, run};
    use super::*;

    #[test]
    fn if_expressions_become_and_or() {
        assert_eq!(
            run("local x = if c then 1 else 2\n", remove_if_expression),
            "local x = c and 1 or 2\n"
        );
        assert_eq!(
            run(
                "local x = if a > b then \"y\" else \"n\"\n",
                remove_if_expression
            ),
            "local x = (a > b) and \"y\" or \"n\"\n"
        );
    }

    #[test]
    fn elseif_chains_nest() {
        assert_eq!(
            run(
                "local x = if a then 1 elseif b then 2 else 3\n",
                remove_if_expression
            ),
            "local x = a and 1 or (b and 2 or 3)\n"
        );
    }

    #[test]
    fn falsy_then_values_are_left_alone() {
        // `c and nil or 2` would always give 2, the rewrite is not equivalent
        let src = "local x = if c then nil else 2\n";
        assert_eq!(run(src, remove_if_expression), src);
        let src = "local x = if c then false else 2\n";
        assert_eq!(run(src, remove_if_expression), src);
        // a plain name could be nil at runtime
        let src = "local x = if c then a else b\n";
        assert_eq!(run(src, remove_if_expression), src);
    }

    #[test]
    fn string_keys_become_fields() {
        assert_eq!(
            run("local v = t[\"field\"]\n", convert_index_to_field),
            "local v = t.field\n"
        );
        assert_eq!(
            run("local t = { [\"k\"] = 1 }\n", convert_index_to_field),
            "local t = { k = 1 }\n"
        );
    }

    #[test]
    fn keys_that_are_not_names_stay_indexed() {
        let src = "local v = t[\"two words\"]\n";
        assert_eq!(run(src, convert_index_to_field), src);
        // a reserved word cannot follow a dot
        let src = "local v = t[\"end\"]\n";
        assert_eq!(run(src, convert_index_to_field), src);
        let src = "local v = t[\"1st\"]\n";
        assert_eq!(run(src, convert_index_to_field), src);
        // an escape means the bytes are not what they look like
        let src = "local v = t[\"a\\98c\"]\n";
        assert_eq!(run(src, convert_index_to_field), src);
        let src = "local v = t[k]\n";
        assert_eq!(run(src, convert_index_to_field), src);
    }

    #[test]
    fn luau_numbers_become_plain_lua() {
        assert_eq!(
            run("local x = 0b1010\n", convert_luau_number),
            "local x = 0xA\n"
        );
        assert_eq!(
            run("local x = 1_000_000\n", convert_luau_number),
            "local x = 1000000\n"
        );
        assert_eq!(
            run("local x = 0xFF_FF\n", convert_luau_number),
            "local x = 0xFFFF\n"
        );
    }

    #[test]
    fn ordinary_numbers_are_untouched() {
        let src = "local x = 42\nlocal y = 0xFF\nlocal z = 1.5e3\n";
        assert_eq!(run(src, convert_luau_number), src);
    }

    #[test]
    fn single_literal_arguments_drop_their_parens() {
        assert_eq!(run("f(\"x\")\n", remove_function_call_parens), "f\"x\"\n");
        assert_eq!(run("f({})\n", remove_function_call_parens), "f{}\n");
        assert_eq!(
            run("obj:m(\"x\")\n", remove_function_call_parens),
            "obj:m\"x\"\n"
        );
    }

    #[test]
    fn other_call_shapes_keep_their_parens() {
        let src = "f(x)\n";
        assert_eq!(run(src, remove_function_call_parens), src);
        let src = "f(\"a\", \"b\")\n";
        assert_eq!(run(src, remove_function_call_parens), src);
        let src = "f()\n";
        assert_eq!(run(src, remove_function_call_parens), src);
    }

    #[test]
    fn square_root_becomes_a_power() {
        assert_eq!(
            run("local d = math.sqrt(x)\n", convert_square_root_call),
            "local d = (x ^ 0.5)\n"
        );
        assert_eq!(
            run("local d = math.sqrt(a + b)\n", convert_square_root_call),
            "local d = ((a + b) ^ 0.5)\n"
        );
        // the parens keep it correct as the base of another power
        assert_eq!(
            run("local d = math.sqrt(x) ^ 2\n", convert_square_root_call),
            "local d = (x ^ 0.5) ^ 2\n"
        );
    }

    #[test]
    fn other_math_calls_are_left_alone() {
        let src = "local d = math.floor(x)\n";
        assert_eq!(run(src, convert_square_root_call), src);
        let src = "local d = sqrt(x)\n";
        assert_eq!(run(src, convert_square_root_call), src);
        // two arguments is not the sqrt we know
        let src = "local d = math.sqrt(x, y)\n";
        assert_eq!(run(src, convert_square_root_call), src);
        // an expression cannot stand in for a statement
        let src = "math.sqrt(x)\n";
        assert_eq!(run(src, convert_square_root_call), src);
    }

    #[test]
    fn multiline_if_expression_keeps_its_lines() {
        let src = "local x = if c then\n    1\nelse\n    2\n";
        let out = run(src, remove_if_expression);

        assert_lines_kept(src, &out);
    }
}
