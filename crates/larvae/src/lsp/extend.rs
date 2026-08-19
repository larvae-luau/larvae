/*!
The two paths a worm will reach the editor through.

Neither one carries anything yet. This module is the seam, so that the work
which fills it changes one file and not the dispatch loop, the capability
list, and the tests all at once.

**Code actions.** `textDocument/codeAction` is where a worm will offer a fix
for a finding it reported, or a rewrite it knows how to make. Larvae already
routes a claimed file to its worm for formatting and linting, so the worm that
found the problem is the one that can repair it. The host owns the protocol
and the worm owns the edit, which is the split every other capability uses.

**Definitions.** A worm that teaches larvae a new kind of module has to tell
the type system what that module is. A worm that makes a data file requirable
is the case that asks for this: `require("./items.json")` has a type, the worm
is what knows it, and nothing in larvae or in luau-lsp can work it out alone.
The path is Luau definition text, because that is what luau-lsp already reads.
`larvae worm types` writes the definitions for the worm API itself, which is a
different file for a different reader; these are definitions for the code of
the project.

Both functions take the pool so the signature does not change when the calls
to the worms arrive.
*/

use serde_json::{Value, json};

use super::rpc::Lines;
use crate::worm::pool::Pool;

/// One worm's contribution to the types of a project.
#[derive(Debug, Clone, PartialEq)]
pub struct Definitions {
    /// The worm that supplied it, for a message that names a source.
    pub worm: String,
    /// Luau definition text, as luau-lsp reads it.
    pub text: String,
}

/*
The actions the worms offer over this range.

Each worm is asked for the actions it has, and the name of the worm goes into
the title, so a user reading the lightbulb sees which extension offered the
fix rather than a bare sentence from nowhere.

A worm speaks in byte offsets, because it parsed the file and knows where
things are in it. The host turns those into the positions the protocol wants,
the same conversion a finding already goes through.
*/
pub fn code_actions(worms: &Pool, uri: &str, text: &str, range: &Value) -> Vec<Value> {
    if worms.is_empty() {
        return Vec::new();
    }

    let lines = Lines::new(text);
    let span = byte_range(&lines, text, range);

    worms
        .code_actions(text, span)
        .into_iter()
        .map(|(worm, action)| {
            let edits: Vec<Value> = action
                .edits
                .iter()
                .map(|edit| {
                    json!({
                        "range": lines.range(text, edit.span),
                        "newText": edit.text,
                    })
                })
                .collect();

            let mut out = json!({
                "title": format!("{} ({worm})", action.title),
                "kind": "quickfix",
                "edit": { "changes": { uri: edits } },
            });

            /*
            A fix that names its lint is grouped under that diagnostic, so it
            appears on the problem and not in a general list.
            */
            if let Some(lint) = &action.fixes {
                out["diagnostics"] = json!([{
                    "source": "larvae",
                    "code": format!("{worm}.{lint}"),
                }]);
            }

            out
        })
        .collect()
}

/*
The byte range the editor asked about, or the whole file when it named none.

The same reading as `lsp::actions`, kept here rather than shared, because the
two modules answer different owners and a change to one is not a change to the
other.
*/
fn byte_range(lines: &Lines, text: &str, range: &Value) -> (u32, u32) {
    let at = |which: &str| -> Option<u32> {
        let point = range.get(which)?;
        let line = point.get("line")?.as_u64()? as u32;
        let character = point.get("character")?.as_u64()? as u32;

        Some(lines.offset(text, line, character))
    };

    match (at("start"), at("end")) {
        (Some(start), Some(end)) => (start, end),

        _ => (0, text.len() as u32),
    }
}

/*
The type definitions the worms of this project supply.

Each worm is asked once. A worm that returns nothing is left out, so the list
holds only the worms that have something to say about the types of the
project.
*/
pub fn definitions(worms: &Pool) -> Vec<Definitions> {
    worms
        .definitions()
        .into_iter()
        .map(|(worm, text)| Definitions { worm, text })
        .collect()
}

/// The definitions as the custom request reports them.
pub fn definitions_reply(worms: &Pool) -> Value {
    let supplied: Vec<Value> = definitions(worms)
        .into_iter()
        .map(|d| json!({ "worm": d.worm, "text": d.text }))
        .collect();

    json!({ "definitions": supplied })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lsp::state::no_worms;

    /*
    The seam answers, and it answers with the right shape.

    A request that errors and a request that returns nothing look the same to
    a user and are not the same to an editor. These say that the path is
    wired before anything travels down it.
    */
    #[test]
    fn the_code_action_path_answers_with_a_list() {
        assert_eq!(
            code_actions(&no_worms(), "file:///t.luau", "local x = 1\n", &json!(null)),
            Vec::<Value>::new()
        );
    }

    #[test]
    fn the_definitions_path_answers_with_a_list() {
        let reply = definitions_reply(&no_worms());

        assert!(
            reply["definitions"].is_array(),
            "an editor reads this as a list: {reply}"
        );
        assert_eq!(reply["definitions"].as_array().unwrap().len(), 0);
    }
}
