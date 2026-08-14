/*!
The data that `init` detects about a project without user input

A config that a user fills in by hand is a config with errors. The scan reads
the data that the project file already contains. So a Wally or pesde tree
gets a full config and not a commented template.
*/

use std::path::{Path, PathBuf};

use crate::project::rojo::{self, Project};

/// A package directory and the alias that larvae gives it
const PACKAGE_DIRS: [(&str, &str); 6] = [
    ("Packages", "pkg"),
    ("ServerPackages", "serverpkg"),
    ("DevPackages", "devpkg"),
    ("roblox_packages", "pkg"),
    ("luau_packages", "pkg"),
    ("packages", "pkg"),
];

/*
Returns the alias for a dependency directory, when this path is inside one.

The check reads every component and not only the last one. A package tree is
usually mounted one level down: `packages/roblox` has the file name `roblox`
and is still a package directory. A check of the last component alone put
that mount in `input`, so `larvae init` proposed the dependencies of a
project as sources.

The check reads the path relative to the project root. The absolute path
would also match when the repository itself lives under a directory named
`packages`.
*/
fn package_alias(rel: &Path) -> Option<&'static str> {
    rel.components().find_map(|c| {
        let name = c.as_os_str().to_str()?;

        PACKAGE_DIRS
            .iter()
            .find(|(dir, _)| *dir == name)
            .map(|(_, alias)| *alias)
    })
}

/*
Folds sibling roots into the directory that holds them.

A Rojo project mounts `src/client`, `src/server` and `src/shared` separately.
A list of all three roots is noisy, and it causes a later fault: larvae would
silently skip a file added under `src` before a mount exists for it. One root
covers the three mounts, and one root is what almost every project means.

The fold applies only to two or more roots, so a project that mounts a single
subdirectory keeps it. The fold stops at a real shared parent, so unrelated
roots such as `src` and `assets` stay separate and do not collapse to the
repository root.
*/
fn collapse(inputs: Vec<PathBuf>) -> Vec<PathBuf> {
    if inputs.len() < 2 {
        return inputs;
    }

    let mut shared: Vec<_> = inputs[0].components().collect();

    for path in &inputs[1..] {
        let other: Vec<_> = path.components().collect();
        let keep = shared
            .iter()
            .zip(&other)
            .take_while(|(a, b)| a == b)
            .count();

        shared.truncate(keep);
    }

    // The roots share only the repository root, so the list stays as it is.
    if shared.is_empty() {
        return inputs;
    }

    vec![shared.iter().collect()]
}

/// The data that init found for the config
pub struct Detected {
    /// Mounted source directories that hold code and not dependencies
    pub inputs: Vec<PathBuf>,
    /// The alias name and the `@game/...` value that it must hold
    pub aliases: Vec<(String, String)>,
    /// Aliases that a .luaurc already gives the project; they need no config
    pub luaurc_aliases: Vec<String>,
}

