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
	A handle to one node of larvae's AST.

	It is a pair of integers, not a pointer into our tree, and it is only valid
	during the file it was handed to you for. Keep one across files and the next
	call errors rather than silently reading the wrong file.

	Spans are byte offsets into the original source, half open.
]=]
export type Node = {
	kind: (self: Node) -> string,
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
	What a worm module returns.

	```luau
	local worm: Worm = {
		rules = {
			strip_debug = {
				visit = function(node, ctx)
					if node:kind() == "CallExpr" then
						ctx:remove(node)
					end
				end,
			},
		},
	}

	return worm
	```

	A worm needs at least one role. Both are optional here because either may
	be absent, but returning neither is refused at load.
]=]
export type Worm = {
	init: Init?,
	frontend: Frontend?,
	rules: { [string]: Rule }?,
}

return {}
