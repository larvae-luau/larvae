//! Expressions, precedence climbing and the suffixed chain

use crate::syntax::lexer::TokKind;

use super::*;

impl<'a> Parser<'a> {
    // --- expressions -------------------------------------------------------

    pub(super) fn expr(&mut self) -> Result<Expr, ParseError> {
        self.sub_expr(0)
    }

    pub(super) fn sub_expr(&mut self, limit: u8) -> Result<Expr, ParseError> {
        self.enter()?;
        let start = self.pos;
        let mut left = if is_unary_op(self.text()) {
            let op = self.bump();
            let operand = self.sub_expr(UNARY_PRIORITY)?;

            Expr::Unary {
                op: TokSpan::new(op, op + 1),
                operand: Box::new(operand),
                span: TokSpan::new(start, self.pos),
            }
        } else {
            self.simple_expr()?
        };

        while let Some((left_prec, right_prec)) = binop_priority(self.text()) {
            if left_prec <= limit {
                break;
            }

            let op = self.bump();
            let rhs = self.sub_expr(right_prec)?;

            left = Expr::Binary {
                op: TokSpan::new(op, op + 1),
                lhs: Box::new(left),
                rhs: Box::new(rhs),
                span: TokSpan::new(start, self.pos),
            };
        }

        self.leave();

        Ok(left)
    }

    pub(super) fn simple_expr(&mut self) -> Result<Expr, ParseError> {
        let start = self.pos;
        let mut e = match self.text() {
            "nil" => {
                self.bump();

                Expr::Nil(TokSpan::new(start, self.pos))
            }

            "true" => {
                self.bump();

                Expr::True(TokSpan::new(start, self.pos))
            }

            "false" => {
                self.bump();

                Expr::False(TokSpan::new(start, self.pos))
            }

            "..." => {
                self.bump();

                Expr::Vararg(TokSpan::new(start, self.pos))
            }

            "function" => {
                self.bump();
                let body = self.function_body()?;
                Expr::Function {
                    attributes: Vec::new(),
                    body: Box::new(body),
                    span: TokSpan::new(start, self.pos),
                }
            }

            "@" => {
                let attributes = self.attributes()?;
                self.expect("function")?;
                let body = self.function_body()?;
                Expr::Function {
                    attributes,
                    body: Box::new(body),
                    span: TokSpan::new(start, self.pos),
                }
            }

            "{" => self.table_expr()?,

            "if" => self.if_else_expr()?,

            _ => match self.kind_at(0) {
                Some(TokKind::Number) => {
                    self.bump();

                    Expr::Number(TokSpan::new(start, self.pos))
                }

                Some(TokKind::Str { .. }) => {
                    self.bump();

                    Expr::String(TokSpan::new(start, self.pos))
                }

                Some(TokKind::InterpStr) => {
                    self.bump();

                    Expr::InterpString(TokSpan::new(start, self.pos))
                }

                _ => self.suffixed_expr()?,
            },
        };

        // `expr :: T` binds tighter than any binary operator
        while self.at("::") {
            self.bump();
            let ty = self.type_()?;
            e = Expr::TypeAssert {
                expr: Box::new(e),
                ty,
                span: TokSpan::new(start, self.pos),
            };
        }

        Ok(e)
    }

    pub(super) fn primary_expr(&mut self) -> Result<Expr, ParseError> {
        let start = self.pos;

        if self.at("(") {
            self.bump();
            let inner = self.expr()?;
            self.expect(")")?;

            return Ok(Expr::Paren {
                inner: Box::new(inner),
                span: TokSpan::new(start, self.pos),
            });
        }

        let name = self.expect_name()?;

        Ok(Expr::Name(name))
    }

