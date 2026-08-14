--!strict
--[[
	Type definitions for worms written in Luau.

	Point luau-lsp at this file. Then your worm typechecks and autocompletes:

		luau-lsp analyze --definitions=worm.d.luau init.luau

	or in .vscode/settings.json:

		"luau-lsp.types.definitionFiles": ["worm.d.luau"]

	Write this file out at any time with `larvae worm types`.
]]

--[=[
	The function takes source text and returns source text.

	`source` is the contents of the file. `config` is the content under
	`[config.<your name>]` in larvae.toml, serialized again as TOML, or an
	empty string when the user wrote none. Parse it yourself. larvae does not
	read it.

	Return the transformed source. To report a problem instead, call
	`error("...")` with a message for a human. larvae attributes the error to
	your worm and does not crash the build.

	The line count is important. larvae guarantees that line N of dist is line
	N of src. This holds end to end only if you keep the line count too.
]=]
export type Compile = (source: string, config: string) -> string

--[=[
	A byte range in the source, as a half open `{start, stop}` pair.

	The offsets are 0-based and count bytes, in the same way as every other
	span of larvae.
]=]
export type Span = { number }

--[=[
	One piece of layout, for a format reply.

	larvae renders the document with the `[fmt]` settings of the project.
	Thus your output obeys `column_width` without any width code in the worm.
	A unit piece is a plain string. Every other piece is a table with one key.

	- `"nil"` gives no output.
	- `"line"` gives a space when flat and a newline when broken.
	- `"soft"` gives nothing when flat and a newline when broken.
	- `"hard"` gives a newline in both modes.
	- `"blank"` keeps a blank line that the author wrote.
	- `{ src = {s, e} }` copies an exact slice of the source.
	- `{ lit = "<" }` inserts text that the worm generated.
	- `{ group = doc }` is flat when it fits and broken when it does not fit.
	- `{ indent = doc }` adds one level of indentation inside.
	- `{ if_break = {flat, broken} }` selects one part by the mode of the group.
	- `{ concat = {...} }` joins the parts in order.
	- `{ host = { start = s, ["end"] = e, parse = "block" } }` marks a span of
	  ordinary Luau, and larvae formats the span itself. `parse` is `"block"`
	  for statements and `"expr"` for one expression. `"block"` is the
	  default.
]=]
export type Doc =
	"nil"
	| "line"
	| "soft"
	| "hard"
	| "blank"
	| { src: Span }
	| { lit: string }
	| { group: Doc }
	| { indent: Doc }
	| { if_break: { Doc } }
	| { concat: { Doc } }
	| { host: { start: number, ["end"]: number, parse: ("block" | "expr")? } }

--[=[
	The value your `format` function returns.

	Send `document` when the worm lays out its own syntax. Send `spans` when
	the worm only names the byte ranges that hold Luau: larvae formats each
	range and keeps every other byte as the author wrote it. `document` wins
	when you send both.

	`comments` holds the span of every comment. larvae refuses an output that
	drops one of them, so a layout bug cannot delete a comment silently.
]=]
export type FormatReply = {
	document: Doc?,
	spans: { Span }?,
	comments: { Span }?,
}

--[=[
	The format half of a front-end. Set `fmt = true` under `[frontend]` in
	worm.toml, and `larvae fmt` calls this function on every claimed file.

	To report a problem instead, call `error("...")` with a message for a
	human. larvae attributes the error to your worm.
]=]
export type Format = (source: string) -> FormatReply

--[=[
	One problem the worm found. There is no severity here. The host stamps
	the level from `[lint.rules]`, so a worm cannot decide that a build
	fails.

	`lint` names one of the lints your worm.toml declares under `[lints]`.
	`span` is the byte range the report points at.
]=]
export type Finding = {
	span: Span,
	lint: string,
	message: string,
	help: string?,
}

--[=[
	The value your `lint` function returns.

	`comments` holds the comment spans, so `-- larvae: allow(...)` works in a
	claimed file. `luau` is the Luau shadow of the file: the source, with
	every region that is not Luau replaced by filler of the same byte length.
	The builtin lints of larvae read the shadow, and every offset stays
	exact.
]=]
export type LintReply = {
	findings: { Finding }?,
	comments: { Span }?,
	luau: string?,
}

--[=[
	The lint half of a front-end. Declare lints under `[lints]` in worm.toml,
	and `larvae lint` calls this function on every claimed file.
]=]
export type Lint = (source: string) -> LintReply

--[=[
	The front-end role. It claims a file extension and turns the file into
	Luau before the pipeline of larvae reads it. `format` and `lint` are the
	optional halves that give the claimed files `larvae fmt` and
	`larvae lint`.

	Declare the extensions in worm.toml, not here:

		[frontend]
		claims = [".luaux"]
]=]
export type Frontend = {
	compile: Compile,
	format: Format?,
	lint: Lint?,
}

