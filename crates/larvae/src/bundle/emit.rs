/*!
The single file, and the runtime that makes it behave like the project did.

## The shape

Every module becomes a thunk in a registry, and requires between them become
calls into a loader:

```lua
local __larvae = {}
local __result = {}
local __loading = {}

local function __require(id) ... end

__larvae["src/shared/util"] = function()
    -- the module's own source, requires rewritten
end

return __require("src/main")
```

## Why lazy thunks and not an initialisation order

A bundler can emit modules in dependency order and run them top to bottom.
Then a cycle has no valid order, and the emitter must invent one. Thunks
remove the question: nothing runs until something requires it, so
**initialisation order is demand order**, which is exactly the order the
unbundled project had. A bundle therefore cannot move the side effects of a
module, and that property makes bundling safe to turn on for an existing
game.

The topological sort still runs, for a different reason: it decides the
order the thunks are *written* to the file, so the bundle is byte identical
across runs. Correctness does not depend on it.

## Cycles

A cyclic require that happens at load time errors, naming the module. This
matches unbundled Roblox, which raises "Requested module was required
recursively". And an error here is better than a half built module: a nil
field read three files away is much harder to trace back than an error at
the require that caused it.

The pattern that looks cyclic and is not, a require inside a function that
runs later, keeps working: by the time it runs, the other module has
finished. That is the common shape in Roblox code, and the reason `check`
reports cycles as a warning and not an error.
*/

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// One module, ready to be written into the bundle
pub struct Module {
    /// What `__require` is called with, stable and readable in an error
    pub id: String,
    /// The module's source, with its requires already rewritten
    pub source: String,
}

/*
The loader.

Written out and not generated, because it is the one part of the output a
human reads when something goes wrong, and it is the same every time.

`__result` holds a one field table and not the value itself, so a module
that legitimately returns nil is still cached: `__result[id]` asks whether
the module has run, which is not the same question as whether it returned
something.
*/
const RUNTIME: &str = r#"local __larvae = {}
local __result = {}
local __loading = {}

local function __require(id)
	local cached = __result[id]

	if cached then
		return cached.value
	end

	if __loading[id] then
		error("cyclic require of " .. id .. ", it is still loading", 2)
	end

	local thunk = __larvae[id]

	if not thunk then
		error("no module " .. id .. " in this bundle", 2)
	end

	__loading[id] = true
	local value = thunk()
	__loading[id] = nil
	__result[id] = { value = value }

	return value
end
"#;

/// Write the bundle, entry last so it runs once everything is registered
pub fn write(modules: &[Module], entry: &str) -> String {
    let mut out = String::with_capacity(
        RUNTIME.len() + modules.iter().map(|m| m.source.len() + 64).sum::<usize>(),
    );

    out.push_str(RUNTIME);

    for module in modules {
        out.push('\n');
        out.push_str(&format!("__larvae[{}] = function()\n", quote(&module.id)));

        /*
        Indented by one tab, which is cosmetic, and otherwise untouched,
        which is not. A module's own source must reach the runtime byte for
        byte, or a long string that contains Luau-like text changes meaning.
        */
        for line in module.source.lines() {
            if line.is_empty() {
                out.push('\n');
            } else {
                out.push('\t');
                out.push_str(line);
                out.push('\n');
            }
        }

        out.push_str("end\n");
    }

    out.push_str(&format!("\nreturn __require({})\n", quote(entry)));

    out
}

/// A Luau string literal, escaped
pub fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');

    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),

            '\\' => out.push_str("\\\\"),

            '\n' => out.push_str("\\n"),

            '\r' => out.push_str("\\r"),

            c => out.push(c),
        }
    }

    out.push('"');

    out
}

/*
The id a module is known by inside the bundle.

The path relative to the project root, without its extension. So the id
reads like the module the author wrote and not like an opaque number, and a
runtime error names something findable. Separators normalise to `/`, so a
bundle built on Windows and one built on Linux come out identical.
*/
pub fn module_id(root: &Path, path: &Path) -> String {
    let rel = path.strip_prefix(root).unwrap_or(path);

    let text = rel.to_string_lossy().replace('\\', "/");

    text.strip_suffix(".luau")
        .or_else(|| text.strip_suffix(".lua"))
        .unwrap_or(&text)
        .to_string()
}

