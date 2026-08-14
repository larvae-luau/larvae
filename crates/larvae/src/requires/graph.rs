/*!
The require graph: which module requires which, and the answers that gives.

`check` answers three whole project questions, and no single file can answer
them: a cycle is a property of the whole graph, an unused module is a module
with no incoming edge, and a bundle initialisation order is a topological
sort. So resolution collects the edges while it walks every require, and the
analyses run once at the end.

The edges are filesystem paths, not DataModel paths, on purpose. Two requires
can spell one module differently: `./sibling` from one file, and
`@game/ReplicatedStorage/app/sibling` from another. Both resolve to the same
file, so both are one node here. A graph keyed on the written form would
count that module as two nodes, and would find no cycle and no reuse.

Collection is per file and lock free, as section 4.4 asks: every worker
fills its own edge list, and the merge is one serial pass at the end.
*/

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/*
The graph node for a file on disk.

A require of a directory module resolves to the directory, not to the init
file inside it. So the node for `foo/init.luau` is `foo`, and the edge into
the directory meets the edges out of the init file on one node. Only a plain
`init.luau` or `init.lua` forms a directory module, so only those two names
map to the parent.
*/
pub fn node_of(path: &Path) -> &Path {
    let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");

    if name == "init.luau" || name == "init.lua" {
        return path.parent().unwrap_or(path);
    }

    path
}

/// Every require that resolved to a module, as file to file edges
#[derive(Debug, Default, Clone)]
pub struct Graph {
    /// The modules each module requires, in the order the requires appear
    edges: BTreeMap<PathBuf, Vec<PathBuf>>,
    /// Every module the walk saw, including leaves that nothing requires
    nodes: BTreeSet<PathBuf>,
    /// The files whose requires the walk does not see; a worm resolves them
    opaque: BTreeSet<PathBuf>,
}

impl Graph {
    /// Record that `from` requires `to`
    pub fn add(&mut self, from: &Path, to: &Path) {
        self.nodes.insert(from.to_path_buf());
        self.nodes.insert(to.to_path_buf());

        let out = self.edges.entry(from.to_path_buf()).or_default();

        // The same module required twice is one edge. So a cycle is reported
        // once, not once for each require site.
        if !out.iter().any(|p| p == to) {
            out.push(to.to_path_buf());
        }
    }

    /// Record a file from the walk, with or without requires
    pub fn see(&mut self, file: &Path) {
        self.nodes.insert(file.to_path_buf());
    }

    /*
    Record a file whose requires larvae does not see.

    A worm that owns its requires resolves them itself, so the walk collects
    no edges for the file. The file is still a node. The unused analysis
    checks for this mark, because a node with an unknown edge list makes
    "nothing requires this" a guess.
    */
    pub fn see_opaque(&mut self, file: &Path) {
        self.nodes.insert(file.to_path_buf());
        self.opaque.insert(file.to_path_buf());
    }

    /// True when a node with an unknown edge list exists
    pub fn has_opaque(&self) -> bool {
        !self.opaque.is_empty()
    }

