/*!
The shape of one document: the folds, the selection chain, and the outline.

These three requests answer from the larvae parser alone. They need no type
information, so they stay correct in a project that has no analyzer, and they
answer in the microseconds that one parse costs.

Each function takes the source text and returns plain data. The caller turns
byte offsets into protocol positions, because only the caller knows the line
table of the document it holds. A file that does not parse gives an empty
result. An editor asks for these on every keystroke, and half of those
keystrokes leave the file broken.
*/

use crate::lint::ctx::flatten;
use crate::syntax::ast::{
    Block, ClassMember, Expr, Function, Local, Stmt, TableField, TokSpan, TypeAlias,
};
use crate::syntax::lexer::Tok;

use super::rpc::Lines;

/*
The symbol kinds of the protocol, by their numbers.

A type alias reports as `Interface`. The two candidates are `Interface` and
`TypeParameter`. `TypeParameter` names the `T` of a generic declaration, so a
reader who trusts the icon reads `type Point = ...` as a parameter of
something else. `Interface` names a type that stands on its own, which is what
an alias is.
*/
pub const METHOD: u8 = 6;
pub const FIELD: u8 = 8;
pub const INTERFACE: u8 = 11;
pub const FUNCTION: u8 = 12;
pub const VARIABLE: u8 = 13;
pub const CONSTANT: u8 = 14;
pub const CLASS: u8 = 5;

/// One region that the editor can collapse. The lines are zero based.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FoldRange {
    pub start: u32,
    pub end: u32,
    /// `"comment"` for a block of comments, `None` for a region of code.
    pub kind: Option<&'static str>,
}

/// One node of the outline. Both ranges are byte offsets into the source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Symbol {
    pub name: String,
    pub kind: u8,
    /// The whole declaration, the body included.
    pub range: (u32, u32),
    /// The name alone; the editor reveals this part.
    pub selection: (u32, u32),
    pub children: Vec<Symbol>,
}

/*
The source with the two tables that every span lookup needs.

The tree holds token indexes, not bytes. So each answer here converts twice:
once from a token index to a byte offset, and once from a byte offset to a
line. Both tables are built once per request.
*/
struct Src<'a> {
    src: &'a str,
    toks: &'a [Tok],
    lines: Lines,
}

impl<'a> Src<'a> {
    fn new(src: &'a str, toks: &'a [Tok]) -> Self {
        Self {
            src,
            toks,
            lines: Lines::new(src),
        }
    }

    /// The half open byte range of a token span. An empty span gives a point.
    fn bytes(&self, span: TokSpan) -> (u32, u32) {
        if span.is_empty() {
            let at = self
                .toks
                .get(span.start as usize)
                .map_or(self.src.len() as u32, |t| t.start);

            return (at, at);
        }

        (
            self.toks[span.start as usize].start,
            self.toks[span.end as usize - 1].end,
        )
    }

    fn text(&self, span: TokSpan) -> &'a str {
        let (a, b) = self.bytes(span);

        &self.src[a as usize..b as usize]
    }

    fn line(&self, byte: u32) -> u32 {
        self.lines.position(self.src, byte).0
    }
}

