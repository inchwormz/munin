use anyhow::Result;
use chrono::{DateTime, Utc};
use std::collections::{HashMap, HashSet};

use super::signals::{
    first_non_empty, meaningful_checkpoint_summary, MemoryOsCorrectionObservationRow,
    MemoryOsReplayShellRow, MemoryOsSourceBehaviorAccumulator,
};
use super::{
    compact_display_text, memory_os_repo_label, MemoryOsCheckpointEnvelope, Tracker,
    WORKING_MEMORY_BURY_MIN_AGE_HOURS,
};

const CURRENT_SEMANTIC_ONBOARDING_SCHEMA: &str = "memory-os-session-onboarding-v10";

pub(super) fn build_memory_os_top_projects(
    replay_shells: &[MemoryOsReplayShellRow],
    checkpoints: &[MemoryOsCheckpointEnvelope],
) -> Vec<crate::core::memory_os::MemoryOsProjectSummary> {
    let mut session_sets: HashMap<String, HashSet<(String, String)>> = HashMap::new();
    let mut shell_counts: HashMap<String, usize> = HashMap::new();

    for shell in replay_shells {
        session_sets
            .entry(shell.project_path.clone())
            .or_default()
            .insert((shell.source.clone(), shell.session_id.clone()));
        *shell_counts.entry(shell.project_path.clone()).or_default() += 1;
    }

    for checkpoint in checkpoints
        .iter()
        .filter(|checkpoint| checkpoint.capture.profile == "session-onboarding")
    {
        let packet_id = checkpoint.capture.packet_id.as_str();
        let parts = packet_id.strip_prefix("onboarding-").unwrap_or(packet_id);
        let mut segments = parts.splitn(2, '-');
        let source = segments.next().unwrap_or("unknown").to_string();
        let session_id = segments.next().unwrap_or(packet_id).to_string();
        session_sets
            .entry(checkpoint.project_path.clone())
            .or_default()
            .insert((source, session_id));
    }

    let mut projects = session_sets
        .into_iter()
        .map(
            |(project_path, sessions)| crate::core::memory_os::MemoryOsProjectSummary {
                repo_label: memory_os_repo_label(&project_path),
                project_path: project_path.clone(),
                sessions: sessions.len(),
                shell_executions: shell_counts.remove(&project_path).unwrap_or(0),
            },
        )
        .collect::<Vec<_>>();

    projects.sort_by(|left, right| {
        memory_os_repo_specificity_rank(&right.repo_label)
            .cmp(&memory_os_repo_specificity_rank(&left.repo_label))
            .then(right.shell_executions.cmp(&left.shell_executions))
            .then(right.sessions.cmp(&left.sessions))
            .then(left.repo_label.cmp(&right.repo_label))
    });
    projects.truncate(8);
    projects
}

pub(super) fn checkpoint_should_bury_from_working_memory(
    checkpoint: &MemoryOsCheckpointEnvelope,
) -> bool {
    let age = Utc::now() - checkpoint.captured_at;
    age >= chrono::Duration::hours(WORKING_MEMORY_BURY_MIN_AGE_HOURS)
}

fn memory_os_repo_specificity_rank(repo_label: &str) -> i32 {
    match repo_label {
        label
            if label.contains(".omx")
                || label.contains(".codex")
                || label.contains(".videoclone")
                || label.contains("launch-detached") =>
        {
            -1
        }
        "workspace-root" | "home-root" => 0,
        _ => 1,
    }
}

fn normalize_user_prose_text(text: &str) -> String {
    let compact = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut normalized = compact.trim().trim_matches('"').to_string();

    if normalized.starts_with("[$") {
        if let Some((_, rest)) = normalized.split_once("] ") {
            normalized = rest.trim().to_string();
        } else if let Some(idx) = normalized.find(']') {
            normalized = normalized[idx + 1..].trim().to_string();
        }
    }

    if normalized.starts_with('$') {
        if let Some((token, rest)) = normalized.split_once(' ') {
            if token.len() <= 40 && !rest.trim().is_empty() {
                normalized = rest.trim().to_string();
            }
        }
    }

    if normalized.to_ascii_lowercase().starts_with("team ") {
        if let Some((_, rest)) = normalized.split_once(":executor") {
            normalized = rest
                .trim_start_matches(|ch: char| ch.is_whitespace() || ch == '-' || ch == ':')
                .to_string();
        }
    }

    normalized
}

