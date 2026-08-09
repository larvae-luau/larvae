/*!
What every lint gets to look at, and the suppression comments that overrule it.

The shared analysis lives here rather than in each lint because several of them
want the same thing, which name refers to what, and recomputing it per lint
would multiply the cost of a pass that has to finish between keystrokes.
*/

use std::collections::HashMap;

use crate::syntax::ast::*;
use crate::syntax::lexer::Tok;

use super::config::{Level, LintConfig};
use super::scope::Names;

/// One thing a lint found
#[derive(Debug, Clone)]
pub struct Finding {
    /// Which lint said so
    pub lint: &'static str,
    /// Filled in by the runner from the config, a lint leaves it at its default
    pub level: Level,
    /// Byte range in the source
    pub span: (u32, u32),
    pub message: String,
    pub help: Option<String>,
}

impl Finding {
    pub fn new(lint: &'static str, span: (u32, u32), message: impl Into<String>) -> Self {
        Self {
            lint,
            level: Level::Warn,
            span,
            message: message.into(),
            help: None,
        }
    }

    pub fn with_help(mut self, help: impl Into<String>) -> Self {
        self.help = Some(help.into());

        self
    }
}

pub struct LintCtx<'a> {
    pub src: &'a str,
    pub toks: &'a [Tok],
    pub comments: &'a [(u32, u32)],
    pub chunk: &'a Chunk,
    pub cfg: &'a LintConfig,
    /// What every name in the file refers to, resolved once
    pub names: Names<'a>,
    /*
    Every node in the file, flattened once.

    Each lint used to walk the tree itself, so a file was walked once per
    lint and thirty of them meant thirty walks. Collecting into three lists
    up front turns that into one walk and thirty passes over a vector of
    references, which is the same work stated once instead of thirty times.
    Measured on a 367 file corpus: 74ms to 34ms.
    */
    pub exprs: Vec<&'a Expr>,
    pub stmts: Vec<&'a Stmt>,
    pub blocks: Vec<&'a Block>,
    /// Which lints are suppressed on which line
    allowed: HashMap<u32, Vec<String>>,
    /// Byte offset of the start of each line, for the line lookup
    line_starts: Vec<u32>,
}

impl<'a> LintCtx<'a> {
    pub fn new(
        src: &'a str,
        toks: &'a [Tok],
        comments: &'a [(u32, u32)],
        chunk: &'a Chunk,
        cfg: &'a LintConfig,
    ) -> Self {
        let mut line_starts = vec![0u32];

        for (i, b) in src.bytes().enumerate() {
            if b == b'\n' {
                line_starts.push(i as u32 + 1);
            }
        }

        let names = super::scope::resolve(src, toks, chunk);
        let allowed = collect_suppressions(src, comments, &line_starts);
        let (exprs, stmts, blocks) = flatten(chunk);

        Self {
            src,
            toks,
            comments,
            chunk,
            cfg,
            names,
            exprs,
            stmts,
            blocks,
            allowed,
            line_starts,
        }
    }

    /// Byte range covered by a token span, half open
    pub fn bytes(&self, span: TokSpan) -> (u32, u32) {
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

    /// Source text covered by a token span
    pub fn text(&self, span: TokSpan) -> &'a str {
        let (a, b) = self.bytes(span);

        &self.src[a as usize..b as usize]
    }

    /// Text of one token
    pub fn tok(&self, index: u32) -> &'a str {
        self.toks[index as usize].text(self.src)
    }

    /// Zero based line holding this byte
    pub fn line(&self, byte: u32) -> u32 {
        (self.line_starts.partition_point(|&s| s <= byte) - 1) as u32
    }

    /*
    Whether an author already said this one is wrong here.

    A suppression on the line above covers the line below, which is the form
    people write, and one on the same line covers itself, which is the form
    people write when the statement is short. Both are checked because guessing
    which an author meant is worse than accepting either.
    */
    pub fn suppressed(&self, finding: &Finding) -> bool {
        let line = self.line(finding.span.0);

        [line, line.wrapping_sub(1)].iter().any(|l| {
            self.allowed
                .get(l)
                .is_some_and(|names| names.iter().any(|n| n == "*" || n == finding.lint))
        })
    }

    /*
    Whether two spans are the same code, wherever they sit.

    Compared token by token rather than as text, because the text carries the
    author's spacing and `a+b` and `a + b` are the same expression. Splitting
    the text on whitespace does not fix that either, since it still reads `+y`
    as one piece and `+ y` as two.
    */
    pub fn same_text(&self, a: TokSpan, b: TokSpan) -> bool {
        if a.end - a.start != b.end - b.start {
            return false;
        }

        (0..a.end - a.start).all(|i| self.tok(a.start + i) == self.tok(b.start + i))
    }
}

