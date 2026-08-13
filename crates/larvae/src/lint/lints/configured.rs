/*!
Lints that a project tunes, and lints that need a table of known functions.

selene keeps the tables in its standard library files. Thus a project that
wants `must_use` on its own function writes a library. larvae ships the
tables here, and a project adds to them under `[lint.options]`. That is a
smaller thing to learn, and it covers what users need in practice.
*/

use serde::Deserialize;

use crate::lint::ctx::{Finding, LintCtx};
use crate::lints;
use crate::syntax::ast::*;

use super::correctness::{each_block, each_expr, each_stmt};

lints! {
    BadStringEscape => "bad_string_escape", Warn,
        "an escape sequence the language does not define";
    Deprecated => "deprecated", Warn,
        "a function that still works but has been replaced";
    HighCyclomaticComplexity => "high_cyclomatic_complexity", Allow,
        "a function with more branches than anyone can hold at once";
    ManualTableClone => "manual_table_clone", Warn,
        "a loop that copies a table, which table.clone does in one call";
    MismatchedArgCount => "mismatched_arg_count", Warn,
        "calling a function in this file with the wrong number of arguments";
    MustUse => "must_use", Warn,
        "calling a pure function and discarding what it returned";
    RestrictedModulePaths => "restricted_module_paths", Warn,
        "requiring a module the project has ruled out";
}

// --- bad_string_escape -----------------------------------------------------

impl BadStringEscape {
    /*
    `"\q"`.

    Luau defines a fixed set of escapes and rejects the rest. Thus an
    undefined escape is a syntax error, not a literal backslash. The usual
    cause is a Windows path whose separators are not doubled.
    */
    fn check(ctx: &LintCtx<'_>, out: &mut Vec<Finding>) {
        for (i, tok) in ctx.toks.iter().enumerate() {
            let crate::syntax::lexer::TokKind::Str {
                inner_start,
                inner_end,
            } = tok.kind
            else {
                continue;
            };

            // A long string has no escapes. Its content is literal.
            if ctx.tok(i as u32).starts_with('[') {
                continue;
            }

            let inner = &ctx.src[inner_start as usize..inner_end as usize];

            for (offset, escape) in bad_escapes(inner) {
                let at = inner_start + offset;

                out.push(
                    Finding::new(
                        "bad_string_escape",
                        (at, at + escape.len() as u32),
                        format!("\\{escape} is not an escape sequence"),
                    )
                    .with_help("write \\\\ for a literal backslash"),
                );
            }
        }
    }
}

/// Returns every undefined escape in this string's content, with its start offset.
fn bad_escapes(inner: &str) -> Vec<(u32, String)> {
    let bytes = inner.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] != b'\\' {
            i += 1;

            continue;
        }

        let Some(&next) = bytes.get(i + 1) else {
            break;
        };

        match next {
            // The simple escapes. A backslash before a newline continues the line.
            b'a' | b'b' | b'f' | b'n' | b'r' | b't' | b'v' | b'\\' | b'"' | b'\'' | b'\n'
            | b'\r' => i += 2,

            // `\z` skips the whitespace that follows it.
            b'z' => i += 2,

            // `\xFF`, with exactly two hex digits.
            b'x' => {
                let ok = bytes
                    .get(i + 2..i + 4)
                    .is_some_and(|d| d.iter().all(u8::is_ascii_hexdigit));

                if !ok {
                    out.push((i as u32, "x".to_string()));
                }

                i += 4.min(bytes.len() - i);
            }

            // `\u{1F600}`
            b'u' => {
                let ok = bytes.get(i + 2) == Some(&b'{')
                    && inner[i + 3..].split_once('}').is_some_and(|(d, _)| {
                        !d.is_empty() && d.bytes().all(|b| b.is_ascii_hexdigit())
                    });

                if !ok {
                    out.push((i as u32, "u".to_string()));
                }

                i += 2;
            }

            // `\ddd`, with one to three decimal digits.
            b'0'..=b'9' => {
                i += 2;

                let mut digits = 1;

                while digits < 3 && bytes.get(i).is_some_and(u8::is_ascii_digit) {
                    i += 1;
                    digits += 1;
                }
            }

            other => {
                out.push((i as u32, (other as char).to_string()));
                i += 2;
            }
        }
    }

    out
}

