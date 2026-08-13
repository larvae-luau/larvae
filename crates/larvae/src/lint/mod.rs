/*!
`larvae lint`.

A lint reads a parsed file and reports what looks wrong. This module holds
what every lint shares: the trait, the registry, the run, and the suppression
comments. With such a comment, an author says that one finding is wrong here.

The editor is the design constraint. When the language server exists, a lint
pass runs on every keystroke. Thus no code here builds an index that it can
borrow instead. larvae computes the analysis that every lint needs, the
target of each name, once per file in [`ctx`] and not once per lint.
*/

pub mod config;
pub mod ctx;
pub mod globals;
pub mod lints;
pub mod scope;

use std::path::Path;

use anyhow::Result;

use crate::diag::{Diag, Severity};
use crate::syntax::{lexer, parser};

pub use config::{Level, LintConfig};
pub use ctx::{Finding, LintCtx};

/// One thing that can be wrong with a file.
pub trait Lint: Sync {
    /// The name that a user writes to configure the lint. It matches selene's spelling.
    fn name(&self) -> &'static str;

    /// Tells if the lint is on by default, and at which level.
    fn default_level(&self) -> Level;

    /// One line of description. `larvae lint --explain` shows it.
    fn about(&self) -> &'static str;

    fn run(&self, ctx: &LintCtx<'_>, out: &mut Vec<Finding>);
}

/// Returns every lint, in the order that larvae reports their findings within a line.
pub fn registry() -> &'static [&'static dyn Lint] {
    lints::ALL
}

/// Look up one lint by the name that a user writes.
pub fn find(name: &str) -> Option<&'static dyn Lint> {
    registry().iter().copied().find(|l| l.name() == name)
}

/// A file that did not parse, so larvae cannot report on it.
#[derive(Debug)]
pub struct ParseFailure {
    pub offset: usize,
    pub message: String,
}

/*
Run every enabled lint over one file, and keep the byte spans.

The spans are the reason that this is separate from [`lint`]. A terminal
needs a line and a column. An editor needs a range to underline. To turn a
span into either form is easy. To turn a line and a column back into a span
is not easy.
*/
pub fn analyze(src: &str, cfg: &LintConfig) -> Result<Vec<Finding>, ParseFailure> {
    let fail = |offset, message: String| ParseFailure {
        offset,
        message: format!("syntax error, {message}"),
    };

    let lexed = lexer::lex(src).map_err(|e| fail(e.offset, e.message))?;
    let chunk = parser::parse(src, &lexed.toks).map_err(|e| fail(e.offset, e.message))?;

    let ctx = LintCtx::new(src, &lexed.toks, &lexed.comments, &chunk, cfg);
    let mut findings = Vec::new();

    for lint in registry() {
        let level = cfg.level_for(lint.name(), lint.default_level());

        if level == Level::Allow {
            continue;
        }

        let before = findings.len();
        lint.run(&ctx, &mut findings);

        // A lint states what it found. The config states the level.
        for finding in &mut findings[before..] {
            finding.level = level;
        }
    }

    findings.retain(|f| !ctx.suppressed(f));
    findings.sort_by(|a, b| (a.span.0, a.lint.as_ref()).cmp(&(b.span.0, b.lint.as_ref())));

    Ok(findings)
}

/*
Lint one claimed file with the findings that its worm reported.

The worm found the problems, and nothing more. The host treats the findings
here, exactly as it treats the builtin findings. The `[lint.rules]` levels
override the manifest's defaults. The `-- larvae: allow(...)` comments
suppress through the comment spans that the worm returned. The rendering is
the same.

The host refuses a finding under a name that the worm did not declare.
Without that rule, a typo in a worm is a lint that the user cannot configure
or explain.
*/
pub fn from_worm(
    path: &Path,
    src: &str,
    reply: crate::worm::proto::LintReply,
    cfg: &LintConfig,
    declared: &std::collections::BTreeMap<String, crate::worm::manifest::LintDecl>,
    worm: &str,
) -> Result<Vec<Diag>, Diag> {
    let bad_span = |what: &str, (start, end): (u32, u32)| {
        Diag::error(
            path,
            format!("worm `{worm}` reported {what} span {start}..{end} off the source"),
        )
    };

    for &span in &reply.comments {
        if !span_ok(src, span) {
            return Err(bad_span("a comment", span));
        }
    }

    let line_starts = ctx::line_starts_of(src);
    let allowed = ctx::collect_suppressions(src, &reply.comments, &line_starts);

    let mut findings = Vec::new();

    for finding in reply.findings {
        let Some(decl) = declared.get(&finding.lint) else {
            return Err(Diag::error(
                path,
                format!(
                    "worm `{worm}` reported lint `{}`, which it does not declare",
                    finding.lint
                ),
            ));
        };

        if !span_ok(src, finding.span) {
            return Err(bad_span("a finding", finding.span));
        }

        // The worm states what it found. The config states the level.
        let level = cfg.level_for(&finding.lint, decl.default);

        if level == Level::Allow {
            continue;
        }

        let line = (line_starts.partition_point(|&s| s <= finding.span.0) - 1) as u32;

        if ctx::allowed_here(&allowed, line, &finding.lint) {
            continue;
        }

        findings.push(Finding {
            lint: std::borrow::Cow::Owned(finding.lint),
            level,
            span: finding.span,
            message: finding.message,
            help: finding.help,
        });
    }

    findings.sort_by(|a, b| (a.span.0, a.lint.as_ref()).cmp(&(b.span.0, b.lint.as_ref())));

    let index = crate::diag::LineIndex::new(src);

    Ok(findings
        .into_iter()
        .map(|f| f.into_diag(path, src, &index))
        .collect())
}

