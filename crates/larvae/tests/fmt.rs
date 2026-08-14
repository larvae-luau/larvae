/*!
These tests cover the formatter.

Most tests pair a source with its expected output, and this kind is easy to
read. A few tests assert properties over every sample at once. This kind finds
the failures that no person wrote a case for. The properties are: the output
must parse, a second format must change nothing, no line can end in
whitespace, and every comment must stay.
*/

use larvae::fmt::config::{
    CallParens, CollapseSimpleStatement, IndentType, LineEndings, QuoteStyle,
    SpaceAfterFunctionNames,
};
use larvae::fmt::{FmtConfig, format};

fn fmt(src: &str) -> String {
    format(src, &FmtConfig::default()).expect("formats")
}

fn fmt_with(src: &str, cfg: FmtConfig) -> String {
    format(src, &cfg).expect("formats")
}

fn narrow(width: usize) -> FmtConfig {
    FmtConfig {
        column_width: width,
        ..Default::default()
    }
}

/// This list holds each construct that must survive a round trip. The
/// property tests use it.
const SAMPLES: &[&str] = &[
    "local a = 1\n",
    "local a, b: number = 1, 2\n",
    "local t = { a = 1, [2] = 'x', 3 }\n",
    "local f = function(a, b) return a + b end\n",
    "function M.thing:method(a: number): string\n\treturn tostring(a)\nend\n",
    "if a then\n\tb()\nelseif c then\n\td()\nelse\n\te()\nend\n",
    "for i = 1, 10, 2 do\n\tprint(i)\nend\n",
    "for k, v in pairs(t) do\n\tprint(k, v)\nend\n",
    "while true do\n\tbreak\nend\n",
    "repeat\n\tx()\nuntil done\n",
    "do\n\tlocal scoped = 1\nend\n",
    "export type Thing = { a: number, b: string? }\n",
    "local x = a and b or c\n",
    "local x = -y + #z\n",
    "local x = (a + b) * c\n",
    "local s = `interp {value} here`\n",
    "local s = [[\nlong\n]]\n",
    "-- leading\nlocal a = 1 -- trailing\n\n-- after a gap\nlocal b = 2\n",
    "--[[\n\ta long comment\n]]\nlocal a = 1\n",
    "local x = obj:method(1):chain(2).field\n",
    "return\n",
    "local x = value :: SomeType\n",
    "local x = if cond then a else b\n",
    "continue\n",
    "local f = require('@pkg/thing')\n",
    "do -- a note on the keyword\n\tx()\nend\n",
];

// --- properties ------------------------------------------------------------

#[test]
fn formatting_is_idempotent() {
    for src in SAMPLES {
        let once = fmt(src);
        let twice = fmt(&once);

        assert_eq!(once, twice, "unstable for {src:?}");
    }
}

/// The output must be in the same language as the input.
#[test]
fn output_always_parses() {
    for src in SAMPLES {
        let out = fmt(src);
        let lexed = larvae::syntax::lexer::lex(&out)
            .unwrap_or_else(|e| panic!("{src:?} produced unlexable output, {}", e.message));

        larvae::syntax::parser::parse(&out, &lexed.toks)
            .unwrap_or_else(|e| panic!("{src:?} produced unparsable output, {}", e.message));
    }
}

#[test]
fn no_output_line_ends_in_whitespace() {
    for src in SAMPLES {
        for line in fmt(src).lines() {
            assert_eq!(line, line.trim_end(), "trailing whitespace from {src:?}");
        }
    }
}

/*
A lost comment causes users to stop trusting a formatter. A case-by-case test
does not show this failure, because the output still looks correct.
*/
#[test]
fn every_comment_survives() {
    for src in SAMPLES {
        let out = fmt(src);
        let before = larvae::syntax::lexer::lex(src).expect("lexes");

        for (start, end) in &before.comments {
            let text = src[*start as usize..*end as usize].trim_end();

            assert!(out.contains(text), "{src:?} lost the comment {text:?}");
        }
    }
}

/// A file that is already in the target style must stay byte-identical.
#[test]
fn formatted_input_is_left_alone() {
    let already = "local Players = game:GetService(\"Players\")\n\nlocal function greet(name: string): string\n\treturn \"hi \" .. name\nend\n\nreturn greet\n";

    assert_eq!(fmt(already), already);
}