// --- must_use --------------------------------------------------------------

/*
Functions whose only effect is their return value.

A call to one of these as a statement computes a value and discards it, and
no author means that. The list is short by design. Every entry is pure by
definition, so there is no doubt about if a call site relied on a side
effect.
*/
const PURE: &[&str] = &[
    "math.abs",
    "math.ceil",
    "math.clamp",
    "math.floor",
    "math.max",
    "math.min",
    "math.round",
    "math.sqrt",
    "os.date",
    "os.time",
    "rawequal",
    "rawget",
    "rawlen",
    "select",
    "string.byte",
    "string.char",
    "string.find",
    "string.format",
    "string.gsub",
    "string.len",
    "string.lower",
    "string.match",
    "string.rep",
    "string.reverse",
    "string.split",
    "string.sub",
    "string.upper",
    "table.concat",
    "table.find",
    "table.pack",
    "tonumber",
    "tostring",
    "type",
    "typeof",
    "utf8.char",
    "utf8.len",
];

impl MustUse {
    fn check(ctx: &LintCtx<'_>, out: &mut Vec<Finding>) {
        each_stmt(ctx, out, |ctx, s, out| {
            let Stmt::Call(e, span) = s else {
                return;
            };

            let Expr::Call { func, method, .. } = e else {
                return;
            };

            if method.is_some() {
                return;
            }

            let Some(path) = global_path(ctx, func) else {
                return;
            };

            if !PURE.contains(&path.as_str()) {
                return;
            }

            out.push(
                Finding::new(
                    "must_use",
                    ctx.bytes(*span),
                    format!("{path} returns a value and this discards it"),
                )
                .with_help("assign the result, or the call does nothing"),
            );
        });
    }
}

/*
The dotted path of an expression, when its root is a global.

The global root matters. A local named `string` is the author's own table. A
report about `string.format` on it would be a guess about code that larvae
cannot see.
*/
fn global_path(ctx: &LintCtx<'_>, e: &Expr) -> Option<String> {
    match e {
        Expr::Name(span) => {
            // A name that nothing bound is a global, and these tables are globals.
            ctx.names
                .is_global(span.start)
                .then(|| ctx.text(*span).to_string())
        }

        Expr::Index {
            object,
            key: IndexKey::Field(field),
            ..
        } => Some(format!(
            "{}.{}",
            global_path(ctx, object)?,
            ctx.text(*field)
        )),

        _ => None,
    }
}

// --- deprecated ------------------------------------------------------------

/// The functions that Roblox replaced, and their replacements.
const REPLACED: &[(&str, &str)] = &[
    ("delay", "task.delay"),
    ("elapsedTime", "os.clock"),
    ("spawn", "task.spawn"),
    ("table.foreach", "a for loop"),
    ("table.foreachi", "a for loop"),
    ("table.getn", "the # operator"),
    ("wait", "task.wait"),
];

/*
The methods that Roblox replaced. The lint matches them by name, because
larvae does not know the type of the receiver.

For that reason, `remove` is not on the list. `Instance:remove()` is
deprecated, and `Queue:remove(1)` is the author's own collection. The two
have the same spelling. To report the second is worse than to miss the
first. The list keeps the legacy camelCase names that only Roblox had.
*/
const REPLACED_METHODS: &[(&str, &str)] = &[
    ("Remove", "Destroy"),
    ("children", "GetChildren"),
    ("findFirstChild", "FindFirstChild"),
    ("getChildren", "GetChildren"),
];

#[derive(Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct DeprecatedOptions {
    /// The names that the project itself deprecated, as `old = "new"`.
    pub additional: std::collections::BTreeMap<String, String>,
}

