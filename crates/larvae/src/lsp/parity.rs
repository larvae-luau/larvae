/*!
The requests that larvae answers from its own parser, in protocol shape.

The answers live in `navigate`, `structure` and `decorate`, which know
nothing about JSON and take a byte offset. This module is the seam: it turns
a protocol position into a byte, calls the answer, and turns byte ranges back
into positions. Keeping the two apart is what lets those modules be tested
with plain strings and no server.

None of this needs the Luau analyzer. The analyzer is a C++ build that only
happens where the vendored Luau is checked out, and a user without it still
gets navigation, folding, symbols, links and colours.
*/

use serde_json::{Value, json};

use super::{Server, decorate, navigate, rpc, structure, uri::path_of_uri};

impl Server {
    /// The document and its line index, for a request that names a document
    fn document(&self, params: &Value) -> Option<(&String, rpc::Lines)> {
        let uri = params["textDocument"]["uri"].as_str()?;
        let src = self.documents.get(uri)?;

        Some((src, rpc::Lines::new(src)))
    }

    /// The byte offset a `position` names, in the document a request names
    fn at(&self, params: &Value) -> Option<(&String, rpc::Lines, u32)> {
        let (src, lines) = self.document(params)?;
        let line = params["position"]["line"].as_u64()? as u32;
        let character = params["position"]["character"].as_u64()? as u32;
        let byte = lines.byte_of(src, line, character);

        Some((src, lines, byte))
    }

    /*
    Goto definition, for a binding this file declares.

    Cross module definition needs the analyzer, so a name that comes from a
    require answers with nothing here rather than with a guess.
    */
    pub(super) fn definition(&self, params: &Value) -> Value {
        let Some((src, lines, byte)) = self.at(params) else {
            return Value::Null;
        };

        /*
        The local answer first, because it needs no type checker and it is
        exact. A name that comes through a require, a method on an imported
        table, or a global from the definitions has no local declaration, and
        only the analyzer can follow it.
        */
        if let Some(span) = navigate::definition(src, byte) {
            return json!({
                "uri": params["textDocument"]["uri"],
                "range": lines.range(src, span),
            });
        }

        let Some(path) = params["textDocument"]["uri"].as_str().and_then(path_of_uri) else {
            return Value::Null;
        };

        let found = self
            .analysis
            .borrow_mut()
            .as_mut()
            .and_then(|a| a.definition(&path, byte));

        match found {
            Some(at) => location(&at),

            None => Value::Null,
        }
    }

    /// Goto the declaration of the type, which only the analyzer knows
    pub(super) fn type_definition(&self, params: &Value) -> Value {
        let Some((_, _, byte)) = self.at(params) else {
            return Value::Null;
        };

        let Some(path) = params["textDocument"]["uri"].as_str().and_then(path_of_uri) else {
            return Value::Null;
        };

        let found = self
            .analysis
            .borrow_mut()
            .as_mut()
            .and_then(|a| a.type_definition(&path, byte));

        match found {
            Some(at) => location(&at),

            None => Value::Null,
        }
    }

    /// Find references, for a binding this file declares
    pub(super) fn references(&self, params: &Value) -> Value {
        let Some((src, lines, byte)) = self.at(params) else {
            return json!([]);
        };

        let include = params["context"]["includeDeclaration"]
            .as_bool()
            .unwrap_or(true);

        let uri = &params["textDocument"]["uri"];

        let out: Vec<Value> = navigate::references(src, byte, include)
            .into_iter()
            .map(|span| json!({ "uri": uri, "range": lines.range(src, span) }))
            .collect();

        json!(out)
    }

    /// The other uses of the binding under the cursor, for the editor to shade
    pub(super) fn highlights(&self, params: &Value) -> Value {
        let Some((src, lines, byte)) = self.at(params) else {
            return json!([]);
        };

        let out: Vec<Value> = navigate::highlights(src, byte)
            .into_iter()
            .map(|(span, kind)| json!({ "range": lines.range(src, span), "kind": kind.code() }))
            .collect();

        json!(out)
    }

    /*
    Rename, for a binding this file declares.

    The answer is null when the name cannot be renamed safely, and the
    module decides that. A partial edit is worse than no edit: it leaves a
    file that does not compile and the user has to find the pieces.
    */
    pub(super) fn rename(&self, params: &Value) -> Value {
        let Some((src, lines, byte)) = self.at(params) else {
            return Value::Null;
        };

        let Some(new_name) = params["newName"].as_str() else {
            return Value::Null;
        };

        let Some(spans) = navigate::rename(src, byte, new_name) else {
            return Value::Null;
        };

        let edits: Vec<Value> = spans
            .into_iter()
            .map(|span| json!({ "range": lines.range(src, span), "newText": new_name }))
            .collect();

        let uri = params["textDocument"]["uri"].as_str().unwrap_or_default();

        json!({ "changes": { uri: edits } })
    }

