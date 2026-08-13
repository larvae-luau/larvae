/*!
These tests compare `larvae.example.toml` with the code it documents.

The file claims to list every key that larvae reads, with its default value.
Each half can drift without a signal. A renamed key makes the example wrong.
A changed default makes the example incorrect. At one point, the file contained
a full duplicate of itself, so it did not parse, and no test found the problem.

Every config type has `deny_unknown_fields`. Thus, when the real parsers load
the file, the load is a complete check that its keys exist. The tests then
compare the defaults one by one.
*/

use std::path::Path;

use larvae::config::Config;
use larvae::fmt::FmtConfig;
use larvae::lint::LintConfig;

fn example() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("the workspace root")
        .join("larvae.example.toml");

    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {}, {e}", path.display()))
}

/// The test writes the file into its own directory, because discovery reads
/// from the root.
fn loaded() -> (tempfile::TempDir, Config) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("larvae.toml");
    std::fs::write(&path, example()).unwrap();

    let config = Config::load(&path).expect("the example config should load");

    (dir, config)
}

#[test]
fn the_example_is_valid_toml() {
    toml::from_str::<toml::Value>(&example()).expect("valid TOML");
}

/// The file once contained a verbatim duplicate. TOML rejects the second table.
#[test]
fn the_example_is_not_duplicated() {
    let text = example();

    assert_eq!(
        text.matches("\n[process]").count(),
        1,
        "[process] appears more than once"
    );
    assert_eq!(
        text.matches("\n[fmt]").count(),
        1,
        "[fmt] appears more than once"
    );
    assert_eq!(
        text.matches("\n[lint]").count(),
        1,
        "[lint] appears more than once"
    );
}

/// Because of `deny_unknown_fields`, a correct load proves that every named
/// key exists.
#[test]
fn every_key_in_the_example_is_one_larvae_reads() {
    let (_dir, _config) = loaded();
}

#[test]
fn the_fmt_and_lint_tables_parse_through_their_own_types() {
    let (dir, config) = loaded();

    FmtConfig::discover(dir.path(), config.fmt.as_ref()).expect("[fmt] should parse");
    LintConfig::discover(dir.path(), config.lint.as_ref()).expect("[lint] should parse");
}

#[test]
fn every_profile_in_the_example_applies() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("larvae.toml");
    std::fs::write(&path, example()).unwrap();

    Config::load_profile(&path, Some("release")).expect("--profile release should apply");
}

// --- the values that the example calls defaults match the code ---------------

#[test]
fn the_documented_process_defaults_match() {
    let (_dir, from_example) = loaded();
    let default: Config = toml::from_str("").expect("an empty config is all defaults");

    let (a, b) = (&from_example.process, &default.process);

    assert_eq!(a.generator, b.generator);
    assert_eq!(a.quotes, b.quotes);
    assert_eq!(a.run_order, b.run_order);
    assert_eq!(a.strip_flags, b.strip_flags);
    assert_eq!(a.cache, b.cache);
    assert_eq!(a.cache_dir, b.cache_dir);
    assert_eq!(a.include, b.include);
    assert_eq!(a.exclude, b.exclude);
    assert_eq!(a.inputs(), b.inputs());
    assert_eq!(a.output, b.output);
}

#[test]
fn the_documented_requires_defaults_match() {
    let (_dir, from_example) = loaded();
    let default: Config = toml::from_str("").expect("an empty config is all defaults");

    let (a, b) = (&from_example.requires, &default.requires);

    assert_eq!(a.target, b.target);
    assert_eq!(a.strict, b.strict);
    assert_eq!(a.instance_input, b.instance_input);
}

#[test]
fn the_documented_fmt_defaults_match() {
    let (dir, config) = loaded();
    let from_example = FmtConfig::discover(dir.path(), config.fmt.as_ref()).unwrap();
    let default = FmtConfig::default();

    assert_eq!(from_example.column_width, default.column_width);
    assert_eq!(from_example.line_endings, default.line_endings);
    assert_eq!(from_example.indent_type, default.indent_type);
    assert_eq!(from_example.indent_width, default.indent_width);
    assert_eq!(from_example.quote_style, default.quote_style);
    assert_eq!(from_example.call_parentheses, default.call_parentheses);
    assert_eq!(
        from_example.space_after_function_names,
        default.space_after_function_names
    );
    assert_eq!(
        from_example.collapse_simple_statement,
        default.collapse_simple_statement
    );
    assert_eq!(from_example.block_newline_gaps, default.block_newline_gaps);
    assert_eq!(
        from_example.magic_trailing_comma,
        default.magic_trailing_comma
    );
    assert_eq!(
        from_example.space_inside_braces,
        default.space_inside_braces
    );
    assert_eq!(
        from_example.space_inside_parens,
        default.space_inside_parens
    );
    assert_eq!(
        from_example.space_inside_brackets,
        default.space_inside_brackets
    );
    assert_eq!(from_example.trailing_comma, default.trailing_comma);
    assert_eq!(
        from_example.sort_requires.enabled,
        default.sort_requires.enabled
    );
}

/// Each level that the example prints must equal the lint's own default.
#[test]
fn the_documented_lint_levels_match_each_lint() {
    let (dir, config) = loaded();
    let from_example = LintConfig::discover(dir.path(), config.lint.as_ref()).unwrap();

    for (name, level) in &from_example.rules {
        let lint = larvae::lint::find(name)
            .unwrap_or_else(|| panic!("the example names {name}, which is not a lint"));

        assert_eq!(
            *level,
            lint.default_level(),
            "the example says {name} defaults to {level:?}, the lint says {:?}",
            lint.default_level()
        );
    }
}

/*
The example keeps `[rojo]` commented out, and it must stay commented out.

A written `project` key changes the meaning. When the key is unset, larvae uses
default.project.json if the file exists. When the key is set, the file is
required, so a project without the file would stop building. An uncommented key
in the example would give every reader that regression.
*/
#[test]
fn the_rojo_defaults_are_not_written_out() {
    let (_dir, config) = loaded();

    assert_eq!(config.rojo.project, None);
    assert_eq!(config.rojo.build_project, None);
}
