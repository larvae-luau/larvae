//! Finds `require("...")` call sites in a token stream.

use crate::syntax::lexer::{Tok, TokKind};

#[derive(Debug, Clone, Copy)]
pub struct RequireSite {
    /// The byte range of the content of the string literal, between the quotes or brackets.
    pub inner_start: u32,
    pub inner_end: u32,
    /// The whole string token with quotes. Instance rewrites replace this span.
    pub tok_start: u32,
    pub tok_end: u32,
    /// True for require("x"). A call without parentheses needs the expression wrapped in them.
    pub has_parens: bool,
    /// The byte offset of the `require` identifier, for diagnostics.
    pub at: u32,
    /// The index of the require identifier in the token stream, for context checks.
    pub require_idx: usize,
}

/// One hop along an instance expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// `.Parent`.
    Up,
    /// `.Name`, `["Name"]`, or a FindFirstChild style call.
    Down(String),
}

/// The place where an instance expression starts to count from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Root {
    /// `script`, relative to the own instance of the file that requires.
    Script,
    /// `game`, absolute from the DataModel root.
    Game,
}

/*
An instance require, for example require(script.Parent.Foo).

Legacy Roblox code has many of these, and Wally link modules generate them.
The ability to read them lets an existing codebase move over. The rewrite
replaces the whole expression, not only a string. So the spans cover every
token.
*/
#[derive(Debug, Clone)]
pub struct InstanceSite {
    pub root: Root,
    pub steps: Vec<Step>,
    pub start: u32,
    pub end: u32,
    /// The byte offset of the `require` identifier, for diagnostics.
    pub at: u32,
}

impl InstanceSite {
    /// Returns the expression as it reads in the source, for diagnostics.
    pub fn render(&self) -> String {
        let mut out = match self.root {
            Root::Script => String::from("script"),

            Root::Game => String::from("game"),
        };

        for step in &self.steps {
            match step {
                Step::Up => out.push_str(".Parent"),

                Step::Down(name) => {
                    out.push('.');
                    out.push_str(name);
                }
            }
        }

        out
    }
}

#[derive(Debug, Default)]
pub struct ScanResult {
    pub sites: Vec<RequireSite>,
    /// Requires with an instance expression. Larvae resolves them through the project map.
    pub instances: Vec<InstanceSite>,
    /// The offsets of dynamic requires. Larvae does not change them, and check counts them.
    pub dynamic: Vec<u32>,
}

pub fn scan(src: &str, toks: &[Tok]) -> ScanResult {
    let mut out = ScanResult::default();

    for (i, tok) in toks.iter().enumerate() {
        if tok.kind != TokKind::Ident || tok.text(src) != "require" {
            continue;
        }

        // `foo.require(...)` and `foo:require(...)` are not the global require.
        if i > 0 && matches!(toks[i - 1].kind, TokKind::Dot | TokKind::Colon) {
            continue;
        }

        match toks.get(i + 1).map(|t| t.kind) {
            // require("path")
            Some(TokKind::LParen) => match (toks.get(i + 2), toks.get(i + 3)) {
                (
                    Some(&Tok {
                        kind:
                            TokKind::Str {
                                inner_start,
                                inner_end,
                            },

                        start,
                        end,
                    }),
                    Some(&Tok {
                        kind: TokKind::RParen,
                        ..
                    }),
                ) => out.sites.push(RequireSite {
                    inner_start,
                    inner_end,
                    tok_start: start,
                    tok_end: end,
                    has_parens: true,
                    at: tok.start,
                    require_idx: i,
                }),

                // require(<expr>) is an instance chain when the scanner can read it.
                _ => match instance_expr(src, toks, i + 2) {
                    Some((root, steps, end)) => out.instances.push(InstanceSite {
                        root,
                        steps,
                        start: toks[i + 2].start,
                        end: toks[end - 1].end,
                        at: tok.start,
                    }),

                    None => out.dynamic.push(tok.start),
                },
            },

            // require "path" is call sugar without parentheses.
            Some(TokKind::Str {
                inner_start,
                inner_end,
            }) => {
                let s = toks[i + 1];
                out.sites.push(RequireSite {
                    inner_start,
                    inner_end,
                    tok_start: s.start,
                    tok_end: s.end,
                    has_parens: false,
                    at: tok.start,
                    require_idx: i,
                });
            }

            Some(TokKind::InterpStr) => out.dynamic.push(tok.start),
            // A bare require reference. There is nothing to rewrite, but the count matters.
            _ => out.dynamic.push(tok.start),
        }
    }

    out
}

