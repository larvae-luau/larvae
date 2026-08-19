/*!
Adding a worm, installing what a project names, and taking one out again.

These three touch `larvae.toml` and the cache, and nothing else in larvae
does. The edits are textual and keep every other byte, comments included, the
same approach `larvae init` takes with `.luaurc`: a parse and re-serialize
would lose the layout of a whole file over one line.
*/

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result};

use crate::ui;

/// The config the command edits: the one named, or ./larvae.toml.
fn config_path(config: Option<PathBuf>) -> Result<PathBuf> {
    let path = match config {
        Some(path) => path,

        None => std::env::current_dir()?.join("larvae.toml"),
    };

    if !path.exists() {
        anyhow::bail!(
            "no larvae.toml at {}; run `larvae init` first",
            crate::ui::rel(&path)
        );
    }

    Ok(path)
}

/*
What the user asked for on the command line.

`luaux`, `owner/repo`, and either of those with `@version`. A short name
expands through the list larvae knows, which is the whole point of having one.
*/
#[derive(Debug)]
struct Asked {
    /// `owner/repo` for a release, or the crate name for cargo.
    base: String,
    /// The key the worm goes under in `[worms]`.
    name: String,
    /// The version the user pinned, when they pinned one.
    version: Option<String>,
}

fn parse_spec(spec: &str, cargo: bool, name: Option<String>) -> Result<Asked> {
    let (base, version) = match spec.rsplit_once('@') {
        Some((base, version)) => (base, Some(version.to_string())),

        None => (spec, None),
    };

    if cargo {
        let key = name.unwrap_or_else(|| base.trim_end_matches("-worm").to_string());

        return Ok(Asked {
            base: base.to_string(),
            name: key,
            version,
        });
    }

    /*
    A name with no slash is a short name, and it has to be one larvae knows.
    Guessing an owner would send the fetch at a repository that may exist and
    may belong to someone else.
    */
    let repo = match base.contains('/') {
        true => base.to_string(),

        false => match crate::worm::known::repo_of(base) {
            Some(repo) => repo.to_string(),

            None => anyhow::bail!(
                "larvae does not know a worm called `{base}`. Write it as owner/repo, \
                 or use one of these: {}",
                crate::worm::known::names().join(", ")
            ),
        },
    };

    /*
    The key is the repo name with `-worm` taken off, because that is how a
    worm repository is named and the key is what a user writes in every other
    table. `larvae-luau/luaux-worm` becomes `luaux`.
    */
    let key = name.unwrap_or_else(|| {
        repo.rsplit('/')
            .next()
            .unwrap_or(&repo)
            .trim_end_matches("-worm")
            .to_string()
    });

    Ok(Asked {
        base: repo,
        name: key,
        version,
    })
}

/*
`larvae worm add`: name a worm, and larvae writes the newest release into the
config.

The version it writes is `^`, which every install reads as the newest release
there is. That is what a user adding a worm almost always wants, and it is the
only form that keeps working as the worm publishes.

A user who wants one release writes `@version`, and then the entry never
moves. Those are the two states: `^` follows, and a number holds. A range,
`^0.1.0`, sits between them and follows inside what semver calls compatible.

The command still asks the repository what the newest release is, even though
it writes `^`. That call is what catches a typo in the name: without it a
misspelled repo lands in the config and fails later, in the middle of an
install of several worms.

It writes the config and stops. Installing is `larvae worm install`, because
an edit to a file is instant and offline while a download is neither, and a
user who adds three worms wants one download and not three.
*/
pub(super) fn add(
    spec: &str,
    cargo: bool,
    name: Option<String>,
    config: Option<PathBuf>,
) -> Result<ExitCode> {
    let path = config_path(config)?;
    let asked = parse_spec(spec, cargo, name)?;

    let text = std::fs::read_to_string(&path)?;

    if already_named(&text, &asked.name) {
        anyhow::bail!(
            "`{}` is already in {}; remove it first, or edit the version",
            asked.name,
            crate::ui::rel(&path)
        );
    }

    // What goes in the file, and what the newest is right now, are two things.
    let (version, newest) = match &asked.version {
        Some(version) => (version.clone(), None),

        None => {
            eprintln!("  checking {}...", asked.base);

            let latest = match cargo {
                true => crate::net::crates_io::latest_version(&asked.base)?,

                /*
                The `v` comes off. A tag carries it and a version key does
                not need it, and the resolver reads either, so the config
                keeps the plain semver that a reader compares by eye.
                */
                false => crate::net::github::latest_release(&asked.base)
                    .map(|r| crate::worm::version::clean(&r.tag_name).to_string())
                    .with_context(|| format!("no releases in {}", asked.base))?,
            };

            ("^".to_string(), Some(latest))
        }
    };

    /*
    A table, because that is the only shape `[worms]` reads. The version is a
    key of its own, so a reader sees what is pinned and `install` can tell an
    exact release from a range without splitting a string.
    */
    let line = match cargo {
        true => format!(
            "{} = {{ cargo = \"{}\", version = \"{version}\" }}\n",
            asked.name, asked.base
        ),

        false => format!(
            "{} = {{ repo = \"{}\", version = \"{version}\" }}\n",
            asked.name, asked.base
        ),
    };

    std::fs::write(&path, with_worm(&text, &line))?;

    let says = match &newest {
        Some(latest) => format!("^, the newest is {latest} today"),

        None => version.clone(),
    };

    ui::print_success(&format!(
        "added {} = {} at {says} to {}",
        asked.name,
        asked.base,
        crate::ui::rel(&path)
    ));

    // Adding and installing are separate on purpose: an edit to the config is
    // instant and offline, and a download is neither.
    eprintln!("  run `larvae worm install` to fetch it");

    Ok(ExitCode::SUCCESS)
}