/*
Every region of the file that folds.

The walk uses the flat node lists, because a region is one node and needs no
parent. Every region runs from the line of its first token to the line of its
last one. A region that starts and ends on one line does not reach the output:
a fold there hides nothing, and the editor still draws an arrow for it.

An `if` gives one region per branch, not one region for the statement. A
reader collapses the branch that does not apply and keeps the one that does.
*/
pub fn folding_ranges(src: &str) -> Vec<FoldRange> {
    let Ok(lexed) = crate::syntax::lexer::lex(src) else {
        return Vec::new();
    };

    let Ok(chunk) = crate::syntax::parser::parse(src, &lexed.toks) else {
        return Vec::new();
    };

    let s = Src::new(src, &lexed.toks);
    let (exprs, stmts, _) = flatten(&chunk);
    let mut out = Vec::new();

    for stmt in stmts {
        match stmt {
            Stmt::Do(n) => region(&mut out, &s, n.span),
            Stmt::While(n) => region(&mut out, &s, n.span),
            Stmt::Repeat(n) => region(&mut out, &s, n.span),
            Stmt::NumericFor(n) => region(&mut out, &s, n.span),
            Stmt::GenericFor(n) => region(&mut out, &s, n.span),
            Stmt::Function(n) => region(&mut out, &s, n.span),
            Stmt::LocalFunction(n) => region(&mut out, &s, n.span),

            Stmt::If(n) => {
                for (cond, block) in &n.branches {
                    // The branch keyword sits one token before its condition.
                    let head = cond.span().start.saturating_sub(1);
                    region(&mut out, &s, closed(head, block.span.end, &s));
                }

                if let Some(block) = &n.else_block {
                    let head = block.span.start.saturating_sub(1);
                    region(&mut out, &s, closed(head, block.span.end, &s));
                }
            }

            /*
            A class is not in the flat expression list, so its methods fold
            here. The statements inside a method body are in the list, because
            the flat walk enters the body block.
            */
            Stmt::Class(n) => {
                region(&mut out, &s, n.span);

                for member in &n.members {
                    if let ClassMember::Method(f) = member {
                        region(&mut out, &s, f.span);
                    }
                }
            }

            _ => {}
        }
    }

    for expr in exprs {
        match expr {
            Expr::Function { span, .. } | Expr::Table { span, .. } => region(&mut out, &s, *span),
            _ => {}
        }
    }

    comment_regions(&s, &lexed.comments, &mut out);

    out.sort_by_key(|r| (r.start, r.end));
    out.dedup();

    out
}

/// The span from a header token to the token that closes the region.
fn closed(head: u32, closer: u32, s: &Src) -> TokSpan {
    let end = (closer + 1).min(s.toks.len() as u32);

    TokSpan {
        start: head,
        end: end.max(head),
    }
}

/// Adds a code region, unless it lives on one line.
fn region(out: &mut Vec<FoldRange>, s: &Src, span: TokSpan) {
    let (a, b) = s.bytes(span);

    if b <= a {
        return;
    }

    // `b` is the byte after the region, so the last byte of it names the line.
    let (start, end) = (s.line(a), s.line(b - 1));

    if end > start {
        out.push(FoldRange {
            start,
            end,
            kind: None,
        });
    }
}

/*
The comment folds: a run of line comments, and a long comment over lines.

A run needs two lines to fold. One comment line is already as short as it
gets. A comment that follows code on its line is not part of a run either.
Such a comment explains that line, and a fold that swallows the code with it
hides what the reader wants to see.
*/
fn comment_regions(s: &Src, comments: &[(u32, u32)], out: &mut Vec<FoldRange>) {
    let mut run: Option<(u32, u32)> = None;

    for &(start, end) in comments {
        if is_long(s.src, start) {
            let (first, last) = (s.line(start), s.line(end.saturating_sub(1)));

            if last > first {
                out.push(FoldRange {
                    start: first,
                    end: last,
                    kind: Some("comment"),
                });
            }

            flush(run.take(), out);
            continue;
        }

        let line = s.line(start);

        if !starts_line(s.src, start) {
            flush(run.take(), out);
            continue;
        }

        run = match run {
            Some((first, last)) if line == last + 1 => Some((first, line)),

            other => {
                flush(other, out);
                Some((line, line))
            }
        };
    }

    flush(run, out);
}

fn flush(run: Option<(u32, u32)>, out: &mut Vec<FoldRange>) {
    if let Some((first, last)) = run
        && last > first
    {
        out.push(FoldRange {
            start: first,
            end: last,
            kind: Some("comment"),
        });
    }
}

/// True when only whitespace comes before this byte on its line.
fn starts_line(src: &str, byte: u32) -> bool {
    let at = byte as usize;
    let head = src[..at].rfind('\n').map_or(0, |i| i + 1);

    src[head..at].trim().is_empty()
}

/// True for the `--[[ ]]` form, which folds on its own.
fn is_long(src: &str, start: u32) -> bool {
    let rest = &src.as_bytes()[start as usize + 2..];
    let mut i = 0;

    if rest.first() != Some(&b'[') {
        return false;
    }

    i += 1;

    while rest.get(i) == Some(&b'=') {
        i += 1;
    }

    rest.get(i) == Some(&b'[')
}

