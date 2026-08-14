/*!
The JSON schema of `larvae.toml`, with the worms of a project merged in.

The shipped schema is one file for every project, so it cannot know the worms
that one project loads. It therefore says nothing about the rules, the lints,
and the settings that a worm declares, and an editor offers no completion for
them.

This module merges the declarations of the loaded worms into a copy of that
schema and writes the copy into the cache directory. The declarations are the
same ones larvae validates against, so the editor and the loader cannot
disagree.
*/

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::{Map, Value, json};

/// The schema that larvae ships, before a project adds its worms
pub const BASE: &str = include_str!("../larvae.schema.json");

/// The filename of the generated copy inside the cache directory
pub const FILE: &str = "larvae.schema.json";

/// Merge the worms of a project into the shipped schema
pub fn for_project(registry: &crate::worm::registry::Registry) -> Result<Value> {
    let mut schema: Value = serde_json::from_str(BASE).context("the shipped schema is not JSON")?;

    /*
    The level list is written into each entry rather than referenced.

    An editor that follows draft 07 replaces a schema that holds `$ref` with
    the schema it points at, and the description beside the `$ref` is lost.
    The reader then sees what a level means where it asked what the lint
    means. The builtin lints are written the same way, for the same reason.
    */
    let levels = schema["$defs"]["lint_level"]["enum"].clone();

    for loaded in registry.iter() {
        let worm = loaded.worm.name();
        let mut props = Map::new();

        for (name, decl) in &loaded.worm.manifest.lints {
            let mut entry = json!({ "enum": levels });

            if let Some(text) = &decl.description {
                entry["description"] = json!(text);
            }

            props.insert(name.clone(), entry);
        }

        if props.is_empty() {
            continue;
        }

        /*
        The lints of a worm sit in a table under its key, so the editor
        completes them where the project writes them.
        */
        schema["$defs"]["lint_rules"]["properties"][worm] = json!({
            "type": "object",
            "additionalProperties": false,
            "description": format!("The lints of worm `{worm}`."),
            "properties": Value::Object(props),
        });
    }

    for loaded in registry.iter() {
        let worm = loaded.worm.name();
        let mut props = Map::new();

        for (name, option) in &loaded.worm.manifest.fmt {
            props.insert(name.clone(), option_schema(option));
        }

        if props.is_empty() {
            continue;
        }

        /*
        The format options of a worm sit in a table under its key, the same
        way its lints do, so the editor completes them where a project writes
        them.
        */
        schema["$defs"]["fmt"]["properties"][worm] = json!({
            "type": "object",
            "additionalProperties": false,
            "description": format!("The format options of worm `{worm}`."),
            "properties": Value::Object(props),
        });
    }

    let entry = schema["$defs"]["worms"]["additionalProperties"].clone();

    for loaded in registry.iter() {
        let worm = &loaded.worm.manifest;
        let mut per_worm = entry.clone();

        /*
        The entry of a worm is a string or a table. Only the table holds the
        keys a worm declares, so the string branch is copied unchanged.
        */
        let Some(table) = per_worm
            .get_mut("oneOf")
            .and_then(|o| o.get_mut(1))
            .filter(|t| t.get("properties").is_some())
        else {
            continue;
        };

        table["properties"]["rules"] = rules_of(worm);
        table["properties"]["config"] = config_of(worm);

        schema["$defs"]["worms"]["properties"][name_of(worm)] = per_worm;
    }

    Ok(schema)
}

/// Write the merged schema into the cache directory of a project
pub fn write(cache: &Path, registry: &crate::worm::registry::Registry) -> Result<PathBuf> {
    let path = cache.join(FILE);

    std::fs::create_dir_all(cache)
        .with_context(|| format!("cannot create {}", crate::ui::rel(cache)))?;

    let text = serde_json::to_string_pretty(&for_project(registry)?)?;

    std::fs::write(&path, text)
        .with_context(|| format!("cannot write {}", crate::ui::rel(&path)))?;

    Ok(path)
}

fn name_of(worm: &crate::worm::Manifest) -> &str {
    &worm.name
}

/// The `rules` table of one worm, so the editor names each rule it declares
fn rules_of(worm: &crate::worm::Manifest) -> Value {
    let mut props = Map::new();

    for (name, decl) in &worm.rules {
        let mut entry = json!({});

        if let Some(text) = &decl.description {
            entry["description"] = json!(text);
        }

        props.insert(name.clone(), entry);
    }

    json!({
        "type": "object",
        "additionalProperties": false,
        "description": "The rules of this worm, under the names it declares.",
        "properties": Value::Object(props),
    })
}

/// The `config` table of one worm, from the options it declares
fn config_of(worm: &crate::worm::Manifest) -> Value {
    /*
    A worm that declares no option keeps the open table it always had. The
    schema must stay open there, because larvae accepts every key.
    */
    if worm.options.is_empty() {
        return json!({
            "type": "object",
            "description": "The settings of this worm, handed to it untouched at init.",
        });
    }

    let mut props = Map::new();

    for (name, option) in &worm.options {
        props.insert(name.clone(), option_schema(option));
    }

    json!({
        "type": "object",
        "additionalProperties": false,
        "description": "The settings of this worm, under the options it declares.",
        "properties": Value::Object(props),
    })
}

/// One declared option as a schema entry, with its type, its text, its
/// default, and the values it accepts
fn option_schema(option: &crate::worm::manifest::OptionDecl) -> Value {
    let mut entry = json!({ "type": option.kind.name() });

    if let Some(text) = &option.description {
        entry["description"] = json!(text);
    }

    if let Some(default) = &option.default {
        entry["default"] = value_of(default);
    }

    if !option.values.is_empty() {
        entry["enum"] = Value::Array(option.values.iter().map(value_of).collect());
    }

    entry
}

/// One TOML scalar as JSON. larvae builds TOML without a serializer, so the
/// scalars are written by hand here as well.
fn value_of(value: &toml::Value) -> Value {
    match value {
        toml::Value::String(s) => json!(s),

        toml::Value::Integer(n) => json!(n),

        toml::Value::Float(f) => json!(f),

        toml::Value::Boolean(b) => json!(b),

        other => json!(other.type_str()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shipped_schema_parses() {
        let schema: Value = serde_json::from_str(BASE).unwrap();

        assert!(schema["$defs"]["lint_rules"]["properties"].is_object());
        assert!(schema["$defs"]["worms"]["additionalProperties"].is_object());
    }
}
