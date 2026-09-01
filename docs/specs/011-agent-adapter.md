---
date_created: 2026-09-01
date_modified: 2026-09-02
status: draft
audience: both
cross_references:
  - docs/research/010-agent-session-integration-landscape.md
  - docs/001-architecture.md
  - docs/specs/001-initial-delivery.md
  - src/agents/mod.rs
  - src/cli/mod.rs
  - src/config/schema.rs
  - src/scan/mod.rs
  - src/merge/set_union.rs
  - src/materialize_cache.rs
  - src/sync_state.rs
  - src/doctor/mod.rs
  - src/git/commit.rs
---

# Spec 011 — Agent Adapter Abstraction

## 1. Summary

Replace Chronicle's hard-coded Pi/Claude orchestration with an `AgentAdapter`
lifecycle abstraction. Retrofit both current providers without changing their
configuration, repository layout, canonical bytes, merge behavior, or CLI
results.

This refactor establishes a safe extension point for agents with different
storage and merge rules. It does not add another agent, plugins, snapshot
storage, or SQLite synchronization.

## 2. Motivation

The existing `Agent` trait models only default session paths and project
directory encoding. Provider lifecycle logic remains coupled to Pi and Claude:

- import, sync, push, materialization, status, and doctor branch by provider;
- `is_pi: bool` selects canonicalization and partial-history behavior;
- staging and commit summaries use separate provider vectors and counters;
- repository initialization creates two fixed trees;
- doctor accepts Pi/Claude-specific parameters;
- L1 canonicalization exposes provider-named methods; and
- scanner and merger behavior is selected globally.

This assumes every provider is a tree of compatible JSONL files. Codex and
Gemini require different record identities; snapshots need conflict policies;
Hermes and OpenCode need logical export/import. See
`docs/research/010-agent-session-integration-landscape.md`.

Pi and Claude are favorable retrofit targets because they share discovery,
line canonicalization, grow-only set-union merge, and direct file
materialization. Their differences can be expressed as policies rather than
branches.

## 3. Goals

1. Make every provider-aware command iterate an adapter registry.
2. Preserve all current Pi and Claude behavior and storage compatibility.
3. Give adapters ownership of discovery, project mapping, canonical export,
   merge, materialization, recency, and health checks.
4. Keep Git, manifests, scheduling, locking, and command orchestration generic.
5. Replace boolean dispatch with stable provider identities.
6. Allow a future built-in adapter to use different storage semantics without
   changing command coordination.
7. Preserve deterministic ordering, diagnostics, and commit messages.
8. Add characterization and parity tests before deleting old branches.

## 4. Non-Goals

This delivery does not:

- add Codex, Gemini, Qwen, Hermes, OpenCode, or another provider;
- define a third-party plugin ABI or dynamically load adapters;
- replace typed TOML sections with dynamic configuration;
- synchronize live SQLite, WAL, or shared-memory files;
- change grow-only history, deletion, token, or canonicalization semantics;
- migrate repository or cache layouts;
- change `chronicle import --agent`; or
- remove public `Agent`, `PiAgent`, or `ClaudeAgent` symbols.

Interfaces should permit future storage models without implementing speculative
mechanics before a real provider requires them.

## 5. Compatibility Requirements

| Surface | Required behavior |
|---------|-------------------|
| Configuration | Existing `[agents.pi]` and `[agents.claude]` TOML and defaults remain valid |
| Local paths | Configured `session_dir` continues to override provider defaults |
| Repository | Paths remain `pi/sessions/...` and `claude/projects/...` |
| Canonicalization | Existing L1/L2/L3 output remains byte-identical |
| Merge | Existing grow-only set-union with remote-wins tie-break on divergent common entries, ordering, and newlines remain unchanged |
| Import | One commit per non-empty provider project directory remains unchanged |
| Partial history | Pi uses filename time; Claude uses earliest recognized entry time |
| Caches | Existing `state.json`, `materialize_cache.json`, and `sync_state.json` formats and keys remain byte-identical |
| CLI | Commands, flags, porcelain keys, and exit behavior remain compatible |
| Commits | Existing two-provider summary text and order remain compatible (see §9.5) |
| Doctor | `agents.pi` and `agents.claude` porcelain keys remain unchanged |

