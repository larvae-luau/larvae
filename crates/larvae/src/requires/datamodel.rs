//! Filesystem to DataModel mapping, plus the container rules: realms and Starter cloning

use std::path::{Component, Path, PathBuf};

/// One filesystem directory mounted at a DataModel location
#[derive(Debug, Clone)]
pub struct Mount {
    /// Absolute filesystem path of the mounted directory
    pub fs: PathBuf,
    /// DataModel path segments from the root (ex: ["ReplicatedStorage", "shared"])
    pub dm: Vec<String>,
}

#[derive(Debug, Default)]
pub struct MountTable {
    /// Sorted deepest first, so a lookup finds the most specific mount
    mounts: Vec<Mount>,
    /*
    The answer from rojo, when the project supplies one. It wins over the
    mounts, because it is what rojo built and not what larvae inferred from
    the project file.
    */
    map: crate::project::sourcemap::SourceMap,
}

/// Replication/runtime classification of a DataModel location
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Realm {
    ServerOnly,
    /// Starter containers run as clones; an absolute @game path into them reaches the template
    StarterClone,
    Shared,
}

pub fn realm_of_container(service: &str) -> Realm {
    match service {
        "ServerScriptService" | "ServerStorage" => Realm::ServerOnly,

        "StarterPlayer" | "StarterGui" | "StarterPack" => Realm::StarterClone,

        _ => Realm::Shared,
    }
}

/// A file's position in the DataModel
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DmPath {
    /// Segments from the DataModel root to this node (inclusive)
    pub segments: Vec<String>,
    /// The count of leading segments from the mount; relative emission stays below this
    pub mount_depth: usize,
    /// The mount that produced this path (an index into the table)
    pub mount: usize,
}

impl DmPath {
    pub fn service(&self) -> &str {
        &self.segments[0]
    }

    pub fn realm(&self) -> Realm {
        realm_of_container(self.service())
    }

    pub fn game_path(&self) -> String {
        format!("@game/{}", self.segments.join("/"))
    }
}

impl MountTable {
    pub fn new(mut mounts: Vec<Mount>) -> Self {
        mounts.sort_by_key(|m| std::cmp::Reverse(m.fs.components().count()));

        Self {
            mounts,
            map: Default::default(),
        }
    }

    /// Use the rojo sourcemap as the authority; mounts cover what it misses
    pub fn with_sourcemap(mut self, map: crate::project::sourcemap::SourceMap) -> Self {
        self.map = map;

        self
    }

    /// True when no mounts and no sourcemap describe the tree
    pub fn is_empty(&self) -> bool {
        self.mounts.is_empty() && self.map.is_empty()
    }

    pub fn mounts(&self) -> &[Mount] {
        &self.mounts
    }

    /*
    The other direction: the disk location of a DataModel path. An instance
    require arrives as a datamodel path and must map back to a file before
    larvae can emit it again. The most specific mount wins, because a deeper
    mount is the more precise answer.
    */
    pub fn fs_of(&self, segments: &[String]) -> Option<PathBuf> {
        if let Some(base) = self.map.fs_of(segments) {
            return Some(base.to_path_buf());
        }

        let mount = self
            .mounts
            .iter()
            .filter(|m| segments.len() >= m.dm.len() && segments[..m.dm.len()] == m.dm[..])
            .max_by_key(|m| m.dm.len())?;

        let mut fs = mount.fs.clone();

        for seg in &segments[mount.dm.len()..] {
            fs.push(seg);
        }

        Some(fs)
    }

    /**
    Map a resolved module path to its DataModel node. The map removes
    extensions and .server/.client markers, and an init file collapses into
    its directory.
    */
    pub fn dm_of(&self, fs_path: &Path) -> Option<DmPath> {
        if let Some(segments) = self.map.dm_of(fs_path) {
            return Some(DmPath {
                segments: segments.to_vec(),
                /*
                Every node in a sourcemap has a file behind it, so the only
                floor for a relative require is the service itself. One map
                covers the whole tree, so all nodes share a mount.
                */
                mount_depth: 1,
                mount: usize::MAX,
            });
        }

        let (idx, mount) = self
            .mounts
            .iter()
            .enumerate()
            .find(|(_, m)| fs_path.starts_with(&m.fs))?;
        let rel = fs_path.strip_prefix(&mount.fs).ok()?;

        let mut segments = mount.dm.clone();
        let mount_depth = segments.len();

        let comps: Vec<&str> = rel
            .components()
            .filter_map(|c| match c {
                Component::Normal(s) => s.to_str(),

                _ => None,
            })
            .collect();

        for (i, comp) in comps.iter().enumerate() {
            let is_last = i + 1 == comps.len();

            if !is_last {
                segments.push((*comp).to_string());
                continue;
            }

            // The final component: apply the script naming rules if it is a file.
            if fs_path.is_dir() {
                segments.push((*comp).to_string());
            } else {
                match script_instance_name(comp) {
                    // init.* collapses into the parent directory node.
                    None => {}
                    Some(name) => segments.push(name.to_string()),
                }
            }
        }

        if segments.is_empty() {
            return None; // The mount root itself, with an init collapse above it.
        }

        Some(DmPath {
            segments,
            mount_depth,
            mount: idx,
        })
    }
}

