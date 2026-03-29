# CLAUDE.md — Chronicle

> Claude-specific instructions for working in this repository.
> Read `AGENTS.md` first for general agent conventions.

## Project Context

- **Name:** chronicle
- **Language:** Rust
- **Description:** Bidirectional sync tool for AI coding agent session history across machines, using path canonicalization and Git as the storage/transport backend

## Coding Style

### Rust-Specific Rules

- Follow [Rust API Guidelines](https://rust-lang.github.io/api-guidelines/) and idiomatic Rust
- Use `clippy` with default + pedantic lints (`cargo clippy -- -D warnings`)
- Prefer `Result<T, E>` over panics for recoverable errors
- Use `thiserror` for library error types, `anyhow` for application-level errors
- Prefer iterators over manual loops
- Use `#[must_use]` on functions with important return values
- Lifetime annotations only when the compiler requires them
- Use `#[derive(Debug)]` on all public types
- Prefer `&str` over `String` in function parameters where ownership isn't needed

### General Rules

- Prefer explicit over implicit
- Favor composition over inheritance (traits + generics)
- Write small, focused functions (≤30 lines as a guideline)
- Name things clearly — avoid abbreviations except widely-known ones (e.g., `URL`, `ID`, `JSONL`)
- Every public API must have rustdoc documentation
- No commented-out code in commits — use version control history instead
- Error messages must be actionable: say what went wrong AND how to fix it

### Formatting

- Follow `.editorconfig` settings (4-space indent for Rust)
- Run `cargo fmt` before committing
- Let rustfmt win — do not manually override its decisions

### Imports / Dependencies

- Group: 1) `std`, 2) External crates, 3) Internal modules (`crate::`)
- Use `use` statements at module top
- Prefer specific imports over glob (`use std::collections::HashMap`, not `use std::collections::*`)
- Minimize dependencies — prefer `std` where possible
- New dependencies must pass `cargo deny check` (license allowlist in `deny.toml`)

## Testing

### Rust Testing Rules

- Unit tests: `#[cfg(test)] mod tests` at the bottom of each source file
- Integration tests: `tests/` directory
- Property tests: Use `proptest` for merge commutativity/associativity/idempotency and canonicalization round-trips
- Use `assert_eq!`, `assert_ne!`, `assert!()` with descriptive messages
- Name tests: `test_<function>_<scenario>_<expected>`

### General Testing Rules

- Test behavior, not implementation
- Each test should have a single clear assertion focus
- Use descriptive test names: `test_canon_roundtrip_preserves_non_home_paths`
- Avoid mocking unless testing integration boundaries (e.g., `git2` operations)
- Unit tests in `mod tests` at the bottom of the source file; integration tests in `tests/`

## Commit Messages

