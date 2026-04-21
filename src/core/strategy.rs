//! Minimal durable business-strategy kernel and scorecard support.
use super::config::{context_data_dir, Config, StrategyScopeConfig};
use crate::core::memory_os::MemoryOsInspectionScope;
use crate::core::tracking::Tracker;
use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

const STRATEGY_DIR: &str = "strategy";
const STRATEGY_KERNEL_FILE: &str = "kernel.json";
const STRATEGY_REGISTRY_FILE: &str = "registry.json";
const STRATEGY_TEMPLATE_MD_FILE: &str = "strategic-plan.md";
const STRATEGY_TEMPLATE_JSON_FILE: &str = "strategic-plan.context.json";
const STRATEGY_DEFAULT_METRICS_FILE: &str = "metrics.json";

#[derive(Debug, Clone)]
pub struct StrategySetupOptions {
    pub scope: String,
    pub import_path: Option<PathBuf>,
    pub bootstrap_claude: bool,
    pub template: bool,
}

#[derive(Debug, Clone)]
pub struct StrategyReadOptions {
    pub scope: String,
}

#[derive(Debug, Clone)]
pub struct StrategyMetricSetOptions {
    pub scope: String,
    pub metric_key: String,
    pub value: f64,
    pub unit: Option<String>,
    pub updated_at: Option<String>,
}

#[derive(Debug, Clone)]
pub struct StrategyMetricGetOptions {
    pub scope: String,
    pub metric_key: Option<String>,
}