pub fn scan(root: &Path, project: Option<&Project>) -> Detected {
    let mut inputs = Vec::new();
    let mut aliases = Vec::new();
    let existing = luaurc_aliases(root);

    if let Some(p) = project {
        for mount in rojo::mounts(p) {
            let Ok(rel) = mount.fs.strip_prefix(root) else {
                continue;
            };

            match package_alias(rel) {
                /*
                The directory holds dependencies, so it gets an alias and no
                processing, unless `.luaurc` already names the alias.

                larvae reads the `.luaurc` aliases directly. A copy in
                `larvae.toml` keeps the same fact in a second place and
                changes nothing. The two spellings also differ: `.luaurc`
                holds a filesystem path, because an editor can follow one,
                and the copy would put a DataModel path beside it.
                */
                Some(alias) => {
                    let value = format!("@game/{}", mount.dm.join("/"));

                    let known = existing.iter().any(|a| a == alias)
                        || aliases.iter().any(|(a, _): &(String, String)| a == alias);

                    if !known {
                        aliases.push((alias.to_string(), value));
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

    // No mounts found; use the layout that most projects use.
    if inputs.is_empty() && root.join("src").is_dir() {
        inputs.push(PathBuf::from("src"));
    }

    inputs.sort();
    inputs = collapse(inputs);

    Detected {
        inputs,
        aliases,
        luaurc_aliases: existing,
    }
}

/// Alias names that a root .luaurc defines; larvae keeps them unchanged
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

    /*
    A list of every mount alone would silently skip a file added under `src`
    before a mount exists for it. So sibling mounts fold into the directory
    that holds them.
    */
    #[test]
    fn sibling_mounts_collapse_to_the_directory_holding_them() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        write(
            root,
            "default.project.json",
            r#"{ "name": "g", "tree": { "$className": "DataModel",
                 "ReplicatedStorage": { "app": { "$path": "src/shared" } },
                 "ServerScriptService": { "s": { "$path": "src/server" } },
                 "StarterPlayer": { "c": { "$path": "src/client" } } } }"#,
        );

        for d in ["src/shared", "src/server", "src/client"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }

        let project = rojo::load(&root.join("default.project.json")).unwrap();

        assert_eq!(scan(root, Some(&project)).inputs, [PathBuf::from("src")]);
    }

    /// Unrelated roots share only the repository root, and that is not a fold
    #[test]
    fn roots_with_nothing_in_common_stay_separate() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        write(
            root,
            "default.project.json",
            r#"{ "name": "g", "tree": { "$className": "DataModel",
                 "ReplicatedStorage": { "app": { "$path": "src/shared" } },
                 "Workspace": { "a": { "$path": "assets/models" } } } }"#,
        );
        std::fs::create_dir_all(root.join("src/shared")).unwrap();
        std::fs::create_dir_all(root.join("assets/models")).unwrap();

        let project = rojo::load(&root.join("default.project.json")).unwrap();

        assert_eq!(
            scan(root, Some(&project)).inputs,
            [PathBuf::from("assets/models"), PathBuf::from("src/shared")]
        );
    }

    /*
    A package tree is usually mounted one level down, so `packages/roblox` has
    the file name `roblox`. A check of the last component alone proposed the
    dependencies of a project as sources.
    */
    #[test]
    fn a_package_directory_mounted_one_level_down_is_still_a_dependency() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        write(
            root,
            "default.project.json",
            r#"{ "name": "g", "tree": { "$className": "DataModel",
                 "ReplicatedStorage": {
                   "shared":   { "$path": "src/shared" },
                   "Packages": { "$path": "packages/roblox" } } } }"#,
        );
        std::fs::create_dir_all(root.join("src/shared")).unwrap();
        std::fs::create_dir_all(root.join("packages/roblox")).unwrap();

        let project = rojo::load(&root.join("default.project.json")).unwrap();
        let found = scan(root, Some(&project));

        assert_eq!(found.inputs, [PathBuf::from("src/shared")], "not an input");
        assert!(
            found
                .aliases
                .contains(&("pkg".into(), "@game/ReplicatedStorage/Packages".into())),
            "{:?}",
            found.aliases
        );
    }

    /// A repository that lives under a directory named packages must not match
    #[test]
    fn only_the_path_below_the_root_is_checked_for_package_names() {
        assert_eq!(package_alias(Path::new("packages/roblox")), Some("pkg"));
        assert_eq!(package_alias(Path::new("src/shared")), None);
    }

    /*
    larvae reads the `.luaurc` aliases directly. A copy in `larvae.toml` keeps
    the same fact in a second place and changes nothing. The two spellings
    also differ: `.luaurc` holds a filesystem path that an editor can follow,
    and the copy would put a DataModel path beside it.
    */
    #[test]
    fn an_alias_luaurc_already_defines_is_not_proposed_again() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        write(
            root,
            "default.project.json",
            r#"{ "name": "g", "tree": { "$className": "DataModel",
                 "ReplicatedStorage": {
                   "shared":   { "$path": "src/shared" },
                   "Packages": { "$path": "packages/roblox" } } } }"#,
        );
        write(
            root,
            ".luaurc",
            r#"{ "aliases": { "pkg": "packages/roblox" } }"#,
        );
        std::fs::create_dir_all(root.join("src/shared")).unwrap();
        std::fs::create_dir_all(root.join("packages/roblox")).unwrap();

        let project = rojo::load(&root.join("default.project.json")).unwrap();
        let found = scan(root, Some(&project));

        assert!(found.aliases.is_empty(), "{:?}", found.aliases);
        assert_eq!(found.luaurc_aliases, ["pkg"]);
    }

    /// A package directory that `.luaurc` does not name still gets an alias
    #[test]
    fn an_alias_luaurc_does_not_define_is_still_proposed() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        write(
            root,
            "default.project.json",
            r#"{ "name": "g", "tree": { "$className": "DataModel",
                 "ReplicatedStorage": {
                   "shared":   { "$path": "src/shared" },
                   "Packages": { "$path": "packages/roblox" } } } }"#,
        );
        write(
            root,
            ".luaurc",
            r#"{ "aliases": { "shared": "src/shared" } }"#,
        );
        std::fs::create_dir_all(root.join("src/shared")).unwrap();
        std::fs::create_dir_all(root.join("packages/roblox")).unwrap();

        let project = rojo::load(&root.join("default.project.json")).unwrap();
        let found = scan(root, Some(&project));

        assert_eq!(
            found.aliases,
            [(
                "pkg".to_string(),
                "@game/ReplicatedStorage/Packages".to_string()
            )]
        );
    }

    #[test]
    fn a_single_mount_is_left_exactly_as_it_is() {
        assert_eq!(
            collapse(vec![PathBuf::from("src/shared")]),
            [PathBuf::from("src/shared")]
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
