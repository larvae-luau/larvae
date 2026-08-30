/*!
A table constructor.

Two things hold a table open whatever its width: a newline after the `{`, and
a trailing comma on the last field. Both come from stylua and both are a
signal from the author that this is a list of things and not an expression.
*/

use super::*;

impl<'a> Emitter<'a> {
    pub(super) fn table(&self, fields: &[TableField], span: TokSpan) -> Doc<'a> {
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

        /*
        `sort_tables` reorders the fields before the layout reads them.
        The emitter writes every separator itself, commas whatever the
        author used, so the moved fields cannot leave a `;` behind or
        drop the trailing comma the magic option reads: that signal is
        the source's, and the source does not move.
        */
        let fields = self.sorted_fields(fields, commented);
        let fields = fields.as_slice();

        let each: Vec<Doc<'a>> = fields
            .iter()
            .map(|f| match *f {
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

            for (field, doc) in fields.iter().copied().zip(each) {
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

    /*
    The fields in the order `sort_tables` asks for, or as written.

    The guards keep the sort away from every table it cannot read
    whole. A comment cannot move with its field, a positional field is
    order as data, and a computed key that is not a plain string names
    nothing to sort by. Any one of them keeps the whole table as
    written, the same all-or-nothing rule the type sort holds.
    */
    fn sorted_fields<'f>(&self, fields: &'f [TableField], commented: bool) -> Vec<&'f TableField> {
        let order = self.cfg.sort_tables.order;

        if order == crate::fmt::config::PropertyOrder::None || fields.len() < 2 || commented {
            return fields.iter().collect();
        }

        let mut named: Vec<(std::borrow::Cow<'_, str>, usize, &TableField)> =
            Vec::with_capacity(fields.len());

        for field in fields {
            let name = match field {
                TableField::Named { name, .. } => std::borrow::Cow::Borrowed(self.one(*name)),

                TableField::Computed {
                    key: Expr::String(span),
                    ..
                } => {
                    let text = self.one(*span);

                    std::borrow::Cow::Owned(text.trim_matches(['"', '\'']).to_owned())
                }

                _ => return fields.iter().collect(),
            };

            // The size orders measure the whole field, not the key.
            let span = self.field_span(field);
            let (lo, hi) = self.byte_span(span);
            let width = self.src[lo as usize..hi as usize].chars().count();

            named.push((name, width, field));
        }

        /*
        The order is total: the measure first, then the name, and the
        sort is stable under that. So a second format finds the fields
        in the order the first one left them, and two fields that
        measure the same never trade places from run to run.
        */
        named.sort_by(|(a, a_width, _), (b, b_width, _)| {
            use crate::fmt::config::PropertyOrder;

            let (a_len, b_len) = (a.chars().count(), b.chars().count());

            match order {
                PropertyOrder::Descending => b_len.cmp(&a_len),
                PropertyOrder::Alphabetical => std::cmp::Ordering::Equal,
                PropertyOrder::SizeAscending => a_width.cmp(b_width),
                PropertyOrder::SizeDescending => b_width.cmp(a_width),

                _ => a_len.cmp(&b_len),
            }
            .then_with(|| a.cmp(b))
        });

        named.into_iter().map(|(_, _, field)| field).collect()
    }
}