    /// The foldable regions of the document
    pub(super) fn folding_ranges(&self, params: &Value) -> Value {
        let Some((src, _)) = self.document(params) else {
            return json!([]);
        };

        let out: Vec<Value> = structure::folding_ranges(src)
            .into_iter()
            .map(|r| match r.kind {
                Some(kind) => json!({ "startLine": r.start, "endLine": r.end, "kind": kind }),

                None => json!({ "startLine": r.start, "endLine": r.end }),
            })
            .collect();

        json!(out)
    }

    /*
    The chain of ranges around each position the request names.

    The protocol wants a linked list from the innermost outward, and the
    module answers innermost first, so the chain is built from the back.
    */
    pub(super) fn selection_ranges(&self, params: &Value) -> Value {
        let Some((src, lines)) = self.document(params) else {
            return json!([]);
        };

        let positions = params["positions"].as_array().cloned().unwrap_or_default();

        let out: Vec<Value> = positions
            .iter()
            .map(|position| {
                let line = position["line"].as_u64().unwrap_or(0) as u32;
                let character = position["character"].as_u64().unwrap_or(0) as u32;
                let byte = lines.byte_of(src, line, character);

                let mut chain = Value::Null;

                for span in structure::selection_ranges(src, byte).into_iter().rev() {
                    chain = match chain {
                        Value::Null => json!({ "range": lines.range(src, span) }),

                        parent => json!({ "range": lines.range(src, span), "parent": parent }),
                    };
                }

                chain
            })
            .collect();

        json!(out)
    }

    /// A clickable link on every require that resolves to a file
    pub(super) fn links(&self, params: &Value) -> Value {
        let Some((src, lines)) = self.document(params) else {
            return json!([]);
        };

        let Some(path) = params["textDocument"]["uri"].as_str().and_then(path_of_uri) else {
            return json!([]);
        };

        let Some(root) = self.root.as_deref() else {
            return json!([]);
        };

        let out: Vec<Value> = decorate::links_with_aliases(src, &path, root, &self.aliases)
            .into_iter()
            .filter_map(|link| {
                let target = super::uri::uri_of_path(&link.target)?;

                Some(json!({ "range": lines.range(src, link.range), "target": target }))
            })
            .collect();

        json!(out)
    }

    /// A swatch on every Color3 the file writes out in full
    pub(super) fn colors(&self, params: &Value) -> Value {
        let Some((src, lines)) = self.document(params) else {
            return json!([]);
        };

        let out: Vec<Value> = decorate::colors(src)
            .into_iter()
            .map(|c| {
                json!({
                    "range": lines.range(src, c.range),
                    "color": { "red": c.red, "green": c.green, "blue": c.blue, "alpha": 1.0 },
                })
            })
            .collect();

        json!(out)
    }

    /*
    The texts offered when a user picks a colour from a swatch.

    The form the user already wrote comes first, so an edit of a `fromRGB`
    gives back a `fromRGB`. The request carries the range, so the written
    form is read back from the source rather than carried through the client.
    */
    pub(super) fn color_presentation(&self, params: &Value) -> Value {
        let Some((src, lines)) = self.document(params) else {
            return json!([]);
        };

        let color = &params["color"];
        let red = color["red"].as_f64().unwrap_or(0.0) as f32;
        let green = color["green"].as_f64().unwrap_or(0.0) as f32;
        let blue = color["blue"].as_f64().unwrap_or(0.0) as f32;

        let start = &params["range"]["start"];
        let byte = lines.byte_of(
            src,
            start["line"].as_u64().unwrap_or(0) as u32,
            start["character"].as_u64().unwrap_or(0) as u32,
        );

        let form = decorate::colors(src)
            .into_iter()
            .find(|c| c.range.0 <= byte && byte < c.range.1)
            .map(|c| c.form)
            .unwrap_or(decorate::Form::FromRgb);

        let out: Vec<Value> = decorate::color_presentation(red, green, blue, form)
            .into_iter()
            .map(|label| json!({ "label": label }))
            .collect();

        json!(out)
    }

    /*
    The document outline, as a tree.

    Larvae shipped a flat list, and an outline of a large module then read as
    one long run of names with no shape. The protocol has carried
    `DocumentSymbol` with children for years, and every editor larvae
    targets prefers it. A function inside a function is a child of it now.
    */
    pub(super) fn symbol_tree(&self, uri: &str) -> Value {
        let Some(src) = self.documents.get(uri) else {
            return json!([]);
        };

        let lines = rpc::Lines::new(src);

        fn render(node: &structure::Symbol, src: &str, lines: &rpc::Lines) -> Value {
            let children: Vec<Value> = node
                .children
                .iter()
                .map(|c| render(c, src, lines))
                .collect();

            json!({
                "name": node.name,
                "kind": node.kind,
                "range": lines.range(src, node.range),
                "selectionRange": lines.range(src, node.selection),
                "children": children,
            })
        }

        let out: Vec<Value> = structure::symbols(src)
            .iter()
            .map(|s| render(s, src, &lines))
            .collect();

        json!(out)
    }
}

impl Server {
    /*
    Build the project symbol index.

    A build with no root is an empty index rather than a walk of whatever
    directory the server happens to sit in.
    */
    pub(super) fn reindex(&mut self) {
        if !self.lsp.index.enabled {
            self.symbols = super::workspace::Index::default();

            return;
        }

        self.symbols = match self.root.as_deref() {
            Some(root) => super::workspace::Index::build(root, &self.excluded),

            None => super::workspace::Index::default(),
        };
    }

