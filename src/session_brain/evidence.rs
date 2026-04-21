use super::types::{SessionBrainMessage, SessionBrainSignal};
use crate::core::utils::{normalize_windows_path_string, truncate};
use std::cmp::{Ordering, Reverse};
use std::collections::HashSet;

#[derive(Debug, Clone)]
pub(crate) struct SessionEvidence {
    pub summary: String,
    pub source: String,
    pub timestamp: Option<String>,
    pub evidence: Vec<String>,
    pub role: String,
    pub priority: i32,
    pub bucket: String,
    pub source_class: String,
    pub evidence_kind: String,
    pub line_number: usize,
    pub subject_tokens: Vec<String>,
}

impl SessionEvidence {
    pub(crate) fn to_signal(&self) -> SessionBrainSignal {
        SessionBrainSignal {
            summary: self.summary.clone(),
            source: self.source.clone(),
            timestamp: self.timestamp.clone(),
            evidence: public_evidence(&self.evidence),
        }
    }

    pub(crate) fn matches_text(&self, text: &str) -> bool {
        let other = extract_subject_tokens(text);
        if normalize_summary(&self.summary) == normalize_summary(text) {
            return true;
        }
        self.subject_tokens
            .iter()
            .filter(|token| other.contains(token))
            .count()
            >= 2
    }
}

fn public_evidence(evidence: &[String]) -> Vec<String> {
    evidence
        .iter()
        .filter(|item| !looks_like_raw_transcript_ref(item))
        .take(3)
        .cloned()
        .collect()
}

fn looks_like_raw_transcript_ref(value: &str) -> bool {
    let lowered = value.to_ascii_lowercase().replace('\\', "/");
    lowered.contains(".jsonl")
        || lowered.contains("/.codex/sessions/")
        || lowered.contains("/.claude/projects/")
}

#[derive(Debug, Clone)]
pub(crate) struct SessionTaskHint {
    pub value: String,
    pub priority: i32,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct SessionFocus {
    pub saw_user_message: bool,
    pub current_ask_candidates: Vec<SessionEvidence>,
    pub redirects: Vec<SessionEvidence>,
    pub suppression_signals: Vec<SessionEvidence>,
    pub rejections: Vec<SessionEvidence>,
    pub decisions: Vec<SessionEvidence>,
    pub findings: Vec<SessionEvidence>,
    pub blockers: Vec<SessionEvidence>,
    pub resolved_blockers: Vec<SessionEvidence>,
    pub verified_facts: Vec<SessionEvidence>,
    pub next_move_candidates: Vec<SessionEvidence>,
    pub command_echoes: Vec<SessionEvidence>,
    pub boilerplate: Vec<SessionEvidence>,
    pub task_hints: Vec<SessionTaskHint>,
}

impl SessionFocus {
    pub(crate) fn ordered_task_hints(&self) -> Vec<String> {
        self.task_hints
            .iter()
            .map(|hint| hint.value.clone())
            .collect()
    }

    pub(crate) fn preferred_live_goal(&self) -> Option<String> {
        let live_ask = self
            .current_ask_candidates
            .iter()
            .find(|item| is_live_user_task_signal(item));
        let live_redirect = self
            .redirects
            .iter()
            .find(|item| is_live_user_task_signal(item));

        match (live_ask, live_redirect) {
            (Some(ask), Some(redirect)) if redirect_replaces_ask(redirect, ask) => {
                Some(redirect.summary.clone())
            }
            (Some(ask), _) => Some(ask.summary.clone()),
            (None, Some(redirect)) => Some(redirect.summary.clone()),
            _ => None,
        }
    }

    pub(crate) fn has_live_user_intent(&self) -> bool {
        self.preferred_live_goal().is_some()
            || self
                .current_ask_candidates
                .iter()
                .any(|item| item.role == "user")
            || self.redirects.iter().any(|item| item.role == "user")
            || self
                .next_move_candidates
                .iter()
                .any(|item| item.role == "user")
    }

