---
date_created: 2026-03-29
date_modified: 2026-03-30
status: active
audience: both
cross_references:
  - docs/001-architecture.md
  - docs/002-development-guide.md
  - docs/specs/001-initial-delivery.md
  - AGENTS.md
---

# Codebase Audit — Chronicle v0.2.2 (resolved in v0.3.0)

Comprehensive audit covering: implementation drift, potential bugs,
documentation issues, dead/unreachable code, and consistency issues.

---

## Issues

### Critical

**(none)**

---

### High

#### H-1 · `bug` · `src/cli/mod.rs:1668`
**`status_impl` hardcodes `"main"` instead of using `cfg.storage.branch`**

`status_impl` calls `RepoManager::init_or_open(&repo_path, None, "main")`
with a hardcoded `"main"` instead of reading `cfg.storage.branch`. If a user
configures `storage.branch = "trunk"`, the status command would open the repo
with the wrong branch, potentially misreading the manifest.

**Suggested fix:** Pass `&cfg.storage.branch` instead of `"main"`.

---

#### H-2 · `bug` · `src/cli/mod.rs:1943–1955`
**`set_config_value` accepts any `u8` for `canonicalization.level` — no range validation**

The error message says "expected a number 1–3" but the code accepts any `u8`
(0, 4, 255 all succeed). `canonicalize_line` only branches on `< 2`, `== 2`,
and `>= 3`, so level 0 silently disables canonicalization and levels 4–255
behave identically to level 3 — neither of which is documented.

**Suggested fix:** Add a `1..=3` range check after the parse:
```rust
let level = value.parse::<u8>()...;
if !(1..=3).contains(&level) { bail!("..."); }
```

---

#### H-3 · `bug` · `src/errors/ring_buffer.rs` (global path) / `src/cli/mod.rs:622,905,1133`
**Error ring buffer still uses `RingBuffer::default_path()` (global XDG path)**

The state cache was fixed to use a repo-relative path (`path_for_repo`), but
the error ring buffer still resolves to the global
`~/.local/share/chronicle/errors.jsonl` in `sync_impl`, `push_impl`, and
`pull_impl`. If a user runs two Chronicle installations with different repo
paths (or in tests), errors from both write to the same file — same class of
race condition that was fixed for the state cache.

**Suggested fix:** Add a `RingBuffer::path_for_repo()` (or derive the path
from `repo_path`) and use it in the CLI command impls.

---

#### H-4 · `bug` · `src/agents/mod.rs:147–156` (ClaudeAgent::decode_dir)
**Claude decode is lossy — `.` and `/` both encode to `-`, round-trip fails for dotfiles**

`ClaudeAgent::encode_dir` replaces both `/` and `.` with `-`, but `decode_dir`
converts every `-` back to `/`. A path like `/Users/brad/.config/foo` encodes
to `-Users-brad--config-foo`, which decodes to `/Users/brad//config/foo`
(double slash). The doc comment acknowledges this ("best-effort inverse") but
the `Agent::decode_dir` trait doc does not mention lossiness, and the
`ChronicleError::CanonicalizationError` return type implies decoding should
succeed or fail cleanly — not silently produce incorrect paths.

**Suggested fix:** Document this prominently in the `Agent` trait doc, and
consider whether `decode_dir` for Claude should return a `Result` with a
warning when the decoded path contains patterns that look like encoding
artifacts (`//`).

---

### Medium

#### M-1 · `doc-drift` · `AGENTS.md:55–65` / `docs/002-development-guide.md`
**Repository structure in AGENTS.md still shows the pre-implementation layout**

`AGENTS.md` lists sub-files like `src/cli/init.rs`, `src/cli/import.rs`, etc.
but the actual implementation is a single `src/cli/mod.rs`. The dev guide
(`docs/002-development-guide.md`) was corrected during the post-loop wrap-up
but AGENTS.md was not.

**Suggested fix:** Update the tree in AGENTS.md to match the actual layout
(same as docs/002).

---

#### M-2 · `consistency` · `src/cli/mod.rs` vs `src/config/mod.rs`
**Two different `expand_home` / `expand_path` functions with overlapping behaviour**

`config::expand_path(&str) -> PathBuf` uses `dirs::home_dir()` for `~`
expansion. `cli::expand_home(&str, &Path) -> PathBuf` does the same thing
but accepts an explicit `home` parameter for testability. Several CLI functions
use `config::expand_path` (which uses the real $HOME) while `status_impl`
uses `expand_home` (which respects the injected test home). This creates a
subtle inconsistency: `sync_impl`, `push_impl`, and `pull_impl` call
`config::expand_path` — they are **not** fully testable with injected home dirs.

