//! L1/L2/L3 canonicalization dispatch for JSONL content lines.
//!
//! L1 (directory names) is handled at the filesystem layer; this module
//! handles L2 (whitelisted JSON fields) and L3 (all string values).

use serde_json::Value;

use crate::errors::ChronicleError;

use super::{fields, TokenRegistry};

/// Warning printed to stderr when level-3 canonicalization is enabled.
///
/// The CLI `sync` / `import` commands must emit this at startup when
/// `canonicalization.level >= 3`.
pub const L3_WARNING: &str = "\
⚠ WARNING: Level 3 canonicalization is enabled. All string content in session files\n  \
will be scanned for home directory paths. This may alter conversation content,\n  \
code snippets, and documentation references. Use with caution.";

impl TokenRegistry {
    /// Canonicalize the JSON content of a single JSONL line.
    ///
    /// | `level` | Behaviour |
    /// |---------|-----------|
    /// | `< 2`   | Returned unchanged (L1 paths handled at filesystem layer) |
    /// | `2`     | Only whitelisted JSON field paths are canonicalized |
    /// | `>= 3`  | All string values in the JSON object are canonicalized |
    ///
    /// The caller is responsible for emitting [`L3_WARNING`] at startup when
    /// `level >= 3`.
    ///
    /// # Errors
    ///
    /// Returns [`ChronicleError::MalformedLine`] when `line` is not valid JSON.
    /// Returns [`ChronicleError::CanonicalizationError`] if re-serialization fails
    /// (should be unreachable in practice).
    pub fn canonicalize_line(&self, line: &str, level: u8) -> Result<String, ChronicleError> {
        if level < 2 {
            return Ok(line.to_owned());
        }

        let mut value: Value =
            serde_json::from_str(line).map_err(|e| ChronicleError::MalformedLine {
                file: String::new(),
                line: 0,
                snippet: format!("invalid JSON: {e}"),
            })?;

        if level == 2 {
            if let Value::Object(obj) = &mut value {
                fields::apply_to_whitelisted(obj, |s| self.try_canonicalize_path(s));
            }
        } else {
            // Level >= 3: scan every string value.
            fields::apply_to_all_strings(&mut value, &mut |s| self.try_canonicalize_text(s));
        }

        serde_json::to_string(&value).map_err(|e| ChronicleError::CanonicalizationError {
            path: String::new(),
            message: format!("serialize error: {e}"),
        })
    }