fn user_prose_text_has_command_noise(text: &str) -> bool {
    let lowered = text.trim().to_ascii_lowercase();
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
        "read c:\\",
        "read c:/",
        "read .omx",
        "run /",
        "list any startup context",
        "leader task:",
        "<task>",
        "codex --dangerously",
        "omx setup",
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
        "context proxy",
        "shell executions",
        "sessions, ",
        "inbox.md",
        "worker-",
        ".omx",
        ".omx2",
        "[omx_tmux_inject]",
        "<run_id>",
        "<deliverable>",
        "execute your assignment",
        "report concrete status",
        "report status + evidence",
        "open the inbox file",
        "status.json",
        "omx_team_state_root",
    ];
    command_markers
        .iter()
        .any(|needle| lowered.contains(needle))
}

fn focused_user_prose_summary(text: &str) -> String {
    let compact = compact_display_text(text, 420);
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

    if selected.is_empty() {
        compact_display_text(text, 320)
    } else {
        compact_display_text(&selected.join(". "), 320)
    }
}

fn meaningful_user_prose_summary(text: &str) -> Option<String> {
    let normalized = normalize_user_prose_text(text);
    let trimmed = normalized.trim();
    if trimmed.is_empty() || user_prose_text_has_command_noise(trimmed) {
        return None;
    }
    let word_count = trimmed.split_whitespace().count();
    if word_count < 5 {
        return None;
    }
    let lowered = trimmed.to_ascii_lowercase();
    let generic_questions = [
        "what do you know about me",
        "what you know about me",
        "how do i like to work",
        "what am i working on",
        "what are the next best steps",
        "what should we do next",
        "what we should be doing",
        "as much detail as you want",
        "so i want to see what you know",
    ];
    if generic_questions
        .iter()
        .any(|question| lowered.contains(question))
    {
        return None;
    }
    Some(focused_user_prose_summary(trimmed))
}

fn user_prose_score(summary: &str) -> i64 {
    let lowered = summary.to_ascii_lowercase();
    let mut score = summary.split_whitespace().count().min(30) as i64;

    for (needle, weight) in [
        ("i want", 8),
        ("i need", 8),
        ("i think", 6),
        ("i prefer", 8),
        ("i don't want", 10),
        ("i dont want", 10),
        ("i'm asking", 7),
        ("im asking", 7),
        ("the point is", 6),
        ("the goal is", 7),
        ("this needs", 8),
        ("needs to", 6),
        ("should", 4),
        ("must", 5),
        ("actually", 3),
        ("rather than", 4),
    ] {
        if lowered.contains(needle) {
            score += weight;
        }
    }

    for (needle, weight) in [
        ("don't", 5),
        ("dont", 5),
        ("don't tell me", 12),
        ("dont tell me", 12),
        ("stop", 6),
        ("ban", 6),
        ("what the hell are you doing", 16),
        ("what did you just do", 14),
        ("why would you do that", 14),
        ("what are you doing", 10),
        ("wtf", 14),
        ("omfg", 12),
        ("omg", 8),
        ("fuck", 12),
        ("useless", 9),
        ("useful", 7),
        ("garbage", 7),
        ("prose", 8),
        ("command noise", 10),
        ("noise", 5),
        ("raw command", 7),
        ("surface", 5),
        ("memory os", 5),
        ("brief", 4),
        ("inspect", 4),
        ("working on", 4),
    ] {
        if lowered.contains(needle) {
            score += weight;
        }
    }

    for (needle, weight) in [
        ("good job", 5),
        ("nice one", 5),
        ("thank you", 4),
        ("thanks", 3),
        ("perfect", 4),
        ("great", 3),
        ("that's better", 5),
        ("thats better", 5),
        ("much better", 5),
        ("works now", 4),
    ] {
        if lowered.contains(needle) {
            score += weight;
        }
    }

    score += lowered.matches('!').count().min(3) as i64 * 2;
    if lowered.contains("?!") || lowered.contains("!?") {
        score += 4;
    }
    score
}

