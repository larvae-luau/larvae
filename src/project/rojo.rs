/*!
Rojo project integration, derives the mount table from default.project.json
and writes the derived build project, $paths under input remapped to output
and rerelativized since Rojo resolves them from the project file's own dir
*/

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde_json::Value;

use crate::requires::datamodel::Mount;

pub struct Project {
    pub path: PathBuf,
    /// Directory the project file lives in (base for its relative $paths)
    pub dir: PathBuf,
    pub json: Value,
    /// False for model tree projects, @game output means nothing there
    pub is_datamodel: bool,
}

pub fn find_project(root: &Path, configured: Option<&Path>) -> Option<PathBuf> {
    if let Some(p) = configured {
        return Some(root.join(p));
    }

    let default = root.join("default.project.json");

    default.exists().then_some(default)
}

pub fn load(path: &Path) -> Result<Project> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", crate::ui::rel(path)))?;
    let json: Value = serde_json::from_str(&text)
        .with_context(|| format!("invalid JSON in {}", crate::ui::rel(path)))?;
    let tree = json
        .get("tree")
        .with_context(|| format!("{}: missing \"tree\"", crate::ui::rel(path)))?;
    let is_datamodel = tree
        .get("$className")
        .and_then(Value::as_str)
        .map(|c| c == "DataModel")
        // service children with no $className acts as a DataModel, a root $path means model project
        .unwrap_or_else(|| tree.get("$path").is_none());
    let dir = path.parent().unwrap_or(Path::new("")).to_owned();
    Ok(Project {
        path: path.to_owned(),
        dir,
        json,
        is_datamodel,
    })
}

/// Derive mounts from the project tree
pub fn mounts(project: &Project) -> Vec<Mount> {
    let mut out = Vec::new();

    if let Some(tree) = project.json.get("tree") {
        walk_mounts(
            tree,
            &mut Vec::new(),
            &project.dir,
            project.is_datamodel,
            &mut out,
        );
    }

    out
}

fn walk_mounts(
    node: &Value,
    dm_path: &mut Vec<String>,
    project_dir: &Path,
    is_datamodel: bool,
    out: &mut Vec<Mount>,
) {
    let Some(obj) = node.as_object() else { return };

    if let Some(path_value) = obj.get("$path") {
        let fs_rel = match path_value {
            Value::String(s) => Some(s.as_str()),

            Value::Object(o) => o.get("optional").and_then(Value::as_str),

            _ => None,
        };

        if let (Some(fs_rel), false) = (fs_rel, dm_path.is_empty() && is_datamodel) {
            let fs = normalize(&project_dir.join(fs_rel));
            // nested project files are skipped as mounts, copied verbatim with a warning
            if !fs_rel.ends_with(".project.json") {
                out.push(Mount {
                    fs,
                    dm: dm_path.clone(),
                });
            }
        }
    }

    for (key, child) in obj {
        if key.starts_with('$') {
            continue;
        }

        dm_path.push(key.clone());
        walk_mounts(child, dm_path, project_dir, is_datamodel, out);
        dm_path.pop();
    }
}

/// Build the derived project JSON, paths rerelativized against build_dir
pub fn derive_build_project(
    project: &Project,
    input: &Path,
    output: &Path,
    build_dir: &Path,
    warnings: &mut Vec<String>,
) -> Value {
    let mut json = project.json.clone();

    if let Some(name) = json
        .get_mut("name")
        .and_then(|v| v.as_str().map(str::to_owned))
    {
        json["name"] = Value::String(name); // keep name as is
    }

    if let Some(tree) = json.get_mut("tree") {
        rewrite_paths(tree, &project.dir, input, output, build_dir, warnings);
    }

    json
}

fn rewrite_paths(
    node: &mut Value,
    project_dir: &Path,
    input: &Path,
    output: &Path,
    build_dir: &Path,
    warnings: &mut Vec<String>,
) {
    let Some(obj) = node.as_object_mut() else {
        return;
    };

    for (key, child) in obj.iter_mut() {
        if key == "$path" {
            rewrite_one_path(child, project_dir, input, output, build_dir, warnings);
        } else if !key.starts_with('$') {
            rewrite_paths(child, project_dir, input, output, build_dir, warnings);
        }
    }
}

fn rewrite_one_path(
    value: &mut Value,
    project_dir: &Path,
    input: &Path,
    output: &Path,
    build_dir: &Path,
    warnings: &mut Vec<String>,
) {
    let mut rewrite = |s: &str| -> String {
        if s.ends_with(".project.json") {
            warnings.push(format!(
                "$path \"{s}\" points at another project file; copied verbatim - nested projects get first-class support post-M2"
            ));
        }

        let abs = normalize(&project_dir.join(s));
        let mapped = match abs.strip_prefix(input) {
            Ok(rel) => output.join(rel),

            Err(_) => abs,
        };

        relative_to(build_dir, &mapped)
    };

    match value {
        Value::String(s) => *value = Value::String(rewrite(s)),

        Value::Object(o) => {
            if let Some(Value::String(s)) = o.get("optional") {
                let new = rewrite(s);
                o.insert("optional".into(), Value::String(new));
            }
        }

        _ => {}
    }
}

fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();

    for c in p.components() {
        match c {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }

            other => out.push(other),
        }
    }

    out
}

fn relative_to(from_dir: &Path, to: &Path) -> String {
    let from: Vec<_> = from_dir.components().collect();
    let to_c: Vec<_> = to.components().collect();
    let common = from.iter().zip(&to_c).take_while(|(a, b)| a == b).count();

    let mut parts: Vec<String> =
        std::iter::repeat_n("..".to_string(), from.len() - common).collect();
    parts.extend(
        to_c[common..]
            .iter()
            .map(|c| c.as_os_str().to_string_lossy().into_owned()),
    );

    if parts.is_empty() {
        ".".to_string()
    } else {
        parts.join("/")
    }
}

/// Write the derived project, returns its path
pub fn write_build_project(
    project: &Project,
    input: &Path,
    output: &Path,
    cache_dir: &Path,
    configured_out: Option<&Path>,
) -> Result<(PathBuf, Vec<String>)> {
    let build_path = configured_out
        .map(Path::to_owned)
        .unwrap_or_else(|| cache_dir.join("build.project.json"));
    let build_dir = build_path.parent().unwrap_or(Path::new(".")).to_owned();
    std::fs::create_dir_all(&build_dir)
        .with_context(|| format!("failed to create {}", crate::ui::rel(&build_dir)))?;

    if !project.is_datamodel {
        bail!(
            "{} is a model-tree project (root has $path / non-DataModel $className); serve/build derivation only supports place projects",
            crate::ui::rel(&project.path)
        );
    }

    let mut warnings = Vec::new();
    let json = derive_build_project(project, input, output, &build_dir, &mut warnings);
    let text = serde_json::to_string_pretty(&json)?;
    let tmp = build_path.with_extension("json.tmp");

    std::fs::write(&tmp, &text)?;
    std::fs::rename(&tmp, &build_path)?;

    Ok((build_path, warnings))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(json: &str, dir: &str) -> Project {
        Project {
            path: PathBuf::from(dir).join("default.project.json"),
            dir: dir.into(),
            json: serde_json::from_str(json).unwrap(),
            is_datamodel: true,
        }
    }

    const SAMPLE: &str = r#"{
        "name": "game",
        "tree": {
            "$className": "DataModel",
            "ReplicatedStorage": {
                "shared": { "$path": "src/shared" },
                "Packages": { "$path": "Packages" }
            },

            "ServerScriptService": { "$path": "src/server" }
        }
    }"#;

    #[test]
    fn derives_mounts() {
        let p = project(SAMPLE, "/proj");
        let m = mounts(&p);
        let find = |fs: &str| m.iter().find(|m| m.fs == Path::new(fs)).unwrap();

        assert_eq!(find("/proj/src/shared").dm, ["ReplicatedStorage", "shared"]);
        assert_eq!(find("/proj/Packages").dm, ["ReplicatedStorage", "Packages"]);
        assert_eq!(find("/proj/src/server").dm, ["ServerScriptService"]);
    }

    #[test]
    fn derives_build_project() {
        let p = project(SAMPLE, "/proj");
        let mut warnings = Vec::new();

        let out = derive_build_project(
            &p,
            Path::new("/proj/src"),
            Path::new("/proj/dist"),
            Path::new("/proj/.larvae"),
            &mut warnings,
        );
        let tree = &out["tree"];
        // src/** remapped to dist/** and rerelativized against .larvae/
        assert_eq!(
            tree["ReplicatedStorage"]["shared"]["$path"],
            "../dist/shared"
        );
        assert_eq!(tree["ServerScriptService"]["$path"], "../dist/server");
        // Outside the input root, untouched source location, rerelativized
        assert_eq!(
            tree["ReplicatedStorage"]["Packages"]["$path"],
            "../Packages"
        );
        assert!(warnings.is_empty());
    }

    #[test]
    fn model_project_detected() {
        let p: Project = load_from_str(r#"{ "name": "lib", "tree": { "$path": "src" } }"#, "/proj");
        assert!(!p.is_datamodel);
    }

    fn load_from_str(json: &str, dir: &str) -> Project {
        let v: Value = serde_json::from_str(json).unwrap();
        let is_datamodel = v["tree"]
            .get("$className")
            .and_then(Value::as_str)
            .map(|c| c == "DataModel")
            .unwrap_or_else(|| v["tree"].get("$path").is_none());
        Project {
            path: PathBuf::from(dir).join("default.project.json"),
            dir: dir.into(),
            json: v,
            is_datamodel,
        }
    }
}
