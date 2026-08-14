/*!
Builtin rules

The token level rules live in this file. Each rule that needs the tree
goes through the engine. The darklua parity rules live in one submodule.
The larvae rules live in the other submodule.
*/

pub mod darklua;
pub mod defines;
pub mod edits;
pub mod engine;
pub mod native;
pub mod scope;

use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::config::RulesConfig;
use crate::diag::Diag;
use crate::syntax::lexer::{Lexed, Tok, TokKind};
use crate::syntax::scan::RequireSite;

pub use edits::{Conflict, Edits, Family, Rule, splice};

/// True when one or more enabled rules need a parse.
pub fn wants_ast(cfg: &RulesConfig, defines: &HashMap<String, defines::Value>) -> bool {
    !defines.is_empty() || darklua::wants(cfg) || native::wants(cfg)
}

/*
Parse once, then give the tree to both rule families. Larvae cannot
transform a file that does not parse. The user asked for rules that
larvae cannot run, so a parse failure is an error and not a silent skip.
*/
#[allow(clippy::too_many_arguments)]
pub fn apply_ast_rules(
    cfg: &RulesConfig,
    defines: &HashMap<String, defines::Value>,
    src: &str,
    lexed: &Lexed,
    require_forms: &[(RequireSite, String)],
    dm_path: Option<&str>,
    quote: char,
    edits: &mut Edits,
    diags: &mut Vec<Diag>,
    path: &Path,
) {
    let chunk = match crate::syntax::parser::parse(src, &lexed.toks) {
        Ok(c) => c,

        Err(e) => {
            diags.push(
                Diag::error(
                    path,
                    format!(
                        "rules need a parse and this file has a syntax error, {}",
                        e.message
                    ),
                )
                .at(src, e.offset),
            );

            return;
        }
    };

    /*
    The split of locals from globals costs a walk. Larvae does the walk
    only when there are defines to look up. Without the walk, larvae
    would substitute a local named DEBUG like the global name.
    */
    let globals = if defines.is_empty() {
        HashSet::new()
    } else {
        scope::globals(&engine::RuleCtx {
            src,
            toks: &lexed.toks,
            chunk: &chunk,
            comments: &lexed.comments,
            require_forms,
            dm_path,
            quote,
            defines,
            globals: &HashSet::new(),
        })
    };

    let ctx = engine::RuleCtx {
        src,
        toks: &lexed.toks,
        chunk: &chunk,
        comments: &lexed.comments,
        require_forms,
        dm_path,
        quote,
        defines,
        globals: &globals,
    };

    // The defines run first. The folding rules need the literals in place.
    edits.run("defines", |e| defines::apply(&ctx, e));
    darklua::apply(cfg, &ctx, edits, diags, path);
    native::apply(cfg, &ctx, edits, diags, path);
}

/*
const_requires: change `local X = require(...)` to `const X = require(...)`.
Then single assignment requires get Luau's const treatment. The rule is
conservative by design. It does not change multi bindings or annotated
locals.

Luau enforces `const`. A converted name that a later statement reassigns
turns a file that ran into a syntax error. So the rule resolves the scopes,
the same walk that the linter and `require_binding` use, and keeps `local`
on every binding with writes. A file that does not parse gets no conversion,
because the rule cannot prove that a conversion compiles.
*/
pub fn const_requires(
    src: &str,
    toks: &[Tok],
    sites: &[RequireSite],
    replacements: &mut Vec<(u32, u32, String)>,
) {
    let mut candidates = Vec::new();

    for site in sites {
        let i = site.require_idx;
        // The rule looks backward for the pattern: local <name> = require
        if i < 3 {
            continue;
        }

        let (eq, name, local) = (&toks[i - 1], &toks[i - 2], &toks[i - 3]);

        if eq.kind == TokKind::Symbol
            && eq.text(src) == "="
            && name.kind == TokKind::Ident
            && local.kind == TokKind::Ident
            && local.text(src) == "local"
        {
            // The name token index identifies the binding in the scope walk.
            candidates.push(((i - 2) as u32, local));
        }
    }

    if candidates.is_empty() {
        return;
    }

    let Ok(chunk) = crate::syntax::parser::parse(src, toks) else {
        return;
    };

    let names = crate::lint::scope::resolve(src, toks, &chunk);

    for (name_idx, local) in candidates {
        // A later statement reassigns the name, so const would be a syntax error.
        let reassigned = names
            .by_token
            .get(&name_idx)
            .and_then(|&b| names.bindings.get(b))
            .is_none_or(|b| !b.writes.is_empty());

        if !reassigned {
            replacements.push((local.start, local.end, "const".to_string()));
        }
    }
}

