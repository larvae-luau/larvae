/*!
Requires follow a rename, behind a question.

The editor says a file or a folder moved, after the move. Every require
that named the old place is now wrong, and nothing on disk can resolve it
any more, so the scan here matches specs by spelling: the spec's path
arithmetic against the old path, no filesystem asked. What matches is
rewritten in the form it was written in, a relative spec staying relative
and an alias staying under its alias.

The rewrite waits for the user. A rename is sometimes a move of one file
and sometimes a refactor half done, so the server asks with one dialog,
and the edit applies on the answer and never before. luau-lsp rewrites on
its own; the question is larvae's difference, asked for by name.
*/

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use super::rpc::{self, Lines};
use super::uri::{path_of_uri, uri_of_path};
use crate::project::luaurc::LuaurcIndex;

/// The question in flight: the edit to apply if the user says yes.
pub(super) struct Pending {
    pub id: Value,
    pub edit: Value,
}

/// The answer that applies the edit; the other button leaves the files.
pub(super) const UPDATE: &str = "Update requires";

/// What the server declares for `workspace/didRenameFiles`.
pub(super) fn capabilities() -> Value {
    json!({
        "didRename": {
            "filters": [
                { "pattern": { "glob": "**/*", "matches": "file" } },
                { "pattern": { "glob": "**", "matches": "folder" } },
            ]
        }
    })
}

/// The extensions a require can address, with and without writing them.
const EXTENSIONS: [&str; 5] = ["luau", "lua", "luaux", "json", "toml"];

/*
A spec's path, resolved by arithmetic alone.

`..` folds and `.` drops, and nothing asks the disk: the old path is gone,
so a lookup would answer that the require points nowhere, which is exactly
the state this module exists to repair.
*/
fn lexical(base: &Path, rest: &str) -> PathBuf {
    let mut out = base.to_path_buf();

    for part in rest.split('/') {
        match part {
            "" | "." => {}

            ".." => {
                out.pop();
            }

            name => out.push(name),
        }
    }

    out
}

/// Where an alias points, from the project table first and `.luaurc` second.
fn alias_base(
    name: &str,
    requirer: &Path,
    root: &Path,
    aliases: &std::collections::HashMap<String, String>,
    luaurc: &LuaurcIndex,
) -> Option<PathBuf> {
    if let Some(value) = aliases.get(name) {
        return Some(root.join(value));
    }

    let dir = requirer.parent().unwrap_or(requirer);
    let (value, from) = luaurc.lookup(dir, name)?;

    Some(from.join(value))
}

/// The abstract file a spec addresses, in the world before the rename.
fn addressed(
    spec: &str,
    requirer: &Path,
    root: &Path,
    aliases: &std::collections::HashMap<String, String>,
    luaurc: &LuaurcIndex,
) -> Option<PathBuf> {
    let dir = requirer.parent().unwrap_or(requirer);

    if let Some(rest) = spec.strip_prefix("@self/") {
        return Some(lexical(dir, rest));
    }

    if spec.starts_with("./") || spec.starts_with("../") {
        return Some(lexical(dir, spec));
    }

    let named = spec.strip_prefix('@')?;
    let (name, rest) = named.split_once('/').unwrap_or((named, ""));

    if name == "game" {
        // The DataModel form matches by its own segments, not by a path.
        return None;
    }

    Some(lexical(
        &alias_base(name, requirer, root, aliases, luaurc)?,
        rest,
    ))
}

/*
Whether an addressed path names the renamed one, and where it now points.

A spec may write the extension or leave it off, and a folder rename moves
everything under it, so the match tries the spelling as written, then each
extension a require can omit, then the folder prefix.
*/
fn moved(addressed: &Path, old: &Path, new: &Path) -> Option<PathBuf> {
    if addressed == old {
        return Some(new.to_path_buf());
    }

    for ext in EXTENSIONS {
        let mut with = addressed.as_os_str().to_owned();
        with.push(".");
        with.push(ext);

        if Path::new(&with) == old {
            return Some(new.to_path_buf());
        }
    }

    addressed.strip_prefix(old).ok().map(|kept| new.join(kept))
}

