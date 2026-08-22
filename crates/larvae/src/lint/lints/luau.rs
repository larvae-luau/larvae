/*!
The lints that Luau's own compiler reports and larvae did not.

Luau ships twenty-eight lints with the compiler. A project that moves to
larvae keeps its formatter, its config and its editor, and it must not lose a
warning that the compiler used to give it. The lints here close that gap, so
that `larvae lint` is a replacement for `luau-analyze` and not a second
opinion beside it.

Each one keeps larvae's rule about false reports: where Luau can consult the
type checker and larvae cannot, the lint here reports the narrower case that
needs no types. `unknown_type` is the clearest example. `typeof(x) == "Vecor3"`
is a typo that only the Roblox API can confirm, so under the Roblox library
this lint speaks only about the lowercase names, which are the ones that the
language itself fixes.
*/

use crate::lint::config::StdLib;
use crate::lint::ctx::{Finding, LintCtx};
use crate::lint::globals;
use crate::lints;
use crate::syntax::ast::*;
use crate::syntax::lexer::TokKind;

use super::configured::global_path;
use super::correctness::{each_block, each_expr, each_stmt, number, unwrap_parens};

lints! {
    BadCommentDirective => "bad_comment_directive", Correctness, Warn,
        "a --! directive that Luau does not know, or one that comes after the code it should govern";
    BuiltinGlobalWrite => "builtin_global_write", Suspicious, Warn,
        "an assignment over a standard global, which every later script sees";
    ComparisonPrecedence => "comparison_precedence", Correctness, Warn,
        "not a == b, or a chain like a < b < c, which does not group the way it reads";
    DuplicateFunction => "duplicate_function", Correctness, Warn,
        "two functions of the same name in one scope, where the first is discarded";
    DuplicateLocal => "duplicate_local", Correctness, Deny,
        "one local statement or parameter list that declares the same name twice";
    FormatString => "format_string", Correctness, Deny,
        "a format string that string.format or os.date rejects at runtime";
    ImplicitReturn => "implicit_return", Suspicious, Allow,
        "a function that returns a value on one path and falls off the end on another";
    MisleadingAndOr => "misleading_and_or", Correctness, Warn,
        "cond and false or b, which always gives b because the middle is never truthy";
    NumberLiteralOverflow => "number_literal_overflow", Correctness, Warn,
        "a hexadecimal or binary literal wider than 64 bits, which is truncated";
    PlaceholderRead => "placeholder_read", Suspicious, Warn,
        "reading _, the name that says a value is discarded";
    TableOperations => "table_operations", Correctness, Warn,
        "a table.insert or table.remove whose index or argument count is wrong";
    ImplicitAnyLocal => "implicit_any_local", Suspicious, Warn,
        "a local declared with no value and no type, so what it holds is decided elsewhere";
    ImplicitAnyParameter => "implicit_any_parameter", Suspicious, Allow,
        "a parameter with no type, so what it takes is decided by the caller";
    UninitializedLocal => "uninitialized_local", Correctness, Warn,
        "a local declared with no value and never assigned, so every read is nil";
    UnknownType => "unknown_type", Correctness, Warn,
        "comparing type(x) against a string that type() never returns";
    ZeroStepLoop => "zero_step_loop", Correctness, Deny,
        "a numeric for whose step is zero, so the counter never moves";
}

// --- builtin_global_write --------------------------------------------------

impl BuiltinGlobalWrite {
    /*
    `string = nil`, or `function print() end`.

    The standard library is one table per process. An assignment over one of
    its names does not shadow it, it replaces it, and every module loaded
    after this line sees the replacement. The usual cause is a name that the
    author wanted for a local and wrote without `local`.

    This lint and `unscoped_variables` divide the same ground and never
    overlap: that one reports a write to a name the library does not define,
    and this one reports a write to a name it does.
    */
    fn check(ctx: &LintCtx<'_>, out: &mut Vec<Finding>) {
        for &token in &ctx.names.global_writes {
            let name = ctx.tok(token);

            if !globals::has(ctx.cfg.std, name) {
                continue;
            }

            let span = TokSpan::new(token as usize, token as usize + 1);

            out.push(
                Finding::new(
                    "builtin_global_write",
                    ctx.bytes(span),
                    format!("{name} is a standard global, and this replaces it"),
                )
                .with_help("every script loaded after this one sees the new value, use a local"),
            );
        }
    }
}

// --- placeholder_read ------------------------------------------------------

