//! .luaurc discovery and alias lookup, as the Luau RFCs specify. The module
//! parses each file once. A lookup walks upward, and the nearest entry wins.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct LuaurcFile {
    #[serde(default)]
    aliases: HashMap<String, String>,
    // larvae does not own the other .luaurc fields, so it accepts them.
    #[serde(flatten)]
    _rest: HashMap<String, serde_json::Value>,
}

/// Aliases that the `.luaurc` in one directory defines (keys in lowercase)
#[derive(Debug, Default)]
pub struct DirAliases {
    /// Alias name (lowercase) -> (value, directory that defined it)
    pub aliases: HashMap<String, String>,
}

#[derive(Debug, Default)]
pub struct LuaurcIndex {
    /// Directory -> aliases that the `.luaurc` directly in it defines
    dirs: HashMap<PathBuf, DirAliases>,
    root: PathBuf,
}

impl LuaurcIndex {
    pub fn new(root: &Path) -> Self {
        Self {
            dirs: HashMap::new(),
            root: root.to_owned(),
        }
    }

    /// Parse and register a .luaurc; a bad file gets an error message and no entry
    pub fn add_file(&mut self, path: &Path) -> Result<(), String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("failed to read {}: {e}", crate::ui::rel(path)))?;
        // .luaurc is JSON with comments; json5 accepts a superset of that.
        let parsed: LuaurcFile = json5::from_str(&text)
            .map_err(|e| format!("invalid .luaurc at {}: {e}", crate::ui::rel(path)))?;
        let dir = path.parent().unwrap_or(Path::new("")).to_owned();
        let entry = self.dirs.entry(dir).or_default();

        for (name, value) in parsed.aliases {
            let name = name.strip_prefix('@').unwrap_or(&name).to_lowercase();
            entry.aliases.insert(name, value);
        }

        Ok(())
    }

    /// Look up an alias for a file with a walk up to the root; returns the value
    /// and the directory that defines it
    pub fn lookup(&self, from_dir: &Path, alias: &str) -> Option<(&str, &Path)> {
        let mut dir = from_dir;

        loop {
            if let Some((key, d)) = self.dirs.get_key_value(dir)
                && let Some(found) = d.aliases.get(alias)
            {
                return Some((found.as_str(), key.as_path()));
            }

            if dir == self.root {
                return None;
            }

            dir = dir.parent()?;
            // The walk never goes above the project root.
            if !dir.starts_with(&self.root) && dir != self.root {
                return None;
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        self.dirs.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_jsonc_and_inherits_upward() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();

        std::fs::create_dir_all(root.join("src/deep")).unwrap();
        std::fs::write(
            root.join(".luaurc"),
            r#"{
                // project-wide aliases
                "aliases": { "Pkg": "./Packages", "util": "./util" }
            }"#,
        )
        .unwrap();
        std::fs::write(
            root.join("src/.luaurc"),
            r#"{ "aliases": { "util": "./local-util" } }"#,
        )
        .unwrap();

        let mut idx = LuaurcIndex::new(root);
        idx.add_file(&root.join(".luaurc")).unwrap();
        idx.add_file(&root.join("src/.luaurc")).unwrap();

        // The nearest entry wins for each key; a missing key inherits from a parent.
        let (v, d) = idx.lookup(&root.join("src/deep"), "util").unwrap();
        assert_eq!(v, "./local-util");
        assert_eq!(d, root.join("src"));
        let (v, d) = idx.lookup(&root.join("src/deep"), "pkg").unwrap();
        assert_eq!(v, "./Packages");
        assert_eq!(d, root);
        assert!(idx.lookup(&root.join("src"), "nope").is_none());
    }
}
