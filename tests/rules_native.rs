//! Pipeline tests for larvae's own rules, before and after on a real project

use std::fs;
use std::path::Path;

use larvae::config::Config;
use larvae::diag::Severity;
use larvae::pipeline;

fn write(root: &Path, rel: &str, content: &str) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, content).unwrap();
}

fn read(root: &Path, rel: &str) -> String {
    fs::read_to_string(root.join(rel)).unwrap()
}

/*
A Rojo shaped project so every file under src has a datamodel path, the
rules under test are the only ones enabled, `rules` is the body of [rules]
*/
fn project(rules: &str) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();

    write(
        root,
        "default.project.json",
        r#"{
            "name": "fixture",
            "tree": {
                "$className": "DataModel",
                "ReplicatedStorage": {
                    "shared": { "$path": "src/shared" }
                },

                "ServerScriptService": { "$path": "src/server" }
            }
        }"#,
    );
    write(
        root,
        "larvae.toml",
        &format!("[aliases]\npkg = \"@game/ReplicatedStorage/shared/util\"\n\n[rules]\n{rules}\n"),
    );
    write(root, "src/shared/util/math.luau", "return {}\n");

    tmp
}

/// Run the pipeline in write mode and fail loudly on any error diagnostic
fn build(root: &Path) -> Vec<larvae::diag::Diag> {
    let config = Config::load_or_default(root).unwrap();
    let outcome = pipeline::run(root, &config, true).unwrap();

    for d in &outcome.diags {
        eprintln!("{d}");
    }

    assert!(!outcome.has_errors(), "unexpected errors");

    outcome.diags
}

/// Output lines must line up with input lines, that is the retain-lines promise
fn same_line_count(before: &str, after: &str) {
    assert_eq!(
        before.lines().count(),
        after.lines().count(),
        "line count changed\nbefore:\n{before}\nafter:\n{after}"
    );
}

#[test]
fn remove_calls_drops_statement_calls_only() {
    let tmp = project("remove_calls = [\"print\", \"debug.profilebegin\"]");
    let root = tmp.path();

    let before = concat!(
        "--!strict\n",
        "print(\"loading\")\n",
        "debug.profilebegin(\"work\")\n",
        "local n = 1\n",
        "if n > 0 then\n",
        "    print(n)\n",
        "end\n",
        "local shown = print(n)\n",
        "print(compute())\n",
        "warn(\"kept\")\n",
        "return shown\n",
    );
    write(root, "src/shared/mod.luau", before);
    build(root);

    let after = read(root, "dist/shared/mod.luau");
    same_line_count(before, &after);
    // statement calls are gone, nested blocks included
    assert!(!after.contains("print(\"loading\")"), "{after}");
    assert!(!after.contains("debug.profilebegin"), "{after}");
    assert!(!after.contains("    print(n)\n"), "{after}");
    // the value is used, the call has to stay
    assert!(after.contains("local shown = print(n)"), "{after}");
    // the argument calls something, that call may be the point of the line
    assert!(after.contains("print(compute())"), "{after}");
    // not on the list
    assert!(after.contains("warn(\"kept\")"), "{after}");
}

#[test]
fn remove_calls_can_ignore_argument_side_effects() {
    let tmp = project(
        "remove_calls = { functions = [\"print\"], preserve_arguments_side_effects = false }",
    );
    let root = tmp.path();
    write(root, "src/shared/mod.luau", "print(compute())\nreturn 1\n");
    build(root);
    assert_eq!(read(root, "dist/shared/mod.luau"), "\nreturn 1\n");
}

#[test]
fn use_get_service_rewrites_known_services() {
    let tmp = project("use_get_service = true");
    let root = tmp.path();

    let before = concat!(
        "local rs = game.ReplicatedStorage\n",
        "local player = game.Players.LocalPlayer\n",
        "local folder = game.SomeFolder\n",
        "return rs, player, folder\n",
    );
    write(root, "src/shared/mod.luau", before);
    build(root);

    let after = read(root, "dist/shared/mod.luau");
    same_line_count(before, &after);
    assert!(after.contains("local rs = game:GetService(\"ReplicatedStorage\")"));
    assert!(after.contains("local player = game:GetService(\"Players\").LocalPlayer"));
    // not a service, it stays a plain property
    assert!(after.contains("local folder = game.SomeFolder"));
}

#[test]
fn use_get_service_skips_a_file_that_shadows_game() {
    let tmp = project("use_get_service = true");
    let root = tmp.path();

    let before = concat!(
        "local game = { Players = {} }\n",
        "local p = game.Players\n",
        "return p\n",
    );
    write(root, "src/shared/mod.luau", before);
    build(root);
    // the local wins over the global, rewriting it would call a table
    assert_eq!(read(root, "dist/shared/mod.luau"), before);
}

