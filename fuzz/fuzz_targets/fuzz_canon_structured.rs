//! Structured semantic fuzzing for L2/L3 canonicalization.

#![no_main]

use std::collections::HashMap;
use std::path::Path;

use arbitrary::Arbitrary;
use chronicle::canon::TokenRegistry;
use chronicle::config::schema::CanonicalizationConfig;
use libfuzzer_sys::fuzz_target;
use serde_json::{json, Value};

#[derive(Arbitrary, Debug)]
struct CanonInput {
    level_three: bool,
    linux_home: bool,
    custom_token: bool,
    alternate_home_token: bool,
    field_selector: u8,
    username: String,
    component_one: String,
    component_two: String,
    prefix: String,
    suffix: String,
}

fn safe_component(raw: &str, fallback: &str) -> String {
    let value: String = raw
        .chars()
        .filter(|c| c.is_alphanumeric() || matches!(c, ' ' | '.' | '_' | '-'))
        .take(32)
        .collect();
    if value.is_empty() || value == "." || value == ".." {
        fallback.to_owned()
    } else {
        value
    }
}

fn safe_text(raw: &str) -> String {
    raw.chars()
        .filter(|c| !matches!(c, '{' | '}' | '/' | '\\' | '\0'))
        .take(48)
        .collect()
}

fn pointer_for(selector: u8) -> &'static str {
    match selector % 7 {
        0 => "/cwd",
        1 => "/path",
        2 => "/file_path",
        3 => "/message/cwd",
        4 => "/arguments/path",
        5 => "/arguments/file_path",
        _ => "/arguments/command",
    }
}

fn insert_selected_path(value: &mut Value, selector: u8, path: &str) {
    match selector % 7 {
        0 => value["cwd"] = json!(path),
        1 => value["path"] = json!(path),
        2 => value["file_path"] = json!(path),
        3 => value["message"]["cwd"] = json!(path),
        4 => value["arguments"]["path"] = json!(path),
        5 => value["arguments"]["file_path"] = json!(path),
        _ => value["arguments"]["command"] = json!(path),
    }
}

fn config(home_token: &str, custom_value: Option<String>, level: u8) -> CanonicalizationConfig {
    let tokens = custom_value
        .map(|value| HashMap::from([(String::from("{{SYNC_PROJECTS}}"), value)]))
        .unwrap_or_default();
    CanonicalizationConfig {
        home_token: home_token.to_owned(),
        level,
        tokens,
    }
}

fn replace_path_boundaries(text: &str, from: &str, to: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut remaining = text;
    while let Some(position) = remaining.find(from) {
        let after = &remaining[position + from.len()..];
        result.push_str(&remaining[..position]);
        if after.is_empty() || after.starts_with('/') {
            result.push_str(to);
        } else {
            result.push_str(from);
        }
        remaining = after;
    }
    result.push_str(remaining);
    result
}

fn expected_l3(text: &str, home: &str, home_token: &str, custom_value: Option<&str>) -> String {
    let mut result = replace_path_boundaries(text, home, home_token);
    if let Some(value) = custom_value {
        let canonical_value = replace_path_boundaries(value, home, home_token);
        result = replace_path_boundaries(&result, &canonical_value, "{{SYNC_PROJECTS}}");
    }
    result
}

fn expected_decanonicalized(
    text: &str,
    home: &str,
    home_token: &str,
    custom_value: Option<&str>,
) -> String {
    let result = custom_value.map_or_else(
        || text.to_owned(),
        |value| text.replace("{{SYNC_PROJECTS}}", value),
    );
    result.replace(home_token, home)
}

