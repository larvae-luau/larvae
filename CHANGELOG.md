# Changelog

Notable changes land here. Format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), versions follow
[semver](https://semver.org/spec/v2.0.0.html).

## Unreleased

### Added

- `larvae worm update`, which bumps every worm in larvae.toml to its latest
  release: the newest GitHub release for a `repo@version` source, the newest
  stable crate for a `cargo` source, and nothing for a path worm. `--check`
  reports what waits and fails without writing, for CI. The edit is textual
  and keeps every other byte, comments included, and a worm that an
  `extends` base declares is reported rather than edited behind its owner
- The editor reports a config that fails to resolve. The server raised no
  sign before, so a broken larvae.toml meant defaults for a whole session
  and an editor that looked broken in a quiet way. One warning toast now
  carries the reason, on start and on each settings reload, and the server
  still serves defaults until the config loads. A project without a
  larvae.toml stays quiet, because zero config is a supported state

## 0.3.0 - 2026-08-17

### Added

- Flag comments that hold a tool off over a span of lines. `-- larvae: fmt
  off` runs to the matching `-- larvae: fmt on`, or to the end of the file
  when no `on` follows, so one marker at the top of a file holds the whole
  file. `fmt off(5)` holds the marker line and five lines below it. `lint`
  reads the same way, and a `lint` marker holds every lint where `allow(...)`
  names the lints it holds
- stylua's `ignore start` and `ignore end` are read as `fmt off` and `fmt on`,
  so a project that comes from stylua keeps the markers already in its files
- A file that a worm claims reads the markers too. The comments come from the
  reply of the worm, because larvae does not read such a file as Luau and so
  finds no comment in it itself
- `format` is read as a name for the formatter beside `fmt`, in each of the
  three forms, because an author reaches for either one
- `[fmt] if_expression`, which lays out Luau's `if` expression. `expand`
  takes `never`, `always`, or `when-large`; `width` gives the boundary that
  `when-large` measures against; `style` takes `block` or `leading`;
  `placement` selects whether the `if` stays on the line of the binding;
  `indent` gives the levels a continuation line takes. `expand` is `never` by
  default, and that layout is what larvae always wrote. A nested expression
  waits for `width` in every mode, because `always` at each level gives a
  stair of keywords for an expression that reads well on one line

- Differential tests against the real Luau parser. `luau-lsp` carries that
  parser and reports a syntax error apart from a type error, so it serves as
  an oracle. The verdicts are recorded under
  `crates/larvae/tests/fixtures/parser`, so CI needs no Luau, and
  `scripts/parser_oracle.sh` makes the recording and sweeps a corpus of your
  own
- `scripts/bench_selene.sh`, which measures the linter against selene

### Fixed

- A string continuation reads over CRLF line endings. A Windows checkout
  turns LF into CRLF, and the `\` before the end of the line is then three
  bytes. The lexer took two, which left a bare LF inside the literal, and
  larvae refused a file that Luau accepts
- A comment keeps the blank line below it. The formatter gave a leading
  comment a plain line break, so a note that an author had separated from the
  code below it came back joined to that code
- Three constructs that Luau parses and larvae refused. The `\z` escape, which
  takes the whitespace that follows it and is how a long string is written
  over several lines. A leading `|` or `&` in a function return type,
  `() -> | string | number`. And a variadic type holding a union,
  `(..."hit" | "miss") -> ()`. A file using any of them was refused by every
  command
- The editor no longer offers one value two times. A boolean carried a type
  and a default and no list of values, and Taplo adds the default to a list it
  built from the type, so a boolean that defaults to false offered false two
  times. And the `[fmt]` and `[lint.rules]` tables each held an open
  `additionalProperties` beside their `properties`, so Taplo read both
  branches for a worm and offered every lint of that worm two times
- Every marker form now holds in a file that a worm claims. A worm sends the
  layout for the whole file, so a marker held the Luau of a region and not
  the markup around it: with `attribute_per_line` on, the attributes of an
  element inside `fmt off` still moved. Larvae writes the source back over
  each held region after it renders what the worm sent

## 0.2.0 - 2026-08-16

### Added

- `[fmt] semicolons`, which takes `always` or `never`. Luau needs a
  semicolon in one place only, before a statement that opens with `(`, and
  larvae writes that one whatever the setting says. So `never` and
  `as-needed` name the same output, and both spellings are accepted
- `[fmt] final_newline`, on by default. It covers the newline at the end of
  the file only: larvae removes whitespace at the end of every line whatever
  the setting says. editorconfig calls it `insert_final_newline`, and that
  name works too
- `[fmt] call_parentheses = "as-needed"`, the name Biome and Prettier use for
  what stylua spells `none`. The two select the same output. Luau accepts the
  bare call for one string or one table and nothing else, so `h(a)` keeps its
  parentheses
- Fourteen lints, which are the lints of the Luau compiler that larvae did
  not have: `builtin_global_write`, `placeholder_read`, `unknown_type`,
  `implicit_return`, `duplicate_local`, `format_string`,
  `uninitialized_local`, `duplicate_function`, `table_operations`,
  `misleading_and_or`, `bad_comment_directive`, `number_literal_overflow`,
  `comparison_precedence`, and `zero_step_loop`. Larvae now covers all 28
  lints of the Luau compiler, and the registry holds 49
- `non_const_require`, which reports `local X = require(...)` where `const X`
  says more. It is off by default, and it skips a name that the file
  reassigns, because `const` would then be a syntax error
- `larvae init` writes `"lint": { "*": false }` into `.luaurc`, so one linter
  reports. The edit is textual and keeps every other byte, comments included.
  The command changes nothing when the file already sets `lint`, and creates
  no `.luaurc` when the project has none
- Each option in `[fmt]` carries a link to its own anchor on the docs site.
  Even Better TOML renders a description as Markdown, so the link is what a
  reader clicks in the hover card

- Minification: `generator = "dense"` re-emits the output tokens with the
  least whitespace that lexes the same, so the program cannot change
  meaning. The `[minify]` table tunes it, `column_span` keeps line numbers
  useful and `rename_variables` shortens locals for dense builds only
- `generator = "readable"` prints the output through the formatter, in the
  `[fmt]` style of the project. The generator also prints the
  `larvae bundle` output
- A root `exclude` / `include` pair in `larvae.toml` that every command
  inherits. The include of an area wins over every exclude, the exclude of
  an area wins over the root include, and the root include cancels the root
  exclude alone
- Root short forms `input`, `output`, and `target` for the keys every
  project sets, so the first line of a config needs no table header.
  Writing both spellings of one key is an error
- `extends`, a base config that a file layers over by relative path, with
  the merge rules of `[profile]`. Chains resolve, loops are refused, and a
  base can hold the profiles of a whole workspace

### Changed

- `implicit_return` and `multiple_statements` report by default. Both are
  lints that the Luau compiler reports, and a project that turns Luau's
  linter off must lose no report. `multiple_statements` was off because it
  appeared to report `if x then return end`; that was a defect in the lint,
  which compared every statement in the file instead of siblings
- The parser refuses the ambiguous call that Luau refuses. A `(` that opens a
  line after a complete expression reads as a call of the line above and as a
  new statement, and Luau asks for a `;`. Larvae read it as a call, so
  `larvae check` passed a file that the compiler rejects and `larvae fmt`
  joined the two lines into a reading the author never chose
- `larvae init` proposes one input root. It folds sibling mounts into the
  directory that holds them, so a Rojo project with `src/client`,
  `src/server` and `src/shared` gets `input = "src"`. It also stops
  proposing an alias that `.luaurc` already defines
- `larvae self code` writes the generated schema again whenever one is on
  disk, and not only when the project has worms. It then says to run
  "Developer: Reload Window", because the editor holds the schema in memory

- `[process] include` and `[process] exclude` match relative to the project
  root now, like every other list, and the exclude follows the same
  directory-name rule. Patterns written against `input` need respelling
- `larvae init` writes the root short forms and stops listing every default
  as a comment; the docs and the schema hold the full lists
- Smaller dependency tables behind the same behavior: URL parsing keeps the
  compact unicode backend, and the TOML parser dropped its edit layer

### Fixed

- `larvae self install` works while something runs the installed binary. The
  copy opened the destination for writing, which no process can do to a
  running executable, so an editor that ran `larvae lsp` from the installed
  path failed the install with "Text file busy". The bytes now rename into
  place
- A semicolon at the edge of a Luau span that a worm named. A worm draws its
  spans from its own parse and can end one at the last token of a statement,
  which left the `;` outside and made `semicolons` work on some statements of
  a file and not others. The other edge was worse: a `;` that opened a span
  read as a stray statement and went, and the result was the ambiguous call
  above, so larvae wrote a file that larvae cannot read again
- `larvae init` treats `packages/roblox` as a package tree. The check read
  the last component of the path only, found `roblox`, and proposed
  processing the dependencies of the project
- `[fmt] insert_final_newline` is accepted. The merge writes the config to a
  table first, so the alias arrived as a second key for one field and serde
  refused the pair as a duplicate field
- A non-ASCII byte outside a string no longer ends the run. The lexer made a
  one byte token of it, which cut the character in half, and the next slice
  of that span panicked
- An unterminated `[[` reports. The lexer read it to the end of the file, so
  it reported nothing and returned content two bytes short of its own

## 0.1.1 - 2026-08-14

### Added

- The require graph and `larvae check` gates under `[check]`: `cycles`,
  `unused_modules`, `early_require`, and `entries`
- `larvae bundle`: one tree-shaken file with a lazy module registry, so
  bundling cannot move a side effect and a load-time cycle errors naming
  the module
- `larvae sync-luaurc` writes the merged aliases back into `.luaurc`
- `[fmt] require_binding` selects the keyword that binds a required module,
  and the `non_const_require` lint reports the requires that `const` cannot
  bind

### Fixed

- `const_requires` skips a binding that a later statement reassigns, which
  would have been a syntax error under Luau's `const`
- Linux release builds ship, built against musl with a C++ toolchain

### Performance

- The require graph is harvested only when `check` or `bundle` reads it, so
  a plain build pays nothing for it
- The lint report renders into one buffer and one write

## 0.1.0 - 2026-08-13

### Added

- Require rewriting with three output targets, native Roblox string requires,
  filesystem paths for Lune, and Instance expressions with `find_first_child`,
  `wait_for_child` or `property` indexing
- Aliases from `larvae.toml` and `.luaurc`, merged per key, with chain and
  cycle handling
- Realm and container validation, client code cannot require server only
  containers and Starter containers only ever get relative requires
- Rojo integration, mounts derived from `default.project.json` and a build
  project written to `.larvae/build.project.json`
- A Luau parser and printer, round trips byte for byte, used by `check` to
  report syntax errors
- Incremental builds keyed on a resolution epoch, plus `process --watch`
- Formatting with stylua parity and options beyond it, and linting with the
  selene rule set, both reading the config files those tools leave
- The worm system: extensions in three forms, `luau`, `wasm`, and `native`.
  A worm transforms, formats, and lints the files it claims: a format reply
  is a layout document that larvae renders in the style of the project, and
  a lint reply is stamped with levels from `[lint.rules]`
- A worm namespaces its options and lints under its own key,
  `[fmt.<worm>]` and `[lint.rules.<worm>]`, and each lint reads
  `worm.name` in messages, in `--explain`, and in allow comments
- Inheritance controls per worm: builtin lints on claimed files,
  `[worms.<name>.inherit]` with `lints_only`, `lints_except`, and
  `fmt_except`
- A cargo install channel for worms, `cargo = "crate@version"`, beside the
  GitHub release and path channels
- A generated per-project schema, connected to the editor by
  `larvae self code`
- Rules, `const_requires`, `remove_comments`, `append_text_comment` and
  `add_luau_directive`, with every darklua rule name accepted
- `larvae init`, `larvae self code`, and `larvae self install`, `update`
  and `uninstall`

### Fixed

- `larvae-worm` links into a worm that is not wasm. The node API of the wasm
  form declared its host functions on every target, so `link.exe` refused a
  native worm over nine unresolved names and bound the tenth, `remove`, to the
  function of the C library that deletes a file
