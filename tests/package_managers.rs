/*!
Onboarding a project from each package manager

All four put dependencies outside `[process].input`, which is the case most
likely to break quietly. What these assert is mostly that larvae leaves
things alone, package files are never read, rewritten or copied, and the
derived project keeps pointing rojo at the real directory
*/

use larvae::config::Config;
use larvae::pipeline;

mod common;
use common::*;

/// Every `$path` the derived build project came out with
fn build_paths(root: &std::path::Path) -> String {
    read(root, ".larvae/build.project.json")
}

#[test]
fn wally() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    write(
        root,
        "wally.toml",
        "[package]\nname = \"acme/game\"\nversion = \"0.1.0\"\nrealm = \"shared\"\n",
    );
    write(
        root,
        "default.project.json",
        r#"{
  "name": "game",
  "tree": {
    "$className": "DataModel",
    "ReplicatedStorage": {
      "app": { "$path": "src/shared" },
      "Packages": { "$path": "Packages" }
    },
    "ServerScriptService": {
      "server": { "$path": "src/server" },
      "ServerPackages": { "$path": "ServerPackages" }
    }
  }
}"#,
    );
    write(
        root,
        "larvae.toml",
        concat!(
            "[aliases]\n",
            "pkg = \"@game/ReplicatedStorage/Packages\"\n",
            "srv = \"@game/ServerScriptService/ServerPackages\"\n",
        ),
    );

    // a link module and the versioned tree it points into
    write(
        root,
        "Packages/Signal.luau",
        "return require(script.Parent._Index[\"sleitnick_signal@2.0.1\"][\"signal\"])\n",
    );
    write(
        root,
        "Packages/_Index/sleitnick_signal@2.0.1/signal/init.luau",
        "return {}\n",
    );
    write(root, "ServerPackages/Net.luau", "return {}\n");
    write(
        root,
        "src/shared/util.luau",
        "return require(\"@pkg/Signal\")\n",
    );
    write(
        root,
        "src/server/main.server.luau",
        "return require(\"@srv/Net\"), require(\"@game/ReplicatedStorage/app/util\")\n",
    );

    let config = Config::load_or_default(root).unwrap();
    let out = pipeline::run(root, &config, true).unwrap();

    assert!(!out.has_errors(), "{:?}", out.diags);
    assert!(
        read(root, "dist/shared/util.luau")
            .contains("require(\"@game/ReplicatedStorage/Packages/Signal\")")
    );
    assert!(
        read(root, "dist/server/main.server.luau")
            .contains("require(\"@game/ServerScriptService/ServerPackages/Net\")")
    );

    // packages were never processed, never copied
    assert!(!root.join("dist/Packages").exists());
    assert!(!root.join("dist/ServerPackages").exists());
    // and the derived project still points rojo at the real ones
    let derived = build_paths(root);
    assert!(derived.contains("../Packages"), "{derived}");
    assert!(derived.contains("../ServerPackages"), "{derived}");
    assert!(derived.contains("../dist/shared"), "{derived}");
}

#[test]
fn pesde() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    write(
        root,
        "pesde.toml",
        "name = \"acme/game\"\nversion = \"0.1.0\"\n[target]\nenvironment = \"roblox\"\n",
    );
    write(
        root,
        "default.project.json",
        r#"{
  "name": "game",
  "tree": {
    "$className": "DataModel",
    "ReplicatedStorage": {
      "app": { "$path": "src" },
      "roblox_packages": { "$path": "roblox_packages" }
    }
  }
}"#,
    );
    write(
        root,
        "larvae.toml",
        "[aliases]\npkg = \"@game/ReplicatedStorage/roblox_packages\"\n",
    );
    write(root, "roblox_packages/signal.luau", "return {}\n");
    write(
        root,
        "src/init.luau",
        "return require(\"@pkg/signal\"), require(\"@self/helper\")\n",
    );
    write(root, "src/helper.luau", "return {}\n");

    let config = Config::load_or_default(root).unwrap();
    let out = pipeline::run(root, &config, true).unwrap();

    assert!(!out.has_errors(), "{:?}", out.diags);

    let got = read(root, "dist/init.luau");
    assert!(
        got.contains("require(\"@game/ReplicatedStorage/roblox_packages/signal\")"),
        "{got}"
    );
    // @self passes straight through, it already means the right thing
    assert!(got.contains("require(\"@self/helper\")"), "{got}");
    assert!(build_paths(root).contains("../roblox_packages"));
}

