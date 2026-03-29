// Git repository management: init/open, working tree structure, manifest,
// fetch/push with retry, and commit formatting.
// US-010: init/open, working tree, manifest.
// US-011: fetch/push with exponential-backoff retry (fetch_push.rs).
// US-012: staging and commit formatting.
#![allow(dead_code)]

mod commit;
mod fetch_push;
#[allow(unused_imports)]
pub use commit::{format_import_message, format_sync_message, SyncSummary};
#[allow(unused_imports)]
pub use fetch_push::is_network_error;
#[allow(unused_imports)]
pub(crate) use fetch_push::run_push_retry;

use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Errors produced by git repository operations.
#[derive(Debug, Error)]
pub enum GitError {
    /// A git2 library error (network, auth, corrupt repo, etc.).
    #[error("git2 error: {0}")]
    Git2(#[from] git2::Error),

    /// A filesystem I/O error during working-tree or manifest operations.
    #[error("IO error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// `manifest.json` could not be parsed or serialized.
    #[error("manifest error: {0}")]
    Manifest(String),

    /// Push was rejected by the remote because it has advanced past our local tip.
    #[error("push rejected on '{refname}': {message}")]
    PushRejected {
        /// The git reference that was rejected.
        refname: String,
        /// Rejection message from the remote.
        message: String,
    },

    /// All push retries were exhausted without success.
    #[error("push failed after {attempts} attempt(s); remote repeatedly rejected")]
    PushExhausted {
        /// Total number of push attempts made (initial + retries).
        attempts: usize,
    },
}

// ---------------------------------------------------------------------------
// Retry constants
// ---------------------------------------------------------------------------

/// Backoff delays (in seconds) before each push retry attempt.
///
/// Index `i` is the delay before retry `i+1`.  The first retry (`i = 0`) is
/// immediate (0 s); subsequent retries wait 5 s and 25 s respectively.
pub const PUSH_BACKOFF_SECS: [u64; 3] = [0, 5, 25];

/// Maximum number of push retries (in addition to the initial attempt).
pub const PUSH_MAX_RETRIES: usize = PUSH_BACKOFF_SECS.len();

impl GitError {
    fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }
}

// ---------------------------------------------------------------------------
// Manifest types
// ---------------------------------------------------------------------------

/// Per-machine record stored in `manifest.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MachineEntry {
    /// ISO-8601 timestamp of the first time this machine ran Chronicle.
    pub first_seen: DateTime<Utc>,

    /// ISO-8601 timestamp of the most recent successful sync.
    /// Absent on the initial entry (before the first sync completes).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_sync: Option<DateTime<Utc>>,

    /// Home path stored as the canonical token `"{{SYNC_HOME}}"`.
    pub home_path: String,

    /// OS identifier: `"macos"` or `"linux"`.
    pub os: String,
}

/// Root structure of `.chronicle/manifest.json`.
///
/// ```json
/// {
///   "version": 1,
///   "machines": {
///     "cheerful-chinchilla": {
///       "first_seen": "2026-03-28T10:00:00Z",
///       "last_sync": "2026-03-28T15:30:00Z",
///       "home_path": "{{SYNC_HOME}}",
///       "os": "macos"
///     }
///   }
/// }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    /// Schema version — always `1` for this release.
    pub version: u32,

    /// Machine registry: machine name → [`MachineEntry`].
    pub machines: HashMap<String, MachineEntry>,
}

