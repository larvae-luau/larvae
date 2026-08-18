/*!
The emitter turns the tree into a layout document.

This module knows Luau and knows nothing about widths. The emitter expresses
each possible line break as a group. `doc::render` decides which groups break.
This split lets a nested call stay on one line inside a call that broke.
Hand-written formatters usually get this wrong.

Two places print source verbatim instead of a rebuild, and both are
intentional. The tree stores types as token spans, because larvae parses types
but does not have to interpret them. A long comment carries its own line
breaks, and new indentation would corrupt them. Verbatim output is still
normalized: the emitter replays the tokens and collapses the whitespace
between them. So a type reads the same in each written form.
*/

use std::borrow::Cow;

use crate::syntax::ast::*;
use crate::syntax::lexer::{Tok, TokKind};

use super::config::{
    BlockNewlineGaps, CallParens, CollapseSimpleStatement, FmtConfig, IfExpansion, IfPlacement,
    IfStyle, QuoteStyle, RequireGrouping, Semicolons,
};
use super::doc::Doc;
use super::trivia::{Attached, Comment, Trivia};

pub struct Emitter<'a> {
    src: &'a str,
    toks: &'a [Tok],
    trivia: &'a Trivia<'a>,
    cfg: &'a FmtConfig,
    /// The keywords that `require_binding` decided to change, by their token index
    rebindings: super::rebind::Rebindings,
    /// Byte ranges that a `fmt off` flag holds the formatter out of
    ignored: Vec<(u32, u32)>,
    /*
    The count of `if` expressions that enclose the one the emitter is on.

    An `if` expression inside another one follows a rule of its own, so the
    emitter has to know which it has. The whole emitter runs behind `&self`,
    because a document borrows the source. A counter is the one piece of
    state the walk carries, so it sits in a `Cell` rather than turn every
    method into `&mut self`.
    */
    if_depth: std::cell::Cell<usize>,
}

impl<'a> Emitter<'a> {
    pub fn new(
        src: &'a str,
        toks: &'a [Tok],
        trivia: &'a Trivia<'a>,
        cfg: &'a FmtConfig,
        rebindings: super::rebind::Rebindings,
    ) -> Self {
        Self {
            src,
            toks,
            trivia,
            cfg,
            rebindings,
            ignored: Vec::new(),
            if_depth: std::cell::Cell::new(0),
        }
    }

    /// Holds the emitter out of these byte ranges
    pub fn ignoring(mut self, ranges: Vec<(u32, u32)>) -> Self {
        self.ignored = ranges;

        self
    }

    /*
    The source of a statement that a `fmt off` flag covers, or None.

    The emitter writes those bytes instead of a rebuild. The first line loses
    its indentation, because the document supplies that, and every later line
    keeps the indentation the author gave it. The renderer treats a text with
    newlines in it as written, so the shape of the block survives.
    */
    fn verbatim_stmt(&self, stmt: &Stmt) -> Option<&'a str> {
        let (lo, hi) = self.byte_span(stmt.span());

        if !crate::flags::within(&self.ignored, lo) {
            return None;
        }

        // take whole lines, so a trailing comment on the last line comes too
        let hi = match self.src[hi as usize..].find('\n') {
            Some(n) => hi + n as u32,

            None => self.src.len() as u32,
        };

