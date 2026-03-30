//! File change detection via mtime/size comparison against a persisted
//! state cache (`state.json`) — §14.1.
//!
//! # Design
//!
//! [`StateCache`] maps string keys → [`FileState`].  When used from the
//! sync pipeline (US-017), keys will be canonical repo-relative paths (e.g.
//! `pi/sessions/--{{SYNC_HOME}}-Dev-foo--/session1.jsonl`).  In isolation
//! the scanner uses the absolute local path as the key so unit tests do not
//! depend on the canonicalization module.
//!
//! [`scan_dir`] walks a session directory recursively for `.jsonl` files,
//! classifies each against the cache, and enforces the `follow_symlinks`
//! policy on the **top-level** directory only.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// ─── State cache ──────────────────────────────────────────────────────────────

/// Per-file state entry in `state.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileState {
    /// Last-modified timestamp recorded when the file was last processed.
    pub local_mtime: DateTime<Utc>,
    /// File size in bytes recorded when the file was last processed.
    pub local_size: u64,
    /// File size at the time of the last successful sync push.
    pub last_synced_size: u64,
    /// Absolute path to the file on the local filesystem.
    pub local_path: PathBuf,
}

/// Top-level document persisted to
/// `~/.local/share/chronicle/state.json` (§14.1).
///
/// The map key is a canonical repo-relative path such as
/// `pi/sessions/--{{SYNC_HOME}}-Dev-foo--/session1.jsonl`.  When only the
/// local path is known (e.g. in scanner unit tests), the absolute path
/// string is used as a temporary key until US-017 wires canonicalization.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StateCache {
    /// Map of file key → cached state.
    pub files: HashMap<String, FileState>,
}

impl StateCache {
    /// Load the state cache from `path`.
    ///
    /// Returns an empty (all-new) cache when the file does not exist.
    /// Any other I/O or JSON parse error is propagated.
    pub fn load(path: &Path) -> std::io::Result<Self> {
        match fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e),
        }
    }

    /// Atomically persist the state cache to `path`.
    ///
    /// Writes to a `.state.<pid>.<nanos>.tmp` sibling file then renames it
    /// to the target path so a crash mid-write never leaves a partial file.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let parent = path.parent().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "state.json path has no parent directory",
            )
        })?;
        fs::create_dir_all(parent)?;

        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let tmp = parent.join(format!(".state.{}.{nanos}.tmp", std::process::id()));

        let text = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        fs::write(&tmp, text)?;
        fs::rename(&tmp, path)?;
        Ok(())
    }

    /// XDG-compliant default path: `~/.local/share/chronicle/state.json`.
    ///
    /// # Deprecation
    ///
    /// This method is superseded by [`StateCache::path_for_repo`], which
    /// co-locates the cache with a specific sync repository and avoids
    /// global-path race conditions when multiple repos are in use.
    #[must_use]
    #[deprecated(
        since = "0.2.2",
        note = "Use `StateCache::path_for_repo` instead; it co-locates the \
                cache with the sync repo and prevents race conditions in \
                concurrent multi-repo setups."
    )]
    pub fn default_path() -> PathBuf {
        dirs::data_local_dir()
            .unwrap_or_else(|| {
                dirs::home_dir()
                    .unwrap_or_else(|| PathBuf::from("."))
                    .join(".local")
                    .join("share")
            })
            .join("chronicle")
            .join("state.json")
    }

    /// Return the state cache path co-located with a specific repo directory.
    ///
    /// Placing the cache next to the repo keeps it isolated to that
    /// installation, which also prevents test parallelism races when multiple
    /// tests each use a distinct tempdir for `storage.repo_path`.
    ///
    /// Path: `<repo_path>/../state.json` (sibling of the repo dir).
    #[must_use]
    pub fn path_for_repo(repo_path: &std::path::Path) -> PathBuf {
        repo_path.parent().unwrap_or(repo_path).join("state.json")
    }
}

