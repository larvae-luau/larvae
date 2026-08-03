//! One input root flattens, several keep their own directory

use larvae::config::Config;
use larvae::pipeline;

mod common;
use common::*;

#[test]
fn a_single_root_still_flattens() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    write(
        root,
        "larvae.toml",
        "[process]\ninput = \"src\"\noutput = \"dist\"\n\n[requires]\ntarget = \"path\"\n",
    );
    write(root, "src/a/b.luau", "return 1\n");

    let config = Config::load_or_default(root).unwrap();
    let out = pipeline::run(root, &config, true).unwrap();

    assert!(!out.has_errors(), "{:?}", out.diags);
    assert!(root.join("dist/a/b.luau").exists());
}

/// The list form, which is what a project with vendored code needs
#[test]
fn several_roots_each_keep_their_directory() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    write(
        root,
        "larvae.toml",
        "[process]\ninput = [\"src\", \"vendor\"]\noutput = \"dist\"\n\n[requires]\ntarget = \"path\"\n",
    );
    // the same file name under both, which is exactly what flattening would lose
    write(root, "src/init.luau", "return require(\"./helper\")\n");
    write(root, "src/helper.luau", "return 1\n");
    write(root, "vendor/init.luau", "return 2\n");

    let config = Config::load_or_default(root).unwrap();
    let out = pipeline::run(root, &config, true).unwrap();

    assert!(!out.has_errors(), "{:?}", out.diags);
    assert!(root.join("dist/src/init.luau").exists());
    assert!(root.join("dist/vendor/init.luau").exists());
    assert_eq!(read(root, "dist/vendor/init.luau"), "return 2\n");
    // relative requires still resolve inside their own root
    assert!(read(root, "dist/src/init.luau").contains("./helper"));
}

#[test]
fn a_missing_root_says_which_one() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    write(
        root,
        "larvae.toml",
        "[process]\ninput = [\"src\", \"nope\"]\noutput = \"dist\"\n",
    );
    write(root, "src/a.luau", "return 1\n");

    let config = Config::load_or_default(root).unwrap();
    let err = match pipeline::run(root, &config, true) {
        Err(e) => e.to_string(),

        Ok(_) => panic!("a missing root should not build"),
    };

    assert!(err.contains("nope"), "{err}");
}

#[test]
fn a_nested_root_owns_its_own_files() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    write(
        root,
        "larvae.toml",
        "[process]\ninput = [\"src\", \"src/vendor\"]\noutput = \"dist\"\n\n[requires]\ntarget = \"path\"\n",
    );
    write(root, "src/a.luau", "return 1\n");
    write(root, "src/vendor/b.luau", "return 2\n");

    let config = Config::load_or_default(root).unwrap();
    let out = pipeline::run(root, &config, true).unwrap();

    assert!(!out.has_errors(), "{:?}", out.diags);
    assert!(root.join("dist/src/a.luau").exists());
    // claimed by the deeper root, so it is not also under dist/src/vendor
    assert!(root.join("dist/src/vendor/b.luau").exists());
    assert_eq!(out.stats.files_processed, 2);
}

#[test]
fn deletions_still_mirror_with_several_roots() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    write(
        root,
        "larvae.toml",
        "[process]\ninput = [\"src\", \"vendor\"]\noutput = \"dist\"\n\n[requires]\ntarget = \"path\"\n",
    );
    write(root, "src/a.luau", "return 1\n");
    write(root, "vendor/b.luau", "return 2\n");

    let config = Config::load_or_default(root).unwrap();
    pipeline::run(root, &config, true).unwrap();
    assert!(root.join("dist/vendor/b.luau").exists());

    std::fs::remove_file(root.join("vendor/b.luau")).unwrap();
    let out = pipeline::run(root, &config, true).unwrap();

    assert!(!root.join("dist/vendor/b.luau").exists());
    assert_eq!(out.stats.files_pruned, 1);
}