#[derive(Debug, Clone)]
pub struct StrategyMetricSyncOptions {
    pub scope: String,
    pub from_path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StrategySourceRef {
    pub source_id: String,
    pub source_path: String,
    pub section_path: String,
    pub line_start: usize,
    pub line_end: usize,
    pub excerpt: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StrategySourceDocument {
    pub source_id: String,
    pub source_type: String,
    pub path: String,
    pub content_hash: String,
    pub imported_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StrategyGoal {
    pub goal_id: String,
    pub horizon: String,
    pub title: String,
    pub summary: String,
    pub due_date: Option<String>,
    pub source_refs: Vec<StrategySourceRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StrategyKpi {
    pub kpi_id: String,
    pub title: String,
    pub metric_key: String,
    pub unit: Option<String>,
    pub target: Option<f64>,
    pub green_threshold: Option<f64>,
    pub yellow_threshold: Option<f64>,
    pub cadence: Option<String>,
    pub due_date: Option<String>,
    pub goal_ids: Vec<String>,
    pub initiative_ids: Vec<String>,
    pub source_refs: Vec<StrategySourceRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StrategyInitiative {
    pub initiative_id: String,
    pub kind: String,
    pub title: String,
    pub owner: Option<String>,
    pub due_date: Option<String>,
    pub depends_on: Vec<String>,
    pub supports_goal_ids: Vec<String>,
    pub deferred: bool,
    pub source_refs: Vec<StrategySourceRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StrategyConstraint {
    pub constraint_id: String,
    pub title: String,
    pub suppression_kind: String,
    pub summary: Option<String>,
    pub source_refs: Vec<StrategySourceRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StrategyAssumption {
    pub assumption_id: String,
    #[serde(default)]
    pub statement: String,
    pub source_refs: Vec<StrategySourceRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StrategyKernel {
    pub schema_version: String,
    pub scope_id: String,
    pub imported_at: String,
    pub sources: Vec<StrategySourceDocument>,
    pub goals: Vec<StrategyGoal>,
    pub kpis: Vec<StrategyKpi>,
    pub initiatives: Vec<StrategyInitiative>,
    pub constraints: Vec<StrategyConstraint>,
    pub assumptions: Vec<StrategyAssumption>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StrategySourceRegistry {
    pub schema_version: String,
    pub scope_id: String,
    pub artifact_path: PathBuf,
    pub metrics_path: PathBuf,
    pub continuity_project_path: Option<PathBuf>,
    pub signal_paths: Vec<PathBuf>,
    pub storage_dir: PathBuf,
    pub bootstrap_requested: bool,
    pub template_managed: bool,
    pub imported_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StrategyMetricRecord {
    pub current: Option<f64>,
    pub unit: Option<String>,
    pub updated_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct StrategyInitiativeSignal {
    pub status: Option<String>,
    pub blocked: Option<bool>,
    pub updated_at: Option<String>,
    #[serde(default)]
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct StrategyMetricsSnapshot {
    pub generated_at: Option<String>,
    #[serde(default)]
    pub kpis: BTreeMap<String, StrategyMetricRecord>,
    #[serde(default)]
    pub instrumentation: BTreeMap<String, bool>,
    #[serde(default)]
    pub dependency_states: BTreeMap<String, bool>,
    #[serde(default)]
    pub initiatives: BTreeMap<String, StrategyInitiativeSignal>,
    #[serde(default)]
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct StrategyImportJsonDocument {
    #[serde(default)]
    organization: StrategyImportJsonOrganization,
    #[serde(default)]
    goals: Vec<StrategyImportJsonGoal>,
    #[serde(default)]
    kpis: Vec<StrategyImportJsonKpi>,
    #[serde(default)]
    initiatives: Vec<StrategyImportJsonInitiative>,
    #[serde(default)]
    constraints: Vec<StrategyImportJsonConstraint>,
    #[serde(default)]
    assumptions: Vec<StrategyImportJsonAssumption>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
struct StrategyImportJsonOrganization {
    #[serde(default)]
    scope_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct StrategyImportJsonGoal {
    id: String,
    title: String,
    horizon: String,
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    due_date: Option<String>,
    #[serde(default)]
    source_section: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct StrategyImportJsonLineage {
    #[serde(default)]
    goal_ids: Vec<String>,
    #[serde(default)]
    initiative_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct StrategyImportJsonKpi {
    id: String,
    title: String,
    metric_key: String,
    #[serde(default)]
    unit: Option<String>,
    #[serde(default)]
    cadence: Option<String>,
    #[serde(default)]
    target: Option<f64>,
    #[serde(default)]
    green_threshold: Option<f64>,
    #[serde(default)]
    yellow_threshold: Option<f64>,
    #[serde(default)]
    due_date: Option<String>,
    #[serde(default)]
    source_section: Option<String>,
    #[serde(default)]
    lineage: Option<StrategyImportJsonLineage>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct StrategyImportJsonInitiative {
    id: String,
    title: String,
    kind: String,
    #[serde(default)]
    owner: Option<String>,
    #[serde(default)]
    due_date: Option<String>,
    #[serde(default)]
    depends_on: Vec<String>,
    #[serde(default)]
    supports_goal_ids: Vec<String>,
    #[serde(default)]
    deferred: bool,
    #[serde(default)]
    source_section: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct StrategyImportJsonConstraint {
    id: String,
    title: String,
    suppression_kind: String,
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    source_section: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct StrategyImportJsonAssumption {
    id: String,
    #[serde(default)]
    statement: String,
    #[serde(default)]
    source_section: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StrategyContinuitySnapshot {
    pub active: bool,
    pub summary: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StrategySetupReport {
    pub generated_at: String,
    pub scope_id: String,
    pub artifact_path: String,
    pub metrics_path: String,
    pub continuity_project_path: Option<String>,
    pub storage_dir: String,
    pub registry_path: String,
    pub kernel_path: String,
    pub bootstrap_requested: bool,
    pub template_managed: bool,
    pub imported_goal_count: usize,
    pub imported_kpi_count: usize,
    pub imported_initiative_count: usize,
    pub imported_constraint_count: usize,
    pub imported_assumption_count: usize,
    pub next_step_hint: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StrategyInspectReport {
    pub generated_at: String,
    pub scope_id: String,
    pub registry: StrategySourceRegistry,
    pub kernel: StrategyKernel,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StrategyStatusItem {
    pub item_id: String,
    pub item_kind: String,
    pub title: String,
    pub supports: Vec<String>,
    pub status: String,
    pub evidence: Vec<String>,
    pub evidence_freshness: String,
    pub confidence: String,
    pub missing_instrumentation: bool,
    pub due_date: Option<String>,
    pub depends_on: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StrategyKernelSummary {
    pub goals: usize,
    pub kpis: usize,
    pub initiatives: usize,
    pub constraints: usize,
    pub assumptions: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StrategyStatusReport {
    pub generated_at: String,
    pub scope_id: String,
    pub registry: StrategySourceRegistry,
    pub kernel_summary: StrategyKernelSummary,
    pub continuity: StrategyContinuitySnapshot,
    pub items: Vec<StrategyStatusItem>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StrategicNudge {
    pub task: String,
    pub item_id: Option<String>,
    pub item_kind: String,
    pub supports: Vec<String>,
    pub why_now: String,
    pub evidence: Vec<String>,
    pub evidence_freshness: String,
    pub confidence: String,
    pub interrupt_level: String,
    pub suppression_reason: Option<String>,
    pub expected_effect: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NudgeTask {
    pub task: String,
    pub source: String,
    pub why_now: String,
    pub evidence: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StrategyRecommendReport {
    pub generated_at: String,
    pub scope_id: String,
    pub continuity: StrategyContinuitySnapshot,
    pub nudge_tasks: Vec<String>,
    pub continuity_tasks: Vec<NudgeTask>,
    pub nudges: Vec<StrategicNudge>,
    pub suppressed_nudges: Vec<StrategicNudge>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StrategyMetricsReport {
    pub generated_at: String,
    pub scope_id: String,
    pub metrics_path: String,
    pub kpis: BTreeMap<String, StrategyMetricRecord>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone)]
struct StrategyStorePaths {
    storage_dir: PathBuf,
    registry_path: PathBuf,
    kernel_path: PathBuf,
    metrics_path: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StrategySectionKind {
    Goal,
    Kpi,
    Initiative,
    Constraint,
    Assumption,
    Ignore,
}

pub fn setup(options: &StrategySetupOptions) -> Result<StrategySetupReport> {
    let mut config = Config::load().context("Failed to load config.toml")?;
    config.strategy.enabled = true;
    let scope_id = config.strategy.resolve_scope_name(Some(&options.scope));
    config.strategy.default_scope = Some(scope_id.clone());
    {
        let scope_config = config.strategy.ensure_scope(scope_id.clone());
        if scope_config.label.is_none() {
            scope_config.label = Some(scope_id.clone());
        }
    }
    let mut scope_config = config
        .strategy
        .scope(Some(&scope_id))
        .map(|(_, config)| config.clone())
        .expect("strategy scope should exist");

    let store_paths = resolve_store_paths(&config, scope_id.as_str(), &scope_config)?;
    fs::create_dir_all(&store_paths.storage_dir)
        .with_context(|| format!("Failed to create {}", store_paths.storage_dir.display()))?;

    let existing_registry = load_registry(&store_paths.registry_path).ok();
    let existing_staged = existing_registry
        .as_ref()
        .map(|registry| registry.template_managed || registry.bootstrap_requested)
        .unwrap_or(false);
    let bootstrap_requested = if options.import_path.is_some() {
        false
    } else {
        options.bootstrap_claude
            || existing_registry
                .as_ref()
                .map(|registry| registry.bootstrap_requested)
                .unwrap_or(false)
    };
    let template_managed = if options.import_path.is_some() {
        false
    } else {
        options.template
            || options.bootstrap_claude
            || existing_registry
                .as_ref()
                .map(|registry| registry.template_managed)
                .unwrap_or(false)
    };
    let mut next_step_hint = None;
    let stage_only = options.import_path.is_none()
        && (options.template || options.bootstrap_claude || existing_staged);
    let artifact_path = if let Some(path) = &options.import_path {
        path.clone()
    } else if options.template || options.bootstrap_claude {
        let markdown_path = store_paths.storage_dir.join(STRATEGY_TEMPLATE_MD_FILE);
        let json_path = store_paths.storage_dir.join(STRATEGY_TEMPLATE_JSON_FILE);
        if !markdown_path.exists() {
            fs::write(&markdown_path, strategy_template(scope_id.as_str())).with_context(|| {
                format!(
                    "Failed to write strategy markdown template to {}",
                    markdown_path.display()
                )
            })?;
        }
        if !json_path.exists() {
            fs::write(&json_path, strategy_json_template(scope_id.as_str())).with_context(
                || {
                    format!(
                        "Failed to write strategy JSON template to {}",
                        json_path.display()
                    )
                },
            )?;
        }
        if options.bootstrap_claude {
            next_step_hint = Some(format!(
                "First-cut strategy kernel staged at `{}` (markdown companion at `{}`). It uses inferred founder defaults — refine in chat via the `/munin-strategy` skill, then re-run `munin strategy setup --scope {} --import {}` to commit changes.",
                json_path.display(),
                markdown_path.display(),
                scope_id,
                json_path.display()
            ));
        } else {
            next_step_hint = Some(format!(
                "Templates created at `{}` and `{}`. Fill them with real strategy content and re-run `munin strategy setup --scope {} --import {}` using the JSON sidecar.",
                markdown_path.display(),
                json_path.display(),
                scope_id,
                json_path.display()
            ));
        }
        json_path
    } else if existing_staged {
        let staged_path = existing_registry
            .as_ref()
            .map(|registry| registry.artifact_path.clone())
            .or_else(|| scope_config.artifact_path.clone())
            .ok_or_else(|| anyhow!("No staged strategy artifact is configured for `{scope_id}`"))?;
        next_step_hint = Some(format!(
            "A staged strategy artifact already exists at `{}`. Populate the markdown companion and JSON sidecar with real strategy content, then re-run `munin strategy setup --scope {} --import {}` using the JSON sidecar.",
            staged_path.display(),
            scope_id,
            staged_path.display()
        ));
        staged_path
    } else if let Some(existing) = scope_config.artifact_path.clone() {
        existing
    } else {
        return Err(anyhow!(
            "strategy setup requires --import, --template, or --bootstrap-claude"
        ));
    };

    scope_config.artifact_path = Some(artifact_path.clone());
    scope_config.storage_dir = Some(store_paths.storage_dir.clone());
    scope_config.metrics_path = Some(
        scope_config
            .metrics_path
            .clone()
            .unwrap_or_else(|| store_paths.metrics_path.clone()),
    );
    *config.strategy.ensure_scope(scope_id.clone()) = scope_config.clone();
    config.save().context("Failed to save strategy config")?;

    ensure_metrics_file(
        scope_config.metrics_path.as_ref().expect("metrics path"),
        scope_id.as_str(),
    )?;
    let registry = build_source_registry(
        scope_id.as_str(),
        &artifact_path,
        &scope_config,
        &store_paths,
        bootstrap_requested,
        template_managed,
    )?;
    let kernel = if stage_only {
        StrategyKernel {
            schema_version: "strategy-kernel-v1".to_string(),
            scope_id: scope_id.clone(),
            imported_at: Utc::now().to_rfc3339(),
            sources: Vec::new(),
            goals: Vec::new(),
            kpis: Vec::new(),
            initiatives: Vec::new(),
            constraints: Vec::new(),
            assumptions: Vec::new(),
        }
    } else {
        import_strategy_kernel(scope_id.as_str(), &artifact_path)?
    };
    save_registry(&store_paths.registry_path, &registry)?;
    save_kernel(&store_paths.kernel_path, &kernel)?;

    Ok(StrategySetupReport {
        generated_at: Utc::now().to_rfc3339(),
        scope_id,
        artifact_path: registry.artifact_path.display().to_string(),
        metrics_path: registry.metrics_path.display().to_string(),
        continuity_project_path: registry
            .continuity_project_path
            .as_ref()
            .map(|path| path.display().to_string()),
        storage_dir: registry.storage_dir.display().to_string(),
        registry_path: store_paths.registry_path.display().to_string(),
        kernel_path: store_paths.kernel_path.display().to_string(),
        bootstrap_requested: registry.bootstrap_requested,
        template_managed: registry.template_managed,
        imported_goal_count: kernel.goals.len(),
        imported_kpi_count: kernel.kpis.len(),
        imported_initiative_count: kernel.initiatives.len(),
        imported_constraint_count: kernel.constraints.len(),
        imported_assumption_count: kernel.assumptions.len(),
        next_step_hint,
    })
}

pub fn inspect(options: &StrategyReadOptions) -> Result<StrategyInspectReport> {
    let (_, registry, kernel) = load_scope_bundle(options.scope.as_str())?;
    Ok(StrategyInspectReport {
        generated_at: Utc::now().to_rfc3339(),
        scope_id: options.scope.clone(),
        registry,
        kernel,
    })
}

pub fn default_strategy_scope_hint() -> String {
    if let Ok(config) = Config::load() {
        if let Some(name) = config.strategy.configured_scope_name(None) {
            return name;
        }
    }
    crate::core::config::DEFAULT_STRATEGY_SCOPE.to_string()
}

pub fn discover_inspect_reports(limit: usize) -> Result<Vec<StrategyInspectReport>> {
    let config = Config::load().context("Failed to load config.toml")?;
    let strategy_root = config
        .strategy
        .directory
        .clone()
        .unwrap_or(context_data_dir()?.join(STRATEGY_DIR));
    let mut scope_dirs = BTreeMap::new();

    for scope_name in config.strategy.scopes.keys() {
        scope_dirs.insert(scope_name.clone(), strategy_root.join(scope_name));
    }

    if let Some(default_scope) = config.strategy.configured_scope_name(None) {
        scope_dirs
            .entry(default_scope.clone())
            .or_insert_with(|| strategy_root.join(default_scope));
    }

    if strategy_root.exists() {
        for entry in fs::read_dir(&strategy_root)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let scope_name = entry.file_name().to_string_lossy().to_string();
            scope_dirs.entry(scope_name).or_insert_with(|| entry.path());
        }
    }

    let mut reports = Vec::new();
    for (scope_name, storage_dir) in scope_dirs {
        let registry_path = storage_dir.join(STRATEGY_REGISTRY_FILE);
        let kernel_path = storage_dir.join(STRATEGY_KERNEL_FILE);
        if !registry_path.exists() || !kernel_path.exists() {
            continue;
        }
        let registry = match load_registry(&registry_path) {
            Ok(registry) => registry,
            Err(_) => continue,
        };
        let mut kernel = match load_kernel(&kernel_path) {
            Ok(kernel) => kernel,
            Err(_) => continue,
        };
        if strategy_kernel_is_empty(&kernel) && registry.artifact_path.exists() {
            if let Ok(imported) =
                import_strategy_kernel(&registry.scope_id, &registry.artifact_path)
            {
                kernel = imported;
            }
        }
        reports.push(StrategyInspectReport {
            generated_at: Utc::now().to_rfc3339(),
            scope_id: if registry.scope_id.trim().is_empty() {
                scope_name
            } else {
                registry.scope_id.clone()
            },
            registry,
            kernel,
        });
    }

    reports.sort_by(|left, right| {
        strategy_kernel_signal_count(&right.kernel)
            .cmp(&strategy_kernel_signal_count(&left.kernel))
            .then(left.scope_id.cmp(&right.scope_id))
    });
    reports.truncate(limit);
    Ok(reports)
}

pub fn status(options: &StrategyReadOptions) -> Result<StrategyStatusReport> {
    let (paths, registry, kernel) = load_scope_bundle(options.scope.as_str())?;
    let mut metrics = load_metrics_snapshot(&paths.metrics_path)?;
    hydrate_metric_slots_from_kernel(&mut metrics, &kernel);
    let continuity = load_continuity_snapshot(registry.continuity_project_path.as_deref())?;
    let mut items = Vec::new();
    let mut warnings = Vec::new();

    for kpi in &kernel.kpis {
        items.push(status_for_kpi(kpi, &metrics));
    }
    for initiative in &kernel.initiatives {
        items.push(status_for_initiative(initiative, &metrics));
    }

    if items.is_empty() {
        warnings.push("Strategy kernel has no KPI or initiative items to score.".to_string());
    }
    if strategy_metrics_have_no_current_values(&metrics) {
        warnings.push(
            "Metrics snapshot has ingested KPI slots but no current values; strategy status can list KPIs but cannot mark them green/yellow yet."
                .to_string(),
        );
    }
    if metrics.generated_at.is_none() {
        warnings.push(
            "Metrics snapshot does not declare generated_at; freshness defaults to unknown."
                .to_string(),
        );
    }

    Ok(StrategyStatusReport {
        generated_at: Utc::now().to_rfc3339(),
        scope_id: options.scope.clone(),
        registry,
        kernel_summary: StrategyKernelSummary {
            goals: kernel.goals.len(),
            kpis: kernel.kpis.len(),
            initiatives: kernel.initiatives.len(),
            constraints: kernel.constraints.len(),
            assumptions: kernel.assumptions.len(),
        },
        continuity,
        items,
        warnings,
    })
}

fn strategy_kernel_is_empty(kernel: &StrategyKernel) -> bool {
    strategy_kernel_signal_count(kernel) == 0
}

fn strategy_metrics_have_no_current_values(metrics: &StrategyMetricsSnapshot) -> bool {
    metrics.kpis.values().all(|record| record.current.is_none())
        && metrics.instrumentation.is_empty()
        && metrics.dependency_states.is_empty()
        && metrics.initiatives.is_empty()
}

fn strategy_kernel_signal_count(kernel: &StrategyKernel) -> usize {
    kernel.goals.len()
        + kernel.kpis.len()
        + kernel.initiatives.len()
        + kernel.constraints.len()
        + kernel.assumptions.len()
}

pub fn recommend(options: &StrategyReadOptions) -> Result<StrategyRecommendReport> {
    let status_report = status(options)?;
    let (_, registry, kernel) = load_scope_bundle(options.scope.as_str())?;
    let mut nudges = Vec::new();
    let mut suppressed_nudges = deferred_suppression_nudges(&kernel);

    for item in &status_report.items {
        if let Some(nudge) = nudge_for_status_item(item) {
            nudges.push(nudge);
        }
    }

    nudges.sort_by(|left, right| {
        nudge_rank(right)
            .cmp(&nudge_rank(left))
            .then(left.task.cmp(&right.task))
    });

    let mut retained_nudges = Vec::new();
    let mut retained_instrumentation = false;
    for mut nudge in nudges.drain(..) {
        if nudge.task.starts_with("Instrument or measure") {
            if retained_instrumentation {
                nudge.interrupt_level = "defer".to_string();
                nudge.suppression_reason = Some("collapsed_to_single_metrics_setup".to_string());
                suppressed_nudges.push(nudge);
                continue;
            }
            retained_instrumentation = true;
        }
        retained_nudges.push(nudge);
    }
    nudges = retained_nudges;

    if status_report.continuity.active {
        let mut retained = Vec::new();
        for mut nudge in nudges.drain(..) {
            if !nudge_can_preempt_continuity(&nudge) {
                nudge.interrupt_level = "defer".to_string();
                nudge.suppression_reason = Some("continuity_preempts_strategy".to_string());
                suppressed_nudges.push(nudge);
            } else {
                retained.push(nudge);
            }
        }
        nudges = retained;
    }

    suppressed_nudges.sort_by(|left, right| left.task.cmp(&right.task));

    let mut warnings = status_report.warnings;
    if registry.bootstrap_requested && nudges.is_empty() {
        warnings.push(
            "Bootstrap mode is configured; populate the canonical artifact before trusting recommendation output."
                .to_string(),
        );
    }

    let continuity_tasks = continuity_nudge_tasks(registry.continuity_project_path.as_deref())?;
    let nudge_tasks = nudges
        .iter()
        .map(|nudge| nudge.task.clone())
        .chain(continuity_tasks.iter().map(|task| task.task.clone()))
        .collect();

    Ok(StrategyRecommendReport {
        generated_at: Utc::now().to_rfc3339(),
        scope_id: options.scope.clone(),
        continuity: status_report.continuity,
        nudge_tasks,
        continuity_tasks,
        nudges,
        suppressed_nudges,
        warnings,
    })
}

pub fn metrics_set(options: StrategyMetricSetOptions) -> Result<StrategyMetricsReport> {
    let (paths, _, kernel) = load_scope_bundle(options.scope.as_str())?;
    let (canonical_key, default_unit) =
        resolve_metric_key(&kernel, options.metric_key.as_str()).ok_or_else(|| {
            anyhow!(
                "unknown metric key `{}`; expected one of the strategy KPI metric_key or kpi_id values",
                options.metric_key
            )
        })?;
    let mut snapshot = load_metrics_snapshot(&paths.metrics_path)?;
    snapshot.generated_at = Some(Utc::now().to_rfc3339());
    snapshot.kpis.insert(
        canonical_key,
        StrategyMetricRecord {
            current: Some(options.value),
            unit: options.unit.or(default_unit),
            updated_at: Some(
                options
                    .updated_at
                    .unwrap_or_else(|| Utc::now().to_rfc3339()),
            ),
        },
    );
    save_metrics_snapshot(&paths.metrics_path, &snapshot)?;
    metrics_report_from_snapshot(&paths, &kernel.scope_id, snapshot, None)
}

pub fn metrics_get(options: StrategyMetricGetOptions) -> Result<StrategyMetricsReport> {
    let (paths, _, kernel) = load_scope_bundle(options.scope.as_str())?;
    let mut snapshot = load_metrics_snapshot(&paths.metrics_path)?;
    hydrate_metric_slots_from_kernel(&mut snapshot, &kernel);
    let mut warnings = Vec::new();
    if let Some(key) = options.metric_key.as_deref() {
        let Some((canonical_key, _)) = resolve_metric_key(&kernel, key) else {
            return Err(anyhow!(
                "unknown metric key `{}`; expected one of the strategy KPI metric_key or kpi_id values",
                key
            ));
        };
        snapshot
            .kpis
            .retain(|existing_key, _| existing_key == &canonical_key);
        if snapshot.kpis.is_empty() {
            warnings.push(format!(
                "No ingested KPI definition or current metric value was found for `{canonical_key}`."
            ));
        } else if snapshot
            .kpis
            .get(&canonical_key)
            .is_some_and(|record| record.current.is_none())
        {
            warnings.push(format!(
                "Ingested KPI `{canonical_key}` exists, but no current metric value is recorded."
            ));
        }
    } else if strategy_metrics_have_no_current_values(&snapshot) && !snapshot.kpis.is_empty() {
        warnings.push(
            "Loaded ingested strategy KPI definitions, but no current metric values are recorded."
                .to_string(),
        );
    }
    metrics_report_from_snapshot(&paths, &kernel.scope_id, snapshot, Some(warnings))
}

pub fn metrics_sync(options: StrategyMetricSyncOptions) -> Result<StrategyMetricsReport> {
    let (paths, _, kernel) = load_scope_bundle(options.scope.as_str())?;
    let content = fs::read_to_string(&options.from_path).with_context(|| {
        format!(
            "Failed to read metrics import {}",
            options.from_path.display()
        )
    })?;
    let snapshot: StrategyMetricsSnapshot = serde_json::from_str(&content).with_context(|| {
        format!(
            "Failed to parse metrics import {}",
            options.from_path.display()
        )
    })?;
    for key in snapshot.kpis.keys() {
        if resolve_metric_key(&kernel, key).is_none() {
            return Err(anyhow!(
                "unknown metric key `{}`; expected one of the strategy KPI metric_key or kpi_id values",
                key
            ));
        }
    }
    save_metrics_snapshot(&paths.metrics_path, &snapshot)?;
    metrics_report_from_snapshot(&paths, &kernel.scope_id, snapshot, None)
}

fn resolve_store_paths(
    config: &Config,
    scope_id: &str,
    scope_config: &StrategyScopeConfig,
) -> Result<StrategyStorePaths> {
    let storage_dir = if let Some(dir) = &scope_config.storage_dir {
        dir.clone()
    } else if let Some(dir) = &config.strategy.directory {
        dir.join(scope_id)
    } else {
        context_data_dir()?.join(STRATEGY_DIR).join(scope_id)
    };
    Ok(StrategyStorePaths {
        registry_path: storage_dir.join(STRATEGY_REGISTRY_FILE),
        kernel_path: storage_dir.join(STRATEGY_KERNEL_FILE),
        metrics_path: scope_config
            .metrics_path
            .clone()
            .unwrap_or_else(|| storage_dir.join(STRATEGY_DEFAULT_METRICS_FILE)),
        storage_dir,
    })
}

fn build_source_registry(
    scope_id: &str,
    artifact_path: &Path,
    scope_config: &StrategyScopeConfig,
    store_paths: &StrategyStorePaths,
    bootstrap_requested: bool,
    template_managed: bool,
) -> Result<StrategySourceRegistry> {
    Ok(StrategySourceRegistry {
        schema_version: "strategy-registry-v1".to_string(),
        scope_id: scope_id.to_string(),
        artifact_path: absolute_or_original(artifact_path)?,
        metrics_path: absolute_or_original(
            scope_config.metrics_path.as_ref().expect("metrics path"),
        )?,
        continuity_project_path: scope_config
            .continuity_project_path
            .as_ref()
            .map(|path| absolute_or_original(path))
            .transpose()?,
        signal_paths: scope_config
            .signal_paths
            .iter()
            .map(|path| absolute_or_original(path))
            .collect::<Result<Vec<_>>>()?,
        storage_dir: absolute_or_original(&store_paths.storage_dir)?,
        bootstrap_requested,
        template_managed,
        imported_at: Utc::now().to_rfc3339(),
    })
}

fn import_strategy_kernel(scope_id: &str, artifact_path: &Path) -> Result<StrategyKernel> {
    let content = fs::read_to_string(artifact_path).with_context(|| {
        format!(
            "Failed to read strategy artifact {}",
            artifact_path.display()
        )
    })?;
    let source_path = absolute_or_original(artifact_path)?;
    let source_id = format!(
        "source:{}",
        short_hash(&format!("{}:{}", source_path.display(), content))
    );
    let source_doc = StrategySourceDocument {
        source_id: source_id.clone(),
        source_type: infer_strategy_source_type(artifact_path, &content).to_string(),
        path: source_path.display().to_string(),
        content_hash: hash_text(&content),
        imported_at: Utc::now().to_rfc3339(),
    };
    if source_doc.source_type == "strategy-json" {
        parse_strategy_json(scope_id, &content, &source_doc)
    } else {
        parse_strategy_markdown(scope_id, &content, &source_doc)
    }
}

fn load_scope_bundle(
    requested_scope: &str,
) -> Result<(StrategyStorePaths, StrategySourceRegistry, StrategyKernel)> {
    let config = Config::load().context("Failed to load config.toml")?;
    let scope_name = config.strategy.resolve_scope_name(Some(requested_scope));
    let paths = if let Some((_, scope_config)) = config.strategy.scope(Some(&scope_name)) {
        resolve_store_paths(&config, &scope_name, scope_config)?
    } else {
        let strategy_root = config
            .strategy
            .directory
            .clone()
            .unwrap_or(context_data_dir()?.join(STRATEGY_DIR));
        let storage_dir = strategy_root.join(&scope_name);
        if !storage_dir.exists() {
            return Err(anyhow!(
                "No strategy scope is configured for `{scope_name}`"
            ));
        }
        let fallback_scope = StrategyScopeConfig {
            storage_dir: Some(storage_dir),
            ..StrategyScopeConfig::default()
        };
        resolve_store_paths(&config, &scope_name, &fallback_scope)?
    };
    let registry = load_registry(&paths.registry_path)?;
    let mut kernel = load_kernel(&paths.kernel_path)?;
    if strategy_kernel_is_empty(&kernel) && registry.artifact_path.exists() {
        kernel = import_strategy_kernel(&registry.scope_id, &registry.artifact_path)?;
        save_kernel(&paths.kernel_path, &kernel)?;
    }
    Ok((paths, registry, kernel))
}

fn load_registry(path: &Path) -> Result<StrategySourceRegistry> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("Failed to read strategy registry {}", path.display()))?;
    serde_json::from_str(&content)
        .with_context(|| format!("Failed to parse strategy registry {}", path.display()))
}

fn save_registry(path: &Path, registry: &StrategySourceRegistry) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, serde_json::to_string_pretty(registry)?)
        .with_context(|| format!("Failed to write strategy registry {}", path.display()))
}

fn load_kernel(path: &Path) -> Result<StrategyKernel> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("Failed to read strategy kernel {}", path.display()))?;
    serde_json::from_str(&content)
        .with_context(|| format!("Failed to parse strategy kernel {}", path.display()))
}

fn save_kernel(path: &Path, kernel: &StrategyKernel) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, serde_json::to_string_pretty(kernel)?)
        .with_context(|| format!("Failed to write strategy kernel {}", path.display()))
}

fn load_metrics_snapshot(path: &Path) -> Result<StrategyMetricsSnapshot> {
    if !path.exists() {
        return Ok(StrategyMetricsSnapshot::default());
    }
    let content = fs::read_to_string(path)
        .with_context(|| format!("Failed to read metrics snapshot {}", path.display()))?;
    serde_json::from_str(&content)
        .with_context(|| format!("Failed to parse metrics snapshot {}", path.display()))
}

fn hydrate_metric_slots_from_kernel(
    snapshot: &mut StrategyMetricsSnapshot,
    kernel: &StrategyKernel,
) {
    for kpi in &kernel.kpis {
        snapshot
            .kpis
            .entry(kpi.metric_key.clone())
            .or_insert_with(|| StrategyMetricRecord {
                current: None,
                unit: kpi.unit.clone(),
                updated_at: None,
            });
    }
}

fn save_metrics_snapshot(path: &Path, snapshot: &StrategyMetricsSnapshot) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, serde_json::to_string_pretty(snapshot)?)
        .with_context(|| format!("Failed to write metrics snapshot {}", path.display()))
}

fn resolve_metric_key(kernel: &StrategyKernel, key: &str) -> Option<(String, Option<String>)> {
    kernel
        .kpis
        .iter()
        .find(|kpi| kpi.metric_key == key || kpi.kpi_id == key)
        .map(|kpi| (kpi.metric_key.clone(), kpi.unit.clone()))
}

fn metrics_report_from_snapshot(
    paths: &StrategyStorePaths,
    scope_id: &str,
    snapshot: StrategyMetricsSnapshot,
    warnings: Option<Vec<String>>,
) -> Result<StrategyMetricsReport> {
    Ok(StrategyMetricsReport {
        generated_at: Utc::now().to_rfc3339(),
        scope_id: scope_id.to_string(),
        metrics_path: paths.metrics_path.to_string_lossy().to_string(),
        kpis: snapshot.kpis,
        warnings: warnings.unwrap_or_default(),
    })
}

fn ensure_metrics_file(path: &Path, scope_id: &str) -> Result<()> {
    if path.exists() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let template = StrategyMetricsSnapshot {
        generated_at: Some(Utc::now().to_rfc3339()),
        notes: vec![format!(
            "Populate KPI and instrumentation values for scope `{scope_id}`."
        )],
        ..Default::default()
    };
    fs::write(path, serde_json::to_string_pretty(&template)?)
        .with_context(|| format!("Failed to write metrics template {}", path.display()))
}

fn load_continuity_snapshot(project_path: Option<&Path>) -> Result<StrategyContinuitySnapshot> {
    let Some(project_path) = project_path else {
        return Ok(StrategyContinuitySnapshot {
            active: false,
            summary: None,
        });
    };
    let tracker = Tracker::new().context("Failed to initialize tracking database")?;
    let findings = tracker.get_memory_os_continuity_findings(
        MemoryOsInspectionScope::Project,
        Some(&project_path.display().to_string()),
    )?;
    Ok(StrategyContinuitySnapshot {
        active: !findings.is_empty(),
        summary: findings.first().map(|finding| finding.summary.clone()),
    })
}

fn continuity_nudge_tasks(project_path: Option<&Path>) -> Result<Vec<NudgeTask>> {
    let tracker = Tracker::new().context("Failed to initialize tracking database")?;
    let project_string = project_path.map(|path| path.display().to_string());
    let mut tasks = collect_memory_tasks(
        &tracker,
        MemoryOsInspectionScope::Project,
        project_string.as_deref(),
    )?;
    if tasks.is_empty() {
        tasks = collect_memory_tasks(&tracker, MemoryOsInspectionScope::User, None)?;
    }
    Ok(dedupe_nudge_tasks(tasks, 5))
}

fn collect_memory_tasks(
    tracker: &Tracker,
    scope: MemoryOsInspectionScope,
    project_path: Option<&str>,
) -> Result<Vec<NudgeTask>> {
    let mut tasks = Vec::new();
    for finding in tracker
        .get_memory_os_continuity_findings(scope, project_path)?
        .into_iter()
        .take(3)
    {
        if !usable_memory_task_summary(&finding.summary) {
            continue;
        }
        tasks.push(NudgeTask {
            task: format!("Resume incomplete work: {}", compact_task_text(&finding.summary)),
            source: "verified-incomplete-task".to_string(),
            why_now:
                "Memory OS has an explicit continuity commitment or open obligation from earlier work."
                    .to_string(),
            evidence: finding.evidence.into_iter().take(2).collect(),
        });
    }

    let overview = tracker.get_memory_os_overview_report(scope, project_path)?;
    for finding in overview.active_work.into_iter().take(3) {
        if !usable_memory_task_summary(&finding.summary) {
            continue;
        }
        tasks.push(NudgeTask {
            task: format!(
                "Continue {}: {}",
                finding.title,
                compact_task_text(&finding.summary)
            ),
            source: "active-project-memory".to_string(),
            why_now:
                "Recent completed sessions still point to this as the next active project area."
                    .to_string(),
            evidence: finding.evidence.into_iter().take(2).collect(),
        });
    }
    for project in overview.top_projects.into_iter().take(2) {
        tasks.push(NudgeTask {
            task: format!("Review next step for {}", project.repo_label),
            source: "recent-project-history".to_string(),
            why_now:
                "This project has a high concentration of recent agent sessions and completed work."
                    .to_string(),
            evidence: vec![format!(
                "{} sessions, {} shell executions",
                project.sessions, project.shell_executions
            )],
        });
    }
    Ok(tasks)
}

fn usable_memory_task_summary(summary: &str) -> bool {
    let lowered = summary.to_ascii_lowercase();
    let trimmed = summary.trim();
    if trimmed.len() < 24 {
        return false;
    }
    if lowered.starts_with("no this")
        || lowered.starts_with("no, this")
        || lowered.starts_with("nah")
        || lowered.starts_with("wait")
    {
        return false;
    }
    true
}

fn dedupe_nudge_tasks(tasks: Vec<NudgeTask>, limit: usize) -> Vec<NudgeTask> {
    let mut seen = std::collections::BTreeSet::new();
    tasks
        .into_iter()
        .filter(|task| seen.insert(task.task.to_ascii_lowercase()))
        .take(limit)
        .collect()
}

fn compact_task_text(text: &str) -> String {
    let compact = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.len() <= 160 {
        compact
    } else {
        format!("{}...", compact.chars().take(157).collect::<String>())
    }
}

fn parse_strategy_markdown(
    scope_id: &str,
    content: &str,
    source_doc: &StrategySourceDocument,
) -> Result<StrategyKernel> {
    let mut goals = Vec::new();
    let mut kpis = Vec::new();
    let mut initiatives = Vec::new();
    let mut constraints = Vec::new();
    let mut assumptions = Vec::new();
    let mut headings: Vec<String> = Vec::new();

    for (index, raw_line) in content.lines().enumerate() {
        let line_number = index + 1;
        let trimmed = raw_line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some((level, title)) = parse_heading(trimmed) {
            resize_heading_stack(&mut headings, level);
            headings.push(title.to_string());
            continue;
        }
        let Some(item_text) = parse_list_item(trimmed) else {
            continue;
        };
        let section_path = if headings.is_empty() {
            "root".to_string()
        } else {
            headings.join(" > ")
        };
        let kind = classify_section_kind(&section_path);
        if matches!(kind, StrategySectionKind::Ignore) {
            continue;
        }
        let (body, metadata) = parse_metadata(item_text.as_str());
        if body.is_empty() {
            continue;
        }
        let source_ref = StrategySourceRef {
            source_id: source_doc.source_id.clone(),
            source_path: source_doc.path.clone(),
            section_path: section_path.clone(),
            line_start: line_number,
            line_end: line_number,
            excerpt: body.clone(),
        };

        match kind {
            StrategySectionKind::Goal => {
                let title = strip_known_prefixes(&body, &["goal", "target"]);
                let goal_id = metadata
                    .get("id")
                    .cloned()
                    .unwrap_or_else(|| slugify(&title));
                goals.push(StrategyGoal {
                    goal_id,
                    horizon: infer_goal_horizon(&section_path),
                    title,
                    summary: body.clone(),
                    due_date: metadata.get("due").cloned(),
                    source_refs: vec![source_ref],
                });
            }
            StrategySectionKind::Kpi => {
                let title = strip_known_prefixes(&body, &["kpi", "critical number", "metric"]);
                let kpi_id = metadata
                    .get("id")
                    .cloned()
                    .unwrap_or_else(|| slugify(&title));
                let metric_key = metadata
                    .get("metric")
                    .cloned()
                    .unwrap_or_else(|| kpi_id.clone());
                kpis.push(StrategyKpi {
                    kpi_id,
                    title,
                    metric_key,
                    unit: metadata.get("unit").cloned(),
                    target: metadata.get("target").and_then(|value| parse_number(value)),
                    green_threshold: metadata.get("green").and_then(|value| parse_number(value)),
                    yellow_threshold: metadata.get("yellow").and_then(|value| parse_number(value)),
                    cadence: metadata.get("cadence").cloned(),
                    due_date: metadata.get("due").cloned(),
                    goal_ids: Vec::new(),
                    initiative_ids: Vec::new(),
                    source_refs: vec![source_ref],
                });
            }
            StrategySectionKind::Initiative => {
                let title = strip_known_prefixes(&body, &["initiative", "rock", "priority"]);
                let initiative_id = metadata
                    .get("id")
                    .cloned()
                    .unwrap_or_else(|| slugify(&title));
                let depends_on = metadata
                    .get("depends_on")
                    .map(|value| parse_csv(value))
                    .unwrap_or_default();
                initiatives.push(StrategyInitiative {
                    initiative_id,
                    kind: infer_initiative_kind(&section_path),
                    title,
                    owner: metadata.get("owner").cloned(),
                    due_date: metadata.get("due").cloned(),
                    depends_on,
                    supports_goal_ids: Vec::new(),
                    deferred: metadata
                        .get("deferred")
                        .map(|value| value.eq_ignore_ascii_case("true"))
                        .unwrap_or(false)
                        || section_path.to_ascii_lowercase().contains("deferred"),
                    source_refs: vec![source_ref],
                });
            }
            StrategySectionKind::Constraint => {
                let title = strip_known_prefixes(&body, &["constraint", "deferred", "not now"]);
                let constraint_id = metadata
                    .get("id")
                    .cloned()
                    .unwrap_or_else(|| slugify(&title));
                constraints.push(StrategyConstraint {
                    constraint_id,
                    title,
                    suppression_kind: if section_path.to_ascii_lowercase().contains("deferred")
                        || section_path.to_ascii_lowercase().contains("not now")
                    {
                        "deferred_not_now".to_string()
                    } else {
                        "constraint".to_string()
                    },
                    summary: Some(body.clone()),
                    source_refs: vec![source_ref],
                });
            }
            StrategySectionKind::Assumption => {
                let assumption_id = metadata
                    .get("id")
                    .cloned()
                    .unwrap_or_else(|| slugify(&body));
                assumptions.push(StrategyAssumption {
                    assumption_id,
                    statement: body,
                    source_refs: vec![source_ref],
                });
            }
            StrategySectionKind::Ignore => {}
        }
    }

    Ok(StrategyKernel {
        schema_version: "strategy-kernel-v1".to_string(),
        scope_id: scope_id.to_string(),
        imported_at: Utc::now().to_rfc3339(),
        sources: vec![source_doc.clone()],
        goals,
        kpis,
        initiatives,
        constraints,
        assumptions,
    })
}

fn parse_strategy_json(
    scope_id: &str,
    content: &str,
    source_doc: &StrategySourceDocument,
) -> Result<StrategyKernel> {
    let parsed: StrategyImportJsonDocument =
        serde_json::from_str(content).context("Failed to parse strategy JSON sidecar")?;
    let declared_scope = parsed
        .organization
        .scope_id
        .as_deref()
        .filter(|value| !value.trim().is_empty());
    if let Some(declared_scope) = declared_scope {
        if declared_scope != scope_id {
            return Err(anyhow!(
                "Strategy JSON sidecar scope_id '{}' does not match requested scope '{}'",
                declared_scope,
                scope_id
            ));
        }
    }

    let goals = parsed
        .goals
        .into_iter()
        .map(|goal| StrategyGoal {
            goal_id: goal.id.clone(),
            horizon: goal.horizon,
            title: goal.title.clone(),
            summary: goal.summary.unwrap_or_else(|| goal.title.clone()),
            due_date: goal.due_date,
            source_refs: vec![json_source_ref(
                source_doc,
                goal.source_section.as_deref().unwrap_or("Goals"),
                &goal.title,
            )],
        })
        .collect::<Vec<_>>();
    let kpis = parsed
        .kpis
        .into_iter()
        .map(|kpi| StrategyKpi {
            kpi_id: kpi.id,
            title: kpi.title.clone(),
            metric_key: kpi.metric_key,
            unit: kpi.unit,
            target: kpi.target,
            green_threshold: kpi.green_threshold,
            yellow_threshold: kpi.yellow_threshold,
            cadence: kpi.cadence,
            due_date: kpi.due_date,
            goal_ids: kpi
                .lineage
                .as_ref()
                .map(|lineage| lineage.goal_ids.clone())
                .unwrap_or_default(),
            initiative_ids: kpi
                .lineage
                .as_ref()
                .map(|lineage| lineage.initiative_ids.clone())
                .unwrap_or_default(),
            source_refs: vec![json_source_ref(
                source_doc,
                kpi.source_section.as_deref().unwrap_or("KPIs"),
                &kpi.title,
            )],
        })
        .collect::<Vec<_>>();
    let initiatives = parsed
        .initiatives
        .into_iter()
        .map(|initiative| StrategyInitiative {
            initiative_id: initiative.id,
            kind: initiative.kind,
            title: initiative.title.clone(),
            owner: initiative.owner,
            due_date: initiative.due_date,
            depends_on: initiative.depends_on,
            supports_goal_ids: initiative.supports_goal_ids,
            deferred: initiative.deferred,
            source_refs: vec![json_source_ref(
                source_doc,
                initiative
                    .source_section
                    .as_deref()
                    .unwrap_or("Initiatives"),
                &initiative.title,
            )],
        })
        .collect::<Vec<_>>();
    let constraints = parsed
        .constraints
        .into_iter()
        .map(|constraint| StrategyConstraint {
            constraint_id: constraint.id,
            title: constraint.title.clone(),
            suppression_kind: constraint.suppression_kind,
            summary: constraint.summary,
            source_refs: vec![json_source_ref(
                source_doc,
                constraint
                    .source_section
                    .as_deref()
                    .unwrap_or("Constraints"),
                &constraint.title,
            )],
        })
        .collect::<Vec<_>>();
    let assumptions = parsed
        .assumptions
        .into_iter()
        .map(|assumption| StrategyAssumption {
            assumption_id: assumption.id,
            statement: assumption.statement.clone(),
            source_refs: vec![json_source_ref(
                source_doc,
                assumption
                    .source_section
                    .as_deref()
                    .unwrap_or("Assumptions"),
                &assumption.statement,
            )],
        })
        .collect::<Vec<_>>();

    if goals.is_empty() && kpis.is_empty() && initiatives.is_empty() && constraints.is_empty() {
        return Err(anyhow!(
            "Strategy JSON sidecar did not contain any ingestible goals, KPIs, initiatives, or constraints"
        ));
    }

    Ok(StrategyKernel {
        schema_version: "strategy-kernel-v1".to_string(),
        scope_id: scope_id.to_string(),
        imported_at: Utc::now().to_rfc3339(),
        sources: vec![source_doc.clone()],
        goals,
        kpis,
        initiatives,
        constraints,
        assumptions,
    })
}

fn status_for_kpi(kpi: &StrategyKpi, metrics: &StrategyMetricsSnapshot) -> StrategyStatusItem {
    let metric_record = metrics
        .kpis
        .get(&kpi.metric_key)
        .or_else(|| metrics.kpis.get(&kpi.kpi_id));
    let instrumentation_state = metrics
        .instrumentation
        .get(&kpi.metric_key)
        .copied()
        .or_else(|| metrics.instrumentation.get(&kpi.kpi_id).copied());
    let mut evidence = Vec::new();
    let freshness = metric_record
        .and_then(|record| record.updated_at.as_deref())
        .map(freshness_label)
        .unwrap_or_else(|| {
            metrics
                .generated_at
                .as_deref()
                .map(freshness_label)
                .unwrap_or_else(|| "unknown".to_string())
        });
    let missing_instrumentation = instrumentation_state == Some(false) || metric_record.is_none();
    let (status, confidence) = if missing_instrumentation {
        evidence.push("No reliable metric value is available yet.".to_string());
        ("unknown".to_string(), "low".to_string())
    } else if let Some(record) = metric_record {
        let Some(current) = record.current else {
            evidence.push("Metric record exists but `current` is null.".to_string());
            return StrategyStatusItem {
                item_id: kpi.kpi_id.clone(),
                item_kind: "kpi".to_string(),
                title: kpi.title.clone(),
                supports: build_supports(&kpi.goal_ids, &kpi.initiative_ids),
                status: "unknown".to_string(),
                evidence,
                evidence_freshness: freshness,
                confidence: "low".to_string(),
                missing_instrumentation: true,
                due_date: kpi.due_date.clone(),
                depends_on: Vec::new(),
            };
        };
        evidence.push(format!("Current value: {}", format_float(current)));
        if let Some(target) = kpi.target {
            evidence.push(format!("Target: {}", format_float(target)));
        }
        if let Some(green) = kpi.green_threshold {
            evidence.push(format!("Green threshold: {}", format_float(green)));
        }
        if let Some(yellow) = kpi.yellow_threshold {
            evidence.push(format!("Yellow threshold: {}", format_float(yellow)));
        }
        let status = score_numeric_status(
            current,
            kpi.green_threshold.or(kpi.target),
            kpi.yellow_threshold,
        );
        (status, "medium".to_string())
    } else {
        ("unknown".to_string(), "low".to_string())
    };

    StrategyStatusItem {
        item_id: kpi.kpi_id.clone(),
        item_kind: "kpi".to_string(),
        title: kpi.title.clone(),
        supports: build_supports(&kpi.goal_ids, &kpi.initiative_ids),
        status,
        evidence,
        evidence_freshness: freshness,
        confidence,
        missing_instrumentation,
        due_date: kpi.due_date.clone(),
        depends_on: Vec::new(),
    }
}

fn status_for_initiative(
    initiative: &StrategyInitiative,
    metrics: &StrategyMetricsSnapshot,
) -> StrategyStatusItem {
    let mut evidence = Vec::new();
    if initiative.deferred {
        evidence.push("Marked deferred / not now in the imported strategy kernel.".to_string());
        return StrategyStatusItem {
            item_id: initiative.initiative_id.clone(),
            item_kind: "initiative".to_string(),
            title: initiative.title.clone(),
            supports: build_supports(&initiative.supports_goal_ids, &[]),
            status: "deferred".to_string(),
            evidence,
            evidence_freshness: "n/a".to_string(),
            confidence: "high".to_string(),
            missing_instrumentation: false,
            due_date: initiative.due_date.clone(),
            depends_on: initiative.depends_on.clone(),
        };
    }

    let mut blocked_by = Vec::new();
    for dep in &initiative.depends_on {
        if metrics.dependency_states.get(dep).copied() == Some(false) {
            blocked_by.push(dep.clone());
        }
    }
    let initiative_signal = metrics.initiatives.get(&initiative.initiative_id);
    let freshness = initiative_signal
        .and_then(|signal| signal.updated_at.as_deref())
        .map(freshness_label)
        .unwrap_or_else(|| {
            metrics
                .generated_at
                .as_deref()
                .map(freshness_label)
                .unwrap_or_else(|| "unknown".to_string())
        });
    let status =
        if !blocked_by.is_empty() || initiative_signal.and_then(|s| s.blocked).unwrap_or(false) {
            evidence.push(format!("Blocked by: {}", blocked_by.join(", ")));
            "blocked".to_string()
        } else if let Some(signal) = initiative_signal.and_then(|signal| signal.status.clone()) {
            evidence.push(format!("Initiative status: {signal}"));
            signal
        } else {
            evidence.push("No initiative status signal is available yet.".to_string());
            "unknown".to_string()
        };

    StrategyStatusItem {
        item_id: initiative.initiative_id.clone(),
        item_kind: "initiative".to_string(),
        title: initiative.title.clone(),
        supports: build_supports(&initiative.supports_goal_ids, &[]),
        status,
        evidence,
        evidence_freshness: freshness,
        confidence: if initiative_signal.is_some() {
            "medium".to_string()
        } else {
            "low".to_string()
        },
        missing_instrumentation: initiative_signal.is_none(),
        due_date: initiative.due_date.clone(),
        depends_on: initiative.depends_on.clone(),
    }
}

fn deferred_suppression_nudges(kernel: &StrategyKernel) -> Vec<StrategicNudge> {
    let mut nudges = Vec::new();
    for initiative in kernel
        .initiatives
        .iter()
        .filter(|initiative| initiative.deferred)
    {
        nudges.push(StrategicNudge {
            task: format!("Do not start `{}` yet", initiative.title),
            item_id: Some(initiative.initiative_id.clone()),
            item_kind: "initiative".to_string(),
            supports: vec![format!("Deferred initiative: {}", initiative.title)],
            why_now: "The active strategy explicitly marks this work as deferred / not now."
                .to_string(),
            evidence: vec!["Deferred item in strategy kernel".to_string()],
            evidence_freshness: "n/a".to_string(),
            confidence: "high".to_string(),
            interrupt_level: "none".to_string(),
            suppression_reason: Some("deferred_not_now".to_string()),
            expected_effect:
                "Preserve focus on the current strategy instead of re-opening deferred work."
                    .to_string(),
        });
    }
    for constraint in &kernel.constraints {
        nudges.push(StrategicNudge {
            task: format!("Respect strategy constraint: {}", constraint.title),
            item_id: Some(constraint.constraint_id.clone()),
            item_kind: "constraint".to_string(),
            supports: vec![format!("Constraint: {}", constraint.title)],
            why_now: "The imported strategy includes an explicit constraint that suppresses this line of work."
                .to_string(),
            evidence: vec!["Constraint from strategy kernel".to_string()],
            evidence_freshness: "n/a".to_string(),
            confidence: "high".to_string(),
            interrupt_level: "none".to_string(),
            suppression_reason: Some(constraint.suppression_kind.clone()),
            expected_effect: "Prevent speculative work that conflicts with the active strategy."
                .to_string(),
        });
    }
    nudges
}

fn nudge_for_status_item(item: &StrategyStatusItem) -> Option<StrategicNudge> {
    if item.status == "deferred" {
        return None;
    }
    let supports = if item.supports.is_empty() {
        vec![format!("{}: {}", item.item_kind, item.title)]
    } else {
        item.supports.clone()
    };
    let mut nudge = if item.missing_instrumentation || item.status == "unknown" {
        StrategicNudge {
            task: format!("Instrument or measure `{}`", item.title),
            item_id: Some(item.item_id.clone()),
            item_kind: item.item_kind.clone(),
            supports,
            why_now: "This strategy item lacks reliable evidence, so the first move should be to create a trustworthy signal."
                .to_string(),
            evidence: item.evidence.clone(),
            evidence_freshness: item.evidence_freshness.clone(),
            confidence: "low".to_string(),
            interrupt_level: "defer".to_string(),
            suppression_reason: None,
            expected_effect: "Turn an unknown strategic risk into a measurable one before choosing execution work."
                .to_string(),
        }
    } else if item.status == "blocked" {
        StrategicNudge {
            task: format!("Unblock `{}`", item.title),
            item_id: Some(item.item_id.clone()),
            item_kind: item.item_kind.clone(),
            supports,
            why_now: "This initiative is blocked on a known dependency, so the dependency needs attention before downstream work."
                .to_string(),
            evidence: item.evidence.clone(),
            evidence_freshness: item.evidence_freshness.clone(),
            confidence: if item.evidence_freshness == "fresh" {
                "high".to_string()
            } else {
                "medium".to_string()
            },
            interrupt_level: if item.evidence_freshness == "fresh" {
                "interrupt".to_string()
            } else {
                "suggest".to_string()
            },
            suppression_reason: None,
            expected_effect: "Restore momentum on a blocked strategic item by clearing the dependency."
                .to_string(),
        }
    } else if item.status == "red" {
        StrategicNudge {
            task: format!("Address red-state `{}`", item.title),
            item_id: Some(item.item_id.clone()),
            item_kind: item.item_kind.clone(),
            supports,
            why_now:
                "This strategic item is below its expected threshold and needs corrective work."
                    .to_string(),
            evidence: item.evidence.clone(),
            evidence_freshness: item.evidence_freshness.clone(),
            confidence: if item.evidence_freshness == "fresh" {
                "high".to_string()
            } else {
                "medium".to_string()
            },
            interrupt_level: if item.evidence_freshness == "fresh" {
                "interrupt".to_string()
            } else {
                "suggest".to_string()
            },
            suppression_reason: None,
            expected_effect: "Move the item back toward its target or risk band.".to_string(),
        }
    } else if item.status == "yellow" {
        StrategicNudge {
            task: format!("Improve yellow-state `{}`", item.title),
            item_id: Some(item.item_id.clone()),
            item_kind: item.item_kind.clone(),
            supports,
            why_now:
                "This item is drifting toward risk and is worth attention before it turns red."
                    .to_string(),
            evidence: item.evidence.clone(),
            evidence_freshness: item.evidence_freshness.clone(),
            confidence: "medium".to_string(),
            interrupt_level: "defer".to_string(),
            suppression_reason: None,
            expected_effect: "Reduce the chance that a strategic item falls into a red state."
                .to_string(),
        }
    } else {
        return None;
    };
    if item.item_kind == "kpi" && (item.status == "unknown" || item.missing_instrumentation) {
        nudge.suppression_reason = Some("instrumentation_first".to_string());
    }
    Some(nudge)
}

fn nudge_rank(nudge: &StrategicNudge) -> i32 {
    match nudge.interrupt_level.as_str() {
        "interrupt" => 4,
        "suggest" => 3,
        "defer" => 2,
        _ => 1,
    }
}

fn nudge_can_preempt_continuity(nudge: &StrategicNudge) -> bool {
    nudge.interrupt_level == "interrupt"
        && nudge.confidence == "high"
        && nudge.evidence_freshness == "fresh"
}

fn build_supports(goal_ids: &[String], initiative_ids: &[String]) -> Vec<String> {
    let mut supports = Vec::new();
    supports.extend(goal_ids.iter().map(|goal_id| format!("goal:{goal_id}")));
    supports.extend(
        initiative_ids
            .iter()
            .map(|initiative_id| format!("initiative:{initiative_id}")),
    );
    supports
}

fn parse_heading(value: &str) -> Option<(usize, &str)> {
    let hashes = value.chars().take_while(|ch| *ch == '#').count();
    if hashes == 0 {
        return None;
    }
    Some((hashes, value[hashes..].trim()))
}

fn infer_strategy_source_type(artifact_path: &Path, content: &str) -> &'static str {
    if artifact_path
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| value.eq_ignore_ascii_case("json"))
        .unwrap_or(false)
        || content.trim_start().starts_with('{')
    {
        "strategy-json"
    } else {
        "strategy-markdown"
    }
}

fn json_source_ref(
    source_doc: &StrategySourceDocument,
    source_section: &str,
    excerpt: &str,
) -> StrategySourceRef {
    StrategySourceRef {
        source_id: source_doc.source_id.clone(),
        source_path: source_doc.path.clone(),
        section_path: source_section.to_string(),
        line_start: 0,
        line_end: 0,
        excerpt: excerpt.to_string(),
    }
}

fn resize_heading_stack(headings: &mut Vec<String>, level: usize) {
    let keep = level.saturating_sub(1);
    while headings.len() > keep {
        headings.pop();
    }
}

fn parse_list_item(value: &str) -> Option<String> {
    if let Some(rest) = value
        .strip_prefix("- ")
        .or_else(|| value.strip_prefix("* "))
    {
        return Some(rest.trim().to_string());
    }
    let bytes = value.as_bytes();
    let mut idx = 0;
    while idx < bytes.len() && bytes[idx].is_ascii_digit() {
        idx += 1;
    }
    if idx > 0
        && idx + 1 < bytes.len()
        && (bytes[idx] == b'.' || bytes[idx] == b')')
        && bytes[idx + 1] == b' '
    {
        return Some(value[idx + 2..].trim().to_string());
    }
    None
}

fn parse_metadata(value: &str) -> (String, BTreeMap<String, String>) {
    let mut parts = value.split('|').map(str::trim);
    let body = parts.next().unwrap_or_default().to_string();
    let mut metadata = BTreeMap::new();
    for part in parts {
        if let Some((key, value)) = part.split_once('=') {
            metadata.insert(normalize_key(key), value.trim().to_string());
        }
    }
    (body, metadata)
}

fn normalize_key(value: &str) -> String {
    value.trim().to_ascii_lowercase().replace(' ', "_")
}

fn classify_section_kind(section_path: &str) -> StrategySectionKind {
    let lowered = section_path.to_ascii_lowercase();
    if lowered.contains("non-goal") || lowered.contains("non goal") {
        return StrategySectionKind::Constraint;
    }
    if lowered.contains("deferred") || lowered.contains("not now") || lowered.contains("constraint")
    {
        return StrategySectionKind::Constraint;
    }
    if lowered.contains("assumption") {
        return StrategySectionKind::Assumption;
    }
    if lowered.contains("kpi") || lowered.contains("critical number") || lowered.contains("metric")
    {
        return StrategySectionKind::Kpi;
    }
    if lowered.contains("initiative") || lowered.contains("rock") || lowered.contains("priority") {
        return StrategySectionKind::Initiative;
    }
    if lowered.contains("goal")
        || lowered.contains("target")
        || lowered.contains("bhag")
        || lowered.contains("quarter")
        || lowered.contains("annual")
    {
        return StrategySectionKind::Goal;
    }
    StrategySectionKind::Ignore
}

fn infer_goal_horizon(section_path: &str) -> String {
    let lowered = section_path.to_ascii_lowercase();
    if lowered.contains("bhag") {
        "bhag".to_string()
    } else if lowered.contains("3-5") || lowered.contains("three") || lowered.contains("3 year") {
        "3y".to_string()
    } else if lowered.contains("annual") || lowered.contains("year") {
        "annual".to_string()
    } else if lowered.contains("quarter") {
        "quarterly".to_string()
    } else {
        "goal".to_string()
    }
}

fn infer_initiative_kind(section_path: &str) -> String {
    let lowered = section_path.to_ascii_lowercase();
    if lowered.contains("rock") {
        "rock".to_string()
    } else {
        "initiative".to_string()
    }
}

fn strip_known_prefixes(value: &str, prefixes: &[&str]) -> String {
    for prefix in prefixes {
        let needle = format!("{}:", prefix);
        if value.to_ascii_lowercase().starts_with(&needle) {
            return value[needle.len()..].trim().to_string();
        }
    }
    value.trim().to_string()
}

fn parse_csv(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn parse_number(value: &str) -> Option<f64> {
    let cleaned = value.replace(',', "");
    cleaned.parse::<f64>().ok()
}

fn score_numeric_status(current: f64, green: Option<f64>, yellow: Option<f64>) -> String {
    if let Some(green_value) = green {
        if current >= green_value {
            return "green".to_string();
        }
    }
    if let Some(yellow_value) = yellow {
        if current >= yellow_value {
            return "yellow".to_string();
        }
    }
    "red".to_string()
}

fn freshness_label(value: &str) -> String {
    let Ok(parsed) = DateTime::parse_from_rfc3339(value) else {
        return "unknown".to_string();
    };
    let age = Utc::now().signed_duration_since(parsed.with_timezone(&Utc));
    if age <= Duration::days(7) {
        "fresh".to_string()
    } else if age <= Duration::days(30) {
        "stale".to_string()
    } else {
        "very_stale".to_string()
    }
}

fn absolute_or_original(path: &Path) -> Result<PathBuf> {
    match path.canonicalize() {
        Ok(path) => Ok(path),
        Err(_) => Ok(path.to_path_buf()),
    }
}

fn hash_text(value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn short_hash(value: &str) -> String {
    hash_text(value)[..12].to_string()
}

fn slugify(value: &str) -> String {
    let lowered = value.to_ascii_lowercase();
    let mut slug = String::with_capacity(lowered.len());
    let mut last_was_sep = false;
    for ch in lowered.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch);
            last_was_sep = false;
        } else if !last_was_sep {
            slug.push('-');
            last_was_sep = true;
        }
    }
    slug.trim_matches('-').to_string()
}

fn format_float(value: f64) -> String {
    if value.fract() == 0.0 {
        format!("{value:.0}")
    } else {
        format!("{value:.2}")
    }
}

fn strategy_template(scope_id: &str) -> String {
    format!(
        "# Strategy Plan\n\n## Goals\n- Goal: Reach 10 paying customers by June 30 | id=reach-10-customers | due=2026-06-30\n\n## KPIs\n- KPI: Outreach volume | id=outreach-volume | metric=outreach_volume | target=1500 | green=1500 | yellow=1000 | unit=weekly\n- KPI: Paying customers | id=paying-customers | metric=paying_customers | target=10 | green=10 | yellow=5\n\n## Initiatives\n- Initiative: Fix checkout links | id=fix-checkout-links\n- Rock: Register Google Search Console | id=register-gsc | depends_on=fix-checkout-links\n\n## Deferred / Not Now\n- Deferred: Self-serve CMS | id=self-serve-cms\n\n## Assumptions\n- Assumption: Cold outreach remains the primary acquisition channel for {scope_id}\n"
    )
}

fn strategy_json_template(scope_id: &str) -> String {
    let now = Utc::now();
    let date = now.format("%Y-%m-%d");
    let year = now.format("%Y");
    let quarter = ((now.format("%-m").to_string().parse::<u32>().unwrap_or(1) - 1) / 3) + 1;
    let org_name = derive_org_name_from_scope(scope_id);
    format!(
        "{{\n  \"schema_version\": \"strategic-plan-context-v1\",\n  \"organization\": {{\n    \"name\": \"{org_name}\",\n    \"scope_id\": \"{scope_id}\",\n    \"date\": \"{date}\",\n    \"plan_period\": \"FY{year}\",\n    \"active_quarter\": \"Q{quarter} {year}\"\n  }},\n  \"goals\": [\n    {{\n      \"id\": \"goal-annual-first-ten\",\n      \"title\": \"Reach 10 paying customers\",\n      \"horizon\": \"annual\",\n      \"summary\": \"First-cut starter goal — refine with /munin-strategy.\",\n      \"due_date\": \"{year}-12-31\",\n      \"source_section\": \"Annual Goals\"\n    }}\n  ],\n  \"kpis\": [\n    {{\n      \"id\": \"kpi-outreach-reply-rate\",\n      \"title\": \"Outreach reply rate\",\n      \"metric_key\": \"outreach_reply_rate\",\n      \"cadence\": \"weekly\",\n      \"target\": 3,\n      \"green_threshold\": 3,\n      \"yellow_threshold\": 1,\n      \"lineage\": {{\n        \"goal_ids\": [\"goal-annual-first-ten\"],\n        \"initiative_ids\": [\"rock-close-first-customer\"]\n      }},\n      \"source_section\": \"KPIs\"\n    }},\n    {{\n      \"id\": \"kpi-paying-customers\",\n      \"title\": \"Paying customers\",\n      \"metric_key\": \"paying_customers\",\n      \"cadence\": \"monthly\",\n      \"target\": 10,\n      \"green_threshold\": 10,\n      \"yellow_threshold\": 5,\n      \"lineage\": {{\n        \"goal_ids\": [\"goal-annual-first-ten\"],\n        \"initiative_ids\": [\"rock-close-first-customer\"]\n      }},\n      \"source_section\": \"KPIs\"\n    }},\n    {{\n      \"id\": \"kpi-revenue\",\n      \"title\": \"Revenue (NZD)\",\n      \"metric_key\": \"revenue_nzd\",\n      \"cadence\": \"monthly\",\n      \"target\": 1500,\n      \"green_threshold\": 1500,\n      \"yellow_threshold\": 500,\n      \"lineage\": {{\n        \"goal_ids\": [\"goal-annual-first-ten\"],\n        \"initiative_ids\": [\"rock-close-first-customer\"]\n      }},\n      \"source_section\": \"KPIs\"\n    }}\n  ],\n  \"initiatives\": [\n    {{\n      \"id\": \"rock-close-first-customer\",\n      \"title\": \"Close first paying customer\",\n      \"kind\": \"rock\",\n      \"depends_on\": [],\n      \"supports_goal_ids\": [\"goal-annual-first-ten\"],\n      \"deferred\": false,\n      \"source_section\": \"Quarterly Rocks\"\n    }}\n  ],\n  \"constraints\": [],\n  \"assumptions\": [\n    {{\n      \"id\": \"assumption-cold-outreach\",\n      \"title\": \"Cold outreach is the primary acquisition channel for {scope_id}\",\n      \"summary\": \"First-cut starter assumption — refine with /munin-strategy.\",\n      \"source_section\": \"Assumptions\"\n    }}\n  ],\n  \"risks\": []\n}}\n"
    )
}

fn derive_org_name_from_scope(scope_id: &str) -> String {
    let primary = scope_id.split('-').next().unwrap_or(scope_id);
    let mut chars = primary.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => "Organization".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::config::DEFAULT_STRATEGY_SCOPE;
    use std::sync::Mutex;
    use tempfile::tempdir;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn sample_strategy_markdown() -> String {
        r#"# One-Page Strategic Plan

## Annual Goals
- Goal: Reach 10 paying customers by June 30 | id=reach-10-customers | due=2026-06-30

## KPIs
- KPI: Outreach volume | id=outreach-volume | metric=outreach_volume | target=1500 | green=1500 | yellow=1000 | unit=weekly
- KPI: Paying customers | id=paying-customers | metric=paying_customers | target=10 | green=10 | yellow=5

## Quarterly Rocks
- Rock: Fix checkout links | id=fix-checkout-links
- Initiative: Register Google Search Console | id=register-gsc | depends_on=fix-checkout-links

## Deferred / Not Now
- Deferred: Self-serve CMS | id=self-serve-cms

## Assumptions
- Assumption: Cold outreach remains the primary acquisition channel
"#
        .to_string()
    }

    fn sample_strategy_json() -> String {
        r#"{
  "schema_version": "strategic-plan-context-v1",
  "organization": {
    "name": "SiteSorted",
    "scope_id": "sitesorted-business",
    "date": "2026-04-14",
    "plan_period": "FY2026",
    "active_quarter": "Q2 2026"
  },
  "goals": [
    {
      "id": "goal-annual-first-ten",
      "title": "Reach 10 paying customers",
      "horizon": "annual",
      "summary": "Get to 10 paying customers by June 30.",
      "due_date": "2026-06-30",
      "source_section": "Annual Goals"
    }
  ],
  "kpis": [
    {
      "id": "kpi-outreach-volume",
      "title": "Outreach volume",
      "metric_key": "outreach_volume",
      "cadence": "weekly",
      "target": 1500,
      "green_threshold": 1500,
      "yellow_threshold": 1000,
      "lineage": {
        "goal_ids": [
          "goal-annual-first-ten"
        ],
        "initiative_ids": [
          "rock-close-first-customer"
        ]
      },
      "source_section": "KPIs"
    }
  ],
  "initiatives": [
    {
      "id": "rock-close-first-customer",
      "title": "Close first customer",
      "kind": "rock",
      "depends_on": [],
      "supports_goal_ids": [
        "goal-annual-first-ten"
      ],
      "deferred": false,
      "source_section": "Rocks"
    }
  ],
  "constraints": [
    {
      "id": "constraint-no-self-serve-cms",
      "title": "Do not build self-serve CMS yet",
      "suppression_kind": "deferred_not_now",
      "summary": "Keep CMS deferred until demand is proven.",
      "source_section": "Deferred"
    }
  ],
  "assumptions": [
    {
      "id": "assumption-primary-channel",
      "statement": "Cold outreach remains the primary channel.",
      "source_section": "Assumptions"
    }
  ]
}"#
        .to_string()
    }

    #[test]
    fn test_parse_strategy_markdown_imports_kernel_with_provenance() {
        let source_doc = StrategySourceDocument {
            source_id: "source:test".to_string(),
            source_type: "strategy-markdown".to_string(),
            path: "C:/tmp/opsp.md".to_string(),
            content_hash: "abc".to_string(),
            imported_at: "2026-04-14T00:00:00Z".to_string(),
        };
        let kernel = parse_strategy_markdown(
            "sitesorted-business",
            &sample_strategy_markdown(),
            &source_doc,
        )
        .expect("kernel");

        assert_eq!(kernel.scope_id, "sitesorted-business");
        assert_eq!(kernel.goals.len(), 1);
        assert_eq!(kernel.kpis.len(), 2);
        assert_eq!(kernel.initiatives.len(), 2);
        assert_eq!(kernel.constraints.len(), 1);
        assert_eq!(kernel.assumptions.len(), 1);
        assert_eq!(
            kernel.kpis[0].source_refs[0].section_path,
            "One-Page Strategic Plan > KPIs"
        );
    }

    #[test]
    fn test_parse_strategy_json_imports_kernel_with_provenance() {
        let source_doc = StrategySourceDocument {
            source_id: "source:test-json".to_string(),
            source_type: "strategy-json".to_string(),
            path: "C:/tmp/strategic-plan.context.json".to_string(),
            content_hash: "abc".to_string(),
            imported_at: "2026-04-14T00:00:00Z".to_string(),
        };
        let kernel =
            parse_strategy_json("sitesorted-business", &sample_strategy_json(), &source_doc)
                .expect("kernel");

        assert_eq!(kernel.scope_id, "sitesorted-business");
        assert_eq!(kernel.goals.len(), 1);
        assert_eq!(kernel.kpis.len(), 1);
        assert_eq!(kernel.initiatives.len(), 1);
        assert_eq!(kernel.constraints.len(), 1);
        assert_eq!(kernel.assumptions.len(), 1);
        assert_eq!(kernel.goals[0].source_refs[0].section_path, "Annual Goals");
        assert_eq!(
            kernel.kpis[0].goal_ids,
            vec!["goal-annual-first-ten".to_string()]
        );
        assert_eq!(
            kernel.initiatives[0].supports_goal_ids,
            vec!["goal-annual-first-ten".to_string()]
        );
    }

    #[test]
    fn test_parse_strategy_json_rejects_scope_mismatch() {
        let source_doc = StrategySourceDocument {
            source_id: "source:test-json".to_string(),
            source_type: "strategy-json".to_string(),
            path: "C:/tmp/strategic-plan.context.json".to_string(),
            content_hash: "abc".to_string(),
            imported_at: "2026-04-14T00:00:00Z".to_string(),
        };
        let content = sample_strategy_json().replace("sitesorted-business", "other-business");
        let err = parse_strategy_json("sitesorted-business", &content, &source_doc)
            .expect_err("scope mismatch should fail");
        assert!(err.to_string().contains("does not match requested scope"));
    }

    #[test]
    fn test_strategy_status_marks_missing_metrics_unknown() {
        let kpi = StrategyKpi {
            kpi_id: "outreach-volume".to_string(),
            title: "Outreach volume".to_string(),
            metric_key: "outreach_volume".to_string(),
            unit: Some("weekly".to_string()),
            target: Some(1500.0),
            green_threshold: Some(1500.0),
            yellow_threshold: Some(1000.0),
            cadence: None,
            due_date: None,
            goal_ids: Vec::new(),
            initiative_ids: Vec::new(),
            source_refs: Vec::new(),
        };
        let item = status_for_kpi(&kpi, &StrategyMetricsSnapshot::default());
        assert_eq!(item.status, "unknown");
        assert!(item.missing_instrumentation);
    }

    #[test]
    fn test_kpi_null_current_stays_unknown() {
        let kpi = StrategyKpi {
            kpi_id: "outreach-volume".to_string(),
            title: "Outreach volume".to_string(),
            metric_key: "outreach_volume".to_string(),
            unit: Some("weekly".to_string()),
            target: Some(1500.0),
            green_threshold: Some(1500.0),
            yellow_threshold: Some(1000.0),
            cadence: None,
            due_date: None,
            goal_ids: Vec::new(),
            initiative_ids: Vec::new(),
            source_refs: Vec::new(),
        };
        let metrics = StrategyMetricsSnapshot {
            kpis: BTreeMap::from([(
                "outreach_volume".to_string(),
                StrategyMetricRecord {
                    current: None,
                    unit: Some("weekly".to_string()),
                    updated_at: Some(Utc::now().to_rfc3339()),
                },
            )]),
            ..Default::default()
        };
        let item = status_for_kpi(&kpi, &metrics);
        assert_eq!(item.status, "unknown");
        assert!(item.missing_instrumentation);
    }

    #[test]
    fn test_strategy_recommend_prefers_instrumentation_first() {
        let item = StrategyStatusItem {
            item_id: "outreach-volume".to_string(),
            item_kind: "kpi".to_string(),
            title: "Outreach volume".to_string(),
            supports: Vec::new(),
            status: "unknown".to_string(),
            evidence: vec!["No metric value".to_string()],
            evidence_freshness: "unknown".to_string(),
            confidence: "low".to_string(),
            missing_instrumentation: true,
            due_date: None,
            depends_on: Vec::new(),
        };
        let nudge = nudge_for_status_item(&item).expect("nudge");
        assert!(nudge.task.contains("Instrument"));
        assert_eq!(
            nudge.suppression_reason.as_deref(),
            Some("instrumentation_first")
        );
    }

    #[test]
    fn test_red_fresh_nudge_can_preempt_continuity() {
        let item = StrategyStatusItem {
            item_id: "paying-customers".to_string(),
            item_kind: "kpi".to_string(),
            title: "Paying customers".to_string(),
            supports: vec!["goal:goal-annual-first-ten".to_string()],
            status: "red".to_string(),
            evidence: vec!["Current value: 1".to_string(), "Target: 10".to_string()],
            evidence_freshness: "fresh".to_string(),
            confidence: "medium".to_string(),
            missing_instrumentation: false,
            due_date: None,
            depends_on: Vec::new(),
        };
        let nudge = nudge_for_status_item(&item).expect("nudge");
        assert_eq!(nudge.interrupt_level, "interrupt");
        assert_eq!(nudge.confidence, "high");
        assert!(nudge_can_preempt_continuity(&nudge));
    }

    #[test]
    fn test_setup_and_store_round_trip_uses_scope_storage_dir() {
        let temp = tempdir().expect("tempdir");
        let artifact_path = temp.path().join("ops.md");
        fs::write(&artifact_path, sample_strategy_markdown()).expect("artifact");

        let storage_dir = temp.path().join("strategy-store");
        let metrics_path = storage_dir.join(STRATEGY_DEFAULT_METRICS_FILE);
        let registry = build_source_registry(
            "sitesorted-business",
            &artifact_path,
            &StrategyScopeConfig {
                enabled: true,
                label: Some("sitesorted-business".to_string()),
                artifact_path: Some(artifact_path.clone()),
                metrics_path: Some(metrics_path.clone()),
                continuity_project_path: None,
                storage_dir: Some(storage_dir.clone()),
                signal_paths: Vec::new(),
            },
            &StrategyStorePaths {
                storage_dir: storage_dir.clone(),
                registry_path: storage_dir.join(STRATEGY_REGISTRY_FILE),
                kernel_path: storage_dir.join(STRATEGY_KERNEL_FILE),
                metrics_path: metrics_path.clone(),
            },
            false,
            false,
        )
        .expect("registry");
        let kernel = import_strategy_kernel("sitesorted-business", &artifact_path).expect("kernel");

        save_registry(&storage_dir.join(STRATEGY_REGISTRY_FILE), &registry).expect("save registry");
        save_kernel(&storage_dir.join(STRATEGY_KERNEL_FILE), &kernel).expect("save kernel");
        ensure_metrics_file(&metrics_path, "sitesorted-business").expect("metrics file");

        assert!(storage_dir.join(STRATEGY_REGISTRY_FILE).exists());
        assert!(storage_dir.join(STRATEGY_KERNEL_FILE).exists());
        assert!(metrics_path.exists());
    }

    #[test]
    fn test_read_falls_back_to_existing_store_and_rehydrates_empty_kernel() {
        let _guard = ENV_LOCK.lock().expect("env lock");
        let temp = tempdir().expect("tempdir");
        let config_dir = temp.path().join("config");
        let data_dir = temp.path().join("data");
        std::env::set_var("MUNIN_CONFIG_DIR", &config_dir);
        std::env::set_var("MUNIN_DATA_DIR", &data_dir);

        let mut config = crate::core::config::Config::default();
        config.strategy.default_scope = Some("sitesorted-business".to_string());
        config.save().expect("config");

        let storage_dir = data_dir.join(STRATEGY_DIR).join("sitesorted-business");
        let artifact_path = storage_dir.join(STRATEGY_TEMPLATE_JSON_FILE);
        let metrics_path = storage_dir.join(STRATEGY_DEFAULT_METRICS_FILE);
        fs::create_dir_all(&storage_dir).expect("storage dir");
        fs::write(&artifact_path, sample_strategy_json()).expect("artifact");

        let scope_config = StrategyScopeConfig {
            enabled: true,
            label: Some("sitesorted-business".to_string()),
            artifact_path: Some(artifact_path.clone()),
            metrics_path: Some(metrics_path.clone()),
            continuity_project_path: None,
            storage_dir: Some(storage_dir.clone()),
            signal_paths: Vec::new(),
        };
        let store_paths = StrategyStorePaths {
            storage_dir: storage_dir.clone(),
            registry_path: storage_dir.join(STRATEGY_REGISTRY_FILE),
            kernel_path: storage_dir.join(STRATEGY_KERNEL_FILE),
            metrics_path: metrics_path.clone(),
        };
        let registry = build_source_registry(
            "sitesorted-business",
            &artifact_path,
            &scope_config,
            &store_paths,
            false,
            false,
        )
        .expect("registry");
        save_registry(&store_paths.registry_path, &registry).expect("save registry");
        save_kernel(
            &store_paths.kernel_path,
            &StrategyKernel {
                schema_version: "strategy-kernel-v1".to_string(),
                scope_id: "sitesorted-business".to_string(),
                imported_at: Utc::now().to_rfc3339(),
                sources: Vec::new(),
                goals: Vec::new(),
                kpis: Vec::new(),
                initiatives: Vec::new(),
                constraints: Vec::new(),
                assumptions: Vec::new(),
            },
        )
        .expect("save empty kernel");
        ensure_metrics_file(&metrics_path, "sitesorted-business").expect("metrics");

        let report = inspect(&StrategyReadOptions {
            scope: "sitesorted-business".to_string(),
        })
        .expect("inspect should fall back to existing store");
        let saved_kernel = load_kernel(&store_paths.kernel_path).expect("saved kernel");
        let status_report = status(&StrategyReadOptions {
            scope: "sitesorted-business".to_string(),
        })
        .expect("status");

        assert_eq!(report.kernel.goals.len(), 1);
        assert_eq!(saved_kernel.kpis.len(), 1);
        assert!(status_report
            .warnings
            .iter()
            .any(|warning| warning.contains("no current values")));

        std::env::remove_var("MUNIN_CONFIG_DIR");
        std::env::remove_var("MUNIN_DATA_DIR");
    }

    #[test]
    fn test_metrics_get_hydrates_ingested_kpi_slots() {
        let _guard = ENV_LOCK.lock().expect("env lock");
        let temp = tempdir().expect("tempdir");
        let config_dir = temp.path().join("config");
        let data_dir = temp.path().join("data");
        std::env::set_var("MUNIN_CONFIG_DIR", &config_dir);
        std::env::set_var("MUNIN_DATA_DIR", &data_dir);

        let mut config = crate::core::config::Config::default();
        config.strategy.default_scope = Some("sitesorted-business".to_string());
        config.save().expect("config");

        let storage_dir = data_dir.join(STRATEGY_DIR).join("sitesorted-business");
        let artifact_path = storage_dir.join(STRATEGY_TEMPLATE_JSON_FILE);
        let metrics_path = storage_dir.join(STRATEGY_DEFAULT_METRICS_FILE);
        fs::create_dir_all(&storage_dir).expect("storage dir");
        fs::write(&artifact_path, sample_strategy_json()).expect("artifact");
        let scope_config = StrategyScopeConfig {
            enabled: true,
            label: Some("sitesorted-business".to_string()),
            artifact_path: Some(artifact_path.clone()),
            metrics_path: Some(metrics_path.clone()),
            continuity_project_path: None,
            storage_dir: Some(storage_dir.clone()),
            signal_paths: Vec::new(),
        };
        let store_paths = StrategyStorePaths {
            storage_dir: storage_dir.clone(),
            registry_path: storage_dir.join(STRATEGY_REGISTRY_FILE),
            kernel_path: storage_dir.join(STRATEGY_KERNEL_FILE),
            metrics_path: metrics_path.clone(),
        };
        let registry = build_source_registry(
            "sitesorted-business",
            &artifact_path,
            &scope_config,
            &store_paths,
            false,
            false,
        )
        .expect("registry");
        let kernel = import_strategy_kernel("sitesorted-business", &artifact_path).expect("kernel");
        save_registry(&store_paths.registry_path, &registry).expect("save registry");
        save_kernel(&store_paths.kernel_path, &kernel).expect("save kernel");
        ensure_metrics_file(&metrics_path, "sitesorted-business").expect("metrics");

        let report = metrics_get(StrategyMetricGetOptions {
            scope: "sitesorted-business".to_string(),
            metric_key: None,
        })
        .expect("metrics get");

        assert!(report.kpis.contains_key("outreach_volume"));
        assert!(report.kpis.values().all(|record| record.current.is_none()));
        assert!(report
            .warnings
            .iter()
            .any(|warning| warning.contains("ingested strategy KPI definitions")));

        std::env::remove_var("MUNIN_CONFIG_DIR");
        std::env::remove_var("MUNIN_DATA_DIR");
    }

    #[test]
    fn test_bootstrap_setup_stages_template_without_importing_placeholder_kernel() {
        let _guard = ENV_LOCK.lock().expect("env lock");
        let temp = tempdir().expect("tempdir");
        let config_dir = temp.path().join("config");
        let data_dir = temp.path().join("data");
        let db_path = temp.path().join("history.db");

        std::env::set_var("MUNIN_CONFIG_DIR", &config_dir);
        std::env::set_var("MUNIN_DATA_DIR", &data_dir);
        std::env::set_var("MUNIN_DB_PATH", &db_path);

        let report = setup(&StrategySetupOptions {
            scope: DEFAULT_STRATEGY_SCOPE.to_string(),
            import_path: None,
            bootstrap_claude: true,
            template: false,
        })
        .expect("bootstrap setup");
        let kernel = load_kernel(&PathBuf::from(&report.kernel_path)).expect("kernel");

        assert!(report.bootstrap_requested);
        assert_eq!(report.imported_goal_count, 0);
        assert!(PathBuf::from(&report.artifact_path).exists());
        assert!(kernel.goals.is_empty());
        assert!(kernel.kpis.is_empty());
        assert!(kernel.sources.is_empty());

        std::env::remove_var("MUNIN_CONFIG_DIR");
        std::env::remove_var("MUNIN_DATA_DIR");
        std::env::remove_var("MUNIN_DB_PATH");
    }

    #[test]
    fn test_plain_setup_after_bootstrap_keeps_staged_kernel_empty() {
        let _guard = ENV_LOCK.lock().expect("env lock");
        let temp = tempdir().expect("tempdir");
        let config_dir = temp.path().join("config");
        let data_dir = temp.path().join("data");
        let db_path = temp.path().join("history.db");

        std::env::set_var("MUNIN_CONFIG_DIR", &config_dir);
        std::env::set_var("MUNIN_DATA_DIR", &data_dir);
        std::env::set_var("MUNIN_DB_PATH", &db_path);

        setup(&StrategySetupOptions {
            scope: DEFAULT_STRATEGY_SCOPE.to_string(),
            import_path: None,
            bootstrap_claude: true,
            template: false,
        })
        .expect("bootstrap setup");

        let second_report = setup(&StrategySetupOptions {
            scope: DEFAULT_STRATEGY_SCOPE.to_string(),
            import_path: None,
            bootstrap_claude: false,
            template: false,
        })
        .expect("plain follow-up setup");
        let kernel = load_kernel(&PathBuf::from(&second_report.kernel_path)).expect("kernel");

        assert!(second_report.bootstrap_requested);
        assert_eq!(second_report.imported_goal_count, 0);
        assert!(kernel.goals.is_empty());
        assert!(kernel.sources.is_empty());

        std::env::remove_var("MUNIN_CONFIG_DIR");
        std::env::remove_var("MUNIN_DATA_DIR");
        std::env::remove_var("MUNIN_DB_PATH");
    }
}