impl PlaceholderRead {
    /*
    `local _ = f()`, then a later `print(_)`.

    A bare `_` states that the value is thrown away. To read it later
    contradicts the statement, and the value that arrives is whatever the
    last discard happened to leave there. Either the read wants a real name,
    or the discard was never a discard.

    The lint speaks only about a `_` that this file binds. A `_` that
    nothing binds is a global, and `undefined_variable` already reports it.
    */
    fn check(ctx: &LintCtx<'_>, out: &mut Vec<Finding>) {
        each_expr(ctx, out, |ctx, e, out| {
            let Expr::Name(span) = e else {
                return;
            };

            if ctx.text(*span) != "_" || !ctx.names.read_of.contains_key(&span.start) {
                return;
            }

            out.push(
                Finding::new(
                    "placeholder_read",
                    ctx.bytes(*span),
                    "_ is the name for a value that is thrown away, and this reads it",
                )
                .with_help("give the value a name, or this read means nothing"),
            );
        });
    }
}

// --- unknown_type ----------------------------------------------------------

/// Every string that `type()` can return.
const TYPE_NAMES: &[&str] = &[
    "boolean", "buffer", "function", "nil", "number", "string", "table", "thread", "userdata",
    "vector",
];

impl UnknownType {
    /*
    `type(x) == "numbr"`.

    The comparison is false for every value, so the branch behind it is
    dead. Nothing at runtime says so, because a string compares against a
    string without complaint.

    `typeof` under Roblox returns a class name as well, and larvae does not
    have the class list. Thus there the lint reports only a name that starts
    lowercase. Every Roblox data type is capitalised, so a lowercase name is
    one of the ten that the language defines, misspelled. Under plain Luau
    `typeof` and `type` return the same ten names, and the lint checks the
    full set.
    */
    fn check(ctx: &LintCtx<'_>, out: &mut Vec<Finding>) {
        each_expr(ctx, out, |ctx, e, out| {
            let Expr::Binary { op, lhs, rhs, .. } = e else {
                return;
            };

            if !matches!(ctx.text(*op), "==" | "~=") {
                return;
            }

            for (call, literal) in [(lhs, rhs), (rhs, lhs)] {
                let Some(name) = type_call(ctx, unwrap_parens(call)) else {
                    continue;
                };

                let Some((text, _)) = string_content(ctx, unwrap_parens(literal)) else {
                    continue;
                };

                // An escape hides the real content from a plain comparison.
                if text.contains('\\') || TYPE_NAMES.contains(&text) {
                    continue;
                }

                /*
                Under Roblox, `typeof` also returns the name of a data type,
                and those all start with a capital. larvae does not ship the
                list, so it says nothing about a capitalised name.
                */
                if name == "typeof"
                    && ctx.cfg.std == StdLib::Roblox
                    && !text.starts_with(|c: char| c.is_ascii_lowercase())
                {
                    continue;
                }

                out.push(
                    Finding::new(
                        "unknown_type",
                        ctx.bytes(literal.span()),
                        format!("{name}() never returns \"{text}\", so this is always false"),
                    )
                    .with_help(format!(
                        "the names it returns are {}",
                        TYPE_NAMES.join(", ")
                    )),
                );
            }
        });
    }
}

/// `type(x)` or `typeof(x)`, where the name is the global one.
fn type_call<'a>(ctx: &LintCtx<'a>, e: &Expr) -> Option<&'a str> {
    let Expr::Call { func, method, .. } = e else {
        return None;
    };

    if method.is_some() {
        return None;
    }

    let Expr::Name(span) = func.as_ref() else {
        return None;
    };

    let name = ctx.text(*span);

    (matches!(name, "type" | "typeof") && ctx.names.is_global(span.start)).then_some(name)
}

// --- implicit_return -------------------------------------------------------

impl ImplicitReturn {
    /*
    A function with `return x` on one path and no return on another.

    The path that falls off the end returns nothing, and the caller reads
    nil. When the function is a lookup, that is the design. When it is not,
    the author forgot a branch, and the nil arrives far from here.

    The lint is `allow`, because the first reading is common and
    idiomatic. `local function find(t, x) for i, v in t do if v == x then
    return i end end end` is correct Luau, and this lint reports it. A
    project that wants every exit spelled out turns the lint on and writes
    the trailing `return nil`.
    */
    fn check(ctx: &LintCtx<'_>, out: &mut Vec<Finding>) {
        each_function(ctx, |body| {
            if !returns_a_value(&body.block) || always_exits(ctx, &body.block) {
                return;
            }

            let end = TokSpan::new(body.span.end as usize - 1, body.span.end as usize);

            out.push(
                Finding::new(
                    "implicit_return",
                    ctx.bytes(end),
                    "this function returns a value elsewhere, and this path returns nothing",
                )
                .with_help("add an explicit return, nil included, to say which one is meant"),
            );
        });
    }
}

/// Returns true if some path of this body returns a value.
fn returns_a_value(block: &Block) -> bool {
    block.stmts.iter().any(|s| match s {
        Stmt::Return(n) => !n.values.is_empty(),

        Stmt::If(n) => {
            n.branches.iter().any(|(_, b)| returns_a_value(b))
                || n.else_block.as_ref().is_some_and(returns_a_value)
        }

        Stmt::Do(n) => returns_a_value(&n.block),

        Stmt::While(n) => returns_a_value(&n.block),

        Stmt::Repeat(n) => returns_a_value(&n.block),

        Stmt::NumericFor(n) => returns_a_value(&n.block),

        Stmt::GenericFor(n) => returns_a_value(&n.block),

        // A nested function returns to its own caller, not to this one.
        _ => false,
    })
}

