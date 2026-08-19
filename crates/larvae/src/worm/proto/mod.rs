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

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

mod regions;
mod render;

pub use render::render_format;

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

/*
One edit a worm wants made, as byte offsets into the file it was given.

Bytes and not a line and column, because a worm parses the file it claims and
knows where things are in it. The host turns these into the positions the
editor speaks, which is the same conversion it already makes for a finding.
*/
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireEdit {
    /// The byte range to replace. An empty range inserts.
    pub span: (u32, u32),
    /// What goes there. Empty deletes.
    #[serde(default)]
    pub text: String,
}

/// One code action a worm offers over a range.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireAction {
    /// What the editor shows in the lightbulb.
    pub title: String,
    /// The edits, applied together as one change.
    #[serde(default)]
    pub edits: Vec<WireEdit>,
    /*
    The lint this repairs, when it repairs one.

    An editor groups a fix under the diagnostic it belongs to, so a fix that
    names its lint appears on the problem rather than in a general list.
    */
    #[serde(default)]
    pub fixes: Option<String>,
}

/// An actions reply, assembled by the transport
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ActionsReply {
    #[serde(default)]
    pub actions: Vec<WireAction>,
}

/*
A definitions reply, assembled by the transport.

A worm that teaches larvae a new kind of module states what that module is,
as Luau definition text, because that is what luau-lsp reads. A worm that
makes a data file requirable is the case this exists for.
*/
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DefinitionsReply {
    #[serde(default)]
    pub definitions: String,
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
pub(super) fn slice(src: &str, start: u32, end: u32) -> Result<&str> {
    let (s, e) = (start as usize, end as usize);

    if s > e || e > src.len() || !src.is_char_boundary(s) || !src.is_char_boundary(e) {
        bail!("the span {start}..{end} does not lie on the source");
    }

    Ok(&src[s..e])
}
