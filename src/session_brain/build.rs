use super::evidence::{build_session_focus, SessionEvidence, SessionFocus};
use super::guidance::build_guidance;
use super::messages::{
    load_context_snapshot_messages, read_current_session_messages, SessionMessages,
};
use super::project::build_project_context;
use super::strategy::build_strategy_context;
use super::types::{
    SessionBrain, SessionBrainAgenda, SessionBrainMessageGroup, SessionBrainMeta,
    SessionBrainSignal, SessionBrainState, SessionBrainStrategyContext,
};
use super::user::build_user_context;
use crate::core::tracking::Tracker;
use crate::core::utils::{current_project_root_string, detect_project_root, truncate};
use crate::core::worldview::{CompiledContext, ContextClaim, ContextFact, FailureFact};
use anyhow::Result;
use chrono::Utc;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default)]
pub struct SessionBrainBuildOptions {
    pub explicit_goal: Option<String>,
    pub allow_session_fallback: bool,
}

#[derive(Debug, Clone, Default)]
pub struct SessionBrainCompiledInput {
    pub project_path: String,
    pub current_state: Vec<ContextFact>,
    pub live_claims: Vec<ContextClaim>,
    pub open_obligations: Vec<ContextClaim>,
}

impl From<&CompiledContext> for SessionBrainCompiledInput {
    fn from(value: &CompiledContext) -> Self {
        Self {
            project_path: value.project_path.clone(),
            current_state: value.current_state.clone(),
            live_claims: value.live_claims.clone(),
            open_obligations: value.open_obligations.clone(),
        }
    }
}

#[derive(Debug, Clone, Default)]
struct TaskPath {
    paths: Vec<String>,
    terms: Vec<String>,
}

impl TaskPath {
    fn is_empty(&self) -> bool {
        self.paths.is_empty() && self.terms.is_empty()
    }

    fn add_hint(&mut self, hint: &str) {
        if looks_like_path_hint(hint) {
            let normalized = normalize_hint_path(hint);
            if !normalized.is_empty() && !self.paths.contains(&normalized) {
                self.paths.push(normalized);
            }
        }
        self.add_text(hint);
    }

    fn add_text(&mut self, text: &str) {
        for term in extract_match_terms(text) {
            if !self.terms.contains(&term) {
                self.terms.push(term);
            }
        }
    }

    fn matches_text(&self, text: &str) -> bool {
        let lowered = text.to_ascii_lowercase();
        if self
            .paths
            .iter()
            .any(|path| lowered.contains(&path.to_ascii_lowercase()))
        {
            return true;
        }
        let overlap = self
            .terms
            .iter()
            .filter(|term| lowered.contains(term.as_str()))
            .count();
        overlap >= required_overlap(self.terms.len())
    }
}

pub fn build_session_brain(
    tracker: &Tracker,
    compiled: &SessionBrainCompiledInput,
    failures: &[FailureFact],
    options: &SessionBrainBuildOptions,
) -> Result<SessionBrain> {
    let project_root = if compiled.project_path.is_empty() {
        PathBuf::from(current_project_root_string())
    } else {
        PathBuf::from(&compiled.project_path)
    };
    let messages = read_current_session_messages(&project_root, options.allow_session_fallback)?;
    let mut focus_user_messages = messages.user.clone();
    focus_user_messages.extend(load_context_snapshot_messages(
        &project_root,
        &messages.user,
    )?);
    let focus = build_session_focus(&focus_user_messages, &messages.assistant);
    let effective_project_root = infer_effective_project_root(&project_root, &messages);
    let user = build_user_context(tracker);
    let mut strategy = build_strategy_context(&effective_project_root).unwrap_or_else(|_| {
        SessionBrainStrategyContext {
            summary: Vec::new(),
            source_paths: Vec::new(),
            planning_complete: false,
        }
    });
    if strategy.summary.is_empty() {
        strategy.summary = partial_strategy_from_user_context(&user);
    }
    let guidance = build_guidance(&user);
    let agenda = build_agenda(compiled, &focus, &strategy.summary, options);
    let task_path = build_task_path(agenda.current_goal.as_deref(), &focus, compiled);
    let state = build_state(
        compiled,
        failures,
        &focus,
        &task_path,
        agenda.current_goal.as_deref(),
    );
    let project = build_project_context(
        &effective_project_root,
        agenda.current_goal.as_deref(),
        &focus.ordered_task_hints(),
    )?;
    let cwd = std::env::current_dir()
        .ok()
        .map(|path| path.to_string_lossy().to_string())
        .unwrap_or_default();

    Ok(SessionBrain {
        meta: SessionBrainMeta {
            session_id: messages.session_id.clone(),
            cwd,
            project_root: effective_project_root.to_string_lossy().to_string(),
            built_at: Utc::now().to_rfc3339(),
            version: 1,
            provider: messages.provider,
            transcript_source_path: messages.transcript_path.clone(),
            transcript_modified_at: messages.transcript_modified_at.clone(),
            source_status: messages.source_status.clone(),
        },
        messages: SessionBrainMessageGroup {
            user: messages.user,
            assistant: messages.assistant,
        },
        agenda,
        state,
        project,
        strategy,
        user,
        guidance,
    })
}

