/*!
remove_interpolated_string

The lexer keeps a backtick string as one opaque token, so this rule
splits out the pieces. The result is a string.format call. The `string`
strategy wraps each value in tostring and formats with `%s`. The
`tostring` strategy uses Luau's `%*` instead.

The rule keeps each string that the split cannot handle. This covers a
raw newline in the literal, because no quoted string can hold one without
a shift in line numbers. It also covers a nested backtick string, because
this single pass would never visit it again.
*/

use crate::rules::engine::{Edit, Flow, RuleCtx, Visit, walk_chunk};
use crate::syntax::ast::*;

/// One piece of a split backtick string.
enum Piece {
    /// Literal text, already escaped for a quoted string.
    Text(String),
    /// The source text of an interpolated expression.
    Value(String),
}

pub fn remove_interpolated_string(ctx: &RuleCtx, edits: &mut Vec<Edit>, strategy: &str) {
    struct V<'a, 'b> {
        ctx: &'a RuleCtx<'b>,
        edits: &'a mut Vec<Edit>,
        tostring_strategy: bool,
    }

    impl Visit for V<'_, '_> {
        fn expr(&mut self, e: &Expr) -> Flow {
            let Expr::InterpString(span) = e else {
                return Flow::Next;
            };
            let raw = self.ctx.text(*span);

            let Some(pieces) = split(raw, self.ctx.quote) else {
                return Flow::Next;
            };

            let text = render(&pieces, self.ctx.quote, self.tostring_strategy);
            let (a, b) = self.ctx.bytes(*span);

            self.edits.push((a, b, text));

            Flow::Next
        }
    }

    walk_chunk(
        ctx.chunk,
        &mut V {
            ctx,
            edits,
            tostring_strategy: strategy == "tostring",
        },
    );
}

/*
Split `\`a {x} b\`` into literal and value pieces.

The literal bytes come out ready for a quoted string. The function
unwraps the escape that Luau permits only in a backtick string. It also
escapes the quote character that the output will use. It returns None
when the transform would not be safe.
*/
fn split(raw: &str, quote: char) -> Option<Vec<Piece>> {
    let body = raw.strip_prefix('`')?.strip_suffix('`')?;
    let bytes = body.as_bytes();
    let mut pieces = Vec::new();
    let mut text = String::new();
    let mut i = 0usize;

    while i < bytes.len() {
        match bytes[i] {
            b'\\' => {
                let next = *bytes.get(i + 1)?;

                match next {
                    // The escape has meaning only in backticks. A quoted string takes it plain.
                    b'{' => text.push('{'),

                    b'`' => text.push('`'),

                    _ => {
                        let c = body[i + 1..].chars().next()?;
                        text.push('\\');
                        text.push(c);
                        i += 1 + c.len_utf8();
                        continue;
                    }
                }

                i += 2;
            }

            b'{' => {
                let (expr, next) = scan_expr(body, i)?;
                // The pass would never visit a nested backtick string again.
                if expr.contains('`') || expr.trim().is_empty() {
                    return None;
                }

                pieces.push(Piece::Text(std::mem::take(&mut text)));
                pieces.push(Piece::Value(expr.to_string()));
                i = next;
            }

            // A quoted string cannot hold a raw newline without a shift in lines.
            b'\n' => return None,

            b'%' => {
                text.push_str("%%");
                i += 1;
            }

            c if c == quote as u8 => {
                text.push('\\');
                text.push(quote);
                i += 1;
            }

            _ => {
                let c = body[i..].chars().next()?;
                text.push(c);
                i += c.len_utf8();
            }
        }
    }

    pieces.push(Piece::Text(text));

    Some(pieces)
}

/// Find the expression inside `{...}`. Return its text and the offset after `}`.
fn scan_expr(body: &str, open: usize) -> Option<(&str, usize)> {
    let bytes = body.as_bytes();
    let mut depth = 0usize;
    let mut i = open;

    while i < bytes.len() {
        match bytes[i] {
            b'{' => {
                depth += 1;
                i += 1;
            }

            b'}' => {
                depth -= 1;
                i += 1;

                if depth == 0 {
                    return Some((&body[open + 1..i - 1], i));
                }
            }

            // A string inside the braces must not unbalance the scan.
            q @ (b'"' | b'\'') => {
                i += 1;

                while i < bytes.len() && bytes[i] != q {
                    if bytes[i] == b'\\' {
                        i += 1;
                    }

                    i += 1;
                }

                i += 1;
            }

            b'\\' => i += 2,

            _ => i += 1,
        }
    }

    None
}