#[test]
fn an_empty_file_stays_empty() {
    assert_eq!(fmt(""), "\n");
    assert_eq!(fmt("\n\n\n"), "\n");
}

#[test]
fn a_file_that_does_not_parse_is_refused_rather_than_mangled() {
    assert!(format("local = = =", &FmtConfig::default()).is_err());
    assert!(format("local x = [[unterminated", &FmtConfig::default()).is_err());
}

// --- layout ----------------------------------------------------------------

#[test]
fn indentation_follows_nesting() {
    let out = fmt("if a then if b then c() end end");

    assert_eq!(out, "if a then\n\tif b then\n\t\tc()\n\tend\nend\n");
}

#[test]
fn a_call_that_does_not_fit_breaks_one_argument_per_line() {
    assert_eq!(
        fmt_with("f(alpha, beta, gamma)", narrow(16)),
        "f(\n\talpha,\n\tbeta,\n\tgamma\n)\n"
    );
}

/// This is the purpose of a group-based layout: an outer break is not an
/// inner break.
#[test]
fn an_inner_call_stays_on_one_line_when_the_outer_one_breaks() {
    let out = fmt_with("outer(inner(a), someVeryLongArgumentName)", narrow(30));

    assert!(
        out.contains("inner(a)"),
        "inner should not break, got {out}"
    );
    assert!(out.contains("outer(\n"), "outer should break, got {out}");
}

#[test]
fn a_long_binary_chain_breaks_with_the_operator_leading() {
    let out = fmt_with("local ok = first and second and third", narrow(24));

    assert_eq!(out, "local ok = first\n\tand second\n\tand third\n");
}

/// Operators with different precedences must not break at the same place.
#[test]
fn a_chain_breaks_at_the_loosest_operator_first() {
    let out = fmt_with("local x = aaaa and bbbb or cccc and dddd", narrow(24));

    assert!(out.contains("\n\tor "), "should break at or, got {out}");
    assert!(
        out.contains("aaaa and bbbb"),
        "and should stay flat, got {out}"
    );
}

#[test]
fn a_callback_hugs_the_parentheses_instead_of_indenting_twice() {
    let out = fmt("thing:Connect(function(a)\n\tprint(a)\nend)");

    assert_eq!(out, "thing:Connect(function(a)\n\tprint(a)\nend)\n");
}

#[test]
fn a_table_assigned_to_a_name_hangs_off_the_equals() {
    let out = fmt("local t = {\n\ta = 1,\n}");

    assert_eq!(out, "local t = {\n\ta = 1,\n}\n");
}

// --- what the author wrote -------------------------------------------------

#[test]
fn one_blank_line_is_kept_and_several_collapse_to_one() {
    assert_eq!(
        fmt("local a = 1\n\nlocal b = 2\n"),
        "local a = 1\n\nlocal b = 2\n"
    );
    assert_eq!(
        fmt("local a = 1\n\n\n\nlocal b = 2\n"),
        "local a = 1\n\nlocal b = 2\n"
    );
}

#[test]
fn a_newline_after_the_brace_keeps_a_table_expanded() {
    let out = fmt("local t = {\n\ta = 1\n}");

    assert_eq!(out, "local t = {\n\ta = 1,\n}\n");
}

#[test]
fn a_table_written_on_one_line_stays_on_one_line() {
    assert_eq!(fmt("local t = { a = 1 }"), "local t = { a = 1 }\n");
}

/*
This is the magic trailing comma. An author's comma after the last argument is
a request to stay expanded. This prevents a reflow of the call each time an
argument name changes.
*/
#[test]
fn a_trailing_comma_keeps_a_one_line_table_expanded() {
    assert_eq!(fmt("local t = { a, b, }"), "local t = {\n\ta,\n\tb,\n}\n");
}

#[test]
fn without_the_trailing_comma_the_same_table_stays_flat() {
    assert_eq!(fmt("local t = { a, b }"), "local t = { a, b }\n");
}

#[test]
fn magic_trailing_comma_can_be_turned_off() {
    let cfg = FmtConfig {
        magic_trailing_comma: false,
        ..Default::default()
    };

    assert_eq!(fmt_with("local t = { a, b, }", cfg), "local t = { a, b }\n");
}

