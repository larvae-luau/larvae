/*!
`larvae lsp`, the language server that the editor extension talks to.

The server speaks the protocol directly and does not use a framework. The
reason is size: a runtime and a protocol crate would add several megabytes to
a binary whose main goal is a small size. Those crates provide the framing in
[`rpc`] and a dispatch table, and both are short.

The server is single threaded and synchronous, and this is not a limitation.
The server answers a request with a parse of one file, and a parse of one
file takes microseconds. The work that an async server overlaps does not
occur here.

The server reads all text from the document store: the editor sends the text
on every change. So the server never reads a file that the editor has open,
and never answers from a version that the user already edited past.

A worm of the project can claim an extension, for example `.luaux`. The
server sends such a file to its worm, and does not read the file as Luau. So
the editor shows the findings and the layout of the worm. Without this route,
the Luau parser reads the first markup character and reports a syntax error.
*/

pub mod analysis;
pub mod rpc;

mod actions;
/*
The three modules below answer a request from larvae's own parser, with no
Luau analyzer behind them. They are `pub` so the tests inside them run and
so nothing reads as dead where the analyzer feature is off.
*/
pub mod decorate;
mod diagnostics;
pub mod extend;
mod features;
pub mod instances;
pub mod navigate;
mod parity;
mod renames;
pub mod requires;
mod state;
pub mod structure;
pub mod studio;
mod studio_link;
#[cfg(test)]
mod tests;
pub mod tokens;
mod uri;
pub mod workspace;

use std::collections::HashMap;
use std::io::{BufReader, Write};
use std::path::PathBuf;

use anyhow::Result;
use serde_json::{Value, json};

use crate::config::Excludes;
use crate::fmt::FmtConfig;
use crate::lint::LintConfig;
use crate::worm::pool::Pool;

use state::no_worms;
use uri::uri_of;

pub fn run() -> Result<()> {
    run_with(None)
}

/*
The entry the larvae-lsp binary uses: the same server, with an analyzer
plugged into the seam. `larvae lsp` passes None and serves lint and
format, as it always did.
*/
pub fn run_with(analysis: Option<Box<dyn analysis::Analysis>>) -> Result<()> {
    run_pending(analysis.map(Pending::Ready))
}

/*
The server, with an analyzer that is possibly still being built.

Luau's type definitions take about fourteen seconds to load, and a session
cannot answer a type question until they are in. Doing that before the first
reply held the editor for the whole of it: a file opened, nothing happened,
and the editor showed no reason.

So the binary builds the session on a thread and hands over a receiver. The
server advertises what it will be able to do, answers what it can from its
own parser at once, and says "loading" to the rest until the session lands.
That is the shape luau-lsp has, which answers `initialize` in four
milliseconds and pays for the definitions on the first file.
*/
pub enum Pending {
    Ready(Box<dyn analysis::Analysis>),
    Builder(Builder),
}

/*
What builds a session, once the flags of the project are known.

Luau's flags are global to the process and some of them decide what a
session is: `LuauSolverV2` picks the type solver, and the globals of a
session are registered under whichever solver was on when it was built. So
the build cannot start before `initialize` says which project this is, and
the server hands the flags to this rather than the binary guessing them.
*/
pub type Builder =
    Box<dyn FnOnce(&crate::config::lsp::FFlagsConfig) -> Box<dyn analysis::Analysis> + Send>;

/*
What the loop waits on: a message from the editor, or the session landing.

The two arrive on different threads and the server has to answer both, so
they meet on one channel. The alternative was to look for the session on
each message, and that left a project whose editor went quiet with a
session it had built and never picked up.
*/
pub(super) enum Event {
    Message(Box<rpc::Message>),
    /// Typing paused; the editor is told to ask for its hints again
    Settled,
    Analysis(Box<dyn analysis::Analysis>),
    /// The editor closed the stream, or the read failed
    Eof,
}

