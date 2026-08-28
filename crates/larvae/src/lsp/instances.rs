/*!
The instance tree of a rojo sourcemap, as Luau types.

A Roblox script reaches its neighbours through `script`: `script.Providers`,
`script.Parent.Config`. Nothing in the language says what those are. The
sourcemap does, because it maps every file to a place in the DataModel and
names the children of each place.

So the tree becomes a declaration per node, and each file learns the name of
its own. The server binds `script` to that name per module, which is the one
thing a global declaration cannot do: `script` means a different instance in
every file.

Each declaration also names its parent, so `script.Parent` carries the type
of the folder above and not the bare `Instance` that `Parent` gives every
instance. That is the half that makes a sibling reachable.

A file that a worm claims is added on top of what rojo wrote. rojo maps the
extensions it knows, so a `.luaux` beside a `.luau` is missing from the
sourcemap and the script beside it cannot see it. The build turns that file
into a module of the place, so the tree says so too.

This is the same shape as the Studio bridge in `studio.rs` and a different
source. The bridge mirrors a live place; this reads what rojo wrote. A
project usually has one or the other, and either answers the same question.
*/

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct Node {
    name: String,
    #[serde(default, rename = "className")]
    class_name: String,
    #[serde(default, rename = "filePaths")]
    file_paths: Vec<PathBuf>,
    #[serde(default)]
    children: Vec<Node>,
}

/// The declaration text for a tree, and the type each file's `script` takes
#[derive(Debug, Default)]
pub struct Instances {
    pub definitions: String,
    /// Absolute file path, to the name of the type that describes its instance
    pub script_types: HashMap<PathBuf, String>,
    /*
    The class behind each declared name, for the text a reader sees.

    Every node needs a name of its own, because two folders called `Modules`
    under different parents are different types. Those names are made up and
    a reader has no use for them, so a card that would say
    `_larvae_sourcemap_1_176` says `LocalScript` instead.
    */
    classes: HashMap<String, String>,
}

impl Instances {
    pub fn is_empty(&self) -> bool {
        self.definitions.is_empty()
    }

    /*
    One rendered type, with the made-up names put back as classes.

    Anything the analyzer renders can carry them: a hover card, an inlay
    hint, a signature, the message of a diagnostic. So the rewrite happens
    at the edge, on the text, and every route through it reads the same.
    */
    pub fn readable<'a>(&self, text: &'a str) -> std::borrow::Cow<'a, str> {
        if self.classes.is_empty() || !text.contains(PREFIX) {
            return std::borrow::Cow::Borrowed(text);
        }

        let mut out = String::with_capacity(text.len());
        let mut rest = text;

        while let Some(at) = rest.find(PREFIX) {
            out.push_str(&rest[..at]);

            let tail = &rest[at..];
            let end = tail
                .find(|c: char| !c.is_ascii_alphanumeric() && c != '_')
                .unwrap_or(tail.len());

            match self.classes.get(&tail[..end]) {
                Some(class) => out.push_str(class),

                None => out.push_str(&tail[..end]),
            }

            rest = &tail[end..];
        }

        out.push_str(rest);

        std::borrow::Cow::Owned(collapse_repeats(&out))
    }
}

