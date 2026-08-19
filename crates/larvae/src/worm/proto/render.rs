/*!
Turning the layout a worm sent into text, in the style of the project.

A worm that formats returns a document and not text, so larvae renders it with
the `[fmt]` of the project and no worm reimplements the printer. The `host`
spans of that document mark ordinary Luau that the worm did not parse, and
larvae formats those itself.

The other half of this file holds the region markers. A file a worm claims
obeys `-- larvae: fmt off` the same as a Luau file does, and the worm knows
nothing about it, so the host writes the source back over each held region
after the render. See [`hold_regions`].
*/

use std::borrow::Cow;

use anyhow::{Context, Result, bail};

use super::regions::{counts_of, hold_regions, plant};
use super::*;
use crate::fmt::{FmtConfig, doc, doc::Doc};

/*
The start of the Luau after a `;` that opens the span, when one does.

Only spaces and tabs may sit between the `;` and the Luau. The result never
goes below `floor`, which is the end of the span before this one.
*/
fn semicolon_before(src: &str, start: u32, floor: u32) -> Option<u32> {
    let bytes = src.as_bytes();

    if start < floor || bytes.get(start as usize) != Some(&b';') {
        return None;
    }

    let mut i = start as usize + 1;

    while matches!(bytes.get(i), Some(b' ' | b'\t')) {
        i += 1;
    }

    Some(i as u32)
}

/*
The end of a `;` that follows `end`, when one does before `limit`.

Only spaces and tabs may sit between. A newline ends the search, because a `;`
on the next line belongs to whatever the worm put there and not to the span
that came before.
*/
fn semicolon_after(src: &str, end: u32, limit: u32) -> Option<u32> {
    let bytes = src.as_bytes();
    let mut i = end as usize;

    while i < limit as usize && matches!(bytes.get(i), Some(b' ' | b'\t')) {
        i += 1;
    }

    match bytes.get(i) {
        Some(b';') if (i as u32) < limit => Some(i as u32 + 1),

        _ => None,
    }
}

/// Render the format reply of a worm with the style of the project
pub fn render_format(src: &str, reply: &FormatReply, cfg: &FmtConfig) -> Result<String> {
    if reply.doc != DOC_VERSION {
        bail!(
            "the reply speaks doc v{}, this larvae speaks v{DOC_VERSION}",
            reply.doc
        );
    }

    for &(start, end) in &reply.comments {
        slice(src, start, end).context("in a comment span")?;
    }

    /*
    A file that a worm claims can hold the formatter off in full.

    The comments come from the reply, because larvae does not read a claimed
    file as Luau and so finds no comment in it itself. The worm reports where
    they are, and the markers read the same as they do in a Luau file.
    */
    let ignored = crate::flags::off_ranges(src, &reply.comments, crate::flags::Subject::Fmt);

    if ignored
        .iter()
        .any(|&(a, b)| a == 0 && b >= src.len() as u32)
    {
        return Ok(src.to_string());
    }

    /*
    A count region is marked in the document before the render.

    Each held region opens at a marker comment, and the text of that comment
    reaches the output, so a search finds where the region starts. An `on`
    marker closes most regions, and the same search finds the end. A count,
    `off(4)`, closes at a source position that no comment names, so that one
    boundary is marked in the document instead, beside a node that carries a
    source span.
    */
    let counts = counts_of(src, &ignored, &reply.comments);

    let planted = match &reply.document {
        Some(document) if !ignored.is_empty() => plant(document, &counts),

        _ => None,
    };

    /*
    The splice covers the Luau of a region as well as the markup.

    So the pass inside a `host` span stands down where the splice runs. With
    both on, the region would be written back one time by each of them.
    */
    let splices = reply.document.is_some() && !ignored.is_empty();
    let holds = !splices;

    let document = match (&reply.document, &planted) {
        (_, Some(marked)) => convert(marked, src, cfg, holds)?,

        (Some(document), None) => convert(document, src, cfg, holds)?,

        (None, None) => from_spans(src, &reply.spans, cfg)?,
    };

    // the host owns the file-final newline, the same as it does for Luau,
    // so no worm encodes it and every file ends in the same way
    let document = Doc::concat([document, Doc::Hard]);
    let out = doc::render(&document, cfg.style());

    let out = match splices {
        true => hold_regions(src, out, &ignored, &reply.comments),

        false => out,
    };

    crate::fmt::check_comments_survived(src, &reply.comments, &out)?;

    Ok(out)
}

