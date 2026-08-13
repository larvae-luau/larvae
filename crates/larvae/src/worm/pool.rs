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
    /// Where the worm was unpacked, which a native worm execs from
    pub dir: std::path::PathBuf,
    /// `[worms.<name>.config]`, untouched
    pub config: toml::Value,
    /// Rules that are on, by name, with the value each resolved to
    pub rules: BTreeMap<String, toml::Value>,
    /// What the user asked for, which beats what the worm declared
    pub run_order: Option<super::manifest::Stage>,
    /// Who owns the requires in this worm's output
    pub requires: super::RequireOwner,
    /// Extensions this worm's front-end claims, empty when it has none
    pub claims: Vec<String>,
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

    /// Whether this worm lays its claimed files out for `larvae fmt`
    pub fn formats(&self) -> bool {
        self.manifest.frontend.as_ref().is_some_and(|f| f.fmt)
    }

    /// Whether this worm reports lints on its claimed files
    pub fn lints(&self) -> bool {
        !self.manifest.lints.is_empty()
    }
}

/// Worms this build will run, shared across every worker
pub struct Pool {
    build: u64,
    specs: Vec<Arc<Spec>>,
    /// Where larvae's own rules sit, so before and after mean something
    native: i64,
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
    pub fn new(specs: Vec<Arc<Spec>>, native: i64) -> Self {
        Self {
            build: NEXT_BUILD.fetch_add(1, Ordering::Relaxed),
            specs,
            native,
        }
    }

    /// Everything a worm brings that could change output, for the cache epoch
    pub fn specs(&self) -> &[Arc<Spec>] {
        &self.specs
    }

    /// The slot larvae's own rules run in
    pub fn native(&self) -> i64 {
        self.native
    }

    /// The slot one worm's rules run in, user first, then the worm, then after us
    pub fn slot_of(&self, spec: &Spec) -> i64 {
        spec.run_order
            .or(spec.manifest.run_order)
            .map(|s| s.slot(self.native))
            .unwrap_or(self.native + 1)
    }

    /// Every distinct slot that has work in it, in the order they run
    pub fn slots(&self) -> Vec<i64> {
        let mut out: Vec<i64> = self
            .specs
            .iter()
            .filter(|s| !s.rules.is_empty())
            .map(|s| self.slot_of(s))
            .collect();

        out.push(self.native);
        out.sort_unstable();
        out.dedup();

        out
    }

    /// Worms with rules in one slot
    pub fn in_slot(&self, slot: i64) -> Vec<(usize, &Arc<Spec>)> {
        self.specs
            .iter()
            .enumerate()
            .filter(|(_, s)| !s.rules.is_empty() && self.slot_of(s) == slot)
            .collect()
    }

    pub fn is_empty(&self) -> bool {
        self.specs.is_empty()
    }

    /// Node kinds any enabled rule wants, so a file with none skips everything
    pub fn filters(&self) -> Vec<super::nodes::Kind> {
        let mut kinds: Vec<_> = self.specs.iter().flat_map(|s| s.filters()).collect();

        kinds.sort_unstable_by_key(|k| k.name());
        kinds.dedup();

        kinds
    }

    /*
    Whether we own the requires in whatever this file becomes.

    A worm saying `requires = "worm"` resolves its own, so we do not scan,
    resolve or rewrite them. That costs realm validation for those files, which
    is why it is a thing a worm has to say rather than a thing we guess.
    */
    pub fn owns_requires(&self, front: Option<usize>) -> bool {
        match front {
            Some(index) => self.specs[index].requires == super::RequireOwner::Larvae,

            None => true,
        }
    }

    /// The worm whose front-end claims this path, if any
    pub fn frontend_for(&self, path: &std::path::Path) -> Option<usize> {
        let ext = path.extension().and_then(|e| e.to_str())?;

        self.specs.iter().position(|s| {
            s.claims
                .iter()
                .any(|c| c.strip_prefix('.').is_some_and(|c| c == ext))
        })
    }

    /// Every extension any front-end claims, without the dot
    pub fn claimed(&self) -> Vec<String> {
        self.specs
            .iter()
            .flat_map(|s| &s.claims)
            .filter_map(|c| c.strip_prefix('.').map(str::to_owned))
            .collect()
    }

    /*
    The claimed extensions with a capability behind them, without the dot.

    `larvae fmt` and `larvae lint` widen their walks with these rather than
    with `claimed()`, so a frontend-only worm's files stay skipped: walking
    one with nothing able to format it would turn a passing `fmt --check`
    into a failing one.
    */
    pub fn fmt_claimed(&self) -> Vec<String> {
        Self::extensions(self.specs.iter().filter(|s| s.formats()))
    }

    /// The same, for the lint walk
    pub fn lint_claimed(&self) -> Vec<String> {
        Self::extensions(self.specs.iter().filter(|s| s.lints()))
    }