/*
A union or intersection with one spelling repeated says the name once.

Two different tree nodes are two different types to Luau, and a union of
them is honest. Substituting the class names above can erase the
difference: both spell `Folder`, and the reader gets `Folder | Folder`,
which says nothing twice. The walk keeps a chain's first spelling of
each member and drops the repeats, at every bracket depth, and it never
looks inside a string, so a singleton like `"a | a"` keeps its bytes.
*/
fn collapse_repeats(text: &str) -> String {
    // Edge whitespace survives the walk; the walk works on the middle.
    let trimmed = text.trim_matches(' ');

    if trimmed != text {
        let lead = &text[..text.len() - text.trim_start_matches(' ').len()];
        let trail = &text[text.trim_end_matches(' ').len()..];

        return format!("{lead}{}{trail}", collapse_repeats(trimmed));
    }

    // A list first: each element or field collapses on its own.
    if let Some(parts) = split_separated(text) {
        return parts
            .into_iter()
            .map(|(sep, part)| format!("{sep}{}", collapse_repeats(part)))
            .collect();
    }

    // A field or a binding: the name stays, the type after the colon walks.
    if let Some(at) = top_level(text, ": ") {
        return format!("{}: {}", &text[..at], collapse_repeats(&text[at + 2..]));
    }

    // A function type: the arguments and the returns walk separately.
    if let Some(at) = top_level(text, " -> ") {
        return format!(
            "{} -> {}",
            collapse_repeats(&text[..at]),
            collapse_repeats(&text[at + 4..])
        );
    }

    let pieces = split_top(text, &['|', '&']);

    if pieces.len() > 1 {
        let mut seen: Vec<String> = Vec::new();
        let mut out = String::new();

        for (op, piece) in &pieces {
            let inner = collapse_repeats(piece.trim());

            if seen.contains(&inner) {
                continue;
            }

            if !out.is_empty() {
                out.push(' ');
                out.push(*op);
                out.push(' ');
            }

            out.push_str(&inner);
            seen.push(inner);
        }

        return out;
    }

    // No chain here: descend into each bracket group and rebuild around it.
    let mut out = String::new();
    let mut rest = text;

    while let Some(open) = rest.find(['(', '{', '<']) {
        let Some(close) = matching(rest, open) else {
            out.push_str(rest);

            return out;
        };

        out.push_str(&rest[..=open]);
        out.push_str(&collapse_repeats(&rest[open + 1..close]));
        out.push_str(&rest[close..=close]);
        rest = &rest[close + 1..];
    }

    out.push_str(rest);

    out
}

/*
The list elements of `text`, split at the top-level commas, semicolons,
and newlines, each keeping the separator text before it. `None` when the
text is one element, so the caller does not recurse forever.
*/
fn split_separated(text: &str) -> Option<Vec<(&str, &str)>> {
    let mut cuts = Vec::new();
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    let mut previous = ' ';

    for (i, c) in text.char_indices() {
        if let Some(q) = quote {
            if c == q && previous != '\\' {
                quote = None;
            }

            previous = c;

            continue;
        }

        match c {
            '"' | '\'' | '`' => quote = Some(c),

            '(' | '{' | '<' => depth += 1,

            ')' | '}' => depth -= 1,

            '>' if previous != '-' => depth -= 1,

            ',' | ';' | '\n' if depth == 0 => cuts.push(i),

            _ => {}
        }

        previous = c;
    }

    if cuts.is_empty() {
        return None;
    }

    let mut parts = Vec::new();
    let mut start = 0usize;

    for cut in cuts {
        parts.push((&text[start..=cut], ""));
        start = cut + 1;
    }

    parts.push((&text[start..], ""));

    // The separator travels with the piece before it; rebuild as (lead, body).
    Some(
        parts
            .into_iter()
            .map(|(chunk, _)| {
                let body_end = chunk
                    .char_indices()
                    .rev()
                    .find(|(_, c)| matches!(c, ',' | ';' | '\n'))
                    .map(|(i, _)| i);

                match body_end {
                    Some(i) => (&chunk[..i], &chunk[i..]),

                    None => (chunk, ""),
                }
            })
            .map(|(body, sep)| ("", body, sep))
            .map(|(_, body, sep)| (body, sep))
            .collect::<Vec<_>>()
            .into_iter()
            .map(|(body, sep)| (sep, body))
            .collect(),
    )
}

/// Where `needle` first appears at the top depth, quotes skipped.
fn top_level(text: &str, needle: &str) -> Option<usize> {
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    let mut previous = ' ';

    for (i, c) in text.char_indices() {
        if let Some(q) = quote {
            if c == q && previous != '\\' {
                quote = None;
            }

            previous = c;

            continue;
        }

        match c {
            '"' | '\'' | '`' => quote = Some(c),

            '(' | '{' | '<' => depth += 1,

            ')' | '}' => depth -= 1,

            '>' if previous != '-' => depth -= 1,

            _ if depth == 0 && text[i..].starts_with(needle) => return Some(i),

            _ => {}
        }

        previous = c;
    }

    None
}

