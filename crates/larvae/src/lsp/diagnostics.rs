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

        let diagnostics = match claimed {
            Some((path, index)) => self.claimed_diagnostics(path, index, src, &lines),

            None => match lint::analyze(src, &self.lint) {
                Ok(findings) => findings
                    .into_iter()
                    .map(|f| diagnostic(src, &lines, f))
                    .collect::<Vec<_>>(),

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

        rpc::notify(
            out,
            "textDocument/publishDiagnostics",
            json!({ "uri": uri, "diagnostics": diagnostics }),
        )
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

fn severity_of(level: Level) -> u8 {
    match level {
        // 1 is Error, 2 is Warning, 3 is Information, and Allow never reaches here
        Level::Deny => 1,

        Level::Info => 3,

        _ => 2,
    }
}
