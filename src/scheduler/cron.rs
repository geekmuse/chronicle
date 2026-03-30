//! Crontab management: install, uninstall, and status of Chronicle entries.
//!
//! Chronicle cron entries are identified by the [`MARKER`] comment suffix.
//! All mutating operations (install, uninstall) follow a read–filter–append–write
//! pattern so unrelated crontab lines are never touched.

use std::io::Write as _;
use std::process::{Command, Stdio};

use thiserror::Error;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Comment suffix appended to every Chronicle-managed crontab line.
pub const MARKER: &str = "# chronicle-sync";

/// Standard `sync_interval` values paired with their cron expressions (§10.2).
const SUPPORTED: &[(&str, &str)] = &[
    ("1m", "* * * * *"),
    ("5m", "*/5 * * * *"),
    ("10m", "*/10 * * * *"),
    ("15m", "*/15 * * * *"),
    ("30m", "*/30 * * * *"),
    ("1h", "0 * * * *"),
];

/// Minute values corresponding to each [`SUPPORTED`] entry (same order).
const SUPPORTED_MINUTES: &[u64] = &[1, 5, 10, 15, 30, 60];

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Errors that can occur while managing crontab entries.
#[derive(Debug, Error)]
pub enum SchedulerError {
    /// Failed to spawn or communicate with the `crontab` process.
    #[error("crontab I/O ({context}): {source}")]
    Io {
        context: String,
        #[source]
        source: std::io::Error,
    },

    /// The `crontab` process exited with a non-zero status.
    #[error("crontab command failed: {0}")]
    Command(String),
}

impl SchedulerError {
    fn io(context: impl Into<String>, source: std::io::Error) -> Self {
        Self::Io {
            context: context.into(),
            source,
        }
    }
}

// ---------------------------------------------------------------------------
// Interval mapping (§10.2)
// ---------------------------------------------------------------------------

/// Maps a `sync_interval` config string to a cron expression (§10.2).
///
/// Returns `(cron_expression, optional_warning)`.  Non-standard intervals are
/// rounded down to the nearest supported value; in that case the optional
/// warning string is `Some(…)` — the caller decides how to emit it.
pub fn interval_to_cron(interval: &str) -> (String, Option<String>) {
    // Exact match first.
    for &(name, expr) in SUPPORTED {
        if interval == name {
            return (expr.to_owned(), None);
        }
    }

    // Parse to minutes, clamp to at least 1.
    let minutes = parse_interval_minutes(interval).max(1);

    // Find the largest supported step that is ≤ minutes.
    let mut best_idx = 0usize;
    for (i, &m) in SUPPORTED_MINUTES.iter().enumerate() {
        if m <= minutes {
            best_idx = i;
        }
    }

    let (canonical_name, cron_expr) = SUPPORTED[best_idx];
    let warning = format!(
        "sync_interval '{interval}' is not a standard cron interval; \
         rounding down to '{canonical_name}'"
    );
    (cron_expr.to_owned(), Some(warning))
}

/// Converts a raw cron expression back to a canonical interval name, if known.
pub fn cron_expr_to_interval(expr: &str) -> Option<&'static str> {
    SUPPORTED
        .iter()
        .find(|&&(_, e)| e == expr)
        .map(|&(name, _)| name)
}

fn parse_interval_minutes(interval: &str) -> u64 {
    if let Some(h) = interval.strip_suffix('h') {
        h.parse::<u64>().unwrap_or(1).saturating_mul(60)
    } else if let Some(m) = interval.strip_suffix('m') {
        m.parse::<u64>().unwrap_or(1)
    } else {
        interval.parse::<u64>().unwrap_or(1)
    }
}

// ---------------------------------------------------------------------------
// Jitter
// ---------------------------------------------------------------------------

