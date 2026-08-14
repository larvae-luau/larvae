//! One file: lex, scan, resolve, edit, output.

use std::path::Path;

use anyhow::{Context, Result};

use crate::config::{Config, QuoteStyle};
use crate::diag::Diag;
use crate::requires::resolve::{FileCtx, Resolver, Rewrite, lua_quote};
use crate::rules::{Edits, Family, Rule};
use crate::syntax::lexer;
use crate::syntax::scan;

use super::output::write_atomic;

/// The owner label for the edits of the require rewriter
const REQUIRES: &str = "the require rewriter";

/// Per file transform options, resolved from the config once
pub struct FileOpts {
    quotes: QuoteStyle,
    /// Check mode also parses, so syntax errors appear before Studio sees them
    validate_syntax: bool,
    const_requires: bool,
    /// Compiled `except` patterns, Some means the rule is on
    remove_comments: Option<Vec<regex::Regex>>,
    /// Remove `-- larvae: allow(...)` comments from the output
    strip_flags: bool,
    /// The comment text, and true when the text goes at the start
    append_comment: Option<(String, bool)>,
    directive: Option<String>,
    /// Read require(script.Parent.Foo) chains as input
    instance_input: bool,
    /// Compile time constants; empty when the config sets none
    defines: std::collections::HashMap<String, crate::rules::defines::Value>,
    /// Target overrides for each path, in the order of the config
    overrides: Vec<crate::config::Override>,
}

impl FileOpts {
    /// Resolve the transform options once, not once for each file
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
            /*
            remove_comments applies to every comment in the file, and this
            rule to a few. So the broader rule wins in full, and the two rules
            do not edit the same span. A project that keeps flags with an
            `except` pattern wants that result.
            */
            strip_flags: config.process.strip_flags && remove_comments.is_none(),
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

/// The contribution of one file to the run summary
pub(super) struct FileOutcome {
    pub rewrites: usize,
    pub dynamic: usize,
    /// The rules that changed this file, each listed once
    pub applied: Vec<Rule>,
    /// The modules this file requires; its contribution to the require graph
    pub required: Vec<std::path::PathBuf>,
    /// Each resolved require site, for anything that must rewrite one
    pub sites: Vec<crate::requires::graph::Site>,
    /*
    The text the native pass scanned, kept only when the run asks for it.

    The byte spans in `sites` index this exact text, not the file on disk: a
    front-end worm replaces the buffer of a claimed file before the scan.
    The bundler reads modules from here for the same reason.
    */
    pub source: Option<String>,
}

/*
One file, through every stage in run order.

Stages are real and not a sort of edits. The larvae rules use the slot that
`[process] run_order` names, and a worm sits on each side of it. Between two
slots, larvae splices the buffer and lexes it again. So a later stage reads
the true output of an earlier one. When no config sets an order, there is one
stage, and this costs nothing.
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
    // False when a front-end declares that it resolves its own requires.
    own_requires: bool,
    // True when the caller wants the scanned text back, see FileOutcome.
    keep_source: bool,
    diags: &mut Vec<Diag>,
) -> Option<FileOutcome> {
    let slots = worms.slots();
    let mut current = std::borrow::Cow::Borrowed(src);
    let mut outcome = None;

    for (i, &slot) in slots.iter().enumerate() {
        let last = i + 1 == slots.len();
        let mut edits = Edits::new();

        /*
        The slot list holds the native slot once, so this pass and its
        require harvest run once for the file. A later stage extends
        `applied` only, and adds no edges.
        */
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

            // A file that does not lex is out of the build, later stages included.
            outcome.as_ref()?;

            /*
            The buffer at the native slot, which is the text the require
            sites index. A later slot can edit the buffer again, so the copy
            happens here and not at the end.
            */
            if keep_source && let Some(o) = outcome.as_mut() {
                o.source = Some(current.to_string());
            }
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
                // Check does not build the output, but it must give the same warnings.
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

/// The larvae work: requires, token rules, and the ast rules that need a tree
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
    When a worm owns its requires, larvae does not examine them, so there is
    nothing to scan. The skip here, and not later, also prevents realm
    diagnostics for a file that larvae must not analyze.
    */
    let scanned = if own_requires {
        scan::scan(src, &lexed.toks)
    } else {
        Default::default()
    };

    /*
    A mixed project can want a different output form for each directory.
    Example: client code that runs from a Starter container cannot use
    absolute @game strings the way shared code can.
    */
    let (target, style) = match crate::config::override_for(&opts.overrides, dest_rel) {
        Some(o) => (o.target, o.style.unwrap_or(resolver.style)),

        None => (resolver.target, resolver.style),
    };

    let ctx = FileCtx::new(path, resolver.mounts, target, style);

    let quote = opts.quotes.char();
    let requote = opts.quotes != QuoteStyle::Preserve;

    // Every site with its final emitted form. Rules use this to find
    // requires that point at the same module.
    let mut site_forms: Vec<(scan::RequireSite, String)> = Vec::new();

    // Every site with the module it resolved to, for the require graph.
    let mut resolved_sites: Vec<crate::requires::graph::Site> = Vec::new();

    for site in &scanned.sites {
        let spec = &src[site.inner_start as usize..site.inner_end as usize];

        /*
        The resolver appends to `ctx.required` when a require resolves to a
        module. So the entry at the old length, when one exists, is the
        target of this site.
        */
        let before = ctx.required.borrow().len();
        let rewrite = resolver.resolve(&ctx, spec, src, site.at as usize, diags);

        if let Some(target) = ctx.required.borrow().get(before) {
            resolved_sites.push(crate::requires::graph::Site {
                at: site.at,
                tok_start: site.tok_start,
                tok_end: site.tok_end,
                target: target.clone(),
            });
        }

        match rewrite {
            Rewrite::Keep => {
                site_forms.push((*site, spec.to_string()));
                // Unchanged requires also get the configured quote style.
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

            // Instance expressions replace the full argument; calls without parens need added parens.
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
    Instance chains, the legacy form. The resolver follows them through the
    project map and emits them like any other require. When the resolver
    cannot follow the full chain, the chain stays exactly as written.
    */
    if opts.instance_input {
        for site in &scanned.instances {
            if let Some(expr) = resolver.resolve_instance(&ctx, site, src, diags) {
                edits.push(REQUIRES, (site.start, site.end, expr));
            }
        }
    }

    let rewrites = edits.len();

    // Each edit after the requires comes from a rule inside larvae.
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

        if opts.strip_flags {
            edits.run("strip_flags", |e| {
                crate::rules::strip_flags(src, &lexed.comments, e)
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
        required: ctx.required.take(),
        sites: resolved_sites,
        source: None,
    })
}

/*
Flatten the file once, group its nodes by kind, and give each worm the nodes
that its rules request. The parse happens here and only here. So a project
with narrow worm filters keeps the fast path on files that match nothing.
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
        // Check already reports a syntax error, and a rule cannot help here.
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
