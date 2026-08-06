//! The [process] table, where files come from and how output is written

use std::path::PathBuf;

use serde::Deserialize;

/*
Where source comes from, one directory or several

A single root flattens into the output the way it always has. Several keep
their own directories so two roots cannot collide, which they would the
moment both held an init.luau
*/
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum Input {
    One(PathBuf),
    Many(Vec<PathBuf>),
}

impl ProcessConfig {
    /// Configured roots, always as a list
    pub fn inputs(&self) -> Vec<PathBuf> {
        match &self.input {
            Input::One(p) => vec![p.clone()],

            Input::Many(list) => list.clone(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessConfig {
    #[serde(default = "default_input")]
    pub input: Input,

    #[serde(default = "default_output")]
    pub output: PathBuf,

    #[serde(default = "default_include")]
    pub include: Vec<String>,

    #[serde(default)]
    pub exclude: Vec<String>,

    #[serde(default = "default_generator")]
    pub generator: String,

    #[serde(default)]
    pub quotes: QuoteStyle,

    /*
    Where our own rules sit in the sequence. A worm runs after this unless it
    says otherwise, so an author writing "before" gets a smaller number and
    "after" a larger one without ever needing to know what this is.
    */
    #[serde(default = "default_run_order")]
    pub run_order: i64,

    #[serde(default = "default_true")]
    pub cache: bool,

    #[serde(default = "default_cache_dir")]
    pub cache_dir: PathBuf,
}

/// Quote character for require strings in the output
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum QuoteStyle {
    /// Keep whatever the source used
    #[default]
    Preserve,
    Double,
    Single,
}

impl QuoteStyle {
    /// The quote char to emit, preserve defaults to double for generated text
    pub fn char(self) -> char {
        match self {
            QuoteStyle::Single => '\'',

            QuoteStyle::Preserve | QuoteStyle::Double => '"',
        }
    }
}

impl Default for ProcessConfig {
    fn default() -> Self {
        Self {
            input: default_input(),
            output: default_output(),
            include: default_include(),
            exclude: Vec::new(),
            generator: default_generator(),
            quotes: QuoteStyle::default(),
            run_order: default_run_order(),
            cache: true,
            cache_dir: default_cache_dir(),
        }
    }
}

fn default_input() -> Input {
    Input::One("src".into())
}
fn default_output() -> PathBuf {
    "dist".into()
}
fn default_include() -> Vec<String> {
    vec!["**/*.luau".into(), "**/*.lua".into()]
}
fn default_generator() -> String {
    "retain-lines".into()
}
fn default_cache_dir() -> PathBuf {
    ".larvae".into()
}
pub(super) fn default_run_order() -> i64 {
    1
}

pub(super) fn default_true() -> bool {
    true
}