/*
Build the document of a worm that named its Luau regions and nothing else.

Larvae formats each region and keeps every byte between two regions exactly as
the author wrote it. The regions are sorted first, because a worm can find
them in any order, and they must not overlap.

The final newline of the file is dropped here, because [`render_format`] adds
one for every file.
*/
fn from_spans<'a>(src: &'a str, spans: &[(u32, u32)], cfg: &FmtConfig) -> Result<Doc<'a>> {
    if spans.is_empty() {
        bail!("the reply carries neither a document nor any Luau span");
    }

    let mut sorted = spans.to_vec();
    sorted.sort_unstable();

    /*
    Take a `;` that sits just after a span into that span.

    A `;` that follows Luau is Luau: it terminates the statement before it. But
    a worm draws its spans from its own parse, and a worm that ends a span at
    the last token of a statement leaves the terminator outside. Larvae then
    keeps that byte as written, and `semicolons` appears to work on some
    statements of the file and not others.

    Larvae takes the byte instead. The alternative is a contract that needs
    byte exact boundaries from every worm, and one wrong boundary gives output
    in two styles.
    */
    for i in 0..sorted.len() {
        let limit = sorted.get(i + 1).map_or(src.len() as u32, |next| next.0);
        let floor = match i {
            0 => 0,

            _ => sorted[i - 1].1,
        };

        let (start, end) = sorted[i];

        /*
        Leave a `;` that opens a span outside it.

        Larvae formats each span as a chunk of its own, so it cannot see the
        text before the span. A leading `;` reads as a stray statement there
        and goes, but for the line above it is the separator that stops
        `(b)()` from continuing that line as a call. Larvae keeps the byte as
        written instead, and the meaning holds.
        */
        if let Some(shrunk) = semicolon_before(src, start, floor) {
            sorted[i].0 = shrunk;
        }

        if let Some(grown) = semicolon_after(src, end, limit) {
            sorted[i].1 = grown;
        }
    }

    let mut parts = Vec::new();
    let mut at = 0u32;

    for (start, end) in sorted {
        if start < at {
            bail!("the Luau spans overlap at byte {start}");
        }

        parts.push(Doc::Text(Cow::Borrowed(slice(src, at, start)?)));
        parts.push(
            crate::fmt::doc_of(slice(src, start, end)?, cfg)
                .with_context(|| format!("in the Luau span {start}..{end}"))?,
        );

        at = end;
    }

    // render_format ends every file with one newline, so the tail gives none
    let tail = slice(src, at, src.len() as u32)?;
    let tail = tail.strip_suffix('\n').unwrap_or(tail);

    parts.push(Doc::Text(Cow::Borrowed(tail)));

    Ok(Doc::concat(parts))
}

