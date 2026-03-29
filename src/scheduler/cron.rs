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

/// Builds the two Chronicle crontab entries: `@reboot` + the interval entry.
pub fn build_entries(binary_path: &str, cron_expr: &str) -> [String; 2] {
    [
        format!("@reboot {binary_path} sync --quiet  {MARKER}"),
        format!("{cron_expr} {binary_path} sync --quiet  {MARKER}"),
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
/// Prefers the `@reboot` line; falls back to the interval line.
pub fn parse_installed_binary(lines: &[String]) -> Option<String> {
    // "@reboot /path/to/chronicle sync --quiet  # chronicle-sync"
    for line in lines {
        if line.contains(MARKER) && line.starts_with("@reboot ") {
            return line.split_whitespace().nth(1).map(str::to_owned);
        }
    }
    // "*/5 * * * * /path/to/chronicle sync --quiet  # chronicle-sync"
    for line in lines {
        if line.contains(MARKER) && !line.starts_with("@reboot") {
            return line.split_whitespace().nth(5).map(str::to_owned);
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
        assert_eq!(
            entries[0],
            "@reboot /usr/local/bin/chronicle sync --quiet  # chronicle-sync"
        );
        assert_eq!(
            entries[1],
            "*/5 * * * * /usr/local/bin/chronicle sync --quiet  # chronicle-sync"
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
}
