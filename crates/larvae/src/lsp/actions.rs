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
const FIXABLE: [&str; 3] = ["unused_variable", "unused_function", "prefer_const"];

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

        let word = &text[finding.span.0 as usize..finding.span.1 as usize];

        /*
        Each lint says what its own fix is, because the two are different
        shapes. The unused pair renames a binding, which touches every place
        the name is written. `prefer_const` swaps one keyword.
        */
        let fix = match finding.lint.as_ref() {
            "prefer_const" => to_const(&lines, text, finding.span),

            _ => underscore(&lines, text, &lexed, &names, finding.span, word),
        };

        let Some((title, edits)) = fix else {
            continue;
        };

        out.push(json!({
            "title": title,
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

/*
Rename a binding so it starts with an underscore.

The declaration and every write of it move together. A binding that is
assigned and never read reports as well, and prefixing the declaration alone
would leave the assignments naming something nothing declares, which is a
global and a worse bug than the warning it silenced.
*/
fn underscore(
    lines: &Lines,
    text: &str,
    lexed: &lexer::Lexed,
    names: &scope::Names<'_>,
    span: (u32, u32),
    name: &str,
) -> Option<(String, Vec<Value>)> {
    // The convention is a prefix, so a name that has one is already silent.
    if name.starts_with('_') {
        return None;
    }

    let mut spans = vec![span];

    /*
    A binding carries its writes, and each one names the same variable. A
    global function has no binding, and no writes either: the declaration is
    the only place the name is written.
    */
    for binding in &names.bindings {
        let declared = &lexed.toks[binding.declared_at as usize];

        if (declared.start, declared.end) != span {
            continue;
        }

        for &write in &binding.writes {
            let token = &lexed.toks[write as usize];

            spans.push((token.start, token.end));
        }
    }

    spans.sort_unstable();
    spans.dedup();

    let edits = spans
        .iter()
        .map(|&(start, _)| {
            json!({
                "range": lines.range(text, (start, start)),
                "newText": "_",
            })
        })
        .collect();

    Some((format!("Prefix `{name}` with an underscore"), edits))
}

/*
Turn the `local` of a declaration into `const`.

`prefer_const` reports on the keyword itself, so the span to replace is the
word, and one edit is the whole fix. The lint has already established that
nothing reassigns the names, which is the condition Luau enforces, so the
swap cannot turn a file that ran into a syntax error.
*/
fn to_const(lines: &Lines, text: &str, span: (u32, u32)) -> Option<(String, Vec<Value>)> {
    if &text[span.0 as usize..span.1 as usize] != "local" {
        return None;
    }

    let edits = vec![json!({
        "range": lines.range(text, span),
        "newText": "const",
    })];

    Some(("Change `local` to `const`".to_string(), edits))
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

    /// `prefer_const` is off by default, so a test of it has to turn it on.
    fn with_prefer_const() -> LintConfig {
        let mut cfg = LintConfig::default();

        cfg.rules
            .insert("prefer_const".to_string(), crate::lint::Level::Warn);

        cfg
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

    /*
    `prefer_const` swaps one keyword, and that is the whole fix.

    The lint has already established that nothing reassigns the names, which
    is the condition Luau enforces, so the swap cannot turn a file that ran
    into "Variable is constant and may not be reassigned".
    */
    #[test]
    fn a_local_that_never_moves_is_offered_const() {
        let cfg = with_prefer_const();
        let found = for_range(
            "file:///t.luau",
            "local held = 1\nprint(held)\n",
            &at(0, 2),
            &cfg,
        );

        assert_eq!(found.len(), 1, "{found:#?}");
        assert_eq!(found[0]["title"], "Change `local` to `const`");
        assert_eq!(found[0]["kind"], "quickfix");

        let edit = &found[0]["edit"]["changes"]["file:///t.luau"][0];
        assert_eq!(edit["newText"], "const");
        assert_eq!(edit["range"]["start"]["character"], 0);
        assert_eq!(
            edit["range"]["end"]["character"], 5,
            "the keyword, and only it"
        );
    }

    /// Applying the fix has to leave Luau that parses, and that the lint accepts.
    #[test]
    fn applying_the_const_fix_leaves_parsing_luau() {
        let text = "local held = 1\nprint(held)\n";
        let cfg = with_prefer_const();
        let found = for_range("file:///t.luau", text, &at(0, 2), &cfg);

        let edit = &found[0]["edit"]["changes"]["file:///t.luau"][0];
        let fixed = text.replacen("local", edit["newText"].as_str().unwrap(), 1);

        assert_eq!(fixed, "const held = 1\nprint(held)\n");

        let lexed = crate::syntax::lexer::lex(&fixed).expect("the fix lexes");
        crate::syntax::parser::parse(&fixed, &lexed.toks).expect("the fix parses");

        // and the lint it fixed no longer fires
        let again = for_range("file:///t.luau", &fixed, &at(0, 2), &cfg);
        assert!(again.is_empty(), "{again:#?}");
    }

    /// The action is off while the lint is, because there is no finding to fix.
    #[test]
    fn no_const_action_while_the_lint_is_allowed() {
        assert!(actions("local held = 1\nprint(held)\n", at(0, 2)).is_empty());
    }

    /*
    Applying the underscore fix has to leave Luau that parses too.

    The assigned-but-never-read case is the one that could break: the
    declaration and the assignment have to move together or the assignment
    names a global.
    */
    #[test]
    fn applying_the_underscore_fix_leaves_parsing_luau() {
        let text = "local written = 1\nwritten = 2\n";
        let found = actions(text, at(0, 6));

        let mut fixed = text.to_string();

        // apply back to front so the earlier offsets stay valid
        let mut edits = edits(&found[0]);
        edits.sort_unstable();

        for (line, character) in edits.into_iter().rev() {
            let at = fixed
                .lines()
                .take(line as usize)
                .map(|l| l.len() + 1)
                .sum::<usize>()
                + character as usize;

            fixed.insert(at, '_');
        }

        assert_eq!(fixed, "local _written = 1\n_written = 2\n");

        let lexed = crate::syntax::lexer::lex(&fixed).expect("the fix lexes");
        crate::syntax::parser::parse(&fixed, &lexed.toks).expect("the fix parses");
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
