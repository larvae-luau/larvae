/*!
`[worms]`: the table where a project names the extensions that it wants.

The key is the name of the worm, and it must match `name` in its `worm.toml`.
The same name also namespaces the rules and the settings of the worm. One
identity cannot drift; three identities can.

All data about one worm sits under its own key: its source, its run position,
its enabled rules, and the settings that it reads. The settings stay in their
own table and not in loose keys. So a worm setting named `repo` cannot
collide with the key that names the fetch source.
*/

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Result, bail};
use serde::Deserialize;

use crate::worm::manifest::Stage;

/// One worm, as the project requests it
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    /// The choice of the user about the inherited lints. It overrides what
    /// the worm declared in its manifest.
    pub inherit_lints: Option<bool>,
    /// Which of the inherited lints and format options apply to this worm
    pub inherit: Inherit,
    pub source: Source,
    /// The position of the rules of this worm; the user setting wins
    pub run_order: Option<Stage>,
    /*
    The rules that the user switched on, by name. They live here and not in
    [rules], so a worm cannot shadow a builtin. Otherwise the typed config of
    larvae would consume a worm declaration of const_requires, and the worm
    would never see it.
    */
    pub rules: BTreeMap<String, toml::Value>,
    /// The settings of the worm, passed on unchanged. larvae never reads inside them.
    pub config: toml::Value,
}

/// The source of one worm
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// A GitHub release, `owner/repo@version`
    Release {
        repo: String,
        version: String,
        /// The asset to fetch; the default is `<name>-worm.zip`, then `worm.zip`
        asset: Option<String>,
    },

    /*
    A directory on disk. A worm does not ship this way. But a worm author
    must run their worm before publication, and a debug session needs the
    same access.
    */
    Local {
        path: PathBuf,
    },

    /*
    A crate on crates.io that ships the worm as its binary.

    `cargo install` builds from source on the machine of the user, so one
    published crate serves every platform and a worm author uploads no
    per platform zip. The binary carries its own `worm.toml` and returns it
    over the pipe, because `cargo install` ships no data files.
    */
    Cargo {
        package: String,
        version: String,
    },
}

impl Source {
    /// The asset names to look for in a release, given the key of the worm
    pub fn asset_names(&self, name: &str) -> Vec<String> {
        match self {
            Self::Release {
                asset: Some(asset), ..
            } => vec![asset.clone()],

            /*
            The platform name comes first, because a native worm ships one
            artifact per platform. A worm with one portable artifact, wasm or
            Luau, has no such asset and the plain names answer instead.
            */
            Self::Release { .. } => vec![
                format!(
                    "{name}-worm-{}-{}.zip",
                    std::env::consts::ARCH,
                    std::env::consts::OS
                ),
                format!("{name}-worm.zip"),
                "worm.zip".to_owned(),
            ],

            Self::Local { .. } | Self::Cargo { .. } => Vec::new(),
        }
    }
}

