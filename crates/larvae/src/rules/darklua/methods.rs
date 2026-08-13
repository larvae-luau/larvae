/*!
Rules that reshape function declarations and calls

remove_method_definition, remove_method_call,
convert_function_to_assignment, and convert_local_function_to_assign all
move the implicit `self`. For this reason, they share the parameter list
rewrite in one place.
*/

use super::support::{self, insert, tok_bytes};
use crate::rules::engine::{Edit, Flow, RuleCtx, Visit, walk_block, walk_chunk};
use crate::syntax::ast::*;

/*
Put `self` at the head of a parameter list. Return false when the `(` is
not at the position that the tree gives. The caller then keeps the whole
definition and does not emit half a rewrite.
*/
fn insert_self(ctx: &RuleCtx, body: &FunctionBody, edits: &mut Vec<Edit>) -> bool {
    let Some(lparen) = support::params_lparen(ctx, body) else {
        return false;
    };

    let (_, after) = tok_bytes(ctx, lparen);
    let text = if body.params.is_empty() {
        "self"
    } else {
        "self, "
    };

    insert(after, text, edits);

    true
}

/// The token index of the `:` before a method name, checked against the source.
fn method_colon(ctx: &RuleCtx, name: TokSpan) -> Option<u32> {
    let idx = name.start.checked_sub(1)?;

    (ctx.tok_text(idx) == ":").then_some(idx)
}

/// remove_method_definition: `function C:m()` becomes `function C.m(self)`.
pub fn remove_method_definition(ctx: &RuleCtx, edits: &mut Vec<Edit>) {
    struct V<'a, 'b> {
        ctx: &'a RuleCtx<'b>,
        edits: &'a mut Vec<Edit>,
    }

    impl Visit for V<'_, '_> {
        fn stmt(&mut self, s: &Stmt) -> Flow {
            let Stmt::Function(f) = s else {
                return Flow::Next;
            };

            if !f.is_method {
                return Flow::Next;
            }

            let Some(&last) = f.path.last() else {
                return Flow::Next;
            };
            let Some(colon) = method_colon(self.ctx, last) else {
                return Flow::Next;
            };

            // Apply both edits or neither. A dangling `.` would not compile.
            let mut staged = Vec::new();

            if !insert_self(self.ctx, &f.body, &mut staged) {
                return Flow::Next;
            }

            let (a, b) = tok_bytes(self.ctx, colon);
            self.edits.push((a, b, ".".to_string()));
            self.edits.append(&mut staged);

            Flow::Next
        }
    }

    walk_chunk(ctx.chunk, &mut V { ctx, edits });
}

/*
convert_function_to_assignment: `function a.b()` becomes
`a.b = function()`. A method definition also gains the explicit self
parameter. The rule does not change attributed functions, because
`@native f = function()` is not valid Luau.
*/
pub fn convert_function_to_assignment(ctx: &RuleCtx, edits: &mut Vec<Edit>) {
    struct V<'a, 'b> {
        ctx: &'a RuleCtx<'b>,
        edits: &'a mut Vec<Edit>,
    }

    impl Visit for V<'_, '_> {
        fn stmt(&mut self, s: &Stmt) -> Flow {
            let Stmt::Function(f) = s else {
                return Flow::Next;
            };

            if !f.attributes.is_empty() || f.path.is_empty() {
                return Flow::Next;
            }

            let Some(kw) = f.path[0].start.checked_sub(1) else {
                return Flow::Next;
            };

            if self.ctx.tok_text(kw) != "function" {
                return Flow::Next;
            }

            let mut staged = Vec::new();

            if f.is_method && !insert_self(self.ctx, &f.body, &mut staged) {
                return Flow::Next;
            }

            let path: Vec<&str> = f.path.iter().map(|&p| self.ctx.text(p)).collect();
            let target = path.join(".");
            let from = tok_bytes(self.ctx, kw).0;
            let to = self.ctx.bytes(*f.path.last().unwrap()).1;

            // The head is always one line, so no newline bookkeeping is necessary here.
            self.edits.push((from, to, format!("{target} = function")));
            self.edits.append(&mut staged);

            Flow::Next
        }
    }

    walk_chunk(ctx.chunk, &mut V { ctx, edits });
}

