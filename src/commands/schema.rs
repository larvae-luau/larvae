//! larvae schema, adds the #:schema directive so editors get completion and docs

use std::path::Path;
use std::process::ExitCode;

use anyhow::{Result, bail};

use crate::ui;

/// Where the schema is hosted (this repo)
pub const SCHEMA_URL: &str =
    "https://raw.githubusercontent.com/larvae-luau/larvae/master/larvae.schema.json";

/// The directive line, ready to prepend
pub fn directive() -> String {
    format!("#:schema {SCHEMA_URL}")
}

pub fn run(root: &Path) -> Result<ExitCode> {
    let path = root.join("larvae.toml");

    if !path.exists() {
        bail!("no larvae.toml here - run `larvae init` first");
    }

    let content = std::fs::read_to_string(&path)?;
    let directive = directive();

    let first = content.lines().next().unwrap_or("");

    let new_content = if first == directive {
        ui::print_success("larvae.toml already references the schema");

        return Ok(ExitCode::SUCCESS);
    } else if first.starts_with("#:schema") {
        // Replace a stale/other schema directive
        let rest = content.split_once('\n').map(|(_, r)| r).unwrap_or("");
        format!("{directive}\n{rest}")
    } else {
        format!("{directive}\n{content}")
    };

    std::fs::write(&path, new_content)?;
    ui::print_success(&format!(
        "Added schema reference to {}",
        crate::ui::rel(&path)
    ));

    eprintln!("Editors with Even Better TOML / Taplo now get completion and docs.");

    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adds_and_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();

        std::fs::write(root.join("larvae.toml"), "[process]\ninput = \"src\"\n").unwrap();

        run(root).unwrap();

        let content = std::fs::read_to_string(root.join("larvae.toml")).unwrap();
        assert!(content.starts_with(&directive()));
        assert!(content.contains("[process]"));

        // Second run leaves the file unchanged
        run(root).unwrap();

        let again = std::fs::read_to_string(root.join("larvae.toml")).unwrap();
        assert_eq!(content, again);
    }

    #[test]
    fn replaces_stale_directive() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();

        std::fs::write(
            root.join("larvae.toml"),
            "#:schema https://old.example/x.json\n[process]\n",
        )
        .unwrap();
        run(root).unwrap();

        let content = std::fs::read_to_string(root.join("larvae.toml")).unwrap();

        assert!(content.starts_with(&directive()));
        assert!(!content.contains("old.example"));
        assert!(content.contains("[process]"));
    }

    #[test]
    fn errors_without_config() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(run(tmp.path()).is_err());
    }
}
