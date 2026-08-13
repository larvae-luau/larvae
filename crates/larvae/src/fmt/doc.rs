/*!
The layout document, and the renderer that turns it into text.

Formatting is two problems, and they are much easier apart than together. This
half decides only *where the line breaks go*: the emitter builds a tree of
groups, and a group either fits on the rest of the line and is printed flat, or
does not and is printed broken. Nothing here knows what Luau is.

The algorithm is the Wadler one every modern formatter uses, which is worth
saying because it is the reason output is stable: a group's decision depends
only on its own width and the column it starts at, never on what a previous
group chose to do.

Text borrows from the source wherever possible, so formatting a file allocates
roughly one `Vec` per nesting level rather than a string per token.
*/

use std::borrow::Cow;

/// One piece of layout
#[derive(Debug, Clone)]
pub enum Doc<'a> {
    /// Nothing at all, the identity for concatenation
    Nil,
    /// Literal text, usually a slice of the source
    Text(Cow<'a, str>),
    /// A space when flat, a newline when broken
    Line,
    /// Nothing when flat, a newline when broken
    Soft,
    /// A newline either way, which also forces every enclosing group to break
    Hard,
    /// A blank line the author wrote, kept because it separates ideas
    Blank,
    /*
    One thing when the enclosing group is flat, another when it is broken.

    The trailing comma on a table exists for this: it must be absent on one
    line and present on many, and deciding which without knowing the group's
    outcome is what makes a formatter emit output it will not reproduce when
    given its own output back.
    */
    IfBreak(Box<Doc<'a>>, Box<Doc<'a>>),
    /// Flat if it fits, broken if it does not
    Group(Box<Doc<'a>>),
    /// One more level of indentation for anything inside
    Indent(Box<Doc<'a>>),
    /// In order
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

    /// `broken` only when the enclosing group breaks, `flat` otherwise
    pub fn if_break(flat: Doc<'a>, broken: Doc<'a>) -> Self {
        Self::IfBreak(Box::new(flat), Box::new(broken))
    }

    pub fn concat(parts: impl IntoIterator<Item = Doc<'a>>) -> Self {
        let parts: Vec<_> = parts.into_iter().filter(|d| !d.is_nil()).collect();

        match parts.len() {
            0 => Self::Nil,

            1 => parts.into_iter().next().expect("length checked"),

            _ => Self::Concat(parts),
        }
    }

    /// `parts` separated by `sep`, which is the shape most lists take
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

    /// The same document with every borrow bought out, for a document that
    /// has to outlive the source and token buffers it was emitted from
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
    Whether anything inside forces a break.

    A hard newline or a blank line cannot be printed flat, so every group
    containing one is broken before its width is even considered. This is what
    makes a function body with statements in it always expand.
    */
    fn must_break(&self) -> bool {
        match self {
            Self::Hard | Self::Blank => true,

            // a long comment spanning lines cannot sit on a flat line
            Self::Text(s) => s.contains('\n'),

            Self::Group(inner) | Self::Indent(inner) => inner.must_break(),

            // only the flat side can rule flat out, the broken side never runs flat
            Self::IfBreak(flat, _) => flat.must_break(),

            Self::Concat(parts) => parts.iter().any(Self::must_break),

            _ => false,
        }
    }
}

/// How the renderer lays text out
#[derive(Debug, Clone, Copy)]
pub struct Style {
    /// The column to aim to stay under, a guide rather than a hard limit
    pub width: usize,
    /// One level of indentation, already expanded to the characters to emit
    pub indent: Indent,
    /// What ends a line
    pub newline: &'static str,
}

/// A single indentation level
#[derive(Debug, Clone, Copy)]
pub enum Indent {
    Tabs {
        /// Only for width accounting, a tab emits one character
        width: usize,
    },
    Spaces(usize),
}

impl Indent {
    /// Characters this level occupies on screen, for the fits calculation
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

/// Render a document to text
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
                A long comment carries its own newlines and is emitted exactly
                as the author wrote it, since re-indenting its inner lines
                would change what the comment says. The column continues from
                its last line rather than from its total width.
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
            A blank line is one break more than a hard one, so it stands in
            for the separator rather than being added to it. Emitting both
            would give two blank lines where the author left one.
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
                The one decision this whole file exists to make. A group is
                flat when it has no forced break and what remains of it fits in
                the columns left on this line.
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

/// No trailing whitespace, ever, so every break trims what comes before it
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
Whether a document printed flat still lands inside the width.

Measured from `column`, so the same group can be flat in one place and broken
in another, which is the behaviour people expect from a formatter and the
reason a naive "is this node short" check produces bad output.
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

            // a forced break means the flat question is already answered
            Doc::Hard | Doc::Blank => return false,

            // measuring flat, so only the flat side is ever reached
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
Display width of a string.

Counting bytes would break every file with a non-ASCII string literal in it,
and counting `char`s is close enough without pulling in a full grapheme table:
the cases it gets wrong, wide CJK and combining marks, cost a column or two in
a comment rather than producing wrong code.
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

    /// The decision is per group, so an outer break does not force an inner one
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

    /// Trailing whitespace is the most common formatter complaint, so never emit it
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

    /// Width is measured in characters, so a non-ASCII literal does not break early
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
