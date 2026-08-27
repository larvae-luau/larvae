/*!
The completion list inside `require("...")`.

The analyzer answers a type question and this one is not: the text between
the quotes names a file, and only the filesystem knows what is there. So the
server answers it from the same rules the resolver holds, and the author sees
the aliases of the project, the directories under them, and the files inside.

Everything is offered, not only Luau. A worm that claims `.json` vendors the
type of a data file, so the require of one is real code. A file that no worm
claims is offered too and resolves to an unsupported path, which is the true
answer and a better one than an empty list.
*/

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::project::luaurc::LuaurcIndex;
use crate::requires::datamodel::MountTable;

/// One offer for the text between the quotes
pub struct Candidate {
    pub label: String,
    /// The protocol's CompletionItemKind: 19 is Folder, 17 is File, 9 is Module
    pub kind: u8,
    /// What the editor writes; a directory keeps the caret going
    pub insert: String,
    pub detail: Option<String>,
}

/*
The half-written spec at the cursor, when the cursor sits in a require.

The text does not parse while it is being typed: `require("./` is an
unterminated string, and the lexer stops there. So the scan is by hand, over
the line the cursor is on, and it tracks one thing: whether a quote is open.

Nothing comes back unless `require` stands right before the string. A string
elsewhere on the line is a string, and an editor that offered file paths
inside every one of them would be noise.
*/
pub fn spec_at(src: &str, at: u32) -> Option<&str> {
    let at = (at as usize).min(src.len());
    let line_start = src[..at].rfind('\n').map(|n| n + 1).unwrap_or(0);
    let head = &src[line_start..at];

    let bytes = head.as_bytes();
    let mut quote: Option<u8> = None;
    let mut opened = 0usize;
    let mut i = 0usize;

    while i < bytes.len() {
        let byte = bytes[i];

        match quote {
            Some(_) if byte == b'\\' => i += 1,

            Some(q) if byte == q => quote = None,

            Some(_) => {}

            None if byte == b'"' || byte == b'\'' => {
                quote = Some(byte);
                opened = i + 1;
            }

            None => {}
        }

        i += 1;
    }

    quote?;

    // `require` and `require (`, and nothing else, open a spec.
    let before = head[..opened.saturating_sub(1)].trim_end();
    let before = before.strip_suffix('(').unwrap_or(before).trim_end();

    before.ends_with("require").then(|| &head[opened..])
}

/*
What a half-written spec can become.

`base` is where the path stands so far, and the last segment is what the
author is typing; the editor filters the list by it, so the list holds every
name of the directory and not only the ones that match.
*/
pub fn candidates(
    partial: &str,
    file: &Path,
    root: &Path,
    toml_aliases: &HashMap<String, String>,
    luaurc: &LuaurcIndex,
    mounts: &MountTable,
    claimed: &[String],
) -> Vec<Candidate> {
    let (head, _) = match partial.rfind('/') {
        Some(at) => (&partial[..=at], &partial[at + 1..]),

        // Nothing is settled yet, so the answer is the roots a spec can take.
        None => return roots(partial, file, toml_aliases, luaurc, mounts),
    };

    let Some(base) = base_of(head, file, root, toml_aliases, luaurc, mounts) else {
        return Vec::new();
    };

    entries(&base, claimed)
}

