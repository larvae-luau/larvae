/*!
`textDocument/semanticTokens/full`, the colours that an editor cannot guess.

A regular expression grammar colours a Luau file by shape. It cannot tell a
parameter from a global, or a type name from a variable of the same spelling.
larvae already answers those questions for its lints: [`crate::lint::scope`]
resolves the target of every name once per file. This module reuses that
answer and reports it to the editor.

The module holds pure functions. The server calls [`semantic_tokens`] with
the text of one document and writes the result. Nothing here reads a file or
a configuration.

The paint runs in four passes, and a later pass wins over an earlier one:

1. The lexer pass. It colours strings, numbers, keywords and operators.
2. The type pass. It colours the token spans that hold a type.
3. The scope pass. It colours declarations, reads and globals.
4. The tree pass. It colours what the tree names exactly: a function name, a
   field, a method, a type alias.

The order matters. The scope walk resolves `t` in `t.field` and knows nothing
about `field`, so the tree pass paints the field afterwards. The tree pass
knows that `local function f` declares a function, which is more exact than
the local variable that the scope walk reports, so it wins there too.
*/

use std::collections::HashSet;

use crate::lint::config::StdLib;
use crate::lint::globals;
use crate::lint::scope::{self, Names, Origin};
use crate::syntax::ast::*;
use crate::syntax::lexer::{self, Lexed, Tok, TokKind};
use crate::syntax::parser;

use super::rpc::Lines;

/// The token types and modifiers that this server reports, in index order.
pub struct Legend {
    pub types: Vec<&'static str>,
    pub modifiers: Vec<&'static str>,
}

/*
The legend, and it never changes order.

The encoded array carries an index into these lists and not a name. So a
client that read the legend at `initialize` keeps using it for the whole
session. To reorder a list here would recolour every open file wrongly. A new
entry goes on the end.

`enum` and `interface` have no Luau construct behind them yet. They stay in
the list so that the indexes after them do not move when one arrives.
*/
const TYPES: &[&str] = &[
    "namespace",
    "type",
    "class",
    "enum",
    "interface",
    "typeParameter",
    "parameter",
    "variable",
    "property",
    "function",
    "method",
    "keyword",
    "comment",
    "string",
    "number",
    "operator",
    "decorator",
];

const MODIFIERS: &[&str] = &[
    "declaration",
    "definition",
    "readonly",
    "static",
    "deprecated",
    "defaultLibrary",
];

const NAMESPACE: u32 = 0;
const TYPE: u32 = 1;
const CLASS: u32 = 2;
const TYPE_PARAMETER: u32 = 5;
const PARAMETER: u32 = 6;
const VARIABLE: u32 = 7;
const PROPERTY: u32 = 8;
const FUNCTION: u32 = 9;
const METHOD: u32 = 10;
const KEYWORD: u32 = 11;
const COMMENT: u32 = 12;
const STRING: u32 = 13;
const NUMBER: u32 = 14;
const OPERATOR: u32 = 15;
const DECORATOR: u32 = 16;

const DECLARATION: u32 = 1 << 0;
const READONLY: u32 = 1 << 2;
const DEPRECATED: u32 = 1 << 4;
const DEFAULT_LIBRARY: u32 = 1 << 5;

/// The legend that the server advertises. The client decodes every index with it.
pub fn legend() -> Legend {
    Legend {
        types: TYPES.to_vec(),
        modifiers: MODIFIERS.to_vec(),
    }
}

/*
The words that Luau reserves everywhere.

`continue`, `type`, `const`, `export`, `open` and `class` are missing on
purpose. Luau reads each of them as a keyword only where a statement can
start, and as a plain name everywhere else. The tree pass paints those,
because only the tree knows which reading applies.
*/
fn is_keyword(word: &str) -> bool {
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

/// The words that can stand in front of a declared name.
fn is_declaration_word(word: &str) -> bool {
    matches!(
        word,
        "class" | "const" | "export" | "extends" | "function" | "local" | "open" | "type"
    )
}

/*
Punctuation that groups, rather than an operator that computes.

The protocol has no `punctuation` type, and to call a comma an operator tells
the editor something false. So these stay uncoloured and the theme paints
them as plain text.
*/
fn is_punctuation(text: &str) -> bool {
    matches!(text, "," | ";" | "[" | "]" | "{" | "}" | "@")
}

/*
Globals that still work and that new code must not use.

Roblox keeps `wait`, `spawn`, `delay` and `tick` for old scripts and steers
authors to `task`. Luau keeps `getfenv`, `setfenv`, `loadstring` and
`newproxy` from Lua 5.1, and each one turns off an optimisation. The editor
strikes through a name with this modifier, which is the warning the author
needs at the moment they type it.
*/
const DEPRECATED_GLOBALS: &[&str] = &[
    "delay",
    "getfenv",
    "loadstring",
    "newproxy",
    "setfenv",
    "spawn",
    "tick",
    "wait",
];

/*
The encoded tokens of one document, in the relative form the protocol wants.

Five numbers per token: the line delta from the token before, the character
delta from the token before when both sit on one line, the length, the type
index and the modifier bits. Every count is in UTF-16 code units, because
that is what a position means in the protocol.

A file that does not lex gives an empty array. A file that lexes but does not
parse still gives the strings, numbers, comments, keywords and operators. An
author edits a file into a broken state on the way to a working one, and to
drop every colour on each keystroke makes the file flash.
*/
pub fn semantic_tokens(src: &str) -> Vec<u32> {
    semantic_tokens_for(src, StdLib::Roblox)
}

/*
The same, against the globals the project actually has.

A name reads as `defaultLibrary` only when the standard library of the
project defines it. A project on `std = "luau"` has no `game` and no
`Instance`, and to paint them as library names there tells the reader
something untrue about their own file.
*/
pub fn semantic_tokens_for(src: &str, std: StdLib) -> Vec<u32> {
    let Ok(lexed) = lexer::lex(src) else {
        return Vec::new();
    };

    let mut painter = Painter::new(src, &lexed.toks, std);
    painter.paint_lexed();

    if let Ok(chunk) = parse_any(src, &lexed.toks) {
        painter.walk_block(&chunk.block);
        painter.paint_types();

        let names = scope::resolve(src, &lexed.toks, &chunk);

        painter.paint_scope(&names);
        painter.paint_exact();
    }

    encode(src, &lexed, &painter.marks)
}

/*
Parse the text as Luau, and as a definitions file when that fails.

The request carries a document and no name, so nothing here says whether the
file is a `.d.luau`. Only a definitions file holds a `declare` statement, and
only a definitions file fails the ordinary parse because of one. So the
second parse costs nothing on a file that already parsed.
*/
fn parse_any(src: &str, toks: &[Tok]) -> Result<Chunk, parser::ParseError> {
    parser::parse(src, toks).or_else(|_| {
        parser::parse_with(
            src,
            toks,
            parser::ParseOptions {
                definitions: true,
                ..Default::default()
            },
        )
    })
}

// --- the paint -------------------------------------------------------------

struct Painter<'a> {
    src: &'a str,
    toks: &'a [Tok],
    /// The colour of each token, by token index.
    marks: Vec<Option<(u32, u32)>>,
    /// The token spans that hold a type expression.
    types: Vec<TokSpan>,
    /// The `<...>` lists that introduce type parameters.
    generics: Vec<TokSpan>,
    /// The colours that the tree knows exactly. They are painted last.
    exact: Vec<(u32, u32, u32)>,
    /// Every bare name in an expression. A global is one of these.
    name_exprs: Vec<u32>,
    /// The declaration token of each `const` binding.
    consts: HashSet<u32>,
    /// The declaration token of each class.
    classes: HashSet<u32>,
    /// The globals the project has, so `defaultLibrary` means what it says.
    std: StdLib,
}

