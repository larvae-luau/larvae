/*!
Rules that need the use of each local

Both rules use the scope walk and not the tree alone. For this reason,
they waited on the scope walk. The rules are conservative where it
matters. A local whose value can have a side effect stays in place.
*/

use std::collections::HashMap;

use crate::rules::engine::{Edit, RuleCtx};
use crate::rules::scope::{self, Origin};
use crate::syntax::ast::*;

/*
remove_unused_variable: remove locals that no code reads.

The rule removes only whole statements. A partly removed `local a, b`
would change the number of values that the right hand side supplies. The
declaration must also be inert. `local x = f()` must still call f, even
when no code reads the result.

A type counts as a reader. `local T = {}` under `type Alias = typeof(T)`
stays, because the removal would leave the alias with nothing to read.
*/
pub fn remove_unused_variable(ctx: &RuleCtx, edits: &mut Vec<Edit>) {
    let names = scope::resolve(ctx);
    let dead: Vec<u32> = names
        .bindings
        .iter()
        .filter(|b| b.uses.is_empty() && b.type_uses.is_empty())
        .map(|b| b.declared_at)
        .collect();

    if dead.is_empty() {
        return;
    }

    let mut v = Sweeper {
        ctx,
        dead,
        edits,
        origins: names
            .bindings
            .iter()
            .map(|b| (b.declared_at, b.origin))
            .collect(),
    };

    walk(ctx.chunk, &mut v);
}

struct Sweeper<'a, 'b> {
    ctx: &'a RuleCtx<'b>,
    dead: Vec<u32>,
    origins: HashMap<u32, Origin>,
    edits: &'a mut Vec<Edit>,
}

impl Sweeper<'_, '_> {
    fn unused(&self, span: TokSpan) -> bool {
        self.dead.contains(&span.start)
    }

    fn stmt(&mut self, s: &Stmt) {
        match s {
            Stmt::Local(n) => {
                let all_dead = n.names.iter().all(|b| self.unused(b.name))
                    && n.names
                        .iter()
                        .all(|b| self.origins.get(&b.name.start) == Some(&Origin::Local));

                if all_dead && n.values.iter().all(inert) {
                    self.ctx.delete_keep_lines(n.span, self.edits);
                }
            }

            // A function definition alone has no effect, so removal is always safe.
            Stmt::LocalFunction(n) if self.unused(n.name) => {
                self.ctx.delete_keep_lines(n.span, self.edits);
            }

            _ => {}
        }
    }
}

/// Where a rename takes its new names from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NameStyle {
    /// `a`, `b`, ... the shortest names that lex. This is the smallest output.
    #[default]
    Short,
    /// `_0x0`, `_0x1`, ... the shape a reader expects from an obfuscator.
    Hex,
}

/*
rename_variables: give each local a short name.

Each binding gets its own name. The rule does not reuse a name across
scopes that do not overlap. The output is a little longer than darklua's
output, but the rule needs no shadowing analysis to be correct. The
difference only matters when the dense generator exists to make short
names useful.

A type reads names too, and the parser keeps a type as a raw span. The
scope walk recovers those reads, so `x :: jecs.Component` takes the new
name of `jecs` with it. Where the walk cannot say what a name in a type
means, it blocks that name and the rule leaves the binding alone.
*/
pub fn rename_variables(ctx: &RuleCtx, edits: &mut Vec<Edit>) {
    rename_with(ctx, edits, NameStyle::Short);
}

/// The same rename with the names from another source, see [`NameStyle`].
pub fn rename_with(ctx: &RuleCtx, edits: &mut Vec<Edit>, style: NameStyle) {
    let names = scope::resolve(ctx);
    let mut supply = Supply::new(&names.taken, style);

    for binding in &names.bindings {
        let old = ctx.tok_text(binding.declared_at);

        // A vararg has no name to take, and Luau passes self implicitly.
        if old == "self" || names.pinned.contains(old) {
            continue;
        }

        let Some(new) = supply.next_name() else {
            return;
        };

        for at in std::iter::once(&binding.declared_at)
            .chain(binding.uses.iter())
            .chain(binding.type_uses.iter())
        {
            let tok = &ctx.toks[*at as usize];
            edits.push((tok.start, tok.end, new.clone()));
        }
    }
}

