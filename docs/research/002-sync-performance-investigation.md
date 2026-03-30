---
date_created: 2026-03-30
date_modified: 2026-03-30
status: active
audience: agent
cross_references:
  - docs/research/001-codebase-audit.md
  - docs/001-architecture.md
  - docs/research/003-sync-performance-validation.md
  - src/cli/mod.rs
  - src/scan/mod.rs
  - src/materialize_cache.rs
---

# Research: Sync Performance Investigation

## Goal

Diagnose why `chronicle sync` cron jobs were not reliably pushing to the
remote on macOS 12.7.6 (radiant-axolotl), while the same version worked on
macOS 26.3 Tahoe (cheerful-sparrow).

---

## Environment

| Machine | OS | Hostname | Chronicle version |
|---|---|---|---|
| radiant-axolotl (primary) | macOS 12.7.6 | local | 0.4.1 |
| cheerful-sparrow | macOS 26.3 Tahoe | remote | 0.4.x |

Session files: **2,402 total** (~988 Pi + ~1,414 Claude), **avg 744 KB each**,
**~1.74 GB total** in the repo working tree.

---

## Root Cause #1 — FIXED in 0.4.1

### State cache never populated for already-current files

**File:** `src/cli/mod.rs` — `process_push_file` and call sites in
`sync_impl` / `push_impl`

**What happened:**

`process_push_file` returns `Ok(None)` when the merged content equals what is
already in the repo (file up-to-date). The caller loops did nothing in the
`Ok(None)` arm — no entry was added to `cache_updates`. As a result, the
state cache was **never populated** for the ~2,400 files already in the repo.

Before the SSH auth fix (v0.2.2) every sync failed at the push step, so the
state cache (which is only written on successful sync completion) was always
empty on radiant-axolotl. After the SSH fix, syncs completed but the
`Ok(None)` path still never cached anything.

**Consequence:**

Every cron run re-classified all 2,400+ files as `New` (not in cache), called
`process_push_file` for each, read the local file, read the repo file, merged,
compared, got `Ok(None)`, and discarded. Each sync took **3–5 minutes** on
macOS 12.7.6, exceeding the 5-minute cron interval. Subsequent cron invocations
queued waiting on the git index lock, creating a cascade that eventually ran
for 133+ minutes of back-to-back syncs working through the queue.

**Fix applied (commit `5d9e7da`):**

Changed the "already current" path in `process_push_file` from `Ok(None)` to
`Ok(Some(PushedFile { stage: false, ... }))`. The `stage: false` flag tells
the caller to add to `cache_updates` but NOT to `pi_staged`/`claude_staged`.
Updated both `sync_impl` and `push_impl` loops to always push to
`cache_updates` regardless of `pushed.stage`.

The `Ok(None)` return is now reserved only for files sitting directly in
`source_dir` (no session-subdir level), which the scanner shouldn't produce
in practice.

**Verification:**

After one warm-up sync with 0.4.1:
- State cache populated: `cached: 2402` (was `cached: 2`)
- `chronicle status` shows `Pending: 0 new, 1 modified` (the active session)

---

## Root Cause #2 — FIXED in 0.4.1

### Materialize phase runs unconditionally — reads 1.74 GB every sync

**File:** `src/cli/mod.rs` — `sync_impl` Phase 3

**What happened:**

`materialize_repo_to_local` reads every `.jsonl` file in the repo working
tree (`~1.74 GB`), de-canonicalizes each line, and compares with the local
copy. Previously it ran unconditionally on every sync regardless of whether
any remote content had arrived.

**Consequence:**

Even with the state cache warm (scan = instant), each sync still took
**~2.5 minutes** on the materialize phase alone, reading 1.74 GB from disk.

**Fix applied (commit after `5d9e7da`, same branch):**

Lifted the `integrated` counter out of the Phase 2 `if remote_url.is_some()`
block into a `remote_integrated: usize` variable visible to Phase 3. Phase 3
now only runs materialize when `remote_integrated > 0`:

```rust
let materialized = if remote_integrated > 0 {
    materialize_repo_to_local(...)
} else {
    0
};
```

Files that went **out** are already present locally — there is nothing to
materialize for outgoing-only cycles.

**Note:** The initial attempt included `|| outgoing_count > 0` in the
condition, which was wrong: `outgoing_count` is non-zero on virtually every
run (the active session file is always modified), so materialize never
skipped. This was corrected to `remote_integrated > 0` only.

---

## Remaining Issue — FIXED in v0.4.2

### Materialize state cache eliminates full 1.74 GB read when remote_integrated > 0

**Files changed:** `src/materialize_cache.rs` (new), `src/cli/mod.rs`, `src/lib.rs`

**What happened:**

When any remote files arrived (`remote_integrated > 0`), `materialize_repo_to_local`
read **all** 2,402 repo files, de-canonicalized them, and compared with local —
even though only a handful actually changed. There was no mtime/size record of
which repo files had already been materialized.

**Measured impact (before fix):** ~2.5 minutes for a single remote file integrated.

**Fix applied (v0.4.2):**