fuzz_target!(|input: CanonInput| {
    let username = safe_component(&input.username, "sender");
    let first = safe_component(&input.component_one, "project");
    let second = safe_component(&input.component_two, "src");
    let prefix = safe_text(&input.prefix);
    let suffix = safe_text(&input.suffix);

    let home_a = if input.linux_home {
        format!("/home/{username}")
    } else {
        format!("/Users/{username}")
    };
    let home_b = if input.linux_home {
        String::from("/home/receiver")
    } else {
        String::from("/Users/receiver")
    };
    if home_a == home_b {
        return;
    }

    let home_token = if input.alternate_home_token {
        "{{SYNC_HOME_ALT}}"
    } else {
        "{{SYNC_HOME}}"
    };
    let level = if input.level_three { 3 } else { 2 };
    let custom_a = input.custom_token.then(|| format!("{home_a}/Projects"));
    let custom_b = input.custom_token.then(|| format!("{home_b}/Projects"));
    let subpath = format!("{first}/{second}");
    let path_a = custom_a.as_ref().map_or_else(
        || format!("{home_a}/{subpath}"),
        |base| format!("{base}/{subpath}"),
    );
    let registry_a = TokenRegistry::from_config(
        &config(home_token, custom_a.clone(), level),
        Path::new(&home_a),
    );
    let registry_b = TokenRegistry::from_config(
        &config(home_token, custom_b.clone(), level),
        Path::new(&home_b),
    );

    let content = format!("{prefix} {path_a} {suffix}");
    let mut value = json!({
        "type": "message",
        "message": {},
        "arguments": {},
        "content": content,
        "unlisted_path": path_a,
        "boundary": format!("{home_a}suffix"),
        "nested": {"items": [format!("{prefix}{path_a}"), {"path": path_a}]}
    });
    insert_selected_path(&mut value, input.field_selector, &path_a);
    let line = serde_json::to_string(&value).expect("Value serialization must succeed");

    let canonical = registry_a
        .canonicalize_line(&line, level)
        .expect("generated JSON must canonicalize");
    let canonical_value: Value =
        serde_json::from_str(&canonical).expect("canonical output must be JSON");
    let selected = canonical_value
        .pointer(pointer_for(input.field_selector))
        .and_then(Value::as_str)
        .expect("selected L2 field must remain a string");
    let expected_selected = if level == 3 {
        expected_l3(&path_a, &home_a, home_token, custom_a.as_deref())
    } else if input.custom_token {
        format!("{{{{SYNC_PROJECTS}}}}/{subpath}")
    } else {
        format!("{home_token}/{subpath}")
    };
    assert_eq!(
        selected, expected_selected,
        "eligible path was not canonicalized"
    );
    assert_eq!(
        canonical_value["boundary"],
        Value::String(format!("{home_a}suffix")),
        "non-boundary home prefix was modified"
    );

    let unlisted = canonical_value["unlisted_path"]
        .as_str()
        .expect("unlisted_path must be a string");
    if level == 2 {
        assert_eq!(unlisted, path_a, "L2 modified a non-whitelisted field");
    } else {
        assert_eq!(
            unlisted,
            expected_l3(&path_a, &home_a, home_token, custom_a.as_deref()),
            "L3 produced an unexpected unlisted path"
        );
        assert_eq!(
            canonical_value["content"].as_str(),
            Some(expected_l3(&content, &home_a, home_token, custom_a.as_deref()).as_str()),
            "L3 produced unexpected freeform content"
        );
    }

    let canonical_twice = registry_a
        .canonicalize_line(&canonical, level)
        .expect("canonical JSON must canonicalize again");
    assert_eq!(
        canonical_twice, canonical,
        "canonicalization is not idempotent"
    );

    let restored = registry_a
        .decanonicalize_line(&canonical)
        .expect("canonical JSON must de-canonicalize");
    assert_eq!(restored, line, "same-machine round-trip failed");

    let cross_machine = registry_b
        .decanonicalize_line(&canonical)
        .expect("canonical JSON must de-canonicalize on another machine");
    let cross_value: Value =
        serde_json::from_str(&cross_machine).expect("cross-machine output must be JSON");
    let expected_cross_path =
        expected_decanonicalized(&expected_selected, &home_b, home_token, custom_b.as_deref());
    assert_eq!(
        cross_value
            .pointer(pointer_for(input.field_selector))
            .and_then(Value::as_str),
        Some(expected_cross_path.as_str()),
        "cross-machine path substitution failed"
    );
    if level == 3 {
        let expected_canonical_content =
            expected_l3(&content, &home_a, home_token, custom_a.as_deref());
        let expected_cross_content = expected_decanonicalized(
            &expected_canonical_content,
            &home_b,
            home_token,
            custom_b.as_deref(),
        );
        assert_eq!(
            cross_value["content"].as_str(),
            Some(expected_cross_content.as_str()),
            "cross-machine L3 substitution failed"
        );
    }
});