fn user_prose_title(summary: &str) -> String {
    let lowered = summary.to_ascii_lowercase();
    if lowered.contains("what the hell")
        || lowered.contains("what did you just do")
        || lowered.contains("why would you do that")
        || lowered.contains("wtf")
        || lowered.contains("omfg")
        || lowered.contains("fuck")
        || lowered.contains("useless")
        || lowered.contains("garbage")
        || lowered.contains("command noise")
    {
        return "Frustration signal".to_string();
    }
    if lowered.contains("good job")
        || lowered.contains("nice one")
        || lowered.contains("thank you")
        || lowered.contains("much better")
        || lowered.contains("that's better")
        || lowered.contains("thats better")
    {
        return "Positive feedback".to_string();
    }
    if lowered.contains("site sorted")
        || lowered.contains("sitesorted")
        || lowered.contains(" on gate")
        || lowered.contains("everything should go to site")
    {
        return "SiteSorted focus".to_string();
    }
    if lowered.contains("lead database")
        || lowered.contains("builders")
        || lowered.contains("plumbers")
        || lowered.contains("electricians")
        || lowered.contains("small businesses")
        || lowered.contains("sales")
        || lowered.contains("outreach")
    {
        return "Lead generation strategy".to_string();
    }
    if lowered.contains("business strategy")
        || lowered.contains("kpi")
        || lowered.contains("kpis")
        || lowered.contains("opsp")
        || lowered.contains("bhag")
        || lowered.contains("paying customers")
        || lowered.contains("annual goal")
    {
        return "Business strategy".to_string();
    }
    if lowered.contains("unfinished")
        || lowered.contains("working on")
        || lowered.contains("upcoming")
        || lowered.contains("pickup plan")
        || lowered.contains("next fix")
        || lowered.contains("next task")
        || lowered.contains("continue fixing")
        || lowered.contains("fixing issues")
        || lowered.contains("work on it if")
        || lowered.contains("work on it until")
        || lowered.contains("work on this until")
        || lowered.contains("this isn't done")
        || lowered.contains("this isnt done")
        || lowered.contains("not done until")
        || lowered.contains("still not returning")
        || lowered.contains("useful pertinent information")
        || lowered.contains("please review and inspect")
        || lowered.contains("goal is to get this process working seamlessly today")
        || lowered.contains("process working seamlessly today")
        || lowered.contains("always-on prompt mass")
    {
        return "Current work".to_string();
    }
    if lowered.contains("memory os")
        || lowered.contains("memoryos")
        || lowered.contains("brief")
        || lowered.contains("startup")
        || lowered.contains("inspect")
    {
        return "Memory OS direction".to_string();
    }
    if lowered.contains("look of it to change")
        || lowered.contains("functional changes")
        || lowered.contains("i don't want the look")
        || lowered.contains("i dont want the look")
    {
        return "Product constraint".to_string();
    }
    if lowered.contains("i prefer")
        || lowered.contains("i don't want")
        || lowered.contains("i dont want")
        || lowered.contains("i want")
        || lowered.contains("i need")
        || lowered.contains("must")
        || lowered.contains("should")
    {
        return "Working preference".to_string();
    }
    "Session signal".to_string()
}