/*
remove_comments: delete the comment spans but keep their newlines. Then
retain-lines output stays on the same line numbers. A comment that matches
an `except` pattern stays. Luau directives such as --!strict stay by
default.
*/
pub fn remove_comments(
    src: &str,
    comments: &[(u32, u32)],
    except: &[regex::Regex],
    replacements: &mut Vec<(u32, u32, String)>,
) {
    let bytes = src.as_bytes();

    for &(start, end) in comments {
        let text = &src[start as usize..end as usize];

        if except.iter().any(|re| re.is_match(text)) {
            continue;
        }

        // Remove the horizontal space in front, so no trailing blanks remain.
        let mut from = start as usize;

        while from > 0 && matches!(bytes[from - 1], b' ' | b'\t') {
            from -= 1;
        }

        let newlines = text.bytes().filter(|&b| b == b'\n').count();
        replacements.push((from as u32, end, "\n".repeat(newlines)));
    }
}

/*
strip_flags: remove the comments that give instructions to larvae.

A `-- larvae: allow(...)` comment is an instruction to the linter. The
game does not need it, so larvae removes it like each other build time
instruction. Notes to a reader stay. The rule removes only the comments
that [`crate::flags`] recognises.

The rule keeps newlines in the same way as remove_comments. Then
retain-lines output stays on the same line numbers.
*/
pub fn strip_flags(src: &str, comments: &[(u32, u32)], replacements: &mut Vec<(u32, u32, String)>) {
    let bytes = src.as_bytes();

    for &(start, end) in comments {
        if !crate::flags::is_flag(&src[start as usize..end as usize]) {
            continue;
        }

        // Remove the horizontal space in front, so no trailing blanks remain.
        let mut from = start as usize;

        while from > 0 && matches!(bytes[from - 1], b' ' | b'\t') {
            from -= 1;
        }

        let newlines = src[start as usize..end as usize]
            .bytes()
            .filter(|&b| b == b'\n')
            .count();

        replacements.push((from as u32, end, "\n".repeat(newlines)));
    }
}

/*
add_luau_directive: make sure that each file selects a Luau mode. The rule
does not change an existing directive of the same kind, or a different
mode. An explicit choice in the source always wins.
*/
pub fn add_luau_directive(src: &str, directive: &str) -> Option<(u32, u32, String)> {
    let wanted = format!("--!{directive}");

    for line in src.lines() {
        let line = line.trim();

        if line.is_empty() {
            continue;
        }

        if let Some(rest) = line.strip_prefix("--!") {
            // The same family, for example strict and nonstrict. Keep the existing choice.
            let head = |s: &str| s.split_whitespace().next().unwrap_or("").to_string();

            if line == wanted || mode_family(&head(rest)) == mode_family(&head(directive)) {
                return None;
            }

            continue;
        }

        if line.starts_with("--") {
            continue;
        }

        break;
    }

    Some((0, 0, format!("{wanted}\n")))
}

/// Directives that contradict each other. Each family holds one choice.
fn mode_family(word: &str) -> &'static str {
    match word {
        "strict" | "nonstrict" | "nocheck" => "typecheck",

        "native" => "native",

        "optimize" => "optimize",

        _ => "other",
    }
}

