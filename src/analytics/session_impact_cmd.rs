//! Derives continuity and correction metrics from saved agent session logs.

use crate::core::tracking::{ContextItemEventRow, Tracker};
use crate::rewrite_engine::detector::{extract_base_command, find_corrections, CommandExecution};
use anyhow::{Context, Result};
use chrono::{DateTime, Local, NaiveDate, Utc};
use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum SessionSource {
    Claude,
    Codex,
    Recall,
}

impl SessionSource {
    fn from_flag(value: &str) -> Result<Option<Self>> {
        match value.to_ascii_lowercase().as_str() {
            "all" => Ok(None),
            "claude" => Ok(Some(Self::Claude)),
            "codex" => Ok(Some(Self::Codex)),
            other => anyhow::bail!("unsupported source '{}'; use all, claude, or codex", other),
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Recall => "recall",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CommandOutcome {
    Success,
    Failure,
    Unknown,
}

impl CommandOutcome {
    pub(crate) fn is_success(self) -> bool {
        matches!(self, Self::Success)
    }

    pub(crate) fn is_failure(self) -> bool {
        matches!(self, Self::Failure)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct UserPrompt {
    pub(crate) timestamp: DateTime<Utc>,
    #[allow(dead_code)]
    pub(crate) text: String,
}

#[derive(Debug, Clone)]
pub(crate) struct ShellExecution {
    pub(crate) timestamp: DateTime<Utc>,
    pub(crate) command: String,
    pub(crate) output: String,
    pub(crate) outcome: CommandOutcome,
}

#[derive(Debug, Clone)]
pub(crate) struct SessionRecord {
    pub(crate) source: SessionSource,
    pub(crate) session_id: String,
    pub(crate) cwd: String,
    pub(crate) started_at: DateTime<Utc>,
    pub(crate) user_prompts: Vec<UserPrompt>,
    pub(crate) shells: Vec<ShellExecution>,
}

#[derive(Debug, Clone, Serialize)]
struct ScopeSummary {
    source: String,
    project_filter: Option<String>,
    since_days: Option<u64>,
    before_today: bool,
    before_today_cutoff_local_date: Option<String>,
    before_date: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
struct ImpactCounters {
    sessions_scanned: usize,
    sessions_with_shell: usize,
    shell_commands: usize,
    successful_shell_commands: usize,
    failed_shell_commands: usize,
    user_redirects: usize,
    redirected_sessions: usize,
    redirects_with_resumed_shell: usize,
    redirects_with_success_after_resume: usize,
    resumed_redirect_sessions: usize,
    successful_redirect_sessions: usize,
    cli_correction_pairs: usize,
    sessions_with_cli_corrections: usize,
    duplicate_shell_failures: usize,
}

#[derive(Debug, Clone, Serialize)]
struct ImpactTotals {
    #[serde(flatten)]
    counters: ImpactCounters,
    avg_shell_commands_to_success_after_redirect: Option<f64>,
    avg_seconds_to_success_after_redirect: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
struct SourceImpact {
    source: SessionSource,
    totals: ImpactTotals,
}

#[derive(Debug, Clone, Serialize)]
struct SessionImpactReport {
    generated_at: String,
    scope: ScopeSummary,
    totals: ImpactTotals,
    memory_hits: MemoryHitTotals,
    by_source: Vec<SourceImpact>,
    definitions: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
struct MemoryHitSection {
    section: String,
    hits: usize,
}

#[derive(Debug, Clone, Default, Serialize)]
struct MemoryHitItem {
    item_id: String,
    section: String,
    hits: usize,
    summary: String,
}

#[derive(Debug, Clone, Default, Serialize)]
struct MemoryHitTotals {
    packet_count: usize,
    item_hits: usize,
    sessions_with_hits: usize,
    redirects_after_hit: usize,
    successful_redirects_after_hit: usize,
    sections: Vec<MemoryHitSection>,
    top_items: Vec<MemoryHitItem>,
}

#[derive(Debug, Clone, Default)]
struct RunningImpact {
    counters: ImpactCounters,
    redirect_success_commands_sum: usize,
    redirect_success_seconds_sum: f64,
}

struct ClaudePendingCommand {
    command: String,
    timestamp: DateTime<Utc>,
}

struct CodexPendingCommand {
    command: String,
    timestamp: DateTime<Utc>,
}

impl RunningImpact {
    fn add_session(&mut self, session: &SessionRecord) {
        self.counters.sessions_scanned += 1;

        if session.shells.is_empty() {
            return;
        }

        self.counters.sessions_with_shell += 1;
        self.counters.shell_commands += session.shells.len();
        self.counters.successful_shell_commands += session
            .shells
            .iter()
            .filter(|shell| shell.outcome.is_success())
            .count();
        self.counters.failed_shell_commands += session
            .shells
            .iter()
            .filter(|shell| shell.outcome.is_failure())
            .count();

        let command_history: Vec<CommandExecution> = session
            .shells
            .iter()
            .map(|shell| CommandExecution {
                command: shell.command.clone(),
                is_error: shell.outcome.is_failure(),
                output: shell.output.clone(),
            })
            .collect();
        let corrections = find_corrections(&command_history);
        self.counters.cli_correction_pairs += corrections.len();
        if !corrections.is_empty() {
            self.counters.sessions_with_cli_corrections += 1;
        }

        self.counters.duplicate_shell_failures += count_duplicate_failures(&session.shells);

        let Some(first_shell_ts) = session.shells.first().map(|shell| shell.timestamp) else {
            return;
        };
        let redirects: Vec<&UserPrompt> = session
            .user_prompts
            .iter()
            .filter(|prompt| prompt.timestamp > first_shell_ts)
            .collect();

        if redirects.is_empty() {
            return;
        }

        self.counters.user_redirects += redirects.len();
        self.counters.redirected_sessions += 1;

        let mut session_had_resume = false;
        let mut session_had_success = false;

        for redirect in redirects {
            let shells_after = session
                .shells
                .iter()
                .filter(|shell| shell.timestamp > redirect.timestamp);

            if shells_after.clone().next().is_some() {
                self.counters.redirects_with_resumed_shell += 1;
                session_had_resume = true;
            }

            let mut commands_until_success = 0usize;
            let mut first_success: Option<&ShellExecution> = None;
            for shell in shells_after {
                commands_until_success += 1;
                if shell.outcome.is_success() {
                    first_success = Some(shell);
                    break;
                }
            }

            if let Some(success) = first_success {
                self.counters.redirects_with_success_after_resume += 1;
                self.redirect_success_commands_sum += commands_until_success;
                self.redirect_success_seconds_sum +=
                    (success.timestamp - redirect.timestamp).num_milliseconds() as f64 / 1000.0;
                session_had_success = true;
            }
        }

        if session_had_resume {
            self.counters.resumed_redirect_sessions += 1;
        }
        if session_had_success {
            self.counters.successful_redirect_sessions += 1;
        }
    }

    fn finalize(&self) -> ImpactTotals {
        let success_count = self.counters.redirects_with_success_after_resume;
        ImpactTotals {
            counters: self.counters.clone(),
            avg_shell_commands_to_success_after_redirect: if success_count > 0 {
                Some(self.redirect_success_commands_sum as f64 / success_count as f64)
            } else {
                None
            },
            avg_seconds_to_success_after_redirect: if success_count > 0 {
                Some(self.redirect_success_seconds_sum / success_count as f64)
            } else {
                None
            },
        }
    }
}

pub fn run(
    project: Option<String>,
    all: bool,
    since: Option<u64>,
    before_today: bool,
    before_date: Option<String>,
    source: String,
    format: String,
) -> Result<()> {
    let source_filter = SessionSource::from_flag(&source)?;
    let before_cutoff = resolve_before_cutoff(before_today, before_date.as_deref())?;
    let project_filter = if all {
        None
    } else if let Some(project) = project {
        Some(project)
    } else {
        Some(
            std::env::current_dir()
                .context("failed to resolve current directory")?
                .to_string_lossy()
                .to_string(),
        )
    };

    let sessions = load_sessions(
        project_filter.as_deref(),
        since,
        before_cutoff,
        source_filter,
    )?;
    let tracker = Tracker::new()?;
    let mut memory_events = tracker.get_context_item_events_filtered(
        project_filter.as_deref(),
        since.map(|days| Utc::now() - chrono::Duration::days(days as i64)),
    )?;
    if let Some(cutoff_date) = before_cutoff {
        memory_events
            .retain(|event| event.timestamp.with_timezone(&Local).date_naive() < cutoff_date);
    }
    let report = build_report(
        &sessions,
        &memory_events,
        ScopeSummary {
            source: source_filter
                .map(SessionSource::as_str)
                .unwrap_or("all")
                .to_string(),
            project_filter,
            since_days: since,
            before_today,
            before_today_cutoff_local_date: before_today
                .then(|| Local::now().date_naive().to_string()),
            before_date: before_cutoff.map(|date| date.to_string()),
        },
    );

    match format.as_str() {
        "json" => println!("{}", serde_json::to_string_pretty(&report)?),
        "text" => render_text(&report),
        other => anyhow::bail!("unsupported format '{}'; use text or json", other),
    }

    Ok(())
}

fn build_report(
    sessions: &[SessionRecord],
    memory_events: &[ContextItemEventRow],
    scope: ScopeSummary,
) -> SessionImpactReport {
    let mut total = RunningImpact::default();
    let mut by_source = Vec::new();

    for source in [SessionSource::Claude, SessionSource::Codex] {
        let source_sessions: Vec<_> = sessions
            .iter()
            .filter(|session| session.source == source)
            .cloned()
            .collect();
        if source_sessions.is_empty() {
            continue;
        }

        let mut running = RunningImpact::default();
        for session in &source_sessions {
            running.add_session(session);
            total.add_session(session);
        }
        by_source.push(SourceImpact {
            source,
            totals: running.finalize(),
        });
    }

    SessionImpactReport {
        generated_at: Utc::now().to_rfc3339(),
        scope,
        totals: total.finalize(),
        memory_hits: build_memory_hit_totals(sessions, memory_events),
        by_source,
        definitions: vec![
            "User redirect = a non-tool-result user message that arrived after shell execution had already started in the session.".to_string(),
            "CLI correction pair = a fail-then-correct shell command pair detected from saved session history.".to_string(),
            "Duplicate shell failure = a repeated normalized failure signature in the same session.".to_string(),
            "Resume success after redirect = the session later produced at least one successful shell command after that redirect.".to_string(),
            "Memory hit = a selected context manifest item recorded when a context packet was compiled.".to_string(),
        ],
    }
}

fn render_text(report: &SessionImpactReport) {
    println!("Context Session Impact");
    println!("{}", "═".repeat(60));
    println!();

    println!("Scope");
    println!("{}", "-".repeat(60));
    println!("Source: {}", report.scope.source);
    println!(
        "Project filter: {}",
        report
            .scope
            .project_filter
            .as_deref()
            .unwrap_or("all projects")
    );
    println!(
        "Since days: {}",
        report
            .scope
            .since_days
            .map(|days| days.to_string())
            .unwrap_or_else(|| "all history".to_string())
    );
    println!(
        "Before cutoff date: {}",
        report.scope.before_date.as_deref().unwrap_or("none")
    );
    println!();

    render_totals("Totals", &report.totals);
    render_memory_hits(&report.memory_hits);

    if !report.by_source.is_empty() {
        println!("By Source");
        println!("{}", "-".repeat(60));
        println!(
            "{:<10} {:>8} {:>8} {:>10} {:>10} {:>11}",
            "Source", "Sessions", "Shells", "Redirects", "Corrections", "Dup fails"
        );
        println!("{}", "-".repeat(60));
        for source in &report.by_source {
            println!(
                "{:<10} {:>8} {:>8} {:>10} {:>10} {:>11}",
                source.source.as_str(),
                source.totals.counters.sessions_scanned,
                source.totals.counters.shell_commands,
                source.totals.counters.user_redirects,
                source.totals.counters.cli_correction_pairs,
                source.totals.counters.duplicate_shell_failures,
            );
        }
        println!();
    }

    println!("Definitions");
    println!("{}", "-".repeat(60));
    for definition in &report.definitions {
        println!("- {}", definition);
    }
}

fn render_totals(title: &str, totals: &ImpactTotals) {
    println!("{}", title);
    println!("{}", "-".repeat(60));
    println!(
        "Sessions scanned:                     {}",
        totals.counters.sessions_scanned
    );
    println!(
        "Sessions with shell work:            {}",
        totals.counters.sessions_with_shell
    );
    println!(
        "Shell commands:                      {}",
        totals.counters.shell_commands
    );
    println!(
        "Successful shell commands:           {}",
        totals.counters.successful_shell_commands
    );
    println!(
        "Failed shell commands:               {}",
        totals.counters.failed_shell_commands
    );
    println!(
        "User redirects:                      {}",
        totals.counters.user_redirects
    );
    println!(
        "Redirected sessions:                 {}",
        totals.counters.redirected_sessions
    );
    println!(
        "Redirects with resumed shell work:   {}",
        totals.counters.redirects_with_resumed_shell
    );
    println!(
        "Redirects with later success:        {}",
        totals.counters.redirects_with_success_after_resume
    );
    println!(
        "Resumed redirect sessions:           {}",
        totals.counters.resumed_redirect_sessions
    );
    println!(
        "Successful redirect sessions:        {}",
        totals.counters.successful_redirect_sessions
    );
    println!(
        "CLI correction pairs:                {}",
        totals.counters.cli_correction_pairs
    );
    println!(
        "Sessions with CLI corrections:       {}",
        totals.counters.sessions_with_cli_corrections
    );
    println!(
        "Duplicate shell failures:            {}",
        totals.counters.duplicate_shell_failures
    );
    println!(
        "Avg cmds to success after redirect:  {}",
        format_optional_float(totals.avg_shell_commands_to_success_after_redirect, "cmds")
    );
    println!(
        "Avg sec to success after redirect:   {}",
        format_optional_float(totals.avg_seconds_to_success_after_redirect, "s")
    );
    println!();
}

fn render_memory_hits(memory_hits: &MemoryHitTotals) {
    println!("Memory Hits");
    println!("{}", "-".repeat(60));
    println!(
        "Memory-hit packets:                  {}",
        memory_hits.packet_count
    );
    println!(
        "Memory-hit item rows:                {}",
        memory_hits.item_hits
    );
    println!(
        "Sessions with memory hits:           {}",
        memory_hits.sessions_with_hits
    );
    println!(
        "Redirects after memory hits:         {}",
        memory_hits.redirects_after_hit
    );
    println!(
        "Successful redirects after hits:     {}",
        memory_hits.successful_redirects_after_hit
    );
    if !memory_hits.sections.is_empty() {
        let section_summary = memory_hits
            .sections
            .iter()
            .map(|section| format!("{}={}", section.section, section.hits))
            .collect::<Vec<_>>()
            .join(", ");
        println!("Sections:                            {}", section_summary);
    }
    if !memory_hits.top_items.is_empty() {
        println!("Top surfaced items:");
        for item in &memory_hits.top_items {
            println!("  - [{}] {} x{}", item.section, item.summary, item.hits);
        }
    }
    println!();
}

fn format_optional_float(value: Option<f64>, suffix: &str) -> String {
    value
        .map(|value| format!("{:.1} {}", value, suffix))
        .unwrap_or_else(|| "n/a".to_string())
}

fn build_memory_hit_totals(
    sessions: &[SessionRecord],
    memory_events: &[ContextItemEventRow],
) -> MemoryHitTotals {
    let mut section_counts: HashMap<String, usize> = HashMap::new();
    let mut item_counts: HashMap<(String, String), (usize, String)> = HashMap::new();
    for event in memory_events {
        *section_counts.entry(event.section.clone()).or_default() += 1;
        let entry = item_counts
            .entry((event.item_id.clone(), event.section.clone()))
            .or_insert((0, event.summary.clone()));
        entry.0 += 1;
        if entry.1.is_empty() {
            entry.1 = event.summary.clone();
        }
    }

    let packet_count = memory_events
        .iter()
        .map(|event| event.packet_id.clone())
        .collect::<std::collections::HashSet<_>>()
        .len();

    let mut sessions_with_hits = 0usize;
    let mut redirects_after_hit = 0usize;
    let mut successful_redirects_after_hit = 0usize;

    for session in sessions {
        let session_events = memory_events_for_session(session, memory_events);
        if session_events.is_empty() {
            continue;
        }

        sessions_with_hits += 1;
        let redirects = redirect_outcomes(session);
        for redirect in redirects {
            if session_events
                .iter()
                .any(|event| event.timestamp <= redirect.timestamp)
            {
                redirects_after_hit += 1;
                if redirect.has_success_after_resume {
                    successful_redirects_after_hit += 1;
                }
            }
        }
    }

    let mut sections = section_counts
        .into_iter()
        .map(|(section, hits)| MemoryHitSection { section, hits })
        .collect::<Vec<_>>();
    sections.sort_by(|left, right| {
        right
            .hits
            .cmp(&left.hits)
            .then_with(|| left.section.cmp(&right.section))
    });

    let mut top_items = item_counts
        .into_iter()
        .map(|((item_id, section), (hits, summary))| MemoryHitItem {
            item_id,
            section,
            hits,
            summary,
        })
        .collect::<Vec<_>>();
    top_items.sort_by(|left, right| {
        right
            .hits
            .cmp(&left.hits)
            .then_with(|| left.section.cmp(&right.section))
            .then_with(|| left.item_id.cmp(&right.item_id))
    });
    top_items.truncate(10);

    MemoryHitTotals {
        packet_count,
        item_hits: memory_events.len(),
        sessions_with_hits,
        redirects_after_hit,
        successful_redirects_after_hit,
        sections,
        top_items,
    }
}

#[derive(Debug, Clone, Copy)]
struct RedirectOutcome {
    timestamp: DateTime<Utc>,
    has_success_after_resume: bool,
}

fn redirect_outcomes(session: &SessionRecord) -> Vec<RedirectOutcome> {
    let Some(first_shell_ts) = session.shells.first().map(|shell| shell.timestamp) else {
        return Vec::new();
    };
    session
        .user_prompts
        .iter()
        .filter(|prompt| prompt.timestamp > first_shell_ts)
        .map(|prompt| RedirectOutcome {
            timestamp: prompt.timestamp,
            has_success_after_resume: session
                .shells
                .iter()
                .any(|shell| shell.timestamp > prompt.timestamp && shell.outcome.is_success()),
        })
        .collect()
}

fn memory_events_for_session<'a>(
    session: &SessionRecord,
    events: &'a [ContextItemEventRow],
) -> Vec<&'a ContextItemEventRow> {
    let session_end = session_end(session);
    let session_project = normalize_path(&session.cwd);

    events
        .iter()
        .filter(|event| {
            let event_project = normalize_path(&event.project_path);
            path_matches_project(&session_project, &event_project)
                && event.timestamp >= session.started_at
                && event.timestamp <= session_end
        })
        .collect()
}

fn session_end(session: &SessionRecord) -> DateTime<Utc> {
    session
        .user_prompts
        .iter()
        .map(|prompt| prompt.timestamp)
        .chain(session.shells.iter().map(|shell| shell.timestamp))
        .max()
        .unwrap_or(session.started_at)
}

fn normalize_path(path: &str) -> String {
    path.replace('\\', "/").to_ascii_lowercase()
}

fn path_matches_project(path: &str, project_path: &str) -> bool {
    path == project_path || path.starts_with(&(project_path.to_string() + "/"))
}

fn sort_and_dedup_prompts(prompts: &mut Vec<UserPrompt>) {
    prompts.sort_by(|left, right| {
        left.timestamp
            .cmp(&right.timestamp)
            .then_with(|| left.text.cmp(&right.text))
    });
    let mut deduped = Vec::with_capacity(prompts.len());
    for prompt in prompts.drain(..) {
        let normalized = normalize_prompt_text(&prompt.text);
        let is_duplicate = deduped.last().map(|last: &UserPrompt| {
            normalize_prompt_text(&last.text) == normalized
                && (prompt.timestamp - last.timestamp).num_seconds().abs() <= 5
        });
        if is_duplicate.unwrap_or(false) {
            continue;
        }
        deduped.push(prompt);
    }
    *prompts = deduped;
}

fn normalize_prompt_text(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

fn resolve_before_cutoff(
    before_today: bool,
    before_date: Option<&str>,
) -> Result<Option<NaiveDate>> {
    if let Some(value) = before_date {
        return Ok(Some(
            NaiveDate::parse_from_str(value, "%Y-%m-%d").with_context(|| {
                format!("invalid --before-date '{}', expected YYYY-MM-DD", value)
            })?,
        ));
    }

    Ok(before_today.then(|| Local::now().date_naive()))
}

pub(crate) fn load_sessions(
    project_filter: Option<&str>,
    since_days: Option<u64>,
    before_cutoff: Option<NaiveDate>,
    source_filter: Option<SessionSource>,
) -> Result<Vec<SessionRecord>> {
    let mut sessions = Vec::new();

    match source_filter {
        Some(SessionSource::Claude) => sessions.extend(load_claude_sessions(
            project_filter,
            since_days,
            before_cutoff,
        )?),
        Some(SessionSource::Codex) => sessions.extend(load_codex_sessions(
            project_filter,
            since_days,
            before_cutoff,
        )?),
        Some(SessionSource::Recall) => {}
        None => {
            sessions.extend(load_claude_sessions(
                project_filter,
                since_days,
                before_cutoff,
            )?);
            sessions.extend(load_codex_sessions(
                project_filter,
                since_days,
                before_cutoff,
            )?);
        }
    }

    sessions.sort_by(|left, right| {
        left.started_at
            .cmp(&right.started_at)
            .then_with(|| left.session_id.cmp(&right.session_id))
    });
    Ok(sessions)
}

fn load_claude_sessions(
    project_filter: Option<&str>,
    since_days: Option<u64>,
    before_cutoff: Option<NaiveDate>,
) -> Result<Vec<SessionRecord>> {
    let Some(home) = session_home_dir() else {
        return Ok(Vec::new());
    };
    let root = home.join(".claude").join("projects");
    if !root.exists() {
        return Ok(Vec::new());
    }

    let mut sessions = Vec::new();
    for entry in WalkDir::new(&root)
        .follow_links(false)
        .into_iter()
        .filter_map(|entry| entry.ok())
    {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
            continue;
        }
        if path.to_string_lossy().contains("subagents") {
            continue;
        }

        if let Some(session) = parse_claude_session(path)? {
            if session_matches_filters(&session, project_filter, since_days, before_cutoff) {
                sessions.push(session);
            }
        }
    }
    Ok(sessions)
}

fn load_codex_sessions(
    project_filter: Option<&str>,
    since_days: Option<u64>,
    before_cutoff: Option<NaiveDate>,
) -> Result<Vec<SessionRecord>> {
    let roots = discover_codex_session_roots()?;
    let mut sessions_by_id: HashMap<String, SessionRecord> = HashMap::new();

    for session in load_codex_history_sessions(project_filter, since_days, before_cutoff)? {
        sessions_by_id.insert(session.session_id.clone(), session);
    }

    if roots.is_empty() {
        let mut sessions = sessions_by_id.into_values().collect::<Vec<_>>();
        sessions.sort_by(|left, right| {
            left.started_at
                .cmp(&right.started_at)
                .then_with(|| left.session_id.cmp(&right.session_id))
        });
        return Ok(sessions);
    }

    for root in roots {
        for entry in WalkDir::new(&root)
            .follow_links(false)
            .into_iter()
            .filter_map(|entry| entry.ok())
        {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
                continue;
            }

            if let Some(session) = parse_codex_session(path)? {
                if !session_matches_filters(&session, project_filter, since_days, before_cutoff) {
                    continue;
                }

                sessions_by_id
                    .entry(session.session_id.clone())
                    .and_modify(|existing| {
                        let better = session.shells.len() > existing.shells.len()
                            || (session.shells.len() == existing.shells.len()
                                && session.user_prompts.len() > existing.user_prompts.len())
                            || (session.shells.len() == existing.shells.len()
                                && session.user_prompts.len() == existing.user_prompts.len()
                                && session.started_at > existing.started_at);
                        if better {
                            *existing = session.clone();
                        }
                    })
                    .or_insert(session);
            }
        }
    }

    let mut sessions = sessions_by_id.into_values().collect::<Vec<_>>();
    sessions.sort_by(|left, right| {
        left.started_at
            .cmp(&right.started_at)
            .then_with(|| left.session_id.cmp(&right.session_id))
    });
    Ok(sessions)
}

fn load_codex_history_sessions(
    project_filter: Option<&str>,
    since_days: Option<u64>,
    before_cutoff: Option<NaiveDate>,
) -> Result<Vec<SessionRecord>> {
    let Some(home) = session_home_dir() else {
        return Ok(Vec::new());
    };
    let history_path = home.join(".codex").join("history.jsonl");
    if !history_path.exists() {
        return Ok(Vec::new());
    }

    load_codex_history_sessions_from_path(&history_path, project_filter, since_days, before_cutoff)
}

fn load_codex_history_sessions_from_path(
    history_path: &Path,
    project_filter: Option<&str>,
    since_days: Option<u64>,
    before_cutoff: Option<NaiveDate>,
) -> Result<Vec<SessionRecord>> {
    let file = File::open(history_path)
        .with_context(|| format!("failed to open {}", history_path.display()))?;
    let reader = BufReader::new(file);
    let mut sessions_by_id: HashMap<String, SessionRecord> = HashMap::new();

    for line in reader.lines() {
        let line = match line {
            Ok(line) => line,
            Err(_) => continue,
        };
        let value: Value = match serde_json::from_str(&line) {
            Ok(value) => value,
            Err(_) => continue,
        };

        let Some(session_id) = value.get("session_id").and_then(Value::as_str) else {
            continue;
        };
        let Some(ts) = value.get("ts").and_then(Value::as_i64) else {
            continue;
        };
        let Some(started_at) = DateTime::<Utc>::from_timestamp(ts, 0) else {
            continue;
        };

        let text = value
            .get("text")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or("");
        if text.is_empty() {
            continue;
        }

        let prompt = UserPrompt {
            timestamp: started_at,
            text: text.to_string(),
        };

        let session = sessions_by_id
            .entry(session_id.to_string())
            .or_insert_with(|| SessionRecord {
                source: SessionSource::Codex,
                session_id: session_id.to_string(),
                cwd: String::new(),
                started_at,
                user_prompts: Vec::new(),
                shells: Vec::new(),
            });

        if started_at < session.started_at {
            session.started_at = started_at;
        }
        session.user_prompts.push(prompt);
    }

    let mut sessions = sessions_by_id
        .into_values()
        .filter(|session| {
            session_matches_filters(session, project_filter, since_days, before_cutoff)
        })
        .collect::<Vec<_>>();
    sessions.sort_by(|left, right| {
        left.started_at
            .cmp(&right.started_at)
            .then_with(|| left.session_id.cmp(&right.session_id))
    });
    Ok(sessions)
}

fn discover_codex_session_roots() -> Result<Vec<PathBuf>> {
    let mut roots = Vec::new();
    let mut seen = std::collections::HashSet::new();

    if let Some(home) = session_home_dir() {
        let primary = home.join(".codex").join("sessions");
        if primary.exists() && seen.insert(primary.clone()) {
            roots.push(primary);
        }
    }

    if explicit_session_home_dir().is_some() {
        roots.sort();
        return Ok(roots);
    }

    let projects_root = Path::new("C:\\Users\\OEM\\Projects");
    if projects_root.exists() {
        for entry in WalkDir::new(projects_root)
            .follow_links(false)
            .into_iter()
            .filter_map(|entry| entry.ok())
        {
            if !entry.file_type().is_dir() {
                continue;
            }
            let path = entry.path();
            let path_str = path
                .to_string_lossy()
                .to_ascii_lowercase()
                .replace('/', "\\");
            let is_codex_sessions = path_str.ends_with("\\.codex\\sessions")
                || path_str.ends_with("\\.codex-omx2\\sessions")
                || path_str.ends_with("\\codex-state\\sessions");
            if is_codex_sessions {
                let owned = path.to_path_buf();
                if seen.insert(owned.clone()) {
                    roots.push(owned);
                }
            }
        }
    }

    Ok(roots)
}

fn session_home_dir() -> Option<PathBuf> {
    explicit_session_home_dir().or_else(dirs::home_dir)
}

fn explicit_session_home_dir() -> Option<PathBuf> {
    std::env::var_os("MUNIN_SESSION_HOME")
        .or_else(|| std::env::var_os("MUNIN_INSTALL_HOME"))
        .map(PathBuf::from)
}

fn session_matches_filters(
    session: &SessionRecord,
    project_filter: Option<&str>,
    since_days: Option<u64>,
    before_cutoff: Option<NaiveDate>,
) -> bool {
    if let Some(filter) = project_filter {
        let filter = filter.to_ascii_lowercase();
        if !session.cwd.to_ascii_lowercase().contains(&filter) {
            return false;
        }
    }

    if let Some(days) = since_days {
        let cutoff = Utc::now() - chrono::Duration::days(days as i64);
        if session.started_at < cutoff {
            return false;
        }
    }

    if let Some(cutoff_date) = before_cutoff {
        let session_local = session.started_at.with_timezone(&Local).date_naive();
        if session_local >= cutoff_date {
            return false;
        }
    }

    true
}

fn parse_claude_session(path: &Path) -> Result<Option<SessionRecord>> {
    let file = File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let reader = BufReader::new(file);

    let session_id = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("unknown")
        .to_string();

    let mut cwd = String::new();
    let mut started_at: Option<DateTime<Utc>> = None;
    let mut user_prompts = Vec::new();
    let mut shells = Vec::new();
    let mut pending: HashMap<String, ClaudePendingCommand> = HashMap::new();

    for line in reader.lines() {
        let line = match line {
            Ok(line) => line,
            Err(_) => continue,
        };
        let entry: Value = match serde_json::from_str(&line) {
            Ok(value) => value,
            Err(_) => continue,
        };
        if entry
            .get("isSidechain")
            .and_then(|value| value.as_bool())
            .unwrap_or(false)
        {
            continue;
        }

        let timestamp = entry
            .get("timestamp")
            .and_then(|value| value.as_str())
            .and_then(parse_timestamp);

        if started_at.is_none() {
            started_at = timestamp;
        }
        if cwd.is_empty() {
            cwd = entry
                .get("cwd")
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .to_string();
        }

        match entry
            .get("type")
            .and_then(|value| value.as_str())
            .unwrap_or("")
        {
            "assistant" => {
                if let Some(content) = entry
                    .pointer("/message/content")
                    .and_then(|value| value.as_array())
                {
                    for block in content {
                        if block.get("type").and_then(|value| value.as_str()) == Some("tool_use")
                            && block.get("name").and_then(|value| value.as_str()) == Some("Bash")
                        {
                            if let (Some(tool_id), Some(command), Some(ts)) = (
                                block.get("id").and_then(|value| value.as_str()),
                                block
                                    .pointer("/input/command")
                                    .and_then(|value| value.as_str()),
                                timestamp,
                            ) {
                                pending.insert(
                                    tool_id.to_string(),
                                    ClaudePendingCommand {
                                        command: command.to_string(),
                                        timestamp: ts,
                                    },
                                );
                            }
                        }
                    }
                }
            }
            "user" => {
                if let Some(prompt_text) = extract_claude_user_prompt(&entry) {
                    if let Some(ts) = timestamp {
                        user_prompts.push(UserPrompt {
                            timestamp: ts,
                            text: prompt_text,
                        });
                    }
                }

                if let Some(content) = entry
                    .pointer("/message/content")
                    .and_then(|value| value.as_array())
                {
                    for block in content {
                        if block.get("type").and_then(|value| value.as_str()) != Some("tool_result")
                        {
                            continue;
                        }
                        let Some(tool_id) =
                            block.get("tool_use_id").and_then(|value| value.as_str())
                        else {
                            continue;
                        };
                        let Some(pending_command) = pending.remove(tool_id) else {
                            continue;
                        };

                        let output = block
                            .get("content")
                            .and_then(|value| value.as_str())
                            .unwrap_or("")
                            .to_string();
                        let is_error = block
                            .get("is_error")
                            .and_then(|value| value.as_bool())
                            .unwrap_or(false);
                        let interrupted = entry
                            .pointer("/toolUseResult/interrupted")
                            .and_then(|value| value.as_bool())
                            .unwrap_or(false);
                        let outcome = if interrupted || is_error {
                            CommandOutcome::Failure
                        } else {
                            CommandOutcome::Success
                        };

                        shells.push(ShellExecution {
                            timestamp: timestamp.unwrap_or(pending_command.timestamp),
                            command: pending_command.command,
                            output,
                            outcome,
                        });
                    }
                }
            }
            _ => {}
        }
    }

    if shells.is_empty() && user_prompts.is_empty() {
        return Ok(None);
    }
    sort_and_dedup_prompts(&mut user_prompts);

    Ok(Some(SessionRecord {
        source: SessionSource::Claude,
        session_id,
        cwd,
        started_at: started_at.unwrap_or_else(Utc::now),
        user_prompts,
        shells,
    }))
}

fn extract_claude_user_prompt(entry: &Value) -> Option<String> {
    let content = entry.pointer("/message/content")?;
    if let Some(text) = content.as_str() {
        let trimmed = text.trim();
        return (!trimmed.is_empty()).then(|| trimmed.to_string());
    }

    let blocks = content.as_array()?;
    let mut parts = Vec::new();
    for block in blocks {
        match block.get("type").and_then(|value| value.as_str()) {
            Some("tool_result") => return None,
            Some("text") => {
                if let Some(text) = block.get("text").and_then(|value| value.as_str()) {
                    let trimmed = text.trim();
                    if !trimmed.is_empty() {
                        parts.push(trimmed.to_string());
                    }
                }
            }
            _ => {}
        }
    }
    (!parts.is_empty()).then(|| parts.join("\n"))
}

fn parse_codex_session(path: &Path) -> Result<Option<SessionRecord>> {
    let file = File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let reader = BufReader::new(file);

    let mut session_id = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("unknown")
        .to_string();
    let mut cwd = String::new();
    let mut started_at: Option<DateTime<Utc>> = None;
    let mut user_prompts = Vec::new();
    let mut shells = Vec::new();
    let mut pending: HashMap<String, CodexPendingCommand> = HashMap::new();
    let mut _is_subagent = false;

    for line in reader.lines() {
        let line = match line {
            Ok(line) => line,
            Err(_) => continue,
        };
        let entry: Value = match serde_json::from_str(&line) {
            Ok(value) => value,
            Err(_) => continue,
        };

        let timestamp = entry
            .get("timestamp")
            .and_then(|value| value.as_str())
            .and_then(parse_timestamp);

        if started_at.is_none() {
            started_at = timestamp;
        }

        match entry
            .get("type")
            .and_then(|value| value.as_str())
            .unwrap_or("")
        {
            "session_meta" => {
                if let Some(payload) = entry.get("payload") {
                    if payload.pointer("/source/subagent/thread_spawn").is_some() {
                        _is_subagent = true;
                    }
                    session_id = payload
                        .get("id")
                        .and_then(|value| value.as_str())
                        .unwrap_or(&session_id)
                        .to_string();
                    cwd = payload
                        .get("cwd")
                        .and_then(|value| value.as_str())
                        .unwrap_or("")
                        .to_string();
                    if let Some(meta_ts) = payload
                        .get("timestamp")
                        .and_then(|value| value.as_str())
                        .and_then(parse_timestamp)
                    {
                        started_at = Some(meta_ts);
                    }
                }
            }
            "event_msg" => {
                if let Some(payload) = entry.get("payload") {
                    if payload.get("type").and_then(|value| value.as_str()) == Some("user_message")
                    {
                        if let (Some(text), Some(ts)) = (
                            payload.get("message").and_then(|value| value.as_str()),
                            timestamp,
                        ) {
                            let trimmed = text.trim();
                            if !trimmed.is_empty() {
                                user_prompts.push(UserPrompt {
                                    timestamp: ts,
                                    text: trimmed.to_string(),
                                });
                            }
                        }
                    }
                }
            }
            "response_item" => {
                let Some(payload) = entry.get("payload") else {
                    continue;
                };
                if let Some(text) = extract_codex_user_prompt_from_payload(payload) {
                    if let Some(ts) = timestamp {
                        user_prompts.push(UserPrompt {
                            timestamp: ts,
                            text,
                        });
                    }
                }
                match payload.get("type").and_then(|value| value.as_str()) {
                    Some("function_call")
                        if payload.get("name").and_then(|value| value.as_str())
                            == Some("shell_command") =>
                    {
                        let Some(call_id) = payload.get("call_id").and_then(|value| value.as_str())
                        else {
                            continue;
                        };
                        let Some(arguments) =
                            payload.get("arguments").and_then(|value| value.as_str())
                        else {
                            continue;
                        };
                        let command =
                            serde_json::from_str::<Value>(arguments)
                                .ok()
                                .and_then(|args| {
                                    if cwd.is_empty() {
                                        cwd = args
                                            .get("workdir")
                                            .and_then(|value| value.as_str())
                                            .unwrap_or("")
                                            .to_string();
                                    }
                                    args.get("command")
                                        .and_then(|value| value.as_str())
                                        .map(str::to_string)
                                });
                        if let (Some(command), Some(ts)) = (command, timestamp) {
                            pending.insert(
                                call_id.to_string(),
                                CodexPendingCommand {
                                    command,
                                    timestamp: ts,
                                },
                            );
                        }
                    }
                    Some("function_call_output") => {
                        let Some(call_id) = payload.get("call_id").and_then(|value| value.as_str())
                        else {
                            continue;
                        };
                        let Some(pending_command) = pending.remove(call_id) else {
                            continue;
                        };
                        let output = payload
                            .get("output")
                            .and_then(|value| value.as_str())
                            .unwrap_or("")
                            .to_string();
                        let outcome = parse_codex_shell_outcome(&output);
                        shells.push(ShellExecution {
                            timestamp: timestamp.unwrap_or(pending_command.timestamp),
                            command: pending_command.command,
                            output,
                            outcome,
                        });
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    if shells.is_empty() && user_prompts.is_empty() {
        return Ok(None);
    }
    sort_and_dedup_prompts(&mut user_prompts);

    Ok(Some(SessionRecord {
        source: SessionSource::Codex,
        session_id,
        cwd,
        started_at: started_at.unwrap_or_else(Utc::now),
        user_prompts,
        shells,
    }))
}

fn extract_codex_user_prompt_from_payload(payload: &Value) -> Option<String> {
    if payload.get("type").and_then(|value| value.as_str()) != Some("message") {
        return None;
    }
    if payload.get("role").and_then(|value| value.as_str()) != Some("user") {
        return None;
    }

    let content = payload.get("content")?.as_array()?;
    let mut parts = Vec::new();
    for item in content {
        if let Some(text) = item.get("text").and_then(|value| value.as_str()) {
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                parts.push(trimmed.to_string());
                continue;
            }
        }
        if let Some(text) = item.get("content").and_then(|value| value.as_str()) {
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                parts.push(trimmed.to_string());
            }
        }
    }
    (!parts.is_empty()).then(|| parts.join("\n"))
}

fn parse_codex_shell_outcome(output: &str) -> CommandOutcome {
    for line in output.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("Exit code:") {
            return match rest.trim().parse::<i32>() {
                Ok(0) => CommandOutcome::Success,
                Ok(_) => CommandOutcome::Failure,
                Err(_) => CommandOutcome::Unknown,
            };
        }
    }

    if output.contains("command timed out after") {
        CommandOutcome::Failure
    } else {
        CommandOutcome::Unknown
    }
}

fn count_duplicate_failures(shells: &[ShellExecution]) -> usize {
    let mut seen = std::collections::HashSet::new();
    let mut duplicates = 0usize;

    for shell in shells {
        if !shell.outcome.is_failure() {
            continue;
        }

        let signature = normalize_failure_signature(&shell.command, &shell.output);
        if signature.is_empty() {
            continue;
        }

        if !seen.insert(signature) {
            duplicates += 1;
        }
    }

    duplicates
}

fn normalize_failure_signature(command: &str, output: &str) -> String {
    let base = extract_base_command(command);
    let first_line = output
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let normalized = first_line
        .chars()
        .map(|ch| {
            if ch.is_ascii_digit() {
                '#'
            } else if ch.is_ascii_whitespace() {
                ' '
            } else {
                ch
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let normalized = normalized.chars().take(160).collect::<String>();

    if base.is_empty() && normalized.is_empty() {
        String::new()
    } else {
        format!("{}|{}", base, normalized)
    }
}

fn parse_timestamp(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|timestamp| timestamp.with_timezone(&Utc))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    fn write_lines(path: &Path, lines: &[&str]) {
        let mut file = File::create(path).unwrap();
        for line in lines {
            writeln!(file, "{}", line).unwrap();
        }
    }

    #[test]
    fn parses_claude_prompts_and_shells() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("session.jsonl");
        write_lines(
            &path,
            &[
                r#"{"type":"user","message":{"role":"user","content":"run the command"},"timestamp":"2026-04-10T00:00:00Z","cwd":"C:\\repo"}"#,
                r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"git statuz"}}]},"timestamp":"2026-04-10T00:00:01Z","cwd":"C:\\repo"}"#,
                r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"error: command not found","is_error":true}]},"timestamp":"2026-04-10T00:00:02Z","cwd":"C:\\repo","toolUseResult":{"interrupted":false}}"#,
                r#"{"type":"user","message":{"role":"user","content":"actually use git status"},"timestamp":"2026-04-10T00:00:03Z","cwd":"C:\\repo"}"#,
                r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"t2","name":"Bash","input":{"command":"git status"}}]},"timestamp":"2026-04-10T00:00:04Z","cwd":"C:\\repo"}"#,
                r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t2","content":"clean","is_error":false}]},"timestamp":"2026-04-10T00:00:05Z","cwd":"C:\\repo","toolUseResult":{"interrupted":false}}"#,
            ],
        );

        let session = parse_claude_session(&path).unwrap().unwrap();
        assert_eq!(session.cwd, "C:\\repo");
        assert_eq!(session.user_prompts.len(), 2);
        assert_eq!(session.shells.len(), 2);
        assert!(session.shells[0].outcome.is_failure());
        assert!(session.shells[1].outcome.is_success());
    }

    #[test]
    fn parses_codex_shells_and_keeps_subagents_with_content() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("rollout.jsonl");
        write_lines(
            &path,
            &[
                r#"{"timestamp":"2026-04-10T01:00:00Z","type":"session_meta","payload":{"id":"abc","timestamp":"2026-04-10T01:00:00Z","cwd":"C:\\repo"}}"#,
                r#"{"timestamp":"2026-04-10T01:00:01Z","type":"event_msg","payload":{"type":"user_message","message":"do the thing"}}"#,
                r#"{"timestamp":"2026-04-10T01:00:02Z","type":"response_item","payload":{"type":"function_call","name":"shell_command","arguments":"{\"command\":\"rg foo\",\"workdir\":\"C:\\\\repo\"}","call_id":"c1"}}"#,
                r#"{"timestamp":"2026-04-10T01:00:03Z","type":"response_item","payload":{"type":"function_call_output","call_id":"c1","output":"Exit code: 1\nWall time: 0.2 seconds\nOutput:\nno matches"}}"#,
                r#"{"timestamp":"2026-04-10T01:00:04Z","type":"event_msg","payload":{"type":"user_message","message":"actually use rg -n"}}"#,
                r#"{"timestamp":"2026-04-10T01:00:05Z","type":"response_item","payload":{"type":"function_call","name":"shell_command","arguments":"{\"command\":\"rg -n foo .\",\"workdir\":\"C:\\\\repo\"}","call_id":"c2"}}"#,
                r#"{"timestamp":"2026-04-10T01:00:06Z","type":"response_item","payload":{"type":"function_call_output","call_id":"c2","output":"Exit code: 0\nWall time: 0.2 seconds\nOutput:\nmatch"}}"#,
            ],
        );

        let session = parse_codex_session(&path).unwrap().unwrap();
        assert_eq!(session.session_id, "abc");
        assert_eq!(session.cwd, "C:\\repo");
        assert_eq!(session.user_prompts.len(), 2);
        assert_eq!(session.shells.len(), 2);
        assert!(session.shells[0].outcome.is_failure());
        assert!(session.shells[1].outcome.is_success());

        let subagent_path = dir.path().join("subagent.jsonl");
        write_lines(
            &subagent_path,
            &[
                r#"{"timestamp":"2026-04-10T01:00:00Z","type":"session_meta","payload":{"id":"sub","timestamp":"2026-04-10T01:00:00Z","cwd":"C:\\repo","source":{"subagent":{"thread_spawn":{"parent_thread_id":"p"}}}}}"#,
                r#"{"timestamp":"2026-04-10T01:00:01Z","type":"event_msg","payload":{"type":"user_message","message":"check the status"}}"#,
                r#"{"timestamp":"2026-04-10T01:00:02Z","type":"response_item","payload":{"type":"function_call","name":"shell_command","arguments":"{\"command\":\"git status\",\"workdir\":\"C:\\\\repo\"}","call_id":"s1"}}"#,
                r#"{"timestamp":"2026-04-10T01:00:03Z","type":"response_item","payload":{"type":"function_call_output","call_id":"s1","output":"Exit code: 0\nWall time: 0.2 seconds\nOutput:\nclean"}}"#,
            ],
        );
        let sub = parse_codex_session(&subagent_path).unwrap().unwrap();
        assert_eq!(sub.session_id, "sub");
        assert_eq!(sub.user_prompts.len(), 1);
        assert_eq!(sub.shells.len(), 1);
    }

    #[test]
    fn loads_codex_history_as_prompt_only_sessions() {
        let dir = TempDir::new().unwrap();
        let history_path = dir.path().join("history.jsonl");
        write_lines(
            &history_path,
            &[
                r#"{"session_id":"session-a","ts":1772742454,"text":"first prompt"}"#,
                r#"{"session_id":"session-a","ts":1772743887,"text":"follow up"}"#,
                r#"{"session_id":"session-b","ts":1772744000,"text":"another prompt"}"#,
            ],
        );

        let sessions =
            load_codex_history_sessions_from_path(&history_path, None, None, None).unwrap();

        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].session_id, "session-a");
        assert_eq!(sessions[0].user_prompts.len(), 2);
        assert!(sessions[0].shells.is_empty());
        assert_eq!(sessions[1].session_id, "session-b");
        assert_eq!(sessions[1].user_prompts.len(), 1);
        assert!(sessions[1].shells.is_empty());
    }

    #[test]
    fn normalize_failure_signature_handles_multibyte_text() {
        let signature = normalize_failure_signature("npm test", "失敗失敗失敗\nsecond line");
        assert!(signature.starts_with("npm test|"));
        assert!(signature.contains("失敗失敗失敗"));
    }

    #[test]
    fn normalize_failure_signature_folds_digits() {
        let signature = normalize_failure_signature("npm test", "Error 123 at line 456");
        assert_eq!(signature, "npm test|error ### at line ###");
    }

    #[test]
    fn computes_redirect_and_correction_metrics() {
        let session = SessionRecord {
            source: SessionSource::Claude,
            session_id: "s1".to_string(),
            cwd: "C:\\repo".to_string(),
            started_at: parse_timestamp("2026-04-10T00:00:00Z").unwrap(),
            user_prompts: vec![
                UserPrompt {
                    timestamp: parse_timestamp("2026-04-10T00:00:00Z").unwrap(),
                    text: "initial".to_string(),
                },
                UserPrompt {
                    timestamp: parse_timestamp("2026-04-10T00:00:03Z").unwrap(),
                    text: "redirect".to_string(),
                },
            ],
            shells: vec![
                ShellExecution {
                    timestamp: parse_timestamp("2026-04-10T00:00:02Z").unwrap(),
                    command: "git commit --ammend".to_string(),
                    output: "error: unexpected argument '--ammend'".to_string(),
                    outcome: CommandOutcome::Failure,
                },
                ShellExecution {
                    timestamp: parse_timestamp("2026-04-10T00:00:05Z").unwrap(),
                    command: "git commit --amend".to_string(),
                    output: "clean".to_string(),
                    outcome: CommandOutcome::Success,
                },
            ],
        };

        let report = build_report(
            &[session],
            &[],
            ScopeSummary {
                source: "all".to_string(),
                project_filter: None,
                since_days: None,
                before_today: false,
                before_today_cutoff_local_date: None,
                before_date: None,
            },
        );

        assert_eq!(report.totals.counters.sessions_scanned, 1);
        assert_eq!(report.totals.counters.user_redirects, 1);
        assert_eq!(report.totals.counters.redirects_with_resumed_shell, 1);
        assert_eq!(
            report.totals.counters.redirects_with_success_after_resume,
            1
        );
        assert_eq!(report.totals.counters.cli_correction_pairs, 1);
        assert_eq!(
            report
                .totals
                .avg_shell_commands_to_success_after_redirect
                .unwrap(),
            1.0
        );
        assert_eq!(
            report.totals.avg_seconds_to_success_after_redirect.unwrap(),
            2.0
        );
    }

    #[test]
    fn counts_duplicate_failures_by_signature() {
        let shells = vec![
            ShellExecution {
                timestamp: parse_timestamp("2026-04-10T00:00:01Z").unwrap(),
                command: "git status".to_string(),
                output: "fatal: not a git repository".to_string(),
                outcome: CommandOutcome::Failure,
            },
            ShellExecution {
                timestamp: parse_timestamp("2026-04-10T00:00:02Z").unwrap(),
                command: "git status".to_string(),
                output: "fatal: not a git repository".to_string(),
                outcome: CommandOutcome::Failure,
            },
        ];

        assert_eq!(count_duplicate_failures(&shells), 1);
    }

    #[test]
    fn dedupes_near_identical_prompt_duplicates() {
        let mut prompts = vec![
            UserPrompt {
                timestamp: parse_timestamp("2026-04-10T00:00:00Z").unwrap(),
                text: "Fix this now".to_string(),
            },
            UserPrompt {
                timestamp: parse_timestamp("2026-04-10T00:00:03Z").unwrap(),
                text: "Fix   this   now".to_string(),
            },
            UserPrompt {
                timestamp: parse_timestamp("2026-04-10T00:00:10Z").unwrap(),
                text: "Actually do something else".to_string(),
            },
        ];

        sort_and_dedup_prompts(&mut prompts);
        assert_eq!(prompts.len(), 2);
    }
}