/*
The pieces of one operator chain at the top depth, with the operator
before each piece. One kind of operator per chain: Luau parenthesizes a
mixed one, so a mix at one depth is not a chain and stays whole.
*/
fn split_top<'a>(text: &'a str, ops: &[char]) -> Vec<(char, &'a str)> {
    let mut pieces = Vec::new();
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    let mut start = 0usize;
    let mut kind = ' ';

    let bytes: Vec<char> = text.chars().collect();
    let mut byte_at = 0usize;

    for (i, c) in bytes.iter().enumerate() {
        let here = byte_at;
        byte_at += c.len_utf8();

        if let Some(q) = quote {
            if *c == q && (i == 0 || bytes[i - 1] != '\\') {
                quote = None;
            }

            continue;
        }

        match c {
            '"' | '\'' | '`' => quote = Some(*c),

            '(' | '{' => depth += 1,

            ')' | '}' => depth -= 1,

            // `<` opens a generic; the `>` of an arrow closes nothing.
            '<' => depth += 1,

            '>' if i > 0 && bytes[i - 1] != '-' => depth -= 1,

            '|' | '&' if depth == 0 => {
                if kind == ' ' {
                    kind = *c;
                } else if kind != *c {
                    // A mixed chain at one depth is not one chain.
                    return Vec::new();
                }

                pieces.push((*c, &text[start..here]));
                start = here + 1;
            }

            _ => {}
        }
    }

    let _ = ops;

    if pieces.is_empty() {
        return Vec::new();
    }

    pieces.push((kind, &text[start..]));

    // The first piece carries no operator of its own; give it the chain's.
    pieces[0].0 = kind;

    pieces
}

/// The index of the bracket that closes the one at `open`, quotes skipped.
fn matching(text: &str, open: usize) -> Option<usize> {
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    let mut previous = ' ';

    for (i, c) in text.char_indices().skip_while(|(i, _)| *i < open) {
        if let Some(q) = quote {
            if c == q && previous != '\\' {
                quote = None;
            }

            previous = c;

            continue;
        }

        match c {
            '"' | '\'' | '`' => quote = Some(c),

            '(' | '{' | '<' => depth += 1,

            ')' | '}' => {
                depth -= 1;

                if depth == 0 {
                    return Some(i);
                }
            }

            '>' if previous != '-' => {
                depth -= 1;

                if depth == 0 {
                    return Some(i);
                }
            }

            _ => {}
        }

        previous = c;
    }

    None
}

/// What every generated type name starts with, so the rewrite can find one
const PREFIX: &str = "_larvae_sourcemap_";

/// One node of the tree, with its place in the flat list beside it
struct Flat {
    class_name: String,
    parent: Option<usize>,
    /// The children that can be written as a field, by name
    fields: BTreeMap<String, usize>,
    /// The directory this node stands for, where the tree says what it is
    dir: Option<PathBuf>,
}