Existing checkouts must remain usable by binaries from before and after the
refactor. No migration command is permitted.

> **Merge terminology.** “Remote-wins” in this document is shorthand for the
> merge in `src/merge/set_union.rs`: a grow-only set union keyed by
> `EntryKey`, with the remote raw bytes retained on divergent common entries.
> Local-only entries are never dropped.

## 6. Design Principles

1. **Adapters own provider semantics.** Coordinators never infer behavior from
   an ID, path, or storage kind.
2. **Git stays outside adapters.** Adapters produce and consume canonical
   artifacts; coordinators own Git transport and commits.
3. **Stable identities replace booleans.** Names are metadata, not control flow.
4. **Common behavior is shared.** Pi and Claude compose one JSONL helper with
   different path and recency policies.
5. **Native state is not the merge unit.** Future database adapters export
   deterministic logical artifacts; live databases never enter Git.
6. **Artifact failures remain non-fatal.** One corrupt session warns and skips;
   adapter initialization or repository-integrity failures remain fatal.
7. **No downcasting.** Commands operate only through `AgentAdapter`.

## 7. Proposed Interfaces

Exact Rust names may change, but ownership and data flow must remain equivalent.

### 7.1 Identity and metadata

```rust
#[derive(Clone, Copy, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum AgentId {
    Pi,
    Claude,
}

impl AgentId {
    /// Stable wire encoding used for all string-typed surfaces (see §7.6).
    pub fn as_str(self) -> &'static str {
        match self {
            AgentId::Pi => "pi",
            AgentId::Claude => "claude",
        }
    }
}

pub struct AgentMetadata {
    pub id: AgentId,
    pub display_name: &'static str,
    pub repo_root: &'static str,
}
```

`AgentId` initially lists built-ins. Adding a variant is acceptable, but
command orchestration must not gain a branch.

A `StorageKind` enum was considered and **deliberately omitted**: §6 principle
1 forbids coordinators inferring behavior from storage kind, and no in-tree
consumer would exist for it. It may be reintroduced by a future spec when a
concrete non-dispatch consumer (docs generation, verbose status label) is
identified.

### 7.2 Lifecycle values

```rust
pub struct AdapterContext<'a> {
    /// User home directory used for expanding `~` in adapter-configured
    /// local roots. **Not** used for canonicalization — see `tokens`.
    pub home: &'a Path,
    /// Token registry used exclusively for L1/L2/L3 canonicalization and
    /// de-canonicalization. Adapters must not derive local paths from it.
    pub tokens: &'a TokenRegistry,
    pub canonicalization_level: u8,
    pub follow_symlinks: bool,
}

pub struct DiscoveredArtifact {
    pub local_path: PathBuf,
    pub local_mtime: DateTime<Utc>,
    pub local_size: u64,
    pub change_kind: ChangeKind,
    /// Grouping key used by the import coordinator to produce one commit
    /// per project. For the current JSONL providers this is the encoded
    /// project directory name (e.g. `--Users-bradmatic-Dev-foo--`). Adapters
    /// with no directory grouping return the artifact's stable identifier;
    /// coordinators treat the value as opaque.
    pub project_id: String,
}

pub struct CanonicalArtifact {
    pub repo_path: PathBuf,
    pub content: String,
    pub cache_key: String,
    pub file_state: FileState,
}

pub struct RepositoryArtifact {
    pub repo_path: PathBuf,
    pub content: String,
    pub repo_mtime: DateTime<Utc>,
    pub repo_size: u64,
}
```

The first implementation is text-oriented because both current providers are
JSONL. A future database adapter may emit deterministic canonical text without
exposing its native database.

### 7.3 Adapter lifecycle

