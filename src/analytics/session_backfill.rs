//! Automatic first-run session backfill into Memory OS.

use crate::analytics::session_impact_cmd::{
    load_sessions, project_roots_for_session_discovery, session_home_dir, CommandOutcome,
    SessionRecord,
};
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
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;
use std::time::Duration;

const ONBOARDING_SCHEMA_VERSION: &str = "memory-os-session-onboarding-v16";
const ONBOARDING_STATE_FILE: &str = "memory_os_session_onboarding.json";
const ONBOARDING_LOCK_DB_FILE: &str = "memory_os_session_onboarding.lock.sqlite";
const INCREMENTAL_CHECK_INTERVAL_MINUTES: i64 = 15;
const BACKFILL_LOCK_TIMEOUT_SECONDS: u64 = 120;
const MAX_SEMANTIC_ITEMS_PER_SESSION: usize = 8;
const SESSION_SUMMARY_BULLET_COUNT: usize = 5;
const SESSION_SUMMARY_MAX_BULLET_CHARS: usize = 260;

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
    ensure_memory_os_session_backfill_internal(false, false)
}

pub fn refresh_memory_os_session_import_before_read() -> Result<()> {
    ensure_memory_os_session_backfill_check_now()
        .context("Failed to refresh Memory OS session import before reading compiled state")?;
    Ok(())
}

pub fn ensure_memory_os_session_backfill_check_now() -> Result<Option<SessionBackfillReport>> {
    ensure_memory_os_session_backfill_internal(false, true)
}

pub fn ensure_memory_os_session_backfill_with_force(
    force: bool,
) -> Result<Option<SessionBackfillReport>> {
    ensure_memory_os_session_backfill_internal(force, false)
}

