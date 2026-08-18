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

pub mod rpc;

mod diagnostics;
mod features;
mod state;
#[cfg(test)]
mod tests;
mod uri;

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
    let stdin = std::io::stdin();
    let mut input = BufReader::new(stdin.lock());
    let stdout = std::io::stdout();
    let mut output = stdout.lock();

    let mut server = Server::default();

    while let Some(message) = rpc::read(&mut input)? {
        if server.handle(&message, &mut output)? {
            break;
        }
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
    /// `shutdown` sets this, so a later `exit` is clean and not abrupt
    shutting_down: bool,
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
            shutting_down: false,
        }
    }
}

impl Server {
    /// Returns true when the server must stop
    pub(super) fn handle(&mut self, message: &rpc::Message, out: &mut impl Write) -> Result<bool> {
        match message.method.as_str() {
            "initialize" => {
                self.initialize(&message.params, out)?;

                self.reply(message, out, capabilities())?;
            }

            "shutdown" => {
                self.shutting_down = true;

                self.reply(message, out, Value::Null)?;
            }

            "exit" => return Ok(true),

            "initialized" => {}

            /*
            A configuration change can turn a lint on. So the server checks
            every open document again and does not wait for each edit. An
            editor that shows stale warnings after a settings change looks
            broken.
            */
            "workspace/didChangeConfiguration" => {
                self.load_config(out)?;

                for uri in self.documents.keys().cloned().collect::<Vec<_>>() {
                    self.publish(&uri, out)?;
                }
            }

            "textDocument/didOpen" => {
                self.refresh_worms();

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
                self.refresh_worms();

                let uri = uri_of(&message.params);

                if let Some(change) = message.params["contentChanges"]
                    .as_array()
                    .and_then(|c| c.last())
                    .and_then(|c| c["text"].as_str())
                {
                    self.documents.insert(uri.clone(), change.to_string());
                    self.publish(&uri, out)?;
                }
            }

            "textDocument/didSave" => {
                self.refresh_worms();

                let uri = uri_of(&message.params);

                self.publish(&uri, out)?;
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
                self.refresh_worms();

                let result = self.format(&uri_of(&message.params));

                match result {
                    Ok(edits) => self.reply(message, out, edits)?,

                    // A file in the middle of an edit does not parse. A report on
                    // every keystroke would be noise, so the format request declines.
                    Err(_) => self.reply(message, out, Value::Null)?,
                }
            }

            "textDocument/documentSymbol" => {
                let symbols = self.symbols(&uri_of(&message.params));

                self.reply(message, out, symbols)?;
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
fn capabilities() -> Value {
    json!({
        "capabilities": {
            // 1 is full sync, see the note on didChange
            "textDocumentSync": { "openClose": true, "change": 1, "save": true },
            "documentFormattingProvider": true,
            "documentSymbolProvider": true,
        },
        "serverInfo": { "name": "larvae", "version": env!("CARGO_PKG_VERSION") },
    })
}