fn infer_effective_project_root(default_root: &Path, messages: &SessionMessages) -> PathBuf {
    let default_root = detect_project_root(default_root);
    let mut roots = HashMap::<PathBuf, (usize, String, usize)>::new();

    for message in messages.user.iter().chain(messages.assistant.iter()) {
        let Some(cwd) = message.cwd.as_deref() else {
            continue;
        };
        let inferred = detect_project_root(Path::new(cwd));
        if !inferred.exists() {
            continue;
        }
        let timestamp = message.timestamp.clone().unwrap_or_default();
        let entry = roots
            .entry(inferred)
            .or_insert_with(|| (0, String::new(), 0));
        entry.0 += 1;
        if timestamp > entry.1 || (timestamp == entry.1 && message.line_number > entry.2) {
            entry.1 = timestamp;
            entry.2 = message.line_number;
        }
    }

    if roots.is_empty() {
        return default_root;
    }

    let child_roots = roots
        .iter()
        .filter(|(path, _)| *path != &default_root && path.starts_with(&default_root))
        .map(|(path, value)| (path.clone(), value.clone()))
        .collect::<Vec<_>>();
    if child_roots.is_empty() {
        return default_root;
    }

    child_roots
        .into_iter()
        .max_by(|(left_path, left), (right_path, right)| {
            left.0
                .cmp(&right.0)
                .then_with(|| {
                    left_path
                        .components()
                        .count()
                        .cmp(&right_path.components().count())
                })
                .then_with(|| left.1.cmp(&right.1))
                .then_with(|| left.2.cmp(&right.2))
        })
        .map(|(root, _)| root)
        .unwrap_or(default_root)
}

fn build_agenda(
    compiled: &SessionBrainCompiledInput,
    focus: &SessionFocus,
    strategy_summary: &[String],
    options: &SessionBrainBuildOptions,
) -> SessionBrainAgenda {
    let suppress_machine_fallback = focus.suppresses_machine_fallback();
    let current_goal = options
        .explicit_goal
        .as_ref()
        .map(|goal| truncate(goal, 200))
        .or_else(|| focus.preferred_live_goal())
        .or_else(|| {
            focus
                .next_move_candidates
                .iter()
                .find(|item| item.role == "user")
                .map(|item| item.summary.clone())
        })
        .or_else(|| {
            focus
                .next_move_candidates
                .iter()
                .find(|item| item.role == "assistant")
                .map(|item| item.summary.clone())
        })
        .or_else(|| {
            (!suppress_machine_fallback)
                .then(|| dependency_linked_obligation(compiled, &focus.ordered_task_hints()))
                .flatten()
        })
        .or_else(|| {
            (!suppress_machine_fallback)
                .then(|| strategy_summary.first().map(|line| truncate(line, 200)))
                .flatten()
        });

    let mut subgoals = focus
        .next_move_candidates
        .iter()
        .filter(|item| item.role == "user")
        .filter_map(|item| {
            (Some(item.summary.as_str()) != current_goal.as_deref()).then(|| item.summary.clone())
        })
        .take(4)
        .collect::<Vec<_>>();
    if let Some(goal) = current_goal.as_deref() {
        subgoals.retain(|item| !same_agenda_item(item, goal));
    }
    dedupe_strings(&mut subgoals);

    let redirects = focus
        .redirects
        .iter()
        .take(5)
        .map(SessionEvidence::to_signal)
        .collect::<Vec<_>>();

    let mut next_actions = focus
        .next_move_candidates
        .iter()
        .filter(|item| item.role == "user")
        .map(|item| item.summary.clone())
        .collect::<Vec<_>>();
    next_actions.extend(
        focus
            .next_move_candidates
            .iter()
            .filter(|item| item.role == "assistant")
            .map(|item| item.summary.clone()),
    );
    if next_actions.is_empty() && !suppress_machine_fallback {
        next_actions.extend(matching_obligations(compiled, &focus.ordered_task_hints()));
    }
    dedupe_strings(&mut next_actions);

    SessionBrainAgenda {
        current_goal,
        subgoals,
        redirects,
        next_actions,
    }
}

