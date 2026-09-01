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
            Some(at) => location(&self.real_path(at)),

            None => Value::Null,
        }
    }

    /*
    A definition file's own path, in place of the name it loaded under.

    `[lsp] definitions` loads each file as `@user/<entry>`, which is the
    analyzer's name for it and not a path an editor can open. The entry
    is the path relative to the root, so the name maps straight back.
    */
    fn real_path(
        &self,
        mut at: super::analysis::AnalysisLocation,
    ) -> super::analysis::AnalysisLocation {
        let Some(root) = self.root.as_deref() else {
            return at;
        };

        if let Some(entry) = at.path.to_string_lossy().strip_prefix("@user/") {
            at.path = root.join(entry);
        }

        at
    }

    /// Goto the declaration of the type, which only the analyzer knows
    pub(super) fn type_definition(&self, params: &Value) -> Value {
        // `[lsp] analyzer = false` turns this half off whole.
        if !self.lsp.analyzer {
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
            .and_then(|a| a.type_definition(&path, byte));

        match found {
            Some(at) => location(&self.real_path(at)),

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

        let mut out: Vec<Value> = navigate::references(src, byte, include)
            .into_iter()
            .map(|span| json!({ "uri": uri, "range": lines.range(src, span) }))
            .collect();

        /*
        The project answers next, for a name the file does not own.

        A local is answered above and belongs to one file by
        definition. Everything else, a module's export or a type, is
        used in files this one never mentions, and an editor that lists
        one file for those is listing the wrong thing. The walk asks
        the analyzer where each candidate resolves and keeps the ones
        that land on the declaration the cursor named, so a name that
        two modules both use does not collect the other module's uses.
        */
        let already: std::collections::HashSet<String> =
            out.iter().map(|hit| hit["range"].to_string()).collect();

        if let Some(found) = self.project_references(params, src, byte, include) {
            out.extend(
                found
                    .into_iter()
                    .filter(|hit| !already.contains(&hit["range"].to_string())),
            );
        }

        json!(out)
    }

    /*
    The uses of one declaration across the project.

    `None` means the question is not a project question: the cursor
    named a local, or there is no analyzer, or no root to walk. The
    declaration is the identity, so the walk resolves every candidate
    and compares: same file, same position, same name.

    The token filter is what makes this affordable. A file that never
    spells the name cannot use it, so the analyzer sees a handful of
    files rather than the project.
    */
    fn project_references(
        &self,
        params: &Value,
        src: &str,
        byte: u32,
        include: bool,
    ) -> Option<Vec<Value>> {
        let root = self.root.clone()?;
        let name = super::navigate::name_at(src, byte)?;
        let here = params["textDocument"]["uri"]
            .as_str()
            .and_then(path_of_uri)?;

        // The declaration this question is about, as the analyzer sees it.
        let anchor = {
            let mut analysis = self.analysis.borrow_mut();
            let analysis = analysis.as_mut()?;

            analysis.definition(&here, byte)?
        };

        let mut out = Vec::new();

        for path in self.project_files(&root) {
            let text = match path == here {
                true => src.to_owned(),

                false => match self.documents.get(&super::uri::uri_of_path(&path)?) {
                    Some(open) => open.clone(),

                    None => std::fs::read_to_string(&path).ok()?,
                },
            };

            if !text.contains(name.as_str()) {
                continue;
            }

            let lines = rpc::Lines::new(&text);
            let uri = super::uri::uri_of_path(&path)?;

            for (start, end) in super::navigate::occurrences(&text, &name) {
                // The declaration itself is the caller's choice to include.
                let same_file_declaration = path == here
                    && anchor.path == here
                    && lines.position(&text, start).0 == anchor.start.0;

                if same_file_declaration && !include {
                    continue;
                }

                /*
                The borrow ends with the block, before the next
                candidate asks for it again. A resolution that answers
                nothing is a name that is spelled the same and
                declared elsewhere.
                */
                let found = {
                    let mut analysis = self.analysis.borrow_mut();

                    let Some(analysis) = analysis.as_mut() else {
                        return Some(out);
                    };

                    analysis.open(&path, &super::analysis::plain_view(&text));
                    analysis.definition(&path, start)
                };

                let Some(found) = found else {
                    continue;
                };

                if found.path == anchor.path && found.start == anchor.start {
                    out.push(json!({
                        "uri": uri,
                        "range": lines.range(&text, (start, end)),
                    }));
                }
            }
        }

        Some(out)
    }

    /*
    The Luau files of the project, for a walk that answers one request.

    The root is the tree, and not `[process] input`. A use of a name
    lives wherever someone wrote it: a tool, a test, a script beside
    the build. The build reads its inputs, and this question reads the
    project.

    The excludes of the project apply, because a file the project
    excluded is not part of it. A failure to read the config leaves the
    walk empty rather than guessing at a tree.
    */
    fn project_files(&self, root: &std::path::Path) -> Vec<std::path::PathBuf> {
        let Ok((root_in, root_ex)) = crate::commands::fmt::root_lists(root, None) else {
            return Vec::new();
        };

        let Ok(excludes) = self.lint.excludes_under(root, &root_in, &root_ex) else {
            return Vec::new();
        };

        crate::commands::fmt::collect(
            root,
            &[root.to_path_buf()],
            &excludes,
            &self.worms.claimed(),
        )
        .unwrap_or_default()
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

        let answer = json!(out);

        self.hint_cache
            .borrow_mut()
            .insert(uri.to_string(), answer.clone());

        answer
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
        // `[lsp] analyzer = false` turns this half off whole.
        if !self.lsp.analyzer {
            return Value::Null;
        }

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
        // `[lsp] analyzer = false` turns this half off whole.
        if !self.lsp.analyzer {
            return json!([]);
        }

        let Some(path) = params["textDocument"]["uri"].as_str().and_then(path_of_uri) else {
            return json!([]);
        };

        /*
        A hint is text the editor draws into a line the author did not
        write, so nothing is drawn until the project asks. The two kinds
        are separate because a reader often wants one and not the other.
        */
        let cfg = &self.lsp.inlay_hints;
        let names = cfg.parameter_names.mode();

        if !cfg.variable_types && !cfg.parameter_types && !cfg.function_return_types && names == 0 {
            return json!([]);
        }

        /*
        While the author types, the hints hold still. A request that
        lands mid-edit answers with the last settled hints, so the text
        does not jump under the cursor, and the refresh after the pause
        makes the editor ask again for fresh ones.
        */
        let uri = params["textDocument"]["uri"].as_str().unwrap_or_default();
        let typing = cfg.update_delay > 0
            && self.hint_hold.get(uri).is_some_and(|at| {
                at.elapsed() < std::time::Duration::from_millis(cfg.update_delay)
            });

        if typing && let Some(held) = self.hint_cache.borrow().get(uri) {
            return held.clone();
        }

        let hints = self
            .analysis
            .borrow_mut()
            .as_mut()
            .map(|a| {
                a.hints(
                    &path,
                    cfg.variable_types,
                    cfg.parameter_types,
                    cfg.function_return_types,
                    names,
                )
            })
            .unwrap_or_default();

        let from = params["range"]["start"]["line"].as_u64().map(|l| l as u32);
        let to = params["range"]["end"]["line"].as_u64().map(|l| l as u32);

        let out: Vec<Value> = hints
            .into_iter()
            .filter(|h| from.is_none_or(|f| h.line >= f) && to.is_none_or(|t| h.line <= t))
            .map(|h| {
                let mut text = self.instances.readable(&h.label).into_owned();

                /*
                A types-only module hints as `{ }`, which is true and
                says nothing. The view names its exports instead:
                `{ type PlayerData }`. Display speech, not syntax, so
                the accept gate keeps it out of the file.
                */
                if h.kind == 1
                    && super::features::empty_table_label(&text).is_some()
                    && let Some(src) = self.documents.get(uri)
                    && let Some(view) = self.type_export_view(src, &path, h.line)
                {
                    text = format!(": {view}");
                }

                // A hint longer than the code it annotates hides the code.
                let label = match text.chars().count() > cfg.type_hint_max_length {
                    true => {
                        let kept: String = text.chars().take(cfg.type_hint_max_length).collect();

                        format!("{kept}...")
                    }

                    false => text.clone(),
                };

                let mut hint = json!({
                    "position": { "line": h.line, "character": h.character },
                    "label": label,
                    "kind": h.kind,
                    "paddingLeft": false,
                    // A parameter name sits before its argument, and the
                    // argument must not read as glued to it.
                    "paddingRight": h.kind == 2,
                    // The resolve request sends the hint back alone, so it
                    // carries its document.
                    "data": { "uri": uri },
                });

                /*
                A double click accepts a type hint into the file, when the
                type is syntax Luau reads. The edit writes the whole type,
                so a truncated label still accepts what it stands for.
                `@metatable` is display notation, not a type, and a
                parameter name has no written form at a call site, so
                those hints stay display only.

                A type spelled from primitives alone accepts right away. A
                type that names an alias waits for the resolve, where the
                server proves the name means something in this file: the
                printer writes a required module's alias bare, and
                accepting that would write a name the file cannot see.
                */
                if h.kind == 1 && insertable_type(&text) {
                    match primitives_only(&text) {
                        true => {
                            hint["textEdits"] = json!([{
                                "range": {
                                    "start": { "line": h.line, "character": h.character },
                                    "end": { "line": h.line, "character": h.character },
                                },
                                "newText": text,
                            }]);
                        }

                        false => {
                            hint["data"]["insert"] = json!(text);
                        }
                    }
                }

                hint
            })
            .collect();

        json!(out)
    }
}

/*
Report if a type hint's text is syntax an accept can write.

The label prints the way `toString` speaks, and that speech holds
notation no parser reads: `@metatable`, `*error-type*`, and the `...`
tail of a truncated hint. The one honest test is a parse: the label
lands in a type alias, and what does not parse does not insert.
*/
fn insertable_type(label: &str) -> bool {
    let Some(ty) = label.strip_prefix(": ") else {
        return false;
    };

    crate::syntax::parse_one(&format!("type __hint = {ty}\n")).is_ok()
}

impl Server {
    /*
    Where an alias the accept adds goes: with the imports, above the
    first statement, which is where an auto-import lands too.
    */
    pub(super) fn import_line(&self, uri: &str) -> Option<u32> {
        self.documents
            .get(uri)
            .map(|src| super::features::import_insertion_line(src))
    }

    /*
    The same type, written the way this file can say it.

    A hint prints a required module's type bare, ex: `Query`, because
    the printer writes the name the module gave it and knows nothing
    about the file reading it. That name means nothing here, so the
    accept used to stay away. The module is required under some name
    in this file, and through that name the type has a spelling this
    file understands: `jecs.Query`.

    The answer carries the text to write and, under `alias`, the
    declaration to add above the first statement.
    */
    pub(super) fn imported_insert(
        &self,
        uri: &str,
        insert: &str,
    ) -> Option<(String, Option<String>)> {
        let mode = self.lsp.inlay_hints.accept_imports;

        if mode == crate::config::lsp::AcceptImports::Off {
            return None;
        }

        let path = super::uri::path_of_uri(uri)?;
        let root = self.root.as_deref()?;
        let src = self.documents.get(uri)?;

        /*
        The names this file requires, each with the types its module
        exports. A file that requires nothing has nothing to offer, and
        the walk stops before it reads a single module.
        */
        let bindings = self.require_bindings(src, &path, root);

        if bindings.is_empty() {
            return None;
        }

        let mut written = insert.to_owned();
        let mut aliases = Vec::new();

        for name in type_names(insert) {
            /*
            One module has to own the name. Two that export it name
            two types, and a guess between them writes the wrong one
            into someone's file.
            */
            let mut owners = bindings
                .iter()
                .filter(|(_, exports)| exports.contains(&name));

            let (binding, _) = owners.next()?;

            if owners.next().is_some() {
                return None;
            }

            let qualified = format!("{binding}.{name}");

            match mode {
                crate::config::lsp::AcceptImports::Alias => {
                    aliases.push(format!("type {name} = {qualified}"));
                }

                _ => written = replace_name(&written, &name, &qualified),
            }
        }

        if written == insert && aliases.is_empty() {
            return None;
        }

        Some((written, (!aliases.is_empty()).then(|| aliases.join("\n"))))
    }

    /*
    Each `local name = require(...)` of this file, with the types its
    module exports.

    The require resolves the way a document link resolves, so an alias,
    a `@game` path, and a relative spec all answer. A module larvae
    cannot read contributes nothing rather than stopping the walk.
    */
    fn require_bindings(
        &self,
        src: &str,
        path: &std::path::Path,
        root: &std::path::Path,
    ) -> Vec<(String, Vec<String>)> {
        let claimed = self.worms.claimed();
        let mut out = Vec::new();

        for (line, name) in require_locals(src) {
            let Some(target) = super::decorate::require_target_on_line(
                src,
                path,
                root,
                &self.aliases,
                &claimed,
                &self.mounts,
                line,
            ) else {
                continue;
            };

            let text = super::uri::uri_of_path(&target)
                .and_then(|uri| self.documents.get(&uri).cloned())
                .or_else(|| std::fs::read_to_string(&target).ok());

            let Some(text) = text else {
                continue;
            };

            let exports = super::features::export_type_names(&text);

            if !exports.is_empty() {
                out.push((name, exports));
            }
        }

        out
    }

    /*
    Prove one type text means something in this file.

    The type lands in a throwaway alias appended to the buffer, and the
    analyzer checks the whole thing. A diagnostic on the appended line
    is a name this file cannot see, and the accept stays away. The real
    text goes back in afterward, so the next request reads the buffer
    the author has.
    */
    pub(super) fn type_resolves_here(&self, uri: &str, insert: &str) -> bool {
        // A claimed file's analyzer view is the worm's lowering, and the
        // probe would splice into the wrong text.
        let Some(path) = super::uri::path_of_uri(uri) else {
            return false;
        };

        if self.worms.frontend_for(&path).is_some() {
            return false;
        }

        let Some(src) = self.documents.get(uri) else {
            return false;
        };

        let ty = insert.trim_start().trim_start_matches(':').trim_start();

        /*
        The alias splices in before the last top-level return, because a
        statement after one is a parse error and every module ends with
        one. A file without a return takes the alias at its end.
        */
        let at = last_top_level_return(src).unwrap_or(src.len());
        let alias = format!("type __larvae_probe = {ty}\n");
        let probe = format!("{}{alias}{}", &src[..at], &src[at..]);
        let inserted = at..at + alias.len();

        let mut analysis = self.analysis.borrow_mut();

        let Some(analysis) = analysis.as_mut() else {
            return false;
        };

        analysis.open(&path, &super::analysis::plain_view(&probe));

        let clean = analysis
            .check(&path)
            .into_iter()
            .all(|d| !inserted.contains(&(d.span.0 as usize)));

        analysis.open(&path, &super::analysis::plain_view(src));

        clean
    }
}

/*
The byte where the last top-level `return` starts.

`function`, `if`, `do`, and `repeat` open a block; `while` and `for` do
not, because their own `do` opens it. A `return` at depth zero is the
module's, and the last one is where the module ends.
*/
fn last_top_level_return(src: &str) -> Option<usize> {
    use crate::syntax::lexer::TokKind;

    let lexed = crate::syntax::lexer::lex(src).ok()?;
    let mut depth = 0i32;
    let mut found = None;

    for tok in &lexed.toks {
        if tok.kind != TokKind::Ident {
            continue;
        }

        match tok.text(src) {
            "function" | "if" | "do" | "repeat" => depth += 1,
            "end" | "until" => depth -= 1,
            "return" if depth == 0 => found = Some(tok.start as usize),

            _ => {}
        }
    }

    found
}

/*
Report if a type spells itself from primitives alone.

Such a type means the same thing in every file, so the accept needs no
scope proof. Anything with a name in it waits for the resolve check.
*/
fn primitives_only(label: &str) -> bool {
    let mut chars = label.char_indices().peekable();

    while let Some((at, c)) = chars.next() {
        if !(c.is_ascii_alphabetic() || c == '_') {
            continue;
        }

        let mut end = at + c.len_utf8();

        while let Some(&(next, nc)) = chars.peek() {
            if nc.is_ascii_alphanumeric() || nc == '_' {
                end = next + nc.len_utf8();
                chars.next();
            } else {
                break;
            }
        }

        let word = &label[at..end];

        // A word a colon follows is a field or a parameter name, not a
        // type reference, and a name is what the scope proof is for.
        if label[end..].trim_start().starts_with(':') {
            continue;
        }

        if !matches!(
            word,
            "number"
                | "string"
                | "boolean"
                | "nil"
                | "any"
                | "unknown"
                | "never"
                | "thread"
                | "buffer"
                | "true"
                | "false"
        ) {
            return false;
        }
    }

    true
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
#[cfg(test)]
mod hint_accepts {
    use super::insertable_type;

    /// Primitives accept at once; a named type waits for the scope proof.
    #[test]
    fn primitives_accept_at_once() {
        use super::primitives_only;

        assert!(primitives_only(": number"));
        assert!(primitives_only(": { a: number, b: string? }"));
        assert!(primitives_only(": (number) -> string"));
        assert!(primitives_only(": nil"));

        assert!(!primitives_only(": Config"));
        assert!(!primitives_only(": { part: Part }"));
        assert!(!primitives_only(": Folder | Part"));
        assert!(!primitives_only(": typeof(x)"));
    }

    /// Real type syntax inserts; display notation stays display.
    #[test]
    fn only_syntax_inserts() {
        assert!(insertable_type(": number"));
        assert!(insertable_type(": { hp: number, name: string }"));
        assert!(insertable_type(": (number) -> string"));
        assert!(insertable_type(
            ": { a: number, e: number, f: boolean, foo: string, test: number }"
        ));
        assert!(insertable_type(": Folder | Part"));

        // toString notation, not types: these never insert.
        assert!(!insertable_type(": { @metatable Class, {  } }"));
        assert!(!insertable_type(": *error-type*"));
        assert!(!insertable_type(": { hp: number, na..."));
        assert!(!insertable_type("name:"));
    }
}

/*
The names a type text mentions, each one a candidate for a module.

A name that a dot follows is already qualified, and a name that
follows a dot is a field of one, so neither is a bare alias this file
has to know. Everything else is a name the parser would look up.
*/
fn type_names(insert: &str) -> Vec<String> {
    let bytes = insert.as_bytes();
    let mut out: Vec<String> = Vec::new();
    let mut at = 0;

    while at < bytes.len() {
        if !(bytes[at].is_ascii_alphabetic() || bytes[at] == b'_') {
            at += 1;

            continue;
        }

        let start = at;

        while at < bytes.len() && (bytes[at].is_ascii_alphanumeric() || bytes[at] == b'_') {
            at += 1;
        }

        let name = &insert[start..at];
        let after = insert[at..].chars().next();
        let before = insert[..start].chars().next_back();

        // A field of something, or a name that already names a module.
        if before == Some('.') || after == Some('.') {
            continue;
        }

        /*
        A name a colon follows is a key or a parameter, not a type:
        the `first` of `{ first: Query }` and the `x` of `(x: number)`.
        Qualifying one would write a module in front of a field name.
        */
        if insert[at..].trim_start().starts_with(':') {
            continue;
        }

        // The words a type text holds that are not names to look up.
        if matches!(
            name,
            "string"
                | "number"
                | "boolean"
                | "nil"
                | "any"
                | "unknown"
                | "never"
                | "thread"
                | "buffer"
                | "typeof"
                | "keyof"
                | "rawkeyof"
                | "read"
                | "write"
                | "true"
                | "false"
        ) {
            continue;
        }

        if !out.iter().any(|held| held == name) {
            out.push(name.to_owned());
        }
    }

    out
}

/// The same text with one whole name replaced, fields left alone
fn replace_name(text: &str, name: &str, with: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;

    while let Some(at) = rest.find(name) {
        let before = rest[..at].chars().next_back();
        let after = rest[at + name.len()..].chars().next();

        let whole = !before.is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '.')
            && !after.is_some_and(|c| c.is_alphanumeric() || c == '_');

        out.push_str(&rest[..at]);
        out.push_str(match whole {
            true => with,

            false => name,
        });

        rest = &rest[at + name.len()..];
    }

    out.push_str(rest);

    out
}

/*
The `local name = require(...)` statements of a file, by line.

The scan reads tokens and not a tree, because the buffer under an
editor is often mid-edit and a tree of it may not exist. A line that
binds one name to one require is the shape every import takes.
*/
fn require_locals(src: &str) -> Vec<(u32, String)> {
    let mut out = Vec::new();

    for (index, line) in src.lines().enumerate() {
        let text = line.trim_start();

        let rest = text
            .strip_prefix("local ")
            .or_else(|| text.strip_prefix("const "))
            .unwrap_or_default();

        let Some((name, value)) = rest.split_once('=') else {
            continue;
        };

        let name = name.trim();

        if !value.trim_start().starts_with("require")
            || name.is_empty()
            || !name.chars().all(|c| c.is_alphanumeric() || c == '_')
        {
            continue;
        }

        out.push((index as u32, name.to_owned()));
    }

    out
}

#[cfg(test)]
mod imported_accepts {
    use super::{replace_name, type_names};

    /*
    The names a type text asks this file to know. A field of something
    and a name a dot follows are already spoken for.
    */
    #[test]
    fn only_the_bare_names_are_candidates() {
        assert_eq!(type_names(": Query"), vec!["Query"]);
        assert_eq!(
            type_names(": { first: Query, rest: { Entity } }"),
            vec!["Query", "Entity"]
        );

        // Already qualified, so neither half is a name to look up.
        assert!(type_names(": jecs.Query").is_empty());

        // The words a type text holds that name no module.
        assert!(type_names(": { hp: number, name: string }").is_empty());
        assert!(type_names(": (number) -> boolean").is_empty());
    }

    /// A replacement takes the whole name and leaves a field alone.
    #[test]
    fn a_field_of_the_same_name_survives() {
        assert_eq!(
            replace_name(": Query", "Query", "jecs.Query"),
            ": jecs.Query"
        );
        assert_eq!(
            replace_name(": { q: Query, k: t.Query }", "Query", "jecs.Query"),
            ": { q: jecs.Query, k: t.Query }"
        );

        // A longer name that holds this one is not this one.
        assert_eq!(
            replace_name(": QueryBuilder", "Query", "jecs.Query"),
            ": QueryBuilder"
        );
    }
}