    pub(crate) fn suppresses_machine_fallback(&self) -> bool {
        self.has_live_user_intent()
            || self
                .suppression_signals
                .iter()
                .any(|item| is_live_user_task_signal(item))
    }
}

#[derive(Debug, Clone)]
struct SectionBlock {
    label: String,
    items: Vec<String>,
}

pub(crate) fn build_session_focus(
    user_messages: &[SessionBrainMessage],
    assistant_messages: &[SessionBrainMessage],
) -> SessionFocus {
    let mut focus = SessionFocus::default();
    let mut raw_evidence = Vec::new();

    for (index, message) in merge_messages_by_chronology(user_messages, assistant_messages)
        .into_iter()
        .enumerate()
    {
        if message.text.trim().is_empty() {
            continue;
        }
        classify_message(&mut focus, &mut raw_evidence, message, index as i32);
    }

    for item in project_evidence(raw_evidence) {
        match item.bucket.as_str() {
            "current_ask" => push_evidence(&mut focus.current_ask_candidates, item),
            "redirect" => push_evidence(&mut focus.redirects, item),
            "suppression" => push_evidence(&mut focus.suppression_signals, item),
            "rejection" => push_evidence(&mut focus.rejections, item),
            "decision" => push_evidence(&mut focus.decisions, item),
            "finding" => push_evidence(&mut focus.findings, item),
            "blocker" => push_evidence(&mut focus.blockers, item),
            "resolved_blocker" => {
                push_evidence(&mut focus.resolved_blockers, item.clone());
                push_evidence(&mut focus.verified_facts, item);
            }
            "verified_fact" => push_evidence(&mut focus.verified_facts, item),
            "next_move" => push_evidence(&mut focus.next_move_candidates, item),
            "command_echo" => push_evidence(&mut focus.command_echoes, item),
            "boilerplate" => push_evidence(&mut focus.boilerplate, item),
            _ => {}
        }
    }

    if has_live_transcript(&focus.current_ask_candidates) {
        focus
            .current_ask_candidates
            .retain(|item| item.source_class == "live_transcript");
    }

    if focus.next_move_candidates.iter().any(|item| {
        matches!(
            item.source_class.as_str(),
            "live_transcript" | "assistant_chatter"
        )
    }) {
        focus
            .next_move_candidates
            .retain(|item| item.source_class != "snapshot_seeded");
    }

    sort_and_dedupe_evidence(&mut focus.current_ask_candidates);
    sort_and_dedupe_evidence(&mut focus.redirects);
    sort_and_dedupe_evidence(&mut focus.suppression_signals);
    sort_and_dedupe_evidence(&mut focus.rejections);
    sort_and_dedupe_evidence(&mut focus.decisions);
    sort_and_dedupe_evidence(&mut focus.findings);
    sort_and_dedupe_evidence(&mut focus.blockers);
    sort_and_dedupe_evidence(&mut focus.resolved_blockers);
    sort_and_dedupe_evidence(&mut focus.verified_facts);
    sort_and_dedupe_evidence(&mut focus.next_move_candidates);
    sort_and_dedupe_evidence(&mut focus.command_echoes);
    sort_and_dedupe_evidence(&mut focus.boilerplate);
    sort_and_dedupe_hints(&mut focus.task_hints);

    focus
}

fn classify_message(
    focus: &mut SessionFocus,
    raw_evidence: &mut Vec<SessionEvidence>,
    message: &SessionBrainMessage,
    recency: i32,
) {
    if message.role == "user" && !message.text.trim().is_empty() {
        focus.saw_user_message = true;
    }

    if message.role == "assistant" && is_assistant_session_brain_echo(&message.text) {
        return;
    }

    let sections = parse_message_sections(&message.text);
    for section in sections {
        let section_key = normalize_section_label(&section.label);
        for item in section.items {
            classify_item(focus, raw_evidence, message, recency, &section_key, &item);
        }
    }
}

fn classify_item(
    focus: &mut SessionFocus,
    raw_evidence: &mut Vec<SessionEvidence>,
    message: &SessionBrainMessage,
    recency: i32,
    section: &str,
    item: &str,
) {
    let trimmed = item.trim();
    if trimmed.is_empty() {
        return;
    }

    if message.role == "assistant" && is_assistant_progress_chatter(trimmed) {
        return;
    }

    if is_boilerplate_item(trimmed) {
        push_evidence(
            &mut focus.boilerplate,
            make_evidence(message, trimmed, 20 + recency, "boilerplate", "boilerplate"),
        );
        return;
    }

    if is_command_echo(trimmed) {
        push_evidence(
            &mut focus.command_echoes,
            make_evidence(
                message,
                trimmed,
                30 + recency,
                "command_echo",
                "command_echo",
            ),
        );
        return;
    }

    for hint in extract_task_hints(trimmed) {
        push_task_hint(
            &mut focus.task_hints,
            SessionTaskHint {
                value: hint,
                priority: task_hint_priority(section, trimmed) + recency,
            },
        );
    }

    match section {
        "task statement" | "current ask" | "goal" | "request" if message.role == "user" => {
            push_evidence(
                raw_evidence,
                make_evidence(message, trimmed, 460 + recency, "current_ask", "task"),
            )
        }
        "approved plan" | "plan" | "follow-up" | "handoff" => push_evidence(
            raw_evidence,
            make_evidence(message, trimmed, 210 + recency, "plan_context", "plan"),
        ),
        "redirect" | "redirects" if message.role == "user" => push_evidence(
            raw_evidence,
            make_evidence(message, trimmed, 380 + recency, "redirect", "task"),
        ),
        "rejected" | "constraints" if message.role == "user" => push_evidence(
            raw_evidence,
            make_evidence(message, trimmed, 340 + recency, "rejection", "rejection"),
        ),
        "decision" | "decisions" | "decision drivers" | "principles" | "adr" | "chosen"
        | "why chosen"
            if message.role == "user" =>
        {
            push_evidence(
                raw_evidence,
                make_evidence(
                    message,
                    &label_aware_summary(section, trimmed),
                    330 + recency,
                    "decision",
                    "decision",
                ),
            )
        }
        "known facts / evidence" | "known facts" | "evidence" => {
            if item_is_verified(trimmed) {
                push_evidence(
                    raw_evidence,
                    make_evidence(
                        message,
                        trimmed,
                        320 + recency,
                        "verified_fact",
                        "verification",
                    ),
                );
            } else if looks_like_blocker_clear(trimmed) {
                push_evidence(
                    raw_evidence,
                    make_evidence(
                        message,
                        trimmed,
                        318 + recency,
                        "resolved_blocker",
                        "blocker_clear",
                    ),
                );
            } else if looks_like_blocker(trimmed) {
                push_evidence(
                    raw_evidence,
                    make_evidence(message, trimmed, 315 + recency, "blocker", "blocker"),
                );
            } else {
                push_evidence(
                    raw_evidence,
                    make_evidence(message, trimmed, 310 + recency, "finding", "finding"),
                );
            }
        }
        "desired outcome" | "consequences" if looks_like_finding(trimmed) => push_evidence(
            raw_evidence,
            make_evidence(message, trimmed, 220 + recency, "finding", "finding"),
        ),
        "acceptance criteria" => {}
        "likely codebase touchpoints" | "main touchpoints" => {}
        _ => {}
    }

    if message.role == "user"
        && section_allows_action_heuristics(section)
        && looks_like_current_ask(trimmed)
    {
        push_evidence(
            raw_evidence,
            make_evidence(message, trimmed, 260 + recency, "current_ask", "task"),
        );
    }

    if message.role == "user"
        && matches!(section, "root" | "redirect" | "redirects")
        && looks_like_redirect(trimmed)
    {
        push_evidence(
            raw_evidence,
            make_evidence(message, trimmed, 270 + recency, "redirect", "task"),
        );
    }

    if message.role == "user"
        && matches!(section, "root" | "redirect" | "redirects")
        && looks_like_dissatisfaction(trimmed)
    {
        push_evidence(
            raw_evidence,
            make_evidence(
                message,
                trimmed,
                255 + recency,
                "suppression",
                "dissatisfaction",
            ),
        );
    }

    if message.role == "user"
        && section == "root"
        && !is_root_meta_instruction(trimmed)
        && looks_like_rejection(trimmed)
    {
        push_evidence(
            raw_evidence,
            make_evidence(message, trimmed, 265 + recency, "rejection", "rejection"),
        );
    }

    if message.role == "user"
        && section == "root"
        && !is_root_meta_instruction(trimmed)
        && looks_like_decision(trimmed)
    {
        push_evidence(
            raw_evidence,
            make_evidence(message, trimmed, 250 + recency, "decision", "decision"),
        );
    }

    if generic_observation_section(section) && looks_like_blocker_clear(trimmed) {
        push_evidence(
            raw_evidence,
            make_evidence(
                message,
                trimmed,
                246 + recency,
                "resolved_blocker",
                "blocker_clear",
            ),
        );
    } else if generic_observation_section(section) && item_is_verified(trimmed) {
        push_evidence(
            raw_evidence,
            make_evidence(
                message,
                trimmed,
                245 + recency,
                "verified_fact",
                "verification",
            ),
        );
    } else if generic_observation_section(section) && looks_like_blocker(trimmed) {
        push_evidence(
            raw_evidence,
            make_evidence(message, trimmed, 242 + recency, "blocker", "blocker"),
        );
    } else if generic_observation_section(section) && looks_like_finding(trimmed) {
        push_evidence(
            raw_evidence,
            make_evidence(message, trimmed, 240 + recency, "finding", "finding"),
        );
    }

    if section_allows_action_heuristics(section)
        && !matches!(
            section,
            "task statement" | "current ask" | "goal" | "request"
        )
        && looks_like_next_move(message.role.as_str(), trimmed)
    {
        push_evidence(
            raw_evidence,
            make_evidence(message, trimmed, 230 + recency, "next_move", "task"),
        );
    }
}

fn make_evidence(
    message: &SessionBrainMessage,
    text: &str,
    priority: i32,
    bucket: &str,
    evidence_kind: &str,
) -> SessionEvidence {
    SessionEvidence {
        summary: truncate(text, 220),
        source: format!("{}-message", message.role),
        timestamp: message.timestamp.clone(),
        evidence: vec![
            message.record_type.clone(),
            format!("{}:{}", message.transcript_path, message.line_number),
        ],
        role: message.role.clone(),
        priority,
        bucket: bucket.to_string(),
        source_class: evidence_source_class(message, evidence_kind).to_string(),
        evidence_kind: evidence_kind.to_string(),
        line_number: message.line_number,
        subject_tokens: extract_subject_tokens(text),
    }
}

fn evidence_source_class(message: &SessionBrainMessage, evidence_kind: &str) -> &'static str {
    if message.source_kind.eq_ignore_ascii_case("snapshot") {
        "snapshot_seeded"
    } else if message.role == "assistant"
        && !matches!(
            evidence_kind,
            "verification" | "completion" | "blocker_clear"
        )
    {
        "assistant_chatter"
    } else {
        "live_transcript"
    }
}

