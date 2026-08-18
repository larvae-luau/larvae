/*!
A small, fast Luau lexer. Tokens carry byte spans into the source. The lexer
skips trivia, because the rewriter splices ranges and never touches the rest.
The token surface covers require scanning, and it grows with the parser
later.
*/

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokKind {
    Ident,
    /// A string literal. The inner range is the content between the quotes.
    /// The rewriter replaces that content.
    Str {
        inner_start: u32,
        inner_end: u32,
    },

    /// A backtick interpolated string. The lexer treats it as one opaque token.
    InterpStr,
    Number,
    LParen,
    RParen,
    Dot,
    Colon,
    /// Any other operator or punctuation. A multi-character operator is one token.
    Symbol,
}

#[derive(Debug, Clone, Copy)]
pub struct Tok {
    pub kind: TokKind,
    pub start: u32,
    pub end: u32,
}

impl Tok {
    pub fn text<'s>(&self, src: &'s str) -> &'s str {
        &src[self.start as usize..self.end as usize]
    }
}

#[derive(Debug)]
pub struct LexError {
    pub offset: usize,
    pub message: String,
}

/// The significant tokens, plus the comment spans that the rewriter can touch.
#[derive(Debug, Default)]
pub struct Lexed {
    pub toks: Vec<Tok>,
    /// The byte ranges of every comment, in the line form and the long form.
    pub comments: Vec<(u32, u32)>,
}

/*
The byte just past an escape sequence that opens at `at`.

`\z` takes the whitespace that follows it, newlines included. Luau keeps this
escape from Lua 5.2, and it is how an author writes one long string over
several lines. Without the rule the newline after `\z` closes the literal, and
larvae refuses a file that Luau accepts.

A `\` before the end of the line continues the string onto the next line. On
a file with CRLF endings that escape is three bytes, `\`, CR, LF, and Luau
reads it as one continuation. Two bytes here would leave a bare LF inside
the literal, and larvae would refuse a Windows checkout that Luau accepts.

Every other escape is two bytes here. The lexer only needs the extent, so it
does not read what the escape means.
*/
fn past_escape(b: &[u8], at: usize) -> usize {
    let mut i = at + 2;

    if b.get(at + 1) == Some(&b'z') {
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
    } else if b.get(at + 1) == Some(&b'\r') && b.get(i) == Some(&b'\n') {
        i += 1;
    }

    i
}

