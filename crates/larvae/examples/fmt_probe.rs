//! This harness checks the format invariants over the files it is given.
fn main() {
    let cfg = larvae::fmt::FmtConfig::default();
    let (mut ok, mut bad) = (0u32, 0u32);

    for path in std::env::args().skip(1) {
        let Ok(src) = std::fs::read_to_string(&path) else {
            continue;
        };
        let parses = larvae::syntax::lexer::lex(&src)
            .ok()
            .is_some_and(|l| larvae::syntax::parser::parse(&src, &l.toks).is_ok());

        let out = match larvae::fmt::format(&src, &cfg) {
            Ok(out) => out,
            Err(e) => {
                if parses {
                    println!("FORMAT FAIL {path}: {e:#}");
                    bad += 1
                }
                continue;
            }
        };

        let lexed = match larvae::syntax::lexer::lex(&out) {
            Ok(l) => l,
            Err(e) => {
                println!("OUTPUT DOES NOT LEX {path}: {}", e.message);
                bad += 1;
                continue;
            }
        };

        if let Err(e) = larvae::syntax::parser::parse(&out, &lexed.toks) {
            println!("OUTPUT DOES NOT PARSE {path}: {}", e.message);
            bad += 1;
            continue;
        }

        match larvae::fmt::format(&out, &cfg) {
            Ok(again) if again == out => ok += 1,
            Ok(_) => {
                println!("NOT IDEMPOTENT {path}");
                bad += 1
            }
            Err(e) => {
                println!("SECOND PASS FAILED {path}: {e:#}");
                bad += 1
            }
        }
    }

    println!("{ok} clean, {bad} broken");
}
