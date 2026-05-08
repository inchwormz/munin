use anyhow::Result;
use chrono::{DateTime, Duration, Utc};
use rusqlite::params;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use super::{
    compact_display_text, memory_os_scope_params, parse_rfc3339_to_utc, push_unique_string,
    resolved_project_path, MemoryOsCheckpointEnvelope, Tracker,
};

pub(super) fn extract_replay_source(payload_json: &str) -> Option<(String, String)> {
    let payload: serde_json::Value = serde_json::from_str(payload_json).ok()?;
    let replay_source = payload.get("replay_source")?;
    let source = replay_source
        .get("session_source")
        .and_then(|value| value.as_str())
        .unwrap_or("unknown")
        .to_string();
    let session_id = replay_source
        .get("session_id")
        .and_then(|value| value.as_str())
        .unwrap_or("unknown")
        .to_string();
    Some((source, session_id))
}

fn correction_source_from_ref(source_ref: &str) -> String {
    source_ref
        .split(':')
        .next()
        .unwrap_or("unknown")
        .to_string()
}

fn classify_misunderstanding_label(error_kind: &str, wrong_command: &str) -> String {
    let lowered = error_kind.to_ascii_lowercase();
    if lowered.contains("flag") || wrong_command.contains("--") {
        "CLI syntax drift".to_string()
    } else if lowered.contains("path")
        || lowered.contains("file")
        || wrong_command.contains('\\')
        || wrong_command.contains('/')
    {
        "Path assumption drift".to_string()
    } else if lowered.contains("command") || lowered.contains("tool") {
        "Tool availability drift".to_string()
    } else {
        "Execution assumption drift".to_string()
    }
}

pub(super) fn first_non_empty(values: &[Option<String>]) -> Option<String> {
    values
        .iter()
        .flatten()
        .find(|value| !value.trim().is_empty())
        .cloned()
}

pub(super) fn meaningful_checkpoint_summary(text: &str) -> Option<String> {
    let compact = compact_display_text(text, 160);
    if compact.trim().is_empty() {
        return None;
    }
    let lowered = compact.to_ascii_lowercase();
    if checkpoint_summary_is_command_or_build_noise(&lowered) {
        return None;
    }
    Some(compact)
}

