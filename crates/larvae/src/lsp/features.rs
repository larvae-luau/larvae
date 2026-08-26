/*!
The requests beyond diagnostics: formatting and the outline.
*/

use anyhow::{Context, Result};
use serde_json::{Value, json};

use crate::fmt;
use crate::worm::proto;

use super::uri::path_of_uri;
use super::{Server, rpc};

/// The identifier the author is in the middle of typing, before the cursor
fn word_before(src: &str, at: u32) -> String {
    let head = &src[..at.min(src.len() as u32) as usize];

    head.chars()
        .rev()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
}

/*
The line where an auto-import lands: after the imports the file already
has, before its first real statement.

The scan walks whole lines from the top. A comment, a blank line, or an
existing import (`local`/`const` bound to a `require` or a `GetService`)
extends the preamble; the first line that is none of those ends it. So a
file that opens with a guard clause gets its import above the guard, and
a file with an import block gets the new line at the block's end.
*/
fn import_insertion_line(src: &str) -> u32 {
    let mut last_import_end = 0u32;

    for (line, text) in (0u32..).zip(src.lines()) {
        let trimmed = text.trim_start();

        let is_preamble = trimmed.is_empty()
            || trimmed.starts_with("--")
            || ((trimmed.starts_with("local ") || trimmed.starts_with("const "))
                && (trimmed.contains("require(") || trimmed.contains("GetService(")));

        if !is_preamble {
            break;
        }

        if !trimmed.is_empty() && !trimmed.starts_with("--") {
            last_import_end = line + 1;
        }
    }

    last_import_end
}

/*
An offset of the original, moved onto the lowering by line.

A front-end worm preserves line numbers by contract, so the line carries
over whole, and the column clamps to the lowered line's length. Exact for
the Luau between markup, and close enough at a markup boundary for the
analyzer to anchor.
*/
fn lowered_offset(original: &str, lowered: &str, at: u32) -> u32 {
    let head = &original[..(at as usize).min(original.len())];
    let line = head.matches('\n').count();
    let column = head.len() - head.rfind('\n').map(|n| n + 1).unwrap_or(0);

    let mut start = 0usize;

    for (i, text) in lowered.split_inclusive('\n').enumerate() {
        if i == line {
            let content = text.trim_end_matches('\n').len();

            return (start + column.min(content)) as u32;
        }

        start += text.len();
    }

    lowered.len() as u32
}

/// The byte offset of the position in a request's params
fn position_byte(src: &str, params: &Value) -> u32 {
    let line = params["position"]["line"].as_u64().unwrap_or(0) as u32;
    let character = params["position"]["character"].as_u64().unwrap_or(0) as u32;

    rpc::Lines::new(src).byte_of(src, line, character)
}

impl Server {
    /*
    The type at the cursor, from the analyzer behind the seam.

    The position arrives as a line and a UTF-16 character, converts to a
    byte offset once here, and crosses the seam as bytes. No analyzer, or
    a claimed file, answers null, and the editor shows nothing.
    */
    pub(super) fn hover(&self, params: &Value) -> Value {
        let uri = super::uri::uri_of(params);

        // `[lsp.hover] enabled = false` answers with nothing, as luau-lsp does.
        if !self.lsp.hover.enabled || self.declines(&uri) {
            return Value::Null;
        }

        let Some(src) = self.documents.get(&uri) else {
            return Value::Null;
        };

        let Some(path) = path_of_uri(&uri) else {
            return Value::Null;
        };

        let at = position_byte(src, params);
        let context = json!({ "path": path, "text": src, "offset": at });

        /*
        A claimed file gets both halves. The worm's respond hook answers
        the markup, ex: the class behind a tag, and wins where it answers.
        The Luau between the markup goes to the analyzer as the worm's
        lowering, positions mapped by line, because a claimed front-end
        preserves lines by contract.
        */
        if let Some(index) = self.worms.frontend_for(&path) {
            let from_worm = self.worms.lsp_respond("hover", &context, Value::Null);

            if !from_worm.is_null() {
                return from_worm;
            }

            let Ok(outcome) = self.worms.compile(index, src) else {
                return Value::Null;
            };

            if !outcome.ok {
                return Value::Null;
            }

            let lowered = super::analysis::plain_view(&outcome.text);
            let mut analysis = self.analysis.borrow_mut();

            let Some(text) = analysis.as_mut().and_then(|a| {
                a.open(&path, &lowered);

                a.hover(
                    &path,
                    lowered_offset(src, &lowered, at),
                    self.lsp.hover.show_table_kinds,
                )
            }) else {
                return Value::Null;
            };

            return json!({
                "contents": { "kind": "markdown", "value": format!("```luau\n{text}\n```") }
            });
        }

        let view = super::analysis::plain_view(src);
        let mut analysis = self.analysis.borrow_mut();

        let Some(text) = analysis.as_mut().and_then(|a| {
            a.open(&path, &view);

            a.hover(&path, at, self.lsp.hover.show_table_kinds)
        }) else {
            return Value::Null;
        };

        drop(analysis);

        let hover = json!({
            "contents": { "kind": "markdown", "value": format!("```luau\n{text}\n```") }
        });

        // Tier 3: the worms that transform hovers see it before the editor.
        self.worms.lsp_respond("hover", &context, hover)
    }