/*
convert_local_function_to_assign: `local function f()` becomes
`local f = function()`. The rule applies only when the body never
mentions f. The local form is in scope inside itself, and the assignment
form is not.
*/
pub fn convert_local_function_to_assign(ctx: &RuleCtx, edits: &mut Vec<Edit>) {
    struct V<'a, 'b> {
        ctx: &'a RuleCtx<'b>,
        edits: &'a mut Vec<Edit>,
    }

    impl Visit for V<'_, '_> {
        fn stmt(&mut self, s: &Stmt) -> Flow {
            let Stmt::LocalFunction(f) = s else {
                return Flow::Next;
            };

            if !f.attributes.is_empty() {
                return Flow::Next;
            }

            let name = self.ctx.text(f.name);

            if references_name(self.ctx, &f.body.block, name) {
                return Flow::Next;
            }

            let Some(kw) = f.name.start.checked_sub(1) else {
                return Flow::Next;
            };

            if self.ctx.tok_text(kw) != "function" {
                return Flow::Next;
            }

            let from = tok_bytes(self.ctx, kw).0;
            let to = self.ctx.bytes(f.name).1;

            self.edits.push((from, to, format!("{name} = function")));

            Flow::Next
        }
    }

    walk_chunk(ctx.chunk, &mut V { ctx, edits });
}

/*
True when this block reads the name anywhere. A shadowing local inside
counts as a hit. That costs larvae a possible transform, but it never
causes a wrong transform.
*/
fn references_name(ctx: &RuleCtx, block: &Block, name: &str) -> bool {
    struct V<'a, 'b> {
        ctx: &'a RuleCtx<'b>,
        name: &'a str,
        found: bool,
    }

    impl Visit for V<'_, '_> {
        fn expr(&mut self, e: &Expr) -> Flow {
            if let Expr::Name(s) = e
                && self.ctx.text(*s) == self.name
            {
                self.found = true;
            }

            Flow::Next
        }
    }

    let mut v = V {
        ctx,
        name,
        found: false,
    };

    walk_block(block, &mut v);

    v.found
}

/*
remove_method_call: `obj:m(x)` becomes `obj.m(obj, x)`. The rule applies
only to a plain identifier receiver. Each other receiver would run twice.
*/
pub fn remove_method_call(ctx: &RuleCtx, edits: &mut Vec<Edit>) {
    struct V<'a, 'b> {
        ctx: &'a RuleCtx<'b>,
        edits: &'a mut Vec<Edit>,
    }

    impl Visit for V<'_, '_> {
        fn expr(&mut self, e: &Expr) -> Flow {
            let Expr::Call {
                func, method, args, ..
            } = e
            else {
                return Flow::Next;
            };

            let Some(m) = method else { return Flow::Next };
            let Expr::Name(recv_span) = func.as_ref() else {
                return Flow::Next;
            };

            let recv = self.ctx.text(*recv_span);
            let Some(colon) = method_colon(self.ctx, *m) else {
                return Flow::Next;
            };

            let mut staged: Vec<Edit> = Vec::new();

            match args {
                CallArgs::Paren(list) => {
                    // The `(` is directly after the method name.
                    let lparen = m.end;

                    if self.ctx.tok_text(lparen) != "(" {
                        return Flow::Next;
                    }

                    let after = tok_bytes(self.ctx, lparen).1;
                    let text = if list.is_empty() {
                        recv.to_string()
                    } else {
                        format!("{recv}, ")
                    };

                    insert(after, &text, &mut staged);
                }

                // A parenless call must gain parentheses to accept the receiver.
                CallArgs::Str(s) => {
                    let (a, b) = self.ctx.bytes(*s);
                    let lit = &self.ctx.src[a as usize..b as usize];

                    staged.push((a, b, format!("({recv}, {lit})")));
                }

                CallArgs::Table(t) => {
                    let (a, b) = self.ctx.bytes(t.span());
                    insert(a, &format!("({recv}, "), &mut staged);
                    insert(b, ")", &mut staged);
                }
            }

            let (ca, cb) = tok_bytes(self.ctx, colon);
            self.edits.push((ca, cb, ".".to_string()));
            self.edits.append(&mut staged);

            Flow::Next
        }
    }

    walk_chunk(ctx.chunk, &mut V { ctx, edits });
}

