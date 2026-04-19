//! Reads user settings from config.toml.

use super::constants::{CONFIG_TOML, CONTEXT_DATA_DIR, DEFAULT_HISTORY_DAYS};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

pub const DEFAULT_STRATEGY_SCOPE: &str = "default";

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct Config {
    #[serde(default)]
    pub artifacts: ArtifactsConfig,
    #[serde(default)]
    pub tracking: TrackingConfig,
    #[serde(default)]
    pub memory_os: MemoryOsConfig,
    #[serde(default)]
    pub display: DisplayConfig,
    #[serde(default)]
    pub filters: FilterConfig,
    #[serde(default)]
    pub tee: crate::core::tee::TeeConfig,
    #[serde(default)]
    pub telemetry: TelemetryConfig,
    #[serde(default)]
    pub hooks: HooksConfig,
    #[serde(default)]
    pub limits: LimitsConfig,
    #[serde(default)]
    pub strategy: StrategyConfig,
    #[serde(default)]
    pub proactivity: ProactivityConfig,
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum ProactivityProvider {
    Claude,
    Codex,
}

impl ProactivityProvider {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ProactivityConfig {
    pub enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_scope: Option<String>,
    pub schedule_local: String,
    pub provider: ProactivityProvider,
    #[serde(default)]
    pub auto_spawn: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_path: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub queue_dir: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub results_dir: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub briefs_dir: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state_dir: Option<PathBuf>,
    pub max_spawns_per_day: u32,
    pub stale_claim_minutes: u64,
}

impl Default for ProactivityConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            default_scope: None,
            schedule_local: "08:00".to_string(),
            provider: ProactivityProvider::Claude,
            auto_spawn: true,
            project_path: None,
            queue_dir: None,
            results_dir: None,
            briefs_dir: None,
            state_dir: None,
            max_spawns_per_day: 1,
            stale_claim_minutes: 90,
        }
    }
}

impl ProactivityConfig {
    pub fn resolve_scope_name(&self, strategy: &StrategyConfig, requested: Option<&str>) -> String {
        requested
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
            .or_else(|| {
                self.default_scope
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(ToOwned::to_owned)
            })
            .unwrap_or_else(|| strategy.resolve_scope_name(None))
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct StrategyConfig {
    pub enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_scope: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub directory: Option<PathBuf>,
    #[serde(default)]
    pub scopes: BTreeMap<String, StrategyScopeConfig>,
}

impl Default for StrategyConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            default_scope: None,
            directory: None,
            scopes: BTreeMap::new(),
        }
    }
}

impl StrategyConfig {
    pub fn configured_scope_name(&self, requested: Option<&str>) -> Option<String> {
        requested
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
            .or_else(|| {
                self.default_scope
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(ToOwned::to_owned)
            })
    }

    pub fn resolve_scope_name(&self, requested: Option<&str>) -> String {
        self.configured_scope_name(requested)
            .unwrap_or_else(|| DEFAULT_STRATEGY_SCOPE.to_string())
    }

    pub fn scope(&self, requested: Option<&str>) -> Option<(&str, &StrategyScopeConfig)> {
        let scope_name = self.configured_scope_name(requested)?;
        self.scopes
            .get_key_value(&scope_name)
            .map(|(name, config)| (name.as_str(), config))
    }

