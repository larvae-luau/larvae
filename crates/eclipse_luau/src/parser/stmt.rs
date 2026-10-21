//! Statements, blocks, and the declaration forms.

use crate::lexer::TokKind;

use super::*;

impl<'a> Parser<'a> {
    // --- statements --------------------------------------------------------

    pub(super) fn at_block_end(&self) -> bool {
        matches!(self.text(), "end" | "else" | "elseif" | "until")
    }

    pub(super) fn block(&mut self) -> Result<Block, ParseError> {
        self.enter()?;

        let start = self.pos;
        let mut stmts = Vec::new();

        while !self.at_end() && !self.at_block_end() {
            let is_return = self.at("return");
            stmts.push(self.stmt()?);

            if is_return {
                // A return ends its block. Only a `;` can follow.
                if self.at(";") {
                    let i = self.bump();
                    stmts.push(Stmt::Empty(TokSpan::new(i, i + 1)));
                }

                break;
            }
        }

        self.leave();
        Ok(Block {
            stmts,
            span: TokSpan::new(start, self.pos),
        })
    }

    pub(super) fn stmt(&mut self) -> Result<Stmt, ParseError> {
        self.enter()?;
        let r = self.stmt_inner();
        self.leave();

        r
    }

    pub(super) fn stmt_inner(&mut self) -> Result<Stmt, ParseError> {
        let start = self.pos;

        match self.text() {
            ";" => {
                self.bump();

                Ok(Stmt::Empty(TokSpan::new(start, self.pos)))
            }

            "if" => self.if_stmt(start),

            "while" => {
                self.bump();
                let cond = self.expr()?;
                self.expect("do")?;
                let block = self.block()?;
                self.expect("end")?;
                Ok(Stmt::While(While {
                    cond,
                    block,
                    span: TokSpan::new(start, self.pos),
                }))
            }

            "do" => {
                self.bump();
                let block = self.block()?;
                self.expect("end")?;
                Ok(Stmt::Do(DoBlock {
                    block,
                    span: TokSpan::new(start, self.pos),
                }))
            }

            "for" => self.for_stmt(start),

            "repeat" => {
                self.bump();
                let block = self.block()?;
                self.expect("until")?;
                let cond = self.expr()?;
                Ok(Stmt::Repeat(Repeat {
                    block,
                    cond,
                    span: TokSpan::new(start, self.pos),
                }))
            }

            "function" => self.function_stmt(start, Vec::new()),

            "local" | "const" => self.local_stmt(start),

            "return" => {
                self.bump();
                let values = if self.at_end() || self.at_block_end() || self.at(";") {
                    Vec::new()
                } else {
                    self.expr_list()?
                };

                Ok(Stmt::Return(Return {
                    values,
                    span: TokSpan::new(start, self.pos),
                }))
            }

            "break" => {
                self.bump();

                Ok(Stmt::Break(TokSpan::new(start, self.pos)))
            }

            "continue" if self.continue_is_keyword() => {
                self.bump();

                Ok(Stmt::Continue(TokSpan::new(start, self.pos)))
            }

            "@" => {
                let attributes = self.attributes()?;

                // A definitions file decorates declarations the same way.
                if self.options.definitions && self.at("declare") {
                    return self.declare_stmt(start);
                }

                // `@native` and then `export function f()`; the RFC composes them.
                let exported = matches!(self.text(), "export")
                    && matches!(self.text_at(1), "local" | "const" | "function");

                if exported {
                    self.bump();
                }

                let stmt = if self.at("local") || self.at("const") {
                    let is_const = self.at("const");
                    self.bump();

                    self.local_function(start, attributes, is_const)
                } else {
                    self.function_stmt(start, attributes)
                }?;

                Ok(match exported {
                    true => mark_exported(stmt),

                    false => stmt,
                })
            }

            /*
            `export` is contextual, like `type`. It opens a declaration only
            when a declaration follows, so a variable named export keeps
            parsing as an expression.
            */
            "export" if self.text_at(1) == "type" => self.type_alias(start),

            "export"
                if matches!(self.text_at(1), "local" | "const" | "function" | "class")
                    || (self.text_at(1) == "open" && self.text_at(2) == "class") =>
            {
                self.bump();

                if self.at("class") || self.at("open") {
                    return self.class_stmt(start, true);
                }

                if self.at("function") {
                    return Ok(mark_exported(self.function_stmt(start, Vec::new())?));
                }

                Ok(mark_exported(self.local_stmt(start)?))
            }

            // `class` and `open` are contextual too: a declaration only before a name.
            "class" if self.name_at(1) => self.class_stmt(start, false),

            "open" if self.text_at(1) == "class" && self.name_at(2) => {
                self.class_stmt(start, false)
            }

            "type" if self.type_is_alias() => self.type_alias(start),

            /*

            `declare` is the statement of a definitions file, and it stays

            contextual: `declare = 1` and `declare(x)` are a name in code, and

            only the three declaration forms take the keyword reading.

            */
            "declare"
                if self.options.definitions
                    && (matches!(self.text_at(1), "function" | "class" | "extern")
                        || self.text_at(2) == ":") =>
            {
                self.declare_stmt(start)
            }

            _ => self.expr_stmt(start),
        }
    }