fn checkpoint_summary_is_command_or_build_noise(lowered: &str) -> bool {
    if lowered.starts_with("exit code:")
        || lowered.starts_with("exit ")
        || lowered.contains("blocked by policy")
        || lowered.starts_with("error: process didn't exit")
    {
        return true;
    }

    let command_prefixes = [
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
    ];
    if command_prefixes
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
        " | branch ",
        "staged ",
        "modified ",
        "shell executions",
        "shells/session",
        "build output",
        "cargo build:",
        "cargo test:",
        "npm run build",
        "next build",
        "compiled successfully",
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

pub(super) fn memory_os_serving_policy_lines() -> Vec<String> {
    vec![
        "For 'what do you know about me', 'how do I like to work', 'what am I working on', and 'what are the next best steps', read Memory OS projections first.".to_string(),
        "Answer from the compiled user/profile/active-work/friction state before opening raw recall or session history.".to_string(),
        "Use recall or raw session history only as fallback evidence or provenance when the Memory OS view is missing detail.".to_string(),
    ]
}

pub(super) fn build_memory_os_imported_sources(
    imported_source_counts: &[(String, usize)],
    replay_shells: &[MemoryOsReplayShellRow],
) -> Vec<crate::core::memory_os::MemoryOsImportedSourceSummary> {
    let mut shell_counts: HashMap<String, usize> = HashMap::new();
    for shell in replay_shells {
        *shell_counts.entry(shell.source.clone()).or_default() += 1;
    }

    let mut sources = imported_source_counts
        .iter()
        .map(
            |(source, sessions)| crate::core::memory_os::MemoryOsImportedSourceSummary {
                source: source.clone(),
                sessions: *sessions,
                shell_executions: shell_counts.remove(source).unwrap_or(0),
            },
        )
        .collect::<Vec<_>>();

    for (source, shell_executions) in shell_counts {
        sources.push(crate::core::memory_os::MemoryOsImportedSourceSummary {
            source,
            sessions: 0,
            shell_executions,
        });
    }

    sources.sort_by(|left, right| {
        right
            .sessions
            .cmp(&left.sessions)
            .then(right.shell_executions.cmp(&left.shell_executions))
            .then(left.source.cmp(&right.source))
    });
    sources
}

pub(super) fn build_memory_os_friction_triggers(
    correction_patterns: &[crate::core::memory_os::MemoryOsCorrectionPatternSummary],
) -> Vec<crate::core::memory_os::MemoryOsNarrativeFinding> {
    let mut seen_labels: HashSet<String> = HashSet::new();
    correction_patterns
        .iter()
        .filter_map(|pattern| {
            let label =
                classify_misunderstanding_label(&pattern.error_kind, &pattern.wrong_command);
            if !seen_labels.insert(label.clone()) {
                return None;
            }
            Some(crate::core::memory_os::MemoryOsNarrativeFinding {
                title: label,
                summary: format!(
                    "{} appears repeatedly in command-correction memory.",
                    pattern.error_kind
                ),
                evidence: vec![format!(
                    "{} hits, {} successful replays",
                    pattern.count, pattern.successful_replays
                )],
            })
        })
        .take(4)
        .collect()
}

pub(super) fn build_memory_os_misunderstandings(
    correction_patterns: &[crate::core::memory_os::MemoryOsCorrectionPatternSummary],
) -> Vec<crate::core::memory_os::MemoryOsMisunderstandingPattern> {
    let mut grouped: HashMap<String, crate::core::memory_os::MemoryOsMisunderstandingPattern> =
        HashMap::new();
    for pattern in correction_patterns.iter().take(12) {
        let label = classify_misunderstanding_label(&pattern.error_kind, &pattern.wrong_command);
        let entry = grouped.entry(label.clone()).or_insert_with(|| {
            crate::core::memory_os::MemoryOsMisunderstandingPattern {
                label: label.clone(),
                summary: format!("{label} shows up repeatedly in correction memory."),
                count: 0,
                examples: Vec::new(),
            }
        });
        entry.count += pattern.count;
        push_unique_string(
            &mut entry.examples,
            format!("{} -> {}", pattern.wrong_command, pattern.corrected_command),
        );
    }
    let mut patterns = grouped.into_values().collect::<Vec<_>>();
    patterns.sort_by(|left, right| {
        right
            .count
            .cmp(&left.count)
            .then(left.label.cmp(&right.label))
    });
    patterns.truncate(6);
    patterns
}

pub(super) fn build_memory_os_friction_fixes(
    correction_patterns: &[crate::core::memory_os::MemoryOsCorrectionPatternSummary],
    likely_misunderstandings: &[crate::core::memory_os::MemoryOsMisunderstandingPattern],
    behavior_changes: &[crate::core::memory_os::MemoryOsBehaviorChangeRecommendation],
    redirects: &crate::core::memory_os::MemoryOsRedirectSummary,
    checkpoints: &[MemoryOsCheckpointEnvelope],
    durable_fixes: &UserProseDurableFixes,
) -> Vec<crate::core::memory_os::MemoryOsFrictionFix> {
    let mut fixes = Vec::new();
    let now = Utc::now();
    let prose_signal_counts = count_user_prose_signals(checkpoints);
    fixes.extend(user_prose_friction_fixes(checkpoints, durable_fixes, now));
    fixes.extend(command_friction_fixes(
        correction_patterns,
        likely_misunderstandings,
    ));
    fixes.extend(behavior_change_friction_fixes(
        behavior_changes,
        redirects,
        durable_fixes,
        prose_signal_counts.latest_autonomy_at,
        now,
    ));

    let mut seen = HashSet::new();
    fixes.retain(|fix| seen.insert(fix.fix_id.clone()));
    fixes.sort_by(|left, right| {
        friction_status_rank(right.status.as_str())
            .cmp(&friction_status_rank(left.status.as_str()))
            .then(
                friction_impact_rank(right.impact.as_str())
                    .cmp(&friction_impact_rank(left.impact.as_str())),
            )
            .then(right.score.cmp(&left.score))
            .then(left.title.cmp(&right.title))
    });
    fixes
}

pub(super) fn apply_completed_friction_statuses(
    fixes: &mut [crate::core::memory_os::MemoryOsFrictionFix],
    completed: &[crate::core::tracking::ApprovalJobRecord],
) {
    for fix in fixes {
        if completed_friction_fix_matches(
            Some(fix.fix_id.as_str()),
            "friction-fix",
            &fix.evidence,
            completed,
        ) {
            fix.status = "fixed".to_string();
        }
    }
}

pub(super) fn filter_completed_behavior_changes(
    behavior_changes: Vec<crate::core::memory_os::MemoryOsBehaviorChangeRecommendation>,
    completed: &[crate::core::tracking::ApprovalJobRecord],
) -> Vec<crate::core::memory_os::MemoryOsBehaviorChangeRecommendation> {
    behavior_changes
        .into_iter()
        .filter(|change| {
            let item_id = format!("friction:behavior:{}", change.target_agent);
            !completed_friction_fix_matches(
                Some(item_id.as_str()),
                "friction-fix",
                &change.evidence,
                completed,
            )
        })
        .collect()
}

fn completed_friction_fix_matches(
    item_id: Option<&str>,
    item_kind: &str,
    evidence: &[String],
    completed: &[crate::core::tracking::ApprovalJobRecord],
) -> bool {
    let Ok(current_evidence_json) = serde_json::to_string(evidence) else {
        return false;
    };
    completed.iter().any(|record| {
        record.item_kind == item_kind
            && record.item_id.as_deref() == item_id
            && record.evidence_json == current_evidence_json
    })
}

#[derive(Debug, Default, Clone)]
pub(super) struct UserProseSignalCounts {
    pub(super) command_noise: usize,
    pub(super) autonomy: usize,
    pub(super) stale_output: usize,
    pub(super) latest_command_noise_at: Option<DateTime<Utc>>,
    pub(super) latest_autonomy_at: Option<DateTime<Utc>>,
    pub(super) command_noise_evidence: Vec<String>,
}

#[derive(Debug, Clone)]
pub(super) struct DurableFrictionFixEvidence {
    pub(super) path: String,
    pub(super) codified_at: DateTime<Utc>,
}

#[derive(Debug, Default, Clone)]
pub(super) struct UserProseDurableFixes {
    pub(super) autonomy_polling: Option<DurableFrictionFixEvidence>,
    pub(super) codex_autonomy_polling: Option<DurableFrictionFixEvidence>,
    pub(super) command_noise_surface_policy: Option<DurableFrictionFixEvidence>,
    pub(super) context_reversal_clarification: Option<DurableFrictionFixEvidence>,
}

const CONTEXT_REVERSAL_FRICTION_MARKER: &str = "munin-friction:context-reversal";

pub(super) fn detect_user_prose_durable_fixes(project_path: Option<&str>) -> UserProseDurableFixes {
    let autonomy_polling = find_durable_autonomy_polling_instruction(project_path);
    let codex_autonomy_polling = autonomy_polling
        .clone()
        .or_else(|| find_codex_durable_autonomy_polling_instruction(project_path));
    let context_reversal_clarification =
        find_context_reversal_clarification_instruction(project_path);
    UserProseDurableFixes {
        autonomy_polling,
        codex_autonomy_polling,
        command_noise_surface_policy: Some(current_binary_durable_fix_evidence(
            "Memory OS text surfaces suppress command/build noise",
        )),
        context_reversal_clarification,
    }
}

pub(super) fn count_user_prose_signals(
    checkpoints: &[MemoryOsCheckpointEnvelope],
) -> UserProseSignalCounts {
    let mut command_noise_evidence = Vec::new();
    let mut command_noise_seen = HashSet::new();
    let mut autonomy_seen = HashSet::new();
    let mut stale_output_seen = HashSet::new();
    let mut latest_command_noise_at = None;
    let mut latest_autonomy_at = None;

    for checkpoint in checkpoints
        .iter()
        .filter(|checkpoint| checkpoint.capture.profile == "session-onboarding")
    {
        for text in checkpoint_user_prose(checkpoint) {
            let signal_key = compact_display_text(text, 220).to_ascii_lowercase();
            let lowered = text.to_ascii_lowercase();
            if lowered.contains("command noise")
                || lowered.contains("garbage")
                || lowered.contains("useless")
                || lowered.contains("wtf")
                || lowered.contains("what the hell")
            {
                command_noise_seen.insert(signal_key.clone());
                counts_latest_at(
                    &mut latest_command_noise_at,
                    checkpoint_original_signal_time(checkpoint),
                );
                push_unique_string(
                    &mut command_noise_evidence,
                    format!("user correction at {}", checkpoint.capture.generated_at),
                );
            }
            if text_has_autonomy_signal(&lowered) {
                autonomy_seen.insert(signal_key.clone());
                if text_has_autonomy_correction(&lowered) {
                    counts_latest_at(
                        &mut latest_autonomy_at,
                        checkpoint_original_signal_time(checkpoint),
                    );
                }
            }
            if lowered.contains("still not returning")
                || lowered.contains("not done until")
                || lowered.contains("shows useful")
                || lowered.contains("correct info")
            {
                stale_output_seen.insert(signal_key.clone());
            }
        }
    }

    UserProseSignalCounts {
        command_noise: command_noise_seen.len(),
        autonomy: autonomy_seen.len(),
        stale_output: stale_output_seen.len(),
        latest_command_noise_at,
        latest_autonomy_at,
        command_noise_evidence,
    }
}

fn user_prose_friction_fixes(
    checkpoints: &[MemoryOsCheckpointEnvelope],
    durable_fixes: &UserProseDurableFixes,
    now: DateTime<Utc>,
) -> Vec<crate::core::memory_os::MemoryOsFrictionFix> {
    let counts = count_user_prose_signals(checkpoints);

    let mut fixes = Vec::new();
    if counts.command_noise > 0 {
        let command_noise_status = command_noise_friction_status(
            counts.latest_command_noise_at,
            durable_fixes.command_noise_surface_policy.as_ref(),
            now,
        );
        if command_noise_status != "retired" {
            let mut evidence = counts
                .command_noise_evidence
                .into_iter()
                .take(3)
                .collect::<Vec<_>>();
            if let Some(durable) = &durable_fixes.command_noise_surface_policy {
                evidence.push(format!(
                    "durable surface policy active in {} at {}",
                    durable.path,
                    durable.codified_at.to_rfc3339()
                ));
                if counts
                    .latest_command_noise_at
                    .is_some_and(|latest| latest > durable.codified_at)
                {
                    evidence.push(
                        "newer command-noise correction exists after codification".to_string(),
                    );
                }
            }
            fixes.push(crate::core::memory_os::MemoryOsFrictionFix {
                fix_id: "friction:user-command-noise".to_string(),
                title: "Stop surfacing command/build noise as memory".to_string(),
                impact: "high".to_string(),
                status: command_noise_status,
                summary: format!(
                    "User has directly corrected noisy or useless Memory OS output {} times.",
                    counts.command_noise
                ),
                permanent_fix:
                    "Keep strategy facts and user prose above shell/build output; reserve raw commands for inspect/json evidence."
                        .to_string(),
                evidence,
                score: 120 + counts.command_noise.min(20) as i64,
            });
        }
    }
    if counts.stale_output > 0 {
        fixes.push(crate::core::memory_os::MemoryOsFrictionFix {
            fix_id: "friction:stale-memory-output".to_string(),
            title: "Keep Memory OS output current and pertinent".to_string(),
            impact: "high".to_string(),
            status: "active".to_string(),
            summary: format!(
                "User has flagged stale or non-pertinent Memory OS output {} times.",
                counts.stale_output
            ),
            permanent_fix:
                "Refresh session imports before serving friction/brief surfaces and rank active work above stale session fragments."
                    .to_string(),
            evidence: vec![format!("{} stale-output corrections", counts.stale_output)],
            score: 115 + counts.stale_output.min(20) as i64,
        });
    }
    if counts.autonomy > 0 {
        let status = autonomy_polling_friction_status(
            counts.latest_autonomy_at,
            durable_fixes.autonomy_polling.as_ref(),
            now,
        );
        if status == "retired" {
            return fixes;
        }
        let mut evidence = vec![format!("{} autonomy/polling corrections", counts.autonomy)];
        if let Some(durable) = &durable_fixes.autonomy_polling {
            evidence.push(format!(
                "durable instruction codified in {} at {}",
                durable.path,
                durable.codified_at.to_rfc3339()
            ));
            if counts
                .latest_autonomy_at
                .is_some_and(|latest| latest > durable.codified_at)
            {
                evidence.push("newer autonomy correction exists after codification".to_string());
            }
        }
        fixes.push(crate::core::memory_os::MemoryOsFrictionFix {
            fix_id: "friction:autonomy-polling".to_string(),
            title: "Keep autonomous work moving without manual polling".to_string(),
            impact: "high".to_string(),
            status,
            summary: format!(
                "User has asked for stronger autonomous polling/approval behavior {} times.",
                counts.autonomy
            ),
            permanent_fix:
                "When a task calls for polling, waiting, or iterating until something is solved, keep cycling without pausing to ask. Stop only when the task is verified solved or a concrete blocker is recorded."
                    .to_string(),
            evidence,
            score: 100 + counts.autonomy.min(20) as i64,
        });
    }
    fixes
}

pub(super) fn build_memory_os_new_unproven_friction(
    checkpoints: &[MemoryOsCheckpointEnvelope],
    durable_fixes: &UserProseDurableFixes,
) -> Vec<crate::core::memory_os::MemoryOsFrictionFix> {
    let mut wrong_terminal_evidence = Vec::new();
    let mut wrong_terminal_seen = HashSet::new();
    let context_reversal_codified_at = durable_fixes
        .context_reversal_clarification
        .as_ref()
        .map(|durable| durable.codified_at);

    for checkpoint in checkpoints
        .iter()
        .filter(|checkpoint| checkpoint.capture.profile == "session-onboarding")
    {
        for text in checkpoint_user_prose(checkpoint) {
            let lowered = text.to_ascii_lowercase();
            if text_has_wrong_terminal_clarification_signal(&lowered) {
                let signal_at = checkpoint_original_signal_time(checkpoint);
                if context_reversal_codified_at.is_some_and(|codified_at| signal_at <= codified_at)
                {
                    continue;
                }
                let key = compact_display_text(text, 180).to_ascii_lowercase();
                if wrong_terminal_seen.insert(key) {
                    push_unique_string(
                        &mut wrong_terminal_evidence,
                        format!("user correction at {}", checkpoint.capture.generated_at),
                    );
                }
            }
        }
    }

    if wrong_terminal_evidence.is_empty() {
        return Vec::new();
    }

    vec![crate::core::memory_os::MemoryOsFrictionFix {
        fix_id: "friction:new-unproven:clarify-context-reversal".to_string(),
        title: "Clarify before reversing direction on likely wrong-terminal context slips"
            .to_string(),
        impact: "high".to_string(),
        status: "monitoring".to_string(),
        summary: format!(
            "User corrected a likely wrong-terminal/context-slip interpretation {} time(s).",
            wrong_terminal_seen.len()
        ),
        permanent_fix:
            "When a user message reverses the current task framing or sounds like it may belong to another terminal, ask one concise clarifying question before editing."
                .to_string(),
        evidence: wrong_terminal_evidence.into_iter().take(3).collect(),
        score: 90 + wrong_terminal_seen.len().min(10) as i64,
    }]
}

fn checkpoint_user_prose(checkpoint: &MemoryOsCheckpointEnvelope) -> impl Iterator<Item = &str> {
    checkpoint
        .capture
        .goal
        .iter()
        .map(|value| value.as_str())
        .chain(
            checkpoint
                .capture
                .reentry
                .current_recommendation
                .iter()
                .map(|value| value.as_str()),
        )
        .chain(
            checkpoint
                .capture
                .selected_items
                .iter()
                .filter(|item| item.section == "user_prompts")
                .map(|item| item.summary.as_str()),
        )
}

fn counts_latest_at(current: &mut Option<DateTime<Utc>>, candidate: DateTime<Utc>) {
    match current {
        Some(existing) if *existing >= candidate => {}
        _ => *current = Some(candidate),
    }
}

fn checkpoint_original_signal_time(checkpoint: &MemoryOsCheckpointEnvelope) -> DateTime<Utc> {
    parse_rfc3339_to_utc(&checkpoint.capture.generated_at)
}

fn text_has_autonomy_signal(lowered: &str) -> bool {
    lowered.contains("poll")
        || lowered.contains("autonomous")
        || lowered.contains("autonomously")
        || lowered.contains("keep going until")
        || lowered.contains("until it's done")
        || lowered.contains("until its done")
        || lowered.contains("infinite task")
        || lowered.contains("don't stop")
        || lowered.contains("dont stop")
}

fn text_has_wrong_terminal_clarification_signal(lowered: &str) -> bool {
    lowered.contains("wrong terminal")
        || lowered.contains("typed this in the wrong")
        || lowered.contains("context slip")
        || (lowered.contains("clarifying question")
            && (lowered.contains("before editing")
                || lowered.contains("before touching")
                || lowered.contains("before acting")
                || lowered.contains("ask")
                || lowered.contains("confirm")))
        || (lowered.contains("ask")
            && lowered.contains("confirm")
            && (lowered.contains("revers")
                || lowered.contains("wrong terminal")
                || lowered.contains("mistake")))
}

fn text_has_autonomy_correction(lowered: &str) -> bool {
    if lowered.contains("agents.md instructions")
        || lowered.contains("autonomy directive")
        || lowered.contains("codex global contract")
        || lowered.contains("you are an autonomous coding agent")
    {
        return false;
    }

    lowered.contains("manual polling")
        || lowered.contains("keep polling")
        || lowered.contains("do not stop")
        || lowered.contains("don't stop")
        || lowered.contains("dont stop")
        || lowered.contains("should i proceed")
        || lowered.contains("keep going until")
        || lowered.contains("until it's done")
        || lowered.contains("until its done")
        || lowered.contains("infinite task")
}

pub(super) fn autonomy_polling_friction_status(
    latest_correction_at: Option<DateTime<Utc>>,
    durable_fix: Option<&DurableFrictionFixEvidence>,
    now: DateTime<Utc>,
) -> String {
    durable_friction_status(latest_correction_at, durable_fix, now)
}

pub(super) fn command_noise_friction_status(
    latest_correction_at: Option<DateTime<Utc>>,
    durable_fix: Option<&DurableFrictionFixEvidence>,
    now: DateTime<Utc>,
) -> String {
    durable_friction_status(latest_correction_at, durable_fix, now)
}

fn durable_friction_status(
    latest_correction_at: Option<DateTime<Utc>>,
    durable_fix: Option<&DurableFrictionFixEvidence>,
    now: DateTime<Utc>,
) -> String {
    let Some(durable_fix) = durable_fix else {
        return "active".to_string();
    };

    if latest_correction_at.is_some_and(|latest| latest > durable_fix.codified_at) {
        return "active".to_string();
    }

    let clean_age = now - durable_fix.codified_at;
    if clean_age >= Duration::days(90) {
        "retired".to_string()
    } else if clean_age >= Duration::days(45) {
        "fixed".to_string()
    } else {
        "codified".to_string()
    }
}

fn current_binary_durable_fix_evidence(policy: &str) -> DurableFrictionFixEvidence {
    let path = std::env::current_exe()
        .ok()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "compiled munin binary".to_string());
    let codified_at = std::env::current_exe()
        .ok()
        .and_then(|path| std::fs::metadata(path).ok())
        .and_then(|metadata| metadata.modified().ok())
        .map(DateTime::<Utc>::from)
        .unwrap_or_else(Utc::now);

    DurableFrictionFixEvidence {
        path: format!("{path} ({policy})"),
        codified_at,
    }
}

