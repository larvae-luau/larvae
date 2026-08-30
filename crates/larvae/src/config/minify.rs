/*!
`[minify]`, the tuning table for `generator = "dense"`.

The dense generator is the minifier: it re-emits the tokens of the output
with the least whitespace that lexes the same. This table tunes that
emission. With another generator the table is inert configuration, the same
as a `[fmt]` table next to a stylua.toml. The one exception is `obfuscate`,
which turns the dense generator on by itself.
*/

use serde::{Deserialize, Serialize};

/// The column the dense emitter breaks at when nothing else decides
pub const DEFAULT_COLUMN_SPAN: usize = 120;

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub struct MinifyConfig {
    /*
    The column where the emitter breaks the line.

    A minified file on one line is hostile to every tool that reports
    line numbers, and a crash report with `line 1` says nothing. A break
    near a column keeps the file dense and keeps positions meaningful. A
    token longer than the span, ex: a long string, stays whole.

    Absent is not the same as 120 here, which is why the field is an
    option. `obfuscate` wants one line, and it can only tell that the
    project does not care about line numbers if the project wrote no
    number. Read the resolved value through [`MinifyConfig::span`].
    */
    #[serde(default)]
    pub column_span: Option<usize>,

    /*
    Give every local a short name while minifying.

    Off by default, like every rule. The key is a convenience: it turns on
    the `rename_variables` rule for a dense build without editing `[rules]`,
    so one profile can hold the whole minify story.
    */
    #[serde(default)]
    pub rename_variables: bool,

    /*
    Make the output hard to read.

    Roblox removed `loadstring`, so a shipped file has to be Luau that the
    compiler reads, and no packer can hide it behind a decoder. What is
    left is what a reader uses: the names and the strings. So the output
    prints through the dense emitter whatever `generator` says, every type
    goes, every local takes a `_0x` name, and every string literal becomes
    the `\xNN` form of its own bytes. See [`crate::obfuscate`].

    The file lands on one line, because `obfuscate` sets the column span to
    unlimited. A project that wrote its own `column_span` keeps that
    number: an explicit key beats an implied one everywhere else in this
    config, and a project that asked for line breaks asked on purpose.
    */
    #[serde(default)]
    pub obfuscate: bool,
}

impl MinifyConfig {
    /*
    The column span the emitter runs with.

    The order is: the number the project wrote, then the unlimited span
    that `obfuscate` implies, then the default.
    */
    pub fn span(&self) -> usize {
        match (self.column_span, self.obfuscate) {
            (Some(span), _) => span,

            (None, true) => usize::MAX,

            (None, false) => DEFAULT_COLUMN_SPAN,
        }
    }
}

impl Default for MinifyConfig {
    fn default() -> Self {
        toml::from_str("").expect("every field has a default")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_defaults_break_lines_and_rename_nothing() {
        let c = MinifyConfig::default();

        assert_eq!(c.span(), DEFAULT_COLUMN_SPAN);
        assert!(!c.rename_variables);
        assert!(!c.obfuscate);
    }

    #[test]
    fn obfuscate_alone_asks_for_one_line() {
        let c: MinifyConfig = toml::from_str("obfuscate = true").unwrap();

        assert_eq!(c.span(), usize::MAX);
    }

    #[test]
    fn a_written_column_span_beats_the_one_obfuscate_implies() {
        let c: MinifyConfig = toml::from_str("obfuscate = true\ncolumn_span = 80").unwrap();

        assert_eq!(c.span(), 80);
    }

    #[test]
    fn an_unknown_key_is_refused_like_everywhere_else() {
        assert!(toml::from_str::<MinifyConfig>("colum_span = 80").is_err());
    }
}
