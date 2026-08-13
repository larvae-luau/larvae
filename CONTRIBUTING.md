# Contributing

Thank you for your interest. Issues and pull requests are both welcome.

## Getting set up

```bash
cargo build
cargo test
```

That is the whole setup. There is no code generation step, there are no
submodules, and the tests use no network.

Before you push, run the same commands that CI runs.

```bash
cargo fmt
cargo clippy --all-targets -- -D warnings
cargo test
```

Clippy runs with warnings denied, so a warning fails the build. If you
change the lexer or the parser, also run the conformance suite on its own.
It is the slow suite, and it finds the most mistakes.

```bash
cargo test --test parser
```

## Commit messages

We use [Conventional Commits](https://www.conventionalcommits.org). The
subject line is `type(optional scope): summary`. Write it in the
imperative, in lowercase, and with no trailing period.

```
feat(requires): resolve aliases through the luaurc chain
fix(lexer): stop long strings from swallowing the closing bracket
docs: write the contributing guide
```

Types we use:

| type | when |
|---|---|
| `feat` | new behavior a user can see |
| `fix` | a bug fix |
| `perf` | faster with no behavior change |
| `refactor` | internal restructuring, no behavior change |
| `docs` | documentation only |
| `test` | tests only |
| `build` | dependencies, cargo config, packaging |
| `ci` | workflow changes |
| `chore` | everything else |

Scopes are optional and free form. The most common scopes are `requires`,
`lexer`, `parser`, `rules`, `rojo`, `cache`, `cli`, `config` and `ui`.

Add a body when the subject does not explain the change. Explain why, not
what; the diff already says what. Wrap the body at 72 columns.

Mark a breaking change with a `!` after the type. Add a `BREAKING CHANGE:`
line in the body that says what to do instead.

```
feat(config)!: move requires out of the rules list

BREAKING CHANGE: rewriting is configured under [requires] now, the
convert_require rule no longer exists.
```

## Writing style

These rules apply to comments, docs, and all text that a user reads.

Do not use em dashes. Use a comma, or split the sentence. Keep sentences
short and plain. Write "ex:" and not "e.g.". Omit the trailing period on
short labels and table cells. A comment must read as if a person wrote it,
so explain the reasoning and do not restate the code.

Turn a group of two or more line comments into a block comment. Keep a
single line comment as a line comment.

## Where things live

All data flows in one direction. A command in `src/commands/` loads the
config and passes it to `src/pipeline/`, which walks the input tree once.
For each file, the pipeline lexes, scans for requires, resolves them, runs
the enabled rules, and splices the results into the output. Nothing flows
back.

Find the step that your change belongs to. That step tells you which
directory to open.

| path | what is in it |
|---|---|
| `src/syntax/` | lexer, AST, printer |
| `src/syntax/parser/` | the parser, split by grammar area into statements, expressions and types |
| `src/requires/` | require resolution and the DataModel map |
| `src/requires/resolve/` | the input forms, output emission, and the checks that keep a rewrite working at runtime |
| `src/project/` | Rojo project files and `.luaurc` |
| `src/rules/` | builtin transforms, darklua parity in one folder and ours in the other |
| `src/rules/edits.rs` | the edit model and the splice that every transform uses |
| `src/config/` | one file per table in `larvae.toml` |
| `src/pipeline/` | discovery, the parallel loop, writing output |
| `src/commands/` | one file per CLI command |
| `src/ui.rs` | all theming, the brand color lives here and nowhere else |
| `tests/` | end to end and parser conformance |
| `fuzz/` | cargo fuzz targets, nightly only |

Keep files under about 600 lines. When a file grows past that limit, split
it into a directory with a `mod.rs`. Do not let the file grow more. We
split the parser, the resolver, the config and the pipeline that way, and
the result reads better.

The require semantics and the DataModel rules contain the surprising
decisions. Read the code around your change before you start. The comments
explain the reasoning where it is not obvious.

## Adding a rule

Rules live in `src/rules/`. Put darklua parity rules in `darklua/` and our
own rules in `native/`. A new rule needs four things: the implementation,
an entry in `RulesConfig`, an entry in `larvae.schema.json` so that editors
know the rule, and a test in `tests/e2e_rules.rs` that shows the before and
after.

Register the rule in that module's `wants` and `apply`. `wants` decides if
larvae parses a file at all. A rule that is absent from `wants` passes its
unit tests and then does nothing in a real build.

A rule walks the tree and pushes byte range replacements. It never sees
another rule's output. When two rules want the same bytes, the rule with
the first start position wins. larvae reports the second rule as a warning
that names both rules. Return `Flow::Skip` from a visitor callback when you
have already handled all nodes below that node.

Rule names match darklua where a darklua equivalent exists. That is
deliberate: a config must port without a rename.

## Touching the parser

The parser is one `Parser` type. Its methods are divided across the files
in `src/syntax/parser/`. A method that another file in that directory calls
is `pub(super)`. That is the only reason for the keyword.

Two invariants hold, and the test suite enforces both. First, a parse
followed by a print returns the input byte for byte. Second, every block's
statements tile the block's token span with no holes. If you add a node,
add it to the coverage walk in `printer.rs`. Also add a snippet to the
corpus in `tests/parser.rs`.

The recursion has an intentional depth guard. Deeply nested input must
produce a clean error, never a stack overflow.

## Reporting a bug

The most useful report contains a small Luau file, the config that
mishandles it, and the output that you expected. If larvae emitted a
require that fails at runtime, state the container that holds the requiring
script. That detail is usually the whole answer.
