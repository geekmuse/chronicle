// Grow-only set merge algorithm for JSONL session files (§5.2).
// Items here are consumed by the full sync pipeline (US-015/US-017).
//
// This module implements all five steps of §5.2:
//   1. Parse both files into entry sets
//   2. Prefix verification (§5.4): common entries must be byte-identical
//   3. Compute the set-union
//   4. Sort: header first, then ascending timestamp (stable, remote-first tie-break)
//   5. Serialise back to JSONL

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::merge::entry::{parse_entry, EntryKey, ParsedEntry};

// ── Public types ─────────────────────────────────────────────────────────────

/// A JSONL line that failed to parse and was skipped (§5.5).
#[derive(Debug, Clone)]
pub struct MalformedLine {
    /// Path of the file containing the malformed line.
    pub path: PathBuf,
    /// 1-based line number within the file.
    pub line_number: usize,
    /// A short snippet of the malformed content (at most 80 characters).
    pub snippet: String,
}

/// Interface for recording prefix-mismatch incidents.
///
/// The real implementation (US-008) will write entries to the error ring
/// buffer. Until that story is complete, callers may pass [`NullReporter`]
/// to discard reports.
pub trait ConflictReporter {
    /// Called when a common entry (same [`EntryKey`]) carries different raw
    /// content in the local and remote versions of a file (§5.4).
    ///
    /// # Arguments
    ///
    /// * `file`       — path to the file being merged.
    /// * `entry_key`  — composite key of the conflicting entry.
    /// * `local_raw`  — raw JSON from the local version.
    /// * `remote_raw` — raw JSON from the remote version (this version wins).
    fn report_prefix_mismatch(
        &self,
        file: &Path,
        entry_key: &EntryKey,
        local_raw: &str,
        remote_raw: &str,
    );
}

/// A no-op [`ConflictReporter`].
///
/// Used in tests and until US-008 wires in the real error ring buffer.
#[derive(Debug, Default)]
pub struct NullReporter;

impl ConflictReporter for NullReporter {
    fn report_prefix_mismatch(
        &self,
        _file: &Path,
        _entry_key: &EntryKey,
        _local_raw: &str,
        _remote_raw: &str,
    ) {
    }
}

/// A conflict record produced when a common entry carries different content
/// in the local and remote versions of a file (§5.4).
///
/// The remote version wins in all conflict cases.
#[derive(Debug, Clone)]
pub struct PrefixConflict {
    /// Path of the file being merged (as provided to [`merge_jsonl`]).
    pub file: PathBuf,
    /// Composite key of the conflicting entry.
    pub entry_key: EntryKey,
    /// Raw JSON from the **local** version of the entry.
    pub local_raw: String,
    /// Raw JSON from the **remote** version (the version that wins).
    pub remote_raw: String,
}

/// Output of a grow-only set merge operation.
#[derive(Debug)]
pub struct MergeOutput {
    /// The merged JSONL content, ready to write to disk.
    ///
    /// The session header is always first (if present), followed by all other
    /// entries sorted by timestamp ascending. Non-empty output ends with a
    /// trailing newline.
    pub content: String,
    /// Malformed lines that were skipped during parsing.
    pub malformed: Vec<MalformedLine>,
    /// Entries that were present in both local and remote with differing
    /// content; the remote version was used for each (§5.4).
    pub conflicts: Vec<PrefixConflict>,
}

// ── Internal types ────────────────────────────────────────────────────────────

/// Which file an entry originated from, used as a stable-sort tie-breaker.
///
/// Remote entries (`0`) sort before local entries (`1`) when timestamps are
/// equal, providing deterministic output (§5.2 step 4c).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Source {
    Remote = 0,
    Local = 1,
}

/// A parsed entry annotated with its source and original line position.
#[derive(Debug, Clone)]
struct TaggedEntry {
    entry: ParsedEntry,
    source: Source,
    /// 0-based position in the source file; used as a final tie-breaker to
    /// preserve intra-file ordering for entries with identical timestamps.
    original_index: usize,
}

// ── File parsing ──────────────────────────────────────────────────────────────

/// Parse every non-empty line of `content` into [`TaggedEntry`] values.
///
/// Malformed lines are skipped; a warning is logged and a [`MalformedLine`]
/// record is appended to `malformed` (§5.5).
fn parse_file(
    content: &str,
    path: &Path,
    source: Source,
    malformed: &mut Vec<MalformedLine>,
) -> Vec<TaggedEntry> {
    let mut entries = Vec::new();

    for (idx, line) in content.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }

        match parse_entry(line) {
            Some(parsed) => entries.push(TaggedEntry {
                entry: parsed,
                source,
                original_index: idx,
            }),
            None => {
                let snippet: String = line.chars().take(80).collect();
                tracing::warn!(
                    file = %path.display(),
                    line = idx + 1,
                    snippet = %snippet,
                    "skipping malformed JSONL line"
                );
                malformed.push(MalformedLine {
                    path: path.to_owned(),
                    line_number: idx + 1,
                    snippet,
                });
            }
        }
    }

    entries
}