    /*
    One `declare` statement, in its three forms:

    `declare function name<T>(a: T, ...: any): R`
    `declare name: T`
    `declare class Name extends Base ... end`

    A class body holds properties (`name: T`, `["a b"]: T`), methods
    (`function name(self): R`), and indexers (`[T]: U`), each with an
    optional `read` or `write` in front. The tree keeps only the span; a
    declaration is meta code that larvae validates and never rewrites.
    */
    fn declare_stmt(&mut self, start: usize) -> Result<Stmt, ParseError> {
        self.bump(); // declare

        match self.text() {
            "function" => {
                self.bump();
                self.expect_name()?;
                self.declare_signature()?;
            }

            /*
            The new solver's spelling: `declare extern type Name with ...
            end`. The members are the members of a class declaration, so
            both forms share the loop below.
            */
            "extern" => {
                self.bump();
                self.expect("type")?;
                self.expect_name()?;

                if self.at("extends") {
                    self.bump();
                    self.expect_name()?;
                }

                self.expect("with")?;
                self.declare_members()?;
                self.expect("end")?;

                return Ok(Stmt::Declare(Declare {
                    span: TokSpan::new(start, self.pos),
                }));
            }

            "class" => {
                self.bump();
                self.expect_name()?;

                if self.at("extends") {
                    self.bump();
                    self.expect_name()?;
                }

                self.declare_members()?;
                self.expect("end")?;

                return Ok(Stmt::Declare(Declare {
                    span: TokSpan::new(start, self.pos),
                }));
            }

            _ => {
                self.expect_name()?;
                self.expect(":")?;
                self.type_()?;
            }
        }

        Ok(Stmt::Declare(Declare {
            span: TokSpan::new(start, self.pos),
        }))
    }

    /// The members of a class or extern type declaration, up to its `end`
    fn declare_members(&mut self) -> Result<(), ParseError> {
        while !self.at("end") {
            if self.at_end() {
                return Err(self.err("this declaration never ends"));
            }

            // A member takes attributes, ex: `@deprecated` above a method.
            if self.at("@") {
                self.attributes()?;
            }

            // The modifier changes nothing about the shape that follows.
            if self.at("read") || self.at("write") {
                self.bump();
            }

            if self.at("function") {
                self.bump();
                self.expect_name()?;
                self.declare_signature()?;
            } else if self.at("[") {
                self.bump();

                // A quoted name is a property; a type is an indexer.
                if matches!(self.kind_at(0), Some(TokKind::Str { .. })) {
                    self.bump();
                } else {
                    self.type_()?;
                }

                self.expect("]")?;
                self.expect(":")?;
                self.type_()?;
            } else {
                self.expect_name()?;
                self.expect(":")?;
                self.type_()?;
            }
        }

        Ok(())
    }

    /// The parameter list and return type of a declared function, no body
    fn declare_signature(&mut self) -> Result<(), ParseError> {
        if self.at("<") {
            self.angle_span()?;
        }

        self.expect("(")?;

        while !self.at(")") {
            if self.at_end() {
                return Err(self.err("this parameter list never closes"));
            }

            if self.at("...") {
                self.bump();

                if self.at(":") {
                    self.bump();
                    self.type_()?;
                }

                break;
            }

            self.expect_name()?;

            if self.at(":") {
                self.bump();
                self.type_()?;
            }

            if self.at(",") {
                self.bump();
            } else {
                break;
            }
        }

        self.expect(")")?;

        if self.at(":") {
            self.bump();
            self.type_ret()?;
        }

        Ok(())
    }