**Suggested fix:** Consolidate on a single `expand_path(path, home)` that
accepts a home parameter, and have `config::expand_path` delegate to it
with `dirs::home_dir()` for the production caller.

---

#### M-3 · `doc-inaccurate` · `docs/002-development-guide.md` (§ CI/CD)
**CI section describes recommendations but CI workflows now exist**

The dev guide says "A CI pipeline **should** include:" (future tense / aspirational)
but `.github/workflows/ci.yml` and `.forgejo/workflows/ci.yml` now exist.
The section should reference the actual workflows.

**Suggested fix:** Update the section to say "CI is configured in
`.github/workflows/` and `.forgejo/workflows/`" and describe what each
workflow does.

---

#### M-4 · `consistency` · `src/scan/mod.rs:85–91` vs `src/scan/mod.rs:109–116`
**`StateCache::default_path()` is still called from `status_impl` for the
ring buffer but not for the state cache**

After the state-cache fix, `default_path()` is effectively dead for its
originally intended purpose (state cache default path). The only remaining
consumer is the `default_path_ends_with_expected_suffix` test. If the
intent is to keep it as a reference/fallback, it should have a doc comment
explaining that it's superseded by `path_for_repo` in practice.

**Suggested fix:** Either remove `default_path()` (breaking change for
anyone calling it directly) or add a `#[deprecated]` attribute with a note
pointing to `path_for_repo`.

---

#### M-5 · `bug` · `src/scan/mod.rs:82–90`
**`StateCache::save` temp filename uses only `subsec_nanos` — collisions possible**

`save()` constructs the temp filename as `.state.<pid>.<nanos>.tmp` but
uses `subsec_nanos()` (0–999,999,999) instead of full nanoseconds since epoch.
If two calls happen within the same second (same PID, same sub-second
timestamp), the temp file collides. This is not a concern today (single-writer)
but violates the atomicity contract in the doc comment.

**Suggested fix:** Use `as_nanos()` (full nanoseconds since epoch) matching
what `ring_buffer.rs` already does.

---

#### M-6 · `doc-missing` · `src/lib.rs`
**No crate-level documentation**

`lib.rs` has a brief comment but no `//!` crate-level doc. Public API users
(e.g. integration tests) see no description when running `cargo doc`.

**Suggested fix:** Add a `//!` doc block describing the crate purpose and
re-exports.

---

#### M-7 · `dead-code` · every source file
**`#![allow(dead_code)]` blanket on every module — originally per-story, now all stories are done**

Every `mod.rs` and sub-module has `#![allow(dead_code)]` with comments
referencing user-story delivery ("allow dead-code until US-XXX wires it in").
All 21 stories are complete — these blankets now suppress warnings about
genuinely unused code and should be removed.

**Suggested fix:** Remove all `#![allow(dead_code)]` attributes, run
`cargo build`, and fix or `#[allow]` only the specific items that are
still unused.

---

#### M-8 · `doc-drift` · `docs/specs/001-initial-delivery.md` (§13 Crate structure)
**Spec lists separate files per module; implementation uses single `mod.rs` files**

The spec's §13 describes files like `canonicalize.rs`, `decanon.rs`,
`tokens.rs`, `repo.rs`, `pi.rs`, `claude.rs`, `diff.rs`, `types.rs`, etc.
The actual implementation consolidates into fewer files (e.g. all agent
logic in `agents/mod.rs`, all git repo logic in `git/mod.rs`).

**Suggested fix:** Update §13 to match the delivered file layout.

---

#### M-9 · `security` · `src/git/fetch_push.rs:56–58`
**SSH key file fallback uses `None` passphrase — silently fails for passphrase-protected keys**

`git2::Cred::ssh_key(username, pub_opt, &private_key, None)` passes `None`
for the passphrase. If the key file is passphrase-protected (and not in the
agent), this silently fails and falls through to the "no credentials" error.
Since Chronicle runs from cron (no TTY), it can't prompt — but the error
message should explicitly mention passphrase-protected keys as a known
limitation.

**Suggested fix:** Update the error message from "no SSH credentials
available…" to also mention "if your key has a passphrase, ensure it is
loaded in ssh-agent".

---

### Low

#### L-1 · `doc-inaccurate` · `AGENTS.md` (Repository Structure)
**Missing `lib.rs`, `tests/integration.rs`, LICENSE, CONTRIBUTING, SECURITY, etc.**

