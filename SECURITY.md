# Security Policy

## Supported Versions

Chronicle is pre-1.0 and under active development. Security fixes are applied to the latest release only.

| Version | Supported |
|---------|-----------|
| 0.x (latest) | ✅ |
| Older 0.x | ❌ |

## Reporting a Vulnerability

**Please do not open a public GitHub issue for security vulnerabilities.**

Report security issues by emailing **[INSERT SECURITY EMAIL]** with:

- A description of the vulnerability
- Steps to reproduce
- Potential impact
- Any suggested mitigations (optional)

You should receive an acknowledgment within **48 hours** and a more detailed response within **7 days** indicating next steps.

## Scope

Chronicle is a local CLI tool that reads session files from disk and pushes them to a user-configured Git remote. Areas of potential security concern include:

- **Path traversal** in canonicalization/de-canonicalization
- **Arbitrary file write** during pull/materialize operations
- **Git credential handling** (Chronicle delegates entirely to `git2`/libgit2 and the system credential store)
- **JSONL injection** via maliciously crafted session files

## Out of Scope

- Vulnerabilities in the user's Git remote or hosting provider
- Social engineering attacks
- Issues requiring physical access to the machine

## Disclosure Policy

Once a fix is ready, we will:
1. Release a patched version
2. Publish a security advisory on GitHub
3. Credit the reporter (unless they prefer to remain anonymous)