/// Returns true if a span lies on the source. The span came from a wire, so the host checks it.
fn span_ok(src: &str, (start, end): (u32, u32)) -> bool {
    let (s, e) = (start as usize, end as usize);

    s <= e && e <= src.len() && src.is_char_boundary(s) && src.is_char_boundary(e)
}

/*
Lint one file, and return diagnostics for a terminal.

A parse failure comes back as a diagnostic, not as an error. A lint run over
a tree must report on every file that the user gave it. One file that does
not compile must not stop the run.
*/
pub fn lint(path: &Path, src: &str, cfg: &LintConfig) -> Result<Vec<Diag>, Diag> {
    let findings = analyze(src, cfg).map_err(|e| Diag::error(path, e.message).at(src, e.offset))?;

    // One scan for the file, not one scan per finding.
    let index = crate::diag::LineIndex::new(src);

    Ok(findings
        .into_iter()
        .map(|f| f.into_diag(path, src, &index))
        .collect())
}

impl Finding {
    fn into_diag(self, path: &Path, src: &str, index: &crate::diag::LineIndex) -> Diag {
        let severity = match self.level {
            Level::Deny => Severity::Error,

            _ => Severity::Warning,
        };

        let diag = Diag {
            severity,
            file: path.to_owned(),
            line_col: None,
            message: format!("{} ({})", self.message, self.lint),
            help: self.help,
        };

        diag.at_indexed(index, src, self.span.0 as usize)
    }
}

#[cfg(test)]
mod worm_tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::worm::manifest::LintDecl;
    use crate::worm::proto::{LintReply, WireFinding};

    fn declared(name: &str, default: Level) -> BTreeMap<String, LintDecl> {
        BTreeMap::from([(
            name.to_owned(),
            LintDecl {
                description: None,
                default,
            },
        )])
    }

    fn finding(lint: &str, span: (u32, u32)) -> WireFinding {
        WireFinding {
            span,
            lint: lint.to_owned(),
            message: "untidy".to_owned(),
            help: None,
        }
    }

    fn reply(findings: Vec<WireFinding>) -> LintReply {
        LintReply {
            findings,
            comments: Vec::new(),
        }
    }

    #[test]
    fn the_host_stamps_levels_and_the_manifest_default_holds() {
        let src = "<Frame>\n";
        let path = Path::new("a.luaux");

        let diags = from_worm(
            path,
            src,
            reply(vec![finding("tidy", (0, 7))]),
            &LintConfig::default(),
            &declared("tidy", Level::Warn),
            "luaux",
        )
        .unwrap();

        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].severity, crate::diag::Severity::Warning);
        assert!(diags[0].message.contains("(tidy)"), "{}", diags[0].message);

        // The [lint.rules] level overrides the manifest, exactly as for a builtin lint.
        let mut cfg = LintConfig::default();
        cfg.rules.insert("tidy".to_owned(), Level::Deny);

        let raised = from_worm(
            path,
            src,
            reply(vec![finding("tidy", (0, 7))]),
            &cfg,
            &declared("tidy", Level::Warn),
            "luaux",
        )
        .unwrap();

        assert_eq!(raised[0].severity, crate::diag::Severity::Error);
    }

    #[test]
    fn an_allow_level_drops_a_worm_finding() {
        let mut cfg = LintConfig::default();
        cfg.rules.insert("tidy".to_owned(), Level::Allow);

        let diags = from_worm(
            Path::new("a.luaux"),
            "<Frame>\n",
            reply(vec![finding("tidy", (0, 7))]),
            &cfg,
            &declared("tidy", Level::Warn),
            "luaux",
        )
        .unwrap();

        assert!(diags.is_empty());
    }

    #[test]
    fn an_allow_comment_suppresses_via_the_worms_comment_spans() {
        let src = "-- larvae: allow(tidy)\n<Frame>\n";

        let suppressed = from_worm(
            Path::new("a.luaux"),
            src,
            LintReply {
                findings: vec![finding("tidy", (23, 30))],
                comments: vec![(0, 22)],
            },
            &LintConfig::default(),
            &declared("tidy", Level::Warn),
            "luaux",
        )
        .unwrap();

        assert!(suppressed.is_empty());

        // Without the comment spans, the same finding stays. This is by design.
        let stands = from_worm(
            Path::new("a.luaux"),
            src,
            reply(vec![finding("tidy", (23, 30))]),
            &LintConfig::default(),
            &declared("tidy", Level::Warn),
            "luaux",
        )
        .unwrap();

        assert_eq!(stands.len(), 1);
    }

    #[test]
    fn an_undeclared_lint_is_refused_naming_the_worm() {
        let refusal = from_worm(
            Path::new("a.luaux"),
            "<Frame>\n",
            reply(vec![finding("typo", (0, 7))]),
            &LintConfig::default(),
            &declared("tidy", Level::Warn),
            "luaux",
        )
        .unwrap_err();

        assert!(refusal.message.contains("luaux"), "{}", refusal.message);
        assert!(refusal.message.contains("`typo`"), "{}", refusal.message);
    }

    #[test]
    fn a_finding_span_off_the_source_is_refused() {
        let refusal = from_worm(
            Path::new("a.luaux"),
            "<Frame>\n",
            reply(vec![finding("tidy", (0, 99))]),
            &LintConfig::default(),
            &declared("tidy", Level::Warn),
            "luaux",
        )
        .unwrap_err();

        assert!(refusal.message.contains("0..99"), "{}", refusal.message);
    }
}