/*
Reads an instance chain from the token stream, for example script.Parent.Foo.

The function returns the root, the hops, and the token index just past the
expression. The caller checks that this index is the closing parenthesis. An
unrecognized shape returns None and stays a dynamic require. A chain that
larvae cannot read in full is a chain that larvae must not rewrite.
*/
fn instance_expr(src: &str, toks: &[Tok], start: usize) -> Option<(Root, Vec<Step>, usize)> {
    let first = toks.get(start)?;

    if first.kind != TokKind::Ident {
        return None;
    }

    let root = match first.text(src) {
        "script" => Root::Script,

        "game" => Root::Game,

        _ => return None,
    };

    let mut steps = Vec::new();
    let mut j = start + 1;

    while let Some(tok) = toks.get(j) {
        match tok.kind {
            TokKind::Dot => {
                let name = toks.get(j + 1)?;

                if name.kind != TokKind::Ident {
                    return None;
                }

                let text = name.text(src);
                steps.push(if text == "Parent" {
                    Step::Up
                } else {
                    Step::Down(text.to_string())
                });
                j += 2;
            }

            // :GetService("X"), :FindFirstChild("X"), :WaitForChild("X")
            TokKind::Colon => {
                let method = toks.get(j + 1)?;

                if method.kind != TokKind::Ident
                    || !matches!(
                        method.text(src),
                        "GetService" | "FindFirstChild" | "WaitForChild"
                    )
                    || toks.get(j + 2)?.kind != TokKind::LParen
                    || toks.get(j + 4)?.kind != TokKind::RParen
                {
                    return None;
                }

                steps.push(Step::Down(literal_name(src, toks.get(j + 3)?)?));
                j += 5;
            }

            // ["X"] is the form that Wally link modules use to index their _Index folder.
            TokKind::Symbol if tok.text(src) == "[" => {
                let closing = toks.get(j + 2)?;

                if closing.kind != TokKind::Symbol || closing.text(src) != "]" {
                    return None;
                }

                steps.push(Step::Down(literal_name(src, toks.get(j + 1)?)?));
                j += 3;
            }

            _ => break,
        }
    }

    // The caller only accepts a chain that ends exactly at the closing parenthesis.
    if steps.is_empty() || toks.get(j)?.kind != TokKind::RParen {
        return None;
    }

    Some((root, steps, j))
}

/// Returns the contents of a plain string literal. A literal with escapes
/// stays dynamic, because the scanner does not guess its value.
fn literal_name(src: &str, tok: &Tok) -> Option<String> {
    let TokKind::Str {
        inner_start,
        inner_end,
    } = tok.kind
    else {
        return None;
    };

    let text = &src[inner_start as usize..inner_end as usize];

    (!text.contains('\\')).then(|| text.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax::lexer::lex;

    fn spec(src: &str) -> Vec<String> {
        let toks = lex(src).unwrap().toks;
        scan(src, &toks)
            .sites
            .iter()
            .map(|s| src[s.inner_start as usize..s.inner_end as usize].to_string())
            .collect()
    }

    #[test]
    fn finds_plain_requires() {
        assert_eq!(spec(r#"local a = require("./foo")"#), vec!["./foo"]);
        assert_eq!(spec(r#"local a = require('@pkg/x')"#), vec!["@pkg/x"]);
        assert_eq!(spec(r#"local a = require "@pkg/x""#), vec!["@pkg/x"]);
        assert_eq!(spec("local a = require [[@pkg/x]]"), vec!["@pkg/x"]);
    }

    #[test]
    fn skips_members_comments_dynamic() {
        assert!(spec(r#"foo.require("./x")"#).is_empty());
        assert!(spec(r#"foo:require("./x")"#).is_empty());
        assert!(spec(r#"-- require("./x")"#).is_empty());
        assert!(spec(r#"local s = "require('./x')""#).is_empty());
        assert!(spec(r#"require(path)"#).is_empty());
    }

    #[test]
    fn counts_dynamic() {
        let src = r#"require(p) require(`@x/{y}`) local r = require"#;
        let toks = lex(src).unwrap().toks;

        assert_eq!(scan(src, &toks).dynamic.len(), 3);
    }

    #[test]
    fn multiple_sites() {
        let src = r#"
            local A = require("./a") -- comment
            local B = require("../b")
        "#;
        assert_eq!(spec(src), vec!["./a", "../b"]);
    }
}
