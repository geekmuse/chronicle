// Items in this module are fully wired by US-015/US-017/US-018 (sync and CLI
// commands). Allow dead-code until those stories call append() and read().
#![allow(dead_code)]

//! 30-entry error ring buffer stored as JSONL (§11.1).
//!
//! All mutating operations are atomic: entries are serialized to a temporary
//! file adjacent to the target path, then `rename`d into place.  POSIX
//! `rename(2)` is atomic within the same filesystem, so a crash mid-write
//! never leaves a partially written file.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::ChronicleError;

/// Maximum number of entries the ring buffer retains before rotating.
pub const RING_BUFFER_CAPACITY: usize = 30;

/// Severity level for an error ring buffer entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// The event represents a hard error that prevented an operation.
    Error,
    /// The event represents a recoverable warning.
    Warning,
    /// Informational event (e.g., sync activity summary).
    Info,
}

/// A single entry in the error ring buffer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorEntry {
    /// ISO 8601 UTC timestamp when the event occurred.
    pub timestamp: DateTime<Utc>,
    /// Severity level.
    pub severity: Severity,
    /// Error category — one of the seven defined in §11.1.
    pub category: String,
    /// Repository-relative file path associated with the error, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// Human-readable description of what went wrong.
    pub message: String,
    /// Extra diagnostic context (e.g., commit hashes, raw line content).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl ErrorEntry {
    /// Create a new entry timestamped at the current UTC time.
    #[must_use]
    pub fn new(
        severity: Severity,
        category: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            timestamp: Utc::now(),
            severity,
            category: category.into(),
            file: None,
            message: message.into(),
            detail: None,
        }
    }

    /// Set the associated file path (builder method).
    #[must_use]
    pub fn with_file(mut self, file: impl Into<String>) -> Self {
        self.file = Some(file.into());
        self
    }

    /// Set the detail field (builder method).
    #[must_use]
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }
}

/// A 30-entry JSONL ring buffer persisted to `path`.
///
/// - [`RingBuffer::append`] serializes the updated entry list atomically.
/// - [`RingBuffer::read`] returns all entries or the last *N*.
/// - [`RingBuffer::clear`] truncates the file to zero entries.
#[derive(Debug)]
pub struct RingBuffer {
    path: PathBuf,
    capacity: usize,
}