    /*
    Search the project for a symbol by name.

    The index reads the tree as it was last saved, so an unsaved rename
    answers with the old name until the file is written. That is the same
    bargain every editor makes with a project wide search, and the
    alternative is a re-parse of every open buffer on each keystroke.
    */
    pub(super) fn workspace_symbols(&self, params: &Value) -> Value {
        let query = params["query"].as_str().unwrap_or_default();

        // The picker opens with an empty query, and a project dump is not an answer.
        if query.is_empty() {
            return json!([]);
        }

        let out: Vec<Value> = self
            .symbols
            .search(query, 256)
            .into_iter()
            .filter_map(|found| {
                let uri = super::uri::uri_of_path(&found.path)?;

                Some(json!({
                    "name": found.name,
                    "kind": found.kind,
                    "containerName": found.container,
                    "location": {
                        "uri": uri,
                        "range": {
                            "start": { "line": found.range.0, "character": 0 },
                            "end": { "line": found.range.1, "character": 0 },
                        },
                    },
                }))
            })
            .collect();

        json!(out)
    }

    /*
    The whole document, coloured.

    The encoding is relative: each token carries its distance from the one
    before it. That is the protocol's shape and the module builds it, so
    nothing here does arithmetic on positions.
    */
    pub(super) fn semantic_tokens(&self, params: &Value) -> Value {
        let Some((src, _)) = self.document(params) else {
            return Value::Null;
        };

        // The colours read the globals the project has, not a fixed platform.
        json!({ "data": super::tokens::semantic_tokens_for(src, self.lint.std) })
    }

    /// The signature of the call the caret sits in; the analyzer alone knows it
    pub(super) fn signature_help(&self, params: &Value) -> Value {
        if !self.lsp.signature_help.enabled {
            return Value::Null;
        }

        let Some((_, _, byte)) = self.at(params) else {
            return Value::Null;
        };

        let Some(path) = params["textDocument"]["uri"].as_str().and_then(path_of_uri) else {
            return Value::Null;
        };

        let found = self
            .analysis
            .borrow_mut()
            .as_mut()
            .and_then(|a| a.signature(&path, byte));

        let Some(sig) = found else {
            return Value::Null;
        };

        let parameters: Vec<Value> = sig
            .parameters
            .iter()
            .map(|p| json!({ "label": self.instances.readable(p) }))
            .collect();

        json!({
            "signatures": [{
                "label": self.instances.readable(&sig.label),
                "parameters": parameters,
                "activeParameter": sig.active,
            }],
            "activeSignature": 0,
            "activeParameter": sig.active,
        })
    }

    /*
    The types the author left out, for the range the editor asked about.

    The analyzer answers for the whole module, because a type check is per
    module and a range would not make it cheaper. The filter happens here,
    so the editor receives only what it drew.
    */
    pub(super) fn inlay_hints(&self, params: &Value) -> Value {
        let Some(path) = params["textDocument"]["uri"].as_str().and_then(path_of_uri) else {
            return json!([]);
        };

        /*
        A hint is text the editor draws into a line the author did not
        write, so nothing is drawn until the project asks. The two kinds
        are separate because a reader often wants one and not the other.
        */
        let cfg = &self.lsp.inlay_hints;

        if !cfg.variable_types && !cfg.parameter_types {
            return json!([]);
        }

        let hints = self
            .analysis
            .borrow_mut()
            .as_mut()
            .map(|a| a.hints(&path))
            .unwrap_or_default();

        let from = params["range"]["start"]["line"].as_u64().map(|l| l as u32);
        let to = params["range"]["end"]["line"].as_u64().map(|l| l as u32);

        let out: Vec<Value> = hints
            .into_iter()
            .filter(|h| from.is_none_or(|f| h.line >= f) && to.is_none_or(|t| h.line <= t))
            .map(|h| {
                let text = self.instances.readable(&h.label);

                // A hint longer than the code it annotates hides the code.
                let label = match text.chars().count() > cfg.type_hint_max_length {
                    true => {
                        let kept: String = text.chars().take(cfg.type_hint_max_length).collect();

                        format!("{kept}...")
                    }

                    false => text.into_owned(),
                };

                json!({
                    "position": { "line": h.line, "character": h.character },
                    "label": label,
                    "kind": h.kind,
                    "paddingLeft": false,
                })
            })
            .collect();

        json!(out)
    }
}

/*
An analyzer location in protocol shape.

The analyzer answers in line and character already, because the target is
often a module the server has no text for. So nothing converts here.
*/
fn location(at: &super::analysis::AnalysisLocation) -> Value {
    let Some(uri) = super::uri::uri_of_path(&at.path) else {
        return Value::Null;
    };

    json!({
        "uri": uri,
        "range": {
            "start": { "line": at.start.0, "character": at.start.1 },
            "end": { "line": at.end.0, "character": at.end.1 },
        },
    })
}
