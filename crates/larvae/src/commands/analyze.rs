/*!
`larvae analyze`: type diagnostics in the terminal, from the same
analyzer the editor runs.

The engine lives here and the analyzer lives in the `larvae-lsp`
binary, so the command has two halves. `larvae analyze` finds the
server binary and hands the arguments over; the binary builds the
session and calls [`engine`]. The session mirrors the editor's: the
platform globals, the `[lsp] definitions` files, the worms' lowering
hooks, the DataModel mounts, and the sourcemap tree, so a require
types here exactly as it types under the cursor.
*/

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result, bail};

use crate::commands::fmt::{collect, worm_pool};
use crate::config::Config;
use crate::diag::{Diag, Severity};

/// What the argument list said, parsed by [`parse`] on either half
#[derive(Debug, Default, Clone)]
pub struct Options {
    /// Files or directories; the default is the input of the project
    pub paths: Vec<PathBuf>,
    /// Extra definition files, on top of `[lsp] definitions`
    pub definitions: Vec<PathBuf>,
    /// A config file other than ./larvae.toml
    pub config: Option<PathBuf>,
}

/*
The flat argument list, shared by the spawner and the server binary.

Clap owns the `larvae` side, so this parser exists for the `larvae-lsp`
side, and the spawner emits exactly what it reads: paths, and the two
options below.
*/
pub fn parse(args: &[String]) -> Result<Options> {
    let mut opts = Options::default();
    let mut it = args.iter();

    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--definitions" => {
                let value = it.next().context("--definitions takes a path")?;

                opts.definitions.push(PathBuf::from(value));
            }

            "--config" => {
                let value = it.next().context("--config takes a path")?;

                opts.config = Some(PathBuf::from(value));
            }

            other if other.starts_with("--") => bail!("unknown option {other}"),

            path => opts.paths.push(PathBuf::from(path)),
        }
    }

    Ok(opts)
}

/// The project's `[lsp]` config, for the flags the caller applies first
pub fn lsp_config(opts: &Options) -> Result<crate::config::lsp::LspConfig> {
    let root = std::env::current_dir()?;
    let path = opts
        .config
        .clone()
        .unwrap_or_else(|| root.join("larvae.toml"));

    Ok(match path.exists() {
        true => Config::load(&path)?.lsp,

        false => Default::default(),
    })
}

