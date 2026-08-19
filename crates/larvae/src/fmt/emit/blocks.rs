/*!
A block, and what goes between the statements in one.

This is where the trivia of a file lands: the blank lines an author left, the
comments between two statements, and the run of requires that `sort_requires`
may reorder. The statements themselves are in `stmt`; a block decides what
sits between them.
*/

use super::*;

impl<'a> Emitter<'a> {
    // --- blocks ------------------------------------------------------------

    /// Returns the byte just past the opener of this block: `do`, `then`, `)` and so on.
    fn block_lo(&self, block: &Block) -> u32 {
        match block.span.start {
            0 => 0,

            i => self.tok_end(i - 1),
        }
    }

    /// Returns the byte where the closer of this block starts, or the end of the file.
    fn block_hi(&self, block: &Block) -> u32 {
        match self.toks.get(block.span.end as usize) {
            Some(t) => t.start,

            None => self.src.len() as u32,
        }
    }

    /*
    Emits the statements of a block, with their comments and blank lines.

    The emitter drops a stray `;` instead of a print of it. The `;` is a
    statement in the grammar and noise in the output. Every formatter for the
    language drops it. All other content keeps the shape that the author gave
    it: one statement per line, and one blank line where the author left one
    or more.
    */
    pub(crate) fn block_body(&self, block: &Block) -> Doc<'a> {
        let (prologue, mut pieces, tail) = self.pieces(block);

        if self.cfg.sort_requires.enabled {
            sort_requires(&mut pieces, self.cfg.sort_requires.grouping);
        }

        let mut parts: Vec<Doc<'a>> = Vec::with_capacity(pieces.len() * 3 + 2);

        for c in &prologue {
            parts.push(self.comment_line(*c));
        }

        for (i, piece) in pieces.iter().enumerate() {
            if i > 0 {
                parts.push(self.trailing_doc(pieces[i - 1].trailing));

                // A blank line is the separator, not an addition to it.
                parts.push(if piece.blank_before {
                    Doc::Blank
                } else {
                    Doc::Hard
                });
            }

            for c in &piece.leading {
                parts.push(self.comment_line(*c));
            }

            match self.verbatim_stmt(piece.stmt) {
                Some(raw) => parts.push(Doc::text(raw)),

                None => {
                    parts.push(self.stmt(piece.stmt));
                    parts.push(self.terminator(piece.stmt, pieces.get(i + 1).map(|p| p.stmt)));
                }
            }
        }

        if let Some(last) = pieces.last() {
            parts.push(self.trailing_doc(last.trailing));
        }

        // These are the comments between the last statement and the closer of the block.
        if !tail.leading.is_empty() {
            if !pieces.is_empty() {
                parts.push(if tail.blank_before {
                    Doc::Blank
                } else {
                    Doc::Hard
                });
            }

            /*
            The break comes before the comment here, and not after it. The
            closer of the block follows the last comment, and the caller
            supplies the break above that closer.
            */
            for (i, c) in tail.leading.iter().enumerate() {
                if i > 0 {
                    parts.push(self.break_after(tail.leading[i - 1]));
                }

                parts.push(self.comment_doc(*c));
            }
        }

