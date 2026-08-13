/*!
The format and lint payloads that cross a worm boundary, whatever the
transport.

A worm that formats does not return text. It returns a layout document, and
larvae renders it with the project's own [`FmtConfig`], so a claimed file
obeys `column_width` and friends by construction and no worm reimplements the
printer. The document's `host` spans mark ordinary Luau the worm does not
parse: larvae lays those out itself and splices the result in, which is how a
`.luaux` file and a `.luau` file come out in one style.

A worm that lints returns findings without a severity. The host stamps levels
from `[lint.rules]`, applies `-- larvae: allow(...)`, renders, and owns exit
codes, so a worm cannot decide a build fails.

The shapes here are the contract, mirrored by hand on the guest side of each
transport the way [`crate::worm::host`] mirrors `larvae-worm`'s `abi`. The
tests at the bottom pin the exact JSON, which is what stands in for a shared
crate.
*/

use std::borrow::Cow;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::fmt::{FmtConfig, doc, doc::Doc};

/// The layout contract revision, sent to a worm at init and required back on
/// every format reply
pub const DOC_VERSION: u32 = 1;

/*
One piece of layout, as it crosses the wire.

The shape mirrors [`Doc`] with two substitutions that keep text cheap and
honest: source text crosses as a `src` span rather than a copy, and `lit` is
reserved for text the worm generated, so a reply that rewrites the author's
bytes says so. `host` is the variant that makes the whole design work in
unison, see the module docs.
*/
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WireDoc {
    /// Nothing at all
    Nil,
    /// A verbatim slice of the source, `{"src": [start, end]}`
    Src(u32, u32),
    /// Text the worm generated, `{"lit": "<Frame>"}`
    Lit(String),
    /// A space when flat, a newline when broken
    Line,
    /// Nothing when flat, a newline when broken
    Soft,
    /// A newline either way
    Hard,
    /// A blank line the author wrote
    Blank,
    /// `{"if_break": [flat, broken]}`
    IfBreak(Box<WireDoc>, Box<WireDoc>),
    /// Flat if it fits, broken if it does not
    Group(Box<WireDoc>),
    /// One more level of indentation for anything inside
    Indent(Box<WireDoc>),
    /// In order
    Concat(Vec<WireDoc>),
    /*
    A span of ordinary Luau. Host, format it.

    `parse` is explicit because a hole or attribute value is an expression
    while the run between markup regions is statements, and guessing which a
    worm meant would turn a worm bug into a confusing parse error downstream.
    */
    Host {
        start: u32,
        end: u32,
        #[serde(default)]
        parse: HostParse,
    },
}

/// How a `host` span parses
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
    /// The [`DOC_VERSION`] the worm speaks, refused on mismatch
    pub doc: u32,
    /// The layout for the whole file
    pub document: WireDoc,
    /// Every comment's span, for the survival backstop
    #[serde(default)]
    pub comments: Vec<(u32, u32)>,
}

/// One problem a worm found, severity deliberately absent
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireFinding {
    /// Byte range in the source
    pub span: (u32, u32),
    /// The lint's name, which must be declared in the worm's `[lints]`
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
    /// Comment spans, so `-- larvae: allow(...)` works in a claimed file.
    /// A worm that omits them opts its findings out of suppression.
    #[serde(default)]
    pub comments: Vec<(u32, u32)>,
}

/// Render a worm's format reply against the project's own style
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

    let document = convert(&reply.document, src, cfg)?;

    // the host owns the file-final newline, exactly as it does for Luau,
    // so no worm encodes it and every file ends the same way
    let document = Doc::concat([document, Doc::Hard]);
    let out = doc::render(&document, cfg.style());

    crate::fmt::check_comments_survived(src, &reply.comments, &out)?;

    Ok(out)
}

/*
Turn the wire document into a real one.

`src` spans borrow the source and `lit` borrows the reply, so the only copies
made are for `host` spans, which have to outlive the token buffers they were
emitted from. Depth is bounded before this runs: serde_json refuses more than
128 levels of nesting, so the recursion here cannot go deeper than that.
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

        WireDoc::IfBreak(flat, broken) => Doc::if_break(
            convert(flat, src, cfg)?,
            convert(broken, src, cfg)?,
        ),

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

/// A validated span, since every span here came off a wire
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

    /// The JSON here is the contract every guest mirrors, changed only with
    /// a DOC_VERSION bump
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
            document,
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
        // é is two bytes, so 1..2 lands inside it
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
}