Use [Conventional Commits](https://www.conventionalcommits.org/) for every commit. See `AGENTS.md` for the full spec.

### Format

```
<type>(<scope>): <imperative description>
```

- Subject line: imperative mood ("add", not "added"), ≤72 characters, no period
- Scope: the module or area affected (optional but encouraged)
- Body: explain what and why, not how — wrap at 72 characters

### Examples for This Project

```bash
# Feature
git commit -m "feat(canon): add L3 freeform text canonicalization"

# Bug fix
git commit -m "fix(merge): handle duplicate session headers in set-union"

# Documentation
git commit -m "docs: update architecture doc with scheduler component"

# Multi-line with body
git commit -m "refactor(canon): extract token registry into separate module

The token handling logic was growing complex with custom token support.
Splitting it into its own module improves testability and makes the
canonicalization pipeline easier to follow."

# Breaking change
git commit -m "feat(config)!: rename sync_interval to schedule_interval

BREAKING CHANGE: The config key sync_interval has been renamed to
schedule_interval. Update ~/.config/chronicle/config.toml."

# Chore
git commit -m "chore(deps): update git2 to 0.19"

# Test
git commit -m "test(merge): add property-based commutativity tests"
```

### Rules

- One logical change per commit — don't bundle unrelated changes
- Run `cargo test` before committing
- Never use `--no-verify` to skip hooks

## File Creation Conventions

When creating new files:

- **Source files:** `src/<module>/snake_case.rs`
- **Test files:** `mod tests` in source file (unit), `tests/test_<name>.rs` (integration)
- **Feature specs:** `docs/specs/NNN-feature-name.md` with front-matter
- **ADRs:** `docs/adrs/NNN-decision-title.md` with front-matter
- **Reference docs:** `docs/references/NNN-topic.md` with front-matter
- **Task breakdowns:** `docs/tasks/NNN-task-name.md` with front-matter
- **Research/spikes:** `docs/research/NNN-topic.md` with front-matter
- **Config files:** Project root

See `AGENTS.md` for directory purpose and `docs/003-documentation-standards.md` for full rules.

## Patterns to Follow

- Builder pattern for complex construction (e.g., config, sync options)
- Newtype pattern for type safety (e.g., `CanonicalPath(String)` vs raw `String`)
- `From`/`Into` for type conversions
- Error enums with `thiserror` for each module
- `?` operator for error propagation
- Traits for abstraction boundaries (e.g., `Agent` trait for Pi/Claude)

## Anti-Patterns to Avoid

- `.unwrap()` in library code — use `?` operator or `expect("reason")`
- `.clone()` without justification — prefer borrowing
- `unsafe` without a `// SAFETY:` comment documenting the invariant
- Stringly-typed APIs — use enums, newtypes, or typed builders
- Nested `match` deeper than 2 levels — extract into functions
- Raw string manipulation for paths — use `std::path::PathBuf`

## Common Tasks

### Adding a New Feature

1. Create a feature branch: `git checkout -b feat/<name>`
2. Write a spec if non-trivial: `docs/specs/NNN-feature-name.md`
3. Implement the feature with tests
4. Update relevant docs if behavior changes
5. Update `CHANGELOG.md` under `[Unreleased]`
6. Commit with `feat(<scope>): <description>`

### Fixing a Bug

1. Create a fix branch: `git checkout -b fix/<name>`
2. Write a failing test that reproduces the bug
3. Fix the bug
4. Verify the test passes
5. Commit with `fix(<scope>): <description>`

### Making a Technical Decision

1. If research is needed, create `docs/research/NNN-topic.md` first
2. Create an ADR: `docs/adrs/NNN-decision-title.md`
3. Include context, decision, consequences, alternatives
4. Cross-reference the research doc if applicable
5. Commit with `docs: add ADR for <decision>`

### Updating Documentation

1. Identify the correct file and directory (see `AGENTS.md` directory table)
2. Edit the file
3. Bump `date_modified` in front-matter
4. Update cross-references if needed
5. Commit with `docs: <description>`

## Decision-Making Preferences

When multiple approaches are viable:

1. **Prefer `std`** over third-party crates for simple tasks
2. **Prefer readability** over cleverness
3. **Prefer existing patterns** in the codebase over introducing new ones
4. **Prefer small PRs** over large ones — split if possible
5. **When truly uncertain**, document the tradeoffs in an ADR (`docs/adrs/`) and pick the simpler option

## Key Specification Reference

The full project specification is at `docs/specs/001-initial-delivery.md`. Key sections:

| Section | Topic |
|---------|-------|
| §4 | Canonicalization (levels, token format, field whitelist, round-trip invariant) |
| §5 | Merge algorithm (grow-only set, entry identity, ordering, conflict resolution) |
| §6 | Git storage (repo structure, commit format, push retry, initial import) |
| §7 | Partial history materialization |
| §8 | Configuration (schema, precedence, machine identity) |
| §9 | CLI interface (all commands and flags) |
| §10 | Scheduling (cron-based) |
| §13 | Rust architecture (crate structure, dependencies) |
| §14 | Sync cycle detailed flow |
| §15 | Testing requirements (unit, integration, property-based) |
