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

/// None when the file was skipped, ex: it would not lex
#[allow(clippy::too_many_arguments)]
pub(super) fn process_file(
    path: &Path,
    // where this file lands, relative to the output directory
    dest_rel: &Path,
    output: &Path,
    resolver: &Resolver,
    opts: &FileOpts,
    rules_cfg: &crate::config::RulesConfig,
    write: bool,
    diags: &mut Vec<Diag>,
) -> Option<FileOutcome> {
    let src = match std::fs::read_to_string(path) {
        Ok(s) => s,

        Err(e) => {
            diags.push(Diag::error(
                path,
                format!("cannot read file (UTF-8 required): {e}"),
            ));

            return None;
        }
    };

    let lexed = match lexer::lex(&src) {
        Ok(t) => t,

        Err(e) => {
            diags.push(Diag::error(path, format!("lex error: {}", e.message)).at(&src, e.offset));

            return None;
        }
    };

    if opts.validate_syntax
        && let Err(e) = crate::syntax::parser::parse(&src, &lexed.toks)
    {
        diags.push(Diag::error(path, format!("syntax error, {}", e.message)).at(&src, e.offset));
    }

    let scanned = scan::scan(&src, &lexed.toks);

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
    let mut edits = Edits::new();

    // every site with its final emitted form, rules use this to spot
    // requires that point at the same module
    let mut site_forms: Vec<(scan::RequireSite, String)> = Vec::new();

    for site in &scanned.sites {
        let spec = &src[site.inner_start as usize..site.inner_end as usize];

        match resolver.resolve(&ctx, spec, &src, site.at as usize, diags) {
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
            if let Some(expr) = resolver.resolve_instance(&ctx, site, &src, diags) {
                edits.push(REQUIRES, (site.start, site.end, expr));
            }
        }
    }

    let rewrites = edits.len();

    // everything past the requires is a rule that ships inside larvae
    edits.family(Family::Native, |edits| {
        if opts.const_requires {
            edits.run("const_requires", |e| {
                crate::rules::const_requires(&src, &lexed.toks, &scanned.sites, e)
            });
        }

        if let Some(except) = &opts.remove_comments {
            edits.run("remove_comments", |e| {
                crate::rules::remove_comments(&src, &lexed.comments, except, e)
            });
        }

        if let Some(directive) = &opts.directive
            && let Some(rep) = crate::rules::add_luau_directive(&src, directive)
        {
            edits.push("add_luau_directive", rep);
        }

        if let Some((text, at_start)) = &opts.append_comment
            && let Some(rep) = crate::rules::append_text_comment(&src, text, *at_start)
        {
            edits.push("append_text_comment", rep);
        }

        if crate::rules::wants_ast(rules_cfg, &opts.defines) {
            let dm = ctx.dm.as_ref().map(|d| d.game_path());
            crate::rules::apply_ast_rules(
                rules_cfg,
                &opts.defines,
                &src,
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

    let mut clashes = Vec::new();

    if write {
        let dest = output.join(dest_rel);
        let out_src = crate::rules::splice(&src, &edits, &mut clashes);

        if let Err(e) = write_atomic(&dest, out_src.as_bytes()) {
            diags.push(Diag::error(path, format!("write failed: {e:#}")));
        }
    } else {
        // check does not build the output, it still owes the same warnings
        clashes = crate::rules::edits::conflicts(&edits);
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
            .at(&src, c.at as usize),
        );
    }

    Some(FileOutcome {
        rewrites,
        dynamic: scanned.dynamic.len(),
        applied: edits.applied(),
    })
}
