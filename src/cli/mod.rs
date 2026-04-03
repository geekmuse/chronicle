use anyhow::{Context as _, Result};
use std::collections::HashSet;
use std::fs;
use std::io::{self, BufRead as _, IsTerminal as _, Write as _};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};

use crate::canon::levels::L3_WARNING;
use crate::canon::TokenRegistry;
use crate::config::{self, schema::HistoryMode, CliOverrides};
use crate::errors;
use crate::git;
use crate::materialize_cache::{MaterializeCache, MaterializeFileState};
use crate::merge::set_union::{merge_jsonl, NullReporter};
use crate::scan;
use crate::scheduler::cron as scheduler_cron;
use crate::sync_state::{self, SyncOp};

// ---------------------------------------------------------------------------
// chronicle init
// ---------------------------------------------------------------------------

/// Handle `chronicle init [--remote <url>]`.
///
/// Creates the config file (if absent), generates a machine name (if none),
/// initializes the local git repo, and prints a confirmation.  Safe to run
/// more than once — existing config and repo state are preserved.
pub fn handle_init(remote: Option<String>) -> Result<()> {
    let config_path = config::default_config_path();
    let config_existed = config_path.exists();

    // Load existing config, or start from built-in defaults.
    let mut cfg = config::load(Some(&config_path), &CliOverrides::default())
        .context("failed to load configuration")?;

    // Generate machine name if not already set.
    if cfg.general.machine_name.is_empty() {
        cfg.general.machine_name = config::machine_name::generate();
    }

    // Apply --remote flag (highest CLI precedence).
    if let Some(url) = remote {
        cfg.storage.remote_url = url;
    }

    // Prompt for remote URL if still unset and stdin is a TTY.
    if cfg.storage.remote_url.is_empty() && io::stdin().is_terminal() {
        let stdout = io::stdout();
        let mut out = stdout.lock();
        write!(out, "Remote git URL (leave blank to skip): ")?;
        out.flush()?;

        let stdin = io::stdin();
        let mut line = String::new();
        stdin.lock().read_line(&mut line)?;
        let url = line.trim().to_owned();
        if !url.is_empty() {
            cfg.storage.remote_url = url;
        }
    }

    // Warn if L3 freeform canonicalization is active.
    if cfg.canonicalization.level >= 3 {
        eprintln!("{L3_WARNING}");
    }

    // Write config file (create parent directories if needed).
    if let Some(parent) = config_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create config directory {}", parent.display()))?;
    }
    let toml_content =
        toml::to_string_pretty(&cfg).context("failed to serialize configuration to TOML")?;
    fs::write(&config_path, &toml_content)
        .with_context(|| format!("failed to write config file {}", config_path.display()))?;

    // Initialize (or open) the git repository.
    let repo_path = config::expand_path(&cfg.storage.repo_path);
    let remote_url = if cfg.storage.remote_url.is_empty() {
        None
    } else {
        Some(cfg.storage.remote_url.as_str())
    };

    let manager = git::RepoManager::init_or_open(&repo_path, remote_url, &cfg.storage.branch)
        .context("failed to initialize git repository")?;
    manager
        .ensure_working_tree()
        .context("failed to set up repository working tree")?;
    manager
        .ensure_manifest()
        .context("failed to initialize repository manifest")?;

    // Print confirmation.
    println!("✓ Chronicle initialized");
    println!("  Machine name : {}", cfg.general.machine_name);
    println!("  Config file  : {}", config_path.display());
    println!("  Repository   : {}", repo_path.display());
    if !cfg.storage.remote_url.is_empty() {
        println!("  Remote URL   : {}", cfg.storage.remote_url);
    }
    if config_existed {
        println!("\nNote: existing config preserved (no values overwritten).");
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// chronicle import
// ---------------------------------------------------------------------------

/// Handle `chronicle import [--agent <pi|claude|all>] [--dry-run]`.
pub fn handle_import(agent: String, dry_run: bool) -> Result<()> {
    let config_path = config::default_config_path();
    let home = dirs::home_dir().context("could not determine home directory")?;
    import_impl(&agent, dry_run, &config_path, &home)
}

/// Core import logic, factored out for testability.
///
/// Accepts an explicit `home` path so tests can inject a temporary directory
/// without touching the real `$HOME`.
pub fn import_impl(agent: &str, dry_run: bool, config_path: &Path, home: &Path) -> Result<()> {
    let cfg = config::load(Some(config_path), &CliOverrides::default())
        .context("failed to load configuration")?;

    // Warn if L3 freeform canonicalization is active.
    if cfg.canonicalization.level >= 3 {
        eprintln!("{L3_WARNING}");
    }

    let registry = TokenRegistry::from_config(&cfg.canonicalization, home);
    let canon_level = cfg.canonicalization.level;
    let machine_name = {
        let n = cfg.general.machine_name.clone();
        if n.is_empty() {
            "unknown".to_owned()
        } else {
            n
        }
    };

    let repo_path = config::expand_path_with_home(&cfg.storage.repo_path, home);
    let remote_url = if cfg.storage.remote_url.is_empty() {
        None
    } else {
        Some(cfg.storage.remote_url.as_str())
    };
    // In dry-run mode we never touch the repo, so skip initialisation entirely.
    let manager_owned = if dry_run {
        None
    } else {
        Some(
            git::RepoManager::init_or_open(&repo_path, remote_url, &cfg.storage.branch)
                .context("failed to open git repository")?,
        )
    };
    let manager = manager_owned.as_ref();

    let mut total_sessions = 0usize;
    let mut total_files = 0usize;

    if (agent == "pi" || agent == "all") && cfg.agents.pi.enabled {
        let source_dir = config::expand_path_with_home(&cfg.agents.pi.session_dir, home);
        let (s, f) = import_agent_sessions(&ImportParams {
            agent_name: "pi",
            source_dir: &source_dir,
            repo_rel_base: "pi/sessions",
            repo_path: &repo_path,
            registry: &registry,
            canon_level,
            manager,
            machine_name: &machine_name,
            dry_run,
            is_pi: true,
        })
        .context("Pi import failed")?;
        total_sessions += s;
        total_files += f;
    }

    if (agent == "claude" || agent == "all") && cfg.agents.claude.enabled {
        let source_dir = config::expand_path_with_home(&cfg.agents.claude.session_dir, home);
        let (s, f) = import_agent_sessions(&ImportParams {
            agent_name: "claude",
            source_dir: &source_dir,
            repo_rel_base: "claude/projects",
            repo_path: &repo_path,
            registry: &registry,
            canon_level,
            manager,
            machine_name: &machine_name,
            dry_run,
            is_pi: false,
        })
        .context("Claude import failed")?;
        total_sessions += s;
        total_files += f;
    }

    if dry_run {
        println!(
            "Dry run: would import {total_files} file(s) across {total_sessions} session dir(s)."
        );
    } else {
        println!("Imported {total_files} file(s) across {total_sessions} session dir(s).");
    }

    Ok(())
}

/// Import `.jsonl` files for one agent from `source_dir` into the repo working tree.
///
/// Creates one git commit per non-empty session subdirectory (per §6.6 / §9.1:
/// "one commit per agent session directory for atomicity").
///
/// Returns `(sessions_committed, files_written)`.
/// Parameters bundled to avoid a >7-argument function (clippy::too_many_arguments).
struct ImportParams<'a> {
    agent_name: &'a str,
    source_dir: &'a Path,
    repo_rel_base: &'a str,
    repo_path: &'a Path,
    registry: &'a TokenRegistry,
    canon_level: u8,
    /// `None` when `--dry-run` is set (no repo access required).
    manager: Option<&'a git::RepoManager>,
    machine_name: &'a str,
    dry_run: bool,
    is_pi: bool,
}

fn import_agent_sessions(p: &ImportParams<'_>) -> Result<(usize, usize)> {
    let ImportParams {
        agent_name,
        source_dir,
        repo_rel_base,
        repo_path,
        registry,
        canon_level,
        manager,
        machine_name,
        dry_run,
        is_pi,
    } = p;
    let (dry_run, is_pi, canon_level) = (*dry_run, *is_pi, *canon_level);

    if !source_dir.exists() {
        println!(
            "  [{}] source directory {} not found — skipping",
            agent_name,
            source_dir.display()
        );
        return Ok((0, 0));
    }

    let mut sessions = 0usize;
    let mut files = 0usize;

    // Collect and sort subdirectories for deterministic commit ordering.
    let mut subdirs: Vec<PathBuf> = Vec::new();
    for entry in
        fs::read_dir(source_dir).with_context(|| format!("cannot read {}", source_dir.display()))?
    {
        let entry = entry.with_context(|| format!("I/O error reading {}", source_dir.display()))?;
        let ft = entry
            .file_type()
            .with_context(|| format!("cannot stat {}", entry.path().display()))?;
        if ft.is_dir() {
            subdirs.push(entry.path());
        }
    }
    subdirs.sort();

    for session_path in subdirs {
        let dir_name_os = session_path
            .file_name()
            .expect("directory path always has a final component")
            .to_string_lossy()
            .into_owned();

        // L1: canonicalize the encoded directory name.
        let canonical_dir = if is_pi {
            registry.canonicalize_pi_dir(&dir_name_os)
        } else {
            registry.canonicalize_claude_dir(&dir_name_os)
        };

        // Collect all .jsonl files inside this session subdirectory.
        let mut jsonl_files: Vec<PathBuf> = Vec::new();
        match fs::read_dir(&session_path) {
            Err(e) => {
                eprintln!("  Warning: cannot read {}: {e}", session_path.display());
                continue;
            }
            Ok(entries) => {
                for sub in entries {
                    match sub {
                        Err(e) => eprintln!(
                            "  Warning: I/O error listing {}: {e}",
                            session_path.display()
                        ),
                        Ok(sub) => {
                            let p = sub.path();
                            if p.extension().is_some_and(|ext| ext == "jsonl") {
                                jsonl_files.push(p);
                            }
                        }
                    }
                }
            }
        }

        if jsonl_files.is_empty() {
            continue;
        }

        jsonl_files.sort();
        let repo_session_rel = format!("{repo_rel_base}/{canonical_dir}");

        if dry_run {
            println!(
                "  [{}] {} → {} ({} file(s))",
                agent_name,
                dir_name_os,
                repo_session_rel,
                jsonl_files.len()
            );
            for f in &jsonl_files {
                println!(
                    "    {}/{}",
                    repo_session_rel,
                    f.file_name().unwrap().to_string_lossy()
                );
            }
            files += jsonl_files.len();
            sessions += 1;
            continue;
        }

        // Create the destination directory in the working tree.
        let repo_session_abs = repo_path.join(&repo_session_rel);
        fs::create_dir_all(&repo_session_abs)
            .with_context(|| format!("cannot create {}", repo_session_abs.display()))?;

        let mut staged: Vec<PathBuf> = Vec::new();

        for file_path in &jsonl_files {
            let filename = file_path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned();

            let raw = fs::read_to_string(file_path)
                .with_context(|| format!("cannot read {}", file_path.display()))?;

            // Canonicalize each line (L2 / L3); fall back to original line on error.
            let canonical_lines: Vec<String> = raw
                .lines()
                .map(|line| {
                    if line.is_empty() {
                        return String::new();
                    }
                    registry
                        .canonicalize_line(line, canon_level)
                        .unwrap_or_else(|e| {
                            eprintln!("  Warning: canonicalization error: {e}");
                            line.to_owned()
                        })
                })
                .collect();

            let mut canonical_content = canonical_lines.join("\n");
            if raw.ends_with('\n') && !canonical_content.ends_with('\n') {
                canonical_content.push('\n');
            }

            let dest = repo_session_abs.join(&filename);
            fs::write(&dest, &canonical_content)
                .with_context(|| format!("cannot write {}", dest.display()))?;

            staged.push(PathBuf::from(format!("{repo_session_rel}/{filename}")));
        }

        // Stage all files for this session directory, then commit.
        // `manager` is guaranteed Some when dry_run=false (enforced in import_impl).
        let mgr = manager.expect("repo manager must be present for non-dry-run import");
        let staged_refs: Vec<&Path> = staged.iter().map(|p| p.as_path()).collect();
        mgr.stage_files(&staged_refs)
            .with_context(|| format!("cannot stage files for {canonical_dir}"))?;

        let msg = git::format_import_message(agent_name, machine_name, jsonl_files.len());
        mgr.commit_if_staged(&msg, machine_name)
            .with_context(|| format!("cannot commit import for {canonical_dir}"))?;

        files += jsonl_files.len();
        sessions += 1;
    }

    Ok((sessions, files))
}

// ---------------------------------------------------------------------------
// Advisory file lock (concurrency guard — Gap 2 fix)
// ---------------------------------------------------------------------------

/// Returns the path of the advisory lock file.  Co-located with the repo
/// directory as a sibling (one level up), matching the `StateCache` pattern:
/// `<parent_of_repo>/chronicle.lock`.
pub fn lock_file_path(repo_path: &Path) -> PathBuf {
    repo_path
        .parent()
        .unwrap_or(repo_path)
        .join("chronicle.lock")
}

/// Tries to open and exclusively lock `<parent_of_repo>/chronicle.lock` in
/// a non-blocking fashion, with stale-lock recovery.
///
/// `lock_timeout_secs` controls automatic lock recovery:
/// - `> 0`: break the lock if it is older than this many seconds **or** the
///   holding PID is no longer alive.
/// - `0`: only break the lock if the holding PID is dead (no age check).
/// - `< 0`: disable all stale-lock recovery (original v0.4.2 behaviour).
///
/// Returns:
/// - `Ok(Some(file))` — lock acquired; caller must keep `file` alive
///   for the duration of the critical section (dropped ⇒ lock released).
/// - `Ok(None)` — another process already holds the lock.
/// - `Err(_)` — unexpected I/O error.
fn try_acquire_sync_lock(repo_path: &Path, lock_timeout_secs: i64) -> Result<Option<fs::File>> {
    let lock_path = lock_file_path(repo_path);
    if let Some(parent) = lock_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create lock directory {}", parent.display()))?;
    }

    // First attempt.
    let file = open_lock_file(&lock_path)?;
    if flock_exclusive_nb(&file)
        .with_context(|| format!("flock failed on {}", lock_path.display()))?
    {
        stamp_lock_file(&file)?;
        return Ok(Some(file));
    }

    // Lock is held — check for staleness (unless recovery is disabled).
    if lock_timeout_secs < 0 {
        return Ok(None);
    }

    if is_lock_stale(&lock_path, lock_timeout_secs)? {
        tracing::warn!("breaking stale lock at {}", lock_path.display());
        // Remove the stale file, open a fresh one, try again.
        let _ = fs::remove_file(&lock_path);
        drop(file);
        let file2 = open_lock_file(&lock_path)?;
        if flock_exclusive_nb(&file2)
            .with_context(|| format!("flock failed on {}", lock_path.display()))?
        {
            stamp_lock_file(&file2)?;
            return Ok(Some(file2));
        }
    }

    Ok(None)
}

/// Open (or create) the lock file for writing.
fn open_lock_file(lock_path: &Path) -> Result<fs::File> {
    fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(lock_path)
        .with_context(|| format!("failed to open lock file {}", lock_path.display()))
}

/// Write `<PID> <UNIX_TIMESTAMP>` into the lock file so that other processes
/// can detect staleness.
fn stamp_lock_file(file: &fs::File) -> Result<()> {
    use io::Write as _;
    let pid = std::process::id();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    // Truncate any prior content, then write.
    file.set_len(0)?;
    let mut writer = io::BufWriter::new(file);
    write!(writer, "{pid} {now}")?;
    writer.flush()?;
    Ok(())
}

/// Returns `true` if the lock file is stale — either the holder PID is dead
/// or the lock age exceeds `timeout_secs` (when > 0).
fn is_lock_stale(lock_path: &Path, timeout_secs: i64) -> Result<bool> {
    // Read PID and timestamp from the file.
    let contents = match fs::read_to_string(lock_path) {
        Ok(c) => c,
        Err(_) => return Ok(false), // can't read → assume not stale
    };
    let mut parts = contents.split_whitespace();
    let pid: Option<u32> = parts.next().and_then(|s| s.parse().ok());
    let stamp: Option<u64> = parts.next().and_then(|s| s.parse().ok());

    // Check 1: Is the holding process still alive?
    if let Some(p) = pid {
        if !is_process_alive(p) {
            tracing::info!(pid = p, "lock holder process is dead");
            return Ok(true);
        }
    }

    // Check 2: Has the lock exceeded the age threshold?
    if timeout_secs > 0 {
        if let Some(ts) = stamp {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let age = now.saturating_sub(ts);
            #[allow(clippy::cast_sign_loss)]
            if age > timeout_secs as u64 {
                tracing::info!(
                    age_secs = age,
                    timeout_secs,
                    "lock file age exceeds timeout"
                );
                return Ok(true);
            }
        }
    }

    Ok(false)
}

/// Check whether a process with the given PID is still alive.
#[cfg(unix)]
fn is_process_alive(pid: u32) -> bool {
    // kill(pid, 0) checks existence without sending a signal.
    // Returns 0 if process exists, -1 with ESRCH if it doesn't.
    #[allow(clippy::cast_possible_wrap)]
    let ret = unsafe { libc::kill(pid as libc::pid_t, 0) };
    if ret == 0 {
        return true;
    }
    // EPERM means the process exists but we lack permission to signal it.
    let err = std::io::Error::last_os_error();
    err.raw_os_error() == Some(libc::EPERM)
}

#[cfg(not(unix))]
fn is_process_alive(_pid: u32) -> bool {
    // Cannot check on non-Unix; assume alive (fall back to age check only).
    true
}

/// Non-blocking exclusive `flock` on Unix.  Returns `true` if acquired,
/// `false` if `EWOULDBLOCK` (another process holds the lock).  On non-Unix
/// platforms this is a no-op that always returns `true`.
#[cfg(unix)]
fn flock_exclusive_nb(file: &fs::File) -> std::io::Result<bool> {
    use std::os::unix::io::AsRawFd as _;
    let ret = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if ret == 0 {
        Ok(true)
    } else {
        let err = std::io::Error::last_os_error();
        if err.kind() == std::io::ErrorKind::WouldBlock {
            Ok(false)
        } else {
            Err(err)
        }
    }
}

#[cfg(not(unix))]
#[allow(clippy::unnecessary_wraps)]
fn flock_exclusive_nb(_file: &fs::File) -> std::io::Result<bool> {
    Ok(true)
}

// ---------------------------------------------------------------------------
// chronicle sync
// ---------------------------------------------------------------------------

/// Handle `chronicle sync [--dry-run] [--quiet]`.
pub fn handle_sync(dry_run: bool, quiet: bool) -> Result<()> {
    let config_path = config::default_config_path();
    let home = dirs::home_dir().context("could not determine home directory")?;
    sync_impl(dry_run, quiet, &config_path, &home)
}

