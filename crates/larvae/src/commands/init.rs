//! `larvae init`, scaffold a starter larvae.toml

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
    support, and prefers wiring Even Better TOML in .vscode/settings.json to
    putting a URL at the top of somebody's config.
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

    std::fs::write(&path, template)?;
    ui::print_success(&format!("Wrote {}", crate::ui::rel(&path)));
    report(&found);
    ensure_gitignore(root)?;

    Ok(ExitCode::SUCCESS)
}

/// Say what was picked up, so nobody has to diff the file to find out
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

/// Config paths are written with forward slashes on every platform
fn slashed(path: &Path) -> String {
    path.components()
        .filter_map(|c| c.as_os_str().to_str())
        .collect::<Vec<_>>()
        .join("/")
}

/// Entries larvae's outputs need ignored
const IGNORE_ENTRIES: [&str; 2] = [".larvae/", "dist/"];

/// Keep the output dirs ignored, append to an existing .gitignore or offer to create one
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

/// True when the content already covers entry, slashes optional
fn ignores(content: &str, entry: &str) -> bool {
    let want = entry.trim_start_matches('/').trim_end_matches('/');
    content
        .lines()
        .any(|line| line.trim().trim_start_matches('/').trim_end_matches('/') == want)
}

#[cfg(test)]
mod tests {
    use super::ignores;

    #[test]
    fn gitignore_matching() {
        assert!(ignores("dist/\n", "dist/"));
        assert!(ignores("/dist\n", "dist/"));
        assert!(ignores("target\n.larvae\n", ".larvae/"));
        assert!(!ignores("distros/\n", "dist/"));
        assert!(!ignores("", "dist/"));
    }
}
