//! The [rules] table holds the darklua names verbatim, plus the rules of larvae.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::Deserialize;

use super::process::default_true;

/*
Builtin transforms, all off by default. larvae accepts every darklua rule
name, so a config ports over 1:1. A name that is not implemented gives an
error that names its milestone; larvae does not ignore it without a message.
*/
#[derive(Debug, Default, Deserialize)]
pub struct RulesConfig {
    #[serde(default)]
    pub const_requires: bool,

    #[serde(default)]
    pub remove_comments: Option<RemoveComments>,

    #[serde(default)]
    pub append_text_comment: Option<AppendTextComment>,

    /// larvae only, ex: "strict" writes `--!strict` at the top
    #[serde(default)]
    pub add_luau_directive: Option<String>,

    // --- darklua parity, tier one, plain tree edits -----------------------
    #[serde(default)]
    pub remove_method_definition: bool,

    #[serde(default)]
    pub remove_compound_assignment: bool,

    #[serde(default)]
    pub remove_floor_division: bool,

    #[serde(default)]
    pub remove_if_expression: bool,

    #[serde(default)]
    pub remove_method_call: bool,

    #[serde(default)]
    pub convert_index_to_field: bool,

    #[serde(default)]
    pub convert_function_to_assignment: bool,

    #[serde(default)]
    pub convert_luau_number: bool,

    #[serde(default)]
    pub make_assignment_local: bool,

    #[serde(default)]
    pub remove_types: bool,

    #[serde(default)]
    pub remove_attribute: Option<RemoveAttribute>,

    #[serde(default)]
    pub remove_function_call_parens: bool,

    #[serde(default)]
    pub filter_after_early_return: bool,

    #[serde(default)]
    pub remove_interpolated_string: Option<RemoveInterpolatedString>,

    #[serde(default)]
    pub remove_continue: bool,

    #[serde(default)]
    pub remove_empty_do: bool,

    // --- darklua parity, tier two, needs the evaluator --------------------
    #[serde(default)]
    pub remove_assertions: Option<PreserveSideEffects>,

    #[serde(default)]
    pub remove_debug_profiling: Option<PreserveSideEffects>,

    #[serde(default)]
    pub compute_expression: bool,

    #[serde(default)]
    pub remove_unused_if_branch: bool,

    #[serde(default)]
    pub remove_unused_while: bool,

    #[serde(default)]
    pub remove_nil_declaration: bool,

    #[serde(default)]
    pub group_local_assignment: bool,

    #[serde(default)]
    pub convert_local_function_to_assign: bool,

    #[serde(default)]
    pub convert_square_root_call: bool,

    #[serde(default)]
    pub remove_unused_variable: bool,

    #[serde(default)]
    pub rename_variables: bool,

    // --- larvae only, no darklua rule does any of these ---
    /// Calls in statement position to remove, ex: ["print", "debug.profilebegin"]
    #[serde(default)]
    pub remove_calls: Option<RemoveCalls>,

    /// `game.Players` becomes `game:GetService("Players")`
    #[serde(default)]
    pub use_get_service: bool,

    /// Top level requires of the same module merge into the first one
    #[serde(default)]
    pub dedupe_requires: bool,

    /// The name of a constant that holds the datamodel path of this file
    #[serde(default)]
    pub inject_module_path: Option<String>,

    /// Wrap a provably safe module return in `table.freeze`
    #[serde(default)]
    pub freeze_module: bool,

    #[serde(flatten)]
    /// Names that larvae does not own; a worm rule lands here
    pub rest: HashMap<String, toml::Value>,
}

/*
`remove_assertions = true`, or a table with the darklua option
`preserve_arguments_side_effects`. remove_debug_profiling shares this type,
because darklua spells the option the same way in both rules.
*/
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum PreserveSideEffects {
    Enabled(bool),
    Options {
        /// Keep calls whose arguments can have side effects; on by default
        #[serde(default = "default_true")]
        preserve_arguments_side_effects: bool,
    },
}

impl PreserveSideEffects {
    pub fn enabled(&self) -> bool {
        !matches!(self, PreserveSideEffects::Enabled(false))
    }

    pub fn preserve(&self) -> bool {
        match self {
            PreserveSideEffects::Enabled(_) => true,
            PreserveSideEffects::Options {
                preserve_arguments_side_effects,
            } => *preserve_arguments_side_effects,
        }
    }
}

/// `remove_attribute = true` or a table with `match` patterns
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum RemoveAttribute {
    Enabled(bool),
    Options {
        /// Regexes that larvae tests against the attribute name; empty means all attributes
        #[serde(default, rename = "match")]
        patterns: Vec<String>,
    },
}

impl RemoveAttribute {
    pub fn enabled(&self) -> bool {
        !matches!(self, RemoveAttribute::Enabled(false))
    }

