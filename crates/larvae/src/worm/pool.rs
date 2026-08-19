/*!
One worm instance per rayon worker.

`mlua::Lua` is `!Send`. Thus a move of a Luau worm into a parallel closure is
not possible, and a share is also not possible. A wasm `Store` can move, but
two threads cannot use it at the same time. Thus the registry holds artifacts
and settings, which are `Sync`. Each worker builds its own instances the first
time it meets a file that needs one.

Measurements show this is necessary. A rule crossing costs about 1.2 µs in
Luau and 6.9 µs in wasm. At three thousand files, a serial pass is 0.6 s and
2.7 s, against a build that is otherwise 25 ms.

**A worm that keeps state between files has undefined behavior.** Work
stealing decides which files a worker sees. Thus cross file state makes the
output depend on the schedule. By intent, a rule receives a node and a context
and nothing else, so larvae gives no place for such state.
*/

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result};

use crate::rules::edits::Edit;

use super::ctx::FileCtx;
use super::manifest::Manifest;
use super::{Worm, toml_text};

/// The id of one registry, so a worker can see that its instances are stale
static NEXT_BUILD: AtomicU64 = AtomicU64::new(1);

/// All the data a worker needs to make its own instance of one worm
pub struct Spec {
    pub manifest: Manifest,
    /// The Luau source or wasm module. It is kept so a worker does not touch the disk.
    pub artifact: Vec<u8>,
    /// The unpack directory of the worm. A native worm executes from it.
    pub dir: std::path::PathBuf,
    /// `[worms.<name>.config]`, unchanged
    pub config: toml::Value,
    /// The rules that are on, by name, with the resolved value of each
    pub rules: BTreeMap<String, toml::Value>,
    /// The order the user requested. It wins over the order the worm declared.
    pub run_order: Option<super::manifest::Stage>,
    /// The choice of the user about the inherited lints. It wins over the manifest.
    pub inherit_lints: Option<bool>,
    /// Which inherited lints and format options apply in the files of this worm
    pub inherit: crate::config::worms::Inherit,
    /// The owner of the requires in the output of this worm
    pub requires: super::RequireOwner,
    /// The extensions the front-end of this worm claims. It is empty when there is none.
    pub claims: Vec<String>,
}

impl Spec {
    /// The rule names in a stable order, which is also the wasm rule index
    pub fn enabled(&self) -> Vec<&str> {
        self.manifest
            .rules
            .keys()
            .map(String::as_str)
            .filter(|name| self.rules.contains_key(*name))
            .collect()
    }

    /// The node kinds that the enabled rules of this worm ask to see
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

    /// Report if this worm formats its claimed files for `larvae fmt`
    pub fn formats(&self) -> bool {
        self.manifest.frontend.as_ref().is_some_and(|f| f.fmt)
    }

    /// Report if this worm reports lints on its claimed files
    pub fn lints(&self) -> bool {
        !self.manifest.lints.is_empty()
    }

    /*
    Report if the lints of larvae also run on the claimed files of this worm.

    The worm declares the default in its manifest. The user overrides it in
    `[worms.<name>] inherit_lints`, because a project can want the markup
    lints of a worm and none of the Luau lints, or the opposite.
    */
    pub fn inherits_lints(&self) -> bool {
        self.inherit_lints.unwrap_or_else(|| {
            self.manifest
                .frontend
                .as_ref()
                .is_some_and(|f| f.inherit_lints)
        })
    }
}

/// The worms this build runs, shared across every worker
pub struct Pool {
    build: u64,
    specs: Vec<Arc<Spec>>,
    /// The slot of the native rules of larvae. It gives before and after a meaning.
    native: i64,
    /// The resolved settings of the project, given to each worm at init
    settings: super::Settings,
}

thread_local! {
    /// The instances of this worker. They are rebuilt when the build id changes.
    static LOCAL: RefCell<Local> = const { RefCell::new(Local { build: 0, worms: Vec::new() }) };
}

struct Local {
    build: u64,
    worms: Vec<Option<Worm>>,
}

impl Pool {
    pub fn new(specs: Vec<Arc<Spec>>, native: i64) -> Self {
        Self::with_settings(specs, native, super::Settings::default())
    }

