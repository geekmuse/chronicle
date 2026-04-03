---
date_created: 2026-04-03
date_modified: 2026-04-03
status: active
audience: both
cross_references:
  - docs/001-architecture.md
  - docs/specs/001-initial-delivery.md
  - docs/specs/002-status-improvements.md
  - src/cli/mod.rs
  - src/config/mod.rs
  - src/git/mod.rs
  - src/scheduler/cron.rs
  - src/agents/mod.rs
  - src/doctor/mod.rs
---

# Spec 004 — `chronicle doctor`

## 1. Goal

Add a `chronicle doctor` command that performs a structured, pre-flight
health check across all major subsystems and reports problems with plain-English
remediation hints.  It is report-only — no automatic fixes.

## 2. Primary Use Case

Run `chronicle doctor` when:
- Setting up chronicle on a new machine.
- Diagnosing why syncs are silently failing.
- Onboarding a new user and confirming the environment is correctly configured.

## 3. Output Format

### 3.1 Default (human-readable)

Output is grouped into four sections.  Each check within a section is
prefixed with a status symbol:

| Symbol | Color  | Meaning |
|--------|--------|---------|
| `✓`    | green  | Check passed |
| `⚠`    | yellow | Warning — non-blocking but should be addressed |
| `✗`    | red    | Error — blocks correct operation |

Each failed or warned check is followed by an indented remediation hint on the
next line(s).

```
Config
  ✓  Config file found:      /Users/bradmatic/.config/chronicle/config.toml
  ✓  Config is valid TOML
  ✗  git.remote is empty
     Set git.remote in your config.toml, e.g.:
       [git]
       remote = "git@github.com:you/chronicle-sync.git"

Git
  ✓  Repository initialised:  /Users/bradmatic/.local/share/chronicle/repo
  ✗  Remote not reachable:    git@github.com:you/chronicle-sync.git
     Verify the remote URL and that your SSH key is authorised.
  ✓  SSH key found:           /Users/bradmatic/.ssh/id_ed25519

Agents
  ✓  Pi sessions dir:         /Users/bradmatic/.pi/sessions  (12 session files)
  ✗  Claude sessions dir not found: /Users/bradmatic/.claude/projects
     Install Claude Code or update the agent.claude.sessions_dir config key.

Scheduler
  ✓  Crontab entry installed
  ⚠  Lock file present:       held by PID 99312 (alive) since 2026-04-03T14:10Z
     A sync is currently running. If it appears stuck, run: chronicle sync

─────────────────────────────────────────────
2 errors · 1 warning · 5 checks passed
Exit code: 2
```

A summary line is always printed at the end.

### 3.2 Porcelain (`--porcelain`)

Stable `key=value` lines, one per check, no color or symbols:

```
check.config.file=ok
check.config.toml=ok
check.config.remote=error:git.remote is empty
check.git.repo=ok
check.git.remote=error:not reachable
check.git.ssh_key=ok
check.agents.pi=ok:12 files
check.agents.claude=error:directory not found
check.scheduler.cron=ok
check.scheduler.lock=warning:lock held by live PID 99312
summary.errors=2
summary.warnings=1
summary.passed=5
```

Values follow the pattern `ok[:<detail>]`, `warning:<detail>`, or
`error:<detail>`.  Keys are stable across versions.

## 4. Checks

### 4.1 Config Section

| Check | Key | Pass condition |
|-------|-----|----------------|
| Config file found | `config.file` | File exists at XDG config path |
| Config parses as valid TOML | `config.toml` | `serde_toml` deserialization succeeds |
| `git.remote` is set | `config.remote` | Non-empty string |

**Remediation hints:**

- File not found: `Run chronicle init to create a default config file.`
- Invalid TOML: `Edit the config file and fix the TOML syntax error: <error message>.`
- Remote empty: `Set git.remote in your config.toml.`

### 4.2 Git Section

| Check | Key | Pass condition |
|-------|-----|----------------|
| Repo initialised | `git.repo` | Chronicle repo directory exists and is a valid git repo |
| Remote reachable | `git.remote` | `git ls-remote <remote>` succeeds (or SSH handshake for SSH remotes) |
| SSH key found | `git.ssh_key` | At least one of `~/.ssh/id_ed25519`, `~/.ssh/id_ecdsa`, `~/.ssh/id_rsa` exists and is readable |