/*
Read a sourcemap and turn it into types.

A missing or unreadable file is not an error worth a message. A project
without rojo has no sourcemap and wants none, and one whose sourcemap is
half written is mid-build.

`generation` separates one read from the next. A reload declares the same
tree again, and a type name the global scope already holds is a redefinition
error, so every read spells its names with a number of its own.
*/
pub fn read(path: &Path, root: &Path, generation: u64, claimed: &[String]) -> Instances {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Instances::default();
    };

    let Ok(tree) = serde_json::from_str::<Node>(&text) else {
        return Instances::default();
    };

    let mut out = Instances::default();
    let mut nodes: Vec<Flat> = Vec::new();

    let walk = Walk {
        root,
        generation,
        claimed,
    };

    flatten(&tree, None, &walk, &mut nodes, &mut out, 0);
    add_claimed(&mut nodes, &mut out, generation, claimed);

    let mut text = String::from("-- The rojo sourcemap, as types. Do not edit.\n\n");

    /*
    Order does not matter here. Luau reads a definition file in two passes:
    it declares every extern type first, then fills the members in. So a
    field can name a type that the file declares further down, which is what
    a parent and a child naming each other needs.
    */
    for (index, node) in nodes.iter().enumerate() {
        let base = match usable_class(&node.class_name) {
            true => node.class_name.as_str(),

            false => "Instance",
        };

        text.push_str(&format!(
            "declare extern type {} extends {base} with\n",
            type_name(generation, index)
        ));

        if let Some(parent) = node.parent {
            text.push_str(&format!("\tParent: {}\n", type_name(generation, parent)));
        }

        for (field, child) in &node.fields {
            text.push_str(&format!("\t{field}: {}\n", type_name(generation, *child)));
        }

        text.push_str("end\n");
    }

    /*
    The root of a sourcemap is the DataModel, so `game` takes its type and
    the absolute spelling of a path resolves too. Studio's mirror declares
    the same name from a live place, and a project that runs both gets
    whichever spoke last, which is the one the user is looking at.
    */
    if nodes
        .first()
        .is_some_and(|n| usable_class(&n.class_name) && n.class_name == "DataModel")
    {
        text.push_str(&format!("\ndeclare game: {}\n", type_name(generation, 0)));
    }

    out.definitions = text;

    out
}

/*
Walk the tree into the flat list, and give each file the name of its node.

The walk is pre-order, so a parent takes a lower index than its children and
the root is index zero. Nothing depends on the order beyond that, because the
two-pass load lets a declaration name a type that comes later.
*/
/// What the walk holds the whole way down, and never changes
struct Walk<'a> {
    root: &'a Path,
    generation: u64,
    /// The extensions the worms of the project claim, without the dot
    claimed: &'a [String],
}

fn flatten(
    node: &Node,
    parent: Option<usize>,
    walk: &Walk,
    nodes: &mut Vec<Flat>,
    out: &mut Instances,
    depth: usize,
) -> Option<usize> {
    // A tree this deep is a cycle or a mistake, and no place needs it.
    if depth > 64 {
        return None;
    }

    let index = nodes.len();

    nodes.push(Flat {
        class_name: node.class_name.clone(),
        parent,
        fields: BTreeMap::new(),
        dir: own_dir(node, walk.root),
    });

    /*
    Every file the node came from learns this name. A node carries more than
    one path when rojo joins a folder and its init file, and both of them are
    the same instance.
    */
    /*
    A script is Luau, or a file a worm claims. larvae names no worm's
    extension of its own: the pool says which ones are scripts here, and a
    project with no worm has only Luau.
    */
    for file in &node.file_paths {
        let script = file
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|ext| {
                ext == "luau"
                    || ext == "lua"
                    || walk
                        .claimed
                        .iter()
                        .any(|c| c.trim_start_matches('.') == ext)
            });

        if script {
            out.script_types
                .insert(walk.root.join(file), type_name(walk.generation, index));
        }
    }

    out.classes.insert(
        type_name(walk.generation, index),
        match usable_class(&node.class_name) {
            true => node.class_name.clone(),

            false => "Instance".to_owned(),
        },
    );

    let mut seen: HashSet<&str> = HashSet::new();

    for child in &node.children {
        let Some(child_index) = flatten(child, Some(index), walk, nodes, out, depth + 1) else {
            continue;
        };

        if !plain(&child.name) || TAKEN.contains(&child.name.as_str()) {
            continue;
        }

        // The first child of a name wins, as it does when Roblox resolves one.
        if seen.insert(&child.name) {
            nodes[index].fields.insert(child.name.clone(), child_index);
        }
    }

    Some(index)
}

/*
The directory a node stands for, when the sourcemap says which one.

A folder with an init file names its own directory. A folder rojo made from
a directory carries no path of its own, and the paths of its children say
where it is. A plain script is a file and stands for no directory.
*/
fn own_dir(node: &Node, root: &Path) -> Option<PathBuf> {
    for file in &node.file_paths {
        let is_init = file
            .file_stem()
            .and_then(|s| s.to_str())
            .is_some_and(|stem| stem.starts_with("init"));

        if is_init {
            return root.join(file).parent().map(Path::to_path_buf);
        }
    }

    if !node.file_paths.is_empty() {
        return None;
    }

    node.children
        .iter()
        .flat_map(|child| &child.file_paths)
        .find_map(|file| root.join(file).parent().map(Path::to_path_buf))
}