fn parse_message_sections(text: &str) -> Vec<SectionBlock> {
    let lines = text.lines().collect::<Vec<_>>();
    let mut sections = Vec::new();
    let mut current_label = "root".to_string();
    let mut current_items = Vec::new();

    for (index, raw_line) in lines.iter().enumerate() {
        let trimmed = raw_line.trim();
        if trimmed.is_empty() {
            continue;
        }

        if let Some((label, inline)) =
            classify_heading(trimmed, lines.get(index + 1).map(|line| line.trim()))
        {
            if !current_items.is_empty() || current_label != "root" {
                sections.push(SectionBlock {
                    label: current_label,
                    items: current_items,
                });
            }
            current_label = label;
            current_items = Vec::new();
            if let Some(value) = inline {
                current_items.push(value);
            }
            continue;
        }

        let item = strip_list_prefix(trimmed).trim();
        if !item.is_empty() {
            current_items.push(item.to_string());
        }
    }

    if !current_items.is_empty() || current_label != "root" {
        sections.push(SectionBlock {
            label: current_label,
            items: current_items,
        });
    }

    if sections.is_empty() {
        sections.push(SectionBlock {
            label: "root".to_string(),
            items: Vec::new(),
        });
    }

    sections
}

fn classify_heading(trimmed: &str, next_line: Option<&str>) -> Option<(String, Option<String>)> {
    if let Some(label) = trimmed.strip_prefix('#') {
        let heading = label.trim_start_matches('#').trim();
        return (!heading.is_empty()).then(|| (heading.to_string(), None));
    }

    let bullet_stripped = strip_list_prefix(trimmed).trim();
    if let Some(label) = bullet_stripped.strip_suffix(':') {
        let heading = label.trim();
        if is_short_heading(heading) {
            return Some((heading.to_string(), None));
        }
    }

    if let Some((label, inline)) = split_inline_heading(bullet_stripped) {
        return Some((label, Some(inline)));
    }

    if is_plain_heading(trimmed, next_line) {
        return Some((trimmed.to_string(), None));
    }

    None
}

fn split_inline_heading(text: &str) -> Option<(String, String)> {
    let (label, rest) = text.split_once(':')?;
    let label = label.trim();
    let rest = rest.trim();
    if !rest.is_empty() && is_short_heading(label) {
        return Some((label.to_string(), rest.to_string()));
    }
    None
}

fn is_plain_heading(trimmed: &str, next_line: Option<&str>) -> bool {
    if trimmed.starts_with("- ")
        || trimmed.starts_with("* ")
        || trimmed.starts_with("+ ")
        || numbered_prefix(trimmed).is_some()
    {
        return false;
    }
    if !is_short_heading(trimmed) {
        return false;
    }
    if trimmed.ends_with('.') {
        return false;
    }
    next_line.map(|line| !line.is_empty()).unwrap_or(false)
}

fn is_short_heading(text: &str) -> bool {
    let words = text.split_whitespace().count();
    words > 0 && words <= 5 && text.len() <= 48 && !text.contains('/') && !text.contains('\\')
}

fn merge_messages_by_chronology<'a>(
    user_messages: &'a [SessionBrainMessage],
    assistant_messages: &'a [SessionBrainMessage],
) -> Vec<&'a SessionBrainMessage> {
    let mut merged = user_messages
        .iter()
        .chain(assistant_messages.iter())
        .collect::<Vec<_>>();
    merged.sort_by(|left, right| compare_messages(left, right));
    merged
}

fn compare_messages(left: &SessionBrainMessage, right: &SessionBrainMessage) -> Ordering {
    left.timestamp
        .cmp(&right.timestamp)
        .then_with(|| left.line_number.cmp(&right.line_number))
        .then_with(|| message_source_rank(left).cmp(&message_source_rank(right)))
        .then_with(|| left.role.cmp(&right.role))
}

