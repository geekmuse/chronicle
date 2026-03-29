// Git staging and commit message formatting.
//
// US-012: stage files via git2 index, format sync and import commit messages,
// and commit staged changes with the canonical Chronicle committer identity.
#![allow(dead_code)]

use std::path::Path;

use chrono::{DateTime, Utc};

use super::{GitError, RepoManager};

// The canonical git empty-tree SHA-1 OID.
// This is the hash git produces when writing an index with no entries.
const EMPTY_TREE_OID: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

/// The Chronicle committer e-mail address used for every commit.
const COMMITTER_EMAIL: &str = "chronicle@local";

// ---------------------------------------------------------------------------
// Commit message types
// ---------------------------------------------------------------------------

/// Statistics used to build the body of a sync commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyncSummary {
    /// Files that did not previously exist in the repo.
    pub new_files: usize,
    /// Files that already existed but have been appended to.
    pub modified_files: usize,
    /// Total changed files attributed to the pi agent.
    pub pi_total: usize,
    /// Total changed files attributed to the claude agent.
    pub claude_total: usize,
}

// ---------------------------------------------------------------------------
// Commit message formatting
// ---------------------------------------------------------------------------

/// Format a sync commit message.
///
/// ```text
/// sync: cheerful-chinchilla @ 2026-03-28T15:30:00Z
///
/// +3 files, ~12 files (pi: 8, claude: 7)
/// ```
///
/// Where `+` = new files, `~` = modified (appended) files.
#[must_use]
pub fn format_sync_message(
    machine: &str,
    timestamp: &DateTime<Utc>,
    summary: &SyncSummary,
) -> String {
    let subject = format!(
        "sync: {} @ {}",
        machine,
        timestamp.format("%Y-%m-%dT%H:%M:%SZ")
    );
    let body = format!(
        "+{} files, ~{} files (pi: {}, claude: {})",
        summary.new_files, summary.modified_files, summary.pi_total, summary.claude_total
    );
    format!("{subject}\n\n{body}\n")
}

/// Format an import commit message.
///
/// ```text
/// import: pi sessions (cheerful-chinchilla)
///
/// Added 633 session files
/// ```
#[must_use]
pub fn format_import_message(agent: &str, machine: &str, count: usize) -> String {
    let subject = format!("import: {agent} sessions ({machine})");
    let suffix = if count == 1 { "" } else { "s" };
    let body = format!("Added {count} session file{suffix}");
    format!("{subject}\n\n{body}\n")
}

// ---------------------------------------------------------------------------
// RepoManager — staging and committing
// ---------------------------------------------------------------------------

impl RepoManager {
    /// Stage (add to the git index) the given repo-relative file paths.
    ///
    /// Each entry in `repo_relative_paths` must be a path **relative to the
    /// repository root** — the same root returned by [`RepoManager::repo_path`].
    ///
    /// Call [`commit_if_staged`](RepoManager::commit_if_staged) after staging
    /// to create the commit.
    ///
    /// # Errors
    ///
    /// Returns [`GitError::Git2`] if a path cannot be found in the working
    /// tree or added to the index.
    pub fn stage_files(&self, repo_relative_paths: &[&Path]) -> Result<(), GitError> {
        let mut index = self.repo.index()?;
        for path in repo_relative_paths {
            index.add_path(path)?;
        }
        index.write()?;
        Ok(())
    }

