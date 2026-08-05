/*!
`larvae self code`, wiring an editor up for completion and hover docs.

Two ways to get there, and we prefer the tidier one. Even Better TOML can map a
filename pattern to a schema in the editor's user settings, which leaves every
`larvae.toml` alone and covers every project at once, since the pattern matches
the filename wherever it sits. Without it we fall back to the `#:schema` line at
the top of the file, which any Taplo based editor reads.
*/

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, bail};

use crate::ui;

/// Where the schema is hosted (this repo)
pub const SCHEMA_URL: &str =
    "https://raw.githubusercontent.com/larvae-luau/larvae/master/crates/larvae/larvae.schema.json";

/// The Even Better TOML setting that maps a filename to a schema
const ASSOCIATIONS: &str = "evenBetterToml.schema.associations";

/*
Matches `larvae.toml` at a path root or after a separator, on either platform.
Written as a regex because that is what the setting takes, not because we want
one.
*/
const PATTERN: &str = r"(^|[/\\])larvae\.toml$";

/// The directive line, for the fallback and for `init`
pub fn directive() -> String {
    format!("#:schema {SCHEMA_URL}")
}

pub fn run(root: &Path) -> Result<ExitCode> {
    let path = root.join("larvae.toml");

    if !path.exists() {
        bail!("no larvae.toml here - run `larvae init` first");
    }

    match installed() {
        Some(editor) => {
            eprintln!("Found Even Better TOML in {}", editor.name);

            associate(&editor.settings)
        }

        None => {
            eprintln!("Even Better TOML not installed, falling back to the schema line.");

            directive_in(&path)
        }
    }
}

/// One editor's extension directory and where its user settings live
struct Editor {
    name: &'static str,
    extensions: PathBuf,
    settings: PathBuf,
}

/*
The editors that ship Even Better TOML, in the order we would rather find them.

Extensions live beside the home directory and settings under the config
directory, which `dirs` already resolves per platform: `~/.config` on Linux,
Application Support on macOS, Roaming on Windows.
*/
fn editors() -> Vec<Editor> {
    let (Some(home), Some(config)) = (dirs::home_dir(), dirs::config_dir()) else {
        return Vec::new();
    };

    [
        ("VS Code", ".vscode", "Code"),
        ("VS Code Insiders", ".vscode-insiders", "Code - Insiders"),
        ("VSCodium", ".vscode-oss", "VSCodium"),
        ("Cursor", ".cursor", "Cursor"),
        ("Windsurf", ".windsurf", "Windsurf"),
    ]
    .into_iter()
    .map(|(name, ext, cfg)| Editor {
        name,
        extensions: home.join(ext).join("extensions"),
        settings: config.join(cfg).join("User/settings.json"),
    })
    .collect()
}

/*
Look for the extension on disk rather than shelling out to `code`, which is
often not on PATH even where the editor is installed.
*/
fn installed() -> Option<Editor> {
    editors().into_iter().find(|editor| {
        let Ok(entries) = std::fs::read_dir(&editor.extensions) else {
            return false;
        };

        // versioned directories, ex: tamasfe.even-better-toml-0.19.2
        entries.flatten().any(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with("tamasfe.even-better-toml")
        })
    })
}

/*
Add the association to the editor's user settings.

User settings rather than a `.vscode` folder in the project, because the
pattern matches `larvae.toml` wherever it sits, so one entry covers every
project this person will ever open instead of one per repo.
*/
fn associate(path: &Path) -> Result<ExitCode> {
    let changed = edit_settings(path, |table| {
        let associations = table
            .entry(ASSOCIATIONS)
            .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));

        let Some(associations) = associations.as_object_mut() else {
            bail!("{ASSOCIATIONS} is not an object, leaving it alone");
        };

        if associations.get(PATTERN).and_then(|v| v.as_str()) == Some(SCHEMA_URL) {
            return Ok(false);
        }

        associations.insert(PATTERN.to_owned(), SCHEMA_URL.into());

        Ok(true)
    })?;

    if changed {
        ui::print_success(&format!("Wired up {}", path.display()));
        eprintln!("Every larvae.toml you open now gets completion and hover docs.");
    } else {
        ui::print_success("Even Better TOML already points at the larvae schema");
    }

    Ok(ExitCode::SUCCESS)
}

/*
Read an editor's settings, let a caller change them, and write back only if it
did. Not writing when nothing changed matters because reserializing drops the
comments VS Code allows, so an unnecessary write costs somebody their notes.
*/
pub fn edit_settings(
    path: &Path,
    edit: impl FnOnce(&mut serde_json::Map<String, serde_json::Value>) -> Result<bool>,
) -> Result<bool> {
    let mut settings = read_settings(path)?;

    let table = settings
        .as_object_mut()
        .with_context(|| format!("{} is not a JSON object", ui::rel(path)))?;

    if !edit(table).with_context(|| format!("in {}", ui::rel(path)))? {
        return Ok(false);
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("cannot create {}", ui::rel(parent)))?;
    }

    let mut text = serde_json::to_string_pretty(&settings)?;
    text.push('\n');

    std::fs::write(path, text).with_context(|| format!("cannot write {}", ui::rel(path)))?;

    Ok(true)
}

/*
Read existing settings, tolerating the comments and trailing commas VS Code
allows. We reserialize as plain JSON, so a comment in there does not survive,
which is why an association that is already correct returns without writing.
*/
fn read_settings(path: &Path) -> Result<serde_json::Value> {
    if !path.exists() {
        return Ok(serde_json::Value::Object(serde_json::Map::new()));
    }

    let text =
        std::fs::read_to_string(path).with_context(|| format!("cannot read {}", ui::rel(path)))?;

    if text.trim().is_empty() {
        return Ok(serde_json::Value::Object(serde_json::Map::new()));
    }

    json5::from_str(&text).with_context(|| format!("{} is not valid JSON", ui::rel(path)))
}

