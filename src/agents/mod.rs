// Items in this module are used by the canonicalization and CLI layers
// (US-004, US-013, US-014). Allow dead-code on the structs/trait until those
// stories are wired in.
#![allow(dead_code)]

//! Agent-specific directory encoding and decoding.
//!
//! Pi and Claude Code store their session files under directories whose names
//! encode the project path on the originating machine:
//!
//! | Agent  | Example local directory name            | Decoded path                  |
//! |--------|-----------------------------------------|-------------------------------|
//! | Pi     | `--Users-bradmatic-Dev-foo--`           | `/Users/bradmatic/Dev/foo`    |
//! | Claude | `-Users-bradmatic-Dev-foo`              | `/Users/bradmatic/Dev/foo`    |
//!
//! These formats are set by Pi and Claude themselves; Chronicle reads and
//! writes them to map session files between machines.

use std::path::{Path, PathBuf};

use crate::errors::ChronicleError;

// ── Agent trait ───────────────────────────────────────────────────────────────

/// Behaviour required of every supported agent module.
pub trait Agent {
    /// Returns the root directory where this agent stores session
    /// subdirectories, given the current user's home directory.
    ///
    /// * Pi    → `<home>/.pi/agent/sessions`
    /// * Claude → `<home>/.claude/projects`
    fn session_dir(&self, home: &Path) -> PathBuf;

    /// Encodes an absolute filesystem path to the agent's directory-name
    /// format.
    ///
    /// * Pi    → `--<components-joined-by-dashes>--`
    /// * Claude → `-<components-joined-by-dashes>` (dots also replaced)
    fn encode_dir(&self, path: &Path) -> String;

    /// Decodes an agent directory name back to an absolute filesystem path.
    ///
    /// # Errors
    ///
    /// Returns [`ChronicleError::CanonicalizationError`] if `name` does not
    /// have the expected wrapper/prefix for this agent.
    fn decode_dir(&self, name: &str) -> Result<PathBuf, ChronicleError>;
}

// ── Pi agent ──────────────────────────────────────────────────────────────────

/// Pi agent: session files live under `~/.pi/agent/sessions/<encoded-dir>/`.
///
/// **Encoding rule:**
/// 1. Strip the leading `/` from the absolute path.
/// 2. Replace every remaining `/` with `-`.
/// 3. Wrap the result with `--` on both sides.
///
/// `/Users/bradmatic/Dev/foo` → `--Users-bradmatic-Dev-foo--`
pub struct PiAgent;

impl Agent for PiAgent {
    fn session_dir(&self, home: &Path) -> PathBuf {
        home.join(".pi").join("agent").join("sessions")
    }

    fn encode_dir(&self, path: &Path) -> String {
        let inner = path
            .to_string_lossy()
            .trim_start_matches('/')
            .replace('/', "-");
        format!("--{inner}--")
    }

    fn decode_dir(&self, name: &str) -> Result<PathBuf, ChronicleError> {
        let inner = name
            .strip_prefix("--")
            .and_then(|s| s.strip_suffix("--"))
            .ok_or_else(|| ChronicleError::CanonicalizationError {
                path: name.to_owned(),
                message: String::from(
                    "Pi directory name must be wrapped in '--' \
                     (e.g. '--Users-bradmatic-Dev-foo--')",
                ),
            })?;

        // Restore the leading `/` then convert every `-` back to a `/`.
        let path_str = format!("/{}", inner.replace('-', "/"));
        Ok(PathBuf::from(path_str))
    }
}

// ── Claude agent ──────────────────────────────────────────────────────────────

/// Claude agent: session files live under `~/.claude/projects/<encoded-dir>/`.
///
/// **Encoding rule:**
/// 1. Strip the leading `/` from the absolute path.
/// 2. Replace every remaining `/` **and** `.` with `-`.
/// 3. Prefix the result with a single `-`.
///
/// `/Users/bradmatic/Dev/foo` → `-Users-bradmatic-Dev-foo`
pub struct ClaudeAgent;

impl Agent for ClaudeAgent {
    fn session_dir(&self, home: &Path) -> PathBuf {
        home.join(".claude").join("projects")
    }

    fn encode_dir(&self, path: &Path) -> String {
        let inner = path
            .to_string_lossy()
            .trim_start_matches('/')
            .replace(['/', '.'], "-");
        format!("-{inner}")
    }

