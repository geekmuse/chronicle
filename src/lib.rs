//! # chronicle
//!
//! `chronicle` synchronises [Pi](https://github.com/mariozechner/pi) and
//! [Claude Code](https://github.com/anthropics/claude-code) session history
//! across machines where `$HOME` paths differ.
//!
//! ## How it works
//!
//! 1. **Canonicalize** — session files are rewritten so that every embedded
//!    `$HOME` path is replaced by a portable token
//!    (e.g. `{{SYNC_HOME}}`).  Three levels of canonicalization are
//!    supported: L1 (file-system paths), L2 (selected JSON fields), and L3
//!    (freeform text).
//!
//! 2. **Merge** — JSONL session files are merged as grow-only sets: new
//!    lines are appended; duplicate lines are discarded.  Ordering within
//!    a session is preserved.
//!
//! 3. **Git transport** — a dedicated bare repository (or a normal one
//!    configured via `storage.repo_path`) acts as the sync store.  Push and
//!    pull operations use `libgit2` with automatic retry on rejection.
//!
//! ## Library vs binary
//!
//! This crate is **primarily a binary tool** (`chronicle`).  The library
//! target exists solely to enable integration tests in `tests/`.  All
//! modules are re-exported as `pub` so the integration-test crate can
//! import the testable `*_impl` helpers directly.

pub mod agents;
pub mod canon;
pub mod cli;
pub mod config;
pub mod errors;
pub mod git;
pub mod merge;
pub mod scan;
pub mod scheduler;
