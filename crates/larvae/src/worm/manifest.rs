//! `worm.toml`, the manifest a worm ships beside its artifact

use std::collections::BTreeMap;

use anyhow::{Result, bail};
use serde::Deserialize;

use super::ABI_VERSION;

/// Which of the two guest forms a worm ships
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Form {
    /// Luau source, run in an embedded VM
    Luau,
    /// A `wasm32` module, run in the interpreter
    Wasm,
    /*
    An ordinary executable, spoken to over a pipe.

    Native speed, and no sandbox: it runs with everything the user can reach,
    where a wasm worm cannot read a file we did not hand it. Worth it for a
    worm doing real work that a project trusts, which is why it is opt in per
    worm rather than the default.
    */
    Native,
}

/*
Where a worm's rules sit relative to larvae's own.

An author should not have to know what number our native stage is, so they say
which side of it they want and we work out the rest. A user writing an explicit
number still wins over both.
*/
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(untagged)]
pub enum Stage {
    /// `run_order = 2`, an explicit slot
    At(i64),
    /// `run_order = "before"` or `"after"`, relative to our native rules
    Named(Side),
}

/// Which side of larvae's native rules a worm asked for
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    /// Runs first, so larvae's rules see this worm's output
    Before,
    /// Runs after our native rules, which is what you get by saying nothing
    #[default]
    After,
}

impl std::fmt::Display for Stage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::At(n) => write!(f, "slot {n}"),

            Self::Named(Side::Before) => write!(f, "before larvae's rules"),

            Self::Named(Side::After) => write!(f, "after larvae's rules"),
        }
    }
}

impl Stage {
    /// The slot this resolves to, given where our own rules sit
    pub fn slot(self, native: i64) -> i64 {
        match self {
            Self::At(n) => n,

            Self::Named(Side::Before) => native - 1,

            Self::Named(Side::After) => native + 1,
        }
    }
}

/// Who owns the requires in a worm's output
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RequireOwner {
    /// We process requires in the worm's output, the default and almost always right
    #[default]
    Larvae,
    /// Hands off, the worm resolves its own and we do not look
    Worm,
}

/// The front-end role, which claims file extensions before the pipeline
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Frontend {
    /// Extensions this worm turns into Luau, ex: `[".luaux"]`
    pub claims: Vec<String>,
}

/// One rule a worm adds to our `[rules]` table
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleDecl {
    /// Value used when the user does not set one, and what "off" looks like
    #[serde(default)]
    pub default: Option<toml::Value>,
    /// Shown in editor hover, via the JSON schema
    #[serde(default)]
    pub description: Option<String>,
    /// Node kinds that cross the boundary, everything else stays on the fast path
    #[serde(default)]
    pub filter: Vec<String>,
}

impl RuleDecl {
    /// Whether a resolved value means the rule is off, so we never register it
    pub fn is_off(value: Option<&toml::Value>) -> bool {
        matches!(value, None | Some(toml::Value::Boolean(false)))
    }

    /// The value this rule runs with, given whatever the user wrote
    pub fn resolve<'a>(&'a self, user: Option<&'a toml::Value>) -> Option<&'a toml::Value> {
        user.or(self.default.as_ref())
    }
}

/// A parsed `worm.toml`
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    /// Must match the key under `[worms]`, since it also namespaces the rules
    pub name: String,
    /// The extension API this worm was built against
    pub api: u32,
    /// Which guest form `entry` is
    pub form: Form,
    /// Artifact filename inside the zip
    pub entry: String,
    /// Which side of our native rules this worm wants, overridable by the user
    #[serde(default)]
    pub run_order: Option<Stage>,
    /// Who owns requires in this worm's output
    #[serde(default)]
    pub requires: RequireOwner,
    /// Present when the worm claims file extensions
    #[serde(default)]
    pub frontend: Option<Frontend>,
    /// Rules this worm creates, and nothing else
    #[serde(default)]
    pub rules: BTreeMap<String, RuleDecl>,
}

impl Manifest {
    /// Parse and validate a `worm.toml`
    pub fn parse(text: &str) -> Result<Self> {
        let manifest: Self = toml::from_str(text)?;

        manifest.validate()?;

        Ok(manifest)
    }

    /// True when this worm takes a slot in the run order
    pub fn has_rules(&self) -> bool {
        !self.rules.is_empty()
    }

