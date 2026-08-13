/*!
`larvae lint`.

A lint is a thing that reads a parsed file and says what looks wrong. This
module holds what every lint shares: the trait, the registry, the run, and the
suppression comments that let an author say a particular one is wrong here.

The design constraint that shaped it is the editor. A lint pass runs on every
keystroke once the language server exists, so nothing here builds an index it
could have borrowed, and the shared analysis every lint wants, what each name
refers to, is computed once per file in [`ctx`] rather than once per lint.
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

/// One thing that can be wrong with a file
pub trait Lint: Sync {
    /// What a user writes to configure it, matching selene's spelling
    fn name(&self) -> &'static str;

    /// Whether it is on by default, and how loudly
    fn default_level(&self) -> Level;

    /// One line, shown by `larvae lint --explain`
    fn about(&self) -> &'static str;

    fn run(&self, ctx: &LintCtx<'_>, out: &mut Vec<Finding>);
}

/// Every lint, in the order their findings are reported within a line
pub fn registry() -> &'static [&'static dyn Lint] {
    lints::ALL
}

/// Look up one lint by the name a user would write
pub fn find(name: &str) -> Option<&'static dyn Lint> {
    registry().iter().copied().find(|l| l.name() == name)
}

/// A file that did not parse, so nothing could be said about it
#[derive(Debug)]
pub struct ParseFailure {
    pub offset: usize,
    pub message: String,
}

/*
Run every enabled lint over one file, keeping the byte spans.

The spans are why this is separate from [`lint`]. A terminal wants a line and
column, an editor wants a range to underline, and turning a span into either is
easy while turning a line and column back into a span is not.
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

        // a lint states what it found, the config states how loudly to say it
        for finding in &mut findings[before..] {
            finding.level = level;
        }
    }

    findings.retain(|f| !ctx.suppressed(f));
    findings.sort_by(|a, b| (a.span.0, a.lint.as_ref()).cmp(&(b.span.0, b.lint.as_ref())));

    Ok(findings)
}

/*
Lint one claimed file with what its worm reported.

The worm found the problems and nothing more. Everything about how they are
treated happens here, exactly as it does for the builtins: `[lint.rules]`
levels over the manifest's defaults, `-- larvae: allow(...)` suppression via
the comment spans the worm returned, and the same rendering. A finding under
a name the worm never declared is refused, because otherwise a typo in a worm
is a lint that cannot be configured or explained.
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

        // the worm states what it found, the config states how loudly
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

/// Whether a wire span lies on the source, since it came off a wire
fn span_ok(src: &str, (start, end): (u32, u32)) -> bool {
    let (s, e) = (start as usize, end as usize);

    s <= e && e <= src.len() && src.is_char_boundary(s) && src.is_char_boundary(e)
}

/*
Lint one file, as diagnostics for a terminal.

Parse failures come back as a diagnostic rather than an error, because a lint
run over a tree is expected to report on every file it was given and one file
that does not compile should not end the run.
*/
pub fn lint(path: &Path, src: &str, cfg: &LintConfig) -> Result<Vec<Diag>, Diag> {
    let findings = analyze(src, cfg).map_err(|e| Diag::error(path, e.message).at(src, e.offset))?;

    // one scan for the file, rather than one per finding
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

        // [lint.rules] beats the manifest, exactly as it does for builtins
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

        // without the comment spans the same finding stands, by design
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
