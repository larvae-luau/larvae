//! One file, lex to scan to resolve to edits to output

use std::path::Path;

use anyhow::{Context, Result};

use crate::config::{Config, QuoteStyle};
use crate::diag::Diag;
use crate::requires::resolve::{FileCtx, Resolver, Rewrite, lua_quote};
use crate::rules::{Edits, Family, Rule};
use crate::syntax::lexer;
use crate::syntax::scan;

use super::output::write_atomic;

/// Owner label for the require rewriter's own edits
const REQUIRES: &str = "the require rewriter";

/// Per file transform options, resolved from the config once
pub struct FileOpts {
    quotes: QuoteStyle,
    /// Check mode parses too, so syntax errors surface before Studio sees them
    validate_syntax: bool,
    const_requires: bool,
    /// Compiled `except` patterns, Some means the rule is on
    remove_comments: Option<Vec<regex::Regex>>,
    /// Comment text and whether it goes at the start
    append_comment: Option<(String, bool)>,
    directive: Option<String>,
    /// Read require(script.Parent.Foo) chains as input
    instance_input: bool,
    /// Compile time constants, empty when none are configured
    defines: std::collections::HashMap<String, crate::rules::defines::Value>,
    /// Per path target overrides, in the order the config wrote them
    overrides: Vec<crate::config::Override>,
}

impl FileOpts {
    /// Resolve the per file transform options once, not per file
    pub fn from_config(root: &Path, config: &Config, write: bool) -> Result<Self> {
        let remove_comments = match &config.rules.remove_comments {
            Some(rc) if rc.enabled() => {
                let mut pats = Vec::new();

                for p in rc.except() {
                    match regex::Regex::new(&p) {
                        Ok(re) => pats.push(re),

                        Err(e) => {
                            anyhow::bail!("invalid remove_comments except pattern \"{p}\": {e}")
                        }
                    }
                }

                Some(pats)
            }

            _ => None,
        };

        let append_comment = match &config.rules.append_text_comment {
            Some(a) => {
                let text = match (&a.text, &a.file) {
                    (Some(t), _) => t.clone(),

                    (None, Some(f)) => {
                        std::fs::read_to_string(root.join(f)).with_context(|| {
                            format!("append_text_comment file {}", crate::ui::rel(f))
                        })?
                    }
                    _ => unreachable!("validated at load"),
                };

                Some((text, a.location == "start"))
            }

            None => None,
        };

        Ok(Self {
            quotes: config.process.quotes,
            validate_syntax: !write,
            const_requires: config.rules.const_requires,
            remove_comments,
            append_comment,
            directive: config.rules.add_luau_directive.clone(),
            instance_input: config.requires.instance_input,
            defines: match &config.defines {
                Some(table) => crate::rules::defines::parse(table).map_err(anyhow::Error::msg)?,

                None => Default::default(),
            },
            overrides: match &config.requires.overrides {
                Some(table) => crate::config::parse_overrides(table)?,

                None => Vec::new(),
            },
        })
    }
}

/// What one file contributed to the run summary
pub(super) struct FileOutcome {
    pub rewrites: usize,
    pub dynamic: usize,
    /// Rules that changed something in this file, each listed once
    pub applied: Vec<Rule>,
}

