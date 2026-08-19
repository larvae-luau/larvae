/*!
Expressions and strings.

The two live together because a string literal is an expression and the quote
rules are reachable from nowhere else. This is the half that decides where a
long line breaks: calls, tables, binary chains, and the `if` expression.
*/

use super::*;

impl<'a> Emitter<'a> {
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
    pub(super) fn indented(&self, parts: Vec<Doc<'a>>) -> Doc<'a> {
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
    // --- strings -----------------------------------------------------------

    /*
    Emits a short string, quoted again in the configured style.

    The emitter keeps long strings, `[[...]]`, exactly as written. Their
    content is literal, so there is no quote to change and nothing to escape.
    */
    pub(super) fn string(&self, span: TokSpan) -> Doc<'a> {
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
