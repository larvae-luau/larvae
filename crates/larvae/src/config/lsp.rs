/*!
`[lsp]`, how the editor server behaves.

The server lints and formats, and luau-lsp beside it serves the types. So
this table is short: whether the server runs, which files it serves, and the
editor settings of the worms.
*/

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct LspConfig {
    /// Off answers every request with nothing, so another server owns the files
    #[serde(default = "on")]
    pub enabled: bool,

    /// On serves only the files that a worm claims; off serves every Luau file
    #[serde(default)]
    pub claim_only: bool,

    /*
    The editor settings of the worms: `[lsp.oop]` holds the settings of
    the worm named oop. The keys are free here because each worm
    declares its own under `[options]` in its manifest; the server
    checks every key against that declaration and says so when one does
    not fit. A key that names no worm gets the same message, which is
    what `deny_unknown_fields` used to answer with a parse error.
    */
    #[serde(default, flatten)]
    pub worms: std::collections::BTreeMap<String, toml::Value>,
}

/*
The keys of the analyzer, which left larvae in 0.10.

A project that still sets one parses, because a config that stopped loading
over a dead setting would cost more than the setting. The server says once
that the key does nothing, and names luau-lsp as the new owner.
*/
pub const RETIRED: &[&str] = &[
    "analyzer",
    "completion",
    "inlay_hints",
    "signature_help",
    "hover",
    "color_picker",
    "index",
    "studio",
    "fflags",
    "bytecode",
    "character_type",
    "sourcemap",
    "sourcemap_autogenerate",
    "rojo_project_file",
    "sourcemap_command",
    "roblox_security_level",
    "definitions",
    "documentation",
];

fn on() -> bool {
    true
}

impl Default for LspConfig {
    fn default() -> Self {
        toml::from_str("").expect("every field has a default")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_server_is_on_and_serves_everything_by_default() {
        let c = LspConfig::default();

        assert!(c.enabled);
        assert!(!c.claim_only);
    }

    /*
    An unknown key parses into the worm-settings map, and the server
    refuses it there, naming it in a warning. The parse cannot refuse
    it any more, because `[lsp.oop]` is legal and the worm names are
    not known until the pool loads.
    */
    #[test]
    fn an_unknown_key_lands_in_the_worm_settings() {
        let cfg = toml::from_str::<LspConfig>("clam_only = true").expect("parses");

        assert_eq!(
            cfg.worms.get("clam_only"),
            Some(&toml::Value::Boolean(true))
        );
    }

    /// A retired table still parses, so the project loads and hears why.
    #[test]
    fn a_retired_key_still_parses() {
        let cfg =
            toml::from_str::<LspConfig>("[fflags]\nenable_new_solver = true\n").expect("parses");

        assert!(cfg.worms.contains_key("fflags"));
        assert!(RETIRED.contains(&"fflags"));
    }
}
