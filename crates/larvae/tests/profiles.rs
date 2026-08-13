//! These tests cover build profiles. larvae merges a profile over the config
//! before it types anything.

use larvae::config::Config;
use larvae::pipeline;

mod common;
use common::*;

const CONFIG: &str = r#"
[process]
input = "src"
output = "dist"

[requires]
target = "path"

[defines]
DEBUG = true

[profile.release]
process = { output = "build" }
defines = { DEBUG = false }
rules = { remove_comments = true }

[profile.lune]
requires = { target = "path" }
"#;

fn project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    write(root, "larvae.toml", CONFIG);
    write(root, "src/main.luau", "-- a comment\nreturn DEBUG\n");

    dir
}

#[test]
fn no_profile_uses_the_base() {
    let tmp = project();
    let root = tmp.path();

    let config = Config::load_or_default(root).unwrap();
    let out = pipeline::run(root, &config, true).unwrap();

    assert!(!out.has_errors(), "{:?}", out.diags);
    assert_eq!(read(root, "dist/main.luau"), "-- a comment\nreturn true\n");
}

#[test]
fn a_profile_overrides_key_by_key() {
    let tmp = project();
    let root = tmp.path();

    let config = Config::load_or_default_profile(root, Some("release")).unwrap();
    let out = pipeline::run(root, &config, true).unwrap();

    assert!(!out.has_errors(), "{:?}", out.diags);

    // The output moved, the define changed, and the profile's own rule ran.
    let got = read(root, "build/main.luau");
    assert!(got.contains("return false"), "{got}");
    assert!(!got.contains("a comment"), "{got}");
    // The profile never named the input, so the input kept the base value.
    assert!(root.join("src/main.luau").exists());
}

#[test]
fn the_profile_block_is_inert_without_the_flag() {
    let tmp = project();
    let root = tmp.path();

    // The block parses correctly and changes nothing. There is also no
    // unknown-key error.
    let config = Config::load_or_default(root).unwrap();

    assert_eq!(config.process.output, std::path::PathBuf::from("dist"));
}

#[test]
fn an_unknown_profile_lists_the_real_ones() {
    let tmp = project();
    let err = Config::load_or_default_profile(tmp.path(), Some("nope"))
        .unwrap_err()
        .to_string();

    // anyhow chains the context, so the test walks the chain for the useful part.
    let full = format!("{err:#}");
    assert!(full.contains("nope"), "{full}");
}

#[test]
fn asking_for_a_profile_with_no_config_says_so() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "src/main.luau", "return 1\n");

    let err = Config::load_or_default_profile(dir.path(), Some("release"))
        .unwrap_err()
        .to_string();

    assert!(err.contains("needs a larvae.toml"), "{err}");
}
