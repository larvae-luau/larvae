--!strict
--[[
	Type definitions for worms written in Luau.

	Point luau-lsp at this file and your worm typechecks and autocompletes:

		luau-lsp analyze --definitions=worm.d.luau init.luau

	or in .vscode/settings.json:

		"luau-lsp.types.definitionFiles": ["worm.d.luau"]

	Write it out any time with `larvae worm types`.
]]

--[=[
	Source text in, source text out.

	`source` is the file's contents. `config` is whatever sits under
	`[config.<your name>]` in larvae.toml, re-serialized as TOML, or an empty
	string when the user wrote none. Parse it yourself, we never look at it.

	Return the transformed source. To report a problem instead, `error("...")`
	with a message meant for a human, and larvae will attribute it to your worm
	rather than crashing the build.

	Line count matters. larvae guarantees line N of dist is line N of src, and
	that only holds end to end if you preserve it too.
]=]
export type Compile = (source: string, config: string) -> string

--[=[
	The front-end role: claims a file extension and turns it into Luau before
	larvae's pipeline reads it.

	Declare the extensions in worm.toml, not here:

		[frontend]
		claims = [".luaux"]
]=]
export type Frontend = {
	compile: Compile,
}

--[=[
	Every node kind larvae's AST has, as singleton strings.

	Comparing against one of these autocompletes and typos fail to typecheck,
	rather than quietly never matching. These are also the names you put in
	`filter` in worm.toml.
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
	A handle to one node of larvae's AST.

	It is a pair of integers, not a pointer into our tree, and it is only valid
	during the file it was handed to you for. Keep one across files and the next
	call errors rather than silently reading the wrong file.

	Spans are byte offsets into the original source, half open.
]=]
export type Node = {
	kind: (self: Node) -> Kind,
	text: (self: Node) -> string,
	span: (self: Node) -> (number, number),
	children: (self: Node) -> { Node },
	parent: (self: Node) -> Node?,
}

--[=[
	What a rule may do to the file it is walking.

	Edits are deferred and measured against the original bytes, so you never see
	another rule's output and one splice happens at the end. `remove` leaves the
	node's newlines behind, because line N of dist must stay line N of src.

	`value` is what this rule was configured with, your worm.toml default unless
	the user overrode it in [rules].
]=]
export type Context = {
	path: string,
	value: any,
	replace: (self: Context, node: Node, text: string) -> (),
	remove: (self: Context, node: Node) -> (),
}

--[=[
	One rule. It runs only on nodes matching the `filter` you declared in
	worm.toml, so undeclared kinds never cross the boundary at all.
]=]
export type Rule = {
	visit: (node: Node, ctx: Context) -> (),
}

--[=[
	Called once, before any file is seen, with your settings.

	`config` is `[config.<your name>]` from larvae.toml as a table, already
	parsed. `rules` holds only the rules that are switched on, keyed by name,
	with the value each resolved to. A rule that is off is simply absent.
]=]
export type Init = (config: { [string]: any }, rules: { [string]: any }) -> ()

--[=[
	What a worm module returns. Three roles, and a worm may hold any combination.

	**A front-end** claims a file extension and turns it into Luau before larvae
	reads it. Text in, text out. Use it for a syntax larvae cannot parse.

	```luau
	local worm: Worm = {
		frontend = {
			compile = function(source, config)
				return (source:gsub("<>", "{}"))
			end,
		},
	}
	```

	**A rule** takes a slot in run_order and walks the AST, queueing edits. Use
	it for a transform over ordinary Luau. It only sees the node kinds you named
	in `filter`, so anything else stays on larvae's fast path.

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

	**Both**, when a worm transpiles a syntax and also ships a rule over the
	result. One worm rather than two, so there is one version to pin and one
	`[config.<name>]` to keep straight.

	```luau
	local worm: Worm = {
		init = function(config, rules)
			-- settings arrive once, before any file
		end,

		frontend = { compile = compileMarkup },

		rules = {
			inline_constants = { visit = inlineConstants },
		},
	}

	return worm
	```

	At least one role is required. Returning neither is refused at load rather
	than loaded and never called.
]=]
export type Worm = {
	init: Init?,
	frontend: Frontend?,
	rules: { [string]: Rule }?,
}

return {}
