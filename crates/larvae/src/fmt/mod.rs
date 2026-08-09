/*!
`larvae fmt`, the formatter.

Four pieces, in the order a file moves through them. [`trivia`] finds the
comments the parser does not keep, [`emit`] turns the tree into a layout
document, [`doc`] decides which of that document's groups break, and [`config`]
says how wide and with what.

The property that matters is idempotence: formatting formatted output must
change nothing. It is checked as a test rather than assumed, because a formatter
that oscillates between two outputs turns every save into a diff.
*/

pub mod config;
pub mod doc;
pub mod emit;
pub mod trivia;

use anyhow::{Context, Result};

pub use config::FmtConfig;

use crate::syntax::{lexer, parser};

/// Format one file's source
pub fn format(src: &str, cfg: &FmtConfig) -> Result<String> {
    let lexed = lexer::lex(src)
        .map_err(|e| anyhow::anyhow!("syntax error at byte {}, {}", e.offset, e.message))?;

    let chunk = parser::parse(src, &lexed.toks)
        .map_err(|e| anyhow::anyhow!("{}", e.message))
        .context("cannot format a file that does not parse")?;

    let trivia = trivia::Trivia::new(src, &lexed.comments);
    let emitter = emit::Emitter::new(src, &lexed.toks, &trivia, cfg);
    let document = emitter.chunk(&chunk);
    let out = doc::render(&document, cfg.style());

    check_comments_survived(src, &lexed.comments, &out)?;

    Ok(out)
}

/*
Refuse to return output that lost a comment.

The emitter places comments at the positions it knows about, between statements
and after a block's opening keyword. A comment somewhere else, inside a table or
an argument list, falls in a gap nothing reads and simply disappears.

Deleting somebody's comment is worse than declining to format their file, and
`larvae fmt` writes to disk. So the property the tests assert is also checked at
runtime, and a file that would lose one comes back as an error naming it: the
file is left exactly as it was and the run reports it.

This is a backstop, not the fix. Every position it catches is a position the
emitter should learn to place, and it costs one pass over the comment list.
*/
fn check_comments_survived(src: &str, comments: &[(u32, u32)], out: &str) -> Result<()> {
    for &(start, end) in comments {
        let text = src[start as usize..end as usize].trim_end();

        if !out.contains(text) {
            anyhow::bail!("formatting would drop the comment {text:?}, so the file was left alone");
        }
    }

    Ok(())
}
