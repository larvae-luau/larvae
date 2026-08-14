/*!
`larvae self code` configures an editor for completion and hover docs.

The command has two methods, and it prefers the first. Even Better TOML can map
a filename pattern to a schema in the editor's user settings. This method does
not change any `larvae.toml` file. The pattern matches the filename in every
location, so one entry covers every project. If Even Better TOML is not
installed, the command writes the `#:schema` line at the top of the file. Every
Taplo based editor reads that line.
*/

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, bail};

use crate::ui;

/// The URL where this repository hosts the schema
pub const SCHEMA_URL: &str =
    "https://raw.githubusercontent.com/larvae-luau/larvae/master/crates/larvae/larvae.schema.json";

/// The Even Better TOML setting that maps a filename to a schema
const ASSOCIATIONS: &str = "evenBetterToml.schema.associations";

/*
The pattern matches `larvae.toml` at a path root or after a separator, on each
platform. The setting accepts only a regex, so the pattern is a regex.
*/
const PATTERN: &str = r"(^|[/\\])larvae\.toml$";

/// The directive line for the fallback and for `init`
pub fn directive() -> String {
    format!("#:schema {SCHEMA_URL}")
}

pub fn run(root: &Path) -> Result<ExitCode> {
    let path = root.join("larvae.toml");

    if !path.exists() {
        bail!("no larvae.toml here - run `larvae init` first");
    }

    // a project with worms has a schema of its own, and it wins for that project
    project_schema(root)?;

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

/// The extension directory of one editor and the path of its user settings
struct Editor {
    name: &'static str,
    extensions: PathBuf,
    settings: PathBuf,
}

/*
The editors that can hold Even Better TOML, in the preferred search order.

Each editor keeps extensions beside the home directory and user settings under
the config directory. The `dirs` crate resolves the config directory for each
platform: `~/.config` on Linux, Application Support on macOS, Roaming on
Windows.
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
Look for the extension on disk and do not run `code`. The `code` command is
often not on PATH when the editor is installed.
*/
fn installed() -> Option<Editor> {
    editors().into_iter().find(|editor| {
        let Ok(entries) = std::fs::read_dir(&editor.extensions) else {
            return false;
        };

        // The directory names have versions, for example: tamasfe.even-better-toml-0.19.2
        entries.flatten().any(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with("tamasfe.even-better-toml")
        })
    })
}

/*
Add the association to the user settings of the editor.

The command writes user settings and not a `.vscode` folder in the project.
The pattern matches `larvae.toml` in every location. So one entry covers every
project that the user opens, and each repository does not need its own entry.
*/
/*
Point this project at the schema that knows its worms.

The hosted schema covers every project, so it cannot describe the rules, the
lints, and the settings that one project's worms declare. Larvae writes a
merged copy into the cache directory when it loads worms. This association
lives in the settings of the project, so it applies to this repository alone.
*/
/*
The `file://` URL of a path, because an editor takes a URL and not a path.

Even Better TOML refuses a relative value: it reports `relative URL without a
base`. So larvae writes an absolute URL. The URL names this machine, which is
why this entry belongs to a developer and not to a repository.
*/
fn file_url(path: &Path) -> String {
    let absolute = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let text = absolute.display().to_string();

    match cfg!(windows) {
        // a windows path starts with a drive letter, and the URL needs a root
        true => format!("file:///{}", text.replace('\\', "/")),

        false => format!("file://{text}"),
    }
}

fn project_schema(root: &Path) -> Result<()> {
    let config = crate::config::Config::load(&root.join("larvae.toml"))?;
    let generated = root
        .join(&config.process.cache_dir)
        .join(crate::schema::FILE);

    /*
    The command writes the schema itself rather than wait for a build.

    A user runs this command to set an editor up, often before any other
    command has run in the project. Without this the schema does not exist
    yet, the command finds nothing to point at, and the editor completes
    nothing that a worm declares. Loading the worms also refreshes a schema
    that a worm has since changed.
    */
    match crate::worm::registry::Registry::for_project(root, &config) {
        Ok(worms) if !worms.is_empty() => {
            crate::schema::write(&root.join(&config.process.cache_dir), &worms)?;
        }

        // a project with no worms wants the schema that larvae hosts
        Ok(_) => return Ok(()),

        /*
        A worm that larvae cannot load is a problem for a build to report.
        Here it only means there is no schema of this project to point at,
        and the hosted schema still answers.
        */
        Err(e) => {
            eprintln!(
                "Cannot read the worms of this project, so the editor keeps the hosted schema."
            );
            eprintln!("  {e:#}");

            return Ok(());
        }
    }

    if !generated.exists() {
        return Ok(());
    }

    let settings = root.join(".vscode/settings.json");
    let target = file_url(&generated);
    let listed = target.clone();

    let changed = edit_settings(&settings, move |table| {
        let associations = table
            .entry(ASSOCIATIONS)
            .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));

        let Some(associations) = associations.as_object_mut() else {
            bail!("{ASSOCIATIONS} is not an object, leaving it alone");
        };

        if associations.get(PATTERN).and_then(|v| v.as_str()) == Some(listed.as_str()) {
            return Ok(false);
        }

        associations.insert(PATTERN.to_owned(), listed.into());

        Ok(true)
    })?;

    match changed {
        true => ui::print_success(&format!(
            "{} now points at {target}, which knows this project's worms",
            ui::rel(&settings)
        )),

        false => ui::print_success(&format!(
            "{} already points at {target}",
            ui::rel(&settings)
        )),
    }

    Ok(())
}

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
Read the settings of an editor, let the caller change them, and write the file
only after a change. A write serializes plain JSON and removes the comments
that VS Code permits. So a write without a change deletes the notes of the
user.
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
Read the current settings, and accept the comments and trailing commas that
VS Code permits. The write path serializes plain JSON and removes comments.
For that reason, a correct association returns without a write.
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

/// The fallback: a `#:schema` line that every Taplo based editor reads
fn directive_in(path: &Path) -> Result<ExitCode> {
    let content = std::fs::read_to_string(path)?;
    let directive = directive();
    let first = content.lines().next().unwrap_or("");

    let updated = if first == directive {
        ui::print_success("larvae.toml already references the schema");

        return Ok(ExitCode::SUCCESS);
    } else if first.starts_with("#:schema") {
        // Replace a stale or foreign directive; do not add a second directive.
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

    /// JSON escaping must keep the pattern a valid regex.
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

    /// The real settings of a user; the command must not overwrite them.
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

    /// A rewrite removes comments, so the command must not change a correct file.
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

    /// Each editor has its own settings path, so the command writes to the correct file.
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
