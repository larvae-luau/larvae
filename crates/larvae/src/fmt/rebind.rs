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

use super::config::{PreferConst, RequireBinding};

/// Maps a keyword token index to the keyword that must replace it.
pub type Rebindings = HashMap<u32, &'static str>;

pub fn plan(
    src: &str,
    toks: &[Tok],
    chunk: &Chunk,
    mode: RequireBinding,
    prefer: &PreferConst,
) -> Rebindings {
    let mut out = Rebindings::new();

    if mode == RequireBinding::Preserve && !prefer.enabled {
        return out;
    }

    /*
    Only `const` needs the resolution, and the resolution is a whole extra
    walk of the file. So the safe direction does not pay for it.
    */
    let names = match mode == RequireBinding::Const || prefer.enabled {
        true => Some(crate::lint::scope::resolve(src, toks, chunk)),

        false => None,
    };

    let mut locals = Vec::new();
    collect(chunk, &mut locals);

    /*
    `prefer_const` runs first, and it covers every declaration rather than
    only the ones that hold a require. A require binding that it already
    turned into `const` needs nothing from the pass below.
    */
    if prefer.enabled {
        let names = names.as_ref().expect("resolved for const");
        let mutated = match prefer.mutated_tables_stay_local {
            true => mutated_bindings(src, toks, chunk, names),

            false => std::collections::HashSet::new(),
        };

        for local in &locals {
            if takes_const(local, names, &mutated) {
                out.insert(local.keyword.start, "const");
            }
        }
    }

    for local in &locals {
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
The keyword each function declaration takes, for `function_style`.

Three spellings mean one thing at the top level of a file: `local
function f`, `const function f`, and a bare `function f`. Inside a
table they do not: `function t.m()` and `function t:m()` assign a
field, and no keyword can precede them. An anonymous function is an
expression and declares nothing. Both stay as written, whatever the
option says.

`const` needs one more proof, the same one `require_binding` needs: a
name something reassigns cannot take it, because Luau refuses the
program. A conversion the resolver cannot prove safe stays put.
*/
pub fn function_plan(
    src: &str,
    toks: &[Tok],
    chunk: &Chunk,
    style: super::config::FunctionStyle,
) -> Rebindings {
    use super::config::FunctionStyle;

    let mut out = Rebindings::new();

    if style == FunctionStyle::Preserve {
        return out;
    }

    let names = match style == FunctionStyle::Const {
        true => Some(crate::lint::scope::resolve(src, toks, chunk)),

        false => None,
    };

    /*
    Only the top level converts. A declaration inside a block is
    scoped to it, so a bare `function f` there writes a global and the
    two spellings stop meaning the same thing.
    */
    for stmt in &chunk.block.stmts {
        match stmt {
            Stmt::LocalFunction(n) => {
                // The statement opens on its keyword, `local` or `const`.
                let keyword = n.span.start;

                match style {
                    FunctionStyle::Global => {
                        out.insert(keyword, "");
                    }

                    FunctionStyle::Local if n.is_const => {
                        out.insert(keyword, "local");
                    }

                    FunctionStyle::Const if !n.is_const => {
                        let names = names.as_ref().expect("resolved for const");
                        let writable = names
                            .by_token
                            .get(&n.name.start)
                            .and_then(|&i| names.bindings.get(i))
                            .is_none_or(|b| !b.writes.is_empty());

                        if !writable {
                            out.insert(keyword, "const");
                        }
                    }

                    _ => {}
                }
            }

            /*
            A bare `function f` takes a keyword only where the name is
            one word. `function t.m` and `function t:m` assign a field
            of a table, which no keyword can precede.
            */
            Stmt::Function(n) if n.path.len() == 1 && !n.is_method && !n.exported => {
                let word = match style {
                    FunctionStyle::Local => "local",
                    FunctionStyle::Const => "const",

                    _ => continue,
                };

                /*
                A bare declaration writes a global, so the resolver has
                no binding to read. The proof is the other list: a write
                to that name anywhere else in the file, the declaration
                itself excepted, is a reassignment `const` refuses.
                */
                if word == "const" {
                    let names = names.as_ref().expect("resolved for const");
                    let name = n.path[0];
                    let text = &src[toks[name.start as usize].start as usize
                        ..toks[name.start as usize].end as usize];

                    let reassigned = names.global_writes.iter().any(|&at| {
                        at != name.start
                            && !names.global_functions.contains(&at)
                            && &src
                                [toks[at as usize].start as usize..toks[at as usize].end as usize]
                                == text
                    });

                    if reassigned {
                        continue;
                    }
                }

                out.insert(n.span.start, word);
            }

            _ => {}
        }
    }

    out
}

/*
Reports if this declaration can become `const`.

Three rules, and each one is a place where the swap would not compile or
would not apply. Luau needs an initialiser, so a declaration with no value
stays. `const` binds the declaration and not one name inside it, so every
name has to be free of later assignment. And a declaration already saying
`const` needs nothing.

The rules are the ones the `prefer_const` lint reports on, because the lint
and this option describe the same shape.
*/
fn takes_const(
    local: &Local,
    names: &crate::lint::scope::Names<'_>,
    mutated: &std::collections::HashSet<usize>,
) -> bool {
    if local.is_const || local.values.is_empty() {
        return false;
    }

    local.names.iter().all(|binding| {
        names
            .by_token
            .get(&binding.name.start)
            .and_then(|&i| names.bindings.get(i).map(|b| (i, b)))
            .is_some_and(|(i, b)| b.writes.is_empty() && !mutated.contains(&i))
    })
}

/*
The bindings the file changes through a field or a `table` function.

`t.x = 1` and `table.insert(t, 1)` leave the binding itself alone, so the
resolver records no write for either. A project that wants a mutated table to
keep `local` needs them found, and this is the walk that finds them. It is the
same rule as `[lint.options.prefer_const]`, so the two answer alike.
*/
fn mutated_bindings(
    src: &str,
    toks: &[Tok],
    chunk: &Chunk,
    names: &crate::lint::scope::Names<'_>,
) -> std::collections::HashSet<usize> {
    let mut out = std::collections::HashSet::new();
    // The linter already walks a chunk into these three lists; one walk serves both.
    let (exprs, stmts, _) = crate::lint::ctx::flatten(chunk);

    let text = |span: TokSpan| toks[span.start as usize].text(src);

    let mut mark = |root: Option<TokSpan>| {
        if let Some(span) = root
            && let Some(&index) = names.read_of.get(&span.start)
        {
            out.insert(index);
        }
    };

    for stmt in &stmts {
        let Stmt::Assign(n) = stmt else {
            continue;
        };

        for target in &n.targets {
            if matches!(target, Expr::Index { .. }) {
                mark(root_name(target));
            }
        }
    }

    for expr in &exprs {
        let Expr::Call { func, args, .. } = expr else {
            continue;
        };

        let Expr::Index {
            object,
            key: IndexKey::Field(name),
            ..
        } = func.as_ref()
        else {
            continue;
        };

        let Expr::Name(base) = object.as_ref() else {
            continue;
        };

        if text(*base) != "table"
            || !matches!(text(*name), "insert" | "remove" | "sort" | "clear" | "move")
        {
            continue;
        }

        let CallArgs::Paren(list) = args else {
            continue;
        };

        if let Some(Expr::Name(name)) = list.first() {
            mark(Some(*name));
        }
    }

    out
}

/// The name an index chain starts from: `t` in `t.a.b[c]`.
fn root_name(e: &Expr) -> Option<TokSpan> {
    match e {
        Expr::Name(span) => Some(*span),

        Expr::Index { object, .. } => root_name(object),

        _ => None,
    }
}

/*
Returns the name that a `local X = require(...)` binds, when it is exactly
that.

The match is narrow on purpose, and it follows the `const_requires` transform
and the `non_const_require` lint: one name, one value, no type annotation. A
multi binding cannot be const one name at a time, and an annotated local
states something that the author cared about.
*/
pub(super) fn single_require_binding<'a>(
    src: &str,
    toks: &[Tok],
    local: &'a Local,
) -> Option<&'a Binding> {
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
pub(super) fn collect<'a>(chunk: &'a Chunk, out: &mut Vec<&'a Local>) {
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
