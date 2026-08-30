/*!
Obfuscation: hex names, hex strings, and one line.

Roblox removed `loadstring`, so a Luau file cannot ship as a blob that a
loader decodes at run time. The bytes on disk have to be Luau that the
compiler reads. That rules out the shape most people picture when they say
obfuscation, and it leaves the part of a file a reader actually uses to
understand it: the names and the strings. Both go here, the whitespace
goes with them, and the program that remains runs exactly as it did.

Three passes, in this order.

`remove_types` deletes every annotation. A type names the module it came
from and the shape of every value, which is the best documentation in the
file.

`rename_variables` gives each local a name from `_0x0` upward. Only a
local can move. A global names something the file does not own, so the
name is part of the interface and stays.

Then every string literal becomes its own bytes in `\xNN` form. This is
the pass that has to be exact, because a string is data and a wrong byte
is a wrong program. Each literal is decoded to bytes and written back
escaped, and a literal the decoder cannot account for is left as it was.
An unobfuscated string is a small loss; a corrupted one is a broken build.

A backtick string stays whole. The lexer keeps it as one token, so the
static text and the `{}` holes are not separated here, and separating them
means implementing the interpolation grammar a second time. The failure
mode is bad: an escaped `{` turns a hole into text, or a hole into a
syntax error, and the damage shows up at run time in a message nobody
reads. The names inside a hole still get renamed, because the rename walks
tokens and the hole is inside one.

The pass runs on the finished text of a file, after the require rewriter
and every rule, and the dense emitter prints the result. `larvae process`
and `larvae bundle` both print through the generator, so both get the same
treatment from one place. It is not a rule for the same reason: a rule
edits one source file, and the bundle never runs the rules of the project
over its modules.
*/

use crate::rules::darklua::{locals, types};
use crate::rules::edits::{Edits, splice};
use crate::rules::engine::RuleCtx;
use crate::syntax::lexer::{self, TokKind};
use crate::syntax::{dense, parser};

/// The obfuscated text of one Luau source, broken near `column_span` columns
pub fn obfuscate(src: &str, column_span: usize) -> Result<String, String> {
    let stripped = strip_and_rename(src)?;
    let hexed = hex_strings(&stripped)?;

    dense::dense(&hexed, column_span)
}

/// Drop every type, then give every local a `_0x` name.
fn strip_and_rename(src: &str) -> Result<String, String> {
    let lexed = lexer::lex(src).map_err(|e| e.message)?;
    let chunk = parser::parse(src, &lexed.toks).map_err(|e| e.message)?;

    let ctx = RuleCtx {
        src,
        toks: &lexed.toks,
        chunk: &chunk,
        comments: &lexed.comments,
        require_forms: &[],
        dm_path: None,
        quote: '"',
        defines: &Default::default(),
        globals: &Default::default(),
    };

    let mut edits = Edits::new();
    edits.run("remove_types", |e| types::remove_types(&ctx, e));
    edits.run("rename_variables", |e| {
        locals::rename_with(&ctx, e, locals::NameStyle::Hex)
    });

    /*
    A rename inside a type lands within the span that `remove_types` just
    deleted, so the splice drops it without a word. That is the same order
    the rule dispatcher uses, and the reason the two rules have always
    worked together.
    */
    Ok(splice(src, &edits, &mut Vec::new()))
}

/// Rewrite each string literal as the `\xNN` form of its own bytes.
fn hex_strings(src: &str) -> Result<String, String> {
    let lexed = lexer::lex(src).map_err(|e| e.message)?;

    /*
    A require's spec stays as written. The engine decodes either
    spelling, so escaping the path hides nothing, and it breaks every
    reader that checks the spec textually: larvae's own checker, the
    RFC validation of another tool, a person tracing a module.
    */
    let scanned = crate::syntax::scan::scan(src, &lexed.toks);
    let specs: std::collections::HashSet<u32> =
        scanned.sites.iter().map(|site| site.tok_start).collect();

    let mut out = String::with_capacity(src.len());
    let mut cursor = 0usize;

    for tok in &lexed.toks {
        if specs.contains(&tok.start) {
            continue;
        }

        let TokKind::Str {
            inner_start,
            inner_end,
        } = tok.kind
        else {
            continue;
        };

        let inner = &src[inner_start as usize..inner_end as usize];
        let long = src.as_bytes()[tok.start as usize] == b'[';

        let Some(bytes) = (match long {
            true => long_bytes(inner),
            false => quoted_bytes(inner),
        }) else {
            continue;
        };

        out.push_str(&src[cursor..tok.start as usize]);
        out.push('"');

        for b in bytes {
            out.push_str(&format!("\\x{b:02x}"));
        }

        out.push('"');
        cursor = tok.end as usize;
    }

    out.push_str(&src[cursor..]);

    Ok(out)
}