pub fn run_pending(analysis: Option<Pending>) -> Result<()> {
    let stdout = std::io::stdout();
    let mut output = stdout.lock();

    let (events, inbox) = std::sync::mpsc::channel();

    let (ready, builder) = match analysis {
        Some(Pending::Ready(a)) => (Some(a), None),

        Some(Pending::Builder(build)) => (None, Some(build)),

        None => (None, None),
    };

    let reader = events.clone();

    std::thread::spawn(move || {
        let stdin = std::io::stdin();
        let mut input = BufReader::new(stdin.lock());

        loop {
            match rpc::read(&mut input) {
                Ok(Some(message)) => {
                    if reader.send(Event::Message(Box::new(message))).is_err() {
                        return;
                    }
                }

                // A closed stream is how an editor shuts a server down.
                _ => {
                    let _ = reader.send(Event::Eof);

                    return;
                }
            }
        }
    });

    /*
    The settle thread turns a stream of keystrokes into one pause. Every
    change posts a deadline; the thread waits out the newest one and then
    reports Settled, which becomes one hint refresh. It costs a thread
    that spends its life asleep.
    */
    let (settle, strokes) = std::sync::mpsc::channel::<std::time::Instant>();
    let settle_events = events.clone();

    std::thread::spawn(move || {
        while let Ok(mut deadline) = strokes.recv() {
            loop {
                let now = std::time::Instant::now();

                if deadline <= now {
                    break;
                }

                match strokes.recv_timeout(deadline - now) {
                    Ok(next) => deadline = next,

                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => break,

                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
                }
            }

            if settle_events.send(Event::Settled).is_err() {
                return;
            }
        }
    });

    let mut server = Server {
        analysis: std::cell::RefCell::new(ready),
        analysis_pending: builder.is_some(),
        builder,
        events: Some(events),
        settle: Some(settle),
        ..Default::default()
    };

    for event in inbox {
        match event {
            Event::Message(message) => {
                if server.handle(&message, &mut output)? {
                    break;
                }
            }

            Event::Analysis(built) => server.take_analysis(built, &mut output)?,

            /*
            The pause after typing. The held hints are stale by design,
            and this one refresh makes the editor ask again now that the
            text stopped moving.
            */
            Event::Settled => {
                if let Some(uri) = server.last_typed.take() {
                    server.refresh_dependents(&uri, &mut output)?;
                }

                if server.lsp.inlay_hints.update_delay > 0 {
                    rpc::request(&mut output, "workspace/inlayHint/refresh", Value::Null)?;
                }
            }

            Event::Eof => break,
        }
    }

    // The generator is this server's child; an orphan would outlive the editor.
    if let Some(mut watch) = server.sourcemap_watch.take() {
        state::stop_watch(&mut watch);
    }

    Ok(())
}

