/*!
Worms, the extensions named under `[worms]` in `larvae.toml`.

A worm ships as a release zip that holds a [`Manifest`] and one artifact. The
artifact is Luau source or a `wasm32` module. Both forms follow the same
contract. Thus the code above this module is written once, and the pipeline
and the config layer do not know which form they speak to.
*/

pub mod ctx;
pub mod fetch;
pub mod host;
pub mod luau;
pub mod manifest;
pub mod native;
pub mod nodes;
pub mod pool;
pub mod proto;
pub mod registry;

use std::path::Path;

use anyhow::{Context, Result, bail};

pub use host::{Outcome, WasmWorm};
pub use luau::LuauWorm;
pub use manifest::{Form, Frontend, Manifest, RequireOwner, RuleDecl};

/// The ABI revision this host speaks. It must match `api` in the `worm.toml` of a worm.
pub const ABI_VERSION: u32 = 1;

/// The manifest filename inside the zip of a worm
pub const MANIFEST: &str = "worm.toml";

/*
The settings of the project, for a worm that lays out or reports.

A worm that formats its own constructs has to know the width and the spacing
the project asked for. Without these settings the user writes the same values
a second time under `[worms.<name>.config]`, and the two copies drift.

Both fields are JSON text, because the native transport already speaks JSON
and larvae builds TOML without a serializer. The fields are empty for a
project that states nothing, and a worm that ignores them stays correct.
*/
#[derive(Debug, Clone, Default)]
pub struct Settings {
    /// The resolved `[fmt]` table
    pub fmt: String,
    /// The resolved lint levels, by lint name
    pub lint: String,
}

impl Settings {
    /// The settings a project resolved, for the worms it loads
    pub fn new(fmt: &crate::fmt::FmtConfig, lint: &crate::lint::LintConfig) -> Self {
        Self {
            fmt: serde_json::to_string(fmt).unwrap_or_default(),
            lint: serde_json::to_string(&lint.rules).unwrap_or_default(),
        }
    }
}

/// A loaded worm of either form
enum Backend {
    Luau(Box<LuauWorm>),
    Wasm(Box<WasmWorm>),
    Native(Box<native::NativeWorm>),
}

/// A worm, loaded and ready for one call per file
pub struct Worm {
    /// The declarations the worm made about itself
    pub manifest: Manifest,
    backend: Backend,
}

impl Worm {
    /// Build a worm from a manifest and its artifact bytes. A worker does
    /// this when it needs its own instance.
    pub fn from_parts(manifest: Manifest, artifact: &[u8]) -> Result<Self> {
        Self::build(manifest, artifact, None)
    }

    /*
    Build a worm, with the unpack directory when the form needs it.

    Only the native form needs the directory. The other two forms get their
    bytes and run them in process, while a subprocess must be a file that the
    operating system can execute. A caller with no directory can still build
    the other two forms. This keeps `from_parts` usable.
    */
    pub fn build(manifest: Manifest, artifact: &[u8], dir: Option<&Path>) -> Result<Self> {
        let backend = match manifest.form {
            Form::Luau => {
                let source = std::str::from_utf8(artifact)
                    .with_context(|| format!("worm `{}` is not UTF-8", manifest.name))?;

                Backend::Luau(Box::new(LuauWorm::load(source, &manifest.name)?))
            }

            Form::Wasm => Backend::Wasm(Box::new(WasmWorm::load(artifact)?)),

            Form::Native => {
                let dir = dir.with_context(|| {
                    format!(
                        "worm `{}` is native, which needs the file on disk to run",
                        manifest.name
                    )
                })?;

                Backend::Native(Box::new(native::NativeWorm::load(
                    &native_entry(dir, &manifest.entry),
                    &manifest.name,
                )?))
            }
        };

        Ok(Self { manifest, backend })
    }

    /// Load a worm from an unpacked directory that holds `worm.toml` and its artifact
    pub fn load(dir: &Path) -> Result<Self> {
        let path = dir.join(MANIFEST);
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("cannot read {}", crate::ui::rel(&path)))?;

        let manifest =
            Manifest::parse(&text).with_context(|| format!("in {}", crate::ui::rel(&path)))?;

        let entry = dir.join(&manifest.entry);
        let artifact = std::fs::read(&entry)
            .with_context(|| format!("cannot read {}", crate::ui::rel(&entry)))?;