/// Returns true if control cannot reach the end of this block.
fn always_exits(ctx: &LintCtx<'_>, block: &Block) -> bool {
    let Some(last) = block
        .stmts
        .iter()
        .rev()
        .find(|s| !matches!(s, Stmt::Empty(_)))
    else {
        return false;
    };

    match last {
        Stmt::Return(_) | Stmt::Break(_) | Stmt::Continue(_) => true,

        // `error(...)` does not come back, so the line after it is not a path.
        Stmt::Call(e, _) => matches!(e, Expr::Call { func, method: None, .. }
            if global_path(ctx, func).as_deref() == Some("error")),

        Stmt::Do(n) => always_exits(ctx, &n.block),

        // Only an if with an else covers every path.
        Stmt::If(n) => {
            n.else_block.as_ref().is_some_and(|b| always_exits(ctx, b))
                && n.branches.iter().all(|(_, b)| always_exits(ctx, b))
        }

        Stmt::While(n) => matches!(&n.cond, Expr::True(_)) && !has_break(&n.block),

        _ => false,
    }
}

/// Returns true if a `break` in this block leaves the loop that holds it.
fn has_break(block: &Block) -> bool {
    block.stmts.iter().any(|s| match s {
        Stmt::Break(_) => true,

        Stmt::If(n) => {
            n.branches.iter().any(|(_, b)| has_break(b))
                || n.else_block.as_ref().is_some_and(has_break)
        }

        Stmt::Do(n) => has_break(&n.block),

        // A break inside a nested loop belongs to that loop.
        _ => false,
    })
}

// --- duplicate_local -------------------------------------------------------

impl DuplicateLocal {
    /*
    `local x, x = 1, 2`, or `function f(a, a)`.

    Only the last one is reachable, so the first value is lost at the moment
    it arrives. A parameter list is the worse of the two, because the caller
    passes both and the function reads the second.

    `_` is exempt in both places. `local _, _ = f()` and `function f(_, _)`
    say that two values are discarded, and that is the point of the name.
    */
    fn check(ctx: &LintCtx<'_>, out: &mut Vec<Finding>) {
        each_stmt(ctx, out, |ctx, s, out| {
            let Stmt::Local(n) = s else {
                return;
            };

            report_dupes(
                ctx,
                n.names.iter().map(|b| b.name),
                "is declared twice by this local",
                out,
            );
        });

        each_function(ctx, |body| {
            report_dupes(
                ctx,
                body.params.iter().filter(|p| !p.is_vararg).map(|p| p.name),
                "is already a parameter of this function",
                out,
            );
        });
    }
}

fn report_dupes(
    ctx: &LintCtx<'_>,
    names: impl Iterator<Item = TokSpan>,
    what: &str,
    out: &mut Vec<Finding>,
) {
    let mut seen: Vec<&str> = Vec::new();

    for span in names {
        let name = ctx.tok(span.start);

        // The discard name says nothing about identity, so it may repeat.
        if name == "_" {
            continue;
        }

        if seen.contains(&name) {
            out.push(
                Finding::new(
                    "duplicate_local",
                    ctx.bytes(TokSpan::new(span.start as usize, span.start as usize + 1)),
                    format!("{name} {what}"),
                )
                .with_help("only the last one can be read, rename or remove one"),
            );
        } else {
            seen.push(name);
        }
    }
}

// --- uninitialized_local ---------------------------------------------------

impl UninitializedLocal {
    /*
    `local total` with no value, read but never assigned.

    The declaration is the shape that an author writes before a loop fills
    the name in. When nothing ever fills it in, every read is nil, and the
    first arithmetic or index on it throws.

    A name that something assigns later is not this lint's business, whether
    the assignment is in this block or inside a closure. The scope walk sees
    both.
    */
    fn check(ctx: &LintCtx<'_>, out: &mut Vec<Finding>) {
        each_stmt(ctx, out, |ctx, s, out| {
            let Stmt::Local(n) = s else {
                return;
            };

            if !n.values.is_empty() {
                return;
            }

            for binding in &n.names {
                let name = ctx.tok(binding.name.start);

                // An underscore name states that nobody wants the value.
                if name.starts_with('_') {
                    continue;
                }

                let unassigned = ctx
                    .names
                    .by_token
                    .get(&binding.name.start)
                    .and_then(|&i| ctx.names.bindings.get(i))
                    .is_some_and(|b| b.writes.is_empty() && !b.reads.is_empty());

                if !unassigned {
                    continue;
                }

                out.push(
                    Finding::new(
                        "uninitialized_local",
                        ctx.bytes(TokSpan::new(
                            binding.name.start as usize,
                            binding.name.start as usize + 1,
                        )),
                        format!("{name} is never assigned, so every read of it is nil"),
                    )
                    .with_help(format!("write local {name} = nil if nil is what is meant")),
                );
            }
        });
    }
}

