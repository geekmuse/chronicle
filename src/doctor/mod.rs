//! chronicle doctor — health-check subsystem.
//!
//! Defines the [`CheckState`] / [`CheckResult`] data model and the four
//! pure check functions used by `chronicle doctor`.  All check functions
//! accept injected paths/state so they are unit-testable without a real
//! filesystem or network.

use std::fs;
use std::net::ToSocketAddrs as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

// ---------------------------------------------------------------------------
// CheckState
// ---------------------------------------------------------------------------

/// Outcome of a single `chronicle doctor` check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckState {
    /// The check passed without issues.
    Pass,
    /// The check revealed a non-critical concern.
    Warn,
    /// The check detected a problem that requires attention.
    Error,
    /// The check was intentionally skipped (e.g. no remote configured).
    Skipped,
}

// ---------------------------------------------------------------------------
// CheckResult
// ---------------------------------------------------------------------------

/// Result of a single `chronicle doctor` check.
#[derive(Debug, Clone)]
pub struct CheckResult {
    /// Stable dot-separated key identifying the check (e.g. `"config.file"`).
    pub key: String,
    /// Outcome of the check.
    pub state: CheckState,
    /// Human-readable detail line.
    pub detail: String,
    /// Optional remediation hint shown when `state` is [`CheckState::Error`]
    /// or [`CheckState::Warn`].
    pub hint: Option<String>,
}

impl CheckResult {
    /// Construct a [`CheckState::Pass`] result with no hint.
    #[must_use]
    pub fn pass(key: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            state: CheckState::Pass,
            detail: detail.into(),
            hint: None,
        }
    }

    /// Construct a [`CheckState::Warn`] result with a remediation hint.
    #[must_use]
    pub fn warn(
        key: impl Into<String>,
        detail: impl Into<String>,
        hint: impl Into<String>,
    ) -> Self {
        Self {
            key: key.into(),
            state: CheckState::Warn,
            detail: detail.into(),
            hint: Some(hint.into()),
        }
    }

    /// Construct a [`CheckState::Error`] result with a remediation hint.
    #[must_use]
    pub fn error(
        key: impl Into<String>,
        detail: impl Into<String>,
        hint: impl Into<String>,
    ) -> Self {
        Self {
            key: key.into(),
            state: CheckState::Error,
            detail: detail.into(),
            hint: Some(hint.into()),
        }
    }

    /// Construct a [`CheckState::Skipped`] result with a reason.
    #[must_use]
    pub fn skipped(key: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            state: CheckState::Skipped,
            detail: reason.into(),
            hint: None,
        }
    }
}

// ---------------------------------------------------------------------------
// check_config
// ---------------------------------------------------------------------------

/// Check the Chronicle configuration file.
///
/// Returns 3 [`CheckResult`]s in order:
///
/// | Key | What is checked |
/// |-----|----------------|
/// | `config.file`   | Config file exists on disk |
/// | `config.toml`   | Config file is valid TOML (skipped if file missing) |
/// | `config.remote` | `storage.remote_url` is non-empty (skipped if TOML invalid) |
#[must_use]
pub fn check_config(config_path: &Path) -> Vec<CheckResult> {
    let mut results = Vec::with_capacity(3);

    // --- config.file -------------------------------------------------------
    if !config_path.exists() {
        results.push(CheckResult::error(
            "config.file",
            format!("not found at {}", config_path.display()),
            "run `chronicle init` to create the config file",
        ));
        results.push(CheckResult::skipped("config.toml", "config file missing"));
        results.push(CheckResult::skipped("config.remote", "config file missing"));
        return results;
    }
    results.push(CheckResult::pass(
        "config.file",
        format!("found at {}", config_path.display()),
    ));

    // --- config.toml -------------------------------------------------------
    let raw = match fs::read_to_string(config_path) {
        Ok(s) => s,
        Err(e) => {
            results.push(CheckResult::error(
                "config.toml",
                format!("cannot read: {e}"),
                "check file permissions",
            ));
            results.push(CheckResult::skipped("config.remote", "config unreadable"));
            return results;
        }
    };

    let cfg = match toml::from_str::<crate::config::schema::Config>(&raw) {
        Ok(c) => c,
        Err(e) => {
            results.push(CheckResult::error(
                "config.toml",
                format!("invalid TOML: {e}"),
                "fix syntax errors or run `chronicle init` to regenerate",
            ));
            results.push(CheckResult::skipped("config.remote", "config invalid"));
            return results;
        }
    };
    results.push(CheckResult::pass("config.toml", "valid"));

    // --- config.remote -----------------------------------------------------
    if cfg.storage.remote_url.is_empty() {
        results.push(CheckResult::error(
            "config.remote",
            "remote URL not configured",
            "run `chronicle init --remote <url>` to set a remote",
        ));
    } else {
        results.push(CheckResult::pass(
            "config.remote",
            cfg.storage.remote_url.clone(),
        ));
    }

    results
}

