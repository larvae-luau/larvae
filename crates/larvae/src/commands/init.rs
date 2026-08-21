//! `larvae init` writes a starter larvae.toml.

use std::path::Path;
use std::process::ExitCode;

use anyhow::{Result, bail};

use crate::ui;

pub fn run(root: &Path) -> Result<ExitCode> {
    let path = root.join("larvae.toml");

    if path.exists() {
        bail!("larvae.toml already exists");
    }

    let project = crate::project::rojo::find_project(root, None)
        .and_then(|p| crate::project::rojo::load(&p).ok());
    let found = crate::commands::detect::scan(root, project.as_ref());

    let project_note = if project.is_some() {
        "# Mounts come from default.project.json, nothing to repeat here.\n"
    } else {
        "# No default.project.json found, the roblox-string target needs one\n# or a [requires.mounts] table.\n"
    };

    /*
    No schema line here. `larvae self code` decides how a project gets editor
    support. That command prefers an Even Better TOML entry in
    .vscode/settings.json to a URL at the top of the config of a user.
    */
    let mut template = format!(
        "# larvae configuration, run `larvae self code` for editor completion.\n\
         {project_note}\n"
    );

    /*
    The three keys every project sets, in their root short forms. TOML reads
    a root key only before the first table header, so these lines come first.
    */
    template.push_str(&match found.inputs.as_slice() {
        [] => "input = \"src\"\n".to_string(),

        [one] => format!("input = \"{}\"\n", slashed(one)),

        many => {
            let list: Vec<String> = many.iter().map(|p| format!("\"{}\"", slashed(p))).collect();

            format!("input = [{}]\n", list.join(", "))
        }
    });
    template.push_str("output = \"dist\"\ntarget = \"roblox-string\"\n");

    if !found.aliases.is_empty() {
        template.push_str("\n[aliases]\n");

        for (name, value) in &found.aliases {
            template.push_str(&format!("{name} = \"{value}\"\n"));
        }
    }

    template.push_str(&fmt_section(root));
    template.push_str(&lint_section(root));

    std::fs::write(&path, template)?;
    ui::print_success(&format!("Wrote {}", crate::ui::rel(&path)));
    report(&found);
    report_existing(root);
    ensure_gitignore(root)?;
    hand_over_linting(root);

    Ok(ExitCode::SUCCESS)
}

/*
Turn Luau's own lints off in `.luaurc`, so one linter reports.

Luau's compiler ships twenty eight lints and larvae covers them, so leaving
both on means every finding twice: once from luau-lsp in the editor and once
from `larvae lint` in CI, under two names, with two ways to silence one. The
`.luaurc` key that settles it is `lint`, and `"*": false` covers every rule
including ones a later Luau adds.

The edit is textual rather than a parse and a re-serialise, because `.luaurc`
is another tool's file: it holds the project's aliases, it allows comments, and
larvae has no business reformatting it. Inserting after the opening brace keeps
every other byte, comments included.

Nothing happens when the file already sets `lint`. That is the project having
an opinion, and this is not the command to overrule it.
*/
fn hand_over_linting(root: &Path) {
    let path = root.join(".luaurc");

    let Ok(text) = std::fs::read_to_string(&path) else {
        // no .luaurc to take over, and creating one only for this is overreach
        return;
    };

    let already = json5::from_str::<serde_json::Value>(&text)
        .ok()
        .is_some_and(|v| v.get("lint").is_some());

    if already {
        eprintln!("  .luaurc already sets lint, leaving it alone");

        return;
    }

    let Some(open) = text.find('{') else {
        eprintln!("  could not read .luaurc, add \"lint\": {{ \"*\": false }} to it yourself");

        return;
    };

    // an empty object takes no comma, anything else needs one
    let rest = text[open + 1..].trim_start();
    let comma = if rest.starts_with('}') { "" } else { "," };

    let mut out = String::with_capacity(text.len() + 32);
    out.push_str(&text[..=open]);
    out.push_str(&format!("\n  \"lint\": {{ \"*\": false }}{comma}"));
    out.push_str(&text[open + 1..]);

    match std::fs::write(&path, out) {
        Ok(()) => eprintln!("  turned Luau's own lints off in .luaurc, larvae reports them now"),

        Err(e) => eprintln!("  could not write .luaurc, {e}"),
    }
}

