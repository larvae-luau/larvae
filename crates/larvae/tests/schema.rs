/*!
larvae.schema.json against the code it documents.

The schema is what an editor shows while somebody types, so a key that exists
in one and not the other is worse than no schema at all: completion stops
offering a real option, or offers one that errors on the next run. The two
tables that grow are the lints and the formatter's options, so those are
checked name by name and the rest is checked for dangling refs.
*/

use std::collections::BTreeSet;

use larvae::fmt::FmtConfig;
use larvae::lint::registry;

fn schema() -> serde_json::Value {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/larvae.schema.json");
    let text = std::fs::read_to_string(path).expect("the schema ships with the crate");

    serde_json::from_str(&text).expect("the schema is valid JSON")
}

fn keys(value: &serde_json::Value) -> BTreeSet<String> {
    value
        .as_object()
        .expect("an object")
        .keys()
        .cloned()
        .collect()
}

/// Every lint a project can name, and no lint it cannot
#[test]
fn lint_rules_match_the_registry() {
    let schema = schema();
    let documented = keys(&schema["$defs"]["lint_rules"]["properties"]);
    let real: BTreeSet<String> = registry().iter().map(|l| l.name().to_string()).collect();

    assert_eq!(
        documented.difference(&real).collect::<Vec<_>>(),
        Vec::<&String>::new(),
        "the schema offers lints that do not exist"
    );

    assert_eq!(
        real.difference(&documented).collect::<Vec<_>>(),
        Vec::<&String>::new(),
        "these lints are missing from [lint.rules] in the schema"
    );

    /*
    Names beyond the builtins are worm lints, which the schema cannot know,
    so extras must stay legal and must still be levels. `false` here would
    make every editor flag a worm lint the moment a project levels one.
    */
    assert_eq!(
        schema["$defs"]["lint_rules"]["additionalProperties"],
        serde_json::json!({ "$ref": "#/$defs/lint_level" }),
        "[lint.rules] must accept worm lint names as levels"
    );
}

/// Every level a lint can be set to, spelled the way the config parses it
#[test]
fn lint_levels_are_the_ones_the_config_takes() {
    let schema = schema();
    let levels = schema["$defs"]["lint_level"]["enum"]
        .as_array()
        .expect("an enum");

    let empty = tempfile::tempdir().expect("a temp dir");

    for level in levels {
        let text = format!("[rules]\nshadowing = {level}");
        let value: toml::Value = toml::from_str(&text).expect("valid toml");

        larvae::lint::LintConfig::discover(empty.path(), Some(&value))
            .unwrap_or_else(|e| panic!("the schema offers {level}, which the config refuses: {e}"));
    }
}

/// Every formatter option, and no option that is not one
#[test]
fn fmt_options_match_the_config() {
    let schema = schema();
    let documented = keys(&schema["$defs"]["fmt"]["properties"]);

    let default = toml::Value::try_from(FmtConfig::default()).expect("the config serializes");
    let mut real: BTreeSet<String> = default
        .as_table()
        .expect("a table")
        .keys()
        .cloned()
        .collect();

    // taken from stylua and ignored, so it never appears in a serialized config
    real.insert("syntax".to_string());

    assert_eq!(
        documented.difference(&real).collect::<Vec<_>>(),
        Vec::<&String>::new(),
        "the schema offers [fmt] keys that do not exist"
    );

    assert_eq!(
        real.difference(&documented).collect::<Vec<_>>(),
        Vec::<&String>::new(),
        "these [fmt] keys are missing from the schema"
    );
}

/// A ref into a def that is not there silently documents nothing
#[test]
fn every_ref_resolves() {
    let schema = schema();
    let defs = keys(&schema["$defs"]);
    let mut seen = Vec::new();

    collect_refs(&schema, &mut seen);

    assert!(!seen.is_empty(), "the schema uses refs");

    for reference in seen {
        let name = reference
            .strip_prefix("#/$defs/")
            .unwrap_or_else(|| panic!("{reference} is not a local def ref"));

        assert!(defs.contains(name), "{reference} points at nothing");
    }
}

fn collect_refs(value: &serde_json::Value, out: &mut Vec<String>) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, value) in map {
                match (key.as_str(), value.as_str()) {
                    ("$ref", Some(target)) => out.push(target.to_string()),

                    _ => collect_refs(value, out),
                }
            }
        }

        serde_json::Value::Array(items) => items.iter().for_each(|i| collect_refs(i, out)),

        _ => {}
    }
}
