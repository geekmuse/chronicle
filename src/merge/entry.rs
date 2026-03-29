// Entry identity and parsing for JSONL session files (§5.1).
// Items here are foundational types consumed by set_union.rs (US-006),
// prefix verification (US-007), and the full sync pipeline (US-015/US-017).
// Allow dead-code until those callers are wired in.
#![allow(dead_code)]
// This module is the foundation for the grow-only set merge algorithm in
// set_union.rs and the prefix verification in US-007.

use serde_json::Value;

/// Composite identity key for a JSONL entry (§5.1).
///
/// Session headers (`type == "session"`) are identified by type alone —
/// there is exactly one per file. All other entries are identified by the
/// composite `(type, id)` key.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum EntryKey {
    /// The session header (`type == "session"`); at most one per file.
    Header,
    /// A regular entry identified by `(entry_type, id)`.
    Entry {
        /// The value of the JSON `type` field.
        entry_type: String,
        /// The value of the JSON `id` (or `uuid`) field.
        id: String,
    },
}

/// A successfully parsed JSONL entry.
#[derive(Debug, Clone)]
pub struct ParsedEntry {
    /// Composite identity key used for set-union operations.
    pub key: EntryKey,
    /// Original raw JSON string (preserved verbatim for output and
    /// byte-identical comparison in US-007).
    pub raw: String,
    /// ISO 8601 timestamp extracted from the entry, used for sort ordering.
    /// `None` if no recognised timestamp field is present.
    pub timestamp: Option<String>,
}

impl ParsedEntry {
    /// Returns `true` if this entry is a session header.
    #[must_use]
    pub fn is_header(&self) -> bool {
        self.key == EntryKey::Header
    }
}

/// Extract a timestamp string from a parsed JSON value.
///
/// Tries `timestamp`, `created_at`, and `createdAt` in that order.
/// Returns the first match found, or `None` if none are present.
#[must_use]
pub fn extract_timestamp(value: &Value) -> Option<String> {
    for field in ["timestamp", "created_at", "createdAt"] {
        if let Some(ts) = value.get(field).and_then(|v| v.as_str()) {
            return Some(ts.to_owned());
        }
    }
    None
}