struct Server {
    /// Open documents, keyed by the uri that the editor gave them
    documents: HashMap<String, String>,
    root: Option<PathBuf>,
    fmt: FmtConfig,
    lint: LintConfig,
    /// The paths that `[lint] exclude` covers, so an excluded file stays quiet
    excluded: Excludes,
    /// The worms of the project. They own the files that they claim.
    worms: Pool,
    /// What the artifacts of the pool looked like at the last load
    worm_stamp: Vec<(std::path::PathBuf, Option<std::time::SystemTime>, u64)>,
    /// Why the pool last failed to build, so the message is said once
    worm_error: Option<String>,
    /// `shutdown` sets this, so a later `exit` is clean and not abrupt
    shutting_down: bool,
    /// The `[lsp]` table of the project; the default serves every Luau file
    lsp: crate::config::lsp::LspConfig,
    /// `[aliases]`, so a document link resolves a require the way the build does
    aliases: HashMap<String, String>,
    /*
    The DataModel map of the project, kept for the require completions.

    The analyzer gets its own copy for resolution. This one answers what a
    half-written `@game/` spec can become, which is a filesystem question and
    reaches no analyzer at all.
    */
    mounts: crate::requires::datamodel::MountTable,
    /*
    The project symbol index, for `workspace/symbol`.

    It is built once the editor says it is ready, and again on a save. A
    build re-reads and re-parses the tree, which measured 25ms over 300
    files, so a rebuild per request would be affordable and wasteful. A save
    is the moment the tree changed, and the moment the user is not waiting
    on a keystroke.
    */
    symbols: workspace::Index,
    /*
    The link the Roblox Studio plugin posts to, when the project asked for
    one. It owns a thread, so dropping the server stops the listener.
    */
    studio: Option<studio_link::Link>,
    /*
    The settings blob the editor sent, kept whole.

    The extension mirrors luau-lsp's ids, and the server knows only some of
    them. To keep the blob rather than the parsed few means a later feature
    reads its own setting without the editor having to send it again.
    */
    editor: Value,
    /// The analyzer behind the seam, when the binary provides one.
    /// A cell, because a publish borrows the server shared.
    analysis: std::cell::RefCell<Option<Box<dyn analysis::Analysis>>>,
    /*
    Whether a thread is still building the session.

    Until it lands, every type question answers that it is loading, which is
    a truer answer than nothing and one an editor can show.
    */
    analysis_pending: bool,
    /*
    What builds the session, until the config that decides its flags is read.

    `load_config` takes it and spawns it, because that is the first moment
    the project has been read. A server with no analyzer holds none.
    */
    builder: Option<Builder>,
    /// Where the builder thread posts the session it finished
    events: Option<std::sync::mpsc::Sender<Event>>,
    /// The rename dialog in flight, holding the edit its answer applies
    pending_rename: Option<renames::Pending>,
    /*
    Why a worm refused to lower a module, by file.

    The load hook writes it from the analyzer's thread, and the next
    publish pins the reason to every require that names the file. A
    successful load clears its entry.
    */
    load_errors: std::sync::Arc<std::sync::Mutex<std::collections::HashMap<PathBuf, String>>>,
    /// When each open document last changed, for the hint hold
    hint_hold: HashMap<String, std::time::Instant>,
    /// The last settled hints per document, served while the author types
    hint_cache: std::cell::RefCell<HashMap<String, Value>>,
    /// Wakes the settle thread on every change; it reports the pause back
    settle: Option<std::sync::mpsc::Sender<std::time::Instant>>,
    /// The document the last keystroke landed in, refreshed at the pause
    last_typed: Option<String>,
    /// The sourcemap generator this server owns, when the setting spawns one
    sourcemap_watch: Option<std::process::Child>,
    /// What the running generator was spawned with, to respawn only on change
    sourcemap_watch_params: Option<(PathBuf, Vec<String>)>,
    /// The did-not-start note is said once, not per config load
    sourcemap_watch_said: bool,
    /*
    The instance tree of the rojo sourcemap, as types.

    It is what makes `script.Providers` and `script.Parent.Config` resolve:
    the tree names the neighbours of each file, and the analyzer binds
    `script` per module to the type of its own node.
    */
    instances: instances::Instances,
    /// What the sourcemap looked like at the last read, so a rewrite reloads
    sourcemap_stamp: Option<(std::time::SystemTime, u64)>,
    /// Which `[lsp] sourcemap` value the last read used, so a rename reloads
    sourcemap_config: String,
    /*
    Whether a read happened at all.

    A missing sourcemap has no stamp, and so does a project the server has
    not looked at yet. Without this the two read the same and the server
    would re-read a file that is not there on every message.
    */
    sourcemap_read: bool,
    /*
    Which read of the sourcemap the current type names belong to.

    A reload declares the tree again, and a type name the global scope
    already holds is a redefinition error, so each read spells its names
    with a number of its own.
    */
    sourcemap_generation: u64,
}

impl Default for Server {
    fn default() -> Self {
        Self {
            documents: HashMap::new(),
            root: None,
            fmt: FmtConfig::default(),
            lint: LintConfig::default(),
            excluded: Excludes::default(),
            worms: no_worms(),
            worm_stamp: Vec::new(),
            worm_error: None,
            shutting_down: false,
            lsp: Default::default(),
            aliases: HashMap::new(),
            mounts: Default::default(),
            symbols: workspace::Index::default(),
            studio: None,
            editor: Value::Null,
            analysis: std::cell::RefCell::new(None),
            analysis_pending: false,
            builder: None,
            events: None,
            pending_rename: None,
            load_errors: Default::default(),
            hint_hold: HashMap::new(),
            hint_cache: Default::default(),
            settle: None,
            last_typed: None,
            sourcemap_watch: None,
            sourcemap_watch_params: None,
            sourcemap_watch_said: false,
            instances: instances::Instances::default(),
            sourcemap_stamp: None,
            sourcemap_config: String::new(),
            sourcemap_read: false,
            sourcemap_generation: 0,
        }
    }
}