/// The formatter and linter configs that another tool can leave; larvae reads them unchanged
const STYLUA: [&str; 2] = ["stylua.toml", ".stylua.toml"];
const SELENE: [&str; 1] = ["selene.toml"];

/*
The template writes only the options that users change often, because no
user reads a long list of defaults. The full list lives in the docs and in
the schema, and `larvae self code` connects the schema to the editor.

The function writes nothing when the project has a stylua.toml. These keys
layer over that file. So a write of the defaults here would override settings
that the project made.
*/
fn fmt_section(root: &Path) -> String {
    match existing(root, &STYLUA) {
        Some(name) => format!(
            "\n# Formatting follows {name}, which larvae reads as it is.\n\
             # A [fmt] table here would layer over it.\n"
        ),

        /*
        `recommended = true` is written out, though it is also the default.

        The value is not the point; the key is. A new project reads the file
        it was given, and a key that is there can be turned off. A key that
        is absent has to be found in the docs first.
        */
        None => format!("\n[fmt]\nrecommended = true\n{}", fmt_defaults()),
    }
}

/*
The four options a project states most often, at their default values.

The values come from the config type, so they cannot drift from the code.
Every other option keeps its default silently; the docs list them all.
*/
fn fmt_defaults() -> String {
    let value = serde_json::to_value(crate::fmt::FmtConfig::default()).unwrap_or_default();

    let Some(table) = value.as_object() else {
        return String::new();
    };

    const LIVE: [&str; 4] = ["column_width", "indent_type", "indent_width", "quote_style"];

    let mut out = String::new();

    for name in LIVE {
        if let Some(value) = table.get(name).and_then(scalar_of) {
            out.push_str(&format!("{name} = {value}\n"));
        }
    }

    out
}

/// One JSON scalar as the user would write it in TOML. A table and a list
/// return nothing, because their shape needs more than one line.
fn scalar_of(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(s) => Some(format!("{s:?}")),

        serde_json::Value::Bool(b) => Some(b.to_string()),

        serde_json::Value::Number(n) => Some(n.to_string()),

        _ => None,
    }
}

/// The same behavior for the linter, for the same reasons
fn lint_section(root: &Path) -> String {
    match existing(root, &SELENE) {
        Some(name) => format!(
            "\n# Linting follows {name}, which larvae reads as it is.\n\
             # A [lint] table here would layer over it.\n"
        ),

        None => "\n[lint]\nrecommended = true\nstd = \"roblox\"\n\
             # Set a level for one lint under [lint.rules], or for a whole kind\n\
             # under [lint.groups]. `larvae lint --explain <name>` describes one.\n"
            .to_string(),
    }
}

/// The first of these files that the project has, if the project has one
fn existing<'a>(root: &Path, names: &[&'a str]) -> Option<&'a str> {
    names.iter().copied().find(|name| root.join(name).exists())
}

/// Report when larvae uses the config of another tool
fn report_existing(root: &Path) {
    for name in STYLUA.iter().chain(SELENE.iter()) {
        if root.join(name).exists() {
            eprintln!("  found {name}, larvae reads it as it is");
        }
    }
}

/// Report the detected data, so the user does not have to diff the file
fn report(found: &crate::commands::detect::Detected) {
    for (name, value) in &found.aliases {
        eprintln!("  found a package directory, added @{name} = {value}");
    }

    if !found.luaurc_aliases.is_empty() {
        eprintln!(
            "  .luaurc already defines @{}, larvae uses those as they are",
            found.luaurc_aliases.join(", @")
        );
    }

    if found.inputs.len() > 1 {
        eprintln!(
            "  {} source directories, each keeps its own folder under dist",
            found.inputs.len()
        );
    }
}

