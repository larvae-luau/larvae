//! End to end require rewriting, the flagship path

use larvae::config::Config;
use larvae::diag::Severity;
use larvae::pipeline;
use std::fs;

mod common;
use common::*;

#[test]
fn processes_fixture_end_to_end() {
    let tmp = fixture();
    let root = tmp.path();
    let config = Config::load_or_default(root).unwrap();
    let outcome = pipeline::run(root, &config, true).unwrap();

    for d in &outcome.diags {
        eprintln!("{d}");
    }

    assert!(!outcome.has_errors(), "unexpected errors");

    // Alias expanded to a native @game require
    let main = read(root, "dist/server/main.server.luau");
    assert!(
        main.contains(r#"require("@game/ReplicatedStorage/Packages/signal")"#),
        "alias not expanded: {main}"
    );
    // Cross mount relative went absolute
    assert!(
        main.contains(r#"require("@game/ReplicatedStorage/shared/util/math")"#),
        "cross-mount require not rewritten: {main}"
    );
    // Comment untouched (splice preserves all other bytes)
    assert!(main.contains("require(\"./inside-comment\")"));
    // Trailing content preserved
    assert!(main.contains("print(Signal, math)"));

    // Same mount sibling stays relative (identical -> byte-identical output)
    let geometry = read(root, "dist/shared/util/geometry.luau");
    assert_eq!(geometry, "local math = require(\"./math\")\nreturn math\n");

    // @self pass-through
    let init = read(root, "dist/shared/pkg/init.luau");
    assert!(init.contains(r#"require("@self/sub")"#));

    // Sibling require of a directory module stays relative
    let consumer = read(root, "dist/shared/consumer.luau");
    assert_eq!(consumer, "return require(\"./pkg\")\n");

    // Non code file copied
    assert_eq!(read(root, "dist/shared/data.json"), "{\"k\":1}\n");

    // Derived build project generated with rerelativized paths
    let bp = outcome.build_project.expect("derived build project");
    let bp_json: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&bp).unwrap()).unwrap();
    assert_eq!(
        bp_json["tree"]["ReplicatedStorage"]["shared"]["$path"],
        "../dist/shared"
    );
    assert_eq!(
        bp_json["tree"]["ReplicatedStorage"]["Packages"]["$path"],
        "../Packages"
    );
    assert_eq!(
        bp_json["tree"]["ServerScriptService"]["$path"],
        "../dist/server"
    );
}

#[test]
fn idempotent_reprocessing() {
    let tmp = fixture();
    let root = tmp.path();
    let config = Config::load_or_default(root).unwrap();

    pipeline::run(root, &config, true).unwrap();
    let first = read(root, "dist/server/main.server.luau");

    // Process the dist tree as input, already-native requires pass through
    write(root, "src/server/main.server.luau", &first);
    let outcome = pipeline::run(root, &config, true).unwrap();

    for d in &outcome.diags {
        eprintln!("{d}");
    }

    assert!(!outcome.has_errors());
    assert_eq!(read(root, "dist/server/main.server.luau"), first);
}

#[test]
fn unknown_alias_is_error() {
    let tmp = fixture();
    let root = tmp.path();

    write(root, "src/shared/bad.luau", "return require(\"@nope/x\")\n");

    let config = Config::load_or_default(root).unwrap();
    let outcome = pipeline::run(root, &config, false).unwrap();

    assert!(outcome.has_errors());
    assert!(
        outcome
            .diags
            .iter()
            .any(|d| d.severity == Severity::Error && d.message.contains("unknown alias @nope"))
    );
}

#[test]
fn client_requiring_server_is_error() {
    let tmp = fixture();
    let root = tmp.path();

    // Client-marked script requiring a server only module
    write(root, "src/server/secret.luau", "return {}\n");
    write(
        root,
        "src/shared/ui.client.luau",
        "return require(\"@game/ServerScriptService/secret\")\n",
    );

    let config = Config::load_or_default(root).unwrap();
    let outcome = pipeline::run(root, &config, false).unwrap();

    assert!(outcome.has_errors());
    assert!(
        outcome
            .diags
            .iter()
            .any(|d| d.message.contains("does not replicate")
                || d.message.contains("cannot require from"))
    );
}

#[test]
fn absolute_into_starter_container_is_error() {
    let tmp = fixture();
    let root = tmp.path();

    write(
        root,
        "src/shared/bad_starter.luau",
        "return require(\"@game/StarterGui/hud/logic\")\n",
    );

    let config = Config::load_or_default(root).unwrap();
    let outcome = pipeline::run(root, &config, false).unwrap();

    assert!(outcome.has_errors());
    assert!(outcome.diags.iter().any(|d| d.message.contains("clones")));
}

#[test]
fn unprefixed_require_is_error() {
    let tmp = fixture();
    let root = tmp.path();

    write(
        root,
        "src/shared/legacy.luau",
        "return require(\"sibling\")\n",
    );

    let config = Config::load_or_default(root).unwrap();
    let outcome = pipeline::run(root, &config, false).unwrap();

    assert!(outcome.has_errors());
    assert!(
        outcome
            .diags
            .iter()
            .any(|d| d.message.contains("not RFC-valid"))
    );
}

#[test]
fn missing_target_warns_then_errors_under_strict() {
    let tmp = fixture();
    let root = tmp.path();

    write(
        root,
        "src/shared/dangling.luau",
        "return require(\"./ghost\")\n",
    );

    let config = Config::load_or_default(root).unwrap();
    let outcome = pipeline::run(root, &config, false).unwrap();

    assert!(
        !outcome.has_errors(),
        "missing target should be a warning by default"
    );
    assert!(
        outcome
            .diags
            .iter()
            .any(|d| d.severity == Severity::Warning)
    );

    write(
        root,
        "larvae.toml",
        r#"
            [aliases]
            pkg = "@game/ReplicatedStorage/Packages"
            [requires]
            strict = true
        "#,
    );

    let config = Config::load_or_default(root).unwrap();
    let outcome = pipeline::run(root, &config, false).unwrap();

    assert!(
        outcome.has_errors(),
        "strict should upgrade missing-target to error"
    );
}

#[test]
fn luaurc_aliases_work_zero_config() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();

    write(
        root,
        "default.project.json",
        r#"{
            "name": "z",
            "tree": {
                "$className": "DataModel",
                "ReplicatedStorage": { "app": { "$path": "src" } }
            }
        }"#,
    );
    write(
        root,
        ".luaurc",
        r#"{ "aliases": { "util": "./src/util" } }"#,
    );
    write(root, "src/util/list.luau", "return {}\n");
    write(root, "src/main.luau", "return require(\"@util/list\")\n");

    // No larvae.toml at all
    let config = Config::load_or_default(root).unwrap();
    let outcome = pipeline::run(root, &config, true).unwrap();

    for d in &outcome.diags {
        eprintln!("{d}");
    }

    assert!(!outcome.has_errors());
    let main = read(root, "dist/main.luau");
    // util maps into the same mount -> relative require
    assert_eq!(main, "return require(\"./util/list\")\n");
}

