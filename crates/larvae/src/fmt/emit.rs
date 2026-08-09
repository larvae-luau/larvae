/*!
The tree, turned into a layout document.

This half knows Luau and nothing about widths. Every decision about where a line
could break is expressed as a group, and `doc::render` decides which of them
actually do. Splitting it this way is why a nested call can stay on one line
inside a call that broke, which is the thing hand rolled formatters get wrong.

Two places print source verbatim rather than rebuilding it, and both are
deliberate. Types are stored as token spans because larvae parses types but has
never needed to interpret them, and a long comment carries its own line breaks
that re-indenting would corrupt. Verbatim here still means normalised: tokens
are replayed with the whitespace between them collapsed, so a type reads the
same however it was written.
*/

use std::borrow::Cow;

use crate::syntax::ast::*;
use crate::syntax::lexer::{Tok, TokKind};

use super::config::{
    BlockNewlineGaps, CallParens, CollapseSimpleStatement, FmtConfig, QuoteStyle, RequireGrouping,
};
use super::doc::Doc;
use super::trivia::{Attached, Comment, Trivia};

pub struct Emitter<'a> {
    src: &'a str,
    toks: &'a [Tok],
    trivia: &'a Trivia<'a>,
    cfg: &'a FmtConfig,
}

impl<'a> Emitter<'a> {
    pub fn new(src: &'a str, toks: &'a [Tok], trivia: &'a Trivia<'a>, cfg: &'a FmtConfig) -> Self {
        Self {
            src,
            toks,
            trivia,
            cfg,
        }
    }

    pub fn chunk(&self, chunk: &Chunk) -> Doc<'a> {
        let body = self.block_body(&chunk.block);

