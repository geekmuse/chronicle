# Chronicle

[![CI](https://github.com/geekmuse/chronicle/actions/workflows/ci.yml/badge.svg)](https://github.com/geekmuse/chronicle/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)

> Bidirectional sync for AI coding agent session history across machines, with path canonicalization and Git-backed storage.

---

> [!WARNING]
> **ALPHA SOFTWARE — USE WITH CAUTION**
>
> Chronicle is alpha-quality software. It directly modifies AI agent session files on
> your machine. Bugs in the canonicalization, merge, or materialization logic could
> **corrupt or permanently delete your session history**.
>
> **Back up your existing sessions before installing or running Chronicle** (see
> [Before You Start](#before-you-start-back-up-your-existing-sessions) below).
>
> Chronicle is provided as-is, with no warranty of any kind. See [LICENSE](LICENSE).

---

## Before You Start: Back Up Your Existing Sessions

Chronicle modifies session files in-place during `import` and `pull`. Take a complete
snapshot of your session data **before** running any Chronicle command for the first time.

**Step 1 — Identify your session directories**

| Agent | Default session directory |
|-------|--------------------------|
| Pi | `~/.pi/agent/sessions/` |
| Claude Code | `~/.claude/projects/` |

> These directories may not both exist if you only use one agent.

**Step 2 — Create a dated backup**

```bash
# Back up Pi sessions (skip if you don't use Pi)
cp -r ~/.pi/agent/sessions/ ~/chronicle-backup-pi-$(date +%Y%m%d)/

# Back up Claude Code sessions (skip if you don't use Claude Code)
cp -r ~/.claude/projects/ ~/chronicle-backup-claude-$(date +%Y%m%d)/
```

**Step 3 — Verify the backup**

```bash
# Confirm the backup directories exist and are non-empty
ls -lh ~/chronicle-backup-pi-$(date +%Y%m%d)/ 2>/dev/null
ls -lh ~/chronicle-backup-claude-$(date +%Y%m%d)/ 2>/dev/null
```

**Step 4 — Store the backup somewhere safe**

Copy the backup directories to an external drive, cloud storage, or any location
outside `$HOME` before proceeding. Do not rely on the backup being in `$HOME` — if
something goes wrong you want it clearly separated.

> **Keep these backups.** Do not delete them until you have been running Chronicle
> successfully across multiple machines for at least a week and have confirmed your
> session history is intact.

---

## Overview

Chronicle synchronizes Pi and Claude Code session history across multiple machines
where `$HOME` paths differ. It uses a canonicalization layer to abstract away
per-machine path differences and Git as the storage and transport backend. Session
files are merged using a grow-only CRDT (set-union), preserving the append-only
invariant of JSONL session data.

## Features

- **Cross-machine sync** — Session history follows you between machines with different `$HOME` paths
- **Path canonicalization** — `$HOME` paths are replaced with `{{SYNC_HOME}}` tokens, with configurable canonicalization levels (paths, structured fields, freeform text)
- **CRDT merge** — Grow-only set merge ensures no session data is ever lost, even with concurrent edits on different machines
- **Partial materialization** — Pull only the N most recent sessions per project, while the Git repo retains complete history
- **Agent-agnostic** — Supports Pi and Claude Code with extensible agent architecture
- **Stateless CLI** — No daemon; a simple CLI invoked by cron on a configurable schedule
- **Rich `status` command** — Human-friendly (✓/⚠/✗) and machine-readable (`--porcelain`) output covering last-sync time/duration/operation, pending-file count, lock state, scheduler health, and per-agent sessions-dir existence; `--verbose` expands file lists and effective config values
- **`doctor` command** — Pre-flight health check across Config, Git, Agents, and Scheduler subsystems; plain-English remediation hints; `--porcelain` for scripting; exit codes 0/1/2 (pass/warn/error)
- **Fuzz-tested canonicalization** — A `cargo-fuzz` / libFuzzer target (`fuzz/fuzz_targets/fuzz_roundtrip.rs`) verifies the L2/L3 round-trip invariant against arbitrary inputs; runs weekly in CI (`fuzz.yml`) for 60 seconds with zero-crash enforcement; `fuzz-build` step runs on every PR

---

## Installation

### Option 1: Pre-built Binaries (Recommended)

When the GitHub Actions CI pipeline is active, pre-built binaries are attached to
each [GitHub Release](https://github.com/geekmuse/chronicle/releases). Download
the binary for your platform:

| Platform | Binary |
|----------|--------|
| Linux x86-64 | `chronicle-x86_64-unknown-linux-gnu` |
| Linux ARM64 | `chronicle-aarch64-unknown-linux-gnu` |
| macOS Intel | `chronicle-x86_64-apple-darwin` |
| macOS Apple Silicon | `chronicle-aarch64-apple-darwin` |

```bash
# Example: macOS Apple Silicon
curl -L https://github.com/geekmuse/chronicle/releases/latest/download/chronicle-aarch64-apple-darwin \
  -o /usr/local/bin/chronicle
chmod +x /usr/local/bin/chronicle
```

> **Note:** If no release binaries exist yet, use Option 2 below.

### Option 2: From Source

```bash
# Prerequisites: Rust stable (https://rustup.rs)
git clone https://github.com/geekmuse/chronicle.git
cd chronicle

# Install into ~/.cargo/bin (must be on your PATH)
cargo install --path .
```

**After install, verify it works:**

```bash
chronicle --version
```

---

## Backend Repository Setup

Chronicle uses a private Git repository as the sync backend — your session history
is stored there in canonicalized form and exchanged between machines via normal
Git push/pull.

### Why it must be private

Session files contain the full text of your conversations with AI coding agents,
including code, file paths, and potentially sensitive details about your projects.
**The backend repository must be private.** Never use a public repository.

### Step 1 — Create a private repository

Create an empty **private** repository on GitHub, GitLab, Gitea, or any Git host
you control. Do not initialize it with a README, `.gitignore`, or any other files —
Chronicle will set up the repository contents itself.

```
GitHub:  https://github.com/new   → set to Private
GitLab:  https://gitlab.com/projects/new → set Visibility to Private
```

### Step 2 — Configure SSH access

Chronicle uses [libgit2](https://libgit2.org/) for all Git operations. **libgit2 is
not `~/.ssh/config`-aware** — it ignores `IdentityFile`, `Host` blocks, and other
ssh config directives entirely. All SSH authentication goes through the SSH agent
protocol via `SSH_AUTH_SOCK`.

This means **your SSH key must be loaded in a running `ssh-agent`** before Chronicle
can push or pull.

#### macOS

macOS ships a Keychain-integrated SSH agent managed by `launchd`. Use it to load
your key once; it will survive reboots automatically:

```bash
# Add your key to macOS Keychain (done once)
ssh-add --apple-use-keychain ~/.ssh/id_ed25519

# Verify the key is loaded
ssh-add -l
```

If `ssh-add -l` returns `The agent has no identities`, your key is not loaded. Run
the `--apple-use-keychain` command above.

#### Linux

Most desktop environments start an SSH agent automatically. If you are on a headless
server or your agent is not running:

```bash
# Start an agent for the current shell session
eval $(ssh-agent -s)

# Add your key
ssh-add ~/.ssh/id_ed25519

# For persistence across sessions, add to ~/.bashrc / ~/.zshrc:
# if [ -z "$SSH_AUTH_SOCK" ]; then
#   eval $(ssh-agent -s)
#   ssh-add ~/.ssh/id_ed25519
# fi
```

For systemd-based systems you can also use `systemd --user` to run a persistent
agent socket at `/run/user/$(id -u)/ssh-agent.socket`. Chronicle's cron entries
fall back to this path automatically on Linux.

#### Verify SSH access to your backend repo

```bash
# Test that SSH auth works before configuring Chronicle
ssh -T git@github.com       # GitHub
ssh -T git@gitlab.com       # GitLab
```

### Step 3 — Note the SSH remote URL

Use the SSH remote URL (not HTTPS) for your backend repo, for example:

```
git@github.com:yourname/chronicle-sessions.git
```

Chronicle will push to this URL. Using HTTPS is technically possible but requires
credential helpers to be configured separately; SSH via the agent is the recommended
and tested path.

---

## Quick Start

```bash
# 1. First-time setup — creates config at ~/.config/chronicle/config.toml,
#    generates a machine name, and initializes the local mirror repo
chronicle init

# 2. Set your backend remote URL (the private repo you created above)
chronicle config set general.remote_url git@github.com:yourname/chronicle-sessions.git

# 3. Import existing session history (one-time, before first sync)
#    This stages all current sessions into the local repo without pushing
chronicle import

# 4. Run a manual sync to push your history to the remote
chronicle sync

# 5. (Optional) Install the cron schedule for automatic background sync
chronicle schedule install    # runs every 5 minutes by default
```

---

## Usage

```bash
# First-time setup — creates config, generates machine name, inits local repo
chronicle init

# Import existing session history (one-time, before first sync)
chronicle import

# Run a single sync cycle (fetch → merge → push)
chronicle sync

# Push local commits to the remote without a full sync
chronicle push

# Pull and materialise the latest remote sessions locally
chronicle pull

# Check sync status (human-friendly output)
chronicle status

# Verbose: show pending file paths and effective config values
chronicle status --verbose

# Machine-readable key=value output for scripts
chronicle status --porcelain

# Pre-flight health check (Config, Git, Agents, Scheduler)
chronicle doctor

# Doctor with machine-readable key=value output
chronicle doctor --porcelain

# View recent sync errors
chronicle errors

# Show or change a config value
chronicle config get canonicalization.level
chronicle config reset canonicalization.level

# Install / remove / check the cron schedule
chronicle schedule install    # runs every 5 minutes by default
chronicle schedule uninstall
chronicle schedule status
```

---

## Known Setup Gotchas

**Project directory paths must be consistent across machines**

Chronicle canonicalizes your home directory (`$HOME` → `{{SYNC_HOME}}`), but it
does **not** automatically handle differences in the path structure beneath it.
If your projects live at `~/Dev/` on one machine and `~/projects/` on another,
Chronicle will treat them as entirely separate project trees — sessions will not
merge, and both machines will accumulate independent histories that never converge.

For example:

| Machine | Raw path | Canonical form |
|---------|----------|----------------|
| A | `/Users/alice/Dev/myproject` | `{{SYNC_HOME}}/Dev/myproject` |
| B | `/home/alice/projects/myproject` | `{{SYNC_HOME}}/projects/myproject` |

These are different canonical paths. Chronicle will never merge their sessions.

**The simplest fix:** use the same sub-`$HOME` path layout on every machine
(e.g., always `~/Dev/`, always `~/code/`, etc.).

**If your paths already differ:** define a custom token that maps each machine's
projects root to the same canonical name. In each machine's
`~/.config/chronicle/config.toml`:

```toml
# Machine A  (~/.config/chronicle/config.toml)
[canonicalization.tokens]
"{{SYNC_PROJECTS}}" = "/Users/alice/Dev"
```

```toml
# Machine B  (~/.config/chronicle/config.toml)
[canonicalization.tokens]
"{{SYNC_PROJECTS}}" = "/home/alice/projects"
```

With this in place, both paths canonicalize to `{{SYNC_PROJECTS}}/myproject`
and sessions will merge correctly. The token value is machine-local and never
stored in the shared repository.

> **Note:** Custom tokens only help for sessions created *after* the token is
> configured. Pre-existing sessions already stored under mismatched paths will
> remain separate. Plan your directory layout before the first sync.

---

**SSH agent not available in cron**

Chronicle's `schedule install` command generates cron entries that automatically
discover and forward `SSH_AUTH_SOCK` at runtime, so you should not need to do
anything special. However, if `chronicle status` shows auth errors after installing
the schedule:

- **macOS:** Make sure your key is added to Keychain (`ssh-add --apple-use-keychain`).
  The cron entry uses a `find`-based socket discovery trick that locates the
  Keychain agent socket even in the stripped cron environment.
- **Linux:** The cron entry falls back to the systemd user SSH agent socket
  (`/run/user/$(id -u)/ssh-agent.socket`). Ensure your SSH key is loaded there.

**`chronicle init` fails with "remote already exists"**

The local mirror repo already has a remote configured. Run
`chronicle config set general.remote_url <url>` instead of re-running `init`.

**Large repos make the first `sync` slow**

The initial `chronicle import` can be slow on machines with many sessions (thousands
of `.jsonl` files). This is a one-time cost. Subsequent syncs are fast because the
state cache (`materialize-state.json`) tracks what has already been processed.

**Cron overlap / `index.lock` errors**

Chronicle acquires an advisory file lock (`chronicle.lock`) before each sync so
that overlapping cron invocations exit cleanly rather than crashing with git index
errors. The lock file is automatically deleted when the sync exits cleanly. If
`chronicle status` or `chronicle doctor` shows a stale lock warning, the holding
process has already exited and the file will be cleared on the next sync run. If
the warning persists and no sync is running, delete `<repo-parent>/chronicle.lock`
manually.

**Canonicalization level**

The `canonicalization.level` config key controls how aggressively paths are
replaced in session data. Changing this on an already-synced repo can cause
re-canonicalization conflicts. Do not change the level after your first successful
sync unless you understand the implications and are prepared to re-import.

---

## Documentation

Detailed documentation lives in the [`docs/`](docs/) directory:

| Section | Path | Description |
|---------|------|-------------|
| Architecture | [`docs/001-architecture.md`](docs/001-architecture.md) | System design and key decisions |
| Development Guide | [`docs/002-development-guide.md`](docs/002-development-guide.md) | How to develop, test, and contribute |
| Doc Standards | [`docs/003-documentation-standards.md`](docs/003-documentation-standards.md) | How docs are structured and maintained |
| Specs | [`docs/specs/`](docs/specs/) | Feature specifications and design docs |
| ADRs | [`docs/adrs/`](docs/adrs/) | Architecture Decision Records |
| References | [`docs/references/`](docs/references/) | CLI reference, config reference, glossary |
| Tasks | [`docs/tasks/`](docs/tasks/) | Work items and implementation plans |
| Research | [`docs/research/`](docs/research/) | Spikes, investigations, POC write-ups |

## Development

```bash
# Clone the repository
git clone https://github.com/geekmuse/chronicle.git
cd chronicle

# Build
cargo build

# Run tests
cargo test

# Run linter
cargo clippy -- -D warnings

# Run the libFuzzer fuzz target for 30 seconds (requires nightly)
cargo +nightly fuzz run fuzz_roundtrip -- -max_total_time=30
```

See [Development Guide](docs/002-development-guide.md) for full details.

## Contributing

1. Fork the repository
2. Create a feature branch (`git checkout -b feat/amazing-feature`)
3. Commit using [conventional commits](https://www.conventionalcommits.org/) (`git commit -m 'feat: add amazing feature'`)
4. Push to the branch (`git push origin feat/amazing-feature`)
5. Open a Pull Request

Please read [AGENTS.md](AGENTS.md) for project conventions and [docs/002-development-guide.md](docs/002-development-guide.md) for the full development workflow.

## Versioning

This project uses [Semantic Versioning](https://semver.org/). See [CHANGELOG.md](CHANGELOG.md) for release history.

## License

MIT — see [LICENSE](LICENSE) for details.

## Author

Brad Campbell