Added `MaterializeCache` (`src/materialize_cache.rs`) — a `HashMap<String,
MaterializeFileState>` keyed by repo-relative path, where `MaterializeFileState`
holds `repo_mtime: DateTime<Utc>` and `repo_size: u64`. Persisted as
`<repo-parent>/materialize-state.json` (sibling of the repo dir, consistent
with `StateCache::path_for_repo`).

`materialize_agent_dir` now:
1. Reads `fs::metadata` for each repo file.
2. Checks if `(mtime, size)` matches the cache — if so, skips `read_to_string`
   and de-canonicalization entirely.
3. Updates the cache entry after writing or confirming identical local content.

The cache is loaded once in `materialize_repo_to_local` and saved after a
successful pass. Both `sync_impl` and `pull_impl` benefit automatically.

Cache invalidation: a `config_hash` field (`"level:home_token"`) is stored in
the cache and compared on load; a mismatch clears the cache and forces a full
re-materialization pass.

**Additional fix (pull_impl fast-path):**

`pull_impl` now applies the same `remote_integrated > 0` guard that `sync_impl`
already had. A `chronicle pull` with no remote changes now returns immediately
without any materialize I/O.

**Additional fix (advisory file lock):**

`sync_impl` and `push_impl` now acquire a non-blocking `flock` on
`<repo-parent>/chronicle.lock` at startup. A second concurrent process exits
cleanly with a message rather than queuing on the git index lock.

**Additional factor for Claude (unchanged):**

`select_partial_session_files` scores Claude files by calling
`claude_earliest_file_timestamp(path)`, which reads the entire file. The user
is on `history_mode = "full"` which skips this path entirely. The materialize
cache does not help here because partial-mode scoring is done before
materialization — but this path is never reached in the current config.

---

## Current State After All Fixes (v0.4.2)

| Phase | Before v0.4.1 | After v0.4.1 | After v0.4.2 |
|---|---|---|---|
| Scan (2,402 files, warm cache) | 3–5 min (cache always empty) | ~0.1 s | ~0.1 s |
| Git fetch + push | 2 s | 2 s | 2 s |
| Materialize (remote_integrated = 0) | ~2.5 min (always ran) | 0 s (skipped) | 0 s (skipped) |
| Materialize (remote_integrated > 0, warm mat-cache) | ~2.5 min | ~2.5 min | < 1 s (cache hit) |
| Materialize (remote_integrated > 0, cold/changed) | ~2.5 min | ~2.5 min | ~2.5 min (first run only) |
| **Typical cron run (no remote changes)** | **3–5 min** | **~5–10 s** | **~5–10 s** |
| **Cron run with remote changes (warm cache)** | **3–5 min** | **~2.5 min** | **~5–10 s** |

The **typical cron run** (no remote changes) is now ~5–10 seconds instead of
3–5 minutes. Overlapping runs and the lock-queue cascade are eliminated.
Pushes reliably reach the remote on both machines.

---

## Files Modified

### v0.4.1

| File | Change |
|---|---|
| `src/cli/mod.rs` | `process_push_file`: `Ok(None)` → `Ok(Some(stage:false))` for already-current case |
| `src/cli/mod.rs` | `sync_impl`/`push_impl` loops: always push to `cache_updates` |
| `src/cli/mod.rs` | `sync_impl` Phase 2: lift `integrated` to `remote_integrated` |
| `src/cli/mod.rs` | `sync_impl` Phase 3: skip materialize when `remote_integrated == 0` |
| `Cargo.toml` | Version bumped `0.4.0` → `0.4.1` |
| `CHANGELOG.md` | Added `[0.4.1]` entry |

### v0.4.2 (this branch: ralph/sync-perf-remaining)

| File | Change |
|---|---|
| `src/cli/mod.rs` | `pull_impl`: skip materialize when `remote_integrated == 0` (US-001) |
| `src/materialize_cache.rs` | New: `MaterializeCache` + `MaterializeFileState` structs with load/save/path_for_repo (US-002) |
| `src/lib.rs` | Register `pub mod materialize_cache` (US-002) |
| `src/cli/mod.rs` | `materialize_agent_dir`: accept `MaterializeCache` param, check mtime/size before read (US-003) |
| `src/cli/mod.rs` | `materialize_repo_to_local`: load/save `MaterializeCache`; apply config_hash invalidation (US-003) |
| `src/cli/mod.rs` | `sync_impl`/`push_impl`: acquire advisory `flock` via `try_acquire_sync_lock` (US-004) |
| `Cargo.toml` | Add `libc = "0.2"` under `[target.'cfg(unix)'.dependencies]`; bump version `0.4.1` → `0.4.2` |
| `CHANGELOG.md` | Added `[0.4.2]` entry |
| `AGENTS.md` | Updated Current Version to `0.4.2` |
| `docs/research/002-sync-performance-investigation.md` | Updated Remaining Issue section and tables |
| `docs/research/003-sync-performance-validation.md` | Updated gap status for Gap 1 and Gap 2 |

## Commits (v0.4.2)

- `5181344` — US-001: apply pull_impl fast-path (remote_integrated > 0 guard)
- `697682d` — US-002: add MaterializeCache schema and persistence
- `ff697c5` — US-003: wire MaterializeCache into materialize_agent_dir
- `815ebe5` — US-004: add advisory flock to sync_impl and push_impl
- (US-005 documentation commit — this branch)