#[test]
fn npmluau() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    write(root, "package.json", "{ \"name\": \"game\" }\n");
    write(
        root,
        ".luaurc",
        "{\n  \"aliases\": {\n    \"signal\": \"node_modules/@acme/signal\",\n    \"strings\": \"node_modules/strings\"\n  }\n}\n",
    );
    write(
        root,
        "larvae.toml",
        "[process]\ninput = \"src\"\noutput = \"dist\"\n\n[requires]\ntarget = \"path\"\n",
    );
    write(root, "node_modules/@acme/signal/init.luau", "return {}\n");
    write(
        root,
        "node_modules/@acme/signal/lib/conn.luau",
        "return {}\n",
    );
    write(root, "node_modules/strings/init.luau", "return {}\n");
    write(
        root,
        "src/main.luau",
        concat!(
            "local s = require(\"@signal\")\n",
            "local c = require(\"@signal/lib/conn\")\n",
            "local t = require(\"@strings\")\n",
            "return { s, c, t }\n",
        ),
    );

    let config = Config::load_or_default(root).unwrap();
    let out = pipeline::run(root, &config, true).unwrap();

    assert!(!out.has_errors(), "{:?}", out.diags);

    let got = read(root, "dist/main.luau");
    // relative to dist, where the output actually runs from
    assert!(
        got.contains("require(\"../node_modules/@acme/signal\")"),
        "{got}"
    );
    // a scoped name deep in the tree, the @acme segment is a directory not an alias
    assert!(
        got.contains("require(\"../node_modules/@acme/signal/lib/conn\")"),
        "{got}"
    );
    assert!(
        got.contains("require(\"../node_modules/strings\")"),
        "{got}"
    );
    assert!(!root.join("dist/node_modules").exists());
}

/// pnpm links packages into a store, the emitted path must not follow the link
#[test]
fn npmluau_with_a_symlinked_package() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    write(
        root,
        ".store/signal@2.0.1/node_modules/signal/init.luau",
        "return {}\n",
    );
    std::fs::create_dir_all(root.join("node_modules")).unwrap();

    #[cfg(unix)]
    std::os::unix::fs::symlink(
        root.join(".store/signal@2.0.1/node_modules/signal"),
        root.join("node_modules/signal"),
    )
    .unwrap();

    #[cfg(not(unix))]
    return;

    write(
        root,
        ".luaurc",
        "{\n  \"aliases\": { \"signal\": \"node_modules/signal\" }\n}\n",
    );
    write(
        root,
        "larvae.toml",
        "[process]\ninput = \"src\"\noutput = \"dist\"\n\n[requires]\ntarget = \"path\"\n",
    );
    write(root, "src/main.luau", "return require(\"@signal\")\n");

    let config = Config::load_or_default(root).unwrap();
    let out = pipeline::run(root, &config, true).unwrap();

    assert!(!out.has_errors(), "{:?}", out.diags);

    let got = read(root, "dist/main.luau");
    assert!(got.contains("require(\"../node_modules/signal\")"), "{got}");
    // following the link would bake the store path into the output
    assert!(!got.contains(".store"), "{got}");
}

#[test]
fn lpm() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    write(
        root,
        "lpm.toml",
        concat!(
            "[package]\nname = \"acme/game\"\nversion = \"0.1.0\"\n",
            "[target]\nenvironment = \"shared\"\nmain = \"src/init.luau\"\n",
        ),
    );
    write(
        root,
        "default.project.json",
        r#"{
  "name": "game",
  "tree": {
    "$className": "DataModel",
    "ReplicatedStorage": {
      "app": { "$path": "src" },
      "packages": { "$path": "packages" }
    }
  }
}"#,
    );
    write(
        root,
        "larvae.toml",
        "[aliases]\npkg = \"@game/ReplicatedStorage/packages\"\n",
    );
    write(root, "packages/signal/init.luau", "return {}\n");
    write(
        root,
        "src/init.luau",
        "return require(\"@pkg/signal\"), require(\"@self/sub\")\n",
    );
    write(root, "src/sub.luau", "return {}\n");

    let config = Config::load_or_default(root).unwrap();
    let out = pipeline::run(root, &config, true).unwrap();

    assert!(!out.has_errors(), "{:?}", out.diags);
    assert!(
        read(root, "dist/init.luau")
            .contains("require(\"@game/ReplicatedStorage/packages/signal\")")
    );
    assert!(!root.join("dist/packages").exists());
    assert!(build_paths(root).contains("../packages"));
}

/// The misconfiguration these tests exist to catch
#[test]
fn a_package_directory_nobody_mounted_is_reported() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    write(
        root,
        "default.project.json",
        r#"{ "name": "g", "tree": { "$className": "DataModel",
             "ReplicatedStorage": { "app": { "$path": "src" } } } }"#,
    );
    write(
        root,
        "larvae.toml",
        "[aliases]\npkg = \"@game/ReplicatedStorage/Packages\"\n",
    );
    write(root, "Packages/Signal.luau", "return {}\n");
    write(root, "src/main.luau", "return require(\"@pkg/Signal\")\n");

    let config = Config::load_or_default(root).unwrap();
    let out = pipeline::run(root, &config, true).unwrap();

    assert!(
        out.diags
            .iter()
            .any(|d| d.message.contains("nothing in the project maps there")),
        "{:?}",
        out.diags
    );
}
