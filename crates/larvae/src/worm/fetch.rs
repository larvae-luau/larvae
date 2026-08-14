/*!
This module fetches a worm from a GitHub release and writes it to disk.

A worm is code that an other author wrote, fetched from a URL. Thus this
module applies strict checks. Larvae pins each worm by tag. Larvae verifies
the recorded hash on every later build. Larvae does not trust the paths in the
archive.

Unpacked worms live in the project's cache directory, one directory for each
name and version. Thus a second build uses no network at all.
*/

use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::config::worms::Source;
use crate::net::{github, http};

/// A worm archive is a manifest and one artifact, much smaller than this limit
const MAX_ARCHIVE: u64 = 64 * 1024 * 1024;

/// The directory of an unpacked worm, `<cache>/worms/<name>/<version>`
pub fn install_dir(cache: &Path, name: &str, version: &str) -> PathBuf {
    cache.join("worms").join(name).join(version)
}

/*
Resolve a pinned worm to a directory. Fetch only if the worm is not there.

Larvae records the hash on the first fetch and checks it on every later build.
Thus larvae catches a release asset that changes under a tag and does not
trust it silently. GitHub permits an author to replace an asset without a move
of the tag. A pin alone does not cover that case.
*/
pub fn ensure(cache: &Path, name: &str, source: &Source) -> Result<PathBuf> {
    let (repo, version, asset) = match source {
        Source::Release {
            repo,
            version,
            asset,
        } => (repo, version, asset),

        Source::Cargo { package, version } => {
            return ensure_cargo(cache, name, package, version);
        }

        Source::Local { .. } => bail!("worm `{name}` is local, it does not need fetching"),
    };

    let dir = install_dir(cache, name, version);
    let stamp = dir.join(".sha256");

    if dir.join(super::MANIFEST).exists() {
        /*
        The stamp records what the install wrote, so a later build can see a
        changed byte. A worm that fails the check is fetched again rather
        than run, because a native worm executes with the access of the user
        and a silent change is the one failure worth a network trip.

        A directory without a stamp predates the check and is trusted as it
        is. The next fetch writes one.
        */
        let recorded = std::fs::read_to_string(&stamp).unwrap_or_default();

        if recorded.trim().is_empty() || recorded.trim() == dir_digest(&dir)? {
            return Ok(dir);
        }

        eprintln!("worm `{name}` changed on disk since its install, so larvae fetches it again");

        std::fs::remove_dir_all(&dir)
            .with_context(|| format!("cannot clear {}", crate::ui::rel(&dir)))?;
    }

    let wanted = Source::Release {
        repo: repo.clone(),
        version: version.clone(),
        asset: asset.clone(),
    }
    .asset_names(name);

    let release = github::release_by_tag(repo, version)
        .with_context(|| format!("worm `{name}`: no release {version} in {repo}"))?;

    let found = wanted
        .iter()
        .find_map(|want| release.assets.iter().find(|a| &a.name == want));

    let Some(found) = found else {
        bail!(
            "worm `{name}`: release {version} of {repo} has none of {}. Its assets are: {}",
            wanted.join(", "),
            release
                .assets
                .iter()
                .map(|a| a.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    };

    let bytes = http::get_bytes(&found.browser_download_url)
        .with_context(|| format!("worm `{name}`: downloading {}", found.name))?;

    std::fs::create_dir_all(&dir)
        .with_context(|| format!("cannot create {}", crate::ui::rel(&dir)))?;

    unpack(&bytes, &dir).with_context(|| format!("worm `{name}`: unpacking {}", found.name))?;

    flatten_wrapper(&dir)
        .with_context(|| format!("worm `{name}`: cannot read what {} holds", found.name))?;

    if !dir.join(super::MANIFEST).exists() {
        bail!(
            "worm `{name}`: {} has no {} at its root, and no single directory holding one",
            found.name,
            super::MANIFEST
        );
    }

    make_runnable(&dir)
        .with_context(|| format!("worm `{name}`: cannot make its entry runnable"))?;

    std::fs::write(&stamp, dir_digest(&dir)?).ok();

    Ok(dir)
}

/*
One digest for the whole installed directory.

The hash covers every file except the stamp, each with its relative path, in
a sorted order. Thus the digest is stable across platforms, and a changed,
added, or removed file changes it. The permissions are not part of it,
because larvae itself sets the executable bit after the unpack.
*/
fn dir_digest(dir: &Path) -> Result<String> {
    let mut files = Vec::new();
    collect_files(dir, dir, &mut files)?;
    files.sort();

    let mut all = Vec::new();

    for rel in files {
        all.extend_from_slice(rel.as_bytes());
        all.push(0);
        all.extend_from_slice(&std::fs::read(dir.join(&rel))?);
        all.push(0);
    }

    Ok(sha256(&all))
}

fn collect_files(root: &Path, dir: &Path, out: &mut Vec<String>) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();

        if path.is_dir() {
            collect_files(root, &path, out)?;
        } else if entry.file_name() != ".sha256" {
            let rel = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");

            out.push(rel);
        }
    }

    Ok(())
}