fn build_state(
    compiled: &SessionBrainCompiledInput,
    failures: &[FailureFact],
    focus: &SessionFocus,
    task_path: &TaskPath,
    current_goal: Option<&str>,
) -> SessionBrainState {
    let allow_machine_state =
        !focus.saw_user_message || current_goal.map(text_has_specific_anchor).unwrap_or(false);
    let current_task_cutoff = latest_current_ask_line(focus);
    let mut decisions = focus
        .decisions
        .iter()
        .take(4)
        .map(SessionEvidence::to_signal)
        .collect::<Vec<_>>();
    if decisions.len() < 4 && allow_machine_state {
        decisions.extend(
            compiled
                .live_claims
                .iter()
                .filter(|claim| claim.claim_type.eq_ignore_ascii_case("decision"))
                .filter(|claim| task_path.is_empty() || task_path.matches_text(&claim.claim))
                .take(4 - decisions.len())
                .map(|claim| SessionBrainSignal {
                    summary: truncate(&claim.claim, 180),
                    source: format!("claim:{}", claim.claim_type),
                    timestamp: Some(claim.observed_at.clone()),
                    evidence: claim.dependencies.iter().take(3).cloned().collect(),
                }),
        );
    }
    dedupe_signals(&mut decisions);

    let mut findings = focus
        .findings
        .iter()
        .filter(|item| in_current_task_window(item, current_task_cutoff, current_goal))
        .map(SessionEvidence::to_signal)
        .collect::<Vec<_>>();
    if allow_machine_state {
        findings.extend(
            non_blocking_failure_signals(failures, task_path)
                .into_iter()
                .take(2),
        );
    }
    if findings.len() < 4 && allow_machine_state {
        findings.extend(
            compiled
                .current_state
                .iter()
                .filter(|fact| task_path.is_empty() || task_path.matches_text(&fact.summary))
                .take(4 - findings.len())
                .map(|fact| SessionBrainSignal {
                    summary: truncate(&fact.summary, 180),
                    source: fact.event_type.clone(),
                    timestamp: Some(fact.observed_at.clone()),
                    evidence: fact.artifact_id.iter().cloned().collect::<Vec<_>>(),
                }),
        );
    }
    dedupe_signals(&mut findings);

    let mut blockers = focus
        .blockers
        .iter()
        .filter(|item| in_current_task_window(item, current_task_cutoff, current_goal))
        .filter(|item| task_path.is_empty() || task_path.matches_text(&item.summary))
        .filter(|item| !blocker_cleared_by_resolved_evidence(item, &focus.resolved_blockers))
        .map(SessionEvidence::to_signal)
        .collect::<Vec<_>>();
    if allow_machine_state {
        blockers.extend(
            failures
                .iter()
                .filter(|failure| failure_is_blocker(failure, task_path))
                .filter(|failure| {
                    !focus
                        .resolved_blockers
                        .iter()
                        .any(|item| item.matches_text(&failure.summary))
                })
                .take(4)
                .map(|failure| SessionBrainSignal {
                    summary: truncate(&failure.summary, 180),
                    source: failure.event_type.clone(),
                    timestamp: Some(failure.observed_at.clone()),
                    evidence: failure.details.iter().take(2).cloned().collect(),
                }),
        );
    }
    dedupe_signals(&mut blockers);

    let mut verified_facts = focus
        .verified_facts
        .iter()
        .filter(|item| in_current_task_window(item, current_task_cutoff, current_goal))
        .map(SessionEvidence::to_signal)
        .collect::<Vec<_>>();
    if verified_facts.len() < 3 && allow_machine_state {
        verified_facts.extend(
            compiled
                .current_state
                .iter()
                .filter(|fact| fact_is_verification(&fact.summary))
                .filter(|fact| task_path.is_empty() || task_path.matches_text(&fact.summary))
                .take(3 - verified_facts.len())
                .map(|fact| SessionBrainSignal {
                    summary: truncate(&fact.summary, 180),
                    source: "verified-state".to_string(),
                    timestamp: Some(fact.observed_at.clone()),
                    evidence: fact.artifact_id.iter().cloned().collect::<Vec<_>>(),
                }),
        );
    }
    dedupe_signals(&mut verified_facts);

    let mut rejected_options = focus
        .rejections
        .iter()
        .take(4)
        .map(SessionEvidence::to_signal)
        .collect::<Vec<_>>();
    dedupe_signals(&mut rejected_options);

    SessionBrainState {
        decisions,
        findings,
        blockers,
        verified_facts,
        rejected_options,
    }
}