impl Server {
    /// Returns true when the server must stop
    pub(super) fn handle(&mut self, message: &rpc::Message, out: &mut impl Write) -> Result<bool> {
        /*
        The sourcemap is checked before every message, because rojo rewrites
        it while the editor is open and the tree it describes is what a type
        question is answered against. The check is one stat, and the read
        only happens when the file changed.
        */
        self.refresh_instances();

        /*
        A response has no method. The one conversation the server starts
        is the rename dialog, and its answer routes by the id it carries.
        Handle answers true to stop the server, so a response answers
        false: the editor replying to a request must never read as the
        editor hanging up.
        */
        if message.method.is_empty() {
            if let Some(id) = &message.id {
                self.on_reply(id, &message.result, out)?;
            }

            return Ok(false);
        }

        match message.method.as_str() {
            "initialize" => {
                self.initialize(&message.params, out)?;
                self.notice_widened_serving(out)?;

                /*
                `[lsp] enabled = false` answers with no capabilities, so the
                editor sends nothing further and another server owns the
                files. The reply still comes, because a silent server looks
                crashed and the editor restarts it.
                */
                let caps = match self.lsp.enabled {
                    // What it will do, not what it can do this instant.
                    // What it will do: the analyzer half needs the seam AND the setting.
                    true => capabilities(self.will_analyse() && self.lsp.analyzer),

                    false => serde_json::json!({ "capabilities": {} }),
                };

                self.reply(message, out, caps)?;
            }

            "shutdown" => {
                self.shutting_down = true;

                self.reply(message, out, Value::Null)?;
            }

            "exit" => return Ok(true),

            "initialized" => self.reindex(),

            /*
            A configuration change can turn a lint on. So the server checks
            every open document again and does not wait for each edit. An
            editor that shows stale warnings after a settings change looks
            broken.
            */
            "workspace/didChangeConfiguration" => {
                // The editor sends the whole blob again, so the server takes it again.
                if !message.params["settings"].is_null() {
                    self.editor = message.params["settings"].clone();
                }

                self.load_config(out)?;

                for uri in self.documents.keys().cloned().collect::<Vec<_>>() {
                    self.publish(&uri, out)?;
                }

                // A setting can turn the hints on, and only the editor redraws them.
                rpc::request(out, "workspace/inlayHint/refresh", Value::Null)?;
            }

            "textDocument/didOpen" => {
                self.refresh_worms(out)?;

                let uri = uri_of(&message.params);
                let text = message.params["textDocument"]["text"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();

                self.documents.insert(uri.clone(), text);
                self.publish(&uri, out)?;
            }

            /*
            Full sync only, declared in the capabilities.

            Incremental sync would save the editor a send of the whole
            buffer, and would cost a rope and a patch path. For files of the
            size that Luau projects hold, a send of the text is cheaper than
            the machinery that avoids the send.
            */
            "textDocument/didChange" => {
                self.refresh_worms(out)?;

                let uri = uri_of(&message.params);

                if let Some(change) = message.params["contentChanges"]
                    .as_array()
                    .and_then(|c| c.last())
                    .and_then(|c| c["text"].as_str())
                {
                    if let Some(old) = self.documents.get(&uri) {
                        self.shift_hint_cache(&uri, &old.clone(), change);
                    }

                    self.documents.insert(uri.clone(), change.to_string());
                    self.publish(&uri, out)?;

                    /*
                    With a hold configured, the dependents wait for the
                    pause too: a keystroke should not re-check every open
                    file. Without one, they refresh here and now.
                    */
                    if !self.note_typing(&uri) {
                        self.refresh_dependents(&uri, out)?;
                    }
                }
            }

            "textDocument/didSave" => {
                self.refresh_worms(out)?;

                // The tree changed, and the user is not waiting on a keystroke.
                self.reindex();

                let uri = uri_of(&message.params);

                self.publish(&uri, out)?;
                self.refresh_dependents(&uri, out)?;
            }

            // The diagnostics clear with the document, or the editor keeps them on screen.
            "textDocument/didClose" => {
                let uri = uri_of(&message.params);
                self.documents.remove(&uri);

                rpc::notify(
                    out,
                    "textDocument/publishDiagnostics",
                    json!({ "uri": uri, "diagnostics": [] }),
                )?;
            }

            "textDocument/formatting" => {
                self.refresh_worms(out)?;

                let result = self.format(&uri_of(&message.params));

                match result {
                    Ok(edits) => self.reply(message, out, edits)?,

                    // A file in the middle of an edit does not parse. A report on
                    // every keystroke would be noise, so the format request declines.
                    Err(_) => self.reply(message, out, Value::Null)?,
                }
            }

            "textDocument/hover" => {
                let result = self.hover(&message.params);

                self.reply(message, out, result)?;
            }

            "textDocument/completion" => {
                let result = self.completions(&message.params);

                self.reply(message, out, result)?;
            }

            /*
            The requests larvae answers from its own parser.

            Each one replies even when it has nothing, because an error and
            an empty answer read the same to a user and differently to an
            editor, which logs a failure and can stop asking.
            */
            "textDocument/definition" => {
                let result = self.definition(&message.params);

                self.reply(message, out, result)?;
            }

            "textDocument/semanticTokens/full" => {
                let result = self.semantic_tokens(&message.params);

                self.reply(message, out, result)?;
            }

            "workspace/symbol" => {
                let result = self.workspace_symbols(&message.params);

                self.reply(message, out, result)?;
            }

            "workspace/didRenameFiles" => {
                self.on_did_rename(&message.params, out)?;
            }

            "textDocument/signatureHelp" => {
                let result = self.signature_help(&message.params);

                self.reply(message, out, result)?;
            }

            "textDocument/inlayHint" => {
                let result = self.inlay_hints(&message.params);

                self.reply(message, out, result)?;
            }

            /*
            The tooltip of one hint, on demand. Hovering a hint shows the
            hint's own tooltip and never a textDocument/hover, so a hint
            over a linted require showed the lint alone. The resolve
            answers with the same card a hover on the annotated name gives,
            so the type overview rides the hint too.
            */
            "inlayHint/resolve" => {
                let mut hint = message.params.clone();

                let anchor = json!({
                    "textDocument": { "uri": hint["data"]["uri"] },
                    "position": {
                        "line": hint["position"]["line"],
                        "character": hint["position"]["character"]
                            .as_u64()
                            .unwrap_or(0)
                            .saturating_sub(1),
                    },
                });

                let card = self.hover(&anchor);

                if let Some(value) = card["contents"]["value"].as_str() {
                    hint["tooltip"] = json!({ "kind": "markdown", "value": value });
                }

                /*
                The deferred accept: the type names an alias, and the
                printer writes a required module's alias bare, so the
                name can be one this file never sees. The probe proves
                it: the type lands in a throwaway alias at the end of
                the file, and only a clean check attaches the edit.
                */
                if hint["textEdits"].is_null()
                    && let Some(insert) = hint["data"]["insert"].as_str().map(str::to_owned)
                    && let Some(uri) = hint["data"]["uri"].as_str().map(str::to_owned)
                    && self.type_resolves_here(&uri, &insert)
                {
                    hint["textEdits"] = json!([{
                        "range": {
                            "start": hint["position"],
                            "end": hint["position"],
                        },
                        "newText": insert,
                    }]);
                }

                self.reply(message, out, hint)?;
            }

            "textDocument/typeDefinition" => {
                let result = self.type_definition(&message.params);

                self.reply(message, out, result)?;
            }

            "textDocument/references" => {
                let result = self.references(&message.params);

                self.reply(message, out, result)?;
            }

            "textDocument/documentHighlight" => {
                let result = self.highlights(&message.params);

                self.reply(message, out, result)?;
            }

            "textDocument/rename" => {
                let result = self.rename(&message.params);

                self.reply(message, out, result)?;
            }

            "textDocument/foldingRange" => {
                let result = self.folding_ranges(&message.params);

                self.reply(message, out, result)?;
            }

            "textDocument/selectionRange" => {
                let result = self.selection_ranges(&message.params);

                self.reply(message, out, result)?;
            }

            "textDocument/documentLink" => {
                let result = self.links(&message.params);

                self.reply(message, out, result)?;
            }

            "textDocument/documentColor" => {
                let result = self.colors(&message.params);

                self.reply(message, out, result)?;
            }

            "textDocument/colorPresentation" => {
                let result = self.color_presentation(&message.params);

                self.reply(message, out, result)?;
            }

            "textDocument/documentSymbol" => {
                let uri = uri_of(&message.params);

                /*
                An excluded or claimed file answers with nothing, and that
                rule lives with the flat version. So the decline is asked
                first and the tree is built only for a file larvae serves.
                */
                let symbols = match self.symbols(&uri) {
                    Value::Array(list) if list.is_empty() => json!([]),

                    _ => self.symbol_tree(&uri),
                };

                self.reply(message, out, symbols)?;
            }

            /*
            larvae's own actions come first, then the worms'. The worms have
            nothing to offer yet, and an empty list is still the right reply:
            an empty list and an error read the same to a user and are not
            the same to an editor, which logs a failure on every keystroke
            that opens the lightbulb.
            */
            "textDocument/codeAction" => {
                self.refresh_worms(out)?;

                let uri = uri_of(&message.params);
                let text = self.documents.get(&uri).cloned().unwrap_or_default();
                let range = message.params["range"].clone();

                let mut actions = actions::for_range(&uri, &text, &range, &self.lint);

                actions.extend(extend::code_actions(&self.worms, &uri, &text, &range));

                self.reply(message, out, Value::Array(actions))?;
            }

            /*
            The compiled form of a document, under larvae's own names.

            The editor's command asks here, with an optimization level, and
            shows the answer in a read-only document. luau-lsp serves the
            same two views under its own prefix.
            */
            "larvae/bytecode" => {
                let result = self.bytecode(&message.params, false);

                self.reply(message, out, result)?;
            }

            "larvae/compilerRemarks" => {
                let result = self.bytecode(&message.params, true);

                self.reply(message, out, result)?;
            }

            /*
            A request of larvae's own, under its own name, because no method
            in the protocol carries this. A worm that teaches larvae a new
            kind of module supplies the type of that module, and an editor
            that wants those types asks here.
            */
            "larvae/definitions" => {
                self.refresh_worms(out)?;

                let reply = extend::definitions_reply(&self.worms);

                self.reply(message, out, reply)?;
            }

            // All other methods get an answer only if the message expects one.
            _ => {
                if let Some(id) = &message.id {
                    rpc::respond_error(out, id, format!("{} is not supported", message.method))?;
                }
            }
        }

        Ok(false)
    }

    fn reply(&self, message: &rpc::Message, out: &mut impl Write, result: Value) -> Result<()> {
        match &message.id {
            Some(id) => rpc::respond(out, id, result),

            // A notification wants no reply, and a reply is a protocol error.
            None => Ok(()),
        }
    }
}

/// The abilities of this server; the editor then asks only for these
fn capabilities(analysis: bool) -> Value {
    let mut caps = json!({
        // 1 is full sync, see the note on didChange
        "textDocumentSync": { "openClose": true, "change": 1, "save": true },
        "documentFormattingProvider": true,
        "documentSymbolProvider": true,
        /*
        A worm will offer the actions. Larvae advertises the capability
        now and answers with an empty list, because an editor decides at
        initialize whether to ever ask, and a capability that appears
        later needs the client to be told and to agree to re register.
        */
        "codeActionProvider": true,
        /*
        These nine come from larvae's own parser and scope resolution, so
        they are advertised whether or not the binary carries an analyzer.
        A capability that appears later needs the client to be told and to
        agree to re register, and most do not, so the answer at initialize
        has to be the true one.
        */
        "definitionProvider": true,
        "workspaceSymbolProvider": true,
        /*
        The legend comes from the module that fills it, so the two cannot
        drift. An editor reads a token type by its index in this list, and a
        server that advertised a different order would paint every file
        wrong.
        */
        "semanticTokensProvider": {
            "legend": {
                "tokenTypes": tokens::legend().types,
                "tokenModifiers": tokens::legend().modifiers,
            },
            "full": true,
        },
        "referencesProvider": true,
        "documentHighlightProvider": true,
        "renameProvider": true,
        "foldingRangeProvider": true,
        "selectionRangeProvider": true,
        "documentLinkProvider": { "resolveProvider": false },
        "colorProvider": true,
        /*
        The rename notice, so a moved file can carry its requires along.
        The editor tells the server after the move, the server asks the
        user with one dialog, and the edit applies on the answer.
        */
        "workspace": {
            "fileOperations": renames::capabilities(),
        },
    });

    /*
    Hover and completion exist only through the analyzer, so a server
    without one does not advertise them. The editor then never asks, and
    stock luau-lsp answers instead when both servers run.
    */
    if analysis {
        caps["hoverProvider"] = json!(true);
        // Only the frontend knows where a type was declared.
        caps["typeDefinitionProvider"] = json!(true);
        // Both read the type graph, so neither means anything without it.
        caps["signatureHelpProvider"] = json!({ "triggerCharacters": ["(", ","] });
        // Resolve fills a hint's tooltip on demand, when the editor asks.
        caps["inlayHintProvider"] = json!({ "resolveProvider": true });
        /*
        Every string mark opens a require spec, and a project that writes
        `'` got no list at all: the editor asks on a trigger character, and
        only `"` was one. `/` re-asks after a directory, so a path completes
        a segment at a time without the author retyping anything, and `@`
        opens the aliases.
        */
        caps["completionProvider"] = json!({
            "triggerCharacters": [".", ":", "\"", "'", "`", "/", "@"],
        });
    }

    json!({
        "capabilities": caps,
        "serverInfo": { "name": "larvae", "version": env!("CARGO_PKG_VERSION") },
    })
}