impl<'a> Painter<'a> {
    fn new(src: &'a str, toks: &'a [Tok], std: StdLib) -> Self {
        Self {
            src,
            toks,
            std,
            marks: vec![None; toks.len()],
            types: Vec::new(),
            generics: Vec::new(),
            exact: Vec::new(),
            name_exprs: Vec::new(),
            consts: HashSet::new(),
            classes: HashSet::new(),
        }
    }

    fn text(&self, tok: u32) -> &'a str {
        self.toks[tok as usize].text(self.src)
    }

    fn set(&mut self, tok: u32, ty: u32, mods: u32) {
        if let Some(slot) = self.marks.get_mut(tok as usize) {
            *slot = Some((ty, mods));
        }
    }

    /// The word at `at`, if it is still inside `limit`.
    fn peek(&self, at: u32, limit: u32) -> Option<&'a str> {
        (at < limit && at < self.toks.len() as u32).then(|| self.text(at))
    }

    /// The first pass. It colours what one token decides on its own.
    fn paint_lexed(&mut self) {
        for i in 0..self.toks.len() as u32 {
            let colour = match self.toks[i as usize].kind {
                TokKind::Str { .. } | TokKind::InterpStr => Some(STRING),

                TokKind::Number => Some(NUMBER),

                TokKind::Ident if is_keyword(self.text(i)) => Some(KEYWORD),

                TokKind::Symbol if !is_punctuation(self.text(i)) => Some(OPERATOR),

                _ => None,
            };

            if let Some(ty) = colour {
                self.set(i, ty, 0);
            }
        }
    }

    /// The second pass. It colours the token spans that hold a type.
    fn paint_types(&mut self) {
        for span in std::mem::take(&mut self.generics) {
            self.paint_generic_span(span);
        }

        for span in std::mem::take(&mut self.types) {
            self.paint_type_span(span);
        }
    }

    /*
    The names inside one type.

    The tree keeps a type as a span and never reads its structure, so this
    pass reads the tokens. Three shapes decide the colour of a name: a name
    before a `.` names a module, a name before a `:` names a field of a table
    type or a parameter, and every other name is a type.
    */
    fn paint_type_span(&mut self, span: TokSpan) {
        let end = span.end.min(self.toks.len() as u32);

        for i in span.start..end {
            if self.toks[i as usize].kind != TokKind::Ident {
                continue;
            }

            let word = self.text(i);

            if is_keyword(word) || word == "typeof" {
                self.set(i, KEYWORD, 0);
                continue;
            }

            let ty = match self.peek(i + 1, end) {
                Some(".") => NAMESPACE,

                Some(":") => PROPERTY,

                _ => TYPE,
            };

            self.set(i, ty, 0);
        }
    }

    /*
    A `<T, U = string>` list.

    Its names are type parameters. A name after `=` is the default of the
    parameter before it, and a default is an ordinary type.
    */
    fn paint_generic_span(&mut self, span: TokSpan) {
        let end = span.end.min(self.toks.len() as u32);

        for i in span.start..end {
            if self.toks[i as usize].kind != TokKind::Ident {
                continue;
            }

            if i > span.start && matches!(self.text(i - 1), "=" | ".") {
                self.set(i, TYPE, 0);
            } else {
                self.set(i, TYPE_PARAMETER, DECLARATION);
            }
        }
    }

    /*
    The third pass. It reports what the scope walk resolved.

    Every bare name starts as a variable, so a global that the file itself
    defines still gets a colour. The declarations and the reads then paint
    over that default, because both are more exact.
    */
    fn paint_scope(&mut self, names: &Names<'_>) {
        for i in 0..self.name_exprs.len() {
            let tok = self.name_exprs[i];
            let mut mods = 0;

            if names.is_global(tok) {
                let word = self.text(tok);

                if globals::has(self.std, word) {
                    mods |= DEFAULT_LIBRARY;
                }

                if DEPRECATED_GLOBALS.contains(&word) {
                    mods |= DEPRECATED;
                }
            }

            self.set(tok, VARIABLE, mods);
        }

        for (&tok, &index) in &names.by_token {
            let ty = kind_of(names.bindings[index].origin);
            let mut mods = DECLARATION;

            if self.consts.contains(&tok) {
                mods |= READONLY;
            }

            self.set(tok, ty, mods);
        }

        /*
        A read takes the colour of the binding it resolves to. A parameter
        stays a parameter where it is used, and a call of `local function f`
        reads as a function. Both are what luau-lsp shows.
        */
        for (&tok, &index) in &names.read_of {
            let binding = &names.bindings[index];

            let ty = match binding.origin {
                Origin::Param | Origin::LocalFunction => kind_of(binding.origin),

                _ if self.classes.contains(&binding.declared_at) => CLASS,

                _ => VARIABLE,
            };

            self.set(tok, ty, 0);
        }
    }

    /// The last pass. The tree named these tokens, so nothing may paint over them.
    fn paint_exact(&mut self) {
        for (tok, ty, mods) in std::mem::take(&mut self.exact) {
            self.set(tok, ty, mods);
        }
    }

    // --- the walk ----------------------------------------------------------

    fn mark(&mut self, tok: u32, ty: u32, mods: u32) {
        self.exact.push((tok, ty, mods));
    }

    fn add_type(&mut self, ty: Option<TokSpan>) {
        if let Some(span) = ty {
            self.types.push(span);
        }
    }

    /*
    Colour the words that introduce a declared name.

    `type`, `const`, `export`, `class` and `open` are names anywhere else, so
    the lexer pass left them alone. The run in front of a declaration is
    short and every word in it is a keyword, so the walk steps back from the
    name until a word is not one.
    */
    fn declaration_words(&mut self, before: u32) {
        let mut i = before;

        while i > 0 {
            i -= 1;

            let tok = self.toks[i as usize];

            if tok.kind != TokKind::Ident || !is_declaration_word(tok.text(self.src)) {
                break;
            }

            self.mark(i, KEYWORD, 0);
        }
    }

    /// `@native` and its siblings. Each span holds the `@` and the name.
    fn attributes(&mut self, spans: &[TokSpan]) {
        for span in spans {
            for i in span.start..span.end {
                self.mark(i, DECORATOR, 0);
            }
        }
    }

    fn walk_block(&mut self, block: &Block) {
        for stmt in &block.stmts {
            self.walk_stmt(stmt);
        }
    }

    fn walk_stmt(&mut self, stmt: &Stmt) {
        match stmt {
            Stmt::Empty(_) | Stmt::Break(_) | Stmt::Continue(_) => {}

            Stmt::Local(n) => {
                if let Some(first) = n.names.first() {
                    self.declaration_words(first.name.start);
                }

                for binding in &n.names {
                    if n.is_const {
                        self.consts.insert(binding.name.start);
                    }

                    self.add_type(binding.ty);
                }

                for value in &n.values {
                    self.walk_expr(value);
                }
            }

            Stmt::Assign(n) => {
                for target in &n.targets {
                    self.walk_expr(target);
                }

                for value in &n.values {
                    self.walk_expr(value);
                }
            }

            Stmt::Call(e, _) => self.walk_expr(e),

            Stmt::Do(n) => self.walk_block(&n.block),

            Stmt::While(n) => {
                self.walk_expr(&n.cond);
                self.walk_block(&n.block);
            }

            Stmt::Repeat(n) => {
                self.walk_block(&n.block);
                self.walk_expr(&n.cond);
            }

            Stmt::If(n) => {
                for (cond, block) in &n.branches {
                    self.walk_expr(cond);
                    self.walk_block(block);
                }

                if let Some(block) = &n.else_block {
                    self.walk_block(block);
                }
            }

            Stmt::NumericFor(n) => {
                self.add_type(n.var.ty);
                self.walk_expr(&n.start);
                self.walk_expr(&n.limit);

                if let Some(step) = &n.step {
                    self.walk_expr(step);
                }

                self.walk_block(&n.block);
            }

            Stmt::GenericFor(n) => {
                for var in &n.vars {
                    self.add_type(var.ty);
                }

                for e in &n.exprs {
                    self.walk_expr(e);
                }

                self.walk_block(&n.block);
            }

            Stmt::Function(n) => {
                self.attributes(&n.attributes);

                if let Some(first) = n.path.first() {
                    self.declaration_words(first.start);
                }

                self.walk_path(&n.path, n.is_method);
                self.walk_body(&n.body);
            }

            Stmt::LocalFunction(n) => {
                self.attributes(&n.attributes);
                self.declaration_words(n.name.start);

                let mods = if n.is_const {
                    self.consts.insert(n.name.start);

                    DECLARATION | READONLY
                } else {
                    DECLARATION
                };

                self.mark(n.name.start, FUNCTION, mods);
                self.walk_body(&n.body);
            }

            Stmt::Return(n) => {
                for e in &n.values {
                    self.walk_expr(e);
                }
            }

            Stmt::TypeAlias(n) => self.walk_type_alias(n),

            Stmt::Class(n) => self.walk_class(n),

            Stmt::Declare(n) => self.walk_declare(n),
        }
    }

    /*
    `declare function f(...)`, `declare name: T` and `declare class N ... end`.

    A definitions file states what a host provides and holds no code. The tree
    keeps the whole statement as one span, so the walk reads the two words
    that open it and paints the rest as a type. A method name inside a
    declared class comes back as a type, which is the one place this is
    wrong, and no code runs from a `.d.luau` file.
    */
    fn walk_declare(&mut self, declare: &Declare) {
        let end = declare.span.end.min(self.toks.len() as u32);
        let mut at = declare.span.start;

        if at >= end {
            return;
        }

        self.mark(at, KEYWORD, 0);
        at += 1;

        /*
        The words between `declare` and the name. `extern type Name with` is
        the new solver's spelling of `class Name`, so it takes two of them.
        */
        let mut named = VARIABLE;

        while let Some(word) = self.peek(at, end) {
            named = match word {
                "function" => FUNCTION,

                "class" | "extern" | "type" => CLASS,

                _ => break,
            };

            self.mark(at, KEYWORD, 0);
            at += 1;
        }

        if at < end && self.toks[at as usize].kind == TokKind::Ident {
            self.mark(at, named, DECLARATION);
            at += 1;
        }

        self.types.push(TokSpan { start: at, end });
    }

    /*
    The name path of `function a.b:c()`.

    The first name is a value that the scope walk already resolved, so it
    keeps that colour. Every name between is a field of the table before it.
    The last name is what this statement defines.
    */
    fn walk_path(&mut self, path: &[TokSpan], is_method: bool) {
        let Some((last, middle)) = path.split_last() else {
            return;
        };

        for part in middle.iter().skip(1) {
            self.mark(part.start, PROPERTY, 0);
        }

        let ty = if is_method { METHOD } else { FUNCTION };

        self.mark(last.start, ty, DECLARATION);
    }

    fn walk_body(&mut self, body: &FunctionBody) {
        if let Some(generics) = body.generics {
            self.generics.push(generics);
        }

        for param in &body.params {
            self.add_type(param.ty);
        }

        self.add_type(body.ret_type);
        self.walk_block(&body.block);
    }

    /*
    `type Name<T> = ...`, and the `type function` form beside it.

    The tree keeps the name and the extent of the statement, and drops the
    right side. So the walk finds the `=` itself. The search counts angle
    brackets, because `type Pair<T = string> = ...` puts an earlier `=`
    inside the parameter list. No `=` means the `type function` form, whose
    body is code and not a type.
    */
    fn walk_type_alias(&mut self, alias: &TypeAlias) {
        self.declaration_words(alias.name.start);
        self.mark(alias.name.start, TYPE, DECLARATION);

        let end = alias.span.end.min(self.toks.len() as u32);
        let mut depth = 0i32;
        let mut equals = None;

        for i in alias.name.end..end {
            match self.text(i) {
                "<" => depth += 1,

                ">" => depth -= 1,

                "=" if depth == 0 => {
                    equals = Some(i);
                    break;
                }

                _ => {}
            }
        }

        let Some(equals) = equals else {
            return;
        };

        if self.peek(alias.name.end, end) == Some("<") {
            self.generics.push(TokSpan {
                start: alias.name.end,
                end: equals,
            });
        }

        self.types.push(TokSpan {
            start: equals + 1,
            end,
        });
    }

    fn walk_class(&mut self, class: &Class) {
        self.declaration_words(class.name.start);
        self.classes.insert(class.name.start);
        self.mark(class.name.start, CLASS, DECLARATION);

        if let Some(base) = class.extends {
            self.declaration_words(base.start);
            self.mark(base.start, CLASS, 0);
        }

        for member in &class.members {
            match member {
                ClassMember::Field { name, ty, .. } => {
                    self.mark(name.start, PROPERTY, DECLARATION);
                    self.add_type(*ty);
                }

                ClassMember::Method(f) => {
                    self.attributes(&f.attributes);

                    if let Some(name) = f.path.last() {
                        self.mark(name.start, METHOD, DECLARATION);
                    }

                    self.walk_body(&f.body);
                }
            }
        }
    }

    fn walk_expr(&mut self, e: &Expr) {
        match e {
            Expr::Nil(_)
            | Expr::True(_)
            | Expr::False(_)
            | Expr::Vararg(_)
            | Expr::Number(_)
            | Expr::String(_)
            | Expr::InterpString(_) => {}

            Expr::Name(span) => self.name_exprs.push(span.start),

            Expr::Function {
                attributes, body, ..
            } => {
                self.attributes(attributes);
                self.walk_body(body);
            }

            Expr::Table { fields, .. } => {
                for field in fields {
                    match field {
                        TableField::Positional(v) => self.walk_expr(v),

                        TableField::Named { name, value } => {
                            self.mark(name.start, PROPERTY, DECLARATION);
                            self.walk_expr(value);
                        }

                        TableField::Computed { key, value } => {
                            self.walk_expr(key);
                            self.walk_expr(value);
                        }
                    }
                }
            }

            Expr::Binary { lhs, rhs, .. } => {
                self.walk_expr(lhs);
                self.walk_expr(rhs);
            }

            Expr::Unary { operand, .. } => self.walk_expr(operand),

            Expr::Paren { inner, .. } => self.walk_expr(inner),

            Expr::Index { object, key, .. } => {
                self.walk_expr(object);

                match key {
                    IndexKey::Field(span) => self.mark(span.start, PROPERTY, 0),

                    IndexKey::Computed(k) => self.walk_expr(k),
                }
            }

            Expr::Call {
                func,
                method,
                type_args,
                args,
                ..
            } => {
                self.walk_expr(func);

                if let Some(name) = method {
                    self.mark(name.start, METHOD, 0);
                }

                self.add_type(*type_args);

                match args {
                    CallArgs::Paren(list) => {
                        for a in list {
                            self.walk_expr(a);
                        }
                    }

                    CallArgs::Table(t) => self.walk_expr(t),

                    CallArgs::Str(_) => {}
                }
            }

            Expr::IfElse {
                branches,
                else_value,
                ..
            } => {
                for (cond, value) in branches {
                    self.walk_expr(cond);
                    self.walk_expr(value);
                }

                self.walk_expr(else_value);
            }

            Expr::TypeAssert { expr, ty, .. } => {
                self.walk_expr(expr);
                self.types.push(*ty);
            }
        }
    }
}

