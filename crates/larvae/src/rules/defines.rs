/*!
Compile time constants

The `[defines]` table in the config maps global names to values. Larvae
replaces each global reference to a defined name with its literal. This is
the only mechanism for build time values. darklua's `inject_global_value`
maps onto it. When the user also enables compute_expression and
remove_unused_if_branch, larvae removes `if DEBUG then` blocks completely.

Larvae does not change a name that the source binds itself. For this
reason, the rule uses the scope pass and not a text match.
*/

use std::collections::HashMap;

use crate::rules::edits::Edit;
use crate::rules::engine::RuleCtx;

/// A value for a define. Larvae limits the set to values that a Luau literal can express.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Bool(bool),
    Number(String),
    Str(String),
    Nil,
}

impl Value {
    /// The literal that larvae writes into the output.
    pub fn literal(&self, quote: char) -> String {
        match self {
            Value::Bool(b) => b.to_string(),

            Value::Number(n) => n.clone(),

            Value::Str(s) => crate::requires::resolve::lua_quote(s, quote),

            Value::Nil => "nil".to_string(),
        }
    }
}

/// Read the `[defines]` table. Reject each value that has no literal form.
pub fn parse(table: &toml::Value) -> Result<HashMap<String, Value>, String> {
    let Some(map) = table.as_table() else {
        return Err("[defines] must be a table of names and values".to_string());
    };

    let mut out = HashMap::new();

    for (name, value) in map {
        if !crate::rules::native::is_ident(name) {
            return Err(format!("[defines] \"{name}\" is not a valid Luau name"));
        }

        let parsed = match value {
            toml::Value::Boolean(b) => Value::Bool(*b),

            toml::Value::Integer(i) => Value::Number(i.to_string()),

            toml::Value::Float(f) => Value::Number(f.to_string()),

            toml::Value::String(s) => Value::Str(s.clone()),

            other => {
                return Err(format!(
                    "[defines] {name} is a {}, only booleans, numbers and strings work",
                    other.type_str()
                ));
            }
        };

        out.insert(name.clone(), parsed);
    }

    Ok(out)
}

/// Replace each global reference to a defined name with its value.
pub fn apply(ctx: &RuleCtx, edits: &mut Vec<Edit>) {
    if ctx.defines.is_empty() {
        return;
    }

    for tok in ctx.globals {
        if let Some(value) = ctx.defines.get(ctx.tok_text(*tok)) {
            let t = &ctx.toks[*tok as usize];
            edits.push((t.start, t.end, value.literal(ctx.quote)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::edits::{Edits, splice};
    use crate::syntax::{lexer, parser};

    fn run(defines: &str, src: &str) -> String {
        let table: toml::Value = toml::from_str(defines).expect("defines");
        let map = parse(&table).expect("valid");
        let lexed = lexer::lex(src).unwrap();
        let chunk = parser::parse(src, &lexed.toks).unwrap();

        let bare = RuleCtx {
            src,
            toks: &lexed.toks,
            chunk: &chunk,
            comments: &lexed.comments,
            require_forms: &[],
            dm_path: None,
            quote: '"',
            defines: &map,
            globals: &Default::default(),
        };
        let globals = crate::rules::scope::globals(&bare);
        let ctx = RuleCtx {
            globals: &globals,
            ..bare
        };

        let mut edits = Edits::new();
        edits.run("defines", |e| apply(&ctx, e));

        splice(src, &edits, &mut Vec::new())
    }

    #[test]
    fn substitutes_every_kind() {
        assert_eq!(
            run(
                "DEBUG = false\nMAX = 100\nNAME = \"game\"\nRATE = 1.5",
                "return DEBUG, MAX, NAME, RATE"
            ),
            "return false, 100, \"game\", 1.5"
        );
    }

    #[test]
    fn leaves_anything_the_source_bound() {
        assert_eq!(
            run("DEBUG = false", "local DEBUG = 1\nreturn DEBUG"),
            "local DEBUG = 1\nreturn DEBUG"
        );
        assert_eq!(
            run("DEBUG = false", "local function f(DEBUG) return DEBUG end"),
            "local function f(DEBUG) return DEBUG end"
        );
        // A field with the same name is not the global.
        assert_eq!(run("DEBUG = false", "return t.DEBUG"), "return t.DEBUG");
    }

    #[test]
    fn shadowing_only_covers_its_block() {
        assert_eq!(
            run("DEBUG = false", "do local DEBUG = 1 end\nreturn DEBUG"),
            "do local DEBUG = 1 end\nreturn false"
        );
    }

    #[test]
    fn rejects_values_with_no_literal_form() {
        let table: toml::Value = toml::from_str("LIST = [1, 2]").unwrap();
        assert!(parse(&table).unwrap_err().contains("only booleans"));

        let table: toml::Value = toml::from_str("\"not a name\" = 1").unwrap();
        assert!(parse(&table).unwrap_err().contains("valid Luau name"));
    }
}
