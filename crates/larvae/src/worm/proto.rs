/*!
The format and lint payloads that cross a worm boundary, on every transport.

A worm that formats does not return text. It returns a layout document, and
larvae renders the document with the [`FmtConfig`] of the project. Thus a
claimed file obeys `column_width` and the related settings by construction,
and no worm reimplements the printer. The `host` spans of the document mark
ordinary Luau that the worm does not parse. Larvae formats those spans itself
and splices the result in. This is how a `.luaux` file and a `.luau` file come
out in one style.

A worm that lints returns findings without a severity. The host stamps the
levels from `[lint.rules]`, applies `-- larvae: allow(...)`, renders, and owns
the exit codes. Thus a worm cannot decide that a build fails.

The shapes here are the contract. The guest side of each transport mirrors
them by hand, in the same way as [`crate::worm::host`] mirrors the `abi` of
`larvae-worm`. The tests at the bottom pin the exact JSON. The tests stand in
for a shared crate.
*/

use std::borrow::Cow;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::fmt::{FmtConfig, doc, doc::Doc};

/// The layout contract revision. The host sends it to a worm at init and
/// requires it back on every format reply.
pub const DOC_VERSION: u32 = 1;

/*
One piece of layout, as it crosses the wire.

The shape mirrors [`Doc`] with two substitutions. These keep text cheap and
explicit. Source text crosses as a `src` span and not as a copy. `lit` is
reserved for text the worm generated, so a reply that rewrites the bytes of
the author states this. `host` is the variant that makes the design work as
one system. See the module docs.
*/
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WireDoc {
    /// No output at all
    Nil,
    /// An exact slice of the source, `{"src": [start, end]}`
    Src(u32, u32),
    /// Text that the worm generated, `{"lit": "<Frame>"}`
    Lit(String),
    /// A space when flat, a newline when broken
    Line,
    /// Nothing when flat, a newline when broken
    Soft,
    /// A newline in both modes
    Hard,
    /// A blank line that the author wrote
    Blank,
    /// `{"if_break": [flat, broken]}`
    IfBreak(Box<WireDoc>, Box<WireDoc>),
    /// Flat when it fits, broken when it does not fit
    Group(Box<WireDoc>),
    /// One more level of indentation for the content inside
    Indent(Box<WireDoc>),
    /// The parts, in order
    Concat(Vec<WireDoc>),
    /*
    A span of ordinary Luau. The host formats it.

    `parse` is explicit for a reason. A hole or attribute value is an
    expression, while the run between markup regions is statements. A guess
    about the intent of the worm would turn a worm bug into an unclear parse
    error later.
    */
    Host {
        start: u32,
        end: u32,
        #[serde(default)]
        parse: HostParse,
    },
}

/// The parse mode of a `host` span
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostParse {
    /// Statements, the whole-file shape
    #[default]
    Block,
    /// One expression, a `{expr}` hole or attribute value
    Expr,
}

/// A format reply, assembled by the transport
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FormatReply {
    /// The [`DOC_VERSION`] the worm speaks. The host refuses a mismatch.
    pub doc: u32,
    /// The layout for the whole file. A worm that sends `spans` instead
    /// leaves this empty.
    #[serde(default)]
    pub document: Option<WireDoc>,
    /*
    The regions of ordinary Luau, for a worm that lays out nothing itself.

    This is the least a worm can do and still format. The worm names the byte
    ranges that hold Luau, and larvae builds the document: it formats each
    range and keeps every byte between the ranges as the author wrote it. Thus
    a worm inherits the formatter of larvae for the Luau in its files, and its
    own syntax is untouched.

    `document` wins when a worm sends both.
    */
    #[serde(default)]
    pub spans: Vec<(u32, u32)>,
    /// The span of every comment, for the survival backstop
    #[serde(default)]
    pub comments: Vec<(u32, u32)>,
}