    fn decode_dir(&self, name: &str) -> Result<PathBuf, ChronicleError> {
        // Reject Pi-encoded names that start with `--`.
        if name.starts_with("--") {
            return Err(ChronicleError::CanonicalizationError {
                path: name.to_owned(),
                message: String::from(
                    "Claude directory name must start with a single '-', not '--' \
                     (did you pass a Pi-encoded name?)",
                ),
            });
        }

        let inner =
            name.strip_prefix('-')
                .ok_or_else(|| ChronicleError::CanonicalizationError {
                    path: name.to_owned(),
                    message: String::from(
                        "Claude directory name must start with '-' \
                     (e.g. '-Users-bradmatic-Dev-foo')",
                    ),
                })?;

        // Restore the leading `/` then convert every `-` back to a `/`.
        // Note: dots that were encoded as `-` cannot be recovered — the
        // decode is a best-effort inverse that is lossless only for path
        // components that contain no `-` or `.` characters themselves.
        let path_str = format!("/{}", inner.replace('-', "/"));
        Ok(PathBuf::from(path_str))
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Pi: basic encode / decode ─────────────────────────────────────────────

    #[test]
    fn pi_encode_simple() {
        let agent = PiAgent;
        assert_eq!(
            agent.encode_dir(Path::new("/Users/bradmatic/Dev/foo")),
            "--Users-bradmatic-Dev-foo--"
        );
    }

    #[test]
    fn pi_decode_simple() {
        let agent = PiAgent;
        assert_eq!(
            agent.decode_dir("--Users-bradmatic-Dev-foo--").unwrap(),
            PathBuf::from("/Users/bradmatic/Dev/foo")
        );
    }

    #[test]
    fn pi_session_dir() {
        let agent = PiAgent;
        assert_eq!(
            agent.session_dir(Path::new("/Users/bradmatic")),
            PathBuf::from("/Users/bradmatic/.pi/agent/sessions")
        );
    }

    // ── Pi: round-trip (path components must not contain `-`) ─────────────────

    #[test]
    fn pi_round_trip() {
        let agent = PiAgent;
        let paths = [
            "/Users/bradmatic/Dev/foo",
            "/home/brad/projects/myapp",
            "/Users/alice/a/b/c/d/e/deeply/nested",
            "/tmp/work",
        ];
        for path_str in paths {
            let encoded = agent.encode_dir(Path::new(path_str));
            let decoded = agent.decode_dir(&encoded).unwrap();
            assert_eq!(
                decoded,
                PathBuf::from(path_str),
                "Pi round-trip failed for {path_str}"
            );
        }
    }

    // ── Pi: error cases ───────────────────────────────────────────────────────

    #[test]
    fn pi_decode_missing_wrapper() {
        let agent = PiAgent;
        // Missing both wrappers.
        assert!(agent.decode_dir("Users-bradmatic-Dev-foo").is_err());
        // Missing trailing `--`.
        assert!(agent.decode_dir("--Users-bradmatic-Dev-foo").is_err());
        // Single-dash prefix (Claude format).
        assert!(agent.decode_dir("-Users-bradmatic-Dev-foo").is_err());
    }

    // ── Claude: basic encode / decode ─────────────────────────────────────────

    #[test]
    fn claude_encode_simple() {
        let agent = ClaudeAgent;
        assert_eq!(
            agent.encode_dir(Path::new("/Users/bradmatic/Dev/foo")),
            "-Users-bradmatic-Dev-foo"
        );
    }

    #[test]
    fn claude_decode_simple() {
        let agent = ClaudeAgent;
        assert_eq!(
            agent.decode_dir("-Users-bradmatic-Dev-foo").unwrap(),
            PathBuf::from("/Users/bradmatic/Dev/foo")
        );
    }

    #[test]
    fn claude_session_dir() {
        let agent = ClaudeAgent;
        assert_eq!(
            agent.session_dir(Path::new("/Users/bradmatic")),
            PathBuf::from("/Users/bradmatic/.claude/projects")
        );
    }

    // ── Claude: dots in path components ──────────────────────────────────────

    #[test]
    fn claude_encode_dots() {
        // Dots in path components (e.g. `~/.config/foo`) are replaced with `-`.
        let agent = ClaudeAgent;
        // `/Users/bradmatic/.config/foo`:
        //   trim `/` → `Users/bradmatic/.config/foo`
        //   replace `/` and `.` → `Users-bradmatic--config-foo`
        //   prefix `-` → `-Users-bradmatic--config-foo`
        assert_eq!(
            agent.encode_dir(Path::new("/Users/bradmatic/.config/foo")),
            "-Users-bradmatic--config-foo"
        );
    }

    // ── Claude: round-trip (path components must not contain `-` or `.`) ──────

    #[test]
    fn claude_round_trip() {
        let agent = ClaudeAgent;
        let paths = [
            "/Users/bradmatic/Dev/foo",
            "/home/brad/projects/myapp",
            "/Users/alice/a/b/c/d/e/deeply/nested",
            "/tmp/work",
        ];
        for path_str in paths {
            let encoded = agent.encode_dir(Path::new(path_str));
            let decoded = agent.decode_dir(&encoded).unwrap();
            assert_eq!(
                decoded,
                PathBuf::from(path_str),
                "Claude round-trip failed for {path_str}"
            );
        }
    }

    // ── Claude: error cases ───────────────────────────────────────────────────

    #[test]
    fn claude_decode_no_leading_dash() {
        let agent = ClaudeAgent;
        assert!(agent.decode_dir("Users-bradmatic-Dev-foo").is_err());
    }

    #[test]
    fn claude_decode_rejects_pi_format() {
        // A Pi-encoded name (starts with `--`) should be rejected by Claude.
        let agent = ClaudeAgent;
        assert!(agent.decode_dir("--Users-bradmatic-Dev-foo--").is_err());
    }

    // ── Cross-agent: same path encodes differently ────────────────────────────

    #[test]
    fn pi_and_claude_produce_different_encodings() {
        let path = Path::new("/Users/bradmatic/Dev/foo");
        let pi = PiAgent;
        let claude = ClaudeAgent;
        assert_ne!(pi.encode_dir(path), claude.encode_dir(path));
    }
}