/*
Add the files of the directory that rojo does not map.

rojo writes the extensions it knows. A worm teaches larvae another one, and
the build turns such a file into a module of the place, so the tree has to
hold it or the script beside it cannot reach its own neighbour.

A name the sourcemap already carries wins. rojo is the authority on what it
mapped, and this only ever adds.
*/
fn add_claimed(nodes: &mut Vec<Flat>, out: &mut Instances, generation: u64, claimed: &[String]) {
    if claimed.is_empty() {
        return;
    }

    for index in 0..nodes.len() {
        let Some(dir) = nodes[index].dir.clone() else {
            continue;
        };

        let Ok(read) = std::fs::read_dir(&dir) else {
            continue;
        };

        let mut found: Vec<(String, PathBuf)> = Vec::new();

        for entry in read.flatten() {
            let path = entry.path();

            let claims = path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|ext| claimed.iter().any(|c| c.trim_start_matches('.') == ext));

            if !claims || !path.is_file() {
                continue;
            }

            let Some(name) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };

            if !plain(name) || TAKEN.contains(&name) || nodes[index].fields.contains_key(name) {
                continue;
            }

            found.push((name.to_owned(), path));
        }

        // The order of a directory read is the filesystem's, and a tree is not.
        found.sort();

        for (name, path) in found {
            let child = nodes.len();

            nodes.push(Flat {
                // The build makes a module of it, whatever the worm reads.
                class_name: "ModuleScript".to_owned(),
                parent: Some(index),
                fields: BTreeMap::new(),
                dir: None,
            });

            out.script_types.insert(path, type_name(generation, child));
            out.classes
                .insert(type_name(generation, child), "ModuleScript".to_owned());

            nodes[index].fields.insert(name, child);
        }
    }
}

/*
The name of the type for one node.

The index is what makes it unique, because two folders called `Modules` under
different parents are different instances. The generation separates one read
of the sourcemap from the next.
*/
fn type_name(generation: u64, index: usize) -> String {
    format!("_larvae_sourcemap_{generation}_{index}")
}

/*
Whether a class name can stand as the base of the declaration.

A class Roblox added after the vendored types were built is the case this
answers: the child is still reachable, it just carries the type every
instance has.
*/
fn usable_class(name: &str) -> bool {
    plain(name) && name.starts_with(|c: char| c.is_ascii_uppercase())
}

/// Whether a name can be written as a Luau field without quoting
fn plain(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with(|c: char| c.is_ascii_digit())
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !RESERVED.contains(&name)
}

const RESERVED: &[&str] = &[
    "and", "break", "do", "else", "elseif", "end", "false", "for", "function", "if", "in", "local",
    "nil", "not", "or", "repeat", "return", "then", "true", "until", "while", "type", "export",
    "continue",
];

/*
Every name that reaches a script through `Instance` already.

A child called `Name` or `ClassName` would shadow the property the language
gives every instance, so `script.Name` would stop meaning the name. The child
is left out rather than allowed to break the ones that matter.

`Parent` is not on this list because the declaration writes it itself, from
the tree, and a child of that name is dropped where the field is filled.
*/
const TAKEN: &[&str] = &[
    "Name",
    "Parent",
    "ClassName",
    "Archivable",
    "Changed",
    "AncestryChanged",
    "ChildAdded",
    "ChildRemoved",
    "Destroying",
    "AttributeChanged",
    "GetChildren",
    "GetDescendants",
    "FindFirstChild",
    "FindFirstChildOfClass",
    "FindFirstChildWhichIsA",
    "FindFirstAncestor",
    "FindFirstAncestorOfClass",
    "FindFirstAncestorWhichIsA",
    "WaitForChild",
    "IsA",
    "IsAncestorOf",
    "IsDescendantOf",
    "Destroy",
    "Clone",
    "ClearAllChildren",
    "GetAttribute",
    "SetAttribute",
    "GetAttributes",
    "GetFullName",
    "GetPropertyChangedSignal",
    "GetAttributeChangedSignal",
    "GetTags",
    "HasTag",
    "AddTag",
    "RemoveTag",
];

