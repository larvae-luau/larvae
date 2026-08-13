/*!
The `[lint]` configuration, and the `selene.toml` file that a project can already have.

This follows the same rule as the formatter's config. A project that already
lints with selene can point larvae at its settings. Thus larvae reads
`selene.toml` directly, and `[lint]` in `larvae.toml` overrides it. selene's
names also work in `[lint]`: `config` for the per-lint table, and selene's
`std` spellings. This lets the user move the file as one unit, not key by key.

larvae ignores an unknown key in `selene.toml`, because that file belongs to
selene. An unknown key in `larvae.toml` is an error, because there it is a typo.
*/

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Deserializer, Serialize};

use crate::config::Excludes;

/// The report level of a lint. larvae uses the same spellings as selene.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    /// The lint is off.
    Allow,
    /// larvae reports the finding. The exit code does not change.
    #[default]
    Warn,
    /// larvae reports the finding, and the run fails.
    Deny,
}

/// The set of globals that larvae checks a bare name against.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum StdLib {
    /// Only the globals that Luau itself defines.
    Luau,
    /// The Luau globals plus the Roblox API surface.
    #[default]
    Roblox,
    /// No globals, so every bare name is undefined.
    None,
}

impl StdLib {
    /*
    Parse a `std` name into a set of globals. selene's spellings are accepted.

    selene's `std` names a library file, and can chain files with `+`, as in
    `roblox+testez`. Thus only the first name decides the set. Every Lua
    dialect maps to Luau. For this purpose, to know if a bare name exists,
    Luau is a superset of each dialect.
    */
    pub fn parse(name: &str) -> Option<Self> {
        match name.split('+').next()?.trim() {
            "luau" | "lua51" | "lua52" | "lua53" | "lua54" | "luajit" => Some(Self::Luau),

            "roblox" => Some(Self::Roblox),

            "none" | "empty" => Some(Self::None),

            _ => None,
        }
    }
}

/// This manual impl makes sure that `[lint]` accepts the same `std` strings as selene's file.
impl<'de> Deserialize<'de> for StdLib {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let name = String::deserialize(deserializer)?;

        Self::parse(&name).ok_or_else(|| {
            serde::de::Error::custom(format!(
                "unknown std \"{name}\", expected \"roblox\", \"luau\" or \"none\""
            ))
        })
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub struct LintConfig {
    /// The level for each lint, keyed by the lint's name.
    #[serde(default, deserialize_with = "levels")]
    pub rules: BTreeMap<String, Level>,

    /// The settings for each lint. larvae gives them to the lint that
    /// requests them. selene calls the same table `config`.
    #[serde(default, alias = "config")]
    pub options: BTreeMap<String, toml::Value>,

    /// The globals that exist before this file runs.
    #[serde(default)]
    pub std: StdLib,

    /// The extra globals that the project defines beyond the standard library.
    #[serde(default)]
    pub globals: Vec<String>,

    /// The globs that a walk skips, relative to the project root. The key
    /// uses selene's spelling. larvae still lints a file named on the
    /// command line, see [`Excludes`].
    #[serde(default)]
    pub exclude: Vec<String>,
}

impl LintConfig {
    /// Returns the level for one lint. The lint's own default applies unless the project sets one.
    pub fn level_for(&self, name: &str, default: Level) -> Level {
        self.rules.get(name).copied().unwrap_or(default)
    }

    /// Returns the paths that this config tells `larvae lint` to skip.
    pub fn excludes(&self, root: &Path) -> Result<Excludes> {
        Excludes::new(root, &self.exclude).context("[lint]")
    }

    /// Returns the settings for one lint, deserialised into the shape that the lint requests.
    pub fn options_for<T: for<'de> Deserialize<'de> + Default>(&self, name: &str) -> T {
        self.options
            .get(name)
            .and_then(|v| v.clone().try_into().ok())
            .unwrap_or_default()
    }

