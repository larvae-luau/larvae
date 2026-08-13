//! `worm.toml`, the manifest that a worm ships beside its artifact

use std::collections::BTreeMap;

use anyhow::{Result, bail};
use serde::Deserialize;

use super::ABI_VERSION;

/// The guest form that a worm ships, one of two
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Form {
    /// Luau source, which larvae runs in an embedded VM
    Luau,
    /// A `wasm32` module, which larvae runs in the interpreter
    Wasm,
    /*
    An ordinary executable. Larvae speaks to it over a pipe.

    This form has native speed and no sandbox. It runs with the full access of
    the user, while a wasm worm cannot read a file the host did not give it.
    The trade is good for a heavy worm that a project trusts. For this reason
    the user must opt in per worm, and this form is not the default.
    */
    Native,
}

impl Form {
    /// The manifest spelling, for error messages
    pub fn name(self) -> &'static str {
        match self {
            Self::Luau => "luau",

            Self::Wasm => "wasm",

            Self::Native => "native",
        }
    }
}

/*
The position of the rules of a worm, relative to the native rules of larvae.

An author must not have to know the number of the native stage. The author
names the side they want, and larvae computes the slot. An explicit number
from the user wins over both.
*/
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(untagged)]
pub enum Stage {
    /// `run_order = 2`, an explicit slot
    At(i64),
    /// `run_order = "before"` or `"after"`, relative to the native rules of larvae
    Named(Side),
}

/// The side of the native rules of larvae that a worm requested
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    /// The worm runs first, so the rules of larvae see the output of this worm
    Before,
    /// The worm runs after the native rules. This is the default when the author writes nothing.
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
    /// The slot this stage resolves to, given the slot of the native rules
    pub fn slot(self, native: i64) -> i64 {
        match self {
            Self::At(n) => n,

            Self::Named(Side::Before) => native - 1,

            Self::Named(Side::After) => native + 1,
        }
    }
}

/// The owner of the requires in the output of a worm
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RequireOwner {
    /// Larvae processes the requires in the output of the worm. This default is correct in
    /// almost all cases.
    #[default]
    Larvae,
    /// The worm resolves its own requires, and larvae does not touch them
    Worm,
}

/// The front-end role. It claims file extensions before the pipeline runs.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Frontend {
    /// The extensions this worm turns into Luau, for example `[".luaux"]`
    pub claims: Vec<String>,
    /// If true, this worm also formats the claimed files for `larvae fmt`
    #[serde(default)]
    pub fmt: bool,
    /*
    If true, the lints of larvae also run on the claimed files.

    The worm reports what only the worm can know. The builtin lints know
    Luau, and a claimed file is mostly Luau. With this key the project gets
    both sets, and the worm author writes no Luau lint of their own.
    */
    #[serde(default)]
    pub inherit_lints: bool,
}

/// One lint that a worm declares, so `[lint.rules]` can set its level by name
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LintDecl {
    /// `larvae lint --explain` shows this text, via the JSON schema
    #[serde(default)]
    pub description: Option<String>,
    /// The level when `[lint.rules]` sets nothing. The default is warn, like most builtins.
    #[serde(default)]
    pub default: crate::lint::Level,
}

/// The type that one declared option takes
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OptionType {
    Boolean,
    Integer,
    Float,
    String,
}

impl OptionType {
    /// The name of this type in a message and in the JSON schema
    pub fn name(self) -> &'static str {
        match self {
            Self::Boolean => "boolean",
            Self::Integer => "integer",
            Self::Float => "number",
            Self::String => "string",
        }
    }

    /// Report if a value has this type
    pub fn accepts(self, value: &toml::Value) -> bool {
        match self {
            Self::Boolean => value.is_bool(),
            Self::Integer => value.is_integer(),
            Self::Float => value.is_float() || value.is_integer(),
            Self::String => value.is_str(),
        }
    }
}

/*
One setting that a worm declares for its own `[worms.<name>.config]` table.

A worm that declares its options gets three things larvae cannot give an
opaque table: larvae refuses a key the worm does not declare, larvae fills a
missing key with the default before init, and the editor completes the key
with its description. A worm that declares nothing keeps the opaque table.
*/
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OptionDecl {
    /// The type this option takes
    #[serde(rename = "type")]
    pub kind: OptionType,
    /// The value larvae sends when the user writes nothing
    #[serde(default)]
    pub default: Option<toml::Value>,
    /// The values this option accepts. An empty list accepts every value of
    /// the type.
    #[serde(default)]
    pub values: Vec<toml::Value>,
    /// The editor hover shows this text, through the generated JSON schema
    #[serde(default)]
    pub description: Option<String>,
}

/// One rule that a worm adds to the `[rules]` table of larvae
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleDecl {
    /// The value larvae uses when the user sets none. It also defines the "off" state.
    #[serde(default)]
    pub default: Option<toml::Value>,
    /// The editor hover shows this text, via the JSON schema
    #[serde(default)]
    pub description: Option<String>,
    /// The node kinds that cross the boundary. All other kinds stay on the fast path.
    #[serde(default)]
    pub filter: Vec<String>,
}

impl RuleDecl {
    /// Report if a resolved value means the rule is off, so larvae does not register it
    pub fn is_off(value: Option<&toml::Value>) -> bool {
        matches!(value, None | Some(toml::Value::Boolean(false)))
    }