/// The token type that matches the construct which introduced a binding.
fn kind_of(origin: Origin) -> u32 {
    match origin {
        Origin::Param => PARAMETER,

        Origin::LocalFunction => FUNCTION,

        Origin::Local | Origin::Loop => VARIABLE,
    }
}

// --- the encoding ----------------------------------------------------------

/*
Turn the coloured tokens into the array that the protocol carries.

Two rules shape this step. A token may not cross a line, because a client
accepts one only when it asked for that ability, so a long string or a block
comment is cut into one piece per line. And each piece is written as a delta
from the piece before it, which is what makes the array small.
*/
fn encode(src: &str, lexed: &Lexed, marks: &[Option<(u32, u32)>]) -> Vec<u32> {
    let mut raw: Vec<(u32, u32, u32, u32)> = Vec::with_capacity(marks.len());

    for (i, mark) in marks.iter().enumerate() {
        if let Some((ty, mods)) = *mark {
            let tok = lexed.toks[i];

            raw.push((tok.start, tok.end, ty, mods));
        }
    }

    // A comment is not a token, so the lexer keeps its range on the side.
    for &(start, end) in &lexed.comments {
        raw.push((start, end, COMMENT, 0));
    }

    raw.sort_unstable_by_key(|&(start, end, ..)| (start, end));

    let lines = Lines::new(src);
    let mut out = Vec::with_capacity(raw.len() * 5);
    let mut prev_line = 0u32;
    let mut prev_char = 0u32;

    for (start, end, ty, mods) in raw {
        for (from, to) in line_pieces(src, start, end) {
            let (line, character) = lines.position(src, from);
            let (_, end_character) = lines.position(src, to);
            let length = end_character.saturating_sub(character);

            // An empty piece would move the cursor and colour nothing.
            if length == 0 {
                continue;
            }

            let delta_line = line - prev_line;

            let delta_start = if delta_line == 0 {
                character - prev_char
            } else {
                character
            };

            out.extend_from_slice(&[delta_line, delta_start, length, ty, mods]);

            prev_line = line;
            prev_char = character;
        }
    }

    out
}