/*
The chain of ranges around one byte, from the tightest one outward.

The editor grows a selection by one step per keypress, so each range must
hold the range before it. The tree gives that for free: a token sits in an
expression, the expression sits in a statement, and the statement sits in a
block. The list is sorted by width and then filtered by containment, which
drops the two candidates that a caret between two tokens produces.

The result is innermost first. The caller builds the parent chain that the
protocol asks for.
*/
pub fn selection_ranges(src: &str, byte: u32) -> Vec<(u32, u32)> {
    let Ok(lexed) = crate::syntax::lexer::lex(src) else {
        return Vec::new();
    };

    let Ok(chunk) = crate::syntax::parser::parse(src, &lexed.toks) else {
        return Vec::new();
    };

    let s = Src::new(src, &lexed.toks);
    let byte = byte.min(src.len() as u32);
    let (exprs, stmts, blocks) = flatten(&chunk);
    let mut found: Vec<(u32, u32)> = Vec::new();

    // The token under the caret is the name step, which the tree has no node for.
    for tok in &lexed.toks {
        if tok.start <= byte && byte <= tok.end {
            found.push((tok.start, tok.end));
        }
    }

    for e in exprs {
        found.push(s.bytes(e.span()));
    }

    for stmt in stmts {
        found.push(s.bytes(stmt.span()));
    }

    for block in blocks {
        found.push(s.bytes(block.span));
    }

    // The whole file is the last step out, and a chunk block stops at its last token.
    found.push((0, src.len() as u32));

    found.retain(|&(a, b)| a <= byte && byte <= b);
    found.sort_by_key(|&(a, b)| (b - a, std::cmp::Reverse(a)));

    let mut out: Vec<(u32, u32)> = Vec::new();

    for r in found {
        match out.last() {
            None => out.push(r),

            Some(&last) => {
                if r != last && r.0 <= last.0 && r.1 >= last.1 {
                    out.push(r);
                }
            }
        }
    }

    out
}

/*
The outline of the file as a tree.

A helper that lives inside a function belongs under that function, not beside
it. So the walk descends and returns children, and the caller of the request
gets one root per top level declaration.

A block that declares nothing of its own, for example the body of an `if`,
passes its symbols up to the parent. The alternative is a node named `if` in
the outline, which no reader navigates to.
*/
pub fn symbols(src: &str) -> Vec<Symbol> {
    let Ok(lexed) = crate::syntax::lexer::lex(src) else {
        return Vec::new();
    };

    let Ok(chunk) = crate::syntax::parser::parse(src, &lexed.toks) else {
        return Vec::new();
    };

    let s = Src::new(src, &lexed.toks);

    block_symbols(&s, &chunk.block)
}

fn block_symbols(s: &Src, block: &Block) -> Vec<Symbol> {
    let mut out = Vec::new();

    for stmt in &block.stmts {
        stmt_symbols(s, stmt, &mut out);
    }

    out
}

fn stmt_symbols(s: &Src, stmt: &Stmt, out: &mut Vec<Symbol>) {
    match stmt {
        Stmt::Function(n) => out.push(function_symbol(s, n, None)),

        Stmt::LocalFunction(n) => out.push(Symbol {
            name: s.text(n.name).to_string(),
            kind: FUNCTION,
            range: s.bytes(n.span),
            selection: s.bytes(n.name),
            children: block_symbols(s, &n.body.block),
        }),

        Stmt::Local(n) => local_symbols(s, n, out),

        Stmt::TypeAlias(n) => out.push(alias_symbol(s, n)),

        Stmt::Class(n) => out.push(Symbol {
            name: s.text(n.name).to_string(),
            kind: CLASS,
            range: s.bytes(n.span),
            selection: s.bytes(n.name),
            children: n.members.iter().map(|m| member_symbol(s, m)).collect(),
        }),

        /*
        `M.f = function() end` declares a function under another name. The
        outline reports it, because the reader who searches for `M.f` finds
        nothing otherwise.
        */
        Stmt::Assign(n) => {
            for (target, value) in n.targets.iter().zip(&n.values) {
                if let Expr::Function { body, span, .. } = value {
                    out.push(Symbol {
                        name: s.text(target.span()).to_string(),
                        kind: FUNCTION,
                        range: (s.bytes(target.span()).0, s.bytes(*span).1),
                        selection: s.bytes(target.span()),
                        children: block_symbols(s, &body.block),
                    });
                }
            }
        }

        Stmt::Do(n) => out.extend(block_symbols(s, &n.block)),
        Stmt::While(n) => out.extend(block_symbols(s, &n.block)),
        Stmt::Repeat(n) => out.extend(block_symbols(s, &n.block)),
        Stmt::NumericFor(n) => out.extend(block_symbols(s, &n.block)),
        Stmt::GenericFor(n) => out.extend(block_symbols(s, &n.block)),

        Stmt::If(n) => {
            for (_, block) in &n.branches {
                out.extend(block_symbols(s, block));
            }

            if let Some(block) = &n.else_block {
                out.extend(block_symbols(s, block));
            }
        }

        _ => {}
    }
}