/// Compute a deterministic jitter (in seconds) for the given machine name.
///
/// The jitter spreads machines uniformly across the sync interval so that
/// machines sharing the same `*/5` (or similar) cron expression don't all
/// hit the remote at the same instant.
///
/// # Arguments
///
/// * `machine_name` — the configured machine identity (e.g. `"cheerful-sparrow"`).
/// * `sync_interval` — the interval string from config (e.g. `"5m"`).
/// * `jitter_config` — the `sync_jitter_secs` config value:
///   - `0` (default): auto — use the full interval as the jitter window.
///   - `> 0`: cap the jitter to this many seconds.
///   - `< 0`: disable jitter entirely (return 0).
///
/// # Returns
///
/// Duration in seconds to sleep before starting the sync cycle.
#[must_use]
pub fn compute_jitter(machine_name: &str, sync_interval: &str, jitter_config: i32) -> u64 {
    if jitter_config < 0 || machine_name.is_empty() {
        return 0;
    }

    let interval_secs = parse_interval_minutes(sync_interval).saturating_mul(60);
    if interval_secs == 0 {
        return 0;
    }

    // Cap: either the configured max or 90% of the interval (leave 10% headroom
    // so the sync itself has time to complete before the next cron fire).
    let window = if jitter_config > 0 {
        (jitter_config as u64).min(interval_secs * 9 / 10)
    } else {
        interval_secs * 9 / 10
    };

    if window == 0 {
        return 0;
    }

    // Simple deterministic hash: FNV-1a on the machine name bytes.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in machine_name.as_bytes() {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }

    hash % window
}

// ---------------------------------------------------------------------------
// Pure / testable logic
// ---------------------------------------------------------------------------

/// Removes all lines containing [`MARKER`] from a crontab line list.
pub fn filter_marker_lines(lines: &[String]) -> Vec<String> {
    lines
        .iter()
        .filter(|l| !l.contains(MARKER))
        .cloned()
        .collect()
}

/// Shell snippet that discovers `SSH_AUTH_SOCK` at runtime so that the
/// `ssh_key_from_agent()` credential callback can reach the user's SSH agent
/// from within a cron job (which runs in a minimal environment).
///
/// - **macOS:** Discovers the launchd-managed SSH agent socket by scanning
///   `/private/tmp/com.apple.launchd.*/Listeners` for a socket owned by the
///   current user.  Neither `launchctl getenv` nor `launchctl asuser` work
///   from cron because cron runs in the system bootstrap domain, which cannot
///   query the user's GUI session domain.
/// - **Linux:** Falls back to the well-known systemd user socket at
///   `/run/user/<uid>/ssh-agent.socket` when `SSH_AUTH_SOCK` is unset.
#[cfg(target_os = "macos")]
const SSH_AGENT_ENV: &str = "SSH_AUTH_SOCK=$(find /private/tmp/com.apple.launchd.* -name Listeners -user $(whoami) -type s 2>/dev/null | head -1) ";

#[cfg(not(target_os = "macos"))]
const SSH_AGENT_ENV: &str =
    r#"SSH_AUTH_SOCK="${SSH_AUTH_SOCK:-/run/user/$(id -u)/ssh-agent.socket}" "#;

/// Builds the two Chronicle crontab entries: `@reboot` + the interval entry.
///
/// Each command is prefixed with [`SSH_AGENT_ENV`] so that `git2`'s
/// `ssh_key_from_agent()` credential callback can reach the user's SSH agent
/// even in the minimal cron environment.
pub fn build_entries(binary_path: &str, cron_expr: &str) -> [String; 2] {
    [
        format!("@reboot {SSH_AGENT_ENV}{binary_path} sync --quiet  {MARKER}"),
        format!("{cron_expr} {SSH_AGENT_ENV}{binary_path} sync --quiet  {MARKER}"),
    ]
}

/// Pure core of `install`: filters existing marker lines then appends new entries.
pub fn apply_install(existing: &[String], binary_path: &str, cron_expr: &str) -> Vec<String> {
    let mut lines = filter_marker_lines(existing);
    lines.extend(build_entries(binary_path, cron_expr));
    lines
}

/// Pure core of `uninstall`: removes all marker lines from the crontab.
pub fn apply_uninstall(existing: &[String]) -> Vec<String> {
    filter_marker_lines(existing)
}

/// Extracts the chronicle binary path from installed crontab lines.
///
/// The binary path is the token immediately before `"sync"` in the command.
/// This is position-independent, so it works regardless of whether an
/// `SSH_AUTH_SOCK=…` prefix is present.
///
/// Prefers the `@reboot` line; falls back to the interval line.
pub fn parse_installed_binary(lines: &[String]) -> Option<String> {
    // Helper: find the token before "sync" in a marker line.
    let extract = |line: &str| -> Option<String> {
        let parts: Vec<&str> = line.split_whitespace().collect();
        parts
            .iter()
            .position(|&t| t == "sync")
            .and_then(|i| i.checked_sub(1))
            .map(|i| parts[i].to_owned())
    };

    for line in lines {
        if line.contains(MARKER) && line.starts_with("@reboot ") {
            return extract(line);
        }
    }
    for line in lines {
        if line.contains(MARKER) && !line.starts_with("@reboot") {
            return extract(line);
        }
    }
    None
}