// ── Merge ─────────────────────────────────────────────────────────────────────

/// Perform a grow-only set merge of two JSONL session files (§5.2).
///
/// # Arguments
///
/// * `remote_content` — content of the remote (committed) version.
/// * `remote_path`    — path used for warning messages about the remote file.
/// * `local_content`  — content of the local working-tree version.
/// * `local_path`     — path used for warning messages about the local file.
/// * `reporter`       — receives an incident record for each prefix mismatch
///   detected (§5.4). Pass [`NullReporter`] to discard reports until
///   US-008 wires in the real error ring buffer.
///
/// # Algorithm
///
/// 1. Both files are parsed into entry sets keyed by [`EntryKey`].
/// 2. **Prefix verification** (§5.4): for every entry present in **both**
///    files, the raw content is compared byte-by-byte. A mismatch triggers a
///    `tracing::warn!`, a call to `reporter`, and a [`PrefixConflict`] record
///    in the output. Remote version wins in every case.
/// 3. The union is computed: remote entries populate the set first; local-only
///    entries are added; the remote version is kept for all common entries
///    regardless of whether they matched.
/// 4. The merged set is sorted: session header first, then all other entries
///    by timestamp ascending. Ties are broken by source (remote before local)
///    then by original line position (strict total order).
/// 5. The sorted entries are serialised back to JSONL.
///
/// Malformed lines are skipped with a warning (§5.5).
#[must_use]
pub fn merge_jsonl(
    remote_content: &str,
    remote_path: &Path,
    local_content: &str,
    local_path: &Path,
    reporter: &dyn ConflictReporter,
) -> MergeOutput {
    let mut malformed = Vec::new();

    let remote_entries = parse_file(remote_content, remote_path, Source::Remote, &mut malformed);
    let local_entries = parse_file(local_content, local_path, Source::Local, &mut malformed);

    // Build the union keyed by EntryKey.
    // Remote entries populate first; remote wins on all key collisions.
    let mut key_map: HashMap<EntryKey, TaggedEntry> = HashMap::new();
    let mut conflicts: Vec<PrefixConflict> = Vec::new();

    for tagged in remote_entries {
        key_map.insert(tagged.entry.key.clone(), tagged);
    }

    // Step 2 (§5.2 / §5.4): prefix verification.
    // For every entry present in both files, verify byte-identical raw content.
    // Divergent entries trigger a warning, a reporter call, and a conflict
    // record. Remote version wins in every case.
    for tagged in local_entries {
        if let Some(remote_tagged) = key_map.get(&tagged.entry.key) {
            if remote_tagged.entry.raw != tagged.entry.raw {
                tracing::warn!(
                    file = %remote_path.display(),
                    entry = ?tagged.entry.key,
                    remote_snippet = %remote_tagged.entry.raw.chars().take(80).collect::<String>(),
                    local_snippet  = %tagged.entry.raw.chars().take(80).collect::<String>(),
                    "prefix mismatch: common entry has divergent content; \
                     remote version wins (§5.4)"
                );
                reporter.report_prefix_mismatch(
                    remote_path,
                    &tagged.entry.key,
                    &tagged.entry.raw,
                    &remote_tagged.entry.raw,
                );
                conflicts.push(PrefixConflict {
                    file: remote_path.to_owned(),
                    entry_key: tagged.entry.key.clone(),
                    local_raw: tagged.entry.raw.clone(),
                    remote_raw: remote_tagged.entry.raw.clone(),
                });
                // Remote version is already in key_map — skip the local entry.
            }
            // Identical content: remote version already in map; nothing to do.
        } else {
            // Local-only entry: add it to the union.
            key_map.insert(tagged.entry.key.clone(), tagged);
        }
    }

    // Collect and sort: header first, then ascending timestamp, then source,
    // then original index (gives a strict total order — no two entries are
    // ever "equal" by this key).
    let mut merged: Vec<TaggedEntry> = key_map.into_values().collect();

    merged.sort_by(|a, b| {
        // Session header is always the first entry.
        let a_header = a.entry.key == EntryKey::Header;
        let b_header = b.entry.key == EntryKey::Header;
        match (a_header, b_header) {
            (true, false) => return std::cmp::Ordering::Less,
            (false, true) => return std::cmp::Ordering::Greater,
            _ => {}
        }

        // Primary: ascending timestamp (entries without a timestamp sort last).
        let ts_ord = match (&a.entry.timestamp, &b.entry.timestamp) {
            (Some(ta), Some(tb)) => ta.cmp(tb),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        };
        if ts_ord != std::cmp::Ordering::Equal {
            return ts_ord;
        }

        // Secondary: remote entries sort before local entries.
        let src_ord = a.source.cmp(&b.source);
        if src_ord != std::cmp::Ordering::Equal {
            return src_ord;
        }

        // Tertiary: original line position preserves intra-file ordering.
        a.original_index.cmp(&b.original_index)
    });

    // Serialise back to JSONL (one JSON object per line, trailing newline).
    let content = if merged.is_empty() {
        String::new()
    } else {
        let mut out = String::new();
        for tagged in &merged {
            out.push_str(&tagged.entry.raw);
            out.push('\n');
        }
        out
    };

    MergeOutput {
        content,
        malformed,
        conflicts,
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::path::Path;

    fn remote_path() -> &'static Path {
        Path::new("remote.jsonl")
    }
    fn local_path() -> &'static Path {
        Path::new("local.jsonl")
    }

    // ── Helper: recording reporter ─────────────────────────────────────────

    /// A [`ConflictReporter`] that captures every call for inspection.
    struct RecordingReporter {
        calls: RefCell<Vec<(PathBuf, String, String, String)>>,
    }

    impl RecordingReporter {
        fn new() -> Self {
            Self {
                calls: RefCell::new(Vec::new()),
            }
        }

        fn call_count(&self) -> usize {
            self.calls.borrow().len()
        }
    }

    impl ConflictReporter for RecordingReporter {
        fn report_prefix_mismatch(
            &self,
            file: &Path,
            entry_key: &EntryKey,
            local_raw: &str,
            remote_raw: &str,
        ) {
            self.calls.borrow_mut().push((
                file.to_owned(),
                format!("{entry_key:?}"),
                local_raw.to_owned(),
                remote_raw.to_owned(),
            ));
        }
    }

    // ── Empty files ────────────────────────────────────────────────────────

    #[test]
    fn merge_two_empty_files_produces_empty_output() {
        let out = merge_jsonl("", remote_path(), "", local_path(), &NullReporter);
        assert!(out.content.is_empty());
        assert!(out.malformed.is_empty());
        assert!(out.conflicts.is_empty());
    }

    #[test]
    fn merge_empty_remote_with_local_entries_returns_local_path() {
        let local = "{\"type\":\"session\"}\n{\"type\":\"message\",\"id\":\"m1\",\"timestamp\":\"2024-01-01T00:00:00Z\"}\n";
        let out = merge_jsonl("", remote_path(), local, local_path(), &NullReporter);
        let lines: Vec<&str> = out.content.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("\"session\""));
        assert!(out.malformed.is_empty());
        assert!(out.conflicts.is_empty());
    }

    #[test]
    fn merge_remote_entries_with_empty_local_returns_remote_path() {
        let remote = "{\"type\":\"session\"}\n{\"type\":\"message\",\"id\":\"m1\",\"timestamp\":\"2024-01-01T00:00:00Z\"}\n";
        let out = merge_jsonl(remote, remote_path(), "", local_path(), &NullReporter);
        let lines: Vec<&str> = out.content.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("\"session\""));
        assert!(out.malformed.is_empty());
        assert!(out.conflicts.is_empty());
    }

    // ── Header ordering ─────────────────────────────────────────────────────

    #[test]
    fn session_header_is_always_first_in_output() {
        // Header appears second in the remote file — must still end up first.
        let remote = concat!(
            "{\"type\":\"message\",\"id\":\"m1\",\"timestamp\":\"2024-01-01T01:00:00Z\"}\n",
            "{\"type\":\"session\",\"timestamp\":\"2024-01-01T00:00:00Z\"}\n",
        );
        let out = merge_jsonl(remote, remote_path(), "", local_path(), &NullReporter);
        let first = out.content.lines().next().unwrap();
        assert!(first.contains("\"session\""));
    }

    // ── Set-union semantics ─────────────────────────────────────────────────

    #[test]
    fn union_combines_disjoint_entry_sets() {
        let remote = concat!(
            "{\"type\":\"session\"}\n",
            "{\"type\":\"message\",\"id\":\"a\",\"timestamp\":\"2024-01-01T01:00:00Z\"}\n",
        );
        let local = concat!(
            "{\"type\":\"session\"}\n",
            "{\"type\":\"message\",\"id\":\"b\",\"timestamp\":\"2024-01-01T02:00:00Z\"}\n",
        );
        let out = merge_jsonl(remote, remote_path(), local, local_path(), &NullReporter);
        assert!(out.malformed.is_empty());
        assert!(out.conflicts.is_empty());
        let content = &out.content;
        // Both entries present.
        assert!(content.contains("\"a\""));
        assert!(content.contains("\"b\""));
        // Session header appears exactly once.
        assert_eq!(content.matches("\"session\"").count(), 1);
    }

    #[test]
    fn remote_wins_for_duplicate_entry_key() {
        let remote_entry =
            "{\"type\":\"message\",\"id\":\"x\",\"content\":\"remote\",\"timestamp\":\"2024-01-01T01:00:00Z\"}";
        let local_entry =
            "{\"type\":\"message\",\"id\":\"x\",\"content\":\"local\",\"timestamp\":\"2024-01-01T01:00:00Z\"}";
        let remote = format!("{remote_entry}\n");
        let local = format!("{local_entry}\n");
        let out = merge_jsonl(&remote, remote_path(), &local, local_path(), &NullReporter);
        assert!(out.content.contains("remote"));
        assert!(!out.content.contains("local"));
    }

    #[test]
    fn idempotent_merge_same_file_returns_same_entries() {
        let content = concat!(
            "{\"type\":\"session\"}\n",
            "{\"type\":\"message\",\"id\":\"1\",\"timestamp\":\"2024-01-01T01:00:00Z\"}\n",
            "{\"type\":\"message\",\"id\":\"2\",\"timestamp\":\"2024-01-01T02:00:00Z\"}\n",
        );
        let out = merge_jsonl(content, remote_path(), content, local_path(), &NullReporter);
        let merged_lines: Vec<&str> = out.content.lines().collect();
        let original_lines: Vec<&str> = content.lines().collect();
        assert_eq!(merged_lines.len(), original_lines.len());
        // Idempotent merge of identical content produces no conflicts.
        assert!(out.conflicts.is_empty());
    }

    // ── Timestamp ordering ──────────────────────────────────────────────────

    #[test]
    fn entries_sorted_by_timestamp_ascending() {
        let remote = concat!(
            "{\"type\":\"session\"}\n",
            "{\"type\":\"message\",\"id\":\"late\",\"timestamp\":\"2024-01-01T03:00:00Z\"}\n",
        );
        let local = concat!(
            "{\"type\":\"session\"}\n",
            "{\"type\":\"message\",\"id\":\"early\",\"timestamp\":\"2024-01-01T01:00:00Z\"}\n",
        );
        let out = merge_jsonl(remote, remote_path(), local, local_path(), &NullReporter);
        let lines: Vec<&str> = out.content.lines().collect();
        // Line 0: header, Line 1: early, Line 2: late.
        assert!(lines[0].contains("\"session\""));
        assert!(lines[1].contains("\"early\""));
        assert!(lines[2].contains("\"late\""));
    }

    #[test]
    fn entries_without_timestamp_sort_after_timestamped_entries() {
        let remote = concat!(
            "{\"type\":\"session\"}\n",
            "{\"type\":\"message\",\"id\":\"no_ts\"}\n",
        );
        let local = concat!(
            "{\"type\":\"session\"}\n",
            "{\"type\":\"message\",\"id\":\"has_ts\",\"timestamp\":\"2024-01-01T01:00:00Z\"}\n",
        );
        let out = merge_jsonl(remote, remote_path(), local, local_path(), &NullReporter);
        let lines: Vec<&str> = out.content.lines().collect();
        assert!(lines[0].contains("\"session\""));
        assert!(lines[1].contains("\"has_ts\""));
        assert!(lines[2].contains("\"no_ts\""));
    }

    // ── Stable sort: remote-first tie-break ─────────────────────────────────

    #[test]
    fn equal_timestamps_remote_entries_precede_local_path() {
        let ts = "2024-01-01T00:00:00Z";
        let remote = format!(
            "{{\"type\":\"session\"}}\n\
             {{\"type\":\"message\",\"id\":\"r\",\"timestamp\":\"{ts}\"}}\n"
        );
        let local = format!(
            "{{\"type\":\"session\"}}\n\
             {{\"type\":\"message\",\"id\":\"l\",\"timestamp\":\"{ts}\"}}\n"
        );
        let out = merge_jsonl(&remote, remote_path(), &local, local_path(), &NullReporter);
        let lines: Vec<&str> = out.content.lines().collect();
        assert!(lines[0].contains("\"session\""));
        // Remote entry "r" must come before local entry "l".
        let pos_r = lines.iter().position(|l| l.contains("\"r\"")).unwrap();
        let pos_l = lines.iter().position(|l| l.contains("\"l\"")).unwrap();
        assert!(
            pos_r < pos_l,
            "remote entry should sort before local for equal timestamps"
        );
    }

    // ── Malformed line handling (§5.5) ──────────────────────────────────────

    #[test]
    fn malformed_line_is_skipped_and_valid_lines_preserved() {
        let content = concat!(
            "{\"type\":\"session\"}\n",
            "NOT VALID JSON\n",
            "{\"type\":\"message\",\"id\":\"ok\",\"timestamp\":\"2024-01-01T01:00:00Z\"}\n",
        );
        let out = merge_jsonl(content, remote_path(), "", local_path(), &NullReporter);
        assert_eq!(out.malformed.len(), 1);
        assert_eq!(out.malformed[0].line_number, 2);
        assert_eq!(out.malformed[0].snippet, "NOT VALID JSON");
        // Valid entries still appear in output.
        assert!(out.content.contains("\"session\""));
        assert!(out.content.contains("\"ok\""));
    }

    #[test]
    fn malformed_line_recorded_with_correct_path_and_snippet() {
        let bad_content = "{bad json here}\n";
        let out = merge_jsonl(bad_content, remote_path(), "", local_path(), &NullReporter);
        assert_eq!(out.malformed.len(), 1);
        assert_eq!(out.malformed[0].path, remote_path());
        assert_eq!(out.malformed[0].line_number, 1);
        assert!(out.malformed[0].snippet.contains("bad json here"));
    }

    #[test]
    fn long_malformed_line_snippet_truncated_to_80_chars() {
        let long_bad: String = "x".repeat(200);
        let out = merge_jsonl(&long_bad, remote_path(), "", local_path(), &NullReporter);
        assert_eq!(out.malformed[0].snippet.len(), 80);
    }

    #[test]
    fn multiple_malformed_lines_all_recorded() {
        let content = "bad1\nbad2\nbad3\n";
        let out = merge_jsonl(content, remote_path(), "", local_path(), &NullReporter);
        assert_eq!(out.malformed.len(), 3);
    }

    // ── Trailing newline and output format ───────────────────────────────────

    #[test]
    fn non_empty_output_ends_with_trailing_newline() {
        let content = "{\"type\":\"session\"}\n";
        let out = merge_jsonl(content, remote_path(), "", local_path(), &NullReporter);
        assert!(out.content.ends_with('\n'));
    }

    #[test]
    fn empty_output_has_no_trailing_newline() {
        let out = merge_jsonl("", remote_path(), "", local_path(), &NullReporter);
        assert!(out.content.is_empty());
    }

    // ── Merge scenarios from §5.3 ────────────────────────────────────────────

    #[test]
    fn scenario_appended_file_local_has_more_entries() {
        // Repo has header + 2 entries; local has header + 3 entries (append-only).
        let remote = concat!(
            "{\"type\":\"session\"}\n",
            "{\"type\":\"message\",\"id\":\"1\",\"timestamp\":\"2024-01-01T01:00:00Z\"}\n",
            "{\"type\":\"message\",\"id\":\"2\",\"timestamp\":\"2024-01-01T02:00:00Z\"}\n",
        );
        let local = concat!(
            "{\"type\":\"session\"}\n",
            "{\"type\":\"message\",\"id\":\"1\",\"timestamp\":\"2024-01-01T01:00:00Z\"}\n",
            "{\"type\":\"message\",\"id\":\"2\",\"timestamp\":\"2024-01-01T02:00:00Z\"}\n",
            "{\"type\":\"message\",\"id\":\"3\",\"timestamp\":\"2024-01-01T03:00:00Z\"}\n",
        );
        let out = merge_jsonl(remote, remote_path(), local, local_path(), &NullReporter);
        let lines: Vec<&str> = out.content.lines().collect();
        // All 4 unique entries (header + 3) should be present.
        assert_eq!(lines.len(), 4);
        assert!(out.malformed.is_empty());
        assert!(out.conflicts.is_empty());
    }

    #[test]
    fn scenario_divergent_file_both_sides_appended() {
        // Both machines appended different entries from the same ancestor.
        let remote = concat!(
            "{\"type\":\"session\"}\n",
            "{\"type\":\"message\",\"id\":\"common\",\"timestamp\":\"2024-01-01T00:00:00Z\"}\n",
            "{\"type\":\"message\",\"id\":\"from_remote\",\"timestamp\":\"2024-01-01T02:00:00Z\"}\n",
        );
        let local = concat!(
            "{\"type\":\"session\"}\n",
            "{\"type\":\"message\",\"id\":\"common\",\"timestamp\":\"2024-01-01T00:00:00Z\"}\n",
            "{\"type\":\"message\",\"id\":\"from_local\",\"timestamp\":\"2024-01-01T03:00:00Z\"}\n",
        );
        let out = merge_jsonl(remote, remote_path(), local, local_path(), &NullReporter);
        assert!(out.malformed.is_empty());
        // "common" is identical in both → no conflict.
        assert!(out.conflicts.is_empty());
        assert!(out.content.contains("\"common\""));
        assert!(out.content.contains("\"from_remote\""));
        assert!(out.content.contains("\"from_local\""));
        // "common" appears exactly once.
        assert_eq!(out.content.matches("\"common\"").count(), 1);
    }

    #[test]
    fn session_header_appears_exactly_once_when_present_in_both() {
        let remote = "{\"type\":\"session\",\"id\":\"s\"}\n";
        let local = "{\"type\":\"session\",\"id\":\"s\"}\n";
        let out = merge_jsonl(remote, remote_path(), local, local_path(), &NullReporter);
        assert_eq!(out.content.matches("\"session\"").count(), 1);
    }

    // ── Output ends with newline for every non-empty file ────────────────────

    #[test]
    fn content_without_trailing_newline_in_input_still_valid() {
        // Input lacks trailing newline — output should still be valid JSONL.
        let content = "{\"type\":\"session\"}";
        let out = merge_jsonl(content, remote_path(), "", local_path(), &NullReporter);
        assert!(out.content.ends_with('\n'));
    }

    // ── Prefix verification (US-007 / §5.4) ─────────────────────────────────

    #[test]
    fn identical_common_entries_produce_no_conflict() {
        // Both sides have the same entry with identical content.
        let content = concat!(
            "{\"type\":\"session\"}\n",
            "{\"type\":\"message\",\"id\":\"x\",\"timestamp\":\"2024-01-01T00:00:00Z\"}\n",
        );
        let reporter = RecordingReporter::new();
        let out = merge_jsonl(content, remote_path(), content, local_path(), &reporter);
        assert_eq!(
            reporter.call_count(),
            0,
            "no conflict reporter calls expected"
        );
        assert!(out.conflicts.is_empty());
        // Content still present.
        assert!(out.content.contains("\"x\""));
    }

    #[test]
    fn divergent_common_entry_triggers_conflict_and_uses_remote() {
        let remote_entry = "{\"type\":\"message\",\"id\":\"y\",\"extra\":\"remote_data\",\"timestamp\":\"2024-01-01T00:00:00Z\"}";
        let local_entry =
            "{\"type\":\"message\",\"id\":\"y\",\"extra\":\"local_data\",\"timestamp\":\"2024-01-01T00:00:00Z\"}";
        let remote = format!("{remote_entry}\n");
        let local = format!("{local_entry}\n");

        let reporter = RecordingReporter::new();
        let out = merge_jsonl(&remote, remote_path(), &local, local_path(), &reporter);

        // Conflict detected and reporter called.
        assert_eq!(reporter.call_count(), 1);
        assert_eq!(out.conflicts.len(), 1);

        let conflict = &out.conflicts[0];
        assert_eq!(conflict.file, remote_path());
        assert!(conflict.local_raw.contains("local_data"));
        assert!(conflict.remote_raw.contains("remote_data"));

        // Remote version wins in the merged output.
        assert!(out.content.contains("remote_data"));
        assert!(!out.content.contains("local_data"));
    }

    #[test]
    fn conflict_reporter_receives_correct_arguments() {
        let remote_entry = "{\"type\":\"message\",\"id\":\"z\",\"v\":\"r\",\"timestamp\":\"2024-01-01T00:00:00Z\"}";
        let local_entry =
            "{\"type\":\"message\",\"id\":\"z\",\"v\":\"l\",\"timestamp\":\"2024-01-01T00:00:00Z\"}";
        let remote = format!("{remote_entry}\n");
        let local = format!("{local_entry}\n");

        let reporter = RecordingReporter::new();
        let _ = merge_jsonl(&remote, remote_path(), &local, local_path(), &reporter);

        let calls = reporter.calls.borrow();
        assert_eq!(calls.len(), 1);
        let (file, key_debug, local_raw, remote_raw) = &calls[0];
        assert_eq!(file, remote_path());
        // Key debug should mention the type and id.
        assert!(key_debug.contains("message"));
        assert!(key_debug.contains("z"));
        // local_raw is what was passed as the local argument.
        assert!(local_raw.contains("\"l\""));
        // remote_raw is the committed version that wins.
        assert!(remote_raw.contains("\"r\""));
    }

    #[test]
    fn non_conflicting_entries_unaffected_by_prefix_verification() {
        // "common" is byte-identical in both; "only_remote" and "only_local"
        // are new entries that should be unaffected by verification.
        let remote = concat!(
            "{\"type\":\"session\"}\n",
            "{\"type\":\"message\",\"id\":\"common\",\"timestamp\":\"2024-01-01T00:00:00Z\"}\n",
            "{\"type\":\"message\",\"id\":\"only_remote\",\"timestamp\":\"2024-01-01T01:00:00Z\"}\n",
        );
        let local = concat!(
            "{\"type\":\"session\"}\n",
            "{\"type\":\"message\",\"id\":\"common\",\"timestamp\":\"2024-01-01T00:00:00Z\"}\n",
            "{\"type\":\"message\",\"id\":\"only_local\",\"timestamp\":\"2024-01-01T02:00:00Z\"}\n",
        );
        let reporter = RecordingReporter::new();
        let out = merge_jsonl(remote, remote_path(), local, local_path(), &reporter);
        // No conflict: "common" is identical; the other entries are unique.
        assert_eq!(reporter.call_count(), 0);
        assert!(out.conflicts.is_empty());
        assert!(out.content.contains("\"common\""));
        assert!(out.content.contains("\"only_remote\""));
        assert!(out.content.contains("\"only_local\""));
    }

    #[test]
    fn multiple_conflicting_entries_all_reported() {
        // Two entries conflict; both should be reported and remote versions win.
        let remote = concat!(
            "{\"type\":\"message\",\"id\":\"a\",\"v\":\"r1\",\"timestamp\":\"2024-01-01T01:00:00Z\"}\n",
            "{\"type\":\"message\",\"id\":\"b\",\"v\":\"r2\",\"timestamp\":\"2024-01-01T02:00:00Z\"}\n",
        );
        let local = concat!(
            "{\"type\":\"message\",\"id\":\"a\",\"v\":\"l1\",\"timestamp\":\"2024-01-01T01:00:00Z\"}\n",
            "{\"type\":\"message\",\"id\":\"b\",\"v\":\"l2\",\"timestamp\":\"2024-01-01T02:00:00Z\"}\n",
        );
        let reporter = RecordingReporter::new();
        let out = merge_jsonl(remote, remote_path(), local, local_path(), &reporter);
        assert_eq!(reporter.call_count(), 2);
        assert_eq!(out.conflicts.len(), 2);
        // Remote values win.
        assert!(out.content.contains("\"r1\""));
        assert!(out.content.contains("\"r2\""));
        assert!(!out.content.contains("\"l1\""));
        assert!(!out.content.contains("\"l2\""));
    }

    #[test]
    fn conflict_record_contains_file_path_and_both_raw_values() {
        let remote_entry =
            "{\"type\":\"message\",\"id\":\"k\",\"data\":\"from_remote\",\"timestamp\":\"2024-01-01T00:00:00Z\"}";
        let local_entry =
            "{\"type\":\"message\",\"id\":\"k\",\"data\":\"from_local\",\"timestamp\":\"2024-01-01T00:00:00Z\"}";
        let remote = format!("{remote_entry}\n");
        let local = format!("{local_entry}\n");

        let out = merge_jsonl(&remote, remote_path(), &local, local_path(), &NullReporter);
        assert_eq!(out.conflicts.len(), 1);

        let c = &out.conflicts[0];
        assert_eq!(c.file, remote_path());
        assert!(c.local_raw.contains("from_local"));
        assert!(c.remote_raw.contains("from_remote"));
        assert_eq!(
            c.entry_key,
            EntryKey::Entry {
                entry_type: "message".to_owned(),
                id: "k".to_owned(),
            }
        );
    }

    // ── Property-based tests (US-020) ─────────────────────────────────────────

    use proptest::prelude::*;
    use std::collections::BTreeSet;

    fn pa() -> &'static Path {
        Path::new("a.jsonl")
    }
    fn pb() -> &'static Path {
        Path::new("b.jsonl")
    }
    fn pc() -> &'static Path {
        Path::new("c.jsonl")
    }

    /// Build a JSONL string from a slice of raw entry lines.
    fn to_jsonl(lines: &[String]) -> String {
        if lines.is_empty() {
            String::new()
        } else {
            lines.join("\n") + "\n"
        }
    }

    /// Extract the set of `"type:id"` key strings from a JSONL content string.
    ///
    /// Implemented directly with `serde_json` so the helper is independent of
    /// the internal `parse_entry` function.
    fn key_set(content: &str) -> BTreeSet<String> {
        content
            .lines()
            .filter(|l| !l.trim().is_empty())
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .filter_map(|v| {
                let ty = v.get("type")?.as_str()?.to_owned();
                let id = v
                    .get("id")
                    .or_else(|| v.get("uuid"))
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_owned();
                Some(format!("{ty}:{id}"))
            })
            .collect()
    }

    /// Strategy: generate at most `max` JSONL message entries with unique IDs.
    ///
    /// IDs use `"<prefix><index:04>"` — caller passes a distinct prefix per
    /// strategy so that two generated sets are guaranteed to be disjoint.
    fn arb_entries(prefix: &'static str, max: usize) -> impl Strategy<Value = Vec<String>> {
        prop::collection::vec((0u64..24u64, 0u64..60u64, 0u64..60u64), 0..=max).prop_map(
            move |timestamps| {
                timestamps
                    .into_iter()
                    .enumerate()
                    .map(|(i, (h, m, s))| {
                        format!(
                            r#"{{"type":"message","id":"{}{:04}","timestamp":"2024-01-01T{:02}:{:02}:{:02}Z"}}"#,
                            prefix, i, h, m, s
                        )
                    })
                    .collect()
            },
        )
    }

    proptest! {
        /// Commutativity: the entry-key SET is the same regardless of which
        /// file is "remote" and which is "local" (§15.3).
        ///
        /// Disjoint ID spaces (`"a*"` vs `"b*"`) avoid conflict-resolution
        /// differences so the grow-only set-union property is exercised directly.
        #[test]
        fn prop_merge_commutativity(
            a_lines in arb_entries("a", 5),
            b_lines in arb_entries("b", 5),
        ) {
            let a = to_jsonl(&a_lines);
            let b = to_jsonl(&b_lines);

            let out_ab = merge_jsonl(&a, pa(), &b, pb(), &NullReporter);
            let out_ba = merge_jsonl(&b, pa(), &a, pb(), &NullReporter);

            let keys_ab = key_set(&out_ab.content);
            let keys_ba = key_set(&out_ba.content);
            prop_assert_eq!(keys_ab, keys_ba, "commutativity: entry-key sets differ");
        }

        /// Associativity: the entry-key SET of `merge(merge(A,B), C)` equals
        /// the entry-key set of `merge(A, merge(B,C))` (§15.3).
        #[test]
        fn prop_merge_associativity(
            a_lines in arb_entries("a", 4),
            b_lines in arb_entries("b", 4),
            c_lines in arb_entries("c", 4),
        ) {
            let a = to_jsonl(&a_lines);
            let b = to_jsonl(&b_lines);
            let c = to_jsonl(&c_lines);

            // (A ∪ B) ∪ C
            let ab   = merge_jsonl(&a, pa(), &b, pb(), &NullReporter).content;
            let ab_c = merge_jsonl(&ab, pa(), &c, pc(), &NullReporter);

            // A ∪ (B ∪ C)
            let bc   = merge_jsonl(&b, pb(), &c, pc(), &NullReporter).content;
            let a_bc = merge_jsonl(&a, pa(), &bc, pb(), &NullReporter);

            let keys_abc  = key_set(&ab_c.content);
            let keys_a_bc = key_set(&a_bc.content);
            prop_assert_eq!(keys_abc, keys_a_bc, "associativity: entry-key sets differ");
        }

        /// Idempotency: `merge(A, A)` contains exactly the same entries as A,
        /// with no conflicts (§15.3).
        #[test]
        fn prop_merge_idempotency(a_lines in arb_entries("x", 6)) {
            let a = to_jsonl(&a_lines);
            let out = merge_jsonl(&a, pa(), &a, pa(), &NullReporter);

            // No conflicts: identical entries share identical raw content.
            prop_assert!(
                out.conflicts.is_empty(),
                "idempotency: conflicts detected in merge(A, A)"
            );

            // Same entry-key set as the original.
            let orig_keys   = key_set(&a);
            let merged_keys = key_set(&out.content);
            prop_assert_eq!(
                orig_keys,
                merged_keys,
                "idempotency: key set changed after merge(A, A)"
            );
        }

        /// Superset: every entry from A and every entry from B appears in
        /// `merge(A, B)` (§15.3).
        #[test]
        fn prop_merge_superset(
            a_lines in arb_entries("a", 5),
            b_lines in arb_entries("b", 5),
        ) {
            let a = to_jsonl(&a_lines);
            let b = to_jsonl(&b_lines);

            let out = merge_jsonl(&a, pa(), &b, pb(), &NullReporter);
            let merged_keys = key_set(&out.content);

            for k in key_set(&a) {
                prop_assert!(
                    merged_keys.contains(&k),
                    "superset: A-entry '{}' missing from merge(A, B)",
                    k
                );
            }
            for k in key_set(&b) {
                prop_assert!(
                    merged_keys.contains(&k),
                    "superset: B-entry '{}' missing from merge(A, B)",
                    k
                );
            }
        }
    }
}
