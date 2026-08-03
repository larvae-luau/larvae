// Anything that parses must print back byte for byte and tile the tokens
#![no_main]

use libfuzzer_sys::fuzz_target;

use larvae::syntax::{lexer, parser, printer};

fuzz_target!(|data: &[u8]| {
    let Ok(src) = std::str::from_utf8(data) else {
        return;
    };
    let Ok(lexed) = lexer::lex(src) else { return };
    let Ok(chunk) = parser::parse(src, &lexed.toks) else {
        return;
    };
    let holes = printer::coverage_errors(&chunk);
    assert!(holes.is_empty(), "coverage holes: {holes:?}");
    let printed = printer::print_chunk(src, &lexed.toks, &chunk);
    assert_eq!(printed, src, "round trip changed the source");
});
