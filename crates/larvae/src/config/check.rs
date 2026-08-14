/*!
`[check]`, the CI gate.

Every question here is a whole project question, not a per file one. That is
why it lives behind its own command and its own table: a cycle is a property
of the graph, and no module can see from inside itself that nothing requires
it.

Each check is a level, not a boolean, to match `[lint.rules]`. So a project
decides in one place what fails CI. `larvae check` reports unresolvable
requires and realm violations unconditionally, because those are wrong
rather than untidy.
*/

use serde::{Deserialize, Serialize};

pub use crate::lint::Level;

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub struct CheckConfig {
    /*
    Modules that require each other, directly or through a chain.

    The default is a warning, not an error. A cyclic require is legal at
    runtime and common in Roblox code: a lazy require inside a function
    breaks the load order cycle on purpose. A build failure on one would
    reject working projects, so a team that decides against them raises the
    level.
    */
    #[serde(default = "warn")]
    pub cycles: Level,

    /*
    Modules that nothing requires.

    Off by default, because larvae cannot always know what runs a module. A
    Script or a LocalScript is run by Roblox, not required, and is never
    reported, which covers most files. But a module reached only through a
    dynamic require, or from outside the project, would be a false positive,
    and a report that makes a user delete working code is worse than
    silence.
    */
    #[serde(default = "allow")]
    pub unused_modules: Level,

    /*
    Client scripts that require through an indexing style that does not
    wait.

    A LocalScript can run before the instances it needs have replicated. So
    a require emitted as `Parent.Thing` or `FindFirstChild("Thing")` is a
    race: it works on a fast join and returns nil on a slow one. The
    `wait_for_child` style closes the race.

    The check has meaning only for the `roblox-instance` target, because
    the other targets do not index anything. It reports once per file, not
    once per require site, because the fix is one config line.
    */
    #[serde(default = "warn")]
    pub early_require: Level,

    /*
    Extra entry points, for the modules that `unused_modules` cannot infer.

    The paths are relative to the project root. A module in this list counts
    as one the project runs, so no incoming edge is not a finding for it.
    */
    #[serde(default)]
    pub entries: Vec<std::path::PathBuf>,
}

fn warn() -> Level {
    Level::Warn
}

fn allow() -> Level {
    Level::Allow
}

impl Default for CheckConfig {
    fn default() -> Self {
        toml::from_str("").expect("every field has a default")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cycles_warn_and_unused_modules_stay_quiet_by_default() {
        let c = CheckConfig::default();

        assert_eq!(c.cycles, Level::Warn);
        assert_eq!(c.unused_modules, Level::Allow);
        assert_eq!(c.early_require, Level::Warn);
        assert!(c.entries.is_empty());
    }

    #[test]
    fn a_project_can_raise_either_to_deny() {
        let c: CheckConfig =
            toml::from_str("cycles = \"deny\"\nunused_modules = \"deny\"").unwrap();

        assert_eq!(c.cycles, Level::Deny);
        assert_eq!(c.unused_modules, Level::Deny);
    }

    #[test]
    fn entries_are_read_as_paths() {
        let c: CheckConfig = toml::from_str("entries = [\"src/boot.luau\"]").unwrap();

        assert_eq!(c.entries, [std::path::PathBuf::from("src/boot.luau")]);
    }

    #[test]
    fn an_unknown_key_is_refused_like_everywhere_else() {
        assert!(toml::from_str::<CheckConfig>("whoops = true").is_err());
    }
}