fn user_prose_title_priority(title: &str) -> i32 {
    match title {
        "Current work" => 0,
        "Business strategy" => 1,
        "Lead generation strategy" => 2,
        "SiteSorted focus" => 3,
        "Memory OS direction" => 4,
        "Working preference" => 5,
        "Product constraint" => 6,
        "Frustration signal" => 7,
        "Positive feedback" => 8,
        _ => 9,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::memory_os::{MemoryOsCheckpointCapture, MemoryOsCheckpointReentry};

    #[test]
    fn user_prose_score_prioritizes_frustration_and_correction_signals() {
        let neutral = "Please review the next step carefully before changing the code again.";
        let frustrated = "What the hell are you doing? Don't tell me this command noise is useful!";

        assert!(user_prose_score(frustrated) > user_prose_score(neutral));
        assert!(meaningful_user_prose_summary("Fuck!").is_none());
    }

    #[test]
    fn user_prose_score_keeps_positive_feedback_as_a_weaker_signal() {
        let neutral = "Please review the next step carefully before changing the code again.";
        let positive = "Good job, nice one, thank you, that was much better than before.";
        let frustrated = "WTF, why would you do that? Stop showing useless command noise!";

        assert!(user_prose_score(positive) > user_prose_score(neutral));
        assert!(user_prose_score(frustrated) > user_prose_score(positive));
    }

    #[test]
    fn user_prose_summary_filters_command_noise() {
        assert!(meaningful_user_prose_summary(
            "context proxy powershell -NoProfile -Command cargo test"
        )
        .is_none());
        assert!(meaningful_user_prose_summary(
            "Read C:\\Users\\OEM\\Projects\\sitesorted\\.omx2\\codex-state\\team\\x\\workers\\worker-1\\inbox.md and execute your assignment."
        )
        .is_none());
        assert_eq!(
            meaningful_user_prose_summary(
                "[$prompt-master] I need you to undo the font change and keep the layout stable."
            ),
            Some("I need you to undo the font change and keep the layout stable".to_string())
        );
        assert!(meaningful_user_prose_summary(
            "I want the brief to show prose instead of raw command noise."
        )
        .is_some());
    }

    #[test]
    fn user_prose_title_maps_signals_to_useful_sections() {
        assert_eq!(
            user_prose_title("I want it on Gate. I just want it on Site Sorted."),
            "SiteSorted focus"
        );
        assert_eq!(
            user_prose_title("What the hell are you doing? This is useless command noise."),
            "Frustration signal"
        );
        assert_eq!(
            user_prose_title("Good job, nice one, that was much better."),
            "Positive feedback"
        );
        assert_eq!(
            user_prose_title("okay please commit the unfinished work, then start pickup plan"),
            "Current work"
        );
        assert_eq!(
            user_prose_title(
                "please review and inspect memory os output and work on it if its still not returning good data"
            ),
            "Current work"
        );
        assert_ne!(
            user_prose_title("we havent deleted 33k lines of code today"),
            "Current work"
        );
        assert!(meaningful_user_prose_summary(
            "Tell me what you know about me and how I like to work."
        )
        .is_none());
    }

    #[test]
    fn build_memory_os_user_prose_findings_prefers_title_diversity() {
        let checkpoint =
            |packet_id: &str, generated_at: &str, goal: &str| MemoryOsCheckpointEnvelope {
                project_path: "C:/repo".to_string(),
                captured_at: DateTime::parse_from_rfc3339(generated_at)
                    .expect("timestamp")
                    .with_timezone(&Utc),
                capture: MemoryOsCheckpointCapture {
                    packet_id: packet_id.to_string(),
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
                    reentry: MemoryOsCheckpointReentry {
                        recommended_command: "munin resume --format prompt".to_string(),
                        current_recommendation: None,
                        first_question: "What still matters?".to_string(),
                        first_verification: "Verify the next step.".to_string(),
                    },
                },
            };

        let findings = build_memory_os_user_prose_findings(
            &[
                checkpoint(
                    "a",
                    "2026-04-17T00:00:00Z",
                    "I don't want to build something just for this benchmark. I want the broad Memory OS tool.",
                ),
                checkpoint(
                    "b",
                    "2026-04-17T01:00:00Z",
                    "I want it on Gate. I just want it on Site Sorted.",
                ),
                checkpoint(
                    "c",
                    "2026-04-17T02:00:00Z",
                    "okay please commit the unfinished work, then start pickup plan",
                ),
            ],
            3,
        );

        assert!(findings
            .iter()
            .any(|finding| finding.title == "Memory OS direction"));
        assert!(findings
            .iter()
            .any(|finding| finding.title == "SiteSorted focus"));
        assert!(findings
            .iter()
            .any(|finding| finding.title == "Current work"));
    }
}

pub(super) fn build_memory_os_user_prose_findings(
    checkpoints: &[MemoryOsCheckpointEnvelope],
    limit: usize,
) -> Vec<crate::core::memory_os::MemoryOsNarrativeFinding> {
    let mut candidates: Vec<(
        i64,
        DateTime<Utc>,
        crate::core::memory_os::MemoryOsNarrativeFinding,
    )> = Vec::new();

    for checkpoint in checkpoints
        .iter()
        .filter(|checkpoint| checkpoint.capture.profile == "session-onboarding")
    {
        let repo_label = memory_os_repo_label(&checkpoint.project_path);
        let prompt_texts = checkpoint
            .capture
            .selected_items
            .iter()
            .filter(|item| item.section == "user_prompts")
            .map(|item| item.summary.as_str())
            .chain(checkpoint.capture.goal.iter().map(|value| value.as_str()))
            .chain(
                checkpoint
                    .capture
                    .reentry
                    .current_recommendation
                    .iter()
                    .map(|value| value.as_str()),
            );

        for text in prompt_texts {
            let Some(summary) = meaningful_user_prose_summary(text) else {
                continue;
            };
            candidates.push((
                user_prose_score(&summary),
                checkpoint.captured_at,
                crate::core::memory_os::MemoryOsNarrativeFinding {
                    title: user_prose_title(&summary),
                    summary,
                    evidence: vec![
                        format!(
                            "user prompt checkpoint at {}",
                            checkpoint.capture.generated_at
                        ),
                        format!("project: {}", repo_label),
                    ],
                },
            ));
        }
    }

    candidates.sort_by(|left, right| {
        right
            .0
            .cmp(&left.0)
            .then(right.1.cmp(&left.1))
            .then(left.2.summary.cmp(&right.2.summary))
    });

    let mut diversified = candidates.clone();
    diversified.sort_by(|left, right| {
        user_prose_title_priority(left.2.title.as_str())
            .cmp(&user_prose_title_priority(right.2.title.as_str()))
            .then_with(|| {
                if left.2.title == "Current work" && right.2.title == "Current work" {
                    right.1.cmp(&left.1).then(right.0.cmp(&left.0))
                } else {
                    right.0.cmp(&left.0).then(right.1.cmp(&left.1))
                }
            })
            .then(left.2.summary.cmp(&right.2.summary))
    });

    let mut findings = Vec::new();
    let mut seen = HashSet::new();
    let mut seen_titles = HashSet::new();
    for (_, _, finding) in diversified {
        let key = finding.summary.to_ascii_lowercase();
        if seen.insert(key) && seen_titles.insert(finding.title.clone()) {
            findings.push(finding);
            if findings.len() >= limit {
                return findings;
            }
        }
    }

    for (_, _, finding) in candidates {
        let key = finding.summary.to_ascii_lowercase();
        if seen.insert(key) {
            findings.push(finding);
            if findings.len() >= limit {
                break;
            }
        }
    }
    findings
}

pub(super) fn build_memory_os_semantic_fact_findings(
    checkpoints: &[MemoryOsCheckpointEnvelope],
    limit: usize,
) -> Vec<crate::core::memory_os::MemoryOsNarrativeFinding> {
    let mut candidates: Vec<(
        i64,
        DateTime<Utc>,
        crate::core::memory_os::MemoryOsNarrativeFinding,
    )> = Vec::new();

    for checkpoint in checkpoints.iter().filter(|checkpoint| {
        checkpoint.capture.profile == "session-onboarding"
            && checkpoint
                .capture
                .packet_id
                .contains(CURRENT_SEMANTIC_ONBOARDING_SCHEMA)
    }) {
        let repo_label = memory_os_repo_label(&checkpoint.project_path);
        for item in &checkpoint.capture.selected_items {
            let Some(title) = semantic_item_title(item.section.as_str(), item.kind.as_str()) else {
                continue;
            };
            let Some(summary) = meaningful_user_prose_summary(&item.summary) else {
                continue;
            };
            candidates.push((
                item.score,
                checkpoint.captured_at,
                crate::core::memory_os::MemoryOsNarrativeFinding {
                    title: title.to_string(),
                    summary,
                    evidence: vec![
                        format!("semantic checkpoint at {}", checkpoint.capture.generated_at),
                        format!("project: {}", repo_label),
                    ],
                },
            ));
        }
    }

    candidates.sort_by(|left, right| {
        user_prose_title_priority(left.2.title.as_str())
            .cmp(&user_prose_title_priority(right.2.title.as_str()))
            .then_with(|| {
                if left.2.title == "Current work" && right.2.title == "Current work" {
                    right.1.cmp(&left.1).then(right.0.cmp(&left.0))
                } else {
                    right.0.cmp(&left.0).then(right.1.cmp(&left.1))
                }
            })
            .then(left.2.summary.cmp(&right.2.summary))
    });

    let mut findings = Vec::new();
    let mut seen = HashSet::new();
    let mut seen_titles = HashSet::new();
    for (_, _, finding) in candidates {
        let key = finding.summary.to_ascii_lowercase();
        if seen.insert(key) && seen_titles.insert(finding.title.clone()) {
            findings.push(finding);
            if findings.len() >= limit {
                break;
            }
        }
    }
    findings
}

fn semantic_item_title(section: &str, kind: &str) -> Option<&'static str> {
    match (section, kind) {
        ("user_active_work", _) | (_, "current-work") => Some("Current work"),
        ("user_strategy_facts", _) | (_, "business-strategy") => Some("Business strategy"),
        ("user_project_facts", _) | (_, "project-focus") => Some("Project focus"),
        ("user_work_style", _) | (_, "working-preference") => Some("Working preference"),
        ("user_product_constraints", _) | (_, "product-constraint") => Some("Product constraint"),
        ("user_memory_requirements", _) | (_, "memory-os-direction") => Some("Memory OS direction"),
        _ => None,
    }
}