    /// `continue` is contextual. It is the keyword only when no token that
    /// would continue an expression follows it.
    pub(super) fn continue_is_keyword(&self) -> bool {
        !matches!(
            self.text_at(1),
            "=" | "," | "." | "(" | "[" | ":" | "+=" | "-=" | "*=" | "/=" | "%=" | "^=" | "..="
        )
    }

    /// `type` is also contextual: `type X =`, `type X<`, `type function f`.
    pub(super) fn type_is_alias(&self) -> bool {
        if self.text_at(1) == "function" {
            return true;
        }

        matches!(self.kind_at(1), Some(TokKind::Ident))
            && !is_reserved(self.text_at(1))
            && matches!(self.text_at(2), "=" | "<")
    }

    pub(super) fn attributes(&mut self) -> Result<Vec<TokSpan>, ParseError> {
        let mut out = Vec::new();

        while self.at("@") {
            let start = self.bump();

            /*
            The bracket form of a definitions file, ex:
            `@[deprecated { use = "task.spawn" }]`. The group skips whole
            and balanced; its content is metadata larvae reads past.
            */
            if self.at("[") {
                let mut depth = 0usize;

                loop {
                    if self.at_end() {
                        return Err(self.err("this attribute never closes"));
                    }

                    if self.at("[") {
                        depth += 1;
                    } else if self.at("]") {
                        depth -= 1;

                        if depth == 0 {
                            self.bump();

                            break;
                        }
                    }

                    self.bump();
                }
            } else {
                self.expect_name()?;
            }

            out.push(TokSpan::new(start, self.pos));
        }

        Ok(out)
    }

    pub(super) fn if_stmt(&mut self, start: usize) -> Result<Stmt, ParseError> {
        self.expect("if")?;

        let mut branches = Vec::new();
        let local = self.if_local()?;
        let cond = self.expr()?;

        self.expect("then")?;
        branches.push((local, cond, self.block()?));

        while self.at("elseif") {
            self.bump();
            let local = self.if_local()?;
            let cond = self.expr()?;
            self.expect("then")?;
            branches.push((local, cond, self.block()?));
        }

        let else_block = if self.eat("else") {
            Some(self.block()?)
        } else {
            None
        };

        self.expect("end")?;
        Ok(Stmt::If(If {
            branches,
            else_block,
            span: TokSpan::new(start, self.pos),
        }))
    }

    /*
    The `local x =` or `const x =` after an `if` or `elseif`, in both the
    statement and the expression form.

    `const` is the keyword only before a name, the rule upstream Luau uses.
    So `if const then` still reads a variable named const. The RFC allows
    one binding, so a `,` after the name is an error.
    */
    pub(super) fn if_local(&mut self) -> Result<Option<IfLocal>, ParseError> {
        let is_const = self.at("const") && self.name_at(1);

        if !self.at("local") && !is_const {
            return Ok(None);
        }

        let i = self.bump();
        let binding = self.binding()?;

        if self.at(",") {
            return Err(self.err("`if local` takes one binding, found `,`"));
        }

        self.expect("=")?;

        Ok(Some(IfLocal {
            keyword: TokSpan::new(i, i + 1),
            is_const,
            binding,
        }))
    }

    pub(super) fn for_stmt(&mut self, start: usize) -> Result<Stmt, ParseError> {
        self.expect("for")?;
        let first = self.binding()?;

        if self.eat("=") {
            let from = self.expr()?;
            self.expect(",")?;
            let limit = self.expr()?;
            let step = if self.eat(",") {
                Some(self.expr()?)
            } else {
                None
            };

            self.expect("do")?;
            let block = self.block()?;
            self.expect("end")?;

            return Ok(Stmt::NumericFor(NumericFor {
                var: first,
                start: from,
                limit,
                step,
                block,
                span: TokSpan::new(start, self.pos),
            }));
        }

        let mut vars = vec![first];

        while self.eat(",") {
            vars.push(self.binding()?);
        }

        self.expect("in")?;
        let exprs = self.expr_list()?;
        self.expect("do")?;
        let block = self.block()?;
        self.expect("end")?;
        Ok(Stmt::GenericFor(GenericFor {
            vars,
            exprs,
            block,
            span: TokSpan::new(start, self.pos),
        }))
    }