/*
The shapes `[worms]` reads, before validation.

A worm is a table. The string form is still parsed so that a config written
for an older larvae gets a message that says what to write instead, rather
than the message serde produces for a type it did not expect.
*/
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum Raw {
    /// `luaux = "owner/repo@0.1.0"`, the form that larvae no longer takes
    Pin(String),
    Table(Box<Table>),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Table {
    #[serde(default)]
    repo: Option<String>,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    asset: Option<String>,
    #[serde(default)]
    path: Option<PathBuf>,
    #[serde(default)]
    cargo: Option<String>,
    #[serde(default)]
    run_order: Option<Stage>,
    #[serde(default)]
    rules: BTreeMap<String, toml::Value>,
    #[serde(default)]
    config: Option<toml::Value>,
    #[serde(default)]
    inherit_lints: Option<bool>,
    #[serde(default)]
    inherit: Inherit,
}

/*
Which inherited lints and format options apply inside the files of one worm.

A project turns inheritance on and gets everything, because that is the answer
a project wants almost every time. A project that wants less states `only` or
`except`. The two are exclusive: a list of what to keep and a list of what to
drop, given together, cannot both be the answer.
*/
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Inherit {
    /// The only lints of larvae that run in the files of this worm
    #[serde(default)]
    pub lints_only: Vec<String>,
    /// The lints of larvae that do not run in the files of this worm
    #[serde(default)]
    pub lints_except: Vec<String>,
    /// The format options of larvae that do not apply in the files of this
    /// worm. An option named here keeps its own default there.
    #[serde(default)]
    pub fmt_except: Vec<String>,
}

impl Inherit {
    /// Report if one inherited lint runs in the files of this worm
    pub fn allows_lint(&self, name: &str) -> bool {
        if !self.lints_only.is_empty() {
            return self.lints_only.iter().any(|n| n == name);
        }

        !self.lints_except.iter().any(|n| n == name)
    }

    /// Check that the project stated one list and not both
    pub fn validate(&self, name: &str) -> Result<()> {
        if !self.lints_only.is_empty() && !self.lints_except.is_empty() {
            bail!("worm `{name}`: inherit takes lints_only or lints_except, not both");
        }

        Ok(())
    }
}

/// Every worm that a project requests, by name
#[derive(Debug, Default)]
pub struct Worms(pub BTreeMap<String, Entry>);

impl Worms {
    /// Read the `[worms]` table, and reject each ambiguous entry
    pub fn parse(value: &toml::Value) -> Result<Self> {
        let entries: BTreeMap<String, Raw> = value
            .clone()
            .try_into()
            .map_err(|e| anyhow::anyhow!("[worms]: {e}"))?;

        let mut out = BTreeMap::new();

        for (name, entry) in entries {
            out.insert(name.clone(), source_of(&name, entry)?);
        }

        Ok(Self(out))
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&String, &Entry)> {
        self.0.iter()
    }
}

/*
A version without the `v` that a tag often carries.

The version is a key of its own now, so the config holds plain semver and the
install directory is named the same way whichever form the user wrote. The `v`
comes off only in front of a digit, or `^` and a range would lose theirs.
*/
fn plain(version: &str) -> String {
    let trimmed = version.trim();

    match trimmed.strip_prefix('v') {
        Some(rest) if rest.starts_with(|c: char| c.is_ascii_digit()) => rest.to_string(),

        _ => trimmed.to_string(),
    }
}

fn source_of(name: &str, raw: Raw) -> Result<Entry> {
    let (run_order, rules, config, inherit_lints, inherit) = match &raw {
        Raw::Table(t) => (
            t.run_order,
            t.rules.clone(),
            t.config.clone().unwrap_or_else(empty_table),
            t.inherit_lints,
            t.inherit.clone(),
        ),

        Raw::Pin(_) => (
            None,
            BTreeMap::new(),
            empty_table(),
            None,
            Inherit::default(),
        ),
    };

    inherit.validate(name)?;

    if !config.is_table() {
        bail!("worm `{name}`: config has to be a table, ex: [worms.{name}.config]");
    }

    let source = match raw {
        /*
        The version lives on a key of its own now.

        A pin packs the repo and the version into one string, so nothing can
        read either half without splitting it, and `^` sits in the middle of
        a name. A table keeps them apart, which is what install needs to
        resolve a range and what a reader needs to see what is pinned.
        */
        Raw::Pin(pin) => {
            let (repo, version) = match pin.rsplit_once('@') {
                Some((repo, version)) => (repo, version),

                None => (pin.as_str(), "^"),
            };

            bail!(
                "worm `{name}`: a worm is a table now, not a string. Write:\n\
                 \n    {name} = {{ repo = \"{repo}\", version = \"{version}\" }}\n\
                 \nA version of \"^\" takes the newest release on every install."
            )
        }

        Raw::Table(t) => match (t.path.clone(), t.repo.clone(), t.version.clone()) {
            _ if t.cargo.is_some() => {
                if t.path.is_some() || t.repo.is_some() || t.asset.is_some() {
                    bail!("worm `{name}`: cargo excludes path, repo, and asset");
                }

                let package = t.cargo.clone().expect("checked in the guard");

                /*
                The version can sit in the package pin or in the version key,
                because both forms read naturally. Two versions at one time
                cannot both be the answer.
                */
                let (package, pinned) = match package.rsplit_once('@') {
                    Some((p, v)) => (p.to_owned(), Some(v.to_owned())),

                    None => (package, None),
                };

                let version = match (pinned, t.version.clone()) {
                    (Some(a), Some(b)) if a != b => {
                        bail!("worm `{name}`: cargo pins {a} and version says {b}")
                    }

                    (Some(v), _) | (None, Some(v)) => v.trim_start_matches('v').to_owned(),

                    (None, None) => bail!("worm `{name}`: cargo needs a version to pin"),
                };

                Source::Cargo { package, version }
            }

            (Some(path), None, None) => {
                if t.asset.is_some() {
                    bail!("worm `{name}`: asset means nothing with path, it is not a release");
                }

                Source::Local { path }
            }

            (None, Some(repo), Some(version)) => Source::Release {
                repo,
                version: plain(&version),
                asset: t.asset,
            },

            (Some(_), _, _) => bail!("worm `{name}`: use path or repo, not both"),

            (None, Some(_), None) => bail!(
                "worm `{name}`: repo needs a version. Write \"^\" for the newest release, \
                 a number such as \"0.1.0\" to hold one, or a range such as \"^0.1.0\""
            ),

            (None, None, _) => bail!("worm `{name}`: needs either repo and version, or path"),
        },
    };

    Ok(Entry {
        source,
        run_order,
        rules,
        config,
        inherit_lints,
        inherit,
    })
}

fn empty_table() -> toml::Value {
    toml::Value::Table(toml::map::Map::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn worms(toml_src: &str) -> Result<Worms> {
        Worms::parse(&toml::from_str::<toml::Value>(toml_src).unwrap())
    }

    #[test]
    fn a_pin_is_the_short_form() {
        let w = worms(r#"luaux = { repo = "luau-xml/worm", version = "0.1.0" }"#).unwrap();

        assert_eq!(
            w.0["luaux"].source,
            Source::Release {
                repo: "luau-xml/worm".into(),
                version: "0.1.0".into(),
                asset: None,
            }
        );
    }

    #[test]
    fn a_leading_v_on_the_tag_is_accepted() {
        let w = worms(r#"luaux = { repo = "luau-xml/worm", version = "v0.1.0" }"#).unwrap();

        assert!(
            matches!(&w.0["luaux"].source, Source::Release { version, .. } if version == "0.1.0")
        );
    }

    #[test]
    fn a_cargo_worm_parses_with_the_version_in_either_place() {
        let w = worms(r#"luaux = { cargo = "luaux-worm@0.1.0" }"#).unwrap();

        assert_eq!(
            w.0["luaux"].source,
            Source::Cargo {
                package: "luaux-worm".into(),
                version: "0.1.0".into()
            }
        );

        let w = worms(r#"luaux = { cargo = "luaux-worm", version = "0.1.0" }"#).unwrap();

        assert_eq!(
            w.0["luaux"].source,
            Source::Cargo {
                package: "luaux-worm".into(),
                version: "0.1.0".into()
            }
        );
    }

    #[test]
    fn a_cargo_worm_without_a_version_is_refused() {
        let err = worms(r#"luaux = { cargo = "luaux-worm" }"#).err().unwrap();

        assert!(format!("{err:#}").contains("needs a version"), "{err:#}");
    }

    #[test]
    fn two_versions_that_disagree_are_refused() {
        let err = worms(r#"luaux = { cargo = "luaux-worm@0.1.0", version = "0.2.0" }"#)
            .err()
            .unwrap();

        assert!(format!("{err:#}").contains("0.1.0"), "{err:#}");
        assert!(format!("{err:#}").contains("0.2.0"), "{err:#}");
    }

    #[test]
    fn cargo_excludes_the_other_sources() {
        let err = worms(r#"luaux = { cargo = "luaux-worm@0.1.0", path = "w" }"#)
            .err()
            .unwrap();

        assert!(format!("{err:#}").contains("excludes"), "{err:#}");
    }

    #[test]
    fn the_asset_defaults_to_the_worms_own_name() {
        let w = worms(r#"luaux = { repo = "luau-xml/worm", version = "0.1.0" }"#).unwrap();
        let names = w.0["luaux"].source.asset_names("luaux");

        /*
        The platform name comes first, because a native worm ships one
        artifact per platform. The plain names follow, for a worm with one
        portable artifact.
        */
        assert_eq!(
            names[0],
            format!(
                "luaux-worm-{}-{}.zip",
                std::env::consts::ARCH,
                std::env::consts::OS
            )
        );
        assert_eq!(names[1..], ["luaux-worm.zip", "worm.zip"]);
    }

    #[test]
    fn an_explicit_asset_is_the_only_one_tried() {
        let w = worms(
            r#"
[luaux]
repo = "luau-xml/luaux"
version = "0.1.2"
asset = "custom.zip"
"#,
        )
        .unwrap();

        assert_eq!(w.0["luaux"].source.asset_names("luaux"), ["custom.zip"]);
    }

    /// The main purpose of the local form
    #[test]
    fn a_path_needs_no_release_at_all() {
        let w = worms(
            r#"
[mine]
path = "build/myworm"
"#,
        )
        .unwrap();

        assert_eq!(
            w.0["mine"].source,
            Source::Local {
                path: "build/myworm".into()
            }
        );
    }

    /*
    The string form is gone, and the message says what replaced it.

    A config written for an older larvae has to say something better than the
    error serde gives for a type it did not expect, because the fix is one
    line and the user cannot guess it.
    */
    #[test]
    fn the_old_string_form_says_what_to_write_instead() {
        let err = worms(r#"luaux = "luau-xml/worm@0.1.0""#).err().unwrap();
        let text = err.to_string();

        assert!(text.contains("a table now"), "{text}");
        assert!(
            text.contains(r#"repo = "luau-xml/worm", version = "0.1.0""#),
            "the message is copy-pasteable: {text}"
        );
    }

    /// A string with no version suggests the form that follows the releases.
    #[test]
    fn a_string_with_no_version_suggests_the_caret() {
        let err = worms(r#"luaux = "luau-xml/worm""#).err().unwrap();

        assert!(err.to_string().contains(r#"version = "^""#), "{err}");
    }

    #[test]
    fn a_repo_with_no_version_says_what_a_version_can_be() {
        let err = worms(r#"luaux = { repo = "luau-xml/worm" }"#)
            .err()
            .unwrap();
        let text = err.to_string();

        assert!(text.contains("^"), "{text}");
        assert!(text.contains("0.1.0"), "{text}");
    }

    /// `^` and a range keep their caret; only a tag loses its `v`.
    #[test]
    fn a_caret_is_not_mistaken_for_a_tag() {
        for (written, want) in [("^", "^"), ("^0.1.0", "^0.1.0"), ("v0.1.0", "0.1.0")] {
            let src = format!(r#"luaux = {{ repo = "o/w", version = "{written}" }}"#);
            let w = worms(&src).unwrap();

            assert!(
                matches!(&w.0["luaux"].source, Source::Release { version, .. } if version == want),
                "{written} became something else"
            );
        }
    }

    #[test]
    fn mixing_path_and_repo_is_refused() {
        let err = worms(
            r#"
[mine]
path = "x"
repo = "a/b"
version = "1"
"#,
        )
        .err()
        .unwrap();

        assert!(err.to_string().contains("not both"), "{err}");
    }

    #[test]
    fn a_repo_without_a_version_is_refused() {
        let err = worms(
            r#"
[mine]
repo = "a/b"
"#,
        )
        .err()
        .unwrap();

        assert!(err.to_string().contains("needs a version"), "{err}");
    }

    #[test]
    fn an_asset_on_a_local_worm_is_refused() {
        let err = worms(
            r#"
[mine]
path = "x"
asset = "y.zip"
"#,
        )
        .err()
        .unwrap();

        assert!(err.to_string().contains("not a release"), "{err}");
    }

    /// An author names a side of the larvae rules and does not give a number
    #[test]
    fn a_worm_can_ask_for_before_or_after_by_name() {
        let w = worms(
            r#"
[a]
path = "x"
run_order = "before"

[b]
path = "y"
run_order = "after"

[c]
path = "z"
run_order = 7
"#,
        )
        .unwrap();

        assert!(w.0["a"].run_order.unwrap().slot(1) < 1);
        assert!(w.0["b"].run_order.unwrap().slot(1) > 1);
        assert_eq!(w.0["c"].run_order.unwrap().slot(1), 7);
    }

    #[test]
    fn rules_live_under_the_worm_so_one_cannot_shadow_a_builtin() {
        let w = worms(
            r#"
[mine]
path = "x"
rules = { tidy = true, const_requires = false }
"#,
        )
        .unwrap();

        assert_eq!(w.0["mine"].rules["tidy"], toml::Value::Boolean(true));
        // A name that larvae also uses; the two stay apart and do not collide.
        assert_eq!(
            w.0["mine"].rules["const_requires"],
            toml::Value::Boolean(false)
        );
    }

    /// All data about one worm under one key, settings included
    #[test]
    fn settings_live_with_the_worm_that_reads_them() {
        let w = worms(
            r#"
[mine]
path = "x"

[mine.config]
pretty = true
indent = 2
"#,
        )
        .unwrap();

        let config = w.0["mine"].config.as_table().unwrap();

        assert_eq!(config["pretty"], toml::Value::Boolean(true));
        assert_eq!(config["indent"], toml::Value::Integer(2));
    }

    /// A separate table, so the `repo` key of a worm cannot collide with the larvae key
    #[test]
    fn settings_are_kept_apart_from_the_source() {
        let w = worms(
            r#"
[mine]
repo = "a/b"
version = "1.0.0"
config = { repo = "something the worm means by it", version = 3 }
"#,
        )
        .unwrap();

        assert_eq!(
            w.0["mine"].source,
            Source::Release {
                repo: "a/b".into(),
                version: "1.0.0".into(),
                asset: None,
            }
        );
        assert_eq!(
            w.0["mine"].config.as_table().unwrap()["version"],
            toml::Value::Integer(3)
        );
    }

    #[test]
    fn a_worm_with_no_settings_gets_an_empty_table() {
        let w = worms(r#"luaux = { repo = "luau-xml/worm", version = "0.1.0" }"#).unwrap();

        assert_eq!(w.0["luaux"].config, toml::Value::Table(Default::default()));
    }

    #[test]
    fn settings_that_are_not_a_table_say_what_to_write() {
        let err = worms("[mine]\npath = \"x\"\nconfig = 3\n").err().unwrap();

        assert!(err.to_string().contains("[worms.mine.config]"), "{err}");
    }

    #[test]
    fn an_unknown_key_is_refused() {
        assert!(
            worms(
                r#"
[mine]
path = "x"
whoops = true
"#
            )
            .is_err()
        );
    }
}
