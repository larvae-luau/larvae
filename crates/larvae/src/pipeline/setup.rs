//! Everything `run` has to work out before a single file is touched

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use globset::{Glob, GlobSet, GlobSetBuilder};
use walkdir::WalkDir;

use crate::cache::EpochInputs;
use crate::config::Config;
use crate::diag::Diag;
use crate::project::luaurc::LuaurcIndex;
use crate::project::rojo::{self, Project};
use crate::requires::datamodel::{Mount, MountTable, parse_game_path};

/// Directories the whole walk stays out of, output and caches are ours
pub fn skip_dirs(root: &Path, config: &Config) -> [PathBuf; 4] {
    [
        root.join(&config.process.output),
        root.join(&config.process.cache_dir),
        root.join(".git"),
        root.join("target"),
    ]
}

/// Config mounts win over the ones derived from the project file
pub fn mount_table(
    root: &Path,
    config: &Config,
    project: Option<&Project>,
    diags: &mut Vec<Diag>,
) -> MountTable {
    let mut mounts: Vec<Mount> = Vec::new();

    for (fs_rel, dm_value) in &config.requires.mounts {
        match parse_game_path(dm_value) {
            Some(dm) => mounts.push(Mount { fs: normalize(&root.join(fs_rel)), dm }),

            None => diags.push(Diag::error(
                Path::new("larvae.toml"),
                format!("[requires.mounts] \"{fs_rel}\" = \"{dm_value}\": value must be an @game/... path"),
            )),
        }
    }

    if let Some(p) = project {
        for m in rojo::mounts(p) {
            if !mounts.iter().any(|existing| existing.fs == m.fs) {
                mounts.push(m);
            }
        }
    }

    let table = MountTable::new(mounts);

    // rojo's own map, when the project keeps one, wins over what we inferred
    let Some(rel) = &config.requires.sourcemap else {
        return table;
    };

    match crate::project::sourcemap::load(&root.join(rel), root) {
        Ok(map) => table.with_sourcemap(map),

        Err(e) => {
            diags.push(Diag::error(&root.join(rel), format!("{e:#}")));

            table
        }
    }
}

/// Every .luaurc in the project, aliases resolve by walking up through these
pub fn luaurc_index(root: &Path, skip: &[PathBuf], diags: &mut Vec<Diag>) -> LuaurcIndex {
    let mut luaurc = LuaurcIndex::new(root);

    for entry in walk(root, skip) {
        if entry.file_name() == ".luaurc"
            && let Err(msg) = luaurc.add_file(entry.path())
        {
            diags.push(Diag::error(entry.path(), msg));
        }
    }

    luaurc
}

/// Split the input tree into files we transform and files we copy through
pub fn discover(
    roots: &[super::roots::Root],
    config: &Config,
    /*
    Extensions a front-end worm claimed. Without these a .luaux file would be
    copied through untouched, which looks like it worked right up until Studio
    tries to run markup.
    */
    claimed: &[String],
) -> Result<(Vec<PathBuf>, Vec<PathBuf>)> {
    let include = globset(&config.process.include)?;
    let exclude = globset(&config.process.exclude)?;
    let mut to_process: Vec<PathBuf> = Vec::new();
    let mut to_copy: Vec<PathBuf> = Vec::new();

    for root in roots {
        for entry in WalkDir::new(&root.dir).into_iter().filter_map(|e| e.ok()) {
            if !entry.file_type().is_file() {
                continue;
            }

            // a nested root owns its own files, the outer one skips them
            if super::roots::owner(roots, entry.path()).map(|o| &o.dir) != Some(&root.dir) {
                continue;
            }

            let rel = entry.path().strip_prefix(&root.dir).unwrap();

            if exclude.is_match(rel) {
                continue;
            }

            let is_claimed = rel
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|ext| claimed.iter().any(|c| c == ext));

            if include.is_match(rel) || is_claimed {
                to_process.push(entry.path().to_owned());
            } else if entry.file_name() != ".luaurc" {
                to_copy.push(entry.path().to_owned());
            }
        }
    }

    Ok((to_process, to_copy))
}

/*
Hash of everything require resolution reads, per file content hashing alone
would be unsound because resolving one file reads others, ex: a .luaurc up
the tree or a sibling that makes a require ambiguous. Anything here moving
invalidates the whole cache, coarse but never stale
*/
pub fn epoch(
    root: &Path,
    config: &Config,
    project: Option<&Project>,
    skip: &[PathBuf],
    files: &[&[PathBuf]],
) -> u64 {
    let mut epoch = EpochInputs::new();

    if let Some(p) = project {
        epoch.add_file(&p.path);
    }

    if let Some(rel) = &config.requires.sourcemap {
        epoch.add_file(&root.join(rel));
    }

    let cfg_path = root.join("larvae.toml");

    if cfg_path.exists() {
        epoch.add_file(&cfg_path);
    }

    for entry in walk(root, skip) {
        if entry.file_name() == ".luaurc" {
            epoch.add_file(entry.path());
        }
    }

    let mut all: Vec<PathBuf> = files.iter().flat_map(|set| set.iter().cloned()).collect();
    epoch.add_paths(&mut all);

    epoch.finish()
}

fn walk(root: &Path, skip: &[PathBuf]) -> impl Iterator<Item = walkdir::DirEntry> + use<> {
    let skip = skip.to_vec();

    WalkDir::new(root)
        .into_iter()
        .filter_entry(move |e| !skip.iter().any(|s| e.path() == s))
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
}

fn globset(patterns: &[String]) -> Result<GlobSet> {
    let mut b = GlobSetBuilder::new();

    for p in patterns {
        b.add(Glob::new(p).with_context(|| format!("invalid glob \"{p}\""))?);
    }

    Ok(b.build()?)
}

/// Flatten `.` and `..` without touching the disk, symlinks must stay intact
pub fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();

    for c in p.components() {
        match c {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }

            other => out.push(other),
        }
    }

    out
}