/*
Turn the wire document into a real document.

`src` spans borrow the source, and `lit` borrows the reply. Thus the only
copies are for `host` spans, which must live longer than the token buffers
they came from. The depth is bounded before this runs: serde_json refuses more
than 128 levels of nesting, so the recursion here cannot go deeper than that.
*/
fn convert<'a>(wire: &'a WireDoc, src: &'a str, cfg: &FmtConfig, holds: bool) -> Result<Doc<'a>> {
    Ok(match wire {
        WireDoc::Nil => Doc::Nil,

        WireDoc::Src(start, end) => Doc::Text(Cow::Borrowed(slice(src, *start, *end)?)),

        WireDoc::Lit(text) => Doc::Text(Cow::Borrowed(text)),

        WireDoc::Line => Doc::Line,

        WireDoc::Soft => Doc::Soft,

        WireDoc::Hard => Doc::Hard,

        WireDoc::Blank => Doc::Blank,

        WireDoc::IfBreak(flat, broken) => Doc::if_break(
            convert(flat, src, cfg, holds)?,
            convert(broken, src, cfg, holds)?,
        ),

        WireDoc::Group(inner) => Doc::group(convert(inner, src, cfg, holds)?),

        WireDoc::Indent(inner) => Doc::indent(convert(inner, src, cfg, holds)?),

        WireDoc::Concat(parts) => Doc::concat(
            parts
                .iter()
                .map(|part| convert(part, src, cfg, holds))
                .collect::<Result<Vec<_>>>()?,
        ),

        WireDoc::Host { start, end, parse } => {
            let piece = slice(src, *start, *end)?;

            match parse {
                HostParse::Block => crate::fmt::doc_of_holding(piece, cfg, holds),
                HostParse::Expr => crate::fmt::doc_of_expr(piece, cfg),
            }
            .with_context(|| format!("in the host span {start}..{end}"))?
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The JSON here is the contract that every guest mirrors. It changes
    /// only with a DOC_VERSION bump.
    #[test]
    fn the_wire_doc_shape_is_the_documented_one() {
        let doc = WireDoc::Concat(vec![
            WireDoc::Nil,
            WireDoc::Src(0, 4),
            WireDoc::Lit("<".into()),
            WireDoc::Line,
            WireDoc::Soft,
            WireDoc::Hard,
            WireDoc::Blank,
            WireDoc::IfBreak(Box::new(WireDoc::Nil), Box::new(WireDoc::Hard)),
            WireDoc::Group(Box::new(WireDoc::Indent(Box::new(WireDoc::Host {
                start: 8,
                end: 12,
                parse: HostParse::Expr,
            })))),
        ]);

        let json = serde_json::to_string(&doc).unwrap();

        assert_eq!(
            json,
            r#"{"concat":["nil",{"src":[0,4]},{"lit":"<"},"line","soft","hard","blank",{"if_break":["nil","hard"]},{"group":{"indent":{"host":{"start":8,"end":12,"parse":"expr"}}}}]}"#
        );

        assert_eq!(serde_json::from_str::<WireDoc>(&json).unwrap(), doc);
    }

    #[test]
    fn a_host_span_defaults_to_block() {
        let parsed: WireDoc = serde_json::from_str(r#"{"host":{"start":0,"end":3}}"#).unwrap();

        assert_eq!(
            parsed,
            WireDoc::Host {
                start: 0,
                end: 3,
                parse: HostParse::Block
            }
        );
    }

    #[test]
    fn a_finding_crosses_without_a_severity() {
        let json = r#"{"findings":[{"span":[2,7],"lint":"tidy","message":"untidy"}]}"#;
        let reply: LintReply = serde_json::from_str(json).unwrap();

        assert_eq!(reply.findings[0].lint, "tidy");
        assert_eq!(reply.findings[0].help, None);
        assert!(reply.comments.is_empty());
    }

    fn reply(document: WireDoc) -> FormatReply {
        FormatReply {
            doc: DOC_VERSION,
            document: Some(document),
            spans: Vec::new(),
            comments: Vec::new(),
        }
    }

    #[test]
    fn a_reply_speaking_another_doc_version_is_refused() {
        let mut r = reply(WireDoc::Nil);
        r.doc = 2;

        let err = render_format("", &r, &FmtConfig::default()).unwrap_err();

        assert!(format!("{err:#}").contains("doc v2"), "{err:#}");
    }

    #[test]
    fn a_span_off_the_source_is_refused() {
        let r = reply(WireDoc::Src(0, 99));
        let err = render_format("short", &r, &FmtConfig::default()).unwrap_err();

        assert!(format!("{err:#}").contains("0..99"), "{err:#}");
    }

    #[test]
    fn a_span_splitting_a_character_is_refused() {
        // é is two bytes, so 1..2 points inside it
        let r = reply(WireDoc::Src(1, 2));

        assert!(render_format("é", &r, &FmtConfig::default()).is_err());
    }

    #[test]
    fn a_host_expr_span_is_formatted_by_larvae() {
        let src = "x={a+b}";
        let document = WireDoc::Concat(vec![
            WireDoc::Src(0, 2),
            WireDoc::Lit("{".into()),
            WireDoc::Host {
                start: 3,
                end: 6,
                parse: HostParse::Expr,
            },
            WireDoc::Lit("}".into()),
        ]);

        let out = render_format(src, &reply(document), &FmtConfig::default()).unwrap();

        assert_eq!(out, "x={a + b}\n");
    }

    #[test]
    fn a_host_block_span_obeys_the_project_width() {
        let src = "local abc = 1 + 2";
        let document = WireDoc::Host {
            start: 0,
            end: src.len() as u32,
            parse: HostParse::Block,
        };

        let narrow: FmtConfig = toml::from_str("column_width = 10").unwrap();
        let wide = FmtConfig::default();

        let flat = render_format(src, &reply(document.clone()), &wide).unwrap();
        let broken = render_format(src, &reply(document), &narrow).unwrap();

        assert_eq!(flat, "local abc = 1 + 2\n");
        assert_ne!(flat, broken, "a narrow width has to break the line");
    }

    /// The least a worm can do: name the Luau, and larvae lays it out
    #[test]
    fn named_luau_spans_become_a_document() {
        let src = "<Frame>\nlocal  x   =  1\n</Frame>\n";
        let start = src.find("local").unwrap() as u32;
        let end = src.find("\n</Frame>").unwrap() as u32;

        let reply = FormatReply {
            doc: DOC_VERSION,
            document: None,
            spans: vec![(start, end)],
            comments: Vec::new(),
        };

        let out = render_format(src, &reply, &FmtConfig::default()).unwrap();

        // the Luau is formatted, and every other byte is the author's
        assert_eq!(out, "<Frame>\nlocal x = 1\n</Frame>\n");
    }

    #[test]
    fn a_reply_with_no_document_and_no_span_is_refused() {
        let reply = FormatReply {
            doc: DOC_VERSION,
            document: None,
            spans: Vec::new(),
            comments: Vec::new(),
        };

        assert!(render_format("x", &reply, &FmtConfig::default()).is_err());
    }

    #[test]
    fn a_document_dropping_a_comment_is_refused() {
        let src = "-- keep me\nx = 1";
        let mut r = reply(WireDoc::Src(11, 16));
        r.comments = vec![(0, 10)];

        let err = render_format(src, &r, &FmtConfig::default()).unwrap_err();

        assert!(format!("{err:#}").contains("keep me"), "{err:#}");
    }

    #[test]
    fn a_document_keeping_its_comments_passes_the_backstop() {
        let src = "-- keep me\nx = 1";
        let mut r = reply(WireDoc::Concat(vec![
            WireDoc::Src(0, 10),
            WireDoc::Hard,
            WireDoc::Src(11, 16),
        ]));
        r.comments = vec![(0, 10)];

        assert_eq!(
            render_format(src, &r, &FmtConfig::default()).unwrap(),
            "-- keep me\nx = 1\n"
        );
    }

    /// The batched rules payload, pinned byte for byte, because every guest
    /// mirrors this JSON by hand
    #[test]
    fn the_rule_call_shape_is_the_documented_one() {
        let call = RuleCall {
            name: "x".into(),
            nodes: vec![WireNode {
                id: 1,
                kind: "CallExpr".into(),
                span: (4, 20),
            }],
        };

        let json = serde_json::to_string(&call).unwrap();

        assert_eq!(
            json,
            r#"{"name":"x","nodes":[{"id":1,"kind":"CallExpr","span":[4,20]}]}"#
        );
        assert_eq!(serde_json::from_str::<RuleCall>(&json).unwrap(), call);
    }

    #[test]
    fn the_rules_reply_shape_is_the_documented_one() {
        let reply = RulesReply {
            edits: vec![(4, 20, "new".to_owned())],
        };

        let json = serde_json::to_string(&reply).unwrap();

        assert_eq!(json, r#"{"edits":[[4,20,"new"]]}"#);
        assert_eq!(serde_json::from_str::<RulesReply>(&json).unwrap(), reply);
    }

    /// A reply without edits means no change, not a parse failure
    #[test]
    fn a_rules_reply_defaults_to_no_edits() {
        let reply: RulesReply = serde_json::from_str("{}").unwrap();

        assert!(reply.edits.is_empty());
    }
}

#[cfg(test)]
mod semicolon_after_a_span {
    use super::*;

    fn rendered(src: &str, spans: Vec<(u32, u32)>, cfg: &FmtConfig) -> String {
        let reply = FormatReply {
            doc: DOC_VERSION,
            document: None,
            spans,
            comments: Vec::new(),
        };

        render_format(src, &reply, cfg).unwrap()
    }

    /*
    A worm that ends a span at the last token of a statement leaves the `;`
    outside it. Larvae takes that byte, or `semicolons` works on some
    statements of a file and not others.
    */
    #[test]
    fn a_semicolon_just_past_the_span_is_still_dropped() {
        let src = "<F>\nconst T = {\n\ta = 1,\n};\n</F>\n";
        let start = src.find("const").unwrap() as u32;
        let end = src.find(";\n</F>").unwrap() as u32;

        assert_eq!(
            rendered(src, vec![(start, end)], &FmtConfig::default()),
            "<F>\nconst T = {\n\ta = 1,\n}\n</F>\n"
        );
    }

    #[test]
    fn a_span_that_already_holds_the_semicolon_is_unchanged() {
        let src = "<F>\nconst T = {\n\ta = 1,\n};\n</F>\n";
        let start = src.find("const").unwrap() as u32;
        let end = src.find("\n</F>").unwrap() as u32;

        assert_eq!(
            rendered(src, vec![(start, end)], &FmtConfig::default()),
            "<F>\nconst T = {\n\ta = 1,\n}\n</F>\n"
        );
    }

    #[test]
    fn spaces_between_the_span_and_the_semicolon_are_taken_too() {
        let src = "<F>\nlocal x = 1  ;\n</F>\n";
        let start = src.find("local").unwrap() as u32;
        let end = src.find("  ;").unwrap() as u32;

        assert_eq!(
            rendered(src, vec![(start, end)], &FmtConfig::default()),
            "<F>\nlocal x = 1\n</F>\n"
        );
    }

    /// A `;` on the next line belongs to whatever the worm put there
    #[test]
    fn a_semicolon_on_a_later_line_is_left_to_the_worm() {
        let src = "<F>\nlocal x = 1\n;\n</F>\n";
        let start = src.find("local").unwrap() as u32;
        let end = src.find("\n;").unwrap() as u32;

        assert!(rendered(src, vec![(start, end)], &FmtConfig::default()).contains("\n;\n"));
    }

    /// The byte must never be taken from the span that follows
    #[test]
    fn a_semicolon_belonging_to_the_next_span_is_not_taken() {
        let src = "local a = 1\n;(b)()\n";
        let first = (0u32, src.find('\n').unwrap() as u32);
        let second = (first.1 + 1, src.len() as u32 - 1);

        // the second span starts at the `;`, so the first must not reach it
        let out = rendered(src, vec![first, second], &FmtConfig::default());

        assert!(out.contains(";(b)()"), "{out}");
    }

    #[test]
    fn semicolons_always_still_puts_one_back() {
        let cfg = FmtConfig {
            semicolons: crate::fmt::config::Semicolons::Always,
            ..Default::default()
        };

        let src = "<F>\nlocal x = 1;\n</F>\n";
        let start = src.find("local").unwrap() as u32;
        let end = src.find(";\n</F>").unwrap() as u32;

        assert_eq!(
            rendered(src, vec![(start, end)], &cfg),
            "<F>\nlocal x = 1;\n</F>\n"
        );
    }
}
