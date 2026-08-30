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
    /*
    Drops the parentheses wherever Luau allows the bare form, and keeps them
    everywhere else. `as-needed` is the same setting under the name that Biome
    and Prettier use for this shape; `none` is the name that stylua uses, and a
    pasted `stylua.toml` keeps working.

    Worth knowing what "wherever Luau allows" covers, because it is narrower
    than it sounds. The bare form takes one string or one table and nothing
    else, so `f "s"` and `g { x = 1 }` lose their parentheses while `h(a)`
    keeps them. `h a` is not terser Luau, it is a syntax error.
    */
    #[serde(alias = "as-needed")]
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

/*
Selects what the formatter does with a required module that nothing uses.

Off by default, and the default matters more here than it does for the other
options. `require` runs the module the first time a file asks for it, so a
module can do its work by being required at all: it connects an event, it
registers a component, it fills a table somewhere else. To delete the line
then stops that work, and the file still compiles, so nothing says the
behaviour changed. Larvae cannot tell that module from a module that only
returns a value, because the answer is inside a file the formatter is not
reading.

So the project decides. `underscore` keeps the require and marks the name,
which changes no behaviour at all and silences the lint. `remove` deletes the
statement, which is what a project wants when its modules only return values.

A name that a type uses is used. `const jecs = require("@pkg/jecs")` with
`type C = jecs.Component` below it reads the binding, and the resolution the
linter builds already counts it.
*/
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum UnusedImports {
    /// Leave it as written.
    #[default]
    Ignore,
    /// `local _Signal = require(...)`, which keeps the module and marks the name.
    Underscore,
    /// Delete the declaration.
    Remove,
}

/*
Selects whether a statement ends with a semicolon.

Luau needs one in exactly one place: before a statement that opens with `(`,
which would otherwise continue the line above as a call. Larvae emits that one
whatever this option says, because the alternative is output that does not mean
what the input meant.

That is also why `never` and `as-needed` name the same setting. In a language
where the separator is optional everywhere except one spot where it is never
optional, "omit the ones that are not needed" and "omit all of them" describe
the same output. Larvae accepts both names, because which one a project reaches
for depends on the formatter it came from.
*/
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Semicolons {
    /// Only where Luau requires one.
    #[default]
    #[serde(alias = "as-needed")]
    Never,
    /// After every statement.
    Always,
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

/*
Turns a `local` that nothing reassigns into a `const`.

The `prefer_const` lint reports this shape. This option is the same rule with
the formatter making the edit instead of a person, which is why it carries the
same sub option under the same name.

Off by default. It rewrites a keyword, which is a bigger step than moving
spaces, and `require_binding` is off for the same reason.
*/
/*
Keys stay snake case here, which is larvae's rule for a key, and it is also
the spelling `[lint.options.prefer_const]` uses. One option under two tables
has to be written one way.
*/
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(deny_unknown_fields)]
pub struct PreferConst {
    #[serde(default)]
    pub enabled: bool,
    /*
    A binding the file mutates through a field keeps `local`.

    Off by default, because `const` is correct there: Luau enforces `const`
    against reassignment of the name and says nothing about the value, so
    `const t = {}` followed by `t.x = 1` compiles. The option is for a project
    that reads `local` as "this one changes", and it is the same choice,
    spelled the same way, as `[lint.options.prefer_const]`.
    */
    #[serde(default)]
    pub mutated_tables_stay_local: bool,
}

/*
Selects when a list between parentheses opens over several lines.

An argument list and a parameter list read the same way here, so one enum
serves both. The tables that use it are separate, because a project that wants
every call opened does not always want every declaration opened too.
*/
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ListExpansion {
    /// Open the list only where the line does not fit. This is the layout larvae always had.
    #[default]
    WhenNeeded,
    /// Open every list, whatever its width.
    Always,
    /*
    Keep the list on one line.

    A value inside it can still open, so `f(t)` with a large `t` opens the
    table and not the list. The line can run past `column_width`, because the
    option asks for that.
    */
    Never,
}