/*
Cut a byte range into one piece per line.

The line break itself is dropped, and so is the carriage return in front of
it. A line comment on a Windows checkout ends after that carriage return, and
to colour it would put the token one code unit past the end of the line.
*/
fn line_pieces(src: &str, start: u32, end: u32) -> Vec<(u32, u32)> {
    let text = &src[start as usize..end as usize];

    if !text.contains('\n') {
        return vec![(start, trimmed_end(src, start, end))];
    }

    let mut out = Vec::new();
    let mut from = start;

    for (offset, ch) in text.char_indices() {
        if ch != '\n' {
            continue;
        }

        let at = start + offset as u32;
        let to = trimmed_end(src, from, at);

        if to > from {
            out.push((from, to));
        }

        from = at + 1;
    }

    let to = trimmed_end(src, from, end);

    if to > from {
        out.push((from, to));
    }

    out
}

/// The end of a piece, without the carriage return of a CRLF line ending.
fn trimmed_end(src: &str, from: u32, end: u32) -> u32 {
    if end > from && src.as_bytes()[end as usize - 1] == b'\r' {
        end - 1
    } else {
        end
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One token in the absolute form, decoded back from the wire array.
    #[derive(Debug, PartialEq, Eq)]
    struct Absolute {
        line: u32,
        character: u32,
        length: u32,
        ty: &'static str,
        mods: Vec<&'static str>,
    }

    /// Undo the relative encoding. Every test reads the result through this.
    fn decode(src: &str) -> Vec<Absolute> {
        let data = semantic_tokens(src);

        assert_eq!(data.len() % 5, 0, "the array holds five numbers per token");

        let legend = legend();
        let mut out = Vec::new();
        let mut line = 0u32;
        let mut character = 0u32;

        let (groups, _) = data.as_chunks::<5>();

        for group in groups {
            let (delta_line, delta_start) = (group[0], group[1]);

            line += delta_line;

            character = if delta_line == 0 {
                character + delta_start
            } else {
                delta_start
            };

            let mods = (0..legend.modifiers.len())
                .filter(|bit| group[4] & (1 << bit) != 0)
                .map(|bit| legend.modifiers[bit])
                .collect();

            out.push(Absolute {
                line,
                character,
                length: group[2],
                ty: legend.types[group[3] as usize],
                mods,
            });
        }

        out
    }

    /// The line and UTF-16 character of the byte at `at`.
    fn place(src: &str, at: usize) -> (u32, u32) {
        let before = &src[..at];
        let line = before.matches('\n').count() as u32;
        let start = before.rfind('\n').map_or(0, |n| n + 1);
        let character = src[start..at].chars().map(char::len_utf16).sum::<usize>();

        (line, character as u32)
    }

    /// The place of the first `needle` in `src`.
    fn find(src: &str, needle: &str) -> (u32, u32) {
        let at = src.find(needle).unwrap_or_else(|| panic!("no {needle:?}"));

        place(src, at)
    }

    /// The token that starts at the first occurrence of `needle`.
    fn at<'t>(tokens: &'t [Absolute], src: &str, needle: &str) -> &'t Absolute {
        let (line, character) = find(src, needle);

        tokens
            .iter()
            .find(|t| t.line == line && t.character == character)
            .unwrap_or_else(|| panic!("no token at {needle:?}, line {line} char {character}"))
    }

    /// Every token that starts at an occurrence of `needle`, in source order.
    fn all<'t>(tokens: &'t [Absolute], src: &str, needle: &str) -> Vec<&'t Absolute> {
        let mut out = Vec::new();
        let mut cursor = 0usize;

        while let Some(offset) = src[cursor..].find(needle) {
            let byte = cursor + offset;
            let (line, character) = place(src, byte);

            if let Some(token) = tokens
                .iter()
                .find(|t| t.line == line && t.character == character)
            {
                out.push(token);
            }

            cursor = byte + needle.len();
        }

        out
    }

    #[test]
    fn the_legend_lists_every_type_and_modifier() {
        let legend = legend();

        assert_eq!(legend.types.len(), 17);
        assert_eq!(legend.modifiers.len(), 6);
        assert_eq!(legend.types[0], "namespace");
        assert_eq!(legend.types[16], "decorator");
        assert_eq!(legend.modifiers[5], "defaultLibrary");
    }

    /// The legend order is a promise to the client, so the indexes are pinned.
    #[test]
    fn the_legend_indexes_never_move() {
        let legend = legend();

        for (index, name) in [
            (NAMESPACE, "namespace"),
            (TYPE, "type"),
            (CLASS, "class"),
            (TYPE_PARAMETER, "typeParameter"),
            (PARAMETER, "parameter"),
            (VARIABLE, "variable"),
            (PROPERTY, "property"),
            (FUNCTION, "function"),
            (METHOD, "method"),
            (KEYWORD, "keyword"),
            (COMMENT, "comment"),
            (STRING, "string"),
            (NUMBER, "number"),
            (OPERATOR, "operator"),
            (DECORATOR, "decorator"),
        ] {
            assert_eq!(legend.types[index as usize], name);
        }
    }

    #[test]
    fn an_empty_file_gives_nothing() {
        assert!(semantic_tokens("").is_empty());
    }

    /*
    A file that the lexer refuses gives nothing.

    An unterminated string swallows the rest of the file, so there is no
    token stream to colour and no honest guess to make.
    */
    #[test]
    fn a_file_that_does_not_lex_gives_nothing() {
        assert!(semantic_tokens("local s = \"open").is_empty());
        assert!(semantic_tokens("local s = [[open").is_empty());
    }

    /*
    A half typed file keeps the colours that one token decides.

    The author is between two working states. To drop every colour would make
    the file flash on each keystroke.
    */
    #[test]
    fn a_file_that_does_not_parse_still_gives_lexer_colours() {
        let src = "-- note\nlocal x = 1 +\n";
        let tokens = decode(src);

        assert!(!tokens.is_empty());
        assert_eq!(at(&tokens, src, "-- note").ty, "comment");
        assert_eq!(at(&tokens, src, "local").ty, "keyword");
        assert_eq!(at(&tokens, src, "1").ty, "number");
        assert_eq!(at(&tokens, src, "+").ty, "operator");

        // No pass resolved the name, so nothing claims that it is a variable.
        assert!(tokens.iter().all(|t| t.ty != "variable"));
    }

    #[test]
    fn a_broken_file_never_panics() {
        for src in [
            "end",
            "local",
            "function",
            "local x = {",
            "a.b.c",
            "if then else",
            "type = = =",
            "for i =",
            "return return",
            "::",
            "@",
            "local x: = 1",
            "function t:",
            "class",
            "type",
            "const",
        ] {
            let _ = semantic_tokens(src);
        }
    }

    /*
    The relative encoding is the part that is easy to get wrong.

    So the test decodes the array back to absolute positions and checks each
    one against the place of that text in the source.
    */
    #[test]
    fn the_relative_encoding_decodes_back_to_the_source_positions() {
        let src = "local alpha = 1\nlocal beta = alpha\nprint(beta)\n";
        let tokens = decode(src);

        for (needle, line, character, length) in [
            ("local", 0, 0, 5),
            ("alpha", 0, 6, 5),
            ("=", 0, 12, 1),
            ("1", 0, 14, 1),
            ("beta", 1, 6, 4),
            ("print", 2, 0, 5),
        ] {
            let found = at(&tokens, src, needle);

            assert_eq!((found.line, found.character), (line, character), "{needle}");
            assert_eq!(found.length, length, "{needle}");
        }

        // Every token lands on the text that it claims to cover.
        for token in &tokens {
            let (line, character) = (token.line, token.character);

            assert!(
                tokens
                    .iter()
                    .filter(|t| t.line == line && t.character == character)
                    .count()
                    == 1,
                "two tokens start at line {line} char {character}"
            );
        }
    }

    /// A file with wide characters counts positions in UTF-16 code units.
    #[test]
    fn positions_count_utf16_code_units() {
        let src = "local s = \"\u{1F600}\"\nlocal n = 1\n";
        let tokens = decode(src);
        let string = at(&tokens, src, "\"");

        // Two quotes and one emoji, and the emoji is a surrogate pair.
        assert_eq!(string.ty, "string");
        assert_eq!(string.length, 4);

        // The line after it starts again from character zero.
        assert_eq!(at(&tokens, src, "local n").line, 1);
        assert_eq!(at(&tokens, src, "local n").character, 0);
    }

    #[test]
    fn every_type_index_is_inside_the_legend() {
        let src = "\
--!strict
-- a note
local Types = require(script.Types)
type Pair<T> = { first: T, second: T }
const answer = 42
local function make(count: number, label: string): Pair<number>
\tlocal sum = count + 1
\treturn { first = sum, second = label }
end
local t = {}
function t.helper() end
function t:method() end
t.field = make(1, \"two\")
for i = 1, 10 do print(i) end
local ok = t.field :: any
print(answer, Types, ok)
";
        let data = semantic_tokens(src);
        let legend = legend();
        let modifier_mask = (1u32 << legend.modifiers.len()) - 1;

        assert!(!data.is_empty());

        let (groups, _) = data.as_chunks::<5>();

        for group in groups {
            assert!(
                (group[3] as usize) < legend.types.len(),
                "type index {} is out of the legend",
                group[3]
            );

            assert_eq!(
                group[4] & !modifier_mask,
                0,
                "modifier bits {} are out of the legend",
                group[4]
            );
        }
    }

    /// The protocol needs tokens in order, and a client draws an overlap wrongly.
    #[test]
    fn tokens_are_sorted_and_never_overlap() {
        let src = "\
--[[ a long
comment over lines ]]
local text = [[a long
string over lines]]
local n = 0xFF
function f(a, b) return a + b end
print(f(n, 1), text)
";
        let tokens = decode(src);

        assert!(!tokens.is_empty());

        for pair in tokens.windows(2) {
            let (a, b) = (&pair[0], &pair[1]);

            assert!(
                (a.line, a.character) < (b.line, b.character),
                "{a:?} does not come before {b:?}"
            );

            if a.line == b.line {
                assert!(a.character + a.length <= b.character, "{a:?} meets {b:?}");
            }
        }
    }

    /// A token that crosses a line is cut, because a client need not accept one.
    #[test]
    fn a_multiline_token_is_cut_into_one_piece_per_line() {
        let src = "local text = [[one\ntwo]]\n";
        let tokens = decode(src);
        let pieces: Vec<_> = tokens.iter().filter(|t| t.ty == "string").collect();

        assert_eq!(pieces.len(), 2);
        assert_eq!(pieces[0].line, 0);
        assert_eq!(pieces[0].character, 13);
        assert_eq!(pieces[0].length, "[[one".len() as u32);
        assert_eq!(pieces[1].line, 1);
        assert_eq!(pieces[1].character, 0);
        assert_eq!(pieces[1].length, "two]]".len() as u32);
    }

    #[test]
    fn a_declaration_is_marked_and_a_read_is_not() {
        let src = "local counter = 1\nprint(counter)\n";
        let tokens = decode(src);
        let declaration = at(&tokens, src, "counter");

        assert_eq!(declaration.ty, "variable");
        assert_eq!(declaration.mods, vec!["declaration"]);

        let read = all(&tokens, src, "counter")[1];

        assert_eq!(read.ty, "variable");
        assert!(read.mods.is_empty());
    }

    #[test]
    fn a_const_binding_is_read_only_and_a_local_is_not() {
        let src = "const fixed = 1\nlocal loose = 2\nprint(fixed, loose)\n";
        let tokens = decode(src);

        assert_eq!(
            at(&tokens, src, "fixed").mods,
            vec!["declaration", "readonly"]
        );

        assert_eq!(at(&tokens, src, "loose").mods, vec!["declaration"]);

        // The modifier belongs to the declaration and not to every use.
        assert!(all(&tokens, src, "fixed")[1].mods.is_empty());
    }

    /// `const` is a name anywhere else, so only the tree can call it a keyword.
    #[test]
    fn a_contextual_keyword_is_a_keyword_where_it_declares() {
        let src = "const fixed = 1\nexport type Alias = number\nlocal const = 2\n";
        let tokens = decode(src);

        assert_eq!(at(&tokens, src, "const").ty, "keyword");
        assert_eq!(at(&tokens, src, "export").ty, "keyword");
        assert_eq!(at(&tokens, src, "type").ty, "keyword");

        // The last line binds a variable that happens to spell `const`.
        let bound = all(&tokens, src, "const")[1];

        assert_eq!(bound.ty, "variable");
        assert_eq!(bound.mods, vec!["declaration"]);
    }

    #[test]
    fn a_parameter_keeps_its_kind_where_it_is_used() {
        let src = "local function add(left, right)\n\treturn left + right\nend\n";
        let tokens = decode(src);

        assert_eq!(at(&tokens, src, "left").ty, "parameter");
        assert_eq!(at(&tokens, src, "left").mods, vec!["declaration"]);

        let read = all(&tokens, src, "left")[1];

        assert_eq!(read.ty, "parameter");
        assert!(read.mods.is_empty());
    }

    #[test]
    fn a_function_name_is_a_function_and_a_method_name_is_a_method() {
        let src = "\
local function helper() end
local t = {}
function t.plain() end
function t:greet() end
helper()
t:greet()
";
        let tokens = decode(src);

        assert_eq!(at(&tokens, src, "helper").ty, "function");
        assert_eq!(at(&tokens, src, "helper").mods, vec!["declaration"]);
        assert_eq!(all(&tokens, src, "helper")[1].ty, "function");

        assert_eq!(at(&tokens, src, "plain").ty, "function");
        assert_eq!(at(&tokens, src, "greet").ty, "method");
        assert_eq!(at(&tokens, src, "greet").mods, vec!["declaration"]);
        assert_eq!(all(&tokens, src, "greet")[1].ty, "method");

        // `t` in `function t.plain()` is the local, not a part of the name.
        assert_eq!(all(&tokens, src, "t")[1].ty, "variable");
    }

    #[test]
    fn a_field_read_is_a_property() {
        let src = "local t = { key = 1 }\nprint(t.key)\n";
        let tokens = decode(src);

        assert_eq!(at(&tokens, src, "key").ty, "property");
        assert_eq!(at(&tokens, src, "key").mods, vec!["declaration"]);

        let read = all(&tokens, src, "key")[1];

        assert_eq!(read.ty, "property");
        assert!(read.mods.is_empty());
    }

    #[test]
    fn a_stdlib_global_carries_the_default_library_modifier() {
        let src = "print(math.pi)\nlocal print = 1\nprint(print)\n";
        let tokens = decode(src);

        assert_eq!(at(&tokens, src, "print").ty, "variable");
        assert_eq!(at(&tokens, src, "print").mods, vec!["defaultLibrary"]);
        assert_eq!(at(&tokens, src, "math").mods, vec!["defaultLibrary"]);
        assert_eq!(at(&tokens, src, "pi").ty, "property");

        // The file shadows the name, so the later uses are the local.
        let shadow = all(&tokens, src, "print")[1];

        assert_eq!(shadow.ty, "variable");
        assert_eq!(shadow.mods, vec!["declaration"]);
        assert!(all(&tokens, src, "print")[2].mods.is_empty());
    }

    /// A global that still works and that new code must not use.
    #[test]
    fn a_deprecated_global_is_marked() {
        let src = "wait(1)\n";
        let tokens = decode(src);

        assert_eq!(
            at(&tokens, src, "wait").mods,
            vec!["deprecated", "defaultLibrary"]
        );
    }

    #[test]
    fn a_type_alias_declares_a_type_and_an_annotation_reads_one() {
        let src = "type Point = { x: number }\nlocal p: Point = { x = 1 }\nprint(p)\n";
        let tokens = decode(src);

        assert_eq!(at(&tokens, src, "Point").ty, "type");
        assert_eq!(at(&tokens, src, "Point").mods, vec!["declaration"]);

        let reference = all(&tokens, src, "Point")[1];

        assert_eq!(reference.ty, "type");
        assert!(reference.mods.is_empty());

        // The field of a table type is a property, and its type is a type.
        assert_eq!(at(&tokens, src, "x: number").ty, "property");
        assert_eq!(at(&tokens, src, "number").ty, "type");
    }

    #[test]
    fn a_dotted_type_names_a_module_first() {
        let src = "local Types = require(script)\ntype Id = Types.Id\n";
        let tokens = decode(src);

        assert_eq!(all(&tokens, src, "Types")[1].ty, "namespace");
        assert_eq!(all(&tokens, src, "Id")[1].ty, "type");
    }

    #[test]
    fn a_generic_parameter_is_a_type_parameter() {
        let src = "type Box<T> = { value: T }\n";
        let tokens = decode(src);

        assert_eq!(at(&tokens, src, "T>").ty, "typeParameter");
        assert_eq!(at(&tokens, src, "T>").mods, vec!["declaration"]);
        assert_eq!(at(&tokens, src, "T }").ty, "type");
    }

    #[test]
    fn a_class_declares_a_class_and_its_members() {
        let src = "\
class Shape
\tpublic sides: number
\tfunction area(self) return 0 end
end
local s = Shape
print(s)
";
        let tokens = decode(src);

        assert_eq!(at(&tokens, src, "class").ty, "keyword");
        assert_eq!(at(&tokens, src, "Shape").ty, "class");
        assert_eq!(at(&tokens, src, "Shape").mods, vec!["declaration"]);
        assert_eq!(at(&tokens, src, "sides").ty, "property");
        assert_eq!(at(&tokens, src, "area").ty, "method");
        assert_eq!(all(&tokens, src, "Shape")[1].ty, "class");
    }

    #[test]
    fn an_attribute_is_a_decorator() {
        let src = "@native\nlocal function fast() end\nfast()\n";
        let tokens = decode(src);

        assert_eq!(at(&tokens, src, "@").ty, "decorator");
        assert_eq!(at(&tokens, src, "native").ty, "decorator");
    }

    /// A comment is not a token, so it comes from the side list the lexer keeps.
    #[test]
    fn comments_are_coloured_in_both_forms() {
        let src = "-- line\nlocal x = 1 --[[ block ]] + 2\nprint(x)\n";
        let tokens = decode(src);

        assert_eq!(at(&tokens, src, "-- line").ty, "comment");
        assert_eq!(at(&tokens, src, "--[[").ty, "comment");
    }

    /// Grouping punctuation has no type in the protocol, so it stays uncoloured.
    #[test]
    fn punctuation_is_not_an_operator() {
        let src = "local t = { 1, 2 }\nprint(t)\n";
        let tokens = decode(src);
        let (line, character) = find(src, "{");

        assert!(
            !tokens
                .iter()
                .any(|t| t.line == line && t.character == character),
            "the brace got a colour"
        );

        assert_eq!(at(&tokens, src, "=").ty, "operator");
    }

    /// A CRLF checkout must not put a comment one code unit past the line end.
    #[test]
    fn a_windows_line_ending_stays_out_of_a_comment() {
        let src = "-- note\r\nlocal x = 1\r\nprint(x)\r\n";
        let tokens = decode(src);
        let comment = at(&tokens, src, "-- note");

        assert_eq!(comment.ty, "comment");
        assert_eq!(comment.length, "-- note".len() as u32);
    }

    /*
    A definitions file colours too.

    The request carries no file name, so the pass parses the text as Luau
    first and as a definitions file only when that fails.
    */
    #[test]
    fn a_definitions_file_colours_its_declarations() {
        let src = "\
declare function hello(name: string): string
declare version: number
declare class Widget
\tsize: number
end
";
        let tokens = decode(src);

        assert_eq!(at(&tokens, src, "declare").ty, "keyword");
        assert_eq!(at(&tokens, src, "hello").ty, "function");
        assert_eq!(at(&tokens, src, "hello").mods, vec!["declaration"]);
        assert_eq!(at(&tokens, src, "name").ty, "property");
        assert_eq!(at(&tokens, src, "string)").ty, "type");
        assert_eq!(at(&tokens, src, "version").ty, "variable");
        assert_eq!(at(&tokens, src, "Widget").ty, "class");
        assert_eq!(at(&tokens, src, "size").ty, "property");
    }

    /// The result of one source text never changes between runs.
    #[test]
    fn the_result_is_stable() {
        let src = "\
local a, b = 1, 2
local function f(x: number): number return x + a + b end
print(f(3))
";
        let first = semantic_tokens(src);

        for _ in 0..8 {
            assert_eq!(semantic_tokens(src), first);
        }
    }
}

#[cfg(test)]
mod standard_library {
    use super::*;

    /// The `defaultLibrary` bit follows the standard library of the project.
    #[test]
    fn a_roblox_global_is_not_a_library_name_under_plain_luau() {
        let src = "print(game)\n";

        let bit = |std| {
            let data = semantic_tokens_for(src, std);

            // Find the `game` token: the second name on the line.
            data.chunks(5)
                .find(|t| t[2] == 4)
                .map(|t| t[4] & DEFAULT_LIBRARY)
                .unwrap_or(0)
        };

        assert_ne!(bit(StdLib::Roblox), 0, "game is a Roblox global");
        assert_eq!(bit(StdLib::Luau), 0, "plain Luau has no game");
    }

    /// A global both libraries define keeps the bit either way.
    #[test]
    fn a_shared_global_is_a_library_name_in_both() {
        for std in [StdLib::Roblox, StdLib::Luau] {
            let data = semantic_tokens_for("print(1)\n", std);

            assert!(
                data.chunks(5).any(|t| t[4] & DEFAULT_LIBRARY != 0),
                "print is a library name under {std:?}"
            );
        }
    }
}
