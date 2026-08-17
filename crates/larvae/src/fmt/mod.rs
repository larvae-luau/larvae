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
pub mod rebind;
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

    let ignored = crate::flags::off_ranges(src, &lexed.comments, crate::flags::Subject::Fmt);

    /*
    A file held off in full comes back byte for byte.

    The general path re-emits an ignored statement from its own source, which
    keeps that statement exact. It does not promise the same for the space
    between two statements, and a reader who switches the formatter off for a
    whole file means the file. So the whole-file case returns early and
    promises the stronger thing.
    */
    if ignored
        .iter()
        .any(|&(a, b)| a == 0 && b >= src.len() as u32)
    {
        return Ok(src.to_string());
    }

    let trivia = trivia::Trivia::new(src, &lexed.comments);
    let rebindings = rebind::plan(src, &lexed.toks, &chunk, cfg.require_binding);
    let emitter = emit::Emitter::new(src, &lexed.toks, &trivia, cfg, rebindings).ignoring(ignored);
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
    doc_of_holding(src, cfg, true)
}

/*
The same, and `holds` says whether a marker inside the slice takes effect.

A worm that sends a whole document gets its regions held by the caller, over
the rendered text, because the layout of the markup around the Luau is the
worm's and not larvae's. That pass covers the Luau in the region as well. So
the caller turns this one off, and the region is written back one time rather
than two.
*/
pub(crate) fn doc_of_holding(src: &str, cfg: &FmtConfig, holds: bool) -> Result<doc::Doc<'static>> {
    let lexed = lexer::lex(src)
        .map_err(|e| anyhow::anyhow!("syntax error at byte {}, {}", e.offset, e.message))?;

    let chunk = parser::parse(src, &lexed.toks).map_err(|e| anyhow::anyhow!("{}", e.message))?;

    let trivia = trivia::Trivia::new(src, &lexed.comments);

    /*
    The slice is not the whole file, so `require_binding` cannot prove that a
    conversion compiles here. The empty plan changes no keyword.
    */
    let emitter = emit::Emitter::new(src, &lexed.toks, &trivia, cfg, rebind::Rebindings::new());

    /*
    A `fmt off` inside the slice holds here too.

    The flags are read from the comments of this slice, so they carry the
    offsets of the slice and need no rebase. A file that a worm claims reaches
    the formatter only through this function, so without this a marker in such
    a file would do nothing.
    */
    let ignored = match holds {
        true => crate::flags::off_ranges(src, &lexed.comments, crate::flags::Subject::Fmt),

        false => Vec::new(),
    };

    let emitter = emitter.ignoring(ignored);

    Ok(emitter.block_body(&chunk.block).into_owned())
}

/// Does the same for a slice that holds one expression, not statements.
pub(crate) fn doc_of_expr(src: &str, cfg: &FmtConfig) -> Result<doc::Doc<'static>> {
    let lexed = lexer::lex(src)
        .map_err(|e| anyhow::anyhow!("syntax error at byte {}, {}", e.offset, e.message))?;

    let expr =
        parser::parse_expr(src, &lexed.toks).map_err(|e| anyhow::anyhow!("{}", e.message))?;

    let trivia = trivia::Trivia::new(src, &lexed.comments);

    // An expression declares nothing, so the empty plan is exact here.
    let emitter = emit::Emitter::new(src, &lexed.toks, &trivia, cfg, rebind::Rebindings::new());

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