    /// Create a commit containing all currently staged changes.
    ///
    /// - Both committer and author are set to `committer_name <chronicle@local>`.
    /// - Returns `Some(Oid)` containing the new commit's OID on success.
    /// - Returns `None` when there are no staged changes (idempotent: no
    ///   commit is created and the repository state is unchanged).
    ///
    /// Handles the initial-commit case automatically (no parent, no HEAD).
    ///
    /// # Errors
    ///
    /// Returns [`GitError::Git2`] if any git2 operation fails.
    pub fn commit_if_staged(
        &self,
        message: &str,
        committer_name: &str,
    ) -> Result<Option<git2::Oid>, GitError> {
        // Write the current index state as a tree.
        let mut index = self.repo.index()?;
        let tree_oid = index.write_tree()?;

        // Resolve HEAD (None for an unborn branch / initial commit).
        let (head_tree_oid, parent_commit) = match self.repo.head() {
            Ok(head_ref) => {
                let commit = head_ref.peel_to_commit()?;
                let tree_id = commit.tree_id();
                (Some(tree_id), Some(commit))
            }
            Err(e)
                if e.code() == git2::ErrorCode::UnbornBranch
                    || e.code() == git2::ErrorCode::NotFound =>
            {
                (None, None)
            }
            Err(e) => return Err(GitError::Git2(e)),
        };

        // Determine whether anything is actually staged.
        let should_commit = match head_tree_oid {
            // Non-initial commit: staged if index tree differs from HEAD tree.
            Some(ht_oid) => ht_oid != tree_oid,
            // Initial commit: staged if index is non-empty (tree ≠ empty-tree OID).
            None => {
                let empty_oid = git2::Oid::from_str(EMPTY_TREE_OID)?;
                tree_oid != empty_oid
            }
        };

        if !should_commit {
            return Ok(None);
        }

        let tree = self.repo.find_tree(tree_oid)?;
        let now = Utc::now();
        let time = git2::Time::new(now.timestamp(), 0);
        let sig = git2::Signature::new(committer_name, COMMITTER_EMAIL, &time)?;

        let parents: Vec<git2::Commit> = parent_commit.into_iter().collect();
        let parent_refs: Vec<&git2::Commit> = parents.iter().collect();

        let oid = self
            .repo
            .commit(Some("HEAD"), &sig, &sig, message, &tree, &parent_refs)?;

        Ok(Some(oid))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::RepoManager;
    use std::fs;
    use tempfile::TempDir;

    fn tmp() -> TempDir {
        tempfile::tempdir().expect("create tempdir")
    }

    fn make_manager(dir: &TempDir) -> RepoManager {
        let path = dir.path().join("repo");
        RepoManager::init_or_open(&path, None).expect("init repo")
    }

    // -----------------------------------------------------------------------
    // format_sync_message
    // -----------------------------------------------------------------------

    #[test]
    fn sync_message_subject_format() {
        use chrono::TimeZone as _;
        let ts = Utc.with_ymd_and_hms(2026, 3, 28, 15, 30, 0).unwrap();
        let summary = SyncSummary {
            new_files: 3,
            modified_files: 12,
            pi_total: 8,
            claude_total: 7,
        };
        let msg = format_sync_message("cheerful-chinchilla", &ts, &summary);
        assert!(
            msg.starts_with("sync: cheerful-chinchilla @ 2026-03-28T15:30:00Z"),
            "subject must match spec format; got: {msg:?}"
        );
    }

    #[test]
    fn sync_message_body_format() {
        use chrono::TimeZone as _;
        let ts = Utc.with_ymd_and_hms(2026, 3, 28, 15, 30, 0).unwrap();
        let summary = SyncSummary {
            new_files: 3,
            modified_files: 12,
            pi_total: 8,
            claude_total: 7,
        };
        let msg = format_sync_message("cheerful-chinchilla", &ts, &summary);
        assert!(
            msg.contains("+3 files, ~12 files (pi: 8, claude: 7)"),
            "body must match spec format; got: {msg:?}"
        );
    }

    #[test]
    fn sync_message_blank_line_between_subject_and_body() {
        use chrono::TimeZone as _;
        let ts = Utc.with_ymd_and_hms(2026, 3, 28, 15, 30, 0).unwrap();
        let summary = SyncSummary {
            new_files: 0,
            modified_files: 0,
            pi_total: 0,
            claude_total: 0,
        };
        let msg = format_sync_message("m", &ts, &summary);
        let lines: Vec<&str> = msg.lines().collect();
        // line 0 = subject, line 1 = blank, line 2 = body
        assert!(lines.len() >= 3, "must have at least 3 lines");
        assert!(
            lines[1].is_empty(),
            "line 1 must be blank (git commit message convention)"
        );
    }

    #[test]
    fn sync_message_zero_counts() {
        use chrono::TimeZone as _;
        let ts = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        let summary = SyncSummary {
            new_files: 0,
            modified_files: 0,
            pi_total: 0,
            claude_total: 0,
        };
        let msg = format_sync_message("bold-barracuda", &ts, &summary);
        assert!(msg.contains("+0 files, ~0 files (pi: 0, claude: 0)"));
    }

    // -----------------------------------------------------------------------
    // format_import_message
    // -----------------------------------------------------------------------

    #[test]
    fn import_message_subject_format() {
        let msg = format_import_message("pi", "cheerful-chinchilla", 633);
        assert!(
            msg.starts_with("import: pi sessions (cheerful-chinchilla)"),
            "subject must match spec format; got: {msg:?}"
        );
    }

    #[test]
    fn import_message_body_plural() {
        let msg = format_import_message("pi", "cheerful-chinchilla", 633);
        assert!(
            msg.contains("Added 633 session files"),
            "body must state file count (plural); got: {msg:?}"
        );
    }

    #[test]
    fn import_message_body_singular() {
        let msg = format_import_message("claude", "gentle-gecko", 1);
        assert!(
            msg.contains("Added 1 session file"),
            "body must use singular for count=1; got: {msg:?}"
        );
        assert!(
            !msg.contains("session files"),
            "must not use plural for count=1"
        );
    }

    #[test]
    fn import_message_blank_line_between_subject_and_body() {
        let msg = format_import_message("pi", "m", 10);
        let lines: Vec<&str> = msg.lines().collect();
        assert!(lines.len() >= 3);
        assert!(
            lines[1].is_empty(),
            "line 1 must be blank (git commit message convention)"
        );
    }

    #[test]
    fn import_message_claude_agent() {
        let msg = format_import_message("claude", "bold-barracuda", 42);
        assert!(msg.starts_with("import: claude sessions (bold-barracuda)"));
        assert!(msg.contains("Added 42 session files"));
    }

    // -----------------------------------------------------------------------
    // stage_files + commit_if_staged
    // -----------------------------------------------------------------------

    #[test]
    fn commit_if_staged_returns_none_when_nothing_staged() {
        let dir = tmp();
        let manager = make_manager(&dir);
        let result = manager
            .commit_if_staged("test message", "test-machine")
            .expect("commit_if_staged must not error on empty repo");
        assert!(
            result.is_none(),
            "must return None when index is empty and no HEAD"
        );
    }

    #[test]
    fn stage_and_commit_creates_commit() {
        let dir = tmp();
        let manager = make_manager(&dir);
        let repo_path = manager.repo_path().to_path_buf();

        // Write a file to the working tree.
        let file_path = repo_path.join("test.jsonl");
        fs::write(&file_path, b"{\"type\":\"session\"}\n").unwrap();

        // Stage the file (repo-relative path).
        manager
            .stage_files(&[Path::new("test.jsonl")])
            .expect("stage_files");

        let oid = manager
            .commit_if_staged("test: initial commit", "happy-hedgehog")
            .expect("commit_if_staged");

        assert!(
            oid.is_some(),
            "must return Some(Oid) after committing a file"
        );
    }

    #[test]
    fn commit_sets_correct_committer_name() {
        let dir = tmp();
        let manager = make_manager(&dir);
        let repo_path = manager.repo_path().to_path_buf();

        fs::write(repo_path.join("a.jsonl"), b"line\n").unwrap();
        manager.stage_files(&[Path::new("a.jsonl")]).unwrap();

        let oid = manager
            .commit_if_staged("subject", "cheerful-chinchilla")
            .expect("commit")
            .expect("must be Some");

        let commit = manager.repository().find_commit(oid).unwrap();
        assert_eq!(commit.committer().name().unwrap(), "cheerful-chinchilla");
        assert_eq!(commit.committer().email().unwrap(), "chronicle@local");
        assert_eq!(commit.author().name().unwrap(), "cheerful-chinchilla");
        assert_eq!(commit.author().email().unwrap(), "chronicle@local");
    }

    #[test]
    fn commit_if_staged_idempotent_after_commit() {
        let dir = tmp();
        let manager = make_manager(&dir);
        let repo_path = manager.repo_path().to_path_buf();

        fs::write(repo_path.join("b.jsonl"), b"line\n").unwrap();
        manager.stage_files(&[Path::new("b.jsonl")]).unwrap();
        manager
            .commit_if_staged("first", "m")
            .unwrap()
            .expect("first commit must succeed");

        // Call commit_if_staged again without staging new changes.
        let result = manager
            .commit_if_staged("second", "m")
            .expect("must not error");
        assert!(
            result.is_none(),
            "must return None when index matches HEAD (no new staged changes)"
        );
    }

    #[test]
    fn second_commit_with_new_file_creates_commit() {
        let dir = tmp();
        let manager = make_manager(&dir);
        let repo_path = manager.repo_path().to_path_buf();

        // First commit.
        fs::write(repo_path.join("c.jsonl"), b"line\n").unwrap();
        manager.stage_files(&[Path::new("c.jsonl")]).unwrap();
        manager
            .commit_if_staged("first", "m")
            .unwrap()
            .expect("first commit");

        // Add a second file and commit again.
        fs::write(repo_path.join("d.jsonl"), b"line2\n").unwrap();
        manager.stage_files(&[Path::new("d.jsonl")]).unwrap();
        let oid2 = manager
            .commit_if_staged("second", "m")
            .unwrap()
            .expect("second commit must succeed");

        // Second commit must have the first commit as parent.
        let commit2 = manager.repository().find_commit(oid2).unwrap();
        assert_eq!(
            commit2.parent_count(),
            1,
            "second commit must have exactly one parent"
        );
    }

    #[test]
    fn sync_commit_message_used_verbatim() {
        use chrono::TimeZone as _;
        let dir = tmp();
        let manager = make_manager(&dir);
        let repo_path = manager.repo_path().to_path_buf();

        let ts = Utc.with_ymd_and_hms(2026, 3, 28, 15, 30, 0).unwrap();
        let summary = SyncSummary {
            new_files: 1,
            modified_files: 0,
            pi_total: 1,
            claude_total: 0,
        };
        let msg = format_sync_message("happy-hedgehog", &ts, &summary);

        fs::write(repo_path.join("e.jsonl"), b"content\n").unwrap();
        manager.stage_files(&[Path::new("e.jsonl")]).unwrap();
        let oid = manager
            .commit_if_staged(&msg, "happy-hedgehog")
            .unwrap()
            .expect("commit");

        let commit = manager.repository().find_commit(oid).unwrap();
        assert_eq!(commit.message().unwrap(), msg.as_str());
    }

    #[test]
    fn import_commit_message_used_verbatim() {
        let dir = tmp();
        let manager = make_manager(&dir);
        let repo_path = manager.repo_path().to_path_buf();

        let msg = format_import_message("pi", "cheerful-chinchilla", 5);

        fs::write(repo_path.join("f.jsonl"), b"content\n").unwrap();
        manager.stage_files(&[Path::new("f.jsonl")]).unwrap();
        let oid = manager
            .commit_if_staged(&msg, "cheerful-chinchilla")
            .unwrap()
            .expect("commit");

        let commit = manager.repository().find_commit(oid).unwrap();
        assert_eq!(commit.message().unwrap(), msg.as_str());
    }
}
