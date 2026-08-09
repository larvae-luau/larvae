/*!
`larvae fmt`.

Three ways to run, and they exist for three different callers. With paths or
none it formats files in place, which is what a person types. With `--check` it
writes nothing and fails if anything would change, which is what CI runs. With
`--stdin` it reads one file's text and writes the result, which is what an
editor calls on every save and the reason that path allocates nothing it does
not need.

Files are formatted in parallel and only rewritten when the bytes actually
change, so a run over an already formatted tree touches no mtimes and triggers
no watcher.
*/

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result};
use rayon::prelude::*;

use crate::config::{Config, Excludes};
use crate::fmt::{FmtConfig, format};
use crate::ui;

/// What happened to one file
enum Outcome {
    Unchanged,
    Changed,
    Failed(String),
}

pub fn run(
    root: &Path,
    paths: Vec<PathBuf>,
    check: bool,
    stdin: bool,
    config: Option<PathBuf>,
) -> Result<ExitCode> {
    let cfg = discover(root, config)?;

    if stdin {
        return from_stdin(&cfg);
    }

    let files = collect(root, &paths, &cfg.excludes(root)?)?;

    if files.is_empty() {
        ui::print_error("no Luau files found");

        return Ok(ExitCode::FAILURE);
    }

    let outcomes: Vec<(PathBuf, Outcome)> = files
        .into_par_iter()
        .map(|path| {
            let outcome = one(&path, &cfg, check);

            (path, outcome)
        })
        .collect();

    report(outcomes, check)
}

/*
The `[fmt]` table, over a `stylua.toml` if the project still has one.

`larvae.toml` is optional here on purpose. Formatting a directory should not
require a project file, since the first thing someone does with a formatter is
point it at a folder to see what it does.
*/
fn discover(root: &Path, config: Option<PathBuf>) -> Result<FmtConfig> {
    let path = config.unwrap_or_else(|| root.join("larvae.toml"));

    let larvae = match path.exists() {
        true => Config::load(&path)?.fmt,

        false => None,
    };

    FmtConfig::discover(root, larvae.as_ref())
}

fn one(path: &Path, cfg: &FmtConfig, check: bool) -> Outcome {
    let src = match std::fs::read_to_string(path) {
        Ok(src) => src,

        Err(e) => return Outcome::Failed(format!("cannot read, {e}")),
    };

    let out = match format(&src, cfg) {
        Ok(out) => out,

        Err(e) => return Outcome::Failed(format!("{e:#}")),
    };

    if out == src {
        return Outcome::Unchanged;
    }

    if check {
        return Outcome::Changed;
    }

    match std::fs::write(path, out) {
        Ok(()) => Outcome::Changed,

        Err(e) => Outcome::Failed(format!("cannot write, {e}")),
    }
}

/// One file over the pipes, which is how an editor asks
fn from_stdin(cfg: &FmtConfig) -> Result<ExitCode> {
    let mut src = String::new();
    std::io::stdin()
        .read_to_string(&mut src)
        .context("cannot read stdin")?;

    let out = format(&src, cfg)?;

    print!("{out}");

    Ok(ExitCode::SUCCESS)
}

/*
Which files to format.

Explicit paths win, and a named file is formatted whatever it is called, since
someone naming a file means it. Without paths the project's input directory is
walked when there is a config, and the working directory otherwise.

`exclude` applies to what a walk turns up, named directories included, and not
to a file somebody named themselves. That is the same line: a walk is us
guessing, a name is somebody telling us.
*/
pub fn collect(root: &Path, paths: &[PathBuf], excludes: &Excludes) -> Result<Vec<PathBuf>> {
    let walked = |dir: &Path| {
        walk(dir)
            .into_iter()
            .filter(|p| !excludes.skips(p))
            .collect::<Vec<_>>()
    };

    if paths.is_empty() {
        return Ok(default_roots(root).iter().flat_map(|d| walked(d)).collect());
    }

    let mut out = Vec::new();

    for path in paths {
        let path = match path.is_absolute() {
            true => path.clone(),

            false => root.join(path),
        };

        if path.is_dir() {
            out.extend(walked(&path));
        } else if path.exists() {
            out.push(path);
        } else {
            anyhow::bail!("no such path, {}", ui::rel(&path));
        }
    }

    out.sort();
    out.dedup();

    Ok(out)
}

fn default_roots(root: &Path) -> Vec<PathBuf> {
    let path = root.join("larvae.toml");

    if !path.exists() {
        return vec![root.to_path_buf()];
    }

    match Config::load(&path) {
        Ok(config) => config
            .process
            .inputs()
            .iter()
            .map(|dir| root.join(dir))
            .collect(),

        // a config too broken to read is not a reason to refuse to format
        Err(_) => vec![root.to_path_buf()],
    }
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    walkdir::WalkDir::new(dir)
        .into_iter()
        .filter_entry(|e| {
            // a build output or a package tree is not the project's to format
            !matches!(
                e.file_name().to_str(),
                Some(".git" | "node_modules" | ".larvae")
            )
        })
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .map(walkdir::DirEntry::into_path)
        /*
        Only what larvae itself can read, for now.
        A front-end worm claiming `.luaux` should widen this, and deliberately
        does not yet: nothing can format a claimed file, so walking one would
        parse it as Luau, fail, and turn a passing `fmt --check` into a failing
        one for every project using a front-end. The filter widens in the same
        change that gives worms something to format with.
        */
        .filter(|p| matches!(p.extension().and_then(|e| e.to_str()), Some("luau" | "lua")))
        .collect()
}

fn files(n: usize) -> String {
    match n {
        1 => "1 file".to_string(),

        n => format!("{n} files"),
    }
}

fn report(outcomes: Vec<(PathBuf, Outcome)>, check: bool) -> Result<ExitCode> {
    let mut changed = Vec::new();
    let mut failed = Vec::new();
    let mut clean = 0usize;

    for (path, outcome) in outcomes {
        match outcome {
            Outcome::Unchanged => clean += 1,

            Outcome::Changed => changed.push(path),

            Outcome::Failed(why) => failed.push((path, why)),
        }
    }

    for (path, why) in &failed {
        ui::print_error(&format!("{}, {why}", ui::rel(path)));
    }

    if check {
        for path in &changed {
            println!("would reformat {}", ui::rel(path));
        }

        if changed.is_empty() && failed.is_empty() {
            ui::print_success(&format!("{} already formatted", files(clean)));

            return Ok(ExitCode::SUCCESS);
        }

        return Ok(ExitCode::FAILURE);
    }

    if failed.is_empty() {
        ui::print_success(&format!(
            "formatted {}, {clean} unchanged",
            files(changed.len())
        ));

        return Ok(ExitCode::SUCCESS);
    }

    Ok(ExitCode::FAILURE)
}
