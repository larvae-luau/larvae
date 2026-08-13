/*!
Rojo sourcemaps

A sourcemap is the answer from rojo about which file is which instance. So
it settles the cases that a static read of the project file cannot: a
`$path` that points at another project file, a model file that brings its
own subtree, globs, and all data that rojo computes and does not state.

A sourcemap is a generated artifact, so it is a fallback and not the normal
path. Auto mounts from the project file cover most projects, and they need
no synchronization.
*/

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct Node {
    name: String,
    #[serde(default)]
    #[serde(rename = "filePaths")]
    file_paths: Vec<PathBuf>,
    #[serde(default)]
    children: Vec<Node>,
}

/// Both directions of the file to instance mapping, with constant time lookup
#[derive(Debug, Default)]
pub struct SourceMap {
    by_fs: HashMap<PathBuf, Vec<String>>,
    /// Instance path to the extensionless base that a require must resolve from
    by_dm: HashMap<Vec<String>, PathBuf>,
}

pub fn load(path: &Path, project_root: &Path) -> Result<SourceMap> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", crate::ui::rel(path)))?;
    let root: Node = serde_json::from_str(&text)
        .with_context(|| format!("invalid sourcemap in {}", crate::ui::rel(path)))?;

    let mut map = SourceMap::default();

    // The root node is the DataModel; segments start below it.
    for child in &root.children {
        walk(child, &mut Vec::new(), project_root, &mut map);
    }

    Ok(map)
}

fn walk(node: &Node, segments: &mut Vec<String>, project_root: &Path, map: &mut SourceMap) {
    segments.push(node.name.clone());

    /*
    A node can list several paths, ex: a script next to its meta.json. A
    require can reach only the script, so the walk skips the other paths.
    */
    if let Some(file) = node.file_paths.iter().find(|p| is_module(p)) {
        let abs = project_root.join(file);

        map.by_fs.insert(abs.clone(), segments.clone());
        map.by_dm
            .entry(segments.clone())
            .or_insert_with(|| base_of(&abs));
    }

    for child in &node.children {
        walk(child, segments, project_root, map);
    }

    segments.pop();
}

fn is_module(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("luau") | Some("lua")
    )
}

/*
The extensionless path that a require resolves from; the rest of the
resolver works in this form. An init file stands for its directory, so the
directory is the base and not the file.
*/
fn base_of(file: &Path) -> PathBuf {
    let stem = file
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default();

    if stem == "init" || stem == "init.server" || stem == "init.client" {
        return file.parent().unwrap_or(file).to_path_buf();
    }

    file.with_extension("")
}

impl SourceMap {
    pub fn is_empty(&self) -> bool {
        self.by_fs.is_empty()
    }

    /// The instance path of a file, when the sourcemap knows it
    pub fn dm_of(&self, path: &Path) -> Option<&[String]> {
        self.by_fs.get(path).map(|v| v.as_slice())
    }

    /// The extensionless base that an instance path resolves from
    pub fn fs_of(&self, segments: &[String]) -> Option<&Path> {
        self.by_dm.get(segments).map(|p| p.as_path())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
      "name": "game",
      "className": "DataModel",
      "children": [
        { "name": "ReplicatedStorage", "className": "ReplicatedStorage", "children": [
          { "name": "shared", "className": "Folder", "children": [
            { "name": "util", "className": "ModuleScript",
              "filePaths": ["src/shared/util.luau", "src/shared/util.meta.json"] },
            { "name": "pkg", "className": "ModuleScript",
              "filePaths": ["src/shared/pkg/init.luau"] }
          ]}
        ]}
      ]
    }"#;

    fn sample() -> SourceMap {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sourcemap.json");
        std::fs::write(&path, SAMPLE).unwrap();

        load(&path, Path::new("/proj")).unwrap()
    }

    #[test]
    fn maps_a_file_to_its_instance_path() {
        let map = sample();

        assert_eq!(
            map.dm_of(Path::new("/proj/src/shared/util.luau")).unwrap(),
            ["ReplicatedStorage", "shared", "util"]
        );
    }

    #[test]
    fn maps_an_instance_path_back_to_an_extensionless_base() {
        let map = sample();
        let segments = ["ReplicatedStorage", "shared", "util"].map(String::from);

        assert_eq!(
            map.fs_of(&segments).unwrap(),
            Path::new("/proj/src/shared/util")
        );
    }

    #[test]
    fn an_init_file_stands_for_its_directory() {
        let map = sample();
        let segments = ["ReplicatedStorage", "shared", "pkg"].map(String::from);

        assert_eq!(
            map.fs_of(&segments).unwrap(),
            Path::new("/proj/src/shared/pkg")
        );
    }

    #[test]
    fn a_meta_json_beside_a_script_is_ignored() {
        let map = sample();

        assert!(
            map.dm_of(Path::new("/proj/src/shared/util.meta.json"))
                .is_none()
        );
    }
}
