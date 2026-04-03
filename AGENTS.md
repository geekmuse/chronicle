# AGENTS.md — Chronicle

> **Audience:** AI agents and automated tools working on this repository.
> **Read this file first** before making any changes.

## Project Overview

- **Name:** chronicle
- **Language:** Rust
- **Purpose:** Synchronize Pi and Claude Code session history across machines where `$HOME` paths differ, using path canonicalization and Git as the storage/transport backend
- **Current Version:** 0.5.0
- **Status:** Stable (v0.5.0)

## Quick Start — Quality Checks

After **ANY** code change, run all quality checks:

```bash
cargo test && cargo clippy -- -D warnings && cargo fmt --check && cargo deny check
```

Individual commands:

| Command | Purpose |
|---------|---------|
| `cargo test` | Run all unit and integration tests |
| `cargo clippy -- -D warnings` | Lint with warnings as errors |
| `cargo build` | Compile the project |
| `cargo fmt --check` | Check formatting (fix with `cargo fmt`) |
| `cargo deny check` | Verify dependency licenses against allowlist |

> **Rule:** All five checks must pass before committing. The pre-commit hook enforces `cargo fmt --check`, `cargo clippy`, `cargo test`, and `cargo deny check`.

## Repository Structure

```
chronicle/
├── README.md                        # Human-facing project documentation
├── AGENTS.md                        # THIS FILE — agent-facing guidance
├── CLAUDE.md                        # Claude-specific instructions
├── CHANGELOG.md                     # Version history (Keep a Changelog format)
├── CONTRIBUTING.md                  # Contribution guidelines
├── CODE_OF_CONDUCT.md               # Contributor Covenant code of conduct
├── SECURITY.md                      # Security policy and vulnerability reporting
├── LICENSE                          # MIT licence
├── Cargo.toml                       # Rust package manifest
├── Cargo.lock                       # Dependency lock file (committed)
├── deny.toml                        # cargo-deny licence/advisory allowlist
├── .editorconfig                    # Editor formatting rules
├── .gitattributes                   # Git line-ending normalization
├── .gitignore                       # Ignored files
├── .github/                         # GitHub-specific configuration
│   ├── ISSUE_TEMPLATE/
│   │   ├── bug_report.md
│   │   └── feature_request.md
│   ├── PULL_REQUEST_TEMPLATE.md
│   └── workflows/
│       ├── ci.yml                   # GitHub Actions CI (build/lint/test/deny/fuzz-build)
│       ├── release.yml              # GitHub Actions release (binary artefacts)
│       └── fuzz.yml                 # Weekly scheduled fuzz run (60 s, nightly)
├── .forgejo/                        # Forgejo-specific CI (mirrors .github/workflows)
│   └── workflows/
│       ├── ci.yml
│       ├── release.yml
│       └── fuzz.yml
├── fuzz/                            # cargo-fuzz sub-workspace
│   ├── Cargo.toml
│   ├── corpus/fuzz_roundtrip/          # Seed inputs for the fuzz target
│   └── fuzz_targets/
│       └── fuzz_roundtrip.rs            # libFuzzer round-trip invariant target
├── docs/                            # Project documentation
│   ├── 001-architecture.md          # System architecture and design
│   ├── 002-development-guide.md     # Development workflow and tooling
│   ├── 003-documentation-standards.md # How docs are structured
│   ├── specs/                       # Feature specifications and design docs
│   │   └── 001-initial-delivery.md  # Full project specification (v1.0)
│   ├── adrs/                        # Architecture Decision Records
│   │   └── 001-stale-lock-recovery.md # Stale lock recovery after sleep/suspend
│   ├── references/                  # CLI reference, config reference
│   ├── tasks/                       # Work items, backlogs
│   └── research/                    # Spikes, investigations
│       ├── 001-codebase-audit.md    # v0.2.2 audit; resolved in v0.3.0
│       ├── 002-sync-performance-investigation.md  # v0.4.x sync perf diagnosis; resolved in v0.4.2
│       └── 003-sync-performance-validation.md     # Independent validation of 002 findings
├── src/                             # Source code
│   ├── lib.rs                       # Library root (exposes modules; used by tests)
│   ├── main.rs                      # CLI entry point (clap)
│   ├── cli/
│   │   └── mod.rs                   # All CLI commands (init, import, sync, push,
│   │                                #   pull, status, errors, config, schedule)
│   ├── config/                      # Config loading, validation, precedence
│   │   ├── mod.rs
│   │   ├── schema.rs                # Serde structs for config.toml
│   │   └── machine_name.rs          # adjective-animal name generator
│   ├── canon/                       # Canonicalization / de-canonicalization
│   │   ├── mod.rs                   # Token registry, canonicalize/decanon entry points
│   │   ├── fields.rs                # L2 whitelisted field path walker
│   │   └── levels.rs                # L1/L2/L3 dispatch
│   ├── merge/                       # Grow-only set merge for JSONL
│   │   ├── mod.rs
│   │   ├── entry.rs                 # Entry identity (type + id), parsing
│   │   └── set_union.rs             # Grow-only set merge + prefix verification
│   ├── git/                         # Git operations (git2/libgit2)
│   │   ├── mod.rs                   # Repo init, working tree management
│   │   ├── fetch_push.rs            # Fetch, push with retry + backoff
│   │   └── commit.rs                # Staging, commit message formatting
│   ├── agents/
│   │   └── mod.rs                   # Pi and Claude dir encoding / file naming
│   ├── scheduler/                   # Cron scheduling
│   │   ├── mod.rs
│   │   └── cron.rs                  # Crontab read/write/install/uninstall
│   ├── errors/                      # Error ring buffer
│   │   ├── mod.rs
│   │   └── ring_buffer.rs           # 30-entry error ring buffer (JSONL file)
│   ├── materialize_cache.rs         # Materialization state cache (mtime/size, config hash)
│   ├── sync_state.rs                # sync_state.json schema, atomic write/read (last sync time/op/duration)
│   └── scan/                        # File change detection
│       └── mod.rs                   # mtime/size-based change detection + state cache
└── tests/
    └── integration.rs               # End-to-end multi-machine scenario tests
```