/// Parse a single JSONL line into a [`ParsedEntry`].
///
/// Returns `None` if the line is empty, not valid JSON, not a JSON object,
/// or lacks a `type` field (all of which trigger a malformed-line warning in
/// the caller).
#[must_use]
pub fn parse_entry(raw: &str) -> Option<ParsedEntry> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }

    let value: Value = serde_json::from_str(trimmed).ok()?;
    let obj = value.as_object()?;

    let entry_type = obj.get("type")?.as_str()?.to_owned();

    let key = if entry_type == "session" {
        EntryKey::Header
    } else {
        // The spec uses `id`; Claude sessions use `uuid` — accept either.
        let id = obj
            .get("id")
            .or_else(|| obj.get("uuid"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_owned();
        EntryKey::Entry { entry_type, id }
    };

    let timestamp = extract_timestamp(&value);

    Some(ParsedEntry {
        key,
        raw: trimmed.to_owned(),
        timestamp,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Header detection ───────────────────────────────────────────────────

    #[test]
    fn session_header_produces_header_key() {
        let line = r#"{"type":"session","id":"s1","timestamp":"2024-01-01T00:00:00Z"}"#;
        let entry = parse_entry(line).expect("should parse");
        assert_eq!(entry.key, EntryKey::Header);
        assert!(entry.is_header());
    }

    #[test]
    fn non_session_type_produces_entry_key() {
        let line = r#"{"type":"message","id":"m1","timestamp":"2024-01-02T00:00:00Z"}"#;
        let entry = parse_entry(line).expect("should parse");
        assert_eq!(
            entry.key,
            EntryKey::Entry {
                entry_type: "message".to_owned(),
                id: "m1".to_owned(),
            }
        );
        assert!(!entry.is_header());
    }

    // ── Composite key: type + id ────────────────────────────────────────────

    #[test]
    fn model_change_type_uses_id() {
        let line = r#"{"type":"model_change","id":"mc42"}"#;
        let entry = parse_entry(line).expect("should parse");
        assert_eq!(
            entry.key,
            EntryKey::Entry {
                entry_type: "model_change".to_owned(),
                id: "mc42".to_owned(),
            }
        );
    }

    #[test]
    fn uuid_field_used_as_fallback_for_id() {
        // Claude sessions use "uuid" instead of "id".
        let line = r#"{"type":"human","uuid":"abc-123"}"#;
        let entry = parse_entry(line).expect("should parse");
        assert_eq!(
            entry.key,
            EntryKey::Entry {
                entry_type: "human".to_owned(),
                id: "abc-123".to_owned(),
            }
        );
    }

    #[test]
    fn missing_id_and_uuid_uses_empty_string() {
        let line = r#"{"type":"tool_result"}"#;
        let entry = parse_entry(line).expect("should parse");
        assert_eq!(
            entry.key,
            EntryKey::Entry {
                entry_type: "tool_result".to_owned(),
                id: String::new(),
            }
        );
    }

    // ── Timestamp extraction ────────────────────────────────────────────────

    #[test]
    fn timestamp_field_extracted() {
        let line = r#"{"type":"message","id":"x","timestamp":"2024-06-15T12:00:00Z"}"#;
        let entry = parse_entry(line).expect("should parse");
        assert_eq!(entry.timestamp.as_deref(), Some("2024-06-15T12:00:00Z"));
    }

    #[test]
    fn created_at_field_extracted_when_no_timestamp() {
        let line = r#"{"type":"message","id":"x","created_at":"2024-06-15T12:00:00Z"}"#;
        let entry = parse_entry(line).expect("should parse");
        assert_eq!(entry.timestamp.as_deref(), Some("2024-06-15T12:00:00Z"));
    }

    #[test]
    fn created_at_camel_extracted_when_no_timestamp() {
        let line = r#"{"type":"message","id":"x","createdAt":"2024-06-15T12:00:00Z"}"#;
        let entry = parse_entry(line).expect("should parse");
        assert_eq!(entry.timestamp.as_deref(), Some("2024-06-15T12:00:00Z"));
    }

    #[test]
    fn no_timestamp_field_gives_none() {
        let line = r#"{"type":"message","id":"x"}"#;
        let entry = parse_entry(line).expect("should parse");
        assert!(entry.timestamp.is_none());
    }

    // ── Malformed / edge cases ─────────────────────────────────────────────

    #[test]
    fn invalid_json_returns_none() {
        assert!(parse_entry("{not valid json}").is_none());
    }

    #[test]
    fn json_array_returns_none() {
        assert!(parse_entry("[1,2,3]").is_none());
    }

    #[test]
    fn missing_type_field_returns_none() {
        assert!(parse_entry(r#"{"id":"x","timestamp":"t"}"#).is_none());
    }

    #[test]
    fn empty_line_returns_none() {
        assert!(parse_entry("").is_none());
        assert!(parse_entry("   ").is_none());
    }

    #[test]
    fn raw_field_preserves_trimmed_original() {
        let line = "  {\"type\":\"session\"}  ";
        let entry = parse_entry(line).expect("should parse");
        assert_eq!(entry.raw, "{\"type\":\"session\"}");
    }

    // ── extract_timestamp unit tests ───────────────────────────────────────

    #[test]
    fn extract_timestamp_prefers_timestamp_over_created_at() {
        let v: serde_json::Value = serde_json::json!({
            "timestamp": "T1",
            "created_at": "T2"
        });
        assert_eq!(extract_timestamp(&v).as_deref(), Some("T1"));
    }

    #[test]
    fn extract_timestamp_falls_back_to_created_at() {
        let v: serde_json::Value = serde_json::json!({ "created_at": "T2" });
        assert_eq!(extract_timestamp(&v).as_deref(), Some("T2"));
    }

    #[test]
    fn extract_timestamp_returns_none_for_no_fields() {
        let v: serde_json::Value = serde_json::json!({ "foo": "bar" });
        assert!(extract_timestamp(&v).is_none());
    }
}
