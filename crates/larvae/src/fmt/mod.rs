/*!
`larvae fmt`, the formatter.

The formatter has four parts. A file moves through them in this order.
[`trivia`] finds the comments that the parser does not keep. [`emit`] turns
the tree into a layout document. [`doc`] decides which groups of that document
break. [`config`] sets the width and the indentation characters.

The important property is idempotence: when larvae formats formatted output,
the output must not change. A test checks this property. The reason is that a
formatter that oscillates between two outputs turns every save into a diff.
*/

pub mod config;
pub mod doc;
pub mod emit;
pub mod trivia;

use anyhow::{Context, Result};

pub use config::FmtConfig;

use crate::syntax::{lexer, parser};

/// Formats the source of one file.
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
Returns the layout document for a source slice. The document does not have the
file-final newline that `format` adds.

These functions exist for worm formatting. A worm's document marks spans of
ordinary Luau as `host`, and larvae lays them out. Because of this, a claimed
file breaks lines the same way a Luau file does. The function converts the
document to `'static` because the tokens and trivia it borrows live on this
stack frame. This conversion copies data, so `format` does not call these
functions.
*/
pub(crate) fn doc_of(src: &str, cfg: &FmtConfig) -> Result<doc::Doc<'static>> {
    let lexed = lexer::lex(src)
        .map_err(|e| anyhow::anyhow!("syntax error at byte {}, {}", e.offset, e.message))?;

    let chunk = parser::parse(src, &lexed.toks).map_err(|e| anyhow::anyhow!("{}", e.message))?;

    let trivia = trivia::Trivia::new(src, &lexed.comments);
    let emitter = emit::Emitter::new(src, &lexed.toks, &trivia, cfg);

    Ok(emitter.block_body(&chunk.block).into_owned())
}

/// Does the same for a slice that holds one expression, not statements.
pub(crate) fn doc_of_expr(src: &str, cfg: &FmtConfig) -> Result<doc::Doc<'static>> {
    let lexed = lexer::lex(src)
        .map_err(|e| anyhow::anyhow!("syntax error at byte {}, {}", e.offset, e.message))?;

    let expr =
        parser::parse_expr(src, &lexed.toks).map_err(|e| anyhow::anyhow!("{}", e.message))?;

    let trivia = trivia::Trivia::new(src, &lexed.comments);
    let emitter = emit::Emitter::new(src, &lexed.toks, &trivia, cfg);

    Ok(emitter.expr(&expr).into_owned())
}

/*
Rejects output that lost a comment.

The emitter places comments at the positions it knows about: between statements
and after the opening keyword of a block. A comment at another position, for
example inside a table or an argument list, falls in a gap that no code reads.
That comment disappears from the output.

To delete a comment of the user is worse than to refuse to format the file, and
`larvae fmt` writes to disk. So larvae also checks at runtime the property that
the tests assert. A file that would lose a comment comes back as an error that
names the comment. Larvae keeps the file exactly as it was, and the run reports
the error.

This check is a safety measure, not the fix. Each position it catches is a
position that the emitter must learn to place. The check costs one pass over
the comment list.
*/
pub(crate) fn check_comments_survived(src: &str, comments: &[(u32, u32)], out: &str) -> Result<()> {
    for &(start, end) in comments {
        let text = src[start as usize..end as usize].trim_end();

        if !out.contains(text) {
            anyhow::bail!("formatting would drop the comment {text:?}, so the file was left alone");
        }
    }

    Ok(())
}