/// The template writes config paths with forward slashes on every platform
fn slashed(path: &Path) -> String {
    path.components()
        .filter_map(|c| c.as_os_str().to_str())
        .collect::<Vec<_>>()
        .join("/")
}

/// The entries that git must ignore for the outputs of larvae
const IGNORE_ENTRIES: [&str; 2] = [".larvae/", "dist/"];

/// Keep the output directories ignored: append to an existing .gitignore, or offer to create one
fn ensure_gitignore(root: &Path) -> Result<()> {
    let path = root.join(".gitignore");

    if path.exists() {
        let content = std::fs::read_to_string(&path)?;
        let missing: Vec<&str> = IGNORE_ENTRIES
            .iter()
            .filter(|e| !ignores(&content, e))
            .copied()
            .collect();
        if missing.is_empty() {
            return Ok(());
        }

        let mut out = content;

        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }

        for entry in &missing {
            out.push_str(entry);
            out.push('\n');
        }

        std::fs::write(&path, out)?;
        ui::print_success(&format!("Added {} to .gitignore", missing.join(" and ")));
    } else if ui::confirm(
        "No .gitignore found. Create one ignoring .larvae/ and dist/?",
        true,
    ) {
        std::fs::write(&path, format!("{}\n", IGNORE_ENTRIES.join("\n")))?;
        ui::print_success(&format!("Created {}", crate::ui::rel(&path)));
    }

    Ok(())
}

