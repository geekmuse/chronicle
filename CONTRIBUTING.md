# Contributing to Chronicle

Thank you for your interest in contributing! This document explains how to get started.

## Quick Start

```bash
git clone https://github.com/YOUR_USERNAME/chronicle.git
cd chronicle
cargo build
cargo test
```

See the [Development Guide](docs/002-development-guide.md) for the full workflow.

## How to Contribute

### Reporting Bugs

Open a [GitHub issue](https://github.com/YOUR_USERNAME/chronicle/issues/new?template=bug_report.md) with:
- What you did
- What you expected
- What actually happened
- OS, Rust version (`rustc --version`), chronicle version

### Suggesting Features

Open a [GitHub issue](https://github.com/YOUR_USERNAME/chronicle/issues/new?template=feature_request.md) describing the use case. Features that fit the [project scope](docs/specs/001-initial-delivery.md) and come with a clear motivating scenario are most likely to be accepted.

### Submitting a Pull Request

1. Fork the repository and create a branch: `git checkout -b feat/my-thing`
2. Make your changes with tests
3. Ensure all checks pass:
   ```bash
   cargo test && cargo clippy -- -D warnings && cargo fmt --check && cargo deny check
   ```
4. Commit using [Conventional Commits](https://www.conventionalcommits.org/):
   ```
   feat(canon): add custom token support
   fix(merge): handle duplicate session headers
   ```
5. Open a PR against `main` — fill in the PR template

### Commit Message Format

```
<type>(<scope>): <short description>
```

Types: `feat`, `fix`, `docs`, `style`, `refactor`, `perf`, `test`, `chore`, `ci`

See [AGENTS.md](AGENTS.md) for the full convention reference.

## Code Style

- Rust: follow [Rust API Guidelines](https://rust-lang.github.io/api-guidelines/)
- Clippy: `cargo clippy -- -D warnings` must pass
- Format: `cargo fmt` (rustfmt wins — don't fight it)
- Every public item needs rustdoc comments
- Tests live in `mod tests` at the bottom of each source file; integration tests in `tests/`

## What We're Looking For

Good contributions:
- Fix a real bug with a regression test
- Add a documented, tested feature that fits the project scope
- Improve error messages to be more actionable
- Improve documentation clarity

Out of scope for now:
- Windows support (deferred — see architecture docs)
- Syncing agent settings, extensions, or themes
- Deletion propagation
- GUI / web UI

## License

By contributing, you agree that your contributions will be licensed under the [MIT License](LICENSE).