pub fn lex(src: &str) -> Result<Lexed, LexError> {
    let b = src.as_bytes();
    let mut toks = Vec::with_capacity(src.len() / 6);
    let mut comments: Vec<(u32, u32)> = Vec::new();
    let mut i = 0usize;

    macro_rules! err {
        ($pos:expr, $($msg:tt)*) => {
            return Err(LexError { offset: $pos, message: format!($($msg)*) })
        };
    }

    while i < b.len() {
        let c = b[i];

        match c {
            b' ' | b'\t' | b'\r' | b'\n' => i += 1,

            b'-' if i + 1 < b.len() && b[i + 1] == b'-' => {
                // This is a comment: the long form `--[=*[` or a line comment.
                let start = i;

                match try_long_bracket(b, i + 2) {
                    LongBracket::Ends(end) => i = end,

                    LongBracket::Unterminated => err!(start, "unterminated long comment"),

                    LongBracket::No => {
                        while i < b.len() && b[i] != b'\n' {
                            i += 1;
                        }
                    }
                }

                comments.push((start as u32, i as u32));
            }

            b'[' => match try_long_bracket(b, i) {
                LongBracket::Ends(end) => {
                    // This is a long string. The inner range excludes the brackets.
                    let level = level_of(b, i);
                    toks.push(Tok {
                        kind: TokKind::Str {
                            inner_start: (i + 2 + level) as u32,
                            inner_end: (end - 2 - level) as u32,
                        },

                        start: i as u32,
                        end: end as u32,
                    });

                    i = end;
                }

                LongBracket::Unterminated => err!(i, "unterminated long string"),

                LongBracket::No => {
                    toks.push(single(TokKind::Symbol, i));
                    i += 1;
                }
            },

            b'"' | b'\'' => {
                let quote = c;
                let start = i;

                i += 1;
                let inner_start = i;

                loop {
                    if i >= b.len() {
                        err!(start, "unterminated string literal");
                    }

                    match b[i] {
                        b'\\' => i = past_escape(b, i),

                        b'\n' => err!(start, "unterminated string literal (newline)"),

                        bb if bb == quote => break,

                        _ => i += 1,
                    }
                }

                toks.push(Tok {
                    kind: TokKind::Str {
                        inner_start: inner_start as u32,
                        inner_end: i as u32,
                    },

                    start: start as u32,
                    end: (i + 1) as u32,
                });

                i += 1;
            }

            b'`' => {
                let start = i;
                i += 1;
                let mut brace_depth = 0usize;

                loop {
                    if i >= b.len() {
                        err!(start, "unterminated interpolated string");
                    }

                    match b[i] {
                        b'\\' => i = past_escape(b, i),

                        b'{' => {
                            brace_depth += 1;
                            i += 1;
                        }

                        b'}' => {
                            brace_depth = brace_depth.saturating_sub(1);
                            i += 1;
                        }

                        // Skip nested strings inside braces. Their contents
                        // must not unbalance the depth count.
                        b'"' | b'\'' if brace_depth > 0 => {
                            let q = b[i];
                            i += 1;

                            while i < b.len() && b[i] != q {
                                if b[i] == b'\\' {
                                    i += 1;
                                }

                                i += 1;
                            }

                            i += 1;
                        }

                        b'`' if brace_depth == 0 => break,

                        _ => i += 1,
                    }
                }

                toks.push(Tok {
                    kind: TokKind::InterpStr,
                    start: start as u32,
                    end: (i + 1) as u32,
                });

                i += 1;
            }

            b'0'..=b'9' => {
                let start = i;
                i = scan_number(b, i);
                toks.push(Tok {
                    kind: TokKind::Number,
                    start: start as u32,
                    end: i as u32,
                });
            }

            b'.' if i + 1 < b.len() && b[i + 1].is_ascii_digit() => {
                // `.5` is a number. Every other dot is an operator.
                let start = i;
                i = scan_number(b, i);
                toks.push(Tok {
                    kind: TokKind::Number,
                    start: start as u32,
                    end: i as u32,
                });
            }

            b'A'..=b'Z' | b'a'..=b'z' | b'_' => {
                let start = i;

                while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                    i += 1;
                }

                toks.push(Tok {
                    kind: TokKind::Ident,
                    start: start as u32,
                    end: i as u32,
                });
            }

            /*
            A byte with the high bit set starts a character that only Luau
            string and comment content can hold. The code above consumes both
            whole. So a byte that reaches this arm sits in code, and Luau
            does not allow it there either.

            The lexer reports it instead of a token, because a one-byte token
            would cut the character in half. Every later slice of that span
            would then panic on the character boundary.
            */
            _ if c >= 0x80 => {
                let ch = src[i..]
                    .chars()
                    .next()
                    .unwrap_or(char::REPLACEMENT_CHARACTER);

                err!(i, "unexpected character {ch:?}")
            }

            _ => {
                let (len, kind) = symbol_at(b, i);
                toks.push(Tok {
                    kind,
                    start: i as u32,
                    end: (i + len) as u32,
                });

                i += len;
            }
        }
    }

    Ok(Lexed { toks, comments })
}

