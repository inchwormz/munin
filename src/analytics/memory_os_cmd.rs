//! Read-only inspection commands for Memory OS shadow state.

use crate::analytics::session_impact_cmd::{load_sessions, SessionSource};
use crate::core::memory_os::{
    MemoryOsActionPolicyViewReport, MemoryOsBriefReport, MemoryOsCorrectionPatternSummary,
    MemoryOsEvidenceEventRecord, MemoryOsFrictionFix, MemoryOsFrictionReport,
    MemoryOsInspectionScope, MemoryOsNarrativeFinding, MemoryOsOverviewReport,
    MemoryOsProfileReport, MemoryOsPromotedAssertionRecord, MemoryOsPromotionReport,
    MemoryOsSourceBehaviorSummary, MemoryOsTrustObservationRecord, MemoryOsTrustReport,
};
use crate::core::strategy;
use crate::core::tracking::{
    ContextEventStats, ContextRuntimeInfo, ContextSelectedItemRecord, Tracker,
};
use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

#[derive(Debug, Clone, Serialize)]
struct MemoryOsInspectSourceOrigin {
    origin: String,
    root: String,
    sessions: usize,
}

#[derive(Debug, Clone, Serialize)]
struct MemoryOsInspectSourceCoverage {
    source: String,
    raw_current: usize,
    imported_total: usize,
    missing_current: usize,
    imported_not_current: usize,
    excluded_subagents: usize,
    parse_failures: usize,
    prompt_only_sessions: usize,
    shell_history_sessions: usize,
    oldest_session: Option<String>,
    newest_session: Option<String>,
    source_origins: Vec<MemoryOsInspectSourceOrigin>,
    missing_current_ids_sample: Vec<String>,
    imported_not_current_ids_sample: Vec<String>,
    parse_failure_ids_sample: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
struct MemoryOsInspectOnboardingStatus {
    schema_version: String,
    status: String,
    started_at: Option<String>,
    completed_at: Option<String>,
    sessions_processed: usize,
    shells_ingested: usize,
    corrections_ingested: usize,
    imported_source_counts: Vec<(String, usize)>,
}

#[derive(Debug, Clone, Serialize)]
struct MemoryOsInspectImportPipeline {
    onboarding: MemoryOsInspectOnboardingStatus,
    recall_imported_total: usize,
}

#[derive(Debug, Clone, Serialize)]
struct MemoryOsInspectCompiledMemory {
    brief: MemoryOsBriefReport,
    overview: MemoryOsOverviewReport,
    profile: MemoryOsProfileReport,
    friction: MemoryOsFrictionReport,
    action_policy: MemoryOsActionPolicyViewReport,
    promoted_assertions: Vec<MemoryOsPromotedAssertionRecord>,
    evidence_events: Vec<MemoryOsEvidenceEventRecord>,
    promotion: MemoryOsPromotionReport,
    trust: MemoryOsTrustReport,
}

#[derive(Debug, Clone, Serialize)]
struct MemoryOsInspectReport {
    schema_version: String,
    generated_at: String,
    scope: MemoryOsInspectionScope,
    source_roots: Vec<String>,
    raw_sources: Vec<MemoryOsInspectSourceCoverage>,
    import_pipeline: MemoryOsInspectImportPipeline,
    compiled_memory: MemoryOsInspectCompiledMemory,
}

#[derive(Debug, Clone, Serialize)]
struct MemoryOsDoctorCorpusSource {
    source: String,
    imported_sessions: usize,
    import_status: String,
}

#[derive(Debug, Clone, Serialize)]
struct MemoryOsDoctorCorpus {
    onboarding_status: String,
    onboarding_completed_at: Option<String>,
    sessions_processed: usize,
    shells_ingested: usize,
    corrections_ingested: usize,
    imported_sessions_total: usize,
    imported_shell_executions_total: usize,
    checkpoint_count: usize,
    journal_event_count: usize,
    latest_import_completed_at: Option<String>,
    latest_import_freshness: String,
    raw_coverage_mode: String,
    source_health: Vec<MemoryOsDoctorCorpusSource>,
}

#[derive(Debug, Clone, Serialize)]
struct MemoryOsDoctorCheck {
    name: String,
    status: String,
    summary: String,
    evidence: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
struct MemoryOsDoctorProblem {
    severity: String,
    title: String,
    summary: String,
    permanent_fix: String,
    evidence: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
struct MemoryOsDoctorReport {
    schema_version: String,
    generated_at: String,
    scope: MemoryOsInspectionScope,
    overall_status: String,
    corpus: MemoryOsDoctorCorpus,
    signal_quality: Vec<MemoryOsDoctorCheck>,
    top_pipeline_problems: Vec<MemoryOsDoctorProblem>,
    recommended_permanent_fix: String,
}

struct LoadedOnboardingStatus {
    status: MemoryOsInspectOnboardingStatus,
    imported_ids: HashMap<String, HashSet<String>>,
}

#[derive(Debug, Clone, Default)]
struct StrategyBriefFindings {
    knowledge: Vec<MemoryOsNarrativeFinding>,
    active: Vec<MemoryOsNarrativeFinding>,
    watchouts: Vec<MemoryOsNarrativeFinding>,
}

#[derive(Debug, Clone)]
struct ParsedSessionSummary {
    session_id: String,
    started_at: DateTime<Utc>,
    prompt_count: usize,
    shell_count: usize,
}

#[derive(Debug, Clone)]
struct RawSourceSummary {
    roots: Vec<String>,
    raw_ids: HashSet<String>,
    excluded_subagents: HashSet<String>,
    parsed_sessions: Vec<ParsedSessionSummary>,
    origins: Vec<MemoryOsInspectSourceOrigin>,
}

pub fn run_snapshot(project_path: Option<&str>, format: &str, _verbose: u8) -> Result<()> {
    validate_format(format)?;

    let tracker = Tracker::new().context("Failed to initialize tracking database")?;
    render_snapshot_with_tracker(&tracker, project_path, format)
}

fn render_snapshot_with_tracker(
    tracker: &Tracker,
    project_path: Option<&str>,
    format: &str,
) -> Result<()> {
    let snapshot = tracker
        .get_memory_os_project_snapshot(project_path)
        .context("Failed to load Memory OS project snapshot")?;

    match format {
        "json" => {
            println!("{}", serde_json::to_string_pretty(&snapshot)?);
        }
        "text" => {
            println!("Memory OS Snapshot");
            println!("------------------");
            println!("Project: {}", snapshot.project_path);
            println!("Journal events: {}", snapshot.journal_event_count);
            println!(
                "Last journal seq: {}",
                snapshot
                    .last_journal_seq
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "none".to_string())
            );
            println!(
                "Verification results (scoped): {}",
                snapshot.verification_result_count
            );
            println!("Trust observations: {}", snapshot.trust_observation_count);
            println!(
                "Projection checkpoints: {}",
                snapshot.projection_checkpoints.len()
            );
            for checkpoint in &snapshot.projection_checkpoints {
                println!(
                    "- {} [{}..{}] {} @ {}",
                    checkpoint.projection_name,
                    checkpoint.from_seq,
                    checkpoint.to_seq,
                    checkpoint.rebuild_kind,
                    checkpoint.created_at
                );
            }
        }
        _ => unreachable!("format is validated before database access"),
    }

    Ok(())
}

pub fn run_kernel(project_path: Option<&str>, format: &str, _verbose: u8) -> Result<()> {
    validate_format(format)?;

    let tracker = Tracker::new().context("Failed to initialize tracking database")?;
    render_kernel_with_tracker(&tracker, project_path, format)
}

fn render_kernel_with_tracker(
    tracker: &Tracker,
    project_path: Option<&str>,
    format: &str,
) -> Result<()> {
    let kernel = tracker
        .get_memory_os_project_kernel(project_path)
        .context("Failed to rebuild Memory OS kernel")?;

    match format {
        "json" => {
            println!("{}", serde_json::to_string_pretty(&kernel)?);
        }
        "text" => {
            println!("Memory OS Kernel");
            println!("----------------");
            println!("Project: {}", kernel.project_path);
            println!(
                "Last journal seq: {}",
                kernel
                    .last_journal_seq
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "none".to_string())
            );
            println!("Claims: {}", kernel.claims.len());
            println!("Open loops: {}", kernel.open_loops.len());
            println!("Checkpoints: {}", kernel.checkpoints.len());

            if let Some(checkpoint) = kernel.checkpoints.first() {
                println!(
                    "Latest checkpoint: {} @ {}",
                    checkpoint.preset, checkpoint.captured_at
                );
                if let Some(recommendation) = &checkpoint.current_recommendation {
                    println!("Current recommendation: {}", recommendation);
                }
                println!(
                    "First verification: {}",
                    checkpoint.reentry.first_verification
                );
            }

            for open_loop in kernel.open_loops.iter().take(5) {
                println!(
                    "- [{}|{}] {}",
                    open_loop.status, open_loop.severity, open_loop.summary
                );
            }
        }
        _ => unreachable!("format is validated before database access"),
    }

