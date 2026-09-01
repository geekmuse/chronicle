//! Extensible adapters for session-history agents.
//!
//! The registry centralizes the small amount of agent-specific knowledge that
//! Chronicle needs: a session root, a repository layout, L1 directory-name
//! canonicalization, and recognition of session artifacts.  Callers should
//! use [`AdapterRegistry`] instead of branching on a Pi/Claude boolean.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use thiserror::Error;

use crate::canon::TokenRegistry;
use crate::config::schema::AgentsConfig;

/// Stable identifier for a supported session-history agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AgentId {
    /// The Pi coding agent.
    Pi,
    /// Anthropic's Claude Code.
    Claude,
}

impl AgentId {
    /// Stable lowercase name used in output and configuration.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pi => "pi",
            Self::Claude => "claude",
        }
    }
}

impl std::fmt::Display for AgentId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Static descriptive data supplied by an [`AgentAdapter`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgentMetadata {
    /// Stable agent identifier.
    pub id: AgentId,
    /// Human-readable name used in diagnostics.
    pub display_name: &'static str,
    /// Configuration section name, for example `agents.pi`.
    pub config_key: &'static str,
    /// Agent-specific root within the canonical repository.
    pub repo_rel_base: &'static str,
}

/// Runtime configuration resolved for one registered agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentContext {
    /// Metadata for the selected adapter.
    pub metadata: AgentMetadata,
    /// Whether this agent is enabled in the Chronicle configuration.
    pub enabled: bool,
    /// Local directory containing this agent's session directories.
    pub session_dir: PathBuf,
}

/// A JSONL session file and its canonical repository destination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionArtifact {
    /// Local JSONL source file.
    pub source_path: PathBuf,
    /// Name of the session directory containing the file.
    pub session_dir_name: String,
    /// Relative destination path below the Chronicle repository root.
    pub repo_relative_path: PathBuf,
}

/// Counts produced by an agent-specific import or materialization operation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AdapterOutcome {
    /// Number of session directories processed.
    pub sessions: usize,
    /// Number of JSONL files processed.
    pub files: usize,
}

impl AdapterOutcome {
    /// Add another operation's counts to this outcome.
    pub fn extend(&mut self, other: Self) {
        self.sessions += other.sessions;
        self.files += other.files;
    }
}

/// Errors raised while resolving or validating an adapter artifact.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum AdapterError {
    /// A caller asked for an agent that is not in the registry.
    #[error("unsupported agent '{0}'")]
    UnsupportedAgent(String),
    /// A registry cannot contain two adapters for the same identifier.
    #[error("adapter '{0}' is already registered")]
    DuplicateAdapter(AgentId),
    /// An artifact was outside the configured session root.
    #[error("session artifact {path} is not below {session_dir}")]
    OutsideSessionRoot { path: PathBuf, session_dir: PathBuf },
    /// An artifact did not have the expected JSONL extension.
    #[error("session artifact {0} is not a .jsonl file")]
    NotJsonl(PathBuf),
    /// A session file must reside immediately below a session directory.
    #[error("session artifact {0} has no session directory")]
    MissingSessionDirectory(PathBuf),
}

/// Agent-specific operations required by Chronicle's sync pipeline.
pub trait AgentAdapter: Send + Sync {
    /// Static metadata describing this adapter.
    fn metadata(&self) -> AgentMetadata;

    /// Default root where this agent stores session directories.
    fn default_session_dir(&self, home: &Path) -> PathBuf;

    /// Canonicalize an encoded session-directory name.
    fn canonicalize_dir(&self, registry: &TokenRegistry, name: &str) -> String;

    /// De-canonicalize an encoded session-directory name.
    fn decanonicalize_dir(&self, registry: &TokenRegistry, name: &str) -> String;

    /// Return the timestamp used to rank a repository JSONL artifact by recency.
    ///
    /// Adapters which cannot infer a timestamp return `None`; callers treat
    /// those artifacts as oldest.
    fn repository_artifact_recency(&self, _path: &Path) -> Option<DateTime<Utc>> {
        None
    }

