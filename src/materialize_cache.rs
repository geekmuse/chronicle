//! Materialization state cache (`materialize-state.json`) — §US-002.
//!
//! # Design
//!
//! [`MaterializeCache`] tracks the mtime/size of **repo working-tree files**
//! at the time they were last materialized to local agent directories.  On
//! subsequent materialization passes, any repo file whose mtime/size has not
//! changed since the last pass can be skipped entirely — no read, no
//! de-canonicalization, no local write comparison needed.
//!
//! A `config_hash` field records a hash of the canonicalization configuration
//! (level + home_token).  When the loaded hash differs from the current
//! configuration, the cache is invalidated and a full re-materialization is
//! triggered (US-003).
//!
//! Patterns mirror [`crate::scan::StateCache`] exactly:
//! - `HashMap<String, …>` keyed by repo-relative path
//! - JSON serialization with atomic write (temp file + rename)
//! - `path_for_repo` co-location: sibling of the repo directory

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// ─── Per-file state ────────────────────────────────────────────────────────────

/// Per-file entry in `materialize-state.json`.
///
/// Records the mtime and size of a **repo working-tree file** as observed
/// at the end of the last successful materialization pass.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MaterializeFileState {
    /// Last-modified timestamp of the repo file at last materialization.
    pub repo_mtime: DateTime<Utc>,
    /// File size in bytes of the repo file at last materialization.
    pub repo_size: u64,
}

// ─── Cache document ────────────────────────────────────────────────────────────

/// Top-level document persisted to `<repo_path>/../materialize-state.json`.
///
/// The map key is a **repo-relative path** such as
/// `pi/sessions/--{{SYNC_HOME}}-Dev-foo--/session1.jsonl`, matching the keys
/// used throughout the sync pipeline.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MaterializeCache {
    /// Map of repo-relative file path → cached mtime/size.
    pub files: HashMap<String, MaterializeFileState>,

    /// Hash of the canonicalization configuration at the time the cache was
    /// written (e.g. `"<level>:<home_token>"`).  If this differs from the
    /// current configuration, the cache must be cleared and a full
    /// re-materialization performed.
    #[serde(default)]
    pub config_hash: String,
}

impl MaterializeCache {
    /// Load the materialize cache from `path`.
    ///
    /// Returns an empty cache when the file does not exist.  Any other I/O
    /// or JSON parse error is propagated.
    pub fn load(path: &Path) -> std::io::Result<Self> {
        match fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e),
        }
    }

    /// Atomically persist the materialize cache to `path`.
    ///
    /// Writes to a `.mcache.<pid>.<nanos>.tmp` sibling file then renames it
    /// to the target path so a crash mid-write never leaves a partial file.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let parent = path.parent().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "materialize-state.json path has no parent directory",
            )
        })?;
        fs::create_dir_all(parent)?;

        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let tmp = parent.join(format!(".mcache.{}.{nanos}.tmp", std::process::id()));

        let text = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        fs::write(&tmp, text)?;
        fs::rename(&tmp, path)?;
        Ok(())
    }

    /// Return the cache path co-located with a specific repo directory.
    ///
    /// Path: `<repo_path>/../materialize-state.json` (sibling of the repo
    /// dir), mirroring the `StateCache::path_for_repo` convention.
    #[must_use]
    pub fn path_for_repo(repo_path: &Path) -> PathBuf {
        repo_path
            .parent()
            .unwrap_or(repo_path)
            .join("materialize-state.json")
    }
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;
    use tempfile::tempdir;

    #[test]
    fn load_missing_file_returns_empty_cache() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("materialize-state.json");
        let cache = MaterializeCache::load(&path).unwrap();
        assert!(cache.files.is_empty());
        assert!(cache.config_hash.is_empty());
    }

    #[test]
    fn save_and_load_round_trip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("materialize-state.json");

        let mtime = Utc.timestamp_opt(1_700_000_000, 0).unwrap();
        let mut cache = MaterializeCache::default();
        cache.config_hash = "l2:{{SYNC_HOME}}".to_owned();
        cache.files.insert(
            "pi/sessions/--{{SYNC_HOME}}-Dev-foo--/session.jsonl".to_owned(),
            MaterializeFileState {
                repo_mtime: mtime,
                repo_size: 2048,
            },
        );
        cache.save(&path).unwrap();

        let loaded = MaterializeCache::load(&path).unwrap();
        assert_eq!(loaded.config_hash, "l2:{{SYNC_HOME}}");
        assert_eq!(loaded.files.len(), 1);
        let state = loaded
            .files
            .get("pi/sessions/--{{SYNC_HOME}}-Dev-foo--/session.jsonl")
            .unwrap();
        assert_eq!(state.repo_size, 2048);
        assert_eq!(state.repo_mtime, mtime);
    }

    #[test]
    fn save_creates_parent_directories() {
        let dir = tempdir().unwrap();
        let path = dir
            .path()
            .join("a")
            .join("b")
            .join("materialize-state.json");
        let cache = MaterializeCache::default();
        cache.save(&path).unwrap();
        assert!(path.exists());
    }

    #[test]
    fn path_for_repo_returns_expected_suffix() {
        let repo = PathBuf::from("/home/user/.config/chronicle/repo");
        let p = MaterializeCache::path_for_repo(&repo);
        assert!(
            p.ends_with("materialize-state.json"),
            "expected path ending with materialize-state.json, got {p:?}"
        );
        // Sibling of repo dir, not inside it.
        assert_eq!(
            p,
            PathBuf::from("/home/user/.config/chronicle/materialize-state.json")
        );
    }

    #[test]
    fn path_for_repo_root_falls_back_gracefully() {
        // When repo_path has no parent (e.g. a bare "/" path), fall back to
        // using repo_path itself as the directory.
        let root = PathBuf::from("/");
        let p = MaterializeCache::path_for_repo(&root);
        assert!(p.ends_with("materialize-state.json"));
    }

    #[test]
    fn default_has_empty_config_hash() {
        let cache = MaterializeCache::default();
        assert!(cache.config_hash.is_empty());
    }
}