/// True when the content covers the entry; slashes are optional
fn ignores(content: &str, entry: &str) -> bool {
    let want = entry.trim_start_matches('/').trim_end_matches('/');
    content
        .lines()
        .any(|line| line.trim().trim_start_matches('/').trim_end_matches('/') == want)
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- .luaurc, handing linting over -------------------------------------

    fn luaurc(body: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".luaurc");
        std::fs::write(&path, body).unwrap();

        (dir, path)
    }

    /*
    Luau's compiler ships lints that larvae covers. With both on, every finding
    arrives twice under two names, with two ways to silence one.
    */
    #[test]
    fn luau_lints_are_turned_off_and_the_aliases_survive() {
        let (dir, path) = luaurc(r#"{ "aliases": { "pkg": "p" } }"#);
        hand_over_linting(dir.path());

        let out = std::fs::read_to_string(&path).unwrap();

        assert!(out.contains(r#""lint""#), "{out}");
        assert!(out.contains(r#""*": false"#), "{out}");
        assert!(out.contains(r#""pkg""#), "the aliases must survive: {out}");
    }

    /// The result has to be something Luau still reads
    #[test]
    fn the_rewritten_file_is_still_valid() {
        let (dir, path) = luaurc("{\n  // our packages\n  \"aliases\": { \"pkg\": \"p\" }\n}");
        hand_over_linting(dir.path());

        let out = std::fs::read_to_string(&path).unwrap();
        let parsed: serde_json::Value = json5::from_str(&out).expect("still parses");

        assert_eq!(parsed["lint"]["*"], serde_json::json!(false));
        assert_eq!(parsed["aliases"]["pkg"], serde_json::json!("p"));
    }

    /// `.luaurc` belongs to another tool, so the edit keeps every other byte
    #[test]
    fn a_comment_is_not_lost() {
        let (dir, path) = luaurc("{\n  // our packages\n  \"aliases\": {}\n}");
        hand_over_linting(dir.path());

        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("// our packages"),
            "the comment was dropped"
        );
    }

    #[test]
    fn an_empty_object_is_handled_without_a_stray_comma() {
        let (dir, path) = luaurc("{}");
        hand_over_linting(dir.path());

        let out = std::fs::read_to_string(&path).unwrap();
        let parsed: serde_json::Value = json5::from_str(&out).expect("still parses");

        assert_eq!(parsed["lint"]["*"], serde_json::json!(false));
    }

    /// A project with an opinion about lints keeps it
    #[test]
    fn a_file_that_already_sets_lint_is_left_alone() {
        let body = r#"{ "lint": { "LocalUnused": false } }"#;
        let (dir, path) = luaurc(body);
        hand_over_linting(dir.path());

        assert_eq!(std::fs::read_to_string(&path).unwrap(), body);
    }

    #[test]
    fn no_luaurc_means_none_is_created() {
        let dir = tempfile::tempdir().unwrap();
        hand_over_linting(dir.path());

        assert!(!dir.path().join(".luaurc").exists());
    }

    /// The defaults that init writes must equal the defaults of larvae. If not,
    /// the first run after `larvae init` formats differently than the run before it.
    #[test]
    fn the_written_defaults_are_the_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("larvae.toml");

        std::fs::write(
            &path,
            format!("{}{}", fmt_section(dir.path()), lint_section(dir.path())),
        )
        .unwrap();

        let config = crate::config::Config::load(&path).unwrap();
        let fmt = crate::fmt::FmtConfig::discover(dir.path(), config.fmt.as_ref()).unwrap();
        let lint = crate::lint::LintConfig::discover(dir.path(), config.lint.as_ref()).unwrap();

        fn value<T: serde::Serialize>(v: &T) -> toml::Value {
            toml::Value::try_from(v).unwrap()
        }

        /*
        `recommended = true` is written out, and it is also the default, so
        the two configs differ by that key alone. The comparison sets it on
        both sides rather than skipping it: a default that ever stopped
        meaning `true` has to fail here.
        */
        let fmt_default = crate::fmt::FmtConfig {
            recommended: Some(true),
            ..Default::default()
        };

        let lint_default = crate::lint::LintConfig {
            recommended: Some(true),
            ..Default::default()
        };

        assert_eq!(value(&fmt), value(&fmt_default));
        assert_eq!(value(&lint), value(&lint_default));

        // And what init wrote is what larvae does with no config at all.
        assert_eq!(
            fmt.magic_trailing_comma,
            crate::fmt::FmtConfig::default().magic_trailing_comma
        );
        assert_eq!(
            lint.level_for(
                "shadowing",
                Some(crate::lint::Group::Suspicious),
                crate::lint::Level::Warn
            ),
            crate::lint::Level::Warn
        );
    }

    /// A project that formats with stylua keeps its settings. The file that
    /// init writes must not override them.
    #[test]
    fn another_tools_config_is_left_in_charge() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("stylua.toml"), "column_width = 60\n").unwrap();
        std::fs::write(dir.path().join("selene.toml"), "std = \"luau\"\n").unwrap();

        let path = dir.path().join("larvae.toml");
        std::fs::write(
            &path,
            format!("{}{}", fmt_section(dir.path()), lint_section(dir.path())),
        )
        .unwrap();

        let config = crate::config::Config::load(&path).unwrap();

        assert!(config.fmt.is_none(), "[fmt] should not have been written");
        assert!(config.lint.is_none(), "[lint] should not have been written");

        let fmt = crate::fmt::FmtConfig::discover(dir.path(), config.fmt.as_ref()).unwrap();
        let lint = crate::lint::LintConfig::discover(dir.path(), config.lint.as_ref()).unwrap();

        assert_eq!(fmt.column_width, 60);
        assert_eq!(lint.std, crate::lint::config::StdLib::Luau);
    }

    #[test]
    fn gitignore_matching() {
        assert!(ignores("dist/\n", "dist/"));
        assert!(ignores("/dist\n", "dist/"));
        assert!(ignores("target\n.larvae\n", ".larvae/"));
        assert!(!ignores("distros/\n", "dist/"));
        assert!(!ignores("", "dist/"));
    }
}
