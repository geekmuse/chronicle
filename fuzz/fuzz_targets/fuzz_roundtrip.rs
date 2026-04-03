//! Fuzz target: round-trip invariant for the L2/L3 canonicalization pipeline.
//!
//! # Input layout (raw bytes)
//!
//! ```text
//! ┌──────────────────┬─────────────────┬────────────────────┬────────────────────┐
//! │ home_path_len:u8 │ level_byte:u8   │ home_path (UTF-8)  │ json_line (UTF-8)  │
//! │  (byte 0)        │  (byte 1)       │  [2 .. 2+len)      │  [2+len ..)        │
//! └──────────────────┴─────────────────┴────────────────────┴────────────────────┘
//! ```
//!
//! - `home_path_len` controls how many of the remaining bytes are the home path.
//! - `level_byte % 2 + 2` maps any byte value to canonicalization level 2 or 3.
//! - Both slices must be valid UTF-8; otherwise the target returns early.
//!
//! # Scope — JSON objects only
//!
//! Session JSONL lines are always JSON objects (`{...}`).  The target skips any
//! input that parses as a non-object (bare number, string, array, null/bool).
//! This also sidesteps a known serde_json limitation: large integer literals can
//! serialise to a different float representation than their source text (e.g.
//! `33333...` → `3.333e+31` → `3.33e+31`) — that non-idempotency only affects
//! bare numbers; JSON objects with reasonable field values round-trip stably.
//!
//! # serde_json normalisation (idempotency guard)
//!
//! serde_json serialises `Object` keys in alphabetical order (BTreeMap) and
//! may round-trip some float values to a shorter decimal.  The target
//! double-normalises the input and skips any input where two consecutive
//! `from_str → to_string` passes produce different bytes.  This restricts the
//! test to inputs for which `canonicalize_line` and `decanonicalize_line`
//! are guaranteed to be idempotent — which is exactly the class of inputs that
//! occurs in production (session files already use serde_json's output).
//!
//! # Invariant
//!
//! For a double-normalised JSON object, if `canonicalize_line` returns
//! `Ok(canonical)` then `decanonicalize_line(&canonical)` must return
//! `Ok(restored)` where `restored == normalised_input`.

#![no_main]

use std::collections::HashMap;
use std::path::Path;

use chronicle::canon::TokenRegistry;
use chronicle::config::schema::CanonicalizationConfig;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // ── Decode the structured input layout ───────────────────────────────────

    // At least 2 header bytes required.
    if data.len() < 2 {
        return;
    }

    let home_path_len = data[0] as usize;
    let level_byte = data[1];
    let rest = &data[2..];

    // Enough bytes for the home path?
    if rest.len() < home_path_len {
        return;
    }

    let home_bytes = &rest[..home_path_len];
    let json_bytes = &rest[home_path_len..];

    // Home path must be non-empty valid UTF-8.
    let home_str = match std::str::from_utf8(home_bytes) {
        Ok(s) if !s.is_empty() => s,
        _ => return,
    };

    // JSON fragment must be valid UTF-8.
    let json_str = match std::str::from_utf8(json_bytes) {
        Ok(s) => s,
        Err(_) => return,
    };

    // ── Parse and filter: JSON objects only ──────────────────────────────────
    //
    // Session JSONL lines are always JSON objects.  Bare numbers, strings,
    // arrays, null, and booleans are not valid session lines; skipping them
    // also avoids the serde_json large-integer float-precision issue.

    let json_value: serde_json::Value = match serde_json::from_str(json_str) {
        Ok(v) => v,
        Err(_) => return,
    };

    if !json_value.is_object() {
        return;
    }

    // ── Double-normalise: idempotency guard ──────────────────────────────────
    //
    // serde_json may serialise some float values differently across two
    // `from_str → to_string` passes.  We only proceed when two consecutive
    // normalisations produce identical bytes.  That is precisely the class of
    // JSON objects produced by serde_json itself, which is what the production
    // pipeline always operates on.

    let json_line_1 = serde_json::to_string(&json_value)
        .expect("serialising a parsed Value must succeed");

    let json_value_2: serde_json::Value = serde_json::from_str(&json_line_1)
        .expect("re-parsing serde_json output must succeed");
    let json_line_2 = serde_json::to_string(&json_value_2)
        .expect("re-serialising a parsed Value must succeed");

    // Skip non-idempotent inputs (typically float precision edge cases).
    if json_line_1 != json_line_2 {
        return;
    }

    let json_line = json_line_1;

    // ── Build the token registry ─────────────────────────────────────────────

    // Map any byte to canonicalization level 2 or 3.
    let level = (level_byte % 2) + 2;

    let config = CanonicalizationConfig {
        home_token: "{{SYNC_HOME}}".to_owned(),
        level,
        tokens: HashMap::new(),
    };
    let registry = TokenRegistry::from_config(&config, Path::new(home_str));

    // ── Guard: skip inputs that already embed the home token ─────────────────
    //
    // If the normalised JSON already contains "{{SYNC_HOME}}", de-canonicali-
    // sation would expand those occurrences to the home path even though canon
    // never inserted them.  Production session files never contain the literal
    // token string.
    if json_line.contains("{{SYNC_HOME}}") {
        return;
    }

    // ── Round-trip invariant ─────────────────────────────────────────────────
    //
    // For a double-normalised JSON object, canonicalize then decanonicalize
    // must be a lossless round-trip.

    let canonical = match registry.canonicalize_line(&json_line, level) {
        Ok(c) => c,
        // Malformed JSON (should be unreachable here, but tolerated).
        Err(_) => return,
    };

    let restored = registry
        .decanonicalize_line(&canonical)
        .expect("decanonicalize_line must not fail on well-formed canonical JSON");

    assert_eq!(
        restored,
        json_line,
        "round-trip invariant violated\n  home   = {home_str:?}\n  level  = {level}\n  input  = {json_line:?}\n  canon  = {canonical:?}\n  result = {restored:?}"
    );
});