```rust
pub trait AgentAdapter: Send + Sync {
    fn metadata(&self) -> &AgentMetadata;
    /// Convenience mirror of the registry's enablement bit; adapters must
    /// not mutate this value at runtime (see §7.4).
    fn enabled(&self) -> bool;
    fn local_root(&self) -> &Path;

    fn discover(&self, cache: &StateCache, context: &AdapterContext<'_>)
        -> Result<Vec<DiscoveredArtifact>, AdapterError>;
    fn export(&self, artifact: &DiscoveredArtifact, context: &AdapterContext<'_>)
        -> Result<CanonicalArtifact, AdapterError>;
    fn merge(&self, repository: Option<&RepositoryArtifact>, outgoing: &CanonicalArtifact)
        -> Result<AdapterMerge, AdapterError>;
    fn materialize(&self, artifact: &RepositoryArtifact, context: &AdapterContext<'_>)
        -> Result<MaterializeOutcome, AdapterError>;
    fn recency(&self, artifact: &RepositoryArtifact)
        -> Result<Option<DateTime<Utc>>, AdapterError>;
    fn health_check(&self) -> AgentHealth;
}
```

Coordinators retain dry-run, cache persistence, Git, aggregate counts, and
user-facing reporting. `merge(None, ...)` creates a new artifact.
`materialize` translates canonical project identity to native local storage.
Diagnostics must use read-only discovery or health operations.

**Adapters are the only source of canonicalization.** The coordinator never
calls `TokenRegistry` methods on paths it received from an adapter; the
adapter has already canonicalized them in `export` and must de-canonicalize
them in `materialize`.

**Cache-key layering.** The coordinator owns `StateCache` persistence, but the
canonical repo-relative path used as the cache key is produced by the adapter
(`CanonicalArtifact::cache_key`). The unit-test convention in `src/scan/mod.rs`
of keying by absolute local path is scoped to isolated scanner tests and does
not leak into production; adapters must not rely on it.

**Recency policy.**

- `Err(_)` → the coordinator logs a warning and treats the artifact as if
  `recency` returned `Ok(None)`.
- `Ok(None)` → the artifact is eligible for partial-history selection but
  sorts last (after all timestamped artifacts) using a stable secondary key
  (`repo_path`).
- `Ok(Some(ts))` → sorts by `ts` descending; ties broken by `repo_path`.

### 7.4 Registry

```rust
pub struct AdapterRegistry {
    adapters: Vec<Box<dyn AgentAdapter>>,
}

impl AdapterRegistry {
    pub fn from_config(config: &Config, home: &Path) -> Result<Self, AdapterError>;
    pub fn all(&self) -> impl Iterator<Item = &dyn AgentAdapter>;
    pub fn enabled(&self) -> impl Iterator<Item = &dyn AgentAdapter>;
    pub fn selected(&self, selector: &str)
        -> Result<Vec<&dyn AgentAdapter>, AdapterError>;
}
```

`from_config` is the only production location allowed to access both
`cfg.agents.pi` and `cfg.agents.claude` directly, with two named exceptions:
doctor's config-reading CLI plumbing (which passes typed structs to
`check_agents` — replaced by a registry-taking signature per §9.4) and the
serde round-trip tests in `src/config/schema.rs`.

`from_config` expands configured paths and constructs adapters. The registry
is the authoritative source of enablement; the trait's `enabled()` mirrors
the registry decision and must not diverge at runtime.

#### Selector grammar

`selected(selector)` accepts exactly one of:

| Selector | Result |
|----------|--------|
| `"all"` | Every **enabled** adapter, in stable `AgentId` order |
| `AgentId::as_str()` (e.g. `"pi"`, `"claude"`) | Exactly that adapter if enabled; error otherwise |

Case-sensitive. Comma-separated or repeated selectors are rejected in this
delivery; a future spec may extend the grammar when a caller needs it.
Invalid selectors return an `AdapterError::UnknownSelector { input, supported }`
listing supported IDs.

Disabled adapters remain registered for repository initialization and
diagnostics but are not returned by `enabled()` or by `selected("all")`.

### 7.5 Outcome and error types

