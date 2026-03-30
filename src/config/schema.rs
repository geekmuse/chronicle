use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Root configuration schema for Chronicle.
///
/// Mirrors the layout of `~/.config/chronicle/config.toml` (§8.2).
/// Every section has sensible built-in defaults so a missing config file
/// is never an error.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Config {
    #[serde(default)]
    pub general: GeneralConfig,
    #[serde(default)]
    pub notifications: NotificationsConfig,
    #[serde(default)]
    pub storage: StorageConfig,
    #[serde(default)]
    pub canonicalization: CanonicalizationConfig,
    #[serde(default)]
    pub agents: AgentsConfig,
    #[serde(default)]
    pub sync: SyncConfig,
}

// ---------------------------------------------------------------------------
// [general]
// ---------------------------------------------------------------------------

fn general_default_sync_interval() -> String {
    "5m".to_owned()
}
fn general_default_log_level() -> String {
    "info".to_owned()
}

/// `[general]` section.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneralConfig {
    /// Machine identity — auto-generated on `chronicle init`.
    #[serde(default)]
    pub machine_name: String,

    /// Sync interval used by `chronicle schedule install` (e.g., `"5m"`).
    #[serde(default = "general_default_sync_interval")]
    pub sync_interval: String,

    /// Log level: `trace`, `debug`, `info`, `warn`, `error`.
    #[serde(default = "general_default_log_level")]
    pub log_level: String,

    /// Follow symlinks when scanning session directories.
    #[serde(default)]
    pub follow_symlinks: bool,

    /// Maximum jitter (in seconds) added before a `--quiet` (cron) sync to
    /// stagger machines that share the same cron interval.  Default is `0`,
    /// which means *auto*: chronicle derives a per-machine offset from the
    /// machine name and `sync_interval` so that no two machines with the
    /// same interval fire at the same instant.  Set to an explicit value to
    /// cap the jitter window, or `-1` to disable jitter entirely.
    #[serde(default)]
    pub sync_jitter_secs: i32,
}

impl Default for GeneralConfig {
    fn default() -> Self {
        Self {
            machine_name: String::new(),
            sync_interval: general_default_sync_interval(),
            log_level: general_default_log_level(),
            follow_symlinks: false,
            sync_jitter_secs: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// [notifications]
// ---------------------------------------------------------------------------

fn notifications_default_on_error() -> bool {
    true
}

/// `[notifications]` section.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotificationsConfig {
    /// Log sync errors to stderr.
    #[serde(default = "notifications_default_on_error")]
    pub on_error: bool,

    /// Log every successful sync to stderr.
    #[serde(default)]
    pub on_success: bool,
}

impl Default for NotificationsConfig {
    fn default() -> Self {
        Self {
            on_error: true,
            on_success: false,
        }
    }
}

// ---------------------------------------------------------------------------
// [storage]
// ---------------------------------------------------------------------------

fn storage_default_repo_path() -> String {
    "~/.local/share/chronicle/repo".to_owned()
}
fn storage_default_branch() -> String {
    "main".to_owned()
}

/// `[storage]` section.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageConfig {
    /// Local git repository path (tilde-expanded at use time).
    #[serde(default = "storage_default_repo_path")]
    pub repo_path: String,

    /// Git remote URL — user-defined, Chronicle assumes it already exists.
    #[serde(default)]
    pub remote_url: String,

    /// Git branch.
    #[serde(default = "storage_default_branch")]
    pub branch: String,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            repo_path: storage_default_repo_path(),
            remote_url: String::new(),
            branch: storage_default_branch(),
        }
    }
}

// ---------------------------------------------------------------------------
// [canonicalization]
// ---------------------------------------------------------------------------

fn canon_default_home_token() -> String {
    "{{SYNC_HOME}}".to_owned()
}
fn canon_default_level() -> u8 {
    2
}

/// Serde visitor that rejects any value outside `1..=3`.
fn deserialize_canon_level<'de, D>(d: D) -> Result<u8, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let level = u8::deserialize(d)?;
    if !(1..=3).contains(&level) {
        return Err(serde::de::Error::custom(format!(
            "canonicalization.level must be 1, 2, or 3, got {level}"
        )));
    }
    Ok(level)
}

