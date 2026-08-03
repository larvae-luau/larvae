# Contributing

Thanks for looking. Issues and pull requests are both welcome.

## Getting set up

```bash
cargo build
cargo test
```

That is the whole setup. No code generation step, no submodules, no network
during tests.

Before you push, run what CI runs.

```bash
cargo fmt
cargo clippy --all-targets -- -D warnings
cargo test
```

Clippy runs with warnings denied, so a warning fails the build. If you touch
the lexer or the parser, also run the conformance suite on its own, it is the
slow one and the one most likely to catch you out.

```bash
cargo test --test parser
```

## Commit messages

We use [Conventional Commits](https://www.conventionalcommits.org). The
subject line is `type(optional scope): summary`, written in the imperative,
lowercase, and no trailing period.

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
| `refactor` | internal shuffling, no behavior change |
| `docs` | documentation only |
| `test` | tests only |
| `build` | dependencies, cargo config, packaging |
| `ci` | workflow changes |
| `chore` | everything else |

Scopes are optional and free form. The ones that show up most are `requires`,
`lexer`, `parser`, `rules`, `rojo`, `cache`, `cli`, `config` and `ui`.

Put a body on anything that is not obvious from the subject. Explain why, not
what, the diff already says what. Wrap it at 72 columns.

Breaking changes get a `!` after the type, plus a `BREAKING CHANGE:` line in
the body saying what to do instead.

```
feat(config)!: move requires out of the rules list

BREAKING CHANGE: rewriting is configured under [requires] now, the
convert_require rule no longer exists.
```

## Writing style

This applies to comments, docs, and anything a user reads.

No em dashes. Use a comma, or split the sentence. Keep sentences short and
plain. Say "ex:" instead of "e.g.". Skip the trailing period on short labels
and table cells. Comments should read like a person wrote them, so explain the
reasoning rather than restating the code.

Groups of two or more line comments become a block comment. One liners stay
line comments.

## Where things live

Everything runs one direction. A command in `src/commands/` loads the config
and hands it to `src/pipeline/`, which walks the input tree once. For each
file it lexes, scans for requires, resolves them, runs whatever rules are
turned on, and splices the results into the output. Nothing loops back.

Work out which of those steps your change belongs to and you know which
directory to open.

| path | what is in it |
|---|---|
| `src/syntax/` | lexer, AST, printer |
| `src/syntax/parser/` | the parser, split by grammar area into statements, expressions and types |
| `src/requires/` | require resolution and the DataModel map |
| `src/requires/resolve/` | the input forms, output emission, and the checks that keep a rewrite working at runtime |
| `src/project/` | Rojo project files and `.luaurc` |
| `src/rules/` | builtin transforms, darklua parity in one folder and ours in the other |
| `src/rules/edits.rs` | the edit model and the splice every transform ends up in |
| `src/config/` | one file per table in `larvae.toml` |
| `src/pipeline/` | discovery, the parallel loop, writing output |
| `src/commands/` | one file per CLI command |
| `src/ui.rs` | all theming, the brand color lives here and nowhere else |
| `tests/` | end to end and parser conformance |
| `fuzz/` | cargo fuzz targets, nightly only |

Files stay under about 600 lines. When one grows past that, split it into a
directory with a `mod.rs` instead of letting it keep going. The parser, the
resolver, the config and the pipeline were all split that way and they read
better for it.

The require semantics and the DataModel rules are where the surprising
decisions are. Read the code around your change before you start, the
comments explain the reasoning where it is not obvious.

## Adding a rule

Rules live in `src/rules/`. darklua parity rules go in `darklua/` and ours go
in `native/`. A new one needs four things, the implementation, an entry in
`RulesConfig`, and an entry in `larvae.schema.json` so editors know about
it. Add a test in `tests/e2e_rules.rs` showing the before and after.

Register it in that module's `wants` and `apply`. `wants` decides whether a
file gets parsed at all, so a rule that is missing from it will pass its unit
tests and then do nothing in a real build.

A rule walks the tree and pushes byte range replacements, it never sees
another rule's output. If two rules want the same bytes the first one by start
position wins and the second is reported as a warning naming both. Return
`Flow::Skip` from a visitor callback when you have already handled everything
below that node.

Rule names match darklua wherever a darklua equivalent exists. That is
deliberate, a config should port over without renaming anything.

## Touching the parser

The parser is one `Parser` type with its methods spread across the files in
`src/syntax/parser/`. Anything called from another file in that directory is
`pub(super)`, that is the only reason the keyword is there.

Two invariants hold and the test suite enforces both. Parsing then printing
returns the input byte for byte, and every block's statements tile its token
span with no holes. If you add a node, add it to the coverage walk in
`printer.rs` and add a snippet to the corpus in `tests/parser.rs`.

Recursion is depth guarded on purpose. Deeply nested input must produce a
clean error, never a stack overflow.

## Reporting a bug

The most useful report is a small Luau file plus the config that mishandles
it, and what you expected instead. If larvae emitted a require that fails at
runtime, say which container the requiring script lives in, that detail is
usually the whole answer.
