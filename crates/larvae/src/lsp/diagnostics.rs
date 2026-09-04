/*!
Findings as protocol diagnostics: the lint pass over one document, and the
push that puts the result on screen.
*/

use std::io::Write;
use std::path::Path;

use anyhow::Result;
use serde_json::{Value, json};

use crate::lint::{self, Finding, Level};

use super::uri::path_of_uri;
use super::{Server, rpc};

impl Server {
    /// Lint one document and push the result
    pub(super) fn publish(&self, uri: &str, out: &mut impl Write) -> Result<()> {
        /*
        The Studio tree reaches the type checker here, because a publish is
        the moment the analyzer is free and the moment its answer is about
        to be read.
        */
        self.refresh_studio();

        let Some(src) = self.documents.get(uri) else {
            return Ok(());
        };

        let path = path_of_uri(uri);

        /*
        The server publishes an excluded file as empty and does not skip it.
        A skip would keep the old diagnostics on screen until the editor
        closed the file. `[lint] enabled = false` clears the file for the
        same reason.
        */
        if !self.lint.is_enabled() || path.as_deref().is_some_and(|p| self.excluded.skips(p)) {
            return rpc::notify(
                out,
                "textDocument/publishDiagnostics",
                json!({ "uri": uri, "diagnostics": [] }),
            );
        }

        let lines = rpc::Lines::new(src);

        // The owner of the extension reports on the file, and nobody else.
        let claimed = path
            .as_deref()
            .and_then(|p| self.worms.frontend_for(p).map(|index| (p, index)));

        /*
        Claim-only mode serves the files of worms and stays quiet on the
        rest, so stock luau-lsp can own the plain Luau of the project. The
        publish is empty and not skipped, for the same reason as an
        excluded file: a skip keeps old squiggles on screen.
        */
        if (!self.lsp.enabled || (self.lsp.claim_only && !self.worms.lsp_serves_luau()))
            && claimed.is_none()
        {
            return rpc::notify(
                out,
                "textDocument/publishDiagnostics",
                json!({ "uri": uri, "diagnostics": [] }),
            );
        }

        /*
        A claimed file is a module of the analyzer too: a plain Luau file
        requires it through the worm's lowering, and the analyzer caches
        that lowering by path. Without the invalidation here, an edit to a
        data file kept every dependent on the old shape until a restart:
        the author saved a new field and the require still typed the old
        table.
        */
        if let Some((path, index)) = claimed
            && self.lsp.analyzer
            && let Some(analysis) = self.analysis.borrow_mut().as_mut()
        {
            analysis.invalidate(path);

            /*
            The buffer's own lowering opens too, or the next check would
            read the file from disk: an unsaved edit reflected nowhere,
            and a saved one only because the disk happened to agree. The
            same open is what the plain path below does with its view.
            */
            if let Ok(outcome) = self.worms.compile(index, src)
                && outcome.ok
            {
                analysis.open(path, &super::analysis::plain_view(&outcome.text));
            }
        }

        let mut diagnostics = match claimed {
            Some((path, index)) => self.claimed_diagnostics(path, index, src, &lines),

            None => match lint::analyze_with(
                src,
                &self.lint,
                path.as_deref()
                    .map(crate::syntax::parser::ParseOptions::for_path)
                    .unwrap_or_default(),
            ) {
                Ok(mut findings) => {
                    /*
                    The worms that lint plain Luau speak after the builtin
                    lints. A worm that fails stays quiet here, because one
                    broken worm must not take the diagnostics of every
                    Luau file down with it.
                    */
                    if let Some(path) = path.as_deref()
                        && let Ok(more) = lint::foreign(path, src, &self.lint, &self.worms, None)
                    {
                        findings.extend(more);
                    }

                    findings
                        .into_iter()
                        .map(|f| diagnostic(src, &lines, f))
                        .collect::<Vec<_>>()
                }

                // A syntax error is one diagnostic, and it stops the other checks.
                Err(e) => {
                    /*
                    One break, one message, and Luau's own words where the
                    analyzer is landed: its parse errors ride the check
                    below, spelled the way every Luau reader knows them.
                    larvae's spelling stands in while the session loads,
                    and for the files no analyzer reads.
                    */
                    let luau_speaks =
                        self.lsp.analyzer && path.is_some() && self.analysis.borrow().is_some();

                    match luau_speaks {
                        true => Vec::new(),

                        false => {
                            let at = e.offset as u32;

                            vec![json!({
                                "range": lines.range(src, (at, at + 1)),
                                "severity": 1,
                                "source": "larvae",
                                "message": e.message,
                            })]
                        }
                    }
                }
            },
        };

        // The realm findings, the same ones `larvae check` reports.
        if let Some(path) = path.as_deref() {
            diagnostics.extend(self.realm_diagnostics(path, src, &lines));
        }

        /*
        The analyzer's findings join the lint findings in one publish. The
        analyzer reads plain Luau, so a claimed file stays out until the
        tier-1 hooks lower it; the lints of the worm already cover it.
        */
        if claimed.is_none()
            && self.lsp.analyzer
            && let Some(analysis) = self.analysis.borrow_mut().as_mut()
            && let Some(path) = path.as_deref()
        {
            analysis.invalidate(path);
            analysis.open(path, &super::analysis::plain_view(src));

            for diag in analysis.check(path) {
                let mut entry = json!({
                    "range": lines.range(src, (diag.span.0, diag.span.1)),
                    "severity": diag.severity,
                    /*
                    Luau's type checker found it, so Luau is what the editor
                    names. luau-lsp says the same, and a reader who searches
                    the message finds answers written about Luau.
                    */
                    "source": "Luau",
                    // The tree's own type names mean nothing to a reader.
                    "message": self.instances.readable(&diag.message),
                });

                /*
                The error number, shown as `Luau(1042)` and linked to the
                page that explains the checker. Luau numbers its errors from
                1000, and the number names the kind of the error.
                */
                if let Some(code) = diag.code.as_deref().and_then(|c| c.parse::<i64>().ok()) {
                    entry["code"] = json!(code);
                    entry["codeDescription"] = json!({ "href": TYPE_ERROR_DOCS });
                }

                diagnostics.push(entry);
            }
        }

        /*
        The deprecated uses, struck through and quiet.

        Severity 4 is Hint, which draws no squiggle and stays out of the
        problems panel, and tag 2 is Deprecated, which is the
        strikethrough. Luau's own linter finds the uses, so a member
        found through a type is found here.
        */
        if claimed.is_none()
            && self.lsp.analyzer
            && let Some(analysis) = self.analysis.borrow_mut().as_mut()
            && let Some(path) = path.as_deref()
        {
            let marks: Vec<Value> = analysis
                .deprecated_uses(path)
                .into_iter()
                .map(|diag| {
                    json!({
                        "range": lines.range(src, (diag.span.0, diag.span.1)),
                        "severity": diag.severity,
                        "source": "Luau",
                        "message": diag.message,
                        "tags": [2],
                    })
                })
                .collect();

            /*
            One deprecation, one voice. Larvae's own `deprecated` lint
            carries a built-in list for the CLI, where no analyzer runs;
            here the platform's marks are the precise ones, so a larvae
            finding that overlaps one stands down. A name the project
            deprecated itself gets no platform mark, and larvae still
            speaks for it.
            */
            diagnostics.retain(|d| {
                d["code"] != "deprecated"
                    || !marks.iter().any(|m| {
                        m["range"]["start"]["line"] == d["range"]["start"]["line"]
                            && m["range"]["end"]["line"] == d["range"]["end"]["line"]
                            && overlaps(&m["range"], &d["range"])
                    })
            });

            diagnostics.extend(marks);
        }

        /*
        A worm that refused to lower a required module says why, here, at
        the require that names it. Without this the require answered
        `*error-type*` and nothing anywhere said the reason, while the
        build would have printed it. The check above is what ran the
        loads, so the reasons are current by the time this reads them.
        */
        if let (Some(path), Some(root)) = (path.as_deref(), self.root.as_deref())
            && let Ok(errors) = self.load_errors.lock()
            && !errors.is_empty()
        {
            let claims = self.worms.lsp_resolved_claims();

            for link in super::decorate::links_with_claims(src, path, root, &self.aliases, &claims)
            {
                let canonical = link.target.canonicalize().ok();
                let Some(reason) = errors
                    .get(&link.target)
                    .or_else(|| canonical.as_deref().and_then(|c| errors.get(c)))
                else {
                    continue;
                };

                diagnostics.push(json!({
                    "range": lines.range(src, link.range),
                    "severity": 1,
                    "source": "larvae",
                    "message": format!(
                        "this module does not lower: {}",
                        reason.lines().next().unwrap_or(reason)
                    ),
                }));
            }
        }

        // Tier 3: the worms that transform diagnostics see the list first.
        let context = json!({ "path": path, "text": src });
        let mut diagnostics = self
            .worms
            .lsp_respond("diagnostics", &context, json!(diagnostics));

        /*
        `--no-warning` drops every warning after the worms have spoken, so
        a warning a worm added goes the same way as a builtin one. Errors
        stay, and so do the hints: a strikethrough is not a warning.
        */
        if self.forced.no_warnings
            && let Some(list) = diagnostics.as_array_mut()
        {
            list.retain(|d| d["severity"] != 2);
        }

        rpc::notify(
            out,
            "textDocument/publishDiagnostics",
            json!({ "uri": uri, "diagnostics": diagnostics }),
        )
    }

