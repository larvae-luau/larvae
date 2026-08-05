/*!
Worms, the extensions named under `[worms]` in `larvae.toml`.

A worm ships as a release zip holding a [`Manifest`] and one artifact, which is
either Luau source or a `wasm32` module. Both forms answer the same contract, so
everything above this module is written once and neither the pipeline nor the
config layer knows which form it is talking to.
*/

pub mod ctx;
pub mod host;
pub mod luau;
pub mod manifest;
pub mod nodes;
pub mod registry;

use std::path::Path;

use anyhow::{Context, Result};

pub use host::{Outcome, WasmWorm};
pub use luau::LuauWorm;
pub use manifest::{Form, Frontend, Manifest, RequireOwner, RuleDecl};

/// The ABI revision this host speaks, matching `api` in a worm's `worm.toml`
pub const ABI_VERSION: u32 = 1;

/// The manifest filename inside a worm's zip
pub const MANIFEST: &str = "worm.toml";

/// A loaded worm of either form
enum Backend {
    Luau(Box<LuauWorm>),
    Wasm(Box<WasmWorm>),
}

/// A worm, loaded and ready to be called once per file
pub struct Worm {
    /// What the worm declared about itself
    pub manifest: Manifest,
    backend: Backend,
}

impl Worm {
    /// Load a worm from an unpacked directory holding `worm.toml` and its artifact
    pub fn load(dir: &Path) -> Result<Self> {
        let path = dir.join(MANIFEST);
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("cannot read {}", crate::ui::rel(&path)))?;

        let manifest =
            Manifest::parse(&text).with_context(|| format!("in {}", crate::ui::rel(&path)))?;

        let entry = dir.join(&manifest.entry);

        let backend = match manifest.form {
            Form::Luau => {
                let source = std::fs::read_to_string(&entry)
                    .with_context(|| format!("cannot read {}", crate::ui::rel(&entry)))?;

                Backend::Luau(Box::new(LuauWorm::load(&source, &manifest.name)?))
            }

            Form::Wasm => {
                let bytes = std::fs::read(&entry)
                    .with_context(|| format!("cannot read {}", crate::ui::rel(&entry)))?;

                Backend::Wasm(Box::new(WasmWorm::load(&bytes)?))
            }
        };

        Ok(Self { manifest, backend })
    }

    /// The worm's name, which is also its key under `[worms]` and `[config]`
    pub fn name(&self) -> &str {
        &self.manifest.name
    }

    /// Whether this worm claims `path`'s extension as a front-end
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

    /// Run the worm's front-end over one file, with `[config.<name>]` as TOML
    pub fn transform(&mut self, source: &str, config: &str) -> Result<Outcome> {
        match &mut self.backend {
            Backend::Luau(worm) => worm.transform(source, config),

            Backend::Wasm(worm) => worm.transform(source, config),
        }
        .with_context(|| format!("worm `{}`", self.manifest.name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, name: &str, body: &str) {
        std::fs::write(dir.join(name), body).unwrap();
    }

    /// The same contract, reached through the form that needs no toolchain
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
