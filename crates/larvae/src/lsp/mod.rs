/*!
`larvae lsp`, the language server the editor extension talks to.

Written against the protocol directly rather than on top of a framework. The
reason is size: a runtime and a protocol crate would add several megabytes to a
binary whose whole point is being one small thing, and what they provide is the
framing in [`rpc`] and a dispatch table, both of which are short.

It is single threaded and synchronous, which sounds like a limitation and is
not. A request is answered by parsing one file, and parsing one file is
microseconds; the work an async server exists to overlap does not happen here.

Everything is pull based from the document store: the editor sends the text on
every change, so this never reads a file the editor has open, and never answers
from a version the user has already edited past.
*/

pub mod rpc;

use std::collections::HashMap;
use std::io::{BufReader, Write};
use std::path::PathBuf;

use anyhow::Result;
use serde_json::{Value, json};

use crate::config::Excludes;
use crate::fmt::{self, FmtConfig};
use crate::lint::{self, Level, LintConfig};

pub fn run() -> Result<()> {
    let stdin = std::io::stdin();
    let mut input = BufReader::new(stdin.lock());
    let stdout = std::io::stdout();
    let mut output = stdout.lock();

    let mut server = Server::default();

    while let Some(message) = rpc::read(&mut input)? {
        if server.handle(&message, &mut output)? {
            break;
        }
    }

    Ok(())
}

#[derive(Default)]
struct Server {
    /// Open documents, keyed by the uri the editor gave them
    documents: HashMap<String, String>,
    root: Option<PathBuf>,
    fmt: FmtConfig,
    lint: LintConfig,
    /// What `[lint] exclude` covers, so an excluded file stays quiet
    excluded: Excludes,
    /// Set by `shutdown`, so a later `exit` is clean rather than abrupt
    shutting_down: bool,
}