/*
Selects the shape of an opened argument list.

```lua
-- one-per-line
Colors:Apply(
    frame,
    "Rarity",
    { Children = { stroke } }
)

-- hug-last
Colors:Apply(frame, "Rarity", {
    Children = { stroke },
})
```
*/
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CallStyle {
    /// Every argument on a line of its own.
    #[default]
    OnePerLine,
    /*
    The arguments stay on the line of the call, and the last one opens.

    This applies where the last argument is a table, a function, or a string
    that carries its own newlines. Those are the values that read as a block.
    A call whose last argument is none of them opens one per line, because
    there is nothing there to hold the shape.
    */
    HugLast,
}

/// How the formatter lays out the argument list of a call.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct FunctionCall {
    #[serde(default)]
    pub expand: ListExpansion,
    #[serde(default)]
    pub style: CallStyle,
    /// The indent levels that an opened argument takes.
    #[serde(default = "default_list_indent")]
    pub indent: usize,
}

/// How the formatter lays out the parameter list of a declaration.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct FunctionDeclaration {
    #[serde(default)]
    pub expand: ListExpansion,
    /// The indent levels that an opened parameter takes.
    #[serde(default = "default_list_indent")]
    pub indent: usize,
}

fn default_list_indent() -> usize {
    1
}

/// Selects when the formatter opens an `if ... then ... else ...` expression over several lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum IfExpansion {
    /// Keep the expression on one line where it fits. This is the layout larvae always had.
    #[default]
    Never,
    /// Open every expression, whatever its width.
    Always,
    /// Open an expression that is wider than `width`.
    WhenLarge,
}

/*
Selects the shape of an opened `if` expression.

Both shapes put one clause on a line. They differ on which side of the
keyword the line breaks, and so on where the reader's eye finds the keyword.
*/
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum IfStyle {
    /*
    The value goes below its keyword, in the shape of an `if` statement.

    ```
    local a = if bar then
        "baz"
    else
        "foo"
    ```
    */
    #[default]
    Block,
    /*
    The keyword starts the line and takes its value.

    ```
    local a = if bar
        then "baz"
        else "foo"
    ```

    This is the position the formatter already gives the operator of a long
    binary chain. The reader finds the keyword at one column instead of at
    the uneven right edge.
    */
    Leading,
}

/// Selects where the `if` sits when the formatter opens the expression.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum IfPlacement {
    /// `local a = if cond then`. The `if` stays on the line of the binding.
    #[default]
    SameLine,
    /// `local a =`, and the `if` starts the line below.
    NextLine,
}

/*
How the formatter lays out an `if` expression.

Luau has an `if` expression, and stylua has no option for it, so a long one
runs off to the right. The options here open it over several lines instead.
*/
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct IfExpression {
    #[serde(default)]
    pub expand: IfExpansion,
    /*
    The width at which `when-large` opens an expression.

    This width also governs a nested expression, in every mode. An `if`
    inside an `if` that opens with its parent gives a stair of keywords for
    an expression that reads well on one line. So the inner one waits until
    it earns the room on its own.
    */
    #[serde(default = "default_if_width")]
    pub width: usize,
    #[serde(default)]
    pub style: IfStyle,
    #[serde(default)]
    pub placement: IfPlacement,
    /// The indent levels that a continuation line of the expression takes.
    #[serde(default = "default_if_indent")]
    pub indent: usize,
}

impl Default for FunctionCall {
    fn default() -> Self {
        Self {
            expand: ListExpansion::default(),
            style: CallStyle::default(),
            indent: default_list_indent(),
        }
    }
}

impl Default for FunctionDeclaration {
    fn default() -> Self {
        Self {
            expand: ListExpansion::default(),
            indent: default_list_indent(),
        }
    }
}

impl Default for IfExpression {
    fn default() -> Self {
        Self {
            expand: IfExpansion::default(),
            width: default_if_width(),
            style: IfStyle::default(),
            placement: IfPlacement::default(),
            indent: default_if_indent(),
        }
    }
}

