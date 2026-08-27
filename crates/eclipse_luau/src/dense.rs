/*!
The dense generator: the tokens of a file, with the least whitespace that
lexes the same.

The emitter never invents, drops, or edits a token. It only changes the
trivia between tokens, so the token stream of the output equals the token
stream of the input, and the program cannot change meaning. Comments are
trivia, so a dense file has none. All size beyond that comes from rules,
ex: `rename_variables`, which edit tokens before this emitter runs.

Two token pairs must keep a separator, or the pair would lex as one token:
a word against a word (`local x`), and the character pairs that merge into
a longer operator (`1 ..`, `- -`, `> =`). The table in `must_separate`
holds those, and each entry states the merge it prevents.

One pair keeps its newline: an expression, then a statement that opens with
`(`. Luau reads a `(` after an expression as a call unless a line break or a
`;` separates the two. The emitter changes no token, so it cannot add the
`;`. It keeps the break instead, and it never inserts a break before a `(`
for the same reason in the other direction.
*/

use crate::lexer::{self, Tok};

/// The dense text of one Luau source, broken near `column_span` columns
pub fn dense(src: &str, column_span: usize) -> Result<String, String> {
    let lexed = lexer::lex(src).map_err(|e| e.message)?;

    let mut out = String::with_capacity(src.len() / 2);
    let mut column = 0usize;

    for (i, tok) in lexed.toks.iter().enumerate() {
        let text = tok.text(src);

        if i > 0 {
            let prev = &lexed.toks[i - 1];

            if keeps_newline(prev, tok, src) {
                out.push('\n');
                column = 0;
            } else {
                let space = must_separate(prev, tok, src);
                let width = usize::from(space) + text.chars().count();

                /*
                A break happens before a token, never inside one. A token
                wider than the span stays whole, so a long string can pass
                the column and the line ends after it.
                */
                if column + width > column_span && column > 0 && !text.starts_with('(') {
                    out.push('\n');
                    column = 0;
                } else if space {
                    out.push(' ');
                    column += 1;
                }
            }
        }

        out.push_str(text);

        // A long string can hold newlines; the column continues after the last one.
        match text.rfind('\n') {
            Some(at) => column = text[at + 1..].chars().count(),

            None => column += text.chars().count(),
        }
    }

    if !out.is_empty() {
        out.push('\n');
    }

    Ok(out)
}

/*
True when the source broke the line here and the next token opens with `(`.

Joining that pair would turn two statements into one call. The break stays,
and the output means what the input meant, whatever Luau decides about the
ambiguity.
*/
fn keeps_newline(prev: &Tok, next: &Tok, src: &str) -> bool {
    next.text(src).starts_with('(') && src[prev.end as usize..next.start as usize].contains('\n')
}

