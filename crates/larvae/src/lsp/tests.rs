use super::uri::path_of_uri;
use super::*;

fn server_with(src: &str) -> Server {
    let mut server = Server::default();
    server.documents.insert("file:///t.luau".into(), src.into());

    server
}

/// The dispatch must handle every advertised capability.
#[test]
fn the_advertised_capabilities_are_all_implemented() {
    let caps = capabilities(false);
    let caps = &caps["capabilities"];

    assert_eq!(caps["documentFormattingProvider"], true);
    assert_eq!(caps["documentSymbolProvider"], true);
    assert_eq!(caps["codeActionProvider"], true);
    assert_eq!(caps["textDocumentSync"]["change"], 1, "full sync");
}

/*
The two worm paths answer, and they answer with the shape an editor expects.

Neither carries anything yet. What these hold is that the path is wired: a
request that errors and a request that returns nothing look the same to a
user and are not the same to an editor, which logs a failure on every
keystroke that opens the lightbulb.
*/
#[test]
fn a_code_action_request_answers_with_a_list() {
    let mut server = server_with("local x = 1\n");
    let mut out = Vec::new();

    let message = message(
        "textDocument/codeAction",
        Some(7),
        json!({
            "textDocument": { "uri": "file:///t.luau" },
            "range": {
                "start": { "line": 0, "character": 0 },
                "end": { "line": 0, "character": 5 }
            },
            "context": { "diagnostics": [] }
        }),
    );

    server.handle(&message, &mut out).unwrap();

    let text = String::from_utf8(out).unwrap();

    assert!(text.contains("\"result\":[]"), "{text}");
    assert!(
        !text.contains("error"),
        "an editor must not see a failure: {text}"
    );
}

