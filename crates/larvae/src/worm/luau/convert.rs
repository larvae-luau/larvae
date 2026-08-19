/*!
Values crossing between Rust and Luau, and the message that comes back.

A worm receives its settings as real Luau tables and not as text, which the
wasm form cannot do and this form should not give up. These functions are that
conversion, plus the one that turns an mlua error into a sentence a user can
act on.
*/

use anyhow::{Context, Result};
use mlua::LuaSerdeExt;

/// Convert TOML to a Luau value, so a Luau worm reads a table and not a string
pub(super) fn to_lua(lua: &mlua::Lua, value: &toml::Value) -> mlua::Result<mlua::Value> {
    Ok(match value {
        toml::Value::String(s) => mlua::Value::String(lua.create_string(s)?),

        toml::Value::Integer(n) => mlua::Value::Integer(*n),

        toml::Value::Float(f) => mlua::Value::Number(*f),

        toml::Value::Boolean(b) => mlua::Value::Boolean(*b),

        toml::Value::Datetime(d) => mlua::Value::String(lua.create_string(d.to_string())?),

        toml::Value::Array(items) => {
            let out = lua.create_table()?;

            for (i, item) in items.iter().enumerate() {
                out.set(i + 1, to_lua(lua, item)?)?;
            }

            mlua::Value::Table(out)
        }

        toml::Value::Table(map) => {
            let out = lua.create_table()?;

            for (k, v) in map {
                out.set(k.as_str(), to_lua(lua, v)?)?;
            }

            mlua::Value::Table(out)
        }
    })
}

pub(super) fn to_lua_opt(
    lua: &mlua::Lua,
    value: Option<&toml::Value>,
) -> mlua::Result<mlua::Value> {
    match value {
        Some(v) => to_lua(lua, v),

        None => Ok(mlua::Value::Nil),
    }
}

/*
Convert one settings field to a Luau value.

The registry stores each field as JSON text, because the native transport
speaks JSON. A Luau worm gets a real table instead, for the same reason it
gets its config as a table. An empty field gives an empty table, so a worm
indexes the settings without a nil check.

The serializer turns a JSON null into `nil` and not into a userdata marker,
so an absent option compares equal to nil in the worm.
*/
pub(super) fn json_to_lua(lua: &mlua::Lua, json: &str) -> Result<mlua::Value> {
    let value: serde_json::Value = match json {
        "" => serde_json::Value::Object(Default::default()),

        text => serde_json::from_str(text).context("the settings are not valid JSON")?,
    };

    let options = mlua::serde::SerializeOptions::new()
        .serialize_none_to_null(false)
        .serialize_unit_to_null(false);

    lua.to_value_with(&value, options)
        .context("the settings do not convert to Luau")
}

/*
A Luau error arrives with the chunk name and line at the front and a traceback
at the back. These parts do not help a user who wants the reason from the
worm. Thus this function extracts the message that the worm wrote.
*/
pub(super) fn worm_message(e: &mlua::Error) -> String {
    let full = match e {
        mlua::Error::CallbackError { cause, .. } => cause.to_string(),

        other => other.to_string(),
    };

    let head = full.split("\nstack traceback:").next().unwrap_or(&full);

    head.rsplit_once(':')
        .map(|(_, msg)| msg.trim())
        .filter(|msg| !msg.is_empty())
        .unwrap_or(head.trim())
        .to_owned()
}