/// New names in order. The supply skips each name that the file already uses.
struct Supply<'a> {
    taken: &'a std::collections::HashSet<String>,
    style: NameStyle,
    counter: usize,
}

impl<'a> Supply<'a> {
    fn new(taken: &'a std::collections::HashSet<String>, style: NameStyle) -> Self {
        Self {
            taken,
            style,
            counter: 0,
        }
    }

    fn next_name(&mut self) -> Option<String> {
        // After a few million names, the state is wrong. Stop without a report.
        while self.counter < 5_000_000 {
            let n = self.counter;
            self.counter += 1;

            let name = match self.style {
                NameStyle::Short => short_name(n),
                NameStyle::Hex => format!("_0x{n:x}"),
            };

            // is_ident rejects the keywords, so the supply never emits `do` or `end`.
            if !self.taken.contains(&name) && crate::rules::native::is_ident(&name) {
                return Some(name);
            }
        }

        None
    }
}

/// The nth name in `a`, `b`, ... `z`, `A`, ... `aa`, ...
fn short_name(mut n: usize) -> String {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ";
    let mut name = String::new();

    loop {
        name.push(ALPHABET[n % ALPHABET.len()] as char);
        n /= ALPHABET.len();

        if n == 0 {
            return name;
        }

        n -= 1;
    }
}

/// True when evaluation of the expression cannot have an observable effect.
fn inert(e: &Expr) -> bool {
    match e {
        Expr::Nil(_)
        | Expr::True(_)
        | Expr::False(_)
        | Expr::Number(_)
        | Expr::String(_)
        | Expr::Vararg(_)
        | Expr::Name(_)
        | Expr::Function { .. } => true,

        Expr::Paren { inner, .. } => inert(inner),

        Expr::Unary { operand, .. } => inert(operand),

        Expr::Binary { lhs, rhs, .. } => inert(lhs) && inert(rhs),

        Expr::Table { fields, .. } => fields.iter().all(|f| match f {
            TableField::Positional(e) => inert(e),

            TableField::Named { value, .. } => inert(value),

            TableField::Computed { key, value } => inert(key) && inert(value),
        }),

        Expr::TypeAssert { expr, .. } => inert(expr),

        // An index runs __index. A call runs any code. Interpolation runs tostring.
        Expr::Index { .. } | Expr::Call { .. } | Expr::InterpString(_) | Expr::IfElse { .. } => {
            false
        }
    }
}

/// A statement walk that reaches each nested block.
fn walk(chunk: &Chunk, v: &mut Sweeper) {
    walk_block(&chunk.block, v);
}

fn walk_block(b: &Block, v: &mut Sweeper) {
    for s in &b.stmts {
        v.stmt(s);
        descend(s, v);
    }
}

