---
date_created: 2026-03-30
date_modified: 2026-03-30
status: active
audience: both
cross_references:
  - docs/research/002-sync-performance-investigation.md
  - docs/research/001-codebase-audit.md
  - docs/001-architecture.md
  - src/cli/mod.rs
  - src/scan/mod.rs
---

# Validation: Sync Performance Investigation (002)

Independent review of `docs/research/002-sync-performance-investigation.md`
covering correctness, breadth, accuracy, and proposed fix assessment.

---

## Finding-by-Finding Validation

### Root Cause #1 — State cache never populated for already-current files

**Verdict: ✅ CONFIRMED — correct in all material respects**

**Code trace:**

The `git diff 5d9e7da~1..5d9e7da -- src/cli/mod.rs` confirms the pre-fix
state exactly as described:

1. **Pre-fix `process_push_file`** (line ~1112 before patch): when
   `out.content == repo_content`, the function returned `Ok(None)`.
2. **Pre-fix caller loops** in both `sync_impl` (line ~557) and `push_impl`
   (line ~871): the `Ok(None)` arm performed no action — `cache_updates`
   was not populated.
3. **Post-fix `process_push_file`**: the "already current" path now returns
   `Ok(Some(PushedFile { staged_rel: PathBuf::new(), cache_key, file_state,
   stage: false }))`.
4. **Post-fix caller loops**: `cache_updates.push(...)` is now unconditional
   on the `Ok(Some(pushed))` arm; the `pushed.stage` flag gates only the
   staging/counting logic.

The `PushedFile` struct now has a `stage: bool` field (added in the same
commit), and its doc comment accurately describes the semantics.

**File path accuracy:** `src/cli/mod.rs` — correct. Functions
`process_push_file`, `sync_impl`, `push_impl` — all exist at the described
locations.

**Commit reference:** `5d9e7da` — verified in git log as
`fix(cli): cache already-current files to prevent endless re-scan`. The
commit message body matches the investigation's description.

**Behavioral accuracy:** The described cascade (empty cache → all files
classified as `New` → full re-read/merge/compare → `Ok(None)` → cache stays
empty → repeat) is exactly what the code would produce. The `StateCache`
is only written in the bookkeeping phase at the end of `sync_impl`
(line ~720), and only from `cache_updates`, which was never populated for
the majority of files.

**Minor note:** The investigation states `Ok(None)` is "now reserved only
for files sitting directly in `source_dir` (no session-subdir level)". This
is confirmed by the code at line ~1087 (`if file_name_str.is_empty() {
return Ok(None); }`), which fires when a file has no session subdirectory
component. The comment in the `push_impl` `Ok(None)` arm was updated to
reflect this.

---

### Root Cause #2 — Materialize phase runs unconditionally

**Verdict: ✅ CONFIRMED — correct, with one stale comment noted**

**Code trace:**

The `git diff f69acfc..eb248c0 -- src/cli/mod.rs` confirms:

1. **Pre-fix `sync_impl` Phase 2**: the `if remote_url.is_some() { ... }`
   block was a statement (no return value), so `integrated` was scoped inside
   the block and unavailable to Phase 3.
2. **Post-fix**: Phase 2 is restructured as `let remote_integrated: usize =
   if remote_url.is_some() { ... integrated } else { 0 };` — the
   `integrated` count is now visible to Phase 3.
3. **Post-fix Phase 3**: `let materialized = if remote_integrated > 0 {
   materialize_repo_to_local(...) } else { 0 };` — materialize is now
   conditional.

**File path accuracy:** `src/cli/mod.rs` Phase 3 — confirmed at lines
~692–710 in the current code.

**Commit reference:** `eb248c0` — verified as
`perf(cli): skip materialize when no remote changes arrived`.

**`outgoing_count` note accuracy:** The investigation states the initial
attempt included `|| outgoing_count > 0` which was wrong because
`outgoing_count` is non-zero on virtually every run. This is confirmed:
`outgoing_count` equals `all_staged.len()` (line 583), which counts files
that had actual content changes. On a typical run with an active session,
this is indeed non-zero. The current code correctly uses only
`remote_integrated > 0`.

