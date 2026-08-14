<div align="center">

<img src="assets/larvae.png" alt="larvae" width="140">

# larvae

**Format, lint, and ship Luau.**

One parallel Rust binary. Requires that cannot break, style that cannot
drift, and worms that teach it languages beyond Luau.

[![CI](https://github.com/larvae-luau/larvae/actions/workflows/ci.yml/badge.svg)](https://github.com/larvae-luau/larvae/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/larvae-luau/larvae?color=10E694&label=release)](https://github.com/larvae-luau/larvae/releases/latest)
[![Downloads](https://img.shields.io/github/downloads/larvae-luau/larvae/total?color=10E694&label=downloads)](https://github.com/larvae-luau/larvae/releases)
[![License](https://img.shields.io/badge/license-MIT-10E694)](LICENSE.md)

</div>

larvae is three tools and an extension system in one binary. `larvae
process` rewrites requires and refuses to emit one that would fail at
runtime. `larvae fmt` formats with stylua parity and a Wadler printer.
`larvae lint` carries thirty four lints with selene's spellings. Worms
extend all three to languages that compile to Luau, such as LuauX.

The requires came first, because no other tool has them. Roblox shipped
native string requires, and `@game/...` became available in early 2026. No
tool generated them. larvae does.

```lua
-- what you write
local Signal = require("@pkg/signal")

-- what ships
local Signal = require("@game/ReplicatedStorage/Packages/signal")
```

```toml
# larvae.toml
[aliases]
pkg = "@game/ReplicatedStorage/Packages"
```

That is the whole idea. There are no Instance chains and no sourcemap, and
the output stays readable in a diff.

## Why not darklua

darklua is good software. larvae accepts every darklua rule name, so a user
can port a config with almost no changes. Five things are different.

**It emits native string requires.** darklua only produces Instance chains
such as `script.Parent:FindFirstChild("Foo")`. larvae can also produce
them, with all three indexing styles, but it does not have to.

**It refuses to emit requires that fail at runtime.** Code under
StarterPlayerScripts runs as a clone. An absolute `@game/StarterPlayer/...`
require therefore resolves to the template, not the copy, and module state
silently duplicates. Client code cannot reach ServerScriptService at all.
larvae maps both ends of every require into the DataModel and reports an
error in both cases. No other tool in the ecosystem checks this, and that
includes darklua and luau lsp.

**It is faster by architecture, not by tuning.** larvae processes files in
parallel. A rewrite is a byte range splice, not a full reprint. There is
also an incremental cache. darklua is single threaded, and has been since
the issue was filed in 2021.

| files | larvae cold | larvae warm | darklua | speedup |
|---:|---:|---:|---:|---:|
| 3000 | 27 ms | 14 ms | 424 ms | 15.7x |
| 5000 | 44 ms | 24 ms | 722 ms | 16.4x |
| one 3.5 MB file | 21 ms | 3 ms | 1375 ms | 65.4x |

darklua ran with an empty rule list, so it only parsed and reprinted, while
larvae did the full job. Run `scripts/bench.sh` to reproduce the numbers.

**One Rojo project file.** The usual setup keeps two almost identical
project files: one points at the source for sourcemaps, and one points at
the build output for serving. larvae derives the second file from the first
and keeps it current, so the user edits one file.

**It never runs rojo.** Serving is rojo's job. larvae writes
`.larvae/build.project.json` and does nothing more.

## Why not stylua

stylua is good software, and larvae does not ask a project to leave it.
larvae reads `stylua.toml` as it is and aims at stylua parity for plain
Luau, so trying larvae costs nothing and changes no diffs. Four things are
different.

**It formats languages that stylua cannot.** A worm compiles a language such
as LuauX into the pipeline and formats its files through larvae's own
printer. The markup and the Luau inside it come out in one style, with the
project's width and indentation, because there is one printer.

**It would rather decline than delete a comment.** The formatter checks
every comment against its output at runtime. A file whose formatting would
drop one is left exactly as it was, and the run reports it.

**It has options past stylua.** `magic_trailing_comma`, `trailing_comma`,
`space_inside_braces` and its siblings, and `block_newline_gaps`. Every
stylua option keeps its stylua name.

**The formatter is not alone.** It shares the config, the excludes, the
schema, and the language server with the linter and the transformer. An
excluded file loses its stale diagnostics; a claimed file routes to its
worm; one `larvae.toml` describes all of it.

## Why not selene

The same answer, and the same respect. larvae reads `selene.toml`, keeps
selene's lint names, levels, and `std` spellings, and honors
`-- selene: allow(...)` comments, so a port costs nothing. Four things are
different.

**It carries lints that selene does not have.** Four so far, each for a
problem no existing rule catches, and each cheap enough for a keystroke:
`unreachable_code`, code after a `return`, a `break`, or a `continue`;
`self_assignment`, a value assigned to itself; `loop_invariant_call`, a call
in a loop whose result cannot change between iterations; and
`string_concat_in_loop`, the accumulator pattern that copies the whole
string on every pass.

**It lints languages that selene cannot.** A worm reports findings that only
its own parser can see, and the builtin lints run on the same files through
a byte exact shadow, so a LuauX file reports `unused_variable` at the right
column beside `luaux.useless_fragment`.

**A project can add lints.** A worm declares a lint with a name, a default
level, and a description. The lint sits in `[lint.rules.<worm>]` beside the
builtins, obeys the same levels and the same allow comments, and appears in
`--explain` and in editor completion. selene's lint set is fixed at compile
time.

**The editor knows every lint.** larvae generates a schema for the project,
so `[lint.rules]` completes each name, builtin and worm alike, and hover
shows the description of the lint rather than the meaning of a level.

## Install

```bash
cargo install --path .
larvae self install
```

`self install` copies the binary to `~/.larvae/bin` and prints the line to
add to the shell profile. Run `larvae self update` later to get the latest
release.

## Getting started

```bash
cd my-rojo-project
larvae init      # writes larvae.toml, offers to update .gitignore
larvae process   # writes dist/ and .larvae/build.project.json
rojo serve .larvae/build.project.json
```

A project that already has a `default.project.json` and `.luaurc` aliases
needs no config. Mounts come from the project file, and aliases come from
`.luaurc`. `larvae process` then works without setup.

During editing, `larvae process --watch` rebuilds on each save and mirrors
deletions. When a file does not lex, the command keeps the last good
output. A partial save therefore does not spread require failures into a
live Studio session.

## Commands

| command | what it does |
|---|---|
| `larvae process` | rewrite requires into the output directory |
| `larvae process --watch` | the same, on every save |
| `larvae check` | validate requires and syntax, write nothing, exit non zero on errors |
| `larvae fmt` | format in place; `--check` for CI, `--stdin` for editors |
| `larvae lint` | thirty four lints; `--explain <name>` describes one |
| `larvae lsp` | diagnostics and formatting over stdio, for any editor |
| `larvae worm` | develop an extension: `run`, `run --fmt`, `run --lint`, `info`, `types` |
| `larvae init` | scaffold a config with every default written out |
| `larvae self code` | set up editor completion for larvae.toml |
| `larvae self install` | manage the install, with `update` and `uninstall` |

`check` is the CI gate. It reports requires that do not resolve, realm
violations, alias cycles, and syntax errors. It also counts the dynamic
requires that it intentionally did not change.

## Configuration

Every key is optional. Run `larvae self code` to get completion and hover
docs. The command sets up Even Better TOML when that extension is
installed. When the extension is absent, the command writes a schema line
in the config instead.

```toml
[aliases]
pkg = "@game/ReplicatedStorage/Packages"

[process]
input = "src"
output = "dist"
quotes = "preserve"        # or double, or single

[requires]
target = "roblox-string"   # or path for Lune, or roblox-instance
strict = false

[rules]
const_requires = true      # local X = require(...) becomes const X = require(...)
add_luau_directive = "strict"
```

An unknown key is a hard error. A key for a feature that does not exist yet
reports the release that adds the feature. larvae ignores nothing silently.

## Status

Requires, formatting, linting, the language server, the Rojo integration,
compile time constants, build profiles, the rule set, the cache, and watch
mode all work today.

Worms work today as well. A worm is an extension that adds a language on top
of Luau: it compiles its files into the pipeline, formats them through
larvae's own printer, and lints them beside the builtin lints. A worm ships
as a GitHub release, as a crate on crates.io, or as a path during
development. The [first worm](https://github.com/larvae-luau/luaux) is [LuauX](https://github.com/luau-xml/luaux),
Luau with JSX syntax.

The next work items are bundling with a documented module init order and
cross module dead code elimination, then minify.

## License

MIT, see [LICENSE.md](LICENSE.md).
