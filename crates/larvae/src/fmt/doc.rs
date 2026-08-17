/*!
The layout document, and the renderer that turns it into text.

Formatting is two problems. They are easier to solve apart than together. This
module decides only where the line breaks go. The emitter builds a tree of
groups. A group that fits on the rest of the line is printed flat. A group
that does not fit is printed broken. No code in this module knows what Luau is.

The algorithm is the Wadler algorithm that every modern formatter uses. This
algorithm is the reason the output is stable. The decision for a group depends
only on the width of the group and the column where it starts. The decision
never depends on what a previous group chose.

Text borrows from the source where possible. Because of this, the renderer
allocates roughly one `Vec` per nesting level, not a string per token.
*/

use std::borrow::Cow;

/// One piece of the layout.
#[derive(Debug, Clone)]
pub enum Doc<'a> {
    /// An empty document. It is the identity for concatenation.
    Nil,
    /// Literal text, usually a slice of the source.
    Text(Cow<'a, str>),
    /// A space when flat, a newline when broken.
    Line,
    /// Nothing when flat, a newline when broken.
    Soft,
    /// A newline in both modes. It also forces each enclosing group to break.
    Hard,
    /// A blank line that the author wrote. Larvae keeps it because it separates ideas.
    Blank,
    /*
    The renderer prints one document when the enclosing group is flat, and the
    other when it is broken.

    The trailing comma on a table is the reason this variant exists. The comma
    must be absent on one line and present on many lines. A formatter that
    decides this without the outcome of the group emits output that it does
    not reproduce when it formats its own output again.
    */
    IfBreak(Box<Doc<'a>>, Box<Doc<'a>>),
    /// The renderer prints this flat if it fits, and broken if it does not fit.
    Group(Box<Doc<'a>>),
    /// One more level of indentation for the content inside.
    Indent(Box<Doc<'a>>),
    /// A sequence of documents, printed in order.
    Concat(Vec<Doc<'a>>),
}

impl<'a> Doc<'a> {
    pub fn text(s: impl Into<Cow<'a, str>>) -> Self {
        Self::Text(s.into())
    }

    pub fn group(inner: Doc<'a>) -> Self {
        Self::Group(Box::new(inner))
    }

    pub fn indent(inner: Doc<'a>) -> Self {
        Self::Indent(Box::new(inner))
    }

    /// Prints `broken` only when the enclosing group breaks, and `flat` otherwise.
    pub fn if_break(flat: Doc<'a>, broken: Doc<'a>) -> Self {
        Self::IfBreak(Box::new(flat), Box::new(broken))
    }

    /*
    The width this document takes on one line, or None when it cannot take
    one line.

    A caller that must choose a layout before the renderer runs asks this.
    `fits` answers a related question, but it answers it against a column and
    a style, and it stops as soon as it knows. This returns the width itself,
    which a caller compares against a width of its own.
    */
    pub fn flat_width(&self) -> Option<usize> {
        match self {
            Self::Nil | Self::Soft => Some(0),

            Self::Text(s) => match s.contains('\n') {
                true => None,

                false => Some(width_of(s)),
            },

            Self::Line => Some(1),

            // A forced break already answers the flat question.
            Self::Hard | Self::Blank => None,

            Self::IfBreak(flat, _) => flat.flat_width(),

            Self::Group(inner) | Self::Indent(inner) => inner.flat_width(),

            Self::Concat(parts) => parts
                .iter()
                .try_fold(0, |sum, part| Some(sum + part.flat_width()?)),
        }
    }

    pub fn concat(parts: impl IntoIterator<Item = Doc<'a>>) -> Self {
        let parts: Vec<_> = parts.into_iter().filter(|d| !d.is_nil()).collect();

        match parts.len() {
            0 => Self::Nil,

            1 => parts.into_iter().next().expect("length checked"),

            _ => Self::Concat(parts),
        }
    }

    /// Joins `parts` with `sep` between them. Most lists have this shape.
    pub fn join(sep: Doc<'a>, parts: impl IntoIterator<Item = Doc<'a>>) -> Self {
        let mut out = Vec::new();

        for (i, part) in parts.into_iter().enumerate() {
            if i > 0 {
                out.push(sep.clone());
            }

            out.push(part);
        }

        Self::concat(out)
    }

    fn is_nil(&self) -> bool {
        matches!(self, Self::Nil)
    }

    /// Returns the same document with each borrow copied to owned data.
    /// Use this when the document must outlive the source and token buffers.
    pub fn into_owned(self) -> Doc<'static> {
        match self {
            Self::Nil => Doc::Nil,

            Self::Text(s) => Doc::Text(Cow::Owned(s.into_owned())),

            Self::Line => Doc::Line,

            Self::Soft => Doc::Soft,

            Self::Hard => Doc::Hard,

            Self::Blank => Doc::Blank,

            Self::IfBreak(flat, broken) => {
                Doc::IfBreak(Box::new(flat.into_owned()), Box::new(broken.into_owned()))
            }

            Self::Group(inner) => Doc::Group(Box::new(inner.into_owned())),

            Self::Indent(inner) => Doc::Indent(Box::new(inner.into_owned())),

            Self::Concat(parts) => Doc::Concat(parts.into_iter().map(Doc::into_owned).collect()),
        }
    }

    /*
    Reports if content inside this document forces a break.

    The renderer cannot print a hard newline or a blank line flat. So each
    group that contains one breaks before the renderer considers its width.
    This rule is the reason a function body with statements always expands.
    */
    fn must_break(&self) -> bool {
        match self {
            Self::Hard | Self::Blank => true,

            // A long comment that spans lines cannot sit on a flat line.
            Self::Text(s) => s.contains('\n'),

            Self::Group(inner) | Self::Indent(inner) => inner.must_break(),

            // Only the flat side can exclude flat mode. The broken side never runs flat.
            Self::IfBreak(flat, _) => flat.must_break(),

            Self::Concat(parts) => parts.iter().any(Self::must_break),

            _ => false,
        }
    }
}

/// Controls how the renderer lays text out.
#[derive(Debug, Clone, Copy)]
pub struct Style {
    /// The column that the renderer tries to stay under. It is a guide, not a hard limit.
    pub width: usize,
    /// One level of indentation, already expanded to the characters to emit.
    pub indent: Indent,
    /// The characters that end a line.
    pub newline: &'static str,
}

/// A single indentation level.
#[derive(Debug, Clone, Copy)]
pub enum Indent {
    Tabs {
        /// The width is only for width accounting. A tab emits one character.
        width: usize,
    },
    Spaces(usize),
}

impl Indent {
    /// Returns the number of columns this level occupies on screen.
    /// The fits calculation uses this number.
    fn columns(self) -> usize {
        match self {
            Self::Tabs { width } => width,

            Self::Spaces(n) => n,
        }
    }

    fn push(self, out: &mut String) {
        match self {
            Self::Tabs { .. } => out.push('\t'),

            Self::Spaces(n) => out.extend(std::iter::repeat_n(' ', n)),
        }
    }
}

impl Default for Style {
    fn default() -> Self {
        Self {
            width: 120,
            indent: Indent::Tabs { width: 4 },
            newline: "\n",
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Flat,
    Broken,
}

/// Renders a document to text.
pub fn render(doc: &Doc<'_>, style: Style) -> String {
    let mut out = String::with_capacity(256);
    let mut column = 0usize;
    let mut stack = vec![(0usize, Mode::Broken, doc)];

    while let Some((depth, mode, doc)) = stack.pop() {
        match doc {
            Doc::Nil => {}

            Doc::Text(s) => {
                out.push_str(s);

                /*
                A long comment carries its own newlines. The renderer emits it
                exactly as the author wrote it, because new indentation on its
                inner lines would change what the comment says. The column
                continues from its last line, not from its total width.
                */
                column = match s.rsplit_once('\n') {
                    Some((_, last)) => width_of(last),

                    None => column + width_of(s),
                };
            }

            Doc::Line if mode == Mode::Flat => {
                out.push(' ');
                column += 1;
            }

            Doc::Soft if mode == Mode::Flat => {}

            Doc::Line | Doc::Soft | Doc::Hard => {
                newline(&mut out, style, depth, &mut column);
            }

            /*
            A blank line is one break more than a hard one. So it replaces the
            separator instead of an addition to it. If the renderer emitted
            both, the output would have two blank lines where the author left
            one.
            */
            Doc::Blank => {
                trim_end(&mut out);
                out.push_str(style.newline);
                newline(&mut out, style, depth, &mut column);
            }

            Doc::IfBreak(flat, broken) => stack.push((
                depth,
                mode,
                match mode {
                    Mode::Flat => flat,
                    Mode::Broken => broken,
                },
            )),

            Doc::Indent(inner) => stack.push((depth + 1, mode, inner)),

            Doc::Concat(parts) => {
                for part in parts.iter().rev() {
                    stack.push((depth, mode, part));
                }
            }

            Doc::Group(inner) => {
                /*
                This is the one decision this file exists to make. A group is
                flat when it has no forced break and its remaining content fits
                in the columns left on this line.
                */
                let mode = if !inner.must_break() && fits(inner, style, column) {
                    Mode::Flat
                } else {
                    Mode::Broken
                };

                stack.push((depth, mode, inner));
            }
        }
    }

    out
}

/// The output must never have trailing whitespace. So each break trims the text before it.
fn trim_end(out: &mut String) {
    while out.ends_with(' ') || out.ends_with('\t') {
        out.pop();
    }
}

fn newline(out: &mut String, style: Style, depth: usize, column: &mut usize) {
    trim_end(out);
    out.push_str(style.newline);

    for _ in 0..depth {
        style.indent.push(out);
    }

    *column = depth * style.indent.columns();
}

/*
Reports if a document printed flat stays inside the width.

The function measures from `column`. So the same group can be flat in one
place and broken in another place. Users expect this behavior from a
formatter. A simple check of only the node length produces bad output for
this reason.
*/
fn fits(doc: &Doc<'_>, style: Style, column: usize) -> bool {
    let mut remaining = style.width.saturating_sub(column);
    let mut stack = vec![doc];

    while let Some(doc) = stack.pop() {
        match doc {
            Doc::Nil | Doc::Soft => {}

            Doc::Text(s) => {
                if s.contains('\n') {
                    return false;
                }

                let w = width_of(s);

                if w > remaining {
                    return false;
                }

                remaining -= w;
            }

            Doc::Line => {
                if remaining == 0 {
                    return false;
                }

                remaining -= 1;
            }

            // A forced break already answers the flat question.
            Doc::Hard | Doc::Blank => return false,

            // The function measures flat mode, so it only reaches the flat side.
            Doc::IfBreak(flat, _) => stack.push(flat),

            Doc::Group(inner) | Doc::Indent(inner) => stack.push(inner),

            Doc::Concat(parts) => {
                for part in parts.iter().rev() {
                    stack.push(part);
                }
            }
        }
    }

    true
}

/*
Returns the display width of a string.

A byte count would break each file that has a non-ASCII string literal. A
`char` count is close enough and does not need a full grapheme table. The
cases it gets wrong are wide CJK characters and combining marks. Those cases
cost one or two columns in a comment. They do not produce wrong code.
*/
fn width_of(s: &str) -> usize {
    s.chars().count()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn narrow(width: usize) -> Style {
        Style {
            width,
            indent: Indent::Spaces(2),
            newline: "\n",
        }
    }

    fn call<'a>(name: &'a str, args: Vec<Doc<'a>>) -> Doc<'a> {
        Doc::group(Doc::concat([
            Doc::text(name),
            Doc::text("("),
            Doc::indent(Doc::concat([
                Doc::Soft,
                Doc::join(Doc::concat([Doc::text(","), Doc::Line]), args),
            ])),
            Doc::Soft,
            Doc::text(")"),
        ]))
    }

    #[test]
    fn a_group_that_fits_is_printed_flat() {
        let doc = call("f", vec![Doc::text("a"), Doc::text("b")]);

        assert_eq!(render(&doc, narrow(80)), "f(a, b)");
    }

    #[test]
    fn a_group_that_does_not_fit_breaks() {
        let doc = call("f", vec![Doc::text("aaaa"), Doc::text("bbbb")]);

        assert_eq!(render(&doc, narrow(10)), "f(\n  aaaa,\n  bbbb\n)");
    }

    /// The renderer decides per group. An outer break does not force an inner break.
    #[test]
    fn an_inner_group_stays_flat_when_it_can() {
        let doc = call(
            "outer",
            vec![call("inner", vec![Doc::text("a")]), Doc::text("bbbbbbbbbb")],
        );

        let out = render(&doc, narrow(20));

        assert!(out.contains("inner(a)"), "inner should stay flat: {out}");
        assert!(out.contains('\n'), "outer should break: {out}");
    }

    #[test]
    fn a_hard_break_forces_every_group_around_it() {
        let doc = Doc::group(Doc::concat([
            Doc::text("do"),
            Doc::indent(Doc::concat([Doc::Hard, Doc::text("x")])),
            Doc::Hard,
            Doc::text("end"),
        ]));

        assert_eq!(render(&doc, narrow(200)), "do\n  x\nend");
    }

    #[test]
    fn indentation_nests() {
        let doc = Doc::concat([
            Doc::text("a"),
            Doc::indent(Doc::concat([
                Doc::Hard,
                Doc::text("b"),
                Doc::indent(Doc::concat([Doc::Hard, Doc::text("c")])),
            ])),
        ]);

        assert_eq!(render(&doc, narrow(80)), "a\n  b\n    c");
    }

    #[test]
    fn a_blank_line_survives_as_one_blank_line() {
        let doc = Doc::concat([Doc::text("a"), Doc::Blank, Doc::text("b")]);

        assert_eq!(render(&doc, narrow(80)), "a\n\nb");
    }

    /// Trailing whitespace is the most common formatter complaint. The renderer never emits it.
    #[test]
    fn no_line_ever_ends_in_whitespace() {
        let doc = Doc::concat([
            Doc::text("a"),
            Doc::text(" "),
            Doc::Hard,
            Doc::text("b"),
            Doc::text("\t"),
            Doc::Hard,
            Doc::text("c"),
        ]);

        for line in render(&doc, narrow(80)).lines() {
            assert_eq!(line, line.trim_end(), "trailing whitespace on {line:?}");
        }
    }

    #[test]
    fn tabs_indent_by_one_character_but_count_as_their_width() {
        let style = Style {
            width: 10,
            indent: Indent::Tabs { width: 4 },
            newline: "\n",
        };

        let doc = Doc::concat([Doc::indent(Doc::concat([Doc::Hard, Doc::text("x")]))]);

        assert_eq!(render(&doc, style), "\n\tx");
    }

    #[test]
    fn windows_line_endings_are_honoured() {
        let style = Style {
            width: 80,
            indent: Indent::Spaces(2),
            newline: "\r\n",
        };

        let doc = Doc::concat([Doc::text("a"), Doc::Hard, Doc::text("b")]);

        assert_eq!(render(&doc, style), "a\r\nb");
    }

    /// The renderer measures width in characters. So a non-ASCII literal does not break early.
    #[test]
    fn width_counts_characters_and_not_bytes() {
        assert_eq!(width_of("héllo"), 5);
        assert_eq!(width_of("\"héllo\""), 7);

        let doc = call("f", vec![Doc::text("\"héllo\"")]);

        assert_eq!(render(&doc, narrow(12)), "f(\"héllo\")");
    }

    #[test]
    fn an_empty_group_renders_to_nothing() {
        assert_eq!(render(&Doc::group(Doc::Nil), narrow(80)), "");
        assert_eq!(render(&Doc::concat([]), narrow(80)), "");
    }
}
