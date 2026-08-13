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
	The front-end role. It claims a file extension and turns the file into
	Luau before the pipeline of larvae reads it.

	Declare the extensions in worm.toml, not here:

		[frontend]
		claims = [".luaux"]
]=]
export type Frontend = {
	compile: Compile,
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
	larvae calls this once, before it sees a file, with your settings.

	`config` is `[config.<your name>]` from larvae.toml as a table, already
	parsed. `rules` holds only the rules that are switched on, keyed by name,
	with the resolved value of each. A rule that is off is absent.
]=]
export type Init = (config: { [string]: any }, rules: { [string]: any }) -> ()

--[=[
	The value a worm module returns. There are three roles, and a worm can
	hold each combination.

	**A front-end** claims a file extension and turns the file into Luau
	before larvae reads it. It takes text and returns text. Use it for a
	syntax that larvae cannot parse.

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
