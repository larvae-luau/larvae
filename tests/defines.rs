//! [defines], compile time constants

use larvae::config::Config;
use larvae::pipeline;

mod common;
use common::*;

fn project(config: &str, source: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    write(root, "larvae.toml", config);
    write(root, "src/main.luau", source);

    dir
}

const BASE: &str =
    "[process]\ninput = \"src\"\noutput = \"dist\"\n\n[requires]\ntarget = \"path\"\n";

#[test]
fn every_value_kind_substitutes() {
    let tmp = project(
        &format!("{BASE}\n[defines]\nDEBUG = false\nMAX = 32\nNAME = \"game\"\nRATE = 1.5\n"),
        "return DEBUG, MAX, NAME, RATE\n",
    );
    let root = tmp.path();

    let config = Config::load_or_default(root).unwrap();
    let out = pipeline::run(root, &config, true).unwrap();

    assert!(!out.has_errors(), "{:?}", out.diags);
    assert_eq!(
        read(root, "dist/main.luau"),
        "return false, 32, \"game\", 1.5\n"
    );
}

#[test]
fn a_name_the_source_bound_is_never_touched() {
    let tmp = project(
        &format!("{BASE}\n[defines]\nDEBUG = false\n"),
        concat!(
            "local DEBUG = \"mine\"\n",
            "local function f(DEBUG) return DEBUG end\n",
            "return DEBUG, f, t.DEBUG\n",
        ),
    );
    let root = tmp.path();

    let config = Config::load_or_default(root).unwrap();
    let out = pipeline::run(root, &config, true).unwrap();

    assert!(!out.has_errors(), "{:?}", out.diags);
    // nothing changed, every DEBUG here is either a local or a field
    assert!(!read(root, "dist/main.luau").contains("false"));
}

#[test]
fn shadowing_only_covers_its_own_block() {
    let tmp = project(
        &format!("{BASE}\n[defines]\nDEBUG = true\n"),
        "do local DEBUG = 1 end\nreturn DEBUG\n",
    );
    let root = tmp.path();

    let config = Config::load_or_default(root).unwrap();
    let out = pipeline::run(root, &config, true).unwrap();

    assert!(!out.has_errors(), "{:?}", out.diags);
    assert_eq!(
        read(root, "dist/main.luau"),
        "do local DEBUG = 1 end\nreturn true\n"
    );
}

/// The point of defines, a branch folds away entirely
#[test]
fn folding_and_pruning_compose_with_defines() {
    let tmp = project(
        &format!(
            "{BASE}\n[defines]\nDEBUG = false\n\n[rules]\ncompute_expression = true\nremove_unused_if_branch = true\n"
        ),
        "if DEBUG then\n    print(\"dev\")\nelse\n    print(\"live\")\nend\n",
    );
    let root = tmp.path();

    let config = Config::load_or_default(root).unwrap();
    let out = pipeline::run(root, &config, true).unwrap();

    assert!(!out.has_errors(), "{:?}", out.diags);

    let got = read(root, "dist/main.luau");
    assert!(!got.contains("dev"), "{got}");
    assert!(got.contains("live"), "{got}");
    // retain-lines still holds
    assert_eq!(got.lines().count(), 5, "{got}");
}

#[test]
fn a_value_with_no_literal_form_fails_at_load() {
    let tmp = project(
        &format!("{BASE}\n[defines]\nLIST = [1, 2]\n"),
        "return LIST\n",
    );
    let err = Config::load_or_default(tmp.path()).unwrap_err().to_string();

    assert!(err.contains("only booleans"), "{err}");
}

#[test]
fn a_name_luau_cannot_hold_fails_at_load() {
    let tmp = project(
        &format!("{BASE}\n[defines]\n\"not a name\" = 1\n"),
        "return x\n",
    );
    let err = Config::load_or_default(tmp.path()).unwrap_err().to_string();

    assert!(err.contains("valid Luau name"), "{err}");
}

#[test]
fn defines_count_as_a_rule_in_the_summary() {
    use larvae::rules::Family;

    let tmp = project(
        &format!("{BASE}\n[defines]\nDEBUG = false\n"),
        "return DEBUG\n",
    );
    let root = tmp.path();

    let config = Config::load_or_default(root).unwrap();
    let out = pipeline::run(root, &config, true).unwrap();

    assert_eq!(
        out.stats.applied(Family::Native),
        1,
        "{:?}",
        out.stats.rules_applied
    );
}
