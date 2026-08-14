//! These tests cover `larvae check` and the [check] analyses end to end.

use std::path::Path;
use std::process::Command;

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

fn run(dir: &Path) -> (bool, String) {
    let out = Command::new(bin())
        .arg("check")
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

/// A project on the path target, so the checks run with no mounts
fn path_project(dir: &Path, check: &str) {
    write(
        dir,
        "larvae.toml",
        &format!("[requires]\ntarget = \"path\"\n{check}"),
    );
}

// --- cycles ------------------------------------------------------------------

#[test]
fn a_require_cycle_names_both_members_and_warns() {
    let dir = tempfile::tempdir().unwrap();
    path_project(dir.path(), "");
    write(dir.path(), "src/a.luau", "return require(\"./b\")\n");
    write(
        dir.path(),
        "src/b.luau",
        "local a = require(\"./a\")\nreturn a\n",
    );

    let (ok, out) = run(dir.path());

    assert!(ok, "a warning does not fail the run: {out}");
    assert!(out.contains("require each other"), "{out}");
    assert!(out.contains("a.luau") && out.contains("b.luau"), "{out}");
}

/// The level lever: deny turns the same finding into a failing error
#[test]
fn cycles_raised_to_deny_fail_the_run() {
    let dir = tempfile::tempdir().unwrap();
    path_project(dir.path(), "[check]\ncycles = \"deny\"\n");
    write(dir.path(), "src/a.luau", "return require(\"./b\")\n");
    write(dir.path(), "src/b.luau", "return require(\"./a\")\n");

    let (ok, out) = run(dir.path());

    assert!(!ok, "{out}");
    assert!(out.contains("require each other"), "{out}");
}

#[test]
fn cycles_set_to_allow_say_nothing() {
    let dir = tempfile::tempdir().unwrap();
    path_project(dir.path(), "[check]\ncycles = \"allow\"\n");
    write(dir.path(), "src/a.luau", "return require(\"./b\")\n");
    write(dir.path(), "src/b.luau", "return require(\"./a\")\n");

    let (ok, out) = run(dir.path());

    assert!(ok, "{out}");
    assert!(!out.contains("require each other"), "{out}");
}

/// The graph keys a directory module and its init file on one node
#[test]
fn a_cycle_through_a_directory_module_is_found() {
    let dir = tempfile::tempdir().unwrap();
    path_project(dir.path(), "");
    write(
        dir.path(),
        "src/consumer.luau",
        "return require(\"./pkg\")\n",
    );
    write(
        dir.path(),
        "src/pkg/init.luau",
        "local c = require(\"./consumer\")\nreturn c\n",
    );

    let (ok, out) = run(dir.path());

    assert!(ok, "{out}");
    assert!(out.contains("require each other"), "{out}");
    assert!(
        out.contains("consumer.luau") && out.contains("pkg"),
        "{out}"
    );
}

// --- unused modules ----------------------------------------------------------

#[test]
fn an_unused_module_is_reported_when_asked() {
    let dir = tempfile::tempdir().unwrap();
    path_project(dir.path(), "[check]\nunused_modules = \"deny\"\n");
    write(
        dir.path(),
        "src/main.server.luau",
        "local u = require(\"./used\")\nprint(u)\n",
    );
    write(dir.path(), "src/used.luau", "return {}\n");
    write(dir.path(), "src/orphan.luau", "return {}\n");

    let (ok, out) = run(dir.path());

    assert!(!ok, "{out}");
    assert!(out.contains("nothing requires this module"), "{out}");
    assert!(out.contains("orphan.luau"), "{out}");
    // Roblox runs the server script, so it is never an unused module.
    assert!(!out.contains("main.server.luau"), "{out}");
}

/// The escape hatch: an entry in the config is a module the project runs
#[test]
fn an_entry_in_the_config_silences_the_unused_report() {
    let dir = tempfile::tempdir().unwrap();
    path_project(
        dir.path(),
        "[check]\nunused_modules = \"deny\"\nentries = [\"src/orphan.luau\"]\n",
    );
    write(dir.path(), "src/orphan.luau", "return {}\n");

    let (ok, out) = run(dir.path());

    assert!(ok, "{out}");
    assert!(!out.contains("nothing requires this module"), "{out}");
}

#[test]
fn unused_modules_stay_quiet_by_default() {
    let dir = tempfile::tempdir().unwrap();
    path_project(dir.path(), "");
    write(dir.path(), "src/orphan.luau", "return {}\n");

    let (ok, out) = run(dir.path());

    assert!(ok, "{out}");
    assert!(!out.contains("nothing requires this module"), "{out}");
}

// --- early require -----------------------------------------------------------

/// A client project on the instance target, with the given indexing style
fn instance_project(dir: &Path, style: &str) {
    write(
        dir,
        "larvae.toml",
        &format!(
            concat!(
                "[requires]\n",
                "target = \"roblox-instance\"\n",
                "{}\n",
                "[requires.mounts]\n",
                "src = \"@game/ReplicatedStorage/app\"\n",
            ),
            style
        ),
    );
    write(
        dir,
        "src/ui.client.luau",
        "local h = require(\"./helper\")\nprint(h)\n",
    );
    write(dir, "src/helper.luau", "return {}\n");
}

#[test]
fn a_client_script_that_does_not_wait_is_reported() {
    let dir = tempfile::tempdir().unwrap();
    instance_project(dir.path(), "indexing_style = \"find_first_child\"");

    let (ok, out) = run(dir.path());

    assert!(ok, "a warning does not fail the run: {out}");
    assert!(out.contains("without waiting for replication"), "{out}");
    assert!(out.contains("wait_for_child"), "{out}");
}

#[test]
fn wait_for_child_silences_the_early_require_check() {
    let dir = tempfile::tempdir().unwrap();
    instance_project(dir.path(), "indexing_style = \"wait_for_child\"");

    let (ok, out) = run(dir.path());

    assert!(ok, "{out}");
    assert!(!out.contains("without waiting for replication"), "{out}");
}

/// The other targets emit no indexing, so there is no race to report
#[test]
fn the_path_target_has_no_replication_race() {
    let dir = tempfile::tempdir().unwrap();
    path_project(dir.path(), "");
    write(
        dir.path(),
        "src/ui.client.luau",
        "local h = require(\"./helper\")\nprint(h)\n",
    );
    write(dir.path(), "src/helper.luau", "return {}\n");

    let (ok, out) = run(dir.path());

    assert!(ok, "{out}");
    assert!(!out.contains("without waiting for replication"), "{out}");
}