fn message_source_rank(message: &SessionBrainMessage) -> i32 {
    if message.source_kind.eq_ignore_ascii_case("snapshot") {
        0
    } else {
        1
    }
}

fn project_evidence(items: Vec<SessionEvidence>) -> Vec<SessionEvidence> {
    let mut projected = Vec::new();

    for (index, item) in items.iter().enumerate() {
        if items
            .iter()
            .skip(index + 1)
            .any(|later| later_supersedes(item, later))
        {
            continue;
        }
        projected.push(item.clone());
    }

    projected
}

fn is_live_user_task_signal(item: &SessionEvidence) -> bool {
    item.role == "user" && item.source_class == "live_transcript"
}

fn redirect_replaces_ask(redirect: &SessionEvidence, ask: &SessionEvidence) -> bool {
    is_newer(redirect, ask) && !same_item(redirect, ask)
}

fn is_newer(left: &SessionEvidence, right: &SessionEvidence) -> bool {
    left.timestamp > right.timestamp
        || (left.timestamp == right.timestamp && left.line_number > right.line_number)
}

fn later_supersedes(current: &SessionEvidence, later: &SessionEvidence) -> bool {
    if !same_item(current, later) {
        return false;
    }

    if compare_freshness(later, current) != Ordering::Greater {
        return false;
    }

    if later.evidence_kind == current.evidence_kind {
        return later.summary == current.summary || later.source_class != current.source_class;
    }

    matches!(
        (current.evidence_kind.as_str(), later.evidence_kind.as_str()),
        ("task", "verification")
            | ("task", "completion")
            | ("task", "blocker_clear")
            | ("plan", "verification")
            | ("plan", "completion")
            | ("plan", "blocker_clear")
            | ("blocker", "verification")
            | ("blocker", "completion")
            | ("blocker", "blocker_clear")
    )
}

fn compare_freshness(left: &SessionEvidence, right: &SessionEvidence) -> Ordering {
    evidence_kind_rank(left.evidence_kind.as_str())
        .cmp(&evidence_kind_rank(right.evidence_kind.as_str()))
        .then_with(|| left.timestamp.cmp(&right.timestamp))
        .then_with(|| left.line_number.cmp(&right.line_number))
        .then_with(|| {
            source_class_rank(left.source_class.as_str())
                .cmp(&source_class_rank(right.source_class.as_str()))
        })
}

fn evidence_kind_rank(kind: &str) -> i32 {
    match kind {
        "blocker_clear" | "completion" | "verification" => 3,
        "blocker" => 2,
        "task" | "plan" => 1,
        _ => 0,
    }
}

fn source_class_rank(source_class: &str) -> i32 {
    match source_class {
        "live_transcript" => 2,
        "snapshot_seeded" => 1,
        "assistant_chatter" => 0,
        _ => 0,
    }
}

fn has_live_transcript(items: &[SessionEvidence]) -> bool {
    items
        .iter()
        .any(|item| item.source_class == "live_transcript")
}

fn same_item(left: &SessionEvidence, right: &SessionEvidence) -> bool {
    if normalize_summary(&left.summary) == normalize_summary(&right.summary) {
        return true;
    }

    let overlap = left
        .subject_tokens
        .iter()
        .filter(|token| right.subject_tokens.contains(token))
        .count();

    overlap >= 2
}

