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
    fixes.extend(user_prose_actionable_friction_fixes(
        checkpoints,
        durable_fixes,
        now,
    ));
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
            fix.last_signal_at.as_deref(),
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
            !completed_friction_fix_matches(Some(item_id.as_str()), "friction-fix", None, completed)
        })
        .collect()
}

fn completed_friction_fix_matches(
    item_id: Option<&str>,
    item_kind: &str,
    last_signal_at: Option<&str>,
    completed: &[crate::core::tracking::ApprovalJobRecord],
) -> bool {
    completed.iter().any(|record| {
        record.item_kind == item_kind
            && super::friction_fix_item_ids_related(record.item_id.as_deref(), item_id)
            && !friction_signal_is_newer_than_completion(last_signal_at, record)
    })
}

fn friction_signal_is_newer_than_completion(
    last_signal_at: Option<&str>,
    record: &crate::core::tracking::ApprovalJobRecord,
) -> bool {
    let Some(last_signal_at) = last_signal_at else {
        return false;
    };
    let signal_at = parse_rfc3339_to_utc(last_signal_at);
    let completed_at = record
        .last_reviewed_at
        .as_deref()
        .map(parse_rfc3339_to_utc)
        .unwrap_or(record.updated_at);
    signal_at > completed_at
}

#[derive(Debug, Default, Clone)]
pub(super) struct UserProseSignalCounts {
    pub(super) command_noise: usize,
    pub(super) autonomy: usize,
    pub(super) stale_output: usize,
    pub(super) latest_command_noise_at: Option<DateTime<Utc>>,
    pub(super) latest_autonomy_at: Option<DateTime<Utc>>,
    pub(super) latest_stale_output_at: Option<DateTime<Utc>>,
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
    pub(super) live_runtime_verification: Option<DurableFrictionFixEvidence>,
    pub(super) outcome_repair: Option<DurableFrictionFixEvidence>,
    pub(super) checkpoint_resume: Option<DurableFrictionFixEvidence>,
    pub(super) proxy_completion: Option<DurableFrictionFixEvidence>,
}

const CONTEXT_REVERSAL_FRICTION_MARKER: &str = "munin-friction:context-reversal";

pub(super) fn detect_user_prose_durable_fixes(project_path: Option<&str>) -> UserProseDurableFixes {
    let autonomy_polling = find_durable_autonomy_polling_instruction(project_path);
    let codex_autonomy_polling = autonomy_polling
        .clone()
        .or_else(|| find_codex_durable_autonomy_polling_instruction(project_path));
    let context_reversal_clarification =
        find_context_reversal_clarification_instruction(project_path);
    let live_runtime_verification = find_live_runtime_verification_instruction(project_path);
    let outcome_repair = find_outcome_repair_instruction(project_path);
    let checkpoint_resume = find_checkpoint_resume_instruction(project_path);
    let proxy_completion = find_proxy_completion_instruction(project_path);
    UserProseDurableFixes {
        autonomy_polling,
        codex_autonomy_polling,
        command_noise_surface_policy: Some(current_binary_durable_fix_evidence(
            "Memory OS text surfaces suppress command/build noise",
        )),
        context_reversal_clarification,
        live_runtime_verification,
        outcome_repair,
        checkpoint_resume,
        proxy_completion,
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
    let mut latest_stale_output_at = None;

    for checkpoint in checkpoints
        .iter()
        .filter(|checkpoint| checkpoint.capture.profile == "session-onboarding")
    {
        for text in checkpoint_user_prose(checkpoint) {
            let signal_key = compact_display_text(text, 220).to_ascii_lowercase();
            let lowered = text.to_ascii_lowercase();
            if user_prose_friction_text_is_instruction_payload(&lowered) {
                continue;
            }
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
            if text_has_autonomy_signal(&lowered) && text_has_autonomy_correction(&lowered) {
                autonomy_seen.insert(signal_key.clone());
                counts_latest_at(
                    &mut latest_autonomy_at,
                    checkpoint_original_signal_time(checkpoint),
                );
            }
            if lowered.contains("still not returning")
                || lowered.contains("not done until")
                || lowered.contains("shows useful")
                || lowered.contains("correct info")
            {
                stale_output_seen.insert(signal_key.clone());
                counts_latest_at(
                    &mut latest_stale_output_at,
                    checkpoint_original_signal_time(checkpoint),
                );
            }
        }
    }

    UserProseSignalCounts {
        command_noise: command_noise_seen.len(),
        autonomy: autonomy_seen.len(),
        stale_output: stale_output_seen.len(),
        latest_command_noise_at,
        latest_autonomy_at,
        latest_stale_output_at,
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
                last_signal_at: counts.latest_command_noise_at.map(|value| value.to_rfc3339()),
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
            last_signal_at: counts.latest_stale_output_at.map(|value| value.to_rfc3339()),
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
            last_signal_at: counts.latest_autonomy_at.map(|value| value.to_rfc3339()),
            score: 100 + counts.autonomy.min(20) as i64,
        });
    }
    fixes
}

#[derive(Debug, Clone, Copy)]
struct UserProseFrictionSpec {
    id: &'static str,
    title: &'static str,
    summary: &'static str,
    permanent_fix: &'static str,
    impact: &'static str,
    score: i64,
    any: &'static [&'static str],
    all: &'static [&'static str],
}