fn build_task_path(
    current_goal: Option<&str>,
    focus: &SessionFocus,
    compiled: &SessionBrainCompiledInput,
) -> TaskPath {
    let mut task_path = TaskPath::default();
    let suppress_machine_fallback = focus.suppresses_machine_fallback();

    if let Some(goal) = current_goal {
        task_path.add_text(goal);
    }

    for hint in focus.ordered_task_hints().iter().take(12) {
        task_path.add_hint(hint);
    }

    if task_path.is_empty() {
        for item in focus.current_ask_candidates.iter().take(3) {
            task_path.add_text(&item.summary);
        }
    }

    if task_path.is_empty() {
        for item in focus
            .next_move_candidates
            .iter()
            .filter(|item| item.role == "assistant")
            .take(2)
        {
            task_path.add_text(&item.summary);
        }
    }

    if task_path.is_empty() && !suppress_machine_fallback {
        for claim in compiled.open_obligations.iter().take(2) {
            task_path.add_text(&claim.claim);
            for dependency in &claim.dependencies {
                task_path.add_text(dependency);
            }
        }
    }

    task_path
}

fn matching_obligations(
    compiled: &SessionBrainCompiledInput,
    task_hints: &[String],
) -> Vec<String> {
    let hints = task_hints
        .iter()
        .map(|hint| hint.to_ascii_lowercase())
        .collect::<Vec<_>>();
    compiled
        .open_obligations
        .iter()
        .filter(|claim| {
            hints.is_empty()
                || hints.iter().any(|hint| {
                    claim.claim.to_ascii_lowercase().contains(hint)
                        || claim
                            .dependencies
                            .iter()
                            .any(|dependency| dependency.to_ascii_lowercase().contains(hint))
                })
        })
        .take(3)
        .map(|claim| truncate(&claim.claim, 160))
        .collect()
}

fn dependency_linked_obligation(
    compiled: &SessionBrainCompiledInput,
    task_hints: &[String],
) -> Option<String> {
    matching_obligations(compiled, task_hints)
        .into_iter()
        .next()
        .or_else(|| {
            compiled
                .open_obligations
                .first()
                .map(|claim| truncate(&claim.claim, 200))
        })
}

fn partial_strategy_from_user_context(user: &super::types::SessionBrainUserContext) -> Vec<String> {
    [
        user.brief.as_str(),
        user.overview.as_str(),
        user.profile.as_str(),
    ]
    .iter()
    .flat_map(|line| line.split(" | "))
    .map(str::trim)
    .filter(|line| {
        let lowered = line.to_ascii_lowercase();
        lowered.contains("strategy")
            || lowered.contains("kpi")
            || lowered.contains("lead database")
            || lowered.contains("sitesorted")
            || lowered.contains("quarterly")
            || lowered.contains("revenue")
    })
    .take(5)
    .map(|line| format!("Partial strategy memory: {line}"))
    .collect()
}

fn non_blocking_failure_signals(
    failures: &[FailureFact],
    task_path: &TaskPath,
) -> Vec<SessionBrainSignal> {
    failures
        .iter()
        .filter(|failure| !failure_is_blocker(failure, task_path))
        .filter(|failure| {
            let combined = failure_text(failure);
            task_path.is_empty() || task_path.matches_text(&combined)
        })
        .take(2)
        .map(|failure| SessionBrainSignal {
            summary: truncate(&failure.summary, 180),
            source: failure.event_type.clone(),
            timestamp: Some(failure.observed_at.clone()),
            evidence: failure.details.iter().take(2).cloned().collect(),
        })
        .collect()
}

