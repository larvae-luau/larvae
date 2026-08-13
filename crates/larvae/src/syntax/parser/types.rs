//! Type syntax. The parser consumes it for its extent and never interprets it.

use crate::syntax::lexer::TokKind;

use super::*;

impl<'a> Parser<'a> {
    // --- types, extent only ------------------------------------------------

    /// Consumes a balanced `<...>` group. Generic parameter lists use this.
    pub(super) fn angle_span(&mut self) -> Result<TokSpan, ParseError> {
        let start = self.pos;
        self.expect("<")?;
        let mut depth = 1usize;

        while depth > 0 {
            if self.at_end() {
                return Err(self.err("unterminated generic parameter list"));
            }

            match self.text() {
                "<" => depth += 1,

                ">" => depth -= 1,

                ">=" if depth == 1 => {
                    return Err(self.err("write `> =` here, `>=` reads as one operator"));
                }

                _ => {}
            }

            self.bump();
        }

        Ok(TokSpan::new(start, self.pos))
    }

    pub(super) fn type_(&mut self) -> Result<TokSpan, ParseError> {
        self.enter()?;

        let start = self.pos;
        let r = self.type_body();

        self.leave();
        r?;

        Ok(TokSpan::new(start, self.pos))
    }

    pub(super) fn type_body(&mut self) -> Result<(), ParseError> {
        // Luau allows a leading `|` or `&`.
        if self.at("|") || self.at("&") {
            self.bump();
        }

        self.type_suffixed()?;

        while self.at("|") || self.at("&") {
            self.bump();
            self.type_suffixed()?;
        }

        Ok(())
    }

    /// Parses a return type: a single type or a parenthesized pack.
    pub(super) fn type_ret(&mut self) -> Result<TokSpan, ParseError> {
        self.type_()
    }

    pub(super) fn type_suffixed(&mut self) -> Result<(), ParseError> {
        self.type_primary()?;

        loop {
            if self.at("?") {
                self.bump();
            } else if self.at("->") {
                self.bump();
                self.type_suffixed()?;
            } else {
                break;
            }
        }

        Ok(())
    }

    pub(super) fn type_primary(&mut self) -> Result<(), ParseError> {
        self.enter()?;
        let r = self.type_primary_inner();
        self.leave();

        r
    }

    pub(super) fn type_primary_inner(&mut self) -> Result<(), ParseError> {
        match self.text() {
            "nil" | "true" | "false" => {
                self.bump();

                Ok(())
            }

            "typeof" if self.text_at(1) == "(" => {
                self.bump();
                self.bump();
                self.expr()?;
                self.expect(")")?;

                Ok(())
            }

            "..." => {
                // This is the variadic element of a type pack.
                self.bump();

                self.type_suffixed()
            }

            // This is a generic function type: `<T>(T) -> T`.
            "<" => {
                self.angle_span()?;

                self.type_primary_inner()
            }

            "(" => {
                // This is a parenthesized type or the parameters of a function type.
                self.bump();

                if !self.at(")") {
                    loop {
                        if self.at("...") {
                            self.bump();

                            if !self.at(")") && !self.at(",") {
                                self.type_suffixed()?;
                            }
                        } else {
                            // The parameter can have a name.
                            if self.at_name() && self.text_at(1) == ":" {
                                self.bump();
                                self.bump();
                            }

                            self.type_body()?;
                        }

                        if !self.eat(",") {
                            break;
                        }
                    }
                }

                self.expect(")")?;

                Ok(())
            }

            "{" => self.type_table(),

            _ => match self.kind_at(0) {
                Some(TokKind::Str { .. }) => {
                    // This is a singleton string type.
                    self.bump();

                    Ok(())
                }

                Some(TokKind::Ident) if !is_reserved(self.text()) => {
                    self.bump();

                    if self.at(".") {
                        self.bump();
                        self.expect_name()?;
                    }

                    if self.at("<") {
                        self.angle_span()?;
                    }

                    // This is a generic type pack: `T...`.
                    if self.at("...") {
                        self.bump();
                    }

                    Ok(())
                }

                _ => Err(self.err(&format!("expected a type, found {}", self.found()))),
            },
        }
    }

    pub(super) fn type_table(&mut self) -> Result<(), ParseError> {
        self.expect("{")?;

        while !self.at("}") {
            if self.at_end() {
                return Err(self.err("unterminated table type"));
            }

            if self.at("[") {
                self.bump();
                self.type_body()?;
                self.expect("]")?;
                self.expect(":")?;
                self.type_body()?;
            } else {
                // The `read` and `write` access modifiers come before the name.
                if matches!(self.text(), "read" | "write")
                    && matches!(self.kind_at(1), Some(TokKind::Ident))
                {
                    self.bump();
                }

                if self.at_name() && self.text_at(1) == ":" {
                    self.bump();
                    self.bump();
                    self.type_body()?;
                } else {
                    // This is an array style element.
                    self.type_body()?;
                }
            }

            if !self.eat(",") && !self.eat(";") {
                break;
            }
        }

        self.expect("}")?;

        Ok(())
    }
}
