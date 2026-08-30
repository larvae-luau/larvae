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
    BlockNewlineGaps, CallParens, CallStyle, CollapseSimpleStatement, FmtConfig, IfExpansion,
    IfPlacement, IfStyle, ListExpansion, PropertyOrder, QuoteStyle, RequireGrouping, Semicolons,
    TypeExpansion,
};
use super::doc::Doc;
use super::trivia::{Attached, Comment, Trivia};

mod blocks;
mod calls;
mod expr;
mod stmt;
mod tables;

pub struct Emitter<'a> {
    src: &'a str,
    toks: &'a [Tok],
    trivia: &'a Trivia<'a>,
    cfg: &'a FmtConfig,
    /// The keywords that `require_binding` decided to change, by their token index
    rebindings: super::rebind::Rebindings,
    /// What `unused_imports` decided about each dead require
    unused: super::unused::Plan,
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
            unused: super::unused::Plan::default(),
            ignored: Vec::new(),
            if_depth: std::cell::Cell::new(0),
        }
    }

    /// Gives the emitter what `unused_imports` decided
    pub fn dropping(mut self, plan: super::unused::Plan) -> Self {
        self.unused = plan;

        self
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
    pub(crate) fn verbatim_stmt(&self, stmt: &Stmt) -> Option<&'a str> {
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
    The separator between fields follows the option in both layouts. A union
    or an intersection follows `type_operators` in the same way.

    A comment inside the span keeps the author's text unchanged, for the
    reason `verbatim` states: a replay has no position to put one back.
    */
    fn type_doc(&self, span: TokSpan) -> Doc<'a> {
        if span.is_empty() || !self.cfg.lays_out_types() {
            return Doc::text(self.verbatim(span));
        }

        let (lo, hi) = self.byte_span(span);

        if !self.trivia.between(lo, hi).is_empty() {
            return Doc::text(self.verbatim(span));
        }

        self.type_region(span.start, span.end, Fit::Free)
    }

    /// The tokens of one type stretch: the operator chain first, then the tables
    fn type_region(&self, from: u32, to: u32, fit: Fit) -> Doc<'a> {
        if self.cfg.type_operators.expand != TypeExpansion::Auto
            && let Some(chain) = self.operator_chain(from, to, fit)
        {
            return chain;
        }

        self.type_run(from, to, fit)
    }

    /// One stretch with no operator to split: tables laid out, the rest replayed
    fn type_run(&self, from: u32, to: u32, fit: Fit) -> Doc<'a> {
        let mut parts: Vec<Doc<'a>> = Vec::new();
        let mut flat = String::new();
        let mut prev: Option<u32> = None;
        let mut i = from;

        while i < to {
            let spaced = prev.is_some_and(|p| needs_space(self.tok(p), self.tok(i)));

            if self.cfg.table_types.enabled
                && self.tok(i) == "{"
                && let Some(close) = self.matching_brace(i, to)
            {
                if spaced {
                    flat.push(' ');
                }

                if !flat.is_empty() {
                    parts.push(Doc::text(std::mem::take(&mut flat)));
                }

                parts.push(self.table_type(i, close, fit));
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
    fn table_type(&self, open: u32, close: u32, fit: Fit) -> Doc<'a> {
        let cfg = &self.cfg.table_types;

        let mut spans: Vec<(u32, u32)> = Vec::new();
        let mut depth = 0i32;
        let mut start = open + 1;

        for i in open + 1..close {
            let t = self.tok(i);

            match t {
                "{" | "(" | "[" => depth += 1,

                "}" | ")" | "]" => depth -= 1,

                "," | ";" if depth == 0 => {
                    if start < i {
                        spans.push((start, i));
                    }

                    start = i + 1;
                }

                _ if t.bytes().all(|b| b == b'<') => depth += t.len() as i32,

                _ if t.bytes().all(|b| b == b'>') => depth -= t.len() as i32,

                _ => {}
            }
        }

        if start < close {
            spans.push((start, close));
        }

        if spans.is_empty() {
            return Doc::text("{}");
        }

        self.sort_properties(&mut spans);

        let fields: Vec<Doc<'a>> = spans
            .into_iter()
            .map(|(from, to)| self.type_region(from, to, fit))
            .collect();

        let sep = cfg.separator.text();

        // `{ ` and ` }` around the fields, and a separator with a space between them
        let mut width = Some(4 + (fields.len() - 1) * (sep.len() + 1));

        for field in &fields {
            width = match (width, field.flat_width()) {
                (Some(sum), Some(w)) => Some(sum + w),

                _ => None,
            };
        }

        /*
        `Fit::Flat` says the caller already chose one line for the text
        around this table, so the width of the table decides nothing here.
        `type_operators` asked for that line and `column_width` allowed it.
        */
        if fit == Fit::Flat || width.is_some_and(|w| w <= cfg.width) {
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

    /*
    Puts the properties of one table type in the order `sort_table_types`
    asks for.

    The sort reads the name of each property from its first tokens. A field
    that names nothing leaves the whole table in the order the author wrote:
    a table the sort cannot read in full is a table it must not touch. The
    element type of `{ string }` is that case, and so is a key written as a
    string, because neither has a name to measure.

    A comment never reaches this point. `type_doc` prints a type that holds
    one exactly as the author wrote it, because a token replay has no
    position to put a comment back. So the sort cannot separate a comment
    from the property it sits on: a table with a comment in it stays as
    written, whatever `order` says. A moved comment that now describes
    another property is worse than a table this option skipped.
    */
    fn sort_properties(&self, spans: &mut [(u32, u32)]) {
        let cfg = &self.cfg.sort_table_types;

        if cfg.order == PropertyOrder::None || spans.len() < 2 {
            return;
        }

        let mut named = Vec::with_capacity(spans.len());

        for &(from, to) in spans.iter() {
            match self.property_name(from, to) {
                Some(name) => named.push((name, (from, to))),

                None => return,
            }
        }

        /*
        The sort is stable, so two properties that measure the same and
        carry the same name keep the order the author gave them.
        */
        named.sort_by(|(a, a_span), (b, b_span)| {
            let (a_span, b_span) = (*a_span, *b_span);

            let place = cfg.indexer_position();

            let rank = |name: &Property<'_>| match name.indexer {
                false => 1u8,

                true => match place {
                    crate::fmt::config::IndexerPosition::First => 0,
                    crate::fmt::config::IndexerPosition::Last => 2,
                    crate::fmt::config::IndexerPosition::Sorted => 1,
                },
            };

            let (a_len, b_len) = (a.text.chars().count(), b.text.chars().count());

            // The size orders measure the whole field, not the name.
            let width = |(from, to): (u32, u32)| {
                let (lo, hi) = self.byte_span(TokSpan {
                    start: from,
                    end: to,
                });

                self.src[lo as usize..hi as usize].chars().count()
            };

            rank(a)
                .cmp(&rank(b))
                .then_with(|| match cfg.order {
                    PropertyOrder::Descending => b_len.cmp(&a_len),

                    // Alphabetical ignores the lengths; the final
                    // tiebreak below is already the whole order.
                    PropertyOrder::Alphabetical => std::cmp::Ordering::Equal,

                    PropertyOrder::SizeAscending => width(a_span).cmp(&width(b_span)),
                    PropertyOrder::SizeDescending => width(b_span).cmp(&width(a_span)),

                    _ => a_len.cmp(&b_len),
                })
                .then_with(|| a.text.cmp(b.text))
        });

        for (slot, (_, span)) in spans.iter_mut().zip(named) {
            *slot = span;
        }
    }

    /*
    The name of one property of a table type, or None where it has none.

    `read` and `write` sit before the name, and either one is also a name a
    property can carry, so the `:` after it decides which of the two this is.
    */
    fn property_name(&self, from: u32, to: u32) -> Option<Property<'a>> {
        if from + 1 >= to {
            return None;
        }

        /*
        An indexer states the shape of every key instead of one key, so it
        measures as an empty name. A key written as a string is not an
        indexer, and it carries quotes that no length of a name can compare
        against, so the table keeps the order it has.
        */
        if self.tok(from) == "[" {
            let inner = self.tok(from + 1);

            if inner.starts_with(['"', '\'', '[', '`']) {
                return None;
            }

            return Some(Property {
                text: "",
                indexer: true,
            });
        }

        let at = match matches!(self.tok(from), "read" | "write") && self.tok(from + 1) != ":" {
            true => from + 1,

            false => from,
        };

        let text = self.tok(at);

        if !is_atom(text) || text.starts_with(|c: char| c.is_ascii_digit()) {
            return None;
        }

        // A field without a `:` is the element type of an array, which names nothing.
        if at + 1 >= to || self.tok(at + 1) != ":" {
            return None;
        }

        Some(Property {
            text,
            indexer: false,
        })
    }

    /*
    A union or an intersection, laid out by `type_operators`.

    Returns None where this stretch holds no operator of its own top level,
    and the caller replays the tokens instead.

    The split reads the operators that this stretch owns. One inside `<>` or
    `()` keeps its line, because the replay does not descend into either. A
    member that is a table type still opens, and the fields of that table are
    stretches of their own, so a nested chain follows the option at every
    level the layout reaches.

    Only whitespace moves, and that is what makes the split safe where it
    reads a chain the parser groups another way. `A | B & C` groups as
    `A | (B & C)`, and `(T) -> A | B` returns the union, whatever the lines
    look like. The one token this adds is the leading `|` or `&` of an opened
    chain, which Luau allows and its own parser skips.
    */
    fn operator_chain(&self, from: u32, to: u32, fit: Fit) -> Option<Doc<'a>> {
        let head = self.chain_head(from, to);
        let members = self.chain_members(head, to)?;
        let prefix = self.type_run(from, head, Fit::Flat);

        // the operator that opens the first line, which is the first one the chain uses
        let lead = members
            .iter()
            .map(|(op, ..)| *op)
            .find(|op| !op.is_empty())
            .unwrap_or("|");

        // One line takes flat members, so the tables inside them stay on it too.
        let flat = || {
            let mut parts = vec![prefix.clone()];

            for (i, &(op, start, end)) in members.iter().enumerate() {
                match i {
                    0 if matches!(prefix, Doc::Nil) => {}

                    0 => parts.push(Doc::text(" ")),

                    _ => parts.push(Doc::text(format!(" {op} "))),
                }

                parts.push(self.type_region(start, end, Fit::Flat));
            }

            Doc::concat(parts)
        };

        if fit == Fit::Flat {
            return Some(flat());
        }

        let broken = || {
            let lines = members.iter().enumerate().map(|(i, &(op, start, end))| {
                let op = match i {
                    0 => lead,

                    _ => op,
                };

                Doc::concat([
                    Doc::Hard,
                    Doc::text(op),
                    Doc::text(" "),
                    self.type_region(start, end, Fit::Free),
                ])
            });

            Doc::concat([prefix.clone(), Doc::indent(Doc::concat(lines))])
        };

        match self.cfg.type_operators.expand {
            TypeExpansion::Always => Some(broken()),

            /*
            The group asks the renderer, which measures from the column the
            chain starts at. So `column_width` decides, and it is the only
            thing that can take the one line this setting asks for.
            */
            TypeExpansion::Never => Some(Doc::group(Doc::if_break(flat(), broken()))),

            TypeExpansion::Auto => None,
        }
    }

    /*
    The token where the operator chain of a stretch starts.

    A stretch is not always the type alone. An alias arrives as the whole
    statement, and a field of a table type arrives with its name, so a `=`
    or a `:` of the top level ends a prefix that no line break may move.
    */
    fn chain_head(&self, from: u32, to: u32) -> u32 {
        let mut depth = 0i32;
        let mut head = from;

        for i in from..to {
            let t = self.tok(i);

            match t {
                "{" | "(" | "[" => depth += 1,

                "}" | ")" | "]" => depth -= 1,

                "=" | ":" if depth == 0 => head = i + 1,

                _ if t.bytes().all(|b| b == b'<') => depth += t.len() as i32,

                _ if t.bytes().all(|b| b == b'>') => depth -= t.len() as i32,

                _ => {}
            }
        }

        head
    }

    /*
    The members of one chain, each with the operator that precedes it.

    The operator of the first member is empty, unless the author wrote the
    leading `|` that Luau allows there. Returns None where the stretch holds
    fewer than two members, because one member is not a chain.
    */
    fn chain_members(&self, from: u32, to: u32) -> Option<Vec<(&'a str, u32, u32)>> {
        let mut members: Vec<(&'a str, u32, u32)> = Vec::new();
        let mut depth = 0i32;
        let mut start = from;
        let mut op = "";

        for i in from..to {
            let t = self.tok(i);

            match t {
                "{" | "(" | "[" => depth += 1,

                "}" | ")" | "]" => depth -= 1,

                "|" | "&" if depth == 0 => {
                    if start < i {
                        members.push((op, start, i));
                    }

                    op = t;
                    start = i + 1;
                }

                _ if t.bytes().all(|b| b == b'<') => depth += t.len() as i32,

                _ if t.bytes().all(|b| b == b'>') => depth -= t.len() as i32,

                _ => {}
            }
        }

        if start < to {
            members.push((op, start, to));
        }

        (members.len() > 1).then_some(members)
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
}

/// Reports if a statement has no block nested inside it.
pub(super) fn is_simple(stmt: &Stmt) -> bool {
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
pub(super) enum Single {
    Str,
    Table,
}

/*
Reports how much room a stretch of a type has.

`Flat` says the caller already took one line for the text around this
stretch. So every table type inside it stays on that line, whatever
`table_types.width` says, and no group inside it can break. `type_operators`
is the one option that asks for this, and `column_width` has already allowed
the line by the time it does.
*/
#[derive(PartialEq, Eq, Clone, Copy)]
pub(super) enum Fit {
    Free,
    Flat,
}

/// The name of one property of a table type, as `sort_table_types` reads it.
struct Property<'a> {
    text: &'a str,
    /// An indexer names the shape of a key, so it carries no name of its own.
    indexer: bool,
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
pub(super) fn needs_space(prev: &str, next: &str) -> bool {
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
        /*
        `read [string]: T`: a word before `[` is an access modifier ahead
        of an indexer, because a type indexes nothing else. Everything
        other than a word attaches, ex: `Map<`.
        */
        return prev == "{" || matches!(prev, "read" | "write");
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
