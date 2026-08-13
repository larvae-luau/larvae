//! `larvae self <command>` manages the larvae installation.

use std::path::Path;
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use clap::Subcommand;
use semver::Version;

use crate::net::{github, http};
use crate::sys::paths;
use crate::ui;

/// The GitHub repository that holds the releases
const REPO: &str = "larvae-luau/larvae";

#[derive(Subcommand)]
pub enum SelfCommand {
    /// Install larvae to ~/.larvae/bin
    Install,

    /// Update larvae to the latest release
    Update {
        /// Replace the binary even when another tool manages it
        #[arg(long)]
        force: bool,
    },

    /// Remove larvae from this machine
    Uninstall,

    /// Set up editor completion for larvae.toml
    Code,
}

pub fn run(root: &Path, cmd: SelfCommand) -> Result<ExitCode> {
    match cmd {
        SelfCommand::Install => install(),

        SelfCommand::Update { force } => update(force),

        SelfCommand::Uninstall => uninstall(),

        SelfCommand::Code => crate::commands::code::run(root),
    }
}

fn install() -> Result<ExitCode> {
    let me = std::env::current_exe().context("cannot locate the running executable")?;
    let bin_dir = paths::bin_dir()?;
    let target = paths::installed_exe()?;

    if let Some(tool) = paths::managing_tool(&me) {
        eprintln!(
            "note: {tool} already manages this binary, a copy in {} may shadow it on PATH",
            crate::ui::rel(&bin_dir)
        );
    }

    if paths::same_file(&me, &target) {
        ui::print_success(&format!(
            "larvae is already installed at {}",
            target.display()
        ));
    } else {
        std::fs::create_dir_all(&bin_dir)
            .with_context(|| format!("failed to create {}", bin_dir.display()))?;
        std::fs::copy(&me, &target)
            .with_context(|| format!("failed to copy to {}", target.display()))?;
        ui::print_success(&format!("Installed larvae to {}", target.display()));
    }

    add_to_path(&bin_dir);

    Ok(ExitCode::SUCCESS)
}

fn update(force: bool) -> Result<ExitCode> {
    let me = std::env::current_exe().context("cannot locate the running executable")?;

    /*
    The command replaces only the copy that `self install` wrote. A binary
    that a version manager pins belongs to that manager. If larvae overwrote
    it, the manifest would show one version and the bytes on disk another.
    */
    if !force && !paths::is_self_installed(&me) {
        let hint = match paths::managing_tool(&me) {
            Some("cargo") => "reinstall with `cargo install larvae`".to_string(),

            Some(tool) => format!("{tool} manages this binary, bump the version in its manifest"),

            None => "run `larvae self install` first, or reinstall from the release".to_string(),
        };

        bail!(
            "larvae did not install {}, so it will not replace it\n  {hint}\n  pass --force to overwrite it anyway",
            crate::ui::rel(&me)
        );
    }

    let current = Version::parse(env!("CARGO_PKG_VERSION"))?;
    eprintln!("Checking for updates (currently v{current})");

    let release = github::latest_release(REPO)
        .with_context(|| format!("failed to query releases for {REPO}"))?;

    let latest = Version::parse(release.tag_name.trim_start_matches('v'))
        .with_context(|| format!("release tag {:?} is not a version", release.tag_name))?;

    if latest <= current {
        ui::print_success("larvae is already up to date");

        return Ok(ExitCode::SUCCESS);
    }

    /*
    Release assets have the name larvae-{os}-{arch} from std::env::consts.
    The zip archive takes the name of the binary. The command also accepts a
    bare uncompressed asset with the same stem, so an older release stays
    installable.
    */
    let stem = format!("larvae-{}-{}", std::env::consts::OS, std::env::consts::ARCH);
    let zip_name = format!("{stem}.zip");
    let bare_name = format!("{stem}{}", std::env::consts::EXE_SUFFIX);

    let Some(asset) = release
        .assets
        .iter()
        .find(|a| a.name == zip_name)
        .or_else(|| release.assets.iter().find(|a| a.name == bare_name))
    else {
        bail!("release v{latest} has no asset named {zip_name}");
    };

    eprintln!("Downloading {} v{latest}", asset.name);

    let downloaded = http::get_bytes(&asset.browser_download_url)?;

    let bytes = if asset.name == zip_name {
        unzip_binary(&downloaded).with_context(|| format!("failed to unpack {}", asset.name))?
    } else {
        downloaded
    };

    let staged = std::env::temp_dir().join(&bare_name);

    std::fs::write(&staged, &bytes)
        .with_context(|| format!("failed to stage {}", staged.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755))?;
    }

    self_replace::self_replace(&staged).context("failed to replace the running executable")?;
    let _ = std::fs::remove_file(&staged);

    ui::print_success(&format!("Updated larvae v{current} -> v{latest}"));

    Ok(ExitCode::SUCCESS)
}

