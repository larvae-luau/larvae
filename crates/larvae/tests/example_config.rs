/*!
`larvae.example.toml` against the code it documents.

The file claims to list every key larvae reads, with its default. Both halves
drift silently: a renamed key leaves the example wrong, and a changed default
leaves it lying. It was duplicated end to end at one point, so it did not even
parse, and nothing noticed.

Every config type has `deny_unknown_fields`, so loading the file through the
real parsers is a complete check that its keys exist. The defaults are then
compared one by one.
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

/// Written into a directory of its own, since discovery reads from the root
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

/// It was duplicated verbatim once, which TOML rejects for the second table
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

/// `deny_unknown_fields` means loading it proves every key it names is real
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

// --- the values it calls defaults really are ---------------------------------

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

/// Each level the example prints must be the lint's own default
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
`[rojo]` is commented out on purpose and has to stay that way.

Writing `project` changes what it means: unset it is "use default.project.json
if it is there", set it is "this file is required", so a project without one
would stop building. Uncommenting it in the example would hand every reader
that regression.
*/
#[test]
fn the_rojo_defaults_are_not_written_out() {
    let (_dir, config) = loaded();

    assert_eq!(config.rojo.project, None);
    assert_eq!(config.rojo.build_project, None);
}
