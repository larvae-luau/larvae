//! `larvae bundle` collects the modules of the project into one file.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, bail};

use crate::commands::process::load_config;
use crate::{bundle, pipeline, ui};

pub fn run(
    root: &Path,
    entry: Option<PathBuf>,
    out: Option<PathBuf>,
    config: Option<PathBuf>,
    profile: Option<String>,
) -> Result<ExitCode> {
    let config = load_config(root, config, profile.as_deref())?;

    /*
    The command line beats the config, in the same order every other command
    uses. So a project keeps one entry configured, and bundles a different
    one once without an edit.
    */
    let entry = entry
        .or_else(|| config.bundle.entry.clone())
        .context("no bundle entry, pass --entry or set [bundle] entry in larvae.toml")?;

    let out = out
        .or_else(|| config.bundle.output.clone())
        .unwrap_or_else(|| PathBuf::from("bundle.luau"));

    // The graph keys on the canonical root, so the entry and the module ids
    // must join onto the same form.
    let root = root
        .canonicalize()
        .with_context(|| format!("cannot resolve project root {}", root.display()))?;

    /*
    Resolution runs first and writes nothing. The bundle needs the same
    require graph that `check` uses, and a second resolution is a second
    chance for the bundle and the diagnostics to disagree about what
    requires what. The run also keeps the processed text of each module; see
    [`pipeline::Outcome::sources`] for why the files on disk are not the
    source of truth.
    */
    let resolved = pipeline::run_keeping_sources(&root, &config)?;

    let color = ui::want_color_stderr();

    for diag in &resolved.diags {
        eprintln!("{}", diag.render(color));
    }

    if resolved.has_errors() {
        bail!("the project does not resolve cleanly, so larvae cannot bundle it");
    }

    let mut plan = bundle::plan(
        &root,
        &root.join(&entry),
        &resolved.graph,
        config.bundle.tree_shake,
    )?;
    let mut diags = std::mem::take(&mut plan.diags);
    let mut modules = Vec::new();

    for path in bundle::emit::emission_order(&plan.graph, &plan.modules) {
        /*
        The processed text of the pipeline is the source of truth: a
        front-end worm compiles a claimed file in there, and the require
        site spans index that exact text. A module without processed text
        sat outside the input roots, so the pipeline never touched it and no
        worm claims it; for that module, the file on disk is the text the
        project runs.
        */
        let src = match resolved.sources.get(&path) {
            Some(text) => text.clone(),

            None => read_module(&path)?,
        };

        modules.push(bundle::emit::Module {
            id: plan.modules[&path].clone(),
            source: bundle::rewrite(&src, &path, &plan, &mut diags)?,
        });
    }

    crate::diag::sort(&mut diags);

    for diag in &diags {
        eprintln!("{}", diag.render(color));
    }

    let entry_id = plan
        .modules
        .get(&plan.entry)
        .context("the entry is not in its own plan")?;

    let text = bundle::emit::write(&modules, entry_id);
    let out_path = root.join(&out);

    if let Some(parent) = out_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("cannot create {}", ui::rel(parent)))?;
    }

    std::fs::write(&out_path, &text)
        .with_context(|| format!("cannot write {}", ui::rel(&out_path)))?;

    let shaken = resolved.graph.nodes().count().saturating_sub(modules.len());

    ui::print_success(&format!(
        "bundled {} modules into {}{}",
        modules.len(),
        ui::rel(&out_path),
        match shaken {
            0 => String::new(),

            n => format!(", {n} unreachable left out"),
        }
    ));

    Ok(ExitCode::SUCCESS)
}

/// A node outside the pipeline; a directory node stands for its init file
fn read_module(node: &Path) -> Result<String> {
    let file = if node.is_dir() {
        ["init.luau", "init.lua"]
            .iter()
            .map(|name| node.join(name))
            .find(|p| p.is_file())
            .with_context(|| format!("no init file in {}", ui::rel(node)))?
    } else {
        node.to_path_buf()
    };

    std::fs::read_to_string(&file).with_context(|| format!("cannot read {}", ui::rel(&file)))
}