**⚠️ Stale comment in current code (lines 695–700):** The block comment
above the Phase 3 condition still says "Materialize still runs when
`remote_integrated > 0` (new remote content) *or when `outgoing_count > 0`*
(we just committed, so we should reflect that back to local dirs)." This
comment describes the initial (incorrect) logic, not the final condition.
The actual code only checks `remote_integrated > 0`. This is a cosmetic
issue but could confuse a future reader. **Recommend removing the
`outgoing_count > 0` sentence from the comment.**

---

### Remaining Issue — Materialize reads all 1.74 GB when `remote_integrated > 0`

**Verdict: ✅ CONFIRMED — accurate diagnosis**

**Code trace:**

`materialize_agent_dir` (lines ~1555–1693) performs a full `fs::read_dir`
walk of all session subdirectories, reads every `.jsonl` file via
`fs::read_to_string`, de-canonicalizes each line, and compares with the
local copy. There is no mtime/size check before reading — every file is
read unconditionally.

The only short-circuit is the content comparison at line ~1676:
```rust
if local_file_path.exists() {
    if let Ok(existing) = fs::read_to_string(&local_file_path) {
        if existing == decanon_content { continue; }
    }
}
```
This requires reading BOTH the repo file (for de-canonicalization) AND the
local file (for comparison) before skipping — so even unchanged files incur
two full reads plus de-canonicalization CPU cost.

**Accuracy of the `claude_earliest_file_timestamp` concern:**

The investigation correctly identifies that `claude_earliest_file_timestamp`
(line ~1493) reads the entire file to find the earliest timestamp. It's
called from `select_partial_session_files` (line ~1525) which is only
invoked when `MaterializeFilter::Partial` is active. The investigation
correctly notes the user is on `history_mode = "full"` which skips this
path entirely.

**Proposed fixes assessment:**

1. **Materialization state cache** (`HashMap<String, FileState>` keyed by
   repo-relative path, storing mtime/size at last materialization): This is
   sound. The scan module's `StateCache` is a proven pattern in this
   codebase. Checking mtime/size before reading would eliminate the I/O for
   unchanged files.

2. **Git blob OID approach**: Technically sound but higher implementation
   complexity — requires plumbing `git2::Oid` through the materialize path,
   and working-tree files don't have blob OIDs without an explicit
   `git2::Repository::blob_path()` call. The mtime/size approach is simpler
   and sufficient.

3. **Short-term mitigation (separate sidecar file)**: Reasonable as a quick
   win. However, placing it in `~/.local/share/chronicle/` would reintroduce
   the global-path issue fixed in v0.3.0. The investigation suggests this
   path but the better location would be alongside the existing state cache
   (sibling of the repo dir), consistent with `StateCache::path_for_repo`.

---

### Performance Numbers

**Verdict: ⚠️ PARTIALLY VERIFIABLE — plausible but not independently reproducible**

The investigation reports:

| Metric | Claimed value |
|--------|---------------|
| Session files | 2,402 total (~988 Pi + ~1,414 Claude) |
| Average file size | ~744 KB |
| Total repo size | ~1.74 GB |
| Scan time (warm cache) | ~0.1 s |
| Materialize time (full) | ~2.5 min |
| Pre-fix cron run | 3–5 min |
| Post-fix cron run (no remote) | ~5–10 s |

**Assessment:**

- The 2,402 file count and 1.74 GB total are stated as environment-specific
  facts from the user's radiant-axolotl machine; cannot be independently
  verified from the codebase alone, but are internally consistent
  (2402 × 744 KB ≈ 1.74 GB).
- The "3–5 minutes" pre-fix timing is plausible: reading 2,402 files ×
  744 KB = ~1.74 GB of reads for the push phase, plus merging each file
  (parsing JSONL, building hash maps, comparing), on a macOS 12.7.6 system
  with potentially spinning disk or aged SSD. macOS 12 has `read_to_string`
  overhead from APFS metadata lookups.
- The "~2.5 minutes" materialize time is plausible: it requires reading
  all 2,402 repo files (1.74 GB) + de-canonicalization CPU + reading all
  2,402 local files for comparison (~1.74 GB) = ~3.48 GB of I/O total.
