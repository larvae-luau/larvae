//! Pipeline tests for the darklua parity rules, before and after on a real project

use std::fs;
use std::path::Path;

use larvae::config::Config;
use larvae::pipeline;

fn write(root: &Path, rel: &str, content: &str) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, content).unwrap();
}

fn read(root: &Path, rel: &str) -> String {
    fs::read_to_string(root.join(rel)).unwrap()
}

/// A minimal project with only the rules under test switched on
fn project(rules: &str) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "larvae.toml", &format!("[rules]\n{rules}\n"));

    tmp
}

fn build(root: &Path) {
    let config = Config::load_or_default(root).unwrap();
    let outcome = pipeline::run(root, &config, true).unwrap();

    for d in &outcome.diags {
        eprintln!("{d}");
    }

    assert!(!outcome.has_errors(), "unexpected errors");
}

/// Output lines must line up with input lines, that is the retain-lines promise
fn same_line_count(before: &str, after: &str) {
    assert_eq!(
        before.lines().count(),
        after.lines().count(),
        "line count changed\nbefore:\n{before}\nafter:\n{after}"
    );
}

/*
Run one source file through the pipeline with `rules` enabled and hand back
what came out the other side
*/
fn transform(rules: &str, before: &str) -> String {
    let tmp = project(rules);
    let root = tmp.path();

    write(root, "src/init.luau", before);
    build(root);
    let after = read(root, "dist/init.luau");
    same_line_count(before, &after);

    after
}

/// The rule changes nothing on this input
fn unchanged(rules: &str, src: &str) {
    assert_eq!(transform(rules, src), src);
}

// --- tier one, plain tree edits -----------------------------------------

#[test]
fn remove_method_definition() {
    assert_eq!(
        transform(
            "remove_method_definition = true",
            "local C = {}\nfunction C:greet(name)\n    return name\nend\nreturn C\n"
        ),
        "local C = {}\nfunction C.greet(self, name)\n    return name\nend\nreturn C\n"
    );
    unchanged(
        "remove_method_definition = true",
        "local C = {}\nfunction C.greet(name)\n    return name\nend\nreturn C\n",
    );
}

#[test]
fn remove_compound_assignment() {
    assert_eq!(
        transform(
            "remove_compound_assignment = true",
            "local x = 1\nx += 2\nx ..= \"a\"\nreturn x\n"
        ),
        "local x = 1\nx = x + 2\nx = x .. \"a\"\nreturn x\n"
    );
    // a call in the key would run twice, so it stays
    unchanged(
        "remove_compound_assignment = true",
        "local t = {}\nt[f()] += 1\nreturn t\n",
    );
}

#[test]
fn remove_floor_division() {
    assert_eq!(
        transform(
            "remove_floor_division = true",
            "local a, b = 7, 2\nlocal c = a // b\nreturn c\n"
        ),
        "local a, b = 7, 2\nlocal c = math.floor(a / b)\nreturn c\n"
    );
    unchanged(
        "remove_floor_division = true",
        "local a, b = 7, 2\nlocal c = a / b\nreturn c\n",
    );
}

#[test]
fn remove_if_expression() {
    assert_eq!(
        transform(
            "remove_if_expression = true",
            "local c = true\nlocal x = if c then 1 else 2\nreturn x\n"
        ),
        "local c = true\nlocal x = c and 1 or 2\nreturn x\n"
    );
    // a then value that could be nil would break the and/or form
    unchanged(
        "remove_if_expression = true",
        "local c, a, b = true, 1, 2\nlocal x = if c then a else b\nreturn x\n",
    );
}

#[test]
fn remove_method_call() {
    assert_eq!(
        transform(
            "remove_method_call = true",
            "local obj = {}\nobj:go(1)\nreturn obj\n"
        ),
        "local obj = {}\nobj.go(obj, 1)\nreturn obj\n"
    );
    // a dotted receiver would be evaluated twice
    unchanged(
        "remove_method_call = true",
        "local a = {}\na.b:go(1)\nreturn a\n",
    );
}

#[test]
fn convert_index_to_field() {
    assert_eq!(
        transform(
            "convert_index_to_field = true",
            "local t = { [\"a\"] = 1 }\nreturn t[\"a\"]\n"
        ),
        "local t = { a = 1 }\nreturn t.a\n"
    );
    // a reserved word cannot follow a dot
    unchanged(
        "convert_index_to_field = true",
        "local t = {}\nreturn t[\"end\"]\n",
    );
}

#[test]
fn convert_function_to_assignment() {
    assert_eq!(
        transform(
            "convert_function_to_assignment = true",
            "local M = {}\nfunction M.run(x)\n    return x\nend\nreturn M\n"
        ),
        "local M = {}\nM.run = function(x)\n    return x\nend\nreturn M\n"
    );
    unchanged(
        "convert_function_to_assignment = true",
        "local function f(x)\n    return x\nend\nreturn f\n",
    );
}

