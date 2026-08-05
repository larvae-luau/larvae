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
	What a worm module returns.

	```luau
	local worm: Worm = {
		frontend = {
			compile = function(source, config)
				return source:gsub("<>", "{}")
			end,
		},
	}

	return worm
	```

	`frontend` is required because it is the only role that exists today, which
	also means a misspelled key is caught rather than read as "no frontend".
	The `rules` role, which takes a slot in run_order and pushes edits against
	larvae's AST, is not implemented yet. When it lands it appears here and
	both become optional.
]=]
export type Worm = {
	frontend: Frontend,
}

return {}
