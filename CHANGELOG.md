# Changelog

All notable changes to **chronicle** will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

### Fixed

### Changed

## [0.2.0] - 2026-03-29

### Added
- **Config module** — TOML schema with per-agent settings, machine-name generation (adjective-animal), XDG-compliant config path, CLI/env/file/default precedence chain
- **Canonicalization engine** — L1 `$HOME` path canonicalization, L2 whitelisted-field JSONL walker, L3 freeform text scan, `{{SYNC_HOME}}` token, custom token registry, full round-trip de-canonicalization
- **Merge module** — JSONL entry identity (type + id), grow-only set-union merge preserving append-only invariant, prefix verification to detect tampered history
- **Git module** — repo initialization, working tree management, fetch/push with exponential backoff retry, conventional-commit message formatting, staging
- **Agent modules** — Pi and Claude Code directory path encoding/decoding and session file naming conventions
- **CLI commands** — `chronicle init`, `import`, `sync`, `push`, `pull`, `status`, `errors`, `config`, `schedule install/uninstall/status`
- **Partial history materialization** — pull only the N most recent sessions per project while retaining full Git history
- **File scanner** — mtime/size cache for detecting changed `.jsonl` files without full re-scan
- **Scheduler** — crontab read/write/install/uninstall/status for macOS and Linux
- **Error ring buffer** — 30-entry structured error log persisted to disk
- **`src/lib.rs`** — library root exposing all modules for integration test access
- **Integration tests** — 8 end-to-end multi-machine scenario tests in `tests/integration.rs`
- **Property-based tests** — `proptest` round-trip tests for canonicalization and merge commutativity/idempotency
- Initial project scaffold with documentation and conventions
- `README.md` — project overview, setup, and usage
- `AGENTS.md` — agent-facing development guidance
- `CLAUDE.md` — Claude-specific coding instructions
- `docs/001-architecture.md` — system architecture
- `docs/002-development-guide.md` — development workflow
- `docs/003-documentation-standards.md` — documentation conventions
- `docs/specs/001-initial-delivery.md` — full project specification (v1.0)
- `.editorconfig` — cross-editor formatting rules
- `.gitattributes` — Git line-ending normalization
- `Cargo.toml` — Rust package manifest

## [0.1.0] - 2026-03-29

### Added
- Project initialized

[Unreleased]: ssh://git@git.bradleyscampbell.net:10022/geekmuse/chronicle.git/compare/v0.2.0...HEAD
[0.2.0]: ssh://git@git.bradleyscampbell.net:10022/geekmuse/chronicle.git/compare/v0.1.0...v0.2.0
[0.1.0]: ssh://git@git.bradleyscampbell.net:10022/geekmuse/chronicle.git/releases/tag/v0.1.0
