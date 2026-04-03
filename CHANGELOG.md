# Changelog

All notable changes to **chronicle** will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

### Fixed

### Changed

## [0.8.2] - 2026-04-03

### Fixed
- **`chronicle doctor` SSH key false negative** — `git.ssh_key` reported
  `✗ Error: no readable SSH key found` even when the key was loaded in
  the SSH agent (`ssh-add`, macOS Keychain, 1Password SSH agent, agent
  forwarding, etc.). The check now falls through to an SSH agent probe
  (`SSH_AUTH_SOCK` set + socket path exists) before reporting an error.
  Pass detail is `"key loaded in SSH agent"`. Error text updated to
  `"no SSH key file found and no SSH agent available"`; hint now leads
  with `ssh-add` before `ssh-keygen`. Two new unit tests cover the
  agent-available (pass) and no-file-no-agent (error) paths.

## [0.8.1] - 2026-04-03

### Fixed
- **`chronicle doctor` stale-lock false positive** — A lock file whose
  PID has exited (sync completed normally) was reported as `✗ Error`,
  implying the operation failed. It is now reported as `⚠ Warn` with the
  message *"stale lock — PID X has exited"* and a hint that the file is
  auto-cleared on the next sync. Only a lock held by a **live** process
  past `lock_timeout_secs` retains the `✗ Error` severity (possible hung
  sync). One existing test renamed and updated; one new test added for
  the live-PID-past-timeout error path.

## [0.8.0] - 2026-04-03

### Added
- **`chronicle doctor` command** — Pre-flight health check across four subsystems:
  - *Config*: verifies config file exists, parses as valid TOML, and `git.remote`
    is set.
  - *Git*: verifies the local repo is initialised, the remote is reachable (5 s
    TCP timeout), and at least one SSH key exists (check skipped for HTTPS
    remotes).
  - *Agents*: verifies each enabled agent’s sessions directory exists and reports
    the JSONL file count.
  - *Scheduler*: verifies the cron entry is installed and no stale lock is held.
- Plain-English remediation hints follow each failing or warned check.
- `--porcelain` flag emits stable `check.<key>=<state>[:<detail>]` and
  `summary.*` lines for scripting.
- Exit codes: `0` all-pass, `1` warnings-only, `2` any error.
- Color output uses TTY detection and respects `NO_COLOR` / `--no-color`,
  consistent with `chronicle status`.
- `src/doctor/mod.rs` — `CheckState` enum (`Pass`, `Warn`, `Error`, `Skipped`),
  `CheckResult` struct with concise constructors, `check_config()`, `check_git()`,
  `check_agents()`, `check_scheduler()`; `default_check_remote()` and
  `default_ssh_key_paths()` for production use; 23 unit tests.
- 3 integration tests covering all-pass happy path and two error paths.

## [0.7.0] - 2026-04-03

### Added
- **Expanded proptest generators** — `arb_home_path()` now generates usernames
  with hyphens, dots, and spaces under both `/Users` and `/home` roots.
  `arb_subpath()` covers plain, space-containing, and dot-containing components
  up to 8 levels deep.  Three new property-based tests exercise the round-trip
  invariant against content templates, deeply-nested (4-level) JSON objects, and
  arrays of 1–5 home paths.
- **`cargo-fuzz` sub-workspace and `fuzz_roundtrip` target** — A libFuzzer
  target (`fuzz/fuzz_targets/fuzz_roundtrip.rs`) verifies the L2/L3
  canonicalize→decanonicalize round-trip invariant against arbitrary byte
  inputs.  The target parses inputs as `(home_path_len, level, home_path,
  json_line)`, double-normalises via `serde_json` to guard against float
  non-idempotency, and asserts `decanon(canon(x)) == x` for all valid JSON
  objects.  Ships with two seed corpus entries (`seed_simple.bin`,
  `seed_claude.bin`).  Live fuzzing uncovered and fixed two correctness issues
  (BTreeMap key-ordering pre-normalisation; float non-idempotency guard).
- **Fuzz CI integration** — `fuzz-build` job added to both
  `.github/workflows/ci.yml` and `.forgejo/workflows/ci.yml` (nightly
  toolchain, build-only on every PR).  New `.github/workflows/fuzz.yml` and
  `.forgejo/workflows/fuzz.yml` run the fuzz target for 60 seconds every Sunday
  at 02:00 UTC; any crash fails the workflow automatically.

## [0.6.0] - 2026-04-03

