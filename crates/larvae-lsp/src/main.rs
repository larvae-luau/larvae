/*!
The larvae language server binary.

The server itself lives in the larvae crate, and `larvae lsp` runs it with
lint and format alone. This binary plugs Luau's analysis frontend into the
server's seam, so hover, completions, and type diagnostics join in. Built
without the `analyzer` feature, the binary is the same server as the
subcommand, which keeps a plain workspace build away from the C++.
*/

#[cfg(feature = "analyzer")]
mod analyzer;

// Pure path logic, so it compiles and tests without the vendored C++.
mod resolve;

fn main() -> std::process::ExitCode {
    /*
    The session is built on a thread, because Luau's type definitions take
    about fourteen seconds to load and the editor should not wait for them.

    The server answers `initialize` at once, serves everything its own parser
    can while the load runs, and says "loading" to the type questions until
    the session arrives. luau-lsp does the same and answers `initialize` in
    four milliseconds.
    */
    #[cfg(feature = "analyzer")]
    let analysis = {
        let (tx, rx) = std::sync::mpsc::channel();

        std::thread::spawn(move || {
            let built =
                Box::new(analyzer::LuauAnalysis::new()) as Box<dyn larvae::lsp::analysis::Analysis>;

            // The server is gone if this fails, and there is nothing to do about it.
            let _ = tx.send(built);
        });

        Some(larvae::lsp::Pending::Building(rx))
    };

    #[cfg(not(feature = "analyzer"))]
    let analysis = None;

    match larvae::lsp::run_pending(analysis) {
        Ok(()) => std::process::ExitCode::SUCCESS,

        Err(e) => {
            eprintln!("larvae-lsp: {e:#}");

            std::process::ExitCode::FAILURE
        }
    }
}
