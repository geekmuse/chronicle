//! Structured fuzzing for grow-only JSONL merge invariants.

#![no_main]

use std::collections::{HashMap, HashSet};
use std::path::Path;

use arbitrary::Arbitrary;
use chronicle::merge::entry::{parse_entry, EntryKey};
use chronicle::merge::set_union::{merge_jsonl, NullReporter};
use libfuzzer_sys::fuzz_target;
use serde_json::json;

#[derive(Arbitrary, Debug)]
struct EntryInput {
    kind: u8,
    id: u16,
    timestamp: i32,
    payload: String,
}

#[derive(Arbitrary, Debug)]
struct MergeInput {
    remote: Vec<EntryInput>,
    local: Vec<EntryInput>,
    remote_noise: Vec<String>,
    local_noise: Vec<String>,
    /// Selects Pi/Claude canonical repository roots for merge diagnostics.
    adapter_paths: u8,
}

fn safe_payload(raw: &str) -> String {
    raw.chars().filter(|c| !c.is_control()).take(64).collect()
}

fn entry_line(entry: &EntryInput) -> String {
    if entry.kind.is_multiple_of(11) {
        serde_json::to_string(&json!({
            "type": "session",
            "id": format!("session-{}", entry.id),
            "timestamp": format!("{:011}", entry.timestamp),
            "payload": safe_payload(&entry.payload),
        }))
        .expect("generated session entry must serialize")
    } else {
        let entry_type = match entry.kind % 4 {
            0 => "message",
            1 => "model_change",
            2 => "tool_result",
            _ => "event",
        };
        serde_json::to_string(&json!({
            "type": entry_type,
            "id": format!("{}-{}", entry.kind % 7, entry.id),
            "timestamp": format!("{:011}", entry.timestamp),
            "payload": safe_payload(&entry.payload),
        }))
        .expect("generated entry must serialize")
    }
}

fn build_file(entries: &[EntryInput], noise: &[String]) -> String {
    let mut lines: Vec<String> = entries.iter().take(32).map(entry_line).collect();
    lines.extend(
        noise
            .iter()
            .take(8)
            .map(|value| format!("not-json:{}", safe_payload(value))),
    );
    if lines.is_empty() {
        String::new()
    } else {
        format!("{}\n", lines.join("\n"))
    }
}

fn parsed_lines(content: &str) -> Vec<(EntryKey, String)> {
    content
        .lines()
        .filter_map(|line| parse_entry(line).map(|entry| (entry.key, entry.raw)))
        .collect()
}

fn key_set(content: &str) -> HashSet<EntryKey> {
    parsed_lines(content)
        .into_iter()
        .map(|(key, _)| key)
        .collect()
}

fuzz_target!(|input: MergeInput| {
    let remote = build_file(&input.remote, &input.remote_noise);
    let local = build_file(&input.local, &input.local_noise);
    let (remote_path, local_path) = if input.adapter_paths.is_multiple_of(2) {
        (
            Path::new("pi/sessions/--SYNC_HOME-Dev-app--/remote.jsonl"),
            Path::new("claude/projects/-SYNC_HOME-Dev-app/local.jsonl"),
        )
    } else {
        (
            Path::new("claude/projects/-SYNC_HOME-Dev-app/remote.jsonl"),
            Path::new("pi/sessions/--SYNC_HOME-Dev-app--/local.jsonl"),
        )
    };

    let output = merge_jsonl(&remote, remote_path, &local, local_path, &NullReporter);

    let remote_keys = key_set(&remote);
    let local_keys = key_set(&local);
    let expected_keys: HashSet<EntryKey> = remote_keys.union(&local_keys).cloned().collect();
    let output_lines = parsed_lines(&output.content);
    let output_keys: HashSet<EntryKey> = output_lines.iter().map(|(key, _)| key.clone()).collect();

    assert_eq!(
        output_keys, expected_keys,
        "merge did not produce the key union"
    );
    assert_eq!(
        output_lines.len(),
        output_keys.len(),
        "merge emitted duplicate entry keys"
    );
    assert!(
        output
            .content
            .lines()
            .all(|line| parse_entry(line).is_some()),
        "merge emitted malformed JSONL"
    );
    assert!(
        output_lines
            .iter()
            .filter(|(key, _)| *key == EntryKey::Header)
            .count()
            <= 1,
        "merge emitted more than one session header"
    );

    let expected_malformed =
        input.remote_noise.iter().take(8).count() + input.local_noise.iter().take(8).count();
    assert_eq!(
        output.malformed.len(),
        expected_malformed,
        "malformed-line accounting changed"
    );

    // The last remote occurrence wins both remote duplicates and local collisions.
    let remote_winners: HashMap<EntryKey, String> = parsed_lines(&remote).into_iter().collect();
    let output_by_key: HashMap<EntryKey, String> = output_lines.iter().cloned().collect();
    for (key, raw) in remote_winners {
        assert_eq!(
            output_by_key.get(&key),
            Some(&raw),
            "remote-wins conflict policy changed"
        );
    }

    let idempotent = merge_jsonl(
        &output.content,
        remote_path,
        &output.content,
        local_path,
        &NullReporter,
    );
    assert_eq!(
        idempotent.content, output.content,
        "merging output with itself was not idempotent"
    );

    let reversed = merge_jsonl(&local, local_path, &remote, remote_path, &NullReporter);
    assert_eq!(
        key_set(&reversed.content),
        expected_keys,
        "merge key union depends on argument order"
    );
});