### Added
- **`sync_state.json` data layer** — `chronicle sync`, `push`, and `pull` now
  atomically write a `sync_state.json` file (co-located with `chronicle.lock`)
  recording the timestamp, wall-clock duration, and operation type
  (`sync`/`push`/`pull`) of the last successful operation.  Read by
  `chronicle status` to display last-sync information.  Non-fatal on write
  failure (logged at `WARN`, never blocks the sync).
- **`chronicle status` improvements** — Complete redesign of the status output:
  - **Config / Machine section** — machine name, remote URL, per-agent
    sessions-directory existence (with `⚠` when missing).
  - **Last Sync section** — timestamp (RFC 3339), elapsed duration, and
    operation type from `sync_state.json`; shows `never synced` when the
    file does not exist.
  - **Pending Files section** — count of locally modified session files not
    yet pushed; `--verbose` expands to the full file list.
  - **Lock State section** — whether `chronicle.lock` is held, by whom (PID),
    and for how long.
  - **Scheduler section** — cron schedule installed/not-installed/malformed;
    when installed, shows the cron expression and next-run time.
  - **`--verbose` / `-v`** — expands Pending Files to paths and Config to
    effective config values.
  - **`--porcelain`** — stable `key=value` output for scripting; all defined
    keys are always emitted (empty value when not applicable).
  - **`--no-color`** — suppress ANSI colour; also honoured via `NO_COLOR` env
    var and non-TTY stdout detection.
  - All output written through a generic `StatusFormatter<W: io::Write>` so
    the full command is unit-testable without spawning a subprocess.

## [0.5.0] - 2026-03-30

### Added
- **Stale lock recovery (ADR-001)** — `try_acquire_sync_lock` now writes the
  holder's PID and a UTC timestamp into `chronicle.lock`.  When a new process
  finds the lock held, it checks whether the holder is still alive (`kill(pid, 0)`)
  and whether the lock age exceeds a configurable timeout.  If either check
  indicates staleness the lock is broken automatically, allowing the next cron
  invocation to proceed without manual intervention.  This fixes the scenario
  where a machine sleeps mid-sync and the hung process keeps the lock for hours.
- **`general.lock_timeout_secs` config option** — controls the maximum lock age
  before automatic recovery (default: 300 seconds / 5 minutes).  Set to `0` for
  PID-only recovery, or `-1` to disable recovery entirely.
- **Advisory lock on `chronicle pull`** — `pull_impl` now acquires the same
  advisory flock as `sync` and `push`, preventing concurrent pull/sync races.

### Fixed
- **Stale lock after sleep/suspend** — machines that entered a low-power state
  during a cron-scheduled sync would hold the lock indefinitely after waking,
  silently skipping all subsequent syncs.  The new staleness detection
  (PID liveness + age timeout) resolves this automatically.

### Changed
- **`toml` bumped from 0.8 to 1.1** — API-compatible major version stabilization.
- **`rand` bumped from 0.8 to 0.9** — `SliceRandom` renamed to `IndexedRandom`,
  `thread_rng()` renamed to `rng()`.
- **CI actions bumped** — `actions/checkout` v6, `actions/cache` v5,
  `actions/upload-artifact` v7, `actions/download-artifact` v8.

## [0.4.3] - 2026-03-31

### Fixed
- **Cross-platform release builds** — `openssl-sys` failed to locate a system
  OpenSSL library in cross-compile contexts (`x86_64-apple-darwin` on ARM macOS
  runners; `aarch64-unknown-linux-gnu` via `cross`). Enabled `git2`'s
  `vendored-openssl` feature to compile OpenSSL from source, eliminating the
  system library dependency for all cross-compile targets.

### Changed
- **`git2` bumped from 0.18 to 0.20.4** — resolves a low-severity advisory:
  potential undefined behaviour when dereferencing a `Buf` struct.

## [0.4.2] - 2026-03-30

### Added
- **Materialization state cache** — `MaterializeCache` (stored as `materialize-state.json`
  alongside the repo) tracks each repo file's mtime/size at the time it was last
  materialized. `materialize_agent_dir` now checks the cache before reading any repo
  file; unchanged files are skipped entirely (no `fs::read_to_string`, no
  de-canonicalization). On a typical cron run where only a handful of files changed
  on the remote, this reduces the materialize pass from ~2.5 minutes to < 1 second.
  Cache is invalidated automatically when the canonicalization config (`level` or
  `home_token`) changes.
