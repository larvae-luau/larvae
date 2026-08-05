/*!
One worm instance per rayon worker.

`mlua::Lua` is `!Send`, so a Luau worm cannot even be moved into a parallel
closure, let alone shared. A wasm `Store` can move but not be used from two
threads at once. So the registry holds artifacts and settings, which are `Sync`,
and each worker builds its own instances the first time it meets a file that
needs one.

Measured, this is not optional: a rule crossing costs about 1.2µs in Luau and
6.9µs in wasm, so at three thousand files a serial pass is 0.6s and 2.7s
respectively, against a build that is otherwise 25ms.

**A worm keeping state between files is undefined.** Which files a worker sees
is decided by work stealing, so cross file state makes output depend on
scheduling. A rule receives a node and a context and nothing else, deliberately,
so there is nowhere for larvae to invite it.
*/

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result};

use crate::rules::edits::Edit;

use super::ctx::FileCtx;
use super::manifest::Manifest;
use super::{Worm, toml_text, toml_text_map};

/// Identifies one registry, so a worker can tell its instances are stale
static NEXT_BUILD: AtomicU64 = AtomicU64::new(1);

/// Everything a worker needs to make its own instance of one worm
pub struct Spec {
    pub manifest: Manifest,
    /// The Luau source or wasm module, kept so a worker never touches the disk
    pub artifact: Vec<u8>,
    /// `[config.<name>]`, untouched
    pub config: toml::Value,
    /// Rules that are on, by name, with the value each resolved to
    pub rules: BTreeMap<String, toml::Value>,
}

impl Spec {
    /// Rule names in a stable order, which is also the wasm rule index
    pub fn enabled(&self) -> Vec<&str> {
        self.manifest
            .rules
            .keys()
            .map(String::as_str)
            .filter(|name| self.rules.contains_key(*name))
            .collect()
    }

    /// Node kinds this worm's enabled rules ask to see
    pub fn filters(&self) -> Vec<super::nodes::Kind> {
        let mut kinds = Vec::new();

        for name in self.enabled() {
            let Some(decl) = self.manifest.rules.get(name) else {
                continue;
            };

            for filter in &decl.filter {
                if let Some(kind) = super::nodes::Kind::from_name(filter) {
                    kinds.push(kind);
                }
            }
        }

        kinds.sort_unstable_by_key(|k| k.name());
        kinds.dedup();

        kinds
    }
}

/// Worms this build will run, shared across every worker
pub struct Pool {
    build: u64,
    specs: Vec<Arc<Spec>>,
}

thread_local! {
    /// This worker's instances, rebuilt when the build id moves
    static LOCAL: RefCell<Local> = const { RefCell::new(Local { build: 0, worms: Vec::new() }) };
}

struct Local {
    build: u64,
    worms: Vec<Option<Worm>>,
}

impl Pool {
    pub fn new(specs: Vec<Arc<Spec>>) -> Self {
        Self {
            build: NEXT_BUILD.fetch_add(1, Ordering::Relaxed),
            specs,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.specs.is_empty()
    }

    /// Every worm with at least one rule switched on, in run order
    pub fn with_rules(&self) -> Vec<(usize, &Arc<Spec>)> {
        let mut out: Vec<_> = self
            .specs
            .iter()
            .enumerate()
            .filter(|(_, s)| !s.rules.is_empty())
            .collect();

        out.sort_by_key(|(i, _)| (order_of(&self.specs[*i]), *i));

        out
    }

    /// Node kinds any enabled rule wants, so a file with none skips everything
    pub fn filters(&self) -> Vec<super::nodes::Kind> {
        let mut kinds: Vec<_> = self.specs.iter().flat_map(|s| s.filters()).collect();

        kinds.sort_unstable_by_key(|k| k.name());
        kinds.dedup();

        kinds
    }

    /*
    Run one worm's rules over a file, on this thread's instance.

    The instance is built on first use and kept, so a worker that never meets a
    worm file pays nothing and one that does pays once rather than per file.
    */
    pub fn run(&self, index: usize, file: Arc<FileCtx>, matched: &Matched) -> Result<Vec<Edit>> {
        let spec = Arc::clone(&self.specs[index]);
        let build = self.build;

        LOCAL.with(|local| {
            let mut local = local.borrow_mut();

            // a new build means whatever this worker kept belongs to the old one
            if local.build != build {
                local.build = build;
                local.worms.clear();
            }

            while local.worms.len() <= index {
                local.worms.push(None);
            }

            if local.worms[index].is_none() {
                let mut worm = Worm::from_parts(spec.manifest.clone(), &spec.artifact)
                    .with_context(|| format!("worm `{}`", spec.manifest.name))?;

                init(&mut worm, &spec)?;

                local.worms[index] = Some(worm);
            }

            let worm = local.worms[index].as_mut().expect("just built");
            let mut edits = Vec::new();

            for (i, name) in spec.enabled().iter().enumerate() {
                let Some(ids) = matched.for_rule(&spec, name) else {
                    continue;
                };

                if ids.is_empty() {
                    continue;
                }

                edits.extend(worm.run_rule(name, i as u32, Arc::clone(&file), &ids)?);
            }

            Ok(edits)
        })
    }
}

fn init(worm: &mut Worm, spec: &Spec) -> Result<()> {
    match worm.manifest.form {
        super::Form::Luau => worm.init(&spec.config, &spec.rules),

        super::Form::Wasm => {
            let _ = (toml_text(&spec.config), toml_text_map(&spec.rules));

            worm.init(&spec.config, &spec.rules)
        }
    }
}

fn order_of(spec: &Spec) -> i64 {
    spec.manifest
        .run_order
        .map(|s| s.slot())
        .unwrap_or(super::manifest::Stage::NATIVE + 1)
}

/// Nodes matching each rule's filter, worked out once per file
pub struct Matched {
    by_kind: BTreeMap<&'static str, Vec<u32>>,
}

impl Matched {
    /// Bucket a file's nodes by kind, so every rule reads its own without a walk
    pub fn of(table: &super::nodes::NodeTable, wanted: &[super::nodes::Kind]) -> Self {
        let mut by_kind: BTreeMap<&'static str, Vec<u32>> = BTreeMap::new();

        for (id, node) in table.iter() {
            if wanted.contains(&node.kind) {
                by_kind.entry(node.kind.name()).or_default().push(id);
            }
        }

        Self { by_kind }
    }

    pub fn is_empty(&self) -> bool {
        self.by_kind.is_empty()
    }

    /*
    A rule with no filter sees nothing rather than everything. Handing an
    undeclared rule the whole tree would make `filter` advice instead of the
    cost control it is.
    */
    fn for_rule(&self, spec: &Spec, rule: &str) -> Option<Vec<u32>> {
        let decl = spec.manifest.rules.get(rule)?;
        let mut ids: Vec<u32> = decl
            .filter
            .iter()
            .filter_map(|f| self.by_kind.get(f.as_str()))
            .flatten()
            .copied()
            .collect();

        ids.sort_unstable();
        ids.dedup();

        Some(ids)
    }
}
