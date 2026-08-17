//! These tests run `larvae fmt` and `larvae lint` on files that a native worm
//! claims.

#![cfg(unix)]

use std::path::Path;
use std::process::Command;

fn bin() -> &'static Path {
    Path::new(env!("CARGO_BIN_EXE_larvae"))
}

fn write(dir: &Path, name: &str, body: &str) -> std::path::PathBuf {
    let path = dir.join(name);

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("makes the directory");
    }

    std::fs::write(&path, body).expect("writes");

    path
}

fn run(dir: &Path, args: &[&str]) -> (bool, String) {
    let out = Command::new(bin())
        .args(args)
        .current_dir(dir)
        .output()
        .expect("runs");

    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    (out.status.success(), text)
}

/*
This is a full native worm in a few lines of python.

Its format op returns the entire file as one `host` span, so larvae lays the
file out as Luau. This is enough to prove that the document crosses, splices,
and renders. Its lint op reports one finding when the file contains "bad".
The op also returns the span of a leading comment, so `-- larvae: allow(...)`
works.
*/
fn install_worm(root: &Path) {
    write(
        root,
        "mywormdir/worm.toml",
        r#"
name  = "markup"
api   = 1
form  = "native"
entry = "worm.py"

[frontend]
claims = [".luaux"]
fmt = true
inherit_lints = true

[lints.bad_word]
description = "The word bad is bad"
"#,
    );

    let script = write(
        root,
        "mywormdir/worm.py",
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

def comments(src):
    if src.startswith("--"):
        return [[0, src.index("\n")]]
    return []

while True:
    req = read()
    op = req["op"]
    if op == "init":
        send({"ok": True})
    elif op == "format":
        src = req["source"]
        send({"ok": True, "doc": 1,
              "document": {"host": {"start": 0, "end": len(src.encode())}},
              "comments": comments(src)})
    elif op == "lint":
        src = req["source"]
        findings = []
        if "bad" in src:
            i = src.index("bad")
            findings.append({"span": [i, i + 3], "lint": "bad_word",
                             "message": "the word bad"})
        shadow = src.replace("<Frame/>", "(nil)   ")
        send({"ok": True, "findings": findings, "comments": comments(src),
              "luau": shadow})
    else:
        send({"ok": True, "output": req["source"]})
"#,
    );

    {
        use std::os::unix::fs::PermissionsExt;

        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

fn project(worms: bool) -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();

    let worms_table = match worms {
        true => "\n[worms.markup]\npath = \"mywormdir\"\n",
        false => "",
    };

    write(
        root.path(),
        "larvae.toml",
        &format!("[process]\ninput = \"src\"\n{worms_table}"),
    );

    if worms {
        install_worm(root.path());
    }

    root
}

#[test]
fn fmt_formats_a_claimed_file_through_its_worm() {
    let root = project(true);
    let file = write(root.path(), "src/app.luaux", "local  x   =    1\n");
    write(root.path(), "src/plain.luau", "local  y  =  2\n");

    let (ok, text) = run(root.path(), &["fmt"]);

    assert!(ok, "{text}");

    // larvae's own emitter rendered the worm's host span.
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "local x = 1\n");

    // The ordinary file went through the ordinary path.
    assert_eq!(
        std::fs::read_to_string(root.path().join("src/plain.luau")).unwrap(),
        "local y = 2\n"
    );
}

#[test]
fn fmt_check_counts_a_claimed_file() {
    let root = project(true);
    write(root.path(), "src/app.luaux", "local  x   =    1\n");

    let (ok, text) = run(root.path(), &["fmt", "--check"]);

    assert!(
        !ok,
        "an unformatted claimed file has to fail --check: {text}"
    );
    assert!(text.contains("app.luaux"), "{text}");

    write(root.path(), "src/app.luaux", "local x = 1\n");

    let (ok, text) = run(root.path(), &["fmt", "--check"]);

    assert!(ok, "{text}");
}

#[test]
fn without_worms_a_claimed_file_is_left_alone() {
    let root = project(false);
    let file = write(root.path(), "src/app.luaux", "local  x   =    1\n");
    write(root.path(), "src/plain.luau", "local y = 2\n");

    let (ok, text) = run(root.path(), &["fmt"]);

    assert!(ok, "{text}");
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "local  x   =    1\n",
        "no worm formats .luaux, so the walk must skip it"
    );
}