- **Advisory file lock for sync/push** — `sync_impl` and `push_impl` now acquire an
  exclusive non-blocking `flock` on `<repo-parent>/chronicle.lock` before starting
  work. If the lock is already held by another Chronicle process, the new invocation
  logs a message and exits cleanly without error, eliminating the git-index-lock
  cascade that occurred when cron intervals overlapped.

### Fixed
- **`pull_impl` materializes unconditionally** — `pull --` (manual) called
  `materialize_repo_to_local` even when `integrate_remote_changes` returned 0 (remote
  already in sync). Now applies the same `remote_integrated > 0` fast-path guard that
  `sync_impl` has, skipping the full ~1.74 GB repo read when nothing arrived from the
  remote.

## [0.4.1] - 2026-03-30

### Fixed
- **State cache never populated for existing sessions** — `process_push_file`
  returned `Ok(None)` when the repo already had identical content, so those
  files were never added to the state cache. Every subsequent scan re-classified
  all of them as new, causing each cron sync to re-read and re-merge thousands
  of files, run for 3–5 minutes, and overlap with the next cron invocation.
  Now returns `Ok(Some(PushedFile { stage: false, … }))` so the file’s
  mtime/size is recorded and future scans skip it as `Unchanged`.

## [0.4.0] - 2026-03-30

### Added
- **Sync jitter** — `chronicle sync --quiet` (cron mode) now sleeps a deterministic per-machine offset before starting the sync cycle, spreading machines uniformly across the cron interval to avoid thundering-herd push contention. Configurable via `general.sync_jitter_secs`: `0` (default) = auto, `> 0` = cap in seconds, `-1` = disable.

### Fixed

### Changed

## [0.3.0] - 2026-03-30

### Added
- **Crate-level documentation** — `src/lib.rs` now carries a full `//!` doc block
  describing Chronicle's purpose and module layout (`cargo doc` is now useful)
- **`StateCache::path_for_repo`** — new helper co-locates the state cache with
  the repo so multiple Chronicle installs are always isolated from one another
- **`RingBuffer::path_for_repo`** — same isolation applied to the error ring
  buffer; `sync_impl`, `push_impl`, and `pull_impl` now derive the ring buffer
  path from `storage.repo_path` instead of the global XDG default

### Fixed
- **`canonicalization.level` range validation** — values outside `1–3` are now
  rejected at both the serde deserialization layer and `chronicle config set`
- **`status_impl` branch** — `chronicle status` now reads `cfg.storage.branch`
  instead of the hardcoded `"main"` when opening the git repo
- **State cache temp filename collision** — `StateCache::save` now uses full
  nanoseconds since epoch (matching `RingBuffer::write_atomic`) instead of
  `subsec_nanos` which wraps every second
- **`push_impl` manifest** — `chronicle push` now writes and stages
  `manifest.json` so `last_sync` is recorded for push-only users
- **`push_with_retry` on_rejection** — the retry closure in `push_impl` now
  performs a real `fetch` + `integrate_remote_changes` cycle so retries can
  actually resolve divergence (previously a no-op)
- **Claude `decode_dir` lossiness** — double-slash paths produced by the lossy
  `/` + `.` → `-` encoding now emit a `tracing::warn!` at runtime

### Changed
- **`StateCache::default_path` deprecated** — replaced by `path_for_repo` in
  all production call sites; marked `#[deprecated(since = "0.2.2")]`
- **Dead-code blankets removed** — all `#![allow(dead_code)]` module-level
  attributes removed now that all 21 delivery stories are complete
- **`expand_path` / `expand_home` consolidated** — `config::expand_path_with_home`
  is now the single implementation; `config::expand_path` delegates to it and
  all CLI impls accept an injected home parameter for full testability

## [0.2.4] - 2026-03-30

### Fixed
- **macOS cron SSH agent discovery** — neither `launchctl getenv` nor `launchctl asuser` can reach the user's GUI session from cron's system bootstrap context; replaced with a `find` scan of `/private/tmp/com.apple.launchd.*/Listeners` filtered by socket type and ownership, which reliably discovers the launchd-managed SSH agent socket

## [0.2.3] - 2026-03-30

### Fixed
- **Cron SSH agent visibility** — crontab entries now inject `SSH_AUTH_SOCK` discovery so that `ssh_key_from_agent()` can reach the user's SSH agent from the minimal cron environment (macOS: `launchctl getenv`, Linux: systemd socket fallback)
- **Binary path parsing** — `parse_installed_binary` now locates the chronicle binary by finding the token before `"sync"` instead of relying on a fixed positional index, making it resilient to the `SSH_AUTH_SOCK` prefix
- **Test bare-repo branch name** — bare remotes in tests now explicitly set `initial_head("main")` so tests pass on systems where git defaults to `master`

