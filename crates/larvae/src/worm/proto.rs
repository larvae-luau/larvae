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

    let document = match &reply.document {
        Some(document) => convert(document, src, cfg)?,

        None => from_spans(src, &reply.spans, cfg)?,
    };

    // the host owns the file-final newline, the same as it does for Luau,
    // so no worm encodes it and every file ends in the same way
    let document = Doc::concat([document, Doc::Hard]);
    let out = doc::render(&document, cfg.style());

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
fn convert<'a>(wire: &'a WireDoc, src: &'a str, cfg: &FmtConfig) -> Result<Doc<'a>> {
    Ok(match wire {
        WireDoc::Nil => Doc::Nil,

        WireDoc::Src(start, end) => Doc::Text(Cow::Borrowed(slice(src, *start, *end)?)),

        WireDoc::Lit(text) => Doc::Text(Cow::Borrowed(text)),

        WireDoc::Line => Doc::Line,

        WireDoc::Soft => Doc::Soft,

        WireDoc::Hard => Doc::Hard,

        WireDoc::Blank => Doc::Blank,

        WireDoc::IfBreak(flat, broken) => {
            Doc::if_break(convert(flat, src, cfg)?, convert(broken, src, cfg)?)
        }

        WireDoc::Group(inner) => Doc::group(convert(inner, src, cfg)?),

        WireDoc::Indent(inner) => Doc::indent(convert(inner, src, cfg)?),

        WireDoc::Concat(parts) => Doc::concat(
            parts
                .iter()
                .map(|part| convert(part, src, cfg))
                .collect::<Result<Vec<_>>>()?,
        ),

        WireDoc::Host { start, end, parse } => {
            let piece = slice(src, *start, *end)?;

            match parse {
                HostParse::Block => crate::fmt::doc_of(piece, cfg),
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