    /// Whether `path` is a session artifact handled by this adapter.
    fn is_session_file(&self, path: &Path) -> bool {
        path.extension()
            .is_some_and(|extension| extension == "jsonl")
    }

    /// Validate a local JSONL file and derive its canonical repository path.
    fn artifact(
        &self,
        session_dir: &Path,
        source_path: &Path,
        registry: &TokenRegistry,
    ) -> Result<SessionArtifact, AdapterError> {
        if !self.is_session_file(source_path) {
            return Err(AdapterError::NotJsonl(source_path.to_path_buf()));
        }
        let relative = source_path.strip_prefix(session_dir).map_err(|_| {
            AdapterError::OutsideSessionRoot {
                path: source_path.to_path_buf(),
                session_dir: session_dir.to_path_buf(),
            }
        })?;
        let mut components = relative.components();
        let session_name = components
            .next()
            .and_then(|component| component.as_os_str().to_str())
            .ok_or_else(|| AdapterError::MissingSessionDirectory(source_path.to_path_buf()))?;
        let file_name = components
            .next()
            .filter(|_| components.next().is_none())
            .map(|component| component.as_os_str())
            .ok_or_else(|| AdapterError::MissingSessionDirectory(source_path.to_path_buf()))?;
        let metadata = self.metadata();
        Ok(SessionArtifact {
            source_path: source_path.to_path_buf(),
            session_dir_name: session_name.to_owned(),
            repo_relative_path: Path::new(metadata.repo_rel_base)
                .join(self.canonicalize_dir(registry, session_name))
                .join(file_name),
        })
    }
}

/// A deterministic collection of registered agent adapters.
#[derive(Default)]
pub struct AdapterRegistry {
    adapters: BTreeMap<AgentId, Box<dyn AgentAdapter>>,
}

impl AdapterRegistry {
    /// Create a registry containing Pi and Claude Code adapters.
    #[must_use]
    pub fn with_defaults() -> Self {
        let mut registry = Self::default();
        // These built-ins have distinct fixed identifiers.
        registry.register(PiAdapter).expect("Pi adapter is unique");
        registry
            .register(ClaudeAdapter)
            .expect("Claude adapter is unique");
        registry
    }

    /// Add an adapter, rejecting duplicate identifiers.
    pub fn register<A>(&mut self, adapter: A) -> Result<(), AdapterError>
    where
        A: AgentAdapter + 'static,
    {
        let id = adapter.metadata().id;
        if self.adapters.contains_key(&id) {
            return Err(AdapterError::DuplicateAdapter(id));
        }
        self.adapters.insert(id, Box::new(adapter));
        Ok(())
    }

    /// Find a registered adapter by its stable identifier.
    #[must_use]
    pub fn get(&self, id: AgentId) -> Option<&dyn AgentAdapter> {
        self.adapters.get(&id).map(Box::as_ref)
    }

    /// Iterate adapters in stable [`AgentId`] order.
    pub fn iter(&self) -> impl Iterator<Item = &dyn AgentAdapter> {
        self.adapters.values().map(Box::as_ref)
    }

    /// Resolve all registered adapters against the Pi/Claude configuration.
    #[must_use]
    pub fn contexts(&self, config: &AgentsConfig, home: &Path) -> Vec<AgentContext> {
        self.iter()
            .map(|adapter| {
                let metadata = adapter.metadata();
                let (enabled, configured_dir) = match metadata.id {
                    AgentId::Pi => (&config.pi.enabled, &config.pi.session_dir),
                    AgentId::Claude => (&config.claude.enabled, &config.claude.session_dir),
                };
                let session_dir = if configured_dir.is_empty() {
                    adapter.default_session_dir(home)
                } else {
                    crate::config::expand_path_with_home(configured_dir, home)
                };
                AgentContext {
                    metadata,
                    enabled: *enabled,
                    session_dir,
                }
            })
            .collect()
    }

    /// Number of registered adapters.
    #[must_use]
    pub fn len(&self) -> usize {
        self.adapters.len()
    }

    /// Whether no adapters are registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.adapters.is_empty()
    }
}

/// Pi JSONL session adapter.
#[derive(Debug, Default)]
pub struct PiAdapter;

