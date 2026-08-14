/*!
These tests cover the lints.

Each lint gets a case that must fire and at least one case that must not fire.
A lint that fires on everything is worse than no lint. The near-miss cases
show where each rule stops.
*/

use std::path::Path;

use larvae::diag::Severity;
use larvae::lint::config::Level;
use larvae::lint::{LintConfig, lint};

/// This function returns the lints that fired, in source order.
fn names(src: &str) -> Vec<String> {
    fired(src, &LintConfig::default())
}

fn fired(src: &str, cfg: &LintConfig) -> Vec<String> {
    lint(Path::new("test.luau"), src, cfg)
        .expect("parses")
        .into_iter()
        .map(|d| {
            let m = d.message;
            let open = m.rfind('(').expect("the lint name is appended");

            m[open + 1..m.len() - 1].to_string()
        })
        .collect()
}

/// This function reports if one named lint fired.
fn fires(name: &str, src: &str) -> bool {
    names(src).iter().any(|n| n == name)
}

fn with(name: &str, level: Level) -> LintConfig {
    let mut cfg = LintConfig::default();
    cfg.rules.insert(name.to_string(), level);

    cfg
}

// --- the registry ----------------------------------------------------------

#[test]
fn every_lint_has_a_distinct_name_and_an_explanation() {
    let mut seen = Vec::new();

    for lint in larvae::lint::registry() {
        assert!(!lint.name().is_empty());
        assert!(!lint.about().is_empty(), "{} has no about", lint.name());
        assert!(
            lint.name()
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'),
            "{} should be snake_case",
            lint.name()
        );
        assert!(
            !seen.contains(&lint.name()),
            "{} is registered twice",
            lint.name()
        );

        seen.push(lint.name());
    }

    assert!(
        seen.len() >= 29,
        "expected the full set, found {}",
        seen.len()
    );
}

#[test]
fn a_lint_can_be_looked_up_by_name() {
    assert!(larvae::lint::find("divide_by_zero").is_some());
    assert!(larvae::lint::find("no_such_lint").is_none());
}

#[test]
fn clean_code_produces_nothing() {
    let src =
        "local function add(a: number, b: number): number\n\treturn a + b\nend\n\nreturn add\n";

    assert_eq!(names(src), Vec::<String>::new());
}

#[test]
fn a_file_that_does_not_parse_comes_back_as_one_diagnostic() {
    let err = lint(
        Path::new("test.luau"),
        "local = = =",
        &LintConfig::default(),
    )
    .expect_err("should not parse");

    assert_eq!(err.severity, Severity::Error);
    assert!(err.message.contains("syntax error"));
}

// --- levels and suppression ------------------------------------------------

#[test]
fn a_lint_set_to_allow_says_nothing() {
    let src = "local x = 1 / 0\n";

    assert!(fires("divide_by_zero", src));
    assert!(
        !fired(src, &with("divide_by_zero", Level::Allow))
            .iter()
            .any(|n| n == "divide_by_zero")
    );
}

#[test]
fn deny_makes_it_an_error_and_warn_leaves_it_a_warning() {
    let src = "local x = 1 / 0\n";

    let at = |level| {
        lint(Path::new("t.luau"), src, &with("divide_by_zero", level))
            .unwrap()
            .into_iter()
            .find(|d| d.message.contains("divide_by_zero"))
            .expect("it fired")
            .severity
    };

    assert_eq!(at(Level::Warn), Severity::Warning);
    assert_eq!(at(Level::Deny), Severity::Error);
}

#[test]
fn a_suppression_comment_silences_one_lint() {
    let src = "-- larvae: allow(divide_by_zero)\nlocal x = 1 / 0\n";

    assert!(!fires("divide_by_zero", src));
}

#[test]
fn a_suppression_for_a_different_lint_does_not_silence_this_one() {
    let src = "-- larvae: allow(shadowing)\nlocal x = 1 / 0\n";

    assert!(fires("divide_by_zero", src));
}

#[test]
fn selenes_suppression_spelling_works_too() {
    let src = "-- selene: allow(divide_by_zero)\nlocal x = 1 / 0\n";

    assert!(!fires("divide_by_zero", src));
}

// --- correctness -----------------------------------------------------------

#[test]
fn almost_swapped_catches_the_two_line_swap() {
    assert!(fires("almost_swapped", "a = b\nb = a\n"));
    assert!(fires("almost_swapped", "t.x = t.y\nt.y = t.x\n"));
}

#[test]
fn a_real_swap_is_not_reported() {
    assert!(!fires("almost_swapped", "a, b = b, a\n"));
    assert!(!fires("almost_swapped", "local tmp = a\na = b\nb = tmp\n"));
}

/// These are two unrelated assignments that are adjacent.
#[test]
fn assignments_that_are_not_a_swap_are_left_alone() {
    assert!(!fires("almost_swapped", "a = b\nc = d\n"));
    assert!(!fires("almost_swapped", "a = a\na = a\n"));
}