The AGENTS.md tree is stale and doesn't list files that now exist:
`src/lib.rs`, `tests/integration.rs`, `LICENSE`, `CONTRIBUTING.md`,
`SECURITY.md`, `CODE_OF_CONDUCT.md`, `.github/`, `.forgejo/`.

**Suggested fix:** Regenerate the tree from `find . -not -path '*/target/*' ...`.

---

#### L-2 · `doc-missing` · `README.md`
**No `chronicle errors`, `chronicle config`, or `chronicle schedule` in Usage section**

The README Quick Start shows `init`, `import`, `sync`, `schedule install`,
and `status`, but omits `chronicle errors`, `chronicle config [key] [value]`,
`chronicle push`, `chronicle pull`, and `chronicle schedule uninstall/status`.

**Suggested fix:** Add a full CLI reference table or link to one.

---

#### L-3 · `consistency` · `src/cli/mod.rs`
**`push_impl` does not update `manifest.json` but `sync_impl` does**

`sync_impl` calls `build_updated_manifest` and writes+stages `manifest.json`
alongside session changes. `push_impl` (which does the same outgoing work)
does not update the manifest. A user who only uses `chronicle push` will
never see their `last_sync` timestamp updated.

**Suggested fix:** Have `push_impl` also call `build_updated_manifest` and
write it, matching `sync_impl`'s behaviour.

---

#### L-4 · `consistency` · `src/cli/mod.rs`
**`push_impl` `on_rejection` closure does nothing — push-with-retry can't actually recover**

`push_impl` calls `manager.push_with_retry("origin", || Ok(()), ...)`.
The `on_rejection` closure is `|| Ok(())` — it doesn't fetch or re-merge.
So on a push rejection, Chronicle waits through the backoff and retries
the same non-fast-forward push, which will fail again. The retry is only
useful if `on_rejection` actually performs a fetch+merge cycle.

**Suggested fix:** Wire `on_rejection` to do `manager.fetch("origin")` +
`integrate_remote_changes(...)`, or document that the retry is
best-effort (works only for transient locks, not true divergence).

---

#### L-5 · `doc-missing` · `SECURITY.md:18`
**`[INSERT SECURITY EMAIL]` placeholder not filled in**

**Suggested fix:** Replace with an actual email or Forgejo/GitHub security
advisory URL.

---

#### L-6 · `doc-missing` · `CODE_OF_CONDUCT.md:3`
**`[INSERT CONTACT EMAIL]` placeholder not filled in**

**Suggested fix:** Replace with an actual email address.

---

#### L-7 · `other` · `Cargo.toml`, `README.md`, `CHANGELOG.md`, `CONTRIBUTING.md`
**`YOUR_USERNAME` placeholder throughout**

The GitHub URL placeholder `YOUR_USERNAME` needs to be replaced before
open-sourcing. This was flagged previously but not yet actioned.

**Suggested fix:** Global find-replace once the GitHub org/handle is chosen.

---

#### L-8 · `consistency` · `src/config/schema.rs:165`
**`canonicalization.level` is `u8` but only 1–3 are valid; no schema-level constraint**

The config file parser accepts any `u8` for `level` (e.g. `level = 255`
parses without error). The CLI `set_config_value` has the same issue (H-2).
At the schema level, serde doesn't validate the range.

**Suggested fix:** Add a `#[serde(deserialize_with = "...")]` custom
deserializer that rejects values outside 1–3, or validate in
`config::load()`.

---

#### L-9 · `doc-drift` · `README.md` (Contributing section)
**Still references `git clone ssh://...` for the private Gitea repo**

The main clone URLs in the README were updated, but the Contributing section
just says "Fork the repository" and `git push origin feat/amazing-feature`
without a specific URL — which is fine. No issue here upon re-check.
(Retracted.)

---

## Summary

| Category | Critical | High | Medium | Low | Total |
|----------|----------|------|--------|-----|-------|
| `bug` | 0 | 3 | 1 | 0 | **4** |
| `doc-drift` | 0 | 0 | 2 | 1 | **3** |
| `doc-missing` | 0 | 0 | 1 | 3 | **4** |
| `doc-inaccurate` | 0 | 0 | 1 | 0 | **1** |
| `security` | 0 | 0 | 1 | 0 | **1** |
| `dead-code` | 0 | 0 | 1 | 0 | **1** |
| `consistency` | 0 | 0 | 1 | 3 | **4** |
| `other` | 0 | 0 | 0 | 1 | **1** |
| **Total** | **0** | **3** | **8** | **8** | **19** |
