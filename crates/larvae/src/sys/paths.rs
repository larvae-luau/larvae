//! The `~/.larvae` home layout that the `self` commands use

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// `~/.larvae`, the directory where `self install` writes files
pub fn larvae_dir() -> Result<PathBuf> {
    Ok(dirs::home_dir()
        .context("cannot determine your home directory")?
        .join(".larvae"))
}

/// `~/.larvae/bin`, on PATH after `self install`
pub fn bin_dir() -> Result<PathBuf> {
    Ok(larvae_dir()?.join("bin"))
}

/// The installed binary path, `~/.larvae/bin/larvae[.exe]`
pub fn installed_exe() -> Result<PathBuf> {
    Ok(bin_dir()?.join(format!("larvae{}", std::env::consts::EXE_SUFFIX)))
}

/// A same file check through canonicalize; false when either path is missing
pub fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,

        _ => false,
    }
}

/*
The tool that owns the binary in use. larvae replaces only its own copy. A
binary that a version manager pins belongs to that manager.
*/
pub fn managing_tool(exe: &Path) -> Option<&'static str> {
    let path = exe.to_string_lossy().to_lowercase();

    for (marker, name) in [
        (".rokit", "rokit"),
        (".aftman", "aftman"),
        (".foreman", "foreman"),
        (".lpm", "lpm"),
        (".cargo", "cargo"),
    ] {
        if path.contains(marker) {
            return Some(name);
        }
    }

    None
}

/// True when this binary is the one that `self install` wrote
pub fn is_self_installed(exe: &Path) -> bool {
    match installed_exe() {
        Ok(target) => same_file(exe, &target),
        Err(_) => false,
    }
}
