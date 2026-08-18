# The Luau conformance corpus

These files come from `luau-lang/luau`, `tests/conformance`, as of
2026-08-17. Luau is MIT licensed; see the upstream repository for the
license text. The files are test data here and larvae never ships them.

The suite in `tests/luau_conformance.rs` demands that every file lexes,
parses, tiles its token stream with no holes, and prints back exactly.
Three files hold bytes that are not UTF-8 on purpose, and they go through
the same stand-in that `larvae check` and `larvae lint` use.

To refresh: download `tests/conformance/*.luau` from the upstream default
branch over this directory, run the suite, and read every new failure
before touching the parser. A failure can mean upstream grew syntax, and
it can mean a file tests an extension larvae must not accept.