- The "~0.1 s" warm-cache scan time is plausible: `scan_dir` only calls
  `fs::metadata` (stat) per file, no reads. 2,402 stat calls on a warm
  filesystem cache is sub-second.
- The "~5–10 s" post-fix timing is plausible: scan (~0.1 s) + process
  ~1 modified file + git operations (~2 s) + skip materialize (0 s) +
  cache write.

**The numbers are not benchmarks in the formal sense** — no profiling tool
output, no repeated runs, no confidence intervals. They appear to be
wall-clock observations from cron logs. This is appropriate for a diagnostic
investigation but should not be cited as precise measurements.

---

## Breadth Assessment — Gaps and Underexplored Areas

### Gap 1: `pull_impl` still materializes unconditionally

**Severity: Medium**

The investigation focuses on `sync_impl` but `pull_impl` (lines 1200–1251)
calls `materialize_repo_to_local` unconditionally — there is no
`remote_integrated > 0` guard. On a pull with zero integrated changes (e.g.,
remote is already in sync), the full 1.74 GB materialize pass still runs.

This is less critical because `pull` is a manual command (not cron-driven),
but for consistency and user experience, `pull_impl` should apply the same
fast-path. The investigation doesn't mention this.

### Gap 2: No concurrency protection / stale lock cascade

**Severity: Medium (diagnostic gap)**

The investigation mentions "Subsequent cron invocations queued waiting on
the git index lock, creating a cascade" but doesn't describe what mechanism
prevents this. There is:

- No file lock / flock in `sync_impl` to prevent concurrent Chronicle
  processes.
- No git index lock acquisition/check before starting work.
- No PID file or advisory lock.

The fix (making sync fast enough to complete within the cron interval) is
an implicit mitigation, but if sync ever slows down again (e.g., large
remote materialize), the cascade will recur. The investigation should have
noted the absence of explicit concurrency protection as a systemic risk.

### Gap 3: `integrate_remote_changes` reads all remote blobs

**Severity: Low-Medium**

`integrate_remote_changes` (line ~1257) walks the entire remote tree and
reads every `.jsonl` blob via `repo.find_blob(oid)`. For 2,402 files, this
means reading all blobs from the git object store, then comparing content
with the local working tree files. The `if local_content == remote_content
{ continue; }` check (line ~1313) avoids the merge/write, but the blob
read + working tree read still happens for every file.

This is O(n) in total files, not O(changed files). For the current
workload it's inside the git object store (packed objects, memory-mapped),
so it's faster than the filesystem-based materialize, but it will scale
linearly with repo growth.

The investigation doesn't mention this as a performance factor, likely
because it's dominated by the materialize cost. But as the repo grows
(especially with more machines contributing sessions), this could become
significant.

### Gap 4: Sequential file processing (no parallelism)

**Severity: Low**

All file processing in `sync_impl` is sequential: scan → process each file
serially → integrate remote changes serially → materialize serially. For
2,402 files averaging 744 KB, parallelizing I/O-bound work (e.g., using
`rayon` for the push/materialize loops) could provide significant speedup,
especially on SSDs with high queue depths.

The investigation doesn't explore parallelism as an optimization avenue.
This is understandable for a diagnostic document but worth noting for
future work.

### Gap 5: `merge_jsonl` allocations per file

**Severity: Low**

Each call to `merge_jsonl` (in `process_push_file` and
`integrate_remote_changes`) parses both files into `HashMap<EntryKey, ...>`
structures, allocating per-entry. For large session files (744 KB average,
potentially thousands of JSONL entries), this creates significant allocator
pressure. The investigation doesn't profile allocation overhead, which
could contribute to the observed times.

### Gap 6: State cache key uses absolute local path

**Severity: Low (correctness concern)**

The state cache keys files by their absolute local path string (e.g.,
`/Users/brad/.pi/agent/sessions/abc/session.jsonl`). If the user moves
their home directory or the agent session path changes, the entire cache
is invalidated and all files are re-scanned as `New`. The investigation
correctly identifies that a warm cache eliminates the scan cost, but
doesn't flag this fragility.

---

## Proposed Fixes Assessment