    /*
    The realm findings of one open file, the way `larvae check` finds
    them.

    A cross-realm require compiles, resolves, and then fails in a live
    game, and the resolver is the only reader that sees both ends. The
    build reported it and the editor said nothing. The same validation
    runs here, filtered to the realm findings: everything else the
    resolver says is the build's business, and the analyzer already
    covers the requires that resolve to nothing.
    */
    fn realm_diagnostics(&self, path: &Path, src: &str, lines: &rpc::Lines) -> Vec<Value> {
        let Some(root) = self.root.as_deref() else {
            return Vec::new();
        };

        let Ok(lexed) = crate::syntax::lexer::lex(src) else {
            return Vec::new();
        };

        let scanned = crate::syntax::scan::scan(src, &lexed.toks);

        if scanned.sites.is_empty() && scanned.instances.is_empty() {
            return Vec::new();
        }

        let luaurc = super::decorate::luaurc_upward(path, root);
        let claimed = self.worms.claimed();

        let resolver = crate::requires::resolve::Resolver {
            root,
            toml_aliases: &self.aliases,
            luaurc: &luaurc,
            mounts: &self.mounts,
            target: crate::config::Target::RobloxString,
            style: crate::config::IndexingStyle::default(),
            quote: '"',
            strict: false,
            cross_realm: Default::default(),
            claimed: &claimed,
            client_relative_requires: false,
        };

        let ctx = crate::requires::resolve::FileCtx::new(
            path,
            &self.mounts,
            crate::config::Target::RobloxString,
            crate::config::IndexingStyle::default(),
        );

        let mut found = Vec::new();

        let mut keep = |diags: Vec<crate::diag::Diag>, span: (u32, u32)| {
            for d in diags {
                let Some(message) = d.message.strip_suffix(" (cross_realm_require)") else {
                    continue;
                };

                let message = match &d.help {
                    Some(help) => format!("{message}\n{help}"),

                    None => message.to_owned(),
                };

                found.push(json!({
                    "range": lines.range(src, span),
                    "severity": 1,
                    "source": "larvae",
                    "code": "cross_realm_require",
                    "message": message,
                }));
            }
        };

        for site in &scanned.sites {
            let spec = &src[site.inner_start as usize..site.inner_end as usize];
            let mut diags = Vec::new();
            let _ = resolver.resolve(&ctx, spec, src, site.at as usize, &mut diags);

            keep(diags, (site.tok_start, site.tok_end));
        }

        for site in &scanned.instances {
            let mut diags = Vec::new();
            let _ = resolver.resolve_instance(&ctx, site, src, &mut diags);

            keep(diags, (site.start, site.end));
        }

        found
    }

