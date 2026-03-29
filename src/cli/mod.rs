use anyhow::{Context as _, Result};
use std::fs;
use std::io::{self, BufRead as _, IsTerminal as _, Write as _};
use std::path::{Path, PathBuf};

use crate::canon::levels::L3_WARNING;
use crate::canon::TokenRegistry;
use crate::config::{self, CliOverrides};
use crate::git;

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

    let manager = git::RepoManager::init_or_open(&repo_path, remote_url)
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
fn import_impl(agent: &str, dry_run: bool, config_path: &Path, home: &Path) -> Result<()> {
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

    let repo_path = config::expand_path(&cfg.storage.repo_path);
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
            git::RepoManager::init_or_open(&repo_path, remote_url)
                .context("failed to open git repository")?,
        )
    };
    let manager = manager_owned.as_ref();

    let mut total_sessions = 0usize;
    let mut total_files = 0usize;

    if (agent == "pi" || agent == "all") && cfg.agents.pi.enabled {
        let source_dir = config::expand_path(&cfg.agents.pi.session_dir);
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
        let source_dir = config::expand_path(&cfg.agents.claude.session_dir);
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
// chronicle sync
// ---------------------------------------------------------------------------

/// Handle `chronicle sync [--dry-run] [--quiet]`.
pub fn handle_sync(_dry_run: bool, _quiet: bool) -> Result<()> {
    println!("not implemented: sync");
    Ok(())
}

// ---------------------------------------------------------------------------
// chronicle push
// ---------------------------------------------------------------------------

/// Handle `chronicle push [--dry-run]`.
pub fn handle_push(_dry_run: bool) -> Result<()> {
    println!("not implemented: push");
    Ok(())
}

// ---------------------------------------------------------------------------
// chronicle pull
// ---------------------------------------------------------------------------

/// Handle `chronicle pull [--dry-run]`.
pub fn handle_pull(_dry_run: bool) -> Result<()> {
    println!("not implemented: pull");
    Ok(())
}

// ---------------------------------------------------------------------------
// chronicle status
// ---------------------------------------------------------------------------

/// Handle `chronicle status`.
pub fn handle_status() -> Result<()> {
    println!("not implemented: status");
    Ok(())
}

// ---------------------------------------------------------------------------
// chronicle errors
// ---------------------------------------------------------------------------

/// Handle `chronicle errors [--limit <n>]`.
pub fn handle_errors(_limit: Option<usize>) -> Result<()> {
    println!("not implemented: errors");
    Ok(())
}

// ---------------------------------------------------------------------------
// chronicle config
// ---------------------------------------------------------------------------

/// Handle `chronicle config [<key>] [<value>]`.
pub fn handle_config(_key: Option<String>, _value: Option<String>) -> Result<()> {
    println!("not implemented: config");
    Ok(())
}

// ---------------------------------------------------------------------------
// chronicle schedule *
// ---------------------------------------------------------------------------

/// Handle `chronicle schedule install`.
pub fn handle_schedule_install() -> Result<()> {
    println!("not implemented: schedule install");
    Ok(())
}

/// Handle `chronicle schedule uninstall`.
pub fn handle_schedule_uninstall() -> Result<()> {
    println!("not implemented: schedule uninstall");
    Ok(())
}

/// Handle `chronicle schedule status`.
pub fn handle_schedule_status() -> Result<()> {
    println!("not implemented: schedule status");
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
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

        let manager = git::RepoManager::init_or_open(&repo_path, remote_url)
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
}