impl Deprecated {
    fn check(ctx: &LintCtx<'_>, out: &mut Vec<Finding>) {
        let options: DeprecatedOptions = ctx.cfg.options_for("deprecated");

        each_expr(ctx, out, |ctx, e, out| {
            let Expr::Call {
                func, method, span, ..
            } = e
            else {
                return;
            };

            if let Some(m) = method {
                let name = ctx.text(*m);

                // These names mean nothing outside Roblox.
                if ctx.cfg.std != crate::lint::config::StdLib::Roblox {
                    return;
                }

                if let Some((_, replacement)) =
                    REPLACED_METHODS.iter().find(|(old, _)| *old == name)
                {
                    out.push(
                        Finding::new("deprecated", ctx.bytes(*m), format!("{name} is deprecated"))
                            .with_help(format!("use {replacement} instead")),
                    );
                }

                return;
            }

            let Some(path) = global_path(ctx, func) else {
                return;
            };

            let replacement = REPLACED
                .iter()
                .find(|(old, _)| *old == path)
                .map(|(_, new)| (*new).to_string())
                .or_else(|| options.additional.get(&path).cloned());

            if let Some(replacement) = replacement {
                out.push(
                    Finding::new(
                        "deprecated",
                        ctx.bytes(*span),
                        format!("{path} is deprecated"),
                    )
                    .with_help(format!("use {replacement} instead")),
                );
            }
        });
    }
}

// --- restricted_module_paths -----------------------------------------------

#[derive(Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct RestrictedOptions {
    /// The require paths that the project forbids, as `path = "why"`.
    pub paths: std::collections::BTreeMap<String, String>,
}

impl RestrictedModulePaths {
    /*
    This lint is fully config driven, and it is silent until a project
    fills the config in.

    The use is architectural. One example is a shared module that must not
    reach into a server-only package. Another is a deprecated package that
    the project retires one call site at a time. No path is wrong in
    general, so a built-in list does not exist and cannot exist.
    */
    fn check(ctx: &LintCtx<'_>, out: &mut Vec<Finding>) {
        let options: RestrictedOptions = ctx.cfg.options_for("restricted_module_paths");

        if options.paths.is_empty() {
            return;
        }

        each_expr(ctx, out, |ctx, e, out| {
            let Expr::Call {
                func, args, span, ..
            } = e
            else {
                return;
            };

            if !matches!(func.as_ref(), Expr::Name(n) if ctx.text(*n) == "require") {
                return;
            }

            let literal = match args {
                CallArgs::Str(s) => *s,

                CallArgs::Paren(list) => match list.as_slice() {
                    [Expr::String(s)] => *s,

                    _ => return,
                },

                CallArgs::Table(_) => return,
            };

            let text = ctx.text(literal);
            let path = text.get(1..text.len() - 1).unwrap_or(text);

            if let Some(why) = options.paths.get(path) {
                out.push(
                    Finding::new(
                        "restricted_module_paths",
                        ctx.bytes(*span),
                        format!("{path} is restricted"),
                    )
                    .with_help(why.clone()),
                );
            }
        });
    }
}

// --- high_cyclomatic_complexity --------------------------------------------

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ComplexityOptions {
    pub maximum_complexity: u32,
}

impl Default for ComplexityOptions {
    fn default() -> Self {
        // selene's default. larvae keeps it so a project that moves over gets the same results.
        Self {
            maximum_complexity: 40,
        }
    }
}

impl HighCyclomaticComplexity {
    /*
    This lint is off by default.

    The score counts branches, and a number is not the same as a problem.
    A long dispatch table scores high and reads well. A short function
    with three nested conditions scores low and does not read well. The
    lint has value as a limit that a project sets with intent, not as an
    opinion that ships on by default.
    */
    fn check(ctx: &LintCtx<'_>, out: &mut Vec<Finding>) {
        let options: ComplexityOptions = ctx.cfg.options_for("high_cyclomatic_complexity");

        each_stmt(ctx, out, |ctx, s, out| {
            let (body, name) = match s {
                Stmt::Function(n) => (&n.body, ctx.text(n.span)),

                Stmt::LocalFunction(n) => (&n.body, ctx.text(n.name)),

                _ => return,
            };

            let score = 1 + branches_in(ctx, &body.block);

            if score <= options.maximum_complexity {
                return;
            }

            let name = name.split_whitespace().nth(1).unwrap_or(name);

            out.push(
                Finding::new(
                    "high_cyclomatic_complexity",
                    ctx.bytes(body.span),
                    format!(
                        "{name} has a complexity of {score}, over the limit of {}",
                        options.maximum_complexity
                    ),
                )
                .with_help("split it into named pieces"),
            );
        });
    }
}