/*
One file, through every stage in run order.

Stages are real rather than a sorting of edits. Our own rules occupy the slot
`[process] run_order` names, a worm sits either side of it, and between two
slots the buffer is spliced and re-lexed so a later stage genuinely reads what
an earlier one produced. With nobody asking for an order there is one stage and
this costs nothing.
*/
#[allow(clippy::too_many_arguments)]
pub(super) fn process_file(
    path: &Path,
    src: &str,
    dest_rel: &Path,
    output: &Path,
    resolver: &Resolver,
    opts: &FileOpts,
    rules_cfg: &crate::config::RulesConfig,
    write: bool,
    worms: &crate::worm::pool::Pool,
    // false when a front-end declared it resolves its own requires
    own_requires: bool,
    diags: &mut Vec<Diag>,
) -> Option<FileOutcome> {
    let slots = worms.slots();
    let mut current = std::borrow::Cow::Borrowed(src);
    let mut outcome = None;

    for (i, &slot) in slots.iter().enumerate() {
        let last = i + 1 == slots.len();
        let mut edits = Edits::new();

        if slot == worms.native() {
            outcome = native_pass(
                path,
                &current,
                dest_rel,
                resolver,
                opts,
                rules_cfg,
                own_requires,
                &mut edits,
                diags,
            );

            // a file that will not lex is out of the build, later stages included
            outcome.as_ref()?;
        } else {
            let Ok(lexed) = lexer::lex(&current) else {
                continue;
            };

            run_worm_rules(worms, slot, &current, &lexed, path, &mut edits, diags);
        }

        let mut clashes = Vec::new();

        if last {
            if write {
                let dest = output.join(dest_rel);
                let out = crate::rules::splice(&current, &edits, &mut clashes);

                if let Err(e) = write_atomic(&dest, out.as_bytes()) {
                    diags.push(Diag::error(path, format!("write failed: {e:#}")));
                }
            } else {
                // check does not build the output, it still owes the same warnings
                clashes = crate::rules::edits::conflicts(&edits);
            }
        } else {
            current = std::borrow::Cow::Owned(crate::rules::splice(&current, &edits, &mut clashes));
        }

        for c in clashes {
            diags.push(
                Diag::warning(
                    path,
                    format!(
                        "{} and {} both rewrote these bytes, only {} was applied",
                        c.kept, c.dropped, c.kept
                    ),
                )
                .at(src, c.at as usize),
            );
        }

        if let Some(outcome) = outcome.as_mut() {
            outcome.applied.extend(edits.applied());
        }
    }

    outcome
}

/// Our own work: requires, token rules, and the ast rules that want a tree
#[allow(clippy::too_many_arguments)]
fn native_pass(
    path: &Path,
    src: &str,
    dest_rel: &Path,
    resolver: &Resolver,
    opts: &FileOpts,
    rules_cfg: &crate::config::RulesConfig,
    own_requires: bool,
    edits: &mut Edits,
    diags: &mut Vec<Diag>,
) -> Option<FileOutcome> {
    let lexed = match lexer::lex(src) {
        Ok(t) => t,

        Err(e) => {
            diags.push(Diag::error(path, format!("lex error: {}", e.message)).at(src, e.offset));

            return None;
        }
    };

    if opts.validate_syntax
        && let Err(e) = crate::syntax::parser::parse(src, &lexed.toks)
    {
        diags.push(Diag::error(path, format!("syntax error, {}", e.message)).at(src, e.offset));
    }

    /*
    A worm that owns its own requires means we do not look at them, so there is
    nothing to scan. Skipping here rather than later also means no realm
    diagnostics for a file we were told not to reason about.
    */
    let scanned = if own_requires {
        scan::scan(src, &lexed.toks)
    } else {
        Default::default()
    };

    /*
    A mixed project can want a different output form per directory, ex:
    client code that runs out of a Starter container cannot use absolute
    @game strings the way shared code can
    */
    let (target, style) = match crate::config::override_for(&opts.overrides, dest_rel) {
        Some(o) => (o.target, o.style.unwrap_or(resolver.style)),

        None => (resolver.target, resolver.style),
    };

    let ctx = FileCtx::new(path, resolver.mounts, target, style);

    let quote = opts.quotes.char();
    let requote = opts.quotes != QuoteStyle::Preserve;

    // every site with its final emitted form, rules use this to spot
    // requires that point at the same module
    let mut site_forms: Vec<(scan::RequireSite, String)> = Vec::new();

    for site in &scanned.sites {
        let spec = &src[site.inner_start as usize..site.inner_end as usize];

        match resolver.resolve(&ctx, spec, src, site.at as usize, diags) {
            Rewrite::Keep => {
                site_forms.push((*site, spec.to_string()));
                // untouched requires still get the configured quote style
                if requote
                    && src.as_bytes()[site.tok_start as usize] != quote as u8
                    && !spec.contains(['"', '\'', '\\'])
                {
                    edits.push(
                        REQUIRES,
                        (site.tok_start, site.tok_end, lua_quote(spec, quote)),
                    );
                }
            }

            Rewrite::Replace(new) => {
                site_forms.push((*site, new.clone()));

                if requote {
                    edits.push(
                        REQUIRES,
                        (site.tok_start, site.tok_end, lua_quote(&new, quote)),
                    );
                } else {
                    edits.push(REQUIRES, (site.inner_start, site.inner_end, new));
                }
            }

            // instance exprs replace the whole argument, parenless calls need wrapping parens
            Rewrite::Expr(expr) => {
                site_forms.push((*site, expr.clone()));
                let expr = if site.has_parens {
                    expr
                } else {
                    format!("({expr})")
                };

                edits.push(REQUIRES, (site.tok_start, site.tok_end, expr));
            }
        }
    }

    /*
    Instance chains, the legacy form. Resolved through the project map and
    re-emitted like any other require, and left exactly as written whenever
    the chain cannot be followed all the way
    */
    if opts.instance_input {
        for site in &scanned.instances {
            if let Some(expr) = resolver.resolve_instance(&ctx, site, src, diags) {
                edits.push(REQUIRES, (site.start, site.end, expr));
            }
        }
    }

    let rewrites = edits.len();

    // everything past the requires is a rule that ships inside larvae
    edits.family(Family::Native, |edits| {
        if opts.const_requires {
            edits.run("const_requires", |e| {
                crate::rules::const_requires(src, &lexed.toks, &scanned.sites, e)
            });
        }

        if let Some(except) = &opts.remove_comments {
            edits.run("remove_comments", |e| {
                crate::rules::remove_comments(src, &lexed.comments, except, e)
            });
        }

        if let Some(directive) = &opts.directive
            && let Some(rep) = crate::rules::add_luau_directive(src, directive)
        {
            edits.push("add_luau_directive", rep);
        }

        if let Some((text, at_start)) = &opts.append_comment
            && let Some(rep) = crate::rules::append_text_comment(src, text, *at_start)
        {
            edits.push("append_text_comment", rep);
        }

        if crate::rules::wants_ast(rules_cfg, &opts.defines) {
            let dm = ctx.dm.as_ref().map(|d| d.game_path());
            crate::rules::apply_ast_rules(
                rules_cfg,
                &opts.defines,
                src,
                &lexed,
                &site_forms,
                dm.as_deref(),
                quote,
                edits,
                diags,
                path,
            );
        }
    });

    Some(FileOutcome {
        rewrites,
        dynamic: scanned.dynamic.len(),
        applied: Vec::new(),
    })
}