/*
The forms a spec can start with: the aliases, and the two relative marks.

An alias of `larvae.toml` and an alias of a `.luaurc` are both offered, and
the project file wins where both define a name, which is the rule the
resolver holds.
*/
fn roots(
    partial: &str,
    file: &Path,
    toml_aliases: &HashMap<String, String>,
    luaurc: &LuaurcIndex,
    mounts: &MountTable,
) -> Vec<Candidate> {
    let mut out = Vec::new();

    // The relative forms read against the file, so they need no lookup.
    if !partial.starts_with('@') {
        for (label, detail) in [
            ("./", "a file beside this one"),
            ("../", "a file beside the directory of this one"),
        ] {
            out.push(Candidate {
                label: label.to_owned(),
                kind: 19,
                insert: label.to_owned(),
                detail: Some(detail.to_owned()),
            });
        }
    }

    out.push(Candidate {
        label: "@self/".to_owned(),
        kind: 19,
        insert: "@self/".to_owned(),
        detail: Some("a file beside this one".to_owned()),
    });

    if !mounts.is_empty() {
        out.push(Candidate {
            label: "@game/".to_owned(),
            kind: 19,
            insert: "@game/".to_owned(),
            detail: Some("a path from the DataModel".to_owned()),
        });
    }

    let mut named: Vec<(String, String)> = Vec::new();

    for (name, value) in toml_aliases {
        named.push((name.to_lowercase(), value.clone()));
    }

    let dir = file.parent().unwrap_or(file);

    for name in luaurc.names(dir) {
        if named.iter().any(|(seen, _)| *seen == name) {
            continue;
        }

        let Some((value, _)) = luaurc.lookup(dir, &name) else {
            continue;
        };

        named.push((name, value.to_owned()));
    }

    named.sort();

    for (name, value) in named {
        out.push(Candidate {
            label: format!("@{name}/"),
            kind: 19,
            insert: format!("@{name}/"),
            detail: Some(value),
        });
    }

    out
}

/*
The directory that a settled part of a spec names.

The rules are the resolver's own, so a completion cannot offer a path that a
require of the same text would not find. An init file resolves `./` from the
directory above its own, which is the one case the two spellings differ.
*/
fn base_of(
    head: &str,
    file: &Path,
    root: &Path,
    toml_aliases: &HashMap<String, String>,
    luaurc: &LuaurcIndex,
    mounts: &MountTable,
) -> Option<PathBuf> {
    let own_dir = file.parent()?;

    let is_init = file
        .file_stem()
        .is_some_and(|s| s == "init" || s == "init.server" || s == "init.client");

    let dot_base = match is_init {
        true => own_dir.parent().unwrap_or(own_dir),

        false => own_dir,
    };

    if let Some(rest) = head.strip_prefix("@self/") {
        return Some(own_dir.join(rest));
    }

    if head.starts_with("./") || head.starts_with("../") {
        return Some(dot_base.join(head));
    }

    let rest = head.strip_prefix('@')?;
    let (alias, tail) = rest.split_once('/')?;
    let alias = alias.to_lowercase();

    /*
    `@game` is a path from the DataModel, and the mount table is what turns
    one into a directory. A project with no rojo project and no mounts has
    no DataModel, so nothing is offered, which is the true answer.
    */
    if alias == "game" && !toml_aliases.contains_key("game") {
        let segments: Vec<String> = tail
            .trim_end_matches('/')
            .split('/')
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect();

        return mounts.fs_of(&segments);
    }

    /*
    An alias of `larvae.toml` reads against the root of the project. The
    `.luaurc` rule differs: a value there reads against the directory of the
    file that wrote it, and the index hands that directory back beside it.
    */
    let base = match toml_aliases.get(&alias) {
        Some(value) => root.join(value),

        None => {
            let (value, dir) = luaurc.lookup(own_dir, &alias)?;

            dir.join(value)
        }
    };

    Some(base.join(tail))
}