fn ensure_memory_os_session_backfill_internal(
    force: bool,
    bypass_incremental_interval: bool,
) -> Result<Option<SessionBackfillReport>> {
    if cfg!(test)
        || std::env::var("MUNIN_SKIP_MEMORY_OS_ONBOARDING")
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

    let _lock = acquire_backfill_lock()?;
    let mut state = load_state()?;
    if force {
        state.completed_at = None;
        state.last_checked_at = None;
        state.processed_session_ids.clear();
        state.sessions_processed = 0;
        state.shells_ingested = 0;
        state.corrections_ingested = 0;
    }
    if !force && should_skip_incremental_backfill(&state, bypass_incremental_interval) {
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

fn should_skip_incremental_backfill(
    state: &SessionBackfillState,
    bypass_incremental_interval: bool,
) -> bool {
    if state.schema_version != ONBOARDING_SCHEMA_VERSION || state.completed_at.is_none() {
        return false;
    }
    if bypass_incremental_interval {
        return false;
    }
    if std::env::var("MUNIN_MEMORY_OS_FORCE_ONBOARDING")
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
    let Some(home) = session_home_dir() else {
        return Ok(Vec::new());
    };
    let root = home
        .join("Documents")
        .join("Obsidian Vault")
        .join("Sessions");
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
        tracker.record_memory_os_read_model_action_observation_for_project(
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
            tracker.record_memory_os_read_model_action_execution_at_for_project(
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
    resolve_recall_project_path_with_roots(project, &project_roots_for_session_discovery())
}

fn resolve_recall_project_path_with_roots(project: &str, roots: &[PathBuf]) -> String {
    let project = project.trim();
    if project.is_empty() {
        return "recall://unknown".to_string();
    }
    let direct = PathBuf::from(project);
    if direct.is_absolute() && direct.exists() {
        return direct.to_string_lossy().to_string();
    }
    for root in roots {
        let candidate = root.join(project);
        if candidate.exists() {
            return candidate.to_string_lossy().to_string();
        }
    }
    format!("recall://{}", project)
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
    let corrections = session_correction_occurrences(session);
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

    for correction in corrections.iter().take(3) {
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
    selected_items.push(session_summary_selection(session, &corrections));

    let last_successful_command = session
        .shells
        .iter()
        .rev()
        .find(|shell| shell.outcome.is_success())
        .map(|shell| shell.command.clone());
    let recommended_command = corrections
        .last()
        .map(|correction| correction.pair.right_command.clone())
        .or(last_successful_command)
        .unwrap_or_else(|| "munin resume --format prompt".to_string());

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

fn session_summary_selection(
    session: &SessionRecord,
    corrections: &[CorrectionOccurrence],
) -> MemoryOsPacketSelection {
    let bullets = session_summary_bullets(session, corrections);
    let summary = bullets.join("\n");
    MemoryOsPacketSelection {
        section: "session_summary".to_string(),
        kind: "session-summary".to_string(),
        summary: summary.clone(),
        token_estimate: summary.split_whitespace().count().min(180),
        score: 1800,
        artifact_id: Some(format!(
            "session-summary:{}:{}",
            session.source.as_str(),
            hash_text(&session.session_id)
        )),
        subject: Some(format!(
            "session-summary:{}:{}",
            session.source.as_str(),
            session.session_id
        )),
        provenance: vec![
            format!("session:{}", session.source.as_str()),
            format!("session-id:{}", session.session_id),
        ],
    }
}

fn session_summary_bullets(
    session: &SessionRecord,
    corrections: &[CorrectionOccurrence],
) -> Vec<String> {
    let meaningful_prompts = session
        .user_prompts
        .iter()
        .map(|prompt| prompt.text.trim())
        .filter(|text| !text.is_empty() && !semantic_text_is_noise(text))
        .collect::<Vec<_>>();
    let first_prompt = meaningful_prompts.first().copied();
    let last_prompt = meaningful_prompts.last().copied();
    let shell_count = session.shells.len();
    let success_count = session
        .shells
        .iter()
        .filter(|shell| shell.outcome.is_success())
        .count();
    let failure_count = session
        .shells
        .iter()
        .filter(|shell| shell.outcome.is_failure())
        .count();
    let unknown_count = shell_count.saturating_sub(success_count + failure_count);
    let project = compact_semantic_summary(session.cwd.trim(), 120);
    let mut bullets = Vec::new();

    bullets.push(format!(
        "Session summary: {} session `{}` for `{}` started {}.",
        session.source.as_str(),
        compact_semantic_summary(&session.session_id, 80),
        if project.is_empty() {
            "unknown project".to_string()
        } else {
            project
        },
        session.started_at.to_rfc3339()
    ));

    bullets.push(match first_prompt {
        Some(prompt) => format!(
            "User asked first: {}",
            punctuated_session_fragment(&sentence_for_session_summary(prompt))
        ),
        None => "User asked first: no direct user prompt was captured in this source session."
            .to_string(),
    });

    bullets.push(match (first_prompt, last_prompt) {
        (Some(first), Some(last)) if first != last => {
            format!(
                "Latest user ask: {}",
                punctuated_session_fragment(&sentence_for_session_summary(last))
            )
        }
        (Some(_), Some(_)) => format!(
            "Prompt coverage: {} captured user prompt(s); the latest ask matches the first ask.",
            meaningful_prompts.len()
        ),
        _ => format!(
            "Prompt coverage: {} captured user prompt(s); use command activity for extra context.",
            meaningful_prompts.len()
        ),
    });

    bullets.push(format!(
        "Work performed: {} command(s) captured, {} succeeded, {} failed, {} had unknown outcome.",
        shell_count, success_count, failure_count, unknown_count
    ));

    bullets.push(session_summary_handoff(session, corrections));

    while bullets.len() < SESSION_SUMMARY_BULLET_COUNT {
        bullets.push(
            "Recall handoff: verify this summary against current repo state before acting."
                .to_string(),
        );
    }

    bullets
        .into_iter()
        .take(SESSION_SUMMARY_BULLET_COUNT)
        .map(|bullet| {
            format!(
                "- {}",
                compact_semantic_summary(
                    bullet.trim().trim_start_matches("- "),
                    SESSION_SUMMARY_MAX_BULLET_CHARS
                )
            )
        })
        .collect()
}

fn session_summary_handoff(
    session: &SessionRecord,
    corrections: &[CorrectionOccurrence],
) -> String {
    if let Some(correction) = corrections.last() {
        return format!(
            "Important handoff: a correction replaced `{}` with `{}`; verify the corrected command before reuse.",
            command_for_session_summary(&correction.pair.wrong_command),
            command_for_session_summary(&correction.pair.right_command)
        );
    }

    if let Some(shell) = session
        .shells
        .iter()
        .rev()
        .find(|shell| shell.outcome.is_failure())
    {
        return format!(
            "Important handoff: the most recent failure involved `{}`; inspect current state before retrying.",
            command_for_session_summary(&shell.command)
        );
    }

    if let Some(shell) = session.shells.iter().rev().find(|shell| {
        shell.outcome.is_success() && !low_information_command_summary(&shell.command)
    }) {
        return format!(
            "Important handoff: the latest successful command was `{}`; rerun only after checking current state.",
            command_for_session_summary(&shell.command)
        );
    }

    if session
        .shells
        .iter()
        .any(|shell| shell.outcome.is_success())
    {
        "Important handoff: only low-information navigation or inspection commands were captured; use the prompt summary as recall context."
            .to_string()
    } else {
        "Important handoff: no shell command evidence was captured; use the prompt summary as recall context."
            .to_string()
    }
}

fn sentence_for_session_summary(text: &str) -> String {
    let compact = scrub_local_paths_for_summary(&clean_prompt_for_session_summary(text))
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let sentence = compact
        .split(|ch: char| matches!(ch, '\n' | '\r'))
        .next()
        .unwrap_or(compact.as_str())
        .trim()
        .trim_matches('"')
        .trim_end_matches(['.', '?', '!']);
    compact_semantic_summary(sentence, 180)
}

fn punctuated_session_fragment(fragment: &str) -> String {
    let trimmed = fragment.trim();
    if trimmed.ends_with('.') || trimmed.ends_with('?') || trimmed.ends_with('!') {
        trimmed.to_string()
    } else {
        format!("{trimmed}.")
    }
}

fn command_for_session_summary(command: &str) -> String {
    let command = command.trim();
    if command.contains("&&")
        || command.contains('|')
        || command.contains("2>&1")
        || command.len() > 120
    {
        let normalized = normalize_replay_command(command);
        let base = extract_base_command(&normalized);
        if !base.trim().is_empty() {
            if base.contains(':') || base.contains('/') || base.contains('\\') {
                return normalized
                    .split_whitespace()
                    .next()
                    .unwrap_or(base.as_str())
                    .to_string();
            }
            return base;
        }
    }

    let compact = command
        .split_whitespace()
        .take(10)
        .collect::<Vec<_>>()
        .join(" ");
    compact_semantic_summary(&compact, 120)
}

fn low_information_command_summary(command: &str) -> bool {
    matches!(
        command_for_session_summary(command).as_str(),
        "cd" | "ls" | "dir" | "pwd" | "get-childitem" | "get-content" | "cat" | "type"
    )
}

fn clean_prompt_for_session_summary(text: &str) -> String {
    let compact = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if let Some(command_args) = extract_tag_content(&compact, "command-args") {
        return strip_angle_tags(&command_args);
    }
    let without_command_message = remove_tag_block(&compact, "command-message");
    let without_command_name = remove_tag_block(&without_command_message, "command-name");
    strip_angle_tags(&without_command_name)
}

fn scrub_local_paths_for_summary(text: &str) -> String {
    text.split_whitespace()
        .map(|token| {
            let trimmed =
                token.trim_matches(|ch| matches!(ch, '\'' | '"' | '`' | '(' | ')' | '[' | ']'));
            if trimmed.starts_with("C:\\")
                || trimmed.starts_with("C:/")
                || trimmed.starts_with("c:\\")
                || trimmed.starts_with("c:/")
            {
                let prefix = token
                    .chars()
                    .take_while(|ch| matches!(ch, '\'' | '"' | '`' | '(' | '['))
                    .collect::<String>();
                let suffix = token
                    .chars()
                    .rev()
                    .take_while(|ch| {
                        matches!(ch, '\'' | '"' | '`' | ')' | ']' | '.' | ',' | ';' | ':')
                    })
                    .collect::<String>()
                    .chars()
                    .rev()
                    .collect::<String>();
                format!("{prefix}[local path]{suffix}")
            } else {
                token.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn extract_tag_content(text: &str, tag: &str) -> Option<String> {
    let start_tag = format!("<{}>", tag);
    let end_tag = format!("</{}>", tag);
    let start = text.find(&start_tag)? + start_tag.len();
    let end = text[start..].find(&end_tag)? + start;
    Some(text[start..end].trim().to_string())
}

fn remove_tag_block(text: &str, tag: &str) -> String {
    let start_tag = format!("<{}>", tag);
    let end_tag = format!("</{}>", tag);
    let mut remaining = text.to_string();
    while let Some(start) = remaining.find(&start_tag) {
        let Some(end_offset) = remaining[start..].find(&end_tag) else {
            break;
        };
        let end = start + end_offset + end_tag.len();
        remaining.replace_range(start..end, "");
    }
    remaining
}

fn strip_angle_tags(text: &str) -> String {
    let mut cleaned = String::new();
    let mut in_tag = false;
    for ch in text.chars() {
        match ch {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if !in_tag => cleaned.push(ch),
            _ => {}
        }
    }
    cleaned.trim().to_string()
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
            "example-project",
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

fn onboarding_lock_path() -> Result<PathBuf> {
    let root = crate::core::config::context_data_dir()?;
    fs::create_dir_all(&root)?;
    Ok(root.join(ONBOARDING_LOCK_DB_FILE))
}

struct SessionBackfillLock {
    conn: Connection,
}

impl Drop for SessionBackfillLock {
    fn drop(&mut self) {
        let _ = self.conn.execute_batch("COMMIT;");
    }
}

fn acquire_backfill_lock() -> Result<SessionBackfillLock> {
    let path = onboarding_lock_path()?;
    let conn = Connection::open(&path).with_context(|| {
        format!(
            "failed to open Memory OS session import lock {}",
            path.display()
        )
    })?;
    conn.busy_timeout(Duration::from_secs(BACKFILL_LOCK_TIMEOUT_SECONDS))
        .with_context(|| {
            format!(
                "failed to configure Memory OS session import lock timeout at {}",
                path.display()
            )
        })?;
    conn.execute_batch("BEGIN IMMEDIATE TRANSACTION;")
        .with_context(|| {
            format!(
                "Timed out waiting for Memory OS session import lock at {}",
                path.display()
            )
        })?;
    Ok(SessionBackfillLock { conn })
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
    use std::sync::Mutex;
    use tempfile::TempDir;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

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
        assert!(capture
            .selected_items
            .iter()
            .any(|item| item.section == "session_summary" && item.kind == "session-summary"));
    }

    #[test]
    fn session_checkpoint_capture_attaches_exactly_five_summary_bullets() {
        let capture = session_checkpoint_capture(&sample_session());
        let summary = capture
            .selected_items
            .iter()
            .find(|item| item.kind == "session-summary")
            .expect("session summary");

        let bullet_lines = summary
            .summary
            .lines()
            .filter(|line| line.starts_with("- "))
            .collect::<Vec<_>>();

        assert_eq!(bullet_lines.len(), 5);
        assert!(summary.summary.split("\n\n").count() <= 5);
        assert!(summary.summary.contains("Session summary: claude session"));
        assert!(summary
            .summary
            .contains("Work performed: 2 command(s) captured"));
        assert!(summary.summary.contains("git commit --amend"));
        assert!(summary
            .subject
            .as_deref()
            .unwrap_or_default()
            .starts_with("session-summary:claude:session-001"));
    }

    #[test]
    fn session_summary_removes_command_wrapper_and_long_shell_noise() {
        let mut session = sample_session();
        session.user_prompts = vec![UserPrompt {
            timestamp: DateTime::parse_from_rfc3339("2026-04-10T00:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            text: "<command-message>plan-ceo-review</command-message> <command-name>/plan-ceo-review</command-name> <command-args>I think Munin memory needs session summaries attached to recall. Read C:\\Users\\OEM\\AppData\\Local\\context\\brief.md first.</command-args>".to_string(),
        }];
        session.shells = vec![ShellExecution {
            timestamp: DateTime::parse_from_rfc3339("2026-04-10T00:00:02Z")
                .unwrap()
                .with_timezone(&Utc),
            command:
                "context ls C:/Users/OEM/Projects/example-project/watcher-v2/logs/sales-autopilot-*.log 2>&1"
                    .to_string(),
            output: "Done".to_string(),
            outcome: CommandOutcome::Success,
        }];

        let capture = session_checkpoint_capture(&session);
        let summary = capture
            .selected_items
            .iter()
            .find(|item| item.kind == "session-summary")
            .expect("session summary");

        assert!(summary.summary.contains("I think Munin memory needs"));
        assert!(!summary.summary.contains("command-message"));
        assert!(!summary.summary.contains("sales-autopilot-*.log"));
        assert!(!summary.summary.contains("AppData\\Local\\context"));
        assert!(summary.summary.contains("[local path]"));
        assert!(summary
            .summary
            .contains("only low-information navigation or inspection commands were captured"));
    }

    #[test]
    fn session_summary_scrubs_backticked_windows_paths() {
        let text = scrub_local_paths_for_summary(
            "Read `C:\\Users\\OEM\\AppData\\Local\\context\\brief.md` before starting.",
        );

        assert!(text.contains("`[local path]`"));
        assert!(!text.contains("AppData\\Local\\context"));
    }

    #[test]
    fn recall_sessions_follow_sandboxed_session_home() {
        let _guard = ENV_LOCK.lock().expect("env lock");
        let tmp = TempDir::new().expect("temp dir");
        std::env::set_var("MUNIN_SESSION_HOME", tmp.path());

        let sessions = load_recall_sessions().expect("recall sessions");

        assert!(sessions.is_empty());
        std::env::remove_var("MUNIN_SESSION_HOME");
    }

    #[test]
    fn recall_project_resolution_uses_env_aware_project_roots() {
        let tmp = TempDir::new().expect("temp dir");
        let project_dir = tmp.path().join("Projects").join("acme");
        std::fs::create_dir_all(&project_dir).expect("project dir");

        let resolved =
            resolve_recall_project_path_with_roots("acme", &[tmp.path().join("Projects")]);

        assert_eq!(resolved, project_dir.to_string_lossy());
        assert_eq!(
            resolve_recall_project_path_with_roots("missing", &[tmp.path().join("Projects")]),
            "recall://missing"
        );
    }

    #[test]
    fn replay_session_writes_corrections_into_read_model_when_action_flag_is_off() {
        let _guard = ENV_LOCK.lock().expect("env lock");
        let tmp = TempDir::new().expect("temp dir");
        let config_dir = tmp.path().join("config");
        let data_dir = tmp.path().join("data");
        std::fs::create_dir_all(&config_dir).expect("config dir");
        std::fs::create_dir_all(&data_dir).expect("data dir");
        std::env::set_var("MUNIN_CONFIG_DIR", &config_dir);
        std::env::set_var("MUNIN_DATA_DIR", &data_dir);
        std::env::set_var("MUNIN_MEMORYOS_READ_MODEL_V1", "true");
        std::env::set_var("MUNIN_MEMORYOS_ACTION_V1", "false");

        let tracker =
            Tracker::new_at_path(&tmp.path().join("tracking.db")).expect("tracker at temp path");
        replay_session(&tracker, &sample_session()).expect("replay session");

        let report = tracker
            .get_memory_os_friction_report(
                crate::core::memory_os::MemoryOsInspectionScope::User,
                None,
            )
            .expect("friction report");

        assert_eq!(report.repeated_corrections.len(), 1);
        assert_eq!(
            report.repeated_corrections[0].wrong_command,
            "git commit --ammend"
        );
        assert_eq!(
            report.repeated_corrections[0].corrected_command,
            "git commit --amend"
        );
        assert_eq!(report.repeated_corrections[0].successful_replays, 1);

        std::env::remove_var("MUNIN_CONFIG_DIR");
        std::env::remove_var("MUNIN_DATA_DIR");
        std::env::remove_var("MUNIN_MEMORYOS_READ_MODEL_V1");
        std::env::remove_var("MUNIN_MEMORYOS_ACTION_V1");
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
        assert!(should_skip_incremental_backfill(&fresh_state, false));
        assert!(!should_skip_incremental_backfill(&fresh_state, true));

        let stale_state = SessionBackfillState {
            last_checked_at: Some(
                (Utc::now() - chrono::Duration::minutes(INCREMENTAL_CHECK_INTERVAL_MINUTES + 1))
                    .to_rfc3339(),
            ),
            ..fresh_state.clone()
        };
        assert!(!should_skip_incremental_backfill(&stale_state, false));

        let old_schema_state = SessionBackfillState {
            schema_version: "memory-os-session-onboarding-v2".to_string(),
            ..fresh_state
        };
        assert!(!should_skip_incremental_backfill(&old_schema_state, false));
    }
}
