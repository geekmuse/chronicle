# Chronicle

> Bidirectional sync for AI coding agent session history across machines, with path canonicalization and Git-backed storage.

## Overview

Chronicle synchronizes Pi and Claude Code session history across multiple machines where `$HOME` paths differ. It uses a canonicalization layer to abstract away per-machine path differences and Git as the storage and transport backend. Session files are merged using a grow-only CRDT (set-union), preserving the append-only invariant of JSONL session data.

## Features

- **Cross-machine sync** — Session history follows you between machines with different `$HOME` paths
- **Path canonicalization** — `$HOME` paths are replaced with `{{SYNC_HOME}}` tokens, with configurable canonicalization levels (paths, structured fields, freeform text)
- **CRDT merge** — Grow-only set merge ensures no session data is ever lost, even with concurrent edits on different machines
- **Partial materialization** — Pull only the N most recent sessions per project, while the Git repo retains complete history
- **Agent-agnostic** — Supports Pi and Claude Code with extensible agent architecture
- **Stateless CLI** — No daemon; a simple CLI invoked by cron on a configurable schedule

## Quick Start

### Prerequisites

- [Rust](https://www.rust-lang.org/tools/install) (latest stable)
- Git 2.x+
- A Git remote (GitHub, GitLab, Gitea, self-hosted — any host works)

### Installation

```bash
# Build from source
git clone https://github.com/geekmuse/chronicle
cd chronicle
cargo build --release

# Install the binary
cargo install --path .
```

### Usage

```bash
# First-time setup — creates config, generates machine name, inits local repo
chronicle init

# Import existing session history (one-time)
chronicle import

# Run a single sync cycle
chronicle sync

# Install cron schedule (runs every 5 minutes by default)
chronicle schedule install

# Check sync status
chronicle status
```

## Documentation

Detailed documentation lives in the [`docs/`](docs/) directory:

| Section | Path | Description |
|---------|------|-------------|
| Architecture | [`docs/001-architecture.md`](docs/001-architecture.md) | System design and key decisions |
| Development Guide | [`docs/002-development-guide.md`](docs/002-development-guide.md) | How to develop, test, and contribute |
| Doc Standards | [`docs/003-documentation-standards.md`](docs/003-documentation-standards.md) | How docs are structured and maintained |
| Specs | [`docs/specs/`](docs/specs/) | Feature specifications and design docs |
| ADRs | [`docs/adrs/`](docs/adrs/) | Architecture Decision Records |
| References | [`docs/references/`](docs/references/) | CLI reference, config reference, glossary |
| Tasks | [`docs/tasks/`](docs/tasks/) | Work items and implementation plans |
| Research | [`docs/research/`](docs/research/) | Spikes, investigations, POC write-ups |

## Development

```bash
# Clone the repository
git clone https://github.com/geekmuse/chronicle
cd chronicle

# Build
cargo build

# Run tests
cargo test

# Run linter
cargo clippy -- -D warnings
```

See [Development Guide](docs/002-development-guide.md) for full details.

## Contributing

1. Fork the repository
2. Create a feature branch (`git checkout -b feat/amazing-feature`)
3. Commit using [conventional commits](https://www.conventionalcommits.org/) (`git commit -m 'feat: add amazing feature'`)
4. Push to the branch (`git push origin feat/amazing-feature`)
5. Open a Pull Request

Please read [AGENTS.md](AGENTS.md) for project conventions and [docs/002-development-guide.md](docs/002-development-guide.md) for the full development workflow.

## Versioning

This project uses [Semantic Versioning](https://semver.org/). See [CHANGELOG.md](CHANGELOG.md) for release history.

## License

MIT — see [LICENSE](LICENSE) for details.

## Author

Brad Campbell
