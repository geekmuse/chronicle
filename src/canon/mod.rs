//! Token registry and L1 path canonicalization.
//!
//! # Canonicalization levels
//!
//! | Level | What is canonicalized | Implemented in |
//! |-------|-----------------------|----------------|
//! | L1    | Directory name paths  | US-004 (here)  |
//! | L2    | Whitelisted JSON fields | US-005       |
//! | L3    | All freeform text (opt-in) | US-005     |
//!
//! # Token ordering
//!
//! **Canonicalization (outgoing):** `{{SYNC_HOME}}` first, then custom tokens
//! in descending value-path-length order (most-specific first).
//!
//! **De-canonicalization (incoming):** custom tokens first (same specificity
//! order), then `{{SYNC_HOME}}`.  This is the exact reverse, ensuring correct
//! nesting when a custom token's path is under `$HOME`.
//!
//! # Path-boundary matching
//!
//! Matching is performed at encoded-path-component boundaries: the character
//! immediately after the matched prefix must be `-` (encoded separator) or the
//! end of the string.  This prevents `/Users/bradmatic2` from matching when the
//! home directory is `/Users/bradmatic`.

pub mod fields;
pub mod levels;

use std::path::{Path, PathBuf};

use crate::config::schema::CanonicalizationConfig;

// ── Token registry ────────────────────────────────────────────────────────────

/// Manages the `{{SYNC_HOME}}` token and any custom tokens from config.
///
/// After construction the registry is immutable; one instance is built per
/// sync cycle from the loaded [`CanonicalizationConfig`].
#[derive(Debug, Clone)]
pub struct TokenRegistry {
    /// The local machine's home directory (used to compute encoded prefixes).
    home: PathBuf,
    /// The home-directory token string (default: `"{{SYNC_HOME}}"`).
    home_token: String,
    /// Custom tokens sorted by **descending** raw-value path-string byte length
    /// so more-specific (longer) paths are matched first.
    custom_tokens: Vec<(String, PathBuf)>,
}

impl TokenRegistry {
    /// Build a registry from a [`CanonicalizationConfig`] and the local home directory.
    ///
    /// Custom tokens are sorted by descending value-path length so that
    /// longer (more-specific) paths win over shorter ones.
    #[must_use]
    pub fn from_config(config: &CanonicalizationConfig, home: &Path) -> Self {
        let mut custom: Vec<(String, PathBuf)> = config
            .tokens
            .iter()
            .map(|(k, v)| (k.clone(), PathBuf::from(v)))
            .collect();

        // Most-specific (longest absolute path) first.
        custom.sort_by(|(_, a), (_, b)| b.as_os_str().len().cmp(&a.as_os_str().len()));

        Self {
            home: home.to_owned(),
            home_token: config.home_token.clone(),
            custom_tokens: custom,
        }
    }

    /// Returns the local home directory this registry was built for.
    #[must_use]
    pub fn home(&self) -> &Path {
        &self.home
    }

    /// Returns the home-directory token string (e.g., `"{{SYNC_HOME}}"`).
    #[must_use]
    pub fn home_token(&self) -> &str {
        &self.home_token
    }
}

// ── Agent-specific path encoders ──────────────────────────────────────────────

/// Compute the Pi *inner* encoding of an absolute path.
///
/// Strips the leading `/` then replaces every remaining `/` with `-`.
///
/// `/Users/bradmatic/Dev/foo` → `Users-bradmatic-Dev-foo`
fn pi_encode_inner(path: &Path) -> String {
    path.to_string_lossy()
        .trim_start_matches('/')
        .replace('/', "-")
}

/// Compute the Claude *inner* encoding of an absolute path.
///
/// Strips the leading `/` then replaces every remaining `/` **and** `.`
/// with `-`.
///
/// `/Users/bradmatic/Dev/foo` → `Users-bradmatic-Dev-foo`
fn claude_encode_inner(path: &Path) -> String {
    path.to_string_lossy()
        .trim_start_matches('/')
        .replace(['/', '.'], "-")
}

// ── Boundary-aware prefix helpers ─────────────────────────────────────────────

/// Returns `true` when `s` starts with `prefix` **at a path boundary**.
///
/// A path boundary means the character immediately after `prefix` in `s` is
/// either `-` (the encoded path-separator) or the string ends exactly there.
/// An empty prefix never matches (avoids replacing everything).
fn boundary_match(s: &str, prefix: &str) -> bool {
    if prefix.is_empty() {
        return false;
    }
    if s == prefix {
        return true;
    }
    s.starts_with(prefix) && s[prefix.len()..].starts_with('-')
}

/// If `s` starts with `prefix` at a path boundary, return the suffix after the
/// prefix (e.g., `"-Dev-foo"` or `""`).  Otherwise return `None`.
fn strip_boundary_prefix<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    if boundary_match(s, prefix) {
        Some(&s[prefix.len()..])
    } else {
        None
    }
}

