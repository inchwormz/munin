use super::config::{context_data_dir, Config, ProactivityProvider};
use super::memory_os::{
    MemoryOsCorrectionPatternSummary, MemoryOsFrictionFix, MemoryOsInspectionScope,
    MemoryOsNarrativeFinding, MemoryOsOverviewReport, MemoryOsProjectSummary,
};
use super::strategy::{self, StrategicNudge, StrategyReadOptions, StrategyRecommendReport};
use super::tracking::Tracker;
use super::utils::resolve_binary;
use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Duration, Local, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const PROACTIVITY_DIR: &str = "proactivity";
const COMPLETED_FILE: &str = "completed.json";
const HEARTBEAT_FILE: &str = "proactivity-daemon.heartbeat";
const SCHEDULE_SWEEP_INTERVAL_MINUTES: u32 = 30;
const MORNING_PROMPT_TOKEN: &str = "munin-morning";
const MAX_FRICTION_NUDGES: usize = 12;

#[derive(Debug, Clone)]
pub struct ProactivityRunOptions {
    pub scope: Option<String>,
    pub provider: Option<ProactivityProvider>,
    pub dry_run: bool,
    pub auto_spawn: bool,
    pub no_spawn: bool,
}

#[derive(Debug, Clone)]
pub struct ProactivityScopeOptions {
    pub scope: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ProactivityScheduleInstallOptions {
    pub scope: Option<String>,
    pub provider: Option<ProactivityProvider>,
    pub project_path: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct ProactivityClaimOptions {
    pub job_id: String,
}

#[derive(Debug, Clone)]
pub struct ProactivityApproveOptions {
    pub job_id: String,
    pub no_spawn: bool,
}

#[derive(Debug, Clone)]
pub struct ProactivityCompleteOptions {
    pub job_id: String,
    pub status: ProactivityTerminalStatus,
    pub summary: String,
    pub error: Option<String>,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ProactivityTerminalStatus {
    Complete,
    Failed,
    Deferred,
    Suppressed,
}

impl ProactivityTerminalStatus {
    #[allow(dead_code)]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::Failed => "failed",
            Self::Deferred => "deferred",
            Self::Suppressed => "suppressed",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ProactivityDecisionOutcome {
    QueueApproval,
    Deferred,
    Suppressed,
    Error,
}

impl ProactivityDecisionOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::QueueApproval => "queue-approval",
            Self::Deferred => "deferred",
            Self::Suppressed => "suppressed",
            Self::Error => "error",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProactivityPaths {
    pub queue_dir: PathBuf,
    pub results_dir: PathBuf,
    pub briefs_dir: PathBuf,
    pub state_dir: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProactivityJob {
    pub schema_version: String,
    pub job_type: String,
    pub job_id: String,
    pub scope_id: String,
    pub local_date: String,
    pub created_at: String,
    pub provider: ProactivityProvider,
    pub project_path: String,
    pub session_name: String,
    pub prompt_token: String,
    pub brief_path: String,
    pub launch_instructions_path: String,
    pub decision_path: String,
    pub result_path: String,
    pub continuity_active: bool,
    pub nudge_tasks: Vec<String>,
    #[serde(default)]
    pub intervention_job_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProactivityDecisionArtifact {
    pub schema_version: String,
    pub generated_at: String,
    pub job_id: String,
    pub scope_id: String,
    pub local_date: String,
    pub provider: ProactivityProvider,
    pub outcome: ProactivityDecisionOutcome,
    pub reasons: Vec<String>,
    pub continuity_active: bool,
    pub brief_path: String,
    pub queue_path: Option<String>,
    pub result_path: Option<String>,
    pub nudge_tasks: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProactivityResultArtifact {
    pub schema_version: String,
    pub recorded_at: String,
    pub job_id: String,
    pub scope_id: String,
    pub local_date: String,
    pub provider: ProactivityProvider,
    pub status: ProactivityTerminalStatus,
    pub summary: String,
    pub error: Option<String>,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProactivityCompletedRecord {
    pub job_id: String,
    pub scope_id: String,
    pub local_date: String,
    pub provider: ProactivityProvider,
    pub status: ProactivityTerminalStatus,
    pub recorded_at: String,
    pub result_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ProactivityCompletedIndex {
    pub schema_version: String,
    pub records: Vec<ProactivityCompletedRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProactivityRunReport {
    pub generated_at: String,
    pub scope_id: String,
    pub local_date: String,
    pub provider: ProactivityProvider,
    pub outcome: ProactivityDecisionOutcome,
    pub reasons: Vec<String>,
    pub project_path: String,
    pub queue_path: Option<String>,
    pub claim_path: Option<String>,
    pub brief_path: String,
    pub launch_instructions_path: Option<String>,
    pub decision_path: String,
    pub result_path: Option<String>,
    pub spawned: bool,
    pub spawn_command_preview: Option<String>,
    pub continuity_active: bool,
    pub nudges: Vec<StrategicNudge>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProactivitySweepReport {
    pub generated_at: String,
    pub scope_id: String,
    pub released_stale_pending: usize,
    pub released_stale_claims: usize,
    pub finalized_results: usize,
    pub pending_jobs: usize,
    pub claimed_jobs: usize,
    pub result_files: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProactivityClaimReport {
    pub generated_at: String,
    pub job_id: String,
    pub claimed: bool,
    pub queue_path: Option<String>,
    pub claim_path: Option<String>,
    pub result_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProactivityApproveReport {
    pub generated_at: String,
    pub job_id: String,
    pub claimed: bool,
    pub launched: bool,
    pub queue_path: Option<String>,
    pub claim_path: Option<String>,
    pub result_path: Option<String>,
    pub launch_instructions_path: Option<String>,
    pub spawn_command_preview: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProactivityCompleteReport {
    pub generated_at: String,
    pub job_id: String,
    pub result_path: String,
    pub status: ProactivityTerminalStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProactivityScheduleTaskStatus {
    pub name: String,
    pub installed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProactivityStatusReport {
    pub generated_at: String,
    pub scope_id: String,
    pub provider: ProactivityProvider,
    pub project_path: String,
    pub schedule_local: String,
    pub max_spawns_per_day: u32,
    pub stale_claim_minutes: u64,
    pub paths: ProactivityPaths,
    pub today_job_id: String,
    pub today_pending: bool,
    pub today_claimed: bool,
    pub today_result_status: Option<ProactivityTerminalStatus>,
    pub completed_records: usize,
    pub morning_task: ProactivityScheduleTaskStatus,
    pub alternate_morning_tasks: Vec<ProactivityScheduleTaskStatus>,
    pub sweep_task: ProactivityScheduleTaskStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProactivityScheduleInstallReport {
    pub generated_at: String,
    pub scope_id: String,
    pub provider: ProactivityProvider,
    pub project_path: String,
    pub morning_task: String,
    pub sweep_task: String,
    pub schedule_local: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProactivityScheduleRemoveReport {
    pub generated_at: String,
    pub scope_id: String,
    pub removed_tasks: Vec<String>,
}

#[derive(Debug)]
struct RuntimeContext {
    config: Config,
    scope_id: String,
    provider: ProactivityProvider,
    project_path: PathBuf,
    schedule_local: String,
    max_spawns_per_day: u32,
    stale_claim_minutes: u64,
    paths: ProactivityPaths,
}

#[derive(Debug, Clone)]
struct FileSet {
    job_id: String,
    local_date: String,
    pending_path: PathBuf,
    claim_path: PathBuf,
    result_path: PathBuf,
    decision_path: PathBuf,
    brief_path: PathBuf,
    launch_instructions_path: PathBuf,
    completed_path: PathBuf,
    heartbeat_path: PathBuf,
}

#[derive(Debug, Clone)]
struct LaunchCommand {
    preview: String,
    runner: String,
    args: Vec<String>,
}

pub fn run(options: &ProactivityRunOptions) -> Result<ProactivityRunReport> {
    let runtime = resolve_runtime(options.scope.as_deref(), options.provider, None)?;
    ensure_runtime_dirs(&runtime.paths)?;
    write_heartbeat(&runtime)?;
    let _ = sweep_internal(&runtime)?;
    let tracker = Tracker::new().context("Failed to initialize tracking database")?;

    let strategy_bootstrap_requested;
    let mut recommend_report;
    match (
        strategy::status(&StrategyReadOptions {
            scope: runtime.scope_id.clone(),
        }),
        strategy::recommend(&StrategyReadOptions {
            scope: runtime.scope_id.clone(),
        }),
    ) {
        (Ok(status), Ok(recommend)) => {
            strategy_bootstrap_requested = status.registry.bootstrap_requested;
            recommend_report = recommend;
        }
        (status_result, recommend_result) => {
            strategy_bootstrap_requested = false;
            let warning = status_result
                .err()
                .or_else(|| recommend_result.err())
                .map(|err| format!("Strategy unavailable; using friction-only nudges: {err}"))
                .unwrap_or_else(|| "Strategy unavailable; using friction-only nudges.".to_string());
            recommend_report = fallback_recommend_report(&runtime.scope_id, warning);
        }
    }
    add_friction_nudges(&tracker, &runtime, &mut recommend_report)?;
    let files = file_set(&runtime)?;
    let completed_index = load_completed_index(&files.completed_path)?;
    let mut decision = evaluate_decision(
        &runtime,
        &files,
        strategy_bootstrap_requested,
        &recommend_report,
        &completed_index,
    )?;
    let brief = build_brief(&runtime, &files, &decision, &recommend_report)?;
    write_text_file(&files.brief_path, &brief)?;
    write_json_file(&files.decision_path, &decision)?;

    let mut spawned = false;
    let mut spawn_command_preview = None;
    let mut launch_instructions_path = None;

    match decision.outcome {
        ProactivityDecisionOutcome::QueueApproval => {
            let job = build_job(&runtime, &files, &recommend_report)?;
            write_json_file(&files.pending_path, &job)?;
            let launch_instructions = build_launch_instructions(&runtime, &job)?;
            write_text_file(&files.launch_instructions_path, &launch_instructions)?;
            decision.queue_path = Some(files.pending_path.display().to_string());
            let should_auto_spawn = runtime.config.proactivity.auto_spawn || options.auto_spawn;
            if should_auto_spawn {
                let launch = build_launch_command(&runtime, &job, &files.launch_instructions_path)?;
                if !(options.dry_run || options.no_spawn) {
                    if files.pending_path.exists() {
                        fs::rename(&files.pending_path, &files.claim_path).with_context(|| {
                            format!(
                                "Failed to claim proactivity job {} before spawn",
                                files.job_id
                            )
                        })?;
                    }
                    if let Err(err) = run_launch_command(&launch) {
                        if files.claim_path.exists() && !files.pending_path.exists() {
                            let _ = fs::rename(&files.claim_path, &files.pending_path);
                        }
                        return Err(err);
                    }
                    spawned = true;
                }
                spawn_command_preview = Some(launch.preview.clone());
                launch_instructions_path =
                    Some(files.launch_instructions_path.display().to_string());
            }
            write_json_file(&files.decision_path, &decision)?;
        }
        ProactivityDecisionOutcome::Deferred
        | ProactivityDecisionOutcome::Suppressed
        | ProactivityDecisionOutcome::Error => {
            let status = match decision.outcome {
                ProactivityDecisionOutcome::Deferred => ProactivityTerminalStatus::Deferred,
                ProactivityDecisionOutcome::Suppressed => ProactivityTerminalStatus::Suppressed,
                ProactivityDecisionOutcome::Error => ProactivityTerminalStatus::Failed,
                ProactivityDecisionOutcome::QueueApproval => unreachable!(),
            };
            let result = build_terminal_result(
                &runtime,
                &files,
                status,
                &decision.reasons.join("; "),
                None,
                Vec::new(),
            );
            write_json_file(&files.result_path, &result)?;
            update_completed_index(&files.completed_path, &result, &files.result_path)?;
        }
    }
    record_proactivity_approval_jobs(
        &tracker,
        &runtime,
        &files,
        &decision,
        &recommend_report,
        spawned,
    )?;

    Ok(ProactivityRunReport {
        generated_at: Utc::now().to_rfc3339(),
        scope_id: runtime.scope_id,
        local_date: files.local_date,
        provider: runtime.provider,
        outcome: decision.outcome,
        reasons: decision.reasons,
        project_path: runtime.project_path.display().to_string(),
        queue_path: decision.queue_path,
        claim_path: Some(files.claim_path.display().to_string()),
        brief_path: files.brief_path.display().to_string(),
        launch_instructions_path,
        decision_path: files.decision_path.display().to_string(),
        result_path: decision.result_path,
        spawned,
        spawn_command_preview,
        continuity_active: recommend_report.continuity.active,
        nudges: recommend_report.nudges,
    })
}

pub fn sweep(options: &ProactivityScopeOptions) -> Result<ProactivitySweepReport> {
    let runtime = resolve_runtime(options.scope.as_deref(), None, None)?;
    ensure_runtime_dirs(&runtime.paths)?;
    write_heartbeat(&runtime)?;
    sweep_internal(&runtime)
}

pub fn claim(options: &ProactivityClaimOptions) -> Result<ProactivityClaimReport> {
    let config = Config::load().context("Failed to load config.toml")?;
    let paths = resolve_paths(&config.proactivity)?;
    ensure_runtime_dirs(&paths)?;
    let _ = ensure_job_projection(&paths, &options.job_id)?;
    let (pending_path, claim_path, result_path) = locate_job_paths(&paths, &options.job_id)?;
    let mut claimed = false;
    if pending_path.exists() {
        fs::rename(&pending_path, &claim_path).with_context(|| {
            format!(
                "Failed to claim proactivity job {} via {}",
                options.job_id,
                claim_path.display()
            )
        })?;
        claimed = true;
    }

    Ok(ProactivityClaimReport {
        generated_at: Utc::now().to_rfc3339(),
        job_id: options.job_id.clone(),
        claimed,
        queue_path: pending_path
            .exists()
            .then(|| pending_path.display().to_string()),
        claim_path: claim_path
            .exists()
            .then(|| claim_path.display().to_string()),
        result_path: result_path
            .exists()
            .then(|| result_path.display().to_string()),
    })
}

pub fn approve(options: &ProactivityApproveOptions) -> Result<ProactivityApproveReport> {
    let config = Config::load().context("Failed to load config.toml")?;
    let paths = resolve_paths(&config.proactivity)?;
    let _ = ensure_job_projection(&paths, &options.job_id)?;
    let (pending_path, claim_path, result_path) = locate_job_paths(&paths, &options.job_id)?;
    if !pending_path.exists() && claim_path.exists() {
        return Ok(ProactivityApproveReport {
            generated_at: Utc::now().to_rfc3339(),
            job_id: options.job_id.clone(),
            claimed: false,
            launched: false,
            queue_path: None,
            claim_path: Some(claim_path.display().to_string()),
            result_path: result_path
                .exists()
                .then(|| result_path.display().to_string()),
            launch_instructions_path: None,
            spawn_command_preview: None,
        });
    }

    let job = load_job_for_completion(&paths, &options.job_id)?;
    let runtime = RuntimeContext {
        scope_id: job.scope_id.clone(),
        provider: job.provider,
        project_path: PathBuf::from(&job.project_path),
        schedule_local: config.proactivity.schedule_local.clone(),
        max_spawns_per_day: config.proactivity.max_spawns_per_day,
        stale_claim_minutes: config.proactivity.stale_claim_minutes,
        paths: paths.clone(),
        config,
    };
    ensure_runtime_dirs(&runtime.paths)?;
    let mut claimed = false;
    if pending_path.exists() {
        fs::rename(&pending_path, &claim_path).with_context(|| {
            format!(
                "Failed to approve queued proactivity job {} via {}",
                options.job_id,
                claim_path.display()
            )
        })?;
        claimed = true;
    }

    let mut launched = false;
    let mut spawn_command_preview = None;
    if claimed && !options.no_spawn && claim_path.exists() {
        let launch =
            build_launch_command(&runtime, &job, Path::new(&job.launch_instructions_path))?;
        run_launch_command(&launch)?;
        launched = true;
        spawn_command_preview = Some(launch.preview.clone());
    }
    if let Ok(tracker) = Tracker::new() {
        let claim_path_text = claim_path.display().to_string();
        let result_path_text = result_path.display().to_string();
        let _ = tracker.set_approval_job_status(
            &options.job_id,
            if launched {
                crate::core::tracking::ApprovalJobStatus::Approved
            } else if claimed {
                crate::core::tracking::ApprovalJobStatus::Approved
            } else {
                crate::core::tracking::ApprovalJobStatus::Queued
            },
            Some(claim_path_text.as_str()),
            result_path.exists().then_some(result_path_text.as_str()),
            None,
        );
        for intervention_job_id in &job.intervention_job_ids {
            let _ = tracker.set_approval_job_status(
                intervention_job_id,
                crate::core::tracking::ApprovalJobStatus::Approved,
                Some(claim_path_text.as_str()),
                result_path.exists().then_some(result_path_text.as_str()),
                None,
            );
        }
    }

    Ok(ProactivityApproveReport {
        generated_at: Utc::now().to_rfc3339(),
        job_id: options.job_id.clone(),
        claimed,
        launched,
        queue_path: pending_path
            .exists()
            .then(|| pending_path.display().to_string()),
        claim_path: claim_path
            .exists()
            .then(|| claim_path.display().to_string()),
        result_path: result_path
            .exists()
            .then(|| result_path.display().to_string()),
        launch_instructions_path: Some(job.launch_instructions_path),
        spawn_command_preview,
    })
}

pub fn complete(options: &ProactivityCompleteOptions) -> Result<ProactivityCompleteReport> {
    let config = Config::load().context("Failed to load config.toml")?;
    let paths = resolve_paths(&config.proactivity)?;
    ensure_runtime_dirs(&paths)?;
    let _ = ensure_job_projection(&paths, &options.job_id)?;
    let (pending_path, claim_path, result_path) = locate_job_paths(&paths, &options.job_id)?;
    let job = load_job_for_completion(&paths, &options.job_id)?;
    let result = ProactivityResultArtifact {
        schema_version: "munin-proactivity-result-v1".to_string(),
        recorded_at: Utc::now().to_rfc3339(),
        job_id: job.job_id.clone(),
        scope_id: job.scope_id.clone(),
        local_date: job.local_date.clone(),
        provider: job.provider,
        status: options.status,
        summary: options.summary.clone(),
        error: options.error.clone(),
        notes: options.notes.clone(),
    };
    write_json_file(&result_path, &result)?;
    let completed_path = paths.state_dir.join(COMPLETED_FILE);
    update_completed_index(&completed_path, &result, &result_path)?;
    if pending_path.exists() {
        let _ = fs::remove_file(&pending_path);
    }
    if claim_path.exists() {
        let _ = fs::remove_file(&claim_path);
    }
    if let Ok(tracker) = Tracker::new() {
        let _ = update_terminal_approval_jobs(
            &tracker,
            &job,
            &result_path,
            options.status,
            &options.summary,
        );
    }

    Ok(ProactivityCompleteReport {
        generated_at: Utc::now().to_rfc3339(),
        job_id: options.job_id.clone(),
        result_path: result_path.display().to_string(),
        status: options.status,
    })
}

fn update_terminal_approval_jobs(
    tracker: &Tracker,
    job: &ProactivityJob,
    result_path: &Path,
    status: ProactivityTerminalStatus,
    summary: &str,
) -> Result<()> {
    let result_path_text = result_path.display().to_string();
    let durable_status = match status {
        ProactivityTerminalStatus::Complete => crate::core::tracking::ApprovalJobStatus::Completed,
        ProactivityTerminalStatus::Failed => crate::core::tracking::ApprovalJobStatus::Failed,
        ProactivityTerminalStatus::Deferred => crate::core::tracking::ApprovalJobStatus::Deferred,
        ProactivityTerminalStatus::Suppressed => crate::core::tracking::ApprovalJobStatus::Rejected,
    };
    let _ = tracker.set_approval_job_status(
        &job.job_id,
        durable_status,
        None,
        Some(result_path_text.as_str()),
        Some(summary),
    )?;
    if let Some(intervention_job_id) = job.intervention_job_ids.first() {
        let _ = tracker.set_approval_job_status(
            intervention_job_id,
            durable_status,
            None,
            Some(result_path_text.as_str()),
            Some(summary),
        )?;
    }
    Ok(())
}

pub fn status(options: &ProactivityScopeOptions) -> Result<ProactivityStatusReport> {
    let runtime = resolve_runtime(options.scope.as_deref(), None, None)?;
    ensure_runtime_dirs(&runtime.paths)?;
    let files = file_set(&runtime)?;
    let completed_index = load_completed_index(&files.completed_path)?;
    let today_result_status = if files.result_path.exists() {
        Some(load_result(&files.result_path)?.status)
    } else {
        None
    };

    Ok(ProactivityStatusReport {
        generated_at: Utc::now().to_rfc3339(),
        scope_id: runtime.scope_id.clone(),
        provider: runtime.provider,
        project_path: runtime.project_path.display().to_string(),
        schedule_local: runtime.schedule_local.clone(),
        max_spawns_per_day: runtime.max_spawns_per_day,
        stale_claim_minutes: runtime.stale_claim_minutes,
        paths: runtime.paths.clone(),
        today_job_id: files.job_id,
        today_pending: files.pending_path.exists(),
        today_claimed: files.claim_path.exists(),
        today_result_status,
        completed_records: completed_index.records.len(),
        morning_task: ProactivityScheduleTaskStatus {
            name: morning_task_name(&runtime.scope_id, runtime.provider),
            installed: query_task_installed(&morning_task_name(
                &runtime.scope_id,
                runtime.provider,
            )),
        },
        alternate_morning_tasks: alternate_morning_task_statuses(
            &runtime.scope_id,
            runtime.provider,
        ),
        sweep_task: ProactivityScheduleTaskStatus {
            name: sweep_task_name(&runtime.scope_id),
            installed: query_task_installed(&sweep_task_name(&runtime.scope_id)),
        },
    })
}

pub fn install_schedule(
    options: &ProactivityScheduleInstallOptions,
) -> Result<ProactivityScheduleInstallReport> {
    let mut runtime = resolve_runtime(
        options.scope.as_deref(),
        options.provider,
        options.project_path.as_deref(),
    )?;
    ensure_runtime_dirs(&runtime.paths)?;
    let exe = std::env::current_exe().context("Failed to resolve current munin executable")?;
    let mut installed_names = Vec::new();
    let install_result = (|| -> Result<()> {
        for stale in removable_task_names(&runtime.scope_id) {
            if query_task_installed(&stale) {
                delete_scheduled_task(&stale)?;
            }
        }
        let morning_name = morning_task_name(&runtime.scope_id, runtime.provider);
        let provider_arg = runtime.provider.as_str();
        install_scheduled_task(
            &morning_name,
            &exe,
            &[
                "proactivity",
                "run",
                "--scope",
                runtime.scope_id.as_str(),
                "--provider",
                provider_arg,
            ],
            ScheduleSpec::Daily {
                local_time: runtime.schedule_local.clone(),
            },
        )?;
        installed_names.push(morning_name);
        let sweep_name = sweep_task_name(&runtime.scope_id);
        install_scheduled_task(
            &sweep_name,
            &exe,
            &["proactivity", "sweep", "--scope", runtime.scope_id.as_str()],
            ScheduleSpec::IntervalMinutes(SCHEDULE_SWEEP_INTERVAL_MINUTES),
        )?;
        installed_names.push(sweep_name);
        Ok(())
    })();
    if let Err(err) = install_result {
        for name in installed_names {
            if query_task_installed(&name) {
                let _ = delete_scheduled_task(&name);
            }
        }
        return Err(err);
    }

    runtime.config.proactivity.enabled = true;
    runtime.config.proactivity.default_scope = Some(runtime.scope_id.clone());
    runtime.config.proactivity.provider = runtime.provider;
    runtime.config.proactivity.auto_spawn = true;
    runtime.config.proactivity.project_path = Some(runtime.project_path.clone());
    if let Err(err) = runtime
        .config
        .save()
        .context("Failed to save proactivity config")
    {
        for name in removable_task_names(&runtime.scope_id) {
            if query_task_installed(&name) {
                let _ = delete_scheduled_task(&name);
            }
        }
        return Err(err);
    }

    Ok(ProactivityScheduleInstallReport {
        generated_at: Utc::now().to_rfc3339(),
        scope_id: runtime.scope_id.clone(),
        provider: runtime.provider,
        project_path: runtime.project_path.display().to_string(),
        morning_task: morning_task_name(&runtime.scope_id, runtime.provider),
        sweep_task: sweep_task_name(&runtime.scope_id),
        schedule_local: runtime.schedule_local,
    })
}

pub fn remove_schedule(
    options: &ProactivityScopeOptions,
) -> Result<ProactivityScheduleRemoveReport> {
    let runtime = resolve_runtime(options.scope.as_deref(), None, None)?;
    let mut removed_tasks = Vec::new();
    for task in removable_task_names(&runtime.scope_id) {
        if query_task_installed(&task) {
            delete_scheduled_task(&task)?;
            removed_tasks.push(task);
        }
    }
    Ok(ProactivityScheduleRemoveReport {
        generated_at: Utc::now().to_rfc3339(),
        scope_id: runtime.scope_id,
        removed_tasks,
    })
}

fn resolve_runtime(
    requested_scope: Option<&str>,
    provider_override: Option<ProactivityProvider>,
    project_override: Option<&Path>,
) -> Result<RuntimeContext> {
    let config = Config::load().context("Failed to load config.toml")?;
    let scope_id = config
        .proactivity
        .resolve_scope_name(&config.strategy, requested_scope);
    validate_scope_id(&scope_id)?;
    let provider = provider_override.unwrap_or(config.proactivity.provider);
    let discovered_strategy_project_path =
        strategy::discover_inspect_reports(1)
            .ok()
            .and_then(|reports| {
                reports
                    .first()
                    .and_then(|report| report.registry.continuity_project_path.clone())
            });
    let project_path = project_override
        .map(Path::to_path_buf)
        .or_else(|| config.proactivity.project_path.clone())
        .or(discovered_strategy_project_path)
        .unwrap_or(std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let paths = resolve_paths(&config.proactivity)?;
    let schedule_local = config.proactivity.schedule_local.clone();
    let max_spawns_per_day = config.proactivity.max_spawns_per_day;
    let stale_claim_minutes = config.proactivity.stale_claim_minutes;

    Ok(RuntimeContext {
        config,
        scope_id,
        provider,
        project_path,
        schedule_local,
        max_spawns_per_day,
        stale_claim_minutes,
        paths,
    })
}

fn resolve_paths(config: &crate::core::config::ProactivityConfig) -> Result<ProactivityPaths> {
    let root = context_data_dir()?.join(PROACTIVITY_DIR);
    Ok(ProactivityPaths {
        queue_dir: config
            .queue_dir
            .clone()
            .unwrap_or_else(|| root.join("queue")),
        results_dir: config
            .results_dir
            .clone()
            .unwrap_or_else(|| root.join("results")),
        briefs_dir: config
            .briefs_dir
            .clone()
            .unwrap_or_else(|| root.join("briefs")),
        state_dir: config
            .state_dir
            .clone()
            .unwrap_or_else(|| root.join("state")),
    })
}

fn ensure_runtime_dirs(paths: &ProactivityPaths) -> Result<()> {
    fs::create_dir_all(&paths.queue_dir)?;
    fs::create_dir_all(&paths.results_dir)?;
    fs::create_dir_all(&paths.briefs_dir)?;
    fs::create_dir_all(&paths.state_dir)?;
    Ok(())
}

fn file_set(runtime: &RuntimeContext) -> Result<FileSet> {
    let local_date = Local::now().format("%Y-%m-%d").to_string();
    file_set_for_job(
        runtime,
        &build_job_id(&runtime.scope_id, runtime.provider, &local_date),
        &local_date,
    )
}

fn file_set_for_job(runtime: &RuntimeContext, job_id: &str, local_date: &str) -> Result<FileSet> {
    validate_job_id_file_stem(job_id)?;
    Ok(FileSet {
        job_id: job_id.to_string(),
        local_date: local_date.to_string(),
        pending_path: runtime.paths.queue_dir.join(format!("{job_id}.json")),
        claim_path: runtime
            .paths
            .queue_dir
            .join(format!("{job_id}.processing.json")),
        result_path: runtime.paths.results_dir.join(format!("{job_id}.json")),
        decision_path: runtime
            .paths
            .results_dir
            .join(format!("{job_id}.decision.json")),
        brief_path: runtime.paths.briefs_dir.join(format!("{job_id}.md")),
        launch_instructions_path: runtime.paths.briefs_dir.join(format!("{job_id}.launch.md")),
        completed_path: runtime.paths.state_dir.join(COMPLETED_FILE),
        heartbeat_path: runtime.paths.state_dir.join(HEARTBEAT_FILE),
    })
}

fn build_job_id(scope_id: &str, provider: ProactivityProvider, local_date: &str) -> String {
    format!("morning-{scope_id}-{}-{local_date}", provider.as_str())
}

fn validate_scope_id(scope_id: &str) -> Result<()> {
    validate_safe_identifier(scope_id, "proactivity scope")
}

fn validate_job_id_file_stem(job_id: &str) -> Result<()> {
    validate_safe_identifier(job_id, "proactivity job id")
}

fn validate_safe_identifier(value: &str, label: &str) -> Result<()> {
    let trimmed = value.trim();
    let safe = !trimmed.is_empty()
        && trimmed == value
        && trimmed != "."
        && trimmed != ".."
        && trimmed
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'));
    if !safe {
        anyhow::bail!(
            "invalid {label} `{value}`; use only ASCII letters, numbers, dots, underscores, and hyphens"
        );
    }
    Ok(())
}

fn build_intervention_job_id(
    scope_id: &str,
    local_date: &str,
    item_kind: &str,
    item_id: Option<&str>,
    task: &str,
) -> String {
    let stable_item = item_id.map(|value| value.to_string()).unwrap_or_else(|| {
        let mut hasher = Sha256::new();
        hasher.update(task.as_bytes());
        format!("{:x}", hasher.finalize())[..12].to_string()
    });
    format!("approval-{scope_id}-{local_date}-{item_kind}-{stable_item}")
}

fn preserve_approval_job_status(
    existing: Option<crate::core::tracking::ApprovalJobStatus>,
    incoming: crate::core::tracking::ApprovalJobStatus,
) -> crate::core::tracking::ApprovalJobStatus {
    use crate::core::tracking::ApprovalJobStatus;

    match existing {
        Some(ApprovalJobStatus::Completed)
        | Some(ApprovalJobStatus::Failed)
        | Some(ApprovalJobStatus::Rejected) => existing.expect("matched Some above"),
        Some(ApprovalJobStatus::Approved)
            if matches!(
                incoming,
                ApprovalJobStatus::Queued
                    | ApprovalJobStatus::Deferred
                    | ApprovalJobStatus::Suppressed
            ) =>
        {
            ApprovalJobStatus::Approved
        }
        Some(ApprovalJobStatus::Deferred) if incoming == ApprovalJobStatus::Suppressed => {
            ApprovalJobStatus::Deferred
        }
        _ => incoming,
    }
}

fn current_approval_job_statuses(
    tracker: &Tracker,
    job_ids: &[String],
) -> Result<HashMap<String, crate::core::tracking::ApprovalJobStatus>> {
    let mut statuses = HashMap::new();
    for job_id in job_ids {
        if let Some(record) = tracker.get_approval_job(job_id)? {
            statuses.insert(job_id.clone(), record.status);
        }
    }
    Ok(statuses)
}

fn record_proactivity_approval_jobs(
    tracker: &Tracker,
    runtime: &RuntimeContext,
    files: &FileSet,
    decision: &ProactivityDecisionArtifact,
    recommend_report: &StrategyRecommendReport,
    spawned: bool,
) -> Result<()> {
    let review_after = Some((Utc::now() + Duration::days(1)).to_rfc3339());
    let expires_at = Some((Utc::now() + Duration::days(7)).to_rfc3339());
    let queue_path = if spawned {
        Some(files.claim_path.display().to_string())
    } else {
        decision.queue_path.clone()
    };
    let result_path = decision.result_path.clone();
    let project_path = runtime.project_path.display().to_string();
    let aggregate_status = match decision.outcome {
        ProactivityDecisionOutcome::QueueApproval if spawned => {
            crate::core::tracking::ApprovalJobStatus::Approved
        }
        ProactivityDecisionOutcome::QueueApproval => {
            crate::core::tracking::ApprovalJobStatus::Queued
        }
        ProactivityDecisionOutcome::Deferred => crate::core::tracking::ApprovalJobStatus::Deferred,
        ProactivityDecisionOutcome::Suppressed => {
            crate::core::tracking::ApprovalJobStatus::Suppressed
        }
        ProactivityDecisionOutcome::Error => crate::core::tracking::ApprovalJobStatus::Failed,
    };
    let intervention_job_ids = recommend_report
        .nudges
        .iter()
        .map(|nudge| {
            build_intervention_job_id(
                &runtime.scope_id,
                &files.local_date,
                &nudge.item_kind,
                nudge.item_id.as_deref(),
                &nudge.task,
            )
        })
        .chain(recommend_report.suppressed_nudges.iter().map(|nudge| {
            build_intervention_job_id(
                &runtime.scope_id,
                &files.local_date,
                &nudge.item_kind,
                nudge.item_id.as_deref(),
                &nudge.task,
            )
        }))
        .collect::<Vec<_>>();
    let mut existing_statuses = current_approval_job_statuses(
        tracker,
        &std::iter::once(files.job_id.clone())
            .chain(intervention_job_ids.iter().cloned())
            .collect::<Vec<_>>(),
    )?;
    tracker.upsert_approval_job_for_project(
        &project_path,
        &crate::core::tracking::ApprovalJobInput {
            job_id: files.job_id.clone(),
            scope: "project".to_string(),
            scope_target: Some(project_path.clone()),
            local_date: files.local_date.clone(),
            item_id: Some(runtime.scope_id.clone()),
            item_kind: "morning-proactivity".to_string(),
            title: format!("Morning proactivity for {}", runtime.scope_id),
            summary: decision.reasons.join("; "),
            status: preserve_approval_job_status(
                existing_statuses.remove(&files.job_id),
                aggregate_status,
            ),
            source_kind: "proactivity-envelope".to_string(),
            provider: Some(runtime.provider.as_str().to_string()),
            continuity_active: decision.continuity_active,
            expected_effect: Some(
                "Review and execute the current highest-priority intervention.".to_string(),
            ),
            queue_path: queue_path.clone(),
            result_path: result_path.clone(),
            evidence_json: serde_json::to_string(&decision.nudge_tasks)?,
            review_after: review_after.clone(),
            expires_at: expires_at.clone(),
        },
    )?;

    for nudge in &recommend_report.nudges {
        let job_id = build_intervention_job_id(
            &runtime.scope_id,
            &files.local_date,
            &nudge.item_kind,
            nudge.item_id.as_deref(),
            &nudge.task,
        );
        tracker.upsert_approval_job_for_project(
            &project_path,
            &crate::core::tracking::ApprovalJobInput {
                job_id: job_id.clone(),
                scope: "project".to_string(),
                scope_target: Some(project_path.clone()),
                local_date: files.local_date.clone(),
                item_id: nudge.item_id.clone(),
                item_kind: nudge.item_kind.clone(),
                title: nudge.task.clone(),
                summary: nudge.why_now.clone(),
                status: preserve_approval_job_status(
                    existing_statuses.remove(&job_id),
                    aggregate_status,
                ),
                source_kind: "strategy-nudge".to_string(),
                provider: Some(runtime.provider.as_str().to_string()),
                continuity_active: recommend_report.continuity.active,
                expected_effect: Some(nudge.expected_effect.clone()),
                queue_path: queue_path.clone(),
                result_path: result_path.clone(),
                evidence_json: serde_json::to_string(&nudge.evidence)?,
                review_after: review_after.clone(),
                expires_at: expires_at.clone(),
            },
        )?;
    }

    for nudge in &recommend_report.suppressed_nudges {
        let status = if matches!(
            nudge.suppression_reason.as_deref(),
            Some("continuity_preempts_strategy")
        ) {
            crate::core::tracking::ApprovalJobStatus::Deferred
        } else {
            crate::core::tracking::ApprovalJobStatus::Suppressed
        };
        let job_id = build_intervention_job_id(
            &runtime.scope_id,
            &files.local_date,
            &nudge.item_kind,
            nudge.item_id.as_deref(),
            &nudge.task,
        );
        tracker.upsert_approval_job_for_project(
            &project_path,
            &crate::core::tracking::ApprovalJobInput {
                job_id: job_id.clone(),
                scope: "project".to_string(),
                scope_target: Some(project_path.clone()),
                local_date: files.local_date.clone(),
                item_id: nudge.item_id.clone(),
                item_kind: nudge.item_kind.clone(),
                title: nudge.task.clone(),
                summary: nudge
                    .suppression_reason
                    .clone()
                    .unwrap_or_else(|| nudge.why_now.clone()),
                status: preserve_approval_job_status(existing_statuses.remove(&job_id), status),
                source_kind: "strategy-suppressed-nudge".to_string(),
                provider: Some(runtime.provider.as_str().to_string()),
                continuity_active: recommend_report.continuity.active,
                expected_effect: Some(nudge.expected_effect.clone()),
                queue_path: None,
                result_path: result_path.clone(),
                evidence_json: serde_json::to_string(&nudge.evidence)?,
                review_after: review_after.clone(),
                expires_at: expires_at.clone(),
            },
        )?;
        if status == crate::core::tracking::ApprovalJobStatus::Suppressed
            && nudge.item_kind == "friction-fix"
        {
            tracker.suppress_related_pending_friction_jobs(
                nudge.item_id.as_deref(),
                result_path.as_deref(),
                nudge
                    .suppression_reason
                    .as_deref()
                    .unwrap_or("friction no longer actionable"),
            )?;
        }
    }

    Ok(())
}

fn add_friction_nudges(
    tracker: &Tracker,
    runtime: &RuntimeContext,
    report: &mut StrategyRecommendReport,
) -> Result<()> {
    let runtime_project_path = runtime.project_path.display().to_string();
    let friction = tracker.get_memory_os_friction_report(
        MemoryOsInspectionScope::User,
        Some(runtime_project_path.as_str()),
    )?;
    let mut fixed_friction_nudges = friction
        .top_fixes
        .iter()
        .filter(|fix| matches!(fix.status.as_str(), "codified" | "fixed" | "retired"))
        .map(friction_fix_to_nudge)
        .map(|nudge| with_suppression_reason(nudge, "watching_for_recurrence"))
        .collect::<Vec<_>>();
    let mut friction_nudges = friction
        .top_fixes
        .iter()
        .filter(|fix| !matches!(fix.status.as_str(), "codified" | "fixed" | "retired"))
        .take(MAX_FRICTION_NUDGES)
        .map(friction_fix_to_nudge)
        .collect::<Vec<_>>();
    report.suppressed_nudges.append(&mut fixed_friction_nudges);
    report
        .nudge_tasks
        .extend(friction_nudges.iter().map(|nudge| nudge.task.clone()));
    report.nudges.append(&mut friction_nudges);
    suppress_completed_friction_nudges(tracker, runtime, report)?;
    report.nudges.sort_by(|left, right| {
        proactivity_nudge_rank(right)
            .cmp(&proactivity_nudge_rank(left))
            .then(left.task.cmp(&right.task))
    });
    Ok(())
}

fn suppress_completed_friction_nudges(
    tracker: &Tracker,
    _runtime: &RuntimeContext,
    report: &mut StrategyRecommendReport,
) -> Result<()> {
    let completed = tracker.get_approval_jobs_filtered(
        500,
        None,
        Some(&[crate::core::tracking::ApprovalJobStatus::Completed]),
    )?;
    if completed.is_empty() {
        return Ok(());
    }

    let mut remaining = Vec::with_capacity(report.nudges.len());
    let mut suppressed_tasks = Vec::new();
    for nudge in report.nudges.drain(..) {
        if nudge.item_kind == "friction-fix"
            && completed_friction_fix_still_watching(&nudge, &completed)
        {
            suppressed_tasks.push(nudge.task.clone());
            report
                .suppressed_nudges
                .push(with_suppression_reason(nudge, "watching_for_recurrence"));
        } else {
            remaining.push(nudge);
        }
    }
    if !suppressed_tasks.is_empty() {
        report
            .nudge_tasks
            .retain(|task| !suppressed_tasks.iter().any(|suppressed| suppressed == task));
    }
    report.nudges = remaining;
    Ok(())
}

fn completed_friction_fix_still_watching(
    nudge: &StrategicNudge,
    completed: &[crate::core::tracking::ApprovalJobRecord],
) -> bool {
    completed.iter().any(|record| {
        record.item_kind == nudge.item_kind
            && crate::core::tracking::friction_fix_item_ids_related(
                record.item_id.as_deref(),
                nudge.item_id.as_deref(),
            )
            && !friction_signal_is_newer_than_completion(nudge.last_signal_at.as_deref(), record)
    })
}

fn friction_signal_is_newer_than_completion(
    last_signal_at: Option<&str>,
    record: &crate::core::tracking::ApprovalJobRecord,
) -> bool {
    let Some(last_signal_at) = last_signal_at else {
        return false;
    };
    let Ok(signal_at) =
        DateTime::parse_from_rfc3339(last_signal_at).map(|value| value.with_timezone(&Utc))
    else {
        return false;
    };
    let completed_at = record
        .last_reviewed_at
        .as_deref()
        .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
        .map(|value| value.with_timezone(&Utc))
        .unwrap_or(record.updated_at);
    signal_at > completed_at
}

fn with_suppression_reason(mut nudge: StrategicNudge, reason: &str) -> StrategicNudge {
    nudge.suppression_reason = Some(reason.to_string());
    nudge
}

fn fallback_recommend_report(scope_id: &str, warning: String) -> StrategyRecommendReport {
    StrategyRecommendReport {
        generated_at: Utc::now().to_rfc3339(),
        scope_id: scope_id.to_string(),
        continuity: strategy::StrategyContinuitySnapshot {
            active: false,
            summary: None,
        },
        nudge_tasks: Vec::new(),
        continuity_tasks: Vec::new(),
        nudges: Vec::new(),
        suppressed_nudges: Vec::new(),
        warnings: vec![warning],
    }
}

fn friction_fix_to_nudge(fix: &MemoryOsFrictionFix) -> StrategicNudge {
    let interrupt_level = if fix.impact == "high" && fix.status == "active" {
        "interrupt"
    } else {
        "suggest"
    };
    StrategicNudge {
        task: format!("Fix friction: {}", fix.title),
        item_id: Some(fix.fix_id.clone()),
        item_kind: "friction-fix".to_string(),
        supports: Vec::new(),
        why_now: fix.summary.clone(),
        evidence: fix.evidence.clone(),
        last_signal_at: fix.last_signal_at.clone(),
        evidence_freshness: if fix.status == "active" {
            "fresh".to_string()
        } else {
            "known".to_string()
        },
        confidence: if fix.impact == "high" {
            "high".to_string()
        } else {
            "medium".to_string()
        },
        interrupt_level: interrupt_level.to_string(),
        suppression_reason: None,
        expected_effect: format!(
            "Permanently reduce recurring friction: {}",
            fix.permanent_fix
        ),
    }
}

fn proactivity_nudge_rank(nudge: &StrategicNudge) -> i32 {
    match nudge.interrupt_level.as_str() {
        "interrupt" => 3,
        "suggest" => 2,
        "defer" => 1,
        _ => 0,
    }
}

fn evaluate_decision(
    runtime: &RuntimeContext,
    files: &FileSet,
    strategy_bootstrap_requested: bool,
    recommend_report: &StrategyRecommendReport,
    completed_index: &ProactivityCompletedIndex,
) -> Result<ProactivityDecisionArtifact> {
    let mut reasons = Vec::new();
    let outcome = if files.claim_path.exists() {
        reasons.push("morning_session_active".to_string());
        ProactivityDecisionOutcome::Suppressed
    } else if files.pending_path.exists() {
        reasons.push("morning_job_already_queued".to_string());
        ProactivityDecisionOutcome::Suppressed
    } else if already_spawned_today(runtime, files, completed_index)? {
        reasons.push("same_day_spawn_limit_reached".to_string());
        ProactivityDecisionOutcome::Suppressed
    } else if !interactive_desktop_available() {
        reasons.push("desktop_session_unavailable".to_string());
        ProactivityDecisionOutcome::Suppressed
    } else if strategy_bootstrap_requested && recommend_report.nudges.is_empty() {
        reasons.push("bootstrap_strategy_scope".to_string());
        ProactivityDecisionOutcome::Deferred
    } else if recommend_report.continuity.active
        && !recommend_report
            .nudges
            .iter()
            .any(|nudge| nudge.interrupt_level == "interrupt")
    {
        reasons.push("continuity_preempts_strategy".to_string());
        ProactivityDecisionOutcome::Deferred
    } else if recommend_report.nudges.is_empty() {
        reasons.push("no_strategic_nudges_above_threshold".to_string());
        ProactivityDecisionOutcome::Suppressed
    } else {
        reasons.push("approval_queue_ready".to_string());
        ProactivityDecisionOutcome::QueueApproval
    };

    Ok(ProactivityDecisionArtifact {
        schema_version: "munin-proactivity-decision-v1".to_string(),
        generated_at: Utc::now().to_rfc3339(),
        job_id: files.job_id.clone(),
        scope_id: runtime.scope_id.clone(),
        local_date: files.local_date.clone(),
        provider: runtime.provider,
        outcome,
        reasons,
        continuity_active: recommend_report.continuity.active,
        brief_path: files.brief_path.display().to_string(),
        queue_path: None,
        result_path: Some(files.result_path.display().to_string()),
        nudge_tasks: recommend_report.nudge_tasks.clone(),
    })
}

fn build_job(
    runtime: &RuntimeContext,
    files: &FileSet,
    recommend_report: &StrategyRecommendReport,
) -> Result<ProactivityJob> {
    Ok(ProactivityJob {
        schema_version: "munin-proactivity-v1".to_string(),
        job_type: "morning-proactivity".to_string(),
        job_id: files.job_id.clone(),
        scope_id: runtime.scope_id.clone(),
        local_date: files.local_date.clone(),
        created_at: Utc::now().to_rfc3339(),
        provider: runtime.provider,
        project_path: runtime.project_path.display().to_string(),
        session_name: build_session_name(runtime.provider, &runtime.scope_id),
        prompt_token: MORNING_PROMPT_TOKEN.to_string(),
        brief_path: files.brief_path.display().to_string(),
        launch_instructions_path: files.launch_instructions_path.display().to_string(),
        decision_path: files.decision_path.display().to_string(),
        result_path: files.result_path.display().to_string(),
        continuity_active: recommend_report.continuity.active,
        nudge_tasks: recommend_report.nudge_tasks.clone(),
        intervention_job_ids: recommend_report
            .nudges
            .iter()
            .map(|nudge| {
                build_intervention_job_id(
                    &runtime.scope_id,
                    &files.local_date,
                    &nudge.item_kind,
                    nudge.item_id.as_deref(),
                    &nudge.task,
                )
            })
            .collect(),
    })
}

fn build_brief(
    runtime: &RuntimeContext,
    files: &FileSet,
    decision: &ProactivityDecisionArtifact,
    recommend_report: &StrategyRecommendReport,
) -> Result<String> {
    let intervention_prompt = render_morning_intervention_prompt(recommend_report);
    let resume_prompt = render_resume_prompt()?;
    let strategy_prompt = render_strategy_prompt(recommend_report)?;
    Ok(format!(
        "# Munin Morning Brief\n\n- Scope: {}\n- Local date: {}\n- Provider: {}\n- Decision: {}\n- Queue path: {}\n- Result path: {}\n- Continuity active: {}\n\n{}\n\n{}\n\n{}\n",
        runtime.scope_id,
        files.local_date,
        runtime.provider.as_str(),
        decision.outcome.as_str(),
        files.pending_path.display(),
        files.result_path.display(),
        recommend_report.continuity.active,
        intervention_prompt,
        resume_prompt,
        strategy_prompt
    ))
}

fn build_launch_instructions(runtime: &RuntimeContext, job: &ProactivityJob) -> Result<String> {
    Ok(format!(
        "# {token}\n\nYou are the morning proactivity session for scope `{scope}`.\n\nThe daemon already claimed this job for you.\n\n1. Read the morning brief at:\n   `{brief_path}`\n2. Start with the top `friction-fix` in the brief when one exists. Your job is to permanently reduce that friction, not merely describe it.\n3. Keep working until the fix is implemented, verified, or blocked by a concrete recorded blocker.\n4. When you finish, record the result:\n   `munin proactivity complete --job-id \"{job_id}\" --status complete --summary \"<one sentence summary>\"`\n5. If you cannot continue, record failure:\n   `munin proactivity complete --job-id \"{job_id}\" --status failed --summary \"<short failure summary>\" --error \"<concrete error>\"`\n6. Do not delete queue or result files manually.\n\nClaim path: `{claim_path}`\nDecision path: `{decision_path}`\nResult path: `{result_path}`\nProject path: `{project_path}`\nProvider: `{provider}`\n",
        token = MORNING_PROMPT_TOKEN,
        scope = job.scope_id,
        job_id = job.job_id,
        brief_path = job.brief_path,
        claim_path = runtime
            .paths
            .queue_dir
            .join(format!("{}.processing.json", job.job_id))
            .display(),
        decision_path = job.decision_path,
        result_path = job.result_path,
        project_path = job.project_path,
        provider = runtime.provider.as_str(),
    ))
}

fn render_resume_prompt() -> Result<String> {
    let tracker = Tracker::new().context("Failed to initialize tracking database")?;
    let overview = tracker
        .get_memory_os_overview_report(MemoryOsInspectionScope::User, None)
        .context("Failed to compile Memory OS overview for proactivity")?;
    Ok(render_resume_prompt_from_overview(&overview))
}

fn render_resume_prompt_from_overview(overview: &MemoryOsOverviewReport) -> String {
    let mut buffer = String::new();
    let _ = writeln!(
        buffer,
        "<startup_memory_brief scope=\"user\" generated_at=\"{}\">",
        overview.generated_at
    );
    let _ = writeln!(buffer, "<what_i_know>");
    let _ = writeln!(
        buffer,
        "- Memory OS is ready; this startup brief is compiled from user prose, strategy, and friction signals."
    );
    for project in proactivity_project_focus(overview).iter().take(4) {
        let _ = writeln!(buffer, "- Recent project focus: {}", project.repo_label);
    }
    let _ = writeln!(buffer, "</what_i_know>");
    let _ = writeln!(buffer, "<what_is_active>");
    for finding in proactivity_safe_findings(&overview.active_work)
        .iter()
        .take(4)
    {
        let _ = writeln!(buffer, "- {}: {}", finding.title, finding.summary);
        for evidence in finding
            .evidence
            .iter()
            .filter(|evidence| !proactivity_text_has_command_noise(evidence))
            .take(2)
        {
            let _ = writeln!(buffer, "  evidence: {evidence}");
        }
    }
    if proactivity_safe_findings(&overview.active_work).is_empty() {
        let _ = writeln!(
            buffer,
            "- No active Memory OS work item is currently compiled."
        );
    }
    let _ = writeln!(buffer, "</what_is_active>");
    let _ = writeln!(buffer, "<watchouts>");
    for correction in proactivity_safe_corrections(&overview.top_correction_patterns)
        .iter()
        .take(4)
    {
        let _ = writeln!(
            buffer,
            "- Repeated operational correction [{}] x{}; exact commands stay in inspect/json evidence.",
            correction.error_kind, correction.count
        );
    }
    if proactivity_safe_corrections(&overview.top_correction_patterns).is_empty() {
        let _ = writeln!(
            buffer,
            "- No repeated correction pattern is currently compiled."
        );
    }
    let _ = writeln!(buffer, "</watchouts>");
    let _ = writeln!(buffer, "<startup_rules>");
    let _ = writeln!(
        buffer,
        "- Use this compiled Memory OS brief before raw transcript search."
    );
    let _ = writeln!(
        buffer,
        "- Prefer the morning intervention and strategy nudges below."
    );
    for rule in proactivity_command_friction_rules(&overview.top_correction_patterns) {
        let _ = writeln!(buffer, "- {rule}");
    }
    let _ = writeln!(buffer, "</startup_rules>");
    let _ = writeln!(buffer, "</startup_memory_brief>");
    buffer
}

fn proactivity_project_focus(overview: &MemoryOsOverviewReport) -> Vec<&MemoryOsProjectSummary> {
    overview
        .top_projects
        .iter()
        .filter(|project| {
            !matches!(project.repo_label.as_str(), "workspace-root" | "home-root")
                && !proactivity_text_has_command_noise(&project.repo_label)
        })
        .collect()
}

fn proactivity_safe_findings(
    findings: &[MemoryOsNarrativeFinding],
) -> Vec<&MemoryOsNarrativeFinding> {
    findings
        .iter()
        .filter(|finding| {
            !proactivity_text_has_command_noise(&finding.title)
                && !proactivity_text_has_command_noise(&finding.summary)
        })
        .collect()
}

fn proactivity_safe_corrections(
    corrections: &[MemoryOsCorrectionPatternSummary],
) -> Vec<&MemoryOsCorrectionPatternSummary> {
    corrections
        .iter()
        .filter(|correction| {
            !correction.error_kind.trim().is_empty()
                && !proactivity_text_has_command_noise(&correction.error_kind)
        })
        .collect()
}

fn proactivity_command_friction_rules(
    corrections: &[MemoryOsCorrectionPatternSummary],
) -> Vec<&'static str> {
    let mut labels = Vec::new();
    for correction in proactivity_safe_corrections(corrections).iter().take(12) {
        let label = proactivity_correction_label(correction);
        if !labels.contains(&label) {
            labels.push(label);
        }
    }
    labels
        .into_iter()
        .filter_map(proactivity_rule_for_correction_label)
        .take(4)
        .collect()
}

fn proactivity_correction_label(correction: &MemoryOsCorrectionPatternSummary) -> &'static str {
    let lowered = correction.error_kind.to_ascii_lowercase();
    if lowered.contains("flag") || correction.wrong_command.contains("--") {
        "CLI syntax drift"
    } else if lowered.contains("path")
        || lowered.contains("file")
        || correction.wrong_command.contains('\\')
        || correction.wrong_command.contains('/')
    {
        "Path assumption drift"
    } else if lowered.contains("command") || lowered.contains("tool") {
        "Tool availability drift"
    } else {
        "Execution assumption drift"
    }
}

fn proactivity_rule_for_correction_label(label: &str) -> Option<&'static str> {
    match label {
        "CLI syntax drift" => Some(
            "Command guardrail: before running abbreviated CLI syntax, use an exact known command template or check the tool help.",
        ),
        "Path assumption drift" => Some(
            "Path guardrail: resolve the project root and exact script/test path before running file-sensitive commands.",
        ),
        "Tool availability drift" => Some(
            "Tool guardrail: probe command availability with a cheap version/help check before relying on it.",
        ),
        "Execution assumption drift" => Some(
            "Execution guardrail: run a cheap read-only probe before assuming the execution path is valid.",
        ),
        _ => None,
    }
}

fn proactivity_text_has_command_noise(text: &str) -> bool {
    let lowered = text.trim().to_ascii_lowercase();
    if lowered.is_empty() {
        return false;
    }

    let command_starts = [
        "cd ",
        "git ",
        "context ",
        "context proxy ",
        "powershell",
        "pwsh",
        "cmd ",
        "node ",
        "cargo ",
        "npm ",
        "npx ",
        "python ",
        "python3 ",
        ".\\",
        "./",
        "run /",
    ];
    if command_starts
        .iter()
        .any(|prefix| lowered.starts_with(prefix))
    {
        return true;
    }

    let command_markers = [
        "&&",
        "||",
        "get-childitem",
        "select-string",
        ".ps1",
        ".exe",
        ".cmd",
        ".omx",
        ".omx2",
        ".codex-state",
        "inbox.md",
        "worker-",
        "launch-detached",
        "origin/",
        "context proxy",
        "shell executions",
        " shells",
        "sessions,",
        "[omx_tmux_inject]",
        "execute your assignment",
        "report concrete status",
        "report status + evidence",
        "status.json",
        "<task>",
        "<run_id>",
        "<deliverable>",
    ];
    command_markers
        .iter()
        .any(|needle| lowered.contains(needle))
}

fn render_morning_intervention_prompt(report: &StrategyRecommendReport) -> String {
    let friction_nudges = report
        .nudges
        .iter()
        .filter(|nudge| nudge.item_kind == "friction-fix")
        .collect::<Vec<_>>();
    if friction_nudges.is_empty() {
        let tasks = report
            .nudges
            .iter()
            .take(3)
            .map(|nudge| format!("- {}: {}", nudge.task, nudge.why_now))
            .collect::<Vec<_>>();
        if tasks.is_empty() {
            return "<morning_intervention>\n- No actionable nudges are currently above threshold.\n</morning_intervention>".to_string();
        }
        return format!(
            "<morning_intervention>\n<priority>strategy</priority>\n{}\n</morning_intervention>",
            tasks.join("\n")
        );
    }

    let top = friction_nudges[0];
    let mut lines = vec![
        "<morning_intervention>".to_string(),
        "<priority>friction-fix</priority>".to_string(),
        format!("- Task: {}", top.task),
        format!("- Why now: {}", top.why_now),
        format!("- Permanent fix: {}", top.expected_effect),
        format!(
            "- Completion bar: implement and verify the permanent fix, or record the concrete blocker with `munin proactivity complete --job-id \"<job-id>\" --status failed --summary \"<summary>\" --error \"<blocker>\"`."
        ),
    ];
    for evidence in top.evidence.iter().take(3) {
        lines.push(format!("- Evidence: {}", evidence));
    }
    if friction_nudges.len() > 1 {
        lines.push("- Also queued friction fixes:".to_string());
        for nudge in friction_nudges.iter().skip(1).take(2) {
            lines.push(format!("  - {}: {}", nudge.task, nudge.why_now));
        }
    }
    lines.push("</morning_intervention>".to_string());
    lines.join("\n")
}

fn render_strategy_prompt(report: &StrategyRecommendReport) -> Result<String> {
    Ok(format!(
        "<strategy_report format=\"prompt\">\n{}\n</strategy_report>",
        serde_json::to_string_pretty(report)?
    ))
}

fn already_spawned_today(
    runtime: &RuntimeContext,
    files: &FileSet,
    completed_index: &ProactivityCompletedIndex,
) -> Result<bool> {
    if files.result_path.exists() {
        let result = load_result(&files.result_path)?;
        return Ok(matches!(
            result.status,
            ProactivityTerminalStatus::Complete | ProactivityTerminalStatus::Failed
        ));
    }
    let count = completed_index
        .records
        .iter()
        .filter(|record| {
            record.scope_id == runtime.scope_id
                && record.local_date == files.local_date
                && matches!(
                    record.status,
                    ProactivityTerminalStatus::Complete | ProactivityTerminalStatus::Failed
                )
        })
        .count();
    Ok(count >= runtime.max_spawns_per_day as usize)
}

fn sweep_internal(runtime: &RuntimeContext) -> Result<ProactivitySweepReport> {
    let now = Utc::now();
    let stale_age = Duration::minutes(runtime.stale_claim_minutes as i64);
    let mut released_stale_pending = 0usize;
    let mut released_stale_claims = 0usize;
    let mut finalized_results = 0usize;
    let completed_path = runtime.paths.state_dir.join(COMPLETED_FILE);

    for entry in fs::read_dir(&runtime.paths.results_dir)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() || path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        if file_name.ends_with(".decision.json") {
            continue;
        }
        let result = load_result(&path)?;
        if update_completed_index(&completed_path, &result, &path)? {
            finalized_results += 1;
        }
        let pending = runtime
            .paths
            .queue_dir
            .join(format!("{}.json", result.job_id));
        let claim = runtime
            .paths
            .queue_dir
            .join(format!("{}.processing.json", result.job_id));
        if pending.exists() {
            let _ = fs::remove_file(&pending);
        }
        if claim.exists() {
            let _ = fs::remove_file(&claim);
        }
    }

    for entry in fs::read_dir(&runtime.paths.queue_dir)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() || path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let metadata = fs::metadata(&path)?;
        let modified = metadata.modified().ok().map(DateTime::<Utc>::from);
        let is_stale = modified
            .map(|modified_at| now - modified_at > stale_age)
            .unwrap_or(false);
        if !is_stale {
            continue;
        }

        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        let job = load_job(&path)?;
        let files = file_set_for_job(runtime, &job.job_id, &job.local_date)?;
        let result = if file_name.ends_with(".processing.json") {
            released_stale_claims += 1;
            build_terminal_result(
                runtime,
                &files,
                ProactivityTerminalStatus::Failed,
                "Morning session timed out before writing a result.",
                Some("stale_claim_timeout".to_string()),
                Vec::new(),
            )
        } else {
            released_stale_pending += 1;
            build_terminal_result(
                runtime,
                &files,
                ProactivityTerminalStatus::Failed,
                "Morning session never claimed the queued job.",
                Some("stale_pending_timeout".to_string()),
                Vec::new(),
            )
        };
        write_json_file(&files.result_path, &result)?;
        update_completed_index(&completed_path, &result, &files.result_path)?;
        let _ = fs::remove_file(&path);
    }

    Ok(ProactivitySweepReport {
        generated_at: Utc::now().to_rfc3339(),
        scope_id: runtime.scope_id.clone(),
        released_stale_pending,
        released_stale_claims,
        finalized_results,
        pending_jobs: count_queue_entries(&runtime.paths.queue_dir, false)?,
        claimed_jobs: count_queue_entries(&runtime.paths.queue_dir, true)?,
        result_files: count_result_files(&runtime.paths.results_dir)?,
    })
}

fn build_terminal_result(
    runtime: &RuntimeContext,
    files: &FileSet,
    status: ProactivityTerminalStatus,
    summary: &str,
    error: Option<String>,
    notes: Vec<String>,
) -> ProactivityResultArtifact {
    ProactivityResultArtifact {
        schema_version: "munin-proactivity-result-v1".to_string(),
        recorded_at: Utc::now().to_rfc3339(),
        job_id: files.job_id.clone(),
        scope_id: runtime.scope_id.clone(),
        local_date: files.local_date.clone(),
        provider: runtime.provider,
        status,
        summary: summary.to_string(),
        error,
        notes,
    }
}

fn load_completed_index(path: &Path) -> Result<ProactivityCompletedIndex> {
    if !path.exists() {
        return Ok(ProactivityCompletedIndex {
            schema_version: "munin-proactivity-completed-v1".to_string(),
            records: Vec::new(),
        });
    }
    let content = fs::read_to_string(path)?;
    let mut index: ProactivityCompletedIndex = serde_json::from_str(&content)?;
    if index.schema_version.is_empty() {
        index.schema_version = "munin-proactivity-completed-v1".to_string();
    }
    Ok(index)
}

fn update_completed_index(
    path: &Path,
    result: &ProactivityResultArtifact,
    result_path: &Path,
) -> Result<bool> {
    let mut index = load_completed_index(path)?;
    if index
        .records
        .iter()
        .any(|record| record.job_id == result.job_id)
    {
        return Ok(false);
    }
    index.records.push(ProactivityCompletedRecord {
        job_id: result.job_id.clone(),
        scope_id: result.scope_id.clone(),
        local_date: result.local_date.clone(),
        provider: result.provider,
        status: result.status,
        recorded_at: result.recorded_at.clone(),
        result_path: result_path.display().to_string(),
    });
    write_json_file(path, &index)?;
    Ok(true)
}

fn load_job(path: &Path) -> Result<ProactivityJob> {
    let content = fs::read_to_string(path)?;
    serde_json::from_str(&content).context("Failed to parse proactivity job")
}

fn parse_provider_text(value: Option<&str>) -> ProactivityProvider {
    match value
        .unwrap_or("claude")
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "claude" => ProactivityProvider::Claude,
        "codex" => ProactivityProvider::Codex,
        _ => ProactivityProvider::Claude,
    }
}

fn reconstruct_job_from_approval_record(
    paths: &ProactivityPaths,
    record: &crate::core::tracking::ApprovalJobRecord,
    related_jobs: &[crate::core::tracking::ApprovalJobRecord],
) -> Result<Option<ProactivityJob>> {
    if record.item_kind != "morning-proactivity" {
        return Ok(None);
    }

    let provider = parse_provider_text(record.provider.as_deref());
    let runtime = RuntimeContext {
        scope_id: record
            .item_id
            .clone()
            .unwrap_or_else(|| record.job_id.clone()),
        provider,
        project_path: PathBuf::from(&record.project_path),
        schedule_local: "08:00".to_string(),
        max_spawns_per_day: 1,
        stale_claim_minutes: 90,
        paths: paths.clone(),
        config: Config::default(),
    };
    let files = file_set_for_job(&runtime, &record.job_id, &record.local_date)?;
    let intervention_job_ids = related_jobs
        .iter()
        .into_iter()
        .filter(|job| {
            job.job_id != record.job_id
                && job.local_date == record.local_date
                && job.source_kind.starts_with("strategy-")
        })
        .map(|job| job.job_id.clone())
        .collect::<Vec<_>>();
    let nudge_tasks = serde_json::from_str::<Vec<String>>(&record.evidence_json)
        .ok()
        .filter(|tasks| !tasks.is_empty())
        .unwrap_or_else(|| {
            intervention_job_ids
                .iter()
                .map(std::string::ToString::to_string)
                .collect()
        });
    let job = ProactivityJob {
        schema_version: "munin-proactivity-v1".to_string(),
        job_type: "morning-proactivity".to_string(),
        job_id: record.job_id.clone(),
        scope_id: record
            .item_id
            .clone()
            .unwrap_or_else(|| runtime.scope_id.clone()),
        local_date: record.local_date.clone(),
        created_at: record.created_at.to_rfc3339(),
        provider,
        project_path: record.project_path.clone(),
        session_name: build_session_name(provider, &runtime.scope_id),
        prompt_token: MORNING_PROMPT_TOKEN.to_string(),
        brief_path: files.brief_path.display().to_string(),
        launch_instructions_path: files.launch_instructions_path.display().to_string(),
        decision_path: files.decision_path.display().to_string(),
        result_path: record
            .result_path
            .clone()
            .unwrap_or_else(|| files.result_path.display().to_string()),
        continuity_active: record.continuity_active,
        nudge_tasks,
        intervention_job_ids,
    };

    if !files.brief_path.exists() {
        let brief = format!(
            "# Munin Morning Brief\n\nThis brief was reconstructed from durable approval state.\n\n- Scope: {}\n- Local date: {}\n- Provider: {}\n- Project path: {}\n- Nudges: {}\n- Result path: {}\n",
            job.scope_id,
            job.local_date,
            job.provider.as_str(),
            job.project_path,
            if job.nudge_tasks.is_empty() {
                "none".to_string()
            } else {
                job.nudge_tasks.join(", ")
            },
            job.result_path
        );
        write_text_file(&files.brief_path, &brief)?;
    }
    if !files.launch_instructions_path.exists() {
        let launch_instructions = build_launch_instructions(&runtime, &job)?;
        write_text_file(&files.launch_instructions_path, &launch_instructions)?;
    }
    if !files.decision_path.exists() {
        let outcome = match record.status {
            crate::core::tracking::ApprovalJobStatus::Queued
            | crate::core::tracking::ApprovalJobStatus::Approved => {
                ProactivityDecisionOutcome::QueueApproval
            }
            crate::core::tracking::ApprovalJobStatus::Deferred => {
                ProactivityDecisionOutcome::Deferred
            }
            crate::core::tracking::ApprovalJobStatus::Suppressed
            | crate::core::tracking::ApprovalJobStatus::Rejected => {
                ProactivityDecisionOutcome::Suppressed
            }
            crate::core::tracking::ApprovalJobStatus::Completed
            | crate::core::tracking::ApprovalJobStatus::Failed => ProactivityDecisionOutcome::Error,
        };
        let decision = ProactivityDecisionArtifact {
            schema_version: "munin-proactivity-decision-v1".to_string(),
            generated_at: record.updated_at.to_rfc3339(),
            job_id: record.job_id.clone(),
            scope_id: job.scope_id.clone(),
            local_date: job.local_date.clone(),
            provider,
            outcome,
            reasons: vec!["reconstructed_from_durable_approval".to_string()],
            continuity_active: record.continuity_active,
            brief_path: job.brief_path.clone(),
            queue_path: record.queue_path.clone(),
            result_path: Some(job.result_path.clone()),
            nudge_tasks: job.nudge_tasks.clone(),
        };
        write_json_file(&files.decision_path, &decision)?;
    }

    let projection_path = match record.status {
        crate::core::tracking::ApprovalJobStatus::Queued => Some(files.pending_path.clone()),
        crate::core::tracking::ApprovalJobStatus::Approved => Some(files.claim_path.clone()),
        _ => None,
    };
    if let Some(path) = projection_path {
        if !path.exists() {
            write_json_file(&path, &job)?;
        }
    }

    Ok(Some(job))
}

fn ensure_job_projection(paths: &ProactivityPaths, job_id: &str) -> Result<Option<ProactivityJob>> {
    let (pending_path, claim_path, _) = locate_job_paths(paths, job_id)?;
    if pending_path.exists() || claim_path.exists() {
        return Ok(None);
    }
    let tracker = Tracker::new().context("Failed to initialize tracking database")?;
    let Some(record) = tracker.get_approval_job(job_id)? else {
        return Ok(None);
    };
    let related_jobs = tracker.get_approval_jobs_filtered(200, Some(&record.project_path), None)?;
    let reconstructed = reconstruct_job_from_approval_record(paths, &record, &related_jobs)?;
    if let Some(job) = reconstructed.as_ref() {
        let queue_path_text = if paths
            .queue_dir
            .join(format!("{}.processing.json", job.job_id))
            .exists()
        {
            Some(
                paths
                    .queue_dir
                    .join(format!("{}.processing.json", job.job_id))
                    .display()
                    .to_string(),
            )
        } else if paths
            .queue_dir
            .join(format!("{}.json", job.job_id))
            .exists()
        {
            Some(
                paths
                    .queue_dir
                    .join(format!("{}.json", job.job_id))
                    .display()
                    .to_string(),
            )
        } else {
            None
        };
        let result_path_text = Path::new(&job.result_path)
            .exists()
            .then(|| job.result_path.as_str());
        let _ = tracker.set_approval_job_status(
            &record.job_id,
            record.status,
            queue_path_text.as_deref(),
            result_path_text,
            None,
        );
    }
    Ok(reconstructed)
}

fn load_job_for_completion(paths: &ProactivityPaths, job_id: &str) -> Result<ProactivityJob> {
    let pending = paths.queue_dir.join(format!("{job_id}.json"));
    if pending.exists() {
        return load_job(&pending);
    }
    let claim = paths.queue_dir.join(format!("{job_id}.processing.json"));
    if claim.exists() {
        return load_job(&claim);
    }
    if let Some(job) = ensure_job_projection(paths, job_id)? {
        return Ok(job);
    }
    Err(anyhow!("No proactivity job exists for `{job_id}`"))
}

fn load_result(path: &Path) -> Result<ProactivityResultArtifact> {
    let content = fs::read_to_string(path)?;
    serde_json::from_str(&content).context("Failed to parse proactivity result")
}

fn locate_job_paths(paths: &ProactivityPaths, job_id: &str) -> Result<(PathBuf, PathBuf, PathBuf)> {
    validate_job_id_file_stem(job_id)?;
    Ok((
        paths.queue_dir.join(format!("{job_id}.json")),
        paths.queue_dir.join(format!("{job_id}.processing.json")),
        paths.results_dir.join(format!("{job_id}.json")),
    ))
}

fn count_queue_entries(queue_dir: &Path, claimed: bool) -> Result<usize> {
    let mut count = 0usize;
    for entry in fs::read_dir(queue_dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if claimed {
            if name.ends_with(".processing.json") {
                count += 1;
            }
        } else if name.ends_with(".json") && !name.ends_with(".processing.json") {
            count += 1;
        }
    }
    Ok(count)
}

fn count_result_files(results_dir: &Path) -> Result<usize> {
    let mut count = 0usize;
    for entry in fs::read_dir(results_dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.ends_with(".json") && !name.ends_with(".decision.json") {
            count += 1;
        }
    }
    Ok(count)
}

fn write_json_file<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, serde_json::to_string_pretty(value)?)
        .with_context(|| format!("Failed to write {}", path.display()))
}

fn write_text_file(path: &Path, value: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, value).with_context(|| format!("Failed to write {}", path.display()))
}

fn write_heartbeat(runtime: &RuntimeContext) -> Result<()> {
    let files = file_set(runtime)?;
    fs::write(files.heartbeat_path, Utc::now().to_rfc3339())
        .context("Failed to write proactivity heartbeat")
}

fn interactive_desktop_available() -> bool {
    if !cfg!(windows) {
        return true;
    }
    if std::env::var("USERNAME")
        .ok()
        .map(|value| value.eq_ignore_ascii_case("SYSTEM"))
        .unwrap_or(false)
    {
        return false;
    }

    Command::new("tasklist")
        .args(["/fi", "imagename eq explorer.exe", "/nh", "/fo", "csv"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| {
            String::from_utf8_lossy(&output.stdout)
                .to_ascii_lowercase()
                .contains("explorer.exe")
        })
        .unwrap_or(false)
}

fn build_launch_command(
    runtime: &RuntimeContext,
    job: &ProactivityJob,
    launch_instructions_path: &Path,
) -> Result<LaunchCommand> {
    let leading_task = job
        .nudge_tasks
        .iter()
        .find(|task| task.starts_with("Fix friction:"))
        .or_else(|| job.nudge_tasks.first())
        .map(|task| {
            format!(
                " Start with this intervention: {task}. Work it until implemented, verified, or concretely blocked."
            )
        })
        .unwrap_or_default();
    let prompt = format!(
        "{token}. The daemon already claimed the job. Read the brief at '{brief_path}'.{leading_task} When you finish, run: munin proactivity complete --job-id '{job_id}' --status complete --summary '<one sentence summary>'. On failure run: munin proactivity complete --job-id '{job_id}' --status failed --summary '<short failure summary>' --error '<concrete error>'. Full instructions are also at '{launch_path}'.",
        token = MORNING_PROMPT_TOKEN,
        job_id = job.job_id,
        brief_path = job.brief_path,
        launch_path = launch_instructions_path.display(),
    );

    match runtime.provider {
        ProactivityProvider::Claude => build_provider_launch_command(
            "Munin Morning Claude",
            &runtime.project_path,
            "claude",
            &[
                "--dangerously-skip-permissions",
                "--model",
                "claude-opus-4-6",
                &prompt,
            ],
            &[],
        ),
        ProactivityProvider::Codex => build_provider_launch_command(
            &job.session_name,
            &runtime.project_path,
            "codex",
            &["--dangerously-bypass-approvals-and-sandbox", &prompt],
            &[],
        ),
    }
}

fn build_provider_launch_command(
    title: &str,
    cwd: &Path,
    binary_name: &str,
    args: &[&str],
    cleared_env: &[&str],
) -> Result<LaunchCommand> {
    let command_path = resolve_binary(binary_name)?;
    let mut segments = Vec::new();
    for env_key in cleared_env {
        segments.push(format!("set {}=", env_key));
    }
    segments.push(format!(
        "cd /d {}",
        quote_for_cmd(cwd.display().to_string())
    ));
    segments.push(render_windows_command(&command_path, args));
    let inner = segments.join(" && ");
    let launch = format!(
        "$host.ui.RawUI.WindowTitle = {}; Start-Process -FilePath 'cmd.exe' -ArgumentList @('/k', {}) -WorkingDirectory {} -WindowStyle Normal",
        quote_for_powershell(title),
        quote_for_powershell(&inner),
        quote_for_powershell(&cwd.display().to_string())
    );
    Ok(LaunchCommand {
        preview: launch.clone(),
        runner: "powershell.exe".to_string(),
        args: vec![
            "-NoLogo".to_string(),
            "-NoProfile".to_string(),
            "-Command".to_string(),
            launch,
        ],
    })
}

fn run_launch_command(command: &LaunchCommand) -> Result<()> {
    let status = Command::new(&command.runner)
        .args(&command.args)
        .status()
        .context("Failed to launch interactive proactivity session")?;
    if !status.success() {
        anyhow::bail!("Interactive launch command exited with {}", status);
    }
    Ok(())
}

fn render_windows_command(command_path: &Path, args: &[&str]) -> String {
    let extension = command_path
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| value.to_ascii_lowercase())
        .unwrap_or_default();
    let rendered_args = args
        .iter()
        .map(|arg| quote_arg_if_needed(arg))
        .collect::<Vec<_>>()
        .join(" ");
    let rendered_command = quote_for_cmd(command_path.display().to_string());

    if extension == "ps1" {
        format!(
            "powershell.exe -NoLogo -NoProfile -ExecutionPolicy Bypass -File {} {}",
            rendered_command, rendered_args
        )
    } else if extension == "cmd" || extension == "bat" {
        format!("call {} {}", rendered_command, rendered_args)
    } else {
        format!("{} {}", rendered_command, rendered_args)
            .trim()
            .to_string()
    }
}

fn quote_arg_if_needed(value: &str) -> String {
    if value.is_empty() {
        return "\"\"".to_string();
    }
    if value.chars().any(|ch| ch.is_whitespace() || ch == '"') {
        quote_for_cmd(value)
    } else {
        value.to_string()
    }
}

fn quote_for_cmd(value: impl AsRef<str>) -> String {
    format!("\"{}\"", value.as_ref().replace('"', "\"\""))
}

fn quote_for_powershell(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

fn build_session_name(provider: ProactivityProvider, scope_id: &str) -> String {
    match provider {
        ProactivityProvider::Claude => format!("munin-morning-{scope_id}-claude"),
        ProactivityProvider::Codex => format!("munin-morning-{scope_id}-codex"),
    }
}

fn morning_task_name(scope_id: &str, provider: ProactivityProvider) -> String {
    format!("Munin-Proactivity-Morning-{scope_id}-{}", provider.as_str())
}

fn modern_morning_task_names(scope_id: &str) -> Vec<String> {
    [ProactivityProvider::Claude, ProactivityProvider::Codex]
        .iter()
        .map(|provider| morning_task_name(scope_id, *provider))
        .collect()
}

fn alternate_morning_task_statuses(
    scope_id: &str,
    current_provider: ProactivityProvider,
) -> Vec<ProactivityScheduleTaskStatus> {
    [ProactivityProvider::Claude, ProactivityProvider::Codex]
        .iter()
        .copied()
        .filter(|provider| *provider != current_provider)
        .map(|provider| {
            let name = morning_task_name(scope_id, provider);
            ProactivityScheduleTaskStatus {
                installed: query_task_installed(&name),
                name,
            }
        })
        .collect()
}

fn sweep_task_name(scope_id: &str) -> String {
    format!("Munin-Proactivity-Sweep-{scope_id}")
}

fn legacy_task_names(scope_id: &str, provider: ProactivityProvider) -> Vec<String> {
    vec![
        format!("Context-Munin-Proactivity-Morning-{scope_id}"),
        format!(
            "Context-Munin-Proactivity-Morning-{scope_id}-{}",
            provider.as_str()
        ),
        format!("Context-Munin-Proactivity-Sweep-{scope_id}"),
    ]
}

fn all_legacy_task_names(scope_id: &str) -> Vec<String> {
    let mut tasks = Vec::new();
    for provider in [ProactivityProvider::Claude, ProactivityProvider::Codex] {
        for name in legacy_task_names(scope_id, provider) {
            if !tasks.contains(&name) {
                tasks.push(name);
            }
        }
    }
    tasks
}

fn removable_task_names(scope_id: &str) -> Vec<String> {
    let mut tasks = modern_morning_task_names(scope_id);
    tasks.push(sweep_task_name(scope_id));
    for name in all_legacy_task_names(scope_id) {
        if !tasks.contains(&name) {
            tasks.push(name);
        }
    }
    tasks
}

fn build_task_action(exe: &Path, args: &[&str]) -> String {
    let rendered_args = args
        .iter()
        .map(|arg| quote_arg_if_needed(arg))
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "{} {}",
        quote_for_cmd(exe.display().to_string()),
        rendered_args
    )
}

#[derive(Debug, Clone)]
enum ScheduleSpec {
    Daily { local_time: String },
    IntervalMinutes(u32),
}

fn install_scheduled_task(
    name: &str,
    exe: &Path,
    args: &[&str],
    schedule: ScheduleSpec,
) -> Result<()> {
    #[cfg(windows)]
    {
        return install_windows_task(name, exe, args, schedule);
    }
    #[cfg(target_os = "macos")]
    {
        return install_macos_launch_agent(name, exe, args, schedule);
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        return install_systemd_user_timer(name, exe, args, schedule);
    }
    #[cfg(not(any(windows, unix)))]
    {
        anyhow::bail!("Scheduled task installation is not supported on this operating system");
    }
}

fn delete_scheduled_task(name: &str) -> Result<()> {
    #[cfg(windows)]
    {
        return delete_windows_task(name);
    }
    #[cfg(target_os = "macos")]
    {
        return delete_macos_launch_agent(name);
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        return delete_systemd_user_timer(name);
    }
    #[cfg(not(any(windows, unix)))]
    {
        anyhow::bail!("Scheduled task removal is not supported on this operating system");
    }
}

fn query_task_installed(name: &str) -> bool {
    #[cfg(windows)]
    {
        return Command::new("schtasks")
            .args(["/Query", "/TN", name])
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false);
    }
    #[cfg(target_os = "macos")]
    {
        return macos_launch_agent_path(name).exists();
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        return systemd_timer_path(name).exists();
    }
    #[allow(unreachable_code)]
    false
}

#[cfg(windows)]
fn install_windows_task(
    name: &str,
    exe: &Path,
    args: &[&str],
    schedule: ScheduleSpec,
) -> Result<()> {
    let action = build_task_action(exe, args);
    let mut task_args = vec![
        "/Create".to_string(),
        "/TN".to_string(),
        name.to_string(),
        "/TR".to_string(),
        action,
    ];
    match schedule {
        ScheduleSpec::Daily { local_time } => task_args.extend([
            "/SC".to_string(),
            "DAILY".to_string(),
            "/ST".to_string(),
            local_time,
            "/IT".to_string(),
            "/F".to_string(),
        ]),
        ScheduleSpec::IntervalMinutes(minutes) => task_args.extend([
            "/SC".to_string(),
            "MINUTE".to_string(),
            "/MO".to_string(),
            minutes.to_string(),
            "/IT".to_string(),
            "/F".to_string(),
        ]),
    }
    let output = Command::new("schtasks")
        .args(task_args.iter().map(String::as_str))
        .output()
        .context("Failed to execute schtasks /Create")?;
    if !output.status.success() {
        anyhow::bail!(
            "schtasks /Create failed for {}: {}",
            name,
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}

#[cfg(windows)]
fn delete_windows_task(name: &str) -> Result<()> {
    let output = Command::new("schtasks")
        .args(["/Delete", "/TN", name, "/F"])
        .output()
        .context("Failed to execute schtasks /Delete")?;
    if !output.status.success() {
        anyhow::bail!(
            "schtasks /Delete failed for {}: {}",
            name,
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn install_macos_launch_agent(
    name: &str,
    exe: &Path,
    args: &[&str],
    schedule: ScheduleSpec,
) -> Result<()> {
    let path = macos_launch_agent_path(name);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create {}", parent.display()))?;
    }
    let label = launchd_label(name);
    fs::write(&path, launchd_plist(&label, exe, args, &schedule))
        .with_context(|| format!("Failed to write {}", path.display()))?;
    let domain = launchd_domain();
    let _ = Command::new("launchctl")
        .args(["bootout", domain.as_str(), path.to_string_lossy().as_ref()])
        .output();
    let output = Command::new("launchctl")
        .args([
            "bootstrap",
            domain.as_str(),
            path.to_string_lossy().as_ref(),
        ])
        .output()
        .context("Failed to execute launchctl bootstrap")?;
    if !output.status.success() {
        anyhow::bail!(
            "launchctl bootstrap failed for {}: {}",
            name,
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let _ = Command::new("launchctl")
        .args(["enable", format!("{domain}/{label}").as_str()])
        .output();
    Ok(())
}

#[cfg(target_os = "macos")]
fn delete_macos_launch_agent(name: &str) -> Result<()> {
    let path = macos_launch_agent_path(name);
    let label = launchd_label(name);
    let domain = launchd_domain();
    let _ = Command::new("launchctl")
        .args(["bootout", domain.as_str(), path.to_string_lossy().as_ref()])
        .output();
    let _ = Command::new("launchctl")
        .args(["disable", format!("{domain}/{label}").as_str()])
        .output();
    if path.exists() {
        fs::remove_file(&path).with_context(|| format!("Failed to remove {}", path.display()))?;
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn macos_launch_agent_path(name: &str) -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Library")
        .join("LaunchAgents")
        .join(format!("{}.plist", launchd_label(name)))
}

#[cfg(target_os = "macos")]
fn launchd_domain() -> String {
    format!("gui/{}", unsafe { libc::getuid() })
}

#[cfg(target_os = "macos")]
fn launchd_label(name: &str) -> String {
    format!("com.munin.{}", scheduler_slug(name))
}

#[cfg(target_os = "macos")]
fn launchd_plist(label: &str, exe: &Path, args: &[&str], schedule: &ScheduleSpec) -> String {
    let mut program_args = String::new();
    program_args.push_str(&format!(
        "        <string>{}</string>\n",
        xml_escape(&exe.display().to_string())
    ));
    for arg in args {
        program_args.push_str(&format!("        <string>{}</string>\n", xml_escape(arg)));
    }
    let schedule_xml = match schedule {
        ScheduleSpec::Daily { local_time } => {
            let (hour, minute) = parse_local_time(local_time).unwrap_or((8, 0));
            format!(
                "    <key>StartCalendarInterval</key>\n    <dict>\n        <key>Hour</key>\n        <integer>{hour}</integer>\n        <key>Minute</key>\n        <integer>{minute}</integer>\n    </dict>\n"
            )
        }
        ScheduleSpec::IntervalMinutes(minutes) => {
            format!(
                "    <key>StartInterval</key>\n    <integer>{}</integer>\n",
                minutes.saturating_mul(60)
            )
        }
    };
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "https://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{}</string>
    <key>ProgramArguments</key>
    <array>
{}    </array>
{}    <key>RunAtLoad</key>
    <false/>
    <key>StandardOutPath</key>
    <string>{}</string>
    <key>StandardErrorPath</key>
    <string>{}</string>
</dict>
</plist>
"#,
        xml_escape(label),
        program_args,
        schedule_xml,
        xml_escape(&scheduler_log_path(label, "out").display().to_string()),
        xml_escape(&scheduler_log_path(label, "err").display().to_string())
    )
}

#[cfg(all(unix, not(target_os = "macos")))]
fn install_systemd_user_timer(
    name: &str,
    exe: &Path,
    args: &[&str],
    schedule: ScheduleSpec,
) -> Result<()> {
    let service = systemd_service_path(name);
    let timer = systemd_timer_path(name);
    if let Some(parent) = service.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create {}", parent.display()))?;
    }
    fs::write(&service, systemd_service(name, exe, args))
        .with_context(|| format!("Failed to write {}", service.display()))?;
    fs::write(&timer, systemd_timer(name, &schedule))
        .with_context(|| format!("Failed to write {}", timer.display()))?;
    let _ = Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .output();
    let output = Command::new("systemctl")
        .args([
            "--user",
            "enable",
            "--now",
            &systemd_unit_name(name, "timer"),
        ])
        .output()
        .context("Failed to execute systemctl --user enable --now")?;
    if !output.status.success() {
        anyhow::bail!(
            "systemctl --user enable failed for {}: {}",
            name,
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}

#[cfg(all(unix, not(target_os = "macos")))]
fn delete_systemd_user_timer(name: &str) -> Result<()> {
    let timer_unit = systemd_unit_name(name, "timer");
    let _ = Command::new("systemctl")
        .args(["--user", "disable", "--now", &timer_unit])
        .output();
    for path in [systemd_timer_path(name), systemd_service_path(name)] {
        if path.exists() {
            fs::remove_file(&path)
                .with_context(|| format!("Failed to remove {}", path.display()))?;
        }
    }
    let _ = Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .output();
    Ok(())
}

#[cfg(all(unix, not(target_os = "macos")))]
fn systemd_user_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".config")
        })
        .join("systemd")
        .join("user")
}

#[cfg(all(unix, not(target_os = "macos")))]
fn systemd_service_path(name: &str) -> PathBuf {
    systemd_user_dir().join(systemd_unit_name(name, "service"))
}

#[cfg(all(unix, not(target_os = "macos")))]
fn systemd_timer_path(name: &str) -> PathBuf {
    systemd_user_dir().join(systemd_unit_name(name, "timer"))
}

#[cfg(all(unix, not(target_os = "macos")))]
fn systemd_unit_name(name: &str, suffix: &str) -> String {
    format!("munin-proactivity-{}.{}", scheduler_slug(name), suffix)
}

#[cfg(all(unix, not(target_os = "macos")))]
fn systemd_service(name: &str, exe: &Path, args: &[&str]) -> String {
    let exec = std::iter::once(exe.display().to_string())
        .chain(args.iter().map(|arg| arg.to_string()))
        .map(|part| systemd_escape_arg(&part))
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "[Unit]\nDescription=Munin proactivity {name}\n\n[Service]\nType=oneshot\nExecStart={exec}\n"
    )
}

#[cfg(all(unix, not(target_os = "macos")))]
fn systemd_timer(name: &str, schedule: &ScheduleSpec) -> String {
    let schedule_line = match schedule {
        ScheduleSpec::Daily { local_time } => format!("OnCalendar=*-*-* {local_time}:00"),
        ScheduleSpec::IntervalMinutes(minutes) => {
            format!("OnUnitActiveSec={}min\nOnBootSec={}min", minutes, minutes)
        }
    };
    format!(
        "[Unit]\nDescription=Munin proactivity timer {name}\n\n[Timer]\n{schedule_line}\nPersistent=true\n\n[Install]\nWantedBy=timers.target\n"
    )
}

#[cfg(all(unix, not(target_os = "macos")))]
fn systemd_escape_arg(value: &str) -> String {
    if value
        .chars()
        .any(|ch| ch.is_whitespace() || ch == '"' || ch == '\'')
    {
        format!("'{}'", value.replace('\'', "'\\''"))
    } else {
        value.to_string()
    }
}

#[cfg(any(target_os = "macos", all(unix, not(target_os = "macos"))))]
fn scheduler_slug(name: &str) -> String {
    name.chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

#[cfg(target_os = "macos")]
fn parse_local_time(value: &str) -> Option<(u32, u32)> {
    let (hour, minute) = value.split_once(':')?;
    Some((hour.parse().ok()?, minute.parse().ok()?))
}

#[cfg(target_os = "macos")]
fn scheduler_log_path(label: &str, stream: &str) -> PathBuf {
    context_data_dir()
        .unwrap_or_else(|_| {
            dirs::data_local_dir()
                .or_else(dirs::data_dir)
                .unwrap_or_else(|| PathBuf::from("."))
                .join("context")
        })
        .join(PROACTIVITY_DIR)
        .join("logs")
        .join(format!("{label}.{stream}.log"))
}

#[cfg(target_os = "macos")]
fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::config::{ProactivityConfig, StrategyConfig};
    use crate::core::memory_os::{MemoryOsImportedSourceSummary, MemoryOsOnboardingState};
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn sample_config() -> Config {
        let mut config = Config::default();
        config.proactivity.enabled = true;
        config.proactivity.default_scope = Some("sitesorted-business".to_string());
        config.proactivity.project_path = Some(PathBuf::from("C:/Users/OEM/Projects/sitesorted"));
        config
    }

    fn sample_memory_os_overview() -> MemoryOsOverviewReport {
        MemoryOsOverviewReport {
            generated_at: "2026-05-04T00:00:00Z".to_string(),
            scope: MemoryOsInspectionScope::User,
            imported_sessions: 3086,
            imported_shell_executions: 40356,
            imported_sources: vec![MemoryOsImportedSourceSummary {
                source: "codex".to_string(),
                sessions: 10,
                shell_executions: 100,
            }],
            top_projects: vec![
                MemoryOsProjectSummary {
                    project_path: "C:/Users/OEM/Projects/sitesorted/watcher-v2".to_string(),
                    repo_label: "watcher-v2".to_string(),
                    sessions: 377,
                    shell_executions: 5370,
                },
                MemoryOsProjectSummary {
                    project_path: "C:/Users/OEM".to_string(),
                    repo_label: "home-root".to_string(),
                    sessions: 100,
                    shell_executions: 200,
                },
            ],
            top_correction_patterns: vec![MemoryOsCorrectionPatternSummary {
                error_kind: "wrong-terminal".to_string(),
                wrong_command: "npm run build && cargo test".to_string(),
                corrected_command: "cargo test".to_string(),
                count: 9,
                successful_replays: 2,
                failed_replays: 1,
                last_observed_at: Some("2026-05-04T00:00:00Z".to_string()),
            }],
            active_work: vec![
                MemoryOsNarrativeFinding {
                    title: "Current work".to_string(),
                    summary: "Fix the Memory OS brief so it surfaces useful user prose before build output."
                        .to_string(),
                    evidence: vec![
                        "user prompt checkpoint at 2026-05-04T00:00:00Z".to_string(),
                        "377 sessions, 5370 shell executions".to_string(),
                    ],
                },
                MemoryOsNarrativeFinding {
                    title: "cd C:/repo && npm run build".to_string(),
                    summary: "cd C:/repo && npm run build".to_string(),
                    evidence: Vec::new(),
                },
            ],
            top_action_memory_candidates: Vec::new(),
            onboarding: MemoryOsOnboardingState {
                schema_version: "test".to_string(),
                status: "completed".to_string(),
                started_at: None,
                completed_at: Some("2026-05-04T00:00:00Z".to_string()),
                sessions_processed: 3086,
                shells_ingested: 40356,
                corrections_ingested: 9,
                imported_sources: Vec::new(),
                checkpoint_count: 10,
                journal_event_count: 20,
            },
            serving_policy: Vec::new(),
        }
    }

    #[test]
    fn resume_prompt_keeps_command_and_build_noise_out_of_startup_memory() {
        let rendered = render_resume_prompt_from_overview(&sample_memory_os_overview());

        assert!(rendered.contains("compiled from user prose, strategy, and friction signals"));
        assert!(rendered.contains("Recent project focus: watcher-v2"));
        assert!(rendered.contains("surfaces useful user prose"));
        assert!(rendered.contains("exact commands stay in inspect/json evidence"));
        assert!(rendered.contains("Execution guardrail"));
        assert!(!rendered.contains("40356"));
        assert!(!rendered.contains("shell executions"));
        assert!(!rendered.contains("5370"));
        assert!(!rendered.contains("npm run build"));
        assert!(!rendered.contains("cargo test"));
        assert!(!rendered.contains("home-root"));
    }

    #[test]
    fn build_job_id_is_scope_and_date_stable() {
        assert_eq!(
            build_job_id(
                "sitesorted-business",
                ProactivityProvider::Claude,
                "2026-04-14"
            ),
            "morning-sitesorted-business-claude-2026-04-14"
        );
        assert_eq!(
            build_job_id(
                "sitesorted-business",
                ProactivityProvider::Codex,
                "2026-04-14"
            ),
            "morning-sitesorted-business-codex-2026-04-14"
        );
    }

    #[test]
    fn quote_arg_if_needed_wraps_spaces() {
        assert_eq!(quote_arg_if_needed("simple"), "simple");
        assert_eq!(quote_arg_if_needed("two words"), "\"two words\"");
    }

    #[test]
    fn render_windows_command_supports_ps1_wrappers() {
        let rendered = render_windows_command(
            Path::new("C:/Users/OEM/bin/codex.ps1"),
            &[
                "--dangerously-bypass-approvals-and-sandbox",
                "munin-morning",
                "Read C:/brief.md",
            ],
        );
        assert!(rendered.contains("powershell.exe"));
        assert!(rendered.contains("-ExecutionPolicy Bypass"));
        assert!(rendered.contains("--dangerously-bypass-approvals-and-sandbox"));
        assert!(rendered.contains("munin-morning"));
    }

    #[test]
    fn provider_text_preserves_legacy_claude_fallback() {
        assert_eq!(parse_provider_text(None), ProactivityProvider::Claude);
        assert_eq!(parse_provider_text(Some("")), ProactivityProvider::Claude);
        assert_eq!(
            parse_provider_text(Some("unknown")),
            ProactivityProvider::Claude
        );
        assert_eq!(
            parse_provider_text(Some("claude")),
            ProactivityProvider::Claude
        );
        assert_eq!(
            parse_provider_text(Some("codex")),
            ProactivityProvider::Codex
        );
    }

    #[test]
    fn build_task_action_quotes_executable() {
        let action = build_task_action(
            Path::new("C:/Program Files/context/context.exe"),
            &[
                "proactivity",
                "run",
                "--scope",
                "sitesorted-business",
                "--provider",
                "codex",
            ],
        );
        assert!(action.starts_with("\"C:/Program Files/context/context.exe\""));
        assert!(action.contains("proactivity run"));
        assert!(action.contains("--provider codex"));
    }

    #[test]
    fn removable_task_names_include_both_provider_morning_tasks() {
        let tasks = removable_task_names("sitesorted-business");

        assert!(tasks.contains(&"Munin-Proactivity-Morning-sitesorted-business-claude".to_string()));
        assert!(tasks.contains(&"Munin-Proactivity-Morning-sitesorted-business-codex".to_string()));
        assert!(tasks.contains(&"Munin-Proactivity-Sweep-sitesorted-business".to_string()));
        assert!(tasks
            .contains(&"Context-Munin-Proactivity-Morning-sitesorted-business-claude".to_string()));
        assert!(tasks
            .contains(&"Context-Munin-Proactivity-Morning-sitesorted-business-codex".to_string()));
    }

    #[test]
    fn proactivity_scope_and_job_ids_reject_path_traversal() {
        assert!(validate_scope_id("sitesorted-business").is_ok());
        assert!(validate_scope_id("..\\..\\Temp\\escape").is_err());
        assert!(validate_scope_id("../escape").is_err());
        assert!(validate_scope_id("bad:scope").is_err());

        let paths = ProactivityPaths {
            queue_dir: PathBuf::from("queue"),
            results_dir: PathBuf::from("results"),
            briefs_dir: PathBuf::from("briefs"),
            state_dir: PathBuf::from("state"),
        };
        assert!(locate_job_paths(&paths, "morning-sitesorted-business-codex-2026-05-11").is_ok());
        assert!(locate_job_paths(&paths, "..\\escape").is_err());
        assert!(locate_job_paths(&paths, "C:\\Temp\\escape").is_err());
    }

    #[test]
    fn session_names_include_provider() {
        assert_eq!(
            build_session_name(ProactivityProvider::Claude, "sitesorted-business"),
            "munin-morning-sitesorted-business-claude"
        );
        assert_eq!(
            build_session_name(ProactivityProvider::Codex, "sitesorted-business"),
            "munin-morning-sitesorted-business-codex"
        );
        assert_eq!(
            morning_task_name("sitesorted-business", ProactivityProvider::Claude),
            "Munin-Proactivity-Morning-sitesorted-business-claude"
        );
        assert_eq!(
            morning_task_name("sitesorted-business", ProactivityProvider::Codex),
            "Munin-Proactivity-Morning-sitesorted-business-codex"
        );
    }

    #[test]
    fn build_launch_instructions_references_claim_and_complete_commands() {
        let runtime = RuntimeContext {
            config: sample_config(),
            scope_id: "sitesorted-business".to_string(),
            provider: ProactivityProvider::Claude,
            project_path: PathBuf::from("C:/Users/OEM/Projects/sitesorted"),
            schedule_local: "08:00".to_string(),
            max_spawns_per_day: 1,
            stale_claim_minutes: 90,
            paths: ProactivityPaths {
                queue_dir: PathBuf::from("C:/tmp/q"),
                results_dir: PathBuf::from("C:/tmp/r"),
                briefs_dir: PathBuf::from("C:/tmp/b"),
                state_dir: PathBuf::from("C:/tmp/s"),
            },
        };
        let job = ProactivityJob {
            schema_version: "munin-proactivity-v1".to_string(),
            job_type: "morning-proactivity".to_string(),
            job_id: "morning-sitesorted-business-2026-04-14".to_string(),
            scope_id: "sitesorted-business".to_string(),
            local_date: "2026-04-14".to_string(),
            created_at: Utc::now().to_rfc3339(),
            provider: ProactivityProvider::Claude,
            project_path: "C:/Users/OEM/Projects/sitesorted".to_string(),
            session_name: "munin-morning-sitesorted-business-claude".to_string(),
            prompt_token: MORNING_PROMPT_TOKEN.to_string(),
            brief_path: "C:/tmp/b/morning.md".to_string(),
            launch_instructions_path: "C:/tmp/b/morning.launch.md".to_string(),
            decision_path: "C:/tmp/r/morning.decision.json".to_string(),
            result_path: "C:/tmp/r/morning.json".to_string(),
            continuity_active: false,
            nudge_tasks: vec!["Instrument KPI".to_string()],
            intervention_job_ids: Vec::new(),
        };
        let text = build_launch_instructions(&runtime, &job).expect("instructions");
        assert!(text.contains("daemon already claimed"));
        assert!(text.contains("munin proactivity complete"));
        assert!(text.contains("munin-morning"));
        assert!(text.contains("top `friction-fix`"));
    }

    #[test]
    fn queue_approval_outcome_uses_truthful_label() {
        assert_eq!(
            ProactivityDecisionOutcome::QueueApproval.as_str(),
            "queue-approval"
        );
    }

    #[test]
    fn bootstrap_flag_does_not_defer_when_strategy_has_interrupt_nudges() {
        let temp = tempfile::tempdir().expect("tempdir");
        let runtime = RuntimeContext {
            config: sample_config(),
            scope_id: "sitesorted-business".to_string(),
            provider: ProactivityProvider::Claude,
            project_path: PathBuf::from("C:/Users/OEM/Projects/sitesorted"),
            schedule_local: "08:00".to_string(),
            max_spawns_per_day: 1,
            stale_claim_minutes: 90,
            paths: ProactivityPaths {
                queue_dir: temp.path().join("queue"),
                results_dir: temp.path().join("results"),
                briefs_dir: temp.path().join("briefs"),
                state_dir: temp.path().join("state"),
            },
        };
        let files = FileSet {
            job_id: "morning-sitesorted-business-claude-2026-04-20".to_string(),
            local_date: "2026-04-20".to_string(),
            pending_path: temp.path().join("queue").join("job.json"),
            claim_path: temp.path().join("queue").join("job.processing.json"),
            result_path: temp.path().join("results").join("job.result.json"),
            decision_path: temp.path().join("results").join("job.decision.json"),
            brief_path: temp.path().join("briefs").join("job.md"),
            launch_instructions_path: temp.path().join("briefs").join("job.launch.md"),
            completed_path: temp.path().join("state").join("completed.json"),
            heartbeat_path: temp.path().join("state").join("heartbeat"),
        };
        let mut report = fallback_recommend_report("sitesorted-business", "test".to_string());
        report.continuity.active = false;
        report.nudges.push(StrategicNudge {
            task: "Address red-state `Revenue (NZD)`".to_string(),
            item_id: Some("kpi-revenue-nzd".to_string()),
            item_kind: "kpi".to_string(),
            supports: Vec::new(),
            why_now: "Current value is below target.".to_string(),
            evidence: vec!["Current value: 0".to_string()],
            last_signal_at: None,
            evidence_freshness: "fresh".to_string(),
            confidence: "high".to_string(),
            interrupt_level: "interrupt".to_string(),
            suppression_reason: None,
            expected_effect: "Move the KPI toward target.".to_string(),
        });
        let decision = evaluate_decision(
            &runtime,
            &files,
            true,
            &report,
            &ProactivityCompletedIndex {
                schema_version: "test".to_string(),
                records: Vec::new(),
            },
        )
        .expect("decision");

        assert_eq!(
            decision.outcome,
            ProactivityDecisionOutcome::QueueApproval,
            "reasons: {:?}",
            decision.reasons
        );
        assert!(decision
            .reasons
            .contains(&"approval_queue_ready".to_string()));
    }

    #[test]
    fn friction_fix_to_nudge_queues_active_high_impact_work() {
        let fix = MemoryOsFrictionFix {
            fix_id: "friction:user-command-noise".to_string(),
            title: "Stop surfacing command noise".to_string(),
            impact: "high".to_string(),
            status: "active".to_string(),
            summary: "User corrected noisy Memory OS output repeatedly.".to_string(),
            permanent_fix: "Keep strategy and user prose above shell output.".to_string(),
            evidence: vec!["user correction at 2026-04-17T00:00:00Z".to_string()],
            last_signal_at: Some("2026-04-17T00:00:00Z".to_string()),
            score: 120,
        };

        let nudge = friction_fix_to_nudge(&fix);

        assert_eq!(nudge.item_kind, "friction-fix");
        assert_eq!(
            nudge.item_id.as_deref(),
            Some("friction:user-command-noise")
        );
        assert_eq!(nudge.interrupt_level, "interrupt");
        assert!(nudge.task.contains("Stop surfacing command noise"));
        assert!(nudge.expected_effect.contains("Permanently reduce"));
    }

    fn sample_runtime_with_project(project_path: PathBuf, root: &Path) -> RuntimeContext {
        RuntimeContext {
            config: sample_config(),
            scope_id: "sitesorted-business".to_string(),
            provider: ProactivityProvider::Claude,
            project_path,
            schedule_local: "08:00".to_string(),
            max_spawns_per_day: 1,
            stale_claim_minutes: 90,
            paths: ProactivityPaths {
                queue_dir: root.join("queue"),
                results_dir: root.join("results"),
                briefs_dir: root.join("briefs"),
                state_dir: root.join("state"),
            },
        }
    }

    fn sample_friction_nudge(evidence: Vec<String>, last_signal_at: &str) -> StrategicNudge {
        StrategicNudge {
            task: "Fix friction: Keep autonomous work moving without manual polling".to_string(),
            item_id: Some("friction:autonomy-polling".to_string()),
            item_kind: "friction-fix".to_string(),
            supports: Vec::new(),
            why_now: "User has asked for stronger autonomous polling behavior.".to_string(),
            evidence,
            last_signal_at: Some(last_signal_at.to_string()),
            evidence_freshness: "fresh".to_string(),
            confidence: "high".to_string(),
            interrupt_level: "interrupt".to_string(),
            suppression_reason: None,
            expected_effect: "Permanently reduce recurring friction: poll until terminal result."
                .to_string(),
        }
    }

    fn sample_recommend_report_with_nudge(nudge: StrategicNudge) -> StrategyRecommendReport {
        StrategyRecommendReport {
            generated_at: Utc::now().to_rfc3339(),
            scope_id: "sitesorted-business".to_string(),
            continuity: strategy::StrategyContinuitySnapshot {
                active: false,
                summary: None,
            },
            nudge_tasks: vec![nudge.task.clone()],
            continuity_tasks: Vec::new(),
            nudges: vec![nudge],
            suppressed_nudges: Vec::new(),
            warnings: Vec::new(),
        }
    }

    fn seed_completed_friction_fix(tracker: &Tracker, project_path: &str, evidence: &[String]) {
        seed_completed_friction_fix_for_item(
            tracker,
            project_path,
            "friction:autonomy-polling",
            evidence,
        )
    }

    fn seed_completed_friction_fix_for_item(
        tracker: &Tracker,
        project_path: &str,
        item_id: &str,
        evidence: &[String],
    ) {
        tracker
            .upsert_approval_job_for_project(
                project_path,
                &crate::core::tracking::ApprovalJobInput {
                    job_id: format!(
                        "approval-sitesorted-business-2026-05-01-friction-fix-{}",
                        item_id.replace(':', "-")
                    ),
                    scope: "project".to_string(),
                    scope_target: Some(project_path.to_string()),
                    local_date: "2026-05-01".to_string(),
                    item_id: Some(item_id.to_string()),
                    item_kind: "friction-fix".to_string(),
                    title: format!("Fix {item_id}"),
                    summary: "done; watching for recurrence".to_string(),
                    status: crate::core::tracking::ApprovalJobStatus::Completed,
                    source_kind: "strategy-nudge".to_string(),
                    provider: Some("codex".to_string()),
                    continuity_active: false,
                    expected_effect: Some(
                        "Permanently reduce recurring polling friction.".to_string(),
                    ),
                    queue_path: None,
                    result_path: Some("C:/tmp/result.json".to_string()),
                    evidence_json: serde_json::to_string(evidence).expect("evidence json"),
                    review_after: Some("2026-05-02T00:00:00Z".to_string()),
                    expires_at: Some("2026-05-08T00:00:00Z".to_string()),
                },
            )
            .expect("seed completed friction approval");
    }

    fn seed_pending_friction_fix_for_item(
        tracker: &Tracker,
        project_path: &str,
        item_id: &str,
        status: crate::core::tracking::ApprovalJobStatus,
    ) -> String {
        let job_id = format!(
            "approval-sitesorted-2026-05-09-friction-fix-{}",
            item_id.replace(':', "-")
        );
        tracker
            .upsert_approval_job_for_project(
                project_path,
                &crate::core::tracking::ApprovalJobInput {
                    job_id: job_id.clone(),
                    scope: "project".to_string(),
                    scope_target: Some(project_path.to_string()),
                    local_date: "2026-05-09".to_string(),
                    item_id: Some(item_id.to_string()),
                    item_kind: "friction-fix".to_string(),
                    title: format!("Fix {item_id}"),
                    summary: "pending stale friction approval".to_string(),
                    status,
                    source_kind: "strategy-nudge".to_string(),
                    provider: Some("codex".to_string()),
                    continuity_active: false,
                    expected_effect: Some("Reduce recurring friction.".to_string()),
                    queue_path: Some("C:/tmp/queue.json".to_string()),
                    result_path: None,
                    evidence_json: "[]".to_string(),
                    review_after: Some("2026-05-10T00:00:00Z".to_string()),
                    expires_at: Some("2026-05-16T00:00:00Z".to_string()),
                },
            )
            .expect("seed queued friction approval");
        job_id
    }

    #[test]
    fn completed_friction_fix_is_suppressed_when_evidence_changes_without_new_signal() {
        let temp = tempfile::tempdir().expect("tempdir");
        let tracker = Tracker::new_at_path(&temp.path().join("history.db")).expect("tracker");
        let project_path = temp.path().join("project");
        let runtime = sample_runtime_with_project(project_path.clone(), temp.path());
        let evidence = vec!["99 autonomy/polling corrections".to_string()];
        seed_completed_friction_fix(&tracker, &project_path.display().to_string(), &evidence);

        let changed_evidence = vec![
            "100 autonomy/polling corrections".to_string(),
            "changed evidence text without a new signal".to_string(),
        ];
        let mut report = sample_recommend_report_with_nudge(sample_friction_nudge(
            changed_evidence,
            "2026-05-01T00:00:00Z",
        ));
        suppress_completed_friction_nudges(&tracker, &runtime, &mut report).expect("suppression");

        assert!(report.nudges.is_empty());
        assert!(report.nudge_tasks.is_empty());
        assert_eq!(report.suppressed_nudges.len(), 1);
        assert_eq!(
            report.suppressed_nudges[0].suppression_reason.as_deref(),
            Some("watching_for_recurrence")
        );
    }

    #[test]
    fn completed_friction_fix_suppresses_related_command_family_across_projects() {
        let temp = tempfile::tempdir().expect("tempdir");
        let tracker = Tracker::new_at_path(&temp.path().join("history.db")).expect("tracker");
        let runtime_project = temp.path().join("project");
        let runtime = sample_runtime_with_project(runtime_project.clone(), temp.path());
        seed_completed_friction_fix_for_item(
            &tracker,
            "C:/Windows/System32",
            "friction:cli-syntax-drift",
            &["command guardrail completed".to_string()],
        );

        let mut nudge = sample_friction_nudge(
            vec!["174 examples retained in JSON evidence".to_string()],
            "2026-05-01T00:00:00Z",
        );
        nudge.task = "Fix friction: Path assumption drift".to_string();
        nudge.item_id = Some("friction:path-assumption-drift".to_string());
        nudge.expected_effect =
            "Permanently reduce recurring friction: resolve paths first.".to_string();
        let mut report = sample_recommend_report_with_nudge(nudge);

        suppress_completed_friction_nudges(&tracker, &runtime, &mut report).expect("suppression");

        assert!(report.nudges.is_empty());
        assert_eq!(report.suppressed_nudges.len(), 1);
        assert_eq!(
            report.suppressed_nudges[0].suppression_reason.as_deref(),
            Some("watching_for_recurrence")
        );
    }

    #[test]
    fn suppress_related_pending_friction_jobs_clears_family_variants() {
        let temp = tempfile::tempdir().expect("tempdir");
        let tracker = Tracker::new_at_path(&temp.path().join("history.db")).expect("tracker");
        let behavior_job = seed_pending_friction_fix_for_item(
            &tracker,
            "C:/Users/OEM/Projects/munin-memory.omx-worktrees/fix-command-noise-memory",
            "friction:behavior:codex",
            crate::core::tracking::ApprovalJobStatus::Approved,
        );
        let command_job = seed_pending_friction_fix_for_item(
            &tracker,
            "C:/Users/OEM/Projects/munin-memory",
            "friction:path-assumption-drift",
            crate::core::tracking::ApprovalJobStatus::Queued,
        );

        let suppressed = tracker
            .suppress_related_pending_friction_jobs(
                Some("friction:autonomy-polling"),
                Some("C:/tmp/result.json"),
                "watching_for_recurrence",
            )
            .expect("suppress queued family");

        assert_eq!(suppressed, 1);
        assert_eq!(
            tracker
                .get_approval_job(&behavior_job)
                .expect("behavior job")
                .expect("behavior job exists")
                .status,
            crate::core::tracking::ApprovalJobStatus::Suppressed
        );
        assert_eq!(
            tracker
                .get_approval_job(&command_job)
                .expect("command job")
                .expect("command job exists")
                .status,
            crate::core::tracking::ApprovalJobStatus::Queued
        );
    }

    #[test]
    fn completed_friction_fix_requeues_when_signal_is_newer_than_completion() {
        let temp = tempfile::tempdir().expect("tempdir");
        let tracker = Tracker::new_at_path(&temp.path().join("history.db")).expect("tracker");
        let project_path = temp.path().join("project");
        let runtime = sample_runtime_with_project(project_path.clone(), temp.path());
        seed_completed_friction_fix(
            &tracker,
            &project_path.display().to_string(),
            &["99 autonomy/polling corrections".to_string()],
        );

        let changed_evidence = vec![
            "100 autonomy/polling corrections".to_string(),
            "newer autonomy correction exists after codification".to_string(),
        ];
        let newer_signal_at = (Utc::now() + Duration::days(1)).to_rfc3339();
        let mut report = sample_recommend_report_with_nudge(sample_friction_nudge(
            changed_evidence,
            &newer_signal_at,
        ));
        suppress_completed_friction_nudges(&tracker, &runtime, &mut report).expect("suppression");

        assert_eq!(report.nudges.len(), 1);
        assert_eq!(report.nudge_tasks.len(), 1);
        assert!(report.suppressed_nudges.is_empty());
    }

    #[test]
    fn queued_jobs_can_be_suppressed_when_the_item_is_no_longer_actionable() {
        assert_eq!(
            preserve_approval_job_status(
                Some(crate::core::tracking::ApprovalJobStatus::Queued),
                crate::core::tracking::ApprovalJobStatus::Suppressed,
            ),
            crate::core::tracking::ApprovalJobStatus::Suppressed
        );
        assert_eq!(
            preserve_approval_job_status(
                Some(crate::core::tracking::ApprovalJobStatus::Approved),
                crate::core::tracking::ApprovalJobStatus::Suppressed,
            ),
            crate::core::tracking::ApprovalJobStatus::Approved
        );
    }

    #[test]
    fn morning_intervention_prompt_prioritizes_friction_fixes() {
        let report = StrategyRecommendReport {
            generated_at: Utc::now().to_rfc3339(),
            scope_id: "sitesorted-business".to_string(),
            continuity: strategy::StrategyContinuitySnapshot {
                active: false,
                summary: None,
            },
            nudge_tasks: vec![
                "Instrument KPI".to_string(),
                "Fix friction: Keep autonomous work moving without manual polling".to_string(),
            ],
            continuity_tasks: Vec::new(),
            nudges: vec![
                StrategicNudge {
                    task: "Instrument KPI".to_string(),
                    item_id: Some("kpi-paying-customers".to_string()),
                    item_kind: "kpi".to_string(),
                    supports: Vec::new(),
                    why_now: "Missing instrumentation.".to_string(),
                    evidence: vec!["No metric".to_string()],
                    last_signal_at: None,
                    evidence_freshness: "unknown".to_string(),
                    confidence: "medium".to_string(),
                    interrupt_level: "suggest".to_string(),
                    suppression_reason: None,
                    expected_effect: "Track KPI.".to_string(),
                },
                StrategicNudge {
                    task: "Fix friction: Keep autonomous work moving without manual polling"
                        .to_string(),
                    item_id: Some("friction:autonomy-polling".to_string()),
                    item_kind: "friction-fix".to_string(),
                    supports: Vec::new(),
                    why_now: "User has asked for stronger autonomous polling behavior.".to_string(),
                    evidence: vec!["154 autonomy/polling corrections".to_string()],
                    last_signal_at: Some("2026-05-01T00:00:00Z".to_string()),
                    evidence_freshness: "fresh".to_string(),
                    confidence: "high".to_string(),
                    interrupt_level: "interrupt".to_string(),
                    suppression_reason: None,
                    expected_effect:
                        "Permanently reduce recurring friction: poll until terminal result."
                            .to_string(),
                },
            ],
            suppressed_nudges: Vec::new(),
            warnings: Vec::new(),
        };

        let rendered = render_morning_intervention_prompt(&report);

        assert!(rendered.contains("<priority>friction-fix</priority>"));
        assert!(rendered.contains("Keep autonomous work moving"));
        assert!(rendered.contains("Completion bar"));
        assert!(!rendered.starts_with("Instrument KPI"));
    }

    #[test]
    fn launch_prompt_names_top_friction_intervention() {
        let runtime = RuntimeContext {
            config: sample_config(),
            scope_id: "sitesorted-business".to_string(),
            provider: ProactivityProvider::Claude,
            project_path: PathBuf::from("C:/Users/OEM/Projects/sitesorted"),
            schedule_local: "08:00".to_string(),
            max_spawns_per_day: 1,
            stale_claim_minutes: 90,
            paths: ProactivityPaths {
                queue_dir: PathBuf::from("C:/tmp/q"),
                results_dir: PathBuf::from("C:/tmp/r"),
                briefs_dir: PathBuf::from("C:/tmp/b"),
                state_dir: PathBuf::from("C:/tmp/s"),
            },
        };
        let job = ProactivityJob {
            schema_version: "munin-proactivity-v1".to_string(),
            job_type: "morning-proactivity".to_string(),
            job_id: "morning-sitesorted-business-2026-04-18".to_string(),
            scope_id: "sitesorted-business".to_string(),
            local_date: "2026-04-18".to_string(),
            created_at: Utc::now().to_rfc3339(),
            provider: ProactivityProvider::Claude,
            project_path: "C:/Users/OEM/Projects/sitesorted".to_string(),
            session_name: "munin-morning-sitesorted-business-claude".to_string(),
            prompt_token: MORNING_PROMPT_TOKEN.to_string(),
            brief_path: "C:/tmp/b/morning.md".to_string(),
            launch_instructions_path: "C:/tmp/b/morning.launch.md".to_string(),
            decision_path: "C:/tmp/r/morning.decision.json".to_string(),
            result_path: "C:/tmp/r/morning.json".to_string(),
            continuity_active: false,
            nudge_tasks: vec![
                "Fix friction: Keep autonomous work moving without manual polling".to_string(),
                "Instrument KPI".to_string(),
            ],
            intervention_job_ids: Vec::new(),
        };

        let launch = build_launch_command(&runtime, &job, Path::new("C:/tmp/b/morning.launch.md"))
            .expect("launch");

        assert!(launch
            .preview
            .contains("Start with this intervention: Fix friction"));
        assert!(launch.preview.contains("Work it until implemented"));
        assert!(!launch.preview.contains("set CLAUDECODE="));
        assert!(!launch.preview.contains("set ANTHROPIC_API_KEY="));
        assert!(!launch.preview.contains("set ANTHROPIC_AUTH_TOKEN="));
    }

    #[test]
    fn approve_does_not_relaunch_already_claimed_job() {
        let _guard = ENV_LOCK.lock().expect("env lock");
        let temp = tempfile::tempdir().expect("tempdir");
        std::env::set_var("MUNIN_CONFIG_DIR", temp.path().join("config"));

        let mut config = Config::default();
        config.proactivity.enabled = true;
        config.proactivity.project_path = Some(temp.path().join("project"));
        config.proactivity.queue_dir = Some(temp.path().join("queue"));
        config.proactivity.results_dir = Some(temp.path().join("results"));
        config.proactivity.briefs_dir = Some(temp.path().join("briefs"));
        config.proactivity.state_dir = Some(temp.path().join("state"));
        config.save().expect("save config");

        let paths = resolve_paths(&config.proactivity).expect("paths");
        ensure_runtime_dirs(&paths).expect("runtime dirs");
        let job = ProactivityJob {
            schema_version: "munin-proactivity-v1".to_string(),
            job_type: "morning-proactivity".to_string(),
            job_id: "morning-sitesorted-business-2026-04-14".to_string(),
            scope_id: "sitesorted-business".to_string(),
            local_date: "2026-04-14".to_string(),
            created_at: Utc::now().to_rfc3339(),
            provider: ProactivityProvider::Claude,
            project_path: temp.path().join("project").display().to_string(),
            session_name: "munin-morning-sitesorted-business-claude".to_string(),
            prompt_token: MORNING_PROMPT_TOKEN.to_string(),
            brief_path: temp.path().join("briefs/brief.md").display().to_string(),
            launch_instructions_path: temp.path().join("briefs/launch.md").display().to_string(),
            decision_path: temp
                .path()
                .join("results/decision.json")
                .display()
                .to_string(),
            result_path: temp
                .path()
                .join("results/result.json")
                .display()
                .to_string(),
            continuity_active: false,
            nudge_tasks: vec!["Instrument KPI".to_string()],
            intervention_job_ids: Vec::new(),
        };
        let claim_path = paths
            .queue_dir
            .join(format!("{}.processing.json", job.job_id));
        write_json_file(&claim_path, &job).expect("write claimed job");

        let report = approve(&ProactivityApproveOptions {
            job_id: job.job_id.clone(),
            no_spawn: true,
        })
        .expect("approve");

        assert!(!report.claimed);
        assert!(!report.launched);
        assert_eq!(
            report.claim_path.as_deref(),
            Some(claim_path.to_string_lossy().as_ref())
        );

        std::env::remove_var("MUNIN_CONFIG_DIR");
    }

    #[test]
    fn reconstruct_job_from_durable_approval_rebuilds_projection() {
        let temp = tempfile::tempdir().expect("tempdir");
        let paths = ProactivityPaths {
            queue_dir: temp.path().join("queue"),
            results_dir: temp.path().join("results"),
            briefs_dir: temp.path().join("briefs"),
            state_dir: temp.path().join("state"),
        };
        ensure_runtime_dirs(&paths).expect("runtime dirs");

        let record = crate::core::tracking::ApprovalJobRecord {
            job_id: "morning-sitesorted-business-2026-04-14".to_string(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            project_path: temp.path().join("project").display().to_string(),
            scope: "project".to_string(),
            scope_target: Some(temp.path().join("project").display().to_string()),
            local_date: "2026-04-14".to_string(),
            item_id: Some("sitesorted-business".to_string()),
            item_kind: "morning-proactivity".to_string(),
            title: "Morning proactivity".to_string(),
            summary: "Recovered from durable queue state.".to_string(),
            status: crate::core::tracking::ApprovalJobStatus::Queued,
            source_kind: "proactivity-envelope".to_string(),
            provider: Some("claude".to_string()),
            continuity_active: false,
            expected_effect: Some("Review the top intervention.".to_string()),
            queue_path: None,
            result_path: None,
            evidence_json: r#"["Instrument KPI"]"#.to_string(),
            review_after: None,
            expires_at: None,
            last_reviewed_at: None,
            closure_reason: None,
        };
        let job = reconstruct_job_from_approval_record(&paths, &record, &[])
            .expect("reconstruct")
            .expect("job");

        assert_eq!(job.job_id, record.job_id);
        assert!(paths
            .queue_dir
            .join(format!("{}.json", record.job_id))
            .exists());
        assert!(temp
            .path()
            .join("briefs")
            .join("morning-sitesorted-business-2026-04-14.launch.md")
            .exists());
    }

    #[test]
    fn claimed_rerun_does_not_downgrade_durable_approval_status() {
        let temp = tempfile::tempdir().expect("tempdir");
        let db_path = temp.path().join("history.db");
        let tracker = Tracker::new_at_path(&db_path).expect("tracker at temp path");
        let runtime = RuntimeContext {
            config: sample_config(),
            scope_id: "sitesorted-business".to_string(),
            provider: ProactivityProvider::Claude,
            project_path: temp.path().join("project"),
            schedule_local: "08:00".to_string(),
            max_spawns_per_day: 1,
            stale_claim_minutes: 90,
            paths: ProactivityPaths {
                queue_dir: temp.path().join("queue"),
                results_dir: temp.path().join("results"),
                briefs_dir: temp.path().join("briefs"),
                state_dir: temp.path().join("state"),
            },
        };
        ensure_runtime_dirs(&runtime.paths).expect("runtime dirs");
        let files = file_set_for_job(
            &runtime,
            "morning-sitesorted-business-2026-04-14",
            "2026-04-14",
        )
        .expect("file set");
        write_json_file(&files.claim_path, &serde_json::json!({"claimed": true}))
            .expect("claim marker");
        tracker
            .upsert_approval_job_for_project(
                &runtime.project_path.display().to_string(),
                &crate::core::tracking::ApprovalJobInput {
                    job_id: files.job_id.clone(),
                    scope: "project".to_string(),
                    scope_target: Some(runtime.project_path.display().to_string()),
                    local_date: files.local_date.clone(),
                    item_id: Some(runtime.scope_id.clone()),
                    item_kind: "morning-proactivity".to_string(),
                    title: "Morning proactivity".to_string(),
                    summary: "already approved".to_string(),
                    status: crate::core::tracking::ApprovalJobStatus::Approved,
                    source_kind: "proactivity-envelope".to_string(),
                    provider: Some(runtime.provider.as_str().to_string()),
                    continuity_active: true,
                    expected_effect: None,
                    queue_path: Some(files.claim_path.display().to_string()),
                    result_path: None,
                    evidence_json: "[]".to_string(),
                    review_after: None,
                    expires_at: None,
                },
            )
            .expect("seed approval");

        record_proactivity_approval_jobs(
            &tracker,
            &runtime,
            &files,
            &ProactivityDecisionArtifact {
                schema_version: "munin-proactivity-decision-v1".to_string(),
                generated_at: Utc::now().to_rfc3339(),
                job_id: files.job_id.clone(),
                scope_id: runtime.scope_id.clone(),
                local_date: files.local_date.clone(),
                provider: runtime.provider,
                outcome: ProactivityDecisionOutcome::Suppressed,
                reasons: vec!["morning_session_active".to_string()],
                continuity_active: true,
                brief_path: files.brief_path.display().to_string(),
                queue_path: None,
                result_path: None,
                nudge_tasks: Vec::new(),
            },
            &StrategyRecommendReport {
                generated_at: Utc::now().to_rfc3339(),
                scope_id: runtime.scope_id.clone(),
                continuity: strategy::StrategyContinuitySnapshot {
                    active: true,
                    summary: Some("already running".to_string()),
                },
                nudge_tasks: Vec::new(),
                continuity_tasks: Vec::new(),
                nudges: Vec::new(),
                suppressed_nudges: Vec::new(),
                warnings: Vec::new(),
            },
            false,
        )
        .expect("rerun should not downgrade");

        let approvals = tracker
            .get_approval_jobs_filtered(10, Some(&runtime.project_path.display().to_string()), None)
            .expect("approval jobs");
        assert_eq!(
            approvals[0].status,
            crate::core::tracking::ApprovalJobStatus::Approved
        );
    }

    #[test]
    fn proactivity_paths_default_under_munin_data_dir() {
        let _guard = ENV_LOCK.lock().expect("env lock");
        let temp = tempfile::tempdir().expect("tempdir");
        std::env::set_var("MUNIN_DATA_DIR", temp.path());

        let paths = resolve_paths(&ProactivityConfig::default()).expect("paths");
        assert!(paths
            .queue_dir
            .ends_with(Path::new("proactivity").join("queue")));
        assert!(paths
            .results_dir
            .ends_with(Path::new("proactivity").join("results")));
        assert!(paths
            .briefs_dir
            .ends_with(Path::new("proactivity").join("briefs")));
        assert!(paths
            .state_dir
            .ends_with(Path::new("proactivity").join("state")));

        std::env::remove_var("MUNIN_DATA_DIR");
    }

    #[test]
    fn completed_index_updates_once_per_job() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("completed.json");
        let result_path = temp.path().join("custom-results").join("job.json");
        let result = ProactivityResultArtifact {
            schema_version: "munin-proactivity-result-v1".to_string(),
            recorded_at: Utc::now().to_rfc3339(),
            job_id: "morning-sitesorted-business-2026-04-14".to_string(),
            scope_id: "sitesorted-business".to_string(),
            local_date: "2026-04-14".to_string(),
            provider: ProactivityProvider::Claude,
            status: ProactivityTerminalStatus::Complete,
            summary: "Done".to_string(),
            error: None,
            notes: Vec::new(),
        };
        assert!(update_completed_index(&path, &result, &result_path).expect("first insert"));
        assert!(!update_completed_index(&path, &result, &result_path).expect("dedupe insert"));
        let index = load_completed_index(&path).expect("index");
        assert_eq!(
            index.records[0].result_path,
            result_path.display().to_string()
        );
    }

    #[test]
    fn complete_marks_only_primary_intervention_complete() {
        let temp = tempfile::tempdir().expect("tempdir");
        let tracker = Tracker::new_at_path(&temp.path().join("history.db")).expect("tracker");
        let job_id = "morning-sitesorted-business-codex-2026-05-04".to_string();
        let primary_id =
            "approval-sitesorted-business-2026-05-04-friction-fix-friction:autonomy-polling";
        let secondary_id =
            "approval-sitesorted-business-2026-05-04-friction-fix-friction:user-command-noise";
        let tertiary_id =
            "approval-sitesorted-business-2026-05-04-friction-fix-friction:behavior:claude";
        let project_path = "C:/Users/OEM/Projects/sitesorted";
        let job = ProactivityJob {
            schema_version: "munin-proactivity-v1".to_string(),
            job_type: "morning-proactivity".to_string(),
            job_id: job_id.clone(),
            scope_id: "sitesorted-business".to_string(),
            local_date: "2026-05-04".to_string(),
            created_at: Utc::now().to_rfc3339(),
            provider: ProactivityProvider::Codex,
            project_path: project_path.to_string(),
            session_name: "codex".to_string(),
            prompt_token: MORNING_PROMPT_TOKEN.to_string(),
            brief_path: temp.path().join("brief.md").display().to_string(),
            launch_instructions_path: temp.path().join("launch.md").display().to_string(),
            decision_path: temp.path().join("decision.json").display().to_string(),
            result_path: temp.path().join("result.json").display().to_string(),
            continuity_active: false,
            nudge_tasks: vec![
                "Fix friction: Keep autonomous work moving without manual polling".to_string(),
                "Fix friction: Stop surfacing command/build noise as memory".to_string(),
                "Fix friction: Behavior change for claude".to_string(),
            ],
            intervention_job_ids: vec![
                primary_id.to_string(),
                secondary_id.to_string(),
                tertiary_id.to_string(),
            ],
        };

        for (id, item_id, title, evidence) in [
            (
                primary_id,
                "friction:autonomy-polling",
                "Fix friction: Keep autonomous work moving without manual polling",
                r#"["99 autonomy/polling corrections"]"#,
            ),
            (
                secondary_id,
                "friction:user-command-noise",
                "Fix friction: Stop surfacing command/build noise as memory",
                r#"["user correction at 2026-04-19T21:25:29.597+00:00"]"#,
            ),
            (
                tertiary_id,
                "friction:behavior:claude",
                "Fix friction: Behavior change for claude",
                r#"["Shared contract with codex so both lanes behave the same under polling instructions"]"#,
            ),
        ] {
            tracker
                .upsert_approval_job_for_project(
                    project_path,
                    &crate::core::tracking::ApprovalJobInput {
                        job_id: id.to_string(),
                        scope: "project".to_string(),
                        scope_target: Some(project_path.to_string()),
                        local_date: "2026-05-04".to_string(),
                        item_id: Some(item_id.to_string()),
                        item_kind: "friction-fix".to_string(),
                        title: title.to_string(),
                        summary: "queued intervention".to_string(),
                        status: crate::core::tracking::ApprovalJobStatus::Approved,
                        source_kind: "strategy-nudge".to_string(),
                        provider: Some("codex".to_string()),
                        continuity_active: false,
                        expected_effect: None,
                        queue_path: Some(
                            temp.path()
                                .join("job.processing.json")
                                .display()
                                .to_string(),
                        ),
                        result_path: Some(job.result_path.clone()),
                        evidence_json: evidence.to_string(),
                        review_after: None,
                        expires_at: None,
                    },
                )
                .expect("seed intervention");
        }

        update_terminal_approval_jobs(
            &tracker,
            &job,
            Path::new(&job.result_path),
            ProactivityTerminalStatus::Complete,
            "Primary fix completed.",
        )
        .expect("terminal approval update");

        assert_eq!(
            tracker
                .get_approval_job(primary_id)
                .expect("primary lookup")
                .expect("primary")
                .status,
            crate::core::tracking::ApprovalJobStatus::Completed
        );
        assert_eq!(
            tracker
                .get_approval_job(secondary_id)
                .expect("secondary lookup")
                .expect("secondary")
                .status,
            crate::core::tracking::ApprovalJobStatus::Approved
        );
        assert_eq!(
            tracker
                .get_approval_job(tertiary_id)
                .expect("tertiary lookup")
                .expect("tertiary")
                .status,
            crate::core::tracking::ApprovalJobStatus::Approved
        );
    }

    #[test]
    fn strategy_scope_resolution_prefers_proactivity_default_scope() {
        let mut config = Config::default();
        config.strategy = StrategyConfig::default();
        config.proactivity.default_scope = Some("custom-scope".to_string());
        assert_eq!(
            config
                .proactivity
                .resolve_scope_name(&config.strategy, None),
            "custom-scope"
        );
    }
}