```rust
pub struct AdapterMerge {
    /// Canonical bytes to write back to the repository working tree.
    pub content: String,
    /// Per-entry conflicts (divergent common entries where remote was kept).
    pub conflicts: Vec<PrefixConflict>,
    /// Lines that failed to parse and were skipped.
    pub malformed: Vec<MalformedLine>,
    /// Post-merge repo-relative path plus mtime/size for the state cache.
    pub file_state: FileState,
}

pub struct MaterializeOutcome {
    /// Absolute path written on the local filesystem, or `None` if the
    /// materialize cache indicated the file was already up to date.
    pub local_path: Option<PathBuf>,
    /// Bytes written; zero when the materialize was a cache hit.
    pub bytes_written: u64,
}

pub enum AgentHealth {
    Ok { detail: String },
    Warn { detail: String, hint: String },
    Error { detail: String, hint: String },
    Skipped { reason: String },
}

#[derive(thiserror::Error, Debug)]
pub enum AdapterError {
    /// Fatal — aborts the current command before any Git mutation.
    #[error("adapter {agent} initialization failed: {reason}")]
    Init { agent: AgentId, reason: String },
    /// Fatal — an invariant of the on-disk layout was violated.
    #[error("adapter {agent} integrity error at {path}: {reason}")]
    Integrity { agent: AgentId, path: PathBuf, reason: String },
    /// Non-fatal — the coordinator logs a warning and skips this artifact.
    #[error("adapter {agent} artifact {path} failed: {reason}")]
    Artifact { agent: AgentId, path: PathBuf, reason: String },
    /// Fatal — selector did not match a known adapter (see §7.4).
    #[error("unknown selector `{input}` (supported: {supported:?})")]
    UnknownSelector { input: String, supported: Vec<&'static str> },
    /// Fatal — bubbles up from the filesystem or a wrapped library.
    #[error("adapter {agent} I/O error at {path}: {source}")]
    Io {
        agent: AgentId,
        path: PathBuf,
        #[source] source: std::io::Error,
    },
}
```

**Coordinator disposition:**

| Error variant | Coordinator behavior |
|---------------|----------------------|
| `Init`, `Integrity`, `UnknownSelector` | Abort before staging; leave the working tree and remote untouched |
| `Artifact` | Log warning, record in error ring buffer, continue with remaining artifacts |
| `Io` | Abort the current adapter phase; other adapters still run; commit only if at least one adapter produced staged changes |

### 7.6 `AgentId` wire encoding

`AgentId::as_str()` is the single source of truth for every string-typed
surface where an agent is named:

| Surface | Example |
|---------|---------|
| Config section keys | `[agents.pi]`, `[agents.claude]` |
| Repository roots | `pi/sessions`, `claude/projects` (via `AgentMetadata::repo_root`) |
| Porcelain output | `agents.pi=ok`, `agents.claude=ok` |
| `--agent` selector | `chronicle import --agent pi` |
| Commit summary group | `(pi: 8, claude: 7)` (see §9.5) |
| Tracing fields | `agent="pi"`, `agent="claude"` |

All six surfaces must derive from `AgentId::as_str()`; string literals for
agent names are forbidden outside the `AgentId` type itself.

## 8. Current Provider Implementations

### 8.1 Shared JSONL helper

A private `JsonlFileAdapter` helper owns behavior common to Pi and Claude:

- recursive `.jsonl` discovery and state-cache classification;
- line-by-line L2/L3 canonicalization and de-canonicalization;
- repository path construction and containment validation;
- grow-only `merge_jsonl` behavior and newline preservation;
- direct permission-preserving materialization;
- materialization-cache checks; and
- conversion of artifact failures into warnings.

The helper uses composition; `PiAdapter` and `ClaudeAdapter` are the registered
objects and the only public provider identities.

### 8.2 Provider policies

| Policy | Pi | Claude |
|--------|----|--------|
| ID/display | `pi` / `Pi` | `claude` / `Claude` |
| Default root | `~/.pi/agent/sessions` | `~/.claude/projects` |
| Repository root | `pi/sessions` | `claude/projects` |
| Project mapping | Existing Pi wrapper rules | Existing Claude prefix/dot rules |
| Recency | Timestamp in filename | Earliest recognized JSONL timestamp |
| Merge | Existing `merge_jsonl` | Existing `merge_jsonl` |

The configured root is stored on each adapter. Encode/decode behavior stays
unchanged.

#### `Agent::session_dir` deprecation

`PiAgent::session_dir(home)` and `ClaudeAgent::session_dir(home)` return the
**default** provider root and ignore user configuration. To avoid callers
silently receiving the wrong path after this refactor, the method is marked
`#[deprecated]` in favor of `AgentAdapter::local_root()`, which returns the
effective (configured or default) path.

**Acceptance:** an AC in §14 asserts that no in-tree call site outside
compatibility tests uses `Agent::session_dir` for effective-path resolution.

