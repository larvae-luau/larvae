/*!
Turning `[worms]` into loaded worms.

This is where a project's config meets the manifests, so it is where the checks
that span more than one worm live: names have to agree, two worms cannot claim
the same extension, and a rule the user switched on has to be one some worm
actually declares.
*/

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result, bail};

use crate::config::worms::{Source, Worms as WormsConfig};

use super::manifest::{RuleDecl, Stage};

use super::{RequireOwner, Worm};

/// A worm plus what the project configured it with
pub struct Loaded {
    pub worm: Worm,
    /// Kept so a worker can build its own instance without touching the disk
    pub artifact: Vec<u8>,
    /// Where it was unpacked, which only a native worm needs, to exec from
    pub dir: std::path::PathBuf,
    /// `[worms.<name>.config]`, untouched, handed over at init
    pub config: toml::Value,
    /// Rules that are on, by name, with the value each resolved to
    pub rules: BTreeMap<String, toml::Value>,
    /// What the user asked for in `[worms.<name>] run_order`
    pub run_order: Option<Stage>,
}

impl Loaded {
    /// Who owns requires in this worm's output
    pub fn requires(&self) -> RequireOwner {
        self.worm.manifest.requires
    }

    /*
    Where this worm's rules sit in the sequence. Highest wins: the user's word
    in [worms.<name>], then what the worm declared, then after larvae.
    */
}

/// Every worm a build will use
#[derive(Default)]
pub struct Registry {
    worms: Vec<Loaded>,
}

impl Registry {
    /// Load every worm the config named, relative to the project root
    /*
    The worms a project asks for, or an empty set when it asks for none.

    Every command that touches a project's files needs this, not just the
    pipeline: a front-end decides which files exist as far as larvae is
    concerned, so `fmt` and `lint` walking a tree without asking would miss the
    claimed ones and format the wrong set.
    */
    pub fn for_project(root: &Path, config: &crate::config::Config) -> Result<Self> {
        let Some(value) = config.worms.as_ref() else {
            return Ok(Self::default());
        };

        let named = crate::config::worms::Worms::parse(value)?;

        Self::load(root, &root.join(&config.process.cache_dir), &named)
    }

    pub fn load(root: &Path, cache: &Path, config: &WormsConfig) -> Result<Self> {
        let mut worms = Vec::new();

        for (name, entry) in config.iter() {
            let dir = match &entry.source {
                Source::Local { path } => root.join(path),

                /*
                Fetched once and kept in the cache, so a later build does no
                network. The pin decides the version and the recorded hash
                decides whether the bytes are still the ones we installed.
                */
                source @ Source::Release { .. } => super::fetch::ensure(cache, name, source)?,
            };

            let (manifest, artifact) =
                Worm::read_parts(&dir).with_context(|| format!("loading worm `{name}`"))?;

            let worm = Worm::build(manifest, &artifact, Some(&dir))
                .with_context(|| format!("loading worm `{name}`"))?;

            /*
            One identity. The key namespaces the worm's rules and its settings,
            so a manifest disagreeing with it would leave a user configuring
            something that never reads what they wrote.
            */
            if worm.name() != name {
                bail!(
                    "worm `{name}` is named `{}` in its worm.toml, the two have to match",
                    worm.name()
                );
            }

            let enabled = resolve_rules(name, &worm, &entry.rules)?;

            worms.push(Loaded {
                dir,
                worm,
                artifact,
                config: entry.config.clone(),
                rules: enabled,
                run_order: entry.run_order,
            });
        }

        let registry = Self { worms };

        registry.check_claims()?;

        Ok(registry)
    }

