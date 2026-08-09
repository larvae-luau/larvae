//! `larvae fmt`, the three ways it can be run

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

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

#[test]
fn a_file_is_formatted_in_place() {
    let dir = tempfile::tempdir().unwrap();
    let path = write(dir.path(), "a.luau", "local x={a=1}\n");

    let (ok, _) = run(dir.path(), &["fmt", "a.luau"]);

    assert!(ok);
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "local x = { a = 1 }\n"
    );
}

#[test]
fn a_directory_is_walked() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "src/a.luau", "local x={a=1}\n");
    write(dir.path(), "src/deep/b.luau", "local y={b=2}\n");

    let (ok, out) = run(dir.path(), &["fmt", "src"]);

    assert!(ok, "{out}");
    assert!(out.contains("2 files"), "{out}");
}

/// A build output is not the project's to format
#[test]
fn generated_directories_are_skipped() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "src/a.luau", "local x = { a = 1 }\n");
    write(dir.path(), ".larvae/worms/w/x.luau", "local x={a=1}\n");
    write(dir.path(), "node_modules/p/y.luau", "local x={a=1}\n");

    let (ok, out) = run(dir.path(), &["fmt", "--check", "."]);

    assert!(ok, "{out}");
}

#[test]
fn check_writes_nothing_and_fails_when_a_file_would_change() {
    let dir = tempfile::tempdir().unwrap();
    let path = write(dir.path(), "a.luau", "local x={a=1}\n");

    let (ok, out) = run(dir.path(), &["fmt", "--check", "a.luau"]);

    assert!(!ok, "should have failed");
    assert!(out.contains("would reformat"), "{out}");
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "local x={a=1}\n",
        "check must not write"
    );
}

#[test]
fn check_succeeds_on_an_already_formatted_tree() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "a.luau", "local x = { a = 1 }\n");

    let (ok, out) = run(dir.path(), &["fmt", "--check", "a.luau"]);

    assert!(ok, "{out}");
}

/// The path an editor calls on every save
#[test]
fn stdin_writes_the_result_to_stdout() {
    let mut child = Command::new(bin())
        .args(["fmt", "--stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("runs");

    child
        .stdin
        .as_mut()
        .expect("has stdin")
        .write_all(b"local x={a=1}\n")
        .expect("writes");

    let out = child.wait_with_output().expect("finishes");

    assert!(out.status.success());
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "local x = { a = 1 }\n"
    );
}

#[test]
fn a_file_that_does_not_parse_is_reported_and_left_alone() {
    let dir = tempfile::tempdir().unwrap();
    let path = write(dir.path(), "bad.luau", "local = = =\n");

    let (ok, out) = run(dir.path(), &["fmt", "bad.luau"]);

    assert!(!ok, "should have failed");
    assert!(out.contains("bad.luau"), "{out}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "local = = =\n");
}

/// One bad file must not stop the rest of the run
#[test]
fn a_broken_file_does_not_block_its_neighbours() {
    let dir = tempfile::tempdir().unwrap();
    let good = write(dir.path(), "good.luau", "local x={a=1}\n");
    write(dir.path(), "bad.luau", "local = = =\n");

    let (ok, _) = run(dir.path(), &["fmt", "."]);

    assert!(!ok, "the run should report the failure");
    assert_eq!(
        std::fs::read_to_string(&good).unwrap(),
        "local x = { a = 1 }\n",
        "the good file should still have been formatted"
    );
}

#[test]
fn a_missing_path_is_an_error() {
    let dir = tempfile::tempdir().unwrap();

    let (ok, out) = run(dir.path(), &["fmt", "nope.luau"]);

    assert!(!ok);
    assert!(out.contains("no such path"), "{out}");
}

// --- configuration ---------------------------------------------------------

#[test]
fn the_fmt_table_in_larvae_toml_is_used() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "larvae.toml",
        "[fmt]\nindent_type = \"spaces\"\nindent_width = 2\n",
    );

    let path = write(dir.path(), "a.luau", "do\nx()\nend\n");
    run(dir.path(), &["fmt", "a.luau"]);

    assert_eq!(std::fs::read_to_string(&path).unwrap(), "do\n  x()\nend\n");
}