        Doc::concat(parts)
    }

    /*
    Splits a block into one piece per statement. Each piece holds the trivia
    that belongs to it.

    Direct emission from the statement list would be shorter, and it was, but
    `sort_requires` must move statements. Larvae finds a comment by its
    position in the source. So when a statement moves, a comment left behind
    would attach to the statement that lands in its place. This method binds
    the trivia to the statement first, so a reorder carries the trivia along.
    */
    fn pieces<'s>(&self, block: &'s Block) -> (Vec<Comment>, Vec<Piece<'s, 'a>>, Tail) {
        let mut pieces: Vec<Piece<'_, 'a>> = Vec::with_capacity(block.stmts.len());
        let mut prologue: Vec<Comment> = Vec::new();
        let mut cursor = self.block_lo(block);

        for stmt in &block.stmts {
            /*
            The emitter skips a stray `;`, and the cursor moves past it. So
            the blank line check does not count the newline on either side of
            it as a gap that the author requested.
            */
            if let Stmt::Empty(span) = stmt {
                cursor = self.tok_end(span.end - 1);

                continue;
            }

            let span = stmt.span();
            let start = self.tok_start(span.start);
            let att = self.trivia.split(cursor, start);

            // The trailing comment of this gap sits on the line above, not on this line.
            if let Some(prev) = pieces.last_mut() {
                prev.trailing = att.trailing;
            }

            let blank_before = if att.leading.is_empty() {
                self.trivia.blank_before_code(cursor, start)
            } else {
                att.blank_before_leading
            };

            /*
            A `--!` directive belongs to the file, not to the statement below
            it. The directive only takes effect before any code. So the
            emitter lifts it out of the leading comments of the first
            statement and into a prologue. The prologue keeps it at the top,
            independent of what `sort_requires` does with the statements. If
            the directive stays attached, a sort can move it down the file.
            That change would silently remove strict mode from a module.
            */
            let mut leading = att.leading.to_vec();

            if pieces.is_empty() {
                let (directives, rest) = leading
                    .iter()
                    .partition(|c| c.text(self.src).starts_with("--!"));

                prologue = directives;
                leading = rest;
            }

            pieces.push(Piece {
                stmt,
                blank_before,
                leading,
                trailing: None,
                key: self.require_path(stmt),
            });

            cursor = self.tok_end(span.end - 1);
        }

        let att = self.trivia.split(cursor, self.block_hi(block));

        if let Some(prev) = pieces.last_mut() {
            prev.trailing = att.trailing;
        }

        let tail = Tail {
            leading: att.leading.to_vec(),
            blank_before: att.blank_before_leading,
        };

        (prologue, pieces, tail)
    }

    /// A comment on its own line, with the break that the author put below it.
    fn comment_line(&self, c: Comment) -> Doc<'a> {
        Doc::concat([self.comment_doc(c), self.break_after(c)])
    }

    /*
    The break that goes below this comment.

    A leading comment took a plain hard break before, so a blank line below a
    comment was lost. The gap above the comment survived, because the piece
    carries it. That made a comment behave as one more line of the code below
    it, and an author who separated a note from the code got the two joined.
    */
    fn break_after(&self, c: Comment) -> Doc<'a> {
        match self.trivia.blank_after(c) {
            true => Doc::Blank,

            false => Doc::Hard,
        }
    }

    pub(super) fn trailing_doc(&self, comment: Option<Comment>) -> Doc<'a> {
        match comment {
            Some(c) => Doc::concat([Doc::text(" "), self.comment_doc(c)]),

            None => Doc::Nil,
        }
    }

    /*
    Returns the path of a `local name = require("path")`, and nothing else.

    The match is narrow on purpose. A computed path, for example
    `require(base .. name)`, or a bind to more than one name, can depend on
    order. Larvae cannot assume that its order does not matter. So larvae
    keeps such a statement where the author put it, and the statement breaks
    the run around it.
    */
    fn require_path(&self, stmt: &Stmt) -> Option<&'a str> {
        let Stmt::Local(local) = stmt else {
            return None;
        };

        if local.names.len() != 1 || local.values.len() != 1 {
            return None;
        }

        let Expr::Call {
            func, method, args, ..
        } = &local.values[0]
        else {
            return None;
        };

        if method.is_some() || !matches!(func.as_ref(), Expr::Name(n) if self.one(*n) == "require")
        {
            return None;
        }

        let span = match args {
            CallArgs::Str(s) => *s,

            CallArgs::Paren(list) if list.len() == 1 => match &list[0] {
                Expr::String(s) => *s,

                _ => return None,
            },

            _ => return None,
        };

        let TokKind::Str {
            inner_start,
            inner_end,
        } = self.toks[span.start as usize].kind
        else {
            return None;
        };

        Some(&self.src[inner_start as usize..inner_end as usize])
    }

    /*
    Emits a comment that sits on the same line as the keyword that opened
    this block.

    `block_body` cannot emit it. The comment attaches to the `do` or `then`
    that the caller printed, not to any statement. Without this method the
    comment would disappear. A lost comment is the failure that makes users
    distrust a formatter.
    */
    fn open_comment(&self, block: &Block) -> Doc<'a> {
        let lo = self.block_lo(block);
        let hi = match block.stmts.first() {
            Some(stmt) => self.tok_start(stmt.span().start),

            None => self.block_hi(block),
        };

        self.trailing(&self.trivia.split(lo, hi))
    }

    /// Emits a block indented inside its keywords. The block is always broken.
    pub(super) fn nested(&self, block: &Block) -> Doc<'a> {
        let open = self.open_comment(block);
        let body = self.block_body(block);

        if matches!(body, Doc::Nil) {
            return Doc::concat([open, Doc::Hard]);
        }

        let (before, after) = self.block_gaps(block);

        Doc::concat([open, Doc::indent(Doc::concat([before, body])), after])
    }

    /*
    Returns the breaks at the two edges of a block. The `block_newline_gaps`
    option decides them.

    By default, the emitter drops a blank line just inside `do` or just
    before `end`. Almost all users want this, and stylua does the same.
    `preserve` keeps the blank lines of the author. That style uses them to
    give a long body visual space.

    This rule applies only to the edges. The emitter keeps blank lines
    between statements in both modes, because those separate ideas. They do
    not pad a boundary.
    */
    fn block_gaps(&self, block: &Block) -> (Doc<'a>, Doc<'a>) {
        if self.cfg.block_newline_gaps == BlockNewlineGaps::Never {
            return (Doc::Hard, Doc::Hard);
        }

        let lo = self.block_lo(block);
        let hi = self.block_hi(block);

        let first = block
            .stmts
            .iter()
            .find(|s| !matches!(s, Stmt::Empty(_)))
            .map_or(hi, |s| self.tok_start(s.span().start));

        let last = block
            .stmts
            .iter()
            .rev()
            .find(|s| !matches!(s, Stmt::Empty(_)))
            .map_or(lo, |s| self.tok_end(s.span().end - 1));

        let gap = |from: u32, to: u32| match self.trivia.blank_between(from, to) {
            true => Doc::Blank,

            false => Doc::Hard,
        };

        (gap(lo, first), gap(last, hi))
    }

    /*
    Emits a block that can collapse onto one line.

    `collapse_simple_statement` applies only to a block that holds exactly
    one statement. That statement must have no nested block and no comments
    to lose. The check is narrow on purpose. A collapse of anything else
    hides code.
    */
    pub(super) fn collapsible(&self, block: &Block, allowed: bool) -> Doc<'a> {
        if !allowed || block.stmts.len() != 1 || !is_simple(&block.stmts[0]) {
            return self.nested(block);
        }

        let stmt = &block.stmts[0];
        let span = stmt.span();

        // A comment at any position in the block prevents the collapse. The comment would move.
        if !self
            .trivia
            .between(self.block_lo(block), self.block_hi(block))
            .is_empty()
        {
            return self.nested(block);
        }

        // A blank line from the author also prevents it. That line is an intended separation.
        if self
            .trivia
            .blank_between(self.block_lo(block), self.tok_start(span.start))
        {
            return self.nested(block);
        }

        Doc::concat([
            Doc::indent(Doc::concat([Doc::Line, self.stmt(stmt)])),
            Doc::Line,
        ])
    }

    pub(super) fn collapse_functions(&self) -> bool {
        matches!(
            self.cfg.collapse_simple_statement,
            CollapseSimpleStatement::FunctionOnly | CollapseSimpleStatement::Always
        )
    }

    pub(super) fn collapse_conditionals(&self) -> bool {
        matches!(
            self.cfg.collapse_simple_statement,
            CollapseSimpleStatement::ConditionalOnly | CollapseSimpleStatement::Always
        )
    }
}