/*
Flatten the file once, bucket its nodes by kind, and hand each worm the ones
its rules asked for. Parsing happens here and only here, so a project whose
worms declare narrow filters keeps the fast path on files that match nothing.
*/
fn run_worm_rules(
    worms: &crate::worm::pool::Pool,
    slot: i64,
    src: &str,
    lexed: &crate::syntax::lexer::Lexed,
    path: &Path,
    edits: &mut Edits,
    diags: &mut Vec<Diag>,
) {
    use std::sync::Arc;

    use crate::worm::ctx::FileCtx;
    use crate::worm::nodes::NodeTable;
    use crate::worm::pool::Matched;

    let in_slot = worms.in_slot(slot);

    if in_slot.is_empty() {
        return;
    }

    let Ok(chunk) = crate::syntax::parser::parse(src, &lexed.toks) else {
        // a syntax error is already reported by check, and a rule cannot help here
        return;
    };

    let toks = &lexed.toks;
    let bytes = |span: crate::syntax::ast::TokSpan| -> (u32, u32) {
        if span.is_empty() {
            let at = toks
                .get(span.start as usize)
                .map(|t| t.start)
                .unwrap_or(src.len() as u32);

            return (at, at);
        }

        (
            toks[span.start as usize].start,
            toks[span.end as usize - 1].end,
        )
    };

    let table = NodeTable::build(&chunk, &bytes);
    let matched = Matched::of(&table, &worms.filters());

    if matched.is_empty() {
        return;
    }

    let file = Arc::new(FileCtx::new(table, src.to_owned(), crate::ui::rel(path)));

    for (index, spec) in in_slot {
        match worms.run(index, Arc::clone(&file), &matched) {
            Ok(worm_edits) => {
                for edit in worm_edits {
                    edits.push("worm", edit);
                }
            }

            Err(e) => diags.push(Diag::error(
                path,
                format!("worm `{}`: {e:#}", spec.manifest.name),
            )),
        }
    }
}
