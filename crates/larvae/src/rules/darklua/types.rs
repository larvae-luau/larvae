/*!
Rules that remove Luau only annotations for plain Lua

The parser keeps types as opaque token spans, and these rules need
exactly that. The extent is known, and the rules never read the contents.
One detail remains: a type span starts at the type itself, so the rule
must find the colon in front of it by hand.
*/

use super::support::tok_bytes;
use crate::rules::engine::{Edit, Flow, RuleCtx, Visit, walk_chunk};
use crate::syntax::ast::*;

/*
Delete a type together with the punctuation that introduces it. `lead` is
the expected token in front. Example: `:` for an annotation and `::` for
an assertion.
*/
fn drop_type_with_lead(ctx: &RuleCtx, ty: TokSpan, lead: &str, edits: &mut Vec<Edit>) {
    let Some(idx) = ty.start.checked_sub(1) else {
        return;
    };

    if ctx.tok_text(idx) != lead {
        return;
    }

    let mut from = tok_bytes(ctx, idx).0;
    // Also remove the space in front. `y :: T` must not leave `y ` behind.
    let bytes = ctx.src.as_bytes();

    while from > 0 && matches!(bytes[from as usize - 1], b' ' | b'\t') {
        from -= 1;
    }

    let to = ctx.bytes(ty).1;
    ctx.delete_bytes_keep_lines(from, to, edits);
}

fn drop_binding_type(ctx: &RuleCtx, ty: Option<TokSpan>, edits: &mut Vec<Edit>) {
    if let Some(ty) = ty {
        drop_type_with_lead(ctx, ty, ":", edits);
    }
}

fn drop_body_types(ctx: &RuleCtx, body: &FunctionBody, edits: &mut Vec<Edit>) {
    if let Some(g) = body.generics {
        ctx.delete_keep_lines(g, edits);
    }

    for p in &body.params {
        drop_binding_type(ctx, p.ty, edits);
    }

    if let Some(r) = body.ret_type {
        drop_type_with_lead(ctx, r, ":", edits);
    }
}

/*
remove_types: remove each annotation. This covers bindings, parameters,
return types, generic lists, whole type aliases, and the tail of a `::`
assertion.
*/
pub fn remove_types(ctx: &RuleCtx, edits: &mut Vec<Edit>) {
    struct V<'a, 'b> {
        ctx: &'a RuleCtx<'b>,
        edits: &'a mut Vec<Edit>,
    }

    impl Visit for V<'_, '_> {
        fn stmt(&mut self, s: &Stmt) -> Flow {
            match s {
                Stmt::Local(l) => {
                    for n in &l.names {
                        drop_binding_type(self.ctx, n.ty, self.edits);
                    }
                }

                Stmt::NumericFor(f) => drop_binding_type(self.ctx, f.var.ty, self.edits),

                Stmt::GenericFor(f) => {
                    for v in &f.vars {
                        drop_binding_type(self.ctx, v.ty, self.edits);
                    }
                }

                Stmt::Function(f) => drop_body_types(self.ctx, &f.body, self.edits),

                Stmt::LocalFunction(f) => drop_body_types(self.ctx, &f.body, self.edits),
                // An alias exists only for the type checker. The rule removes it whole.
                Stmt::TypeAlias(t) => self.ctx.delete_keep_lines(t.span, self.edits),
                _ => {}
            }

            Flow::Next
        }

        fn expr(&mut self, e: &Expr) -> Flow {
            match e {
                Expr::Function { body, .. } => drop_body_types(self.ctx, body, self.edits),

                Expr::TypeAssert { ty, .. } => drop_type_with_lead(self.ctx, *ty, "::", self.edits),
                _ => {}
            }

            Flow::Next
        }
    }

    walk_chunk(ctx.chunk, &mut V { ctx, edits });
}