/// Reports if `[worms]` already holds this key, in any of the forms it takes.
fn already_named(text: &str, name: &str) -> bool {
    [
        format!("[worms.{name}]"),
        format!("[worms.\"{name}\"]"),
        format!("\n{name} = "),
    ]
    .iter()
    .any(|form| text.contains(form.as_str()))
}

/*
The config with one line added to `[worms]`.

The edit is textual and keeps every other byte, comments included, the same
approach `larvae init` takes with `.luaurc`. A parse and re-serialize would
lose the layout of the whole file over one added line.

A file with no `[worms]` table gets one at the end, because a table header has
to come after the root keys and the end is the only place that is always safe.
*/
fn with_worm(text: &str, line: &str) -> String {
    let Some(at) = text.find("[worms]") else {
        let mut out = text.to_string();

        if !out.ends_with('\n') {
            out.push('\n');
        }

        if !out.ends_with("\n\n") {
            out.push('\n');
        }

        out.push_str("[worms]\n");
        out.push_str(line);

        return out;
    };

    // Just past the header line, so the new entry is the first in the table.
    let after = text[at..]
        .find('\n')
        .map(|n| at + n + 1)
        .unwrap_or(text.len());

    let mut out = String::with_capacity(text.len() + line.len());
    out.push_str(&text[..after]);
    out.push_str(line);
    out.push_str(&text[after..]);

    out
}

/*
`larvae worm install`: put every worm the config names on disk.

Installing used to happen inside whatever command needed a worm, so the first
`larvae fmt` of a checkout downloaded from GitHub while the user waited with
nothing on screen. It is a step of its own now, with a bar, and the commands
that use worms read what is already there.

The version decides how much work this is. An exact pin is a directory test
and no request. A range or `^` asks the repository what exists, because the
answer changes when the worm publishes.
*/
pub(super) fn install(config: Option<PathBuf>, force: bool) -> Result<ExitCode> {
    use crate::config::worms::Source;

    let path = config_path(config)?;
    let parsed = crate::config::Config::load(&path)?;

    let Some(value) = parsed.worms.as_ref() else {
        ui::print_success("no [worms] table, nothing to install");

        return Ok(ExitCode::SUCCESS);
    };

    let worms = crate::config::worms::Worms::parse(value)?;

    if worms.0.is_empty() {
        ui::print_success("no worms to install");

        return Ok(ExitCode::SUCCESS);
    }

    let root = path.parent().unwrap_or(Path::new(".")).to_path_buf();
    let cache = root.join(&parsed.process.cache_dir);

    let mut bar = crate::ui::Progress::new(worms.0.len());
    let mut failed = 0usize;

    for (name, entry) in &worms.0 {
        bar.start(name);

        if let Source::Local { path } = &entry.source {
            bar.finish_step(name, &format!("path {}", crate::ui::rel(path)));

            continue;
        }

        match install_one(&cache, name, &entry.source, force) {
            Ok(note) => bar.finish_step(name, &note),

            Err(e) => {
                bar.finish_step(name, "failed");
                ui::print_error(&format!("{name}: {e:#}"));
                failed += 1;
            }
        }
    }

    bar.done();

    match failed {
        0 => {
            ui::print_success(&format!("{} worm(s) ready", worms.0.len()));

            Ok(ExitCode::SUCCESS)
        }

        _ => Ok(ExitCode::FAILURE),
    }
}