// ---------------------------------------------------------------------------
// check_git — helpers
// ---------------------------------------------------------------------------

/// Returns the default SSH key paths to probe, in preference order.
///
/// Probes: `~/.ssh/id_ed25519`, `~/.ssh/id_ecdsa`, `~/.ssh/id_rsa`.
#[must_use]
pub fn default_ssh_key_paths(home: &Path) -> Vec<PathBuf> {
    ["id_ed25519", "id_ecdsa", "id_rsa"]
        .iter()
        .map(|name| home.join(".ssh").join(name))
        .collect()
}

/// Returns `true` when an SSH agent is reachable via `SSH_AUTH_SOCK`.
///
/// Covers keys loaded via `ssh-add`, macOS Keychain agent, 1Password SSH
/// agent, and similar forwarding mechanisms.  Chronicle can authenticate
/// through the agent even when no key file exists on disk.
#[must_use]
pub fn ssh_agent_available() -> bool {
    std::env::var_os("SSH_AUTH_SOCK")
        .filter(|s| !s.is_empty())
        .map(std::path::PathBuf::from)
        .is_some_and(|p| p.exists())
}

/// Returns `true` when the remote URL uses HTTP or HTTPS.
#[must_use]
pub fn is_https_remote(url: &str) -> bool {
    url.starts_with("https://") || url.starts_with("http://")
}

/// Parse the `(host, port)` pair from a git remote URL.
///
/// Supports `https://`, `http://`, `git://`, `ssh://`, and SCP-style
/// (`[user@]host:path`) formats.
fn parse_remote_host_port(url: &str) -> Result<(String, u16), String> {
    // Scheme-prefixed URLs
    for (prefix, default_port) in &[
        ("https://", 443_u16),
        ("http://", 80_u16),
        ("git://", 9418_u16),
    ] {
        if let Some(rest) = url.strip_prefix(prefix) {
            let authority = rest.split('/').next().unwrap_or("");
            return split_host_port(authority, *default_port);
        }
    }

    // ssh://
    if let Some(rest) = url.strip_prefix("ssh://") {
        let rest = rest.split_once('@').map_or(rest, |(_, r)| r);
        let authority = rest.split('/').next().unwrap_or("");
        return split_host_port(authority, 22);
    }

    // SCP-style: [user@]host:path  (no scheme)
    let without_user = url.split_once('@').map_or(url, |(_, r)| r);
    if let Some(colon_pos) = without_user.find(':') {
        let host = &without_user[..colon_pos];
        // Guard against Windows drive letters (e.g. C:/) and IPv6 literals
        if host.len() > 1 && !host.contains('/') {
            return Ok((host.to_owned(), 22));
        }
    }

    Err(format!("cannot parse host from remote URL: {url}"))
}

/// Split an authority string (`host` or `host:port`) into `(host, port)`.
fn split_host_port(authority: &str, default_port: u16) -> Result<(String, u16), String> {
    // IPv6 literal: [::1]:port
    if let Some(rest) = authority.strip_prefix('[') {
        if let Some((host, port_part)) = rest.split_once(']') {
            let port = if let Some(p) = port_part.strip_prefix(':') {
                p.parse::<u16>()
                    .map_err(|e| format!("invalid port in authority: {e}"))?
            } else {
                default_port
            };
            return Ok((host.to_owned(), port));
        }
    }
    // Regular host[:port]
    if let Some((host, port_str)) = authority.rsplit_once(':') {
        if !host.is_empty() {
            if let Ok(port) = port_str.parse::<u16>() {
                return Ok((host.to_owned(), port));
            }
        }
    }
    Ok((authority.to_owned(), default_port))
}