/*
Pull the binary out of a release archive. The workflow packs exactly one
file, with the name of the executable and not the asset. So the function
looks for that name, and falls back to the only file present. The function
does not use entry paths on disk and reads only the base name. So a crafted
`../` path in the archive has no effect.
*/
fn unzip_binary(archive: &[u8]) -> Result<Vec<u8>> {
    use std::io::Read;

    /// A release binary is a few megabytes; a file near this limit is not one
    const MAX_UNPACKED: u64 = 256 * 1024 * 1024;

    let want = format!("larvae{}", std::env::consts::EXE_SUFFIX);
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(archive))
        .context("not a readable zip archive")?;

    let mut files = Vec::new();
    let mut exact = None;

    for i in 0..zip.len() {
        let entry = zip
            .by_index(i)
            .with_context(|| format!("entry {i} is unreadable"))?;

        if !entry.is_file() {
            continue;
        }

        let base = entry.name().rsplit(['/', '\\']).next().unwrap_or_default();

        if base == want {
            exact = Some(i);
        }

        files.push(i);
    }

    let index = match (exact, files.as_slice()) {
        (Some(i), _) => i,

        (None, [only]) => *only,

        (None, []) => bail!("the archive holds no files"),

        (None, _) => bail!("the archive has no entry named {want}"),
    };

    let mut entry = zip.by_index(index)?;

    if entry.size() > MAX_UNPACKED {
        bail!(
            "{} unpacks to {} bytes, refusing",
            entry.name(),
            entry.size()
        );
    }

    let mut out = Vec::with_capacity(entry.size() as usize);

    entry
        .read_to_end(&mut out)
        .context("failed to decompress the archive")?;

    Ok(out)
}

fn uninstall() -> Result<ExitCode> {
    let dir = paths::larvae_dir()?;

    if !dir.exists() {
        bail!("larvae is not installed at {}", dir.display());
    }

    if !ui::confirm(&format!("Remove {}?", dir.display()), false) {
        eprintln!("Aborted.");

        return Ok(ExitCode::SUCCESS);
    }

    // A binary inside ~/.larvae deletes itself first, so larvae can remove the directory.
    let me = std::env::current_exe()?;

    if me
        .canonicalize()
        .map(|p| p.starts_with(&dir))
        .unwrap_or(false)
    {
        self_replace::self_delete_outside_path(&dir)
            .context("failed to remove the running executable")?;
    }

    std::fs::remove_dir_all(&dir).with_context(|| format!("failed to remove {}", dir.display()))?;
    ui::print_success(&format!("Removed {}", dir.display()));

    let bin = paths::bin_dir()?;

    eprintln!("You can now drop {} from your PATH.", bin.display());

    Ok(ExitCode::SUCCESS)
}

/*
Put the bin directory on PATH. On Windows, the command writes the registry.
Unix shells differ too much for a safe profile edit without user consent, so
the command prints the line instead.
*/
fn add_to_path(bin_dir: &Path) {
    let on_path = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|entry| entry == bin_dir))
        .unwrap_or(false);

    if on_path {
        return;
    }

    #[cfg(windows)]
    match windows_path_add(bin_dir) {
        Ok(true) => {
            ui::print_success(&format!("Added {} to your PATH", bin_dir.display()));
            eprintln!("Open a new terminal for it to take effect.");
        }

        Ok(false) => {}

        Err(e) => {
            ui::print_error(&format!("could not edit your PATH, {e}"));
            eprintln!(
                "Add {} through Settings > Environment Variables.",
                bin_dir.display()
            );
        }
    }

    #[cfg(not(windows))]
    {
        eprintln!("Add this to your shell profile, then open a new terminal:");
        eprintln!("  export PATH=\"{}:$PATH\"", bin_dir.display());
    }
}

