//! Automatic first-run session backfill into Memory OS.

use crate::analytics::session_impact_cmd::{load_sessions, CommandOutcome, SessionRecord};
use crate::core::memory_os::{
    MemoryOsAction, MemoryOsActionCue, MemoryOsCheckpointCapture, MemoryOsCheckpointReentry,
    MemoryOsCheckpointTelemetry, MemoryOsPacketSelection,
};
use crate::core::tracking::Tracker;
use crate::core::worldview::replay_command_observation;
use crate::rewrite_engine::detector::{
    extract_base_command, find_correction_occurrences, CommandExecution, CorrectionOccurrence,
};
use anyhow::{Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;

const ONBOARDING_SCHEMA_VERSION: &str = "memory-os-session-onboarding-v10";
const ONBOARDING_STATE_FILE: &str = "memory_os_session_onboarding.json";
const INCREMENTAL_CHECK_INTERVAL_MINUTES: i64 = 15;
const MAX_SEMANTIC_ITEMS_PER_SESSION: usize = 8;

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct SessionBackfillState {
    schema_version: String,
    started_at: Option<String>,
    completed_at: Option<String>,
    last_checked_at: Option<String>,
    processed_session_ids: Vec<String>,
    sessions_processed: usize,
    shells_ingested: usize,
    corrections_ingested: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionBackfillReport {
    pub sessions_processed: usize,
    pub shells_ingested: usize,
    pub corrections_ingested: usize,
    pub completed_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionBackfillStatus {
    pub schema_version: String,
    pub status: String,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub sessions_processed: usize,
    pub shells_ingested: usize,
    pub corrections_ingested: usize,
    pub imported_source_counts: Vec<(String, usize)>,
}

pub fn get_memory_os_session_backfill_status() -> Result<SessionBackfillStatus> {
    let state = load_state()?;
    let mut source_counts = HashSet::new();
    let mut ordered_counts = Vec::new();
    for processed in &state.processed_session_ids {
        let source = processed
            .split_once(':')
            .map(|(source, _)| source)
            .unwrap_or("unknown")
            .to_string();
        if source_counts.insert(source.clone()) {
            ordered_counts.push(source);
        }
    }

    let imported_source_counts = ordered_counts
        .into_iter()
        .map(|source| {
            let count = state
                .processed_session_ids
                .iter()
                .filter(|processed| processed.starts_with(&(source.clone() + ":")))
                .count();
            (source, count)
        })
        .collect::<Vec<_>>();

    let status = if state.completed_at.is_some() {
        "completed"
    } else if state.started_at.is_some() {
        "in_progress"
    } else {
        "not_started"
    };

    Ok(SessionBackfillStatus {
        schema_version: state.schema_version,
        status: status.to_string(),
        started_at: state.started_at,
        completed_at: state.completed_at,
        sessions_processed: state.sessions_processed,
        shells_ingested: state.shells_ingested,
        corrections_ingested: state.corrections_ingested,
        imported_source_counts,
    })
}

pub fn ensure_memory_os_session_backfill() -> Result<Option<SessionBackfillReport>> {
    ensure_memory_os_session_backfill_with_force(false)
}

pub fn ensure_memory_os_session_backfill_with_force(
    force: bool,
) -> Result<Option<SessionBackfillReport>> {
    if cfg!(test)
        || std::env::var("CONTEXT_SKIP_MEMORY_OS_ONBOARDING")
            .ok()
            .as_deref()
            == Some("1")
    {
        return Ok(None);
    }

    let flags = crate::core::config::memory_os();
    if !flags.read_model_v1
        && !(flags.journal_v1 && flags.dual_write_v1 && flags.checkpoint_v1 && flags.action_v1)
    {
        return Ok(None);
    }

    let mut state = load_state()?;
    if force {
        state.completed_at = None;
        state.last_checked_at = None;
        state.processed_session_ids.clear();
        state.sessions_processed = 0;
        state.shells_ingested = 0;
        state.corrections_ingested = 0;
    }
    if !force && should_skip_incremental_backfill(&state) {
        return Ok(None);
    }

    if state.schema_version != ONBOARDING_SCHEMA_VERSION {
        state.schema_version = ONBOARDING_SCHEMA_VERSION.to_string();
        state.completed_at = None;
        state.last_checked_at = None;
        state.processed_session_ids.clear();
        state.sessions_processed = 0;
        state.shells_ingested = 0;
        state.corrections_ingested = 0;
    }
    if state.started_at.is_none() {
        state.started_at = Some(Utc::now().to_rfc3339());
    }

    let processed: HashSet<String> = state.processed_session_ids.iter().cloned().collect();
    let sessions = load_onboarding_sessions()?;
    let tracker =
        Tracker::new().context("Failed to initialize tracking database for session onboarding")?;
    let mut sessions_processed_this_run = 0usize;
    let mut shells_ingested_this_run = 0usize;
    let mut corrections_ingested_this_run = 0usize;

    for session in sessions {
        let processed_key = processed_session_key(&session);
        if processed.contains(&processed_key)
            || processed
                .iter()
                .any(|value| value.ends_with(&format!(":{}", session.session_id)))
        {
            continue;
        }

        replay_session(&tracker, &session)?;
        state.processed_session_ids.push(processed_key);
        sessions_processed_this_run += 1;
        shells_ingested_this_run += session.shells.len();
        corrections_ingested_this_run += session_correction_occurrences(&session).len();
        state.sessions_processed += 1;
        state.shells_ingested += session.shells.len();
        state.corrections_ingested += session_correction_occurrences(&session).len();
        save_state(&state)?;
    }

    let completed_at = Utc::now().to_rfc3339();
    state.completed_at = Some(completed_at.clone());
    state.last_checked_at = Some(completed_at.clone());
    save_state(&state)?;

    if sessions_processed_this_run == 0 {
        return Ok(None);
    }

    Ok(Some(SessionBackfillReport {
        sessions_processed: sessions_processed_this_run,
        shells_ingested: shells_ingested_this_run,
        corrections_ingested: corrections_ingested_this_run,
        completed_at,
    }))
}

fn should_skip_incremental_backfill(state: &SessionBackfillState) -> bool {
    if state.schema_version != ONBOARDING_SCHEMA_VERSION || state.completed_at.is_none() {
        return false;
    }
    if std::env::var("CONTEXT_MEMORY_OS_FORCE_ONBOARDING")
        .ok()
        .as_deref()
        == Some("1")
    {
        return false;
    }
    let Some(last_checked_at) = state.last_checked_at.as_deref() else {
        return false;
    };
    let Ok(last_checked_at) = chrono::DateTime::parse_from_rfc3339(last_checked_at) else {
        return false;
    };
    Utc::now() - last_checked_at.with_timezone(&Utc)
        < chrono::Duration::minutes(INCREMENTAL_CHECK_INTERVAL_MINUTES)
}

fn load_onboarding_sessions() -> Result<Vec<SessionRecord>> {
    let mut sessions = load_sessions(None, None, None, None)?;
    sessions.extend(load_recall_sessions()?);

    let mut by_id = std::collections::HashMap::new();
    for session in sessions {
        by_id
            .entry(session.session_id.clone())
            .and_modify(|existing: &mut SessionRecord| {
                let existing_weight = existing.shells.len() * 10
                    + existing.user_prompts.len()
                    + source_bias(existing.source);
                let new_weight = session.shells.len() * 10
                    + session.user_prompts.len()
                    + source_bias(session.source);
                if new_weight > existing_weight {
                    *existing = session.clone();
                }
            })
            .or_insert(session);
    }

    let mut merged = by_id.into_values().collect::<Vec<_>>();
    merged.sort_by(|left, right| {
        left.started_at
            .cmp(&right.started_at)
            .then_with(|| left.session_id.cmp(&right.session_id))
    });
    Ok(merged)
}

fn source_bias(source: crate::analytics::session_impact_cmd::SessionSource) -> usize {
    match source {
        crate::analytics::session_impact_cmd::SessionSource::Recall => 0,
        crate::analytics::session_impact_cmd::SessionSource::Codex => 2,
        crate::analytics::session_impact_cmd::SessionSource::Claude => 2,
    }
}

fn load_recall_sessions() -> Result<Vec<SessionRecord>> {
    let root = PathBuf::from("C:\\Users\\OEM\\Documents\\Obsidian Vault\\Sessions");
    if !root.exists() {
        return Ok(Vec::new());
    }

    let mut sessions = Vec::new();
    for entry in walkdir::WalkDir::new(&root)
        .follow_links(false)
        .into_iter()
        .filter_map(|entry| entry.ok())
    {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("md") {
            continue;
        }
        if let Some(session) = parse_recall_session(path)? {
            sessions.push(session);
        }
    }
    Ok(sessions)
}

fn replay_session(tracker: &Tracker, session: &SessionRecord) -> Result<()> {
    let project_path = if session.cwd.trim().is_empty() {
        format!(
            "session://{}/{}",
            session.source.as_str(),
            session.session_id
        )
    } else {
        session.cwd.clone()
    };

    for shell in &session.shells {
        let exit_code = match shell.outcome {
            CommandOutcome::Success => 0,
            CommandOutcome::Failure => 1,
            CommandOutcome::Unknown => 2,
        };
        let event_type = session_shell_event_type(&shell.command);
        let observation =
            replay_command_observation(event_type, &shell.command, &shell.output, exit_code)?;
        let subject_key = session_subject_key(event_type, &shell.command, &project_path);
        let mut payload: serde_json::Value = serde_json::from_str(&observation.payload_json)?;
        payload["replay_source"] = serde_json::json!({
            "session_source": session.source.as_str(),
            "session_id": session.session_id,
            "shell_timestamp": shell.timestamp.to_rfc3339(),
        });
        let payload_json = payload.to_string();
        tracker.record_worldview_replay_event_for_project(
            &project_path,
            event_type,
            &subject_key,
            &shell.command,
            &observation.summary,
            &hash_text(&observation.fingerprint_source),
            &payload_json,
        )?;
    }

    let corrections = session_correction_occurrences(session);
    for correction in &corrections {
        let observed_at =
            correction_observed_at(session, correction).unwrap_or(session.started_at.to_rfc3339());
        let cue = MemoryOsActionCue {
            cue_kind: "cli-correction".to_string(),
            packet_preset: None,
            intent: Some(correction_redirect_intent(correction)),
            override_type: Some(correction_override_type(correction)),
            correction_shape: Some("wrong-command-to-correct-command".to_string()),
            trigger_section: Some(
                correction
                    .pair
                    .error_type
                    .as_str()
                    .to_ascii_lowercase()
                    .replace(' ', "-"),
            ),
            trigger_subject: None,
            trigger_summary: Some(correction.pair.wrong_command.clone()),
        };
        let action = MemoryOsAction {
            action_kind: "run_command".to_string(),
            command_sig: Some(correction.pair.right_command.clone()),
            recommendation: Some(format!(
                "Use `{}` instead of `{}`",
                correction.pair.right_command, correction.pair.wrong_command
            )),
        };
        tracker.record_memory_os_action_observation_for_project(
            &project_path,
            "session-correction",
            &cue,
            &action,
            &format!(
                "{}:{}:{}",
                session.source.as_str(),
                session.session_id,
                correction.wrong_index
            ),
            &observed_at,
        )?;
        if let Some((observed_at, exit_code)) = correction_execution_details(session, correction) {
            tracker.record_memory_os_action_execution_at_for_project(
                &project_path,
                "session-replay",
                &correction.pair.right_command,
                None,
                exit_code,
                &observed_at,
            )?;
        }
    }

    let checkpoint = session_checkpoint_capture(session);
    tracker.record_memory_os_packet_checkpoint_for_project(&project_path, &checkpoint)?;

    Ok(())
}

fn correction_redirect_intent(correction: &CorrectionOccurrence) -> String {
    format!(
        "cli-correction:{}",
        correction
            .pair
            .error_type
            .as_str()
            .to_ascii_lowercase()
            .replace(' ', "-")
    )
}

fn correction_override_type(correction: &CorrectionOccurrence) -> String {
    let wrong_base = extract_base_command(&correction.pair.wrong_command);
    let right_base = extract_base_command(&correction.pair.right_command);
    if correction.pair.right_command.starts_with("context ")
        && !correction.pair.wrong_command.starts_with("context ")
    {
        "context-proxy-redirect".to_string()
    } else if wrong_base != right_base {
        "command-substitution".to_string()
    } else {
        "argument-or-path-correction".to_string()
    }
}

fn parse_recall_session(path: &std::path::Path) -> Result<Option<SessionRecord>> {
    let content = fs::read_to_string(path)?;
    let mut lines = content.lines();

    let mut date = None;
    let mut project = None;
    let mut session_id = None;

    if matches!(lines.next(), Some("---")) {
        for line in &mut lines {
            let trimmed = line.trim();
            if trimmed == "---" {
                break;
            }
            if let Some(value) = trimmed.strip_prefix("date:") {
                date = Some(value.trim().to_string());
            } else if let Some(value) = trimmed.strip_prefix("project:") {
                project = Some(value.trim().to_string());
            } else if let Some(value) = trimmed.strip_prefix("session:") {
                session_id = Some(value.trim().to_string());
            }
        }
    }

    let Some(session_id) = session_id else {
        return Ok(None);
    };

    let mut user_prompts = Vec::new();
    for line in lines {
        if let Some(text) = line.trim().strip_prefix("**You:**") {
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                user_prompts.push(crate::analytics::session_impact_cmd::UserPrompt {
                    timestamp: recall_session_timestamp(date.as_deref()),
                    text: trimmed.to_string(),
                });
            }
        }
    }

    let project_name = project.unwrap_or_else(|| "unknown".to_string());
    let cwd = resolve_recall_project_path(&project_name);

    Ok(Some(SessionRecord {
        source: crate::analytics::session_impact_cmd::SessionSource::Recall,
        session_id,
        cwd,
        started_at: recall_session_timestamp(date.as_deref()),
        user_prompts,
        shells: Vec::new(),
    }))
}

fn recall_session_timestamp(date: Option<&str>) -> chrono::DateTime<Utc> {
    date.and_then(|value| chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d").ok())
        .and_then(|date| date.and_hms_opt(0, 0, 0))
        .map(|naive| chrono::DateTime::<Utc>::from_naive_utc_and_offset(naive, Utc))
        .unwrap_or_else(Utc::now)
}

fn resolve_recall_project_path(project: &str) -> String {
    let candidate = PathBuf::from("C:\\Users\\OEM\\Projects").join(project);
    if candidate.exists() {
        candidate.to_string_lossy().to_string()
    } else {
        format!("recall://{}", project)
    }
}

fn session_correction_occurrences(session: &SessionRecord) -> Vec<CorrectionOccurrence> {
    let commands = session
        .shells
        .iter()
        .map(|shell| CommandExecution {
            command: shell.command.clone(),
            is_error: shell.outcome.is_failure(),
            output: shell.output.clone(),
        })
        .collect::<Vec<_>>();
    find_correction_occurrences(&commands)
}

fn session_checkpoint_capture(session: &SessionRecord) -> MemoryOsCheckpointCapture {
    let mut selected_items = Vec::new();
    for shell in session
        .shells
        .iter()
        .rev()
        .filter(|shell| shell.outcome.is_failure())
        .take(3)
    {
        selected_items.push(MemoryOsPacketSelection {
            section: "current_failures".to_string(),
            kind: "failure".to_string(),
            summary: summarize_shell_for_checkpoint(shell),
            token_estimate: shell.output.split_whitespace().count().min(64),
            score: 90,
            artifact_id: None,
            subject: Some(format!("command:{}", shell.command)),
            provenance: vec![format!("session:{}", session.source.as_str())],
        });
    }

    for correction in session_correction_occurrences(session).iter().take(3) {
        selected_items.push(MemoryOsPacketSelection {
            section: "open_obligations".to_string(),
            kind: "action-memory".to_string(),
            summary: format!(
                "Prefer `{}` after `{}`",
                correction.pair.right_command, correction.pair.wrong_command
            ),
            token_estimate: 24,
            score: 80,
            artifact_id: None,
            subject: None,
            provenance: vec![format!("session:{}", session.source.as_str())],
        });
    }

    let last_prompt = session
        .user_prompts
        .last()
        .map(|prompt| prompt.text.clone());
    if let Some(prompt) = last_prompt
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        selected_items.push(MemoryOsPacketSelection {
            section: "user_prompts".to_string(),
            kind: "user-prompt".to_string(),
            summary: prompt.to_string(),
            token_estimate: prompt.split_whitespace().count().min(80),
            score: 100,
            artifact_id: None,
            subject: Some(format!(
                "prompt:{}:{}",
                session.source.as_str(),
                session.session_id
            )),
            provenance: vec![format!("session:{}", session.source.as_str())],
        });
    }

    selected_items.extend(session_semantic_items(session));

    let last_successful_command = session
        .shells
        .iter()
        .rev()
        .find(|shell| shell.outcome.is_success())
        .map(|shell| shell.command.clone());
    let recommended_command = session_correction_occurrences(session)
        .last()
        .map(|correction| correction.pair.right_command.clone())
        .or(last_successful_command)
        .unwrap_or_else(|| "context context".to_string());

    let captured_at = session
        .shells
        .last()
        .map(|shell| shell.timestamp)
        .unwrap_or(session.started_at)
        .to_rfc3339();

    MemoryOsCheckpointCapture {
        packet_id: format!(
            "onboarding-{}-{}-{}",
            ONBOARDING_SCHEMA_VERSION,
            session.source.as_str(),
            session.session_id
        ),
        generated_at: captured_at,
        preset: "resume".to_string(),
        intent: "diagnose".to_string(),
        profile: "session-onboarding".to_string(),
        goal: last_prompt.clone(),
        budget: 1600,
        estimated_tokens: 0,
        estimated_source_tokens: 0,
        pager_manifest_hash: hash_text(&format!(
            "{}:{}",
            session.source.as_str(),
            session.session_id
        )),
        recall_mode: "off".to_string(),
        recall_used: false,
        recall_reason: "session-onboarding".to_string(),
        telemetry: MemoryOsCheckpointTelemetry {
            current_fact_count: 0,
            recent_change_count: session.shells.len(),
            live_claim_count: 0,
            open_obligation_count: selected_items
                .iter()
                .filter(|item| item.section == "open_obligations")
                .count(),
            artifact_handle_count: 0,
            failure_count: selected_items
                .iter()
                .filter(|item| item.section == "current_failures")
                .count(),
        },
        selected_items,
        exclusions: Vec::new(),
        reentry: MemoryOsCheckpointReentry {
            recommended_command,
            current_recommendation: last_prompt,
            first_question: "What still matters from this session?".to_string(),
            first_verification:
                "Verify the recommended command against current repo state before acting."
                    .to_string(),
        },
    }
}

#[derive(Debug, Clone)]
struct SessionSemanticFact {
    section: &'static str,
    kind: &'static str,
    summary: String,
    score: i64,
}

fn session_semantic_items(session: &SessionRecord) -> Vec<MemoryOsPacketSelection> {
    let mut facts = Vec::new();
    let prompt_count = session.user_prompts.len().max(1);
    for (index, prompt) in session.user_prompts.iter().enumerate() {
        let text = prompt.text.trim();
        if semantic_text_is_noise(text) {
            continue;
        }
        let Some(summary) = semantic_summary(text) else {
            continue;
        };
        let recency_boost = ((index + 1) * 20 / prompt_count) as i64;
        for (section, kind, score) in semantic_fact_categories(&summary) {
            facts.push(SessionSemanticFact {
                section,
                kind,
                summary: summary.clone(),
                score: score + recency_boost,
            });
        }
    }

    facts.sort_by(|left, right| {
        right
            .score
            .cmp(&left.score)
            .then(left.section.cmp(right.section))
            .then(left.summary.cmp(&right.summary))
    });

    let mut items = Vec::new();
    let mut seen = HashSet::new();
    for fact in facts {
        let key = format!("{}:{}", fact.section, fact.summary.to_ascii_lowercase());
        if !seen.insert(key) {
            continue;
        }
        items.push(MemoryOsPacketSelection {
            section: fact.section.to_string(),
            kind: fact.kind.to_string(),
            summary: fact.summary.clone(),
            token_estimate: fact.summary.split_whitespace().count().min(80),
            score: fact.score,
            artifact_id: None,
            subject: Some(format!(
                "semantic:{}:{}",
                fact.kind,
                hash_text(&fact.summary)
            )),
            provenance: vec![format!("session:{}", session.source.as_str())],
        });
        if items.len() >= MAX_SEMANTIC_ITEMS_PER_SESSION {
            break;
        }
    }
    items
}

fn semantic_summary(text: &str) -> Option<String> {
    let compact = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let compact = compact.trim().trim_matches('"');
    if compact.split_whitespace().count() < 5 {
        return None;
    }
    let mut selected = Vec::new();
    for sentence in compact
        .split(|ch: char| matches!(ch, '.' | '?' | '!'))
        .map(str::trim)
        .filter(|sentence| sentence.split_whitespace().count() >= 4)
    {
        selected.push(sentence.to_string());
        if selected.len() >= 2 {
            break;
        }
    }
    let summary = if selected.is_empty() {
        compact.to_string()
    } else {
        selected.join(". ")
    };
    Some(compact_semantic_summary(&summary, 360))
}

fn compact_semantic_summary(text: &str, max_len: usize) -> String {
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

fn semantic_text_is_noise(text: &str) -> bool {
    let lowered = text.trim().to_ascii_lowercase();
    if lowered.is_empty() {
        return true;
    }
    let starts = [
        "read c:\\",
        "read c:/",
        "read .omx",
        "context ",
        "cd ",
        "git ",
        "npm ",
        "npx ",
        "cargo ",
        "node ",
        "python ",
        "run /",
        "# /",
        "omx2 team",
        "omx team",
        "$team",
        "leader task:",
        "<task>",
        "<skill>",
        "<turn_aborted>",
        "<task-notification>",
        "<subagent_notification>",
        "base directory for this skill",
    ];
    if starts.iter().any(|needle| lowered.starts_with(needle)) {
        return true;
    }
    let markers = [
        "inbox.md",
        "worker-",
        ".omx",
        ".omx2",
        "codex-state",
        "skill.md",
        "allowed-tools",
        "keywords:",
        "description:",
        "name:",
        "</skill>",
        "for (needle",
        "(needle, weight)",
        "execute your assignment",
        "status.json",
        "report concrete status",
        "output-file>",
        "<tool-use-id>",
        "[request interrupted by user]",
    ];
    markers.iter().any(|needle| lowered.contains(needle))
}

fn semantic_fact_categories(text: &str) -> Vec<(&'static str, &'static str, i64)> {
    let lowered = text.to_ascii_lowercase();
    let mut categories = Vec::new();

    if contains_any(
        &lowered,
        &[
            "not done until",
            "still not returning",
            "work on it if",
            "work on it until",
            "work on this until",
            "continue fixing",
            "fixing issues",
            "current task",
            "next task",
            "pickup plan",
            "unfinished work",
            "approved plan",
            "this needs to",
        ],
    ) {
        categories.push(("user_active_work", "current-work", 120));
    }

    if contains_any(
        &lowered,
        &[
            "lead database",
            "bad-average websites",
            "sales-autopilot",
            "outreach",
            "paying customers",
            "kpi",
            "opsp",
            "business strategy",
            "annual goal",
        ],
    ) {
        categories.push(("user_strategy_facts", "business-strategy", 100));
    }

    if contains_any(
        &lowered,
        &[
            "sitesorted",
            "site sorted",
            "watcher-v2",
            "siterecord",
            "extract-analyse-generate",
            "clone-rebind",
            "bach-deal",
            "context memory os",
            "memory os",
            "munin",
        ],
    ) {
        categories.push(("user_project_facts", "project-focus", 90));
    }

    if contains_any(
        &lowered,
        &[
            "i prefer",
            "i don't want",
            "i dont want",
            "don't stop",
            "dont stop",
            "autonomously",
            "poll every",
            "approval",
            "commit",
            "read-only",
            "do not edit",
            "full qa",
            "inspect",
        ],
    ) {
        categories.push(("user_work_style", "working-preference", 80));
    }

    if contains_any(
        &lowered,
        &[
            "don't want the look",
            "dont want the look",
            "keep the look",
            "functional changes",
            "i don't want the look",
            "i dont want the look",
        ],
    ) {
        categories.push(("user_product_constraints", "product-constraint", 75));
    }

    if contains_any(
        &lowered,
        &[
            "memory os",
            "startup brief",
            "recall",
            "what do you know about me",
            "active work",
            "session corpus",
            "useful pertinent information",
            "command noise",
        ],
    ) {
        categories.push(("user_memory_requirements", "memory-os-direction", 110));
    }

    categories
}

fn contains_any(text: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| text.contains(needle))
}

fn summarize_shell_for_checkpoint(
    shell: &crate::analytics::session_impact_cmd::ShellExecution,
) -> String {
    let base = extract_base_command(&shell.command);
    let first_line = shell
        .output
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("")
        .trim();
    if first_line.is_empty() {
        format!("{} failed", base)
    } else {
        format!("{} -> {}", base, first_line)
    }
}

fn correction_execution_details(
    session: &SessionRecord,
    correction: &CorrectionOccurrence,
) -> Option<(String, i32)> {
    for shell in session.shells.iter().skip(correction.right_index) {
        if shell.command == correction.pair.right_command {
            return Some((
                shell.timestamp.to_rfc3339(),
                match shell.outcome {
                    CommandOutcome::Success => 0,
                    CommandOutcome::Failure => 1,
                    CommandOutcome::Unknown => 2,
                },
            ));
        }
    }
    None
}

fn correction_observed_at(
    session: &SessionRecord,
    correction: &CorrectionOccurrence,
) -> Option<String> {
    session
        .shells
        .get(correction.wrong_index)
        .map(|shell| shell.timestamp.to_rfc3339())
}

fn session_shell_event_type(command: &str) -> &'static str {
    let normalized = normalize_replay_command(command);
    if normalized.starts_with("cargo test") {
        "cargo-test"
    } else if normalized.starts_with("cargo build") {
        "cargo-build"
    } else if normalized.starts_with("cargo check") {
        "cargo-check"
    } else if normalized.starts_with("cargo clippy") {
        "cargo-clippy"
    } else if normalized.starts_with("cargo fmt") {
        "cargo-fmt"
    } else if normalized.starts_with("cargo install") {
        "cargo-install"
    } else if normalized.starts_with("cargo nextest") {
        "cargo-nextest"
    } else if normalized.starts_with("pytest") || normalized.contains(" pytest ") {
        "pytest"
    } else if normalized.starts_with("tsc") || normalized.contains(" tsc ") {
        "tsc"
    } else if normalized.starts_with("go build") {
        "go-build"
    } else if normalized.starts_with("go test") {
        "go-test"
    } else if normalized.starts_with("go vet") {
        "go-vet"
    } else if normalized.starts_with("mypy") {
        "mypy"
    } else if normalized.starts_with("rspec") {
        "rspec"
    } else if normalized.starts_with("rubocop") {
        "rubocop"
    } else if normalized.starts_with("next build") || normalized.contains(" next build") {
        "next-build"
    } else if normalized.starts_with("ruff format") {
        "ruff-format"
    } else if normalized.starts_with("ruff") {
        "ruff"
    } else {
        "session-shell"
    }
}

fn session_subject_key(event_type: &str, command: &str, project_path: &str) -> String {
    match event_type {
        "cargo-build" | "cargo-test" | "cargo-clippy" | "cargo-check" | "cargo-fmt"
        | "cargo-install" | "cargo-nextest" | "pytest" | "tsc" | "go-build" | "go-test"
        | "go-vet" | "mypy" | "rspec" | "rubocop" | "next-build" | "ruff" | "ruff-format" => {
            format!("{}:{}", event_type, project_path)
        }
        _ => format!(
            "session-shell:{}:{}",
            extract_base_command(&normalize_replay_command(command)),
            project_path
        ),
    }
}

fn normalize_replay_command(command: &str) -> String {
    let trimmed = command.trim();
    trimmed
        .strip_prefix("context ")
        .unwrap_or(trimmed)
        .to_ascii_lowercase()
}

fn processed_session_key(session: &SessionRecord) -> String {
    format!("{}:{}", session.source.as_str(), session.session_id)
}

fn hash_text(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn onboarding_state_path() -> Result<PathBuf> {
    let root = crate::core::config::context_data_dir()?;
    fs::create_dir_all(&root)?;
    Ok(root.join(ONBOARDING_STATE_FILE))
}

fn load_state() -> Result<SessionBackfillState> {
    let path = onboarding_state_path()?;
    if !path.exists() {
        return Ok(SessionBackfillState {
            schema_version: ONBOARDING_SCHEMA_VERSION.to_string(),
            ..Default::default()
        });
    }
    let content = fs::read_to_string(&path)?;
    let state: SessionBackfillState = serde_json::from_str(&content)?;
    Ok(state)
}

fn save_state(state: &SessionBackfillState) -> Result<()> {
    let path = onboarding_state_path()?;
    fs::write(path, serde_json::to_string_pretty(state)?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analytics::session_impact_cmd::{
        CommandOutcome, SessionSource, ShellExecution, UserPrompt,
    };
    use chrono::DateTime;

    fn sample_session() -> SessionRecord {
        SessionRecord {
            source: SessionSource::Claude,
            session_id: "session-001".to_string(),
            cwd: "C:\\repo".to_string(),
            started_at: DateTime::parse_from_rfc3339("2026-04-10T00:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            user_prompts: vec![UserPrompt {
                timestamp: DateTime::parse_from_rfc3339("2026-04-10T00:00:00Z")
                    .unwrap()
                    .with_timezone(&Utc),
                text: "Fix the CLI flow".to_string(),
            }],
            shells: vec![
                ShellExecution {
                    timestamp: DateTime::parse_from_rfc3339("2026-04-10T00:00:01Z")
                        .unwrap()
                        .with_timezone(&Utc),
                    command: "git commit --ammend".to_string(),
                    output: "error: unexpected argument '--ammend'".to_string(),
                    outcome: CommandOutcome::Failure,
                },
                ShellExecution {
                    timestamp: DateTime::parse_from_rfc3339("2026-04-10T00:00:02Z")
                        .unwrap()
                        .with_timezone(&Utc),
                    command: "git commit --amend".to_string(),
                    output: "Done".to_string(),
                    outcome: CommandOutcome::Success,
                },
            ],
        }
    }

    #[test]
    fn session_shell_event_type_matches_known_commands() {
        assert_eq!(session_shell_event_type("cargo test --all"), "cargo-test");
        assert_eq!(session_shell_event_type("pytest -q"), "pytest");
        assert_eq!(session_shell_event_type("echo hello"), "session-shell");
    }

    #[test]
    fn session_checkpoint_capture_prefers_last_correction_command() {
        let capture = session_checkpoint_capture(&sample_session());
        assert_eq!(capture.reentry.recommended_command, "git commit --amend");
        assert!(capture
            .selected_items
            .iter()
            .any(|item| item.section == "current_failures"));
        assert!(capture
            .selected_items
            .iter()
            .any(|item| item.section == "open_obligations"));
        assert!(capture
            .selected_items
            .iter()
            .any(|item| { item.section == "user_prompts" && item.summary == "Fix the CLI flow" }));
    }

    #[test]
    fn session_checkpoint_capture_emits_typed_semantic_items() {
        let mut session = sample_session();
        session.user_prompts.push(UserPrompt {
            timestamp: DateTime::parse_from_rfc3339("2026-04-10T00:00:03Z")
                .unwrap()
                .with_timezone(&Utc),
            text: "please review and inspect Memory OS output and work on it until it shows useful pertinent information".to_string(),
        });
        session.user_prompts.push(UserPrompt {
            timestamp: DateTime::parse_from_rfc3339("2026-04-10T00:00:04Z")
                .unwrap()
                .with_timezone(&Utc),
            text: "I want you to scrape NZ builders and create a lead database for small businesses with bad websites.".to_string(),
        });

        let capture = session_checkpoint_capture(&session);

        assert!(capture
            .selected_items
            .iter()
            .any(|item| item.section == "user_active_work" && item.kind == "current-work"));
        assert!(capture
            .selected_items
            .iter()
            .any(|item| item.section == "user_strategy_facts" && item.kind == "business-strategy"));
        assert!(capture.selected_items.iter().any(|item| item
            .subject
            .as_deref()
            .unwrap_or("")
            .starts_with("semantic:")));
    }

    #[test]
    fn processed_session_key_is_source_scoped() {
        let session = sample_session();
        assert_eq!(processed_session_key(&session), "claude:session-001");
    }

    #[test]
    fn completed_backfill_rechecks_when_state_is_stale_or_old_schema() {
        let fresh_state = SessionBackfillState {
            schema_version: ONBOARDING_SCHEMA_VERSION.to_string(),
            completed_at: Some(Utc::now().to_rfc3339()),
            last_checked_at: Some(Utc::now().to_rfc3339()),
            ..Default::default()
        };
        assert!(should_skip_incremental_backfill(&fresh_state));

        let stale_state = SessionBackfillState {
            last_checked_at: Some(
                (Utc::now() - chrono::Duration::minutes(INCREMENTAL_CHECK_INTERVAL_MINUTES + 1))
                    .to_rfc3339(),
            ),
            ..fresh_state.clone()
        };
        assert!(!should_skip_incremental_backfill(&stale_state));

        let old_schema_state = SessionBackfillState {
            schema_version: "memory-os-session-onboarding-v2".to_string(),
            ..fresh_state
        };
        assert!(!should_skip_incremental_backfill(&old_schema_state));
    }
}
