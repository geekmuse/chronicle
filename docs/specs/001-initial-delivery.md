---
date_created: 2026-03-29
date_modified: 2026-03-29
status: active
audience: both
cross_references:
  - docs/001-architecture.md
  - docs/002-development-guide.md
  - AGENTS.md
---

# Chronicle — Agent Session History Sync

## Specification v1.0

**Language:** Rust
**Platforms:** macOS, Linux (Windows planned)
**License:** TBD

---

## 1. Problem Statement

AI coding agent session history (Pi, Claude Code) is stored locally under `$HOME`-relative paths. Users who work across multiple machines — where `$HOME` varies (`/Users/bradmatic`, `/Users/brad`, `/home/bradmatic`, `/home/brad`) — have no way to access or continue session history from another machine.

Chronicle is a bidirectional sync tool that keeps session history files synchronized across machines using a canonicalization layer that abstracts away per-machine path differences, with Git as the storage and transport backend. Scheduling is handled by cron — Chronicle itself is a stateless CLI that runs a sync cycle and exits.

---

## 2. Scope

### In Scope

- Pi agent session history (`~/.pi/agent/sessions/**/*.jsonl`)
- Claude Code session history (`~/.claude/projects/**/*.jsonl`)
- Configurable per-agent (enable/disable individually)
- Full history retained in canonical Git storage
- Configurable partial materialization on local machines

### Out of Scope

- Agent settings/configuration files
- Installed extensions, skills, themes, packages
- Project-local files (`CLAUDE.md`, `.ralphi/`, `.pi/`)
- Shell dotfiles, git config, SSH config
- Deletion propagation (deferred — files are never removed from canonical store)

---

## 3. Architecture Overview

```
┌─────────────────────────────────────────────────────────────────┐
│                         Machine A                               │
│                                                                 │
│  ~/.pi/agent/sessions/         ~/.claude/projects/              │
│  └── --Users-bradmatic-…--/    └── -Users-bradmatic-…/          │
│      └── session1.jsonl            └── session1.jsonl           │
│              │                            │                     │
│              ▼                            ▼                     │
│     ┌────────────────────────────────────────────┐              │
│     │           Chronicle (sync)                  │              │
│     │  ┌──────────┐  ┌───────────┐  ┌──────────┐│              │
│     │  │ Canonicalizer │  Merger  │  │ De-canon ││              │
│     │  └──────────┘  └───────────┘  └──────────┘│              │
│     └────────────────────┬───────────────────────┘              │
│                          │                                      │
│     ~/.local/share/chronicle/repo/  (git working tree)          │
│     └── --{{SYNC_HOME}}-Dev-foo--/                              │
│         └── session1.jsonl  (canonicalized content)             │
└──────────────────────────┬──────────────────────────────────────┘
                           │  git push / pull
                           ▼
                    ┌──────────────┐
                    │  Git Remote  │
                    │  (user-def)  │
                    └──────────────┘
                           │
                           ▼
┌──────────────────────────┬──────────────────────────────────────┐
│                       Machine B                                 │
│     ~/.local/share/chronicle/repo/  (git working tree)          │
│     └── --{{SYNC_HOME}}-Dev-foo--/                              │
│         └── session1.jsonl  (canonicalized content)             │
│                          │                                      │
│     ┌────────────────────┴───────────────────────┐              │
│     │           Chronicle (sync)                  │              │
│     └────────────────────┬───────────────────────┘              │
│              ┌───────────┴────────────┐                         │
│              ▼                        ▼                         │
│  ~/.pi/agent/sessions/     ~/.claude/projects/                  │
│  └── --Users-brad-…--/    └── -Users-brad-…/                    │
│      └── session1.jsonl       └── session1.jsonl                │
│      (de-canonicalized)       (de-canonicalized)                │
└─────────────────────────────────────────────────────────────────┘
```

### Data Flow — Outgoing (Local → Storage)

1. `chronicle sync` (fired by cron) detects new/changed `.jsonl` files in configured agent session directories
2. Canonicalizes file paths and file content (home paths → `{{SYNC_HOME}}` token)
3. Writes canonicalized files to the Git working tree under canonical directory names
4. Commits and pushes to remote

### Data Flow — Incoming (Storage → Local)

1. Chronicle pulls from remote
2. Merges any conflicts at the JSONL entry level (grow-only set)
3. De-canonicalizes file paths and content (`{{SYNC_HOME}}` → local home path)
4. Writes de-canonicalized files to the correct local agent session directories

---

## 4. Canonicalization

### 4.1 Token Format

```
{{SYNC_HOME}}
```

**Rationale:** Double-brace syntax is visually distinct, immediately recognizable as a placeholder (Handlebars/Jinja convention), does not conflict with shell expansion (`$HOME`), markdown (`~`), regex (`.`), or any standard tool convention. Easily greppable for debugging.

### 4.2 Canonicalization Levels

| Level | What is Canonicalized | Default |
|-------|----------------------|---------|
| **L1 — Paths** | File and directory paths (the sync destination on the filesystem) | Always on |
| **L2 — Structured Fields** | Whitelisted JSON field values within JSONL entries | On by default |
| **L3 — Freeform Text** | All string content in JSONL entries (including conversation text, tool output) | Off by default, opt-in with warnings |

Configuration: `canonicalization.level = 2` (default).

When L3 is enabled, the tool MUST emit a warning at startup:

```
⚠ WARNING: Level 3 canonicalization is enabled. All string content in session files
  will be scanned for home directory paths. This may alter conversation content,
  code snippets, and documentation references. Use with caution.
```

### 4.3 Level 2 — Whitelisted JSON Field Paths

The following JSON field paths are canonicalized when `level >= 2`:

```
cwd
path
file_path
message.cwd
arguments.path
arguments.file_path
arguments.command
```

Only values that **begin with the local home directory path** are replaced. UUIDs, entry IDs, and non-path strings are never touched, even if they happen to contain a matching substring.