        // exactly one newline ends the file, which render does not add itself
        Doc::concat([body, Doc::Hard])
    }

    // --- tokens ------------------------------------------------------------

    fn tok(&self, i: u32) -> &'a str {
        self.toks[i as usize].text(self.src)
    }

    /// Text of a one token span
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
    A type, rebuilt from its tokens with the spacing decided rather than copied.

    The tree keeps types as token spans, because larvae parses types and has
    never needed to interpret one. That is enough to format them: spacing in a
    type is decided entirely by which two tokens are adjacent, so a pairwise
    rule reaches the same answer a type tree would without carrying one.

    What it cannot do is re-wrap a type across lines, so a very long type stays
    on one line. That is a real limit and the reason this is not simply called
    formatting.
    */
    fn verbatim(&self, span: TokSpan) -> Cow<'a, str> {
        if span.is_empty() {
            return Cow::Borrowed("");
        }

        let (lo, hi) = self.byte_span(span);
        let flat = &self.src[lo as usize..hi as usize];

        /*
        A comment inside a type cannot survive a token replay, because comments
        are not tokens. There is nowhere sensible to put one back either, since
        the replay has no structure to hang it on, so the region is emitted
        exactly as the author wrote it.
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

    /// Whether the author left a newline between two adjacent tokens
    fn newline_between(&self, a: u32, b: u32) -> bool {
        let (lo, hi) = (self.tok_end(a) as usize, self.tok_start(b) as usize);

        lo < hi && self.src[lo..hi].contains('\n')
    }

    /// Whether this expression prints starting with a `-`, which `-` cannot abut
    fn starts_with_minus(&self, e: &Expr) -> bool {
        self.tok(e.span().start) == "-"
    }

    /// The `)` closing the `(` at this token index, counting depth on the way
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

    /// Token span of one table field, which the tree does not store directly
    fn field_span(&self, field: &TableField) -> TokSpan {
        match field {
            TableField::Positional(e) => e.span(),

            // the key token comes first, and the value's span ends the field
            TableField::Named { name, value } => {
                TokSpan::new(name.start as usize, value.span().end as usize)
            }

            TableField::Computed { key, value } => TokSpan::new(
                // the `[` sits one token before the key
                key.span().start.saturating_sub(1) as usize,
                value.span().end as usize,
            ),
        }
    }

    /// Byte range covered by a token span
    fn byte_span(&self, span: TokSpan) -> (u32, u32) {
        (self.tok_start(span.start), self.tok_end(span.end - 1))
    }

    /// Whether a statement opens with `(`, which would continue the line above
    fn starts_with_paren(&self, stmt: &Stmt) -> bool {
        self.tok(stmt.span().start) == "("
    }

    /// Whether this expression prints starting with a `[`, which `[` cannot abut
    fn starts_with_bracket(&self, e: &Expr) -> bool {
        self.tok(e.span().start).starts_with('[')
    }

    /// Whether the token before this closing bracket is a comma
    fn has_trailing_comma(&self, close: u32) -> bool {
        close > 0 && self.tok(close - 1) == ","
    }

    /*
    Whether an expression absorbs a break rather than moving to its own line.

    Tables and functions hang because indenting them one extra level buys
    nothing. A `[[ ]]` string or a multi-line interpolation hangs for a harder
    reason: it carries its own newlines, so it forces every group around it to
    break, and a break at the punctuation before it can never make it fit. Left
    out, `f[[...]]` gains parentheses on one pass and then breaks at them on the
    next, which is a formatter that never settles.
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

    /// Byte just past whatever opened this block, `do`, `then`, `)` and so on
    fn block_lo(&self, block: &Block) -> u32 {
        match block.span.start {
            0 => 0,

            i => self.tok_end(i - 1),
        }
    }

    /// Byte where whatever closes this block begins, or the end of the file
    fn block_hi(&self, block: &Block) -> u32 {
        match self.toks.get(block.span.end as usize) {
            Some(t) => t.start,

            None => self.src.len() as u32,
        }
    }

    /*
    The statements of a block, with their comments and blank lines.

    A stray `;` is dropped rather than printed, since it is a statement in the
    grammar and noise in the output, and every formatter for the language drops
    it. Everything else keeps the shape the author gave it: one statement per
    line, and one blank line wherever they left one or more.
    */
    fn block_body(&self, block: &Block) -> Doc<'a> {
        let (prologue, mut pieces, tail) = self.pieces(block);

        if self.cfg.sort_requires.enabled {
            sort_requires(&mut pieces, self.cfg.sort_requires.grouping);
        }

        let mut parts: Vec<Doc<'a>> = Vec::with_capacity(pieces.len() * 3 + 2);

        for c in &prologue {
            parts.push(self.comment_doc(*c));
            parts.push(Doc::Hard);
        }

        for (i, piece) in pieces.iter().enumerate() {
            if i > 0 {
                parts.push(self.trailing_doc(pieces[i - 1].trailing));

                // a blank line is the separator, not something added to it
                parts.push(if piece.blank_before {
                    Doc::Blank
                } else {
                    Doc::Hard
                });
            }

            for c in &piece.leading {
                parts.push(self.comment_doc(*c));
                parts.push(Doc::Hard);
            }

            /*
            A statement opening with `(` continues the line above as a call,
            which is Lua's oldest ambiguity. The author's `;` is dropped like
            any other, so one is put back wherever it is load bearing: without
            it `local a = b` followed by `(c)()` reads as `local a = b(c)()`.
            */
            if i > 0 && self.starts_with_paren(piece.stmt) {
                parts.push(Doc::text(";"));
            }

            parts.push(self.stmt(piece.stmt));
        }

        if let Some(last) = pieces.last() {
            parts.push(self.trailing_doc(last.trailing));
        }

        // comments between the last statement and whatever closes the block
        if !tail.leading.is_empty() {
            if !pieces.is_empty() {
                parts.push(if tail.blank_before {
                    Doc::Blank
                } else {
                    Doc::Hard
                });
            }

            for (i, c) in tail.leading.iter().enumerate() {
                if i > 0 {
                    parts.push(Doc::Hard);
                }

                parts.push(self.comment_doc(*c));
            }
        }

        Doc::concat(parts)
    }

    /*
    A block split into one piece per statement, each holding the trivia that
    belongs to it.

    Emitting straight from the statement list would be shorter, and was, until
    `sort_requires` needed to move statements. A comment is found by its
    position in the source, so once a statement moves, a comment left behind
    would attach to whatever landed in its place. Binding the trivia to the
    statement first means reordering carries it along.
    */
    fn pieces<'s>(&self, block: &'s Block) -> (Vec<Comment>, Vec<Piece<'s, 'a>>, Tail) {
        let mut pieces: Vec<Piece<'_, 'a>> = Vec::with_capacity(block.stmts.len());
        let mut prologue: Vec<Comment> = Vec::new();
        let mut cursor = self.block_lo(block);

        for stmt in &block.stmts {
            /*
            A stray `;` is skipped, and the cursor moves past it so the blank
            line check does not count the newline on either side of it as a gap
            the author asked for.
            */
            if let Stmt::Empty(span) = stmt {
                cursor = self.tok_end(span.end - 1);

                continue;
            }

            let span = stmt.span();
            let start = self.tok_start(span.start);
            let att = self.trivia.split(cursor, start);

            // this gap's trailing comment sits on the line above, not this one
            if let Some(prev) = pieces.last_mut() {
                prev.trailing = att.trailing;
            }

            let blank_before = if att.leading.is_empty() {
                self.trivia.blank_before_code(cursor, start)
            } else {
                att.blank_before_leading
            };

            /*
            A `--!` directive belongs to the file rather than to the statement
            it happens to sit above, and only takes effect before any code. So
            it is lifted out of the first statement's leading comments into a
            prologue, which keeps it at the top whatever `sort_requires` then
            does with the statements. Left attached, sorting could move it down
            the file and quietly downgrade a module out of strict mode.
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

    fn trailing_doc(&self, comment: Option<Comment>) -> Doc<'a> {
        match comment {
            Some(c) => Doc::concat([Doc::text(" "), self.comment_doc(c)]),

            None => Doc::Nil,
        }
    }

    /*
    The path of a `local name = require("path")`, and nothing else.

    Deliberately narrow. Anything computed, `require(base .. name)`, or bound
    to more than one name, is not something whose order can be assumed not to
    matter, so it is left where the author put it and breaks the run around it.
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
    A comment sitting on the same line as the keyword that opened this block.

    `block_body` cannot emit it, because it attaches to the `do` or `then` the
    caller printed rather than to any statement. Without this it would simply
    vanish, which is the failure mode that makes people distrust a formatter.
    */
    fn open_comment(&self, block: &Block) -> Doc<'a> {
        let lo = self.block_lo(block);
        let hi = match block.stmts.first() {
            Some(stmt) => self.tok_start(stmt.span().start),

            None => self.block_hi(block),
        };

        self.trailing(&self.trivia.split(lo, hi))
    }

    /// A block indented inside its keywords, always broken
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
    The breaks at the two edges of a block, which `block_newline_gaps` decides.

    A blank line just inside `do` or just before `end` is dropped by default,
    which is what almost everyone wants and what stylua does. `preserve` keeps
    the author's, for the style that uses them to give a long body room to
    breathe.

    Only the edges: blank lines between statements are kept either way, because
    those separate ideas rather than pad a boundary.
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
    A block that may collapse onto one line.

    `collapse_simple_statement` only ever applies to a block holding exactly one
    statement with nothing nested inside it and no comments to lose, so the
    check is narrow on purpose: collapsing anything else hides code.
    */
    fn collapsible(&self, block: &Block, allowed: bool) -> Doc<'a> {
        if !allowed || block.stmts.len() != 1 || !is_simple(&block.stmts[0]) {
            return self.nested(block);
        }

        let stmt = &block.stmts[0];
        let span = stmt.span();

        // a comment anywhere in the block rules it out, since it would move
        if !self
            .trivia
            .between(self.block_lo(block), self.block_hi(block))
            .is_empty()
        {
            return self.nested(block);
        }

        // so does an author's blank line, which is a deliberate separation
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
            The tree keeps only the alias's name, so the rest prints from its
            tokens. That is fine for a type, which is punctuation the pairwise
            spacing rules were written for, and wrong for `type function f()`,
            whose body is arbitrary Luau: replaying it drops the newlines and
            runs `1` into `return`, which is a malformed number.

            So an alias covering more than one line is emitted exactly as the
            author wrote it. That keeps its comments too, which a token replay
            can never do.
            */
            Stmt::TypeAlias(n) => {
                let (lo, hi) = self.byte_span(n.span);
                let raw = &self.src[lo as usize..hi as usize];

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
                Doc::text(self.verbatim(ty)),
            ]),

            None => Doc::text(self.one(b.name)),
        }
    }

    fn local(&self, n: &Local) -> Doc<'a> {
        let keyword = self.one(n.keyword);
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

    /// ` = a, b`, breaking after the `=` when the values do not fit
    fn assigned(&self, values: &[Expr]) -> Doc<'a> {
        Doc::concat([Doc::text(" ="), self.values(values)])
    }

    fn values(&self, values: &[Expr]) -> Doc<'a> {
        let list = Doc::join(
            Doc::concat([Doc::text(","), Doc::Line]),
            values.iter().map(|e| self.expr(e)),
        );

        /*
        One value hangs off the `=` rather than moving to its own line.

        There is nothing to gain by breaking there: a lone value that does not
        fit does not fit one indent level further in either, and it breaks
        inside itself instead. Two or more values are a list, and a list that
        does not fit is worth putting on its own lines.
        */
        if values.len() == 1 {
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

    /// `<T>(a: A, b: B): R <block> end`, everything after the name
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
            parts.push(Doc::text(self.verbatim(ret)));
        }

        parts.push(self.collapsible(&body.block, self.collapse_functions()));
        parts.push(Doc::text("end"));

        Doc::group(Doc::concat(parts))
    }

    fn params(&self, params: &[Param], open: u32) -> Doc<'a> {
        /*
        Same reasoning as a type: the tree has no place to hang a comment
        written between two parameters, so a list holding one is emitted as
        written rather than losing it.
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
                Doc::text(self.verbatim(ty)),
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

    fn expr(&self, e: &Expr) -> Doc<'a> {
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
                `not x` needs its space and `#x` must not have one, which is the
                easy half. The hard half is `- -x`: two minus signs run together
                spell a line comment, so the rest of the line disappears and the
                file stops parsing. A space between them is not cosmetic.
                */
                let space = op == "not" || (op == "-" && self.starts_with_minus(operand));

                Doc::concat([
                    Doc::text(op),
                    Doc::text(if space { " " } else { "" }),
                    self.expr(operand),
                ])
            }

            Expr::Paren { inner, .. } => {
                Doc::concat([Doc::text("("), self.expr(inner), Doc::text(")")])
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
                        // `[` against a `[[ ]]` string opens a long string instead
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
            } => {
                let mut parts = Vec::with_capacity(branches.len() * 4 + 2);

                for (i, (cond, value)) in branches.iter().enumerate() {
                    parts.push(Doc::text(if i == 0 { "if " } else { "elseif " }));
                    parts.push(self.expr(cond));
                    parts.push(Doc::text(" then "));
                    parts.push(self.expr(value));
                    parts.push(Doc::Line);
                }

                parts.push(Doc::text("else "));
                parts.push(self.expr(else_value));

                Doc::group(Doc::indent(Doc::concat(parts)))
            }

            Expr::TypeAssert { expr, ty, .. } => Doc::concat([
                self.expr(expr),
                Doc::text(" :: "),
                Doc::text(self.verbatim(*ty)),
            ]),
        }
    }

    /*
    A binary chain, flattened so `a + b + c` breaks as one list rather than
    nesting a level per operator.

    The operator leads its continuation line, which is what makes a broken
    condition readable: the eye finds the `and` at a fixed column instead of
    hunting for it at the ragged right edge.
    */
    fn binary(&self, e: &Expr) -> Doc<'a> {
        let Expr::Binary { op, .. } = e else {
            return self.expr(e);
        };

        let prec = precedence(self.one(*op));
        let mut ops: Vec<&'a str> = Vec::new();
        let mut operands: Vec<Doc<'a>> = Vec::new();

        self.flatten_binary(e, prec, &mut ops, &mut operands);

        let mut operands = operands.into_iter();
        let first = operands.next().expect("a chain has a left operand");
        let mut rest = Vec::with_capacity(ops.len() * 3);

        for (op, operand) in ops.into_iter().zip(operands) {
            rest.push(Doc::Line);
            rest.push(Doc::text(op));
            rest.push(Doc::text(" "));
            rest.push(operand);
        }

        Doc::group(Doc::concat([first, Doc::indent(Doc::concat(rest))]))
    }

    fn flatten_binary(
        &self,
        e: &Expr,
        prec: u8,
        ops: &mut Vec<&'a str>,
        operands: &mut Vec<Doc<'a>>,
    ) {
        match e {
            Expr::Binary { op, lhs, rhs, .. } if precedence(self.one(*op)) == prec => {
                self.flatten_binary(lhs, prec, ops, operands);
                ops.push(self.one(*op));
                self.flatten_binary(rhs, prec, ops, operands);
            }

            _ => operands.push(self.expr(e)),
        }
    }

    /*
    A call, and the one place the `call_parentheses` option lives.

    `f "str"` and `f {t}` are calls without parentheses, which Luau allows and
    the option decides about. `input` keeps whatever the author wrote, which is
    the only setting that does not enforce consistency and exists because some
    codebases use the bare form as a DSL and the parenthesised form for calls.
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
        The option applies to the argument, not to the syntax the author
        happened to reach for, so `f("x")` and `f "x"` are the same call and
        settle on the same form. Deciding from the written form instead is what
        makes a setting look like it does nothing on half a codebase.
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

                // the author's choice stands, which is the only setting that
                // does not make the codebase consistent, and is why it exists
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
        A comment among the arguments forces the call to expand, for the same
        reason a table does: a line comment flat inside the parentheses would
        comment out the closing one and everything after it.
        */
        if let Some(doc) = self.commented_args(list, span) {
            return doc;
        }

        if list.is_empty() {
            return Doc::text("()");
        }

        /*
        A single function argument hugs the parentheses rather than being
        indented inside them, so a callback reads as a block and not as a list
        of one that happens to be enormous.
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
    An argument list holding a comment, one argument per line with its comments
    placed. `None` when there are none, which is almost always.

    Luau rejects a trailing comma in a call, so the commas go between rather
    than after, which is the one way this differs from the table case.
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

        // and whatever sits between the last argument and the closing paren
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

    /// The `(` opening the `)` at this token index, counting depth backwards
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
    Brackets around a breakable list.

    Flat, the optional inner spaces apply. Broken, the contents indent one level
    and the brackets sit on their own lines, and the inner spaces are irrelevant
    because a newline already separates them.
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
    A table.

    Two things force a table to stay expanded, and both come from stylua because
    people rely on them: a newline right after the `{`, and a trailing comma on
    the last field. Either one is the author saying this table is a list of
    things and not an expression, and width alone should not overrule that.
    */
    fn table(&self, fields: &[TableField], span: TokSpan) -> Doc<'a> {
        if fields.is_empty() {
            // `{ -- nothing yet }` holds a comment and is not the same as `{}`
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
        A comment anywhere in the table forces it to expand.

        Not a style preference. A line comment flat inside braces would comment
        out the closing brace and everything after it, so the choice is between
        expanding and losing the comment, and the comment is the author's.
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

                // this gap's trailing comment sits on the line above, not this one
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

            // and whatever sits between the last field and the closing brace
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
        A table the layout engine chooses to break still needs its trailing
        comma, or reading the output back would see an expanded table and add
        one, and the formatter would disagree with itself about its own output.
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
    A short string, re quoted to the configured style.

    Long strings, `[[...]]`, are left exactly as written: their content is
    literal, so there is no quote to change and nothing to escape.
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

            // prefer the configured quote unless the other needs fewer escapes
            QuoteStyle::AutoPreferDouble if doubles > singles => b'\'',

            QuoteStyle::AutoPreferDouble => b'"',

            QuoteStyle::AutoPreferSingle if singles > doubles => b'"',

            QuoteStyle::AutoPreferSingle => b'\'',

            QuoteStyle::Preserve => current,
        }
    }
}