/// Install one worm, and report what happened in a few words.
fn install_one(
    cache: &Path,
    name: &str,
    source: &crate::config::worms::Source,
    force: bool,
) -> Result<String> {
    use crate::config::worms::Source;
    use crate::worm::version::Wanted;

    let resolved = match source {
        Source::Local { .. } => unreachable!("handled by the caller"),

        Source::Cargo { package, version } => {
            let wanted = Wanted::parse(version)?;

            /*
            crates.io answers with the newest and not with a list, so a range
            here is met by the newest that satisfies it, or by nothing.
            */
            let picked = match &wanted {
                Wanted::Exact(v) => v.clone(),

                _ => {
                    let latest = crate::net::crates_io::latest_version(package)?;

                    match wanted.pick(&[latest.as_str()]) {
                        Some(tag) => tag.to_string(),

                        None => anyhow::bail!(
                            "the newest {package} is {latest}, which {version} does not accept"
                        ),
                    }
                }
            };

            Source::Cargo {
                package: package.clone(),
                version: picked,
            }
        }

        Source::Release {
            repo,
            version,
            asset,
        } => {
            let wanted = Wanted::parse(version)?;

            let picked = match &wanted {
                Wanted::Exact(v) => v.clone(),

                _ => {
                    let releases = crate::net::github::releases(repo)?;
                    let tags: Vec<&str> = releases.iter().map(|r| r.tag_name.as_str()).collect();

                    match wanted.pick(&tags) {
                        Some(tag) => tag.to_string(),

                        None => anyhow::bail!(
                            "no release of {repo} satisfies {version}; it has {}",
                            match tags.is_empty() {
                                true => "none".to_string(),

                                false => tags.join(", "),
                            }
                        ),
                    }
                }
            };

            Source::Release {
                repo: repo.clone(),
                version: picked,
                asset: asset.clone(),
            }
        }
    };

    let version = match &resolved {
        Source::Release { version, .. } | Source::Cargo { version, .. } => version.clone(),

        Source::Local { .. } => unreachable!("handled by the caller"),
    };

    let dir = crate::worm::fetch::install_dir(cache, name, &version);

    if dir.exists() && !force {
        return Ok(format!("{version} (already installed)"));
    }

    if force && dir.exists() {
        let _ = std::fs::remove_dir_all(&dir);
    }

    crate::worm::fetch::ensure(cache, name, &resolved)?;

    Ok(version)
}

/*
`larvae worm remove`: take a worm out of the config and off the disk.

Both halves, because leaving the files behind means a directory that nothing
names and that the next `install` will not clean. The config edit is textual,
as `add` is.
*/
pub(super) fn remove(name: &str, config: Option<PathBuf>, keep_files: bool) -> Result<ExitCode> {
    let path = config_path(config)?;
    let text = std::fs::read_to_string(&path)?;

    let Some(next) = without_worm(&text, name) else {
        anyhow::bail!(
            "`{name}` is not written in {}; an extends base may declare it, so edit that file",
            crate::ui::rel(&path)
        );
    };

    std::fs::write(&path, next)?;
    ui::print_success(&format!("removed `{name}` from {}", crate::ui::rel(&path)));

    if keep_files {
        return Ok(ExitCode::SUCCESS);
    }

    let parsed = crate::config::Config::load(&path)?;
    let root = path.parent().unwrap_or(Path::new(".")).to_path_buf();
    let dir = root
        .join(&parsed.process.cache_dir)
        .join("worms")
        .join(name);

    if dir.exists() {
        remove_tree(&dir).with_context(|| format!("cannot delete {}", crate::ui::rel(&dir)))?;

        ui::print_success(&format!("deleted {}", crate::ui::rel(&dir)));
    }

    Ok(ExitCode::SUCCESS)
}

