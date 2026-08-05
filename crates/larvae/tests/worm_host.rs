//! Both halves of the worm ABI against each other, over a real wasm module
//!
//! `tests/fixtures/echo_worm.wasm` is built from `crates/echo-worm`, which uses
//! the `larvae_worm::frontend!` macro a real worm would. Rebuild instructions
//! are at the top of that crate's manifest.

use larvae::worm::WasmWorm;

const FIXTURE: &[u8] = include_bytes!("fixtures/echo_worm.wasm");

fn load() -> WasmWorm {
    WasmWorm::load(FIXTURE).expect("the fixture worm loads")
}

#[test]
fn a_string_survives_the_round_trip() {
    let out = load().transform("hello", "cfg").unwrap();

    assert!(out.ok);
    assert_eq!(out.text, "hello|cfg");
}

#[test]
fn utf8_and_newlines_cross_intact() {
    let src = "local s = \"héllo ✓\"\nreturn s\n";
    let out = load().transform(src, "").unwrap();

    assert!(out.ok);
    assert_eq!(out.text, format!("{src}|"));
}

#[test]
fn an_empty_file_is_not_a_special_case() {
    let out = load().transform("", "").unwrap();

    assert!(out.ok);
    assert_eq!(out.text, "|");
}

/// Nothing here should grow the guest heap without bound, so hammer one instance
#[test]
fn one_instance_serves_many_files() {
    let mut worm = load();

    for i in 0..500 {
        let src = format!("file {i}");
        let out = worm.transform(&src, "cfg").unwrap();

        assert_eq!(out.text, format!("{src}|cfg"));
    }
}

#[test]
fn a_worm_reporting_a_problem_is_not_an_error() {
    let out = load().transform("FAIL", "cfg").unwrap();

    assert!(!out.ok);
    assert!(out.text.contains("refused"), "{}", out.text);
    assert!(out.text.contains("cfg"), "{}", out.text);
}

#[test]
fn into_source_turns_a_reported_problem_into_one() {
    let err = load()
        .transform("FAIL", "")
        .unwrap()
        .into_source()
        .unwrap_err();

    assert!(err.to_string().contains("refused"));
}

/// A worm bug reaches us as a trap, and must stay recoverable so one bad file
/// does not take down a watch session
#[test]
fn a_trap_is_an_error_and_not_a_crash() {
    let err = load().transform("TRAP", "").unwrap_err();

    assert!(err.to_string().contains("trapped"), "{err}");
}

#[test]
fn a_trap_does_not_poison_the_rest_of_the_build() {
    let mut worm = load();

    assert!(worm.transform("TRAP", "").is_err());

    // the same instance keeps working, which is what makes per file recovery viable
    let out = worm.transform("after", "cfg").unwrap();

    assert_eq!(out.text, "after|cfg");
}

#[test]
fn junk_is_not_a_worm() {
    assert!(WasmWorm::load(b"not a wasm module at all").is_err());
}