/// One problem a worm found. The severity is absent by intent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireFinding {
    /// The byte range in the source
    pub span: (u32, u32),
    /// The name of the lint. The worm must declare it in `[lints]`.
    pub lint: String,
    pub message: String,
    #[serde(default)]
    pub help: Option<String>,
}

/// A lint reply, assembled by the transport
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LintReply {
    #[serde(default)]
    pub findings: Vec<WireFinding>,
    /// The comment spans, so `-- larvae: allow(...)` works in a claimed file.
    /// A worm that omits them removes its findings from suppression.
    #[serde(default)]
    pub comments: Vec<(u32, u32)>,
    /*
    The Luau shadow of the file, for the lints of larvae to read.

    The shadow is the source with every region that is not Luau replaced by
    filler of the same byte length. Thus each offset in the shadow is the same
    offset in the source, and larvae maps no spans at all. The shadow must
    parse as Luau, because a lint reads a tree.

    A worm that sets `inherit_lints` and omits this field gets the output of
    its own `transform` instead. That output maps by line and not by byte, so
    the columns are less exact.
    */
    #[serde(default)]
    pub luau: Option<String>,
}

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
The mark that stands for one count boundary in the rendered text.

The private use area holds no character that Luau or a markup dialect writes,
so a mark cannot collide with the content of the file. One character keeps the
mark out of the width the renderer measures, so a line that a mark sits on
breaks where it would break without one.
*/
fn mark(index: usize) -> String {
    char::from_u32(0xE000 + index as u32)
        .expect("the private use area holds every index larvae plants")
        .to_string()
}

/*
The closing position of every region that no `on` marker closes.

A region that runs to the end of the file is left out, because it closes at
the end of the output and needs no mark.
*/
fn counts_of(src: &str, ignored: &[(u32, u32)], comments: &[(u32, u32)]) -> Vec<u32> {
    ignored
        .iter()
        .filter(|&&(lo, hi)| hi < src.len() as u32 && !closed(src, lo, hi, comments))
        .map(|&(_, hi)| hi)
        .collect()
}

/// Reports if an `on` marker is the last comment of this region.
fn closed(src: &str, lo: u32, hi: u32, comments: &[(u32, u32)]) -> bool {
    let last = comments
        .iter()
        .filter(|&&(start, _)| start >= lo && start < hi)
        .max_by_key(|&&(start, _)| start);

    let Some(&(start, end)) = last else {
        return false;
    };

    matches!(
        crate::flags::switch(&src[start as usize..end as usize]),
        Some((crate::flags::Subject::Fmt, crate::flags::Switch::On))
    )
}

/*
Plants a mark in the document where each boundary opens.

A `src` node often reaches back over the newline that ends the line before
it, so the boundary sits inside the node and not at its start. Such a node is
split at the boundary and the mark goes between the halves, which puts it at
the head of the line in the rendered text. A boundary at or before the start
of a node needs no split, and the mark goes in front of the node.

A `host` node is parsed Luau, so it is never split. A boundary inside one
cannot be placed.

None means larvae could not place every mark, and then it plants none. A worm
is free to build a document that this walk does not follow, and a splice on
marks that are not all there would cut the wrong text.
*/
fn plant(document: &WireDoc, bounds: &[u32]) -> Option<WireDoc> {
    if bounds.is_empty() {
        return None;
    }

    let mut next = 0;
    let marked = walk(document, bounds, &mut next);

    match next == bounds.len() {
        true => Some(marked),

        false => None,
    }
}