        Some(self.src[lo as usize..hi as usize].trim_end_matches([' ', '\t']))
    }

    pub fn chunk(&self, chunk: &Chunk) -> Doc<'a> {
        let body = self.block_body(&chunk.block);

        // Exactly one newline ends the file. The renderer does not add it.
        match self.cfg.final_newline {
            true => Doc::concat([body, Doc::Hard]),

            false => body,
        }
    }

    // --- tokens ------------------------------------------------------------

    fn tok(&self, i: u32) -> &'a str {
        self.toks[i as usize].text(self.src)
    }

    /// Returns the text of a one-token span.
    fn one(&self, span: TokSpan) -> &'a str {
        self.tok(span.start)
    }

    fn tok_start(&self, i: u32) -> u32 {
        self.toks[i as usize].start
    }

    fn tok_end(&self, i: u32) -> u32 {
        self.toks[i as usize].end
    }

    /*
    Rebuilds a type from its tokens. The emitter decides the spacing instead
    of a copy of it.

    The tree keeps types as token spans, because larvae parses types and does
    not have to interpret them. That is enough to format them. The two adjacent
    tokens fully decide the spacing in a type. So a pairwise rule reaches the
    same answer as a type tree, without the cost of one.

    This method cannot wrap a type across lines. So a very long type stays on
    one line. That is a real limit, and the reason this is not full formatting.
    */
    fn verbatim(&self, span: TokSpan) -> Cow<'a, str> {
        if span.is_empty() {
            return Cow::Borrowed("");
        }

        let (lo, hi) = self.byte_span(span);
        let flat = &self.src[lo as usize..hi as usize];

        /*
        A comment inside a type cannot survive a token replay, because
        comments are not tokens. The replay has no structure to attach a
        comment to, so there is no good position to put one back. So the
        emitter outputs the region exactly as the author wrote it.
        */
        if !self.trivia.between(lo, hi).is_empty() {
            return Cow::Borrowed(flat);
        }

        let mut out = String::with_capacity(flat.len());

        for i in span.start..span.end {
            if i > span.start && needs_space(self.tok(i - 1), self.tok(i)) {
                out.push(' ');
            }

            out.push_str(self.tok(i));
        }

        if out == flat {
            return Cow::Borrowed(flat);
        }

        Cow::Owned(out)
    }

    /*
    A type, with each table type inside it given a layout.

    The replay in `verbatim` keeps a type on one line. This method replays
    the same tokens, and treats each `{ ... }` region as a body: flat when
    its flat form fits `table_types.width`, one field per line otherwise.
    The separator between fields follows the option in both layouts.

    A comment inside the span keeps the author's text unchanged, for the
    reason `verbatim` states: a replay has no position to put one back.
    */
    fn type_doc(&self, span: TokSpan) -> Doc<'a> {
        if span.is_empty() || !self.cfg.table_types.enabled {
            return Doc::text(self.verbatim(span));
        }

        let (lo, hi) = self.byte_span(span);

        if !self.trivia.between(lo, hi).is_empty() {
            return Doc::text(self.verbatim(span));
        }

        self.type_region(span.start, span.end)
    }

    /// The tokens of one type stretch: tables laid out, the rest replayed
    fn type_region(&self, from: u32, to: u32) -> Doc<'a> {
        let mut parts: Vec<Doc<'a>> = Vec::new();
        let mut flat = String::new();
        let mut prev: Option<u32> = None;
        let mut i = from;

        while i < to {
            let spaced = prev.is_some_and(|p| needs_space(self.tok(p), self.tok(i)));

            if self.tok(i) == "{"
                && let Some(close) = self.matching_brace(i, to)
            {
                if spaced {
                    flat.push(' ');
                }

                if !flat.is_empty() {
                    parts.push(Doc::text(std::mem::take(&mut flat)));
                }

                parts.push(self.table_type(i, close));
                prev = Some(close);
                i = close + 1;

                continue;
            }

            if spaced {
                flat.push(' ');
            }

            flat.push_str(self.tok(i));
            prev = Some(i);
            i += 1;
        }

        if !flat.is_empty() {
            parts.push(Doc::text(flat));
        }

        Doc::concat(parts)
    }

    /// Finds the `}` that closes the `{` at this token, inside the bound
    fn matching_brace(&self, open: u32, to: u32) -> Option<u32> {
        let mut depth = 0usize;

        for i in open..to {
            match self.tok(i) {
                "{" => depth += 1,

                "}" => {
                    depth -= 1;

                    if depth == 0 {
                        return Some(i);
                    }
                }

                _ => {}
            }
        }

        None
    }

    /*
    One table type, `{` fields `}`.

    The fields split at the separators of the top level. Depth counts every
    bracket pair, and the generic brackets count by character, because the
    lexer reads the `>>` at the end of a nested generic as one token. A `->`
    holds a `>` too and closes nothing, so the count reads only the runs
    that are all one character.

    The width measures the flat form of this table alone. So a short table
    nested inside a long one keeps its line, and a long inner one opens its
    parent with it, because a parent holding a broken child cannot stay
    flat.
    */
    fn table_type(&self, open: u32, close: u32) -> Doc<'a> {
        let cfg = &self.cfg.table_types;

        let mut fields: Vec<Doc<'a>> = Vec::new();
        let mut depth = 0i32;
        let mut start = open + 1;

        for i in open + 1..close {
            let t = self.tok(i);

            match t {
                "{" | "(" | "[" => depth += 1,

                "}" | ")" | "]" => depth -= 1,

                "," | ";" if depth == 0 => {
                    if start < i {
                        fields.push(self.type_region(start, i));
                    }

                    start = i + 1;
                }

                _ if t.bytes().all(|b| b == b'<') => depth += t.len() as i32,

                _ if t.bytes().all(|b| b == b'>') => depth -= t.len() as i32,

                _ => {}
            }
        }

        if start < close {
            fields.push(self.type_region(start, close));
        }

        if fields.is_empty() {
            return Doc::text("{}");
        }

        let sep = cfg.separator.text();

        // `{ ` and ` }` around the fields, and a separator with a space between them
        let mut width = Some(4 + (fields.len() - 1) * (sep.len() + 1));

        for field in &fields {
            width = match (width, field.flat_width()) {
                (Some(sum), Some(w)) => Some(sum + w),

                _ => None,
            };
        }

        if width.is_some_and(|w| w <= cfg.width) {
            return Doc::concat([
                Doc::text("{ "),
                Doc::join(Doc::text(format!("{sep} ")), fields),
                Doc::text(" }"),
            ]);
        }

        let broken = fields
            .into_iter()
            .map(|field| Doc::concat([Doc::Hard, field, Doc::text(sep)]));

        Doc::concat([
            Doc::text("{"),
            Doc::indent(Doc::concat(broken)),
            Doc::Hard,
            Doc::text("}"),
        ])
    }

    /// Reports if the author left a newline between two adjacent tokens.
    fn newline_between(&self, a: u32, b: u32) -> bool {
        let (lo, hi) = (self.tok_end(a) as usize, self.tok_start(b) as usize);

        lo < hi && self.src[lo..hi].contains('\n')
    }

    /// Reports if this expression prints with a `-` first. A `-` cannot touch another `-`.
    fn starts_with_minus(&self, e: &Expr) -> bool {
        self.tok(e.span().start) == "-"
    }

    /// Finds the `)` that closes the `(` at this token index. It counts depth on the way.
    fn matching_paren(&self, open: u32) -> Option<u32> {
        if self.toks.get(open as usize).is_none() || self.tok(open) != "(" {
            return None;
        }

        let mut depth = 0usize;

        for i in open..self.toks.len() as u32 {
            match self.tok(i) {
                "(" => depth += 1,

                ")" => {
                    depth -= 1;

                    if depth == 0 {
                        return Some(i);
                    }
                }

                _ => {}
            }
        }

        None
    }

    /// Returns the token span of one table field. The tree does not store this span directly.
    fn field_span(&self, field: &TableField) -> TokSpan {
        match field {
            TableField::Positional(e) => e.span(),

            // The key token comes first. The span of the value ends the field.
            TableField::Named { name, value } => {
                TokSpan::new(name.start as usize, value.span().end as usize)
            }

            TableField::Computed { key, value } => TokSpan::new(
                // The `[` sits one token before the key.
                key.span().start.saturating_sub(1) as usize,
                value.span().end as usize,
            ),
        }
    }

    /// Returns the byte range that a token span covers.
    fn byte_span(&self, span: TokSpan) -> (u32, u32) {
        (self.tok_start(span.start), self.tok_end(span.end - 1))
    }

    /// Reports if a statement opens with `(`. Such a statement would continue the line above.
    fn starts_with_paren(&self, stmt: &Stmt) -> bool {
        self.tok(stmt.span().start) == "("
    }

    /*
    The semicolon that follows a statement, if any.

    Two rules meet here, and the second one outranks the first. `semicolons`
    says what the project wants. Luau says what the next statement needs: one
    that opens with `(` continues this line as a call, so `local a = b`
    followed by `(c)()` reads as `local a = b(c)()`. The emitter drops the
    author's semicolons like any other trivia, so it has to put that one back
    whatever the setting is.

    Reading it from the *next* statement rather than prefixing it to that
    statement is what lets one function answer both questions, and it is also
    where the separator belongs: it terminates the statement before it.
    */
    fn terminator(&self, stmt: &Stmt, next: Option<&Stmt>) -> Doc<'a> {
        if self.cfg.semicolons == Semicolons::Always {
            return Doc::text(";");
        }

        // `return` ends a block, so nothing can follow it to need separating
        if matches!(stmt, Stmt::Return(_)) {
            return Doc::Nil;
        }

        match next.is_some_and(|n| self.starts_with_paren(n)) {
            true => Doc::text(";"),

            false => Doc::Nil,
        }
    }

    /// Reports if this expression prints with a `[` first. A `[` cannot touch another `[`.
    fn starts_with_bracket(&self, e: &Expr) -> bool {
        self.tok(e.span().start).starts_with('[')
    }

    /// Reports if the token before this closing bracket is a comma.
    fn has_trailing_comma(&self, close: u32) -> bool {
        close > 0 && self.tok(close - 1) == ","
    }

    /*
    Reports if an expression absorbs a break instead of a move to its own
    line.

    Tables and functions hang, because one extra indent level gives no
    benefit. A `[[ ]]` string or a multi-line interpolation hangs for a harder
    reason. It carries its own newlines, so it forces each group around it to
    break. A break at the punctuation before it can never make it fit. Without
    this rule, `f[[...]]` gains parentheses on one pass and then breaks at
    them on the next pass. Such a formatter never reaches a stable output.
    */
    fn hangs(&self, e: &Expr) -> bool {
        match e {
            Expr::Table { .. } | Expr::Function { .. } => true,

            Expr::String(s) | Expr::InterpString(s) => self.one(*s).contains('\n'),

            _ => false,
        }
    }

    // --- comments ----------------------------------------------------------

    fn comment_doc(&self, c: Comment) -> Doc<'a> {
        Doc::text(c.text(self.src))
    }

    fn trailing(&self, att: &Attached<'_>) -> Doc<'a> {
        match att.trailing {
            Some(c) => Doc::concat([Doc::text(" "), self.comment_doc(c)]),

            None => Doc::Nil,
        }
    }

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

    fn trailing_doc(&self, comment: Option<Comment>) -> Doc<'a> {
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
    fn nested(&self, block: &Block) -> Doc<'a> {
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
    fn collapsible(&self, block: &Block, allowed: bool) -> Doc<'a> {
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

    fn collapse_functions(&self) -> bool {
        matches!(
            self.cfg.collapse_simple_statement,
            CollapseSimpleStatement::FunctionOnly | CollapseSimpleStatement::Always
        )
    }

    fn collapse_conditionals(&self) -> bool {
        matches!(
            self.cfg.collapse_simple_statement,
            CollapseSimpleStatement::ConditionalOnly | CollapseSimpleStatement::Always
        )
    }

    // --- statements --------------------------------------------------------

    fn stmt(&self, stmt: &Stmt) -> Doc<'a> {
        match stmt {
            Stmt::Empty(_) => Doc::Nil,

            Stmt::Local(n) => self.local(n),

            Stmt::Assign(n) => self.assign(n),

            Stmt::Call(e, _) => self.expr(e),

            Stmt::Do(n) => Doc::group(Doc::concat([
                Doc::text("do"),
                self.nested(&n.block),
                Doc::text("end"),
            ])),

            Stmt::While(n) => Doc::group(Doc::concat([
                Doc::text("while "),
                self.expr(&n.cond),
                Doc::text(" do"),
                self.nested(&n.block),
                Doc::text("end"),
            ])),

            Stmt::Repeat(n) => Doc::group(Doc::concat([
                Doc::text("repeat"),
                self.nested(&n.block),
                Doc::text("until "),
                self.expr(&n.cond),
            ])),

            Stmt::If(n) => self.if_stmt(n),

            Stmt::NumericFor(n) => self.numeric_for(n),

            Stmt::GenericFor(n) => self.generic_for(n),

            Stmt::Function(n) => self.function(n),

            Stmt::LocalFunction(n) => self.local_function(n),

            Stmt::Return(n) => self.ret(n),

            Stmt::Break(_) => Doc::text("break"),

            Stmt::Continue(_) => Doc::text("continue"),

            /*
            The tree keeps only the name of the alias. So the rest prints
            from its tokens. That works for a type, because a type is the
            punctuation that the pairwise spacing rules cover. It is wrong
            for `type function f()`, whose body is arbitrary Luau. A replay
            drops the newlines and joins `1` with `return`, which makes a
            malformed number.

            So the emitter outputs an alias that covers more than one line
            exactly as the author wrote it. That also keeps its comments. A
            token replay can never keep them.
            */
            Stmt::TypeAlias(n) => {
                let (lo, hi) = self.byte_span(n.span);
                let raw = &self.src[lo as usize..hi as usize];

                /*
                `type function f()` holds arbitrary Luau, and a replay of
                its tokens joins lines into malformed code. It prints as
                the author wrote it, whatever the options say.
                */
                let s = n.span.start;
                let type_function = self.tok(s + 1) == "function"
                    || (self.tok(s) == "export" && self.tok(s + 2) == "function");

                if !type_function && self.cfg.table_types.enabled {
                    // `type_doc` keeps the author's text when a comment sits inside.
                    return self.type_doc(n.span);
                }

                match raw.contains('\n') {
                    true => Doc::text(raw),

                    false => Doc::text(self.verbatim(n.span)),
                }
            }
        }
    }

    fn binding(&self, b: &Binding) -> Doc<'a> {
        match b.ty {
            Some(ty) => Doc::concat([
                Doc::text(self.one(b.name)),
                Doc::text(": "),
                self.type_doc(ty),
            ]),

            None => Doc::text(self.one(b.name)),
        }
    }

    fn local(&self, n: &Local) -> Doc<'a> {
        // `require_binding` can decide that this declaration says the other keyword.
        let keyword = self
            .rebindings
            .get(&n.keyword.start)
            .copied()
            .unwrap_or_else(|| self.one(n.keyword));
        let names = Doc::join(Doc::text(", "), n.names.iter().map(|b| self.binding(b)));

        if n.values.is_empty() {
            return Doc::concat([Doc::text(keyword), Doc::text(" "), names]);
        }

        Doc::group(Doc::concat([
            Doc::text(keyword),
            Doc::text(" "),
            names,
            self.assigned(&n.values),
        ]))
    }

    fn assign(&self, n: &Assign) -> Doc<'a> {
        let targets = Doc::join(Doc::text(", "), n.targets.iter().map(|e| self.expr(e)));

        Doc::group(Doc::concat([
            targets,
            Doc::text(" "),
            Doc::text(self.one(n.op)),
            self.values(&n.values),
        ]))
    }

    /// Emits ` = a, b`. It breaks after the `=` when the values do not fit.
    fn assigned(&self, values: &[Expr]) -> Doc<'a> {
        Doc::concat([Doc::text(" ="), self.values(values)])
    }

    fn values(&self, values: &[Expr]) -> Doc<'a> {
        let list = Doc::join(
            Doc::concat([Doc::text(","), Doc::Line]),
            values.iter().map(|e| self.expr(e)),
        );

        /*
        One value hangs off the `=` instead of a move to its own line.

        A break there gives no benefit. A lone value that does not fit also
        does not fit one indent level deeper. The value breaks inside itself
        instead. Two or more values are a list. A list that does not fit
        deserves its own lines.
        */
        if values.len() == 1 {
            /*
            An `if` expression that opens is the one exception, and only when
            the user asks for it.

            The test is the document and not the option, because the option
            says `next-line` for an expression that stays on one line as
            well. A document that cannot go flat is one that `if_else`
            settled on opening. So the break after the `=` follows the
            expression, and the two never disagree.
            */
            let next_line = self.cfg.if_expression.placement == IfPlacement::NextLine
                && matches!(values[0], Expr::IfElse { .. })
                && list.flat_width().is_none();

            if next_line {
                return self.indented(vec![Doc::Hard, list]);
            }

            return Doc::concat([Doc::text(" "), list]);
        }

        Doc::group(Doc::indent(Doc::concat([Doc::Line, list])))
    }

    fn if_stmt(&self, n: &If) -> Doc<'a> {
        let mut parts = Vec::with_capacity(n.branches.len() * 4 + 3);
        let collapse =
            self.collapse_conditionals() && n.branches.len() == 1 && n.else_block.is_none();

        for (i, (cond, block)) in n.branches.iter().enumerate() {
            parts.push(Doc::text(if i == 0 { "if " } else { "elseif " }));
            parts.push(self.expr(cond));
            parts.push(Doc::text(" then"));
            parts.push(self.collapsible(block, collapse));
        }

        if let Some(block) = &n.else_block {
            parts.push(Doc::text("else"));
            parts.push(self.nested(block));
        }

        parts.push(Doc::text("end"));

        Doc::group(Doc::concat(parts))
    }

    fn numeric_for(&self, n: &NumericFor) -> Doc<'a> {
        let mut head = vec![
            Doc::text("for "),
            self.binding(&n.var),
            Doc::text(" = "),
            self.expr(&n.start),
            Doc::text(", "),
            self.expr(&n.limit),
        ];

        if let Some(step) = &n.step {
            head.push(Doc::text(", "));
            head.push(self.expr(step));
        }

        head.push(Doc::text(" do"));
        head.push(self.nested(&n.block));
        head.push(Doc::text("end"));

        Doc::group(Doc::concat(head))
    }

    fn generic_for(&self, n: &GenericFor) -> Doc<'a> {
        Doc::group(Doc::concat([
            Doc::text("for "),
            Doc::join(Doc::text(", "), n.vars.iter().map(|b| self.binding(b))),
            Doc::text(" in "),
            Doc::join(Doc::text(", "), n.exprs.iter().map(|e| self.expr(e))),
            Doc::text(" do"),
            self.nested(&n.block),
            Doc::text("end"),
        ]))
    }

    fn attributes(&self, attrs: &[TokSpan]) -> Doc<'a> {
        if attrs.is_empty() {
            return Doc::Nil;
        }

        Doc::concat(
            attrs
                .iter()
                .map(|a| Doc::concat([Doc::text(self.verbatim(*a)), Doc::Hard])),
        )
    }

    fn function(&self, n: &Function) -> Doc<'a> {
        let mut path = String::new();

        for (i, part) in n.path.iter().enumerate() {
            if i > 0 {
                let last = i + 1 == n.path.len();

                path.push(if last && n.is_method { ':' } else { '.' });
            }

            path.push_str(self.one(*part));
        }

        Doc::concat([
            self.attributes(&n.attributes),
            Doc::text("function "),
            Doc::text(path),
            self.function_body(&n.body),
        ])
    }

    fn local_function(&self, n: &LocalFunction) -> Doc<'a> {
        Doc::concat([
            self.attributes(&n.attributes),
            Doc::text(if n.is_const {
                "const function "
            } else {
                "local function "
            }),
            Doc::text(self.one(n.name)),
            self.function_body(&n.body),
        ])
    }

    /// Emits `<T>(a: A, b: B): R <block> end`, which is everything after the name.
    fn function_body(&self, body: &FunctionBody) -> Doc<'a> {
        let mut parts = Vec::with_capacity(6);

        if let Some(generics) = body.generics {
            parts.push(Doc::text(self.verbatim(generics)));
        }

        if self.cfg.space_before_definition_parens() {
            parts.push(Doc::text(" "));
        }

        let open = match body.generics {
            Some(generics) => generics.end,

            None => body.span.start,
        };

        parts.push(self.params(&body.params, open));

        if let Some(ret) = body.ret_type {
            parts.push(Doc::text(": "));
            parts.push(self.type_doc(ret));
        }

        parts.push(self.collapsible(&body.block, self.collapse_functions()));
        parts.push(Doc::text("end"));

        Doc::group(Doc::concat(parts))
    }

    fn params(&self, params: &[Param], open: u32) -> Doc<'a> {
        /*
        The reason is the same as for a type. The tree has no place to attach
        a comment written between two parameters. So the emitter outputs a
        list that holds one as written, instead of a loss of the comment.
        */
        if let Some(close) = self.matching_paren(open) {
            let (lo, hi) = (self.tok_start(open), self.tok_end(close));

            if !self.trivia.between(lo, hi).is_empty() {
                return Doc::text(&self.src[lo as usize..hi as usize]);
            }
        }

        if params.is_empty() {
            return Doc::text("()");
        }

        let each = params.iter().map(|p| match p.ty {
            Some(ty) => Doc::concat([
                Doc::text(self.one(p.name)),
                Doc::text(": "),
                self.type_doc(ty),
            ]),

            None => Doc::text(self.one(p.name)),
        });

        self.bracketed(
            "(",
            ")",
            Doc::join(Doc::concat([Doc::text(","), Doc::Line]), each),
            false,
        )
    }

    fn ret(&self, n: &Return) -> Doc<'a> {
        if n.values.is_empty() {
            return Doc::text("return");
        }

        let list = Doc::join(
            Doc::concat([Doc::text(","), Doc::Line]),
            n.values.iter().map(|e| self.expr(e)),
        );

        if n.values.len() == 1 {
            return Doc::concat([Doc::text("return "), list]);
        }

        Doc::group(Doc::concat([
            Doc::text("return"),
            Doc::indent(Doc::concat([Doc::Line, list])),
        ]))
    }

    // --- expressions -------------------------------------------------------

    pub(crate) fn expr(&self, e: &Expr) -> Doc<'a> {
        match e {
            Expr::Nil(_) => Doc::text("nil"),

            Expr::True(_) => Doc::text("true"),

            Expr::False(_) => Doc::text("false"),

            Expr::Vararg(_) => Doc::text("..."),

            Expr::Number(s) => Doc::text(self.one(*s)),

            Expr::String(s) => self.string(*s),

            Expr::InterpString(s) => Doc::text(self.one(*s)),

            Expr::Name(s) => Doc::text(self.one(*s)),

            Expr::Function {
                attributes, body, ..
            } => Doc::concat([
                self.attributes(attributes),
                Doc::text("function"),
                self.function_body(body),
            ]),

            Expr::Table { fields, span } => self.table(fields, *span),

            Expr::Binary { .. } => self.binary(e),

            Expr::Unary { op, operand, .. } => {
                let op = self.one(*op);

                /*
                `not x` needs its space, and `#x` must not have one. That is
                the easy part. The hard part is `- -x`. Two minus signs
                together spell a line comment. The rest of the line then
                disappears, and the file stops parsing. A space between them
                is not cosmetic.
                */
                let space = op == "not" || (op == "-" && self.starts_with_minus(operand));

                Doc::concat([
                    Doc::text(op),
                    Doc::text(if space { " " } else { "" }),
                    self.expr(operand),
                ])
            }

            Expr::Paren { inner, .. } => {
                let doc = self.expr(inner);

                /*
                A parenthesised `if` expression that opens takes the shape of
                the parentheses with it.

                Without this the `(` sits against the `if` and the `)` sits
                against the last value, and the reader has to find where the
                expression starts and stops inside a line that is already
                broken over four of them. A table and a function body already
                read this way.

                Only an `if` that opened is treated so. One that stays on a
                line keeps its parentheses against it, which is what every
                other parenthesised expression does.
                */
                if matches!(**inner, Expr::IfElse { .. }) && doc.flat_width().is_none() {
                    return Doc::concat([
                        Doc::text("("),
                        Doc::indent(Doc::concat([Doc::Hard, doc])),
                        Doc::Hard,
                        Doc::text(")"),
                    ]);
                }

                Doc::concat([Doc::text("("), doc, Doc::text(")")])
            }

            Expr::Index { object, key, .. } => match key {
                IndexKey::Field(name) => Doc::concat([
                    self.expr(object),
                    Doc::text("."),
                    Doc::text(self.one(*name)),
                ]),

                IndexKey::Computed(k) => Doc::concat([
                    self.expr(object),
                    self.bracketed(
                        "[",
                        "]",
                        self.expr(k),
                        // A `[` that touches a `[[ ]]` string opens a long string instead.
                        self.cfg.space_inside_brackets || self.starts_with_bracket(k),
                    ),
                ]),
            },

            Expr::Call {
                func,
                method,
                type_args,
                args,
                span,
            } => self.call(func, *method, *type_args, args, *span),

            Expr::IfElse {
                branches,
                else_value,
                ..
            } => self.if_else(branches, else_value),

            Expr::TypeAssert { expr, ty, .. } => {
                Doc::concat([self.expr(expr), Doc::text(" :: "), self.type_doc(*ty)])
            }
        }
    }

    /*
    Emits `if c then a else b`, the expression and not the statement.

    Two layouts exist. The flat one is what larvae always wrote, and it is
    still the default. The open one puts each keyword at the start of a line
    and each value below its keyword:

    ```
    if cond then
        first
    else
        second
    ```

    `if_expression.expand` selects between them. The open layout uses a hard
    break, so the choice happens here and not in the renderer. The renderer
    decides by width alone, and it cannot know that a user asked for the open
    layout at every width.
    */
    fn if_else(&self, branches: &[(Expr, Expr)], else_value: &Expr) -> Doc<'a> {
        let cfg = &self.cfg.if_expression;

        let nested = self.if_depth.get() > 0;
        self.if_depth.set(self.if_depth.get() + 1);

        /*
        The old layout stays byte for byte where the option is off.

        It indents the whole expression and breaks once, before the `else`.
        That shape is not the shape below, so a rewrite of it would move the
        output of every project that never asked for this option.
        */
        if cfg.expand == IfExpansion::Never {
            let mut parts = Vec::with_capacity(branches.len() * 5 + 2);

            for (i, (cond, value)) in branches.iter().enumerate() {
                parts.push(Doc::text(if i == 0 { "if " } else { "elseif " }));
                parts.push(self.expr(cond));
                parts.push(Doc::text(" then "));
                parts.push(self.expr(value));
                parts.push(Doc::Line);
            }

            parts.push(Doc::text("else "));
            parts.push(self.expr(else_value));

            self.if_depth.set(self.if_depth.get() - 1);

            return Doc::group(Doc::indent(Doc::concat(parts)));
        }

        /*
        The children are emitted once, and the width comes from them.

        The choice needs the flat width, and the flat width is the width of
        the children plus the keywords around them. Measuring the children
        alone lets the emitter build the document one time, with the break it
        settled on. An assemble, measure, and re-assemble would cost one more
        build for every level of nesting.
        */
        let parts: Vec<(Doc<'a>, Doc<'a>)> = branches
            .iter()
            .map(|(cond, value)| (self.expr(cond), self.expr(value)))
            .collect();

        let other = self.expr(else_value);

        self.if_depth.set(self.if_depth.get() - 1);

        // `if ` + cond + ` then ` + value per branch, and ` else ` + the last value
        let mut flat = other.flat_width().map(|w| w + " else ".len());

        for (i, (cond, value)) in parts.iter().enumerate() {
            let keyword = match i {
                0 => "if ".len(),

                _ => " elseif ".len(),
            };

            flat = match (flat, cond.flat_width(), value.flat_width()) {
                (Some(sum), Some(c), Some(v)) => Some(sum + keyword + c + " then ".len() + v),

                _ => None,
            };
        }

        /*
        A nested expression waits for the width, whatever the mode says.

        `always` at every level gives a stair of keywords for an expression
        that reads well on one line. The width is the measure of whether the
        inner one has earned its own lines.
        */
        let wide = flat.is_none_or(|w| w > cfg.width);
        let open = match cfg.expand {
            IfExpansion::Never => false,

            IfExpansion::Always => !nested || wide,

            IfExpansion::WhenLarge => wide,
        };

        // The open layout is a request, so it breaks at every width.
        let split = || match open {
            true => Doc::Hard,

            false => Doc::Line,
        };

        let mut out: Vec<Doc<'a>> = Vec::with_capacity(parts.len() * 4 + 3);

        /*
        The two shapes break on the two sides of the keyword.

        `block` ends a line after `then`, so the value sits below it. `leading`
        ends a line before `then`, so the keyword starts the line and takes
        its value. Flat, the two produce the same characters, because the
        break is one space either way.
        */
        for (i, (cond, value)) in parts.into_iter().enumerate() {
            let keyword = Doc::text(if i == 0 { "if " } else { "elseif " });

            match self.cfg.if_expression.style {
                IfStyle::Block => {
                    if i > 0 {
                        out.push(split());
                    }

                    out.push(keyword);
                    out.push(cond);
                    out.push(Doc::text(" then"));
                    out.push(self.indented(vec![split(), value]));
                }

                IfStyle::Leading => {
                    match i {
                        0 => out.push(keyword),

                        _ => out.push(self.indented(vec![split(), keyword])),
                    }

                    out.push(cond);
                    out.push(self.indented(vec![split(), Doc::text("then "), value]));
                }
            }
        }

        match self.cfg.if_expression.style {
            IfStyle::Block => {
                out.push(split());
                out.push(Doc::text("else"));
                out.push(self.indented(vec![split(), other]));
            }

            IfStyle::Leading => {
                out.push(self.indented(vec![split(), Doc::text("else "), other]));
            }
        }

        Doc::group(Doc::concat(out))
    }

    /*
    Wraps parts in the indent levels that `if_expression.indent` asks for.

    Zero levels is a valid answer, and it means the value sits at the column
    of its keyword.
    */
    fn indented(&self, parts: Vec<Doc<'a>>) -> Doc<'a> {
        (0..self.cfg.if_expression.indent).fold(Doc::concat(parts), |doc, _| Doc::indent(doc))
    }

    /*
    Emits a binary chain. The emitter flattens it, so `a + b + c` breaks as
    one list. It does not nest one level per operator.

    The operator starts its continuation line. This position makes a broken
    condition readable. The reader finds the `and` at a fixed column. The
    reader does not have to search for it at the uneven right edge.
    */
    fn binary(&self, e: &Expr) -> Doc<'a> {
        let Expr::Binary { op, .. } = e else {
            return self.expr(e);
        };

        let prec = precedence(self.one(*op));
        let mut ops: Vec<&'a str> = Vec::new();
        let mut operands: Vec<(Doc<'a>, bool)> = Vec::new();

        self.flatten_binary(e, prec, &mut ops, &mut operands);

        let count = ops.len();
        let mut operands = operands.into_iter();
        let (first, _) = operands.next().expect("a chain has a left operand");
        let mut rest = Vec::with_capacity(count * 3);
        let mut tail = Doc::Nil;

        for (i, (op, (operand, hangs))) in ops.into_iter().zip(operands).enumerate() {
            /*
            The last operand hangs off the operator instead of moving below
            it.

            `a .. (if c then x else y)` with the `if` opened reads as one
            thing that starts at the `(`. A break before the `..` would put
            the operator on a line of its own above a block that already has
            its own lines. A table argument hangs off a `=` for the same
            reason.
            */
            if i + 1 == count && hangs {
                tail = Doc::concat([Doc::text(" "), Doc::text(op), Doc::text(" "), operand]);

                break;
            }

            rest.push(Doc::Line);
            rest.push(Doc::text(op));
            rest.push(Doc::text(" "));
            rest.push(operand);
        }

        Doc::group(Doc::concat([first, Doc::indent(Doc::concat(rest)), tail]))
    }

    fn flatten_binary(
        &self,
        e: &Expr,
        prec: u8,
        ops: &mut Vec<&'a str>,
        operands: &mut Vec<(Doc<'a>, bool)>,
    ) {
        match e {
            Expr::Binary { op, lhs, rhs, .. } if precedence(self.one(*op)) == prec => {
                self.flatten_binary(lhs, prec, ops, operands);
                ops.push(self.one(*op));
                self.flatten_binary(rhs, prec, ops, operands);
            }

            _ => {
                let doc = self.expr(e);

                /*
                Only a parenthesised `if` that opened hangs here.

                A table and a function already reach this point on a line of
                their own, through paths that predate the `if` option, and
                widening the rule to them would move the output of every
                project that never asked for it.
                */
                let hangs = matches!(e, Expr::Paren { inner, .. } if matches!(**inner, Expr::IfElse { .. }))
                    && doc.flat_width().is_none();

                operands.push((doc, hangs));
            }
        }
    }

    /*
    Emits a call. This is the one place where the `call_parentheses` option
    applies.

    `f "str"` and `f {t}` are calls without parentheses. Luau allows them,
    and the option decides about them. `input` keeps the form that the author
    wrote. It is the only setting that does not enforce consistency. It
    exists because some codebases use the bare form as a DSL and the
    parenthesized form for calls.
    */
    fn call(
        &self,
        func: &Expr,
        method: Option<TokSpan>,
        type_args: Option<TokSpan>,
        args: &CallArgs,
        span: TokSpan,
    ) -> Doc<'a> {
        let mut parts = vec![self.expr(func)];

        if let Some(m) = method {
            parts.push(Doc::text(":"));
            parts.push(Doc::text(self.one(m)));
        }

        if let Some(ty) = type_args {
            parts.push(Doc::text(self.verbatim(ty)));
        }

        if self.cfg.space_before_call_parens() {
            parts.push(Doc::text(" "));
        }

        parts.push(self.call_args(args, span));

        Doc::concat(parts)
    }

    fn call_args(&self, args: &CallArgs, span: TokSpan) -> Doc<'a> {
        /*
        The option applies to the argument, not to the syntax that the author
        chose. So `f("x")` and `f "x"` are the same call, and both get the
        same form. A decision based on the written form makes a setting look
        inactive on half a codebase.
        */
        let (single, inner) = match args {
            CallArgs::Str(s) => (Some(Single::Str), self.string(*s)),

            CallArgs::Table(t) => (Some(Single::Table), self.expr(t)),

            CallArgs::Paren(list) if list.len() == 1 => match &list[0] {
                Expr::String(s) => (Some(Single::Str), self.string(*s)),

                e @ Expr::Table { .. } => (Some(Single::Table), self.expr(e)),

                _ => (None, Doc::Nil),
            },

            _ => (None, Doc::Nil),
        };

        if let Some(single) = single {
            let keep = match self.cfg.call_parentheses {
                CallParens::Always => true,

                CallParens::None => false,

                CallParens::NoSingleString => single != Single::Str,

                CallParens::NoSingleTable => single != Single::Table,

                // The choice of the author stands. This is the only setting that
                // does not make the codebase consistent, and that is why it exists.
                CallParens::Input => matches!(args, CallArgs::Paren(_)),
            };

            return if keep {
                Doc::concat([Doc::text("("), inner, Doc::text(")")])
            } else {
                Doc::concat([Doc::text(" "), inner])
            };
        }

        let CallArgs::Paren(list) = args else {
            unreachable!("the bare forms both hold exactly one argument")
        };

        /*
        A comment among the arguments forces the call to expand. The reason
        is the same as for a table. A line comment flat inside the
        parentheses would comment out the closing parenthesis and everything
        after it.
        */
        if let Some(doc) = self.commented_args(list, span) {
            return doc;
        }

        if list.is_empty() {
            return Doc::text("()");
        }

        /*
        A single function argument stays against the parentheses. The emitter
        does not indent it inside them. So a callback reads as a block, not
        as a very large list of one item.
        */
        if list.len() == 1 && self.hangs(&list[0]) {
            return Doc::concat([Doc::text("("), self.expr(&list[0]), Doc::text(")")]);
        }

        let inner = Doc::join(
            Doc::concat([Doc::text(","), Doc::Line]),
            list.iter().map(|e| self.expr(e)),
        );

        self.bracketed("(", ")", inner, self.cfg.space_inside_parens)
    }

    /*
    Emits an argument list that holds a comment: one argument per line, with
    its comments placed. It returns `None` when there are no comments, which
    is the usual case.

    Luau rejects a trailing comma in a call. So the commas go between the
    arguments, not after them. This is the one difference from the table
    case.
    */
    fn commented_args(&self, list: &[Expr], span: TokSpan) -> Option<Doc<'a>> {
        let close = span.end - 1;
        let open = self.matching_open_paren(close)?;
        let (lo, hi) = (self.tok_end(open), self.tok_start(close));

        if self.trivia.between(lo, hi).is_empty() {
            return None;
        }

        let mut parts = Vec::with_capacity(list.len() * 5 + 2);
        let mut cursor = lo;

        for (i, arg) in list.iter().enumerate() {
            let att = self.trivia.split(cursor, self.tok_start(arg.span().start));

            parts.push(self.trailing_doc(att.trailing));

            for c in att.leading {
                parts.push(Doc::Hard);
                parts.push(self.comment_doc(*c));
            }

            parts.push(Doc::Hard);
            parts.push(self.expr(arg));

            if i + 1 < list.len() {
                parts.push(Doc::text(","));
            }

            cursor = self.tok_end(arg.span().end - 1);
        }

        // These are the comments between the last argument and the closing parenthesis.
        let att = self.trivia.split(cursor, hi);
        parts.push(self.trailing_doc(att.trailing));

        for c in att.leading {
            parts.push(Doc::Hard);
            parts.push(self.comment_doc(*c));
        }

        Some(Doc::concat([
            Doc::text("("),
            Doc::indent(Doc::concat(parts)),
            Doc::Hard,
            Doc::text(")"),
        ]))
    }

    /// Finds the `(` that opens the `)` at this token index. It counts depth backwards.
    fn matching_open_paren(&self, close: u32) -> Option<u32> {
        if self.tok(close) != ")" {
            return None;
        }

        let mut depth = 0usize;

        for i in (0..=close).rev() {
            match self.tok(i) {
                ")" => depth += 1,

                "(" => {
                    depth -= 1;

                    if depth == 0 {
                        return Some(i);
                    }
                }

                _ => {}
            }
        }

        None
    }

    /*
    Emits brackets around a breakable list.

    In flat mode, the optional inner spaces apply. In broken mode, the
    contents indent one level, and the brackets sit on their own lines. The
    inner spaces then do not matter, because a newline already separates
    them.
    */
    fn bracketed(&self, open: &'a str, close: &'a str, inner: Doc<'a>, spaced: bool) -> Doc<'a> {
        let pad = if spaced { Doc::Line } else { Doc::Soft };

        Doc::group(Doc::concat([
            Doc::text(open),
            Doc::indent(Doc::concat([pad.clone(), inner])),
            pad,
            Doc::text(close),
        ]))
    }

    /*
    Emits a table.

    Two things force a table to stay expanded: a newline directly after the
    `{`, and a trailing comma on the last field. Both rules come from stylua,
    and users rely on them. Each one is a signal from the author that this
    table is a list of things and not an expression. Width alone must not
    override that signal.
    */
    fn table(&self, fields: &[TableField], span: TokSpan) -> Doc<'a> {
        if fields.is_empty() {
            // `{ -- nothing yet }` holds a comment and is not the same as `{}`.
            let inside = self
                .trivia
                .between(self.tok_end(span.start), self.tok_start(span.end - 1));

            if inside.is_empty() {
                return Doc::text("{}");
            }

            let mut parts = Vec::with_capacity(inside.len() * 2);

            for c in inside {
                parts.push(Doc::Hard);
                parts.push(self.comment_doc(*c));
            }

            return Doc::concat([
                Doc::text("{"),
                Doc::indent(Doc::concat(parts)),
                Doc::Hard,
                Doc::text("}"),
            ]);
        }

        let open = span.start;
        let close = span.end - 1;

        /*
        A comment at any position in the table forces it to expand.

        This is not a style preference. A line comment flat inside braces
        would comment out the closing brace and everything after it. So the
        choice is between an expanded table and a lost comment, and the
        comment belongs to the author.
        */
        let commented = !self
            .trivia
            .between(self.tok_end(open), self.tok_start(close))
            .is_empty();

        let expanded = commented
            || self.newline_between(open, open + 1)
            || (self.cfg.magic_trailing_comma && self.has_trailing_comma(close));

        let each: Vec<Doc<'a>> = fields
            .iter()
            .map(|f| match f {
                TableField::Positional(e) => self.expr(e),

                TableField::Named { name, value } => Doc::concat([
                    Doc::text(self.one(*name)),
                    Doc::text(" = "),
                    self.expr(value),
                ]),

                TableField::Computed { key, value } => Doc::concat([
                    self.bracketed(
                        "[",
                        "]",
                        self.expr(key),
                        self.cfg.space_inside_brackets || self.starts_with_bracket(key),
                    ),
                    Doc::text(" = "),
                    self.expr(value),
                ]),
            })
            .collect();

        if expanded {
            let mut parts = Vec::with_capacity(each.len() * 5 + 2);
            let mut cursor = self.tok_end(open);

            for (field, doc) in fields.iter().zip(each) {
                let start = self.tok_start(self.field_span(field).start);
                let att = self.trivia.split(cursor, start);

                // The trailing comment of this gap sits on the line above, not on this line.
                parts.push(self.trailing_doc(att.trailing));

                for c in att.leading {
                    parts.push(Doc::Hard);
                    parts.push(self.comment_doc(*c));
                }

                parts.push(Doc::Hard);
                parts.push(doc);
                parts.push(Doc::text(","));

                cursor = self.tok_end(self.field_span(field).end - 1);
            }

            if !self.cfg.trailing_comma {
                parts.pop();
            }

            // These are the comments between the last field and the closing brace.
            let att = self.trivia.split(cursor, self.tok_start(close));
            parts.push(self.trailing_doc(att.trailing));

            for c in att.leading {
                parts.push(Doc::Hard);
                parts.push(self.comment_doc(*c));
            }

            return Doc::concat([
                Doc::text("{"),
                Doc::indent(Doc::concat(parts)),
                Doc::Hard,
                Doc::text("}"),
            ]);
        }

        /*
        A table that the layout engine breaks still needs its trailing comma.
        Without it, a read of the output back would see an expanded table and
        add one. The formatter would then disagree with itself about its own
        output.
        */
        let comma = if self.cfg.trailing_comma {
            Doc::if_break(Doc::Nil, Doc::text(","))
        } else {
            Doc::Nil
        };

        let inner = Doc::concat([
            Doc::join(Doc::concat([Doc::text(","), Doc::Line]), each),
            comma,
        ]);

        self.bracketed("{", "}", inner, self.cfg.space_inside_braces)
    }

    // --- strings -----------------------------------------------------------

    /*
    Emits a short string, quoted again in the configured style.

    The emitter keeps long strings, `[[...]]`, exactly as written. Their
    content is literal, so there is no quote to change and nothing to escape.
    */
    fn string(&self, span: TokSpan) -> Doc<'a> {
        let raw = self.one(span);

        let TokKind::Str {
            inner_start,
            inner_end,
        } = self.toks[span.start as usize].kind
        else {
            return Doc::text(raw);
        };

        if !raw.starts_with(['"', '\'']) || self.cfg.quote_style == QuoteStyle::Preserve {
            return Doc::text(raw);
        }

        let inner = &self.src[inner_start as usize..inner_end as usize];
        let quote = self.pick_quote(inner, raw.as_bytes()[0]);

        if raw.as_bytes()[0] == quote {
            return Doc::text(raw);
        }

        Doc::text(requote(inner, quote))
    }

    fn pick_quote(&self, inner: &str, current: u8) -> u8 {
        let (doubles, singles) = count_quotes(inner);

        match self.cfg.quote_style {
            QuoteStyle::ForceDouble => b'"',

            QuoteStyle::ForceSingle => b'\'',

            // Use the configured quote, unless the other quote needs fewer escapes.
            QuoteStyle::AutoPreferDouble if doubles > singles => b'\'',

            QuoteStyle::AutoPreferDouble => b'"',

            QuoteStyle::AutoPreferSingle if singles > doubles => b'"',

            QuoteStyle::AutoPreferSingle => b'\'',

            QuoteStyle::Preserve => current,
        }
    }
}

