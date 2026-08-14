//! The CLI definition, the dispatch, and the fastfetch style help layout

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Result;
use clap::{CommandFactory, FromArgMatches, Parser, Subcommand};

use crate::commands::{self, self_cmd::SelfCommand};
use crate::{art, ui};
#[derive(Parser)]
#[command(
    name = "larvae",
    version,
    about = "One toolchain for all of Luau: transforms today, formatting and linting next",
    disable_help_flag = true
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    /// Print help
    #[arg(short = 'h', long = "help", action = clap::ArgAction::SetTrue, global = true)]
    help: bool,
}

#[derive(Subcommand)]
enum Command {
    /// Process the project and rewrite requires into the output directory
    Process {
        /// Path to larvae.toml (defaults to ./larvae.toml when present)
        #[arg(long)]
        config: Option<PathBuf>,
        /// Merge [profile.<name>] over the config before building
        #[arg(long)]
        profile: Option<String>,
        /// Rebuild when a source file changes; Ctrl-C stops it
        #[arg(long, short)]
        watch: bool,
    },

    /// Validate requires and syntax without writing any output
    Check {
        /// Path to larvae.toml (defaults to ./larvae.toml when present)
        #[arg(long)]
        config: Option<PathBuf>,
        /// Merge [profile.<name>] over the config before checking
        #[arg(long)]
        profile: Option<String>,
    },

    /// Format Luau source in place
    Fmt {
        /// Files or directories; the default is the input of the project
        paths: Vec<PathBuf>,
        /// Write nothing; exit non zero if a file would change
        #[arg(long)]
        check: bool,
        /// Read one file from stdin and write the result to stdout
        #[arg(long, conflicts_with_all = ["paths", "check"])]
        stdin: bool,
        /// The path the stdin content has, so a worm can claim it
        #[arg(long, requires = "stdin")]
        stdin_filepath: Option<PathBuf>,
        /// Path to larvae.toml (defaults to ./larvae.toml when present)
        #[arg(long)]
        config: Option<PathBuf>,
    },

    /// Report suspicious code; do not change it
    Lint {
        /// Files or directories; the default is the input of the project
        paths: Vec<PathBuf>,
        /// Read one file from stdin
        #[arg(long, conflicts_with = "paths")]
        stdin: bool,
        /// The path the stdin content has, so a worm can claim it
        #[arg(long, requires = "stdin")]
        stdin_filepath: Option<PathBuf>,
        /// Describe one lint and stop
        #[arg(long, value_name = "LINT")]
        explain: Option<String>,
        /// Path to larvae.toml (defaults to ./larvae.toml when present)
        #[arg(long)]
        config: Option<PathBuf>,
    },

    /// Serve diagnostics and formatting to an editor over stdio
    Lsp,

    /// Create a starter larvae.toml for this project
    Init,

    /// Develop a worm before publication or installation
    Worm {
        #[command(subcommand)]
        command: commands::worm::WormCommand,
    },

    /// Manage the larvae installation
    #[command(name = "self")]
    SelfManage {
        #[command(subcommand)]
        command: Option<SelfCommand>,
    },
}