/// Returns one count per place where control can go two ways.
fn branches_in(ctx: &LintCtx<'_>, block: &Block) -> u32 {
    let mut score = 0;

    for stmt in &block.stmts {
        score += match stmt {
            Stmt::If(n) => {
                n.branches.len() as u32
                    + n.branches
                        .iter()
                        .map(|(c, b)| conditions_in(ctx, c) + branches_in(ctx, b))
                        .sum::<u32>()
                    + n.else_block.as_ref().map_or(0, |b| branches_in(ctx, b))
            }

            Stmt::While(n) => 1 + conditions_in(ctx, &n.cond) + branches_in(ctx, &n.block),

            Stmt::Repeat(n) => 1 + conditions_in(ctx, &n.cond) + branches_in(ctx, &n.block),

            Stmt::NumericFor(n) => 1 + branches_in(ctx, &n.block),

            Stmt::GenericFor(n) => 1 + branches_in(ctx, &n.block),

            Stmt::Do(n) => branches_in(ctx, &n.block),

            _ => 0,
        };
    }

    score
}

/// `and` and `or` are also branches. Each one is a path that control can skip.
fn conditions_in(ctx: &LintCtx<'_>, e: &Expr) -> u32 {
    match e {
        Expr::Binary { op, lhs, rhs, .. } => {
            let own = u32::from(matches!(ctx.text(*op), "and" | "or"));

            own + conditions_in(ctx, lhs) + conditions_in(ctx, rhs)
        }

        Expr::Unary { operand, .. } => conditions_in(ctx, operand),

        Expr::Paren { inner, .. } => conditions_in(ctx, inner),

        _ => 0,
    }
}

// --- manual_table_clone ----------------------------------------------------

impl ManualTableClone {
    /*
    ```lua
    for k, v in pairs(source) do
        target[k] = v
    end
    ```

    `table.clone` does this in one call, in C, without the per-key dispatch.
    The lint reports only a body that is exactly this one assignment.
    Anything else in the loop means that the copy is not the whole purpose.
    */
    fn check(ctx: &LintCtx<'_>, out: &mut Vec<Finding>) {
        each_block(ctx, out, |ctx, block, out| {
            for pair in block.stmts.windows(2) {
                /*
                The destination must be a table that this block just created.

                Without that check, the lint reports a merge:
                `for k, v in pairs(source) do target[k] = v end` inside a
                function that takes `target`. There the advice is worse than
                the code, because `table.clone` would discard everything
                already in `target`.
                */
                let Stmt::Local(local) = &pair[0] else {
                    continue;
                };

                let ([fresh], [Expr::Table { fields, .. }]) =
                    (local.names.as_slice(), local.values.as_slice())
                else {
                    continue;
                };

                if !fields.is_empty() {
                    continue;
                }

                Self::one(ctx, &pair[1], ctx.text(fresh.name), out);
            }
        });
    }

    fn one(ctx: &LintCtx<'_>, stmt: &Stmt, fresh: &str, out: &mut Vec<Finding>) {
        {
            let Stmt::GenericFor(n) = stmt else {
                return;
            };

            let [key, value] = n.vars.as_slice() else {
                return;
            };

            let [iterator] = n.exprs.as_slice() else {
                return;
            };

            let Some(source) = pairs_over(ctx, iterator) else {
                return;
            };

            let [Stmt::Assign(assign)] = n.block.stmts.as_slice() else {
                return;
            };

            if ctx.text(assign.op) != "=" {
                return;
            }

            let ([target], [assigned]) = (assign.targets.as_slice(), assign.values.as_slice())
            else {
                return;
            };

            // The assigned value must be the loop's value variable.
            if !matches!(assigned, Expr::Name(n) if ctx.text(*n) == ctx.text(value.name)) {
                return;
            }

            // The index of the target must be the loop's key variable.
            let Expr::Index {
                object,
                key: IndexKey::Computed(index),
                ..
            } = target
            else {
                return;
            };

            if !matches!(index.as_ref(), Expr::Name(n) if ctx.text(*n) == ctx.text(key.name)) {
                return;
            }

            // The target must be the table declared empty directly above.
            if !matches!(object.as_ref(), Expr::Name(n) if ctx.text(*n) == fresh) {
                return;
            }

            out.push(
                Finding::new(
                    "manual_table_clone",
                    ctx.bytes(n.span),
                    "this loop copies a table one key at a time",
                )
                .with_help(format!("table.clone({source}) does it in one call")),
            );
        }
    }
}