    /// The same, with the settings of the project for the worms that lay out
    /// or report
    pub fn with_settings(specs: Vec<Arc<Spec>>, native: i64, settings: super::Settings) -> Self {
        Self {
            build: NEXT_BUILD.fetch_add(1, Ordering::Relaxed),
            specs,
            native,
            settings,
        }
    }

    /// All the worm data that can change the output, for the cache epoch
    pub fn specs(&self) -> &[Arc<Spec>] {
        &self.specs
    }

    /// The slot in which the native rules of larvae run
    pub fn native(&self) -> i64 {
        self.native
    }

    /// The slot for the rules of one worm. The user wins, then the worm, then the default
    /// of one slot after the native rules.
    pub fn slot_of(&self, spec: &Spec) -> i64 {
        spec.run_order
            .or(spec.manifest.run_order)
            .map(|s| s.slot(self.native))
            .unwrap_or(self.native + 1)
    }

    /// Every distinct slot that has work in it, in run order
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

    /// The worms with rules in one slot
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

    /// The node kinds that the enabled rules want. A file with none skips all rule work.
    pub fn filters(&self) -> Vec<super::nodes::Kind> {
        let mut kinds: Vec<_> = self.specs.iter().flat_map(|s| s.filters()).collect();

        kinds.sort_unstable_by_key(|k| k.name());
        kinds.dedup();

        kinds
    }

    /*
    Report if larvae owns the requires in the output of this file.

    A worm that sets `requires = "worm"` resolves its own requires. Then
    larvae does not scan, resolve, or rewrite them. This costs the realm
    validation for those files. For this reason the worm must state it, and
    larvae does not guess it.
    */
    pub fn owns_requires(&self, front: Option<usize>) -> bool {
        match front {
            Some(index) => self.specs[index].requires == super::RequireOwner::Larvae,

            None => true,
        }
    }

    /// The worm whose front-end claims this path, if one exists
    pub fn frontend_for(&self, path: &std::path::Path) -> Option<usize> {
        let ext = path.extension().and_then(|e| e.to_str())?;

        self.specs.iter().position(|s| {
            s.claims
                .iter()
                .any(|c| c.strip_prefix('.').is_some_and(|c| c == ext))
        })
    }

    /// Every extension that a front-end claims, without the dot
    pub fn claimed(&self) -> Vec<String> {
        self.specs
            .iter()
            .flat_map(|s| &s.claims)
            .filter_map(|c| c.strip_prefix('.').map(str::to_owned))
            .collect()
    }

    /*
    The claimed extensions with a capability behind them, without the dot.

    `larvae fmt` and `larvae lint` widen their walks with these and not with
    `claimed()`. Thus the files of a frontend-only worm stay skipped. A walk
    over such a file, with no worm able to format it, would turn a passing
    `fmt --check` into a failing one.
    */
    pub fn fmt_claimed(&self) -> Vec<String> {
        Self::extensions(self.specs.iter().filter(|s| s.formats()))
    }

    /// The same list, for the lint walk
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

    /// Run a front-end over one file, on the instance of this thread
    pub fn compile(&self, index: usize, source: &str) -> Result<super::Outcome> {
        self.with_worm(index, |worm, spec| {
            worm.transform(source, &toml_text(&spec.config))
        })
    }

    /// Ask a worm for the layout of one file, on the instance of this thread
    pub fn format(&self, index: usize, source: &str) -> Result<super::proto::FormatReply> {
        self.with_worm(index, |worm, _spec| worm.format(source))
    }

    /// Ask a worm for the findings of one file, on the instance of this thread
    pub fn lint(&self, index: usize, source: &str) -> Result<super::proto::LintReply> {
        self.with_worm(index, |worm, _spec| worm.lint(source))
    }