/// append_text_comment: add a comment at the start or the end of the file.
pub fn append_text_comment(src: &str, text: &str, at_start: bool) -> Option<(u32, u32, String)> {
    let comment: String = text
        .lines()
        .map(|line| format!("-- {line}\n"))
        .collect::<Vec<_>>()
        .concat();
    if at_start {
        Some((0, 0, comment))
    } else {
        let end = src.len() as u32;
        let lead = if src.ends_with('\n') { "" } else { "\n" };

        Some((end, end, format!("{lead}{comment}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax::{lexer, scan};

    fn apply(src: &str) -> String {
        let toks = lexer::lex(src).unwrap().toks;
        let scanned = scan::scan(src, &toks);
        let mut reps = Vec::new();

        const_requires(src, &toks, &scanned.sites, &mut reps);
        reps.sort_by_key(|r| r.0);

        let mut out = String::new();
        let mut cursor = 0usize;

        for (s, e, new) in reps {
            out.push_str(&src[cursor..s as usize]);
            out.push_str(&new);
            cursor = e as usize;
        }

        out.push_str(&src[cursor..]);

        out
    }

    #[test]
    fn converts_simple_require_locals() {
        assert_eq!(
            apply("local Signal = require(\"./signal\")"),
            "const Signal = require(\"./signal\")"
        );
        assert_eq!(
            apply("local a = 1\nlocal S = require(\"@pkg/s\")\n"),
            "local a = 1\nconst S = require(\"@pkg/s\")\n"
        );
    }

    #[test]
    fn leaves_tricky_forms_alone() {
        // A multi binding.
        assert_eq!(
            apply("local a, b = require(\"./x\"), 2"),
            "local a, b = require(\"./x\"), 2"
        );
        // A type annotation.
        assert_eq!(
            apply("local x: Foo = require(\"./x\")"),
            "local x: Foo = require(\"./x\")"
        );
        // Not a local.
        assert_eq!(apply("x = require(\"./x\")"), "x = require(\"./x\")");
        // A dynamic require has no site.
        assert_eq!(apply("local x = require(p)"), "local x = require(p)");
    }

    /*
    Luau enforces const. A conversion of a reassigned name would produce
    `Variable 'M' is constant and may not be reassigned`, so the rule must
    keep `local` there.
    */
    #[test]
    fn a_reassigned_require_binding_keeps_local() {
        let src = "local M = require(\"./m\")\nM = fallback\nreturn M\n";

        assert_eq!(apply(src), src);
    }

    #[test]
    fn remove_comments_keeps_line_count_and_directives() {
        let src = "--!strict\nlocal a = 1 -- trailing\n--[[ block\nspans lines ]]\nreturn a\n";
        let lexed = lexer::lex(src).unwrap();
        let except = vec![regex::Regex::new("^--!").unwrap()];
        let mut reps = Vec::new();

        remove_comments(src, &lexed.comments, &except, &mut reps);
        reps.sort_by_key(|r| r.0);

        let mut out = String::new();
        let mut cursor = 0usize;

        for (s, e, new) in reps {
            out.push_str(&src[cursor..s as usize]);
            out.push_str(&new);
            cursor = e as usize;
        }

        out.push_str(&src[cursor..]);
        // The directive stays, the other comments go, and the line count does not change.
        assert!(out.starts_with("--!strict"));
        assert!(!out.contains("trailing"));
        assert!(!out.contains("spans lines"));
        assert_eq!(src.lines().count(), out.lines().count());
    }

    #[test]
    fn strip_flags_takes_the_instructions_and_leaves_the_notes() {
        let src = concat!(
            "--!strict\n",
            "-- larvae: allow(unused_variable)\n",
            "local a = 1 -- selene: allow(shadowing)\n",
            "-- larvae: this one is a note\n",
            "return a -- why\n",
        );

        let lexed = lexer::lex(src).unwrap();
        let mut reps = Vec::new();

        strip_flags(src, &lexed.comments, &mut reps);
        reps.sort_by_key(|r| r.0);

        let mut out = String::new();
        let mut cursor = 0usize;

        for (s, e, new) in reps {
            out.push_str(&src[cursor..s as usize]);
            out.push_str(&new);
            cursor = e as usize;
        }

        out.push_str(&src[cursor..]);

        assert!(!out.contains("allow("), "the flags go: {out}");
        assert!(out.contains("--!strict"));
        assert!(out.contains("this one is a note"), "a note stays: {out}");
        assert!(out.contains("-- why"));
        assert!(
            out.contains("local a = 1\n"),
            "no trailing space left: {out}"
        );
        assert_eq!(src.lines().count(), out.lines().count());
    }

    #[test]
    fn luau_directive_respects_the_source() {
        assert_eq!(
            add_luau_directive("return 1\n", "strict").unwrap().2,
            "--!strict\n"
        );
        // The directive is already there.
        assert!(add_luau_directive("--!strict\nreturn 1\n", "strict").is_none());
        // A different typecheck mode is an explicit choice.
        assert!(add_luau_directive("--!nonstrict\nreturn 1\n", "strict").is_none());
        // An unrelated directive does not block it.
        assert_eq!(
            add_luau_directive("--!native\nreturn 1\n", "strict")
                .unwrap()
                .2,
            "--!strict\n"
        );
        // Plain leading comments also do not block it.
        assert_eq!(
            add_luau_directive("-- header\nreturn 1\n", "strict")
                .unwrap()
                .2,
            "--!strict\n"
        );
    }

    #[test]
    fn append_comment_at_both_ends() {
        let (s, e, text) = append_text_comment("return 1\n", "generated", true).unwrap();
        assert_eq!((s, e), (0, 0));
        assert_eq!(text, "-- generated\n");

        let src = "return 1";
        let (s, e, text) = append_text_comment(src, "two\nlines", false).unwrap();

        assert_eq!((s, e), (8, 8));
        assert_eq!(text, "\n-- two\n-- lines\n");
    }
}