fn failure_is_blocker(failure: &FailureFact, task_path: &TaskPath) -> bool {
    let combined = failure_text(failure);
    if !task_path.is_empty() && !task_path.matches_text(&combined) {
        return false;
    }
    looks_like_blocker(&combined) && !combined.contains("warning")
}

fn failure_text(failure: &FailureFact) -> String {
    format!("{}\n{}", failure.summary, failure.details.join("\n")).to_ascii_lowercase()
}

fn looks_like_blocker(text: &str) -> bool {
    let lowered = text.to_ascii_lowercase();
    let tokens = lowered
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .filter(|token| !token.is_empty())
        .collect::<Vec<_>>();
    let blocker_token = tokens.iter().any(|token| {
        matches!(
            *token,
            "blocked" | "blocker" | "error" | "failed" | "panic" | "unable" | "cannot"
        )
    });
    blocker_token
        && !tokens
            .iter()
            .any(|token| *token == "warning" || *token == "warnings")
}

fn fact_is_verification(text: &str) -> bool {
    let lowered = text.to_ascii_lowercase();
    lowered.contains("passed")
        || lowered.contains("verified")
        || lowered.contains("confirmed")
        || lowered.contains("ok")
}

fn dedupe_strings(items: &mut Vec<String>) {
    let mut seen = HashSet::new();
    items.retain(|item| seen.insert(agenda_item_key(item)));
}

fn dedupe_signals(items: &mut Vec<SessionBrainSignal>) {
    let mut seen = HashSet::new();
    items.retain(|item| seen.insert(item.summary.clone()));
}

fn same_agenda_item(left: &str, right: &str) -> bool {
    agenda_item_key(left) == agenda_item_key(right)
}

fn agenda_item_key(value: &str) -> String {
    value
        .trim()
        .trim_end_matches('.')
        .trim_end_matches("...")
        .to_ascii_lowercase()
        .split_whitespace()
        .take(16)
        .collect::<Vec<_>>()
        .join(" ")
}

fn blocker_cleared_by_resolved_evidence(
    blocker: &SessionEvidence,
    resolved_blockers: &[SessionEvidence],
) -> bool {
    resolved_blockers.iter().any(|resolved| {
        resolved.line_number >= blocker.line_number
            && (resolved.matches_text(&blocker.summary) || broad_completion_signal(resolved))
    })
}

fn latest_current_ask_line(focus: &SessionFocus) -> usize {
    focus
        .current_ask_candidates
        .iter()
        .map(|item| item.line_number)
        .max()
        .unwrap_or(0)
}

fn in_current_task_window(
    item: &SessionEvidence,
    cutoff_line: usize,
    current_goal: Option<&str>,
) -> bool {
    cutoff_line == 0
        || item.line_number >= cutoff_line
        || current_goal
            .map(|goal| item.matches_text(goal))
            .unwrap_or(false)
}

fn broad_completion_signal(evidence: &SessionEvidence) -> bool {
    let lowered = evidence.summary.to_ascii_lowercase();
    (lowered.contains("done")
        || lowered.contains("completed")
        || lowered.contains("fixed")
        || lowered.contains("resolved")
        || lowered.contains("verified")
        || lowered.contains("passed"))
        && (lowered.contains("test")
            || lowered.contains("build")
            || lowered.contains("architect")
            || lowered.contains("blocker")
            || lowered.contains("ralph")
            || lowered.contains("complete"))
}

fn required_overlap(term_count: usize) -> usize {
    if term_count <= 3 {
        1
    } else {
        2
    }
}

fn normalize_hint_path(hint: &str) -> String {
    hint.trim_matches(|ch: char| matches!(ch, '`' | '"' | '\'' | '(' | ')' | '[' | ']'))
        .trim_end_matches('.')
        .replace('\\', "/")
}

fn looks_like_path_hint(hint: &str) -> bool {
    let lowered = hint.to_ascii_lowercase();
    lowered.contains("src/")
        || lowered.contains("tests/")
        || lowered.contains('\\')
        || lowered.ends_with(".rs")
        || lowered.ends_with(".md")
        || lowered.ends_with(".toml")
        || lowered.ends_with(".json")
}

