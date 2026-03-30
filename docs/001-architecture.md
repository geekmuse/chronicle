---
date_created: 2026-03-29
date_modified: 2026-03-30
status: active
audience: both
cross_references:
  - docs/002-development-guide.md
  - docs/specs/001-initial-delivery.md
  - AGENTS.md
---

# Architecture — Chronicle

## Overview

Chronicle is a Rust CLI tool that synchronizes AI coding agent session history (Pi, Claude Code) across machines where `$HOME` paths differ, using path canonicalization and Git as the storage/transport backend.

## System Context

```
┌─────────────┐     ┌──────────────────┐     ┌─────────────┐
│  Machine A   │────▶│    Chronicle     │────▶│ Git Remote  │
│  (sessions)  │◀────│  (sync cycle)   │◀────│  (storage)  │
└─────────────┘     └──────────────────┘     └─────────────┘
                            │
                            ▼
                    ┌──────────────┐
                    │  Machine B   │
                    │  (sessions)  │
                    └──────────────┘
```

- **Users:** Developers using Pi and/or Claude Code across multiple machines
- **External systems:** Git remote (user-provided), local agent session directories
- **Transport:** Git fetch/push over SSH or HTTPS

## High-Level Components

| Component | Responsibility | Key Files |
|-----------|---------------|-----------|
| CLI | Command parsing and dispatch | `src/main.rs`, `src/cli/` |
| Config | Config loading, validation, precedence (CLI > env > file > defaults) | `src/config/` |
| Canonicalization | Replace machine-specific `$HOME` paths with `{{SYNC_HOME}}` token and reverse | `src/canon/` |
| Merge | Grow-only set merge of JSONL session entries | `src/merge/` |
| Git | Repo init, fetch, push with retry, commit formatting, SSH/HTTPS credential callbacks | `src/git/` |
| Agents | Pi and Claude-specific directory encoding and file naming | `src/agents/` |
| Scheduler | Crontab installation and management | `src/scheduler/` |
| Scan | File change detection via mtime/size cache | `src/scan/` |
| Errors | Ring buffer (30 entries) for structured error logging | `src/errors/` |

## Data Flow

### Outgoing (Local → Git Remote)

1. Cron fires `chronicle sync`
2. Scanner detects changed `.jsonl` files via mtime/size cache
3. Canonicalizer replaces `$HOME` paths with `{{SYNC_HOME}}` (L1 paths + L2 whitelisted fields)
4. Merger performs set-union if file exists in repo
5. Git module commits and pushes

### Incoming (Git Remote → Local)

1. Git module fetches from remote
2. Merger resolves any divergent entries (set-union, remote-wins for conflicts)
3. Partial history filter limits materialization to N most recent files per directory
4. De-canonicalizer replaces `{{SYNC_HOME}}` with local `$HOME`
5. Files written to agent session directories with preserved permissions

## Key Design Decisions

| Decision | Rationale | ADR |
|----------|-----------|-----|
| Rust | Performance for JSONL parsing, strong type safety, single binary distribution | — |
| Git as storage backend | Content-addressed, distributed, users already have remotes | — |
| Grow-only CRDT merge | Session files are append-only; set-union is commutative, associative, idempotent | — |
| `{{SYNC_HOME}}` token | Visually distinct, doesn't conflict with shell/markdown/regex | — |
| Cron for scheduling | Single cross-platform code path for macOS + Linux, zero build complexity | — |
| `git2` (libgit2) over CLI | No system git dependency, programmatic error handling, explicit SSH agent credentials callback | — |

> For detailed decision records, see [`docs/adrs/`](adrs/).

## Dependencies

### Runtime

| Dependency | Purpose | Version |
|------------|---------|---------|
| `clap` | CLI argument parsing | latest |
| `serde` + `serde_json` | JSONL parsing/serialization | latest |
| `toml` | Config file parsing | latest |
| `git2` | Git operations (libgit2 bindings) | latest |
| `chrono` | Timestamp parsing and comparison | latest |
| `dirs` | XDG-compliant directory resolution | latest |
| `tracing` | Structured logging | latest |
| `thiserror` / `anyhow` | Error types | latest |
| `uuid` | Session UUID generation | latest |
| `rand` | Machine name generation | latest |

### Development

| Tool | Purpose |
|------|---------|
| `cargo fmt` | Code formatting (rustfmt) |
| `cargo clippy` | Linting (pedantic) |
| `cargo test` | Unit + integration tests |
| `cargo deny` | License compliance, security advisories |
| `proptest` | Property-based testing |

## Constraints & Non-Goals

### Constraints
- macOS and Linux only (Windows deferred)
- Must work with any Git remote the user provides (GitHub, GitLab, Gitea, self-hosted)
- No long-running daemon — stateless CLI invoked by cron
- Session files are append-only JSONL; merge must preserve this invariant

### Non-Goals (Explicit)
- Syncing agent settings, extensions, themes, or packages
- Syncing project-local files (`CLAUDE.md`, `.ralphi/`, `.pi/`)
- Deletion propagation (files are never removed from canonical store)
- Windows support (deferred to future release)
- Encryption at rest (future consideration)

## Future Considerations

- Deletion propagation via tombstone mechanism
- Windows support with Task Scheduler
- Platform-native scheduling (launchd/systemd) for tighter OS integration
- Encryption at rest (GPG or age)
- Selective per-project sync rules
- Web UI for browsing synced session history