    Ok(())
}

pub fn run_actions(project_path: Option<&str>, format: &str, _verbose: u8) -> Result<()> {
    validate_format(format)?;

    let tracker = Tracker::new().context("Failed to initialize tracking database")?;
    let candidates = tracker
        .get_memory_os_action_candidates(project_path)
        .context("Failed to derive Memory OS action candidates")?;

    match format {
        "json" => {
            println!("{}", serde_json::to_string_pretty(&candidates)?);
        }
        "text" => {
            println!("Memory OS Action Candidates");
            println!("---------------------------");
            println!("Candidates: {}", candidates.len());
            for candidate in candidates.iter().take(10) {
                println!(
                    "- [{}|{}|{}] {}",
                    candidate.status,
                    candidate.actuator_type,
                    candidate.confidence,
                    candidate.title
                );
                println!("  {}", candidate.summary);
                println!(
                    "  precedents: {} | success: {} | failure: {} | aging: {}",
                    candidate.precedent_count,
                    candidate.success_count,
                    candidate.failure_count,
                    candidate.aging_status
                );
                if let Some(command) = &candidate.action.command_sig {
                    println!("  command: {}", command);
                }
                if let Some(recommendation) = &candidate.action.recommendation {
                    println!("  recommendation: {}", recommendation);
                }
                println!(
                    "  review-after: {} | expires: {} | policy: {}",
                    candidate.review_after.as_deref().unwrap_or("not-scheduled"),
                    candidate.expires_at.as_deref().unwrap_or("not-set"),
                    candidate.lifecycle_policy.as_deref().unwrap_or("expiring")
                );
            }
        }
        _ => unreachable!("format is validated before database access"),
    }

    Ok(())
}

pub fn run_overview(
    scope: &str,
    project_path: Option<&str>,
    format: &str,
    _verbose: u8,
) -> Result<()> {
    validate_format(format)?;
    let scope = validate_scope(scope, project_path)?;

    let tracker = Tracker::new().context("Failed to initialize tracking database")?;
    let report = tracker
        .get_memory_os_overview_report(scope, project_path)
        .context("Failed to compile Memory OS overview report")?;

    match format {
        "json" => println!("{}", serde_json::to_string_pretty(&report)?),
        "text" => render_overview_text(&report),
        _ => unreachable!("format is validated before database access"),
    }

    Ok(())
}

pub fn run_recall(
    scope: &str,
    project_path: Option<&str>,
    query: &str,
    format: &str,
    _verbose: u8,
) -> Result<()> {
    validate_format(format)?;
    let scope = validate_scope(scope, project_path)?;
    let query = query.trim();
    if query.is_empty() {
        anyhow::bail!("munin recall needs a topic, for example: munin recall \"resolver\"");
    }

    let tracker = Tracker::new().context("Failed to initialize tracking database")?;
    let report = tracker
        .get_memory_os_recall_report(scope, project_path, query)
        .context("Failed to compile Memory OS recall report")?;

    match format {
        "json" => println!("{}", serde_json::to_string_pretty(&report)?),
        "text" => render_recall_text(&report),
        _ => unreachable!("format is validated before database access"),
    }

    Ok(())
}

pub fn run_action_policy(
    scope: &str,
    project_path: Option<&str>,
    format: &str,
    _verbose: u8,
) -> Result<()> {
    validate_format(format)?;
    let scope = validate_scope(scope, project_path)?;

    let tracker = Tracker::new().context("Failed to initialize tracking database")?;
    let report = tracker
        .get_memory_os_action_policy_view_report(scope, project_path)
        .context("Failed to compile Memory OS action policy view")?;

    match format {
        "json" => println!("{}", serde_json::to_string_pretty(&report)?),
        "text" => render_action_policy_text(&report),
        _ => unreachable!("format is validated before database access"),
    }

    Ok(())
}

pub fn run_profile(
    scope: &str,
    project_path: Option<&str>,
    format: &str,
    _verbose: u8,
) -> Result<()> {
    validate_format(format)?;
    let scope = validate_scope(scope, project_path)?;

    let tracker = Tracker::new().context("Failed to initialize tracking database")?;
    let report = tracker
        .get_memory_os_profile_report(scope, project_path)
        .context("Failed to compile Memory OS profile report")?;

    match format {
        "json" => println!("{}", serde_json::to_string_pretty(&report)?),
        "text" => render_profile_text(&report),
        _ => unreachable!("format is validated before database access"),
    }

    Ok(())
}

pub fn run_friction(
    scope: &str,
    project_path: Option<&str>,
    format: &str,
    _verbose: u8,
) -> Result<()> {
    run_friction_filtered(scope, project_path, format, None, None)
}

pub fn run_friction_filtered(
    scope: &str,
    project_path: Option<&str>,
    format: &str,
    agent: Option<&str>,
    last: Option<&str>,
) -> Result<()> {
    validate_format(format)?;
    let scope = validate_scope(scope, project_path)?;

    let tracker = Tracker::new().context("Failed to initialize tracking database")?;
    let mut report = tracker
        .get_memory_os_friction_report(scope, project_path)
        .context("Failed to compile Memory OS friction report")?;
    apply_friction_filters(&mut report, agent, last);

    match format {
        "json" => println!("{}", serde_json::to_string_pretty(&report)?),
        "text" => render_friction_text(&report),
        _ => unreachable!("format is validated before database access"),
    }

    Ok(())
}

pub fn run_trust(
    scope: &str,
    project_path: Option<&str>,
    format: &str,
    _verbose: u8,
) -> Result<()> {
    validate_format(format)?;
    let scope = validate_scope(scope, project_path)?;

    let tracker = Tracker::new().context("Failed to initialize tracking database")?;
    render_trust_with_tracker(&tracker, scope, project_path, format)
}

fn render_trust_with_tracker(
    tracker: &Tracker,
    scope: MemoryOsInspectionScope,
    project_path: Option<&str>,
    format: &str,
) -> Result<()> {
    let report = tracker
        .get_memory_os_trust_report(scope, project_path)
        .context("Failed to compile Memory OS trust report")?;

    match format {
        "json" => println!("{}", serde_json::to_string_pretty(&report)?),
        "text" => render_trust_text(&report),
        _ => unreachable!("format is validated before report rendering"),
    }

    Ok(())
}

pub fn run_promotion(format: &str, _verbose: u8) -> Result<()> {
    validate_format(format)?;
    let tracker = Tracker::new().context("Failed to initialize tracking database")?;
    println!("{}", render_promotion_with_tracker(&tracker, format)?);
    Ok(())
}

pub fn run_inspect(
    scope: &str,
    project_path: Option<&str>,
    format: &str,
    _verbose: u8,
) -> Result<()> {
    validate_format(format)?;
    let scope = validate_scope(scope, project_path)?;
    let tracker = Tracker::new().context("Failed to initialize tracking database")?;
    let report = build_inspect_report(&tracker, scope, project_path)?;
    match format {
        "json" => println!("{}", serde_json::to_string_pretty(&report)?),
        "text" => println!("{}", render_inspect_text(&report)),
        _ => unreachable!("format is validated before report rendering"),
    }
    Ok(())
}

pub fn run_doctor(
    scope: &str,
    project_path: Option<&str>,
    format: &str,
    _verbose: u8,
) -> Result<()> {
    validate_format(format)?;
    let scope = validate_scope(scope, project_path)?;
    let tracker = Tracker::new().context("Failed to initialize tracking database")?;
    let report = build_doctor_report(&tracker, scope, project_path)?;
    match format {
        "json" => println!("{}", serde_json::to_string_pretty(&report)?),
        "text" => println!("{}", render_doctor_text(&report)),
        _ => unreachable!("format is validated before report rendering"),
    }
    Ok(())
}

pub fn run_brief(
    scope: &str,
    project_path: Option<&str>,
    format: &str,
    startup_bootstrap: bool,
    _verbose: u8,
) -> Result<()> {
    validate_brief_format(format)?;
    let scope = validate_scope(scope, project_path)?;
    let tracker = Tracker::new().context("Failed to initialize tracking database")?;
    let rendered = render_brief_with_tracker(
        &tracker,
        scope,
        project_path,
        format,
        startup_bootstrap,
        "memory-os-brief",
        "memory-os-startup",
    )?;
    println!("{}", rendered);
    Ok(())
}

pub(crate) fn render_brief_with_tracker(
    tracker: &Tracker,
    scope: MemoryOsInspectionScope,
    project_path: Option<&str>,
    format: &str,
    record_context_event: bool,
    context_event_type: &str,
    runtime_source: &str,
) -> Result<String> {
    validate_brief_format(format)?;
    let brief = build_brief_report(tracker, scope, project_path)?;
    let rendered = render_brief_output(&brief, format)?;

    if record_context_event {
        record_brief_context_event(
            tracker,
            &brief,
            &rendered,
            context_event_type,
            runtime_source,
        )?;
    }

    Ok(rendered)
}

fn validate_format(format: &str) -> Result<()> {
    if format != "json" && format != "text" {
        return Err(anyhow!(
            "unsupported format '{}' (expected text or json)",
            format
        ));
    }
    Ok(())
}

fn validate_brief_format(format: &str) -> Result<()> {
    if format != "json" && format != "text" && format != "prompt" {
        return Err(anyhow!(
            "unsupported format '{}' (expected text, json, or prompt)",
            format
        ));
    }
    Ok(())
}

fn validate_scope(scope: &str, project_path: Option<&str>) -> Result<MemoryOsInspectionScope> {
    let parsed = match scope {
        "user" => MemoryOsInspectionScope::User,
        "project" => MemoryOsInspectionScope::Project,
        other => {
            return Err(anyhow!(
                "unsupported scope '{}' (expected user or project)",
                other
            ))
        }
    };

    if parsed == MemoryOsInspectionScope::User && project_path.is_some() {
        return Err(anyhow!(
            "--project is only valid when --scope project is selected"
        ));
    }

    Ok(parsed)
}

fn build_brief_report(
    tracker: &Tracker,
    scope: MemoryOsInspectionScope,
    project_path: Option<&str>,
) -> Result<MemoryOsBriefReport> {
    let overview = tracker
        .get_memory_os_overview_report(scope, project_path)
        .context("Failed to load Memory OS overview report")?;
    let profile = tracker
        .get_memory_os_profile_report(scope, project_path)
        .context("Failed to load Memory OS profile report")?;
    let friction = tracker
        .get_memory_os_friction_report(scope, project_path)
        .context("Failed to load Memory OS friction report")?;
    let continuity = tracker
        .get_memory_os_continuity_findings(scope, project_path)
        .context("Failed to load Memory OS continuity findings")?;
    let promoted_assertions = tracker
        .get_memory_os_promoted_assertions(scope, project_path, 20)
        .context("Failed to load Memory OS promoted assertions")?;
    let strategy_findings = build_strategy_brief_findings();

    let assertion_findings = promoted_assertion_findings(&promoted_assertions);
    let project_focus = project_focus_findings(&overview);
    let authoritative_strategy_active = !strategy_findings.knowledge.is_empty();

    let what_i_know_candidates = strategy_findings
        .knowledge
        .iter()
        .chain(assertion_findings.iter())
        .chain(profile.preferences.iter())
        .chain(project_focus.iter())
        .chain(profile.recurring_themes.iter())
        .filter(|finding| {
            brief_finding_belongs_in_knowledge(finding, authoritative_strategy_active)
        })
        .collect::<Vec<_>>();
    let what_i_know = rank_brief_findings(what_i_know_candidates, 5);

    let how_you_work_candidates = profile
        .preferences
        .iter()
        .chain(profile.operating_style.iter())
        .chain(profile.autonomy_tendencies.iter())
        .filter(|finding| brief_finding_belongs_in_work_style(finding))
        .collect::<Vec<_>>();
    let how_you_work = rank_brief_findings(how_you_work_candidates, 5);

    let active_candidates = strategy_findings
        .active
        .iter()
        .chain(overview.active_work.iter())
        .filter(|finding| active_finding_is_specific(finding))
        .collect::<Vec<_>>();
    let active_work = rank_brief_findings(active_candidates, 5);
    let mut what_is_active = active_work;
    let mut active_summaries = what_is_active
        .iter()
        .map(|finding| finding.summary.clone())
        .collect::<HashSet<_>>();
    if what_is_active.len() < 3 {
        what_is_active.extend(
            continuity
                .iter()
                .filter(|finding| active_summaries.insert(finding.summary.clone()))
                .cloned(),
        );
    }
    what_is_active.truncate(5);

    let next_steps = build_brief_next_steps(
        &overview,
        &profile,
        &friction,
        &continuity,
        &strategy_findings.active,
        &what_is_active,
    );
    let watchouts = build_brief_watchouts(&overview, &friction, &strategy_findings.watchouts);

    Ok(MemoryOsBriefReport {
        generated_at: Utc::now().to_rfc3339(),
        scope,
        what_i_know,
        how_you_work,
        what_is_active,
        next_steps,
        watchouts,
    })
}

fn promoted_assertion_findings(
    assertions: &[MemoryOsPromotedAssertionRecord],
) -> Vec<MemoryOsNarrativeFinding> {
    assertions
        .iter()
        .filter(|assertion| assertion.status == "active")
        .map(|assertion| {
            let title = match assertion.category.as_str() {
                "obligation" => "Durable obligation",
                "rejection" => "Durable rejection",
                "decision" => "Durable decision",
                "hypothesis-tested" => "Tested belief",
                _ => "Durable memory",
            };
            MemoryOsNarrativeFinding {
                title: title.to_string(),
                summary: assertion.statement.clone(),
                evidence: assertion
                    .supporting_evidence
                    .iter()
                    .take(2)
                    .cloned()
                    .collect(),
            }
        })
        .collect()
}

fn project_focus_findings(overview: &MemoryOsOverviewReport) -> Vec<MemoryOsNarrativeFinding> {
    let mut findings = Vec::new();
    let project_names = overview
        .top_projects
        .iter()
        .filter(|project| {
            project.repo_label != "workspace-root" && project.repo_label != "home-root"
        })
        .take(4)
        .map(|project| project.repo_label.clone())
        .collect::<Vec<_>>();
    if !project_names.is_empty() {
        findings.push(MemoryOsNarrativeFinding {
            title: "Project focus".to_string(),
            summary: format!(
                "Recurring work clusters around {}.",
                project_names.join(", ")
            ),
            evidence: overview
                .top_projects
                .iter()
                .filter(|project| project_names.contains(&project.repo_label))
                .take(3)
                .map(|project| {
                    format!(
                        "{}: {} sessions, {} shells",
                        project.repo_label, project.sessions, project.shell_executions
                    )
                })
                .collect(),
        });
    }
    findings
}

fn build_strategy_brief_findings() -> StrategyBriefFindings {
    let mut findings = StrategyBriefFindings::default();
    let Ok(reports) = strategy::discover_inspect_reports(1) else {
        return findings;
    };

    for report in reports {
        let kernel_findings = strategy_kernel_brief_findings(&report.kernel);
        findings.knowledge.extend(kernel_findings.knowledge);
        findings.active.extend(kernel_findings.active);
        findings.watchouts.extend(kernel_findings.watchouts);
    }

    findings.knowledge = dedupe_brief_findings(findings.knowledge, 6);
    findings.active = dedupe_brief_findings(findings.active, 5);
    findings.watchouts = dedupe_brief_findings(findings.watchouts, 4);
    findings
}

fn strategy_kernel_brief_findings(kernel: &strategy::StrategyKernel) -> StrategyBriefFindings {
    let mut findings = StrategyBriefFindings::default();

    let goals = kernel
        .goals
        .iter()
        .filter(|goal| !goal.title.trim().is_empty())
        .take(4)
        .map(|goal| match goal.due_date.as_deref() {
            Some(due) => format!("{} by {}", goal.title, due),
            None => goal.title.clone(),
        })
        .collect::<Vec<_>>();
    if !goals.is_empty() {
        findings.knowledge.push(MemoryOsNarrativeFinding {
            title: "Authoritative strategy".to_string(),
            summary: format!("{} strategy goals: {}.", kernel.scope_id, goals.join("; ")),
            evidence: strategy_source_evidence(kernel),
        });
    }

    let kpis = kernel
        .kpis
        .iter()
        .filter(|kpi| !kpi.title.trim().is_empty())
        .take(4)
        .map(|kpi| {
            let target = kpi
                .target
                .map(|value| {
                    if value.fract() == 0.0 {
                        format!("{}", value as i64)
                    } else {
                        format!("{value:.1}")
                    }
                })
                .unwrap_or_else(|| "untracked".to_string());
            match kpi.unit.as_deref() {
                Some(unit) if !unit.trim().is_empty() => {
                    format!("{} target {} {}", kpi.title, target, unit)
                }
                _ => format!("{} target {}", kpi.title, target),
            }
        })
        .collect::<Vec<_>>();
    if !kpis.is_empty() {
        findings.knowledge.push(MemoryOsNarrativeFinding {
            title: "Strategy KPI".to_string(),
            summary: kpis.join("; "),
            evidence: strategy_source_evidence(kernel),
        });
    }

    let initiatives = kernel
        .initiatives
        .iter()
        .filter(|initiative| !initiative.deferred)
        .filter(|initiative| !initiative.title.trim().is_empty())
        .take(4)
        .map(|initiative| match initiative.due_date.as_deref() {
            Some(due) => format!("{} by {}", initiative.title, due),
            None => initiative.title.clone(),
        })
        .collect::<Vec<_>>();
    if !initiatives.is_empty() {
        findings.active.push(MemoryOsNarrativeFinding {
            title: "Strategy initiative".to_string(),
            summary: format!("Active strategy work: {}.", initiatives.join("; ")),
            evidence: strategy_source_evidence(kernel),
        });
    }

    let constraints = kernel
        .constraints
        .iter()
        .filter(|constraint| !constraint.title.trim().is_empty())
        .take(3)
        .map(|constraint| constraint.title.clone())
        .collect::<Vec<_>>();
    if !constraints.is_empty() {
        findings.watchouts.push(MemoryOsNarrativeFinding {
            title: "Strategy constraint".to_string(),
            summary: format!("Respect strategy constraints: {}.", constraints.join("; ")),
            evidence: strategy_source_evidence(kernel),
        });
    }

    let deferred = kernel
        .initiatives
        .iter()
        .filter(|initiative| initiative.deferred)
        .filter(|initiative| !initiative.title.trim().is_empty())
        .take(3)
        .map(|initiative| initiative.title.clone())
        .collect::<Vec<_>>();
    if !deferred.is_empty() {
        findings.watchouts.push(MemoryOsNarrativeFinding {
            title: "Strategy deferred".to_string(),
            summary: format!("Explicitly not now: {}.", deferred.join("; ")),
            evidence: strategy_source_evidence(kernel),
        });
    }

    findings
}

fn strategy_source_evidence(kernel: &strategy::StrategyKernel) -> Vec<String> {
    kernel
        .sources
        .iter()
        .take(2)
        .map(|source| {
            format!(
                "strategy source: {}",
                clean_strategy_source_path(&source.path)
            )
        })
        .collect()
}

fn clean_strategy_source_path(path: &str) -> String {
    let cleaned = path.trim_start_matches(r"\\?\");
    let normalized = cleaned.replace('\\', "/");
    if let Some((_, suffix)) = normalized.split_once("/Projects/") {
        return suffix.to_string();
    }
    if let Some((_, suffix)) = normalized.split_once("/context/") {
        return format!("context/{suffix}");
    }
    normalized
}

fn dedupe_brief_findings(
    findings: Vec<MemoryOsNarrativeFinding>,
    limit: usize,
) -> Vec<MemoryOsNarrativeFinding> {
    let mut deduped = Vec::new();
    let mut seen = HashSet::new();
    for finding in findings {
        let key = format!("{}:{}", finding.title, finding.summary).to_ascii_lowercase();
        if seen.insert(key) {
            deduped.push(finding);
            if deduped.len() >= limit {
                break;
            }
        }
    }
    deduped
}

fn brief_finding_belongs_in_knowledge(
    finding: &MemoryOsNarrativeFinding,
    authoritative_strategy_active: bool,
) -> bool {
    if brief_finding_has_command_noise(finding) {
        return false;
    }
    if authoritative_strategy_active
        && matches!(
            finding.title.as_str(),
            "Business strategy" | "Lead generation strategy" | "SiteSorted focus"
        )
    {
        return false;
    }
    matches!(
        finding.title.as_str(),
        "Durable decision"
            | "Durable memory"
            | "Tested belief"
            | "Authoritative strategy"
            | "Strategy KPI"
            | "Business strategy"
            | "Lead generation strategy"
            | "Memory OS direction"
            | "Project focus"
            | "SiteSorted focus"
    )
}

fn brief_finding_belongs_in_work_style(finding: &MemoryOsNarrativeFinding) -> bool {
    if brief_finding_has_command_noise(finding) {
        return false;
    }
    if work_style_finding_is_task_specific(finding) {
        return false;
    }
    matches!(
        finding.title.as_str(),
        "Working preference" | "Product constraint" | "Positive feedback"
    )
}

fn work_style_finding_is_task_specific(finding: &MemoryOsNarrativeFinding) -> bool {
    let lowered = finding.summary.to_ascii_lowercase();
    [
        "current runs",
        "finish this task",
        "this task first",
        "please review",
        "do not edit files",
        "review the current",
        "local url",
        "artifacts:",
    ]
    .iter()
    .any(|needle| lowered.contains(needle))
}

fn render_brief_output(report: &MemoryOsBriefReport, format: &str) -> Result<String> {
    validate_brief_format(format)?;
    Ok(match format {
        "json" => serde_json::to_string_pretty(report)?,
        "text" => render_brief_text(report),
        "prompt" => render_brief_prompt(report),
        _ => unreachable!("format is validated before report rendering"),
    })
}

fn build_brief_next_steps(
    overview: &MemoryOsOverviewReport,
    _profile: &MemoryOsProfileReport,
    _friction: &MemoryOsFrictionReport,
    continuity: &[MemoryOsNarrativeFinding],
    strategy_active: &[MemoryOsNarrativeFinding],
    active_work: &[MemoryOsNarrativeFinding],
) -> Vec<MemoryOsNarrativeFinding> {
    let mut steps = Vec::new();
    let mut active_seen = active_work
        .iter()
        .map(brief_finding_key)
        .collect::<HashSet<_>>();

    if let Some(strategy_step) = strategy_active.iter().find(|finding| {
        !brief_finding_has_command_noise(finding)
            && !active_seen.contains(&brief_finding_key(finding))
    }) {
        active_seen.insert(brief_finding_key(strategy_step));
        steps.push(MemoryOsNarrativeFinding {
            title: strategy_step.title.clone(),
            summary: display_text(&strategy_step.summary, 320),
            evidence: strategy_step.evidence.iter().take(2).cloned().collect(),
        });
    }

    if let Some(commitment) = continuity.iter().find(|finding| {
        !brief_finding_has_command_noise(finding)
            && !active_seen.contains(&brief_finding_key(finding))
    }) {
        active_seen.insert(brief_finding_key(commitment));
        steps.push(MemoryOsNarrativeFinding {
            title: commitment.title.clone(),
            summary: display_text(&commitment.summary, 320),
            evidence: commitment.evidence.iter().take(2).cloned().collect(),
        });
    }

    if let Some(active) = overview.active_work.iter().find(|finding| {
        active_finding_is_specific(finding) && !active_seen.contains(&brief_finding_key(finding))
    }) {
        steps.push(MemoryOsNarrativeFinding {
            title: active.title.clone(),
            summary: display_text(&active.summary, 320),
            evidence: active.evidence.iter().take(2).cloned().collect(),
        });
    }

    if steps.is_empty() {
        if let Some(primary_active) = active_work
            .iter()
            .find(|finding| !brief_finding_has_command_noise(finding))
        {
            steps.push(MemoryOsNarrativeFinding {
                title: "Continue active work".to_string(),
                summary: display_text(&primary_active.summary, 320),
                evidence: primary_active.evidence.iter().take(2).cloned().collect(),
            });
        }
    }

    steps.truncate(4);
    steps
}

fn brief_finding_key(finding: &MemoryOsNarrativeFinding) -> String {
    format!("{}:{}", finding.title, finding.summary).to_ascii_lowercase()
}

fn rank_brief_findings<'a>(
    findings: impl IntoIterator<Item = &'a MemoryOsNarrativeFinding>,
    limit: usize,
) -> Vec<MemoryOsNarrativeFinding> {
    let mut prose = Vec::new();
    let mut fallback = Vec::new();
    let mut seen = HashSet::new();

    for finding in findings {
        if brief_finding_has_command_noise(finding) {
            continue;
        }
        if !seen.insert((finding.title.clone(), finding.summary.clone())) {
            continue;
        }
        if brief_finding_is_prose(finding) {
            prose.push(finding.clone());
        } else {
            fallback.push(finding.clone());
        }
    }

    prose.extend(fallback);
    prose.truncate(limit);
    prose
}

fn build_brief_watchouts(
    overview: &MemoryOsOverviewReport,
    friction: &MemoryOsFrictionReport,
    strategy_watchouts: &[MemoryOsNarrativeFinding],
) -> Vec<MemoryOsNarrativeFinding> {
    let mut watchouts = Vec::new();

    watchouts.extend(
        strategy_watchouts
            .iter()
            .filter(|finding| !brief_finding_has_command_noise(finding))
            .take(2)
            .cloned(),
    );

    if let Some(pattern) = friction.likely_misunderstandings.first() {
        watchouts.push(MemoryOsNarrativeFinding {
            title: pattern.label.clone(),
            summary: display_text(&pattern.summary, 160),
            evidence: vec![format!("{} correction memories", pattern.count)],
        });
    }

    if overview
        .active_work
        .iter()
        .any(|finding| finding.summary.contains("Recent continue checkpoint"))
    {
        watchouts.push(MemoryOsNarrativeFinding {
            title: "Active-work detail is still shallow".to_string(),
            summary: "Some recent checkpoints still collapse to generic continue state, so the startup brief can identify the area but not always the precise task.".to_string(),
            evidence: vec!["Refresh the checkpoint narrative when the current task becomes clearer.".to_string()],
        });
    }

    watchouts
}

fn render_brief_text(report: &MemoryOsBriefReport) -> String {
    let mut lines = vec![
        "Context Memory Brief".to_string(),
        "--------------------".to_string(),
        format!("Scope: {}", report.scope),
        String::new(),
        "What I Know About You".to_string(),
        "---------------------".to_string(),
    ];
    lines.extend(render_brief_section(&report.what_i_know));
    lines.push(String::new());
    lines.push("How You Like To Work".to_string());
    lines.push("--------------------".to_string());
    lines.extend(render_brief_section(&report.how_you_work));
    lines.push(String::new());
    lines.push("What You're Working On".to_string());
    lines.push("----------------------".to_string());
    lines.extend(render_brief_section(&report.what_is_active));
    lines.push(String::new());
    lines.push("Next Best Steps".to_string());
    lines.push("---------------".to_string());
    lines.extend(render_brief_section(&report.next_steps));
    lines.push(String::new());
    lines.push("Watchouts".to_string());
    lines.push("---------".to_string());
    lines.extend(render_brief_section(&report.watchouts));
    lines.join("\n")
}

fn render_brief_prompt(report: &MemoryOsBriefReport) -> String {
    let mut lines = vec![
        format!(
            "<startup_memory_brief scope=\"{}\" generated_at=\"{}\">",
            report.scope, report.generated_at
        ),
        "<what_i_know>".to_string(),
    ];
    lines.extend(render_prompt_section(&report.what_i_know));
    lines.push("</what_i_know>".to_string());
    lines.push("<how_you_work>".to_string());
    lines.extend(render_prompt_section(&report.how_you_work));
    lines.push("</how_you_work>".to_string());
    lines.push("<what_is_active>".to_string());
    lines.extend(render_prompt_section(&report.what_is_active));
    lines.push("</what_is_active>".to_string());
    lines.push("<next_steps>".to_string());
    lines.extend(render_prompt_section(&report.next_steps));
    lines.push("</next_steps>".to_string());
    lines.push("<watchouts>".to_string());
    lines.extend(render_prompt_section(&report.watchouts));
    lines.push("</watchouts>".to_string());
    lines.push("<startup_rules>".to_string());
    lines.push(
        "- Open the session proactively using this brief before asking for more context."
            .to_string(),
    );
    lines.push("- Do not search recall or raw session history unless this brief is insufficient or historical evidence is explicitly requested.".to_string());
    lines.push("- Keep the opening concrete, low-noise, and grounded in Memory OS.".to_string());
    lines.push("</startup_rules>".to_string());
    lines.push("</startup_memory_brief>".to_string());
    lines.join("\n")
}

fn render_promotion_text(report: &MemoryOsPromotionReport) -> String {
    let mut lines = vec![
        "Memory OS Promotion".to_string(),
        "-------------------".to_string(),
        format!(
            "Strict gate: {}",
            if report.strict_gate_enabled {
                "enabled"
            } else {
                "disabled"
            }
        ),
        format!(
            "Read model: {}",
            if report.read_model_enabled {
                "enabled"
            } else {
                "disabled"
            }
        ),
        format!(
            "Resume cutover: {}",
            if report.resume_cutover_ready {
                "ready"
            } else {
                "blocked"
            }
        ),
        format!(
            "Handoff cutover: {}",
            if report.handoff_cutover_ready {
                "ready"
            } else {
                "blocked"
            }
        ),
        format!(
            "Required replay proof set: {} / {}",
            report.required_system, report.required_split
        ),
        format!("Matching results: {}", report.matching_result_count),
        format!(
            "Contaminated or unattested results: {}",
            report.contaminated_result_count
        ),
        format!("Decision: {}", report.decision_summary),
    ];

    if let Some(latest) = &report.latest_matching_result {
        lines.push(String::new());
        lines.push("Latest Matching Result".to_string());
        lines.push(format!("- Result: {}", latest.result));
        lines.push(format!("- Verified at: {}", latest.verification_time));
        lines.push(format!("- Proof id: {}", latest.verification_result_id));
        if let Some(reason) = &latest.reason {
            lines.push(format!("- Detail: {}", reason));
        }
        lines.push(format!("- Independent: {}", latest.independent));
        lines.push(format!(
            "- Contamination free: {}",
            latest.contamination_free
        ));
    }

    if !report.required_results.is_empty() {
        lines.push(String::new());
        lines.push("Required Proofs".to_string());
        for proof in &report.required_results {
            lines.push(format!(
                "- {}: {} (independent {}, contamination-free {})",
                proof.split, proof.result, proof.independent, proof.contamination_free
            ));
        }
    }

    if !report.missing_required_splits.is_empty() {
        lines.push(String::new());
        lines.push(format!(
            "Missing required splits: {}",
            report.missing_required_splits.join(", ")
        ));
    }

    lines.join("\n")
}

fn render_promotion_with_tracker(tracker: &Tracker, format: &str) -> Result<String> {
    validate_format(format)?;
    let report = tracker
        .get_memory_os_promotion_report()
        .context("Failed to load Memory OS promotion report")?;
    Ok(match format {
        "json" => serde_json::to_string_pretty(&report)?,
        "text" => render_promotion_text(&report),
        _ => unreachable!("format is validated before report rendering"),
    })
}

fn build_inspect_report(
    tracker: &Tracker,
    scope: MemoryOsInspectionScope,
    project_path: Option<&str>,
) -> Result<MemoryOsInspectReport> {
    let brief = build_brief_report(tracker, scope, project_path)?;
    let overview = tracker
        .get_memory_os_overview_report(scope, project_path)
        .context("Failed to load Memory OS overview report")?;
    let profile = tracker
        .get_memory_os_profile_report(scope, project_path)
        .context("Failed to load Memory OS profile report")?;
    let friction = tracker
        .get_memory_os_friction_report(scope, project_path)
        .context("Failed to load Memory OS friction report")?;
    let action_policy = tracker
        .get_memory_os_action_policy_view_report(scope, project_path)
        .context("Failed to load Memory OS action policy view")?;
    let promoted_assertions = tracker
        .get_memory_os_promoted_assertions(scope, project_path, 20)
        .context("Failed to load Memory OS promoted assertions")?;
    let evidence_events = tracker
        .get_memory_os_evidence_events(scope, project_path, 20)
        .context("Failed to load Memory OS evidence events")?;
    let promotion = tracker
        .get_memory_os_promotion_report()
        .context("Failed to load Memory OS promotion report")?;
    let trust = tracker
        .get_memory_os_trust_report(scope, project_path)
        .context("Failed to load Memory OS trust report")?;
    let onboarding = load_onboarding_status()?;
    let claude = build_claude_source_summary()?;
    let codex = build_codex_source_summary()?;

    let source_roots = claude
        .roots
        .iter()
        .chain(codex.roots.iter())
        .cloned()
        .collect::<Vec<_>>();
    let raw_sources = vec![
        summarize_raw_source("claude", &claude, onboarding.imported_ids.get("claude")),
        summarize_raw_source("codex", &codex, onboarding.imported_ids.get("codex")),
    ];
    let recall_imported_total = onboarding
        .status
        .imported_source_counts
        .iter()
        .find_map(|(source, count)| (source == "recall").then_some(*count))
        .unwrap_or(0);

    Ok(MemoryOsInspectReport {
        schema_version: "memory-os-inspect-v1".to_string(),
        generated_at: Utc::now().to_rfc3339(),
        scope,
        source_roots,
        raw_sources,
        import_pipeline: MemoryOsInspectImportPipeline {
            onboarding: onboarding.status,
            recall_imported_total,
        },
        compiled_memory: MemoryOsInspectCompiledMemory {
            brief,
            overview,
            profile,
            friction,
            action_policy,
            promoted_assertions,
            evidence_events,
            promotion,
            trust,
        },
    })
}

fn build_doctor_report(
    tracker: &Tracker,
    scope: MemoryOsInspectionScope,
    _project_path: Option<&str>,
) -> Result<MemoryOsDoctorReport> {
    let onboarding = tracker
        .get_memory_os_onboarding_state_fast()
        .context("Failed to load Memory OS import status for doctor")?;
    let promotion = tracker
        .get_memory_os_promotion_report()
        .context("Failed to load Memory OS promotion for doctor")?;
    let strategy_scope = doctor_strategy_scope();
    let strategy_inspect = strategy::inspect(&strategy::StrategyReadOptions {
        scope: strategy_scope.clone(),
    });
    let strategy_metrics = match &strategy_inspect {
        Ok(report) => doctor_load_strategy_metrics(report),
        Err(err) => Err(anyhow!("strategy inspect failed: {err}")),
    };

    let corpus = build_doctor_corpus(&onboarding);
    let strategy_signal_count = strategy_inspect
        .as_ref()
        .map(|report| doctor_strategy_signal_count(&report.kernel))
        .unwrap_or(0);
    let strategy_metrics_empty = strategy_metrics
        .as_ref()
        .map(doctor_strategy_metrics_empty)
        .unwrap_or(false);

    let mut signal_quality = Vec::new();
    signal_quality.push(MemoryOsDoctorCheck {
        name: "Corpus import is current".to_string(),
        status: if corpus.imported_sessions_total == 0 {
            "fail"
        } else if corpus.onboarding_status != "completed"
            || corpus.latest_import_freshness.starts_with("stale")
        {
            "warn"
        } else {
            "pass"
        }
        .to_string(),
        summary: format!(
            "{} sessions imported from compiled onboarding state; raw coverage is not scanned in fast Doctor mode.",
            corpus.imported_sessions_total
        ),
        evidence: vec![
            format!("onboarding: {}", corpus.onboarding_status),
            format!(
                "latest import completion: {}",
                corpus
                    .latest_import_completed_at
                    .as_deref()
                    .unwrap_or("not completed")
            ),
            format!("raw coverage: {}", corpus.raw_coverage_mode),
        ],
    });
    signal_quality.push(MemoryOsDoctorCheck {
        name: "Compiled Memory OS substrate exists".to_string(),
        status: if corpus.checkpoint_count > 0 && corpus.journal_event_count > 0 {
            "pass"
        } else if corpus.journal_event_count > 0 {
            "warn"
        } else {
            "fail"
        }
        .to_string(),
        summary: format!(
            "{} journal events and {} packet checkpoints are available for compiled surfaces.",
            corpus.journal_event_count, corpus.checkpoint_count
        ),
        evidence: vec![
            "Fast Doctor does not render brief/friction text; it checks whether the substrate exists."
                .to_string(),
            "Run `munin memory-os brief` or `munin memory-os friction` for user-facing output."
                .to_string(),
        ],
    });
    signal_quality.push(MemoryOsDoctorCheck {
        name: "Strategy CLI routing is healthy".to_string(),
        status: if strategy_signal_count > 0 {
            "pass"
        } else {
            "fail"
        }
        .to_string(),
        summary: if strategy_signal_count > 0 {
            format!(
                "Scope `{}` resolves to {} strategy goals/KPIs/initiatives/constraints/assumptions.",
                strategy_scope, strategy_signal_count
            )
        } else {
            format!("Scope `{strategy_scope}` did not resolve to a populated strategy kernel.")
        },
        evidence: match &strategy_inspect {
            Ok(report) => vec![
                format!("goals: {}", report.kernel.goals.len()),
                format!("kpis: {}", report.kernel.kpis.len()),
                format!("initiatives: {}", report.kernel.initiatives.len()),
            ],
            Err(err) => vec![err.to_string()],
        },
    });
    signal_quality.push(MemoryOsDoctorCheck {
        name: "Strategy metrics are instrumented".to_string(),
        status: if strategy_metrics.is_ok() && !strategy_metrics_empty {
            "pass"
        } else if strategy_metrics.is_ok() {
            "warn"
        } else {
            "fail"
        }
        .to_string(),
        summary: if strategy_metrics_empty {
            "Ingested KPI slots are present but current values are missing, so KPIs cannot turn green/yellow yet."
                .to_string()
        } else if strategy_metrics.is_ok() {
            "Metrics snapshot has at least one instrumented KPI, dependency, instrumentation flag, or initiative signal."
                .to_string()
        } else {
            format!("Could not read strategy metrics for scope `{strategy_scope}`.")
        },
        evidence: match &strategy_metrics {
            Ok(metrics) => doctor_strategy_metrics_evidence(metrics),
            Err(err) => vec![err.to_string()],
        },
    });
    signal_quality.push(MemoryOsDoctorCheck {
        name: "Promotion gate is inspectable".to_string(),
        status: if promotion.eligible { "pass" } else { "warn" }.to_string(),
        summary: promotion.decision_summary.clone(),
        evidence: promotion
            .required_results
            .iter()
            .take(3)
            .map(|result| format!("{} / {}: {}", result.split, result.system, result.result))
            .collect(),
    });

    let top_pipeline_problems = build_doctor_pipeline_problems(&corpus, &signal_quality);
    let overall_status = doctor_overall_status(&signal_quality, &top_pipeline_problems);
    let recommended_permanent_fix = top_pipeline_problems
        .first()
        .map(|problem| problem.permanent_fix.clone())
        .unwrap_or_else(|| {
            "Keep running Memory OS Doctor after Memory OS surface changes; no immediate pipeline fix is required.".to_string()
        });

    Ok(MemoryOsDoctorReport {
        schema_version: "memory-os-doctor-v1".to_string(),
        generated_at: Utc::now().to_rfc3339(),
        scope,
        overall_status,
        corpus,
        signal_quality,
        top_pipeline_problems,
        recommended_permanent_fix,
    })
}

fn build_doctor_corpus(
    onboarding: &crate::core::memory_os::MemoryOsOnboardingState,
) -> MemoryOsDoctorCorpus {
    let source_health = onboarding
        .imported_sources
        .iter()
        .map(|source| MemoryOsDoctorCorpusSource {
            source: source.source.clone(),
            imported_sessions: source.sessions,
            import_status: if source.sessions == 0 {
                "not-imported".to_string()
            } else {
                "imported".to_string()
            },
        })
        .collect();

    MemoryOsDoctorCorpus {
        onboarding_status: onboarding.status.clone(),
        onboarding_completed_at: onboarding.completed_at.clone(),
        sessions_processed: onboarding.sessions_processed,
        shells_ingested: onboarding.shells_ingested,
        corrections_ingested: onboarding.corrections_ingested,
        imported_sessions_total: onboarding.sessions_processed,
        imported_shell_executions_total: onboarding.shells_ingested,
        checkpoint_count: onboarding.checkpoint_count,
        journal_event_count: onboarding.journal_event_count,
        latest_import_completed_at: onboarding.completed_at.clone(),
        latest_import_freshness: doctor_import_freshness_status(onboarding.completed_at.as_deref()),
        raw_coverage_mode:
            "fast compiled-state check; run `munin memory-os inspect --format json` for raw coverage"
                .to_string(),
        source_health,
    }
}

fn build_doctor_pipeline_problems(
    corpus: &MemoryOsDoctorCorpus,
    checks: &[MemoryOsDoctorCheck],
) -> Vec<MemoryOsDoctorProblem> {
    let mut problems = Vec::new();
    if corpus.imported_sessions_total == 0 {
        problems.push(MemoryOsDoctorProblem {
            severity: "high".to_string(),
            title: "No imported session corpus detected".to_string(),
            summary: "Memory OS cannot prove it has imported Claude/Codex sessions into compiled state."
                .to_string(),
            permanent_fix:
                "Run or repair the session backfill before trusting any Memory OS brief or friction output."
                    .to_string(),
            evidence: vec![format!("onboarding: {}", corpus.onboarding_status)],
        });
    }
    if corpus.onboarding_status != "completed" {
        problems.push(MemoryOsDoctorProblem {
            severity: "medium".to_string(),
            title: "Session import is not complete".to_string(),
            summary: format!(
                "Session backfill status is '{}', so compiled memory may be partial.",
                corpus.onboarding_status
            ),
            permanent_fix:
                "Complete the session backfill, then record the completion timestamp before serving startup surfaces."
                    .to_string(),
            evidence: vec![format!(
                "latest import completion: {}",
                corpus
                    .latest_import_completed_at
                    .as_deref()
                    .unwrap_or("not completed")
            )],
        });
    }
    if corpus.latest_import_freshness.starts_with("stale") {
        problems.push(MemoryOsDoctorProblem {
            severity: "medium".to_string(),
            title: "Session import may be stale".to_string(),
            summary: format!(
                "Latest completed import is {}.",
                corpus.latest_import_freshness
            ),
            permanent_fix:
                "Refresh Memory OS imports before answering user/profile/current-work questions from compiled state."
                    .to_string(),
            evidence: vec![format!(
                "completed_at: {}",
                corpus
                    .latest_import_completed_at
                    .as_deref()
                    .unwrap_or("not completed")
            )],
        });
    }

    for check in checks.iter().filter(|check| check.status != "pass") {
        match check.name.as_str() {
            "Compiled Memory OS substrate exists" => problems.push(MemoryOsDoctorProblem {
                severity: if check.status == "fail" { "high" } else { "medium" }.to_string(),
                title: "Compiled Memory OS substrate is incomplete".to_string(),
                summary: check.summary.clone(),
                permanent_fix:
                    "Repair packet checkpoint/journal projection writes before debugging brief wording; the read surfaces need a healthy substrate first."
                        .to_string(),
                evidence: check.evidence.clone(),
            }),
            "Strategy CLI routing is healthy" => problems.push(MemoryOsDoctorProblem {
                severity: "high".to_string(),
                title: "Strategy CLI cannot reach the populated strategy kernel".to_string(),
                summary: check.summary.clone(),
                permanent_fix:
                    "Resolve strategy scopes from existing store directories as well as config.toml, and rehydrate empty kernels from their registered strategic-plan.context.json artifact."
                        .to_string(),
                evidence: check.evidence.clone(),
            }),
            "Strategy metrics are instrumented" => problems.push(MemoryOsDoctorProblem {
                severity: if check.status == "fail" { "high" } else { "medium" }.to_string(),
                title: "Strategy metrics need current values".to_string(),
                summary: check.summary.clone(),
                permanent_fix:
                    "Pick an instrumentation source for each KPI, then write current metric values into metrics.json so strategy status can mark KPIs green/yellow/red."
                        .to_string(),
                evidence: check.evidence.clone(),
            }),
            "Promotion gate is inspectable" => problems.push(MemoryOsDoctorProblem {
                severity: "low".to_string(),
                title: "Promotion gate is not fully proven".to_string(),
                summary: check.summary.clone(),
                permanent_fix:
                    "Keep promotion details in Doctor/inspect surfaces, not in the startup brief, until independent proof rows are present."
                        .to_string(),
                evidence: check.evidence.clone(),
            }),
            _ => {}
        }
    }

    problems.sort_by(|left, right| {
        doctor_severity_rank(&right.severity)
            .cmp(&doctor_severity_rank(&left.severity))
            .then(left.title.cmp(&right.title))
    });
    problems.truncate(8);
    problems
}

fn render_doctor_text(report: &MemoryOsDoctorReport) -> String {
    let mut lines = Vec::new();
    lines.push("Memory OS Doctor".to_string());
    lines.push("----------------".to_string());
    lines.push(format!("Status: {}", report.overall_status));
    lines.push(format!("Scope: {}", report.scope));
    lines.push(format!("Generated: {}", report.generated_at));
    lines.push(String::new());
    lines.push("Corpus".to_string());
    lines.push(format!(
        "- onboarding: {} | sessions: {} | shells: {} | corrections: {}",
        report.corpus.onboarding_status,
        report.corpus.sessions_processed,
        report.corpus.shells_ingested,
        report.corpus.corrections_ingested
    ));
    lines.push(format!(
        "- compiled import: {} sessions | {} shells | checkpoints: {} | journal events: {}",
        report.corpus.imported_sessions_total,
        report.corpus.imported_shell_executions_total,
        report.corpus.checkpoint_count,
        report.corpus.journal_event_count
    ));
    lines.push(format!(
        "- latest import: {} | {}",
        report
            .corpus
            .latest_import_completed_at
            .as_deref()
            .unwrap_or("not completed"),
        report.corpus.latest_import_freshness
    ));
    lines.push(format!(
        "- raw coverage: {}",
        report.corpus.raw_coverage_mode
    ));
    for source in &report.corpus.source_health {
        lines.push(format!(
            "  - {}: {} imported sessions | {}",
            source.source, source.imported_sessions, source.import_status
        ));
    }
    lines.push(String::new());
    lines.push("Signal Quality".to_string());
    for check in &report.signal_quality {
        lines.push(format!(
            "- [{}] {}: {}",
            check.status, check.name, check.summary
        ));
        for evidence in check
            .evidence
            .iter()
            .filter(|value| !value.is_empty())
            .take(3)
        {
            lines.push(format!("  evidence: {}", evidence));
        }
    }
    lines.push(String::new());
    lines.push("Top Pipeline Problems".to_string());
    if report.top_pipeline_problems.is_empty() {
        lines.push("- none detected".to_string());
    } else {
        for (index, problem) in report.top_pipeline_problems.iter().enumerate() {
            lines.push(format!(
                "{}. [{}] {}",
                index + 1,
                problem.severity,
                problem.title
            ));
            lines.push(format!("   {}", problem.summary));
            lines.push(format!("   permanent fix: {}", problem.permanent_fix));
            for evidence in problem
                .evidence
                .iter()
                .filter(|value| !value.is_empty())
                .take(3)
            {
                lines.push(format!("   evidence: {}", evidence));
            }
        }
    }
    lines.push(String::new());
    lines.push("Recommended Permanent Fix".to_string());
    lines.push(format!("- {}", report.recommended_permanent_fix));
    lines.join("\n")
}

fn doctor_strategy_scope() -> String {
    crate::core::config::Config::load()
        .ok()
        .and_then(|config| config.strategy.configured_scope_name(None))
        .unwrap_or_else(|| "sitesorted-business".to_string())
}

fn doctor_strategy_signal_count(kernel: &strategy::StrategyKernel) -> usize {
    kernel.goals.len()
        + kernel.kpis.len()
        + kernel.initiatives.len()
        + kernel.constraints.len()
        + kernel.assumptions.len()
}

fn doctor_load_strategy_metrics(
    report: &strategy::StrategyInspectReport,
) -> Result<strategy::StrategyMetricsSnapshot> {
    let path = &report.registry.metrics_path;
    let mut metrics = if path.exists() {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read strategy metrics {}", path.display()))?;
        serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse strategy metrics {}", path.display()))?
    } else {
        strategy::StrategyMetricsSnapshot::default()
    };
    for kpi in &report.kernel.kpis {
        metrics
            .kpis
            .entry(kpi.metric_key.clone())
            .or_insert_with(|| strategy::StrategyMetricRecord {
                current: None,
                unit: kpi.unit.clone(),
                updated_at: None,
            });
    }
    Ok(metrics)
}

fn doctor_strategy_metrics_empty(metrics: &strategy::StrategyMetricsSnapshot) -> bool {
    metrics.kpis.values().all(|record| record.current.is_none())
        && metrics.instrumentation.is_empty()
        && metrics.dependency_states.is_empty()
        && metrics.initiatives.is_empty()
}

fn doctor_strategy_metrics_evidence(metrics: &strategy::StrategyMetricsSnapshot) -> Vec<String> {
    vec![
        format!("kpis: {}", metrics.kpis.len()),
        format!("instrumentation: {}", metrics.instrumentation.len()),
        format!("dependency_states: {}", metrics.dependency_states.len()),
        format!("initiatives: {}", metrics.initiatives.len()),
    ]
}

fn doctor_import_freshness_status(completed_at: Option<&str>) -> String {
    let Some(completed_at) = completed_at else {
        return "not-completed".to_string();
    };
    let Some(parsed) = parse_rfc3339_utc(completed_at) else {
        return "freshness-unknown".to_string();
    };
    let age_days = Utc::now().signed_duration_since(parsed).num_days();
    if age_days <= 1 {
        format!("fresh ({age_days}d)")
    } else if age_days <= 7 {
        format!("aging ({age_days}d)")
    } else {
        format!("stale ({age_days}d)")
    }
}

fn parse_rfc3339_utc(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|parsed| parsed.with_timezone(&Utc))
}

