/*!
`[worms]`, where a project names the extensions it wants.

The key is the worm's name, which has to match `name` in its `worm.toml`,
because it is also what namespaces the worm's rules and what `[config.<key>]`
attaches to. One identity rather than three that can drift.
*/

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Result, bail};
use serde::Deserialize;

use crate::worm::manifest::Stage;

/// One worm, as the project asked for it
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub source: Source,
    /// Where this worm's rules sit, the user's word being final
    pub run_order: Option<Stage>,
    /*
    Rules the user switched on, by name. They live here rather than in [rules]
    so a worm cannot shadow a builtin: a worm declaring const_requires would
    otherwise be eaten by our typed config and never see it.
    */
    pub rules: BTreeMap<String, toml::Value>,
}

/// Where one worm comes from
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// A GitHub release, `owner/repo@version`
    Release {
        repo: String,
        version: String,
        /// Asset to fetch, defaults to `<name>-worm.zip` then `worm.zip`
        asset: Option<String>,
    },

    /*
    A directory on disk. Not how a worm ships, but a worm author has to be able
    to run their own before publishing it, and so does anyone debugging one.
    */
    Local {
        path: PathBuf,
    },
}

impl Source {
    /// The asset name to look for in a release, given the worm's key
    pub fn asset_names(&self, name: &str) -> Vec<String> {
        match self {
            Self::Release {
                asset: Some(asset), ..
            } => vec![asset.clone()],

            Self::Release { .. } => vec![format!("{name}-worm.zip"), "worm.zip".to_owned()],

            Self::Local { .. } => Vec::new(),
        }
    }
}

/// The shapes `[worms]` accepts, before validation
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum Raw {
    /// `luaux = "luau-xml/worm@0.1.0"`
    Pin(String),
    Table(Table),
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
    run_order: Option<Stage>,
    #[serde(default)]
    rules: BTreeMap<String, toml::Value>,
}

/// Every worm a project asked for, by name
#[derive(Debug, Default)]
pub struct Worms(pub BTreeMap<String, Entry>);

impl Worms {
    /// Read the `[worms]` table, rejecting anything ambiguous
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

fn source_of(name: &str, raw: Raw) -> Result<Entry> {
    let (run_order, rules) = match &raw {
        Raw::Table(t) => (t.run_order, t.rules.clone()),

        Raw::Pin(_) => (None, BTreeMap::new()),
    };

    let source = match raw {
        Raw::Pin(pin) => parse_pin(name, &pin)?,

        Raw::Table(t) => match (t.path, t.repo, t.version) {
            (Some(path), None, None) => {
                if t.asset.is_some() {
                    bail!("worm `{name}`: asset means nothing with path, it is not a release");
                }

                Source::Local { path }
            }

            (None, Some(repo), Some(version)) => Source::Release {
                repo,
                version,
                asset: t.asset,
            },

            (Some(_), _, _) => bail!("worm `{name}`: use path or repo, not both"),

            (None, Some(_), None) => bail!("worm `{name}`: repo needs a version to pin"),

            (None, None, _) => bail!("worm `{name}`: needs either repo and version, or path"),
        },
    };

    Ok(Entry {
        source,
        run_order,
        rules,
    })
}

/// `owner/repo@version`, which is the form almost every worm uses
fn parse_pin(name: &str, pin: &str) -> Result<Source> {
    let Some((repo, version)) = pin.rsplit_once('@') else {
        bail!("worm `{name}`: {pin:?} has no @version, ex: \"owner/repo@0.1.0\"");
    };

    if repo.split('/').filter(|s| !s.is_empty()).count() != 2 {
        bail!("worm `{name}`: {repo:?} is not owner/repo");
    }

    if version.is_empty() {
        bail!("worm `{name}`: {pin:?} has an empty version");
    }

    Ok(Source::Release {
        repo: repo.to_owned(),
        version: version.trim_start_matches('v').to_owned(),
        asset: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn worms(toml_src: &str) -> Result<Worms> {
        Worms::parse(&toml::from_str::<toml::Value>(toml_src).unwrap())
    }

    #[test]
    fn a_pin_is_the_short_form() {
        let w = worms(r#"luaux = "luau-xml/worm@0.1.0""#).unwrap();

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
        let w = worms(r#"luaux = "luau-xml/worm@v0.1.0""#).unwrap();

        assert!(
            matches!(&w.0["luaux"].source, Source::Release { version, .. } if version == "0.1.0")
        );
    }

    #[test]
    fn the_asset_defaults_to_the_worms_own_name() {
        let w = worms(r#"luaux = "luau-xml/worm@0.1.0""#).unwrap();

        assert_eq!(
            w.0["luaux"].source.asset_names("luaux"),
            ["luaux-worm.zip", "worm.zip"]
        );
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

    /// The whole point of the local form
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

    #[test]
    fn a_pin_without_a_version_says_what_to_write() {
        let err = worms(r#"luaux = "luau-xml/worm""#).err().unwrap();

        assert!(err.to_string().contains("owner/repo@0.1.0"), "{err}");
    }

    #[test]
    fn a_pin_that_is_not_owner_slash_repo_is_refused() {
        assert!(worms(r#"luaux = "worm@0.1.0""#).is_err());
        assert!(worms(r#"luaux = "a/b/c@0.1.0""#).is_err());
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

    /// An author says which side of our rules they want, not a number
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
        // a name that is also ours, kept apart rather than colliding
        assert_eq!(
            w.0["mine"].rules["const_requires"],
            toml::Value::Boolean(false)
        );
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
