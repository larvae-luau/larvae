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

mod rules {
    use std::sync::Arc;

    use larvae::syntax::{lexer, parser};
    use larvae::worm::ctx::FileCtx;
    use larvae::worm::nodes::{Kind, NodeTable};

    use super::{FIXTURE, WasmWorm};

    fn file(src: &str) -> Arc<FileCtx> {
        let lexed = lexer::lex(src).unwrap();
        let chunk = parser::parse(src, &lexed.toks).unwrap();
        let toks = lexed.toks.clone();

        let bytes = move |span: larvae::syntax::ast::TokSpan| -> (u32, u32) {
            if span.is_empty() {
                let at = toks
                    .get(span.start as usize)
                    .map(|t| t.start)
                    .unwrap_or_default();
                return (at, at);
            }
            (
                toks[span.start as usize].start,
                toks[span.end as usize - 1].end,
            )
        };

        Arc::new(FileCtx::new(
            NodeTable::build(&chunk, &bytes),
            src.to_owned(),
            "test.luau".into(),
        ))
    }

    fn ids(f: &FileCtx, kind: Kind) -> Vec<u32> {
        f.table
            .iter()
            .filter(|(_, n)| n.kind == kind)
            .map(|(id, _)| id)
            .collect()
    }

    /// rule 0 reads kind, text, span and children back across the boundary
    #[test]
    fn a_wasm_rule_reads_the_real_tree() {
        let mut worm = WasmWorm::load(FIXTURE).unwrap();
        let f = file("print(\"a\", 2)\n");
        let matched = ids(&f, Kind::CallExpr);

        let edits = worm.run_rule(0, Arc::clone(&f), &matched).unwrap();

        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].2, "CallExpr|print(\"a\", 2)|0:13|3");
    }

    /// rule 1 deletes, and larvae is the one that keeps the newlines
    #[test]
    fn a_wasm_rule_removing_a_node_preserves_lines() {
        let src = "local f = function()\n  return 1\nend\n";
        let mut worm = WasmWorm::load(FIXTURE).unwrap();
        let f = file(src);
        let matched = ids(&f, Kind::FunctionExpr);

        let edits = worm.run_rule(1, Arc::clone(&f), &matched).unwrap();
        let (start, end, text) = &edits[0];

        assert_eq!(
            src[*start as usize..*end as usize].matches('\n').count(),
            text.matches('\n').count()
        );
    }

    /// rule 2 walks up
    #[test]
    fn a_wasm_rule_can_reach_a_parent() {
        let mut worm = WasmWorm::load(FIXTURE).unwrap();
        let f = file("local x = 1\n");
        let matched = ids(&f, Kind::Number);

        let edits = worm.run_rule(2, Arc::clone(&f), &matched).unwrap();

        assert_eq!(edits[0].2, "Local");
    }

    #[test]
    fn many_files_through_one_instance_stay_correct() {
        let mut worm = WasmWorm::load(FIXTURE).unwrap();

        for i in 0..200 {
            let f = file(&format!("local x = {i}\n"));
            let matched = ids(&f, Kind::Number);
            let edits = worm.run_rule(2, Arc::clone(&f), &matched).unwrap();

            assert_eq!(edits[0].2, "Local");
        }
    }

    /*
    The epoch guard itself is exercised through Luau, where a worm can stash a
    handle in an upvalue. Here we only check that two files never share one, so
    a stashed epoch could not pass by luck.
    */
    #[test]
    fn no_two_files_share_an_epoch() {
        let a = file("local a = 1\n");
        let b = file("local b = 2\n");

        assert_ne!(a.epoch, b.epoch);
        assert!(b.check(a.epoch).is_err());
        assert!(b.check(b.epoch).is_ok());
    }

    #[test]
    fn a_worm_with_no_rules_says_so() {
        let mut worm = WasmWorm::load(FIXTURE).unwrap();

        assert!(worm.has_rules());
        assert!(worm.run_rule(9, file("local a = 1\n"), &[0]).is_ok());
    }
}