/// A project already using stylua should switch without editing anything
#[test]
fn an_existing_stylua_config_is_honoured() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "stylua.toml",
        "indent_type = \"Spaces\"\nindent_width = 3\n",
    );

    let path = write(dir.path(), "a.luau", "do\nx()\nend\n");
    run(dir.path(), &["fmt", "a.luau"]);

    assert_eq!(std::fs::read_to_string(&path).unwrap(), "do\n   x()\nend\n");
}

#[test]
fn larvae_toml_wins_over_stylua_toml() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "stylua.toml",
        "indent_type = \"Spaces\"\nindent_width = 3\n",
    );
    write(dir.path(), "larvae.toml", "[fmt]\nindent_width = 5\n");

    let path = write(dir.path(), "a.luau", "do\nx()\nend\n");
    run(dir.path(), &["fmt", "a.luau"]);

    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "do\n     x()\nend\n",
        "spaces from stylua, width from larvae"
    );
}

/// Without arguments the project's own input directory is what gets formatted
#[test]
fn the_project_input_is_the_default_target() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "larvae.toml", "[process]\ninput = \"src\"\n");

    let inside = write(dir.path(), "src/a.luau", "local x={a=1}\n");
    let outside = write(dir.path(), "other/b.luau", "local y={b=2}\n");

    run(dir.path(), &["fmt"]);

    assert_eq!(
        std::fs::read_to_string(&inside).unwrap(),
        "local x = { a = 1 }\n"
    );
    assert_eq!(
        std::fs::read_to_string(&outside).unwrap(),
        "local y={b=2}\n",
        "outside the configured input, so untouched"
    );
}

/// A walk passes over what [fmt] excluded, whether it starts at the project
/// input or at a directory somebody named
#[test]
fn an_excluded_path_is_walked_past() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "larvae.toml",
        "[process]\ninput = \"src\"\n\n[fmt]\nexclude = [\"src/vendor\", \"**/*.gen.luau\"]\n",
    );

    let mine = write(dir.path(), "src/a.luau", "local x={a=1}\n");
    let vendored = write(dir.path(), "src/vendor/b.luau", "local y={b=2}\n");
    let generated = write(dir.path(), "src/c.gen.luau", "local z={c=3}\n");

    run(dir.path(), &["fmt"]);

    assert_eq!(
        std::fs::read_to_string(&mine).unwrap(),
        "local x = { a = 1 }\n"
    );
    assert_eq!(
        std::fs::read_to_string(&vendored).unwrap(),
        "local y={b=2}\n",
        "a named directory takes what is under it"
    );
    assert_eq!(
        std::fs::read_to_string(&generated).unwrap(),
        "local z={c=3}\n"
    );

    run(dir.path(), &["fmt", "src"]);

    assert_eq!(
        std::fs::read_to_string(&vendored).unwrap(),
        "local y={b=2}\n",
        "naming the directory above it changes nothing"
    );
}

/// Naming the file is saying you meant it, exclude or no exclude
#[test]
fn an_excluded_file_is_still_formatted_when_named() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "larvae.toml",
        "[fmt]\nexclude = [\"src/vendor\"]\n",
    );

    let vendored = write(dir.path(), "src/vendor/b.luau", "local y={b=2}\n");

    let (ok, out) = run(dir.path(), &["fmt", "src/vendor/b.luau"]);

    assert!(ok, "{out}");
    assert_eq!(
        std::fs::read_to_string(&vendored).unwrap(),
        "local y = { b = 2 }\n"
    );
}

#[test]
fn a_broken_exclude_glob_is_reported() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "larvae.toml", "[fmt]\nexclude = [\"a[\"]\n");
    write(dir.path(), "a.luau", "local x={a=1}\n");

    let (ok, out) = run(dir.path(), &["fmt", "a.luau"]);

    assert!(!ok);
    assert!(out.contains("exclude"), "should name the key: {out}");
}