/// Extracts the cron expression from the installed interval line.
///
/// The cron expression occupies the first five whitespace-separated tokens.
pub fn parse_installed_cron_expr(lines: &[String]) -> Option<String> {
    for line in lines {
        if line.contains(MARKER) && !line.starts_with("@reboot") {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 5 {
                return Some(format!(
                    "{} {} {} {} {}",
                    parts[0], parts[1], parts[2], parts[3], parts[4]
                ));
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Status type
// ---------------------------------------------------------------------------

/// Current state of Chronicle's crontab entries.
pub struct ScheduleStatus {
    /// Whether any Chronicle-managed entries are present in the crontab.
    pub installed: bool,
    /// Canonical interval name (e.g., `"5m"`) derived from the installed expression.
    pub interval: Option<String>,
    /// Raw cron expression from the installed entries.
    pub cron_expression: Option<String>,
    /// Absolute path to the `chronicle` binary as written in the crontab.
    pub binary_path: Option<String>,
}

/// Builds a [`ScheduleStatus`] from a set of crontab lines (pure, no I/O).
pub fn parse_status(lines: &[String]) -> ScheduleStatus {
    let installed = lines.iter().any(|l| l.contains(MARKER));
    let binary_path = parse_installed_binary(lines);
    let cron_expression = parse_installed_cron_expr(lines);
    let interval = cron_expression
        .as_deref()
        .and_then(cron_expr_to_interval)
        .map(str::to_owned);
    ScheduleStatus {
        installed,
        interval,
        cron_expression,
        binary_path,
    }
}

// ---------------------------------------------------------------------------
// Crontab I/O
// ---------------------------------------------------------------------------

/// Reads the current user crontab.
///
/// Returns an empty vec when the user has no crontab (the "no crontab for …"
/// diagnostic is treated as a normal empty-crontab state, not an error).
pub fn crontab_read() -> Result<Vec<String>, SchedulerError> {
    let output = Command::new("crontab")
        .arg("-l")
        .output()
        .map_err(|e| SchedulerError::io("crontab -l", e))?;

    if output.status.success() {
        let content = String::from_utf8_lossy(&output.stdout);
        return Ok(content.lines().map(str::to_owned).collect());
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    // Both macOS and Linux emit a "no crontab" diagnostic to stderr when the
    // user has no crontab — treat this as an empty crontab, not an error.
    if stderr.contains("no crontab") {
        return Ok(Vec::new());
    }

    Err(SchedulerError::Command(format!(
        "crontab -l: {}",
        stderr.trim()
    )))
}

/// Writes lines to the user's crontab.
///
/// If `lines` is empty the crontab is deleted via `crontab -r`; otherwise
/// the lines are piped to `crontab -`.
pub fn crontab_write(lines: &[String]) -> Result<(), SchedulerError> {
    if lines.is_empty() {
        let output = Command::new("crontab")
            .arg("-r")
            .output()
            .map_err(|e| SchedulerError::io("crontab -r", e))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            // "no crontab for user" on -r means it was already gone — acceptable.
            if !stderr.contains("no crontab") {
                return Err(SchedulerError::Command(format!(
                    "crontab -r: {}",
                    stderr.trim()
                )));
            }
        }
        return Ok(());
    }

    let content = lines.join("\n") + "\n";
    let mut child = Command::new("crontab")
        .arg("-")
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|e| SchedulerError::io("crontab -", e))?;

    // Write then drop stdin to send EOF before waiting.
    {
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| SchedulerError::Command("crontab stdin unavailable".into()))?;
        stdin
            .write_all(content.as_bytes())
            .map_err(|e| SchedulerError::io("crontab stdin write", e))?;
    }

    let status = child
        .wait()
        .map_err(|e| SchedulerError::io("crontab - wait", e))?;

    if !status.success() {
        return Err(SchedulerError::Command("crontab - failed".into()));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// High-level operations
// ---------------------------------------------------------------------------

/// Installs Chronicle crontab entries for the given binary path and cron expression.
///
/// Existing Chronicle entries (identified by [`MARKER`]) are replaced in place
/// so re-running install does not create duplicate entries.
pub fn install(binary_path: &str, cron_expr: &str) -> Result<(), SchedulerError> {
    let existing = crontab_read()?;
    let updated = apply_install(&existing, binary_path, cron_expr);
    crontab_write(&updated)
}

/// Removes all Chronicle crontab entries.
///
/// If the crontab is empty after removal, the crontab file is deleted via
/// `crontab -r`.
pub fn uninstall() -> Result<(), SchedulerError> {
    let existing = crontab_read()?;
    let updated = apply_uninstall(&existing);
    crontab_write(&updated)
}

/// Returns the current Chronicle crontab status (no side-effects).
pub fn status() -> Result<ScheduleStatus, SchedulerError> {
    let lines = crontab_read()?;
    Ok(parse_status(&lines))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &str) -> String {
        v.to_owned()
    }

    // ── interval_to_cron ────────────────────────────────────────────────────

    #[test]
    fn interval_1m_exact() {
        let (expr, warn) = interval_to_cron("1m");
        assert_eq!(expr, "* * * * *");
        assert!(warn.is_none());
    }

    #[test]
    fn interval_5m_exact() {
        let (expr, warn) = interval_to_cron("5m");
        assert_eq!(expr, "*/5 * * * *");
        assert!(warn.is_none());
    }

    #[test]
    fn interval_10m_exact() {
        let (expr, warn) = interval_to_cron("10m");
        assert_eq!(expr, "*/10 * * * *");
        assert!(warn.is_none());
    }

    #[test]
    fn interval_15m_exact() {
        let (expr, warn) = interval_to_cron("15m");
        assert_eq!(expr, "*/15 * * * *");
        assert!(warn.is_none());
    }

    #[test]
    fn interval_30m_exact() {
        let (expr, warn) = interval_to_cron("30m");
        assert_eq!(expr, "*/30 * * * *");
        assert!(warn.is_none());
    }

    #[test]
    fn interval_1h_exact() {
        let (expr, warn) = interval_to_cron("1h");
        assert_eq!(expr, "0 * * * *");
        assert!(warn.is_none());
    }

    #[test]
    fn interval_7m_rounds_down_to_5m() {
        let (expr, warn) = interval_to_cron("7m");
        assert_eq!(expr, "*/5 * * * *");
        let w = warn.expect("expected a warning for non-standard interval");
        assert!(w.contains("7m"), "warning should mention '7m': {w}");
        assert!(
            w.contains("5m"),
            "warning should mention rounded-down '5m': {w}"
        );
    }

    #[test]
    fn interval_45m_rounds_down_to_30m() {
        let (expr, warn) = interval_to_cron("45m");
        assert_eq!(expr, "*/30 * * * *");
        assert!(warn.is_some());
    }

    #[test]
    fn interval_2h_rounds_down_to_1h() {
        let (expr, warn) = interval_to_cron("2h");
        assert_eq!(expr, "0 * * * *");
        assert!(warn.is_some());
    }

    // ── cron_expr_to_interval ───────────────────────────────────────────────

    #[test]
    fn cron_expr_known_values_map_correctly() {
        assert_eq!(cron_expr_to_interval("* * * * *"), Some("1m"));
        assert_eq!(cron_expr_to_interval("*/5 * * * *"), Some("5m"));
        assert_eq!(cron_expr_to_interval("*/10 * * * *"), Some("10m"));
        assert_eq!(cron_expr_to_interval("*/15 * * * *"), Some("15m"));
        assert_eq!(cron_expr_to_interval("*/30 * * * *"), Some("30m"));
        assert_eq!(cron_expr_to_interval("0 * * * *"), Some("1h"));
    }

    #[test]
    fn cron_expr_unknown_returns_none() {
        assert_eq!(cron_expr_to_interval("*/7 * * * *"), None);
    }

    // ── filter_marker_lines ─────────────────────────────────────────────────

    #[test]
    fn filter_removes_marker_lines() {
        let lines = vec![
            s("0 * * * * some_other_cmd"),
            s("*/5 * * * * /bin/chronicle sync --quiet  # chronicle-sync"),
            s("@reboot /bin/chronicle sync --quiet  # chronicle-sync"),
            s("30 4 * * * cleanup.sh"),
        ];
        let result = filter_marker_lines(&lines);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0], "0 * * * * some_other_cmd");
        assert_eq!(result[1], "30 4 * * * cleanup.sh");
    }

    #[test]
    fn filter_keeps_unrelated_lines() {
        let lines = vec![s("0 * * * * backup.sh")];
        assert_eq!(filter_marker_lines(&lines), lines);
    }

    // ── build_entries ───────────────────────────────────────────────────────

    #[test]
    fn build_entries_correct_format() {
        let entries = build_entries("/usr/local/bin/chronicle", "*/5 * * * *");
        assert!(
            entries[0].starts_with("@reboot "),
            "reboot entry must start with @reboot"
        );
        assert!(
            entries[0].contains("/usr/local/bin/chronicle sync --quiet"),
            "reboot entry must contain binary and args"
        );
        assert!(
            entries[0].ends_with(MARKER),
            "reboot entry must end with marker"
        );
        assert!(
            entries[1].starts_with("*/5 * * * *"),
            "interval entry must start with cron expression"
        );
        assert!(
            entries[1].contains("/usr/local/bin/chronicle sync --quiet"),
            "interval entry must contain binary and args"
        );
        assert!(
            entries[1].ends_with(MARKER),
            "interval entry must end with marker"
        );
        // Both entries must include the SSH_AUTH_SOCK env snippet.
        assert!(
            entries[0].contains("SSH_AUTH_SOCK"),
            "reboot entry must propagate SSH_AUTH_SOCK"
        );
        assert!(
            entries[1].contains("SSH_AUTH_SOCK"),
            "interval entry must propagate SSH_AUTH_SOCK"
        );
    }

    // ── apply_install ───────────────────────────────────────────────────────

    #[test]
    fn apply_install_empty_crontab_gets_two_entries() {
        let result = apply_install(&[], "/bin/chronicle", "*/5 * * * *");
        assert_eq!(result.len(), 2);
        assert!(result[0].starts_with("@reboot"));
        assert!(result[1].starts_with("*/5"));
    }

    #[test]
    fn apply_install_preserves_unrelated_entries() {
        let existing = vec![s("0 3 * * * backup.sh"), s("30 4 * * * cleanup.sh")];
        let result = apply_install(&existing, "/bin/chronicle", "*/10 * * * *");
        assert_eq!(result.len(), 4);
        assert_eq!(result[0], "0 3 * * * backup.sh");
        assert_eq!(result[1], "30 4 * * * cleanup.sh");
    }

    #[test]
    fn apply_install_replaces_existing_chronicle_entries() {
        let existing = vec![
            s("@reboot /old/chronicle sync --quiet  # chronicle-sync"),
            s("*/5 * * * * /old/chronicle sync --quiet  # chronicle-sync"),
            s("0 3 * * * backup.sh"),
        ];
        let result = apply_install(&existing, "/new/chronicle", "*/15 * * * *");
        // Unrelated entry first, then two new Chronicle entries appended.
        assert_eq!(result.len(), 3);
        assert_eq!(result[0], "0 3 * * * backup.sh");
        assert!(result[1].contains("/new/chronicle") && result[1].starts_with("@reboot"));
        assert!(result[2].contains("/new/chronicle") && result[2].contains("*/15 * * * *"));
    }

    // ── apply_uninstall ─────────────────────────────────────────────────────

    #[test]
    fn apply_uninstall_removes_chronicle_entries() {
        let existing = vec![
            s("@reboot /bin/chronicle sync --quiet  # chronicle-sync"),
            s("*/5 * * * * /bin/chronicle sync --quiet  # chronicle-sync"),
            s("0 3 * * * backup.sh"),
        ];
        let result = apply_uninstall(&existing);
        assert_eq!(result, vec![s("0 3 * * * backup.sh")]);
    }

    #[test]
    fn apply_uninstall_empty_when_only_chronicle_entries() {
        let existing = vec![
            s("@reboot /bin/chronicle sync --quiet  # chronicle-sync"),
            s("*/5 * * * * /bin/chronicle sync --quiet  # chronicle-sync"),
        ];
        assert!(apply_uninstall(&existing).is_empty());
    }

    // ── parse_installed_binary ──────────────────────────────────────────────

    #[test]
    fn parse_binary_prefers_reboot_line() {
        let lines = vec![
            s("@reboot /usr/local/bin/chronicle sync --quiet  # chronicle-sync"),
            s("*/5 * * * * /usr/local/bin/chronicle sync --quiet  # chronicle-sync"),
        ];
        assert_eq!(
            parse_installed_binary(&lines),
            Some("/usr/local/bin/chronicle".to_owned())
        );
    }

    #[test]
    fn parse_binary_fallback_to_interval_line() {
        let lines = vec![s(
            "*/5 * * * * /usr/local/bin/chronicle sync --quiet  # chronicle-sync",
        )];
        assert_eq!(
            parse_installed_binary(&lines),
            Some("/usr/local/bin/chronicle".to_owned())
        );
    }

    #[test]
    fn parse_binary_none_when_no_entries() {
        let lines = vec![s("0 3 * * * backup.sh")];
        assert!(parse_installed_binary(&lines).is_none());
    }

    // ── parse_installed_cron_expr ───────────────────────────────────────────

    #[test]
    fn parse_cron_expr_from_interval_line() {
        let lines = vec![
            s("@reboot /bin/chronicle sync --quiet  # chronicle-sync"),
            s("*/5 * * * * /bin/chronicle sync --quiet  # chronicle-sync"),
        ];
        assert_eq!(
            parse_installed_cron_expr(&lines),
            Some("*/5 * * * *".to_owned())
        );
    }

    #[test]
    fn parse_cron_expr_1h_format() {
        let lines = vec![
            s("@reboot /bin/chronicle sync --quiet  # chronicle-sync"),
            s("0 * * * * /bin/chronicle sync --quiet  # chronicle-sync"),
        ];
        assert_eq!(
            parse_installed_cron_expr(&lines),
            Some("0 * * * *".to_owned())
        );
    }

    // ── parse_status ────────────────────────────────────────────────────────

    #[test]
    fn parse_status_installed() {
        let lines = vec![
            s("@reboot /bin/chronicle sync --quiet  # chronicle-sync"),
            s("*/5 * * * * /bin/chronicle sync --quiet  # chronicle-sync"),
        ];
        let st = parse_status(&lines);
        assert!(st.installed);
        assert_eq!(st.interval.as_deref(), Some("5m"));
        assert_eq!(st.cron_expression.as_deref(), Some("*/5 * * * *"));
        assert_eq!(st.binary_path.as_deref(), Some("/bin/chronicle"));
    }

    #[test]
    fn parse_status_not_installed() {
        let lines = vec![s("0 3 * * * backup.sh")];
        let st = parse_status(&lines);
        assert!(!st.installed);
        assert!(st.interval.is_none());
        assert!(st.binary_path.is_none());
    }

    #[test]
    fn parse_status_empty_crontab() {
        let st = parse_status(&[]);
        assert!(!st.installed);
    }

    // ── compute_jitter ──────────────────────────────────────────────────────

    #[test]
    fn jitter_disabled_when_config_negative() {
        assert_eq!(compute_jitter("cheerful-sparrow", "5m", -1), 0);
    }

    #[test]
    fn jitter_zero_for_empty_machine_name() {
        assert_eq!(compute_jitter("", "5m", 0), 0);
    }

    #[test]
    fn jitter_auto_within_90_percent_of_interval() {
        let j = compute_jitter("cheerful-sparrow", "5m", 0);
        // 5m = 300s, 90% = 270s
        assert!(j < 270, "jitter {j} must be < 270");
    }

    #[test]
    fn jitter_capped_by_config_value() {
        let j = compute_jitter("cheerful-sparrow", "5m", 30);
        assert!(j < 30, "jitter {j} must be < 30");
    }

    #[test]
    fn jitter_deterministic_for_same_machine() {
        let a = compute_jitter("cheerful-sparrow", "5m", 0);
        let b = compute_jitter("cheerful-sparrow", "5m", 0);
        assert_eq!(a, b, "jitter must be deterministic");
    }

    #[test]
    fn jitter_differs_across_machines() {
        let a = compute_jitter("cheerful-sparrow", "5m", 0);
        let b = compute_jitter("grumpy-walrus", "5m", 0);
        // Could theoretically collide, but with FNV-1a and these names it won't.
        assert_ne!(a, b, "different machines should get different jitter");
    }

    #[test]
    fn jitter_cap_does_not_exceed_interval() {
        // Config says 9999s but interval is only 60s — cap to 90% of 60 = 54.
        let j = compute_jitter("test-machine", "1m", 9999);
        assert!(j < 54, "jitter {j} must be < 54 (90% of 60s)");
    }
}
