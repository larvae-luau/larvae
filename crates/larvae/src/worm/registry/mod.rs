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

use super::manifest::Stage;

use super::{RequireOwner, Worm};

/*
What a caller does about a worm the disk does not have.

`larvae worm install` is the one place that downloads. Every other caller
reads what that put there, and they differ only in whether a missing worm is
worth saying out loud.
*/
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fetch {
    /// Fetch a worm that the cache does not hold yet
    Allowed,
    /// Use the cache alone, and skip a missing worm without a word (the editor)
    Quiet,
    /// Use the cache alone, and name a missing worm (a command a person ran)
    Report,
}

/// The directory of a release worm, when the cache already holds it
/*
The installed directory for this worm, without asking the network.

A version resolves against what is on disk here, and not against what a
repository has. `larvae worm install` did the online half and named the
directory after the release it settled on, so a range finds the newest
installed release that satisfies it and a build stays offline.

The alternative would be resolving the range again on every command, which
puts a request in front of every `larvae fmt` and makes the version of a build
depend on the day it ran.
*/
fn cached(cache: &Path, name: &str, source: &Source) -> Option<std::path::PathBuf> {
    let (Source::Release { version, .. } | Source::Cargo { version, .. }) = source else {
        return None;
    };

    let has_manifest = |dir: &Path| dir.join(super::MANIFEST).exists();

    let wanted = super::version::Wanted::parse(version).ok()?;

    if !wanted.needs_the_list() {
        let dir = super::fetch::install_dir(cache, name, version);

        return has_manifest(&dir).then_some(dir);
    }

    let installed: Vec<String> = std::fs::read_dir(cache.join("worms").join(name))
        .ok()?
        .filter_map(|entry| {
            let entry = entry.ok()?;

            has_manifest(&entry.path()).then(|| entry.file_name().to_string_lossy().into_owned())
        })
        .collect();

    let names: Vec<&str> = installed.iter().map(String::as_str).collect();
    let picked = wanted.pick(&names)?;

    Some(super::fetch::install_dir(cache, name, picked))
}

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
    /// The choice of the user about the inherited lints, which wins over the manifest
    pub inherit_lints: Option<bool>,
    /// Which inherited lints and format options apply in the files of this worm
    pub inherit: crate::config::worms::Inherit,
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
        /*
        No command downloads a worm. `larvae worm install` does that, and it
        is the only place that resolves a range against the repository. A
        command reads what install left and names what is missing, so a build
        makes no request and a version does not change under it mid run.
        */
        Self::project(root, config, Fetch::Report)
    }

    /*
    The same, without the network.

    The editor uses this. A worm that is not in the cache is skipped, and the
    rest of the project still works, because a keystroke cannot wait for a
    download. The next command in the terminal fetches the worm.
    */
    pub fn for_project_cached(root: &Path, config: &crate::config::Config) -> Result<Self> {
        Self::project(root, config, Fetch::Quiet)
    }

    /// The same, and a missing worm is named rather than passed over silently.
    pub fn for_project_reporting(root: &Path, config: &crate::config::Config) -> Result<Self> {
        Self::project(root, config, Fetch::Report)
    }

    fn project(root: &Path, config: &crate::config::Config, fetch: Fetch) -> Result<Self> {
        let Some(value) = config.worms.as_ref() else {
            return Ok(Self::default());
        };

        let named = crate::config::worms::Worms::parse(value)?;

        Self::load_with(root, &root.join(&config.process.cache_dir), &named, fetch)
    }

    pub fn load(root: &Path, cache: &Path, config: &WormsConfig) -> Result<Self> {
        Self::load_with(root, cache, config, Fetch::Allowed)
    }

    /// The same, with a choice about the network
    pub fn load_with(
        root: &Path,
        cache: &Path,
        config: &WormsConfig,
        fetch: Fetch,
    ) -> Result<Self> {
        let mut worms = Vec::new();
        let mut skipped = false;

        for (name, entry) in config.iter() {
            let dir = match &entry.source {
                Source::Local { path } => root.join(path),

                /*
                Larvae fetches the worm once and keeps it in the cache, so a
                later build uses no network. The pin decides the version. The
                recorded hash decides if the bytes are still the installed
                bytes.

                A caller that refuses the network skips a worm it does not
                have yet. The editor is such a caller: it must answer a
                keystroke, and a download is not an answer.
                */
                source @ (Source::Release { .. } | Source::Cargo { .. }) => match fetch {
                    Fetch::Allowed => super::fetch::ensure(cache, name, source)?,

                    Fetch::Quiet | Fetch::Report => match cached(cache, name, source) {
                        Some(dir) => dir,

                        /*
                        A worm the project names and the disk does not have.

                        The editor reaches here on every keystroke and must
                        stay quiet, so the skip is silent for it. A command
                        run by a person says so once: without the worm, a
                        file that worm claims is read as plain Luau or not at
                        all, and a silent skip makes that look like a bug in
                        larvae.
                        */
                        None => {
                            if fetch == Fetch::Report {
                                crate::ui::print_error(&format!(
                                    "worm `{name}` is not installed, run `larvae worm install`"
                                ));
                            }

                            skipped = true;

                            continue;
                        }
                    },
                },
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
                inherit_lints: entry.inherit_lints,
                inherit: entry.inherit.clone(),
            });
        }

        let registry = Self { worms };

        registry.check_claims()?;
        registry.check_lints()?;
        registry.check_fmt()?;

        /*
        The editor needs a schema that knows these worms. The write is best
        effort, because a read only checkout must still build and lint. A
        load that skipped a worm writes nothing, because a schema without
        that worm would flag its keys until the next full load.
        */
        if !registry.is_empty() && !skipped {
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
                    inherit_lints: l.inherit_lints,
                    inherit: l.inherit.clone(),
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

    /*
    Check the `[fmt]` table of a project against the options the worms
    declare, and fill each missing option with its default.

    A key that larvae does not own reaches this point in `rest`. It has to
    belong to a worm, because a key that belongs to nobody is a setting that
    does nothing, and that is the failure mode larvae refuses everywhere.
    */
    /*
    Check the `[fmt]` table of a project against the options its worms
    declare, and fill each missing option with its default.

    A worm names its options under its own key, exactly as it names its
    lints:

    ```toml
    [fmt.luaux]
    attribute_per_line = true
    ```

    A key that larvae does not own reaches this point in `rest`. It has to be
    the key of a worm, because a key that belongs to nobody is a setting that
    does nothing, and larvae refuses that failure everywhere.
    */
    pub fn resolve_fmt(&self, cfg: &mut crate::fmt::FmtConfig) -> Result<()> {
        for (key, value) in &cfg.rest {
            let Some(loaded) = self.worms.iter().find(|l| l.worm.name() == key) else {
                bail!("`[fmt] {key}` is not an option of larvae or of any worm this project loads");
            };

            let declared = &loaded.worm.manifest.fmt;

            let Some(table) = value.as_table() else {
                bail!("`[fmt] {key}` holds the options of worm `{key}`, so it has to be a table");
            };

            for (name, value) in table {
                let Some(option) = declared.get(name) else {
                    bail!("worm `{key}` has no format option `{name}`");
                };

                if !option.kind.accepts(value) {
                    bail!("`[fmt.{key}] {name}` takes a {}", option.kind.name());
                }

                if !option.values.is_empty() && !option.values.contains(value) {
                    let allowed: Vec<String> = option.values.iter().map(scalar).collect();

                    bail!("`[fmt.{key}] {name}` takes one of {}", allowed.join(", "));
                }
            }
        }

        for loaded in &self.worms {
            if loaded.worm.manifest.fmt.is_empty() {
                continue;
            }

            let entry = cfg
                .rest
                .entry(loaded.worm.name().to_owned())
                .or_insert_with(|| toml::Value::Table(toml::map::Map::new()));

            let Some(table) = entry.as_table_mut() else {
                continue;
            };

            for (name, option) in &loaded.worm.manifest.fmt {
                if let Some(default) = &option.default
                    && !table.contains_key(name)
                {
                    table.insert(name.clone(), default.clone());
                }
            }
        }

        Ok(())
    }

    /// Every format option that a worm declared, with the worm that declared it
    pub fn declared_fmt(&self) -> impl Iterator<Item = (&str, &str, &super::manifest::OptionDecl)> {
        self.worms.iter().flat_map(|l| {
            l.worm
                .manifest
                .fmt
                .iter()
                .map(|(name, option)| (l.worm.name(), name.as_str(), option))
        })
    }

    /*
    A format option of a worm shares the `[fmt]` table with the builtin
    options, so its name has to be free. A collision would make one line of
    `[fmt]` mean two settings.
    */
    fn check_fmt(&self) -> Result<()> {
        let builtin = serde_json::to_value(crate::fmt::FmtConfig::default()).unwrap_or_default();

        for loaded in &self.worms {
            let name = loaded.worm.name();

            if !loaded.worm.manifest.fmt.is_empty() && builtin.get(name).is_some() {
                bail!(
                    "worm `{name}` declares format options, and larvae already owns a `[fmt]` key of that name"
                );
            }
        }

        Ok(())
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

mod resolve;

use resolve::{resolve_config, resolve_rules, scalar};

#[cfg(test)]
mod tests;