## 9. Orchestration Changes

### 9.1 Import

Resolve `--agent` through the registry (§7.4 grammar) and invoke a generic
import coordinator. Provider identity, roots, export, and project mapping
come from the adapter. The coordinator groups `DiscoveredArtifact`s by
`project_id` and produces one commit per non-empty group, preserving today's
per-directory import commit shape.

### 9.2 Sync and push

```text
for each enabled adapter in stable AgentId order
  discover changed artifacts
  export each artifact to canonical form
  merge with its repository artifact
  collect cache updates and per-agent totals
stage all changed paths and commit once
```

Remove `ScannedChange.is_pi`, `PushFileParams.is_pi`, provider staging vectors,
and Pi/Claude branches.

### 9.3 Pull and materialization

Enumerate each adapter's repository root, apply partial-history filtering via
`adapter.recency` (per the policy in §7.3), then call `adapter.materialize`.
Provider directory decoding must leave `src/cli/mod.rs`. Materialization cache
keys remain repository-relative paths, produced by the adapter via
`CanonicalArtifact::cache_key`. The `MaterializeCache` config-hash input
changes only if canonical bytes change, which this refactor forbids.

### 9.4 Status and doctor

Iterate metadata and adapter health results. Existing Pi/Claude labels and
porcelain keys remain exact. Pending-file status uses read-only discovery and
must not update caches, write provider state, or acquire provider write locks.

#### `check_agents` migration

The current per-provider signature

```rust
pub fn check_agents(
    pi_enabled: bool, pi_session_dir: &Path,
    claude_enabled: bool, claude_session_dir: &Path,
) -> Vec<CheckResult>
```

is replaced by

```rust
pub fn check_agents(registry: &AdapterRegistry) -> Vec<CheckResult>
```

which iterates registered adapters in stable `AgentId` order and derives
porcelain keys as `format!("agents.{}", meta.id.as_str())`. For each adapter
the function calls `adapter.health_check()` and, on `AgentHealth::Ok`,
augments the detail with the JSONL file count via the shared discovery helper.

`check_config`, `check_git`, and `check_scheduler` are unchanged by this
spec. Existing doctor unit tests (23) and integration tests (3) are migrated
to construct a two-adapter registry rather than passing raw booleans; the
porcelain output must remain byte-identical.

### 9.5 Repository and summaries

`RepoManager::ensure_working_tree` receives sorted roots from registry metadata.
Existing Pi/Claude roots are retained even when disabled.

Replace provider counters with deterministic accounting:

```rust
pub struct SyncSummary {
    pub new_files: usize,
    pub modified_files: usize,
    /// Totals keyed by AgentId. Renderer iterates **every registered agent**
    /// (not just present keys) so zero-total agents still appear.
    pub agent_totals: BTreeMap<AgentId, usize>,
}
```

**Rendering contract.** The commit-message formatter takes the registry
(for the list of registered agents) plus the `SyncSummary` and emits one
`name: count` pair per registered `AgentId` in stable order, defaulting
missing keys to `0`. With Pi and Claude registered this always produces:

```text
+3 files, ~12 files (pi: 8, claude: 7)
```

and for a Pi-only install with Claude disabled but still registered:

```text
+3 files, ~12 files (pi: 8, claude: 0)
```

A characterization test in Phase 0 (§12) freezes the exact string for the
current two-provider layout. `pi_total`/`claude_total` accessors are retained
on `SyncSummary` as a compatibility façade so external callers of
`git::commit` continue to compile; both are computed from `agent_totals`.

### 9.6 End-to-end sync sequence

The full lifecycle sits between two coordinator boundaries: the advisory
`chronicle.lock` (`SyncLockGuard`) and the sync-state persistence. Adapters
run only inside the locked region.