#[test]
fn compare_nan_catches_the_zero_over_zero_idiom() {
    assert!(fires("compare_nan", "if x == 0/0 then end\n"));
    assert!(fires("compare_nan", "if 0/0 ~= x then end\n"));
}

/// This is the correct nan test, and the lint must not report it.
#[test]
fn comparing_a_value_to_itself_is_not_reported() {
    assert!(!fires("compare_nan", "if x ~= x then end\n"));
}

#[test]
fn constant_table_comparison_catches_comparing_to_a_literal() {
    assert!(fires("constant_table_comparison", "if t == {} then end\n"));
    assert!(fires(
        "constant_table_comparison",
        "if t ~= { a = 1 } then end\n"
    ));
}

#[test]
fn comparing_two_named_tables_is_a_real_question() {
    assert!(!fires("constant_table_comparison", "if a == b then end\n"));
}

#[test]
fn divide_by_zero_catches_every_dividing_operator() {
    assert!(fires("divide_by_zero", "local x = n / 0\n"));
    assert!(fires("divide_by_zero", "local x = n // 0\n"));
    assert!(fires("divide_by_zero", "local x = n % 0\n"));
}

#[test]
fn dividing_by_something_that_might_be_zero_is_not_reported() {
    assert!(!fires("divide_by_zero", "local x = n / d\n"));
}

/// `0/0` is the written form of nan, and compare_nan owns that case.
#[test]
fn nan_is_not_also_reported_as_a_division() {
    assert!(!fires("divide_by_zero", "local x = 0/0\n"));
}

#[test]
fn duplicate_keys_catches_a_repeated_name_and_a_repeated_literal() {
    assert!(fires("duplicate_keys", "local t = { a = 1, a = 2 }\n"));
    assert!(fires(
        "duplicate_keys",
        "local t = { [1] = 'x', [1] = 'y' }\n"
    ));
    assert!(fires(
        "duplicate_keys",
        "local t = { ['k'] = 1, ['k'] = 2 }\n"
    ));
}

#[test]
fn distinct_keys_are_fine_and_a_computed_key_is_not_guessed_at() {
    assert!(!fires("duplicate_keys", "local t = { a = 1, b = 2 }\n"));
    assert!(!fires("duplicate_keys", "local t = { [i] = 1, [j] = 2 }\n"));
    assert!(!fires("duplicate_keys", "local t = { 1, 2, 3 }\n"));
}

#[test]
fn ifs_same_cond_catches_a_branch_that_can_never_run() {
    assert!(fires(
        "ifs_same_cond",
        "if a then x() elseif a then y() end\n"
    ));
}

#[test]
fn different_conditions_are_fine() {
    assert!(!fires(
        "ifs_same_cond",
        "if a then x() elseif b then y() end\n"
    ));
}

#[test]
fn if_same_then_else_catches_two_identical_branches() {
    assert!(fires(
        "if_same_then_else",
        "if a then\n\tx()\nelse\n\tx()\nend\n"
    ));
}

#[test]
fn branches_that_differ_are_fine() {
    assert!(!fires(
        "if_same_then_else",
        "if a then\n\tx()\nelse\n\ty()\nend\n"
    ));
}

#[test]
fn suspicious_reverse_loop_catches_a_countdown_without_a_step() {
    assert!(fires(
        "suspicious_reverse_loop",
        "for i = 10, 1 do print(i) end\n"
    ));
}

#[test]
fn a_countdown_with_a_negative_step_is_correct() {
    assert!(!fires(
        "suspicious_reverse_loop",
        "for i = 10, 1, -1 do print(i) end\n"
    ));
    assert!(!fires(
        "suspicious_reverse_loop",
        "for i = 1, 10 do print(i) end\n"
    ));
}

/// A limit that is not a literal can hold any value.
#[test]
fn a_loop_over_computed_bounds_is_not_guessed_at() {
    assert!(!fires(
        "suspicious_reverse_loop",
        "for i = n, 1 do print(i) end\n"
    ));
    assert!(!fires(
        "suspicious_reverse_loop",
        "for i = 10, #t do print(i) end\n"
    ));
}

#[test]
fn type_check_inside_call_catches_the_misplaced_parenthesis() {
    assert!(fires(
        "type_check_inside_call",
        "if type(x == 'number') then end\n"
    ));
    assert!(fires(
        "type_check_inside_call",
        "if typeof(x == 'Vector3') then end\n"
    ));
}

#[test]
fn the_correct_form_is_not_reported() {
    assert!(!fires(
        "type_check_inside_call",
        "if type(x) == 'number' then end\n"
    ));
}

#[test]
fn unbalanced_assignments_catches_both_directions() {
    assert!(fires("unbalanced_assignments", "local a, b = 1\n"));
    assert!(fires("unbalanced_assignments", "a, b = 1, 2, 3\n"));
}

#[test]
fn a_matched_assignment_is_fine() {
    assert!(!fires("unbalanced_assignments", "local a, b = 1, 2\n"));
}

