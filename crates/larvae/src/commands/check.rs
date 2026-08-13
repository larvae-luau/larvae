//! `larvae check` validates all requires and does not write output.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::Result;

use crate::commands::process::{load_config, report};
use crate::pipeline;

pub fn run(root: &Path, config: Option<PathBuf>, profile: Option<String>) -> Result<ExitCode> {
    let config = load_config(root, config, profile.as_deref())?;
    let outcome = pipeline::run(root, &config, false)?;

    report(&outcome, false)
}
