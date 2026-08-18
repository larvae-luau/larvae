/*!
`larvae bundle`, the whole project as one file.

Two halves. This one decides *which* modules are in and rewrites their
requires to point at each other; [`emit`] decides what the file looks like
and holds the runtime that makes it behave.

## What it is for

Roblox has no payload pressure, so a bundle here is not about bytes. It is
for the cases where a tree of modules must arrive as one instance: a plugin,
a library published as a single ModuleScript, a script pasted into a place
file.

## Reachability

The plan collects modules with a walk over the requires from the entry, not
with a list of the input directory. So a bundle contains what the entry can
reach. That walk is also the dead code elimination: the walk never visits a
module that nothing requires, so the bundle never contains it, and no
separate pass is necessary.

The cost of that is dynamic requires. The walk cannot follow
`require(someVariable)`, so a module reached only that way is not in the
bundle, and the call fails at run time. The rewrite reports a dynamic
require in a bundled module, so the problem does not wait for run time.
*/

pub mod emit;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::diag::Diag;
use crate::requires::graph::Graph;

/// Everything a bundle needs, worked out before a byte is written
pub struct Plan {
    /// Every module in the bundle, by path, with the id it is known by
    pub modules: BTreeMap<PathBuf, String>,
    pub graph: Graph,
    /// The entry, in the node form of the graph
    pub entry: PathBuf,
    /// The findings of the walk, for the caller to print
    pub diags: Vec<Diag>,
}

/*
Walk out from the entry, and collect what it can reach.

The walk runs over the already resolved graph and does not resolve again. So
the bundle agrees with `check` about what requires what by construction: two
passes that resolve independently are two chances to disagree.

With `tree_shake` off, every module of the project enters the bundle, the
unreachable ones included. A project keeps them for the requires the walk
cannot follow: a dynamic require finds its module in the registry at run
time only when the bundle contains it.
*/
pub fn plan(root: &Path, entry: &Path, graph: &Graph, tree_shake: bool) -> Result<Plan> {
    if !entry.exists() {
        bail!("the bundle entry {} does not exist", crate::ui::rel(entry));
    }

    let entry = entry
        .canonicalize()
        .with_context(|| format!("cannot resolve {}", crate::ui::rel(entry)))?;

    // An init file keys on its directory in the graph, so the entry must
    // take the same form, or the walk starts on a node with no edges.
    let entry = crate::requires::graph::node_of(&entry).to_path_buf();

    let mut modules: BTreeMap<PathBuf, String> = BTreeMap::new();
    let mut queue = vec![entry.clone()];

    while let Some(path) = queue.pop() {
        if modules.contains_key(&path) {
            continue;
        }

        modules.insert(path.clone(), emit::module_id(root, &path));

        for to in graph.requires_of(&path) {
            if !modules.contains_key(to) {
                queue.push(to.clone());
            }
        }
    }

    if !tree_shake {
        for node in graph.nodes() {
            if !modules.contains_key(node) {
                modules.insert(node.to_path_buf(), emit::module_id(root, node));
            }
        }
    }

    /*
    A worm that owns its requires resolves them out of sight, so the graph
    has no edges for the file. The walk cannot follow what it cannot see,
    and silence here would turn that into a missing module at run time.
    */
    let diags = modules
        .keys()
        .filter(|path| graph.is_opaque(path))
        .map(|path| {
            Diag::warning(
                path,
                "a worm resolves the requires of this module, so the bundle cannot follow them",
            )
            .with_help("a module reached only through this one is not in the bundle")
        })
        .collect();

    Ok(Plan {
        modules,
        graph: graph.clone(),
        entry,
        diags,
    })
}