impl RingBuffer {
    /// Create a ring buffer backed by `path`.
    #[must_use]
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            capacity: RING_BUFFER_CAPACITY,
        }
    }

    /// Returns the XDG-compliant default path:
    /// `~/.local/share/chronicle/errors.jsonl`
    #[must_use]
    pub fn default_path() -> PathBuf {
        dirs::data_local_dir()
            .unwrap_or_else(|| {
                dirs::home_dir()
                    .unwrap_or_else(|| PathBuf::from("."))
                    .join(".local")
                    .join("share")
            })
            .join("chronicle")
            .join("errors.jsonl")
    }

    /// Return an `errors.jsonl` path co-located with `repo_path`.
    ///
    /// Placing the ring buffer next to the repository keeps it isolated to that
    /// installation and avoids races when multiple tests each use a distinct
    /// `storage.repo_path` tempdir.
    ///
    /// Path: `<repo_path>/../errors.jsonl` (sibling of the repo dir).
    #[must_use]
    pub fn path_for_repo(repo_path: &std::path::Path) -> PathBuf {
        repo_path.parent().unwrap_or(repo_path).join("errors.jsonl")
    }

    /// Append `entry` and rotate the buffer to at most [`RING_BUFFER_CAPACITY`]
    /// entries, dropping the oldest when the limit is exceeded.
    ///
    /// The parent directory is created if it does not exist.
    pub fn append(&self, entry: ErrorEntry) -> Result<(), ChronicleError> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut entries = self.load()?;
        entries.push(entry);
        if entries.len() > self.capacity {
            let drop_count = entries.len() - self.capacity;
            entries.drain(..drop_count);
        }
        self.write_atomic(&entries)
    }

    /// Return up to `limit` of the most-recent entries, or all entries if
    /// `limit` is `None`.
    ///
    /// Returns an empty list when the backing file does not exist.
    pub fn read(&self, limit: Option<usize>) -> Result<Vec<ErrorEntry>, ChronicleError> {
        let entries = self.load()?;
        Ok(match limit {
            None => entries,
            Some(n) => {
                let start = entries.len().saturating_sub(n);
                entries[start..].to_vec()
            }
        })
    }

    /// Remove all entries by atomically replacing the file with an empty one.
    pub fn clear(&self) -> Result<(), ChronicleError> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        self.write_atomic(&[])
    }

    // ── private helpers ──────────────────────────────────────────────────────

    /// Read all entries from disk.  Malformed or empty lines are skipped
    /// without error.
    fn load(&self) -> Result<Vec<ErrorEntry>, ChronicleError> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }
        let file = fs::File::open(&self.path)?;
        let reader = BufReader::new(file);
        Ok(reader
            .lines()
            .filter_map(|line| {
                let line = line.ok()?;
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    return None;
                }
                serde_json::from_str(trimmed).ok()
            })
            .collect())
    }

    /// Serialize `entries` to a temporary file in the same directory, then
    /// atomically rename it to `self.path`.
    fn write_atomic(&self, entries: &[ErrorEntry]) -> Result<(), ChronicleError> {
        let parent = self.path.parent().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "ring buffer path has no parent directory",
            )
        })?;

        // Use PID + nanoseconds to make the temp name unique enough for our
        // single-writer use case (chronicle is not concurrent).
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let tmp_path = parent.join(format!(".errors.{}.{}.tmp", std::process::id(), nanos));

        {
            let mut tmp = fs::File::create(&tmp_path)?;
            for e in entries {
                let line = serde_json::to_string(e).map_err(|je| {
                    std::io::Error::new(std::io::ErrorKind::InvalidData, je.to_string())
                })?;
                tmp.write_all(line.as_bytes())?;
                tmp.write_all(b"\n")?;
            }
            tmp.flush()?;
        }

        fs::rename(&tmp_path, &self.path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    // ── helpers ───────────────────────────────────────────────────────────────

    fn make_rb() -> (tempfile::TempDir, RingBuffer) {
        let dir = tempdir().unwrap();
        let rb = RingBuffer::new(dir.path().join("errors.jsonl"));
        (dir, rb)
    }

    fn entry(category: &str, msg: &str) -> ErrorEntry {
        ErrorEntry::new(Severity::Error, category, msg)
    }

    // ── basic append & read ───────────────────────────────────────────────────

    #[test]
    fn read_empty_when_no_file() {
        let (_dir, rb) = make_rb();
        let entries = rb.read(None).unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn append_and_read_single_entry() {
        let (_dir, rb) = make_rb();
        rb.append(entry("git_error", "network timeout")).unwrap();
        let entries = rb.read(None).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].category, "git_error");
        assert_eq!(entries[0].message, "network timeout");
    }

    #[test]
    fn append_preserves_order() {
        let (_dir, rb) = make_rb();
        for i in 0..5u32 {
            rb.append(entry("io_error", &format!("msg {i}"))).unwrap();
        }
        let entries = rb.read(None).unwrap();
        assert_eq!(entries.len(), 5);
        for (i, e) in entries.iter().enumerate() {
            assert_eq!(e.message, format!("msg {i}"));
        }
    }

    // ── rotation ──────────────────────────────────────────────────────────────

    #[test]
    fn rotation_drops_oldest_at_capacity_plus_one() {
        let (_dir, rb) = make_rb();
        // Fill to capacity + 1 (31 entries).
        for i in 0..=30u32 {
            rb.append(entry("push_conflict", &format!("msg {i}")))
                .unwrap();
        }
        let entries = rb.read(None).unwrap();
        assert_eq!(entries.len(), 30, "buffer must not exceed 30 entries");
        // Entry 0 ("msg 0") was dropped; oldest surviving is "msg 1".
        assert_eq!(entries[0].message, "msg 1");
        assert_eq!(entries[29].message, "msg 30");
    }

    #[test]
    fn rotation_handles_many_appends() {
        let (_dir, rb) = make_rb();
        for i in 0..100u32 {
            rb.append(entry("disk_full", &format!("msg {i}"))).unwrap();
        }
        let entries = rb.read(None).unwrap();
        assert_eq!(entries.len(), 30);
        // Oldest surviving is msg 70, newest is msg 99.
        assert_eq!(entries[0].message, "msg 70");
        assert_eq!(entries[29].message, "msg 99");
    }

    // ── read with limit ───────────────────────────────────────────────────────

    #[test]
    fn read_limit_returns_last_n() {
        let (_dir, rb) = make_rb();
        for i in 0..10u32 {
            rb.append(entry("malformed_line", &format!("msg {i}")))
                .unwrap();
        }
        let last5 = rb.read(Some(5)).unwrap();
        assert_eq!(last5.len(), 5);
        assert_eq!(last5[0].message, "msg 5");
        assert_eq!(last5[4].message, "msg 9");
    }

    #[test]
    fn read_limit_larger_than_total_returns_all() {
        let (_dir, rb) = make_rb();
        for i in 0..3u32 {
            rb.append(entry("io_error", &format!("msg {i}"))).unwrap();
        }
        let result = rb.read(Some(100)).unwrap();
        assert_eq!(result.len(), 3);
    }

    #[test]
    fn read_limit_zero_returns_empty() {
        let (_dir, rb) = make_rb();
        rb.append(entry("git_error", "msg 0")).unwrap();
        let result = rb.read(Some(0)).unwrap();
        assert!(result.is_empty());
    }

    // ── clear ─────────────────────────────────────────────────────────────────

    #[test]
    fn clear_removes_all_entries() {
        let (_dir, rb) = make_rb();
        for i in 0..5u32 {
            rb.append(entry("canonicalization_error", &format!("msg {i}")))
                .unwrap();
        }
        rb.clear().unwrap();
        let entries = rb.read(None).unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn clear_on_nonexistent_file_succeeds() {
        let (_dir, rb) = make_rb();
        // Should not error even if file was never created.
        rb.clear().unwrap();
        assert!(rb.read(None).unwrap().is_empty());
    }

    // ── all 7 error categories ────────────────────────────────────────────────

    #[test]
    fn all_seven_categories_round_trip() {
        let (_dir, rb) = make_rb();
        let categories = [
            "push_conflict",
            "malformed_line",
            "prefix_mismatch",
            "canonicalization_error",
            "git_error",
            "io_error",
            "disk_full",
        ];
        for cat in &categories {
            rb.append(entry(cat, &format!("test {cat}"))).unwrap();
        }
        let entries = rb.read(None).unwrap();
        assert_eq!(entries.len(), 7);
        for (i, cat) in categories.iter().enumerate() {
            assert_eq!(entries[i].category, *cat);
        }
    }

    // ── optional fields ───────────────────────────────────────────────────────

    #[test]
    fn with_file_and_detail_round_trip() {
        let (_dir, rb) = make_rb();
        let e = ErrorEntry::new(Severity::Warning, "prefix_mismatch", "entries differ")
            .with_file("pi/sessions/--home-foo--/session.jsonl")
            .with_detail("remote: abc123, local: def456");
        rb.append(e).unwrap();

        let entries = rb.read(None).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0].file.as_deref(),
            Some("pi/sessions/--home-foo--/session.jsonl")
        );
        assert_eq!(
            entries[0].detail.as_deref(),
            Some("remote: abc123, local: def456")
        );
        assert_eq!(entries[0].severity, Severity::Warning);
    }

    #[test]
    fn absent_optional_fields_omitted_in_json() {
        let (_dir, rb) = make_rb();
        rb.append(entry("io_error", "write failed")).unwrap();

        // Read raw JSONL and confirm file/detail keys are absent.
        let raw = std::fs::read_to_string(rb.path.clone()).unwrap();
        assert!(!raw.contains("\"file\""));
        assert!(!raw.contains("\"detail\""));
    }

    // ── severity serialization ────────────────────────────────────────────────

    #[test]
    fn severity_serializes_to_lowercase() {
        let e = ErrorEntry::new(Severity::Info, "git_error", "msg");
        let json = serde_json::to_string(&e).unwrap();
        assert!(json.contains("\"info\""));
    }

    // ── default_path ──────────────────────────────────────────────────────────

    #[test]
    fn default_path_ends_with_expected_suffix() {
        let path = RingBuffer::default_path();
        assert!(path.ends_with("chronicle/errors.jsonl"));
    }

    // ── atomicity: parent dir created automatically ───────────────────────────

    #[test]
    fn append_creates_parent_directory() {
        let dir = tempdir().unwrap();
        // Nested path whose intermediate directories don't exist yet.
        let rb = RingBuffer::new(dir.path().join("a").join("b").join("errors.jsonl"));
        rb.append(entry("git_error", "test")).unwrap();
        assert_eq!(rb.read(None).unwrap().len(), 1);
    }
}