impl AgentAdapter for PiAdapter {
    fn metadata(&self) -> AgentMetadata {
        AgentMetadata {
            id: AgentId::Pi,
            display_name: "Pi",
            config_key: "agents.pi",
            repo_rel_base: "pi/sessions",
        }
    }

    fn default_session_dir(&self, home: &Path) -> PathBuf {
        home.join(".pi").join("agent").join("sessions")
    }

    fn canonicalize_dir(&self, registry: &TokenRegistry, name: &str) -> String {
        registry.canonicalize_pi_dir(name)
    }

    fn decanonicalize_dir(&self, registry: &TokenRegistry, name: &str) -> String {
        registry.decanonicalize_pi_dir(name)
    }

    fn repository_artifact_recency(&self, path: &Path) -> Option<DateTime<Utc>> {
        let filename = path.file_name()?.to_str()?;
        let stem = filename.strip_suffix(".jsonl")?;
        let (timestamp, _) = stem.split_once('_')?;
        let (date, time) = timestamp.split_once('T')?;
        let mut components = time.splitn(4, '-');
        let (hour, minute, second, milliseconds) = (
            components.next()?,
            components.next()?,
            components.next()?,
            components.next()?.strip_suffix('Z')?,
        );
        DateTime::parse_from_rfc3339(&format!("{date}T{hour}:{minute}:{second}.{milliseconds}Z"))
            .ok()
            .map(|timestamp| timestamp.with_timezone(&Utc))
    }
}

/// Claude Code JSONL session adapter.
#[derive(Debug, Default)]
pub struct ClaudeAdapter;

impl AgentAdapter for ClaudeAdapter {
    fn metadata(&self) -> AgentMetadata {
        AgentMetadata {
            id: AgentId::Claude,
            display_name: "Claude",
            config_key: "agents.claude",
            repo_rel_base: "claude/projects",
        }
    }

    fn default_session_dir(&self, home: &Path) -> PathBuf {
        home.join(".claude").join("projects")
    }

    fn canonicalize_dir(&self, registry: &TokenRegistry, name: &str) -> String {
        registry.canonicalize_claude_dir(name)
    }

    fn decanonicalize_dir(&self, registry: &TokenRegistry, name: &str) -> String {
        registry.decanonicalize_claude_dir(name)
    }

    fn repository_artifact_recency(&self, path: &Path) -> Option<DateTime<Utc>> {
        fs::read_to_string(path)
            .ok()?
            .lines()
            .filter(|line| !line.is_empty())
            .filter_map(|line| {
                let value: serde_json::Value = serde_json::from_str(line).ok()?;
                for field in ["timestamp", "created_at", "createdAt"] {
                    if let Some(timestamp) = value.get(field).and_then(|value| value.as_str()) {
                        if let Ok(timestamp) = DateTime::parse_from_rfc3339(timestamp) {
                            return Some(timestamp.with_timezone(&Utc));
                        }
                    }
                }
                None
            })
            .min()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canon::TokenRegistry;
    use crate::config::schema::{CanonicalizationConfig, PiAgentConfig};

    fn registry(home: &Path) -> TokenRegistry {
        TokenRegistry::from_config(&CanonicalizationConfig::default(), home)
    }

    #[test]
    fn defaults_are_stably_ordered() {
        let registry = AdapterRegistry::with_defaults();
        let ids: Vec<_> = registry
            .iter()
            .map(|adapter| adapter.metadata().id)
            .collect();
        assert_eq!(ids, vec![AgentId::Pi, AgentId::Claude]);
    }

    #[test]
    fn resolves_configured_contexts() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path();
        let config = AgentsConfig {
            pi: PiAgentConfig {
                enabled: false,
                session_dir: "~/custom-pi".to_owned(),
            },
            ..AgentsConfig::default()
        };
        let contexts = AdapterRegistry::with_defaults().contexts(&config, home);
        assert_eq!(contexts[0].metadata.id, AgentId::Pi);
        assert!(!contexts[0].enabled);
        assert_eq!(contexts[0].session_dir, home.join("custom-pi"));
        assert_eq!(contexts[1].session_dir, home.join(".claude/projects"));
    }