/*
Delete a directory and everything under it.

`remove_dir_all` alone is not enough here. A worm directory lives under the
project, and a project can sit on a filesystem that reports a directory as not
empty right after its last file was unlinked: NTFS through FUSE does this, and
so do some network mounts. The call then fails with ENOTEMPTY on a directory
that is empty by the time the error is read.

So the tree comes apart from the bottom, and the last step retries. A worm
that fails to uninstall leaves a directory nothing names and that the next
install will not clean, which is worse than a short wait.
*/
fn remove_tree(dir: &Path) -> std::io::Result<()> {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();

            match path.is_dir() {
                true => remove_tree(&path)?,

                false => {
                    let _ = std::fs::remove_file(&path);
                }
            }
        }
    }

    let mut last = std::fs::remove_dir(dir);

    for _ in 0..5 {
        if last.is_ok() || !dir.exists() {
            return Ok(());
        }

        std::thread::sleep(std::time::Duration::from_millis(20));
        last = std::fs::remove_dir(dir);
    }

    match dir.exists() {
        true => last,

        false => Ok(()),
    }
}

/*
The config with one worm taken out, or None when the file does not name it.

An entry is one line in `[worms]`, or a `[worms.<name>]` table with the keys
under it. The table form runs to the next header, and the line form to the end
of its line.
*/
fn without_worm(text: &str, name: &str) -> Option<String> {
    for header in [format!("[worms.{name}]"), format!("[worms.\"{name}\"]")] {
        if let Some(at) = text.find(&header) {
            let start = text[..at].rfind('\n').map_or(0, |n| n + 1);

            let end = text[at..]
                .find("\n[")
                .map(|n| at + n + 1)
                .unwrap_or(text.len());

            return Some(format!("{}{}", &text[..start], &text[end..]));
        }
    }

    // The line form, which has to be inside [worms] and not a root key.
    let table = text.find("[worms]")?;
    let rest = &text[table..];

    let end_of_table = rest.find("\n[").map(|n| n + 1).unwrap_or(rest.len());
    let region = &rest[..end_of_table];

    let key = format!("{name} = ");
    let at = region
        .match_indices(&key)
        .find(|(i, _)| *i == 0 || region.as_bytes()[i - 1] == b'\n')
        .map(|(i, _)| table + i)?;

    let end = text[at..]
        .find('\n')
        .map(|n| at + n + 1)
        .unwrap_or(text.len());

    Some(drop_empty_worms(&format!(
        "{}{}",
        &text[..at],
        &text[end..]
    )))
}

/*
The config without a `[worms]` table that holds nothing.

An empty table is noise, and it reads as though a worm is configured when none
is. The header goes with the blank line above it, so removing the last worm
leaves the file as it was before the first one was added.
*/
fn drop_empty_worms(text: &str) -> String {
    let Some(at) = text.find("[worms]") else {
        return text.to_string();
    };

    let after = text[at..]
        .find('\n')
        .map(|n| at + n + 1)
        .unwrap_or(text.len());

    // Anything up to the next header decides whether the table is empty.
    let rest = &text[after..];
    let end = rest
        .find("\n[")
        .map(|n| after + n + 1)
        .unwrap_or(text.len());

    if !text[after..end].trim().is_empty() {
        return text.to_string();
    }

    let start = text[..at].trim_end_matches('\n').len();
    let keep_before = match start {
        0 => String::new(),

        _ => format!("{}\n", &text[..start]),
    };

    format!("{keep_before}{}", &text[end..])
}

#[cfg(test)]
mod worm_management {
    use super::*;

    // --- parse_spec ---------------------------------------------------------

    #[test]
    fn a_short_name_expands_to_the_repo_and_the_key() {
        let asked = parse_spec("luaux", false, None).unwrap();

        assert_eq!(asked.base, "larvae-luau/luaux-worm");
        assert_eq!(asked.name, "luaux");
        assert_eq!(asked.version, None);
    }

    /// The key drops `-worm`, because that suffix names the repo and not the worm.
    #[test]
    fn a_repo_gives_up_its_last_segment_as_the_key() {
        let asked = parse_spec("someone/markup-worm", false, None).unwrap();

        assert_eq!(asked.base, "someone/markup-worm");
        assert_eq!(asked.name, "markup");
    }

    #[test]
    fn a_version_can_come_with_the_spec() {
        let asked = parse_spec("luaux@0.2.0", false, None).unwrap();

        assert_eq!(asked.base, "larvae-luau/luaux-worm");
        assert_eq!(asked.version.as_deref(), Some("0.2.0"));
    }