// ─── Scanner ──────────────────────────────────────────────────────────────────

/// Classification of a scanned file relative to the state cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    /// File is not present in the cache (first run or genuinely new file).
    New,
    /// File is present in the cache but mtime or size has changed.
    Modified,
    /// File matches cached mtime and size — can be skipped.
    Unchanged,
}

/// Result for a single `.jsonl` file found during [`scan_dir`].
#[derive(Debug)]
pub struct ScanEntry {
    /// Absolute path to the file on the local filesystem.
    pub path: PathBuf,
    /// Current last-modified time reported by the OS.
    pub mtime: DateTime<Utc>,
    /// Current file size in bytes.
    pub size: u64,
    /// How this file compares to the cached state.
    pub kind: ChangeKind,
}

/// Errors that can occur during [`scan_dir`].
#[derive(Debug, thiserror::Error)]
pub enum ScanError {
    /// The session directory is a symbolic link and
    /// `follow_symlinks = false`.
    #[error("symlink refused: `{0}` is a symbolic link and follow_symlinks=false")]
    SymlinkRefused(PathBuf),

    /// An underlying I/O error was encountered while scanning.
    #[error("I/O error while scanning `{path}`: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// Convert a [`SystemTime`] to [`DateTime<Utc>`].
fn system_time_to_utc(st: SystemTime) -> DateTime<Utc> {
    DateTime::<Utc>::from(st)
}

/// Recursively collect all `.jsonl` files under `dir`.
///
/// Symbolic links encountered during the walk (files or sub-directories
/// other than `dir` itself) are skipped silently; only the top-level
/// directory symlink is subject to policy (handled by the caller).
fn collect_jsonl(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let ft = entry.file_type()?;
        let path = entry.path();
        if ft.is_symlink() {
            // Skip symlinked entries inside the directory.
            continue;
        }
        if ft.is_dir() {
            out.extend(collect_jsonl(&path)?);
        } else if ft.is_file() && path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
            out.push(path);
        }
    }
    Ok(out)
}

/// Classify a single file against `cache` using its absolute path string as
/// the lookup key.
fn classify(path: &Path, mtime: DateTime<Utc>, size: u64, cache: &StateCache) -> ChangeKind {
    let key = path.to_string_lossy().into_owned();
    match cache.files.get(&key) {
        None => ChangeKind::New,
        Some(state) => {
            if state.local_mtime == mtime && state.local_size == size {
                ChangeKind::Unchanged
            } else {
                ChangeKind::Modified
            }
        }
    }
}