    /*
    Read `selene.toml` if it exists, then apply `[lint]` over it.

    selene puts levels under `[rules]` and per-lint settings under
    `[config]`. This is the same information under different names. Thus
    larvae translates the file instead of parsing it into a second type.
    */
    pub fn discover(root: &Path, larvae: Option<&toml::Value>) -> Result<Self> {
        let mut config = selene_file(root)?.unwrap_or_default();

        if let Some(value) = larvae {
            let over: Self = value.clone().try_into().context("[lint]")?;

            config.rules.extend(over.rules);
            config.options.extend(over.options);
            config.globals.extend(over.globals);
            config.exclude.extend(over.exclude);

            if value.get("std").is_some() {
                config.std = over.std;
            }
        }

        Ok(config)
    }
}

/*
The keys of selene's file that larvae uses.

This struct does not use `deny_unknown_fields`, and that is intentional. The
file belongs to selene, so larvae must not refuse a key that a later selene
adds. Also, to read the rules from a partly unknown file is better than to
read none.
*/
#[derive(Deserialize)]
struct SeleneFile {
    #[serde(default)]
    rules: BTreeMap<String, Level>,
    #[serde(default)]
    config: BTreeMap<String, toml::Value>,
    #[serde(default)]
    std: Option<String>,
    #[serde(default)]
    exclude: Vec<String>,
}

fn selene_file(root: &Path) -> Result<Option<LintConfig>> {
    let path = root.join("selene.toml");

    if !path.exists() {
        return Ok(None);
    }

    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("cannot read {}", crate::ui::rel(&path)))?;

    let file: SeleneFile = toml::from_str(&text).context("in selene.toml")?;

    /*
    An unknown name falls back to the default instead of failing. Without
    this, a chain that names a library file larvae does not have would
    silently mean "no globals at all". Then undefined_variable findings
    would flood the file.
    */
    let std = file
        .std
        .as_deref()
        .and_then(StdLib::parse)
        .unwrap_or_default();

    Ok(Some(LintConfig {
        rules: file.rules,
        options: file.config,
        std,
        globals: Vec::new(),
        exclude: file.exclude,
    }))
}

/*
One level, or a table of them under a namespace.

A worm names its lints under its own key, so a project writes them together:

```toml
[lint.rules.luaux]
useless_fragment = "warn"
```

TOML reads that as a table inside `[lint.rules]`, while a builtin lint is a
level in the same table. Larvae accepts both, and joins a namespace to a name
with a dot. Thus `luaux.useless_fragment` is the name everywhere else: in a
message, in `--explain`, and in an `allow` comment.
*/
#[derive(Deserialize)]
#[serde(untagged)]
enum LevelOrTable {
    One(Level),
    Namespace(BTreeMap<String, Level>),
}