/// The instance name for a script file: foo.server.luau becomes foo, and init files return None
pub fn script_instance_name(file_name: &str) -> Option<&str> {
    let stem = file_name
        .strip_suffix(".luau")
        .or_else(|| file_name.strip_suffix(".lua"))
        .unwrap_or(file_name);
    let stem = stem
        .strip_suffix(".server")
        .or_else(|| stem.strip_suffix(".client"))
        .unwrap_or(stem);
    if stem == "init" { None } else { Some(stem) }
}

/// The script kind from a file name (the realm checks use it)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScriptKind {
    Server,
    Client,
    Module,
}

pub fn script_kind(file_name: &str) -> ScriptKind {
    let stem = file_name
        .strip_suffix(".luau")
        .or_else(|| file_name.strip_suffix(".lua"))
        .unwrap_or(file_name);
    if stem.ends_with(".server") {
        ScriptKind::Server
    } else if stem.ends_with(".client") {
        ScriptKind::Client
    } else {
        ScriptKind::Module
    }
}

/// Parse a `[requires.mounts]`-style DataModel value, `@game/A/B` -> ["A","B"]
pub fn parse_game_path(value: &str) -> Option<Vec<String>> {
    let rest = value
        .strip_prefix("@game/")
        .or_else(|| (value == "@game").then_some(""))?;
    let segs: Vec<String> = rest
        .split('/')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    Some(segs)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table() -> MountTable {
        MountTable::new(vec![
            Mount {
                fs: "/proj/src/shared".into(),
                dm: vec!["ReplicatedStorage".into(), "shared".into()],
            },
            Mount {
                fs: "/proj/src/server".into(),
                dm: vec!["ServerScriptService".into()],
            },
            Mount {
                fs: "/proj/Packages".into(),
                dm: vec!["ReplicatedStorage".into(), "Packages".into()],
            },
        ])
    }

    #[test]
    fn maps_with_naming_rules() {
        let t = table();
        let dm = t
            .dm_of(Path::new("/proj/src/shared/util/math.luau"))
            .unwrap();
        assert_eq!(dm.segments, ["ReplicatedStorage", "shared", "util", "math"]);
        assert_eq!(dm.mount_depth, 2);
        assert_eq!(dm.game_path(), "@game/ReplicatedStorage/shared/util/math");

        let dm = t
            .dm_of(Path::new("/proj/src/server/main.server.luau"))
            .unwrap();
        assert_eq!(dm.segments, ["ServerScriptService", "main"]);
        assert_eq!(dm.realm(), Realm::ServerOnly);
    }

    #[test]
    fn init_collapses_into_dir() {
        let t = table();
        let dm = t
            .dm_of(Path::new("/proj/src/shared/pkg/init.luau"))
            .unwrap();
        assert_eq!(dm.segments, ["ReplicatedStorage", "shared", "pkg"]);
    }

    #[test]
    fn deepest_mount_wins() {
        let t = MountTable::new(vec![
            Mount {
                fs: "/proj/src".into(),
                dm: vec!["ReplicatedStorage".into()],
            },
            Mount {
                fs: "/proj/src/server".into(),
                dm: vec!["ServerScriptService".into()],
            },
        ]);
        let dm = t.dm_of(Path::new("/proj/src/server/x.luau")).unwrap();
        assert_eq!(dm.segments, ["ServerScriptService", "x"]);
    }

    #[test]
    fn unmounted_is_none() {
        assert!(table().dm_of(Path::new("/elsewhere/x.luau")).is_none());
    }

    #[test]
    fn realms() {
        assert_eq!(realm_of_container("ServerStorage"), Realm::ServerOnly);
        assert_eq!(realm_of_container("StarterGui"), Realm::StarterClone);
        assert_eq!(realm_of_container("ReplicatedStorage"), Realm::Shared);
    }

    #[test]
    fn game_path_parsing() {
        assert_eq!(
            parse_game_path("@game/ReplicatedStorage/packages").unwrap(),
            vec!["ReplicatedStorage".to_string(), "packages".into()]
        );
        assert!(parse_game_path("./relative").is_none());
    }
}
