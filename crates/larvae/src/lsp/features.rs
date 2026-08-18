/*!
The requests beyond diagnostics: formatting and the outline.
*/

use anyhow::{Context, Result};
use serde_json::{Value, json};

use crate::fmt;
use crate::worm::proto;

use super::uri::path_of_uri;
use super::{Server, rpc};

impl Server {
    /// One edit that replaces the whole document; a formatter produces this
    pub(super) fn format(&self, uri: &str) -> Result<Value> {
        let Some(src) = self.documents.get(uri) else {
            return Ok(Value::Null);
        };

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
        let Some(src) = self.documents.get(uri) else {
            return json!([]);
        };

        let Ok(lexed) = crate::syntax::lexer::lex(src) else {
            return json!([]);
        };

        let Ok(chunk) = crate::syntax::parser::parse(src, &lexed.toks) else {
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
