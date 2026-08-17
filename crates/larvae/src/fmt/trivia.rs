/*!
Comments and blank lines. The parser intentionally does not carry them.

The lexer keeps comments in a list beside the tokens, not inside the tree.
Everything larvae did before this formatter spliced byte ranges and never
needed them. A formatter rebuilds the file from the tree. So content that is
not in the tree is lost, unless some code puts it back. This module is that
code.

The rule is the one that every formatter converges on. A comment on the same
line as the code before it belongs to that code and trails it. In all other
cases it leads the code that comes next. The formatter keeps blank lines but
collapses them. One blank line separates two ideas. Four blank lines only add
scroll distance.
*/

/// One comment, as written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Comment {
    pub start: u32,
    pub end: u32,
}

impl Comment {
    pub fn text<'s>(&self, src: &'s str) -> &'s str {
        // A line comment runs to the newline. The newline is not part of it.
        src[self.start as usize..self.end as usize].trim_end()
    }

    /// Reports if this is a `--[[ ]]` form. That form can span lines.
    pub fn is_long(&self, src: &str) -> bool {
        self.text(src).contains('\n')
    }
}

/// Looks up comments and blank lines in one file.
pub struct Trivia<'a> {
    src: &'a str,
    comments: Vec<Comment>,
    /// The byte offset of the first character of each line.
    line_starts: Vec<u32>,
}

impl<'a> Trivia<'a> {
    pub fn new(src: &'a str, comments: &[(u32, u32)]) -> Self {
        let mut line_starts = vec![0u32];

        for (i, b) in src.bytes().enumerate() {
            if b == b'\n' {
                line_starts.push(i as u32 + 1);
            }
        }

        Self {
            src,
            comments: comments
                .iter()
                .map(|&(start, end)| Comment { start, end })
                .collect(),
            line_starts,
        }
    }

    /// Returns the zero-based line that holds this byte.
    pub fn line(&self, byte: u32) -> usize {
        self.line_starts.partition_point(|&s| s <= byte) - 1
    }

    /*
    Returns every comment that starts in `[lo, hi)`.

    The comments are sorted, and a comment never overlaps a token. So a byte
    range is enough to find the comments that sit in a gap. No per-token
    index is necessary.
    */
    pub fn between(&self, lo: u32, hi: u32) -> &[Comment] {
        let start = self.comments.partition_point(|c| c.start < lo);
        let end = self.comments.partition_point(|c| c.start < hi);

        &self.comments[start..end.max(start)]
    }

    /// Reports if an author left at least one blank line in this range.
    pub fn blank_between(&self, lo: u32, hi: u32) -> bool {
        if lo >= hi {
            return false;
        }

        self.src[lo as usize..hi as usize]
            .bytes()
            .filter(|&b| b == b'\n')
            .nth(1)
            .is_some()
    }

    /*
    Splits the comments in a gap into two sets: the one comment that trails
    the code before the gap, and the comments that lead the code after it.

    `lo` is the byte just past the previous token. `hi` is the start of the
    next token. A comment trails only when it starts on the same line where
    the previous token ended. That is the whole rule.
    */
    pub fn split(&self, lo: u32, hi: u32) -> Attached<'_> {
        let all = self.between(lo, hi);

        let trailing = all
            .first()
            .filter(|c| lo > 0 && self.line(lo.saturating_sub(1)) == self.line(c.start))
            .copied();

        let leading = if trailing.is_some() { &all[1..] } else { all };

