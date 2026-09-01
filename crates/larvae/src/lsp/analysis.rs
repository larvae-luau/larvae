/*!
The seam where a type analyzer plugs into the server.

The server in this crate lints and formats. The larvae-lsp binary adds
Luau's real analysis frontend, and this trait is the whole boundary
between them: the server calls these five methods and knows nothing about
the shim, the C++, or the vendored build behind them. `larvae lsp` runs
with no analyzer and serves exactly what it always served.

Positions cross this boundary as byte offsets, in both directions. The
line and column conversion of the protocol happens in the server, once,
at the edge.
*/

use std::path::Path;

/// One diagnostic from the analyzer, byte addressed
#[derive(Debug)]
pub struct AnalysisDiag {
    pub span: (u32, u32),
    /// 1 is Error and 2 is Warning, the numbering of the protocol
    pub severity: u8,
    pub message: String,
    pub code: Option<String>,
}

/// One completion the analyzer offers at a position
pub struct AnalysisCompletion {
    pub label: String,
    /// The protocol's CompletionItemKind, ex: 5 is Field, 3 is Function
    pub kind: u8,
    /*
    The type of the entry, rendered.

    An editor draws it to the right of the label, and it is how a reader
    tells a function from a field without accepting either one. A keyword
    carries none, because a keyword has no type.
    */
    pub detail: Option<String>,
    /*
    The argument names of a function, ex: `(self, className)`.

    An editor draws it against the label itself, before the detail. So a
    row reads `IsA (self, className)  (Object, string) -> boolean`, which
    is what a reader picking from a list needs: the names to fill in and
    the types they take.
    */
    pub label_detail: Option<String>,
    /// What the editor writes on accept, when it differs from the label
    pub insert_text: Option<String>,
    /// The comment block above the declaration, as markdown
    pub documentation: Option<String>,
    /// Whether the declaration carries `@deprecated`
    pub deprecated: bool,
    /*
    Whether the entry fits the type the position expects.

    0 is no, 1 is yes, and 2 is a function whose result fits. It is what
    ranks the props of a component above every global in scope, which is the
    difference between a useful list and an alphabet. luau-lsp ranks on the
    same answer.
    */
    pub type_correct: u8,
    /// Whether the entry comes through an index the type does not take
    pub wrong_index_type: bool,
}

/*
The module hooks the server installs before the first request.

Resolve answers a require spec from a module, or passes. Load answers the
text the analyzer should see for a path the hooks resolved, with the span
map back onto the original. Both run on the analyzer's hot path, so the
implementations behind them are resident worms, not spawns.
*/
pub type ResolveHook = Box<dyn Fn(&Path, &str) -> Option<String> + Send>;
pub type LoadHook = Box<dyn Fn(&str) -> Option<String> + Send>;

pub struct ModuleHooks {
    pub resolve: ResolveHook,
    pub load: LoadHook,
    /*
    The extensions a resolving worm claims, without the dot.

    A require that lands on one of these is a module, because the worm
    behind it hands the analyzer a lowering and a type. A require that lands
    on any other non-Luau file is a path Luau cannot load, and it says so.
    Both halves are needed: the worm has to claim the extension and to
    answer resolution, or nothing is there to read the file.
    */
    pub claims: Vec<String>,
}

/*
The plain-Luau view of larvae source, with every byte offset kept.

The analyzer is stock Luau and stops at larvae's own syntax. `const` is
the common case, and `local` has the same five bytes, so the swap keeps
the offsets identical and no position maps. The rule mirrors the parser:
`const` before a name or `function` is the keyword. Source that does not
lex returns unchanged, and the analyzer recovers as it does today.
*/
pub fn plain_view(src: &str) -> std::borrow::Cow<'_, str> {
    use crate::syntax::lexer::TokKind;

    let Ok(lexed) = crate::syntax::lexer::lex(src) else {
        return std::borrow::Cow::Borrowed(src);
    };

    /*
    A reserved word never names a binding, so `const` before one is the
    identifier in an expression, ex: `local x = const` before `return`.
    */
    let reserved = |text: &str| {
        matches!(
            text,
            "and"
                | "break"
                | "do"
                | "else"
                | "elseif"
                | "end"
                | "false"
                | "for"
                | "if"
                | "in"
                | "local"
                | "nil"
                | "not"
                | "or"
                | "repeat"
                | "return"
                | "then"
                | "true"
                | "until"
                | "while"
        )
    };

    let starts: Vec<usize> = lexed
        .toks
        .windows(2)
        .filter(|pair| {
            matches!(pair[0].kind, TokKind::Ident)
                && matches!(pair[1].kind, TokKind::Ident)
                && pair[0].text(src) == "const"
                && !reserved(pair[1].text(src))
        })
        .map(|pair| pair[0].start as usize)
        .collect();

    if starts.is_empty() {
        return std::borrow::Cow::Borrowed(src);
    }

    let mut out = src.to_string();

    for start in starts {
        out.replace_range(start..start + 5, "local");
    }

    std::borrow::Cow::Owned(out)
}

