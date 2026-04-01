---
date_created: 2026-03-31
date_modified: 2026-03-31
status: active
audience: human
cross_references:
  - docs/references/003-threat-model.md
  - docs/references/002-storage-backends.md
  - docs/001-architecture.md
  - docs/specs/001-initial-delivery.md
---

# Encryption — Options and Tradeoffs

## Disclaimer

Chronicle is provided **as-is, without warranty of any kind**, express or implied.
By using Chronicle you accept all risk associated with the storage, transmission,
and synchronization of your session data. This includes all risks related to your
choice of Git hosting provider, their data handling practices, and the disposition
of data stored on their infrastructure. See [LICENSE](../../LICENSE) for full terms.

This document references third-party tools, services, and provider policies for
informational purposes only. Chronicle's documentation is **non-canonical reference
material** — it may become outdated as external tools and providers evolve. Always
consult the official documentation of any tool or service before implementing it:

- [git-crypt documentation](https://github.com/AGWA/git-crypt#readme) ([source](https://github.com/AGWA/git-crypt))
- [git-remote-gcrypt documentation](https://spwhitton.name/tech/code/git-remote-gcrypt/) ([source & README](https://github.com/spwhitton/git-remote-gcrypt/blob/master/README.rst))
- [GnuPG (GPG) documentation](https://gnupg.org/documentation/)
- [GitHub security documentation](https://docs.github.com/en/github/authenticating-to-github)
- [GitLab security documentation](https://docs.gitlab.com/ee/security/)

## Overview

Chronicle stores your canonicalized AI session history in a private Git repository.
That repository contains the full text of every conversation you've had with Pi and
Claude Code — including source code, file paths, shell commands, and debugging
discussions. This is sensitive data.

Chronicle does **not** provide its own encryption layer. This is a deliberate design
decision, not an oversight. This document explains why, describes the available
third-party options, and details the tradeoffs each option carries so you can make
an informed choice.

## Why Chronicle doesn't encrypt natively

Three architectural constraints make native encryption impractical without
compromising Chronicle's core value proposition:

### 1. Whole-file encryption destroys Git delta compression

Chronicle's viability as a Git-backed tool depends on
[delta compression](https://git-scm.com/book/en/v2/Git-Internals-Packfiles). When
you append 200 KB of new session entries to a 50 MB file, Git stores a ~200 KB
delta — not a second copy of the 50 MB file.

Whole-file encryption ([AES-GCM](https://csrc.nist.gov/pubs/sp/800/38/d/final),
[ChaCha20-Poly1305](https://datatracker.ietf.org/doc/html/rfc7539), or any standard
[authenticated encryption](https://csrc.nist.gov/projects/block-cipher-techniques/bcm/modes-development)
scheme) produces ciphertext where a single changed byte alters the entire output.
Git sees a completely new file and stores the full content again:

```
Without encryption:  50 MB file + 100 appends × 200 KB ≈ 70 MB in packfile
With encryption:     50 MB file + 100 appends × 50 MB  ≈ 5 GB in packfile
```

At typical session volumes, this would exhaust free-tier Git hosting limits
(see [Storage Backends — Repository growth](002-storage-backends.md#repository-growth))
within weeks and make clone/fetch operations unusably slow.

### 2. Encryption breaks JSONL merge

Chronicle's merge algorithm parses each JSONL entry, keys on `(type, id)`, and
performs a grow-only set union across machines. Encrypted content cannot be parsed.
Any native encryption scheme would require:

1. Decrypt all entries in both versions
2. Merge in plaintext
3. Re-encrypt the merged result

This means plaintext exists on disk during every sync cycle — reducing the security
benefit to data-at-rest protection on the Git remote only. And re-encrypting the
merged file after every sync re-triggers the delta compression problem above.

### 3. Key management is the hard problem

Encryption algorithms are well-understood. Key management is where encryption
systems fail in practice. A native implementation would need to answer:

| Question | Complexity |
|----------|-----------|
| Where is the key stored on each machine? | OS keychain? File? Environment variable? |
| How does a new machine get the key? | Out-of-band transfer? Passphrase-derived? |
| What happens when the key is lost? | All synced history is **permanently unrecoverable** |
| How is key rotation handled? | Re-encrypt every entry in the repo? |
| Multiple users sharing a repo? | Key distribution, revocation, access control |

Getting any of these wrong produces a system that is either insecure (defeating the
purpose) or fragile (one lost key = permanent data loss). Mature, security-reviewed
tools already solve these problems. Chronicle defers to them rather than
reimplementing key management.

## What encryption protects (and doesn't)

Before choosing an encryption approach, understand the threat model. See
[Threat Model](003-threat-model.md) for full details. In summary:

| Threat | Encryption helps? |
|--------|-------------------|
| Git hosting provider reads your data | ✅ Yes |
| Provider data breach exposes your repo | ✅ Yes |
| Law enforcement / subpoena to provider | ✅ Yes |
| Your Git hosting account is compromised | ⚠️ Partially — attacker can't read, but can delete |
| Your local machine is compromised | ❌ No — plaintext sessions already on disk at `~/.pi/` and `~/.claude/` |
| Network eavesdropping | ❌ No — SSH/TLS already handles this |

Encryption protects against **provider-side access only**. If your primary concern
is local machine security or network interception, encryption of the Git remote
does not address those vectors.

## Option 1: `git-crypt` — transparent file-level encryption

[git-crypt](https://github.com/AGWA/git-crypt) provides transparent file-level
encryption in Git repositories using AES-256-CTR. Files are encrypted on push and
decrypted on checkout. See the
[git-crypt README](https://github.com/AGWA/git-crypt#readme) for complete usage
documentation, security properties, and known limitations.

### Example setup

> **Note:** The following is a summary for orientation. Consult the
> [git-crypt documentation](https://github.com/AGWA/git-crypt#readme) for
> authoritative instructions, especially regarding GPG key management.

```bash
# Install git-crypt (verify current install methods in git-crypt docs)
brew install git-crypt        # macOS
sudo apt install git-crypt    # Debian/Ubuntu

# Initialize in the Chronicle repo
cd ~/.local/share/chronicle/repo
git-crypt init

# Add your GPG key (you need a GPG key pair)
git-crypt add-gpg-user YOUR_GPG_KEY_ID

# Configure which files to encrypt
cat >> .gitattributes << 'EOF'
pi/**/*.jsonl filter=git-crypt diff=git-crypt
claude/**/*.jsonl filter=git-crypt diff=git-crypt
EOF

git add .gitattributes .git-crypt/
git commit -m "chore: configure git-crypt for session encryption"
```

### Adding another machine

```bash
# On the new machine, after chronicle init:
cd ~/.local/share/chronicle/repo
git-crypt unlock  # uses your GPG key (must be available on this machine)
```

Or export a symmetric key for machines without GPG:

```bash
# On a trusted machine
git-crypt export-key /path/to/keyfile

# On the new machine
git-crypt unlock /path/to/keyfile
```

### Tradeoffs

| Characteristic | Detail |
|----------------|--------|
| Transparency | Chronicle does not need to know about it — encryption is handled at the Git filter level |
| Delta compression | ⚠️ **Destroyed.** Every modified session file is stored in full on every commit. Monitor repo size accordingly |
| Key management | Requires GPG key infrastructure. New machines need the GPG key or an exported symmetric keyfile |
| Selective encryption | All files matching the `.gitattributes` pattern are encrypted; no per-file granularity within a pattern |
| Maturity | Established project; review the [git-crypt issue tracker](https://github.com/AGWA/git-crypt/issues) for known issues |

### Monitoring repo growth with git-crypt

With git-crypt enabled, repo growth accelerates because delta compression is
ineffective on encrypted content. Monitor and manage accordingly:

```bash
# Check repo size periodically
du -sh ~/.local/share/chronicle/repo/.git

# Aggressive garbage collection
cd ~/.local/share/chronicle/repo
git gc --aggressive --prune=now

# Shallow clone on new machines to avoid downloading full history
git clone --depth=1 <remote-url>
```

## Option 2: `git-remote-gcrypt` — full remote encryption

[git-remote-gcrypt](https://spwhitton.name/tech/code/git-remote-gcrypt/) encrypts
the **entire remote repository** — not just file contents but also commit messages,
filenames, branch names, and metadata. The remote is stored as opaque GPG-encrypted
data. See the
[git-remote-gcrypt README](https://github.com/spwhitton/git-remote-gcrypt/blob/master/README.rst)
for complete documentation.

### Example setup

> **Note:** The following is a summary for orientation. Consult the
> [git-remote-gcrypt documentation](https://spwhitton.name/tech/code/git-remote-gcrypt/)
> for authoritative instructions.

```bash
# Install (verify current install methods in git-remote-gcrypt docs)
brew install git-remote-gcrypt    # macOS
sudo apt install git-remote-gcrypt  # Debian/Ubuntu

# Reconfigure the Chronicle remote to use gcrypt
cd ~/.local/share/chronicle/repo
git remote set-url origin gcrypt::git@github.com:yourname/chronicle-sessions.git

# Set your GPG key
git config remote.origin.gcrypt-participants "YOUR_GPG_KEY_ID"

# Push (first push re-encrypts entire repo)
git push origin main
```

### Tradeoffs

| Characteristic | Detail |
|----------------|--------|
| Scope of encryption | Complete — metadata, filenames, commit history, everything. The provider sees only opaque blobs |
| Storage overhead | **Very high.** The entire repo is re-encrypted on every push |
| Performance | Push and pull operations are significantly slower than unencrypted Git |
| Key management | All participants need GPG keys |
| Partial/shallow clone | Not supported |
| Metadata leakage | None — the provider cannot determine file counts, commit frequency, or branch names |

## Option 3: Provider-managed encryption at rest

Some Git hosting providers encrypt repository data at rest on their storage
infrastructure. This is transparent to the user and requires no configuration:

| Provider | Encryption at rest | Reference |
|----------|-------------------|-----------|
| GitHub | Encrypted disks (algorithm not publicly specified) | [Git data encryption at rest (2019)](https://github.blog/changelog/2019-05-23-git-data-encryption-at-rest/) · [GitHub security overview](https://github.com/security) |
| GitLab.com | AES-256 (Google Cloud Platform managed keys) | [GitLab encryption policy](https://handbook.gitlab.com/handbook/security/product-security/vulnerability-management/encryption-policy/) · [GitLab security practices](https://handbook.gitlab.com/handbook/security/) |
| Codeberg | Full disk encryption | [Codeberg privacy policy](https://codeberg.org/Codeberg/org/src/branch/main/PrivacyPolicy.md) |
| Gitea (self-hosted) | Depends on your storage backend and disk encryption configuration | [Gitea documentation](https://docs.gitea.com/) |

Provider-managed encryption protects against physical theft of storage media. It
does **not** protect against provider employee access, account compromise, or
legal requests — the provider holds the decryption keys.

> **Important:** Provider policies and encryption implementations change over time.
> Verify current encryption practices directly with your provider before relying on
> them.

## Option 4: No additional encryption

A **private Git repository** with SSH key authentication and two-factor
authentication on the hosting account provides:

- Transport encryption via SSH or TLS
- Access control via SSH keys and account authentication
- Provider-managed encryption at rest (on major platforms — see Option 3)

This option preserves Chronicle's full performance characteristics: delta
compression, fast sync cycles, and efficient storage growth.

The tradeoff is that the Git hosting provider (and anyone who compromises your
hosting account) can read your session history in plaintext. See
[Threat Model](003-threat-model.md) for a detailed analysis of these vectors.

## Decision factors

The following factors are relevant when choosing an encryption approach. The
right choice depends on your threat model, compliance requirements, and tolerance
for the performance and operational tradeoffs described above.

| Factor | No encryption | Provider encryption | git-crypt | git-remote-gcrypt |
|--------|--------------|--------------------|-----------|--------------------|
| Delta compression preserved | ✅ | ✅ | ❌ | ❌ |
| Provider cannot read content | ❌ | ❌ | ✅ | ✅ |
| Provider cannot read metadata | ❌ | ❌ | ❌ | ✅ |
| Setup complexity | None | None | GPG key management | GPG key management |
| Repo growth rate | Normal | Normal | Accelerated | High |
| Shallow clone support | ✅ | ✅ | ✅ | ❌ |
| Key loss = permanent data loss | N/A | N/A | ✅ | ✅ |

## External references

- [NIST SP 800-38D — AES-GCM](https://csrc.nist.gov/pubs/sp/800/38/d/final) — The authenticated encryption standard referenced in this document
- [RFC 7539 — ChaCha20-Poly1305](https://datatracker.ietf.org/doc/html/rfc7539) — Alternative authenticated encryption construction
- [Git Internals — Packfiles](https://git-scm.com/book/en/v2/Git-Internals-Packfiles) — How Git delta compression works
- [git-crypt](https://github.com/AGWA/git-crypt) — Transparent file encryption in Git
- [git-remote-gcrypt](https://spwhitton.name/tech/code/git-remote-gcrypt/) — Full-remote GPG encryption for Git
- [GnuPG](https://gnupg.org/) — The GPG implementation used by both git-crypt and git-remote-gcrypt
- [Git data encryption at rest](https://github.blog/changelog/2019-05-23-git-data-encryption-at-rest/) — GitHub's 2019 announcement confirming encryption at rest via encrypted disks (algorithm not publicly specified)
- [GitHub security overview](https://github.com/security) — GitHub's security landing page
- [GitHub General Privacy Statement](https://docs.github.com/en/site-policy/privacy-policies/github-general-privacy-statement) — GitHub's privacy and data handling policy
- [GitLab encryption policy](https://handbook.gitlab.com/handbook/security/product-security/vulnerability-management/encryption-policy/) — GitLab's encryption-at-rest policy
- [GitLab cryptographic standard](https://handbook.gitlab.com/handbook/security/cryptographic-standard/) — GitLab's cryptographic practices
- [Codeberg privacy policy](https://codeberg.org/Codeberg/org/src/branch/main/PrivacyPolicy.md) — Codeberg's data handling