/// Reports if a statement has no block nested inside it.
fn is_simple(stmt: &Stmt) -> bool {
    matches!(
        stmt,
        Stmt::Local(_)
            | Stmt::Assign(_)
            | Stmt::Call(..)
            | Stmt::Return(_)
            | Stmt::Break(_)
            | Stmt::Continue(_)
    )
}

/// The single argument form that a call uses. The `call_parentheses` option needs this.
#[derive(PartialEq, Eq, Clone, Copy)]
enum Single {
    Str,
    Table,
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

/*
Reports if two adjacent tokens in a type need a space between them.

Read this function as a table, not as logic. Types have much punctuation, and
each rule here is one shape that users write: `Array<T>` has no inner spaces,
`A | B` has spaces around the bar, `{ x: number }` has spaces inside its
braces, and `(T) -> U` has its arrow alone in the middle. No other part of
the language needs this, because the emitter rebuilds everything else from a
tree that already knows where its spaces go.
*/
fn needs_space(prev: &str, next: &str) -> bool {
    // No space ever sits between a name and the token that qualifies or closes it.
    if matches!(next, "," | ")" | "]" | ">" | "?" | ":" | ";" | ".") {
        return false;
    }

    if matches!(prev, "(" | "[" | "<" | "." | "..." | "@") {
        return false;
    }

    // An opening parenthesis attaches to the token before it, unless that token is an operator.
    if next == "(" {
        return matches!(prev, "," | ":" | "->" | "|" | "&" | "{" | "=");
    }

    if matches!(prev, "," | ":" | "->" | "|" | "&" | "{" | "=" | "..") {
        return true;
    }

    if matches!(next, "->" | "|" | "&" | "}" | "=" | "..") {
        return true;
    }

    if matches!(next, "<" | "[") {
        return prev == "{";
    }

    /*
    This case covers two atoms. An atom is a token that starts with a letter,
    a digit or an underscore. Words are the clear case, for example
    `typeof T`. Numbers matter for a reason that is easy to miss. A type can
    hold an expression, through `typeof(...)`. `and 1` joined spells the
    identifier `and1`, and `2 or` joined spells the malformed number `2or`.
    A test for words alone let both through.
    */
    is_atom(prev) && is_atom(next)
}

fn is_atom(tok: &str) -> bool {
    tok.starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_')
}

/*
Returns the binding power. The emitter uses it only to decide what flattens
into one breakable chain.

The output is correct at any of these values. The tree already holds the
right shape, and an infix replay reproduces it. The benefit of these values
is that `a and b or c` breaks at `or` before it breaks at `and`. Without
them, every operator in the expression would be an equally good fold point.
*/
fn precedence(op: &str) -> u8 {
    match op {
        "or" => 1,
        "and" => 2,
        "<" | ">" | "<=" | ">=" | "~=" | "==" => 3,
        ".." => 4,
        "+" | "-" => 5,
        "*" | "/" | "//" | "%" => 6,
        "^" => 7,
        _ => 0,
    }
}

/// Counts the unescaped quote characters of each kind.
fn count_quotes(inner: &str) -> (usize, usize) {
    let (mut doubles, mut singles) = (0, 0);
    let mut escaped = false;

    for c in inner.chars() {
        match c {
            _ if escaped => escaped = false,

            '\\' => escaped = true,

            '"' => doubles += 1,

            '\'' => singles += 1,

            _ => {}
        }
    }

    (doubles, singles)
}

/*
Returns the content of a string, wrapped in `quote`, with the escapes
corrected.

A change of the quote of a literal is not a textual swap. The target quote
must become escaped, and the other quote must lose its escape. Without these
changes, the literal changes meaning. This is the reason `quote_style` is
more than a substitution.
*/
fn requote(inner: &str, quote: u8) -> String {
    let quote = quote as char;
    let other = if quote == '"' { '\'' } else { '"' };
    let mut out = String::with_capacity(inner.len() + 2);
    out.push(quote);

    let mut chars = inner.chars();

    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.next() {
                // The other quote no longer needs its escape.
                Some(next) if next == other => out.push(other),

                Some(next) => {
                    out.push('\\');
                    out.push(next);
                }

                None => out.push('\\'),
            },

            c if c == quote => {
                out.push('\\');
                out.push(quote);
            }

            c => out.push(c),
        }
    }

    out.push(quote);

    out
}