/// The fallback, a `#:schema` line any Taplo based editor reads
fn directive_in(path: &Path) -> Result<ExitCode> {
    let content = std::fs::read_to_string(path)?;
    let directive = directive();
    let first = content.lines().next().unwrap_or("");

    let updated = if first == directive {
        ui::print_success("larvae.toml already references the schema");

        return Ok(ExitCode::SUCCESS);
    } else if first.starts_with("#:schema") {
        // replace a stale or foreign directive rather than stacking a second
        let rest = content.split_once('\n').map(|(_, r)| r).unwrap_or("");

        format!("{directive}\n{rest}")
    } else {
        format!("{directive}\n{content}")
    };

    std::fs::write(path, updated)?;

    ui::print_success(&format!("Added the schema line to {}", ui::rel(path)));
    eprintln!("Editors using Taplo now get completion and hover docs.");

    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings_at(dir: &Path) -> PathBuf {
        dir.join("User/settings.json")
    }

    fn read(path: &Path) -> serde_json::Value {
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    #[test]
    fn an_association_is_written_into_fresh_settings() {
        let tmp = tempfile::tempdir().unwrap();
        let path = settings_at(tmp.path());

        associate(&path).unwrap();

        assert_eq!(
            read(&path)[ASSOCIATIONS][PATTERN],
            serde_json::Value::from(SCHEMA_URL)
        );
    }

    /// The pattern has to survive JSON escaping as the regex it is
    #[test]
    fn the_written_pattern_is_the_regex_even_better_toml_expects() {
        let tmp = tempfile::tempdir().unwrap();
        let path = settings_at(tmp.path());

        associate(&path).unwrap();

        let text = std::fs::read_to_string(&path).unwrap();

        assert!(
            text.contains(r#"(^|[/\\\\])larvae\\.toml$"#),
            "escaping is wrong: {text}"
        );
    }

    /// Somebody's real settings, which we must not trample
    #[test]
    fn other_settings_keep_their_keys() {
        let tmp = tempfile::tempdir().unwrap();
        let path = settings_at(tmp.path());

        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, r#"{ "editor.tabSize": 4, "files.eol": "\n" }"#).unwrap();

        associate(&path).unwrap();

        let s = read(&path);

        assert_eq!(s["editor.tabSize"], 4);
        assert_eq!(s["files.eol"], "\n");
        assert!(s[ASSOCIATIONS][PATTERN].is_string());
    }

    #[test]
    fn another_tools_association_is_left_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let path = settings_at(tmp.path());

        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            r#"{ "evenBetterToml.schema.associations": { "(^|[/\\])lpm\\.toml$": "https://luaupm.com/lpm.schema.json" } }"#,
        )
        .unwrap();

        associate(&path).unwrap();

        let s = read(&path);

        assert_eq!(
            s[ASSOCIATIONS][r"(^|[/\])lpm\.toml$"],
            serde_json::Value::from("https://luaupm.com/lpm.schema.json")
        );
        assert!(s[ASSOCIATIONS][PATTERN].is_string());
    }

    #[test]
    fn comments_and_trailing_commas_are_tolerated() {
        let tmp = tempfile::tempdir().unwrap();
        let path = settings_at(tmp.path());

        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "{\n  // what VS Code writes\n  \"editor.tabSize\": 2,\n}\n",
        )
        .unwrap();

        associate(&path).unwrap();

        assert_eq!(read(&path)["editor.tabSize"], 2);
    }

    /// Rewriting drops comments, so an already correct file must not be touched
    #[test]
    fn a_correct_association_leaves_the_file_byte_identical() {
        let tmp = tempfile::tempdir().unwrap();
        let path = settings_at(tmp.path());

        associate(&path).unwrap();

        let before = std::fs::read_to_string(&path).unwrap();

        associate(&path).unwrap();

        assert_eq!(before, std::fs::read_to_string(&path).unwrap());
    }

    #[test]
    fn the_fallback_adds_and_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("larvae.toml");

        std::fs::write(&path, "[process]\ninput = \"src\"\n").unwrap();
        directive_in(&path).unwrap();

        let content = std::fs::read_to_string(&path).unwrap();

        assert!(content.starts_with(&directive()));
        assert!(content.contains("[process]"));

        directive_in(&path).unwrap();

        assert_eq!(content, std::fs::read_to_string(&path).unwrap());
    }

    #[test]
    fn the_fallback_replaces_a_stale_directive() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("larvae.toml");

        std::fs::write(&path, "#:schema https://old.example/x.json\n[process]\n").unwrap();
        directive_in(&path).unwrap();

        let content = std::fs::read_to_string(&path).unwrap();

        assert!(content.starts_with(&directive()));
        assert!(!content.contains("old.example"));
        assert!(content.contains("[process]"));
    }

    #[test]
    fn errors_without_config() {
        let tmp = tempfile::tempdir().unwrap();

        assert!(run(tmp.path()).is_err());
    }

    /// A settings path per editor, so we never write into the wrong one
    #[test]
    fn every_editor_has_its_own_settings_path() {
        let paths: Vec<_> = editors().into_iter().map(|e| e.settings).collect();
        let mut unique = paths.clone();

        unique.sort();
        unique.dedup();

        assert_eq!(paths.len(), unique.len());
        assert!(paths.iter().all(|p| p.ends_with("User/settings.json")));
    }
}
