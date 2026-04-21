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

fn format_optional_metric(value: Option<f64>) -> String {
    value
        .map(|metric| format!("{metric:.1}"))
        .unwrap_or_else(|| "n/a".to_string())
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
    if lowered.starts_with("exit code:")
        || lowered.starts_with("exit ")
        || lowered.contains("blocked by policy")
        || lowered.starts_with("error: process didn't exit")
    {
        return None;
    }
    Some(compact)
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
    fixes.extend(user_prose_friction_fixes(
        checkpoints,
        durable_fixes,
        Utc::now(),
    ));
    fixes.extend(command_friction_fixes(
        correction_patterns,
        likely_misunderstandings,
    ));
    fixes.extend(behavior_change_friction_fixes(behavior_changes, redirects));

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

#[derive(Debug, Default, Clone)]
pub(super) struct UserProseSignalCounts {
    pub(super) command_noise: usize,
    pub(super) autonomy: usize,
    pub(super) stale_output: usize,
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
}

pub(super) fn detect_user_prose_durable_fixes(project_path: Option<&str>) -> UserProseDurableFixes {
    UserProseDurableFixes {
        autonomy_polling: find_durable_autonomy_polling_instruction(project_path),
    }
}

pub(super) fn count_user_prose_signals(
    checkpoints: &[MemoryOsCheckpointEnvelope],
) -> UserProseSignalCounts {
    let mut command_noise_evidence = Vec::new();
    let mut command_noise_seen = HashSet::new();
    let mut autonomy_seen = HashSet::new();
    let mut stale_output_seen = HashSet::new();
    let mut latest_autonomy_at = None;

    for checkpoint in checkpoints
        .iter()
        .filter(|checkpoint| checkpoint.capture.profile == "session-onboarding")
    {
        for text in checkpoint
            .capture
            .goal
            .iter()
            .chain(checkpoint.capture.reentry.current_recommendation.iter())
            .map(|value| value.as_str())
            .chain(
                checkpoint
                    .capture
                    .selected_items
                    .iter()
                    .filter(|item| item.section == "user_prompts")
                    .map(|item| item.summary.as_str()),
            )
        {
            let signal_key = compact_display_text(text, 220).to_ascii_lowercase();
            let lowered = text.to_ascii_lowercase();
            if lowered.contains("command noise")
                || lowered.contains("garbage")
                || lowered.contains("useless")
                || lowered.contains("wtf")
                || lowered.contains("what the hell")
            {
                command_noise_seen.insert(signal_key.clone());
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
        fixes.push(crate::core::memory_os::MemoryOsFrictionFix {
            fix_id: "friction:user-command-noise".to_string(),
            title: "Stop surfacing command/build noise as memory".to_string(),
            impact: "high".to_string(),
            status: "active".to_string(),
            summary: format!(
                "User has directly corrected noisy or useless Memory OS output {} times.",
                counts.command_noise
            ),
            permanent_fix:
                "Keep strategy facts and user prose above shell/build output; reserve raw commands for inspect/json evidence."
                    .to_string(),
            evidence: counts.command_noise_evidence.into_iter().take(3).collect(),
            score: 120 + counts.command_noise.min(20) as i64,
        });
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

fn find_durable_autonomy_polling_instruction(
    project_path: Option<&str>,
) -> Option<DurableFrictionFixEvidence> {
    let start = PathBuf::from(resolved_project_path(project_path));
    let agents_path = find_nearest_agents_file(&start)?;
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
        fixes.push(crate::core::memory_os::MemoryOsFrictionFix {
            fix_id: format!("friction:behavior:{}", change.target_agent),
            title: format!("Behavior change for {}", change.target_agent),
            impact: "medium".to_string(),
            status: "active".to_string(),
            summary: change.rationale.clone(),
            permanent_fix: change.change.clone(),
            evidence: change.evidence.iter().take(3).cloned().collect(),
            score: 60,
        });
    }
    fixes
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
) -> Vec<crate::core::memory_os::MemoryOsBehaviorChangeRecommendation> {
    let mut recommendations = Vec::new();

    let autonomy_rule_needed = autonomy_count > 0
        && !matches!(
            autonomy_friction_status,
            Some("codified" | "fixed" | "retired")
        );

    if autonomy_rule_needed {
        let autonomy_rationale = format!(
            "User has asked for stronger autonomous polling/approval behavior {autonomy_count} times; treat any 'poll', 'keep going', 'until done', or long-running instruction as an infinite-loop contract.",
        );
        recommendations.push(
            crate::core::memory_os::MemoryOsBehaviorChangeRecommendation {
                target_agent: "codex".to_string(),
                change: "When a task calls for polling, waiting, or iterating until something is solved, keep cycling without pausing to ask \"should I continue?\". Stop only when the task is verified solved or a concrete blocker is recorded.".to_string(),
                rationale: autonomy_rationale.clone(),
                evidence: vec![
                    format!("{autonomy_count} autonomy/polling corrections"),
                    "Codex is the bigger offender for mid-loop pauses".to_string(),
                ],
            },
        );
        recommendations.push(
            crate::core::memory_os::MemoryOsBehaviorChangeRecommendation {
                target_agent: "claude".to_string(),
                change: "When the user asks for polling or long-running work, keep iterating until the task is solved or a concrete blocker is recorded. Do not return to the prompt between cycles or summarise progress in place of continuing.".to_string(),
                rationale: autonomy_rationale,
                evidence: vec![
                    format!("{autonomy_count} autonomy/polling corrections"),
                    "Shared contract with codex so both lanes behave the same under polling instructions".to_string(),
                ],
            },
        );
    }

    recommendations.push(
        crate::core::memory_os::MemoryOsBehaviorChangeRecommendation {
            target_agent: "codex".to_string(),
                change: "Use `munin memory-os overview/profile/friction --scope user` before reading raw recall or session history for user/profile/current-work questions.".to_string(),
            rationale: "Codex needs a deterministic Memory OS-first read path so fresh sessions stop trawling docs and archives for questions the compiled state can already answer.".to_string(),
            evidence: memory_os_serving_policy_lines(),
        },
    );

    if let Some(codex) = by_source.iter().find(|source| source.source == "codex") {
        recommendations.push(
            crate::core::memory_os::MemoryOsBehaviorChangeRecommendation {
                target_agent: "codex".to_string(),
                change: "Keep moving after grounding, but front-load the scoped Memory OS profile so command corrections and active-work cues are visible before acting.".to_string(),
                rationale: "The Codex lane is highly execution-heavy, so front-load the Memory OS profile so command corrections and active-work cues are visible before acting.".to_string(),
                evidence: vec![
                    format!("{:.1} shells/session", codex.shells_per_session),
                    format!(
                        "{} corrections per 100 shells",
                        format_optional_metric(Some(codex.corrections_per_100_shells))
                    ),
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
                    format!("{} sessions", claude.sessions),
                    format!("{} shell executions", claude.shell_executions),
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

        let without_signal = build_memory_os_behavior_changes(&by_source, &redirects, 0, None);
        assert!(
            !without_signal
                .iter()
                .any(|rec| rec.change.contains("polling")),
            "no polling rule should appear when autonomy_count is 0"
        );

        let with_signal = build_memory_os_behavior_changes(&by_source, &redirects, 154, None);
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
            build_memory_os_behavior_changes(&by_source, &redirects, 154, Some("codified"));

        assert!(
            !codified.iter().any(|rec| rec.change.contains("polling")),
            "durably codified polling rules should stop surfacing as behavior changes"
        );
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
    fn agents_file_codification_requires_autonomy_and_completion_contract() {
        assert!(agents_file_codifies_autonomy_polling(
            "AUTONOMY DIRECTIVE\nYOU ARE AN AUTONOMOUS CODING AGENT.\nEXECUTE TASKS TO COMPLETION WITHOUT ASKING FOR PERMISSION.\nDO NOT STOP TO ASK \"SHOULD I PROCEED?\""
        ));
        assert!(!agents_file_codifies_autonomy_polling(
            "You are autonomous, but ask before proceeding."
        ));
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