fn doctor_overall_status(
    checks: &[MemoryOsDoctorCheck],
    problems: &[MemoryOsDoctorProblem],
) -> String {
    if checks.iter().any(|check| check.status == "fail")
        || problems.iter().any(|problem| problem.severity == "high")
    {
        "fail".to_string()
    } else if checks.iter().any(|check| check.status == "warn") || !problems.is_empty() {
        "warn".to_string()
    } else {
        "pass".to_string()
    }
}

fn doctor_severity_rank(severity: &str) -> i32 {
    match severity {
        "high" => 3,
        "medium" => 2,
        "low" => 1,
        _ => 0,
    }
}

fn load_onboarding_status() -> Result<LoadedOnboardingStatus> {
    let base = dirs::data_local_dir()
        .or_else(dirs::data_dir)
        .ok_or_else(|| anyhow!("could not determine local data directory"))?;
    let path = base
        .join("context")
        .join("memory_os_session_onboarding.json");
    if !path.exists() {
        return Ok(LoadedOnboardingStatus {
            status: MemoryOsInspectOnboardingStatus {
                schema_version: "missing".to_string(),
                status: "missing".to_string(),
                started_at: None,
                completed_at: None,
                sessions_processed: 0,
                shells_ingested: 0,
                corrections_ingested: 0,
                imported_source_counts: Vec::new(),
            },
            imported_ids: HashMap::new(),
        });
    }
    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let value: serde_json::Value = serde_json::from_str(&content)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    let processed = value
        .get("processed_session_ids")
        .and_then(|entries| entries.as_array())
        .cloned()
        .unwrap_or_default();

    let mut ordered = Vec::new();
    let mut seen = HashSet::new();
    let mut imported_ids: HashMap<String, HashSet<String>> = HashMap::new();
    for item in &processed {
        let (source, session_id) = item
            .as_str()
            .and_then(|entry| {
                entry
                    .split_once(':')
                    .map(|(source, session_id)| (source.to_string(), session_id.to_string()))
            })
            .unwrap_or_else(|| ("unknown".to_string(), item.to_string()));
        imported_ids
            .entry(source.clone())
            .or_default()
            .insert(session_id);
        if seen.insert(source.clone()) {
            ordered.push(source);
        }
    }

    let imported_source_counts = ordered
        .iter()
        .map(|source| {
            (
                source.clone(),
                imported_ids.get(source).map(|ids| ids.len()).unwrap_or(0),
            )
        })
        .collect::<Vec<_>>();

    let completed_at = value
        .get("completed_at")
        .and_then(|item| item.as_str())
        .map(str::to_string);

    Ok(LoadedOnboardingStatus {
        status: MemoryOsInspectOnboardingStatus {
            schema_version: value
                .get("schema_version")
                .and_then(|item| item.as_str())
                .unwrap_or("unknown")
                .to_string(),
            status: if completed_at.is_some() {
                "completed".to_string()
            } else {
                "pending".to_string()
            },
            started_at: value
                .get("started_at")
                .and_then(|item| item.as_str())
                .map(str::to_string),
            completed_at,
            sessions_processed: processed.len(),
            shells_ingested: value
                .get("shells_ingested")
                .and_then(|item| item.as_u64())
                .unwrap_or(0) as usize,
            corrections_ingested: value
                .get("corrections_ingested")
                .and_then(|item| item.as_u64())
                .unwrap_or(0) as usize,
            imported_source_counts,
        },
        imported_ids,
    })
}