// --- duplicate_function ----------------------------------------------------

impl DuplicateFunction {
    /*
    Two `function f()` definitions in one block.

    The second one replaces the first, so the first body never runs. The
    cause is a copied block whose name nobody changed, or a merge that kept
    both sides.

    The lint compares within one block, which is one scope. Two definitions
    of the same name in two branches of an `if` are the intended shape and
    are not reported.
    */
    fn check(ctx: &LintCtx<'_>, out: &mut Vec<Finding>) {
        each_block(ctx, out, |ctx, block, out| {
            let mut seen: Vec<(String, u32)> = Vec::new();

            for stmt in &block.stmts {
                let Some((name, span)) = defined_function(ctx, stmt) else {
                    continue;
                };

                if let Some((_, first)) = seen.iter().find(|(n, _)| *n == name) {
                    out.push(
                        Finding::new(
                            "duplicate_function",
                            ctx.bytes(span),
                            format!("{name} is defined twice in this scope"),
                        )
                        .with_help(format!(
                            "the definition on line {} never runs",
                            ctx.line(*first) + 1
                        )),
                    );
                } else {
                    seen.push((name, ctx.bytes(span).0));
                }
            }
        });
    }
}

/// The name that a function statement defines, and the span to point at.
fn defined_function(ctx: &LintCtx<'_>, stmt: &Stmt) -> Option<(String, TokSpan)> {
    match stmt {
        Stmt::LocalFunction(n) => Some((ctx.text(n.name).to_string(), n.name)),

        /*
        The path carries the method name too, so `function t:m()` and
        `function t.m()` give the same key. They do define the same field,
        and the second one wins.
        */
        Stmt::Function(n) => {
            let (first, last) = (n.path.first()?, n.path.last()?);
            let name = n
                .path
                .iter()
                .map(|p| ctx.text(*p))
                .collect::<Vec<_>>()
                .join(".");

            Some((name, TokSpan::new(first.start as usize, last.end as usize)))
        }

        _ => None,
    }
}

// --- table_operations ------------------------------------------------------

impl TableOperations {
    /*
    `table.insert(t, #t + 1, v)`, `table.insert(t, 0, v)`, `table.insert(t)`.

    The first one names the position that a two argument insert already
    uses, and pays for a shift that moves nothing. The second one writes to
    index zero, which no length and no `ipairs` will ever see, because a Lua
    array starts at one. The third one cannot run at all.

    The table must be the global one. A local named `table` belongs to
    somebody else, and its `insert` takes whatever it takes.
    */
    fn check(ctx: &LintCtx<'_>, out: &mut Vec<Finding>) {
        each_expr(ctx, out, |ctx, e, out| {
            let Expr::Call {
                func,
                method: None,
                args: CallArgs::Paren(args),
                span,
                ..
            } = e
            else {
                return;
            };

            let path = global_path(ctx, func);
            let inserting = match path.as_deref() {
                Some("table.insert") => true,

                Some("table.remove") => false,

                _ => return,
            };

            let name = if inserting {
                "table.insert"
            } else {
                "table.remove"
            };

            /*
            A call in the last position expands to any number of values, so
            the written count says nothing about the real count.
            */
            let spreads = matches!(args.last(), Some(Expr::Call { .. } | Expr::Vararg(_)));
            let wanted: &[usize] = if inserting { &[2, 3] } else { &[1, 2] };

            if !spreads && !wanted.contains(&args.len()) {
                out.push(
                    Finding::new(
                        "table_operations",
                        ctx.bytes(*span),
                        format!(
                            "{name} takes {} arguments, not {}",
                            either(wanted),
                            args.len()
                        ),
                    )
                    .with_help("the table comes first, then the optional position"),
                );

                return;
            }

            // The index argument, when the call passes one.
            let index = match (inserting, args.len()) {
                (true, 3) => &args[1],

                (false, 2) => &args[1],

                _ => return,
            };

            if number(ctx, index) == Some(0.0) {
                out.push(
                    Finding::new(
                        "table_operations",
                        ctx.bytes(index.span()),
                        format!("{name} is given index 0, and a Lua array starts at 1"),
                    )
                    .with_help("write 1 for the first element"),
                );

                return;
            }

            if inserting && is_append_index(ctx, index, &args[0]) {
                out.push(
                    Finding::new(
                        "table_operations",
                        ctx.bytes(index.span()),
                        "this is the position that table.insert appends to anyway",
                    )
                    .with_help("drop the index, table.insert(t, v) is the same and is faster"),
                );
            }
        });
    }
}