fn walk(node: &WireDoc, bounds: &[u32], next: &mut usize) -> WireDoc {
    match node {
        WireDoc::Concat(parts) => {
            WireDoc::Concat(parts.iter().map(|p| walk(p, bounds, next)).collect())
        }

        WireDoc::Group(inner) => WireDoc::Group(Box::new(walk(inner, bounds, next))),

        WireDoc::Indent(inner) => WireDoc::Indent(Box::new(walk(inner, bounds, next))),

        WireDoc::Src(start, end) => split(*start, *end, node, bounds, next),

        WireDoc::Host { start, .. } => ahead(*start, node, bounds, next),

        /*
        An `if_break` holds two arms, and the render keeps one of them. A
        walk into both would plant each mark two times, and a walk into one
        would plant it where the render may not keep it. So neither arm is
        followed, and a boundary inside one drops the whole splice.
        */
        other => other.clone(),
    }
}

/// Cuts a source node at each boundary inside it, and puts the mark at the cut.
fn split(start: u32, end: u32, node: &WireDoc, bounds: &[u32], next: &mut usize) -> WireDoc {
    if *next >= bounds.len() || bounds[*next] >= end {
        return ahead(start, node, bounds, next);
    }

    let mut parts = Vec::new();
    let mut at = start;

    while *next < bounds.len() && bounds[*next] < end {
        let cut = bounds[*next].max(start);

        if cut > at {
            parts.push(WireDoc::Src(at, cut));
        }

        parts.push(WireDoc::Lit(mark(*next)));

        at = cut;
        *next += 1;
    }

    if at < end {
        parts.push(WireDoc::Src(at, end));
    }

    WireDoc::Concat(parts)
}

/// Puts the marks that this node's position has reached in front of it.
fn ahead(start: u32, node: &WireDoc, bounds: &[u32], next: &mut usize) -> WireDoc {
    if *next >= bounds.len() || bounds[*next] > start {
        return node.clone();
    }

    let mut parts = Vec::new();

    while *next < bounds.len() && bounds[*next] <= start {
        parts.push(WireDoc::Lit(mark(*next)));
        *next += 1;
    }

    parts.push(node.clone());

    WireDoc::Concat(parts)
}

/*
Puts the held regions of a claimed file back as the author wrote them.

A worm owns the layout of the file it claims, so larvae cannot hold the
formatter out of one region the way it does in a Luau file. It renders what
the worm sent and writes the source over each region afterwards.

Each boundary is found by the means that suits it. A region opens at a marker
comment, and every comment of a worm reaches the output, so the text of the
marker locates the start. An `on` marker closes most regions and locates the
end the same way. A count closes at a source position, and a mark planted in
the document answers for that one. A region with no `on` and no count runs to
the end of the file.

The search for a comment runs forward and never looks back, so a marker text
that appears more than one time still pairs with the right occurrence.
*/
fn hold_regions(src: &str, out: String, ignored: &[(u32, u32)], comments: &[(u32, u32)]) -> String {
    let mut sorted = comments.to_vec();
    sorted.sort_unstable();

    let mut result = String::with_capacity(out.len());
    let mut taken = 0;
    let mut scanned = 0;
    let mut planted = 0;

    for &(lo, hi) in ignored {
        let inside: Vec<(u32, u32)> = sorted
            .iter()
            .copied()
            .filter(|&(start, _)| start >= lo && start < hi)
            .collect();

        // A region always opens at a marker, so an empty list is not one.
        let (Some(&open), Some(&close)) = (inside.first(), inside.last()) else {
            continue;
        };

        let Some(at) = seek(&out, src, open, scanned.max(taken)) else {
            continue;
        };

        let from = line_start(&out, at);

        let to = if hi >= src.len() as u32 {
            out.len()
        } else if closed(src, lo, hi, comments) {
            match seek(&out, src, close, at) {
                Some(end) => line_end(&out, end),

                None => continue,
            }
        } else {
            let found = find(&out, planted, at);
            planted += 1;

            match found {
                Some(end) => line_start(&out, end),

                None => continue,
            }
        };

        scanned = to;

        if from < taken || to < from {
            continue;
        }

        result.push_str(&out[taken..from]);
        result.push_str(src[lo as usize..hi as usize].trim_end_matches('\n'));
        result.push('\n');

        taken = to;
    }

    result.push_str(&out[taken..]);

    // A mark that no region used must not reach the file.
    result.retain(|c| !('\u{E000}'..'\u{F000}').contains(&c));

    result
}

