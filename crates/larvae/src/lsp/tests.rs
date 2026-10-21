use super::uri::path_of_uri;
use super::*;

fn server_with(src: &str) -> Server {
    let mut server = Server::default();
    server.documents.insert("file:///t.luau".into(), src.into());

    server
}

/*
The server advertises lint, format, and fixes, and none of the type work.

luau-lsp runs beside it and answers hover, completion, and the rest. Two
servers that both advertise one leave the editor to pick, so larvae must
not claim any of them.
*/
#[test]
fn the_advertised_capabilities_are_lint_and_format() {
    let caps = capabilities();
    let caps = &caps["capabilities"];

    assert_eq!(caps["documentFormattingProvider"], true);
    assert_eq!(caps["codeActionProvider"], true);
    assert_eq!(caps["textDocumentSync"]["change"], 1, "full sync");

    for owned in ["hoverProvider", "completionProvider", "definitionProvider"] {
        assert!(caps[owned].is_null(), "{owned} belongs to luau-lsp");
    }
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

#[test]
fn a_clean_document_produces_no_diagnostics() {
    assert_eq!(diagnostics_of("return 1\n"), json!([]));
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
    let caps = capabilities();

    assert!(
        caps["capabilities"]["monikerProvider"].is_null(),
        "moniker is advertised now, so the test above needs a new example"
    );
}

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

/// A setting of the analyzer still loads, and says it does nothing now.
#[test]
fn a_retired_setting_says_it_does_nothing() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("larvae.toml"),
        "[lsp]\ncolor_picker = false\n\n[lsp.fflags]\nenable_new_solver = true\n",
    )
    .unwrap();

    let out = initialized_at(dir.path());

    assert!(
        out.contains("[lsp] fflags does nothing since larvae 0.10"),
        "{out}"
    );
    assert!(out.contains("[lsp] color_picker does nothing"), "{out}");
    assert!(!out.contains("no worm named"), "{out}");
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
fn claim_only_declines_formatting_for_a_plain_luau_file() {
    let mut server = server_with("local x={a=1}\nreturn x\n");
    server.lsp = crate::config::lsp::LspConfig {
        enabled: true,
        claim_only: true,
        ..Default::default()
    };

    assert_eq!(server.format("file:///t.luau").unwrap(), Value::Null);
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

// --- worms that lint ---------------------------------------------------

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

The require compiles and resolves, so the type checker is content, and the
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
        toml::from_str("claim_only = true\n\n[privy]\nstrictness = false\n")
            .expect("the worm table parses beside the typed keys");

    assert!(lsp.claim_only);

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

/*
A worm with no actions costs a reply and not an error.

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
}

// --- the editor settings blob ----------------------------------------------

/*
The editor can speak, and the project wins where both do.

The extension sends its `larvae-lsp` section at initialize and again on every
change, and the server reads `enabled` and `claimOnly` from it.
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
                                "enabled": false,
                            }
                        }
                    }
                }),
            ),
            &mut out,
        )
        .expect("initializes");

    assert!(server.lsp.claim_only, "claimOnly did not reach the server");
    assert!(!server.lsp.enabled, "enabled did not reach the server");
}

/*
A project wins the settings it spells, and only those.

`[lsp]` used to be copied over the whole table, so any `larvae.toml` threw
away every editor setting, including the ones it says nothing about.
*/
#[test]
fn the_project_wins_only_where_it_names_a_setting() {
    let mut server = Server {
        editor: json!({
            "larvae-lsp": {
                "claimOnly": true,
                "enabled": false,
            }
        }),
        ..Default::default()
    };

    let project = toml::from_str::<toml::Value>("[lsp]\nclaim_only = false\n").expect("parses");

    server.apply_editor_settings(Some(&project));

    // The project spelled this one, so it wins.
    assert!(!server.lsp.claim_only);

    // It said nothing about this one, so the editor keeps it.
    assert!(!server.lsp.enabled, "the editor's enabled was thrown away");
}

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

An editor that is ahead of the server sends ids the server has never seen,
and a server that failed on one would not start there.
*/
#[test]
fn an_unknown_setting_is_ignored() {
    let mut server = Server {
        editor: json!({
            "larvae-lsp": {
                "inlayHints": { "parameterNames": "all" },
                "somethingNobodyShipped": 7,
                "claimOnly": true,
            }
        }),
        ..Default::default()
    };

    server.apply_editor_settings(None);

    // The one it knows still lands.
    assert!(server.lsp.claim_only);
}

/// An empty blob leaves the defaults alone.
#[test]
fn no_editor_settings_changes_nothing() {
    let mut server = Server::default();
    let before = (server.lsp.enabled, server.lsp.claim_only);

    server.apply_editor_settings(None);

    assert_eq!((server.lsp.enabled, server.lsp.claim_only), before);
}
