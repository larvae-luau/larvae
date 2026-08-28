/*!
`workspace/symbol`: one name, searched over every file of the project.

The request answers from the larvae parser alone. It needs no type
information, so a project without an analyzer still finds its declarations.

The index reuses two parts that already exist. [`structure::symbols`] reads
one file and returns the outline tree, and `commands::fmt::collect` walks the
project. So the index holds exactly the files that `larvae fmt` formats, and
it names a symbol exactly as the outline of its file names it. A second walker
or a second extractor would drift from those two answers.

The index is a flat list. The outline is a tree, and the protocol asks for a
flat result with a container name per entry, so the build flattens the tree
once and the search never walks it again.
*/

use std::path::{Path, PathBuf};

use rayon::prelude::*;

use crate::config::Excludes;

use super::rpc::Lines;
use super::structure;

/// One entry of the answer. The lines are zero based.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub name: String,
    /// A protocol symbol kind, from the constants of [`structure`]
    pub kind: u8,
    pub path: PathBuf,
    /// The first and last line of the whole declaration, the body included.
    pub range: (u32, u32),
    /// The name of the enclosing symbol, or None for a top level one
    pub container: Option<String>,
}

/*
One symbol, with the lowercase name that the search compares against.

The build lowers each name once. The alternative lowers every name on every
keystroke, and a user types a query one character at a time.
*/
#[derive(Debug, Clone)]
struct Entry {
    found: Found,
    lower: String,
}

/*
The quality of a match, best first.

A user who types the start of a name wants that name, so a prefix match
outranks the rest. A run of characters inside a name is still text the user
saw, so it outranks a match whose characters are scattered.
*/
const PREFIX: u8 = 0;
const RUN: u8 = 1;
const SCATTERED: u8 = 2;

/// Every symbol of the project, ready to search.
#[derive(Debug, Clone, Default)]
pub struct Index {
    entries: Vec<Entry>,
    /// Every file the walk covered, for the module auto-imports
    files: Vec<std::path::PathBuf>,
}

impl Index {
    /*
    Reads and parses every file of the project once.

    The walk comes from `larvae fmt`, with the same excludes, so the index and
    the formatter agree on what the project holds. The walk asks for no worm
    extensions: a claimed file is not Luau, and the Luau parser gives nothing
    for it.

    A file that larvae cannot read, and a file that does not parse, add no
    symbols and stop nothing. A broken file is the normal state of the file
    that the user edits.

    The cost is one lex and one parse per file, in parallel. The build holds
    no cache, so the caller decides when to build again.
    */
    pub fn build(root: &Path, excludes: &Excludes) -> Self {
        let mut files = crate::commands::fmt::collect(root, &[], excludes, &[]).unwrap_or_default();
        files.sort();

        let mut entries: Vec<Entry> = files
            .par_iter()
            .flat_map_iter(|path| match std::fs::read_to_string(path) {
                Ok(src) => of_file(path, &src),

                Err(_) => Vec::new(),
            })
            .collect();

        // The walk returns the files in the order of the file system, which varies.
        entries.sort_by(|a, b| {
            (&a.found.path, a.found.range, &a.found.name).cmp(&(
                &b.found.path,
                b.found.range,
                &b.found.name,
            ))
        });

        Self { entries, files }
    }

    /// The files of the last walk, sorted, for the module auto-imports
    pub fn files(&self) -> &[std::path::PathBuf] {
        &self.files
    }

    /// The number of symbols that the index holds
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /*
    The best `limit` symbols for a query, best first.

    An empty query gives nothing. The editor sends one when the user opens the
    symbol picker, before any typing, and a list of every name in the project
    answers no question the user asked.

    The rank is the match quality first, then the length of the name. At equal
    quality the shorter name is the closer fit, because the query covers more
    of it. Name and path break the last tie, so two runs give one order.

    `limit` cuts the list. The protocol allows a partial answer, and the user
    reads the top of a ranked list either way.
    */
    pub fn search(&self, query: &str, limit: usize) -> Vec<Found> {
        if query.is_empty() || limit == 0 {
            return Vec::new();
        }

        let query = query.to_lowercase();

        let mut hits: Vec<(u8, &Entry)> = self
            .entries
            .iter()
            .filter_map(|e| quality(&e.lower, &query).map(|q| (q, e)))
            .collect();

        hits.sort_by(|(qa, a), (qb, b)| {
            (qa, a.found.name.len(), &a.found.name, &a.found.path).cmp(&(
                qb,
                b.found.name.len(),
                &b.found.name,
                &b.found.path,
            ))
        });

        hits.truncate(limit);

        hits.into_iter().map(|(_, e)| e.found.clone()).collect()
    }
}