/// A declaration of names for later values is normal, not an imbalance.
#[test]
fn a_declaration_with_no_values_is_not_reported() {
    assert!(!fires("unbalanced_assignments", "local a, b\n"));
}

/// A call can return any number of values, so the counts do not have to match.
#[test]
fn a_call_or_vararg_in_last_position_excuses_the_count() {
    assert!(!fires("unbalanced_assignments", "local a, b = f()\n"));
    assert!(!fires("unbalanced_assignments", "local a, b = ...\n"));
    assert!(!fires("unbalanced_assignments", "local a, b, c = 1, f()\n"));
}

// --- style -----------------------------------------------------------------

#[test]
fn empty_if_catches_an_empty_branch() {
    assert!(fires("empty_if", "if a then end\n"));
    assert!(fires("empty_if", "if a then x() else end\n"));
}

/// A branch that holds a comment is intentional. The comment is the content.
#[test]
fn a_branch_with_only_a_comment_is_left_alone() {
    assert!(!fires(
        "empty_if",
        "if a then\n\t-- nothing to do yet\nend\n"
    ));
}

#[test]
fn empty_loop_catches_every_loop_form() {
    assert!(fires("empty_loop", "while true do end\n"));
    assert!(fires("empty_loop", "for i = 1, 10 do end\n"));
    assert!(fires("empty_loop", "for k in pairs(t) do end\n"));
    assert!(fires("empty_loop", "repeat until done\n"));
}

#[test]
fn a_loop_with_a_body_is_fine() {
    assert!(!fires("empty_loop", "while true do work() end\n"));
}

#[test]
fn mixed_table_catches_both_halves_in_one_table() {
    assert!(fires("mixed_table", "local t = { 1, 2, a = 3 }\n"));
}

#[test]
fn a_table_that_is_only_one_shape_is_fine() {
    assert!(!fires("mixed_table", "local t = { 1, 2, 3 }\n"));
    assert!(!fires("mixed_table", "local t = { a = 1, b = 2 }\n"));
}

#[test]
fn parenthese_conditions_catches_the_habit() {
    assert!(fires("parenthese_conditions", "if (a) then end\n"));
    assert!(fires("parenthese_conditions", "while (a) do x() end\n"));
}

#[test]
fn parentheses_that_group_something_are_left_alone() {
    assert!(!fires(
        "parenthese_conditions",
        "if (a or b) and c then end\n"
    ));
}

#[test]
fn multiple_statements_is_off_until_a_project_asks() {
    let src = "local a = 1 local b = 2\n";

    assert!(!fires("multiple_statements", src));
    assert!(
        fired(src, &with("multiple_statements", Level::Warn))
            .iter()
            .any(|n| n == "multiple_statements")
    );
}

/// The lint must not report this pattern, even when the lint is on.
#[test]
fn a_one_line_guard_is_not_two_statements() {
    let cfg = with("multiple_statements", Level::Warn);

    assert!(
        !fired("if x then return end\n", &cfg)
            .iter()
            .any(|n| n == "multiple_statements"),
        "the return is in its own block"
    );
}

// --- names -----------------------------------------------------------------

#[test]
fn unused_variable_catches_a_local_nothing_reads() {
    assert!(fires("unused_variable", "local x = 1\n"));
    assert!(fires("unused_variable", "local function helper() end\n"));
}

#[test]
fn a_local_that_is_read_is_fine() {
    assert!(!fires("unused_variable", "local x = 1\nprint(x)\n"));
}

/// This is the convention for a name that the author intends not to use.
#[test]
fn an_underscore_name_is_exempt() {
    assert!(!fires("unused_variable", "local _ = f()\n"));
    assert!(!fires("unused_variable", "local _unused = 1\n"));
}

/// A signature is the caller's shape. A `for k, v` loop that uses only k is
/// normal.
#[test]
fn parameters_and_loop_variables_are_not_reported_by_default() {
    assert!(!fires(
        "unused_variable",
        "local function f(a, b)\n\treturn a\nend\nprint(f)\n"
    ));
    assert!(!fires(
        "unused_variable",
        "for k, v in pairs(t) do print(k) end\n"
    ));
}

#[test]
fn parameters_can_be_asked_for() {
    let mut cfg = LintConfig::default();
    cfg.options.insert(
        "unused_variable".into(),
        toml::from_str::<toml::Value>("parameters = true").unwrap(),
    );

    let src = "local function f(a, b)\n\treturn a\nend\nprint(f)\n";

    assert!(fired(src, &cfg).iter().any(|n| n == "unused_variable"));
}

/// The code computes a value and never reads it. That reads as a bug, not as
/// cleanup.
#[test]
fn a_variable_written_but_never_read_is_reported_differently() {
    let out = lint(
        Path::new("t.luau"),
        "local x = 1\nx = 2\n",
        &LintConfig::default(),
    )
    .unwrap();

    assert!(
        out.iter()
            .any(|d| d.message.contains("assigned but never read")),
        "{:?}",
        out.iter().map(|d| &d.message).collect::<Vec<_>>()
    );
}

