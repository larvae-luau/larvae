/*!
`[fmt]`, and the `stylua.toml` that a project possibly already has.

Larvae accepts each stylua option under its own name. So a project can point
larvae at an existing `stylua.toml` and get the same output without edits. Or
the user can paste the whole file into `[fmt]` and delete it. The options
beyond that set are the options that users frequently request from stylua and
that Biome already has: granular spacing, and a trailing comma with an effect.

Larvae drops an unknown key from `stylua.toml`. That file belongs to another
tool, and it can name options from a stylua version that larvae does not track
yet. The same unknown key in `[fmt]` is still an error, because there it is a
typo.
*/

use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::doc::{Indent, Style};
use crate::config::Excludes;

/// Selects the quotes of a string literal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum QuoteStyle {
    /// Double quotes, unless single quotes need fewer escapes.
    #[default]
    AutoPreferDouble,
    /// Single quotes, unless double quotes need fewer escapes.
    AutoPreferSingle,
    ForceDouble,
    ForceSingle,
    /// Keep every literal exactly as written.
    Preserve,
}

/// Selects when a call with one string or table argument keeps its parentheses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CallParens {
    #[default]
    Always,
    NoSingleString,
    NoSingleTable,
    None,
    /// Keep the form that the author wrote. This does not enforce consistency.
    Input,
}

/// Selects where a space goes between a function name and its parentheses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SpaceAfterFunctionNames {
    #[default]
    Never,
    /// `function foo ()` but `foo()`.
    Definitions,
    /// `foo ()` but `function foo()`.
    Calls,
    Always,
}

/// Selects which one-line bodies can stay on one line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CollapseSimpleStatement {
    #[default]
    Never,
    FunctionOnly,
    ConditionalOnly,
    Always,
}

/// Selects if the blank lines that an author left at the edge of a block survive.
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

/*
Selects which keyword binds a required module.

`preserve` is the default for two reasons. A formatter that changes the
keyword of a declaration takes a bigger step than a formatter that moves its
spaces. And Luau enforces `const`: the conversion can turn a file that ran
into a syntax error, when a later statement reassigns the name. Larvae finds
that case and keeps `local` there.
*/
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum RequireBinding {
    /// Keep every declaration exactly as written.
    #[default]
    Preserve,
    /// `const X = require(...)`, where nothing reassigns X.
    Const,
    /// `local X = require(...)`.
    Local,
}

/// Selects how `sort_requires` groups the requires it sorts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum RequireGrouping {
    /// One sorted run. This is what stylua does.
    #[default]
    Flat,
    /// Aliases, then absolute paths, then relative paths, with a blank line between each group.
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
#[serde(rename_all = "snake_case")]
pub struct FmtConfig {
    // --- stylua parity: the same names and the same defaults -------------
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

    // --- options beyond stylua -------------------------------------------
    /// Selects which keyword binds a required module.
    #[serde(default)]
    pub require_binding: RequireBinding,

    /*
    A trailing comma that the author left in a table means "keep this table
    expanded".

    This is the magic trailing comma from Prettier. With it, the author
    decides the line breaks per table. Width alone does not decide them. So a
    short table that the author means as a list of things does not collapse
    onto one line.

    The option applies to tables only, and not by choice. Luau rejects
    `f(a, b,)`, so a call has no trailing comma to read. Width alone lays out
    a call.
    */
    #[serde(default = "default_true")]
    pub magic_trailing_comma: bool,

    /// `f( a )` instead of `f(a)`.
    #[serde(default)]
    pub space_inside_parens: bool,

    /// `t[ k ]` instead of `t[k]`.
    #[serde(default)]
    pub space_inside_brackets: bool,

    /// `{ a }` instead of `{a}`. This is the Luau convention.
    #[serde(default = "default_true")]
    pub space_inside_braces: bool,

    /// A table that broke keeps a trailing comma on its last field.
    #[serde(default = "default_true")]
    pub trailing_comma: bool,

    /// Globs that a walk skips, relative to the project root. Larvae still
    /// formats a file named on the command line, see [`Excludes`].
    #[serde(default)]
    pub exclude: Vec<String>,

    // --- accepted keys with no effect -------------------------------------
    /*
    This is the dialect switch of stylua. Larvae accepts it, so a
    `stylua.toml` can move into `[fmt]` whole, not key by key. Larvae formats
    Luau and only Luau, which is what each value of this option requests
    here. So larvae reads the option and then ignores it.
    */
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub syntax: Option<String>,