/*
The symbols of one source text, flat.

The line table is built once here and not once per symbol. The tree gives the
container name for free: the parent of a node is the node that recursed into
it.
*/
fn of_file(path: &Path, src: &str) -> Vec<Entry> {
    let tree = structure::symbols(src);

    if tree.is_empty() {
        return Vec::new();
    }

    let lines = Lines::new(src);
    let mut out = Vec::new();

    flatten(&tree, path, src, &lines, None, &mut out);

    out
}

fn flatten(
    tree: &[structure::Symbol],
    path: &Path,
    src: &str,
    lines: &Lines,
    container: Option<&str>,
    out: &mut Vec<Entry>,
) {
    for symbol in tree {
        let (start, end) = symbol.range;

        // `end` is the byte after the declaration, so the byte before it names the line.
        let last = lines.position(src, end.saturating_sub(1)).0;

        out.push(Entry {
            found: Found {
                name: symbol.name.clone(),
                kind: symbol.kind,
                path: path.to_path_buf(),
                range: (lines.position(src, start).0, last),
                container: container.map(str::to_string),
            },
            lower: symbol.name.to_lowercase(),
        });

        flatten(&symbol.children, path, src, lines, Some(&symbol.name), out);
    }
}

/*
How well a name answers a query, or None when it does not.

Both strings are already lowercase, so the match ignores case. A scattered
match is what an editor user expects from a symbol search: `gpn` finds
`getPlayerName`, and typing three characters beats typing thirteen.
*/
fn quality(name: &str, query: &str) -> Option<u8> {
    match name.find(query) {
        Some(0) => return Some(PREFIX),

        Some(_) => return Some(RUN),

        None => {}
    }

    let mut rest = name.chars();

    for want in query.chars() {
        rest.find(|&c| c == want)?;
    }

    Some(SCATTERED)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A project on disk, one entry per file. A `/` in a name makes a directory.
    fn project(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();

        for (name, src) in files {
            let path = dir.path().join(name);

            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }

            std::fs::write(&path, src).unwrap();
        }

        dir
    }

    fn names(found: &[Found]) -> Vec<&str> {
        found.iter().map(|f| f.name.as_str()).collect()
    }

    fn excludes(root: &Path, patterns: &[&str]) -> Excludes {
        let owned: Vec<String> = patterns.iter().map(|p| p.to_string()).collect();

        Excludes::new(root, &owned).unwrap()
    }

    #[test]
    fn an_empty_query_finds_nothing() {
        let dir = project(&[("a.luau", "local function alpha() end\n")]);
        let index = Index::build(dir.path(), &Excludes::default());

        assert_eq!(index.len(), 1);
        assert!(index.search("", 50).is_empty());
        assert!(index.search("alpha", 0).is_empty());
    }

    #[test]
    fn an_empty_project_answers_nothing() {
        let dir = project(&[]);
        let index = Index::build(dir.path(), &Excludes::default());

        assert!(index.is_empty());
        assert!(index.search("a", 50).is_empty());
    }

    #[test]
    fn every_file_of_a_project_reaches_the_index() {
        let dir = project(&[
            ("a.luau", "local function alpha() end\n"),
            ("src/b.luau", "local function bravo() end\n"),
            ("src/deep/c.lua", "local function charlie() end\n"),
        ]);

        let index = Index::build(dir.path(), &Excludes::default());

        for name in ["alpha", "bravo", "charlie"] {
            assert_eq!(names(&index.search(name, 50)), vec![name]);
        }
    }

    /// A file that does not parse gives nothing, and the other files still answer.
    #[test]
    fn a_broken_file_does_not_stop_the_walk() {
        let dir = project(&[
            ("a.luau", "local function alpha() end\n"),
            ("broken.luau", "local = = function ??? end\n"),
            ("z.luau", "local function zulu() end\n"),
        ]);

        let index = Index::build(dir.path(), &Excludes::default());

        assert_eq!(index.len(), 2);
        assert_eq!(names(&index.search("alpha", 50)), vec!["alpha"]);
        assert_eq!(names(&index.search("zulu", 50)), vec!["zulu"]);
    }

    #[test]
    fn an_excluded_path_contributes_nothing() {
        let dir = project(&[
            ("src/a.luau", "local function alpha() end\n"),
            ("vendor/v.luau", "local function alphaVendor() end\n"),
        ]);

        let index = Index::build(dir.path(), &excludes(dir.path(), &["vendor"]));

        assert_eq!(names(&index.search("alpha", 50)), vec!["alpha"]);
    }

    /// A file that is not Luau never reaches the parser.
    #[test]
    fn another_extension_contributes_nothing() {
        let dir = project(&[
            ("a.luau", "local function alpha() end\n"),
            ("notes.md", "local function alphaNote() end\n"),
        ]);

        let index = Index::build(dir.path(), &Excludes::default());

        assert_eq!(index.len(), 1);
    }

    /*
    The rank that the task names. Both names hold `g`, `p`, and `n` in that
    order and neither holds the run `gpn`, so only the length separates them.
    */
    #[test]
    fn a_shorter_name_wins_at_equal_quality() {
        let dir = project(&[(
            "a.luau",
            "local function regretfullyPlanNothing() end\nlocal function getPlayerName() end\n",
        )]);

        let index = Index::build(dir.path(), &Excludes::default());

        assert_eq!(
            names(&index.search("gpn", 50)),
            vec!["getPlayerName", "regretfullyPlanNothing"]
        );
    }

    #[test]
    fn a_prefix_beats_a_run_beats_a_scattered_match() {
        let dir = project(&[(
            "a.luau",
            "\
local function abc() end
local function xxabcxx() end
local function aXbXc() end
",
        )]);

        let index = Index::build(dir.path(), &Excludes::default());

        assert_eq!(
            names(&index.search("abc", 50)),
            vec!["abc", "xxabcxx", "aXbXc"]
        );
    }

    /*
    Quality outranks length. `xabc` is longer than `aXbXc` and still comes
    first, because its match is one run and the other one is scattered.
    */
    #[test]
    fn quality_outranks_length() {
        let dir = project(&[(
            "a.luau",
            "local function aXbXc() end\nlocal function xabc() end\n",
        )]);

        let index = Index::build(dir.path(), &Excludes::default());

        assert_eq!(names(&index.search("abc", 50)), vec!["xabc", "aXbXc"]);
    }

    #[test]
    fn the_search_ignores_case() {
        let dir = project(&[("a.luau", "local function GetPlayerName() end\n")]);
        let index = Index::build(dir.path(), &Excludes::default());

        for query in ["GPN", "gpn", "getplayer", "GetPlayerName"] {
            assert_eq!(names(&index.search(query, 50)), vec!["GetPlayerName"]);
        }
    }

    #[test]
    fn a_name_that_does_not_match_stays_out() {
        let dir = project(&[("a.luau", "local function alpha() end\n")]);
        let index = Index::build(dir.path(), &Excludes::default());

        // The two characters are in the name, but not in this order.
        assert!(index.search("hp", 50).is_empty());
        assert!(index.search("alphaz", 50).is_empty());
    }

    #[test]
    fn the_limit_caps_the_list() {
        let dir = project(&[(
            "a.luau",
            "local function ab() end\nlocal function abc() end\nlocal function abcd() end\n",
        )]);

        let index = Index::build(dir.path(), &Excludes::default());

        assert_eq!(index.search("ab", 50).len(), 3);
        assert_eq!(names(&index.search("ab", 2)), vec!["ab", "abc"]);
    }

    #[test]
    fn a_nested_symbol_names_its_container() {
        let src = "\
local function outer()
\tlocal function inner() end
end
";
        let dir = project(&[("a.luau", src)]);
        let index = Index::build(dir.path(), &Excludes::default());

        let outer = index.search("outer", 1).remove(0);
        let inner = index.search("inner", 1).remove(0);

        assert_eq!(outer.container, None);
        assert_eq!(inner.container.as_deref(), Some("outer"));
        assert_eq!(inner.kind, structure::FUNCTION);
    }

    #[test]
    fn the_range_holds_the_lines_of_the_declaration() {
        let src = "\
local a = 1

local function outer()
\treturn 2
end
";
        let dir = project(&[("a.luau", src)]);
        let index = Index::build(dir.path(), &Excludes::default());

        let outer = index.search("outer", 1).remove(0);

        assert_eq!(outer.range, (2, 4));
        assert_eq!(outer.path, dir.path().join("a.luau"));

        assert_eq!(index.search("a", 1).remove(0).range, (0, 0));
    }

    #[test]
    #[ignore = "timing, run explicitly"]
    fn what_a_build_costs() {
        let body: String = (0..40)
            .map(|i| format!("local function name{i}(a, b)\n\treturn a + b + {i}\nend\n"))
            .collect();

        let files: Vec<(String, String)> = (0..300)
            .map(|i| (format!("src/d{}/f{i}.luau", i % 20), body.clone()))
            .collect();

        let pairs: Vec<(&str, &str)> = files
            .iter()
            .map(|(a, b)| (a.as_str(), b.as_str()))
            .collect();

        let dir = project(&pairs);

        let at = std::time::Instant::now();
        let index = Index::build(dir.path(), &Excludes::default());
        let build = at.elapsed();

        let at = std::time::Instant::now();
        let hits = index.search("nm7", 100);
        let search = at.elapsed();

        println!(
            "{} symbols over 300 files: build {build:?}, search {search:?}, {} hits",
            index.len(),
            hits.len()
        );
    }

    #[test]
    fn a_method_keeps_the_name_of_its_declaration() {
        let dir = project(&[("a.luau", "local t = {}\nfunction t:reset() end\n")]);
        let index = Index::build(dir.path(), &Excludes::default());

        let found = index.search("t:reset", 1).remove(0);

        assert_eq!(found.name, "t:reset");
        assert_eq!(found.kind, structure::METHOD);
    }
}
