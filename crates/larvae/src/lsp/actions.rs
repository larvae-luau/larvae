/*!
The code actions larvae offers for its own findings.

A worm's actions arrive through [`super::extend`]. These are larvae's, and
they exist because the fix for an unused name is one keystroke that the editor
can make for the author.

`unused_variable` and `unused_function` both say "prefix the name with _ to
keep it", and that sentence is the action. The underscore is the convention
Luau and selene both read, and larvae reads it too, so the name stops being
reported without the code changing.

The action renames the declaration and every write of it. A binding that is
assigned and never read reports as well, and prefixing the declaration alone
there would leave the assignments pointing at a name nothing declares, which
is a global and a different bug than the one being silenced.
*/

use serde_json::{Value, json};

use crate::lint::{LintConfig, scope};
use crate::syntax::{lexer, parser};

use super::rpc::Lines;

/// The lints this module can fix.
const FIXABLE: [&str; 2] = ["unused_variable", "unused_function"];

/*
The actions larvae offers over this range.

`range` is what the editor asked about. An action is offered when the finding
it fixes overlaps that range, so a caret on the line gets the lightbulb and a
caret elsewhere does not.
*/
pub fn for_range(uri: &str, text: &str, range: &Value, cfg: &LintConfig) -> Vec<Value> {
    let Ok(findings) = crate::lint::analyze(text, cfg) else {
        // A file in the middle of an edit does not parse, and it has no actions.
        return Vec::new();
    };

    let Ok(lexed) = lexer::lex(text) else {
        return Vec::new();
    };

    let Ok(chunk) = parser::parse(text, &lexed.toks) else {
        return Vec::new();
    };

    let names = scope::resolve(text, &lexed.toks, &chunk);
    let lines = Lines::new(text);

    let wanted = byte_range(&lines, text, range);

    let mut out = Vec::new();

    for finding in findings {
        if !FIXABLE.contains(&finding.lint.as_ref()) {
            continue;
        }

        if !overlaps(finding.span, wanted) {
            continue;
        }

        let name = &text[finding.span.0 as usize..finding.span.1 as usize];

        // The convention is a prefix, so a name that has one is already silent.
        if name.starts_with('_') {
            continue;
        }

        let mut spans = vec![finding.span];

        /*
        A binding carries its writes, and each one names the same variable.
        A global function has no binding, and it has no writes either: the
        declaration is the only place the name is written.
        */
        for binding in &names.bindings {
            let declared = &lexed.toks[binding.declared_at as usize];

            if (declared.start, declared.end) != finding.span {
                continue;
            }

            for &write in &binding.writes {
                let token = &lexed.toks[write as usize];

                spans.push((token.start, token.end));
            }
        }

        spans.sort_unstable();
        spans.dedup();

        let edits: Vec<Value> = spans
            .iter()
            .map(|&(start, _)| {
                json!({
                    "range": lines.range(text, (start, start)),
                    "newText": "_",
                })
            })
            .collect();

        out.push(json!({
            "title": format!("Prefix `{name}` with an underscore"),
            "kind": "quickfix",
            "diagnostics": [{
                "range": lines.range(text, finding.span),
                "source": "larvae",
                "code": finding.lint,
            }],
            "edit": { "changes": { uri: edits } },
        }));
    }

    out
}

/// The byte range the editor asked about, or the whole file when it named none.
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
Reports if a finding is worth offering over this range.

A caret is an empty range, so a touch at either edge counts. Without that, the
lightbulb appears on the name and not at the end of it.
*/
fn overlaps(finding: (u32, u32), wanted: (u32, u32)) -> bool {
    finding.0 <= wanted.1 && wanted.0 <= finding.1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(line: u32, character: u32) -> Value {
        json!({
            "start": { "line": line, "character": character },
            "end": { "line": line, "character": character },
        })
    }

    fn actions(text: &str, range: Value) -> Vec<Value> {
        for_range("file:///t.luau", text, &range, &LintConfig::default())
    }

    fn edits(action: &Value) -> Vec<(u32, u32)> {
        action["edit"]["changes"]["file:///t.luau"]
            .as_array()
            .expect("the edit names the file")
            .iter()
            .map(|e| {
                assert_eq!(e["newText"], "_", "the fix inserts one underscore");

                (
                    e["range"]["start"]["line"].as_u64().unwrap() as u32,
                    e["range"]["start"]["character"].as_u64().unwrap() as u32,
                )
            })
            .collect()
    }

    #[test]
    fn an_unused_function_is_offered_the_underscore() {
        let found = actions("local function helper() end\n", at(0, 15));

        assert_eq!(found.len(), 1, "{found:#?}");
        assert_eq!(found[0]["kind"], "quickfix");
        assert_eq!(found[0]["title"], "Prefix `helper` with an underscore");
        assert_eq!(edits(&found[0]), vec![(0, 15)]);
    }

    #[test]
    fn an_unused_variable_is_offered_the_underscore() {
        let found = actions("local x = 1\n", at(0, 6));

        assert_eq!(found.len(), 1, "{found:#?}");
        assert_eq!(edits(&found[0]), vec![(0, 6)]);
    }

    /*
    A binding that is assigned and never read is renamed everywhere.

    The declaration alone would leave `written = 2` assigning a name that
    nothing declares, which is a global. That trades the warning being
    silenced for a worse bug than the one it reported.
    */
    #[test]
    fn every_write_is_renamed_with_the_declaration() {
        let found = actions("local written = 1\nwritten = 2\n", at(0, 6));

        assert_eq!(found.len(), 1, "{found:#?}");
        assert_eq!(edits(&found[0]), vec![(0, 6), (1, 0)]);
    }

    /// A global function has no writes past the one that declares it.
    #[test]
    fn an_unused_global_function_is_offered_the_underscore() {
        let found = actions("function onTouch() end\n", at(0, 9));

        assert_eq!(found.len(), 1, "{found:#?}");
        assert_eq!(edits(&found[0]), vec![(0, 9)]);
    }

    /// A caret somewhere else gets no lightbulb.
    #[test]
    fn a_range_away_from_the_finding_is_offered_nothing() {
        let text = "local function helper() end\nprint(1)\n";

        assert!(actions(text, at(1, 3)).is_empty());
    }

    /// The convention is a prefix, so a name that has one is already silent.
    #[test]
    fn a_name_that_is_already_prefixed_is_offered_nothing() {
        assert!(actions("local function _helper() end\n", at(0, 16)).is_empty());
    }

    /// A file in the middle of an edit has no findings and no actions.
    #[test]
    fn a_file_that_does_not_parse_is_offered_nothing() {
        assert!(actions("local function = = =\n", at(0, 5)).is_empty());
    }

    /// A lint the module cannot fix is not offered a fix.
    #[test]
    fn only_the_unused_pair_is_offered_a_fix() {
        let found = actions("if x then end\n", at(0, 0));

        assert!(
            found
                .iter()
                .all(|a| a["title"].as_str().unwrap().contains("underscore")),
            "{found:#?}"
        );
    }
}
