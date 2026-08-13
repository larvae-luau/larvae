/*!
`larvae lint`.

The command has the same shape as `larvae fmt` by design: the same path
handling, the same default target, the same parallel walk. A user who ran one
command does not have to learn the other.

The exit code is important. A warning reports and exits zero. An error exits
one. So a project decides what fails CI with severity levels and not with a
different command.
*/

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result};
use rayon::prelude::*;

use crate::commands::fmt::{collect, worm_pool};
use crate::config::Config;
use crate::diag::{Diag, Severity};
use crate::lint::{LintConfig, lint, registry};
use crate::ui;
use crate::worm::pool::Pool;

pub fn run(
    root: &Path,
    paths: Vec<PathBuf>,
    stdin: bool,
    explain: Option<String>,
    config: Option<PathBuf>,
) -> Result<ExitCode> {
    if let Some(name) = explain {
        return explain_lint(&name);
    }

    let cfg = discover(root, config.clone())?;

    // Stdin has no file path that routes to a worm, so stdin lints only Luau.
    if stdin {
        return from_stdin(&cfg);
    }

    let pool = worm_pool(root, config)?;
    let files = collect(root, &paths, &cfg.excludes(root)?, &pool.lint_claimed())?;

    if files.is_empty() {
        ui::print_error("no Luau files found");

        return Ok(ExitCode::FAILURE);
    }

    let mut diags: Vec<Diag> = files
        .par_iter()
        .flat_map(|path| match std::fs::read_to_string(path) {
            Ok(src) => match one(path, &src, &cfg, &pool) {
                Ok(found) => found,

                Err(e) => vec![e],
            },

            Err(e) => vec![Diag::error(path, format!("cannot read, {e}"))],
        })
        .collect();

    diags.sort_by(|a, b| a.file.cmp(&b.file).then(a.line_col.cmp(&b.line_col)));

    report(&diags, files.len())
}

/*
The diagnostics of one file, from the owner of its extension.

The worm lints a claimed file when the worm declares lints. Otherwise the
function skips the file without a message. The walk returns claimed files
only for worms that lint, so only a named file reaches the quiet arm. A named
file that a linter cannot check is not an error, unlike the same case in the
formatter. The linter simply has nothing to report.
*/
fn one(path: &Path, src: &str, cfg: &LintConfig, pool: &Pool) -> Result<Vec<Diag>, Diag> {
    let Some(index) = pool.frontend_for(path) else {
        return lint(path, src, cfg);
    };

    let spec = pool.spec(index);
    let name = &spec.manifest.name;
    let mut diags = Vec::new();

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
                .map_err(|e| Diag::error(path, format!("worm `{name}`, {e:#}")))?,
        };

        if let Some(text) = view {
            let view = match reply.as_ref().and_then(|r| r.luau.as_ref()).is_some() {
                true => crate::lint::LuauView::Shadow(&text),

                false => crate::lint::LuauView::Projection(&text),
            };

            diags.extend(crate::lint::inherited(path, src, &view, cfg, name)?);
        }
    }

    if let Some(reply) = reply {
        diags.extend(crate::lint::from_worm(
            path,
            src,
            reply,
            cfg,
            &spec.manifest.lints,
            name,
        )?);
    }

    diags.sort_by_key(|d| d.line_col);

    Ok(diags)
}

fn discover(root: &Path, config: Option<PathBuf>) -> Result<LintConfig> {
    let path = config.unwrap_or_else(|| root.join("larvae.toml"));

    let larvae = match path.exists() {
        true => Config::load(&path)?.lint,

        false => None,
    };

    LintConfig::discover(root, larvae.as_ref())
}

/// One file over stdin and stdout; an editor uses this path
fn from_stdin(cfg: &LintConfig) -> Result<ExitCode> {
    let mut src = String::new();
    std::io::stdin()
        .read_to_string(&mut src)
        .context("cannot read stdin")?;

    let path = Path::new("stdin");

    let diags = match lint(path, &src, cfg) {
        Ok(found) => found,

        Err(e) => vec![e],
    };

    report(&diags, 1)
}

/// `--explain <name>` shows the details of a finding in the terminal
fn explain_lint(name: &str) -> Result<ExitCode> {
    if let Some(found) = crate::lint::find(name) {
        println!("{}\n  {}", found.name(), found.about());
        println!("  default: {:?}", found.default_level());

        return Ok(ExitCode::SUCCESS);
    }

    ui::print_error(&format!("no lint called {name}"));
    println!("\navailable lints:");

    // The list is sorted for lookup, and the width keeps the second column aligned.
    let mut all: Vec<_> = registry().iter().collect();
    all.sort_by_key(|l| l.name());

    let width = all.iter().map(|l| l.name().len()).max().unwrap_or(0);

    for lint in all {
        println!("  {:<width$}  {}", lint.name(), lint.about());
    }

    Ok(ExitCode::FAILURE)
}

fn files(n: usize) -> String {
    match n {
        1 => "1 file".to_string(),

        n => format!("{n} files"),
    }
}

fn report(diags: &[Diag], scanned: usize) -> Result<ExitCode> {
    let color = ui::want_color();

    /*
    The report renders into one buffer, and one write sends it.

    A `println!` for each diagnostic takes the stdout lock each time. A run
    over a large project produces thousands of diagnostics. A measurement on a
    367 file corpus with 3952 diagnostics went from 74ms to 44ms. The change
    affects only how the text reaches the terminal.
    */
    let mut buffer = String::with_capacity(diags.len() * 128);

    for diag in diags {
        buffer.push_str(&diag.render(color));
        buffer.push('\n');
    }

    {
        use std::io::Write;

        let stdout = std::io::stdout();
        let mut out = stdout.lock();

        // A closed pipe, for example `larvae lint | head`, is a normal end and not a failure.
        if let Err(e) = out.write_all(buffer.as_bytes())
            && e.kind() != std::io::ErrorKind::BrokenPipe
        {
            return Err(e.into());
        }
    }

    let errors = diags
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .count();

    let warnings = diags.len() - errors;

    if diags.is_empty() {
        ui::print_success(&format!("{}, nothing to report", files(scanned)));

        return Ok(ExitCode::SUCCESS);
    }

    println!("\n{}, {errors} errors, {warnings} warnings", files(scanned));

    /*
    Only an error fails the run. A project that wants a warning to fail CI
    raises the warning to deny in its config. This keeps the decision in one
    place and does not split it between the config and the command line.
    */
    match errors {
        0 => Ok(ExitCode::SUCCESS),

        _ => Ok(ExitCode::FAILURE),
    }
}