fn descend(s: &Stmt, v: &mut Sweeper) {
    match s {
        Stmt::Do(n) => walk_block(&n.block, v),

        Stmt::While(n) => walk_block(&n.block, v),

        Stmt::Repeat(n) => walk_block(&n.block, v),

        Stmt::NumericFor(n) => walk_block(&n.block, v),

        Stmt::GenericFor(n) => walk_block(&n.block, v),

        Stmt::Function(n) => walk_block(&n.body.block, v),

        Stmt::LocalFunction(n) => walk_block(&n.body.block, v),

        Stmt::If(n) => {
            for (_, body) in &n.branches {
                walk_block(body, v);
            }

            if let Some(e) = &n.else_block {
                walk_block(e, v);
            }
        }

        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::darklua::testing::{assert_lines_kept, run};

    fn sweep(src: &str) -> String {
        let out = run(src, remove_unused_variable);
        assert_lines_kept(src, &out);

        out
    }

    #[test]
    fn drops_a_local_nothing_reads() {
        assert_eq!(sweep("local x = 1\nreturn 2\n"), "\nreturn 2\n");
    }

    #[test]
    fn keeps_one_that_is_read() {
        assert_eq!(sweep("local x = 1\nreturn x\n"), "local x = 1\nreturn x\n");
    }

    #[test]
    fn keeps_a_declaration_that_does_something() {
        // f must still run.
        assert_eq!(
            sweep("local x = f()\nreturn 1\n"),
            "local x = f()\nreturn 1\n"
        );
        // The metatable behind an index must also run.
        assert_eq!(
            sweep("local x = t.k\nreturn 1\n"),
            "local x = t.k\nreturn 1\n"
        );
    }

    #[test]
    fn leaves_a_partly_used_multi_binding() {
        // Removal of half would change the number of values that the right side supplies.
        assert_eq!(
            sweep("local a, b = 1, 2\nreturn a\n"),
            "local a, b = 1, 2\nreturn a\n"
        );
    }

    #[test]
    fn drops_an_unused_local_function() {
        assert_eq!(sweep("local function f() end\nreturn 1\n"), "\nreturn 1\n");
    }

    #[test]
    fn keeps_params_and_loop_variables() {
        assert_eq!(
            sweep("local function f(a) return 1 end\nreturn f\n"),
            "local function f(a) return 1 end\nreturn f\n"
        );
        assert_eq!(
            sweep("for i = 1, 3 do print(1) end\n"),
            "for i = 1, 3 do print(1) end\n"
        );
    }

    #[test]
    fn reaches_into_nested_blocks() {
        // The indent stays behind, the same as each other rule that deletes.
        assert_eq!(
            sweep("do\n    local x = 1\nend\nreturn 2\n"),
            "do\n    \nend\nreturn 2\n"
        );
    }

    #[test]
    fn a_recursive_call_counts_as_a_use() {
        let src = "local function f() return f() end\nreturn 1\n";
        assert_eq!(sweep(src), src);
    }
}

#[cfg(test)]
mod rename_tests {
    use super::*;
    use crate::rules::darklua::testing::{assert_lines_kept, run};

    fn rename(src: &str) -> String {
        let out = run(src, rename_variables);
        assert_lines_kept(src, &out);
        parses(&out);

        out
    }

    /// A rename that strands a name in a type produces source that still lexes.
    /// Only a parse proves the output is Luau.
    fn parses(src: &str) {
        let lexed = crate::syntax::lexer::lex(src).expect("lexes");
        crate::syntax::parser::parse(src, &lexed.toks).expect("parses");
    }

    #[test]
    fn locals_get_short_names() {
        assert_eq!(
            rename("local counter = 1\nreturn counter\n"),
            "local a = 1\nreturn a\n"
        );
    }

    #[test]
    fn every_binding_gets_its_own_name() {
        assert_eq!(
            rename("local one = 1\nlocal two = 2\nreturn one + two\n"),
            "local a = 1\nlocal b = 2\nreturn a + b\n"
        );
    }

    #[test]
    fn globals_are_never_touched() {
        assert_eq!(
            rename("local x = print\nreturn print(x)\n"),
            "local a = print\nreturn print(a)\n"
        );
    }

    #[test]
    fn a_name_the_file_already_uses_is_not_handed_out() {
        // `a` is a global here, so no local can take it.
        let out = rename("local counter = a\nreturn counter\n");
        assert!(out.contains("= a\n"), "{out}");
        assert!(!out.starts_with("local a ="), "{out}");
    }

    #[test]
    fn params_and_loop_variables_rename_too() {
        assert_eq!(
            rename("local function f(value) return value end\nreturn f\n"),
            "local function a(b) return b end\nreturn a\n"
        );
    }

    #[test]
    fn shadowing_still_resolves_to_the_inner_one() {
        assert_eq!(
            rename("local x = 1\ndo local x = 2\nreturn x end\n"),
            "local a = 1\ndo local b = 2\nreturn b end\n"
        );
    }

    /// The reported defect. The cast held the old name while the binding moved.
    #[test]
    fn a_cast_takes_the_new_name_of_its_prefix() {
        assert_eq!(
            rename(
                "local jecs = require(\"@pkg/jecs\")\nlocal e = x :: jecs.Component\nreturn e\n"
            ),
            "local a = require(\"@pkg/jecs\")\nlocal b = x :: a.Component\nreturn b\n"
        );
    }

    /// The same file without the cast. This is what already worked.
    #[test]
    fn the_same_file_without_a_cast_is_unchanged() {
        assert_eq!(
            rename("local jecs = require(\"@pkg/jecs\")\nlocal e = x\nreturn e\n"),
            "local a = require(\"@pkg/jecs\")\nlocal b = x\nreturn b\n"
        );
    }

    #[test]
    fn an_annotation_takes_the_new_name_of_its_prefix() {
        assert_eq!(
            rename("local jecs = require(\"@pkg/jecs\")\nlocal e: jecs.Entity = x\nreturn e\n"),
            "local a = require(\"@pkg/jecs\")\nlocal b: a.Entity = x\nreturn b\n"
        );
    }

    #[test]
    fn a_generic_argument_takes_the_new_name_of_its_prefix() {
        assert_eq!(
            rename(
                "local jecs = require(\"@pkg/jecs\")\nlocal e: Array<jecs.Entity> = x\nreturn e\n"
            ),
            "local a = require(\"@pkg/jecs\")\nlocal b: Array<a.Entity> = x\nreturn b\n"
        );
    }

    #[test]
    fn a_prefix_in_an_alias_a_return_type_and_a_parameter_all_follow() {
        assert_eq!(
            rename(
                "local t = require(\"@pkg/t\")\ntype E = t.Entity\nlocal function f(v: t.Id): t.Out return v end\nreturn f\n"
            ),
            "local a = require(\"@pkg/t\")\ntype E = a.Entity\nlocal function b(c: a.Id): a.Out return c end\nreturn b\n"
        );
    }

    /// A table type puts the field name in front of the colon, so `e` declares
    /// and does not read. Only the prefix behind the colon moves.
    #[test]
    fn a_field_name_in_a_table_type_is_not_a_reference() {
        assert_eq!(
            rename("local t = require(\"@pkg/t\")\ntype R = { e: t.Entity }\nreturn t\n"),
            "local a = require(\"@pkg/t\")\ntype R = { e: a.Entity }\nreturn a\n"
        );
    }

    #[test]
    fn typeof_reads_a_value_so_the_local_and_the_type_move_together() {
        assert_eq!(
            rename("local config = {}\ntype T = typeof(config)\nreturn config\n"),
            "local a = {}\ntype T = typeof(a)\nreturn a\n"
        );
    }

    /// A bare name in a type is a type. The walk cannot prove the local and the
    /// alias are the same thing, so it renames neither.
    #[test]
    fn a_bare_name_in_a_type_blocks_the_local_of_that_name() {
        let src = "local Entity = 1\ntype T = Entity\nreturn Entity\n";
        assert_eq!(rename(src), src);
    }

    /// A type function runs at check time and cannot see a runtime local, so a
    /// name in its body means something else.
    #[test]
    fn a_type_function_body_blocks_the_names_it_mentions() {
        let src = "local ty = 1\ntype function f(t) return ty.x end\nreturn ty\n";
        assert_eq!(rename(src), src);
    }

    /// The type positions that the old walk never reached.
    #[test]
    fn a_cast_in_a_loop_header_and_a_call_type_argument_follow() {
        assert_eq!(
            rename(
                "local t = require(\"@pkg/t\")\nfor i = 1, (n :: t.Count) do f<<t.Id>>(i) end\n"
            ),
            "local a = require(\"@pkg/t\")\nfor b = 1, (n :: a.Count) do f<<a.Id>>(b) end\n"
        );
    }
}
