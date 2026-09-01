---
date_created: 2026-09-01
date_modified: 2026-09-01
status: active
audience: both
cross_references:
  - docs/001-architecture.md
  - docs/specs/001-initial-delivery.md
  - src/agents/mod.rs
  - src/scan/mod.rs
  - src/merge/entry.rs
  - src/canon/fields.rs
---

# AI Agent Session Integration Landscape

## Executive Summary

The best **near-term Chronicle integrations** are **Codex CLI**, **Gemini CLI**,
and **Qwen Code**. All three retain resumable sessions as append-only JSONL and
therefore align with Chronicle's file-oriented synchronization model.

The two most popular open-source candidates in this survey, **Hermes Agent** and
**OpenCode**, are still conceptually integratable, but both now use live SQLite
state. Chronicle must synchronize logical sessions through an adapter rather
than copy their database files. Hermes has a stable JSONL export path but no
corresponding restore/import path was found. OpenCode exposes structured session
state internally and over HTTP, but no stable offline export/import command was
found.

No new agent can be safely supported by merely adding another implementation of
Chronicle's current `Agent` trait. The scanner, merge identity, configuration,
CLI flow, and materializer all contain Pi/Claude or generic-JSONL assumptions.
An adapter abstraction is required before broadening support.

### Recommended order

1. Extract an agent adapter and per-agent merge policy.
2. Add Codex CLI as the first implementation and architecture proof.
3. Add Gemini CLI, including project-registry remapping.
4. Add Qwen Code, including its lossy project-directory encoding.
5. Add a logical database adapter, then target Hermes and OpenCode.
6. Consider Goose, Continue CLI, and Cline after the two priority tracks.

---

## Scope and Method

Popularity is ranked by GitHub stars captured on **2026-09-01**. Stars are a
visible, comparable proxy for open-source interest, not an install count or a
measure of active users. Proprietary products such as Cursor, GitHub Copilot,
Claude Code, and Amp do not expose comparable adoption data and are excluded
from the ranking. Claude Code is already supported by Chronicle.

Integration fit was evaluated against these requirements:

1. Full history is stored locally and can resume an agent session.
2. The durable representation can be exported without racing a live writer.
3. Sessions have stable identities and deterministic record ordering.
4. Project identity can be translated when `$HOME` differs across machines.
5. Concurrent additions can be merged without corrupting agent state.
6. Incoming history can be imported through a documented or stable interface.
7. Session data is separable from credentials and unrelated application state.