fn render_inspect_text(report: &MemoryOsInspectReport) -> String {
    let mut lines = vec![
        "Memory OS Inspect".to_string(),
        "-----------------".to_string(),
        format!("Schema: {}", report.schema_version),
        format!("Generated: {}", report.generated_at),
        format!("Scope: {}", report.scope),
        String::new(),
        "Raw Sources".to_string(),
        "-----------".to_string(),
    ];

    for source in &report.raw_sources {
        lines.push(format!("{}:", capitalize(&source.source)));
        lines.push(format!(
            "- raw current {} | imported total {} | missing current {} | imported-not-current {}",
            source.raw_current,
            source.imported_total,
            source.missing_current,
            source.imported_not_current
        ));
        lines.push(format!(
            "- excluded subagents {} | parse failures {} | prompt-only {} | shell-history {}",
            source.excluded_subagents,
            source.parse_failures,
            source.prompt_only_sessions,
            source.shell_history_sessions
        ));
        lines.push(format!(
            "- oldest {} | newest {}",
            source.oldest_session.as_deref().unwrap_or("n/a"),
            source.newest_session.as_deref().unwrap_or("n/a")
        ));
        for origin in &source.source_origins {
            lines.push(format!(
                "- origin {} | root {} | sessions {}",
                origin.origin, origin.root, origin.sessions
            ));
        }
        if !source.missing_current_ids_sample.is_empty() {
            lines.push(format!(
                "- missing current sample: {}",
                source.missing_current_ids_sample.join(", ")
            ));
        }
        if !source.imported_not_current_ids_sample.is_empty() {
            lines.push(format!(
                "- imported-not-current sample: {}",
                source.imported_not_current_ids_sample.join(", ")
            ));
        }
        if !source.parse_failure_ids_sample.is_empty() {
            lines.push(format!(
                "- parse-failure sample: {}",
                source.parse_failure_ids_sample.join(", ")
            ));
        }
        lines.push(String::new());
    }

    lines.push(String::new());
    lines.push("Import Pipeline".to_string());
    lines.push("---------------".to_string());
    lines.push(format!(
        "- onboarding {} | schema {}",
        report.import_pipeline.onboarding.status, report.import_pipeline.onboarding.schema_version
    ));
    lines.push(format!(
        "- imported sessions {} | shells {} | corrections {}",
        report.import_pipeline.onboarding.sessions_processed,
        report.import_pipeline.onboarding.shells_ingested,
        report.import_pipeline.onboarding.corrections_ingested
    ));
    lines.push(format!(
        "- started {} | completed {}",
        report
            .import_pipeline
            .onboarding
            .started_at
            .as_deref()
            .unwrap_or("n/a"),
        report
            .import_pipeline
            .onboarding
            .completed_at
            .as_deref()
            .unwrap_or("n/a")
    ));
    for (source, count) in &report.import_pipeline.onboarding.imported_source_counts {
        lines.push(format!("- imported {} {}", source, count));
    }
    lines.push(format!(
        "- recall imported total {}",
        report.import_pipeline.recall_imported_total
    ));
    lines.push(String::new());
    lines.push("Compiled Memory".to_string());
    lines.push("----------------".to_string());
    lines.push("What It Knows".to_string());
    lines.extend(render_findings_with_evidence(
        &report.compiled_memory.brief.what_i_know,
        3,
    ));
    lines.push(String::new());
    lines.push("How You Work".to_string());
    lines.extend(render_findings_with_evidence(
        &report.compiled_memory.brief.how_you_work,
        3,
    ));
    lines.push(String::new());
    lines.push("What Is Active".to_string());
    lines.extend(render_findings_with_evidence(
        &report.compiled_memory.brief.what_is_active,
        3,
    ));
    lines.push(String::new());
    lines.push("Next Steps".to_string());
    lines.extend(render_findings_with_evidence(
        &report.compiled_memory.brief.next_steps,
        2,
    ));
    lines.push(String::new());
    lines.push("Watchouts".to_string());
    lines.extend(render_findings_with_evidence(
        &report.compiled_memory.brief.watchouts,
        2,
    ));
    lines.push(String::new());
    lines.push("Top Projects".to_string());
    if report.compiled_memory.overview.top_projects.is_empty() {
        lines.push("- none".to_string());
    } else {
        for project in report.compiled_memory.overview.top_projects.iter().take(10) {
            lines.push(format!(
                "- {} | sessions {} | shells {} | repo {}",
                project.project_path,
                project.sessions,
                project.shell_executions,
                project.repo_label
            ));
        }
    }
    lines.push(String::new());
    lines.push("Top Correction Patterns".to_string());
    if report
        .compiled_memory
        .overview
        .top_correction_patterns
        .is_empty()
    {
        lines.push("- none".to_string());
    } else {
        for pattern in report
            .compiled_memory
            .overview
            .top_correction_patterns
            .iter()
            .take(10)
        {
            lines.push(format!(
                "- {} -> {} | {}x | {}",
                pattern.wrong_command, pattern.corrected_command, pattern.count, pattern.error_kind
            ));
        }
    }
    lines.push(String::new());
    lines.push("Behavior Changes".to_string());
    if report.compiled_memory.friction.behavior_changes.is_empty() {
        lines.push("- none".to_string());
    } else {
        for change in report
            .compiled_memory
            .friction
            .behavior_changes
            .iter()
            .take(10)
        {
            lines.push(format!(
                "- {}: {}",
                change.target_agent,
                display_text(&change.change, 180)
            ));
            for evidence in change.evidence.iter().take(3) {
                lines.push(format!("  evidence: {}", evidence));
            }
        }
    }
    lines.push(String::new());
    lines.push("Misunderstandings".to_string());
    if report
        .compiled_memory
        .friction
        .likely_misunderstandings
        .is_empty()
    {
        lines.push("- none".to_string());
    } else {
        for misunderstanding in report
            .compiled_memory
            .friction
            .likely_misunderstandings
            .iter()
            .take(10)
        {
            lines.push(format!(
                "- {} | {}x | {}",
                misunderstanding.label, misunderstanding.count, misunderstanding.summary
            ));
        }
    }
    let (rejections, assertions): (
        Vec<&MemoryOsPromotedAssertionRecord>,
        Vec<&MemoryOsPromotedAssertionRecord>,
    ) = report
        .compiled_memory
        .promoted_assertions
        .iter()
        .partition(|record| record.category == "rejection");

    let render_assertion =
        |lines: &mut Vec<String>, assertion: &MemoryOsPromotedAssertionRecord| {
            lines.push(format!(
                "- [{}|{}|{}] {}",
                assertion.scope, assertion.category, assertion.status, assertion.statement
            ));
            lines.push(format!("  normalized: {}", assertion.normalized_claim));
            lines.push(format!(
                "  confidence: {} | first-promoted: {}",
                assertion.confidence, assertion.first_promoted_at
            ));
            lines.push(format!(
                "  review-after: {} | expires: {}",
                assertion.review_after.as_deref().unwrap_or("not-scheduled"),
                assertion.expires_at.as_deref().unwrap_or("not-set")
            ));
            if let Some(last_reviewed_at) = &assertion.last_reviewed_at {
                lines.push(format!("  dependency-reviewed: {}", last_reviewed_at));
            }
            if let Some(demotion_reason) = &assertion.demotion_reason {
                lines.push(format!("  demotion-reason: {}", demotion_reason));
            }
            if let Some(target) = &assertion.scope_target {
                lines.push(format!("  target: {}", target));
            }
            for evidence in assertion.supporting_evidence.iter().take(2) {
                lines.push(format!("  evidence: {}", evidence));
            }
        };

    lines.push(String::new());
    lines.push("Promoted Assertions".to_string());
    if assertions.is_empty() {
        lines.push("- none".to_string());
    } else {
        for assertion in assertions.iter().take(10) {
            render_assertion(&mut lines, assertion);
        }
    }
    lines.push(String::new());
    lines.push("Recent Rejections".to_string());
    if rejections.is_empty() {
        lines.push("- none".to_string());
    } else {
        for rejection in rejections.iter().take(10) {
            render_assertion(&mut lines, rejection);
        }
    }
    lines.push(String::new());
    lines.push("Evidence Events".to_string());
    if report.compiled_memory.evidence_events.is_empty() {
        lines.push("- none".to_string());
    } else {
        for event in report.compiled_memory.evidence_events.iter().take(10) {
            lines.push(format!(
                "- [{}|{}] {}",
                event.lane,
                event.derivation_kind,
                display_text(&event.summary, 140)
            ));
            lines.push(format!(
                "  source: {} | root: {}",
                event.source_record_id, event.root_source_id
            ));
        }
    }
    lines.push(String::new());
    lines.push("Action Policy".to_string());
    if report.compiled_memory.action_policy.candidates.is_empty()
        && report.compiled_memory.action_policy.rules.is_empty()
        && report.compiled_memory.action_policy.approvals.is_empty()
        && report
            .compiled_memory
            .action_policy
            .hook_capabilities
            .is_empty()
    {
        lines.push("- none".to_string());
    } else {
        lines.push(format!(
            "- candidates {} | behavior changes {} | approvals {} | hook lanes {}",
            report.compiled_memory.action_policy.candidate_count,
            report.compiled_memory.action_policy.behavior_change_count,
            report.compiled_memory.action_policy.approvals_count,
            report.compiled_memory.action_policy.hook_capabilities.len()
        ));
        for candidate in report
            .compiled_memory
            .action_policy
            .candidates
            .iter()
            .take(10)
        {
            lines.push(format!(
                "- [{}|{}|{}] {}",
                candidate.status, candidate.actuator_type, candidate.confidence, candidate.title
            ));
            lines.push(format!("  {}", display_text(&candidate.summary, 180)));
            lines.push(format!(
                "  precedents: {} | success: {} | failure: {} | aging: {}",
                candidate.precedent_count,
                candidate.success_count,
                candidate.failure_count,
                candidate.aging_status
            ));
            if let Some(command) = &candidate.action.command_sig {
                lines.push(format!("  command: {}", command));
            }
            if let Some(recommendation) = &candidate.action.recommendation {
                lines.push(format!(
                    "  recommendation: {}",
                    display_text(recommendation, 160)
                ));
            }
            lines.push(format!(
                "  review-after: {} | expires: {} | policy: {}",
                candidate.review_after.as_deref().unwrap_or("not-scheduled"),
                candidate.expires_at.as_deref().unwrap_or("not-set"),
                candidate.lifecycle_policy.as_deref().unwrap_or("expiring")
            ));
        }
        for rule in report.compiled_memory.action_policy.rules.iter().take(10) {
            lines.push(format!(
                "- [{}|{}|{}|{}] {}",
                rule.scope, rule.action_kind, rule.strength, rule.confidence, rule.title
            ));
            lines.push(format!("  {}", display_text(&rule.summary, 180)));
            if let Some(command) = &rule.suggested_command {
                lines.push(format!("  command: {}", command));
            }
            if let Some(recommendation) = &rule.recommendation {
                lines.push(format!(
                    "  recommendation: {}",
                    display_text(recommendation, 160)
                ));
            }
            if let Some(target_agent) = &rule.target_agent {
                lines.push(format!("  target: {}", target_agent));
            }
            lines.push(format!(
                "  review-after: {} | expires: {} | policy: {} | aging: {}",
                rule.review_after.as_deref().unwrap_or("not-scheduled"),
                rule.expires_at.as_deref().unwrap_or("not-set"),
                rule.lifecycle_policy.as_deref().unwrap_or("expiring"),
                rule.aging_status
            ));
            for evidence in rule.supporting_evidence.iter().take(3) {
                lines.push(format!("  evidence: {}", evidence));
            }
        }
    }
    if !report.compiled_memory.action_policy.approvals.is_empty() {
        lines.push("Approval Jobs".to_string());
        for approval in report
            .compiled_memory
            .action_policy
            .approvals
            .iter()
            .take(10)
        {
            lines.push(format!(
                "- [{}|{}|{}] {}",
                approval.status, approval.item_kind, approval.local_date, approval.title
            ));
            lines.push(format!("  {}", display_text(&approval.summary, 180)));
            if let Some(effect) = &approval.expected_effect {
                lines.push(format!("  expected-effect: {}", display_text(effect, 160)));
            }
            lines.push(format!(
                "  review-after: {} | expires: {}",
                approval.review_after.as_deref().unwrap_or("not-scheduled"),
                approval.expires_at.as_deref().unwrap_or("not-set")
            ));
            if let Some(last_reviewed_at) = &approval.last_reviewed_at {
                lines.push(format!("  last-reviewed: {}", last_reviewed_at));
            }
            if let Some(reason) = &approval.closure_reason {
                lines.push(format!("  closure-reason: {}", reason));
            }
        }
    }
    if !report
        .compiled_memory
        .action_policy
        .hook_capabilities
        .is_empty()
    {
        lines.push("Hook Capability".to_string());
        for capability in &report.compiled_memory.action_policy.hook_capabilities {
            lines.push(format!(
                "- [{}] {} | rewrite {} | ask {} | updated-input {} | fallback {}",
                capability.status,
                capability.surface,
                capability.rewrite_support,
                capability.ask_support,
                capability.updated_input_support,
                capability.fallback_mode
            ));
        }
    }
    lines.push(String::new());
    lines.push("Trust".to_string());
    lines.push(format!(
        "- observations {} | secrets {} | pii {} | must-not-packetize {}",
        report.compiled_memory.trust.observation_count,
        report.compiled_memory.trust.secret_count,
        report.compiled_memory.trust.pii_count,
        report.compiled_memory.trust.must_not_packetize_count
    ));
    lines.push(String::new());
    lines.push("Promotion".to_string());
    lines.push(format!(
        "- strict gate {} | resume {} | handoff {}",
        if report.compiled_memory.promotion.strict_gate_enabled {
            "enabled"
        } else {
            "disabled"
        },
        if report.compiled_memory.promotion.resume_cutover_ready {
            "ready"
        } else {
            "blocked"
        },
        if report.compiled_memory.promotion.handoff_cutover_ready {
            "ready"
        } else {
            "blocked"
        }
    ));
    lines.push(format!(
        "- {}",
        report.compiled_memory.promotion.decision_summary
    ));

    lines.join("\n")
}

