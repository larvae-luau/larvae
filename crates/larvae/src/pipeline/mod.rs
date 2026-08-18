//! The pipeline: discovery, then parallel lex/scan/resolve/splice, then atomic writes

mod file;
mod frontend;
mod output;
pub mod roots;
pub(crate) mod setup;

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
    /// Every rule that changed a file in the project
    pub rules_applied: BTreeSet<Rule>,
}

impl Stats {
    /// The count of rules in one family that changed a file
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
    /*
    Which module requires which, for the whole project analyses in `check`.

    The graph is complete when the cache is off, which holds for every read
    only run: the cache applies only to writes.
    */
    pub graph: crate::requires::graph::Graph,
    /*
    The processed text of every module, keyed like the graph keys its nodes.

    The bundler reads modules from here and not from disk, for two reasons.
    A front-end worm compiles a claimed file inside the pipeline, so the
    file on disk holds markup that is not Luau. And the require sites of the
    graph hold byte spans, and the spans index this exact text.

    The map fills only on a [`run_keeping_sources`] run, because it holds
    the whole project in memory.
    */
    pub sources: std::collections::BTreeMap<PathBuf, String>,
}

impl Outcome {
    pub fn has_errors(&self) -> bool {
        self.diags
            .iter()
            .any(|d| d.severity == diag::Severity::Error)
    }
}

pub fn run(root: &Path, config: &Config, write: bool) -> Result<Outcome> {
    run_inner(root, config, write, false, false)
}

/*
Run the pipeline without writes, and build the require graph.

This is the entry for `check`. The graph harvest costs a little on every
file, so a plain build does not pay it; only the two commands that read the
graph ask for it.
*/
pub fn run_analysing(root: &Path, config: &Config) -> Result<Outcome> {
    run_inner(root, config, false, false, true)
}

/*
Run the pipeline without writes, and keep the processed text of every module.

This is the entry for the bundler. The run does not write, so the cache is
off, and the graph and the sources cover every file. See [`Outcome::sources`]
for why the bundler must not read the files from disk itself.
*/
pub fn run_keeping_sources(root: &Path, config: &Config) -> Result<Outcome> {
    run_inner(root, config, false, true, true)
}

