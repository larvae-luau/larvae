//! `larvae worm <command>`, for developing a worm before anyone can install it

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::Subcommand;

use crate::ui;
use crate::worm::Worm;

/// Luau type definitions for the worm API, written by `larvae worm types`
pub const TYPES: &str = include_str!("../worm/worm.d.luau");

/// Where `types` writes when nothing is asked for
const TYPES_FILE: &str = "worm.d.luau";

/// luau-lsp's definition files, a map of package name to path
const DEFINITIONS: &str = "luau-lsp.types.definitionFiles";

/// The package name our types load under, so a second tool's entry sits beside it
const PACKAGE: &str = "larvae-worm";

#[derive(Subcommand)]
pub enum WormCommand {
    /// Run a worm from a directory over one file, no project or install needed
    Run {
        /// Directory holding worm.toml and its artifact
        worm: PathBuf,
        /// File to pass through the worm
        file: PathBuf,
        /// TOML handed to the worm as its [config.<name>] table
        #[arg(long)]
        config: Option<PathBuf>,
        /// Write the result here instead of to stdout
        #[arg(long, short)]
        out: Option<PathBuf>,
    },

    /// Report what a worm declares, without running it
    Info {
        /// Directory holding worm.toml and its artifact
        worm: PathBuf,
    },

    /// Write the Luau type definitions for worm authors
    Types {
        /// Where to write, defaults to ./worm.d.luau
        #[arg(long, short)]
        out: Option<PathBuf>,
        /// Print to stdout instead of writing a file
        #[arg(long)]
        stdout: bool,
    },
}

pub fn run(cmd: WormCommand) -> Result<ExitCode> {
    match cmd {
        WormCommand::Run {
            worm,
            file,
            config,
            out,
        } => run_worm(&worm, &file, config.as_deref(), out.as_deref()),

        WormCommand::Info { worm } => info(&worm),

        WormCommand::Types { out, stdout } => types(out.as_deref(), stdout),
    }
}

fn run_worm(
    dir: &Path,
    file: &Path,
    config: Option<&Path>,
    out: Option<&Path>,
) -> Result<ExitCode> {
    let source =
        std::fs::read_to_string(file).with_context(|| format!("cannot read {}", ui::rel(file)))?;

    let config = match config {
        Some(path) => std::fs::read_to_string(path)
            .with_context(|| format!("cannot read {}", ui::rel(path)))?,

        None => String::new(),
    };

    let mut worm = Worm::load(dir)?;
    let outcome = worm.transform(&source, &config)?;

    if !outcome.ok {
        ui::print_error(&format!("worm `{}`: {}", worm.name(), outcome.text));

        return Ok(ExitCode::FAILURE);
    }

    /*
    Line preservation is the property most easily broken by accident and the
    hardest to notice, so say it every run rather than making an author think
    to check.
    */
    let (before, after) = (source.lines().count(), outcome.text.lines().count());

    if before == after {
        eprintln!("{} lines in, {} lines out", before, after);
    } else {
        ui::print_error(&format!(
            "line count changed, {before} in and {after} out, which breaks retain lines downstream"
        ));
    }

    match out {
        Some(path) => {
            std::fs::write(path, &outcome.text)
                .with_context(|| format!("cannot write {}", ui::rel(path)))?;

            ui::print_success(&format!("wrote {}", ui::rel(path)));
        }

        None => print!("{}", outcome.text),
    }

    Ok(ExitCode::SUCCESS)
}

fn info(dir: &Path) -> Result<ExitCode> {
    let worm = Worm::load(dir)?;
    let m = &worm.manifest;
    let color = ui::want_color_stderr();

    eprintln!("{} {}", ui::accent(&m.name, color), m.entry);
    eprintln!("  form       {:?}", m.form);
    eprintln!("  api        {}", m.api);
    eprintln!("  requires   {:?}", m.requires);

    match &m.frontend {
        Some(frontend) => eprintln!("  claims     {}", frontend.claims.join(", ")),

        None => eprintln!("  claims     nothing, this worm has no frontend"),
    }

    if m.rules.is_empty() {
        eprintln!("  rules      none, so it takes no run_order slot");
    } else {
        eprintln!(
            "  run_order  {}",
            match m.run_order {
                Some(order) => order.to_string(),
                None => "unset, runs after larvae".to_owned(),
            }
        );

        for (name, rule) in &m.rules {
            eprintln!("  rule       {name} (default {})", scalar(&rule.default));
        }
    }

    Ok(ExitCode::SUCCESS)
}

/// Render a rule default for display. We build toml without the serializer,
/// so there is no Display to lean on and scalars are all a default can be.
fn scalar(value: &Option<toml::Value>) -> String {
    match value {
        None => "unset".to_owned(),

        Some(toml::Value::Boolean(b)) => b.to_string(),

        Some(toml::Value::Integer(n)) => n.to_string(),

        Some(toml::Value::Float(f)) => f.to_string(),

        Some(toml::Value::String(s)) => format!("{s:?}"),

        Some(other) => other.type_str().to_owned(),
    }
}