/*
One walk, collecting every node so no lint has to walk again.

Written out rather than driven through the shared `Visit` trait, because that
trait hands a callback a reference whose lifetime is the visit call rather than
the tree. Storing those would need the lifetime laundered, and a walk written
honestly is cheaper than an `unsafe` that has to be argued about every time
somebody reads it.
*/
fn flatten(chunk: &Chunk) -> (Vec<&Expr>, Vec<&Stmt>, Vec<&Block>) {
    let mut out = Collected::default();
    out.block(&chunk.block);

    (out.exprs, out.stmts, out.blocks)
}

#[derive(Default)]
struct Collected<'a> {
    exprs: Vec<&'a Expr>,
    stmts: Vec<&'a Stmt>,
    blocks: Vec<&'a Block>,
}

impl<'a> Collected<'a> {
    fn block(&mut self, block: &'a Block) {
        self.blocks.push(block);

        for stmt in &block.stmts {
            self.stmt(stmt);
        }
    }

    fn stmt(&mut self, stmt: &'a Stmt) {
        self.stmts.push(stmt);

        match stmt {
            Stmt::Empty(_) | Stmt::Break(_) | Stmt::Continue(_) | Stmt::TypeAlias(_) => {}

            Stmt::Local(n) => self.exprs_of(&n.values),

            Stmt::Assign(n) => {
                self.exprs_of(&n.targets);
                self.exprs_of(&n.values);
            }

            Stmt::Call(e, _) => self.expr(e),

            Stmt::Do(n) => self.block(&n.block),

            Stmt::While(n) => {
                self.expr(&n.cond);
                self.block(&n.block);
            }

            Stmt::Repeat(n) => {
                self.block(&n.block);
                self.expr(&n.cond);
            }

            Stmt::If(n) => {
                for (cond, block) in &n.branches {
                    self.expr(cond);
                    self.block(block);
                }

                if let Some(block) = &n.else_block {
                    self.block(block);
                }
            }

            Stmt::NumericFor(n) => {
                self.expr(&n.start);
                self.expr(&n.limit);

                if let Some(step) = &n.step {
                    self.expr(step);
                }

                self.block(&n.block);
            }

            Stmt::GenericFor(n) => {
                self.exprs_of(&n.exprs);
                self.block(&n.block);
            }

            Stmt::Function(n) => self.block(&n.body.block),

            Stmt::LocalFunction(n) => self.block(&n.body.block),

            Stmt::Return(n) => self.exprs_of(&n.values),
        }
    }

    fn exprs_of(&mut self, list: &'a [Expr]) {
        for e in list {
            self.expr(e);
        }
    }

    fn expr(&mut self, e: &'a Expr) {
        self.exprs.push(e);

        match e {
            Expr::Nil(_)
            | Expr::True(_)
            | Expr::False(_)
            | Expr::Vararg(_)
            | Expr::Number(_)
            | Expr::String(_)
            | Expr::InterpString(_)
            | Expr::Name(_) => {}

            Expr::Function { body, .. } => self.block(&body.block),

            Expr::Table { fields, .. } => {
                for field in fields {
                    match field {
                        TableField::Positional(v) => self.expr(v),

                        TableField::Named { value, .. } => self.expr(value),

                        TableField::Computed { key, value } => {
                            self.expr(key);
                            self.expr(value);
                        }
                    }
                }
            }

            Expr::Binary { lhs, rhs, .. } => {
                self.expr(lhs);
                self.expr(rhs);
            }

            Expr::Unary { operand, .. } => self.expr(operand),

            Expr::Paren { inner, .. } => self.expr(inner),

            Expr::TypeAssert { expr, .. } => self.expr(expr),

            Expr::Index { object, key, .. } => {
                self.expr(object);

                if let IndexKey::Computed(k) = key {
                    self.expr(k);
                }
            }

            Expr::Call { func, args, .. } => {
                self.expr(func);

                match args {
                    CallArgs::Paren(list) => self.exprs_of(list),

                    CallArgs::Table(t) => self.expr(t),

                    CallArgs::Str(_) => {}
                }
            }

            Expr::IfElse {
                branches,
                else_value,
                ..
            } => {
                for (cond, value) in branches {
                    self.expr(cond);
                    self.expr(value);
                }

                self.expr(else_value);
            }
        }
    }
}