/// True when the two tokens would lex as one without a separator
fn must_separate(prev: &Tok, next: &Tok, src: &str) -> bool {
    let last = prev.text(src).chars().next_back();
    let first = next.text(src).chars().next();

    let (Some(last), Some(first)) = (last, first) else {
        return false;
    };

    let word = |c: char| c.is_alphanumeric() || c == '_';

    // `local x`, `and y`, `1 x`: two words in a row stay two words.
    if word(last) && word(first) {
        return true;
    }

    match (last, first) {
        // `1 ..` and `.. .5`: a dot after a dot or a number grows the operator.
        ('.', '.') => true,

        /*
        The digit has to belong to a number. `_0x0 .new` needs no space,
        because the lexer reads a word that starts with a letter or an
        underscore whole, whatever it ends with, and a rename hands out
        names like that.
        */
        ('0'..='9', '.') if prev.kind == crate::lexer::TokKind::Number => true,

        // `- -x`: two minus signs in a row open a comment.
        ('-', '-') => true,

        // `t[ [[x]] ]`: a bracket before a bracket opens a long string.
        ('[', '[') => true,

        // `A<T> = x`: an equals sign extends `>`, `<`, `=`, and `~`.
        ('=' | '<' | '>' | '~', '=') => true,

        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one_line(src: &str) -> String {
        dense(src, usize::MAX).unwrap()
    }

    /// The token streams must match; that is the whole safety argument.
    fn same_tokens(a: &str, b: &str) {
        let ta = lexer::lex(a).unwrap().toks;
        let tb = lexer::lex(b).unwrap().toks;

        assert_eq!(ta.len(), tb.len(), "{a:?} vs {b:?}");

        for (x, y) in ta.iter().zip(&tb) {
            assert_eq!(x.text(a), y.text(b), "{a:?} vs {b:?}");
        }
    }

    #[test]
    fn whitespace_shrinks_and_the_tokens_survive() {
        let src = "local  x   =  1\n\nreturn   x + 2\n";
        let out = one_line(src);

        assert_eq!(out, "local x=1 return x+2\n");
        same_tokens(src, &out);
    }

    #[test]
    fn comments_are_trivia_and_do_not_survive() {
        let out = one_line("-- gone\nlocal x = 1 --[[ also gone ]]\nreturn x\n");

        assert_eq!(out, "local x=1 return x\n");
    }

    #[test]
    fn words_keep_a_space_and_operators_do_not() {
        let out = one_line("local x = not a and b or c\n");

        assert_eq!(out, "local x=not a and b or c\n");
    }

    /// A word that ends in a digit is still a word, so `.` can follow it.
    #[test]
    fn a_name_that_ends_in_a_digit_needs_no_space_before_a_dot() {
        let src = "local _0x0 = t\nreturn _0x0.field\n";
        let out = one_line(src);

        same_tokens(src, &out);
        assert_eq!(out, "local _0x0=t return _0x0.field\n");
    }

    #[test]
    fn a_number_before_a_dot_keeps_its_space() {
        let src = "return 1 .. 2 .. .5\n";
        let out = one_line(src);

        same_tokens(src, &out);
        // `..2` lexes as `..` then `2`, so only the dot-adjacent pairs keep a space.
        assert_eq!(out, "return 1 ..2 .. .5\n");
    }

    #[test]
    fn two_minus_signs_do_not_become_a_comment() {
        let src = "return - -x\n";
        let out = one_line(src);

        same_tokens(src, &out);
    }

    #[test]
    fn a_bracket_pair_does_not_become_a_long_string() {
        let src = "return t[ [[key]] ]\n";
        let out = one_line(src);

        same_tokens(src, &out);
    }

    #[test]
    fn a_generic_close_does_not_eat_the_equals() {
        let src = "local x: Map<string, number> = {}\nreturn x\n";
        let out = one_line(src);

        same_tokens(src, &out);
    }

    /// The classic ambiguity: an expression, then a statement opening with `(`.
    #[test]
    fn a_paren_statement_keeps_its_line_break() {
        let src = "local f = g\n(h)()\n";
        let out = one_line(src);

        assert_eq!(out, "local f=g\n(h)()\n");
    }

    #[test]
    fn lines_break_near_the_column_span() {
        let src = "local abc = 1\nlocal def = 2\nlocal ghi = 3\n";
        let out = dense(src, 24).unwrap();

        for line in out.lines() {
            assert!(line.chars().count() <= 24, "{line:?} is over the span");
        }

        same_tokens(src, &out);
    }

    #[test]
    fn a_token_wider_than_the_span_stays_whole() {
        let src = "return \"a string wider than the span\"\n";
        let out = dense(src, 8).unwrap();

        same_tokens(src, &out);
    }

    #[test]
    fn a_long_string_keeps_its_newlines_and_the_column_recovers() {
        let src = "local s = [[a\nb]]\nlocal t = 2\n";
        let out = one_line(src);

        same_tokens(src, &out);
        assert!(out.contains("[[a\nb]]"));
    }

    #[test]
    fn an_interpolated_string_is_one_token_and_passes_through() {
        let src = "local n = 1\nreturn `n is {n}`\n";
        let out = one_line(src);

        same_tokens(src, &out);
    }

    #[test]
    fn dense_output_is_a_fixed_point() {
        let src = "local function f(a, b)\n\treturn a + b\nend\n\nreturn f(1, 2)\n";
        let once = dense(src, 40).unwrap();
        let twice = dense(&once, 40).unwrap();

        assert_eq!(once, twice);
    }

    #[test]
    fn empty_input_stays_empty() {
        assert_eq!(one_line(""), "");
        assert_eq!(one_line("-- only a comment\n"), "");
    }
}