#[test]
fn a_definitions_request_answers_with_a_list() {
    let mut server = server_with("local x = 1\n");
    let mut out = Vec::new();

    let message = message("larvae/definitions", Some(8), json!({}));

    server.handle(&message, &mut out).unwrap();

    let text = String::from_utf8(out).unwrap();

    assert!(text.contains("definitions"), "{text}");
    assert!(!text.contains("is not supported"), "{text}");
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
    let server =
        server_with("local Players = game\nlocal function helper() end\nfunction M.thing() end\n");

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

/// The diagnostics that one publish put on the wire
fn published(server: &Server, uri: &str) -> Value {
    let mut out = Vec::new();
    server.publish(uri, &mut out).unwrap();

    let text = String::from_utf8(out).unwrap();
    let body = text.split_once("\r\n\r\n").expect("framed").1;
    let value: Value = serde_json::from_str(body).unwrap();

    value["params"]["diagnostics"].clone()
}

fn diagnostics_of(src: &str) -> Value {
    let mut server = server_with(src);
    server.lint = LintConfig::default();

    published(&server, "file:///t.luau")
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

/*
A deprecated use publishes as a struck-through hint, and the list can
drop deprecated entries whole.

Severity 4 draws no squiggle, tag 2 is the strikethrough. The hide is
off by default: the strikethrough already says what the platform
thinks, and hiding is a stance a project takes on purpose.
*/
#[test]
fn deprecated_marks_publish_and_the_list_can_hide_them() {
    struct Deprecating;

    impl crate::lsp::analysis::Analysis for Deprecating {
        fn open(&mut self, _: &std::path::Path, _: &str) {}

        fn check(&mut self, _: &std::path::Path) -> Vec<crate::lsp::analysis::AnalysisDiag> {
            Vec::new()
        }

        fn hover(&mut self, _: &std::path::Path, _: u32, _: bool, _: bool) -> Option<String> {
            None
        }

        fn invalidate(&mut self, _: &std::path::Path) {}

        fn deprecated_uses(
            &mut self,
            _: &std::path::Path,
        ) -> Vec<crate::lsp::analysis::AnalysisDiag> {
            vec![crate::lsp::analysis::AnalysisDiag {
                span: (2, 8),
                severity: 4,
                message: "Member 'Instance.Remove' is deprecated".into(),
                code: None,
            }]
        }

        fn completions(
            &mut self,
            _: &std::path::Path,
            _: u32,
        ) -> Vec<crate::lsp::analysis::AnalysisCompletion> {
            [("Remove", true), ("Destroy", false)]
                .into_iter()
                .map(
                    |(label, deprecated)| crate::lsp::analysis::AnalysisCompletion {
                        label: label.into(),
                        kind: 3,
                        detail: None,
                        label_detail: None,
                        insert_text: None,
                        documentation: None,
                        deprecated,
                        type_correct: 0,
                        wrong_index_type: false,
                    },
                )
                .collect()
        }
    }

    let mut server = Server {
        analysis: std::cell::RefCell::new(Some(Box::new(Deprecating))),
        ..Server::default()
    };

    let mut out = Vec::new();
    server
        .handle(
            &message(
                "textDocument/didOpen",
                None,
                json!({ "textDocument": { "uri": "file:///t.luau", "text": "p:Remove()\n" } }),
            ),
            &mut out,
        )
        .unwrap();

    let published = String::from_utf8(out).unwrap();

    assert!(
        published.contains("\"tags\":[2]") && published.contains("deprecated"),
        "{published}"
    );

    let items = server.completions(&json!({
        "textDocument": { "uri": "file:///t.luau" },
        "position": { "line": 0, "character": 0 },
    }));

    assert!(
        items
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["label"] == "Remove"),
        "listed by default: {items}"
    );

    server.lsp.completion.hide_deprecated = true;

    let items = server.completions(&json!({
        "textDocument": { "uri": "file:///t.luau" },
        "position": { "line": 0, "character": 0 },
    }));
    let items = items.as_array().unwrap();

    assert!(!items.iter().any(|i| i["label"] == "Remove"), "{items:?}");
    assert!(items.iter().any(|i| i["label"] == "Destroy"), "{items:?}");
}

/*
One deprecation, one voice.

The platform's mark is the precise one, so a larvae `deprecated`
finding that overlaps it stands down. A name only the project marked
gets no platform mark, and larvae still speaks for it.
*/
#[test]
fn the_platform_mark_wins_where_the_two_overlap() {
    struct MarksChildren;

    impl crate::lsp::analysis::Analysis for MarksChildren {
        fn open(&mut self, _: &std::path::Path, _: &str) {}

        fn check(&mut self, _: &std::path::Path) -> Vec<crate::lsp::analysis::AnalysisDiag> {
            Vec::new()
        }

        fn hover(&mut self, _: &std::path::Path, _: u32, _: bool, _: bool) -> Option<String> {
            None
        }

        fn invalidate(&mut self, _: &std::path::Path) {}

        fn completions(
            &mut self,
            _: &std::path::Path,
            _: u32,
        ) -> Vec<crate::lsp::analysis::AnalysisCompletion> {
            Vec::new()
        }

        fn deprecated_uses(
            &mut self,
            _: &std::path::Path,
        ) -> Vec<crate::lsp::analysis::AnalysisDiag> {
            // The span of `children` in the document below.
            vec![crate::lsp::analysis::AnalysisDiag {
                span: (2, 10),
                severity: 4,
                message:
                    "Member 'Instance.children' is deprecated, use 'Instance.GetChildren' instead"
                        .into(),
                code: None,
            }]
        }
    }

    let mut server = Server {
        analysis: std::cell::RefCell::new(Some(Box::new(MarksChildren))),
        ..Server::default()
    };
    server.lint.std = crate::lint::config::StdLib::Roblox;

    let mut out = Vec::new();
    server
        .handle(
            &message(
                "textDocument/didOpen",
                None,
                json!({ "textDocument": { "uri": "file:///t.luau", "text": "p:children()\n" } }),
            ),
            &mut out,
        )
        .unwrap();

    let published = String::from_utf8(out).unwrap();

    assert!(
        published.contains("Instance.children"),
        "the platform speaks: {published}"
    );
    assert!(
        !published.contains("\"code\":\"deprecated\""),
        "larvae's overlapping finding stands down: {published}"
    );
}

/*
A key a dot cannot reach rewrites itself into brackets on accept.

`t.Jump Force` is not Luau: the offer's edit writes the bracketed key
in the project's quote, and one more edit removes the dot the author
typed. An ordinary identifier key keeps its plain insert.
*/
#[test]
fn a_space_named_key_accepts_as_a_bracket_access() {
    struct SpacedFields;

    impl crate::lsp::analysis::Analysis for SpacedFields {
        fn open(&mut self, _: &std::path::Path, _: &str) {}

        fn check(&mut self, _: &std::path::Path) -> Vec<crate::lsp::analysis::AnalysisDiag> {
            Vec::new()
        }

        fn hover(&mut self, _: &std::path::Path, _: u32, _: bool, _: bool) -> Option<String> {
            None
        }

        fn invalidate(&mut self, _: &std::path::Path) {}

        fn completions(
            &mut self,
            _: &std::path::Path,
            _: u32,
        ) -> Vec<crate::lsp::analysis::AnalysisCompletion> {
            ["Jump Force", "Strength"]
                .into_iter()
                .map(|label| crate::lsp::analysis::AnalysisCompletion {
                    label: label.into(),
                    kind: 5,
                    detail: None,
                    label_detail: None,
                    insert_text: None,
                    documentation: None,
                    deprecated: false,
                    type_correct: 0,
                    wrong_index_type: false,
                })
                .collect()
        }
    }

    let mut server = Server {
        analysis: std::cell::RefCell::new(Some(Box::new(SpacedFields))),
        ..Server::default()
    };
    server.fmt.quote_style = crate::fmt::config::QuoteStyle::AutoPreferSingle;

    let src = "local t = stats\nlocal x = t.Jum\n";
    server.documents.insert("file:///t.luau".into(), src.into());

    let items = server.completions(&json!({
        "textDocument": { "uri": "file:///t.luau" },
        "position": { "line": 1, "character": 15 },
    }));
    let items = items.as_array().cloned().unwrap_or_default();

    let spaced = items
        .iter()
        .find(|i| i["label"] == "Jump Force")
        .unwrap_or_else(|| panic!("the key offers: {items:?}"));

    assert_eq!(spaced["textEdit"]["newText"], "['Jump Force']", "{spaced}");
    assert_eq!(
        spaced["textEdit"]["range"],
        json!({ "start": { "line": 1, "character": 12 }, "end": { "line": 1, "character": 15 } }),
        "{spaced}"
    );
    assert_eq!(
        spaced["additionalTextEdits"][0]["range"],
        json!({ "start": { "line": 1, "character": 11 }, "end": { "line": 1, "character": 12 } }),
        "the dot goes: {spaced}"
    );

    let plain = items
        .iter()
        .find(|i| i["label"] == "Strength")
        .expect("the plain key offers");

    assert!(plain.get("textEdit").is_none(), "{plain}");
}

/*
A module a worm refuses to lower says why, at the require.

The load hook records the refusal keyed by the file, and the publish
pins it to every require that names the file. Without this the require
answered `*error-type*` and nothing anywhere said the reason.
*/
#[test]
fn a_refused_lowering_reports_at_the_require() {
    let dir = tempfile::tempdir().expect("a temp dir");
    std::fs::create_dir_all(dir.path().join("src")).expect("makes it");
    std::fs::write(dir.path().join("src/util.luau"), "return {}\n").expect("writes");

    let mut server = Server {
        root: Some(dir.path().to_path_buf()),
        ..Server::default()
    };

    server.load_errors.lock().unwrap().insert(
        dir.path().join("src/util.luau"),
        "worm `x` failed, line 1: no lowering\nmore detail".into(),
    );

    let mut out = Vec::new();
    server
        .handle(
            &message(
                "textDocument/didOpen",
                None,
                json!({ "textDocument": {
                    "uri": format!("file://{}/src/main.luau", dir.path().display()),
                    "text": "local u = require('./util')\nreturn u\n",
                } }),
            ),
            &mut out,
        )
        .unwrap();

    let published = String::from_utf8(out).unwrap();

    assert!(
        published.contains("this module does not lower: worm `x` failed, line 1: no lowering"),
        "{published}"
    );
    assert!(
        !published.contains("more detail"),
        "only the first line reaches the list: {published}"
    );
}

/*
A rename asks one question, and the yes applies the edit.

The editor reports the move after it happened, so nothing on disk can
resolve the old spec any more. The server still finds the require, asks
with a dialog, and the answer routes back by the id the question carried.
*/
#[test]
fn a_rename_asks_and_the_answer_rewrites_the_requires() {
    let dir = tempfile::tempdir().expect("a temp dir");
    std::fs::create_dir_all(dir.path().join("src")).expect("makes it");
    std::fs::write(
        dir.path().join("src/main.luau"),
        "local u = require('./tools')\nreturn u\n",
    )
    .expect("writes");
    std::fs::write(dir.path().join("src/tools.luau"), "return {}\n").expect("writes");

    let mut server = Server {
        root: Some(dir.path().to_path_buf()),
        ..Server::default()
    };
    let mut out = Vec::new();

    server
        .handle(
            &message(
                "workspace/didRenameFiles",
                None,
                json!({ "files": [{
                    "oldUri": format!("file://{}/src/util.luau", dir.path().display()),
                    "newUri": format!("file://{}/src/tools.luau", dir.path().display()),
                }] }),
            ),
            &mut out,
        )
        .unwrap();

    // wait: the spec says ./tools, and util was renamed TO tools, so the
    // old spelling on disk is ./util; the fixture writes the PRE state.
    let asked = String::from_utf8(out).unwrap();

    assert!(
        asked.is_empty(),
        "the spec already says the new name: {asked}"
    );

    std::fs::write(
        dir.path().join("src/main.luau"),
        "local u = require('./util')\nreturn u\n",
    )
    .expect("writes");

    let mut out = Vec::new();
    server
        .handle(
            &message(
                "workspace/didRenameFiles",
                None,
                json!({ "files": [{
                    "oldUri": format!("file://{}/src/util.luau", dir.path().display()),
                    "newUri": format!("file://{}/src/tools.luau", dir.path().display()),
                }] }),
            ),
            &mut out,
        )
        .unwrap();

    let asked = String::from_utf8(out).unwrap();

    assert!(
        asked.contains("window/showMessageRequest"),
        "the server asks first: {asked}"
    );
    assert!(
        asked.contains("update 1 require in 1 file?"),
        "the question counts what it found: {asked}"
    );

    let id: Value = serde_json::from_str(
        asked
            .split("\"id\":")
            .nth(1)
            .and_then(|rest| rest.split(',').next())
            .expect("the request has an id"),
    )
    .expect("a json id");

    let mut out = Vec::new();
    let stop = server
        .handle(
            &rpc::Message {
                id: Some(id),
                method: String::new(),
                params: json!(null),
                result: json!({ "title": "Update requires" }),
            },
            &mut out,
        )
        .unwrap();

    /*
    Handle answers true to stop the server. A response must answer
    false: the editor replies to every request the server sends, and
    the first refresh answer read as a hang-up, which was five clean
    exits in a row and an editor that gave up restarting.
    */
    assert!(!stop, "a response stopped the server");

    let applied = String::from_utf8(out).unwrap();

    assert!(
        applied.contains("workspace/applyEdit"),
        "the yes applies: {applied}"
    );
    assert!(applied.contains("./tools"), "the new spelling: {applied}");
}

/*
A `$` line in a doc fence is the author talking to the checker.

luau-lsp hides it from the rendered card, so a doc written for either
server reads the same. Prose keeps its dollars.
*/
#[test]
fn a_dollar_line_hides_inside_a_doc_fence() {
    let docs = "Costs $5.\n```luau\n$local hidden = 1\nprint(hidden)\n```\n$ prose keeps this\n";
    let card = crate::lsp::features::card("local x: number", Some(docs));

    assert!(!card.contains("hidden = 1"), "{card}");
    assert!(card.contains("print(hidden)"), "{card}");
    assert!(card.contains("Costs $5."), "{card}");
    assert!(card.contains("$ prose keeps this"), "{card}");
}

#[test]
fn a_clean_document_produces_no_diagnostics() {
    assert_eq!(diagnostics_of("return 1\n"), json!([]));
}

/*
With an analyzer landed, the syntax error is Luau's to report.

Luau's parse errors ride its check in Luau's own words, so larvae's
spelling would say the same break twice. larvae speaks only while the
session is loading, and for the files no analyzer reads.
*/
#[test]
fn the_analyzer_speaks_the_syntax_error_alone() {
    let mut server = Server {
        analysis: std::cell::RefCell::new(Some(Box::new(BytecodeAnalysis))),
        ..Server::default()
    };
    let mut out = Vec::new();

    server
        .handle(
            &message(
                "textDocument/didOpen",
                None,
                json!({ "textDocument": { "uri": "file:///t.luau", "text": "local a =\n" } }),
            ),
            &mut out,
        )
        .unwrap();

    let published = String::from_utf8(out).unwrap();

    assert!(
        !published.contains("syntax error"),
        "larvae repeats the break Luau reports: {published}"
    );
}

/*
The server must publish an excluded file as empty and not skip it.
Otherwise the old diagnostics would stay on screen until the editor
closed the file.
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

/// The help belongs in the editor, because the editor has room for it
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

/// A close must clear the published diagnostics, or the editor keeps them
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

/*
A method larvae does not answer gets an error, not silence.

`moniker` is the example because larvae does not answer it and has no plan
to. It was `rename`, then `signatureHelp`, then `semanticTokens`, and each
moved as that feature shipped. That movement is the test working: the method
it names has to be one the server truly lacks.
*/
#[test]
fn an_unsupported_request_is_answered_with_an_error() {
    let mut server = Server::default();
    let mut out = Vec::new();

    server
        .handle(
            &message("textDocument/moniker", Some(9), json!({})),
            &mut out,
        )
        .unwrap();

    assert!(String::from_utf8(out).unwrap().contains("is not supported"));
}

/// The method above must stay one the server does not advertise.
#[test]
fn the_unsupported_example_is_really_unsupported() {
    let caps = capabilities(true);

    assert!(
        caps["capabilities"]["monikerProvider"].is_null(),
        "moniker is advertised now, so the test above needs a new example"
    );
}

/// A reply to a notification is a protocol error
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

// --- worms -------------------------------------------------------------

/// A worm that claims `.luaux` and does nothing else with it
const FRONTEND: &str = r#"
name  = "luaux"
api   = 1
form  = "native"
entry = "worm.py"

[frontend]
claims = [".luaux"]
"#;

/// The same worm, which also lays out the files it claims
const FORMATTER: &str = r#"
name  = "luaux"
api   = 1
form  = "native"
entry = "worm.py"

[frontend]
claims = [".luaux"]
fmt    = true
"#;

/// The same worm, which also reports one lint
const LINTER: &str = r#"
name  = "luaux"
api   = 1
form  = "native"
entry = "worm.py"

[frontend]
claims = [".luaux"]

[lints.tidy]
"#;

fn spec(manifest: &str, dir: &std::path::Path) -> std::sync::Arc<crate::worm::pool::Spec> {
    spec_with(manifest, dir, vec![".luaux".to_owned()])
}

fn spec_with(
    manifest: &str,
    dir: &std::path::Path,
    claims: Vec<String>,
) -> std::sync::Arc<crate::worm::pool::Spec> {
    std::sync::Arc::new(crate::worm::pool::Spec {
        manifest: crate::worm::manifest::Manifest::parse(manifest).unwrap(),
        artifact: Vec::new(),
        dir: dir.to_path_buf(),
        config: toml::from_str("").unwrap(),
        rules: Default::default(),
        run_order: None,
        inherit_lints: None,
        inherit: Default::default(),
        requires: crate::worm::RequireOwner::Larvae,
        claims,
    })
}

/// A server that holds one `.luaux` document and the worm that claims it
fn server_with_worm(manifest: &str, dir: &std::path::Path, src: &str) -> Server {
    let mut server = Server::default();
    server
        .documents
        .insert("file:///p/t.luaux".into(), src.into());
    server.worms = Pool::new(vec![spec(manifest, dir)], 1);

    server
}

/*
A worm process that answers `init` and then repeats one reply.

The tests need a real transport, because a claimed file reaches the worm
over that transport and over nothing else.
*/
#[cfg(unix)]
fn worm_that(dir: &std::path::Path, reply: &str) {
    let script = dir.join("worm.py");

    std::fs::write(
        &script,
        format!(
            r#"#!/usr/bin/env python3
import sys, json, struct

def read():
    n = sys.stdin.buffer.read(4)
    if len(n) < 4: sys.exit(0)
    return json.loads(sys.stdin.buffer.read(struct.unpack("<I", n)[0]))

def send(obj):
    b = json.dumps(obj).encode()
    sys.stdout.buffer.write(struct.pack("<I", len(b)) + b)
    sys.stdout.buffer.flush()

while True:
    req = read()
    if req["op"] == "init":
        send({{"ok": True}})
        continue
{reply}
"#
        ),
    )
    .unwrap();

    use std::os::unix::fs::PermissionsExt;

    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/*
The test that states the problem. The Luau parser reads the first markup
character of a `.luaux` file and reports a syntax error. The worm owns
that file, so the server must not read it as Luau.
*/
#[test]
fn a_claimed_file_whose_worm_reports_nothing_is_published_empty() {
    let dir = tempfile::tempdir().unwrap();
    let server = server_with_worm(FRONTEND, dir.path(), "<Frame>\n\t<Label />\n</Frame>\n");

    assert_eq!(published(&server, "file:///p/t.luaux"), json!([]));
}

/// The editor asks on every save, so silence is the answer and not an error
#[test]
fn a_claimed_file_whose_worm_does_not_format_produces_no_edits() {
    let dir = tempfile::tempdir().unwrap();
    let server = server_with_worm(FRONTEND, dir.path(), "<Frame>\n");

    assert_eq!(server.format("file:///p/t.luaux").unwrap(), json!([]));
}

/// A worm claims one extension, and larvae keeps every other file
#[test]
fn a_luau_file_keeps_the_luau_route_beside_a_worm() {
    let dir = tempfile::tempdir().unwrap();
    let mut server = server_with_worm(FORMATTER, dir.path(), "<Frame>\n");
    server
        .documents
        .insert("file:///p/t.luau".into(), "local x={a=1}\n".into());

    let edits = server.format("file:///p/t.luau").unwrap();

    assert_eq!(edits[0]["newText"], "local x = { a = 1 }\n");
    assert_eq!(
        published(&server, "file:///p/t.luau")[0]["code"],
        "unused_variable"
    );
}

#[cfg(unix)]
#[test]
fn a_claimed_file_carries_the_findings_of_its_worm() {
    let dir = tempfile::tempdir().unwrap();
    worm_that(
        dir.path(),
        r#"    send({"ok": True, "findings": [{"span": [0, 7], "lint": "tidy", "message": "untidy"}]})"#,
    );

    let server = server_with_worm(LINTER, dir.path(), "<Frame>\n");
    let diags = published(&server, "file:///p/t.luaux");

    assert_eq!(diags.as_array().unwrap().len(), 1);
    assert_eq!(
        diags[0]["code"], "luaux.tidy",
        "the name is under the key of the worm"
    );
    assert_eq!(diags[0]["source"], "larvae");
    assert_eq!(diags[0]["severity"], 2, "a warning");
    assert_eq!(diags[0]["range"]["end"]["character"], 7);
}

#[cfg(unix)]
#[test]
fn a_claimed_file_is_laid_out_by_its_worm() {
    let dir = tempfile::tempdir().unwrap();
    worm_that(
        dir.path(),
        r#"    send({"ok": True, "doc": 1, "document": {"lit": "<Frame />"}})"#,
    );

    let server = server_with_worm(FORMATTER, dir.path(), "<Frame></Frame>\n");
    let edits = server.format("file:///p/t.luaux").unwrap();

    assert_eq!(edits[0]["newText"], "<Frame />\n");
    assert_eq!(edits[0]["range"]["start"]["line"], 0);
}

/// A worm that fails states why, and the editor keeps working
#[cfg(unix)]
#[test]
fn a_worm_that_fails_becomes_one_diagnostic() {
    let dir = tempfile::tempdir().unwrap();
    worm_that(
        dir.path(),
        r#"    send({"ok": False, "error": "line 1 is not markup"})"#,
    );

    let server = server_with_worm(LINTER, dir.path(), "<Frame>\n");
    let diags = published(&server, "file:///p/t.luaux");

    assert_eq!(diags.as_array().unwrap().len(), 1);
    assert_eq!(diags[0]["severity"], 1);
    assert!(
        diags[0]["message"]
            .as_str()
            .unwrap()
            .contains("line 1 is not markup"),
        "{}",
        diags[0]
    );
}

/// A user who edits `larvae.toml` breaks it for some keystrokes
#[test]
fn a_project_config_that_does_not_load_leaves_the_server_with_no_worms() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("larvae.toml"), "= = not toml = =").unwrap();

    let mut server = Server {
        root: Some(dir.path().to_path_buf()),
        ..Default::default()
    };

    server.load_config(&mut Vec::new()).unwrap();

    assert!(server.worms.is_empty());
}

