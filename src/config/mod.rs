pub mod machine_name;
pub mod schema;

use std::path::{Path, PathBuf};

pub use schema::Config;

/// CLI-level config overrides — highest precedence in the loading chain.
///
/// Fields are set by command-line flags and applied after file and environment
/// variable values.
#[derive(Debug, Default, Clone)]
pub struct CliOverrides {
    /// Override for `storage.repo_path`.
    pub repo_path: Option<String>,
    /// Override for `storage.remote_url`.
    pub remote_url: Option<String>,
}

/// Returns the default Chronicle config file path.
///
/// Implements the XDG Base Directory specification §3.1:
/// - Uses `$XDG_CONFIG_HOME/chronicle/config.toml` when `XDG_CONFIG_HOME`
///   is set to an absolute path.
/// - Falls back to `$HOME/.config/chronicle/config.toml` otherwise.
#[must_use]
pub fn default_config_path() -> PathBuf {
    config_path_with_xdg_home(std::env::var("XDG_CONFIG_HOME").ok().as_deref())
}

/// Inner implementation of [`default_config_path`], accepting the
/// `XDG_CONFIG_HOME` value as a parameter for testability.
///
/// Relative paths are ignored per XDG spec (only absolute paths are accepted).
#[must_use]
pub(crate) fn config_path_with_xdg_home(xdg_config_home: Option<&str>) -> PathBuf {
    let config_home = xdg_config_home
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".config")
        });
    config_home.join("chronicle").join("config.toml")
}

/// Expand a `~/…` path to an absolute path using the user's home directory.
///
/// - `~/foo` → `$HOME/foo`
/// - `~` → `$HOME`
/// - Any other path → returned unchanged as a [`PathBuf`]
#[must_use]
pub fn expand_path(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(rest)
    } else if path == "~" {
        dirs::home_dir().unwrap_or_else(|| PathBuf::from("."))
    } else {
        PathBuf::from(path)
    }
}

/// Load configuration following the precedence chain:
///
/// **CLI flags** > **environment variables** > **config file** > **built-in defaults**
///
/// If `config_path` is `None`, [`default_config_path()`] is used.
/// A missing config file is **not** an error — built-in defaults are used.
///
/// # Environment variables
///
/// | Variable | Config key |
/// |---|---|
/// | `CHRONICLE_REPO_PATH` | `storage.repo_path` |
/// | `CHRONICLE_REMOTE_URL` | `storage.remote_url` |
/// | `CHRONICLE_SYNC_INTERVAL` | `general.sync_interval` |
///
/// # Errors
///
/// Returns an error if the config file exists but cannot be read or parsed.
pub fn load(config_path: Option<&Path>, cli: &CliOverrides) -> anyhow::Result<Config> {
    // 1. Determine config file path.
    let path = config_path
        .map(Path::to_path_buf)
        .unwrap_or_else(default_config_path);

    // 2. Load from file, or fall back to built-in defaults.
    let mut config = if path.exists() {
        let content = std::fs::read_to_string(&path)
            .map_err(|e| anyhow::anyhow!("failed to read config file {}: {}", path.display(), e))?;
        toml::from_str::<Config>(&content)
            .map_err(|e| anyhow::anyhow!("failed to parse config file {}: {}", path.display(), e))?
    } else {
        Config::default()
    };

    // 3. Apply environment variable overrides (higher than file, lower than CLI).
    let env_repo = std::env::var("CHRONICLE_REPO_PATH")
        .ok()
        .filter(|v| !v.is_empty());
    let env_remote = std::env::var("CHRONICLE_REMOTE_URL")
        .ok()
        .filter(|v| !v.is_empty());
    let env_interval = std::env::var("CHRONICLE_SYNC_INTERVAL")
        .ok()
        .filter(|v| !v.is_empty());
    apply_env_overrides(
        &mut config,
        env_repo.as_deref(),
        env_remote.as_deref(),
        env_interval.as_deref(),
    );

    // 4. Apply CLI overrides (highest priority).
    apply_cli_overrides(&mut config, cli);

    Ok(config)
}

/// Apply explicit env-var override values to a [`Config`].
///
/// Kept separate from [`load`] so unit tests can verify the precedence logic
/// without mutating `std::env` (which is not safe to do in parallel tests).
pub(crate) fn apply_env_overrides(
    config: &mut Config,
    repo_path: Option<&str>,
    remote_url: Option<&str>,
    sync_interval: Option<&str>,
) {
    if let Some(v) = repo_path {
        config.storage.repo_path = v.to_owned();
    }
    if let Some(v) = remote_url {
        config.storage.remote_url = v.to_owned();
    }
    if let Some(v) = sync_interval {
        config.general.sync_interval = v.to_owned();
    }
}