/*
Where a declaration sits, in the units the protocol wants.

Line and character, and not a byte offset, because the answer often names a
module the server has no text for. To convert would mean reading that file
only to count its lines.
*/
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnalysisLocation {
    pub path: std::path::PathBuf,
    pub start: (u32, u32),
    pub end: (u32, u32),
}

/// One call signature, for `textDocument/signatureHelp`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnalysisSignature {
    pub label: String,
    pub parameters: Vec<String>,
    /// Which parameter the caret sits on, so the editor bolds the right one
    pub active: u32,
}

/// One inlay hint, at the place in the text where the author left a type out
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnalysisHint {
    pub line: u32,
    pub character: u32,
    pub label: String,
    /// 1 is a type, 2 is a parameter name, as the protocol numbers them
    pub kind: u8,
}

pub trait Analysis: Send {
    /// Install the module hooks; the server calls this once per worm load
    fn set_module_hooks(&mut self, hooks: ModuleHooks) {
        let _ = hooks;
    }

    /*
    The service names the platform knows, for auto-import completions.

    The analyzer reads them from its definitions, so the list and the
    types cannot drift. A server without an analyzer offers no service
    imports, which is the honest answer.
    */
    fn services(&mut self) -> Vec<String> {
        Vec::new()
    }

    /*
    Give the analyzer the DataModel map of the project.

    `@game/...` is an absolute spec: it is rooted at the DataModel and reads
    nothing from the file that writes it. So the resolver needs the map, and
    it needs it for every file, not only the files a sourcemap covers.

    A server without a map still resolves the relative forms, which is the
    honest answer for a project that has no rojo project and no mounts.
    */
    /*
    The `[aliases]` of the project, for the resolver behind the seam.

    `.luaurc` is not the only file that names an alias, and a project
    that writes them in `larvae.toml` alone still means them. The
    build has read both since the beginning; this is the editor and
    `larvae analyze` reading the same pair.
    */
    fn set_aliases(&mut self, aliases: std::collections::HashMap<String, String>) {
        let _ = aliases;
    }

    fn set_mounts(&mut self, mounts: crate::requires::datamodel::MountTable) {
        let _ = mounts;
    }

    /// Load one .d.luau declaration into the global scope
    fn definitions(&mut self, name: &str, source: &str) -> bool {
        let _ = (name, source);

        false
    }

    /// Drop the project documentation loaded by an earlier config
    fn clear_documentation(&mut self) {}

    /// Add one luau-lsp documentationFiles JSON database
    fn documentation(&mut self, source: &str) -> bool {
        let _ = source;

        false
    }

    /*
    Say what `script` is inside each file of the project.

    `script` names a different instance in every module, so a global
    declaration cannot answer it. The sourcemap names the instance behind
    each file, and [`crate::lsp::instances`] turns that into one declared
    type per node; this hands the frontend the file-to-type map.

    The map replaces whatever the analyzer held. A sourcemap that rojo
    rewrote describes a different tree, and half of the old one is worse
    than none of it.
    */
    fn set_script_types(&mut self, types: &std::collections::HashMap<std::path::PathBuf, String>) {
        let _ = types;
    }

    /*
    Say which rig `Player.Character` types to.

    The platform types it as `Model?`, which knows no body parts. The
    analyzer swaps the property for the rig type the project chose, so
    `player.Character.Humanoid` resolves with no cast.
    */
    fn set_character_type(&mut self, kind: crate::config::lsp::CharacterType) {
        let _ = kind;
    }

    /// Give the analyzer the text of one open document
    fn open(&mut self, path: &Path, text: &str);

    /// Type diagnostics for one document
    fn check(&mut self, path: &Path) -> Vec<AnalysisDiag>;

