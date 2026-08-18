/*!
The printer writes the tree back out. It replays the tokens with the source
between them. So a tree without changes reproduces the input byte for byte.
That is the retain-lines guarantee. It holds by construction, not through bug
patches.
*/

use crate::syntax::ast::*;
use crate::syntax::lexer::Tok;

/// Prints a token range, with the trivia that sits between the tokens included.
pub fn print_range(src: &str, toks: &[Tok], from: u32, to: u32, out: &mut String) {
    for i in from..to {
        let t = &toks[i as usize];

        if i > from {
            let prev_end = toks[i as usize - 1].end as usize;
            out.push_str(&src[prev_end..t.start as usize]);
        }

        out.push_str(t.text(src));
    }
}

pub fn print_span(src: &str, toks: &[Tok], span: TokSpan) -> String {
    let mut out = String::new();
    print_range(src, toks, span.start, span.end, &mut out);

    out
}

/// Prints a whole parsed file, with the leading and trailing trivia included.
pub fn print_chunk(src: &str, toks: &[Tok], chunk: &Chunk) -> String {
    let mut out = String::new();

    if toks.is_empty() {
        out.push_str(src);

        return out;
    }

    out.push_str(&src[..toks[0].start as usize]);
    print_range(
        src,
        toks,
        chunk.block.span.start,
        chunk.block.span.end,
        &mut out,
    );
    out.push_str(&src[toks[toks.len() - 1].end as usize..]);

    out
}

/*
This is a coverage check. The statements of each block must tile the token
span of that block with no holes. Without this check, the round trip could
pass while the tree silently dropped tokens, because the gaps would print the
missing text anyway.
*/
pub fn coverage_errors(chunk: &Chunk) -> Vec<String> {
    let mut errs = Vec::new();
    check_block(&chunk.block, &mut errs);

    errs
}

fn check_block(block: &Block, errs: &mut Vec<String>) {
    let mut cursor = block.span.start;

    for stmt in &block.stmts {
        let span = stmt.span();

        if span.start != cursor {
            errs.push(format!(
                "hole in block, expected a statement at token {cursor} but found one at {}",
                span.start
            ));
        }

        cursor = span.end;

        for inner in nested_blocks(stmt) {
            check_block(inner, errs);
        }
    }

    if cursor != block.span.end {
        errs.push(format!(
            "block ends at token {} but its statements stop at {cursor}",
            block.span.end
        ));
    }
}

fn nested_blocks(stmt: &Stmt) -> Vec<&Block> {
    match stmt {
        Stmt::Do(n) => vec![&n.block],

        Stmt::While(n) => vec![&n.block],

        Stmt::Repeat(n) => vec![&n.block],

        Stmt::NumericFor(n) => vec![&n.block],

        Stmt::GenericFor(n) => vec![&n.block],

        Stmt::Function(n) => vec![&n.body.block],

        Stmt::LocalFunction(n) => vec![&n.body.block],

        Stmt::Class(n) => n
            .members
            .iter()
            .filter_map(|m| match m {
                crate::syntax::ast::ClassMember::Method(f) => Some(&f.body.block),

                crate::syntax::ast::ClassMember::Field { .. } => None,
            })
            .collect(),

        Stmt::If(n) => {
            let mut v: Vec<&Block> = n.branches.iter().map(|(_, b)| b).collect();

            if let Some(e) = &n.else_block {
                v.push(e);
            }

            v
        }

        _ => Vec::new(),
    }
}