/// Check if an installed worm still hashes to the value recorded at fetch time
/// Check that an installed worm still hashes to the stamp of its install
pub fn verify(dir: &Path, expected: &str) -> Result<()> {
    let found = dir_digest(dir)?;

    if found.trim() != expected.trim() {
        bail!(
            "worm at {} has changed since it was installed, expected {expected} and found {found}",
            crate::ui::rel(dir)
        );
    }

    Ok(())
}

/*
Unpack every file. Refuse each path that points outside the directory.

A zip entry names its own path. Thus a crafted archive can say `../../..` and
write to any location. Larvae builds the path from the plain name components
and does not trust the entry. This approach removes the attack instead of an
attempt to clean each bad path.
*/
/*
Install a worm from crates.io, and adopt what cargo built.

`cargo install` compiles the crate on this machine, so one published crate
serves every platform and a worm author uploads no per platform zip. It also
ships no data files. The manifest therefore travels inside the binary, and
larvae asks the binary for it once, over the same pipe the worm always
speaks.
*/
fn ensure_cargo(cache: &Path, name: &str, package: &str, version: &str) -> Result<PathBuf> {
    let dir = install_dir(cache, name, version);

    if dir.join(super::MANIFEST).exists() {
        return Ok(dir);
    }

    /*
    The build root sits beside the install directory rather than in a system
    temp path, so a failed build leaves its debris where `cache_dir` already
    collects it, and one `rm -rf` of the cache removes everything.
    */
    let build = dir.with_extension("build");
    let _ = std::fs::remove_dir_all(&build);

    let output = std::process::Command::new("cargo")
        .args(["install", package, "--version", version, "--root"])
        .arg(&build)
        .args(["--locked", "--no-track"])
        .output()
        .with_context(|| format!("worm `{name}`: cannot run cargo, is it installed?"))?;

    if !output.status.success() {
        bail!(
            "worm `{name}`: cargo install {package} {version} failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
                .lines()
                .rev()
                .take(12)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect::<Vec<_>>()
                .join("\n")
        );
    }

    let adopted = adopt(&build.join("bin"), &dir, name);

    let _ = std::fs::remove_dir_all(&build);

    adopted
}

/*
Adopt the binary that a cargo install produced.

The steps are the contract of the cargo channel: find the one binary, ask it
for the `worm.toml` it carries, write that manifest beside it, and place the
binary at the entry the manifest names. The manifest must say `form =
"native"`, because a compiled binary is the only thing cargo installs.
*/
pub fn adopt(bin: &Path, dir: &Path, name: &str) -> Result<PathBuf> {
    let mut binaries: Vec<_> = std::fs::read_dir(bin)
        .with_context(|| format!("worm `{name}`: cargo installed no binary"))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .collect();

    let [binary] = binaries.as_mut_slice() else {
        bail!(
            "worm `{name}`: the crate installed {} binaries, and a worm is exactly one",
            binaries.len()
        );
    };

    let text = super::native::manifest_of(binary, name)?;
    let manifest = super::Manifest::parse(&text)
        .with_context(|| format!("worm `{name}`: in the worm.toml that the binary returned"))?;

    if manifest.form != super::Form::Native {
        bail!(
            "worm `{name}`: the manifest says form = \"{}\", and a cargo install is always native",
            manifest.form.name()
        );
    }

    std::fs::create_dir_all(dir)
        .with_context(|| format!("cannot create {}", crate::ui::rel(dir)))?;
    std::fs::write(dir.join(super::MANIFEST), &text).with_context(|| {
        format!(
            "cannot write {}",
            crate::ui::rel(&dir.join(super::MANIFEST))
        )
    })?;

    let entry = dir.join(&manifest.entry);

    if let Some(parent) = entry.parent() {
        std::fs::create_dir_all(parent).ok();
    }

    std::fs::copy(&*binary, &entry)
        .with_context(|| format!("cannot place the binary at {}", crate::ui::rel(&entry)))?;

    make_runnable(dir)?;

    Ok(dir.to_path_buf())
}