/// Production remote-reachability check: TCP connect with a 5-second timeout.
///
/// Resolves the host via DNS, then opens a TCP connection to the resulting
/// address.  Pass this to [`check_git`] in production; inject a stub in tests.
///
/// # Errors
///
/// Returns an error string if DNS resolution fails, no addresses are found,
/// or the connection times out.
pub fn default_check_remote(url: &str) -> Result<(), String> {
    let (host, port) = parse_remote_host_port(url)?;
    let addr = (host.as_str(), port)
        .to_socket_addrs()
        .map_err(|e| format!("cannot resolve `{host}`: {e}"))?
        .next()
        .ok_or_else(|| format!("no addresses resolved for `{host}`"))?;
    std::net::TcpStream::connect_timeout(&addr, Duration::from_secs(5))
        .map(|_| ())
        .map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// check_git
// ---------------------------------------------------------------------------

/// Check the Git repository and remote connectivity.
///
/// Returns up to 3 [`CheckResult`]s:
///
/// | Key          | What is checked |
/// |--------------|----------------|
/// | `git.repo`   | Local repository is initialized |
/// | `git.remote` | Remote is reachable (skipped if `remote_url` is empty) |
/// | `git.ssh_key`| An SSH key file is readable **or** an SSH agent is reachable (skipped for HTTPS remotes or no remote) |
///
/// `ssh_key_paths` — ordered list of SSH key paths to probe; pass
/// `default_ssh_key_paths(home)` in production.
///
/// `check_remote` — closure invoked with the remote URL; returns `Ok(())`
/// on success or an `Err(reason)` string.  Inject a stub in tests to avoid
/// real network calls.
///
/// `ssh_agent_fn` — returns `true` when an SSH agent is reachable (e.g.
/// `SSH_AUTH_SOCK` is set and the socket exists).  Pass
/// `|| ssh_agent_available()` in production; inject a stub in tests.
#[must_use]
pub fn check_git(
    repo_path: &Path,
    remote_url: &str,
    ssh_key_paths: &[PathBuf],
    check_remote: impl Fn(&str) -> Result<(), String>,
    ssh_agent_fn: impl Fn() -> bool,
) -> Vec<CheckResult> {
    let mut results = Vec::with_capacity(3);

    // --- git.repo ----------------------------------------------------------
    if git2::Repository::open(repo_path).is_ok() {
        results.push(CheckResult::pass(
            "git.repo",
            format!("initialized at {}", repo_path.display()),
        ));
    } else {
        results.push(CheckResult::error(
            "git.repo",
            format!("not initialized at {}", repo_path.display()),
            "run `chronicle init` to initialize the repository",
        ));
    }

    if remote_url.is_empty() {
        results.push(CheckResult::skipped("git.remote", "no remote configured"));
        results.push(CheckResult::skipped("git.ssh_key", "no remote configured"));
        return results;
    }

    // --- git.remote --------------------------------------------------------
    match check_remote(remote_url) {
        Ok(()) => results.push(CheckResult::pass("git.remote", "reachable")),
        Err(e) => results.push(CheckResult::error(
            "git.remote",
            format!("unreachable: {e}"),
            "check network connectivity and remote URL",
        )),
    }

    // --- git.ssh_key -------------------------------------------------------
    if is_https_remote(remote_url) {
        results.push(CheckResult::skipped("git.ssh_key", "HTTPS remote"));
    } else {
        let found_file = ssh_key_paths
            .iter()
            .find(|p| p.exists() && fs::metadata(p).is_ok());
        if let Some(key_path) = found_file {
            results.push(CheckResult::pass(
                "git.ssh_key",
                format!("found {}", key_path.display()),
            ));
        } else if ssh_agent_fn() {
            // No key file on disk, but a loaded agent is a valid auth source
            // (ssh-add, macOS Keychain, 1Password SSH agent, agent forwarding, …).
            results.push(CheckResult::pass("git.ssh_key", "key loaded in SSH agent"));
        } else {
            results.push(CheckResult::error(
                "git.ssh_key",
                "no SSH key file found and no SSH agent available",
                "add your key to the agent (`ssh-add`), create a key file (`ssh-keygen`), or switch to an HTTPS remote",
            ));
        }
    }

    results
}

// ---------------------------------------------------------------------------
// check_agents — helpers
// ---------------------------------------------------------------------------

/// Recursively count `.jsonl` files under `dir`.
fn count_jsonl_files(dir: &Path) -> usize {
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };
    let mut count = 0_usize;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            count += count_jsonl_files(&path);
        } else if path.extension().is_some_and(|ext| ext == "jsonl") {
            count += 1;
        }
    }
    count
}

