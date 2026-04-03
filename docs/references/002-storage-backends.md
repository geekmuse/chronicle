---
date_created: 2026-03-31
date_modified: 2026-03-31
status: active
audience: human
cross_references:
  - docs/001-architecture.md
  - docs/references/001-encryption.md
  - docs/references/003-threat-model.md
  - docs/specs/001-initial-delivery.md
---

# Storage Backends — Why Git, and What About the Alternatives?

## Disclaimer

Chronicle is provided **as-is, without warranty of any kind**, express or implied.
By using Chronicle you accept all risk associated with the storage, transmission,
and synchronization of your session data. This includes all risks related to your
choice of Git hosting provider, their data handling practices, and the disposition
of data stored on their infrastructure. See [LICENSE](../../LICENSE) for full terms.

This document references third-party tools, services, and provider policies for
informational purposes only. Chronicle's documentation is **non-canonical reference
material** — it may become outdated as external tools and providers evolve. Always
consult the official documentation of any tool or service before relying on it:

- [Git documentation](https://git-scm.com/doc)
- [GitHub documentation](https://docs.github.com/)
- [GitLab documentation](https://docs.gitlab.com/)
- [Codeberg documentation](https://docs.codeberg.org/)
- [Bitbucket documentation](https://support.atlassian.com/bitbucket-cloud/)
- [Syncthing documentation](https://docs.syncthing.net/)
- [rclone documentation](https://rclone.org/docs/)

## Overview

Chronicle uses [Git](https://git-scm.com/) as its storage and transport backend.
Your canonicalized session history lives in a private Git repository, exchanged
between machines via standard `git push` / `git pull` over SSH or HTTPS.

This wasn't the only option considered. This document explains why Git was chosen,
how it compares to alternatives, and where its limitations are.

## The problem any backend must solve

Syncing AI session history across machines isn't a generic file-sync problem. Three
requirements are specific to this use case:

1. **Path canonicalization** — Session files and their contents embed `$HOME` paths
   that differ across machines. The directory `--Users-alice-Dev-foo--` on machine A
   is the same project as `--Users-bob-Dev-foo--` on machine B. Any backend needs a
   canonicalization layer on top — or the files just duplicate.

2. **Content-aware merge** — Session files are append-only JSONL. When two machines
   append different entries to the same file concurrently, a correct merge must
   union both entry sets. File-level conflict resolution (last-writer-wins or
   conflict copies) loses data.

3. **Asynchronous connectivity** — Developer machines may never be online at the
   same time. A desktop at home and a laptop at a café need to sync through an
   intermediary, not peer-to-peer.

No off-the-shelf sync tool solves all three. Chronicle solves (1) and (2)
internally. The backend only needs to solve (3) — reliable, asynchronous
transport and storage.

## Why Git

| Property | How Git delivers it |
|----------|---------------------|
| **Zero infrastructure cost** | [GitHub](https://github.com/pricing), [GitLab](https://about.gitlab.com/pricing/), [Codeberg](https://codeberg.org/), and [Bitbucket](https://www.atlassian.com/software/bitbucket/pricing) all offer free private repositories (see [Provider storage limits](#provider-storage-limits) below). |
| **Already configured** | Developers using Pi or Claude Code typically already have a Git hosting account and SSH keys. No new accounts or tooling required. |
| **Asynchronous by design** | Push when online, pull later. Devices never need to be online simultaneously. Operates through any NAT configuration. |
| **[Delta compression](https://git-scm.com/book/en/v2/Git-Internals-Packfiles)** | Session files are append-only JSONL. Git's packfile delta compression stores only the appended portion — a 200 KB append to a 50 MB file costs ~200 KB in the packfile, not 50 MB. |
| **Content-addressed dedup** | Identical content is stored once regardless of how many commits reference it. |
| **Full provenance** | Every sync cycle produces a commit with a timestamp and machine identity. `git log`, `git diff`, and `git blame` provide full audit capability. |
| **Transport security** | [SSH](https://datatracker.ietf.org/doc/html/rfc4253) or [TLS](https://datatracker.ietf.org/doc/html/rfc8446). |
| **Offline-first** | Each machine has a full local clone. Everything works offline; sync happens when connectivity is available. |

### What Git is not doing here

Chronicle uses Git as **transport and content-addressed storage only**. It never
uses Git's built-in merge. The sync cycle is:

```
fetch → Chronicle merges at the JSONL entry level → commit → push
```

If push is rejected (remote advanced), Chronicle retries: fetch → re-merge →
commit → push. Git's three-way merge algorithm is never invoked.

### Provider storage limits

Chronicle's Git repository grows over time as session data accumulates. The
following are the storage limits documented by major providers as of the date of
this document. **Verify current limits directly with your provider** — these may
change.

| Provider | Free-tier repo size limit | Documentation |
|----------|--------------------------|---------------|
| GitHub | 5 GB soft limit per repo (recommended); [hard limits vary](https://docs.github.com/en/repositories/working-with-files/managing-large-files/about-large-files-on-github) | [GitHub repo size limits](https://docs.github.com/en/repositories/working-with-files/managing-large-files/about-large-files-on-github) · [GitHub storage billing](https://docs.github.com/en/billing/managing-billing-for-git-large-file-storage/about-billing-for-git-large-file-storage) |
| GitLab.com | 10 GB per project (free tier) | [GitLab repository size limits](https://docs.gitlab.com/ee/administration/settings/account_and_limit_settings.html#repository-size-limit) |
| Codeberg | 1 GB per repo (soft limit; contact for exceptions) | [Codeberg FAQ](https://docs.codeberg.org/getting-started/faq/) |
| Bitbucket | 4 GB per repo (hard limit) | [Bitbucket repo limits](https://support.atlassian.com/bitbucket-cloud/docs/reduce-repository-size/) |

### Provider encryption at rest

Major Git hosting providers encrypt repository data at rest. This is transparent and
requires no user configuration. It protects against physical theft of storage media;
it does **not** protect against provider employee access, account compromise, or
legal requests. See [Encryption](001-encryption.md) for client-side options.

| Provider | Encryption at rest | Reference |
|----------|-------------------|-----------|
| GitHub | Encrypted disks (algorithm not publicly specified) | [Git data encryption at rest (2019)](https://github.blog/changelog/2019-05-23-git-data-encryption-at-rest/) · [GitHub security overview](https://github.com/security) |
| GitLab.com | AES-256 (Google Cloud Platform managed keys) | [GitLab encryption policy](https://handbook.gitlab.com/handbook/security/product-security/vulnerability-management/encryption-policy/) · [GitLab security practices](https://handbook.gitlab.com/handbook/security/) |
| Codeberg | Full disk encryption | [Codeberg privacy policy](https://codeberg.org/Codeberg/org/src/branch/main/PrivacyPolicy.md) |
| Bitbucket | AES-256 (AWS managed keys) | [Atlassian data security](https://www.atlassian.com/trust/security/data-management) · [Atlassian security practices](https://www.atlassian.com/trust/security) |
| Gitea (self-hosted) | Depends on your storage backend and disk encryption configuration | [Gitea documentation](https://docs.gitea.com/) |

> **Important:** Provider encryption policies and implementations change over time.
> Verify current practices directly with your chosen provider.

## Comparison with alternatives

### Peer-to-peer sync (Syncthing, Resilio Sync)

[Syncthing](https://syncthing.net/) ([documentation](https://docs.syncthing.net/))
is an open-source continuous file sync tool using peer-to-peer communication.
[Resilio Sync](https://www.resilio.com/) ([documentation](https://connect.resilio.com/hc/en-us))
is a proprietary alternative with similar architecture. Both are well-established
tools with active communities.

They do not solve Chronicle's use case, for two structural reasons:

1. **No asynchronous storage.** Syncthing is peer-to-peer. If two machines are
   never online at the same time, data doesn't sync. Syncthing's
   [relay servers](https://docs.syncthing.net/users/relaying.html) forward data
   in real-time between online peers — they do not store it. Asynchronous sync
   requires an always-on intermediary (a VPS, NAS, or similar), which constitutes
   self-hosted infrastructure.

2. **No content-aware merge.** When two machines append to the same session file
   concurrently, Syncthing creates a
   [`.sync-conflict` file](https://docs.syncthing.net/users/faq.html#what-if-there-is-a-conflict).
   One machine's entries end up in a conflict copy that neither Pi nor Claude Code
   reads. Resolving these conflicts requires the same JSONL-level merge logic that
   Chronicle implements.

**Where Syncthing has advantages:**

Syncthing's [Block Exchange Protocol](https://docs.syncthing.net/specs/bep-v1.html)
uses block-level hashing and transfer. When a 50 MB file grows by 200 KB, only the
new ~128 KB blocks are transferred. Git transfers deltas too (via packfiles), but
processes the full file content on each commit. For the file sizes typical in
session history (median under 1 MB, with occasional 10–50 MB outliers), this
difference is negligible in practice.

### Cloud storage sync (Dropbox, Google Drive, iCloud, OneDrive)

Consumer cloud sync services offer free tiers and work asynchronously through a
central store. They do not provide content-aware merge for JSONL data.

| Property | Cloud sync services | Git remote |
|----------|-------------------|------------|
| Free tier | 2–15 GB depending on provider | See [Provider storage limits](#provider-storage-limits) above |
| Conflict resolution | Last-writer-wins or conflict copies — same data-loss risk as Syncthing | Chronicle handles merge at the JSONL entry level; Git is transport only |
| Developer tooling | GUI apps, proprietary CLIs | `git` CLI, SSH keys |
| Automation | Provider-specific APIs, OAuth flows | `git push` / `git pull` from cron |
| Delta transfer | Provider-dependent; often whole-file | [Delta compression](https://git-scm.com/book/en/v2/Git-Internals-Packfiles) built into protocol |
| Provenance | Limited or no version history | Full commit history with machine identity |
| Encryption | Provider-controlled; varies | User's choice (see [Encryption](001-encryption.md)) |

Using cloud storage as a transport layer is technically possible but would still
require Chronicle's canonicalization and merge logic. It would replace `git push` /
`git pull` with a different transport while losing delta compression, content
addressing, and the commit-based provenance that Git provides.

### `rclone bisync` to cloud or SFTP

[rclone](https://rclone.org/) ([documentation](https://rclone.org/docs/)) provides
command-line access to dozens of storage backends.
[`rclone bisync`](https://rclone.org/bisync/) adds bidirectional sync capability.

| Property | Detail |
|----------|--------|
| Backend support | [70+ cloud and local backends](https://rclone.org/#providers) |
| Stability | `bisync` is [flagged as experimental](https://rclone.org/bisync/#cautions-and-limitations) by the rclone project |
| Conflict resolution | Conflict copies; no content-aware merge |
| Delta transfer | Full file upload on every change (no block-level or delta transfer) |
| Provenance | None — no history of what changed when |

`rclone bisync` would still require Chronicle's canonicalization and merge logic,
providing transport only. As the `bisync` feature matures, it may become a viable
alternative transport for users who prefer cloud storage backends over Git hosting.

### Plain Git (without Chronicle)

If `$HOME` is the same path on every machine (same username, same OS conventions),
the canonicalization problem does not exist. Session directory names match, embedded
paths match, and files can be synced with any tool — including plain Git:

```bash
# Simple git-based sync without Chronicle
cd ~/.pi/agent/sessions
git init && git remote add origin <url>
git add -A && git commit -m "sync" && git push
# On another machine:
git pull
```

This breaks when `$HOME` differs across machines — which is the use case Chronicle
is designed for. When paths are consistent, Chronicle's canonicalization layer is
unnecessary overhead.

## Known limitations of the Git backend

### Repository growth

Git stores every version of every file (delta-compressed in
[packfiles](https://git-scm.com/book/en/v2/Git-Internals-Packfiles)). Over time,
the repository grows. With 2+ GB of session data and no deletion propagation,
estimated growth (based on typical append-only JSONL workloads):

| Timeframe | Estimated repo size | Notes |
|-----------|-------------------|-------|
| Initial import | ~2 GB | Compressed via packfile |
| After 6 months | 3–5 GB | Mostly deltas from appends |
| After 1 year | 5–8 GB | Depends on session volume |

These estimates are within free-tier limits for most providers (see
[Provider storage limits](#provider-storage-limits)). If you approach the limit:

```bash
# Aggressive garbage collection
cd ~/.local/share/chronicle/repo
git gc --aggressive --prune=now

# Shallow clone on new machines
git clone --depth=1 <remote-url>
```

See the [Git documentation on `git gc`](https://git-scm.com/docs/git-gc) and
[shallow clones](https://git-scm.com/docs/git-clone#Documentation/git-clone.txt---depthltdepthgt)
for details.

### Large file performance

Git processes the full file content on each commit, even when only a small portion
changed. For the vast majority of session files (under 1 MB), this is
instantaneous. For the occasional 10–50 MB session, it adds a fraction of a second
to the sync cycle. This is not a practical bottleneck at current scale.

### No real-time sync

Chronicle syncs on a cron schedule (default: every 5 minutes). If you need a
session available on another machine within seconds of creating it, run
`chronicle push` manually. Real-time sync would require a long-running daemon,
which is a deliberate non-goal — see [Architecture](../001-architecture.md).

### Provider dependency

Your session history is stored on a third-party Git host. See
[Threat Model](003-threat-model.md) for the security implications and
[Encryption](001-encryption.md) for options to protect data from provider access.

You can reduce provider dependency by pushing to multiple remotes:

```bash
cd ~/.local/share/chronicle/repo
git remote set-url origin --add git@gitlab.com:yourname/chronicle-sessions.git
git remote set-url origin --add git@codeberg.org:yourname/chronicle-sessions.git
```

This is a [Git feature](https://git-scm.com/docs/git-remote), not a Chronicle
feature — but it works with Chronicle's sync workflow.

## External references

- [Git documentation](https://git-scm.com/doc) — Official Git reference
- [Git Internals — Packfiles](https://git-scm.com/book/en/v2/Git-Internals-Packfiles) — How delta compression works
- [Pro Git book](https://git-scm.com/book/en/v2) — Comprehensive Git reference (free)
- [libgit2](https://libgit2.org/) — The library Chronicle uses for Git operations ([API docs](https://libgit2.org/libgit2/))
- [Syncthing](https://syncthing.net/) — Open-source peer-to-peer sync ([documentation](https://docs.syncthing.net/), [BEP specification](https://docs.syncthing.net/specs/bep-v1.html))
- [Resilio Sync](https://www.resilio.com/) — Proprietary peer-to-peer sync ([documentation](https://connect.resilio.com/hc/en-us))
- [rclone](https://rclone.org/) — Command-line cloud storage tool ([bisync docs](https://rclone.org/bisync/))
- [GitHub pricing](https://github.com/pricing) · [GitHub storage limits](https://docs.github.com/en/repositories/working-with-files/managing-large-files/about-large-files-on-github) · [GitHub security](https://github.com/security)
- [GitLab pricing](https://about.gitlab.com/pricing/) · [GitLab usage quotas](https://docs.gitlab.com/ee/administration/settings/account_and_limit_settings.html#repository-size-limit) · [GitLab security](https://handbook.gitlab.com/handbook/security/)
- [Codeberg FAQ](https://docs.codeberg.org/getting-started/faq/) · [Codeberg privacy policy](https://codeberg.org/Codeberg/org/src/branch/main/PrivacyPolicy.md)
- [Bitbucket pricing](https://www.atlassian.com/software/bitbucket/pricing) · [Bitbucket repo limits](https://support.atlassian.com/bitbucket-cloud/docs/reduce-repository-size/) · [Atlassian security](https://www.atlassian.com/trust/security)