const USER_PROSE_ACTIONABLE_FRICTION_SPECS: &[UserProseFrictionSpec] = &[
    UserProseFrictionSpec {
        id: "resume-from-last-checkpoint",
        title: "Resume from the last proven checkpoint",
        summary: "User frustration says agents restart or re-spec work instead of continuing from proven session state.",
        permanent_fix: "Recover the last proven checkpoint from memory, session logs, git state, or run artifacts before restarting the task.",
        impact: "high",
        score: 108,
        any: &[
            "pick up exactly where",
            "continue from",
            "last proven checkpoint",
            "where it left off",
            "resume from",
            "resume exactly",
        ],
        all: &[],
    },
    UserProseFrictionSpec {
        id: "fix-real-pipeline-end-to-end",
        title: "Fix the real pipeline end to end",
        summary: "User frustration says agents patch a visible symptom while the actual automation path remains broken.",
        permanent_fix: "Trace the complete producer, runner, persistence, and consumer path, then verify the live command that was failing.",
        impact: "high",
        score: 107,
        any: &["end-to-end", "end to end", "real pipeline", "actual pipeline", "not picking up", "automation"],
        all: &[],
    },
    UserProseFrictionSpec {
        id: "verify-live-runtime",
        title: "Verify the live runtime, not just source code",
        summary: "User frustration says source changes are reported as fixed before the installed binary, daemon, watcher, or browser path proves it.",
        permanent_fix: "Run the installed/live command or service path that the user actually depends on before claiming the fix works.",
        impact: "high",
        score: 106,
        any: &["live environment", "live runtime", "actual runtime", "running process", "installed binary", "watcher"],
        all: &[],
    },
    UserProseFrictionSpec {
        id: "promotion-from-temp-worktree",
        title: "Promote verified temp work back to the live checkout",
        summary: "User frustration says completed fixes get stranded in scratch or temporary worktrees instead of reaching the active checkout.",
        permanent_fix: "After verification, merge or apply the verified change into the live checkout and refresh the process using it.",
        impact: "high",
        score: 105,
        any: &["temp worktree", "temporary worktree", "promote it back", "live checkout", "stranded"],
        all: &[],
    },
    UserProseFrictionSpec {
        id: "surface-useful-friction",
        title: "Surface useful friction instead of empty reports",
        summary: "User frustration says Munin hides obvious personal frustration and returns little or nothing actionable.",
        permanent_fix: "Rank explicit user frustration and correction prose as first-class friction candidates, not just command replay failures.",
        impact: "high",
        score: 104,
        any: &["friction", "personal frustration", "nothing appears", "useful work every day", "surface"],
        all: &[],
    },
    UserProseFrictionSpec {
        id: "memory-freshness-before-answering",
        title: "Refresh memory before trusting stale answers",
        summary: "User frustration says stale memory or recall output is used after newer local evidence exists.",
        permanent_fix: "Check current memory status and refresh/import sessions before using memory-derived claims when freshness matters.",
        impact: "high",
        score: 103,
        any: &["stale", "outdated", "not current", "refresh", "new sessions", "indexed", "embedded"],
        all: &["memory"],
    },
    UserProseFrictionSpec {
        id: "recover-from-session-history",
        title: "Use session history when recall is incomplete",
        summary: "User frustration says agents stop when recall is empty despite local session JSONL and git history containing the answer.",
        permanent_fix: "When recall misses, inspect local session exports, recent git history, and run directories before declaring no context.",
        impact: "high",
        score: 102,
        any: &["session history", "session jsonl", "recall is empty", "can't use recall", "git history"],
        all: &[],
    },
    UserProseFrictionSpec {
        id: "avoid-internal-customer-language",
        title: "Keep customer-facing language safe and external",
        summary: "User frustration says agents expose internal terms in customer-visible product or status surfaces.",
        permanent_fix: "Translate implementation terms into customer-safe progress, preview, design example, and outcome language.",
        impact: "medium",
        score: 101,
        any: &[
            "customer-facing",
            "customer language",
            "internal terms",
            "internal worker",
            "internal queue",
            "runtime term",
        ],
        all: &[],
    },
    UserProseFrictionSpec {
        id: "browser-proof-for-ui",
        title: "Use browser proof for UI and frontend claims",
        summary: "User frustration says UI work is treated as done without browser verification of the actual page.",
        permanent_fix: "Open the local page, inspect the DOM or screenshot, and fix visible problems before reporting UI completion.",
        impact: "medium",
        score: 100,
        any: &["browser", "screenshot", "localhost", "visual", "ui", "frontend"],
        all: &["verify"],
    },
    UserProseFrictionSpec {
        id: "focus-on-user-outcome",
        title: "Prefer outcome fixes over diagnostic summaries",
        summary: "User frustration says agents summarize what broke instead of repairing the workflow until it is useful.",
        permanent_fix: "Turn diagnosis into an implemented and verified repair unless the user explicitly asks for report-only analysis.",
        impact: "high",
        score: 99,
        any: &["fix this", "repair", "broken", "useful work", "do useful", "not a report"],
        all: &[],
    },
    UserProseFrictionSpec {
        id: "respect-worktree-hygiene",
        title: "Keep worktree changes committed and cleaned up",
        summary: "User frustration says worktrees are left dirty, stale, or disconnected from the finished work.",
        permanent_fix: "Commit isolated worktree fixes, promote them, and remove stale worktrees after successful integration.",
        impact: "medium",
        score: 98,
        any: &["worktree", "dirty", "stale tree", "remove the worktree", "commit"],
        all: &[],
    },
    UserProseFrictionSpec {
        id: "inspect-active-config",
        title: "Inspect the active config surface before assuming paths",
        summary: "User frustration says agents edit the obvious config file while the active pane, env var, or installed command uses another one.",
        permanent_fix: "Check the active environment, command path, and config home before editing configuration or MCP/runtime wiring.",
        impact: "high",
        score: 97,
        any: &["codex_home", "active config", "config.toml", "mcp list", "pane-local", "environment"],
        all: &[],
    },
    UserProseFrictionSpec {
        id: "windows-native-paths",
        title: "Handle Windows paths and shims correctly",
        summary: "User frustration says agents assume Unix paths or wrapper behavior on this Windows machine.",
        permanent_fix: "Use Windows-native paths, PowerShell-aware quoting, and known direct binaries when shims or Unix paths are suspect.",
        impact: "medium",
        score: 96,
        any: &["windows", "powershell", "c:\\", "tmp", "node.exe", "nvm4w", "shim"],
        all: &[],
    },
    UserProseFrictionSpec {
        id: "do-not-overask-permission",
        title: "Stop asking for permission on obvious next steps",
        summary: "User frustration says agents pause for confirmation instead of executing safe continuation steps.",
        permanent_fix: "Proceed through safe diagnostic, implementation, and verification steps; ask only for destructive or materially branching choices.",
        impact: "high",
        score: 95,
        any: &["should i proceed", "asking for permission", "without asking", "don't ask", "do not wait"],
        all: &[],
    },
    UserProseFrictionSpec {
        id: "layman-first-when-requested",
        title: "Explain in layman terms first when asked",
        summary: "User frustration says agents answer implementation detail when the request is for plain-English understanding.",
        permanent_fix: "Lead with a simple everyday explanation, then answer direct implementation follow-ups precisely.",
        impact: "medium",
        score: 94,
        any: &["laymans terms", "layman", "plain english", "teach me"],
        all: &[],
    },
    UserProseFrictionSpec {
        id: "direct-implementation-answers",
        title: "Answer direct implementation questions directly",
        summary: "User frustration says agents stay at a high-level overview after the user asks a concrete implementation question.",
        permanent_fix: "When the user asks whether a component uses a tool or path, inspect the code and answer that point directly.",
        impact: "medium",
        score: 93,
        any: &["does it use", "implementation-specific", "direct question", "firecrawl"],
        all: &[],
    },
    UserProseFrictionSpec {
        id: "avoid-proxy-completion-signals",
        title: "Do not accept proxy signals as completion",
        summary: "User frustration says passing tests or generated manifests are treated as completion without covering the real requirement.",
        permanent_fix: "Map every explicit requirement to evidence and verify the user-facing behavior, not just proxy green checks.",
        impact: "high",
        score: 92,
        any: &["proxy", "manifest", "not done until", "completion audit", "verify", "evidence"],
        all: &[],
    },
    UserProseFrictionSpec {
        id: "keep-proactivity-actionable",
        title: "Make proactivity pick fixable work",
        summary: "User frustration says daily proactivity surfaces stale or already-solved items instead of useful fixable work.",
        permanent_fix: "Filter completed, codified, monitoring, and stale nudges out of the primary queue and rank fresh fixable friction first.",
        impact: "high",
        score: 91,
        any: &["proactivity", "daily", "morning", "nudge", "already", "monitoring"],
        all: &[],
    },
    UserProseFrictionSpec {
        id: "do-not-revert-user-work",
        title: "Do not revert unrelated user changes",
        summary: "User frustration says agents risk undoing work that was not theirs while trying to clean a checkout.",
        permanent_fix: "Inspect dirty changes, isolate your diff, and never reset or revert unrelated files without explicit request.",
        impact: "high",
        score: 90,
        any: &["do not revert", "don't revert", "unrelated changes", "dirty checkout", "reset --hard"],
        all: &[],
    },
    UserProseFrictionSpec {
        id: "prefer-existing-patterns",
        title: "Follow existing project patterns",
        summary: "User frustration says agents invent new abstractions or dependencies instead of using local conventions.",
        permanent_fix: "Read nearby code first, reuse established helpers, and keep diffs scoped unless the existing pattern is clearly broken.",
        impact: "medium",
        score: 89,
        any: &["existing patterns", "local helper", "new dependency", "small diff", "conventions"],
        all: &[],
    },
    UserProseFrictionSpec {
        id: "open-specified-file-first",
        title: "Open the exact specified file first",
        summary: "User frustration says agents do broad discovery when the user gave a direct file path or live skill path.",
        permanent_fix: "For direct-path tasks, inspect the named file first and only broaden discovery when that file requires it.",
        impact: "medium",
        score: 88,
        any: &["exact file path", "direct-path", "open that file first", "specified file"],
        all: &[],
    },
    UserProseFrictionSpec {
        id: "avoid-wrong-scope-research",
        title: "Keep research scoped to the asked surface",
        summary: "User frustration says agents wander into adjacent skills, repos, or historical artifacts that do not govern the current task.",
        permanent_fix: "Use the loaded skill or repo instructions as the boundary, then expand only when the active file or failing command points there.",
        impact: "medium",
        score: 87,
        any: &[
            "wrong scope",
            "sibling skills",
            "unrelated scope",
            "broad scan",
            "scope creep",
        ],
        all: &[],
    },
    UserProseFrictionSpec {
        id: "surface-what-can-be-fixed",
        title: "Separate fixable friction from background history",
        summary: "User frustration says fixed, retired, codified, or merely monitored history crowds out work the agent can actually improve.",
        permanent_fix: "Primary friction views should prioritize active fix candidates and keep background statuses secondary.",
        impact: "high",
        score: 86,
        any: &["fixed", "retired", "codified", "monitoring", "can fix"],
        all: &["friction"],
    },
    UserProseFrictionSpec {
        id: "verify-installed-cli-after-build",
        title: "Promote and verify the installed CLI after build",
        summary: "User frustration says repository tests pass but the command on PATH still runs old behavior.",
        permanent_fix: "After building CLI changes, install or copy the binary used by PATH and rerun the user-facing command.",
        impact: "high",
        score: 85,
        any: &["on path", "path still", "installed cli", "cargo install", "munin.exe"],
        all: &[],
    },
    UserProseFrictionSpec {
        id: "handle-scheduler-environment-drift",
        title: "Check scheduled-task environments for drift",
        summary: "User frustration says automation works in an interactive shell but fails under the scheduler environment.",
        permanent_fix: "Inspect the scheduled task action, env paths, cache paths, and last result before trusting an interactive success.",
        impact: "medium",
        score: 84,
        any: &["task scheduler", "scheduled", "lasttaskresult", "interactive shell", "wrong-environment"],
        all: &[],
    },
    UserProseFrictionSpec {
        id: "preserve-customer-progress-links",
        title: "Persist progress links across handoff",
        summary: "User frustration says progress is trapped in the browser instead of a durable link that survives email or device handoff.",
        permanent_fix: "Persist status server-side and return stable request, token, and preview links for handoff workflows.",
        impact: "medium",
        score: 83,
        any: &["progress link", "status token", "requestid", "handoff", "email/device", "builderurl"],
        all: &[],
    },
    UserProseFrictionSpec {
        id: "finish-with-concrete-impact",
        title: "Report what changed in user-impact terms",
        summary: "User frustration says final answers omit what the fix now does, what it can do, and the practical impact.",
        permanent_fix: "After technical evidence, include a compact user-impact explanation of the build or fix.",
        impact: "medium",
        score: 82,
        any: &["what it can do", "impact of change", "layman summary", "user impact", "product impact"],
        all: &[],
    },
    UserProseFrictionSpec {
        id: "avoid-returning-mid-loop",
        title: "Keep looping on long-running tasks until terminal evidence",
        summary: "User frustration says agents return progress summaries in the middle of a polling or keep-going instruction.",
        permanent_fix: "For poll, keep-going, until-done, or infinite-task instructions, check, act, and check again until solved or concretely blocked.",
        impact: "high",
        score: 81,
        any: &["polling contract", "between cycles", "keep going", "until done", "infinite task"],
        all: &[],
    },
    UserProseFrictionSpec {
        id: "turn-frustration-into-rules",
        title: "Turn repeated frustration into durable rules",
        summary: "User frustration says recurring corrections stay as vague memory instead of becoming actionable agent behavior.",
        permanent_fix: "Promote repeated personal-frustration patterns into explicit rules, tests, or ranked fix candidates.",
        impact: "high",
        score: 80,
        any: &["recurring", "keeps happening", "codified", "rule", "actual friction", "personal frustration"],
        all: &[],
    },
];

