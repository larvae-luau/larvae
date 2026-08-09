/*!
`[fmt]`, and the `stylua.toml` a project probably already has.

Every stylua option is accepted under its own name, so a project can point
larvae at an existing `stylua.toml` and get the same output without editing
anything, or paste the whole file into `[fmt]` and delete it. The options past
that are the ones people keep asking stylua for and Biome already has: granular
spacing, and a trailing comma that means something.

A key we do not know is dropped from `stylua.toml`, since that file belongs to
another tool and may name options from a version we have not caught up with.
The same key in `[fmt]` is still an error, because there it is a typo.
*/

use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::doc::{Indent, Style};
use crate::config::Excludes;

/// Where a string literal's quotes come from
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum QuoteStyle {
    /// Double unless single needs fewer escapes
    #[default]
    AutoPreferDouble,
    /// Single unless double needs fewer escapes
    AutoPreferSingle,
    ForceDouble,
    ForceSingle,
    /// Leave every literal exactly as written
    Preserve,
}

/// When a call with one string or table argument keeps its parentheses
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CallParens {
    #[default]
    Always,
    NoSingleString,
    NoSingleTable,
    None,
    /// Whatever the author wrote, consistency not enforced
    Input,
}

/// Where a space goes between a function name and its parentheses
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SpaceAfterFunctionNames {
    #[default]
    Never,
    /// `function foo ()` but `foo()`
    Definitions,
    /// `foo ()` but `function foo()`
    Calls,
    Always,
}

/// Which one line bodies may stay on one line
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CollapseSimpleStatement {
    #[default]
    Never,
    FunctionOnly,
    ConditionalOnly,
    Always,
}