    /// Completions at the cursor, from the analyzer behind the seam
    pub(super) fn completions(&self, params: &Value) -> Value {
        let uri = super::uri::uri_of(params);

        if !self.lsp.completion.enabled || self.declines(&uri) {
            return json!([]);
        }

        let Some(src) = self.documents.get(&uri) else {
            return json!([]);
        };

        let Some(path) = path_of_uri(&uri) else {
            return json!([]);
        };

        let at = position_byte(src, params);
        let context = json!({ "path": path, "text": src, "offset": at });

        /*
        A claimed file's completions are the worm's markup answers plus
        the analyzer's answers over the lowering, in one list.
        */
        if let Some(index) = self.worms.frontend_for(&path) {
            let mut items = match self.worms.compile(index, src) {
                Ok(outcome) if outcome.ok => {
                    let lowered = super::analysis::plain_view(&outcome.text);
                    let mut analysis = self.analysis.borrow_mut();

                    analysis
                        .as_mut()
                        .map(|a| {
                            a.open(&path, &lowered);

                            a.completions(&path, lowered_offset(src, &lowered, at))
                        })
                        .unwrap_or_default()
                }

                _ => Vec::new(),
            };

            let base: Vec<Value> = items
                .drain(..)
                .map(|c| {
                    json!({
                        "label": c.label,
                        "kind": c.kind,
                        "detail": c.detail,
                        "sortText": format!("5{}", c.label),
                    })
                })
                .collect();

            return self.worms.lsp_respond("completions", &context, json!(base));
        }

        let view = super::analysis::plain_view(src);
        let mut analysis = self.analysis.borrow_mut();

        let Some(analysis) = analysis.as_mut() else {
            return json!([]);
        };

        analysis.open(&path, &view);

        /*
        The order the editor shows is the order these tiers spell. A
        keyword that fits the position outranks everything, because the
        bug this design answers is real: an author types `end` to close a
        guard clause and the list hands them EncodingService. An exactly
        typed keyword also preselects, so enter confirms what the author
        wrote. Auto-imports rank last: they are the most speculative
        offer in the list, and they must never win a race against syntax.
        */
        let prefix = word_before(src, at);

        let mut items: Vec<Value> = analysis
            .completions(&path, at)
            .into_iter()
            // 14 is Keyword. A project that finds them noisy turns them off.
            .filter(|c| self.lsp.completion.show_keywords || c.kind != 14)
            .map(|c| {
                let tier = match c.kind {
                    14 => '0',

                    5 | 10 => '1',

                    3 | 12 => '2',

                    _ => '5',
                };

                let mut item = json!({
                    "label": c.label,
                    "kind": c.kind,
                    "detail": c.detail,
                    "sortText": format!("{tier}{}", c.label),
                });

                if c.kind == 14 && !prefix.is_empty() && c.label == prefix {
                    item["preselect"] = json!(true);
                }

                item
            })
            .collect();

        /*
        Service auto-imports, the parity feature with the fix built in.
        Each one carries its own insertion: a binding above the first real
        statement of the file, never inside the block the cursor sits in. A
        service the file already binds does not offer.

        `[lsp.completion.imports] use_const` decides the keyword, and the
        detail line shows the same text the edit inserts. A user reads that
        line before accepting, so the two cannot differ.
        */
        if !prefix.is_empty() && self.lsp.completion.imports.enabled {
            let keyword = self.lsp.completion.imports.keyword();
            let lines = rpc::Lines::new(src);

            for service in analysis.services() {
                if !service.starts_with(prefix.as_str())
                    || src.contains(&format!("GetService(\"{service}\")"))
                {
                    continue;
                }

                let insert_at = import_insertion_line(src);

                items.push(json!({
                    "label": service,
                    "kind": 9,
                    "detail": format!(
                        "auto-import: {keyword} {service} = game:GetService(\"{service}\")"
                    ),
                    "sortText": format!("9{service}"),
                    "additionalTextEdits": [{
                        "range": {
                            "start": { "line": insert_at, "character": 0 },
                            "end": { "line": insert_at, "character": 0 },
                        },
                        "newText": format!(
                            "{keyword} {service} = game:GetService(\"{service}\")\n"
                        ),
                    }],
                }));
            }

            let _ = lines;
        }

        // Tier 3: the worms that transform completions see the list first.
        self.worms
            .lsp_respond("completions", &context, json!(items))
    }