#[test]
fn instance_target_find_first_child() {
    let tmp = instance_fixture("find_first_child");
    let root = tmp.path();
    let config = Config::load_or_default(root).unwrap();
    let outcome = pipeline::run(root, &config, true).unwrap();

    for d in &outcome.diags {
        eprintln!("{d}");
    }

    assert!(!outcome.has_errors());

    let main = read(root, "dist/server/main.server.luau");
    // Alias with a @game value becomes an absolute chain
    assert!(
        main.contains(
            r#"require(game:GetService("ReplicatedStorage"):FindFirstChild("Packages"):FindFirstChild("signal"))"#
        ),
        "alias not converted: {main}"
    );
    // Cross mount relative goes absolute too
    assert!(main.contains(
        r#"require(game:GetService("ReplicatedStorage"):FindFirstChild("shared"):FindFirstChild("util"):FindFirstChild("math"))"#
    ));

    // Same mount sibling becomes a script relative chain
    let geometry = read(root, "dist/shared/util/geometry.luau");
    assert!(
        geometry.contains(r#"require(script.Parent:FindFirstChild("math"))"#),
        "{geometry}"
    );

    // @self resolves to a child of the script
    let init = read(root, "dist/shared/pkg/init.luau");
    assert!(
        init.contains(r#"require(script:FindFirstChild("sub"))"#),
        "{init}"
    );
}

#[test]
fn instance_target_wait_for_child() {
    let tmp = instance_fixture("wait_for_child");
    let root = tmp.path();
    let config = Config::load_or_default(root).unwrap();
    let outcome = pipeline::run(root, &config, true).unwrap();

    assert!(!outcome.has_errors());
    let geometry = read(root, "dist/shared/util/geometry.luau");
    assert!(
        geometry.contains(r#"require(script.Parent:WaitForChild("math"))"#),
        "{geometry}"
    );
}

#[test]
fn instance_target_property_style() {
    let tmp = instance_fixture("property");
    let root = tmp.path();

    // A parenless require must get wrapped in parens
    write(
        root,
        "src/shared/parenless.luau",
        "return require \"./util/math\"\n",
    );

    let config = Config::load_or_default(root).unwrap();
    let outcome = pipeline::run(root, &config, true).unwrap();

    for d in &outcome.diags {
        eprintln!("{d}");
    }

    assert!(!outcome.has_errors());

    let main = read(root, "dist/server/main.server.luau");
    assert!(
        main.contains("require(game.ReplicatedStorage.Packages.signal)"),
        "{main}"
    );
    let geometry = read(root, "dist/shared/util/geometry.luau");
    assert!(
        geometry.contains("require(script.Parent.math)"),
        "{geometry}"
    );

    let parenless = read(root, "dist/shared/parenless.luau");
    assert!(
        parenless.contains("require (script.Parent.util.math)"),
        "{parenless}"
    );
}

#[test]
fn instance_style_accepts_kebab_alias() {
    let tmp = instance_fixture("property-instance");
    let config = Config::load_or_default(tmp.path()).unwrap();

    assert_eq!(
        config.requires.indexing_style,
        Some(larvae::config::IndexingStyle::Property)
    );
}

#[test]
fn indexing_style_requires_instance_target() {
    let tmp = fixture();
    let root = tmp.path();

    write(
        root,
        "larvae.toml",
        "[requires]\nindexing_style = \"property\"\n",
    );
    assert!(Config::load_or_default(root).is_err());
}

#[test]
fn path_target_for_lune() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();

    write(root, ".luaurc", r#"{ "aliases": { "lib": "./lib" } }"#);
    write(root, "lib/json.luau", "return {}\n");
    write(root, "larvae.toml", "[requires]\ntarget = \"path\"\n");
    write(root, "src/main.luau", "return require(\"@lib/json\")\n");

    let config = Config::load_or_default(root).unwrap();
    let outcome = pipeline::run(root, &config, true).unwrap();

    for d in &outcome.diags {
        eprintln!("{d}");
    }

    assert!(!outcome.has_errors());
    assert_eq!(
        read(root, "dist/main.luau"),
        "return require(\"../lib/json\")\n"
    );
}