/// Luau rejects a trailing comma in a call, so the width is the only layout
/// input for a call.
#[test]
fn a_call_is_laid_out_by_width_alone() {
    assert_eq!(fmt("f(\n\ta,\n\tb\n)"), "f(a, b)\n");
    assert!(format("f(a, b,)", &FmtConfig::default()).is_err());
}

#[test]
fn a_stray_semicolon_is_dropped() {
    assert_eq!(
        fmt("local a = 1;\n;\nlocal b = 2\n"),
        "local a = 1\nlocal b = 2\n"
    );
}

// --- comments --------------------------------------------------------------

#[test]
fn a_trailing_comment_stays_on_its_line() {
    assert_eq!(fmt("local a = 1   -- why\n"), "local a = 1 -- why\n");
}

#[test]
fn a_leading_comment_keeps_its_own_line_and_its_gap() {
    let out = fmt("local a = 1\n\n-- section\nlocal b = 2\n");

    assert_eq!(out, "local a = 1\n\n-- section\nlocal b = 2\n");
}

#[test]
fn a_comment_on_the_opening_keyword_is_not_lost() {
    assert_eq!(fmt("do -- note\n\tx()\nend"), "do -- note\n\tx()\nend\n");
}

#[test]
fn a_comment_at_the_end_of_a_block_is_kept() {
    assert_eq!(
        fmt("do\n\tx()\n\t-- last\nend"),
        "do\n\tx()\n\t-- last\nend\n"
    );
}

#[test]
fn a_comment_at_the_end_of_the_file_is_kept() {
    assert_eq!(
        fmt("local a = 1\n-- the end\n"),
        "local a = 1\n-- the end\n"
    );
}

/// A long comment's own lines are its content and must not be re-indented
#[test]
fn a_long_comment_keeps_its_interior_exactly() {
    let src = "do\n\t--[[\nnot indented\n\t]]\n\tx()\nend";

    assert!(fmt(src).contains("\nnot indented\n"));
}

#[test]
fn a_shebang_style_directive_survives() {
    assert!(fmt("--!strict\nlocal a = 1\n").starts_with("--!strict\n"));
}

// --- strings ---------------------------------------------------------------

#[test]
fn quotes_normalise_to_double_by_default() {
    assert_eq!(fmt("local s = 'hi'"), "local s = \"hi\"\n");
}

