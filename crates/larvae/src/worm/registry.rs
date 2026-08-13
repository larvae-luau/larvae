/*!
This module turns `[worms]` into loaded worms.

Here the config of a project meets the manifests. Thus the checks that span
more than one worm live here. The names must agree. Two worms cannot claim the
same extension. A rule the user switched on must be one that a worm declares.
*/

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result, bail};

use crate::config::worms::{Source, Worms as WormsConfig};

use super::manifest::{RuleDecl, Stage};

use super::{RequireOwner, Worm};

/// A worm plus the configuration the project gave it
pub struct Loaded {
    pub worm: Worm,
    /// Kept so a worker can build its own instance without disk access
    pub artifact: Vec<u8>,
    /// The unpack directory. Only a native worm needs it, to execute from.
    pub dir: std::path::PathBuf,
    /// `[worms.<name>.config]`, unchanged, given to the worm at init
    pub config: toml::Value,
    /// The rules that are on, by name, with the resolved value of each
    pub rules: BTreeMap<String, toml::Value>,
    /// The order the user requested in `[worms.<name>] run_order`
    pub run_order: Option<Stage>,
}

impl Loaded {
    /// The owner of the requires in the output of this worm
    pub fn requires(&self) -> RequireOwner {
        self.worm.manifest.requires
    }

    /*
    The position of the rules of this worm in the sequence. The highest source
    wins: the user's value in [worms.<name>], then the declaration of the
    worm, then the slot after the rules of larvae.
    */
}

/// Every worm that a build uses
#[derive(Default)]
pub struct Registry {
    worms: Vec<Loaded>,
}

impl Registry {
    /// Load every worm the config named, relative to the project root
    /*
    The worms a project asks for, or an empty set when it asks for none.

    Every command that touches the files of a project needs this, not only the
    pipeline. A front-end decides which files exist from the view of larvae.
    Thus `fmt` and `lint` that walk a tree without this check would miss the
    claimed files and format the wrong set.
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
                Larvae fetches the worm once and keeps it in the cache, so a
                later build uses no network. The pin decides the version. The
                recorded hash decides if the bytes are still the installed
                bytes.
                */
                source @ Source::Release { .. } => super::fetch::ensure(cache, name, source)?,
            };

            let (manifest, artifact) =
                Worm::read_parts(&dir).with_context(|| format!("loading worm `{name}`"))?;

            let worm = Worm::build(manifest, &artifact, Some(&dir))
                .with_context(|| format!("loading worm `{name}`"))?;

            /*
            One identity. The key namespaces the rules and the settings of the
            worm. With a manifest that disagrees, the user would configure a
            name that does not read the values they wrote.
            */
            if worm.name() != name {
                bail!(
                    "worm `{name}` is named `{}` in its worm.toml, the two have to match",
                    worm.name()
                );
            }

            let enabled = resolve_rules(name, &worm, &entry.rules)?;
            let config = resolve_config(name, &worm, &entry.config)?;

            worms.push(Loaded {
                dir,
                worm,
                artifact,
                config,
                rules: enabled,
                run_order: entry.run_order,
            });
        }

        let registry = Self { worms };

        registry.check_claims()?;
        registry.check_lints()?;

        /*
        The editor needs a schema that knows these worms. The write is best
        effort, because a read only checkout must still build and lint.
        */
        if !registry.is_empty() {
            let _ = crate::schema::write(cache, &registry);
        }

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

    /// The worm whose front-end claims this file, if one exists
    pub fn frontend_for(&mut self, path: &Path) -> Option<&mut Loaded> {
        self.worms.iter_mut().find(|l| l.worm.claims(path))
    }

    /*
    The extensions that a front-end claimed, without the dot. Discovery needs
    these. Without them, a claimed file is copied through unchanged. That
    looks correct until Studio tries to run markup.
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
    Give the artifacts and settings to a pool. The parallel loop uses the
    pool. The registry keeps its own instances for the serial front-end pass.
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

    /// The rule names that every worm declared, so config validation can accept them
    pub fn declared_rules(&self) -> impl Iterator<Item = &str> {
        self.worms
            .iter()
            .flat_map(|l| l.worm.manifest.rules.keys().map(String::as_str))
    }

    /// The lint names that every worm declared, with the default of each
    pub fn declared_lints(&self) -> impl Iterator<Item = (&str, &super::manifest::LintDecl)> {
        self.worms.iter().flat_map(|l| {
            l.worm
                .manifest
                .lints
                .iter()
                .map(|(name, decl)| (name.as_str(), decl))
        })
    }

    /*
    A claim is exclusive. Two front-ends over one extension are a config error
    and not a merge. There is no correct order to run them in, and a silent
    choice of one would be worse than a clear error.
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

    /*
    Worm lints share `[lint.rules]` with the builtins. This is what makes
    `luaux_unclosed_element = "deny"` work with no extra steps. Shared names
    need a guard. With a collision, one `[lint.rules]` line would set the
    level of two different lints. Thus larvae refuses both directions and
    names the two parties.
    */
    fn check_lints(&self) -> Result<()> {
        let mut seen: BTreeMap<&str, &str> = BTreeMap::new();

        for loaded in &self.worms {
            for name in loaded.worm.manifest.lints.keys() {
                if crate::lint::find(name).is_some() {
                    bail!(
                        "worm `{}` declares lint `{name}`, which is a builtin lint's name",
                        loaded.worm.name()
                    );
                }

                if let Some(other) = seen.insert(name, loaded.worm.name()) {
                    bail!(
                        "worms `{other}` and `{}` both declare lint `{name}`, only one may",
                        loaded.worm.name()
                    );
                }
            }
        }

        Ok(())
    }
}

