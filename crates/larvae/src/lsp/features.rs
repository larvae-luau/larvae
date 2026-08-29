/*!
The requests beyond diagnostics: formatting and the outline.
*/

use anyhow::{Context, Result};
use serde_json::{Value, json};

use crate::fmt;
use crate::worm::proto;

use super::uri::path_of_uri;
use super::{Server, rpc};

/// The identifier the author is in the middle of typing, before the cursor
fn word_before(src: &str, at: u32) -> String {
    let head = &src[..at.min(src.len() as u32) as usize];

    head.chars()
        .rev()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
}

/*
The line where an auto-import lands: after the imports the file already
has, before its first real statement.

The scan walks whole lines from the top. A comment, a blank line, or an
existing import (`local`/`const` bound to a `require` or a `GetService`)
extends the preamble; the first line that is none of those ends it. So a
file that opens with a guard clause gets its import above the guard, and
a file with an import block gets the new line at the block's end.
*/
fn import_insertion_line(src: &str) -> u32 {
    let mut last_import_end = 0u32;

    for (line, text) in (0u32..).zip(src.lines()) {
        let trimmed = text.trim_start();

        let is_preamble = trimmed.is_empty()
            || trimmed.starts_with("--")
            || ((trimmed.starts_with("local ") || trimmed.starts_with("const "))
                && (trimmed.contains("require(") || trimmed.contains("GetService(")));

        if !is_preamble {
            break;
        }

        if !trimmed.is_empty() && !trimmed.starts_with("--") {
            last_import_end = line + 1;
        }
    }

    last_import_end
}

/*
An offset of the original, moved onto the lowering.

A front-end worm preserves line numbers by contract, so the line carries
over whole. The column does not: a line the worm rewrote holds the same
names at different places, and `<TextLabel Text={props.Title} />` becomes
`vide.create("TextLabel")({ Text = props.Title, })`, where every column past
the first is somewhere else.

So the column moves by the name under the cursor. The name is copied into
the lowering verbatim, because a hole holds Luau and a worm that rewrote it
would break the type it reports. Finding the same name in the lowered line
puts the cursor back on it, and the offset inside the name carries over
unchanged.

Where the line came through untouched, and where there is no name to follow,
the column clamps to the lowered line, which is what it always did.
*/
fn lowered_offset(original: &str, lowered: &str, at: u32) -> u32 {
    let head = &original[..(at as usize).min(original.len())];
    let line = head.matches('\n').count();
    let column = head.len() - head.rfind('\n').map(|n| n + 1).unwrap_or(0);

    let source = original.split_inclusive('\n').nth(line).unwrap_or("");
    let mut start = 0usize;

    for (i, text) in lowered.split_inclusive('\n').enumerate() {
        if i == line {
            let generated = text.trim_end_matches('\n');

            if let Some(column) = aligned(source.trim_end_matches('\n'), generated, column) {
                return (start + column) as u32;
            }

            return (start + column.min(generated.len())) as u32;
        }

        start += text.len();
    }

    lowered.len() as u32
}

/*
The column in the lowered line that holds the same name as `column` does.

A name appearing more than once is answered by counting: the third `value`
of the source line is the third `value` of the lowered line. The count is
what makes this safe on a line the worm rewrote, since a rewrite reorders
names and does not invent them.

Nothing comes back when the line is unchanged, when the cursor is not on a
name, or when the lowering does not hold that name. Each one is a case the
clamp already answers, and a guess would answer worse.
*/
fn aligned(source: &str, generated: &str, column: usize) -> Option<usize> {
    if source == generated || column > source.len() {
        return None;
    }

    let (start, end) = word_at(source, column)?;
    let word = &source[start..end];
    let which = words(source, word).take_while(|at| *at < start).count();

    words(generated, word)
        .nth(which)
        .map(|at| at + column - start)
}

/// Where the name under `column` starts and ends, if the cursor is on one.
fn word_at(line: &str, column: usize) -> Option<(usize, usize)> {
    let bytes = line.as_bytes();
    let mut start = column;
    let mut end = column;

    while start > 0 && is_word(bytes[start - 1]) {
        start -= 1;
    }

    while end < bytes.len() && is_word(bytes[end]) {
        end += 1;
    }

    (start < end).then_some((start, end))
}

/// Every place `word` stands alone in the line, in order.
fn words<'a>(line: &'a str, word: &'a str) -> impl Iterator<Item = usize> + 'a {
    let bytes = line.as_bytes();

    line.match_indices(word).filter_map(move |(at, _)| {
        let before = at == 0 || !is_word(bytes[at - 1]);
        let after = at + word.len() >= bytes.len() || !is_word(bytes[at + word.len()]);

        (before && after).then_some(at)
    })
}

