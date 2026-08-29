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

    /*
    Off serves what larvae always served, and nothing the analyzer adds.

    The lints, the format, the code actions, and the outline stay, on
    claimed and plain files alike. Hover, completion, type diagnostics, and
    the rest of the Luau parity go quiet, and the session is never built,
    which is the serving larvae had before the analyzer landed.
    */
    #[serde(default = "on")]
    pub analyzer: bool,

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

    /// Luau's own feature flags, for the analyzer
    #[serde(default)]
    pub fflags: FFlagsConfig,

    /// How the analyzer would compile, for a build that reads bytecode
    #[serde(default)]
    pub bytecode: BytecodeConfig,

    /*
    What `Player.Character` types to.

    Roblox types it as `Model?`, which knows no body parts, so every rig
    access needs a cast. A project knows which rig it uses, and the types
    `R15Character` and `R6Character` carry the parts. `not_set` types it as
    the union of the two, for a place that allows both.
    */
    #[serde(default)]
    pub character_type: CharacterType,

    /*
    The rojo sourcemap, read for the instance tree of the project.

    The path is relative to the root of the project. The file rojo writes
    by default is the default here, and a project that keeps it elsewhere
    names it. The server reads it when it is there and says nothing when it
    is not, because a project without rojo has no sourcemap and wants none.
    */
    #[serde(default = "sourcemap")]
    pub sourcemap: String,

    /*
    Keep the sourcemap fresh by running rojo, the way luau-lsp does.

    On, the server spawns `rojo sourcemap <project> --watch` when the
    project file is there and rojo is on the path, so a new file or
    folder types without anyone regenerating by hand. A project that
    runs its own watch turns this off and nothing spawns.
    */
    #[serde(default = "on")]
    pub sourcemap_autogenerate: bool,

    /// The rojo project the sourcemap generates from
    #[serde(default = "rojo_project")]
    pub rojo_project_file: String,
}

fn rojo_project() -> String {
    "default.project.json".to_string()
}

fn sourcemap() -> String {
    "sourcemap.json".to_owned()
}

/// The rig `Player.Character` types to
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CharacterType {
    #[default]
    R15,
    R6,
    /// The union of the two rigs, for a place that allows both
    NotSet,
}

/*
`[lsp.fflags]`, Luau's own feature flags.

Luau ships a change behind a flag long before the flag flips, so a language
server that reads only the defaults reads an older Luau than the one it
links. luau-lsp turns them all on for that reason, and it defaults that on.

Larvae defaults it off, and the difference is deliberate. larvae ships one
pinned Luau and the same binary to everyone, so a flag that misbehaves
misbehaves for every user at once and there is no fallback to a system
install. A project that wants the newer behaviour asks for it.

Order of application, which luau-lsp also follows: every flag on, then this
table's overrides, then the two values larvae cannot work without. A later
step wins, which is why the required values go last.
*/
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub struct FFlagsConfig {
    /// Turn on every Luau analysis flag that Luau does not mark experimental
    #[serde(default)]
    pub enable_by_default: bool,

    /// Turn on Luau's new type solver
    #[serde(default)]
    pub enable_new_solver: bool,

    /*
    One value per flag name, which wins over the two switches above.

    The value is text because Luau keeps a boolean list and an integer list,
    and the name decides which one is asked. `"true"` and `"120"` are both
    written the same way here.
    */
    #[serde(default)]
    pub over: std::collections::BTreeMap<String, String>,
}

/*
`[lsp.bytecode]`, how the compiled views compile.

`larvae/bytecode` and `larvae/compilerRemarks` read them: the editor sends a
document and an optimization level, and this table supplies the rest, so the
listing matches what a build of this project would run.
*/
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub struct BytecodeConfig {
    /// 0 none, 1 lines, 2 lines and locals
    #[serde(default = "one")]
    pub debug_level: u8,

    /// 0 none, 1 the types the compiler knows
    #[serde(default = "one")]
    pub type_info_level: u8,

    /// The library a vector literal comes from, for a project with its own
    #[serde(default = "vector3")]
    pub vector_lib: String,

    #[serde(default = "new_")]
    pub vector_ctor: String,

    #[serde(default = "vector3")]
    pub vector_type: String,
}