/*
Every name a directory offers, as the text a require would carry.

A Luau file is offered by its stem, because the extension is not part of the
spec. Every other file is offered whole: that is what a worm's resolver
matches, and a stem would be a guess. A stem that two files share falls back
to the whole name for both, so the list never offers one text for two files.
*/
fn entries(dir: &Path, claimed: &[String]) -> Vec<Candidate> {
    let Ok(read) = std::fs::read_dir(dir) else {
        return Vec::new();
    };

    let mut files: Vec<(String, String, u8)> = Vec::new();
    let mut out: Vec<Candidate> = Vec::new();

    for entry in read.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();

        if name.starts_with('.') {
            continue;
        }

        let path = entry.path();

        if path.is_dir() {
            out.push(Candidate {
                label: format!("{name}/"),
                kind: 19,
                insert: format!("{name}/"),
                detail: None,
            });

            // A directory with an init file is a module of its own name too.
            if ["init.luau", "init.lua"]
                .iter()
                .any(|init| path.join(init).is_file())
            {
                out.push(Candidate {
                    label: name.clone(),
                    kind: 9,
                    insert: name,
                    detail: Some("the init file of the directory".to_owned()),
                });
            }

            continue;
        }

        let ext = path
            .extension()
            .map(|e| e.to_string_lossy().into_owned())
            .unwrap_or_default();

        let luau = ext == "luau" || ext == "lua";

        // An init file is the directory it sits in, and never a name of its own.
        if luau && name.starts_with("init.") {
            continue;
        }

        let label = match luau || claimed.contains(&ext) {
            true => path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| name.clone()),

            false => name.clone(),
        };

        files.push((label, name, if luau { 9 } else { 17 }));
    }

    for (label, name, kind) in &files {
        // A stem that two files share names neither of them.
        let shared = files.iter().filter(|(other, ..)| other == label).count() > 1;

        let label = match shared {
            true => name.clone(),

            false => label.clone(),
        };

        out.push(Candidate {
            label: label.clone(),
            kind: *kind,
            insert: label,
            detail: Some(name.clone()),
        });
    }

    out
}

#[cfg(test)]
mod cursor {
    use super::spec_at;

    /// The offset of `|` in the marked text, and the text without it
    fn marked(text: &str) -> (String, u32) {
        let at = text.find('|').expect("a cursor");

        (text.replace('|', ""), at as u32)
    }

    fn spec(text: &str) -> Option<String> {
        let (src, at) = marked(text);

        spec_at(&src, at).map(str::to_owned)
    }

    #[test]
    fn an_open_require_string_is_a_spec() {
        assert_eq!(spec("local a = require(\"|"), Some(String::new()));
        assert_eq!(spec("local a = require(\"./he|"), Some("./he".to_owned()));
        assert_eq!(
            spec("local a = require('@pkg/|')"),
            Some("@pkg/".to_owned())
        );
    }

    /// A require with no parentheses is the same call, and Luau allows it.
    #[test]
    fn the_form_without_parentheses_counts() {
        assert_eq!(spec("local a = require \"./x|\""), Some("./x".to_owned()));
    }

    /// A string that no require opens is a string.
    #[test]
    fn another_string_on_the_line_is_not_a_spec() {
        assert_eq!(spec("local a = \"hello|\""), None);
        assert_eq!(spec("print(\"./sr|\")"), None);
    }

    /// Outside the quotes there is nothing to complete.
    #[test]
    fn a_closed_string_is_not_a_spec() {
        assert_eq!(spec("local a = require(\"./x\")|"), None);
        assert_eq!(spec("local a = require(\"./x\"|)"), None);
    }

    /// The scan reads the line of the cursor, and not the line above it.
    #[test]
    fn a_require_on_an_earlier_line_does_not_carry() {
        assert_eq!(spec("local a = require(\"./x\")\nlocal b = \"|"), None);
    }
}

#[cfg(test)]
mod listing {
    use super::*;

    fn tree(files: &[&str]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("a temp dir");

        for file in files {
            let path = dir.path().join(file);
            std::fs::create_dir_all(path.parent().expect("a parent")).expect("makes it");
            std::fs::write(&path, "return {}\n").expect("writes");
        }

        dir
    }

    fn labels(candidates: &[Candidate]) -> Vec<String> {
        let mut out: Vec<String> = candidates.iter().map(|c| c.label.clone()).collect();
        out.sort();

        out
    }

    fn empty() -> (HashMap<String, String>, LuaurcIndex, MountTable) {
        (
            HashMap::new(),
            LuaurcIndex::new(Path::new("/")),
            MountTable::default(),
        )
    }