#[cfg(test)]
mod collapse {
    use super::collapse_repeats;

    #[test]
    fn a_repeated_member_says_its_name_once() {
        assert_eq!(collapse_repeats("Folder | Folder"), "Folder");
        assert_eq!(collapse_repeats("A | B | A"), "A | B");
        assert_eq!(collapse_repeats("R15 & R15"), "R15");
        assert_eq!(collapse_repeats("Folder | Part"), "Folder | Part");
    }

    #[test]
    fn the_walk_descends_into_brackets() {
        assert_eq!(
            collapse_repeats("{ x: Folder | Folder }"),
            "{ x: Folder | Folder }".replace("Folder | Folder", "Folder")
        );
        assert_eq!(
            collapse_repeats("(Folder | Folder) -> ()"),
            "(Folder) -> ()"
        );
    }

    #[test]
    fn a_string_keeps_its_bytes() {
        assert_eq!(collapse_repeats("\"a | a\""), "\"a | a\"");
        assert_eq!(
            collapse_repeats("kind: \"x | x\" | \"x | x\""),
            "kind: \"x | x\""
        );
    }

    #[test]
    fn an_arrow_is_not_a_close_bracket() {
        assert_eq!(
            collapse_repeats("(a: number) -> Folder | Folder"),
            "(a: number) -> Folder"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, json: &str) -> PathBuf {
        let path = dir.join("sourcemap.json");
        std::fs::write(&path, json).expect("writes");

        path
    }

    /*
    A script learns the type of its own instance, and that type holds its
    children. This is what makes `script.Providers` resolve.
    */
    #[test]
    fn a_script_gets_a_type_that_holds_its_children() {
        let dir = tempfile::tempdir().expect("a temp dir");

        let path = write(
            dir.path(),
            r#"{"name":"game","className":"DataModel","children":[
                 {"name":"Client","className":"LocalScript","filePaths":["src/client/init.client.luau"],
                  "children":[{"name":"Providers","className":"Folder"}]}]}"#,
        );

        let read = read(&path, dir.path(), 1, &[]);
        let script = dir.path().join("src/client/init.client.luau");

        let name = read.script_types.get(&script).expect("the file has a type");