    #[test]
    fn adapters_preserve_agent_specific_directory_codecs() {
        let home = Path::new("/Users/alice");
        let tokens = registry(home);
        let adapters = AdapterRegistry::with_defaults();
        let pi = adapters.get(AgentId::Pi).unwrap();
        let claude = adapters.get(AgentId::Claude).unwrap();
        assert_eq!(
            pi.canonicalize_dir(&tokens, "--Users-alice-Dev-app--"),
            "--{{SYNC_HOME}}-Dev-app--"
        );
        assert_eq!(
            claude.canonicalize_dir(&tokens, "-Users-alice-Dev-app"),
            "-{{SYNC_HOME}}-Dev-app"
        );
        assert_eq!(
            pi.decanonicalize_dir(&tokens, "--{{SYNC_HOME}}-Dev-app--"),
            "--Users-alice-Dev-app--"
        );
        assert_eq!(
            claude.decanonicalize_dir(&tokens, "-{{SYNC_HOME}}-Dev-app"),
            "-Users-alice-Dev-app"
        );
    }

    #[test]
    fn artifact_maps_jsonl_under_canonical_session_directory() {
        let home = PathBuf::from("/Users/alice");
        let session_root = home.join(".pi/agent/sessions");
        let source = session_root.join("--Users-alice-Dev-app--/session.jsonl");
        let tokens = registry(&home);
        let adapter = PiAdapter;
        let artifact = adapter.artifact(&session_root, &source, &tokens).unwrap();
        assert_eq!(
            artifact.repo_relative_path,
            PathBuf::from("pi/sessions/--{{SYNC_HOME}}-Dev-app--/session.jsonl")
        );
    }

    #[test]
    fn artifact_rejects_non_jsonl_and_outside_paths() {
        let root = tempfile::tempdir().unwrap();
        let session_root = root.path().join("sessions");
        let tokens = registry(root.path());
        let adapter = ClaudeAdapter;
        assert!(matches!(
            adapter.artifact(&session_root, &session_root.join("x.txt"), &tokens),
            Err(AdapterError::NotJsonl(_))
        ));
        assert!(matches!(
            adapter.artifact(&session_root, &root.path().join("x.jsonl"), &tokens),
            Err(AdapterError::OutsideSessionRoot { .. })
        ));
    }

    #[test]
    fn pi_artifact_recency_uses_filename_timestamp() {
        let adapter = PiAdapter;
        assert_eq!(
            adapter
                .repository_artifact_recency(Path::new("2024-06-15T12-34-56-789Z_uuid.jsonl"))
                .unwrap()
                .to_rfc3339(),
            "2024-06-15T12:34:56.789+00:00"
        );
        assert!(adapter
            .repository_artifact_recency(Path::new("not-a-pi-session.jsonl"))
            .is_none());
    }

    #[test]
    fn claude_artifact_recency_uses_earliest_entry_timestamp() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("session.jsonl");
        fs::write(
            &path,
            concat!(
                r#"{"timestamp":"2024-06-15T12:00:00Z"}"#,
                "\n",
                r#"{"created_at":"2024-01-01T00:00:00Z"}"#,
                "\n",
                r#"{"createdAt":"2024-03-01T00:00:00Z"}"#
            ),
        )
        .unwrap();
        assert_eq!(
            ClaudeAdapter
                .repository_artifact_recency(&path)
                .unwrap()
                .to_rfc3339(),
            "2024-01-01T00:00:00+00:00"
        );
    }

    #[test]
    fn duplicate_adapter_is_rejected() {
        let mut registry = AdapterRegistry::default();
        registry.register(PiAdapter).unwrap();
        assert_eq!(
            registry.register(PiAdapter),
            Err(AdapterError::DuplicateAdapter(AgentId::Pi))
        );
    }

    #[test]
    fn outcomes_accumulate() {
        let mut outcome = AdapterOutcome {
            sessions: 1,
            files: 2,
        };
        outcome.extend(AdapterOutcome {
            sessions: 3,
            files: 4,
        });
        assert_eq!(
            outcome,
            AdapterOutcome {
                sessions: 4,
                files: 6
            }
        );
    }
}