/*
Half of the default `column_width`.

An `if` expression is one value inside a statement, and it is rarely the
whole line. A value that takes more than half the budget is the one that
makes the line hard to read.
*/
fn default_if_width() -> usize {
    60
}

fn default_if_indent() -> usize {
    1
}

/*
How a table type is laid out.

The emitter replays a type from its tokens, and a replay keeps a type on
one line. A table type is the one type with a body, and a body of many
fields on one line is a wall. These options open it over several lines
instead, in every position a type takes: an alias, an annotation on a
binding or a parameter, a return type, and a `::` assertion.
*/
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct TableTypes {
    /// Off keeps the one line replay, byte for byte what larvae wrote before
    #[serde(default = "default_true")]
    pub enabled: bool,

    /*
    The width at which a table type opens.

    The measure is the flat form of one table alone, so a short table nested
    inside a long one stays on its line. The default matches
    `if_expression.width`: a type is part of a statement, and one that takes
    more than half the column budget is the one that makes the line hard to
    read.
    */
    #[serde(default = "default_table_type_width")]
    pub width: usize,

    /// The separator between fields; Luau reads both
    #[serde(default)]
    pub separator: TypeSeparator,
}

impl Default for TableTypes {
    fn default() -> Self {
        Self {
            enabled: default_true(),
            width: default_table_type_width(),
            separator: TypeSeparator::default(),
        }
    }
}

fn default_table_type_width() -> usize {
    60
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum TypeSeparator {
    #[default]
    Comma,
    Semicolon,
}

impl TypeSeparator {
    pub fn text(self) -> &'static str {
        match self {
            Self::Comma => ",",

            Self::Semicolon => ";",
        }
    }
}

/*
Selects the order of the properties inside a table type.

`none` is the default. A formatter that reorders code nobody asked it to
reorder loses the order the author chose, and that order often groups the
properties by what they mean.

The measure is the length of the property name. A type reads as a block, and
the ragged left edge of its values is what makes a block hard to scan. Two
names of one length sort alphabetically, so the output does not depend on the
order of the input.
*/
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum PropertyOrder {
    /// Keep the order the author wrote.
    #[default]
    None,
    /// The shortest name first.
    Ascending,
    /// The longest name first.
    Descending,
}

/*
Sorts the properties of a table type by the length of their names.

This reaches a type position and nothing else: an alias, an annotation on a
binding or a parameter, a return type, and a `::` assertion. A value table
keeps the order the author wrote, because there the order is data.

Two limits come with it. A table type that holds a comment prints as the
author wrote it, so nothing inside it moves, and `emit::sort_properties`
states why. And the sort reads the fields that the table type layout finds,
so `table_types.enabled = false` leaves every order as written.
*/
/*
Keys stay snake case here, which is larvae's rule for a key.
*/
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SortTableTypes {
    #[serde(default)]
    pub order: PropertyOrder,

    /*
    An indexer such as `[number]: any` goes above the named properties.

    On by default, and it costs nothing until `order` asks for a sort. An
    indexer states the shape of every key instead of one key, so it reads as
    the heading of the table.

    Off puts the indexer in its sorted position. An indexer names nothing, so
    it measures zero: it lands first under `ascending`, which is where
    `indexer_first` puts it anyway, and last under `descending`.
    */
    #[serde(default = "default_true")]
    pub indexer_first: bool,
}

impl Default for SortTableTypes {
    fn default() -> Self {
        Self {
            order: PropertyOrder::default(),
            indexer_first: default_true(),
        }
    }
}

/// Selects when the formatter opens a union or an intersection over several lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum TypeExpansion {
    /*
    Leave the chain to the replay, which is the layout larvae always had.

    The operator breaks no line here, so a chain wider than `column_width`
    runs past it.
    */
    #[default]
    Auto,
    /// Every member on a line of its own, whatever its width.
    Always,
    /*
    One line, whatever `table_types.width` says about a table type inside it.

    `column_width` outranks this. A chain that does not fit the line opens one
    member per line, because a line that runs off the screen is not the one
    line the option asked for. So the order is `column_width` first, then
    `type_operators`, then `table_types.width`.
    */
    Never,
}

