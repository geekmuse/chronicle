---
date_created: 2026-09-01
date_modified: 2026-09-01
status: active
audience: both
cross_references:
  - docs/001-architecture.md
  - docs/research/010-agent-session-integration-landscape.md
  - src/adapters/mod.rs
  - src/agents/mod.rs
  - src/cli/mod.rs
  - src/doctor/mod.rs
  - src/git/commit.rs
---

# Spec 011 — Agent Adapter Registry

## 1. Delivery Status

**Implemented in Chronicle 0.9.0.**

This specification records the adapter registry that ships in
`src/adapters/mod.rs`. It replaces the former provider-specific orchestration
in the CLI with deterministic Pi and Claude Code adapter iteration, while
preserving the existing configuration, repository layout, directory codecs,
JSONL canonicalization, merge semantics, and human/porcelain output.

The implementation is deliberately a small policy boundary, not a complete
provider-plugin framework. The shared CLI coordinator continues to own file
scanning, content canonicalization, JSONL merging, Git operations, caches,
locks, and writes.

## 2. Shipped Scope

The registry owns the provider-specific facts needed by generic coordination:

- stable agent identity and diagnostic/configuration metadata;
- default and configured session roots;
- canonical repository-root prefixes;
- Pi and Claude encoded-project-directory canonicalization and reversal;
- validation and repository-path derivation for direct-child `.jsonl`
  artifacts; and
- partial-history recency policy.

`chronicle import`, `sync`, `push`, materialization, `status`, and `doctor`
resolve enabled agents through the registry. Commit accounting uses stable
`AgentId` keys before it is rendered in the existing Pi/Claude-compatible
commit message form.

## 3. Public Types and Contracts

`src/adapters/mod.rs` exports these types:

| Type | Purpose |
|------|---------|
| `AgentId` | Ordered built-in identity: `Pi` then `Claude`; `as_str()` produces `pi` and `claude` |
| `AgentMetadata` | Stable ID, display name, config key, and repository-relative base |
| `AgentContext` | Resolved metadata, enabled state, and effective session directory |
| `SessionArtifact` | Validated local JSONL path, source session-directory name, and canonical repository-relative destination |
| `AdapterOutcome` | Session/file counts for adapter-scoped operations |
| `AdapterError` | Unsupported/duplicate adapter and artifact-layout validation failures |
| `AgentAdapter` | Provider policy trait |
| `AdapterRegistry` | Deterministically ordered collection of built-in adapters |

The implemented trait is intentionally narrow:

```rust
pub trait AgentAdapter: Send + Sync {
    fn metadata(&self) -> AgentMetadata;
    fn default_session_dir(&self, home: &Path) -> PathBuf;
    fn canonicalize_dir(&self, tokens: &TokenRegistry, name: &str) -> String;
    fn decanonicalize_dir(&self, tokens: &TokenRegistry, name: &str) -> String;
    fn repository_artifact_recency(&self, path: &Path) -> Option<DateTime<Utc>>;
    fn is_session_file(&self, path: &Path) -> bool;
    fn artifact(
        &self,
        session_dir: &Path,
        source_path: &Path,
        tokens: &TokenRegistry,
    ) -> Result<SessionArtifact, AdapterError>;
}
```

`AdapterRegistry::with_defaults()` registers `PiAdapter` and `ClaudeAdapter`
in `AgentId` order. `contexts(&cfg.agents, home)` resolves each adapter's
configured `enabled` flag and `session_dir`; an empty configured path selects
the adapter default. `BTreeMap` storage makes iteration deterministic.

## 4. Built-in Adapter Policies

| Policy | Pi | Claude Code |
|--------|----|-------------|
| Stable ID / display name | `pi` / `Pi` | `claude` / `Claude` |
| Default local root | `~/.pi/agent/sessions` | `~/.claude/projects` |
| Canonical repository base | `pi/sessions` | `claude/projects` |
| Project-directory codec | Existing double-dash Pi codec | Existing single-dash Claude codec |
| Recognized artifact | `.jsonl` file directly below a session directory | Same |
| Partial-history recency | Timestamp parsed from the Pi filename | Earliest valid `timestamp`, `created_at`, or `createdAt` value in the JSONL file |

An artifact must be a `.jsonl` file directly below a directory under the
configured session root. The adapter rejects files outside that root,
non-JSONL files, and paths without exactly one session-directory component.

Files whose recency cannot be determined sort after timestamped files; ties
are resolved by filename in the materialization coordinator. This retains the
previous Pi filename and Claude content policies.

## 5. Coordination Data Flow

### Outgoing

1. The CLI builds `AdapterRegistry::with_defaults()` and resolves contexts
   from `[agents.pi]` and `[agents.claude]`.
2. It scans each enabled context's session directory using the existing
   mtime/size state cache.
3. The matching adapter validates the source file and derives its canonical
   repository-relative path; the shared canonicalization and JSONL merge
   code then processes the content.
4. The coordinator stages changed paths, records `AgentId` totals in
   `SyncSummary`, commits, pushes, and persists the existing cache formats.

### Incoming and diagnostics

1. Materialization iterates enabled contexts and each metadata-defined
   repository base.
2. The shared materialization code asks the adapter for directory decoding and
   artifact recency, then retains the configured number of recent files per
   session directory.
3. `status` iterates contexts for effective-root and pending-file reporting.
   `doctor::check_agent_contexts` derives stable `agents.pi` and
   `agents.claude` results from the same contexts.

No adapter owns Git transport, lock acquisition, cache persistence, content
canonicalization, or the grow-only JSONL merge. Those remain shared behavior
until another storage model requires a broader, separately specified API.

## 6. Compatibility

Version 0.9.0 preserves these user-visible contracts:

- Existing `[agents.pi]` and `[agents.claude]` TOML sections and
  `session_dir` overrides remain valid.
- Canonical repository paths remain `pi/sessions/...` and
  `claude/projects/...`.
- Pi and Claude directory codecs, L1/L2/L3 content canonicalization,
  grow-only JSONL merge, cache-file formats, partial-history behavior, CLI
  flags, and doctor porcelain keys remain unchanged.
- Sync summaries still render `pi` then `claude`, including zero totals.
- The older `Agent`, `PiAgent`, and `ClaudeAgent` codec API remains available
  but is deprecated. New coordination code must use `AgentAdapter` through
  `AdapterRegistry`.

No migration is required for existing repositories or cache files.

## 7. Validation

The delivery added nine adapter unit tests, one registry-context doctor test,
one `SyncSummary` test, and one dual-agent two-machine integration test. The
merge fuzzer now also exercises canonical paths rooted at both built-in
adapter repository bases.

The release validation passed:

```bash
cargo test
cargo clippy -- -D warnings
cargo build
cargo fmt --check
cargo deny check
cargo +nightly fuzz build --fuzz-dir fuzz
```

## 8. Deferred Work

This release does not add a third agent, runtime plugins, dynamic TOML agent
configuration, SQLite/WAL synchronization, or a generalized lifecycle trait
that owns discovery, merge, and materialization. Those need a concrete
provider requirement and a follow-up specification informed by
`docs/research/010-agent-session-integration-landscape.md`.
