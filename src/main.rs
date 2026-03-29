use anyhow::Result;
use clap::{Parser, Subcommand};

use chronicle::cli;

/// Bidirectional sync for AI agent session history across machines.
#[derive(Parser)]
#[command(name = "chronicle", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// First-time setup: create config, generate machine name, init git repo.
    Init {
        /// Remote URL for the git repository.
        #[arg(long)]
        remote: Option<String>,
    },

    /// One-time bulk import of existing session files into the canonical store.
    Import {
        /// Filter by agent: pi, claude, or all.
        #[arg(long, value_name = "AGENT", default_value = "all")]
        agent: String,
        /// Show what would be imported without writing anything.
        #[arg(long)]
        dry_run: bool,
    },

    /// Full bidirectional sync cycle (the command cron invokes).
    Sync {
        /// Show what would be done without writing anything.
        #[arg(long)]
        dry_run: bool,
        /// Suppress all non-error output.
        #[arg(long)]
        quiet: bool,
    },

    /// Outgoing only: canonicalize local changes, commit, and push.
    Push {
        /// Show what would be done without writing anything.
        #[arg(long)]
        dry_run: bool,
    },

    /// Incoming only: fetch, merge at entry level, de-canonicalize, write local.
    Pull {
        /// Show what would be done without writing anything.
        #[arg(long)]
        dry_run: bool,
    },

    /// Show last sync time, pending changes, machine name, and agent status.
    Status,

    /// Display the error ring buffer.
    Errors {
        /// Maximum number of entries to display (default: 30).
        #[arg(long, value_name = "N")]
        limit: Option<usize>,
    },

    /// View or edit configuration values.
    Config {
        /// Configuration key to read or write.
        key: Option<String>,
        /// New value to assign to the key.
        value: Option<String>,
    },

    /// Manage cron scheduling for automatic syncing.
    Schedule {
        #[command(subcommand)]
        command: ScheduleCommands,
    },
}

#[derive(Subcommand)]
enum ScheduleCommands {
    /// Install @reboot and interval crontab entries tagged with # chronicle-sync.
    Install,
    /// Remove all chronicle crontab entries identified by # chronicle-sync.
    Uninstall,
    /// Report whether crontab entries are installed, the interval, and binary path.
    Status,
}

fn main() -> Result<()> {
    let args = Cli::parse();

    match args.command {
        Commands::Init { remote } => cli::handle_init(remote),
        Commands::Import { agent, dry_run } => cli::handle_import(agent, dry_run),
        Commands::Sync { dry_run, quiet } => cli::handle_sync(dry_run, quiet),
        Commands::Push { dry_run } => cli::handle_push(dry_run),
        Commands::Pull { dry_run } => cli::handle_pull(dry_run),
        Commands::Status => cli::handle_status(),
        Commands::Errors { limit } => cli::handle_errors(limit),
        Commands::Config { key, value } => cli::handle_config(key, value),
        Commands::Schedule { command } => match command {
            ScheduleCommands::Install => cli::handle_schedule_install(),
            ScheduleCommands::Uninstall => cli::handle_schedule_uninstall(),
            ScheduleCommands::Status => cli::handle_schedule_status(),
        },
    }
}