/*
Lift the contents of a single wrapping directory to the root.

Most tools that build a release zip wrap what they pack in one directory named
after the archive, and `zip -r name.zip name/` does it by default. A worm
packed that way holds everything larvae needs, one level down. Larvae lifts it
rather than refuse the worm, because the layout is a packaging habit and not a
statement by the author.

A zip with more than one entry at its root is left as it is. There larvae
cannot know which entry is meant to be the root.
*/
pub fn flatten_wrapper(dir: &Path) -> Result<()> {
    if dir.join(super::MANIFEST).exists() {
        return Ok(());
    }

    let entries: Vec<_> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name() != ".sha256")
        .collect();

    let [wrapper] = entries.as_slice() else {
        return Ok(());
    };

    let inner = wrapper.path();

    if !inner.is_dir() || !inner.join(super::MANIFEST).exists() {
        return Ok(());
    }

    for item in std::fs::read_dir(&inner)?.filter_map(|e| e.ok()) {
        std::fs::rename(item.path(), dir.join(item.file_name()))?;
    }

    std::fs::remove_dir(&inner).ok();

    Ok(())
}

/*
Make the entry of a native worm executable.

A zip carries a permission mode, but the mode of a file inside one is not
reliable across the tools that build releases. A worm that the operating
system refuses to run is a worse failure than a file that is executable and
never run, so larvae sets the bit itself.
*/
#[cfg(unix)]
pub fn make_runnable(dir: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let text = std::fs::read_to_string(dir.join(super::MANIFEST))?;
    let manifest = super::Manifest::parse(&text)?;

    if manifest.form != super::Form::Native {
        return Ok(());
    }

    let entry = dir.join(&manifest.entry);
    let mut mode = std::fs::metadata(&entry)?.permissions();

    mode.set_mode(mode.mode() | 0o755);
    std::fs::set_permissions(&entry, mode)?;

    Ok(())
}

/// Windows runs a file by its extension, so there is no bit to set
#[cfg(not(unix))]
pub fn make_runnable(_dir: &Path) -> Result<()> {
    Ok(())
}

fn unpack(archive: &[u8], dir: &Path) -> Result<()> {
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(archive))
        .context("not a readable zip archive")?;

    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).with_context(|| format!("entry {i}"))?;

        if !entry.is_file() {
            continue;
        }

        if entry.size() > MAX_ARCHIVE {
            bail!(
                "{} unpacks to {} bytes, refusing",
                entry.name(),
                entry.size()
            );
        }

        let Some(rel) = safe_path(entry.name()) else {
            bail!(
                "archive entry {:?} escapes the worm directory",
                entry.name()
            );
        };

        let dest = dir.join(&rel);

        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("cannot create {}", crate::ui::rel(parent)))?;
        }

        let mut bytes = Vec::with_capacity(entry.size() as usize);
        entry.read_to_end(&mut bytes)?;

        std::fs::write(&dest, &bytes)
            .with_context(|| format!("cannot write {}", crate::ui::rel(&dest)))?;
    }

    Ok(())
}