/// Where the text of this comment landed in the rendered output, at or after `from`.
fn seek(out: &str, src: &str, comment: (u32, u32), from: usize) -> Option<usize> {
    let text = src[comment.0 as usize..comment.1 as usize].trim_end();

    out.get(from..)
        .and_then(|rest| rest.find(text))
        .map(|at| from + at)
}

/// Where this mark landed in the rendered output, at or after `from`.
fn find(out: &str, index: usize, from: usize) -> Option<usize> {
    out.get(from..)
        .and_then(|rest| rest.find(&mark(index)))
        .map(|at| from + at)
}

fn line_start(s: &str, at: usize) -> usize {
    s[..at].rfind('\n').map_or(0, |n| n + 1)
}

/// The byte just past the newline that ends this line, or the end of the text.
fn line_end(s: &str, at: usize) -> usize {
    s[at..].find('\n').map_or(s.len(), |n| at + n + 1)
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

/*
One rule of a worm, with the nodes that its filter matched, for the batched
rules protocol.

The batch crosses once per file and never once per node. A pipe crossing
costs 24 µs, and a rule worm visits about 120 nodes per file. Thus a per node
protocol costs about 3 ms per file, and one batched crossing costs 24 µs. The
worm receives the whole source and each node as an id, a kind name, and a
byte span. It navigates no tree, and it returns whole span replacements.
*/
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RuleCall {
    /// The rule name, as `[rules]` in `worm.toml` declares it
    pub name: String,
    /// The matched nodes, in pre-order
    pub nodes: Vec<WireNode>,
}

/// One matched node, as it crosses in a [`RuleCall`]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireNode {
    /// The pre-order index of the node in its file
    pub id: u32,
    /// The kind name, the same text as `filter` in `worm.toml`
    pub kind: String,
    /// The byte range in the source, as a half open range
    pub span: (u32, u32),
}

/// The reply to a batched rules request
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RulesReply {
    /// Whole span replacements against the original source: start byte, end
    /// byte, and the new text
    #[serde(default)]
    pub edits: Vec<(u32, u32, String)>,
}