#[test]
fn convert_luau_number() {
    assert_eq!(
        transform(
            "convert_luau_number = true",
            "local mask = 0b1010\nlocal big = 1_000_000\nreturn mask + big\n"
        ),
        "local mask = 0xA\nlocal big = 1000000\nreturn mask + big\n"
    );
    unchanged(
        "convert_luau_number = true",
        "local a = 42\nlocal b = 0xFF\nreturn a + b\n",
    );
}

#[test]
fn make_assignment_local() {
    assert_eq!(
        transform("make_assignment_local = true", "const X = 1\nreturn X\n"),
        "local X = 1\nreturn X\n"
    );
    unchanged("make_assignment_local = true", "local X = 1\nreturn X\n");
}

#[test]
fn remove_types() {
    assert_eq!(
        transform(
            "remove_types = true",
            "type Point = { x: number }\nlocal p: Point = nil\nlocal function id(v: string): string\n    return v\nend\nreturn id\n"
        ),
        "\nlocal p = nil\nlocal function id(v)\n    return v\nend\nreturn id\n"
    );
    unchanged(
        "remove_types = true",
        "local p = 1\nlocal function id(v)\n    return v\nend\nreturn id\n",
    );
}

#[test]
fn remove_attribute() {
    assert_eq!(
        transform(
            "remove_attribute = true",
            "@native function f()\n    return 1\nend\nreturn f\n"
        ),
        "function f()\n    return 1\nend\nreturn f\n"
    );
    // match picks which ones go
    assert_eq!(
        transform(
            "remove_attribute = { match = [\"^native$\"] }",
            "@native @checked function f()\n    return 1\nend\nreturn f\n"
        ),
        "@checked function f()\n    return 1\nend\nreturn f\n"
    );
    unchanged(
        "remove_attribute = true",
        "function f()\n    return 1\nend\nreturn f\n",
    );
}

#[test]
fn remove_function_call_parens() {
    assert_eq!(
        transform(
            "remove_function_call_parens = true",
            "local f = print\nf(\"hi\")\nreturn f\n"
        ),
        "local f = print\nf\"hi\"\nreturn f\n"
    );
    unchanged(
        "remove_function_call_parens = true",
        "local f = print\nf(1)\nreturn f\n",
    );
}

#[test]
fn filter_after_early_return() {
    let after = transform(
        "filter_after_early_return = true",
        "local x = 1\ndo return x end\nx = 2\nreturn x\n",
    );
    assert!(
        after.starts_with("local x = 1\ndo return x end\n"),
        "{after}"
    );
    assert!(!after.contains("x = 2"), "{after}");
    unchanged(
        "filter_after_early_return = true",
        "local x = 1\ndo x = 3 end\nreturn x\n",
    );
}

#[test]
fn remove_interpolated_string() {
    assert_eq!(
        transform(
            "remove_interpolated_string = true",
            "local n = \"world\"\nreturn `hello {n}`\n"
        ),
        "local n = \"world\"\nreturn string.format(\"hello %s\", tostring(n))\n"
    );
    // the tostring strategy leans on Luau's %* instead
    assert_eq!(
        transform(
            "remove_interpolated_string = { strategy = \"tostring\" }",
            "local n = \"world\"\nreturn `hello {n}`\n"
        ),
        "local n = \"world\"\nreturn string.format(\"hello %*\", n)\n"
    );
    unchanged(
        "remove_interpolated_string = true",
        "local n = \"world\"\nreturn n\n",
    );
}

#[test]
fn remove_continue() {
    let after = transform(
        "remove_continue = true",
        "for i = 1, 3 do\n    if i == 2 then continue end\n    print(i)\nend\nreturn 1\n",
    );
    assert!(after.contains("do repeat"), "{after}");
    assert!(after.contains("until true end"), "{after}");
    assert!(!after.contains("continue"), "{after}");
    // a loop that also breaks would have the inner repeat swallow the break
    unchanged(
        "remove_continue = true",
        "for i = 1, 3 do\n    if i == 2 then continue end\n    if i == 3 then break end\nend\nreturn 1\n",
    );
}

// --- tier two, evaluator and side effect checks -------------------------

#[test]
fn compute_expression() {
    assert_eq!(
        transform(
            "compute_expression = true",
            "local day = 60 * 60 * 24\nreturn day\n"
        ),
        "local day = 86400\nreturn day\n"
    );
    // a fraction has no printed form the rule will commit to
    unchanged("compute_expression = true", "local x = 10 / 4\nreturn x\n");
}

#[test]
fn remove_unused_if_branch() {
    let after = transform(
        "remove_unused_if_branch = true",
        "if false then\n    print(1)\nelse\n    print(2)\nend\nreturn 1\n",
    );
    assert!(!after.contains("print(1)"), "{after}");
    assert!(after.contains("print(2)"), "{after}");
    unchanged(
        "remove_unused_if_branch = true",
        "local c = 1\nif c then\n    print(1)\nend\nreturn 1\n",
    );
}

