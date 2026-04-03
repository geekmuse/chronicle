//! Sync-state persistence (`sync_state.json`) — US-001.
//!
//! # Design
//!
//! [`SyncState`] records the timestamp, duration, and operation type of the
//! last successful sync, push, or pull.  It is the data source for the
//! "Last Sync" section of `chronicle status`.
//!
//! The file is written **atomically** (serialize to a `.tmp` sibling then
//! [`fs::rename`]) co-located with `chronicle.lock` in the repo parent
//! directory, following the same convention as [`crate::scan::StateCache`]
//! and [`crate::materialize_cache::MaterializeCache`].
//!
//! Path: `<repo_path>/../sync_state.json`

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// ─── Operation type ───────────────────────────────────────────────────────────

/// The chronicle operation that produced this sync-state record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SyncOp {
    /// `chronicle sync`
    Sync,
    /// `chronicle push`
    Push,
    /// `chronicle pull`
    Pull,
}

// ─── Sync state document ──────────────────────────────────────────────────────

/// Persisted record of the last successful sync/push/pull.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncState {
    /// UTC timestamp of the last successful operation.
    pub last_sync_time: DateTime<Utc>,
    /// Elapsed wall-clock duration of the last successful operation in
    /// milliseconds.
    pub last_sync_duration_ms: u64,
    /// Which operation produced this record.
    pub last_sync_op: SyncOp,
}

// ─── Path helper ──────────────────────────────────────────────────────────────

/// Return the path of `sync_state.json`, co-located with `chronicle.lock`.
///
/// Path: `<repo_path>/../sync_state.json` (sibling of the repo directory).
#[must_use]
pub fn sync_state_path(repo_path: &Path) -> PathBuf {
    repo_path
        .parent()
        .unwrap_or(repo_path)
        .join("sync_state.json")
}

// ─── Write helper ─────────────────────────────────────────────────────────────

/// Atomically write a new [`SyncState`] record.
///
/// Serializes to a `.sync_state.<pid>.<nanos>.tmp` sibling file then renames
/// it to the target path so a crash mid-write never leaves a partial file.
///
/// # Errors
///
/// Returns `Err` on any serialization or I/O failure.
pub fn write_sync_state(repo_path: &Path, op: SyncOp, duration: Duration) -> std::io::Result<()> {
    let path = sync_state_path(repo_path);
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "sync_state.json path has no parent directory",
        )
    })?;
    fs::create_dir_all(parent)?;

    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let tmp = parent.join(format!(".sync_state.{}.{nanos}.tmp", std::process::id()));

    let state = SyncState {
        last_sync_time: Utc::now(),
        last_sync_duration_ms: duration.as_millis() as u64,
        last_sync_op: op,
    };

    let text = serde_json::to_string_pretty(&state).map_err(std::io::Error::other)?;
    fs::write(&tmp, text)?;
    fs::rename(&tmp, &path)?;
    Ok(())
}

// ─── Read helper ──────────────────────────────────────────────────────────────

/// Read the persisted [`SyncState`] from `repo_path`.
///
/// Returns `Ok(None)` when the file does not exist (first run or never
/// synced).  Any other I/O or JSON parse error is propagated.
pub fn read_sync_state(repo_path: &Path) -> std::io::Result<Option<SyncState>> {
    let path = sync_state_path(repo_path);
    match fs::read_to_string(&path) {
        Ok(text) => {
            let state = serde_json::from_str(&text)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
            Ok(Some(state))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn sync_state_path_returns_expected_path() {
        let repo = PathBuf::from("/home/user/.config/chronicle/repo");
        let p = sync_state_path(&repo);
        assert_eq!(
            p,
            PathBuf::from("/home/user/.config/chronicle/sync_state.json")
        );
    }

    #[test]
    fn sync_state_path_fallback_at_root() {
        // When repo_path has no parent (e.g. "/") fall back gracefully.
        let root = PathBuf::from("/");
        let p = sync_state_path(&root);
        assert!(p.ends_with("sync_state.json"));
    }

    #[test]
    fn read_sync_state_missing_file_returns_none() {
        let dir = tempdir().unwrap();
        // Use a fake "repo" dir so the sibling path resolves inside tempdir.
        let repo = dir.path().join("repo");
        let result = read_sync_state(&repo).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn write_then_read_round_trip_sync() {
        let dir = tempdir().unwrap();
        let repo = dir.path().join("repo");

        write_sync_state(&repo, SyncOp::Sync, Duration::from_millis(1234)).unwrap();

        let state = read_sync_state(&repo)
            .unwrap()
            .expect("state should be present");
        assert_eq!(state.last_sync_op, SyncOp::Sync);
        assert_eq!(state.last_sync_duration_ms, 1234);
    }

    #[test]
    fn write_then_read_round_trip_push() {
        let dir = tempdir().unwrap();
        let repo = dir.path().join("repo");

        write_sync_state(&repo, SyncOp::Push, Duration::from_millis(500)).unwrap();

        let state = read_sync_state(&repo)
            .unwrap()
            .expect("state should be present");
        assert_eq!(state.last_sync_op, SyncOp::Push);
        assert_eq!(state.last_sync_duration_ms, 500);
    }

    #[test]
    fn write_then_read_round_trip_pull() {
        let dir = tempdir().unwrap();
        let repo = dir.path().join("repo");

        write_sync_state(&repo, SyncOp::Pull, Duration::from_millis(0)).unwrap();

        let state = read_sync_state(&repo)
            .unwrap()
            .expect("state should be present");
        assert_eq!(state.last_sync_op, SyncOp::Pull);
        assert_eq!(state.last_sync_duration_ms, 0);
    }

    #[test]
    fn write_creates_parent_directories() {
        let dir = tempdir().unwrap();
        // Nested path — directories must be created automatically.
        let repo = dir.path().join("a").join("b").join("repo");

        write_sync_state(&repo, SyncOp::Sync, Duration::from_millis(100)).unwrap();

        let path = sync_state_path(&repo);
        assert!(path.exists(), "sync_state.json should exist at {path:?}");
    }

    #[test]
    fn serde_op_uses_lowercase_strings() {
        // Confirm the on-disk representation is lowercase (not PascalCase).
        let dir = tempdir().unwrap();
        let repo = dir.path().join("repo");

        write_sync_state(&repo, SyncOp::Push, Duration::from_millis(1)).unwrap();

        let raw = fs::read_to_string(sync_state_path(&repo)).unwrap();
        assert!(
            raw.contains("\"push\""),
            "expected lowercase 'push' in JSON, got: {raw}"
        );
    }

    #[test]
    fn last_sync_time_is_recent() {
        let dir = tempdir().unwrap();
        let repo = dir.path().join("repo");
        let before = Utc::now();

        write_sync_state(&repo, SyncOp::Sync, Duration::from_millis(50)).unwrap();

        let state = read_sync_state(&repo).unwrap().unwrap();
        let after = Utc::now();
        assert!(
            state.last_sync_time >= before && state.last_sync_time <= after,
            "last_sync_time should be between {before} and {after}, got {}",
            state.last_sync_time
        );
    }
}
