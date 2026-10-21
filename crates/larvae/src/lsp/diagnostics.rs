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
        let Some(src) = self.documents.get(uri) else {
            return Ok(());
        };

        let path = path_of_uri(uri);

        // The owner of the extension reports on the file, and nobody else.
        let claimed = path
            .as_deref()
            .and_then(|p| self.worms.frontend_for(p).map(|index| (p, index)));

        /*
        The server publishes a file it does not serve as empty, and does not
        skip it. A skip would keep the old diagnostics on screen until the
        editor closed the file. That covers an excluded file, `[lint]
        enabled = false`, and a plain Luau file under claim-only mode, which
        leaves the plain Luau of the project to another server.
        */
        let quiet = !self.lint.is_enabled()
            || path.as_deref().is_some_and(|p| self.excluded.skips(p))
            || (claimed.is_none() && (!self.lsp.enabled || self.lsp.claim_only));

        if quiet {
            return rpc::notify(
                out,
                "textDocument/publishDiagnostics",
                json!({ "uri": uri, "diagnostics": [] }),
            );
        }

        let lines = rpc::Lines::new(src);

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
                    let at = e.offset as u32;

                    vec![json!({
                        "range": lines.range(src, (at, at + 1)),
                        "severity": 1,
                        "source": "larvae",
                        "message": e.message,
                    })]
                }
            },
        };

        // The realm findings, the same ones `larvae check` reports.
        if let Some(path) = path.as_deref() {
            diagnostics.extend(self.realm_diagnostics(path, src, &lines));
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
    resolver says is the build's business, and luau-lsp already reports
    the requires that resolve to nothing.
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

        let luaurc = luaurc_upward(path, root);
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

/*
One finding as a diagnostic of the protocol.

A finding of larvae and a finding of a worm arrive in the same shape, so both
routes render here. The help goes into the message, because the editor has
the room for it.
*/
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

/*
The `.luaurc` files that cover one document, from its own directory upward.

`larvae process` indexes every `.luaurc` of the project, because it resolves
every file. The server resolves one file per request, and a lookup only ever
walks up from that file. So this walk stops at the root, and an editor does
not read the whole tree on each keystroke.
*/
fn luaurc_upward(path: &Path, root: &Path) -> crate::project::luaurc::LuaurcIndex {
    let mut index = crate::project::luaurc::LuaurcIndex::new(root);
    let mut dir = path.parent();

    while let Some(current) = dir {
        let file = current.join(".luaurc");

        if file.is_file() {
            // A broken .luaurc gives no aliases here. `larvae check` reports it.
            let _ = index.add_file(&file);
        }

        if current == root {
            break;
        }

        dir = current.parent();
    }

    index
}

fn severity_of(level: Level) -> u8 {
    match level {
        // 1 is Error, 2 is Warning, 3 is Information, and Allow never reaches here
        Level::Deny => 1,

        Level::Info => 3,

        _ => 2,
    }
}