/*
The bytes of a long string, `[[...]]`.

Luau reads the content raw, with two exceptions that the tests measure
against Luau itself. A `\r\n` pair becomes one `\n`, and a `\n` at the
start is dropped. A lone `\r` is neither, so a content that holds one is
left alone rather than guessed at.
*/
fn long_bytes(inner: &str) -> Option<Vec<u8>> {
    let text = match inner.contains("\r\n") {
        true => {
            /*
            A lone `\r` beside a pair cannot be told apart by a replace, so
            the check runs on what is left after the pairs are gone.
            */
            let joined = inner.replace("\r\n", "\n");

            if joined.contains('\r') {
                return None;
            }

            joined
        }

        false if inner.contains('\r') => return None,

        false => inner.to_string(),
    };

    Some(text.strip_prefix('\n').unwrap_or(&text).as_bytes().to_vec())
}

/*
The bytes of a quoted string, with each escape resolved.

None means the decoder met something it cannot account for. The caller
then leaves the literal exactly as the author wrote it, which is the only
answer that cannot be wrong.
*/
fn quoted_bytes(inner: &str) -> Option<Vec<u8>> {
    let b = inner.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0usize;

    while i < b.len() {
        if b[i] != b'\\' {
            out.push(b[i]);
            i += 1;

            continue;
        }

        let c = *b.get(i + 1)?;
        i += 2;

        match c {
            b'a' => out.push(7),
            b'b' => out.push(8),
            b'f' => out.push(12),
            b'n' => out.push(b'\n'),
            b'r' => out.push(b'\r'),
            b't' => out.push(b'\t'),
            b'v' => out.push(11),

            // A break inside a literal stands for one newline, `\r\n` included.
            b'\n' => out.push(b'\n'),

            b'\r' => {
                if b.get(i) != Some(&b'\n') {
                    return None;
                }

                out.push(b'\n');
                i += 1;
            }

            // `\z` eats the whitespace behind it, so a literal can wrap.
            b'z' => {
                while i < b.len() && b[i].is_ascii_whitespace() {
                    i += 1;
                }
            }

            b'x' => {
                let hex = inner.get(i..i + 2)?;
                out.push(u8::from_str_radix(hex, 16).ok()?);
                i += 2;
            }

            b'u' => {
                if b.get(i) != Some(&b'{') {
                    return None;
                }

                let close = i + 1 + b[i + 1..].iter().position(|&c| c == b'}')?;
                let code = u32::from_str_radix(inner.get(i + 1..close)?, 16).ok()?;
                push_utf8(code, &mut out)?;
                i = close + 1;
            }

            // Up to three decimal digits, and Luau refuses a value over 255.
            b'0'..=b'9' => {
                let mut value = u32::from(c - b'0');
                let mut digits = 1;

                while digits < 3 && b.get(i).is_some_and(|d| d.is_ascii_digit()) {
                    value = value * 10 + u32::from(b[i] - b'0');
                    digits += 1;
                    i += 1;
                }

                out.push(u8::try_from(value).ok()?);
            }

            /*
            Luau drops the backslash of an escape it does not know, so
            `"\q"` is one `q`. That holds for an ascii character only: a
            byte over 127 is part of a character the decoder would cut in
            half, so the literal stays as it is.
            */
            other if other.is_ascii() => out.push(other),

            _ => return None,
        }
    }

    Some(out)
}