/// Whether a statement has no block nested inside it
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

/// Which single argument form a call uses, for the `call_parentheses` option
#[derive(PartialEq, Eq, Clone, Copy)]
enum Single {
    Str,
    Table,
}

/// One statement with the trivia bound to it, so reordering carries both
struct Piece<'s, 'a> {
    stmt: &'s Stmt,
    /// A blank line before it, as the author left one
    blank_before: bool,
    /// Comments on their own lines above it
    leading: Vec<Comment>,
    /// A comment after it on the same line
    trailing: Option<Comment>,
    /// The require path, when this is a plain `local x = require("path")`
    key: Option<&'a str>,
}

/// Comments after the last statement, which belong to no statement
struct Tail {
    leading: Vec<Comment>,
    blank_before: bool,
}

/// Where a require path points, which is what `by-kind` groups on
#[derive(PartialEq, Eq, PartialOrd, Ord, Clone, Copy)]
enum PathKind {
    /// `@pkg/thing`, a name the project configured
    Alias,
    /// `game/ReplicatedStorage/thing`, rooted somewhere known
    Absolute,
    /// `./sibling`, resolved against this file
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
Sort each run of adjacent requires.

A run ends at anything that is not a plain require, and at a blank line, which
is the part worth saying: an author who separated two groups of requires with a
blank line has already grouped them, and shuffling across that line would
discard a decision rather than tidy one. So each run is sorted within itself
and never merged with its neighbour.

Nothing here moves a require past a statement that is not one, so a module
whose requires must run in a particular order relative to other code keeps it.
Order between two adjacent requires is what this assumes does not matter, which
is why the whole thing is off unless asked for.
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
    // the gap before the run belongs to the run's position, not to its first member
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
Whether two adjacent tokens in a type want a space between them.

Read as a table rather than as logic. Types are punctuation heavy and every
rule here is one shape someone writes: `Array<T>` closes up, `A | B` opens out,
`{ x: number }` breathes inside its braces, and `(T) -> U` has its arrow alone
in the middle. Nothing else in the language needs this because everything else
is rebuilt from a tree that already knows where its spaces go.
*/
fn needs_space(prev: &str, next: &str) -> bool {
    // nothing ever sits between a name and what qualifies or closes it
    if matches!(next, "," | ")" | "]" | ">" | "?" | ":" | ";" | ".") {
        return false;
    }

    if matches!(prev, "(" | "[" | "<" | "." | "..." | "@") {
        return false;
    }

    // an opening paren belongs to whatever precedes it unless an operator does
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
    Two atoms, where an atom is anything starting with a letter, a digit or an
    underscore. Words are the obvious case, `typeof T`, but numbers matter for
    a reason that is easy to miss: a type can hold an expression, through
    `typeof(...)`, and `and 1` run together spells the identifier `and1` while
    `2 or` spells the malformed number `2or`. Testing for words alone let both
    through.
    */
    is_atom(prev) && is_atom(next)
}

fn is_atom(tok: &str) -> bool {
    tok.starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_')
}

/*
Binding power, used only to decide what flattens into one breakable chain.

Output is correct at any of these values, because the tree already holds the
right shape and replaying it infix reproduces it. What they buy is that
`a and b or c` breaks at `or` before it breaks at `and`, rather than treating
every operator in the expression as equally good a place to fold.
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

/// Unescaped quote characters of each kind
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
The content of a string, wrapped in `quote` with escaping corrected.

Changing which quote a literal uses is not a textual swap: the target quote has
to become escaped and the other has to stop being, or the literal changes
meaning. This is the whole reason `quote_style` is more than a substitution.
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
                // the other quote no longer needs its escape
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