/// One statement with the trivia bound to it. So a reorder carries both.
struct Piece<'s, 'a> {
    stmt: &'s Stmt,
    /// A blank line before it, as the author left one.
    blank_before: bool,
    /// Comments on their own lines above it.
    leading: Vec<Comment>,
    /// A comment after it on the same line.
    trailing: Option<Comment>,
    /// The require path, when this is a plain `local x = require("path")`.
    key: Option<&'a str>,
}

/// Comments after the last statement. They belong to no statement.
struct Tail {
    leading: Vec<Comment>,
    blank_before: bool,
}

/// The target kind of a require path. `by-kind` groups on this kind.
#[derive(PartialEq, Eq, PartialOrd, Ord, Clone, Copy)]
enum PathKind {
    /// `@pkg/thing`, a name that the project configured.
    Alias,
    /// `game/ReplicatedStorage/thing`, rooted at a known place.
    Absolute,
    /// `./sibling`, resolved against this file.
    Relative,
}

fn path_kind(path: &str) -> PathKind {
    if path.starts_with('@') {
        return PathKind::Alias;
    }

    if path.starts_with("./") || path.starts_with("../") {
        return PathKind::Relative;
    }

    PathKind::Absolute
}

/*
Sorts each run of adjacent requires.

A run ends at each statement that is not a plain require, and at a blank
line. The blank line rule is important. An author who separated two groups of
requires with a blank line already grouped them. A sort across that line
would discard a decision, not tidy one. So larvae sorts each run within
itself and never merges it with its neighbor.

This function never moves a require past a statement that is not a require.
So a module whose requires must run in a set order relative to other code
keeps that order. The function assumes that the order between two adjacent
requires does not matter. That assumption is the reason the feature is off
unless the user enables it.
*/
fn sort_requires(pieces: &mut [Piece<'_, '_>], grouping: RequireGrouping) {
    let mut start = 0;

    while start < pieces.len() {
        if pieces[start].key.is_none() {
            start += 1;

            continue;
        }

        let mut end = start + 1;

        while end < pieces.len() && pieces[end].key.is_some() && !pieces[end].blank_before {
            end += 1;
        }

        if end - start > 1 {
            sort_run(&mut pieces[start..end], grouping);
        }

        start = end;
    }
}

fn sort_run(run: &mut [Piece<'_, '_>], grouping: RequireGrouping) {
    // The gap before the run belongs to the position of the run, not to its first member.
    let opening_gap = run[0].blank_before;

    run.sort_by(|a, b| {
        let (a_key, b_key) = (a.key.unwrap_or(""), b.key.unwrap_or(""));

        match grouping {
            RequireGrouping::Flat => a_key.cmp(b_key),

            RequireGrouping::ByKind => path_kind(a_key)
                .cmp(&path_kind(b_key))
                .then_with(|| a_key.cmp(b_key)),
        }
    });

    for piece in run.iter_mut() {
        piece.blank_before = false;
    }

    if grouping == RequireGrouping::ByKind {
        for i in 1..run.len() {
            let (prev, this) = (run[i - 1].key.unwrap_or(""), run[i].key.unwrap_or(""));

            if path_kind(prev) != path_kind(this) {
                run[i].blank_before = true;
            }
        }
    }

    run[0].blank_before = opening_gap;
}