fn types(out: Option<&Path>, stdout: bool) -> Result<ExitCode> {
    if stdout {
        print!("{TYPES}");

        return Ok(ExitCode::SUCCESS);
    }

    let path = out
        .map(Path::to_path_buf)
        .unwrap_or_else(|| TYPES_FILE.into());

    std::fs::write(&path, TYPES).with_context(|| format!("cannot write {}", ui::rel(&path)))?;

    ui::print_success(&format!("wrote {}", ui::rel(&path)));

    wire_luau_lsp(&path)?;

    Ok(ExitCode::SUCCESS)
}

/*
Add the definitions file to the project's luau-lsp settings.

Project settings this time, not the user's. A definitions file lives beside the
worm it describes, so the setting belongs to the repo and travels with it,
unlike the TOML schema which is one URL good for every project at once.
*/
fn wire_luau_lsp(types: &Path) -> Result<()> {
    let settings = PathBuf::from(".vscode/settings.json");
    let entry = ui::rel(types);
    let listed = entry.clone();

    let changed = crate::commands::code::edit_settings(&settings, move |table| {
        /*
        A map of package name to path, not a list. The neighbouring
        documentationFiles is an array, which is an easy thing to assume this
        one is too.
        */
        let files = table
            .entry(DEFINITIONS)
            .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));

        let Some(files) = files.as_object_mut() else {
            anyhow::bail!("{DEFINITIONS} is not an object, leaving it alone");
        };

        if files.get(PACKAGE).and_then(|v| v.as_str()) == Some(listed.as_str()) {
            return Ok(false);
        }

        files.insert(PACKAGE.to_owned(), listed.into());

        Ok(true)
    })?;

    if changed {
        ui::print_success(&format!("added it to {}", ui::rel(&settings)));
        eprintln!("Reload the window and your worm typechecks as you write it.");
    } else {
        ui::print_success(&format!("{} already lists it", ui::rel(&settings)));
    }

    eprintln!("Outside an editor:");
    eprintln!("  luau-lsp analyze --definitions={entry} your-worm.luau");

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_definitions_declare_what_a_worm_returns() {
        assert!(TYPES.contains("export type Worm"));
        assert!(TYPES.contains("export type Frontend"));
        assert!(TYPES.contains("export type Compile"));
    }

    /*
    The Kind union is written by hand so it reads well, which means it can drift
    from the enum. Every name has to exist on both sides, in both directions.
    */
    #[test]
    fn the_kind_union_matches_the_ast() {
        use crate::worm::nodes::Kind;

        let union: Vec<&str> = TYPES
            .lines()
            .skip_while(|l| !l.starts_with("export type Kind ="))
            .take_while(|l| !l.starts_with("export type Node"))
            .filter_map(|l| l.split_once('"'))
            .filter_map(|(_, rest)| rest.split_once('"'))
            .map(|(name, _)| name)
            .collect();

        assert!(!union.is_empty(), "found no kinds in the union");

        for name in &union {
            assert!(
                Kind::from_name(name).is_some(),
                "worm.d.luau names {name}, the AST does not"
            );
        }

        for kind in Kind::ALL {
            assert!(
                union.contains(&kind.name()),
                "the AST has {}, worm.d.luau does not",
                kind.name()
            );
        }
    }

    /*
    luau-lsp declares definitionFiles as an object of package name to path,
    while documentationFiles beside it is an array. Getting that backwards
    produces settings the extension silently ignores, so pin the shape.
    */
    #[test]
    fn the_luau_lsp_entry_is_a_map_and_not_a_list() {
        let tmp = tempfile::tempdir().unwrap();
        let cwd = std::env::current_dir().unwrap();

        std::env::set_current_dir(tmp.path()).unwrap();
        let result = wire_luau_lsp(Path::new("worm.d.luau"));
        std::env::set_current_dir(cwd).unwrap();

        result.unwrap();

        let text = std::fs::read_to_string(tmp.path().join(".vscode/settings.json")).unwrap();
        let json: serde_json::Value = serde_json::from_str(&text).unwrap();

        assert!(
            json[DEFINITIONS].is_object(),
            "definitionFiles must be a map, got {text}"
        );
        assert_eq!(json[DEFINITIONS][PACKAGE], "worm.d.luau");
    }

    /// The definitions have to be Luau a language server can actually load
    #[test]
    fn the_definitions_parse_as_luau() {
        let lua = mlua::Lua::new();

        lua.load(TYPES)
            .set_name("worm.d.luau")
            .into_function()
            .expect("worm.d.luau is valid Luau");
    }
}