// ── L1 canonicalization (public interface) ────────────────────────────────────

impl TokenRegistry {
    /// Canonicalize a Pi-format directory name.
    ///
    /// ```text
    /// --Users-bradmatic-Dev-foo--  →  --{{SYNC_HOME}}-Dev-foo--
    /// ```
    ///
    /// If the inner encoded string does not begin with the encoded home prefix
    /// at a path boundary the name is returned unchanged.
    #[must_use]
    pub fn canonicalize_pi_dir(&self, name: &str) -> String {
        match name.strip_prefix("--").and_then(|s| s.strip_suffix("--")) {
            None => name.to_owned(),
            Some(inner) => {
                let new_inner = self.apply_tokens_to_inner(inner, false);
                format!("--{new_inner}--")
            }
        }
    }

    /// Canonicalize a Claude-format directory name.
    ///
    /// ```text
    /// -Users-bradmatic-Dev-foo  →  -{{SYNC_HOME}}-Dev-foo
    /// ```
    ///
    /// Pi-encoded names (starting with `--`) are returned unchanged.
    #[must_use]
    pub fn canonicalize_claude_dir(&self, name: &str) -> String {
        if name.starts_with("--") {
            return name.to_owned();
        }
        match name.strip_prefix('-') {
            None => name.to_owned(),
            Some(inner) => {
                let new_inner = self.apply_tokens_to_inner(inner, true);
                format!("-{new_inner}")
            }
        }
    }

    /// De-canonicalize a Pi-format directory name.
    ///
    /// ```text
    /// --{{SYNC_HOME}}-Dev-foo--  →  --Users-bradmatic-Dev-foo--
    /// ```
    #[must_use]
    pub fn decanonicalize_pi_dir(&self, name: &str) -> String {
        match name.strip_prefix("--").and_then(|s| s.strip_suffix("--")) {
            None => name.to_owned(),
            Some(inner) => {
                let new_inner = self.revert_tokens_from_inner(inner, false);
                format!("--{new_inner}--")
            }
        }
    }

    /// De-canonicalize a Claude-format directory name.
    ///
    /// ```text
    /// -{{SYNC_HOME}}-Dev-foo  →  -Users-bradmatic-Dev-foo
    /// ```
    ///
    /// Pi-encoded names (starting with `--`) are returned unchanged.
    #[must_use]
    pub fn decanonicalize_claude_dir(&self, name: &str) -> String {
        if name.starts_with("--") {
            return name.to_owned();
        }
        match name.strip_prefix('-') {
            None => name.to_owned(),
            Some(inner) => {
                let new_inner = self.revert_tokens_from_inner(inner, true);
                format!("-{new_inner}")
            }
        }
    }
}

// ── Internal canonicalization helpers ────────────────────────────────────────

impl TokenRegistry {
    /// Apply SYNC_HOME then custom tokens to an already-stripped inner string.
    ///
    /// `claude_encoding` selects whether to use Claude's encoding rules
    /// (replaces `.` as well as `/`) or Pi's rules (replaces `/` only).
    fn apply_tokens_to_inner(&self, inner: &str, claude_encoding: bool) -> String {
        let encode: fn(&Path) -> String = if claude_encoding {
            claude_encode_inner
        } else {
            pi_encode_inner
        };

        let encoded_home = encode(&self.home);

        // ── Step 1: SYNC_HOME ─────────────────────────────────────────────────
        let after_home = match strip_boundary_prefix(inner, &encoded_home) {
            Some(rest) => format!("{}{rest}", self.home_token),
            None => inner.to_owned(),
        };

        // ── Step 2: custom tokens (most-specific first) ───────────────────────
        // Each custom token is compared in its *canonical encoded* form:
        // encode the raw value, then apply SYNC_HOME to that encoded string.
        // This allows tokens like {{SYNC_PROJECTS}} = /Users/bradmatic/Dev to
        // match the already-SYNC_HOME-canonicalized inner string.
        let mut result = after_home;
        for (token_name, token_value) in &self.custom_tokens {
            let encoded_val = encode(token_value);
            let canonical_encoded_val = match strip_boundary_prefix(&encoded_val, &encoded_home) {
                Some(rest) => format!("{}{rest}", self.home_token),
                None => encoded_val,
            };
            if let Some(rest) = strip_boundary_prefix(&result, &canonical_encoded_val) {
                result = format!("{token_name}{rest}");
                // Only the first matching (most-specific) prefix token applies.
                break;
            }
        }

        result
    }