#[test]
fn undefined_variable_catches_a_name_nothing_declares() {
    assert!(fires("undefined_variable", "print(notDeclaredAnywhere)\n"));
}

#[test]
fn a_standard_global_is_defined() {
    assert!(!fires("undefined_variable", "print(pairs)\n"));
    assert!(!fires(
        "undefined_variable",
        "local s = game:GetService('Players')\nprint(s)\n"
    ));
}

#[test]
fn a_project_can_add_its_own_globals() {
    let mut cfg = LintConfig::default();
    cfg.globals.push("MyFramework".to_string());

    assert!(
        !fired("print(MyFramework)\n", &cfg)
            .iter()
            .any(|n| n == "undefined_variable")
    );
}

/// A Roblox global must not be defined when the project is plain Luau.
#[test]
fn the_std_setting_decides_which_globals_exist() {
    let cfg = LintConfig {
        std: larvae::lint::config::StdLib::Luau,
        ..Default::default()
    };

    assert!(
        fired("print(game)\n", &cfg)
            .iter()
            .any(|n| n == "undefined_variable")
    );
}

/// The value is nil at run time, and that is not a matter of style.
#[test]
fn undefined_variable_is_an_error_rather_than_a_warning() {
    let out = lint(
        Path::new("t.luau"),
        "print(notDeclaredAnywhere)\n",
        &LintConfig::default(),
    )
    .unwrap();

    assert_eq!(out[0].severity, Severity::Error);
}

#[test]
fn unscoped_variables_catches_the_missing_local() {
    assert!(fires("unscoped_variables", "counter = 1\n"));
}

#[test]
fn a_local_assignment_is_fine() {
    assert!(!fires(
        "unscoped_variables",
        "local counter = 0\ncounter = 1\nprint(counter)\n"
    ));
}

#[test]
fn shadowing_catches_a_hidden_name() {
    assert!(fires(
        "shadowing",
        "local x = 1\nprint(x)\ndo\n\tlocal x = 2\n\tprint(x)\nend\n"
    ));
}

/// A binding that computes its value from the name it hides is the
/// intentional form.
#[test]
fn a_binding_that_reads_what_it_hides_is_not_reported() {
    assert!(!fires(
        "shadowing",
        "local x = 1\ndo\n\tlocal x = x + 1\n\tprint(x)\nend\n"
    ));
}

#[test]
fn a_sibling_scope_is_not_shadowing() {
    assert!(!fires(
        "shadowing",
        "do\n\tlocal x = 1\n\tprint(x)\nend\ndo\n\tlocal x = 2\n\tprint(x)\nend\n"
    ));
}

#[test]
fn global_usage_catches_reaching_into_the_shared_table() {
    assert!(fires("global_usage", "_G.thing = 1\n"));
    assert!(fires("global_usage", "print(_G.thing)\n"));
}

/// A user's own table named _G is not the shared table.
#[test]
fn a_local_named_underscore_g_is_not_the_shared_one() {
    assert!(!fires("global_usage", "local _G = {}\nprint(_G.thing)\n"));
}

// --- beyond selene ---------------------------------------------------------

#[test]
fn unreachable_code_catches_statements_after_an_exit() {
    assert!(fires(
        "unreachable_code",
        "while true do\n\tbreak\n\tprint('never')\nend\n"
    ));
    assert!(fires(
        "unreachable_code",
        "for i = 1, 3 do\n\tcontinue\n\tprint('never')\nend\n"
    ));
}

#[test]
fn code_before_the_exit_is_fine() {
    assert!(!fires(
        "unreachable_code",
        "while true do\n\tprint('once')\n\tbreak\nend\n"
    ));
}

/// An exit inside a branch leaves the enclosing block reachable.
#[test]
fn a_conditional_exit_does_not_kill_what_follows() {
    assert!(!fires(
        "unreachable_code",
        "while true do\n\tif done then\n\t\tbreak\n\tend\n\tprint('still runs')\nend\n"
    ));
}

#[test]
fn self_assignment_catches_a_line_that_does_nothing() {
    assert!(fires("self_assignment", "local x = 1\nx = x\nprint(x)\n"));
    assert!(fires("self_assignment", "t.k = t.k\n"));
}

#[test]
fn assigning_something_different_is_fine() {
    assert!(!fires("self_assignment", "local x = 1\nx = y\nprint(x)\n"));
    assert!(!fires("self_assignment", "t.a = t.b\n"));
}

/// A compound operator has an effect. A computed index can point to a
/// different element.
#[test]
fn compound_and_computed_forms_are_left_alone() {
    assert!(!fires("self_assignment", "local x = 1\nx += x\nprint(x)\n"));
    assert!(!fires("self_assignment", "t[i] = t[i]\n"));
}