/// A path as a spec segment string, forward slashes whatever the platform.
pub(super) fn segments(path: &Path) -> String {
    path.components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// The relative spelling from one file to a target, `./` and `../` included.
pub(super) fn relative_spec(requirer: &Path, target: &Path) -> String {
    let dir = requirer.parent().unwrap_or(requirer);
    let mut up = 0usize;
    let mut base = dir.to_path_buf();

    while !target.starts_with(&base) {
        if !base.pop() {
            return segments(target);
        }

        up += 1;
    }

    let rest = segments(target.strip_prefix(&base).unwrap_or(target));

    match up {
        0 => format!("./{rest}"),

        n => format!("{}{rest}", "../".repeat(n)),
    }
}

/*
The moved target, spelled in the form the author wrote.

The extension follows the old spelling: a spec that wrote one keeps the
new file's own, and a spec that left it off stays bare. A form that can
no longer say the new place, ex: `@self/` for a file that left the
directory, falls back to the relative form, which can say anywhere.
*/
fn respelled(
    spec: &str,
    requirer: &Path,
    target: &Path,
    root: &Path,
    aliases: &std::collections::HashMap<String, String>,
    luaurc: &LuaurcIndex,
) -> String {
    let wrote_extension = Path::new(spec)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| EXTENSIONS.contains(&e));

    let spoken = match wrote_extension {
        true => target.to_path_buf(),

        false => target.with_extension(""),
    };

    let dir = requirer.parent().unwrap_or(requirer);

    if spec.starts_with("@self/")
        && let Ok(kept) = spoken.strip_prefix(dir)
    {
        return format!("@self/{}", segments(kept));
    }

    if let Some(named) = spec.strip_prefix('@')
        && !spec.starts_with("@self/")
    {
        let (name, _) = named.split_once('/').unwrap_or((named, ""));

        if let Some(base) = alias_base(name, requirer, root, aliases, luaurc)
            && let Ok(kept) = spoken.strip_prefix(&base)
        {
            return format!("@{name}/{}", segments(kept));
        }
    }

    relative_spec(requirer, &spoken)
}

/// One rewrite: where in the file, and what the spec becomes.
struct Rewrite {
    inner: (u32, u32),
    text: String,
}

/// Every require of `src` that named a renamed path, rewritten.
fn rewrites(
    src: &str,
    path: &Path,
    pairs: &[(PathBuf, PathBuf)],
    root: &Path,
    aliases: &std::collections::HashMap<String, String>,
    luaurc: &LuaurcIndex,
) -> Vec<Rewrite> {
    let Ok(lexed) = crate::syntax::lexer::lex(src) else {
        return Vec::new();
    };

    let scanned = crate::syntax::scan::scan(src, &lexed.toks);
    let mut out = Vec::new();

    for site in &scanned.sites {
        let spec = &src[site.inner_start as usize..site.inner_end as usize];

        let Some(target) = addressed(spec, path, root, aliases, luaurc) else {
            continue;
        };

        for (old, new) in pairs {
            let Some(now) = moved(&target, old, new) else {
                continue;
            };

            out.push(Rewrite {
                inner: (site.inner_start, site.inner_end),
                text: respelled(spec, path, &now, root, aliases, luaurc),
            });

            break;
        }
    }

    out
}

