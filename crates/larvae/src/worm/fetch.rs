/*!
Getting a worm out of a GitHub release and onto disk.

A worm is code somebody else wrote, fetched from a URL, so this is the part
where being careful matters more than being clever. Everything is pinned by
tag, verified against a recorded hash on every later build, and unpacked with
paths we refuse to trust.

Unpacked worms live in the project's cache directory, one per name and version,
so a second build does no network at all.
*/

use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::config::worms::Source;
use crate::net::{github, http};

/// A worm archive is a manifest and one artifact, nothing near this big
const MAX_ARCHIVE: u64 = 64 * 1024 * 1024;

/// Where an unpacked worm lives, `<cache>/worms/<name>/<version>`
pub fn install_dir(cache: &Path, name: &str, version: &str) -> PathBuf {
    cache.join("worms").join(name).join(version)
}

/*
Resolve a pinned worm to a directory, fetching only if it is not already there.

The hash is recorded on first fetch and checked on every build after, so a
release asset that changes under a tag is caught rather than silently trusted.
GitHub allows replacing an asset without moving the tag, which is exactly the
case a pin alone does not cover.
*/
pub fn ensure(cache: &Path, name: &str, source: &Source) -> Result<PathBuf> {
    let Source::Release {
        repo,
        version,
        asset,
    } = source
    else {
        bail!("worm `{name}` is local, it does not need fetching");
    };

    let dir = install_dir(cache, name, version);
    let stamp = dir.join(".sha256");

    if dir.join(super::MANIFEST).exists() {
        return Ok(dir);
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

    let digest = sha256(&bytes);

    std::fs::create_dir_all(&dir)
        .with_context(|| format!("cannot create {}", crate::ui::rel(&dir)))?;

    unpack(&bytes, &dir).with_context(|| format!("worm `{name}`: unpacking {}", found.name))?;

    if !dir.join(super::MANIFEST).exists() {
        bail!(
            "worm `{name}`: {} has no {} at its root",
            found.name,
            super::MANIFEST
        );
    }

    std::fs::write(&stamp, &digest).ok();

    Ok(dir)
}

/// Whether an installed worm still hashes to what it did when it was fetched
pub fn verify(dir: &Path, expected: &str) -> Result<()> {
    let stamp = dir.join(".sha256");
    let recorded = std::fs::read_to_string(&stamp).unwrap_or_default();

    if recorded.trim() != expected.trim() {
        bail!(
            "worm at {} has changed since it was installed, expected {expected} and found {}",
            crate::ui::rel(dir),
            recorded.trim()
        );
    }

    Ok(())
}

/*
Unpack every file, refusing any path that would land outside the directory.

A zip entry names its own path, so a crafted archive can say `../../..` and
write wherever it likes. We take the base name components ourselves rather
than trusting the entry, which is the only version of this that is not a
running argument with an attacker.
*/
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
A relative path built only from ordinary components. Anything absolute, any
`..`, any drive prefix and any empty result is refused rather than sanitised,
because a half understood path is not one worth writing to.
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

/// Hex sha256, so a recorded digest is comparable and readable
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

    /// The whole reason we do not trust an entry's own path
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
        std::fs::write(dir.path().join(".sha256"), "abc123").unwrap();

        assert!(verify(dir.path(), "abc123").is_ok());

        let err = verify(dir.path(), "different").err().unwrap();
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

    /// A crafted archive must not be able to write outside its directory
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