fn normalize_summary(text: &str) -> String {
    text.to_ascii_lowercase()
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn extract_subject_tokens(text: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut tokens = Vec::new();

    for hint in extract_task_hints(text) {
        if seen.insert(hint.clone()) {
            tokens.push(hint);
        }
    }

    for token in text
        .to_ascii_lowercase()
        .split(|ch: char| !ch.is_ascii_alphanumeric() && ch != '_' && ch != '/' && ch != '\\')
    {
        let trimmed = token.trim_matches(|ch: char| !ch.is_ascii_alphanumeric());
        if trimmed.len() < 4 || subject_stopword(trimmed) {
            continue;
        }
        let normalized = trimmed.to_string();
        if seen.insert(normalized.clone()) {
            tokens.push(normalized);
        }
    }

    tokens
}

fn subject_stopword(token: &str) -> bool {
    matches!(
        token,
        "this"
            | "that"
            | "with"
            | "from"
            | "into"
            | "then"
            | "next"
            | "will"
            | "would"
            | "should"
            | "have"
            | "been"
            | "were"
            | "task"
            | "plan"
            | "approved"
            | "follow"
            | "followup"
            | "handoff"
            | "session"
            | "brain"
            | "current"
            | "actual"
            | "real"
            | "verified"
            | "fixed"
            | "rebuilt"
            | "passed"
            | "resolved"
            | "clear"
            | "cleared"
    )
}

fn strip_list_prefix(text: &str) -> &str {
    text.strip_prefix("- ")
        .or_else(|| text.strip_prefix("* "))
        .or_else(|| text.strip_prefix("+ "))
        .or_else(|| numbered_prefix(text))
        .unwrap_or(text)
}

fn numbered_prefix(text: &str) -> Option<&str> {
    let bytes = text.as_bytes();
    let mut index = 0;
    while index < bytes.len() && bytes[index].is_ascii_digit() {
        index += 1;
    }
    if index > 0 && bytes.get(index) == Some(&b'.') && bytes.get(index + 1) == Some(&b' ') {
        return Some(&text[index + 2..]);
    }
    None
}

fn normalize_section_label(label: &str) -> String {
    let lowered = label.trim().to_ascii_lowercase();
    match lowered.as_str() {
        "task statement" => "task statement".to_string(),
        "desired outcome" => "desired outcome".to_string(),
        "known facts / evidence" | "known facts" | "evidence" => "known facts / evidence".into(),
        "likely codebase touchpoints" | "main touchpoints" => {
            "likely codebase touchpoints".to_string()
        }
        "acceptance criteria" => "acceptance criteria".to_string(),
        "approved plan" => "approved plan".to_string(),
        "decision drivers" => "decision drivers".to_string(),
        "principles" => "principles".to_string(),
        "adr" => "adr".to_string(),
        "chosen" => "chosen".to_string(),
        "rejected" => "rejected".to_string(),
        "handoff" => "handoff".to_string(),
        "follow-up" => "follow-up".to_string(),
        "redirects" | "redirect" => "redirects".to_string(),
        "constraints" => "constraints".to_string(),
        "goal" => "goal".to_string(),
        _ => lowered,
    }
}

fn label_aware_summary(section: &str, item: &str) -> String {
    match section {
        "chosen" => format!("Chosen: {}", item),
        "rejected" => format!("Rejected: {}", item),
        _ => item.to_string(),
    }
}

fn task_hint_priority(section: &str, item: &str) -> i32 {
    let mut priority = match section {
        "task statement" | "current ask" | "goal" => 260,
        "approved plan" | "likely codebase touchpoints" | "main touchpoints" => 240,
        "known facts / evidence" => 220,
        _ => 180,
    };
    if item.contains("src/") || item.contains('\\') || item.ends_with(".rs") {
        priority += 20;
    }
    priority
}

fn push_evidence(target: &mut Vec<SessionEvidence>, item: SessionEvidence) {
    target.push(item);
}

fn push_task_hint(target: &mut Vec<SessionTaskHint>, hint: SessionTaskHint) {
    target.push(hint);
}

fn sort_and_dedupe_evidence(items: &mut Vec<SessionEvidence>) {
    items.sort_by_key(|item| (Reverse(item.priority), item.summary.clone()));
    let mut seen = HashSet::new();
    items.retain(|item| seen.insert(item.summary.clone()));
}

fn sort_and_dedupe_hints(items: &mut Vec<SessionTaskHint>) {
    items.sort_by_key(|item| (Reverse(item.priority), item.value.clone()));
    let mut seen = HashSet::new();
    items.retain(|item| seen.insert(item.value.clone()));
}

fn extract_task_hints(text: &str) -> Vec<String> {
    let mut hints = Vec::new();
    let mut seen = HashSet::new();
    for token in text.split_whitespace() {
        let cleaned = token.trim_matches(|ch: char| {
            matches!(
                ch,
                '`' | '"' | '\'' | '(' | ')' | '[' | ']' | '{' | '}' | ',' | ';'
            )
        });
        if cleaned.is_empty() {
            continue;
        }
        if looks_like_path(cleaned) {
            let normalized = normalize_windows_path_string(cleaned.trim_end_matches('.'));
            if seen.insert(normalized.clone()) {
                hints.push(normalized);
            }
            continue;
        }
        if looks_like_symbol(cleaned) {
            let normalized = cleaned.trim_end_matches('.').to_string();
            if seen.insert(normalized.clone()) {
                hints.push(normalized);
            }
        }
    }
    hints
}

fn looks_like_path(text: &str) -> bool {
    let lowered = text.to_ascii_lowercase();
    lowered.contains("src/")
        || lowered.contains("tests/")
        || lowered.contains('\\')
        || lowered.ends_with(".rs")
        || lowered.ends_with(".md")
        || lowered.ends_with(".toml")
        || lowered.ends_with(".json")
}

fn looks_like_symbol(text: &str) -> bool {
    let trimmed = text.trim_end_matches('.');
    trimmed.len() >= 4
        && trimmed.len() <= 64
        && !trimmed.contains("://")
        && (trimmed.contains('_') || trimmed.contains('-'))
        && trimmed
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | ':' | '.'))
}

fn looks_like_current_ask(text: &str) -> bool {
    let lowered = text.to_ascii_lowercase();
    if text.trim_end().ends_with(':') && text.split_whitespace().count() <= 8 {
        return false;
    }
    if lowered.trim_start().starts_with('$') {
        return true;
    }
    if matches!(
        lowered.as_str(),
        "approve" | "approved" | "ralplan-dr" | "adr" | "handoff"
    ) {
        return false;
    }
    if lowered.contains("unless the user asks") || lowered.contains("do not summarize") {
        return false;
    }
    starts_with_action_verb(&lowered)
        || lowered.starts_with("fix ")
        || lowered.starts_with("do ")
        || lowered.starts_with("make ")
        || lowered.starts_with("keep ")
        || lowered.starts_with("show ")
        || lowered.starts_with("run ")
        || lowered.starts_with("what ")
        || lowered.starts_with("which ")
        || lowered.starts_with("ensure ")
        || lowered.starts_with("no i want you to ")
        || lowered.starts_with("i want you to ")
        || lowered.contains("i want you to fix")
}

fn starts_with_action_verb(lowered: &str) -> bool {
    [
        "fix ",
        "implement ",
        "add ",
        "refactor ",
        "update ",
        "rebuild ",
        "tighten ",
        "verify ",
        "filter ",
        "remove ",
        "reorder ",
        "rank ",
        "treat ",
        "preserve ",
        "keep ",
        "make ",
    ]
    .iter()
    .any(|prefix| lowered.starts_with(prefix))
}

fn looks_like_redirect(text: &str) -> bool {
    let lowered = text.to_ascii_lowercase();
    (lowered.starts_with("actually ")
        || lowered.contains(" no, actually ")
        || lowered.contains(" instead"))
        && (starts_with_action_verb(&lowered)
            || lowered.starts_with("show ")
            || lowered.starts_with("run ")
            || lowered.starts_with("what ")
            || lowered.starts_with("which "))
        || lowered.contains("change direction")
}

fn looks_like_dissatisfaction(text: &str) -> bool {
    let lowered = text.to_ascii_lowercase();
    lowered.contains("not what i asked")
        || lowered.contains("wrong task")
        || lowered.contains("does not represent what i asked")
        || lowered.contains("doesn't represent what i asked")
        || lowered.contains("garbage")
        || lowered.contains("almost useless")
        || lowered.contains("is almost useless")
        || lowered.contains("useless for")
        || lowered.contains("not what i'm concerned about")
}

fn looks_like_rejection(text: &str) -> bool {
    let lowered = text.to_ascii_lowercase();
    lowered.contains("reject")
        || lowered.contains("rejected")
        || lowered.contains("do not add")
        || lowered.contains("do not turn")
        || lowered.contains("do not redesign")
        || lowered.contains("no new public")
        || lowered.contains("not an option")
        || lowered.contains("step 4 rejected")
}

