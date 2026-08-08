//! The pipeline, discover, then parallel lex/scan/resolve/splice, then atomic writes

mod file;
mod frontend;
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

    /*
    Worms load before anything is discovered, because a front-end decides which
    files are even transformable. Loading also validates the whole set: names
    against their keys, and no two worms claiming one extension.
    */
    let worms = crate::worm::registry::Registry::for_project(&root, config)?;

    let claimed = worms.claimed_extensions();

    /*
    Instances for the parallel loop. mlua::Lua is !Send, so a worm cannot be
    moved into a worker at all, let alone shared. The pool holds artifacts and
    settings, which are Sync, and each worker builds its own on first use.
    */
    let pool = crate::worm::pool::Pool::new(worms.specs(), config.process.run_order);

    /*
    Realm validation is the thing nothing else in the ecosystem has, so a worm
    switching it off for its files is worth saying out loud once rather than
    letting it disappear quietly.
    */
    for spec in pool.specs() {
        if spec.requires == crate::worm::RequireOwner::Worm {
            diags.push(Diag::warning(
                Path::new("larvae.toml"),
                format!(
                    "worm `{}` resolves its own requires, so realm and clone validation is off for {}",
                    spec.manifest.name,
                    match spec.claims.is_empty() {
                        true => "the files it produces".to_owned(),
                        false => spec.claims.join(", "),
                    }
                ),
            ));
        }
    }

    // the registry validated everything, the pool is what the loop uses
    drop(worms);

    let skip = setup::skip_dirs(&root, config);
    let mounts = setup::mount_table(&root, config, project.as_ref(), &mut diags);
    let luaurc = setup::luaurc_index(&root, &skip, &mut diags);
    let (to_process, to_copy) = setup::discover(&roots, config, &claimed)?;

    let epoch = setup::epoch(
        &root,
        config,
        project.as_ref(),
        &skip,
        &[&to_process, &to_copy],
        &pool,
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
        let Some(mut rel) = roots::dest_of(&roots, path) else {
            return;
        };

        /*
        A claimed file is renamed without asking its worm, because the rename
        follows from the extension alone. That lets the cache be consulted
        before the front-end runs, so a warm build calls no worm at all.
        */
        let front = pool.frontend_for(path);

        if front.is_some() {
            rel = frontend::luau_dest(&rel);
        }

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

        let mut text = match String::from_utf8(source) {
            Ok(t) => t,

            Err(e) => {
                shared_diags
                    .lock()
                    .unwrap()
                    .push(Diag::error(path, format!("file is not UTF-8: {e}")));
                return;
            }
        };

        if cache.is_fresh(&rel_key, source_hash, &output.join(&rel)) {
            let mut s = stats.lock().unwrap();
            s.files_processed += 1;
            s.files_cached += 1;

            return;
        }

        /*
        The front-end, now that we know this file is not already built. Its
        output replaces the buffer, so every stage below reads ordinary Luau and
        none of them learn a worm was involved.
        */
        if let Some(index) = front {
            let mut local = Vec::new();
            let compiled = frontend::compile(&pool, index, path, &text, &mut local);

            if !local.is_empty() {
                shared_diags.lock().unwrap().extend(local);
            }

            match compiled {
                Some(compiled) => text = compiled,

                None => return,
            }
        }

        let mut local_diags = Vec::new();
        let rewritten = process_file(
            path,
            &text,
            &rel,
            &output,
            &resolver,
            &opts,
            &config.rules,
            write,
            &pool,
            pool.owns_requires(front),
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

        /*
        Keyed by where the file lands, so a claimed one has to be renamed here
        too. Without it every cached entry for a worm's output is evicted and
        the next build recompiles everything.
        */
        for path in &to_process {
            if let Some(rel) = roots::dest_of(&roots, path) {
                let rel = match pool.frontend_for(path) {
                    Some(_) => frontend::luau_dest(&rel),

                    None => rel,
                };

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
        /*
        A claimed file lands under its renamed dest, so the set has to agree or
        pruning deletes what the build just wrote as though it were stale.
        */
        let produced: HashSet<PathBuf> = to_process
            .iter()
            .chain(to_copy.iter())
            .filter_map(|p| {
                let rel = roots::dest_of(&roots, p)?;

                Some(match pool.frontend_for(p) {
                    Some(_) => output.join(frontend::luau_dest(&rel)),

                    None => output.join(rel),
                })
            })
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
