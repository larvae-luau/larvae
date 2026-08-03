//! The pipeline, discover, then parallel lex/scan/resolve/splice, then atomic writes

mod file;
mod output;
pub mod roots;
mod setup;

use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result};
use rayon::prelude::*;

use crate::cache::{Cache, hash_bytes};
use crate::config::Config;
use crate::diag::{self, Diag};
use crate::project::rojo;
use crate::requires::resolve::Resolver;
use crate::rules::{Family, Rule};

use file::{FileOpts, process_file};
use output::{copy_atomic, prune_output};

#[derive(Debug, Default)]
pub struct Stats {
    pub files_processed: usize,
    pub files_copied: usize,
    pub files_cached: usize,
    pub files_pruned: usize,
    pub requires_rewritten: usize,
    pub requires_dynamic: usize,
    /// Every rule that changed something anywhere in the project
    pub rules_applied: BTreeSet<Rule>,
}

impl Stats {
    /// How many rules in one family actually did something
    pub fn applied(&self, family: Family) -> usize {
        self.rules_applied
            .iter()
            .filter(|r| r.family == family)
            .count()
    }
}

pub struct Outcome {
    pub stats: Stats,
    pub diags: Vec<Diag>,
    pub build_project: Option<PathBuf>,
}

impl Outcome {
    pub fn has_errors(&self) -> bool {
        self.diags
            .iter()
            .any(|d| d.severity == diag::Severity::Error)
    }
}

pub fn run(root: &Path, config: &Config, write: bool) -> Result<Outcome> {
    let root = root
        .canonicalize()
        .with_context(|| format!("cannot resolve project root {}", root.display()))?;
    let roots = roots::resolve(&root, config)?;
    let output = root.join(&config.process.output);
    // the first root still stands in wherever one path is all that fits
    let input = roots[0].dir.clone();

    let mut diags: Vec<Diag> = Vec::new();

    // the project file gives us auto mounts and the derived build project
    let project = match rojo::find_project(&root, config.rojo.project.as_deref()) {
        Some(path) => match rojo::load(&path) {
            Ok(p) => Some(p),

            Err(e) => {
                diags.push(Diag::error(&path, format!("{e:#}")));

                None
            }
        },

        None => None,
    };

    let skip = setup::skip_dirs(&root, config);
    let mounts = setup::mount_table(&root, config, project.as_ref(), &mut diags);
    let luaurc = setup::luaurc_index(&root, &skip, &mut diags);
    let (to_process, to_copy) = setup::discover(&roots, config)?;

    let epoch = setup::epoch(
        &root,
        config,
        project.as_ref(),
        &skip,
        &[&to_process, &to_copy],
    );

    // caching only applies when writing, check must re-report every diagnostic
    let mut cache = Cache::load(
        &root.join(&config.process.cache_dir),
        epoch,
        config.process.cache && write,
    );

    let resolver = Resolver {
        root: &root,
        toml_aliases: &config.alias_map(),
        luaurc: &luaurc,
        mounts: &mounts,
        target: config.requires.target,
        style: config.requires.indexing_style.unwrap_or_default(),
        quote: config.process.quotes.char(),
        strict: config.requires.strict,
    };

    let opts = FileOpts::from_config(&root, config, write)?;

    // --- Parallel per file processing ---------------------------------------
    let shared_diags = Mutex::new(diags);
    let stats = Mutex::new(Stats::default());

    let fresh_hashes: Mutex<Vec<(String, u64)>> = Mutex::new(Vec::new());

    to_process.par_iter().for_each(|path| {
        let Some(rel) = roots::dest_of(&roots, path) else {
            return;
        };
        let rel_key = rel.to_string_lossy().into_owned();

        let source = match std::fs::read(path) {
            Ok(b) => b,

            Err(e) => {
                shared_diags
                    .lock()
                    .unwrap()
                    .push(Diag::error(path, format!("cannot read file: {e}")));
                return;
            }
        };

        let source_hash = hash_bytes(&source);

        if cache.is_fresh(&rel_key, source_hash, &output.join(&rel)) {
            let mut s = stats.lock().unwrap();
            s.files_processed += 1;
            s.files_cached += 1;

            return;
        }

        let mut local_diags = Vec::new();
        let rewritten = process_file(
            path,
            &rel,
            &output,
            &resolver,
            &opts,
            &config.rules,
            write,
            &mut local_diags,
        );
        let mut s = stats.lock().unwrap();
        s.files_processed += 1;

        if let Some(file) = &rewritten {
            s.requires_rewritten += file.rewrites;
            s.requires_dynamic += file.dynamic;
            s.rules_applied.extend(file.applied.iter().copied());
        }

        drop(s);
        // only a clean file earns a cache entry, errors must resurface
        if rewritten.is_some()
            && !local_diags
                .iter()
                .any(|d| d.severity == diag::Severity::Error)
        {
            fresh_hashes.lock().unwrap().push((rel_key, source_hash));
        }

        if !local_diags.is_empty() {
            shared_diags.lock().unwrap().extend(local_diags);
        }
    });

    if write {
        to_copy.par_iter().for_each(|path| {
            let Some(rel) = roots::dest_of(&roots, path) else {
                return;
            };
            let dest = output.join(rel);

            if let Err(e) = copy_atomic(path, &dest) {
                shared_diags
                    .lock()
                    .unwrap()
                    .push(Diag::error(path, format!("copy failed: {e:#}")));
            } else {
                stats.lock().unwrap().files_copied += 1;
            }
        });
    }

    let mut diags = shared_diags.into_inner().unwrap();
    let mut stats = stats.into_inner().unwrap();

    // --- Cache bookkeeping ---------------------------------------------------
    if write {
        let mut keep: HashSet<String> = HashSet::new();

        for (rel, hash) in fresh_hashes.into_inner().unwrap() {
            keep.insert(rel.clone());
            cache.record(rel, hash);
        }

        for path in &to_process {
            if let Some(rel) = roots::dest_of(&roots, path) {
                keep.insert(rel.to_string_lossy().into_owned());
            }
        }

        cache.retain(&keep);

        if let Err(e) = cache.save() {
            diags.push(Diag::warning(
                Path::new(".larvae"),
                format!("could not write the build cache: {e}"),
            ));
        }

        // --- Mirror deletions, stale output is worse than slow output --------
        let produced: HashSet<PathBuf> = to_process
            .iter()
            .chain(to_copy.iter())
            .filter_map(|p| roots::dest_of(&roots, p).map(|r| output.join(r)))
            .collect();
        stats.files_pruned = prune_output(&output, &input, &root, &produced, &mut diags);
    }

    // --- Derived build project (only when writing and a place project exists)
    let mut build_project = None;

    if write && let Some(p) = &project {
        match rojo::write_build_project(
            p,
            &input,
            &output,
            &root.join(&config.process.cache_dir),
            config
                .rojo
                .build_project
                .as_deref()
                .map(|bp| root.join(bp))
                .as_deref(),
        ) {
            Ok((path, warnings)) => {
                for w in warnings {
                    diags.push(Diag::warning(&p.path, w));
                }

                build_project = Some(path);
            }

            Err(e) => diags.push(Diag::warning(&p.path, format!("{e:#}"))),
        }
    }

    diag::sort(&mut diags);
    Ok(Outcome {
        stats,
        diags,
        build_project,
    })
}