        Self::build(manifest, &artifact, Some(dir))
    }

    /// Read the manifest and artifact of a worm, without an instance of either
    pub fn read_parts(dir: &Path) -> Result<(Manifest, Vec<u8>)> {
        let path = dir.join(MANIFEST);
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("cannot read {}", crate::ui::rel(&path)))?;

        let manifest =
            Manifest::parse(&text).with_context(|| format!("in {}", crate::ui::rel(&path)))?;

        let entry = dir.join(&manifest.entry);
        let artifact = std::fs::read(&entry)
            .with_context(|| format!("cannot read {}", crate::ui::rel(&entry)))?;

        Ok((manifest, artifact))
    }

    /// Give the settings to the worm once, before the worm sees a file
    pub fn init(
        &mut self,
        config: &toml::Value,
        rules: &std::collections::BTreeMap<String, toml::Value>,
        settings: &Settings,
    ) -> Result<()> {
        match &mut self.backend {
            Backend::Luau(worm) => worm.init(config, rules, settings),

            /*
            Every form receives the settings of the project, because every
            form can format and lint. The wasm form takes them through the
            optional `larvae_settings` export, so an old module runs
            unchanged.
            */
            Backend::Wasm(worm) => worm.init(&toml_text(config), &toml_text_map(rules), settings),

            Backend::Native(worm) => worm.init(&toml_text(config), &toml_text_map(rules), settings),
        }
    }

    /// Run one rule of the worm over the nodes that its filter matched
    pub fn run_rule(
        &mut self,
        rule: &str,
        index: u32,
        file: std::sync::Arc<ctx::FileCtx>,
        matched: &[u32],
    ) -> Result<Vec<crate::rules::edits::Edit>> {
        match &mut self.backend {
            Backend::Luau(worm) => worm.run_rule(rule, file, matched),

            Backend::Wasm(worm) => worm.run_rule(index, file, matched),

            /*
            This is not built, and by intent it is not simulated.

            The per node protocol crosses once per node. The native form
            exists to avoid that shape: 120 crossings per file over a pipe
            would be slower than the wasm path this form must beat. The rules
            of a native worm cross with [`Worm::run_rules_batched`], once per
            file.
            */
            Backend::Native(_) => {
                let _ = (rule, index, file, matched);

                anyhow::bail!(
                    "worm `{}` is native, whose rules run batched and not per node",
                    self.manifest.name
                )
            }
        }
    }

    /*
    Run every enabled rule of the worm over one file, in one crossing.

    Only the native form answers this. A pipe crossing costs 24 µs, and a
    rule worm visits about 120 nodes per file, so the batch is the shape that
    keeps this form fast. The in-process forms keep their per node calls,
    because a crossing costs them 1.2 µs in Luau and 6.9 µs in wasm.
    */
    pub fn run_rules_batched(
        &mut self,
        source: &str,
        rules: &[proto::RuleCall],
    ) -> Result<Vec<crate::rules::edits::Edit>> {
        match &mut self.backend {
            Backend::Native(worm) => worm.rules(source, rules),

            Backend::Luau(_) | Backend::Wasm(_) => bail!(
                "worm `{}` is {}, whose rules run one node at a time",
                self.manifest.name,
                self.manifest.form.name()
            ),
        }
    }

    /// The name of the worm, which is also its key under `[worms]` and `[config]`
    pub fn name(&self) -> &str {
        &self.manifest.name
    }

    /// Report if this worm claims the extension of `path` as a front-end
    pub fn claims(&self, path: &Path) -> bool {
        let Some(frontend) = &self.manifest.frontend else {
            return false;
        };

        let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
            return false;
        };

        frontend
            .claims
            .iter()
            .any(|claim| claim.strip_prefix('.').is_some_and(|c| c == ext))
    }

    /// Run the front-end of the worm over one file, with `[worms.<name>.config]` as TOML
    pub fn transform(&mut self, source: &str, config: &str) -> Result<Outcome> {
        match &mut self.backend {
            Backend::Luau(worm) => worm.transform(source, config),

            Backend::Wasm(worm) => worm.transform(source, config),

            /*
            A native worm reports a failure in its reply and does not trap.
            Thus a message that arrives is intact and is output. Each problem
            comes back as an error from the call.
            */
            Backend::Native(worm) => worm
                .transform(source)
                .map(|text| Outcome { text, ok: true }),
        }
        .with_context(|| format!("worm `{}`", self.manifest.name))
    }

    /// Format one claimed file, for the host to render in the project style
    pub fn format(&mut self, source: &str) -> Result<proto::FormatReply> {
        match &mut self.backend {
            Backend::Native(worm) => worm.format(source),

            Backend::Luau(worm) => worm.format(source),

            Backend::Wasm(worm) => worm.format(source),
        }
        .with_context(|| format!("worm `{}`", self.manifest.name))
    }

    /// Report the problems of one claimed file. The host decides the severity.
    pub fn lint(&mut self, source: &str) -> Result<proto::LintReply> {
        match &mut self.backend {
            Backend::Native(worm) => worm.lint(source),

            Backend::Wasm(worm) => worm.lint(source),

            Backend::Luau(worm) => worm.lint(source),
        }
        .with_context(|| format!("worm `{}`", self.manifest.name))
    }
}