#[test]
fn lint_reports_a_worm_finding_and_the_level_lever_works() {
    let root = project(true);
    write(root.path(), "src/app.luaux", "local bad = 1\n");

    let (ok, text) = run(root.path(), &["lint"]);

    assert!(ok, "a warning alone exits zero: {text}");
    assert!(text.contains("markup.bad_word"), "{text}");

    // A deny level in [lint.rules] fails the run, the same as a builtin lint.
    let config = std::fs::read_to_string(root.path().join("larvae.toml")).unwrap();
    write(
        root.path(),
        "larvae.toml",
        &format!("{config}\n[lint.rules.markup]\nbad_word = \"deny\"\n"),
    );

    let (ok, text) = run(root.path(), &["lint"]);

    assert!(!ok, "{text}");
}

#[test]
fn an_allow_comment_suppresses_a_worm_finding() {
    let root = project(true);
    write(
        root.path(),
        "src/app.luaux",
        "-- larvae: allow(markup.bad_word)\nlocal bad = 1\n",
    );

    let (ok, text) = run(root.path(), &["lint"]);

    assert!(ok, "{text}");
    assert!(
        !text.contains("markup.bad_word"),
        "the allow comment must silence it: {text}"
    );
}

/*
A `lint off` region holds the findings of a worm too.

Where `allow(...)` names one lint, the region holds every finding on those
lines. A reader who switches the linter off does not care which tool found
one.
*/
#[test]
fn a_lint_off_region_suppresses_a_worm_finding() {
    let root = project(true);
    write(
        root.path(),
        "src/app.luaux",
        "-- larvae: lint off\nlocal bad = 1\n-- larvae: lint on\n",
    );

    let (ok, text) = run(root.path(), &["lint"]);

    assert!(ok, "{text}");
    assert!(
        !text.contains("markup.bad_word"),
        "the region must hold it: {text}"
    );
}

/*
The count form holds a worm finding too.

A finding of a worm carries a source span, and the three forms of the marker
all arrive as a range of the source. So the count needs nothing of its own
here, and this test says so.
*/
#[test]
fn a_lint_count_suppresses_a_worm_finding_on_those_lines() {
    let root = project(true);
    write(
        root.path(),
        "src/app.luaux",
        "-- larvae: lint off(1)\nlocal bad = 1\n",
    );

    let (ok, text) = run(root.path(), &["lint"]);

    assert!(ok, "{text}");
    assert!(
        !text.contains("markup.bad_word"),
        "the count must hold it: {text}"
    );
}

/// A count that runs out leaves the lines below it alone.
#[test]
fn a_lint_count_stops_where_it_says() {
    let root = project(true);
    write(
        root.path(),
        "src/app.luaux",
        "-- larvae: lint off(1)\nlocal fine = 1\nlocal bad = 2\n",
    );

    let (_, text) = run(root.path(), &["lint"]);

    assert!(
        text.contains("markup.bad_word"),
        "the line past the count still reports: {text}"
    );
}

/// A file held off in full reports nothing from the worm.
#[test]
fn a_claimed_file_can_hold_the_linter_off_in_full() {
    let root = project(true);
    write(
        root.path(),
        "src/app.luaux",
        "-- larvae: lint off\nlocal bad = 1\n",
    );

    let (ok, text) = run(root.path(), &["lint"]);

    assert!(ok, "{text}");
    assert!(!text.contains("markup.bad_word"), "{text}");
}

/*
The generated schema names every worm, so its two tables close.

Each table held an open `additionalProperties` beside its `properties`, to
describe a worm that the shipped schema cannot know. A project schema knows
them all. Taplo reads both branches for one key, so with both present every
option of a described worm reached the completion list two times.
*/
#[test]
fn the_project_schema_closes_the_tables_it_filled() {
    let root = project(true);
    write(root.path(), "src/app.luaux", "local x = 1\n");

    let (ok, text) = run(root.path(), &["lint"]);
    assert!(ok, "{text}");

    let path = root.path().join(".larvae").join("larvae.schema.json");
    let raw = std::fs::read_to_string(&path).expect("the schema is generated");
    let schema: serde_json::Value = serde_json::from_str(&raw).expect("it is JSON");

    for table in ["fmt", "lint_rules"] {
        assert_eq!(
            schema["$defs"][table]["additionalProperties"],
            serde_json::json!(false),
            "[{table}] must offer one branch per key"
        );
    }

    // the worm is named in both, so closing them refuses nothing it should allow
    assert!(
        schema["$defs"]["lint_rules"]["properties"]["markup"].is_object(),
        "the lints of the worm are missing"
    );
    assert!(
        schema["$defs"]["fmt"]["properties"]["markup"].is_object(),
        "the format table of the worm is missing"
    );
}