    pub(super) fn local_stmt(&mut self, start: usize) -> Result<Stmt, ParseError> {
        let is_const = self.at("const");
        // `start` can sit on `export`; the keyword is the token here.
        let keyword_at = self.bump();

        if self.at("function") {
            return self.local_function(start, Vec::new(), is_const);
        }

        if self.at("@") {
            let attributes = self.attributes()?;

            return self.local_function(start, attributes, is_const);
        }

        let keyword = TokSpan::new(keyword_at, keyword_at + 1);
        let mut names = vec![self.binding()?];

        while self.eat(",") {
            names.push(self.binding()?);
        }

        let values = if self.eat("=") {
            self.expr_list()?
        } else {
            Vec::new()
        };

        Ok(Stmt::Local(Local {
            keyword,
            exported: false,
            is_const,
            names,
            values,
            span: TokSpan::new(start, self.pos),
        }))
    }

    pub(super) fn local_function(
        &mut self,
        start: usize,
        attributes: Vec<TokSpan>,
        is_const: bool,
    ) -> Result<Stmt, ParseError> {
        self.expect("function")?;

        let name = self.expect_name()?;
        let body = self.function_body()?;

        Ok(Stmt::LocalFunction(LocalFunction {
            attributes,
            exported: false,
            is_const,
            name,
            body,
            span: TokSpan::new(start, self.pos),
        }))
    }

    pub(super) fn function_stmt(
        &mut self,
        start: usize,
        attributes: Vec<TokSpan>,
    ) -> Result<Stmt, ParseError> {
        self.expect("function")?;

        let mut path = vec![self.expect_name()?];
        let mut is_method = false;

        loop {
            if self.eat(".") {
                path.push(self.expect_name()?);
            } else if self.at(":") {
                self.bump();
                path.push(self.expect_name()?);
                is_method = true;
                break;
            } else {
                break;
            }
        }

        let body = self.function_body()?;
        Ok(Stmt::Function(Function {
            attributes,
            exported: false,
            path,
            is_method,
            body,
            span: TokSpan::new(start, self.pos),
        }))
    }

    pub(super) fn function_body(&mut self) -> Result<FunctionBody, ParseError> {
        let start = self.pos;
        let generics = if self.at("<") {
            Some(self.angle_span()?)
        } else {
            None
        };

        self.expect("(")?;
        let mut params = Vec::new();

        if !self.at(")") {
            loop {
                if self.at("...") {
                    let i = self.bump();
                    let ty = if self.eat(":") {
                        Some(self.type_()?)
                    } else {
                        None
                    };

                    params.push(Param {
                        name: TokSpan::new(i, i + 1),
                        is_vararg: true,
                        ty,
                    });

                    break;
                }

                let b = self.binding()?;
                params.push(Param {
                    name: b.name,
                    is_vararg: false,
                    ty: b.ty,
                });

                if !self.eat(",") {
                    break;
                }
            }
        }

        self.expect(")")?;
        let ret_type = if self.eat(":") {
            Some(self.type_ret()?)
        } else {
            None
        };

        let block = self.block()?;
        self.expect("end")?;
        Ok(FunctionBody {
            generics,
            params,
            ret_type,
            block,
            span: TokSpan::new(start, self.pos),
        })
    }