### Fix 1: State cache population (Root Cause #1)

**Verdict: ✅ Sound — correctly addresses the root cause**

The `stage: false` pattern is clean and backward-compatible. The `PushedFile`
struct extension is well-documented. Both `sync_impl` and `push_impl` callers
are updated. No regressions identified — all 343 unit tests + 8 integration
tests pass.

**Potential edge case:** If `process_push_file` encounters a read error on
the local file (line ~1097 `fs::read_to_string`), it logs a warning and
returns `Ok(None)`. This file is NOT cached, so it will be retried on every
subsequent sync. This is correct behavior (the file might become readable
later), but the investigation doesn't mention this explicitly.

### Fix 2: Conditional materialize (Root Cause #2)

**Verdict: ✅ Sound — correctly addresses the root cause, with one minor
concern**

Skipping materialize when `remote_integrated == 0` is correct because:
- Outgoing files originate from the local filesystem and are already present
  locally.
- The only reason to materialize is when new content arrives from the remote.

**Minor concern:** The comment block (lines 695–700) is stale and describes
a condition (`|| outgoing_count > 0`) that was removed. This should be
cleaned up.

**Regression risk:** If a user manually edits a file in the repo working
tree (outside of Chronicle's normal flow) and then runs `chronicle sync`,
the change won't be materialized to local agent dirs unless remote changes
also arrive. This is an extremely unlikely user action and not a practical
concern.

### Proposed Fix 3: Materialization state cache (not yet implemented)

**Verdict: ✅ Sound design, with implementation recommendations**

The proposed approach (mtime/size cache for repo files, checked before
reading in `materialize_agent_dir`) mirrors the proven `StateCache` pattern
used for scan. Recommendations:

1. **Location:** Use `StateCache::path_for_repo` pattern (sibling of repo
   dir), NOT a global `~/.local/share/chronicle/` path.
2. **Key:** Use repo-relative path (e.g., `pi/sessions/--foo--/s.jsonl`),
   not absolute path, since repo files have stable relative paths.
3. **Scope:** Add it as a second field in the existing `StateCache` struct
   or as a co-located `materialize-state.json` file. A separate file avoids
   changing the existing cache format.
4. **Cache invalidation:** Invalidate when the de-canonicalization config
   changes (e.g., `canonicalization.level` or `home_token` changes). This
   isn't mentioned in the investigation but is important — if the token
   changes, all files need re-materialization even if repo mtime is
   unchanged.

---

## Summary

| Finding | Verdict | Notes |
|---------|---------|-------|
| Root Cause #1: State cache never populated | ✅ Confirmed | Code, commit, and behavior all verified |
| Root Cause #2: Materialize runs unconditionally | ✅ Confirmed | Stale comment re `outgoing_count` noted |
| Remaining: Materialize reads all files | ✅ Confirmed | Accurate diagnosis and sound proposals |
| Performance numbers | ⚠️ Plausible | Wall-clock observations, not formal benchmarks |
| Proposed Fix #1 (cache population) | ✅ Sound | No regressions, well-tested |
| Proposed Fix #2 (conditional materialize) | ✅ Sound | Minor stale comment to clean up |
| Proposed Fix #3 (materialize cache) | ✅ Sound design | Needs cache-invalidation strategy |

**Gaps identified:**

| Gap | Severity | Description |
|-----|----------|-------------|
| `pull_impl` unconditional materialize | Medium | Not covered by the sync fast-path |
| No concurrency protection | Medium | Lock cascade risk remains systemic |
| `integrate_remote_changes` O(n) blob reads | Low-Medium | Scales with total files, not changes |
| Sequential file processing | Low | No parallelism explored |
| `merge_jsonl` allocation pressure | Low | Not profiled |
| State cache key fragility | Low | Absolute path keys; no migration |

**Overall assessment:** The investigation is accurate, well-evidenced, and
its fixes are sound. The two identified root causes are genuine and the
applied fixes correctly address them without introducing regressions. The
remaining materialization performance issue is accurately diagnosed and the
proposed solutions are viable. The main gaps are the omission of
`pull_impl`'s identical problem and the absence of concurrency protection
discussion.