fn user_prose_actionable_friction_fixes(
    checkpoints: &[MemoryOsCheckpointEnvelope],
    durable_fixes: &UserProseDurableFixes,
    now: DateTime<Utc>,
) -> Vec<crate::core::memory_os::MemoryOsFrictionFix> {
    let mut matched: BTreeMap<
        &'static str,
        (
            &'static UserProseFrictionSpec,
            HashSet<String>,
            Option<DateTime<Utc>>,
            Vec<String>,
        ),
    > = BTreeMap::new();

    for checkpoint in checkpoints
        .iter()
        .filter(|checkpoint| checkpoint.capture.profile == "session-onboarding")
    {
        let signal_at = checkpoint_original_signal_time(checkpoint);
        for text in checkpoint_user_prose(checkpoint) {
            let lowered = text.to_ascii_lowercase();
            if checkpoint_summary_is_command_or_build_noise(&lowered)
                || user_prose_friction_text_is_instruction_payload(&lowered)
            {
                continue;
            }
            for spec in USER_PROSE_ACTIONABLE_FRICTION_SPECS {
                if !user_prose_matches_spec(&lowered, spec) {
                    continue;
                }
                let entry =
                    matched
                        .entry(spec.id)
                        .or_insert((spec, HashSet::new(), None, Vec::new()));
                let signal_key = compact_display_text(text, 220).to_ascii_lowercase();
                if !entry.1.insert(signal_key) {
                    continue;
                }
                counts_latest_at(&mut entry.2, signal_at);
                push_unique_string(
                    &mut entry.3,
                    format!(
                        "user signal at {}: {}",
                        checkpoint.capture.generated_at,
                        compact_display_text(text, 140)
                    ),
                );
            }
        }
    }

    let mut fixes = matched
        .into_values()
        .filter_map(|(spec, signal_keys, latest_at, mut evidence)| {
            let count = signal_keys.len();
            if count < 2 {
                return None;
            }
            let status =
                user_prose_actionable_friction_status(spec.id, latest_at, durable_fixes, now);
            if status != "active" {
                return None;
            }
            if let Some(durable) = user_prose_actionable_durable_fix(spec.id, durable_fixes) {
                evidence.push(format!(
                    "newer user signal exists after durable rule in {} at {}",
                    durable.path,
                    durable.codified_at.to_rfc3339()
                ));
            }
            Some(crate::core::memory_os::MemoryOsFrictionFix {
                fix_id: format!("friction:user-prose:{}", spec.id),
                title: spec.title.to_string(),
                impact: spec.impact.to_string(),
                status,
                summary: format!(
                    "{} Matched {} user-frustration signal(s).",
                    spec.summary, count
                ),
                permanent_fix: spec.permanent_fix.to_string(),
                evidence: evidence.into_iter().take(3).collect(),
                last_signal_at: latest_at.map(|value| value.to_rfc3339()),
                score: spec.score + count.min(25) as i64,
            })
        })
        .collect::<Vec<_>>();
    fixes.sort_by(|left, right| {
        right
            .score
            .cmp(&left.score)
            .then(left.title.cmp(&right.title))
    });
    fixes
}