--[=[
	Every node kind of the larvae AST, as singleton strings.

	A comparison against one of these autocompletes, and a typo fails to
	typecheck. Thus a typo cannot silently match nothing. These are also the
	names you put in `filter` in worm.toml.
]=]
export type Kind =
	-- structure
	"Block"
	-- statements
	| "Local"
	| "Assign"
	| "Call"
	| "Do"
	| "While"
	| "Repeat"
	| "If"
	| "NumericFor"
	| "GenericFor"
	| "Function"
	| "LocalFunction"
	| "Return"
	| "Break"
	| "Continue"
	| "TypeAlias"
	| "Empty"
	-- expressions
	| "Nil"
	| "True"
	| "False"
	| "Vararg"
	| "Number"
	| "String"
	| "InterpString"
	| "Name"
	| "FunctionExpr"
	| "Table"
	| "Binary"
	| "Unary"
	| "Index"
	| "CallExpr"
	| "Paren"
	| "IfElse"
	| "TypeAssert"

--[=[
	A handle to one node of the larvae AST.

	The handle is a pair of integers, not a pointer into the tree. It is valid
	only for the file larvae gave it to you for. If you keep one across files,
	the next call errors and does not silently read the wrong file.

	Spans are byte offsets into the original source, as half open ranges.
]=]
export type Node = {
	kind: (self: Node) -> Kind,
	text: (self: Node) -> string,
	span: (self: Node) -> (number, number),
	children: (self: Node) -> { Node },
	parent: (self: Node) -> Node?,
}

--[=[
	The operations a rule can apply to the file it walks.

	larvae defers the edits and measures them against the original bytes. Thus
	you do not see the output of an other rule, and one splice occurs at the
	end. `remove` keeps the newlines of the node, because line N of dist must
	stay line N of src.

	`value` is the configured value of this rule: your worm.toml default,
	unless the user overrode it in [rules].
]=]
export type Context = {
	path: string,
	value: any,
	replace: (self: Context, node: Node, text: string) -> (),
	remove: (self: Context, node: Node) -> (),
}

--[=[
	One rule. It runs only on the nodes that match the `filter` you declared
	in worm.toml. Thus undeclared kinds do not cross the boundary at all.
]=]
export type Rule = {
	visit: (node: Node, ctx: Context) -> (),
}

--[=[
	The resolved settings of the project.

	`fmt` is the `[fmt]` table of larvae.toml, with every default filled in.
	`lint` maps each lint name to its resolved level. Read them to lay your
	own constructs out in the style of the project. Without them the user
	writes the same values a second time under `[config.<your name>]`, and
	the two copies drift.
]=]
export type Settings = {
	fmt: { [string]: any },
	lint: { [string]: any },
}

--[=[
	larvae calls this once, before it sees a file, with your settings.

	`config` is `[config.<your name>]` from larvae.toml as a table, already
	parsed. `rules` holds only the rules that are switched on, keyed by name,
	with the resolved value of each. A rule that is off is absent. `settings`
	holds the resolved project settings. A worm that names only the first two
	parameters stays correct, because Lua drops the extra argument.
]=]
export type Init = (config: { [string]: any }, rules: { [string]: any }, settings: Settings) -> ()

--[=[
	The value a worm module returns. There are three roles, and a worm can
	hold each combination.

	**A front-end** claims a file extension and turns the file into Luau
	before larvae reads it. It takes text and returns text. Use it for a
	syntax that larvae cannot parse. With the optional `format` and `lint`
	halves, the claimed files also serve `larvae fmt` and `larvae lint`.

	```luau
	local worm: Worm = {
		frontend = {
			compile = function(source, config)
				return (source:gsub("<>", "{}"))
			end,
		},
	}
	```

	**A rule** takes a slot in run_order, walks the AST, and queues edits. Use
	it for a transform over ordinary Luau. It sees only the node kinds you
	named in `filter`, so all other kinds stay on the fast path of larvae.

	```luau
	local worm: Worm = {
		rules = {
			strip_debug = {
				visit = function(node, ctx)
					if node:kind() == "CallExpr" and node:text():find("^dprint") then
						ctx:remove(node)
					end
				end,
			},
		},
	}
	```

	**Both roles** apply when a worm transpiles a syntax and also ships a rule
	over the result. This is one worm and not two, so there is one version to
	pin and one `[config.<name>]` to manage.

	```luau
	local worm: Worm = {
		init = function(config, rules)
			-- the settings arrive once, before the first file
		end,

		frontend = { compile = compileMarkup },

		rules = {
			inline_constants = { visit = inlineConstants },
		},
	}

	return worm
	```

	At least one role is required. larvae refuses a module that returns no
	role at load. It does not load a worm that it can never call.
]=]
export type Worm = {
	init: Init?,
	frontend: Frontend?,
	rules: { [string]: Rule }?,
}

return {}