    fn validate(&self) -> Result<()> {
        if self.name.is_empty() {
            bail!("worm.toml: name is empty");
        }

        /*
        A wasm worm is a pinned artifact compiled against our host ABI, so a
        mismatch has to be refused with a sentence rather than discovered as a
        trap somewhere inside the module.
        */
        if self.api != ABI_VERSION {
            bail!(
                "worm `{}` targets api {} but this larvae speaks api {ABI_VERSION}",
                self.name,
                self.api
            );
        }

        if self.entry.is_empty() {
            bail!("worm `{}`: entry is empty", self.name);
        }

        /*
        run_order addresses the rule half. A worm with no rules has nothing in
        the sequence to order, and quietly ignoring the key is the failure mode
        we refuse everywhere else.
        */
        if self.run_order.is_some() && !self.has_rules() {
            bail!(
                "worm `{}` sets run_order but declares no rules, so there is nothing to order",
                self.name
            );
        }

        if self.frontend.is_none() && !self.has_rules() {
            bail!(
                "worm `{}` declares neither a frontend nor any rules, so it would never run",
                self.name
            );
        }

        if let Some(frontend) = &self.frontend {
            if frontend.claims.is_empty() {
                bail!("worm `{}`: [frontend] claims nothing", self.name);
            }

            for claim in &frontend.claims {
                if !claim.starts_with('.') || claim.len() < 2 {
                    bail!(
                        "worm `{}` claims {claim:?}, which is not a file extension like \".luaux\"",
                        self.name
                    );
                }
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRONTEND: &str = r#"
name  = "luaux"
api   = 1
form  = "wasm"
entry = "luaux.wasm"

[frontend]
claims = [".luaux"]
"#;

    #[test]
    fn a_frontend_only_worm_parses() {
        let m = Manifest::parse(FRONTEND).unwrap();

        assert_eq!(m.name, "luaux");
        assert_eq!(m.form, Form::Wasm);
        assert_eq!(m.frontend.as_ref().unwrap().claims, [".luaux"]);
        assert!(!m.has_rules());
    }

    #[test]
    fn requires_defaults_to_larvae() {
        assert_eq!(
            Manifest::parse(FRONTEND).unwrap().requires,
            RequireOwner::Larvae
        );
    }

    #[test]
    fn a_worm_can_hand_requires_to_itself() {
        let text = FRONTEND.replace("form  =", "requires = \"worm\"\nform  =");

        assert_eq!(Manifest::parse(&text).unwrap().requires, RequireOwner::Worm);
    }

    #[test]
    fn rules_carry_their_defaults_and_filters() {
        let m = Manifest::parse(
            r#"
name  = "tailwind"
api   = 1
form  = "luau"
entry = "init.luau"
run_order = 2

[rules.expand_classes]
default = false
description = "Expand class strings"
filter = ["Call"]
"#,
        )
        .unwrap();

        let rule = &m.rules["expand_classes"];

        assert_eq!(rule.filter, ["Call"]);
        assert_eq!(rule.description.as_deref(), Some("Expand class strings"));
        assert!(m.has_rules());
        assert_eq!(m.run_order, Some(Stage::At(2)));
    }

    /// The author's default, when the user says nothing
    #[test]
    fn a_worm_declares_which_side_of_our_rules_it_wants() {
        let before = Manifest::parse(
            r#"
name  = "w"
api   = 1
form  = "luau"
entry = "init.luau"
run_order = "before"

[rules.r]
default = true
"#,
        )
        .unwrap();

        assert!(before.run_order.unwrap().slot(1) < 1);

        let after = Manifest::parse(&FRONTEND.replace("form  =", "run_order = \"after\"\nform  ="));

        // still an error on a worm with no rules, whichever side it named
        assert!(after.is_err());
    }

    #[test]
    fn an_api_mismatch_is_refused_by_name() {
        let err = Manifest::parse(&FRONTEND.replace("api   = 1", "api   = 99")).unwrap_err();

        assert!(err.to_string().contains("api 99"), "{err}");
        assert!(err.to_string().contains("luaux"), "{err}");
    }

    /// The error case behind luaux's question 4
    #[test]
    fn run_order_on_a_worm_with_no_rules_is_an_error() {
        let text = FRONTEND.replace("form  =", "run_order = 1\nform  =");
        let err = Manifest::parse(&text).unwrap_err();

        assert!(err.to_string().contains("nothing to order"), "{err}");
    }

    #[test]
    fn a_worm_that_would_never_run_is_an_error() {
        let err = Manifest::parse(
            r#"
name = "idle"
api = 1
form = "luau"
entry = "init.luau"
"#,
        )
        .unwrap_err();

        assert!(err.to_string().contains("never run"), "{err}");
    }

    #[test]
    fn a_claim_must_look_like_an_extension() {
        let text = FRONTEND.replace("\".luaux\"", "\"luaux\"");
        let err = Manifest::parse(&text).unwrap_err();

        assert!(err.to_string().contains("not a file extension"), "{err}");
    }

    #[test]
    fn an_unknown_key_is_refused() {
        let text = format!("{FRONTEND}\nwhat_is_this = true\n");

        assert!(Manifest::parse(&text).is_err());
    }

    #[test]
    fn an_off_rule_is_recognised_however_it_got_that_way() {
        assert!(RuleDecl::is_off(None));
        assert!(RuleDecl::is_off(Some(&toml::Value::Boolean(false))));
        assert!(!RuleDecl::is_off(Some(&toml::Value::Boolean(true))));
        assert!(!RuleDecl::is_off(Some(&toml::Value::String("x".into()))));
    }

    #[test]
    fn a_user_value_beats_the_manifest_default() {
        let m = Manifest::parse(
            r#"
name = "w"
api = 1
form = "luau"
entry = "init.luau"

[rules.r]
default = false
"#,
        )
        .unwrap();

        let rule = &m.rules["r"];
        let on = toml::Value::Boolean(true);

        assert_eq!(rule.resolve(Some(&on)), Some(&on));
        assert_eq!(rule.resolve(None), Some(&toml::Value::Boolean(false)));
    }
}