## [0.2.2] - 2026-03-29

### Fixed
- SSH credentials for remote Git operations — libgit2 requires an explicit
  credentials callback; tries SSH agent, then key files (`~/.ssh/id_ed25519`,
  `~/.ssh/id_ecdsa`, `~/.ssh/id_rsa`), then system git credential helper for
  HTTPS remotes
- Default branch enforcement — `git init` now always creates the configured
  branch (default: `main`) via `RepositoryInitOptions::initial_head`,
  overriding the system `init.defaultBranch` setting; push and pull use the
  configured branch explicitly rather than reading HEAD
- Integration test parallelism race — state cache path is now derived from
  `storage.repo_path` instead of a global XDG path, isolating each test

## [0.2.1] - 2026-03-29

### Fixed
- SSH authentication for remote Git operations — libgit2 requires an explicit
  credentials callback; now tries SSH agent first, then key files
  (`~/.ssh/id_ed25519`, `~/.ssh/id_ecdsa`, `~/.ssh/id_rsa`), then the system
  git credential helper for HTTPS remotes

## [0.2.0] - 2026-03-29

### Added
- **Config module** - TOML schema with per-agent settings, machine-name generation (adjective-animal), XDG-compliant config path, CLI/env/file/default precedence chain
- **Canonicalization engine** - L1 `$HOME` path canonicalization, L2 whitelisted-field JSONL walker, L3 freeform text scan, `{{SYNC_HOME}}` token, custom token registry, full round-trip de-canonicalization
- **Merge module** - JSONL entry identity (type + id), grow-only set-union merge preserving append-only invariant, prefix verification to detect tampered history
- **Git module** - repo initialization, working tree management, fetch/push with exponential backoff retry, conventional-commit message formatting, staging
- **Agent modules** - Pi and Claude Code directory path encoding/decoding and session file naming conventions
- **CLI commands** - `chronicle init`, `import`, `sync`, `push`, `pull`, `status`, `errors`, `config`, `schedule install/uninstall/status`
- **Partial history materialization** - pull only the N most recent sessions per project while retaining full Git history
- **File scanner** - mtime/size cache for detecting changed `.jsonl` files without full re-scan
- **Scheduler** - crontab read/write/install/uninstall/status for macOS and Linux
- **Error ring buffer** - 30-entry structured error log persisted to disk
- **`src/lib.rs`** - library root exposing all modules for integration test access
- **Integration tests** - 8 end-to-end multi-machine scenario tests in `tests/integration.rs`
- **Property-based tests** - `proptest` round-trip tests for canonicalization and merge commutativity/idempotency
- Initial project scaffold with documentation and conventions
- `README.md` - project overview, setup, and usage
- `AGENTS.md` - agent-facing development guidance
- `CLAUDE.md` - Claude-specific coding instructions
- `docs/001-architecture.md` - system architecture
- `docs/002-development-guide.md` - development workflow
- `docs/003-documentation-standards.md` - documentation conventions
- `docs/specs/001-initial-delivery.md` - full project specification (v1.0)
- `.editorconfig` - cross-editor formatting rules
- `.gitattributes` - Git line-ending normalization
- `Cargo.toml` - Rust package manifest

## [0.1.0] - 2026-03-29

### Added
- Project initialized

[Unreleased]: https://github.com/geekmuse/chronicle/compare/v0.5.0...HEAD
[0.5.0]: https://github.com/geekmuse/chronicle/compare/v0.4.3...v0.5.0
[0.4.3]: https://github.com/geekmuse/chronicle/compare/v0.4.2...v0.4.3
[0.4.2]: https://github.com/geekmuse/chronicle/compare/v0.4.1...v0.4.2
[0.4.1]: https://github.com/geekmuse/chronicle/compare/v0.4.0...v0.4.1
[0.4.0]: https://github.com/geekmuse/chronicle/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/geekmuse/chronicle/compare/v0.2.4...v0.3.0
[0.2.4]: https://github.com/geekmuse/chronicle/compare/v0.2.3...v0.2.4
[0.2.3]: https://github.com/geekmuse/chronicle/compare/v0.2.2...v0.2.3
[0.2.2]: https://github.com/geekmuse/chronicle/compare/v0.2.1...v0.2.2
[0.2.1]: https://github.com/geekmuse/chronicle/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/geekmuse/chronicle/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/geekmuse/chronicle/releases/tag/v0.1.0