#[test]
fn dedupe_requires_folds_specs_that_resolve_together() {
    let tmp = project("dedupe_requires = true");
    let root = tmp.path();

    // two different specs, the rewriter lands both on the same @game path
    let before = concat!(
        "local A = require(\"@pkg/math\")\n",
        "local B = require(\"@game/ReplicatedStorage/shared/util/math\")\n",
        "return A, B\n",
    );
    write(root, "src/server/main.server.luau", before);
    build(root);

    let after = read(root, "dist/server/main.server.luau");
    same_line_count(before, &after);
    assert!(
        after.contains("local A = require(\"@game/ReplicatedStorage/shared/util/math\")"),
        "{after}"
    );
    assert!(after.contains("local B = A\n"), "{after}");
    assert!(!after.contains("local B = require"), "{after}");
}

#[test]
fn dedupe_requires_leaves_distinct_modules_alone() {
    let tmp = project("dedupe_requires = true");
    let root = tmp.path();

    write(root, "src/shared/util/vec.luau", "return {}\n");
    let before = concat!(
        "local A = require(\"./util/math\")\n",
        "local B = require(\"./util/vec\")\n",
        "do\n",
        "    local C = require(\"./util/math\")\n",
        "    print(C)\n",
        "end\n",
        "return A, B\n",
    );
    write(root, "src/shared/mod.luau", before);
    build(root);
    // different modules, and the nested one is not a top level statement
    assert_eq!(read(root, "dist/shared/mod.luau"), before);
}

#[test]
fn inject_module_path_defines_the_constant() {
    let tmp = project("inject_module_path = \"MODULE_PATH\"");
    let root = tmp.path();

    let before = concat!(
        "--!strict\n",
        "-- header\n",
        "local math = require(\"@pkg/math\")\n",
        "return { path = MODULE_PATH, math = math }\n",
    );
    write(root, "src/shared/mod.luau", before);
    build(root);

    let after = read(root, "dist/shared/mod.luau");
    // after the directive and the header comment, one line longer
    assert_eq!(
        after,
        concat!(
            "--!strict\n",
            "-- header\n",
            "local MODULE_PATH = \"@game/ReplicatedStorage/shared/mod\"\n",
            "local math = require(\"@game/ReplicatedStorage/shared/util/math\")\n",
            "return { path = MODULE_PATH, math = math }\n",
        )
    );
}

#[test]
fn inject_module_path_stays_quiet_or_warns() {
    let tmp = project("inject_module_path = \"MODULE_PATH\"");
    let root = tmp.path();

    // never referenced, nothing to inject
    write(root, "src/shared/quiet.luau", "return 1\n");
    // referenced but no mount covers this directory
    write(root, "src/loose/thing.luau", "return MODULE_PATH\n");
    let diags = build(root);

    assert_eq!(read(root, "dist/shared/quiet.luau"), "return 1\n");
    assert_eq!(read(root, "dist/loose/thing.luau"), "return MODULE_PATH\n");
    assert!(
        diags
            .iter()
            .any(|d| d.severity == Severity::Warning
                && d.message.contains("no mount covers this file")),
        "{diags:?}"
    );
}

#[test]
fn freeze_module_wraps_only_what_it_can_prove() {
    let tmp = project("freeze_module = true");
    let root = tmp.path();
    let safe = concat!("local M = {\n", "    answer = 42,\n", "}\n", "return M\n",);

    write(root, "src/shared/safe.luau", safe);
    let methods = concat!("local M = {}\n", "function M.run() end\n", "return M\n",);
    write(root, "src/shared/methods.luau", methods);
    write(root, "src/shared/literal.luau", "return { a = 1 }\n");
    build(root);

    let after = read(root, "dist/shared/safe.luau");
    same_line_count(safe, &after);
    assert!(after.ends_with("return table.freeze(M)\n"), "{after}");
    // a method table is written to after it is built, freezing it would throw
    assert_eq!(read(root, "dist/shared/methods.luau"), methods);
    assert_eq!(
        read(root, "dist/shared/literal.luau"),
        "return table.freeze({ a = 1 })\n"
    );
}

#[test]
fn rules_off_by_default() {
    let tmp = project("");
    let root = tmp.path();

    let before = concat!(
        "print(\"kept\")\n",
        "local rs = game.ReplicatedStorage\n",
        "local A = require(\"@pkg/math\")\n",
        "local B = require(\"@pkg/math\")\n",
        "return { rs, A, B, MODULE_PATH }\n",
    );
    write(root, "src/shared/mod.luau", before);
    build(root);

    let after = read(root, "dist/shared/mod.luau");
    // only the require rewriting ran
    assert!(after.contains("print(\"kept\")"));
    assert!(after.contains("game.ReplicatedStorage"));
    assert!(!after.contains("MODULE_PATH ="));
    assert_eq!(
        after
            .matches("require(\"@game/ReplicatedStorage/shared/util/math\")")
            .count(),
        2
    );
}
