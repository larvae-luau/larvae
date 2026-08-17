/*!
Differential tests against the real Luau parser.

Larvae has its own parser, so the question "does it read Luau the way Luau
does" has to be answered by comparison and not by assertion. The answer that
matters is a verdict: Luau accepts a file, or it refuses it, and larvae has to
agree.

The verdicts here are recorded, not computed, so CI needs no Luau. The three
directories say what each file is:

- `accept` Luau parses it, and so must larvae. A miss here refuses a file that
  a user wrote correctly, and no command works on it.
- `reject` Luau refuses it, and so must larvae. A miss here lets `larvae
  check` pass a file that the compiler will not build.
- `lenient` Luau refuses it and larvae parses it. These are known, and the
  test holds them to the property that makes them harmless: the output is
  stable and keeps every non-whitespace byte. See the README in that
  directory.

`scripts/parser_oracle.sh` re-derives the verdicts from a real `luau-lsp` and
reports where the recording has fallen behind. Run it when Luau changes, or
over a corpus of your own.
*/

use std::path::{Path, PathBuf};

use larvae::syntax::{lexer, parser};

fn dir(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/parser")
        .join(name)
}

/// The Luau files in one fixture directory, by name, sorted for a stable report.
fn files(name: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = std::fs::read_dir(dir(name))
        .unwrap_or_else(|e| panic!("cannot read the {name} fixtures, {e}"))
        .filter_map(|entry| {
            let path = entry.ok()?.path();

            if path.extension()? != "luau" {
                return None;
            }

            let text = std::fs::read_to_string(&path).ok()?;

            Some((path.file_name()?.to_string_lossy().into_owned(), text))
        })
        .collect();

    out.sort();

    assert!(!out.is_empty(), "the {name} fixtures are missing");

    out
}

/// Reports if larvae reads this source as Luau.
fn parses(src: &str) -> bool {
    match lexer::lex(src) {
        Ok(lexed) => parser::parse(src, &lexed.toks).is_ok(),

        Err(_) => false,
    }
}

/*
A file that Luau parses must reach every command of larvae.

This is the direction that costs a user something. A file larvae refuses
cannot be formatted, linted, or transformed, and the user has correct Luau
that the tool will not read.
*/
#[test]
fn larvae_parses_what_luau_parses() {
    let refused: Vec<String> = files("accept")
        .into_iter()
        .filter(|(_, text)| !parses(text))
        .map(|(name, _)| name)
        .collect();

    assert!(
        refused.is_empty(),
        "Luau parses these and larvae does not: {refused:#?}"
    );
}

/*
A file that Luau refuses must not pass `larvae check`.

The check is the gate a project runs in CI. A file it passes that the compiler
will not build makes the gate say less than the compiler, which is the reason
the ambiguous call case was fixed.
*/
#[test]
fn larvae_refuses_what_luau_refuses() {
    let accepted: Vec<String> = files("reject")
        .into_iter()
        .filter(|(_, text)| parses(text))
        .map(|(name, _)| name)
        .collect();

    assert!(
        accepted.is_empty(),
        "Luau refuses these and larvae parses them: {accepted:#?}"
    );
}

/*
The known leniency has to stay inert.

Larvae parses these and Luau does not. That costs nothing while larvae only
declines to complain. It starts costing something when larvae reads a
construct wrongly and then writes it back, so each file must format to a
stable result and keep every one of its non-whitespace bytes.
*/
#[test]
fn the_known_leniency_changes_no_code() {
    let cfg = larvae::fmt::FmtConfig::default();

    for (name, text) in files("lenient") {
        assert!(
            parses(&text),
            "{name} is recorded as leniency, but larvae refuses it. \
             Move it to the reject directory."
        );

        let Ok(once) = larvae::fmt::format(&text, &cfg) else {
            panic!("{name} parses but does not format");
        };

        let twice = larvae::fmt::format(&once, &cfg).expect("the output formats");

        assert_eq!(once, twice, "{name} does not format to a stable result");

        let before: String = text.chars().filter(|c| !c.is_whitespace()).collect();
        let after: String = once.chars().filter(|c| !c.is_whitespace()).collect();

        assert_eq!(before, after, "{name} lost or gained code");
    }
}

/// A fixture in two directories at once would make one of the tests a lie.
#[test]
fn no_fixture_is_recorded_two_times() {
    let mut seen: Vec<String> = Vec::new();

    for group in ["accept", "reject", "lenient"] {
        for (name, _) in files(group) {
            assert!(!seen.contains(&name), "{name} is recorded in two places");

            seen.push(name);
        }
    }
}
