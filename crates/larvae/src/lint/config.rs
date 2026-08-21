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
    /*
    larvae reports the finding as an info. The exit code does not change.

    It is the level below `warn` for a lint a project wants on the record
    without adding to the pile it reads every day. An editor draws it as a
    hint rather than a squiggle.
    */
    Info,
    /// larvae reports the finding. The exit code does not change.
    #[default]
    Warn,
    /// larvae reports the finding, and the run fails.
    Deny,
}

/*
The kind of mistake a lint catches, so a project can set many at once.

Biome has these, and the reason to copy them is the one Biome has: a project
that wants every style opinion off should not have to name nine lints. The
groups live in `[lint.groups]` and not inside `[lint.rules]`, which is not a
matter of taste. A table under `[lint.rules]` already means the lints of a
worm of that name, and nothing reserves a worm name, so a worm published as
`style` would make `[lint.rules.style]` mean two things at once. A separate
table has no such collision, and every config written before this reads
exactly as it did.
*/
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Group {
    /// The code cannot do what it says.
    Correctness,
    /// Probably wrong, and a human decides.
    Suspicious,
    /// A matter of how the code reads.
    Style,
    /// More shape than the job needs.
    Complexity,
    /// Correct, and it costs more than it has to.
    Performance,
    /// A Roblox data type used in a way that does not hold.
    Roblox,
}

impl Group {
    /// The name a project writes under `[lint.groups]`
    pub fn name(self) -> &'static str {
        match self {
            Self::Correctness => "correctness",
            Self::Suspicious => "suspicious",
            Self::Style => "style",
            Self::Complexity => "complexity",
            Self::Performance => "performance",
            Self::Roblox => "roblox",
        }
    }

    /// Every group, for the schema and for `--explain`
    pub fn all() -> [Self; 6] {
        [
            Self::Correctness,
            Self::Suspicious,
            Self::Style,
            Self::Complexity,
            Self::Performance,
            Self::Roblox,
        ]
    }
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
    /*
    Whether `larvae lint` reports on this project at all.

    `false` reports nothing and exits zero. A project that wants larvae for
    its formatter and its requires, and keeps another linter, says so here
    rather than by not running the command.
    */
    #[serde(default)]
    pub enabled: Option<bool>,

    /*
    Whether a lint larvae recommends is on without the project saying so.

    Three states, as Biome has them. Absent and `true` both mean the defaults
    apply, which is what larvae always did. `false` starts every lint at
    `allow`, so a project gets the lints it names and no others.

    A level the project wrote always wins, in either state. `recommended =
    false` with `shadowing = "warn"` is one lint on and the rest off, which is
    the reason to reach for it.
    */
    #[serde(default)]
    pub recommended: Option<bool>,

    /*
    A level for a whole group of lints.

    It sits between `recommended` and `[lint.rules]`: a name in `[lint.rules]`
    always wins, and a group covers every lint the project did not name. So
    `style = "allow"` with `mixed_table = "warn"` is one style lint back on
    and the rest off.
    */
    #[serde(default)]
    pub groups: BTreeMap<Group, Level>,

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

    /// Globs that this area reads even when an exclude removes them
    #[serde(default)]
    pub include: Vec<String>,
}

impl LintConfig {
    /// Returns the level for one lint. The lint's own default applies unless the project sets one.
    /// Reports whether the command runs. Absent reads as on.
    pub fn is_enabled(&self) -> bool {
        self.enabled != Some(false)
    }

    /*
    The level of one lint, most specific answer first.

    A name the project wrote wins over its group, a group wins over
    `recommended`, and `recommended` wins over the default of the lint. A
    worm lint passes `None` for the group, because a worm declares a name and
    not a kind.
    */
    pub fn level_for(&self, name: &str, group: Option<Group>, default: Level) -> Level {
        if let Some(level) = self.rules.get(name) {
            return *level;
        }

        /*
        A group covers the lints of its kind that report, and it does not
        wake one that is off.

        `prefer_const` is `allow` on purpose: it rewrites a keyword, and a
        codebase of ordinary `local` would report on nearly every line. A
        project that writes `style = "info"` is asking the style lints it
        already sees to say less, not asking for a lint it never had. Biome
        draws the line in the same place. To turn one on, name it in
        `[lint.rules]`, which beats the group anyway.
        */
        if default != Level::Allow
            && let Some(level) = group.and_then(|g| self.groups.get(&g))
        {
            return *level;
        }

        /*
        A lint the project did not mention falls back to what larvae
        recommends, unless the project turned that off. Absent reads as
        `true`, so a config written before this option behaves as it did.
        */
        match self.recommended {
            Some(false) => Level::Allow,

            _ => default,
        }
    }

    /// Returns the paths that this config tells `larvae lint` to skip.
    pub fn excludes(&self, root: &Path) -> Result<Excludes> {
        self.excludes_under(root, &[], &[])
    }