fn either(counts: &[usize]) -> String {
    counts
        .iter()
        .map(usize::to_string)
        .collect::<Vec<_>>()
        .join(" or ")
}

/// `#t + 1`, where `t` is the table that the call inserts into.
fn is_append_index(ctx: &LintCtx<'_>, index: &Expr, table: &Expr) -> bool {
    let Expr::Binary { op, lhs, rhs, .. } = unwrap_parens(index) else {
        return false;
    };

    if ctx.text(*op) != "+" {
        return false;
    }

    let one = |e: &Expr| number(ctx, e) == Some(1.0);
    let length = |e: &Expr| {
        matches!(unwrap_parens(e), Expr::Unary { op, operand, .. }
            if ctx.text(*op) == "#" && ctx.same_text(operand.span(), table.span()))
    };

    (length(lhs) && one(rhs)) || (one(lhs) && length(rhs))
}

// --- misleading_and_or -----------------------------------------------------

/*
Reports whether this expression can only be `true` or `false`.

Syntax answers this on its own, which is the whole reason the lint can widen
without types. A comparison yields a boolean whatever its operands are, and
`not` yields one as well. `and` and `or` do not: they give back an operand,
so `a and b` is whatever `b` holds.
*/
fn always_boolean(ctx: &LintCtx<'_>, e: &Expr) -> bool {
    match unwrap_parens(e) {
        Expr::Binary { op, .. } => matches!(ctx.text(*op), "==" | "~=" | "<" | "<=" | ">" | ">="),

        Expr::Unary { op, .. } => ctx.text(*op) == "not",

        _ => false,
    }
}

impl MisleadingAndOr {
    /*
    `cond and false or other`, and the wider case that hides behind it.

    `a and b or c` stands in for a conditional, and it works only while `b`
    is truthy. With `false` or `nil` in the middle, the `and` gives that
    value, the `or` sees it as false, and the whole expression is `c` for
    every input. The author wanted `if cond then false else other`, which
    Luau writes as an expression.

    A middle that is provably a boolean is the same defect with a smaller
    blast radius, and it is the one that reaches production. `ready and
    (count == 0) or "pending"` gives "pending" when the count is not zero,
    which is exactly when the author wanted `false`. Nothing about that
    needs a type: a comparison yields a boolean because it is a comparison.

    The two cases carry different messages, because the first is wrong for
    every input and the second is wrong for half of them. A reader who sees
    "always" for a case that is sometimes right stops trusting the linter.
    */
    fn check(ctx: &LintCtx<'_>, out: &mut Vec<Finding>) {
        each_expr(ctx, out, |ctx, e, out| {
            let Expr::Binary { op, lhs, span, .. } = e else {
                return;
            };

            if ctx.text(*op) != "or" {
                return;
            }

            let Expr::Binary {
                op: and,
                rhs: middle,
                ..
            } = lhs.as_ref()
            else {
                return;
            };

            if ctx.text(*and) != "and" {
                return;
            }

            let message = match unwrap_parens(middle.as_ref()) {
                Expr::False(_) => {
                    "the middle of this and-or is false, so the result is always the last part"
                        .to_string()
                }

                Expr::Nil(_) => {
                    "the middle of this and-or is nil, so the result is always the last part"
                        .to_string()
                }

                other if always_boolean(ctx, other) => format!(
                    "the middle of this and-or is the boolean `{}`, so the result is the last part \
                     whenever that is false",
                    ctx.text(other.span())
                ),

                _ => return,
            };

            out.push(
                Finding::new("misleading_and_or", ctx.bytes(*span), message)
                    .with_help("write if cond then a else b, which is an expression in Luau"),
            );
        });
    }
}

// --- bad_comment_directive -------------------------------------------------

/// The directives that Luau reads.
const DIRECTIVES: &[&str] = &[
    "native",
    "nocheck",
    "nolint",
    "nonstrict",
    "optimize",
    "strict",
];

impl BadCommentDirective {
    /*
    `--!strct`, or a `--!strict` below the first line of code.

    A directive is a comment, so nothing rejects a misspelling. The file
    keeps the default mode, and the author believes it is strict. The same
    silence covers a directive that arrives too late: Luau reads them only
    in the header, and one below the first token does nothing.
    */
    fn check(ctx: &LintCtx<'_>, out: &mut Vec<Finding>) {
        let code_starts = ctx.toks.first().map(|t| t.start);

        for &(start, end) in ctx.comments {
            let text = &ctx.src[start as usize..end as usize];

            let Some(rest) = text.strip_prefix("--!") else {
                continue;
            };

            let word = rest.split_whitespace().next().unwrap_or("");

            if word.is_empty() {
                continue;
            }

            if !DIRECTIVES.contains(&word) {
                out.push(
                    Finding::new(
                        "bad_comment_directive",
                        (start, end),
                        format!("{word} is not a directive that Luau knows"),
                    )
                    .with_help(format!("the directives are {}", DIRECTIVES.join(", "))),
                );

                continue;
            }

            if code_starts.is_some_and(|at| start > at) {
                out.push(
                    Finding::new(
                        "bad_comment_directive",
                        (start, end),
                        format!("this {word} directive comes after the code, so it does nothing"),
                    )
                    .with_help("move it above the first line of code"),
                );
            }
        }
    }
}

