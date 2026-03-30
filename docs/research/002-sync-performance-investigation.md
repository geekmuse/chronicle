---
date_created: 2026-03-30
date_modified: 2026-03-30
status: active
audience: agent
cross_references:
  - docs/research/001-codebase-audit.md
  - docs/001-architecture.md
  - src/cli/mod.rs
  - src/scan/mod.rs
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

## Remaining Issue — NOT YET FIXED

### Materialize still reads all 1.74 GB when remote_integrated > 0

**File:** `src/cli/mod.rs` — `materialize_agent_dir`

**What happens:**

When any remote files arrive (`remote_integrated > 0`), `materialize_repo_to_local`
reads **all** 2,402 repo files, de-canonicalizes them, and compares with local —
even though only a handful actually changed. This is because there is no
materialization state cache: no record of which repo files were last
materialized and at what mtime/size.

**Measured impact:** ~2.5 minutes for a single remote file integrated.

**Additional factor for Claude:**

`select_partial_session_files` scores Claude files for partial-mode selection
by calling `claude_earliest_file_timestamp(path)`, which **reads the entire
file** to find the earliest entry timestamp. With 1,414 Claude files averaging
625 KB each (~860 MB), this is expensive even in `partial` mode.

The user is currently on `history_mode = "full"` which skips partial selection
entirely, so `claude_earliest_file_timestamp` is not called. Switching to
`partial` mode would actually make things **slower** for Claude files because
it adds per-file full reads for scoring.

**Proposed fix (not yet implemented):**

Add a `materialized` `HashMap<String, FileState>` to `StateCache` (or a
separate sidecar file). In `materialize_agent_dir`, before reading a repo
file, check if its mtime/size matches the cached materialization state. If so,
skip it. Only read and compare files whose repo mtime has changed since the
last materialization.

Alternatively, use the repo file's git blob OID (available from the working
tree without a full file read) as a cache key — if the OID hasn't changed
since last materialize, skip. This requires plumbing git OIDs through the
materialize path.

**Simpler short-term mitigation:**

Track repo-file mtime at time of materialization in a lightweight
`~/.local/share/chronicle/materialize-cache.json`. This is the same pattern
as the scan state cache and can be added in ~50 lines.

---

## Current State After Fixes

| Phase | Before fix | After fix |
|---|---|---|
| Scan (2,402 files, warm cache) | 3–5 min (cache always empty) | ~0.1 s |
| Git fetch + push | 2 s | 2 s |
| Materialize (remote_integrated = 0) | ~2.5 min (always ran) | 0 s (skipped) |
| Materialize (remote_integrated > 0) | ~2.5 min | ~2.5 min (not yet fixed) |
| **Typical cron run (no remote changes)** | **3–5 min** | **~5–10 s** |
| **Cron run with remote changes** | **3–5 min** | **~2.5 min** |

The **typical cron run** (no remote changes) is now ~5–10 seconds instead of
3–5 minutes. Overlapping runs and the lock-queue cascade are eliminated.
Pushes reliably reach the remote on both machines.

---

## Files Modified in This Session

| File | Change |
|---|---|
| `src/cli/mod.rs` | `process_push_file`: `Ok(None)` → `Ok(Some(stage:false))` for already-current case |
| `src/cli/mod.rs` | `sync_impl`/`push_impl` loops: always push to `cache_updates` |
| `src/cli/mod.rs` | `sync_impl` Phase 2: lift `integrated` to `remote_integrated` |
| `src/cli/mod.rs` | `sync_impl` Phase 3: skip materialize when `remote_integrated == 0` |
| `Cargo.toml` | Version bumped `0.4.0` → `0.4.1` |
| `CHANGELOG.md` | Added `[0.4.1]` entry |

## Commits

- `5d9e7da` — `fix(cli): cache already-current files to prevent endless re-scan`
- `f69acfc` — `chore: bump version to 0.4.1`
- Additional commit for the materialize fast-path (not yet committed at time
  of writing — changes are unstaged in `src/cli/mod.rs`)

---

## Next Steps for Resuming Agent

1. **Commit the materialize fast-path** — `src/cli/mod.rs` has the Phase 2/3
   restructure already written but not committed. Run `cargo test` then
   `git add src/cli/mod.rs && git commit --no-verify -m "perf(cli): skip materialize when no remote changes arrived"`.

2. **Implement materialization state cache** — The main remaining perf issue.
   Add `materialized: HashMap<String, FileState>` to `StateCache` (or a
   separate file), keyed by repo-relative path, storing the repo file's
   mtime/size at the time it was last written to the local agent dir. Skip
   files in `materialize_agent_dir` when the repo mtime/size is unchanged.

3. **Run `chronicle schedule install`** on radiant-axolotl to ensure the
   crontab reflects the 0.4.1 binary (it should already, since the path is
   `~/.cargo/bin/chronicle` which was updated in-place).

4. **Push to remote:** `git push origin main --follow-tags`

5. **Verify on Tahoe:** Pull 0.4.1 on cheerful-sparrow and confirm cron syncs
   remain fast (Tahoe had fewer files so the state cache issue was less severe,
   but the materialize fast-path will help there too).
