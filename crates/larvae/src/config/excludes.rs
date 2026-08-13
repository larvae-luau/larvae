/*!
The paths that a config tells larvae to skip.

`[fmt]`, `[lint]`, and the file of selene all use the same form: a list of
globs relative to the project root. So the match logic lives here once and
not three times.

A match on any directory in the path counts. So a directory name excludes the
content under it, with or without a wildcard at the end of the pattern. The
user does not have to remember one specific spelling.

Excludes do not cover a file that the user named on the command line. A name
is an instruction from the user. A formatter that does nothing to a named
file, without a message, is worse than one that formats an excluded file.
Excludes apply only to the files that a walk finds.
*/

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use globset::{Glob, GlobSet, GlobSetBuilder};

#[derive(Debug, Clone, Default)]
pub struct Excludes {
    root: PathBuf,
    /// None when the config excludes nothing; this is the usual case and skips the work
    set: Option<GlobSet>,
}

impl Excludes {
    pub fn new(root: &Path, patterns: &[String]) -> Result<Self> {
        if patterns.is_empty() {
            return Ok(Self::default());
        }

        let mut builder = GlobSetBuilder::new();

        for pattern in patterns {
            builder.add(
                Glob::new(pattern)
                    .with_context(|| format!("exclude \"{pattern}\" is not a glob"))?,
            );
        }

        Ok(Self {
            root: root.to_path_buf(),
            set: Some(builder.build()?),
        })
    }

    /// True when the project tells larvae to skip this path
    pub fn skips(&self, path: &Path) -> bool {
        let Some(set) = &self.set else {
            return false;
        };

        let rel = path.strip_prefix(&self.root).unwrap_or(path);

        rel.ancestors()
            .filter(|a| !a.as_os_str().is_empty())
            .any(|a| set.is_match(a))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn excludes(patterns: &[&str]) -> Excludes {
        let owned: Vec<String> = patterns.iter().map(|p| p.to_string()).collect();

        Excludes::new(Path::new("/project"), &owned).unwrap()
    }

    #[test]
    fn nothing_is_excluded_by_default() {
        assert!(!Excludes::default().skips(Path::new("/project/src/init.luau")));
        assert!(!excludes(&[]).skips(Path::new("/project/src/init.luau")));
    }

    #[test]
    fn a_glob_matches_the_path_below_the_root() {
        let e = excludes(&["Packages/**", "**/*.spec.luau"]);

        assert!(e.skips(Path::new("/project/Packages/signal/init.luau")));
        assert!(e.skips(Path::new("/project/src/thing.spec.luau")));
        assert!(!e.skips(Path::new("/project/src/thing.luau")));
    }

    /// A directory name must be enough; users forget the /** part.
    #[test]
    fn naming_a_directory_excludes_what_is_under_it() {
        let e = excludes(&["Packages", "src/vendor"]);

        assert!(e.skips(Path::new("/project/Packages/signal/init.luau")));
        assert!(e.skips(Path::new("/project/src/vendor/a.luau")));
        assert!(!e.skips(Path::new("/project/src/vendored.luau")));
    }

    /// A path outside the root matches as written and does not pass without a
    /// check. The alternative is a format of an excluded file.
    #[test]
    fn a_path_outside_the_root_still_matches_on_its_own_terms() {
        assert!(excludes(&["**/vendor/**"]).skips(Path::new("/elsewhere/vendor/a.luau")));
    }

    #[test]
    fn a_pattern_that_is_not_a_glob_is_an_error() {
        assert!(Excludes::new(Path::new("/project"), &["a[".to_string()]).is_err());
    }
}