fn looks_like_decision(text: &str) -> bool {
    let lowered = text.to_ascii_lowercase();
    lowered.starts_with("decision:")
        || lowered.starts_with("keep step ")
        || lowered.starts_with("treat step ")
        || lowered.starts_with("preserve ")
        || lowered.contains("chosen")
        || lowered.contains("decision")
}

fn looks_like_finding(text: &str) -> bool {
    let lowered = text.to_ascii_lowercase();
    if starts_with_action_verb(&lowered) || lowered.starts_with("do not ") {
        return false;
    }
    lowered.contains("found")
        || lowered.contains("currently")
        || lowered.contains("overweights")
        || lowered.contains("misses")
        || lowered.contains("shows")
        || lowered.contains("echoes")
        || lowered.contains("includes")
        || lowered.contains("fails")
        || lowered.contains("warnings")
}

fn item_is_verified(text: &str) -> bool {
    let lowered = text.to_ascii_lowercase();
    let has_result_verb = lowered.contains("works")
        || lowered.contains("confirmed")
        || lowered.contains("verified")
        || lowered.contains("passed")
        || lowered.contains("fixed")
        || lowered.contains("resolved")
        || lowered.contains("rebuilt")
        || lowered.contains("succeeded")
        || lowered.contains("embeds");
    has_result_verb
        && !looks_like_vague_passive_status(&lowered)
        && has_concrete_result_subject(text, &lowered)
}

fn looks_like_blocker(text: &str) -> bool {
    let lowered = text.to_ascii_lowercase();
    lowered.contains("blocked")
        || lowered.contains("blocker")
        || lowered.contains("failure")
        || lowered.contains("failed")
        || lowered.contains("error")
}

fn looks_like_blocker_clear(text: &str) -> bool {
    let lowered = text.to_ascii_lowercase();
    (lowered.contains("fixed")
        || lowered.contains("resolved")
        || lowered.contains("cleared")
        || lowered.contains("passed")
        || lowered.contains("verified"))
        && !lowered.contains("do not")
        && !looks_like_vague_passive_status(&lowered)
        && has_concrete_result_subject(text, &lowered)
}

fn section_allows_action_heuristics(section: &str) -> bool {
    matches!(
        section,
        "root" | "task statement" | "current ask" | "goal" | "request" | "redirect" | "redirects"
    )
}

fn is_root_meta_instruction(text: &str) -> bool {
    let lowered = text.to_ascii_lowercase();
    lowered.contains("this skill")
        || lowered.contains("simple skill")
        || lowered.contains("raw output")
        || lowered.contains("unless the user asks")
        || lowered.contains("do not summarize")
        || lowered.contains("runs that command")
}

fn looks_like_vague_passive_status(lowered: &str) -> bool {
    lowered.contains("already fixed")
        || lowered.contains("already resolved")
        || lowered.contains("problems that were already")
        || lowered.contains("issues that were already")
        || lowered.contains("things that were already")
}

fn has_concrete_result_subject(text: &str, lowered: &str) -> bool {
    !extract_task_hints(text).is_empty()
        || lowered.contains("session brain")
        || lowered.contains("agenda")
        || lowered.contains("prompt")
        || lowered.contains("current ask")
        || lowered.contains("task path")
        || lowered.contains("worldview")
        || lowered.contains("open obligation")
        || lowered.contains("cargo build")
        || lowered.contains("cargo test")
        || lowered.contains("build.rs")
        || lowered.contains("evidence.rs")
        || lowered.contains("messages.rs")
        || lowered.contains("inspection command")
}

fn looks_like_next_move(role: &str, text: &str) -> bool {
    let lowered = text.to_ascii_lowercase();
    if role == "assistant"
        && (lowered.starts_with("next ")
            || lowered.starts_with("i'll ")
            || lowered.starts_with("i will ")
            || lowered.starts_with("i am going to ")
            || lowered.starts_with("i'm going to "))
    {
        return true;
    }
    starts_with_action_verb(&lowered) || lowered.starts_with("run ")
}

fn generic_observation_section(section: &str) -> bool {
    matches!(
        section,
        "root"
            | "known facts / evidence"
            | "known facts"
            | "evidence"
            | "desired outcome"
            | "consequences"
    )
}

fn is_assistant_progress_chatter(text: &str) -> bool {
    let lowered = text.to_ascii_lowercase();
    lowered.starts_with("i'm checking")
        || lowered.starts_with("i am checking")
        || lowered.contains("i'm checking")
        || lowered.contains("i’m checking")
        || lowered.contains("i am checking")
        || lowered.starts_with("i'm pulling")
        || lowered.starts_with("i am pulling")
        || lowered.starts_with("i'm rerunning")
        || lowered.starts_with("i am rerunning")
        || lowered.contains("i'm rerunning")
        || lowered.contains("i’m rerunning")
        || lowered.contains("i am rerunning")
        || lowered.contains("i'm staging")
        || lowered.contains("i’m staging")
        || lowered.contains("i am staging")
        || lowered.contains("i'm fixing")
        || lowered.contains("i’m fixing")
        || lowered.contains("i am fixing")
        || lowered.contains("i'm going")
        || lowered.contains("i’m going")
        || lowered.contains("i am going")
        || lowered.contains("i'm building")
        || lowered.contains("i’m building")
        || lowered.contains("i am building")
        || lowered.contains("running the real")
        || lowered.contains("verification suite passed")
        || lowered.starts_with("the debug cli now reports")
        || lowered.starts_with("continuing ralph as a completion loop")
        || lowered.contains("i’m treating")
        || lowered.contains("i'm treating")
        || lowered.contains("i am treating")
        || lowered.starts_with("focused tests pass")
        || lowered.starts_with("the garbage has")
        || lowered.starts_with("status shows")
        || lowered.starts_with("architect found")
        || lowered.starts_with("the final architect rejected")
        || lowered.starts_with("the two architectural blockers")
        || lowered.starts_with("i'm waiting")
        || lowered.starts_with("i am waiting")
        || lowered.starts_with("i've tightened")
        || lowered.starts_with("i have tightened")
        || lowered.starts_with("i found no deeper")
        || lowered.contains("before touching code")
        || lowered.contains("manual surface checks")
        || lowered.contains("competing test job")
        || lowered.contains("which `context` binary")
}