fn build_claude_source_summary() -> Result<RawSourceSummary> {
    let root = dirs::home_dir()
        .ok_or_else(|| anyhow!("could not determine home directory"))?
        .join(".claude")
        .join("projects");
    let mut raw_ids = HashSet::new();
    let mut excluded_subagents = HashSet::new();
    if root.exists() {
        for entry in walkdir::WalkDir::new(&root)
            .follow_links(false)
            .into_iter()
            .filter_map(|entry| entry.ok())
        {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
                continue;
            }
            let session_id = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or("")
                .to_string();
            if session_id.is_empty() {
                continue;
            }
            if path
                .to_string_lossy()
                .to_ascii_lowercase()
                .contains("subagents")
            {
                excluded_subagents.insert(session_id);
            } else {
                raw_ids.insert(session_id);
            }
        }
    }

    let parsed_sessions = load_sessions(None, None, None, Some(SessionSource::Claude))?
        .into_iter()
        .map(|session| ParsedSessionSummary {
            session_id: session.session_id,
            started_at: session.started_at,
            prompt_count: session.user_prompts.len(),
            shell_count: session.shells.len(),
        })
        .collect::<Vec<_>>();

    Ok(RawSourceSummary {
        roots: vec![root.display().to_string()],
        raw_ids: raw_ids.clone(),
        excluded_subagents,
        parsed_sessions,
        origins: vec![MemoryOsInspectSourceOrigin {
            origin: "claude.projects".to_string(),
            root: root.display().to_string(),
            sessions: raw_ids.len(),
        }],
    })
}

fn build_codex_source_summary() -> Result<RawSourceSummary> {
    let home = dirs::home_dir().ok_or_else(|| anyhow!("could not determine home directory"))?;
    let sessions_root = home.join(".codex").join("sessions");
    let history_path = home.join(".codex").join("history.jsonl");

    let mut structured_top_level_ids = HashSet::new();
    let mut excluded_subagents = HashSet::new();
    if sessions_root.exists() {
        for entry in walkdir::WalkDir::new(&sessions_root)
            .follow_links(false)
            .into_iter()
            .filter_map(|entry| entry.ok())
        {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let Some((session_id, is_subagent)) = read_codex_session_meta(path)? else {
                continue;
            };
            if is_subagent {
                excluded_subagents.insert(session_id);
            } else {
                structured_top_level_ids.insert(session_id);
            }
        }
    }

    let mut history_ids = HashSet::new();
    if history_path.exists() {
        let file = File::open(&history_path)
            .with_context(|| format!("failed to open {}", history_path.display()))?;
        let reader = BufReader::new(file);
        for line in reader.lines() {
            let line = match line {
                Ok(line) => line,
                Err(_) => continue,
            };
            let value: Value = match serde_json::from_str(&line) {
                Ok(value) => value,
                Err(_) => continue,
            };
            if let Some(session_id) = value.get("session_id").and_then(|item| item.as_str()) {
                history_ids.insert(session_id.to_string());
            }
        }
    }

    let mut raw_ids = structured_top_level_ids.clone();
    raw_ids.extend(history_ids.iter().cloned());
    for session_id in &excluded_subagents {
        raw_ids.remove(session_id);
    }

    let parsed_sessions = load_sessions(None, None, None, Some(SessionSource::Codex))?
        .into_iter()
        .map(|session| ParsedSessionSummary {
            session_id: session.session_id,
            started_at: session.started_at,
            prompt_count: session.user_prompts.len(),
            shell_count: session.shells.len(),
        })
        .collect::<Vec<_>>();

    let mut origins = Vec::new();
    if sessions_root.exists() {
        origins.push(MemoryOsInspectSourceOrigin {
            origin: "codex.sessions".to_string(),
            root: sessions_root.display().to_string(),
            sessions: structured_top_level_ids.len(),
        });
    }
    if history_path.exists() {
        origins.push(MemoryOsInspectSourceOrigin {
            origin: "codex.history".to_string(),
            root: history_path.display().to_string(),
            sessions: history_ids.len(),
        });
    }

    Ok(RawSourceSummary {
        roots: vec![
            sessions_root.display().to_string(),
            history_path.display().to_string(),
        ],
        raw_ids,
        excluded_subagents,
        parsed_sessions,
        origins,
    })
}

fn read_codex_session_meta(path: &Path) -> Result<Option<(String, bool)>> {
    let file = File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let reader = BufReader::new(file);
    for line in reader.lines() {
        let line = match line {
            Ok(line) => line,
            Err(_) => continue,
        };
        let value: Value = match serde_json::from_str(&line) {
            Ok(value) => value,
            Err(_) => continue,
        };
        if value.get("type").and_then(|item| item.as_str()) != Some("session_meta") {
            continue;
        }
        let payload = match value.get("payload") {
            Some(payload) => payload,
            None => continue,
        };
        let Some(session_id) = payload.get("id").and_then(|item| item.as_str()) else {
            continue;
        };
        let is_subagent = payload.pointer("/source/subagent/thread_spawn").is_some();
        return Ok(Some((session_id.to_string(), is_subagent)));
    }
    Ok(None)
}

fn summarize_raw_source(
    source: &str,
    summary: &RawSourceSummary,
    imported_ids: Option<&HashSet<String>>,
) -> MemoryOsInspectSourceCoverage {
    let imported_ids = imported_ids.cloned().unwrap_or_default();
    let parsed_ids = summary
        .parsed_sessions
        .iter()
        .map(|session| session.session_id.clone())
        .collect::<HashSet<_>>();
    let oldest_session = summary
        .parsed_sessions
        .iter()
        .map(|session| session.started_at)
        .min()
        .map(|value| value.to_rfc3339());
    let newest_session = summary
        .parsed_sessions
        .iter()
        .map(|session| session.started_at)
        .max()
        .map(|value| value.to_rfc3339());

    MemoryOsInspectSourceCoverage {
        source: source.to_string(),
        raw_current: summary.raw_ids.len(),
        imported_total: imported_ids.len(),
        missing_current: summary.raw_ids.difference(&imported_ids).count(),
        imported_not_current: imported_ids.difference(&summary.raw_ids).count(),
        excluded_subagents: summary.excluded_subagents.len(),
        parse_failures: summary.raw_ids.difference(&parsed_ids).count(),
        prompt_only_sessions: summary
            .parsed_sessions
            .iter()
            .filter(|session| session.prompt_count > 0 && session.shell_count == 0)
            .count(),
        shell_history_sessions: summary
            .parsed_sessions
            .iter()
            .filter(|session| session.shell_count > 0)
            .count(),
        oldest_session,
        newest_session,
        source_origins: summary.origins.clone(),
        missing_current_ids_sample: sorted_sample(summary.raw_ids.difference(&imported_ids), 20),
        imported_not_current_ids_sample: sorted_sample(
            imported_ids.difference(&summary.raw_ids),
            20,
        ),
        parse_failure_ids_sample: sorted_sample(summary.raw_ids.difference(&parsed_ids), 20),
    }
}