fn one() -> u8 {
    1
}

fn vector3() -> String {
    "Vector3".to_string()
}

fn new_() -> String {
    "new".to_string()
}

impl Default for BytecodeConfig {
    fn default() -> Self {
        toml::from_str("").expect("every field has a default")
    }
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
#[derive(Debug, Clone, Deserialize, Serialize)]
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

    /// The inferred return type of a function, after its parameter list
    #[serde(default)]
    pub function_return_types: bool,

    /// The name of each parameter, before the argument that fills it
    #[serde(default)]
    pub parameter_names: ParameterNames,

    /*
    How long the hints hold still while the author types, in milliseconds.

    A hint request that lands mid-edit answers with the last settled
    hints, and one refresh follows the pause, so the text stops jumping
    under the cursor. Zero turns the hold off and every request computes.
    */
    #[serde(default = "update_delay")]
    pub update_delay: u64,
}

fn update_delay() -> u64 {
    700
}

/// Which call arguments get a parameter name drawn before them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ParameterNames {
    /// No call site gets one. luau-lsp's default, and so larvae's.
    #[default]
    None,

    /// Only a literal argument, where the value says nothing about the name.
    Literals,

    All,
}

impl ParameterNames {
    /// The scale the analyzer seam speaks: 0 none, 1 literals, 2 all.
    pub fn mode(self) -> u8 {
        match self {
            Self::None => 0,

            Self::Literals => 1,

            Self::All => 2,
        }
    }
}

/*
Parsed from nothing, like the others, so the field defaults hold.

A derived Default zeroes the length, because serde's field defaults apply
only while parsing. The zero then cut every hint down to `...` the moment a
project left the setting out.
*/
impl Default for InlayHintsConfig {
    fn default() -> Self {
        toml::from_str("").expect("every field has a default")
    }
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

    /*
    Keep the `{| |}` markers that say a table is sealed.

    Off, as luau-lsp has it. The marker answers a question somebody writing a
    type asks, and a reader hovering a value is not asking it.
    */
    #[serde(default)]
    pub show_table_kinds: bool,

    /*
    Say how long a string literal is.

    On, as luau-lsp has it. A reader hovering `"Loaded"` knows it is a
    string; the length is the thing they cannot count at a glance.
    */
    #[serde(default = "on")]
    pub include_string_length: bool,
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

    /*
    Drop the deprecated entries from the list entirely.

    Off by default: a deprecated member still exists, the strikethrough
    already says what the platform thinks of it, and hiding it is a
    stance a project takes on purpose.
    */
    #[serde(default)]
    pub hide_deprecated: bool,

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

    /*
    How an auto-imported require spells its path.

    luau-lsp's ids, mapped onto string requires. Its `nearestAbsolute`
    anchors an instance path on the nearest stable ancestor; the string
    equivalent of that anchor is an alias when one covers the module and
    the `@game` absolute when none does.
    */
    #[serde(default)]
    pub require_style: RequireStyle,
}

/// How `[lsp.completion.imports] require_style` spells a require.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RequireStyle {
    /// An alias where one covers the module, the relative form elsewhere.
    #[default]
    Auto,

    AlwaysRelative,

    /// The `@game` absolute; the relative form where no mount covers it.
    AlwaysAbsolute,

    /// The shortest stable anchor: an alias, then `@game`, then relative.
    NearestAbsolute,
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

    /*
    The defaults of a left-out subtable are the parsed defaults.

    A derived Default gave the hints a maximum length of zero, so every
    hint rendered as `...` for a project that never touched the setting.
    */
    #[test]
    fn a_left_out_subtable_keeps_its_field_defaults() {
        assert_eq!(LspConfig::default().inlay_hints.type_hint_max_length, 50);
        assert_eq!(InlayHintsConfig::default().type_hint_max_length, 50);
        assert_eq!(
            toml::from_str::<LspConfig>("enabled = true")
                .expect("parses")
                .inlay_hints
                .type_hint_max_length,
            50
        );
    }

    #[test]
    fn an_unknown_key_is_refused_like_everywhere_else() {
        assert!(toml::from_str::<LspConfig>("clam_only = true").is_err());
    }
}