fn is_assistant_session_brain_echo(text: &str) -> bool {
    let lowered = text.to_ascii_lowercase();
    lowered.contains("actual details from `munin brain --format prompt`")
        || lowered.contains("source_status:")
        || lowered.contains("session brain sourcestatus:")
        || lowered.contains("<session_brain")
        || lowered.contains("<runtime_context_v1")
        || lowered.contains("current agenda reported by munin")
}

fn is_command_echo(text: &str) -> bool {
    let lowered = text.trim_matches('`').trim().to_ascii_lowercase();
    let natural_language = lowered.contains("run `")
        || lowered.contains("please ")
        || lowered.contains("should ")
        || lowered.contains("fix ")
        || lowered.contains("implement ");
    if natural_language {
        return false;
    }
    let is_single_command = lowered.split_whitespace().count() <= 8
        && (lowered.starts_with("context ")
            || lowered.starts_with("cargo ")
            || lowered.starts_with("git ")
            || lowered.starts_with("npm ")
            || lowered.starts_with("pnpm "));
    let is_inspection = lowered.contains("inspect-current")
        || lowered.contains("--format prompt")
        || lowered.contains("git status --short");
    is_single_command && is_inspection
}

fn is_boilerplate_item(text: &str) -> bool {
    let lowered = text.to_ascii_lowercase();
    lowered.contains("ag ents.md instructions")
        || lowered.contains("agents.md instructions")
        || lowered.contains("autonomy directive")
        || lowered.contains("you are an autonomous coding agent")
        || lowered.contains("files called agents.md")
        || lowered.contains("environment_context")
        || lowered.contains("turn aborted")
        || lowered.contains("continue the current task using the packet below")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(role: &str, text: &str) -> SessionBrainMessage {
        message_with_source(role, text, "root", "2026-04-15T00:00:00Z", 1)
    }

    fn message_with_source(
        role: &str,
        text: &str,
        source_kind: &str,
        timestamp: &str,
        line_number: usize,
    ) -> SessionBrainMessage {
        SessionBrainMessage {
            role: role.to_string(),
            provider: super::super::types::SessionBrainProvider::Codex,
            session_id: Some("sess-1".to_string()),
            timestamp: Some(timestamp.to_string()),
            cwd: Some("C:/repo".to_string()),
            transcript_path: "C:/repo/session.jsonl".to_string(),
            record_type: "fixture".to_string(),
            line_number,
            text: text.to_string(),
            source_kind: source_kind.to_string(),
        }
    }

    #[test]
    fn focus_prioritizes_task_statement_and_filters_command_echoes() {
        let user = vec![
            message(
                "user",
                "Task Statement\nFix the Session Brain content so it reflects the actual session.\nLikely Codebase Touchpoints\n- src/session_brain/build.rs\n- src/session_brain/project.rs",
            ),
            message("user", "context session-brain inspect-current"),
        ];
        let assistant = vec![message(
            "assistant",
            "I found the current bad output echoes the inspection command.\nNext I'll rebuild agenda precedence.",
        )];

        let focus = build_session_focus(&user, &assistant);
        assert_eq!(
            focus
                .current_ask_candidates
                .first()
                .map(|item| item.summary.as_str()),
            Some("Fix the Session Brain content so it reflects the actual session.")
        );
        assert_eq!(focus.command_echoes.len(), 1);
        assert!(focus
            .ordered_task_hints()
            .iter()
            .any(|hint| hint == "src/session_brain/build.rs"));
        assert_eq!(
            focus
                .next_move_candidates
                .first()
                .map(|item| item.summary.as_str()),
            Some("Next I'll rebuild agenda precedence.")
        );
    }

    #[test]
    fn focus_extracts_rejections_and_decisions_from_structured_plan() {
        let user = vec![message(
            "user",
            "ADR\n- Chosen:\n  - B\n- Rejected:\n  - Do not add a second durable memory system.\nApproved Plan\n1. Add a transient SessionFocus / SessionEvidence layer inside src/session_brain/.\nConstraints\n- Do not turn it into an OMX-only feature.",
        )];

        let focus = build_session_focus(&user, &[]);

        assert!(focus
            .decisions
            .iter()
            .any(|item| item.summary == "Chosen: B"));
        assert!(focus
            .rejections
            .iter()
            .any(|item| item.summary.contains("second durable memory system")));
        assert!(!focus
            .next_move_candidates
            .iter()
            .any(|item| item.summary.contains("SessionFocus / SessionEvidence")));
    }

    #[test]
    fn focus_prefers_live_transcript_over_snapshot_seeded_task_statement() {
        let user = vec![
            message_with_source(
                "user",
                "Task Statement\nFollow the old plan from the snapshot.",
                "snapshot",
                "2026-04-15T00:00:00Z",
                1,
            ),
            message_with_source(
                "user",
                "Task Statement\nFix the Session Brain content so it reflects the actual session.",
                "root",
                "2026-04-15T00:00:00Z",
                2,
            ),
        ];

        let focus = build_session_focus(&user, &[]);
        assert_eq!(
            focus
                .current_ask_candidates
                .first()
                .map(|item| item.summary.as_str()),
            Some("Fix the Session Brain content so it reflects the actual session.")
        );
        assert!(!focus
            .current_ask_candidates
            .iter()
            .any(|item| item.summary.contains("old plan")));
    }

    #[test]
    fn later_live_redirect_replaces_earlier_live_ask() {
        let user = vec![
            message_with_source(
                "user",
                "Task Statement\nFix the Session Brain content so it reflects the actual session.",
                "root",
                "2026-04-15T00:00:00Z",
                1,
            ),
            message_with_source(
                "user",
                "Redirect\nShow me the session brain command instead.",
                "root",
                "2026-04-15T00:05:00Z",
                2,
            ),
        ];

        let focus = build_session_focus(&user, &[]);

        assert_eq!(
            focus.preferred_live_goal().as_deref(),
            Some("Show me the session brain command instead.")
        );
    }

    #[test]
    fn focus_later_assistant_completion_supersedes_earlier_user_plan_for_same_item() {
        let user = vec![message_with_source(
            "user",
            "Approved Plan\n- Rebuild agenda precedence.",
            "root",
            "2026-04-15T00:00:00Z",
            1,
        )];
        let assistant = vec![message_with_source(
            "assistant",
            "Verified agenda precedence rebuilt; cargo test passed.",
            "root",
            "2026-04-15T00:05:00Z",
            2,
        )];

        let focus = build_session_focus(&user, &assistant);

        assert!(focus.next_move_candidates.is_empty());
        assert!(focus
            .verified_facts
            .iter()
            .any(|item| item.summary.contains("agenda precedence rebuilt")));
    }

    #[test]
    fn focus_constraints_and_principles_do_not_enter_next_moves_or_verified_facts() {
        let user = vec![message(
            "user",
            "Constraints\n- Keep legacy files unchanged for one release cycle.\nPrinciples\n- Keep freshness ownership inside Session Brain.\nKnown Facts / Evidence\n- This already stays visible.",
        )];

        let focus = build_session_focus(&user, &[]);

        assert!(focus.next_move_candidates.is_empty());
        assert!(focus.verified_facts.is_empty());
    }

    #[test]
    fn root_level_meta_instruction_does_not_become_durable_state() {
        let user = vec![message(
            "user",
            "Keep this skill simple. Do not summarize unless the user asks for interpretation.\nmake it a simple skill that runs that command please",
        )];

        let focus = build_session_focus(&user, &[]);

        assert!(focus.decisions.is_empty());
        assert!(focus.rejections.is_empty());
        assert_eq!(
            focus
                .current_ask_candidates
                .first()
                .map(|item| item.summary.as_str()),
            Some("make it a simple skill that runs that command please")
        );
    }

    #[test]
    fn root_level_skill_invocation_becomes_current_ask() {
        let focus = build_session_focus(&[message("user", "$munin-brain")], &[]);

        assert_eq!(
            focus
                .current_ask_candidates
                .first()
                .map(|item| item.summary.as_str()),
            Some("$munin-brain")
        );
        assert!(focus.suppresses_machine_fallback());
    }

    #[test]
    fn table_heading_ending_with_colon_is_not_current_ask() {
        let focus = build_session_focus(
            &[message(
                "user",
                "What the 5 UserPromptSubmit hooks actually do:",
            )],
            &[],
        );

        assert!(focus.current_ask_candidates.is_empty());
        assert!(focus.redirects.is_empty());
    }

    #[test]
    fn dollar_skill_with_arguments_becomes_current_ask() {
        let focus = build_session_focus(
            &[message(
                "user",
                "$ralph to completion, boil the lake, don't come back until everything is complete",
            )],
            &[],
        );

        assert_eq!(
            focus
                .current_ask_candidates
                .first()
                .map(|item| item.summary.as_str()),
            Some(
                "$ralph to completion, boil the lake, don't come back until everything is complete"
            )
        );
    }

    #[test]
    fn dissatisfaction_with_garbage_suppresses_machine_fallback() {
        let focus = build_session_focus(
            &[message("user", "ALmost all of this is absolute garbage")],
            &[],
        );

        assert!(focus.suppression_signals.iter().any(|item| item
            .summary
            .to_ascii_lowercase()
            .contains("absolute garbage")));
        assert!(focus.suppresses_machine_fallback());
    }

    #[test]
    fn assistant_rerun_progress_does_not_become_finding() {
        let focus = build_session_focus(
            &[],
            &[message(
                "assistant",
                "The full verification suite passed again after the fast user-context change. I’m rerunning the real `brain`/`resume` prompt timings with the fixed debug binary and no session env vars.",
            )],
        );

        assert!(focus.findings.is_empty());
        assert!(focus.verified_facts.is_empty());
        assert!(focus.next_move_candidates.is_empty());
    }

    #[test]
    fn assistant_build_progress_does_not_become_finding() {
        let focus = build_session_focus(
            &[],
            &[message(
                "assistant",
                "The debug CLI now reports `source=live`, keeps the current ask, has no bogus findings/blockers/verified facts, and points at the Munin worktree. Full tests pass. I’m building release and updating the live binary.",
            )],
        );

        assert!(focus.findings.is_empty());
        assert!(focus.verified_facts.is_empty());
    }

    #[test]
    fn assistant_staging_progress_does_not_become_blocker_or_finding() {
        let focus = build_session_focus(
            &[],
            &[message(
                "assistant",
                "Architect found another real staging blocker: `src/core/worldview.rs` had the correct Munin changes in the working tree but was not staged. I’m staging it and rerunning the final sign-off.",
            )],
        );

        assert!(focus.findings.is_empty());
        assert!(focus.blockers.is_empty());
    }

    #[test]
    fn assistant_ralph_progress_does_not_become_finding() {
        let focus = build_session_focus(
            &[],
            &[message(
                "assistant",
                "Continuing Ralph as a completion loop. I’m treating “complete” as the full Munin end-user runtime-context cutover.",
            )],
        );

        assert!(focus.findings.is_empty());
        assert!(focus.verified_facts.is_empty());
    }

    #[test]
    fn assistant_session_brain_readout_does_not_reenter_evidence() {
        let focus = build_session_focus(
            &[],
            &[message(
                "assistant",
                "Here are the actual details from `munin brain --format prompt` that I saw.\n\nsource_status: live\n\nAgenda\ncurrent ask: Resolve active failure: cargo test: 1 errors\n\nState\nfinding: cargo test: 1 errors",
            )],
        );

        assert!(focus.current_ask_candidates.is_empty());
        assert!(focus.findings.is_empty());
        assert!(focus.blockers.is_empty());
        assert!(focus.verified_facts.is_empty());
    }

    #[test]
    fn assistant_runtime_context_packet_does_not_reenter_evidence() {
        let focus = build_session_focus(
            &[],
            &[message(
                "assistant",
                "<runtime_context_v1 surface=\"brain\" source_mode=\"fallback-latest\"><redirect><reason>Session Brain is not live.</reason><recommended_command>munin resume --format prompt</recommended_command></redirect></runtime_context_v1>",
            )],
        );

        assert!(focus.current_ask_candidates.is_empty());
        assert!(focus.findings.is_empty());
        assert!(focus.blockers.is_empty());
        assert!(focus.verified_facts.is_empty());
    }

    #[test]
    fn vague_already_fixed_language_does_not_become_verified() {
        let focus = build_session_focus(
            &[message(
                "user",
                "Known Facts / Evidence\n- problems that were already fixed",
            )],
            &[],
        );

        assert!(focus.verified_facts.is_empty());
        assert!(focus.resolved_blockers.is_empty());
    }
}