/// Read `[lint.rules]`, and flatten each namespace into a dotted name
fn levels<'de, D>(deserializer: D) -> Result<BTreeMap<String, Level>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw: BTreeMap<String, LevelOrTable> = Deserialize::deserialize(deserializer)?;
    let mut out = BTreeMap::new();

    for (key, value) in raw {
        match value {
            LevelOrTable::One(level) => {
                out.insert(key, level);
            }

            LevelOrTable::Namespace(table) => {
                for (name, level) in table {
                    out.insert(format!("{key}.{name}"), level);
                }
            }
        }
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_lint_uses_its_own_default_until_the_project_says_otherwise() {
        let mut cfg = LintConfig::default();

        assert_eq!(cfg.level_for("unused_variable", Level::Warn), Level::Warn);

        cfg.rules.insert("unused_variable".into(), Level::Allow);

        assert_eq!(cfg.level_for("unused_variable", Level::Warn), Level::Allow);
    }

    #[test]
    fn a_selene_config_is_read_as_written() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("selene.toml"),
            "std = \"roblox\"\n\n[rules]\nunused_variable = \"allow\"\nshadowing = \"deny\"\n",
        )
        .unwrap();

        let cfg = LintConfig::discover(dir.path(), None).unwrap();

        assert_eq!(cfg.std, StdLib::Roblox);
        assert_eq!(cfg.rules["unused_variable"], Level::Allow);
        assert_eq!(cfg.rules["shadowing"], Level::Deny);
    }

    #[test]
    fn selene_per_lint_settings_come_across() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("selene.toml"),
            "[config]\nhigh_cyclomatic_complexity = { maximum_complexity = 40 }\n",
        )
        .unwrap();

        let cfg = LintConfig::discover(dir.path(), None).unwrap();

        assert!(cfg.options.contains_key("high_cyclomatic_complexity"));
    }

    /// A chained std that names a library larvae does not ship must not mean "no globals".
    #[test]
    fn an_unknown_std_falls_back_rather_than_emptying_the_globals() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("selene.toml"), "std = \"roblox+testez\"\n").unwrap();

        assert_eq!(
            LintConfig::discover(dir.path(), None).unwrap().std,
            StdLib::Roblox
        );

        std::fs::write(dir.path().join("selene.toml"), "std = \"something_else\"\n").unwrap();

        assert_eq!(
            LintConfig::discover(dir.path(), None).unwrap().std,
            StdLib::default()
        );
    }

    #[test]
    fn larvae_config_layers_over_selene() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("selene.toml"),
            "[rules]\nunused_variable = \"allow\"\nshadowing = \"allow\"\n",
        )
        .unwrap();

        let over = toml::from_str::<toml::Value>("[rules]\nshadowing = \"deny\"").unwrap();
        let cfg = LintConfig::discover(dir.path(), Some(&over)).unwrap();

        assert_eq!(cfg.rules["shadowing"], Level::Deny, "larvae should win");
        assert_eq!(
            cfg.rules["unused_variable"],
            Level::Allow,
            "and leave the rest of selene alone"
        );
    }

    /// The file belongs to selene, so a key that larvae does not use causes no error.
    #[test]
    fn unknown_keys_in_the_selene_file_are_ignored() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("selene.toml"),
            "std = \"roblox\"\nexclude = [\"Packages/**\"]\nsomething_new = true\n\n[rules]\nshadowing = \"deny\"\n",
        )
        .unwrap();

        let cfg = LintConfig::discover(dir.path(), None).unwrap();

        assert_eq!(cfg.std, StdLib::Roblox, "the keys we know still apply");
        assert_eq!(cfg.rules["shadowing"], Level::Deny);
    }

    /// The user can paste a selene.toml into [lint] with selene's own names.
    #[test]
    fn selene_names_work_in_larvae_toml_too() {
        let over = toml::from_str::<toml::Value>(
            "std = \"lua53\"\n\n[config]\nhigh_cyclomatic_complexity = { maximum_complexity = 12 }\n",
        )
        .unwrap();

        let cfg = LintConfig::discover(tempfile::tempdir().unwrap().path(), Some(&over)).unwrap();

        assert_eq!(cfg.std, StdLib::Luau, "selene spells the dialects out");
        assert!(cfg.options.contains_key("high_cyclomatic_complexity"));
    }

    /// larvae.toml belongs to larvae, so an unknown key is a typo and larvae reports it.
    #[test]
    fn an_unknown_key_in_larvae_toml_is_still_refused() {
        let over = toml::from_str::<toml::Value>("[rulez]\nshadowing = \"deny\"").unwrap();

        assert!(LintConfig::discover(tempfile::tempdir().unwrap().path(), Some(&over)).is_err());

        let over = toml::from_str::<toml::Value>("std = \"robloks\"").unwrap();

        assert!(LintConfig::discover(tempfile::tempdir().unwrap().path(), Some(&over)).is_err());
    }

    #[test]
    fn options_deserialise_into_whatever_a_lint_asked_for() {
        #[derive(Deserialize, Default, PartialEq, Debug)]
        struct Opts {
            maximum: usize,
        }

        let mut cfg = LintConfig::default();
        cfg.options.insert(
            "thing".into(),
            toml::from_str::<toml::Value>("maximum = 7").unwrap(),
        );

        assert_eq!(cfg.options_for::<Opts>("thing"), Opts { maximum: 7 });

        // An absent or malformed table falls back to the default. The run does not fail.
        assert_eq!(cfg.options_for::<Opts>("missing"), Opts::default());
    }
}
