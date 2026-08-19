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
The actions a worm offers over this range.

Empty until the worms answer. The shape is the LSP one, a list of `CodeAction`
objects, and the caller sends it as the result of the request.

The filled version asks each worm that claims this file for the actions it has
over the range, and stamps the name of the worm into the title, so a user sees
which extension offered the fix.
*/
pub fn code_actions(_worms: &Pool, _uri: &str, _text: &str, _range: &Value) -> Vec<Value> {
    Vec::new()
}

/*
The type definitions the worms of this project supply.

Empty until the worms answer. The filled version asks each worm for the
definition text it wants in scope, and the caller hands the list to the editor
or writes it beside the project for luau-lsp to read.
*/
pub fn definitions(_worms: &Pool) -> Vec<Definitions> {
    Vec::new()
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