## Conventions

### Commit Messages

This project enforces [Conventional Commits](https://www.conventionalcommits.org/). Every commit **must** follow this format:

```
<type>(<scope>): <short description>    ← subject line (≤72 chars, imperative mood)

[optional body]                          ← what and why, not how (wrap at 72 chars)

[optional footer(s)]                     ← BREAKING CHANGE, issue refs, co-authors
```

**Types:**

| Type | When to use | Version impact |
|------|------------|----------------|
| `feat` | New feature or capability | MINOR bump |
| `fix` | Bug fix | PATCH bump |
| `docs` | Documentation only (no code change) | PATCH bump |
| `style` | Formatting, whitespace (no logic change) | PATCH bump |
| `refactor` | Code restructuring (no feature/fix) | PATCH bump |
| `perf` | Performance improvement | PATCH bump |
| `test` | Adding or fixing tests | PATCH bump |
| `chore` | Build, tooling, dependencies | PATCH bump |
| `ci` | CI/CD configuration | PATCH bump |

**Scopes** (use the module or area affected):

```
feat(canon): add L3 freeform text canonicalization
fix(merge): handle duplicate session headers
refactor(git): extract retry logic into helper
docs(spec): update merge algorithm description
test(canon): add property-based round-trip tests
chore(deps): update git2 to 0.19
feat(cli): add chronicle push --dry-run
fix(agents): correct Claude directory encoding
feat(scheduler): add cron interval mapping
```

When a commit spans multiple scopes, either omit the scope or use the primary area affected.

**Breaking changes:**

Use `!` after the type/scope OR add a `BREAKING CHANGE:` footer:
```
feat(config)!: rename sync_interval to schedule_interval

BREAKING CHANGE: The config key `sync_interval` has been renamed to
`schedule_interval`. Update your ~/.config/chronicle/config.toml.
```

**Multi-line commit body:**

Use the body to explain *what* changed and *why* (not *how* — the diff shows that):
```
refactor(canon): extract token registry into separate module

The token handling logic was growing complex with custom token support.
Splitting it into its own module improves testability and makes the
canonicalization pipeline easier to follow.
```

**Enforcement:** A `commit-msg` git hook validates the format. See `docs/002-development-guide.md` for setup.

### Branching

- `main` — stable, release-ready
- `feat/<name>` — new features
- `fix/<name>` — bug fixes
- `docs/<name>` — documentation changes
- `chore/<name>` — maintenance tasks

### Code Style

- Follow Rust API Guidelines and idiomatic Rust
- Use `clippy` with default + pedantic lints
- Prefer `Result<T, E>` over panics for recoverable errors
- Use `thiserror` for library errors, `anyhow` for application errors
- Prefer iterators over manual loops
- Use `#[must_use]` on functions with important return values
- Lifetime annotations only when the compiler requires them
- 4-space indentation (enforced by `rustfmt`)

### File Naming

- Source code: `snake_case.rs` in the appropriate module directory
- Docs: `NNN-kebab-case-title.md` within the appropriate `docs/` subdirectory
- Tests: `mod tests` blocks in source files (unit), `tests/*.rs` (integration)

## Documentation Rules

### Front-Matter (Required for all docs)

Every markdown file in `docs/` and its subdirectories must include:

```yaml
---
date_created: YYYY-MM-DD
date_modified: YYYY-MM-DD
status: draft | active | review | deprecated
audience: human | agent | both
cross_references:
  - docs/001-architecture.md
---
```

### Directory Purpose

| Directory | What goes here | When to create a file |
|-----------|---------------|----------------------|
| `docs/` (root) | Foundational, cross-cutting docs | New cross-cutting concern (e.g., security model) |
| `docs/specs/` | Feature specs, design docs | Before or during feature implementation |
| `docs/adrs/` | Architecture Decision Records | When making a significant technical decision |
| `docs/references/` | CLI reference, config reference, glossary | When a stable interface needs documentation |
| `docs/tasks/` | Work items, backlogs | When breaking down a body of work |
| `docs/research/` | Spikes, investigations, POCs | When evaluating a tool, approach, or pattern |

### Numbered Files

Files within each directory are numbered `NNN-kebab-case-title.md`:
- Sequential within each directory (001, 002, 003...)
- Leave gaps of 5–10 between files (001, 005, 010) to allow insertions
- **Never renumber existing files** — cross-references would break
- Numbers are scoped per directory — `specs/001-*.md` and `adrs/001-*.md` are independent

### Creating New Docs

1. Choose the correct subdirectory based on the purpose table above
2. Pick the next available number (use gaps; never renumber)
3. Include full front-matter with today's date
4. Add cross-references to related docs
5. Update this file's repository structure if adding a new directory

### Updating Existing Docs

1. Bump `date_modified` for substantive changes (not typos/formatting)
2. Update `status` if the document's lifecycle has changed
3. Add new cross-references if the update relates to other docs

## Versioning

**Semantic Versioning (semver):**

| Change Type | Version Bump | Example Commit |
|-------------|-------------|----------------|
| Breaking change | MAJOR | `feat(config)!: rename sync_interval` |
| New feature | MINOR | `feat(canon): add custom token support` |
| Bug fix | PATCH | `fix(merge): handle duplicate headers` |
| Docs/refactor | PATCH | `docs: update architecture diagrams` |

Update `CHANGELOG.md` with every version bump following [Keep a Changelog](https://keepachangelog.com/) format.

## Task Decomposition (for agents)

When picking up work:

1. **Read this file first** to understand current state
2. **Read `docs/001-architecture.md`** for system context
3. **Read `docs/specs/001-initial-delivery.md`** for the full specification
4. **Check `docs/tasks/`** for outstanding and in-progress work
5. **Check `CHANGELOG.md`** for recent changes and current version
6. **Break work into atomic tasks** — each task should:
   - Touch ≤5 files when possible
   - Have clear "done" criteria
   - Be completable in a single session
   - Be documented in `docs/tasks/` if non-trivial
7. **Commit frequently** with conventional commit messages
8. **Update docs** if your changes affect documented behavior

### Context Window Management

- Individual docs are kept under 500 lines
- Use cross-references (`see docs/specs/001-initial-delivery.md §5`) instead of duplicating
- Front-load critical information (inverted pyramid style)
- Prefer tables and lists over prose for structured data
- The spec (`docs/specs/001-initial-delivery.md`) is large — reference specific sections by number

### Decision Records

When making significant technical decisions:
1. Create an ADR in `docs/adrs/NNN-decision-title.md`
2. Include: Context, Decision, Consequences, Alternatives Considered
3. Set `status: proposed` until accepted
4. Never delete ADRs — set status to `deprecated` or `superseded`
5. Link to any `docs/research/` spikes that informed the decision

### Research & Spikes

When investigating a tool, approach, or pattern:
1. Create a research doc in `docs/research/NNN-topic.md`
2. Include: Goal, Findings, Recommendation, Links
3. Cross-reference the ADR or spec it informs
4. Set `status: active` when complete, `deprecated` when superseded

## Current Work Items

<!-- Agents: update this section as work progresses -->
<!-- For detailed task breakdowns, see docs/tasks/ -->

- [x] Cargo.toml and initial crate structure
- [x] Config module (schema, loading, validation, machine name generation)
- [x] Canonicalization engine (L1 paths, L2 whitelisted fields, token registry)
- [x] Merge module (entry parsing, set-union, prefix verification)
- [x] Git module (repo init, fetch/push with retry, commit formatting)
- [x] Agent modules (Pi and Claude directory encoding / file naming)
- [x] CLI commands (init, import, sync, push, pull, status, errors, config, schedule)
- [x] Scanner (mtime/size change detection)
- [x] Scheduler (crontab install/uninstall/status)
- [x] Error ring buffer
- [x] Integration tests
- [x] CI/CD pipeline
- [x] Sync performance fixes (v0.4.1–v0.4.2): state cache population, conditional materialize,
      `MaterializeCache` for O(1) re-materialize, advisory flock for concurrency safety
- [x] Rich `chronicle status` (v0.6.0): `sync_state.json` data layer, `StatusFormatter<W>` with
      ANSI/no-color/TTY detection, Config/Machine, Last Sync, Pending Files, Lock State, Scheduler
      sections; `--verbose`, `--porcelain`, `--no-color` flags
- [x] L3 canonicalization hardening (v0.7.0): expanded proptest generators (arb_home_path,
      arb_subpath with spaces/dots, content templates, deeply-nested JSON, array-of-strings);
      cargo-fuzz sub-workspace with fuzz_roundtrip libFuzzer target and seed corpus;
      fuzz-build step in CI + weekly scheduled fuzz.yml (GitHub + Forgejo)