    /// Reports if the `[lsp]` mode leaves this file to another server
    fn declines(&self, uri: &str) -> bool {
        if !self.lsp.enabled {
            return true;
        }

        if !self.lsp.claim_only {
            return false;
        }

        /*
        A worm can declare that its hooks answer inside plain Luau files,
        ex: the json worm resolving data requires written in .luau code.
        Claim-only gating widens then, or installing the worm changes
        nothing in the editor.
        */
        if self.worms.lsp_serves_luau() {
            return false;
        }

        !path_of_uri(uri).is_some_and(|p| self.worms.frontend_for(&p).is_some())
    }

    /// One edit that replaces the whole document; a formatter produces this
    pub(super) fn format(&self, uri: &str) -> Result<Value> {
        if self.declines(uri) {
            return Ok(Value::Null);
        }

        let Some(src) = self.documents.get(uri) else {
            return Ok(Value::Null);
        };

        // `[fmt] enabled = false` reaches the editor as a formatter with no edits
        if !self.fmt.enabled {
            return Ok(json!([]));
        }

        let Some(formatted) = self.formatted(uri, src)? else {
            return Ok(json!([]));
        };

        // An edit that changes nothing still makes the editor mark the file dirty.
        if formatted == *src {
            return Ok(json!([]));
        }

        Ok(json!([{
            "range": rpc::Lines::new(src).whole(src),
            "newText": formatted,
        }]))
    }

    /*
    The formatted text of one document, from the owner of its extension.

    A claimed file goes to its worm. The worm replies with a layout document,
    and larvae renders it in the style of the project. A worm that does not
    format its files gives `None` here, and the server then sends no edit. A
    message is correct for `larvae fmt`, because a user named that file. A
    message is wrong for an editor, because the editor asks on each save.
    */
    fn formatted(&self, uri: &str, src: &str) -> Result<Option<String>> {
        let Some(index) = path_of_uri(uri).and_then(|p| self.worms.frontend_for(&p)) else {
            return fmt::format(src, &self.fmt).map(Some);
        };

        let spec = self.worms.spec(index);

        if !spec.formats() {
            return Ok(None);
        }

        let reply = self.worms.format(index, src)?;

        // a project can keep one option out of the files this worm claims
        let cfg = self.fmt.without(&spec.inherit.fmt_except);

        proto::render_format(src, &reply, &cfg)
            .with_context(|| format!("worm `{}`", spec.manifest.name))
            .map(Some)
    }

    /*
    The outline; a symbol picker and the breadcrumb bar read it.

    Top level declarations only. No user navigates to a nested helper by
    name. A list of them makes the outline of a large module longer than the
    module.
    */
    pub(super) fn symbols(&self, uri: &str) -> Value {
        if self.declines(uri) {
            return json!([]);
        }

        let Some(src) = self.documents.get(uri) else {
            return json!([]);
        };

        let Ok(lexed) = crate::syntax::lexer::lex(src) else {
            return json!([]);
        };

        let options = path_of_uri(uri)
            .map(|p| crate::syntax::parser::ParseOptions::for_path(&p))
            .unwrap_or_default();

        let Ok(chunk) = crate::syntax::parser::parse_with(src, &lexed.toks, options) else {
            return json!([]);
        };

        let lines = rpc::Lines::new(src);
        let bytes = |span: crate::syntax::ast::TokSpan| {
            (
                lexed.toks[span.start as usize].start,
                lexed.toks[span.end as usize - 1].end,
            )
        };

        let mut out = Vec::new();

        for stmt in &chunk.block.stmts {
            use crate::syntax::ast::Stmt;

            // 12 is Function and 13 is Variable, in the numbering of the protocol.
            let (name, kind, span) = match stmt {
                Stmt::Function(n) => {
                    let path: Vec<&str> = n
                        .path
                        .iter()
                        .map(|p| lexed.toks[p.start as usize].text(src))
                        .collect();

                    (path.join("."), 12, n.span)
                }

                Stmt::Class(n) => (
                    lexed.toks[n.name.start as usize].text(src).to_string(),
                    // 5 is Class, in the numbering of the protocol.
                    5,
                    n.span,
                ),

                Stmt::LocalFunction(n) => (
                    lexed.toks[n.name.start as usize].text(src).to_string(),
                    12,
                    n.span,
                ),

                Stmt::Local(n) => match n.names.as_slice() {
                    [binding] => (
                        lexed.toks[binding.name.start as usize]
                            .text(src)
                            .to_string(),
                        13,
                        n.span,
                    ),

                    _ => continue,
                },

                _ => continue,
            };

            let range = lines.range(src, bytes(span));

            out.push(json!({
                "name": name,
                "kind": kind,
                "range": range,
                "selectionRange": range,
            }));
        }

        json!(out)
    }
}