/*
One module's source, with every require pointing into the bundle instead.

The spans come from the require sites of the graph, not from a new
resolution: a site holds the byte range of the string token, and nothing
about the rewrite needs a parse. The splices apply back to front, so every
earlier offset stays valid, for the same reason the edit list elsewhere
sorts before it applies.
*/
pub fn rewrite(src: &str, path: &Path, plan: &Plan, diags: &mut Vec<Diag>) -> Result<String> {
    let lexed = crate::syntax::lexer::lex(src)
        .map_err(|e| anyhow::anyhow!("{} at byte {}", e.message, e.offset))
        .with_context(|| format!("cannot bundle {}", crate::ui::rel(path)))?;

    let scanned = crate::syntax::scan::scan(src, &lexed.toks);

    /*
    A require whose argument is not a literal cannot be followed, so what it
    names can be missing from the bundle. This is the only chance to say so:
    at run time it is a missing module with no clue where it came from.
    */
    for site in &scanned.dynamic {
        diags.push(
            Diag::warning(
                path,
                "this require is computed, so what it names cannot be bundled",
            )
            .at(src, *site as usize)
            .with_help("require a literal path, or the call fails inside the bundle"),
        );
    }

    /*
    Every change is one splice list, sorted and applied back to front, so
    each earlier offset stays valid whatever order the changes were found
    in. Two kinds of change land here. A require splice points the call
    into the bundle. An `export type` loses its `export`: the keyword is
    legal at the top level of a module only, the bundle wraps every module
    in a function, and a plain `type` alias is legal in any block while
    changing nothing at run time.

    `export local` and its siblings stay, because their run time meaning
    is the module's value, and a bundle cannot erase that without changing
    what the module returns. A runtime that accepts them at the top level
    is the runtime this bundle targets.
    */
    let mut splices: Vec<(usize, usize, String)> = Vec::new();

    if let Ok(chunk) = crate::syntax::parser::parse(src, &lexed.toks) {
        for stmt in &chunk.block.stmts {
            if let crate::syntax::ast::Stmt::TypeAlias(alias) = stmt
                && alias.exported
            {
                let tok = &lexed.toks[alias.span.start as usize];

                if tok.text(src) == "export" {
                    let start = tok.start as usize;
                    let end = start
                        + "export".len()
                        + src[start + "export".len()..]
                            .chars()
                            .take_while(|c| *c == ' ')
                            .count();

                    splices.push((start, end, String::new()));
                }
            }
        }
    }

    /*
    Two splices per require: the string token becomes the module id, and the
    `require` identifier becomes `__require`. One splice for the whole call
    would need the end of the call, and the scanner does not record it: it
    records the identifier and the string token, which is enough for both
    halves. `f "x"` without parentheses is a call too, and the two splices
    handle it without more knowledge.
    */
    for site in plan.graph.sites_of(path) {
        let Some(id) = plan.modules.get(&site.target) else {
            continue;
        };

        splices.push((
            site.tok_start as usize,
            site.tok_end as usize,
            emit::quote(id),
        ));

        splices.push((
            site.at as usize,
            site.at as usize + "require".len(),
            "__require".to_string(),
        ));
    }

    splices.sort_by_key(|(start, _, _)| *start);

    let mut out = src.to_string();

    for (start, end, text) in splices.iter().rev() {
        out.replace_range(start..end, text);
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn graph_of(edges: &[(&str, &str)]) -> Graph {
        let mut g = Graph::default();

        for (from, to) in edges {
            g.add(Path::new(from), Path::new(to));
        }

        g
    }

    /// A plan checks that the entry exists, so the walk needs a real tree
    fn tree(files: &[&str]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();

        for f in files {
            let path = dir.path().join(f);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, "return 1\n").unwrap();
        }

        dir
    }

    #[test]
    fn the_plan_reaches_every_module_the_entry_requires() {
        let dir = tree(&["a.luau", "b.luau", "c.luau"]);
        let root = dir.path().canonicalize().unwrap();

        let g = graph_of(&[
            (
                root.join("a.luau").to_str().unwrap(),
                root.join("b.luau").to_str().unwrap(),
            ),
            (
                root.join("b.luau").to_str().unwrap(),
                root.join("c.luau").to_str().unwrap(),
            ),
        ]);

        let plan = plan(&root, &root.join("a.luau"), &g, true).unwrap();

        assert_eq!(plan.modules.len(), 3, "{:?}", plan.modules);
    }

    /// The dead code elimination, which is the walk not visiting it
    #[test]
    fn a_module_the_entry_cannot_reach_is_left_out() {
        let dir = tree(&["a.luau", "b.luau", "orphan.luau"]);
        let root = dir.path().canonicalize().unwrap();

        let mut g = graph_of(&[(
            root.join("a.luau").to_str().unwrap(),
            root.join("b.luau").to_str().unwrap(),
        )]);
        g.see(&root.join("orphan.luau"));

        let plan = plan(&root, &root.join("a.luau"), &g, true).unwrap();

        assert_eq!(plan.modules.len(), 2);
        assert!(!plan.modules.contains_key(&root.join("orphan.luau")));
    }

    /// With tree_shake off, the walk result grows to the whole project
    #[test]
    fn without_tree_shaking_every_module_is_in() {
        let dir = tree(&["a.luau", "b.luau", "orphan.luau"]);
        let root = dir.path().canonicalize().unwrap();

        let mut g = graph_of(&[(
            root.join("a.luau").to_str().unwrap(),
            root.join("b.luau").to_str().unwrap(),
        )]);
        g.see(&root.join("orphan.luau"));

        let plan = plan(&root, &root.join("a.luau"), &g, false).unwrap();

        assert!(plan.modules.contains_key(&root.join("orphan.luau")));
    }

    /// A cycle must not make the walk loop
    #[test]
    fn a_cycle_terminates_and_includes_both_modules() {
        let dir = tree(&["a.luau", "b.luau"]);
        let root = dir.path().canonicalize().unwrap();

        let (a, b) = (root.join("a.luau"), root.join("b.luau"));
        let g = graph_of(&[
            (a.to_str().unwrap(), b.to_str().unwrap()),
            (b.to_str().unwrap(), a.to_str().unwrap()),
        ]);

        let plan = plan(&root, &a, &g, true).unwrap();

        assert_eq!(plan.modules.len(), 2);
    }

    /// The graph keys an init file on its directory, and the entry must match
    #[test]
    fn an_init_file_entry_walks_from_its_directory_node() {
        let dir = tree(&["pkg/init.luau", "pkg/helper.luau"]);
        let root = dir.path().canonicalize().unwrap();

        let g = graph_of(&[(
            root.join("pkg").to_str().unwrap(),
            root.join("pkg/helper.luau").to_str().unwrap(),
        )]);

        let plan = plan(&root, &root.join("pkg/init.luau"), &g, true).unwrap();

        assert_eq!(plan.entry, root.join("pkg"));
        assert_eq!(plan.modules.len(), 2, "{:?}", plan.modules);
    }

    #[test]
    fn a_missing_entry_is_an_error_naming_it() {
        let dir = tempfile::tempdir().unwrap();
        let Err(err) = plan(
            dir.path(),
            &dir.path().join("nope.luau"),
            &Graph::default(),
            true,
        ) else {
            panic!("there is no entry there to plan from")
        };

        assert!(format!("{err:#}").contains("nope.luau"));
    }

    /// The walk cannot follow the requires of an opaque module
    #[test]
    fn an_opaque_module_in_the_bundle_is_reported() {
        let dir = tree(&["a.luau", "styled.luau"]);
        let root = dir.path().canonicalize().unwrap();

        let mut g = graph_of(&[(
            root.join("a.luau").to_str().unwrap(),
            root.join("styled.luau").to_str().unwrap(),
        )]);
        g.see_opaque(&root.join("styled.luau"));

        let plan = plan(&root, &root.join("a.luau"), &g, true).unwrap();

        assert_eq!(plan.diags.len(), 1, "{:?}", plan.diags);
        assert!(plan.diags[0].file.ends_with("styled.luau"));
    }

    // --- rewriting -----------------------------------------------------------

    /// `src` matters: the site spans must match the text under rewrite
    fn one_module_plan(root: &Path, from: &Path, to: &Path, src: &str) -> Plan {
        let mut modules = BTreeMap::new();
        modules.insert(from.to_path_buf(), emit::module_id(root, from));
        modules.insert(to.to_path_buf(), emit::module_id(root, to));

        let mut graph = graph_of(&[(from.to_str().unwrap(), to.to_str().unwrap())]);

        let lexed = crate::syntax::lexer::lex(src).unwrap();

        for site in crate::syntax::scan::scan(src, &lexed.toks).sites {
            graph.add_site(
                from,
                crate::requires::graph::Site {
                    at: site.at,
                    tok_start: site.tok_start,
                    tok_end: site.tok_end,
                    target: to.to_path_buf(),
                },
            );
        }

        Plan {
            modules,
            graph,
            entry: from.to_path_buf(),
            diags: Vec::new(),
        }
    }

    fn empty_plan(entry: &Path) -> Plan {
        Plan {
            modules: BTreeMap::new(),
            graph: Graph::default(),
            entry: entry.to_path_buf(),
            diags: Vec::new(),
        }
    }

    #[test]
    fn a_require_becomes_a_call_into_the_bundle() {
        let root = Path::new("/p");
        let (a, b) = (Path::new("/p/a.luau"), Path::new("/p/b.luau"));
        let src = "local x = require(\"./b\")\n";
        let plan = one_module_plan(root, a, b, src);

        assert_eq!(
            rewrite(src, a, &plan, &mut Vec::new()).unwrap(),
            "local x = __require(\"b\")\n"
        );
    }

    #[test]
    fn the_rest_of_the_line_is_untouched() {
        let root = Path::new("/p");
        let (a, b) = (Path::new("/p/a.luau"), Path::new("/p/b.luau"));
        let src = "local x = require(\"./b\").field -- note\n";
        let plan = one_module_plan(root, a, b, src);

        assert_eq!(
            rewrite(src, a, &plan, &mut Vec::new()).unwrap(),
            "local x = __require(\"b\").field -- note\n"
        );
    }

    /// The walk cannot follow it, and a discovery at run time is much worse
    #[test]
    fn a_computed_require_is_reported() {
        let a = Path::new("/p/a.luau");
        let plan = empty_plan(a);

        let mut diags = Vec::new();
        rewrite("local x = require(name)\n", a, &plan, &mut diags).unwrap();

        assert_eq!(diags.len(), 1);
        assert!(
            diags[0].message.contains("computed"),
            "{}",
            diags[0].message
        );
    }

    /// The graph deduplicates edges. A pairing of sites against edges by
    /// position rewrote the wrong call the second time a module was required.
    #[test]
    fn requiring_one_module_twice_rewrites_both_calls() {
        let root = Path::new("/p");
        let (a, b) = (Path::new("/p/a.luau"), Path::new("/p/b.luau"));
        let src = "local x = require(\"./b\")\nlocal y = require(\"./b\")\n";
        let plan = one_module_plan(root, a, b, src);

        assert_eq!(
            rewrite(src, a, &plan, &mut Vec::new()).unwrap(),
            "local x = __require(\"b\")\nlocal y = __require(\"b\")\n"
        );
    }

    #[test]
    fn a_module_with_no_requires_comes_through_unchanged() {
        let a = Path::new("/p/a.luau");
        let plan = empty_plan(a);

        let src = "local x = 1\nreturn x\n";

        assert_eq!(rewrite(src, a, &plan, &mut Vec::new()).unwrap(), src);
    }

    #[test]
    fn a_file_that_does_not_lex_is_refused_naming_it() {
        let a = Path::new("/p/a.luau");
        let plan = empty_plan(a);

        let err = rewrite("local x = \"unterminated\n", a, &plan, &mut Vec::new())
            .expect_err("should not lex");

        assert!(format!("{err:#}").contains("a.luau"));
    }
}