        Attached {
            trailing,
            leading,
            blank_before_leading: leading.first().is_some_and(|first| {
                let after = trailing.map_or(lo, |t| t.end);

                self.blank_between(after, first.start)
            }),
        }
    }

    /*
    Reports if the author left a blank line directly below this comment.

    A comment is a paragraph of its own. The author who puts a blank line
    below one separates it from the code that follows, and the author who
    does not attaches it to that code. The two say different things, so the
    formatter keeps the difference.

    The question goes to the source and not to the comment list. The next
    item below a comment is another comment, or code, or the closer of a
    block, or the end of the file. All four read the same way here, so the
    caller does not have to know which one it has.
    */
    pub fn blank_after(&self, c: Comment) -> bool {
        let rest = &self.src[c.end as usize..];

        let next = rest
            .find(|ch: char| !ch.is_whitespace())
            .unwrap_or(rest.len());

        self.blank_between(c.end, c.end + next as u32)
    }

    /// Reports if a blank line separates the code that ends at `lo` from the code at `hi`.
    /// It looks past the comments in between, so a commented statement keeps its gap.
    pub fn blank_before_code(&self, lo: u32, hi: u32) -> bool {
        let all = self.between(lo, hi);

        // The gap that matters is the gap before the first item in the range.
        let first = all.first().map_or(hi, |c| c.start);
        let trailing_same_line = all
            .first()
            .is_some_and(|c| lo > 0 && self.line(lo.saturating_sub(1)) == self.line(c.start));

        if trailing_same_line {
            let after = all[0].end;
            let next = all.get(1).map_or(hi, |c| c.start);

            return self.blank_between(after, next);
        }

        self.blank_between(lo, first)
    }
}

/// The comments that sit in one gap, sorted into the places where they belong.
#[derive(Debug)]
pub struct Attached<'t> {
    /// A comment on the same line as the code before it.
    pub trailing: Option<Comment>,
    /// Comments on their own lines. They belong to the code that follows.
    pub leading: &'t [Comment],
    /// Reports if a blank line comes before those leading comments.
    pub blank_before_leading: bool,
}