fn find_durable_autonomy_polling_instruction(
    project_path: Option<&str>,
) -> Option<DurableFrictionFixEvidence> {
    let start = PathBuf::from(resolved_project_path(project_path));
    find_nearest_agents_file(&start).and_then(durable_autonomy_polling_instruction_at)
}

fn find_codex_durable_autonomy_polling_instruction(
    project_path: Option<&str>,
) -> Option<DurableFrictionFixEvidence> {
    find_codex_durable_autonomy_polling_instruction_with_global_candidates(
        project_path,
        global_codex_agents_candidates(),
    )
}

fn find_codex_durable_autonomy_polling_instruction_with_global_candidates(
    project_path: Option<&str>,
    global_agents_candidates: Vec<PathBuf>,
) -> Option<DurableFrictionFixEvidence> {
    find_durable_autonomy_polling_instruction(project_path).or_else(|| {
        global_agents_candidates
            .into_iter()
            .find_map(durable_autonomy_polling_instruction_at)
    })
}

fn find_context_reversal_clarification_instruction(
    project_path: Option<&str>,
) -> Option<DurableFrictionFixEvidence> {
    find_context_reversal_clarification_instruction_with_global_candidates(
        project_path,
        global_context_reversal_instruction_candidates(),
    )
}

fn find_context_reversal_clarification_instruction_with_global_candidates(
    project_path: Option<&str>,
    global_instruction_candidates: Vec<PathBuf>,
) -> Option<DurableFrictionFixEvidence> {
    let start = PathBuf::from(resolved_project_path(project_path));
    find_nearest_agents_file(&start)
        .and_then(durable_context_reversal_clarification_instruction_at)
        .or_else(|| {
            global_instruction_candidates
                .into_iter()
                .find_map(durable_context_reversal_clarification_instruction_at)
        })
}

fn durable_autonomy_polling_instruction_at(
    agents_path: PathBuf,
) -> Option<DurableFrictionFixEvidence> {
    let contents = std::fs::read_to_string(&agents_path).ok()?;
    if !agents_file_codifies_autonomy_polling(&contents) {
        return None;
    }
    let codified_at = std::fs::metadata(&agents_path)
        .ok()
        .and_then(|metadata| metadata.modified().ok())
        .map(DateTime::<Utc>::from)
        .unwrap_or_else(Utc::now);

    Some(DurableFrictionFixEvidence {
        path: agents_path.display().to_string(),
        codified_at,
    })
}

fn durable_context_reversal_clarification_instruction_at(
    instruction_path: PathBuf,
) -> Option<DurableFrictionFixEvidence> {
    let contents = std::fs::read_to_string(&instruction_path).ok()?;
    if !instructions_file_codifies_context_reversal_clarification(&contents) {
        return None;
    }
    let codified_at = instruction_context_reversal_codified_at(&contents)
        .or_else(|| {
            std::fs::metadata(&instruction_path)
                .ok()
                .and_then(|metadata| metadata.modified().ok())
                .map(DateTime::<Utc>::from)
        })
        .unwrap_or_else(Utc::now);

    Some(DurableFrictionFixEvidence {
        path: instruction_path.display().to_string(),
        codified_at,
    })
}

fn global_context_reversal_instruction_candidates() -> Vec<PathBuf> {
    let mut candidates = global_codex_agents_candidates();
    if let Some(home) = dirs::home_dir() {
        candidates.push(home.join(".claude").join("CLAUDE.md"));
        candidates.push(home.join("CLAUDE.md"));
        candidates.push(home.join("AGENTS.md"));
    }
    candidates
}

fn global_codex_agents_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(codex_home) = std::env::var("CODEX_HOME") {
        if let Some(candidate) = codex_home_agents_candidate(&codex_home) {
            candidates.push(candidate);
        }
    }
    if let Some(home) = dirs::home_dir() {
        candidates.push(home.join(".codex").join("AGENTS.md"));
    }
    candidates
}

fn codex_home_agents_candidate(codex_home: &str) -> Option<PathBuf> {
    let trimmed = codex_home.trim();
    if trimmed.is_empty() {
        return None;
    }
    let path = PathBuf::from(trimmed);
    if !path.is_absolute() {
        return None;
    }
    Some(path.join("AGENTS.md"))
}

fn find_nearest_agents_file(start: &Path) -> Option<PathBuf> {
    let mut cursor = if start.is_file() {
        start.parent()?.to_path_buf()
    } else {
        start.to_path_buf()
    };

    loop {
        let candidate = cursor.join("AGENTS.md");
        if candidate.is_file() {
            return Some(candidate);
        }
        if !cursor.pop() {
            return None;
        }
    }
}