    /// A relative spec lists the directory of the file that writes it.
    #[test]
    fn a_relative_spec_lists_the_neighbours() {
        let dir = tree(&[
            "src/a.luau",
            "src/helper.luau",
            "src/data.json",
            "src/sub/b.luau",
        ]);
        let (toml, luaurc, mounts) = empty();

        let found = candidates(
            "./",
            &dir.path().join("src/a.luau"),
            dir.path(),
            &toml,
            &luaurc,
            &mounts,
            &["json".to_owned()],
        );

        assert_eq!(labels(&found), ["a", "data", "helper", "sub/"]);
    }

    /*
    A file no worm claims is offered whole.

    The require of it reports an unsupported path, which is the answer that
    file deserves. An empty list would say the file is not there.
    */
    #[test]
    fn an_unclaimed_file_is_offered_with_its_extension() {
        let dir = tree(&["src/a.luau", "src/notes.txt"]);
        let (toml, luaurc, mounts) = empty();

        let found = candidates(
            "./",
            &dir.path().join("src/a.luau"),
            dir.path(),
            &toml,
            &luaurc,
            &mounts,
            &[],
        );

        assert_eq!(labels(&found), ["a", "notes.txt"]);
    }

    /// Two files of one stem are offered whole, or one text would name both.
    #[test]
    fn a_shared_stem_falls_back_to_the_whole_name() {
        let dir = tree(&["src/a.luau", "src/thing.luau", "src/thing.json"]);
        let (toml, luaurc, mounts) = empty();

        let found = candidates(
            "./",
            &dir.path().join("src/a.luau"),
            dir.path(),
            &toml,
            &luaurc,
            &mounts,
            &["json".to_owned()],
        );

        assert_eq!(labels(&found), ["a", "thing.json", "thing.luau"]);
    }

    /// An init file reads `./` from the directory above its own.
    #[test]
    fn an_init_file_lists_the_directory_above() {
        let dir = tree(&["src/pkg/init.luau", "src/other.luau"]);
        let (toml, luaurc, mounts) = empty();

        let found = candidates(
            "./",
            &dir.path().join("src/pkg/init.luau"),
            dir.path(),
            &toml,
            &luaurc,
            &mounts,
            &[],
        );

        assert_eq!(labels(&found), ["other", "pkg", "pkg/"]);
    }

    /// An alias of a `.luaurc` reads against the directory that defined it.
    #[test]
    fn a_luaurc_alias_lists_its_target() {
        let dir = tree(&["src/a.luau", "packages/thing.luau"]);
        std::fs::write(
            dir.path().join(".luaurc"),
            r#"{ "aliases": { "pkg": "packages" } }"#,
        )
        .expect("writes");

        let mut luaurc = LuaurcIndex::new(dir.path());
        luaurc
            .add_file(&dir.path().join(".luaurc"))
            .expect("parses");

        let found = candidates(
            "@pkg/",
            &dir.path().join("src/a.luau"),
            dir.path(),
            &HashMap::new(),
            &luaurc,
            &MountTable::default(),
            &[],
        );

        assert_eq!(labels(&found), ["thing"]);
    }

    /// With nothing typed, the answer is the forms a spec can take.
    #[test]
    fn an_empty_spec_offers_the_aliases() {
        let dir = tree(&["src/a.luau"]);
        std::fs::write(
            dir.path().join(".luaurc"),
            r#"{ "aliases": { "pkg": "packages", "ui": "src/ui" } }"#,
        )
        .expect("writes");

        let mut luaurc = LuaurcIndex::new(dir.path());
        luaurc
            .add_file(&dir.path().join(".luaurc"))
            .expect("parses");

        let found = candidates(
            "",
            &dir.path().join("src/a.luau"),
            dir.path(),
            &HashMap::new(),
            &luaurc,
            &MountTable::default(),
            &[],
        );

        assert_eq!(labels(&found), ["../", "./", "@pkg/", "@self/", "@ui/"]);
    }
}