    /// The value this rule runs with, given the value the user wrote
    pub fn resolve<'a>(&'a self, user: Option<&'a toml::Value>) -> Option<&'a toml::Value> {
        user.or(self.default.as_ref())
    }
}

/// A parsed `worm.toml`
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    /// The name must match the key under `[worms]`, because it also namespaces the rules
    pub name: String,
    /// The extension API the author built this worm against
    pub api: u32,
    /// The guest form of `entry`
    pub form: Form,
    /// The artifact filename inside the zip
    pub entry: String,
    /// The side of the native rules this worm wants. The user can override it.
    #[serde(default)]
    pub run_order: Option<Stage>,
    /// The owner of the requires in the output of this worm
    #[serde(default)]
    pub requires: RequireOwner,
    /// This field is present when the worm claims file extensions
    #[serde(default)]
    pub frontend: Option<Frontend>,
    /// The rules this worm creates, and no other rules
    #[serde(default)]
    pub rules: BTreeMap<String, RuleDecl>,
    /// The lints this worm reports on the files it claims
    #[serde(default)]
    pub lints: BTreeMap<String, LintDecl>,
    /// The settings this worm takes in `[worms.<name>.config]`
    #[serde(default)]
    pub options: BTreeMap<String, OptionDecl>,
    /*
    The format options this worm adds to the `[fmt]` table of larvae.

    These sit beside the builtin options, in the same way the lints of a worm
    sit beside the builtin lints. A name has to be free, so prefix each one
    with the name of the worm.
    */
    #[serde(default)]
    pub fmt: BTreeMap<String, OptionDecl>,
}

impl Manifest {
    /// Parse and validate a `worm.toml`
    pub fn parse(text: &str) -> Result<Self> {
        let manifest: Self = toml::from_str(text)?;

        manifest.validate()?;

        Ok(manifest)
    }

    /// This is true when the worm takes a slot in the run order
    pub fn has_rules(&self) -> bool {
        !self.rules.is_empty()
    }

    fn validate(&self) -> Result<()> {
        if self.name.is_empty() {
            bail!("worm.toml: name is empty");
        }

        /*
        A wasm worm is a pinned artifact compiled against the host ABI. Thus
        larvae must refuse a mismatch with a clear message. A mismatch must not
        appear later as a trap inside the module.
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
        run_order applies to the rule half. A worm with no rules has no item in
        the sequence to order. Larvae does not ignore the key silently, because
        larvae refuses that failure mode in all other places.
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

        /*
        Lints run on the files a worm claims. Thus a lint with no claims has no
        file it can see. Larvae requires the frontend at parse time. This is
        clearer than a lint that silently does not fire.
        */
        if self.frontend.is_none() && !self.lints.is_empty() {
            bail!(
                "worm `{}` declares lints but no [frontend], and claims are what say which files its lints run on",
                self.name
            );
        }

        /*
        A default and a listed value must have the type the option declares.
        The manifest states the type, so larvae checks the manifest against
        itself before a project can meet it.
        */
        for (name, option) in self.options.iter().chain(&self.fmt) {
            if let Some(default) = &option.default
                && !option.kind.accepts(default)
            {
                bail!(
                    "worm `{}`: option `{name}` defaults to a value that is not a {}",
                    self.name,
                    option.kind.name()
                );
            }

            for value in &option.values {
                if !option.kind.accepts(value) {
                    bail!(
                        "worm `{}`: option `{name}` lists a value that is not a {}",
                        self.name,
                        option.kind.name()
                    );
                }
            }
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

    /// The default from the author applies when the user sets nothing
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

        // this is still an error on a worm with no rules, for each named side
        assert!(after.is_err());
    }

    #[test]
    fn an_api_mismatch_is_refused_by_name() {
        let err = Manifest::parse(&FRONTEND.replace("api   = 1", "api   = 99")).unwrap_err();

        assert!(err.to_string().contains("api 99"), "{err}");
        assert!(err.to_string().contains("luaux"), "{err}");
    }

    /// The error case behind question 4 from luaux
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

    #[test]
    fn a_frontend_can_say_it_formats() {
        let text = FRONTEND.replace("claims = [\".luaux\"]", "claims = [\".luaux\"]\nfmt = true");

        assert!(Manifest::parse(&text).unwrap().frontend.unwrap().fmt);

        // when the manifest sets nothing, the worm does not format
        assert!(!Manifest::parse(FRONTEND).unwrap().frontend.unwrap().fmt);
    }

    #[test]
    fn a_lint_declaration_carries_its_default() {
        let text = format!(
            r#"{FRONTEND}
[lints.luaux_unclosed_element]
description = "An element opened and never closed"
default = "deny"

[lints.luaux_untidy]
"#
        );

        let m = Manifest::parse(&text).unwrap();

        assert_eq!(
            m.lints["luaux_unclosed_element"].default,
            crate::lint::Level::Deny
        );

        // an unset default warns, the same as most builtins
        assert_eq!(m.lints["luaux_untidy"].default, crate::lint::Level::Warn);
    }

    #[test]
    fn lints_without_a_frontend_are_refused() {
        let err = Manifest::parse(
            r#"
name = "w"
api = 1
form = "luau"
entry = "init.luau"

[rules.r]

[lints.tidy]
"#,
        )
        .err()
        .unwrap();

        assert!(err.to_string().contains("no [frontend]"), "{err}");
    }
}