```text
 coordinator                        adapter(s)                fs / git
 ───────────                        ──────────                ────────
 1. acquire chronicle.lock (§11.1)
 2. load StateCache, MaterializeCache
 3. git fetch + fast-forward or
    detect divergence
 4. for each enabled adapter in
    stable AgentId order:
      a. adapter.discover(cache) ──▶ Vec<DiscoveredArtifact>
      b. for each discovered:
           adapter.export ────────▶ CanonicalArtifact
           read repo file (if any)
           adapter.merge ─────────▶ AdapterMerge
           write repo working tree
           accumulate StateCache updates
           accumulate agent_totals[id] += 1
 5. stage all changed repo paths
 6. commit once with rendered SyncSummary (§9.5)
 7. git push with retry/backoff
 8. for each enabled adapter:
      a. enumerate repo root
      b. apply adapter.recency-based partial filter
      c. adapter.materialize ─────▶ MaterializeOutcome
         (skip on MaterializeCache hit)
 9. persist StateCache, MaterializeCache
10. write sync_state.json (last op, duration, timestamp)
11. release chronicle.lock (guard drop deletes lock file)
```

`Artifact` errors from step 4b log and continue; `Init` / `Integrity` /
`Io` errors follow the disposition table in §7.5. Steps 5–7 are skipped
when no adapter produced staged changes.

## 10. Configuration

Typed configuration remains unchanged:

```toml
[agents.pi]
enabled = true
session_dir = "~/.pi/agent/sessions"

[agents.claude]
enabled = true
session_dir = "~/.claude/projects"
```

Registry construction may normalize both structs into a private runtime view.
A dynamic schema waits until a third provider supplies concrete requirements.

## 11. Error and Safety Requirements

1. Errors identify provider, operation, and artifact when known (§7.5).
2. Repository paths must be relative, normalized, and under declared roots.
3. Materialization paths must remain under configured local roots.
4. Existing symlink policy remains enforced. The **coordinator** enforces
   the top-level `follow_symlinks` policy before invoking
   `adapter.discover`; the adapter enforces it on any nested walks it
   performs. `AdapterContext::follow_symlinks` is provided so adapters that
   walk their own trees stay consistent.
5. File writes retain current permission-safe behavior.
6. Adapters must exclude credentials, config, locks, runtime files, databases,
   WAL files, and shared-memory files unless a later spec declares otherwise.
7. Iteration and summaries are deterministic.
8. One bad artifact does not suppress another artifact or provider.
9. Enabled-adapter initialization fails before Git mutation begins.
10. Dry runs may read but never mutate provider state, caches, Git, or remotes.
    Enforcement is coordinator-owned: adapters have no `dry_run` flag; the
    coordinator simply skips staging, commit, push, materialize, and cache
    persistence when `dry_run == true`.

### 11.1 Concurrency and locking

- The coordinator acquires `<repo>/../chronicle.lock` via `SyncLockGuard`
  before calling any mutating adapter method (`export`, `merge`,
  `materialize`). The current stale-lock recovery policy
  (dead-PID vs live-PID-past-`lock_timeout_secs`) is unchanged.
- `discover` and `health_check` must be lock-free and side-effect-free so
  `chronicle status` and `chronicle doctor` can run while a sync is in
  progress.
- `AgentAdapter: Send + Sync` is required. A static assertion
  (`fn _assert_send_sync<T: Send + Sync>() {}` invoked with each concrete
  adapter) is added under `#[cfg(test)]`.
- Adapters must not spawn threads that outlive a method call.

### 11.2 Observability

- Every lifecycle call is wrapped in a `tracing::info_span!` with fields
  `agent = %meta.id.as_str()` and `op = "discover"|"export"|"merge"|
  "materialize"|"recency"|"health_check"`.
- Warnings emitted during merge (malformed lines, prefix mismatches) and
  materialize (permission-safe write fallbacks) preserve their existing
  fields; the `agent` field is added but no existing field is renamed.
- The error ring buffer entry for an `AdapterError::Artifact` records
  `agent`, `op`, `path`, and `reason`.

## 12. Implementation Plan

### Phase 0 — Characterize behavior

- Add golden Pi/Claude export and materialization fixtures.
- Capture commit summaries (both two-provider and Pi-only-with-Claude-disabled
  cases), recency, cache keys, status, and doctor output.
- Add a two-provider end-to-end case; current integration tests emphasize Pi.
- Freeze `state.json`, `materialize_cache.json`, and `sync_state.json` byte
  layouts as golden fixtures for §13 round-trip parity.

### Phase 1 — Add types and registry