fn extract_match_terms(text: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    text.split(|ch: char| !ch.is_ascii_alphanumeric() && ch != '_' && ch != '-')
        .filter_map(|token| {
            let lowered = token.to_ascii_lowercase();
            let keep = lowered.len() >= 4
                && !matches!(
                    lowered.as_str(),
                    "with"
                        | "from"
                        | "that"
                        | "this"
                        | "into"
                        | "then"
                        | "only"
                        | "will"
                        | "keep"
                        | "make"
                        | "have"
                        | "does"
                        | "current"
                        | "next"
                );
            keep.then_some(lowered)
        })
        .filter(|token| seen.insert(token.clone()))
        .collect()
}

fn generic_task_query_term(term: &str) -> bool {
    matches!(
        term,
        "show"
            | "details"
            | "detail"
            | "actual"
            | "seen"
            | "what"
            | "which"
            | "tell"
            | "give"
            | "output"
            | "readout"
            | "status"
    )
}

fn text_has_specific_anchor(text: &str) -> bool {
    if text.trim_start().starts_with('$') {
        return false;
    }
    looks_like_path_hint(text)
        || extract_match_terms(text)
            .iter()
            .any(|term| !generic_task_query_term(term))
}

#[cfg(test)]
mod tests {
    use super::super::types::{SessionBrainMessage, SessionBrainProvider};
    use super::*;
    use crate::core::worldview::{ContextClaim, ContextFact};

    fn sample_context() -> SessionBrainCompiledInput {
        SessionBrainCompiledInput {
            project_path: "C:/repo".to_string(),
            current_state: vec![ContextFact {
                observed_at: "2026-04-15T00:00:00Z".to_string(),
                event_type: "git-status".to_string(),
                subject: "git-status:C:/repo".to_string(),
                status: "changed".to_string(),
                summary: "repo has the active parser patch".to_string(),
                command_sig: "context git status".to_string(),
                artifact_id: None,
            }],
            live_claims: vec![ContextClaim {
                observed_at: "2026-04-15T00:00:00Z".to_string(),
                claim_type: "decision".to_string(),
                confidence: "high".to_string(),
                claim: "Keep Session Brain in the context runtime instead of an OMX-only lane."
                    .to_string(),
                rationale_capsule: None,
                dependencies: vec!["user-decision:session-brain".to_string()],
                evidence: vec![],
            }],
            open_obligations: vec![ContextClaim {
                observed_at: "2026-04-15T00:00:00Z".to_string(),
                claim_type: "obligation".to_string(),
                confidence: "high".to_string(),
                claim: "Resolve the current failing auth test.".to_string(),
                rationale_capsule: None,
                dependencies: vec!["worldview:cargo-test:C:/repo".to_string()],
                evidence: vec![],
            }],
        }
    }

    fn failure(summary: &str, details: &[&str]) -> FailureFact {
        FailureFact {
            observed_at: "2026-04-15T00:00:00Z".to_string(),
            event_type: "cargo-build".to_string(),
            subject: "cargo-build:C:/repo".to_string(),
            summary: summary.to_string(),
            details: details.iter().map(|item| item.to_string()).collect(),
            artifact_id: None,
        }
    }

    fn message(role: &str, text: &str) -> SessionBrainMessage {
        SessionBrainMessage {
            role: role.to_string(),
            provider: SessionBrainProvider::Codex,
            session_id: Some("sess-1".to_string()),
            timestamp: Some("2026-04-15T00:00:00Z".to_string()),
            cwd: Some("C:/repo".to_string()),
            transcript_path: "C:/repo/session.jsonl".to_string(),
            record_type: "fixture".to_string(),
            line_number: 1,
            text: text.to_string(),
            source_kind: "root".to_string(),
        }
    }

    #[test]
    fn agenda_prefers_transcript_ask_over_worldview_obligation() {
        let focus = build_session_focus(
            &[message(
                "user",
                "Task Statement\nFix the Session Brain content so it reflects the actual session.",
            )],
            &[],
        );

        let agenda = build_agenda(
            &sample_context(),
            &focus,
            &[],
            &SessionBrainBuildOptions {
                explicit_goal: None,
                allow_session_fallback: false,
            },
        );

        assert_eq!(
            agenda.current_goal.as_deref(),
            Some("Fix the Session Brain content so it reflects the actual session.")
        );
    }

    #[test]
    fn agenda_does_not_reuse_previous_current_asks_as_subgoals() {
        let focus = build_session_focus(
            &[
                message(
                    "user",
                    "$ralph to completion, boil the lake, don't come back until everything is complete and there are no dependencies left, just time to test.",
                ),
                message("user", "$munin-brain"),
            ],
            &[],
        );

        let agenda = build_agenda(
            &sample_context(),
            &focus,
            &[],
            &SessionBrainBuildOptions::default(),
        );

        assert_eq!(agenda.current_goal.as_deref(), Some("$munin-brain"));
        assert!(agenda.subgoals.is_empty());
    }