/// A byte a name is made of. A digit counts, and the first byte of a name
/// never is one, so this needs no second rule.
fn is_word(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

#[cfg(test)]
mod lowering_tests {
    use super::lowered_offset;

    /// The offset of `|` in the marked text, and the text without it.
    fn cursor(marked: &str) -> (String, u32) {
        let at = marked.find('|').expect("a cursor");

        (marked.replace('|', ""), at as u32)
    }

    /// The lowered text from the offset out, so a failure reads as a place.
    fn landed_on(original: &str, lowered: &str, marked: &str) -> String {
        let (_, at) = cursor(marked);
        let at = lowered_offset(original, lowered, at) as usize;

        lowered[at..].chars().take(12).collect()
    }

    #[test]
    fn a_line_the_worm_left_alone_keeps_its_column() {
        let text = "local a = 1\nlocal b = 2\n";

        assert_eq!(
            landed_on(text, text, "local a = 1\nlocal |b = 2\n"),
            "b = 2\n"
        );
    }

    /*
    The case the whole thing exists for: a name inside a hole, on a line
    the worm rewrote around it.
    */
    #[test]
    fn a_name_inside_a_hole_lands_on_itself() {
        let original = "\t<TextLabel Text={props.Title} />\n";
        let lowered = "\tcreate(\"TextLabel\")({ Text = props.Title, })\n";

        assert_eq!(
            landed_on(original, lowered, "\t<TextLabel Text={props.T|itle} />\n"),
            "itle, })\n"
        );
    }

    /// The same name two times is answered by which one, not by the first.
    #[test]
    fn the_second_of_a_name_stays_the_second() {
        let original = "\t<Frame A={value} B={value} />\n";
        let lowered = "\tcreate(\"Frame\")({ A = value, B = value, })\n";

        let (_, at) = cursor("\t<Frame A={value} B={va|lue} />\n");
        let landed = lowered_offset(original, lowered, at) as usize;

        assert!(
            lowered[..landed].contains("B = "),
            "landed at {landed}, before it: {:?}",
            &lowered[..landed]
        );
        assert_eq!(&lowered[landed..landed + 3], "lue");
    }

    #[test]
    fn a_cursor_on_no_name_clamps_as_it_did() {
        let original = "\t<Frame A={x} />\n";
        let lowered = "\tcreate(\"Frame\")({ A = x, })\n";
        let (_, at) = cursor("\t<Frame A={x}| />\n");

        // Nothing to follow, so the column clamps and stays on the line.
        let landed = lowered_offset(original, lowered, at) as usize;

        assert!(landed <= lowered.trim_end_matches('\n').len());
    }

    #[test]
    fn a_name_the_lowering_dropped_clamps_rather_than_guesses() {
        let original = "\t<Frame Gone={1} />\n";
        let lowered = "\tcreate(\"Frame\")({ })\n";
        let (_, at) = cursor("\t<Frame Go|ne={1} />\n");

        let landed = lowered_offset(original, lowered, at) as usize;

        assert!(landed <= lowered.trim_end_matches('\n').len());
    }

    #[test]
    fn a_line_past_the_end_of_the_lowering_answers_its_end() {
        assert_eq!(lowered_offset("a\nb\nc\n", "a\n", 4), 2);
    }
}

/*
One hover card: the type, then the reference page under it.

The rule is the shape luau-lsp writes, so a reader who moves between the two
servers reads one card. The type goes in a Luau fence, a rule of ten dashes
separates it, and the prose follows as markdown.
*/
pub(super) fn card(text: &str, documentation: Option<&str>) -> String {
    let mut out = format!("```luau\n{text}\n```");

    if let Some(docs) = documentation
        .map(visible_docs)
        .filter(|d| !d.trim().is_empty())
    {
        out.push_str("\n----------\n");
        out.push_str(&docs);
    }

    out
}

/*
Documentation with its `$` lines hidden.

A doc comment's Luau fence sometimes needs a line for the example to
typecheck that no reader needs to see, ex: the `local` that binds what
the interesting line uses. luau-lsp hides a fence line that starts with
`$`, so a doc written for either server renders the same. Only a `luau`
fence hides them: a dollar elsewhere in prose is a dollar.
*/
/*
Where a hover on a declaration keyword should point instead.

`const`, `local`, and `export` declare something, and a hover on the
keyword should answer with what they declare, so the redirect returns
the offset of the bound name. `export` redirects only while the session
parses value exports; switched off, the analyzer's own error-type
answer stands, which is the true state of that file.
*/
pub(super) fn dialect_target(src: &str, at: u32, export_values: bool) -> Option<u32> {
    use crate::syntax::lexer::TokKind;

    let lexed = crate::syntax::lexer::lex(src).ok()?;
    let toks = &lexed.toks;

    let k = toks
        .iter()
        .position(|t| t.start <= at && at < t.end && matches!(t.kind, TokKind::Ident))?;

    let name_after = |mut j: usize| -> Option<u32> {
        if toks.get(j).is_some_and(|t| t.text(src) == "function") {
            j += 1;
        }

        let t = toks.get(j)?;

        matches!(t.kind, TokKind::Ident).then_some(t.start)
    };

    match toks[k].text(src) {
        "const" | "local" => name_after(k + 1),

        "export" if export_values => match toks.get(k + 1)?.text(src) {
            "local" | "const" => name_after(k + 2),
            "function" => name_after(k + 1),

            _ => None,
        },

        _ => None,
    }
}

/*
The author's own spelling on a hover card.

The analyzer reads `local` where the file says `const` or
`export local`, so its card starts with `local`. This looks up the
declaration of the hovered name in the file and puts the dialect's
words back. The nearest declaration at or above the hover wins, so a
shadowing `local` inside a function keeps its own plain card.
*/
pub(super) fn dialect_label(src: &str, at: u32, text: &str) -> Option<String> {
    use crate::syntax::lexer::TokKind;

    let is_function = text.starts_with("function ");
    let after = text
        .strip_prefix("local ")
        .or_else(|| text.strip_prefix("function "))?;

    let name: String = after
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();

    if name.is_empty() {
        return None;
    }

    let lexed = crate::syntax::lexer::lex(src).ok()?;
    let toks = &lexed.toks;

    // Every declaration of the name: (keyword offset, const, exported)
    let mut decls: Vec<(u32, bool, bool)> = Vec::new();

    for (k, t) in toks.iter().enumerate() {
        if !matches!(t.kind, TokKind::Ident) {
            continue;
        }

        let exported = k > 0 && toks[k - 1].text(src) == "export";

        match t.text(src) {
            word @ ("const" | "local") => {
                let mut j = k + 1;

                if toks.get(j).is_some_and(|n| n.text(src) == "function") {
                    j += 1;
                }

                // The binding list: plain names over commas, until anything else.
                while let Some(n) = toks.get(j) {
                    if !matches!(n.kind, TokKind::Ident) {
                        break;
                    }

                    if n.text(src) == name {
                        decls.push((t.start, word == "const", exported));
                        break;
                    }

                    match toks.get(j + 1).map(|s| s.text(src)) {
                        Some(",") => j += 2,

                        _ => break,
                    }
                }
            }

            "function"
                if exported
                    && toks.get(k + 1).is_some_and(|n| {
                        matches!(n.kind, TokKind::Ident) && n.text(src) == name
                    }) =>
            {
                decls.push((t.start, false, true));
            }

            _ => {}
        }
    }

    let &(_, is_const, exported) = decls
        .iter()
        .rfind(|d| d.0 <= at)
        .or_else(|| decls.first())?;

    if !is_const && !exported {
        return None;
    }

    let mut out = text.to_string();

    if is_const && !is_function {
        out = out.replacen("local ", "const ", 1);
    }

    if exported {
        out = format!("export {out}");
    }

    Some(out)
}

fn visible_docs(docs: &str) -> String {
    let mut out = String::with_capacity(docs.len());
    let mut in_luau_fence = false;

    for line in docs.split_inclusive('\n') {
        let trimmed = line.trim_start();

        if let Some(rest) = trimmed.strip_prefix("```") {
            in_luau_fence = !in_luau_fence && rest.trim_end().eq_ignore_ascii_case("luau");
        } else if in_luau_fence && trimmed.starts_with('$') {
            continue;
        }

        out.push_str(line);
    }

    out
}

/*
A key a dot cannot reach rewrites itself into brackets on accept.

`t.Jump Force` is not Luau, so accepting `Jump Force` after a dot has
to write `t["Jump Force"]`: the edit replaces what was typed with the
bracketed key, in the quote `[fmt] quote_style` keeps, and one more
edit removes the dot the author already wrote. The label stays as the
key, so typing `Jum` keeps matching it.
*/
fn bracket_member_offers(items: &mut [Value], src: &str, at: u32, quote: char) {
    let at = (at as usize).min(src.len());
    let bytes = src.as_bytes();
    let mut prefix = at;

    while prefix > 0 && (bytes[prefix - 1].is_ascii_alphanumeric() || bytes[prefix - 1] == b'_') {
        prefix -= 1;
    }

    // Only after a member dot; `..` is concatenation, not access.
    if prefix == 0 || bytes[prefix - 1] != b'.' || (prefix >= 2 && bytes[prefix - 2] == b'.') {
        return;
    }

    let lines = rpc::Lines::new(src);
    let dot = (prefix - 1) as u32;

    for item in items {
        let Some(label) = item["label"].as_str().map(str::to_string) else {
            continue;
        };
        let label = label.as_str();

        let identifier = !label.is_empty()
            && label
                .chars()
                .enumerate()
                .all(|(i, c)| c == '_' || c.is_ascii_alphabetic() || (i > 0 && c.is_ascii_digit()));

        // 14 is Keyword; a keyword after a dot is not a key of anything.
        if identifier || item["kind"] == 14 {
            continue;
        }

        let escaped = label
            .replace('\\', "\\\\")
            .replace(quote, &format!("\\{quote}"));

        item["textEdit"] = json!({
            "range": lines.range(src, (prefix as u32, at as u32)),
            "newText": format!("[{quote}{escaped}{quote}]"),
        });
        item["additionalTextEdits"] = json!([{
            "range": lines.range(src, (dot, dot + 1)),
            "newText": "",
        }]);
        item["filterText"] = json!(label);
    }
}

/*
The binding name a module auto-import offers, or nothing.

A name has to be writable as a Luau identifier to bind, a script is not
a module, and the file being edited never offers itself.
*/
fn module_import_stem(target: &std::path::Path, editing: &std::path::Path) -> Option<String> {
    if target == editing {
        return None;
    }

    let stem = target.file_stem()?.to_str()?;

    if stem.contains(".server") || stem.contains(".client") {
        return None;
    }

    let stem = match stem {
        "init" => target.parent()?.file_name()?.to_str()?,

        named => named,
    };

    let identifier = stem
        .chars()
        .enumerate()
        .all(|(i, c)| c == '_' || c.is_ascii_alphabetic() || (i > 0 && c.is_ascii_digit()));

    (identifier && !stem.is_empty()).then(|| stem.to_string())
}

/// The byte offset of the position in a request's params
fn position_byte(src: &str, params: &Value) -> u32 {
    let line = params["position"]["line"].as_u64().unwrap_or(0) as u32;
    let character = params["position"]["character"].as_u64().unwrap_or(0) as u32;

    rpc::Lines::new(src).byte_of(src, line, character)
}

/*
One completion, as the protocol wants it.

Both routes render through here: the plain Luau file and the file a worm
claims. They used to differ, and the claimed one ranked every entry the
same, so the props of a component sat in the alphabet with the whole global
scope. One list cannot have two orders.

The tiers are luau-lsp's, with one addition of larvae's own. A keyword that
fits the position outranks everything, because the bug that answers is real:
an author types `end` to close a guard clause and the list hands them
EncodingService. Under that, the order is the answer Luau gives and not a
guess from the kind: an entry that fits the type the position expects comes
first, which is what puts a component's props above the alphabet.
*/
fn completion_item(c: &crate::lsp::analysis::AnalysisCompletion) -> Value {
    let tier = match (c.kind, c.type_correct, c.wrong_index_type, c.deprecated) {
        (14, ..) => '0',

        // A name the type does not take reads last of the real answers.
        (_, _, true, _) => '7',

        // Deprecated is offered and never preferred.
        (.., true) => '8',

        (_, 1, ..) => '1',

        (_, 2, ..) => '2',

        (5 | 10, ..) => '3',

        (3 | 12, ..) => '4',

        _ => '5',
    };

    let mut item = json!({
        "label": c.label,
        "kind": c.kind,
        "detail": c.detail,
        "sortText": format!("{tier}{}", c.label),
        // 1 is PlainText: an insertion is text here and never a snippet.
        "insertTextFormat": 1,
        "deprecated": c.deprecated,
        "preselect": false,
    });

    /*
    The argument names go against the label, and the editor writes the
    parentheses of a call itself.
    */
    if let Some(names) = &c.label_detail {
        item["labelDetails"] = json!({ "detail": names });
    }

    if let Some(insert) = &c.insert_text {
        item["insertText"] = json!(insert);
    }

    /*
    The comment above the declaration, as markdown. An editor draws it in
    the panel beside the list, which is where a reader looks while they
    scroll it.
    */
    if let Some(docs) = &c.documentation {
        item["documentation"] = json!({ "kind": "markdown", "value": visible_docs(docs) });
    }

    // 1 is Deprecated, the one tag the protocol defines.
    if c.deprecated {
        item["tags"] = json!([1]);
    }

    item
}

impl Server {
    /*
    The compiled form of one open document, for `larvae/bytecode` and
    `larvae/compilerRemarks`.

    The editor asks with an optimization level, and `[lsp.bytecode]` supplies
    the rest: the debug level, the type info level, and the vector
    configuration, so the listing matches what a build of this project would
    run. A claimed file compiles through its worm's lowering, because that is
    the Luau the place receives.
    */
    pub(super) fn bytecode(&self, params: &Value, remarks: bool) -> Value {
        if !self.lsp.analyzer {
            return Value::Null;
        }

        let uri = super::uri::uri_of(params);

        let Some(src) = self.documents.get(&uri) else {
            return Value::Null;
        };

        let Some(path) = path_of_uri(&uri) else {
            return Value::Null;
        };

        // luau-lsp's default is O2, and its editor command asks the same way.
        let optimization = params["optimizationLevel"].as_u64().unwrap_or(2).min(2) as u8;

        let text = match self.worms.frontend_for(&path) {
            Some(index) => match self.worms.compile(index, src) {
                Ok(outcome) if outcome.ok => outcome.text,

                // A file that does not compile has no bytecode to show.
                _ => return Value::Null,
            },

            None => src.clone(),
        };

        let view = super::analysis::plain_view(&text);

        let listing = self
            .analysis
            .borrow_mut()
            .as_mut()
            .and_then(|a| a.bytecode(&view, optimization, remarks, &self.lsp.bytecode));

        match listing {
            Some(text) => json!(text),

            None => Value::Null,
        }
    }

    /*
    What the half-written require at the cursor can become.

    Every offer carries its own insertion, because a directory ends in a
    slash that the label shows and the filter would otherwise fight. The
    list is complete for the directory it names, so the editor filters it
    on the next keystroke rather than asking again.
    */
    fn require_completions(
        &self,
        src: &str,
        at: u32,
        partial: &str,
        path: &std::path::Path,
    ) -> Value {
        let root = match &self.root {
            Some(root) => root.as_path(),

            // With no project root, a relative spec still reads against the file.
            None => path.parent().unwrap_or(path),
        };

        /*
        The edit replaces what the author typed of the current segment, and
        the range says so. Without it the editor guesses a word range, and
        its guess has no `@` and no `.` in it: typing `@sh` filtered
        against a word of `sh`, no offer matched, and the list closed
        instead of narrowing.
        */
        let segment = partial.rfind('/').map(|cut| cut + 1).unwrap_or(0);
        let lines = rpc::Lines::new(src);
        let replace_from = at - (partial.len() - segment) as u32;
        let range = lines.range(src, (replace_from, at));

        let luaurc = super::decorate::luaurc_upward(path, root);

        let items: Vec<Value> = super::requires::candidates(
            partial,
            path,
            root,
            &self.aliases,
            &luaurc,
            &self.mounts,
            &self.worms.lsp_resolved_claims(),
        )
        .into_iter()
        .map(|c| {
            json!({
                "label": c.label,
                "kind": c.kind,
                "detail": c.detail,
                "textEdit": { "range": range, "newText": c.insert },
                "filterText": c.label,
                // A directory sorts after the files that sit beside it.
                "sortText": match c.kind {
                    19 => format!("1{}", c.label),

                    _ => format!("0{}", c.label),
                },
            })
        })
        .collect();

        json!({ "isIncomplete": false, "items": items })
    }

    /*
    The type at the cursor, from the analyzer behind the seam.

    The position arrives as a line and a UTF-16 character, converts to a
    byte offset once here, and crosses the seam as bytes. No analyzer, or
    a claimed file, answers null, and the editor shows nothing.
    */
    pub(super) fn hover(&self, params: &Value) -> Value {
        let uri = super::uri::uri_of(params);

        // `[lsp.hover] enabled = false` answers with nothing, as luau-lsp does.
        // `[lsp] analyzer = false` turns the whole half off the same way.
        if !self.lsp.analyzer || !self.lsp.hover.enabled || self.declines(&uri) {
            return Value::Null;
        }

        /*
        A card that says it is loading, while the session is still being
        built. Nothing at all reads as "this has no type", which is wrong
        and which the reader cannot tell from the truth. The card said
        `...` before, which says nothing to the person reading it.
        */
        if self.analysis_loading() {
            return json!({
                "contents": { "kind": "markdown", "value": "```luau\nLoading...\n```" },
            });
        }

        let Some(src) = self.documents.get(&uri) else {
            return Value::Null;
        };

        let Some(path) = path_of_uri(&uri) else {
            return Value::Null;
        };

        let at = position_byte(src, params);
        let context = json!({ "path": path, "text": src, "offset": at });

        /*
        A hover on `const`, `local`, or `export` answers with what the
        keyword declares. The analyzer is asked at the bound name; the
        context keeps the real cursor, so a worm's markup answer stays
        where the author points.
        */
        let export_values = self
            .lsp
            .fflags
            .over
            .get("LuauExportValueSyntax")
            .is_none_or(|v| v == "true");

        let ask = dialect_target(src, at, export_values).unwrap_or(at);

        /*
        A claimed file gets both halves, in the order the completions
        take: the analyzer answers first, over the worm's lowering with
        positions mapped by line, and the worm's respond hook sees that
        answer and wins where it answers. The order is what lets a markup
        card carry the type underneath it: a component hover shows the
        signature of the function the element calls, and the worm can
        only say so if the analyzer has already spoken.
        */
        if let Some(index) = self.worms.frontend_for(&path) {
            let base = match self.worms.compile(index, src) {
                Ok(outcome) if outcome.ok => {
                    let lowered = super::analysis::plain_view(&outcome.text);
                    let mut analysis = self.analysis.borrow_mut();

                    analysis
                        .as_mut()
                        .and_then(|a| {
                            a.open(&path, &lowered);

                            a.hover(
                                &path,
                                lowered_offset(src, &lowered, ask),
                                self.lsp.hover.show_table_kinds,
                                self.lsp.hover.include_string_length,
                            )
                        })
                        .map(|text| {
                            let text = dialect_label(src, ask, &text).unwrap_or(text);

                            json!({
                                "contents": {
                                    "kind": "markdown",
                                    "value": card(&self.instances.readable(&text), None),
                                }
                            })
                        })
                        .unwrap_or(Value::Null)
                }

                _ => Value::Null,
            };

            return self.worms.lsp_respond("hover", &context, base);
        }

        let view = super::analysis::plain_view(src);
        let mut analysis = self.analysis.borrow_mut();

        let Some((text, docs)) = analysis.as_mut().and_then(|a| {
            a.open(&path, &view);

            let text = a.hover(
                &path,
                ask,
                self.lsp.hover.show_table_kinds,
                self.lsp.hover.include_string_length,
            )?;

            Some((text, a.hover_documentation(&path, ask)))
        }) else {
            return Value::Null;
        };

        let text = dialect_label(src, ask, &text).unwrap_or(text);

        drop(analysis);

        let hover = json!({
            "contents": {
                "kind": "markdown",
                "value": card(&self.instances.readable(&text), docs.as_deref()),
            }
        });

        // Tier 3: the worms that transform hovers see it before the editor.
        self.worms.lsp_respond("hover", &context, hover)
    }

    /// Completions at the cursor, from the analyzer behind the seam
    pub(super) fn completions(&self, params: &Value) -> Value {
        let uri = super::uri::uri_of(params);

        if !self.lsp.analyzer || !self.lsp.completion.enabled || self.declines(&uri) {
            return json!([]);
        }

        let Some(src) = self.documents.get(&uri) else {
            return json!([]);
        };

        let Some(path) = path_of_uri(&uri) else {
            return json!([]);
        };

        let at = position_byte(src, params);

        /*
        A require spec is answered first, and by the filesystem.

        The analyzer has nothing to say about the text between the quotes:
        it names a file, and the list of files is what the author needs. So
        this answers while the session is still loading, and it answers
        before the worm route too, because a require of a claimed file is
        written in a plain Luau file just as often.
        */
        if let Some(partial) = super::requires::spec_at(src, at) {
            return self.require_completions(src, at, partial, &path);
        }

        /*
        An incomplete list, so the editor asks again on the next keystroke
        rather than caching an empty one for the rest of the session.
        */
        if self.analysis_loading() {
            return json!({ "isIncomplete": true, "items": [] });
        }

        let context = json!({ "path": path, "text": src, "offset": at });

        /*
        A claimed file's completions are the worm's markup answers plus
        the analyzer's answers over the lowering, in one list.
        */
        if let Some(index) = self.worms.frontend_for(&path) {
            let mut items = match self.worms.compile(index, src) {
                Ok(outcome) if outcome.ok => {
                    let lowered = super::analysis::plain_view(&outcome.text);
                    let mut analysis = self.analysis.borrow_mut();

                    analysis
                        .as_mut()
                        .map(|a| {
                            a.open(&path, &lowered);

                            a.completions(&path, lowered_offset(src, &lowered, at))
                        })
                        .unwrap_or_default()
                }

                _ => Vec::new(),
            };

            let mut base: Vec<Value> = items
                .drain(..)
                .filter(|c| !self.lsp.completion.hide_deprecated || !c.deprecated)
                .map(|c| completion_item(&c))
                .collect();

            bracket_member_offers(&mut base, src, at, self.insert_quote());

            return self.worms.lsp_respond("completions", &context, json!(base));
        }

        let view = super::analysis::plain_view(src);
        let mut analysis = self.analysis.borrow_mut();

        let Some(analysis) = analysis.as_mut() else {
            return json!([]);
        };

        analysis.open(&path, &view);

        /*
        [`completion_item`] spells the order. Auto-imports rank last, below
        every tier it writes: they are the most speculative offer in the
        list, and they must never win a race against syntax.
        */
        let prefix = word_before(src, at);

        let mut items: Vec<Value> = analysis
            .completions(&path, at)
            .into_iter()
            // 14 is Keyword. A project that finds them noisy turns them off.
            .filter(|c| self.lsp.completion.show_keywords || c.kind != 14)
            .filter(|c| !self.lsp.completion.hide_deprecated || !c.deprecated)
            .map(|c| {
                let mut item = completion_item(&c);

                /*
                An exactly typed keyword preselects, so enter confirms what
                the author wrote rather than the first name in the list.
                */
                if c.kind == 14 && !prefix.is_empty() && c.label == prefix {
                    item["preselect"] = json!(true);
                }

                item
            })
            .collect();

        /*
        Service auto-imports, the parity feature with the fix built in.
        Each one carries its own insertion: a binding above the first real
        statement of the file, never inside the block the cursor sits in. A
        service the file already binds does not offer.

        `[lsp.completion.imports] use_const` decides the keyword, and the
        detail line shows the same text the edit inserts. A user reads that
        line before accepting, so the two cannot differ.
        */
        if !prefix.is_empty() && self.lsp.completion.imports.enabled {
            let keyword = self.lsp.completion.imports.keyword();
            let lines = rpc::Lines::new(src);

            /*
            The quote follows `[fmt] quote_style`, so an accepted import
            does not fight the formatter one save later. A service name
            never needs an escape, so the two auto styles read as their
            preference alone, and `preserve` writes the default.
            */
            let quote = self.insert_quote();

            for service in analysis.services() {
                if !service.starts_with(prefix.as_str())
                    || src.contains(&format!("GetService(\"{service}\")"))
                    || src.contains(&format!("GetService('{service}')"))
                {
                    continue;
                }

                let insert_at = import_insertion_line(src);

                items.push(json!({
                    "label": service,
                    "kind": 9,
                    "detail": format!(
                        "auto-import: {keyword} {service} = game:GetService({quote}{service}{quote})"
                    ),
                    "sortText": format!("9{service}"),
                    "additionalTextEdits": [{
                        "range": {
                            "start": { "line": insert_at, "character": 0 },
                            "end": { "line": insert_at, "character": 0 },
                        },
                        "newText": format!(
                            "{keyword} {service} = game:GetService({quote}{service}{quote})\n"
                        ),
                    }],
                }));
            }

            /*
            Module auto-imports, beside the services. Every file of the
            project is offerable by its stem, and accepting one writes the
            require in the style `[lsp.completion.imports] require_style`
            picks, in the quote the formatter keeps. A `.server.` or
            `.client.` file is a script and not a module, so it never
            offers, and an `init` file offers as its directory.
            */
            for target in self.symbols.files() {
                let Some(stem) = module_import_stem(target, &path) else {
                    continue;
                };

                if !stem.starts_with(prefix.as_str()) || src.contains(&format!(" {stem} = require"))
                {
                    continue;
                }

                let spoken = match target.file_stem().and_then(|s| s.to_str()) {
                    Some("init") => target.parent().unwrap_or(target).to_path_buf(),

                    _ => target.with_extension(""),
                };

                let spec = self.import_spec(&path, &spoken);
                let insert_at = import_insertion_line(src);

                items.push(json!({
                    "label": stem,
                    "kind": 9,
                    "detail": format!(
                        "auto-import: {keyword} {stem} = require({quote}{spec}{quote})"
                    ),
                    "sortText": format!("9{stem}"),
                    "additionalTextEdits": [{
                        "range": {
                            "start": { "line": insert_at, "character": 0 },
                            "end": { "line": insert_at, "character": 0 },
                        },
                        "newText": format!(
                            "{keyword} {stem} = require({quote}{spec}{quote})\n"
                        ),
                    }],
                }));
            }

            let _ = lines;
        }

        bracket_member_offers(&mut items, src, at, self.insert_quote());

        // Tier 3: the worms that transform completions see the list first.
        self.worms
            .lsp_respond("completions", &context, json!(items))
    }

    /*
    The spec an auto-import writes for one module, in the configured style.

    Every style falls back to the relative form, which can say anywhere:
    an alias only speaks for the tree under it, and `@game` only for a
    mounted one.
    */
    fn import_spec(&self, requirer: &std::path::Path, spoken: &std::path::Path) -> String {
        use crate::config::lsp::RequireStyle;

        let style = self.lsp.completion.imports.require_style;

        let alias = || {
            let root = self.root.as_deref()?;

            self.aliases
                .iter()
                .filter_map(|(name, value)| {
                    let kept = spoken.strip_prefix(root.join(value)).ok()?;

                    Some(format!("@{name}/{}", super::renames::segments(kept)))
                })
                .min_by_key(String::len)
        };

        let game = || {
            let dm = self.mounts.dm_of(spoken)?;

            Some(dm.game_path())
        };

        match style {
            RequireStyle::Auto => alias(),

            RequireStyle::AlwaysRelative => None,

            RequireStyle::AlwaysAbsolute => game(),

            RequireStyle::NearestAbsolute => alias().or_else(game),
        }
        .unwrap_or_else(|| super::renames::relative_spec(requirer, spoken))
    }

    /// The quote an inserted string takes, from `[fmt] quote_style`.
    fn insert_quote(&self) -> char {
        match self.fmt.quote_style {
            crate::fmt::config::QuoteStyle::AutoPreferSingle
            | crate::fmt::config::QuoteStyle::ForceSingle => '\'',

            _ => '"',
        }
    }

    /// Reports if the `[lsp]` mode leaves this file to another server
    fn declines(&self, uri: &str) -> bool {
        if !self.lsp.enabled {
            return true;
        }

        if !self.lsp.claim_only {
            return false;
        }

        /*
        A worm can declare that its hooks answer inside plain Luau files,
        ex: the json worm resolving data requires written in .luau code.
        Claim-only gating widens then, or installing the worm changes
        nothing in the editor.
        */
        if self.worms.lsp_serves_luau() {
            return false;
        }

        !path_of_uri(uri).is_some_and(|p| self.worms.frontend_for(&p).is_some())
    }

    /// One edit that replaces the whole document; a formatter produces this
    pub(super) fn format(&self, uri: &str) -> Result<Value> {
        if self.declines(uri) {
            return Ok(Value::Null);
        }

        let Some(src) = self.documents.get(uri) else {
            return Ok(Value::Null);
        };

        // `[fmt] enabled = false` reaches the editor as a formatter with no edits
        if !self.fmt.enabled {
            return Ok(json!([]));
        }

        let Some(formatted) = self.formatted(uri, src)? else {
            return Ok(json!([]));
        };

        // An edit that changes nothing still makes the editor mark the file dirty.
        if formatted == *src {
            return Ok(json!([]));
        }

        Ok(json!([{
            "range": rpc::Lines::new(src).whole(src),
            "newText": formatted,
        }]))
    }

    /*
    The formatted text of one document, from the owner of its extension.

    A claimed file goes to its worm. The worm replies with a layout document,
    and larvae renders it in the style of the project. A worm that does not
    format its files gives `None` here, and the server then sends no edit. A
    message is correct for `larvae fmt`, because a user named that file. A
    message is wrong for an editor, because the editor asks on each save.
    */
    fn formatted(&self, uri: &str, src: &str) -> Result<Option<String>> {
        let Some(index) = path_of_uri(uri).and_then(|p| self.worms.frontend_for(&p)) else {
            return fmt::format(src, &self.fmt).map(Some);
        };

        let spec = self.worms.spec(index);

        if !spec.formats() {
            return Ok(None);
        }

        let reply = self.worms.format(index, src)?;

        // a project can keep one option out of the files this worm claims
        let cfg = self.fmt.without(&spec.inherit.fmt_except);

        proto::render_format(src, &reply, &cfg)
            .with_context(|| format!("worm `{}`", spec.manifest.name))
            .map(Some)
    }

    /*
    The outline; a symbol picker and the breadcrumb bar read it.

    Top level declarations only. No user navigates to a nested helper by
    name. A list of them makes the outline of a large module longer than the
    module.
    */
    pub(super) fn symbols(&self, uri: &str) -> Value {
        if self.declines(uri) {
            return json!([]);
        }

        let Some(src) = self.documents.get(uri) else {
            return json!([]);
        };

        let Ok(lexed) = crate::syntax::lexer::lex(src) else {
            return json!([]);
        };

        let options = path_of_uri(uri)
            .map(|p| crate::syntax::parser::ParseOptions::for_path(&p))
            .unwrap_or_default();

        let Ok(chunk) = crate::syntax::parser::parse_with(src, &lexed.toks, options) else {
            return json!([]);
        };

        let lines = rpc::Lines::new(src);
        let bytes = |span: crate::syntax::ast::TokSpan| {
            (
                lexed.toks[span.start as usize].start,
                lexed.toks[span.end as usize - 1].end,
            )
        };

        let mut out = Vec::new();

        for stmt in &chunk.block.stmts {
            use crate::syntax::ast::Stmt;

            // 12 is Function and 13 is Variable, in the numbering of the protocol.
            let (name, kind, span) = match stmt {
                Stmt::Function(n) => {
                    let path: Vec<&str> = n
                        .path
                        .iter()
                        .map(|p| lexed.toks[p.start as usize].text(src))
                        .collect();

                    (path.join("."), 12, n.span)
                }

                Stmt::Class(n) => (
                    lexed.toks[n.name.start as usize].text(src).to_string(),
                    // 5 is Class, in the numbering of the protocol.
                    5,
                    n.span,
                ),

                Stmt::LocalFunction(n) => (
                    lexed.toks[n.name.start as usize].text(src).to_string(),
                    12,
                    n.span,
                ),

                Stmt::Local(n) => match n.names.as_slice() {
                    [binding] => (
                        lexed.toks[binding.name.start as usize]
                            .text(src)
                            .to_string(),
                        13,
                        n.span,
                    ),

                    _ => continue,
                },

                _ => continue,
            };

            let range = lines.range(src, bytes(span));

            out.push(json!({
                "name": name,
                "kind": kind,
                "range": range,
                "selectionRange": range,
            }));
        }

        json!(out)
    }
}
#[cfg(test)]
mod dialect_hovers {
    use super::{dialect_label, dialect_target};

    fn at(src: &str, what: &str) -> u32 {
        src.find(what).expect("the probe text is present") as u32
    }

    /// A hover on the keyword answers at the name it binds.
    #[test]
    fn keywords_point_at_their_binding() {
        let src = "export local test = 3\n";

        assert_eq!(
            dialect_target(src, at(src, "export"), true),
            Some(at(src, "test"))
        );
        assert_eq!(
            dialect_target(src, at(src, "local"), true),
            Some(at(src, "test"))
        );

        let src = "const t = 3\n";
        assert_eq!(
            dialect_target(src, at(src, "const"), true),
            Some(at(src, "t ="))
        );

        let src = "local function f() end\n";
        assert_eq!(dialect_target(src, 0, true), Some(at(src, "f()")));

        let src = "export function go() end\n";
        assert_eq!(dialect_target(src, 0, true), Some(at(src, "go")));
    }

    /*
    With value exports off, `export` stays what the analyzer says it is,
    which is an error. The flag is the author's choice, and the hover
    does not paper over it.
    */
    #[test]
    fn export_stays_alone_when_the_flag_is_off() {
        let src = "export local test = 3\n";

        assert_eq!(dialect_target(src, at(src, "export"), false), None);
    }

    /// `export type` is stock Luau, and the analyzer already answers it.
    #[test]
    fn export_type_is_not_redirected() {
        let src = "export type T = number\n";

        assert_eq!(dialect_target(src, 0, true), None);
    }

    /// The card reads `const` where the author wrote `const`.
    #[test]
    fn a_const_card_says_const() {
        let src = "const t = 3\nprint(t)\n";

        assert_eq!(
            dialect_label(src, at(src, "t = 3"), "local t: number").as_deref(),
            Some("const t: number")
        );

        // At the use site too: the declaration above the hover decides.
        assert_eq!(
            dialect_label(src, at(src, "t)"), "local t: number").as_deref(),
            Some("const t: number")
        );
    }

    /// The exported spellings come back whole.
    #[test]
    fn exported_cards_carry_the_export() {
        let src = "export local test = 3\n";
        assert_eq!(
            dialect_label(src, at(src, "test"), "local test: number").as_deref(),
            Some("export local test: number")
        );

        let src = "export const x = 1\n";
        assert_eq!(
            dialect_label(src, at(src, "x"), "local x: number").as_deref(),
            Some("export const x: number")
        );

        let src = "export function go() end\n";
        assert_eq!(
            dialect_label(src, at(src, "go"), "function go(): ()").as_deref(),
            Some("export function go(): ()")
        );
    }

    /// A shadowing plain `local` keeps its plain card.
    #[test]
    fn a_shadow_keeps_its_own_card() {
        let src = "const x = 1\nlocal function f()\n\tlocal x = 2\n\treturn x\nend\n";

        assert_eq!(
            dialect_label(src, at(src, "x = 2"), "local x: number"),
            None
        );
        assert_eq!(
            dialect_label(src, at(src, "x = 1"), "local x: number").as_deref(),
            Some("const x: number")
        );
    }

    /// A plain local needs no rewrite at all.
    #[test]
    fn a_plain_local_stays() {
        assert_eq!(dialect_label("local y = 1\n", 6, "local y: number"), None);
    }
}
