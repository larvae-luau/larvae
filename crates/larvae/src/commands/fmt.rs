/*!
`larvae fmt`.

The command has three modes, one for each type of caller. With paths or no
paths, the command formats files in place; a user types this. With `--check`,
the command writes nothing and fails if a file would change; CI runs this.
With `--stdin`, the command reads the text of one file and writes the result.
An editor calls this mode on every save, so this path allocates only what it
needs.

The command formats files in parallel and rewrites a file only when the bytes
change. So a run over a formatted tree changes no mtimes and does not activate
a file watcher.
*/

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result};
use rayon::prelude::*;

use crate::config::{Config, Excludes};
use crate::fmt::{FmtConfig, format};
use crate::ui;
use crate::worm::{pool::Pool, proto, registry::Registry};

/// The result for one file
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
    stdin_filepath: Option<PathBuf>,
    config: Option<PathBuf>,
) -> Result<ExitCode> {
    let mut cfg = discover(root, config.clone())?;

    /*
    Stdin alone has no file path, so it formats only Luau. An editor that
    pipes a claimed file names the path with --stdin-filepath, and the pool
    routes on it exactly as a walk does.
    */
    if stdin {
        let pool = match &stdin_filepath {
            Some(_) => worm_pool(root, config, &mut cfg)?,

            None => Pool::new(Vec::new(), 1),
        };

        return from_stdin(&cfg, stdin_filepath.as_deref(), &pool);
    }

    let pool = worm_pool(root, config.clone(), &mut cfg)?;
    let (root_in, root_ex) = root_lists(root, config)?;
    let files = collect(
        root,
        &paths,
        &cfg.excludes_under(root, &root_in, &root_ex)?,
        &pool.fmt_claimed(),
    )?;

    if files.is_empty() {
        ui::print_error("no Luau files found");

        return Ok(ExitCode::FAILURE);
    }

    let outcomes: Vec<(PathBuf, Outcome)> = files
        .into_par_iter()
        .map(|path| {
            let outcome = one(&path, &cfg, check, &pool);

            (path, outcome)
        })
        .collect();

    report(outcomes, check)
}

/*
The root level include and exclude, which every area inherits.

The lists live on the whole config and not on one area, so the two commands
that walk a tree read them through this one helper. A project without a
config file has empty lists, which cost nothing.
*/
pub fn root_lists(root: &Path, config: Option<PathBuf>) -> Result<(Vec<String>, Vec<String>)> {
    let path = config.unwrap_or_else(|| root.join("larvae.toml"));

    if !path.exists() {
        return Ok((Vec::new(), Vec::new()));
    }

    let cfg = Config::load(&path)?;

    Ok((cfg.include, cfg.exclude))
}

/*
The worms of the project, for the commands that walk a tree.

A project with no config or no `[worms]` gets an empty pool at no cost. So
`larvae fmt` on a bare directory keeps its old behavior. A pinned worm on a
cold cache does a fetch here, the same as `larvae process`.
*/
pub fn worm_pool(root: &Path, config: Option<PathBuf>, fmt: &mut FmtConfig) -> Result<Pool> {
    pool_with(root, config, fmt, crate::worm::registry::Fetch::Report)
}

/*
The same, with a choice about the network.

A command in the terminal fetches a worm the cache does not hold. The editor
does not, because a keystroke cannot wait for a download.
*/
pub fn pool_with(
    root: &Path,
    config: Option<PathBuf>,
    fmt: &mut FmtConfig,
    fetch: crate::worm::registry::Fetch,
) -> Result<Pool> {
    let path = config.unwrap_or_else(|| root.join("larvae.toml"));

    if !path.exists() {
        /*
        No project, so no worm can own a key. A key larvae does not own is
        then a mistake, and the same message says so.
        */
        Registry::default().resolve_fmt(fmt)?;

        return Ok(Pool::new(Vec::new(), 1));
    }

    let cfg = Config::load(&path)?;

    let registry = match fetch {
        crate::worm::registry::Fetch::Allowed => Registry::for_project(root, &cfg)?,

        crate::worm::registry::Fetch::Quiet => Registry::for_project_cached(root, &cfg)?,

        crate::worm::registry::Fetch::Report => Registry::for_project_reporting(root, &cfg)?,
    };

    // the worms of a project decide which `[fmt]` keys are real
    registry.resolve_fmt(fmt)?;

    let lint = crate::lint::LintConfig::discover(root, cfg.lint.as_ref())?;

    Ok(Pool::with_settings(
        registry.specs(),
        cfg.process.run_order,
        crate::worm::Settings::new(fmt, &lint),
    ))
}

