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
    let findings = worm_findings(path, src, reply, cfg, declared, worm)?;

    Ok(into_diags(path, src, findings))
}

/*
The same findings, with the byte spans kept.

[`from_worm`] gives a line and a column, which a terminal needs. The language
server needs the byte span instead, because the editor underlines a range.
*/
fn worm_findings(
    path: &Path,
    src: &str,
    reply: crate::worm::proto::LintReply,
    cfg: &LintConfig,
    declared: &std::collections::BTreeMap<String, crate::worm::manifest::LintDecl>,
    worm: &str,
) -> Result<Vec<Finding>, Diag> {
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

        /*
        The worm declares a bare name, and larvae puts it under the key of the
        worm. Thus a project writes every lint of one worm in one table, and a
        worm cannot take a name that larvae or another worm already owns.
        */
        let name = qualified(worm, &finding.lint);

        // The worm states what it found. The config states the level.
        let level = cfg.level_for(&name, decl.default);

        if level == Level::Allow {
            continue;
        }

        let line = (line_starts.partition_point(|&s| s <= finding.span.0) - 1) as u32;

        if ctx::allowed_here(&allowed, line, &name) {
            continue;
        }

        findings.push(Finding {
            lint: std::borrow::Cow::Owned(name),
            level,
            span: finding.span,
            message: finding.message,
            help: finding.help,
        });
    }

    findings.sort_by(|a, b| (a.span.0, a.lint.as_ref()).cmp(&(b.span.0, b.lint.as_ref())));

    Ok(findings)
}

