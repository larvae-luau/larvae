/*!
`[lsp]`, how the editor server behaves.

The plan for the server is a claim-only default once the Luau analyzer
lands: answer for the files that worms claim, and coexist with luau-lsp on
the rest. Today's server lints and formats plain Luau too, and projects
rely on that, so `claim_only` defaults to off until the analyzer era, and
the flip will be a stated breaking change.
*/

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub struct LspConfig {
    /// Off answers every request with nothing, so another server owns the files
    #[serde(default = "on")]
    pub enabled: bool,

    /// On serves only the files that a worm claims; off serves every Luau file
    #[serde(default)]
    pub claim_only: bool,

    /// What the completion list offers, and how it writes what it inserts
    #[serde(default)]
    pub completion: CompletionConfig,

    /// The types the editor draws in a line the author left unannotated
    #[serde(default)]
    pub inlay_hints: InlayHintsConfig,

    /// The call signature the editor shows while the author types arguments
    #[serde(default)]
    pub signature_help: SignatureHelpConfig,

    /// What a hover card carries
    #[serde(default)]
    pub hover: HoverConfig,

    /// The project wide symbol index that `workspace/symbol` searches
    #[serde(default)]
    pub index: IndexConfig,

    /// The link the Roblox Studio plugin talks to
    #[serde(default)]
    pub studio: StudioConfig,
}

/*
`[lsp.studio]`, the link to the Roblox Studio plugin.

The plugin mirrors the live DataModel into larvae, so the type checker knows
the instances the place actually holds and not only the ones a sourcemap
describes.

Off by default, and this default is a decision rather than caution. To turn
it on opens a listening socket, and a tool that opens one without being asked
has changed what it is. The socket binds to loopback and to nothing else, so
the tree never leaves the machine, but any process on that machine can still
reach it. A user who wants the link says so.
*/
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub struct StudioConfig {
    #[serde(default)]
    pub enabled: bool,

    /// The port the plugin posts to. The plugin ships with the same number.
    #[serde(default = "studio_port")]
    pub port: u16,
}

fn studio_port() -> u16 {
    3773
}

impl Default for StudioConfig {
    fn default() -> Self {
        toml::from_str("").expect("every field has a default")
    }
}

/*
`[lsp.index]`, mirroring luau-lsp's `index.*`.

The index reads and parses every Luau file the project holds. That measured
25ms over 300 files, which is nothing on a save and something on a very
large tree, so a project that feels it turns the index off and keeps
`documentSymbol` for the file in front of it.
*/
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub struct IndexConfig {
    #[serde(default = "on")]
    pub enabled: bool,
}

impl Default for IndexConfig {
    fn default() -> Self {
        toml::from_str("").expect("every field has a default")
    }
}

/*
`[lsp.inlay_hints]`, mirroring luau-lsp's `inlayHints.*`.

Off by default, every one of them. A hint is text the editor draws into a
line the author did not write, and a reader who did not ask for that reads
it as the file changing under them. luau-lsp defaults them off for the same
reason.
*/
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub struct InlayHintsConfig {
    /// The inferred type of a local the author left unannotated
    #[serde(default)]
    pub variable_types: bool,

    /// The inferred type of a parameter the author left unannotated
    #[serde(default)]
    pub parameter_types: bool,

    /*
    How long a hint can be before it is cut.

    A hint longer than the code it annotates hides the code. luau-lsp uses
    50 and that number reads well, so larvae takes it rather than invent a
    different one.
    */
    #[serde(default = "fifty")]
    pub type_hint_max_length: usize,
}

fn fifty() -> usize {
    50
}

/// `[lsp.signature_help]`, mirroring luau-lsp's `signatureHelp.*`
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub struct SignatureHelpConfig {
    #[serde(default = "on")]
    pub enabled: bool,
}

impl Default for SignatureHelpConfig {
    fn default() -> Self {
        toml::from_str("").expect("every field has a default")
    }
}

/// `[lsp.hover]`, mirroring luau-lsp's `hover.*`
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub struct HoverConfig {
    #[serde(default = "on")]
    pub enabled: bool,
}

impl Default for HoverConfig {
    fn default() -> Self {
        toml::from_str("").expect("every field has a default")
    }
}

/*
`[lsp.completion]`, which mirrors luau-lsp's `completion.*` settings.

The names match luau-lsp's, with larvae's snake_case spelling, so a user who
moves between the two servers keeps the setting they already know. The editor
extension exposes the same ids under `larvae-lsp.`, and this table is the
project side of them. Where both speak, the project wins.
*/
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub struct CompletionConfig {
    /// Off answers every completion request with an empty list
    #[serde(default = "on")]
    pub enabled: bool,

    /// Offer the keywords that fit the position, beside the names
    #[serde(default = "on")]
    pub show_keywords: bool,

    #[serde(default)]
    pub imports: ImportsConfig,
}

impl Default for CompletionConfig {
    fn default() -> Self {
        toml::from_str("").expect("every field has a default")
    }
}

/// `[lsp.completion.imports]`, the auto-import settings
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub struct ImportsConfig {
    /// Offer a service or module the file has not imported yet
    #[serde(default = "on")]
    pub enabled: bool,

    /*
    Whether an auto-import writes `const` or `local`.

    On by default, which is a deliberate departure from luau-lsp. That server
    defaults it off because Luau had no `const` when the setting was written.
    Larvae's platform has the keyword, and an auto-import is the clearest case
    for it: the line binds a service or a module and nothing reassigns it.

    A project that has not adopted `const` sets this off, and the completion
    writes `local` instead. The setting is its own thing and does not read
    `[fmt] require_binding`, because that option governs a `require` binding
    and a `game:GetService` line is not one. To tie them would make the
    formatter decide what the editor types.
    */
    #[serde(default = "on")]
    pub use_const: bool,
}

impl Default for ImportsConfig {
    fn default() -> Self {
        toml::from_str("").expect("every field has a default")
    }
}

impl ImportsConfig {
    /// The keyword an auto-import writes
    pub fn keyword(&self) -> &'static str {
        match self.use_const {
            true => "const",

            false => "local",
        }
    }
}

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

    #[test]
    fn an_unknown_key_is_refused_like_everywhere_else() {
        assert!(toml::from_str::<LspConfig>("clam_only = true").is_err());
    }
}
