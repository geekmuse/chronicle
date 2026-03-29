---
date_created: 2026-03-29
date_modified: 2026-03-29
status: draft
audience: both
cross_references:
  - docs/001-architecture.md
  - docs/003-documentation-standards.md
  - CLAUDE.md
---

# Development Guide — Chronicle

## Prerequisites

- [Rust](https://www.rust-lang.org/tools/install) (latest stable, via `rustup`)
- `cargo` (included with Rust)
- Git 2.x+
- A Git remote for testing sync (any host — GitHub, GitLab, Gitea, etc.)

## Getting Started

```bash
# Clone
git clone https://github.com/geekmuse/chronicle
cd chronicle

# Build
cargo build

# Verify setup
cargo test
```

## Development Workflow

### 1. Branch from main

```bash
git checkout main
git pull origin main
git checkout -b <type>/<short-name>
```

Branch types: `feat/`, `fix/`, `docs/`, `chore/`, `refactor/`, `test/`

### 2. Make Changes

- Follow the coding style in `CLAUDE.md`
- Keep changes focused — one logical change per branch
- Write/update tests for any behavior changes
- For non-trivial features, write a spec first in `docs/specs/`

### 3. Test

```bash
# Run full test suite
cargo test

# Run specific test
cargo test test_name

# Run tests for a specific module
cargo test --lib canon

# Run integration tests only
cargo test --test '*'
```

### 4. Lint & Format

```bash
# Check formatting
cargo fmt -- --check

# Auto-fix formatting
cargo fmt

# Lint (pedantic)
cargo clippy -- -D warnings
```

### 5. Commit

Use [conventional commits](https://www.conventionalcommits.org/):

```bash
git add .
git commit -m "feat(canon): add custom token support"
```

### 6. Push & PR

```bash
git push origin <branch-name>
```

## Project Structure

```
chronicle/
├── Cargo.toml                     # Package manifest
├── Cargo.lock                     # Dependency lock file
├── src/
│   ├── main.rs                    # CLI entry point (clap)
│   ├── cli/                       # Command handlers
│   │   ├── mod.rs
│   │   ├── init.rs                # chronicle init
│   │   ├── import.rs              # chronicle import
│   │   ├── sync.rs                # chronicle sync
│   │   ├── push.rs                # chronicle push
│   │   ├── pull.rs                # chronicle pull
│   │   ├── status.rs              # chronicle status
│   │   ├── errors.rs              # chronicle errors
│   │   ├── config.rs              # chronicle config
│   │   └── schedule.rs            # chronicle schedule
│   ├── config/                    # Configuration loading
│   │   ├── mod.rs
│   │   ├── schema.rs              # Serde structs for config.toml
│   │   └── machine_name.rs        # adjective-animal name generator
│   ├── canon/                     # Canonicalization engine
│   │   ├── mod.rs
│   │   ├── canonicalize.rs        # Local → canonical transforms
│   │   ├── decanon.rs             # Canonical → local transforms
│   │   ├── tokens.rs              # Token registry (SYNC_HOME + custom)
│   │   ├── fields.rs              # L2 whitelisted field path walker
│   │   └── levels.rs              # L1/L2/L3 dispatch
│   ├── merge/                     # JSONL merge
│   │   ├── mod.rs
│   │   ├── entry.rs               # Entry identity (type + id), parsing
│   │   ├── set_union.rs           # Grow-only set merge algorithm
│   │   └── verify.rs              # Prefix verification
│   ├── git/                       # Git operations
│   │   ├── mod.rs
│   │   ├── repo.rs                # Init, clone, working tree
│   │   ├── fetch_push.rs          # Fetch, push with retry + backoff
│   │   └── commit.rs              # Staging, commit message formatting
│   ├── agents/                    # Agent-specific logic
│   │   ├── mod.rs
│   │   ├── pi.rs                  # Pi dir encoding, file naming
│   │   └── claude.rs              # Claude dir encoding, file naming
│   ├── scheduler/                 # Cron scheduling
│   │   ├── mod.rs
│   │   └── cron.rs                # Crontab read/write/install
│   ├── errors/                    # Error handling
│   │   ├── mod.rs
│   │   ├── ring_buffer.rs         # 30-entry error ring buffer
│   │   └── types.rs               # Error category enum
│   └── scan/                      # File change detection
│       ├── mod.rs
│       └── diff.rs                # mtime/size-based change detection
├── tests/                         # Integration tests
├── docs/                          # Documentation
└── .editorconfig                  # Editor formatting rules
```

## Testing Strategy

| Layer | Tool | Location |
|-------|------|----------|
| Unit tests | `#[test]` / `#[cfg(test)]` | `mod tests` at bottom of source files |
| Integration tests | `#[test]` | `tests/` directory |
| Property tests | `proptest` | Alongside unit tests |

### Writing Tests

- Unit tests go in a `mod tests` block at the bottom of the source file
- Integration tests go in `tests/` and test cross-module behavior
- Use `proptest` for merge commutativity/associativity/idempotency and canonicalization round-trips
- Name tests descriptively: `test_canon_roundtrip_with_custom_tokens`
- Use `assert_eq!` with clear left/right labels

## Code Quality Tools

| Tool | Purpose | Command |
|------|---------|---------|
| rustfmt | Code formatting | `cargo fmt` |
| clippy | Static analysis (pedantic) | `cargo clippy -- -D warnings` |
| cargo-deny | License compliance, security advisories | `cargo deny check` |
| (compiler) | Type checking | `cargo check` |

## Git Hooks

Git hooks enforce code quality and commit conventions automatically. This project uses hooks to validate conventional commit messages and run checks before pushing.

### Setup

Hooks are installed automatically during project bootstrap. To reinstall manually:

```bash
# commit-msg hook (validates conventional commits)
cp docs/references/commit-msg-hook.sh .git/hooks/commit-msg
chmod +x .git/hooks/commit-msg
```

Or use the hook script inline — see the commit-msg Hook section below.

### Active Hooks

| Hook | What it does | Bypass (emergencies only) |
|------|-------------|--------------------------|
| `pre-commit` | Runs `cargo fmt -- --check` and `cargo clippy` on staged files | `git commit --no-verify` |
| `commit-msg` | Validates conventional commit format | `git commit --no-verify` |
| `pre-push` | Runs `cargo test` | `git push --no-verify` |

> ⚠️ **Do not use `--no-verify` routinely.** If a hook is failing, fix the underlying issue.

### commit-msg Hook

The `commit-msg` hook validates that every commit follows [Conventional Commits](https://www.conventionalcommits.org/):

```bash
#!/usr/bin/env bash
# .git/hooks/commit-msg

commit_msg=$(cat "$1")
pattern='^(feat|fix|docs|style|refactor|perf|test|chore|ci)(\(.+\))?(!)?: .{1,72}'

if ! echo "$commit_msg" | head -1 | grep -qE "$pattern"; then
  echo "ERROR: Commit message does not follow Conventional Commits format."
  echo ""
  echo "  Expected: <type>(<scope>): <description>"
  echo "  Types:    feat, fix, docs, style, refactor, perf, test, chore, ci"
  echo "  Example:  feat(canon): add custom token support"
  echo ""
  echo "  See: https://www.conventionalcommits.org/"
  exit 1
fi
```

### Commit Message Quick Reference

```
feat(scope): add new feature          → MINOR version bump
fix(scope): correct a bug             → PATCH version bump
docs: update documentation            → PATCH version bump
refactor(scope): restructure code     → PATCH version bump
feat(scope)!: breaking change         → MAJOR version bump
```

## CI/CD (Recommended)

A CI pipeline should include:

1. **Build** — `cargo build`
2. **Lint** — `cargo fmt -- --check` + `cargo clippy -- -D warnings`
3. **Test** — `cargo test`
4. **License & Advisories** — `cargo deny check`
5. **Doc Check** — Validate front-matter in `docs/` and subdirectories

### Doc Validation Script

```bash
# Simple front-matter validation for all docs (add to CI)
for f in $(find docs -name '*.md' -not -name '.gitkeep'); do
  head -1 "$f" | grep -q "^---$" || echo "MISSING FRONT-MATTER: $f"
done
```

## Releasing

### Version Bump

Edit `Cargo.toml` version field, or use `cargo-release`:

```bash
cargo install cargo-release
cargo release patch  # or minor, major
```

### Release Checklist

1. [ ] All tests pass (`cargo test`)
2. [ ] `CHANGELOG.md` updated with release notes under new version header
3. [ ] Version bumped in `Cargo.toml`
4. [ ] Git tag created: `git tag v<version>`
5. [ ] Tag pushed: `git push origin v<version>`

## Troubleshooting

### Common Issues

| Problem | Solution |
|---------|----------|
| Dependencies won't build | Delete `target/`, run `cargo build` again |
| `git2` fails to compile | Ensure `libssl-dev` / `openssl` and `pkg-config` are installed |
| Tests fail on clean checkout | Ensure Rust stable is at the latest version: `rustup update stable` |
| Formatting differs | Run `cargo fmt` and ensure `.editorconfig` is respected by your editor |