/// A validated span, because every span here came off a wire
fn slice(src: &str, start: u32, end: u32) -> Result<&str> {
    let (s, e) = (start as usize, end as usize);

    if s > e || e > src.len() || !src.is_char_boundary(s) || !src.is_char_boundary(e) {
        bail!("the span {start}..{end} does not lie on the source");
    }

    Ok(&src[s..e])
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

#[cfg(test)]
mod flags_in_a_claimed_file {
    use super::*;

    fn comment_at(src: &str, text: &str) -> (u32, u32) {
        let at = src.find(text).expect("the marker is in the source") as u32;

        (at, at + text.len() as u32)
    }

    fn render(src: &str, spans: Vec<(u32, u32)>, comments: Vec<(u32, u32)>) -> String {
        let reply = FormatReply {
            doc: DOC_VERSION,
            document: None,
            spans,
            comments,
        };

        render_format(src, &reply, &FmtConfig::default()).unwrap()
    }

    /*
    A marker inside a claimed file reads as it does in a Luau file.

    Larvae does not read a claimed file as Luau, so it finds no comment in one
    itself. The worm reports where the comments are, and the flags come from
    that list.
    */
    #[test]
    fn a_region_inside_a_claimed_file_is_untouched() {
        let src = "<F>\n-- larvae: fmt off\nlocal  m = {1,0}\n-- larvae: fmt on\n</F>\n";
        let off = comment_at(src, "-- larvae: fmt off");
        let on = comment_at(src, "-- larvae: fmt on");
        let end = src.find("\n</F>").unwrap() as u32;

        let out = render(src, vec![(off.0, end)], vec![off, on]);

        assert!(out.contains("local  m = {1,0}"), "{out}");
    }

    #[test]
    fn a_claimed_file_held_off_in_full_comes_back_unchanged() {
        let src = "<F>\n-- larvae: fmt off\nlocal  m = {1,0}\n</F>\n";
        let off = comment_at(src, "-- larvae: fmt off");
        let end = src.find("\n</F>").unwrap() as u32;

        assert_eq!(render(src, vec![(off.0, end)], vec![off]), src);
    }

    /// The Luau outside the region still gets laid out.
    #[test]
    fn only_the_held_lines_are_left_alone() {
        let src = "<F>\nlocal  a  = 1\n-- larvae: fmt off\nlocal  m = {1,0}\n-- larvae: fmt on\nlocal  b  = 2\n</F>\n";
        let off = comment_at(src, "-- larvae: fmt off");
        let on = comment_at(src, "-- larvae: fmt on");
        let start = src.find("local  a").unwrap() as u32;
        let end = src.find("\n</F>").unwrap() as u32;

        let out = render(src, vec![(start, end)], vec![off, on]);

        assert!(out.contains("local a = 1"), "outside the region: {out}");
        assert!(out.contains("local b = 2"), "outside the region: {out}");
        assert!(out.contains("local  m = {1,0}"), "inside it: {out}");
    }

    #[test]
    fn a_count_holds_that_many_lines_in_a_claimed_file() {
        let src = "<F>\n-- larvae: fmt off(1)\nlocal  m = {1,0}\nlocal  n  = 2\n</F>\n";
        let off = comment_at(src, "-- larvae: fmt off(1)");
        let start = off.0;
        let end = src.find("\n</F>").unwrap() as u32;

        let out = render(src, vec![(start, end)], vec![off]);

        assert!(out.contains("local  m = {1,0}"), "held: {out}");
        assert!(out.contains("local n = 2"), "not held: {out}");
    }

    /*
    A worm that owns the layout of the file must still honour a region.

    The worm sends a whole document for a file it claims, and the document
    carries text of its own as `lit`, which has no source span. So larvae
    cannot hold the formatter out of a range of that document. It renders
    what the worm sent and then puts the source back over each held region.

    The document here does what a markup worm does with the attributes of an
    element: it rewrites them onto one line of its own choosing.
    */
    #[test]
    fn a_region_holds_a_worm_that_sends_a_whole_document() {
        let src = concat!(
            "-- larvae: fmt off\n",
            "local root = <ScreenGui Name=\"ROOT\" Parent={ Gui } ResetOnSpawn={ false }>\n",
            "  <TextLabel>test</TextLabel>\n",
            "</ScreenGui>\n",
            "-- larvae: fmt on\n",
            "return root\n",
        );

        let off = comment_at(src, "-- larvae: fmt off");
        let on = comment_at(src, "-- larvae: fmt on");

        // what a markup worm sends: one attribute per line, its own text
        let document = WireDoc::Concat(vec![
            WireDoc::Src(off.0, off.1),
            WireDoc::Hard,
            WireDoc::Lit("local root = <ScreenGui".to_string()),
            WireDoc::Hard,
            WireDoc::Lit("\tName=\"ROOT\"".to_string()),
            WireDoc::Hard,
            WireDoc::Lit("\tParent={ Gui }".to_string()),
            WireDoc::Hard,
            WireDoc::Lit("\tResetOnSpawn={ false }".to_string()),
            WireDoc::Hard,
            WireDoc::Lit(">".to_string()),
            WireDoc::Hard,
            WireDoc::Lit("\t<TextLabel>test</TextLabel>".to_string()),
            WireDoc::Hard,
            WireDoc::Lit("</ScreenGui>".to_string()),
            WireDoc::Hard,
            WireDoc::Src(on.0, on.1),
            WireDoc::Hard,
            WireDoc::Lit("return root".to_string()),
        ]);

        let reply = FormatReply {
            doc: DOC_VERSION,
            document: Some(document),
            spans: Vec::new(),
            comments: vec![off, on],
        };

        let out = render_format(src, &reply, &FmtConfig::default()).unwrap();

        assert!(
            out.contains(
                "local root = <ScreenGui Name=\"ROOT\" Parent={ Gui } ResetOnSpawn={ false }>"
            ),
            "the attributes moved inside the region: {out}"
        );
        assert!(
            out.contains("  <TextLabel>test</TextLabel>"),
            "the indentation of the region moved: {out}"
        );

        // outside the region the worm still owns the layout
        assert!(out.contains("return root"), "{out}");
    }

    /// Everything the worm wrote outside a region survives the splice.
    #[test]
    fn a_region_does_not_eat_the_lines_around_it() {
        let src = concat!(
            "local  a  = 1\n",
            "-- larvae: fmt off\n",
            "local  m = {1,0}\n",
            "-- larvae: fmt on\n",
            "local  b  = 2\n",
        );

        let off = comment_at(src, "-- larvae: fmt off");
        let on = comment_at(src, "-- larvae: fmt on");

        let document = WireDoc::Concat(vec![
            WireDoc::Lit("local a = 1".to_string()),
            WireDoc::Hard,
            WireDoc::Src(off.0, off.1),
            WireDoc::Hard,
            WireDoc::Lit("local m = { 1, 0 }".to_string()),
            WireDoc::Hard,
            WireDoc::Src(on.0, on.1),
            WireDoc::Hard,
            WireDoc::Lit("local b = 2".to_string()),
        ]);

        let reply = FormatReply {
            doc: DOC_VERSION,
            document: Some(document),
            spans: Vec::new(),
            comments: vec![off, on],
        };

        let out = render_format(src, &reply, &FmtConfig::default()).unwrap();

        assert_eq!(
            out,
            "local a = 1\n-- larvae: fmt off\nlocal  m = {1,0}\n-- larvae: fmt on\nlocal b = 2\n",
            "{out}"
        );
    }

    /*
    The count form works in a claimed file too.

    No `on` marker closes such a region, so the end of it is a source
    position and not a comment. The marks answer for it in the same way as
    they answer for a pair of markers.
    */
    #[test]
    fn a_count_holds_that_many_lines_of_a_whole_document() {
        let src = concat!(
            "local  a  = 1\n",
            "-- larvae: fmt off(2)\n",
            "local  m = {1,0}\n",
            "local  n = {2,3}\n",
            "local  b  = 2\n",
        );

        let off = comment_at(src, "-- larvae: fmt off(2)");

        let document = WireDoc::Concat(vec![
            WireDoc::Lit("local a = 1".to_string()),
            WireDoc::Hard,
            WireDoc::Src(off.0, off.1),
            WireDoc::Hard,
            WireDoc::Lit("local m = { 1, 0 }".to_string()),
            WireDoc::Hard,
            WireDoc::Lit("local n = { 2, 3 }".to_string()),
            WireDoc::Hard,
            WireDoc::Src(
                src.find("local  b").unwrap() as u32,
                src.find("  = 2").unwrap() as u32 + 5,
            ),
        ]);

        let reply = FormatReply {
            doc: DOC_VERSION,
            document: Some(document),
            spans: Vec::new(),
            comments: vec![off],
        };

        let out = render_format(src, &reply, &FmtConfig::default()).unwrap();

        assert_eq!(
            out,
            "local a = 1\n-- larvae: fmt off(2)\nlocal  m = {1,0}\nlocal  n = {2,3}\nlocal  b  = 2\n",
            "{out}"
        );
    }

    /// No mark may reach the file, whatever the document did.
    #[test]
    fn no_mark_survives_into_the_output() {
        let src = "-- larvae: fmt off\nlocal  m = {1,0}\n-- larvae: fmt on\nreturn m\n";
        let off = comment_at(src, "-- larvae: fmt off");
        let on = comment_at(src, "-- larvae: fmt on");

        let document = WireDoc::Concat(vec![
            WireDoc::Src(off.0, off.1),
            WireDoc::Hard,
            WireDoc::Lit("local m = { 1, 0 }".to_string()),
            WireDoc::Hard,
            WireDoc::Src(on.0, on.1),
            WireDoc::Hard,
            WireDoc::Lit("return m".to_string()),
        ]);

        let reply = FormatReply {
            doc: DOC_VERSION,
            document: Some(document),
            spans: Vec::new(),
            comments: vec![off, on],
        };

        let out = render_format(src, &reply, &FmtConfig::default()).unwrap();

        assert!(
            !out.chars().any(|c| ('\u{E000}'..'\u{F000}').contains(&c)),
            "a mark reached the file: {out:?}"
        );
        assert!(out.contains("local  m = {1,0}"), "{out}");
    }

    /*
    A `src` node that reaches back over a newline is cut at the boundary.

    A worm that lays out markup takes the Luau run before an element as one
    span, and the span starts at the newline that ends the line above. So the
    close of a count region sits inside the node and not at its start. Larvae
    cuts the node there. Without the cut the mark lands further down the
    line, and the splice takes the head of the line away with the region.

    This is the shape the luaux worm sends, and it is the case that found
    this defect.
    */
    #[test]
    fn a_source_node_that_holds_the_boundary_is_cut_at_it() {
        let src = concat!(
            "const a = 1\n",
            "-- larvae: fmt off(1)\n",
            "const root = <F A=\"1\"/>\n",
            "const b = <F B=\"2\"/>\n",
        );

        let off = comment_at(src, "-- larvae: fmt off(1)");
        let held = src.find("const root").unwrap() as u32;
        let after = src.find("\nconst b").unwrap() as u32;

        let document = WireDoc::Concat(vec![
            WireDoc::Src(0, off.0),
            WireDoc::Src(off.0, held),
            WireDoc::Lit("const root = <F".to_string()),
            WireDoc::Indent(Box::new(WireDoc::Concat(vec![
                WireDoc::Hard,
                WireDoc::Lit("A=\"1\"".to_string()),
            ]))),
            WireDoc::Hard,
            WireDoc::Lit("/>".to_string()),
            // the Luau run before the next element, opening on the newline
            WireDoc::Src(after, after + "\nconst b = ".len() as u32),
            WireDoc::Lit("<F".to_string()),
            WireDoc::Indent(Box::new(WireDoc::Concat(vec![
                WireDoc::Hard,
                WireDoc::Lit("B=\"2\"".to_string()),
            ]))),
            WireDoc::Hard,
            WireDoc::Lit("/>".to_string()),
        ]);

        let reply = FormatReply {
            doc: DOC_VERSION,
            document: Some(document),
            spans: Vec::new(),
            comments: vec![off],
        };

        let out = render_format(src, &reply, &FmtConfig::default()).unwrap();

        assert!(
            out.contains("const root = <F A=\"1\"/>"),
            "the region is held on one line: {out}"
        );
        assert!(
            out.contains("const b = <F"),
            "the head of the line past the region survives: {out}"
        );
        assert!(
            out.contains("\tB=\"2\""),
            "the worm still lays out past the region: {out}"
        );
    }

    /// A worm that reports no comment gets the formatter, as before.
    #[test]
    fn a_reply_with_no_comments_formats_as_usual() {
        let src = "<F>\nlocal  m = {1,0}\n</F>\n";
        let start = src.find("local").unwrap() as u32;
        let end = src.find("\n</F>").unwrap() as u32;

        assert!(render(src, vec![(start, end)], Vec::new()).contains("local m = { 1, 0 }"));
    }
}