fn user_prose_actionable_friction_status(
    spec_id: &str,
    latest_at: Option<DateTime<Utc>>,
    durable_fixes: &UserProseDurableFixes,
    now: DateTime<Utc>,
) -> String {
    user_prose_actionable_durable_fix(spec_id, durable_fixes)
        .map(|durable| durable_friction_status(latest_at, Some(durable), now))
        .unwrap_or_else(|| "active".to_string())
}

fn user_prose_actionable_durable_fix<'a>(
    spec_id: &str,
    durable_fixes: &'a UserProseDurableFixes,
) -> Option<&'a DurableFrictionFixEvidence> {
    match spec_id {
        "do-not-overask-permission" | "avoid-returning-mid-loop" => durable_fixes
            .autonomy_polling
            .as_ref()
            .or(durable_fixes.codex_autonomy_polling.as_ref()),
        "surface-useful-friction" | "surface-what-can-be-fixed" => {
            durable_fixes.command_noise_surface_policy.as_ref()
        }
        "verify-live-runtime" => durable_fixes.live_runtime_verification.as_ref(),
        "focus-on-user-outcome" => durable_fixes.outcome_repair.as_ref(),
        "resume-from-last-checkpoint" => durable_fixes.checkpoint_resume.as_ref(),
        "avoid-proxy-completion-signals" => durable_fixes.proxy_completion.as_ref(),
        _ => None,
    }
}

fn user_prose_matches_spec(lowered: &str, spec: &UserProseFrictionSpec) -> bool {
    spec.any.iter().any(|needle| lowered.contains(needle))
        && spec.all.iter().all(|needle| lowered.contains(needle))
}

fn user_prose_friction_text_is_instruction_payload(lowered: &str) -> bool {
    lowered.contains("base directory for this skill:")
        || lowered.contains("<subagent_notification>")
        || lowered.contains("<local-command-stdout>")
        || lowered.contains("<scheduled-task")
        || user_prose_friction_text_is_operational_review_prompt(lowered)
        || lowered.starts_with("read-only review.")
        || lowered.contains("read-only review task")
        || lowered.contains("read-only final review")
        || lowered.contains("do not edit files. workspace:")
        || lowered.starts_with("you are ")
        || lowered.starts_with("you are the critic in")
        || lowered.starts_with("you are the architect in")
        || lowered.starts_with("you are the planner in")
        || lowered.starts_with("munin-morning.")
        || lowered.contains("<skill>")
        || lowered.contains("# agents.md instructions")
        || lowered.contains("<environment_context>")
        || lowered.contains("you are an autonomous coding agent")
        || lowered.contains("codex global contract")
}