- Add identity, metadata, errors (§7.5), contexts, and registry.
- Construct Pi/Claude adapters from unchanged configuration.
- Keep the existing path-codec API (and mark `Agent::session_dir` deprecated).
- Derive repository initialization from descriptors.

### Phase 2 — Retrofit outgoing paths

- Implement common JSONL discovery, export, and merge behind the trait.
- Convert import, sync, and push to registry iteration.
- Remove boolean dispatch and provider staging vectors.
- Generalize deterministic commit accounting per §9.5.
- Re-point `fuzz/fuzz_targets/fuzz_merge.rs` to exercise the adapter
  `merge` path; `fuzz_canon_structured` and `fuzz_roundtrip` continue
  targeting the unchanged canonicalization surface.

### Phase 3 — Retrofit incoming paths

- Move project de-canonicalization, recency, filtering, and writes behind
  adapters.
- Convert pull and sync materialization to registry iteration.
- Preserve cache keys and partial-history selection.

### Phase 4 — Diagnostics and cleanup

- Convert status, pending counts, verbose output, and doctor to the
  registry-taking `check_agents` signature (§9.4).
- Remove provider branches outside registry/config compatibility code.
- Update architecture and user documentation.
- Run mechanical searches for forbidden boolean/provider dispatch (e.g.
  `rg 'is_pi|"pi"|"claude"' src` gated against an allowlist of
  registry/config/compat sites).

Each phase must compile and leave completed-path tests passing.

## 13. Test Plan

### Unit and coordinator tests

- Registry order, enabled filtering, and selector errors (per §7.4 grammar).
- Metadata and configured-root expansion.
- L1/L2/L3 export and project-mapping parity.
- Merge parity, malformed lines, and remote-wins conflicts.
- Pi filename and Claude content recency parity, including the `Err` and
  `Ok(None)` cases from §7.3.
- Path-containment rejection and health states.
- Deterministic summary formatting for both two-provider and
  Pi-only-with-Claude-disabled shapes (§9.5).
- `Send + Sync` static assertions on both concrete adapters.
- A test adapter (see §13.1) completes generic outgoing and incoming
  coordination without a provider branch.

### Integration tests

- Pi-only, Claude-only, and combined sync are byte-compatible.
- Disabled providers are skipped while repository roots remain.
- Existing repositories and caches work without migration; loading a
  pre-refactor `state.json`, `materialize_cache.json`, and `sync_state.json`
  and re-saving must produce byte-identical files.
- Import selectors retain current behavior across the §7.4 grammar.
- Pull applies each partial-history policy.
- A malformed provider artifact does not block valid work elsewhere.

### Fuzzing

- All targets in `fuzz/fuzz_targets/` continue to build under
  `cargo +nightly fuzz build` in CI.
- `fuzz_merge` covers the adapter `merge` path; a `#[no_mangle]` bridge or
  a small wrapper module exposes the merge helper unchanged.

### Quality checks

```bash
cargo test
cargo clippy -- -D warnings
cargo build
cargo fmt --check
cargo deny check
cargo +nightly fuzz build --fuzz-dir fuzz
```

### 13.1 Test adapter contract

A `TestAdapter` under `#[cfg(test)]` (or a `tests/` support module) provides
deterministic coverage of the generic coordinator paths without touching the
filesystem:

- **Identity.** New variant `AgentId::Test` gated on `#[cfg(test)]`, with
  `as_str() == "test"` and `repo_root == "test/artifacts"`.
- **State.** In-memory `Vec<(project_id, path, content, mtime, size)>`
  configurable per-test; discovery yields these deterministically.
- **Behavior.** `export` returns the content unchanged with a fixed
  `cache_key`; `merge` concatenates outgoing after repository content with
  no conflict detection; `materialize` records write requests into a
  `Vec<(PathBuf, String)>` sink; `recency` returns a caller-provided value;
  `health_check` returns a caller-provided `AgentHealth`.
- **Coverage matrix.** The test adapter is used to exercise, in isolation:
  registry iteration order, dry-run skip semantics, per-agent totals in
  `SyncSummary`, error dispositions from §7.5, and locking/observability
  invariants from §11.1–§11.2.

## 14. Acceptance Criteria

