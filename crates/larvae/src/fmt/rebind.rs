/*!
Decides which keyword binds each required module, for `require_binding`.

The decision lives here and not in the emitter, because the conversion to
`const` is the one formatting choice that can stop a file from compiling.
Luau enforces `const`:

```text
SyntaxError: Variable 'X' is constant and may not be reassigned
```

So a name that something reassigns must keep `local`. That knowledge needs
the scope resolution that the linter already builds. The emitter receives the
answer as a lookup, and not the machinery that works it out.

The other direction, `const` to `local`, is always safe. It only removes a
restriction.
*/

use std::collections::HashMap;

use crate::syntax::ast::*;
use crate::syntax::lexer::Tok;

use super::config::RequireBinding;

/// Maps a keyword token index to the keyword that must replace it.
pub type Rebindings = HashMap<u32, &'static str>;

pub fn plan(src: &str, toks: &[Tok], chunk: &Chunk, mode: RequireBinding) -> Rebindings {
    let mut out = Rebindings::new();

    if mode == RequireBinding::Preserve {
        return out;
    }

    /*
    Only `const` needs the resolution, and the resolution is a whole extra
    walk of the file. So the safe direction does not pay for it.
    */
    let names = match mode {
        RequireBinding::Const => Some(crate::lint::scope::resolve(src, toks, chunk)),

        _ => None,
    };

    let mut locals = Vec::new();
    collect(chunk, &mut locals);

    for local in locals {
        let Some(binding) = single_require_binding(src, toks, local) else {
            continue;
        };

        match mode {
            RequireBinding::Local if local.is_const => {
                out.insert(local.keyword.start, "local");
            }

            RequireBinding::Const if !local.is_const => {
                let names = names.as_ref().expect("resolved for const");

                // A later statement reassigns the name, so const would be a syntax error.
                let writable = names
                    .by_token
                    .get(&binding.name.start)
                    .and_then(|&i| names.bindings.get(i))
                    .is_none_or(|b| !b.writes.is_empty());

                if !writable {
                    out.insert(local.keyword.start, "const");
                }
            }

            _ => {}
        }
    }

    out
}

/*
Returns the name that a `local X = require(...)` binds, when it is exactly
that.

The match is narrow on purpose, and it follows the `const_requires` transform
and the `non_const_require` lint: one name, one value, no type annotation. A
multi binding cannot be const one name at a time, and an annotated local
states something that the author cared about.
*/
fn single_require_binding<'a>(src: &str, toks: &[Tok], local: &'a Local) -> Option<&'a Binding> {
    let ([binding], [value]) = (local.names.as_slice(), local.values.as_slice()) else {
        return None;
    };

    if binding.ty.is_some() {
        return None;
    }

    let Expr::Call { func, method, .. } = value else {
        return None;
    };

    if method.is_some() {
        return None;
    }

    match func.as_ref() {
        Expr::Name(n) => (toks[n.start as usize].text(src) == "require").then_some(binding),

        _ => None,
    }
}

/*
Collects every `local` in the file, with the nested ones included.

The walk is written out and does not go through the shared `Visit` trait.
That trait hands a callback a reference whose lifetime is the visit call and
not the tree, so the collector cannot store those references.
*/
fn collect<'a>(chunk: &'a Chunk, out: &mut Vec<&'a Local>) {
    block(&chunk.block, out);
}

fn block<'a>(b: &'a Block, out: &mut Vec<&'a Local>) {
    for s in &b.stmts {
        stmt(s, out);
    }
}

fn stmt<'a>(s: &'a Stmt, out: &mut Vec<&'a Local>) {
    match s {
        Stmt::Local(n) => {
            out.push(n);

            for v in &n.values {
                expr(v, out);
            }
        }

        Stmt::Assign(n) => {
            for e in n.targets.iter().chain(&n.values) {
                expr(e, out);
            }
        }

        Stmt::Call(e, _) => expr(e, out),

        Stmt::Return(n) => {
            for e in &n.values {
                expr(e, out);
            }
        }

        Stmt::Do(n) => block(&n.block, out),

        Stmt::While(n) => {
            expr(&n.cond, out);
            block(&n.block, out);
        }

        Stmt::Repeat(n) => {
            block(&n.block, out);
            expr(&n.cond, out);
        }

        Stmt::If(n) => {
            for (cond, body) in &n.branches {
                expr(cond, out);
                block(body, out);
            }

            if let Some(body) = &n.else_block {
                block(body, out);
            }
        }

        Stmt::NumericFor(n) => {
            for e in [&n.start, &n.limit].into_iter().chain(n.step.as_ref()) {
                expr(e, out);
            }

            block(&n.block, out);
        }

        Stmt::GenericFor(n) => {
            for e in &n.exprs {
                expr(e, out);
            }

            block(&n.block, out);
        }

        Stmt::Function(n) => block(&n.body.block, out),

        Stmt::LocalFunction(n) => block(&n.body.block, out),

        _ => {}
    }
}

/// Descends only into the places where a statement can hide, which is a function body.
fn expr<'a>(e: &'a Expr, out: &mut Vec<&'a Local>) {
    match e {
        Expr::Function { body, .. } => block(&body.block, out),

        Expr::Binary { lhs, rhs, .. } => {
            expr(lhs, out);
            expr(rhs, out);
        }

        Expr::Unary { operand, .. } => expr(operand, out),

        Expr::Paren { inner, .. } => expr(inner, out),

        Expr::TypeAssert { expr: inner, .. } => expr(inner, out),

        Expr::Index { object, key, .. } => {
            expr(object, out);

            if let IndexKey::Computed(k) = key {
                expr(k, out);
            }
        }

        Expr::Call { func, args, .. } => {
            expr(func, out);

            match args {
                CallArgs::Paren(list) => list.iter().for_each(|a| expr(a, out)),

                CallArgs::Table(t) => expr(t, out),

                CallArgs::Str(_) => {}
            }
        }

        Expr::Table { fields, .. } => {
            for f in fields {
                match f {
                    TableField::Positional(v) => expr(v, out),

                    TableField::Named { value, .. } => expr(value, out),

                    TableField::Computed { key, value } => {
                        expr(key, out);
                        expr(value, out);
                    }
                }
            }
        }

        Expr::IfElse {
            branches,
            else_value,
            ..
        } => {
            for (cond, value) in branches {
                expr(cond, out);
                expr(value, out);
            }

            expr(else_value, out);
        }

        _ => {}
    }
}
