/*!
The front-end pre-pass.

A front-end worm turns something we cannot parse into Luau, so it has to finish
before anything that parses Luau starts, including our own require scanning.
That is not an ordering convention we remember to honour, it is what this stage
being separate enforces: a claimed file's bytes are replaced here, and every
later stage receives Luau without ever learning a worm was involved.

It runs serially on purpose. A front-end touches the few files that use its
syntax, not the whole tree, and keeping worms off the parallel loop means no
worm instance is ever shared between threads.
*/

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::diag::Diag;
use crate::worm::registry::Registry;

/// A claimed file, after its worm has had it
pub struct Compiled {
    /// The Luau the rest of the pipeline sees instead of the file on disk
    pub source: String,
    /// `App.luaux` becomes `App.luau`, before anything derives a name from it
    pub dest: PathBuf,
}

/// Everything the front-ends produced, by input path
pub type Outputs = HashMap<PathBuf, Compiled>;

/*
Hand every claimed file to the worm that claimed it. A worm that reports a
problem takes its file out of the build with a diagnostic, rather than letting
markup reach a lexer that cannot read it and reporting a lex error nobody can
act on.
*/
pub fn run(
    registry: &mut Registry,
    files: &[PathBuf],
    dest_of: &impl Fn(&Path) -> Option<PathBuf>,
    diags: &mut Vec<Diag>,
) -> Outputs {
    let mut out = HashMap::new();

    if registry.is_empty() {
        return out;
    }

    for path in files {
        let Some(rel) = dest_of(path) else {
            continue;
        };

        let Some(loaded) = registry.frontend_for(path) else {
            continue;
        };

        let name = loaded.worm.name().to_owned();

        let src = match std::fs::read_to_string(path) {
            Ok(s) => s,

            Err(e) => {
                diags.push(Diag::error(
                    path,
                    format!("cannot read file (UTF-8 required): {e}"),
                ));

                continue;
            }
        };

        let config = toml_text(&loaded.config);

        match loaded.worm.transform(&src, &config) {
            Ok(outcome) if outcome.ok => {
                /*
                Line preservation composes only if every stage keeps it. A worm
                that changes the count would silently move every diagnostic
                below it, so say so once here rather than let it be discovered
                in a stack trace.
                */
                let (before, after) = (src.lines().count(), outcome.text.lines().count());

                if before != after {
                    diags.push(Diag::warning(
                        path,
                        format!(
                            "worm `{name}` changed the line count, {before} in and {after} out, so line numbers below this file will not match"
                        ),
                    ));
                }

                out.insert(
                    path.clone(),
                    Compiled {
                        source: outcome.text,
                        dest: luau_dest(&rel),
                    },
                );
            }

            Ok(outcome) => diags.push(Diag::error(
                path,
                format!("worm `{name}`: {}", outcome.text),
            )),

            Err(e) => diags.push(Diag::error(path, format!("{e:#}"))),
        }
    }

    out
}

/// `App.luaux` becomes `App.luau`, so the DataModel instance is named `App`
pub fn luau_dest(rel: &Path) -> PathBuf {
    rel.with_extension("luau")
}

/// Re-serialize a worm's settings, since we hand them over as text
fn toml_text(value: &toml::Value) -> String {
    let Some(table) = value.as_table() else {
        return String::new();
    };

    let mut out = String::new();

    for (key, value) in table {
        out.push_str(key);
        out.push_str(" = ");
        out.push_str(&scalar(value));
        out.push('\n');
    }

    out
}

/*
We build toml without its serializer, so scalars are written by hand. A worm
wanting richer settings can keep its own config file, which is the arrangement
luaux already asked for.
*/
fn scalar(value: &toml::Value) -> String {
    match value {
        toml::Value::String(s) => format!("{s:?}"),

        toml::Value::Integer(n) => n.to_string(),

        toml::Value::Float(f) => f.to_string(),

        toml::Value::Boolean(b) => b.to_string(),

        other => format!("{:?}", other.type_str()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_claimed_file_is_renamed_before_anything_reads_it() {
        assert_eq!(luau_dest(Path::new("a/App.luaux")), Path::new("a/App.luau"));
        assert_eq!(luau_dest(Path::new("App.rune")), Path::new("App.luau"));
    }

    #[test]
    fn settings_are_handed_over_as_toml_text() {
        let value = toml::from_str::<toml::Value>("factory = \"vide\"\nstrict = true\n").unwrap();
        let text = toml_text(&value);

        assert!(text.contains("factory = \"vide\""), "{text}");
        assert!(text.contains("strict = true"), "{text}");
    }

    #[test]
    fn no_settings_is_an_empty_string_rather_than_a_shape() {
        assert_eq!(toml_text(&toml::Value::Boolean(true)), "");
    }
}
