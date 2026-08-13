//! larvae reads require(script.Parent.Foo) style requires. This is the
//! migration path.

use larvae::config::Config;
use larvae::pipeline;

mod common;
use common::*;

/// A tree with shared, server, and client mounts. Legacy games have this shape.
fn legacy() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    write(
        root,
        "default.project.json",
        r#"{
  "name": "legacy",
  "tree": {
    "$className": "DataModel",
    "ReplicatedStorage": {
      "app": { "$path": "src/shared" },
      "Packages": { "$path": "Packages" }
    },
    "ServerScriptService": { "server": { "$path": "src/server" } },
    "StarterPlayer": {
      "StarterPlayerScripts": { "client": { "$path": "src/client" } }
    }
  }
}"#,
    );
    write(
        root,
        "larvae.toml",
        "[requires]\ntarget = \"roblox-string\"\n",
    );
    write(root, "Packages/Signal.luau", "return {}\n");
    write(root, "src/shared/config.luau", "return {}\n");
    write(root, "src/shared/util/math.luau", "return {}\n");
    write(root, "src/server/keys.luau", "return {}\n");

    dir
}

#[test]
fn script_relative_chains_resolve() {
    let tmp = legacy();
    let root = tmp.path();

    // This is an init module, so `script` is the util directory itself.
    write(
        root,
        "src/shared/util/init.luau",
        "local m = require(script.math)\nlocal c = require(script.Parent.config)\nreturn { m, c }\n",
    );

    let config = Config::load_or_default(root).unwrap();
    let out = pipeline::run(root, &config, true).unwrap();

    assert!(!out.has_errors(), "{:?}", out.diags);

    let got = read(root, "dist/shared/util/init.luau");
    // This target is a child of the init module's own directory.
    assert!(got.contains("require(\"@self/math\")"), "{got}");
    // This target is one level up, so it is a sibling of the directory.
    assert!(got.contains("require(\"./config\")"), "{got}");
}

#[test]
fn absolute_chains_resolve() {
    let tmp = legacy();
    let root = tmp.path();

    write(
        root,
        "src/shared/main.luau",
        concat!(
            "local S = require(game.ReplicatedStorage.Packages.Signal)\n",
            "local c = require(game:GetService(\"ReplicatedStorage\").app.config)\n",
            "return { S, c }\n",
        ),
    );

    let config = Config::load_or_default(root).unwrap();
    let out = pipeline::run(root, &config, true).unwrap();

    assert!(!out.has_errors(), "{:?}", out.diags);

    let got = read(root, "dist/shared/main.luau");
    assert!(
        got.contains("require(\"@game/ReplicatedStorage/Packages/Signal\")"),
        "{got}"
    );
    // GetService reads the same as a plain index.
    assert!(got.contains("require(\"./config\")"), "{got}");
}

#[test]
fn find_first_child_and_wait_for_child_read_the_same() {
    let tmp = legacy();
    let root = tmp.path();

    write(
        root,
        "src/shared/main.luau",
        concat!(
            "local a = require(script.Parent:WaitForChild(\"config\"))\n",
            "local b = require(script.Parent:FindFirstChild(\"config\"))\n",
            "return { a, b }\n",
        ),
    );

    let config = Config::load_or_default(root).unwrap();
    let out = pipeline::run(root, &config, true).unwrap();

    assert!(!out.has_errors(), "{:?}", out.diags);
    assert_eq!(
        read(root, "dist/shared/main.luau")
            .matches("require(\"./config\")")
            .count(),
        2
    );
}

#[test]
fn a_chain_it_cannot_follow_is_left_alone() {
    let tmp = legacy();
    let root = tmp.path();

    write(
        root,
        "src/shared/main.luau",
        concat!(
            // A local replaces a service here. larvae does not track locals.
            "local a = require(ReplicatedStorage.Packages.Signal)\n",
            // This is a computed child.
            "local b = require(script.Parent[name])\n",
            "return { a, b }\n",
        ),
    );

    let config = Config::load_or_default(root).unwrap();
    let out = pipeline::run(root, &config, true).unwrap();

    assert!(!out.has_errors(), "{:?}", out.diags);

    let got = read(root, "dist/shared/main.luau");
    assert!(
        got.contains("require(ReplicatedStorage.Packages.Signal)"),
        "{got}"
    );
    assert!(got.contains("require(script.Parent[name])"), "{got}");
    // larvae still counts both, so the check can report the requires it cannot follow.
    assert_eq!(out.stats.requires_dynamic, 2);
}

#[test]
fn realm_violations_are_caught_through_a_chain() {
    let tmp = legacy();
    let root = tmp.path();

    write(
        root,
        "src/client/init.client.luau",
        "local k = require(game.ServerScriptService.server.keys)\nreturn k\n",
    );

    let config = Config::load_or_default(root).unwrap();
    let out = pipeline::run(root, &config, true).unwrap();

    assert!(out.has_errors());
    let msg = &out
        .diags
        .iter()
        .find(|d| d.message.contains("keys"))
        .unwrap()
        .message;
    // The message shows the expression as the author wrote it, not as a string.
    assert!(
        msg.contains("require(game.ServerScriptService.server.keys)"),
        "{msg}"
    );
    assert!(msg.contains("does not replicate"), "{msg}");
}

#[test]
fn a_target_nothing_maps_to_warns_and_stays_put() {
    let tmp = legacy();
    let root = tmp.path();

    write(
        root,
        "src/shared/main.luau",
        "local x = require(script.Parent.Parent.nope.thing)\nreturn x\n",
    );

    let config = Config::load_or_default(root).unwrap();
    let out = pipeline::run(root, &config, true).unwrap();

    assert!(!out.has_errors(), "{:?}", out.diags);
    assert!(
        out.diags
            .iter()
            .any(|d| d.message.contains("nothing in the project maps there")),
        "{:?}",
        out.diags
    );
    assert!(
        read(root, "dist/shared/main.luau").contains("require(script.Parent.Parent.nope.thing)")
    );
}

#[test]
fn instance_input_can_be_turned_off() {
    let tmp = legacy();
    let root = tmp.path();

    write(
        root,
        "larvae.toml",
        "[requires]\ntarget = \"roblox-string\"\ninstance_input = false\n",
    );
    write(
        root,
        "src/shared/main.luau",
        "local S = require(game.ReplicatedStorage.Packages.Signal)\nreturn S\n",
    );

    let config = Config::load_or_default(root).unwrap();
    let out = pipeline::run(root, &config, true).unwrap();

    assert!(!out.has_errors(), "{:?}", out.diags);
    assert!(
        read(root, "dist/shared/main.luau")
            .contains("require(game.ReplicatedStorage.Packages.Signal)")
    );
}

#[test]
fn a_wally_link_module_chain_reads() {
    let tmp = legacy();
    let root = tmp.path();

    write(
        root,
        "Packages/_Index/sleitnick_signal@2.0.1/signal/init.luau",
        "return {}\n",
    );
    write(
        root,
        "src/shared/main.luau",
        "local S = require(game.ReplicatedStorage.Packages._Index[\"sleitnick_signal@2.0.1\"][\"signal\"])\nreturn S\n",
    );

    let config = Config::load_or_default(root).unwrap();
    let out = pipeline::run(root, &config, true).unwrap();

    assert!(!out.has_errors(), "{:?}", out.diags);
    assert!(
        read(root, "dist/shared/main.luau").contains(
            "require(\"@game/ReplicatedStorage/Packages/_Index/sleitnick_signal@2.0.1/signal\")"
        ),
        "{}",
        read(root, "dist/shared/main.luau")
    );
}
