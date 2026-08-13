//! `larvae worm <command>` supports worm development before a user can install the worm.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::Subcommand;

use crate::ui;
use crate::worm::Worm;

/// Luau type definitions for the worm API; `larvae worm types` writes them
pub const TYPES: &str = include_str!("../worm/worm.d.luau");

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
    }
}

fn run_worm(
    dir: &Path,
    file: &Path,
    config: Option<&Path>,
    out: Option<&Path>,
    fmt: bool,
    lint: bool,
) -> Result<ExitCode> {
    let source =
        std::fs::read_to_string(file).with_context(|| format!("cannot read {}", ui::rel(file)))?;

    let config = match config {
        Some(path) => std::fs::read_to_string(path)
            .with_context(|| format!("cannot read {}", ui::rel(path)))?,

        None => String::new(),
    };

    let value: toml::Value = toml::from_str(&config).context("--config is not TOML")?;

    let mut worm = Worm::load(dir)?;

    // The same handover as a project run, so a worm that reads its config at
    // init sees the config here too.
    worm.init(&value, &Default::default(), &Default::default())?;

    if fmt {
        return fmt_worm(&mut worm, &source, out);
    }

    if lint {
        return lint_worm(&mut worm, file, &source);
    }

    let outcome = worm.transform(&source, &config)?;

    if !outcome.ok {
        ui::print_error(&format!("worm `{}`: {}", worm.name(), outcome.text));

        return Ok(ExitCode::FAILURE);
    }

    /*
    An accident breaks line preservation most easily, and the break is hard
    to see. So the command reports the line counts on every run, and the
    author does not have to remember a check.
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

/*
`--fmt`, the format half of the dev loop.

The render uses the default style, because no project supplies a style here.
The command then formats the output a second time and compares the results.
Idempotence is a guarantee that the worm gives, because larvae never formats
twice at run time. A break of that guarantee must appear in the dev loop and
not in the diff of a user.
*/
fn fmt_worm(worm: &mut Worm, source: &str, out: Option<&Path>) -> Result<ExitCode> {
    let cfg = crate::fmt::FmtConfig::default();
    let name = worm.name().to_owned();

    let render = |worm: &mut Worm, src: &str| -> Result<String> {
        let reply = worm.format(src)?;

        crate::worm::proto::render_format(src, &reply, &cfg)
            .with_context(|| format!("worm `{name}`"))
    };

    let text = render(worm, source)?;

    match render(worm, &text) {
        Ok(second) if second == text => {
            eprintln!("idempotent, formatting the output changes nothing")
        }

        Ok(_) => ui::print_error(
            "not idempotent, formatting the output changes it again, which turns saves into diffs",
        ),

        Err(e) => ui::print_error(&format!("cannot format its own output, {e:#}")),
    }

    match out {
        Some(path) => {
            std::fs::write(path, &text)
                .with_context(|| format!("cannot write {}", ui::rel(path)))?;

            ui::print_success(&format!("wrote {}", ui::rel(path)));
        }

        None => print!("{text}"),
    }

    Ok(ExitCode::SUCCESS)
}

/*
`--lint`, the lint half.

The levels come from the defaults of the manifest, because there is no
project and no `[lint.rules]` here. The exit code follows the same rule as
`larvae lint`, so the fixture files of a worm author can assert on it.
*/
fn lint_worm(worm: &mut Worm, file: &Path, source: &str) -> Result<ExitCode> {
    let reply = worm.lint(source)?;
    let declared = worm.manifest.lints.clone();
    let name = worm.name().to_owned();

    let diags = crate::lint::from_worm(
        file,
        source,
        reply,
        &crate::lint::LintConfig::default(),
        &declared,
        &name,
    )
    .unwrap_or_else(|refusal| vec![refusal]);

    let color = ui::want_color();

    for diag in &diags {
        println!("{}", diag.render(color));
    }

    let errors = diags
        .iter()
        .filter(|d| d.severity == crate::diag::Severity::Error)
        .count();

    if diags.is_empty() {
        ui::print_success("nothing to report");
    }

    match errors {
        0 => Ok(ExitCode::SUCCESS),

        _ => Ok(ExitCode::FAILURE),
    }
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
        Some(frontend) => {
            eprintln!("  claims     {}", frontend.claims.join(", "));

            eprintln!(
                "  fmt        {}",
                match frontend.fmt {
                    true => "yes, it lays claimed files out",
                    false => "no",
                }
            );
        }

        None => eprintln!("  claims     nothing, this worm has no frontend"),
    }

    for (name, decl) in &m.lints {
        eprintln!("  lint       {name} (default {:?})", decl.default);
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

/// Render a rule default for display. The code builds toml without the
/// serializer, so no Display exists, and a default can only be a scalar.
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
Add the definitions file to the luau-lsp settings of the project.

This function writes project settings and not user settings. A definitions
file sits beside the worm that it describes. So the setting belongs to the
repository and moves with it. The TOML schema differs: one URL covers every
project.
*/
fn wire_luau_lsp(types: &Path) -> Result<()> {
    let settings = PathBuf::from(".vscode/settings.json");
    let entry = ui::rel(types);
    let listed = entry.clone();

    let changed = crate::commands::code::edit_settings(&settings, move |table| {
        /*
        A map of package name to path, not a list. The adjacent
        documentationFiles is an array, so it is easy to assume that this
        setting is also an array.
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
    The author writes the Kind union by hand for readability, so it can drift
    from the enum. Every name must exist on both sides, in both directions.
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
    luau-lsp declares definitionFiles as an object of package name to path.
    The adjacent documentationFiles is an array. A swap of the two shapes
    produces settings that the extension ignores without a message, so the
    test pins the shape.
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

    /// The definitions must be valid Luau that a language server can load
    #[test]
    fn the_definitions_parse_as_luau() {
        let lua = mlua::Lua::new();

        lua.load(TYPES)
            .set_name("worm.d.luau")
            .into_function()
            .expect("worm.d.luau is valid Luau");
    }
}
