//! The `~/.larvae` home layout used by the `self` commands

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// `~/.larvae`, where `self install` puts things
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

/// Same file check via canonicalize, false when either path is missing
pub fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,

        _ => false,
    }
}

/*
Who owns the binary we are running from, we only ever replace our own copy,
anything a version manager pins belongs to that manager
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

/// True when this binary is the one `self install` put in place
pub fn is_self_installed(exe: &Path) -> bool {
    match installed_exe() {
        Ok(target) => same_file(exe, &target),
        Err(_) => false,
    }
}
