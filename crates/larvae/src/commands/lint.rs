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
    stdin_filepath: Option<PathBuf>,
    explain: Option<String>,
    config: Option<PathBuf>,
) -> Result<ExitCode> {
    if let Some(name) = explain {
        return explain_lint(root, config.as_deref(), &name);
    }

    let cfg = discover(root, config.clone())?;

    /*
    A project that turned the linter off gets no report and a zero exit, on
    every path including stdin. The command still runs, so a script or an
    editor that calls it needs no branch of its own.
    */
    if !cfg.is_enabled() {
        return Ok(ExitCode::SUCCESS);
    }

    /*
    Stdin alone has no file path, so it lints only Luau. An editor that pipes
    a claimed file names the path with --stdin-filepath, and the pool routes
    on it exactly as a walk does.
    */
    if stdin {
        return from_stdin(root, &cfg, stdin_filepath.as_deref(), config);
    }

    /*
    The lint command also builds the pool, so a worm receives the same
    settings whichever command started it. The `[fmt]` table of the project
    is read here as well, because a key of a worm lives in that table and
    larvae checks it against the worms it loads.
    */
    let path = config.clone().unwrap_or_else(|| root.join("larvae.toml"));

    let mut fmt = match path.exists() {
        true => {
            let project = Config::load(&path)?;

            crate::fmt::FmtConfig::discover(root, project.fmt.as_ref())?
        }

        false => crate::fmt::FmtConfig::default(),
    };

    let pool = worm_pool(root, config.clone(), &mut fmt)?;
    let (root_in, root_ex) = crate::commands::fmt::root_lists(root, config)?;
    let files = collect(
        root,
        &paths,
        &cfg.excludes_under(root, &root_in, &root_ex)?,
        &pool.lint_claimed(),
    )?;

    if files.is_empty() {
        ui::print_error("no Luau files found");

        return Ok(ExitCode::FAILURE);
    }

    let mut diags: Vec<Diag> = files
        .par_iter()
        .flat_map(|path| match read_source(path) {
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

    let findings = crate::lint::claimed(path, src, cfg, pool, index)?;

    Ok(crate::lint::into_diags(path, src, findings))
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
fn from_stdin(
    root: &Path,
    cfg: &LintConfig,
    path: Option<&Path>,
    config: Option<PathBuf>,
) -> Result<ExitCode> {
    let mut bytes = Vec::new();
    std::io::stdin()
        .read_to_end(&mut bytes)
        .context("cannot read stdin")?;

    // the same stand-in as a file read; a lint pass writes nothing back
    let src = crate::sys::utf8_stand_in(bytes).0;

    let diags = match path {
        Some(path) => {
            let mut fmt = crate::fmt::FmtConfig::default();
            let pool = worm_pool(root, config, &mut fmt)?;

            match one(path, &src, cfg, &pool) {
                Ok(found) => found,

                Err(e) => vec![e],
            }
        }

        None => match lint(Path::new("stdin"), &src, cfg) {
            Ok(found) => found,

            Err(e) => vec![e],
        },
    };

    report(&diags, 1)
}

/// `--explain <name>` shows the details of a finding in the terminal
fn explain_lint(root: &Path, config: Option<&Path>, name: &str) -> Result<ExitCode> {
    if let Some(found) = crate::lint::find(name) {
        println!("{}\n  {}", found.name(), found.about());
        println!("  default: {}", level_name(found.default_level()));
        println!(
            "  group:   {}, so [lint.groups] {} covers it",
            found.group().name(),
            found.group().name()
        );

        return Ok(ExitCode::SUCCESS);
    }

    /*
    A lint of a worm is explained from the manifest that declares it. The
    project has to load its worms to read that text, so this runs only after
    the builtin table misses.
    */
    if let Some((worm, decl)) = worm_lint(root, config, name) {
        println!("{name}");

        match &decl.description {
            Some(text) => println!("  {text}"),

            None => println!("  worm `{worm}` declares this lint and describes it nowhere"),
        }

        println!("  default: {}", level_name(decl.default));
        println!("  from:    worm `{worm}`");

        return Ok(ExitCode::SUCCESS);
    }

    ui::print_error(&format!("no lint called {name}"));

    /*
    The list is printed by group, and each name is sorted inside its group.

    Fifty two names in one alphabetical run is a wall. The group is also what
    a project sets under `[lint.groups]`, so the heading is a config key and
    not decoration.
    */
    let width = registry().iter().map(|l| l.name().len()).max().unwrap_or(0);

    for group in crate::lint::Group::all() {
        let mut of_group: Vec<_> = registry().iter().filter(|l| l.group() == group).collect();

        if of_group.is_empty() {
            continue;
        }

        of_group.sort_by_key(|l| l.name());

        println!("\n{}:", group.name());

        for lint in of_group {
            println!("  {:<width$}  {}", lint.name(), lint.about());
        }
    }

    Ok(ExitCode::FAILURE)
}

/// The worm that declares one lint, with what it declared about it
fn worm_lint(
    root: &Path,
    config: Option<&Path>,
    name: &str,
) -> Option<(String, crate::worm::manifest::LintDecl)> {
    let path = config
        .map(Path::to_path_buf)
        .unwrap_or_else(|| root.join("larvae.toml"));

    if !path.exists() {
        return None;
    }

    let project = Config::load(&path).ok()?;
    let registry = crate::worm::registry::Registry::for_project(root, &project).ok()?;

    /*
    A user writes the name that larvae reports, `luaux.useless_fragment`. The
    worm declares the bare half of it, so the lookup splits the name first and
    then asks the worm that owns the first half.
    */
    let (owner, bare) = name.split_once('.')?;

    let loaded = registry.iter().find(|l| l.worm.name() == owner)?;
    let decl = loaded.worm.manifest.lints.get(bare)?;

    Some((owner.to_owned(), decl.clone()))
}

/// The text of one file, with stand-ins for bytes that are not UTF-8
fn read_source(path: &Path) -> std::io::Result<String> {
    std::fs::read(path).map(|bytes| crate::sys::utf8_stand_in(bytes).0)
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

    let count = |want: Severity| diags.iter().filter(|d| d.severity == want).count();

    let errors = count(Severity::Error);
    let warnings = count(Severity::Warning);
    let infos = count(Severity::Info);

    if diags.is_empty() {
        ui::print_success(&format!("{}, nothing to report", files(scanned)));

        return Ok(ExitCode::SUCCESS);
    }

    /*
    The info count joins the line only when a project uses the level. A
    project that never writes `info` reads the summary it always read.
    */
    let tail = match infos {
        0 => String::new(),

        n => format!(", {n} infos"),
    };

    println!(
        "\n{}, {errors} errors, {warnings} warnings{tail}",
        files(scanned)
    );

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

/// The level as a project writes it, which is not how Rust prints the enum
fn level_name(level: crate::lint::Level) -> &'static str {
    use crate::lint::Level;

    match level {
        Level::Allow => "allow",
        Level::Info => "info",
        Level::Warn => "warn",
        Level::Deny => "deny",
    }
}