Upstream storage findings are pinned to exact commits in [Sources](#sources).

---

## Popularity Snapshot

| Rank | Agent | GitHub stars | Product shape | Repository status |
|------|-------|-------------:|---------------|-------------------|
| 1 | Hermes Agent | 239,289 | CLI, gateway, desktop | Active |
| 2 | OpenCode | 202,971 | CLI/TUI, desktop, server | Active |
| 3 | Codex CLI | 120,625 | Terminal coding agent | Active |
| 4 | Gemini CLI | 106,758 | Terminal coding agent | Active |
| 5 | OpenHands | 85,838 | Agent platform, SDK, CLI | Active |
| 6 | Cline | 67,274 | Editor extensions and CLI | Active |
| 7 | Goose | 53,781 | CLI and desktop | Active |
| 8 | Aider | 48,645 | Terminal pair programmer | Active |
| 9 | Continue | 35,723 | Editor extensions and CLI | Active |
| 10 | Crush | 27,847 | Terminal coding agent | Active |
| 11 | Qwen Code | 27,552 | Terminal coding agent | Active |
| 12 | Roo Code | 24,315 | Editor extension | Archived repository |

The ranking argues for eventual Hermes and OpenCode support. The technical fit
argues for proving the adapter model with Codex, Gemini, or Qwen first.

---

## Compatibility Findings

### Summary Matrix

| Agent | Durable session representation | Current fit | Estimated effort | Recommendation |
|-------|--------------------------------|-------------|------------------|----------------|
| Codex CLI | Append-only JSONL rollouts; rebuildable SQLite index | High | Medium | Implement first |
| Gemini CLI | Append-only JSONL per project; legacy JSON migration | High | Medium/large | Implement second |
| Qwen Code | Append-only JSONL under encoded project directories | High | Medium | Implement after Gemini or in parallel |
| Continue CLI | Mutable JSON snapshot per UUID | Medium | Medium | Later snapshot adapter |
| Cline | File-backed JSON stores and task history | Medium, unproven | Large | Validate exact resume payload first |
| Goose | SQLite; supported JSON export and resume-from-file | Medium | Large | Use CLI export/restore boundary |
| Hermes Agent | SQLite; stable JSONL export | Medium | Large | Priority DB adapter; seek import contract |
| OpenCode | SQLite WAL with normalized session/message tables | Medium/low | Extra large | Priority DB/API adapter; never copy DB |
| Crush | Per-project SQLite WAL database | Low | Extra large | Defer |
| Aider | Project-local Markdown history | Low | Medium | Do not prioritize |
| OpenHands | Platform/runtime state, not simple local session files | Low | Extra large | Out of initial scope |

“High” does not mean compatible with Chronicle's current merge implementation.
It means the upstream persistence model fits a file-oriented adapter without a
live database synchronization protocol.

### Codex CLI — strongest first target

Codex writes rollouts beneath
`~/.codex/sessions/YYYY/MM/DD/rollout-<timestamp>-<thread-id>.jsonl`. Resumed
sessions are opened for append. The thread ID is stable in the filename, while
reverted threads create a new immutable rollout ID. Codex also maintains SQLite
state, but its source explicitly scans JSONL rollouts to backfill/reconcile that
index. This makes the rollouts the safest Chronicle boundary.

Required work:

- Discover nested year/month/day files without treating date folders as projects.
- Extract project identity from the session metadata `cwd`, then map it locally.
- Use `ordinal` as the primary merge order/identity where present; do not use
  Chronicle's generic `(type, id)` key.
- Canonicalize nested metadata and tool payload paths, probably with an
  agent-specific field profile plus L3 coverage.
- Materialize files into the local Codex date hierarchy and allow Codex to
  reconcile its index.
- Handle archived and optionally compressed rollouts explicitly.

### Gemini CLI — strong fit with project-registry work

Gemini stores sessions in
`~/.gemini/tmp/<project_slug>/chats/session-<timestamp>-<id>.jsonl`. Writes are
append-only. Records include initial metadata, messages with stable IDs,
metadata updates (`$set`), and rewind markers (`$rewindTo`). Legacy `.json`
snapshots are migrated to JSONL when resumed.

The difficult part is project identity. Current Gemini uses a registry mapping
absolute project roots to short slugs and writes `.project_root` ownership
markers. A slug can differ between machines because collisions depend on each
machine's registry. Chronicle must map the canonical project root to the local
slug and regenerate local registry/marker state; copying a remote marker is not
correct.

Required work:

- Preserve metadata and rewind records that lack a top-level `type`.
- Merge message records by ID and metadata operations by stream position or a
  format-aware identity.
- Translate the project registry rather than canonicalizing only directory names.
- Respect Gemini's default retention/deletion policy; Chronicle's grow-only
  store would otherwise rematerialize sessions Gemini cleaned up.
- Include nested subagent session directories.

### Qwen Code — technically attractive, lower demand

Qwen Code writes append-only JSONL to
`~/.qwen/projects/<sanitized-cwd>/chats/<session-id>.jsonl` by default. Its
source describes the files as append-only and uses an explicit writer lease.
The runtime root can be overridden with `QWEN_HOME`, `QWEN_RUNTIME_DIR`, or
settings.

`sanitizeCwd` replaces every non-alphanumeric character with `-`, so project
decoding is lossy in the same class as Claude's encoding. The adapter should
prefer project metadata from the transcript and use the encoded directory only
as a fallback. It must avoid syncing transient `*.runtime.json` files next to
chat history.

### Hermes Agent — highly popular, database adapter required

Hermes replaced per-session JSONL files with `~/.hermes/state.db`, a SQLite WAL
database containing sessions, messages, FTS tables, model usage, routing,
compression lineage, workspace `cwd`, and other application state. Copying the
DB, `-wal`, and `-shm` files through Git is not a safe merge strategy.

Hermes does provide a stable JSONL export where each line is one complete
session object. This is a useful outgoing boundary, but no matching CLI
restore/import path was found. Chronicle support should therefore wait for one
of these:

1. an upstream Hermes JSONL import command,
2. a supported Hermes API for row-wise session/message upsert, or
3. a version-gated adapter using Hermes's own `SessionDB` library while Hermes
   is quiescent.

Hermes should be the first target for a database-backed adapter because it is
the most popular candidate and already supplies half of the logical interchange
contract.

### OpenCode — highly popular, largest integration gap

OpenCode stores state in an XDG data directory, normally
`~/.local/share/opencode/opencode.db`, with WAL enabled. The normalized schema
contains stable session, message, part, input, context, workspace, and project
IDs. This is mergeable at a logical row level, but not as a database file.

No current offline session export/import command was found. OpenCode has session
HTTP routes and internal storage services, so a supported API/plugin is the
preferred integration boundary. Direct DB access would require schema-version
gates, SQLite backup semantics, migration tracking, and transactional upserts.
It would also need to avoid syncing credentials and unrelated global state.

OpenCode is a strong product priority but a poor first implementation target.

### Goose — viable through its CLI boundary

Goose moved from individual JSONL files to
`~/.local/share/goose/sessions/sessions.db` in version 1.10.0. Legacy JSONL files
remain but are no longer managed. Goose can export a session as JSON and resume
from an exported JSON file. That gives Chronicle a safer interface than direct
SQLite copying, although automated enumeration, import semantics, duplicate
handling, and non-interactive operation need a spike.

### Continue CLI and Cline — mutable JSON adapters

Continue CLI stores one mutable JSON file per UUID under
`~/.continue/sessions/`. It is straightforward to discover and copy, but
concurrent changes require a snapshot-aware merge rather than append-only union.

Cline now documents shared file-backed JSON storage under `~/.cline/data/`
across VS Code, CLI, and JetBrains, including task history and per-workspace
state. A focused spike must identify which files are sufficient for complete
task resumption and confirm they exclude secrets before support is designed.

### Crush and Aider — weak fits

Crush stores sessions in a WAL SQLite database named `crush.db` inside each
project's `.crush` data directory by default. That is both project-local
(currently a Chronicle non-goal) and unsafe to merge as a file.

Aider's chat history is project-local Markdown intended primarily as a readable
transcript. It does not provide the same structured, multi-session resume model,
so synchronizing it would broaden Chronicle from agent session state into
project-local artifacts.

---

## Why the Current Chronicle Model Is Insufficient

Chronicle currently assumes:

- only Pi and Claude configuration sections and CLI branches;
- recursive discovery of files ending in `.jsonl`;
- project identity encoded in an agent-specific directory name;
- every mergeable record has a top-level `type` and usually `id`/`uuid`;
- all sessions use one grow-only JSONL set-union algorithm;
- incoming state can be materialized by writing a file directly; and
- partial history can be selected by file timestamps within project folders.

Codex lines use ordinals and flattened rollout variants. Gemini metadata and
rewind records do not have `type`. Snapshot agents rewrite whole JSON files.
SQLite agents require logical export and transactional import. Applying the
current generic merger would drop or collapse valid records.

---

## Proposed Adapter Direction

Introduce an adapter interface around lifecycle operations rather than only
path encoding:

```text
AgentAdapter
  discover() -> SessionRef[]
  export(SessionRef) -> CanonicalSession
  merge(local, remote) -> CanonicalSession
  import(CanonicalSession, LocalProjectMap)
  health_check() -> AgentHealth
```

Each adapter declares:

- native storage kind: append-only JSONL, mutable snapshot, or logical database;
- project identity strategy and local remapping behavior;
- record identity/order and conflict policy;
- canonicalization fields and whether L3 is required;
- writer locking/quiescence requirements;
- safe import mechanism and supported schema versions; and
- retention/deletion behavior.

The Git repository should store logical per-session artifacts, not live SQLite
files. A possible layout is:

```text
agents/<agent>/<canonical-project-id>/<session-id>.<adapter-format>
```

Database adapters should export deterministic records into that layout and
upsert them through an agent-supported API. SQLite backup files may be useful
for disaster recovery but are not suitable as Chronicle's merge unit.

---

## Recommended Spikes

1. **Codex fixture spike:** collect representative rollout variants and define
   an ordinal-aware merge law, including resume, revert, archive, and compaction.
2. **Gemini registry spike:** prove cross-machine slug/marker regeneration and
   merge metadata, rewind, message, and subagent records.
3. **Adapter ADR:** separate discovery, merge, project mapping, and import from
   hard-coded Pi/Claude CLI branches.
4. **Hermes contract spike:** test JSONL export completeness and propose an
   upstream import command/API.
5. **OpenCode API spike:** evaluate session HTTP routes for complete lossless
   export and transactional import without direct DB writes.
6. **Goose CLI spike:** test whether exported JSON can be restored
   non-interactively and idempotently.

---

## Sources

Popularity data came from GitHub repository metadata on 2026-09-01.

- [Hermes session storage](https://github.com/NousResearch/hermes-agent/blob/18a76be124d7c16ed98b629a358b23fef76a7f46/website/docs/developer-guide/session-storage.md)
  and [JSONL exporter](https://github.com/NousResearch/hermes-agent/blob/18a76be124d7c16ed98b629a358b23fef76a7f46/hermes_cli/session_export.py)
- [OpenCode database path](https://github.com/anomalyco/opencode/blob/ebece6efd7b11401cf1e7390b5a22991b6608cc4/packages/core/src/database/database.ts),
  [XDG paths](https://github.com/anomalyco/opencode/blob/ebece6efd7b11401cf1e7390b5a22991b6608cc4/packages/core/src/global.ts), and
  [session schema](https://github.com/anomalyco/opencode/blob/ebece6efd7b11401cf1e7390b5a22991b6608cc4/packages/core/src/session/sql.ts)
- [Codex rollout recorder](https://github.com/openai/codex/blob/90ae0c4ef944bb80a3c725d15910289dfbb7db51/codex-rs/rollout/src/recorder.rs),
  [rollout types](https://github.com/openai/codex/blob/90ae0c4ef944bb80a3c725d15910289dfbb7db51/codex-rs/history/src/lib.rs), and
  [index backfill](https://github.com/openai/codex/blob/90ae0c4ef944bb80a3c725d15910289dfbb7db51/codex-rs/rollout/src/metadata.rs)
- [Gemini session documentation](https://github.com/google-gemini/gemini-cli/blob/0bd1d439751478771c45d3d0895a6a9760554bf4/docs/cli/session-management.md),
  [JSONL recorder](https://github.com/google-gemini/gemini-cli/blob/0bd1d439751478771c45d3d0895a6a9760554bf4/packages/core/src/services/chatRecordingService.ts),
  [record types](https://github.com/google-gemini/gemini-cli/blob/0bd1d439751478771c45d3d0895a6a9760554bf4/packages/core/src/services/chatRecordingTypes.ts), and
  [project registry](https://github.com/google-gemini/gemini-cli/blob/0bd1d439751478771c45d3d0895a6a9760554bf4/packages/core/src/config/projectRegistry.ts)
- [Qwen append-only recorder](https://github.com/QwenLM/qwen-code/blob/2d6e8a61fa91d25ff0adc8ded9940ab6e8a9edae/packages/core/src/services/chatRecordingService.ts)
  and [project storage](https://github.com/QwenLM/qwen-code/blob/2d6e8a61fa91d25ff0adc8ded9940ab6e8a9edae/packages/core/src/config/storage.ts)
- [Goose storage and migration](https://github.com/aaif-goose/goose/blob/4ad43df42d8e6f5c9dae962d4cf4cbad2aadf3de/documentation/docs/guides/logs.md)
  and [CLI export/resume](https://github.com/aaif-goose/goose/blob/4ad43df42d8e6f5c9dae962d4cf4cbad2aadf3de/documentation/docs/guides/goose-cli-commands.md)
- [Continue CLI session storage](https://github.com/continuedev/continue/blob/5522c6f44ca0ac3528b37244818fbfa39b5af470/extensions/cli/src/session.ts)
- [Cline storage architecture](https://github.com/cline/cline/blob/8eb5f3d57f3eb87f21262f6ec2326ce460445dea/.clinerules/storage.md)
- [Crush SQLite connection](https://github.com/charmbracelet/crush/blob/8597c9b710de31170003eef37090582dd98c2959/internal/db/connect.go)
  and [project data directory](https://github.com/charmbracelet/crush/blob/8597c9b710de31170003eef37090582dd98c2959/internal/config/load.go)
- [Aider configuration options](https://github.com/Aider-AI/aider/blob/5dc9490bb35f9729ef2c95d00a19ccd30c26339c/aider/website/docs/config/options.md)
