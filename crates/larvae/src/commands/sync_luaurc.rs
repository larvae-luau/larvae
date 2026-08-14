/*!
`larvae self sync-luaurc` writes the project aliases into the `.luaurc` that
an editor reads.

A `@game/...` alias value is a DataModel path, and luau-lsp cannot follow
one. A verbatim copy would cost the editor types and go-to-definition on
every require through that alias. So the command inverts the mount table: the
DataModel path goes back to the directory that mounts there, and `.luaurc`
gets a path that the editor can resolve. Rewritten `@game` requires exist
only under the output directory, which nobody edits. So source keeps its
`@pkg` forms, and this file backs them.

The command rewrites only the `aliases` key. Other keys survive. Comments do
not survive, because larvae reads `.luaurc` as JSON with comments and writes
it back as plain JSON.
*/

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use serde_json::{Map, Value};

use crate::commands::process::load_config;
use crate::config::Config;
use crate::pipeline::setup;
use crate::project::rojo;
use crate::requires::datamodel::{MountTable, parse_game_path};
use crate::ui;

/// The result of one run. The exit code follows from it.
#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    /// The config has no `[aliases]`, so the command has nothing to write
    Empty,
    UpToDate,
    Wrote(usize),
    /// `--check` found work and did not do it
    WouldChange,
}

pub fn run(root: &Path, check: bool, config: Option<PathBuf>) -> Result<ExitCode> {
    let path = root.join(".luaurc");

    match sync(root, check, config)? {
        Outcome::Empty => {
            ui::print_success("no [aliases] to sync, .luaurc left alone");

            Ok(ExitCode::SUCCESS)
        }

        Outcome::UpToDate => {
            ui::print_success(&format!("{} is already in sync", ui::rel(&path)));

            Ok(ExitCode::SUCCESS)
        }

        Outcome::Wrote(n) => {
            ui::print_success(&format!("synced {n} aliases into {}", ui::rel(&path)));

            Ok(ExitCode::SUCCESS)
        }

        Outcome::WouldChange => {
            println!("would update {}", ui::rel(&path));

            Ok(ExitCode::FAILURE)
        }
    }
}

/*
The whole job, without the reporting.

The split gives `--check` and the tests an answer rather than an exit code.
The function also touches the file only when its bytes change. So a synced
project does not trigger the watchers that read `.luaurc`.
*/
fn sync(root: &Path, check: bool, config: Option<PathBuf>) -> Result<Outcome> {
    let cfg = load_config(root, config, None)?;

    // The mounts hold absolute paths, so the root must be absolute too.
    let root = root
        .canonicalize()
        .with_context(|| format!("cannot resolve project root {}", root.display()))?;

    let aliases = resolve(&root, &cfg)?;

    if aliases.is_empty() {
        return Ok(Outcome::Empty);
    }

    let path = root.join(".luaurc");

    let current = match path.exists() {
        true => Some(
            std::fs::read_to_string(&path)
                .with_context(|| format!("failed to read {}", ui::rel(&path)))?,
        ),

        false => None,
    };

    let next = merge(current.as_deref(), &aliases)
        .with_context(|| format!("failed to update {}", ui::rel(&path)))?;

    if current.as_deref() == Some(next.as_str()) {
        return Ok(Outcome::UpToDate);
    }

    if check {
        return Ok(Outcome::WouldChange);
    }

    std::fs::write(&path, &next).with_context(|| format!("failed to write {}", ui::rel(&path)))?;

    Ok(Outcome::Wrote(aliases.len()))
}

/*
Returns every alias as `.luaurc` must carry it, sorted so two runs agree.

The inversion goes through the same mount table that the require rewriter
builds, from the Rojo project file and `[requires.mounts]` together. An alias
that resolves to one directory in the editor and to another during a build is
the exact failure that this command exists to prevent. So the command must
not build a mount table of its own.
*/
fn resolve(root: &Path, config: &Config) -> Result<Vec<(String, String)>> {
    /*
    A project file that does not parse is a fatal error here. In the pipeline
    the same fault degrades into a diagnostic, and the build carries on. Here
    a broken project leaves the mount table without its mounts, and a missing
    mount does not fail loudly. It silently becomes "this alias has no
    preimage".
    */
    let project = rojo::find_project(root, config.rojo.project.as_deref())
        .map(|p| rojo::load(&p))
        .transpose()?;

    let mut diags = Vec::new();
    let mounts = setup::mount_table(root, config, project.as_ref(), &mut diags);

    for d in &diags {
        eprintln!("{}", d.render(ui::want_color_stderr()));
    }

    let mut out = Vec::new();

    for (name, value) in &config.aliases {
        out.push((name.clone(), path_value(root, name, value, &mounts)?));
    }

    out.sort();

    Ok(out)
}