impl Attached<'_> {
    pub fn is_empty(&self) -> bool {
        self.trailing.is_none() && self.leading.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax::lexer;

    fn trivia(src: &str) -> Trivia<'_> {
        let lexed = lexer::lex(src).expect("lexes");

        Trivia::new(src, &lexed.comments)
    }

    #[test]
    fn lines_are_counted_from_zero() {
        let src = "a\nb\nc";
        let t = trivia(src);

        assert_eq!(t.line(0), 0);
        assert_eq!(t.line(2), 1);
        assert_eq!(t.line(4), 2);
    }

    #[test]
    fn a_comment_on_the_code_line_trails_it() {
        let src = "local a = 1 -- why\nlocal b = 2\n";
        let t = trivia(src);

        let lo = src.find(" --").unwrap() as u32;
        let hi = src.find("local b").unwrap() as u32;
        let split = t.split(lo, hi);

        assert_eq!(split.trailing.map(|c| c.text(src)), Some("-- why"));
        assert!(split.leading.is_empty());
    }

    #[test]
    fn a_comment_on_its_own_line_leads_what_follows() {
        let src = "local a = 1\n-- why\nlocal b = 2\n";
        let t = trivia(src);

        let lo = src.find('\n').unwrap() as u32;
        let hi = src.find("local b").unwrap() as u32;
        let split = t.split(lo, hi);

        assert_eq!(split.trailing, None);
        assert_eq!(
            split
                .leading
                .iter()
                .map(|c| c.text(src))
                .collect::<Vec<_>>(),
            ["-- why"]
        );
    }

    /// Both at once is the common case, and it is the case that is easy to get wrong.
    #[test]
    fn a_gap_can_hold_a_trailing_and_a_leading_comment() {
        let src = "local a = 1 -- trails\n-- leads\nlocal b = 2\n";
        let t = trivia(src);

        let lo = src.find(" --").unwrap() as u32;
        let hi = src.find("local b").unwrap() as u32;
        let split = t.split(lo, hi);

        assert_eq!(split.trailing.map(|c| c.text(src)), Some("-- trails"));
        assert_eq!(
            split
                .leading
                .iter()
                .map(|c| c.text(src))
                .collect::<Vec<_>>(),
            ["-- leads"]
        );
    }

    #[test]
    fn several_leading_comments_all_come_through() {
        let src = "local a = 1\n-- one\n-- two\n-- three\nlocal b = 2\n";
        let t = trivia(src);

        let lo = src.find('\n').unwrap() as u32;
        let hi = src.find("local b").unwrap() as u32;

        assert_eq!(t.split(lo, hi).leading.len(), 3);
    }

    #[test]
    fn a_blank_line_is_seen_and_a_single_newline_is_not() {
        let src = "a\n\nb";
        let t = trivia(src);

        assert!(t.blank_between(1, 3));
        assert!(!t.blank_between(1, 2));
    }

    /// A blank line before a commented statement belongs to the statement.
    #[test]
    fn the_gap_before_a_leading_comment_is_reported() {
        let src = "local a = 1\n\n-- section\nlocal b = 2\n";
        let t = trivia(src);

        let lo = src.find('\n').unwrap() as u32;
        let hi = src.find("local b").unwrap() as u32;

        assert!(t.split(lo, hi).blank_before_leading);
        assert!(t.blank_before_code(lo, hi));
    }

    #[test]
    fn no_gap_means_no_blank_reported() {
        let src = "local a = 1\n-- section\nlocal b = 2\n";
        let t = trivia(src);

        let lo = src.find('\n').unwrap() as u32;
        let hi = src.find("local b").unwrap() as u32;

        assert!(!t.split(lo, hi).blank_before_leading);
        assert!(!t.blank_before_code(lo, hi));
    }

    /*
    The gap below a comment reads the same whatever comes next.

    Four things can follow a comment: another comment, code, the closer of a
    block, and the end of the file. The check goes to the source, so the
    caller does not have to know which one it has.
    */
    #[test]
    fn the_gap_below_a_comment_is_reported() {
        let cases = [
            ("-- a\n\nlocal b = 2\n", true),
            ("-- a\nlocal b = 2\n", false),
            ("-- a\n\n-- b\nlocal c = 3\n", true),
            ("-- a\n-- b\nlocal c = 3\n", false),
            ("do\n-- a\n\nend\n", true),
            ("do\n-- a\nend\n", false),
            ("local a = 1\n-- a\n", false),
        ];

        for (src, want) in cases {
            let t = trivia(src);
            let first = t.comments[0];

            assert_eq!(t.blank_after(first), want, "{src:?}");
        }
    }

    /// A trailing comment must not hide the blank line after it.
    #[test]
    fn a_blank_after_a_trailing_comment_still_counts() {
        let src = "local a = 1 -- trails\n\nlocal b = 2\n";
        let t = trivia(src);

        let lo = src.find(" --").unwrap() as u32;
        let hi = src.find("local b").unwrap() as u32;

        assert!(t.blank_before_code(lo, hi));
    }

    #[test]
    fn a_long_comment_keeps_its_own_lines() {
        let src = "--[[\n  spans\n]]\nlocal a = 1\n";
        let t = trivia(src);
        let all = t.between(0, src.len() as u32);

        assert_eq!(all.len(), 1);
        assert!(all[0].is_long(src));
        assert!(all[0].text(src).contains("spans"));
    }

    #[test]
    fn a_single_line_long_comment_is_not_treated_as_multi_line() {
        let src = "local a = 1 --[[ inline ]]\n";
        let t = trivia(src);
        let all = t.between(0, src.len() as u32);

        assert_eq!(all.len(), 1);
        assert!(!all[0].is_long(src));
    }

    #[test]
    fn an_empty_range_holds_nothing() {
        let src = "local a = 1\nlocal b = 2\n";
        let t = trivia(src);

        assert!(t.between(0, 0).is_empty());
        assert!(t.split(5, 5).is_empty());
        assert!(!t.blank_between(5, 5));
    }

    /// A file that is only a comment must still round-trip.
    #[test]
    fn a_file_of_only_comments_is_all_leading() {
        let src = "-- one\n-- two\n";
        let t = trivia(src);
        let split = t.split(0, src.len() as u32);

        assert_eq!(split.trailing, None);
        assert_eq!(split.leading.len(), 2);
    }
}