#[test]
fn string_concat_in_loop_catches_the_quadratic_build() {
    assert!(fires(
        "string_concat_in_loop",
        "local s = ''\nfor i = 1, 10 do\n\ts = s .. i\nend\nprint(s)\n"
    ));
    assert!(fires(
        "string_concat_in_loop",
        "local s = ''\nwhile go do\n\ts = s .. 'x'\nend\nprint(s)\n"
    ));
}

#[test]
fn assigning_a_fresh_string_each_round_is_not_accumulating() {
    assert!(!fires(
        "string_concat_in_loop",
        "local s = ''\nfor i = 1, 10 do\n\ts = 'a' .. i\nend\nprint(s)\n"
    ));
}

#[test]
fn table_concat_after_the_loop_is_the_recommended_shape_and_is_quiet() {
    assert!(!fires(
        "string_concat_in_loop",
        "local parts = {}\nfor i = 1, 10 do\n\ttable.insert(parts, i)\nend\nprint(table.concat(parts))\n"
    ));
}

#[test]
fn loop_invariant_call_catches_a_service_lookup_per_iteration() {
    assert!(fires(
        "loop_invariant_call",
        "for i = 1, 10 do\n\tlocal p = game:GetService('Players')\n\tprint(p)\nend\n"
    ));
    assert!(fires(
        "loop_invariant_call",
        "for i = 1, 10 do\n\tlocal m = require('@pkg/thing')\n\tprint(m)\nend\n"
    ));
}

#[test]
fn a_lookup_hoisted_above_the_loop_is_quiet() {
    assert!(!fires(
        "loop_invariant_call",
        "local p = game:GetService('Players')\nfor i = 1, 10 do\n\tprint(p)\nend\n"
    ));
}

/// A call with an argument that varies is not invariant.
#[test]
fn a_call_with_a_computed_argument_is_not_reported() {
    assert!(!fires(
        "loop_invariant_call",
        "for i = 1, 10 do\n\tlocal m = require(names[i])\n\tprint(m)\nend\n"
    ));
}

/// A function body does not run one time per iteration.
#[test]
fn a_call_inside_a_callback_is_not_this_loops_work() {
    assert!(!fires(
        "loop_invariant_call",
        "for i = 1, 10 do\n\tdefer(function()\n\t\tprint(game:GetService('Players'))\n\tend)\nend\n"
    ));
}

// --- tables and configuration ----------------------------------------------

fn opts(name: &str, body: &str) -> LintConfig {
    let mut cfg = LintConfig::default();
    cfg.options.insert(
        name.to_string(),
        toml::from_str::<toml::Value>(body).unwrap(),
    );

    cfg
}