    #[test]
    fn partial_strategy_memory_counts_when_kernel_is_absent() {
        let user = super::super::types::SessionBrainUserContext {
            brief: String::new(),
            overview:
                "Business strategy: Build a lead database for NZ trade businesses | Current work: other"
                    .to_string(),
            profile: "Working preference: direct fixes".to_string(),
            friction: String::new(),
        };

        let summary = partial_strategy_from_user_context(&user);

        assert_eq!(summary.len(), 1);
        assert!(summary[0].contains("Business strategy"));
    }

    #[test]
    fn unclassified_live_user_message_blocks_strategy_current_goal_fallback() {
        let focus = build_session_focus(
            &[message(
                "user",
                "What the 5 UserPromptSubmit hooks actually do:",
            )],
            &[],
        );
        let agenda = build_agenda(
            &sample_context(),
            &focus,
            &["Partial strategy memory: Audit data schema completeness.txt".to_string()],
            &SessionBrainBuildOptions::default(),
        );

        assert!(focus.saw_user_message);
        assert!(focus.current_ask_candidates.is_empty());
        assert!(agenda.current_goal.is_none());
    }

    #[test]
    fn dissatisfaction_suppresses_worldview_fallback_without_replacing_live_ask() {
        let focus = build_session_focus(
            &[message(
                "user",
                "That is not what I asked you to do in this session. Almost all of this is absolute garbage.",
            )],
            &[],
        );

        let agenda = build_agenda(
            &sample_context(),
            &focus,
            &[],
            &SessionBrainBuildOptions::default(),
        );

        assert!(agenda.current_goal.is_none());
        assert!(agenda.next_actions.is_empty());

        let task_path = build_task_path(agenda.current_goal.as_deref(), &focus, &sample_context());
        let state = build_state(
            &sample_context(),
            &[failure(
                "cargo test: 1 errors, 0 warnings (0 crates)",
                &["could not find Cargo.toml in C:/repo or any parent directory"],
            )],
            &focus,
            &task_path,
            agenda.current_goal.as_deref(),
        );

        assert!(state.decisions.is_empty());
        assert!(state.findings.is_empty());
        assert!(state.blockers.is_empty());
        assert!(state.verified_facts.is_empty());
    }

    #[test]
    fn blockers_require_current_task_path_match() {
        let focus = build_session_focus(
            &[message(
                "user",
                "Task Statement\nFix the Session Brain content so it reflects the actual session.\nLikely Codebase Touchpoints\n- src/session_brain/build.rs",
            )],
            &[],
        );
        let task_path = build_task_path(
            Some("Fix the Session Brain content so it reflects the actual session."),
            &focus,
            &sample_context(),
        );
        let state = build_state(
            &sample_context(),
            &[
                failure(
                    "cargo build: 0 errors, 23 warnings",
                    &["warning in src/session_brain/build.rs: unused replay_shells"],
                ),
                failure(
                    "cargo build: 1 failed",
                    &["error in src/session_brain/build.rs: missing field"],
                ),
            ],
            &focus,
            &task_path,
            Some("Fix the Session Brain content so it reflects the actual session."),
        );

        assert_eq!(state.blockers.len(), 1);
        assert!(state.blockers[0].summary.contains("1 failed"));
        assert!(state
            .findings
            .iter()
            .any(|item| item.summary.contains("23 warnings")));
    }

    #[test]
    fn build_task_path_prefers_live_intent_before_worldview_seed() {
        let focus = build_session_focus(
            &[message(
                "user",
                "Task Statement\nShow me the session brain command for this session.",
            )],
            &[],
        );

        let task_path = build_task_path(None, &focus, &sample_context());

        assert!(task_path.matches_text("Show me the session brain command for this session."));
        assert!(!task_path.matches_text("Resolve the current failing auth test."));
    }