/*
The `[fmt]` table, layered over a `stylua.toml` if the project has one.

`larvae.toml` is optional here by design. A format of a directory must not
require a project file. The first action of a new user is to point the
formatter at a folder to see the result.
*/
fn discover(root: &Path, config: Option<PathBuf>) -> Result<FmtConfig> {
    let path = config.unwrap_or_else(|| root.join("larvae.toml"));

    let larvae = match path.exists() {
        true => Config::load(&path)?.fmt,

        false => None,
    };

    FmtConfig::discover(root, larvae.as_ref())
}

fn one(path: &Path, cfg: &FmtConfig, check: bool, pool: &Pool) -> Outcome {
    let src = match std::fs::read_to_string(path) {
        Ok(src) => src,

        Err(e) => return Outcome::Failed(format!("cannot read, {e}")),
    };

    let out = match formatted(path, &src, cfg, pool) {
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

/*
The formatted text of one file, from the owner of its extension.

A claimed file goes to its worm. The worm replies with a layout document, and
larvae renders it in the style of the project. A walk returns claimed files
only when the worm formats. So only a named file reaches the error arm here.
A named file that no tool can format gets an error message and not silence.
*/
fn formatted(path: &Path, src: &str, cfg: &FmtConfig, pool: &Pool) -> Result<String> {
    let Some(index) = pool.frontend_for(path) else {
        return format(src, cfg);
    };

    let spec = pool.spec(index);

    if !spec.formats() {
        anyhow::bail!(
            "worm `{}` claims this file but does not format it",
            spec.manifest.name
        );
    }

    let reply = pool.format(index, src)?;

    // a project can keep one option out of the files this worm claims
    let cfg = cfg.without(&spec.inherit.fmt_except);

    proto::render_format(src, &reply, &cfg)
        .with_context(|| format!("worm `{}`", spec.manifest.name))
}

/// One file over stdin and stdout; an editor uses this path
fn from_stdin(cfg: &FmtConfig, path: Option<&Path>, pool: &Pool) -> Result<ExitCode> {
    let mut src = String::new();
    std::io::stdin()
        .read_to_string(&mut src)
        .context("cannot read stdin")?;

    let out = match path {
        Some(path) => formatted(path, &src, cfg, pool)?,

        None => format(&src, cfg)?,
    };

    print!("{out}");

    Ok(ExitCode::SUCCESS)
}

/*
The files to format.

Explicit paths win. The command formats a named file with any name, because a
user who names a file wants it formatted. Without paths, the command walks the
input directory of the project when a config exists, and the working directory
otherwise.

`exclude` applies to the files that a walk finds, named directories included.
`exclude` does not apply to a file that the user named. The rule is the same
in both cases: a walk is a guess by larvae, and a name is an instruction from
the user.
*/
pub fn collect(
    root: &Path,
    paths: &[PathBuf],
    excludes: &Excludes,
    claimed: &[String],
) -> Result<Vec<PathBuf>> {
    let walked = |dir: &Path| {
        walk(dir, claimed)
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

        // A config that larvae cannot read does not stop the format.
        Err(_) => vec![root.to_path_buf()],
    }
}

fn walk(dir: &Path, claimed: &[String]) -> Vec<PathBuf> {
    walkdir::WalkDir::new(dir)
        .into_iter()
        .filter_entry(|e| {
            // The project does not own a build output or a package tree, so skip them.
            !matches!(
                e.file_name().to_str(),
                Some(".git" | "node_modules" | ".larvae")
            )
        })
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .map(walkdir::DirEntry::into_path)
        /*
        The extensions that larvae reads, plus the claimed extensions that
        the worms of the caller can format. The caller passes the list
        filtered by capability, so the walk skips the files of a
        frontend-only worm. If the walk kept such a file, larvae would parse
        it as Luau and fail. Then a passing `fmt --check` would become a
        failing one.
        */
        .filter(|p| {
            let Some(ext) = p.extension().and_then(|e| e.to_str()) else {
                return false;
            };

            matches!(ext, "luau" | "lua") || claimed.iter().any(|c| c == ext)
        })
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