/// Returns one alias value. The function inverts a DataModel path and keeps any other value.
fn path_value(root: &Path, name: &str, value: &str, mounts: &MountTable) -> Result<String> {
    let Some(segments) = parse_game_path(value) else {
        // The value is already a path, which is what the editor needs.
        return Ok(value.to_owned());
    };

    let fs = mounts.fs_of(&segments).with_context(|| {
        format!(
            "alias \"@{name}\" = \"{value}\" has no filesystem preimage, nothing mounts at that DataModel path. \
             Mount it in the Rojo project file or name it in [requires.mounts]"
        )
    })?;

    let rel = fs.strip_prefix(root).with_context(|| {
        format!(
            "alias \"@{name}\" = \"{value}\" mounts {}, which is outside the project root",
            fs.display()
        )
    })?;

    Ok(slashed(rel))
}

/// A `.luaurc` path is relative to the file's own directory, with forward slashes on every platform
fn slashed(rel: &Path) -> String {
    let joined = rel
        .components()
        .filter_map(|c| c.as_os_str().to_str())
        .collect::<Vec<_>>()
        .join("/");

    match joined.is_empty() {
        // The mount is the project root itself.
        true => "./".to_string(),

        false => format!("./{joined}"),
    }
}

/*
Lays the alias table over what `.luaurc` already says.

The merge goes per key and not wholesale, because `[aliases]` in larvae.toml
also merges over `.luaurc` per key. A replaced table would delete aliases
that larvae still honors, and the editor would stop resolving requires that
build without errors. Names match case insensitively, because Luau resolves
them that way. So a config `Pkg` updates an existing `pkg` and does not sit
beside it as a second entry that nothing can tell apart.
*/
fn merge(current: Option<&str>, aliases: &[(String, String)]) -> Result<String> {
    let mut doc = match current {
        // `.luaurc` is JSON with comments, and json5 accepts a superset of that.
        Some(text) => json5::from_str::<Value>(text).context("not valid JSON")?,

        None => Value::Object(Map::new()),
    };

    let Some(obj) = doc.as_object_mut() else {
        bail!("the file is not a JSON object");
    };

    let table = obj
        .entry("aliases")
        .or_insert_with(|| Value::Object(Map::new()));

    let Some(table) = table.as_object_mut() else {
        bail!("the \"aliases\" key is not a JSON object");
    };

    for (name, value) in aliases {
        let key = table
            .keys()
            .find(|k| k.trim_start_matches('@').eq_ignore_ascii_case(name))
            .cloned()
            .unwrap_or_else(|| name.clone());

        table.insert(key, Value::String(value.clone()));
    }

    let mut text = serde_json::to_string_pretty(&doc)?;
    text.push('\n');

    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROJECT: &str = r#"{
        "name": "game",
        "tree": {
            "$className": "DataModel",
            "ReplicatedStorage": {
                "shared": { "$path": "src/shared" },
                "Packages": { "$path": "Packages" }
            },

            "ServerScriptService": { "$path": "src/server" }
        }
    }"#;

    fn write(root: &Path, rel: &str, text: &str) {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    /// Returns the alias table as it landed in `.luaurc`
    fn aliases_of(root: &Path) -> Map<String, Value> {
        let text = std::fs::read_to_string(root.join(".luaurc")).unwrap();
        let doc: Value = json5::from_str(&text).unwrap();

        doc.get("aliases").unwrap().as_object().unwrap().clone()
    }

    #[test]
    fn a_game_alias_is_inverted_through_the_rojo_mount() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        write(root, "default.project.json", PROJECT);
        write(
            root,
            "larvae.toml",
            "[aliases]\npkg = \"@game/ReplicatedStorage/Packages\"\n",
        );
        std::fs::create_dir_all(root.join("Packages")).unwrap();

        assert_eq!(sync(root, false, None).unwrap(), Outcome::Wrote(1));
        assert_eq!(aliases_of(root)["pkg"], "./Packages");
    }

    /// A mount that ends above the alias must still resolve the segments below it
    #[test]
    fn a_game_alias_below_a_mount_keeps_the_remaining_segments() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        write(root, "default.project.json", PROJECT);
        write(
            root,
            "larvae.toml",
            "[aliases]\nui = \"@game/ReplicatedStorage/shared/ui\"\n",
        );

        sync(root, false, None).unwrap();

        assert_eq!(aliases_of(root)["ui"], "./src/shared/ui");
    }

    #[test]
    fn a_mount_named_in_the_config_inverts_without_a_project_file() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        write(
            root,
            "larvae.toml",
            "[aliases]\npkg = \"@game/ReplicatedStorage/Packages\"\n\
             \n[requires.mounts]\n\"vendor/pkgs\" = \"@game/ReplicatedStorage/Packages\"\n",
        );

        sync(root, false, None).unwrap();

        assert_eq!(aliases_of(root)["pkg"], "./vendor/pkgs");
    }

    #[test]
    fn a_filesystem_alias_passes_through_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        write(root, "default.project.json", PROJECT);
        write(
            root,
            "larvae.toml",
            "[aliases]\nshared = \"./src/shared\"\nnm = \"node_modules/sig\"\n",
        );

        sync(root, false, None).unwrap();

        let aliases = aliases_of(root);
        assert_eq!(aliases["shared"], "./src/shared");
        assert_eq!(aliases["nm"], "node_modules/sig");
    }

    /// `@gameplay` is not `@game`. An inversion would look up a service that does not exist.
    #[test]
    fn a_value_that_only_starts_like_a_game_path_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        write(root, "default.project.json", PROJECT);
        write(root, "larvae.toml", "[aliases]\ngp = \"@gameplay/thing\"\n");

        sync(root, false, None).unwrap();

        assert_eq!(aliases_of(root)["gp"], "@gameplay/thing");
    }

    #[test]
    fn an_alias_with_no_filesystem_preimage_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        write(root, "default.project.json", PROJECT);
        write(
            root,
            "larvae.toml",
            "[aliases]\nghost = \"@game/ServerStorage/nowhere\"\n",
        );

        let err = format!("{:#}", sync(root, false, None).unwrap_err());

        assert!(err.contains("@ghost"), "{err}");
        assert!(err.contains("no filesystem preimage"), "{err}");
        assert!(
            !root.join(".luaurc").exists(),
            "nothing should have been written"
        );
    }

    #[test]
    fn other_luaurc_keys_and_aliases_survive_the_sync() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        write(root, "default.project.json", PROJECT);
        write(
            root,
            "larvae.toml",
            "[aliases]\npkg = \"@game/ReplicatedStorage/Packages\"\n",
        );
        write(
            root,
            ".luaurc",
            r#"{
                // hand written, and only the alias table is ours
                "languageMode": "strict",
                "lint": { "*": true },
                "aliases": { "local": "./src/local" }
            }"#,
        );

        sync(root, false, None).unwrap();

        let text = std::fs::read_to_string(root.join(".luaurc")).unwrap();
        let doc: Value = json5::from_str(&text).unwrap();

        assert_eq!(doc["languageMode"], "strict");
        assert_eq!(doc["lint"]["*"], true);
        // The config does not name this alias, so the sync must not delete it.
        assert_eq!(doc["aliases"]["local"], "./src/local");
        assert_eq!(doc["aliases"]["pkg"], "./Packages");
    }

    /// Luau resolves alias names case insensitively. Two entries that differ
    /// only in case are one alias with two answers.
    #[test]
    fn a_differently_cased_alias_updates_the_existing_key() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        write(root, "default.project.json", PROJECT);
        write(
            root,
            "larvae.toml",
            "[aliases]\nPkg = \"@game/ReplicatedStorage/Packages\"\n",
        );
        write(root, ".luaurc", r#"{ "aliases": { "pkg": "./stale" } }"#);

        sync(root, false, None).unwrap();

        let aliases = aliases_of(root);
        assert_eq!(aliases["pkg"], "./Packages");
        assert_eq!(aliases.len(), 1, "{aliases:?}");
    }

    #[test]
    fn check_writes_nothing_and_reports_that_it_would_change() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        write(root, "default.project.json", PROJECT);
        write(
            root,
            "larvae.toml",
            "[aliases]\npkg = \"@game/ReplicatedStorage/Packages\"\n",
        );
        write(root, ".luaurc", "{ \"aliases\": { \"pkg\": \"./stale\" } }");

        assert_eq!(sync(root, true, None).unwrap(), Outcome::WouldChange);
        assert_eq!(
            std::fs::read_to_string(root.join(".luaurc")).unwrap(),
            "{ \"aliases\": { \"pkg\": \"./stale\" } }"
        );
    }

    /// A second run must change nothing, or `--check` fails on a tree that just synced
    #[test]
    fn a_synced_project_is_up_to_date_under_check() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        write(root, "default.project.json", PROJECT);
        write(
            root,
            "larvae.toml",
            "[aliases]\npkg = \"@game/ReplicatedStorage/Packages\"\nsrv = \"@game/ServerScriptService\"\n",
        );

        assert_eq!(sync(root, false, None).unwrap(), Outcome::Wrote(2));
        assert_eq!(sync(root, false, None).unwrap(), Outcome::UpToDate);
        assert_eq!(sync(root, true, None).unwrap(), Outcome::UpToDate);
    }

    #[test]
    fn a_project_with_no_aliases_leaves_luaurc_alone() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        write(root, "default.project.json", PROJECT);
        write(root, "larvae.toml", "[process]\ninput = \"src\"\n");
        write(root, ".luaurc", r#"{ "languageMode": "strict" }"#);

        assert_eq!(sync(root, false, None).unwrap(), Outcome::Empty);
        assert_eq!(
            std::fs::read_to_string(root.join(".luaurc")).unwrap(),
            r#"{ "languageMode": "strict" }"#
        );
    }

    #[test]
    fn a_luaurc_that_is_not_json_fails_naming_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        write(root, "default.project.json", PROJECT);
        write(
            root,
            "larvae.toml",
            "[aliases]\npkg = \"@game/ReplicatedStorage/Packages\"\n",
        );
        write(root, ".luaurc", "{ this is not json");

        let err = format!("{:#}", sync(root, false, None).unwrap_err());

        assert!(err.contains(".luaurc"), "{err}");
    }
}