    pub fn is_empty(&self) -> bool {
        self.worms.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Loaded> {
        self.worms.iter()
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut Loaded> {
        self.worms.iter_mut()
    }

    /// The worm whose front-end claims this file, if any
    pub fn frontend_for(&mut self, path: &Path) -> Option<&mut Loaded> {
        self.worms.iter_mut().find(|l| l.worm.claims(path))
    }

    /*
    Extensions any front-end claimed, without the dot. Discovery needs these or
    a claimed file is copied through untouched, which looks like it worked until
    Studio tries to run markup.
    */
    pub fn claimed_extensions(&self) -> Vec<String> {
        self.worms
            .iter()
            .filter_map(|l| l.worm.manifest.frontend.as_ref())
            .flat_map(|f| &f.claims)
            .filter_map(|c| c.strip_prefix('.').map(str::to_owned))
            .collect()
    }

    /*
    Hand the artifacts and settings to a pool, which is what the parallel loop
    uses. The registry keeps its own instances for the serial front-end pass.
    */
    pub fn specs(&self) -> Vec<std::sync::Arc<super::pool::Spec>> {
        self.worms
            .iter()
            .map(|l| {
                std::sync::Arc::new(super::pool::Spec {
                    manifest: l.worm.manifest.clone(),
                    artifact: l.artifact.clone(),
                    dir: l.dir.clone(),
                    config: l.config.clone(),
                    rules: l.rules.clone(),
                    run_order: l.run_order,
                    requires: l.worm.manifest.requires,
                    claims: l
                        .worm
                        .manifest
                        .frontend
                        .as_ref()
                        .map(|f| f.claims.clone())
                        .unwrap_or_default(),
                })
            })
            .collect()
    }

    /// Rule names every worm declared, so config validation can accept them
    pub fn declared_rules(&self) -> impl Iterator<Item = &str> {
        self.worms
            .iter()
            .flat_map(|l| l.worm.manifest.rules.keys().map(String::as_str))
    }

    /*
    A claim is exclusive. Two front-ends over one extension is a config error
    rather than a merge, because there is no sensible order to run them in and
    picking one silently would be worse than saying so.
    */
    fn check_claims(&self) -> Result<()> {
        let mut seen: BTreeMap<&str, &str> = BTreeMap::new();

        for loaded in &self.worms {
            let Some(frontend) = &loaded.worm.manifest.frontend else {
                continue;
            };

            for claim in &frontend.claims {
                if let Some(other) = seen.insert(claim, loaded.worm.name()) {
                    bail!(
                        "worms `{other}` and `{}` both claim {claim}, only one may",
                        loaded.worm.name()
                    );
                }
            }
        }

        Ok(())
    }
}

/*
The user's value beats the manifest default, and an off rule is simply absent
so a worm never sees one it should not run. A rule the worm did not declare is
an error, because otherwise a typo in [rules] is a setting that does nothing.
*/
fn resolve_rules(
    name: &str,
    worm: &Worm,
    rules: &BTreeMap<String, toml::Value>,
) -> Result<BTreeMap<String, toml::Value>> {
    let mut out = BTreeMap::new();

    /*
    A rule the user switched on that this worm does not declare would be a
    setting that silently does nothing, so it is named rather than ignored.
    */
    for key in rules.keys() {
        if !worm.manifest.rules.contains_key(key) {
            bail!("worm `{name}` has no rule `{key}`");
        }
    }

    for (rule, decl) in &worm.manifest.rules {
        let user = rules.get(rule);
        let resolved = decl.resolve(user);

        if RuleDecl::is_off(resolved) {
            continue;
        }

        out.insert(
            rule.clone(),
            resolved.cloned().unwrap_or(toml::Value::Boolean(true)),
        );
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_worm(root: &Path, dir: &str, manifest: &str, body: &str) {
        let d = root.join(dir);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("worm.toml"), manifest).unwrap();
        std::fs::write(d.join("init.luau"), body).unwrap();
    }

    const ECHO: &str = "return { frontend = { compile = function(s) return s end } }";

    fn config(src: &str) -> WormsConfig {
        WormsConfig::parse(&toml::from_str::<toml::Value>(src).unwrap()).unwrap()
    }

    #[test]
    fn a_local_worm_loads_through_the_registry() {
        let root = tempfile::tempdir().unwrap();
        write_worm(
            root.path(),
            "w",
            "name = \"echo\"\napi = 1\nform = \"luau\"\nentry = \"init.luau\"\n\n[frontend]\nclaims = [\".echo\"]\n",
            ECHO,
        );

        let r = Registry::load(
            root.path(),
            &root.path().join(".larvae"),
            &config("echo = { path = \"w\" }"),
        )
        .unwrap();

        assert_eq!(r.iter().count(), 1);
    }

    #[test]
    fn a_name_that_disagrees_with_the_key_is_refused() {
        let root = tempfile::tempdir().unwrap();
        write_worm(
            root.path(),
            "w",
            "name = \"actual\"\napi = 1\nform = \"luau\"\nentry = \"init.luau\"\n\n[frontend]\nclaims = [\".x\"]\n",
            ECHO,
        );

        let err = Registry::load(
            root.path(),
            &root.path().join(".larvae"),
            &config("expected = { path = \"w\" }"),
        )
        .err()
        .unwrap();

        assert!(format!("{err:#}").contains("have to match"), "{err:#}");
    }

    #[test]
    fn two_worms_claiming_one_extension_is_refused() {
        let root = tempfile::tempdir().unwrap();

        for (dir, name) in [("a", "one"), ("b", "two")] {
            write_worm(
                root.path(),
                dir,
                &format!(
                    "name = \"{name}\"\napi = 1\nform = \"luau\"\nentry = \"init.luau\"\n\n[frontend]\nclaims = [\".luaux\"]\n"
                ),
                ECHO,
            );
        }

        let err = Registry::load(
            root.path(),
            &root.path().join(".larvae"),
            &config("one = { path = \"a\" }\ntwo = { path = \"b\" }"),
        )
        .err()
        .unwrap();

        assert!(format!("{err:#}").contains("both claim .luaux"), "{err:#}");
    }

    /*
    A pin resolves through the cache, so an already unpacked worm needs no
    network at all. Fetching itself is covered offline in fetch.rs.
    */
    #[test]
    fn a_pinned_worm_is_taken_from_the_cache_when_it_is_there() {
        let root = tempfile::tempdir().unwrap();
        let cache = root.path().join(".larvae");
        let installed = super::super::fetch::install_dir(&cache, "echo", "0.1.0");

        std::fs::create_dir_all(&installed).unwrap();
        std::fs::write(
            installed.join("worm.toml"),
            "name = \"echo\"\napi = 1\nform = \"luau\"\nentry = \"init.luau\"\n\n[frontend]\nclaims = [\".echo\"]\n",
        )
        .unwrap();
        std::fs::write(installed.join("init.luau"), ECHO).unwrap();

        let r = Registry::load(
            root.path(),
            &cache,
            &config("echo = \"someone/echo@0.1.0\""),
        )
        .unwrap();

        assert_eq!(r.iter().count(), 1);
    }

    #[test]
    fn an_off_rule_never_reaches_the_worm() {
        let root = tempfile::tempdir().unwrap();
        write_worm(
            root.path(),
            "w",
            "name = \"r\"\napi = 1\nform = \"luau\"\nentry = \"init.luau\"\nrun_order = 1\n\n[rules.loud]\ndefault = false\n",
            "return { rules = { loud = { visit = function() end } } }",
        );

        let r = Registry::load(
            root.path(),
            &root.path().join(".larvae"),
            &config("r = { path = \"w\" }"),
        )
        .unwrap();

        assert!(r.iter().next().unwrap().rules.is_empty());
    }

    #[test]
    fn the_user_switching_a_rule_on_beats_the_default() {
        let root = tempfile::tempdir().unwrap();
        write_worm(
            root.path(),
            "w",
            "name = \"r\"\napi = 1\nform = \"luau\"\nentry = \"init.luau\"\nrun_order = 1\n\n[rules.loud]\ndefault = false\n",
            "return { rules = { loud = { visit = function() end } } }",
        );

        let r = Registry::load(
            root.path(),
            &root.path().join(".larvae"),
            &config("r = { path = \"w\", rules = { loud = true } }"),
        )
        .unwrap();

        assert_eq!(
            r.iter().next().unwrap().rules["loud"],
            toml::Value::Boolean(true)
        );
    }

    /// Three levels, and the user is the top one
    #[test]
    fn a_users_run_order_overrides_the_worms() {
        let root = tempfile::tempdir().unwrap();
        write_worm(
            root.path(),
            "w",
            "name = \"r\"\napi = 1\nform = \"luau\"\nentry = \"init.luau\"\nrun_order = 5\n\n[rules.loud]\ndefault = true\n",
            "return { rules = { loud = { visit = function() end } } }",
        );

        let r = Registry::load(
            root.path(),
            &root.path().join(".larvae"),
            &config("r = { path = \"w\", run_order = 1 }"),
        )
        .unwrap();

        assert_eq!(r.iter().next().unwrap().run_order, Some(Stage::At(1)));
    }

    #[test]
    fn without_a_user_value_the_worms_own_order_stands() {
        let root = tempfile::tempdir().unwrap();
        write_worm(
            root.path(),
            "w",
            "name = \"r\"\napi = 1\nform = \"luau\"\nentry = \"init.luau\"\nrun_order = 5\n\n[rules.loud]\ndefault = true\n",
            "return { rules = { loud = { visit = function() end } } }",
        );

        let r = Registry::load(
            root.path(),
            &root.path().join(".larvae"),
            &config("r = { path = \"w\" }"),
        )
        .unwrap();

        assert_eq!(
            r.iter().next().unwrap().worm.manifest.run_order,
            Some(Stage::At(5))
        );
    }

    #[test]
    fn a_frontend_is_found_by_the_files_extension() {
        let root = tempfile::tempdir().unwrap();
        write_worm(
            root.path(),
            "w",
            "name = \"echo\"\napi = 1\nform = \"luau\"\nentry = \"init.luau\"\n\n[frontend]\nclaims = [\".echo\"]\n",
            ECHO,
        );

        let mut r = Registry::load(
            root.path(),
            &root.path().join(".larvae"),
            &config("echo = { path = \"w\" }"),
        )
        .unwrap();

        assert!(r.frontend_for(Path::new("a/b.echo")).is_some());
        assert!(r.frontend_for(Path::new("a/b.luau")).is_none());
    }
}