    fn extensions<'a>(specs: impl Iterator<Item = &'a Arc<Spec>>) -> Vec<String> {
        specs
            .flat_map(|s| &s.claims)
            .filter_map(|c| c.strip_prefix('.').map(str::to_owned))
            .collect()
    }

    pub fn spec(&self, index: usize) -> &Arc<Spec> {
        &self.specs[index]
    }

    /// Run a front-end over one file, on this thread's instance
    pub fn compile(&self, index: usize, source: &str) -> Result<super::Outcome> {
        self.with_worm(index, |worm, spec| {
            worm.transform(source, &toml_text(&spec.config))
        })
    }

    /// Ask a worm for one file's layout, on this thread's instance
    pub fn format(&self, index: usize, source: &str) -> Result<super::proto::FormatReply> {
        self.with_worm(index, |worm, _spec| worm.format(source))
    }

    /// Ask a worm for one file's findings, on this thread's instance
    pub fn lint(&self, index: usize, source: &str) -> Result<super::proto::LintReply> {
        self.with_worm(index, |worm, _spec| worm.lint(source))
    }

    /*
    Run one worm's rules over a file, on this thread's instance.

    The instance is built on first use and kept, so a worker that never meets a
    worm file pays nothing and one that does pays once rather than per file.
    */
    pub fn run(&self, index: usize, file: Arc<FileCtx>, matched: &Matched) -> Result<Vec<Edit>> {
        self.with_worm(index, |worm, spec| {
            let mut edits = Vec::new();

            for (i, name) in spec.enabled().iter().enumerate() {
                let Some(ids) = matched.for_rule(spec, name) else {
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

    /*
    Borrow this worker's instance of one worm, building it on first use.

    A worker that never meets a file needing this worm pays nothing, and one
    that does pays the module load once rather than per file.
    */
    fn with_worm<R>(
        &self,
        index: usize,
        f: impl FnOnce(&mut Worm, &Spec) -> Result<R>,
    ) -> Result<R> {
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
                let mut worm = Worm::build(spec.manifest.clone(), &spec.artifact, Some(&spec.dir))
                    .with_context(|| format!("worm `{}`", spec.manifest.name))?;

                init(&mut worm, &spec)?;

                local.worms[index] = Some(worm);
            }

            let result = f(local.worms[index].as_mut().expect("just built"), &spec);

            /*
            A worm that failed may be a subprocess whose pipe is now dead, and
            reusing a dead pipe turns one bad file into every later file on
            this worker failing. Dropping the instance costs one rebuild after
            an error, so it is not worth telling the forms apart.
            */
            if result.is_err() {
                local.worms[index] = None;
            }

            result
        })
    }
}

fn init(worm: &mut Worm, spec: &Spec) -> Result<()> {
    match worm.manifest.form {
        super::Form::Luau => worm.init(&spec.config, &spec.rules),

        super::Form::Wasm | super::Form::Native => {
            let _ = (toml_text(&spec.config), toml_text_map(&spec.rules));

            worm.init(&spec.config, &spec.rules)
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    /*
    A native worm whose first life ends mid call and whose second life works,
    told apart by a marker file beside the script. Only a pool that drops a
    failed instance ever sees the second life.
    */
    #[cfg(unix)]
    #[test]
    fn a_dead_worm_is_respawned_for_the_next_file() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("worm.py");

        std::fs::write(
            &script,
            r#"#!/usr/bin/env python3
import sys, json, struct, os

marker = os.path.join(os.path.dirname(sys.argv[0]), "second-life")
first = not os.path.exists(marker)
if first:
    open(marker, "w").close()

def read():
    n = sys.stdin.buffer.read(4)
    if len(n) < 4: sys.exit(0)
    return json.loads(sys.stdin.buffer.read(struct.unpack("<I", n)[0]))

def send(obj):
    b = json.dumps(obj).encode()
    sys.stdout.buffer.write(struct.pack("<I", len(b)) + b)
    sys.stdout.buffer.flush()

while True:
    req = read()
    if req["op"] == "init":
        send({"ok": True})
    elif first:
        sys.exit(1)
    else:
        send({"ok": True, "output": req["source"]})
"#,
        )
        .unwrap();

        {
            use std::os::unix::fs::PermissionsExt;

            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        let manifest = Manifest::parse(
            r#"
name  = "flaky"
api   = 1
form  = "native"
entry = "worm.py"

[frontend]
claims = [".x"]
"#,
        )
        .unwrap();

        let spec = Arc::new(Spec {
            manifest,
            artifact: Vec::new(),
            dir: dir.path().to_path_buf(),
            config: toml::from_str("").unwrap(),
            rules: BTreeMap::new(),
            run_order: None,
            requires: super::super::RequireOwner::Larvae,
            claims: vec![".x".to_owned()],
        });

        let pool = Pool::new(vec![spec], 1);

        pool.compile(0, "hello").expect_err("the first life dies");

        assert_eq!(
            pool.compile(0, "hello").expect("a fresh child serves").text,
            "hello"
        );
    }
}
