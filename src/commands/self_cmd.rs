//! `larvae self <command>`, manage the larvae installation itself

use std::path::Path;
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use clap::Subcommand;
use semver::Version;

use crate::net::{github, http};
use crate::sys::paths;
use crate::ui;

/// GitHub repository releases are published to
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
}

pub fn run(cmd: SelfCommand) -> Result<ExitCode> {
    match cmd {
        SelfCommand::Install => install(),

        SelfCommand::Update { force } => update(force),

        SelfCommand::Uninstall => uninstall(),
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
    We only replace the copy `self install` put in place, a binary a version
    manager pins belongs to that manager, overwriting it would leave the
    manifest saying one version while the bytes on disk say another
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

    // Release assets are named larvae-{os}-{arch}[.exe] from std::env::consts
    let asset_name = format!(
        "larvae-{}-{}{}",
        std::env::consts::OS,
        std::env::consts::ARCH,
        std::env::consts::EXE_SUFFIX
    );

    let Some(asset) = release.assets.iter().find(|a| a.name == asset_name) else {
        bail!("release v{latest} has no asset named {asset_name}");
    };

    eprintln!("Downloading {} v{latest}", asset.name);

    let bytes = http::get_bytes(&asset.browser_download_url)?;
    let staged = std::env::temp_dir().join(&asset_name);

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

fn uninstall() -> Result<ExitCode> {
    let dir = paths::larvae_dir()?;

    if !dir.exists() {
        bail!("larvae is not installed at {}", dir.display());
    }

    if !ui::confirm(&format!("Remove {}?", dir.display()), false) {
        eprintln!("Aborted.");

        return Ok(ExitCode::SUCCESS);
    }

    // a binary inside ~/.larvae deletes itself first so the dir can go
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
Put the bin directory on PATH, Windows gets it written to the registry for
real, unix shells vary too much to edit a profile behind someone's back so
we print the line instead
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
Prepend to the user Path in HKCU, kept as REG_EXPAND_SZ because existing
entries commonly hold %USERPROFILE% style variables that must keep expanding,
then broadcast the change so new terminals pick it up without a logout
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

/// Tell running processes the environment moved, without it only a logout works
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
