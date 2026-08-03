//! [requires.overrides], a different output form per path

use larvae::config::Config;
use larvae::pipeline;

mod common;
use common::*;

/// Shared code plus client code that runs out of a Starter container
fn mixed(overrides: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    write(
        root,
        "default.project.json",
        r#"{
  "name": "mixed",
  "tree": {
    "$className": "DataModel",
    "ReplicatedStorage": { "app": { "$path": "src/shared" } },
    "StarterPlayer": {
      "StarterPlayerScripts": { "client": { "$path": "src/client" } }
    }
  }
}"#,
    );
    write(
        root,
        "larvae.toml",
        &format!("[requires]\ntarget = \"roblox-string\"\n\n{overrides}"),
    );
    write(root, "src/shared/util.luau", "return {}\n");
    write(
        root,
        "src/shared/main.luau",
        "return require(\"@game/ReplicatedStorage/app/util\")\n",
    );
    write(
        root,
        "src/client/init.client.luau",
        "return require(\"@game/ReplicatedStorage/app/util\")\n",
    );

    dir
}

#[test]
fn an_override_moves_only_the_paths_it_names() {
    let tmp = mixed("[requires.overrides]\n\"client/**\" = \"roblox-instance\"\n");
    let root = tmp.path();

    let config = Config::load_or_default(root).unwrap();
    let out = pipeline::run(root, &config, true).unwrap();

    assert!(!out.has_errors(), "{:?}", out.diags);

    // shared code kept the default string form
    let shared = read(root, "dist/shared/main.luau");
    assert!(
        shared.contains("require(\"@game/ReplicatedStorage/app/util\")"),
        "{shared}"
    );

    // client code came out as an instance expression instead
    let client = read(root, "dist/client/init.client.luau");
    assert!(client.contains("GetService"), "{client}");
    assert!(!client.contains("\"@game/"), "{client}");
}

#[test]
fn an_override_can_carry_its_own_indexing_style() {
    let tmp = mixed(
        "[requires.overrides]\n\"client/**\" = { target = \"roblox-instance\", indexing_style = \"wait_for_child\" }\n",
    );
    let root = tmp.path();

    let config = Config::load_or_default(root).unwrap();
    let out = pipeline::run(root, &config, true).unwrap();

    assert!(!out.has_errors(), "{:?}", out.diags);
    assert!(
        read(root, "dist/client/init.client.luau").contains("WaitForChild"),
        "{}",
        read(root, "dist/client/init.client.luau")
    );
}

#[test]
fn no_override_leaves_every_file_on_the_default() {
    let tmp = mixed("");
    let root = tmp.path();

    let config = Config::load_or_default(root).unwrap();
    let out = pipeline::run(root, &config, true).unwrap();

    assert!(!out.has_errors(), "{:?}", out.diags);
    assert!(read(root, "dist/client/init.client.luau").contains("\"@game/"));
}

#[test]
fn a_bad_target_name_fails_at_load() {
    let tmp = mixed("[requires.overrides]\n\"client/**\" = \"nope\"\n");
    let err = Config::load_or_default(tmp.path()).unwrap_err().to_string();

    assert!(err.contains("roblox-instance"), "{err}");
}