pub(super) fn build_memory_os_active_work(
    checkpoints: &[MemoryOsCheckpointEnvelope],
    _replay_shells: &[MemoryOsReplayShellRow],
    top_projects: &[crate::core::memory_os::MemoryOsProjectSummary],
) -> Vec<crate::core::memory_os::MemoryOsNarrativeFinding> {
    let mut latest_by_project: HashMap<String, &MemoryOsCheckpointEnvelope> = HashMap::new();
    for checkpoint in checkpoints
        .iter()
        .filter(|checkpoint| checkpoint.capture.profile != "session-onboarding")
    {
        let entry = latest_by_project
            .entry(checkpoint.project_path.clone())
            .or_insert(checkpoint);
        if checkpoint.captured_at > entry.captured_at {
            *entry = checkpoint;
        }
    }

    let mut findings = latest_by_project
        .into_values()
        .filter(|checkpoint| !checkpoint_should_bury_from_working_memory(checkpoint))
        .filter_map(|checkpoint| {
            let repo_label = memory_os_repo_label(&checkpoint.project_path);
            let summary = first_non_empty(&[
                checkpoint
                    .capture
                    .reentry
                    .current_recommendation
                    .as_deref()
                    .and_then(meaningful_checkpoint_summary),
                checkpoint
                    .capture
                    .goal
                    .as_deref()
                    .and_then(meaningful_checkpoint_summary),
                checkpoint
                    .capture
                    .selected_items
                    .iter()
                    .find(|item| item.section == "open_obligations")
                    .and_then(|item| meaningful_checkpoint_summary(&item.summary)),
                checkpoint
                    .capture
                    .selected_items
                    .iter()
                    .find(|item| item.section == "current_failures")
                    .and_then(|item| meaningful_checkpoint_summary(&item.summary)),
            ])?;
            let mut evidence = vec![format!(
                "{} checkpoint at {}",
                checkpoint.capture.preset, checkpoint.capture.generated_at
            )];
            if let Some(goal) = checkpoint.capture.goal.as_deref() {
                if let Some(goal) = meaningful_checkpoint_summary(goal) {
                    evidence.push(format!("goal: {}", goal));
                }
            }
            for open_loop in checkpoint
                .capture
                .selected_items
                .iter()
                .filter(|item| item.section == "open_obligations")
                .take(2)
            {
                if let Some(summary) = meaningful_checkpoint_summary(&open_loop.summary) {
                    evidence.push(format!("open obligation: {}", summary));
                }
            }
            Some(crate::core::memory_os::MemoryOsNarrativeFinding {
                title: repo_label,
                summary,
                evidence,
            })
        })
        .collect::<Vec<_>>();

    findings.sort_by(|left, right| left.title.cmp(&right.title));
    findings.truncate(5);
    findings.sort_by(|left, right| {
        let left_label = left.title.as_str();
        let right_label = right.title.as_str();
        let left_sessions = top_projects
            .iter()
            .find(|project| project.repo_label == left_label)
            .map(|project| project.sessions)
            .unwrap_or_default();
        let right_sessions = top_projects
            .iter()
            .find(|project| project.repo_label == right_label)
            .map(|project| project.sessions)
            .unwrap_or_default();
        memory_os_repo_specificity_rank(right_label)
            .cmp(&memory_os_repo_specificity_rank(left_label))
            .then_with(|| right_sessions.cmp(&left_sessions))
            .then_with(|| left.title.cmp(&right.title))
    });

    let mut deduped = Vec::new();
    let mut seen = HashSet::new();
    for finding in findings {
        let key = finding.summary.to_ascii_lowercase();
        if seen.insert(key) {
            deduped.push(finding);
        }
    }

    if !deduped.is_empty() {
        return deduped;
    }

    top_projects
        .iter()
        .take(3)
        .map(|project| crate::core::memory_os::MemoryOsNarrativeFinding {
            title: project.repo_label.clone(),
            summary: "Recent work still clusters here, even though the latest checkpoints do not yet name a concrete task."
                .to_string(),
            evidence: vec![project.project_path.clone()],
        })
        .collect()
}