impl super::Server {
    /*
    The editor renamed files; ask before the requires follow.

    The scan is the project walk the symbol index already does, filtered
    first by the old name's stem, so a rename touches the files that could
    mention it and lexes nothing else.
    */
    pub(super) fn on_did_rename(
        &mut self,
        params: &Value,
        out: &mut impl std::io::Write,
    ) -> anyhow::Result<()> {
        let Some(root) = self.root.clone() else {
            return Ok(());
        };

        let pairs: Vec<(PathBuf, PathBuf)> = params["files"]
            .as_array()
            .map(|files| {
                files
                    .iter()
                    .filter_map(|f| {
                        Some((
                            path_of_uri(f["oldUri"].as_str()?)?,
                            path_of_uri(f["newUri"].as_str()?)?,
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default();

        if pairs.is_empty() {
            return Ok(());
        }

        let stems: Vec<String> = pairs
            .iter()
            .filter_map(|(old, _)| Some(old.file_stem()?.to_string_lossy().into_owned()))
            .map(|stem| {
                stem.trim_end_matches(".server")
                    .trim_end_matches(".client")
                    .to_string()
            })
            .collect();

        let files =
            crate::commands::fmt::collect(&root, &[], &self.excluded, &[]).unwrap_or_default();

        let mut changes = serde_json::Map::new();
        let mut sites = 0usize;

        for file in files {
            let uri = match uri_of_path(&file) {
                Some(uri) => uri,

                None => continue,
            };

            // The open buffer is the truth the editor sees; disk covers the rest.
            let text = match self.documents.get(&uri) {
                Some(open) => open.clone(),

                None => match std::fs::read_to_string(&file) {
                    Ok(text) => text,

                    Err(_) => continue,
                },
            };

            if !stems.iter().any(|stem| text.contains(stem.as_str())) {
                continue;
            }

            let luaurc = super::decorate::luaurc_upward(&file, &root);
            let found = rewrites(&text, &file, &pairs, &root, &self.aliases, &luaurc);

            if found.is_empty() {
                continue;
            }

            let lines = Lines::new(&text);
            sites += found.len();

            let edits: Vec<Value> = found
                .into_iter()
                .map(|r| {
                    json!({
                        "range": lines.range(&text, r.inner),
                        "newText": r.text,
                    })
                })
                .collect();

            changes.insert(uri, json!(edits));
        }

        if sites == 0 {
            return Ok(());
        }

        let (old, new) = &pairs[0];
        let message = format!(
            "{} is now {}: update {} require{} in {} file{}?",
            old.file_name().unwrap_or_default().to_string_lossy(),
            new.file_name().unwrap_or_default().to_string_lossy(),
            sites,
            if sites == 1 { "" } else { "s" },
            changes.len(),
            if changes.len() == 1 { "" } else { "s" },
        );

        let id = rpc::ask(
            out,
            "window/showMessageRequest",
            json!({
                "type": 3,
                "message": message,
                "actions": [{ "title": UPDATE }, { "title": "Leave them" }],
            }),
        )?;

        self.pending_rename = Some(Pending {
            id,
            edit: json!({ "changes": Value::Object(changes) }),
        });

        Ok(())
    }

    /// The dialog answered; the edit applies on the one button that says so.
    pub(super) fn on_reply(
        &mut self,
        id: &Value,
        result: &Value,
        out: &mut impl std::io::Write,
    ) -> anyhow::Result<()> {
        let Some(pending) = self.pending_rename.take() else {
            return Ok(());
        };

        if pending.id != *id {
            self.pending_rename = Some(pending);

            return Ok(());
        }

        if result["title"] == UPDATE {
            rpc::request(out, "workspace/applyEdit", json!({ "edit": pending.edit }))?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_aliases() -> std::collections::HashMap<String, String> {
        std::collections::HashMap::new()
    }

    #[test]
    fn a_relative_spec_follows_the_file() {
        let luaurc = LuaurcIndex::new(Path::new("/p"));
        let requirer = Path::new("/p/src/a.luau");

        let target = addressed("./util", requirer, Path::new("/p"), &no_aliases(), &luaurc)
            .expect("addresses");

        assert_eq!(target, Path::new("/p/src/util"));

        let now = moved(
            &target,
            Path::new("/p/src/util.luau"),
            Path::new("/p/src/tools.luau"),
        )
        .expect("the rename covers it");

        assert_eq!(
            respelled(
                "./util",
                requirer,
                &now,
                Path::new("/p"),
                &no_aliases(),
                &luaurc
            ),
            "./tools"
        );
    }

    #[test]
    fn a_folder_rename_carries_everything_under_it() {
        let luaurc = LuaurcIndex::new(Path::new("/p"));
        let requirer = Path::new("/p/src/a.luau");

        let target = addressed(
            "../lib/deep/thing.luau",
            Path::new("/p/src/sub/b.luau"),
            Path::new("/p"),
            &no_aliases(),
            &luaurc,
        )
        .expect("addresses");

        let now = moved(&target, Path::new("/p/src/lib"), Path::new("/p/src/vendor"))
            .expect("the folder rename covers it");

        assert_eq!(now, Path::new("/p/src/vendor/deep/thing.luau"));

        assert_eq!(
            respelled(
                "../lib/deep/thing.luau",
                requirer,
                &now,
                Path::new("/p"),
                &no_aliases(),
                &luaurc,
            ),
            "./vendor/deep/thing.luau"
        );

        let _ = requirer;
    }

    #[test]
    fn an_alias_spec_stays_under_its_alias() {
        let mut aliases = no_aliases();
        aliases.insert("shared".into(), "src/shared".into());

        let luaurc = LuaurcIndex::new(Path::new("/p"));
        let requirer = Path::new("/p/src/client/a.luau");

        let target = addressed("@shared/util", requirer, Path::new("/p"), &aliases, &luaurc)
            .expect("addresses");

        let now = moved(
            &target,
            Path::new("/p/src/shared/util.luau"),
            Path::new("/p/src/shared/tools.luau"),
        )
        .expect("covered");

        assert_eq!(
            respelled(
                "@shared/util",
                requirer,
                &now,
                Path::new("/p"),
                &aliases,
                &luaurc
            ),
            "@shared/tools"
        );
    }

    /// A move out of the alias falls back to the form that can say anywhere.
    #[test]
    fn a_move_out_of_the_alias_goes_relative() {
        let mut aliases = no_aliases();
        aliases.insert("shared".into(), "src/shared".into());

        let luaurc = LuaurcIndex::new(Path::new("/p"));
        let requirer = Path::new("/p/src/client/a.luau");

        assert_eq!(
            respelled(
                "@shared/util",
                requirer,
                Path::new("/p/src/client/util.luau"),
                Path::new("/p"),
                &aliases,
                &luaurc,
            ),
            "./util"
        );
    }

    /// The spelled extension survives, and only the spelled one.
    #[test]
    fn the_extension_follows_the_old_spelling() {
        let luaurc = LuaurcIndex::new(Path::new("/p"));
        let requirer = Path::new("/p/src/a.luau");

        assert_eq!(
            respelled(
                "./data.json",
                requirer,
                Path::new("/p/src/items.json"),
                Path::new("/p"),
                &no_aliases(),
                &luaurc,
            ),
            "./items.json"
        );
    }
}