    /// Fold the edges of another worker in; this is the serial half of section 4.4
    pub fn merge(&mut self, other: Graph) {
        for node in other.nodes {
            self.nodes.insert(node);
        }

        for node in other.opaque {
            self.opaque.insert(node);
        }

        for (from, tos) in other.edges {
            let out = self.edges.entry(from).or_default();

            for to in tos {
                if !out.contains(&to) {
                    out.push(to);
                }
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    pub fn nodes(&self) -> impl Iterator<Item = &Path> {
        self.nodes.iter().map(PathBuf::as_path)
    }

    pub fn requires_of(&self, file: &Path) -> &[PathBuf] {
        self.edges.get(file).map_or(&[], Vec::as_slice)
    }

    /*
    Every cycle, each reported once and sorted from its lowest member.

    This uses Tarjan's strongly connected components, not a search for back
    edges. A back edge finds one cycle. A strongly connected component finds
    the full set of modules that reach each other, which is the set a reader
    must break and a bundler must condense. A report of `a -> b -> a` for a
    tangle of three modules points at the wrong file.

    A component of one module is a cycle only when the module requires
    itself.
    */
    pub fn cycles(&self) -> Vec<Vec<PathBuf>> {
        let mut state = Tarjan {
            graph: self,
            index: BTreeMap::new(),
            low: BTreeMap::new(),
            on_stack: BTreeSet::new(),
            stack: Vec::new(),
            next: 0,
            out: Vec::new(),
        };

        for node in &self.nodes {
            if !state.index.contains_key(node) {
                state.walk(node);
            }
        }

        for cycle in &mut state.out {
            cycle.sort();
        }

        state.out.sort();

        state.out
    }

    /*
    The modules with no incoming edge that are not entry points.

    An entry point is a module the project runs directly, and the graph
    alone cannot know one: a `Script` under `ServerScriptService` has no
    incoming edge and is the whole program. So the caller passes the entry
    points it knows, and every other module with no incoming edge is a
    finding.
    */
    pub fn unused(&self, entries: &[PathBuf]) -> Vec<PathBuf> {
        let mut required: BTreeSet<&Path> = BTreeSet::new();

        for tos in self.edges.values() {
            for to in tos {
                required.insert(to.as_path());
            }
        }

        self.nodes
            .iter()
            .filter(|n| !required.contains(n.as_path()))
            .filter(|n| !entries.iter().any(|e| e == *n))
            .cloned()
            .collect()
    }

    /*
    The modules in an order a bundle can initialise them, dependencies first.

    The result is `None` when the graph has a cycle, because no such order
    exists. An invented order makes a bundler emit code that reads a nil
    field at startup. A caller that wants an order anyway condenses the
    cycles first.
    */
    pub fn topological(&self) -> Option<Vec<PathBuf>> {
        if !self.cycles().is_empty() {
            return None;
        }

        let mut seen: BTreeSet<&Path> = BTreeSet::new();
        let mut out = Vec::with_capacity(self.nodes.len());

        for node in &self.nodes {
            self.emit_after_deps(node, &mut seen, &mut out);
        }

        Some(out)
    }

    fn emit_after_deps<'g>(
        &'g self,
        node: &'g Path,
        seen: &mut BTreeSet<&'g Path>,
        out: &mut Vec<PathBuf>,
    ) {
        if !seen.insert(node) {
            return;
        }

        for to in self.requires_of(node) {
            self.emit_after_deps(to, seen, out);
        }

        out.push(node.to_path_buf());
    }
}

/// Tarjan's SCC state, in its own struct so the recursion carries one borrow
struct Tarjan<'g> {
    graph: &'g Graph,
    index: BTreeMap<PathBuf, u32>,
    low: BTreeMap<PathBuf, u32>,
    on_stack: BTreeSet<PathBuf>,
    stack: Vec<PathBuf>,
    next: u32,
    out: Vec<Vec<PathBuf>>,
}

impl Tarjan<'_> {
    fn walk(&mut self, node: &Path) {
        let key = node.to_path_buf();

        self.index.insert(key.clone(), self.next);
        self.low.insert(key.clone(), self.next);
        self.next += 1;
        self.stack.push(key.clone());
        self.on_stack.insert(key.clone());

        for to in self.graph.requires_of(node) {
            if !self.index.contains_key(to) {
                self.walk(to);

                let child = self.low[to];
                let mine = self.low[&key];
                self.low.insert(key.clone(), mine.min(child));
            } else if self.on_stack.contains(to) {
                let child = self.index[to];
                let mine = self.low[&key];
                self.low.insert(key.clone(), mine.min(child));
            }
        }

        if self.low[&key] != self.index[&key] {
            return;
        }

        let mut component = Vec::new();

        while let Some(top) = self.stack.pop() {
            self.on_stack.remove(&top);
            let last = top == key;
            component.push(top);

            if last {
                break;
            }
        }

        /*
        One module is a cycle only when it requires itself. Every other
        component of one module is an ordinary module, and a report of those
        would name the whole project.
        */
        let self_edge = component.len() == 1
            && self
                .graph
                .requires_of(&component[0])
                .iter()
                .any(|to| *to == component[0]);

        if component.len() > 1 || self_edge {
            self.out.push(component);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    fn graph(edges: &[(&str, &str)]) -> Graph {
        let mut g = Graph::default();

        for (from, to) in edges {
            g.add(Path::new(from), Path::new(to));
        }

        g
    }

    #[test]
    fn an_edge_records_both_ends_as_nodes() {
        let g = graph(&[("a", "b")]);

        assert_eq!(
            g.nodes().collect::<Vec<_>>(),
            [Path::new("a"), Path::new("b")]
        );
        assert_eq!(g.requires_of(Path::new("a")), [p("b")]);
        assert!(g.requires_of(Path::new("b")).is_empty());
    }

    /// Two requires of one module are one edge, or a cycle reports twice
    #[test]
    fn the_same_edge_twice_is_recorded_once() {
        let g = graph(&[("a", "b"), ("a", "b")]);

        assert_eq!(g.requires_of(Path::new("a")).len(), 1);
    }

    #[test]
    fn a_file_with_no_requires_is_still_a_node() {
        let mut g = Graph::default();
        g.see(Path::new("lonely"));

        assert_eq!(g.nodes().count(), 1);
    }

    #[test]
    fn merging_keeps_every_node_and_edge_once() {
        let mut a = graph(&[("x", "y")]);
        a.merge(graph(&[("x", "y"), ("y", "z")]));

        assert_eq!(a.requires_of(Path::new("x")), [p("y")]);
        assert_eq!(a.requires_of(Path::new("y")), [p("z")]);
        assert_eq!(a.nodes().count(), 3);
    }

    // --- the node key -------------------------------------------------------

    #[test]
    fn an_init_file_keys_on_its_directory() {
        assert_eq!(
            node_of(Path::new("/p/src/pkg/init.luau")),
            Path::new("/p/src/pkg")
        );
        assert_eq!(
            node_of(Path::new("/p/src/pkg/init.lua")),
            Path::new("/p/src/pkg")
        );
    }

    /// Only a plain init file forms a directory module
    #[test]
    fn other_files_key_on_themselves() {
        assert_eq!(
            node_of(Path::new("/p/src/a.luau")),
            Path::new("/p/src/a.luau")
        );
        assert_eq!(
            node_of(Path::new("/p/src/init.client.luau")),
            Path::new("/p/src/init.client.luau")
        );
    }

    // --- opaque files -------------------------------------------------------

    #[test]
    fn an_opaque_file_is_a_node_and_marks_the_graph() {
        let mut g = Graph::default();
        g.see_opaque(Path::new("styled.luau"));

        assert!(g.has_opaque());
        assert_eq!(g.nodes().count(), 1);
    }

    #[test]
    fn merging_carries_the_opaque_mark() {
        let mut a = Graph::default();
        let mut b = Graph::default();
        b.see_opaque(Path::new("styled.luau"));
        a.merge(b);

        assert!(a.has_opaque());
    }

    // --- cycles ------------------------------------------------------------

    #[test]
    fn an_acyclic_graph_has_no_cycles() {
        assert!(graph(&[("a", "b"), ("b", "c")]).cycles().is_empty());
    }

    #[test]
    fn a_two_module_cycle_is_found() {
        let found = graph(&[("a", "b"), ("b", "a")]).cycles();

        assert_eq!(found, vec![vec![p("a"), p("b")]]);
    }

    #[test]
    fn a_longer_cycle_is_reported_as_one_component() {
        let found = graph(&[("a", "b"), ("b", "c"), ("c", "a")]).cycles();

        assert_eq!(found, vec![vec![p("a"), p("b"), p("c")]]);
    }

    /// A back edge search would report a two module cycle inside this one
    #[test]
    fn a_tangle_is_one_component_rather_than_several_pairs() {
        let found = graph(&[("a", "b"), ("b", "a"), ("b", "c"), ("c", "b")]).cycles();

        assert_eq!(found, vec![vec![p("a"), p("b"), p("c")]]);
    }

    #[test]
    fn a_module_requiring_itself_is_a_cycle() {
        assert_eq!(graph(&[("a", "a")]).cycles(), vec![vec![p("a")]]);
    }

    /// Every other component of one is an ordinary module
    #[test]
    fn a_plain_module_is_not_reported_as_a_cycle_of_one() {
        assert!(graph(&[("a", "b")]).cycles().is_empty());
    }

    #[test]
    fn two_separate_cycles_are_both_found() {
        let found = graph(&[("a", "b"), ("b", "a"), ("x", "y"), ("y", "x")]).cycles();

        assert_eq!(found, vec![vec![p("a"), p("b")], vec![p("x"), p("y")]]);
    }

    /// A diamond is not a cycle, and a naive visited check can call it one
    #[test]
    fn a_diamond_is_not_a_cycle() {
        let found = graph(&[("a", "b"), ("a", "c"), ("b", "d"), ("c", "d")]).cycles();

        assert!(found.is_empty(), "{found:?}");
    }

    // --- unused ------------------------------------------------------------

    #[test]
    fn a_module_nothing_requires_is_unused() {
        let g = graph(&[("a", "b")]);

        assert_eq!(g.unused(&[]), [p("a")]);
    }

    #[test]
    fn an_entry_point_is_not_unused() {
        let g = graph(&[("a", "b")]);

        assert!(g.unused(&[p("a")]).is_empty());
    }

    #[test]
    fn an_orphan_with_no_edges_at_all_is_unused() {
        let mut g = graph(&[("a", "b")]);
        g.see(Path::new("orphan"));

        assert_eq!(g.unused(&[p("a")]), [p("orphan")]);
    }

    /// Everything in a cycle has an incoming edge, so nothing there is unused
    #[test]
    fn a_cycle_hides_nothing_from_the_unused_check() {
        let g = graph(&[("a", "b"), ("b", "a")]);

        assert!(g.unused(&[]).is_empty());
    }

    // --- order -------------------------------------------------------------

    #[test]
    fn a_topological_order_puts_dependencies_first() {
        let g = graph(&[("a", "b"), ("b", "c")]);
        let order = g.topological().expect("acyclic");

        let at = |s: &str| order.iter().position(|p| p == Path::new(s)).unwrap();

        assert!(at("c") < at("b"), "{order:?}");
        assert!(at("b") < at("a"), "{order:?}");
    }

    #[test]
    fn a_diamond_orders_the_shared_dependency_first() {
        let g = graph(&[("a", "b"), ("a", "c"), ("b", "d"), ("c", "d")]);
        let order = g.topological().expect("acyclic");

        let at = |s: &str| order.iter().position(|p| p == Path::new(s)).unwrap();

        assert!(at("d") < at("b") && at("d") < at("c"), "{order:?}");
        assert!(at("b") < at("a") && at("c") < at("a"), "{order:?}");
    }

    /// No such order exists, and an invented one reads a nil field at startup
    #[test]
    fn a_cyclic_graph_has_no_topological_order() {
        assert!(graph(&[("a", "b"), ("b", "a")]).topological().is_none());
    }

    #[test]
    fn every_node_appears_exactly_once_in_the_order() {
        let g = graph(&[("a", "b"), ("a", "c"), ("b", "d"), ("c", "d")]);
        let order = g.topological().unwrap();

        assert_eq!(order.len(), 4);

        let mut sorted = order.clone();
        sorted.sort();
        sorted.dedup();

        assert_eq!(sorted.len(), order.len());
    }
}
