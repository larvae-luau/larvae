use std::path::Path;
use std::sync::Arc;

use super::*;
use crate::lint::{self, Level};

const URI: &str = "file:///p/t.luaux";
const FAIL: &str = "error(\"cannot compile markup\")";
const FINDING: &str = r#"
local start = string.find(source, "<Frame")
if start == nil then
    error("the fixture has no opening tag")
end
return {
    findings = {{
        span = {start - 1, start + 5},
        lint = "parse_error",
        message = "unclosed element",
        help = "close the opening tag",
    }},
}
"#;

fn server(src: &str, compile: &str, lint: &str) -> Server {
    let mut spec = spec(
        r#"
name = "markup"
api = 1
form = "luau"
entry = "worm.luau"

[frontend]
claims = [".luaux"]
inherit_lints = true

[lints.parse_error]
default = "deny"
"#,
        Path::new("."),
    );
    Arc::get_mut(&mut spec).unwrap().artifact = format!(
        r#"
type Reply = {{
    findings: {{{{span: {{number}}, lint: string, message: string, help: string?}}}}?,
    comments: {{{{number}}}}?,
    luau: string?,
}}
return table.freeze({{
    frontend = table.freeze({{
        compile = function(source: string): string?
            {compile}
        end,
        lint = function(source: string): Reply
            {lint}
        end,
    }}),
}})
"#
    )
    .into_bytes();

    let mut server = Server::default();
    server.documents.insert(URI.into(), src.into());
    server.worms = Pool::new(vec![spec], 1);
    server
}

#[test]
fn failed_compilation_preserves_byte_spans_and_terminal_positions() {
    let src = "-- header\n  <Frame\n";
    for compile in [FAIL, "return"] {
        let server = server(src, compile, FINDING);
        let path = Path::new("t.luaux");
        let findings = lint::claimed(path, src, &server.lint, &server.worms, 0).unwrap();

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].span, (12, 18));
        assert_eq!(findings[0].lint, "markup.parse_error");
        assert_eq!(findings[0].level, Level::Deny);
        let diags = lint::into_diags(path, src, findings);
        assert_eq!(diags[0].line_col, Some((2, 3)));
    }
}

#[test]
fn failed_compilation_preserves_the_published_utf16_range() {
    let src = "-- header\r\n\"😀\" <Frame\r\n";
    let server = server(src, FAIL, FINDING);
    let diags = published(&server, URI);

    assert_eq!(
        diags,
        json!([{
            "range": {
                "start": {"line": 1, "character": 5},
                "end": {"line": 1, "character": 11},
            },
            "severity": 1,
            "source": "larvae",
            "code": "markup.parse_error",
            "message": "unclosed element\nclose the opening tag",
        }])
    );
}

#[test]
fn failed_compilation_respects_the_configured_level() {
    let mut server = server("<Frame\n", FAIL, FINDING);
    server
        .lint
        .rules
        .insert("markup.parse_error".into(), Level::Info);
    assert_eq!(published(&server, URI)[0]["severity"], 3);

    server
        .lint
        .rules
        .insert("markup.parse_error".into(), Level::Allow);
    assert_eq!(published(&server, URI), json!([]));
}

#[test]
fn failed_compilation_respects_comment_suppressions() {
    let lint = FINDING.replace(
        "return {",
        "return { comments = {{0, string.find(source, \"\\n\") - 1}},",
    );
    for comment in [
        "-- larvae: allow(markup.parse_error)",
        "-- larvae: lint off",
    ] {
        let src = format!("{comment}\n<Frame\n");
        let server = server(&src, FAIL, &lint);
        assert_eq!(published(&server, URI), json!([]), "{comment}");
    }
}

#[test]
fn failed_compilation_without_findings_still_reports_the_failure() {
    for (compile, message) in [
        (FAIL, "cannot compile markup"),
        ("return", "returned nothing"),
    ] {
        let server = server("<Frame\n", compile, "return {}");
        let diags = published(&server, URI);

        assert_eq!(diags.as_array().unwrap().len(), 1);
        assert_eq!(diags[0]["severity"], 1);
        assert!(
            diags[0]["message"].as_str().unwrap().contains(message),
            "{diags}"
        );
    }
}

#[test]
fn failed_compilation_keeps_lint_reply_validation() {
    for (lint, message) in [
        (
            FINDING.replace("parse_error", "unknown"),
            "does not declare",
        ),
        (FINDING.replace("start + 5", "999"), "off the source"),
        (
            FINDING.replace("return {", "return { comments = {{0, 999}},"),
            "a comment span",
        ),
    ] {
        let src = "<Frame\n";
        let server = server(src, FAIL, &lint);
        let error =
            lint::claimed(Path::new("t.luaux"), src, &server.lint, &server.worms, 0).unwrap_err();
        assert!(error.message.contains(message), "{}", error.message);
    }
}

#[test]
fn successful_projection_keeps_inherited_findings_first_on_the_same_byte() {
    let src = "<Frame\n";
    let server = server(src, "return \"local unused = 1\\n\"", FINDING);
    let findings =
        lint::claimed(Path::new("t.luaux"), src, &server.lint, &server.worms, 0).unwrap();

    assert_eq!(findings.len(), 2);
    assert_eq!(findings[0].lint, "unused_variable");
    assert_eq!(findings[1].lint, "markup.parse_error");
    assert_eq!(findings[0].span, (0, 0));
    assert_eq!(findings[1].span, (0, 6));
}

#[test]
fn a_shadow_keeps_exact_inherited_positions_without_compilation() {
    let src = "local unused = <Frame\n";
    let lint = FINDING.replace(
        "return {",
        "return { luau = string.gsub(source, \"<Frame\", \"nil   \"),",
    );
    let server = server(src, FAIL, &lint);
    let findings =
        lint::claimed(Path::new("t.luaux"), src, &server.lint, &server.worms, 0).unwrap();

    assert_eq!(findings.len(), 2);
    assert_eq!(findings[0].lint, "unused_variable");
    assert_eq!(findings[0].span, (6, 12));
    assert_eq!(findings[1].span, (15, 21));
}

#[test]
fn an_invalid_shadow_still_reports_the_worm_failure() {
    let src = "<Frame\n";
    let lint = FINDING.replace("return {", "return { luau = \"local =\",");
    let server = server(src, FAIL, &lint);
    let error =
        lint::claimed(Path::new("t.luaux"), src, &server.lint, &server.worms, 0).unwrap_err();

    assert!(
        error.message.contains("returned Luau larvae cannot read"),
        "{}",
        error.message
    );
}