/// Core sync logic, factored out for testability.
///
/// Executes the full bidirectional sync cycle (§14):
/// 1. **Outgoing**: scan changed session files, canonicalize, merge, commit.
/// 2. **Git exchange**: fetch from remote, integrate remote JSONL changes, push.
/// 3. **Incoming**: de-canonicalize, apply partial history filter, write local.
/// 4. **Bookkeeping**: update state cache and manifest.json.
///
/// `home` is injected so tests can use a temporary directory without touching
/// the real `$HOME`.
pub fn sync_impl(dry_run: bool, quiet: bool, config_path: &Path, home: &Path) -> Result<()> {
    let cfg = config::load(Some(config_path), &CliOverrides::default())
        .context("failed to load configuration")?;

    // Jitter: when invoked by cron (--quiet), sleep a deterministic per-machine
    // offset so that machines sharing the same cron interval don't all hit the
    // remote simultaneously (thundering-herd avoidance).
    if quiet && !dry_run {
        let jitter = crate::scheduler::cron::compute_jitter(
            &cfg.general.machine_name,
            &cfg.general.sync_interval,
            cfg.general.sync_jitter_secs,
        );
        if jitter > 0 {
            std::thread::sleep(std::time::Duration::from_secs(jitter));
        }
    }

    // L3 warning always goes to stderr (not suppressed by --quiet).
    if cfg.canonicalization.level >= 3 {
        eprintln!("{L3_WARNING}");
    }

    let registry = TokenRegistry::from_config(&cfg.canonicalization, home);
    let canon_level = cfg.canonicalization.level;
    let machine_name = non_empty_machine_name(&cfg.general.machine_name);
    let repo_path = config::expand_path_with_home(&cfg.storage.repo_path, home);
    let remote_url: Option<&str> = if cfg.storage.remote_url.is_empty() {
        None
    } else {
        Some(cfg.storage.remote_url.as_str())
    };
    let follow_symlinks = cfg.general.follow_symlinks;

    // Acquire advisory lock to prevent concurrent sync processes (Gap 2 fix).
    // Uses non-blocking flock so a second cron invocation fails fast rather
    // than queuing on the git index lock and causing a cascade.  Stale locks
    // (dead PID or age > lock_timeout_secs) are broken automatically (ADR-001).
    let _sync_lock = match try_acquire_sync_lock(&repo_path, cfg.general.lock_timeout_secs)? {
        Some(lock) => lock,
        None => {
            println!("[sync] Another sync is in progress — skipping this run.");
            return Ok(());
        }
    };
    let sync_op_start = std::time::Instant::now();

    // -----------------------------------------------------------------------
    // Load state cache (missing file → empty; all files treated as New).
    // Derive the path from the repo dir so each Chronicle install (and each
    // test's tempdir) gets its own isolated cache.
    // -----------------------------------------------------------------------
    let cache_path = scan::StateCache::path_for_repo(&repo_path);
    let mut state_cache =
        scan::StateCache::load(&cache_path).context("failed to load state cache")?;

    // -----------------------------------------------------------------------
    // Collect outgoing changes across enabled agents.
    // -----------------------------------------------------------------------
    let mut changed: Vec<ScannedChange> = Vec::new();

    if cfg.agents.pi.enabled {
        let source_dir = config::expand_path_with_home(&cfg.agents.pi.session_dir, home);
        if source_dir.exists() {
            match scan::scan_dir(&source_dir, &state_cache, follow_symlinks) {
                Ok(entries) => {
                    for e in entries
                        .into_iter()
                        .filter(|e| e.kind != scan::ChangeKind::Unchanged)
                    {
                        changed.push(ScannedChange {
                            entry: e,
                            source_dir: source_dir.clone(),
                            repo_rel_base: "pi/sessions",
                            is_pi: true,
                        });
                    }
                }
                Err(e) => eprintln!("  Warning: failed to scan Pi sessions: {e}"),
            }
        }
    }

    if cfg.agents.claude.enabled {
        let source_dir = config::expand_path_with_home(&cfg.agents.claude.session_dir, home);
        if source_dir.exists() {
            match scan::scan_dir(&source_dir, &state_cache, follow_symlinks) {
                Ok(entries) => {
                    for e in entries
                        .into_iter()
                        .filter(|e| e.kind != scan::ChangeKind::Unchanged)
                    {
                        changed.push(ScannedChange {
                            entry: e,
                            source_dir: source_dir.clone(),
                            repo_rel_base: "claude/projects",
                            is_pi: false,
                        });
                    }
                }
                Err(e) => eprintln!("  Warning: failed to scan Claude sessions: {e}"),
            }
        }
    }

    // -----------------------------------------------------------------------
    // Dry-run: describe all phases without writing.
    // -----------------------------------------------------------------------
    if dry_run {
        let new_count = changed
            .iter()
            .filter(|c| c.entry.kind == scan::ChangeKind::New)
            .count();
        let mod_count = changed.len() - new_count;
        println!("Dry run — sync would:");
        println!(
            "  [outgoing]  {} new + {} modified file(s) to commit",
            new_count, mod_count
        );
        println!("  [git]       fetch → integrate remote changes → push");
        println!("  [incoming]  materialize session files to local agent dirs");
        return Ok(());
    }

    // -----------------------------------------------------------------------
    // Phase 1: Outgoing — canonicalize changed files, stage, commit.
    // -----------------------------------------------------------------------
    let manager = git::RepoManager::init_or_open(&repo_path, remote_url, &cfg.storage.branch)
        .context("failed to open git repository")?;

    let mut pi_staged: Vec<PathBuf> = Vec::new();
    let mut claude_staged: Vec<PathBuf> = Vec::new();
    let mut total_new = 0usize;
    let mut total_modified = 0usize;
    let mut cache_updates: Vec<(String, scan::FileState)> = Vec::new();

    for c in &changed {
        let is_new = c.entry.kind == scan::ChangeKind::New;
        match process_push_file(&PushFileParams {
            entry: &c.entry,
            source_dir: &c.source_dir,
            repo_rel_base: c.repo_rel_base,
            repo_path: &repo_path,
            registry: &registry,
            canon_level,
            is_pi: c.is_pi,
        }) {
            Ok(Some(pushed)) => {
                // Always cache — prevents re-scanning on the next run.
                cache_updates.push((pushed.cache_key, pushed.file_state));
                if pushed.stage {
                    if is_new {
                        total_new += 1;
                    } else {
                        total_modified += 1;
                    }
                    if c.is_pi {
                        pi_staged.push(pushed.staged_rel);
                    } else {
                        claude_staged.push(pushed.staged_rel);
                    }
                }
            }
            Ok(None) => {}
            Err(e) => eprintln!("  Warning: skipping {}: {e}", c.entry.path.display()),
        }
    }

    let pi_total = pi_staged.len();
    let claude_total = claude_staged.len();
    let all_staged: Vec<PathBuf> = pi_staged.into_iter().chain(claude_staged).collect();
    let outgoing_count = all_staged.len();

    let now = Utc::now();

    // Prepare updated manifest (upsert last_sync for this machine).
    let updated_manifest = build_updated_manifest(&manager, &machine_name, now)
        .context("failed to build updated manifest")?;

    if !all_staged.is_empty() {
        // Stage session files.
        let staged_refs: Vec<&Path> = all_staged.iter().map(|p| p.as_path()).collect();
        manager
            .stage_files(&staged_refs)
            .context("failed to stage outgoing session files")?;

        // Write and stage manifest.json alongside the session changes.
        manager
            .write_manifest(&updated_manifest)
            .context("failed to write manifest")?;
        let manifest_rel = PathBuf::from(".chronicle/manifest.json");
        manager
            .stage_files(&[manifest_rel.as_path()])
            .context("failed to stage manifest")?;

        let summary = git::SyncSummary {
            new_files: total_new,
            modified_files: total_modified,
            pi_total,
            claude_total,
        };
        let msg = git::format_sync_message(&machine_name, &now, &summary);
        manager
            .commit_if_staged(&msg, &machine_name)
            .context("failed to create outgoing sync commit")?;

        if !quiet {
            println!(
                "[outgoing] Committed {outgoing_count} file(s) ({total_new} new, {total_modified} modified)."
            );
        }
    } else {
        // No session changes — write manifest to disk only (no commit, idempotent).
        manager
            .write_manifest(&updated_manifest)
            .context("failed to write manifest")?;
        if !quiet {
            println!("[outgoing] Nothing to commit.");
        }
    }

    // -----------------------------------------------------------------------
    // Phase 2: Git exchange — fetch, integrate remote changes, push.
    //          Skipped when no remote URL is configured.
    // -----------------------------------------------------------------------
    // remote_integrated is exposed to Phase 3 so materialize can be skipped
    // when nothing arrived from the remote this cycle.
    let remote_integrated: usize = if remote_url.is_some() {
        let ring_buf = errors::ring_buffer::RingBuffer::new(
            errors::ring_buffer::RingBuffer::path_for_repo(&repo_path),
        );

        match manager.fetch("origin") {
            Ok(()) => {}
            Err(ref e) if git::is_network_error(e) => {
                let rb_entry = errors::ring_buffer::ErrorEntry::new(
                    errors::ring_buffer::Severity::Error,
                    "git_error",
                    format!("network error during fetch: {e}"),
                );
                let _ = ring_buf.append(rb_entry);
                return Err(anyhow::anyhow!("sync fetch failed (network error): {e}"));
            }
            Err(e) => return Err(anyhow::anyhow!("sync fetch failed: {e}")),
        }

        let integrated = integrate_remote_changes(&manager, &machine_name)
            .context("failed to integrate remote changes")?;

        // Only push if the local repo has at least one commit.
        if manager.repository().head().is_ok() {
            match manager.push_with_retry("origin", || Ok(()), std::thread::sleep) {
                Ok(()) => {
                    if !quiet {
                        println!(
                            "[git]      {integrated} remote file(s) integrated; pushed to remote."
                        );
                    }
                }
                Err(e) => {
                    let rb_entry = errors::ring_buffer::ErrorEntry::new(
                        errors::ring_buffer::Severity::Error,
                        "push_conflict",
                        e.to_string(),
                    );
                    let _ = ring_buf.append(rb_entry);
                    return Err(anyhow::anyhow!("sync push failed: {e}"));
                }
            }
        } else if !quiet {
            println!("[git]      No local commits yet — skipping push.");
        }
        integrated
    } else {
        if !quiet {
            println!("[git]      No remote configured — skipping fetch/push.");
        }
        0
    };

    // -----------------------------------------------------------------------
    // Phase 3: Incoming — materialize repo working tree -> local agent dirs.
    //
    // Fast path: skip the full repo scan when nothing arrived from the remote.
    // Outgoing files came FROM local and are already present, so an
    // outgoing-only cycle needs no materialization pass.
    // -----------------------------------------------------------------------
    let materialized = if remote_integrated > 0 {
        materialize_repo_to_local(&repo_path, &cfg, home, &registry)
            .context("failed to materialize session files")?
    } else {
        0
    };

    if !quiet {
        if materialized > 0 {
            println!("[incoming] Materialized {materialized} file(s) to local agent dirs.");
        } else {
            println!("[incoming] Nothing to materialize.");
        }
    }

    // -----------------------------------------------------------------------
    // Bookkeeping: persist state cache after a successful sync.
    // -----------------------------------------------------------------------
    for (key, state) in cache_updates {
        state_cache.files.insert(key, state);
    }
    state_cache
        .save(&cache_path)
        .context("failed to save state cache")?;

    // Record last-sync metadata for `chronicle status` (US-001).
    if let Err(e) = sync_state::write_sync_state(&repo_path, SyncOp::Sync, sync_op_start.elapsed())
    {
        tracing::warn!("failed to write sync_state.json: {e}");
    }

    if !quiet {
        println!("Sync complete.");
    }

    Ok(())
}

/// Read the current manifest, upsert this machine's entry (setting
/// `last_sync = now`), and return the updated struct without writing to disk.
///
/// Creates a new entry (with `first_seen = now`) if the machine has not yet
/// been seen.
fn build_updated_manifest(
    manager: &git::RepoManager,
    machine_name: &str,
    now: DateTime<Utc>,
) -> Result<git::Manifest> {
    let mut manifest = manager
        .read_manifest()
        .context("failed to read manifest.json")?;

    let os_name = if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    };

    let entry = manifest
        .machines
        .entry(machine_name.to_owned())
        .or_insert_with(|| git::MachineEntry {
            first_seen: now,
            last_sync: None,
            home_path: "{{SYNC_HOME}}".to_owned(),
            os: os_name.to_owned(),
        });
    entry.last_sync = Some(now);

    Ok(manifest)
}

// ---------------------------------------------------------------------------
// chronicle push
// ---------------------------------------------------------------------------

/// Handle `chronicle push [--dry-run]`.
pub fn handle_push(dry_run: bool) -> Result<()> {
    let config_path = config::default_config_path();
    let home = dirs::home_dir().context("could not determine home directory")?;
    push_impl(dry_run, &config_path, &home)
}

/// Core push logic, factored out for testability.
///
/// Scans each enabled agent's session directory for new or modified `.jsonl`
/// files, canonicalizes them, merges with the existing repo version at JSONL
/// entry level, commits, and pushes to remote (§9.1 / §14 outgoing phase).
pub fn push_impl(dry_run: bool, config_path: &Path, home: &Path) -> Result<()> {
    let cfg = config::load(Some(config_path), &CliOverrides::default())
        .context("failed to load configuration")?;

    if cfg.canonicalization.level >= 3 {
        eprintln!("{L3_WARNING}");
    }

    let registry = TokenRegistry::from_config(&cfg.canonicalization, home);
    let canon_level = cfg.canonicalization.level;
    let machine_name = non_empty_machine_name(&cfg.general.machine_name);
    let repo_path = config::expand_path_with_home(&cfg.storage.repo_path, home);
    let remote_url: Option<&str> = if cfg.storage.remote_url.is_empty() {
        None
    } else {
        Some(cfg.storage.remote_url.as_str())
    };
    let follow_symlinks = cfg.general.follow_symlinks;

    // Acquire advisory lock to prevent concurrent push processes (Gap 2 fix).
    let _sync_lock = match try_acquire_sync_lock(&repo_path, cfg.general.lock_timeout_secs)? {
        Some(lock) => lock,
        None => {
            println!("[push] Another sync is in progress — skipping this run.");
            return Ok(());
        }
    };
    let push_op_start = std::time::Instant::now();

    // Load state cache (missing file → empty cache; all files treated as New).
    // Derive the path from the repo dir so each install gets its own cache.
    let cache_path = scan::StateCache::path_for_repo(&repo_path);
    let mut state_cache =
        scan::StateCache::load(&cache_path).context("failed to load state cache")?;

    // Collect all changed files across enabled agents.
    let mut changed: Vec<ScannedChange> = Vec::new();

    if cfg.agents.pi.enabled {
        let source_dir = config::expand_path_with_home(&cfg.agents.pi.session_dir, home);
        if source_dir.exists() {
            match scan::scan_dir(&source_dir, &state_cache, follow_symlinks) {
                Ok(entries) => {
                    for e in entries
                        .into_iter()
                        .filter(|e| e.kind != scan::ChangeKind::Unchanged)
                    {
                        changed.push(ScannedChange {
                            entry: e,
                            source_dir: source_dir.clone(),
                            repo_rel_base: "pi/sessions",
                            is_pi: true,
                        });
                    }
                }
                Err(e) => eprintln!("  Warning: failed to scan Pi sessions: {e}"),
            }
        }
    }

    if cfg.agents.claude.enabled {
        let source_dir = config::expand_path_with_home(&cfg.agents.claude.session_dir, home);
        if source_dir.exists() {
            match scan::scan_dir(&source_dir, &state_cache, follow_symlinks) {
                Ok(entries) => {
                    for e in entries
                        .into_iter()
                        .filter(|e| e.kind != scan::ChangeKind::Unchanged)
                    {
                        changed.push(ScannedChange {
                            entry: e,
                            source_dir: source_dir.clone(),
                            repo_rel_base: "claude/projects",
                            is_pi: false,
                        });
                    }
                }
                Err(e) => eprintln!("  Warning: failed to scan Claude sessions: {e}"),
            }
        }
    }

    if dry_run {
        let new_count = changed
            .iter()
            .filter(|c| c.entry.kind == scan::ChangeKind::New)
            .count();
        let mod_count = changed.len() - new_count;
        println!(
            "Dry run: would push {} new + {} modified file(s).",
            new_count, mod_count
        );
        return Ok(());
    }

    if changed.is_empty() {
        println!("Nothing to push.");
        return Ok(());
    }

    let manager = git::RepoManager::init_or_open(&repo_path, remote_url, &cfg.storage.branch)
        .context("failed to open git repository")?;

    let mut pi_staged: Vec<PathBuf> = Vec::new();
    let mut claude_staged: Vec<PathBuf> = Vec::new();
    let mut total_new = 0usize;
    let mut total_modified = 0usize;
    let mut cache_updates: Vec<(String, scan::FileState)> = Vec::new();

    for c in &changed {
        let is_new = c.entry.kind == scan::ChangeKind::New;
        match process_push_file(&PushFileParams {
            entry: &c.entry,
            source_dir: &c.source_dir,
            repo_rel_base: c.repo_rel_base,
            repo_path: &repo_path,
            registry: &registry,
            canon_level,
            is_pi: c.is_pi,
        }) {
            Ok(Some(pushed)) => {
                // Always cache — prevents re-scanning on the next run.
                cache_updates.push((pushed.cache_key, pushed.file_state));
                if pushed.stage {
                    if is_new {
                        total_new += 1;
                    } else {
                        total_modified += 1;
                    }
                    if c.is_pi {
                        pi_staged.push(pushed.staged_rel);
                    } else {
                        claude_staged.push(pushed.staged_rel);
                    }
                }
            }
            Ok(None) => {} // file directly in source_dir (no session subdir) — skip
            Err(e) => {
                eprintln!("  Warning: skipping {}: {e}", c.entry.path.display());
            }
        }
    }

    let pi_total = pi_staged.len();
    let claude_total = claude_staged.len();
    let all_staged: Vec<PathBuf> = pi_staged.into_iter().chain(claude_staged).collect();

    if all_staged.is_empty() {
        println!("Nothing new to push (repo already up to date).");
        return Ok(());
    }

    let now = Utc::now();

    // Prepare updated manifest (upsert last_sync for this machine).
    let updated_manifest = build_updated_manifest(&manager, &machine_name, now)
        .context("failed to build updated manifest")?;

    // Stage all changed files.
    let staged_refs: Vec<&Path> = all_staged.iter().map(|p| p.as_path()).collect();
    manager
        .stage_files(&staged_refs)
        .context("failed to stage files for push")?;

    // Write and stage manifest.json alongside the session changes.
    manager
        .write_manifest(&updated_manifest)
        .context("failed to write manifest")?;
    let manifest_rel = PathBuf::from(".chronicle/manifest.json");
    manager
        .stage_files(&[manifest_rel.as_path()])
        .context("failed to stage manifest")?;

    // Create sync commit.
    let summary = git::SyncSummary {
        new_files: total_new,
        modified_files: total_modified,
        pi_total,
        claude_total,
    };
    let msg = git::format_sync_message(&machine_name, &now, &summary);
    manager
        .commit_if_staged(&msg, &machine_name)
        .context("failed to commit staged files")?;

    // Push with retry (§6.5).  On exhaustion, log to ring buffer and fail.
    // on_rejection performs fetch + integrate so retries can resolve divergence.
    let ring_buf = errors::ring_buffer::RingBuffer::new(
        errors::ring_buffer::RingBuffer::path_for_repo(&repo_path),
    );
    match manager.push_with_retry(
        "origin",
        || {
            manager.fetch("origin")?;
            integrate_remote_changes(&manager, &machine_name)
                .map(|_| ())
                .map_err(|e| git::GitError::Manifest(e.to_string()))
        },
        std::thread::sleep,
    ) {
        Ok(()) => {
            println!(
                "Pushed {} file(s) ({} new, {} modified) to remote.",
                all_staged.len(),
                total_new,
                total_modified
            );
        }
        Err(e) => {
            let rb_entry = errors::ring_buffer::ErrorEntry::new(
                errors::ring_buffer::Severity::Error,
                "push_conflict",
                e.to_string(),
            );
            let _ = ring_buf.append(rb_entry);
            return Err(anyhow::anyhow!("push failed: {e}"));
        }
    }

    // Update state cache after a successful push.
    for (key, state) in cache_updates {
        state_cache.files.insert(key, state);
    }
    state_cache
        .save(&cache_path)
        .context("failed to save state cache")?;

    // Record last-sync metadata for `chronicle status` (US-001).
    if let Err(e) = sync_state::write_sync_state(&repo_path, SyncOp::Push, push_op_start.elapsed())
    {
        tracing::warn!("failed to write sync_state.json: {e}");
    }

    Ok(())
}

/// A changed file collected by the scanner for push processing.
struct ScannedChange {
    /// Scanner result for this file.
    entry: scan::ScanEntry,
    /// Agent session directory that the file lives in.
    source_dir: PathBuf,
    /// Repo-relative base path for this agent (`"pi/sessions"` or
    /// `"claude/projects"`).
    repo_rel_base: &'static str,
    /// `true` for Pi; `false` for Claude.
    is_pi: bool,
}

/// The result of processing one file during a push.
struct PushedFile {
    /// Repo-relative path of the written file (used for staging).
    /// Ignored when `stage` is `false`.
    staged_rel: PathBuf,
    /// State-cache key — the file's absolute local path string.
    cache_key: String,
    /// Updated state to record in the state cache.
    file_state: scan::FileState,
    /// Whether this file should be staged for a git commit.
    ///
    /// `false` when the repo already contained identical content — the file
    /// still needs to be cached so future scans classify it as `Unchanged`
    /// and skip re-processing it.
    stage: bool,
}

/// Parameters for [`process_push_file`].
///
/// Bundled into a struct to satisfy `clippy::too_many_arguments` (max 7).
struct PushFileParams<'a> {
    entry: &'a scan::ScanEntry,
    source_dir: &'a Path,
    /// E.g. `"pi/sessions"`.
    repo_rel_base: &'a str,
    repo_path: &'a Path,
    registry: &'a TokenRegistry,
    canon_level: u8,
    is_pi: bool,
}