    /// Revert custom tokens then SYNC_HOME from an already-stripped inner string.
    fn revert_tokens_from_inner(&self, inner: &str, claude_encoding: bool) -> String {
        let encode: fn(&Path) -> String = if claude_encoding {
            claude_encode_inner
        } else {
            pi_encode_inner
        };

        let encoded_home = encode(&self.home);

        // ── Step 1: revert custom tokens (most-specific first) ────────────────
        let mut result = inner.to_owned();
        for (token_name, token_value) in &self.custom_tokens {
            let encoded_val = encode(token_value);
            let canonical_encoded_val = match strip_boundary_prefix(&encoded_val, &encoded_home) {
                Some(rest) => format!("{}{rest}", self.home_token),
                None => encoded_val,
            };
            if let Some(rest) = strip_boundary_prefix(&result, token_name) {
                result = format!("{canonical_encoded_val}{rest}");
                break;
            }
        }

        // ── Step 2: revert SYNC_HOME ──────────────────────────────────────────
        if let Some(rest) = strip_boundary_prefix(&result, &self.home_token) {
            result = format!("{encoded_home}{rest}");
        }

        result
    }
}

// ── Content string helpers (L2 / L3) ─────────────────────────────────────────

/// Replace all path-boundary occurrences of `from` with `to` in `text`.
///
/// A path boundary after `from` means the next character is either:
/// - the end of the string, or
/// - `/` (the path continues as a subdirectory).
///
/// This prevents partial matches: `/Users/bradmatic` does **not** match inside
/// `/Users/bradmatic2/foo` because `2` follows the prefix without a `/`.
fn replace_in_text(text: &str, from: &str, to: &str) -> String {
    if from.is_empty() || !text.contains(from) {
        return text.to_owned();
    }
    let mut result = String::with_capacity(text.len() + 32);
    let mut remaining = text;
    while let Some(pos) = remaining.find(from) {
        let after = &remaining[pos + from.len()..];
        let at_boundary = after.is_empty() || after.starts_with('/');
        if at_boundary {
            result.push_str(&remaining[..pos]);
            result.push_str(to);
            remaining = after;
        } else {
            // Not a boundary — copy through and continue searching past the match.
            result.push_str(&remaining[..pos + from.len()]);
            remaining = after;
        }
    }
    result.push_str(remaining);
    result
}

/// Compute the canonical storage form of a custom token value.
///
/// If `token_value` is under `home` (starts with `home/`), its canonical form
/// replaces the home prefix with the home token.  Otherwise the value is
/// returned as-is (non-home custom tokens are stored verbatim).
fn canonical_token_value(token_value: &Path, home: &str, home_token: &str) -> String {
    let tv = token_value.to_string_lossy();
    if tv == home {
        home_token.to_owned()
    } else if tv.starts_with(home) && tv[home.len()..].starts_with('/') {
        format!("{home_token}{}", &tv[home.len()..])
    } else {
        tv.into_owned()
    }
}

impl TokenRegistry {
    /// Try to canonicalize `s` as a **path value** (L2 semantics).
    ///
    /// The value is a candidate only if it equals `$HOME` or starts with
    /// `$HOME/`.  After `{{SYNC_HOME}}` substitution the most-specific custom
    /// token prefix is applied.
    ///
    /// Returns `Some(new_value)` if the string changed, `None` otherwise.
    pub(crate) fn try_canonicalize_path(&self, s: &str) -> Option<String> {
        let home = self.home.to_string_lossy();
        let rest = if s == home.as_ref() {
            ""
        } else if s.starts_with(home.as_ref()) && s[home.len()..].starts_with('/') {
            &s[home.len()..]
        } else {
            return None;
        };

        let after_home = format!("{}{rest}", self.home_token);

        // Apply custom tokens: prefix-only (most-specific first, break on first match).
        for (token_name, token_value) in &self.custom_tokens {
            let cv = canonical_token_value(token_value, &home, &self.home_token);
            if after_home == cv {
                return Some(token_name.clone());
            }
            if after_home.starts_with(&cv) && after_home[cv.len()..].starts_with('/') {
                return Some(format!("{token_name}{}", &after_home[cv.len()..]));
            }
        }

        Some(after_home)
    }

    /// Try to canonicalize `s` as **freeform text** (L3 semantics).
    ///
    /// Replaces all path-boundary occurrences of the home path with
    /// `{{SYNC_HOME}}`, then applies all custom tokens globally.
    ///
    /// Returns `Some(new_value)` if the string changed, `None` otherwise.
    pub(crate) fn try_canonicalize_text(&self, s: &str) -> Option<String> {
        let home = self.home.to_string_lossy();

        // Step 1: Replace all home-path occurrences.
        let step1 = replace_in_text(s, home.as_ref(), &self.home_token);

        // Step 2: Apply custom tokens globally (most-specific canonical form first).
        let mut result = step1;
        for (token_name, token_value) in &self.custom_tokens {
            let cv = canonical_token_value(token_value, &home, &self.home_token);
            result = replace_in_text(&result, &cv, token_name);
        }

        if result == s {
            None
        } else {
            Some(result)
        }
    }