/*
remove_attribute: remove function attributes. `match` holds regexes that
the rule tests against the attribute name without its `@`. An empty list
removes all attributes.
*/
pub fn remove_attribute(ctx: &RuleCtx, edits: &mut Vec<Edit>, patterns: &[String]) {
    let compiled: Vec<regex::Regex> = patterns
        .iter()
        .filter_map(|p| regex::Regex::new(p).ok())
        .collect();
    // Config load catches a pattern list that does not compile.
    if compiled.len() != patterns.len() {
        return;
    }

    struct V<'a, 'b> {
        ctx: &'a RuleCtx<'b>,
        edits: &'a mut Vec<Edit>,
        patterns: Vec<regex::Regex>,
    }

    impl V<'_, '_> {
        fn strip(&mut self, attrs: &[TokSpan]) {
            for &a in attrs {
                let text = self.ctx.text(a);
                let name = text.trim_start_matches('@');

                if !self.patterns.is_empty() && !self.patterns.iter().any(|re| re.is_match(name)) {
                    continue;
                }

                let (from, mut to) = self.ctx.bytes(a);
                // Also remove the spaces after it, so the line does not start blank.
                let bytes = self.ctx.src.as_bytes();

                while (to as usize) < bytes.len() && matches!(bytes[to as usize], b' ' | b'\t') {
                    to += 1;
                }

                self.ctx.delete_bytes_keep_lines(from, to, self.edits);
            }
        }
    }

    impl Visit for V<'_, '_> {
        fn stmt(&mut self, s: &Stmt) -> Flow {
            match s {
                Stmt::Function(f) => self.strip(&f.attributes),

                Stmt::LocalFunction(f) => self.strip(&f.attributes),
                _ => {}
            }

            Flow::Next
        }

        fn expr(&mut self, e: &Expr) -> Flow {
            if let Expr::Function { attributes, .. } = e {
                self.strip(attributes);
            }

            Flow::Next
        }
    }

    walk_chunk(
        ctx.chunk,
        &mut V {
            ctx,
            edits,
            patterns: compiled,
        },
    );
}

#[cfg(test)]
mod tests {
    use super::super::testing::{assert_lines_kept, run};
    use super::*;

    #[test]
    fn annotations_come_off() {
        assert_eq!(run("local x: number = 1\n", remove_types), "local x = 1\n");
        assert_eq!(
            run("local function f(a: string): boolean end\n", remove_types),
            "local function f(a) end\n"
        );
        assert_eq!(
            run("function f<T>(a: T): T end\n", remove_types),
            "function f(a) end\n"
        );
    }

    #[test]
    fn assertions_keep_their_value() {
        assert_eq!(
            run("local x = y :: number\n", remove_types),
            "local x = y\n"
        );
    }

    #[test]
    fn aliases_go_whole_and_keep_lines() {
        let src = "type Point = { x: number }\nlocal p = 1\n";
        let out = run(src, remove_types);

        assert!(!out.contains("Point"), "{out}");
        assert!(out.contains("local p = 1"), "{out}");
        assert_lines_kept(src, &out);

        let src = "export type A = {\n    x: number,\n}\nreturn 1\n";
        let out = run(src, remove_types);

        assert!(!out.contains("export"), "{out}");
        assert_lines_kept(src, &out);
    }

    #[test]
    fn untyped_code_is_untouched() {
        let src = "local x = 1\nlocal function f(a) return a end\n";
        assert_eq!(run(src, remove_types), src);
    }

    #[test]
    fn for_loop_bindings_lose_their_types() {
        assert_eq!(
            run("for i: number = 1, 10 do end\n", remove_types),
            "for i = 1, 10 do end\n"
        );
        assert_eq!(
            run("for k: string, v: number in t do end\n", remove_types),
            "for k, v in t do end\n"
        );
    }

    fn strip_all(ctx: &RuleCtx, edits: &mut Vec<Edit>) {
        remove_attribute(ctx, edits, &[]);
    }

    #[test]
    fn attributes_come_off() {
        assert_eq!(
            run("@native function f() end\n", strip_all),
            "function f() end\n"
        );
        assert_eq!(
            run("@native @checked function f() end\n", strip_all),
            "function f() end\n"
        );
    }

    #[test]
    fn match_selects_which_attributes_go() {
        fn only_native(ctx: &RuleCtx, edits: &mut Vec<Edit>) {
            remove_attribute(ctx, edits, &["^native$".to_string()]);
        }

        assert_eq!(
            run("@native @checked function f() end\n", only_native),
            "@checked function f() end\n"
        );
        // No pattern matches, so nothing changes.
        let src = "@checked function f() end\n";
        assert_eq!(run(src, only_native), src);
    }
}
