/*!
What `init` can work out about a project on its own

The point is that a config someone has to fill in by hand is a config they
get wrong. Whatever the project file already says gets read back out, so a
Wally or pesde tree comes out configured rather than commented
*/

use std::path::{Path, PathBuf};

use crate::project::rojo::{self, Project};

/// A package directory and the alias worth giving it
const PACKAGE_DIRS: [(&str, &str); 6] = [
    ("Packages", "pkg"),
    ("ServerPackages", "serverpkg"),
    ("DevPackages", "devpkg"),
    ("roblox_packages", "pkg"),
    ("luau_packages", "pkg"),
    ("packages", "pkg"),
];

/// What init found to put in the config
pub struct Detected {
    /// Source directories, mounted and holding code rather than dependencies
    pub inputs: Vec<PathBuf>,
    /// Alias name and the `@game/...` value it should carry
    pub aliases: Vec<(String, String)>,
    /// Aliases the project already gets from a .luaurc, which need no config
    pub luaurc_aliases: Vec<String>,
}

pub fn scan(root: &Path, project: Option<&Project>) -> Detected {
    let mut inputs = Vec::new();
    let mut aliases = Vec::new();

    if let Some(p) = project {
        for mount in rojo::mounts(p) {
            let Ok(rel) = mount.fs.strip_prefix(root) else {
                continue;
            };

            let name = mount
                .fs
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or_default();

            match PACKAGE_DIRS.iter().find(|(dir, _)| *dir == name) {
                // dependencies, so an alias rather than something to process
                Some((_, alias)) => {
                    let value = format!("@game/{}", mount.dm.join("/"));

                    if !aliases.iter().any(|(a, _): &(String, String)| a == alias) {
                        aliases.push(((*alias).to_string(), value));
                    }
                }

                None => {
                    if mount.fs.is_dir() && !inputs.contains(&rel.to_path_buf()) {
                        inputs.push(rel.to_path_buf());
                    }
                }
            }
        }
    }

    // nothing mounted, fall back to the layout almost everyone uses
    if inputs.is_empty() && root.join("src").is_dir() {
        inputs.push(PathBuf::from("src"));
    }

    inputs.sort();

    Detected {
        inputs,
        aliases,
        luaurc_aliases: luaurc_aliases(root),
    }
}

/// Alias names a root .luaurc already defines, larvae honours those as is
fn luaurc_aliases(root: &Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(root.join(".luaurc")) else {
        return Vec::new();
    };

    let Ok(value) = json5::from_str::<serde_json::Value>(&text) else {
        return Vec::new();
    };

    let mut names: Vec<String> = value
        .get("aliases")
        .and_then(|a| a.as_object())
        .map(|o| o.keys().cloned().collect())
        .unwrap_or_default();
    names.sort();

    names
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, rel: &str, text: &str) {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn a_wally_tree_comes_back_configured() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        write(
            root,
            "default.project.json",
            r#"{ "name": "g", "tree": { "$className": "DataModel",
                 "ReplicatedStorage": {
                   "app": { "$path": "src/shared" },
                   "Packages": { "$path": "Packages" } },
                 "ServerScriptService": {
                   "ServerPackages": { "$path": "ServerPackages" } } } }"#,
        );
        std::fs::create_dir_all(root.join("src/shared")).unwrap();
        std::fs::create_dir_all(root.join("Packages")).unwrap();
        std::fs::create_dir_all(root.join("ServerPackages")).unwrap();

        let project = rojo::load(&root.join("default.project.json")).unwrap();
        let found = scan(root, Some(&project));

        assert_eq!(found.inputs, [PathBuf::from("src/shared")]);
        assert!(
            found
                .aliases
                .contains(&("pkg".into(), "@game/ReplicatedStorage/Packages".into())),
            "{:?}",
            found.aliases
        );
        assert!(
            found.aliases.contains(&(
                "serverpkg".into(),
                "@game/ServerScriptService/ServerPackages".into()
            )),
            "{:?}",
            found.aliases
        );
    }

    #[test]
    fn several_source_mounts_all_come_back() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        write(
            root,
            "default.project.json",
            r#"{ "name": "g", "tree": { "$className": "DataModel",
                 "ReplicatedStorage": { "app": { "$path": "src/shared" } },
                 "ServerScriptService": { "s": { "$path": "src/server" } } } }"#,
        );
        std::fs::create_dir_all(root.join("src/shared")).unwrap();
        std::fs::create_dir_all(root.join("src/server")).unwrap();

        let project = rojo::load(&root.join("default.project.json")).unwrap();
        let found = scan(root, Some(&project));

        assert_eq!(
            found.inputs,
            [PathBuf::from("src/server"), PathBuf::from("src/shared")]
        );
    }

    #[test]
    fn with_no_project_it_falls_back_to_src() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("src")).unwrap();

        assert_eq!(scan(root, None).inputs, [PathBuf::from("src")]);
    }

    #[test]
    fn luaurc_aliases_are_reported_not_copied() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(
            root,
            ".luaurc",
            r#"{ "aliases": { "sig": "node_modules/sig" } }"#,
        );

        let found = scan(root, None);

        assert_eq!(found.luaurc_aliases, ["sig"]);
        assert!(found.aliases.is_empty());
    }
}