#[test]
fn worm_run_fmt_is_the_dev_loop() {
    let root = project(true);
    let file = write(root.path(), "messy.luaux", "local  x   =    1\n");

    let (ok, text) = run(
        root.path(),
        &["worm", "run", "--fmt", "mywormdir", file.to_str().unwrap()],
    );

    assert!(ok, "{text}");
    assert!(text.contains("local x = 1"), "{text}");
    assert!(text.contains("idempotent"), "{text}");
}

#[test]
fn worm_run_lint_answers_like_the_linter() {
    let root = project(true);
    let file = write(root.path(), "app.luaux", "local bad = 1\n");

    let (ok, text) = run(
        root.path(),
        &["worm", "run", "--lint", "mywormdir", file.to_str().unwrap()],
    );

    assert!(ok, "a warn-level finding exits zero: {text}");
    assert!(text.contains("markup.bad_word"), "{text}");
}

/*
A claimed file also gets the lints of larvae.

The worm sets `inherit_lints`, so the builtin lints read its Luau shadow. The
shadow keeps every byte offset, so the finding points at the real column.
*/
#[test]
fn a_claimed_file_inherits_the_lints_of_larvae() {
    let root = project(true);
    write(root.path(), "src/app.luaux", "local unused = <Frame/>\n");

    let (ok, text) = run(root.path(), &["lint"]);

    assert!(ok, "{text}");
    assert!(text.contains("unused_variable"), "{text}");
}

/// The inherited lints obey `[lint.rules]`, the same as in a `.luau` file
#[test]
fn an_inherited_lint_takes_the_level_of_the_project() {
    let root = project(true);
    write(root.path(), "src/app.luaux", "local unused = <Frame/>\n");

    let config = std::fs::read_to_string(root.path().join("larvae.toml")).unwrap();
    write(
        root.path(),
        "larvae.toml",
        &format!("{config}\n[lint.rules]\nunused_variable = \"deny\"\n"),
    );

    let (ok, text) = run(root.path(), &["lint"]);

    assert!(!ok, "a denied lint has to fail the run: {text}");
}

/// An allow level turns an inherited lint off, and the file still lints
#[test]
fn an_inherited_lint_can_be_turned_off() {
    let root = project(true);
    write(root.path(), "src/app.luaux", "local unused = <Frame/>\n");

    let config = std::fs::read_to_string(root.path().join("larvae.toml")).unwrap();
    write(
        root.path(),
        "larvae.toml",
        &format!("{config}\n[lint.rules]\nunused_variable = \"allow\"\n"),
    );

    let (ok, text) = run(root.path(), &["lint"]);

    assert!(ok, "{text}");
    assert!(!text.contains("unused_variable"), "{text}");
}

/// A project keeps one inherited lint and drops the rest with `lints_only`
#[test]
fn a_project_can_inherit_only_the_lints_it_names() {
    let root = project(true);
    write(
        root.path(),
        "src/app.luaux",
        "local unused = 1\nlocal x = x\n",
    );

    let config = std::fs::read_to_string(root.path().join("larvae.toml")).unwrap();
    write(
        root.path(),
        "larvae.toml",
        &format!("{config}\n[worms.markup.inherit]\nlints_only = [\"unused_variable\"]\n"),
    );

    let (_, text) = run(root.path(), &["lint"]);

    assert!(text.contains("unused_variable"), "{text}");
    assert!(!text.contains("undefined_variable"), "{text}");
}

/// The same project, stated the other way round with `lints_except`
#[test]
fn a_project_can_drop_one_inherited_lint() {
    let root = project(true);
    write(root.path(), "src/app.luaux", "local unused = 1\n");

    let config = std::fs::read_to_string(root.path().join("larvae.toml")).unwrap();
    write(
        root.path(),
        "larvae.toml",
        &format!("{config}\n[worms.markup.inherit]\nlints_except = [\"unused_variable\"]\n"),
    );

    let (ok, text) = run(root.path(), &["lint"]);

    assert!(ok, "{text}");
    assert!(!text.contains("unused_variable"), "{text}");
}

/// The two lists are exclusive, because both together state two answers
#[test]
fn stating_both_lists_is_refused() {
    let root = project(true);
    write(root.path(), "src/app.luaux", "local x = 1\n");

    let config = std::fs::read_to_string(root.path().join("larvae.toml")).unwrap();
    write(
        root.path(),
        "larvae.toml",
        &format!(
            "{config}\n[worms.markup.inherit]\nlints_only = [\"shadowing\"]\nlints_except = [\"unused_variable\"]\n"
        ),
    );

    let (ok, text) = run(root.path(), &["lint"]);

    assert!(!ok, "{text}");
    assert!(text.contains("not both"), "{text}");
}