#[cfg(test)]
mod tests {
    use super::super::testing::{assert_lines_kept, run};
    use super::*;

    #[test]
    fn method_definition_gains_self() {
        assert_eq!(
            run("function C:m() end\n", remove_method_definition),
            "function C.m(self) end\n"
        );
        assert_eq!(
            run("function C:m(a, b) end\n", remove_method_definition),
            "function C.m(self, a, b) end\n"
        );
        assert_eq!(
            run("function a.b.c:m(...) end\n", remove_method_definition),
            "function a.b.c.m(self, ...) end\n"
        );
    }

    #[test]
    fn plain_definitions_are_left_alone() {
        let src = "function C.m(a) end\nlocal function f() end\n";
        assert_eq!(run(src, remove_method_definition), src);
    }

    #[test]
    fn function_becomes_assignment() {
        assert_eq!(
            run("function foo() end\n", convert_function_to_assignment),
            "foo = function() end\n"
        );
        assert_eq!(
            run("function a.b.c(x) end\n", convert_function_to_assignment),
            "a.b.c = function(x) end\n"
        );
        assert_eq!(
            run("function o:m(x) end\n", convert_function_to_assignment),
            "o.m = function(self, x) end\n"
        );
    }

    #[test]
    fn attributed_and_local_functions_stay_put() {
        let src = "@native function f() end\n";
        assert_eq!(run(src, convert_function_to_assignment), src);
        let src = "local function f() end\n";
        assert_eq!(run(src, convert_function_to_assignment), src);
    }

    #[test]
    fn local_function_becomes_assignment_when_not_recursive() {
        assert_eq!(
            run(
                "local function f(a) return a end\n",
                convert_local_function_to_assign
            ),
            "local f = function(a) return a end\n"
        );
    }

    #[test]
    fn recursive_local_function_is_left_alone() {
        let src = "local function f(n) return f(n - 1) end\n";
        assert_eq!(run(src, convert_local_function_to_assign), src);
        // A mention that is not a call also counts.
        let src = "local function f() return f end\n";
        assert_eq!(run(src, convert_local_function_to_assign), src);
    }

    #[test]
    fn method_call_passes_the_receiver() {
        assert_eq!(run("obj:m(x)\n", remove_method_call), "obj.m(obj, x)\n");
        assert_eq!(run("obj:m()\n", remove_method_call), "obj.m(obj)\n");
        assert_eq!(
            run("obj:m\"s\"\n", remove_method_call),
            "obj.m(obj, \"s\")\n"
        );
        assert_eq!(
            run("obj:m{ a = 1 }\n", remove_method_call),
            "obj.m(obj, { a = 1 })\n"
        );
    }

    #[test]
    fn non_identifier_receivers_are_left_alone() {
        // A second copy of `a.b` or a call result would change the code that runs.
        let src = "a.b:m(x)\n";
        assert_eq!(run(src, remove_method_call), src);
        let src = "f():m(x)\n";
        assert_eq!(run(src, remove_method_call), src);
        // A plain call has no receiver to move.
        let src = "m(x)\n";
        assert_eq!(run(src, remove_method_call), src);
    }

    #[test]
    fn multiline_definitions_keep_their_lines() {
        let src = "function C:m(\n    a,\n    b\n)\nend\n";
        let out = run(src, remove_method_definition);

        assert_lines_kept(src, &out);
        assert!(out.contains("C.m(self, "), "{out}");
    }
}