    pub fn patterns(&self) -> &[String] {
        match self {
            RemoveAttribute::Enabled(_) => &[],

            RemoveAttribute::Options { patterns } => patterns,
        }
    }
}

/// `remove_interpolated_string = true` or a table with `strategy`
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum RemoveInterpolatedString {
    Enabled(bool),
    Options {
        /// "string" wraps each value in tostring, "tostring" uses `%*`
        #[serde(default = "default_interp_strategy")]
        strategy: String,
    },
}

fn default_interp_strategy() -> String {
    "string".to_string()
}

impl RemoveInterpolatedString {
    pub fn enabled(&self) -> bool {
        !matches!(self, RemoveInterpolatedString::Enabled(false))
    }

    pub fn strategy(&self) -> &str {
        match self {
            RemoveInterpolatedString::Enabled(_) => "string",

            RemoveInterpolatedString::Options { strategy } => strategy,
        }
    }
}

/// `remove_comments = true` or a table with `except` patterns
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum RemoveComments {
    Enabled(bool),
    Options {
        /// Regexes for the comments to keep; the default is the Luau directives
        #[serde(default = "default_comment_except")]
        except: Vec<String>,
    },
}

fn default_comment_except() -> Vec<String> {
    // Keep --!strict and related directives; their removal changes how Luau runs.
    vec!["^--!".to_string()]
}

impl RemoveComments {
    pub fn enabled(&self) -> bool {
        !matches!(self, RemoveComments::Enabled(false))
    }

    pub fn except(&self) -> Vec<String> {
        match self {
            RemoveComments::Enabled(_) => default_comment_except(),

            RemoveComments::Options { except } => except.clone(),
        }
    }
}

/// `remove_calls = ["print"]` or a table that also sets the side effect switch
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum RemoveCalls {
    Functions(Vec<String>),

    Options {
        functions: Vec<String>,

        /// Keep calls whose arguments call a function; that call can have an effect
        #[serde(default = "default_true")]
        preserve_arguments_side_effects: bool,
    },
}

impl RemoveCalls {
    pub fn functions(&self) -> &[String] {
        match self {
            RemoveCalls::Functions(f) | RemoveCalls::Options { functions: f, .. } => f,
        }
    }

    pub fn preserve_arguments_side_effects(&self) -> bool {
        match self {
            RemoveCalls::Functions(_) => true,
            RemoveCalls::Options {
                preserve_arguments_side_effects,
                ..
            } => *preserve_arguments_side_effects,
        }
    }
}

/// `append_text_comment = { text = "...", location = "start" }`
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppendTextComment {
    /// Literal comment text, mutually exclusive with `file`
    #[serde(default)]
    pub text: Option<String>,

    /// Read the text from this file instead
    #[serde(default)]
    pub file: Option<PathBuf>,

    #[serde(default = "default_location")]
    pub location: String,
}

fn default_location() -> String {
    "start".to_string()
}

/// The status of a rule name in larvae
pub enum RuleStatus {
    /// Works today
    Done,

    /// Designed; it arrives in this milestone
    Planned(&'static str),

    /// Not a larvae rule; the message names the correct location
    Elsewhere(&'static str),
}

/*
Every darklua rule name (32 of them) plus the larvae rules. So a ported
darklua config gets a useful message and not "unknown key".
*/
pub fn rule_status(name: &str) -> Option<RuleStatus> {
    use RuleStatus::*;

    Some(match name {
        // larvae rules
        "const_requires"
        | "remove_comments"
        | "append_text_comment"
        | "add_luau_directive"
        | "remove_calls"
        | "use_get_service"
        | "dedupe_requires"
        | "inject_module_path"
        | "freeze_module" => Done,
        // The darklua parity rules live in the ast engine.
        "compute_expression"
        | "convert_function_to_assignment"
        | "convert_index_to_field"
        | "convert_local_function_to_assign"
        | "convert_luau_number"
        | "convert_square_root_call"
        | "filter_after_early_return"
        | "group_local_assignment"
        | "make_assignment_local"
        | "remove_assertions"
        | "remove_attribute"
        | "remove_compound_assignment"
        | "remove_continue"
        | "remove_debug_profiling"
        | "remove_floor_division"
        | "remove_function_call_parens"
        | "remove_if_expression"
        | "remove_interpolated_string"
        | "remove_method_call"
        | "remove_method_definition"
        | "remove_nil_declaration"
        | "remove_types"
        | "remove_unused_if_branch"
        | "remove_empty_do"
        | "remove_unused_variable"
        | "rename_variables"
        | "remove_unused_while" => Done,
        // These are not rules in larvae.
        "convert_require" => Elsewhere("the [requires] section handles requires"),

        "inject_global_value" => Elsewhere("use [defines] instead"),

        "remove_spaces" => Elsewhere("use process.generator = \"dense\" (lands in M4)"),
        _ => return None,
    })
}