fn sorted_sample<'a>(values: impl Iterator<Item = &'a String>, limit: usize) -> Vec<String> {
    let mut entries = values.cloned().collect::<Vec<_>>();
    entries.sort();
    entries.truncate(limit);
    entries
}

fn render_findings_with_evidence(
    findings: &[MemoryOsNarrativeFinding],
    evidence_limit: usize,
) -> Vec<String> {
    if findings.is_empty() {
        return vec!["- none".to_string()];
    }
    let mut lines = Vec::new();
    for finding in findings {
        lines.push(format!(
            "- {}: {}",
            finding.title,
            display_text(&finding.summary, 180)
        ));
        for evidence in finding.evidence.iter().take(evidence_limit) {
            lines.push(format!("  evidence: {}", evidence));
        }
    }
    lines
}

fn capitalize(value: &str) -> String {
    let mut chars = value.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

fn render_brief_section(findings: &[MemoryOsNarrativeFinding]) -> Vec<String> {
    if findings.is_empty() {
        return vec!["- none".to_string()];
    }
    findings
        .iter()
        .flat_map(|finding| {
            let mut lines = vec![format!("- {}", finding.title)];
            lines.push(format!("  {}", display_text(&finding.summary, 700)));
            for evidence in finding.evidence.iter().take(2) {
                if !brief_text_has_command_noise(evidence) {
                    lines.push(format!("  evidence: {}", display_text(evidence, 220)));
                }
            }
            lines
        })
        .collect()
}

fn render_prompt_section(findings: &[MemoryOsNarrativeFinding]) -> Vec<String> {
    if findings.is_empty() {
        return vec!["- none".to_string()];
    }
    findings
        .iter()
        .map(|finding| {
            format!(
                "- {}: {}",
                finding.title,
                display_text(&finding.summary, 360)
            )
        })
        .collect()
}

pub(crate) fn record_brief_context_event(
    tracker: &Tracker,
    report: &MemoryOsBriefReport,
    rendered: &str,
    context_event_type: &str,
    runtime_source: &str,
) -> Result<()> {
    let current_fact_count = report.what_i_know.len() + report.how_you_work.len();
    let recent_change_count = report.what_is_active.len();
    let open_obligation_count = report.next_steps.len();
    let failure_count = report.watchouts.len();
    let rendered_tokens = rendered.split_whitespace().count();
    tracker.record_context_event(
        context_event_type,
        ContextEventStats {
            rendered_tokens,
            estimated_source_tokens: rendered_tokens,
            current_fact_count,
            recent_change_count,
            live_claim_count: 0,
            open_obligation_count,
            artifact_handle_count: 0,
            failure_count,
        },
    )?;

    let packet_id = format!(
        "memory-os-brief-{}",
        report.generated_at.replace(':', "-").replace('.', "-")
    );
    let items = brief_context_items(report);
    tracker.record_context_item_events(
        context_event_type,
        &packet_id,
        &ContextRuntimeInfo {
            source: runtime_source.to_string(),
            ..Default::default()
        },
        &items,
    )?;
    Ok(())
}

fn brief_context_items(report: &MemoryOsBriefReport) -> Vec<ContextSelectedItemRecord> {
    [
        ("what_i_know", &report.what_i_know),
        ("how_you_work", &report.how_you_work),
        ("what_is_active", &report.what_is_active),
        ("next_steps", &report.next_steps),
        ("watchouts", &report.watchouts),
    ]
    .into_iter()
    .flat_map(|(section, findings)| {
        findings
            .iter()
            .enumerate()
            .map(move |(index, finding)| ContextSelectedItemRecord {
                item_id: format!("memory-os-brief:{section}:{index}"),
                section: brief_compat_section(section).to_string(),
                kind: brief_compat_kind(section).to_string(),
                summary: finding.summary.clone(),
                token_estimate: finding.summary.split_whitespace().count(),
                score: 100 - index as i64,
                artifact_id: None,
                subject: Some(format!("memory-os-brief:{section}:{index}")),
                provenance: vec![
                    "memory-os:brief".to_string(),
                    format!("brief-section:{section}"),
                ],
            })
    })
    .collect()
}

fn brief_compat_section(section: &str) -> &'static str {
    match section {
        "what_is_active" | "next_steps" => "open_obligations",
        "watchouts" => "current_failures",
        "what_i_know" => "validated_claim_leases",
        _ => "deterministic_worldview",
    }
}

fn brief_compat_kind(section: &str) -> &'static str {
    match section {
        "watchouts" => "failure",
        "what_is_active" | "next_steps" => "obligation",
        "what_i_know" => "claim",
        _ => "memory-os-brief",
    }
}

fn active_finding_is_specific(finding: &MemoryOsNarrativeFinding) -> bool {
    if brief_finding_has_command_noise(finding) {
        return false;
    }
    if matches!(
        finding.title.as_str(),
        "Working preference" | "Product constraint" | "Frustration signal" | "Positive feedback"
    ) {
        return false;
    }
    let lowered = finding.summary.to_ascii_lowercase();
    !(lowered.contains("recent continue checkpoint")
        || lowered.contains("recent work still clusters")
        || lowered.contains("lets checkpoint")
        || lowered.contains("come back to your question")
        || lowered.contains("what do you know about me")
        || lowered.contains("how i like to work")
        || lowered.contains("what i am working on")
        || lowered.contains("what should we do next")
        || lowered.contains("what we should do next")
        || lowered.contains("what are the next best steps")
        || lowered.contains("what should we be doing")
        || lowered.starts_with('/'))
}

fn brief_finding_is_prose(finding: &MemoryOsNarrativeFinding) -> bool {
    let summary = finding.summary.trim();
    !brief_finding_has_command_noise(finding)
        && summary.split_whitespace().count() >= 6
        && !summary
            .chars()
            .all(|ch| ch.is_ascii_uppercase() || !ch.is_ascii_alphabetic())
}

fn brief_finding_has_command_noise(finding: &MemoryOsNarrativeFinding) -> bool {
    brief_text_has_command_noise(&finding.title) || brief_text_has_command_noise(&finding.summary)
}

