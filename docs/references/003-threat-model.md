---
date_created: 2026-03-31
date_modified: 2026-03-31
status: active
audience: human
cross_references:
  - docs/references/001-encryption.md
  - docs/references/002-storage-backends.md
  - docs/001-architecture.md
  - docs/specs/001-initial-delivery.md
---

# Threat Model

## Disclaimer

Chronicle is provided **as-is, without warranty of any kind**, express or implied.
By using Chronicle you accept all risk associated with the storage, transmission,
and synchronization of your session data. This includes all risks related to your
choice of Git hosting provider, their data handling practices, and the disposition
of data stored on their infrastructure. See [LICENSE](../../LICENSE) for full terms.

This document is **not a formal security audit**. It is a practical guide to help
you understand the threat vectors relevant to Chronicle's data flow so you can make
informed decisions about your own risk tolerance and mitigations.

This document references third-party tools, services, and provider policies for
informational purposes only. Chronicle's documentation is **non-canonical reference
material** — it may become outdated as external tools and providers evolve. Always
consult the official documentation of any tool, service, or security practice before
relying on it:

- [GitHub security documentation](https://docs.github.com/en/authentication) · [GitHub security overview](https://github.com/security)
- [GitLab security documentation](https://docs.gitlab.com/ee/security/) · [GitLab security practices](https://handbook.gitlab.com/handbook/security/)
- [GitLab audit events](https://docs.gitlab.com/ee/user/compliance/audit_events.html)
- [OpenSSH documentation](https://www.openssh.com/manual.html)
- [Apple Platform Security (FileVault)](https://support.apple.com/guide/security/volume-encryption-with-filevault-sec4c6dc1b6e/web)
- [LUKS / dm-crypt documentation](https://gitlab.com/cryptsetup/cryptsetup/-/wikis/home)

## What's at stake

Your session history contains the full text of every conversation you've had with
Pi and Claude Code. Concretely, this includes:

- **Source code** — full file contents passed through tool calls
- **Shell commands** — captured in `arguments.command` fields, possibly including
  tokens, passwords, or connection strings
- **Project structure** — file paths, directory layouts, dependency trees
- **Debugging context** — error messages, stack traces, infrastructure details
- **Business logic** — design discussions, architecture decisions, feature planning
- **Your thought process** — the questions you ask when you're stuck, the mistakes
  you make, the approaches you consider and discard

Source code shows *what* you built. Session history also shows *how you think*,
*what you struggled with*, and *what you discussed* with an AI — which may include
topics you would not commit to a repository.

## Assumptions

Chronicle's security posture is built on these assumptions. If any are false for
your situation, adjust your mitigations accordingly.

| Assumption | If this is wrong... |
|---|---|
| The Git remote is a **private** repository | All session history is publicly readable. This is the most critical assumption. |
| You control SSH key access to the remote | Anyone with a valid SSH key can read and write your history. |
| Your Git hosting account has **2FA enabled** | Account takeover via password alone becomes feasible. See your provider's [2FA documentation](#external-references). |
| Your local machines are **not compromised** | Session files exist in plaintext at `~/.pi/` and `~/.claude/`. No amount of transport or at-rest encryption on the remote changes this. |
| Session files are **append-only** | Chronicle's merge algorithm assumes this. A compromised tool writing arbitrary content could poison the merge. |
| You accept the data handling practices of your chosen Git hosting provider | If not, consider client-side encryption ([Encryption](001-encryption.md)) or self-hosting. |

## Threat vectors

### T1: Git hosting provider access

**Threat:** Employees or systems of your Git hosting provider can access the
contents of your private repository.

**Likelihood:** Low for targeted human access (major providers implement internal
access controls and audit logging — see [GitHub staff access policy](https://docs.github.com/en/github/site-policy/github-privacy-statement),
[GitLab data classification standard](https://handbook.gitlab.com/handbook/security/data-classification-standard/)).
Higher for broad exposure via a provider-side data breach.

**Impact:** Full exposure of all session history across all machines and projects.

**Mitigations:**
- Accept the risk as commensurate with the trust you already place in the provider
  for source code hosting
- Enable client-side encryption via
  [git-crypt](https://github.com/AGWA/git-crypt) or
  [git-remote-gcrypt](https://spwhitton.name/tech/code/git-remote-gcrypt/) — see
  [Encryption](001-encryption.md) for tradeoffs
- Self-host your Git remote ([Gitea](https://docs.gitea.com/),
  [GitLab CE](https://about.gitlab.com/install/)) to eliminate the third party

### T2: Git hosting account compromise

**Threat:** An attacker gains access to your Git hosting account via stolen
credentials, phishing, or session hijacking.

**Likelihood:** Medium. Credential stuffing and phishing are
[common attack vectors](https://owasp.org/www-community/attacks/Credential_stuffing).

**Impact:** Full read access to all session history. Write access to inject or
delete data. If the attacker also has access to your source code repos (likely,
since it's the same account), the incremental impact of session exposure is
moderate.

**Mitigations:**
- Enable two-factor authentication on your Git hosting account:
  [GitHub 2FA](https://docs.github.com/en/authentication/securing-your-account-with-two-factor-authentication-2fa),
  [GitLab 2FA](https://docs.gitlab.com/ee/user/profile/account/two_factor_authentication.html),
  [Bitbucket 2FA](https://support.atlassian.com/bitbucket-cloud/docs/enable-two-step-verification/),
  [Codeberg 2FA](https://docs.codeberg.org/security/2fa/)
- Use SSH keys (not passwords) for Git operations
- Use a dedicated SSH key for Chronicle (limit blast radius via
  [deploy keys](https://docs.github.com/en/authentication/connecting-to-github-with-ssh/managing-deploy-keys))
- Review your provider's security/audit log periodically:
  [GitHub security log](https://docs.github.com/en/authentication/keeping-your-account-and-data-secure/reviewing-your-security-log),
  [GitLab audit events](https://docs.gitlab.com/ee/user/compliance/audit_events.html)
- Client-side encryption prevents *reading* history but not deletion

### T3: SSH key compromise

**Threat:** An attacker obtains your SSH private key (from disk, backup, or
another compromised service).

**Likelihood:** Low if keys are passphrase-protected and stored in the OS keychain.
Higher if keys are unencrypted on disk or copied to insecure locations.

**Impact:** Full read/write access to the Chronicle repo (and any other repo the
key has access to).

**Mitigations:**
- Passphrase-protect your SSH keys — see
  [OpenSSH key management](https://www.openssh.com/manual.html)
- Use the OS keychain / ssh-agent:
  [macOS Keychain integration](https://support.apple.com/guide/mac-help/use-keychains-to-store-passwords-mchlf375f392/mac),
  [ssh-agent(1)](https://man.openbsd.org/ssh-agent)
- Use a dedicated SSH key for Chronicle's repo with
  [deploy key](https://docs.github.com/en/authentication/connecting-to-github-with-ssh/managing-deploy-keys)
  permissions (read/write access to one repo only)
- Rotate keys periodically
- Use [Ed25519 keys](https://man.openbsd.org/ssh-keygen#Ed25519) (current best
  practice for SSH key generation)

### T4: Local machine compromise

**Threat:** An attacker gains access to one of your machines (malware, physical
access, stolen device).

**Likelihood:** Varies by environment. Physical theft of laptops is not uncommon.

**Impact:** Full access to all local session files in plaintext (`~/.pi/`,
`~/.claude/`), the local Git clone, the Chronicle config (including remote URL),
and SSH keys (if not passphrase-protected).

**Mitigations:**
- Full-disk encryption:
  [FileVault (macOS)](https://support.apple.com/guide/security/volume-encryption-with-filevault-sec4c6dc1b6e/web),
  [LUKS (Linux)](https://gitlab.com/cryptsetup/cryptsetup/-/wikis/home),
  [BitLocker (Windows)](https://learn.microsoft.com/en-us/windows/security/operating-system-security/data-protection/bitlocker/)
- Screen lock with strong login password
- Passphrase-protected SSH keys
- Remote wipe capability for mobile devices:
  [Find My Mac](https://support.apple.com/guide/icloud/erase-a-device-mmfc0ef36f/icloud)

**Note:** Chronicle does not increase this threat vector. The session files already
exist in plaintext on disk — Chronicle adds a Git clone of the canonicalized
versions. An attacker with local access can read the originals directly.

### T5: Legal / law enforcement access

**Threat:** A government entity issues a subpoena, court order, or national
security letter to your Git hosting provider for the contents of your repository.

**Likelihood:** Low for most individual developers. Higher for those working in
regulated industries, security research, or politically sensitive areas.

**Impact:** Full disclosure of session history to the requesting entity, potentially
without your knowledge (depending on jurisdiction and order type).

**Mitigations:**
- Client-side encryption (provider cannot comply with content requests because they
  don't have the plaintext) — see [Encryption](001-encryption.md)
- Self-hosted Git remote in a jurisdiction you control
- Review your provider's transparency report for context on their track record:
  [GitHub Transparency Center](https://transparencycenter.github.com/),
  [Atlassian transparency report](https://www.atlassian.com/trust/privacy/transparency-report)

### T6: Network eavesdropping

**Threat:** An attacker intercepts Git traffic between your machine and the remote.

**Likelihood:** Low when using SSH or HTTPS. Git over SSH uses the
[SSH transport protocol](https://datatracker.ietf.org/doc/html/rfc4253). Git over
HTTPS uses [TLS 1.2+](https://datatracker.ietf.org/doc/html/rfc8446).

**Impact:** None if using SSH or HTTPS (the traffic is encrypted). If using the
unencrypted `git://` protocol, full exposure.

**Mitigations:**
- Use SSH or HTTPS for the remote URL (verify your config: `chronicle config get
  general.remote_url`)
- Never use the unencrypted `git://` protocol
- Chronicle's cron entries use the remote URL from your config — verify it starts
  with `git@` (SSH) or `https://`

### T7: Canonicalization data leakage

**Threat:** The canonicalization process replaces `$HOME` paths with
`{{SYNC_HOME}}`, but other identifying information remains in session content:
usernames mentioned in conversation, project names, client names, server hostnames,
API endpoints, etc.

**Likelihood:** Certain. Canonicalization handles `$HOME` paths (and any custom
tokens you configure). It does not scrub arbitrary PII or sensitive data from
conversation content.

**Impact:** If the repository is exposed (via any of the threats above), the
attacker learns not just your code and thought process but also the identities and
infrastructure details discussed in your AI sessions.

**Mitigations:**
- Be mindful of what you discuss in AI sessions (good practice regardless of
  Chronicle)
- Use L2 canonicalization (default) to strip home paths from structured fields
- Consider L3 canonicalization for broader path stripping (with the caveat that it
  may alter conversation content — see the
  [spec §4.2](../specs/001-initial-delivery.md) for warnings)
- Custom tokens can canonicalize additional paths (e.g., `{{SYNC_PROJECTS}}` for
  project roots) but cannot scrub arbitrary text

### T8: Supply chain — agent format changes

**Threat:** Pi or Claude Code changes its session file format, directory encoding,
or storage location. Chronicle's canonicalization or merge logic breaks, potentially
corrupting session data during sync.

**Likelihood:** Medium-high. Both tools are under active development. Session
storage is an internal implementation detail of each tool, not a stable public API.

**Impact:** Data corruption during sync. Possible loss of session entries if the
merge algorithm misidentifies entries or writes malformed output.

**Mitigations:**
- Chronicle includes prefix verification to detect when the append-only invariant
  is violated (see the [spec §5.4](../specs/001-initial-delivery.md))
- Malformed JSONL lines are skipped, not propagated
  ([spec §5.5](../specs/001-initial-delivery.md))
- The error ring buffer records anomalies for diagnosis (`chronicle errors`)
- **Back up your session directories before upgrading Chronicle or the agents**
  (see [README backup instructions](../../README.md#before-you-start-back-up-your-existing-sessions))
- Pin Chronicle to a known-good version until you've verified compatibility after
  agent updates
- Watch the Chronicle changelog and issue tracker for agent compatibility notices

### T9: Concurrent sync corruption

**Threat:** Two cron-triggered sync cycles overlap (e.g., the previous cycle hasn't
finished when the next one fires), causing Git index corruption or merge errors.

**Likelihood:** Low under normal conditions (sync cycles complete in seconds).
Higher if the network is slow or the repo is large.

**Impact:** Git `index.lock` errors, failed sync cycles, or in the worst case,
a corrupted Git index requiring manual recovery.

**Mitigations:**
- Chronicle acquires an advisory file lock (`chronicle.lock`) before each sync
  cycle. Overlapping invocations exit cleanly.
- Stale locks (from a killed process or system crash) are detected and recovered
  automatically — see [ADR-001](../adrs/001-stale-lock-recovery.md).
- If you encounter persistent lock errors, verify no Chronicle process is hung and
  delete the lock file manually.

## Risk summary

| Threat | Likelihood | Impact | Primary mitigation |
|--------|-----------|--------|-------------------|
| T1: Provider access | Low | High | Accept or encrypt ([details](001-encryption.md)) |
| T2: Account compromise | Medium | High | 2FA ([provider docs](#t2-git-hosting-account-compromise)) |
| T3: SSH key compromise | Low | High | Passphrase + keychain |
| T4: Local machine compromise | Medium | High | Full-disk encryption |
| T5: Legal access | Low | High | Client-side encryption ([details](001-encryption.md)) |
| T6: Network eavesdropping | Low | High | SSH/HTTPS (default) |
| T7: Canonicalization leakage | Certain | Medium | Awareness + custom tokens |
| T8: Agent format changes | Medium-High | High | Backups + version pinning |
| T9: Concurrent sync | Low | Medium | Advisory lock (built-in) |

## Security checklist

Before using Chronicle, verify:

- [ ] Git remote repository is set to **private**
- [ ] Git hosting account has **2FA enabled**
- [ ] SSH key is **passphrase-protected** and loaded via ssh-agent / OS keychain
- [ ] All machines have **full-disk encryption** enabled
- [ ] You have a **backup** of your session directories (see
  [README](../../README.md#before-you-start-back-up-your-existing-sessions))
- [ ] The remote URL in your config uses `git@` (SSH) or `https://` — not `git://`
- [ ] You've reviewed what canonicalization level (L1/L2/L3) is appropriate for
  your data
- [ ] You've decided whether client-side encryption is warranted for your threat
  model (see [Encryption](001-encryption.md))

## External references

- [OWASP — Credential Stuffing](https://owasp.org/www-community/attacks/Credential_stuffing) — Context for account compromise threats
- [RFC 4253 — SSH Transport Layer Protocol](https://datatracker.ietf.org/doc/html/rfc4253) — SSH security properties
- [RFC 8446 — TLS 1.3](https://datatracker.ietf.org/doc/html/rfc8446) — HTTPS security properties
- [OpenSSH manual](https://www.openssh.com/manual.html) — SSH key generation and management
- [Apple Platform Security — FileVault](https://support.apple.com/guide/security/volume-encryption-with-filevault-sec4c6dc1b6e/web) — macOS full-disk encryption
- [LUKS / dm-crypt](https://gitlab.com/cryptsetup/cryptsetup/-/wikis/home) — Linux full-disk encryption
- [GitHub security overview](https://github.com/security) · [GitHub General Privacy Statement](https://docs.github.com/en/site-policy/privacy-policies/github-general-privacy-statement) · [GitHub Transparency Center](https://transparencycenter.github.com/)
- [GitLab security practices](https://handbook.gitlab.com/handbook/security/) · [GitLab encryption policy](https://handbook.gitlab.com/handbook/security/product-security/vulnerability-management/encryption-policy/)
- [Atlassian Trust Center](https://www.atlassian.com/trust/security) · [Atlassian transparency report](https://www.atlassian.com/trust/privacy/transparency-report)
- [Codeberg privacy policy](https://codeberg.org/Codeberg/org/src/branch/main/PrivacyPolicy.md)