/*
The utf8 bytes of one code point, the way Luau writes them.

Luau applies the encoding to a surrogate as well, so `\u{D800}` is three
bytes and not an error. `char::encode_utf8` refuses that, which is why the
arithmetic is here. The cap is the one Luau enforces.
*/
fn push_utf8(code: u32, out: &mut Vec<u8>) -> Option<()> {
    match code {
        0x0..=0x7f => out.push(code as u8),

        0x80..=0x7ff => out.extend([0xc0 | (code >> 6) as u8, 0x80 | (code & 0x3f) as u8]),

        0x800..=0xffff => out.extend([
            0xe0 | (code >> 12) as u8,
            0x80 | ((code >> 6) & 0x3f) as u8,
            0x80 | (code & 0x3f) as u8,
        ]),

        0x1_0000..=0x10_ffff => out.extend([
            0xf0 | (code >> 18) as u8,
            0x80 | ((code >> 12) & 0x3f) as u8,
            0x80 | ((code >> 6) & 0x3f) as u8,
            0x80 | (code & 0x3f) as u8,
        ]),

        _ => return None,
    }

    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(src: &str) -> String {
        obfuscate(src, usize::MAX).expect("obfuscates")
    }

    /// The output has to be Luau, whatever else it is.
    fn parses(src: &str) {
        let lexed = lexer::lex(src).expect("lexes");
        parser::parse(src, &lexed.toks).expect("parses");
    }

    #[test]
    fn a_string_becomes_the_hex_of_its_bytes() {
        assert_eq!(run("return \"hi\"\n"), "return\"\\x68\\x69\"\n");
    }

    #[test]
    fn every_local_takes_a_hex_name() {
        let out = run("local counter = 1\nlocal other = 2\nreturn counter + other\n");

        assert_eq!(out, "local _0x0=1 local _0x1=2 return _0x0+_0x1\n");
    }

    #[test]
    fn a_global_keeps_its_name() {
        let out = run("local x = print\nreturn print(x)\n");

        assert!(out.contains("print"), "{out}");
    }

    #[test]
    fn types_are_gone_and_the_prefix_follows_the_rename() {
        let out = run("local t = require(\"@pkg/t\")\nlocal e: t.Entity = t.new()\nreturn e\n");

        assert!(!out.contains("Entity"), "{out}");
        assert!(out.contains("_0x1=_0x0.new()"), "{out}");
        parses(&out);
    }

    /// The spec is a path a later reader has to read, so it stays plain.
    #[test]
    fn a_require_spec_stays_readable() {
        let out = run("local t = require(\"./mod\")\nlocal s = \"secret\"\nreturn t\n");

        assert!(out.contains("\"./mod\""), "{out}");
        assert!(!out.contains("secret"), "{out}");
    }

    #[test]
    fn comments_do_not_survive() {
        let out = run("-- a secret\nlocal x = 1 --[[ another ]]\nreturn x\n");

        assert_eq!(out, "local _0x0=1 return _0x0\n");
    }

    #[test]
    fn a_long_string_becomes_a_quoted_one() {
        assert_eq!(run("return [[hi]]\n"), "return\"\\x68\\x69\"\n");
    }

    /// Luau drops the first newline of a long string, so the bytes do too.
    #[test]
    fn a_long_string_loses_its_first_newline_and_no_other() {
        assert_eq!(run("return [[\nhi]]\n"), "return\"\\x68\\x69\"\n");
        assert_eq!(run("return [[\n\nh]]\n"), "return\"\\x0a\\x68\"\n");
    }

    /// A hole is text to the lexer, so the binding it names keeps its name.
    #[test]
    fn an_interpolated_string_and_the_names_it_holds_are_left_alone() {
        let out = run("local n = 1\nlocal other = 2\nreturn `n is {n}`, other\n");

        assert_eq!(out, "local n=1 local _0x0=2 return`n is {n}`,_0x0\n");
    }

    #[test]
    fn the_escapes_luau_takes_all_decode() {
        // Each pair is a literal and the bytes Luau gives it.
        for (literal, bytes) in [
            (r#""\n""#, vec![10u8]),
            (r#""\t\\\"""#, vec![9, 92, 34]),
            (r#""\65\066""#, vec![65, 66]),
            (r#""\x41""#, vec![65]),
            (r#""\u{48}""#, vec![72]),
            (r#""\u{1F600}""#, vec![240, 159, 152, 128]),
            (r#""\u{D800}""#, vec![237, 160, 128]),
            (r#""\q""#, vec![113]),
            ("\"a\\z\n   b\"", vec![97, 98]),
            ("\"a\\\nb\"", vec![97, 10, 98]),
        ] {
            let inner = &literal[1..literal.len() - 1];

            assert_eq!(quoted_bytes(inner), Some(bytes), "{literal}");
        }
    }

    /// A literal the decoder cannot account for stays exactly as it was.
    #[test]
    fn an_unreadable_literal_is_left_alone() {
        assert_eq!(quoted_bytes("\\"), None);
        assert_eq!(quoted_bytes("\\x4"), None);
        assert_eq!(quoted_bytes("\\u{110000}"), None);
        assert_eq!(quoted_bytes("\\400"), None);
        // A lone carriage return in a long string has no measured meaning.
        assert_eq!(long_bytes("a\rb"), None);
    }

    #[test]
    fn a_file_with_no_string_and_no_local_is_only_squeezed() {
        assert_eq!(run("print(1 + 2)\n"), "print(1+2)\n");
    }

    #[test]
    fn the_output_is_one_line() {
        let src = "local function add(a, b)\n\treturn a + b\nend\n\nreturn add(1, 2)\n";
        let out = run(src);

        assert_eq!(out.lines().count(), 1, "{out}");
        parses(&out);
    }
}