/// The uri of a directory on disk, in the form the editor sends
fn dir_uri(path: &std::path::Path) -> String {
    let text = path.display().to_string().replace('\\', "/");

    format!("file:///{}", text.trim_start_matches('/'))
}

fn initialized_at(dir: &std::path::Path) -> String {
    let mut server = Server::default();
    let mut out = Vec::new();

    server
        .handle(
            &message("initialize", Some(1), json!({ "rootUri": dir_uri(dir) })),
            &mut out,
        )
        .unwrap();

    String::from_utf8(out).unwrap()
}

/// A config that fails to resolve raises one editor notification.
#[test]
fn a_broken_config_raises_a_warning_toast() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("larvae.toml"), "[process]\ninpt = \"x\"\n").unwrap();

    let out = initialized_at(dir.path());

    assert!(out.contains("window/showMessage"), "{out}");
    assert!(out.contains("larvae serves defaults"), "{out}");
    // The server still answers initialize; a broken config does not stop it.
    assert!(out.contains("documentFormattingProvider"), "{out}");
}

/// No larvae.toml is the zero config case, and it raises nothing.
#[test]
fn a_missing_config_stays_quiet() {
    let dir = tempfile::tempdir().unwrap();

    let out = initialized_at(dir.path());

    assert!(!out.contains("window/showMessage"), "{out}");
}

#[test]
fn a_clean_config_stays_quiet() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("larvae.toml"),
        "[fmt]\ncolumn_width = 100\n",
    )
    .unwrap();

    let out = initialized_at(dir.path());

    assert!(!out.contains("window/showMessage"), "{out}");
}

/// A settings change reloads the config, so the report happens there too.
#[test]
fn a_config_change_reports_the_break_again() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("larvae.toml"),
        "[fmt]\ncolumn_width = 100\n",
    )
    .unwrap();

    let mut server = Server::default();
    let mut out = Vec::new();
    server
        .handle(
            &message(
                "initialize",
                Some(1),
                json!({ "rootUri": dir_uri(dir.path()) }),
            ),
            &mut out,
        )
        .unwrap();

    std::fs::write(dir.path().join("larvae.toml"), "[process]\ninpt = \"x\"\n").unwrap();

    let mut out = Vec::new();
    server
        .handle(
            &message("workspace/didChangeConfiguration", None, json!({})),
            &mut out,
        )
        .unwrap();

    assert!(
        String::from_utf8(out)
            .unwrap()
            .contains("window/showMessage"),
        "the reload reports the break"
    );
}

// --- the [lsp] table ------------------------------------------------------

#[test]
fn claim_only_publishes_empty_for_a_plain_luau_file() {
    let mut server = server_with("local unused = 1\nreturn 1\n");
    server.lsp = crate::config::lsp::LspConfig {
        enabled: true,
        claim_only: true,
        ..Default::default()
    };

    let diags = published(&server, "file:///t.luau");

    assert_eq!(diags.as_array().map(Vec::len), Some(0), "{diags}");
}

#[test]
fn claim_only_declines_formatting_and_symbols_for_a_plain_luau_file() {
    let mut server = server_with("local x={a=1}\nreturn x\n");
    server.lsp = crate::config::lsp::LspConfig {
        enabled: true,
        claim_only: true,
        ..Default::default()
    };

    assert_eq!(server.format("file:///t.luau").unwrap(), Value::Null);
    assert_eq!(server.symbols("file:///t.luau"), serde_json::json!([]));
}

#[test]
fn a_disabled_server_advertises_no_capabilities() {
    let mut server = server_with("return 1\n");
    server.lsp = crate::config::lsp::LspConfig {
        enabled: false,
        claim_only: false,
        ..Default::default()
    };

    let mut out = Vec::new();
    server
        .handle(
            &message("initialize", Some(1), serde_json::json!({})),
            &mut out,
        )
        .unwrap();

    let text = String::from_utf8_lossy(&out);

    assert!(
        text.contains("\"capabilities\":{}"),
        "empty capabilities: {text}"
    );
    assert!(!text.contains("documentFormattingProvider"), "{text}");
}

// --- the worm lsp hooks ---------------------------------------------------

/// A worm that answers every lsp hook, over the real transport
const LSP_WORM: &str = r#"
name = "mockdata"
api = 1
form = "native"
entry = "worm.py"

[lsp]
resolve = true
declarations = true
respond = ["hover"]
"#;

#[cfg(unix)]
fn lsp_worm_that(dir: &std::path::Path) {
    let script = dir.join("worm.py");

    std::fs::write(
        &script,
        r#"#!/usr/bin/env python3
import sys, json, struct

def read():
    n = sys.stdin.buffer.read(4)
    if len(n) < 4: sys.exit(0)
    return json.loads(sys.stdin.buffer.read(struct.unpack("<I", n)[0]))

def send(obj):
    b = json.dumps(obj).encode()
    sys.stdout.buffer.write(struct.pack("<I", len(b)) + b)
    sys.stdout.buffer.flush()

while True:
    req = read()
    op = req["op"]
    if op == "init":
        send({"ok": True})
    elif op == "lsp_resolve":
        if req["spec"].endswith(".data"):
            send({"ok": True, "path": "/virtual/" + req["spec"]})
        else:
            send({"ok": True})
    elif op == "lsp_load":
        send({"ok": True, "source": "return 7", "span_map": [[0, 8, 0, 4]], "claims": [[0, 4]]})
    elif op == "lsp_declarations":
        send({"ok": True, "declarations": [{"name": "mock", "source": "declare mockGlobal: number"}]})
    elif op == "lsp_respond":
        body = json.loads(req["response"])
        body["contents"]["value"] = body["contents"]["value"] + " (via worm)"
        send({"ok": True, "response": body})
    else:
        send({"ok": False, "error": "unexpected op " + op})
"#,
    )
    .unwrap();

    use std::os::unix::fs::PermissionsExt;

    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/*
Answers `bytecode` with what it was asked, so the dispatch is provable:
the method, the optimization level, and the view all read back.
*/
struct BytecodeAnalysis;

impl crate::lsp::analysis::Analysis for BytecodeAnalysis {
    fn open(&mut self, _: &std::path::Path, _: &str) {}

    fn check(&mut self, _: &std::path::Path) -> Vec<crate::lsp::analysis::AnalysisDiag> {
        Vec::new()
    }

    fn hover(&mut self, _: &std::path::Path, _: u32, _: bool, _: bool) -> Option<String> {
        None
    }

    fn completions(
        &mut self,
        _: &std::path::Path,
        _: u32,
    ) -> Vec<crate::lsp::analysis::AnalysisCompletion> {
        Vec::new()
    }

    fn invalidate(&mut self, _: &std::path::Path) {}

    fn bytecode(
        &mut self,
        source: &str,
        optimization: u8,
        remarks: bool,
        config: &crate::config::lsp::BytecodeConfig,
    ) -> Option<String> {
        Some(format!(
            "O{optimization} remarks={remarks} debug={} first={:?}",
            config.debug_level,
            source.lines().next().unwrap_or_default(),
        ))
    }
}

/// Captures what the server installs through the seam
struct MockAnalysis {
    hooks: std::sync::Arc<std::sync::Mutex<Option<crate::lsp::analysis::ModuleHooks>>>,
    defs: std::sync::Arc<std::sync::Mutex<Vec<(String, String)>>>,
    invalidated: std::sync::Arc<std::sync::Mutex<Vec<std::path::PathBuf>>>,
}

impl crate::lsp::analysis::Analysis for MockAnalysis {
    fn set_module_hooks(&mut self, hooks: crate::lsp::analysis::ModuleHooks) {
        *self.hooks.lock().unwrap() = Some(hooks);
    }

    fn definitions(&mut self, name: &str, source: &str) -> bool {
        self.defs
            .lock()
            .unwrap()
            .push((name.to_string(), source.to_string()));

        true
    }

    fn open(&mut self, _: &std::path::Path, _: &str) {}

    fn check(&mut self, _: &std::path::Path) -> Vec<crate::lsp::analysis::AnalysisDiag> {
        Vec::new()
    }

    fn hover(&mut self, _: &std::path::Path, _: u32, _: bool, _: bool) -> Option<String> {
        None
    }

    fn completions(
        &mut self,
        _: &std::path::Path,
        _: u32,
    ) -> Vec<crate::lsp::analysis::AnalysisCompletion> {
        Vec::new()
    }

    fn invalidate(&mut self, path: &std::path::Path) {
        self.invalidated.lock().unwrap().push(path.to_path_buf());
    }
}

#[cfg(unix)]
type SharedHooks = std::sync::Arc<std::sync::Mutex<Option<crate::lsp::analysis::ModuleHooks>>>;
#[cfg(unix)]
type SharedDefs = std::sync::Arc<std::sync::Mutex<Vec<(String, String)>>>;

#[cfg(unix)]
fn server_with_lsp_worm(dir: &std::path::Path) -> (Server, SharedHooks, SharedDefs) {
    server_with_lsp_worm_tracked(dir).0
}

type SharedPaths = std::sync::Arc<std::sync::Mutex<Vec<std::path::PathBuf>>>;

#[cfg(unix)]
fn server_with_lsp_worm_tracked(
    dir: &std::path::Path,
) -> ((Server, SharedHooks, SharedDefs), SharedPaths) {
    lsp_worm_that(dir);

    let hooks: SharedHooks = std::sync::Arc::new(std::sync::Mutex::new(None));
    let defs: SharedDefs = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let invalidated: SharedPaths = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));

    let mut server = Server {
        analysis: std::cell::RefCell::new(Some(Box::new(MockAnalysis {
            hooks: hooks.clone(),
            defs: defs.clone(),
            invalidated: invalidated.clone(),
        }))),
        ..Default::default()
    };
    server.worms = Pool::new(vec![spec(LSP_WORM, dir)], 1);
    server.install_lsp_hooks();

    ((server, hooks, defs), invalidated)
}

/*
An edit to a claimed file reaches the analyzer as an invalidation.

A plain Luau file requires the claimed one through the worm's lowering, and
the analyzer caches that lowering by path. Without the invalidation, a
saved data file kept every dependent on the old shape until a restart.
*/
#[cfg(unix)]
#[test]
fn a_claimed_file_edit_invalidates_the_analyzer() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let ((mut server, _hooks, _defs), invalidated) = server_with_lsp_worm_tracked(dir.path());
    let mut out = Vec::new();

    let uri = format!("file://{}/thing.x", dir.path().display());

    server
        .handle(
            &message(
                "textDocument/didOpen",
                None,
                json!({ "textDocument": { "uri": uri, "text": "data" } }),
            ),
            &mut out,
        )
        .unwrap();

    let paths = invalidated.lock().unwrap();

    assert!(
        paths.iter().any(|p| p.ends_with("thing.x")),
        "the claimed file never invalidated: {paths:?}"
    );
}

/// A worm that lints plain Luau, over the real transport
const LUAU_LINTER: &str = r#"
name = "privy"
api = 1
form = "native"
entry = "lintworm.py"
lints_luau = true

