//! These tests cover end-to-end require rewriting, the primary feature.

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

    // larvae expands the alias to a native @game require.
    let main = read(root, "dist/server/main.server.luau");
    assert!(
        main.contains(r#"require("@game/ReplicatedStorage/Packages/signal")"#),
        "alias not expanded: {main}"
    );
    // larvae rewrites the cross-mount relative require to an absolute require.
    assert!(
        main.contains(r#"require("@game/ReplicatedStorage/shared/util/math")"#),
        "cross-mount require not rewritten: {main}"
    );
    // The comment is unchanged, because the splice keeps all other bytes.
    assert!(main.contains("require(\"./inside-comment\")"));
    // The trailing content stays in the output.
    assert!(main.contains("print(Signal, math)"));

    // A sibling in the same mount stays relative, so the output is byte-identical.
    let geometry = read(root, "dist/shared/util/geometry.luau");
    assert_eq!(geometry, "local math = require(\"./math\")\nreturn math\n");

    // larvae passes @self through unchanged.
    let init = read(root, "dist/shared/pkg/init.luau");
    assert!(init.contains(r#"require("@self/sub")"#));

    // A sibling require of a directory module stays relative.
    let consumer = read(root, "dist/shared/consumer.luau");
    assert_eq!(consumer, "return require(\"./pkg\")\n");

    // larvae copies the non-code file.
    assert_eq!(read(root, "dist/shared/data.json"), "{\"k\":1}\n");

    // larvae generates the derived build project with rerelativized paths.
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

    // The test processes the dist tree as input. Already-native requires pass through.
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

    // A client-marked script requires a server-only module.
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

    // The project has no larvae.toml file.
    let config = Config::load_or_default(root).unwrap();
    let outcome = pipeline::run(root, &config, true).unwrap();

    for d in &outcome.diags {
        eprintln!("{d}");
    }

    assert!(!outcome.has_errors());
    let main = read(root, "dist/main.luau");
    // The util alias maps into the same mount, so larvae emits a relative require.
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
    // larvae converts an alias with a @game value to an absolute chain.
    assert!(
        main.contains(
            r#"require(game:GetService("ReplicatedStorage"):FindFirstChild("Packages"):FindFirstChild("signal"))"#
        ),
        "alias not converted: {main}"
    );
    // larvae also converts the cross-mount relative require to an absolute chain.
    assert!(main.contains(
        r#"require(game:GetService("ReplicatedStorage"):FindFirstChild("shared"):FindFirstChild("util"):FindFirstChild("math"))"#
    ));

    // larvae converts a sibling in the same mount to a script-relative chain.
    let geometry = read(root, "dist/shared/util/geometry.luau");
    assert!(
        geometry.contains(r#"require(script.Parent:FindFirstChild("math"))"#),
        "{geometry}"
    );

    // @self resolves to a child of the script.
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

    // larvae must wrap a parenless require in parentheses.
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

/*
The entry point of a package is an `init.luau`, and what it requires has to
survive the move into the output.

`./` from an init file resolves against the parent of its directory, which the
RFC states and a runtime enforces: `./utils/helper` from `pkg/init.luau` does
not resolve, and `./pkg/utils/helper` does. So the relative form has to name
the directory the init file sits in.

That name was the bug. Larvae resolved against the source tree and wrote
`./src/utils/helper` into `dist/init.luau`, where the string still points at
`src`. A `dist` shipped without `src` beside it failed to load, and a `dist`
shipped with it ran the unprocessed source instead.
*/
#[test]
fn an_init_file_requires_its_own_directory_by_self() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();

    write(
        root,
        ".luaurc",
        r#"{ "aliases": { "utils": "./src/utils" } }"#,
    );
    write(
        root,
        "larvae.toml",
        "input = \"src\"\noutput = \"dist\"\ntarget = \"path\"\n",
    );
    write(root, "src/init.luau", "return require(\"@utils/helper\")\n");
    write(
        root,
        "src/utils/helper.luau",
        "return require(\"@utils/test\")\n",
    );
    write(root, "src/utils/test.luau", "return {}\n");

    let config = Config::load_or_default(root).unwrap();
    let outcome = pipeline::run(root, &config, true).unwrap();

    for d in &outcome.diags {
        eprintln!("{d}");
    }

    assert!(!outcome.has_errors());

    // the entry point names its own directory without spelling src or dist
    assert_eq!(
        read(root, "dist/init.luau"),
        "return require(\"@self/utils/helper\")\n"
    );

    // a sibling was always right, and stays right
    assert_eq!(
        read(root, "dist/utils/helper.luau"),
        "return require(\"./test\")\n"
    );
}

/*
A nested init file names its own directory the same way.

`src/utils/init.luau` used to emit `./utils/helper`, which happens to survive
the move because both ends travel together. `@self/helper` says the same thing
and says it without depending on where the directory sits.
*/
#[test]
fn a_nested_init_file_also_uses_self() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();

    write(
        root,
        "larvae.toml",
        "input = \"src\"\noutput = \"dist\"\ntarget = \"path\"\n",
    );
    write(
        root,
        "src/utils/init.luau",
        "return require(\"./utils/helper\")\n",
    );
    write(root, "src/utils/helper.luau", "return {}\n");

    let config = Config::load_or_default(root).unwrap();
    let outcome = pipeline::run(root, &config, true).unwrap();

    assert!(!outcome.has_errors());
    assert_eq!(
        read(root, "dist/utils/init.luau"),
        "return require(\"@self/helper\")\n"
    );
}

/// A target outside the init file's own directory keeps the relative form.
#[test]
fn an_init_file_reaching_outside_itself_stays_relative() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();

    write(
        root,
        "larvae.toml",
        "input = \"src\"\noutput = \"dist\"\ntarget = \"path\"\n",
    );
    write(root, "src/a/init.luau", "return require(\"../b/thing\")\n");
    write(root, "src/b/thing.luau", "return {}\n");

    let config = Config::load_or_default(root).unwrap();
    let outcome = pipeline::run(root, &config, true).unwrap();

    assert!(!outcome.has_errors());
    assert!(
        !read(root, "dist/a/init.luau").contains("@self"),
        "the target is not inside a/, so @self would be wrong"
    );
}
