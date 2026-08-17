/*!
These tests compare larvae.schema.json with the code it documents.

An editor shows the schema while the user types. Thus a key that exists in one
and not the other is worse than no schema at all. Completion stops offering a
real option, or offers an option that errors on the next run. The two tables
that grow are the lints and the formatter's options, so the tests check those
name by name. The tests check the rest for dangling refs.
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

/// The schema lists every lint that a project can name, and no other lint.
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
    A name outside the builtins is not a level. A worm names its lints under
    its own key, so an unknown key must be a table of levels and a flat
    `luaux_x = "warn"` line is an error the editor shows.
    */
    let extra = &schema["$defs"]["lint_rules"]["additionalProperties"];

    assert_eq!(extra["type"], "object", "an unknown key is a worm table");
    assert_eq!(
        extra["additionalProperties"],
        serde_json::json!({ "$ref": "#/$defs/lint_level" }),
        "the values of a worm table are levels"
    );

    // the same for [fmt]: an unknown key is the table of one worm
    assert_eq!(
        schema["$defs"]["fmt"]["additionalProperties"]["type"],
        "object"
    );
}

/// The schema lists every level a lint accepts, with the spelling that the
/// config parses.
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

/// The schema lists every formatter option, and no other key.
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

    // This key comes from stylua, and larvae ignores it, so it never appears
    // in a serialized config.
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

/// The schema lists every [bundle] key, and no other key.
#[test]
fn bundle_keys_match_the_config() {
    let schema = schema();
    let documented = keys(&schema["$defs"]["bundle"]["properties"]);

    /*
    Every field set, because toml omits a `None` on serialization. The
    default config would serialize `tree_shake` alone, and the test would
    compare three documented keys against one real key.
    */
    let full = larvae::config::bundle::BundleConfig {
        entry: Some("src/main.luau".into()),
        output: Some("bundle.luau".into()),
        tree_shake: true,
    };

    let real: BTreeSet<String> = toml::Value::try_from(full)
        .expect("the config serializes")
        .as_table()
        .expect("a table")
        .keys()
        .cloned()
        .collect();

    assert_eq!(
        documented.difference(&real).collect::<Vec<_>>(),
        Vec::<&String>::new(),
        "the schema offers [bundle] keys that do not exist"
    );

    assert_eq!(
        real.difference(&documented).collect::<Vec<_>>(),
        Vec::<&String>::new(),
        "these [bundle] keys are missing from the schema"
    );

    // The config denies unknown fields, so the schema must too.
    assert_eq!(
        schema["$defs"]["bundle"]["additionalProperties"],
        serde_json::json!(false)
    );
}

/// A ref to a def that does not exist documents nothing, and gives no error.
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

/// The same guarantee for [check]: the schema and the config cannot drift.
#[test]
fn check_keys_match_the_config() {
    let schema = schema();
    let documented = keys(&schema["$defs"]["check"]["properties"]);

    // Every field of the default serializes: the levels always, entries as [].
    let real: BTreeSet<String> =
        toml::Value::try_from(larvae::config::check::CheckConfig::default())
            .expect("the config serializes")
            .as_table()
            .expect("a table")
            .keys()
            .cloned()
            .collect();

    assert_eq!(
        documented.difference(&real).collect::<Vec<_>>(),
        Vec::<&String>::new(),
        "the schema offers [check] keys that do not exist"
    );

    assert_eq!(
        real.difference(&documented).collect::<Vec<_>>(),
        Vec::<&String>::new(),
        "the config has [check] keys the schema does not offer"
    );
}

/// The same guarantee for [minify]: the schema and the config cannot drift.
#[test]
fn minify_keys_match_the_config() {
    let schema = schema();
    let documented = keys(&schema["$defs"]["minify"]["properties"]);

    let real: BTreeSet<String> =
        toml::Value::try_from(larvae::config::minify::MinifyConfig::default())
            .expect("the config serializes")
            .as_table()
            .expect("a table")
            .keys()
            .cloned()
            .collect();

    assert_eq!(
        documented.difference(&real).collect::<Vec<_>>(),
        Vec::<&String>::new(),
        "the schema offers [minify] keys that do not exist"
    );

    assert_eq!(
        real.difference(&documented).collect::<Vec<_>>(),
        Vec::<&String>::new(),
        "the config has [minify] keys the schema does not offer"
    );
}

/*
Every formatter option carries a link to its own anchor on the docs site.

Even Better TOML renders a description as Markdown, so the link is what a
reader clicks in the hover card. The anchor comes from the option name with
each `_` written as `-`, so a renamed option that keeps its old anchor points
at a heading that no longer exists. This test compares the two.
*/
#[test]
fn every_fmt_option_links_to_its_own_anchor() {
    const BASE: &str = "https://larvae-luau.github.io/docs/reference/formatting";

    let schema = schema();
    let fmt = &schema["$defs"]["fmt"];

    assert!(
        fmt["description"].as_str().unwrap().contains(BASE),
        "the [fmt] table needs a link to the reference page"
    );

    for (key, prop) in fmt["properties"].as_object().unwrap() {
        let want = format!("{BASE}#{}", key.replace('_', "-"));
        let description = prop["description"].as_str().unwrap_or_default();

        assert!(
            description.contains(&want),
            "[fmt] {key} needs a link to {want}"
        );

        let Some(nested) = prop.get("properties").and_then(|p| p.as_object()) else {
            continue;
        };

        for (sub, subprop) in nested {
            let want = format!("{BASE}#{}-{}", key.replace('_', "-"), sub.replace('_', "-"));

            assert!(
                subprop["description"]
                    .as_str()
                    .unwrap_or_default()
                    .contains(&want),
                "[fmt.{key}] {sub} needs a link to {want}"
            );
        }
    }
}

/*
Every boolean states its two values, though the type already implies them.

Taplo builds the completion list from `enum` when an entry has one, and from
the type when it does not. In the second case it adds the `default` to that
list as well, so a boolean that defaults to false offered false two times.
Users read that as a defect in larvae, because larvae wrote the schema.
*/
#[test]
fn every_boolean_states_its_two_values() {
    fn walk(node: &serde_json::Value, path: &str, bare: &mut Vec<String>) {
        match node {
            serde_json::Value::Object(map) => {
                if map.get("type").and_then(|t| t.as_str()) == Some("boolean")
                    && !map.contains_key("enum")
                {
                    bare.push(path.to_string());
                }

                for (key, value) in map {
                    walk(value, &format!("{path}/{key}"), bare);
                }
            }

            serde_json::Value::Array(items) => {
                for (i, item) in items.iter().enumerate() {
                    walk(item, &format!("{path}[{i}]"), bare);
                }
            }

            _ => {}
        }
    }

    let mut bare = Vec::new();
    walk(&schema(), "", &mut bare);

    assert!(
        bare.is_empty(),
        "these boolean entries need \"enum\": [true, false]: {bare:#?}"
    );
}
