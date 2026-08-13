/*!
Watch mode: the watcher processes the tree again on each change.

Errors never remove the output. A file that fails to lex keeps its last good
output on disk. So a save of incomplete code does not spread require failures
through a live rojo session. The cache makes each rebuild cheap.
*/

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::mpsc;
use std::time::Duration;

use anyhow::{Context, Result};
use notify_debouncer_full::new_debouncer;
use notify_debouncer_full::notify::RecursiveMode;
use notify_debouncer_full::notify::event::{EventKind, ModifyKind};

use crate::config::Config;
use crate::pipeline;
use crate::rules::Family;
use crate::ui;

/// Merge a burst of editor writes into one rebuild
const DEBOUNCE: Duration = Duration::from_millis(150);

pub fn run(root: &Path, config: &Config, config_path: Option<PathBuf>) -> Result<ExitCode> {
    let color = ui::want_color_stderr();
    build_once(root, config, color);

    let (tx, rx) = mpsc::channel();
    let mut debouncer = new_debouncer(DEBOUNCE, None, tx).context("could not start the watcher")?;

    // Watch every input root; a project can have more than one.
    for dir in config.process.inputs() {
        let input = root.join(&dir);
        debouncer
            .watch(&input, RecursiveMode::Recursive)
            .with_context(|| format!("cannot watch {}", ui::rel(&input)))?;
    }
    // The project file and .luaurc files change resolution for every file.
    for extra in watched_roots(root, config, config_path.as_deref()) {
        let _ = debouncer.watch(&extra, RecursiveMode::NonRecursive);
    }

    let watching: Vec<String> = config
        .process
        .inputs()
        .iter()
        .map(|d| ui::rel(&root.join(d)))
        .collect();

    eprintln!(
        "{} {} for changes, press Ctrl-C to stop",
        ui::accent("watching", color),
        watching.join(", ")
    );

    let output = root.join(&config.process.output);
    let cache_dir = root.join(&config.process.cache_dir);

    for events in rx {
        let Ok(events) = events else { continue };
        let relevant = events.iter().any(|e| {
            // A read of the sources is not a change. Without this filter, the reads
            // from larvae would wake the watcher, and the watcher would loop forever.
            let interesting = matches!(
                e.kind,
                EventKind::Create(_)
                    | EventKind::Remove(_)
                    | EventKind::Modify(
                        ModifyKind::Data(_) | ModifyKind::Name(_) | ModifyKind::Any
                    )
            );
            interesting
                && e.paths
                    .iter()
                    .any(|p| !p.starts_with(&output) && !p.starts_with(&cache_dir))
        });

        if !relevant {
            continue;
        }

        build_once(root, config, color);
    }

    Ok(ExitCode::SUCCESS)
}

fn build_once(root: &Path, config: &Config, color: bool) {
    match pipeline::run(root, config, true) {
        Ok(outcome) => {
            for d in &outcome.diags {
                eprintln!("{}", d.render(color));
            }

            let s = &outcome.stats;
            let cached = if s.files_cached > 0 {
                format!(", {} unchanged", s.files_cached)
            } else {
                String::new()
            };

            let pruned = if s.files_pruned > 0 {
                format!(", {} stale removed", s.files_pruned)
            } else {
                String::new()
            };

            if outcome.has_errors() {
                ui::print_error(&format!(
                    "build finished with errors, {} file(s){cached}{pruned}",
                    s.files_processed
                ));
            } else {
                let mut rules = String::new();

                for (family, label) in
                    [(Family::Native, "native"), (Family::Extension, "extension")]
                {
                    let n = s.applied(family);

                    if n > 0 {
                        rules.push_str(&format!(", {n} {label} rule(s) applied"));
                    }
                }

                ui::print_success(&format!(
                    "{} file(s), {} require(s) rewritten{rules}{cached}{pruned}",
                    s.files_processed, s.requires_rewritten
                ));
            }
        }

        Err(e) => ui::print_error(&format!("{e:#}")),
    }
}

/// Files outside the input tree that also invalidate a build
fn watched_roots(root: &Path, config: &Config, config_path: Option<&Path>) -> Vec<PathBuf> {
    let mut out = Vec::new();

    if let Some(p) = config_path {
        out.push(root.join(p));
    } else {
        out.push(root.join("larvae.toml"));
    }

    out.push(
        config
            .rojo
            .project
            .as_ref()
            .map(|p| root.join(p))
            .unwrap_or_else(|| root.join("default.project.json")),
    );
    out.push(root.join(".luaurc"));
    out.retain(|p| p.exists());

    out
}