1. Pi and Claude are instantiated through `AdapterRegistry`.
2. Import, sync, push, materialization, status, and doctor iterate adapters.
3. `src/cli/mod.rs` has no `is_pi` or Pi-vs-Claude conditional.
4. Direct provider config access is confined to registry construction, doctor
   config plumbing, and serde round-trip tests (§7.4).
5. Repository roots derive from descriptors while existing paths remain exact.
6. Pi and Claude canonical fixtures remain byte-identical.
7. Existing repository and cache layouts require no migration; the round-trip
   test in §13 passes.
8. Human and porcelain diagnostics remain compatible.
9. Commit summaries remain compatible and deterministic per §9.5.
10. A test adapter (§13.1) traverses generic outgoing and incoming coordinator
    paths.
11. Existing public provider codec APIs remain available; no in-tree call
    site outside compatibility tests uses `Agent::session_dir` for
    effective-path resolution (§8.2).
12. `AgentId::as_str()` is the sole source of every agent-named string
    surface listed in §7.6 (mechanical grep gate in Phase 4).
13. All quality checks pass, including `cargo +nightly fuzz build`.

## 15. Risks and Mitigations

| Risk | Mitigation |
|------|------------|
| Large CLI refactor regresses behavior | Characterization first; convert one lifecycle phase at a time |
| Trait is secretly JSONL-specific | Keep JSONL in a composed helper; lifecycle uses canonical artifacts |
| Premature generalization | Implement only operations exercised by current providers |
| Canonical bytes change | Golden fixtures and byte-parity assertions |
| Caches fully reprocess | Preserve paths, keys, hashes, and formats; §13 round-trip test |
| Public API breaks | Keep current trait and codec structs as compatibility façade |
| Trait-object order varies | Stable registry order and `BTreeMap` accounting |
| Adapter escapes roots | Normalize and validate local and repository paths |
| Diagnostics mutate state | Separate and test read-only discovery/health behavior |
| Commit summary text drift (Pi-only install) | §9.5 renderer iterates every registered `AgentId` with zero defaulting |
| `Agent::session_dir` returns default instead of configured path | `#[deprecated]` + AC 11 |

## 16. Estimate

| Work | Estimate |
|------|----------|
| Characterization and registry | 1–2 days |
| Outgoing retrofit | 1–2 days |
| Incoming retrofit | 1–2 days |
| Diagnostics, accounting, and docs | 1–2 days |
| Full validation | 1 day |

Expected total: **5–8 focused developer days**. The complete registry,
diagnostics, compatibility tests, and future-safe lifecycle boundary are
required before adding a provider.

### 16.1 Versioning and release

This is an additive change: a new public `AgentAdapter` trait and
`AdapterRegistry` type, with the existing `Agent`, `PiAgent`, `ClaudeAgent`,
and `SyncSummary` public API retained as a compatibility façade. Per
`AGENTS.md`, that is a **MINOR** version bump.

- Target version: **0.9.0**.
- CHANGELOG entries:
  - `Added` — `AgentAdapter` trait, `AdapterRegistry`, `AgentId`,
    `AgentMetadata`, error/outcome types from §7.5.
  - `Changed` — Internal orchestration in `src/cli/mod.rs`,
    `src/doctor/mod.rs`, `src/git/commit.rs` now iterates the registry.
    Commit summary rendering iterates every registered `AgentId`.
  - `Deprecated` — `Agent::session_dir` (use `AgentAdapter::local_root`).
  - `Fixed` — n/a (no user-visible defect).

## 17. Follow-Up

Each follow-up carries a loose target version and refers to the research
doc that will inform its design.

1. Specify Codex rollout identity and project mapping. **Target: ≥ 0.10.0**;
   see `docs/research/010-agent-session-integration-landscape.md` §Codex.
2. Validate binary/stream needs before snapshot or database adapters.
   **Target: ≥ 0.10.0**; research doc TBD.
3. Specify Gemini registry remapping and rewind-aware merge.
   **Target: ≥ 0.11.0**; see research doc §Gemini.
4. Design Hermes/OpenCode logical interchange without copying SQLite files.
   **Target: ≥ 0.12.0**; see research doc §Hermes, §OpenCode.
5. Revisit dynamic configuration only when a third provider requires it.
   **Target: opportunistic**, not before the second additional adapter lands.
