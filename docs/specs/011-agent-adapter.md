---
date_created: 2026-09-01
date_modified: 2026-09-01
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
line canonicalization, set-union merge, and direct file materialization. Their
differences can be expressed as policies rather than branches.

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
| Merge | Existing keying, remote-wins behavior, ordering, and newlines remain unchanged |
| Import | One commit per non-empty provider project directory remains unchanged |
| Partial history | Pi uses filename time; Claude uses earliest recognized entry time |
| Caches | Existing state/materialization formats and keys remain valid |
| CLI | Commands, flags, porcelain keys, and exit behavior remain compatible |
| Commits | Existing two-provider summary text and order remain compatible |
| Doctor | `agents.pi` and `agents.claude` keys remain unchanged |

Existing checkouts must remain usable by binaries from before and after the
refactor. No migration command is permitted.

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

pub struct AgentMetadata {
    pub id: AgentId,
    pub display_name: &'static str,
    pub repo_root: &'static str,
    pub storage_kind: StorageKind,
}

pub enum StorageKind {
    AppendOnlyJsonl,
    MutableSnapshot,
    LogicalDatabase,
}
```

`AgentId` initially lists built-ins. Adding a variant is acceptable, but command
orchestration must not gain a branch. `StorageKind` is descriptive metadata and
must not become a coordinator switch.

### 7.2 Lifecycle values

```rust
pub struct AdapterContext<'a> {
    pub home: &'a Path,
    pub tokens: &'a TokenRegistry,
    pub canonicalization_level: u8,
    pub follow_symlinks: bool,
}