/// Scan `dir` for `.jsonl` files and classify each against the state `cache`.
///
/// # Symlink policy (top-level `dir` only)
///
/// | `follow_symlinks` | `dir` is a symlink | Outcome |
/// |---|---|---|
/// | `false` | yes | Error logged, returns `Err(ScanError::SymlinkRefused)` |
/// | `true`  | yes | Warning logged, scan proceeds through the symlink |
/// | any     | no  | Normal scan |
///
/// Symbolic links encountered *inside* `dir` (files or sub-directories)
/// are skipped silently regardless of the policy flag.
///
/// # Errors
///
/// Returns [`ScanError::SymlinkRefused`] when `dir` is a symlink and
/// `follow_symlinks = false`.  Returns [`ScanError::Io`] on any I/O failure.
pub fn scan_dir(
    dir: &Path,
    cache: &StateCache,
    follow_symlinks: bool,
) -> Result<Vec<ScanEntry>, ScanError> {
    // Inspect `dir` without following symlinks.
    let dir_meta = fs::symlink_metadata(dir).map_err(|e| ScanError::Io {
        path: dir.to_owned(),
        source: e,
    })?;

    if dir_meta.file_type().is_symlink() {
        if !follow_symlinks {
            tracing::error!(
                path = %dir.display(),
                "symlink refused: directory is a symbolic link and \
                 follow_symlinks=false — skipping"
            );
            return Err(ScanError::SymlinkRefused(dir.to_owned()));
        }
        tracing::warn!(
            path = %dir.display(),
            "following symbolic link (follow_symlinks=true) — \
             files outside expected session directory may be exposed"
        );
    }

    let paths = collect_jsonl(dir).map_err(|e| ScanError::Io {
        path: dir.to_owned(),
        source: e,
    })?;

    let mut entries = Vec::with_capacity(paths.len());
    for path in paths {
        let meta = fs::metadata(&path).map_err(|e| ScanError::Io {
            path: path.clone(),
            source: e,
        })?;
        let mtime = meta
            .modified()
            .map(system_time_to_utc)
            .unwrap_or(DateTime::UNIX_EPOCH);
        let size = meta.len();
        let kind = classify(&path, mtime, size, cache);
        entries.push(ScanEntry {
            path,
            mtime,
            size,
            kind,
        });
    }

    Ok(entries)
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;
    use std::fs;
    use tempfile::tempdir;

    // ── StateCache ────────────────────────────────────────────────────────────

    #[test]
    fn load_missing_file_returns_empty_cache() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("state.json");
        let cache = StateCache::load(&path).unwrap();
        assert!(cache.files.is_empty());
    }

    #[test]
    fn save_and_load_round_trip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("state.json");

        let mtime = Utc.timestamp_opt(1_700_000_000, 0).unwrap();
        let mut cache = StateCache::default();
        cache.files.insert(
            "pi/sessions/--home--/session.jsonl".to_owned(),
            FileState {
                local_mtime: mtime,
                local_size: 1024,
                last_synced_size: 1024,
                local_path: PathBuf::from("/home/user/.pi/agent/sessions/session.jsonl"),
            },
        );
        cache.save(&path).unwrap();

        let loaded = StateCache::load(&path).unwrap();
        assert_eq!(loaded.files.len(), 1);
        let state = loaded
            .files
            .get("pi/sessions/--home--/session.jsonl")
            .unwrap();
        assert_eq!(state.local_size, 1024);
        assert_eq!(state.last_synced_size, 1024);
        assert_eq!(state.local_mtime, mtime);
    }

    #[test]
    fn save_creates_parent_directories() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("a").join("b").join("state.json");
        let cache = StateCache::default();
        cache.save(&path).unwrap();
        assert!(path.exists());
    }

    #[test]
    #[allow(deprecated)]
    fn default_path_ends_with_expected_suffix() {
        let p = StateCache::default_path();
        assert!(p.ends_with("chronicle/state.json"));
    }

    // ── scan_dir: new / modified / unchanged detection ────────────────────────

    #[test]
    fn first_run_no_cache_all_files_are_new() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("a.jsonl"), b"{}").unwrap();
        fs::write(dir.path().join("b.jsonl"), b"{}").unwrap();

        let cache = StateCache::default();
        let entries = scan_dir(dir.path(), &cache, false).unwrap();
        assert_eq!(entries.len(), 2);
        assert!(entries.iter().all(|e| e.kind == ChangeKind::New));
    }

    #[test]
    fn new_file_not_in_cache_is_new() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("session.jsonl");
        fs::write(&file, b"{}").unwrap();

        let cache = StateCache::default();
        let entries = scan_dir(dir.path(), &cache, false).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].kind, ChangeKind::New);
        assert_eq!(entries[0].path, file);
    }

    #[test]
    fn unchanged_file_matches_cache() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("session.jsonl");
        fs::write(&file, b"{}").unwrap();

        // Capture the actual mtime + size the OS reports.
        let meta = fs::metadata(&file).unwrap();
        let mtime = meta.modified().map(system_time_to_utc).unwrap();
        let size = meta.len();

        let mut cache = StateCache::default();
        cache.files.insert(
            file.to_string_lossy().into_owned(),
            FileState {
                local_mtime: mtime,
                local_size: size,
                last_synced_size: size,
                local_path: file.clone(),
            },
        );

        let entries = scan_dir(dir.path(), &cache, false).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].kind, ChangeKind::Unchanged);
    }

    #[test]
    fn modified_file_size_change_detected() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("session.jsonl");
        fs::write(&file, b"{}").unwrap();

        let meta = fs::metadata(&file).unwrap();
        let mtime = meta.modified().map(system_time_to_utc).unwrap();

        let mut cache = StateCache::default();
        cache.files.insert(
            file.to_string_lossy().into_owned(),
            FileState {
                local_mtime: mtime,
                local_size: 9999, // stale size
                last_synced_size: 9999,
                local_path: file.clone(),
            },
        );

        let entries = scan_dir(dir.path(), &cache, false).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].kind, ChangeKind::Modified);
    }

    #[test]
    fn modified_file_mtime_change_detected() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("session.jsonl");
        fs::write(&file, b"{}").unwrap();

        let meta = fs::metadata(&file).unwrap();
        let size = meta.len();
        // Stale timestamp — older than the actual file mtime.
        let stale = Utc.timestamp_opt(0, 0).unwrap();

        let mut cache = StateCache::default();
        cache.files.insert(
            file.to_string_lossy().into_owned(),
            FileState {
                local_mtime: stale,
                local_size: size,
                last_synced_size: size,
                local_path: file.clone(),
            },
        );

        let entries = scan_dir(dir.path(), &cache, false).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].kind, ChangeKind::Modified);
    }

    #[test]
    fn non_jsonl_files_are_ignored() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("notes.txt"), b"ignored").unwrap();
        fs::write(dir.path().join("data.json"), b"ignored").unwrap();
        fs::write(dir.path().join("session.jsonl"), b"{}").unwrap();

        let cache = StateCache::default();
        let entries = scan_dir(dir.path(), &cache, false).unwrap();
        assert_eq!(entries.len(), 1);
        assert!(entries[0].path.ends_with("session.jsonl"));
    }

    #[test]
    fn empty_directory_returns_no_entries() {
        let dir = tempdir().unwrap();
        let cache = StateCache::default();
        let entries = scan_dir(dir.path(), &cache, false).unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn nested_subdirectories_are_scanned() {
        let dir = tempdir().unwrap();
        let sub = dir.path().join("sessions").join("proj");
        fs::create_dir_all(&sub).unwrap();
        fs::write(sub.join("session.jsonl"), b"{}").unwrap();

        let cache = StateCache::default();
        let entries = scan_dir(dir.path(), &cache, false).unwrap();
        assert_eq!(entries.len(), 1);
        assert!(entries[0].path.ends_with("session.jsonl"));
    }

    // ── scan_dir: symlink policy ───────────────────────────────────────────────

    #[cfg(unix)]
    #[test]
    fn symlink_refused_when_follow_symlinks_false() {
        use std::os::unix::fs::symlink;

        let base = tempdir().unwrap();
        let real_dir = base.path().join("real");
        let link_dir = base.path().join("link");
        fs::create_dir(&real_dir).unwrap();
        fs::write(real_dir.join("session.jsonl"), b"{}").unwrap();
        symlink(&real_dir, &link_dir).unwrap();

        let cache = StateCache::default();
        let result = scan_dir(&link_dir, &cache, false);
        assert!(matches!(result, Err(ScanError::SymlinkRefused(_))));
    }

    #[cfg(unix)]
    #[test]
    fn symlink_followed_when_follow_symlinks_true() {
        use std::os::unix::fs::symlink;

        let base = tempdir().unwrap();
        let real_dir = base.path().join("real");
        let link_dir = base.path().join("link");
        fs::create_dir(&real_dir).unwrap();
        fs::write(real_dir.join("session.jsonl"), b"{}").unwrap();
        symlink(&real_dir, &link_dir).unwrap();

        let cache = StateCache::default();
        let entries = scan_dir(&link_dir, &cache, true).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].kind, ChangeKind::New);
    }
}
