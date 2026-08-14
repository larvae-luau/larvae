//! These tests run `larvae process` with a native worm that declares a
//! transform rule. The rules of a native worm cross once per file, as one
//! batched request, and never once per node.

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
This is a native rule worm in a few lines of python.

It declares one rule with a `String` filter. Its rules op receives the whole
batch of one file: every enabled rule, each with the id, the kind name, and
the byte span of every matched node. The `body` of each fixture answers the
ops after init, so each test states its own behavior.
*/
fn install_worm(root: &Path, body: &str) {
    write(
        root,
        "wormdir/worm.toml",
        r#"
name  = "shouty"
api   = 1
form  = "native"
entry = "worm.py"

[rules.shout_strings]
default = false
filter = ["String"]
"#,
    );

    let script = write(
        root,
        "wormdir/worm.py",
        &format!(
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
    else:
{body}
"#
        ),
    );

    {
        use std::os::unix::fs::PermissionsExt;

        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

/// A project whose config turns the rule of the worm on
fn project(body: &str) -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();

    write(
        root.path(),
        "larvae.toml",
        concat!(
            "[process]\ninput = \"src\"\noutput = \"dist\"\n\n",
            "[requires]\ntarget = \"path\"\n\n",
            "[worms.shouty]\npath = \"wormdir\"\nrules = { shout_strings = true }\n",
        ),
    );

    install_worm(root.path(), body);

    root
}

/// The whole path in one test: the batch crosses, the worm answers, and the
/// edit reaches the output file
#[test]
fn a_batched_rule_edit_lands_in_the_output() {
    let root = project(
        r#"        src = req["source"]
        edits = []
        for rule in req["rules"]:
            for node in rule["nodes"]:
                s, e = node["span"]
                edits.append([s, e, src[s:e].upper()])
        send({"ok": True, "edits": edits})"#,
    );

    write(
        root.path(),
        "src/app.luau",
        "local x = \"hello\"\nlocal y = 1\nreturn x\n",
    );

    let (ok, text) = run(root.path(), &["process"]);

    assert!(ok, "{text}");

    let out = std::fs::read_to_string(root.path().join("dist/app.luau")).unwrap();

    assert!(out.contains("\"HELLO\""), "the rule did not run: {out}");
    assert!(out.contains("local y = 1"), "the rule over reached: {out}");
}

/// The filter is the cost control: a node kind the rule did not ask for must
/// not cross, so the worm sees only its matched kinds
#[test]
fn the_batch_carries_only_the_filtered_kinds() {
    let root = project(
        r#"        kinds = [n["kind"] for r in req["rules"] for n in r["nodes"]]
        if kinds and set(kinds) != {"String"}:
            send({"ok": False, "error": "unexpected kinds " + ",".join(kinds)})
        else:
            send({"ok": True, "edits": []})"#,
    );

    write(
        root.path(),
        "src/app.luau",
        "local x = \"hello\"\nreturn x\n",
    );

    let (ok, text) = run(root.path(), &["process"]);

    assert!(ok, "{text}");
}

/// An edit span off the source fails this file, and the report names the worm
#[test]
fn an_edit_span_off_the_source_fails_the_file_by_name() {
    let root = project(r#"        send({"ok": True, "edits": [[0, 9999, "y"]]})"#);

    write(
        root.path(),
        "src/app.luau",
        "local x = \"hello\"\nreturn x\n",
    );

    let (ok, text) = run(root.path(), &["process"]);

    assert!(!ok, "a bad span has to fail the run: {text}");
    assert!(text.contains("shouty"), "{text}");
    assert!(text.contains("0..9999"), "{text}");
}