    /// The same, with the root level lists that every area inherits
    pub fn excludes_under(
        &self,
        root: &Path,
        root_include: &[String],
        root_exclude: &[String],
    ) -> Result<Excludes> {
        Excludes::layered(
            root,
            &self.include,
            &self.exclude,
            root_include,
            root_exclude,
        )
        .context("[lint]")
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

            /*
            selene has no such key, so `[lint]` is the only place it can come
            from. A merge that forgot it left the option parsed and inert,
            which is worse than not having it.
            */
            config.recommended = over.recommended.or(config.recommended);
            config.enabled = over.enabled.or(config.enabled);

            config.groups.extend(over.groups);
            config.rules.extend(over.rules);
            config.options.extend(over.options);
            config.globals.extend(over.globals);
            config.exclude.extend(over.exclude);
            config.include.extend(over.include);

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
        include: Vec::new(),
        // selene has no such key, so a selene.toml states nothing about them
        recommended: None,
        enabled: None,
        // selene has no groups either, so a selene.toml sets none
        groups: BTreeMap::new(),
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

        assert_eq!(
            cfg.level_for("unused_variable", None, Level::Warn),
            Level::Warn
        );

        cfg.rules.insert("unused_variable".into(), Level::Allow);

        assert_eq!(
            cfg.level_for("unused_variable", None, Level::Warn),
            Level::Allow
        );
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

    #[test]
    fn the_enabled_switch_reads_absent_as_on() {
        let dir = tempfile::tempdir().unwrap();

        assert!(LintConfig::default().is_enabled());

        let over = toml::from_str::<toml::Value>("enabled = false").unwrap();
        let cfg = LintConfig::discover(dir.path(), Some(&over)).unwrap();

        assert!(!cfg.is_enabled());

        /*
        The merge has to carry it. A field by field merge that forgets the
        key leaves the option parsed and inert, which is the defect that
        `recommended` had.
        */
        let over = toml::from_str::<toml::Value>("enabled = true").unwrap();

        assert!(
            LintConfig::discover(dir.path(), Some(&over))
                .unwrap()
                .is_enabled()
        );
    }

    #[test]
    fn a_group_covers_every_lint_of_its_kind() {
        let dir = tempfile::tempdir().unwrap();
        let over = toml::from_str::<toml::Value>("[groups]\nstyle = \"allow\"").unwrap();
        let cfg = LintConfig::discover(dir.path(), Some(&over)).unwrap();

        assert_eq!(
            cfg.level_for("parenthese_conditions", Some(Group::Style), Level::Warn),
            Level::Allow
        );

        // A lint of another kind is untouched.
        assert_eq!(
            cfg.level_for("compare_nan", Some(Group::Correctness), Level::Warn),
            Level::Warn
        );
    }

    #[test]
    fn a_name_the_project_wrote_beats_its_group() {
        let dir = tempfile::tempdir().unwrap();
        let text = "[groups]\nstyle = \"allow\"\n\n[rules]\nmixed_table = \"deny\"";
        let over = toml::from_str::<toml::Value>(text).unwrap();
        let cfg = LintConfig::discover(dir.path(), Some(&over)).unwrap();

        assert_eq!(
            cfg.level_for("mixed_table", Some(Group::Style), Level::Warn),
            Level::Deny
        );
    }

    /*
    A group does not wake a lint that is off on purpose.

    `prefer_const` is `allow` because it rewrites a keyword. A project that
    writes `style = "info"` asks the style lints it already sees to say less;
    it is not asking for a lint it never had.
    */
    #[test]
    fn a_group_does_not_turn_on_a_lint_that_is_allow() {
        let dir = tempfile::tempdir().unwrap();
        let over = toml::from_str::<toml::Value>("[groups]\nstyle = \"info\"").unwrap();
        let cfg = LintConfig::discover(dir.path(), Some(&over)).unwrap();

        assert_eq!(
            cfg.level_for("prefer_const", Some(Group::Style), Level::Allow),
            Level::Allow
        );

        // Naming it is how a project turns it on, and that still works.
        let text = "[groups]\nstyle = \"info\"\n\n[rules]\nprefer_const = \"warn\"";
        let over = toml::from_str::<toml::Value>(text).unwrap();
        let cfg = LintConfig::discover(dir.path(), Some(&over)).unwrap();

        assert_eq!(
            cfg.level_for("prefer_const", Some(Group::Style), Level::Allow),
            Level::Warn
        );
    }

    /*
    A worm lint has a name and no kind, so no group reaches it.

    A worm declares its lints under `[lint.rules.<worm>]`, and nothing says
    which kind of mistake each one catches. A group that guessed would set a
    level the worm author never asked for.
    */
    #[test]
    fn a_group_does_not_reach_a_worm_lint() {
        let dir = tempfile::tempdir().unwrap();
        let over = toml::from_str::<toml::Value>("[groups]\nstyle = \"allow\"").unwrap();
        let cfg = LintConfig::discover(dir.path(), Some(&over)).unwrap();

        assert_eq!(
            cfg.level_for("luaux.useless_fragment", None, Level::Warn),
            Level::Warn
        );
    }

    /// Every lint has a group, so `[lint.groups]` reaches all of them.
    #[test]
    fn every_lint_belongs_to_a_group() {
        for group in Group::all() {
            assert!(
                crate::lint::registry().iter().any(|l| l.group() == group),
                "no lint is in the {} group, so the schema offers a dead key",
                group.name()
            );
        }
    }

    #[test]
    fn info_is_a_level_the_config_takes() {
        let dir = tempfile::tempdir().unwrap();
        let over = toml::from_str::<toml::Value>("[rules]\nshadowing = \"info\"").unwrap();
        let cfg = LintConfig::discover(dir.path(), Some(&over)).unwrap();

        assert_eq!(
            cfg.level_for("shadowing", Some(Group::Suspicious), Level::Warn),
            Level::Info
        );
    }
}
