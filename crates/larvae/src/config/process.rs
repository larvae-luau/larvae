//! The [process] table names the source of the files and controls the output.

use std::path::PathBuf;

use serde::Deserialize;

/*
The source location: one directory or several

A single root flattens into the output, as before. Several roots each keep
their own directory, so two roots cannot collide. A collision would occur
when both roots hold an init.luau.
*/
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum Input {
    One(PathBuf),
    Many(Vec<PathBuf>),
}

impl ProcessConfig {
    /// The configured roots, always as a list
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
    The position of the larvae rules in the sequence. A worm runs after this
    value unless the worm declares a different order. An author who wants
    "before" uses a smaller number, and "after" a larger one, and does not
    have to know this value.
    */
    #[serde(default = "default_run_order")]
    pub run_order: i64,

    /*
    If a flag comment stays in the output.

    The default is on, because flag comments are instructions to larvae, and
    output that keeps them keeps the scaffolding. Set it to false when people
    read the output and not a game, for example a library published as
    source. There a removed suppression would become a warning again for the
    next reader.
    */
    #[serde(default = "default_true")]
    pub strip_flags: bool,

    #[serde(default = "default_true")]
    pub cache: bool,

    #[serde(default = "default_cache_dir")]
    pub cache_dir: PathBuf,
}

/// Quote character for require strings in the output
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum QuoteStyle {
    /// Keep the quote character that the source used
    #[default]
    Preserve,
    Double,
    Single,
}

impl QuoteStyle {
    /// The quote character to emit; preserve defaults to double for generated text
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
            strip_flags: true,
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