/// Returns the table that an iterator walks, when the iterator is `pairs(t)`.
fn pairs_over<'a>(ctx: &LintCtx<'a>, e: &Expr) -> Option<&'a str> {
    let Expr::Call { func, args, .. } = e else {
        return None;
    };

    if !matches!(func.as_ref(), Expr::Name(n) if ctx.text(*n) == "pairs") {
        return None;
    }

    match args {
        CallArgs::Paren(list) => match list.as_slice() {
            [only] => Some(ctx.text(only.span())),

            _ => None,
        },

        _ => None,
    }
}

// --- mismatched_arg_count --------------------------------------------------

impl MismatchedArgCount {
    /*
    A call to a function declared in this file with the wrong number of
    arguments.

    The lint checks only functions bound to a local, and only calls through
    that local. Then the declaration and the call site are both in view.
    Anything can reassign a function reached through a table. A lint that
    assumed otherwise would be wrong on exactly the codebases that need it.

    The lint reports too few arguments, and also too many. The second case
    is legal, and Lua discards the extra values. But an argument that goes
    nowhere means that a person expected the function to read it.
    */
    fn check(ctx: &LintCtx<'_>, out: &mut Vec<Finding>) {
        let mut arity: std::collections::HashMap<usize, (usize, usize)> =
            std::collections::HashMap::new();

        each_stmt(ctx, out, |ctx, s, _| {
            let (token, body) = match s {
                Stmt::LocalFunction(n) => (n.name.start, &n.body),

                Stmt::Local(n) => match (n.names.as_slice(), n.values.as_slice()) {
                    ([binding], [Expr::Function { body, .. }]) => (binding.name.start, &**body),

                    _ => return,
                },

                _ => return,
            };

            let Some(&index) = ctx.names.by_token.get(&token) else {
                return;
            };

            /*
            A vararg parameter accepts any count past the named parameters.
            A binding that is written after its declaration can hold a
            different function. Thus the lint cannot check either case.
            */
            if body.params.iter().any(|p| p.is_vararg)
                || !ctx.names.bindings[index].writes.is_empty()
            {
                return;
            }

            /*
            To pass fewer arguments than a function declares is legal and
            normal Luau, and an optional parameter, `level: string?`, states
            that directly. Thus the lint reports too few arguments only when
            every parameter carries a type and no parameter is optional.
            That form is the author's statement that all parameters are
            required.
            */
            let all_required = !body.params.is_empty()
                && body.params.iter().all(|p| {
                    p.ty.is_some_and(|ty| !ctx.text(ty).trim_end().ends_with('?'))
                });

            let count = body.params.len();
            let min = if all_required { count } else { 0 };

            arity.insert(index, (min, count));
        });

        each_expr(ctx, out, |ctx, e, out| {
            let Expr::Call {
                func,
                method,
                args,
                span,
                ..
            } = e
            else {
                return;
            };

            if method.is_some() {
                return;
            }

            let Expr::Name(name) = func.as_ref() else {
                return;
            };

            let Some(&index) = ctx.names.read_of.get(&name.start) else {
                return;
            };

            let Some(&(min, max)) = arity.get(&index) else {
                return;
            };

            let CallArgs::Paren(list) = args else {
                return;
            };

            // A call or a vararg in the last position can supply any number of values.
            if list
                .last()
                .is_some_and(|e| matches!(e, Expr::Call { .. } | Expr::Vararg(_)))
            {
                return;
            }

            let given = list.len();

            if given >= min && given <= max {
                return;
            }

            let word = if given < min { "expects" } else { "takes only" };

            out.push(
                Finding::new(
                    "mismatched_arg_count",
                    ctx.bytes(*span),
                    format!(
                        "{} {word} {min} arguments but {given} were passed",
                        ctx.text(*name)
                    ),
                )
                .with_help("the extra ones are nil, or discarded"),
            );
        });
    }
}