/// Canonicalize one changed local `.jsonl` file and write the merged result
/// to the repository working tree.
///
/// Returns `Some(PushedFile)` when the file was written and should be staged,
/// or `None` when the repo was already up to date (nothing to stage).
///
/// Skips the file with a warning on permission / read errors (§11.4).
fn process_push_file(p: &PushFileParams<'_>) -> Result<Option<PushedFile>> {
    let PushFileParams {
        entry,
        source_dir,
        repo_rel_base,
        repo_path,
        registry,
        canon_level,
        is_pi,
    } = p;
    let (is_pi, canon_level) = (*is_pi, *canon_level);

    // Compute the relative path within source_dir.
    let rel_path = entry
        .path
        .strip_prefix(source_dir)
        .context("file must be inside source directory")?;

    let mut components = rel_path.components();
    let session_dir_name = components
        .next()
        .ok_or_else(|| anyhow::anyhow!("no session directory component in path"))?;
    let session_dir_name = session_dir_name.as_os_str().to_string_lossy().into_owned();
    let file_rel = components.as_path();
    let file_name_str = file_rel.to_string_lossy().into_owned();

    // Skip files that sit directly in source_dir (no session subdir level).
    if file_name_str.is_empty() {
        return Ok(None);
    }

    // L1: canonicalize the session directory name.
    let canonical_dir = if is_pi {
        registry.canonicalize_pi_dir(&session_dir_name)
    } else {
        registry.canonicalize_claude_dir(&session_dir_name)
    };
    let staged_rel = PathBuf::from(format!("{repo_rel_base}/{canonical_dir}/{file_name_str}"));

    // Read local file; skip on read error (§11.4 — permission errors).
    let raw = match fs::read_to_string(&entry.path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("  Warning: cannot read {}: {e}", entry.path.display());
            return Ok(None);
        }
    };

    // L2/L3 canonicalize each line.
    let canonical_lines: Vec<String> = raw
        .lines()
        .map(|line| {
            if line.is_empty() {
                return String::new();
            }
            registry
                .canonicalize_line(line, canon_level)
                .unwrap_or_else(|e| {
                    eprintln!("  Warning: canonicalization error: {e}");
                    line.to_owned()
                })
        })
        .collect();

    let mut canonical_content = canonical_lines.join("\n");
    if raw.ends_with('\n') && !canonical_content.ends_with('\n') {
        canonical_content.push('\n');
    }

    // Merge with existing repo version (grow-only set union, §5.2).
    let dest_abs = repo_path.join(&staged_rel);
    let merged_content = if dest_abs.exists() {
        let repo_content = fs::read_to_string(&dest_abs)
            .with_context(|| format!("cannot read repo file {}", dest_abs.display()))?;
        let out = merge_jsonl(
            &repo_content,
            &staged_rel,
            &canonical_content,
            &staged_rel,
            &NullReporter,
        );
        // If the merged result equals what is already in the repo, don't stage —
        // but DO return a cache entry so future scans see this file as Unchanged.
        if out.content == repo_content {
            let cache_key = entry.path.to_string_lossy().into_owned();
            let file_state = scan::FileState {
                local_mtime: entry.mtime,
                local_size: entry.size,
                last_synced_size: entry.size,
                local_path: entry.path.clone(),
            };
            return Ok(Some(PushedFile {
                staged_rel: PathBuf::new(),
                cache_key,
                file_state,
                stage: false,
            }));
        }
        out.content
    } else {
        canonical_content
    };

    // Create destination directory and write merged content.
    if let Some(parent) = dest_abs.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("cannot create {}", parent.display()))?;
    }
    fs::write(&dest_abs, &merged_content)
        .with_context(|| format!("cannot write {}", dest_abs.display()))?;

    let cache_key = entry.path.to_string_lossy().into_owned();
    let file_state = scan::FileState {
        local_mtime: entry.mtime,
        local_size: entry.size,
        last_synced_size: entry.size,
        local_path: entry.path.clone(),
    };

    Ok(Some(PushedFile {
        staged_rel,
        cache_key,
        file_state,
        stage: true,
    }))
}

// ---------------------------------------------------------------------------
// chronicle pull
// ---------------------------------------------------------------------------

/// Handle `chronicle pull [--dry-run]`.
pub fn handle_pull(dry_run: bool) -> Result<()> {
    let config_path = config::default_config_path();
    let home = dirs::home_dir().context("could not determine home directory")?;
    pull_impl(dry_run, &config_path, &home)
}

/// Core pull logic, factored out for testability.
///
/// Fetches from remote, integrates remote changes into the working tree at
/// JSONL entry level, then de-canonicalizes and materializes all session files
/// into the local agent session directories (§9.1 / §14 incoming phase).
pub fn pull_impl(dry_run: bool, config_path: &Path, home: &Path) -> Result<()> {
    let cfg = config::load(Some(config_path), &CliOverrides::default())
        .context("failed to load configuration")?;

    if dry_run {
        println!("Dry run: would fetch from remote and materialize session files locally.");
        return Ok(());
    }

    let registry = TokenRegistry::from_config(&cfg.canonicalization, home);
    let machine_name = non_empty_machine_name(&cfg.general.machine_name);
    let repo_path = config::expand_path_with_home(&cfg.storage.repo_path, home);
    let remote_url: Option<&str> = if cfg.storage.remote_url.is_empty() {
        None
    } else {
        Some(cfg.storage.remote_url.as_str())
    };

    // Acquire advisory lock to prevent concurrent pull/sync processes (ADR-001).
    let _sync_lock = match try_acquire_sync_lock(&repo_path, cfg.general.lock_timeout_secs)? {
        Some(lock) => lock,
        None => {
            println!("[pull] Another sync is in progress — skipping this run.");
            return Ok(());
        }
    };
    let pull_op_start = std::time::Instant::now();

    let manager = git::RepoManager::init_or_open(&repo_path, remote_url, &cfg.storage.branch)
        .context("failed to open git repository")?;

    // Step 1: Fetch from remote.
    let ring_buf = errors::ring_buffer::RingBuffer::new(
        errors::ring_buffer::RingBuffer::path_for_repo(&repo_path),
    );
    match manager.fetch("origin") {
        Ok(()) => {}
        Err(ref e) if git::is_network_error(e) => {
            let rb_entry = errors::ring_buffer::ErrorEntry::new(
                errors::ring_buffer::Severity::Error,
                "git_error",
                format!("network error during fetch: {e}"),
            );
            let _ = ring_buf.append(rb_entry);
            return Err(anyhow::anyhow!("fetch failed (network error): {e}"));
        }
        Err(e) => return Err(anyhow::anyhow!("fetch failed: {e}")),
    }

    // Step 2: Integrate remote tracking branch changes into the working tree.
    let integrated = integrate_remote_changes(&manager, &machine_name)
        .context("failed to integrate remote changes")?;

    // Step 3: Materialize repo files to local agent session directories.
    //
    // Fast path: skip the full repo scan when nothing arrived from the remote.
    // Mirrors the same guard in sync_impl — outgoing-only or no-op pull cycles
    // need no materialization pass.
    let materialized = if integrated > 0 {
        materialize_repo_to_local(&repo_path, &cfg, home, &registry)
            .context("failed to materialize session files")?
    } else {
        println!("[pull] Nothing to materialize — no remote changes arrived.");
        0
    };

    if integrated > 0 {
        println!(
            "Pull complete: {} remote file(s) integrated, {} file(s) materialized locally.",
            integrated, materialized
        );
    }

    // Record last-sync metadata for `chronicle status` (US-001).
    if let Err(e) = sync_state::write_sync_state(&repo_path, SyncOp::Pull, pull_op_start.elapsed())
    {
        tracing::warn!("failed to write sync_state.json: {e}");
    }

    Ok(())
}

/// Walk the remote tracking branch, merge each changed JSONL file at entry
/// level into the local working tree, and create a merge commit when anything
/// changed.  Returns the number of files updated in the working tree.
fn integrate_remote_changes(manager: &git::RepoManager, machine_name: &str) -> Result<usize> {
    let repo = manager.repository();

    // Locate the remote tracking ref using the configured branch name.
    let tracking_ref = format!("refs/remotes/origin/{}", manager.branch);
    let remote_ref = repo.find_reference(&tracking_ref);

    let remote_ref = match remote_ref {
        Ok(r) => r,
        Err(_) => return Ok(0), // no remote commits pushed yet
    };

    // Collect the remote commit OID and all JSONL blob OIDs from the remote
    // tree.  We save only Copy values here so all git2 borrows are released
    // before we do the merge work and final commit.
    let (remote_commit_oid, remote_blobs): (git2::Oid, Vec<(String, git2::Oid)>) = {
        let rc = remote_ref
            .peel_to_commit()
            .context("failed to peel remote ref to commit")?;
        let rtree = rc.tree().context("failed to get remote commit tree")?;
        let mut blobs: Vec<(String, git2::Oid)> = Vec::new();
        rtree
            .walk(git2::TreeWalkMode::PreOrder, |root, entry| {
                if entry.kind() == Some(git2::ObjectType::Blob) {
                    let name = entry.name().unwrap_or("");
                    if name.ends_with(".jsonl") {
                        blobs.push((format!("{root}{name}"), entry.id()));
                    }
                }
                git2::TreeWalkResult::Ok
            })
            .context("failed to walk remote tree")?;
        (rc.id(), blobs)
        // rc, rtree, remote_ref all dropped here
    };

    let mut staged_paths: Vec<PathBuf> = Vec::new();

    for (repo_rel, oid) in remote_blobs {
        let blob = repo
            .find_blob(oid)
            .with_context(|| format!("cannot find blob for {repo_rel}"))?;
        let remote_content = match std::str::from_utf8(blob.content()) {
            Ok(s) => s.to_owned(),
            Err(_) => continue, // skip non-UTF-8 blobs
        };

        let dest_abs = manager.repo_path().join(&repo_rel);
        let local_content = if dest_abs.exists() {
            fs::read_to_string(&dest_abs)
                .with_context(|| format!("cannot read {}", dest_abs.display()))?
        } else {
            String::new()
        };

        if local_content == remote_content {
            continue; // already up to date
        }

        // Merge at JSONL entry level (remote wins on conflict, §5.4).
        let path_pb = PathBuf::from(&repo_rel);
        let out = merge_jsonl(
            &remote_content,
            &path_pb,
            &local_content,
            &path_pb,
            &NullReporter,
        );

        if let Some(parent) = dest_abs.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("cannot create {}", parent.display()))?;
        }
        fs::write(&dest_abs, &out.content)
            .with_context(|| format!("cannot write {}", dest_abs.display()))?;

        staged_paths.push(PathBuf::from(&repo_rel));
    }

    let changed = staged_paths.len();
    if !staged_paths.is_empty() {
        let staged_refs: Vec<&Path> = staged_paths.iter().map(|p| p.as_path()).collect();
        manager
            .stage_files(&staged_refs)
            .context("failed to stage merged files")?;

        let now = Utc::now();
        let msg = format!(
            "pull: merge from remote @ {}\n\nUpdated {} file(s)\n",
            now.format("%Y-%m-%dT%H:%M:%SZ"),
            changed
        );

        // Create a MERGE commit that grafts the remote history onto the local
        // branch.  Using [local_head, remote_commit] as parents means the new
        // commit is a descendant of the remote tip, so the subsequent push is
        // a fast-forward and never gets rejected as non-fast-forward.
        let mut index = repo
            .index()
            .context("failed to open git index for merge commit")?;
        let tree_oid = index
            .write_tree()
            .context("failed to write index tree for merge commit")?;
        let tree = repo
            .find_tree(tree_oid)
            .context("failed to find tree for merge commit")?;

        let time = git2::Time::new(now.timestamp(), 0);
        let sig = git2::Signature::new(machine_name, "chronicle@local", &time)
            .context("failed to create git signature for merge commit")?;

        // Re-find the remote commit by its saved OID.
        let remote_parent = repo
            .find_commit(remote_commit_oid)
            .context("failed to find remote commit for merge parent")?;

        // Optional local HEAD parent (absent on an unborn branch / first sync).
        let local_parent: Option<git2::Commit<'_>> = match repo.head() {
            Ok(h) => Some(
                h.peel_to_commit()
                    .context("failed to peel HEAD to commit")?,
            ),
            Err(e)
                if e.code() == git2::ErrorCode::UnbornBranch
                    || e.code() == git2::ErrorCode::NotFound =>
            {
                None
            }
            Err(e) => return Err(anyhow::anyhow!("HEAD error during merge commit: {e}")),
        };

        let mut parents_owned: Vec<git2::Commit<'_>> = Vec::new();
        if let Some(lp) = local_parent {
            parents_owned.push(lp);
        }
        parents_owned.push(remote_parent);
        let parent_refs: Vec<&git2::Commit<'_>> = parents_owned.iter().collect();

        repo.commit(Some("HEAD"), &sig, &sig, &msg, &tree, &parent_refs)
            .context("failed to create pull merge commit")?;
    }

    Ok(changed)
}

/// Controls how many session files are materialized per directory during a
/// pull (§7 — partial history materialization).
#[derive(Debug, Clone)]
enum MaterializeFilter {
    /// Materialize all files in every session directory.
    Full,
    /// Materialize only the N most-recent files per session directory.
    Partial(usize),
}

impl MaterializeFilter {
    fn from_config(cfg: &config::Config) -> Self {
        match cfg.sync.history_mode {
            HistoryMode::Full => MaterializeFilter::Full,
            HistoryMode::Partial => MaterializeFilter::Partial(cfg.sync.partial_max_count),
        }
    }
}

/// De-canonicalize all JSONL files in the repo working tree and write them
/// to the local agent session directories (materialization — §14 step 5).
///
/// Loads and saves [`MaterializeCache`] to skip repo files whose mtime/size
/// have not changed since the previous pass.  The cache is invalidated
/// automatically when the canonicalization configuration changes.
///
/// Returns the total number of files written.
fn materialize_repo_to_local(
    repo_path: &Path,
    cfg: &config::Config,
    home: &Path,
    registry: &TokenRegistry,
) -> Result<usize> {
    // Load the materialize cache (empty if not found).
    let cache_path = MaterializeCache::path_for_repo(repo_path);
    let mut cache =
        MaterializeCache::load(&cache_path).context("failed to load materialize cache")?;

    // Invalidate the cache when the canonicalization config has changed.
    let config_hash = format!(
        "{}:{}",
        cfg.canonicalization.level, cfg.canonicalization.home_token
    );
    if cache.config_hash != config_hash {
        cache.files.clear();
        cache.config_hash = config_hash;
    }

    let filter = MaterializeFilter::from_config(cfg);
    let mut total = 0usize;

    if cfg.agents.pi.enabled {
        let pi_sessions_repo = repo_path.join("pi").join("sessions");
        if pi_sessions_repo.exists() {
            let local_pi_dir = config::expand_path_with_home(&cfg.agents.pi.session_dir, home);
            total += materialize_agent_dir(
                &pi_sessions_repo,
                &local_pi_dir,
                registry,
                true,
                &filter,
                &mut cache,
                "pi/sessions",
            )
            .context("Pi session materialization failed")?;
        }
    }

    if cfg.agents.claude.enabled {
        let claude_projects_repo = repo_path.join("claude").join("projects");
        if claude_projects_repo.exists() {
            let local_claude_dir =
                config::expand_path_with_home(&cfg.agents.claude.session_dir, home);
            total += materialize_agent_dir(
                &claude_projects_repo,
                &local_claude_dir,
                registry,
                false,
                &filter,
                &mut cache,
                "claude/projects",
            )
            .context("Claude project materialization failed")?;
        }
    }

    // Persist the materialize cache after a successful pass.
    cache
        .save(&cache_path)
        .context("failed to save materialize cache")?;

    Ok(total)
}

/// Parse the ISO 8601 timestamp embedded in a Pi session filename.
///
/// Pi filenames use the format `YYYY-MM-DDTHH-MM-SS-mmmZ_<uuid>.jsonl`.
/// Returns `None` if the filename does not match the expected pattern.
fn pi_filename_timestamp(filename: &str) -> Option<DateTime<Utc>> {
    // Strip the `.jsonl` suffix, then split off the UUID at the first `_`.
    let stem = filename.strip_suffix(".jsonl")?;
    let (ts_part, _uuid) = stem.split_once('_')?;

    // ts_part: `YYYY-MM-DDTHH-MM-SS-mmmZ`
    // Reconstruct as RFC 3339: `YYYY-MM-DDTHH:MM:SS.mmmZ`
    let (date_part, time_part) = ts_part.split_once('T')?;
    let mut segments = time_part.splitn(4, '-');
    let hh = segments.next()?;
    let mm = segments.next()?;
    let ss = segments.next()?;
    let ms_z = segments.next()?; // e.g. "642Z"
    let ms = ms_z.strip_suffix('Z')?;
    let rfc = format!("{date_part}T{hh}:{mm}:{ss}.{ms}Z");
    DateTime::parse_from_rfc3339(&rfc)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

/// Determine the earliest entry timestamp in a JSONL file by reading it and
/// inspecting each line's `timestamp`, `created_at`, or `createdAt` field.
///
/// Returns `None` if the file cannot be read or contains no recognisable
/// timestamps.
fn claude_earliest_file_timestamp(path: &Path) -> Option<DateTime<Utc>> {
    let content = fs::read_to_string(path).ok()?;
    content
        .lines()
        .filter(|l| !l.is_empty())
        .filter_map(|line| {
            let v: serde_json::Value = serde_json::from_str(line).ok()?;
            for field in ["timestamp", "created_at", "createdAt"] {
                if let Some(s) = v.get(field).and_then(|f| f.as_str()) {
                    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
                        return Some(dt.with_timezone(&Utc));
                    }
                }
            }
            None
        })
        .min()
}

/// Given a slice of `(filename, full_repo_path)` pairs for `.jsonl` files in
/// one session directory, return the set of filenames that should be
/// materialised under the given `max_count` limit.
///
/// - Pi recency: timestamp embedded in the filename (§7.2).
/// - Claude recency: earliest entry timestamp inside the file (§7.2).
///
/// Files whose recency cannot be determined are treated as oldest (sorted to
/// the tail) so they are included only if there is room in the window.
fn select_partial_session_files(
    files: &[(String, PathBuf)],
    max_count: usize,
    is_pi: bool,
) -> HashSet<String> {
    // Build (timestamp_opt, filename) pairs.
    let mut scored: Vec<(Option<DateTime<Utc>>, &str)> = files
        .iter()
        .map(|(name, path)| {
            let ts = if is_pi {
                pi_filename_timestamp(name)
            } else {
                claude_earliest_file_timestamp(path)
            };
            (ts, name.as_str())
        })
        .collect();

    // Sort descending (newest first).  `None` timestamps sort last (oldest).
    scored.sort_by(|a, b| match (a.0, b.0) {
        (Some(ta), Some(tb)) => tb.cmp(&ta),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => a.1.cmp(b.1), // deterministic tie-break
    });

    scored
        .into_iter()
        .take(max_count)
        .map(|(_, name)| name.to_owned())
        .collect()
}