fn continuity_candidate_score(text: &str) -> i64 {
    let lowered = text.to_ascii_lowercase();
    let mut score = 0i64;

    let markers = [
        ("office hours", 6),
        ("dog food", 6),
        ("dogfood", 6),
        ("memoryos", 5),
        ("memory os", 5),
        ("promised", 5),
        ("next session", 7),
        ("next sessio", 6),
        ("we'll do it tomorrow", 8),
        ("well do it tomorrow", 8),
        ("i'll pick this up later", 8),
        ("ill pick this up later", 8),
        ("pick this up later", 7),
        ("pick it up", 6),
        ("pick up", 4),
        ("first thing", 5),
        ("tomorrow", 4),
        ("resume", 3),
        ("continue", 2),
        ("proof", 3),
        ("cleanly", 2),
    ];

    for (needle, value) in markers {
        if lowered.contains(needle) {
            score += value;
        }
    }

    if lowered.contains("what do you know about me")
        || lowered.contains("how i like to work")
        || lowered.contains("what am i working on")
        || lowered.contains("what should we do next")
        || lowered.contains("what we should do next")
    {
        score -= 8;
    }

    score
}

fn continuity_summary_from_text(text: &str) -> String {
    let lowered = text.to_ascii_lowercase();
    if lowered.contains("office hours")
        && (lowered.contains("dog food")
            || lowered.contains("dogfood")
            || lowered.contains("memoryos")
            || lowered.contains("memory os"))
    {
        return "Resume the office-hours dogfood thread and use it as proof that Memory OS can pick work up cleanly across sessions.".to_string();
    }
    if lowered.contains("pick it up")
        || lowered.contains("first thing")
        || lowered.contains("tomorrow")
    {
        return compact_display_text(text, 180);
    }
    compact_display_text(text, 180)
}