/*
The order thunks are written in, dependencies first where such an order
exists.

Only for reproducibility, per the note at the top: the runtime does not
care. A cyclic graph has no topological order, so those modules fall back to
sorted by path, which is arbitrary but stable.
*/
pub fn emission_order(
    graph: &crate::requires::graph::Graph,
    reachable: &BTreeMap<PathBuf, String>,
) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = match graph.topological() {
        Some(order) => order
            .into_iter()
            .filter(|p| reachable.contains_key(p))
            .collect(),

        None => Vec::new(),
    };

    // Anything the sort could not place, which is every module when cyclic.
    let mut rest: Vec<PathBuf> = reachable
        .keys()
        .filter(|p| !out.contains(p))
        .cloned()
        .collect();

    rest.sort();
    out.extend(rest);

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn module(id: &str, source: &str) -> Module {
        Module {
            id: id.to_string(),
            source: source.to_string(),
        }
    }

    #[test]
    fn the_bundle_registers_every_module_and_returns_the_entry() {
        let out = write(&[module("a", "return 1"), module("b", "return 2")], "a");

        assert!(out.contains(r#"__larvae["a"] = function()"#), "{out}");
        assert!(out.contains(r#"__larvae["b"] = function()"#), "{out}");
        assert!(
            out.trim_end().ends_with(r#"return __require("a")"#),
            "{out}"
        );
    }

    #[test]
    fn a_module_body_is_indented_but_otherwise_untouched() {
        let out = write(&[module("a", "local x = 1\nreturn x")], "a");

        assert!(out.contains("\tlocal x = 1\n\treturn x\n"), "{out}");
    }

    /// An indented blank line is trailing whitespace, which nothing wants
    #[test]
    fn a_blank_line_in_a_module_stays_blank() {
        let out = write(&[module("a", "local x = 1\n\nreturn x")], "a");

        assert!(!out.contains("\t\n"), "indented a blank line: {out:?}");
    }

    #[test]
    fn the_runtime_comes_before_any_module() {
        let out = write(&[module("a", "return 1")], "a");
        let runtime = out.find("local function __require").expect("runtime");
        let first = out.find("__larvae[").expect("a module");

        assert!(runtime < first, "the loader must be defined first");
    }

    // --- ids ---------------------------------------------------------------

    #[test]
    fn an_id_is_the_path_without_its_extension() {
        let id = module_id(Path::new("/p"), Path::new("/p/src/shared/util.luau"));

        assert_eq!(id, "src/shared/util");
    }

    #[test]
    fn a_lua_extension_is_stripped_too() {
        assert_eq!(module_id(Path::new("/p"), Path::new("/p/a.lua")), "a");
    }

    /// A directory module has no extension; its id is the directory
    #[test]
    fn a_directory_node_keeps_its_name() {
        assert_eq!(
            module_id(Path::new("/p"), Path::new("/p/src/pkg")),
            "src/pkg"
        );
    }

    /// So a bundle built on Windows matches one built anywhere else
    #[test]
    fn separators_are_normalised() {
        let id = module_id(Path::new("/p"), Path::new("/p/src/a.luau"));

        assert!(!id.contains('\\'), "{id}");
    }

    // --- quoting -----------------------------------------------------------

    #[test]
    fn a_quote_or_backslash_in_an_id_is_escaped() {
        assert_eq!(quote(r#"a"b"#), r#""a\"b""#);
        assert_eq!(quote(r"a\b"), r#""a\\b""#);
    }

    #[test]
    fn a_newline_cannot_break_out_of_the_literal() {
        assert_eq!(quote("a\nb"), r#""a\nb""#);
    }

    // --- order -------------------------------------------------------------

    fn graph_of(edges: &[(&str, &str)]) -> crate::requires::graph::Graph {
        let mut g = crate::requires::graph::Graph::default();

        for (from, to) in edges {
            g.add(Path::new(from), Path::new(to));
        }

        g
    }

    fn reachable(names: &[&str]) -> BTreeMap<PathBuf, String> {
        names
            .iter()
            .map(|n| (PathBuf::from(*n), (*n).to_string()))
            .collect()
    }

    #[test]
    fn dependencies_are_written_before_the_modules_that_need_them() {
        let g = graph_of(&[("a", "b"), ("b", "c")]);
        let order = emission_order(&g, &reachable(&["a", "b", "c"]));

        let at = |s: &str| order.iter().position(|p| p == Path::new(s)).unwrap();

        assert!(at("c") < at("b") && at("b") < at("a"), "{order:?}");
    }

    /// No topological order exists, and the output must stay stable
    #[test]
    fn a_cyclic_graph_still_emits_every_module_in_a_stable_order() {
        let g = graph_of(&[("a", "b"), ("b", "a")]);
        let names = reachable(&["a", "b"]);

        let once = emission_order(&g, &names);
        let twice = emission_order(&g, &names);

        assert_eq!(once, twice);
        assert_eq!(once.len(), 2);
    }

    #[test]
    fn a_module_outside_the_reachable_set_is_not_emitted() {
        let g = graph_of(&[("a", "b")]);
        let order = emission_order(&g, &reachable(&["a"]));

        assert_eq!(order, [PathBuf::from("a")]);
    }
}