**Notes:**

- If `git.remote` is empty, the remote reachability check is skipped and
  reported as `⚠  Skipped: no remote configured`.
- SSH key check applies only to SSH remotes (URL starts with `git@` or
  `ssh://`).  HTTPS remotes skip this check.
- Remote reachability has a **5-second timeout**; if it times out, report
  `✗ Remote timed out` rather than hanging.

**Remediation hints:**

- Repo not initialised: `Run chronicle init to set up the repository.`
- Remote not reachable: `Verify the remote URL and that your SSH key is authorised on the remote host.`
- SSH key not found: `Generate an SSH key with ssh-keygen -t ed25519 and add the public key to your remote host.`

### 4.3 Agents Section

For each agent enabled in config (`pi`, `claude`):

| Check | Key | Pass condition |
|-------|-----|----------------|
| Sessions directory exists | `agents.<name>` | Directory exists and is readable |

On pass, include a file count in the detail: `ok:N files`.

**Remediation hints:**

- Pi directory not found: `Verify that Pi is installed and that agent.pi.sessions_dir points to the correct path.`
- Claude directory not found: `Verify that Claude Code is installed and that agent.claude.sessions_dir points to the correct path.`

### 4.4 Scheduler Section

| Check | Key | Pass condition |
|-------|-----|----------------|
| Crontab entry installed | `scheduler.cron` | Chronicle cron entry found in crontab |
| No stale lock | `scheduler.lock` | Lock file absent, or held by a live PID that is < lock_timeout_secs old |

**Remediation hints:**

- Cron not installed: `Run chronicle schedule install to add the cron job.`
- Lock held by live PID: `A sync is currently running. If it appears stuck, the lock will be cleared automatically after <timeout> seconds.`
- Stale lock: `The lock is stale. It will be cleared automatically on the next sync, or run chronicle sync to clear it now.`

## 5. Exit Codes

| Code | Condition |
|------|-----------|
| `0`  | All checks passed (no errors, no warnings) |
| `1`  | One or more warnings, no errors |
| `2`  | One or more errors |

If both warnings and errors exist, exit code is `2`.

## 6. Color Handling

Same rules as `chronicle status` (spec 002 §6):
- Enabled by default when stdout is TTY.
- Suppressed by `NO_COLOR`, `--no-color`, or non-TTY stdout.
- `--porcelain` implies no color.

## 7. Implementation Notes

- Add `DoctorArgs { porcelain: bool }` to `src/cli/mod.rs` and a `Doctor`
  subcommand.
- Extract all check logic into a `doctor_impl` function in `src/cli/mod.rs`
  (or a new `src/doctor/mod.rs` if the module grows large).
- Each check returns a `CheckResult { key: &str, state: CheckState, detail: String, hint: Option<String> }`.
- `CheckState` is an enum: `Pass`, `Warn`, `Error`, `Skipped`.
- Remote reachability uses the existing `git2` credential flow; apply a
  connect timeout of 5 seconds via `git2::RemoteCallbacks`.
- The SSH key check is a simple `std::fs::metadata` read.
- Disk-space check is **excluded** per interview (was a suggested option but
  not selected).
- Chronicle binary version check is **excluded** per interview.

## 8. Out of Scope

- Auto-fix mode.
- Interactive fix prompts.
- Disk space check.
- Chronicle binary version check.
- `--json` output (`--porcelain` is sufficient).

## 9. Acceptance Criteria

1. `chronicle doctor` runs all checks in the four sections (Config, Git,
   Agents, Scheduler) and prints grouped output with symbols and color.
2. Each failing check shows a plain-English remediation hint.
3. `--porcelain` produces stable `check.<key>=<state>:<detail>` lines.
4. Exit codes: 0 all-pass, 1 warnings-only, 2 any-error.
5. Remote reachability check times out after ≤ 5 seconds and reports an error
   rather than hanging.
6. SSH key check is skipped for HTTPS remotes.
7. All check logic has unit tests; an integration test covers the happy path
   (all checks pass) and at least two error paths.
