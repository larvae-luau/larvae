/*!
A recursive descent parser for the full Luau grammar, modeled on the
official Parser.cpp. It produces the token span tree in
[`crate::syntax::ast`].

The parser reads types for their extent, but it does not interpret them.
That is intentional. A rule that needs type structure can parse the span
later. The recursion has a depth guard. So pathological nesting is a clean
error and never a crash.
*/

use crate::syntax::ast::*;
use crate::syntax::lexer::{Tok, TokKind};

mod expr;
mod stmt;
mod types;
#[derive(Debug)]
pub struct ParseError {
    pub offset: usize,
    pub message: String,
}

/// The nesting limit. It is deep enough for real code, and shallow enough to
/// protect the stack. The crash class of darklua does not exist here.
const MAX_DEPTH: u32 = 180;
const UNARY_PRIORITY: u8 = 12;

pub fn parse(src: &str, toks: &[Tok]) -> Result<Chunk, ParseError> {
    let mut p = Parser {
        src,
        toks,
        pos: 0,
        depth: 0,
    };

    let block = p.block()?;

    if !p.at_end() {
        return Err(p.err("unexpected token"));
    }

    Ok(Chunk { block })
}

/// Parses one expression that covers the whole token stream. Use this for a
/// source slice that another caller cut, for example the `host` span of a
/// worm that holds an attribute value.
pub fn parse_expr(src: &str, toks: &[Tok]) -> Result<Expr, ParseError> {
    let mut p = Parser {
        src,
        toks,
        pos: 0,
        depth: 0,
    };

    let expr = p.expr()?;

    if !p.at_end() {
        return Err(p.err("unexpected token after the expression"));
    }

    Ok(expr)
}

struct Parser<'a> {
    src: &'a str,
    toks: &'a [Tok],
    pos: usize,
    depth: u32,
}

impl<'a> Parser<'a> {
    // --- token access ------------------------------------------------------

    fn at_end(&self) -> bool {
        self.pos >= self.toks.len()
    }

    fn text_at(&self, n: usize) -> &'a str {
        match self.toks.get(self.pos + n) {
            Some(t) => t.text(self.src),

            None => "",
        }
    }

    fn text(&self) -> &'a str {
        self.text_at(0)
    }

    fn kind_at(&self, n: usize) -> Option<TokKind> {
        self.toks.get(self.pos + n).map(|t| t.kind)
    }

    fn at(&self, s: &str) -> bool {
        self.text() == s
    }

    fn at_name(&self) -> bool {
        matches!(self.kind_at(0), Some(TokKind::Ident)) && !is_reserved(self.text())
    }

    fn bump(&mut self) -> usize {
        let i = self.pos;
        self.pos += 1;

        i
    }

    fn eat(&mut self, s: &str) -> bool {
        if self.at(s) {
            self.bump();
            true
        } else {
            false
        }
    }

    fn expect(&mut self, s: &str) -> Result<usize, ParseError> {
        if self.at(s) {
            Ok(self.bump())
        } else {
            Err(self.err(&format!("expected `{s}`, found {}", self.found())))
        }
    }

    fn expect_name(&mut self) -> Result<TokSpan, ParseError> {
        if self.at_name() {
            let i = self.bump();
            Ok(TokSpan::new(i, i + 1))
        } else {
            Err(self.err(&format!("expected a name, found {}", self.found())))
        }
    }

    fn found(&self) -> String {
        if self.at_end() {
            "end of file".to_string()
        } else {
            format!("`{}`", self.text())
        }
    }

    fn err(&self, message: &str) -> ParseError {
        let offset = match self.toks.get(self.pos) {
            Some(t) => t.start as usize,

            None => self.src.len(),
        };

        ParseError {
            offset,
            message: message.to_string(),
        }
    }

    fn enter(&mut self) -> Result<(), ParseError> {
        self.depth += 1;

        if self.depth > MAX_DEPTH {
            return Err(self.err("expression or statement nests too deeply"));
        }

        Ok(())
    }

    fn leave(&mut self) {
        self.depth -= 1;
    }
}

fn is_reserved(word: &str) -> bool {
    matches!(
        word,
        "and"
            | "break"
            | "do"
            | "else"
            | "elseif"
            | "end"
            | "false"
            | "for"
            | "function"
            | "if"
            | "in"
            | "local"
            | "nil"
            | "not"
            | "or"
            | "repeat"
            | "return"
            | "then"
            | "true"
            | "until"
            | "while"
    )
}

fn is_unary_op(s: &str) -> bool {
    matches!(s, "not" | "-" | "#")
}

fn is_compound_op(s: &str) -> bool {
    matches!(s, "+=" | "-=" | "*=" | "/=" | "%=" | "^=" | "..=" | "//=")
}

/// The left and right binding power. A right value lower than the left value
/// means the operator is right associative.
fn binop_priority(s: &str) -> Option<(u8, u8)> {
    Some(match s {
        "or" => (1, 1),

        "and" => (2, 2),

        "<" | ">" | "<=" | ">=" | "~=" | "==" => (3, 3),

        ".." => (9, 8),

        "+" | "-" => (10, 10),

        "*" | "/" | "//" | "%" => (11, 11),

        "^" => (14, 13),

        _ => return None,
    })
}