    /*
    The keys that larvae does not own. A format option of a worm lands here.

    A worm declares its options in its `worm.toml`, and the user writes them
    in `[fmt]` beside the builtin options. Larvae checks each key against the
    declarations of the loaded worms, and gives the values to the worm that
    declared them.
    */
    #[serde(flatten)]
    pub rest: std::collections::BTreeMap<String, toml::Value>,
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
    /*
    The same settings, with the named options back at their own defaults.

    A project uses this to keep one option out of the files that a worm
    claims. The swap goes through JSON, because larvae builds TOML without a
    serializer, and because a name out of a config file cannot select a field
    in any other way.
    */
    pub fn without(&self, except: &[String]) -> Self {
        if except.is_empty() {
            return self.clone();
        }

        let (Ok(mut mine), Ok(base)) = (
            serde_json::to_value(self),
            serde_json::to_value(Self::default()),
        ) else {
            return self.clone();
        };

        for name in except {
            match base.get(name) {
                Some(value) => mine[name] = value.clone(),

                // a name larvae does not own is an option of a worm, and it drops out
                None => {
                    if let Some(table) = mine.as_object_mut() {
                        table.remove(name);
                    }
                }
            }
        }

        serde_json::from_value(mine).unwrap_or_else(|_| self.clone())
    }

    /// Returns the layout style that this config requests from the renderer.
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

    /// Reports if a definition puts a space before its parentheses.
    pub fn space_before_definition_parens(&self) -> bool {
        matches!(
            self.space_after_function_names,
            SpaceAfterFunctionNames::Definitions | SpaceAfterFunctionNames::Always
        )
    }

    /// Returns the paths that this config tells `larvae fmt` to skip.
    pub fn excludes(&self, root: &Path) -> Result<Excludes> {
        Excludes::new(root, &self.exclude).context("[fmt]")
    }

    /// Reports if a call puts a space before its parentheses.
    pub fn space_before_call_parens(&self) -> bool {
        matches!(
            self.space_after_function_names,
            SpaceAfterFunctionNames::Calls | SpaceAfterFunctionNames::Always
        )
    }

    /*
    Reads `stylua.toml` if the file exists. So a project that already uses
    stylua gets the same output without edits. Where both files set a key,
    `[fmt]` in `larvae.toml` wins, because it is the more specific file.
    */
    pub fn discover(root: &Path, larvae: Option<&toml::Value>) -> Result<Self> {
        let mut config = stylua_file(root)?.unwrap_or_default();

        if let Some(value) = larvae {
            config = config.merged(value)?;
        }

        Ok(config)
    }

    /*
    Applies `over` on top of `self`, key by key.

    The merge round-trips through a `toml::Value` instead of a manual match
    on every field. So a new option later needs no change here, and nobody
    can forget one. The nested `sort_requires` table merges the same way. A
    project that sets only `grouping` must not lose `enabled`.
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
Reads the stylua file, if the file exists.

Stylua spells its enums in PascalCase, and larvae spells them in kebab-case.
So this function lowercases the values before it parses them. The other
option, a duplicate of every enum with a second set of serde names, is not
necessary then.
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
Drops the keys that larvae does not know, in place.

This is the file of stylua, not of larvae. So an unknown key is not a mistake
in it. Stylua adds options on its own schedule, and a project can use an
option that larvae does not track yet. A refusal to read the file for that
reason would ignore the whole config for the sake of one line. So larvae
drops the line and honors the rest. `larvae.toml` stays strict, because there
an unknown key really is a typo.

The known set is the serialized default, not a manual list. So a new option
later needs no change here.
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

/// Turns `"AutoPreferDouble"` into `"auto-prefer-double"`, in place, for values only.
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

    /// The extra options are on by default where the Luau community already
    /// writes that way. That is their purpose.
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

        // A path or a glob must survive without changes.
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

    /// The file belongs to stylua. So a key from a version larvae does not track costs nothing.
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

    /// The user can paste a stylua.toml into [fmt] whole, with the dialect switch included.
    #[test]
    fn a_stylua_only_key_is_accepted_in_larvae_toml() {
        let over = toml::from_str::<toml::Value>("syntax = \"Luau\"\ncolumn_width = 80").unwrap();
        let c = FmtConfig::default().merged(&over).unwrap();

        assert_eq!(c.column_width, 80);
        assert_eq!(c.syntax.as_deref(), Some("Luau"));
    }

    /*
    This file belongs to larvae. There an unknown key is a typo, and larvae
    must report it.

    A worm can add a format option, so the key is not refused while the file
    parses. It lands in `rest`, and the check happens when the worms of the
    project are known. A key that no worm declares is refused there.
    */
    #[test]
    fn an_unknown_key_in_larvae_toml_is_still_refused() {
        let over = toml::from_str::<toml::Value>("colum_width = 80").unwrap();
        let mut cfg = FmtConfig::default().merged(&over).expect("it parses");

        assert_eq!(cfg.rest["colum_width"], toml::Value::Integer(80));

        let err = crate::worm::registry::Registry::default()
            .resolve_fmt(&mut cfg)
            .expect_err("no worm declares it");

        assert!(format!("{err:#}").contains("colum_width"), "{err:#}");
    }
}
