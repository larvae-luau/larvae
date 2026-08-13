//! The module writes results, always atomically; rojo serve watches this tree.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::Result;
use walkdir::WalkDir;

use crate::diag::Diag;

pub(super) fn write_atomic(dest: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let tmp = dest.with_extension("larvae-tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, dest)?;

    Ok(())
}

pub(super) fn copy_atomic(from: &Path, dest: &Path) -> Result<()> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let tmp = dest.with_extension("larvae-tmp");
    std::fs::copy(from, &tmp)?;
    std::fs::rename(&tmp, dest)?;

    Ok(())
}

/*
Remove output files whose source is gone. A guard makes sure that the prune
cannot delete project files when the output path is misconfigured.
*/
pub(super) fn prune_output(
    output: &Path,
    input: &Path,
    root: &Path,
    produced: &HashSet<PathBuf>,
    diags: &mut Vec<Diag>,
) -> usize {
    if !output.is_dir() || output == input || output == root {
        return 0;
    }

    let mut removed = 0;

    for entry in WalkDir::new(output).into_iter().filter_map(|e| e.ok()) {
        if !entry.file_type().is_file() {
            continue;
        }

        if produced.contains(entry.path()) {
            continue;
        }

        match std::fs::remove_file(entry.path()) {
            Ok(()) => removed += 1,

            Err(e) => diags.push(Diag::warning(
                entry.path(),
                format!("stale output could not be removed: {e}"),
            )),
        }
    }

    removed
}