/*
The value of the user wins over the manifest default. An off rule is absent,
so a worm does not see a rule it must not run. A rule the worm did not declare
is an error. Without the error, a typo in [rules] is a setting that does
nothing.
*/
fn resolve_rules(
    name: &str,
    worm: &Worm,
    rules: &BTreeMap<String, toml::Value>,
) -> Result<BTreeMap<String, toml::Value>> {
    let mut out = BTreeMap::new();

    /*
    A rule the user switched on, which this worm does not declare, would be a
    setting that silently does nothing. Thus larvae names it and does not
    ignore it.
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

/*
Check the settings of a project against the options the worm declares, and
fill each missing key with its default.

A worm that declares no option keeps the opaque table it always had. A worm
that declares its options gets a complete table at init, so the guest reads a
key instead of a key and a fallback.
*/
fn resolve_config(name: &str, worm: &Worm, user: &toml::Value) -> Result<toml::Value> {
    let declared = &worm.manifest.options;

    if declared.is_empty() {
        return Ok(user.clone());
    }

    let mut out = user
        .as_table()
        .cloned()
        .unwrap_or_default();

    /*
    A key the worm does not declare is a setting that does nothing. It is
    named here, for the same reason a rule the worm does not declare is.
    */
    for (key, value) in &out {
        let Some(option) = declared.get(key) else {
            bail!("worm `{name}` has no option `{key}`");
        };

        if !option.kind.accepts(value) {
            bail!(
                "worm `{name}`: option `{key}` takes a {}",
                option.kind.name()
            );
        }

        if !option.values.is_empty() && !option.values.contains(value) {
            let allowed: Vec<String> = option.values.iter().map(scalar).collect();

            bail!(
                "worm `{name}`: option `{key}` takes one of {}",
                allowed.join(", ")
            );
        }
    }

    for (key, option) in declared {
        if let Some(default) = &option.default
            && !out.contains_key(key)
        {
            out.insert(key.clone(), default.clone());
        }
    }

    Ok(toml::Value::Table(out))
}

/// One value as a user would write it, for a message that lists the choices
fn scalar(value: &toml::Value) -> String {
    match value {
        toml::Value::String(s) => format!("{s:?}"),

        toml::Value::Integer(n) => n.to_string(),

        toml::Value::Float(f) => f.to_string(),

        toml::Value::Boolean(b) => b.to_string(),

        other => other.type_str().to_owned(),
    }
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
    A pin resolves through the cache, so a worm that is already unpacked needs
    no network at all. The tests in fetch.rs cover the fetch itself offline.
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

    /// There are three levels, and the user is the top one
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

    fn options_worm(root: &Path) {
        write_worm(
            root,
            "w",
            "name = \"echo\"\napi = 1\nform = \"luau\"\nentry = \"init.luau\"\n\n[frontend]\nclaims = [\".echo\"]\n\n[options.pretty]\ntype = \"boolean\"\ndefault = true\n\n[options.factory]\ntype = \"string\"\nvalues = [\"vide\", \"react\"]\n",
            ECHO,
        );
    }

    #[test]
    fn a_declared_option_takes_its_default_when_the_user_writes_nothing() {
        let root = tempfile::tempdir().unwrap();
        options_worm(root.path());

        let r = Registry::load(
            root.path(),
            &root.path().join(".larvae"),
            &config("echo = { path = \"w\" }"),
        )
        .unwrap();

        let settings = r.iter().next().unwrap().config.as_table().unwrap();

        assert_eq!(settings["pretty"], toml::Value::Boolean(true));

        // an option with no default stays absent rather than guessing one
        assert!(!settings.contains_key("factory"));
    }

    #[test]
    fn an_option_the_worm_does_not_declare_is_refused() {
        let root = tempfile::tempdir().unwrap();
        options_worm(root.path());

        let err = Registry::load(
            root.path(),
            &root.path().join(".larvae"),
            &config("echo = { path = \"w\", config = { prety = true } }"),
        )
        .err()
        .unwrap();

        assert!(format!("{err:#}").contains("no option `prety`"), "{err:#}");
    }

    #[test]
    fn an_option_of_the_wrong_type_is_refused() {
        let root = tempfile::tempdir().unwrap();
        options_worm(root.path());

        let err = Registry::load(
            root.path(),
            &root.path().join(".larvae"),
            &config("echo = { path = \"w\", config = { pretty = \"yes\" } }"),
        )
        .err()
        .unwrap();

        assert!(format!("{err:#}").contains("takes a boolean"), "{err:#}");
    }

    #[test]
    fn an_option_outside_its_listed_values_is_refused() {
        let root = tempfile::tempdir().unwrap();
        options_worm(root.path());

        let err = Registry::load(
            root.path(),
            &root.path().join(".larvae"),
            &config("echo = { path = \"w\", config = { factory = \"solid\" } }"),
        )
        .err()
        .unwrap();

        assert!(format!("{err:#}").contains("takes one of"), "{err:#}");
    }

    #[test]
    fn a_worm_lint_named_like_a_builtin_is_refused() {
        let root = tempfile::tempdir().unwrap();
        write_worm(
            root.path(),
            "w",
            "name = \"echo\"\napi = 1\nform = \"luau\"\nentry = \"init.luau\"\n\n[frontend]\nclaims = [\".echo\"]\n\n[lints.unused_variable]\n",
            ECHO,
        );

        let err = Registry::load(
            root.path(),
            &root.path().join(".larvae"),
            &config("echo = { path = \"w\" }"),
        )
        .err()
        .unwrap();

        assert!(format!("{err:#}").contains("builtin"), "{err:#}");
    }

    #[test]
    fn two_worms_declaring_one_lint_is_refused() {
        let root = tempfile::tempdir().unwrap();

        for (dir, name, ext) in [("a", "one", ".aa"), ("b", "two", ".bb")] {
            write_worm(
                root.path(),
                dir,
                &format!(
                    "name = \"{name}\"\napi = 1\nform = \"luau\"\nentry = \"init.luau\"\n\n[frontend]\nclaims = [\"{ext}\"]\n\n[lints.tidy]\n"
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

        assert!(format!("{err:#}").contains("both declare lint `tidy`"), "{err:#}");
    }
}