fn render(pieces: &[Piece], quote: char, tostring_strategy: bool) -> String {
    let values: Vec<&String> = pieces
        .iter()
        .filter_map(|p| match p {
            Piece::Value(v) => Some(v),

            Piece::Text(_) => None,
        })
        .collect();

    let mut format = String::new();

    for p in pieces {
        match p {
            Piece::Text(t) => format.push_str(t),

            Piece::Value(_) => format.push_str(if tostring_strategy { "%*" } else { "%s" }),
        }
    }

    // There are no values to interpolate. The input was only a string.
    if values.is_empty() {
        // The doubled percents were for a format string that larvae does not emit here.
        return format!("{quote}{}{quote}", format.replace("%%", "%"));
    }

    let args: Vec<String> = values
        .iter()
        .map(|v| {
            if tostring_strategy {
                v.trim().to_string()
            } else {
                format!("tostring({})", v.trim())
            }
        })
        .collect();
    format!("string.format({quote}{format}{quote}, {})", args.join(", "))
}

#[cfg(test)]
mod tests {
    use super::super::testing::run;
    use super::*;

    fn as_string(ctx: &RuleCtx, edits: &mut Vec<Edit>) {
        remove_interpolated_string(ctx, edits, "string");
    }

    fn as_tostring(ctx: &RuleCtx, edits: &mut Vec<Edit>) {
        remove_interpolated_string(ctx, edits, "tostring");
    }

    #[test]
    fn string_strategy_wraps_each_value() {
        assert_eq!(
            run("local s = `hello {name}`\n", as_string),
            "local s = string.format(\"hello %s\", tostring(name))\n"
        );
        assert_eq!(
            run("local s = `{a} and {b}`\n", as_string),
            "local s = string.format(\"%s and %s\", tostring(a), tostring(b))\n"
        );
    }

    #[test]
    fn tostring_strategy_uses_the_star_format() {
        assert_eq!(
            run("local s = `hello {name}`\n", as_tostring),
            "local s = string.format(\"hello %*\", name)\n"
        );
    }

    #[test]
    fn percent_signs_are_escaped() {
        assert_eq!(
            run("local s = `100% of {n}`\n", as_string),
            "local s = string.format(\"100%% of %s\", tostring(n))\n"
        );
    }

    #[test]
    fn a_plain_backtick_string_becomes_a_plain_string() {
        assert_eq!(
            run("local s = `hello`\n", as_string),
            "local s = \"hello\"\n"
        );
        // The percent needs no doubled form when there is no format call.
        assert_eq!(run("local s = `50%`\n", as_string), "local s = \"50%\"\n");
    }

    #[test]
    fn escapes_are_carried_across() {
        assert_eq!(
            run("local s = `a\\nb {x}`\n", as_string),
            "local s = string.format(\"a\\nb %s\", tostring(x))\n"
        );
        // An escaped brace is a plain brace after the backticks go away.
        assert_eq!(run("local s = `a\\{b`\n", as_string), "local s = \"a{b\"\n");
    }

    #[test]
    fn quotes_inside_get_escaped() {
        assert_eq!(
            run("local s = `say \"hi\" to {n}`\n", as_string),
            "local s = string.format(\"say \\\"hi\\\" to %s\", tostring(n))\n"
        );
    }

    #[test]
    fn expressions_keep_their_source() {
        assert_eq!(
            run("local s = `{a + b}`\n", as_string),
            "local s = string.format(\"%s\", tostring(a + b))\n"
        );
        assert_eq!(
            run("local s = `{t[\"k\"]}`\n", as_string),
            "local s = string.format(\"%s\", tostring(t[\"k\"]))\n"
        );
    }

    #[test]
    fn nested_backticks_are_left_alone() {
        // One pass can never reach the inner string again.
        let src = "local s = `outer {`inner {x}`}`\n";
        assert_eq!(run(src, as_string), src);
    }

    #[test]
    fn ordinary_strings_are_untouched() {
        let src = "local s = \"hello\"\nlocal t = 'x'\n";
        assert_eq!(run(src, as_string), src);
    }
}