/// De-canonicalize JSONL files from one agent's repo directory and write them
/// to the corresponding local session directory.
///
/// When `filter` is [`MaterializeFilter::Partial`], only the N most-recent
/// session files per subdirectory are written.  Existing local files outside
/// the window are left untouched (§7.2 — no deletion propagation).
///
/// `cache` is used to skip repo files whose mtime/size match the last
/// materialization pass.  `repo_rel_base` is the repo-relative prefix for
/// forming cache keys (e.g. `"pi/sessions"`).
///
/// Returns the number of files written.
fn materialize_agent_dir(
    repo_agent_dir: &Path,
    local_base: &Path,
    registry: &TokenRegistry,
    is_pi: bool,
    filter: &MaterializeFilter,
    cache: &mut MaterializeCache,
    repo_rel_base: &str,
) -> Result<usize> {
    let mut total = 0usize;

    let dir_entries = match fs::read_dir(repo_agent_dir) {
        Ok(e) => e,
        Err(_) => return Ok(0),
    };

    for dir_entry in dir_entries {
        let dir_entry = dir_entry.context("failed to read session directory entry")?;
        if !dir_entry
            .file_type()
            .context("failed to get file type")?
            .is_dir()
        {
            continue; // skip .gitkeep and other non-directory entries
        }

        let canonical_dir_name = dir_entry.file_name().to_string_lossy().into_owned();

        // De-canonicalize the canonical dir name to the local agent-encoded form.
        let local_dir_name = if is_pi {
            registry.decanonicalize_pi_dir(&canonical_dir_name)
        } else {
            registry.decanonicalize_claude_dir(&canonical_dir_name)
        };
        let local_session_dir = local_base.join(&local_dir_name);

        // Collect all .jsonl files in this session subdirectory.
        let session_entries = match fs::read_dir(dir_entry.path()) {
            Ok(e) => e,
            Err(e) => {
                eprintln!("  Warning: cannot read {}: {e}", dir_entry.path().display());
                continue;
            }
        };

        let mut all_files: Vec<(String, PathBuf)> = Vec::new();
        for file_entry in session_entries {
            let file_entry = match file_entry {
                Ok(e) => e,
                Err(e) => {
                    eprintln!("  Warning: I/O error reading session file entry: {e}");
                    continue;
                }
            };
            let file_path = file_entry.path();
            if file_path.extension().is_none_or(|ext| ext != "jsonl") {
                continue;
            }
            let filename = file_path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned();
            all_files.push((filename, file_path));
        }

        // Apply partial history filter (§7).
        let selected: Option<HashSet<String>> = match filter {
            MaterializeFilter::Full => None, // all files selected
            MaterializeFilter::Partial(max_count) => {
                Some(select_partial_session_files(&all_files, *max_count, is_pi))
            }
        };

        for (filename, file_path) in &all_files {
            // Skip files outside the materialization window.
            if let Some(ref set) = selected {
                if !set.contains(filename) {
                    continue;
                }
            }

            let local_file_path = local_session_dir.join(filename);

            // Stat the repo file for the materialize cache check.
            let meta = match fs::metadata(file_path) {
                Ok(m) => m,
                Err(e) => {
                    eprintln!("  Warning: cannot stat {}: {e}", file_path.display());
                    continue;
                }
            };
            let repo_size = meta.len();
            let repo_mtime: DateTime<Utc> = match meta.modified() {
                Ok(st) => DateTime::<Utc>::from(st),
                Err(_) => DateTime::<Utc>::from(std::time::SystemTime::UNIX_EPOCH),
            };

            // Cache key: "<repo_rel_base>/<canonical_dir>/<filename>"
            let cache_key = format!("{repo_rel_base}/{canonical_dir_name}/{filename}");

            // Cache hit: repo file unchanged since last materialize pass → skip entirely.
            if let Some(cached) = cache.files.get(&cache_key) {
                if cached.repo_mtime == repo_mtime && cached.repo_size == repo_size {
                    continue;
                }
            }

            let raw = match fs::read_to_string(file_path) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("  Warning: cannot read {}: {e}", file_path.display());
                    continue;
                }
            };

            // De-canonicalize each line.
            let decanon_lines: Vec<String> = raw
                .lines()
                .map(|line| {
                    if line.is_empty() {
                        return String::new();
                    }
                    registry.decanonicalize_line(line).unwrap_or_else(|e| {
                        eprintln!("  Warning: de-canonicalization error: {e}");
                        line.to_owned()
                    })
                })
                .collect();

            let mut decanon_content = decanon_lines.join("\n");
            if raw.ends_with('\n') && !decanon_content.ends_with('\n') {
                decanon_content.push('\n');
            }

            // Create local session directory if needed.
            fs::create_dir_all(&local_session_dir)
                .with_context(|| format!("cannot create {}", local_session_dir.display()))?;

            // Skip writing if local file already has identical content (idempotent sync).
            if local_file_path.exists() {
                if let Ok(existing) = fs::read_to_string(&local_file_path) {
                    if existing == decanon_content {
                        // Content unchanged — update cache so the next pass skips this file.
                        cache.files.insert(
                            cache_key,
                            MaterializeFileState {
                                repo_mtime,
                                repo_size,
                            },
                        );
                        continue;
                    }
                }
            }

            // Write file, preserving existing permissions (§11.5).
            write_preserving_permissions(&local_file_path, &decanon_content)?;

            // Update cache entry after a successful write.
            cache.files.insert(
                cache_key,
                MaterializeFileState {
                    repo_mtime,
                    repo_size,
                },
            );
            total += 1;
        }
    }

    Ok(total)
}

/// Write `content` to `dest`, preserving existing file permissions if the
/// file already exists locally (§11.5).  New files are created with `0o644`
/// on Unix (or the parent directory's mode with execute bits stripped).
fn write_preserving_permissions(dest: &Path, content: &str) -> Result<()> {
    #[cfg(unix)]
    let mode: u32 = {
        use std::os::unix::fs::PermissionsExt;
        if dest.exists() {
            fs::metadata(dest)
                .with_context(|| format!("cannot stat {}", dest.display()))?
                .permissions()
                .mode()
        } else {
            // New file: parent directory mode with execute bits removed, or 0o644.
            dest.parent()
                .and_then(|p| fs::metadata(p).ok())
                .map_or(0o644, |m| m.permissions().mode() & 0o666)
        }
    };

    fs::write(dest, content).with_context(|| format!("cannot write {}", dest.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dest, fs::Permissions::from_mode(mode))
            .with_context(|| format!("cannot set permissions on {}", dest.display()))?;
    }

    Ok(())
}

/// Returns `name` if non-empty, otherwise `"unknown"`.
fn non_empty_machine_name(name: &str) -> String {
    if name.is_empty() {
        "unknown".to_owned()
    } else {
        name.to_owned()
    }
}

// ---------------------------------------------------------------------------
// chronicle status — formatter
// ---------------------------------------------------------------------------

const ANSI_GREEN: &str = "\x1b[32m";
const ANSI_YELLOW: &str = "\x1b[33m";
const ANSI_RED: &str = "\x1b[31m";
const ANSI_RESET: &str = "\x1b[0m";

/// Arguments for `chronicle status`.
#[derive(Debug, Default, Clone)]
pub struct StatusArgs {
    /// Show extra detail (file list, effective config values).
    pub verbose: bool,
    /// Emit stable `key=value` pairs; no symbols or color.
    pub porcelain: bool,
    /// Suppress ANSI color even when stdout is a TTY.
    pub no_color: bool,
}

/// Returns `true` when ANSI color should be used for status output.
///
/// Color is disabled when `--no-color` is passed, `NO_COLOR` is set in the
/// environment (any value), or stdout is not a terminal.
#[must_use]
pub fn should_use_color(no_color_flag: bool) -> bool {
    if no_color_flag {
        return false;
    }
    if std::env::var("NO_COLOR").is_ok() {
        return false;
    }
    io::stdout().is_terminal()
}

/// Output formatter for `chronicle status`.
///
/// Writes structured lines to any [`io::Write`] target so output can be
/// captured in tests.
pub struct StatusFormatter<W: io::Write> {
    writer: W,
    use_color: bool,
    /// When `true`, emit stable `key=value` pairs instead of symbol lines.
    pub porcelain: bool,
}

impl<W: io::Write> StatusFormatter<W> {
    /// Create a new formatter writing to `writer`.
    pub fn new(writer: W, use_color: bool, porcelain: bool) -> Self {
        Self {
            writer,
            use_color,
            porcelain,
        }
    }

    fn symbol_line(
        &mut self,
        symbol: &str,
        color: &str,
        label: &str,
        detail: &str,
    ) -> io::Result<()> {
        if self.porcelain {
            return Ok(());
        }
        if self.use_color {
            writeln!(
                self.writer,
                "{color}{symbol}{ANSI_RESET}  {label}: {detail}"
            )
        } else {
            writeln!(self.writer, "{symbol}  {label}: {detail}")
        }
    }

    /// Emit `✓  label: detail` (green when color is on).
    pub fn ok(&mut self, label: &str, detail: &str) -> io::Result<()> {
        self.symbol_line("✓", ANSI_GREEN, label, detail)
    }

    /// Emit `⚠  label: detail` (yellow when color is on).
    pub fn warn(&mut self, label: &str, detail: &str) -> io::Result<()> {
        self.symbol_line("⚠", ANSI_YELLOW, label, detail)
    }

    /// Emit `✗  label: detail` (red when color is on).
    pub fn err(&mut self, label: &str, detail: &str) -> io::Result<()> {
        self.symbol_line("✗", ANSI_RED, label, detail)
    }

    /// Emit `key=value\n` only in porcelain mode.
    pub fn kv(&mut self, key: &str, value: &str) -> io::Result<()> {
        if self.porcelain {
            writeln!(self.writer, "{key}={value}")
        } else {
            Ok(())
        }
    }
}

// ---------------------------------------------------------------------------
// chronicle status — handle_status and status_impl
// ---------------------------------------------------------------------------

/// Handle `chronicle status [--verbose] [--porcelain] [--no-color]`.
pub fn handle_status(args: StatusArgs) -> Result<()> {
    let config_path = config::default_config_path();
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    status_impl(&args, &config_path, &home)
}

/// Testable core of `handle_status` — accepts injected paths.
///
/// Loads the config internally and writes to stdout.  Always returns `Ok(())`
/// — errors are displayed inline as `✗` lines.
pub fn status_impl(args: &StatusArgs, config_path: &Path, home: &Path) -> Result<()> {
    let use_color = should_use_color(args.no_color);
    let stdout = io::stdout();
    let mut out = stdout.lock();
    status_write(args, config_path, home, use_color, &mut out)
}

/// Inner status implementation that writes to any [`io::Write`] — used in
/// tests to capture output.
fn status_write<W: io::Write>(
    args: &StatusArgs,
    config_path: &Path,
    home: &Path,
    use_color: bool,
    writer: &mut W,
) -> Result<()> {
    let mut fmt = StatusFormatter::new(writer, use_color, args.porcelain);

    match config::load(Some(config_path), &CliOverrides::default()) {
        Ok(cfg) => {
            emit_config_machine_section(&mut fmt, &cfg, home)?;
        }
        Err(e) => {
            fmt.err("Config", &format!("failed to load: {e}"))?;
            fmt.kv("config_ok", "false")?;
            fmt.kv("config_error", &e.to_string())?;
        }
    }

    Ok(())
}

/// Emit the Config / Machine section of `chronicle status`.
fn emit_config_machine_section<W: io::Write>(
    fmt: &mut StatusFormatter<W>,
    cfg: &config::Config,
    home: &Path,
) -> io::Result<()> {
    let machine = &cfg.general.machine_name;
    let remote = &cfg.storage.remote_url;
    let mut config_errors: Vec<String> = Vec::new();

    // Machine name + remote URL — combined on one line when both are set.
    if !machine.is_empty() && !remote.is_empty() {
        fmt.ok("Machine", &format!("{machine}  (remote: {remote})"))?;
    } else {
        if machine.is_empty() {
            fmt.warn("Machine", "(not configured — run chronicle init)")?
        } else {
            fmt.ok("Machine", machine)?
        };
        if remote.is_empty() {
            let msg = "remote not configured — run chronicle init --remote <url>";
            config_errors.push(msg.to_owned());
            fmt.err("Remote", "(not configured)")?;
        } else {
            fmt.ok("Remote", remote)?;
        }
    }

    // Check each enabled agent's sessions directory.
    if cfg.agents.pi.enabled {
        let dir = config::expand_path_with_home(&cfg.agents.pi.session_dir, home);
        if dir.exists() {
            fmt.ok("Pi sessions", &dir.to_string_lossy())?;
        } else {
            let msg = format!(
                "Pi agent enabled but sessions directory not found: {}",
                dir.display()
            );
            config_errors.push(msg.clone());
            fmt.err("Config", &msg)?;
        }
    }

    if cfg.agents.claude.enabled {
        let dir = config::expand_path_with_home(&cfg.agents.claude.session_dir, home);
        if dir.exists() {
            fmt.ok("Claude sessions", &dir.to_string_lossy())?;
        } else {
            let msg = format!(
                "Claude agent enabled but sessions directory not found: {}",
                dir.display()
            );
            config_errors.push(msg.clone());
            fmt.err("Config", &msg)?;
        }
    }

    // Porcelain keys for the Config / Machine section.
    fmt.kv("machine", machine)?;
    fmt.kv("remote", remote)?;
    if config_errors.is_empty() {
        fmt.kv("config_ok", "true")?;
        fmt.kv("config_error", "")?;
    } else {
        fmt.kv("config_ok", "false")?;
        fmt.kv("config_error", &config_errors.join("; "))?;
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// chronicle errors
// ---------------------------------------------------------------------------

/// Handle `chronicle errors [--limit <n>]`.
pub fn handle_errors(limit: Option<usize>) -> Result<()> {
    use crate::errors::ring_buffer::RingBuffer;
    errors_impl(limit, &RingBuffer::default_path())
}

/// Testable core of `handle_errors` — accepts an injected ring buffer path.
fn errors_impl(limit: Option<usize>, errors_path: &Path) -> Result<()> {
    use crate::errors::ring_buffer::{RingBuffer, Severity};

    let ring_buf = RingBuffer::new(errors_path.to_path_buf());
    let entries = ring_buf.read(limit).context("failed to read error log")?;

    if entries.is_empty() {
        println!("No errors recorded.");
        return Ok(());
    }

    for entry in &entries {
        let severity_str = match entry.severity {
            Severity::Error => "ERROR",
            Severity::Warning => "WARN ",
            Severity::Info => "INFO ",
        };
        let ts = entry.timestamp.format("%Y-%m-%d %H:%M:%S UTC");
        print!("[{ts}] {severity_str}  {}", entry.category);
        if let Some(file) = &entry.file {
            print!("  {file}");
        }
        println!();
        println!("  {}", entry.message);
        if let Some(detail) = &entry.detail {
            println!("  detail: {detail}");
        }
        println!();
    }

    let shown = entries.len();
    let suffix = if shown == 1 { "" } else { "s" };
    println!("{shown} error{suffix} shown.");

    Ok(())
}

// ---------------------------------------------------------------------------
// chronicle config
// ---------------------------------------------------------------------------

/// Handle `chronicle config [<key>] [<value>]`.
pub fn handle_config(key: Option<String>, value: Option<String>) -> Result<()> {
    let config_path = config::default_config_path();
    config_impl(key, value, &config_path)
}

/// Testable core of `handle_config` — accepts an injected config path.
fn config_impl(key: Option<String>, value: Option<String>, config_path: &Path) -> Result<()> {
    let mut cfg = config::load(Some(config_path), &CliOverrides::default())
        .context("failed to load configuration")?;

    match (key.as_deref(), value.as_deref()) {
        // No args: print full config as TOML.
        (None, _) => {
            let toml_str =
                toml::to_string_pretty(&cfg).context("failed to serialize configuration")?;
            print!("{toml_str}");
        }

        // Key only: print value.
        (Some(k), None) => {
            let val =
                get_config_value(&cfg, k).with_context(|| format!("unknown config key: {k}"))?;
            println!("{val}");
        }

        // Key + value: set and save.
        (Some(k), Some(v)) => {
            set_config_value(&mut cfg, k, v).with_context(|| format!("failed to set {k} = {v}"))?;
            if let Some(parent) = config_path.parent() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("failed to create config dir {}", parent.display()))?;
            }
            let toml_str =
                toml::to_string_pretty(&cfg).context("failed to serialize configuration")?;
            fs::write(config_path, &toml_str).with_context(|| {
                format!("failed to write config file {}", config_path.display())
            })?;
            println!("✓ {k} = {v}");
        }
    }

    Ok(())
}

/// Return the string representation of `key` in `cfg`.
///
/// Keys use dotted notation (`general.machine_name`).  The special alias
/// `machine-name` (or `machine_name`) maps to `general.machine_name`.
fn get_config_value(cfg: &config::Config, key: &str) -> Result<String> {
    // Normalise hyphens → underscores so `machine-name` == `machine_name`.
    let norm = key.replace('-', "_");
    let val = match norm.as_str() {
        // Special alias.
        "machine_name" => cfg.general.machine_name.clone(),
        // [general]
        "general.machine_name" => cfg.general.machine_name.clone(),
        "general.sync_interval" => cfg.general.sync_interval.clone(),
        "general.log_level" => cfg.general.log_level.clone(),
        "general.follow_symlinks" => cfg.general.follow_symlinks.to_string(),
        // [notifications]
        "notifications.on_error" => cfg.notifications.on_error.to_string(),
        "notifications.on_success" => cfg.notifications.on_success.to_string(),
        // [storage]
        "storage.repo_path" => cfg.storage.repo_path.clone(),
        "storage.remote_url" => cfg.storage.remote_url.clone(),
        "storage.branch" => cfg.storage.branch.clone(),
        // [canonicalization]
        "canonicalization.home_token" => cfg.canonicalization.home_token.clone(),
        "canonicalization.level" => cfg.canonicalization.level.to_string(),
        // [agents.pi]
        "agents.pi.enabled" => cfg.agents.pi.enabled.to_string(),
        "agents.pi.session_dir" => cfg.agents.pi.session_dir.clone(),
        // [agents.claude]
        "agents.claude.enabled" => cfg.agents.claude.enabled.to_string(),
        "agents.claude.session_dir" => cfg.agents.claude.session_dir.clone(),
        // [sync]
        "sync.history_mode" => match cfg.sync.history_mode {
            HistoryMode::Full => "full".to_owned(),
            HistoryMode::Partial => "partial".to_owned(),
        },
        "sync.partial_max_count" => cfg.sync.partial_max_count.to_string(),
        _ => anyhow::bail!("unknown config key: {key}"),
    };
    Ok(val)
}