    /*
    Run the rules of one worm over a file, on the instance of this thread.

    The pool builds the instance on first use and keeps it. Thus a worker that
    does not meet a worm file pays nothing. A worker that does pays once and
    not per file.
    */
    pub fn run(&self, index: usize, file: Arc<FileCtx>, matched: &Matched) -> Result<Vec<Edit>> {
        self.with_worm(index, |worm, spec| {
            /*
            The native form crosses once per file and never once per node. A
            pipe crossing costs 24 µs, and a rule worm visits about 120 nodes
            per file, so the per node shape of the other forms would cost
            about 3 ms per file here.
            */
            if matches!(spec.manifest.form, super::Form::Native) {
                let calls = batch(spec, &file, matched);

                if calls.is_empty() {
                    return Ok(Vec::new());
                }

                return worm.run_rules_batched(&file.src, &calls);
            }

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
    Borrow the instance of one worm on this worker. Build it on first use.

    A worker that does not meet a file that needs this worm pays nothing. A
    worker that does pays the module load once and not per file.
    */
    /*
    The code actions every worm offers over a byte range of one file.

    A worm that fails is passed over rather than reported. This runs on a
    keystroke, and one broken worm must not take the lightbulb away from the
    others or put an error in the editor log on every press.
    */
    pub fn code_actions(
        &self,
        source: &str,
        span: (u32, u32),
    ) -> Vec<(String, super::proto::WireAction)> {
        let mut out = Vec::new();

        for (index, spec) in self.specs.iter().enumerate() {
            let name = spec.manifest.name.clone();

            let Ok(reply) = self.with_worm(index, |worm, _| worm.code_actions(source, span)) else {
                continue;
            };

            out.extend(reply.actions.into_iter().map(|a| (name.clone(), a)));
        }

        out
    }

    /// The type definitions every worm supplies, by the worm that supplied each.
    pub fn definitions(&self) -> Vec<(String, String)> {
        let mut out = Vec::new();

        for (index, spec) in self.specs.iter().enumerate() {
            let name = spec.manifest.name.clone();

            let Ok(reply) = self.with_worm(index, |worm, _| worm.definitions()) else {
                continue;
            };

            if !reply.definitions.trim().is_empty() {
                out.push((name, reply.definitions));
            }
        }

        out
    }

    fn with_worm<R>(
        &self,
        index: usize,
        f: impl FnOnce(&mut Worm, &Spec) -> Result<R>,
    ) -> Result<R> {
        let spec = Arc::clone(&self.specs[index]);
        let build = self.build;

        LOCAL.with(|local| {
            let mut local = local.borrow_mut();

            // on a new build, the kept instances of this worker belong to the old build
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

                init(&mut worm, &spec, &self.settings)?;

                local.worms[index] = Some(worm);
            }

            let result = f(local.worms[index].as_mut().expect("just built"), &spec);

            /*
            A worm that failed can be a subprocess with a dead pipe. Reuse of
            a dead pipe turns one bad file into a failure for every later file
            on this worker. A drop of the instance costs one rebuild after an
            error, so a distinction between the forms is not worth the code.
            */
            if result.is_err() {
                local.worms[index] = None;
            }

            result
        })
    }
}

fn init(worm: &mut Worm, spec: &Spec, settings: &super::Settings) -> Result<()> {
    worm.init(&spec.config, &spec.rules, settings)
}

/*
Build the batched rule calls of a native worm for one file.

The calls keep the enabled rule order, which is also the order the per node
path runs. Each call carries the id, the kind name, and the byte span of
every matched node, read from the node table here on the host. A rule with
no matched node sends no call, so a quiet file costs no crossing.
*/
fn batch(spec: &Spec, file: &FileCtx, matched: &Matched) -> Vec<super::proto::RuleCall> {
    let mut calls = Vec::new();

    for name in spec.enabled() {
        let Some(ids) = matched.for_rule(spec, name) else {
            continue;
        };

        if ids.is_empty() {
            continue;
        }

        let nodes = ids
            .iter()
            .filter_map(|&id| {
                file.table.get(id).map(|node| super::proto::WireNode {
                    id,
                    kind: node.kind.name().to_owned(),
                    span: node.span,
                })
            })
            .collect();

        calls.push(super::proto::RuleCall {
            name: name.to_owned(),
            nodes,
        });
    }

    calls
}

/// The nodes that match the filter of each rule, computed once per file
pub struct Matched {
    by_kind: BTreeMap<&'static str, Vec<u32>>,
}

impl Matched {
    /// Group the nodes of a file by kind, so every rule reads its own set without a walk
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
    A rule with no filter sees nothing, not the full tree. The full tree for
    an undeclared rule would make `filter` a suggestion instead of the cost
    control it is.
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
    A native worm whose first life stops in the middle of a call and whose
    second life works. A marker file beside the script separates the lives.
    Only a pool that drops a failed instance sees the second life.
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
            inherit_lints: None,
            inherit: Default::default(),
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
