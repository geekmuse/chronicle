---
date_created: 2026-08-29
date_modified: 2026-08-29
status: active
audience: both
cross_references:
  - docs/specs/003-l3-canonicalization-hardening.md
  - docs/001-architecture.md
  - fuzz/fuzz_targets/fuzz_roundtrip.rs
  - fuzz/fuzz_targets/fuzz_canon_structured.rs
  - fuzz/fuzz_targets/fuzz_merge.rs
---

# Spec 010 — Persistent Security Fuzzing Hardening

## 1. Goal

Turn Chronicle's narrow weekly canonicalization fuzz check into a persistent,
reliable fuzzing programme for its highest-risk data-integrity boundaries.

## 2. Targets

| Target | Input style | Required properties |
|--------|-------------|---------------------|
| `fuzz_roundtrip` | Raw bytes decoded as home path, level, and JSON | L2/L3 canonicalize→decanonicalize round-trip for valid normalized JSON objects; malformed input must not panic |
| `fuzz_canon_structured` | `arbitrary`-derived semantic input | Eligible L2 fields are replaced; L2 isolation; L3 freeform scanning; boundary safety; idempotence; same-machine and cross-machine round trips; custom and configurable home tokens |
| `fuzz_merge` | `arbitrary`-derived entry sets plus malformed lines | Key-set union; unique output keys; valid output JSONL; one header; malformed-line accounting; remote-wins conflicts; idempotence; argument-order-independent key union |

The structured targets must cap generated collection and string sizes so one
input cannot consume the entire campaign. The scheduled workflow additionally
sets per-input, memory, and job-level limits.

## 3. Corpus Management

- Every target has a small committed seed corpus.
- Known crash inputs are reduced to concise regression seeds where possible.
- Scheduled runs restore the most recent successful corpus from CI cache.
- Successful runs save their evolved corpus for the next campaign.
- Every run uploads its corpus for short-term inspection.
- Failed runs upload `fuzz/artifacts/<target>/` for 30 days.

A failed run's corpus is not promoted automatically. The crash input must be
reviewed, minimized, and committed with a deterministic regression test.

## 4. CI Policy

### Pull requests and pushes

The normal CI workflow must:

1. Build every fuzz target using a pinned, known-compatible nightly and
   `cargo-fuzz` release.
2. Run every target for 15 seconds under AddressSanitizer.
3. Fail on a crash, panic, timeout, or memory-limit violation.

This is a regression smoke test, not a substitute for the scheduled campaign.

### Scheduled campaigns

GitHub and Forgejo run all targets independently every day. Each target gets:

- 600 seconds of mutation time;
- inputs up to 64 KiB;
- a 10-second per-input timeout;
- a 2 GiB RSS limit;
- a 20-minute job timeout;
- final libFuzzer statistics in the log.

Matrix jobs use `fail-fast: false` so one crash does not suppress the other
campaigns.

## 5. Reproducibility and Security

- The Rust nightly and `cargo-fuzz` versions are explicit and updated together.
- GitHub Actions are pinned to full commit SHAs.
- Workflow permissions remain read-only.
- Tool and build caches are keyed by toolchain, cargo-fuzz version, and both
  lockfiles.
- Scheduled jobs never execute code from an untrusted pull-request ref.

## 6. Acceptance Criteria

1. `cargo +nightly fuzz build` builds every target.
2. Each target completes at least 20,000 local executions without a crash.
3. PR CI executes, rather than merely compiles, every target.
4. Scheduled workflows run daily for 10 minutes per target.
5. Evolved corpora survive successful scheduled runs.
6. Crash artifacts remain downloadable after a failed runner exits.
7. Root quality checks and fuzz-workspace formatting/build checks pass.