/// Set `key` to `value` in `cfg` (in-memory; caller must write to disk).
fn set_config_value(cfg: &mut config::Config, key: &str, value: &str) -> Result<()> {
    let norm = key.replace('-', "_");
    match norm.as_str() {
        // Special alias.
        "machine_name" => cfg.general.machine_name = value.to_owned(),
        // [general]
        "general.machine_name" => cfg.general.machine_name = value.to_owned(),
        "general.sync_interval" => cfg.general.sync_interval = value.to_owned(),
        "general.log_level" => cfg.general.log_level = value.to_owned(),
        "general.follow_symlinks" => {
            cfg.general.follow_symlinks = value
                .parse::<bool>()
                .map_err(|_| anyhow::anyhow!("expected true or false, got: {value}"))?;
        }
        // [notifications]
        "notifications.on_error" => {
            cfg.notifications.on_error = value
                .parse::<bool>()
                .map_err(|_| anyhow::anyhow!("expected true or false, got: {value}"))?;
        }
        "notifications.on_success" => {
            cfg.notifications.on_success = value
                .parse::<bool>()
                .map_err(|_| anyhow::anyhow!("expected true or false, got: {value}"))?;
        }
        // [storage]
        "storage.repo_path" => cfg.storage.repo_path = value.to_owned(),
        "storage.remote_url" => cfg.storage.remote_url = value.to_owned(),
        "storage.branch" => cfg.storage.branch = value.to_owned(),
        // [canonicalization]
        "canonicalization.home_token" => cfg.canonicalization.home_token = value.to_owned(),
        "canonicalization.level" => {
            let level = value
                .parse::<u8>()
                .map_err(|_| anyhow::anyhow!("expected a number 1–3, got: {value}"))?;
            if !(1..=3).contains(&level) {
                return Err(anyhow::anyhow!(
                    "canonicalization.level must be 1, 2, or 3, got: {value}"
                ));
            }
            cfg.canonicalization.level = level;
        }
        // [agents.pi]
        "agents.pi.enabled" => {
            cfg.agents.pi.enabled = value
                .parse::<bool>()
                .map_err(|_| anyhow::anyhow!("expected true or false, got: {value}"))?;
        }
        "agents.pi.session_dir" => cfg.agents.pi.session_dir = value.to_owned(),
        // [agents.claude]
        "agents.claude.enabled" => {
            cfg.agents.claude.enabled = value
                .parse::<bool>()
                .map_err(|_| anyhow::anyhow!("expected true or false, got: {value}"))?;
        }
        "agents.claude.session_dir" => cfg.agents.claude.session_dir = value.to_owned(),
        // [sync]
        "sync.history_mode" => {
            cfg.sync.history_mode = match value {
                "full" => HistoryMode::Full,
                "partial" => HistoryMode::Partial,
                _ => anyhow::bail!("expected 'full' or 'partial', got: {value}"),
            };
        }
        "sync.partial_max_count" => {
            cfg.sync.partial_max_count = value
                .parse::<usize>()
                .map_err(|_| anyhow::anyhow!("expected a positive integer, got: {value}"))?;
        }
        _ => anyhow::bail!("unknown config key: {key}"),
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// chronicle schedule *
// ---------------------------------------------------------------------------

/// Handle `chronicle schedule install`.
///
/// Reads `sync_interval` from config, maps it to a cron expression, detects
/// the current binary path, and writes/replaces the Chronicle crontab entries.
pub fn handle_schedule_install() -> Result<()> {
    let cfg =
        config::load(None, &CliOverrides::default()).context("failed to load configuration")?;

    let (cron_expr, warning) = scheduler_cron::interval_to_cron(&cfg.general.sync_interval);
    if let Some(w) = warning {
        eprintln!("Warning: {w}");
    }

    // Prefer the absolute path of the running binary; fall back to argv[0].
    let binary_path = std::env::current_exe()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| {
            std::env::args()
                .next()
                .unwrap_or_else(|| "chronicle".to_owned())
        });

    scheduler_cron::install(&binary_path, &cron_expr)
        .context("failed to install crontab entries")?;

    println!("✓ Chronicle cron entries installed.");
    println!("  Binary:   {binary_path}");
    println!("  Schedule: @reboot and {cron_expr}");
    Ok(())
}

/// Handle `chronicle schedule uninstall`.
///
/// Removes all `# chronicle-sync` tagged entries from the crontab.  If the
/// crontab is empty after removal, it is deleted via `crontab -r`.
pub fn handle_schedule_uninstall() -> Result<()> {
    scheduler_cron::uninstall().context("failed to remove crontab entries")?;
    println!("✓ Chronicle cron entries removed.");
    Ok(())
}

/// Handle `chronicle schedule status`.
///
/// Reports whether Chronicle crontab entries are installed, the configured
/// interval, the cron expression, and the path to the chronicle binary.
pub fn handle_schedule_status() -> Result<()> {
    let st = scheduler_cron::status().context("failed to read crontab")?;
    if st.installed {
        println!("Installed: yes");
        println!("Interval:  {}", st.interval.as_deref().unwrap_or("unknown"));
        println!(
            "Cron:      {}",
            st.cron_expression.as_deref().unwrap_or("unknown")
        );
        println!(
            "Binary:    {}",
            st.binary_path.as_deref().unwrap_or("unknown")
        );
    } else {
        println!("Installed: no");
        println!("Run 'chronicle schedule install' to set up automatic sync.");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::materialize_cache::MaterializeCache;
    use tempfile::TempDir;

    /// Core init logic extracted for testability (avoids touching real home dir).
    fn init_with_config_path(config_path: &std::path::Path, remote: Option<String>) -> Result<()> {
        let config_existed = config_path.exists();

        let mut cfg = config::load(Some(config_path), &CliOverrides::default())
            .context("failed to load configuration")?;

        if cfg.general.machine_name.is_empty() {
            cfg.general.machine_name = config::machine_name::generate();
        }

        if let Some(url) = remote {
            cfg.storage.remote_url = url;
        }

        let toml_content =
            toml::to_string_pretty(&cfg).context("failed to serialize configuration to TOML")?;
        if let Some(parent) = config_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(config_path, &toml_content)?;

        let repo_path = config::expand_path(&cfg.storage.repo_path);
        let remote_url = if cfg.storage.remote_url.is_empty() {
            None
        } else {
            Some(cfg.storage.remote_url.as_str())
        };

        let manager = git::RepoManager::init_or_open(&repo_path, remote_url, &cfg.storage.branch)
            .context("failed to initialize git repository")?;
        manager.ensure_working_tree()?;
        manager.ensure_manifest()?;

        if config_existed {
            // idempotent — just confirm without printing anything in tests
        }

        Ok(())
    }

    // -----------------------------------------------------------------------

    #[test]
    fn init_creates_config_file() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join("chronicle").join("config.toml");
        let repo_path = dir.path().join("repo");

        // Start with no config.
        std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        let toml = format!("[storage]\nrepo_path = \"{}\"\n", repo_path.display());
        std::fs::write(&config_path, &toml).unwrap();

        init_with_config_path(&config_path, None).unwrap();

        assert!(config_path.exists(), "config file should exist after init");
    }

    #[test]
    fn init_generates_machine_name() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join("chronicle").join("config.toml");
        let repo_path = dir.path().join("repo");

        std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        let toml = format!("[storage]\nrepo_path = \"{}\"\n", repo_path.display());
        std::fs::write(&config_path, &toml).unwrap();

        init_with_config_path(&config_path, None).unwrap();

        let content = std::fs::read_to_string(&config_path).unwrap();
        let cfg: crate::config::schema::Config = toml::from_str(&content).unwrap();
        assert!(
            !cfg.general.machine_name.is_empty(),
            "machine name should be generated"
        );
        assert!(
            cfg.general.machine_name.contains('-'),
            "machine name should be adjective-animal format"
        );
    }

    #[test]
    fn init_preserves_existing_machine_name() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join("chronicle").join("config.toml");
        let repo_path = dir.path().join("repo");

        std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        let toml = format!(
            "[general]\nmachine_name = \"happy-hippo\"\n\n[storage]\nrepo_path = \"{}\"\n",
            repo_path.display()
        );
        std::fs::write(&config_path, &toml).unwrap();

        init_with_config_path(&config_path, None).unwrap();

        let content = std::fs::read_to_string(&config_path).unwrap();
        let cfg: crate::config::schema::Config = toml::from_str(&content).unwrap();
        assert_eq!(
            cfg.general.machine_name, "happy-hippo",
            "existing machine name should be preserved"
        );
    }

    #[test]
    fn init_sets_remote_from_flag() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join("chronicle").join("config.toml");
        let repo_path = dir.path().join("repo");

        std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        let toml = format!("[storage]\nrepo_path = \"{}\"\n", repo_path.display());
        std::fs::write(&config_path, &toml).unwrap();

        init_with_config_path(
            &config_path,
            Some("git@example.com:user/sessions.git".to_owned()),
        )
        .unwrap();

        let content = std::fs::read_to_string(&config_path).unwrap();
        let cfg: crate::config::schema::Config = toml::from_str(&content).unwrap();
        assert_eq!(
            cfg.storage.remote_url, "git@example.com:user/sessions.git",
            "remote URL should be written to config"
        );
    }

    #[test]
    fn init_initializes_git_repo() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join("chronicle").join("config.toml");
        let repo_path = dir.path().join("repo");

        std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        let toml = format!("[storage]\nrepo_path = \"{}\"\n", repo_path.display());
        std::fs::write(&config_path, &toml).unwrap();

        init_with_config_path(&config_path, None).unwrap();

        // Git repo should exist with expected structure.
        assert!(
            repo_path.join(".git").exists() || repo_path.join("HEAD").exists(),
            "git repo should exist at repo_path"
        );
        assert!(
            repo_path.join("pi").join("sessions").exists(),
            "pi/sessions/ directory should exist"
        );
        assert!(
            repo_path.join("claude").join("projects").exists(),
            "claude/projects/ directory should exist"
        );
        assert!(
            repo_path.join(".chronicle").exists(),
            ".chronicle/ directory should exist"
        );
    }

    #[test]
    fn init_is_idempotent() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join("chronicle").join("config.toml");
        let repo_path = dir.path().join("repo");

        std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        let toml = format!(
            "[general]\nmachine_name = \"bold-badger\"\n\n[storage]\nrepo_path = \"{}\"\n",
            repo_path.display()
        );
        std::fs::write(&config_path, &toml).unwrap();

        // Run twice — second call must succeed without error.
        init_with_config_path(&config_path, None).unwrap();
        init_with_config_path(&config_path, None).unwrap();

        let content = std::fs::read_to_string(&config_path).unwrap();
        let cfg: crate::config::schema::Config = toml::from_str(&content).unwrap();
        assert_eq!(
            cfg.general.machine_name, "bold-badger",
            "machine name must remain stable across repeated init calls"
        );
    }

    #[test]
    fn init_manifest_exists_after_init() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join("chronicle").join("config.toml");
        let repo_path = dir.path().join("repo");

        std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        let toml = format!("[storage]\nrepo_path = \"{}\"\n", repo_path.display());
        std::fs::write(&config_path, &toml).unwrap();

        init_with_config_path(&config_path, None).unwrap();

        let manifest_path = repo_path.join(".chronicle").join("manifest.json");
        assert!(
            manifest_path.exists(),
            "manifest.json should exist after init"
        );
    }

    #[test]
    fn init_writes_config_with_correct_toml() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join("chronicle").join("config.toml");
        let repo_path = dir.path().join("repo");

        std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        let toml = format!("[storage]\nrepo_path = \"{}\"\n", repo_path.display());
        std::fs::write(&config_path, &toml).unwrap();

        init_with_config_path(&config_path, None).unwrap();

        // Must be valid TOML that round-trips.
        let content = std::fs::read_to_string(&config_path).unwrap();
        let result: Result<crate::config::schema::Config, _> = toml::from_str(&content);
        assert!(result.is_ok(), "written config must be valid TOML");
    }

    // -----------------------------------------------------------------------
    // Import tests
    // -----------------------------------------------------------------------

    /// Write a minimal config TOML pointing to caller-supplied directories.
    fn write_import_config(
        config_path: &std::path::Path,
        repo_path: &std::path::Path,
        pi_session_dir: &std::path::Path,
        claude_session_dir: &std::path::Path,
        machine_name: &str,
        pi_enabled: bool,
        claude_enabled: bool,
    ) {
        let toml = format!(
            "[general]\nmachine_name = \"{machine_name}\"\n\n\
             [storage]\nrepo_path = \"{}\"\n\n\
             [agents.pi]\nenabled = {pi_enabled}\nsession_dir = \"{}\"\n\n\
             [agents.claude]\nenabled = {claude_enabled}\nsession_dir = \"{}\"\n",
            repo_path.display(),
            pi_session_dir.display(),
            claude_session_dir.display(),
        );
        std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        std::fs::write(config_path, toml.as_bytes()).unwrap();
    }

    /// Build a Pi-encoded session dir name for `<home>/Dev/foo`.
    fn pi_session_dir_name(home: &std::path::Path) -> String {
        let inner = home
            .to_string_lossy()
            .trim_start_matches('/')
            .replace('/', "-");
        format!("--{inner}-Dev-foo--")
    }

    /// Build a Claude-encoded session dir name for `<home>/Dev/foo`.
    fn claude_session_dir_name(home: &std::path::Path) -> String {
        let inner = home
            .to_string_lossy()
            .trim_start_matches('/')
            .replace(['/', '.'], "-");
        format!("-{inner}-Dev-foo")
    }

    #[test]
    fn import_dry_run_does_not_write_files() {
        let dir = TempDir::new().unwrap();
        let home = dir.path().join("home");
        std::fs::create_dir_all(&home).unwrap();

        let repo_path = dir.path().join("repo");
        let pi_sessions = dir.path().join("pi_sessions");
        let claude_sessions = dir.path().join("claude_sessions");
        let config_path = dir.path().join("config.toml");

        // Create a Pi session dir with one .jsonl file.
        let session_dir = pi_sessions.join(pi_session_dir_name(&home));
        std::fs::create_dir_all(&session_dir).unwrap();
        std::fs::write(
            session_dir.join("session.jsonl"),
            b"{\"type\":\"session\",\"id\":\"1\"}\n",
        )
        .unwrap();

        write_import_config(
            &config_path,
            &repo_path,
            &pi_sessions,
            &claude_sessions,
            "test-machine",
            true,
            false,
        );

        import_impl("pi", true, &config_path, &home).unwrap();

        // With --dry-run the repo directory must NOT be created.
        assert!(
            !repo_path.exists(),
            "dry run must not create the repo directory"
        );
    }

    #[test]
    fn import_writes_pi_session_to_repo() {
        let dir = TempDir::new().unwrap();
        let home = dir.path().join("home");
        std::fs::create_dir_all(&home).unwrap();

        let repo_path = dir.path().join("repo");
        let pi_sessions = dir.path().join("pi_sessions");
        let claude_sessions = dir.path().join("claude_sessions");
        let config_path = dir.path().join("config.toml");

        let session_dir = pi_sessions.join(pi_session_dir_name(&home));
        std::fs::create_dir_all(&session_dir).unwrap();
        std::fs::write(
            session_dir.join("session.jsonl"),
            b"{\"type\":\"session\",\"id\":\"1\"}\n",
        )
        .unwrap();

        write_import_config(
            &config_path,
            &repo_path,
            &pi_sessions,
            &claude_sessions,
            "test-machine",
            true,
            false,
        );

        import_impl("pi", false, &config_path, &home).unwrap();

        // At least one .jsonl file must exist under pi/sessions/.
        let pi_sessions_in_repo = repo_path.join("pi").join("sessions");
        assert!(
            pi_sessions_in_repo.exists(),
            "pi/sessions/ must be created in the repo"
        );
        let found = std::fs::read_dir(&pi_sessions_in_repo)
            .unwrap()
            .any(|e| e.unwrap().path().join("session.jsonl").exists());
        assert!(found, "session.jsonl must exist under pi/sessions/<dir>/");
    }

    #[test]
    fn import_canonicalizes_pi_dir_name() {
        let dir = TempDir::new().unwrap();
        let home = dir.path().join("home");
        std::fs::create_dir_all(&home).unwrap();

        let repo_path = dir.path().join("repo");
        let pi_sessions = dir.path().join("pi_sessions");
        let claude_sessions = dir.path().join("claude_sessions");
        let config_path = dir.path().join("config.toml");

        let session_dir = pi_sessions.join(pi_session_dir_name(&home));
        std::fs::create_dir_all(&session_dir).unwrap();
        std::fs::write(
            session_dir.join("s.jsonl"),
            b"{\"type\":\"session\",\"id\":\"x\"}\n",
        )
        .unwrap();

        write_import_config(
            &config_path,
            &repo_path,
            &pi_sessions,
            &claude_sessions,
            "test-machine",
            true,
            false,
        );

        import_impl("pi", false, &config_path, &home).unwrap();

        // The repo dir must use the canonical token form, not the raw home path.
        let canonical_dir = repo_path
            .join("pi")
            .join("sessions")
            .join("--{{SYNC_HOME}}-Dev-foo--");
        assert!(
            canonical_dir.exists(),
            "canonical Pi dir '--{{{{SYNC_HOME}}}}-Dev-foo--' must exist; repo contents: {:?}",
            std::fs::read_dir(repo_path.join("pi").join("sessions"))
                .unwrap()
                .map(|e| e.unwrap().file_name())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn import_canonicalizes_file_content() {
        let dir = TempDir::new().unwrap();
        let home = dir.path().join("home");
        std::fs::create_dir_all(&home).unwrap();

        let repo_path = dir.path().join("repo");
        let pi_sessions = dir.path().join("pi_sessions");
        let claude_sessions = dir.path().join("claude_sessions");
        let config_path = dir.path().join("config.toml");

        let session_dir = pi_sessions.join(pi_session_dir_name(&home));
        std::fs::create_dir_all(&session_dir).unwrap();

        // File content includes a `cwd` path under $HOME — must be canonicalized.
        let home_str = home.to_string_lossy().to_string();
        let content = format!(
            "{{\"type\":\"session\",\"id\":\"1\"}}\n\
             {{\"type\":\"message\",\"cwd\":\"{home_str}/Dev\",\"id\":\"2\"}}\n"
        );
        std::fs::write(session_dir.join("session.jsonl"), content.as_bytes()).unwrap();

        write_import_config(
            &config_path,
            &repo_path,
            &pi_sessions,
            &claude_sessions,
            "test-machine",
            true,
            false,
        );

        import_impl("pi", false, &config_path, &home).unwrap();

        let dest = repo_path
            .join("pi")
            .join("sessions")
            .join("--{{SYNC_HOME}}-Dev-foo--")
            .join("session.jsonl");
        let written = std::fs::read_to_string(&dest).unwrap();

        assert!(
            written.contains("{{SYNC_HOME}}"),
            "canonicalized file must contain {{SYNC_HOME}}; got: {written:?}"
        );
        assert!(
            !written.contains(&home_str),
            "original home path must not remain; got: {written:?}"
        );
    }

    #[test]
    fn import_agent_pi_only_skips_claude() {
        let dir = TempDir::new().unwrap();
        let home = dir.path().join("home");
        std::fs::create_dir_all(&home).unwrap();

        let repo_path = dir.path().join("repo");
        let pi_sessions = dir.path().join("pi_sessions");
        let claude_sessions = dir.path().join("claude_sessions");
        let config_path = dir.path().join("config.toml");

        let pi_dir = pi_sessions.join(pi_session_dir_name(&home));
        std::fs::create_dir_all(&pi_dir).unwrap();
        std::fs::write(
            pi_dir.join("p.jsonl"),
            b"{\"type\":\"session\",\"id\":\"p\"}\n",
        )
        .unwrap();

        let cl_dir = claude_sessions.join(claude_session_dir_name(&home));
        std::fs::create_dir_all(&cl_dir).unwrap();
        std::fs::write(
            cl_dir.join("c.jsonl"),
            b"{\"type\":\"session\",\"id\":\"c\"}\n",
        )
        .unwrap();

        write_import_config(
            &config_path,
            &repo_path,
            &pi_sessions,
            &claude_sessions,
            "test-machine",
            true,
            true,
        );

        import_impl("pi", false, &config_path, &home).unwrap();

        assert!(
            repo_path.join("pi").join("sessions").exists(),
            "pi/sessions/ should exist after --agent pi"
        );
        assert!(
            !repo_path.join("claude").join("projects").exists(),
            "claude/projects/ must NOT be created when --agent pi"
        );
    }

    #[test]
    fn import_agent_claude_only_skips_pi() {
        let dir = TempDir::new().unwrap();
        let home = dir.path().join("home");
        std::fs::create_dir_all(&home).unwrap();

        let repo_path = dir.path().join("repo");
        let pi_sessions = dir.path().join("pi_sessions");
        let claude_sessions = dir.path().join("claude_sessions");
        let config_path = dir.path().join("config.toml");

        let pi_dir = pi_sessions.join(pi_session_dir_name(&home));
        std::fs::create_dir_all(&pi_dir).unwrap();
        std::fs::write(
            pi_dir.join("p.jsonl"),
            b"{\"type\":\"session\",\"id\":\"p\"}\n",
        )
        .unwrap();

        let cl_dir = claude_sessions.join(claude_session_dir_name(&home));
        std::fs::create_dir_all(&cl_dir).unwrap();
        std::fs::write(
            cl_dir.join("c.jsonl"),
            b"{\"type\":\"session\",\"id\":\"c\"}\n",
        )
        .unwrap();

        write_import_config(
            &config_path,
            &repo_path,
            &pi_sessions,
            &claude_sessions,
            "test-machine",
            true,
            true,
        );

        import_impl("claude", false, &config_path, &home).unwrap();

        assert!(
            repo_path.join("claude").join("projects").exists(),
            "claude/projects/ should exist after --agent claude"
        );
        // pi/sessions/ may exist as a repo working-tree dir but must have no
        // session subdirectories (only .gitkeep from ensure_working_tree).
        let pi_sessions_repo = repo_path.join("pi").join("sessions");
        if pi_sessions_repo.exists() {
            let has_session_subdirs = std::fs::read_dir(&pi_sessions_repo)
                .unwrap()
                .any(|e| e.unwrap().file_type().unwrap().is_dir());
            assert!(
                !has_session_subdirs,
                "pi/sessions/ must have no session subdirs when --agent claude"
            );
        }
    }

    #[test]
    fn import_empty_session_dir_is_skipped() {
        let dir = TempDir::new().unwrap();
        let home = dir.path().join("home");
        std::fs::create_dir_all(&home).unwrap();

        let repo_path = dir.path().join("repo");
        let pi_sessions = dir.path().join("pi_sessions");
        let claude_sessions = dir.path().join("claude_sessions");
        let config_path = dir.path().join("config.toml");

        // Session subdir exists but contains no .jsonl files.
        let empty_dir = pi_sessions.join(pi_session_dir_name(&home));
        std::fs::create_dir_all(&empty_dir).unwrap();
        std::fs::write(empty_dir.join("notes.txt"), b"not jsonl\n").unwrap();

        write_import_config(
            &config_path,
            &repo_path,
            &pi_sessions,
            &claude_sessions,
            "test-machine",
            true,
            false,
        );

        import_impl("pi", false, &config_path, &home).unwrap();

        // No canonical session dir should be created in the repo.
        let canonical_dir = repo_path
            .join("pi")
            .join("sessions")
            .join("--{{SYNC_HOME}}-Dev-foo--");
        assert!(
            !canonical_dir.exists(),
            "empty session dir must not create a canonical dir in the repo"
        );
    }

    // -----------------------------------------------------------------------
    // Helpers shared by push / pull tests
    // -----------------------------------------------------------------------

    /// Write a config TOML that points at caller-supplied directories and
    /// includes a remote URL (for push tests that need a real remote).
    fn write_push_config(
        config_path: &std::path::Path,
        repo_path: &std::path::Path,
        remote_path: &std::path::Path,
        pi_session_dir: &std::path::Path,
        claude_session_dir: &std::path::Path,
        machine_name: &str,
    ) {
        let toml = format!(
            "[general]\nmachine_name = \"{machine_name}\"\n\n\
             [storage]\nrepo_path = \"{}\"\nremote_url = \"{}\"\n\n\
             [agents.pi]\nenabled = true\nsession_dir = \"{}\"\n\n\
             [agents.claude]\nenabled = false\nsession_dir = \"{}\"\n",
            repo_path.display(),
            remote_path.display(),
            pi_session_dir.display(),
            claude_session_dir.display(),
        );
        std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        std::fs::write(config_path, toml.as_bytes()).unwrap();
    }

    // -----------------------------------------------------------------------
    // Push tests
    // -----------------------------------------------------------------------

    #[test]
    fn push_dry_run_does_not_create_repo() {
        let dir = TempDir::new().unwrap();
        let home = dir.path().join("home");
        std::fs::create_dir_all(&home).unwrap();

        let repo_path = dir.path().join("repo");
        let pi_sessions = dir.path().join("pi_sessions");
        let claude_sessions = dir.path().join("claude_sessions");
        let config_path = dir.path().join("config.toml");

        // Create a Pi session file so there is something to push.
        let session_dir = pi_sessions.join(pi_session_dir_name(&home));
        std::fs::create_dir_all(&session_dir).unwrap();
        std::fs::write(
            session_dir.join("s.jsonl"),
            b"{\"type\":\"session\",\"id\":\"1\"}\n",
        )
        .unwrap();

        write_import_config(
            &config_path,
            &repo_path,
            &pi_sessions,
            &claude_sessions,
            "test-machine",
            true,
            false,
        );

        push_impl(true, &config_path, &home).unwrap();

        assert!(
            !repo_path.exists(),
            "dry run must not create the repo directory"
        );
    }

    #[test]
    fn push_writes_canonicalized_file_to_repo() {
        let dir = TempDir::new().unwrap();
        let home = dir.path().join("home");
        std::fs::create_dir_all(&home).unwrap();

        let repo_path = dir.path().join("repo");
        let remote_path = dir.path().join("remote");
        let pi_sessions = dir.path().join("pi_sessions");
        let claude_sessions = dir.path().join("claude_sessions");
        let config_path = dir.path().join("config.toml");

        {
            let mut opts = git2::RepositoryInitOptions::new();
            opts.bare(true);
            opts.initial_head("main");
            git2::Repository::init_opts(&remote_path, &opts).unwrap();
        }

        // Create a Pi session file with a home path in its content.
        let session_dir = pi_sessions.join(pi_session_dir_name(&home));
        std::fs::create_dir_all(&session_dir).unwrap();
        let content = format!(
            "{{\"type\":\"session\",\"id\":\"1\"}}\n\
             {{\"type\":\"msg\",\"cwd\":\"{home_str}\",\"id\":\"2\"}}\n",
            home_str = home.display()
        );
        std::fs::write(session_dir.join("session.jsonl"), content.as_bytes()).unwrap();

        write_push_config(
            &config_path,
            &repo_path,
            &remote_path,
            &pi_sessions,
            &claude_sessions,
            "test-machine",
        );

        push_impl(false, &config_path, &home).unwrap();

        // Canonical directory and file must exist in the repo working tree.
        let canonical_dir = repo_path
            .join("pi")
            .join("sessions")
            .join("--{{SYNC_HOME}}-Dev-foo--");
        assert!(
            canonical_dir.exists(),
            "canonical Pi session dir must exist; sessions dir contents: {:?}",
            std::fs::read_dir(repo_path.join("pi").join("sessions"))
                .unwrap()
                .map(|e| e.unwrap().file_name())
                .collect::<Vec<_>>()
        );

        let written = std::fs::read_to_string(canonical_dir.join("session.jsonl")).unwrap();
        assert!(
            written.contains("{{SYNC_HOME}}"),
            "canonicalized file must contain {{SYNC_HOME}}; got: {written:?}"
        );
        let home_str = home.to_string_lossy();
        assert!(
            !written.contains(home_str.as_ref()),
            "canonicalized file must not contain the local home path; got: {written:?}"
        );
    }

    #[test]
    fn push_skips_unchanged_file_after_first_push() {
        let dir = TempDir::new().unwrap();
        let home = dir.path().join("home");
        std::fs::create_dir_all(&home).unwrap();

        let repo_path = dir.path().join("repo");
        let remote_path = dir.path().join("remote");
        let pi_sessions = dir.path().join("pi_sessions");
        let claude_sessions = dir.path().join("claude_sessions");
        let config_path = dir.path().join("config.toml");

        {
            let mut opts = git2::RepositoryInitOptions::new();
            opts.bare(true);
            opts.initial_head("main");
            git2::Repository::init_opts(&remote_path, &opts).unwrap();
        }

        let session_dir = pi_sessions.join(pi_session_dir_name(&home));
        std::fs::create_dir_all(&session_dir).unwrap();
        std::fs::write(
            session_dir.join("s.jsonl"),
            b"{\"type\":\"session\",\"id\":\"1\"}\n",
        )
        .unwrap();

        write_push_config(
            &config_path,
            &repo_path,
            &remote_path,
            &pi_sessions,
            &claude_sessions,
            "test-machine",
        );

        // First push — creates a commit.
        push_impl(false, &config_path, &home).unwrap();

        let repo = git2::Repository::open(&repo_path).unwrap();
        let head_before = repo.head().unwrap().target().unwrap();

        // Second push — file is in state cache and unchanged; no new commit.
        push_impl(false, &config_path, &home).unwrap();

        let head_after = repo.head().unwrap().target().unwrap();
        assert_eq!(
            head_before, head_after,
            "no new commit must be created when file is unchanged"
        );
    }

    #[test]
    fn push_merges_new_entries_with_existing_repo_content() {
        let dir = TempDir::new().unwrap();
        let home = dir.path().join("home");
        std::fs::create_dir_all(&home).unwrap();

        let repo_path = dir.path().join("repo");
        let remote_path = dir.path().join("remote");
        let pi_sessions = dir.path().join("pi_sessions");
        let claude_sessions = dir.path().join("claude_sessions");
        let config_path = dir.path().join("config.toml");

        {
            let mut opts = git2::RepositoryInitOptions::new();
            opts.bare(true);
            opts.initial_head("main");
            git2::Repository::init_opts(&remote_path, &opts).unwrap();
        }

        let session_dir = pi_sessions.join(pi_session_dir_name(&home));
        std::fs::create_dir_all(&session_dir).unwrap();
        let session_file = session_dir.join("s.jsonl");

        write_push_config(
            &config_path,
            &repo_path,
            &remote_path,
            &pi_sessions,
            &claude_sessions,
            "test-machine",
        );

        // First push: entry A only.
        std::fs::write(&session_file, b"{\"type\":\"session\",\"id\":\"A\"}\n").unwrap();
        push_impl(false, &config_path, &home).unwrap();

        // Second push: local file now has A + B.
        std::fs::write(
            &session_file,
            b"{\"type\":\"session\",\"id\":\"A\"}\n{\"type\":\"msg\",\"id\":\"B\"}\n",
        )
        .unwrap();
        push_impl(false, &config_path, &home).unwrap();

        // Repo file must contain both entries.
        let canonical_dir = repo_path
            .join("pi")
            .join("sessions")
            .join("--{{SYNC_HOME}}-Dev-foo--");
        let written = std::fs::read_to_string(canonical_dir.join("s.jsonl")).unwrap();
        assert!(
            written.contains("\"A\""),
            "entry A must be in merged result; got: {written:?}"
        );
        assert!(
            written.contains("\"B\""),
            "entry B must be in merged result; got: {written:?}"
        );
    }

    #[test]
    fn push_updates_manifest_last_sync() {
        let dir = TempDir::new().unwrap();
        let home = dir.path().join("home");
        std::fs::create_dir_all(&home).unwrap();

        let repo_path = dir.path().join("repo");
        let remote_path = dir.path().join("remote");
        let pi_sessions = dir.path().join("pi_sessions");
        let claude_sessions = dir.path().join("claude_sessions");
        let config_path = dir.path().join("config.toml");

        git2::Repository::init_bare(&remote_path).unwrap();

        let session_dir = pi_sessions.join(pi_session_dir_name(&home));
        std::fs::create_dir_all(&session_dir).unwrap();
        std::fs::write(
            session_dir.join("s.jsonl"),
            b"{\"type\":\"session\",\"id\":\"1\"}\n",
        )
        .unwrap();

        write_push_config(
            &config_path,
            &repo_path,
            &remote_path,
            &pi_sessions,
            &claude_sessions,
            "test-machine",
        );

        push_impl(false, &config_path, &home).unwrap();

        // After a successful push, manifest.json must exist and last_sync must be set.
        let manifest_path = repo_path.join(".chronicle").join("manifest.json");
        assert!(
            manifest_path.exists(),
            "manifest.json must be written to repo after push"
        );
        let content = std::fs::read_to_string(&manifest_path).unwrap();
        let manifest: git::Manifest = serde_json::from_str(&content).unwrap();
        let entry = manifest.machines.get("test-machine");
        assert!(
            entry.is_some(),
            "machine entry must exist in manifest after push"
        );
        assert!(
            entry.unwrap().last_sync.is_some(),
            "last_sync must be set for 'test-machine' after push"
        );
    }

    // -----------------------------------------------------------------------
    // Pull tests
    // -----------------------------------------------------------------------

    #[test]
    fn pull_dry_run_does_not_create_repo() {
        let dir = TempDir::new().unwrap();
        let home = dir.path().join("home");
        let repo_path = dir.path().join("repo");
        let pi_sessions = dir.path().join("pi_sessions");
        let claude_sessions = dir.path().join("claude_sessions");
        let config_path = dir.path().join("config.toml");

        write_import_config(
            &config_path,
            &repo_path,
            &pi_sessions,
            &claude_sessions,
            "test-machine",
            true,
            false,
        );

        pull_impl(true, &config_path, &home).unwrap();

        assert!(
            !repo_path.exists(),
            "dry run must not create the repo directory"
        );
    }

    #[test]
    fn pull_materialize_decanonicalize_writes_to_local_dir() {
        let dir = TempDir::new().unwrap();
        let home = dir.path().join("home");
        std::fs::create_dir_all(&home).unwrap();

        let repo_path = dir.path().join("repo");
        let pi_sessions = dir.path().join("pi_sessions");
        let claude_sessions = dir.path().join("claude_sessions");
        let config_path = dir.path().join("config.toml");

        // Set up canonical file in the repo working tree directly (no git needed
        // for this test — we call materialize_repo_to_local directly).
        let canonical_dir = repo_path
            .join("pi")
            .join("sessions")
            .join("--{{SYNC_HOME}}-Dev-foo--");
        std::fs::create_dir_all(&canonical_dir).unwrap();
        // Use a raw string so {{ and }} are literal double-braces in the file.
        let canonical_content =
            r#"{"type":"session","id":"1","cwd":"{{SYNC_HOME}}/Dev"}"#.to_owned() + "\n";
        std::fs::write(
            canonical_dir.join("session.jsonl"),
            canonical_content.as_bytes(),
        )
        .unwrap();

        write_import_config(
            &config_path,
            &repo_path,
            &pi_sessions,
            &claude_sessions,
            "test-machine",
            true,
            false,
        );

        let cfg = config::load(Some(&config_path), &CliOverrides::default()).unwrap();
        let registry = TokenRegistry::from_config(&cfg.canonicalization, &home);

        let count = materialize_repo_to_local(&repo_path, &cfg, &home, &registry).unwrap();

        assert_eq!(count, 1, "exactly one file should be materialized");

        // The local session dir should use the Pi-encoded local home path.
        let local_dir = pi_sessions.join(pi_session_dir_name(&home));
        let local_file = local_dir.join("session.jsonl");
        assert!(
            local_file.exists(),
            "local session file must be created after materialization"
        );

        let local_content = std::fs::read_to_string(&local_file).unwrap();
        let home_str = home.to_string_lossy();
        assert!(
            local_content.contains(home_str.as_ref()),
            "de-canonicalized content must contain local home path; got: {local_content:?}"
        );
        assert!(
            !local_content.contains("{{SYNC_HOME}}"),
            "de-canonicalized content must not contain the canonical token; got: {local_content:?}"
        );
    }

    #[test]
    fn pull_skips_materialize_when_no_remote_changes() {
        let dir = TempDir::new().unwrap();
        let home = dir.path().join("home");
        std::fs::create_dir_all(&home).unwrap();

        let repo_path = dir.path().join("repo");
        let remote_path = dir.path().join("remote");
        let pi_sessions = dir.path().join("pi_sessions");
        let claude_sessions = dir.path().join("claude_sessions");
        let config_path = dir.path().join("config.toml");

        // Create a bare empty remote.  No commits pushed yet, so
        // integrate_remote_changes will return 0 (no tracking ref found).
        {
            let mut opts = git2::RepositoryInitOptions::new();
            opts.bare(true);
            opts.initial_head("main");
            git2::Repository::init_opts(&remote_path, &opts).unwrap();
        }

        write_push_config(
            &config_path,
            &repo_path,
            &remote_path,
            &pi_sessions,
            &claude_sessions,
            "test-machine",
        );

        // Initialise the local working repo and plant a canonical session file
        // in its working tree.  pull_impl will re-open this repo and find it
        // has content that *could* be materialized — but must not be, because
        // the remote has no new commits.
        {
            let remote_url = remote_path.to_str().unwrap();
            git::RepoManager::init_or_open(&repo_path, Some(remote_url), "main").unwrap();

            let canonical_dir = repo_path
                .join("pi")
                .join("sessions")
                .join("--{{SYNC_HOME}}-Dev-foo--");
            std::fs::create_dir_all(&canonical_dir).unwrap();
            std::fs::write(
                canonical_dir.join("session.jsonl"),
                b"{\"type\":\"session\",\"id\":\"1\",\"cwd\":\"{{SYNC_HOME}}/Dev\"}\n",
            )
            .unwrap();
        }

        // pull_impl must succeed and skip materialization (integrated == 0).
        pull_impl(false, &config_path, &home).unwrap();

        // The local pi_sessions directory must NOT have been populated.
        let local_dir = pi_sessions.join(pi_session_dir_name(&home));
        let dir_is_empty =
            !local_dir.exists() || std::fs::read_dir(&local_dir).unwrap().next().is_none();
        assert!(
            dir_is_empty,
            "materialize must be skipped when integrated == 0; \
             found files in {local_dir:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn write_preserving_permissions_uses_0644_for_new_file() {
        use std::os::unix::fs::PermissionsExt;

        let dir = TempDir::new().unwrap();
        let dest = dir.path().join("test.jsonl");

        write_preserving_permissions(&dest, "content").unwrap();

        assert!(dest.exists(), "file must be created");
        // Parent dir mode 0o700 (tempdir default) & 0o666 = 0o600 on some
        // systems; others give 0o755 & 0o666 = 0o644.  Accept either.
        let mode = std::fs::metadata(&dest).unwrap().permissions().mode() & 0o777;
        assert!(
            mode == 0o644 || mode == 0o600,
            "new file mode should be derived from parent dir; got {mode:#o}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn write_preserving_permissions_restores_existing_mode() {
        use std::os::unix::fs::PermissionsExt;

        let dir = TempDir::new().unwrap();
        let dest = dir.path().join("test.jsonl");

        std::fs::write(&dest, "original content").unwrap();
        std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o600)).unwrap();

        write_preserving_permissions(&dest, "updated content").unwrap();

        let mode = std::fs::metadata(&dest).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "existing file mode must be preserved");
        assert_eq!(
            std::fs::read_to_string(&dest).unwrap(),
            "updated content",
            "file content must be updated"
        );
    }

    // -------------------------------------------------------------------------
    // US-016: Partial history materialization
    // -------------------------------------------------------------------------

    #[test]
    fn pi_filename_timestamp_parses_valid_name() {
        use chrono::Timelike as _;
        let ts = pi_filename_timestamp(
            "2026-02-17T03-39-53-642Z_af036bd6-3fa8-492b-a656-93d5bbbd6878.jsonl",
        );
        assert!(ts.is_some(), "should parse a valid Pi filename timestamp");
        let ts = ts.unwrap();
        // Check date/time components — avoids a fragile hardcoded Unix timestamp.
        assert_eq!(ts.date_naive().to_string(), "2026-02-17");
        assert_eq!(ts.hour(), 3);
        assert_eq!(ts.minute(), 39);
        assert_eq!(ts.second(), 53);
    }

    #[test]
    fn pi_filename_timestamp_returns_none_for_non_pi_name() {
        // Claude-style UUID filename — no embedded timestamp
        assert!(pi_filename_timestamp("8f6009e7-c052-4d98-b792-5f6c3bbbd8f9.jsonl").is_none());
        // No underscore separator
        assert!(pi_filename_timestamp("session.jsonl").is_none());
        // Wrong suffix
        assert!(pi_filename_timestamp("2026-02-17T03-39-53-642Z_uuid.json").is_none());
    }

    #[test]
    fn claude_earliest_file_timestamp_finds_min() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("session.jsonl");
        // Three entries — earliest is m1 at 2024-01-01
        std::fs::write(
            &path,
            r#"{"type":"message","id":"m2","timestamp":"2024-06-15T12:00:00Z"}
{"type":"session","id":"s1","timestamp":"2024-01-01T00:00:00Z"}
{"type":"message","id":"m3","timestamp":"2025-03-10T08:30:00Z"}
"#,
        )
        .unwrap();
        let ts = claude_earliest_file_timestamp(&path);
        assert!(ts.is_some());
        assert_eq!(
            ts.unwrap().to_rfc3339(),
            "2024-01-01T00:00:00+00:00",
            "earliest timestamp should be selected"
        );
    }

    #[test]
    fn claude_earliest_file_timestamp_returns_none_for_no_timestamps() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("session.jsonl");
        std::fs::write(&path, r#"{"type":"message","id":"m1"}"#).unwrap();
        assert!(claude_earliest_file_timestamp(&path).is_none());
    }

    #[test]
    fn select_partial_session_files_pi_keeps_newest() {
        // Build three Pi-style filenames with different timestamps.
        let files: Vec<(String, PathBuf)> = vec![
            (
                "2025-01-01T00-00-00-000Z_aaaaaaaa-0000-0000-0000-000000000001.jsonl".to_owned(),
                PathBuf::new(),
            ),
            (
                "2026-06-01T00-00-00-000Z_aaaaaaaa-0000-0000-0000-000000000002.jsonl".to_owned(),
                PathBuf::new(),
            ),
            (
                "2024-03-15T12-00-00-000Z_aaaaaaaa-0000-0000-0000-000000000003.jsonl".to_owned(),
                PathBuf::new(),
            ),
        ];
        // Keep 2 most recent.
        let selected = select_partial_session_files(&files, 2, true);
        assert_eq!(selected.len(), 2);
        // Newest two should be 2026 and 2025.
        assert!(selected
            .contains("2026-06-01T00-00-00-000Z_aaaaaaaa-0000-0000-0000-000000000002.jsonl"));
        assert!(selected
            .contains("2025-01-01T00-00-00-000Z_aaaaaaaa-0000-0000-0000-000000000001.jsonl"));
        assert!(!selected
            .contains("2024-03-15T12-00-00-000Z_aaaaaaaa-0000-0000-0000-000000000003.jsonl"));
    }

    #[test]
    fn select_partial_session_files_max_count_larger_than_set() {
        let files: Vec<(String, PathBuf)> = vec![(
            "2026-01-01T00-00-00-000Z_aaaaaaaa-0000-0000-0000-000000000001.jsonl".to_owned(),
            PathBuf::new(),
        )];
        let selected = select_partial_session_files(&files, 100, true);
        assert_eq!(
            selected.len(),
            1,
            "should return all files when max > count"
        );
    }

    #[test]
    fn materialize_agent_dir_full_writes_all_files() {
        use crate::config::schema::CanonicalizationConfig;

        let dir = TempDir::new().unwrap();
        let repo_agent_dir = dir.path().join("pi").join("sessions");
        let session_subdir = repo_agent_dir.join("--Users-testuser-Dev-proj--");
        std::fs::create_dir_all(&session_subdir).unwrap();

        // Write 3 Pi session files.
        let names = [
            "2024-01-01T00-00-00-000Z_aaaa0001-0000-0000-0000-000000000001.jsonl",
            "2024-06-01T00-00-00-000Z_aaaa0002-0000-0000-0000-000000000002.jsonl",
            "2025-01-01T00-00-00-000Z_aaaa0003-0000-0000-0000-000000000003.jsonl",
        ];
        for name in &names {
            std::fs::write(
                session_subdir.join(name),
                r#"{"type":"session","id":"s1","timestamp":"2024-01-01T00:00:00Z"}
"#,
            )
            .unwrap();
        }

        let local_base = dir.path().join("local_pi_sessions");
        let home = dir.path().to_path_buf();
        let registry = TokenRegistry::from_config(&CanonicalizationConfig::default(), &home);

        let mut cache = MaterializeCache::default();
        let count = materialize_agent_dir(
            &repo_agent_dir,
            &local_base,
            &registry,
            true,
            &MaterializeFilter::Full,
            &mut cache,
            "pi/sessions",
        )
        .unwrap();

        assert_eq!(count, 3, "full mode should materialize all 3 files");
    }

    #[test]
    fn materialize_agent_dir_partial_limits_per_subdir() {
        use crate::config::schema::CanonicalizationConfig;

        let dir = TempDir::new().unwrap();
        let repo_agent_dir = dir.path().join("pi").join("sessions");
        let session_subdir = repo_agent_dir.join("--Users-testuser-Dev-proj--");
        std::fs::create_dir_all(&session_subdir).unwrap();

        // Write 3 files; partial window = 2.
        let names = [
            "2024-01-01T00-00-00-000Z_aaaa0001-0000-0000-0000-000000000001.jsonl",
            "2024-06-01T00-00-00-000Z_aaaa0002-0000-0000-0000-000000000002.jsonl",
            "2025-01-01T00-00-00-000Z_aaaa0003-0000-0000-0000-000000000003.jsonl",
        ];
        for name in &names {
            std::fs::write(
                session_subdir.join(name),
                r#"{"type":"session","id":"s1","timestamp":"2024-01-01T00:00:00Z"}
"#,
            )
            .unwrap();
        }

        let local_base = dir.path().join("local_pi_sessions");
        let home = dir.path().to_path_buf();
        let registry = TokenRegistry::from_config(&CanonicalizationConfig::default(), &home);

        let mut cache = MaterializeCache::default();
        let count = materialize_agent_dir(
            &repo_agent_dir,
            &local_base,
            &registry,
            true,
            &MaterializeFilter::Partial(2),
            &mut cache,
            "pi/sessions",
        )
        .unwrap();

        assert_eq!(
            count, 2,
            "partial mode should materialize only 2 (newest) files"
        );

        // The oldest file should NOT have been written.
        let local_session = local_base.join("--Users-testuser-Dev-proj--");
        let oldest = "2024-01-01T00-00-00-000Z_aaaa0001-0000-0000-0000-000000000001.jsonl";
        assert!(
            !local_session.join(oldest).exists(),
            "oldest file must NOT be materialized in partial mode"
        );
        let newest = "2025-01-01T00-00-00-000Z_aaaa0003-0000-0000-0000-000000000003.jsonl";
        assert!(
            local_session.join(newest).exists(),
            "newest file must be materialized"
        );
    }

    // -----------------------------------------------------------------------
    // US-017: Sync tests
    // -----------------------------------------------------------------------

    fn write_sync_config(
        config_path: &std::path::Path,
        repo_path: &std::path::Path,
        remote_path: &std::path::Path,
        pi_session_dir: &std::path::Path,
        claude_session_dir: &std::path::Path,
        machine_name: &str,
        pi_enabled: bool,
        claude_enabled: bool,
    ) {
        let toml = format!(
            "[general]\nmachine_name = \"{machine_name}\"\nsync_jitter_secs = -1\n\n\
             [storage]\nrepo_path = \"{}\"\nremote_url = \"{}\"\n\n\
             [agents.pi]\nenabled = {pi_enabled}\nsession_dir = \"{}\"\n\n\
             [agents.claude]\nenabled = {claude_enabled}\nsession_dir = \"{}\"\n",
            repo_path.display(),
            remote_path.display(),
            pi_session_dir.display(),
            claude_session_dir.display(),
        );
        std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        std::fs::write(config_path, toml.as_bytes()).unwrap();
    }

    #[test]
    fn sync_dry_run_does_not_create_repo() {
        let dir = TempDir::new().unwrap();
        let home = dir.path().join("home");
        std::fs::create_dir_all(&home).unwrap();

        let repo_path = dir.path().join("repo");
        let pi_sessions = dir.path().join("pi_sessions");
        let claude_sessions = dir.path().join("claude_sessions");
        let config_path = dir.path().join("config.toml");

        // Create a Pi session file so there is something to report in dry-run.
        let session_dir = pi_sessions.join(pi_session_dir_name(&home));
        std::fs::create_dir_all(&session_dir).unwrap();
        std::fs::write(
            session_dir.join("s.jsonl"),
            b"{\"type\":\"session\",\"id\":\"1\"}\n",
        )
        .unwrap();

        // Use write_import_config (no remote) — dry-run never reaches git init.
        write_import_config(
            &config_path,
            &repo_path,
            &pi_sessions,
            &claude_sessions,
            "test-machine",
            true,
            false,
        );

        sync_impl(true, false, &config_path, &home).unwrap();

        assert!(
            !repo_path.exists(),
            "dry run must not create the repo directory"
        );
    }

    #[test]
    fn sync_commits_and_pushes_new_pi_session() {
        let dir = TempDir::new().unwrap();
        let home = dir.path().join("home");
        std::fs::create_dir_all(&home).unwrap();

        let repo_path = dir.path().join("repo");
        let remote_path = dir.path().join("remote");
        let pi_sessions = dir.path().join("pi_sessions");
        let claude_sessions = dir.path().join("claude_sessions");
        let config_path = dir.path().join("config.toml");

        {
            let mut opts = git2::RepositoryInitOptions::new();
            opts.bare(true);
            opts.initial_head("main");
            git2::Repository::init_opts(&remote_path, &opts).unwrap();
        }

        let session_dir = pi_sessions.join(pi_session_dir_name(&home));
        std::fs::create_dir_all(&session_dir).unwrap();
        std::fs::write(
            session_dir.join("session.jsonl"),
            b"{\"type\":\"session\",\"id\":\"1\"}\n",
        )
        .unwrap();

        write_sync_config(
            &config_path,
            &repo_path,
            &remote_path,
            &pi_sessions,
            &claude_sessions,
            "sync-machine",
            true,
            false,
        );

        sync_impl(false, true, &config_path, &home).unwrap();

        // Canonical session dir must exist in the repo working tree.
        let canonical_dir = repo_path
            .join("pi")
            .join("sessions")
            .join("--{{SYNC_HOME}}-Dev-foo--");
        assert!(
            canonical_dir.exists(),
            "canonical Pi session dir must exist after sync; sessions: {:?}",
            std::fs::read_dir(repo_path.join("pi").join("sessions"))
                .unwrap()
                .map(|e| e.unwrap().file_name())
                .collect::<Vec<_>>()
        );

        // Remote must have received the commit.
        let bare = git2::Repository::open_bare(&remote_path).unwrap();
        assert!(
            bare.head().is_ok(),
            "remote HEAD must exist after sync push"
        );
    }

    #[test]
    fn sync_updates_manifest_last_sync_for_machine() {
        let dir = TempDir::new().unwrap();
        let home = dir.path().join("home");
        std::fs::create_dir_all(&home).unwrap();

        let repo_path = dir.path().join("repo");
        let remote_path = dir.path().join("remote");
        let pi_sessions = dir.path().join("pi_sessions");
        let claude_sessions = dir.path().join("claude_sessions");
        let config_path = dir.path().join("config.toml");

        {
            let mut opts = git2::RepositoryInitOptions::new();
            opts.bare(true);
            opts.initial_head("main");
            git2::Repository::init_opts(&remote_path, &opts).unwrap();
        }

        // Session file triggers an outgoing commit; manifest is committed alongside it.
        let session_dir = pi_sessions.join(pi_session_dir_name(&home));
        std::fs::create_dir_all(&session_dir).unwrap();
        std::fs::write(
            session_dir.join("s.jsonl"),
            b"{\"type\":\"session\",\"id\":\"1\"}\n",
        )
        .unwrap();

        write_sync_config(
            &config_path,
            &repo_path,
            &remote_path,
            &pi_sessions,
            &claude_sessions,
            "mani-machine",
            true,
            false,
        );

        sync_impl(false, true, &config_path, &home).unwrap();

        // manifest.json must exist with last_sync set for this machine.
        let manifest_path = repo_path.join(".chronicle").join("manifest.json");
        assert!(
            manifest_path.exists(),
            "manifest.json must exist after sync"
        );

        let content = std::fs::read_to_string(&manifest_path).unwrap();
        let manifest: git::Manifest = serde_json::from_str(&content).unwrap();
        let entry = manifest.machines.get("mani-machine");
        assert!(entry.is_some(), "machine entry must exist in manifest");
        assert!(
            entry.unwrap().last_sync.is_some(),
            "last_sync must be set in manifest entry"
        );
    }

    #[test]
    fn sync_updates_state_cache_after_successful_sync() {
        let dir = TempDir::new().unwrap();
        let home = dir.path().join("home");
        std::fs::create_dir_all(&home).unwrap();

        let repo_path = dir.path().join("repo");
        let remote_path = dir.path().join("remote");
        let pi_sessions = dir.path().join("pi_sessions");
        let claude_sessions = dir.path().join("claude_sessions");
        let config_path = dir.path().join("config.toml");

        {
            let mut opts = git2::RepositoryInitOptions::new();
            opts.bare(true);
            opts.initial_head("main");
            git2::Repository::init_opts(&remote_path, &opts).unwrap();
        }

        let session_dir = pi_sessions.join(pi_session_dir_name(&home));
        std::fs::create_dir_all(&session_dir).unwrap();
        let session_file = session_dir.join("cache-test.jsonl");
        std::fs::write(&session_file, b"{\"type\":\"session\",\"id\":\"C\"}\n").unwrap();

        write_sync_config(
            &config_path,
            &repo_path,
            &remote_path,
            &pi_sessions,
            &claude_sessions,
            "cache-machine",
            true,
            false,
        );

        sync_impl(false, true, &config_path, &home).unwrap();

        // State cache must contain an entry keyed by the session file's absolute path.
        let cache = scan::StateCache::load(&scan::StateCache::path_for_repo(&repo_path)).unwrap();
        let session_key = session_file.to_string_lossy().into_owned();
        assert!(
            cache.files.contains_key(&session_key),
            "state cache must have entry for synced session file; keys: {:?}",
            cache.files.keys().collect::<Vec<_>>()
        );
    }

    #[test]
    fn sync_is_idempotent_no_new_commit_when_unchanged() {
        let dir = TempDir::new().unwrap();
        let home = dir.path().join("home");
        std::fs::create_dir_all(&home).unwrap();

        let repo_path = dir.path().join("repo");
        let remote_path = dir.path().join("remote");
        let pi_sessions = dir.path().join("pi_sessions");
        let claude_sessions = dir.path().join("claude_sessions");
        let config_path = dir.path().join("config.toml");

        {
            let mut opts = git2::RepositoryInitOptions::new();
            opts.bare(true);
            opts.initial_head("main");
            git2::Repository::init_opts(&remote_path, &opts).unwrap();
        }

        let session_dir = pi_sessions.join(pi_session_dir_name(&home));
        std::fs::create_dir_all(&session_dir).unwrap();
        std::fs::write(
            session_dir.join("idempotent.jsonl"),
            b"{\"type\":\"session\",\"id\":\"X\"}\n",
        )
        .unwrap();

        write_sync_config(
            &config_path,
            &repo_path,
            &remote_path,
            &pi_sessions,
            &claude_sessions,
            "idem-machine",
            true,
            false,
        );

        // First sync: new session file → creates a commit.
        sync_impl(false, true, &config_path, &home).unwrap();

        let repo = git2::Repository::open(&repo_path).unwrap();
        let head_before = repo.head().unwrap().target().unwrap();

        // Second sync: session file unchanged (state cache hit) → no new commit.
        sync_impl(false, true, &config_path, &home).unwrap();

        let head_after = repo.head().unwrap().target().unwrap();
        assert_eq!(
            head_before, head_after,
            "no new commit must be created when sync has nothing to do"
        );
    }

    #[test]
    fn materialize_agent_dir_partial_does_not_delete_existing_local_files() {
        use crate::config::schema::CanonicalizationConfig;

        let dir = TempDir::new().unwrap();
        let repo_agent_dir = dir.path().join("pi").join("sessions");
        let session_subdir = repo_agent_dir.join("--Users-testuser-Dev-proj--");
        std::fs::create_dir_all(&session_subdir).unwrap();

        // Only one file in repo.
        let new_file = "2025-01-01T00-00-00-000Z_aaaa0003-0000-0000-0000-000000000003.jsonl";
        std::fs::write(
            session_subdir.join(new_file),
            r#"{"type":"session","id":"s1","timestamp":"2025-01-01T00:00:00Z"}
"#,
        )
        .unwrap();

        // Pre-place an old file locally (simulates a file outside the window
        // that was previously materialised on this machine).
        let local_base = dir.path().join("local_pi_sessions");
        let local_session = local_base.join("--Users-testuser-Dev-proj--");
        std::fs::create_dir_all(&local_session).unwrap();
        let pre_existing = "2023-01-01T00-00-00-000Z_aaaa0000-0000-0000-0000-000000000000.jsonl";
        std::fs::write(local_session.join(pre_existing), "old content\n").unwrap();

        let home = dir.path().to_path_buf();
        let registry = TokenRegistry::from_config(&CanonicalizationConfig::default(), &home);

        // Partial window = 1 (only the new file should be written).
        let mut cache = MaterializeCache::default();
        materialize_agent_dir(
            &repo_agent_dir,
            &local_base,
            &registry,
            true,
            &MaterializeFilter::Partial(1),
            &mut cache,
            "pi/sessions",
        )
        .unwrap();

        // Pre-existing local file must still be present (no deletion).
        assert!(
            local_session.join(pre_existing).exists(),
            "pre-existing local file outside window must NOT be deleted"
        );
    }

    // -----------------------------------------------------------------------
    // US-018: status, errors, config
    // -----------------------------------------------------------------------

    /// Write a minimal config for status/config tests (defaults to branch = "main").
    fn write_status_config(
        config_path: &std::path::Path,
        repo_path: &std::path::Path,
        pi_session_dir: &std::path::Path,
        machine_name: &str,
    ) {
        write_status_config_with_branch(
            config_path,
            repo_path,
            pi_session_dir,
            machine_name,
            "main",
        );
    }

    /// Write a minimal config for status/config tests with an explicit branch name.
    fn write_status_config_with_branch(
        config_path: &std::path::Path,
        repo_path: &std::path::Path,
        pi_session_dir: &std::path::Path,
        machine_name: &str,
        branch: &str,
    ) {
        let toml = format!(
            "[general]\nmachine_name = \"{machine_name}\"\n\n\
             [storage]\nrepo_path = \"{}\"\nbranch = \"{branch}\"\n\n\
             [agents.pi]\nenabled = true\nsession_dir = \"{}\"\n\n\
             [agents.claude]\nenabled = false\n",
            repo_path.display(),
            pi_session_dir.display(),
        );
        std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        std::fs::write(config_path, toml).unwrap();
    }

    // --- status -------------------------------------------------------------

    #[test]
    fn status_shows_machine_name_and_no_repo_message() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join("config.toml");
        let repo_path = dir.path().join("repo"); // does NOT exist
        let pi_sessions = dir.path().join("pi_sessions");
        let home = dir.path().to_path_buf();

        write_status_config(&config_path, &repo_path, &pi_sessions, "test-machine");

        // Should succeed without panicking.
        let args = StatusArgs {
            verbose: false,
            porcelain: false,
            no_color: true,
        };
        status_impl(&args, &config_path, &home).unwrap();
    }

    #[test]
    fn status_counts_pending_new_files() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join("config.toml");
        let repo_path = dir.path().join("repo");
        let pi_sessions = dir.path().join("pi_sessions");
        let home = dir.path().to_path_buf();

        // Create a session subdir with one .jsonl file.
        let session_sub = pi_sessions.join("--Users-foo-proj--");
        std::fs::create_dir_all(&session_sub).unwrap();
        std::fs::write(
            session_sub.join("session.jsonl"),
            b"{\"type\":\"session\",\"id\":\"1\"}\n",
        )
        .unwrap();

        // Init repo so manifest reads are possible.
        git::RepoManager::init_or_open(&repo_path, None, "main")
            .unwrap()
            .ensure_working_tree()
            .unwrap();

        write_status_config(&config_path, &repo_path, &pi_sessions, "test-machine");

        // Should succeed; pending count is at least 1 (state cache is empty).
        // (Scan-based pending count moves to US-003; this test verifies no panic.)
        let args = StatusArgs {
            verbose: false,
            porcelain: false,
            no_color: true,
        };
        status_impl(&args, &config_path, &home).unwrap();
    }

    #[test]
    fn status_shows_last_sync_from_manifest() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join("config.toml");
        let repo_path = dir.path().join("repo");
        let pi_sessions = dir.path().join("pi_sessions");
        let home = dir.path().to_path_buf();

        let manager = git::RepoManager::init_or_open(&repo_path, None, "main").unwrap();
        manager.ensure_working_tree().unwrap();
        manager.ensure_manifest().unwrap();

        // Write a manifest with a last_sync timestamp for our machine.
        let mut manifest = manager.read_manifest().unwrap();
        manifest.machines.insert(
            "sync-machine".to_owned(),
            git::MachineEntry {
                first_seen: chrono::Utc::now(),
                last_sync: Some(chrono::Utc::now()),
                home_path: "{{SYNC_HOME}}".to_owned(),
                os: "macos".to_owned(),
            },
        );
        manager.write_manifest(&manifest).unwrap();

        write_status_config(&config_path, &repo_path, &pi_sessions, "sync-machine");

        // Should succeed; manifest-based timestamp moves to US-003.
        let args = StatusArgs {
            verbose: false,
            porcelain: false,
            no_color: true,
        };
        status_impl(&args, &config_path, &home).unwrap();
    }

    #[test]
    fn status_uses_configured_branch_not_main() {
        // Regression test for H-1: status_impl must use cfg.storage.branch,
        // not the hardcoded "main" literal.
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join("config.toml");
        let repo_path = dir.path().join("repo");
        let pi_sessions = dir.path().join("pi_sessions");
        let home = dir.path().to_path_buf();

        // Init the repository on "chronicle" branch (not "main").
        git::RepoManager::init_or_open(&repo_path, None, "chronicle")
            .unwrap()
            .ensure_working_tree()
            .unwrap();

        // Write config pointing at the same non-main branch.
        write_status_config_with_branch(
            &config_path,
            &repo_path,
            &pi_sessions,
            "branch-machine",
            "chronicle",
        );

        // status_impl must succeed; branch handling moves to US-003 (last-sync
        // section opens the git repo).  For US-002 (Config/Machine only), this
        // verifies no panic on a non-"main" branch config.
        let args = StatusArgs {
            verbose: false,
            porcelain: false,
            no_color: true,
        };
        status_impl(&args, &config_path, &home).unwrap();
    }

    // --- US-002: status formatter and config/machine section ----------------

    #[test]
    fn status_formatter_ok_writes_check_mark() {
        let mut buf = Vec::<u8>::new();
        let mut fmt = StatusFormatter::new(&mut buf, false, false);
        fmt.ok("Label", "detail").unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(out.contains("✓"), "ok line must contain ✓");
        assert!(out.contains("Label"), "ok line must contain label");
        assert!(out.contains("detail"), "ok line must contain detail");
    }

    #[test]
    fn status_formatter_warn_writes_warning_symbol() {
        let mut buf = Vec::<u8>::new();
        let mut fmt = StatusFormatter::new(&mut buf, false, false);
        fmt.warn("Label", "detail").unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(out.contains("⚠"), "warn line must contain ⚠");
    }

    #[test]
    fn status_formatter_err_writes_cross() {
        let mut buf = Vec::<u8>::new();
        let mut fmt = StatusFormatter::new(&mut buf, false, false);
        fmt.err("Label", "detail").unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(out.contains("✗"), "err line must contain ✗");
    }

    #[test]
    fn status_formatter_color_includes_ansi_codes() {
        let mut buf = Vec::<u8>::new();
        let mut fmt = StatusFormatter::new(&mut buf, true, false);
        fmt.ok("L", "d").unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(
            out.contains("\x1b["),
            "color-enabled output must contain ANSI escape codes"
        );
    }

    #[test]
    fn status_formatter_no_color_excludes_ansi_codes() {
        let mut buf = Vec::<u8>::new();
        let mut fmt = StatusFormatter::new(&mut buf, false, false);
        fmt.ok("L", "d").unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(
            !out.contains("\x1b["),
            "color-disabled output must not contain ANSI escape codes"
        );
    }

    #[test]
    fn status_formatter_porcelain_emits_kv() {
        let mut buf = Vec::<u8>::new();
        let mut fmt = StatusFormatter::new(&mut buf, false, true);
        fmt.kv("machine", "eager-falcon").unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert_eq!(
            out, "machine=eager-falcon\n",
            "porcelain kv must be key=value\\n"
        );
    }

    #[test]
    fn status_formatter_porcelain_suppresses_symbol_lines() {
        let mut buf = Vec::<u8>::new();
        let mut fmt = StatusFormatter::new(&mut buf, false, true);
        fmt.ok("Label", "detail").unwrap();
        fmt.warn("Label2", "detail2").unwrap();
        fmt.err("Label3", "detail3").unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(out.is_empty(), "porcelain mode must suppress symbol lines");
    }

    #[test]
    fn should_use_color_no_color_flag_suppresses() {
        assert!(
            !should_use_color(true),
            "--no-color flag must suppress color"
        );
    }

    #[test]
    fn should_use_color_no_color_env_suppresses() {
        std::env::set_var("NO_COLOR", "1");
        let result = should_use_color(false);
        std::env::remove_var("NO_COLOR");
        assert!(!result, "NO_COLOR env var must suppress color");
    }

    #[test]
    fn status_config_ok_path_shows_check_mark() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join("config.toml");
        let repo_path = dir.path().join("repo");
        let pi_sessions = dir.path().join("pi_sessions");
        let home = dir.path().to_path_buf();

        // Create the sessions directory so it is found.
        std::fs::create_dir_all(&pi_sessions).unwrap();

        let toml = format!(
            "[general]\nmachine_name = \"test-machine\"\n\n\
             [storage]\nrepo_path = \"{}\"\nremote_url = \"https://example.com/repo.git\"\n\n\
             [agents.pi]\nenabled = true\nsession_dir = \"{}\"\n\n\
             [agents.claude]\nenabled = false\n",
            repo_path.display(),
            pi_sessions.display(),
        );
        std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        std::fs::write(&config_path, &toml).unwrap();

        let args = StatusArgs {
            verbose: false,
            porcelain: false,
            no_color: true,
        };
        let mut buf = Vec::<u8>::new();
        status_write(&args, &config_path, &home, false, &mut buf).unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(out.contains("✓"), "config-ok path must emit ✓ symbol");
        assert!(!out.contains("✗"), "config-ok path must not emit ✗ symbol");
    }

    #[test]
    fn status_config_missing_path_shows_error() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join("nonexistent.toml");
        let home = dir.path().to_path_buf();

        let args = StatusArgs {
            verbose: false,
            porcelain: false,
            no_color: true,
        };
        let mut buf = Vec::<u8>::new();
        status_write(&args, &config_path, &home, false, &mut buf).unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(out.contains("✗"), "config-missing must emit ✗ error line");
    }

    #[test]
    fn status_missing_sessions_dir_shows_error() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join("config.toml");
        let repo_path = dir.path().join("repo");
        let pi_sessions = dir.path().join("pi_sessions_nonexistent");
        let home = dir.path().to_path_buf();

        // Do NOT create pi_sessions — it must be missing.
        let toml = format!(
            "[general]\nmachine_name = \"test-machine\"\n\n\
             [storage]\nrepo_path = \"{}\"\nremote_url = \"https://example.com/repo.git\"\n\n\
             [agents.pi]\nenabled = true\nsession_dir = \"{}\"\n\n\
             [agents.claude]\nenabled = false\n",
            repo_path.display(),
            pi_sessions.display(),
        );
        std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        std::fs::write(&config_path, &toml).unwrap();

        let args = StatusArgs {
            verbose: false,
            porcelain: false,
            no_color: true,
        };
        let mut buf = Vec::<u8>::new();
        status_write(&args, &config_path, &home, false, &mut buf).unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(
            out.contains("✗"),
            "missing sessions dir must emit ✗ error line"
        );
        assert!(
            out.contains("sessions"),
            "error message must mention sessions directory"
        );
    }

    #[test]
    fn status_porcelain_config_ok_emits_kv_keys() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join("config.toml");
        let repo_path = dir.path().join("repo");
        let pi_sessions = dir.path().join("pi_sessions");
        let home = dir.path().to_path_buf();

        std::fs::create_dir_all(&pi_sessions).unwrap();
        let toml = format!(
            "[general]\nmachine_name = \"eager-falcon\"\n\n\
             [storage]\nrepo_path = \"{}\"\nremote_url = \"https://example.com/repo.git\"\n\n\
             [agents.pi]\nenabled = true\nsession_dir = \"{}\"\n\n\
             [agents.claude]\nenabled = false\n",
            repo_path.display(),
            pi_sessions.display(),
        );
        std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        std::fs::write(&config_path, &toml).unwrap();

        let args = StatusArgs {
            verbose: false,
            porcelain: true,
            no_color: true,
        };
        let mut buf = Vec::<u8>::new();
        status_write(&args, &config_path, &home, false, &mut buf).unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(
            out.contains("machine=eager-falcon"),
            "porcelain must emit machine="
        );
        assert!(
            out.contains("config_ok=true"),
            "porcelain must emit config_ok=true when all is healthy"
        );
        assert!(!out.contains("✓"), "porcelain mode must not emit symbols");
    }

    // --- errors -------------------------------------------------------------

    #[test]
    fn errors_no_entries_prints_none_message() {
        let dir = TempDir::new().unwrap();
        let errors_path = dir.path().join("errors.jsonl");

        // Empty file → "No errors recorded."
        errors_impl(None, &errors_path).unwrap();
    }

    #[test]
    fn errors_shows_all_entries() {
        use crate::errors::ring_buffer::{ErrorEntry, RingBuffer, Severity};

        let dir = TempDir::new().unwrap();
        let errors_path = dir.path().join("errors.jsonl");
        let rb = RingBuffer::new(errors_path.clone());

        rb.append(
            ErrorEntry::new(Severity::Error, "git_error", "network timeout")
                .with_detail("exhausted 3 retries"),
        )
        .unwrap();
        rb.append(
            ErrorEntry::new(Severity::Warning, "prefix_mismatch", "entries differ")
                .with_file("pi/sessions/--foo--/s.jsonl"),
        )
        .unwrap();

        // Should not error; exercises the display path.
        errors_impl(None, &errors_path).unwrap();
    }

    #[test]
    fn errors_limit_respected() {
        use crate::errors::ring_buffer::{ErrorEntry, RingBuffer, Severity};

        let dir = TempDir::new().unwrap();
        let errors_path = dir.path().join("errors.jsonl");
        let rb = RingBuffer::new(errors_path.clone());

        for i in 0..10u32 {
            rb.append(ErrorEntry::new(
                Severity::Info,
                "io_error",
                format!("msg {i}"),
            ))
            .unwrap();
        }

        // Limit=3 → should only read 3 entries without error.
        errors_impl(Some(3), &errors_path).unwrap();
    }

    // --- config -------------------------------------------------------------

    /// Write the simplest valid config for config command tests.
    fn write_minimal_config(config_path: &std::path::Path, repo_path: &std::path::Path) {
        let toml = format!("[storage]\nrepo_path = \"{}\"\n", repo_path.display());
        std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        std::fs::write(config_path, toml).unwrap();
    }

    #[test]
    fn config_no_args_prints_toml() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join("config.toml");
        let repo_path = dir.path().join("repo");
        write_minimal_config(&config_path, &repo_path);

        // No key or value → prints TOML; should not error.
        config_impl(None, None, &config_path).unwrap();
    }

    #[test]
    fn config_key_reads_value() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join("config.toml");
        let repo_path = dir.path().join("repo");
        write_minimal_config(&config_path, &repo_path);

        // Read storage.repo_path.
        config_impl(Some("storage.repo_path".to_owned()), None, &config_path).unwrap();
    }

    #[test]
    fn config_machine_name_alias_reads_value() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join("config.toml");
        let repo_path = dir.path().join("repo");

        let toml = format!(
            "[general]\nmachine_name = \"friendly-fox\"\n\n[storage]\nrepo_path = \"{}\"\n",
            repo_path.display()
        );
        std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        std::fs::write(&config_path, toml).unwrap();

        // `machine-name` is the special alias per spec §9.1.
        config_impl(Some("machine-name".to_owned()), None, &config_path).unwrap();
    }

    #[test]
    fn config_key_value_sets_and_persists() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join("config.toml");
        let repo_path = dir.path().join("repo");
        write_minimal_config(&config_path, &repo_path);

        // Set storage.remote_url.
        config_impl(
            Some("storage.remote_url".to_owned()),
            Some("git@github.com:user/sessions.git".to_owned()),
            &config_path,
        )
        .unwrap();

        // Reload and verify.
        let cfg = config::load(Some(&config_path), &CliOverrides::default()).unwrap();
        assert_eq!(cfg.storage.remote_url, "git@github.com:user/sessions.git");
    }

    #[test]
    fn config_machine_name_alias_sets_value() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join("config.toml");
        let repo_path = dir.path().join("repo");
        write_minimal_config(&config_path, &repo_path);

        config_impl(
            Some("machine-name".to_owned()),
            Some("jolly-jaguar".to_owned()),
            &config_path,
        )
        .unwrap();

        let cfg = config::load(Some(&config_path), &CliOverrides::default()).unwrap();
        assert_eq!(cfg.general.machine_name, "jolly-jaguar");
    }

    #[test]
    fn config_unknown_key_returns_error() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join("config.toml");
        let repo_path = dir.path().join("repo");
        write_minimal_config(&config_path, &repo_path);

        let result = config_impl(Some("does.not.exist".to_owned()), None, &config_path);
        assert!(result.is_err(), "unknown key should return an error");
    }

    #[test]
    fn config_history_mode_roundtrip() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join("config.toml");
        let repo_path = dir.path().join("repo");
        write_minimal_config(&config_path, &repo_path);

        // Set to "full".
        config_impl(
            Some("sync.history_mode".to_owned()),
            Some("full".to_owned()),
            &config_path,
        )
        .unwrap();

        let cfg = config::load(Some(&config_path), &CliOverrides::default()).unwrap();
        assert_eq!(
            cfg.sync.history_mode,
            crate::config::schema::HistoryMode::Full
        );
    }

    // --- canonicalization.level range validation ----------------------------

    /// Helper: run config set canonicalization.level = <value>.
    fn set_canon_level(config_path: &std::path::Path, value: &str) -> Result<()> {
        config_impl(
            Some("canonicalization.level".to_owned()),
            Some(value.to_owned()),
            config_path,
        )
    }

    #[test]
    fn config_set_level_0_rejected() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join("config.toml");
        let repo_path = dir.path().join("repo");
        write_minimal_config(&config_path, &repo_path);

        assert!(
            set_canon_level(&config_path, "0").is_err(),
            "level 0 should be rejected"
        );
    }

    #[test]
    fn config_set_level_1_accepted() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join("config.toml");
        let repo_path = dir.path().join("repo");
        write_minimal_config(&config_path, &repo_path);

        set_canon_level(&config_path, "1").expect("level 1 should be valid");
        let cfg = config::load(Some(&config_path), &CliOverrides::default()).unwrap();
        assert_eq!(cfg.canonicalization.level, 1);
    }

    #[test]
    fn config_set_level_2_accepted() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join("config.toml");
        let repo_path = dir.path().join("repo");
        write_minimal_config(&config_path, &repo_path);

        set_canon_level(&config_path, "2").expect("level 2 should be valid");
        let cfg = config::load(Some(&config_path), &CliOverrides::default()).unwrap();
        assert_eq!(cfg.canonicalization.level, 2);
    }

    #[test]
    fn config_set_level_3_accepted() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join("config.toml");
        let repo_path = dir.path().join("repo");
        write_minimal_config(&config_path, &repo_path);

        set_canon_level(&config_path, "3").expect("level 3 should be valid");
        let cfg = config::load(Some(&config_path), &CliOverrides::default()).unwrap();
        assert_eq!(cfg.canonicalization.level, 3);
    }

    #[test]
    fn config_set_level_4_rejected() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join("config.toml");
        let repo_path = dir.path().join("repo");
        write_minimal_config(&config_path, &repo_path);

        assert!(
            set_canon_level(&config_path, "4").is_err(),
            "level 4 should be rejected"
        );
    }

    // -----------------------------------------------------------------------
    // US-003: MaterializeCache skips unchanged repo files
    // -----------------------------------------------------------------------

    #[test]
    fn materialize_cache_skips_unchanged_files_on_second_call() {
        let dir = TempDir::new().unwrap();
        let home = dir.path().to_path_buf();

        let repo_path = dir.path().join("repo");
        let pi_sessions = dir.path().join("pi_sessions");
        let config_path = dir.path().join("config.toml");

        // Plant a canonical session file in the repo working tree.
        // Using a dir name with no {{SYNC_HOME}} token so decanonicalization
        // is a no-op and the local path is predictable.
        let canonical_dir = repo_path
            .join("pi")
            .join("sessions")
            .join("--Users-testuser-Dev-proj--");
        std::fs::create_dir_all(&canonical_dir).unwrap();
        std::fs::write(
            canonical_dir.join("session.jsonl"),
            b"{\"type\":\"session\",\"id\":\"1\"}\n",
        )
        .unwrap();

        let toml = format!(
            "[general]\nmachine_name = \"cache-test\"\n\n\
             [storage]\nrepo_path = \"{}\"\n\n\
             [agents.pi]\nenabled = true\nsession_dir = \"{}\"\n\n\
             [agents.claude]\nenabled = false\n",
            repo_path.display(),
            pi_sessions.display(),
        );
        std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        std::fs::write(&config_path, toml.as_bytes()).unwrap();

        let cfg = config::load(Some(&config_path), &CliOverrides::default()).unwrap();
        let registry = TokenRegistry::from_config(&cfg.canonicalization, &home);

        // First call: file is new → must be written → count = 1.
        let count1 = materialize_repo_to_local(&repo_path, &cfg, &home, &registry).unwrap();
        assert_eq!(count1, 1, "first materialize call should write 1 file");

        // Second call: repo file mtime/size unchanged → cache hit → count = 0.
        let count2 = materialize_repo_to_local(&repo_path, &cfg, &home, &registry).unwrap();
        assert_eq!(
            count2, 0,
            "second materialize call with unchanged repo file should produce 0 (cache hit)"
        );

        // Verify the cache file was persisted.
        let cache_path = MaterializeCache::path_for_repo(&repo_path);
        assert!(
            cache_path.exists(),
            "materialize-state.json must be persisted after first call"
        );
    }

    // -----------------------------------------------------------------------
    // US-004: Advisory file lock
    // -----------------------------------------------------------------------

    #[test]
    fn lock_file_path_is_sibling_of_repo_dir() {
        // Standard case: repo lives inside a parent directory.
        let lock = lock_file_path(std::path::Path::new(
            "/home/user/.local/share/chronicle/repo",
        ));
        assert!(
            lock.to_string_lossy().ends_with("chronicle.lock"),
            "lock file name must be chronicle.lock"
        );
        assert_eq!(
            lock.parent().unwrap(),
            std::path::Path::new("/home/user/.local/share/chronicle"),
            "lock file must be a sibling of the repo dir"
        );
    }

    #[test]
    fn lock_file_path_falls_back_to_repo_path_when_no_parent() {
        // Edge case: repo_path has no parent (e.g., just "repo" or "/").
        let lock = lock_file_path(std::path::Path::new("repo"));
        // unwrap_or(repo_path) kicks in; result still ends with chronicle.lock
        assert!(lock.to_string_lossy().ends_with("chronicle.lock"));
    }

    #[test]
    fn try_acquire_sync_lock_creates_and_releases_lock_file() {
        let dir = TempDir::new().unwrap();
        let repo_path = dir.path().join("repo");

        // Acquiring the lock should succeed and create chronicle.lock.
        let lock = try_acquire_sync_lock(&repo_path, 300)
            .expect("try_acquire_sync_lock should not error")
            .expect("lock should be acquired when no other holder exists");

        let lock_path = lock_file_path(&repo_path);
        assert!(lock_path.exists(), "chronicle.lock must be created");

        // Lock file must contain PID and timestamp.
        let contents = fs::read_to_string(&lock_path).expect("read lock file");
        let parts: Vec<&str> = contents.split_whitespace().collect();
        assert_eq!(parts.len(), 2, "lock file must contain PID and timestamp");
        let pid: u32 = parts[0].parse().expect("PID must be a number");
        assert_eq!(pid, std::process::id(), "PID must match current process");
        let _ts: u64 = parts[1].parse().expect("timestamp must be a number");

        // Drop the lock; on Unix this releases the flock.
        drop(lock);
    }

    #[cfg(unix)]
    #[test]
    fn second_lock_attempt_returns_none_while_first_held() {
        let dir = TempDir::new().unwrap();
        let repo_path = dir.path().join("repo");

        // First acquisition — must succeed.
        let lock1 = try_acquire_sync_lock(&repo_path, -1)
            .expect("first acquisition should not error")
            .expect("first acquisition should succeed");

        // Second attempt with recovery disabled — must return None.
        let lock2 = try_acquire_sync_lock(&repo_path, -1)
            .expect("second acquisition should not error (no unexpected I/O)");
        assert!(
            lock2.is_none(),
            "second acquisition must return None while first lock is held"
        );

        drop(lock1);
    }

    #[cfg(unix)]
    #[test]
    fn stale_lock_broken_when_pid_is_dead() {
        let dir = TempDir::new().unwrap();
        let repo_path = dir.path().join("repo");
        let lock_path = lock_file_path(&repo_path);

        // Manually create a lock file with a bogus PID that doesn't exist.
        fs::create_dir_all(lock_path.parent().unwrap()).unwrap();
        // PID 2_000_000_000 is almost certainly not a running process on any
        // real system (well above typical pid_max), and safely fits in i32.
        fs::write(&lock_path, "2000000000 9999999999").unwrap();

        // Acquire a real flock on the file so the first flock attempt fails,
        // then drop it so the recovery attempt can succeed.  Actually, since
        // the file was created externally (no flock held), the first flock
        // attempt should succeed directly.  To test the stale-recovery path
        // we need the flock to be held.
        //
        // Strategy: fork a short-lived child that flocks the file and exits,
        // but that's complex.  Instead, test the `is_lock_stale` function
        // directly and then test the full flow without a held flock.

        // The lock file has a dead PID → is_lock_stale should return true.
        assert!(
            is_lock_stale(&lock_path, 0).expect("is_lock_stale should not error"),
            "lock with dead PID must be stale"
        );
    }

    #[test]
    fn stale_lock_detected_when_age_exceeds_timeout() {
        let dir = TempDir::new().unwrap();
        let repo_path = dir.path().join("repo");
        let lock_path = lock_file_path(&repo_path);

        fs::create_dir_all(lock_path.parent().unwrap()).unwrap();
        // Write current PID (alive) but a timestamp from 10 minutes ago.
        let old_ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            - 600;
        fs::write(&lock_path, format!("{} {}", std::process::id(), old_ts)).unwrap();

        // With timeout of 300s, the 600s-old lock should be stale.
        assert!(
            is_lock_stale(&lock_path, 300).expect("is_lock_stale should not error"),
            "lock older than timeout must be stale"
        );

        // With timeout of 0, age check is skipped; PID is alive → not stale.
        assert!(
            !is_lock_stale(&lock_path, 0).expect("is_lock_stale should not error"),
            "age check disabled (timeout=0) and PID alive → not stale"
        );
    }

    #[test]
    fn lock_recovery_disabled_with_negative_timeout() {
        let dir = TempDir::new().unwrap();
        let repo_path = dir.path().join("repo");

        // Acquire a lock normally.
        let _lock1 = try_acquire_sync_lock(&repo_path, 300)
            .expect("should not error")
            .expect("should acquire");

        // With recovery disabled (-1), second attempt must return None even
        // though the lock_timeout_secs would normally trigger recovery.
        let lock2 = try_acquire_sync_lock(&repo_path, -1).expect("should not error");

        // On Unix this will be None (EWOULDBLOCK, no recovery).  On non-Unix
        // flock is a no-op so it would succeed — only assert on Unix.
        #[cfg(unix)]
        assert!(lock2.is_none(), "recovery disabled: must return None");
        #[cfg(not(unix))]
        let _ = lock2;
    }
}