/// A quote change is not a simple swap. The escapes must change with the
/// quotes.
#[test]
fn requoting_fixes_the_escapes() {
    assert_eq!(fmt(r#"local s = 'it\'s'"#), "local s = \"it's\"\n");
    assert_eq!(
        fmt_with(
            r#"local s = "say \"hi\"""#,
            FmtConfig {
                quote_style: QuoteStyle::ForceSingle,
                ..Default::default()
            }
        ),
        "local s = 'say \"hi\"'\n"
    );
}

#[test]
fn the_quote_needing_fewer_escapes_wins() {
    // A double quote inside means single quotes need no escapes, so the
    // formatter keeps them.
    assert_eq!(fmt(r#"local s = 'say "hi"'"#), "local s = 'say \"hi\"'\n");
}

#[test]
fn preserve_leaves_every_literal_alone() {
    let cfg = FmtConfig {
        quote_style: QuoteStyle::Preserve,
        ..Default::default()
    };

    assert_eq!(fmt_with("local s = 'hi'", cfg), "local s = 'hi'\n");
}

#[test]
fn a_long_string_is_never_requoted() {
    assert_eq!(
        fmt("local s = [[it's \"both\"]]"),
        "local s = [[it's \"both\"]]\n"
    );
}

#[test]
fn escape_sequences_are_left_intact() {
    assert_eq!(
        fmt(r#"local s = "a\tb\nc\\d""#),
        "local s = \"a\\tb\\nc\\\\d\"\n"
    );
}

// --- options ---------------------------------------------------------------

#[test]
fn spaces_can_replace_tabs() {
    let cfg = FmtConfig {
        indent_type: IndentType::Spaces,
        indent_width: 2,
        ..Default::default()
    };

    assert_eq!(fmt_with("do\nx()\nend", cfg), "do\n  x()\nend\n");
}

#[test]
fn windows_line_endings_apply_everywhere() {
    let cfg = FmtConfig {
        line_endings: LineEndings::Windows,
        ..Default::default()
    };

    assert_eq!(fmt_with("do\nx()\nend", cfg), "do\r\n\tx()\r\nend\r\n");
}

/// The user asked for this option by name: a space before the parentheses.
#[test]
fn space_after_function_names_targets_definitions_and_calls_separately() {
    let defs = FmtConfig {
        space_after_function_names: SpaceAfterFunctionNames::Definitions,
        ..Default::default()
    };

    assert_eq!(
        fmt_with("local function f(a) return a end", defs),
        "local function f (a)\n\treturn a\nend\n"
    );

    let calls = FmtConfig {
        space_after_function_names: SpaceAfterFunctionNames::Calls,
        ..Default::default()
    };

    assert_eq!(fmt_with("print(1)", calls), "print (1)\n");
}

#[test]
fn inner_spacing_is_configurable() {
    let cfg = FmtConfig {
        space_inside_parens: true,
        space_inside_brackets: true,
        space_inside_braces: false,
        ..Default::default()
    };

    assert_eq!(fmt_with("f(a)", cfg.clone()), "f( a )\n");
    assert_eq!(
        fmt_with("local x = t[k]", cfg.clone()),
        "local x = t[ k ]\n"
    );
    assert_eq!(fmt_with("local t = { a }", cfg), "local t = {a}\n");
}

#[test]
fn call_parentheses_can_be_dropped_for_a_single_string_or_table() {
    let no_string = FmtConfig {
        call_parentheses: CallParens::NoSingleString,
        ..Default::default()
    };

    assert_eq!(fmt_with(r#"require("x")"#, no_string), "require \"x\"\n");

    let no_table = FmtConfig {
        call_parentheses: CallParens::NoSingleTable,
        ..Default::default()
    };

    assert_eq!(fmt_with("f({ a = 1 })", no_table), "f { a = 1 }\n");
}

#[test]
fn call_parentheses_are_added_by_default() {
    assert_eq!(fmt("require 'x'"), "require(\"x\")\n");
    assert_eq!(fmt("f { a = 1 }"), "f({ a = 1 })\n");
}

#[test]
fn collapse_simple_statement_folds_a_one_line_body() {
    let cfg = FmtConfig {
        collapse_simple_statement: CollapseSimpleStatement::Always,
        ..Default::default()
    };

    assert_eq!(
        fmt_with("local function f(a)\n\treturn a\nend", cfg.clone()),
        "local function f(a) return a end\n"
    );

    assert_eq!(
        fmt_with("if a then\n\treturn\nend", cfg),
        "if a then return end\n"
    );
}

#[test]
fn collapsing_never_swallows_a_comment() {
    let cfg = FmtConfig {
        collapse_simple_statement: CollapseSimpleStatement::Always,
        ..Default::default()
    };

    let out = fmt_with("local function f(a)\n\t-- why\n\treturn a\nend", cfg);

    assert!(out.contains("-- why"), "comment lost, got {out}");
    assert!(out.contains('\n'), "should not have collapsed, got {out}");
}

#[test]
fn collapse_is_off_by_default() {
    assert_eq!(
        fmt("local function f(a) return a end"),
        "local function f(a)\n\treturn a\nend\n"
    );
}

// --- types -----------------------------------------------------------------

#[test]
fn a_type_annotation_is_normalised_but_not_restructured() {
    assert_eq!(
        fmt("local x:   Array < string >  = {}"),
        "local x: Array<string> = {}\n"
    );

    assert_eq!(
        fmt("local f: (  number,string )->boolean"),
        "local f: (number, string) -> boolean\n"
    );
    assert_eq!(
        fmt("local t: {x:number,y:number}"),
        "local t: { x: number, y: number }\n"
    );
    assert_eq!(fmt("local u: A|B&C"), "local u: A | B & C\n");
    assert_eq!(fmt("local o: string ?"), "local o: string?\n");
    assert_eq!(
        fmt("local m: {[string] : number}"),
        "local m: { [string]: number }\n"
    );
}

#[test]
fn a_type_alias_keeps_its_shape() {
    let src = "export type Handler<T> = (T) -> ()\n";

    assert_eq!(fmt(src), src);
}

#[test]
fn generics_and_return_types_come_through() {
    let src = "local function map<T, U>(t: { T }, f: (T) -> U): { U }\n\treturn t\nend\n";

    assert_eq!(fmt(src), src);
}

/// This is the turbofish syntax. The parser needed a fix to accept it.
#[test]
fn explicit_type_instantiation_survives() {
    assert_eq!(
        fmt("local a = charm.atom<<number>>()"),
        "local a = charm.atom<<number>>()\n"
    );
    assert_eq!(
        fmt("local a = charm.atom<<(number, string)>>()"),
        "local a = charm.atom<<(number, string)>>()\n"
    );
}

#[test]
fn attributes_stay_above_their_function() {
    let src = "@native\nlocal function hot()\n\treturn 1\nend\n";

    assert_eq!(fmt(src), src);
}

// --- sort_requires ---------------------------------------------------------

use larvae::fmt::config::{RequireGrouping, SortRequires};

fn sorting(grouping: RequireGrouping) -> FmtConfig {
    FmtConfig {
        sort_requires: SortRequires {
            enabled: true,
            grouping,
        },
        ..Default::default()
    }
}

#[test]
fn requires_are_left_alone_unless_asked() {
    let src = "local b = require(\"b\")\nlocal a = require(\"a\")\n";

    assert_eq!(fmt(src), src);
}

#[test]
fn a_run_of_requires_sorts() {
    let out = fmt_with(
        "local c = require(\"c\")\nlocal a = require(\"a\")\nlocal b = require(\"b\")\n",
        sorting(RequireGrouping::Flat),
    );

    assert_eq!(
        out,
        "local a = require(\"a\")\nlocal b = require(\"b\")\nlocal c = require(\"c\")\n"
    );
}

/// The sort must never move a require past code that is not a require.
#[test]
fn a_statement_between_two_requires_breaks_the_run() {
    let src = "local c = require(\"c\")\nsideEffect()\nlocal a = require(\"a\")\n";

    assert_eq!(fmt_with(src, sorting(RequireGrouping::Flat)), src);
}

/// A blank line shows that the author groups the requires. The sort keeps
/// that decision.
#[test]
fn a_blank_line_separates_two_runs_that_sort_independently() {
    let out = fmt_with(
        "local d = require(\"d\")\nlocal c = require(\"c\")\n\nlocal b = require(\"b\")\nlocal a = require(\"a\")\n",
        sorting(RequireGrouping::Flat),
    );

    assert_eq!(
        out,
        "local c = require(\"c\")\nlocal d = require(\"d\")\n\nlocal a = require(\"a\")\nlocal b = require(\"b\")\n"
    );
}

/// This test shows why the emitter cannot write statements directly from the
/// list.
#[test]
fn a_comment_moves_with_the_require_it_describes() {
    let out = fmt_with(
        "-- about c\nlocal c = require(\"c\")\n-- about a\nlocal a = require(\"a\")\n",
        sorting(RequireGrouping::Flat),
    );

    assert_eq!(
        out,
        "-- about a\nlocal a = require(\"a\")\n-- about c\nlocal c = require(\"c\")\n"
    );
}

#[test]
fn a_trailing_comment_moves_with_its_require_too() {
    let out = fmt_with(
        "local c = require(\"c\") -- see c\nlocal a = require(\"a\") -- see a\n",
        sorting(RequireGrouping::Flat),
    );

    assert_eq!(
        out,
        "local a = require(\"a\") -- see a\nlocal c = require(\"c\") -- see c\n"
    );
}

#[test]
fn by_kind_groups_aliases_then_absolute_then_relative() {
    let out = fmt_with(
        "local r = require(\"./sibling\")\nlocal g = require(\"game/Thing\")\nlocal p = require(\"@pkg/signal\")\n",
        sorting(RequireGrouping::ByKind),
    );

    assert_eq!(
        out,
        "local p = require(\"@pkg/signal\")\n\nlocal g = require(\"game/Thing\")\n\nlocal r = require(\"./sibling\")\n"
    );
}

/// A computed require has no order that is safe to change.
#[test]
fn a_computed_require_is_not_sorted_and_breaks_the_run() {
    let src =
        "local c = require(\"c\")\nlocal x = require(base .. name)\nlocal a = require(\"a\")\n";

    assert_eq!(fmt_with(src, sorting(RequireGrouping::Flat)), src);
}

#[test]
fn sorting_is_idempotent_and_keeps_every_comment() {
    let src = "-- c\nlocal c = require(\"c\") -- t\nlocal a = require(\"a\")\n\nlocal b = require(\"@x/b\")\n";
    let cfg = sorting(RequireGrouping::ByKind);
    let once = fmt_with(src, cfg.clone());
    let twice = fmt_with(&once, cfg);

    assert_eq!(once, twice, "unstable");

    for text in ["-- c", "-- t"] {
        assert!(once.contains(text), "lost {text}, got {once}");
    }
}

// --- regressions found by review -------------------------------------------

/// Two adjacent minus signs make a line comment.
#[test]
fn nested_unary_minus_keeps_its_space() {
    assert_eq!(fmt("local y = - -x"), "local y = - -x\n");
    assert_eq!(fmt("local y = -  -  -a"), "local y = - - -a\n");
    assert_eq!(
        fmt("local y = -x"),
        "local y = -x\n",
        "one minus still hugs"
    );
}

/// A statement that starts with `(` continues the line above as a call.
#[test]
fn a_dropped_semicolon_is_put_back_where_it_is_load_bearing() {
    assert_eq!(fmt("local a = b\n;(c)()\n"), "local a = b\n;(c)()\n");
    assert_eq!(fmt("local x = a;\n(f)()\n"), "local x = a\n;(f)()\n");
}

/// A `[` directly before a `[[ ]]` string opens a long string instead.
#[test]
fn a_bracket_index_of_a_long_string_keeps_its_space() {
    assert_eq!(fmt("local x = t[ [[k]] ]"), "local x = t[ [[k]] ]\n");
    assert_eq!(
        fmt("local u = { [ [[key]] ] = 1 }"),
        "local u = { [ [[key]] ] = 1 }\n"
    );
    assert_eq!(fmt("local x = t[ [=[k]=] ]"), "local x = t[ [=[k]=] ]\n");
}

#[test]
fn const_function_stays_const() {
    assert_eq!(fmt("const function f() end"), "const function f()\nend\n");
    assert_eq!(fmt("local function f() end"), "local function f()\nend\n");
}

/// A type can hold an expression through typeof. There, the tokens in `and 1`
/// must keep their space.
#[test]
fn a_word_and_a_number_in_a_type_keep_their_space() {
    assert_eq!(
        fmt("local v: typeof(x and 1) = nil"),
        "local v: typeof(x and 1) = nil\n"
    );
    assert_eq!(
        fmt("local w: typeof(2 or b) = nil"),
        "local w: typeof(2 or b) = nil\n"
    );
    assert_eq!(fmt("type T = typeof(1 .. 2)"), "type T = typeof(1 .. 2)\n");
}

/// A type function body is arbitrary Luau code, and a token replay damages it.
#[test]
fn a_type_function_body_is_emitted_as_written() {
    let src = "type function K(t)\n\tlocal a = 1\n\treturn t\nend\n";

    assert_eq!(fmt(src), src);
}

/// A `--!` directive has file scope, and it works only above every statement.
#[test]
fn sorting_requires_leaves_the_strict_directive_at_the_top() {
    let out = fmt_with(
        "--!strict\nlocal zzz = require(\"./z\")\nlocal aaa = require(\"./a\")\nreturn nil\n",
        sorting(RequireGrouping::Flat),
    );

    assert_eq!(
        out, "--!strict\nlocal aaa = require(\"./a\")\nlocal zzz = require(\"./z\")\nreturn nil\n",
        "the directive stays put and the requires still sort"
    );
}

// --- comments inside a statement -------------------------------------------

#[test]
fn a_comment_above_a_table_field_is_kept() {
    let src = "local t = {\n\t-- describes a\n\ta = 1,\n}\n";

    assert_eq!(fmt(src), src);
}

#[test]
fn a_comment_after_a_table_field_is_kept() {
    let src = "local t = {\n\ta = 1, -- one\n\tb = 2, -- two\n}\n";

    assert_eq!(fmt(src), src);
}

/// A table that holds only a comment is not the same as an empty table.
#[test]
fn a_table_of_only_a_comment_keeps_it() {
    let src = "local t = {\n\t-- nothing yet\n}\n";

    assert_eq!(fmt(src), src);
    assert_eq!(fmt("local t = {}"), "local t = {}\n");
}

/// A line comment kept on one line inside braces would comment out the
/// closing brace.
#[test]
fn a_comment_forces_a_table_to_expand() {
    assert_eq!(
        fmt("local t = { a = 1, -- one\n}"),
        "local t = {\n\ta = 1, -- one\n}\n"
    );
}

#[test]
fn table_comments_survive_a_second_pass() {
    let src = "local t = {\n\t-- a\n\ta = 1, -- t\n\t-- b\n\tb = 2,\n}\n";

    assert_eq!(fmt(&fmt(src)), fmt(src));
    assert_eq!(fmt(src), src);
}

#[test]
fn a_comment_inside_a_type_is_kept() {
    let src = "local function f(\n\toriginalError: (Error & { extensions: any? }) -- new syntax\n): GError?\n\treturn nil\nend\n";

    assert!(fmt(src).contains("-- new syntax"));
}

#[test]
fn a_comment_inside_a_parameter_list_is_kept() {
    let src = "local function x(...--[[comment here]])\nend\n";

    assert!(fmt(src).contains("--[[comment here]]"));
}

#[test]
fn a_comment_among_call_arguments_is_kept() {
    let src = "f(\n\t-- why\n\tx\n)\n";

    assert_eq!(fmt(src), src);
}

#[test]
fn a_comment_after_a_call_argument_is_kept() {
    let src = "g(\n\ta, -- first\n\tb -- second\n)\n";

    assert_eq!(fmt(src), src);
}

#[test]
fn call_argument_comments_survive_a_second_pass() {
    let src = "f(\n\t-- why\n\tx\n)\n";

    assert_eq!(fmt(&fmt(src)), fmt(src));
}

/// The formatter lays out a call with no comments by width, exactly as before.
#[test]
fn placing_argument_comments_does_not_change_a_call_without_any() {
    assert_eq!(fmt("f(\n\ta,\n\tb\n)\n"), "f(a, b)\n");
}

/*
This is the backstop. It makes a lost comment impossible, not only unlikely.
`larvae fmt` writes to disk. Thus, when the emitter cannot place a comment,
larvae refuses the file and does not drop the comment without a message. Each
position that the backstop catches is one the emitter must learn.

The test asserts this over every construct in SAMPLES with a comment added to
it. Thus, when a future emitter change stops placement of a comment, the test
fails, and no user loses a comment.
*/
#[test]
fn a_comment_is_placed_or_the_file_is_refused_never_dropped() {
    let with_comments = [
        "local a = 1 -- trailing\n",
        "-- leading\nlocal a = 1\n",
        "do -- on the keyword\n\tx()\nend\n",
        "do\n\tx()\n\t-- last\nend\n",
        "local t = {\n\t-- field\n\ta = 1,\n}\n",
        "f(\n\t-- argument\n\tx\n)\n",
        "local function g(a --[[ param ]])\nend\n",
        "local x: number -- annotated\n",
        "--[[\n\tlong\n]]\nlocal a = 1\n",
    ];

    for src in with_comments {
        let before = larvae::syntax::lexer::lex(src).expect("lexes");

        match format(src, &FmtConfig::default()) {
            Ok(out) => {
                for (start, end) in &before.comments {
                    let text = src[*start as usize..*end as usize].trim_end();

                    assert!(out.contains(text), "{src:?} silently lost {text:?}");
                }
            }

            // A refusal is the acceptable outcome. A drop is not.
            Err(e) => assert!(
                format!("{e:#}").contains("would drop the comment"),
                "{src:?} failed for an unrelated reason, {e:#}"
            ),
        }
    }
}

// --- block_newline_gaps ----------------------------------------------------

fn gaps(mode: larvae::fmt::config::BlockNewlineGaps) -> FmtConfig {
    FmtConfig {
        block_newline_gaps: mode,
        ..Default::default()
    }
}

/// This is the default, and stylua does the same.
#[test]
fn a_blank_at_the_edge_of_a_block_is_dropped_by_default() {
    let src = "local function f()\n\n\tbody()\n\nend\n";

    assert_eq!(fmt(src), "local function f()\n\tbody()\nend\n");
}

#[test]
fn preserve_keeps_the_blank_at_both_edges() {
    let src = "local function f()\n\n\tbody()\n\nend\n";

    assert_eq!(
        fmt_with(src, gaps(larvae::fmt::config::BlockNewlineGaps::Preserve)),
        src
    );
}

#[test]
fn preserve_keeps_one_edge_when_only_one_has_a_blank() {
    let cfg = gaps(larvae::fmt::config::BlockNewlineGaps::Preserve);

    assert_eq!(
        fmt_with("do\n\n\tx()\nend\n", cfg.clone()),
        "do\n\n\tx()\nend\n"
    );
    assert_eq!(fmt_with("do\n\tx()\n\nend\n", cfg), "do\n\tx()\n\nend\n");
}

/// Blank lines between statements separate ideas, so the formatter keeps them
/// in each mode.
#[test]
fn a_blank_between_statements_is_not_an_edge_gap() {
    let src = "do\n\ta()\n\n\tb()\nend\n";

    assert_eq!(fmt(src), src, "kept even at the default");
}

#[test]
fn preserving_gaps_is_still_idempotent() {
    let cfg = gaps(larvae::fmt::config::BlockNewlineGaps::Preserve);
    let src = "local function f()\n\n\tif a then\n\n\t\tb()\n\n\tend\n\nend\n";
    let once = fmt_with(src, cfg.clone());

    assert_eq!(fmt_with(&once, cfg), once);
}

// --- require_binding -------------------------------------------------------

fn binding(mode: larvae::fmt::config::RequireBinding) -> FmtConfig {
    FmtConfig {
        require_binding: mode,
        ..Default::default()
    }
}

#[test]
fn require_binding_preserves_what_was_written_by_default() {
    let src = "local A = require(\"@pkg/a\")\nconst B = require(\"@pkg/b\")\nreturn A, B\n";

    assert_eq!(fmt(src), src);
}

#[test]
fn const_converts_a_local_require() {
    assert_eq!(
        fmt_with(
            "local Signal = require(\"@pkg/signal\")\nreturn Signal\n",
            binding(larvae::fmt::config::RequireBinding::Const)
        ),
        "const Signal = require(\"@pkg/signal\")\nreturn Signal\n"
    );
}

#[test]
fn local_converts_a_const_require_back() {
    assert_eq!(
        fmt_with(
            "const Signal = require(\"@pkg/signal\")\nreturn Signal\n",
            binding(larvae::fmt::config::RequireBinding::Local)
        ),
        "local Signal = require(\"@pkg/signal\")\nreturn Signal\n"
    );
}

/*
This case would turn a working file into a syntax error. Luau enforces const:
`Variable 'M' is constant and may not be reassigned`.
*/
#[test]
fn a_require_whose_name_is_reassigned_keeps_local() {
    let src = "local M = require(\"@pkg/m\")\nM = fallback\nreturn M\n";

    assert_eq!(
        fmt_with(src, binding(larvae::fmt::config::RequireBinding::Const)),
        src
    );
}

#[test]
fn only_a_single_unannotated_binding_converts() {
    let cfg = binding(larvae::fmt::config::RequireBinding::Const);

    for src in [
        "local A, B = require(\"@pkg/a\"), require(\"@pkg/b\")\nreturn A, B\n",
        "local S: Signal = require(\"@pkg/signal\")\nreturn S\n",
        "local x = compute()\nreturn x\n",
    ] {
        assert_eq!(fmt_with(src, cfg.clone()), src, "{src:?}");
    }
}

/// A require inside a function body is still a require
#[test]
fn a_nested_require_converts_too() {
    let out = fmt_with(
        "local function f()\n\tlocal S = require(\"@pkg/s\")\n\treturn S\nend\nreturn f\n",
        binding(larvae::fmt::config::RequireBinding::Const),
    );

    assert!(out.contains("const S = require"), "{out}");
}

#[test]
fn converting_the_binding_is_idempotent() {
    let cfg = binding(larvae::fmt::config::RequireBinding::Const);
    let once = fmt_with("local S = require(\"@pkg/s\")\nreturn S\n", cfg.clone());

    assert_eq!(fmt_with(&once, cfg), once);
}
