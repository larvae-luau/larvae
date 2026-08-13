//! These tests cover `larvae lint` and the meaning of its exit code.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

fn bin() -> &'static Path {
    Path::new(env!("CARGO_BIN_EXE_larvae"))
}

fn write(dir: &Path, name: &str, body: &str) {
    let path = dir.join(name);

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("makes the directory");
    }

    std::fs::write(&path, body).expect("writes");
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

#[test]
fn a_clean_file_reports_nothing_and_succeeds() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "a.luau",
        "local function f(a)\n\treturn a\nend\n\nreturn f\n",
    );

    let (ok, out) = run(dir.path(), &["lint", "a.luau"]);

    assert!(ok, "{out}");
    assert!(out.contains("nothing to report"), "{out}");
}

/// The command reports a warning, but a warning does not fail the run.
#[test]
fn warnings_alone_still_exit_zero() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "a.luau", "local unused = 1\nreturn 1\n");

    let (ok, out) = run(dir.path(), &["lint", "a.luau"]);

    assert!(ok, "warnings should not fail the run: {out}");
    assert!(out.contains("unused_variable"), "{out}");
}

#[test]
fn an_error_fails_the_run() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "a.luau", "return notDefinedAnywhere\n");

    let (ok, out) = run(dir.path(), &["lint", "a.luau"]);

    assert!(!ok, "{out}");
    assert!(out.contains("undefined_variable"), "{out}");
}

/// A project sets a lint to deny to select what fails CI.
#[test]
fn a_warning_raised_to_deny_fails_the_run() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "larvae.toml",
        "[lint.rules]\nunused_variable = \"deny\"\n",
    );
    write(dir.path(), "a.luau", "local unused = 1\nreturn 1\n");

    let (ok, out) = run(dir.path(), &["lint", "a.luau"]);

    assert!(!ok, "{out}");
}

#[test]
fn a_lint_set_to_allow_says_nothing() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "larvae.toml",
        "[lint.rules]\nunused_variable = \"allow\"\n",
    );
    write(dir.path(), "a.luau", "local unused = 1\nreturn 1\n");

    let (ok, out) = run(dir.path(), &["lint", "a.luau"]);

    assert!(ok, "{out}");
    assert!(out.contains("nothing to report"), "{out}");
}

/// A project that already lints with selene does not have to rewrite its
/// config.
#[test]
fn an_existing_selene_config_is_honoured() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "selene.toml",
        "[rules]\nunused_variable = \"allow\"\n",
    );
    write(dir.path(), "a.luau", "local unused = 1\nreturn 1\n");

    let (ok, out) = run(dir.path(), &["lint", "a.luau"]);

    assert!(ok, "{out}");
    assert!(out.contains("nothing to report"), "{out}");
}

#[test]
fn project_globals_stop_a_name_being_undefined() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "larvae.toml",
        "[lint]\nglobals = [\"MyFramework\"]\n",
    );
    write(dir.path(), "a.luau", "return MyFramework\n");

    let (ok, out) = run(dir.path(), &["lint", "a.luau"]);

    assert!(ok, "{out}");
}

#[test]
fn a_file_that_does_not_parse_is_reported_rather_than_crashing() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "bad.luau", "local = = =\n");

    let (ok, out) = run(dir.path(), &["lint", "bad.luau"]);

    assert!(!ok);
    assert!(out.contains("syntax error"), "{out}");
}

/// One unparsable file must not stop the lint of the other files.
#[test]
fn a_broken_file_does_not_block_its_neighbours() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "bad.luau", "local = = =\n");
    write(dir.path(), "good.luau", "local unused = 1\nreturn 1\n");

    let (_, out) = run(dir.path(), &["lint", "."]);

    assert!(out.contains("syntax error"), "{out}");
    assert!(out.contains("unused_variable"), "{out}");
}

#[test]
fn stdin_lints_one_file() {
    let mut child = Command::new(bin())
        .args(["lint", "--stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("runs");

    child
        .stdin
        .as_mut()
        .expect("has stdin")
        .write_all(b"local unused = 1\nreturn 1\n")
        .expect("writes");

    let out = child.wait_with_output().expect("finishes");

    assert!(String::from_utf8_lossy(&out.stdout).contains("unused_variable"));
}

#[test]
fn explain_describes_one_lint() {
    let dir = tempfile::tempdir().unwrap();

    let (ok, out) = run(dir.path(), &["lint", "--explain", "divide_by_zero"]);

    assert!(ok, "{out}");
    assert!(out.contains("dividing by a literal zero"), "{out}");
}

#[test]
fn explaining_an_unknown_lint_lists_the_real_ones() {
    let dir = tempfile::tempdir().unwrap();

    let (ok, out) = run(dir.path(), &["lint", "--explain", "no_such_lint"]);

    assert!(!ok);
    assert!(
        out.contains("divide_by_zero"),
        "should list what exists: {out}"
    );
}

#[test]
fn a_suppression_comment_is_honoured_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "a.luau",
        "-- larvae: allow(unused_variable)\nlocal unused = 1\nreturn 1\n",
    );

    let (ok, out) = run(dir.path(), &["lint", "a.luau"]);

    assert!(ok, "{out}");
    assert!(out.contains("nothing to report"), "{out}");
}

/// larvae obeys selene's own exclude list, and [lint] adds to it.
#[test]
fn excluded_paths_are_walked_past() {
    let dir = tempfile::tempdir().unwrap();
    let bad = "local unused = 1\nreturn 1\n";

    write(dir.path(), "selene.toml", "exclude = [\"src/Packages\"]\n");
    write(
        dir.path(),
        "larvae.toml",
        "[process]\ninput = \"src\"\n\n[lint]\nexclude = [\"**/*.gen.luau\"]\n",
    );
    write(dir.path(), "src/Packages/vendor.luau", bad);
    write(dir.path(), "src/made.gen.luau", bad);
    write(dir.path(), "src/mine.luau", bad);

    let (_, out) = run(dir.path(), &["lint"]);

    assert!(out.contains("mine.luau"), "{out}");
    assert!(!out.contains("vendor.luau"), "selene's exclude: {out}");
    assert!(!out.contains("made.gen.luau"), "larvae's exclude: {out}");
    assert!(
        out.contains("1 file"),
        "only the one file was linted: {out}"
    );
}

/// A named file is an explicit request, so the exclude list does not apply.
#[test]
fn an_excluded_file_is_still_linted_when_named() {
    let dir = tempfile::tempdir().unwrap();

    write(
        dir.path(),
        "larvae.toml",
        "[lint]\nexclude = [\"src/Packages\"]\n",
    );
    write(
        dir.path(),
        "src/Packages/vendor.luau",
        "local unused = 1\nreturn 1\n",
    );

    let (_, out) = run(dir.path(), &["lint", "src/Packages/vendor.luau"]);

    assert!(out.contains("unused_variable"), "{out}");
}