fn user_prose_friction_text_is_operational_review_prompt(lowered: &str) -> bool {
    let is_read_only_prompt = lowered.contains("read-only") || lowered.contains("read only");
    let is_review_task = lowered.contains("audit")
        || lowered.contains("sweep")
        || lowered.contains("review")
        || lowered.contains("bug hunt")
        || lowered.contains("verification");
    let has_operational_guard = lowered.contains("do not edit files") && is_review_task;

    (is_read_only_prompt && is_review_task)
        || has_operational_guard
        || lowered.starts_with("post-fix clean sweep")
        || lowered.starts_with("clone-speed clean verification sweep")
        || lowered.starts_with("clone-speed runtime verification sweep")
}

pub(super) fn build_memory_os_new_unproven_friction(
    checkpoints: &[MemoryOsCheckpointEnvelope],
    durable_fixes: &UserProseDurableFixes,
) -> Vec<crate::core::memory_os::MemoryOsFrictionFix> {
    let mut wrong_terminal_evidence = Vec::new();
    let mut wrong_terminal_seen = HashSet::new();
    let mut latest_wrong_terminal_at = None;
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
                counts_latest_at(&mut latest_wrong_terminal_at, signal_at);
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
        last_signal_at: latest_wrong_terminal_at.map(|value| value.to_rfc3339()),
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

fn find_live_runtime_verification_instruction(
    project_path: Option<&str>,
) -> Option<DurableFrictionFixEvidence> {
    find_durable_friction_instruction(project_path, instructions_file_codifies_live_runtime)
}

fn find_outcome_repair_instruction(
    project_path: Option<&str>,
) -> Option<DurableFrictionFixEvidence> {
    find_durable_friction_instruction(project_path, instructions_file_codifies_outcome_repair)
}

fn find_checkpoint_resume_instruction(
    project_path: Option<&str>,
) -> Option<DurableFrictionFixEvidence> {
    find_durable_friction_instruction(project_path, instructions_file_codifies_checkpoint_resume)
}

fn find_proxy_completion_instruction(
    project_path: Option<&str>,
) -> Option<DurableFrictionFixEvidence> {
    find_durable_friction_instruction(project_path, instructions_file_codifies_proxy_completion)
}

fn find_durable_friction_instruction(
    project_path: Option<&str>,
    predicate: fn(&str) -> bool,
) -> Option<DurableFrictionFixEvidence> {
    let start = PathBuf::from(resolved_project_path(project_path));
    find_ancestor_agents_files(&start)
        .into_iter()
        .find_map(|path| durable_friction_instruction_at(path, predicate))
        .or_else(|| {
            global_context_reversal_instruction_candidates()
                .into_iter()
                .find_map(|path| durable_friction_instruction_at(path, predicate))
        })
}

fn durable_friction_instruction_at(
    instruction_path: PathBuf,
    predicate: fn(&str) -> bool,
) -> Option<DurableFrictionFixEvidence> {
    let contents = std::fs::read_to_string(&instruction_path).ok()?;
    if !predicate(&contents) {
        return None;
    }
    let codified_at = std::fs::metadata(&instruction_path)
        .ok()
        .and_then(|metadata| metadata.modified().ok())
        .map(DateTime::<Utc>::from)
        .unwrap_or_else(Utc::now);

    Some(DurableFrictionFixEvidence {
        path: instruction_path.display().to_string(),
        codified_at,
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

fn find_ancestor_agents_files(start: &Path) -> Vec<PathBuf> {
    let mut cursor = if start.is_file() {
        match start.parent() {
            Some(parent) => parent.to_path_buf(),
            None => return Vec::new(),
        }
    } else {
        start.to_path_buf()
    };
    let mut candidates = Vec::new();

    loop {
        let candidate = cursor.join("AGENTS.md");
        if candidate.is_file() {
            candidates.push(candidate);
        }
        if !cursor.pop() {
            break;
        }
    }

    candidates
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

fn instructions_file_codifies_live_runtime(contents: &str) -> bool {
    let lowered = contents.to_ascii_lowercase();
    lowered.contains("verify the live runtime")
        && lowered.contains("not just source code")
        && (lowered.contains("actual user-facing command") || lowered.contains("live path"))
}

fn instructions_file_codifies_outcome_repair(contents: &str) -> bool {
    let lowered = contents.to_ascii_lowercase();
    lowered.contains("prefer outcome fixes")
        && lowered.contains("diagnostic summaries")
        && lowered.contains("implemented and verified repair")
}

fn instructions_file_codifies_checkpoint_resume(contents: &str) -> bool {
    let lowered = contents.to_ascii_lowercase();
    lowered.contains("resume from the last proven checkpoint")
        && lowered.contains("before restarting")
        && lowered.contains("latest usable state")
}

fn instructions_file_codifies_proxy_completion(contents: &str) -> bool {
    let lowered = contents.to_ascii_lowercase();
    lowered.contains("do not accept proxy signals as completion")
        && lowered.contains("supporting evidence")
        && lowered.contains("user-facing requirement")
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
        let last_signal_at = related
            .iter()
            .filter_map(|pattern| pattern.last_observed_at.as_deref())
            .max_by_key(|observed_at| parse_rfc3339_to_utc(observed_at))
            .map(ToString::to_string);
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
            last_signal_at,
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
            last_signal_at: None,
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
            last_signal_at: latest_autonomy_at.map(|value| value.to_rfc3339()),
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
    fn user_prose_friction_surfaces_twenty_active_fixable_points() {
        let prompts = [
            "please pick up exactly where it left off from the last proven checkpoint",
            "the real pipeline is broken; fix it end to end",
            "verify the live runtime and installed binary, not just source code",
            "munin friction is broken, nothing appears despite personal frustration",
            "memory is stale; refresh memory before answering",
            "recall is empty, use session history and git history instead",
            "customer-facing copy should not expose internal worker or queue terms",
            "verify the frontend in the browser with a screenshot",
            "repair this, not a report; make it useful",
            "worktree is dirty, commit and remove the stale worktree",
            "inspect CODEX_HOME and active config before editing config.toml",
            "this Windows PowerShell path and node.exe shim handling is wrong",
            "do not ask should I proceed on obvious next steps",
            "explain this in laymans terms first",
            "does it use firecrawl? answer the implementation-specific question",
            "not done until the completion audit maps evidence to every requirement",
            "daily proactivity is showing already fixed monitoring items",
            "do not revert unrelated changes from the dirty checkout",
            "follow existing patterns and do not add a new dependency",
            "open that exact file path first before a broad scan",
            "wrong scope research into sibling skills is the problem",
            "progress link needs requestId status token and handoff URL",
        ];
        let checkpoints = prompts
            .iter()
            .enumerate()
            .flat_map(|(index, prompt)| {
                let day = index + 1;
                [
                    onboarding_checkpoint(
                        &format!("2026-05-{day:02}T00:00:00Z"),
                        &format!("2026-05-{day:02}T00:00:00Z"),
                        prompt,
                    ),
                    onboarding_checkpoint(
                        &format!("2026-05-{day:02}T00:01:00Z"),
                        &format!("2026-05-{day:02}T00:01:00Z"),
                        &format!("{prompt}; this keeps happening"),
                    ),
                ]
            })
            .collect::<Vec<_>>();

        let fixes = build_memory_os_friction_fixes(
            &[],
            &[],
            &[],
            &crate::core::memory_os::MemoryOsRedirectSummary::default(),
            &checkpoints,
            &UserProseDurableFixes::default(),
        );
        let active_user_prose = fixes
            .iter()
            .filter(|fix| {
                fix.fix_id.starts_with("friction:user-prose:")
                    && fix.status != "codified"
                    && fix.status != "monitoring"
                    && fix.status != "fixed"
                    && fix.status != "retired"
            })
            .count();

        assert!(
            active_user_prose >= 20,
            "expected at least 20 active user-prose friction fixes, got {active_user_prose}"
        );
    }

    #[test]
    fn user_prose_friction_ignores_embedded_instruction_payloads() {
        let checkpoints = vec![
            onboarding_checkpoint(
                "2026-05-01T00:00:00Z",
                "2026-05-01T00:00:00Z",
                "You are the Architect in a ralplan workflow. verify the browser and worktree",
            ),
            onboarding_checkpoint(
                "2026-05-02T00:00:00Z",
                "2026-05-02T00:00:00Z",
                "well then it is broken. I have so much friction in munin that is ready to action",
            ),
        ];

        let fixes = user_prose_actionable_friction_fixes(
            &checkpoints,
            &UserProseDurableFixes::default(),
            Utc::now(),
        );

        assert!(fixes.is_empty());
        assert!(!fixes.iter().any(|fix| {
            fix.evidence
                .iter()
                .any(|line| line.contains("You are the Architect"))
        }));
    }

    #[test]
    fn user_prose_friction_ignores_read_only_verification_sweep_prompts() {
        let codified_at = DateTime::parse_from_rfc3339("2026-05-10T19:49:00Z")
            .expect("timestamp")
            .with_timezone(&Utc);
        let durable_fixes = UserProseDurableFixes {
            checkpoint_resume: Some(DurableFrictionFixEvidence {
                path: "C:/Users/OEM/Projects/AGENTS.md".to_string(),
                codified_at,
            }),
            ..UserProseDurableFixes::default()
        };
        let checkpoints = vec![
            onboarding_checkpoint(
                "2026-05-10T20:00:29Z",
                "2026-05-10T20:00:29Z",
                "Clone-speed clean verification sweep 1, lane 2: read-only orchestration/runtime review of current workspace under C:\\Users\\OEM\\Projects. Focus on last proven checkpoint behavior. Do not edit files.",
            ),
            onboarding_checkpoint(
                "2026-05-10T20:01:29Z",
                "2026-05-10T20:01:29Z",
                "Clone-speed runtime verification sweep A, lane 2: inspect CLI/process/filesystem orchestration and resume from the last proven checkpoint path under C:\\Users\\OEM\\Projects.",
            ),
        ];

        let fixes = user_prose_actionable_friction_fixes(
            &checkpoints,
            &durable_fixes,
            codified_at + Duration::hours(1),
        );

        assert!(
            !fixes
                .iter()
                .any(|fix| fix.fix_id == "friction:user-prose:resume-from-last-checkpoint"),
            "delegated verification sweep prompts should not revive codified checkpoint-resume friction"
        );
    }

    #[test]
    fn checkpoint_resume_friction_does_not_match_bare_resume_language() {
        let checkpoints = vec![
            onboarding_checkpoint(
                "2026-05-01T00:00:00Z",
                "2026-05-01T00:00:00Z",
                "Resume create-native-template tradie lane",
            ),
            onboarding_checkpoint(
                "2026-05-01T00:01:00Z",
                "2026-05-01T00:01:00Z",
                "Resume the template proof run later",
            ),
        ];

        let fixes = user_prose_actionable_friction_fixes(
            &checkpoints,
            &UserProseDurableFixes::default(),
            Utc::now(),
        );

        assert!(!fixes
            .iter()
            .any(|fix| fix.fix_id == "friction:user-prose:resume-from-last-checkpoint"));
    }

    #[test]
    fn checkpoint_resume_friction_keeps_real_verification_sweep_complaints() {
        let checkpoints = vec![
            onboarding_checkpoint(
                "2026-05-01T00:00:00Z",
                "2026-05-01T00:00:00Z",
                "the verification sweep keeps restarting instead of resuming from the last proven checkpoint",
            ),
            onboarding_checkpoint(
                "2026-05-01T00:01:00Z",
                "2026-05-01T00:01:00Z",
                "the verification sweep keeps restarting instead of resuming from the last proven checkpoint; this keeps happening",
            ),
        ];

        let fixes = user_prose_actionable_friction_fixes(
            &checkpoints,
            &UserProseDurableFixes::default(),
            Utc::now(),
        );

        assert!(fixes
            .iter()
            .any(|fix| fix.fix_id == "friction:user-prose:resume-from-last-checkpoint"));
    }

    #[test]
    fn checkpoint_resume_friction_keeps_real_verification_lane_complaints() {
        let checkpoints = vec![
            onboarding_checkpoint(
                "2026-05-01T00:00:00Z",
                "2026-05-01T00:00:00Z",
                "the verification lane keeps restarting instead of resuming from the last proven checkpoint",
            ),
            onboarding_checkpoint(
                "2026-05-01T00:01:00Z",
                "2026-05-01T00:01:00Z",
                "the verification lane keeps restarting instead of resuming from the last proven checkpoint; this keeps happening",
            ),
        ];

        let fixes = user_prose_actionable_friction_fixes(
            &checkpoints,
            &UserProseDurableFixes::default(),
            Utc::now(),
        );

        assert!(fixes
            .iter()
            .any(|fix| fix.fix_id == "friction:user-prose:resume-from-last-checkpoint"));
    }

    #[test]
    fn instruction_payload_filter_keeps_do_not_edit_user_corrections() {
        assert!(!user_prose_friction_text_is_instruction_payload(
            "i said do not edit files and you edited anyway"
        ));
        assert!(user_prose_friction_text_is_instruction_payload(
            "Read-only review task. Do not edit files. Workspace: C:\\Users\\OEM\\Projects"
                .to_ascii_lowercase()
                .as_str()
        ));
    }

    #[test]
    fn user_prose_friction_suppresses_codex_global_autonomy_rules() {
        let checkpoints = vec![
            onboarding_checkpoint(
                "2026-05-01T00:00:00Z",
                "2026-05-01T00:00:00Z",
                "do not ask should I proceed on obvious next steps",
            ),
            onboarding_checkpoint(
                "2026-05-01T00:01:00Z",
                "2026-05-01T00:01:00Z",
                "do not ask should I proceed on obvious next steps; this keeps happening",
            ),
        ];
        let durable_fixes = UserProseDurableFixes {
            codex_autonomy_polling: Some(DurableFrictionFixEvidence {
                path: "C:/Users/OEM/.codex/AGENTS.md".to_string(),
                codified_at: DateTime::parse_from_rfc3339("2026-05-02T00:00:00Z")
                    .expect("timestamp")
                    .with_timezone(&Utc),
            }),
            ..UserProseDurableFixes::default()
        };

        let fixes = user_prose_actionable_friction_fixes(
            &checkpoints,
            &durable_fixes,
            DateTime::parse_from_rfc3339("2026-05-03T00:00:00Z")
                .expect("timestamp")
                .with_timezone(&Utc),
        );

        assert!(!fixes
            .iter()
            .any(|fix| fix.fix_id == "friction:user-prose:do-not-overask-permission"));
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
    fn completed_friction_statuses_follow_stable_identity_until_new_signal() {
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
                evidence: vec![
                    "100 autonomy/polling corrections".to_string(),
                    "changed evidence text without a new signal".to_string(),
                ],
                last_signal_at: Some("2026-05-01T00:00:00Z".to_string()),
                score: 120,
            },
            crate::core::memory_os::MemoryOsFrictionFix {
                fix_id: "friction:autonomy-polling".to_string(),
                title: "Keep autonomous work moving without manual polling".to_string(),
                impact: "medium".to_string(),
                status: "active".to_string(),
                summary: "new signal".to_string(),
                permanent_fix: "poll".to_string(),
                evidence: vec![
                    "100 autonomy/polling corrections".to_string(),
                    "newer correction after completion timestamp".to_string(),
                ],
                last_signal_at: Some("2026-05-03T00:00:00Z".to_string()),
                score: 60,
            },
        ];

        apply_completed_friction_statuses(&mut fixes, &completed);

        assert_eq!(fixes[0].status, "fixed");
        assert_eq!(fixes[1].status, "active");
    }

    #[test]
    fn completed_command_family_status_applies_to_related_friction_ids() {
        let completed = vec![completed_friction_record(
            "friction:cli-syntax-drift",
            &["command guardrail completed".to_string()],
        )];
        let mut fixes = vec![crate::core::memory_os::MemoryOsFrictionFix {
            fix_id: "friction:path-assumption-drift".to_string(),
            title: "Path assumption drift".to_string(),
            impact: "medium".to_string(),
            status: "active".to_string(),
            summary: "old signal".to_string(),
            permanent_fix: "resolve paths".to_string(),
            evidence: vec!["174 examples retained in JSON evidence".to_string()],
            last_signal_at: Some("2026-05-01T00:00:00Z".to_string()),
            score: 120,
        }];

        apply_completed_friction_statuses(&mut fixes, &completed);

        assert_eq!(fixes[0].status, "fixed");
    }

    #[test]
    fn completed_behavior_changes_are_filtered_by_stable_identity() {
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
                evidence: vec![
                    "100 autonomy/polling corrections".to_string(),
                    "changed evidence should not re-open the same behavior fix".to_string(),
                ],
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
            ..UserProseDurableFixes::default()
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
    fn friction_guidance_detectors_recognize_codified_hot_path_rules() {
        let contents = r#"
- Friction fix: Verify the live runtime, not just source code. For CLI, watcher, daemon, browser, automation, or installed-tool fixes, run the actual user-facing command or live path before claiming completion.
- Friction fix: Prefer outcome fixes over diagnostic summaries. When the user reports broken behavior, keep working toward an implemented and verified repair unless they explicitly ask for report-only analysis.
- Friction fix: Resume from the last proven checkpoint. Before restarting, re-specifying, or asking the user for repeated context, recover the latest usable state from memory, session logs, git state, run artifacts, or current workspace evidence.
- Friction fix: Do not accept proxy signals as completion. Passing tests, complete manifests, successful validators, generated reports, or substantial implementation effort are supporting evidence only; verify the explicit user-facing requirement before claiming done.
"#;

        assert!(instructions_file_codifies_live_runtime(contents));
        assert!(instructions_file_codifies_outcome_repair(contents));
        assert!(instructions_file_codifies_checkpoint_resume(contents));
        assert!(instructions_file_codifies_proxy_completion(contents));
    }

    #[test]
    fn durable_friction_detection_scans_ancestor_agents_files() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        std::fs::write(
            root.join("AGENTS.md"),
            r#"
- Friction fix: Verify the live runtime, not just source code. For CLI, watcher, daemon, browser, automation, or installed-tool fixes, run the actual user-facing command or live path before claiming completion.
- Friction fix: Prefer outcome fixes over diagnostic summaries. When the user reports broken behavior, keep working toward an implemented and verified repair unless they explicitly ask for report-only analysis.
- Friction fix: Resume from the last proven checkpoint. Before restarting, re-specifying, or asking the user for repeated context, recover the latest usable state from memory, session logs, git state, run artifacts, or current workspace evidence.
- Friction fix: Do not accept proxy signals as completion. Passing tests, complete manifests, successful validators, generated reports, or substantial implementation effort are supporting evidence only; verify the explicit user-facing requirement before claiming done.
"#,
        )
        .expect("write root agents");
        let project = root.join("project").join("child");
        std::fs::create_dir_all(&project).expect("project dirs");
        std::fs::write(
            root.join("project").join("AGENTS.md"),
            "Project-local instructions without the friction fix rules.",
        )
        .expect("write project agents");

        let durable = detect_user_prose_durable_fixes(Some(&project.display().to_string()));

        assert!(durable.live_runtime_verification.is_some());
        assert!(durable.outcome_repair.is_some());
        assert!(durable.checkpoint_resume.is_some());
        assert!(durable.proxy_completion.is_some());
    }

    #[test]
    fn codified_user_prose_friction_specs_are_suppressed() {
        let codified_at = DateTime::parse_from_rfc3339("2026-05-10T00:00:00Z")
            .expect("timestamp")
            .with_timezone(&Utc);
        let durable = DurableFrictionFixEvidence {
            path: "C:/Users/OEM/Projects/AGENTS.md".to_string(),
            codified_at,
        };
        let durable_fixes = UserProseDurableFixes {
            live_runtime_verification: Some(durable.clone()),
            outcome_repair: Some(durable.clone()),
            checkpoint_resume: Some(durable),
            proxy_completion: Some(DurableFrictionFixEvidence {
                path: "C:/Users/OEM/Projects/AGENTS.md".to_string(),
                codified_at,
            }),
            ..UserProseDurableFixes::default()
        };
        let checkpoints = vec![
            onboarding_checkpoint(
                "2026-05-09T00:00:00Z",
                "2026-05-09T00:00:00Z",
                "verify the live runtime, not just source code",
            ),
            onboarding_checkpoint(
                "2026-05-09T00:01:00Z",
                "2026-05-09T00:01:00Z",
                "verify the live runtime, not just source code; this keeps happening",
            ),
            onboarding_checkpoint(
                "2026-05-09T00:02:00Z",
                "2026-05-09T00:02:00Z",
                "broken behavior needs repair, not a report",
            ),
            onboarding_checkpoint(
                "2026-05-09T00:03:00Z",
                "2026-05-09T00:03:00Z",
                "broken behavior needs repair, not a report; this keeps happening",
            ),
            onboarding_checkpoint(
                "2026-05-09T00:04:00Z",
                "2026-05-09T00:04:00Z",
                "resume from the last proven checkpoint",
            ),
            onboarding_checkpoint(
                "2026-05-09T00:05:00Z",
                "2026-05-09T00:05:00Z",
                "resume from the last proven checkpoint; this keeps happening",
            ),
            onboarding_checkpoint(
                "2026-05-09T00:06:00Z",
                "2026-05-09T00:06:00Z",
                "not done until the completion audit maps evidence to every requirement",
            ),
            onboarding_checkpoint(
                "2026-05-09T00:07:00Z",
                "2026-05-09T00:07:00Z",
                "not done until the completion audit maps evidence to every requirement; this keeps happening",
            ),
        ];

        let fixes = user_prose_actionable_friction_fixes(
            &checkpoints,
            &durable_fixes,
            codified_at + Duration::days(1),
        );

        assert!(!fixes
            .iter()
            .any(|fix| fix.fix_id == "friction:user-prose:verify-live-runtime"));
        assert!(!fixes
            .iter()
            .any(|fix| fix.fix_id == "friction:user-prose:focus-on-user-outcome"));
        assert!(!fixes
            .iter()
            .any(|fix| fix.fix_id == "friction:user-prose:resume-from-last-checkpoint"));
        assert!(!fixes
            .iter()
            .any(|fix| fix.fix_id == "friction:user-prose:avoid-proxy-completion-signals"));
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
            ..UserProseDurableFixes::default()
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
            ..UserProseDurableFixes::default()
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
            ..UserProseDurableFixes::default()
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
    fn autonomy_launcher_instruction_does_not_count_as_friction_correction() {
        let checkpoints = vec![onboarding_checkpoint(
            "2026-04-01T00:00:00Z",
            "2026-04-01T00:00:10Z",
            "You are processing a SiteSorted clone-speed job. This is FULLY AUTONOMOUS: never ask questions, never pause for input.",
        )];

        let counts = count_user_prose_signals(&checkpoints);

        assert_eq!(counts.autonomy, 0);
        assert!(counts.latest_autonomy_at.is_none());
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
            last_observed_at: Some("2026-05-04T00:00:00Z".to_string()),
        }];
        let misunderstandings = build_memory_os_misunderstandings(&patterns);
        let fixes = command_friction_fixes(&patterns, &misunderstandings);

        assert_eq!(fixes.len(), 1);
        assert_eq!(fixes[0].status, "fixed");
        assert_eq!(
            fixes[0].last_signal_at.as_deref(),
            Some("2026-05-04T00:00:00Z")
        );
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
                    last_observed_at: Some(correction.observed_at.to_rfc3339()),
                }
            });
            entry.count += 1;
            let should_update_last_observed = entry
                .last_observed_at
                .as_deref()
                .map(parse_rfc3339_to_utc)
                .map(|last| correction.observed_at > last)
                .unwrap_or(true);
            if should_update_last_observed {
                entry.last_observed_at = Some(correction.observed_at.to_rfc3339());
            }
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