fn run_inner(
    root: &Path,
    config: &Config,
    write: bool,
    keep_sources: bool,
    collect_graph: bool,
) -> Result<Outcome> {
    let root = root
        .canonicalize()
        .with_context(|| format!("cannot resolve project root {}", root.display()))?;
    let roots = roots::resolve(&root, config)?;
    let output = root.join(&config.process.output);
    // The first root serves in each place where only one path fits.
    let input = roots[0].dir.clone();

    let mut diags: Vec<Diag> = Vec::new();

    // The project file gives larvae the auto mounts and the derived build project.
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
    Worms load before discovery, because a front-end decides which files
    larvae can transform. The load also validates the full set: names against
    their keys, and no two worms with a claim on one extension.
    */
    let worms = crate::worm::registry::Registry::for_project(&root, config)?;

    let claimed = worms.claimed_extensions();

    /*
    Instances for the parallel loop. mlua::Lua is !Send, so a move of a worm
    into a worker is not possible, and shared use is also not possible. The
    pool holds artifacts and settings, which are Sync. Each worker builds its
    own instance on first use.
    */
    let pool = crate::worm::pool::Pool::new(worms.specs(), config.process.run_order);

    /*
    Realm validation is a feature that no other tool in the ecosystem has. So
    larvae reports once when a worm switches it off for its files, and the
    feature does not disappear without a message.
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

    // The registry validated the full set; the loop uses the pool.
    drop(worms);

    let skip = setup::skip_dirs(&root, config);
    let mounts = setup::mount_table(&root, config, project.as_ref(), &mut diags);
    let luaurc = setup::luaurc_index(&root, &skip, &mut diags);
    let (to_process, to_copy) = setup::discover(&root, &roots, config, &claimed)?;

    let epoch = setup::epoch(
        &root,
        config,
        project.as_ref(),
        &skip,
        &[&to_process, &to_copy],
        &pool,
    );

    // The cache applies only to writes; check must report every diagnostic again.
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

    /*
    `[minify] rename_variables` is a spelling of the `rename_variables`
    rule that lives with the other minify settings. It takes effect only
    under the dense generator, so a profile can carry the whole minify
    story and the default build stays untouched.
    */
    let rules_cfg: std::borrow::Cow<crate::config::RulesConfig> =
        match config.process.generator == "dense" && config.minify.rename_variables {
            true => {
                let mut forced = config.rules.clone();
                forced.rename_variables = true;

                std::borrow::Cow::Owned(forced)
            }

            false => std::borrow::Cow::Borrowed(&config.rules),
        };

    // --- Parallel per file processing ---------------------------------------
    let shared_diags = Mutex::new(diags);
    let shared_graph = Mutex::new(crate::requires::graph::Graph::default());
    let shared_sources = Mutex::new(std::collections::BTreeMap::new());
    let stats = Mutex::new(Stats::default());

    let fresh_hashes: Mutex<Vec<(String, u64)>> = Mutex::new(Vec::new());

    to_process.par_iter().for_each(|path| {
        let Some(mut rel) = roots::dest_of(&roots, path) else {
            return;
        };

        /*
        larvae renames a claimed file without a call to its worm, because the
        rename follows from the extension alone. So larvae can consult the
        cache before the front-end runs, and a warm build calls no worm.
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

        /*
        A read-only pass analyzes a stand-in for a file that is not UTF-8,
        because Luau reads any byte inside a string and a check must not
        refuse a file the compiler accepts. A writing pass still refuses:
        it would splice the stand-in bytes into the output.
        */
        let mut text = match String::from_utf8(source) {
            Ok(t) => t,

            Err(e) if write => {
                shared_diags
                    .lock()
                    .unwrap()
                    .push(Diag::error(path, format!("file is not UTF-8: {e}")));
                return;
            }

            Err(e) => {
                let (text, replaced) = crate::sys::utf8_stand_in(e.into_bytes());

                shared_diags.lock().unwrap().push(Diag::warning(
                    path,
                    format!(
                        "{replaced} byte(s) are not UTF-8; the analysis reads stand-ins for them"
                    ),
                ));

                text
            }
        };

        if cache.is_fresh(&rel_key, source_hash, &output.join(&rel)) {
            let mut s = stats.lock().unwrap();
            s.files_processed += 1;
            s.files_cached += 1;

            return;
        }

        /*
        The front-end runs now, because this file is not already built. Its
        output replaces the buffer. So every stage below reads plain Luau, and
        no stage learns that a worm ran.
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

        let owns = pool.owns_requires(front);

        let mut local_diags = Vec::new();
        let mut rewritten = process_file(
            path,
            &text,
            &rel,
            &output,
            &resolver,
            &opts,
            &rules_cfg,
            write,
            &pool,
            owns,
            keep_sources,
            collect_graph,
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

        /*
        The serial half of section 4.4. Every worker collects its own edges
        with no lock, and this merge is the one place they meet. So the graph
        builds once at the end of each file, and no worker contends on a
        require. An init file keys on its directory, so the edge into a
        directory module meets the edges out of its init file on one node.
        */
        if collect_graph {
            let node = crate::requires::graph::node_of(path);
            let mut g = shared_graph.lock().unwrap();

            if owns {
                g.see(node);
            } else {
                // A worm resolves the requires of this file, so the graph
                // holds no edges for it. The unused analysis checks the mark.
                g.see_opaque(node);
            }

            if let Some(file) = &rewritten {
                for to in &file.required {
                    g.add(node, to);
                }

                // The sites key on the node too, so the bundler finds the
                // sites of an init file under its directory.
                for site in &file.sites {
                    g.add_site(node, site.clone());
                }
            }
        }

        if let Some(text) = rewritten.as_mut().and_then(|f| f.source.take()) {
            let node = crate::requires::graph::node_of(path);
            shared_sources
                .lock()
                .unwrap()
                .insert(node.to_path_buf(), text);
        }
        // Only a clean file gets a cache entry; errors must appear again.
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
        The key is the destination of the file, so a claimed file must get
        the same rename here. Without it, the cache evicts every entry for
        the output of a worm, and the next build compiles every file again.
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

        // --- Mirror deletions; stale output is worse than slow output --------
        /*
        A claimed file lands under its renamed destination, so the set must
        agree. Otherwise the prune deletes the new output of the build as
        stale.
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

    // --- The derived build project (only when the run writes and a project exists)
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
        graph: shared_graph.into_inner().unwrap(),
        sources: shared_sources.into_inner().unwrap(),
    })
}