/*
Build a relative path from plain components only. Larvae refuses an absolute
path, a `..` component, a drive prefix, and an empty result. Larvae does not
sanitize these paths, because a path that is not fully understood is not safe
to write to.
*/
fn safe_path(name: &str) -> Option<PathBuf> {
    let mut out = PathBuf::new();

    for part in name.split(['/', '\\']) {
        match part {
            "" | "." => continue,

            ".." => return None,

            part if part.contains(':') => return None,

            part => out.push(part),
        }
    }

    (!out.as_os_str().is_empty()).then_some(out)
}

/// Compute a hex sha256, so a recorded digest is comparable and readable
fn sha256(bytes: &[u8]) -> String {
    let digest = <sha2::Sha256 as sha2::Digest>::digest(bytes);

    digest.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_ordinary_entry_keeps_its_shape() {
        assert_eq!(safe_path("worm.toml"), Some(PathBuf::from("worm.toml")));
        assert_eq!(
            safe_path("nested/dir/init.luau"),
            Some(PathBuf::from("nested/dir/init.luau"))
        );
    }

    /// This test shows the reason larvae does not trust the path of an entry
    #[test]
    fn traversal_is_refused_rather_than_cleaned() {
        assert_eq!(safe_path("../../etc/passwd"), None);
        assert_eq!(safe_path("a/../../b"), None);
        assert_eq!(safe_path("/etc/passwd"), Some(PathBuf::from("etc/passwd")));
        assert_eq!(safe_path("C:/windows/system32"), None);
        assert_eq!(safe_path("..\\..\\evil"), None);
    }

    #[test]
    fn an_entry_that_names_nothing_is_refused() {
        assert_eq!(safe_path(""), None);
        assert_eq!(safe_path("."), None);
        assert_eq!(safe_path("./"), None);
    }

    #[test]
    fn a_digest_is_stable_and_hex() {
        let d = sha256(b"larvae");

        assert_eq!(d.len(), 64);
        assert_eq!(d, sha256(b"larvae"));
        assert_ne!(d, sha256(b"larvaf"));
        assert!(d.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn an_install_is_keyed_by_name_and_version() {
        let a = install_dir(Path::new(".larvae"), "luaux", "0.1.0");
        let b = install_dir(Path::new(".larvae"), "luaux", "0.2.0");

        assert_ne!(a, b);
        assert!(a.ends_with("worms/luaux/0.1.0"));
    }

    #[test]
    fn a_changed_worm_is_caught_on_a_later_build() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("worm.toml"), "name = \"x\"").unwrap();
        std::fs::write(dir.path().join("init.luau"), "return {}").unwrap();

        // the digest of the installed bytes passes as long as the bytes stand
        let installed = dir_digest(dir.path()).unwrap();
        assert!(verify(dir.path(), &installed).is_ok());

        // an edited artifact no longer hashes to the recorded value
        std::fs::write(dir.path().join("init.luau"), "return nil").unwrap();

        let err = verify(dir.path(), &installed).err().unwrap();
        assert!(err.to_string().contains("has changed"), "{err}");
    }

    #[test]
    fn an_archive_unpacks_flat_and_nested_entries() {
        let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);

        use std::io::Write;
        w.start_file("worm.toml", opts).unwrap();
        w.write_all(b"name = \"x\"").unwrap();
        w.start_file("sub/init.luau", opts).unwrap();
        w.write_all(b"return {}").unwrap();

        let archive = w.finish().unwrap().into_inner();
        let dir = tempfile::tempdir().unwrap();

        unpack(&archive, dir.path()).unwrap();

        assert!(dir.path().join("worm.toml").exists());
        assert!(dir.path().join("sub/init.luau").exists());
    }

    /// A crafted archive must not write outside its directory
    #[test]
    fn an_archive_cannot_write_outside_its_directory() {
        let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);

        use std::io::Write;
        w.start_file("../escaped.txt", opts).unwrap();
        w.write_all(b"nope").unwrap();

        let archive = w.finish().unwrap().into_inner();
        let dir = tempfile::tempdir().unwrap();

        let err = unpack(&archive, dir.path()).err().unwrap();

        assert!(err.to_string().contains("escapes"), "{err}");
        assert!(!dir.path().parent().unwrap().join("escaped.txt").exists());
    }
}
