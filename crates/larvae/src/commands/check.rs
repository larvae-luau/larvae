//! `larvae check` validates all requires and does not write output.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::Result;

use crate::commands::process::{load_config, report};
use crate::pipeline;

pub fn run(root: &Path, config: Option<PathBuf>, profile: Option<String>) -> Result<ExitCode> {
    let config = load_config(root, config, profile.as_deref())?;
    let mut outcome = pipeline::run(root, &config, false)?;

    /*
    The whole project checks. Each one needs the full graph, so they run
    after the pipeline resolves every file. `process` does not run them:
    they are a gate, not a part of producing output, and a cycle does not
    stop a build.

    The graph keys on the canonical root, so the entries in `[check]` must
    join onto the same form.
    */
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());

    outcome.diags.extend(crate::requires::analyse::run(
        &outcome.graph,
        &config.check,
        &config.requires,
        &root,
    ));

    crate::diag::sort(&mut outcome.diags);

    report(&outcome, false)
}