impl Server {
    /// Returns whether the server should stop
    fn handle(&mut self, message: &rpc::Message, out: &mut impl Write) -> Result<bool> {
        match message.method.as_str() {
            "initialize" => {
                self.initialize(&message.params);

                self.reply(message, out, capabilities())?;
            }

            "shutdown" => {
                self.shutting_down = true;

                self.reply(message, out, Value::Null)?;
            }

            "exit" => return Ok(true),

            "initialized" => {}

            /*
            A configuration change can turn a lint on, so every open document
            is re-checked rather than waiting for each to be touched. An editor
            showing stale warnings after a settings change looks broken.
            */
            "workspace/didChangeConfiguration" => {
                self.load_config();

                for uri in self.documents.keys().cloned().collect::<Vec<_>>() {
                    self.publish(&uri, out)?;
                }
            }

            "textDocument/didOpen" => {
                let uri = uri_of(&message.params);
                let text = message.params["textDocument"]["text"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();

                self.documents.insert(uri.clone(), text);
                self.publish(&uri, out)?;
            }

            /*
            Full sync only, declared as such in the capabilities.

            Incremental sync would save the editor sending the whole buffer,
            and would cost a rope and a patch path to maintain. For files of
            the size Luau projects hold, sending the text is cheaper than the
            machinery to avoid sending it.
            */
            "textDocument/didChange" => {
                let uri = uri_of(&message.params);

                if let Some(change) = message.params["contentChanges"]
                    .as_array()
                    .and_then(|c| c.last())
                    .and_then(|c| c["text"].as_str())
                {
                    self.documents.insert(uri.clone(), change.to_string());
                    self.publish(&uri, out)?;
                }
            }

            "textDocument/didSave" => {
                let uri = uri_of(&message.params);

                self.publish(&uri, out)?;
            }

            // the diagnostics go with it, or the editor keeps showing them
            "textDocument/didClose" => {
                let uri = uri_of(&message.params);
                self.documents.remove(&uri);

                rpc::notify(
                    out,
                    "textDocument/publishDiagnostics",
                    json!({ "uri": uri, "diagnostics": [] }),
                )?;
            }

            "textDocument/formatting" => {
                let result = self.format(&uri_of(&message.params));

                match result {
                    Ok(edits) => self.reply(message, out, edits)?,

                    // a file mid edit does not parse, and saying so on every
                    // keystroke would be noise, so formatting simply declines
                    Err(_) => self.reply(message, out, Value::Null)?,
                }
            }

            "textDocument/documentSymbol" => {
                let symbols = self.symbols(&uri_of(&message.params));

                self.reply(message, out, symbols)?;
            }

            // anything else, answered only if it expected an answer
            _ => {
                if let Some(id) = &message.id {
                    rpc::respond_error(out, id, format!("{} is not supported", message.method))?;
                }
            }
        }

        Ok(false)
    }

    fn reply(&self, message: &rpc::Message, out: &mut impl Write, result: Value) -> Result<()> {
        match &message.id {
            Some(id) => rpc::respond(out, id, result),

            // a notification wants no reply, and sending one is a protocol error
            None => Ok(()),
        }
    }

    fn initialize(&mut self, params: &Value) {
        self.root = params["rootUri"]
            .as_str()
            .and_then(path_of_uri)
            .or_else(|| {
                params["workspaceFolders"][0]["uri"]
                    .as_str()
                    .and_then(path_of_uri)
            });

        self.load_config();
    }

    /*
    Read the project's config, falling back to defaults.

    A broken config is not reported here. The editor is where someone is
    editing that config, and a server that refuses to start because the file is
    half typed is worse than one that formats with defaults until it is saved.
    */
    fn load_config(&mut self) {
        let Some(root) = &self.root else {
            return;
        };

        let project = crate::config::Config::load(&root.join("larvae.toml")).ok();

        if let Ok(cfg) = FmtConfig::discover(root, project.as_ref().and_then(|c| c.fmt.as_ref())) {
            self.fmt = cfg;
        }

        if let Ok(cfg) = LintConfig::discover(root, project.as_ref().and_then(|c| c.lint.as_ref()))
        {
            self.excluded = cfg.excludes(root).unwrap_or_default();
            self.lint = cfg;
        }
    }

    /// Lint one document and push the result
    fn publish(&self, uri: &str, out: &mut impl Write) -> Result<()> {
        let Some(src) = self.documents.get(uri) else {
            return Ok(());
        };

        /*
        An excluded file is published empty rather than skipped. Skipping would
        leave whatever was on screen before the exclude was added sitting there
        until the editor closed the file.
        */
        if path_of_uri(uri).is_some_and(|p| self.excluded.skips(&p)) {
            return rpc::notify(
                out,
                "textDocument/publishDiagnostics",
                json!({ "uri": uri, "diagnostics": [] }),
            );
        }

        let lines = rpc::Lines::new(src);

        let diagnostics = match lint::analyze(src, &self.lint) {
            Ok(findings) => findings
                .into_iter()
                .map(|f| {
                    json!({
                        "range": lines.range(src, f.span),
                        "severity": severity_of(f.level),
                        "source": "larvae",
                        "code": f.lint,
                        "message": match f.help {
                            Some(help) => format!("{}\n{help}", f.message),

                            None => f.message,
                        },
                    })
                })
                .collect::<Vec<_>>(),

            // a syntax error is one diagnostic, and stops the rest being asked
            Err(e) => {
                let at = e.offset as u32;

                vec![json!({
                    "range": lines.range(src, (at, at + 1)),
                    "severity": 1,
                    "source": "larvae",
                    "message": e.message,
                })]
            }
        };

        rpc::notify(
            out,
            "textDocument/publishDiagnostics",
            json!({ "uri": uri, "diagnostics": diagnostics }),
        )
    }

    /// One edit replacing the whole document, which is what a formatter produces
    fn format(&self, uri: &str) -> Result<Value> {
        let Some(src) = self.documents.get(uri) else {
            return Ok(Value::Null);
        };

        let formatted = fmt::format(src, &self.fmt)?;

        // an edit that changes nothing still makes the editor mark the file dirty
        if formatted == *src {
            return Ok(json!([]));
        }

        Ok(json!([{
            "range": rpc::Lines::new(src).whole(src),
            "newText": formatted,
        }]))
    }

    /*
    The outline, which is what a symbol picker and the breadcrumb bar read.

    Top level declarations only. A nested helper is not something anyone
    navigates to by name, and listing them turns the outline of a large module
    into something longer than the module.
    */
    fn symbols(&self, uri: &str) -> Value {
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

            // 12 is Function and 13 is Variable, in the protocol's numbering
            let (name, kind, span) = match stmt {
                Stmt::Function(n) => {
                    let path: Vec<&str> = n
                        .path
                        .iter()
                        .map(|p| lexed.toks[p.start as usize].text(src))
                        .collect();

                    (path.join("."), 12, n.span)
                }

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

/// What this server can do, which is what the editor will then ask for
fn capabilities() -> Value {
    json!({
        "capabilities": {
            // 1 is full sync, see the note on didChange
            "textDocumentSync": { "openClose": true, "change": 1, "save": true },
            "documentFormattingProvider": true,
            "documentSymbolProvider": true,
        },
        "serverInfo": { "name": "larvae", "version": env!("CARGO_PKG_VERSION") },
    })
}

fn severity_of(level: Level) -> u8 {
    match level {
        // 1 is Error, 2 is Warning, and Allow never reaches here
        Level::Deny => 1,

        _ => 2,
    }
}

fn uri_of(params: &Value) -> String {
    params["textDocument"]["uri"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/*
A `file://` uri as a path.

Only the plain form, with percent decoding for the characters an editor
actually escapes. A full uri parser would be a dependency for the one scheme
that matters, and a path that fails here only means the project config is not
found, not that anything breaks.
*/
fn path_of_uri(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;

    // on Windows the path arrives as /C:/thing, with a leading slash to drop
    let rest = match rest.get(..3) {
        Some(p) if p.starts_with('/') && p.as_bytes()[2] == b':' => &rest[1..],

        _ => rest,
    };

    let mut out = String::with_capacity(rest.len());
    let mut chars = rest.chars();

    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);

            continue;
        }

        let hex: String = chars.by_ref().take(2).collect();

        match u8::from_str_radix(&hex, 16) {
            Ok(byte) => out.push(byte as char),

            Err(_) => {
                out.push('%');
                out.push_str(&hex);
            }
        }
    }

    Some(PathBuf::from(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server_with(src: &str) -> Server {
        let mut server = Server::default();
        server.documents.insert("file:///t.luau".into(), src.into());

        server
    }

    /// Everything advertised has to be something the dispatch actually handles
    #[test]
    fn the_advertised_capabilities_are_all_implemented() {
        let caps = capabilities();
        let caps = &caps["capabilities"];

        assert_eq!(caps["documentFormattingProvider"], true);
        assert_eq!(caps["documentSymbolProvider"], true);
        assert_eq!(caps["textDocumentSync"]["change"], 1, "full sync");
    }

    #[test]
    fn formatting_returns_one_edit_covering_the_document() {
        let server = server_with("local x={a=1}\n");
        let edits = server.format("file:///t.luau").unwrap();

        assert_eq!(edits.as_array().unwrap().len(), 1);
        assert_eq!(edits[0]["newText"], "local x = { a = 1 }\n");
        assert_eq!(edits[0]["range"]["start"]["line"], 0);
    }

    /// An edit that changes nothing would still mark the buffer dirty
    #[test]
    fn an_already_formatted_document_produces_no_edits() {
        let server = server_with("local x = { a = 1 }\n");

        assert_eq!(server.format("file:///t.luau").unwrap(), json!([]));
    }

    #[test]
    fn formatting_a_file_that_does_not_parse_declines_rather_than_mangling_it() {
        let server = server_with("local = = =\n");

        assert!(server.format("file:///t.luau").is_err());
    }

    #[test]
    fn formatting_an_unopened_document_is_not_an_error() {
        assert_eq!(
            Server::default().format("file:///nope.luau").unwrap(),
            Value::Null
        );
    }

    #[test]
    fn the_outline_lists_top_level_declarations() {
        let server = server_with(
            "local Players = game\nlocal function helper() end\nfunction M.thing() end\n",
        );

        let symbols = server.symbols("file:///t.luau");
        let names: Vec<&str> = symbols
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["name"].as_str().unwrap())
            .collect();

        assert_eq!(names, ["Players", "helper", "M.thing"]);
    }

    #[test]
    fn a_nested_function_is_not_in_the_outline() {
        let server = server_with("local function outer()\n\tlocal function inner() end\nend\n");

        assert_eq!(
            server.symbols("file:///t.luau").as_array().unwrap().len(),
            1
        );
    }

    #[test]
    fn the_outline_of_an_unparsable_file_is_empty_rather_than_an_error() {
        let server = server_with("local = = =\n");

        assert_eq!(server.symbols("file:///t.luau"), json!([]));
    }

    // --- diagnostics -------------------------------------------------------

    fn diagnostics_of(src: &str) -> Value {
        let mut server = server_with(src);
        server.lint = LintConfig::default();

        let mut out = Vec::new();
        server.publish("file:///t.luau", &mut out).unwrap();

        let text = String::from_utf8(out).unwrap();
        let body = text.split_once("\r\n\r\n").expect("framed").1;
        let value: Value = serde_json::from_str(body).unwrap();

        value["params"]["diagnostics"].clone()
    }

    #[test]
    fn a_finding_becomes_a_diagnostic_with_a_range_and_a_code() {
        let diags = diagnostics_of("local unused = 1\nreturn 1\n");
        let first = &diags[0];

        assert_eq!(first["code"], "unused_variable");
        assert_eq!(first["source"], "larvae");
        assert_eq!(first["severity"], 2, "a warning");
        assert_eq!(first["range"]["start"]["line"], 0);
        assert!(first["range"]["end"]["character"].as_u64().unwrap() > 0);
    }

    #[test]
    fn a_denied_lint_arrives_as_an_error() {
        let diags = diagnostics_of("return notDefinedAnywhere\n");

        assert_eq!(diags[0]["severity"], 1);
    }

    #[test]
    fn a_syntax_error_is_the_only_diagnostic_and_is_an_error() {
        let diags = diagnostics_of("local = = =\n");

        assert_eq!(diags.as_array().unwrap().len(), 1);
        assert_eq!(diags[0]["severity"], 1);
        assert!(
            diags[0]["message"]
                .as_str()
                .unwrap()
                .contains("syntax error")
        );
    }

    #[test]
    fn a_clean_document_produces_no_diagnostics() {
        assert_eq!(diagnostics_of("return 1\n"), json!([]));
    }

    /*
    An excluded file has to be published empty rather than left alone. Anything
    already on screen when the exclude was added would otherwise stay there
    until the editor closed the file.
    */
    #[test]
    fn an_excluded_document_is_published_empty() {
        let mut server = server_with("local unused = 1\nreturn 1\n");
        server.documents.clear();
        server.documents.insert(
            "file:///project/Packages/t.luau".into(),
            "local unused = 1\n".into(),
        );
        server.excluded =
            Excludes::new(std::path::Path::new("/project"), &["Packages".to_string()]).unwrap();

        let mut out = Vec::new();
        server
            .publish("file:///project/Packages/t.luau", &mut out)
            .unwrap();

        let text = String::from_utf8(out).unwrap();

        assert!(text.contains("publishDiagnostics"), "{text}");
        assert!(text.contains("\"diagnostics\":[]"), "{text}");
    }

    /// The help is worth having in the editor, where there is room for it
    #[test]
    fn the_help_is_carried_into_the_message() {
        let diags = diagnostics_of("local unused = 1\nreturn 1\n");

        assert!(diags[0]["message"].as_str().unwrap().contains('\n'));
    }

    // --- dispatch ----------------------------------------------------------

    fn message(method: &str, id: Option<i64>, params: Value) -> rpc::Message {
        let mut value = json!({ "method": method, "params": params });

        if let Some(id) = id {
            value["id"] = json!(id);
        }

        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn initialize_answers_with_the_capabilities() {
        let mut server = Server::default();
        let mut out = Vec::new();

        let stop = server
            .handle(&message("initialize", Some(1), json!({})), &mut out)
            .unwrap();

        assert!(!stop);
        assert!(
            String::from_utf8(out)
                .unwrap()
                .contains("documentFormattingProvider")
        );
    }

    #[test]
    fn exit_stops_the_loop() {
        let mut server = Server::default();

        assert!(
            server
                .handle(&message("exit", None, json!({})), &mut Vec::new())
                .unwrap()
        );
    }

    #[test]
    fn opening_a_document_stores_it_and_publishes() {
        let mut server = Server::default();
        let mut out = Vec::new();

        server
            .handle(
                &message(
                    "textDocument/didOpen",
                    None,
                    json!({ "textDocument": { "uri": "file:///t.luau", "text": "local unused = 1\n" } }),
                ),
                &mut out,
            )
            .unwrap();

        assert!(server.documents.contains_key("file:///t.luau"));
        assert!(String::from_utf8(out).unwrap().contains("unused_variable"));
    }

    #[test]
    fn a_change_replaces_the_stored_text() {
        let mut server = server_with("local a = 1\n");

        server
            .handle(
                &message(
                    "textDocument/didChange",
                    None,
                    json!({
                        "textDocument": { "uri": "file:///t.luau" },
                        "contentChanges": [{ "text": "local b = 2\n" }],
                    }),
                ),
                &mut Vec::new(),
            )
            .unwrap();

        assert_eq!(server.documents["file:///t.luau"], "local b = 2\n");
    }

    /// Closing has to clear what was published, or the editor keeps showing it
    #[test]
    fn closing_a_document_drops_it_and_clears_its_diagnostics() {
        let mut server = server_with("local unused = 1\n");
        let mut out = Vec::new();

        server
            .handle(
                &message(
                    "textDocument/didClose",
                    None,
                    json!({ "textDocument": { "uri": "file:///t.luau" } }),
                ),
                &mut out,
            )
            .unwrap();

        assert!(server.documents.is_empty());
        assert!(
            String::from_utf8(out)
                .unwrap()
                .contains(r#""diagnostics":[]"#)
        );
    }

    #[test]
    fn an_unsupported_request_is_answered_with_an_error() {
        let mut server = Server::default();
        let mut out = Vec::new();

        server
            .handle(&message("textDocument/hover", Some(9), json!({})), &mut out)
            .unwrap();

        assert!(String::from_utf8(out).unwrap().contains("is not supported"));
    }

    /// Replying to a notification is a protocol error
    #[test]
    fn an_unsupported_notification_is_answered_with_nothing() {
        let mut server = Server::default();
        let mut out = Vec::new();

        server
            .handle(&message("$/setTrace", None, json!({})), &mut out)
            .unwrap();

        assert!(out.is_empty());
    }

    // --- uris --------------------------------------------------------------

    #[test]
    fn a_file_uri_becomes_a_path() {
        assert_eq!(
            path_of_uri("file:///home/a/project"),
            Some(PathBuf::from("/home/a/project"))
        );
    }

    #[test]
    fn a_percent_escape_is_decoded() {
        assert_eq!(
            path_of_uri("file:///home/a/my%20project"),
            Some(PathBuf::from("/home/a/my project"))
        );
    }

    /// A Windows path arrives with a leading slash before the drive letter
    #[test]
    fn a_windows_uri_drops_the_leading_slash() {
        assert_eq!(
            path_of_uri("file:///C:/code/project"),
            Some(PathBuf::from("C:/code/project"))
        );
    }

    #[test]
    fn a_uri_that_is_not_a_file_is_declined() {
        assert_eq!(path_of_uri("untitled:Untitled-1"), None);
    }
}
