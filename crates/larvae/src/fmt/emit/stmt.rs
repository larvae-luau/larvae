/*!
One statement, laid out.

Each function takes a node and returns the document for it. What goes between
two statements is `blocks`, and the expressions inside one are `expr`.
*/

use super::*;

impl<'a> Emitter<'a> {
    // --- statements --------------------------------------------------------

    pub(super) fn stmt(&self, stmt: &Stmt) -> Doc<'a> {
        match stmt {
            Stmt::Empty(_) => Doc::Nil,

            Stmt::Local(n) => match n.exported {
                true => Doc::concat([Doc::text("export "), self.local(n)]),

                false => self.local(n),
            },

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

            Stmt::Function(n) => match n.exported {
                true => Doc::concat([Doc::text("export "), self.function(n)]),

                false => self.function(n),
            },

            Stmt::Class(n) => self.class(n),

            Stmt::LocalFunction(n) => match n.exported {
                true => Doc::concat([Doc::text("export "), self.local_function(n)]),

                false => self.local_function(n),
            },

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

    pub(super) fn attributes(&self, attrs: &[TokSpan]) -> Doc<'a> {
        if attrs.is_empty() {
            return Doc::Nil;
        }

        Doc::concat(
            attrs
                .iter()
                .map(|a| Doc::concat([Doc::text(self.verbatim(*a)), Doc::Hard])),
        )
    }

    /*
    A class prints one member per line: the fields as written, and the
    methods through the same emitter every function uses. A field's
    annotation goes through `type_doc`, so a wide table type in a field
    opens exactly as it does on a binding.
    */
    fn class(&self, n: &Class) -> Doc<'a> {
        /*
        A comment between members has no member to attach to, the same
        problem a type alias has. The class prints as the author wrote it,
        so the comment survives.
        */
        let (lo, hi) = self.byte_span(n.span);

        if !self.trivia.between(lo, hi).is_empty() {
            return Doc::text(&self.src[lo as usize..hi as usize]);
        }

        let mut parts: Vec<Doc<'a>> = Vec::new();

        if n.exported {
            parts.push(Doc::text("export "));
        }

        if n.open {
            parts.push(Doc::text("open "));
        }

        parts.push(Doc::text("class "));
        parts.push(Doc::text(self.one(n.name)));

        if let Some(base) = n.extends {
            parts.push(Doc::text(" extends "));
            parts.push(Doc::text(self.one(base)));
        }

        let mut body: Vec<Doc<'a>> = Vec::new();

        for member in &n.members {
            body.push(Doc::Hard);

            match member {
                ClassMember::Field {
                    public, name, ty, ..
                } => {
                    if *public {
                        body.push(Doc::text("public "));
                    }

                    body.push(Doc::text(self.one(*name)));

                    if let Some(ty) = ty {
                        body.push(Doc::text(": "));
                        body.push(self.type_doc(*ty));
                    }
                }

                ClassMember::Method(f) => body.push(self.function(f)),
            }
        }

        parts.push(Doc::indent(Doc::concat(body)));
        parts.push(Doc::Hard);
        parts.push(Doc::text("end"));

        Doc::concat(parts)
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
    pub(super) fn function_body(&self, body: &FunctionBody) -> Doc<'a> {
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

        let cfg = &self.cfg.function_declaration;

        self.listed("(", ")", each.collect(), false, cfg.expand, cfg.indent)
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
}