/*
Find every `-- larvae: allow(name, other)` in the file.

What counts as one lives in [`crate::flags`], because `larvae process` reads
the same comments to know which ones it can leave out of the output.
*/
fn collect_suppressions(
    src: &str,
    comments: &[(u32, u32)],
    line_starts: &[u32],
) -> HashMap<u32, Vec<String>> {
    let mut out: HashMap<u32, Vec<String>> = HashMap::new();

    for &(start, end) in comments {
        let Some(names) = crate::flags::allows(&src[start as usize..end as usize]) else {
            continue;
        };

        let line = (line_starts.partition_point(|&s| s <= start) - 1) as u32;

        out.entry(line)
            .or_default()
            .extend(names.map(str::to_string));
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax::{lexer, parser};

    fn ctx_of(src: &str) -> (lexer::Lexed, Chunk) {
        let lexed = lexer::lex(src).expect("lexes");
        let chunk = parser::parse(src, &lexed.toks).expect("parses");

        (lexed, chunk)
    }

    fn suppressions(src: &str) -> HashMap<u32, Vec<String>> {
        let (lexed, _) = ctx_of(src);
        let mut line_starts = vec![0u32];

        for (i, b) in src.bytes().enumerate() {
            if b == b'\n' {
                line_starts.push(i as u32 + 1);
            }
        }

        collect_suppressions(src, &lexed.comments, &line_starts)
    }

    #[test]
    fn a_suppression_comment_is_found() {
        let found = suppressions("-- larvae: allow(unused_variable)\nlocal x = 1\n");

        assert_eq!(found[&0], ["unused_variable"]);
    }

    #[test]
    fn several_lints_can_be_named_at_once() {
        let found = suppressions("-- larvae: allow(unused_variable, shadowing)\nlocal x = 1\n");

        assert_eq!(found[&0], ["unused_variable", "shadowing"]);
    }

    /// A project switching over should not have to rewrite its comments
    #[test]
    fn selenes_spelling_works_too() {
        let found = suppressions("-- selene: allow(unused_variable)\nlocal x = 1\n");

        assert_eq!(found[&0], ["unused_variable"]);
    }

    #[test]
    fn a_comment_that_is_not_a_suppression_is_ignored() {
        assert!(suppressions("-- just a note\nlocal x = 1\n").is_empty());
        assert!(suppressions("-- larvae: something else\n").is_empty());
        assert!(suppressions("-- larvae: allow(unclosed\n").is_empty());
    }

    #[test]
    fn a_suppression_covers_its_own_line_and_the_one_below() {
        let src = "local a = 1 -- larvae: allow(unused_variable)\nlocal b = 2\nlocal c = 3\n";
        let (lexed, chunk) = ctx_of(src);
        let cfg = LintConfig::default();
        let ctx = LintCtx::new(src, &lexed.toks, &lexed.comments, &chunk, &cfg);

        let at = |line: u32| {
            let offset = ctx.line_starts[line as usize];

            ctx.suppressed(&Finding::new("unused_variable", (offset, offset), "x"))
        };

        assert!(at(0), "its own line");
        assert!(at(1), "the line below");
        assert!(!at(2), "and no further");
    }

    #[test]
    fn a_suppression_only_covers_the_lint_it_names() {
        let src = "-- larvae: allow(shadowing)\nlocal x = 1\n";
        let (lexed, chunk) = ctx_of(src);
        let cfg = LintConfig::default();
        let ctx = LintCtx::new(src, &lexed.toks, &lexed.comments, &chunk, &cfg);

        assert!(ctx.suppressed(&Finding::new("shadowing", (28, 28), "x")));
        assert!(!ctx.suppressed(&Finding::new("unused_variable", (28, 28), "x")));
    }

    #[test]
    fn a_star_suppresses_everything_on_the_line() {
        let src = "-- larvae: allow(*)\nlocal x = 1\n";
        let (lexed, chunk) = ctx_of(src);
        let cfg = LintConfig::default();
        let ctx = LintCtx::new(src, &lexed.toks, &lexed.comments, &chunk, &cfg);

        assert!(ctx.suppressed(&Finding::new("anything_at_all", (20, 20), "x")));
    }

    #[test]
    fn whitespace_does_not_make_two_expressions_different() {
        let src = "local a = x  +y\nlocal b = x + y\n";
        let (lexed, chunk) = ctx_of(src);
        let cfg = LintConfig::default();
        let ctx = LintCtx::new(src, &lexed.toks, &lexed.comments, &chunk, &cfg);

        let Stmt::Local(a) = &chunk.block.stmts[0] else {
            panic!("expected a local")
        };

        let Stmt::Local(b) = &chunk.block.stmts[1] else {
            panic!("expected a local")
        };

        assert!(ctx.same_text(a.values[0].span(), b.values[0].span()));
    }
}
