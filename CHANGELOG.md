# Changelog

Notable changes land here. Format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), versions follow
[semver](https://semver.org/spec/v2.0.0.html).

## Unreleased

### Added

- `larvae-lsp` is a crate of its own on crates.io, so `cargo install
  larvae-lsp` builds the server. The binary carries its analyzer
  library inside and writes it out on first run, under
  `~/.larvae/lib/<version>`, so it works from wherever the install put
  it; a copy beside the binary still wins when there is one. A library
  the loader refuses used to kill the process before `main`, and now
  ends in a sentence. The crate depends on larvae with no default
  features, and larvae declares none today

### Changed

- Inlay hints compute on every request, against the text the editor
  holds at that request, the way luau-lsp computes them. The hold that
  served the last hints while the author typed is gone with its cache,
  its settle thread, and the refresh it sent after each pause: a held
  hint was a hint from an older text, and that is where a hint in the
  middle of a word came from, and where the hints that stopped after a
  refresh came from. The editor asks after an edit and the answer is
  current. `[lsp.inlay_hints] update_delay` stays readable and does
  nothing
- The server asks the editor to redraw hints only when a hint setting
  changed, which is when luau-lsp asks. Every settings message used to
- The flags of a doc comment read as bold lines again, `**Private**`
  above the prose, the way every other tag reads
- An entry of a doc section reads as a row: the name and the type each
  take a code span and an em dash opens the description, ex:
  `` `player` `Player` — who joined ``. One run of words left the
  reader to find the parts

### Added

- `@lifecycle` in a doc comment says a framework calls the member, so
  `oop.unused_private` leaves it alone. chief calls the lifecycle of a
  provider and the file that declares one never does, so the lint told
  the author to delete the method the framework needs. The tag colours
  like the rest

## 0.8.0-canary.1

### Added

- A doc comment opened above a declaration offers its moonwave block:
  the name, a line per parameter with the type the author wrote, and
  the return. A name that opens with an underscore takes `@private`,
  the convention that already hides it from a completion list
- The moonwave tags of a comment read as tags. `@param`, `@return`,
  `@private` and the rest take the colour of a keyword, and the word a
  tag names takes the colour of a parameter. A tag counts only at the
  start of its line, so a mail address in prose stays prose

### Changed

- The flags of a doc comment read as a section of their own above the
  prose: a quote holds them, and each flag is a code span, so an
  editor draws its accent bar and its chips around them. As bold lines
  they stacked and pushed the description down the card

### Fixed

- `larvae self install` fetches `larvae-lsp` and its analyzer library
  when they are not beside the binary. A tool manager installs the one
  binary its manifest names, so a larvae from rokit, aftman, or ember
  sat alone and the editor fell back to `larvae lsp`, which serves the
  lints and none of the types, with nothing said about why. The three
  files ship in one release archive, and the install takes the two it
  is missing
- `larvae self install` clears the quarantine macOS puts on a
  downloaded file, and says so when the installed server does not
  start. The analyzer is a library the server links at load, so a file
  Gatekeeper refuses kills the process before it writes a line: the
  editor saw a server that answered nothing and reported nothing. The
  install runs the server once and prints the loader's own words when
  it fails
- `larvae-lsp --version` answers and stops, so a check that the server
  starts is a check and not a session reading an empty stdin
- The names inside an interpolated string take the colour code takes.
  The lexer keeps a backtick string as one token, so the whole thing
  painted as string and `{name}` read as text rather than as the value
  it interpolates. Each hole is now read on its own: a name is a
  variable, a name after a dot is a property, and a keyword, a number,
  and a nested string keep their own colours. An escaped `\{` opens no
  hole
- A held inlay hint no longer vanishes. A hint on a line the author
  rewrote or moved was dropped until the pause, because its column
  belonged to text that changed and a stale one reads as
  `props: Pr: ()ops`. It keeps its line and moves to the end of it
  instead, which is always a boundary, and the pause puts it back
  where it belongs


### Added

- An accept writes a type another module declares. The printer writes
  a required module's type bare, ex: `Query`, and that name means
  nothing in the file the accept writes into, so such a hint stayed
  display only. `[lsp.inlay_hints] accept_imports` decides what it
  writes instead: `qualify` writes it through the binding that
  requires the module, `jecs.Query`; `alias` writes the bare name and
  adds `type Query = jecs.Query` with the imports; `off` keeps the old
  behaviour. A name two required modules both export writes nothing,
  because a guess between them puts the wrong type in someone's file

### Added

- `[lsp] roblox_security_level` picks the Roblox API surface the
  analyzer types against: `none`, `local-user-security`,
  `plugin-security`, or the default `roblox-script-security`, which
  holds every member and is what larvae has always loaded. A plugin
  sees less than that and a live game less again, so a project that
  ships to one of those now gets the surface it really has, in the
  editor and in `larvae analyze` alike. luau-lsp names the same four
  through `types.robloxSecurityLevel`. The four definition sets ship
  compressed, so the binary holding all of them is smaller than the
  one that held a single set

### Fixed

- `[aliases]` of `larvae.toml` resolve with no `.luaurc` in sight. The
  config says its entries merge over that file per key, and the build
  has read both since the beginning, but the analyzer read `.luaurc`
  alone: a project that wrote its aliases in one file got
  `Unknown type` from `larvae analyze` and from the editor. A value
  that names a place in the DataModel goes through the mounts, which
  is the form `.luaurc` cannot express

### Changed

- The inlay hints compute on every request, the way luau-lsp answers
  them, and the editor holds each hint against its own edits until the
  answer arrives. That is what keeps a hint with its text. larvae used
  to hold the hints for `[lsp.inlay_hints] update_delay` and ask the
  editor to redraw at the pause, and a redraw of held hints is what
  moved one away from the text it describes. The delay is `0` now, and
  a project that wants less inference on a slow machine sets one

### Fixed

- The editor is asked to redraw the hints when the hint settings
  change and at no other time, which is the one case luau-lsp asks in.
  A redraw after every pause, and after every edit to a file another
  file requires, is what made the hints flicker

## 0.7.5 - 2026-08-31

### Fixed

- The `Enum` global answers to `Enums`, the class the API dump declares
  for it. The generated definitions type it as a flat table of every
  enum, because that table is what `Enum.KeyCode` indexes, and the two
  never met: a value annotated `Enums`, or a parameter asking for one,
  refused the global that is one. The global takes both now, so the
  indexing reads the table and the name answers the class

## 0.7.4 - 2026-08-31

### Added

- `[lsp] documentation` loads luau-lsp documentationFiles JSON databases
  for hover and completion. A database keeps its existing documentation
  symbols, even though larvae names a configured definition file by its
  path, and later files win when they document the same symbol

- `[requires] cross_realm = "allow"` emits a require that crosses the
  halves of the game instead of stopping the run, and
  `-- larvae: allow(cross_realm_require)` allows one line where the
  project keeps the check. Roblox replicates some containers the realm
  rules call server only, so a place built that way has requires larvae
  cannot know are correct

### Fixed

- Roblox enum names such as `Enum.KeyCode` resolve in type annotations
- Project-wide references, and navigation from a type name. Four
  things were missing together: the walk that finds the node under the
  cursor skipped every type annotation, so an imported `types.User`, a
  local alias, and the declaration itself answered nothing; type
  definition read the type of an expression, and an annotation is not
  one; references read the open document alone, so a module's export
  listed the uses of one file; and a definition file answered under
  the `@user/` name it loaded with, which no editor can open. The
  reference walk resolves each candidate and keeps what lands on the
  declaration the cursor named, so a name two modules share does not
  collect the other module's uses. The walk reads the whole project and
  not `[process] input`, because a use of a name lives wherever someone
  wrote it: a tool, a test, a script beside the build
- Go to definition reaches a field of another module, not only a
  function. The jump asked the type where it came from, and only a
  function type carries a definition of its own, so a constant or a
  table in another module answered nothing. The property of the table
  carries the position it was written at, which reaches the rest

## 0.7.3 - 2026-08-30

### Added

- `[fmt] sort_table_types` takes three more orders: `alphabetical`, and
  `size-ascending` and `size-descending`, which measure the whole field
  instead of the name. Two fields that measure the same are ordered by
  name, so the order is total and a second format finds the file as the
  first one left it
- `[fmt] sort_table_types.indexer` says where an indexer lands: `first`
  as the heading of the table, `last` as the footnote, or `sorted` in
  the place the order gives it. `indexer_first` still works and means
  what it meant
- `[fmt] sort_tables` orders the fields of a value table the same six
  ways, and stays off by default: a value table is data, and the sort
  moves each value with its key. A comment, a positional field, or a
  computed key that is no plain string keeps the table as written. The
  separators are the formatter's, so a sorted table cannot leave a `;`
  behind, and the magic trailing comma still reads the source's own
- `[fmt] function_style` picks the keyword a top-level function
  declaration takes: `local`, `const`, `global`, or `preserve`. A field
  of a table, a method, and an anonymous function take none and are
  left as written, and `const` applies only where nothing reassigns the
  name

### Fixed

- The Windows analyzer links the C runtime statically. With the dynamic
  runtime the library needed the Visual C++ redistributable, and a
  machine without it could not start the server at all, because the
  server links against the library rather than loading it late
- `[lsp] enabled` in `larvae.toml` reaches the editor. The extension
  read its own setting alone to decide whether to start the server, and
  a server that never starts reads no project config, so a project that
  turned the server on stayed dark. The two keys the client acts on,
  `enabled` and `claim_only`, now read the project first, which is the
  rule the server already followed for every other key
- A removed import leaves no gap. The line went and its newline stayed,
  so the file kept a blank line nobody wrote. The cursor moves past the
  dead line where the whole gap holds no comment, and where one does the
  comments still reach the next statement

## 0.7.2 - 2026-08-30

### Added

- `larvae analyze`: type diagnostics in the terminal, from the same
  analyzer the editor runs. The session mirrors the editor's, so the
  platform globals, the worms' lowering, the mounts, and the sourcemap
  tree all hold, and the diagnostics carry Luau's own error numbers.
  Errors fail the run. The analyzer lives in larvae-lsp, so the command
  finds that binary and hands the arguments over
- `[lsp] definitions` names extra type definition files, loaded after
  the platform globals, in the editor and under `larvae analyze` alike.
  `--definitions` adds to the list per run, and the extension mirrors
  the key. luau-lsp users know the idea as definitionFiles

## 0.7.1 - 2026-08-30

### Fixed

- The Windows ARM build carries the analyzer. The sealed library links
  from archives alone, LINK reads the machine off the first object it
  sees, and an all-archive cross link fell back to X64: the ARM64
  members were ignored and every export came out unresolved. The build
  script spells `/MACHINE` from the target now, so no platform ships
  without the type half
- Accepting a type hint proves the type first. The printer writes a
  required module's alias bare, so a double click could write a name
  the file never sees. A type spelled from primitives accepts at once;
  one that names an alias accepts only after the server lands it in a
  throwaway alias and the check comes back clean, so a global class
  accepts and a foreign alias does not
- The bundler strips `export` from the value spellings too: `export
  local` and `export function` are top-level syntax, the bundle wraps
  every module in a function, and the declaration without the keyword
  binds the same names
- Obfuscation leaves require specs as written. The engine decodes
  either spelling, so escaping a path hides nothing, and it broke
  every reader that checks specs textually, larvae's own checker
  included

## 0.7.0 - 2026-08-30

### Beyond luau-lsp

The server tracks luau-lsp feature for feature. The points below have no
counterpart there:

- Worms extend the server itself. A worm lowers the modules it claims,
  so `require("./data.json")` and a `.luaux` component carry real types.
  A worm transforms hover, completions, and diagnostics before the
  editor sees them, and lints plain Luau with its own rules. luau-lsp
  has no extension point inside the server.
- larvae's own lints run beside Luau's diagnostics, with levels up to
  deny, in the editor and in `larvae lint` alike.
- The dialect is served, not tolerated: `const` and `export local`
  parse, type, and hover in the author's own words.
- The oop worm holds the privacy line: an underscore or `@private`
  member neither completes nor passes the lint outside its class.
- `Player.Character` types as a real rig, R15 or R6, each part with its
  own class and no cast.
- The formatter lives in the same server: format follows `[fmt]`, and a
  completion that rewrites `t.Jump Force` into bracket access keeps the
  project's quote style.

### Added

- larvae-lsp, the language server binary with Luau's analysis frontend
  inside: real type diagnostics beside the lints, hover with inferred
  types, and completions, for the same files the server always served.
  The analyzer is the pinned Luau, vendored as a submodule and compiled
  behind a small C shim into one shared library that exports nine
  functions and hides both Luau copies from each other. Positions cross
  the shim as byte offsets, and the Roblox global types load at session
  start, vendored and refreshed nightly. `larvae lsp` stays the
  analyzer-free server it was
- The worm LSP hooks, three tiers in the manifest's `[lsp]` table.
  Resolve: a worm answers the requires it claims, and the analyzer reads
  the lowered Luau it returns, span-mapped back onto the original.
  Declarations: `.d.luau` text a worm injects at load. Respond: a worm
  transforms hover, completions, or diagnostics before the editor sees
  them. Native worms only, refused at load elsewhere, because the hooks
  sit on the analyzer's hot path
- Definitions files parse: `declare function`, `declare name: T`,
  `declare class`, the new solver's `declare extern type ... with ...
  end`, and attributes in both spellings, `@name` and
  `@[deprecated { ... }]`. The mode follows the file name, so `declare`
  in an ordinary `.luau` stays the syntax error Luau gives it. The whole
  837KB Roblox globalTypes.d.luau parses and prints back byte for byte
- Service auto-imports in completions, with the ranking bug of upstream's
  issue #1503 fixed structurally: position-valid keywords rank first, an
  exactly typed keyword preselects, auto-imports rank last, so `end`
  never loses to EncodingService. The inserted binding is
  `const X = game:GetService("X")`, placed above the file's first real
  statement, never inside the block the cursor sits in
- The `read` and `write` access modifiers hold in every table type
  position, the indexer included: `{ read [string]: number }` parses,
  formats, and keeps its space
- A require may name a claimed file outright: `require("./config.json")`
  resolves when a worm claims `.json`, the emitted require drops the
  extension onto the lowered module, and a bundle ships the data. The
  json-toml proof worm exercises both sides
- `serves_luau` in a worm's `[lsp]` table: the worm's hooks answer inside
  plain Luau files, so claim-only serving widens when it loads, with one
  editor notice naming the worm
- A nightly workflow that keeps the analyzer's inputs current: it moves
  the Luau submodule pin when upstream releases and refreshes the
  vendored Roblox global types, each as a pull request that CI validates
- The instance tree of the rojo sourcemap, as types. Each node becomes a
  declared type, and each file learns the name of its own. The server binds
  `script` per module, because `script` names a different instance in every
  file. So `script.Providers`, `script.Parent.Config`, and
  `game.ReplicatedStorage.Packages` resolve. A card reads the class of the
  instance, ex: `ModuleScript`, and not the name the generator made up. A
  file that a worm claims joins the tree beside the files rojo mapped: rojo
  writes the extensions it knows, and the build makes a module of a
  `.luaux` all the same. A folder that holds only claimed files joins
  too: rojo omits the whole folder, so the server synthesizes its node,
  and the children hang from it. `[lsp] sourcemap` names the file. A rewrite of it
  reloads the tree on the next request
- Completions inside `require("...")`. The list holds the aliases of
  `.luaurc` and of `larvae.toml`, then `@self/`, `@game/`, and the two
  relative marks. Under one of those it holds the directories and the files.
  Every file is offered, and not only Luau. A worm that claims `.json`
  gives the type of a data file. A file that no worm claims is offered too,
  because a require of it reports an unsupported path, which is a truer
  answer than an empty list. The list comes from the filesystem, so it
  answers while the type session is still loading
- Luau's own error number on a type diagnostic. The editor shows it as
  `Luau(1061)` and links it to the page that explains the checker, which is
  what luau-lsp shows
- `R15Character` and `R6Character`, larvae's own types beside the Roblox
  globals, in a definitions file the nightly refresh never touches. Each
  carries the parts of its rig, typed as their real classes, and stays a
  `Model` through the intersection. `[lsp] character_type` picks what
  `Player.Character` types to: `r15` by default, `r6`, or `not_set` for
  the union, so `local character: R15Character = player.Character` needs
  no cast and `player.Character.` offers the body parts
- `[lsp] analyzer`, the switch back to the classic server. Off serves what
  larvae always served: the lints, the format, the code actions, and the
  outline, on claimed and plain files alike, while the Luau parity goes
  quiet and the session is never built. On mid-session builds it then
- The comment an author wrote reaches the card. A `--- ` line or a
  `--[[ ]]` block above a declaration renders under the type, on the
  definition and on every use, across a require, with moonwave tags
  formatted the way luau-lsp formats them: `@param` and `@return` become
  their sections, and a plain `-- ` line stays code
- `Instance.new` answers with the class the string names, so
  `Instance.new("Part")` is a `Part` and its methods read as the class a
  reader is looking at
- A type position offers the project's own types. The list capped at 256
  entries out of an unordered map, so the generated instance-tree names
  crowded a file's own aliases out at random. The generated names are
  hidden, the cap holds a full scope, and a type entry carries the kind
  luau-lsp gives one
- The Roblox reference under the type on a hover card, and on a completion.
  The trimmed database ships with the binary: 3.7MB of JSON, 438KB deflated,
  inflated on the thread that builds the session. Luau names the page for a
  member and for a global, and a local or a type reference answers with the
  page of its class, which is what the reader is looking at either way
- A completion carries what luau-lsp sends: the type as the detail, the
  argument names as the label detail, the reference or the comment above the
  declaration as the documentation, the parentheses of a call as the
  insertion, and the deprecated tag. The order is luau-lsp's too, and it is
  the answer Luau gives rather than a guess from the kind: an entry that fits
  the type the position expects comes first, which is what puts the props of
  a component above the whole global scope
- The root of the project reaches a worm at init, so a worm resolves a file
  of its own against it. A host older than the field sends nothing and the
  working directory stays the fallback
- `larvae/bytecode` and `larvae/compilerRemarks`, the two compiled views a
  reader asks for. The editor sends a document and an optimization level,
  `[lsp.bytecode]` supplies the rest, and a claimed file compiles through its
  worm's lowering, because that is the Luau the place receives. luau-lsp
  serves the same two views under its own prefix


- `[lsp]`, the table for the editor server. `enabled = false` answers every
  request with nothing, so another server owns the files. `claim_only =
  true` serves only the files that worms claim, empty diagnostics and
  declined requests for the rest, so stock luau-lsp can own the plain Luau
  of a project while larvae serves the claimed files beside it

- A double click accepts an inlay hint into the file. The edit writes
  the whole type, so a label the display truncated still accepts what
  it stands for. A hint whose text is display notation and not syntax,
  ex: `@metatable` or a type Luau's printer cut off, stays display
  only, and a parameter name never inserts, because a call site has no
  written form for one
- Hovering an inlay hint answers with the same card as hovering the
  name it annotates, through `inlayHint/resolve`. A hint on a linted
  require showed the lint alone, because an editor never sends a hover
  for the hint's own pixels
- A types-only module says what it is for. A require of a module that
  returns `{ }` and carries `export type` lines hinted and hovered as
  the empty table, which is true and says nothing. The card and the
  hint read `{ type PlayerData, type Slot }` instead. The view is
  display speech, not type syntax, so a double click never writes it

### Changed

- The syntax layer lives in its own crate, `eclipse_luau`: the lexer, the
  parser, the AST, the require scanner, the lossless printer, and the dense
  re-emitter, with byte ranges as the identity of every node and the
  round-trip guarantee as a fuzz target. larvae re-exports it as
  `larvae::syntax`, so nothing changes for larvae itself; any other tool
  can now parse Luau with the same parser. Criterion benches ship with the
  crate, a full_moon comparison behind a feature
- `[fmt] unused_imports`, which decides what the formatter does with a required
  module that nothing uses. `ignore` leaves it as written, `underscore` renames
  it to `_Name`, and `remove` deletes the declaration. `ignore` is the default,
  and the default matters more here than for the other options: `require` runs
  a module the first time a file asks for it, so a module can do its work by
  being required at all, and deleting the line stops that work while the file
  still compiles. Larvae cannot see inside that file, so the project decides.
  A name that only a type reads counts as used, which is the case the option
  turns on for: the parser consumes type syntax for its extent and does not
  interpret it, so a walk of the expressions never sees the name again, and the
  resolver the linter builds recovers the reference. A name that already opens
  with `_` is left alone, a declaration binding more than one name is left
  alone, and a statement inside a `fmt off` region is never removed. A removal
  keeps every comment around it: a formatter may rewrite code and may not
  delete prose
- `[minify] obfuscate`, off by default. On, every emitted file prints through
  the dense emitter whatever `generator` says, with three changes: every type
  is gone, every local takes a name from `_0x0` upward, and every string
  literal becomes the `\xNN` form of its own bytes. The file lands on one line,
  because `obfuscate` sets the column span to unlimited. A project that wrote
  its own `[minify] column_span` keeps that number, the same way an explicit
  key beats an implied one everywhere else. Roblox has no `loadstring`, so a
  file cannot ship as a decoded blob: it has to be Luau the compiler reads, and
  hiding the names, the strings, and the shape is what obfuscation can be. A
  backtick string stays whole, because splitting its static text from its holes
  means implementing interpolation twice and a wrong split is a broken build;
  the names inside a hole are pinned instead of renamed. With `[bundle]` set,
  the bundle output gets the same treatment, because a bundle prints through
  the generator like any file
- The last two inlay hint kinds: `function_return_types` draws what an
  unannotated function returns, after its parameter list, and
  `parameter_names` draws the name of each parameter before the argument
  that fills it, on literal arguments or on all of them. The skips are
  luau-lsp's: an argument spelled like its parameter stays bare, compared
  case folded, and a platform call a handler refines names nothing, so no
  `className:` in front of every `game:GetService` line. The variables of
  a `for ... in` hint their types before the `in` keyword
- Module auto-imports. Every module of the workspace offers itself by
  its stem, and accepting one writes the require above the first real
  statement, in the quote `[fmt] quote_style` keeps and the style
  `[lsp.completion.imports] require_style` picks: `auto` writes an alias
  where one covers the module, `always_absolute` writes the `@game`
  path, and `nearest_absolute` takes the shortest stable anchor. The
  service auto-import reads the same quote setting
- A renamed file offers to carry its requires along. The server matches
  every spec that named the old place, rewrites each in the form it was
  written in, and asks with one dialog before anything applies
- `.config.luau` speaks its own shape: the keys typecheck, complete,
  and error at the file's own bytes, with every lint name spelled out
- The data worm narrows a matched string to its literal:
  `[worms.data.config] force_string_singletons` maps a file glob to
  value globs, and `"kind"` lowers as `"kind" :: "kind"`
- A doc fence hides a line that starts with `$`, on hover and on
  completion docs alike; prose keeps its dollars
- `unused_import`, split from `unused_variable` and warn by default: a
  require bound to a name nothing reads is a leftover, and the fix is
  always to delete the line. A method named require stays a variable
- `[lsp.inlay_hints] update_delay`: the hints hold still while the
  author types, follow their lines as edits move them, and recompute
  once after the pause
- A key a dot cannot reach rewrites itself into brackets on accept:
  `Jump Force` after `t.` writes `t["Jump Force"]`, in the quote
  `[fmt] quote_style` keeps, and the dot the author typed goes with it
- Deprecated uses draw a strikethrough in the file, from Luau's own
  linter, as hints that raise no squiggle. The completion list already
  struck them through; `[lsp.completion] hide_deprecated` drops them
  from the list whole, and stays off by default. Where the platform
  marks a use, larvae's own `deprecated` finding stands down, so a
  name the project deprecated itself still gets larvae's message
- `ScopedInstanceIdentity` carries `ResolveInstance`, patched over the
  dump's stub until upstream ships the real shape
- `[bundle] module_ids = "opaque"` numbers the modules of a bundle instead
  of naming their paths. A path id reads well in an error and also ships
  the whole project layout: every file, package, and vendor directory in a
  table key anyone who reads the bundle can read, and obfuscation alone
  hex-escapes those bytes without changing them. Numbers, not hashes, on
  purpose: a hash of a path is deterministic, so a list of likely paths
  recovers the names, and a number carries nothing to recover. The
  numbering follows the sorted paths, so a rebundle emits the same bytes
- `export local x = 3` parses, types, and hovers. The syntax is Luau's,
  behind its LuauExportValueSyntax flag, and larvae turns the flag on
  because the dialect owns the spelling; `[lsp.fflags]` with
  `LuauExportValueSyntax = "false"` restores stock parsing, and the
  keyword then hovers as the error it is. A hover on `export`, `local`,
  or `const` answers with the declaration itself, and the card speaks
  the author's words: `export local test: number`, `const t: number`,
  never a bare `local` for a binding the author spelled otherwise
- `[lsp.<worm>]` holds the editor settings of one worm: `[lsp.oop]`
  `hide_private_completions = false` reaches the oop worm as config.
  Each key checks against the `[options]` the worm declares, so a typo,
  a wrong type, or a name that is no worm warns instead of doing
  nothing. An unknown `[lsp]` key warns the same way now, where it used
  to refuse the whole config at parse
- Instance-form requires resolve: `require(script.Parent.Widget)`,
  `require(game.ReplicatedStorage.App)`, and the call spellings
  `GetService`, `FindFirstChild`, and `WaitForChild`. The chain maps
  through the same mounts that answer `@game`, one hop at a time, the
  way Luau traces it. A worm-claimed target lowers as it does for a
  string require, so a `.luaux` component or a data file types through
  the chain too
- Worms reach the files they do not claim. A worm that sets `lints_luau`
  in its manifest runs its Lint op over every plain Luau file, after the
  builtin lints and on the same levels and suppressions. A claiming worm
  that sets `shared` under `[frontend]` consents to the same for its own
  files: the foreign worm reads the byte-true Luau shadow, so a finding
  lands on the author's line. The oop worm is the first reader: its
  class conventions hold in `.luau` files and in shared `.luaux` files
  alike, while resolve and respond hooks already served both
- The server runs the sourcemap generator itself, so the map follows
  every file the project adds or moves, and `script.Parent.` completes
  on a file made a minute ago. `[lsp] sourcemap_command` names the
  generator for any sync tool, run through the shell in the project
  root; empty infers rojo from `[lsp] rojo_project_file` and runs
  `rojo sourcemap --watch`. `[lsp] sourcemap_autogenerate` turns the
  whole thing off. A generator that does not start is said once and
  costs the autogeneration alone. The generator and everything it
  started die with the server, on Unix even when the editor kills it

### Changed

- Every child of the character rigs is optional: `["Left Arm"]` is
  `Part?` now, the Humanoid and its Animator too. A part can be
  destroyed, detached, or not yet streamed in, and a guaranteed type
  hid the nil the runtime can hand back

### Fixed

- An edit crosses files without a restart. `./test2` resolved to
  `Data/./test2.luau`, the frontend keys modules by the path string, and
  the same file lived as two modules: the open buffer under the clean
  name, the disk text under the dotted one. A require read the disk
  twin, so removing the `return` of a module never reached the file
  requiring it until a restart. Resolved paths now fold `.` and `..`
  away. The dotted twins also duplicated whole package graphs, each
  checked on its own, which is where the slow session and the long
  `Loading...` spells came from
- The `Loading...` hover card stops coloring its dots; the fence is
  plain text now, because luau read `...` as its vararg token
- A member hover carries its name: `t.hp` answers `hp: number` where it
  answered `number` alone, which no theme colored. The name goes on for
  member accesses and keys; every card that already starts with its own
  keyword keeps its words
- The editor reports a cross-realm require, the same finding `larvae
  check` reports and with the same words: the resolver validation runs
  over every open file, on the string forms and the instance forms
  alike. The require compiles and resolves, so the analyzer was content
  and only the build ever said anything

- A read from inside a type is a token index, as every other read is. It was
  the byte offset of the token, so `Binding::reads` held two units at once the
  moment a type named a binding. `shadowing` compares a read against the token
  span of the declaration that hides it, and a byte offset that lands in that
  range silences a real finding. Nine lines reproduce it
- `@game` resolves in the editor from a file the DataModel map does not cover.
  The spec is absolute and reads nothing from the file that writes it, but it
  went through the `.luaurc` alias branch, where it resolved only if the
  project defined a name called `game`. It answers before the alias lookup
  now, through the mount table the pipeline already builds, so the editor and
  `larvae process` read one require the same way. A `.luaurc` that defines
  `game` still wins. This is the larvae half of luau-lsp#1598
- The settings of the editor reach the server. `[lsp]` was copied over the
  whole table, so a project with any `larvae.toml` threw away every setting
  the user made in the editor. That included the names the file says nothing
  about. The project wins the names it spells, and only those, which is the
  rule the config always stated
- `[lsp.fflags] enable_new_solver` builds the session under the new solver.
  The flags went in after the session existed. A Luau flag decides which
  solver the globals are registered under, so the setting did nothing. The
  build starts once the project is read, and the flags go in first. A file
  that writes `f<<T>>()` or `setmetatable<A, B>` needs this: the old solver
  reports `Cannot instantiate type parameters on something without type
  parameters` for code that is correct
- A require of a file that no worm claims resolves to nothing, and Luau
  reports an unsupported path. It resolved to the file, and the analyzer
  read the JSON as Luau. The first brace became a syntax error, inside a
  file the author cannot see
- The type session reaches the editor by itself. The server took it on the
  next request that needed one, so a file opened before the load finished
  kept its parser-only findings until the author typed. The landing is an
  event of the loop now. The session arrives, the config applies to it, and
  every open document is checked again
- The flags, the DataModel map, and the worm hooks reach the session that a
  thread built. They applied while the analyzer did not exist, so they
  applied to nothing. `@game` resolved to nothing, and a worm that claims a
  file answered no require in the editor
- A worm pool that fails to build says why. One worm whose name does not
  match its manifest took every worm with it, in silence. The files of a
  working worm went back to being read as Luau, and its requires reported as
  unknown. The message names the reason. It repeats only when the reason
  changes
- The analyzer builds on Windows and macOS. Four of the six release legs
  could not have produced a working library: the seal step was GNU only.
  The archiver and the driver now come from the build's own toolchain, and
  each platform names and links the library its own way
- A hover on an unchanged file reads the cached check. Every hover marked
  the module dirty through its open, so the next check rebuilt it from
  nothing, which read as the types reloading for no reason
- A completion never reads a type whose arena is gone. Luau's autocomplete
  frees its local arena before it returns, so an entry's type can point into
  freed memory; the documentation path followed one into a crash. The
  session now gathers the arenas it still owns after the check, and reads
  nothing outside them. Size assertions on both sides of the FFI structs
  keep a one-sided edit from shipping again
- The hover card says `Loading...` while the type session is still being
  built. It said `...`, which says nothing to the person who reads it
- A message the server cannot read as a request is skipped inside the read.
  `None` used to mean both "the stream ended" and "this body did not parse",
  and the loop read the second as the first: one response from the editor
  was a clean shutdown. The server sends `workspace/inlayHint/refresh` now,
  so responses arrive
- The editor redraws its inlay hints when the session lands and when a
  setting changes. The hints on screen were drawn before there were types,
  and nothing else makes the editor ask again
- `variable_types` and `parameter_types` gate their own hints. Both kinds
  render as a type hint of the protocol, so only the collector can tell a
  local's hint from a parameter's, and the settings now reach it
- The sourcemap reloads when its file or its `[lsp] sourcemap` path changes,
  and not on every configuration message. Each re-read declared the whole
  tree into the global scope again
- A worm reads a file of its own against the root of the project. The luaux
  worm reads `luaux.toml` that way, and the working directory answered for
  `larvae process` and not for the editor: the file went missing, every
  markup file compiled with the default factory name, and a require of one
  gave an error type while the same require built correctly
- A comment holds prose, and prose has no type. Every word of a doc comment
  hovered as the type of whatever stood under it, because the lookup answers
  for the innermost node that holds the position and a comment inside a table
  is held by that table
- A method reads as the type of the receiver the author wrote.
  `game:GetService` read as `ServiceProvider:GetService` and
  `player:IsDescendantOf` read as `Instance:IsDescendantOf`, which is where
  each is declared and not what the reader is looking at
- A signature prints a named type by its name. `self: World` became the whole
  of `World` inlined, and `ClientImp & { ... }` lost the half a reader can
  recognise. A card for a value still expands what it holds
- A call names the signature it was declared with. `table.create<V>(count,
  value)` read as `table.create(count: number, value: nil)`, because the
  recorded type of the expression is the one the call solved
- An alias carries its parameters: `type Entity<T = nil>`, and not
  `type Entity`, which says nothing about where `T` comes from
- A local on the left of an assignment answers from the scope. `SendSize =
  Save.Size` records no type for the name it writes to, so the card showed
  the function the line stands in
- A keyword inside a function answers with that function, as luau-lsp does,
  and the card goes out unnamed. It carried the name of whatever the cursor
  was on, which read as a function that does not exist
- The Luau flags go back to their startup values when a test that changed
  them ends. They are global to the process, so the flag tests decided what
  every later test in the same binary inferred, and every hover test failed
  when the whole suite ran
- `[fmt.sort_table_types]`, which orders the properties of a table type by the
  length of their names. `order` takes `none`, `ascending` or `descending`, and
  `none` is the default: a formatter must not reorder code nobody asked it to
  reorder. Two names of one length sort alphabetically, so the output does not
  depend on the order of the input. `indexer_first` puts an indexer such as
  `[number]: any` above the named properties, because it states the shape of
  every key instead of one key. This reaches a type position only, so a value
  table keeps the order the author wrote. Two limits: a table type that holds a
  comment stays as written, because a token replay has no position to put a
  comment back, and the sort reads the fields the table type layout finds, so
  `table_types.enabled = false` leaves every order alone
- `[fmt.type_operators] expand`, which lays out a union and an intersection in
  every position a type takes. One option covers `|` and `&`, because they
  share one shape. `auto` is the default and it is the layout larvae always
  had, byte for byte: the operator breaks no line, so a long chain runs past
  `column_width`. `always` puts every member on a line of its own, with the
  operator leading the line, which is the position a long binary chain already
  gives its operator. `never` holds one line whatever `table_types.width` says
  about a table inside the chain, and opens one member per line only where the
  line passes `column_width`. So the order is `column_width` first, then
  `type_operators`, then `table_types.width`
- The new solver builds one globals table, not two. The second table
  serves the old solver's autocomplete, and the new solver never reads
  it, so the session loaded the platform types twice for nothing. The
  first hover card lands at 2.4 seconds where it landed at 19
- A require offer narrows as the author types. The offers carried no
  edit range, so the editor guessed a word range with no `@` in it,
  filtered `@shared/` against `sh`, and closed the list. Every offer
  now carries the edit and the filter text that match what was typed
- Editing a claimed data file updates the types that read it. The
  analyzer kept the module it built from the old text, so a hover on a
  require of the file answered the old shape until a restart. Publishing
  a claimed file invalidates its module, and the next check rebuilds
  every dependent
- A flag override before the first snapshot poisoned the reset: the
  snapshot was taken after the change and the reset put the change back.
  In the tests, the first flag test decided the solver for every later
  session in the binary. Every write path snapshots first now
- A hint no longer repeats the binding's own name. Luau names a table
  type after the binding that holds it, so `const EmptyStats = { ... }`
  drew a hint of `: EmptyStats`, which says nothing
- `game:GetService("X")` answers with the sourcemap's service, children
  and all, so `ReplicatedStorage.Shared` resolves the way
  `game.ReplicatedStorage.Shared` always did
- Every binding of a multi-return call hints its type, and a hint
  survives the old solver's empty scopes by reading the value expression
- A claimed hover reaches the worm with the analyzer's answer, so a
  component hover carries the signature the element calls and follows it
  when it changes
- A data file offers no Luau completions: a ctrl-space in a `.toml`
  answered with the Luau global scope, which is noise in a file that
  holds no code
- With the analyzer landed, a syntax error is reported once, in Luau's
  own words
- A repeated member of a union or intersection says its name once: two
  different tree nodes both spelling `Folder` drew `Folder | Folder`
- A zero-argument function reads `()` across modules. The exported pack
  collapses to a bare hidden variadic, and the vendored stringifier
  printed it as `(...any)`; the build applies a display patch until
  upstream hides the bare form the way it hides the wrapped one
- A worm-lowered module checks strict, a claimed edit reflects before
  the save, and a refused lowering says why at the require that names it
- A signature popup ends where the parameters end, and a component's
  attribute list holds props alone

## 0.6.0 - 2026-08-21

### Added

- `[fmt] enabled` and `[lint] enabled`. `false` turns that half of larvae off:
  the formatter writes no file and reports no `--check` failure, the linter
  reports nothing and exits zero, and the editor gets a formatter with no edits
  and a file with no diagnostics. This is for a project that wants larvae for
  one job and keeps another tool for the other
- `[fmt] recommended`, the same three states as `[lint] recommended`. `false`
  starts from a base that changes as little as possible: `magic_trailing_comma`,
  `space_inside_braces` and `trailing_comma` go off, and the project turns back
  on what it wants. Those three are the options where larvae has an opinion and
  the opinion changes a file. Every other default is either stylua's or a
  setting that does nothing until a project asks for it, so `recommended` does
  not move it
- Eleven lints, mostly Luau equivalents of Biome rules. Four fill real gaps
  that Luau's own linter reports on none of, checked against luau-lsp:
  `length_as_condition` (deny), `builtin_shadowed`, `ignored_pcall_result` and
  `constant_condition`. Two report a conditional used as a value, both allow:
  `and_or_conditional` and `if_expression_assignment`. Three report the shape
  of a branch, all allow: `else_after_return`, `collapsible_if` and
  `negated_condition`. And two more: `implicit_any_parameter` (allow, the
  sibling of `implicit_any_local` on the other side of the call) and
  `restricted_globals` (silent until `[lint.options.restricted_globals]` names
  one, so it costs nothing until a project asks)
- `[lint.groups]`, a level for a whole kind of lint at once: `correctness`,
  `suspicious`, `style`, `complexity`, `performance` and `roblox`. It sits
  between `recommended` and `[lint.rules]`, so a name the project wrote always
  wins and a group covers every lint it did not name. Two rules are worth
  knowing. The table is separate from `[lint.rules]` and not nested inside it,
  because a table there already means the lints of a worm of that name and
  nothing reserves a worm name. And a group does not wake a lint that is
  `allow` on purpose: `style = "info"` asks the style lints a project already
  sees to say less, and `prefer_const` stays off until the project names it
- `info`, a level below `warn`. It reports and it leaves the exit code alone,
  exactly as `warn` does. The two differ in what they ask of the reader, and an
  editor draws an info as a hint and a warning as a squiggle. The summary line
  counts them separately, and only when a project uses the level
- `larvae lint --explain <name>` prints the group of a lint, and the list it
  prints when a name misses is grouped rather than one alphabetical run of 52
- `larvae init` writes `recommended = true` into `[fmt]` and `[lint]`. The
  value is the default already; the key is the point. A key that is there can
  be turned off, and a key that is absent has to be found in the docs first
- `[lint] recommended`, as Biome has it. Absent and `true` both mean the
  default levels apply, which is what larvae always did. `false` starts every
  lint at `allow`, so a project gets the lints it names and no others. A level
  the project wrote always wins, in either state
- `implicit_any_local`, a lint for `local test` with no value and no type. What
  the name holds is then decided by whatever assigns it first, and in a file
  with no `--!strict` directive Luau accepts any later assignment of any type.
  A local that nothing ever assigns is left to `uninitialized_local`, which
  says the more urgent thing about the same line
- `[fmt] prefer_const`, which turns a `local` that nothing reassigns into a
  `const`. The same rule the `prefer_const` lint reports, with the formatter
  making the edit, and it carries the same `mutated_tables_stay_local` option
  under the same name. Off by default: it rewrites a keyword, which is a
  bigger step than moving spaces

### Changed

- `misleading_and_or` reaches a middle that syntax proves is a boolean. It
  fired only on a literal `false` or `nil`, but `ready and (count == 0) or
  "pending"` gives "pending" when the count is not zero, which is exactly when
  the author wanted `false`. A comparison yields a boolean because it is a
  comparison, so the wider net needs no types. The two cases carry different
  messages: the literal is wrong for every input and the boolean is wrong for
  half of them
- Five lints deny by default now: `duplicate_keys`, `duplicate_local`,
  `format_string`, `zero_step_loop` and `length_as_condition`. They join
  `undefined_variable`. The bar
  is two things at once: no reading of the code makes the finding wrong,
  because each check reads a literal and never a runtime value, and the code as
  written cannot be what the author meant. `{ a = 1, a = 2 }` discards the
  first entry, `local a, a` kills the first binding where it stands,
  `string.format("%y", 1)` raises, and a zero step hangs. Over a 364 file
  corpus the four report nothing, which is the point: nobody writes these
  shapes on purpose
- `implicit_return` and `multiple_statements` are `allow` now. `implicit_return`
  said so in its own comment while its level said `warn`; the comment had the
  better argument, because a lookup that falls off the end is idiomatic Luau.
  `multiple_statements` is ground `larvae fmt` already owns: with
  `collapse_simple_statement` at `never` a format run removes every finding the
  lint can make
- `implicit_any_local` warns instead of denying. The fix never changes
  behaviour, which argued for a deny. The shape argues against one: `local
  found` above the loop that fills it in runs correctly, Luau's own linter says
  nothing about it, and a deny failed the build of every project on the day it
  adopted larvae

### Fixed

- A require that names a file a worm claims resolves to the Luau the worm
  writes. The DataModel read the source name, so `App.luaux` became an
  instance called `App.luaux`, and `require("../Interface/App")` came out as
  `require("../Interface/App.luaux")`, which points at nothing. The `path`
  target was already right, because it strips whatever extension it finds; the
  two Roblox targets were not. A directory whose init file is claimed resolves
  as well: `Pkg/init.luaux` is written as `Pkg/init.luau`, so `Pkg` is a module
  and the resolver no longer warns about a require that is correct
- A claimed script keeps its realm suffix. `boot.client.luaux` read as a plain
  module, so the checks that apply to a LocalScript never ran on it, and
  `boot.server.luaux` had the same gap. UI runs on the server as well as the
  client, so both halves matter
- A name used only by a type is no longer reported as unused. The token walk
  that recovers a reference from inside a type skipped a name after a colon,
  which is right for `obj:method` and wrong inside a type: `type T = { e:
  jecs.Entity }` puts the field name before the colon and the type after it

## 0.5.0 - 2026-08-19

### Added

- A worm can offer code actions and supply Luau type definitions, through the
  two LSP paths, in all three forms. A native worm answers the `actions` and
  `definitions` ops, a wasm worm exports `larvae_actions` and
  `larvae_definitions`, and a Luau worm puts `actions` and `definitions` on
  its `frontend` table. All three are optional: a worm with nothing to offer
  answers with nothing and not an error, because the editor asks on a
  keystroke
- `larvae worm add <spec>` writes a worm into `[worms]`. It takes a short name
  larvae knows, `luaux`, or `owner/repo`, either with `@version`. `--cargo`
  takes a crate instead. It writes the config and stops, because an edit is
  instant and offline while a download is neither
- `larvae worm install`, `i` for short, puts every worm the config names on
  disk, with a progress bar. Under a pipe it prints a line per worm instead
- `larvae worm remove <name>`, `rm` for short, takes a worm out of the config
  and off the disk, and drops `[worms]` when its last worm goes
- A code action for `unused_variable` and `unused_function`: prefix the name
  with an underscore, which is the fix those lints already print in their
  help. It renames the declaration and every write of the name, because
  prefixing the declaration alone would leave an assignment pointing at a name
  nothing declares, which is a global and a worse bug than the warning
- `prefer_const`, a lint for a local that nothing reassigns. Off by default,
  because `const` is larvae's own reading of Luau and a codebase of ordinary
  `local` would report on nearly every line the first time it ran. It leaves
  alone what cannot take `const`: a declaration with no initialiser, a
  `local function`, a `for` variable, and a multi name declaration where one
  of the names is reassigned
- `[lint.options.prefer_const] mutated_tables_stay_local` keeps `local` on a
  binding the file mutates through a field, `t.x = 1` or `table.insert(t, 1)`.
  Off by default, because `const` is correct there: Luau enforces it against
  reassignment of the name and says nothing about the value
- Two LSP paths for worms, wired and empty. `textDocument/codeAction` is
  advertised and answers with a list, and `larvae/definitions` is a request of
  larvae's own for the type definitions a worm supplies. Neither carries
  anything yet: `crates/larvae/src/lsp/extend.rs` is the seam, so the work
  that fills them touches one file. The capability is advertised now because
  an editor decides at initialize whether to ever ask
- `[fmt] function_call` and `[fmt] function_declaration` lay out the two lists
  between parentheses. `expand` takes `when-needed`, which is the layout larvae
  always had, `always`, and `never`. `indent` gives the levels an opened item
  takes. Both default to the layout larvae already wrote
- `[fmt] function_call.style = "hug-last"` keeps the arguments on the line of
  the call and opens the last one, where that argument is a table, a function,
  or a string carrying its own newlines. The arguments before it do not break,
  so a long list of them runs past `column_width`, which is why `one-per-line`
  stays the default

### Changed

- A worm is a table, never a string. `xml = "owner/repo@0.1.0"` is refused
  with a message that prints the table to write instead. The version is a key
  of its own now, so `install` can tell a pin from a range without splitting a
  string and a reader can see what is pinned
- A version says whether the project moves. `"^"` takes the newest release on
  every install and is what `add` writes, `"^0.1.0"` follows what semver calls
  compatible, and `"0.1.0"` holds that release
- `larvae worm update` is gone. The version now says whether a project wants
  to move, and a command that bumps a pin the user wrote undoes the pin
- Installing is a step and not a side effect. A command no longer downloads a
  missing worm; it names it once and carries on. The editor still skips in
  silence, because it answers a keystroke
- An unused `local function` reports `unused_function` and not
  `unused_variable`. The Luau compiler separates the two, and it separates
  them by the declaring form and not the value, so `local f = function() end`
  is still `unused_variable`. Each name carries its own level, so a project
  that keeps unused helpers while still wanting unused locals reported can now
  say so. Both read `[lint.options.unused_variable]`. On the same corpus, 44
  of 206 reports moved to the new name and none were lost

### Fixed

- A file a worm claims can be required. `require("@app/widget")` where the
  project holds `widget.luaux` found no module and warned about a require that
  is correct, because the resolver looked only for `.luau` and `.lua`. A
  claimed file is a module: the pipeline turns it into Luau in the output, so
  the require resolves at runtime. A claimed file beside a `.luau` of the same
  name is ambiguous, as two `.luau` files would be
- No command downloads a worm any more. `larvae process` still did, and it
  passed the version through unresolved, so a worm pinned at `^` asked GitHub
  for a release tagged `v^`
- `larvae worm remove` no longer fails with "Directory not empty". NTFS
  through FUSE reports a directory as not empty right after its last file was
  unlinked, so the tree comes apart from the bottom and the last step retries
- A global `function f() end` that no line reads now reports
  `unused_function`. It is dead code, a global belongs to the script that runs
  it, and both selene and the Luau compiler report it. Larvae reported nothing
  once the `unscoped_variables` fix below stopped it reporting the wrong thing
- `function f() end` no longer reports `unscoped_variables`. The statement
  creates a global the same way `f = 1` does, and the two do not read the
  same way: neither selene nor the Luau compiler reports the declaration, and
  a Roblox script defines its callbacks with it. On a 400 file corpus this
  removed 10 of 29 reports. The name a global function declares is still
  defined for the file, so `undefined_variable` stays quiet where it is called
- A native worm loads on Windows. A worm is built one time and released for
  every platform, so its manifest names `luaux-worm` while the Windows zip
  holds `luaux-worm.exe`. Larvae knew that rule in the place that runs a worm
  and not in the two places that read its bytes, so loading stopped at
  "The system cannot find the file specified" before the rule ever ran. Every
  reader now resolves the entry through one function
- A worm installed from cargo keeps the `.exe` that cargo built. It was copied
  to the bare name the manifest gives, which left Windows a file it does not
  run, and it disagreed with the release channel, which ships the extension.
  The write side and the read side now spell the file the same way

## 0.4.0 - 2026-08-18

### Fixed

- A bundle follows the requires of a vendored package. The pipeline
  resolves the input tree only, so the graph ended at the door of a module
  outside the roots: `require("./lib")` inside a package shipped raw and
  failed inside the bundle. The bundler now walks those modules until the
  graph closes, with the same resolver, the same `.luaurc` walk, and the
  same `@self`, so a package that requires a package follows too
- `@self` requires enter the require graph. They passed through as
  natively valid and never recorded an edge, so `check` could not follow a
  cycle through one and a bundle dropped what one named
- A resolution failure inside a vendored module warns instead of stopping
  the bundle, because a package can require another runtime, ex:
  `task or require("@lune/task")`; the require ships as written
- `export type` loses its `export` inside a bundle. The keyword is legal
  at the top level of a module only, and the bundle wraps every module in
  a function, so the bundle of a module that re-exported types did not
  parse

### Added

- The syntax of three merged Luau RFCs. Classes: `[export] [open] class
  Name [extends Base]` with `[public]` fields, methods, the RFC's
  metamethod whitelist plus `__init` from the constructors RFC, and a
  syntax error for any other `__` name. Export by value: `export local`,
  `export const`, `export function`, composing with attributes. Integer
  literals: the `i` suffix on decimal, hex, and binary numbers. `class`,
  `open`, and `export` stay contextual, so code that uses them as names
  parses as before. The formatter lays a class out one member per line, a
  field annotation takes the table type layout, and a class holding a
  comment between members prints as written
- A read-only pass reads a file that is not UTF-8. Luau accepts any byte
  inside a string, and larvae reads files as UTF-8, so `larvae check` and
  `larvae lint` now analyze a stand-in: every invalid byte becomes one
  stand-in byte, so every offset holds. A pass that writes output still
  refuses the file, because a splice of the stand-in would write it
- The upstream Luau conformance corpus as a parser regression net: all 55
  files of `luau-lang/luau tests/conformance` are vendored, and every one
  must lex, parse, tile its token stream, and print back byte for byte

- `larvae worm update`, which bumps every worm in larvae.toml to its latest
  release: the newest GitHub release for a `repo@version` source, the newest
  stable crate for a `cargo` source, and nothing for a path worm. `--check`
  reports what waits and fails without writing, for CI. The edit is textual
  and keeps every other byte, comments included, and a worm that an
  `extends` base declares is reported rather than edited behind its owner
- `[fmt] table_types`, which lays out table types in every position a type
  takes: an alias, an annotation on a binding or a parameter, a return
  type, and a `::` assertion. A table wider than `width` (60 by default)
  opens with one field per line and a trailing separator; the measure is
  one table alone, so a short table nested in a long one keeps its line.
  `separator` selects `comma` or `semicolon`, in both layouts. On by
  default; `enabled = false` keeps the one-line replay byte for byte. The
  new solver's forms hold: `read` and `write` modifiers, indexers,
  intersections, and generic arguments split no field. A comment inside a
  table type keeps the author's text, and `type function` bodies are never
  touched
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