    pub(super) fn suffixed_expr(&mut self) -> Result<Expr, ParseError> {
        self.enter()?;

        let start = self.pos;
        let mut e = self.primary_expr()?;

        loop {
            match self.text() {
                "." => {
                    self.bump();
                    let field = self.expect_name()?;
                    e = Expr::Index {
                        object: Box::new(e),
                        key: IndexKey::Field(field),
                        span: TokSpan::new(start, self.pos),
                    };
                }

                "[" => {
                    self.bump();
                    let key = self.expr()?;
                    self.expect("]")?;
                    e = Expr::Index {
                        object: Box::new(e),
                        key: IndexKey::Computed(Box::new(key)),
                        span: TokSpan::new(start, self.pos),
                    };
                }

                ":" => {
                    self.bump();

                    let method = self.expect_name()?;

                    // a method call takes type arguments too, `obj:m<<T>>()`
                    if self.at("<") && self.text_at(1) == "<" {
                        self.angle_span()?;
                    }

                    let args = self.call_args()?;

                    e = Expr::Call {
                        func: Box::new(e),
                        method: Some(method),
                        args,
                        span: TokSpan::new(start, self.pos),
                    };
                }

                /*
                Explicit type instantiation, `f<<T>>()`, which Luau calls a
                turbofish. This is the only place two `<` sit next to each
                other: no expression starts with `<`, and Luau has no shift
                operator, so there is nothing to disambiguate against.

                The argument is a type or a type pack, so `<<number>>`,
                `<<(number, string)>>`, `<<()>>` and `<<...number>>` are all
                legal. `angle_span` already depth counts brackets, which is why
                the nested pair needs no special case.

                A turbofish only ever precedes a call, so `call_args` is
                required rather than optional, and reports it when missing.
                */
                "<" if self.text_at(1) == "<" => {
                    self.angle_span()?;

                    let args = self.call_args()?;

                    e = Expr::Call {
                        func: Box::new(e),
                        method: None,
                        args,
                        span: TokSpan::new(start, self.pos),
                    };
                }

                "(" | "{" => {
                    let args = self.call_args()?;
                    e = Expr::Call {
                        func: Box::new(e),
                        method: None,
                        args,
                        span: TokSpan::new(start, self.pos),
                    };
                }

                _ => {
                    if matches!(self.kind_at(0), Some(TokKind::Str { .. })) {
                        let args = self.call_args()?;
                        e = Expr::Call {
                            func: Box::new(e),
                            method: None,
                            args,
                            span: TokSpan::new(start, self.pos),
                        };
                    } else {
                        break;
                    }
                }
            }
        }

        self.leave();

        Ok(e)
    }

    pub(super) fn call_args(&mut self) -> Result<CallArgs, ParseError> {
        if self.at("(") {
            self.bump();
            let args = if self.at(")") {
                Vec::new()
            } else {
                self.expr_list()?
            };

            self.expect(")")?;

            return Ok(CallArgs::Paren(args));
        }

        if self.at("{") {
            let table = self.table_expr()?;

            return Ok(CallArgs::Table(Box::new(table)));
        }

        if matches!(self.kind_at(0), Some(TokKind::Str { .. })) {
            let i = self.bump();

            return Ok(CallArgs::Str(TokSpan::new(i, i + 1)));
        }

        Err(self.err(&format!("expected call arguments, found {}", self.found())))
    }

    pub(super) fn table_expr(&mut self) -> Result<Expr, ParseError> {
        let start = self.pos;
        self.expect("{")?;
        let mut fields = Vec::new();

        while !self.at("}") {
            if self.at_end() {
                return Err(self.err("unterminated table"));
            }

            if self.at("[") {
                self.bump();
                let key = self.expr()?;
                self.expect("]")?;
                self.expect("=")?;
                let value = self.expr()?;
                fields.push(TableField::Computed { key, value });
            } else if self.at_name() && self.text_at(1) == "=" {
                let name = self.expect_name()?;
                self.bump();
                let value = self.expr()?;
                fields.push(TableField::Named { name, value });
            } else {
                fields.push(TableField::Positional(self.expr()?));
            }

            if !self.eat(",") && !self.eat(";") {
                break;
            }
        }

        self.expect("}")?;
        Ok(Expr::Table {
            fields,
            span: TokSpan::new(start, self.pos),
        })
    }

    pub(super) fn if_else_expr(&mut self) -> Result<Expr, ParseError> {
        let start = self.pos;
        self.expect("if")?;

        let mut branches = Vec::new();
        let cond = self.expr()?;

        self.expect("then")?;
        branches.push((cond, self.expr()?));

        while self.at("elseif") {
            self.bump();
            let cond = self.expr()?;
            self.expect("then")?;
            branches.push((cond, self.expr()?));
        }

        self.expect("else")?;
        let else_value = self.expr()?;
        Ok(Expr::IfElse {
            branches,
            else_value: Box::new(else_value),
            span: TokSpan::new(start, self.pos),
        })
    }
}