    /*
    `[export] class Name ... end`, per the classes RFC.

    The body holds two member forms. A field is `[public] name [: type]`,
    and a method is an ordinary function with exactly one name. A method
    whose name starts with `__` must be one of the metamethods the RFC
    lists, and everything else with that prefix is a syntax error there.
    Inheritance is deferred in the RFC, so no clause follows the name.
    */
    pub(super) fn class_stmt(&mut self, start: usize, exported: bool) -> Result<Stmt, ParseError> {
        let open = self.eat("open");
        self.expect("class")?;
        let name = self.expect_name()?;

        // `extends Base`, from the inheritance RFC; an open class allows it.
        let extends = match self.eat("extends") {
            true => Some(self.expect_name()?),

            false => None,
        };

        let mut members = Vec::new();

        loop {
            if self.eat("end") {
                break;
            }

            if self.at_end() {
                return Err(self.err("unterminated class, expected `end`"));
            }

            if self.eat(";") {
                continue;
            }

            if self.at("function") || self.at("@") {
                let m_start = self.pos;
                let attributes = match self.at("@") {
                    true => self.attributes()?,

                    false => Vec::new(),
                };

                // The name sits after `function`; the checks read it there.
                let method_name = self.text_at(1);

                if let Some(bare) = method_name.strip_prefix("__")
                    && !CLASS_METAMETHODS.contains(&bare)
                {
                    return Err(
                        self.err(&format!("__{bare} is not a metamethod a class can define"))
                    );
                }

                if matches!(self.text_at(2), "." | ":") {
                    return Err(self.err("a class method takes one name, without `.` or `:`"));
                }

                let Stmt::Function(f) = self.function_stmt(m_start, attributes)? else {
                    unreachable!("function_stmt parses a function");
                };

                members.push(ClassMember::Method(f));

                continue;
            }

            let m_start = self.pos;
            let public = self.eat("public");
            let field = self.expect_name()?;
            let ty = match self.eat(":") {
                true => Some(self.type_()?),

                false => None,
            };

            members.push(ClassMember::Field {
                public,
                name: field,
                ty,
                span: TokSpan::new(m_start, self.pos),
            });
        }

        Ok(Stmt::Class(Class {
            exported,
            open,
            name,
            extends,
            members,
            span: TokSpan::new(start, self.pos),
        }))
    }

    pub(super) fn binding(&mut self) -> Result<Binding, ParseError> {
        let name = self.expect_name()?;
        let ty = if self.eat(":") {
            Some(self.type_()?)
        } else {
            None
        };

        Ok(Binding { name, ty })
    }

    pub(super) fn type_alias(&mut self, start: usize) -> Result<Stmt, ParseError> {
        let exported = self.eat("export");
        self.expect("type")?;

        if self.at("function") {
            // `type function f() ... end` is a user-defined type function.
            self.bump();
            let name = self.expect_name()?;
            self.function_body()?;

            return Ok(Stmt::TypeAlias(TypeAlias {
                exported,
                name,
                span: TokSpan::new(start, self.pos),
            }));
        }

        let name = self.expect_name()?;

        if self.at("<") {
            self.angle_span()?;
        }

        self.expect("=")?;
        self.type_()?;
        Ok(Stmt::TypeAlias(TypeAlias {
            exported,
            name,
            span: TokSpan::new(start, self.pos),
        }))
    }

    pub(super) fn expr_stmt(&mut self, start: usize) -> Result<Stmt, ParseError> {
        let first = self.suffixed_expr()?;
        // This is an assignment, in the plain or the compound form.
        if self.at("=") || self.at(",") || is_compound_op(self.text()) {
            let mut targets = vec![first];

            while self.eat(",") {
                targets.push(self.suffixed_expr()?);
            }

            let op_idx = if is_compound_op(self.text()) {
                self.bump()
            } else {
                self.expect("=")?
            };

            let values = self.expr_list()?;

            return Ok(Stmt::Assign(Assign {
                targets,
                op: TokSpan::new(op_idx, op_idx + 1),
                values,
                span: TokSpan::new(start, self.pos),
            }));
        }

        match &first {
            Expr::Call { .. } => Ok(Stmt::Call(first, TokSpan::new(start, self.pos))),

            _ => Err(self.err("this expression is not a statement")),
        }
    }

    pub(super) fn expr_list(&mut self) -> Result<Vec<Expr>, ParseError> {
        let mut out = vec![self.expr()?];

        while self.eat(",") {
            out.push(self.expr()?);
        }

        Ok(out)
    }
}

/// The metamethods a class can define: the classes RFC list, and `__init`
/// from the constructors RFC that followed it
const CLASS_METAMETHODS: [&str; 17] = [
    "add", "sub", "mul", "div", "mod", "pow", "tostring", "eq", "lt", "le", "iter", "len", "idiv",
    "concat", "unm", "call", "init",
];

/// The statement with its export flag set; `export` reads the same three ways
fn mark_exported(stmt: Stmt) -> Stmt {
    match stmt {
        Stmt::Local(mut n) => {
            n.exported = true;

            Stmt::Local(n)
        }

        Stmt::Function(mut n) => {
            n.exported = true;

            Stmt::Function(n)
        }

        Stmt::LocalFunction(mut n) => {
            n.exported = true;

            Stmt::LocalFunction(n)
        }

        other => other,
    }
}
