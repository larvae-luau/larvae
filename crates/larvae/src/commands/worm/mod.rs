//! `larvae worm <command>` supports worm development before a user can install the worm.

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Result;
use clap::Subcommand;

mod develop;
mod manage;

use develop::{info, run_worm, types};
use manage::{add, install, remove};

/// Luau type definitions for the worm API; `larvae worm types` writes them
pub const TYPES: &str = include_str!("../../worm/worm.d.luau");

/// The path where `types` writes when the user gives no path
const TYPES_FILE: &str = "worm.d.luau";

/// The definition files setting of luau-lsp, a map of package name to path
const DEFINITIONS: &str = "luau-lsp.types.definitionFiles";

/// The package name for the types, so the entry of a second tool can sit beside it
const PACKAGE: &str = "larvae-worm";

#[derive(Subcommand)]
pub enum WormCommand {
    /// Run a worm from a directory over one file; the run needs no project or install
    Run {
        /// Directory that holds worm.toml and its artifact
        worm: PathBuf,
        /// File to pass through the worm
        file: PathBuf,
        /// TOML given to the worm as its [worms.<name>.config] table
        #[arg(long)]
        config: Option<PathBuf>,
        /// Write the result here instead of to stdout
        #[arg(long, short)]
        out: Option<PathBuf>,
        /// Format the file; do not transform it
        #[arg(long)]
        fmt: bool,
        /// Lint the file; do not transform it
        #[arg(long, conflicts_with = "fmt")]
        lint: bool,
    },

    /// Report what a worm declares; do not run it
    Info {
        /// Directory that holds worm.toml and its artifact
        worm: PathBuf,
    },

    /// Write the Luau type definitions for worm authors
    Types {
        /// The output path; the default is ./worm.d.luau
        #[arg(long, short)]
        out: Option<PathBuf>,
        /// Print to stdout; do not write a file
        #[arg(long)]
        stdout: bool,
    },

    /// Add a worm to larvae.toml at its newest release
    Add {
        /// A known name such as `luaux`, or `owner/repo`, either with an optional @version
        spec: String,
        /// Take it from crates.io instead of a GitHub release
        #[arg(long)]
        cargo: bool,
        /// The key to write it under; the default comes from the repo
        #[arg(long)]
        name: Option<String>,
        /// The config to edit; the default is ./larvae.toml
        #[arg(long)]
        config: Option<PathBuf>,
    },

    /// Install every worm that larvae.toml names
    #[command(alias = "i")]
    Install {
        /// The config to read; the default is ./larvae.toml
        #[arg(long)]
        config: Option<PathBuf>,
        /// Install again, even where the version is already on disk
        #[arg(long)]
        force: bool,
    },

    /// Remove a worm from larvae.toml and delete what it installed
    #[command(alias = "rm")]
    Remove {
        /// The key it is written under
        name: String,
        /// The config to edit; the default is ./larvae.toml
        #[arg(long)]
        config: Option<PathBuf>,
        /// Edit the config and leave the installed files alone
        #[arg(long)]
        keep_files: bool,
    },
}

pub fn run(cmd: WormCommand) -> Result<ExitCode> {
    match cmd {
        WormCommand::Run {
            worm,
            file,
            config,
            out,
            fmt,
            lint,
        } => run_worm(&worm, &file, config.as_deref(), out.as_deref(), fmt, lint),

        WormCommand::Info { worm } => info(&worm),

        WormCommand::Types { out, stdout } => types(out.as_deref(), stdout),

        WormCommand::Add {
            spec,
            cargo,
            name,
            config,
        } => add(&spec, cargo, name, config),

        WormCommand::Install { config, force } => install(config, force),

        WormCommand::Remove {
            name,
            config,
            keep_files,
        } => remove(&name, config, keep_files),
    }
}