pub struct DiscoveredArtifact {
    pub local_path: PathBuf,
    pub local_mtime: DateTime<Utc>,
    pub local_size: u64,
    pub change_kind: ChangeKind,
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
`cfg.agents.pi` and `cfg.agents.claude` directly. It expands configured paths
and constructs adapters. `selected("all")` returns enabled adapters in stable
`AgentId` order; invalid selectors list supported IDs. Disabled adapters remain
registered for repository initialization and diagnostics.

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

The configured root is stored on each adapter. The old
`Agent::session_dir(home)` API remains available but is not the effective
configured path. Existing encode/decode behavior stays unchanged.

## 9. Orchestration Changes

### 9.1 Import

Resolve `--agent` through the registry and invoke a generic import coordinator.
Provider identity, roots, export, and project mapping come from the adapter.
Commit-per-project behavior remains coordinator-owned.

### 9.2 Sync and push

```text
for each enabled adapter in stable order
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
`adapter.recency`, then call `adapter.materialize`. Provider directory decoding
must leave `src/cli/mod.rs`. Materialization cache keys remain repository-relative
paths. The config hash changes only if canonical bytes change, which this
refactor forbids.

### 9.4 Status and doctor

Iterate metadata and adapter health results. Existing Pi/Claude labels and
porcelain keys remain exact. Pending-file status uses read-only discovery and
must not update caches, write provider state, or acquire provider write locks.

### 9.5 Repository and summaries

`RepoManager::ensure_working_tree` receives sorted roots from registry metadata.
Existing Pi/Claude roots are retained even when disabled.

Replace provider counters with deterministic accounting:

```rust
pub struct SyncSummary {
    pub new_files: usize,
    pub modified_files: usize,
    pub agent_totals: BTreeMap<AgentId, usize>,
}
```

With Pi and Claude, formatted output remains:

```text
+3 files, ~12 files (pi: 8, claude: 7)
```

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

1. Errors identify provider, operation, and artifact when known.
2. Repository paths must be relative, normalized, and under declared roots.
3. Materialization paths must remain under configured local roots.
4. Existing symlink policy remains enforced.
5. File writes retain current permission-safe behavior.
6. Adapters must exclude credentials, config, locks, runtime files, databases,
   WAL files, and shared-memory files unless a later spec declares otherwise.
7. Iteration and summaries are deterministic.
8. One bad artifact does not suppress another artifact or provider.
9. Enabled-adapter initialization fails before Git mutation begins.
10. Dry runs may read but never mutate provider state, caches, Git, or remotes.

## 12. Implementation Plan

### Phase 0 — Characterize behavior

- Add golden Pi/Claude export and materialization fixtures.
- Capture commit summaries, recency, cache keys, status, and doctor output.
- Add a two-provider end-to-end case; current integration tests emphasize Pi.

### Phase 1 — Add types and registry

- Add identity, metadata, errors, contexts, and registry.
- Construct Pi/Claude adapters from unchanged configuration.
- Keep the existing path-codec API.
- Derive repository initialization from descriptors.

### Phase 2 — Retrofit outgoing paths

- Implement common JSONL discovery, export, and merge.
- Convert import, sync, and push to registry iteration.
- Remove boolean dispatch and provider staging vectors.
- Generalize deterministic commit accounting.

### Phase 3 — Retrofit incoming paths

- Move project de-canonicalization, recency, filtering, and writes behind
  adapters.
- Convert pull and sync materialization to registry iteration.
- Preserve cache keys and partial-history selection.

### Phase 4 — Diagnostics and cleanup

- Convert status, pending counts, verbose output, and doctor.
- Remove provider branches outside registry/config compatibility code.
- Update architecture and user documentation.
- Run mechanical searches for forbidden boolean/provider dispatch.

Each phase must compile and leave completed-path tests passing.

## 13. Test Plan

### Unit and coordinator tests

- Registry order, enabled filtering, and selector errors.
- Metadata and configured-root expansion.
- L1/L2/L3 export and project-mapping parity.
- Merge parity, malformed lines, and remote-wins conflicts.
- Pi filename and Claude content recency parity.
- Path-containment rejection and health states.
- Deterministic summary formatting.
- A test adapter completes generic outgoing and incoming coordination without a
  provider branch.

### Integration tests

- Pi-only, Claude-only, and combined sync are byte-compatible.
- Disabled providers are skipped while repository roots remain.
- Existing repositories and caches work without migration.
- Import selectors retain current behavior.
- Pull applies each partial-history policy.
- A malformed provider artifact does not block valid work elsewhere.

### Quality checks

```bash
cargo test
cargo clippy -- -D warnings
cargo build
cargo fmt --check
cargo deny check
```

## 14. Acceptance Criteria

1. Pi and Claude are instantiated through `AdapterRegistry`.
2. Import, sync, push, materialization, status, and doctor iterate adapters.
3. `src/cli/mod.rs` has no `is_pi` or Pi-vs-Claude conditional.
4. Direct provider config access is confined to registry construction,
   serialization/tests, and compatibility code.
5. Repository roots derive from descriptors while existing paths remain exact.
6. Pi and Claude canonical fixtures remain byte-identical.
7. Existing repository and cache layouts require no migration.
8. Human and porcelain diagnostics remain compatible.
9. Commit summaries remain compatible and deterministic.
10. A test adapter traverses generic outgoing and incoming coordinator paths.
11. Existing public provider codec APIs remain available.
12. All quality checks pass.

## 15. Risks and Mitigations

| Risk | Mitigation |
|------|------------|
| Large CLI refactor regresses behavior | Characterization first; convert one lifecycle phase at a time |
| Trait is secretly JSONL-specific | Keep JSONL in a composed helper; lifecycle uses canonical artifacts |
| Premature generalization | Implement only operations exercised by current providers |
| Canonical bytes change | Golden fixtures and byte-parity assertions |
| Caches fully reprocess | Preserve paths, keys, hashes, and formats |
| Public API breaks | Keep current trait and codec structs as compatibility façade |
| Trait-object order varies | Stable registry order and `BTreeMap` accounting |
| Adapter escapes roots | Normalize and validate local and repository paths |
| Diagnostics mutate state | Separate and test read-only discovery/health behavior |

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

## 17. Follow-Up

1. Specify Codex rollout identity and project mapping.
2. Validate binary/stream needs before snapshot or database adapters.
3. Specify Gemini registry remapping and rewind-aware merge.
4. Design Hermes/OpenCode logical interchange without copying SQLite files.
5. Revisit dynamic configuration only when a third provider requires it.
