/*!
larvae's own rules

These rules have no darklua equivalent. They use data that only larvae
knows: the resolved require forms and the datamodel path of each file.
The contract equals the parity rules. Walk the tree, push byte edits, and
keep newline counts when a rule deletes multiline spans.

Each rule here is conservative by construction. When the tree alone
cannot prove safety, the rule skips the instance without a report. A
wrong rewrite in a live game costs more than a missed transform.
*/

use std::path::Path;

use crate::config::RulesConfig;
use crate::diag::Diag;
use crate::rules::edits::Edits;
use crate::rules::engine::{self, Flow, RuleCtx, Visit};
use crate::syntax::ast::{Expr, IndexKey, TokSpan};

mod dedupe_requires;
mod freeze_module;
mod inject_module_path;
mod remove_calls;
mod use_get_service;

/// True when one or more rules in this module are enabled. This gates the parse.
pub fn wants(cfg: &RulesConfig) -> bool {
    cfg.remove_calls.is_some()
        || cfg.use_get_service
        || cfg.dedupe_requires
        || cfg.inject_module_path.is_some()
        || cfg.freeze_module
}

/// Run each enabled rule. Push edits and diagnostics.
pub fn apply(
    cfg: &RulesConfig,
    ctx: &RuleCtx,
    edits: &mut Edits,
    diags: &mut Vec<Diag>,
    path: &Path,
) {
    if let Some(calls) = &cfg.remove_calls {
        edits.run("remove_calls", |e| remove_calls::apply(calls, ctx, e));
    }

    if cfg.use_get_service {
        edits.run("use_get_service", |e| use_get_service::apply(ctx, e));
    }

    if cfg.dedupe_requires {
        edits.run("dedupe_requires", |e| dedupe_requires::apply(ctx, e));
    }

    if let Some(name) = &cfg.inject_module_path {
        edits.run("inject_module_path", |e| {
            inject_module_path::apply(name, ctx, e, diags, path)
        });
    }

    if cfg.freeze_module {
        edits.run("freeze_module", |e| freeze_module::apply(ctx, e));
    }
}

// --- shared helpers ----------------------------------------------------------

/// The text of a span that is exactly one token. Example: a binding name.
fn name_text<'src>(ctx: &RuleCtx<'src>, span: TokSpan) -> &'src str {
    ctx.tok_text(span.start)
}

/// The dotted name path of a callee or receiver. Example: `debug.profilebegin`.
fn dotted_path(ctx: &RuleCtx, expr: &Expr) -> Option<String> {
    match expr {
        Expr::Name(s) => Some(name_text(ctx, *s).to_string()),
        Expr::Index {
            object,
            key: IndexKey::Field(field),
            ..
        } => Some(format!(
            "{}.{}",
            dotted_path(ctx, object)?,
            name_text(ctx, *field)
        )),
        _ => None,
    }
}

/// True when the expression holds a call anywhere inside it.
fn contains_call(expr: &Expr) -> bool {
    struct Finder(bool);
    impl Visit for Finder {
        fn expr(&mut self, e: &Expr) -> Flow {
            if self.0 {
                // One call is enough. No node below can change the answer.
                return Flow::Skip;
            }

            if matches!(e, Expr::Call { .. }) {
                self.0 = true;

                return Flow::Skip;
            }

            Flow::Next
        }
    }

    let mut finder = Finder(false);
    engine::walk_expr(expr, &mut finder);

    finder.0
}

/// The newline count in a byte range. A replacement must keep the same count.
fn newlines_in(ctx: &RuleCtx, from: u32, to: u32) -> usize {
    ctx.src[from as usize..to as usize]
        .bytes()
        .filter(|&b| b == b'\n')
        .count()
}

/// The start of the line that holds `at`, when only blanks come before it.
fn blank_line_start(ctx: &RuleCtx, at: u32) -> u32 {
    let bytes = ctx.src.as_bytes();
    let mut i = at as usize;

    while i > 0 && matches!(bytes[i - 1], b' ' | b'\t') {
        i -= 1;
    }

    i as u32
}

/// True for a valid Luau identifier. The config checks names before they reach the output.
pub fn is_ident(name: &str) -> bool {
    const KEYWORDS: [&str; 21] = [
        "and", "break", "do", "else", "elseif", "end", "false", "for", "function", "if", "in",
        "local", "nil", "not", "or", "repeat", "return", "then", "true", "until", "while",
    ];
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };

    (first.is_ascii_alphabetic() || first == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !KEYWORDS.contains(&name)
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use crate::syntax::scan::RequireSite;
    use crate::syntax::{lexer, parser, scan};

    /// Run the larvae rules over a source and return the spliced output.
    pub fn run(rules: &str, src: &str) -> String {
        run_full(rules, src, None, &mut Vec::new())
    }

    /// The same run, with a datamodel path for this file and the produced diagnostics.
    pub fn run_full(rules: &str, src: &str, dm: Option<&str>, diags: &mut Vec<Diag>) -> String {
        let cfg: RulesConfig = toml::from_str(rules).expect("rule config");
        let lexed = lexer::lex(src).expect("lex");
        let chunk = parser::parse(src, &lexed.toks).expect("parse");

        // The unit tests replace the rewriter. Each site keeps its own spec.
        let forms: Vec<(RequireSite, String)> = scan::scan(src, &lexed.toks)
            .sites
            .iter()
            .map(|s| {
                (
                    *s,
                    src[s.inner_start as usize..s.inner_end as usize].to_string(),
                )
            })
            .collect();
        let ctx = RuleCtx {
            src,
            toks: &lexed.toks,
            chunk: &chunk,
            comments: &lexed.comments,
            require_forms: &forms,
            dm_path: dm,
            quote: '"',
            defines: &Default::default(),
            globals: &Default::default(),
        };

        let mut edits = Edits::new();
        apply(&cfg, &ctx, &mut edits, diags, Path::new("test.luau"));

        crate::rules::edits::splice(src, &edits, &mut Vec::new())
    }
}