    /*
    The diagnostics of a file that a worm claims.

    The worm reports its own findings. The lints of larvae read the Luau view
    of the file as well, when the project inherits them. A worm that does
    neither leaves the list empty, and the file is then quiet.
    */
    fn claimed_diagnostics(
        &self,
        path: &Path,
        index: usize,
        src: &str,
        lines: &rpc::Lines,
    ) -> Vec<Value> {
        match lint::claimed(path, src, &self.lint, &self.worms, index) {
            Ok(findings) => findings
                .into_iter()
                .map(|f| diagnostic(src, lines, f))
                .collect(),

            /*
            A worm that fails becomes one diagnostic at its position. The
            editor then names the reason, and the file does not look clean.
            */
            Err(e) => {
                let (line, column) = e.line_col.unwrap_or((1, 1));
                let at = json!({
                    "line": line.saturating_sub(1),
                    "character": column.saturating_sub(1),
                });

                vec![json!({
                    "range": { "start": at, "end": at },
                    "severity": 1,
                    "source": "larvae",
                    "message": match e.help {
                        Some(help) => format!("{}\n{help}", e.message),

                        None => e.message,
                    },
                })]
            }
        }
    }
}

/// Where the editor sends a reader who clicks a Luau error number
const TYPE_ERROR_DOCS: &str = "https://luau.org/typecheck";

/*
One finding as a diagnostic of the protocol.

A finding of larvae and a finding of a worm arrive in the same shape, so both
routes render here. The help goes into the message, because the editor has
the room for it.
*/
/// Whether two same-line protocol ranges share any characters.
fn overlaps(a: &Value, b: &Value) -> bool {
    let of = |v: &Value, side: &str| v[side]["character"].as_u64().unwrap_or(0);

    of(a, "start") < of(b, "end") && of(b, "start") < of(a, "end")
}

fn diagnostic(src: &str, lines: &rpc::Lines, finding: Finding) -> Value {
    json!({
        "range": lines.range(src, finding.span),
        "severity": severity_of(finding.level),
        "source": "larvae",
        "code": finding.lint,
        "message": match finding.help {
            Some(help) => format!("{}\n{help}", finding.message),

            None => finding.message,
        },
    })
}

fn severity_of(level: Level) -> u8 {
    match level {
        // 1 is Error, 2 is Warning, 3 is Information, and Allow never reaches here
        Level::Deny => 1,

        Level::Info => 3,

        _ => 2,
    }
}