// --- number_literal_overflow -----------------------------------------------

impl NumberLiteralOverflow {
    /*
    `0x1FFFFFFFFFFFFFFFF`, a literal of more than sixty-four bits.

    Luau parses a hexadecimal or binary literal into a 64 bit integer and
    then converts it to a number. A literal that does not fit wraps to
    2^64, and the value in the file is not the value in the program. A mask
    that gained one digit is the usual way to arrive here.

    The lint counts digits and does not parse. Sixteen hexadecimal digits
    are sixty-four bits exactly, so seventeen is the first that cannot fit.
    */
    fn check(ctx: &LintCtx<'_>, out: &mut Vec<Finding>) {
        for (i, tok) in ctx.toks.iter().enumerate() {
            if tok.kind != TokKind::Number {
                continue;
            }

            let text = tok.text(ctx.src);
            let lower = text.to_ascii_lowercase();

            let (digits, base, limit) = match () {
                _ if lower.starts_with("0x") => (&text[2..], "hexadecimal", 16),

                _ if lower.starts_with("0b") => (&text[2..], "binary", 64),

                _ => continue,
            };

            // A separator is spacing, and a leading zero carries no value.
            let significant = digits
                .trim_start_matches(['_', '0'])
                .chars()
                .filter(|&c| c != '_')
                .count();

            if significant <= limit {
                continue;
            }

            let span = TokSpan::new(i, i + 1);

            out.push(
                Finding::new(
                    "number_literal_overflow",
                    ctx.bytes(span),
                    format!("this {base} literal needs more than 64 bits"),
                )
                .with_help("it wraps to 2^64, so the value here is not the value at runtime"),
            );
        }
    }
}

// --- comparison_precedence -------------------------------------------------

/// The operators that this lint watches, all of the same precedence.
const COMPARISONS: &[&str] = &["==", "~=", "<", "<=", ">", ">="];

impl ComparisonPrecedence {
    /*
    `not a == b`, and `a < b < c`.

    `not` binds tighter than a comparison, so the first is `(not a) == b`,
    which compares a boolean against `b`. A chain is left associative, so
    the second is `(a < b) < c`, which compares a boolean against `c`. Both
    read as the mathematics they resemble and mean something else.

    A parenthesis on the left says that the author meant the grouping, and
    the lint stays quiet there.
    */
    fn check(ctx: &LintCtx<'_>, out: &mut Vec<Finding>) {
        each_expr(ctx, out, |ctx, e, out| {
            let Expr::Binary { op, lhs, span, .. } = e else {
                return;
            };

            let operator = ctx.text(*op);

            if !COMPARISONS.contains(&operator) {
                return;
            }

            match lhs.as_ref() {
                Expr::Unary {
                    op: not, operand, ..
                } if ctx.text(*not) == "not" => {
                    let advice = match operator {
                        "==" => "write x ~= y, or add parentheses to keep this meaning",

                        "~=" => "write x == y, or add parentheses to keep this meaning",

                        _ => "add parentheses to say which grouping is meant",
                    };

                    out.push(
                        Finding::new(
                            "comparison_precedence",
                            ctx.bytes(*span),
                            format!(
                                "not binds tighter than {operator}, so this is (not {}) {operator} ...",
                                ctx.text(operand.span())
                            ),
                        )
                        .with_help(advice),
                    );
                }

                Expr::Binary { op: inner, .. } if COMPARISONS.contains(&ctx.text(*inner)) => {
                    out.push(
                        Finding::new(
                            "comparison_precedence",
                            ctx.bytes(*span),
                            format!(
                                "this chains {} and {operator}, so the second compares a boolean",
                                ctx.text(*inner)
                            ),
                        )
                        .with_help("write a and b for the two comparisons, or add parentheses"),
                    );
                }

                _ => {}
            }
        });
    }
}

// --- zero_step_loop --------------------------------------------------------

impl ZeroStepLoop {
    /*
    `for i = 1, 10, 0 do`.

    The counter never reaches the limit, because nothing moves it. The loop
    is not a slow loop, it is a loop that does not end, and a step written
    as a literal zero is never what an author wanted.
    */
    fn check(ctx: &LintCtx<'_>, out: &mut Vec<Finding>) {
        each_stmt(ctx, out, |ctx, s, out| {
            let Stmt::NumericFor(n) = s else {
                return;
            };

            let Some(step) = &n.step else {
                return;
            };

            if number(ctx, step) != Some(0.0) {
                return;
            }

            out.push(
                Finding::new(
                    "zero_step_loop",
                    ctx.bytes(step.span()),
                    "the step is zero, so the counter never moves",
                )
                .with_help("use 1 to count up, or -1 to count down"),
            );
        });
    }
}

