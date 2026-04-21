//! Deterministic worldview ledger and prompt compiler.

use crate::core::artifacts::ArtifactRenderResult;
use crate::core::filter::FilterLevel;
use crate::core::tracking::{
    ClaimLeaseConfidence, ClaimLeaseDependencyKind, ClaimLeaseRecord, ClaimLeaseStatus,
    ClaimLeaseType, CommandRecord, Tracker, WorldviewRecord,
};
use anyhow::Result;
use chrono::Utc;
use regex::Regex;
use serde::Serialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::OnceLock;

const DEFAULT_FETCH_MULTIPLIER: usize = 12;
const DEFAULT_MIN_FETCH: usize = 48;
const DEFAULT_RECENT_COMMANDS: usize = 6;
const DEFAULT_LIVE_CLAIMS: usize = 5;
const DEFAULT_OPEN_OBLIGATIONS: usize = 3;

#[derive(Debug, Clone, Serialize)]
pub struct ContextFact {
    pub observed_at: String,
    pub event_type: String,
    pub subject: String,
    pub status: String,
    pub summary: String,
    pub command_sig: String,
    pub artifact_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ArtifactHandle {
    pub artifact_id: String,
    pub reopen_hint: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ContextClaim {
    pub observed_at: String,
    pub claim_type: String,
    pub confidence: String,
    pub claim: String,
    pub rationale_capsule: Option<String>,
    pub dependencies: Vec<String>,
    pub evidence: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CompiledContext {
    pub generated_at: String,
    pub project_path: String,
    pub goal: Option<String>,
    pub current_state: Vec<ContextFact>,
    pub live_claims: Vec<ContextClaim>,
    pub open_obligations: Vec<ContextClaim>,
    pub auto_obligation_count: usize,
    pub recent_changes: Vec<ContextFact>,
    pub recent_commands: Vec<String>,
    pub recent_command_input_tokens: usize,
    pub artifact_handles: Vec<ArtifactHandle>,
    pub prompt: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct FailureFact {
    pub observed_at: String,
    pub event_type: String,
    pub subject: String,
    pub summary: String,
    pub details: Vec<String>,
    pub artifact_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct GrepFileSummary {
    path: String,
    matches: usize,
}

fn current_project_path_string() -> String {
    crate::core::utils::current_project_root_string()
}

fn canonical_path_string(path: &Path) -> String {
    normalize_display_path(
        path.canonicalize()
            .unwrap_or_else(|_| path.to_path_buf())
            .to_string_lossy()
            .as_ref(),
    )
}

fn normalize_display_path(path: &str) -> String {
    path.strip_prefix(r"\\?\").unwrap_or(path).to_string()
}

fn hash_text(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn short_hash(hash: &str) -> String {
    hash.chars().take(12).collect()
}

struct CommandObservation {
    summary: String,
    fingerprint_source: String,
    payload: serde_json::Value,
}

#[derive(Debug, Clone)]
pub struct ReplayCommandObservation {
    pub summary: String,
    pub fingerprint_source: String,
    pub payload_json: String,
}

pub fn project_subject(kind: &str) -> String {
    format!("{}:{}", kind, current_project_path_string())
}

fn record_worldview(
    event_type: &str,
    subject_key: &str,
    command_sig: &str,
    summary: &str,
    fingerprint_source: &str,
    artifact_id: Option<&str>,
    payload_json: String,
) -> Result<String> {
    if cfg!(test) {
        return Ok("test".to_string());
    }
    let tracker = Tracker::new()?;
    tracker.record_worldview_event(
        event_type,
        subject_key,
        command_sig,
        summary,
        &hash_text(fingerprint_source),
        artifact_id,
        &payload_json,
    )
}

pub fn observe_read(
    file: &Path,
    content: &str,
    level: FilterLevel,
    max_lines: Option<usize>,
    tail_lines: Option<usize>,
    line_numbers: bool,
    artifact: &ArtifactRenderResult,
) -> Result<String> {
    let path = canonical_path_string(file);
    let lines = content.lines().count();
    let bytes = content.len();
    let hash = hash_text(content);
    let summary = format!(
        "{} | {} lines | {} bytes | hash {}",
        path,
        lines,
        bytes,
        short_hash(&hash)
    );
    let payload = json!({
        "path": path,
        "bytes": bytes,
        "lines": lines,
        "hash": hash,
        "filter_level": level.to_string(),
        "max_lines": max_lines,
        "tail_lines": tail_lines,
        "line_numbers": line_numbers,
        "artifact_id": artifact.artifact_id,
    });

    record_worldview(
        "read",
        &format!("file:{}", path),
        &format!("context read {}", file.display()),
        &summary,
        content,
        artifact.artifact_id.as_deref(),
        payload.to_string(),
    )
}

pub fn observe_grep(
    pattern: &str,
    path: &str,
    file_type: Option<&str>,
    total_matches: usize,
    by_file: &HashMap<String, Vec<(usize, String)>>,
    fingerprint_source: &str,
    artifact: &ArtifactRenderResult,
) -> Result<String> {
    let mut files: Vec<GrepFileSummary> = by_file
        .iter()
        .map(|(file, matches)| GrepFileSummary {
            path: file.clone(),
            matches: matches.len(),
        })
        .collect();
    files.sort_by(|a, b| a.path.cmp(&b.path));

    let summary = format!(
        "grep '{}' in {} | {} matches across {} file(s)",
        pattern,
        path,
        total_matches,
        files.len()
    );
    let payload = json!({
        "pattern": pattern,
        "path": path,
        "file_type": file_type,
        "total_matches": total_matches,
        "file_count": files.len(),
        "files": files,
        "artifact_id": artifact.artifact_id,
    });

    record_worldview(
        "grep",
        &format!("grep:{}:{}:{}", path, file_type.unwrap_or("*"), pattern),
        &format!("context grep {}", pattern),
        &summary,
        fingerprint_source,
        artifact.artifact_id.as_deref(),
        payload.to_string(),
    )
}

pub fn observe_diff(
    left: &str,
    right: &str,
    added: usize,
    removed: usize,
    modified: usize,
    fingerprint_source: &str,
    artifact: &ArtifactRenderResult,
) -> Result<String> {
    let summary = format!(
        "{} → {} | +{} -{} ~{}",
        left, right, added, removed, modified
    );
    let payload = json!({
        "left": left,
        "right": right,
        "added": added,
        "removed": removed,
        "modified": modified,
        "artifact_id": artifact.artifact_id,
    });

    record_worldview(
        "diff",
        &format!("diff:{}->{}", left, right),
        "munin diff",
        &summary,
        fingerprint_source,
        artifact.artifact_id.as_deref(),
        payload.to_string(),
    )
}

pub fn observe_git_status(
    branch: Option<&str>,
    staged: usize,
    modified: usize,
    untracked: usize,
    conflicts: usize,
    fingerprint_source: &str,
    artifact: Option<&str>,
) -> Result<String> {
    let repo = current_project_path_string();
    let summary = format!(
        "{} | branch {} | staged {} | modified {} | untracked {} | conflicts {}",
        repo,
        branch.unwrap_or("detached"),
        staged,
        modified,
        untracked,
        conflicts
    );
    let payload = json!({
        "repo": repo,
        "branch": branch,
        "staged": staged,
        "modified": modified,
        "untracked": untracked,
        "conflicts": conflicts,
        "artifact_id": artifact,
    });

    record_worldview(
        "git-status",
        &format!("git-status:{}", repo),
        "context git status",
        &summary,
        fingerprint_source,
        artifact,
        payload.to_string(),
    )
}

pub fn observe_command_summary(
    event_type: &str,
    subject_key: &str,
    command_sig: &str,
    filtered_output: &str,
    exit_code: i32,
    artifact: &ArtifactRenderResult,
) -> Result<String> {
    let normalized = normalize_transient_output(filtered_output);
    let observation = build_command_observation(
        event_type,
        command_sig,
        &normalized,
        filtered_output,
        exit_code,
        artifact,
    );

    record_worldview(
        event_type,
        subject_key,
        command_sig,
        &observation.summary,
        &observation.fingerprint_source,
        artifact.artifact_id.as_deref(),
        observation.payload.to_string(),
    )
}

pub fn replay_command_observation(
    event_type: &str,
    command_sig: &str,
    filtered_output: &str,
    exit_code: i32,
) -> Result<ReplayCommandObservation> {
    let normalized = normalize_transient_output(filtered_output);
    let observation = build_command_observation(
        event_type,
        command_sig,
        &normalized,
        filtered_output,
        exit_code,
        &ArtifactRenderResult {
            rendered: filtered_output.to_string(),
            artifact_id: None,
            event_kind: None,
        },
    );

    Ok(ReplayCommandObservation {
        summary: observation.summary,
        fingerprint_source: observation.fingerprint_source,
        payload_json: observation.payload.to_string(),
    })
}

#[allow(dead_code)]
pub fn compile_context(
    goal: Option<&str>,
    current_limit: usize,
    change_limit: usize,
) -> Result<CompiledContext> {
    let tracker = Tracker::new()?;
    compile_context_with_tracker(
        &tracker,
        &current_project_path_string(),
        goal,
        current_limit,
        change_limit,
    )
}

#[allow(dead_code)]
pub fn compile_context_packet_source(
    goal: Option<&str>,
    current_limit: usize,
    change_limit: usize,
    failure_limit: usize,
) -> Result<(CompiledContext, Vec<FailureFact>)> {
    let tracker = Tracker::new()?;
    compile_context_packet_source_with_tracker(
        &tracker,
        &current_project_path_string(),
        goal,
        current_limit,
        change_limit,
        failure_limit,
    )
}

pub fn collect_failures(limit: usize) -> Result<Vec<FailureFact>> {
    let tracker = Tracker::new()?;
    collect_failures_with_tracker(&tracker, &current_project_path_string(), limit)
}

#[allow(dead_code)]
fn compile_context_with_tracker(
    tracker: &Tracker,
    project_path: &str,
    goal: Option<&str>,
    current_limit: usize,
    change_limit: usize,
) -> Result<CompiledContext> {
    Ok(compile_context_packet_source_with_tracker(
        tracker,
        project_path,
        goal,
        current_limit,
        change_limit,
        DEFAULT_OPEN_OBLIGATIONS,
    )?
    .0)
}

pub(crate) fn compile_context_packet_source_with_tracker(
    tracker: &Tracker,
    project_path: &str,
    goal: Option<&str>,
    current_limit: usize,
    change_limit: usize,
    failure_limit: usize,
) -> Result<(CompiledContext, Vec<FailureFact>)> {
    tracker.refresh_claim_lease_statuses(Some(project_path))?;
    let fetch_limit =
        (current_limit.max(change_limit) * DEFAULT_FETCH_MULTIPLIER).max(DEFAULT_MIN_FETCH);
    let requested_failure_limit = failure_limit;
    let failure_fetch_limit = requested_failure_limit.max(DEFAULT_OPEN_OBLIGATIONS);
    let event_fetch_limit = fetch_limit.max(failure_fetch_limit.max(DEFAULT_MIN_FETCH)) * 2;
    let events = load_project_worldview_events(tracker, project_path, event_fetch_limit)?;
    let mut failures = collect_failures_from_events(&events, failure_fetch_limit);
    let current_state = latest_facts(&events, current_limit);
    let recent_changes = latest_recent_changes(&events, change_limit);
    let claim_records = tracker.get_claim_leases_filtered(
        fetch_limit,
        Some(project_path),
        Some(&[ClaimLeaseStatus::Live]),
    )?;
    let live_claims = select_relevant_claims(
        &claim_records,
        &current_state,
        &recent_changes,
        DEFAULT_LIVE_CLAIMS,
        false,
    );
    let explicit_open_obligations = select_relevant_claims(
        &claim_records,
        &current_state,
        &recent_changes,
        DEFAULT_OPEN_OBLIGATIONS,
        true,
    );
    let synthesized_obligations = synthesize_failure_obligations(
        &failures,
        &explicit_open_obligations,
        DEFAULT_OPEN_OBLIGATIONS,
    );
    let auto_obligation_count = synthesized_obligations.len();
    let mut open_obligations = explicit_open_obligations;
    open_obligations.extend(synthesized_obligations);
    open_obligations.truncate(DEFAULT_OPEN_OBLIGATIONS);
    let recent_command_records =
        tracker.get_recent_filtered(DEFAULT_RECENT_COMMANDS, Some(project_path))?;
    let recent_command_input_tokens = recent_command_records
        .iter()
        .map(|record| record.input_tokens)
        .sum::<usize>();
    let recent_commands = recent_command_records
        .iter()
        .map(format_command_record)
        .collect::<Vec<_>>();
    let artifact_handles = collect_artifacts(
        &current_state,
        &recent_changes,
        &live_claims,
        &open_obligations,
    );
    let prompt = render_prompt(
        goal,
        project_path,
        &current_state,
        &live_claims,
        &open_obligations,
        &recent_changes,
        &recent_commands,
        &artifact_handles,
    );
    failures.truncate(requested_failure_limit);

    Ok((
        CompiledContext {
            generated_at: Utc::now().to_rfc3339(),
            project_path: project_path.to_string(),
            goal: goal.map(|value| value.to_string()),
            current_state,
            live_claims,
            open_obligations,
            auto_obligation_count,
            recent_changes,
            recent_commands,
            recent_command_input_tokens,
            artifact_handles,
            prompt,
        },
        failures,
    ))
}

fn collect_failures_with_tracker(
    tracker: &Tracker,
    project_path: &str,
    limit: usize,
) -> Result<Vec<FailureFact>> {
    let fetch_limit = limit.max(DEFAULT_MIN_FETCH) * 2;
    let events = tracker
        .get_worldview_events_filtered(fetch_limit, Some(project_path))?
        .into_iter()
        .filter(|event| event_relevant_to_project(event, project_path))
        .collect::<Vec<_>>();

    Ok(collect_failures_from_events(&events, limit))
}

fn load_project_worldview_events(
    tracker: &Tracker,
    project_path: &str,
    limit: usize,
) -> Result<Vec<WorldviewRecord>> {
    Ok(tracker
        .get_worldview_events_filtered(limit, Some(project_path))?
        .into_iter()
        .filter(|event| event_relevant_to_project(event, project_path))
        .collect::<Vec<_>>())
}

fn collect_failures_from_events(events: &[WorldviewRecord], limit: usize) -> Vec<FailureFact> {
    if limit == 0 {
        return Vec::new();
    }

    let mut failures = Vec::new();
    let mut seen = HashSet::new();
    for event in events {
        if !seen.insert(event.subject_key.clone()) {
            continue;
        }
        if let Some(failure) = to_failure_fact(event) {
            failures.push(failure);
            if failures.len() >= limit {
                break;
            }
        }
    }
    failures
}

fn latest_facts(events: &[WorldviewRecord], limit: usize) -> Vec<ContextFact> {
    let mut seen = HashSet::new();
    let mut latest = Vec::new();
    for event in events {
        if seen.insert(event.subject_key.clone()) {
            latest.push(to_context_fact(event));
            if latest.len() >= limit {
                break;
            }
        }
    }
    latest
}

fn latest_recent_changes(events: &[WorldviewRecord], limit: usize) -> Vec<ContextFact> {
    let mut seen = HashSet::new();
    let mut latest = Vec::new();

    for event in events {
        if event.status == "unchanged" || !seen.insert(event.subject_key.clone()) {
            continue;
        }

        latest.push(event);
        if latest.len() >= limit * 3 {
            break;
        }
    }

    latest.sort_by(|left, right| {
        actionable_change_weight(right)
            .cmp(&actionable_change_weight(left))
            .then(right.timestamp.cmp(&left.timestamp))
    });

    let mut content_seen: HashSet<(String, String)> = HashSet::new();
    latest.retain(|event| content_seen.insert((event.event_type.clone(), event.summary.clone())));
    latest.truncate(limit);
    latest.into_iter().map(to_context_fact).collect()
}

fn actionable_change_weight(event: &WorldviewRecord) -> usize {
    if to_failure_fact(event).is_some() {
        return 3;
    }

    let summary = event.summary.to_lowercase();
    if summary.contains("warning") {
        return 2;
    }

    1
}

fn event_relevant_to_project(event: &WorldviewRecord, project_path: &str) -> bool {
    if event.subject_key.contains(project_path) {
        return true;
    }

    let payload: serde_json::Value = match serde_json::from_str(&event.payload_json) {
        Ok(value) => value,
        Err(_) => return true,
    };

    match event.event_type.as_str() {
        "read" => payload
            .get("path")
            .and_then(|v| v.as_str())
            .map(|path| path.starts_with(project_path))
            .unwrap_or(false),
        "grep" => payload
            .get("path")
            .and_then(|v| v.as_str())
            .map(|path| !path.contains(":\\") || path.starts_with(project_path))
            .unwrap_or(true),
        "git-status" => payload
            .get("repo")
            .and_then(|v| v.as_str())
            .map(|repo| repo.starts_with(project_path))
            .unwrap_or(true),
        _ => true,
    }
}

fn to_context_fact(event: &WorldviewRecord) -> ContextFact {
    ContextFact {
        observed_at: event.timestamp.to_rfc3339(),
        event_type: event.event_type.clone(),
        subject: event.subject_key.clone(),
        status: event.status.clone(),
        summary: event.summary.clone(),
        command_sig: event.command_sig.clone(),
        artifact_id: event.artifact_id.clone(),
    }
}

fn to_failure_fact(event: &WorldviewRecord) -> Option<FailureFact> {
    let payload: serde_json::Value = serde_json::from_str(&event.payload_json).ok()?;
    let event_type = event.event_type.as_str();

    let mut details = Vec::new();
    let summary = match event_type {
        "cargo-test" | "pytest" | "go-test" => {
            let failed = payload.get("failed").and_then(|v| v.as_u64()).unwrap_or(0);
            let summary_text = payload
                .get("summary")
                .and_then(|v| v.as_str())
                .unwrap_or(&event.summary)
                .to_string();
            if failed == 0
                && !summary_text.to_lowercase().contains("error")
                && !summary_text.to_lowercase().contains("fail")
            {
                return None;
            }
            if let Some(tests) = payload.get("failed_tests").and_then(|v| v.as_array()) {
                for value in tests.iter().take(5).filter_map(|item| item.as_str()) {
                    push_unique(&mut details, value.to_string());
                }
            }
            if let Some(packages) = payload
                .get("build_failed_packages")
                .and_then(|v| v.as_array())
            {
                for value in packages.iter().take(5).filter_map(|item| item.as_str()) {
                    push_unique(&mut details, format!("build failed: {}", value));
                }
            }
            summary_text
        }
        "tsc" | "mypy" => {
            let errors = payload.get("errors").and_then(|v| v.as_u64()).unwrap_or(0);
            if errors == 0 {
                return None;
            }
            if let Some(files) = payload.get("error_files").and_then(|v| v.as_array()) {
                for value in files.iter().take(5) {
                    if let Some(path) = value.get("path").and_then(|v| v.as_str()) {
                        let count = value.get("errors").and_then(|v| v.as_u64()).unwrap_or(0);
                        push_unique(&mut details, format!("{} ({} errors)", path, count));
                    }
                }
            }
            if let Some(fileless) = payload.get("fileless_errors").and_then(|v| v.as_array()) {
                for value in fileless.iter().take(3).filter_map(|item| item.as_str()) {
                    push_unique(&mut details, value.to_string());
                }
            }
            payload
                .get("summary")
                .and_then(|v| v.as_str())
                .unwrap_or(&event.summary)
                .to_string()
        }
        "ruff" | "rubocop" => {
            let issue_key = if event_type == "ruff" {
                "issues"
            } else {
                "offenses"
            };
            let issues = payload.get(issue_key).and_then(|v| v.as_u64()).unwrap_or(0);
            if issues == 0 {
                return None;
            }
            let files_key = if event_type == "ruff" {
                "files"
            } else {
                "files"
            };
            if let Some(files) = payload.get("offense_files").and_then(|v| v.as_array()) {
                for value in files.iter().take(5) {
                    if let Some(path) = value.get("path").and_then(|v| v.as_str()) {
                        let count = value
                            .get(if event_type == "ruff" {
                                "issues"
                            } else {
                                "offenses"
                            })
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0);
                        push_unique(&mut details, format!("{} ({})", path, count));
                    }
                }
            } else if let Some(total_files) = payload.get(files_key).and_then(|v| v.as_u64()) {
                push_unique(&mut details, format!("{} files affected", total_files));
            }
            payload
                .get("summary")
                .and_then(|v| v.as_str())
                .unwrap_or(&event.summary)
                .to_string()
        }
        "rspec" => {
            let failed = payload.get("failed").and_then(|v| v.as_u64()).unwrap_or(0);
            if failed == 0 {
                return None;
            }
            if let Some(examples) = payload.get("failure_examples").and_then(|v| v.as_array()) {
                for value in examples.iter().take(5).filter_map(|item| item.as_str()) {
                    push_unique(&mut details, value.to_string());
                }
            }
            payload
                .get("summary")
                .and_then(|v| v.as_str())
                .unwrap_or(&event.summary)
                .to_string()
        }
        "next-build" => {
            let errors = payload.get("errors").and_then(|v| v.as_u64()).unwrap_or(0);
            if errors == 0 {
                return None;
            }
            if let Some(bundle) = payload.get("largest_bundle_route").and_then(|v| v.as_str()) {
                push_unique(&mut details, format!("largest bundle: {}", bundle));
            }
            payload
                .get("summary")
                .and_then(|v| v.as_str())
                .unwrap_or(&event.summary)
                .to_string()
        }
        _ => {
            let generic_errors = payload.get("errors").and_then(|v| v.as_u64()).unwrap_or(0);
            let generic_failed = payload.get("failed").and_then(|v| v.as_u64()).unwrap_or(0);
            if generic_errors == 0 && generic_failed == 0 {
                return None;
            }
            payload
                .get("summary")
                .and_then(|v| v.as_str())
                .unwrap_or(&event.summary)
                .to_string()
        }
    };

    Some(FailureFact {
        observed_at: event.timestamp.to_rfc3339(),
        event_type: event.event_type.clone(),
        subject: event.subject_key.clone(),
        summary,
        details,
        artifact_id: event.artifact_id.clone(),
    })
}

fn to_context_claim(record: &ClaimLeaseRecord) -> ContextClaim {
    let mut dependencies = Vec::new();
    let mut evidence =
        serde_json::from_str::<Vec<String>>(&record.evidence_json).unwrap_or_default();

    for dependency in &record.dependencies {
        match dependency.kind {
            ClaimLeaseDependencyKind::WorldviewSubject => {
                dependencies.push(format!("worldview:{}", dependency.key));
            }
            ClaimLeaseDependencyKind::Artifact => {
                dependencies.push(format!("artifact:{}", dependency.key));
                if !evidence.iter().any(|item| item == &dependency.key) {
                    evidence.push(dependency.key.clone());
                }
            }
            ClaimLeaseDependencyKind::UserDecision => {
                dependencies.push(format!("user-decision:{}", dependency.key));
            }
        }
    }

    ContextClaim {
        observed_at: record.timestamp.to_rfc3339(),
        claim_type: record.claim_type.to_string(),
        confidence: record.confidence.to_string(),
        claim: record.claim_text.clone(),
        rationale_capsule: record.rationale_capsule.clone(),
        dependencies,
        evidence,
    }
}

fn select_relevant_claims(
    records: &[ClaimLeaseRecord],
    current_state: &[ContextFact],
    recent_changes: &[ContextFact],
    limit: usize,
    obligations_only: bool,
) -> Vec<ContextClaim> {
    let mut subject_scores = HashMap::new();
    for fact in current_state {
        let score = fact_relevance_weight(fact, false);
        subject_scores
            .entry(fact.subject.clone())
            .and_modify(|current: &mut usize| *current = (*current).max(score))
            .or_insert(score);
    }
    for fact in recent_changes {
        let score = fact_relevance_weight(fact, true);
        subject_scores
            .entry(fact.subject.clone())
            .and_modify(|current: &mut usize| *current = (*current).max(score))
            .or_insert(score);
    }
    let current_artifacts = current_state
        .iter()
        .chain(recent_changes.iter())
        .filter_map(|fact| fact.artifact_id.clone())
        .collect::<HashSet<_>>();

    let mut scored = records
        .iter()
        .filter(|record| {
            (record.claim_type == ClaimLeaseType::Obligation) == obligations_only
                && record.status == ClaimLeaseStatus::Live
        })
        .map(|record| {
            let mut score = 0usize;
            for dependency in &record.dependencies {
                match dependency.kind {
                    ClaimLeaseDependencyKind::WorldviewSubject => {
                        if let Some(subject_score) = subject_scores.get(&dependency.key) {
                            score += *subject_score;
                        }
                    }
                    ClaimLeaseDependencyKind::Artifact => {
                        if current_artifacts.contains(&dependency.key) {
                            score += 2;
                        }
                    }
                    ClaimLeaseDependencyKind::UserDecision => {
                        score += 2;
                    }
                }
            }
            score += claim_type_weight(record.claim_type, obligations_only);
            score += claim_confidence_weight(record.confidence);
            (score, record.timestamp, to_context_claim(record))
        })
        .collect::<Vec<_>>();

    scored.sort_by(|left, right| right.0.cmp(&left.0).then(right.1.cmp(&left.1)));

    let mut selected = scored
        .iter()
        .filter(|(score, _, _)| *score > 0)
        .take(limit)
        .map(|(_, _, claim)| claim.clone())
        .collect::<Vec<_>>();

    if selected.len() < limit {
        let mut seen = selected
            .iter()
            .map(|claim| claim.claim.clone())
            .collect::<HashSet<_>>();
        for (_, _, claim) in scored {
            if selected.len() >= limit {
                break;
            }
            if seen.insert(claim.claim.clone()) {
                selected.push(claim);
            }
        }
    }

    selected
}

fn synthesize_failure_obligations(
    failures: &[FailureFact],
    existing_obligations: &[ContextClaim],
    limit: usize,
) -> Vec<ContextClaim> {
    if limit == 0 {
        return Vec::new();
    }

    let mut covered_subjects = existing_obligations
        .iter()
        .flat_map(|claim| claim.dependencies.iter())
        .filter_map(|dependency| dependency.strip_prefix("worldview:"))
        .map(ToString::to_string)
        .collect::<HashSet<_>>();

    let mut obligations = Vec::new();
    for failure in failures {
        if obligations.len() >= limit {
            break;
        }
        if !covered_subjects.insert(failure.subject.clone()) {
            continue;
        }

        let mut dependencies = vec![format!("worldview:{}", failure.subject)];
        let mut evidence = failure.details.clone();
        if let Some(artifact_id) = &failure.artifact_id {
            dependencies.push(format!("artifact:{}", artifact_id));
            if !evidence.iter().any(|item| item == artifact_id) {
                evidence.push(artifact_id.clone());
            }
        }

        obligations.push(ContextClaim {
            observed_at: failure.observed_at.clone(),
            claim_type: ClaimLeaseType::Obligation.to_string(),
            confidence: ClaimLeaseConfidence::Medium.to_string(),
            claim: format!("Resolve active failure: {}", failure.summary),
            rationale_capsule: Some(
                "Auto-synthesized from the current worldview failure until an explicit claim lease supersedes it.".to_string(),
            ),
            dependencies,
            evidence,
        });
    }

    obligations
}

fn fact_relevance_weight(fact: &ContextFact, is_recent_change: bool) -> usize {
    let summary = fact.summary.to_lowercase();
    let failure_like = summary.contains(" failed")
        || summary.contains("error")
        || summary.contains("issues")
        || summary.contains("offenses")
        || summary.contains("build failed")
        || summary.contains("conflict");

    match (is_recent_change, failure_like) {
        (true, true) => 7,
        (false, true) => 5,
        (true, false) => 4,
        (false, false) => 2,
    }
}

fn claim_type_weight(claim_type: ClaimLeaseType, obligations_only: bool) -> usize {
    if obligations_only {
        return 4;
    }

    match claim_type {
        ClaimLeaseType::Rejection | ClaimLeaseType::HypothesisTested => 3,
        ClaimLeaseType::BenignAnomaly => 2,
        ClaimLeaseType::Decision => 1,
        ClaimLeaseType::Obligation => 0,
    }
}

fn claim_confidence_weight(confidence: ClaimLeaseConfidence) -> usize {
    match confidence {
        ClaimLeaseConfidence::High => 2,
        ClaimLeaseConfidence::Medium => 1,
        ClaimLeaseConfidence::Low => 0,
    }
}

fn collect_artifacts(
    current_state: &[ContextFact],
    recent_changes: &[ContextFact],
    live_claims: &[ContextClaim],
    open_obligations: &[ContextClaim],
) -> Vec<ArtifactHandle> {
    let mut seen = HashSet::new();
    let mut handles = Vec::new();

    for fact in current_state.iter().chain(recent_changes.iter()) {
        if let Some(artifact_id) = &fact.artifact_id {
            if seen.insert(artifact_id.clone()) {
                let reopen_hint = if fact.event_type == "diff" {
                    format!("munin diff {artifact_id} <other-artifact>")
                } else {
                    format!("munin show {artifact_id}")
                };
                handles.push(ArtifactHandle {
                    artifact_id: artifact_id.clone(),
                    reopen_hint,
                });
            }
        }
    }

    for claim in live_claims.iter().chain(open_obligations.iter()) {
        for evidence in &claim.evidence {
            if !crate::core::artifacts::is_artifact_id(evidence) || !seen.insert(evidence.clone()) {
                continue;
            }
            handles.push(ArtifactHandle {
                artifact_id: evidence.clone(),
                reopen_hint: format!("munin show {}", evidence),
            });
        }
    }

    handles
}

fn format_command_record(record: &CommandRecord) -> String {
    format!(
        "{} | saved {} tokens | {:.1}%",
        record.context_cmd, record.saved_tokens, record.savings_pct
    )
}

fn summarize_compact_output(output: &str, exit_code: i32) -> String {
    let meaningful = output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .take(3)
        .collect::<Vec<_>>();

    let mut summary = if meaningful.is_empty() {
        if exit_code == 0 {
            "completed with no output".to_string()
        } else {
            format!("failed with exit code {}", exit_code)
        }
    } else {
        meaningful.join(" | ")
    };

    if exit_code != 0 && !summary.contains("exit code") {
        summary = format!("exit {} | {}", exit_code, summary);
    }

    if summary.chars().count() > 220 {
        let clipped: String = summary.chars().take(217).collect();
        return format!("{}...", clipped);
    }

    summary
}

fn build_command_observation(
    event_type: &str,
    command_sig: &str,
    normalized_output: &str,
    raw_filtered_output: &str,
    exit_code: i32,
    artifact: &ArtifactRenderResult,
) -> CommandObservation {
    match event_type {
        "cargo-test" => cargo_test_observation(
            command_sig,
            normalized_output,
            raw_filtered_output,
            artifact,
        ),
        "pytest" => pytest_observation(
            command_sig,
            normalized_output,
            raw_filtered_output,
            artifact,
        ),
        "tsc" => tsc_observation(
            command_sig,
            normalized_output,
            raw_filtered_output,
            artifact,
        ),
        "go-test" => go_test_observation(
            command_sig,
            normalized_output,
            raw_filtered_output,
            artifact,
        ),
        "mypy" => mypy_observation(
            command_sig,
            normalized_output,
            raw_filtered_output,
            artifact,
        ),
        "rspec" => rspec_observation(
            command_sig,
            normalized_output,
            raw_filtered_output,
            artifact,
        ),
        "rubocop" => rubocop_observation(
            command_sig,
            normalized_output,
            raw_filtered_output,
            artifact,
        ),
        "next-build" => next_build_observation(
            command_sig,
            normalized_output,
            raw_filtered_output,
            artifact,
        ),
        "ruff" | "ruff-format" => ruff_observation(
            event_type,
            command_sig,
            normalized_output,
            raw_filtered_output,
            artifact,
        ),
        _ => {
            let summary = summarize_compact_output(normalized_output, exit_code);
            CommandObservation {
                summary: summary.clone(),
                fingerprint_source: normalized_output.to_string(),
                payload: json!({
                    "event_type": event_type,
                    "command_sig": command_sig,
                    "exit_code": exit_code,
                    "line_count": raw_filtered_output.lines().count(),
                    "summary": summary,
                    "artifact_id": artifact.artifact_id,
                }),
            }
        }
    }
}

fn first_non_empty_line(output: &str) -> &str {
    output
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("")
}

fn push_unique(values: &mut Vec<String>, value: impl Into<String>) {
    let value = value.into();
    if !value.is_empty() && !values.iter().any(|existing| existing == &value) {
        values.push(value);
    }
}

fn semantic_observation(
    summary: String,
    fingerprint_payload: serde_json::Value,
    payload: serde_json::Value,
) -> CommandObservation {
    CommandObservation {
        summary,
        fingerprint_source: fingerprint_payload.to_string(),
        payload,
    }
}

fn cargo_test_observation(
    command_sig: &str,
    normalized_output: &str,
    _raw_filtered_output: &str,
    artifact: &ArtifactRenderResult,
) -> CommandObservation {
    static CARGO_TEST_RE: OnceLock<Regex> = OnceLock::new();
    let re = CARGO_TEST_RE.get_or_init(|| {
        Regex::new(
            r"cargo test:\s+(?P<passed>\d+)\s+passed(?:,\s+(?P<failed>\d+)\s+failed)?(?:,\s+(?P<ignored>\d+)\s+ignored)?(?:,\s+(?P<filtered>\d+)\s+filtered out)?\s+\((?P<suites>\d+)\s+suites?,\s+(?P<time><time>|[\d.]+s)\)",
        )
        .expect("cargo test regex")
    });

    if let Some(caps) = re.captures(normalized_output) {
        let passed = caps
            .name("passed")
            .map_or("0", |m| m.as_str())
            .parse::<usize>()
            .unwrap_or(0);
        let failed = caps
            .name("failed")
            .map_or("0", |m| m.as_str())
            .parse::<usize>()
            .unwrap_or(0);
        let ignored = caps
            .name("ignored")
            .map_or("0", |m| m.as_str())
            .parse::<usize>()
            .unwrap_or(0);
        let filtered = caps
            .name("filtered")
            .map_or("0", |m| m.as_str())
            .parse::<usize>()
            .unwrap_or(0);
        let suites = caps
            .name("suites")
            .map_or("0", |m| m.as_str())
            .parse::<usize>()
            .unwrap_or(0);
        let time = caps.name("time").map_or("<time>", |m| m.as_str());
        let summary = format!(
            "cargo test: {} passed{}{}{} ({} suite{}, {})",
            passed,
            if failed > 0 {
                format!(", {} failed", failed)
            } else {
                String::new()
            },
            if ignored > 0 {
                format!(", {} ignored", ignored)
            } else {
                String::new()
            },
            if filtered > 0 {
                format!(", {} filtered out", filtered)
            } else {
                String::new()
            },
            suites,
            if suites == 1 { "" } else { "s" },
            time
        );
        return CommandObservation {
            summary: summary.clone(),
            fingerprint_source: normalized_output.to_string(),
            payload: json!({
                "event_type": "cargo-test",
                "command_sig": command_sig,
                "passed": passed,
                "failed": failed,
                "ignored": ignored,
                "filtered_out": filtered,
                "suites": suites,
                "time": time,
                "summary": summary,
                "artifact_id": artifact.artifact_id,
            }),
        };
    }

    CommandObservation {
        summary: summarize_compact_output(normalized_output, 0),
        fingerprint_source: normalized_output.to_string(),
        payload: json!({
            "event_type": "cargo-test",
            "command_sig": command_sig,
            "summary": summarize_compact_output(normalized_output, 0),
            "artifact_id": artifact.artifact_id,
        }),
    }
}

fn go_test_observation(
    command_sig: &str,
    normalized_output: &str,
    raw_filtered_output: &str,
    artifact: &ArtifactRenderResult,
) -> CommandObservation {
    static GO_TEST_RE: OnceLock<Regex> = OnceLock::new();
    static GO_TEST_BUILD_FAIL_RE: OnceLock<Regex> = OnceLock::new();
    static GO_TEST_PACKAGE_FAIL_RE: OnceLock<Regex> = OnceLock::new();
    static GO_TEST_CASE_FAIL_RE: OnceLock<Regex> = OnceLock::new();

    let first_line = first_non_empty_line(normalized_output);
    if first_line == "Go test: No tests found" {
        let summary = "go test: no tests found".to_string();
        return semantic_observation(
            summary.clone(),
            json!({
                "no_tests": true,
            }),
            json!({
                "event_type": "go-test",
                "command_sig": command_sig,
                "no_tests": true,
                "summary": summary,
                "artifact_id": artifact.artifact_id,
            }),
        );
    }

    let summary_re = GO_TEST_RE.get_or_init(|| {
        Regex::new(
            r"^Go test:\s+(?P<passed>\d+)\s+passed(?:,\s+(?P<failed>\d+)\s+failed)?(?:,\s+(?P<skipped>\d+)\s+skipped)?\s+in\s+(?P<packages>\d+)\s+packages?$",
        )
        .expect("go test regex")
    });

    if let Some(caps) = summary_re.captures(first_line) {
        let passed = caps["passed"].parse::<usize>().unwrap_or(0);
        let failed = caps
            .name("failed")
            .map_or("0", |m| m.as_str())
            .parse::<usize>()
            .unwrap_or(0);
        let skipped = caps
            .name("skipped")
            .map_or("0", |m| m.as_str())
            .parse::<usize>()
            .unwrap_or(0);
        let packages = caps["packages"].parse::<usize>().unwrap_or(0);

        let build_fail_re = GO_TEST_BUILD_FAIL_RE.get_or_init(|| {
            Regex::new(r"^(?P<package>.+?)\s+\[build failed\]$").expect("go test build fail regex")
        });
        let package_fail_re = GO_TEST_PACKAGE_FAIL_RE.get_or_init(|| {
            Regex::new(r"^(?P<package>.+?)\s+\(\d+\s+passed,\s+\d+\s+failed\)$")
                .expect("go test package fail regex")
        });
        let case_fail_re = GO_TEST_CASE_FAIL_RE.get_or_init(|| {
            Regex::new(r"^\[FAIL\]\s+(?P<test>.+)$").expect("go test case fail regex")
        });

        let mut build_failed_packages = Vec::new();
        let mut failed_packages = Vec::new();
        let mut failed_tests = Vec::new();
        let mut current_package: Option<String> = None;

        for line in normalized_output.lines().map(str::trim) {
            if line.is_empty() || line == first_line {
                continue;
            }
            if let Some(package_caps) = build_fail_re.captures(line) {
                let package = package_caps["package"].to_string();
                push_unique(&mut build_failed_packages, package.clone());
                current_package = Some(package);
                continue;
            }
            if let Some(package_caps) = package_fail_re.captures(line) {
                let package = package_caps["package"].to_string();
                push_unique(&mut failed_packages, package.clone());
                current_package = Some(package);
                continue;
            }
            if let Some(test_caps) = case_fail_re.captures(line) {
                let test_name = test_caps["test"].to_string();
                let scoped_test = current_package
                    .as_ref()
                    .map(|package| format!("{package}::{test_name}"))
                    .unwrap_or(test_name);
                push_unique(&mut failed_tests, scoped_test);
            }
        }

        build_failed_packages.sort();
        failed_packages.sort();
        failed_tests.sort();

        let summary = format!(
            "go test: {} passed{}{} across {} package{}",
            passed,
            if failed > 0 {
                format!(", {} failed", failed)
            } else {
                String::new()
            },
            if skipped > 0 {
                format!(", {} skipped", skipped)
            } else {
                String::new()
            },
            packages,
            if packages == 1 { "" } else { "s" }
        );

        return semantic_observation(
            summary.clone(),
            json!({
                "passed": passed,
                "failed": failed,
                "skipped": skipped,
                "packages": packages,
                "build_failed_packages": build_failed_packages,
                "failed_packages": failed_packages,
                "failed_tests": failed_tests,
            }),
            json!({
                "event_type": "go-test",
                "command_sig": command_sig,
                "passed": passed,
                "failed": failed,
                "skipped": skipped,
                "packages": packages,
                "build_failed_packages": build_failed_packages,
                "failed_packages": failed_packages,
                "failed_tests": failed_tests,
                "summary": summary,
                "artifact_id": artifact.artifact_id,
            }),
        );
    }

    CommandObservation {
        summary: summarize_compact_output(normalized_output, 0),
        fingerprint_source: normalized_output.to_string(),
        payload: json!({
            "event_type": "go-test",
            "command_sig": command_sig,
            "summary": summarize_compact_output(normalized_output, 0),
            "artifact_id": artifact.artifact_id,
            "line_count": raw_filtered_output.lines().count(),
        }),
    }
}

fn pytest_observation(
    command_sig: &str,
    normalized_output: &str,
    raw_filtered_output: &str,
    artifact: &ArtifactRenderResult,
) -> CommandObservation {
    static PYTEST_RE: OnceLock<Regex> = OnceLock::new();
    let re = PYTEST_RE.get_or_init(|| {
        Regex::new(r"Pytest:\s+(?P<passed>\d+)\s+passed(?:,\s+(?P<failed>\d+)\s+failed)?(?:,\s+(?P<skipped>\d+)\s+skipped)?").expect("pytest regex")
    });
    if let Some(caps) = re.captures(normalized_output) {
        let passed = caps
            .name("passed")
            .map_or("0", |m| m.as_str())
            .parse::<usize>()
            .unwrap_or(0);
        let failed = caps
            .name("failed")
            .map_or("0", |m| m.as_str())
            .parse::<usize>()
            .unwrap_or(0);
        let skipped = caps
            .name("skipped")
            .map_or("0", |m| m.as_str())
            .parse::<usize>()
            .unwrap_or(0);
        let summary = format!(
            "pytest: {} passed{}{}",
            passed,
            if failed > 0 {
                format!(", {} failed", failed)
            } else {
                String::new()
            },
            if skipped > 0 {
                format!(", {} skipped", skipped)
            } else {
                String::new()
            }
        );
        return CommandObservation {
            summary: summary.clone(),
            fingerprint_source: normalized_output.to_string(),
            payload: json!({
                "event_type": "pytest",
                "command_sig": command_sig,
                "passed": passed,
                "failed": failed,
                "skipped": skipped,
                "summary": summary,
                "artifact_id": artifact.artifact_id,
            }),
        };
    }
    CommandObservation {
        summary: summarize_compact_output(normalized_output, 0),
        fingerprint_source: normalized_output.to_string(),
        payload: json!({
            "event_type": "pytest",
            "command_sig": command_sig,
            "summary": summarize_compact_output(normalized_output, 0),
            "artifact_id": artifact.artifact_id,
            "line_count": raw_filtered_output.lines().count(),
        }),
    }
}

fn mypy_observation(
    command_sig: &str,
    normalized_output: &str,
    raw_filtered_output: &str,
    artifact: &ArtifactRenderResult,
) -> CommandObservation {
    static MYPY_SUMMARY_RE: OnceLock<Regex> = OnceLock::new();
    static MYPY_FILE_RE: OnceLock<Regex> = OnceLock::new();
    static MYPY_DIAG_RE: OnceLock<Regex> = OnceLock::new();

    let first_line = first_non_empty_line(normalized_output);
    if first_line == "mypy: No issues found" {
        let summary = "mypy: no issues found".to_string();
        return semantic_observation(
            summary.clone(),
            json!({
                "clean": true,
            }),
            json!({
                "event_type": "mypy",
                "command_sig": command_sig,
                "clean": true,
                "summary": summary,
                "artifact_id": artifact.artifact_id,
            }),
        );
    }

    let lines = normalized_output.lines().collect::<Vec<_>>();
    let summary_re = MYPY_SUMMARY_RE.get_or_init(|| {
        Regex::new(r"^mypy:\s+(?P<errors>\d+)\s+errors?\s+in\s+(?P<files>\d+)\s+files?$")
            .expect("mypy summary regex")
    });
    let file_re = MYPY_FILE_RE.get_or_init(|| {
        Regex::new(r"^(?P<file>.+?)\s+\((?P<count>\d+)\s+errors?\)$").expect("mypy file regex")
    });
    let diag_re = MYPY_DIAG_RE.get_or_init(|| {
        Regex::new(r"^L(?P<line>\d+):(?:\s+\[(?P<code>[^\]]+)\])?\s+(?P<message>.+)$")
            .expect("mypy diag regex")
    });

    if let Some(summary_idx) = lines
        .iter()
        .position(|line| summary_re.is_match(line.trim()))
    {
        let summary_caps = summary_re
            .captures(lines[summary_idx].trim())
            .expect("mypy summary capture");
        let errors = summary_caps["errors"].parse::<usize>().unwrap_or(0);
        let files = summary_caps["files"].parse::<usize>().unwrap_or(0);

        let mut fileless_errors = lines[..summary_idx]
            .iter()
            .map(|line| line.trim())
            .filter(|line| !line.is_empty())
            .map(|line| line.to_string())
            .collect::<Vec<_>>();
        fileless_errors.sort();
        fileless_errors.dedup();

        let mut error_files = Vec::new();
        let mut current_path: Option<String> = None;
        let mut current_count = 0usize;
        let mut current_codes = Vec::new();

        for line in &lines[summary_idx + 1..] {
            let trimmed = line.trim();
            if trimmed.is_empty()
                || trimmed.starts_with("════════")
                || trimmed.starts_with("Top codes:")
            {
                continue;
            }
            if let Some(file_caps) = file_re.captures(trimmed) {
                if let Some(path) = current_path.take() {
                    current_codes.sort();
                    current_codes.dedup();
                    error_files.push(json!({
                        "path": path,
                        "errors": current_count,
                        "codes": current_codes,
                    }));
                    current_codes = Vec::new();
                }
                current_path = Some(file_caps["file"].to_string());
                current_count = file_caps["count"].parse::<usize>().unwrap_or(0);
                continue;
            }
            if let Some(diag_caps) = diag_re.captures(trimmed) {
                if let Some(code) = diag_caps.name("code") {
                    push_unique(&mut current_codes, code.as_str().to_string());
                }
            }
        }

        if let Some(path) = current_path.take() {
            current_codes.sort();
            current_codes.dedup();
            error_files.push(json!({
                "path": path,
                "errors": current_count,
                "codes": current_codes,
            }));
        }

        error_files.sort_by(|left, right| left["path"].as_str().cmp(&right["path"].as_str()));

        let summary = format!("mypy: {} errors in {} files", errors, files);
        return semantic_observation(
            summary.clone(),
            json!({
                "errors": errors,
                "files": files,
                "fileless_errors": fileless_errors,
                "error_files": error_files,
            }),
            json!({
                "event_type": "mypy",
                "command_sig": command_sig,
                "errors": errors,
                "files": files,
                "fileless_errors": fileless_errors,
                "error_files": error_files,
                "summary": summary,
                "artifact_id": artifact.artifact_id,
            }),
        );
    }

    CommandObservation {
        summary: summarize_compact_output(normalized_output, 0),
        fingerprint_source: normalized_output.to_string(),
        payload: json!({
            "event_type": "mypy",
            "command_sig": command_sig,
            "summary": summarize_compact_output(normalized_output, 0),
            "artifact_id": artifact.artifact_id,
            "line_count": raw_filtered_output.lines().count(),
        }),
    }
}

fn tsc_observation(
    command_sig: &str,
    normalized_output: &str,
    raw_filtered_output: &str,
    artifact: &ArtifactRenderResult,
) -> CommandObservation {
    static TSC_RE: OnceLock<Regex> = OnceLock::new();
    let re = TSC_RE.get_or_init(|| {
        Regex::new(r"TypeScript:\s+(?P<errors>\d+)\s+errors?\s+in\s+(?P<files>\d+)\s+files?")
            .expect("tsc regex")
    });
    if let Some(caps) = re.captures(normalized_output) {
        let errors = caps
            .name("errors")
            .map_or("0", |m| m.as_str())
            .parse::<usize>()
            .unwrap_or(0);
        let files = caps
            .name("files")
            .map_or("0", |m| m.as_str())
            .parse::<usize>()
            .unwrap_or(0);
        let summary = format!("tsc: {} errors in {} files", errors, files);
        return CommandObservation {
            summary: summary.clone(),
            fingerprint_source: normalized_output.to_string(),
            payload: json!({
                "event_type": "tsc",
                "command_sig": command_sig,
                "errors": errors,
                "files": files,
                "summary": summary,
                "artifact_id": artifact.artifact_id,
            }),
        };
    }
    CommandObservation {
        summary: summarize_compact_output(normalized_output, 0),
        fingerprint_source: normalized_output.to_string(),
        payload: json!({
            "event_type": "tsc",
            "command_sig": command_sig,
            "summary": summarize_compact_output(normalized_output, 0),
            "artifact_id": artifact.artifact_id,
            "line_count": raw_filtered_output.lines().count(),
        }),
    }
}

fn rspec_observation(
    command_sig: &str,
    normalized_output: &str,
    raw_filtered_output: &str,
    artifact: &ArtifactRenderResult,
) -> CommandObservation {
    static RSPEC_PASS_RE: OnceLock<Regex> = OnceLock::new();
    static RSPEC_FAIL_RE: OnceLock<Regex> = OnceLock::new();
    static RSPEC_TEXT_RE: OnceLock<Regex> = OnceLock::new();
    static RSPEC_OUTSIDE_RE: OnceLock<Regex> = OnceLock::new();
    static RSPEC_FAILURE_LINE_RE: OnceLock<Regex> = OnceLock::new();
    static RSPEC_LOCATION_RE: OnceLock<Regex> = OnceLock::new();

    let first_line = first_non_empty_line(normalized_output);
    if first_line == "RSpec: No examples found" {
        let summary = "rspec: no examples found".to_string();
        return semantic_observation(
            summary.clone(),
            json!({
                "no_examples": true,
            }),
            json!({
                "event_type": "rspec",
                "command_sig": command_sig,
                "no_examples": true,
                "summary": summary,
                "artifact_id": artifact.artifact_id,
            }),
        );
    }

    let pass_re = RSPEC_PASS_RE.get_or_init(|| {
        Regex::new(
            r"^(?:\S+\s+)?RSpec:\s+(?P<passed>\d+)\s+passed(?:,\s+(?P<pending>\d+)\s+pending)?(?:\s+\((?:<time>|[\d.]+s)\))?$",
        )
        .expect("rspec pass regex")
    });
    if let Some(caps) = pass_re.captures(first_line) {
        let passed = caps["passed"].parse::<usize>().unwrap_or(0);
        let pending = caps
            .name("pending")
            .map_or("0", |m| m.as_str())
            .parse::<usize>()
            .unwrap_or(0);
        let summary = format!(
            "rspec: {} passed{}",
            passed,
            if pending > 0 {
                format!(", {} pending", pending)
            } else {
                String::new()
            }
        );
        return semantic_observation(
            summary.clone(),
            json!({
                "passed": passed,
                "pending": pending,
                "failed": 0,
            }),
            json!({
                "event_type": "rspec",
                "command_sig": command_sig,
                "passed": passed,
                "failed": 0,
                "pending": pending,
                "summary": summary,
                "artifact_id": artifact.artifact_id,
            }),
        );
    }

    let outside_re = RSPEC_OUTSIDE_RE.get_or_init(|| {
        Regex::new(r"^RSpec:\s+(?P<outside>\d+)\s+errors outside of examples(?:\s+\((?:<time>|[\d.]+s)\))?$")
            .expect("rspec outside regex")
    });
    if let Some(caps) = outside_re.captures(first_line) {
        let outside = caps["outside"].parse::<usize>().unwrap_or(0);
        let summary = format!("rspec: {} errors outside examples", outside);
        return semantic_observation(
            summary.clone(),
            json!({
                "errors_outside_examples": outside,
            }),
            json!({
                "event_type": "rspec",
                "command_sig": command_sig,
                "errors_outside_examples": outside,
                "summary": summary,
                "artifact_id": artifact.artifact_id,
            }),
        );
    }

    let failure_line_re = RSPEC_FAILURE_LINE_RE
        .get_or_init(|| Regex::new(r"^\d+\.\s+\S+\s+(?P<title>.+)$").expect("rspec failure regex"));
    let location_re =
        RSPEC_LOCATION_RE.get_or_init(|| Regex::new(r".+\.rb:\d+").expect("rspec location regex"));
    let mut failure_examples = Vec::new();
    let mut failure_locations = Vec::new();
    for line in normalized_output.lines().map(str::trim) {
        if let Some(caps) = failure_line_re.captures(line) {
            push_unique(&mut failure_examples, caps["title"].to_string());
        }
        if location_re.is_match(line) {
            push_unique(&mut failure_locations, line.to_string());
        }
    }
    failure_examples.sort();
    failure_locations.sort();

    let fail_re = RSPEC_FAIL_RE.get_or_init(|| {
        Regex::new(
            r"^RSpec:\s+(?P<passed>\d+)\s+passed,\s+(?P<failed>\d+)\s+failed(?:,\s+(?P<pending>\d+)\s+pending)?(?:\s+\((?:<time>|[\d.]+s)\))?$",
        )
        .expect("rspec fail regex")
    });
    if let Some(caps) = fail_re.captures(first_line) {
        let passed = caps["passed"].parse::<usize>().unwrap_or(0);
        let failed = caps["failed"].parse::<usize>().unwrap_or(0);
        let pending = caps
            .name("pending")
            .map_or("0", |m| m.as_str())
            .parse::<usize>()
            .unwrap_or(0);
        let summary = format!(
            "rspec: {} passed, {} failed{}",
            passed,
            failed,
            if pending > 0 {
                format!(", {} pending", pending)
            } else {
                String::new()
            }
        );
        return semantic_observation(
            summary.clone(),
            json!({
                "passed": passed,
                "failed": failed,
                "pending": pending,
                "failure_examples": failure_examples,
                "failure_locations": failure_locations,
            }),
            json!({
                "event_type": "rspec",
                "command_sig": command_sig,
                "passed": passed,
                "failed": failed,
                "pending": pending,
                "failure_examples": failure_examples,
                "failure_locations": failure_locations,
                "summary": summary,
                "artifact_id": artifact.artifact_id,
            }),
        );
    }

    let text_re = RSPEC_TEXT_RE.get_or_init(|| {
        Regex::new(
            r"^RSpec:\s+(?P<examples>\d+)\s+examples?,\s+(?P<failed>\d+)\s+failures?(?:,\s+(?P<pending>\d+)\s+pending)?$",
        )
        .expect("rspec text regex")
    });
    if let Some(caps) = text_re.captures(first_line) {
        let examples = caps["examples"].parse::<usize>().unwrap_or(0);
        let failed = caps["failed"].parse::<usize>().unwrap_or(0);
        let pending = caps
            .name("pending")
            .map_or("0", |m| m.as_str())
            .parse::<usize>()
            .unwrap_or(0);
        let summary = format!(
            "rspec: {} examples, {} failed{}",
            examples,
            failed,
            if pending > 0 {
                format!(", {} pending", pending)
            } else {
                String::new()
            }
        );
        return semantic_observation(
            summary.clone(),
            json!({
                "examples": examples,
                "failed": failed,
                "pending": pending,
                "failure_examples": failure_examples,
                "failure_locations": failure_locations,
            }),
            json!({
                "event_type": "rspec",
                "command_sig": command_sig,
                "examples": examples,
                "failed": failed,
                "pending": pending,
                "failure_examples": failure_examples,
                "failure_locations": failure_locations,
                "summary": summary,
                "artifact_id": artifact.artifact_id,
            }),
        );
    }

    CommandObservation {
        summary: summarize_compact_output(normalized_output, 0),
        fingerprint_source: normalized_output.to_string(),
        payload: json!({
            "event_type": "rspec",
            "command_sig": command_sig,
            "summary": summarize_compact_output(normalized_output, 0),
            "artifact_id": artifact.artifact_id,
            "line_count": raw_filtered_output.lines().count(),
        }),
    }
}

fn ruff_observation(
    event_type: &str,
    command_sig: &str,
    normalized_output: &str,
    raw_filtered_output: &str,
    artifact: &ArtifactRenderResult,
) -> CommandObservation {
    static RUFF_RE: OnceLock<Regex> = OnceLock::new();
    let re = RUFF_RE.get_or_init(|| {
        Regex::new(r"Ruff:\s+(?P<issues>\d+)\s+issues?\s+in\s+(?P<files>\d+)\s+files?(?:\s+\((?P<fixable>\d+)\s+fixable\))?").expect("ruff regex")
    });
    if let Some(caps) = re.captures(normalized_output) {
        let issues = caps
            .name("issues")
            .map_or("0", |m| m.as_str())
            .parse::<usize>()
            .unwrap_or(0);
        let files = caps
            .name("files")
            .map_or("0", |m| m.as_str())
            .parse::<usize>()
            .unwrap_or(0);
        let fixable = caps
            .name("fixable")
            .map_or("0", |m| m.as_str())
            .parse::<usize>()
            .unwrap_or(0);
        let summary = format!(
            "{}: {} issues in {} files{}",
            event_type,
            issues,
            files,
            if fixable > 0 {
                format!(", {} fixable", fixable)
            } else {
                String::new()
            }
        );
        return CommandObservation {
            summary: summary.clone(),
            fingerprint_source: normalized_output.to_string(),
            payload: json!({
                "event_type": event_type,
                "command_sig": command_sig,
                "issues": issues,
                "files": files,
                "fixable": fixable,
                "summary": summary,
                "artifact_id": artifact.artifact_id,
            }),
        };
    }
    CommandObservation {
        summary: summarize_compact_output(normalized_output, 0),
        fingerprint_source: normalized_output.to_string(),
        payload: json!({
            "event_type": event_type,
            "command_sig": command_sig,
            "summary": summarize_compact_output(normalized_output, 0),
            "artifact_id": artifact.artifact_id,
            "line_count": raw_filtered_output.lines().count(),
        }),
    }
}

fn rubocop_observation(
    command_sig: &str,
    normalized_output: &str,
    raw_filtered_output: &str,
    artifact: &ArtifactRenderResult,
) -> CommandObservation {
    static RUBOCOP_CLEAN_RE: OnceLock<Regex> = OnceLock::new();
    static RUBOCOP_SUMMARY_RE: OnceLock<Regex> = OnceLock::new();
    static RUBOCOP_TEXT_RE: OnceLock<Regex> = OnceLock::new();
    static RUBOCOP_CORRECTABLE_RE: OnceLock<Regex> = OnceLock::new();
    static RUBOCOP_OFFENSE_RE: OnceLock<Regex> = OnceLock::new();

    let first_line = first_non_empty_line(normalized_output);

    let clean_re = RUBOCOP_CLEAN_RE.get_or_init(|| {
        Regex::new(
            r"^(?:ok\s+\S+\s+)?rubocop(?:\s+(?P<mode>-A))?\s+\((?P<files>\d+)\s+files(?:,\s+(?P<autocorrected>\d+)\s+autocorrected)?\)$",
        )
        .expect("rubocop clean regex")
    });
    if let Some(caps) = clean_re.captures(first_line) {
        let files = caps["files"].parse::<usize>().unwrap_or(0);
        let autocorrected = caps
            .name("autocorrected")
            .map_or("0", |m| m.as_str())
            .parse::<usize>()
            .unwrap_or(0);
        let autocorrect_mode = caps.name("mode").is_some() || autocorrected > 0;
        let summary = if autocorrected > 0 {
            format!(
                "rubocop: clean across {} files, {} autocorrected",
                files, autocorrected
            )
        } else {
            format!("rubocop: clean across {} files", files)
        };
        return semantic_observation(
            summary.clone(),
            json!({
                "clean": true,
                "files": files,
                "autocorrect_mode": autocorrect_mode,
                "autocorrected": autocorrected,
            }),
            json!({
                "event_type": "rubocop",
                "command_sig": command_sig,
                "clean": true,
                "files": files,
                "autocorrect_mode": autocorrect_mode,
                "autocorrected": autocorrected,
                "summary": summary,
                "artifact_id": artifact.artifact_id,
            }),
        );
    }

    if first_line.starts_with("RuboCop error:") {
        let error_lines = normalized_output
            .lines()
            .skip_while(|line| line.trim() != first_line)
            .skip(1)
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .take(3)
            .map(|line| line.to_string())
            .collect::<Vec<_>>();
        let summary = "rubocop: error".to_string();
        return semantic_observation(
            summary.clone(),
            json!({
                "error_lines": error_lines,
            }),
            json!({
                "event_type": "rubocop",
                "command_sig": command_sig,
                "error_lines": error_lines,
                "summary": summary,
                "artifact_id": artifact.artifact_id,
            }),
        );
    }

    let summary_re = RUBOCOP_SUMMARY_RE.get_or_init(|| {
        Regex::new(r"^rubocop:\s+(?P<offenses>\d+)\s+offenses?\s+\((?P<files>\d+)\s+files\)$")
            .expect("rubocop summary regex")
    });
    if let Some(caps) = summary_re.captures(first_line) {
        let offenses = caps["offenses"].parse::<usize>().unwrap_or(0);
        let files = caps["files"].parse::<usize>().unwrap_or(0);
        let correctable_re = RUBOCOP_CORRECTABLE_RE.get_or_init(|| {
            Regex::new(r"^\((?P<correctable>\d+)\s+correctable, run `rubocop -A`\)$")
                .expect("rubocop correctable regex")
        });
        let offense_re = RUBOCOP_OFFENSE_RE.get_or_init(|| {
            Regex::new(r"^:(?P<line>\d+)\s+(?P<cop>\S+)").expect("rubocop offense regex")
        });

        let mut offense_files = Vec::new();
        let mut current_path: Option<String> = None;
        let mut current_offense_count = 0usize;
        let mut current_cops = Vec::new();
        let mut correctable = 0usize;

        for line in normalized_output.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty()
                || trimmed == first_line
                || trimmed.starts_with("════════")
                || trimmed.starts_with("... +")
            {
                continue;
            }
            if let Some(correctable_caps) = correctable_re.captures(trimmed) {
                correctable = correctable_caps["correctable"]
                    .parse::<usize>()
                    .unwrap_or(0);
                continue;
            }
            if !line.starts_with(' ') && !trimmed.starts_with('(') {
                if let Some(path) = current_path.take() {
                    current_cops.sort();
                    current_cops.dedup();
                    offense_files.push(json!({
                        "path": path,
                        "offenses": current_offense_count,
                        "cops": current_cops,
                    }));
                    current_cops = Vec::new();
                    current_offense_count = 0;
                }
                current_path = Some(trimmed.to_string());
                continue;
            }
            if let Some(offense_caps) = offense_re.captures(trimmed) {
                current_offense_count += 1;
                push_unique(&mut current_cops, offense_caps["cop"].to_string());
            }
        }

        if let Some(path) = current_path.take() {
            current_cops.sort();
            current_cops.dedup();
            offense_files.push(json!({
                "path": path,
                "offenses": current_offense_count,
                "cops": current_cops,
            }));
        }

        offense_files.sort_by(|left, right| left["path"].as_str().cmp(&right["path"].as_str()));

        let summary = format!(
            "rubocop: {} offenses in {} files{}",
            offenses,
            files,
            if correctable > 0 {
                format!(", {} correctable", correctable)
            } else {
                String::new()
            }
        );
        return semantic_observation(
            summary.clone(),
            json!({
                "offenses": offenses,
                "files": files,
                "correctable": correctable,
                "offense_files": offense_files,
            }),
            json!({
                "event_type": "rubocop",
                "command_sig": command_sig,
                "offenses": offenses,
                "files": files,
                "correctable": correctable,
                "offense_files": offense_files,
                "summary": summary,
                "artifact_id": artifact.artifact_id,
            }),
        );
    }

    let text_re = RUBOCOP_TEXT_RE.get_or_init(|| {
        Regex::new(
            r"^RuboCop:\s+(?P<files>\d+)\s+files inspected,\s+(?:(?P<offenses>\d+)\s+offenses?\s+detected|no offenses detected)$",
        )
        .expect("rubocop text regex")
    });
    if let Some(caps) = text_re.captures(first_line) {
        let files = caps["files"].parse::<usize>().unwrap_or(0);
        let offenses = caps
            .name("offenses")
            .map_or("0", |m| m.as_str())
            .parse::<usize>()
            .unwrap_or(0);
        let summary = if offenses == 0 {
            format!("rubocop: clean across {} files", files)
        } else {
            format!(
                "rubocop: {} offense{} in {} files",
                offenses,
                if offenses == 1 { "" } else { "s" },
                files
            )
        };
        return semantic_observation(
            summary.clone(),
            json!({
                "files": files,
                "offenses": offenses,
            }),
            json!({
                "event_type": "rubocop",
                "command_sig": command_sig,
                "files": files,
                "offenses": offenses,
                "summary": summary,
                "artifact_id": artifact.artifact_id,
            }),
        );
    }

    CommandObservation {
        summary: summarize_compact_output(normalized_output, 0),
        fingerprint_source: normalized_output.to_string(),
        payload: json!({
            "event_type": "rubocop",
            "command_sig": command_sig,
            "summary": summarize_compact_output(normalized_output, 0),
            "artifact_id": artifact.artifact_id,
            "line_count": raw_filtered_output.lines().count(),
        }),
    }
}

fn next_build_observation(
    command_sig: &str,
    normalized_output: &str,
    raw_filtered_output: &str,
    artifact: &ArtifactRenderResult,
) -> CommandObservation {
    static NEXT_ROUTE_RE: OnceLock<Regex> = OnceLock::new();
    static NEXT_BUNDLE_RE: OnceLock<Regex> = OnceLock::new();
    static NEXT_FOOTER_RE: OnceLock<Regex> = OnceLock::new();

    let route_re = NEXT_ROUTE_RE.get_or_init(|| {
        Regex::new(r"^(?P<total>\d+)\s+routes\s+\((?P<static>\d+)\s+static,\s+(?P<dynamic>\d+)\s+dynamic\)$")
            .expect("next route regex")
    });
    let bundle_re = NEXT_BUNDLE_RE.get_or_init(|| {
        Regex::new(r"^(?P<route>.+?)\s+(?P<size>\d+)\s+kB(?:\s+\[warn\]\s+\(\+(?P<pct>\d+)%\))?$")
            .expect("next bundle regex")
    });
    let footer_re = NEXT_FOOTER_RE.get_or_init(|| {
        Regex::new(
            r"^Time:\s+(?:<time>|<ms>|[\d.]+(?:s|ms))\s+\|\s+Errors:\s+(?P<errors>\d+)\s+\|\s+Warnings:\s+(?P<warnings>\d+)$",
        )
        .expect("next footer regex")
    });

    let mut routes_total = 0usize;
    let mut routes_static = 0usize;
    let mut routes_dynamic = 0usize;
    let mut errors = 0usize;
    let mut warnings = 0usize;
    let mut cached = normalized_output.contains("Already built (using cache)");
    let mut in_bundles = false;
    let mut bundles = Vec::new();

    for line in normalized_output.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed == "Next.js Build" || trimmed.starts_with("════════")
        {
            continue;
        }
        if trimmed == "Bundles:" {
            in_bundles = true;
            continue;
        }
        if let Some(caps) = route_re.captures(trimmed) {
            routes_total = caps["total"].parse::<usize>().unwrap_or(0);
            routes_static = caps["static"].parse::<usize>().unwrap_or(0);
            routes_dynamic = caps["dynamic"].parse::<usize>().unwrap_or(0);
            continue;
        }
        if let Some(caps) = footer_re.captures(trimmed) {
            errors = caps["errors"].parse::<usize>().unwrap_or(0);
            warnings = caps["warnings"].parse::<usize>().unwrap_or(0);
            in_bundles = false;
            continue;
        }
        if trimmed.starts_with("Already built") {
            cached = true;
            continue;
        }
        if in_bundles {
            if let Some(caps) = bundle_re.captures(trimmed) {
                bundles.push(json!({
                    "route": caps["route"].trim(),
                    "size_kb": caps["size"].parse::<usize>().unwrap_or(0),
                    "warn_pct": caps
                        .name("pct")
                        .map_or(0usize, |m| m.as_str().parse::<usize>().unwrap_or(0)),
                }));
            }
        }
    }

    if cached || routes_total > 0 || !bundles.is_empty() || errors > 0 || warnings > 0 {
        bundles.sort_by(|left, right| right["size_kb"].as_u64().cmp(&left["size_kb"].as_u64()));
        let largest_bundle = bundles
            .first()
            .and_then(|bundle| {
                Some(format!(
                    "{} {} kB",
                    bundle["route"].as_str()?,
                    bundle["size_kb"].as_u64()?
                ))
            })
            .unwrap_or_default();
        let summary = if cached && routes_total == 0 {
            format!(
                "next build: cache hit, {} errors, {} warnings",
                errors, warnings
            )
        } else {
            format!(
                "next build: {} routes ({} static, {} dynamic){}; {} errors, {} warnings",
                routes_total,
                routes_static,
                routes_dynamic,
                if largest_bundle.is_empty() {
                    String::new()
                } else {
                    format!(", largest {}", largest_bundle)
                },
                errors,
                warnings
            )
        };

        return semantic_observation(
            summary.clone(),
            json!({
                "cached": cached,
                "routes_total": routes_total,
                "routes_static": routes_static,
                "routes_dynamic": routes_dynamic,
                "errors": errors,
                "warnings": warnings,
                "bundles": bundles,
            }),
            json!({
                "event_type": "next-build",
                "command_sig": command_sig,
                "cached": cached,
                "routes_total": routes_total,
                "routes_static": routes_static,
                "routes_dynamic": routes_dynamic,
                "errors": errors,
                "warnings": warnings,
                "bundles": bundles,
                "summary": summary,
                "artifact_id": artifact.artifact_id,
            }),
        );
    }

    CommandObservation {
        summary: summarize_compact_output(normalized_output, 0),
        fingerprint_source: normalized_output.to_string(),
        payload: json!({
            "event_type": "next-build",
            "command_sig": command_sig,
            "summary": summarize_compact_output(normalized_output, 0),
            "artifact_id": artifact.artifact_id,
            "line_count": raw_filtered_output.lines().count(),
        }),
    }
}

fn normalize_transient_output(output: &str) -> String {
    static DURATION_RE: OnceLock<Regex> = OnceLock::new();
    static MILLIS_RE: OnceLock<Regex> = OnceLock::new();
    static MINSEC_RE: OnceLock<Regex> = OnceLock::new();

    let mut normalized = output.to_string();
    normalized = DURATION_RE
        .get_or_init(|| Regex::new(r"\d+(?:\.\d+)?s").expect("duration regex"))
        .replace_all(&normalized, "<time>")
        .into_owned();
    normalized = MILLIS_RE
        .get_or_init(|| Regex::new(r"\d+(?:\.\d+)?ms").expect("millis regex"))
        .replace_all(&normalized, "<ms>")
        .into_owned();
    normalized = MINSEC_RE
        .get_or_init(|| Regex::new(r"\d+m\d+(?:\.\d+)?s").expect("minsec regex"))
        .replace_all(&normalized, "<duration>")
        .into_owned();
    normalized
}

fn render_prompt(
    goal: Option<&str>,
    project_path: &str,
    current_state: &[ContextFact],
    live_claims: &[ContextClaim],
    open_obligations: &[ContextClaim],
    recent_changes: &[ContextFact],
    recent_commands: &[String],
    artifact_handles: &[ArtifactHandle],
) -> String {
    let mut lines = Vec::new();
    lines.push(format!(
        "<task_goal>{}</task_goal>",
        goal.unwrap_or("Continue the current task using the deterministic worldview below.")
    ));
    lines.push(format!("<project_path>{}</project_path>", project_path));
    lines.push("<deterministic_worldview>".to_string());
    if current_state.is_empty() {
        lines.push("- No worldview facts recorded yet.".to_string());
    } else {
        for fact in current_state {
            let artifact = fact
                .artifact_id
                .as_ref()
                .map(|id| format!(" | artifact {}", id))
                .unwrap_or_default();
            lines.push(format!(
                "- [{}:{}] {}{}",
                fact.event_type, fact.status, fact.summary, artifact
            ));
        }
    }
    lines.push("</deterministic_worldview>".to_string());
    lines.push("<validated_claim_leases>".to_string());
    if live_claims.is_empty() {
        lines.push("- No live validated claims.".to_string());
    } else {
        for claim in live_claims {
            let deps = if claim.dependencies.is_empty() {
                String::new()
            } else {
                format!(" | deps {}", claim.dependencies.join(", "))
            };
            lines.push(format!(
                "- [{}:{}] {}{}",
                claim.claim_type, claim.confidence, claim.claim, deps
            ));
        }
    }
    lines.push("</validated_claim_leases>".to_string());
    lines.push("<open_obligations>".to_string());
    if open_obligations.is_empty() {
        lines.push("- No open obligations.".to_string());
    } else {
        for claim in open_obligations {
            let deps = if claim.dependencies.is_empty() {
                String::new()
            } else {
                format!(" | deps {}", claim.dependencies.join(", "))
            };
            lines.push(format!("- [{}] {}{}", claim.confidence, claim.claim, deps));
        }
    }
    lines.push("</open_obligations>".to_string());
    lines.push("<recent_deltas>".to_string());
    if recent_changes.is_empty() {
        lines.push("- No recent changes captured.".to_string());
    } else {
        for fact in recent_changes {
            lines.push(format!("- [{}] {}", fact.status, fact.summary));
        }
    }
    lines.push("</recent_deltas>".to_string());
    lines.push("<recent_commands>".to_string());
    if recent_commands.is_empty() {
        lines.push("- No recent Context command history for this project.".to_string());
    } else {
        for command in recent_commands {
            lines.push(format!("- {}", command));
        }
    }
    lines.push("</recent_commands>".to_string());
    lines.push("<artifact_handles>".to_string());
    if artifact_handles.is_empty() {
        lines.push("- No artifacts available.".to_string());
    } else {
        for artifact in artifact_handles {
            lines.push(format!(
                "- {} => {}",
                artifact.artifact_id, artifact.reopen_hint
            ));
        }
    }
    lines.push("</artifact_handles>".to_string());
    lines.push("<compiler_rules>".to_string());
    lines.push("- Treat deterministic worldview facts as authoritative unless a reopened artifact contradicts them.".to_string());
    lines.push(
        "- Treat live validated claims as reusable only while their dependencies remain unchanged."
            .to_string(),
    );
    lines.push("- Request only the minimum extra context needed for the next step.".to_string());
    lines.push("- Prefer artifact handles over replaying large raw outputs.".to_string());
    lines.push("</compiler_rules>".to_string());
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::tracking::{
        ClaimLeaseConfidence, ClaimLeaseDependency, ClaimLeaseDependencyKind, ClaimLeaseType,
        Tracker,
    };
    use tempfile::TempDir;

    fn tracker_and_project() -> (TempDir, Tracker, String) {
        let tmp = TempDir::new().expect("temp dir");
        let db_path = tmp.path().join("tracking.db");
        let tracker = Tracker::new_at_path(&db_path).expect("tracker");
        let project = tmp.path().join("project");
        std::fs::create_dir_all(&project).expect("project dir");
        (tmp, tracker, project.to_string_lossy().to_string())
    }

    fn test_artifact() -> ArtifactRenderResult {
        ArtifactRenderResult {
            rendered: String::new(),
            artifact_id: None,
            event_kind: None,
        }
    }

    #[test]
    fn compile_context_prefers_latest_state_per_subject() {
        let (_tmp, tracker, project_path) = tracker_and_project();
        tracker
            .record_worldview_event_for_project(
                &project_path,
                "read",
                &format!("file:{}/demo.rs", project_path),
                "context read demo.rs",
                "demo.rs | 10 lines",
                "hash-a",
                Some("@context/a_old"),
                &format!(
                    r#"{{"path":"{}/demo.rs"}}"#,
                    project_path.replace('\\', "\\\\")
                ),
            )
            .expect("first");
        tracker
            .record_worldview_event_for_project(
                &project_path,
                "read",
                &format!("file:{}/demo.rs", project_path),
                "context read demo.rs",
                "demo.rs | 12 lines",
                "hash-b",
                Some("@context/a_new"),
                &format!(
                    r#"{{"path":"{}/demo.rs"}}"#,
                    project_path.replace('\\', "\\\\")
                ),
            )
            .expect("second");
        tracker
            .record_worldview_event_for_project(
                &project_path,
                "grep",
                "grep:src:Tracker::new",
                "context grep Tracker::new",
                "grep summary",
                "hash-c",
                None,
                r#"{"path":"src/core/worldview.rs"}"#,
            )
            .expect("third");
        tracker
            .create_claim_lease_for_project(
                &project_path,
                ClaimLeaseType::Rejection,
                "Do not split worldview into a second subsystem.",
                Some("The current tracking DB and prompt compiler already cover the state path."),
                ClaimLeaseConfidence::High,
                Some("architecture"),
                &[ClaimLeaseDependency {
                    kind: ClaimLeaseDependencyKind::WorldviewSubject,
                    key: format!("file:{}/demo.rs", project_path),
                    fingerprint: None,
                }],
                r#"["@context/a_new"]"#,
                "test",
            )
            .expect("claim");

        let compiled = compile_context_with_tracker(&tracker, &project_path, Some("Fix it"), 8, 4)
            .expect("compiled");

        assert_eq!(compiled.current_state.len(), 2);
        assert_eq!(compiled.live_claims.len(), 1);
        assert!(compiled
            .current_state
            .iter()
            .any(|fact| fact.summary.contains("12 lines")));
        assert!(compiled
            .live_claims
            .iter()
            .any(|claim| claim.claim.contains("Do not split worldview")));
        assert!(compiled
            .recent_changes
            .iter()
            .any(|fact| fact.status == "changed"));
        assert!(compiled.prompt.contains("<deterministic_worldview>"));
        assert!(compiled.prompt.contains("<validated_claim_leases>"));
    }

    #[test]
    fn compile_context_prioritizes_obligations_on_failure_facts() {
        let (_tmp, tracker, project_path) = tracker_and_project();
        let cargo_subject = format!("cargo-test:{project_path}");
        tracker
            .record_worldview_event_for_project(
                &project_path,
                "cargo-test",
                &cargo_subject,
                "context cargo test",
                "cargo test: 9 passed, 1 failed (1 suite, <time>)",
                "hash-failure",
                None,
                "{}",
            )
            .expect("failure worldview");
        tracker
            .create_claim_lease_for_project(
                &project_path,
                ClaimLeaseType::Obligation,
                "Fix the remaining failing cargo test before relying on the suite again.",
                Some("The latest cargo-test worldview fact is red."),
                ClaimLeaseConfidence::High,
                Some("verification"),
                &[ClaimLeaseDependency {
                    kind: ClaimLeaseDependencyKind::WorldviewSubject,
                    key: cargo_subject,
                    fingerprint: None,
                }],
                r#"["cargo test"]"#,
                "test",
            )
            .expect("obligation");

        let compiled = compile_context_with_tracker(&tracker, &project_path, Some("Fix it"), 8, 4)
            .expect("compiled");

        assert_eq!(compiled.open_obligations.len(), 1);
        assert_eq!(compiled.auto_obligation_count, 0);
        assert!(compiled.open_obligations[0]
            .claim
            .contains("Fix the remaining failing cargo test"));
        assert!(compiled.prompt.contains("<open_obligations>"));
    }

    #[test]
    fn compile_context_synthesizes_obligation_for_active_failure_without_claim_lease() {
        let (_tmp, tracker, project_path) = tracker_and_project();
        let cargo_subject = format!("cargo-test:{project_path}");
        tracker
            .record_worldview_event_for_project(
                &project_path,
                "cargo-test",
                &cargo_subject,
                "context cargo test",
                "cargo test: 2 failed (1 suite, <time>)",
                "hash-failure",
                Some("@context/a_fail"),
                r#"{"summary":"cargo test: 2 failed (1 suite, <time>)","failed":2,"failed_tests":["auth::login","auth::refresh"]}"#,
            )
            .expect("failure worldview");

        let compiled = compile_context_with_tracker(&tracker, &project_path, Some("Fix it"), 8, 4)
            .expect("compiled");

        assert_eq!(compiled.auto_obligation_count, 1);
        assert_eq!(compiled.open_obligations.len(), 1);
        assert!(compiled.open_obligations[0]
            .claim
            .contains("Resolve active failure: cargo test: 2 failed"));
        assert!(compiled.open_obligations[0]
            .dependencies
            .iter()
            .any(|dependency| dependency == &format!("worldview:{}", cargo_subject)));
        assert!(compiled.open_obligations[0]
            .dependencies
            .iter()
            .any(|dependency| dependency == "artifact:@context/a_fail"));
    }

    #[test]
    fn compile_context_recent_changes_suppresses_resolved_intermediate_failures() {
        let (_tmp, tracker, project_path) = tracker_and_project();
        let cargo_subject = format!("cargo-test:{project_path}");

        tracker
            .record_worldview_event_for_project(
                &project_path,
                "cargo-test",
                &cargo_subject,
                "context cargo test",
                "cargo test: 1 failed (1 suite, <time>)",
                "hash-fail",
                None,
                r#"{"summary":"cargo test: 1 failed (1 suite, <time>)","failed":1}"#,
            )
            .expect("failure");
        tracker
            .record_worldview_event_for_project(
                &project_path,
                "cargo-test",
                &cargo_subject,
                "context cargo test",
                "cargo test: 10 passed (1 suite, <time>)",
                "hash-pass",
                None,
                r#"{"summary":"cargo test: 10 passed (1 suite, <time>)","failed":0,"passed":10}"#,
            )
            .expect("success");
        tracker
            .record_worldview_event_for_project(
                &project_path,
                "cargo-test",
                &cargo_subject,
                "context cargo test",
                "cargo test: 10 passed (1 suite, <time>)",
                "hash-pass",
                None,
                r#"{"summary":"cargo test: 10 passed (1 suite, <time>)","failed":0,"passed":10}"#,
            )
            .expect("repeat success");

        let compiled =
            compile_context_with_tracker(&tracker, &project_path, Some("Continue"), 8, 8)
                .expect("compiled");

        assert_eq!(compiled.recent_changes.len(), 1);
        assert!(compiled.recent_changes[0].summary.contains("10 passed"));
        assert!(!compiled.recent_changes[0].summary.contains("1 failed"));
    }

    #[test]
    fn cargo_test_observation_parses_counts() {
        let artifact = test_artifact();
        let observation = build_command_observation(
            "cargo-test",
            "context cargo test foo",
            "cargo test: 14 passed, 1 failed, 2 ignored, 3 filtered out (2 suites, <time>)",
            "cargo test: 14 passed, 1 failed, 2 ignored, 3 filtered out (2 suites, 0.12s)",
            101,
            &artifact,
        );

        assert!(observation
            .summary
            .contains("14 passed, 1 failed, 2 ignored, 3 filtered out"));
        assert_eq!(observation.payload["passed"], 14);
        assert_eq!(observation.payload["failed"], 1);
        assert_eq!(observation.payload["ignored"], 2);
        assert_eq!(observation.payload["filtered_out"], 3);
        assert_eq!(observation.payload["suites"], 2);
    }

    #[test]
    fn pytest_observation_parses_counts() {
        let artifact = test_artifact();
        let observation = build_command_observation(
            "pytest",
            "context pytest",
            "Pytest: 4 passed, 1 failed, 2 skipped",
            "Pytest: 4 passed, 1 failed, 2 skipped",
            1,
            &artifact,
        );

        assert_eq!(observation.payload["passed"], 4);
        assert_eq!(observation.payload["failed"], 1);
        assert_eq!(observation.payload["skipped"], 2);
    }

    #[test]
    fn tsc_observation_parses_counts() {
        let artifact = test_artifact();
        let observation = build_command_observation(
            "tsc",
            "context tsc",
            "TypeScript: 7 errors in 3 files",
            "TypeScript: 7 errors in 3 files",
            2,
            &artifact,
        );

        assert_eq!(observation.payload["errors"], 7);
        assert_eq!(observation.payload["files"], 3);
        assert_eq!(observation.summary, "tsc: 7 errors in 3 files");
    }

    #[test]
    fn ruff_observation_parses_fixable_counts() {
        let artifact = test_artifact();
        let observation = build_command_observation(
            "ruff",
            "context ruff",
            "Ruff: 12 issues in 5 files (8 fixable)",
            "Ruff: 12 issues in 5 files (8 fixable)",
            1,
            &artifact,
        );

        assert_eq!(observation.payload["issues"], 12);
        assert_eq!(observation.payload["files"], 5);
        assert_eq!(observation.payload["fixable"], 8);
    }

    #[test]
    fn go_test_observation_parses_failure_entities() {
        let artifact = test_artifact();
        let observation = build_command_observation(
            "go-test",
            "context go test ./...",
            "Go test: 2 passed, 2 failed, 1 skipped in 2 packages\n═══════════════════════════════════════\n\nfoo (2 passed, 1 failed)\n  [FAIL] TestWidget\nbar [build failed]",
            "Go test: 2 passed, 2 failed, 1 skipped in 2 packages",
            1,
            &artifact,
        );

        assert_eq!(
            observation.summary,
            "go test: 2 passed, 2 failed, 1 skipped across 2 packages"
        );
        assert_eq!(observation.payload["passed"], 2);
        assert_eq!(observation.payload["failed"], 2);
        assert_eq!(observation.payload["packages"], 2);
        assert!(observation.payload["failed_tests"]
            .as_array()
            .expect("failed tests")
            .iter()
            .any(|value| value == "foo::TestWidget"));
        assert!(observation.payload["build_failed_packages"]
            .as_array()
            .expect("build failed packages")
            .iter()
            .any(|value| value == "bar"));
    }

    #[test]
    fn mypy_observation_parses_file_groups() {
        let artifact = test_artifact();
        let observation = build_command_observation(
            "mypy",
            "context mypy src",
            "mypy: error: No module named 'missing'\n\nmypy: 3 errors in 2 files\n═══════════════════════════════════════\nTop codes: return-value (2x)\n\nsrc/api.py (2 errors)\n  L10: [return-value] Incompatible return value type\nsrc/models.py (1 errors)\n  L5: [name-defined] Name \"foo\" is not defined",
            "mypy raw",
            1,
            &artifact,
        );

        assert_eq!(observation.payload["errors"], 3);
        assert_eq!(observation.payload["files"], 2);
        assert!(observation.payload["fileless_errors"]
            .as_array()
            .expect("fileless")
            .iter()
            .any(|value| value == "mypy: error: No module named 'missing'"));
        assert!(observation.payload["error_files"]
            .as_array()
            .expect("error files")
            .iter()
            .any(|value| value["path"] == "src/api.py"));
    }

    #[test]
    fn rspec_observation_parses_failure_examples() {
        let artifact = test_artifact();
        let observation = build_command_observation(
            "rspec",
            "context rspec spec/models/user_spec.rb",
            "RSpec: 1 passed, 1 failed (<time>)\n═══════════════════════════════════════\n\nFailures:\n1. ❌ User saves to database\n   ./spec/models/user_spec.rb:10\n   ExpectationNotMetError: expected true but got false",
            "RSpec raw",
            1,
            &artifact,
        );

        assert_eq!(observation.summary, "rspec: 1 passed, 1 failed");
        assert_eq!(observation.payload["passed"], 1);
        assert_eq!(observation.payload["failed"], 1);
        assert!(observation.payload["failure_examples"]
            .as_array()
            .expect("failure examples")
            .iter()
            .any(|value| value == "User saves to database"));
        assert!(observation.payload["failure_locations"]
            .as_array()
            .expect("failure locations")
            .iter()
            .any(|value| value == "./spec/models/user_spec.rb:10"));
    }

    #[test]
    fn rubocop_observation_parses_offense_files() {
        let artifact = test_artifact();
        let observation = build_command_observation(
            "rubocop",
            "context rubocop",
            "rubocop: 3 offenses (2 files)\n\napp/controllers/users_controller.rb\n  :30 Lint/Syntax — Syntax error\napp/models/user.rb\n  :10 Layout/TrailingWhitespace — Trailing whitespace\n  :25 Lint/UselessAssignment — Useless assignment\n\n(2 correctable, run `rubocop -A`)",
            "rubocop raw",
            1,
            &artifact,
        );

        assert_eq!(
            observation.summary,
            "rubocop: 3 offenses in 2 files, 2 correctable"
        );
        assert_eq!(observation.payload["offenses"], 3);
        assert_eq!(observation.payload["files"], 2);
        assert_eq!(observation.payload["correctable"], 2);
        assert!(observation.payload["offense_files"]
            .as_array()
            .expect("offense files")
            .iter()
            .any(|value| value["path"] == "app/models/user.rb"));
    }

    #[test]
    fn next_build_observation_parses_routes_and_bundles() {
        let artifact = test_artifact();
        let observation = build_command_observation(
            "next-build",
            "context next build",
            "Next.js Build\n═══════════════════════════════════════\n3 routes (2 static, 1 dynamic)\n\nBundles:\n  /dashboard                      156 kB [warn] (+12%)\n  /                               132 kB\n\nTime: <time> | Errors: 0 | Warnings: 1",
            "next raw",
            0,
            &artifact,
        );

        assert_eq!(observation.payload["routes_total"], 3);
        assert_eq!(observation.payload["routes_static"], 2);
        assert_eq!(observation.payload["routes_dynamic"], 1);
        assert_eq!(observation.payload["warnings"], 1);
        assert!(observation.summary.contains("largest /dashboard 156 kB"));
        assert_eq!(
            observation.payload["bundles"]
                .as_array()
                .expect("bundles")
                .len(),
            2
        );
    }
}
