/*!
A call and its arguments.

`call_parentheses` lives here, and so does the decision about where a long
argument list breaks. A table argument is next door in `tables`, and the two
meet when the last argument of a call is one.
*/

use super::*;

impl<'a> Emitter<'a> {
    pub(super) fn call(
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

        let cfg = &self.cfg.function_call;

        /*
        The last argument holds the shape, and the ones before it stay put.

        A call whose last argument is a table or a function reads as a call
        with a block after it, so the block is what opens. The arguments
        before it never break here, which is the trade the style asks for: a
        long list of them runs past `column_width` rather than moving to
        lines of its own.

        Only a value that hangs earns this. A call of plain arguments has
        nothing to hold the shape, so it opens one per line as it always did.
        */
        if cfg.style == CallStyle::HugLast
            && cfg.expand != ListExpansion::Always
            && list.len() > 1
            && list.last().is_some_and(|last| self.hangs(last))
        {
            let mut parts = vec![Doc::text("(")];

            for (i, arg) in list.iter().enumerate() {
                if i > 0 {
                    parts.push(Doc::text(", "));
                }

                parts.push(self.expr(arg));
            }

            parts.push(Doc::text(")"));

            return Doc::concat(parts);
        }

        self.listed(
            "(",
            ")",
            list.iter().map(|e| self.expr(e)).collect(),
            self.cfg.space_inside_parens,
            cfg.expand,
            cfg.indent,
        )
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
    /*
    A parenthesised list, laid out as `expand` asks.

    `when-needed` is the layout larvae always had: the renderer breaks the
    list when the line does not fit. `always` breaks it whatever the width,
    and `never` holds it on one line, where a value inside it can still open
    on its own.

    `levels` is the indent an opened item takes. Zero puts an item at the
    column of the bracket.
    */
    pub(super) fn listed(
        &self,
        open: &'a str,
        close: &'a str,
        items: Vec<Doc<'a>>,
        spaced: bool,
        expand: ListExpansion,
        levels: usize,
    ) -> Doc<'a> {
        if expand == ListExpansion::Never {
            let pad = match spaced {
                true => Doc::text(" "),

                false => Doc::Nil,
            };

            return Doc::concat([
                Doc::text(open),
                pad.clone(),
                Doc::join(Doc::text(", "), items),
                pad,
                Doc::text(close),
            ]);
        }

        let (sep, pad) = match expand {
            ListExpansion::Always => (Doc::Hard, Doc::Hard),

            // Soft holds nothing flat, Line holds the space that `spaced` asks for.
            _ => match spaced {
                true => (Doc::Line, Doc::Line),

                false => (Doc::Line, Doc::Soft),
            },
        };

        let inner = Doc::join(Doc::concat([Doc::text(","), sep]), items);

        let body = (0..levels).fold(Doc::concat([pad.clone(), inner]), |doc, _| Doc::indent(doc));

        Doc::group(Doc::concat([Doc::text(open), body, pad, Doc::text(close)]))
    }

    pub(super) fn bracketed(
        &self,
        open: &'a str,
        close: &'a str,
        inner: Doc<'a>,
        spaced: bool,
    ) -> Doc<'a> {
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
}
