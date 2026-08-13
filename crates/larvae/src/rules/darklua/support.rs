/*!
Shared helpers for the parity rules

This module holds the small predicates that the rules use, and the edit
shapes that appear in more than one place. Each helper here is
conservative by design. A predicate answers yes only when the answer is
provable from the tree alone.
*/

use crate::rules::engine::{Edit, Flow, RuleCtx, Visit, walk_expr};
use crate::syntax::ast::*;
use crate::syntax::lexer::TokKind;

/// Luau's reserved words. A field access can never use one of these.
pub fn is_reserved(word: &str) -> bool {
    matches!(
        word,
        "and"
            | "break"
            | "do"
            | "else"
            | "elseif"
            | "end"
            | "false"
            | "for"
            | "function"
            | "if"
            | "in"
            | "local"
            | "nil"
            | "not"
            | "or"
            | "repeat"
            | "return"
            | "then"
            | "true"
            | "until"
            | "while"
    )
}

/// True for a name that the code can write after a dot.
pub fn is_ident(s: &str) -> bool {
    if s.is_empty() || is_reserved(s) {
        return false;
    }

    let mut chars = s.chars();
    let first = chars.next().unwrap();

    if !(first.is_ascii_alphabetic() || first == '_') {
        return false;
    }

    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/*
A side effect probe. A call is the only node in the tree that clearly
runs user code. An index and arithmetic can also run metamethods, but
darklua treats those as pure. Larvae matches darklua here, and that keeps
ported configs predictable.
*/
pub fn has_call(e: &Expr) -> bool {
    struct Probe {
        found: bool,
    }

    impl Visit for Probe {
        fn expr(&mut self, e: &Expr) -> Flow {
            if matches!(e, Expr::Call { .. }) {
                self.found = true;
            }

            Flow::Next
        }
    }

    let mut p = Probe { found: false };
    walk_expr(e, &mut p);

    p.found
}

/// True when the expression needs no parentheses as an operand.
pub fn is_atomic(e: &Expr) -> bool {
    matches!(
        e,
        Expr::Nil(_)
            | Expr::True(_)
            | Expr::False(_)
            | Expr::Vararg(_)
            | Expr::Number(_)
            | Expr::String(_)
            | Expr::InterpString(_)
            | Expr::Name(_)
            | Expr::Table { .. }
            | Expr::Paren { .. }
            | Expr::Index { .. }
            | Expr::Call { .. }
            | Expr::Function { .. }
    )
}

/*
True when the expression is safe to write twice. Compound assignment
emits its target again, so the target must not run code. A name or a path
of names qualifies. A computed key also qualifies when the key itself
calls nothing.
*/
pub fn is_reemittable(e: &Expr) -> bool {
    match e {
        Expr::Name(_) => true,

        Expr::Paren { inner, .. } => is_reemittable(inner),

        Expr::Index { object, key, .. } => {
            is_reemittable(object)
                && match key {
                    IndexKey::Field(_) => true,

                    IndexKey::Computed(k) => !has_call(k),
                }
        }

        _ => false,
    }
}

/// Values that Lua never treats as false. remove_if_expression needs this guard.
pub fn is_never_falsy(e: &Expr) -> bool {
    match e {
        Expr::Number(_)
        | Expr::String(_)
        | Expr::InterpString(_)
        | Expr::Table { .. }
        | Expr::True(_)
        | Expr::Function { .. } => true,

        Expr::Paren { inner, .. } => is_never_falsy(inner),

        _ => false,
    }
}

// --- edit shapes ---------------------------------------------------------

/// A zero width insert at a byte offset.
pub fn insert(at: u32, text: &str, edits: &mut Vec<Edit>) {
    edits.push((at, at, text.to_string()));
}

/// The byte range of one token.
pub fn tok_bytes(ctx: &RuleCtx, index: u32) -> (u32, u32) {
    let t = &ctx.toks[index as usize];

    (t.start, t.end)
}

/*
Replace a byte range and pad the newline shortfall. The generated text
carries the lines that it needs. The function appends the rest, so
retain-lines output never drifts. The function refuses when the text
would add lines.
*/
pub fn replace_keep_lines(
    ctx: &RuleCtx,
    from: u32,
    to: u32,
    text: &str,
    edits: &mut Vec<Edit>,
) -> bool {
    let had = count_newlines(&ctx.src[from as usize..to as usize]);
    let now = count_newlines(text);

    if now > had {
        return false;
    }

    let mut out = String::with_capacity(text.len() + (had - now));
    out.push_str(text);

    for _ in 0..had - now {
        out.push('\n');
    }

    edits.push((from, to, out));

    true
}

pub fn count_newlines(s: &str) -> usize {
    s.bytes().filter(|&b| b == b'\n').count()
}

/// True when a comment starts inside this byte range.
pub fn has_comment_in(ctx: &RuleCtx, from: u32, to: u32) -> bool {
    ctx.comments.iter().any(|&(s, _)| s >= from && s < to)
}

/// The source text of an expression. The text gets parens when it is not atomic.
pub fn operand_text(ctx: &RuleCtx, e: &Expr) -> String {
    let text = ctx.text(e.span());

    if is_atomic(e) {
        text.to_string()
    } else {
        format!("({text})")
    }
}

/*
The inner content of a string token when it is a plain literal. The
function returns None for each form that the rules must not examine.
Escapes are included in that set. Thus a caller can trust the bytes that
it receives.
*/
pub fn plain_string_value<'a>(ctx: &RuleCtx<'a>, span: TokSpan) -> Option<&'a str> {
    let tok = ctx.toks.get(span.start as usize)?;
    let TokKind::Str {
        inner_start,
        inner_end,
    } = tok.kind
    else {
        return None;
    };

    let inner = &ctx.src[inner_start as usize..inner_end as usize];

    if inner.contains('\\') {
        None
    } else {
        Some(inner)
    }
}

/// The token index of the `(` that opens the parameter list of a function body.
pub fn params_lparen(ctx: &RuleCtx, body: &FunctionBody) -> Option<u32> {
    let from = match body.generics {
        Some(g) => g.end,

        None => body.span.start,
    };

    (ctx.toks.get(from as usize)?.kind == TokKind::LParen).then_some(from)
}

/// True when a statement introduces a binding. An unwrap of a block with
/// one of these would leak the binding into the enclosing scope.
pub fn declares_local(b: &Block) -> bool {
    b.stmts.iter().any(|s| {
        matches!(
            s,
            Stmt::Local(_) | Stmt::LocalFunction(_) | Stmt::TypeAlias(_)
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ident_rules_match_luau() {
        assert!(is_ident("field"));
        assert!(is_ident("_x9"));
        assert!(is_ident("type"));
        assert!(!is_ident("end"));
        assert!(!is_ident("9a"));
        assert!(!is_ident(""));
        assert!(!is_ident("a-b"));
        assert!(!is_ident("a b"));
    }
}