    /// Try to de-canonicalize token occurrences in `s`.
    ///
    /// Reverses both L2 (path values) and L3 (freeform text) canonicalization
    /// by scanning all string occurrences:
    /// 1. Revert custom tokens (most-specific canonical form first).
    /// 2. Revert `{{SYNC_HOME}}` to the local home directory.
    ///
    /// Returns `Some(new_value)` if the string changed, `None` otherwise.
    pub(crate) fn try_decanonicalize_text(&self, s: &str) -> Option<String> {
        let home = self.home.to_string_lossy();

        // Step 1: Revert custom tokens (most-specific first, same order as canon).
        let mut result = s.to_owned();
        for (token_name, token_value) in &self.custom_tokens {
            let cv = canonical_token_value(token_value, &home, &self.home_token);
            result = replace_in_text(&result, token_name, &cv);
        }

        // Step 2: Revert {{SYNC_HOME}}.
        result = replace_in_text(&result, &self.home_token, &home);

        if result == s {
            None
        } else {
            Some(result)
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::config::schema::CanonicalizationConfig;

    // ── Helper ────────────────────────────────────────────────────────────────

    fn registry(home: &str) -> TokenRegistry {
        let config = CanonicalizationConfig {
            home_token: "{{SYNC_HOME}}".to_owned(),
            level: 2,
            tokens: HashMap::new(),
        };
        TokenRegistry::from_config(&config, Path::new(home))
    }

    fn registry_with_tokens(home: &str, tokens: &[(&str, &str)]) -> TokenRegistry {
        let config = CanonicalizationConfig {
            home_token: "{{SYNC_HOME}}".to_owned(),
            level: 2,
            tokens: tokens
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
        };
        TokenRegistry::from_config(&config, Path::new(home))
    }

    // ── TokenRegistry construction ────────────────────────────────────────────

    #[test]
    fn registry_home_and_token_accessors() {
        let reg = registry("/Users/bradmatic");
        assert_eq!(reg.home(), Path::new("/Users/bradmatic"));
        assert_eq!(reg.home_token(), "{{SYNC_HOME}}");
    }

    #[test]
    fn registry_custom_tokens_sorted_longest_first() {
        let reg = registry_with_tokens(
            "/Users/bradmatic",
            &[
                ("{{SHORT}}", "/Users/bradmatic/Dev"),
                ("{{LONG}}", "/Users/bradmatic/Dev/project"),
            ],
        );
        // Longest value must be first in the internal Vec.
        assert_eq!(reg.custom_tokens[0].0, "{{LONG}}");
        assert_eq!(reg.custom_tokens[1].0, "{{SHORT}}");
    }

    // ── Pi: basic canonicalization ────────────────────────────────────────────

    #[test]
    fn pi_canon_simple() {
        let reg = registry("/Users/bradmatic");
        assert_eq!(
            reg.canonicalize_pi_dir("--Users-bradmatic-Dev-foo--"),
            "--{{SYNC_HOME}}-Dev-foo--"
        );
    }

    #[test]
    fn pi_canon_home_itself() {
        // The directory IS the encoded home: inner == encoded_home.
        let reg = registry("/Users/bradmatic");
        assert_eq!(
            reg.canonicalize_pi_dir("--Users-bradmatic--"),
            "--{{SYNC_HOME}}--"
        );
    }

    #[test]
    fn pi_canon_no_match_returns_unchanged() {
        let reg = registry("/Users/bradmatic");
        // Does not start with the home prefix.
        assert_eq!(
            reg.canonicalize_pi_dir("--opt-homebrew-bin--"),
            "--opt-homebrew-bin--"
        );
    }

    #[test]
    fn pi_canon_not_pi_format_returns_unchanged() {
        let reg = registry("/Users/bradmatic");
        // Missing `--` wrappers.
        assert_eq!(
            reg.canonicalize_pi_dir("Users-bradmatic-Dev-foo"),
            "Users-bradmatic-Dev-foo"
        );
    }

    // ── Claude: basic canonicalization ────────────────────────────────────────

    #[test]
    fn claude_canon_simple() {
        let reg = registry("/Users/bradmatic");
        assert_eq!(
            reg.canonicalize_claude_dir("-Users-bradmatic-Dev-foo"),
            "-{{SYNC_HOME}}-Dev-foo"
        );
    }

    #[test]
    fn claude_canon_home_itself() {
        let reg = registry("/Users/bradmatic");
        assert_eq!(
            reg.canonicalize_claude_dir("-Users-bradmatic"),
            "-{{SYNC_HOME}}"
        );
    }

    #[test]
    fn claude_canon_rejects_pi_format() {
        let reg = registry("/Users/bradmatic");
        // Pi-encoded names must be returned unchanged (not garbled).
        assert_eq!(
            reg.canonicalize_claude_dir("--Users-bradmatic-Dev-foo--"),
            "--Users-bradmatic-Dev-foo--"
        );
    }

    // ── Pi: basic de-canonicalization ─────────────────────────────────────────

    #[test]
    fn pi_decanon_simple() {
        let reg = registry("/Users/bradmatic");
        assert_eq!(
            reg.decanonicalize_pi_dir("--{{SYNC_HOME}}-Dev-foo--"),
            "--Users-bradmatic-Dev-foo--"
        );
    }

    #[test]
    fn pi_decanon_home_itself() {
        let reg = registry("/Users/bradmatic");
        assert_eq!(
            reg.decanonicalize_pi_dir("--{{SYNC_HOME}}--"),
            "--Users-bradmatic--"
        );
    }

    // ── Claude: basic de-canonicalization ─────────────────────────────────────

    #[test]
    fn claude_decanon_simple() {
        let reg = registry("/Users/bradmatic");
        assert_eq!(
            reg.decanonicalize_claude_dir("-{{SYNC_HOME}}-Dev-foo"),
            "-Users-bradmatic-Dev-foo"
        );
    }

    // ── Cross-machine de-canonicalization ─────────────────────────────────────

    #[test]
    fn cross_machine_decanon() {
        // Machine B has a different home path.
        let reg_b = registry("/home/brad");
        assert_eq!(
            reg_b.decanonicalize_pi_dir("--{{SYNC_HOME}}-Dev-foo--"),
            "--home-brad-Dev-foo--"
        );
        assert_eq!(
            reg_b.decanonicalize_claude_dir("-{{SYNC_HOME}}-Dev-foo"),
            "-home-brad-Dev-foo"
        );
    }

    // ── Round-trip tests ──────────────────────────────────────────────────────

    #[test]
    fn pi_round_trip() {
        let reg = registry("/Users/bradmatic");
        let cases = [
            "--Users-bradmatic-Dev-foo--",
            "--Users-bradmatic--",
            "--Users-bradmatic-a-b-c-d--",
        ];
        for name in cases {
            let canonical = reg.canonicalize_pi_dir(name);
            let restored = reg.decanonicalize_pi_dir(&canonical);
            assert_eq!(restored, name, "Pi round-trip failed for {name}");
        }
    }

    #[test]
    fn claude_round_trip() {
        let reg = registry("/Users/bradmatic");
        let cases = [
            "-Users-bradmatic-Dev-foo",
            "-Users-bradmatic",
            "-Users-bradmatic-a-b-c",
        ];
        for name in cases {
            let canonical = reg.canonicalize_claude_dir(name);
            let restored = reg.decanonicalize_claude_dir(&canonical);
            assert_eq!(restored, name, "Claude round-trip failed for {name}");
        }
    }

    // ── Path-boundary matching ────────────────────────────────────────────────

    #[test]
    fn boundary_match_prevents_partial_home_match() {
        let reg = registry("/Users/bradmatic");

        // "/Users/bradmatic2/Dev/foo" encoded for Pi: "Users-bradmatic2-Dev-foo"
        // Must NOT be canonicalized because "bradmatic2" ≠ "bradmatic" boundary.
        assert_eq!(
            reg.canonicalize_pi_dir("--Users-bradmatic2-Dev-foo--"),
            "--Users-bradmatic2-Dev-foo--"
        );
        assert_eq!(
            reg.canonicalize_claude_dir("-Users-bradmatic2-Dev-foo"),
            "-Users-bradmatic2-Dev-foo"
        );
    }

    #[test]
    fn boundary_match_helper_direct() {
        assert!(boundary_match("Users-bradmatic-Dev", "Users-bradmatic"));
        assert!(boundary_match("Users-bradmatic", "Users-bradmatic"));
        assert!(!boundary_match("Users-bradmatic2", "Users-bradmatic"));
        assert!(!boundary_match("Users-bradmatic2-Dev", "Users-bradmatic"));
        assert!(!boundary_match("", "Users-bradmatic"));
        assert!(!boundary_match("Users-bradmatic-Dev", ""));
    }

    // ── Custom token ordering ─────────────────────────────────────────────────

    #[test]
    fn custom_token_applied_after_sync_home_during_canon() {
        // {{SYNC_PROJECTS}} = /Users/bradmatic/Dev
        let reg = registry_with_tokens(
            "/Users/bradmatic",
            &[("{{SYNC_PROJECTS}}", "/Users/bradmatic/Dev")],
        );

        // Pi dir encoding of /Users/bradmatic/Dev/foo = --Users-bradmatic-Dev-foo--
        // After SYNC_HOME:  --{{SYNC_HOME}}-Dev-foo--
        // Canonical encoded val for {{SYNC_PROJECTS}}: encode(/Users/bradmatic/Dev)
        //   = "Users-bradmatic-Dev" → apply SYNC_HOME → "{{SYNC_HOME}}-Dev"
        // Match in "{{SYNC_HOME}}-Dev-foo" → replace → "{{SYNC_PROJECTS}}-foo"
        // Result: --{{SYNC_PROJECTS}}-foo--
        assert_eq!(
            reg.canonicalize_pi_dir("--Users-bradmatic-Dev-foo--"),
            "--{{SYNC_PROJECTS}}-foo--"
        );
    }

    #[test]
    fn custom_token_reverted_before_sync_home_during_decanon() {
        let reg = registry_with_tokens(
            "/Users/bradmatic",
            &[("{{SYNC_PROJECTS}}", "/Users/bradmatic/Dev")],
        );

        // Canonical form: --{{SYNC_PROJECTS}}-foo--
        // Step 1 (custom): "{{SYNC_PROJECTS}}-foo" → "{{SYNC_HOME}}-Dev-foo"
        // Step 2 (SYNC_HOME): "{{SYNC_HOME}}-Dev-foo" → "Users-bradmatic-Dev-foo"
        // Wrap: --Users-bradmatic-Dev-foo--
        assert_eq!(
            reg.decanonicalize_pi_dir("--{{SYNC_PROJECTS}}-foo--"),
            "--Users-bradmatic-Dev-foo--"
        );
    }

    #[test]
    fn custom_token_round_trip_pi() {
        let reg = registry_with_tokens(
            "/Users/bradmatic",
            &[("{{SYNC_PROJECTS}}", "/Users/bradmatic/Dev")],
        );
        let original = "--Users-bradmatic-Dev-myproject--";
        let canonical = reg.canonicalize_pi_dir(original);
        assert_eq!(canonical, "--{{SYNC_PROJECTS}}-myproject--");
        let restored = reg.decanonicalize_pi_dir(&canonical);
        assert_eq!(restored, original);
    }

    #[test]
    fn custom_token_round_trip_claude() {
        let reg = registry_with_tokens(
            "/Users/bradmatic",
            &[("{{SYNC_PROJECTS}}", "/Users/bradmatic/Dev")],
        );
        let original = "-Users-bradmatic-Dev-myproject";
        let canonical = reg.canonicalize_claude_dir(original);
        assert_eq!(canonical, "-{{SYNC_PROJECTS}}-myproject");
        let restored = reg.decanonicalize_claude_dir(&canonical);
        assert_eq!(restored, original);
    }

    #[test]
    fn most_specific_custom_token_wins() {
        // Two tokens: {{LONG}} covers /Users/bradmatic/Dev/project (more specific)
        //             {{SHORT}} covers /Users/bradmatic/Dev
        let reg = registry_with_tokens(
            "/Users/bradmatic",
            &[
                ("{{SHORT}}", "/Users/bradmatic/Dev"),
                ("{{LONG}}", "/Users/bradmatic/Dev/project"),
            ],
        );

        // --Users-bradmatic-Dev-project-files-- should use {{LONG}}, not {{SHORT}}
        assert_eq!(
            reg.canonicalize_pi_dir("--Users-bradmatic-Dev-project-files--"),
            "--{{LONG}}-files--"
        );
        // --Users-bradmatic-Dev-other-- should use {{SHORT}}
        assert_eq!(
            reg.canonicalize_pi_dir("--Users-bradmatic-Dev-other--"),
            "--{{SHORT}}-other--"
        );
    }

    #[test]
    fn home_only_path_not_matched_by_custom_token_without_boundary() {
        // Custom token /Users/bradmatic/Dev should NOT match /Users/bradmatic/Developer
        let reg = registry_with_tokens(
            "/Users/bradmatic",
            &[("{{SYNC_PROJECTS}}", "/Users/bradmatic/Dev")],
        );
        // Pi-encoded /Users/bradmatic/Developer/foo = --Users-bradmatic-Developer-foo--
        // After SYNC_HOME: --{{SYNC_HOME}}-Developer-foo--
        // Custom token canonical: "{{SYNC_HOME}}-Dev"
        // "{{SYNC_HOME}}-Developer-foo" does NOT boundary-match "{{SYNC_HOME}}-Dev"
        // because the char after "{{SYNC_HOME}}-Dev" is 'e', not '-'.
        assert_eq!(
            reg.canonicalize_pi_dir("--Users-bradmatic-Developer-foo--"),
            "--{{SYNC_HOME}}-Developer-foo--"
        );
    }

    #[test]
    fn non_home_dir_unchanged_by_canon() {
        let reg = registry("/Users/bradmatic");
        // A directory that doesn't encode the home prefix is passed through.
        assert_eq!(
            reg.canonicalize_pi_dir("--opt-homebrew-cellar--"),
            "--opt-homebrew-cellar--"
        );
    }

    // ── replace_in_text (internal helper) ─────────────────────────────────────

    #[test]
    fn replace_in_text_simple() {
        assert_eq!(
            replace_in_text(
                "/Users/bradmatic/Dev/foo",
                "/Users/bradmatic",
                "{{SYNC_HOME}}"
            ),
            "{{SYNC_HOME}}/Dev/foo"
        );
    }

    #[test]
    fn replace_in_text_exact_match() {
        assert_eq!(
            replace_in_text("/Users/bradmatic", "/Users/bradmatic", "{{SYNC_HOME}}"),
            "{{SYNC_HOME}}"
        );
    }

    #[test]
    fn replace_in_text_no_boundary_no_replace() {
        // "2" after the prefix is not a boundary
        assert_eq!(
            replace_in_text("/Users/bradmatic2/foo", "/Users/bradmatic", "{{SYNC_HOME}}"),
            "/Users/bradmatic2/foo"
        );
    }

    #[test]
    fn replace_in_text_multiple_occurrences() {
        assert_eq!(
            replace_in_text(
                "/Users/bradmatic/a and /Users/bradmatic/b",
                "/Users/bradmatic",
                "{{SYNC_HOME}}"
            ),
            "{{SYNC_HOME}}/a and {{SYNC_HOME}}/b"
        );
    }

    #[test]
    fn replace_in_text_preserves_non_matching_occurrences() {
        // "/Users/bradmatic2" must stay; "/Users/bradmatic/ok" must be replaced
        assert_eq!(
            replace_in_text(
                "/Users/bradmatic2/x and /Users/bradmatic/ok",
                "/Users/bradmatic",
                "{{SYNC_HOME}}"
            ),
            "/Users/bradmatic2/x and {{SYNC_HOME}}/ok"
        );
    }

    #[test]
    fn replace_in_text_empty_from_returns_original() {
        let s = "some text";
        assert_eq!(replace_in_text(s, "", "TOKEN"), s);
    }

    // ── try_canonicalize_path ─────────────────────────────────────────────────

    #[test]
    fn try_canonicalize_path_with_subdir() {
        let reg = registry("/Users/bradmatic");
        assert_eq!(
            reg.try_canonicalize_path("/Users/bradmatic/Dev/foo"),
            Some("{{SYNC_HOME}}/Dev/foo".to_owned())
        );
    }

    #[test]
    fn try_canonicalize_path_exact_home() {
        let reg = registry("/Users/bradmatic");
        assert_eq!(
            reg.try_canonicalize_path("/Users/bradmatic"),
            Some("{{SYNC_HOME}}".to_owned())
        );
    }

    #[test]
    fn try_canonicalize_path_no_match() {
        let reg = registry("/Users/bradmatic");
        assert_eq!(reg.try_canonicalize_path("/tmp/other"), None);
        assert_eq!(reg.try_canonicalize_path("relative/path"), None);
        assert_eq!(reg.try_canonicalize_path("/Users/bradmatic2/foo"), None);
    }

    #[test]
    fn try_canonicalize_path_custom_token() {
        let reg = registry_with_tokens(
            "/Users/bradmatic",
            &[("{{SYNC_PROJECTS}}", "/Users/bradmatic/Dev")],
        );
        assert_eq!(
            reg.try_canonicalize_path("/Users/bradmatic/Dev/myproj"),
            Some("{{SYNC_PROJECTS}}/myproj".to_owned())
        );
        // Path not under custom token: falls back to SYNC_HOME
        assert_eq!(
            reg.try_canonicalize_path("/Users/bradmatic/other"),
            Some("{{SYNC_HOME}}/other".to_owned())
        );
    }

    // ── try_canonicalize_text ─────────────────────────────────────────────────

    #[test]
    fn try_canonicalize_text_finds_embedded_path() {
        let reg = registry("/Users/bradmatic");
        assert_eq!(
            reg.try_canonicalize_text("output: /Users/bradmatic/Dev/foo done"),
            Some("output: {{SYNC_HOME}}/Dev/foo done".to_owned())
        );
    }

    #[test]
    fn try_canonicalize_text_no_home_returns_none() {
        let reg = registry("/Users/bradmatic");
        assert_eq!(reg.try_canonicalize_text("no paths here"), None);
    }

    #[test]
    fn try_canonicalize_text_multiple_occurrences() {
        let reg = registry("/Users/bradmatic");
        assert_eq!(
            reg.try_canonicalize_text("/Users/bradmatic/a and /Users/bradmatic/b"),
            Some("{{SYNC_HOME}}/a and {{SYNC_HOME}}/b".to_owned())
        );
    }

    // ── try_decanonicalize_text ───────────────────────────────────────────────

    #[test]
    fn try_decanonicalize_text_basic() {
        let reg = registry("/Users/bradmatic");
        assert_eq!(
            reg.try_decanonicalize_text("{{SYNC_HOME}}/Dev/foo"),
            Some("/Users/bradmatic/Dev/foo".to_owned())
        );
    }

    #[test]
    fn try_decanonicalize_text_cross_machine() {
        let reg = registry("/home/brad");
        assert_eq!(
            reg.try_decanonicalize_text("{{SYNC_HOME}}/Dev/foo"),
            Some("/home/brad/Dev/foo".to_owned())
        );
    }

    #[test]
    fn try_decanonicalize_text_exact_token() {
        let reg = registry("/Users/bradmatic");
        assert_eq!(
            reg.try_decanonicalize_text("{{SYNC_HOME}}"),
            Some("/Users/bradmatic".to_owned())
        );
    }

    #[test]
    fn try_decanonicalize_text_no_token_returns_none() {
        let reg = registry("/Users/bradmatic");
        assert_eq!(reg.try_decanonicalize_text("/Users/bradmatic/Dev"), None);
    }

    #[test]
    fn try_decanonicalize_text_custom_token() {
        let reg = registry_with_tokens(
            "/Users/bradmatic",
            &[("{{SYNC_PROJECTS}}", "/Users/bradmatic/Dev")],
        );
        // Reverts custom token, then SYNC_HOME
        assert_eq!(
            reg.try_decanonicalize_text("{{SYNC_PROJECTS}}/myproj"),
            Some("/Users/bradmatic/Dev/myproj".to_owned())
        );
    }

    // ── round-trip for content strings ────────────────────────────────────────

    #[test]
    fn content_round_trip_path() {
        let reg = registry("/Users/bradmatic");
        let cases = [
            "/Users/bradmatic/Dev/foo",
            "/Users/bradmatic",
            "/tmp/unrelated",
        ];
        for s in cases {
            let canon = reg.try_canonicalize_path(s);
            let restored = match &canon {
                Some(c) => reg.try_decanonicalize_text(c),
                None => None,
            };
            let final_val = restored.as_deref().or(canon.as_deref()).unwrap_or(s);
            if s.starts_with("/Users/bradmatic") {
                // Should round-trip
                assert_eq!(final_val, s, "content round-trip failed for {s}");
            } else {
                // No change expected
                assert!(canon.is_none(), "unexpected canon for {s}");
            }
        }
    }

    // ── Property-based L1 round-trip tests (US-020) ───────────────────────────

    use proptest::prelude::*;

    proptest! {
        /// L1 Pi directory round-trip with a random home-path user component (§15.3).
        ///
        /// Constructs a valid `--inner--` directory name whose inner string
        /// starts with the Pi-encoded home, then verifies
        /// `decanon_pi(canon_pi(name)) == name`.
        #[test]
        fn prop_l1_pi_round_trip(
            user in "[a-zA-Z][a-zA-Z0-9]{2,10}",
            subs in prop::collection::vec("[a-zA-Z][a-zA-Z0-9]{0,8}", 0..=4),
        ) {
            let home = format!("/Users/{user}");
            let reg = registry(&home);

            // Pi-encoded home for "/Users/<user>" is "Users-<user>".
            // Build: "--Users-<user>[-sub1[-sub2...]]--"
            let mut inner = format!("Users-{user}");
            for c in &subs {
                inner.push('-');
                inner.push_str(c);
            }
            let name = format!("--{inner}--");

            let canonical = reg.canonicalize_pi_dir(&name);
            let restored  = reg.decanonicalize_pi_dir(&canonical);
            prop_assert_eq!(
                &restored,
                &name,
                "Pi L1 round-trip failed for name={} home={}",
                name,
                home
            );
        }

        /// L1 Claude directory round-trip with a random home-path user component (§15.3).
        ///
        /// Constructs a valid `-inner` directory name and verifies
        /// `decanon_claude(canon_claude(name)) == name`.
        #[test]
        fn prop_l1_claude_round_trip(
            user in "[a-zA-Z][a-zA-Z0-9]{2,10}",
            subs in prop::collection::vec("[a-zA-Z][a-zA-Z0-9]{0,8}", 0..=4),
        ) {
            let home = format!("/Users/{user}");
            let reg = registry(&home);

            // Claude-encoded home for "/Users/<user>" is "Users-<user>"
            // (generated components have no dots, so encoding == Pi encoding).
            // Build: "-Users-<user>[-sub1[-sub2...]]"
            let mut inner = format!("Users-{user}");
            for c in &subs {
                inner.push('-');
                inner.push_str(c);
            }
            let name = format!("-{inner}");

            let canonical = reg.canonicalize_claude_dir(&name);
            let restored  = reg.decanonicalize_claude_dir(&canonical);
            prop_assert_eq!(
                &restored,
                &name,
                "Claude L1 round-trip failed for name={} home={}",
                name,
                home
            );
        }
    }
}
