---
date_created: 2026-04-03
date_modified: 2026-04-03
status: active
audience: both
cross_references:
  - docs/001-architecture.md
  - docs/specs/001-initial-delivery.md
  - src/cli/mod.rs
  - src/errors/ring_buffer.rs
  - src/scan/mod.rs
---

# Spec 002 — `chronicle status` Improvements

## 1. Goal

Replace the current terse `chronicle status` output with a structured,
human-friendly display that makes it immediately obvious whether chronicle is
healthy, what has changed since the last sync, and why a sync may have failed.

## 2. Primary Use Cases

1. **Quick sanity check** — run before or after a manual `chronicle sync` to
   confirm the state is clean.
2. **Debugging** — understand why a cron-scheduled sync didn't run (lock held,
   no cron entry, etc.).
3. **Change awareness** — see which session files have changed since the last
   sync without having to run a full sync.

## 3. Output Format

### 3.1 Default (terse)

One line per concern.  Each line starts with a status symbol:

| Symbol | Color  | Meaning                              |
|--------|--------|--------------------------------------|
| `✓`    | green  | OK / healthy                         |
| `⚠`    | yellow | Warning — needs attention but not blocking |
| `✗`    | red    | Error — action required              |

Example:

```
✓  Machine:        eager-falcon  (remote: git@github.com:user/chronicle-sync.git)
✓  Last sync:      2026-04-03 14:22 UTC  (took 1.3 s)
⚠  Pending files:  3 files changed since last sync
✓  Lock:           free
⚠  Scheduler:      cron job installed — next run in 4 min
✗  Config:         claude agent enabled but sessions directory not found
```

### 3.2 Verbose (`--verbose` / `-v`)

Same sections, expanded:

- **Pending files**: lists each changed file (relative to the agent sessions
  root) instead of just the count.
- **Last sync**: adds per-phase timing breakdown if available.
- **Config**: prints effective config values (remote URL, agents enabled,
  canonicalization level, lock timeout).

### 3.3 Porcelain (`--porcelain`)

Key=value pairs, one per line, stable across versions (suitable for scripts):

```
machine=eager-falcon
remote=git@github.com:user/chronicle-sync.git
last_sync_time=2026-04-03T14:22:00Z
last_sync_duration_ms=1300
pending_files=3
lock_state=free
scheduler_state=installed
scheduler_next_run_secs=240
config_ok=false
config_error=claude agent enabled but sessions directory not found
```

Missing values (e.g., `last_sync_time` if never synced) are emitted as empty:
`last_sync_time=`.

## 4. Sections

### 4.1 Config / Machine

| Terse field  | Content |
|---|---|
| Machine name | `general.machine_name` from loaded config |
| Remote URL   | `git.remote` from loaded config |

**Error conditions (✗):**
- Config file not found or not parseable.
- A required field (`git.remote`) is missing or empty.
- An enabled agent's sessions directory does not exist.

### 4.2 Last Sync

Sources: a small state file written at the end of each successful sync
(see §7 — new `sync_state.json`).

| Terse field | Content |
|---|---|
| Timestamp   | UTC timestamp of last successful sync |
| Duration    | Elapsed wall-clock seconds |

If no sync has ever completed, display: `⚠  Last sync: never`.

### 4.3 Pending Files

Uses the existing scan module (`src/scan/mod.rs`) to count files whose mtime
or size differs from the last recorded scan state.

| Mode    | Content |
|---|---|
| Default | Count only: `N files changed since last sync` |
| Verbose | Full list of relative file paths, one per line, indented |

If 0 files pending: `✓  Pending files: none`.

### 4.4 Lock State

Reads `chronicle.lock` from the repo work directory.

| State  | Symbol | Display |
|--------|--------|---------|
| Free   | ✓ | `free` |
| Held (live PID) | ⚠ | `held by PID <n> since <timestamp>` |
| Stale (dead PID or timed out) | ✗ | `stale lock (PID <n> dead) — run chronicle sync to clear` |

### 4.5 Scheduler

Reads crontab entries via the existing scheduler module.

| State       | Symbol | Display |
|-------------|--------|---------|
| Installed   | ✓ | `installed — next run in <N> min` |
| Not installed | ⚠ | `not installed — run chronicle schedule install` |
| Installed but malformed | ✗ | `cron entry present but unrecognised format` |

Next-run time is computed from the cron expression relative to `now`.  If the
cron expression cannot be parsed to determine next-run time, omit the
`next run in` clause.

## 5. Exit Codes

`chronicle status` always exits **0** regardless of what it finds.  It is
informational only; scripting should use `--porcelain` and parse the output.

## 6. Color Handling

- Color and symbols are enabled by default when stdout is a TTY.
- Color is suppressed when stdout is not a TTY, or when `NO_COLOR` is set, or
  when `--no-color` is passed.
- `--porcelain` implies no color and no symbols.

## 7. New: `sync_state.json`

A new file written by `sync_impl` (and `push_impl` / `pull_impl`) at the end
of each successful operation, stored alongside `chronicle.lock` in the repo
work directory:

```json
{
  "last_sync_time": "2026-04-03T14:22:00Z",
  "last_sync_duration_ms": 1300,
  "last_sync_op": "sync"
}
```

`last_sync_op` is one of `"sync"`, `"push"`, `"pull"`.

`chronicle status` reads this file; if absent it reports `⚠  Last sync: never`.

## 8. Implementation Notes

- **No new dependencies** — use existing `chrono` (or `std::time`) for
  timestamps, existing scheduler module for cron parsing, existing scan module
  for pending-file detection.
- Add a `--porcelain` flag to the existing `StatusArgs` struct in `src/cli/mod.rs`.
- Extract `status_impl` from the CLI command handler so it can be unit-tested.
- The `sync_state.json` write must be atomic (write to `.tmp` then rename).

## 9. Out of Scope

- Remote vs local divergence (commit-ahead/behind count) — requires a network
  fetch; excluded to keep `status` instant.
- Recent errors from the ring buffer — excluded per interview; error detail
  lives in `chronicle errors`.
- `--json` output — `--porcelain` is sufficient for scripting.

## 10. Acceptance Criteria

1. `chronicle status` prints all five sections (Config, Last Sync, Pending
   Files, Lock, Scheduler) in default mode with correct symbols and color.
2. `--verbose` expands Pending Files to a file list and Config to effective
   values.
3. `--porcelain` prints stable `key=value` pairs with no color or symbols.
4. Exit code is always 0.
5. `sync_state.json` is written atomically after each successful sync/push/pull.
6. All new logic has unit tests; `chronicle status` integration test covers the
   happy path and at least one error path.