#[test]
fn remove_unused_while() {
    let after = transform(
        "remove_unused_while = true",
        "while false do\n    print(1)\nend\nreturn 1\n",
    );
    assert!(!after.contains("print(1)"), "{after}");
    // zero is truthy in Lua, that loop runs
    unchanged(
        "remove_unused_while = true",
        "while 0 do\n    print(1)\nend\nreturn 1\n",
    );
}

#[test]
fn remove_nil_declaration() {
    assert_eq!(
        transform(
            "remove_nil_declaration = true",
            "local a = nil\nlocal b, c = 1, nil\nreturn a, b, c\n"
        ),
        "local a\nlocal b, c = 1\nreturn a, b, c\n"
    );
    // a leading nil is positional and has to stay
    unchanged(
        "remove_nil_declaration = true",
        "local a, b = nil, 1\nreturn a, b\n",
    );
}

#[test]
fn group_local_assignment() {
    let after = transform(
        "group_local_assignment = true",
        "local a = 1\nlocal b = 2\nreturn a + b\n",
    );
    assert!(after.starts_with("local a, b = 1, 2\n"), "{after}");
    // b reads a, merging would make it read the outer binding
    unchanged(
        "group_local_assignment = true",
        "local a = 1\nlocal b = a\nreturn b\n",
    );
}

#[test]
fn convert_local_function_to_assign() {
    assert_eq!(
        transform(
            "convert_local_function_to_assign = true",
            "local function f(a)\n    return a\nend\nreturn f\n"
        ),
        "local f = function(a)\n    return a\nend\nreturn f\n"
    );
    // recursive, the local form is in scope inside itself and the other is not
    unchanged(
        "convert_local_function_to_assign = true",
        "local function f(n)\n    return f(n - 1)\nend\nreturn f\n",
    );
}

#[test]
fn convert_square_root_call() {
    assert_eq!(
        transform(
            "convert_square_root_call = true",
            "local n = 9\nreturn math.sqrt(n)\n"
        ),
        "local n = 9\nreturn (n ^ 0.5)\n"
    );
    unchanged(
        "convert_square_root_call = true",
        "local n = 9\nreturn math.floor(n)\n",
    );
}

#[test]
fn remove_assertions() {
    let after = transform(
        "remove_assertions = true",
        "local x = 1\nassert(x == 1)\nreturn x\n",
    );
    assert!(!after.contains("assert"), "{after}");
    // an argument that calls something may be the point of the line
    unchanged(
        "remove_assertions = true",
        "local x = 1\nassert(check(x))\nreturn x\n",
    );
    // unless the option says otherwise
    let after = transform(
        "remove_assertions = { preserve_arguments_side_effects = false }",
        "local x = 1\nassert(check(x))\nreturn x\n",
    );
    assert!(!after.contains("assert"), "{after}");
}

#[test]
fn remove_debug_profiling() {
    let after = transform(
        "remove_debug_profiling = true",
        "debug.profilebegin(\"work\")\nlocal x = 1\ndebug.profileend()\nreturn x\n",
    );
    assert!(!after.contains("profile"), "{after}");
    assert!(after.contains("local x = 1"), "{after}");
    unchanged(
        "remove_debug_profiling = true",
        "debug.traceback()\nreturn 1\n",
    );
}

// --- interactions --------------------------------------------------------

#[test]
fn several_rules_compose_in_one_pass() {
    let before = concat!(
        "--!strict\n",
        "local C = {}\n",
        "function C:add(a: number, b: number): number\n",
        "    local total = 0\n",
        "    total += a\n",
        "    total += b\n",
        "    return total\n",
        "end\n",
        "return C\n"
    );
    let after = transform(
        "remove_types = true\nremove_method_definition = true\nremove_compound_assignment = true",
        before,
    );
    assert!(after.contains("function C.add(self, a, b)"), "{after}");
    assert!(after.contains("total = total + a"), "{after}");
    assert!(!after.contains(": number"), "{after}");
}

#[test]
fn method_definitions_never_gain_two_self_parameters() {
    // both rules want to insert self, the broader one has to win
    let after = transform(
        "remove_method_definition = true\nconvert_function_to_assignment = true",
        "local C = {}\nfunction C:go()\n    return 1\nend\nreturn C\n",
    );
    assert!(after.contains("C.go = function(self)"), "{after}");
    assert_eq!(after.matches("self").count(), 1, "{after}");
}

#[test]
fn remove_unused_variable() {
    let after = transform(
        "remove_unused_variable = true",
        "local unused = 1\nlocal kept = 2\nreturn kept\n",
    );

    assert!(!after.contains("unused"), "{after}");
    assert!(after.contains("local kept = 2"), "{after}");
}

#[test]
fn rename_variables() {
    let after = transform(
        "rename_variables = true",
        "local counter = 1\nreturn counter\n",
    );

    assert!(!after.contains("counter"), "{after}");
    assert!(after.contains("local a = 1"), "{after}");
}