/// `[canonicalization]` section.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CanonicalizationConfig {
    /// Home directory token (not usually changed).
    #[serde(default = "canon_default_home_token")]
    pub home_token: String,

    /// Canonicalization level: 1 = paths only, 2 = + whitelisted fields,
    /// 3 = + freeform text.  Valid range: **1–3** (enforced at parse time).
    #[serde(
        default = "canon_default_level",
        deserialize_with = "deserialize_canon_level"
    )]
    pub level: u8,

    /// Custom path tokens applied **after** `{{SYNC_HOME}}` during
    /// canonicalization (and **before** during de-canonicalization).
    #[serde(default)]
    pub tokens: HashMap<String, String>,
}

impl Default for CanonicalizationConfig {
    fn default() -> Self {
        Self {
            home_token: canon_default_home_token(),
            level: canon_default_level(),
            tokens: HashMap::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// [agents]
// ---------------------------------------------------------------------------

fn agent_default_enabled() -> bool {
    true
}
fn pi_default_session_dir() -> String {
    "~/.pi/agent/sessions".to_owned()
}
fn claude_default_session_dir() -> String {
    "~/.claude/projects".to_owned()
}

/// Configuration for the Pi agent (`[agents.pi]`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PiAgentConfig {
    /// Whether this agent is enabled for syncing.
    #[serde(default = "agent_default_enabled")]
    pub enabled: bool,

    /// Path to Pi's session directory.
    #[serde(default = "pi_default_session_dir")]
    pub session_dir: String,
}

impl Default for PiAgentConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            session_dir: pi_default_session_dir(),
        }
    }
}

/// Configuration for the Claude agent (`[agents.claude]`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaudeAgentConfig {
    /// Whether this agent is enabled for syncing.
    #[serde(default = "agent_default_enabled")]
    pub enabled: bool,

    /// Path to Claude's session directory.
    #[serde(default = "claude_default_session_dir")]
    pub session_dir: String,
}

impl Default for ClaudeAgentConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            session_dir: claude_default_session_dir(),
        }
    }
}

/// `[agents]` section.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AgentsConfig {
    /// Pi agent configuration (`[agents.pi]`).
    #[serde(default)]
    pub pi: PiAgentConfig,

    /// Claude agent configuration (`[agents.claude]`).
    #[serde(default)]
    pub claude: ClaudeAgentConfig,
}

// ---------------------------------------------------------------------------
// [sync]
// ---------------------------------------------------------------------------

/// History materialization mode.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum HistoryMode {
    /// Materialize all session files locally.
    Full,

    /// Materialize only the N most recent session files per directory.
    #[default]
    Partial,
}

fn sync_default_partial_max_count() -> usize {
    100
}

/// `[sync]` section.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncConfig {
    /// History materialization mode: `"full"` or `"partial"`.
    #[serde(default)]
    pub history_mode: HistoryMode,

    /// Maximum session files to materialize per directory when
    /// `history_mode = "partial"`.
    #[serde(default = "sync_default_partial_max_count")]
    pub partial_max_count: usize,
}

impl Default for SyncConfig {
    fn default() -> Self {
        Self {
            history_mode: HistoryMode::default(),
            partial_max_count: sync_default_partial_max_count(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parse a full Config TOML with only the canonicalization.level field set.
    fn parse_with_level(level: u64) -> Result<Config, toml::de::Error> {
        let s = format!("[canonicalization]\nlevel = {level}\n");
        toml::from_str::<Config>(&s)
    }

    #[test]
    fn level_0_rejected_at_parse_time() {
        assert!(
            parse_with_level(0).is_err(),
            "level 0 must be rejected by the deserializer"
        );
    }

    #[test]
    fn level_1_accepted_at_parse_time() {
        let cfg = parse_with_level(1).expect("level 1 is valid");
        assert_eq!(cfg.canonicalization.level, 1);
    }

    #[test]
    fn level_2_accepted_at_parse_time() {
        let cfg = parse_with_level(2).expect("level 2 is valid");
        assert_eq!(cfg.canonicalization.level, 2);
    }

    #[test]
    fn level_3_accepted_at_parse_time() {
        let cfg = parse_with_level(3).expect("level 3 is valid");
        assert_eq!(cfg.canonicalization.level, 3);
    }

    #[test]
    fn level_4_rejected_at_parse_time() {
        assert!(
            parse_with_level(4).is_err(),
            "level 4 must be rejected by the deserializer"
        );
    }

    #[test]
    fn level_255_rejected_at_parse_time() {
        assert!(
            parse_with_level(255).is_err(),
            "level 255 must be rejected by the deserializer"
        );
    }

    #[test]
    fn default_level_is_2() {
        assert_eq!(Config::default().canonicalization.level, 2);
    }
}