// ---------------------------------------------------------------------------
// check_agents
// ---------------------------------------------------------------------------

/// Check each enabled agent's session directory.
///
/// Returns one [`CheckResult`] per agent (both Pi and Claude), using
/// [`CheckState::Skipped`] for disabled agents.
///
/// | Key             | What is checked |
/// |-----------------|----------------|
/// | `agents.pi`     | Pi session directory exists; file count on pass |
/// | `agents.claude` | Claude session directory exists; file count on pass |
#[must_use]
pub fn check_agents(
    pi_enabled: bool,
    pi_session_dir: &Path,
    claude_enabled: bool,
    claude_session_dir: &Path,
) -> Vec<CheckResult> {
    let mut results = Vec::with_capacity(2);

    // --- agents.pi ---------------------------------------------------------
    if pi_enabled {
        if pi_session_dir.exists() {
            let count = count_jsonl_files(pi_session_dir);
            results.push(CheckResult::pass(
                "agents.pi",
                format!("{count} session file(s) in {}", pi_session_dir.display()),
            ));
        } else {
            results.push(CheckResult::error(
                "agents.pi",
                format!("sessions directory not found: {}", pi_session_dir.display()),
                "create the directory or update `agents.pi.session_dir` in config",
            ));
        }
    } else {
        results.push(CheckResult::skipped(
            "agents.pi",
            "agent disabled in config",
        ));
    }

    // --- agents.claude -----------------------------------------------------
    if claude_enabled {
        if claude_session_dir.exists() {
            let count = count_jsonl_files(claude_session_dir);
            results.push(CheckResult::pass(
                "agents.claude",
                format!(
                    "{count} session file(s) in {}",
                    claude_session_dir.display()
                ),
            ));
        } else {
            results.push(CheckResult::error(
                "agents.claude",
                format!(
                    "sessions directory not found: {}",
                    claude_session_dir.display()
                ),
                "create the directory or update `agents.claude.session_dir` in config",
            ));
        }
    } else {
        results.push(CheckResult::skipped(
            "agents.claude",
            "agent disabled in config",
        ));
    }

    results
}

// ---------------------------------------------------------------------------
// check_scheduler — helpers
// ---------------------------------------------------------------------------

/// Returns `true` when a process with the given PID is still running.
#[cfg(unix)]
fn is_pid_alive(pid: u32) -> bool {
    // kill(pid, 0) checks existence without sending a signal.
    #[allow(clippy::cast_possible_wrap)]
    let ret = unsafe { libc::kill(pid as libc::pid_t, 0) };
    if ret == 0 {
        return true;
    }
    // EPERM means the process exists but we lack permission to signal it.
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(not(unix))]
fn is_pid_alive(_pid: u32) -> bool {
    // Cannot verify on non-Unix; assume alive (fall back to age check only).
    true
}

// ---------------------------------------------------------------------------
// check_scheduler
// ---------------------------------------------------------------------------