/// Apply CLI flag overrides (highest precedence in the loading chain).
pub(crate) fn apply_cli_overrides(config: &mut Config, cli: &CliOverrides) {
    if let Some(v) = &cli.repo_path {
        config.storage.repo_path = v.clone();
    }
    if let Some(v) = &cli.remote_url {
        config.storage.remote_url = v.clone();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;
    use tempfile::NamedTempFile;

    /// Every default value must match the spec §8.2.
    #[test]
    fn defaults_are_correct() {
        let cfg = Config::default();

        // [general]
        assert!(
            cfg.general.machine_name.is_empty(),
            "machine_name default is empty"
        );
        assert_eq!(cfg.general.sync_interval, "5m");
        assert_eq!(cfg.general.log_level, "info");
        assert!(!cfg.general.follow_symlinks);

        // [notifications]
        assert!(cfg.notifications.on_error, "on_error default is true");
        assert!(!cfg.notifications.on_success, "on_success default is false");

        // [storage]
        assert_eq!(cfg.storage.repo_path, "~/.local/share/chronicle/repo");
        assert_eq!(cfg.storage.remote_url, "");
        assert_eq!(cfg.storage.branch, "main");

        // [canonicalization]
        assert_eq!(cfg.canonicalization.home_token, "{{SYNC_HOME}}");
        assert_eq!(cfg.canonicalization.level, 2);
        assert!(cfg.canonicalization.tokens.is_empty());

        // [agents.pi]
        assert!(cfg.agents.pi.enabled);
        assert_eq!(cfg.agents.pi.session_dir, "~/.pi/agent/sessions");

        // [agents.claude]
        assert!(cfg.agents.claude.enabled);
        assert_eq!(cfg.agents.claude.session_dir, "~/.claude/projects");

        // [sync]
        assert_eq!(cfg.sync.history_mode, schema::HistoryMode::Partial);
        assert_eq!(cfg.sync.partial_max_count, 100);
    }

    /// A missing config file must produce sensible defaults without error.
    #[test]
    fn missing_file_returns_defaults() {
        let cfg = load(
            Some(Path::new("/nonexistent/chronicle/config.toml")),
            &CliOverrides::default(),
        )
        .expect("load should succeed when config file is absent");

        assert_eq!(cfg.storage.branch, "main");
        assert_eq!(cfg.general.sync_interval, "5m");
    }

    /// A partial TOML file is parsed; unspecified fields retain their defaults.
    #[test]
    fn loads_partial_toml_file() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(
            f,
            r#"
[general]
sync_interval = "10m"
machine_name = "happy-hippo"

[storage]
remote_url = "git@example.com:repo.git"
"#
        )
        .unwrap();

        let cfg = load(Some(f.path()), &CliOverrides::default()).unwrap();

        assert_eq!(cfg.general.sync_interval, "10m");
        assert_eq!(cfg.general.machine_name, "happy-hippo");
        assert_eq!(cfg.storage.remote_url, "git@example.com:repo.git");
        // Defaults preserved for unspecified fields.
        assert_eq!(cfg.storage.branch, "main");
        assert_eq!(cfg.storage.repo_path, "~/.local/share/chronicle/repo");
        assert_eq!(cfg.canonicalization.level, 2);
    }

    /// A complete TOML file with all sections is parsed correctly.
    #[test]
    fn loads_full_toml_file() {
        let mut f = NamedTempFile::new().unwrap();
        // Use write_all to avoid writeln! treating {{ / }} as escaped format braces.
        f.write_all(
            br#"
[general]
machine_name = "stellar-stoat"
sync_interval = "15m"
log_level = "debug"
follow_symlinks = true

[notifications]
on_error = false
on_success = true

[storage]
repo_path = "/data/chronicle/repo"
remote_url = "git@github.com:user/sessions.git"
branch = "trunk"

[canonicalization]
home_token = "{{SYNC_HOME}}"
level = 3

[canonicalization.tokens]
"{{SYNC_PROJECTS}}" = "/data/projects"

[agents.pi]
enabled = false
session_dir = "/opt/pi/sessions"

[agents.claude]
enabled = true
session_dir = "/opt/claude/projects"

[sync]
history_mode = "full"
partial_max_count = 50
"#,
        )
        .unwrap();

        let cfg = load(Some(f.path()), &CliOverrides::default()).unwrap();

        assert_eq!(cfg.general.machine_name, "stellar-stoat");
        assert_eq!(cfg.general.sync_interval, "15m");
        assert_eq!(cfg.general.log_level, "debug");
        assert!(cfg.general.follow_symlinks);

        assert!(!cfg.notifications.on_error);
        assert!(cfg.notifications.on_success);

        assert_eq!(cfg.storage.repo_path, "/data/chronicle/repo");
        assert_eq!(cfg.storage.branch, "trunk");

        assert_eq!(cfg.canonicalization.level, 3);
        assert_eq!(
            cfg.canonicalization
                .tokens
                .get("{{SYNC_PROJECTS}}")
                .map(String::as_str),
            Some("/data/projects")
        );

        assert!(!cfg.agents.pi.enabled);
        assert_eq!(cfg.agents.pi.session_dir, "/opt/pi/sessions");

        assert_eq!(cfg.sync.history_mode, schema::HistoryMode::Full);
        assert_eq!(cfg.sync.partial_max_count, 50);
    }

    /// Environment variable values override config file values.
    #[test]
    fn env_overrides_file_values() {
        let mut config = Config::default();
        config.storage.repo_path = "/from/file".to_owned();
        config.storage.remote_url = "file://from/file".to_owned();
        config.general.sync_interval = "30m".to_owned();

        apply_env_overrides(
            &mut config,
            Some("/from/env"),
            Some("env://remote"),
            Some("15m"),
        );

        assert_eq!(config.storage.repo_path, "/from/env");
        assert_eq!(config.storage.remote_url, "env://remote");
        assert_eq!(config.general.sync_interval, "15m");
    }

    /// Empty env var values do NOT override file values.
    #[test]
    fn empty_env_values_do_not_override() {
        let mut config = Config::default();
        config.storage.repo_path = "/from/file".to_owned();

        apply_env_overrides(&mut config, None, None, None);

        assert_eq!(config.storage.repo_path, "/from/file");
    }

    /// CLI flag values override environment variable values.
    #[test]
    fn cli_overrides_env_values() {
        let mut config = Config::default();
        // Simulate env level already applied.
        config.storage.repo_path = "/from/env".to_owned();
        config.storage.remote_url = "env://remote".to_owned();

        let cli = CliOverrides {
            repo_path: Some("/from/cli".to_owned()),
            remote_url: Some("cli://remote".to_owned()),
        };
        apply_cli_overrides(&mut config, &cli);

        assert_eq!(config.storage.repo_path, "/from/cli");
        assert_eq!(config.storage.remote_url, "cli://remote");
    }

    /// Full precedence chain: defaults → file → env vars → CLI flags.
    #[test]
    fn precedence_chain_all_levels() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(
            f,
            r#"
[general]
sync_interval = "10m"

[storage]
repo_path = "/from/file"
remote_url = "file://remote"
"#
        )
        .unwrap();

        // Step 1: defaults + file.
        let mut cfg = load(Some(f.path()), &CliOverrides::default()).unwrap();
        assert_eq!(cfg.general.sync_interval, "10m"); // file wins over default
        assert_eq!(cfg.storage.repo_path, "/from/file"); // from file

        // Step 2: apply env overrides — repo_path changes, sync_interval unchanged.
        apply_env_overrides(&mut cfg, Some("/from/env"), None, None);
        assert_eq!(cfg.storage.repo_path, "/from/env"); // env wins over file
        assert_eq!(cfg.general.sync_interval, "10m"); // file still wins (no env for this key)

        // Step 3: apply CLI overrides — remote_url changes, others unchanged.
        let cli = CliOverrides {
            repo_path: None,
            remote_url: Some("cli://remote".to_owned()),
        };
        apply_cli_overrides(&mut cfg, &cli);
        assert_eq!(cfg.storage.remote_url, "cli://remote"); // CLI wins over file
        assert_eq!(cfg.storage.repo_path, "/from/env"); // env still holds
        assert_eq!(cfg.general.sync_interval, "10m"); // file still holds
    }

    /// XDG_CONFIG_HOME absolute path is respected for config path construction.
    #[test]
    fn xdg_config_home_absolute_path_respected() {
        let result = config_path_with_xdg_home(Some("/home/user/.config-custom"));
        assert_eq!(
            result,
            PathBuf::from("/home/user/.config-custom/chronicle/config.toml")
        );
    }

    /// Relative XDG_CONFIG_HOME is ignored per XDG spec (§3.1); falls back to ~/.config.
    #[test]
    fn xdg_config_home_relative_path_ignored() {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        let expected = home.join(".config").join("chronicle").join("config.toml");
        let result = config_path_with_xdg_home(Some("relative/path"));
        assert_eq!(result, expected);
    }

    /// Absent XDG_CONFIG_HOME falls back to `~/.config/chronicle/config.toml`.
    #[test]
    fn xdg_config_home_absent_uses_dotconfig() {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        let expected = home.join(".config").join("chronicle").join("config.toml");
        let result = config_path_with_xdg_home(None);
        assert_eq!(result, expected);
    }

    /// `expand_path` correctly expands tilde prefixes.
    #[test]
    fn expand_path_tilde_slash() {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        assert_eq!(expand_path("~/foo/bar"), home.join("foo").join("bar"));
    }

    #[test]
    fn expand_path_bare_tilde() {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        assert_eq!(expand_path("~"), home);
    }

    #[test]
    fn expand_path_absolute_unchanged() {
        assert_eq!(expand_path("/abs/path"), PathBuf::from("/abs/path"));
    }

    #[test]
    fn expand_path_relative_unchanged() {
        assert_eq!(expand_path("relative/path"), PathBuf::from("relative/path"));
    }
}