/*
The path of the entry of a native worm.

Windows runs a file by its extension, and a manifest written on unix names
the entry without one. When the named file is absent and a sibling with
`.exe` exists, larvae takes the sibling. Thus one manifest serves every
platform, and a platform zip can still name an exact file.
*/
fn native_entry(dir: &Path, entry: &str) -> std::path::PathBuf {
    let plain = dir.join(entry);

    if cfg!(windows) && !plain.exists() {
        let exe = dir.join(format!("{entry}.exe"));

        if exe.exists() {
            return exe;
        }
    }

    plain
}

/*
Settings cross to a wasm worm as TOML text, because the ABI of that form takes
text. A Luau worm gets real tables instead. For this reason the function
appears on one side only. Larvae builds toml without its serializer, so this
code writes the scalars by hand.
*/
pub fn toml_text(value: &toml::Value) -> String {
    match value.as_table() {
        Some(table) => table
            .iter()
            .map(|(k, v)| format!("{k} = {}\n", scalar(v)))
            .collect(),

        None => String::new(),
    }
}

/// The same conversion, for the resolved rule map
pub fn toml_text_map(rules: &std::collections::BTreeMap<String, toml::Value>) -> String {
    rules
        .iter()
        .map(|(k, v)| format!("{k} = {}\n", scalar(v)))
        .collect()
}

fn scalar(value: &toml::Value) -> String {
    match value {
        toml::Value::String(s) => format!("{s:?}"),

        toml::Value::Integer(n) => n.to_string(),

        toml::Value::Float(f) => f.to_string(),

        toml::Value::Boolean(b) => b.to_string(),

        other => format!("{:?}", other.type_str()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_cross_to_a_wasm_worm_as_toml_text() {
        let value = toml::from_str::<toml::Value>("factory = \"vide\"\nstrict = true\n").unwrap();
        let text = toml_text(&value);

        assert!(text.contains("factory = \"vide\""), "{text}");
        assert!(text.contains("strict = true"), "{text}");
        assert_eq!(toml_text(&toml::Value::Boolean(true)), "");
    }

    fn write(dir: &Path, name: &str, body: &str) {
        std::fs::write(dir.join(name), body).unwrap();
    }

    /// The same contract, through the form that needs no toolchain
    fn luau_worm(dir: &Path) {
        write(
            dir,
            MANIFEST,
            r#"
name  = "echo"
api   = 1
form  = "luau"
entry = "init.luau"

[frontend]
claims = [".echo"]
"#,
        );
        write(
            dir,
            "init.luau",
            r#"
return {
    frontend = {
        compile = function(source, config)
            return source .. "|" .. config
        end,
    },
}
"#,
        );
    }

    #[test]
    fn a_luau_worm_loads_and_runs_from_a_directory() {
        let dir = tempfile::tempdir().unwrap();
        luau_worm(dir.path());

        let mut worm = Worm::load(dir.path()).unwrap();

        assert_eq!(worm.name(), "echo");
        assert_eq!(worm.transform("hi", "cfg").unwrap().text, "hi|cfg");
    }

    #[test]
    fn a_wasm_worm_loads_and_runs_from_a_directory() {
        let dir = tempfile::tempdir().unwrap();

        write(
            dir.path(),
            MANIFEST,
            r#"
name  = "echo"
api   = 1
form  = "wasm"
entry = "echo_worm.wasm"

[frontend]
claims = [".echo"]
"#,
        );
        std::fs::write(
            dir.path().join("echo_worm.wasm"),
            include_bytes!("../../tests/fixtures/echo_worm.wasm"),
        )
        .unwrap();

        let mut worm = Worm::load(dir.path()).unwrap();

        assert_eq!(worm.name(), "echo");
        assert_eq!(worm.transform("hi", "cfg").unwrap().text, "hi|cfg");
    }

    #[test]
    fn claims_match_on_extension() {
        let dir = tempfile::tempdir().unwrap();
        luau_worm(dir.path());

        let worm = Worm::load(dir.path()).unwrap();

        assert!(worm.claims(Path::new("src/App.echo")));
        assert!(!worm.claims(Path::new("src/App.luau")));
        assert!(!worm.claims(Path::new("src/echo")));
    }

    #[test]
    fn a_failure_names_the_worm() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            MANIFEST,
            r#"
name  = "grumpy"
api   = 1
form  = "luau"
entry = "init.luau"

[frontend]
claims = [".x"]
"#,
        );
        write(
            dir.path(),
            "init.luau",
            "return { frontend = { compile = function() return 42 end } }",
        );

        let err = Worm::load(dir.path())
            .unwrap()
            .transform("x", "")
            .unwrap_err();

        assert!(format!("{err:#}").contains("grumpy"), "{err:#}");
    }

    #[test]
    fn a_missing_manifest_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let err = Worm::load(dir.path()).err().unwrap();

        assert!(format!("{err:#}").contains("worm.toml"), "{err:#}");
    }
}