    /*
    The type at a byte offset, rendered for a hover card.

    `show_table_kinds` keeps the `{| |}` markers that say a table is sealed.
    They matter to somebody writing a type and to nobody reading one, so the
    caller passes what the project asked for.
    */
    fn hover(
        &mut self,
        path: &Path,
        at: u32,
        show_table_kinds: bool,
        include_string_length: bool,
    ) -> Option<String>;

    /*
    The reference page for the name at a byte offset, as markdown.

    A type says what a thing is and the reference says what it does. Every
    Roblox class and member has a page, and luau-lsp puts it under the type
    on the card, which is where a reader looks for it. A name the project
    wrote itself has no page, and answers nothing.
    */
    fn hover_documentation(&mut self, path: &Path, at: u32) -> Option<String> {
        let _ = (path, at);

        None
    }

    /// Completions at a byte offset
    fn completions(&mut self, path: &Path, at: u32) -> Vec<AnalysisCompletion>;

    /*
    Where the name at a byte offset is declared.

    This is the half that larvae's own resolver cannot answer. A local
    resolves without a type checker, and `navigate` does that. A name that
    comes through a require, a method on an imported table, or a global from
    the definitions needs the frontend, and only the analyzer has one.
    */
    fn definition(&mut self, path: &Path, at: u32) -> Option<AnalysisLocation> {
        let _ = (path, at);

        None
    }

    /// Where the TYPE of the name at a byte offset is declared
    fn type_definition(&mut self, path: &Path, at: u32) -> Option<AnalysisLocation> {
        let _ = (path, at);

        None
    }

    /// The signature of the call that encloses a byte offset
    fn signature(&mut self, path: &Path, at: u32) -> Option<AnalysisSignature> {
        let _ = (path, at);

        None
    }

    /*
    The types the author left out, for the whole module.

    Every kind renders as a hint of the protocol, so only the collector
    behind this can tell the kinds apart, and the flags carry the project's
    settings down to where the split exists. `names` is luau-lsp's own
    scale: 0 none, 1 the literal arguments, 2 every argument.
    */
    fn hints(
        &mut self,
        path: &Path,
        variables: bool,
        parameters: bool,
        returns: bool,
        names: u8,
    ) -> Vec<AnalysisHint> {
        let _ = (path, variables, parameters, returns, names);

        Vec::new()
    }

    /*
    Apply Luau's own feature flags, in the order that makes a later step win.

    Every flag on, then the project's overrides, then the values larvae
    cannot work without. The names it did not recognise come back, because a
    flag Luau renamed is a setting that quietly stopped working and only the
    user can fix it.
    */
    fn set_flags(&mut self, flags: &crate::config::lsp::FFlagsConfig) -> Vec<String> {
        let _ = flags;

        Vec::new()
    }

    /*
    The compiled form of one module, rendered for a reader.

    `remarks` picks the second view: the source annotated with what the
    compiler chose, rather than the instruction listing. Both answer the
    question `[lsp.bytecode]` exists for, and luau-lsp serves both.

    The compiler is self-contained, so the input is text and not a path:
    no module graph takes part, and a claimed file passes its lowering.
    */
    fn bytecode(
        &mut self,
        source: &str,
        optimization: u8,
        remarks: bool,
        config: &crate::config::lsp::BytecodeConfig,
    ) -> Option<String> {
        let _ = (source, optimization, remarks, config);

        None
    }

    /// Drop the cached state of one document and its dependents
    fn invalidate(&mut self, path: &Path);

    /*
    The deprecated uses of one module, as hint diagnostics.

    The editor draws them as a strikethrough. An analyzer that cannot
    say has nothing struck through, which is the honest default.
    */
    fn deprecated_uses(&mut self, path: &Path) -> Vec<AnalysisDiag> {
        let _ = path;

        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::plain_view;

    #[test]
    fn const_becomes_local_at_the_same_offset() {
        let src = "const card = require(\"./card\")\nconst function f() end\n";
        let out = plain_view(src);

        assert_eq!(
            out.as_ref(),
            "local card = require(\"./card\")\nlocal function f() end\n"
        );
        assert_eq!(out.len(), src.len());
    }

    #[test]
    fn const_as_a_value_stays() {
        let src = "local x = const\nreturn { const = 1 }\n";

        assert_eq!(plain_view(src).as_ref(), src);
    }

    #[test]
    fn plain_source_borrows() {
        let src = "local x = 1\n";

        assert!(matches!(plain_view(src), std::borrow::Cow::Borrowed(_)));
    }
}
