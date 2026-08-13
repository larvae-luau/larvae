/*!
The incremental build cache.

A content hash per file is not sufficient here. Require resolution reads other
files. Thus an edit to `.luaurc`, or a new sibling file that creates an
ambiguity, changes the correct answer for files that did not change.

For this reason the key carries a resolution epoch. The epoch is a hash of
every input that resolution consults. Any change to the epoch rebuilds all
files. This method is coarse, but the cache is never stale.
*/

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use xxhash_rust::xxh3::xxh3_64;

/// Increase this value when the cache layout or the meaning of a hash changes
const FORMAT: u32 = 1;

pub fn hash_bytes(bytes: &[u8]) -> u64 {
    xxh3_64(bytes)
}

/// Add one more value to a running hash
pub fn mix(acc: u64, bytes: &[u8]) -> u64 {
    xxh3_64(&[&acc.to_le_bytes()[..], bytes].concat())
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct CacheFile {
    format: u32,
    epoch: u64,
    /// The content hashes of the output, keyed by the path relative to the input root
    files: HashMap<String, u64>,
}

pub struct Cache {
    path: PathBuf,
    epoch: u64,
    entries: HashMap<String, u64>,
    enabled: bool,
}

impl Cache {
    /// Load the cache. A change of the epoch or the format starts from empty.
    pub fn load(cache_dir: &Path, epoch: u64, enabled: bool) -> Self {
        let path = cache_dir.join("build-cache.json");

        let entries = if enabled {
            std::fs::read_to_string(&path)
                .ok()
                .and_then(|t| serde_json::from_str::<CacheFile>(&t).ok())
                .filter(|c| c.format == FORMAT && c.epoch == epoch)
                .map(|c| c.files)
                .unwrap_or_default()
        } else {
            HashMap::new()
        };

        Self {
            path,
            epoch,
            entries,
            enabled,
        }
    }

    /// This is true when larvae can skip the file. The source is unchanged,
    /// and the output of the last run is still on disk.
    pub fn is_fresh(&self, rel: &str, source_hash: u64, output: &Path) -> bool {
        self.enabled && self.entries.get(rel) == Some(&source_hash) && output.exists()
    }

    pub fn record(&mut self, rel: String, source_hash: u64) {
        if self.enabled {
            self.entries.insert(rel, source_hash);
        }
    }

    /// Remove the entries for files that do not exist anymore
    pub fn retain(&mut self, keep: &HashSet<String>) {
        self.entries.retain(|k, _| keep.contains(k));
    }

    pub fn save(&self) -> std::io::Result<()> {
        if !self.enabled {
            return Ok(());
        }

        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let data = CacheFile {
            format: FORMAT,
            epoch: self.epoch,
            files: self.entries.clone(),
        };

        let text = serde_json::to_string(&data)?;
        let tmp = self.path.with_extension("json.tmp");

        std::fs::write(&tmp, text)?;

        std::fs::rename(&tmp, &self.path)
    }
}

/// Every input that require resolution depends on, beyond the bytes of the file
#[derive(Default)]
pub struct EpochInputs {
    acc: u64,
}

impl EpochInputs {
    pub fn new() -> Self {
        Self {
            acc: hash_bytes(env!("CARGO_PKG_VERSION").as_bytes()),
        }
    }

    pub fn add(&mut self, bytes: &[u8]) {
        self.acc = mix(self.acc, bytes);
    }

    pub fn add_file(&mut self, path: &Path) {
        self.add(path.to_string_lossy().as_bytes());

        if let Ok(bytes) = std::fs::read(path) {
            self.add(&bytes);
        }
    }

    /// The set of source paths is part of the key. A file that is added or
    /// removed can create or resolve a module ambiguity for other files.
    pub fn add_paths(&mut self, paths: &mut [PathBuf]) {
        paths.sort();

        for p in paths.iter() {
            self.add(p.to_string_lossy().as_bytes());
        }
    }

    pub fn finish(self) -> u64 {
        self.acc
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_changes_with_any_input() {
        let mut a = EpochInputs::new();
        a.add(b"one");

        let mut b = EpochInputs::new();
        b.add(b"one");
        b.add(b"two");

        assert_ne!(a.finish(), b.finish());
    }

    #[test]
    fn epoch_mismatch_drops_entries() {
        let tmp = tempfile::tempdir().unwrap();
        let mut c = Cache::load(tmp.path(), 1, true);

        c.record("a.luau".into(), 42);
        c.save().unwrap();

        let same = Cache::load(tmp.path(), 1, true);
        assert!(same.is_fresh("a.luau", 42, tmp.path()));

        let moved = Cache::load(tmp.path(), 2, true);
        assert!(!moved.is_fresh("a.luau", 42, tmp.path()));
    }

    #[test]
    fn disabled_cache_never_reports_fresh() {
        let tmp = tempfile::tempdir().unwrap();
        let mut c = Cache::load(tmp.path(), 1, false);

        c.record("a.luau".into(), 42);
        assert!(!c.is_fresh("a.luau", 42, tmp.path()));
    }
}