/*
How the formatter lays out a union and an intersection.

`|` and `&` share one option, because they share one shape: members with an
operator between them. A project that opens the one and closes the other has
two styles for one construct.

An opened chain takes the leading operator, which is the position the
formatter already gives the operator of a long binary chain. The reader finds
each member at one column instead of at the uneven right edge.
*/
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct TypeOperators {
    #[serde(default)]
    pub expand: TypeExpansion,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct FmtConfig {
    /*
    Whether `larvae fmt` changes this project at all.

    `false` leaves every file as it is. A project that wants larvae for its
    lints and its requires, and keeps another formatter, says so here rather
    than by not running the command.
    */
    #[serde(default = "default_true")]
    pub enabled: bool,

    /*
    Whether larvae's own defaults apply, or only the ones the project wrote.

    Three states, as `[lint] recommended` has them. Absent and `true` both
    mean larvae's defaults apply, which is what larvae always did. `false`
    starts from a base that changes as little as possible, and the project
    builds up from there.

    Only the options where larvae has an opinion move: a trailing comma that
    holds a table open, the space inside a brace, and the trailing comma
    larvae writes into a table it opened. Every other default is either what
    stylua does or a setting that changes nothing until a project asks for it.
    */
    #[serde(default)]
    pub recommended: Option<bool>,

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

    /// Turns a `local` that nothing reassigns into a `const`.
    #[serde(default)]
    pub prefer_const: PreferConst,
    /*
    What to do with a required module that nothing uses.

    `ignore` by default, because `remove` can stop a module from running at
    all. See [`UnusedImports`].
    */
    #[serde(default)]
    pub unused_imports: UnusedImports,

    /// Selects whether a statement ends with a semicolon.
    #[serde(default)]
    pub semicolons: Semicolons,

    /// Selects how an `if` expression opens over several lines.
    #[serde(default)]
    pub if_expression: IfExpression,

    /// Selects how the argument list of a call opens over several lines.
    #[serde(default)]
    pub function_call: FunctionCall,

    /// Selects how the parameter list of a declaration opens over several lines.
    #[serde(default)]
    pub function_declaration: FunctionDeclaration,

    /// Selects how a table type opens over several lines.
    #[serde(default)]
    pub table_types: TableTypes,

    /// Sorts the properties of a table type by the length of their names.
    #[serde(default)]
    pub sort_table_types: SortTableTypes,

    /// Selects how a union and an intersection open over several lines.
    #[serde(default)]
    pub type_operators: TypeOperators,

    /*
    Whether the file ends with a newline.

    On by default, and worth keeping on. POSIX defines a line as text up to a
    newline, so a file without a final one ends in something that is not a
    line: `wc -l` undercounts it, `cat` runs it into the next prompt, and git
    reports "\ No newline at end of file" on every diff that touches the last
    line. Off is here for a project whose tooling wants the bytes to stop where
    the code does.

    This is the single trailing newline only. Larvae removes whitespace at the
    end of every line whatever this says, because trailing spaces are invisible
    and have no reading in which they were intended.
    */
    #[serde(default = "default_true", alias = "insert_final_newline")]
    pub final_newline: bool,

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

    /// Globs that this area reads even when an exclude removes them
    #[serde(default)]
    pub include: Vec<String>,

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
    The base that `recommended = false` starts from.

    Three options carry an opinion of larvae's and change a file on their
    own. The rest of the defaults are stylua's, or they are a setting that
    does nothing until a project asks for it, so they stay as they are: a
    base that turned those off would not be neutral, it would be a third
    style nobody chose.
    */
    pub fn neutral() -> Self {
        Self {
            magic_trailing_comma: false,
            space_inside_braces: false,
            trailing_comma: false,
            ..Self::default()
        }
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

    /*
    Reports if an option gives a type a layout past the one line replay.

    Two options do, and either one on is enough. With both off the emitter
    replays the tokens of a type and writes the bytes it always wrote.
    */
    pub fn lays_out_types(&self) -> bool {
        self.table_types.enabled || self.type_operators.expand != TypeExpansion::Auto
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
        self.excludes_under(root, &[], &[])
    }

    /// The same, with the root level lists that every area inherits
    pub fn excludes_under(
        &self,
        root: &Path,
        root_include: &[String],
        root_exclude: &[String],
    ) -> Result<Excludes> {
        Excludes::layered(
            root,
            &self.include,
            &self.exclude,
            root_include,
            root_exclude,
        )
        .context("[fmt]")
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
        /*
        `recommended` decides what the merge starts from, so an option the
        project wrote lands on top of it in the usual way and nothing has to
        track which keys were written.

        A `stylua.toml` states the style of the project already, so it is the
        base where one exists, whatever `recommended` says.
        */
        let asked = larvae
            .and_then(|value| value.get("recommended"))
            .and_then(toml::Value::as_bool);

        let base = match asked {
            Some(false) => Self::neutral(),

            _ => Self::default(),
        };

        let mut config = stylua_file(root)?.unwrap_or(base);

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
            let key = canonical(key);

            match (base_table.get_mut(key), value) {
                (Some(toml::Value::Table(under)), toml::Value::Table(on_top)) => {
                    for (k, v) in on_top {
                        under.insert(k.clone(), v.clone());
                    }
                }

                _ => {
                    base_table.insert(key.to_string(), value.clone());
                }
            }
        }

        base.try_into().context("[fmt]")
    }
}

/*
Returns the larvae name of an option that another tool spells differently.

A serde alias is not enough on its own. The merge writes the whole config to a
table first, so that table already holds the larvae name. An alias in the
config of the user then arrives as a second key for one field, and serde
refuses the pair as a duplicate field. So the merge renames the key before it
writes it.
*/
fn canonical(key: &str) -> &str {
    match key {
        // the editorconfig name
        "insert_final_newline" => "final_newline",

        other => other,
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

    #[test]
    fn recommended_false_drops_the_opinions_and_keeps_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        let over = toml::from_str::<toml::Value>("recommended = false").unwrap();
        let cfg = FmtConfig::discover(dir.path(), Some(&over)).unwrap();

        assert!(!cfg.magic_trailing_comma);
        assert!(!cfg.space_inside_braces);
        assert!(!cfg.trailing_comma);

        // Everything else is stylua's default, and `recommended` does not move it.
        let same = FmtConfig::default();

        assert_eq!(cfg.column_width, same.column_width);
        assert_eq!(cfg.indent_width, same.indent_width);
        assert!(cfg.final_newline);
    }

    #[test]
    fn a_key_the_project_wrote_beats_the_neutral_base() {
        let dir = tempfile::tempdir().unwrap();
        let text = "recommended = false\nspace_inside_braces = true";
        let over = toml::from_str::<toml::Value>(text).unwrap();
        let cfg = FmtConfig::discover(dir.path(), Some(&over)).unwrap();

        assert!(cfg.space_inside_braces);
        assert!(!cfg.trailing_comma);
    }

    #[test]
    fn recommended_absent_is_the_same_config_as_before_the_option() {
        let dir = tempfile::tempdir().unwrap();
        let over = toml::from_str::<toml::Value>("column_width = 100").unwrap();
        let cfg = FmtConfig::discover(dir.path(), Some(&over)).unwrap();

        assert!(cfg.magic_trailing_comma);
        assert!(cfg.space_inside_braces);
        assert!(cfg.trailing_comma);
        assert_eq!(cfg.column_width, 100);
    }

    #[test]
    fn the_enabled_switch_defaults_to_on() {
        assert!(FmtConfig::default().enabled);

        let dir = tempfile::tempdir().unwrap();
        let over = toml::from_str::<toml::Value>("enabled = false").unwrap();

        assert!(
            !FmtConfig::discover(dir.path(), Some(&over))
                .unwrap()
                .enabled
        );
    }
}