fn agents_file_codifies_autonomy_polling(contents: &str) -> bool {
    let lowered = contents.to_ascii_lowercase();
    let autonomy_contract =
        lowered.contains("autonomy directive") || lowered.contains("autonomous coding agent");
    let no_manual_polling =
        lowered.contains("do not stop to ask") || lowered.contains("should i proceed?");
    let completion_loop = lowered.contains("execute tasks to completion")
        || lowered.contains("continue iterating")
        || lowered.contains("without asking for permission");

    autonomy_contract && no_manual_polling && completion_loop
}

fn instructions_file_codifies_context_reversal_clarification(contents: &str) -> bool {
    let lowered = contents.to_ascii_lowercase();
    let context_slip = lowered.contains("wrong terminal")
        || lowered.contains("context slip")
        || lowered.contains("another terminal")
        || lowered.contains("reverses current task")
        || lowered.contains("reversing direction");
    let asks_before_acting = lowered.contains("clarifying question")
        || (lowered.contains("ask") && lowered.contains("confirm"));
    let before_editing = lowered.contains("before editing")
        || lowered.contains("before acting")
        || lowered.contains("before touching")
        || lowered.contains("before changing");

    context_slip && asks_before_acting && before_editing
}

fn instruction_context_reversal_codified_at(contents: &str) -> Option<DateTime<Utc>> {
    marker_codified_at(contents, CONTEXT_REVERSAL_FRICTION_MARKER)
}

fn marker_codified_at(contents: &str, marker: &str) -> Option<DateTime<Utc>> {
    let marker = marker.to_ascii_lowercase();
    contents
        .lines()
        .filter(|line| line.to_ascii_lowercase().contains(&marker))
        .find_map(|line| line.split_whitespace().find_map(parse_codified_at_token))
}

fn parse_codified_at_token(token: &str) -> Option<DateTime<Utc>> {
    let trimmed = token.trim_matches(|ch: char| {
        ch == ','
            || ch == ';'
            || ch == ')'
            || ch == ']'
            || ch == '>'
            || ch == '-'
            || ch == '"'
            || ch == '\''
    });
    if !trimmed.to_ascii_lowercase().starts_with("codified_at=") {
        return None;
    }
    DateTime::parse_from_rfc3339(&trimmed["codified_at=".len()..])
        .ok()
        .map(|parsed| parsed.with_timezone(&Utc))
}

fn command_friction_fixes(
    correction_patterns: &[crate::core::memory_os::MemoryOsCorrectionPatternSummary],
    likely_misunderstandings: &[crate::core::memory_os::MemoryOsMisunderstandingPattern],
) -> Vec<crate::core::memory_os::MemoryOsFrictionFix> {
    let mut fixes = Vec::new();
    for misunderstanding in likely_misunderstandings {
        let related = correction_patterns
            .iter()
            .filter(|pattern| {
                classify_misunderstanding_label(&pattern.error_kind, &pattern.wrong_command)
                    == misunderstanding.label
            })
            .collect::<Vec<_>>();
        let successful = related
            .iter()
            .map(|pattern| pattern.successful_replays)
            .sum::<usize>();
        let failed = related
            .iter()
            .map(|pattern| pattern.failed_replays)
            .sum::<usize>();
        let count = related.iter().map(|pattern| pattern.count).sum::<usize>();
        let status = friction_fix_status(count, successful, failed);
        let (impact, permanent_fix) = match misunderstanding.label.as_str() {
            "CLI syntax drift" => (
                "medium",
                "Use exact known command templates or tool help before running abbreviated commands.",
            ),
            "Path assumption drift" => (
                "medium",
                "Resolve the project root and exact script/test path before running commands.",
            ),
            "Tool availability drift" => (
                "medium",
                "Probe tool availability with --help/version before relying on a command.",
            ),
            _ => (
                "low",
                "Run a cheap probe before assuming the execution path is valid.",
            ),
        };
        fixes.push(crate::core::memory_os::MemoryOsFrictionFix {
            fix_id: format!(
                "friction:{}",
                misunderstanding
                    .label
                    .to_ascii_lowercase()
                    .replace(' ', "-")
            ),
            title: misunderstanding.label.clone(),
            impact: impact.to_string(),
            status,
            summary: format!(
                "{} correction memories; {} successful corrected replays.",
                count, successful
            ),
            permanent_fix: permanent_fix.to_string(),
            evidence: vec![format!("{} examples retained in JSON evidence", count)],
            score: 50 + count as i64 + successful as i64 - failed as i64,
        });
    }
    fixes
}