pub(super) fn correction_pattern_total_count(
    patterns: &[crate::core::memory_os::MemoryOsCorrectionPatternSummary],
) -> usize {
    patterns.iter().map(|pattern| pattern.count).sum()
}

pub(super) fn build_memory_os_source_behavior(
    imported_sources: &[crate::core::memory_os::MemoryOsImportedSourceSummary],
    corrections: &[MemoryOsCorrectionObservationRow],
) -> Vec<crate::core::memory_os::MemoryOsSourceBehaviorSummary> {
    let mut by_source: HashMap<String, MemoryOsSourceBehaviorAccumulator> = HashMap::new();
    for source in imported_sources {
        by_source.insert(
            source.source.clone(),
            MemoryOsSourceBehaviorAccumulator {
                source: source.source.clone(),
                sessions: source.sessions,
                shell_executions: source.shell_executions,
                corrections: 0,
            },
        );
    }
    for correction in corrections {
        let entry = by_source
            .entry(correction.source.clone())
            .or_insert_with(|| MemoryOsSourceBehaviorAccumulator {
                source: correction.source.clone(),
                ..Default::default()
            });
        entry.corrections += 1;
    }

    let mut summaries = by_source
        .into_values()
        .map(|source| {
            let shells_per_session = if source.sessions > 0 {
                source.shell_executions as f64 / source.sessions as f64
            } else {
                0.0
            };
            let corrections_per_100_shells = if source.shell_executions > 0 {
                source.corrections as f64 * 100.0 / source.shell_executions as f64
            } else {
                0.0
            };
            crate::core::memory_os::MemoryOsSourceBehaviorSummary {
                source: source.source,
                sessions: source.sessions,
                shell_executions: source.shell_executions,
                corrections: source.corrections,
                redirects: 0,
                redirected_sessions: 0,
                successful_redirects: 0,
                shells_per_session,
                corrections_per_100_shells,
                redirects_per_session: 0.0,
                avg_commands_to_success_after_redirect: None,
                avg_seconds_to_success_after_redirect: None,
            }
        })
        .collect::<Vec<_>>();

    summaries.sort_by(|left, right| {
        right
            .sessions
            .cmp(&left.sessions)
            .then(right.shell_executions.cmp(&left.shell_executions))
            .then(left.source.cmp(&right.source))
    });
    summaries
}

pub(super) fn build_memory_os_preferences(
    user_prose: &[crate::core::memory_os::MemoryOsNarrativeFinding],
) -> Vec<crate::core::memory_os::MemoryOsNarrativeFinding> {
    user_prose
        .iter()
        .filter(|finding| {
            matches!(
                finding.title.as_str(),
                "Working preference" | "Product constraint" | "Positive feedback"
            )
        })
        .cloned()
        .take(5)
        .collect()
}

