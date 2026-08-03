//! CLI definition, dispatch, and the fastfetch style help layout

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
    /// Process the project, rewrite requires into the output directory
    Process {
        /// Path to larvae.toml (defaults to ./larvae.toml when present)
        #[arg(long)]
        config: Option<PathBuf>,
        /// Merge [profile.<name>] over the config before building
        #[arg(long)]
        profile: Option<String>,
        /// Rebuild whenever a source file changes, Ctrl-C to stop
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

    /// Create a starter larvae.toml for this project
    Init,

    /// Add the schema reference to larvae.toml for editor intellisense
    Schema,

    /// Manage the larvae installation itself
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
        // sub -h prints that subcommand's help, bare -h gets the fancy layout
        let mut chain = Vec::new();
        let mut level = &matches;

        // follow the whole path so `self update -h` lands on update, not self
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

    // Bare `larvae` gets the fastfetch style logo + help layout too
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

        Command::Init => commands::init::run(&root),

        Command::Schema => commands::schema::run(&root),

        Command::SelfManage { command } => match command {
            Some(cmd) => commands::self_cmd::run(cmd),

            None => {
                // Bare `larvae self`, show the group's help
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

/// fastfetch style help, logo left, gradient help text right, stacked when the terminal is narrow
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

    // help text gets gradient colors, clap only adds bold markers
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

        // Vertically center the shorter column against the taller one
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
                // row gradient color, reapplied after each clap reset so the line stays painted
                let c = ui::fg(art::row_color(row, rows));
                let reapplied = help_line.replace(ui::RESET, &format!("{}{c}", ui::RESET));

                out.push_str(&c);
                out.push_str(&reapplied);
                out.push_str(ui::RESET);
            } else {
                out.push_str(help_line);
            }

            // Avoid trailing whitespace on logo only rows
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