        assert!(
            read.definitions
                .contains(&format!("declare extern type {name} extends LocalScript")),
            "{}",
            read.definitions
        );
        assert!(
            read.definitions.contains("Providers:"),
            "{}",
            read.definitions
        );
    }

    /*
    A node names its parent, so `script.Parent.Sibling` resolves.

    Without this, `Parent` carries the type that `Instance` gives it, and a
    sibling is unreachable from the file beside it. That is the case the
    whole module exists for.
    */
    #[test]
    fn a_node_names_its_parent() {
        let dir = tempfile::tempdir().expect("a temp dir");

        let path = write(
            dir.path(),
            r#"{"name":"game","className":"DataModel","children":[
                 {"name":"Folder","className":"Folder","children":[
                   {"name":"Script","className":"ModuleScript","filePaths":["src/a.luau"]},
                   {"name":"Sibling","className":"ModuleScript","filePaths":["src/b.luau"]}]}]}"#,
        );

        let read = read(&path, dir.path(), 1, &[]);
        let script = read
            .script_types
            .get(&dir.path().join("src/a.luau"))
            .expect("the file has a type");

        // The declaration of the script names the folder above it.
        let block = read
            .definitions
            .split("declare extern type ")
            .find(|b| b.starts_with(script.as_str()))
            .expect("the block");

        assert!(
            block.contains("\tParent: _larvae_sourcemap_1_1\n"),
            "{block}"
        );

        // And the folder holds both children, so the sibling is reachable.
        let folder = read
            .definitions
            .split("declare extern type ")
            .find(|b| b.starts_with("_larvae_sourcemap_1_1 "))
            .expect("the folder block");

        assert!(folder.contains("Script:"), "{folder}");
        assert!(folder.contains("Sibling:"), "{folder}");
    }

    /// A DataModel root types `game`, so an absolute path resolves too.
    #[test]
    fn a_datamodel_root_types_game() {
        let dir = tempfile::tempdir().expect("a temp dir");

        let path = write(
            dir.path(),
            r#"{"name":"game","className":"DataModel","children":[
                 {"name":"ReplicatedStorage","className":"ReplicatedStorage"}]}"#,
        );

        let read = read(&path, dir.path(), 2, &[]);

        assert!(
            read.definitions
                .contains("declare game: _larvae_sourcemap_2_0"),
            "{}",
            read.definitions
        );
    }

    /// A second read spells different names, or the scope holds the first ones.
    #[test]
    fn a_reload_does_not_redeclare_a_name() {
        let dir = tempfile::tempdir().expect("a temp dir");

        let path = write(
            dir.path(),
            r#"{"name":"game","className":"DataModel","children":[]}"#,
        );

        let first = read(&path, dir.path(), 1, &[]);
        let second = read(&path, dir.path(), 2, &[]);

        assert!(first.definitions.contains("_larvae_sourcemap_1_0"));
        assert!(second.definitions.contains("_larvae_sourcemap_2_0"));
        assert!(!second.definitions.contains("_larvae_sourcemap_1_0"));
    }

    /*
    A child that would shadow something every instance has is left out.

    `script.Name` has to keep meaning the name, and a folder called `Name`
    would take that away.
    */
    #[test]
    fn a_child_that_shadows_an_instance_member_is_left_out() {
        let dir = tempfile::tempdir().expect("a temp dir");

        let path = write(
            dir.path(),
            r#"{"name":"game","className":"DataModel","children":[
                 {"name":"Name","className":"Folder"},
                 {"name":"Real","className":"Folder"}]}"#,
        );

        let read = read(&path, dir.path(), 1, &[]);

        assert!(read.definitions.contains("Real:"), "{}", read.definitions);
        assert!(!read.definitions.contains("Name:"), "{}", read.definitions);
    }

    /// A name Luau cannot write as a field is left out rather than written broken.
    #[test]
    fn a_name_that_is_not_an_identifier_is_left_out() {
        let dir = tempfile::tempdir().expect("a temp dir");

        let path = write(
            dir.path(),
            r#"{"name":"game","className":"DataModel","children":[
                 {"name":"My Folder","className":"Folder"},
                 {"name":"2Fast","className":"Folder"},
                 {"name":"end","className":"Folder"},
                 {"name":"Good","className":"Folder"}]}"#,
        );

        let read = read(&path, dir.path(), 1, &[]);

        assert!(read.definitions.contains("Good:"), "{}", read.definitions);
        assert!(
            !read.definitions.contains("My Folder"),
            "{}",
            read.definitions
        );
        assert!(!read.definitions.contains("2Fast"), "{}", read.definitions);
    }

    /// A missing sourcemap is the common case for a project without rojo.
    #[test]
    fn a_missing_sourcemap_is_not_an_error() {
        let dir = tempfile::tempdir().expect("a temp dir");

        assert!(read(&dir.path().join("nope.json"), dir.path(), 1, &[]).is_empty());
    }

    /// A class this build has no type for still gives a reachable instance.
    #[test]
    fn an_unknown_class_falls_back_to_instance() {
        let dir = tempfile::tempdir().expect("a temp dir");

        let path = write(
            dir.path(),
            r#"{"name":"game","className":"lowercase_thing","children":[]}"#,
        );

        let read = read(&path, dir.path(), 1, &[]);

        assert!(
            read.definitions.contains("extends Instance"),
            "{}",
            read.definitions
        );
    }

    /*
    A card reads as the class, and not as the name the generator made up.

    The names exist so two folders of one name stay two types. A reader has
    no use for them, and a hover that showed one would be worse than the
    bare `Instance` this replaced.
    */
    #[test]
    fn a_rendered_type_reads_as_its_class() {
        let dir = tempfile::tempdir().expect("a temp dir");

        let path = write(
            dir.path(),
            r#"{"name":"game","className":"DataModel","children":[
                 {"name":"Client","className":"LocalScript","filePaths":["src/a.luau"]}]}"#,
        );

        let read = read(&path, dir.path(), 1, &[]);

        assert_eq!(read.readable("_larvae_sourcemap_1_1"), "LocalScript");
        assert_eq!(
            read.readable("Key 'X' not found in _larvae_sourcemap_1_0."),
            "Key 'X' not found in DataModel."
        );
        // A name from another read is left alone rather than guessed at.
        assert_eq!(
            read.readable("_larvae_sourcemap_9_9"),
            "_larvae_sourcemap_9_9"
        );
        assert_eq!(read.readable("local x: number"), "local x: number");
    }

    /*
    A file rojo does not map is added from the directory beside it.

    rojo writes the extensions it knows, so a `.luaux` is missing from the
    sourcemap and the script beside it cannot reach its own neighbour. The
    build makes a module of that file, so the tree says so too.
    */
    #[test]
    fn a_worm_claimed_file_joins_the_tree() {
        let dir = tempfile::tempdir().expect("a temp dir");

        std::fs::create_dir_all(dir.path().join("src/ui")).expect("makes it");

        for file in [
            "src/ui/init.luau",
            "src/ui/Panel.luaux",
            "src/ui/Button.luau",
        ] {
            std::fs::write(dir.path().join(file), "return {}\n").expect("writes");
        }

        let path = write(
            dir.path(),
            r#"{"name":"game","className":"DataModel","children":[
                 {"name":"ui","className":"ModuleScript","filePaths":["src/ui/init.luau"],
                  "children":[{"name":"Button","className":"ModuleScript",
                               "filePaths":["src/ui/Button.luau"]}]}]}"#,
        );

        let read = read(&path, dir.path(), 1, &[".luaux".to_owned()]);

        // The folder now names both, and rojo only knew one of them.
        let block = read
            .definitions
            .split("declare extern type ")
            .find(|b| b.starts_with("_larvae_sourcemap_1_1 "))
            .expect("the ui block");

        assert!(block.contains("Button:"), "{block}");
        assert!(block.contains("Panel:"), "{block}");

        // And the claimed file learns what its own `script` is.
        assert!(
            read.script_types
                .contains_key(&dir.path().join("src/ui/Panel.luaux")),
            "{:?}",
            read.script_types
        );
    }

    /// With no worm claiming anything, the tree is what rojo wrote.
    #[test]
    fn nothing_is_added_without_a_claim() {
        let dir = tempfile::tempdir().expect("a temp dir");

        std::fs::create_dir_all(dir.path().join("src/ui")).expect("makes it");

        for file in ["src/ui/init.luau", "src/ui/Panel.luaux"] {
            std::fs::write(dir.path().join(file), "return {}\n").expect("writes");
        }

        let path = write(
            dir.path(),
            r#"{"name":"game","className":"DataModel","children":[
                 {"name":"ui","className":"ModuleScript","filePaths":["src/ui/init.luau"]}]}"#,
        );

        assert!(
            !read(&path, dir.path(), 1, &[])
                .definitions
                .contains("Panel:"),
            "a file no worm claims is not a module"
        );
    }

    /// A file a worm claims is a script too, and it gets the same binding.
    #[test]
    fn a_claimed_file_gets_a_script_type() {
        let dir = tempfile::tempdir().expect("a temp dir");

        let path = write(
            dir.path(),
            r#"{"name":"game","className":"DataModel","children":[
                 {"name":"Panel","className":"ModuleScript","filePaths":["src/Panel.luaux"]}]}"#,
        );

        let read = read(&path, dir.path(), 1, &[".luaux".to_owned()]);

        assert!(
            read.script_types
                .contains_key(&dir.path().join("src/Panel.luaux")),
            "{:?}",
            read.script_types
        );
    }
}