/*
Run the analyzer over the files and print what it reports.

The analysis arrives from the caller, because only the server binary
holds one. The flags are already applied: they are process wide in
Luau and decide what a session means, so the caller sets them before
the session exists, the same order the editor keeps.
*/
pub fn engine(
    mut analysis: Box<dyn crate::lsp::analysis::Analysis>,
    opts: &Options,
) -> Result<ExitCode> {
    let root = std::env::current_dir()?;
    let lsp = lsp_config(opts)?;
    let cfg_path = opts
        .config
        .clone()
        .unwrap_or_else(|| root.join("larvae.toml"));
    let project = cfg_path
        .exists()
        .then(|| Config::load(&cfg_path))
        .transpose()?;

    analysis.set_character_type(lsp.character_type);

    // The definition files: the config's list, then the flag's, in order.
    for entry in lsp
        .definitions
        .iter()
        .map(|e| root.join(e))
        .chain(opts.definitions.iter().cloned())
    {
        let shown = crate::ui::rel(&entry);
        let text = std::fs::read_to_string(&entry)
            .with_context(|| format!("cannot read the definitions file {shown}"))?;

        if !analysis.definitions(&format!("@user/{shown}"), &text) {
            bail!("Luau refused the definitions file {shown}");
        }
    }

    /*
    The worms, so a require of a claimed file types through its
    lowering, and the declarations each worm injects.
    */
    let mut fmt = crate::fmt::FmtConfig::default();
    let pool = worm_pool(&root, opts.config.clone(), &mut fmt)?;

    if pool.has_lsp_hooks() {
        let resolve_pool = pool.clone();
        let load_pool = pool.clone();

        analysis.set_module_hooks(crate::lsp::analysis::ModuleHooks {
            resolve: Box::new(move |from, spec| {
                resolve_pool.lsp_resolve(&from.to_string_lossy(), spec)
            }),
            load: Box::new(move |path| {
                load_pool
                    .lsp_load_any(path)
                    .map(|r| crate::lsp::analysis::plain_view(&r.source).into_owned())
            }),
            claims: pool.lsp_resolved_claims(),
        });

        for decl in pool.lsp_declarations() {
            analysis.definitions(&decl.name, &decl.source);
        }
    }

    // The DataModel map, so `@game` and the instance forms resolve.
    {
        let fallback = Config::default();
        let cfg = project.as_ref().unwrap_or(&fallback);
        let rojo = crate::project::rojo::find_project(&root, cfg.rojo.project.as_deref())
            .and_then(|path| crate::project::rojo::load(&path).ok());
        let mut ignored = Vec::new();

        analysis.set_mounts(crate::pipeline::setup::mount_table(
            &root,
            cfg,
            rojo.as_ref(),
            &mut ignored,
        ));
    }

    // The sourcemap tree, so `script` carries its own type.
    let sourcemap = root.join(&lsp.sourcemap);

    if sourcemap.is_file() {
        let read = crate::lsp::instances::read(&sourcemap, &root, 1, &pool.claimed());

        if !read.definitions.is_empty() {
            analysis.definitions("@sourcemap", &read.definitions);
            analysis.set_script_types(&read.script_types);
        }
    }

    /*
    The walk: the named paths, or the whole project. Claimed files stay
    out, because the analyzer reads Luau and their own findings belong
    to `larvae lint`; a require into one still types through the hooks.
    */
    let lint_cfg =
        crate::lint::LintConfig::discover(&root, project.as_ref().and_then(|p| p.lint.as_ref()))?;
    let (root_in, root_ex) = crate::commands::fmt::root_lists(&root, opts.config.clone())?;
    let files = collect(
        &root,
        &opts.paths,
        &lint_cfg.excludes_under(&root, &root_in, &root_ex)?,
        &[],
    )?;

    if files.is_empty() {
        crate::ui::print_error("no Luau files found");

        return Ok(ExitCode::FAILURE);
    }

    let mut diags: Vec<Diag> = Vec::new();

    for path in &files {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,

            Err(e) => {
                diags.push(Diag::error(path, format!("cannot read, {e}")));

                continue;
            }
        };

        analysis.open(path, &crate::lsp::analysis::plain_view(&text));

        for d in analysis.check(path) {
            let message = match d.code {
                Some(code) => format!("{} ({code})", d.message),

                None => d.message,
            };

            let mut diag = match d.severity {
                1 => Diag::error(path, message),

                _ => Diag::warning(path, message),
            };

            diag.line_col = Some(crate::diag::line_col(&text, d.span.0 as usize));
            diags.push(diag);
        }
    }

    diags.sort_by(|a, b| a.file.cmp(&b.file).then(a.line_col.cmp(&b.line_col)));

    let color = crate::ui::want_color();
    let mut buffer = String::with_capacity(diags.len() * 128);

    for diag in &diags {
        buffer.push_str(&diag.render(color));
        buffer.push('\n');
    }

    print!("{buffer}");

    let errors = diags
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .count();
    let warnings = diags.len() - errors;

    println!(
        "{} file(s), {errors} error(s), {warnings} warning(s)",
        files.len()
    );

    Ok(match errors {
        0 => ExitCode::SUCCESS,

        _ => ExitCode::FAILURE,
    })
}

/*
The `larvae` half: find the server binary and hand the arguments over.

The analyzer is compiled into `larvae-lsp`, not into this binary, and
the release zips ship the pair together. The search order is the
directory of the running binary, then the install directory, then the
PATH, which is the order the pieces travel in.
*/
pub fn spawn(opts: &Options) -> Result<ExitCode> {
    let name = match cfg!(windows) {
        true => "larvae-lsp.exe",

        false => "larvae-lsp",
    };

    let beside = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join(name)))
        .filter(|p| p.is_file());

    let installed = crate::sys::paths::bin_dir()
        .ok()
        .map(|dir| dir.join(name))
        .filter(|p| p.is_file());

    let program = beside.or(installed).unwrap_or_else(|| PathBuf::from(name));

    let mut command = std::process::Command::new(&program);
    command.arg("analyze");

    for path in &opts.paths {
        command.arg(path);
    }

    for def in &opts.definitions {
        command.arg("--definitions").arg(def);
    }

    if let Some(config) = &opts.config {
        command.arg("--config").arg(config);
    }

    let status = command.status().with_context(|| {
        format!(
            "cannot start {}; `larvae self install` from a release puts the analyzer beside larvae",
            crate::ui::rel(&program)
        )
    })?;

    Ok(match status.success() {
        true => ExitCode::SUCCESS,

        false => ExitCode::FAILURE,
    })
}

#[cfg(test)]
mod tests {
    use super::parse;

    /// The flat list reads the same on both halves of the command.
    #[test]
    fn the_arguments_parse_both_ways() {
        let opts = parse(&[
            "src".into(),
            "--definitions".into(),
            "types/global.d.luau".into(),
            "extra.luau".into(),
            "--config".into(),
            "other.toml".into(),
        ])
        .expect("parses");

        assert_eq!(opts.paths.len(), 2);
        assert_eq!(opts.definitions.len(), 1);
        assert!(opts.config.is_some());

        assert!(parse(&["--definitions".into()]).is_err());
        assert!(parse(&["--mystery".into()]).is_err());
    }
}
