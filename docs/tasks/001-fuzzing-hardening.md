---
date_created: 2026-08-29
date_modified: 2026-08-29
status: active
audience: both
cross_references:
  - docs/specs/010-fuzzing-hardening.md
  - docs/specs/003-l3-canonicalization-hardening.md
---

# Task 001 — Persistent Fuzzing Hardening

## Objective

Address the review finding that Chronicle's weekly 60-second canonicalization
fuzz job was useful but too narrow to represent comprehensive security fuzzing.

## Completed Work

- [x] Add structured canonicalization semantics target.
- [x] Add structured JSONL merge-invariant target.
- [x] Retain the raw canonicalization round-trip target.
- [x] Add concise committed seeds for every target.
- [x] Build and execute every target in pull-request CI.
- [x] Run independent daily 10-minute scheduled campaigns.
- [x] Persist evolving corpora between successful runs.
- [x] Upload crash inputs and diagnostic corpora.
- [x] Pin compatible fuzz tooling and enforce runtime resource limits.
- [x] Mirror the policy across GitHub and Forgejo.
- [x] Update developer and project documentation.

## Done Criteria

The root quality suite passes, every fuzz target builds, and each target
completes a local smoke campaign without a crash.