    /// De-canonicalize all token occurrences in a single JSONL line.
    ///
    /// Applies global text replacement to **all** string values, reversing
    /// both L2 (whitelisted fields) and L3 (freeform text) canonicalization.
    ///
    /// # Errors
    ///
    /// Returns [`ChronicleError::MalformedLine`] when `line` is not valid JSON.
    /// Returns [`ChronicleError::CanonicalizationError`] if re-serialization fails.
    pub fn decanonicalize_line(&self, line: &str) -> Result<String, ChronicleError> {
        let mut value: Value =
            serde_json::from_str(line).map_err(|e| ChronicleError::MalformedLine {
                file: String::new(),
                line: 0,
                snippet: format!("invalid JSON: {e}"),
            })?;

        fields::apply_to_all_strings(&mut value, &mut |s| self.try_decanonicalize_text(s));

        serde_json::to_string(&value).map_err(|e| ChronicleError::CanonicalizationError {
            path: String::new(),
            message: format!("serialize error: {e}"),
        })
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::Path;

    use super::*;
    use crate::config::schema::CanonicalizationConfig;

    fn reg(home: &str) -> TokenRegistry {
        let cfg = CanonicalizationConfig {
            home_token: "{{SYNC_HOME}}".to_owned(),
            level: 2,
            tokens: HashMap::new(),
        };
        TokenRegistry::from_config(&cfg, Path::new(home))
    }

    fn reg_with_token(home: &str, token: &str, value: &str) -> TokenRegistry {
        let cfg = CanonicalizationConfig {
            home_token: "{{SYNC_HOME}}".to_owned(),
            level: 2,
            tokens: [(token.to_owned(), value.to_owned())].into_iter().collect(),
        };
        TokenRegistry::from_config(&cfg, Path::new(home))
    }

    // ── L2: whitelisted fields only ───────────────────────────────────────────

    #[test]
    fn l2_canonicalizes_cwd() {
        let reg = reg("/Users/bradmatic");
        let line = r#"{"type":"message","cwd":"/Users/bradmatic/Dev/foo","id":"1"}"#;
        let result = reg.canonicalize_line(line, 2).unwrap();
        let v: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(v["cwd"], "{{SYNC_HOME}}/Dev/foo");
        assert_eq!(v["id"], "1"); // unchanged
    }

    #[test]
    fn l2_canonicalizes_arguments_path() {
        let reg = reg("/Users/bradmatic");
        let line = r#"{"type":"tool","arguments":{"path":"/Users/bradmatic/file.rs"}}"#;
        let result = reg.canonicalize_line(line, 2).unwrap();
        let v: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(v["arguments"]["path"], "{{SYNC_HOME}}/file.rs");
    }

    #[test]
    fn l2_does_not_touch_non_whitelisted_fields() {
        let reg = reg("/Users/bradmatic");
        let line =
            r#"{"type":"message","content":"/Users/bradmatic/secret","cwd":"/Users/bradmatic/p"}"#;
        let result = reg.canonicalize_line(line, 2).unwrap();
        let v: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(v["content"], "/Users/bradmatic/secret"); // untouched
        assert_eq!(v["cwd"], "{{SYNC_HOME}}/p"); // canonicalized
    }

    #[test]
    fn l2_ignores_values_not_starting_with_home() {
        let reg = reg("/Users/bradmatic");
        let line = r#"{"cwd":"/tmp/other","path":"relative/path"}"#;
        let result = reg.canonicalize_line(line, 2).unwrap();
        let v: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(v["cwd"], "/tmp/other");
        assert_eq!(v["path"], "relative/path");
    }

    #[test]
    fn l2_ignores_partial_home_match() {
        let reg = reg("/Users/bradmatic");
        // "/Users/bradmatic2/foo" must NOT be canonicalized
        let line = r#"{"cwd":"/Users/bradmatic2/foo"}"#;
        let result = reg.canonicalize_line(line, 2).unwrap();
        let v: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(v["cwd"], "/Users/bradmatic2/foo");
    }

    #[test]
    fn l2_exact_home_value() {
        let reg = reg("/Users/bradmatic");
        let line = r#"{"cwd":"/Users/bradmatic"}"#;
        let result = reg.canonicalize_line(line, 2).unwrap();
        let v: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(v["cwd"], "{{SYNC_HOME}}");
    }

    #[test]
    fn l1_passthrough_when_level_less_than_2() {
        let reg = reg("/Users/bradmatic");
        let line = r#"{"cwd":"/Users/bradmatic/Dev"}"#;
        let result = reg.canonicalize_line(line, 1).unwrap();
        // Must be returned byte-identical when level < 2
        assert_eq!(result, line);
    }

    // ── L3: all string fields ─────────────────────────────────────────────────

    #[test]
    fn l3_canonicalizes_non_whitelisted_fields() {
        let reg = reg("/Users/bradmatic");
        let line = r#"{"type":"message","content":"output at /Users/bradmatic/Dev/foo end","cwd":"/Users/bradmatic/p"}"#;
        let result = reg.canonicalize_line(line, 3).unwrap();
        let v: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(v["content"], "output at {{SYNC_HOME}}/Dev/foo end");
        assert_eq!(v["cwd"], "{{SYNC_HOME}}/p");
    }

    #[test]
    fn l3_replaces_multiple_occurrences_in_one_string() {
        let reg = reg("/Users/bradmatic");
        let line = r#"{"content":"a: /Users/bradmatic/Dev/x b: /Users/bradmatic/Dev/y"}"#;
        let result = reg.canonicalize_line(line, 3).unwrap();
        let v: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(
            v["content"],
            "a: {{SYNC_HOME}}/Dev/x b: {{SYNC_HOME}}/Dev/y"
        );
    }

    #[test]
    fn l3_does_not_replace_partial_home_match() {
        let reg = reg("/Users/bradmatic");
        let line = r#"{"content":"/Users/bradmatic2/foo"}"#;
        let result = reg.canonicalize_line(line, 3).unwrap();
        let v: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(v["content"], "/Users/bradmatic2/foo");
    }

    // ── De-canonicalization ───────────────────────────────────────────────────

    #[test]
    fn decanon_restores_whitelisted_fields() {
        let reg = reg("/Users/bradmatic");
        let line = r#"{"cwd":"{{SYNC_HOME}}/Dev","path":"{{SYNC_HOME}}/file.rs"}"#;
        let result = reg.decanonicalize_line(line).unwrap();
        let v: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(v["cwd"], "/Users/bradmatic/Dev");
        assert_eq!(v["path"], "/Users/bradmatic/file.rs");
    }

    #[test]
    fn decanon_uses_local_home_path() {
        // Simulate de-canonicalization on a different machine
        let reg = reg("/home/brad");
        let line = r#"{"cwd":"{{SYNC_HOME}}/Dev/project"}"#;
        let result = reg.decanonicalize_line(line).unwrap();
        let v: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(v["cwd"], "/home/brad/Dev/project");
    }

    #[test]
    fn decanon_handles_exact_home_token() {
        let reg = reg("/Users/bradmatic");
        let line = r#"{"cwd":"{{SYNC_HOME}}"}"#;
        let result = reg.decanonicalize_line(line).unwrap();
        let v: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(v["cwd"], "/Users/bradmatic");
    }

    #[test]
    fn decanon_restores_l3_content_fields() {
        let reg = reg("/Users/bradmatic");
        let line = r#"{"content":"saved to {{SYNC_HOME}}/Dev/foo"}"#;
        let result = reg.decanonicalize_line(line).unwrap();
        let v: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(v["content"], "saved to /Users/bradmatic/Dev/foo");
    }

    // ── Round-trip invariant ──────────────────────────────────────────────────

    #[test]
    fn l2_round_trip() {
        let reg = reg("/Users/bradmatic");
        let line = r#"{"cwd":"/Users/bradmatic/Dev","path":"/Users/bradmatic/f.rs","content":"/Users/bradmatic/ignored"}"#;
        let canonical = reg.canonicalize_line(line, 2).unwrap();
        let restored = reg.decanonicalize_line(&canonical).unwrap();
        let orig: serde_json::Value = serde_json::from_str(line).unwrap();
        let rest: serde_json::Value = serde_json::from_str(&restored).unwrap();
        assert_eq!(orig, rest, "L2 round-trip failed");
    }

    #[test]
    fn l3_round_trip() {
        let reg = reg("/Users/bradmatic");
        let line = r#"{"content":"output: /Users/bradmatic/Dev/foo","cwd":"/Users/bradmatic/p"}"#;
        let canonical = reg.canonicalize_line(line, 3).unwrap();
        let restored = reg.decanonicalize_line(&canonical).unwrap();
        let orig: serde_json::Value = serde_json::from_str(line).unwrap();
        let rest: serde_json::Value = serde_json::from_str(&restored).unwrap();
        assert_eq!(orig, rest, "L3 round-trip failed");
    }

    #[test]
    fn round_trip_with_no_home_paths_is_identity() {
        let reg = reg("/Users/bradmatic");
        let line = r#"{"type":"session","id":"abc-123","timestamp":"2024-01-01T00:00:00Z"}"#;
        for level in [2u8, 3] {
            let canonical = reg.canonicalize_line(line, level).unwrap();
            let restored = reg.decanonicalize_line(&canonical).unwrap();
            // JSON round-trip may reorder keys; compare as Values
            let orig: serde_json::Value = serde_json::from_str(line).unwrap();
            let rest: serde_json::Value = serde_json::from_str(&restored).unwrap();
            assert_eq!(orig, rest, "identity round-trip failed at level {level}");
        }
    }

    // ── Custom token nesting ──────────────────────────────────────────────────

    #[test]
    fn custom_token_nested_under_sync_home_l2() {
        let reg = reg_with_token(
            "/Users/bradmatic",
            "{{SYNC_PROJECTS}}",
            "/Users/bradmatic/Dev",
        );
        let line = r#"{"cwd":"/Users/bradmatic/Dev/myproject"}"#;
        let canonical = reg.canonicalize_line(line, 2).unwrap();
        let v: serde_json::Value = serde_json::from_str(&canonical).unwrap();
        assert_eq!(v["cwd"], "{{SYNC_PROJECTS}}/myproject");
    }

    #[test]
    fn custom_token_round_trip_l2() {
        let reg = reg_with_token(
            "/Users/bradmatic",
            "{{SYNC_PROJECTS}}",
            "/Users/bradmatic/Dev",
        );
        let line = r#"{"cwd":"/Users/bradmatic/Dev/myproject","path":"/Users/bradmatic/other"}"#;
        let canonical = reg.canonicalize_line(line, 2).unwrap();
        let restored = reg.decanonicalize_line(&canonical).unwrap();
        let orig: serde_json::Value = serde_json::from_str(line).unwrap();
        let rest: serde_json::Value = serde_json::from_str(&restored).unwrap();
        assert_eq!(orig, rest, "custom token L2 round-trip failed");
    }

    #[test]
    fn custom_token_round_trip_l3() {
        let reg = reg_with_token(
            "/Users/bradmatic",
            "{{SYNC_PROJECTS}}",
            "/Users/bradmatic/Dev",
        );
        let line = r#"{"content":"opened /Users/bradmatic/Dev/main.rs"}"#;
        let canonical = reg.canonicalize_line(line, 3).unwrap();
        // Content should use custom token
        let v: serde_json::Value = serde_json::from_str(&canonical).unwrap();
        assert_eq!(v["content"], "opened {{SYNC_PROJECTS}}/main.rs");
        // And round-trip back
        let restored = reg.decanonicalize_line(&canonical).unwrap();
        let orig: serde_json::Value = serde_json::from_str(line).unwrap();
        let rest: serde_json::Value = serde_json::from_str(&restored).unwrap();
        assert_eq!(orig, rest, "custom token L3 round-trip failed");
    }

    // ── Malformed JSON ────────────────────────────────────────────────────────

    #[test]
    fn malformed_json_returns_error() {
        let reg = reg("/Users/bradmatic");
        assert!(reg.canonicalize_line("not json", 2).is_err());
        assert!(reg.decanonicalize_line("{broken").is_err());
    }

    // ── L3 warning constant ───────────────────────────────────────────────────

    #[test]
    fn l3_warning_contains_expected_text() {
        assert!(L3_WARNING.contains("Level 3 canonicalization is enabled"));
        assert!(L3_WARNING.contains("home directory paths"));
    }

    // ── Property-based round-trip tests (proptest) ────────────────────────────

    use proptest::prelude::*;

    /// Generate a valid path-like string under the given home.
    fn arb_subpath() -> impl Strategy<Value = String> {
        prop::collection::vec("[a-zA-Z0-9_][a-zA-Z0-9_-]{0,8}", 1..=4)
            .prop_map(|parts| parts.join("/"))
    }

    proptest! {
        /// Round-trip invariant at level 2: decanon(canon(line, A), A) == line.
        #[test]
        fn prop_l2_round_trip(subpath in arb_subpath()) {
            let home = "/Users/testuser";
            let reg = reg(&format!("{home}"));
            let full_path = format!("{home}/{subpath}");
            let line = format!(
                r#"{{"type":"message","cwd":"{full_path}","id":"1"}}"#
            );
            let canonical = reg.canonicalize_line(&line, 2).unwrap();
            let restored = reg.decanonicalize_line(&canonical).unwrap();
            let orig: serde_json::Value = serde_json::from_str(&line).unwrap();
            let rest: serde_json::Value = serde_json::from_str(&restored).unwrap();
            prop_assert_eq!(orig, rest);
        }

        /// Round-trip invariant at level 3: decanon(canon(line, A), A) == line.
        #[test]
        fn prop_l3_round_trip(subpath in arb_subpath()) {
            let home = "/Users/testuser";
            let reg = reg(home);
            let full_path = format!("{home}/{subpath}");
            let line = format!(
                r#"{{"type":"message","content":"file at {full_path} done","id":"1"}}"#
            );
            let canonical = reg.canonicalize_line(&line, 3).unwrap();
            let restored = reg.decanonicalize_line(&canonical).unwrap();
            let orig: serde_json::Value = serde_json::from_str(&line).unwrap();
            let rest: serde_json::Value = serde_json::from_str(&restored).unwrap();
            prop_assert_eq!(orig, rest);
        }

        /// Lines with no home paths are unchanged by both levels.
        #[test]
        fn prop_no_home_paths_unchanged(
            key in "[a-z]{3,8}",
            val in "[a-zA-Z0-9_]{1,16}"
        ) {
            let reg = reg("/Users/testuser");
            let line = format!(r#"{{"type":"session","{key}":"{val}"}}"#);
            for level in [2u8, 3] {
                let canonical = reg.canonicalize_line(&line, level).unwrap();
                let restored = reg.decanonicalize_line(&canonical).unwrap();
                let orig: serde_json::Value = serde_json::from_str(&line).unwrap();
                let rest: serde_json::Value = serde_json::from_str(&restored).unwrap();
                prop_assert_eq!(&orig, &rest);
            }
        }

        /// L2 round-trip with a random home path (§15.3).
        ///
        /// Varies both the user component of the home directory and the subpath
        /// to exercise `canonicalize_line(level=2)` → `decanonicalize_line`.
        #[test]
        fn prop_l2_round_trip_random_home(
            user in "[a-zA-Z][a-zA-Z0-9]{2,10}",
            subpath in arb_subpath(),
        ) {
            let home = format!("/Users/{user}");
            let r = reg(&home);
            let full_path = format!("{home}/{subpath}");
            let line = format!(
                r#"{{"type":"message","cwd":"{full_path}","id":"1"}}"#
            );
            let canonical = r.canonicalize_line(&line, 2).unwrap();
            let restored  = r.decanonicalize_line(&canonical).unwrap();
            let orig: serde_json::Value = serde_json::from_str(&line).unwrap();
            let rest: serde_json::Value = serde_json::from_str(&restored).unwrap();
            prop_assert_eq!(orig, rest, "L2 round-trip failed (home={})", home);
        }

        /// L3 round-trip with a random home path (§15.3).
        ///
        /// Embeds the full path inside a freeform `content` field so that
        /// L3 scanning is exercised end-to-end with a randomly generated home.
        #[test]
        fn prop_l3_round_trip_random_home(
            user in "[a-zA-Z][a-zA-Z0-9]{2,10}",
            subpath in arb_subpath(),
        ) {
            let home = format!("/Users/{user}");
            let r = reg(&home);
            let full_path = format!("{home}/{subpath}");
            let line = format!(
                r#"{{"type":"message","content":"file at {full_path} end","id":"1"}}"#
            );
            let canonical = r.canonicalize_line(&line, 3).unwrap();
            let restored  = r.decanonicalize_line(&canonical).unwrap();
            let orig: serde_json::Value = serde_json::from_str(&line).unwrap();
            let rest: serde_json::Value = serde_json::from_str(&restored).unwrap();
            prop_assert_eq!(orig, rest, "L3 round-trip failed (home={})", home);
        }
    }
}