fn behavior_change_friction_fixes(
    behavior_changes: &[crate::core::memory_os::MemoryOsBehaviorChangeRecommendation],
    redirects: &crate::core::memory_os::MemoryOsRedirectSummary,
    durable_fixes: &UserProseDurableFixes,
    latest_autonomy_at: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> Vec<crate::core::memory_os::MemoryOsFrictionFix> {
    let mut fixes = Vec::new();
    if redirects.redirects > 0 {
        fixes.push(crate::core::memory_os::MemoryOsFrictionFix {
            fix_id: "friction:course-correction-follow-through".to_string(),
            title: "Follow through after user redirects".to_string(),
            impact: "medium".to_string(),
            status: if redirects.redirects_with_success_after_resume >= redirects.redirected_sessions
                && redirects.redirected_sessions > 0
            {
                "improving".to_string()
            } else {
                "active".to_string()
            },
            summary: format!(
                "{} recommendation shifts across {} workstreams; {} later succeeded.",
                redirects.redirects,
                redirects.redirected_sessions,
                redirects.redirects_with_success_after_resume
            ),
            permanent_fix:
                "After a redirect, treat the newest recommendation as authoritative and verify success before widening scope."
                    .to_string(),
            evidence: vec![
                format!("{} redirect-like shifts", redirects.redirects),
                format!(
                    "{} successful follow-throughs",
                    redirects.redirects_with_success_after_resume
                ),
            ],
            score: 70 + redirects.redirects as i64,
        });
    }
    for change in behavior_changes {
        if change.change.contains("Use `munin memory-os")
            || change.change.contains("Memory OS-first")
            || change
                .change
                .contains("front-load the scoped Memory OS profile")
            || change.change.contains("open recall only when")
        {
            continue;
        }
        let (status, durable_fix) =
            behavior_change_friction_status(change, durable_fixes, latest_autonomy_at, now);
        if status == "retired" {
            continue;
        }
        let mut evidence = change.evidence.iter().take(3).cloned().collect::<Vec<_>>();
        if let Some(durable) = durable_fix {
            evidence.push(format!("durable instruction codified in {}", durable.path));
            if latest_autonomy_at.is_some_and(|latest| latest > durable.codified_at) {
                evidence.push("newer autonomy correction exists after codification".to_string());
            }
        }
        fixes.push(crate::core::memory_os::MemoryOsFrictionFix {
            fix_id: format!("friction:behavior:{}", change.target_agent),
            title: format!("Behavior change for {}", change.target_agent),
            impact: "medium".to_string(),
            status,
            summary: change.rationale.clone(),
            permanent_fix: change.change.clone(),
            evidence,
            score: 60,
        });
    }
    fixes
}

fn behavior_change_friction_status<'a>(
    change: &crate::core::memory_os::MemoryOsBehaviorChangeRecommendation,
    durable_fixes: &'a UserProseDurableFixes,
    latest_autonomy_at: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> (String, Option<&'a DurableFrictionFixEvidence>) {
    let durable_fix =
        if change.target_agent == "codex" && is_autonomy_polling_behavior_change(change) {
            durable_fixes.codex_autonomy_polling.as_ref()
        } else {
            None
        };
    let status = durable_fix
        .map(|durable| autonomy_polling_friction_status(latest_autonomy_at, Some(durable), now))
        .unwrap_or_else(|| "active".to_string());
    (status, durable_fix)
}

fn is_autonomy_polling_behavior_change(
    change: &crate::core::memory_os::MemoryOsBehaviorChangeRecommendation,
) -> bool {
    let lowered = change.change.to_ascii_lowercase();
    lowered.contains("polling")
        || lowered.contains("iterating until")
        || lowered.contains("concrete blocker")
}

fn friction_fix_status(count: usize, successful: usize, failed: usize) -> String {
    if count > 0 && failed == 0 && successful >= count {
        "fixed".to_string()
    } else if successful > 0 && failed == 0 {
        "improving".to_string()
    } else {
        "active".to_string()
    }
}

fn friction_status_rank(status: &str) -> i32 {
    match status {
        "active" => 5,
        "improving" => 4,
        "codified" => 3,
        "fixed" => 2,
        "retired" => 1,
        _ => 0,
    }
}

fn friction_impact_rank(impact: &str) -> i32 {
    match impact {
        "high" => 3,
        "medium" => 2,
        "low" => 1,
        _ => 0,
    }
}

pub(super) fn build_memory_os_behavior_changes(
    by_source: &[crate::core::memory_os::MemoryOsSourceBehaviorSummary],
    redirects: &crate::core::memory_os::MemoryOsRedirectSummary,
    autonomy_count: usize,
    autonomy_friction_status: Option<&str>,
    codex_autonomy_friction_status: Option<&str>,
) -> Vec<crate::core::memory_os::MemoryOsBehaviorChangeRecommendation> {
    let mut recommendations = Vec::new();

    let autonomy_rule_needed =
        autonomy_count > 0 && !friction_status_is_durable_terminal(autonomy_friction_status);
    let codex_autonomy_rule_needed = autonomy_rule_needed
        && !friction_status_is_durable_terminal(codex_autonomy_friction_status);

    if codex_autonomy_rule_needed || autonomy_rule_needed {
        let autonomy_rationale = format!(
            "User has asked for stronger autonomous polling/approval behavior {autonomy_count} times; treat any 'poll', 'keep going', 'until done', or long-running instruction as an infinite-loop contract.",
        );
        if codex_autonomy_rule_needed {
            recommendations.push(crate::core::memory_os::MemoryOsBehaviorChangeRecommendation {
                target_agent: "codex".to_string(),
                change: "When a task calls for polling, waiting, or iterating until something is solved, keep cycling without pausing to ask \"should I continue?\". Stop only when the task is verified solved or a concrete blocker is recorded.".to_string(),
                rationale: autonomy_rationale.clone(),
                evidence: vec![
                    format!("{autonomy_count} autonomy/polling corrections"),
                    "Codex is the bigger offender for mid-loop pauses".to_string(),
                ],
            });
        }
        if autonomy_rule_needed {
            recommendations.push(crate::core::memory_os::MemoryOsBehaviorChangeRecommendation {
                target_agent: "claude".to_string(),
                change: "When the user asks for polling or long-running work, keep iterating until the task is solved or a concrete blocker is recorded. Do not return to the prompt between cycles or summarise progress in place of continuing.".to_string(),
                rationale: autonomy_rationale,
                evidence: vec![
                    format!("{autonomy_count} autonomy/polling corrections"),
                    "Shared contract with codex so both lanes behave the same under polling instructions".to_string(),
                ],
            });
        }
    }

    recommendations.push(
        crate::core::memory_os::MemoryOsBehaviorChangeRecommendation {
            target_agent: "codex".to_string(),
                change: "Use `munin memory-os overview/profile/friction --scope user` before reading raw recall or session history for user/profile/current-work questions.".to_string(),
            rationale: "Codex needs a deterministic Memory OS-first read path so fresh sessions stop trawling docs and archives for questions the compiled state can already answer.".to_string(),
            evidence: memory_os_serving_policy_lines(),
        },
    );

    if let Some(codex) = by_source
        .iter()
        .find(|source| source.source == "codex" && source.corrections > 0)
    {
        recommendations.push(
            crate::core::memory_os::MemoryOsBehaviorChangeRecommendation {
                target_agent: "codex".to_string(),
                change: "Keep moving after grounding, but front-load the scoped Memory OS profile so command corrections and active-work cues are visible before acting.".to_string(),
                rationale: "The Codex lane has correction memory, so front-load the Memory OS profile before acting.".to_string(),
                evidence: vec![
                    format!("{} codex correction memories", codex.corrections),
                    format!("{} imported codex sessions", codex.sessions),
                ],
            },
        );
    }

    if let Some(claude) = by_source.iter().find(|source| source.source == "claude") {
        recommendations.push(
            crate::core::memory_os::MemoryOsBehaviorChangeRecommendation {
                target_agent: "claude".to_string(),
                change: "Use the same Memory OS-first read path, then open recall only when a specific historical example is needed for provenance.".to_string(),
                rationale: "Claude already carries most of the imported historical footprint, so it benefits from a compact projection-first answer path instead of broad archive scans.".to_string(),
                evidence: vec![
                    format!("{} imported claude sessions", claude.sessions),
                ],
            },
        );
    }

    if redirects.redirects > 0 {
        recommendations.push(
            crate::core::memory_os::MemoryOsBehaviorChangeRecommendation {
                target_agent: "both".to_string(),
                change: "When the active recommendation changes across checkpoints, treat that as the new current-work answer and verify against the newest successful execution before widening scope.".to_string(),
                rationale: "Checkpoint recommendation shifts are the best compiled proxy for course corrections in the current Memory OS substrate.".to_string(),
                evidence: vec![
                    format!("redirect-like recommendation shifts: {}", redirects.redirects),
                    format!(
                        "success after shift: {}",
                        redirects.redirects_with_success_after_resume
                    ),
                ],
            },
        );
    }

    recommendations
}

fn friction_status_is_durable_terminal(status: Option<&str>) -> bool {
    matches!(status, Some("codified" | "fixed" | "retired"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn onboarding_checkpoint(
        generated_at: &str,
        committed_at: &str,
        goal: &str,
    ) -> MemoryOsCheckpointEnvelope {
        MemoryOsCheckpointEnvelope {
            project_path: "C:/repo".to_string(),
            captured_at: DateTime::parse_from_rfc3339(committed_at)
                .expect("committed timestamp")
                .with_timezone(&Utc),
            capture: crate::core::memory_os::MemoryOsCheckpointCapture {
                packet_id: "packet".to_string(),
                generated_at: generated_at.to_string(),
                preset: "resume".to_string(),
                intent: "continue".to_string(),
                profile: "session-onboarding".to_string(),
                goal: Some(goal.to_string()),
                budget: 1600,
                estimated_tokens: 0,
                estimated_source_tokens: 0,
                pager_manifest_hash: "manifest".to_string(),
                recall_mode: "off".to_string(),
                recall_used: false,
                recall_reason: "session-onboarding".to_string(),
                telemetry: crate::core::memory_os::MemoryOsCheckpointTelemetry {
                    current_fact_count: 0,
                    recent_change_count: 0,
                    live_claim_count: 0,
                    open_obligation_count: 0,
                    artifact_handle_count: 0,
                    failure_count: 0,
                },
                selected_items: Vec::new(),
                exclusions: Vec::new(),
                reentry: crate::core::memory_os::MemoryOsCheckpointReentry {
                    recommended_command: "munin resume --format prompt".to_string(),
                    current_recommendation: None,
                    first_question: "What still matters?".to_string(),
                    first_verification: "Verify the next step.".to_string(),
                },
            },
        }
    }

    #[test]
    fn behavior_changes_emit_polling_rules_for_both_agents_when_autonomy_signal_present() {
        let by_source = Vec::new();
        let redirects = crate::core::memory_os::MemoryOsRedirectSummary::default();

        let without_signal =
            build_memory_os_behavior_changes(&by_source, &redirects, 0, None, None);
        assert!(
            !without_signal
                .iter()
                .any(|rec| rec.change.contains("polling")),
            "no polling rule should appear when autonomy_count is 0"
        );

        let with_signal = build_memory_os_behavior_changes(&by_source, &redirects, 154, None, None);
        let codex_rule = with_signal
            .iter()
            .find(|rec| rec.target_agent == "codex" && rec.change.contains("polling"))
            .expect("codex polling rule expected");
        let claude_rule = with_signal
            .iter()
            .find(|rec| rec.target_agent == "claude" && rec.change.contains("polling"))
            .expect("claude polling rule expected");
        assert!(codex_rule.rationale.contains("154"));
        assert!(claude_rule.rationale.contains("154"));
        assert!(codex_rule
            .change
            .contains("verified solved or a concrete blocker"));
        assert!(claude_rule.change.contains("concrete blocker"));
    }

    #[test]
    fn behavior_changes_do_not_repeat_codified_polling_rules() {
        let by_source = Vec::new();
        let redirects = crate::core::memory_os::MemoryOsRedirectSummary::default();

        let codified =
            build_memory_os_behavior_changes(&by_source, &redirects, 154, Some("codified"), None);

        assert!(
            !codified.iter().any(|rec| rec.change.contains("polling")),
            "durably codified polling rules should stop surfacing as behavior changes"
        );
    }

    #[test]
    fn behavior_changes_respect_codex_specific_polling_status() {
        let by_source = Vec::new();
        let redirects = crate::core::memory_os::MemoryOsRedirectSummary::default();

        let recommendations = build_memory_os_behavior_changes(
            &by_source,
            &redirects,
            154,
            Some("active"),
            Some("codified"),
        );

        assert!(
            !recommendations
                .iter()
                .any(|rec| rec.target_agent == "codex" && rec.change.contains("polling")),
            "codex-global durable evidence should suppress the codex polling behavior rule"
        );
        assert!(
            recommendations
                .iter()
                .any(|rec| rec.target_agent == "claude" && rec.change.contains("long-running")),
            "codex-global durable evidence must not suppress claude behavior rules"
        );
    }

    #[test]
    fn behavior_changes_keep_shell_metrics_out_of_human_evidence() {
        let by_source = vec![
            crate::core::memory_os::MemoryOsSourceBehaviorSummary {
                source: "codex".to_string(),
                sessions: 10,
                shell_executions: 120,
                corrections: 2,
                redirects: 0,
                redirected_sessions: 0,
                successful_redirects: 0,
                shells_per_session: 12.0,
                corrections_per_100_shells: 1.7,
                redirects_per_session: 0.0,
                avg_commands_to_success_after_redirect: None,
                avg_seconds_to_success_after_redirect: None,
            },
            crate::core::memory_os::MemoryOsSourceBehaviorSummary {
                source: "claude".to_string(),
                sessions: 8,
                shell_executions: 240,
                corrections: 0,
                redirects: 0,
                redirected_sessions: 0,
                successful_redirects: 0,
                shells_per_session: 30.0,
                corrections_per_100_shells: 0.0,
                redirects_per_session: 0.0,
                avg_commands_to_success_after_redirect: None,
                avg_seconds_to_success_after_redirect: None,
            },
        ];
        let redirects = crate::core::memory_os::MemoryOsRedirectSummary::default();

        let changes = build_memory_os_behavior_changes(&by_source, &redirects, 0, None, None);
        let rendered = changes
            .iter()
            .flat_map(|change| change.evidence.iter())
            .cloned()
            .collect::<Vec<_>>()
            .join("\n");

        assert!(!rendered.contains("shell"));
        assert!(rendered.contains("2 codex correction memories"));
        assert!(rendered.contains("8 imported claude sessions"));
    }

    fn completed_friction_record(
        item_id: &str,
        evidence: &[String],
    ) -> crate::core::tracking::ApprovalJobRecord {
        let timestamp = DateTime::parse_from_rfc3339("2026-05-02T00:00:00Z")
            .expect("timestamp")
            .with_timezone(&Utc);
        crate::core::tracking::ApprovalJobRecord {
            job_id: format!("approval-test-{item_id}"),
            created_at: timestamp,
            updated_at: timestamp,
            project_path: "C:/project".to_string(),
            scope: "project".to_string(),
            scope_target: Some("C:/project".to_string()),
            local_date: "2026-05-02".to_string(),
            item_id: Some(item_id.to_string()),
            item_kind: "friction-fix".to_string(),
            title: format!("Fix {item_id}"),
            summary: "completed".to_string(),
            status: crate::core::tracking::ApprovalJobStatus::Completed,
            source_kind: "strategy-nudge".to_string(),
            provider: Some("codex".to_string()),
            continuity_active: false,
            expected_effect: None,
            queue_path: None,
            result_path: None,
            evidence_json: serde_json::to_string(evidence).expect("evidence json"),
            review_after: None,
            expires_at: None,
            last_reviewed_at: None,
            closure_reason: Some("done".to_string()),
        }
    }

    #[test]
    fn completed_friction_statuses_fix_unchanged_evidence_only() {
        let completed_evidence = vec!["99 autonomy/polling corrections".to_string()];
        let completed = vec![completed_friction_record(
            "friction:autonomy-polling",
            &completed_evidence,
        )];
        let mut fixes = vec![
            crate::core::memory_os::MemoryOsFrictionFix {
                fix_id: "friction:autonomy-polling".to_string(),
                title: "Keep autonomous work moving without manual polling".to_string(),
                impact: "high".to_string(),
                status: "active".to_string(),
                summary: "old signal".to_string(),
                permanent_fix: "poll".to_string(),
                evidence: completed_evidence,
                score: 120,
            },
            crate::core::memory_os::MemoryOsFrictionFix {
                fix_id: "friction:behavior:claude".to_string(),
                title: "Behavior change for claude".to_string(),
                impact: "medium".to_string(),
                status: "active".to_string(),
                summary: "new signal".to_string(),
                permanent_fix: "poll".to_string(),
                evidence: vec![
                    "99 autonomy/polling corrections".to_string(),
                    "newer correction after completion".to_string(),
                ],
                score: 60,
            },
        ];

        apply_completed_friction_statuses(&mut fixes, &completed);

        assert_eq!(fixes[0].status, "fixed");
        assert_eq!(fixes[1].status, "active");
    }

    #[test]
    fn completed_behavior_changes_are_filtered_by_exact_evidence() {
        let completed_evidence = vec![
            "99 autonomy/polling corrections".to_string(),
            "Shared contract with codex so both lanes behave the same under polling instructions"
                .to_string(),
        ];
        let completed = vec![completed_friction_record(
            "friction:behavior:claude",
            &completed_evidence,
        )];
        let changes = vec![
            crate::core::memory_os::MemoryOsBehaviorChangeRecommendation {
                target_agent: "claude".to_string(),
                change: "When the user asks for polling or long-running work, keep iterating."
                    .to_string(),
                rationale: "old signal".to_string(),
                evidence: completed_evidence,
            },
            crate::core::memory_os::MemoryOsBehaviorChangeRecommendation {
                target_agent: "codex".to_string(),
                change: "Use Memory OS first.".to_string(),
                rationale: "different behavior".to_string(),
                evidence: vec!["policy".to_string()],
            },
        ];

        let filtered = filter_completed_behavior_changes(changes, &completed);

        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].target_agent, "codex");
    }

    #[test]
    fn codex_global_agents_codifies_only_codex_behavior_fix() {
        let codified_at = DateTime::parse_from_rfc3339("2026-05-01T00:00:00Z")
            .expect("timestamp")
            .with_timezone(&Utc);
        let durable_fixes = UserProseDurableFixes {
            autonomy_polling: None,
            codex_autonomy_polling: Some(DurableFrictionFixEvidence {
                path: "C:/Users/OEM/.codex/AGENTS.md".to_string(),
                codified_at,
            }),
            command_noise_surface_policy: None,
            context_reversal_clarification: None,
        };
        let behavior_changes = vec![
            crate::core::memory_os::MemoryOsBehaviorChangeRecommendation {
                target_agent: "codex".to_string(),
                change: "When a task calls for polling, keep cycling until verified solved."
                    .to_string(),
                rationale: "codex rationale".to_string(),
                evidence: vec!["99 autonomy/polling corrections".to_string()],
            },
            crate::core::memory_os::MemoryOsBehaviorChangeRecommendation {
                target_agent: "claude".to_string(),
                change: "When a task calls for polling, keep cycling until verified solved."
                    .to_string(),
                rationale: "claude rationale".to_string(),
                evidence: vec!["99 autonomy/polling corrections".to_string()],
            },
        ];

        let fixes = behavior_change_friction_fixes(
            &behavior_changes,
            &crate::core::memory_os::MemoryOsRedirectSummary::default(),
            &durable_fixes,
            Some(codified_at - Duration::days(1)),
            codified_at + Duration::days(1),
        );

        let codex = fixes
            .iter()
            .find(|fix| fix.fix_id == "friction:behavior:codex")
            .expect("codex fix");
        let claude = fixes
            .iter()
            .find(|fix| fix.fix_id == "friction:behavior:claude")
            .expect("claude fix");
        assert_eq!(codex.status, "codified");
        assert!(codex
            .evidence
            .iter()
            .any(|item| item.contains("durable instruction codified")));
        assert_eq!(claude.status, "active");
    }

    #[test]
    fn autonomy_polling_status_tracks_durable_fix_lifecycle() {
        let codified_at = DateTime::parse_from_rfc3339("2026-04-01T00:00:00Z")
            .expect("timestamp")
            .with_timezone(&Utc);
        let durable = DurableFrictionFixEvidence {
            path: "C:/Users/OEM/Projects/AGENTS.md".to_string(),
            codified_at,
        };

        assert_eq!(
            autonomy_polling_friction_status(
                Some(codified_at - Duration::days(1)),
                Some(&durable),
                codified_at + Duration::days(14),
            ),
            "codified"
        );
        assert_eq!(
            autonomy_polling_friction_status(
                Some(codified_at - Duration::days(1)),
                Some(&durable),
                codified_at + Duration::days(45),
            ),
            "fixed"
        );
        assert_eq!(
            autonomy_polling_friction_status(
                Some(codified_at - Duration::days(1)),
                Some(&durable),
                codified_at + Duration::days(90),
            ),
            "retired"
        );
        assert_eq!(
            autonomy_polling_friction_status(
                Some(codified_at + Duration::seconds(1)),
                Some(&durable),
                codified_at + Duration::days(90),
            ),
            "active"
        );
    }

    #[test]
    fn command_noise_status_tracks_durable_fix_lifecycle() {
        let codified_at = DateTime::parse_from_rfc3339("2026-04-01T00:00:00Z")
            .expect("timestamp")
            .with_timezone(&Utc);
        let durable = DurableFrictionFixEvidence {
            path: "munin binary".to_string(),
            codified_at,
        };

        assert_eq!(
            command_noise_friction_status(
                Some(codified_at - Duration::days(1)),
                Some(&durable),
                codified_at + Duration::days(14),
            ),
            "codified"
        );
        assert_eq!(
            command_noise_friction_status(
                Some(codified_at + Duration::seconds(1)),
                Some(&durable),
                codified_at + Duration::days(14),
            ),
            "active"
        );
    }

    #[test]
    fn checkpoint_summary_filters_command_and_build_noise() {
        assert!(meaningful_checkpoint_summary("cargo build: 0 errors, 18 warnings").is_none());
        assert!(meaningful_checkpoint_summary(
            "C:\\Users\\OEM\\Projects\\context | branch main | staged 0 | modified 0"
        )
        .is_none());
        assert_eq!(
            meaningful_checkpoint_summary(
                "Fix the Memory OS brief active-work section before trusting the surface."
            ),
            Some(
                "Fix the Memory OS brief active-work section before trusting the surface."
                    .to_string()
            )
        );
    }

    #[test]
    fn agents_file_codification_requires_autonomy_and_completion_contract() {
        assert!(agents_file_codifies_autonomy_polling(
            "AUTONOMY DIRECTIVE\nYOU ARE AN AUTONOMOUS CODING AGENT.\nEXECUTE TASKS TO COMPLETION WITHOUT ASKING FOR PERMISSION.\nDO NOT STOP TO ASK \"SHOULD I PROCEED?\""
        ));
        assert!(!agents_file_codifies_autonomy_polling(
            "You are autonomous, but ask before proceeding."
        ));
    }

    #[test]
    fn context_reversal_detector_requires_context_slip_and_clarifying_guard() {
        assert!(instructions_file_codifies_context_reversal_clarification(
            "When a user message reverses current task framing or looks like it may belong to another terminal, ask one concise clarifying question before editing."
        ));
        assert!(!instructions_file_codifies_context_reversal_clarification(
            "Ask clarifying questions when needed."
        ));
        assert!(!instructions_file_codifies_context_reversal_clarification(
            "Watch for wrong terminal messages but keep editing."
        ));
    }

    #[test]
    fn context_reversal_marker_supplies_stable_codified_at() {
        let codified_at = instruction_context_reversal_codified_at(
            "<!-- munin-friction:context-reversal codified_at=2026-05-08T00:00:28Z -->",
        )
        .expect("codified marker");

        assert_eq!(
            codified_at,
            DateTime::parse_from_rfc3339("2026-05-08T00:00:28Z")
                .expect("timestamp")
                .with_timezone(&Utc)
        );
    }

    #[test]
    fn context_reversal_marker_ignores_malformed_codified_at() {
        assert!(instruction_context_reversal_codified_at(
            "<!-- munin-friction:context-reversal codified_at=not-a-timestamp -->",
        )
        .is_none());
    }

    #[test]
    fn new_context_reversal_friction_disappears_when_durably_codified() {
        let correction_at = DateTime::parse_from_rfc3339("2026-04-20T19:03:36Z")
            .expect("timestamp")
            .with_timezone(&Utc);
        let durable_fixes = UserProseDurableFixes {
            autonomy_polling: None,
            codex_autonomy_polling: None,
            command_noise_surface_policy: None,
            context_reversal_clarification: Some(DurableFrictionFixEvidence {
                path: "C:/Users/OEM/.codex/AGENTS.md".to_string(),
                codified_at: correction_at + Duration::days(1),
            }),
        };
        let checkpoints = vec![onboarding_checkpoint(
            "2026-04-20T19:03:36Z",
            "2026-04-20T19:03:40Z",
            "that was a mistake typed in the wrong terminal; ask a clarifying question before editing",
        )];

        let fixes = build_memory_os_new_unproven_friction(&checkpoints, &durable_fixes);

        assert!(
            fixes.is_empty(),
            "old context-slip friction should be hidden once codified"
        );
    }

    #[test]
    fn new_context_reversal_friction_reappears_after_newer_correction() {
        let durable_fixes = UserProseDurableFixes {
            autonomy_polling: None,
            codex_autonomy_polling: None,
            command_noise_surface_policy: None,
            context_reversal_clarification: Some(DurableFrictionFixEvidence {
                path: "C:/Users/OEM/.codex/AGENTS.md".to_string(),
                codified_at: DateTime::parse_from_rfc3339("2026-04-20T00:00:00Z")
                    .expect("timestamp")
                    .with_timezone(&Utc),
            }),
        };
        let checkpoints = vec![onboarding_checkpoint(
            "2026-04-21T19:03:36Z",
            "2026-04-21T19:03:40Z",
            "wrong terminal context slip; ask a clarifying question before editing",
        )];

        let fixes = build_memory_os_new_unproven_friction(&checkpoints, &durable_fixes);

        assert_eq!(fixes.len(), 1);
        assert_eq!(fixes[0].status, "monitoring");
    }

    #[test]
    fn new_context_reversal_friction_filters_only_pre_codification_evidence() {
        let durable_fixes = UserProseDurableFixes {
            autonomy_polling: None,
            codex_autonomy_polling: None,
            command_noise_surface_policy: None,
            context_reversal_clarification: Some(DurableFrictionFixEvidence {
                path: "C:/Users/OEM/.codex/AGENTS.md".to_string(),
                codified_at: DateTime::parse_from_rfc3339("2026-04-20T00:00:00Z")
                    .expect("timestamp")
                    .with_timezone(&Utc),
            }),
        };
        let checkpoints = vec![
            onboarding_checkpoint(
                "2026-04-19T19:03:36Z",
                "2026-04-19T19:03:40Z",
                "wrong terminal context slip; ask a clarifying question before editing",
            ),
            onboarding_checkpoint(
                "2026-04-21T19:03:36Z",
                "2026-04-21T19:03:40Z",
                "wrong terminal context slip; ask a clarifying question before editing",
            ),
        ];

        let fixes = build_memory_os_new_unproven_friction(&checkpoints, &durable_fixes);

        assert_eq!(fixes.len(), 1);
        assert!(fixes[0].summary.contains("1 time"));
        assert_eq!(fixes[0].evidence.len(), 1);
        assert!(fixes[0].evidence[0].contains("2026-04-21T19:03:36Z"));
    }

    #[test]
    fn durable_autonomy_polling_falls_back_to_codex_global_agents() {
        let temp = tempfile::tempdir().expect("tempdir");
        let project = temp.path().join("project").join("child");
        std::fs::create_dir_all(&project).expect("project dir");
        let codex_home = temp.path().join("codex-home");
        std::fs::create_dir_all(&codex_home).expect("codex home");
        let agents_path = codex_home.join("AGENTS.md");
        std::fs::write(
            &agents_path,
            "AUTONOMY DIRECTIVE\nYOU ARE AN AUTONOMOUS CODING AGENT.\nEXECUTE TASKS TO COMPLETION WITHOUT ASKING FOR PERMISSION.\nDO NOT STOP TO ASK \"SHOULD I PROCEED?\"",
        )
        .expect("write global agents");

        let durable = find_codex_durable_autonomy_polling_instruction_with_global_candidates(
            Some(project.to_string_lossy().as_ref()),
            vec![agents_path.clone()],
        )
        .expect("global Codex AGENTS should codify polling");

        assert_eq!(durable.path, agents_path.display().to_string());
    }

    #[test]
    fn codex_home_agents_candidate_requires_absolute_non_empty_path() {
        assert!(codex_home_agents_candidate("").is_none());
        assert!(codex_home_agents_candidate("relative/.codex").is_none());

        let temp = tempfile::tempdir().expect("tempdir");
        let candidate = codex_home_agents_candidate(temp.path().to_string_lossy().as_ref())
            .expect("absolute candidate");

        assert_eq!(candidate, temp.path().join("AGENTS.md"));
    }

    #[test]
    fn autonomy_meta_discussion_does_not_reset_codified_lifecycle() {
        let meta_discussion = "can we confirm how friction points are forgotten, like the new polling issues we added?";
        let direct_correction = "do not stop to ask should I proceed, keep going until done";

        assert!(text_has_autonomy_signal(meta_discussion));
        assert!(!text_has_autonomy_correction(meta_discussion));
        assert!(!text_has_autonomy_correction(
            "AGENTS.md instructions\nAUTONOMY DIRECTIVE\nDO NOT STOP TO ASK SHOULD I PROCEED"
        ));
        assert!(text_has_autonomy_signal(direct_correction));
        assert!(text_has_autonomy_correction(direct_correction));
    }

    #[test]
    fn autonomy_latest_correction_uses_original_session_time_not_import_time() {
        let checkpoints = vec![onboarding_checkpoint(
            "2026-04-01T00:00:00Z",
            "2026-04-18T00:00:00Z",
            "do not stop to ask should I proceed; keep going until done",
        )];

        let counts = count_user_prose_signals(&checkpoints);

        assert_eq!(
            counts.latest_autonomy_at.expect("latest correction"),
            DateTime::parse_from_rfc3339("2026-04-01T00:00:00Z")
                .expect("timestamp")
                .with_timezone(&Utc)
        );
    }

    #[test]
    fn new_unproven_friction_surfaces_single_wrong_terminal_clarification() {
        let checkpoints = vec![onboarding_checkpoint(
            "2026-04-21T18:03:06Z",
            "2026-04-21T18:03:10Z",
            "that was a mistake that munin should hopefully catch, and you should recognise the user has typed this in the wrong terminal; ask a clarifying question first to confirm before editing",
        )];

        let fixes =
            build_memory_os_new_unproven_friction(&checkpoints, &UserProseDurableFixes::default());

        assert_eq!(fixes.len(), 1);
        assert_eq!(fixes[0].impact, "high");
        assert_eq!(fixes[0].status, "monitoring");
        assert!(fixes[0].title.contains("Clarify before reversing"));
        assert!(fixes[0]
            .permanent_fix
            .contains("ask one concise clarifying question"));
        assert!(fixes[0].evidence[0].contains("2026-04-21T18:03:06Z"));
    }

    #[test]
    fn friction_fix_status_fades_successful_patterns() {
        assert_eq!(friction_fix_status(2, 2, 0), "fixed");
        assert_eq!(friction_fix_status(3, 1, 0), "improving");
        assert_eq!(friction_fix_status(3, 1, 1), "active");
    }

    #[test]
    fn command_friction_fixes_turn_raw_patterns_into_actions() {
        let patterns = vec![crate::core::memory_os::MemoryOsCorrectionPatternSummary {
            error_kind: "general-error".to_string(),
            wrong_command: "cd C:/repo && node script.js --bad".to_string(),
            corrected_command: "cd C:/repo && node script.js --help".to_string(),
            count: 2,
            successful_replays: 2,
            failed_replays: 0,
        }];
        let misunderstandings = build_memory_os_misunderstandings(&patterns);
        let fixes = command_friction_fixes(&patterns, &misunderstandings);

        assert_eq!(fixes.len(), 1);
        assert_eq!(fixes[0].status, "fixed");
        assert!(fixes[0].permanent_fix.contains("known command templates"));
        assert!(!fixes[0].summary.contains("node script.js"));
    }
}

fn execution_progress_after(
    executions: &[MemoryOsActionExecutionSummaryRow],
    project_path: &str,
    observed_after: DateTime<Utc>,
) -> Option<(usize, f64, bool)> {
    let mut commands = 0usize;
    for execution in executions.iter().filter(|execution| {
        execution.project_path == project_path && execution.observed_at >= observed_after
    }) {
        commands += 1;
        if execution.exit_code == 0 {
            let seconds =
                (execution.observed_at - observed_after).num_milliseconds() as f64 / 1000.0;
            return Some((commands, seconds, true));
        }
    }
    if commands > 0 {
        Some((commands, 0.0, false))
    } else {
        None
    }
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub(super) struct MemoryOsReplayShellRow {
    pub(super) timestamp: DateTime<Utc>,
    pub(super) project_path: String,
    pub(super) source: String,
    pub(super) session_id: String,
}

#[derive(Debug, Clone)]
pub(super) struct MemoryOsCorrectionObservationRow {
    pub(super) source: String,
    pub(super) project_path: String,
    pub(super) observed_at: DateTime<Utc>,
    pub(super) error_kind: String,
    pub(super) wrong_command: String,
    pub(super) corrected_command: String,
}

#[derive(Debug, Clone)]
struct MemoryOsActionExecutionSummaryRow {
    project_path: String,
    command_sig: String,
    exit_code: i32,
    observed_at: DateTime<Utc>,
}

#[derive(Debug, Default, Clone)]
pub(super) struct MemoryOsSourceBehaviorAccumulator {
    pub(super) source: String,
    pub(super) sessions: usize,
    pub(super) shell_executions: usize,
    pub(super) corrections: usize,
}

#[derive(Debug, Default, Clone)]
struct MemoryOsRedirectAccumulator {
    redirects: usize,
    redirected_sessions: usize,
    redirects_with_resumed_shell: usize,
    redirects_with_success_after_resume: usize,
    commands_to_success_sum: usize,
    seconds_to_success_sum: f64,
}

impl Tracker {
    pub(super) fn load_memory_os_replay_shells(
        &self,
        scope: crate::core::memory_os::MemoryOsInspectionScope,
        project_path: Option<&str>,
    ) -> Result<Vec<MemoryOsReplayShellRow>> {
        let (project_exact, project_glob, _) = memory_os_scope_params(scope, project_path);
        let mut stmt = self.conn.prepare(
            "SELECT timestamp, project_path, payload_json
             FROM worldview_events
             WHERE (?1 IS NULL OR project_path = ?1 OR project_path GLOB ?2)
             ORDER BY timestamp DESC, id DESC",
        )?;
        let rows = stmt
            .query_map(params![project_exact, project_glob], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        let mut shells = Vec::new();
        for (timestamp, project_path, payload_json) in rows {
            let Some((source, session_id)) = extract_replay_source(&payload_json) else {
                continue;
            };
            shells.push(MemoryOsReplayShellRow {
                timestamp: parse_rfc3339_to_utc(&timestamp),
                project_path,
                source,
                session_id,
            });
        }
        Ok(shells)
    }

    pub(super) fn load_memory_os_correction_observations(
        &self,
        scope: crate::core::memory_os::MemoryOsInspectionScope,
        project_path: Option<&str>,
    ) -> Result<Vec<MemoryOsCorrectionObservationRow>> {
        let (project_exact, project_glob, _) = memory_os_scope_params(scope, project_path);
        let mut stmt = self.conn.prepare(
            "SELECT project_path, source_ref, cue_json, action_json, observed_at
             FROM memory_os_action_observations
             WHERE source_kind = 'session-correction'
               AND (?1 IS NULL OR project_path = ?1 OR project_path GLOB ?2)
             ORDER BY observed_at DESC, observation_id DESC",
        )?;
        let rows = stmt
            .query_map(params![project_exact, project_glob], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        let mut corrections = Vec::new();
        for (project_path, source_ref, cue_json, action_json, observed_at) in rows {
            let Ok(cue) =
                serde_json::from_str::<crate::core::memory_os::MemoryOsActionCue>(&cue_json)
            else {
                continue;
            };
            let Ok(action) =
                serde_json::from_str::<crate::core::memory_os::MemoryOsAction>(&action_json)
            else {
                continue;
            };
            corrections.push(MemoryOsCorrectionObservationRow {
                source: correction_source_from_ref(&source_ref),
                project_path,
                observed_at: parse_rfc3339_to_utc(&observed_at),
                error_kind: cue.trigger_section.unwrap_or_else(|| "unknown".to_string()),
                wrong_command: cue.trigger_summary.unwrap_or_else(|| "unknown".to_string()),
                corrected_command: action.command_sig.unwrap_or_else(|| "unknown".to_string()),
            });
        }
        Ok(corrections)
    }

    fn load_memory_os_action_executions(
        &self,
        scope: crate::core::memory_os::MemoryOsInspectionScope,
        project_path: Option<&str>,
    ) -> Result<Vec<MemoryOsActionExecutionSummaryRow>> {
        let (project_exact, project_glob, _) = memory_os_scope_params(scope, project_path);
        let mut stmt = self.conn.prepare(
            "SELECT project_path, command_sig, exit_code, observed_at
             FROM memory_os_action_executions
             WHERE execution_kind = 'session-replay'
               AND (?1 IS NULL OR project_path = ?1 OR project_path GLOB ?2)
             ORDER BY observed_at ASC, execution_id ASC",
        )?;
        let rows = stmt
            .query_map(params![project_exact, project_glob], |row| {
                Ok(MemoryOsActionExecutionSummaryRow {
                    project_path: row.get(0)?,
                    command_sig: row.get(1)?,
                    exit_code: row.get(2)?,
                    observed_at: parse_rfc3339_to_utc(&row.get::<_, String>(3)?),
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub(super) fn get_memory_os_correction_patterns(
        &self,
        scope: crate::core::memory_os::MemoryOsInspectionScope,
        project_path: Option<&str>,
    ) -> Result<Vec<crate::core::memory_os::MemoryOsCorrectionPatternSummary>> {
        let corrections = self.load_memory_os_correction_observations(scope, project_path)?;
        let executions = self.load_memory_os_action_executions(scope, project_path)?;
        let mut execution_used = vec![false; executions.len()];
        let mut accumulators: HashMap<
            (String, String, String),
            crate::core::memory_os::MemoryOsCorrectionPatternSummary,
        > = HashMap::new();

        for correction in corrections {
            let key = (
                correction.error_kind.clone(),
                correction.wrong_command.clone(),
                correction.corrected_command.clone(),
            );
            let entry = accumulators.entry(key).or_insert_with(|| {
                crate::core::memory_os::MemoryOsCorrectionPatternSummary {
                    error_kind: correction.error_kind.clone(),
                    wrong_command: compact_display_text(&correction.wrong_command, 120),
                    corrected_command: compact_display_text(&correction.corrected_command, 120),
                    count: 0,
                    successful_replays: 0,
                    failed_replays: 0,
                }
            });
            entry.count += 1;
            if let Some((index, execution)) =
                executions.iter().enumerate().find(|(index, execution)| {
                    !execution_used[*index]
                        && execution.project_path == correction.project_path
                        && execution.command_sig == correction.corrected_command
                        && execution.observed_at >= correction.observed_at
                })
            {
                execution_used[index] = true;
                if execution.exit_code == 0 {
                    entry.successful_replays += 1;
                } else {
                    entry.failed_replays += 1;
                }
            }
        }

        let mut patterns = accumulators.into_values().collect::<Vec<_>>();
        patterns.sort_by(|left, right| {
            right
                .count
                .cmp(&left.count)
                .then(right.successful_replays.cmp(&left.successful_replays))
                .then(left.error_kind.cmp(&right.error_kind))
        });
        Ok(patterns)
    }

    pub(super) fn build_memory_os_redirect_summary(
        &self,
        scope: crate::core::memory_os::MemoryOsInspectionScope,
        project_path: Option<&str>,
        checkpoints: &[MemoryOsCheckpointEnvelope],
    ) -> Result<crate::core::memory_os::MemoryOsRedirectSummary> {
        let executions = self.load_memory_os_action_executions(scope, project_path)?;
        let mut grouped: BTreeMap<String, Vec<&MemoryOsCheckpointEnvelope>> = BTreeMap::new();
        for checkpoint in checkpoints
            .iter()
            .filter(|checkpoint| checkpoint.capture.profile != "session-onboarding")
        {
            grouped
                .entry(checkpoint.project_path.clone())
                .or_default()
                .push(checkpoint);
        }

        let mut totals = MemoryOsRedirectAccumulator::default();
        for (project, mut project_checkpoints) in grouped {
            project_checkpoints.sort_by(|left, right| left.captured_at.cmp(&right.captured_at));
            let mut saw_project_redirect = false;
            for window in project_checkpoints.windows(2) {
                let previous = &window[0].capture;
                let current = &window[1].capture;
                let previous_summary = first_non_empty(&[
                    previous.reentry.current_recommendation.clone(),
                    previous.goal.clone(),
                ]);
                let current_summary = first_non_empty(&[
                    current.reentry.current_recommendation.clone(),
                    current.goal.clone(),
                ]);
                if previous_summary.is_none() || current_summary.is_none() {
                    continue;
                }
                if previous_summary == current_summary {
                    continue;
                }

                totals.redirects += 1;
                saw_project_redirect = true;
                let redirect_at = window[1].captured_at;
                if let Some((commands_until_success, seconds_to_success, had_success)) =
                    execution_progress_after(&executions, &project, redirect_at)
                {
                    totals.redirects_with_resumed_shell += 1;
                    if had_success {
                        totals.redirects_with_success_after_resume += 1;
                        totals.commands_to_success_sum += commands_until_success;
                        totals.seconds_to_success_sum += seconds_to_success;
                    }
                }
            }
            if saw_project_redirect {
                totals.redirected_sessions += 1;
            }
        }

        let success_count = totals.redirects_with_success_after_resume;
        Ok(crate::core::memory_os::MemoryOsRedirectSummary {
            redirects: totals.redirects,
            redirected_sessions: totals.redirected_sessions,
            redirects_with_resumed_shell: totals.redirects_with_resumed_shell,
            redirects_with_success_after_resume: totals.redirects_with_success_after_resume,
            avg_commands_to_success_after_redirect: if success_count > 0 {
                Some(totals.commands_to_success_sum as f64 / success_count as f64)
            } else {
                None
            },
            avg_seconds_to_success_after_redirect: if success_count > 0 {
                Some(totals.seconds_to_success_sum / success_count as f64)
            } else {
                None
            },
        })
    }
}