// --- format_string ---------------------------------------------------------

impl FormatString {
    /*
    `string.format("%y", x)`, or `os.date("%Q")`.

    Both functions parse their first argument at runtime and raise on a
    specifier they do not know. The string is a literal here, so the whole
    check can happen now instead. `string.format("100%")` is the case that
    surprises people: the trailing percent starts a specifier that the
    string never finishes, and the call raises.

    The lint reads only a literal. A format built from a variable is the
    caller's business, and larvae does not guess about it.
    */
    fn check(ctx: &LintCtx<'_>, out: &mut Vec<Finding>) {
        each_expr(ctx, out, |ctx, e, out| {
            let Expr::Call {
                func, method, args, ..
            } = e
            else {
                return;
            };

            // `("%d"):format(n)` is the same call written the other way round.
            let (literal, date) = match method {
                Some(m) if ctx.text(*m) == "format" => (unwrap_parens(func), false),

                Some(_) => return,

                None => {
                    let date = match global_path(ctx, func).as_deref() {
                        Some("string.format") => false,

                        Some("os.date") => true,

                        _ => return,
                    };

                    let CallArgs::Paren(list) = args else {
                        return;
                    };

                    match list.first() {
                        Some(first) => (unwrap_parens(first), date),

                        None => return,
                    }
                }
            };

            let Some((text, at)) = string_content(ctx, literal) else {
                return;
            };

            let problems = if date {
                bad_date_format(text)
            } else {
                bad_format_specifiers(text)
            };

            for (offset, len, message) in problems {
                out.push(
                    Finding::new(
                        "format_string",
                        (at + offset as u32, at + (offset + len) as u32),
                        message,
                    )
                    .with_help(if date {
                        "os.date takes the C89 date specifiers, or \"*t\" for a table"
                    } else {
                        "the conversions are c d i u o x X e E f g G q s, and %% for a percent"
                    }),
                );
            }
        });
    }
}

/// Every conversion that `string.format` accepts, `%` included.
const CONVERSIONS: &str = "cdiouxXeEfgGqs%";

/// The characters that may sit between the `%` and the conversion.
const MODIFIERS: &str = "-+ #0*";

/// Every specifier that `os.date` accepts, which is the C89 set.
const DATE_SPECIFIERS: &str = "aAbBcdHIjmMpSUwWxXyYzZ%";

/*
The specifiers in a `string.format` string that the runtime would reject.

Each entry is an offset into the string, a length, and the message. The scan
walks bytes and reports nothing for a byte outside ASCII: a multi-byte
character after a `%` is not a mistake that anybody makes, and a span that
cut one in half would be worse than silence.
*/
fn bad_format_specifiers(s: &str) -> Vec<(usize, usize, String)> {
    let bytes = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] != b'%' {
            i += 1;

            continue;
        }

        let start = i;
        i += 1;

        while i < bytes.len() && MODIFIERS.contains(bytes[i] as char) {
            i += 1;
        }

        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }

        if i < bytes.len() && bytes[i] == b'.' {
            i += 1;

            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
        }

        match bytes.get(i) {
            None => out.push((
                start,
                s.len() - start,
                "this format specifier is never finished".to_string(),
            )),

            Some(&c) if CONVERSIONS.contains(c as char) => i += 1,

            Some(&c) if c.is_ascii() => {
                out.push((
                    start,
                    i + 1 - start,
                    format!("%{} is not a format specifier", c as char),
                ));

                i += 1;
            }

            // A byte outside ASCII would leave a span that splits a character.
            Some(_) => i += 1,
        }
    }

    out
}

/// The same for `os.date`, whose set of specifiers is a different one.
fn bad_date_format(s: &str) -> Vec<(usize, usize, String)> {
    // A leading `!` asks for UTC, and `*t` asks for a table instead of text.
    let base = usize::from(s.starts_with('!'));

    if s[base..].starts_with("*t") {
        return Vec::new();
    }

    let bytes = s.as_bytes();
    let mut out = Vec::new();
    let mut i = base;

    while i < bytes.len() {
        if bytes[i] != b'%' {
            i += 1;

            continue;
        }

        match bytes.get(i + 1) {
            None => out.push((i, 1, "this date specifier is never finished".to_string())),

            Some(&c) if DATE_SPECIFIERS.contains(c as char) => {}

            Some(&c) if c.is_ascii() => out.push((
                i,
                2,
                format!("%{} is not a date specifier that Luau accepts", c as char),
            )),

            Some(_) => {}
        }

        i += 2;
    }

    out
}

// --- shared ----------------------------------------------------------------