/// Whether the blank lines an author left at the edge of a block survive
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum BlockNewlineGaps {
    #[default]
    Never,
    Preserve,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LineEndings {
    #[default]
    Unix,
    Windows,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum IndentType {
    #[default]
    Tabs,
    Spaces,
}

/// How `sort_requires` groups what it sorts
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum RequireGrouping {
    /// One sorted run, which is what stylua does
    #[default]
    Flat,
    /// Aliases, then absolute, then relative, a blank line between each
    ByKind,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct SortRequires {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub grouping: RequireGrouping,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub struct FmtConfig {
    // --- stylua parity, same names and same defaults --------------------
    #[serde(default = "default_width")]
    pub column_width: usize,

    #[serde(default)]
    pub line_endings: LineEndings,

    #[serde(default)]
    pub indent_type: IndentType,

    #[serde(default = "default_indent_width")]
    pub indent_width: usize,

    #[serde(default)]
    pub quote_style: QuoteStyle,

    #[serde(default)]
    pub call_parentheses: CallParens,

    #[serde(default)]
    pub space_after_function_names: SpaceAfterFunctionNames,

    #[serde(default)]
    pub collapse_simple_statement: CollapseSimpleStatement,

    #[serde(default)]
    pub block_newline_gaps: BlockNewlineGaps,

    #[serde(default)]
    pub sort_requires: SortRequires,

    // --- past stylua ----------------------------------------------------
    /*
    A trailing comma the author left in a table means "keep this expanded".

    Prettier's magic trailing comma. It turns line breaking into something an
    author decides per table rather than something width alone decides, so a
    short table meant as a list of things stops collapsing onto one line.

    Tables only, and not by choice: Luau rejects `f(a, b,)`, so a call has no
    trailing comma to read. A call is laid out by width alone.
    */
    #[serde(default = "default_true")]
    pub magic_trailing_comma: bool,

    /// `f( a )` rather than `f(a)`
    #[serde(default)]
    pub space_inside_parens: bool,

    /// `t[ k ]` rather than `t[k]`
    #[serde(default)]
    pub space_inside_brackets: bool,

    /// `{ a }` rather than `{a}`, which is the Luau convention
    #[serde(default = "default_true")]
    pub space_inside_braces: bool,

    /// A table that broke keeps a trailing comma on its last field
    #[serde(default = "default_true")]
    pub trailing_comma: bool,

    /// Globs a walk passes over, relative to the project root. A file named on
    /// the command line is still formatted, see [`Excludes`].
    #[serde(default)]
    pub exclude: Vec<String>,

    // --- accepted, and nothing to do with ------------------------------
    /*
    stylua's dialect switch, taken so a `stylua.toml` can move into `[fmt]`
    whole rather than key by key. larvae formats Luau and only Luau, which is
    what every value of this asks for here, so it is read and then ignored.
    */
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub syntax: Option<String>,
}

fn default_width() -> usize {
    120
}

fn default_indent_width() -> usize {
    4
}

fn default_true() -> bool {
    true
}

impl Default for FmtConfig {
    fn default() -> Self {
        toml::from_str("").expect("every field has a default")
    }
}

impl FmtConfig {
    /// The layout style this config asks the renderer for
    pub fn style(&self) -> Style {
        Style {
            width: self.column_width,
            indent: match self.indent_type {
                IndentType::Tabs => Indent::Tabs {
                    width: self.indent_width,
                },

                IndentType::Spaces => Indent::Spaces(self.indent_width),
            },
            newline: match self.line_endings {
                LineEndings::Unix => "\n",
                LineEndings::Windows => "\r\n",
            },
        }
    }

    /// Whether a definition puts a space before its parentheses
    pub fn space_before_definition_parens(&self) -> bool {
        matches!(
            self.space_after_function_names,
            SpaceAfterFunctionNames::Definitions | SpaceAfterFunctionNames::Always
        )
    }

    /// The paths this config asked `larvae fmt` to leave alone
    pub fn excludes(&self, root: &Path) -> Result<Excludes> {
        Excludes::new(root, &self.exclude).context("[fmt]")
    }

    /// Whether a call puts a space before its parentheses
    pub fn space_before_call_parens(&self) -> bool {
        matches!(
            self.space_after_function_names,
            SpaceAfterFunctionNames::Calls | SpaceAfterFunctionNames::Always
        )
    }

    /*
    Read `stylua.toml` if there is one, so a project already using stylua gets
    the same output without editing anything. `[fmt]` in `larvae.toml` wins
    where both say something, since it is the more specific file.
    */
    pub fn discover(root: &Path, larvae: Option<&toml::Value>) -> Result<Self> {
        let mut config = stylua_file(root)?.unwrap_or_default();

        if let Some(value) = larvae {
            config = config.merged(value)?;
        }

        Ok(config)
    }

    /*
    `over` laid on top of `self`, key by key.

    Round tripping through a `toml::Value` rather than matching every field by
    hand, so adding an option later needs no change here and cannot be
    forgotten. The nested `sort_requires` table merges the same way, since a
    project setting only `grouping` should not lose `enabled`.
    */
    fn merged(self, over: &toml::Value) -> Result<Self> {
        let mut base = toml::Value::try_from(&self).expect("the config always serializes");

        let (Some(base_table), Some(over_table)) = (base.as_table_mut(), over.as_table()) else {
            return Ok(self);
        };

        for (key, value) in over_table {
            match (base_table.get_mut(key), value) {
                (Some(toml::Value::Table(under)), toml::Value::Table(on_top)) => {
                    for (k, v) in on_top {
                        under.insert(k.clone(), v.clone());
                    }
                }

                _ => {
                    base_table.insert(key.clone(), value.clone());
                }
            }
        }

        base.try_into().context("[fmt]")
    }
}

/*
The stylua file, if present.

stylua spells its enums in PascalCase and ours in kebab, so the values are
lowered before parsing rather than duplicating every enum with a second set of
serde names.
*/
fn stylua_file(root: &Path) -> Result<Option<FmtConfig>> {
    for name in ["stylua.toml", ".stylua.toml"] {
        let path = root.join(name);

        if !path.exists() {
            continue;
        }

        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("cannot read {}", crate::ui::rel(&path)))?;

        let mut value: toml::Value =
            toml::from_str(&lower_enum_values(&text)).with_context(|| format!("in {name}"))?;

        let known = toml::Value::try_from(FmtConfig::default()).expect("the config serializes");

        if let (Some(table), Some(known)) = (value.as_table_mut(), known.as_table()) {
            prune_unknown(table, known);
        }

        return Ok(Some(
            value.try_into().with_context(|| format!("in {name}"))?,
        ));
    }

    Ok(None)
}

/*
Drop the keys we do not know, in place.

This is stylua's file, not ours, so a key we do not recognise is not a mistake
in it: stylua adds options on its own schedule and a project may well be using
one we have not caught up with. Refusing to read the file over that would mean
the whole config is ignored for the sake of one line, so the line is dropped
and the rest is honoured. `larvae.toml` stays strict, where an unknown key
really is a typo.

The known set is the serialized default rather than a hand written list, so
adding an option later needs no change here.
*/
fn prune_unknown(table: &mut toml::value::Table, known: &toml::value::Table) {
    table.retain(|key, value| {
        let Some(reference) = known.get(key) else {
            return false;
        };

        if let (Some(nested), Some(reference)) = (value.as_table_mut(), reference.as_table()) {
            prune_unknown(nested, reference);
        }

        true
    });
}

/// `"AutoPreferDouble"` becomes `"auto-prefer-double"`, in place, for values only
fn lower_enum_values(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;

    while let Some(open) = rest.find('"') {
        out.push_str(&rest[..=open]);
        rest = &rest[open + 1..];

        let Some(close) = rest.find('"') else {
            out.push_str(rest);

            return out;
        };

        let value = &rest[..close];

        if value.chars().next().is_some_and(|c| {
            c.is_ascii_uppercase() && value.chars().all(|c| c.is_ascii_alphabetic())
        }) {
            out.push_str(&kebab(value));
        } else {
            out.push_str(value);
        }

        out.push('"');
        rest = &rest[close + 1..];
    }

    out.push_str(rest);

    out
}

fn kebab(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);

    for (i, c) in s.chars().enumerate() {
        if c.is_ascii_uppercase() {
            if i > 0 {
                out.push('-');
            }

            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_defaults_match_styluas() {
        let c = FmtConfig::default();

        assert_eq!(c.column_width, 120);
        assert_eq!(c.indent_width, 4);
        assert_eq!(c.indent_type, IndentType::Tabs);
        assert_eq!(c.line_endings, LineEndings::Unix);
        assert_eq!(c.quote_style, QuoteStyle::AutoPreferDouble);
        assert_eq!(c.call_parentheses, CallParens::Always);
        assert!(!c.sort_requires.enabled);
    }

    /// The point of the extra options is that they are on by default where the
    /// Luau community already writes that way
    #[test]
    fn the_extra_options_default_to_the_common_style() {
        let c = FmtConfig::default();

        assert!(c.magic_trailing_comma);
        assert!(c.space_inside_braces);
        assert!(!c.space_inside_parens);
        assert!(!c.space_inside_brackets);
    }

    #[test]
    fn a_stylua_file_is_read_as_written() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("stylua.toml"),
            r#"
column_width = 100
indent_type = "Spaces"
indent_width = 2
quote_style = "ForceSingle"
call_parentheses = "NoSingleTable"
"#,
        )
        .unwrap();

        let c = FmtConfig::discover(dir.path(), None).unwrap();

        assert_eq!(c.column_width, 100);
        assert_eq!(c.indent_type, IndentType::Spaces);
        assert_eq!(c.indent_width, 2);
        assert_eq!(c.quote_style, QuoteStyle::ForceSingle);
        assert_eq!(c.call_parentheses, CallParens::NoSingleTable);
    }

    #[test]
    fn a_dotfile_stylua_config_is_found_too() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".stylua.toml"), "column_width = 60\n").unwrap();

        assert_eq!(
            FmtConfig::discover(dir.path(), None).unwrap().column_width,
            60
        );
    }

    #[test]
    fn larvae_config_wins_over_stylua() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("stylua.toml"),
            "column_width = 100\nindent_width = 2\n",
        )
        .unwrap();

        let over = toml::from_str::<toml::Value>("column_width = 80").unwrap();
        let c = FmtConfig::discover(dir.path(), Some(&over)).unwrap();

        assert_eq!(c.column_width, 80, "larvae.toml should win");
        assert_eq!(c.indent_width, 2, "and leave the rest of stylua alone");
    }

    #[test]
    fn pascal_case_values_lower_but_strings_do_not() {
        assert_eq!(
            lower_enum_values(r#"quote_style = "AutoPreferDouble""#),
            r#"quote_style = "auto-prefer-double""#
        );

        // a path or a glob has to survive untouched
        assert_eq!(
            lower_enum_values(r#"ignore = ["Packages/**", "a/B/c.luau"]"#),
            r#"ignore = ["Packages/**", "a/B/c.luau"]"#
        );
    }

    #[test]
    fn the_style_follows_the_config() {
        let c = FmtConfig {
            indent_type: IndentType::Spaces,
            indent_width: 2,
            line_endings: LineEndings::Windows,
            ..Default::default()
        };

        let style = c.style();

        assert_eq!(style.newline, "\r\n");
        assert!(matches!(style.indent, Indent::Spaces(2)));
    }

    #[test]
    fn space_after_function_names_splits_the_two_cases() {
        let defs = FmtConfig {
            space_after_function_names: SpaceAfterFunctionNames::Definitions,
            ..Default::default()
        };

        assert!(defs.space_before_definition_parens());
        assert!(!defs.space_before_call_parens());

        let both = FmtConfig {
            space_after_function_names: SpaceAfterFunctionNames::Always,
            ..Default::default()
        };

        assert!(both.space_before_definition_parens());
        assert!(both.space_before_call_parens());
    }

    /// stylua's file, so a key from a version we do not track costs nothing
    #[test]
    fn an_unknown_key_in_the_stylua_file_is_dropped() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("stylua.toml"),
            "column_width = 90\nwhoops = true\nsyntax = \"Luau\"\n\n[sort_requires]\nenabled = true\nsomething_new = 3\n",
        )
        .unwrap();

        let c = FmtConfig::discover(dir.path(), None).unwrap();

        assert_eq!(c.column_width, 90, "the keys we know still apply");
        assert!(c.sort_requires.enabled, "including nested ones");
    }

    /// A stylua.toml can be pasted into [fmt] whole, dialect switch and all
    #[test]
    fn a_stylua_only_key_is_accepted_in_larvae_toml() {
        let over = toml::from_str::<toml::Value>("syntax = \"Luau\"\ncolumn_width = 80").unwrap();
        let c = FmtConfig::default().merged(&over).unwrap();

        assert_eq!(c.column_width, 80);
        assert_eq!(c.syntax.as_deref(), Some("Luau"));
    }

    /// Our file, where an unknown key is a typo and worth saying so
    #[test]
    fn an_unknown_key_in_larvae_toml_is_still_refused() {
        let over = toml::from_str::<toml::Value>("colum_width = 80").unwrap();

        assert!(FmtConfig::default().merged(&over).is_err());
    }
}
