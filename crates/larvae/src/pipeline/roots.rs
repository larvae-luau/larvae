/*!
Input roots

One root is the common case, and it flattens: `src/a.luau` lands at
`dist/a.luau`. Every existing project expects this layout.

More than one root cannot flatten; two roots would collide when both hold an
`init.luau`. So each root keeps its own directory: `src/a.luau` becomes
`dist/src/a.luau`, and `vendor/x.luau` becomes `dist/vendor/x.luau`. This is
the one place where the output layout depends on the root count, so the rule
is written here in plain words.
*/

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::config::Config;

/// One input directory and the location of its files under the output
pub struct Root {
    /// Absolute path on disk
    pub dir: PathBuf,
    /// The prefix under the output directory; empty for a single root
    pub prefix: PathBuf,
}

/// Resolve the configured roots against the project root
pub fn resolve(project_root: &Path, config: &Config) -> Result<Vec<Root>> {
    let configured = config.process.inputs();
    let many = configured.len() > 1;
    let mut out = Vec::new();

    for rel in configured {
        let dir = project_root.join(&rel);

        if !dir.is_dir() {
            anyhow::bail!(
                "input directory {} does not exist (set [process].input in larvae.toml)",
                crate::ui::rel(&dir)
            );
        }

        out.push(Root {
            dir,
            // The prefix keeps the path of the root, and this stops a collision.
            prefix: if many { rel.clone() } else { PathBuf::new() },
        });
    }

    Ok(out)
}

impl Root {
    /// The destination of a file under this root, relative to the output directory
    pub fn dest(&self, path: &Path) -> Result<PathBuf> {
        let rel = path
            .strip_prefix(&self.dir)
            .with_context(|| format!("{} is not under {}", path.display(), self.dir.display()))?;

        Ok(self.prefix.join(rel))
    }
}

/// The root that owns a discovered file; the deepest root wins, so nested roots work
pub fn owner<'a>(roots: &'a [Root], path: &Path) -> Option<&'a Root> {
    roots
        .iter()
        .filter(|r| path.starts_with(&r.dir))
        .max_by_key(|r| r.dir.components().count())
}

/// The output relative path for a file, or None when no root covers it
pub fn dest_of(roots: &[Root], path: &Path) -> Option<PathBuf> {
    owner(roots, path).and_then(|r| r.dest(path).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roots(dirs: &[&str], many: bool) -> Vec<Root> {
        dirs.iter()
            .map(|d| Root {
                dir: PathBuf::from("/proj").join(d),
                prefix: if many {
                    PathBuf::from(d)
                } else {
                    PathBuf::new()
                },
            })
            .collect()
    }

    #[test]
    fn one_root_flattens() {
        let r = roots(&["src"], false);

        assert_eq!(
            dest_of(&r, Path::new("/proj/src/a/b.luau")).unwrap(),
            Path::new("a/b.luau")
        );
    }

    #[test]
    fn many_roots_keep_their_own_directory() {
        let r = roots(&["src", "vendor"], true);

        assert_eq!(
            dest_of(&r, Path::new("/proj/src/init.luau")).unwrap(),
            Path::new("src/init.luau")
        );
        assert_eq!(
            dest_of(&r, Path::new("/proj/vendor/init.luau")).unwrap(),
            Path::new("vendor/init.luau")
        );
    }

    #[test]
    fn a_nested_root_wins_over_the_one_above_it() {
        let r = roots(&["src", "src/vendor"], true);
        let owner = owner(&r, Path::new("/proj/src/vendor/x.luau")).unwrap();

        assert_eq!(owner.dir, Path::new("/proj/src/vendor"));
    }

    #[test]
    fn a_file_under_no_root_has_no_destination() {
        let r = roots(&["src"], false);

        assert!(dest_of(&r, Path::new("/proj/elsewhere/x.luau")).is_none());
    }
}