/// The text inside a string literal, and the byte where that text starts.
fn string_content<'a>(ctx: &LintCtx<'a>, e: &Expr) -> Option<(&'a str, u32)> {
    let Expr::String(span) = e else {
        return None;
    };

    let TokKind::Str {
        inner_start,
        inner_end,
    } = ctx.toks[span.start as usize].kind
    else {
        return None;
    };

    Some((
        &ctx.src[inner_start as usize..inner_end as usize],
        inner_start,
    ))
}

/// Every function body in the file, whatever form declared it.
fn each_function(ctx: &LintCtx<'_>, mut f: impl FnMut(&FunctionBody)) {
    for stmt in &ctx.stmts {
        match stmt {
            Stmt::Function(n) => f(&n.body),

            Stmt::LocalFunction(n) => f(&n.body),

            _ => {}
        }
    }

    for e in &ctx.exprs {
        if let Expr::Function { body, .. } = e {
            f(body);
        }
    }
}

// --- implicit_any_local ----------------------------------------------------

impl ImplicitAnyLocal {
    /*
    `local test`, with no value and no type annotation.

    What the name holds is then decided by whatever assigns it first, and a
    reader of the declaration cannot tell what that is. In a file with no
    `--!strict` directive, which is most Roblox code, Luau accepts any later
    assignment of any type: checked against luau-lsp, `local x` then `x = 1`
    then `x = "s"` is an error under `--!strict` and silence without it. That
    silence is the implicit `any` this lint names.

    The fix is one of two words. Write the type, `local test: number`, when
    the value arrives later. Write the value, `local test = 0`, when it can
    arrive now.

    The lint warns, and it does not deny. The fix is always available and it
    never changes behaviour, which argued for a deny at first. The shape
    argues against one: `local found` above the loop that fills it in is
    ordinary Luau that runs correctly, Luau's own linter says nothing about
    it, and a deny fails the build of every project on the day it adopts
    larvae. A project that wants the discipline as a gate writes one line.

    A local that nothing ever assigns is left to `uninitialized_local`, which
    says the more urgent thing about the same line: every read of it is nil.
    One line gets one finding.
    */
    fn check(ctx: &LintCtx<'_>, out: &mut Vec<Finding>) {
        each_stmt(ctx, out, |ctx, s, out| {
            let Stmt::Local(n) = s else {
                return;
            };

            // A declaration with a value has its type from that value.
            if !n.values.is_empty() {
                return;
            }

            for binding in &n.names {
                // The author wrote what it holds, which is the whole ask.
                if binding.ty.is_some() {
                    continue;
                }

                let name = ctx.tok(binding.name.start);

                // An underscore name states that nobody wants the value.
                if name.starts_with('_') {
                    continue;
                }

                /*
                Nothing assigns it, so `uninitialized_local` reports the line
                and says the more urgent thing about it.
                */
                let never_assigned = ctx
                    .names
                    .by_token
                    .get(&binding.name.start)
                    .and_then(|&i| ctx.names.bindings.get(i))
                    .is_some_and(|b| b.writes.is_empty());

                if never_assigned {
                    continue;
                }

                out.push(
                    Finding::new(
                        "implicit_any_local",
                        ctx.bytes(TokSpan::new(
                            binding.name.start as usize,
                            binding.name.start as usize + 1,
                        )),
                        format!(
                            "{name} has no value and no type, so what it holds is decided elsewhere"
                        ),
                    )
                    .with_help(format!(
                        "write the type, `local {name}: T`, or give it a value"
                    )),
                );
            }
        });
    }
}

// --- implicit_any_parameter ------------------------------------------------

impl ImplicitAnyParameter {
    /*
    `local function apply(list, transform)`.

    A parameter with no annotation is `any`. What the function takes is then
    decided by each caller, and a reader of the signature cannot tell what
    the body expects. This is the defect that `implicit_any_local` names, on
    the other side of the call: that lint asks what a name holds, and this
    one asks what a function accepts.

    The lint is off by default. Most Luau carries no annotations, so a warn
    default would report hundreds of times on the first run, and a report
    that large teaches users to stop reading the linter. A project that
    wants annotated signatures asks for them in one line.

    Two names are left out. A name that starts with `_` says that nobody
    wants the value, which is what `unused_variable` exempts through its
    `ignore_pattern`. `self` is the receiver of a method, and Luau gives it
    the type of the table that the method hangs on.
    */
    fn check(ctx: &LintCtx<'_>, out: &mut Vec<Finding>) {
        each_function(ctx, |body| {
            for param in &body.params {
                // `...` is not a name, and its annotation is a separate question.
                if param.is_vararg || param.ty.is_some() {
                    continue;
                }

                let name = ctx.tok(param.name.start);

                if name == "self" || name.starts_with('_') {
                    continue;
                }

                out.push(
                    Finding::new(
                        "implicit_any_parameter",
                        ctx.bytes(param.name),
                        format!("{name} has no type, so what it takes is decided by the caller"),
                    )
                    .with_help(format!("write the type, `{name}: T`")),
                );
            }
        });
    }
}