/// Check the scheduler (crontab) and advisory lock state.
///
/// Returns 2 [`CheckResult`]s:
///
/// | Key                | What is checked |
/// |--------------------|----------------|
/// | `scheduler.cron`   | Chronicle crontab entry is installed |
/// | `scheduler.lock`   | Advisory lock is free, held by a live process within timeout, or stale with a dead PID (warn) |
///
/// `crontab_lines` — pre-read crontab lines (inject `[]` in tests).
/// `lock_path`     — path to the advisory lock file.
/// `lock_timeout_secs` — age threshold for stale-lock detection (from config).
#[must_use]
pub fn check_scheduler(
    crontab_lines: &[String],
    lock_path: &Path,
    lock_timeout_secs: i64,
) -> Vec<CheckResult> {
    let mut results = Vec::with_capacity(2);

    // --- scheduler.cron ----------------------------------------------------
    let st = crate::scheduler::cron::parse_status(crontab_lines);
    if st.installed {
        results.push(CheckResult::pass("scheduler.cron", "installed"));
    } else {
        results.push(CheckResult::warn(
            "scheduler.cron",
            "not installed",
            "run `chronicle schedule install` to enable automatic sync",
        ));
    }

    // --- scheduler.lock ----------------------------------------------------
    if !lock_path.exists() {
        results.push(CheckResult::pass("scheduler.lock", "free"));
        return results;
    }

    let contents = match fs::read_to_string(lock_path) {
        Ok(c) => c,
        Err(_) => {
            // Cannot read the lock file — treat as free.
            results.push(CheckResult::pass("scheduler.lock", "free"));
            return results;
        }
    };

    let mut parts = contents.split_whitespace();
    let pid: Option<u32> = parts.next().and_then(|s| s.parse().ok());
    let stamp: Option<u64> = parts.next().and_then(|s| s.parse().ok());

    let pid_dead = pid.is_some_and(|p| !is_pid_alive(p));

    #[allow(clippy::cast_sign_loss)]
    let age_exceeded = lock_timeout_secs > 0
        && stamp.is_some_and(|ts| {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            now.saturating_sub(ts) > lock_timeout_secs as u64
        });

    let pid_str = pid.map_or_else(|| "?".to_owned(), |p| p.to_string());

    if pid_dead {
        // The process that held the lock has exited — the sync completed (or
        // crashed). The orphaned file is harmless and is auto-cleared at the
        // start of the next sync. Report as Warn, not Error, so that a
        // successfully completed sync does not appear to have failed.
        results.push(CheckResult::warn(
            "scheduler.lock",
            format!("stale lock — PID {pid_str} has exited"),
            "orphaned lock; auto-cleared on the next sync, or run `chronicle sync` to clear it now",
        ));
    } else if age_exceeded {
        // PID is still alive but has been holding the lock longer than
        // `lock_timeout_secs`. This indicates a potentially hung sync.
        results.push(CheckResult::error(
            "scheduler.lock",
            format!(
                "PID {pid_str} has been running for over {lock_timeout_secs}s (possible hung sync)"
            ),
            "check whether the process is stuck, then delete the lock file or run `chronicle sync`",
        ));
    } else {
        results.push(CheckResult::warn(
            "scheduler.lock",
            format!("held by PID {pid_str} (sync in progress)"),
            "wait for the sync to complete",
        ));
    }

    results
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    // -----------------------------------------------------------------------
    // check_config
    // -----------------------------------------------------------------------

    #[test]
    fn check_config_file_missing_returns_error_then_two_skipped() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join("config.toml");

        let results = check_config(&config_path);
        assert_eq!(results.len(), 3);
        assert_eq!(results[0].key, "config.file");
        assert_eq!(results[0].state, CheckState::Error);
        assert_eq!(results[1].key, "config.toml");
        assert_eq!(results[1].state, CheckState::Skipped);
        assert_eq!(results[2].key, "config.remote");
        assert_eq!(results[2].state, CheckState::Skipped);
    }

    #[test]
    fn check_config_invalid_toml_returns_error_and_skipped_remote() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join("config.toml");
        fs::write(&config_path, b"not valid = [toml\n").unwrap();

        let results = check_config(&config_path);
        assert_eq!(results.len(), 3);
        assert_eq!(results[0].state, CheckState::Pass, "config.file");
        assert_eq!(results[1].state, CheckState::Error, "config.toml");
        assert!(
            results[1].detail.contains("invalid TOML"),
            "detail: {}",
            results[1].detail
        );
        assert_eq!(results[2].state, CheckState::Skipped, "config.remote");
    }

    #[test]
    fn check_config_empty_remote_returns_error() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join("config.toml");
        fs::write(&config_path, b"[general]\nmachine_name = \"test\"\n").unwrap();

        let results = check_config(&config_path);
        assert_eq!(results.len(), 3);
        assert_eq!(results[0].state, CheckState::Pass, "config.file");
        assert_eq!(results[1].state, CheckState::Pass, "config.toml");
        assert_eq!(results[2].key, "config.remote");
        assert_eq!(results[2].state, CheckState::Error, "config.remote");
    }

    #[test]
    fn check_config_all_pass() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join("config.toml");
        fs::write(
            &config_path,
            b"[storage]\nremote_url = \"git@github.com:user/repo.git\"\n",
        )
        .unwrap();

        let results = check_config(&config_path);
        assert_eq!(results.len(), 3);
        assert_eq!(results[0].state, CheckState::Pass, "config.file");
        assert_eq!(results[1].state, CheckState::Pass, "config.toml");
        assert_eq!(results[2].state, CheckState::Pass, "config.remote");
    }

    // -----------------------------------------------------------------------
    // check_git
    // -----------------------------------------------------------------------

    #[test]
    fn check_git_repo_not_initialized_returns_error_and_skipped() {
        let dir = TempDir::new().unwrap();
        let repo_path = dir.path().join("repo");
        fs::create_dir_all(&repo_path).unwrap();

        let results = check_git(&repo_path, "", &[], |_| Ok(()), || false);
        assert_eq!(results.len(), 3);
        assert_eq!(results[0].key, "git.repo");
        assert_eq!(results[0].state, CheckState::Error);
        assert_eq!(results[1].state, CheckState::Skipped, "git.remote");
        assert_eq!(results[2].state, CheckState::Skipped, "git.ssh_key");
    }

    #[test]
    fn check_git_no_remote_repo_ok_all_skipped() {
        let dir = TempDir::new().unwrap();
        let repo_path = dir.path().join("repo");
        git2::Repository::init(&repo_path).unwrap();

        let results = check_git(&repo_path, "", &[], |_| Ok(()), || false);
        assert_eq!(results.len(), 3);
        assert_eq!(results[0].state, CheckState::Pass, "git.repo");
        assert_eq!(results[1].state, CheckState::Skipped, "git.remote");
        assert_eq!(results[2].state, CheckState::Skipped, "git.ssh_key");
    }

    #[test]
    fn check_git_https_remote_reachable_ssh_skipped() {
        let dir = TempDir::new().unwrap();
        let repo_path = dir.path().join("repo");
        git2::Repository::init(&repo_path).unwrap();

        let results = check_git(
            &repo_path,
            "https://github.com/user/repo.git",
            &[],
            |_| Ok(()),
            || false,
        );
        assert_eq!(results.len(), 3);
        assert_eq!(results[0].state, CheckState::Pass, "git.repo");
        assert_eq!(results[1].state, CheckState::Pass, "git.remote");
        assert_eq!(results[2].state, CheckState::Skipped, "git.ssh_key (HTTPS)");
    }

    #[test]
    fn check_git_https_remote_unreachable_returns_error() {
        let dir = TempDir::new().unwrap();
        let repo_path = dir.path().join("repo");
        git2::Repository::init(&repo_path).unwrap();

        let results = check_git(
            &repo_path,
            "https://github.com/user/repo.git",
            &[],
            |_| Err("connection refused".to_owned()),
            || false,
        );
        assert_eq!(results[1].state, CheckState::Error, "git.remote");
        assert!(
            results[1].detail.contains("unreachable"),
            "detail: {}",
            results[1].detail
        );
        assert_eq!(results[2].state, CheckState::Skipped, "git.ssh_key (HTTPS)");
    }

    #[test]
    fn check_git_ssh_remote_key_found_all_pass() {
        let dir = TempDir::new().unwrap();
        let repo_path = dir.path().join("repo");
        git2::Repository::init(&repo_path).unwrap();

        // Create a fake SSH key file.
        let key_path = dir.path().join("id_ed25519");
        fs::write(&key_path, b"fake key content").unwrap();

        let results = check_git(
            &repo_path,
            "git@github.com:user/repo.git",
            &[key_path.clone()],
            |_| Ok(()),
            || false,
        );
        assert_eq!(results.len(), 3);
        assert_eq!(results[0].state, CheckState::Pass, "git.repo");
        assert_eq!(results[1].state, CheckState::Pass, "git.remote");
        assert_eq!(results[2].state, CheckState::Pass, "git.ssh_key");
        assert!(
            results[2].detail.contains("found"),
            "detail: {}",
            results[2].detail
        );
    }

    #[test]
    fn check_git_ssh_remote_agent_available_no_key_file_returns_pass() {
        let dir = TempDir::new().unwrap();
        let repo_path = dir.path().join("repo");
        git2::Repository::init(&repo_path).unwrap();

        // No key file on disk, but agent reports keys loaded.
        let nonexistent = dir.path().join("id_ed25519_missing");
        let results = check_git(
            &repo_path,
            "git@github.com:user/repo.git",
            &[nonexistent],
            |_| Ok(()),
            || true, // agent available
        );
        assert_eq!(
            results[2].state,
            CheckState::Pass,
            "agent covers missing key file"
        );
        assert!(
            results[2].detail.contains("SSH agent"),
            "detail: {}",
            results[2].detail
        );
    }

    #[test]
    fn check_git_ssh_remote_no_key_no_agent_returns_error() {
        let dir = TempDir::new().unwrap();
        let repo_path = dir.path().join("repo");
        git2::Repository::init(&repo_path).unwrap();

        // No key file and no agent.
        let nonexistent = dir.path().join("id_ed25519_missing");
        let results = check_git(
            &repo_path,
            "git@github.com:user/repo.git",
            &[nonexistent],
            |_| Ok(()),
            || false, // no agent
        );
        assert_eq!(results[2].state, CheckState::Error, "git.ssh_key");
        assert!(
            results[2].detail.contains("no SSH key file"),
            "detail: {}",
            results[2].detail
        );
    }

    // -----------------------------------------------------------------------
    // check_agents
    // -----------------------------------------------------------------------

    #[test]
    fn check_agents_pi_disabled_returns_skipped() {
        let dir = TempDir::new().unwrap();
        let pi_dir = dir.path().join("pi");
        let claude_dir = dir.path().join("claude");
        fs::create_dir_all(&claude_dir).unwrap();

        let results = check_agents(false, &pi_dir, true, &claude_dir);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].key, "agents.pi");
        assert_eq!(results[0].state, CheckState::Skipped);
        assert_eq!(results[1].key, "agents.claude");
        assert_eq!(results[1].state, CheckState::Pass);
    }

    #[test]
    fn check_agents_dir_missing_returns_error() {
        let dir = TempDir::new().unwrap();
        let pi_dir = dir.path().join("pi_missing");
        let claude_dir = dir.path().join("claude_missing");

        let results = check_agents(true, &pi_dir, true, &claude_dir);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].state, CheckState::Error, "agents.pi");
        assert!(
            results[0].detail.contains("not found"),
            "detail: {}",
            results[0].detail
        );
        assert_eq!(results[1].state, CheckState::Error, "agents.claude");
    }

    #[test]
    fn check_agents_dir_exists_reports_file_count() {
        let dir = TempDir::new().unwrap();
        let pi_dir = dir.path().join("pi_sessions");
        let session_subdir = pi_dir.join("session_001");
        fs::create_dir_all(&session_subdir).unwrap();
        fs::write(session_subdir.join("a.jsonl"), b"{}").unwrap();
        fs::write(session_subdir.join("b.jsonl"), b"{}").unwrap();
        // non-jsonl file should not be counted
        fs::write(session_subdir.join("notes.txt"), b"note").unwrap();

        let claude_dir = dir.path().join("claude_sessions");
        fs::create_dir_all(&claude_dir).unwrap();

        let results = check_agents(true, &pi_dir, true, &claude_dir);
        assert_eq!(results[0].state, CheckState::Pass, "agents.pi");
        assert!(
            results[0].detail.contains('2'),
            "detail should report 2 files: {}",
            results[0].detail
        );
        assert_eq!(results[1].state, CheckState::Pass, "agents.claude");
        assert!(
            results[1].detail.contains('0'),
            "detail should report 0 files: {}",
            results[1].detail
        );
    }

    #[test]
    fn check_agents_both_disabled_return_two_skipped() {
        let dir = TempDir::new().unwrap();
        let pi_dir = dir.path().join("pi");
        let claude_dir = dir.path().join("claude");

        let results = check_agents(false, &pi_dir, false, &claude_dir);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].state, CheckState::Skipped);
        assert_eq!(results[1].state, CheckState::Skipped);
    }

    // -----------------------------------------------------------------------
    // check_scheduler
    // -----------------------------------------------------------------------

    fn cron_installed_lines() -> Vec<String> {
        vec![
            "@reboot /bin/chronicle sync --quiet  # chronicle-sync".to_owned(),
            "*/5 * * * * /bin/chronicle sync --quiet  # chronicle-sync".to_owned(),
        ]
    }

    #[test]
    fn check_scheduler_not_installed_lock_free() {
        let dir = TempDir::new().unwrap();
        let lock_path = dir.path().join("chronicle.lock");

        let results = check_scheduler(&[], &lock_path, 300);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].key, "scheduler.cron");
        assert_eq!(results[0].state, CheckState::Warn, "not installed → warn");
        assert_eq!(results[1].key, "scheduler.lock");
        assert_eq!(results[1].state, CheckState::Pass, "lock absent → free");
    }

    #[test]
    fn check_scheduler_installed_lock_free() {
        let dir = TempDir::new().unwrap();
        let lock_path = dir.path().join("chronicle.lock");

        let results = check_scheduler(&cron_installed_lines(), &lock_path, 300);
        assert_eq!(results[0].state, CheckState::Pass, "cron installed");
        assert_eq!(results[1].state, CheckState::Pass, "lock free");
    }

    #[test]
    fn check_scheduler_stale_lock_dead_pid_returns_warn() {
        let dir = TempDir::new().unwrap();
        let lock_path = dir.path().join("chronicle.lock");

        // PID 9999999 is almost certainly dead; timestamp 1 is from epoch.
        fs::write(&lock_path, b"9999999 1").unwrap();

        let results = check_scheduler(&cron_installed_lines(), &lock_path, 300);
        assert_eq!(results[1].key, "scheduler.lock");
        // Dead PID = sync finished; lock is orphaned but harmless — Warn, not Error.
        assert_eq!(
            results[1].state,
            CheckState::Warn,
            "dead PID stale lock → warn"
        );
        assert!(
            results[1].detail.contains("stale"),
            "detail: {}",
            results[1].detail
        );
    }

    #[test]
    fn check_scheduler_live_pid_age_exceeded_returns_error() {
        let dir = TempDir::new().unwrap();
        let lock_path = dir.path().join("chronicle.lock");

        // Current PID (definitely alive) + epoch timestamp (age always exceeded).
        let pid = std::process::id();
        fs::write(&lock_path, format!("{pid} 1").as_bytes()).unwrap();

        let results = check_scheduler(&cron_installed_lines(), &lock_path, 300);
        assert_eq!(results[1].key, "scheduler.lock");
        // Live process held the lock past the timeout — possible hung sync → Error.
        assert_eq!(
            results[1].state,
            CheckState::Error,
            "live PID past timeout → error"
        );
        assert!(
            results[1].detail.contains("hung sync"),
            "detail: {}",
            results[1].detail
        );
    }

    #[test]
    fn check_scheduler_lock_held_by_current_pid_returns_warn() {
        let dir = TempDir::new().unwrap();
        let lock_path = dir.path().join("chronicle.lock");

        // Write the current process PID + current timestamp — live lock.
        let pid = std::process::id();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        fs::write(&lock_path, format!("{pid} {now}").as_bytes()).unwrap();

        let results = check_scheduler(&cron_installed_lines(), &lock_path, 300);
        assert_eq!(results[1].state, CheckState::Warn, "live PID → warn");
        assert!(
            results[1].detail.contains("held by PID"),
            "detail: {}",
            results[1].detail
        );
    }

    // -----------------------------------------------------------------------
    // parse_remote_host_port
    // -----------------------------------------------------------------------

    #[test]
    fn parse_remote_host_port_https_default_port() {
        let (host, port) = parse_remote_host_port("https://github.com/user/repo.git").unwrap();
        assert_eq!(host, "github.com");
        assert_eq!(port, 443);
    }

    #[test]
    fn parse_remote_host_port_ssh_scp_style() {
        let (host, port) = parse_remote_host_port("git@github.com:user/repo.git").unwrap();
        assert_eq!(host, "github.com");
        assert_eq!(port, 22);
    }

    #[test]
    fn parse_remote_host_port_ssh_url_style() {
        let (host, port) = parse_remote_host_port("ssh://git@github.com/user/repo.git").unwrap();
        assert_eq!(host, "github.com");
        assert_eq!(port, 22);
    }

    #[test]
    fn parse_remote_host_port_git_protocol() {
        let (host, port) = parse_remote_host_port("git://github.com/user/repo.git").unwrap();
        assert_eq!(host, "github.com");
        assert_eq!(port, 9418);
    }

    #[test]
    fn parse_remote_host_port_https_with_custom_port() {
        let (host, port) =
            parse_remote_host_port("https://gitlab.example.com:8443/user/repo.git").unwrap();
        assert_eq!(host, "gitlab.example.com");
        assert_eq!(port, 8443);
    }
}