    #[test]
    fn a_name_of_ones_own_wins_over_the_repo() {
        let asked = parse_spec("someone/markup-worm", false, Some("ui".into())).unwrap();

        assert_eq!(asked.name, "ui");
    }

    /*
    A name larvae does not know is refused rather than guessed.

    Guessing an owner would point the fetch at a repository that may exist and
    may belong to someone else.
    */
    #[test]
    fn an_unknown_short_name_is_refused_with_the_list() {
        let err = parse_spec("nothing-like-this", false, None).unwrap_err();
        let text = format!("{err:#}");

        assert!(text.contains("owner/repo"), "{text}");
        assert!(
            text.contains("luaux"),
            "the message lists what larvae knows: {text}"
        );
    }

    #[test]
    fn cargo_takes_the_package_as_written() {
        let asked = parse_spec("luaux-worm@0.1.0", true, None).unwrap();

        assert_eq!(asked.base, "luaux-worm");
        assert_eq!(asked.name, "luaux");
        assert_eq!(asked.version.as_deref(), Some("0.1.0"));
    }

    // --- editing the config -------------------------------------------------

    #[test]
    fn a_worm_goes_in_at_the_top_of_the_table() {
        let text = "input = \"src\"\n\n[worms]\nold = { repo = \"o/old\", version = \"1.0.0\" }\n";
        let out = with_worm(text, "new = { repo = \"o/new\", version = \"2.0.0\" }\n");

        assert_eq!(
            out,
            "input = \"src\"\n\n[worms]\nnew = { repo = \"o/new\", version = \"2.0.0\" }\nold = { repo = \"o/old\", version = \"1.0.0\" }\n"
        );
    }

    /// A table header cannot come before the root keys, so the end is the safe place.
    #[test]
    fn a_config_with_no_worms_table_gets_one() {
        let out = with_worm(
            "input = \"src\"\n",
            "a = { repo = \"o/a\", version = \"1.0.0\" }\n",
        );

        assert_eq!(
            out,
            "input = \"src\"\n\n[worms]\na = { repo = \"o/a\", version = \"1.0.0\" }\n"
        );
    }

    #[test]
    fn the_line_form_comes_out_whole() {
        let text = "[worms]\na = { repo = \"o/a\", version = \"1\" }\nb = { repo = \"o/b\", version = \"1\" }\n";

        assert_eq!(
            without_worm(text, "a").unwrap(),
            "[worms]\nb = { repo = \"o/b\", version = \"1\" }\n"
        );
    }

    #[test]
    fn the_table_form_comes_out_whole() {
        let text = "input = \"src\"\n\n[worms.a]\nrepo = \"o/a\"\nversion = \"1\"\n\n[fmt]\ncolumn_width = 100\n";
        let out = without_worm(text, "a").unwrap();

        assert!(!out.contains("o/a"), "{out}");
        assert!(out.contains("[fmt]"), "the table after it survives: {out}");
    }

    /// An empty table left behind is noise, so it goes with the last worm.
    #[test]
    fn the_worms_table_goes_when_its_last_worm_does() {
        let text = "input = \"src\"\n\n[worms]\na = { repo = \"o/a\", version = \"1\" }\n";

        assert_eq!(without_worm(text, "a").unwrap(), "input = \"src\"\n");
    }

    #[test]
    fn the_worms_table_stays_while_a_worm_is_in_it() {
        let text = "[worms]\na = { repo = \"o/a\", version = \"1\" }\nb = { repo = \"o/b\", version = \"1\" }\n";
        let out = without_worm(text, "a").unwrap();

        assert!(out.contains("[worms]"), "{out}");
    }

    #[test]
    fn a_name_the_config_does_not_hold_is_reported() {
        assert!(without_worm("input = \"src\"\n", "a").is_none());
    }

    #[test]
    fn a_key_is_matched_whole_and_not_by_its_start() {
        let text = "[worms]\nluaux = { repo = \"o/a\", version = \"1\" }\n";

        assert!(without_worm(text, "lua").is_none(), "`lua` is not `luaux`");
    }

    // --- what is already there ----------------------------------------------

    #[test]
    fn an_existing_worm_is_recognised_in_either_form() {
        assert!(already_named("[worms]\na = { repo = \"o/a\" }\n", "a"));
        assert!(already_named("[worms.a]\nrepo = \"o/a\"\n", "a"));
        assert!(!already_named("[worms]\nb = { repo = \"o/b\" }\n", "a"));
    }
}