impl Default for Manifest {
    fn default() -> Self {
        Self {
            version: 1,
            machines: HashMap::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// RepoManager
// ---------------------------------------------------------------------------

/// Manages the Chronicle git repository.
///
/// Wraps a [`git2::Repository`] and provides high-level operations for
/// initializing/opening the repo, laying out the canonical working-tree
/// structure, and reading/writing `manifest.json`.
pub struct RepoManager {
    repo: git2::Repository,
    /// Absolute path to the repository working tree root.
    repo_path: PathBuf,
}

impl RepoManager {
    /// Initialize a new git repository at `repo_path`, or open it if it
    /// already exists.
    ///
    /// - Parent directories are created automatically.
    /// - If `remote_url` is `Some` and non-empty the `origin` remote is
    ///   configured: added on first init, URL updated on re-open.
    ///
    /// # Errors
    ///
    /// Returns [`GitError`] if git2 operations fail or if parent directories
    /// cannot be created.
    pub fn init_or_open(repo_path: &Path, remote_url: Option<&str>) -> Result<Self, GitError> {
        let repo = open_or_init(repo_path)?;
        let manager = Self {
            repo,
            repo_path: repo_path.to_path_buf(),
        };
        if let Some(url) = remote_url.filter(|u| !u.is_empty()) {
            manager.set_remote("origin", url)?;
        }
        Ok(manager)
    }

    /// Returns a reference to the underlying [`git2::Repository`].
    #[must_use]
    pub fn repository(&self) -> &git2::Repository {
        &self.repo
    }

    /// Returns the repository working-tree root path.
    #[must_use]
    pub fn repo_path(&self) -> &Path {
        &self.repo_path
    }

    /// Ensure the canonical working-tree directory structure exists:
    ///
    /// ```text
    /// <repo_root>/
    /// ├── pi/sessions/           ← .gitkeep so the dir is tracked when empty
    /// ├── claude/projects/       ← .gitkeep so the dir is tracked when empty
    /// └── .chronicle/            ← manifest.json placed here by ensure_manifest
    /// ```
    ///
    /// Safe to call on an existing repo — existing files are never overwritten.
    ///
    /// # Errors
    ///
    /// Returns [`GitError::Io`] if a directory or `.gitkeep` file cannot be
    /// created.
    pub fn ensure_working_tree(&self) -> Result<(), GitError> {
        let root = &self.repo_path;

        // Directories that need a .gitkeep so they are tracked when empty.
        let tracked_dirs = [
            root.join("pi").join("sessions"),
            root.join("claude").join("projects"),
        ];
        for dir in &tracked_dirs {
            fs::create_dir_all(dir).map_err(|e| GitError::io(dir, e))?;
            let gitkeep = dir.join(".gitkeep");
            if !gitkeep.exists() {
                fs::write(&gitkeep, b"").map_err(|e| GitError::io(&gitkeep, e))?;
            }
        }

        // .chronicle/ directory — manifest.json is written here by ensure_manifest.
        let chronicle_dir = root.join(".chronicle");
        fs::create_dir_all(&chronicle_dir).map_err(|e| GitError::io(&chronicle_dir, e))?;

        Ok(())
    }

    /// Read `.chronicle/manifest.json`.
    ///
    /// Returns the default (empty) [`Manifest`] if the file does not yet exist.
    ///
    /// # Errors
    ///
    /// Returns [`GitError::Manifest`] if the file exists but cannot be parsed.
    /// Returns [`GitError::Io`] if the file exists but cannot be read.
    pub fn read_manifest(&self) -> Result<Manifest, GitError> {
        let path = self.manifest_path();
        if !path.exists() {
            return Ok(Manifest::default());
        }
        let content = fs::read_to_string(&path).map_err(|e| GitError::io(&path, e))?;
        serde_json::from_str::<Manifest>(&content).map_err(|e| GitError::Manifest(e.to_string()))
    }

    /// Write `manifest` to `.chronicle/manifest.json` (pretty-printed).
    ///
    /// Creates `.chronicle/` if it does not exist.
    ///
    /// # Errors
    ///
    /// Returns [`GitError`] if serialization or the file write fails.
    pub fn write_manifest(&self, manifest: &Manifest) -> Result<(), GitError> {
        let chronicle_dir = self.repo_path.join(".chronicle");
        fs::create_dir_all(&chronicle_dir).map_err(|e| GitError::io(&chronicle_dir, e))?;
        let path = self.manifest_path();
        let content = serde_json::to_string_pretty(manifest)
            .map_err(|e| GitError::Manifest(e.to_string()))?;
        fs::write(&path, content.as_bytes()).map_err(|e| GitError::io(&path, e))?;
        Ok(())
    }

    /// Ensure `.chronicle/manifest.json` exists; write the default if not.
    ///
    /// If the file already exists it is **not** overwritten — use
    /// [`write_manifest`](Self::write_manifest) to update it.
    ///
    /// # Errors
    ///
    /// Returns [`GitError`] if the manifest cannot be read or written.
    pub fn ensure_manifest(&self) -> Result<(), GitError> {
        if !self.manifest_path().exists() {
            self.write_manifest(&Manifest::default())?;
        }
        Ok(())
    }

    // ------------------------------------------------------------------
    // Private helpers
    // ------------------------------------------------------------------

    fn manifest_path(&self) -> PathBuf {
        self.repo_path.join(".chronicle").join("manifest.json")
    }

    /// Configure the named remote: add it if missing, update the URL if it
    /// already exists.
    fn set_remote(&self, name: &str, url: &str) -> Result<(), GitError> {
        match self.repo.find_remote(name) {
            Ok(_) => {
                self.repo.remote_set_url(name, url)?;
            }
            Err(e) if e.code() == git2::ErrorCode::NotFound => {
                self.repo.remote(name, url)?;
            }
            Err(e) => return Err(GitError::Git2(e)),
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Module-level helpers
// ---------------------------------------------------------------------------

/// Open `path` as a git repository if it is one; otherwise initialize a new
/// repository there.
///
/// Parent directories are created when `init` is triggered.
fn open_or_init(path: &Path) -> Result<git2::Repository, GitError> {
    match git2::Repository::open(path) {
        Ok(repo) => Ok(repo),
        Err(e) if e.code() == git2::ErrorCode::NotFound => {
            // Path does not exist or is not a git repository — initialize.
            fs::create_dir_all(path).map_err(|io_err| GitError::io(path, io_err))?;
            Ok(git2::Repository::init(path)?)
        }
        Err(e) => Err(GitError::Git2(e)),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn tmp() -> TempDir {
        tempfile::tempdir().expect("create tempdir")
    }

    // -----------------------------------------------------------------------
    // init_or_open — basic init
    // -----------------------------------------------------------------------

    #[test]
    fn init_creates_repo_at_path() {
        let dir = tmp();
        let repo_path = dir.path().join("repo");
        let manager = RepoManager::init_or_open(&repo_path, None).expect("init should succeed");
        assert!(repo_path.join(".git").exists(), ".git directory must exist");
        assert_eq!(manager.repo_path(), repo_path.as_path());
    }

    #[test]
    fn init_creates_parent_directories() {
        let dir = tmp();
        let repo_path = dir.path().join("a").join("b").join("repo");
        RepoManager::init_or_open(&repo_path, None).expect("init with nested path");
        assert!(repo_path.join(".git").exists());
    }

    // -----------------------------------------------------------------------
    // init_or_open — open existing repo
    // -----------------------------------------------------------------------

    #[test]
    fn open_existing_repo_without_reinit() {
        let dir = tmp();
        let repo_path = dir.path().join("repo");
        // First init.
        RepoManager::init_or_open(&repo_path, None).expect("first init");
        // Write a sentinel file to verify the working tree is preserved.
        fs::write(repo_path.join("sentinel.txt"), b"hello").unwrap();
        // Re-open — must not wipe the working tree.
        let manager = RepoManager::init_or_open(&repo_path, None).expect("re-open must succeed");
        assert!(
            manager.repo_path().join("sentinel.txt").exists(),
            "working tree must be preserved on re-open"
        );
    }

    #[test]
    fn open_existing_returns_correct_path() {
        let dir = tmp();
        let repo_path = dir.path().join("repo");
        RepoManager::init_or_open(&repo_path, None).expect("init");
        let manager = RepoManager::init_or_open(&repo_path, None).expect("re-open");
        assert_eq!(manager.repo_path(), repo_path.as_path());
    }

    // -----------------------------------------------------------------------
    // Remote URL
    // -----------------------------------------------------------------------

    #[test]
    fn remote_set_on_init() {
        let dir = tmp();
        let repo_path = dir.path().join("repo");
        let url = "https://example.com/chronicle.git";
        let manager = RepoManager::init_or_open(&repo_path, Some(url)).expect("init with remote");
        let remote = manager
            .repository()
            .find_remote("origin")
            .expect("origin remote must exist");
        assert_eq!(remote.url().unwrap(), url);
    }

    #[test]
    fn remote_url_updated_on_reopen() {
        let dir = tmp();
        let repo_path = dir.path().join("repo");
        let url1 = "https://example.com/old.git";
        let url2 = "https://example.com/new.git";
        RepoManager::init_or_open(&repo_path, Some(url1)).expect("first init");
        let manager = RepoManager::init_or_open(&repo_path, Some(url2)).expect("reopen");
        let remote = manager
            .repository()
            .find_remote("origin")
            .expect("origin remote");
        assert_eq!(remote.url().unwrap(), url2, "URL must be updated");
    }

    #[test]
    fn no_remote_when_url_is_empty_string() {
        let dir = tmp();
        let repo_path = dir.path().join("repo");
        let manager = RepoManager::init_or_open(&repo_path, Some("")).expect("init with empty url");
        assert!(
            manager.repository().find_remote("origin").is_err(),
            "no origin remote for empty URL"
        );
    }

    #[test]
    fn no_remote_when_url_is_none() {
        let dir = tmp();
        let repo_path = dir.path().join("repo");
        let manager = RepoManager::init_or_open(&repo_path, None).expect("init");
        assert!(
            manager.repository().find_remote("origin").is_err(),
            "no origin remote when url=None"
        );
    }

    // -----------------------------------------------------------------------
    // ensure_working_tree
    // -----------------------------------------------------------------------

    #[test]
    fn working_tree_directories_created() {
        let dir = tmp();
        let repo_path = dir.path().join("repo");
        let manager = RepoManager::init_or_open(&repo_path, None).expect("init");
        manager.ensure_working_tree().expect("ensure_working_tree");

        assert!(
            repo_path.join("pi").join("sessions").is_dir(),
            "pi/sessions/ must exist"
        );
        assert!(
            repo_path.join("claude").join("projects").is_dir(),
            "claude/projects/ must exist"
        );
        assert!(
            repo_path.join(".chronicle").is_dir(),
            ".chronicle/ must exist"
        );
    }

    #[test]
    fn working_tree_gitkeep_files_created() {
        let dir = tmp();
        let repo_path = dir.path().join("repo");
        let manager = RepoManager::init_or_open(&repo_path, None).expect("init");
        manager.ensure_working_tree().expect("ensure_working_tree");

        assert!(
            repo_path
                .join("pi")
                .join("sessions")
                .join(".gitkeep")
                .exists(),
            "pi/sessions/.gitkeep must exist"
        );
        assert!(
            repo_path
                .join("claude")
                .join("projects")
                .join(".gitkeep")
                .exists(),
            "claude/projects/.gitkeep must exist"
        );
    }

    #[test]
    fn ensure_working_tree_is_idempotent() {
        let dir = tmp();
        let repo_path = dir.path().join("repo");
        let manager = RepoManager::init_or_open(&repo_path, None).expect("init");
        manager.ensure_working_tree().expect("first call");
        manager
            .ensure_working_tree()
            .expect("second call must not fail");
    }

    // -----------------------------------------------------------------------
    // manifest
    // -----------------------------------------------------------------------

    #[test]
    fn ensure_manifest_creates_default_file() {
        let dir = tmp();
        let repo_path = dir.path().join("repo");
        let manager = RepoManager::init_or_open(&repo_path, None).expect("init");
        manager.ensure_working_tree().expect("working tree");
        manager.ensure_manifest().expect("ensure_manifest");

        let manifest_path = repo_path.join(".chronicle").join("manifest.json");
        assert!(manifest_path.exists(), "manifest.json must be created");

        let manifest = manager.read_manifest().expect("read_manifest");
        assert_eq!(manifest.version, 1, "version must be 1");
        assert!(manifest.machines.is_empty(), "machines map must be empty");
    }

    #[test]
    fn ensure_manifest_does_not_overwrite_existing() {
        let dir = tmp();
        let repo_path = dir.path().join("repo");
        let manager = RepoManager::init_or_open(&repo_path, None).expect("init");
        manager.ensure_working_tree().expect("working tree");
        manager.ensure_manifest().expect("first ensure");

        // Insert a machine entry into the manifest.
        let mut manifest = manager.read_manifest().unwrap();
        manifest.machines.insert(
            "cheerful-chinchilla".to_owned(),
            MachineEntry {
                first_seen: Utc::now(),
                last_sync: None,
                home_path: "{{SYNC_HOME}}".to_owned(),
                os: "macos".to_owned(),
            },
        );
        manager.write_manifest(&manifest).unwrap();

        // Second ensure_manifest must NOT overwrite the written manifest.
        manager.ensure_manifest().expect("second ensure");
        let loaded = manager.read_manifest().unwrap();
        assert!(
            loaded.machines.contains_key("cheerful-chinchilla"),
            "existing machine entry must be preserved"
        );
    }

    #[test]
    fn manifest_round_trip_with_machine_entry() {
        let dir = tmp();
        let repo_path = dir.path().join("repo");
        let manager = RepoManager::init_or_open(&repo_path, None).expect("init");

        let mut manifest = Manifest::default();
        manifest.machines.insert(
            "happy-hedgehog".to_owned(),
            MachineEntry {
                first_seen: DateTime::parse_from_rfc3339("2026-03-28T10:00:00Z")
                    .unwrap()
                    .with_timezone(&Utc),
                last_sync: Some(
                    DateTime::parse_from_rfc3339("2026-03-28T15:30:00Z")
                        .unwrap()
                        .with_timezone(&Utc),
                ),
                home_path: "{{SYNC_HOME}}".to_owned(),
                os: "linux".to_owned(),
            },
        );
        manager.write_manifest(&manifest).expect("write");

        let loaded = manager.read_manifest().expect("read");
        assert_eq!(loaded.version, 1);
        let entry = loaded
            .machines
            .get("happy-hedgehog")
            .expect("machine entry must be present");
        assert_eq!(entry.home_path, "{{SYNC_HOME}}");
        assert_eq!(entry.os, "linux");
        assert!(
            entry.last_sync.is_some(),
            "last_sync must survive round-trip"
        );
    }

    #[test]
    fn read_manifest_returns_default_when_file_missing() {
        let dir = tmp();
        let repo_path = dir.path().join("repo");
        let manager = RepoManager::init_or_open(&repo_path, None).expect("init");
        // No working tree setup, no manifest file.
        let manifest = manager
            .read_manifest()
            .expect("must return default, not an error");
        assert_eq!(manifest.version, 1);
        assert!(manifest.machines.is_empty());
    }

    #[test]
    fn last_sync_absent_from_json_when_none() {
        let dir = tmp();
        let repo_path = dir.path().join("repo");
        let manager = RepoManager::init_or_open(&repo_path, None).expect("init");

        let mut manifest = Manifest::default();
        manifest.machines.insert(
            "new-machine".to_owned(),
            MachineEntry {
                first_seen: Utc::now(),
                last_sync: None,
                home_path: "{{SYNC_HOME}}".to_owned(),
                os: "macos".to_owned(),
            },
        );
        manager.write_manifest(&manifest).expect("write");

        let raw = fs::read_to_string(manager.manifest_path()).unwrap();
        assert!(
            !raw.contains("last_sync"),
            "last_sync must not appear in JSON when None"
        );
    }
}
