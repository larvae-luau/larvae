/*!
The upstream Luau conformance corpus, as a parser regression net.

Every file in the fixture directory must lex, parse, tile its token stream
with no holes, and print back byte for byte. The corpus holds real
programs that exercise the whole language, classes, integer literals, and
deliberately hostile string content included. A parser change that breaks
any of them fails here first, with the file named.
*/

use larvae::syntax::{lexer, parser, printer};

#[test]
fn every_conformance_file_round_trips() {
    let dir = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/parser/luau-conformance"
    );

    let mut checked = 0usize;
    let mut failures: Vec<String> = Vec::new();

    for entry in std::fs::read_dir(dir).expect("the corpus ships with the repo") {
        let path = entry.expect("reads").path();

        if path.extension().and_then(|e| e.to_str()) != Some("luau") {
            continue;
        }

        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("?")
            .to_string();

        let bytes = std::fs::read(&path).expect("reads");
        let (src, _) = larvae::sys::utf8_stand_in(bytes);

        let lexed = match lexer::lex(&src) {
            Ok(l) => l,

            Err(e) => {
                failures.push(format!("{name}: lex error at {}: {}", e.offset, e.message));

                continue;
            }
        };

        let chunk = match parser::parse(&src, &lexed.toks) {
            Ok(c) => c,

            Err(e) => {
                failures.push(format!(
                    "{name}: parse error at {}: {}",
                    e.offset, e.message
                ));

                continue;
            }
        };

        let holes = printer::coverage_errors(&chunk);

        if !holes.is_empty() {
            failures.push(format!("{name}: coverage holes {holes:?}"));

            continue;
        }

        if printer::print_chunk(&src, &lexed.toks, &chunk) != src {
            failures.push(format!("{name}: the print differs from the source"));

            continue;
        }

        checked += 1;
    }

    assert!(failures.is_empty(), "{failures:#?}");
    assert!(checked >= 55, "the corpus shrank to {checked} files");
}