/// Finds the longest operator or punctuation at `pos`. It returns the length and the kind.
fn symbol_at(b: &[u8], pos: usize) -> (usize, TokKind) {
    const THREE: [&[u8]; 3] = [b"...", b"..=", b"//="];
    const TWO: [&[u8]; 14] = [
        b"..", b"==", b"~=", b"<=", b">=", b"->", b"::", b"+=", b"-=", b"*=", b"/=", b"%=", b"^=",
        b"//",
    ];
    let rest = &b[pos..];

    for pat in THREE {
        if rest.starts_with(pat) {
            return (3, TokKind::Symbol);
        }
    }

    for pat in TWO {
        if rest.starts_with(pat) {
            return (2, TokKind::Symbol);
        }
    }

    let kind = match b[pos] {
        b'(' => TokKind::LParen,

        b')' => TokKind::RParen,

        b'.' => TokKind::Dot,

        b':' => TokKind::Colon,

        _ => TokKind::Symbol,
    };

    (1, kind)
}

#[allow(dead_code)]
fn single(kind: TokKind, i: usize) -> Tok {
    Tok {
        kind,
        start: i as u32,
        end: (i + 1) as u32,
    }
}

fn level_of(b: &[u8], open: usize) -> usize {
    let mut level = 0;
    let mut j = open + 1;

    while j < b.len() && b[j] == b'=' {
        level += 1;
        j += 1;
    }

    level
}

/// Describes what sits at a position that can open a `[[` or `--[==[` bracket.
enum LongBracket {
    /// This is not a long bracket. So `[` is punctuation, and `--` starts a line comment.
    No,
    /// The bracket closes just past this byte.
    Ends(usize),
    /// The bracket opened and never closed. Luau rejects this, and larvae rejects it too.
    Unterminated,
}

fn try_long_bracket(b: &[u8], pos: usize) -> LongBracket {
    if pos >= b.len() || b[pos] != b'[' {
        return LongBracket::No;
    }

    let level = level_of(b, pos);
    let body = pos + 1 + level;

    if body >= b.len() || b[body] != b'[' {
        return LongBracket::No;
    }

    let mut i = body + 1;

    while i < b.len() {
        if b[i] == b']' {
            let mut j = i + 1;
            let mut l = 0;

            while j < b.len() && b[j] == b'=' {
                l += 1;
                j += 1;
            }

            if l == level && j < b.len() && b[j] == b']' {
                return LongBracket::Ends(j + 1);
            }
        }

        i += 1;
    }

    /*
    An earlier version treated a run to the end of the file as one token that
    reached EOF. That version reported no error. It produced a token whose
    inner range was two bytes short of its own content. So the text came back
    truncated, and each format pass added one line to the result. An
    unterminated bracket is a syntax error, and the lexer now reports it as
    one.
    */
    LongBracket::Unterminated
}

