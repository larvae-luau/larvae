/*!
The path a worm reaches the editor through: code actions.

`textDocument/codeAction` is where a worm offers a fix for a finding it
reported, or a rewrite it knows how to make. Larvae already routes a claimed
file to its worm for formatting and linting, so the worm that found the
problem is the one that can repair it. The host owns the protocol and the
worm owns the edit, which is the split every other capability uses.
*/

use serde_json::{Value, json};

use super::rpc::Lines;
use crate::worm::pool::Pool;

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
                /*
                A diagnostic requires its range and its message, or the
                editor's client throws converting it and the whole code
                action reply is lost. The range of the first edit is where
                the fix lands, which is where the finding sits.
                */
                let at = action
                    .edits
                    .first()
                    .map(|edit| lines.range(text, edit.span))
                    .unwrap_or_else(|| lines.range(text, (span.0, span.0)));

                out["diagnostics"] = json!([{
                    "range": at,
                    "source": "larvae",
                    "code": format!("{worm}.{lint}"),
                    "message": format!("a finding of {worm}.{lint}"),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lsp::state::no_worms;

    /*
    The path answers with a list when no worm offers anything.

    A request that errors and a request that returns nothing look the same to
    a user and are not the same to an editor, which logs a failure on every
    keystroke that opens the lightbulb.
    */
    #[test]
    fn the_code_action_path_answers_with_a_list() {
        assert_eq!(
            code_actions(&no_worms(), "file:///t.luau", "local x = 1\n", &json!(null)),
            Vec::<Value>::new()
        );
    }
}