    #[test]
    fn generic_inspection_ask_does_not_admit_machine_failure_state() {
        let focus = build_session_focus(
            &[message("user", "show me the actual details that you see")],
            &[],
        );
        let task_path = build_task_path(
            Some("show me the actual details that you see"),
            &focus,
            &sample_context(),
        );
        let state = build_state(
            &sample_context(),
            &[failure(
                "cargo test: 1 errors, 0 warnings (0 crates)",
                &["could not find Cargo.toml in C:/repo or any parent directory"],
            )],
            &focus,
            &task_path,
            Some("show me the actual details that you see"),
        );

        assert!(!text_has_specific_anchor(
            "show me the actual details that you see"
        ));
        assert!(state.findings.is_empty());
        assert!(state.blockers.is_empty());
    }

    #[test]
    fn skill_invocation_does_not_admit_machine_failure_state() {
        let focus = build_session_focus(&[message("user", "$munin-brain")], &[]);
        let task_path = build_task_path(Some("$munin-brain"), &focus, &sample_context());
        let state = build_state(
            &sample_context(),
            &[failure(
                "cargo test: 1 errors, 0 warnings (0 crates)",
                &["could not find Cargo.toml in C:/repo or any parent directory"],
            )],
            &focus,
            &task_path,
            Some("$munin-brain"),
        );

        assert!(!text_has_specific_anchor("$munin-brain"));
        assert!(state.findings.is_empty());
        assert!(state.blockers.is_empty());
    }

    #[test]
    fn next_actions_prefer_transcript_moves_before_worldview() {
        let focus = build_session_focus(
            &[message(
                "user",
                "Approved Plan\n1. Add a transient SessionFocus / SessionEvidence layer inside src/session_brain/.\n2. Rebuild agenda precedence.",
            )],
            &[message(
                "assistant",
                "Next I'll update the renderers after the normalization layer lands.",
            )],
        );

        let agenda = build_agenda(
            &sample_context(),
            &focus,
            &[],
            &SessionBrainBuildOptions::default(),
        );

        assert_eq!(
            agenda.next_actions.first().map(String::as_str),
            Some("Next I'll update the renderers after the normalization layer lands.")
        );
        assert!(agenda
            .next_actions
            .iter()
            .any(|item| item.contains("renderers")));
        assert!(!agenda
            .next_actions
            .iter()
            .any(|item| item.contains("SessionFocus / SessionEvidence")));
        assert!(!agenda
            .next_actions
            .iter()
            .any(|item| item.contains("failing auth test")));
    }

    #[test]
    fn resolved_blocker_clear_suppresses_matching_failure_fact() {
        let focus = build_session_focus(
            &[message(
                "user",
                "Task Statement\nFix Session Brain freshness.\nKnown Facts / Evidence\n- cargo build failed in src/session_brain/build.rs",
            )],
            &[message(
                "assistant",
                "Verified cargo build passed after fixing src/session_brain/build.rs.",
            )],
        );
        let task_path = build_task_path(
            Some("Fix Session Brain freshness."),
            &focus,
            &sample_context(),
        );

        let state = build_state(
            &sample_context(),
            &[FailureFact {
                event_type: "cargo-build".to_string(),
                subject: "src/session_brain/build.rs".to_string(),
                summary: "cargo build failed in src/session_brain/build.rs".to_string(),
                details: vec!["blocking".to_string()],
                observed_at: "2026-04-15T00:00:00Z".to_string(),
                artifact_id: None,
            }],
            &focus,
            &task_path,
            Some("Fix Session Brain freshness."),
        );

        assert!(state.blockers.is_empty());
        assert!(state
            .verified_facts
            .iter()
            .any(|item| item.summary.contains("cargo build passed")));
    }

    #[test]
    fn resolved_blocker_clear_suppresses_prior_session_blockers() {
        let focus = build_session_focus(
            &[message(
                "user",
                "Known Facts / Evidence\n- Architect found another real staging blocker in src/core/worldview.rs.",
            )],
            &[message(
                "assistant",
                "Done. Architect approved, cargo test passed, and all blockers are resolved.",
            )],
        );
        let task_path = build_task_path(
            Some("Fix Session Brain freshness."),
            &focus,
            &sample_context(),
        );

        let state = build_state(
            &sample_context(),
            &[],
            &focus,
            &task_path,
            Some("Fix Session Brain freshness."),
        );

        assert!(state.blockers.is_empty());
        assert!(!state
            .findings
            .iter()
            .any(|item| item.summary.contains("blockers are resolved")));
        assert!(state
            .verified_facts
            .iter()
            .any(|item| item.summary.contains("cargo test passed")));
    }
}
