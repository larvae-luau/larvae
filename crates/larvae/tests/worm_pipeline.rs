//! A worm front-end through the real pipeline, not through a unit seam

use larvae::config::Config;
use larvae::diag::Severity;
use larvae::pipeline;
use std::path::Path;

mod common;
use common::*;

/// A worm claiming `.mk`, turning `<Tag/>` into a call without moving any lines
fn markup_worm(root: &Path) {
    write(
        root,
        "worms/markup/worm.toml",
        r#"
name  = "markup"
api   = 1
form  = "luau"
entry = "init.luau"

[frontend]
claims = [".mk"]
"#,
    );
    write(
        root,
        "worms/markup/init.luau",
        r#"
return {
    frontend = {
        compile = function(source, config)
            return (source:gsub("<(%w+)%s*/>", "make(\"%1\")"))
        end,
    },
}
"#,
    );
}

fn with_worm(root: &Path) {
    write(
        root,
        "larvae.toml",
        r#"
            [aliases]
            pkg = "@game/ReplicatedStorage/Packages"

            [worms]
            markup = { path = "worms/markup" }
        "#,
    );
}

fn build(root: &Path) -> pipeline::Outcome {
    let config = Config::load_or_default(root).unwrap();

    pipeline::run(root, &config, true).unwrap()
}

fn errors(outcome: &pipeline::Outcome) -> String {
    outcome
        .diags
        .iter()
        .map(|d| d.to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn a_claimed_file_is_compiled_then_rewritten_and_renamed() {
    let tmp = fixture();
    let root = tmp.path();

    markup_worm(root);
    with_worm(root);
    write(
        root,
        "src/shared/App.mk",
        "local S = require(\"@pkg/signal\")\nlocal ui = <Frame/>\nreturn { S, ui }\n",
    );

    let outcome = build(root);

    assert!(!outcome.has_errors(), "{}", errors(&outcome));

    // renamed before routing, so the DataModel instance is App and not App.mk
    let out = read(root, "dist/shared/App.luau");

    assert!(
        out.contains(r#"require("@game/ReplicatedStorage/Packages/signal")"#),
        "requires not rewritten: {out}"
    );
    assert!(out.contains(r#"make("Frame")"#), "worm did not run: {out}");
    assert!(!root.join("dist/shared/App.mk").exists());
}

/// The whole reason a front-end is a pre-pass rather than a run_order slot
#[test]
fn markup_never_reaches_the_lexer() {
    let tmp = fixture();
    let root = tmp.path();

    markup_worm(root);
    with_worm(root);
    // `<Frame/>` is not Luau, so anything below the pre-pass seeing it is the bug
    write(
        root,
        "src/shared/App.mk",
        "local ui = <Frame/>\nreturn ui\n",
    );

    let outcome = build(root);

    assert!(!outcome.has_errors(), "{}", errors(&outcome));
    assert!(read(root, "dist/shared/App.luau").contains(r#"make("Frame")"#));
}

#[test]
fn line_numbers_survive_the_whole_pipeline() {
    let tmp = fixture();
    let root = tmp.path();

    markup_worm(root);
    with_worm(root);

    let src = "local S = require(\"@pkg/signal\")\n\nlocal a = <Frame/>\nlocal b = <Text/>\n\nreturn { S, a, b }\n";
    write(root, "src/shared/App.mk", src);

    build(root);

    assert_eq!(
        read(root, "dist/shared/App.luau").lines().count(),
        src.lines().count()
    );
}

/// Pruning has to know about the rename, or it deletes what the build just wrote
#[test]
fn the_output_is_not_pruned_as_stale() {
    let tmp = fixture();
    let root = tmp.path();

    markup_worm(root);
    with_worm(root);
    write(root, "src/shared/App.mk", "return <Frame/>\n");

    build(root);
    assert!(root.join("dist/shared/App.luau").exists());

    // and again, since a stale set usually shows itself on the second run
    build(root);
    assert!(root.join("dist/shared/App.luau").exists());
}

#[test]
fn an_unclaimed_extension_is_untouched() {
    let tmp = fixture();
    let root = tmp.path();

    markup_worm(root);
    with_worm(root);
    write(root, "src/shared/plain.luau", "local ui = 1\nreturn ui\n");

    build(root);

    assert_eq!(
        read(root, "dist/shared/plain.luau"),
        "local ui = 1\nreturn ui\n"
    );
}

#[test]
fn a_worm_reporting_a_problem_fails_the_file_and_names_itself() {
    let tmp = fixture();
    let root = tmp.path();

    write(
        root,
        "worms/markup/worm.toml",
        "name = \"markup\"\napi = 1\nform = \"luau\"\nentry = \"init.luau\"\n\n[frontend]\nclaims = [\".mk\"]\n",
    );
    write(
        root,
        "worms/markup/init.luau",
        "return { frontend = { compile = function() error(\"unbalanced tag\") end } }",
    );
    with_worm(root);
    write(root, "src/shared/App.mk", "return <Frame\n");

    let outcome = build(root);
    let text = errors(&outcome);

    assert!(
        outcome.diags.iter().any(|d| d.severity == Severity::Error),
        "{text}"
    );
    assert!(text.contains("unbalanced tag"), "{text}");
    assert!(text.contains("markup"), "{text}");
    assert!(!root.join("dist/shared/App.luau").exists());
}

#[test]
fn two_worms_claiming_one_extension_is_refused_at_load() {
    let tmp = fixture();
    let root = tmp.path();

    markup_worm(root);
    write(
        root,
        "worms/other/worm.toml",
        "name = \"other\"\napi = 1\nform = \"luau\"\nentry = \"init.luau\"\n\n[frontend]\nclaims = [\".mk\"]\n",
    );
    write(
        root,
        "worms/other/init.luau",
        "return { frontend = { compile = function(s) return s end } }",
    );
    write(
        root,
        "larvae.toml",
        r#"
            [worms]
            markup = { path = "worms/markup" }
            other = { path = "worms/other" }
        "#,
    );

    let config = Config::load_or_default(root).unwrap();
    let err = pipeline::run(root, &config, true).err().unwrap();

    assert!(format!("{err:#}").contains("both claim .mk"), "{err:#}");
}

/// A worm's rules live in [rules] beside ours, so the name check has to wait
#[test]
fn a_worm_rule_name_is_accepted_and_a_typo_is_not() {
    let tmp = fixture();
    let root = tmp.path();

    write(
        root,
        "worms/r/worm.toml",
        "name = \"r\"\napi = 1\nform = \"luau\"\nentry = \"init.luau\"\nrun_order = 1\n\n[rules.tidy]\ndefault = false\n",
    );
    write(
        root,
        "worms/r/init.luau",
        "return { rules = { tidy = { visit = function() end } } }",
    );
    write(
        root,
        "larvae.toml",
        "[worms]\nr = { path = \"worms/r\" }\n\n[rules]\ntidy = true\n",
    );

    let config = Config::load_or_default(root).unwrap();

    assert!(pipeline::run(root, &config, true).is_ok());

    write(
        root,
        "larvae.toml",
        "[worms]\nr = { path = \"worms/r\" }\n\n[rules]\ntidyy = true\n",
    );

    let config = Config::load_or_default(root).unwrap();
    let err = pipeline::run(root, &config, true).err().unwrap();

    assert!(format!("{err:#}").contains("tidyy"), "{err:#}");
}
