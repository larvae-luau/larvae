//! The [requires] and [rojo] tables control the form of a rewritten require.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::Deserialize;

use super::process::default_true;

/// What a require that crosses the halves of the game costs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CrossRealm {
    /// The run stops on it, which is what a live game would do.
    #[default]
    Deny,
    /// It is emitted as written.
    Allow,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequiresConfig {
    #[serde(default)]
    pub target: Target,

    #[serde(default)]
    pub sourcemap: Option<PathBuf>,

    #[serde(default)]
    pub mounts: HashMap<String, String>,

    #[serde(default)]
    pub strict: bool,

    /*
    Whether a require that crosses the halves of the game stops the run.

    `deny` is the default and the old behaviour: the require compiles and
    then fails in a live game, so larvae refuses to emit it. `allow` emits
    it as written and says nothing.

    Roblox now replicates some containers that the realm rules call server
    only, and a place built that way has requires larvae cannot know are
    correct. So the switch is the project's, and a file that needs the one
    exception writes `-- larvae: allow(cross_realm_require)` on the line
    instead of turning the check off everywhere.
    */
    #[serde(default)]
    pub cross_realm: CrossRealm,

    /*
    Read require(script.Parent.Foo) style requires and rewrite them to the
    configured target. The default is on, because this is the main goal of a
    move from an existing codebase. Off is the escape option for a user who
    wants instance requires kept exactly as written.
    */
    #[serde(default = "default_true")]
    pub instance_input: bool,

    #[serde(default)]
    pub overrides: Option<toml::Value>,

    #[serde(default)]
    pub indexing_style: Option<IndexingStyle>,

    /*
    Write a require inside the StarterPlayer tree as a relative path.

    Roblox clones a Starter container into each player, so the script that
    runs is a copy and a DataModel path names the template it came from. A
    relative require walks the tree the copy sits in, so a script in
    PlayerScripts reaches its siblings after the clone.

    The default is off, because it lowers the floor of a relative walk from
    the mount to the container, and a project that splits StarterPlayerScripts
    over two mounts asked for that split. A shared file that requires client
    code is still an error, with the setting on or off.
    */
    #[serde(default)]
    pub client_relative_requires: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Target {
    #[default]
    RobloxString,
    Path,
    RobloxInstance,
}

/// How the roblox-instance target indexes children in emitted expressions
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IndexingStyle {
    /// `script.Parent:FindFirstChild("x")`
    #[default]
    #[serde(alias = "find-first-child")]
    FindFirstChild,

    /// `script.Parent:WaitForChild("x")`
    #[serde(alias = "wait-for-child")]
    WaitForChild,

    /// `script.Parent.x` (the dot format)
    #[serde(alias = "property-instance", alias = "property_instance")]
    Property,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RojoConfig {
    #[serde(default)]
    pub project: Option<PathBuf>,

    #[serde(default)]
    pub build_project: Option<PathBuf>,
}

impl Default for RequiresConfig {
    fn default() -> Self {
        Self {
            target: Target::default(),
            sourcemap: None,
            mounts: HashMap::new(),
            strict: false,
            cross_realm: CrossRealm::default(),
            instance_input: true,
            overrides: None,
            indexing_style: None,
            client_relative_requires: false,
        }
    }
}