/*
The symbols of one `local` or `const` statement.

Each name gets its own entry, and each entry keeps the whole statement as its
range. A binding whose value is a function reports as a function: the reader
cares that `local f = function() end` is callable, not how it was written.
*/
fn local_symbols(s: &Src, n: &Local, out: &mut Vec<Symbol>) {
    let plain = if n.is_const { CONSTANT } else { VARIABLE };

    for (i, binding) in n.names.iter().enumerate() {
        let (kind, children) = match n.values.get(i) {
            Some(Expr::Function { body, .. }) => (FUNCTION, block_symbols(s, &body.block)),

            Some(Expr::Table { fields, .. }) => (plain, table_symbols(s, fields)),

            _ => (plain, Vec::new()),
        };

        out.push(Symbol {
            name: s.text(binding.name).to_string(),
            kind,
            range: s.bytes(n.span),
            selection: s.bytes(binding.name),
            children,
        });
    }
}

/*
The functions that a table literal holds, as children of the table.

Only the function fields reach the outline. A table of data has one entry per
row, and a list of those rows is the file again, not a summary of it.
*/
fn table_symbols(s: &Src, fields: &[TableField]) -> Vec<Symbol> {
    let mut out = Vec::new();

    for field in fields {
        if let TableField::Named { name, value } = field
            && let Expr::Function { body, span, .. } = value
        {
            out.push(Symbol {
                name: s.text(*name).to_string(),
                kind: FUNCTION,
                range: (s.bytes(*name).0, s.bytes(*span).1),
                selection: s.bytes(*name),
                children: block_symbols(s, &body.block),
            });
        }
    }

    out
}

fn alias_symbol(s: &Src, n: &TypeAlias) -> Symbol {
    Symbol {
        name: s.text(n.name).to_string(),
        kind: INTERFACE,
        range: s.bytes(n.span),
        selection: s.bytes(n.name),
        children: Vec::new(),
    }
}

fn member_symbol(s: &Src, member: &ClassMember) -> Symbol {
    match member {
        ClassMember::Field { name, span, .. } => Symbol {
            name: s.text(*name).to_string(),
            kind: FIELD,
            range: s.bytes(*span),
            selection: s.bytes(*name),
            children: Vec::new(),
        },

        ClassMember::Method(f) => function_symbol(s, f, Some(METHOD)),
    }
}

