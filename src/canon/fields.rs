// Items in this module will be used by the sync pipeline (US-017) and import
// command (US-014). Allow dead-code until those stories wire it in.
#![allow(dead_code)]

//! L2 whitelisted JSON field path walker.
//!
//! Defines the set of JSON field paths that are canonicalized at level 2, and
//! provides helpers for applying a transform to exactly those fields (or to
//! every string value in a JSON tree for level 3).

use serde_json::{Map, Value};

/// Whitelisted JSON field paths for L2 canonicalization.
///
/// Each entry is `(top_level_key, Option<nested_key>)`.  When `nested_key` is
/// `None` the field is at the top level of the entry object; when it is
/// `Some(k)` the field is `top_level_key.k`.
///
/// Source: spec §4.3.
pub(crate) const WHITELISTED: &[(&str, Option<&str>)] = &[
    ("cwd", None),
    ("path", None),
    ("file_path", None),
    ("message", Some("cwd")),
    ("arguments", Some("path")),
    ("arguments", Some("file_path")),
    ("arguments", Some("command")),
];

/// Apply `transform` to every whitelisted string field in `obj`.
///
/// If `transform` returns `Some(new_value)` the field is updated; `None`
/// means "no change".  Non-string values and absent keys are silently skipped.
pub(crate) fn apply_to_whitelisted<F>(obj: &mut Map<String, Value>, mut transform: F)
where
    F: FnMut(&str) -> Option<String>,
{
    for &(top_key, nested_key) in WHITELISTED {
        match nested_key {
            None => {
                if let Some(Value::String(s)) = obj.get_mut(top_key) {
                    if let Some(new_val) = transform(s) {
                        *s = new_val;
                    }
                }
            }
            Some(inner_key) => {
                if let Some(Value::Object(inner)) = obj.get_mut(top_key) {
                    if let Some(Value::String(s)) = inner.get_mut(inner_key) {
                        if let Some(new_val) = transform(s) {
                            *s = new_val;
                        }
                    }
                }
            }
        }
    }
}

/// Recursively apply `transform` to **every** string value in `value` (L3).
///
/// Traverses objects, arrays, and nested structures.  Non-string leaf values
/// (numbers, booleans, null) are passed through unchanged.
pub(crate) fn apply_to_all_strings<F>(value: &mut Value, transform: &mut F)
where
    F: FnMut(&str) -> Option<String>,
{
    match value {
        Value::String(s) => {
            if let Some(new_val) = transform(s) {
                *s = new_val;
            }
        }
        Value::Array(arr) => {
            for item in arr.iter_mut() {
                apply_to_all_strings(item, transform);
            }
        }
        Value::Object(obj) => {
            for v in obj.values_mut() {
                apply_to_all_strings(v, transform);
            }
        }
        _ => {}
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    // ── apply_to_whitelisted ──────────────────────────────────────────────────

    #[test]
    fn whitelisted_top_level_fields_are_transformed() {
        let mut obj = json!({
            "cwd": "/home/user/project",
            "path": "/home/user/file.txt",
            "file_path": "/home/user/other.rs",
        });
        apply_to_whitelisted(obj.as_object_mut().unwrap(), |s| {
            Some(s.replace("/home/user", "TOKEN"))
        });
        assert_eq!(obj["cwd"], "TOKEN/project");
        assert_eq!(obj["path"], "TOKEN/file.txt");
        assert_eq!(obj["file_path"], "TOKEN/other.rs");
    }

    #[test]
    fn whitelisted_nested_fields_are_transformed() {
        let mut obj = json!({
            "message": { "cwd": "/home/user/project" },
            "arguments": {
                "path": "/home/user/file.txt",
                "file_path": "/home/user/other.rs",
                "command": "/home/user/bin/tool",
            },
        });
        apply_to_whitelisted(obj.as_object_mut().unwrap(), |s| {
            Some(s.replace("/home/user", "TOKEN"))
        });
        assert_eq!(obj["message"]["cwd"], "TOKEN/project");
        assert_eq!(obj["arguments"]["path"], "TOKEN/file.txt");
        assert_eq!(obj["arguments"]["file_path"], "TOKEN/other.rs");
        assert_eq!(obj["arguments"]["command"], "TOKEN/bin/tool");
    }

    #[test]
    fn non_whitelisted_fields_are_not_transformed() {
        let mut obj = json!({
            "id": "abc-123",
            "type": "message",
            "content": "/home/user/secret",
            "cwd": "/home/user/project",
        });
        apply_to_whitelisted(obj.as_object_mut().unwrap(), |s| {
            Some(s.replace("/home/user", "TOKEN"))
        });
        // `content` is not whitelisted — must stay unchanged
        assert_eq!(obj["content"], "/home/user/secret");
        // `cwd` is whitelisted — must be transformed
        assert_eq!(obj["cwd"], "TOKEN/project");
    }

    #[test]
    fn non_string_whitelisted_values_are_skipped() {
        let mut obj = json!({ "cwd": 42, "path": null });
        // Should not panic or alter non-string values
        apply_to_whitelisted(obj.as_object_mut().unwrap(), |_| Some("X".to_owned()));
        assert_eq!(obj["cwd"], 42);
        assert_eq!(obj["path"], serde_json::Value::Null);
    }

    #[test]
    fn absent_whitelisted_keys_are_skipped() {
        let mut obj = json!({ "type": "session" });
        // No whitelisted keys present — must not panic
        apply_to_whitelisted(obj.as_object_mut().unwrap(), |_| Some("X".to_owned()));
        assert_eq!(obj["type"], "session");
    }

    // ── apply_to_all_strings ──────────────────────────────────────────────────

    #[test]
    fn all_strings_in_object_are_transformed() {
        let mut v = json!({
            "content": "/home/user/secret",
            "cwd": "/home/user/project",
            "id": "abc",
        });
        apply_to_all_strings(&mut v, &mut |s| Some(s.replace("/home/user", "TOKEN")));
        assert_eq!(v["content"], "TOKEN/secret");
        assert_eq!(v["cwd"], "TOKEN/project");
        assert_eq!(v["id"], "abc"); // no home prefix — replace does nothing but returns Some
    }

    #[test]
    fn all_strings_in_nested_array_are_transformed() {
        let mut v = json!({
            "items": ["/home/user/a", "/home/user/b", 42],
        });
        apply_to_all_strings(&mut v, &mut |s| {
            let r = s.replace("/home/user", "TOKEN");
            if r == s {
                None
            } else {
                Some(r)
            }
        });
        assert_eq!(v["items"][0], "TOKEN/a");
        assert_eq!(v["items"][1], "TOKEN/b");
        assert_eq!(v["items"][2], 42);
    }

    #[test]
    fn none_from_transform_leaves_value_unchanged() {
        let mut v = json!({ "cwd": "/home/user/project" });
        apply_to_all_strings(&mut v, &mut |_| None);
        assert_eq!(v["cwd"], "/home/user/project");
    }
}
