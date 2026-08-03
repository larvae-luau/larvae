//! requires.sourcemap, rojo's own map as the authority

use larvae::config::Config;
use larvae::pipeline;

mod common;
use common::*;

/*
A tree the project file alone cannot describe. `src/vendor` is mounted at a
name that does not match its directory, which is the sort of thing rojo
resolves and a static read of $path entries does not
*/
fn project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    write(
        root,
        "sourcemap.json",
        r#"{
  "name": "game",
  "className": "DataModel",
  "children": [
    { "name": "ReplicatedStorage", "className": "ReplicatedStorage", "children": [
      { "name": "shared", "className": "Folder", "children": [
        { "name": "util", "className": "ModuleScript", "filePaths": ["src/util.luau"] },
        { "name": "main", "className": "ModuleScript", "filePaths": ["src/main.luau"] }
      ]},
      { "name": "Renamed", "className": "Folder", "children": [
        { "name": "lib", "className": "ModuleScript", "filePaths": ["src/vendor/lib.luau"] }
      ]}
    ]}
  ]
}"#,
    );
    write(
        root,
        "larvae.toml",
        "[requires]\ntarget = \"roblox-string\"\nsourcemap = \"sourcemap.json\"\n",
    );
    write(root, "src/util.luau", "return {}\n");
    write(root, "src/vendor/lib.luau", "return {}\n");

    dir
}

#[test]
fn a_sourcemap_drives_resolution() {
    let tmp = project();
    let root = tmp.path();

    write(
        root,
        "src/main.luau",
        "return require(\"./util\"), require(\"./vendor/lib\")\n",
    );

    let config = Config::load_or_default(root).unwrap();
    let out = pipeline::run(root, &config, true).unwrap();

    assert!(!out.has_errors(), "{:?}", out.diags);

    let got = read(root, "dist/main.luau");
    // a sibling stays relative
    assert!(got.contains("require(\"./util\")"), "{got}");
    /*
    src/vendor/lib.luau sits at ReplicatedStorage/Renamed/lib, a name only
    the sourcemap knows. From shared/main that is one hop up and across,
    and relative wins whenever the tree connects
    */
    assert!(got.contains("require(\"../Renamed/lib\")"), "{got}");
}

#[test]
fn an_instance_chain_resolves_through_the_sourcemap() {
    let tmp = project();
    let root = tmp.path();

    write(
        root,
        "src/main.luau",
        "return require(game.ReplicatedStorage.Renamed.lib)\n",
    );

    let config = Config::load_or_default(root).unwrap();
    let out = pipeline::run(root, &config, true).unwrap();

    assert!(!out.has_errors(), "{:?}", out.diags);
    // the chain resolved through the sourcemap, then came back out relative
    assert!(
        read(root, "dist/main.luau").contains("require(\"../Renamed/lib\")"),
        "{}",
        read(root, "dist/main.luau")
    );
}

#[test]
fn a_stale_sourcemap_invalidates_the_cache() {
    let tmp = project();
    let root = tmp.path();

    write(root, "src/main.luau", "return require(\"./vendor/lib\")\n");

    let config = Config::load_or_default(root).unwrap();
    pipeline::run(root, &config, true).unwrap();
    assert!(
        read(root, "dist/main.luau").contains("Renamed"),
        "{}",
        read(root, "dist/main.luau")
    );

    // rojo regenerates it under a different name, nothing else changed
    let map = read(root, "sourcemap.json").replace("Renamed", "Moved");
    write(root, "sourcemap.json", &map);

    let out = pipeline::run(root, &config, true).unwrap();

    assert!(!out.has_errors(), "{:?}", out.diags);
    // a cache that ignored the sourcemap would still say Renamed here
    assert!(
        read(root, "dist/main.luau").contains("Moved"),
        "{}",
        read(root, "dist/main.luau")
    );
    assert_eq!(out.stats.files_cached, 0);
}

#[test]
fn a_broken_sourcemap_is_an_error() {
    let tmp = project();
    let root = tmp.path();

    write(root, "src/main.luau", "return 1\n");
    write(root, "sourcemap.json", "{ not json");

    let config = Config::load_or_default(root).unwrap();
    let out = pipeline::run(root, &config, true).unwrap();

    assert!(out.has_errors());
    assert!(
        out.diags.iter().any(|d| d.message.contains("sourcemap")),
        "{:?}",
        out.diags
    );
}