/*
Prepend to the user Path in HKCU. The value stays REG_EXPAND_SZ because
existing entries often hold %USERPROFILE% style variables that must continue
to expand. Then broadcast the change, so new terminals get it without a
logout.
*/
#[cfg(windows)]
fn windows_path_add(bin_dir: &Path) -> anyhow::Result<bool> {
    use winreg::RegKey;
    use winreg::enums::{HKEY_CURRENT_USER, KEY_READ, KEY_WRITE, RegType};

    let env = RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey_with_flags("Environment", KEY_READ | KEY_WRITE)?;

    let current: String = env.get_value("Path").unwrap_or_default();
    let dir = bin_dir.to_string_lossy().to_string();

    if current
        .split(';')
        .any(|entry| entry.trim().eq_ignore_ascii_case(dir.trim()))
    {
        return Ok(false);
    }

    let updated = if current.trim().is_empty() {
        dir
    } else {
        format!("{dir};{current}")
    };

    let mut bytes: Vec<u8> = updated
        .encode_utf16()
        .flat_map(|unit| unit.to_le_bytes())
        .collect();
    bytes.extend_from_slice(&[0, 0]);

    env.set_raw_value(
        "Path",
        &winreg::RegValue {
            vtype: RegType::REG_EXPAND_SZ,
            bytes,
        },
    )?;

    broadcast_environment_change();

    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Write};

    /// A deflated archive that holds each (name, contents) pair
    fn zip_of(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);

        for (name, body) in entries {
            writer.start_file(*name, options).unwrap();
            writer.write_all(body).unwrap();
        }

        writer.finish().unwrap().into_inner()
    }

    fn exe_name() -> String {
        format!("larvae{}", std::env::consts::EXE_SUFFIX)
    }

    #[test]
    fn the_named_entry_wins_over_its_neighbours() {
        let archive = zip_of(&[
            ("README.md", b"docs"),
            (&exe_name(), b"binary"),
            ("LICENSE.md", b"mit"),
        ]);

        assert_eq!(unzip_binary(&archive).unwrap(), b"binary");
    }

    #[test]
    fn a_nested_path_is_matched_on_its_base_name() {
        let archive = zip_of(&[(&format!("larvae-1.0/{}", exe_name()), b"binary")]);

        assert_eq!(unzip_binary(&archive).unwrap(), b"binary");
    }

    #[test]
    fn a_lone_file_is_taken_whatever_it_is_called() {
        let archive = zip_of(&[("some-other-name", b"binary")]);

        assert_eq!(unzip_binary(&archive).unwrap(), b"binary");
    }

    #[test]
    fn an_ambiguous_archive_is_an_error() {
        let archive = zip_of(&[("one", b"a"), ("two", b"b")]);

        assert!(unzip_binary(&archive).is_err());
    }

    #[test]
    fn junk_is_not_an_archive() {
        assert!(unzip_binary(b"not a zip at all").is_err());
    }
}

/// Tell active processes that the environment changed; without this, only a logout works
#[cfg(windows)]
fn broadcast_environment_change() {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        HWND_BROADCAST, SMTO_ABORTIFHUNG, SendMessageTimeoutW, WM_SETTINGCHANGE,
    };

    let target: Vec<u16> = "Environment\0".encode_utf16().collect();

    unsafe {
        SendMessageTimeoutW(
            HWND_BROADCAST,
            WM_SETTINGCHANGE,
            0,
            target.as_ptr() as isize,
            SMTO_ABORTIFHUNG,
            5000,
            std::ptr::null_mut(),
        );
    }
}