**Matching rule:** A field value is a canonicalization candidate if and only if:
1. It is a string
2. It starts with `$HOME/` or equals `$HOME` exactly (where `$HOME` is the machine's home directory)
3. The match is at a path boundary (avoid partial matches like `/Users/bradmatic2`)

### 4.4 Path Canonicalization

**Directory names:**

| Agent | Local form | Canonical form |
|-------|-----------|----------------|
| Pi | `--Users-bradmatic-Dev-foo--` | `--{{SYNC_HOME}}-Dev-foo--` |
| Claude | `-Users-bradmatic-Dev-foo` | `-{{SYNC_HOME}}-Dev-foo` |

The encoded home path prefix is detected using the same encoded form:
- Pi encoding: `$HOME` with leading `/` stripped, all `/` → `-` → e.g., `Users-bradmatic`
- Claude encoding: `$HOME` with leading `/` stripped, all `/` and `.` → `-` → e.g., `Users-bradmatic`

The encoded prefix is replaced with the token form:
- Pi: `Users-bradmatic` → `{{SYNC_HOME}}`
- Claude: `Users-bradmatic` → `{{SYNC_HOME}}`

**File content:** Within JSONL files, L2 field values matching the home directory path are replaced:
```
"/Users/bradmatic/Dev/foo" → "{{SYNC_HOME}}/Dev/foo"
```

### 4.5 De-canonicalization

The reverse operation. On the receiving machine:
```
"{{SYNC_HOME}}/Dev/foo" → "/Users/brad/Dev/foo"
"--{{SYNC_HOME}}-Dev-foo--" → "--Users-brad-Dev-foo--"
```

### 4.6 Custom Tokens

Users may define additional canonicalization tokens for non-`$HOME` paths that vary across machines:

```toml
[canonicalization.tokens]
"{{SYNC_PROJECTS}}" = "/Users/bradmatic/Dev"
```

On another machine:
```toml
[canonicalization.tokens]
"{{SYNC_PROJECTS}}" = "/home/brad/projects"
```

Custom tokens follow the same matching rules as `{{SYNC_HOME}}` (path-boundary matching, L2 field whitelist). Custom tokens are applied **after** `{{SYNC_HOME}}` to avoid double-replacement (since a custom token's value may be under `$HOME`).

**Ordering:** De-canonicalization applies custom tokens **before** `{{SYNC_HOME}}` (reverse order of canonicalization) to ensure correct nesting.

### 4.7 Round-Trip Invariant

For any file content, the following MUST hold:

```
de_canonicalize(canonicalize(content, machine_A), machine_A) == content
```

And for cross-machine sync:

```
de_canonicalize(canonicalize(content, machine_A), machine_B)
```

produces content identical to the original except with Machine B's home path substituted in canonicalized positions.

---

## 5. Merge Algorithm

### 5.1 Data Model

Session files are append-only JSONL. Each line is a JSON object. The first line is a session header (`type: "session"`). Subsequent lines are entries (`type: "message"`, `type: "model_change"`, etc.) with a unique `id` field.

**Entry identity key:** The composite of `type` + `id` uniquely identifies an entry within a session file. The session header is identified by `type: "session"` alone (there is exactly one per file).

### 5.2 Grow-Only Set Merge

When two versions of the same session file exist (local and remote), the merge algorithm is:

```
1. Parse both files into sets of entries, keyed by (type, id)
2. VERIFY: all entries present in both sets with the same key are identical
   - If an entry with the same (type, id) exists in both but content differs:
     log a warning and prefer the remote version (it was committed first)
3. Compute the union of both entry sets
4. Order the result:
   a. Session header first (type == "session")
   b. All other entries sorted by timestamp (ISO 8601, ascending)
   c. Stable sort: entries with identical timestamps preserve relative order
     from the file they originated in (remote entries first for determinism)
5. Write the merged, ordered entries back as JSONL
```

### 5.3 Merge Scenarios

**New file (exists locally, not in repo):**
Canonicalize the entire file and add it to the repo. No merge needed.

**New file (exists in repo, not locally):**
De-canonicalize and write to the local filesystem. No merge needed.

**Appended file (exists in both, local has more entries):**
Merge via set-union. Since the file is append-only, the repo version should be a prefix of the local version. The verification step (5.2, step 2) confirms this.

**Divergent file (both sides appended different entries from a common ancestor):**
Merge via set-union. Both sides' new entries are included. The result is a valid session tree because both sets of entries branch from the same ancestor — Pi and Claude both support branching natively via `parentId`/`parentUuid` chains.

### 5.4 Prefix Verification

Before merging, verify that the common entries (entries present in both versions) are byte-identical after canonicalization. If they differ, this indicates a violated append-only invariant. The tool MUST:

1. Log a detailed warning identifying the file and the mismatched entries
2. Proceed with the merge using remote-version-wins for conflicting entries
3. Record the incident in the error ring buffer

### 5.5 Malformed Line Handling

If a JSONL line fails to parse:

1. Skip the malformed line
2. Log a warning with the file path, line number, and a snippet of the malformed content
3. Continue processing remaining lines
4. Record the incident in the error ring buffer

Rationale: A truncated write (disk full, power loss) typically corrupts only the last line. Skipping it preserves all valid data.

---

## 6. Git Storage Backend

### 6.1 Role of Git

Git is used as **transport and content-addressed storage only**. Chronicle owns all merge semantics — Git's built-in merge is never used. The workflow is:

1. `git fetch`
2. Chronicle performs entry-level merge between local working tree and fetched remote
3. `git add` + `git commit`
4. `git push`
5. If push is rejected (remote advanced), retry from step 1

### 6.2 Branch Strategy

Single branch: `main`. All machines commit directly to `main`. There are no per-machine branches. Git history provides full provenance (committer identity per machine — see §8.4).

### 6.3 Repo Structure

```
<repo_root>/
├── pi/
│   └── sessions/
│       ├── --{{SYNC_HOME}}-Dev-foo--/
│       │   ├── 2026-03-07T01-13-38-454Z_<uuid>.jsonl
│       │   └── 2026-03-08T14-20-00-153Z_<uuid>.jsonl
│       └── --{{SYNC_HOME}}-Dev-bar--/
│           └── ...
├── claude/
│   └── projects/
│       ├── -{{SYNC_HOME}}-Dev-foo/
│       │   ├── <uuid>.jsonl
│       │   └── subagents/
│       │       └── ...
│       └── -{{SYNC_HOME}}-Dev-bar/
│           └── ...
└── .chronicle/
    └── manifest.json          # metadata: last sync times, machine registry
```

The `pi/` and `claude/` top-level directories mirror the structure under each agent's session storage, but with canonicalized directory names.

The `.chronicle/manifest.json` file tracks:
```json
{
  "version": 1,
  "machines": {
    "cheerful-chinchilla": {
      "first_seen": "2026-03-28T10:00:00Z",
      "last_sync": "2026-03-28T15:30:00Z",
      "home_path": "{{SYNC_HOME}}",
      "os": "macos"
    }
  }
}
```

### 6.4 Commit Format

```
sync: cheerful-chinchilla @ 2026-03-28T15:30:00Z

+3 files, ~12 files (pi: 8, claude: 7)
```

Where `+` = new files, `~` = modified (appended) files. Batched by agent during initial import:

```
import: pi sessions (cheerful-chinchilla)

Added 633 session files from ~/.pi/agent/sessions/
```

### 6.5 Push Conflict Resolution

If `git push` fails because the remote has advanced:

1. Retry: `fetch` → re-merge → `commit` → `push`
2. Maximum **3 retries** with exponential backoff: 0s, 5s, 25s
3. After 3 failures: log error to ring buffer, wait for next cron-scheduled sync cycle

### 6.6 Initial Import

The `chronicle import` command performs a one-time import of all existing session files:

1. Scan all configured agent session directories
2. Canonicalize each file
3. Commit **one commit per agent session directory** for atomicity and readable git history
4. Push all commits

This is separate from the ongoing cron-scheduled sync cycle and is intended to be run once during setup.

---

## 7. Partial History Materialization

### 7.1 Concept

The Git repo always contains the **complete** canonicalized session history. Each machine may choose to materialize only a subset locally.

### 7.2 Configuration

```toml
[sync]
history_mode = "partial"         # "full" or "partial"
partial_max_count = 100          # max session files per agent session directory
```

When `history_mode = "partial"`:
- During the incoming sync (storage → local), only the N most recent session files per directory are de-canonicalized and written to the local filesystem
- "Most recent" is determined by the timestamp in the filename (Pi) or the earliest entry timestamp (Claude)
- Files outside the window are not deleted from the local filesystem if they already exist (no deletion propagation) — they are simply not created if they don't already exist locally

When `history_mode = "full"`:
- All session files are materialized locally

### 7.3 Outgoing Sync

Partial materialization does **not** affect outgoing sync. All local session files (including those outside the partial window) are always canonicalized and pushed to the repo. The partial setting only controls what is pulled down.

---

## 8. Configuration

### 8.1 File Location

```
~/.config/chronicle/config.toml
```

XDG-compliant. Respects `$XDG_CONFIG_HOME` if set.

### 8.2 Full Configuration Schema

```toml
# Chronicle configuration

[general]
# Machine identity — auto-generated on `chronicle init`, retrievable via `chronicle config machine-name`
machine_name = "cheerful-chinchilla"

# Sync interval (used by `chronicle schedule install` to set cron frequency)
sync_interval = "5m"

# Log level: trace, debug, info, warn, error
log_level = "info"

# Follow symlinks when scanning session directories
# ⚠ Security risk: symlinks can point outside expected directories
follow_symlinks = false

[notifications]
# Log sync errors to stderr (visible in cron mail if configured)
on_error = true
# Log every successful sync to stderr
on_success = false

[storage]
# Local git repository path (XDG-compliant default)
repo_path = "~/.local/share/chronicle/repo"

# Git remote URL — user-defined, chronicle assumes it already exists
remote_url = ""

# Git branch
branch = "main"

[canonicalization]
# Home directory token (not usually changed)
home_token = "{{SYNC_HOME}}"

# Canonicalization level: 1 = paths only, 2 = + whitelisted fields, 3 = + freeform text
level = 2

# Custom path tokens (applied after SYNC_HOME during canonicalization)
[canonicalization.tokens]
# "{{SYNC_PROJECTS}}" = "/Users/bradmatic/Dev"

[agents.pi]
enabled = true
session_dir = "~/.pi/agent/sessions"

[agents.claude]
enabled = true
session_dir = "~/.claude/projects"

[sync]
# "full" or "partial"
history_mode = "partial"

# When partial: max session files to materialize per directory
partial_max_count = 100
```

### 8.3 Configuration Precedence

1. CLI flags (highest priority)
2. Environment variables: `CHRONICLE_REPO_PATH`, `CHRONICLE_REMOTE_URL`, `CHRONICLE_SYNC_INTERVAL`
3. Config file
4. Built-in defaults (lowest)

### 8.4 Machine Identity

On `chronicle init`, a machine name is auto-generated using a `{adjective}-{animal}` scheme (inspired by Ubuntu release names). Examples: `cheerful-chinchilla`, `bold-barracuda`, `gentle-gecko`.

The machine name is used as the Git committer name for all commits from that machine:
```
Author: cheerful-chinchilla <chronicle@local>
```

Retrievable via:
```
$ chronicle config machine-name
cheerful-chinchilla
```

The user may override it in `config.toml` or via:
```
$ chronicle config machine-name "my-macbook"
```

---

## 9. CLI Interface

### 9.1 Command Reference

```
chronicle init
```
First-time setup. Creates `~/.config/chronicle/config.toml` with defaults, generates a machine name, initializes the local git repo at `repo_path`. Prompts for `remote_url` if not provided via flag. Does NOT import existing files (use `chronicle import` separately).

---

```
chronicle import [--agent pi|claude|all] [--dry-run]
```
One-time bulk import of existing session files into the canonical store. Scans configured agent session directories, canonicalizes all files, commits one commit per session directory, pushes to remote.

`--dry-run`: Show what would be imported without writing anything.

---

```
chronicle sync [--dry-run]
```
Single sync cycle: pull → merge → push → de-canonicalize → write local. This is the command that cron invokes on a schedule.

---

```
chronicle push [--dry-run]
```
Outgoing only: canonicalize local changes → commit → push. Does not pull or de-canonicalize.

---

```
chronicle pull [--dry-run]
```
Incoming only: fetch → merge → de-canonicalize → write local. Does not push local changes.

---

```
chronicle status
```
Display:
- Last successful sync time
- Pending local changes (new/modified files not yet pushed)
- Remote changes not yet pulled (requires a fetch)
- Current machine name
- Configured agents and their sync status

---

```
chronicle errors [--limit N]
```
Display the error ring buffer (last 30 entries by default). Each entry includes timestamp, error type, file affected, and a human-readable message.

---

```
chronicle config [key] [value]
```
View or edit configuration. Without arguments, prints the full config. With a key, prints that value. With key + value, sets it.

Special keys:
- `machine-name`: view/set the machine identity

---

```
chronicle schedule install
```
Install a crontab entry that runs `chronicle sync --quiet` at the interval specified by `sync_interval` in config (default: every 5 minutes). Also installs an `@reboot` entry so syncing resumes after a restart.

The installed crontab entries are tagged with a comment marker (`# chronicle-sync`) for reliable identification:
```
@reboot /usr/local/bin/chronicle sync --quiet  # chronicle-sync
*/5 * * * * /usr/local/bin/chronicle sync --quiet  # chronicle-sync
```

If entries already exist, they are updated in place (interval may have changed). The command auto-detects the chronicle binary path.

---

```
chronicle schedule uninstall
```
Remove all chronicle crontab entries (identified by the `# chronicle-sync` marker).

---

```
chronicle schedule status
```
Report whether the crontab entries are installed, the configured interval, and the path to the chronicle binary in the crontab.

---

### 9.2 Global Flags

| Flag | Description |
|------|-------------|
| `--config <path>` | Override config file location |
| `--verbose` / `-v` | Increase log verbosity |
| `--quiet` / `-q` | Suppress non-error output |

---

## 10. Scheduling

### 10.1 Cron-Based Scheduling

Chronicle uses cron for scheduling on both macOS and Linux. `chronicle sync` is a stateless CLI command — cron fires it on a schedule, it runs a single sync cycle, and exits.

`chronicle schedule install` writes two entries to the user's crontab:

```crontab
@reboot /usr/local/bin/chronicle sync --quiet  # chronicle-sync
*/5 * * * * /usr/local/bin/chronicle sync --quiet  # chronicle-sync
```

The `# chronicle-sync` comment marker allows `chronicle schedule uninstall` and `chronicle schedule status` to reliably identify and manage these entries without touching other crontab lines.

### 10.2 Interval Mapping

The `sync_interval` config value is mapped to a cron expression:

| `sync_interval` | Cron expression | Notes |
|-----------------|----------------|-------|
| `1m` | `* * * * *` | Every minute (minimum cron granularity) |
| `5m` (default) | `*/5 * * * *` | Every 5 minutes |
| `10m` | `*/10 * * * *` | Every 10 minutes |
| `15m` | `*/15 * * * *` | Every 15 minutes |
| `30m` | `*/30 * * * *` | Every 30 minutes |
| `1h` | `0 * * * *` | Every hour |

Values that don't map cleanly to cron (e.g., `7m`) are rounded down to the nearest cron-compatible interval and a warning is emitted.

### 10.3 Crontab Management

The `schedule install` command:

1. Reads the current crontab (`crontab -l`)
2. Removes any existing lines with the `# chronicle-sync` marker
3. Appends the new `@reboot` and interval entries
4. Writes the updated crontab (`crontab -`)
5. Prints confirmation with the installed schedule

The `schedule uninstall` command:

1. Reads the current crontab
2. Removes all lines with the `# chronicle-sync` marker
3. Writes the updated crontab
4. Prints confirmation

If the crontab is empty after removal, the crontab is deleted (`crontab -r`).

### 10.4 Logging

Since `chronicle sync` is a regular CLI invocation:

- **stdout/stderr** go to cron's default mail handling (typically local mail, or `/dev/null` if `MAILTO=""` is set)
- The `--quiet` flag suppresses stdout on success; only errors go to stderr
- All sync activity is also recorded in the error ring buffer (§11.1) for `chronicle errors` to display
- For richer log inspection, users can redirect cron output to a file:
  ```
  */5 * * * * /usr/local/bin/chronicle sync --quiet 2>> ~/.local/share/chronicle/sync.log
  ```

### 10.5 Why Cron

Cron was chosen over platform-native schedulers (launchd, systemd) because:

- **Single code path** for both macOS and Linux — no platform-specific plist/unit file generation
- **Zero build complexity** — no XML templating, no INI generation, no platform feature flags
- **Familiar to the target audience** — developers who use AI coding agents know cron
- **Debuggable** — `crontab -l` is simpler than `launchctl list` + `journalctl --user`
- **Sufficient reliability** — for a 5-minute sync interval on a developer workstation, cron's guarantees are adequate

Platform-native schedulers (launchd, systemd, Task Scheduler) may be added as an option in a future release if users need tighter OS integration (e.g., wake-from-sleep triggers, energy scheduling).

---

## 11. Error Handling

### 11.1 Error Ring Buffer

Chronicle maintains a ring buffer of the last **30 errors** in a local file:

```
~/.local/share/chronicle/errors.jsonl
```

Each entry:
```json
{
  "timestamp": "2026-03-28T15:30:00Z",
  "severity": "error",
  "category": "push_conflict",
  "file": "pi/sessions/--{{SYNC_HOME}}-Dev-foo--/session1.jsonl",
  "message": "Push rejected after 3 retries. Remote advanced during sync.",
  "detail": "Remote HEAD: abc1234, Local HEAD: def5678"
}
```

Categories:
- `push_conflict` — remote advanced, retries exhausted
- `malformed_line` — unparseable JSONL line skipped
- `prefix_mismatch` — append-only invariant violated
- `canonicalization_error` — path replacement failed
- `git_error` — git command failure (network, auth, etc.)
- `io_error` — filesystem read/write failure
- `disk_full` — write failed due to insufficient space

### 11.2 Disk Full / Incomplete Write Detection

If a write to the local filesystem fails mid-file:
1. Delete the partially-written file (avoid leaving corrupted state)
2. Log the error with the target path
3. Continue processing other files
4. Retry on next sync cycle

If a write to the git working tree fails:
1. `git checkout -- <file>` to restore the repo's version
2. Log and continue

### 11.3 Network Failure

If `git fetch` or `git push` fails due to network:
1. Log the error
2. Skip the current sync cycle
3. Retry on next cron-scheduled run
4. Do NOT retry immediately (the cron interval already handles retry cadence)

### 11.4 Permission Errors

If a file cannot be read due to permissions:
1. Log a warning
2. Skip that file
3. Continue with remaining files

### 11.5 File Permission Preservation

When writing de-canonicalized files to local agent directories, preserve the file permissions of the original file if it already exists locally. For new files (first materialization), use the default permissions of the agent's session directory (copy the mode from the parent directory or use `0644`).

---

## 12. Symlink Policy

By default, Chronicle does **not** follow symlinks when scanning session directories. If a session directory (e.g., `~/.pi/agent/sessions`) is itself a symlink, Chronicle will refuse to operate on it and log an error.

To enable symlink following:

```toml
[general]
follow_symlinks = true
```

When enabled, Chronicle logs a one-time warning at startup:

```
⚠ WARNING: follow_symlinks is enabled. Chronicle will follow symbolic links
  when scanning session directories. This may expose files outside the expected
  directory tree. Ensure your symlink targets are trusted.
```

---

## 13. Rust Architecture

### 13.1 Crate Structure

```
chronicle/
├── Cargo.toml
├── src/
│   ├── main.rs                    # CLI entry point (clap)
│   ├── cli/
│   │   ├── mod.rs
│   │   ├── init.rs
│   │   ├── import.rs
│   │   ├── sync.rs
│   │   ├── push.rs
│   │   ├── pull.rs
│   │   ├── status.rs
│   │   ├── errors.rs
│   │   ├── config.rs
│   │   └── schedule.rs
│   ├── config/
│   │   ├── mod.rs                 # Config loading, validation, precedence
│   │   ├── schema.rs              # Serde structs for config.toml
│   │   └── machine_name.rs        # Fun name generator
│   ├── canon/
│   │   ├── mod.rs
│   │   ├── canonicalize.rs        # Local → canonical transforms
│   │   ├── decanon.rs             # Canonical → local transforms
│   │   ├── tokens.rs              # Token registry (SYNC_HOME + custom)
│   │   ├── fields.rs              # L2 whitelisted field path walker
│   │   └── levels.rs              # L1/L2/L3 dispatch
│   ├── merge/
│   │   ├── mod.rs
│   │   ├── entry.rs               # Entry identity (type + id), parsing
│   │   ├── set_union.rs           # Grow-only set merge algorithm
│   │   └── verify.rs              # Prefix verification, mismatch detection
│   ├── git/
│   │   ├── mod.rs
│   │   ├── repo.rs                # Init, clone, working tree management
│   │   ├── fetch_push.rs          # Fetch, push with retry + backoff
│   │   └── commit.rs              # Staging, commit message formatting
│   ├── agents/
│   │   ├── mod.rs
│   │   ├── pi.rs                  # Pi-specific: dir encoding, file naming, schema knowledge
│   │   └── claude.rs              # Claude-specific: dir encoding, file naming, schema knowledge
│   ├── scheduler/
│   │   ├── mod.rs
│   │   └── cron.rs                # Crontab read/write/install/uninstall
│   ├── errors/
│   │   ├── mod.rs
│   │   ├── ring_buffer.rs         # 30-entry error ring buffer (JSONL file)
│   │   └── types.rs               # Error category enum, structured error type
│   └── scan/
│       ├── mod.rs
│       └── diff.rs                # Detect new/changed files vs. last sync state
```

### 13.2 Key Dependencies

| Crate | Purpose |
|-------|---------|
| `clap` | CLI argument parsing |
| `serde` + `serde_json` | JSONL parsing/serialization |
| `toml` | Config file parsing |
| `git2` (`libgit2` bindings) | Git operations (prefer over shelling out) |
| `chrono` | Timestamp parsing and comparison |
| `dirs` | XDG-compliant directory resolution |
| `uuid` | Session UUID generation |
| `rand` | Machine name generation |
| `tracing` | Structured logging |
| `thiserror` / `anyhow` | Error types |

### 13.3 Git Interaction: `git2` vs. CLI

Prefer `git2` (libgit2 Rust bindings) for all git operations. Advantages:
- No dependency on system `git` installation
- Programmatic error handling
- No shell escaping concerns

Fallback: if `git2` proves insufficient for a specific operation (e.g., SSH agent forwarding), shell out to `git` CLI with structured output parsing.

---

## 14. Sync Cycle — Detailed Flow

```
chronicle sync
├── 1. Load config
├── 2. Validate: repo exists, remote configured
├── 3. OUTGOING PHASE
│   ├── 3a. For each enabled agent:
│   │   ├── Scan local session directory for .jsonl files
│   │   ├── Compare against last-known state (file size/mtime cache)
│   │   └── Collect changed/new files
│   ├── 3b. For each changed/new file:
│   │   ├── Read local file
│   │   ├── Canonicalize content (L1 + L2, optionally L3)
│   │   ├── Compute canonical destination path in repo
│   │   ├── If file exists in repo: MERGE (§5.2)
│   │   └── Write canonicalized/merged result to repo working tree
│   └── 3c. If any changes: git add → commit
├── 4. GIT EXCHANGE
│   ├── 4a. git fetch origin main
│   ├── 4b. If remote has new commits:
│   │   ├── For each file changed in remote:
│   │   │   ├── If file also changed locally (in step 3): MERGE
│   │   │   └── Else: accept remote version
│   │   ├── Update working tree with merged state
│   │   └── git add → commit (merge commit)
│   └── 4c. git push (with retry, §6.5)
├── 5. INCOMING PHASE
│   ├── 5a. For each file in repo not yet materialized locally
│   │     (or with remote changes):
│   │   ├── Apply partial history filter (§7)
│   │   ├── De-canonicalize content
│   │   ├── Compute local destination path
│   │   └── Write to local filesystem (preserve permissions)
│   └── 5b. Update last-known state cache
└── 6. Update manifest.json with last_sync timestamp
```

### 14.1 Change Detection

To avoid re-processing unchanged files on every cycle, Chronicle maintains a local state cache:

```
~/.local/share/chronicle/state.json
```

```json
{
  "files": {
    "pi/sessions/--{{SYNC_HOME}}-Dev-foo--/session1.jsonl": {
      "local_mtime": "2026-03-28T15:00:00Z",
      "local_size": 524288,
      "last_synced_size": 524288,
      "local_path": "/Users/bradmatic/.pi/agent/sessions/--Users-bradmatic-Dev-foo--/session1.jsonl"
    }
  }
}
```

A file is considered changed if its mtime or size differs from the cached values. On first run (no cache), all files are processed.

---

## 15. Testing Requirements

### 15.1 Unit Tests

| Module | Test Cases |
|--------|-----------|
| `canon` | Round-trip: `decanon(canon(x)) == x` for all L1/L2/L3 combinations |
| `canon` | L2 whitelist: only whitelisted fields are touched |
| `canon` | Non-home paths are never modified |
| `canon` | Path-boundary matching: `/Users/bradmatic2` is NOT matched |
| `canon` | Custom tokens applied in correct order |
| `canon` | Token nesting: `{{SYNC_PROJECTS}}` under `{{SYNC_HOME}}` resolves correctly |
| `merge` | Two machines append different entries to different files → both get all files |
| `merge` | Two machines append to the **same** file → merged result contains both sets |
| `merge` | Session header preserved (exactly one, not duplicated) |
| `merge` | Entry ordering: merged entries sorted by timestamp |
| `merge` | Malformed line skipped, valid lines preserved |
| `merge` | Prefix verification: detect when append-only invariant is violated |
| `merge` | Idempotency: `merge(A, merge(A, B)) == merge(A, B)` |
| `agents` | Pi directory encoding round-trip |
| `agents` | Claude directory encoding round-trip |
| `config` | Precedence: CLI > env > file > defaults |
| `config` | Missing config file → sensible defaults |

### 15.2 Integration Tests

| Scenario | Description |
|----------|-------------|
| **Two-machine basic sync** | Machine A creates a session, syncs. Machine B syncs, sees the session. |
| **Concurrent append to different files** | A appends to file1, B appends to file2. Both sync. Both have both files with all entries. |
| **Concurrent append to same file** | A appends entries X to file1, B appends entries Y to file1. Both sync. Merged file contains X ∪ Y. Pi's parent chain forms a valid branch. |
| **Canonicalization round-trip** | File with home paths → canonicalize on Machine A → store → de-canonicalize on Machine B → verify paths are Machine B's home. |
| **Partial history** | Repo has 200 sessions in a directory. Machine with `partial_max_count = 50` only materializes the 50 most recent. |
| **Network failure during push** | Simulate remote unreachable. Verify: error logged, no data corruption, next sync succeeds when network returns. |
| **Network failure during fetch** | Same as above for the pull path. |
| **Disk full during local write** | Simulate `ENOSPC`. Verify: partial file is cleaned up, error logged, retry on next cycle succeeds when space is freed. |
| **Disk full during repo write** | Simulate `ENOSPC` on git working tree. Verify: repo state is restored (`git checkout`), error logged. |
| **Malformed JSONL line** | File with one corrupted line (truncated JSON). Verify: line is skipped, all other entries sync correctly. |
| **Malformed file (zero bytes)** | Empty file in session directory. Verify: skipped, warning logged. |
| **Push conflict exhaustion** | Three consecutive push rejections. Verify: error logged after 3 retries, no infinite loop, next cycle retries fresh. |
| **New machine bootstrap** | Machine C runs `chronicle init` + `chronicle import`. Verify: receives all sessions from A and B. |
| **Symlink refused** | Session dir is a symlink, `follow_symlinks = false`. Verify: error logged, no files processed. |
| **Symlink followed** | Session dir is a symlink, `follow_symlinks = true`. Verify: files behind symlink are processed, warning logged. |
| **L3 freeform canonicalization** | Session with home path in conversation text. L2: path unchanged. L3: path canonicalized. Verify both modes. |
| **Custom token** | `{{SYNC_PROJECTS}}` defined. File with project path. Verify canonicalization/de-canonicalization with different values on two machines. |
| **Import batching** | Import 50 files across 5 directories. Verify: 5 commits (one per directory), not 1 or 50. |
| **Idempotent sync** | Run `chronicle sync` twice with no changes. Verify: no new commits, no file writes. |

### 15.3 Property-Based Tests

Using `proptest` or `quickcheck`:

| Property | Description |
|----------|-------------|
| **Merge commutativity** | `merge(A, B) == merge(B, A)` |
| **Merge associativity** | `merge(merge(A, B), C) == merge(A, merge(B, C))` |
| **Merge idempotency** | `merge(A, A) == A` |
| **Canon round-trip** | For any valid JSONL content and any home path, `decanon(canon(x)) == x` |
| **Append-only merge superset** | `entries(merge(A, B)) ⊇ entries(A) ∪ entries(B)` |

---

## 16. Future Considerations (Not in MVP)

- **Deletion propagation:** Tombstone mechanism for intentional session removal
- **Windows support:** Task Scheduler integration (cron unavailable), different path separators
- **Platform-native scheduling:** Optional launchd (macOS) / systemd (Linux) integration for tighter OS hooks (wake-from-sleep, energy scheduling)
- **Encryption at rest:** Encrypt session content before committing to git (GPG or age)
- **Multiple remotes:** Sync to more than one git remote for redundancy
- **Web UI:** Browse synced session history in a browser
- **Compression:** Gzip or zstd compression for large session files in the repo
- **Agent plugins:** Support for additional agents beyond Pi and Claude Code
- **Selective sync:** Per-project sync rules (only sync sessions for certain projects)

---

## Appendix A: Glossary

| Term | Definition |
|------|-----------|
| **Canonicalize** | Replace machine-specific home directory paths with `{{SYNC_HOME}}` token |
| **De-canonicalize** | Replace `{{SYNC_HOME}}` token with the local machine's home directory path |
| **Session file** | A `.jsonl` file containing a sequence of JSON objects representing an agent conversation |
| **Entry** | A single JSON object (one line) within a session file |
| **Entry identity** | The composite key `(type, id)` that uniquely identifies an entry within a session file |
| **Grow-only set** | A CRDT where elements can be added but never removed; merge = set union |
| **Materialization** | The process of de-canonicalizing a file from the repo and writing it to the local agent session directory |
| **Partial materialization** | Only materializing a subset of session files (e.g., most recent N per directory) |
| **Machine name** | A fun auto-generated identifier for each machine (e.g., `cheerful-chinchilla`) |

---

## Appendix B: Canonical Token Reference

| Token | Meaning | Example (Machine A) | Example (Machine B) |
|-------|---------|---------------------|---------------------|
| `{{SYNC_HOME}}` | User's home directory | `/Users/bradmatic` | `/home/brad` |
| `{{SYNC_*}}` (custom) | User-defined path mapping | Configured per-machine in `config.toml` |

---

## Appendix C: Ralphi Init Gaps for Loop-Based Agents

An audit of the `ralphi-init` skill against the ralphi loop runtime (`loop-config.ts`, `loop-controller.ts`, `loop-engine.ts`, `loop-finalizer.ts`, `runtime.ts`) revealed several gaps where the init skill does not generate configuration that the loop runtime actively consumes or enforces. These gaps cause agents running in autonomous loops to operate without quality controls, safety boundaries, or behavioral directives that the runtime is designed to support.

### C.1 No `loop:` Section Generated

The runtime reads and enforces four loop controls from `.ralphi/config.yaml`:

```yaml
loop:
  guidance: "..."               # injected into system prompt every iteration
  reviewPasses: 1               # gate: phase_done rejected if agent reports fewer
  trajectoryGuard: "off"        # off | warn_on_drift | require_corrective_plan
  reflectEvery: null            # reflection checkpoint cadence (iterations)
  reflectInstructions: null     # custom reflection prompt
```

**Current state:** Init generates `project`, `commands`, `rules`, `boundaries`, and `engine` sections but never generates a `loop:` section. Agents running in loops get zero loop controls unless the user manually edits the config.

**Recommendation:** Generate a commented-out `loop:` section with inline documentation of each option:

```yaml
# loop:
#   guidance: "Project-specific instructions injected into every loop iteration"
#   reviewPasses: 1           # Require N self-review passes before completing (1-3)
#   trajectoryGuard: "off"    # off | warn_on_drift | require_corrective_plan
#   reflectEvery: 3           # Pause for structured reflection every N iterations
#   reflectInstructions: |    # Custom reflection prompt (default: scope/risk/plan)
#     Are we still aligned with the PRD?
```

**Priority:** P0 — agents miss all loop controls without this.

### C.2 `boundaries.never_touch` Is Dangerously Minimal

Init generates only `*.lock` and `.env*`. The loop agent can freely modify files that would corrupt the loop runtime or break the quality gate:

| Missing boundary | Risk |
|---|---|
| `.ralphi/runtime-state.json` | Corrupts loop orchestration state (phase runs, loop tracking) |
| `.git/hooks/*` | Agent could disable the pre-commit quality guard |
| `prek.toml` | Agent could remove the `ralphi check` hook |
| `LICENSE*`, `CHANGELOG*` | Unrelated files modified during loop iterations |
| `.git/` | Agent could corrupt the repository |

Note: `.ralphi/progress.txt` is intentionally written to by the loop agent (append-only) and should NOT be in `never_touch`. The boundary system does not currently distinguish append vs overwrite.

**Recommended additions:**

```yaml
boundaries:
  never_touch:
    - "*.lock"
    - ".env*"
    - ".ralphi/runtime-state.json"
    - ".git/"
    - "prek.toml"
    - "LICENSE*"
    - "CHANGELOG*"
```

**Priority:** P0 — agent can corrupt runtime state without these boundaries.

### C.3 Detected Commands Are Never Verified

Init reads `package.json` scripts (or `Cargo.toml`, `pyproject.toml`, etc.) to detect quality commands. The skill documentation notes:

> "A script named `test` that runs `echo "no tests"` is not a real test command."

However, init never actually runs the detected commands to verify they work. The runtime's prerequisite validator (`collectLoopPrerequisiteIssues`) only checks that **at least one command key exists** under `commands:` — it never executes them.

A single `ralphi check` dry run during init would catch:
- Scripts that are stubs (`echo "no tests yet"`)
- Missing dependencies (`vitest: command not found`)
- Broken builds or type errors

**Recommendation:** Add a verification step after writing `config.yaml` that executes each detected command once and reports pass/fail. Failures should be warnings (the user can fix them), not blockers.

**Priority:** P1 — broken quality gates are discovered too late (during loop iteration failures).

### C.4 No Preflight Validation as Final Init Step

The runtime provides `/ralphi-loop-validate` which runs `collectLoopPrerequisiteIssues()`:

1. Config file exists?
2. At least one command configured under `commands:`?
3. PRD valid JSON with `branchName` + non-empty `userStories[]`?
4. Git repository present?
5. Pre-commit hook runs `ralphi check`?

Init generates the config and pre-commit hook but never runs this validation to confirm everything is wired correctly. Adding a final step that runs the equivalent preflight would catch misconfigurations immediately rather than at loop start time.

**Recommendation:** Add a final init step that runs the preflight checks and reports the result, suggesting `/ralphi-loop-validate` for future verification.

**Priority:** P1 — misconfiguration caught late.

### C.5 Missing Loop-Aware Default Rules

The `rules:` section is injected into the system prompt as `[PROJECT CONFIG RULES]` during every loop iteration. Init only generates project-level convention rules (e.g., "Use vitest for testing", "Follow strict TypeScript"). Rules critical for loop behavior live exclusively in the loop `SKILL.md` and are not present in the config where the runtime injects them:

- `"Commit with message format: feat: [Story ID] - [Story Title]"`
- `"Read .ralphi/progress.txt Codebase Patterns section before starting work"`
- `"Run ralphi check after every code change"`
- `"Do NOT use git commit --no-verify"`

The loop skill instructs the agent to follow these rules, but they are only in the skill prompt — not in the config-level `rules:` array that persists across sessions and is enforced by the runtime's system prompt injection.

**Recommendation:** Include these as default rules when init detects the project is ralphi-enabled. Mark them with a comment so users can distinguish project rules from loop rules:

```yaml
rules:
  # Project conventions
  - "use vitest for testing"
  - "follow strict TypeScript (strict: true)"
  # Loop behavioral rules
  - "commit with message: feat: [Story ID] - [Story Title]"
  - "read .ralphi/progress.txt Codebase Patterns before starting"
  - "run ralphi check after every code change"
  - "never use git commit --no-verify"
```

**Priority:** P2 — loop behavioral rules not in system prompt injection path.

### C.6 No Interactive Loop Control Configuration in Step 4

Init's Step 4 uses `ralphi_ask_user_question` to confirm detected settings and ask for extra conventions. It does not surface loop control options. Users must discover `loop.reviewPasses`, `loop.trajectoryGuard`, and `loop.reflectEvery` by reading docs or source code.

**Recommendation:** Add a loop controls question to Step 4:

```json
{
  "id": "loop_controls",
  "prompt": "Configure loop quality controls?",
  "type": "single",
  "options": [
    "Default (1 review pass, no trajectory guard)",
    "Moderate (2 review passes, warn on drift)",
    "Strict (2 review passes, require corrective plan, reflect every 3 iterations)"
  ]
}
```

Map the selection to the appropriate `loop:` section values and generate them uncommented.

**Priority:** P2 — controls not discoverable.

### C.7 `max_retries: 3` Is Vestigial

Init generates `max_retries: 3` in the config. **The runtime never reads this field.** The loop controller uses `maxIterations` (from CLI args to `/ralphi-loop-start`). Retry logic for quality checks is not configurable — the pre-commit hook either passes or fails.

This field is misleading: users may expect it controls loop retry behavior, but it has no effect.

**Recommendation:** Either remove `max_retries` from init's generated config, or implement consumption in the runtime. If kept for forward compatibility, add a comment: `# (reserved — not yet consumed by runtime)`.

**Priority:** P3 — misleading dead config.

### C.8 AGENTS.md vs `loop.guidance` vs `rules:` — Undocumented Overlap

Three separate mechanisms inject agent directives, with overlapping purpose and no documentation on their relationship:

| Mechanism | Source | Injection Method | Scope |
|---|---|---|---|
| `AGENTS.md` | Git-committed file | Agent reads via file access during iteration | All agents, all contexts |
| `rules:` | `.ralphi/config.yaml` | Runtime injects into system prompt as `[PROJECT CONFIG RULES]` | Ralphi loop iterations |
| `loop.guidance` | `.ralphi/config.yaml` | Runtime injects into system prompt as `[PROJECT LOOP GUIDANCE]` | Ralphi loop iterations only |

An agent in a loop receives `rules:` and `loop.guidance` in its system prompt, and is instructed by the loop skill to read `AGENTS.md` from disk. Without guidance on what goes where, users duplicate content or miss one channel entirely.

**Recommendation:** Add comments in the generated config explaining the distinction:

```yaml
# rules: Project-wide conventions enforced via system prompt in all ralphi phases.
#   Use for: coding style, testing framework, import conventions.
rules:
  - "use vitest for testing"

# loop.guidance: Loop-specific behavioral directives injected only during loop iterations.
#   Use for: iteration strategy, focus areas, temporary constraints.
#   Complement AGENTS.md (which the agent reads from disk for broader project context).
# loop:
#   guidance: "Focus on backend stories first. Skip UI polish until US-010."
```

**Priority:** P3 — overlapping injection points with no documentation.

### C.9 Summary

| Priority | Gap | Impact |
|----------|-----|--------|
| **P0** | Generate commented `loop:` section (C.1) | Agents miss all loop controls |
| **P0** | Expand `boundaries.never_touch` (C.2) | Agent can corrupt runtime state |
| **P1** | Verify detected commands actually run (C.3) | Broken quality gates discovered too late |
| **P1** | Run preflight validation as final init step (C.4) | Misconfiguration caught early |
| **P2** | Add loop-aware default rules (C.5) | Loop behavioral rules not in system prompt |
| **P2** | Ask about loop control preferences in Step 4 (C.6) | Controls not discoverable |
| **P3** | Remove or implement `max_retries` (C.7) | Misleading dead config |
| **P3** | Document AGENTS.md vs loop.guidance vs rules (C.8) | Overlapping injection points |

---

*Spec version: 1.0 — 2026-03-28*
*Authored collaboratively via structured interview.*
