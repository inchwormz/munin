use crate::core::memory_os::{
    MemoryOsBriefReport, MemoryOsInspectionScope, MemoryOsOnboardingState,
};
use crate::core::tracking::Tracker;
use crate::session_brain::{SessionBrain, SessionBrainProvider, SessionBrainSignal};
use anyhow::{anyhow, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};

pub const RUNTIME_CONTEXT_PACKET_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum RuntimeContextSurfaceKind {
    Brain,
    Resume,
}

impl RuntimeContextSurfaceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Brain => "brain",
            Self::Resume => "resume",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum RuntimeContextSourceMode {
    Live,
    FallbackLatest,
    Stale,
    None,
    CompiledStartup,
}

impl RuntimeContextSourceMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Live => "live",
            Self::FallbackLatest => "fallback-latest",
            Self::Stale => "stale",
            Self::None => "none",
            Self::CompiledStartup => "compiled-startup",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum RuntimeContextProvider {
    Codex,
    Claude,
    Unknown,
}

impl RuntimeContextProvider {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeContextRenderMode {
    Text,
    Json,
    Prompt,
}

impl RuntimeContextRenderMode {
    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "text" => Ok(Self::Text),
            "json" => Ok(Self::Json),
            "prompt" => Ok(Self::Prompt),
            other => Err(anyhow!(
                "unsupported format '{}' (expected text, json, or prompt)",
                other
            )),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeContextMeta {
    pub version: u32,
    pub generated_at: String,
    pub surface_kind: RuntimeContextSurfaceKind,
    pub source_mode: RuntimeContextSourceMode,
    pub provider: RuntimeContextProvider,
    pub session_id: Option<String>,
    pub project_root: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transcript_source_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transcript_modified_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeContextRedirect {
    pub recommended_command: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeContextFinding {
    pub title: String,
    pub summary: String,
    pub evidence: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeContextSignal {
    pub summary: String,
    pub source: Option<String>,
    pub timestamp: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeContextMessage {
    pub role: String,
    pub text: String,
    pub timestamp: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeContextAgenda {
    pub current_goal: Option<String>,
    pub subgoals: Vec<String>,
    pub redirects: Vec<RuntimeContextSignal>,
    pub next_actions: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeContextState {
    pub decisions: Vec<RuntimeContextSignal>,
    pub findings: Vec<RuntimeContextSignal>,
    pub blockers: Vec<RuntimeContextSignal>,
    pub verified_facts: Vec<RuntimeContextSignal>,
    pub rejected_options: Vec<RuntimeContextSignal>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeContextProjectCapsule {
    pub summary: Vec<String>,
    pub key_files: Vec<String>,
    pub codebase_map: Option<String>,
    pub priority_notes: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeContextStrategyCapsule {
    pub summary: Vec<String>,
    pub source_paths: Vec<String>,
    pub planning_complete: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeContextUserOperatingModel {
    pub brief: Option<String>,
    pub overview: Option<String>,
    pub profile: Option<String>,
    pub friction: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeContextGuidance {
    pub retrieval_hints: Vec<String>,
    pub avoid: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeContextLiveState {
    pub agenda: RuntimeContextAgenda,
    pub state: RuntimeContextState,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub user_messages: Vec<RuntimeContextMessage>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub assistant_messages: Vec<RuntimeContextMessage>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeContextBootstrap {
    pub scope: String,
    pub sessions_processed: usize,
    pub shells_ingested: usize,
    pub onboarding_status: String,
    pub schema_version: String,
    pub what_i_know: Vec<String>,
    pub what_is_active: Vec<String>,
    pub watchouts: Vec<String>,
    pub startup_rules: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeContextBrief {
    pub what_i_know: Vec<RuntimeContextFinding>,
    pub how_you_work: Vec<RuntimeContextFinding>,
    pub what_is_active: Vec<RuntimeContextFinding>,
    pub next_steps: Vec<RuntimeContextFinding>,
    pub watchouts: Vec<RuntimeContextFinding>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeContextPacketV1 {
    pub meta: RuntimeContextMeta,
    pub redirect: Option<RuntimeContextRedirect>,
    pub brief: Option<RuntimeContextBrief>,
    pub bootstrap: Option<RuntimeContextBootstrap>,
    pub live: Option<RuntimeContextLiveState>,
    pub project: Option<RuntimeContextProjectCapsule>,
    pub strategy: Option<RuntimeContextStrategyCapsule>,
    pub user_operating_model: Option<RuntimeContextUserOperatingModel>,
    pub guidance: Option<RuntimeContextGuidance>,
}

pub fn packet_from_session_brain(brain: &SessionBrain) -> RuntimeContextPacketV1 {
    let source_mode = map_source_mode(&brain.meta.source_status);
    let meta = RuntimeContextMeta {
        version: RUNTIME_CONTEXT_PACKET_VERSION,
        generated_at: brain.meta.built_at.clone(),
        surface_kind: RuntimeContextSurfaceKind::Brain,
        source_mode,
        provider: map_provider(brain.meta.provider),
        session_id: brain.meta.session_id.clone(),
        project_root: Some(brain.meta.project_root.clone()),
        transcript_source_path: None,
        transcript_modified_at: None,
    };

    if source_mode != RuntimeContextSourceMode::Live {
        return RuntimeContextPacketV1 {
            meta,
            redirect: Some(RuntimeContextRedirect {
                recommended_command: "munin resume --format prompt".to_string(),
                reason: format!(
                    "Session Brain is not live ({}). Use resume for compiled project continuity.",
                    brain.meta.source_status
                ),
            }),
            brief: None,
            bootstrap: None,
            live: None,
            project: None,
            strategy: None,
            user_operating_model: None,
            guidance: None,
        };
    }

    RuntimeContextPacketV1 {
        meta,
        redirect: None,
        brief: None,
        bootstrap: None,
        live: Some(RuntimeContextLiveState {
            agenda: RuntimeContextAgenda {
                current_goal: brain.agenda.current_goal.clone(),
                subgoals: brain.agenda.subgoals.clone(),
                redirects: brain.agenda.redirects.iter().map(map_signal).collect(),
                next_actions: brain.agenda.next_actions.clone(),
            },
            state: RuntimeContextState {
                decisions: brain.state.decisions.iter().map(map_signal).collect(),
                findings: brain.state.findings.iter().map(map_signal).collect(),
                blockers: brain.state.blockers.iter().map(map_signal).collect(),
                verified_facts: brain.state.verified_facts.iter().map(map_signal).collect(),
                rejected_options: brain
                    .state
                    .rejected_options
                    .iter()
                    .map(map_signal)
                    .collect(),
            },
            user_messages: Vec::new(),
            assistant_messages: Vec::new(),
        }),
        project: Some(RuntimeContextProjectCapsule {
            summary: brain.project.summary.clone(),
            key_files: brain.project.key_files.clone(),
            codebase_map: (!brain.project.codebase_map.is_empty())
                .then_some(brain.project.codebase_map.clone()),
            priority_notes: (!brain.project.priority_notes.is_empty())
                .then_some(brain.project.priority_notes.clone()),
        }),
        strategy: Some(RuntimeContextStrategyCapsule {
            summary: brain.strategy.summary.clone(),
            source_paths: brain.strategy.source_paths.clone(),
            planning_complete: brain.strategy.planning_complete,
        }),
        user_operating_model: Some(RuntimeContextUserOperatingModel {
            brief: (!brain.user.brief.is_empty()).then_some(brain.user.brief.clone()),
            overview: (!brain.user.overview.is_empty()).then_some(brain.user.overview.clone()),
            profile: (!brain.user.profile.is_empty()).then_some(brain.user.profile.clone()),
            friction: (!brain.user.friction.is_empty()).then_some(brain.user.friction.clone()),
        }),
        guidance: Some(RuntimeContextGuidance {
            retrieval_hints: brain.guidance.retrieval_hints.clone(),
            avoid: brain.guidance.avoid.clone(),
        }),
    }
}

pub fn packet_from_memory_os_brief(report: &MemoryOsBriefReport) -> RuntimeContextPacketV1 {
    RuntimeContextPacketV1 {
        meta: RuntimeContextMeta {
            version: RUNTIME_CONTEXT_PACKET_VERSION,
            generated_at: report.generated_at.clone(),
            surface_kind: RuntimeContextSurfaceKind::Resume,
            source_mode: RuntimeContextSourceMode::CompiledStartup,
            provider: RuntimeContextProvider::Unknown,
            session_id: None,
            project_root: None,
            transcript_source_path: None,
            transcript_modified_at: None,
        },
        redirect: None,
        brief: Some(RuntimeContextBrief {
            what_i_know: report.what_i_know.iter().map(map_finding).collect(),
            how_you_work: report.how_you_work.iter().map(map_finding).collect(),
            what_is_active: report.what_is_active.iter().map(map_finding).collect(),
            next_steps: report.next_steps.iter().map(map_finding).collect(),
            watchouts: report.watchouts.iter().map(map_finding).collect(),
        }),
        bootstrap: None,
        live: None,
        project: None,
        strategy: None,
        user_operating_model: None,
        guidance: None,
    }
}

pub fn packet_from_startup_bootstrap(
    scope: MemoryOsInspectionScope,
    generated_at: String,
    onboarding: &MemoryOsOnboardingState,
) -> RuntimeContextPacketV1 {
    RuntimeContextPacketV1 {
        meta: RuntimeContextMeta {
            version: RUNTIME_CONTEXT_PACKET_VERSION,
            generated_at,
            surface_kind: RuntimeContextSurfaceKind::Resume,
            source_mode: RuntimeContextSourceMode::CompiledStartup,
            provider: RuntimeContextProvider::Unknown,
            session_id: None,
            project_root: None,
            transcript_source_path: None,
            transcript_modified_at: None,
        },
        redirect: None,
        brief: None,
        bootstrap: Some(RuntimeContextBootstrap {
            scope: scope.to_string(),
            sessions_processed: onboarding.sessions_processed,
            shells_ingested: onboarding.shells_ingested,
            onboarding_status: onboarding.status.clone(),
            schema_version: onboarding.schema_version.clone(),
            what_i_know: vec![format!(
                "Memory OS has indexed {} sessions and {} shell executions.",
                onboarding.sessions_processed, onboarding.shells_ingested
            )],
            what_is_active: vec![
                "Fast startup mode skips the heavyweight full brief; use `munin memory-os brief --format prompt` when the complete Memory OS brief is needed.".to_string(),
            ],
            watchouts: vec![
                "Do not open raw transcripts unless this startup brief and Session Brain are insufficient.".to_string(),
            ],
            startup_rules: vec![
                "Use `munin brain --format prompt` for live-session continuity.".to_string(),
                "Use dedicated Memory OS commands for deeper user profile, friction, or historical recall.".to_string(),
            ],
        }),
        live: None,
        project: None,
        strategy: None,
        user_operating_model: None,
        guidance: None,
    }
}

pub fn render_packet(
    packet: &RuntimeContextPacketV1,
    mode: RuntimeContextRenderMode,
) -> Result<String> {
    Ok(match mode {
        RuntimeContextRenderMode::Text => render_text(packet),
        RuntimeContextRenderMode::Json => serde_json::to_string_pretty(packet)?,
        RuntimeContextRenderMode::Prompt => render_prompt(packet),
    })
}

pub fn build_current_brain_packet() -> Result<RuntimeContextPacketV1> {
    let brain = crate::session_brain::build_current_session_brain()?;
    Ok(packet_from_session_brain(&brain))
}

pub fn build_current_resume_packet(scope: &str) -> Result<RuntimeContextPacketV1> {
    let tracker = Tracker::new()?;
    let scope = parse_scope(scope)?;
    let onboarding = tracker.get_memory_os_onboarding_state_fast()?;
    Ok(packet_from_startup_bootstrap(
        scope,
        Utc::now().to_rfc3339(),
        &onboarding,
    ))
}

fn render_text(packet: &RuntimeContextPacketV1) -> String {
    let mut lines = vec![
        format!(
            "Runtime Context [{}:{}]",
            packet.meta.surface_kind.as_str(),
            packet.meta.source_mode.as_str()
        ),
        format!("generated_at: {}", packet.meta.generated_at),
    ];
    if let Some(session_id) = packet.meta.session_id.as_deref() {
        lines.push(format!("session: {}", session_id));
    }
    if let Some(project_root) = packet.meta.project_root.as_deref() {
        lines.push(format!("project: {}", project_root));
    }
    if let Some(redirect) = &packet.redirect {
        lines.push("redirect:".to_string());
        lines.push(format!("- {}", redirect.reason));
        lines.push(format!(
            "- recommended command: {}",
            redirect.recommended_command
        ));
        return lines.join("\n");
    }
    if let Some(live) = &packet.live {
        lines.push("agenda:".to_string());
        if let Some(goal) = live.agenda.current_goal.as_deref() {
            lines.push(format!("- current ask: {}", goal));
        }
        for subgoal in live.agenda.subgoals.iter().take(4) {
            lines.push(format!("- subgoal: {}", subgoal));
        }
    }
    if let Some(bootstrap) = &packet.bootstrap {
        lines.push("startup bootstrap:".to_string());
        lines.extend(
            bootstrap
                .what_i_know
                .iter()
                .map(|item| format!("- {}", item)),
        );
        lines.extend(
            bootstrap
                .what_is_active
                .iter()
                .map(|item| format!("- {}", item)),
        );
    }
    if let Some(project) = &packet.project {
        lines.push("project:".to_string());
        lines.extend(
            project
                .summary
                .iter()
                .take(5)
                .map(|item| format!("- {}", item)),
        );
    }
    lines.join("\n")
}

fn render_prompt(packet: &RuntimeContextPacketV1) -> String {
    let mut lines = vec![format!(
        "<runtime_context_v1 surface=\"{}\" source_mode=\"{}\" generated_at=\"{}\" provider=\"{}\" session_id=\"{}\">",
        packet.meta.surface_kind.as_str(),
        packet.meta.source_mode.as_str(),
        packet.meta.generated_at,
        packet.meta.provider.as_str(),
        packet.meta.session_id.as_deref().unwrap_or("none")
    )];
    if let Some(redirect) = &packet.redirect {
        lines.push("<redirect>".to_string());
        lines.push(format!("- reason: {}", redirect.reason));
        lines.push(format!(
            "- recommended command: {}",
            redirect.recommended_command
        ));
        lines.push("</redirect>".to_string());
        lines.push("</runtime_context_v1>".to_string());
        return lines.join("\n");
    }
    if let Some(live) = &packet.live {
        lines.push("<agenda>".to_string());
        if let Some(goal) = live.agenda.current_goal.as_deref() {
            lines.push(format!("- current ask: {}", goal));
        }
        for subgoal in live.agenda.subgoals.iter().take(4) {
            lines.push(format!("- subgoal: {}", subgoal));
        }
        for redirect in live.agenda.redirects.iter().take(3) {
            lines.push(format!("- redirect: {}", redirect.summary));
        }
        for action in live.agenda.next_actions.iter().take(4) {
            lines.push(format!("- next: {}", action));
        }
        lines.push("</agenda>".to_string());

        lines.push("<state>".to_string());
        for signal in live.state.decisions.iter().take(3) {
            lines.push(format!("- decision: {}", signal.summary));
        }
        for signal in live.state.findings.iter().take(3) {
            lines.push(format!("- finding: {}", signal.summary));
        }
        for signal in live.state.blockers.iter().take(3) {
            lines.push(format!("- blocker: {}", signal.summary));
        }
        for signal in live.state.verified_facts.iter().take(2) {
            lines.push(format!("- verified: {}", signal.summary));
        }
        for signal in live.state.rejected_options.iter().take(2) {
            lines.push(format!("- rejected: {}", signal.summary));
        }
        lines.push("</state>".to_string());
    }
    if let Some(bootstrap) = &packet.bootstrap {
        lines.push("<startup_bootstrap>".to_string());
        lines.push(format!(
            "- onboarding: {} / {} sessions / {} shells",
            bootstrap.onboarding_status, bootstrap.sessions_processed, bootstrap.shells_ingested
        ));
        for item in bootstrap.what_i_know.iter() {
            lines.push(format!("- what_i_know: {}", item));
        }
        for item in bootstrap.what_is_active.iter() {
            lines.push(format!("- what_is_active: {}", item));
        }
        for item in bootstrap.watchouts.iter() {
            lines.push(format!("- watchout: {}", item));
        }
        for item in bootstrap.startup_rules.iter() {
            lines.push(format!("- startup_rule: {}", item));
        }
        lines.push("</startup_bootstrap>".to_string());
    }
    if let Some(brief) = &packet.brief {
        lines.push("<brief>".to_string());
        for finding in brief.what_i_know.iter().take(3) {
            lines.push(format!(
                "- what_i_know: {}: {}",
                finding.title, finding.summary
            ));
        }
        for finding in brief.what_is_active.iter().take(3) {
            lines.push(format!(
                "- what_is_active: {}: {}",
                finding.title, finding.summary
            ));
        }
        for finding in brief.next_steps.iter().take(3) {
            lines.push(format!(
                "- next_step: {}: {}",
                finding.title, finding.summary
            ));
        }
        for finding in brief.watchouts.iter().take(3) {
            lines.push(format!(
                "- watchout: {}: {}",
                finding.title, finding.summary
            ));
        }
        lines.push("</brief>".to_string());
    }
    if let Some(project) = &packet.project {
        lines.push("<project>".to_string());
        for line in project.summary.iter().take(5) {
            lines.push(format!("- {}", line));
        }
        for key_file in project.key_files.iter().take(6) {
            lines.push(format!("- key file: {}", key_file));
        }
        if let Some(codebase_map) = &project.codebase_map {
            lines.push(format!("- codebase map: {}", codebase_map));
        }
        lines.push("</project>".to_string());
    }
    if let Some(strategy) = &packet.strategy {
        lines.push("<strategy>".to_string());
        if strategy.summary.is_empty() {
            lines.push("- no active strategy artifact matched this project".to_string());
        } else {
            for line in strategy.summary.iter().take(5) {
                lines.push(format!("- {}", line));
            }
        }
        lines.push("</strategy>".to_string());
    }
    if let Some(user) = &packet.user_operating_model {
        lines.push("<user_operating_model>".to_string());
        if let Some(brief) = user.brief.as_deref() {
            lines.push(format!("- brief: {}", brief));
        }
        if let Some(overview) = user.overview.as_deref() {
            lines.push(format!("- overview: {}", overview));
        }
        if let Some(profile) = user.profile.as_deref() {
            lines.push(format!("- profile: {}", profile));
        }
        if let Some(friction) = user.friction.as_deref() {
            lines.push(format!("- friction: {}", friction));
        }
        lines.push("</user_operating_model>".to_string());
    }
    if let Some(guidance) = &packet.guidance {
        lines.push("<guidance>".to_string());
        for hint in guidance.retrieval_hints.iter().take(4) {
            lines.push(format!("- retrieval hint: {}", hint));
        }
        for avoid in guidance.avoid.iter().take(4) {
            lines.push(format!("- avoid: {}", avoid));
        }
        lines.push("</guidance>".to_string());
    }
    lines.push("</runtime_context_v1>".to_string());
    lines.join("\n")
}

fn map_source_mode(source_status: &str) -> RuntimeContextSourceMode {
    match source_status {
        "live" => RuntimeContextSourceMode::Live,
        "fallback-latest" => RuntimeContextSourceMode::FallbackLatest,
        "stale" => RuntimeContextSourceMode::Stale,
        _ => RuntimeContextSourceMode::None,
    }
}

fn parse_scope(scope: &str) -> Result<MemoryOsInspectionScope> {
    match scope {
        "user" => Ok(MemoryOsInspectionScope::User),
        "project" => Ok(MemoryOsInspectionScope::Project),
        other => Err(anyhow!(
            "unsupported scope '{}' (expected user or project)",
            other
        )),
    }
}

fn map_provider(provider: SessionBrainProvider) -> RuntimeContextProvider {
    match provider {
        SessionBrainProvider::Codex => RuntimeContextProvider::Codex,
        SessionBrainProvider::Claude => RuntimeContextProvider::Claude,
        SessionBrainProvider::Unknown => RuntimeContextProvider::Unknown,
    }
}

fn map_signal(signal: &SessionBrainSignal) -> RuntimeContextSignal {
    RuntimeContextSignal {
        summary: signal.summary.clone(),
        source: Some(signal.source.clone()),
        timestamp: signal.timestamp.clone(),
        evidence: signal.evidence.clone(),
    }
}

fn map_finding(
    finding: &crate::core::memory_os::MemoryOsNarrativeFinding,
) -> RuntimeContextFinding {
    RuntimeContextFinding {
        title: finding.title.clone(),
        summary: finding.summary.clone(),
        evidence: finding.evidence.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_brain::{
        SessionBrainAgenda, SessionBrainGuidance, SessionBrainMessage, SessionBrainState,
        SessionBrainStrategyContext, SessionBrainUserContext,
    };

    fn live_brain() -> SessionBrain {
        SessionBrain {
            meta: crate::session_brain::SessionBrainMeta {
                session_id: Some("sess-1".to_string()),
                cwd: "C:/repo".to_string(),
                project_root: "C:/repo".to_string(),
                built_at: "2026-04-21T00:00:00Z".to_string(),
                version: 1,
                provider: SessionBrainProvider::Codex,
                transcript_source_path: Some("C:/repo/session.jsonl".to_string()),
                transcript_modified_at: Some("2026-04-21T00:00:00Z".to_string()),
                source_status: "live".to_string(),
            },
            messages: crate::session_brain::SessionBrainMessageGroup {
                user: vec![SessionBrainMessage {
                    role: "user".to_string(),
                    provider: SessionBrainProvider::Codex,
                    session_id: Some("sess-1".to_string()),
                    timestamp: Some("2026-04-21T00:00:00Z".to_string()),
                    cwd: Some("C:/repo".to_string()),
                    transcript_path: "C:/repo/session.jsonl".to_string(),
                    record_type: "fixture".to_string(),
                    line_number: 1,
                    text: "Fix runtime context".to_string(),
                    source_kind: "root".to_string(),
                }],
                assistant: Vec::new(),
            },
            agenda: SessionBrainAgenda {
                current_goal: Some("Fix runtime context".to_string()),
                subgoals: vec!["Add packet contract".to_string()],
                redirects: Vec::new(),
                next_actions: vec!["Implement RuntimeContextPacketV1".to_string()],
            },
            state: SessionBrainState {
                decisions: vec![SessionBrainSignal {
                    summary: "Keep Munin as the runtime context owner".to_string(),
                    source: "decision".to_string(),
                    timestamp: Some("2026-04-21T00:00:00Z".to_string()),
                    evidence: Vec::new(),
                }],
                findings: Vec::new(),
                blockers: Vec::new(),
                verified_facts: Vec::new(),
                rejected_options: Vec::new(),
            },
            project: crate::session_brain::SessionBrainProjectContext {
                summary: vec!["munin-memory: Local memory system".to_string()],
                key_files: vec!["src/bin/munin.rs".to_string()],
                codebase_map: "src: session_brain, analytics".to_string(),
                project_memory: None,
                priority_notes: String::new(),
            },
            strategy: SessionBrainStrategyContext {
                summary: vec!["Replace Context in the runtime path".to_string()],
                source_paths: Vec::new(),
                planning_complete: false,
            },
            user: SessionBrainUserContext {
                brief: "User wants direct fixes".to_string(),
                overview: String::new(),
                profile: String::new(),
                friction: String::new(),
            },
            guidance: SessionBrainGuidance {
                retrieval_hints: vec!["Prefer the runtime packet".to_string()],
                avoid: vec!["Raw transcript dumps".to_string()],
            },
        }
    }

    #[test]
    fn non_live_brain_packet_redirects_to_resume() {
        let mut brain = live_brain();
        brain.meta.source_status = "fallback-latest".to_string();

        let packet = packet_from_session_brain(&brain);

        assert_eq!(
            packet.meta.source_mode,
            RuntimeContextSourceMode::FallbackLatest
        );
        assert!(packet.live.is_none());
        assert!(packet.redirect.is_some());
        assert!(render_packet(&packet, RuntimeContextRenderMode::Prompt)
            .expect("render")
            .contains("munin resume --format prompt"));
    }

    #[test]
    fn live_brain_packet_keeps_live_sections() {
        let packet = packet_from_session_brain(&live_brain());

        assert_eq!(packet.meta.source_mode, RuntimeContextSourceMode::Live);
        assert!(packet.live.is_some());
        assert!(packet.redirect.is_none());
        assert!(packet.meta.transcript_source_path.is_none());
        assert!(packet
            .live
            .as_ref()
            .expect("live state")
            .user_messages
            .is_empty());
        let rendered = render_packet(&packet, RuntimeContextRenderMode::Prompt).expect("render");
        assert!(!rendered.contains("C:/repo/session.jsonl"));
        assert!(!rendered.contains("<messages>"));
    }

    #[test]
    fn startup_bootstrap_packet_renders_prompt() {
        let onboarding = MemoryOsOnboardingState {
            schema_version: "memory-os-v1".to_string(),
            status: "completed".to_string(),
            started_at: None,
            completed_at: None,
            sessions_processed: 100,
            shells_ingested: 200,
            corrections_ingested: 3,
            imported_sources: Vec::new(),
            checkpoint_count: 10,
            journal_event_count: 20,
        };

        let packet = packet_from_startup_bootstrap(
            MemoryOsInspectionScope::User,
            "2026-04-21T00:00:00Z".to_string(),
            &onboarding,
        );
        let rendered = render_packet(&packet, RuntimeContextRenderMode::Prompt).expect("render");

        assert_eq!(packet.meta.surface_kind, RuntimeContextSurfaceKind::Resume);
        assert!(rendered.contains("<startup_bootstrap>"));
        assert!(rendered.contains("100 sessions"));
    }
}