pub fn main() -> ExitCode {
    match run() {
        Ok(code) => code,

        Err(e) => {
            ui::print_error(&format!("{e:#}"));

            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<ExitCode> {
    let matches = Cli::command().styles(ui::help_styles()).get_matches();
    let cli = Cli::from_arg_matches(&matches)?;
    let root = std::env::current_dir()?;

    if cli.help {
        // A sub -h prints the help of that subcommand; a bare -h gets the fancy layout.
        let mut chain = Vec::new();
        let mut level = &matches;

        // Follow the full path, so `self update -h` lands on update and not on self.
        while let Some((name, sub)) = level.subcommand() {
            chain.push(name.to_string());
            level = sub;
        }

        if chain.is_empty() {
            print_fancy_help(ui::want_color())?;

            return Ok(ExitCode::SUCCESS);
        }

        let mut cmd = Cli::command().styles(ui::help_styles());
        let mut target = &mut cmd;

        for name in &chain {
            target = target
                .find_subcommand_mut(name)
                .expect("parsed subcommand exists");
        }

        if let Err(e) = target.print_help()
            && e.kind() != std::io::ErrorKind::BrokenPipe
        {
            return Err(e.into());
        }

        return Ok(ExitCode::SUCCESS);
    }

    // A bare `larvae` also gets the fastfetch style logo + help layout.
    let Some(command) = cli.command else {
        print_fancy_help(ui::want_color())?;

        return Ok(ExitCode::SUCCESS);
    };

    match command {
        Command::Process {
            config,
            profile,
            watch,
        } => commands::process::run(&root, config, profile, watch),

        Command::Check { config, profile } => commands::check::run(&root, config, profile),

        Command::Fmt {
            paths,
            check,
            stdin,
            stdin_filepath,
            config,
        } => commands::fmt::run(&root, paths, check, stdin, stdin_filepath, config),

        Command::Lint {
            paths,
            stdin,
            stdin_filepath,
            explain,
            config,
        } => commands::lint::run(&root, paths, stdin, stdin_filepath, explain, config),

        Command::Lsp => {
            crate::lsp::run()?;

            Ok(ExitCode::SUCCESS)
        }

        Command::Init => commands::init::run(&root),

        Command::Worm { command } => commands::worm::run(command),

        Command::SelfManage { command } => match command {
            Some(cmd) => commands::self_cmd::run(&root, cmd),

            None => {
                // A bare `larvae self` shows the help of the group.
                let mut cmd = Cli::command().styles(ui::help_styles());
                let sub = cmd.find_subcommand_mut("self").expect("self exists");

                if let Err(e) = sub.print_help()
                    && e.kind() != std::io::ErrorKind::BrokenPipe
                {
                    return Err(e.into());
                }

                Ok(ExitCode::SUCCESS)
            }
        },
    }
}

/// fastfetch style help: logo left, gradient help text right, stacked when the terminal is narrow
fn print_fancy_help(color: bool) -> Result<()> {
    use std::io::Write;

    const GAP: usize = 3;

    let logo = art::logo_small(color);
    let logo_lines: Vec<&str> = logo.lines().collect();

    let logo_w = logo_lines
        .iter()
        .map(|l| ui::visible_width(l))
        .max()
        .unwrap_or(0);

    let width = ui::term_width();
    let side_by_side = width >= logo_w + GAP + 50;

    let help_width = if side_by_side {
        (width - logo_w - GAP).min(80)
    } else {
        width.min(100)
    };

    // The help text gets gradient colors; clap only adds bold markers.
    let mut cmd = Cli::command()
        .styles(ui::bold_styles())
        .term_width(help_width);
    let rendered = cmd.render_help();

    let help = if color {
        rendered.ansi().to_string()
    } else {
        rendered.to_string()
    };

    let help_lines: Vec<&str> = help.lines().collect();
    let mut out = String::new();

    if side_by_side {
        let rows = logo_lines.len().max(help_lines.len());

        // Center the shorter column vertically against the taller one.
        let logo_off = (rows - logo_lines.len()) / 2;
        let help_off = (rows - help_lines.len()) / 2;

        for row in 0..rows {
            let logo_line = row
                .checked_sub(logo_off)
                .and_then(|i| logo_lines.get(i).copied())
                .unwrap_or("");

            let help_line = row
                .checked_sub(help_off)
                .and_then(|i| help_lines.get(i).copied())
                .unwrap_or("");

            let pad = logo_w - ui::visible_width(logo_line);
            out.push_str(logo_line);

            for _ in 0..pad + GAP {
                out.push(' ');
            }

            if color && !help_line.is_empty() {
                // The row gradient color, applied again after each clap reset so the line keeps its color.
                let c = ui::fg(art::row_color(row, rows));
                let reapplied = help_line.replace(ui::RESET, &format!("{}{c}", ui::RESET));

                out.push_str(&c);
                out.push_str(&reapplied);
                out.push_str(ui::RESET);
            } else {
                out.push_str(help_line);
            }

            // Remove trailing whitespace on logo only rows.
            while out.ends_with(' ') {
                out.pop();
            }

            out.push('\n');
        }
    } else {
        out.push_str(&logo);
        out.push_str("\n\n");

        if color {
            let rows = help_lines.len();

            for (row, line) in help_lines.iter().enumerate() {
                let c = ui::fg(art::row_color(row, rows));
                let reapplied = line.replace(ui::RESET, &format!("{}{c}", ui::RESET));

                out.push_str(&c);
                out.push_str(&reapplied);
                out.push_str(ui::RESET);
                out.push('\n');
            }
        } else {
            out.push_str(&help);

            if !out.ends_with('\n') {
                out.push('\n');
            }
        }
    }

    // A closed pipe (ex: `larvae | head`) is not an error
    if let Err(e) = std::io::stdout().write_all(out.as_bytes())
        && e.kind() != std::io::ErrorKind::BrokenPipe
    {
        return Err(e.into());
    }

    Ok(())
}
