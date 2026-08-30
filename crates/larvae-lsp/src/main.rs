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
    The session is built on a thread, because Luau's type definitions take a
    few seconds to load and the editor should not wait for them.

    The server answers `initialize` at once, serves everything its own parser
    can while the load runs, and says "Loading..." to the type questions until
    the session arrives. luau-lsp does the same and answers `initialize` in
    four milliseconds.

    The server starts the build, and not this function: the flags below decide
    what a session is, and the project that sets them is only known once the
    editor has said where it is.
    */
    #[cfg(feature = "analyzer")]
    let analysis = Some(larvae::lsp::Pending::Builder(Box::new(|flags| {
        /*
        The flags go in first, on this thread, before the session exists.
        `LuauSolverV2` decides which type solver the globals are registered
        under, so a session built before the project was read would be built
        under the wrong one and the setting would do nothing.
        */
        analyzer::apply_flags(flags);

        Box::new(analyzer::LuauAnalysis::new()) as Box<dyn larvae::lsp::analysis::Analysis>
    })));

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
