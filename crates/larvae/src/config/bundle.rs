/*!
`[bundle]`, single file output.
*/

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub struct BundleConfig {
    /// The module the bundle starts from, relative to the project root
    #[serde(default)]
    pub entry: Option<PathBuf>,

    /// Where the single file goes, relative to the project root
    #[serde(default)]
    pub output: Option<PathBuf>,

    /*
    Drop the modules that the entry cannot reach.

    On by default, because it is the reason to bundle at all: a bundle of
    everything in the project is larger than the project. Reachability comes
    from the require graph, so a module reached only through a dynamic
    require would be dropped. That is why a dynamic require in a bundled
    module is reported and not ignored, and why a project that needs those
    modules turns this off.
    */
    #[serde(default = "yes")]
    pub tree_shake: bool,

    /*
    What the registry calls each module.

    A path id reads well in the output and in the two runtime errors that
    name a module. It also ships the whole project layout: every file,
    every package, every vendor directory, spelled out in a table key that
    anyone who can read the bundle can read. `opaque` numbers the modules
    instead, so the output names no file and no package.

    Numbers, not hashes, on purpose. A hash of a path is deterministic, so
    a list of likely paths recovers the names by hashing candidates. A
    number carries nothing to recover. The numbering follows the sorted
    paths, so the same project bundles to the same bytes.
    */
    #[serde(default)]
    pub module_ids: ModuleIds,
}

/// How `[bundle] module_ids` names the modules of the registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ModuleIds {
    /// The project relative path, readable where an error names a module
    #[default]
    Paths,

    /// A number with no meaning, so the bundle names no file and no package
    Opaque,
}

fn yes() -> bool {
    true
}

/*
Through serde and not derived, so `tree_shake` keeps the default its
attribute names. A derived `Default` gives `false` for a bool and quietly
disagrees with the config file, which only shows up as a bundle that nobody
shrank.
*/
impl Default for BundleConfig {
    fn default() -> Self {
        toml::from_str("").expect("every field has a default")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tree_shaking_is_on_and_the_paths_are_unset_by_default() {
        let c = BundleConfig::default();

        assert!(c.tree_shake);
        assert_eq!(c.entry, None);
        assert_eq!(c.output, None);
        assert_eq!(c.module_ids, ModuleIds::Paths);
    }

    #[test]
    fn module_ids_reads_both_spellings_and_refuses_a_third() {
        let c: BundleConfig = toml::from_str("module_ids = \"opaque\"").unwrap();

        assert_eq!(c.module_ids, ModuleIds::Opaque);
        assert!(toml::from_str::<BundleConfig>("module_ids = \"hashed\"").is_err());
    }

    #[test]
    fn the_paths_are_read_as_written() {
        let c: BundleConfig =
            toml::from_str("entry = \"src/main.luau\"\noutput = \"dist/bundle.luau\"").unwrap();

        assert_eq!(c.entry, Some(PathBuf::from("src/main.luau")));
        assert_eq!(c.output, Some(PathBuf::from("dist/bundle.luau")));
    }

    #[test]
    fn an_unknown_key_is_refused_like_everywhere_else() {
        assert!(toml::from_str::<BundleConfig>("whoops = true").is_err());
    }
}