/*
A Luau view of a claimed file, for the lints of larvae to read.

A claimed file is not Luau, so the builtin lints cannot read it. The worm
gives larvae one of these two views instead, and the lints run unchanged. The
variants differ only in how exact the position of a finding is.
*/
pub enum LuauView<'a> {
    /*
    The source with every region that is not Luau replaced by filler of the
    same byte length.

    Each offset in the shadow is the same offset in the source, so larvae maps
    nothing. The scope is also correct, because the shadow is one whole file
    and not a set of fragments.
    */
    Shadow(&'a str),
    /*
    The output of the front-end of the worm.

    Larvae maps a finding by line, because a front-end keeps the line count of
    its input. The column becomes the start of the line. This view costs the
    worm nothing, because the worm already compiles the file.
    */
    Projection(&'a str),
}

/*
Run the lints of larvae over a claimed file.

The worm reports what only the worm knows. These lints know Luau, and a
claimed file is mostly Luau. Thus a project that opts in with `inherit_lints`
gets both sets, and the worm author writes no Luau lint.

The levels, the suppression, and the rendering are the same code the builtin
path uses. Thus a lint behaves the same way in a `.luau` file and in a file
that a worm claims.
*/
pub fn inherited(
    path: &Path,
    src: &str,
    view: &LuauView<'_>,
    cfg: &LintConfig,
    worm: &str,
    policy: &crate::config::worms::Inherit,
) -> Result<Vec<Diag>, Diag> {
    let findings = inherited_findings(path, src, view, cfg, worm, policy)?;

    Ok(into_diags(path, src, findings))
}

/// The same findings, with the byte spans kept, for an editor
fn inherited_findings(
    path: &Path,
    src: &str,
    view: &LuauView<'_>,
    cfg: &LintConfig,
    worm: &str,
    policy: &crate::config::worms::Inherit,
) -> Result<Vec<Finding>, Diag> {
    let (text, exact) = match view {
        LuauView::Shadow(text) => (*text, true),

        LuauView::Projection(text) => (*text, false),
    };

    /*
    A view that does not parse is a fault of the worm, and not of the file the
    author wrote. The message names the worm, so the reader knows which side
    to repair.
    */
    let keep = |f: &Finding| policy.allows_lint(&f.lint);

    let findings = analyze(text, cfg).map_err(|e| {
        Diag::error(
            path,
            format!(
                "worm `{worm}` returned Luau larvae cannot read, {}",
                e.message
            ),
        )
    })?;

    if exact {
        return Ok(findings
            .into_iter()
            .filter(|f| (f.span.0 as usize) <= src.len() && keep(f))
            .collect());
    }

    /*
    A line of the projection is the same line of the source, because a
    front-end keeps the line count. The byte inside the line moved, so the
    finding points at the start of the line.
    */
    let view_starts = ctx::line_starts_of(text);
    let src_starts = ctx::line_starts_of(src);

    Ok(findings
        .into_iter()
        .filter(keep)
        .filter_map(|mut f| {
            let line = view_starts.partition_point(|&s| s <= f.span.0) - 1;
            let start = *src_starts.get(line)?;

            f.span = (start, start);

            Some(f)
        })
        .collect())
}

/*
Every finding of one claimed file, from the worm and from larvae.

The routing of a claimed file lives here, because `larvae lint` and the
language server both need it. The two callers differ only in what they make
of a finding: the command renders a line and a column, and the server sends a
range to the editor.
*/
pub fn claimed(
    path: &Path,
    src: &str,
    cfg: &LintConfig,
    pool: &crate::worm::pool::Pool,
    index: usize,
) -> Result<Vec<Finding>, Diag> {
    let spec = pool.spec(index);
    let worm = &spec.manifest.name;
    let mut findings = Vec::new();

    /*
    The worm speaks first, and its reply can also carry the Luau shadow. Thus
    a worm that both reports and inherits answers one request and not two.
    */
    let reply = match spec.lints() {
        true => Some(
            pool.lint(index, src)
                .map_err(|e| Diag::error(path, format!("{e:#}")))?,
        ),

        false => None,
    };

    if spec.inherits_lints() {
        let shadow = reply.as_ref().and_then(|r| r.luau.clone());

        /*
        A worm that sends no shadow still inherits, through the output of its
        own front-end. That output costs the worm nothing, because the worm
        already compiles the file for the pipeline.
        */
        let view = match shadow {
            Some(text) => Some(text),

            None => pool
                .compile(index, src)
                .map_err(|e| Diag::error(path, format!("{e:#}")))?
                .into_source()
                .map(Some)
                .map_err(|e| Diag::error(path, format!("worm `{worm}`, {e:#}")))?,
        };

        if let Some(text) = view {
            let view = match reply.as_ref().and_then(|r| r.luau.as_ref()).is_some() {
                true => LuauView::Shadow(&text),

                false => LuauView::Projection(&text),
            };

            findings.extend(inherited_findings(
                path,
                src,
                &view,
                cfg,
                worm,
                &spec.inherit,
            )?);
        }
    }

    if let Some(reply) = reply {
        findings.extend(worm_findings(
            path,
            src,
            reply,
            cfg,
            &spec.manifest.lints,
            worm,
        )?);
    }

    // A stable sort keeps an inherited finding before a worm finding on one byte.
    findings.sort_by_key(|f| f.span.0);

    Ok(findings)
}

/// Findings as diagnostics for a terminal. One index serves the whole file.
pub fn into_diags(path: &Path, src: &str, findings: Vec<Finding>) -> Vec<Diag> {
    let index = crate::diag::LineIndex::new(src);

    findings
        .into_iter()
        .map(|f| f.into_diag(path, src, &index))
        .collect()
}

/*
The name a lint of a worm answers to everywhere outside the worm.

A name that already holds a dot is left as it is, so a worm that qualifies its
own names keeps them.
*/
pub fn qualified(worm: &str, lint: &str) -> String {
    match lint.contains('.') {
        true => lint.to_owned(),

        false => format!("{worm}.{lint}"),
    }
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

    Ok(into_diags(path, src, findings))
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
            luau: None,
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
        assert!(
            diags[0].message.contains("(luaux.tidy)"),
            "{}",
            diags[0].message
        );

        // The [lint.rules] level overrides the manifest, exactly as for a builtin lint.
        let mut cfg = LintConfig::default();
        cfg.rules.insert("luaux.tidy".to_owned(), Level::Deny);

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
        cfg.rules.insert("luaux.tidy".to_owned(), Level::Allow);

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
        let src = "-- larvae: allow(luaux.tidy)\n<Frame>\n";
        let (start, end) = (29, 36);

        let suppressed = from_worm(
            Path::new("a.luaux"),
            src,
            LintReply {
                findings: vec![finding("tidy", (start, end))],
                comments: vec![(0, 28)],
                luau: None,
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
            reply(vec![finding("tidy", (start, end))]),
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
