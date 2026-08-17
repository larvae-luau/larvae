Luau refuses these files and larvae parses them.

Two reasons, and the split matters.

Most of them are type structure. The parser consumes a type for its extent
and never interprets it, which `crates/larvae/src/syntax/parser/types.rs`
states at the top. A formatter and a linter need to know where a type stops,
not whether its generic parameters are in a legal order. Refusing these would
mean writing a second type checker beside the one Luau already ships.

The rest are gaps: `const` arity and the interpolation forms are not type
structure, and larvae could refuse them.

The test holds the line that the leniency stays inert. Each file must format
to a stable result and keep every one of its non-whitespace bytes. Leniency is
harmless while larvae only declines to complain. It stops being harmless the
day a lint or a transform reasons about a construct that larvae read wrongly,
because then the output is wrong rather than the error missing.

`larvae check` is the one command where this costs something. It is the gate a
project runs in CI, and a file it passes that the compiler refuses is a hole
in that gate.
