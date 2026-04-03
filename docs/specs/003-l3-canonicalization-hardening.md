---
date_created: 2026-04-03
date_modified: 2026-04-03
status: active
audience: both
cross_references:
  - docs/001-architecture.md
  - docs/specs/001-initial-delivery.md
  - src/canon/mod.rs
  - src/canon/levels.rs
  - src/canon/fields.rs
---

# Spec 003 — L3 Canonicalization Hardening

## 1. Goal

Strengthen confidence in the L3 (freeform text) canonicalization pipeline by:

1. Expanding property-based test generators to exercise a broader input space
   (special characters, spaces, Unicode, paths embedded in complex strings).
2. Adding a `cargo-fuzz` / libFuzzer fuzzing target that continuously hammers
   the canonicalization round-trip invariant.

The L2 whitelist (`fields.rs`) is **not** changed.  No new canonicalization
behaviour is introduced — this is purely a test-quality improvement.

## 2. Background

### 2.1 Current State

The canon module has solid deterministic tests and proptest round-trips.
However, the proptest generators are narrow:

- `arb_subpath()` generates only `[a-zA-Z0-9_][a-zA-Z0-9_-]{0,8}` components.
- The `content` field template is always `"file at {path} end"`.
- No tests cover paths with spaces, Unicode, or special characters.
- No tests cover paths embedded in arrays of strings or deeply nested objects.
- There is no fuzz target; coverage gaps can hide path-boundary edge cases.

### 2.2 Core Invariant

For any valid JSONL line `L`, home directory `H`, and level `k ∈ {2, 3}`:

```
decanon(canon(L, H, k), H) == L
```

This must hold for arbitrary JSON structure and arbitrary path-like strings
embedded in string values.

## 3. Scope

### 3.1 Expanded Proptest Generators

Extend the existing `proptest` suite in `src/canon/levels.rs` and
`src/canon/mod.rs`:

#### 3.1.1 Richer home-path components

Current generator produces `[a-zA-Z][a-zA-Z0-9]{2,10}` for the username
component.  Extend to include:

- Usernames with dots (e.g., `first.last`) — common on macOS.
- Usernames with hyphens (e.g., `brad-matic`).
- Longer home paths (e.g., `/home/ubuntu`, `/Users/First Last` — space in path).

**Note:** Spaces in home paths are valid on macOS.  The existing
`replace_in_text` and `boundary_match` functions must handle them correctly.
The proptest will exercise this and surface any latent bugs.

#### 3.1.2 Richer subpath components

Current generator: `[a-zA-Z0-9_][a-zA-Z0-9_-]{0,8}`, 1–4 components.
Extend to include:

- Components with spaces (`my project`, `foo bar`).
- Components with dots (`main.rs`, `v1.2.3`).
- Deeper paths (up to 8 components).

#### 3.1.3 Richer content templates

Current: `"file at {path} done"`.  Expand to a set of templates:

```
"{path}"                          — path only
"see {path} for details"          — embedded mid-sentence
"{path} and {path}"              — same path twice
"a: {path} b: {path2}"           — two different subpaths
"<tag attr=\"{path}\"/>"         — path in XML/HTML-like context
"https://example.com/not-a-path {path}"  — URL followed by path
```

`{path}` is a full `$HOME/subpath`; `{path2}` is a distinct subpath under the
same home.  Both must survive round-trip.

#### 3.1.4 Deeply nested JSON

New proptest strategy that generates arbitrary JSON objects up to 4 levels
deep, where leaf strings may or may not contain home paths.  Assert the
round-trip invariant at level 3.

#### 3.1.5 Array of strings

Add a proptest that places multiple home-path strings inside a JSON array
value and verifies all are canonicalized and restored.

### 3.2 Cargo-Fuzz Target

Add a `fuzz/` directory with a libFuzzer target for the canonicalization
round-trip.

#### 3.2.1 Target: `fuzz_roundtrip`

```rust
// fuzz/fuzz_targets/fuzz_roundtrip.rs
#![no_main]
use libfuzzer_sys::fuzz_target;
use chronicle::canon::TokenRegistry;
// ...

fuzz_target!(|data: &[u8]| {
    // Interpret data as (home_path_len_byte, level_byte, rest_is_json).
    // Build a TokenRegistry with the extracted home, pick level 2 or 3,
    // then assert: decanon(canon(line, level)) == line (or error on both sides).
});
```

**Invariant checked:** If `canonicalize_line(input, level)` succeeds with
output `C`, then `decanonicalize_line(C)` must succeed and equal `input`.
If `canonicalize_line` returns an error, the fuzz target does not assert
anything further (malformed JSON is expected to error).

#### 3.2.2 Corpus

Seed corpus consists of:
- The deterministic test vectors already in `levels.rs` serialized to files.
- A representative real Claude/Pi session JSONL line (sanitized — no real
  personal data; use `/Users/testuser/Dev/project` as the home).

#### 3.2.3 CI Integration

The fuzz target is **not** run on every CI push (it requires `cargo +nightly
fuzz run` and is slow).  Instead:

- The target must **compile** in CI (add a `cargo build` step for
  `fuzz/fuzz_targets/fuzz_roundtrip.rs` using nightly).
- A separate scheduled workflow (`fuzz.yml`) runs the target for 60 seconds on
  a cron schedule (e.g., weekly).
- Any crashes or panics are reported as workflow failures.

## 4. Dependencies

| Crate | Use | When |
|-------|-----|------|
| `cargo-fuzz` | fuzz harness CLI | dev-only, not in `Cargo.toml` |
| `libfuzzer-sys` | fuzz target runtime | `[dev-dependencies]` in `fuzz/Cargo.toml` |
| `proptest` | already present | existing dep |
| `arbitrary` | structured fuzzing input | `[dev-dependencies]` in `fuzz/Cargo.toml` |

`cargo-fuzz` uses a separate workspace in `fuzz/`; the root `Cargo.toml` is
not modified.

## 5. Out of Scope

- Expanding the L2 field whitelist.
- L3 semantics changes (false-positive suppression, URL detection).
- Mutation testing (`cargo-mutants`) — possible future work.
- Windows path support.

## 6. Acceptance Criteria

1. All existing tests continue to pass unchanged.
2. New proptest variants cover: spaces in home/subpath, dots in subpath,
   multiple path occurrences in one string, deeply nested JSON (3+ levels),
   and arrays of strings.
3. `cargo +nightly fuzz build fuzz_roundtrip` succeeds with no compile errors.
4. Running `cargo +nightly fuzz run fuzz_roundtrip -- -max_total_time=10`
   locally produces no crashes or panics against the seed corpus.
5. CI compiles the fuzz target without running it (build-only step in `ci.yml`).
6. A scheduled `fuzz.yml` workflow is added that runs the target for 60 seconds.