fn brief_text_has_command_noise(text: &str) -> bool {
    let trimmed = text.trim();
    let lowered = trimmed.to_ascii_lowercase();

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
        "list any startup context",
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
        "worktree",
        "branch ",
        "origin/",
        "powershell",
        "pwsh",
        "context proxy",
        "shell executions",
        "sessions, ",
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

fn render_overview_text(report: &MemoryOsOverviewReport) {
    println!("Memory OS Overview");
    println!("------------------");
    println!("Scope: {}", report.scope);
    println!("Imported sessions: {}", report.imported_sessions);
    println!(
        "Imported shell executions: {}",
        report.imported_shell_executions
    );
    println!();
    println!("Imported Sources");
    println!("----------------");
    render_sources(&report.imported_sources);
    println!();
    println!("Top Projects");
    println!("------------");
    if report.top_projects.is_empty() {
        println!("- none");
    } else {
        for project in &report.top_projects {
            println!(
                "- {} | sessions {} | shells {} | {}",
                project.repo_label,
                project.sessions,
                project.shell_executions,
                project.project_path
            );
        }
    }
    println!();
    println!("Top Corrections");
    println!("---------------");
    render_correction_patterns(&report.top_correction_patterns);
    println!();
    println!("Active Work");
    println!("-----------");
    render_findings(&report.active_work);
    println!();
    println!("Action Memory");
    println!("-------------");
    let action_memory_signal: Vec<_> = report
        .top_action_memory_candidates
        .iter()
        .filter(|candidate| {
            !action_memory_cue_is_noise(candidate.cue.trigger_summary.as_deref().unwrap_or(""))
        })
        .take(8)
        .collect();
    if action_memory_signal.is_empty() {
        println!("- none");
    } else {
        for candidate in action_memory_signal {
            println!(
                "- [{}|{}] {} -> {} (precedents {}, success {}, failure {})",
                candidate.status,
                candidate.autonomy_level,
                display_text(
                    candidate
                        .cue
                        .trigger_summary
                        .as_deref()
                        .unwrap_or("no cue summary"),
                    100
                ),
                display_text(
                    candidate
                        .action
                        .command_sig
                        .as_deref()
                        .unwrap_or("no command"),
                    100
                ),
                candidate.precedent_count,
                candidate.success_count,
                candidate.failure_count
            );
        }
    }
    println!();
    println!("Serving Rule");
    println!("------------");
    for line in &report.serving_policy {
        println!("- {}", line);
    }
    println!();
    println!("Onboarding");
    println!("----------");
    println!("Status: {}", report.onboarding.status);
    println!(
        "Schema: {} | checkpoints {} | journal events {}",
        report.onboarding.schema_version,
        report.onboarding.checkpoint_count,
        report.onboarding.journal_event_count
    );
    println!(
        "Imported: {} sessions | {} shells | {} corrections",
        report.onboarding.sessions_processed,
        report.onboarding.shells_ingested,
        report.onboarding.corrections_ingested
    );
    if let Some(completed_at) = &report.onboarding.completed_at {
        println!("Completed at: {}", completed_at);
    }
}

fn render_recall_text(report: &crate::core::memory_os::MemoryOsRecallReport) {
    println!("Memory OS Recall");
    println!("----------------");
    println!("Scope: {}", report.scope);
    println!("Topic: {}", report.query);
    println!();
    if report.matches.is_empty() {
        println!(
            "{}",
            report
                .no_match_reason
                .as_deref()
                .unwrap_or("No compiled Memory OS evidence matched the query.")
        );
        return;
    }
    for (index, item) in report.matches.iter().enumerate() {
        println!("{}. {}", index + 1, item.title);
        println!("   {}", item.answer);
        println!("   source: {} ({})", item.source_ref, item.source_kind);
        if !item.project_path.is_empty() {
            println!("   project: {}", item.project_path);
        }
        for evidence in item.evidence.iter().take(2) {
            println!("   evidence: {}", evidence);
        }
    }
}

fn apply_friction_filters(
    report: &mut MemoryOsFrictionReport,
    agent: Option<&str>,
    last: Option<&str>,
) {
    let agent = agent.map(|value| value.to_lowercase());
    let since = last.and_then(parse_last_window);
    if agent.is_none() && since.is_none() {
        return;
    }
    report.top_fixes.retain(|fix| {
        let combined = format!(
            "{} {} {} {} {}",
            fix.title,
            fix.summary,
            fix.permanent_fix,
            fix.status,
            fix.evidence.join(" ")
        )
        .to_lowercase();
        let agent_matches = agent
            .as_deref()
            .map(|needle| combined.contains(needle))
            .unwrap_or(true);
        let time_matches = since
            .map(|since| {
                fix.evidence.iter().any(|line| {
                    first_rfc3339_timestamp(line)
                        .map(|timestamp| timestamp >= since)
                        .unwrap_or(false)
                })
            })
            .unwrap_or(true);
        agent_matches && time_matches
    });
    report.by_source.clear();
    report.redirects = crate::core::memory_os::MemoryOsRedirectSummary::default();
    report.repeated_corrections.clear();
    report.likely_misunderstandings.clear();
    report.behavior_changes.clear();
}

fn parse_last_window(value: &str) -> Option<DateTime<Utc>> {
    let trimmed = value.trim().to_lowercase();
    let days = trimmed
        .strip_suffix('d')
        .and_then(|number| number.parse::<i64>().ok())?;
    Some(Utc::now() - chrono::Duration::days(days.max(0)))
}

fn first_rfc3339_timestamp(value: &str) -> Option<DateTime<Utc>> {
    value.split_whitespace().find_map(|part| {
        DateTime::parse_from_rfc3339(part.trim_matches(|ch| ch == ',' || ch == ';'))
            .ok()
            .map(|timestamp| timestamp.with_timezone(&Utc))
    })
}

fn render_profile_text(report: &MemoryOsProfileReport) {
    println!("Memory OS Profile");
    println!("-----------------");
    println!("Scope: {}", report.scope);
    println!("Imported sessions: {}", report.imported_sessions);
    println!();
    println!("By Source");
    println!("---------");
    render_behavior_summaries(&report.by_source);
    println!();
    println!("Preferences");
    println!("-----------");
    render_findings(&report.preferences);
    println!();
    println!("Operating Style");
    println!("---------------");
    render_findings(&report.operating_style);
    println!();
    println!("Autonomy Tendencies");
    println!("-------------------");
    render_findings(&report.autonomy_tendencies);
    println!();
    println!("Epistemic Preferences");
    println!("---------------------");
    render_findings(&report.epistemic_preferences);
    println!();
    println!("Recurring Themes");
    println!("----------------");
    render_findings(&report.recurring_themes);
    println!();
    println!("Friction Triggers");
    println!("-----------------");
    render_findings(&report.friction_triggers);
}

fn render_friction_text(report: &MemoryOsFrictionReport) {
    println!("Memory OS Friction");
    println!("------------------");
    println!("Scope: {}", report.scope);
    println!();
    println!("Top Friction Fixes");
    println!("------------------");
    render_friction_fixes(&report.top_fixes);
    println!();
    println!("By Source");
    println!("---------");
    render_behavior_summaries(&report.by_source);
    println!();
    println!("Redirect Signals");
    println!("----------------");
    println!(
        "Redirect-like recommendation shifts: {}",
        report.redirects.redirects
    );
    println!(
        "Redirected workstreams: {}",
        report.redirects.redirected_sessions
    );
    println!(
        "Shifts with resumed execution: {}",
        report.redirects.redirects_with_resumed_shell
    );
    println!(
        "Shifts with later success: {}",
        report.redirects.redirects_with_success_after_resume
    );
    println!(
        "Avg commands to success: {}",
        format_optional_metric(
            report.redirects.avg_commands_to_success_after_redirect,
            "cmds"
        )
    );
    println!(
        "Avg seconds to success: {}",
        format_optional_metric(report.redirects.avg_seconds_to_success_after_redirect, "s")
    );
    println!();
    println!("Correction Evidence");
    println!("-------------------");
    render_correction_patterns(&report.repeated_corrections);
    println!();
    println!("Likely Misunderstandings");
    println!("-----------------------");
    if report.likely_misunderstandings.is_empty() {
        println!("- none");
    } else {
        for pattern in &report.likely_misunderstandings {
            println!("- {} | {} hits", pattern.label, pattern.count);
            println!("  {}", pattern.summary);
            println!("  raw examples: available in --format json");
        }
    }
    println!();
    println!("Behavior Changes");
    println!("----------------");
    if report.behavior_changes.is_empty() {
        println!("- none");
    } else {
        for change in &report.behavior_changes {
            println!("- [{}] {}", change.target_agent, change.change);
            println!("  rationale: {}", change.rationale);
            for evidence in change.evidence.iter().take(4) {
                println!("  evidence: {}", evidence);
            }
        }
    }
}

fn render_friction_fixes(fixes: &[MemoryOsFrictionFix]) {
    if fixes.is_empty() {
        println!("- none");
        return;
    }
    let non_fixed: Vec<&MemoryOsFrictionFix> =
        fixes.iter().filter(|fix| fix.status != "fixed").collect();
    let detailed_limit = 10usize;
    for (index, fix) in non_fixed.iter().take(detailed_limit).enumerate() {
        println!(
            "{}. [{}|{}] {}",
            index + 1,
            fix.impact,
            fix.status,
            display_text(&fix.title, 110)
        );
        println!("   pattern: {}", display_text(&fix.summary, 180));
        println!(
            "   permanent fix: {}",
            display_text(&fix.permanent_fix, 220)
        );
        for evidence in fix.evidence.iter().take(2) {
            println!("   evidence: {}", display_text(evidence, 160));
        }
    }
    let remaining = non_fixed.len().saturating_sub(detailed_limit);
    if remaining > 0 {
        println!();
        println!(
            "Additional Active Friction ({} more, awareness only)",
            remaining
        );
        println!("------------------------------------------------------");
        for (offset, fix) in non_fixed.iter().skip(detailed_limit).enumerate() {
            println!(
                "{}. [{}|{}] {} (score {})",
                detailed_limit + offset + 1,
                fix.impact,
                fix.status,
                display_text(&fix.title, 110),
                fix.score
            );
        }
    }
    let fixed_count = fixes.iter().filter(|fix| fix.status == "fixed").count();
    if fixed_count > 0 {
        println!(
            "- [background|fixed] {} lower-priority fixes are fading because corrected replays are succeeding.",
            fixed_count
        );
    }
    if non_fixed.is_empty() && fixed_count == 0 {
        println!("- none");
    }
}

fn render_action_policy_text(report: &MemoryOsActionPolicyViewReport) {
    println!("Memory OS Action Policy");
    println!("-----------------------");
    println!("Scope: {}", report.scope);
    println!(
        "Candidates: {} | behavior changes: {} | approvals: {} | hook lanes: {} | rules: {}",
        report.candidate_count,
        report.behavior_change_count,
        report.approvals_count,
        report.hook_capabilities.len(),
        report.rules.len()
    );
    println!();
    if report.candidates.is_empty()
        && report.rules.is_empty()
        && report.approvals.is_empty()
        && report.hook_capabilities.is_empty()
    {
        println!("- none");
        return;
    }
    if !report.candidates.is_empty() {
        println!("Candidates");
        println!("----------");
        for candidate in report.candidates.iter().take(10) {
            println!(
                "- [{}|{}|{}] {}",
                candidate.status, candidate.actuator_type, candidate.confidence, candidate.title
            );
            println!("  {}", candidate.summary);
            println!(
                "  precedents: {} | success: {} | failure: {} | aging: {}",
                candidate.precedent_count,
                candidate.success_count,
                candidate.failure_count,
                candidate.aging_status
            );
            if let Some(command) = &candidate.action.command_sig {
                println!("  command: {}", command);
            }
            if let Some(recommendation) = &candidate.action.recommendation {
                println!("  recommendation: {}", recommendation);
            }
            println!(
                "  review-after: {} | expires: {} | policy: {}",
                candidate.review_after.as_deref().unwrap_or("not-scheduled"),
                candidate.expires_at.as_deref().unwrap_or("not-set"),
                candidate.lifecycle_policy.as_deref().unwrap_or("expiring")
            );
            for evidence in candidate.source_refs.iter().take(3) {
                println!("  evidence: {}", evidence);
            }
        }
        println!();
    }
    for rule in &report.rules {
        println!(
            "- [{}|{}|{}|{}] {}",
            rule.scope, rule.action_kind, rule.strength, rule.confidence, rule.title
        );
        println!("  {}", rule.summary);
        if let Some(command) = &rule.suggested_command {
            println!("  command: {}", command);
        }
        if let Some(recommendation) = &rule.recommendation {
            println!("  recommendation: {}", recommendation);
        }
        if let Some(target_agent) = &rule.target_agent {
            println!("  target: {}", target_agent);
        }
        println!(
            "  review-after: {} | expires: {} | policy: {} | aging: {}",
            rule.review_after.as_deref().unwrap_or("not-scheduled"),
            rule.expires_at.as_deref().unwrap_or("not-set"),
            rule.lifecycle_policy.as_deref().unwrap_or("expiring"),
            rule.aging_status
        );
        for evidence in rule.supporting_evidence.iter().take(4) {
            println!("  evidence: {}", evidence);
        }
    }
    if !report.approvals.is_empty() {
        println!();
        println!("Approval Jobs");
        println!("-------------");
        for approval in &report.approvals {
            println!(
                "- [{}|{}|{}] {}",
                approval.status, approval.item_kind, approval.local_date, approval.title
            );
            println!("  {}", approval.summary);
            if let Some(effect) = &approval.expected_effect {
                println!("  expected-effect: {}", effect);
            }
            println!(
                "  review-after: {} | expires: {}",
                approval.review_after.as_deref().unwrap_or("not-scheduled"),
                approval.expires_at.as_deref().unwrap_or("not-set")
            );
            if let Some(last_reviewed_at) = &approval.last_reviewed_at {
                println!("  last-reviewed: {}", last_reviewed_at);
            }
            if let Some(reason) = &approval.closure_reason {
                println!("  closure-reason: {}", reason);
            }
        }
    }
    if !report.hook_capabilities.is_empty() {
        println!();
        println!("Hook Capability");
        println!("---------------");
        for capability in &report.hook_capabilities {
            println!(
                "- [{}] {} | rewrite {} | ask {} | updated-input {} | fallback {}",
                capability.status,
                capability.surface,
                capability.rewrite_support,
                capability.ask_support,
                capability.updated_input_support,
                capability.fallback_mode
            );
            println!("  {}", capability.summary);
        }
    }
}

fn render_trust_text(report: &MemoryOsTrustReport) {
    println!("Memory OS Trust");
    println!("---------------");
    println!("Scope: {}", report.scope);
    println!("Observations: {}", report.observation_count);
    println!(
        "Must-not-packetize: {} | secret hits: {} | pii hits: {}",
        report.must_not_packetize_count, report.secret_count, report.pii_count
    );
    println!();
    println!("By Target");
    println!("---------");
    if report.by_target.is_empty() {
        println!("- none");
    } else {
        for target in &report.by_target {
            println!(
                "- {} | observations {} | blocked {} | secrets {} | pii {} | latest {}",
                target.target_kind,
                target.observation_count,
                target.must_not_packetize_count,
                target.secret_count,
                target.pii_count,
                target.latest_observed_at
            );
        }
    }
    println!();
    println!("Recent Observations");
    println!("-------------------");
    render_trust_observations(&report.recent_observations);
}

fn render_sources(sources: &[crate::core::memory_os::MemoryOsImportedSourceSummary]) {
    if sources.is_empty() {
        println!("- none");
        return;
    }
    for source in sources {
        println!(
            "- {} | sessions {} | shells {}",
            source.source, source.sessions, source.shell_executions
        );
    }
}

fn render_correction_patterns(patterns: &[MemoryOsCorrectionPatternSummary]) {
    if patterns.is_empty() {
        println!("- none");
        return;
    }
    for pattern in patterns {
        let wrong_preview = display_text(&pattern.wrong_command, 80);
        let corrected_preview = display_text(&pattern.corrected_command, 80);
        println!(
            "- [{}] x{} ({} succeeded, {} failed)",
            pattern.error_kind, pattern.count, pattern.successful_replays, pattern.failed_replays
        );
        if !wrong_preview.trim().is_empty() {
            println!("    was: {}", wrong_preview);
        }
        if !corrected_preview.trim().is_empty() {
            println!("    now: {}", corrected_preview);
        }
    }
}

fn action_memory_cue_is_noise(summary: &str) -> bool {
    let trimmed = summary.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("no cue summary") {
        return true;
    }
    if trimmed.contains('\u{2550}') {
        return true;
    }
    let lowered = trimmed.to_ascii_lowercase();
    let noise_prefixes = [
        "cargo build:",
        "cargo test:",
        "cargo fmt:",
        "cargo check:",
        "cargo clippy:",
        "resolve active failure",
    ];
    if noise_prefixes
        .iter()
        .any(|prefix| lowered.starts_with(prefix))
    {
        return true;
    }
    if lowered.contains(" | branch ")
        && (lowered.contains("staged ") || lowered.contains("modified "))
    {
        return true;
    }
    false
}

fn render_behavior_summaries(summaries: &[MemoryOsSourceBehaviorSummary]) {
    if summaries.is_empty() {
        println!("- none");
        return;
    }
    for summary in summaries {
        println!(
            "- {} | sessions {} | shells {} | corrections {} | shells/session {:.1} | corrections/100 shells {:.1}",
            summary.source,
            summary.sessions,
            summary.shell_executions,
            summary.corrections,
            summary.shells_per_session,
            summary.corrections_per_100_shells
        );
    }
}

fn render_findings(findings: &[MemoryOsNarrativeFinding]) {
    if findings.is_empty() {
        println!("- none");
        return;
    }
    for finding in findings {
        println!("- {}", finding.title);
        println!("  {}", display_text(&finding.summary, 160));
        for evidence in finding.evidence.iter().take(4) {
            println!("  evidence: {}", display_text(evidence, 160));
        }
    }
}

fn render_trust_observations(records: &[MemoryOsTrustObservationRecord]) {
    if records.is_empty() {
        println!("- none");
        return;
    }
    for record in records.iter().take(10) {
        println!(
            "- [{}|{}] {} -> {} @ {}",
            record.decision,
            record.taint_state,
            record.target_kind,
            display_text(&record.target_ref, 80),
            record.observed_at
        );
        println!("  action: {}", record.action_kind);
        println!(
            "  sensitivity: {} | secret {} | pii {} | must-not-packetize {}",
            record.sensitivity_class,
            record.contains_secret,
            record.contains_pii,
            record.must_not_packetize
        );
        println!("  reason: {}", display_text(&record.reason_json, 120));
    }
}

fn format_optional_metric(value: Option<f64>, suffix: &str) -> String {
    value
        .map(|metric| format!("{metric:.1} {suffix}"))
        .unwrap_or_else(|| "n/a".to_string())
}

fn display_text(text: &str, max_len: usize) -> String {
    let compact = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.len() <= max_len {
        compact
    } else {
        let mut truncated = compact
            .chars()
            .take(max_len.saturating_sub(3))
            .collect::<String>();
        truncated.push_str("...");
        truncated
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::tracking::{
        MemoryOsTrustDecision, MemoryOsTrustObservationInput, MemoryOsVerificationResultInput,
        MemoryOsVerificationStatus, Tracker,
    };
    use std::sync::Mutex;
    use tempfile::TempDir;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn temp_tracker() -> (TempDir, Tracker, std::path::PathBuf) {
        let tmp = TempDir::new().expect("temp dir");
        let db_path = tmp.path().join("history.db");
        let tracker = Tracker::new_at_path(&db_path).expect("tracker");
        (tmp, tracker, db_path)
    }

    #[test]
    fn action_memory_cue_is_noise_filters_build_state_and_separators() {
        assert!(action_memory_cue_is_noise(""));
        assert!(action_memory_cue_is_noise("no cue summary"));
        assert!(action_memory_cue_is_noise(
            "cargo build: 0 errors, 18 warnings (0 crates)"
        ));
        assert!(action_memory_cue_is_noise(
            "cargo test: 5 passed, 1761 filtered out"
        ));
        assert!(action_memory_cue_is_noise(
            "Resolve active failure: cargo build: 0 errors"
        ));
        assert!(action_memory_cue_is_noise(
            "Something | ═════════════════════════"
        ));
        assert!(action_memory_cue_is_noise(
            "C:\\Users\\OEM\\Projects\\codex-pool | branch master...origin/master | staged 0 | modified 0"
        ));

        assert!(!action_memory_cue_is_noise(
            "user asked about sitesorted strategy"
        ));
        assert!(!action_memory_cue_is_noise(
            "current work: review memory-os output"
        ));
    }

    #[test]
    fn run_snapshot_rejects_bad_format() {
        let err = run_snapshot(None, "yaml", 0).expect_err("bad format should fail");
        assert!(err.to_string().contains("unsupported format"));
    }

    #[test]
    fn run_promotion_rejects_bad_format() {
        let err = run_promotion("yaml", 0).expect_err("bad format should fail");
        assert!(err.to_string().contains("unsupported format"));
    }

    #[test]
    fn run_kernel_rejects_bad_format() {
        let err = run_kernel(None, "yaml", 0).expect_err("bad format should fail");
        assert!(err.to_string().contains("unsupported format"));
    }

    #[test]
    fn run_actions_rejects_bad_format() {
        let err = run_actions(None, "yaml", 0).expect_err("bad format should fail");
        assert!(err.to_string().contains("unsupported format"));
    }

    #[test]
    fn run_overview_rejects_bad_scope() {
        let err = run_overview("workspace", None, "text", 0).expect_err("bad scope should fail");
        assert!(err.to_string().contains("unsupported scope"));
    }

    #[test]
    fn run_profile_rejects_project_on_user_scope() {
        let err = run_profile("user", Some("C:/repo"), "text", 0)
            .expect_err("user scope should reject project flag");
        assert!(err.to_string().contains("--project is only valid"));
    }

    #[test]
    fn run_brief_rejects_bad_format() {
        let err = run_brief("user", None, "yaml", false, 0).expect_err("bad format should fail");
        assert!(err.to_string().contains("unsupported format"));
    }

    #[test]
    fn run_doctor_rejects_bad_format() {
        let err = run_doctor("user", None, "yaml", 0).expect_err("bad format should fail");
        assert!(err.to_string().contains("unsupported format"));
    }

    #[test]
    fn render_doctor_text_prioritizes_permanent_pipeline_fix() {
        let report = MemoryOsDoctorReport {
            schema_version: "memory-os-doctor-v1".to_string(),
            generated_at: "2026-04-18T00:00:00Z".to_string(),
            scope: MemoryOsInspectionScope::User,
            overall_status: "fail".to_string(),
            corpus: MemoryOsDoctorCorpus {
                onboarding_status: "completed".to_string(),
                onboarding_completed_at: Some("2026-04-18T00:00:00Z".to_string()),
                sessions_processed: 10,
                shells_ingested: 100,
                corrections_ingested: 4,
                imported_sessions_total: 10,
                imported_shell_executions_total: 100,
                checkpoint_count: 8,
                journal_event_count: 20,
                latest_import_completed_at: Some("2026-04-18T00:00:00Z".to_string()),
                latest_import_freshness: "fresh (0d)".to_string(),
                raw_coverage_mode: "fast compiled-state check".to_string(),
                source_health: vec![MemoryOsDoctorCorpusSource {
                    source: "codex".to_string(),
                    imported_sessions: 10,
                    import_status: "imported".to_string(),
                }],
            },
            signal_quality: vec![MemoryOsDoctorCheck {
                name: "Active work is detectable".to_string(),
                status: "fail".to_string(),
                summary: "0 active-work findings are available.".to_string(),
                evidence: Vec::new(),
            }],
            top_pipeline_problems: vec![MemoryOsDoctorProblem {
                severity: "high".to_string(),
                title: "Active work is not inspectable".to_string(),
                summary: "0 active-work findings are available.".to_string(),
                permanent_fix: "Promote recent user prose above fallback project summaries."
                    .to_string(),
                evidence: Vec::new(),
            }],
            recommended_permanent_fix:
                "Promote recent user prose above fallback project summaries.".to_string(),
        };

        let rendered = render_doctor_text(&report);

        assert!(rendered.contains("Memory OS Doctor"));
        assert!(rendered.contains("[high] Active work is not inspectable"));
        assert!(rendered.contains("Recommended Permanent Fix"));
        assert!(rendered.contains("Promote recent user prose"));
    }

    #[test]
    fn brief_context_items_emit_legacy_compatible_sections_without_alias_duplicates() {
        let finding = |summary: &str| MemoryOsNarrativeFinding {
            title: summary.to_string(),
            summary: summary.to_string(),
            evidence: vec![],
        };
        let report = MemoryOsBriefReport {
            generated_at: "2026-04-13T00:00:00Z".to_string(),
            scope: MemoryOsInspectionScope::User,
            what_i_know: vec![finding("Known claim")],
            how_you_work: vec![finding("Workflow note")],
            what_is_active: vec![finding("Active obligation")],
            next_steps: vec![finding("Next obligation")],
            watchouts: vec![finding("Current failure")],
        };

        let items = brief_context_items(&report);
        assert_eq!(items.len(), 5);
        assert_eq!(items[0].section, "validated_claim_leases");
        assert_eq!(items[2].section, "open_obligations");
        assert_eq!(items[3].section, "open_obligations");
        assert_eq!(items[4].section, "current_failures");
        let unique_ids = items
            .iter()
            .map(|item| item.item_id.as_str())
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(unique_ids.len(), items.len());
        assert!(items.iter().all(|item| item
            .provenance
            .iter()
            .any(|entry| entry == "memory-os:brief")));
    }

    #[test]
    fn strategy_kernel_brief_findings_surface_goals_kpis_and_initiatives() {
        let source_ref = strategy::StrategySourceRef {
            source_id: "source-1".to_string(),
            source_path: "C:/strategy/plan.context.json".to_string(),
            section_path: "Goals".to_string(),
            line_start: 1,
            line_end: 1,
            excerpt: "strategy".to_string(),
        };
        let kernel = strategy::StrategyKernel {
            schema_version: "strategy-kernel-v1".to_string(),
            scope_id: "sitesorted-business".to_string(),
            imported_at: "2026-04-17T00:00:00Z".to_string(),
            sources: vec![strategy::StrategySourceDocument {
                source_id: "source-1".to_string(),
                source_type: "strategy-json".to_string(),
                path: "C:/strategy/plan.context.json".to_string(),
                content_hash: "hash".to_string(),
                imported_at: "2026-04-17T00:00:00Z".to_string(),
            }],
            goals: vec![strategy::StrategyGoal {
                goal_id: "goal-first-customer".to_string(),
                horizon: "quarterly".to_string(),
                title: "Land the first paying customer".to_string(),
                summary: "Convert proof into revenue.".to_string(),
                due_date: Some("2026-05-13".to_string()),
                source_refs: vec![source_ref.clone()],
            }],
            kpis: vec![strategy::StrategyKpi {
                kpi_id: "kpi-paying-customers".to_string(),
                title: "Paying customers".to_string(),
                metric_key: "paying_customers".to_string(),
                unit: Some("customers".to_string()),
                target: Some(10.0),
                green_threshold: Some(10.0),
                yellow_threshold: Some(5.0),
                cadence: Some("weekly".to_string()),
                due_date: Some("2026-06-30".to_string()),
                goal_ids: vec!["goal-first-customer".to_string()],
                initiative_ids: vec!["rock-close-first-customer".to_string()],
                source_refs: vec![source_ref.clone()],
            }],
            initiatives: vec![strategy::StrategyInitiative {
                initiative_id: "rock-close-first-customer".to_string(),
                kind: "rock".to_string(),
                title: "Close the first paying customer".to_string(),
                owner: Some("Patrick".to_string()),
                due_date: Some("2026-05-13".to_string()),
                depends_on: Vec::new(),
                supports_goal_ids: vec!["goal-first-customer".to_string()],
                deferred: false,
                source_refs: vec![source_ref.clone()],
            }],
            constraints: vec![strategy::StrategyConstraint {
                constraint_id: "constraint-no-au".to_string(),
                title: "No Australia expansion before NZ proof".to_string(),
                suppression_kind: "not-now".to_string(),
                summary: None,
                source_refs: vec![source_ref],
            }],
            assumptions: Vec::new(),
        };

        let findings = strategy_kernel_brief_findings(&kernel);

        assert!(findings
            .knowledge
            .iter()
            .any(|finding| finding.title == "Authoritative strategy"
                && finding.summary.contains("Land the first paying customer")));
        assert!(findings
            .knowledge
            .iter()
            .any(|finding| finding.title == "Strategy KPI"
                && finding
                    .summary
                    .contains("Paying customers target 10 customers")));
        assert!(findings
            .active
            .iter()
            .any(|finding| finding.title == "Strategy initiative"
                && finding.summary.contains("Close the first paying customer")));
        assert!(findings
            .watchouts
            .iter()
            .any(|finding| finding.title == "Strategy constraint"
                && finding.summary.contains("No Australia expansion")));
    }

    #[test]
    fn active_finding_is_specific_prefers_prose_task_signals() {
        let finding = MemoryOsNarrativeFinding {
            title: "workspace-root".to_string(),
            summary: "Fix the Memory OS brief active-work section before trusting the surface."
                .to_string(),
            evidence: vec![
                "resume checkpoint at 2026-04-17T01:00:00Z".to_string(),
                "goal: Fix the Memory OS brief active-work section".to_string(),
            ],
        };

        assert!(active_finding_is_specific(&finding));
        assert!(brief_finding_is_prose(&finding));
    }

    #[test]
    fn rank_brief_findings_prefers_prose_and_filters_command_noise() {
        let findings = vec![
            MemoryOsNarrativeFinding {
                title: "cd C:/Users/OEM/Projects && node script.js".to_string(),
                summary: "cd C:/Users/OEM/Projects && node script.js".to_string(),
                evidence: vec![],
            },
            MemoryOsNarrativeFinding {
                title: "Memory OS trust proof".to_string(),
                summary:
                    "Resume the trust-proof lane and verify the private replay evidence before cutover."
                        .to_string(),
                evidence: vec![],
            },
            MemoryOsNarrativeFinding {
                title: "workspace-root".to_string(),
                summary: "627 sessions, 24623 shell executions".to_string(),
                evidence: vec![],
            },
        ];

        let ranked = rank_brief_findings(findings.iter(), 3);
        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].title, "Memory OS trust proof");
    }

    #[test]
    fn brief_section_routing_keeps_machine_rejections_and_product_constraints_out_of_wrong_slots() {
        let finding = |title: &str, summary: &str| MemoryOsNarrativeFinding {
            title: title.to_string(),
            summary: summary.to_string(),
            evidence: vec![],
        };

        assert!(!brief_finding_belongs_in_knowledge(
            &finding("Durable rejection", "Bad rule: user rejected this path",),
            false,
        ));
        assert!(brief_finding_belongs_in_knowledge(
            &finding(
                "Memory OS direction",
                "Build the broad Memory OS tool instead of a benchmark-only patch.",
            ),
            false,
        ));
        assert!(!brief_finding_belongs_in_knowledge(
            &finding(
                "Business strategy",
                "I want a lead database for small businesses.",
            ),
            true,
        ));
        assert!(!active_finding_is_specific(&finding(
            "Product constraint",
            "Keep the look the same and only make the functional change.",
        )));
        assert!(active_finding_is_specific(&finding(
            "SiteSorted focus",
            "Everything should go to SiteSorted by default.",
        )));
    }

    #[test]
    fn brief_uses_user_prose_from_onboarding_checkpoint_before_project_fallback() {
        let _guard = ENV_LOCK.lock().expect("env lock");
        let (_tmp, tracker, _) = temp_tracker();
        std::env::set_var("CONTEXT_MEMORYOS_JOURNAL_V1", "true");
        std::env::set_var("CONTEXT_MEMORYOS_DUAL_WRITE_V1", "true");
        std::env::set_var("CONTEXT_MEMORYOS_CHECKPOINT_V1", "true");

        let prompt = "I want the broad Memory OS tool to show useful prose from user sessions.";
        tracker
            .record_memory_os_packet_checkpoint_for_project(
                "C:/repo",
                &crate::core::memory_os::MemoryOsCheckpointCapture {
                    packet_id: "onboarding-codex-prose-001".into(),
                    generated_at: "2026-04-17T00:00:00Z".into(),
                    preset: "resume".into(),
                    intent: "diagnose".into(),
                    profile: "session-onboarding".into(),
                    goal: Some(prompt.into()),
                    budget: 1600,
                    estimated_tokens: 0,
                    estimated_source_tokens: 0,
                    pager_manifest_hash: "manifest-prose".into(),
                    recall_mode: "off".into(),
                    recall_used: false,
                    recall_reason: "session-onboarding".into(),
                    telemetry: crate::core::memory_os::MemoryOsCheckpointTelemetry {
                        current_fact_count: 0,
                        recent_change_count: 0,
                        live_claim_count: 0,
                        open_obligation_count: 0,
                        artifact_handle_count: 0,
                        failure_count: 0,
                    },
                    selected_items: vec![
                        crate::core::memory_os::MemoryOsPacketSelection {
                            section: "user_prompts".into(),
                            kind: "user-prompt".into(),
                            summary: prompt.into(),
                            token_estimate: 12,
                            score: 100,
                            artifact_id: None,
                            subject: Some("prompt:codex:prose-001".into()),
                            provenance: vec!["session:codex".into()],
                        },
                        crate::core::memory_os::MemoryOsPacketSelection {
                            section: "current_failures".into(),
                            kind: "failure".into(),
                            summary: "context proxy powershell -NoProfile -Command cargo test"
                                .into(),
                            token_estimate: 8,
                            score: 90,
                            artifact_id: None,
                            subject: Some("command:context proxy".into()),
                            provenance: vec!["session:codex".into()],
                        },
                    ],
                    exclusions: Vec::new(),
                    reentry: crate::core::memory_os::MemoryOsCheckpointReentry {
                        recommended_command: "context context".into(),
                        current_recommendation: Some(prompt.into()),
                        first_question: "What still matters from this session?".into(),
                        first_verification: "Verify the useful prose survives ranking.".into(),
                    },
                },
            )
            .expect("onboarding checkpoint");

        let brief =
            build_brief_report(&tracker, MemoryOsInspectionScope::User, None).expect("brief");

        assert!(brief
            .what_i_know
            .iter()
            .any(|finding| finding.summary.contains("useful prose")));
        assert!(brief
            .what_is_active
            .iter()
            .any(|finding| finding.summary.contains("useful prose")));
        assert!(!brief
            .what_is_active
            .iter()
            .any(|finding| finding.summary.contains("Recent work still clusters")));
        assert!(!brief
            .what_i_know
            .iter()
            .chain(brief.what_is_active.iter())
            .any(|finding| finding.summary.contains("context proxy")));

        std::env::remove_var("CONTEXT_MEMORYOS_JOURNAL_V1");
        std::env::remove_var("CONTEXT_MEMORYOS_DUAL_WRITE_V1");
        std::env::remove_var("CONTEXT_MEMORYOS_CHECKPOINT_V1");
    }

    #[test]
    fn run_trust_rejects_bad_scope() {
        let err = run_trust("workspace", None, "text", 0).expect_err("bad scope should fail");
        assert!(err.to_string().contains("unsupported scope"));
    }

    #[test]
    fn run_snapshot_success_path_is_read_only() {
        let (_tmp, tracker, _) = temp_tracker();
        tracker
            .record_memory_os_projection_checkpoint("claims", "C:/repo", 1, 1, "incremental")
            .expect("projection checkpoint");

        let before_journal = tracker
            .get_memory_os_project_snapshot(Some("C:/repo"))
            .expect("snapshot before")
            .journal_event_count;
        let before_checkpoints = tracker
            .get_memory_os_project_snapshot(Some("C:/repo"))
            .expect("snapshot before")
            .projection_checkpoints
            .len();

        render_snapshot_with_tracker(&tracker, Some("C:/repo"), "json").expect("snapshot command");

        let after = tracker
            .get_memory_os_project_snapshot(Some("C:/repo"))
            .expect("snapshot after");
        assert_eq!(after.journal_event_count, before_journal);
        assert_eq!(after.projection_checkpoints.len(), before_checkpoints);
    }

    #[test]
    fn run_kernel_success_path_is_read_only() {
        let (_tmp, tracker, _) = temp_tracker();
        let before = tracker
            .get_memory_os_project_snapshot(None)
            .expect("snapshot before");

        render_kernel_with_tracker(&tracker, None, "json").expect("kernel command");

        let after = tracker
            .get_memory_os_project_snapshot(None)
            .expect("snapshot after");
        assert_eq!(after.journal_event_count, before.journal_event_count);
        assert_eq!(
            after.projection_checkpoints.len(),
            before.projection_checkpoints.len()
        );
    }

    #[test]
    fn run_trust_success_path_reports_observations() {
        let (_tmp, tracker, _) = temp_tracker();
        tracker
            .record_memory_os_trust_observation_for_project(
                "C:/repo",
                &MemoryOsTrustObservationInput {
                    observation_id: "trust-001".into(),
                    target_kind: "worldview".into(),
                    target_ref: "file:demo.rs".into(),
                    action_kind: "packetize".into(),
                    decision: MemoryOsTrustDecision::Review,
                    reason_json: "{\"reason\":\"observe-only\"}".into(),
                    read_seq_cut: Some(42),
                    policy_model_id: None,
                    sensitivity_class: "internal".into(),
                    contains_secret: true,
                    contains_pii: false,
                    must_not_packetize: true,
                    taint_state: "tainted".into(),
                    observed_at: "2026-04-13T00:00:00Z".into(),
                },
            )
            .expect("trust observation");

        render_trust_with_tracker(
            &tracker,
            MemoryOsInspectionScope::Project,
            Some("C:/repo"),
            "json",
        )
        .expect("trust report");
    }

    #[test]
    fn render_promotion_with_tracker_blocks_without_replay_proof() {
        let _guard = ENV_LOCK.lock().expect("env lock");
        let (_tmp, tracker, _) = temp_tracker();

        std::env::set_var("CONTEXT_MEMORYOS_READ_MODEL_V1", "true");
        std::env::set_var("CONTEXT_MEMORYOS_RESUME_V1", "true");
        std::env::set_var("CONTEXT_MEMORYOS_HANDOFF_V1", "true");
        std::env::set_var("CONTEXT_MEMORYOS_STRICT_PROMOTION_V1", "true");

        let rendered = render_promotion_with_tracker(&tracker, "text").expect("promotion report");

        assert!(rendered.contains("Resume cutover: blocked"));
        assert!(rendered.contains("missing independent proposed-kernel proof"));

        std::env::remove_var("CONTEXT_MEMORYOS_READ_MODEL_V1");
        std::env::remove_var("CONTEXT_MEMORYOS_RESUME_V1");
        std::env::remove_var("CONTEXT_MEMORYOS_HANDOFF_V1");
        std::env::remove_var("CONTEXT_MEMORYOS_STRICT_PROMOTION_V1");
    }

    #[test]
    fn render_promotion_with_tracker_reports_verified_gate() {
        let _guard = ENV_LOCK.lock().expect("env lock");
        let (_tmp, tracker, _) = temp_tracker();

        std::env::set_var("CONTEXT_MEMORYOS_READ_MODEL_V1", "true");
        std::env::set_var("CONTEXT_MEMORYOS_RESUME_V1", "true");
        std::env::set_var("CONTEXT_MEMORYOS_HANDOFF_V1", "true");
        std::env::set_var("CONTEXT_MEMORYOS_STRICT_PROMOTION_V1", "true");

        for (idx, split) in ["test-private", "adversarial-private"].iter().enumerate() {
            tracker
                .record_memory_os_verification_result(&MemoryOsVerificationResultInput {
                    verification_result_id: format!("verify-{idx}"),
                    proof_id: format!("proof-{idx}"),
                    scope_json: serde_json::json!({
                        "root": "tests/fixtures/replay_eval",
                        "split": split,
                        "system": "proposed-kernel",
                        "proof_tier": "independent",
                        "independent": true,
                        "contamination_free": true
                    })
                    .to_string(),
                    verifier_id: "replay-eval".into(),
                    verifier_version: "v1".into(),
                    trusted_root_id: None,
                    trusted_producer_ids: Vec::new(),
                    materials_hashes: Vec::new(),
                    products_hashes: Vec::new(),
                    verification_time: format!("2026-04-13T00:0{idx}:00Z"),
                    result: MemoryOsVerificationStatus::Verified,
                    reason: Some("verified".into()),
                    attestation_kind: "replay-eval".into(),
                })
                .expect("verification result");
        }

        let rendered = render_promotion_with_tracker(&tracker, "json").expect("promotion report");
        let report: MemoryOsPromotionReport =
            serde_json::from_str(&rendered).expect("promotion json");

        assert!(report.eligible);
        assert!(report.resume_cutover_ready);
        assert_eq!(report.required_results.len(), 2);
        assert!(report
            .required_results
            .iter()
            .all(|record| record.independent && record.contamination_free));

        std::env::remove_var("CONTEXT_MEMORYOS_READ_MODEL_V1");
        std::env::remove_var("CONTEXT_MEMORYOS_RESUME_V1");
        std::env::remove_var("CONTEXT_MEMORYOS_HANDOFF_V1");
        std::env::remove_var("CONTEXT_MEMORYOS_STRICT_PROMOTION_V1");
    }

    #[test]
    fn build_inspect_report_renders_promoted_assertions_and_evidence_events() {
        let (_tmp, tracker, _) = temp_tracker();
        let project = "C:/tmp/project";
        tracker
            .record_worldview_event_for_project(
                project,
                "cargo-test",
                "cargo-test:C:/tmp/project",
                "context cargo test",
                "cargo test: 10 passed (1 suite, <time>)",
                "hash-a",
                None,
                "{}",
            )
            .expect("worldview");
        tracker
            .create_claim_lease_for_project(
                project,
                crate::core::tracking::ClaimLeaseType::Decision,
                "Cargo tests are currently green.",
                Some("Verified by the latest cargo test worldview fact."),
                crate::core::tracking::ClaimLeaseConfidence::High,
                None,
                &[crate::core::tracking::ClaimLeaseDependency {
                    kind: crate::core::tracking::ClaimLeaseDependencyKind::WorldviewSubject,
                    key: "cargo-test:C:/tmp/project".to_string(),
                    fingerprint: None,
                }],
                r#"["cargo test"]"#,
                "test",
            )
            .expect("claim");

        let report =
            build_inspect_report(&tracker, MemoryOsInspectionScope::Project, Some(project))
                .expect("inspect report");
        let rendered = render_inspect_text(&report);

        assert!(!report.compiled_memory.promoted_assertions.is_empty());
        assert!(rendered.contains("Promoted Assertions"));
        assert!(rendered.contains("Evidence Events"));
        assert!(rendered.contains("Cargo tests are currently green."));
        assert!(rendered.contains("target: C:/tmp/project"));
    }
}
