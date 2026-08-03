//! Fixture builders shared by the end to end suites

#![allow(dead_code)]

use std::fs;
use std::path::Path;

pub fn write(root: &Path, rel: &str, content: &str) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, content).unwrap();
}

pub fn read(root: &Path, rel: &str) -> String {
    fs::read_to_string(root.join(rel)).unwrap()
}

/// A Rojo shaped project, shared + server code, packages outside src
pub fn fixture() -> tempfile::TempDir {
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
                    "shared": { "$path": "src/shared" },
                    "Packages": { "$path": "Packages" }
                },

                "ServerScriptService": { "$path": "src/server" }
            }
        }"#,
    );
    write(
        root,
        "larvae.toml",
        r#"
            [aliases]
            pkg = "@game/ReplicatedStorage/Packages"
        "#,
    );
    write(root, "Packages/signal.luau", "return {}\n");
    write(root, "src/shared/util/math.luau", "return {}\n");
    write(
        root,
        "src/shared/util/geometry.luau",
        "local math = require(\"./math\")\nreturn math\n",
    );
    write(
        root,
        "src/server/main.server.luau",
        concat!(
            "-- entry point; require(\"./inside-comment\") must not be touched\n",
            "local Signal = require(\"@pkg/signal\")\n",
            "local math = require(\"../shared/util/math\") -- cross mount\n",
            "print(Signal, math)\n",
        ),
    );
    // A directory module with an init and a child
    write(
        root,
        "src/shared/pkg/init.luau",
        "local sub = require(\"@self/sub\")\nreturn sub\n",
    );
    write(root, "src/shared/pkg/sub.luau", "return 1\n");
    // Consumer of the directory module, sibling file
    write(
        root,
        "src/shared/consumer.luau",
        "return require(\"./pkg\")\n",
    );
    // Non code asset that must be copied through
    write(root, "src/shared/data.json", "{\"k\":1}\n");

    tmp
}

pub fn instance_fixture(indexing_style: &str) -> tempfile::TempDir {
    let tmp = fixture();
    let root = tmp.path();

    write(
        root,
        "larvae.toml",
        &format!(
            "[aliases]\npkg = \"@game/ReplicatedStorage/Packages\"\n\n[requires]\ntarget = \"roblox-instance\"\nindexing_style = \"{indexing_style}\"\n"
        ),
    );

    tmp
}
