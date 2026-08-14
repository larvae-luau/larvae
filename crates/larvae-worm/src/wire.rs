/*!
The payloads that cross a worm boundary, on every transport.

A worm that formats returns a layout document, and larvae renders it with the
settings of the project. A worm that lints returns findings without a
severity, and the host stamps the levels. These shapes are one contract for
the native pipe and for the wasm ABI, so a worm that changes transport keeps
its reply code.

The JSON here mirrors `worm/proto.rs` on the larvae side. The tests of both
crates pin the exact text, which stands in for a shared crate.
*/

use serde::Serialize;

/// The layout contract revision this module speaks. It is `doc` in a format reply.
pub const DOC_VERSION: u32 = 1;

/**
One piece of layout, in exactly the shape that larvae deserializes.

Source text crosses as a `Src` span and not as a copy. `Lit` is reserved for
text that the worm generated. `Host` marks a span of ordinary Luau that
larvae formats itself and splices in. This lets a worm own its markup and no
Luau at all.
*/
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Doc {
    /// No output at all
    Nil,
    /// An exact slice of the source, by byte range
    Src(u32, u32),
    /// Text that the worm generated
    Lit(String),
    /// A space when flat, a newline when broken
    Line,
    /// Nothing when flat, a newline when broken
    Soft,
    /// A newline in both modes. It forces every enclosing group to break.
    Hard,
    /// A blank line that the author wrote. It is kept because it separates ideas.
    Blank,
    /// One value when the enclosing group is flat, an other value when it breaks
    IfBreak(Box<Doc>, Box<Doc>),
    /// Flat when it fits the line, broken when it does not fit
    Group(Box<Doc>),
    /// One more level of indentation for the content inside
    Indent(Box<Doc>),
    /// The parts, in order
    Concat(Vec<Doc>),
    /// A span of ordinary Luau for larvae to format and splice in
    Host {
        /// The byte offset where the span starts
        start: u32,
        /// The byte offset one past its end
        end: u32,
        /// The mode in which larvae parses it
        parse: HostParse,
    },
}

/// The parse mode of a [`Doc::Host`] span
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HostParse {
    /// Statements, which is the shape between markup regions
    Block,
    /// One expression, a `{expr}` hole or attribute value
    Expr,
}

impl Doc {
    /// An exact slice of the source
    pub fn src(start: u32, end: u32) -> Self {
        Self::Src(start, end)
    }

    /// Text that the worm generated
    pub fn lit(s: impl Into<String>) -> Self {
        Self::Lit(s.into())
    }

    /// Flat when it fits, broken when it does not fit
    pub fn group(inner: Doc) -> Self {
        Self::Group(Box::new(inner))
    }

    /// One more level of indentation for the content inside
    pub fn indent(inner: Doc) -> Self {
        Self::Indent(Box::new(inner))
    }

    /// `broken` only when the enclosing group breaks, and `flat` in the other case
    pub fn if_break(flat: Doc, broken: Doc) -> Self {
        Self::IfBreak(Box::new(flat), Box::new(broken))
    }

    /// The parts, in order
    pub fn concat(parts: impl IntoIterator<Item = Doc>) -> Self {
        Self::Concat(parts.into_iter().collect())
    }

    /// `parts` separated by `sep`, which is the shape that most lists take
    pub fn join(sep: Doc, parts: impl IntoIterator<Item = Doc>) -> Self {
        let mut out = Vec::new();

        for (i, part) in parts.into_iter().enumerate() {
            if i > 0 {
                out.push(sep.clone());
            }

            out.push(part);
        }

        Self::Concat(out)
    }

    /// A span of Luau statements for larvae to format
    pub fn host(start: u32, end: u32) -> Self {
        Self::Host {
            start,
            end,
            parse: HostParse::Block,
        }
    }

    /// A span that holds one Luau expression for larvae to format
    pub fn host_expr(start: u32, end: u32) -> Self {
        Self::Host {
            start,
            end,
            parse: HostParse::Expr,
        }
    }
}

/// One problem found. The severity is absent by intent, because the host owns it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Finding {
    /// The byte range in the source
    pub span: (u32, u32),
    /// The name of the lint. You must declare it in `[lints]` in your `worm.toml`.
    pub lint: String,
    /// The description of the problem
    pub message: String,
    /// The fix, when there is a short fix to state
    #[serde(skip_serializing_if = "Option::is_none")]
    pub help: Option<String>,
}

impl Finding {
    /// A new finding. Add help with [`with_help`](Self::with_help).
    pub fn new(lint: impl Into<String>, span: (u32, u32), message: impl Into<String>) -> Self {
        Self {
            span,
            lint: lint.into(),
            message: message.into(),
            help: None,
        }
    }

    /// The same finding with a help line
    pub fn with_help(mut self, help: impl Into<String>) -> Self {
        self.help = Some(help.into());

        self
    }
}

/// The value that [`Handler::format`] returns
#[derive(Debug, Clone, PartialEq)]
pub struct Format {
    /// The layout for the whole file. Leave it empty when you send `spans`.
    pub document: Option<Doc>,
    /**
    The regions of ordinary Luau, for a worm that lays out nothing itself.

    This is the least a worm can do and still format. Name the byte ranges
    that hold Luau, and larvae builds the document: it formats each range and
    keeps every byte between the ranges as the author wrote it. Thus the Luau
    in your files follows the style of the project, and your own syntax is
    untouched.

    `document` wins when you send both.
    */
    pub spans: Vec<(u32, u32)>,
    /// The span of every comment, so larvae can refuse a layout that lost one
    pub comments: Vec<(u32, u32)>,
}

impl Format {
    /// A layout that you built yourself
    pub fn document(document: Doc) -> Self {
        Self {
            document: Some(document),
            spans: Vec::new(),
            comments: Vec::new(),
        }
    }

    /// The Luau regions of the file, for larvae to lay out
    pub fn spans(spans: Vec<(u32, u32)>) -> Self {
        Self {
            document: None,
            spans,
            comments: Vec::new(),
        }
    }

    /// The same reply, with the span of every comment. Larvae refuses output
    /// that lost a comment.
    pub fn with_comments(mut self, comments: Vec<(u32, u32)>) -> Self {
        self.comments = comments;

        self
    }
}

/// The value that [`Handler::lint`] returns
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Lint {
    /// The problems found
    pub findings: Vec<Finding>,
    /**
    The Luau shadow of the file, for the lints of larvae to read.

    The shadow is the source with every region that is not Luau replaced by
    filler of the same byte length. Thus each offset in the shadow is the same
    offset in the source, and larvae maps no spans. The shadow must parse as
    Luau.

    Set this field when `worm.toml` says `inherit_lints = true` and you want
    exact columns. Leave it empty to let larvae read the output of your own
    `transform` instead, which maps by line.
    */
    pub luau: Option<String>,
    /// The comment spans, so `-- larvae: allow(...)` works in a claimed file.
    /// Leave the list empty to remove your findings from suppression.
    pub comments: Vec<(u32, u32)>,
}