/*
One `function a.b:c()` declaration.

The name keeps the separators the author wrote, so the outline shows `t:m`
and not `t.m`. The two forms differ in the call they accept, and a reader who
searches the outline for `t:m` searches for the text of the declaration.
*/
fn function_symbol(s: &Src, n: &Function, force: Option<u8>) -> Symbol {
    let mut name = String::new();
    let last = n.path.len().saturating_sub(1);

    for (i, part) in n.path.iter().enumerate() {
        if i > 0 {
            name.push(if n.is_method && i == last { ':' } else { '.' });
        }

        name.push_str(s.text(*part));
    }

    let selection = match (n.path.first(), n.path.last()) {
        (Some(first), Some(last)) => (s.bytes(*first).0, s.bytes(*last).1),

        _ => s.bytes(n.span),
    };

    let kind = force.unwrap_or(if n.is_method { METHOD } else { FUNCTION });

    Symbol {
        name,
        kind,
        range: s.bytes(n.span),
        selection,
        children: block_symbols(s, &n.body.block),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folds(src: &str) -> Vec<(u32, u32, Option<&'static str>)> {
        folding_ranges(src)
            .into_iter()
            .map(|r| (r.start, r.end, r.kind))
            .collect()
    }

    fn names(list: &[Symbol]) -> Vec<&str> {
        list.iter().map(|s| s.name.as_str()).collect()
    }

    #[test]
    fn a_broken_file_gives_nothing() {
        let src = "local = = function ??? end\n";

        assert!(folding_ranges(src).is_empty());
        assert!(symbols(src).is_empty());
        assert!(selection_ranges(src, 3).is_empty());
    }

    #[test]
    fn an_unterminated_string_gives_nothing() {
        // The lexer fails before the parser here, so both guards matter.
        let src = "local s = \"open\n";

        assert!(folding_ranges(src).is_empty());
        assert!(symbols(src).is_empty());
        assert!(selection_ranges(src, 3).is_empty());
    }

    #[test]
    fn an_empty_file_gives_nothing() {
        assert!(folding_ranges("").is_empty());
        assert!(symbols("").is_empty());
    }

    #[test]
    fn a_function_body_folds() {
        let src = "local function f(a)\n\treturn a\nend\n";

        assert_eq!(folds(src), vec![(0, 2, None)]);
    }

    #[test]
    fn a_single_line_function_does_not_fold() {
        assert!(folding_ranges("local function f() return 1 end\n").is_empty());
        assert!(folding_ranges("local t = { a = 1, b = 2 }\n").is_empty());
    }

    #[test]
    fn a_table_over_lines_folds() {
        let src = "local t = {\n\ta = 1,\n\tb = 2,\n}\n";

        assert_eq!(folds(src), vec![(0, 3, None)]);
    }

    #[test]
    fn every_branch_of_an_if_folds() {
        let src = "if a then\n\tf()\nelseif b then\n\tg()\nelse\n\th()\nend\n";

        assert_eq!(folds(src), vec![(0, 2, None), (2, 4, None), (4, 6, None)]);
    }

    #[test]
    fn loops_and_do_blocks_fold() {
        let src = "\
do
\tlocal a = 1
end
while a do
\ta = false
end
for i = 1, 10 do
\tf(i)
end
for _, v in pairs(t) do
\tf(v)
end
repeat
\tf()
until a
";

        assert_eq!(
            folds(src),
            vec![
                (0, 2, None),
                (3, 5, None),
                (6, 8, None),
                (9, 11, None),
                (12, 14, None),
            ]
        );
    }

    #[test]
    fn a_nested_body_folds_too() {
        let src = "local function f()\n\tif a then\n\t\tf()\n\tend\nend\n";

        assert_eq!(folds(src), vec![(0, 4, None), (1, 3, None)]);
    }

    #[test]
    fn a_run_of_comments_folds_once() {
        let src = "-- one\n-- two\n-- three\nlocal a = 1\n";

        assert_eq!(folds(src), vec![(0, 2, Some("comment"))]);
    }

    #[test]
    fn one_comment_line_does_not_fold() {
        assert!(folding_ranges("-- alone\nlocal a = 1\n").is_empty());
    }

    #[test]
    fn a_gap_ends_a_run_of_comments() {
        let src = "-- one\n-- two\n\n-- three\nlocal a = 1\n";

        assert_eq!(folds(src), vec![(0, 1, Some("comment"))]);
    }

    #[test]
    fn a_comment_after_code_does_not_fold() {
        let src = "local a = 1 -- one\nlocal b = 2 -- two\n";

        assert!(folding_ranges(src).is_empty());
    }

    #[test]
    fn a_long_comment_over_lines_folds() {
        let src = "--[[\nnote\n]]\nlocal a = 1\n";

        assert_eq!(folds(src), vec![(0, 2, Some("comment"))]);
    }

    #[test]
    fn nested_functions_nest() {
        let src = "\
local function outer()
\tlocal function inner()
\t\tlocal function deep() end
\tend

\tlocal x = 1
end
";

        let out = symbols(src);

        assert_eq!(names(&out), vec!["outer"]);
        assert_eq!(names(&out[0].children), vec!["inner", "x"]);
        assert_eq!(names(&out[0].children[0].children), vec!["deep"]);
        assert!(out[0].children[0].children[0].children.is_empty());
    }

    #[test]
    fn a_function_in_a_branch_belongs_to_its_function() {
        let src = "\
local function outer()
\tif a then
\t\tlocal function inner() end
\tend
end
";

        let out = symbols(src);

        assert_eq!(names(&out), vec!["outer"]);
        assert_eq!(names(&out[0].children), vec!["inner"]);
    }

    #[test]
    fn a_method_reports_as_a_method() {
        let src = "local t = {}\nfunction t:m()\nend\nfunction t.f()\nend\n";

        let out = symbols(src);

        assert_eq!(names(&out), vec!["t", "t:m", "t.f"]);
        assert_eq!(out[1].kind, METHOD);
        assert_eq!(out[2].kind, FUNCTION);
    }

    #[test]
    fn a_local_and_a_const_differ() {
        let src = "local a = 1\nconst b = 2\n";

        let out = symbols(src);

        assert_eq!(out[0].kind, VARIABLE);
        assert_eq!(out[1].kind, CONSTANT);
    }

    #[test]
    fn every_name_of_a_local_reports() {
        let out = symbols("local a, b = 1, 2\n");

        assert_eq!(names(&out), vec!["a", "b"]);
    }

    #[test]
    fn a_type_alias_reports_as_an_interface() {
        let out = symbols("type Point = { x: number }\n");

        assert_eq!(names(&out), vec!["Point"]);
        assert_eq!(out[0].kind, INTERFACE);
    }

    #[test]
    fn a_function_value_reports_as_a_function() {
        let src = "local f = function()\n\tlocal g = 1\nend\n";

        let out = symbols(src);

        assert_eq!(out[0].kind, FUNCTION);
        assert_eq!(names(&out[0].children), vec!["g"]);
    }

    #[test]
    fn a_table_of_functions_nests() {
        let src = "\
local M = {
\tf = function()
\t\tlocal a = 1
\tend,

\tn = 2,
}
";

        let out = symbols(src);

        assert_eq!(names(&out), vec!["M"]);
        assert_eq!(names(&out[0].children), vec!["f"]);
        assert_eq!(names(&out[0].children[0].children), vec!["a"]);
    }

    #[test]
    fn an_assigned_function_reports() {
        let src = "local M = {}\nM.f = function() end\n";

        let out = symbols(src);

        assert_eq!(names(&out), vec!["M", "M.f"]);
        assert_eq!(out[1].kind, FUNCTION);
    }

    #[test]
    fn the_selection_range_holds_the_name() {
        let src = "local function hello()\nend\n";

        let out = symbols(src);
        let (a, b) = out[0].selection;

        assert_eq!(&src[a as usize..b as usize], "hello");
        assert_eq!(out[0].range, (0, 26));
    }

    #[test]
    fn a_selection_starts_at_the_name_under_the_caret() {
        let src = "local function f()\n\treturn value + 1\nend\n";
        let at = src.find("value").unwrap() as u32;

        let out = selection_ranges(src, at + 1);

        assert_eq!(&src[out[0].0 as usize..out[0].1 as usize], "value");
        assert_eq!(&src[out[1].0 as usize..out[1].1 as usize], "value + 1");
        assert_eq!(*out.last().unwrap(), (0, src.len() as u32));
    }

    /*
    The property that the editor depends on: one keypress grows the selection
    and never moves it. So each range must hold the range before it, and no
    two ranges in the chain may be equal.
    */
    #[test]
    fn a_selection_chain_only_grows() {
        let sources = [
            "local function f(a)\n\treturn a.b.c(1, 2)\nend\n",
            "local t = {\n\tk = { 1, 2, 3 },\n}\n",
            "if a then\n\twhile b do\n\t\tf(`x{y}z`)\n\tend\nend\n",
            "type T = { x: number }\nlocal v: T = { x = 1 }\nreturn v\n",
            "for i = 1, 10 do\n\trepeat\n\t\tf(i)\n\tuntil i > 2\nend\n",
        ];

        for src in sources {
            for byte in 0..=src.len() as u32 {
                let chain = selection_ranges(src, byte);

                assert!(!chain.is_empty(), "no chain at {byte} of {src:?}");

                for pair in chain.windows(2) {
                    let (inner, outer) = (pair[0], pair[1]);

                    assert!(
                        outer.0 <= inner.0 && inner.1 <= outer.1 && outer != inner,
                        "{outer:?} does not hold {inner:?} in {src:?}"
                    );
                }

                for &(a, b) in &chain {
                    assert!(a <= byte && byte <= b, "{a}..{b} misses {byte}");
                    assert!(src.is_char_boundary(a as usize));
                    assert!(src.is_char_boundary(b as usize));
                }
            }
        }
    }

    #[test]
    fn a_caret_past_the_end_still_answers() {
        let src = "local a = 1\n";

        let out = selection_ranges(src, 9_999);

        assert_eq!(out, vec![(0, src.len() as u32)]);
    }
}