pub(super) fn build_memory_os_operating_style(
    _by_source: &[crate::core::memory_os::MemoryOsSourceBehaviorSummary],
) -> Vec<crate::core::memory_os::MemoryOsNarrativeFinding> {
    Vec::new()
}

pub(super) fn build_memory_os_autonomy_findings(
    _by_source: &[crate::core::memory_os::MemoryOsSourceBehaviorSummary],
    _active_work: &[crate::core::memory_os::MemoryOsNarrativeFinding],
) -> Vec<crate::core::memory_os::MemoryOsNarrativeFinding> {
    Vec::new()
}

pub(super) fn build_memory_os_epistemic_findings(
    _correction_patterns: &[crate::core::memory_os::MemoryOsCorrectionPatternSummary],
) -> Vec<crate::core::memory_os::MemoryOsNarrativeFinding> {
    Vec::new()
}

pub(super) fn build_memory_os_recurring_themes(
    top_projects: &[crate::core::memory_os::MemoryOsProjectSummary],
    user_prose: &[crate::core::memory_os::MemoryOsNarrativeFinding],
    _active_work: &[crate::core::memory_os::MemoryOsNarrativeFinding],
) -> Vec<crate::core::memory_os::MemoryOsNarrativeFinding> {
    let mut themes = user_prose
        .iter()
        .filter(|finding| {
            matches!(
                finding.title.as_str(),
                "Memory OS direction"
                    | "Business strategy"
                    | "Lead generation strategy"
                    | "SiteSorted focus"
                    | "Current work"
            )
        })
        .cloned()
        .collect::<Vec<_>>();

    if !themes.is_empty() {
        themes.truncate(5);
        return themes;
    }

    top_projects
        .iter()
        .filter(|project| memory_os_repo_specificity_rank(&project.repo_label) > 0)
        .take(5)
        .map(|project| crate::core::memory_os::MemoryOsNarrativeFinding {
            title: project.repo_label.clone(),
            summary: format!(
                "{} sessions, {} shell executions",
                project.sessions, project.shell_executions
            ),
            evidence: vec![project.project_path.clone()],
        })
        .collect()
}

impl Tracker {
    pub fn get_memory_os_continuity_findings(
        &self,
        scope: crate::core::memory_os::MemoryOsInspectionScope,
        project_path: Option<&str>,
    ) -> Result<Vec<crate::core::memory_os::MemoryOsNarrativeFinding>> {
        let checkpoints = self.load_memory_os_checkpoint_captures(scope, project_path)?;
        let mut candidates: Vec<(
            i64,
            DateTime<Utc>,
            crate::core::memory_os::MemoryOsNarrativeFinding,
        )> = Vec::new();

        for checkpoint in checkpoints
            .iter()
            .filter(|checkpoint| checkpoint.capture.profile != "session-onboarding")
        {
            if checkpoint_should_bury_from_working_memory(checkpoint) {
                continue;
            }
            let repo_label = memory_os_repo_label(&checkpoint.project_path);
            let texts = checkpoint
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
                        .filter(|item| item.section == "open_obligations")
                        .map(|item| item.summary.as_str()),
                );

            for text in texts {
                let score = continuity_candidate_score(text);
                if score < 7 {
                    continue;
                }
                let summary = continuity_summary_from_text(text);
                let evidence = vec![
                    format!(
                        "{} checkpoint at {}",
                        checkpoint.capture.preset, checkpoint.capture.generated_at
                    ),
                    format!("project: {}", repo_label),
                ];
                candidates.push((
                    score,
                    checkpoint.captured_at,
                    crate::core::memory_os::MemoryOsNarrativeFinding {
                        title: "Continuity commitment".to_string(),
                        summary,
                        evidence,
                    },
                ));
            }
        }

        candidates.sort_by(|left, right| {
            right
                .0
                .cmp(&left.0)
                .then(right.1.cmp(&left.1))
                .then(left.2.summary.cmp(&right.2.summary))
        });

        let mut findings = Vec::new();
        for (_, _, finding) in candidates {
            if findings.iter().any(
                |existing: &crate::core::memory_os::MemoryOsNarrativeFinding| {
                    existing.summary == finding.summary
                },
            ) {
                continue;
            }
            findings.push(finding);
            if findings.len() >= 3 {
                break;
            }
        }

        Ok(findings)
    }
}
