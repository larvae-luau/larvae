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

#[cfg(all(test, feature = "analyzer"))]
mod roblox_enum_tests;

// Pure path logic, so it compiles and tests without the vendored C++.
mod resolve;

fn main() -> std::process::ExitCode {
    /*
    `analyze` runs the analyzer once and prints, no server involved.
    `larvae analyze` spawns this binary with the same arguments, because
    the analyzer is compiled in here and not into the CLI.
    */
    let args: Vec<String> = std::env::args().skip(1).collect();

    if args.first().map(String::as_str) == Some("analyze") {
        return analyze(&args[1..]);
    }

    /*
    `--version` answers and stops, so a check that the server starts is
    a check and not a session that reads an empty stdin.

    The analyzer is a library this binary links at load, so a loader
    that refuses it kills the process before this line. That is what
    the caller is testing: an exit code, and the loader's own words on
    stderr when there are any.
    */
    if matches!(args.first().map(String::as_str), Some("--version" | "-V")) {
        println!("larvae-lsp {}", env!("CARGO_PKG_VERSION"));

        return std::process::ExitCode::SUCCESS;
    }

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
    let analysis = Some(larvae::lsp::Pending::Builder(Box::new(|cfg| {
        /*
        The flags go in first, on this thread, before the session exists.
        `LuauSolverV2` decides which type solver the globals are registered
        under, so a session built before the project was read would be built
        under the wrong one and the setting would do nothing.
        */
        analyzer::apply_flags(&cfg.fflags);

        Box::new(analyzer::LuauAnalysis::with_security(
            cfg.roblox_security_level,
        )) as Box<dyn larvae::lsp::analysis::Analysis>
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

#[cfg(feature = "analyzer")]
fn analyze(args: &[String]) -> std::process::ExitCode {
    let run = || -> anyhow::Result<std::process::ExitCode> {
        let opts = larvae::commands::analyze::parse(args)?;

        // The flags go in before the session exists, as the server orders it.
        let lsp = larvae::commands::analyze::lsp_config(&opts)?;
        analyzer::apply_flags(&lsp.fflags);

        let analysis = Box::new(analyzer::LuauAnalysis::with_security(
            lsp.roblox_security_level,
        )) as Box<dyn larvae::lsp::analysis::Analysis>;

        larvae::commands::analyze::engine(analysis, &opts)
    };

    match run() {
        Ok(code) => code,

        Err(e) => {
            eprintln!("larvae analyze: {e:#}");

            std::process::ExitCode::FAILURE
        }
    }
}

#[cfg(not(feature = "analyzer"))]
fn analyze(_args: &[String]) -> std::process::ExitCode {
    eprintln!("larvae analyze: this build carries no analyzer");

    std::process::ExitCode::FAILURE
}