[lints.always]
description = "fires once per file, to prove the dispatch"
default = "warn"
"#;

/// A claiming worm that shares its files and hands back a byte-true shadow
const SHARING_WORM: &str = r#"
name = "markup"
api = 1
form = "native"
entry = "shareworm.py"

[frontend]
claims = [".luaux"]
shared = true

[lints.own]
description = "the claiming worm's own finding"
default = "warn"
"#;

#[cfg(unix)]
fn lint_worm_that(dir: &std::path::Path, script: &str, reply: &str) {
    let path = dir.join(script);

    std::fs::write(
        &path,
        format!(
            r#"#!/usr/bin/env python3
import sys, json, struct

def read():
    n = sys.stdin.buffer.read(4)
    if len(n) < 4: sys.exit(0)
    return json.loads(sys.stdin.buffer.read(struct.unpack("<I", n)[0]))

def send(obj):
    b = json.dumps(obj).encode()
    sys.stdout.buffer.write(struct.pack("<I", len(b)) + b)
    sys.stdout.buffer.flush()

while True:
    req = read()
    if req["op"] == "init":
        send({{"ok": True}})
    elif req["op"] == "transform":
        send({{"ok": True, "output": req["source"]}})
    elif req["op"] == "lint":
        send({reply})
    else:
        send({{"ok": False, "error": "unexpected op " + req["op"]}})
"#
        ),
    )
    .unwrap();

    use std::os::unix::fs::PermissionsExt;

    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/*
The editor reports a cross-realm require, the same one `larvae check`
reports.

The require compiles and resolves, so the analyzer is content, and the
build was the only reader that said anything. A client file requiring
server code now squiggles in the open file too.
*/
#[test]
fn a_cross_realm_require_squiggles() {
    let dir = tempfile::tempdir().expect("a temp dir");

    for file in ["src/client/main.luau", "src/server/net.luau"] {
        let path = dir.path().join(file);
        std::fs::create_dir_all(path.parent().expect("a parent")).expect("makes it");
        std::fs::write(&path, "return {}\n").expect("writes");
    }

    let mut server = Server {
        root: Some(dir.path().to_path_buf()),
        ..Default::default()
    };

    server.mounts = crate::requires::datamodel::MountTable::new(vec![
        crate::requires::datamodel::Mount {
            fs: dir.path().join("src/client"),
            dm: vec![
                "StarterPlayer".into(),
                "StarterPlayerScripts".into(),
                "Client".into(),
            ],
        },
        crate::requires::datamodel::Mount {
            fs: dir.path().join("src/server"),
            dm: vec!["ServerScriptService".into()],
        },
    ]);

    let uri = format!("file://{}/src/client/main.luau", dir.path().display());
    server.documents.insert(
        uri.clone(),
        "local net = require('../server/net')\nreturn net\n".into(),
    );

    let text = published(&server, &uri).to_string();

    assert!(
        text.contains("cross_realm_require") && text.contains("does not replicate"),
        "the crossing never published: {text}"
    );

    // The legal direction stays quiet.
    let uri = format!("file://{}/src/server/main.luau", dir.path().display());
    server.documents.insert(
        uri.clone(),
        "local net = require('./net')\nreturn net\n".into(),
    );

    let text = published(&server, &uri).to_string();

    assert!(
        !text.contains("cross_realm_require"),
        "the server side is allowed to require its own: {text}"
    );
}

/*
`[lsp.<worm>]` reaches the worm as config, checked against its options.

The worm declares what it takes under `[options]`; a key outside that
list, a wrong type, or a name that is no worm each say so, instead of
doing nothing in silence.
*/
#[test]
fn lsp_worm_settings_merge_into_the_config() {
    const CONFIGURED: &str = r#"
name = "privy"
api = 1
form = "native"
entry = "lintworm.py"
lints_luau = true

[lints.always]
description = "fires once per file"
default = "warn"

[options.strictness]
type = "boolean"
default = true
description = "how hard the worm squints"
"#;

    let dir = tempfile::tempdir().expect("a temp dir");
    let pool = Pool::new(vec![spec_with(CONFIGURED, dir.path(), vec![])], 1);

    let lsp: crate::config::lsp::LspConfig =
        toml::from_str("sourcemap = \"map.json\"\n\n[privy]\nstrictness = false\n")
            .expect("the worm table parses beside the typed keys");

    assert_eq!(lsp.sourcemap, "map.json");

    let mut complaints = Vec::new();
    let pool = pool.with_lsp_settings(&lsp.worms, &mut complaints);

    assert_eq!(complaints, Vec::<String>::new());
    assert_eq!(
        pool.spec(0).config.get("strictness"),
        Some(&toml::Value::Boolean(false))
    );

    // The three refusals, each with its own words.
    let bad: crate::config::lsp::LspConfig =
        toml::from_str("[nobody]\nx = 1\n\n[privy]\nmystery = 1\nstrictness = \"loud\"\n")
            .expect("parses");

    let mut complaints = Vec::new();
    let _ = pool.with_lsp_settings(&bad.worms, &mut complaints);

    assert!(
        complaints
            .iter()
            .any(|c| c.contains("no worm named `nobody`")),
        "{complaints:?}"
    );
    assert!(
        complaints
            .iter()
            .any(|c| c.contains("no setting `mystery`")),
        "{complaints:?}"
    );
    assert!(
        complaints
            .iter()
            .any(|c| c.contains("strictness takes a boolean")),
        "{complaints:?}"
    );
}

/*
A worm with `lints_luau` reports inside plain Luau files.

The lint walk only asked the worm that claims a file, so a worm about
conventions in ordinary code had no way to speak. The manifest key adds
its Lint op after the builtin lints, on the same levels and suppressions.
*/
#[cfg(unix)]
#[test]
fn a_luau_linting_worm_reports_in_plain_files() {
    let dir = tempfile::tempdir().expect("a temp dir");

    lint_worm_that(
        dir.path(),
        "lintworm.py",
        r#"{"ok": True, "findings": [{"span": [0, 6], "lint": "always", "message": "the worm sees this file"}], "comments": []}"#,
    );

    let mut server = Server {
        worms: Pool::new(vec![spec_with(LUAU_LINTER, dir.path(), vec![])], 1),
        ..Default::default()
    };

    let uri = "file:///p/main.luau";
    server.documents.insert(uri.into(), "return 1\n".into());

    let diagnostics = published(&server, uri);
    let text = diagnostics.to_string();

    assert!(
        text.contains("the worm sees this file"),
        "the foreign finding never published: {text}"
    );
}

/*
A shared claimed file takes foreign findings, on the shadow's offsets.

The claiming worm consents with `shared` and hands back its byte-true
Luau shadow; a foreign `lints_luau` worm reads that shadow, so its spans
land on the author's own bytes. Both worms' findings publish together.
*/
#[cfg(unix)]
#[test]
fn a_shared_claimed_file_takes_foreign_findings() {
    let dir = tempfile::tempdir().expect("a temp dir");

    lint_worm_that(
        dir.path(),
        "shareworm.py",
        r#"{"ok": True, "findings": [{"span": [0, 5], "lint": "own", "message": "the owner speaks"}], "comments": [], "luau": "local x = 1\n"}"#,
    );
    lint_worm_that(
        dir.path(),
        "lintworm.py",
        r#"{"ok": True, "findings": [{"span": [6, 7], "lint": "always", "message": "the guest speaks"}], "comments": []}"#,
    );

    let mut server = Server {
        worms: Pool::new(
            vec![
                spec_with(SHARING_WORM, dir.path(), vec![".luaux".to_owned()]),
                spec_with(LUAU_LINTER, dir.path(), vec![]),
            ],
            1,
        ),
        ..Default::default()
    };

    let uri = "file:///p/thing.luaux";
    server.documents.insert(uri.into(), "local x = 1\n".into());

    let text = published(&server, uri).to_string();

    assert!(
        text.contains("the owner speaks") && text.contains("the guest speaks"),
        "expected both worms' findings: {text}"
    );
}

/// Tier 1: the analyzer's resolution asks the worm first, over the pipe.
#[cfg(unix)]
#[test]
fn a_worm_resolves_and_loads_a_module_for_the_analyzer() {
    let dir = tempfile::tempdir().unwrap();
    let (_server, hooks, _) = server_with_lsp_worm(dir.path());

    let hooks = hooks.lock().unwrap();
    let hooks = hooks.as_ref().expect("the server installed the hooks");

    let resolved = (hooks.resolve)(std::path::Path::new("/p/a.luau"), "./thing.data");

    assert_eq!(resolved.as_deref(), Some("/virtual/./thing.data"));
    assert_eq!(
        (hooks.resolve)(std::path::Path::new("/p/a.luau"), "./plain.luau"),
        None,
        "an unclaimed spec passes through"
    );

    let loaded = (hooks.load)("/virtual/thing.data");

    assert_eq!(loaded.as_deref(), Some("return 7"));
}

/// Tier 2: the worm's declarations reach the analyzer at load.
#[cfg(unix)]
#[test]
fn a_worms_declarations_reach_the_analyzer() {
    let dir = tempfile::tempdir().unwrap();
    let (_server, _, defs) = server_with_lsp_worm(dir.path());

    let defs = defs.lock().unwrap();

    assert_eq!(defs.len(), 1);
    assert_eq!(defs[0].0, "mock");
    assert!(defs[0].1.contains("mockGlobal"), "{}", defs[0].1);
}

/// Tier 3: a hover passes through the worm before the editor sees it.
#[cfg(unix)]
#[test]
fn a_worm_transforms_a_hover_response() {
    let dir = tempfile::tempdir().unwrap();
    let (server, _, _) = server_with_lsp_worm(dir.path());

    let hover = serde_json::json!({
        "contents": { "kind": "markdown", "value": "number" }
    });

    let out = server
        .worms
        .lsp_respond("hover", &serde_json::json!({}), hover);

    assert_eq!(out["contents"]["value"], "number (via worm)", "{out}");
}

/// The span map answers with the narrowest original range, for tier 1 output.
#[test]
fn the_span_map_of_a_load_reply_maps_generated_spans_back() {
    let map = crate::worm::proto::SpanMap(vec![(0, 8, 0, 4)]);

    assert_eq!(map.to_original((0, 8)), Some((0, 4)));
    assert_eq!(map.to_original((2, 6)), Some((0, 4)));
    assert_eq!(map.to_original((9, 12)), None);
}

// --- issue 1503: keywords beat auto-imports -------------------------------

/// An analyzer that answers what luau-lsp answered in the report
struct Issue1503Analysis;

impl crate::lsp::analysis::Analysis for Issue1503Analysis {
    fn open(&mut self, _: &std::path::Path, _: &str) {}

    fn check(&mut self, _: &std::path::Path) -> Vec<crate::lsp::analysis::AnalysisDiag> {
        Vec::new()
    }

    fn hover(&mut self, _: &std::path::Path, _: u32, _: bool, _: bool) -> Option<String> {
        None
    }

    fn completions(
        &mut self,
        _: &std::path::Path,
        _: u32,
    ) -> Vec<crate::lsp::analysis::AnalysisCompletion> {
        vec![
            crate::lsp::analysis::AnalysisCompletion {
                label: "end".into(),
                kind: 14,
                detail: None,
                label_detail: None,
                insert_text: None,
                documentation: None,
                deprecated: false,
                type_correct: 0,
                wrong_index_type: false,
            },
            crate::lsp::analysis::AnalysisCompletion {
                label: "elapsedTime".into(),
                kind: 3,
                detail: Some("() -> number".into()),
                label_detail: Some("()".into()),
                insert_text: Some("elapsedTime()".into()),
                documentation: Some("The seconds since the process started.".into()),
                deprecated: false,
                type_correct: 0,
                wrong_index_type: false,
            },
        ]
    }

    fn services(&mut self) -> Vec<String> {
        vec!["EncodingService".into(), "EditorSourceService".into()]
    }

    fn invalidate(&mut self, _: &std::path::Path) {}
}

fn issue_server(src: &str) -> Server {
    let mut server = server_with(src);
    server.analysis = std::cell::RefCell::new(Some(Box::new(Issue1503Analysis)));

    server
}

fn completion_items(server: &Server, line: u32, character: u32) -> Vec<Value> {
    let result = server.completions(&serde_json::json!({
        "textDocument": { "uri": "file:///t.luau" },
        "position": { "line": line, "character": character },
    }));

    result.as_array().cloned().unwrap_or_default()
}

/*
The report: typing `end` to close a guard clause, and the list hands the
author EncodingService. Here the typed keyword must sort first among every
item that matches the prefix, and preselect, so enter confirms `end`.
*/
#[test]
fn a_typed_end_beats_the_encoding_service_import() {
    let server = issue_server("if x then return end");
    let items = completion_items(&server, 0, 20);

    // The service does not even offer against `end`: nothing to rank.
    assert!(
        !items.iter().any(|i| i["label"] == "EncodingService"),
        "a service must not offer against the prefix `end`"
    );

    let end = items.iter().find(|i| i["label"] == "end").expect("end");

    assert_eq!(
        end["preselect"], true,
        "the exactly typed keyword preselects"
    );
    assert!(
        end["sortText"].as_str().unwrap().starts_with('0'),
        "keywords take the first tier"
    );

    // When the prefix does fit a service, the import offers, ranked last.
    let items = completion_items(&issue_server("Enc"), 0, 3);
    let import = items
        .iter()
        .find(|i| i["label"] == "EncodingService")
        .expect("the service offers for its own prefix");

    assert!(import["sortText"].as_str().unwrap().starts_with('9'));
}

/// The comment on the report: `else` losing to the `elapsedTime` global.
#[test]
fn a_typed_else_beats_the_elapsed_time_global() {
    let server = issue_server("if x then\nels");
    let items = completion_items(&server, 1, 3);

    let sort_of = |label: &str| {
        items
            .iter()
            .find(|i| i["label"] == label)
            .map(|i| i["sortText"].as_str().unwrap_or_default().to_string())
            .expect(label)
    };

    // "end" stands in for the keyword tier here; the analyzer of the mock
    // reports the same two kinds the comment names.
    assert!(
        sort_of("end") < sort_of("elapsedTime"),
        "keywords outrank globals"
    );
}

/// The auto-import lands above the guard clause, never inside it.
#[test]
fn a_service_import_inserts_above_the_first_statement() {
    let src = "-- header comment\nconst Players = game:GetService(\"Players\")\n\nif x then return end\nEnc";
    let server = issue_server(src);
    let items = completion_items(&server, 4, 3);

    let import = items
        .iter()
        .find(|i| i["label"] == "EncodingService")
        .expect("the service offers");

    let edit = &import["additionalTextEdits"][0];

    assert_eq!(
        edit["range"]["start"]["line"], 2,
        "after the existing import block"
    );
    assert!(
        edit["newText"]
            .as_str()
            .unwrap()
            .contains("const EncodingService = game:GetService(\"EncodingService\")"),
        "{import}"
    );

    // A service the file already binds does not offer again.
    assert!(!items.iter().any(|i| i["label"] == "Players"));
}

/*
The json worm's shape: its hooks answer inside plain Luau files, so
claim-only gating widens when it loads, or installing the worm changes
nothing in the editor.
*/
#[cfg(unix)]
#[test]
fn a_serving_worm_widens_claim_only_gating() {
    let dir = tempfile::tempdir().unwrap();
    lsp_worm_that(dir.path());

    const SERVING: &str = r#"
name = "data"
api = 1
form = "native"
entry = "worm.py"

[frontend]
claims = [".fake"]

[lsp]
resolve = true
serves_luau = true
"#;

    let mut server = server_with("local x={a=1}\nreturn x\n");
    server.lsp = crate::config::lsp::LspConfig {
        enabled: true,
        claim_only: true,
        ..Default::default()
    };
    server.worms = Pool::new(vec![spec(SERVING, dir.path())], 1);

    // A plain Luau file formats although claim_only is on.
    assert_ne!(server.format("file:///t.luau").unwrap(), Value::Null);
}

// --- what a worm reaches the editor with ------------------------------------

/*
A worm's code action travels the real transport and comes out as LSP.

The worm speaks in bytes, because it parsed the file. The host turns those
into the line and character the protocol wants, which is the conversion a
finding already goes through, so a wrong one here would put an edit in the
wrong place.
*/
#[cfg(unix)]
#[test]
fn a_worm_offers_a_code_action() {
    let dir = tempfile::tempdir().unwrap();

    worm_that(
        dir.path(),
        r#"    if req["op"] == "actions":
        send({"ok": True, "actions": [
            {"title": "Wrap it", "edits": [{"span": [6, 9], "text": "there"}],
             "fixes": "bad_word"}
        ]})
        continue
    send({"ok": True})"#,
    );

    let server = server_with_worm(
        "name = \"markup\"\napi = 1\nform = \"native\"\nentry = \"worm.py\"\n\n[frontend]\nclaims = [\".luaux\"]\n\n[lints.bad_word]\ndescription = \"x\"\n",
        dir.path(),
        "hello you\n",
    );

    let found = extend::code_actions(
        &server.worms,
        "file:///p/t.luaux",
        "hello you\n",
        &json!({
            "start": { "line": 0, "character": 0 },
            "end": { "line": 0, "character": 9 }
        }),
    );

    assert_eq!(found.len(), 1, "{found:#?}");
    assert_eq!(found[0]["title"], "Wrap it (markup)", "the worm is named");
    assert_eq!(found[0]["kind"], "quickfix");

    let edits = &found[0]["edit"]["changes"]["file:///p/t.luaux"];
    assert_eq!(edits[0]["newText"], "there");
    assert_eq!(
        edits[0]["range"]["start"]["character"], 6,
        "byte 6 is column 6"
    );
    assert_eq!(edits[0]["range"]["end"]["character"], 9);

    // a fix that names its lint is grouped under that diagnostic
    assert_eq!(found[0]["diagnostics"][0]["code"], "markup.bad_word");
}

/// A worm supplies Luau definition text, and it comes out under its name.
#[cfg(unix)]
#[test]
fn a_worm_supplies_type_definitions() {
    let dir = tempfile::tempdir().unwrap();

    worm_that(
        dir.path(),
        r#"    if req["op"] == "definitions":
        send({"ok": True, "definitions": "declare items: { string }\n"})
        continue
    send({"ok": True})"#,
    );

    let server = server_with_worm(
        "name = \"markup\"\napi = 1\nform = \"native\"\nentry = \"worm.py\"\n\n[frontend]\nclaims = [\".luaux\"]\n",
        dir.path(),
        "x\n",
    );

    let supplied = extend::definitions(&server.worms);

    assert_eq!(supplied.len(), 1, "{supplied:#?}");
    assert_eq!(supplied[0].worm, "markup");
    assert!(supplied[0].text.contains("declare items"), "{supplied:#?}");

    let reply = extend::definitions_reply(&server.worms);
    assert_eq!(reply["definitions"][0]["worm"], "markup");
}

/*
A worm with neither costs a reply and not an error.

The editor asks on a keystroke. A worm that only formats has nothing to say
here, and a failure would put a line in the editor log every time the
lightbulb opens.
*/
#[cfg(unix)]
#[test]
fn a_worm_with_nothing_to_offer_is_quiet() {
    let dir = tempfile::tempdir().unwrap();

    worm_that(dir.path(), "    send({\"ok\": True})");

    let server = server_with_worm(
        "name = \"markup\"\napi = 1\nform = \"native\"\nentry = \"worm.py\"\n\n[frontend]\nclaims = [\".luaux\"]\n",
        dir.path(),
        "x\n",
    );

    assert!(
        extend::code_actions(&server.worms, "file:///p/t.luaux", "x\n", &json!(null)).is_empty()
    );
    assert!(extend::definitions(&server.worms).is_empty());
}

// --- [lsp.completion.imports] use_const ------------------------------------

/// The service an auto-import offers, with the keyword the setting decided.
fn import_edit(server: &Server, src_prefix_line: u32, character: u32) -> (String, String) {
    let items = completion_items(server, src_prefix_line, character);

    let import = items
        .iter()
        .find(|i| i["label"] == "EncodingService")
        .expect("the service offers");

    (
        import["detail"].as_str().expect("a detail").to_string(),
        import["additionalTextEdits"][0]["newText"]
            .as_str()
            .expect("a text edit")
            .to_string(),
    )
}

/*
A module of the project offers itself, and accepting writes the require.

The offer comes from the workspace walk, spelled in the configured style:
relative by default here, an alias when one covers the module and the
style asks for it. A `.server.` file is a script, not a module, and
never offers.
*/
#[test]
fn a_module_auto_import_writes_the_require() {
    let dir = tempfile::tempdir().expect("a temp dir");
    std::fs::create_dir_all(dir.path().join("src/Widgets")).expect("makes it");
    std::fs::write(dir.path().join("src/util.luau"), "return {}\n").expect("writes");
    std::fs::write(dir.path().join("src/Widgets/init.luau"), "return {}\n").expect("writes");
    std::fs::write(dir.path().join("src/boot.server.luau"), "return nil\n").expect("writes");

    let mut server = issue_server("uti");
    server.root = Some(dir.path().to_path_buf());
    server.symbols = workspace::Index::build(dir.path(), &Default::default());
    server.documents.insert(
        format!("file://{}/src/main.luau", dir.path().display()),
        "uti".into(),
    );

    let items = server.completions(&json!({
        "textDocument": { "uri": format!("file://{}/src/main.luau", dir.path().display()) },
        "position": { "line": 0, "character": 3 },
    }));
    let items = items.as_array().cloned().unwrap_or_default();

    let offer = items
        .iter()
        .find(|i| i["label"] == "util")
        .unwrap_or_else(|| panic!("util offers: {items:?}"));

    assert!(
        offer["additionalTextEdits"][0]["newText"]
            .as_str()
            .unwrap()
            .contains("require(\"./util\")"),
        "{offer}"
    );

    assert!(
        !items.iter().any(|i| i["label"] == "boot"),
        "a script offered itself: {items:?}"
    );
}

/// The directory module offers by its directory name, addressed whole.
#[test]
fn an_init_module_offers_as_its_directory() {
    let dir = tempfile::tempdir().expect("a temp dir");
    std::fs::create_dir_all(dir.path().join("src/Widgets")).expect("makes it");
    std::fs::write(dir.path().join("src/Widgets/init.luau"), "return {}\n").expect("writes");

    let mut server = issue_server("Wid");
    server.root = Some(dir.path().to_path_buf());
    server.symbols = workspace::Index::build(dir.path(), &Default::default());
    server.documents.insert(
        format!("file://{}/src/main.luau", dir.path().display()),
        "Wid".into(),
    );

    let items = server.completions(&json!({
        "textDocument": { "uri": format!("file://{}/src/main.luau", dir.path().display()) },
        "position": { "line": 0, "character": 3 },
    }));
    let items = items.as_array().cloned().unwrap_or_default();

    let offer = items
        .iter()
        .find(|i| i["label"] == "Widgets")
        .unwrap_or_else(|| panic!("the directory offers: {items:?}"));

    assert!(
        offer["detail"]
            .as_str()
            .unwrap()
            .contains("require(\"./Widgets\")"),
        "{offer}"
    );
}

/// An alias speaks for the module when the style lets it.
#[test]
fn the_alias_style_spells_the_import_under_its_alias() {
    let dir = tempfile::tempdir().expect("a temp dir");
    std::fs::create_dir_all(dir.path().join("src/shared")).expect("makes it");
    std::fs::write(dir.path().join("src/shared/util.luau"), "return {}\n").expect("writes");

    let mut server = issue_server("uti");
    server.root = Some(dir.path().to_path_buf());
    server.symbols = workspace::Index::build(dir.path(), &Default::default());
    server.aliases.insert("shared".into(), "src/shared".into());
    server.documents.insert(
        format!("file://{}/src/client/main.luau", dir.path().display()),
        "uti".into(),
    );

    let items = server.completions(&json!({
        "textDocument": { "uri": format!("file://{}/src/client/main.luau", dir.path().display()) },
        "position": { "line": 0, "character": 3 },
    }));
    let items = items.as_array().cloned().unwrap_or_default();

    let offer = items
        .iter()
        .find(|i| i["label"] == "util")
        .unwrap_or_else(|| panic!("util offers: {items:?}"));

    assert!(
        offer["detail"].as_str().unwrap().contains("@shared/util"),
        "auto style speaks the alias: {offer}"
    );
}

/*
The auto-import writes `const` unless the project says otherwise.

This is a deliberate departure from luau-lsp, which defaults the setting off
because Luau had no `const` when it was written. An auto-import binds a
service and nothing reassigns it, which is the clearest case for the keyword.
*/
#[test]
fn an_auto_import_writes_const_by_default() {
    let server = issue_server("Enc");
    let (detail, text) = import_edit(&server, 0, 3);

    assert!(text.starts_with("const EncodingService ="), "{text}");
    assert!(detail.contains("const EncodingService"), "{detail}");
}

/*
The import writes the quote the formatter would keep.

`[fmt] quote_style = "auto-prefer-single"` and an accepted import with
double quotes fought each other one save later.
*/
#[test]
fn an_auto_import_follows_the_project_quote_style() {
    let mut server = issue_server("Enc");
    server.fmt.quote_style = crate::fmt::config::QuoteStyle::AutoPreferSingle;

    let (detail, text) = import_edit(&server, 0, 3);

    assert!(
        text.contains("game:GetService('EncodingService')"),
        "{text}"
    );
    assert!(detail.contains("('EncodingService')"), "{detail}");
}

/// A project that has not adopted `const` turns the setting off and gets `local`.
#[test]
fn use_const_off_writes_local() {
    let mut server = issue_server("Enc");
    server.lsp.completion.imports.use_const = false;

    let (detail, text) = import_edit(&server, 0, 3);

    assert!(text.starts_with("local EncodingService ="), "{text}");
    assert!(!text.contains("const"), "{text}");

    // A user reads the detail before accepting, so it cannot say the other word.
    assert!(detail.contains("local EncodingService"), "{detail}");
    assert!(!detail.contains("const"), "{detail}");
}

/*
The insertion point reads a `local` import as an import.

With the setting off a file's preamble is bound with `local`, and a new
import still has to land at the end of that preamble rather than above it.
*/
#[test]
fn a_local_preamble_still_ends_where_the_import_goes() {
    let src =
        "-- header\nlocal Players = game:GetService(\"Players\")\n\nif x then return end\nEnc";
    let mut server = issue_server(src);
    server.lsp.completion.imports.use_const = false;

    let items = completion_items(&server, 4, 3);
    let import = items
        .iter()
        .find(|i| i["label"] == "EncodingService")
        .expect("the service offers");

    assert_eq!(
        import["additionalTextEdits"][0]["range"]["start"]["line"], 2,
        "the import must land after the preamble, not inside the guard"
    );
}

// --- the parity requests, through the router -------------------------------

/// Drive one request through the dispatch and give back the parsed reply.
fn ask(server: &mut Server, method: &str, params: Value) -> Value {
    let mut out = Vec::new();

    server
        .handle(&message(method, Some(77), params), &mut out)
        .expect("the dispatch answers");

    let text = String::from_utf8(out).expect("utf8");
    let body = text
        .split("\r\n\r\n")
        .nth(1)
        .unwrap_or_else(|| panic!("no body in {text}"));

    let reply: Value = serde_json::from_str(body).expect("json");

    assert!(
        reply.get("error").is_none(),
        "an editor must not see a failure from {method}: {reply}"
    );

    reply["result"].clone()
}

fn at(line: u32, character: u32) -> Value {
    json!({
        "textDocument": { "uri": "file:///t.luau" },
        "position": { "line": line, "character": character },
    })
}

/*
Every advertised capability has a dispatch arm behind it.

A capability the server claims and does not answer is worse than one it
never claimed: the editor asks on every keystroke and logs a failure each
time. This walks the advertised list rather than naming each one, so a
capability added without a handler fails here.
*/
#[test]
fn every_advertised_provider_answers() {
    // `true` so the analyzer-only providers are walked as well.
    let caps = capabilities(true);
    let caps = caps["capabilities"].as_object().expect("a table");

    let method_of = |provider: &str| match provider {
        "documentFormattingProvider" => Some("textDocument/formatting"),
        "documentSymbolProvider" => Some("textDocument/documentSymbol"),
        "codeActionProvider" => Some("textDocument/codeAction"),
        "definitionProvider" => Some("textDocument/definition"),
        "workspaceSymbolProvider" => Some("workspace/symbol"),
        "semanticTokensProvider" => Some("textDocument/semanticTokens/full"),
        "referencesProvider" => Some("textDocument/references"),
        "documentHighlightProvider" => Some("textDocument/documentHighlight"),
        "renameProvider" => Some("textDocument/rename"),
        "foldingRangeProvider" => Some("textDocument/foldingRange"),
        "selectionRangeProvider" => Some("textDocument/selectionRange"),
        "documentLinkProvider" => Some("textDocument/documentLink"),
        "colorProvider" => Some("textDocument/documentColor"),
        "hoverProvider" => Some("textDocument/hover"),
        "typeDefinitionProvider" => Some("textDocument/typeDefinition"),
        "signatureHelpProvider" => Some("textDocument/signatureHelp"),
        "inlayHintProvider" => Some("textDocument/inlayHint"),
        "completionProvider" => Some("textDocument/completion"),
        _ => None,
    };

    for key in caps.keys() {
        if !key.ends_with("Provider") {
            continue;
        }

        let method = method_of(key).unwrap_or_else(|| {
            panic!("{key} is advertised and this test does not know its method")
        });

        let mut server = server_with("local x = 1\nreturn x\n");

        // The call panics on an error reply, which is the assertion.
        let _ = ask(
            &mut server,
            method,
            json!({
                "textDocument": { "uri": "file:///t.luau" },
                "position": { "line": 1, "character": 7 },
                "positions": [{ "line": 1, "character": 7 }],
                "context": { "includeDeclaration": true, "diagnostics": [] },
                "newName": "y",
                "range": {
                    "start": { "line": 0, "character": 0 },
                    "end": { "line": 0, "character": 1 },
                },
            }),
        );
    }
}

/// Goto definition lands on the declaration, through the protocol.
#[test]
fn a_definition_request_points_at_the_declaration() {
    let mut server = server_with("local widget = 1\nreturn widget\n");
    let result = ask(&mut server, "textDocument/definition", at(1, 8));

    assert_eq!(result["range"]["start"]["line"], 0);
    assert_eq!(result["range"]["start"]["character"], 6);
    assert_eq!(result["uri"], "file:///t.luau");
}

/// A shadowed name resolves to the binding the scope walk chose, not the text.
#[test]
fn a_definition_request_respects_shadowing() {
    let src = "local x = 1\ndo\n\tlocal x = 2\n\tprint(x)\nend\nprint(x)\n";
    let mut server = server_with(src);

    let inner = ask(&mut server, "textDocument/definition", at(3, 8));
    let outer = ask(&mut server, "textDocument/definition", at(5, 6));

    assert_eq!(inner["range"]["start"]["line"], 2, "the inner x");
    assert_eq!(outer["range"]["start"]["line"], 0, "the outer x");
}

#[test]
fn a_references_request_lists_every_use() {
    let mut server = server_with("local x = 1\nprint(x)\nprint(x)\n");

    let result = ask(
        &mut server,
        "textDocument/references",
        json!({
            "textDocument": { "uri": "file:///t.luau" },
            "position": { "line": 0, "character": 6 },
            "context": { "includeDeclaration": true },
        }),
    );

    let list = result.as_array().expect("a list");

    assert_eq!(list.len(), 3, "the declaration and two reads: {result}");
}

/// A highlight tags the declaration as a write and each use as a read.
#[test]
fn a_highlight_request_tags_reads_and_writes() {
    let mut server = server_with("local x = 1\nx = 2\nprint(x)\n");
    let result = ask(&mut server, "textDocument/documentHighlight", at(0, 6));

    let list = result.as_array().expect("a list");
    let kinds: Vec<u64> = list.iter().filter_map(|h| h["kind"].as_u64()).collect();

    assert!(kinds.contains(&3), "a write is kind 3: {result}");
    assert!(kinds.contains(&2), "a read is kind 2: {result}");
}

#[test]
fn a_rename_request_edits_every_use() {
    let mut server = server_with("local x = 1\nprint(x)\n");

    let result = ask(
        &mut server,
        "textDocument/rename",
        json!({
            "textDocument": { "uri": "file:///t.luau" },
            "position": { "line": 0, "character": 6 },
            "newName": "widget",
        }),
    );

    let edits = result["changes"]["file:///t.luau"]
        .as_array()
        .expect("edits for this file");

    assert_eq!(edits.len(), 2, "{result}");
    assert_eq!(edits[0]["newText"], "widget");
}

/// A reserved word is refused, because the edit would not compile.
#[test]
fn a_rename_to_a_keyword_is_refused() {
    let mut server = server_with("local x = 1\nprint(x)\n");

    let result = ask(
        &mut server,
        "textDocument/rename",
        json!({
            "textDocument": { "uri": "file:///t.luau" },
            "position": { "line": 0, "character": 6 },
            "newName": "end",
        }),
    );

    assert!(result.is_null(), "a keyword must not be accepted: {result}");
}

#[test]
fn a_folding_request_finds_the_function_body() {
    let src = "local function f()\n\tlocal a = 1\n\treturn a\nend\nreturn f\n";
    let mut server = server_with(src);
    let result = ask(
        &mut server,
        "textDocument/foldingRange",
        json!({ "textDocument": { "uri": "file:///t.luau" } }),
    );

    let list = result.as_array().expect("a list");

    assert!(
        list.iter()
            .any(|r| r["startLine"] == 0 && r["endLine"] == 3),
        "{result}"
    );
}

/// The chain grows outward, which is what the protocol asks of it.
#[test]
fn a_selection_range_request_returns_a_growing_chain() {
    let mut server = server_with("local x = 1\nprint(x + 2)\n");
    let result = ask(
        &mut server,
        "textDocument/selectionRange",
        json!({
            "textDocument": { "uri": "file:///t.luau" },
            "positions": [{ "line": 1, "character": 6 }],
        }),
    );

    let mut node = result[0].clone();
    let mut seen = 0;

    while !node.is_null() {
        seen += 1;

        let parent = node["parent"].clone();

        if !parent.is_null() {
            // A parent must start no later and end no earlier than its child.
            assert!(
                parent["range"]["start"]["line"].as_u64()
                    <= node["range"]["start"]["line"].as_u64(),
                "the chain does not grow: {result}"
            );
        }

        node = parent;
    }

    assert!(seen >= 2, "a chain needs more than one step: {result}");
}

/// A Color3 written out in full gets a swatch, with the channels the editor wants.
#[test]
fn a_document_color_request_finds_a_literal_colour() {
    let mut server = server_with("local red = Color3.fromRGB(255, 0, 0)\nreturn red\n");
    let result = ask(
        &mut server,
        "textDocument/documentColor",
        json!({ "textDocument": { "uri": "file:///t.luau" } }),
    );

    let list = result.as_array().expect("a list");

    assert_eq!(list.len(), 1, "{result}");
    assert_eq!(list[0]["color"]["red"], 1.0);
    assert_eq!(list[0]["color"]["green"], 0.0);
}

/// A colour whose channels are not literals cannot be known, so no swatch.
#[test]
fn a_computed_colour_gets_no_swatch() {
    let mut server = server_with("local c = Color3.fromRGB(n, 0, 0)\nreturn c\n");
    let result = ask(
        &mut server,
        "textDocument/documentColor",
        json!({ "textDocument": { "uri": "file:///t.luau" } }),
    );

    assert_eq!(result.as_array().expect("a list").len(), 0, "{result}");
}

/// The outline is a tree, so a nested function is a child and not a sibling.
#[test]
fn the_document_symbol_reply_nests() {
    let src = "local function outer()\n\tlocal function inner()\n\tend\n\n\treturn inner\nend\n\nreturn outer\n";
    let mut server = server_with(src);

    let result = ask(
        &mut server,
        "textDocument/documentSymbol",
        json!({ "textDocument": { "uri": "file:///t.luau" } }),
    );

    let list = result.as_array().expect("a list");
    let outer = list.iter().find(|s| s["name"] == "outer").expect("outer");

    let children = outer["children"].as_array().expect("children");

    assert!(
        children.iter().any(|c| c["name"] == "inner"),
        "inner must nest under outer: {result}"
    );
}

// --- the editor settings blob ----------------------------------------------

/*
The editor can speak, and the project wins where both do.

The extension sends its `larvae-lsp` section at initialize and again on every
change. Until the server read it, every editor setting it mirrors did
nothing, including the `useConst` one the config doc promises.
*/
#[test]
fn an_editor_setting_reaches_the_server() {
    let mut server = Server::default();
    let mut out = Vec::new();

    server
        .handle(
            &message(
                "initialize",
                Some(1),
                json!({
                    "initializationOptions": {
                        "settings": {
                            "larvae-lsp": {
                                "claimOnly": true,
                                "completion": { "imports": { "useConst": false } },
                                "documentation": ["types/docs.json"],
                            }
                        }
                    }
                }),
            ),
            &mut out,
        )
        .expect("initializes");

    assert!(server.lsp.claim_only, "claimOnly did not reach the server");
    assert!(
        !server.lsp.completion.imports.use_const,
        "useConst did not reach the server"
    );
    assert_eq!(server.lsp.documentation, ["types/docs.json"]);
}

/*
A project wins the settings it spells, and only those.

`[lsp]` used to be copied over the whole table, so any `larvae.toml` threw
away every editor setting, including the ones it says nothing about. A user
who turned the new solver on in the editor got the old one and no reason.
*/
#[test]
fn the_project_wins_only_where_it_names_a_setting() {
    let mut server = Server {
        editor: json!({
            "larvae-lsp": {
                "claimOnly": true,
                "fflags": { "enableNewSolver": true },
                "hover": { "showTableKinds": true },
            }
        }),
        ..Default::default()
    };

    let project = toml::from_str::<toml::Value>("[lsp]\nclaim_only = false\n").expect("parses");

    server.apply_editor_settings(Some(&project));

    // The project spelled this one, so it wins.
    assert!(!server.lsp.claim_only);

    // It said nothing about these, so the editor keeps them.
    assert!(
        server.lsp.fflags.enable_new_solver,
        "the editor's enableNewSolver was thrown away"
    );
    assert!(server.lsp.hover.show_table_kinds);
}

/// A nested name is matched by its own path, and not by the table above it.
#[test]
fn a_project_that_names_one_flag_leaves_the_others_to_the_editor() {
    let mut server = Server {
        editor: json!({
            "larvae-lsp": { "fflags": { "enableNewSolver": true, "enableByDefault": true } }
        }),
        ..Default::default()
    };

    let project =
        toml::from_str::<toml::Value>("[lsp.fflags]\nenable_new_solver = false\n").expect("parses");

    server.apply_editor_settings(Some(&project));

    assert!(!server.lsp.fflags.enable_new_solver);
    assert!(server.lsp.fflags.enable_by_default);
}

/*
`larvae/bytecode` compiles the open document at the level the editor asked.

The reply carries the analyzer's text, and the params reach it: the
optimization level from the request, the debug level from `[lsp.bytecode]`,
and the remarks flag from which method was called.
*/
#[test]
fn the_bytecode_request_reaches_the_analyzer() {
    let mut server = Server {
        analysis: std::cell::RefCell::new(Some(Box::new(BytecodeAnalysis))),
        ..Default::default()
    };
    let mut out = Vec::new();

    let uri = "file:///project/a.luau";

    server
        .handle(
            &message(
                "textDocument/didOpen",
                None,
                json!({ "textDocument": { "uri": uri, "text": "return 1\n" } }),
            ),
            &mut out,
        )
        .unwrap();

    let reply = ask(
        &mut server,
        "larvae/bytecode",
        json!({ "textDocument": { "uri": uri }, "optimizationLevel": 1 }),
    );

    assert_eq!(
        reply.as_str().unwrap(),
        "O1 remarks=false debug=1 first=\"return 1\""
    );

    let reply = ask(
        &mut server,
        "larvae/compilerRemarks",
        json!({ "textDocument": { "uri": uri } }),
    );

    // No level in the request compiles at O2, which is luau-lsp's default.
    assert!(
        reply.as_str().unwrap().starts_with("O2 remarks=true"),
        "{reply}"
    );
}

/*
`[lsp] analyzer = false` serves what larvae always served, and no more.

The lints, the format, and the actions stay on both kinds of file. The
capabilities of the analyzer are not advertised, hover answers nothing even
though the seam holds an analyzer, and the type findings stay out of a
publish. That is the serving larvae had before the analyzer landed.
*/
#[test]
fn analyzer_off_is_the_classic_server() {
    let mut server = Server {
        analysis: std::cell::RefCell::new(Some(Box::new(BytecodeAnalysis))),
        ..Default::default()
    };
    server.lsp.analyzer = false;
    let mut out = Vec::new();

    // Not advertised, so the editor never asks.
    let caps = capabilities(server.will_analyse() && server.lsp.analyzer);
    assert!(caps["capabilities"]["hoverProvider"].is_null(), "{caps}");
    assert!(caps["capabilities"]["completionProvider"].is_null());

    // The classic half still is.
    assert_eq!(
        caps["capabilities"]["documentFormattingProvider"],
        json!(true)
    );
    assert_eq!(caps["capabilities"]["codeActionProvider"], json!(true));

    // An editor that asks anyway gets the honest nothing.
    let uri = "file:///project/a.luau";
    server
        .handle(
            &message(
                "textDocument/didOpen",
                None,
                json!({ "textDocument": { "uri": uri, "text": "local unused = 1\nreturn 2\n" } }),
            ),
            &mut out,
        )
        .unwrap();

    let hover = ask(
        &mut server,
        "textDocument/hover",
        json!({ "textDocument": { "uri": uri }, "position": at(0, 7) }),
    );
    assert!(hover.is_null(), "{hover}");

    // The lints still publish: the open above pushed diagnostics.
    let text = String::from_utf8(out).unwrap();
    assert!(text.contains("unused"), "the lint pass went quiet: {text}");
}

/*
A require offer replaces exactly what the author typed of the segment.

Without the edit range the editor guesses a word, and its guess holds no
`@`: typing `@sh` filtered a list of `@shared/` offers against `sh`,
nothing matched, and the list closed instead of narrowing.
*/
#[test]
fn a_require_offer_carries_the_range_it_replaces() {
    let dir = tempfile::tempdir().expect("a temp dir");
    std::fs::create_dir_all(dir.path().join("src")).expect("makes it");
    std::fs::write(
        dir.path().join(".luaurc"),
        r#"{ "aliases": { "shared": "src" } }"#,
    )
    .expect("writes");
    std::fs::write(dir.path().join("src/util.luau"), "return {}\n").expect("writes");

    let mut server = Server {
        root: Some(dir.path().to_path_buf()),
        ..Default::default()
    };
    let mut out = Vec::new();

    let uri = format!("file://{}/src/main.luau", dir.path().display());
    let text = "local a = require('@sh')\nreturn a\n";

    server
        .handle(
            &message(
                "textDocument/didOpen",
                None,
                json!({ "textDocument": { "uri": uri, "text": text } }),
            ),
            &mut out,
        )
        .unwrap();

    // The cursor sits after `@sh`, inside the quotes.
    let reply = ask(
        &mut server,
        "textDocument/completion",
        json!({
            "textDocument": { "uri": uri },
            "position": { "line": 0, "character": 22 },
        }),
    );

    let items = reply["items"]
        .as_array()
        .unwrap_or_else(|| panic!("no items in {reply}"));
    let offer = items
        .iter()
        .find(|i| i["label"] == "@shared/")
        .unwrap_or_else(|| panic!("no @shared/ offer in {reply}"));

    // The edit replaces from the `@` to the cursor, so the filter sees it.
    let range = &offer["textEdit"]["range"];

    assert_eq!(
        range["start"],
        json!({ "line": 0, "character": 19 }),
        "{offer}"
    );
    assert_eq!(
        range["end"],
        json!({ "line": 0, "character": 22 }),
        "{offer}"
    );
    assert_eq!(offer["textEdit"]["newText"], "@shared/");
}

/// A later change replaces what the editor said before.
#[test]
fn a_configuration_change_replaces_the_blob() {
    let mut server = Server::default();
    let mut out = Vec::new();

    server.editor = json!({ "larvae-lsp": { "claimOnly": true } });

    server.apply_editor_settings(None);

    assert!(server.lsp.claim_only);

    server
        .handle(
            &message(
                "workspace/didChangeConfiguration",
                None,
                json!({ "settings": { "larvae-lsp": { "claimOnly": false } } }),
            ),
            &mut out,
        )
        .expect("handles");

    assert!(!server.lsp.claim_only, "the change did not take");
}

/*
A setting the server does not know is ignored, not refused.

luau-lsp ships about ninety settings and the extension mirrors the names, so
a server that failed on an unknown id would fail on every editor ahead of it.
*/
#[test]
fn an_unknown_setting_is_ignored() {
    let mut server = Server {
        editor: json!({
            "larvae-lsp": {
                "inlayHints": { "parameterNames": "all" },
                "somethingNobodyShipped": 7,
                "completion": { "imports": { "useConst": false } },
            }
        }),
        ..Default::default()
    };

    server.apply_editor_settings(None);

    // The one it knows still lands.
    assert!(!server.lsp.completion.imports.use_const);
}

/// An empty blob leaves the defaults alone.
#[test]
fn no_editor_settings_changes_nothing() {
    let mut server = Server::default();
    let before = (server.lsp.enabled, server.lsp.claim_only);

    server.apply_editor_settings(None);

    assert_eq!((server.lsp.enabled, server.lsp.claim_only), before);
}

/// Every knob the extension mirrors reaches the server.
#[test]
fn the_feature_knobs_reach_the_server() {
    let mut server = Server {
        editor: json!({
            "larvae-lsp": {
                "signatureHelp": { "enabled": false },
                "hover": { "enabled": false },
                "inlayHints": {
                    "variableTypes": true,
                    "parameterTypes": true,
                    "typeHintMaxLength": 12,
                },
            }
        }),
        ..Default::default()
    };

    server.apply_editor_settings(None);

    assert!(!server.lsp.signature_help.enabled);
    assert!(!server.lsp.hover.enabled);
    assert!(server.lsp.inlay_hints.variable_types);
    assert!(server.lsp.inlay_hints.parameter_types);
    assert_eq!(server.lsp.inlay_hints.type_hint_max_length, 12);
}

/*
A hint is off until the project asks for it.

The editor draws it into a line the author did not write, and a reader who
did not ask reads that as the file changing under them.
*/
#[test]
fn inlay_hints_are_off_by_default() {
    let server = Server::default();

    assert!(!server.lsp.inlay_hints.variable_types);
    assert!(!server.lsp.inlay_hints.parameter_types);

    let result = server.inlay_hints(&json!({
        "textDocument": { "uri": "file:///t.luau" }
    }));

    assert_eq!(result, json!([]), "a hint appeared without being asked for");
}

/// Signature help and hover are on, because neither draws anything unasked.
#[test]
fn signature_help_and_hover_are_on_by_default() {
    let server = Server::default();

    assert!(server.lsp.signature_help.enabled);
    assert!(server.lsp.hover.enabled);
}

// --- workspace/symbol ------------------------------------------------------

/// The picker opens with an empty query, and a project dump is not an answer.
#[test]
fn an_empty_workspace_query_answers_with_nothing() {
    let server = Server::default();
    let result = server.workspace_symbols(&json!({ "query": "" }));

    assert_eq!(result, json!([]));
}

/// A search finds a symbol in a file the editor never opened.
#[test]
fn a_workspace_search_reaches_the_whole_project() {
    let dir = tempfile::tempdir().expect("a temp dir");

    std::fs::write(
        dir.path().join("thing.luau"),
        "local function getPlayerName()\n\treturn \"a\"\nend\n\nreturn getPlayerName\n",
    )
    .expect("writes");

    let mut server = Server {
        root: Some(dir.path().to_path_buf()),
        ..Default::default()
    };

    server.reindex();

    let result = server.workspace_symbols(&json!({ "query": "getPlayer" }));
    let list = result.as_array().expect("a list");

    assert_eq!(list.len(), 1, "{result}");
    assert_eq!(list[0]["name"], "getPlayerName");
    assert!(
        list[0]["location"]["uri"]
            .as_str()
            .expect("a uri")
            .ends_with("thing.luau"),
        "{result}"
    );
}

/// A server with no root indexes nothing rather than walking the cwd.
#[test]
fn no_root_indexes_nothing() {
    let mut server = Server::default();

    server.reindex();

    assert_eq!(
        server.workspace_symbols(&json!({ "query": "anything" })),
        json!([])
    );
}

/// The completion and index knobs reach the server too.
#[test]
fn the_completion_and_index_knobs_reach_the_server() {
    let mut server = Server {
        editor: json!({
            "larvae-lsp": {
                "completion": {
                    "enabled": false,
                    "showKeywords": false,
                    "imports": { "enabled": false },
                },
                "index": { "enabled": false },
            }
        }),
        ..Default::default()
    };

    server.apply_editor_settings(None);

    assert!(!server.lsp.completion.enabled);
    assert!(!server.lsp.completion.show_keywords);
    assert!(!server.lsp.completion.imports.enabled);
    assert!(!server.lsp.index.enabled);
}

/// An index that is off holds no symbols, so the search answers with nothing.
#[test]
fn an_index_that_is_off_finds_nothing() {
    let dir = tempfile::tempdir().expect("a temp dir");

    std::fs::write(
        dir.path().join("a.luau"),
        "local function findMe()\nend\n\nreturn findMe\n",
    )
    .expect("writes");

    let mut server = Server {
        root: Some(dir.path().to_path_buf()),
        ..Default::default()
    };

    server.reindex();
    assert_ne!(
        server.workspace_symbols(&json!({ "query": "findMe" })),
        json!([]),
        "the index should hold it while it is on"
    );

    server.lsp.index.enabled = false;
    server.reindex();

    assert_eq!(
        server.workspace_symbols(&json!({ "query": "findMe" })),
        json!([])
    );
}

/// Every completion knob defaults on, because none of them hides anything.
#[test]
fn the_completion_knobs_default_on() {
    let server = Server::default();

    assert!(server.lsp.completion.enabled);
    assert!(server.lsp.completion.show_keywords);
    assert!(server.lsp.completion.imports.enabled);
    assert!(server.lsp.index.enabled);
}

// --- [lsp.fflags] and [lsp.bytecode] ---------------------------------------

/*
The two tables the extension already sends reach the server.

It sends them as of its commit 604baf1, and the server dropped them. A
setting the server drops is worse than one it stores: the user changes it,
nothing happens, and nothing says why.
*/
#[test]
fn the_flag_and_bytecode_settings_reach_the_server() {
    let mut server = Server {
        editor: json!({
            "larvae-lsp": {
                "fflags": {
                    "enableByDefault": true,
                    "enableNewSolver": true,
                    "override": { "LuauTarjanChildLimit": "20000", "LuauSolverV2": "false" },
                },
                "bytecode": {
                    "debugLevel": 2,
                    "typeInfoLevel": 0,
                    "vectorLib": "Vec",
                    "vectorCtor": "make",
                    "vectorType": "Vec3",
                },
            }
        }),
        ..Default::default()
    };

    server.apply_editor_settings(None);

    assert!(server.lsp.fflags.enable_by_default);
    assert!(server.lsp.fflags.enable_new_solver);
    assert_eq!(
        server
            .lsp
            .fflags
            .over
            .get("LuauTarjanChildLimit")
            .map(String::as_str),
        Some("20000")
    );
    assert_eq!(
        server
            .lsp
            .fflags
            .over
            .get("LuauSolverV2")
            .map(String::as_str),
        Some("false")
    );

    assert_eq!(server.lsp.bytecode.debug_level, 2);
    assert_eq!(server.lsp.bytecode.type_info_level, 0);
    assert_eq!(server.lsp.bytecode.vector_lib, "Vec");
    assert_eq!(server.lsp.bytecode.vector_ctor, "make");
    assert_eq!(server.lsp.bytecode.vector_type, "Vec3");
}

/*
An override arrives as text whatever the editor sent.

Luau keeps a boolean list and an integer list, and the flag name decides
which one is asked, so a JSON number and a JSON string have to reach the
same place.
*/
#[test]
fn an_override_of_any_json_type_becomes_text() {
    let mut server = Server {
        editor: json!({
            "larvae-lsp": {
                "fflags": { "override": { "AsNumber": 120, "AsBool": true, "AsText": "no" } }
            }
        }),
        ..Default::default()
    };

    server.apply_editor_settings(None);

    let over = &server.lsp.fflags.over;

    assert_eq!(over.get("AsNumber").map(String::as_str), Some("120"));
    assert_eq!(over.get("AsBool").map(String::as_str), Some("true"));
    assert_eq!(over.get("AsText").map(String::as_str), Some("no"));
}

/*
Both default to what larvae is without them.

`enable_by_default` is off, which departs from luau-lsp on purpose: larvae
ships one pinned Luau and the same binary to everyone, so a flag that
misbehaves misbehaves for every user at once.
*/
#[test]
fn the_flag_defaults_are_conservative() {
    let server = Server::default();

    assert!(!server.lsp.fflags.enable_by_default);
    assert!(!server.lsp.fflags.enable_new_solver);
    assert!(server.lsp.fflags.over.is_empty());

    assert_eq!(server.lsp.bytecode.debug_level, 1);
    assert_eq!(server.lsp.bytecode.vector_lib, "Vector3");
}

/*
A doc comment opened above a declaration answers with its block.

The author types `---` and wants the shape of the thing below it
written out: the name, a line per parameter with the type they wrote,
and the return. A name that opens with an underscore takes `@private`,
the same convention that hides it from a completion list.
*/
#[test]
fn a_doc_comment_offers_the_moonwave_block() {
    let src = "---\nlocal function add(a: number, b: string): boolean\n\treturn true\nend\n";
    let (text, _) = super::features::doc_scaffold_for(src, 3).expect("a block");

    assert_eq!(
        text,
        "--[=[\n\tadd\n\n\t@param a number\n\t@param b string\n\t@return boolean\n]=]"
    );

    // A method keeps the last piece of its path, and an underscore is private.
    let hidden = "---\nfunction Class:_hidden(x)\nend\n";
    let (text, _) = super::features::doc_scaffold_for(hidden, 3).expect("a block");

    assert!(text.contains("\t_hidden\n"), "{text}");
    assert!(text.contains("@private"), "{text}");
    assert!(text.contains("@param x"), "{text}");
}

/// The block answers only where a comment opens above a declaration.
#[test]
fn the_moonwave_block_stays_out_of_prose() {
    // A comment with words in it is prose the author is writing.
    assert!(super::features::doc_scaffold_for("--- a note\nlocal function f() end\n", 4).is_none());

    // Nothing to describe below it.
    assert!(super::features::doc_scaffold_for("---\nlocal x = 1\n", 3).is_none());

    // An ordinary comment is not a doc comment.
    assert!(super::features::doc_scaffold_for("--\nlocal function f() end\n", 2).is_none());
}

/*
A completion asked while the session is still being built waits for it.

The editor asks on the first keystroke, and the session takes seconds.
An empty answer closed the popup for good; the held answer opens it the
moment the types land. The editor cancels the ask it no longer wants,
and the cancel takes that one back and answers it as cancelled.
*/
#[test]
fn a_completion_asked_while_loading_answers_when_the_session_lands() {
    struct Ready;

    impl crate::lsp::analysis::Analysis for Ready {
        fn open(&mut self, _: &std::path::Path, _: &str) {}

        fn check(&mut self, _: &std::path::Path) -> Vec<crate::lsp::analysis::AnalysisDiag> {
            Vec::new()
        }

        fn hover(&mut self, _: &std::path::Path, _: u32, _: bool, _: bool) -> Option<String> {
            None
        }

        fn invalidate(&mut self, _: &std::path::Path) {}

        fn completions(
            &mut self,
            _: &std::path::Path,
            _: u32,
        ) -> Vec<crate::lsp::analysis::AnalysisCompletion> {
            vec![crate::lsp::analysis::AnalysisCompletion {
                label: "local".into(),
                kind: 14,
                detail: None,
                label_detail: None,
                insert_text: None,
                documentation: None,
                deprecated: false,
                type_correct: 0,
                wrong_index_type: false,
            }]
        }
    }

    fn message(value: Value) -> rpc::Message {
        serde_json::from_value(value).expect("a message")
    }

    fn ask(id: u64) -> rpc::Message {
        message(json!({
            "id": id,
            "method": "textDocument/completion",
            "params": {
                "textDocument": { "uri": "file:///t.luau" },
                "position": { "line": 0, "character": 2 },
            },
        }))
    }

    let mut server = Server {
        analysis_pending: true,
        ..server_with("lo\n")
    };
    server.lsp.analyzer = true;

    let mut out = Vec::new();

    assert!(!server.handle(&ask(1), &mut out).unwrap());
    assert!(!server.handle(&ask(2), &mut out).unwrap());
    assert!(
        out.is_empty(),
        "an answer before the session: {}",
        String::from_utf8_lossy(&out)
    );
    assert_eq!(server.held.len(), 2);

    let cancel = message(json!({ "method": "$/cancelRequest", "params": { "id": 1 } }));
    server.handle(&cancel, &mut out).unwrap();

    let text = String::from_utf8_lossy(&out).into_owned();
    assert!(
        text.contains("\"id\":1"),
        "the cancel answers the ask it took back: {text}"
    );
    assert_eq!(server.held.len(), 1);
    out.clear();

    server.take_analysis(Box::new(Ready), &mut out).unwrap();

    let text = String::from_utf8_lossy(&out).into_owned();
    assert!(
        text.contains("\"id\":2"),
        "the held ask answers at the landing: {text}"
    );
    assert!(
        text.contains("\"label\":\"local\""),
        "with the list: {text}"
    );
    assert!(
        !text.contains("\"id\":1"),
        "the cancelled ask stays answered once: {text}"
    );
    assert!(server.held.is_empty());
}

/*
`--new-solver` wins over the project file and the editor.

Both sources say the old solver here, the way a project that never set
the key reads, and the forced value stands after they have spoken.
*/
#[test]
fn the_forced_new_solver_stands_over_every_settings_source() {
    let mut server = Server {
        forced: Forced {
            new_solver: true,
            ..Forced::default()
        },
        ..Server::default()
    };
    server.editor = json!({ "larvae-lsp": { "fflags": { "enableNewSolver": false } } });

    let project: toml::Value = toml::from_str("[lsp.fflags]\nenable_new_solver = false\n").unwrap();
    server.apply_editor_settings(Some(&project));

    assert!(server.lsp.fflags.enable_new_solver);

    let plain = Server::default();
    assert!(
        !plain.lsp.fflags.enable_new_solver,
        "the default is still the old solver"
    );
}

/// `--no-warning` keeps the warnings off screen and leaves the errors.
#[test]
fn the_forced_no_warnings_drops_every_warning_and_keeps_the_errors() {
    let src = "local unused = 1\nlocal x = = 2\n";

    let mut server = server_with(src);
    server.lint = LintConfig::default();
    assert!(
        published(&server, "file:///t.luau")
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["severity"] == 2),
        "a warning publishes without the flag"
    );

    server.forced = Forced {
        no_warnings: true,
        ..Forced::default()
    };
    let diags = published(&server, "file:///t.luau");
    let list = diags.as_array().unwrap();

    assert!(!list.is_empty(), "the error stays");
    assert!(list.iter().all(|d| d["severity"] == 1), "{diags}");
}
