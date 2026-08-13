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
         {project_note}"
    );

    if !found.aliases.is_empty() {
        template.push_str("\n[aliases]\n");

        for (name, value) in &found.aliases {
            template.push_str(&format!("{name} = \"{value}\"\n"));
        }
    }

    template.push_str("\n[process]\n");
    template.push_str(&match found.inputs.as_slice() {
        [] => "input = \"src\"\n".to_string(),

        [one] => format!("input = \"{}\"\n", slashed(one)),

        many => {
            let list: Vec<String> = many.iter().map(|p| format!("\"{}\"", slashed(p))).collect();

            format!("input = [{}]\n", list.join(", "))
        }
    });
    template.push_str("output = \"dist\"\n\n[requires]\ntarget = \"roblox-string\"\n");
    template.push_str(&fmt_section(root));
    template.push_str(&lint_section(root));

    std::fs::write(&path, template)?;
    ui::print_success(&format!("Wrote {}", crate::ui::rel(&path)));
    report(&found);
    report_existing(root);
    ensure_gitignore(root)?;

    Ok(ExitCode::SUCCESS)
}

/// The formatter and linter configs that another tool can leave; larvae reads them unchanged
const STYLUA: [&str; 2] = ["stylua.toml", ".stylua.toml"];
const SELENE: [&str; 1] = ["selene.toml"];

/*
The defaults of the formatter, written out and not implicit.

The template writes them out because the config shows a user which options
exist. No user searches for `quote_style` in a file that does not name it.
The template writes only the options that users change often, because no user
reads a long list of defaults. The schema holds the rest, and `larvae self
code` connects the schema.

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

        None => format!("\n[fmt]\n{}", fmt_defaults()),
    }
}

/*
Every format option at the value it keeps when the user states nothing.

The list is generated from the defaults of the config type, so it cannot drift
from the code. A scalar option appears; a table and a list do not, because
their shape needs more than one line. The first four lines are live, because
almost every project sets them, and the rest are comments.
*/
fn fmt_defaults() -> String {
    let value = serde_json::to_value(crate::fmt::FmtConfig::default()).unwrap_or_default();

    let Some(table) = value.as_object() else {
        return String::new();
    };

    // the options a project states most often, written live rather than commented
    const LIVE: [&str; 4] = ["column_width", "indent_type", "indent_width", "quote_style"];

    let scalars: Vec<(&String, String)> = table
        .iter()
        .filter_map(|(key, value)| Some((key, scalar_of(value)?)))
        .collect();

    let width = scalars
        .iter()
        .filter(|(k, _)| !LIVE.contains(&k.as_str()))
        .map(|(k, _)| k.len())
        .max()
        .unwrap_or(0);

    let mut out = String::new();

    for name in LIVE {
        if let Some((_, value)) = scalars.iter().find(|(k, _)| k.as_str() == name) {
            out.push_str(&format!("{name} = {value}\n"));
        }
    }

    out.push_str("\n# Every option below keeps the value shown until the user states it.\n");

    for (key, value) in &scalars {
        if !LIVE.contains(&key.as_str()) {
            out.push_str(&format!("# {key:<width$} = {value}\n"));
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

/*
Every lint at the level it keeps when the user states nothing.

The list is generated from the registry, so it cannot drift from the code. The
lines are comments, because a project that writes nothing gets these levels
already. To change one, remove the `#` and edit the level.
*/
fn defaults() -> String {
    let mut lints: Vec<_> = crate::lint::registry().iter().collect();
    lints.sort_by_key(|l| l.name());

    let width = lints.iter().map(|l| l.name().len()).max().unwrap_or(0);

    let mut out = String::from(
        "# Every lint keeps the level below until the user names it here.\n\
         # Remove the `#` to change one.\n",
    );

    for lint in lints {
        let level = match lint.default_level() {
            crate::lint::Level::Allow => "allow",
            crate::lint::Level::Warn => "warn",
            crate::lint::Level::Deny => "deny",
        };

        out.push_str(&format!(
            "# {:<width$} = {level:?}\n",
            lint.name(),
            width = width
        ));
    }

    out
}

/// The same behavior for the linter, for the same reasons
fn lint_section(root: &Path) -> String {
    match existing(root, &SELENE) {
        Some(name) => format!(
            "\n# Linting follows {name}, which larvae reads as it is.\n\
             # A [lint] table here would layer over it.\n"
        ),

        None => format!("\n[lint]\nstd = \"roblox\"\n\n[lint.rules]\n{}", defaults()),
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

        assert_eq!(value(&fmt), value(&crate::fmt::FmtConfig::default()));
        assert_eq!(value(&lint), value(&crate::lint::LintConfig::default()));
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
