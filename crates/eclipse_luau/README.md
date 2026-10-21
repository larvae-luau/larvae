# eclipse_luau

A small, fast Luau syntax layer: lexer, parser, AST, require-site scanner,
lossless printer, and a dense re-emitter. It is the parser under
[larvae](https://github.com/larvae-luau/larvae), published on its own so any
tool can use it.

## The contract

- **Byte ranges are identity.** Every token and node carries byte offsets
  into the source, never a line and a column. A consumer derives those on
  demand. So a transform splices byte ranges against the original text with
  no loss, which is what larvae's whole edit model is built on.
- **The round trip holds.** `print(parse(src)) == src`, byte for byte, for
  every file the parser accepts. A fuzz target holds this invariant, and the
  vendored Luau conformance corpus must round-trip on every commit.
- **Parallelism is file level.** One file parses fast and single threaded.
  `parse_many` spreads a list over a thread pool.

## Syntax coverage

Shipped Luau plus the merged RFCs: classes (`class`/`open`/`extends`,
`public` fields, the metamethod whitelist), export by value (`export
local/const/function`), integer literals (`123i`, `0xABi`, `0b1010i`), and
a bound condition (`if local x = f() then`, `if const`, in the statement and
the expression form).
The contextual keywords stay contextual, so code that uses those words as
names parses as it always did.

## Benchmarks

```
cargo bench -p eclipse_luau                             # this crate alone
cargo bench -p eclipse_luau --features full-moon-bench  # against full_moon
BENCH_CORPUS=/path/to/big/codebase cargo bench ...      # a real corpus
```

Measured 2026-08-26 on an Intel i5-3427U (1.8 GHz, Linux). The corpus is
the vendored Luau conformance suite, filtered to the files both parsers
accept, parsed whole per iteration. Criterion medians over 100 samples:

| parser       | time per pass | relative |
| ------------ | ------------- | -------- |
| eclipse_luau | 13.55 ms      | 1.0×     |
| full_moon    | 89.24 ms      | 6.6×     |

The numbers come from `cargo bench -p eclipse_luau --features
full-moon-bench` with no other load pinned. A different machine moves
both numbers; the ratio is the durable claim. Record new numbers here
when the parser or the corpus changes.