    pub fn ensure_scope(&mut self, scope: impl Into<String>) -> &mut StrategyScopeConfig {
        self.scopes.entry(scope.into()).or_default()
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct StrategyScopeConfig {
    #[serde(default = "strategy_scope_enabled_default")]
    pub enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact_path: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metrics_path: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub continuity_project_path: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub storage_dir: Option<PathBuf>,
    #[serde(default)]
    pub signal_paths: Vec<PathBuf>,
}

impl Default for StrategyScopeConfig {
    fn default() -> Self {
        Self {
            enabled: strategy_scope_enabled_default(),
            label: None,
            artifact_path: None,
            metrics_path: None,
            continuity_project_path: None,
            storage_dir: None,
            signal_paths: Vec::new(),
        }
    }
}

fn strategy_scope_enabled_default() -> bool {
    true
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct MemoryOsConfig {
    pub journal_v1: bool,
    pub dual_write_v1: bool,
    pub proof_capture_v1: bool,
    pub openloop_v1: bool,
    pub checkpoint_v1: bool,
    pub action_v1: bool,
    pub trust_v1: bool,
    pub strict_promotion_v1: bool,
    pub read_model_v1: bool,
    pub dual_run_v1: bool,
    pub resume_v1: bool,
    pub handoff_v1: bool,
}

impl Default for MemoryOsConfig {
    fn default() -> Self {
        Self {
            journal_v1: false,
            dual_write_v1: false,
            proof_capture_v1: false,
            openloop_v1: false,
            checkpoint_v1: false,
            action_v1: false,
            trust_v1: false,
            strict_promotion_v1: true,
            read_model_v1: true,
            dual_run_v1: false,
            resume_v1: true,
            handoff_v1: true,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ArtifactsConfig {
    pub enabled: bool,
    pub min_chars: usize,
    pub min_lines: usize,
    pub preview_chars: usize,
    pub delta_lines: usize,
    pub recent_lookup_limit: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub directory: Option<PathBuf>,
}

impl Default for ArtifactsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            min_chars: 4000,
            min_lines: 80,
            preview_chars: 800,
            delta_lines: 12,
            recent_lookup_limit: 200,
            directory: None,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct HooksConfig {
    /// Commands to exclude from auto-rewrite (e.g. ["curl", "playwright"]).
    /// Survives reinstall runs since config.toml is user-owned.
    #[serde(default)]
    pub exclude_commands: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TrackingConfig {
    pub enabled: bool,
    pub history_days: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub database_path: Option<PathBuf>,
}

impl Default for TrackingConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            history_days: DEFAULT_HISTORY_DAYS as u32,
            database_path: None,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DisplayConfig {
    pub colors: bool,
    pub emoji: bool,
    pub max_width: usize,
}

impl Default for DisplayConfig {
    fn default() -> Self {
        Self {
            colors: true,
            emoji: true,
            max_width: 120,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct FilterConfig {
    pub ignore_dirs: Vec<String>,
    pub ignore_files: Vec<String>,
}

impl Default for FilterConfig {
    fn default() -> Self {
        Self {
            ignore_dirs: vec![
                ".git".into(),
                "node_modules".into(),
                "target".into(),
                "__pycache__".into(),
                ".venv".into(),
                "vendor".into(),
            ],
            ignore_files: vec!["*.lock".into(), "*.min.js".into(), "*.min.css".into()],
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TelemetryConfig {
    pub enabled: bool,
}

impl Default for TelemetryConfig {
    fn default() -> Self {
        Self { enabled: true }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct LimitsConfig {
    /// Max total grep results to show (default: 200)
    pub grep_max_results: usize,
    /// Max matches per file in grep output (default: 25)
    pub grep_max_per_file: usize,
    /// Max staged/modified files shown in git status (default: 15)
    pub status_max_files: usize,
    /// Max untracked files shown in git status (default: 10)
    pub status_max_untracked: usize,
    /// Max chars for parser passthrough fallback (default: 2000)
    pub passthrough_max_chars: usize,
}

impl Default for LimitsConfig {
    fn default() -> Self {
        Self {
            grep_max_results: 200,
            grep_max_per_file: 25,
            status_max_files: 15,
            status_max_untracked: 10,
            passthrough_max_chars: 2000,
        }
    }
}

/// Get limits config. Falls back to defaults if config can't be loaded.
pub fn limits() -> LimitsConfig {
    Config::load().map(|c| c.limits).unwrap_or_default()
}

/// Check if telemetry is enabled in config. Returns None if config can't be loaded.
#[allow(dead_code)]
pub fn telemetry_enabled() -> Option<bool> {
    Config::load().ok().map(|c| c.telemetry.enabled)
}

fn env_flag(name: &str) -> Option<bool> {
    std::env::var(name)
        .ok()
        .and_then(|value| match value.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => Some(true),
            "0" | "false" | "no" | "off" => Some(false),
            _ => None,
        })
}

/// Resolve memory OS feature flags from config with optional env overrides for local experimentation.
pub fn memory_os() -> MemoryOsConfig {
    let mut config = Config::load().map(|c| c.memory_os).unwrap_or_default();
    if let Some(value) = env_flag("CONTEXT_MEMORYOS_JOURNAL_V1") {
        config.journal_v1 = value;
    }
    if let Some(value) = env_flag("CONTEXT_MEMORYOS_DUAL_WRITE_V1") {
        config.dual_write_v1 = value;
    }
    if let Some(value) = env_flag("CONTEXT_MEMORYOS_PROOF_CAPTURE_V1") {
        config.proof_capture_v1 = value;
    }
    if let Some(value) = env_flag("CONTEXT_MEMORYOS_OPENLOOP_V1") {
        config.openloop_v1 = value;
    }
    if let Some(value) = env_flag("CONTEXT_MEMORYOS_CHECKPOINT_V1") {
        config.checkpoint_v1 = value;
    }
    if let Some(value) = env_flag("CONTEXT_MEMORYOS_ACTION_V1") {
        config.action_v1 = value;
    }
    if let Some(value) = env_flag("CONTEXT_MEMORYOS_TRUST_V1") {
        config.trust_v1 = value;
    }
    if let Some(value) = env_flag("CONTEXT_MEMORYOS_STRICT_PROMOTION_V1") {
        config.strict_promotion_v1 = value;
    }
    if let Some(value) = env_flag("CONTEXT_MEMORYOS_READ_MODEL_V1") {
        config.read_model_v1 = value;
    }
    if let Some(value) = env_flag("CONTEXT_MEMORYOS_DUAL_RUN_V1") {
        config.dual_run_v1 = value;
    }
    if let Some(value) = env_flag("CONTEXT_MEMORYOS_RESUME_V1") {
        config.resume_v1 = value;
    }
    if let Some(value) = env_flag("CONTEXT_MEMORYOS_HANDOFF_V1") {
        config.handoff_v1 = value;
    }
    config
}

impl Config {
    pub fn load() -> Result<Self> {
        let path = config_path()?;

        if path.exists() {
            let content = std::fs::read_to_string(&path)?;
            let config: Config = toml::from_str(&content)?;
            Ok(config)
        } else {
            Ok(Config::default())
        }
    }

    pub fn save(&self) -> Result<()> {
        let path = config_path()?;

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let content = toml::to_string_pretty(self)?;
        std::fs::write(&path, content)?;
        Ok(())
    }

    pub fn create_default() -> Result<PathBuf> {
        let config = Config::default();
        config.save()?;
        config_path()
    }
}

pub fn context_config_dir() -> Result<PathBuf> {
    if let Ok(path) = std::env::var("CONTEXT_CONFIG_DIR") {
        return Ok(PathBuf::from(path));
    }
    let config_dir = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
    Ok(config_dir.join(CONTEXT_DATA_DIR))
}

pub fn context_data_dir() -> Result<PathBuf> {
    if let Ok(path) = std::env::var("MUNIN_DATA_DIR") {
        return Ok(PathBuf::from(path));
    }
    if let Ok(path) = std::env::var("CONTEXT_DATA_DIR_PATH") {
        return Ok(PathBuf::from(path));
    }
    let data_dir = dirs::data_local_dir().unwrap_or_else(|| PathBuf::from("."));
    Ok(data_dir.join(CONTEXT_DATA_DIR))
}

pub fn config_path() -> Result<PathBuf> {
    Ok(context_config_dir()?.join(CONFIG_TOML))
}

pub fn show_config() -> Result<()> {
    let path = config_path()?;
    println!("Config: {}", path.display());
    println!();

    if path.exists() {
        let config = Config::load()?;
        println!("{}", toml::to_string_pretty(&config)?);
    } else {
        println!("(default config, file not created)");
        println!();
        let config = Config::default();
        println!("{}", toml::to_string_pretty(&config)?);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn test_hooks_config_deserialize() {
        let toml = r#"
[hooks]
exclude_commands = ["curl", "gh"]
"#;
        let config: Config = toml::from_str(toml).expect("valid toml");
        assert_eq!(config.hooks.exclude_commands, vec!["curl", "gh"]);
    }

    #[test]
    fn test_hooks_config_default_empty() {
        let config = Config::default();
        assert!(config.hooks.exclude_commands.is_empty());
    }

    #[test]
    fn test_config_without_hooks_section_is_valid() {
        let toml = r#"
[tracking]
enabled = true
history_days = 90
"#;
        let config: Config = toml::from_str(toml).expect("valid toml");
        assert!(config.hooks.exclude_commands.is_empty());
    }

    #[test]
    fn test_memory_os_phase5_cutover_defaults_are_enabled() {
        let config = Config::default();
        assert!(!config.memory_os.journal_v1);
        assert!(!config.memory_os.dual_write_v1);
        assert!(!config.memory_os.action_v1);
        assert!(config.memory_os.strict_promotion_v1);
        assert!(config.memory_os.read_model_v1);
        assert!(config.memory_os.resume_v1);
        assert!(config.memory_os.handoff_v1);
    }

    #[test]
    fn test_strategy_scope_deserialize() {
        let toml = r#"
[strategy]
enabled = true
default_scope = "sitesorted-business"

[strategy.scopes.sitesorted-business]
artifact_path = "C:/strategy/opsp.md"
metrics_path = "C:/strategy/metrics.json"
continuity_project_path = "C:/Users/OEM/Projects/sitesorted"
storage_dir = "C:/strategy/state"
"#;
        let config: Config = toml::from_str(toml).expect("valid toml");
        let (scope_name, scope) = config
            .strategy
            .scope(None)
            .expect("default strategy scope should resolve");

        assert!(config.strategy.enabled);
        assert_eq!(scope_name, "sitesorted-business");
        assert_eq!(
            scope.artifact_path.as_deref(),
            Some(PathBuf::from("C:/strategy/opsp.md").as_path())
        );
        assert_eq!(
            scope.storage_dir.as_deref(),
            Some(PathBuf::from("C:/strategy/state").as_path())
        );
        assert_eq!(
            scope.metrics_path.as_deref(),
            Some(PathBuf::from("C:/strategy/metrics.json").as_path())
        );
        assert_eq!(
            scope.continuity_project_path.as_deref(),
            Some(PathBuf::from("C:/Users/OEM/Projects/sitesorted").as_path())
        );
    }

    #[test]
    fn test_strategy_config_default_does_not_configure_cross_project_scope() {
        let config = Config::default();
        assert_eq!(config.strategy.configured_scope_name(None), None);
        assert!(config.strategy.scope(None).is_none());
        assert_eq!(
            config.strategy.resolve_scope_name(None),
            DEFAULT_STRATEGY_SCOPE
        );
    }

    #[test]
    fn test_strategy_ensure_scope_creates_generic_scope_config() {
        let mut config = Config::default();
        let scope = config.strategy.ensure_scope("custom-business");
        scope.artifact_path = Some(PathBuf::from("C:/plans/custom.md"));

        let resolved = config
            .strategy
            .scope(Some("custom-business"))
            .expect("custom scope should exist");

        assert_eq!(resolved.0, "custom-business");
        assert_eq!(
            resolved.1.artifact_path.as_deref(),
            Some(PathBuf::from("C:/plans/custom.md").as_path())
        );
    }

    #[test]
    fn test_proactivity_config_deserializes() {
        let toml = r#"
[proactivity]
enabled = true
default_scope = "sitesorted-business"
schedule_local = "08:00"
provider = "codex"
auto_spawn = true
project_path = "C:/Users/OEM/Projects/sitesorted"
queue_dir = "C:/Users/OEM/AppData/Local/context/proactivity/queue"
results_dir = "C:/Users/OEM/AppData/Local/context/proactivity/results"
briefs_dir = "C:/Users/OEM/AppData/Local/context/proactivity/briefs"
state_dir = "C:/Users/OEM/AppData/Local/context/proactivity/state"
max_spawns_per_day = 2
stale_claim_minutes = 120
"#;
        let config: Config = toml::from_str(toml).expect("valid toml");
        assert!(config.proactivity.enabled);
        assert_eq!(
            config.proactivity.default_scope.as_deref(),
            Some("sitesorted-business")
        );
        assert_eq!(config.proactivity.schedule_local, "08:00");
        assert_eq!(config.proactivity.provider, ProactivityProvider::Codex);
        assert!(config.proactivity.auto_spawn);
        assert_eq!(
            config.proactivity.project_path.as_deref(),
            Some(PathBuf::from("C:/Users/OEM/Projects/sitesorted").as_path())
        );
        assert_eq!(config.proactivity.max_spawns_per_day, 2);
        assert_eq!(config.proactivity.stale_claim_minutes, 120);
    }

    #[test]
    fn test_context_dir_helpers_respect_env_overrides() {
        let _guard = ENV_LOCK.lock().expect("env lock");
        std::env::set_var("CONTEXT_CONFIG_DIR", "C:/tmp/context-config");
        std::env::set_var("CONTEXT_DATA_DIR_PATH", "C:/tmp/context-data");

        assert_eq!(
            context_config_dir().expect("config dir"),
            PathBuf::from("C:/tmp/context-config")
        );
        assert_eq!(
            context_data_dir().expect("data dir"),
            PathBuf::from("C:/tmp/context-data")
        );

        std::env::remove_var("CONTEXT_CONFIG_DIR");
        std::env::remove_var("CONTEXT_DATA_DIR_PATH");
    }
}