fn scan_number(b: &[u8], mut i: usize) -> usize {
    while i < b.len() {
        match b[i] {
            b'0'..=b'9'
            | b'a'..=b'd'
            | b'f'
            | b'x'
            | b'o'
            | b'A'..=b'D'
            | b'F'
            | b'X'
            | b'O'
            | b'_' => i += 1,

            b'e' | b'E' | b'p' | b'P' => {
                i += 1;

                if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
                    i += 1;
                }
            }

            b'.' => {
                // Stop before `..`, the concatenation operator after a number.
                if i + 1 < b.len() && b[i + 1] == b'.' {
                    break;
                }

                i += 1;
            }

            _ => break,
        }
    }

    /*
    The integer suffix: `123i`, `0xABABi`, `0b1000_1000i`.

    The `i` joins the number only when it ends the word. So `3if` still
    lexes as the number and the keyword, exactly as it did before the
    suffix existed.
    */
    if i < b.len()
        && b[i] == b'i'
        && b.get(i + 1)
            .is_none_or(|c| !c.is_ascii_alphanumeric() && *c != b'_')
    {
        i += 1;
    }

    i
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(src: &str) -> Vec<TokKind> {
        lex(src).unwrap().toks.iter().map(|t| t.kind).collect()
    }

    #[test]
    fn basics() {
        let toks = lex(r#"local x = require("./foo")"#).unwrap().toks;
        let texts: Vec<_> = toks
            .iter()
            .map(|t| t.text(r#"local x = require("./foo")"#))
            .collect();
        assert_eq!(
            texts,
            vec!["local", "x", "=", "require", "(", "\"./foo\"", ")"]
        );
    }

    #[test]
    fn string_inner_span() {
        let src = r#"require("@pkg/signal")"#;
        let toks = lex(src).unwrap().toks;

        let TokKind::Str {
            inner_start,
            inner_end,
        } = toks[2].kind
        else {
            panic!("expected string, got {:?}", toks[2].kind)
        };

        assert_eq!(
            &src[inner_start as usize..inner_end as usize],
            "@pkg/signal"
        );
    }

    #[test]
    fn comments_are_skipped() {
        assert_eq!(kinds("-- require(\"x\")\ny"), vec![TokKind::Ident]);
        assert_eq!(kinds("--[[ require(\"x\") ]] y"), vec![TokKind::Ident]);
        assert_eq!(
            kinds("--[==[ ]] require(\"x\") ]==] y"),
            vec![TokKind::Ident]
        );
    }

    #[test]
    fn long_strings() {
        let src = "[[hello]]";
        let toks = lex(src).unwrap().toks;

        let TokKind::Str {
            inner_start,
            inner_end,
        } = toks[0].kind
        else {
            panic!()
        };

        assert_eq!(&src[inner_start as usize..inner_end as usize], "hello");
    }

    /// The suffix joins only when it ends the word; `3if` keeps its old split.
    #[test]
    fn integer_suffixes_join_their_number() {
        let toks = lex("local n = 123i + 0xf_fi + 0b10i").unwrap().toks;
        let numbers: Vec<TokKind> = toks
            .iter()
            .filter(|t| t.kind == TokKind::Number)
            .map(|t| t.kind)
            .collect();

        assert_eq!(numbers.len(), 3);

        let src = "local n = 123i";
        let toks = lex(src).unwrap().toks;

        assert_eq!(toks.last().unwrap().text(src), "123i");

        let src = "x = 3if";
        let toks = lex(src).unwrap().toks;

        assert_eq!(toks[2].text(src), "3");
        assert_eq!(toks[3].text(src), "if");
    }

    #[test]
    fn interp_strings_with_braces() {
        let src = r#"`a {("}")} b` z"#;
        let toks = lex(src).unwrap().toks;

        assert_eq!(toks[0].kind, TokKind::InterpStr);
        assert_eq!(toks[1].kind, TokKind::Ident);
    }

    /*
    A Windows checkout turns LF into CRLF, and Luau still reads the file.
    The `\` continuation is then `\`, CR, LF, one escape over three bytes,
    and `\z` swallows CR like any other whitespace. The same file must lex
    here, or a checkout on one platform refuses what another accepts.
    */
    #[test]
    fn string_continuations_survive_crlf_endings() {
        let quoted = "print(\"Hello \\\r\n\tWorld\")\r\n";
        assert!(lex(quoted).is_ok(), "backslash continuation over CRLF");

        let z = "print(\"testing \\z\r\n   twelve\")\r\n";
        assert!(lex(z).is_ok(), "\\z over CRLF");

        let interp = "print(`interp \\z\r\n   twelve`)\r\n";
        assert!(lex(interp).is_ok(), "interp \\z over CRLF");

        let interp_cont = "print(`a \\\r\n b`)\r\n";
        assert!(lex(interp_cont).is_ok(), "interp continuation over CRLF");
    }

    #[test]
    fn escaped_quotes() {
        let src = r#""a\"b" y"#;
        let toks = lex(src).unwrap().toks;

        assert!(matches!(toks[0].kind, TokKind::Str { .. }));
        assert_eq!(toks[1].kind, TokKind::Ident);
    }

    #[test]
    fn numbers_dont_eat_concat() {
        let src = "1 ..x";
        let toks = lex(src).unwrap().toks;

        assert_eq!(toks.len(), 3);
        let src2 = "0x1F_e2";
        assert_eq!(lex(src2).unwrap().toks.len(), 1);
    }
}