#[test]
fn bad_string_escape_catches_an_undefined_escape() {
    assert!(fires("bad_string_escape", r#"local s = "C:\path\to""#));
    assert!(fires("bad_string_escape", r#"local s = "\q""#));
}

#[test]
fn every_escape_the_language_defines_is_accepted() {
    let src = r#"local s = "\a\b\f\n\r\t\v\\\"\'\65\x41\u{1F600}\z  ""#;

    assert!(!fires("bad_string_escape", src), "{:?}", names(src));
}

#[test]
fn a_malformed_hex_or_unicode_escape_is_caught() {
    assert!(fires("bad_string_escape", r#"local s = "\xZZ""#));
    assert!(fires("bad_string_escape", r#"local s = "\u41""#));
}

/// A long string's content is literal, so there is nothing to escape.
#[test]
fn a_long_string_is_not_scanned_for_escapes() {
    assert!(!fires("bad_string_escape", "local s = [[C:\\path\\to]]"));
}

#[test]
fn must_use_catches_a_pure_call_thrown_away() {
    assert!(fires("must_use", "string.format('%d', 1)\n"));
    assert!(fires("must_use", "table.concat(t, ',')\n"));
    assert!(fires("must_use", "tostring(x)\n"));
}

#[test]
fn using_the_result_is_the_whole_point_and_is_quiet() {
    assert!(!fires(
        "must_use",
        "local s = string.format('%d', 1)\nprint(s)\n"
    ));
}

/// A call with side effects is not on the list, so the lint must not guess
/// about it.
#[test]
fn a_call_that_might_do_something_is_left_alone() {
    assert!(!fires("must_use", "table.insert(t, 1)\n"));
    assert!(!fires("must_use", "print('hi')\n"));
}

/// A local named string is the user's own table.
#[test]
fn a_shadowed_standard_table_is_not_assumed_to_be_the_real_one() {
    assert!(!fires(
        "must_use",
        "local string = myOwnThing\nstring.format('%d', 1)\n"
    ));
}

#[test]
fn deprecated_catches_the_replaced_roblox_globals() {
    assert!(fires("deprecated", "wait(1)\n"));
    assert!(fires("deprecated", "spawn(f)\n"));
    assert!(fires("deprecated", "local n = table.getn(t)\nprint(n)\n"));
}

#[test]
fn the_replacement_is_not_itself_deprecated() {
    assert!(!fires("deprecated", "task.wait(1)\n"));
}

#[test]
fn deprecated_catches_the_replaced_methods() {
    assert!(fires("deprecated", "part:Remove()\n"));
    assert!(fires("deprecated", "local c = part:children()\nprint(c)\n"));
}

#[test]
fn a_project_can_deprecate_its_own_functions() {
    let cfg = opts("deprecated", "additional = { oldHelper = \"newHelper\" }");

    assert!(
        fired("oldHelper()\n", &cfg)
            .iter()
            .any(|n| n == "deprecated")
    );
}

#[test]
fn restricted_module_paths_is_quiet_until_a_project_fills_it_in() {
    assert!(!fires(
        "restricted_module_paths",
        "local m = require('@server/secret')\nprint(m)\n"
    ));

    let cfg = opts(
        "restricted_module_paths",
        "paths = { \"@server/secret\" = \"shared code cannot reach the server\" }",
    );

    let out = fired("local m = require('@server/secret')\nprint(m)\n", &cfg);

    assert!(
        out.iter().any(|n| n == "restricted_module_paths"),
        "{out:?}"
    );
}

#[test]
fn high_cyclomatic_complexity_is_off_until_a_project_sets_a_limit() {
    let src = "local function f(a, b)\n\tif a and b then\n\t\treturn 1\n\telseif a or b then\n\t\treturn 2\n\tend\n\treturn 3\nend\nreturn f\n";

    assert!(!fires("high_cyclomatic_complexity", src));

    let mut cfg = opts("high_cyclomatic_complexity", "maximum_complexity = 2");
    cfg.rules
        .insert("high_cyclomatic_complexity".into(), Level::Warn);

    assert!(
        fired(src, &cfg)
            .iter()
            .any(|n| n == "high_cyclomatic_complexity")
    );
}

#[test]
fn a_simple_function_stays_under_any_sensible_limit() {
    let mut cfg = opts("high_cyclomatic_complexity", "maximum_complexity = 5");
    cfg.rules
        .insert("high_cyclomatic_complexity".into(), Level::Warn);

    let src = "local function f(a)\n\treturn a + 1\nend\nreturn f\n";

    assert!(
        !fired(src, &cfg)
            .iter()
            .any(|n| n == "high_cyclomatic_complexity")
    );
}

#[test]
fn manual_table_clone_catches_the_copy_loop() {
    assert!(fires(
        "manual_table_clone",
        "local new = {}\nfor k, v in pairs(old) do\n\tnew[k] = v\nend\nreturn new\n"
    ));
}

/// A loop that transforms while it copies is not a clone
#[test]
fn a_loop_that_does_more_than_copy_is_left_alone() {
    assert!(!fires(
        "manual_table_clone",
        "local new = {}\nfor k, v in pairs(old) do\n\tnew[k] = v * 2\nend\nreturn new\n"
    ));
    assert!(!fires(
        "manual_table_clone",
        "local new = {}\nfor k, v in pairs(old) do\n\tnew[k] = v\n\tcount += 1\nend\nreturn new\n"
    ));
}

#[test]
fn mismatched_arg_count_catches_too_many_arguments() {
    assert!(fires(
        "mismatched_arg_count",
        "local function f(a)\n\treturn a\nend\nf(1, 2, 3)\n"
    ));
}

/*
A call with fewer arguments than the function declares is legal and usual Lua.
Thus the lint reports it only when every parameter has a type and no parameter
is optional. That combination shows the author requires all parameters.
*/
#[test]
fn too_few_arguments_is_reported_only_when_every_parameter_is_required() {
    assert!(fires(
        "mismatched_arg_count",
        "local function f(a: number, b: string)\n\treturn a\nend\nf(1)\n"
    ));

    assert!(!fires(
        "mismatched_arg_count",
        "local function f(a, b)\n\treturn a\nend\nf(1)\n"
    ));
}

/// An optional parameter states that the caller can omit it.
#[test]
fn an_optional_parameter_may_be_omitted() {
    assert!(!fires(
        "mismatched_arg_count",
        "local function log(msg: string, level: string?)\n\treturn msg\nend\nlog(\"hi\")\n"
    ));
}

#[test]
fn a_call_with_the_right_count_is_fine() {
    assert!(!fires(
        "mismatched_arg_count",
        "local function f(a, b)\n\treturn a + b\nend\nf(1, 2)\n"
    ));
}

/// A vararg accepts any arguments after the named parameters.
#[test]
fn a_vararg_function_is_not_checked() {
    assert!(!fires(
        "mismatched_arg_count",
        "local function f(a, ...)\n\treturn a\nend\nf(1, 2, 3)\n"
    ));
}

/// A spread in the last position can supply any number of values.
#[test]
fn a_call_forwarding_a_call_is_not_counted() {
    assert!(!fires(
        "mismatched_arg_count",
        "local function f(a, b)\n\treturn a\nend\nf(g())\n"
    ));
}

/// A binding with a later reassignment can hold a different function at the
/// call site.
#[test]
fn a_reassigned_function_is_not_checked() {
    assert!(!fires(
        "mismatched_arg_count",
        "local function f(a, b)\n\treturn a\nend\nf = other\nf(1)\n"
    ));
}

// --- roblox ----------------------------------------------------------------

#[test]
fn color3_new_over_one_is_caught() {
    assert!(fires(
        "roblox_incorrect_color3_new_bounds",
        "local c = Color3.new(255, 0, 0)\nprint(c)\n"
    ));
}

#[test]
fn color3_new_on_the_right_scale_is_fine() {
    assert!(!fires(
        "roblox_incorrect_color3_new_bounds",
        "local c = Color3.new(1, 0, 0)\nprint(c)\n"
    ));
    assert!(!fires(
        "roblox_incorrect_color3_new_bounds",
        "local c = Color3.fromRGB(255, 0, 0)\nprint(c)\n"
    ));
}

#[test]
fn udim2_new_with_two_arguments_is_caught() {
    assert!(fires(
        "roblox_suspicious_udim2_new",
        "local u = UDim2.new(0.5, 0.5)\nprint(u)\n"
    ));
}

#[test]
fn udim2_new_with_four_arguments_is_the_real_signature() {
    assert!(!fires(
        "roblox_suspicious_udim2_new",
        "local u = UDim2.new(0.5, 10, 0.5, 10)\nprint(u)\n"
    ));
}

#[test]
fn a_udim2_that_is_really_fromscale_or_fromoffset_is_named() {
    assert!(fires(
        "roblox_manual_fromscale_or_fromoffset",
        "local u = UDim2.new(0.5, 0, 0.5, 0)\nprint(u)\n"
    ));
    assert!(fires(
        "roblox_manual_fromscale_or_fromoffset",
        "local u = UDim2.new(0, 10, 0, 20)\nprint(u)\n"
    ));
}

#[test]
fn a_udim2_using_both_halves_is_left_alone() {
    assert!(!fires(
        "roblox_manual_fromscale_or_fromoffset",
        "local u = UDim2.new(0.5, 10, 0.5, 20)\nprint(u)\n"
    ));
}

/// An all-zero value equals UDim2.new(), so the lint gives no advice.
#[test]
fn an_all_zero_udim2_is_not_reported() {
    assert!(!fires(
        "roblox_manual_fromscale_or_fromoffset",
        "local u = UDim2.new(0, 0, 0, 0)\nprint(u)\n"
    ));
}

/// These names have no meaning outside Roblox.
#[test]
fn the_roblox_lints_are_silent_under_plain_luau() {
    let mut cfg = LintConfig {
        std: larvae::lint::config::StdLib::Luau,
        ..Default::default()
    };

    cfg.globals.push("Color3".to_string());

    assert!(
        !fired("local c = Color3.new(255, 0, 0)\nprint(c)\n", &cfg)
            .iter()
            .any(|n| n.starts_with("roblox_"))
    );
}

// --- regressions found by review -------------------------------------------

/// A global that the file itself defines is defined. Roblox scripts use this
/// pattern.
#[test]
fn a_global_declared_in_this_file_is_not_undefined() {
    assert!(!fires(
        "undefined_variable",
        "function onTouch(hit)\n\tprint(hit)\nend\n\nscript.Parent.Touched:Connect(onTouch)\n"
    ));

    assert!(!fires(
        "undefined_variable",
        "counter = 0\nprint(counter)\n"
    ));
}

/// A read before the line that sets the global is still not this lint's
/// concern.
#[test]
fn a_forward_reference_to_a_file_global_is_allowed() {
    assert!(!fires(
        "undefined_variable",
        "local function a()\n\treturn helper()\nend\nfunction helper() end\nreturn a\n"
    ));
}

#[test]
fn a_local_used_only_from_a_type_is_not_unused() {
    assert!(!fires(
        "unused_variable",
        "local Types = require(\"./types\")\nexport type Foo = Types.Foo\n"
    ));
    assert!(!fires(
        "unused_variable",
        "local defaults = { a = 1 }\ntype Config = typeof(defaults)\n"
    ));
    assert!(!fires(
        "unused_variable",
        "local T = require(\"./t\")\nlocal function f(x: T.Thing)\n\treturn x\nend\nreturn f\n"
    ));
}

/// The implicit self has no name token, so the user cannot remove it.
#[test]
fn the_implicit_self_of_a_method_is_never_reported_unused() {
    let cfg = opts("unused_variable", "parameters = true");
    let out = fired("function M:ping()\n\tprint(\"pong\")\nend\n", &cfg);

    assert!(!out.iter().any(|n| n == "unused_variable"), "{out:?}");
}

/// A field write goes to a different place on each iteration, so the cost is
/// linear.
#[test]
fn a_per_iteration_field_write_is_not_an_accumulator() {
    assert!(!fires(
        "string_concat_in_loop",
        "local t = {}\nfor _, child in ipairs(t) do\n\tchild.Name = child.Name .. \"_old\"\nend\n"
    ));
}

/// A string declared inside the body starts empty on each iteration.
#[test]
fn a_string_built_within_one_iteration_is_not_an_accumulator() {
    assert!(!fires(
        "string_concat_in_loop",
        "for i = 1, 10 do\n\tlocal line = \"\"\n\tline = line .. i\n\tprint(line)\nend\n"
    ));
}

/// An append behind a condition is the most frequent shape of this problem.
#[test]
fn an_accumulate_inside_a_branch_is_still_found() {
    assert!(fires(
        "string_concat_in_loop",
        "local s = \"\"\nfor i = 1, 10 do\n\tif i % 2 == 0 then\n\t\ts = s .. i\n\tend\nend\nprint(s)\n"
    ));
}

/// table.clone would discard the data that the destination already held.
#[test]
fn a_merge_into_an_existing_table_is_not_a_clone() {
    assert!(!fires(
        "manual_table_clone",
        "local function merge(target, source)\n\tfor k, v in pairs(source) do\n\t\ttarget[k] = v\n\tend\n\treturn target\nend\nreturn merge\n"
    ));
}

#[test]
fn a_copy_into_a_table_declared_empty_just_above_is_a_clone() {
    assert!(fires(
        "manual_table_clone",
        "local new = {}\nfor k, v in pairs(old) do\n\tnew[k] = v\nend\nreturn new\n"
    ));
}

/// When the lint cannot evaluate the step, it must suppress the report, not
/// permit it.
#[test]
fn a_countdown_with_a_step_it_cannot_read_is_not_reported() {
    assert!(!fires(
        "suspicious_reverse_loop",
        "local step = -1\nfor i = 10, 1, step do\n\tprint(i)\nend\n"
    ));
}

/// `Queue:remove(1)` is the user's own collection, not a deprecated Instance.
#[test]
fn a_lowercase_remove_on_an_unknown_receiver_is_not_deprecated() {
    assert!(!fires("deprecated", "local q = Queue.new()\nq:remove(1)\n"));
}

#[test]
fn the_legacy_roblox_casing_is_still_reported() {
    assert!(fires("deprecated", "part:Remove()\n"));
    assert!(fires("deprecated", "local c = part:children()\nprint(c)\n"));
}

/// These names have no meaning outside Roblox.
#[test]
fn deprecated_methods_are_silent_under_plain_luau() {
    let cfg = LintConfig {
        std: larvae::lint::config::StdLib::Luau,
        ..Default::default()
    };

    assert!(
        !fired("part:Remove()\n", &cfg)
            .iter()
            .any(|n| n == "deprecated")
    );
}

// --- non_const_require -----------------------------------------------------

/// `const` is newer than most codebases, so a project must ask for this lint.
#[test]
fn non_const_require_is_off_until_a_project_asks() {
    let src = "local Signal = require(\"@pkg/signal\")\nreturn Signal\n";

    assert!(!fires("non_const_require", src));
    assert!(
        fired(src, &with("non_const_require", Level::Warn))
            .iter()
            .any(|n| n == "non_const_require")
    );
}

fn const_lint(src: &str) -> bool {
    fired(src, &with("non_const_require", Level::Warn))
        .iter()
        .any(|n| n == "non_const_require")
}

#[test]
fn a_local_bound_to_a_require_is_reported() {
    assert!(const_lint(
        "local Signal = require(\"@pkg/signal\")\nreturn Signal\n"
    ));
}

#[test]
fn one_already_const_is_not() {
    assert!(!const_lint(
        "const Signal = require(\"@pkg/signal\")\nreturn Signal\n"
    ));
}

/*
Advice to make a reassigned name const produces `Variable 'X' is constant
and may not be reassigned`, which is worse than silence.
*/
#[test]
fn a_require_whose_name_is_reassigned_is_left_alone() {
    assert!(!const_lint(
        "local M = require(\"@pkg/m\")\nM = fallback\nreturn M\n"
    ));
}

/// The lint matches the const_requires transform: one name, one value, no annotation.
#[test]
fn the_shapes_const_cannot_express_are_skipped() {
    assert!(!const_lint(
        "local A, B = require(\"@pkg/a\"), require(\"@pkg/b\")\nreturn A, B\n"
    ));
    assert!(!const_lint(
        "local S: Signal = require(\"@pkg/signal\")\nreturn S\n"
    ));
}

#[test]
fn a_local_that_is_not_a_require_is_not_its_business() {
    assert!(!const_lint("local x = compute()\nreturn x\n"));
}

/// A local named require is not the global require.
#[test]
fn a_shadowed_require_says_nothing_about_modules() {
    assert!(!const_lint(
        "local require = myLoader\nlocal S = require(\"x\")\nreturn S\n"
    ));
}
