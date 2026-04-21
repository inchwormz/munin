use super::types::{SessionBrainMessage, SessionBrainProvider};
use crate::core::utils::{detect_project_root, normalize_windows_path_string};
use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Utc};
use serde_json::Value;
use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use walkdir::WalkDir;

#[derive(Debug, Clone)]
pub struct SessionMessages {
    pub session_id: Option<String>,
    pub provider: SessionBrainProvider,
    pub transcript_path: Option<String>,
    pub transcript_modified_at: Option<String>,
    pub source_status: String,
    pub user: Vec<SessionBrainMessage>,
    pub assistant: Vec<SessionBrainMessage>,
}

#[derive(Debug, Clone)]
struct TranscriptTarget {
    provider: SessionBrainProvider,
    session_id: String,
    path: PathBuf,
    source_status: String,
}

#[derive(Debug, Clone)]
struct TranscriptCandidate {
    session_id: String,
    path: PathBuf,
    modified_at: SystemTime,
}

#[derive(Debug, Clone)]
struct TranscriptFile {
    path: PathBuf,
    modified_at: SystemTime,
}

#[derive(Debug, Clone)]
struct CodexMeta {
    session_id: String,
    cwd: Option<String>,
    is_subagent: bool,
}

#[derive(Debug, Clone)]
struct ClaudeMeta {
    session_id: String,
    cwd: Option<String>,
    is_sidechain: bool,
}

pub fn read_current_session_messages(
    project_root: &Path,
    allow_session_fallback: bool,
) -> Result<SessionMessages> {
    let target = resolve_transcript_target(project_root, allow_session_fallback)?;
    let Some(target) = target else {
        return Ok(SessionMessages {
            session_id: None,
            provider: SessionBrainProvider::Unknown,
            transcript_path: None,
            transcript_modified_at: None,
            source_status: "none".to_string(),
            user: Vec::new(),
            assistant: Vec::new(),
        });
    };

    let mut messages = match target.provider {
        SessionBrainProvider::Codex => parse_codex_messages(&target.path, &target.session_id),
        SessionBrainProvider::Claude => parse_claude_messages(&target.path, &target.session_id),
        SessionBrainProvider::Unknown => Ok(SessionMessages {
            session_id: Some(target.session_id),
            provider: SessionBrainProvider::Unknown,
            transcript_path: None,
            transcript_modified_at: None,
            source_status: "unknown".to_string(),
            user: Vec::new(),
            assistant: Vec::new(),
        }),
    }?;
    messages.transcript_path = Some(normalize_windows_path_string(
        target.path.to_string_lossy().as_ref(),
    ));
    messages.transcript_modified_at = transcript_modified_at(&target.path);
    messages.source_status = target.source_status;
    Ok(messages)
}

pub fn load_context_snapshot_messages(
    project_root: &Path,
    user_messages: &[SessionBrainMessage],
) -> Result<Vec<SessionBrainMessage>> {
    let mut snapshots = Vec::new();
    let mut seen = HashSet::new();

    for message in user_messages {
        for candidate in extract_context_snapshot_paths(project_root, &message.text) {
            let normalized_path =
                normalize_windows_path_string(candidate.to_string_lossy().as_ref());
            if !seen.insert(normalized_path.clone()) || !candidate.exists() {
                continue;
            }
            let content = fs::read_to_string(&candidate)
                .with_context(|| format!("failed to read {}", candidate.display()))?;
            let normalized = normalize_message_text(&content);
            if normalized.is_empty() {
                continue;
            }
            snapshots.push(SessionBrainMessage {
                role: "user".to_string(),
                provider: message.provider,
                session_id: message.session_id.clone(),
                timestamp: message.timestamp.clone(),
                cwd: message.cwd.clone(),
                transcript_path: normalized_path,
                record_type: "context-snapshot".to_string(),
                line_number: 1,
                text: normalized,
                source_kind: "snapshot".to_string(),
            });
        }
    }

    Ok(snapshots)
}

fn resolve_transcript_target(
    project_root: &Path,
    allow_session_fallback: bool,
) -> Result<Option<TranscriptTarget>> {
    if let Ok(session_id) = std::env::var("CODEX_THREAD_ID") {
        if let Some(path) = find_codex_session_path(&session_id)? {
            return Ok(Some(TranscriptTarget {
                provider: SessionBrainProvider::Codex,
                session_id,
                path,
                source_status: "live".to_string(),
            }));
        }
    }

    if let Ok(session_id) = std::env::var("CLAUDE_SESSION_ID") {
        if let Some(path) = find_claude_session_path(&session_id)? {
            return Ok(Some(TranscriptTarget {
                provider: SessionBrainProvider::Claude,
                session_id,
                path,
                source_status: "live".to_string(),
            }));
        }
    }

    if !allow_session_fallback {
        return Ok(None);
    }

    let codex = find_latest_codex_session(project_root)?;
    let claude = find_latest_claude_session(project_root)?;
    let best = match (codex, claude) {
        (Some(left), Some(right)) => {
            if left.modified_at >= right.modified_at {
                Some(TranscriptTarget {
                    provider: SessionBrainProvider::Codex,
                    session_id: left.session_id,
                    path: left.path,
                    source_status: fallback_source_status(
                        SessionBrainProvider::Codex,
                        left.modified_at,
                    ),
                })
            } else {
                Some(TranscriptTarget {
                    provider: SessionBrainProvider::Claude,
                    session_id: right.session_id,
                    path: right.path,
                    source_status: fallback_source_status(
                        SessionBrainProvider::Claude,
                        right.modified_at,
                    ),
                })
            }
        }
        (Some(candidate), None) => Some(TranscriptTarget {
            provider: SessionBrainProvider::Codex,
            session_id: candidate.session_id,
            path: candidate.path,
            source_status: fallback_source_status(
                SessionBrainProvider::Codex,
                candidate.modified_at,
            ),
        }),
        (None, Some(candidate)) => Some(TranscriptTarget {
            provider: SessionBrainProvider::Claude,
            session_id: candidate.session_id,
            path: candidate.path,
            source_status: fallback_source_status(
                SessionBrainProvider::Claude,
                candidate.modified_at,
            ),
        }),
        (None, None) => None,
    };

    Ok(best)
}

fn transcript_modified_at(path: &Path) -> Option<String> {
    fs::metadata(path)
        .ok()
        .and_then(|metadata| metadata.modified().ok())
        .map(|modified| DateTime::<Utc>::from(modified).to_rfc3339())
}

fn fallback_source_status(provider: SessionBrainProvider, modified_at: SystemTime) -> String {
    decide_source_status(modified_at, in_active_orchestrator(provider), Utc::now())
}

// Claude Code never exports CLAUDE_SESSION_ID to subshells, so the env-var path
// in resolve_transcript_target is unreachable on the native shell. When we can
// confirm we're inside the matching orchestrator AND the transcript is being
// actively written to, treat it as live. The 120s window covers idle time
// during long-running tool calls.
fn decide_source_status(
    modified_at: SystemTime,
    in_orchestrator: bool,
    now: DateTime<Utc>,
) -> String {
    let modified = DateTime::<Utc>::from(modified_at);
    let age = now.signed_duration_since(modified);
    if age > Duration::hours(24) {
        return "stale".to_string();
    }
    if age <= Duration::seconds(120) && in_orchestrator {
        return "live".to_string();
    }
    "fallback-latest".to_string()
}

fn in_active_orchestrator(provider: SessionBrainProvider) -> bool {
    match provider {
        SessionBrainProvider::Claude => std::env::var("CLAUDECODE").is_ok(),
        SessionBrainProvider::Codex => {
            std::env::var("CODEX_HOME").is_ok() || std::env::var("CODEX_THREAD_ID").is_ok()
        }
        SessionBrainProvider::Unknown => false,
    }
}

fn find_codex_session_path(session_id: &str) -> Result<Option<PathBuf>> {
    let Some(home) = dirs::home_dir() else {
        return Ok(None);
    };
    let root = home.join(".codex").join("sessions");
    if !root.exists() {
        return Ok(None);
    }

    for entry in WalkDir::new(&root)
        .follow_links(false)
        .into_iter()
        .flatten()
    {
        let path = entry.path();
        if !path.is_file() || path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
            continue;
        }
        if path
            .file_name()
            .map(|name| name.to_string_lossy().contains(session_id))
            .unwrap_or(false)
        {
            return Ok(Some(path.to_path_buf()));
        }
    }

    Ok(None)
}

fn find_claude_session_path(session_id: &str) -> Result<Option<PathBuf>> {
    let Some(home) = dirs::home_dir() else {
        return Ok(None);
    };
    let root = home.join(".claude").join("projects");
    if !root.exists() {
        return Ok(None);
    }

    for entry in WalkDir::new(&root)
        .follow_links(false)
        .into_iter()
        .flatten()
    {
        let path = entry.path();
        if !path.is_file() || path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
            continue;
        }
        if path
            .file_stem()
            .map(|stem| stem.to_string_lossy() == session_id)
            .unwrap_or(false)
        {
            return Ok(Some(path.to_path_buf()));
        }
    }

    Ok(None)
}

fn find_latest_codex_session(project_root: &Path) -> Result<Option<TranscriptCandidate>> {
    let Some(home) = dirs::home_dir() else {
        return Ok(None);
    };
    let root = home.join(".codex").join("sessions");
    if !root.exists() {
        return Ok(None);
    }

    find_latest_codex_session_in_root(project_root, &root)
}

fn find_latest_codex_session_in_root(
    project_root: &Path,
    root: &Path,
) -> Result<Option<TranscriptCandidate>> {
    let project_root = normalized_project_root(project_root);
    let files = newest_jsonl_files(root)?;

    for file in files {
        let Some(meta) = read_codex_meta(&file.path)? else {
            continue;
        };
        if meta.is_subagent {
            continue;
        }
        let Some(cwd) = meta.cwd.as_deref() else {
            continue;
        };
        if !project_matches(cwd, &project_root) {
            continue;
        }
        return Ok(Some(TranscriptCandidate {
            session_id: meta.session_id,
            path: file.path,
            modified_at: file.modified_at,
        }));
    }

    Ok(None)
}

fn find_latest_claude_session(project_root: &Path) -> Result<Option<TranscriptCandidate>> {
    let Some(home) = dirs::home_dir() else {
        return Ok(None);
    };
    let root = home.join(".claude").join("projects");
    if !root.exists() {
        return Ok(None);
    }

    let project_sessions_root = root.join(claude_project_dir_name(project_root));
    if project_sessions_root.exists() {
        if let Some(candidate) =
            find_latest_claude_session_in_root(project_root, &project_sessions_root)?
        {
            return Ok(Some(candidate));
        }
    }

    find_latest_claude_session_in_root(project_root, &root)
}

fn find_latest_claude_session_in_root(
    project_root: &Path,
    root: &Path,
) -> Result<Option<TranscriptCandidate>> {
    let project_root = normalized_project_root(project_root);
    let files = newest_jsonl_files(root)?;

    for file in files {
        let Some(meta) = read_claude_meta(&file.path)? else {
            continue;
        };
        if meta.is_sidechain {
            continue;
        }
        let Some(cwd) = meta.cwd.as_deref() else {
            continue;
        };
        if !project_matches(cwd, &project_root) {
            continue;
        }
        return Ok(Some(TranscriptCandidate {
            session_id: meta.session_id,
            path: file.path,
            modified_at: file.modified_at,
        }));
    }

    Ok(None)
}

fn newest_jsonl_files(root: &Path) -> Result<Vec<TranscriptFile>> {
    let mut files = Vec::new();
    for entry in WalkDir::new(root).follow_links(false).into_iter().flatten() {
        if !entry.file_type().is_file()
            || entry.path().extension().and_then(|ext| ext.to_str()) != Some("jsonl")
        {
            continue;
        }
        let modified_at = entry
            .metadata()
            .ok()
            .and_then(|meta| meta.modified().ok())
            .unwrap_or(SystemTime::UNIX_EPOCH);
        files.push(TranscriptFile {
            path: entry.path().to_path_buf(),
            modified_at,
        });
    }
    files.sort_by(|left, right| {
        right
            .modified_at
            .cmp(&left.modified_at)
            .then_with(|| left.path.cmp(&right.path))
    });
    Ok(files)
}

fn claude_project_dir_name(project_root: &Path) -> String {
    normalized_project_root(project_root)
        .replace(':', "-")
        .replace(['/', '\\'], "-")
}

fn extract_context_snapshot_paths(project_root: &Path, text: &str) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let mut seen = HashSet::new();
    for token in text.split_whitespace() {
        let cleaned = token.trim_matches(|ch: char| {
            matches!(
                ch,
                '`' | '"' | '\'' | '(' | ')' | '[' | ']' | '{' | '}' | ',' | ';'
            )
        });
        if !cleaned.to_ascii_lowercase().ends_with(".md") {
            continue;
        }
        if !(cleaned.contains(".codex/context/") || cleaned.contains(".codex\\context\\")) {
            continue;
        }
        let path = PathBuf::from(cleaned);
        let resolved = if path.is_absolute() {
            path
        } else {
            project_root.join(path)
        };
        let normalized = normalize_windows_path_string(resolved.to_string_lossy().as_ref());
        if seen.insert(normalized) {
            paths.push(resolved);
        }
    }
    paths
}

fn read_codex_meta(path: &Path) -> Result<Option<CodexMeta>> {
    let file = File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let reader = BufReader::new(file);
    let fallback_id = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("codex-session")
        .to_string();
    let mut cwd = None;
    let mut session_id = fallback_id;
    let mut is_subagent = false;

    for line in reader.lines().take(64) {
        let line = line?;
        let value: Value = match serde_json::from_str(&line) {
            Ok(value) => value,
            Err(_) => continue,
        };
        match value.get("type").and_then(Value::as_str) {
            Some("session_meta") => {
                if let Some(payload) = value.get("payload") {
                    if payload.pointer("/source/subagent/thread_spawn").is_some() {
                        is_subagent = true;
                    }
                    if let Some(id) = payload.get("id").and_then(Value::as_str) {
                        session_id = id.to_string();
                    }
                    if let Some(meta_cwd) = payload.get("cwd").and_then(Value::as_str) {
                        cwd = Some(meta_cwd.to_string());
                        break;
                    }
                }
            }
            Some("turn_context") => {
                if let Some(payload) = value.get("payload") {
                    if let Some(meta_cwd) = payload.get("cwd").and_then(Value::as_str) {
                        cwd = Some(meta_cwd.to_string());
                        break;
                    }
                }
            }
            _ => {}
        }
    }

    Ok(Some(CodexMeta {
        session_id,
        cwd,
        is_subagent,
    }))
}

fn read_claude_meta(path: &Path) -> Result<Option<ClaudeMeta>> {
    let file = File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let reader = BufReader::new(file);
    let session_id = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("claude-session")
        .to_string();
    let mut cwd = None;
    let mut is_sidechain = false;

    for line in reader.lines().take(64) {
        let line = line?;
        let value: Value = match serde_json::from_str(&line) {
            Ok(value) => value,
            Err(_) => continue,
        };
        if value
            .get("isSidechain")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            is_sidechain = true;
        }
        if let Some(entry_cwd) = value.get("cwd").and_then(Value::as_str) {
            cwd = Some(entry_cwd.to_string());
            break;
        }
    }

    Ok(Some(ClaudeMeta {
        session_id,
        cwd,
        is_sidechain,
    }))
}

fn parse_codex_messages(path: &Path, session_id: &str) -> Result<SessionMessages> {
    let file = File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let reader = BufReader::new(file);
    let mut cwd = None::<String>;
    let mut user = Vec::new();
    let mut assistant = Vec::new();
    let mut seen = HashSet::new();

    for (index, line) in reader.lines().enumerate() {
        let line = line?;
        let value: Value = match serde_json::from_str(&line) {
            Ok(value) => value,
            Err(_) => continue,
        };
        let line_number = index + 1;
        let timestamp = value
            .get("timestamp")
            .and_then(Value::as_str)
            .map(str::to_string);

        match value.get("type").and_then(Value::as_str) {
            Some("session_meta") => {
                if let Some(payload) = value.get("payload") {
                    if let Some(meta_cwd) = payload.get("cwd").and_then(Value::as_str) {
                        cwd = Some(meta_cwd.to_string());
                    }
                }
            }
            Some("turn_context") => {
                if let Some(payload) = value.get("payload") {
                    if let Some(meta_cwd) = payload.get("cwd").and_then(Value::as_str) {
                        cwd = Some(meta_cwd.to_string());
                    }
                }
            }
            Some("event_msg") => {
                let Some(payload) = value.get("payload") else {
                    continue;
                };
                if payload.get("type").and_then(Value::as_str) == Some("user_message") {
                    if let Some(text) = payload.get("message").and_then(Value::as_str) {
                        push_message(
                            &mut user,
                            &mut seen,
                            SessionBrainProvider::Codex,
                            session_id,
                            cwd.as_deref(),
                            path,
                            "event_msg:user_message",
                            line_number,
                            timestamp.clone(),
                            "user",
                            text,
                        );
                    }
                }
            }
            Some("response_item") => {
                let Some(payload) = value.get("payload") else {
                    continue;
                };
                if payload.get("type").and_then(Value::as_str) == Some("function_call") {
                    if payload.get("name").and_then(Value::as_str) == Some("shell_command") {
                        if let Some(arguments) = payload.get("arguments").and_then(Value::as_str) {
                            if let Ok(arguments) = serde_json::from_str::<Value>(arguments) {
                                if let Some(workdir) =
                                    arguments.get("workdir").and_then(Value::as_str)
                                {
                                    cwd = Some(workdir.to_string());
                                }
                            }
                        }
                    }
                    continue;
                }
                if payload.get("type").and_then(Value::as_str) != Some("message") {
                    continue;
                }
                let Some(role) = payload.get("role").and_then(Value::as_str) else {
                    continue;
                };
                let Some(text) = extract_codex_content_text(payload.get("content")) else {
                    continue;
                };
                if role == "user" {
                    push_message(
                        &mut user,
                        &mut seen,
                        SessionBrainProvider::Codex,
                        session_id,
                        cwd.as_deref(),
                        path,
                        "response_item:message:user",
                        line_number,
                        timestamp.clone(),
                        "user",
                        &text,
                    );
                } else if role == "assistant" && assistant_text_is_material(&text) {
                    push_message(
                        &mut assistant,
                        &mut seen,
                        SessionBrainProvider::Codex,
                        session_id,
                        cwd.as_deref(),
                        path,
                        "response_item:message:assistant",
                        line_number,
                        timestamp.clone(),
                        "assistant",
                        &text,
                    );
                }
            }
            _ => {}
        }
    }

    Ok(SessionMessages {
        session_id: Some(session_id.to_string()),
        provider: SessionBrainProvider::Codex,
        transcript_path: None,
        transcript_modified_at: None,
        source_status: "unknown".to_string(),
        user,
        assistant,
    })
}

fn parse_claude_messages(path: &Path, session_id: &str) -> Result<SessionMessages> {
    let file = File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let reader = BufReader::new(file);
    let mut cwd = None::<String>;
    let mut user = Vec::new();
    let mut assistant = Vec::new();
    let mut seen = HashSet::new();

    for (index, line) in reader.lines().enumerate() {
        let line = line?;
        let value: Value = match serde_json::from_str(&line) {
            Ok(value) => value,
            Err(_) => continue,
        };
        if value
            .get("isSidechain")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            continue;
        }

        let line_number = index + 1;
        let timestamp = value
            .get("timestamp")
            .and_then(Value::as_str)
            .map(str::to_string);
        if cwd.is_none() {
            cwd = value.get("cwd").and_then(Value::as_str).map(str::to_string);
        }

        match value.get("type").and_then(Value::as_str) {
            Some("user") => {
                if let Some(text) = extract_claude_user_text(&value) {
                    push_message(
                        &mut user,
                        &mut seen,
                        SessionBrainProvider::Claude,
                        session_id,
                        cwd.as_deref(),
                        path,
                        "user",
                        line_number,
                        timestamp.clone(),
                        "user",
                        &text,
                    );
                }
            }
            Some("assistant") => {
                if let Some(text) = extract_claude_assistant_text(&value) {
                    if assistant_text_is_material(&text) {
                        push_message(
                            &mut assistant,
                            &mut seen,
                            SessionBrainProvider::Claude,
                            session_id,
                            cwd.as_deref(),
                            path,
                            "assistant",
                            line_number,
                            timestamp.clone(),
                            "assistant",
                            &text,
                        );
                    }
                }
            }
            _ => {}
        }
    }

    Ok(SessionMessages {
        session_id: Some(session_id.to_string()),
        provider: SessionBrainProvider::Claude,
        transcript_path: None,
        transcript_modified_at: None,
        source_status: "unknown".to_string(),
        user,
        assistant,
    })
}

fn push_message(
    target: &mut Vec<SessionBrainMessage>,
    seen: &mut HashSet<String>,
    provider: SessionBrainProvider,
    session_id: &str,
    cwd: Option<&str>,
    path: &Path,
    record_type: &str,
    line_number: usize,
    timestamp: Option<String>,
    role: &str,
    text: &str,
) {
    let normalized = normalize_message_text(text);
    if normalized.is_empty() {
        return;
    }
    let key = format!(
        "{}|{}|{}",
        role,
        timestamp.as_deref().unwrap_or(""),
        normalized
    );
    if !seen.insert(key) {
        return;
    }
    target.push(SessionBrainMessage {
        role: role.to_string(),
        provider,
        session_id: Some(session_id.to_string()),
        timestamp,
        cwd: cwd.map(str::to_string),
        transcript_path: normalize_windows_path_string(path.to_string_lossy().as_ref()),
        record_type: record_type.to_string(),
        line_number,
        text: normalized,
        source_kind: "root".to_string(),
    });
}

fn normalize_message_text(text: &str) -> String {
    let mut lines = Vec::new();
    let mut skip_until = None::<&str>;

    for raw_line in text.lines() {
        let line = raw_line.trim_end();
        let trimmed = line.trim();

        if let Some(marker) = skip_until {
            if trimmed == marker {
                skip_until = None;
            }
            continue;
        }

        if let Some(marker) = noise_block_end_marker(trimmed) {
            skip_until = Some(marker);
            continue;
        }

        if is_noise_line(trimmed) {
            continue;
        }

        if !trimmed.is_empty() {
            lines.push(line.to_string());
        }
    }

    lines.join("\n").trim().to_string()
}

fn noise_block_end_marker(line: &str) -> Option<&'static str> {
    if line == "<INSTRUCTIONS>" {
        Some("</INSTRUCTIONS>")
    } else if line == "<environment_context>" {
        Some("</environment_context>")
    } else if line.starts_with("<context_packet") {
        Some("</context_packet>")
    } else if line.starts_with("<runtime_context_v1") {
        Some("</runtime_context_v1>")
    } else {
        wrapper_block_end_marker(line)
    }
}

fn is_noise_line(line: &str) -> bool {
    if line.is_empty() {
        return false;
    }
    let lowered = line.to_ascii_lowercase();
    lowered.starts_with("# agents.md instructions for")
        || lowered.contains("autonomy directive")
        || lowered.contains("you are an autonomous coding agent")
        || lowered.contains("files called agents.md commonly appear")
        || lowered.contains("their purpose is to pass along human guidance")
        || lowered.contains("when two agents.md files disagree")
        || lowered.contains("continue the current task using the packet below")
        || lowered.starts_with("<task_goal>")
        || lowered.starts_with("<context_packet")
        || lowered.starts_with("</context_packet")
        || lowered.starts_with("<runtime_context_v1")
        || lowered.starts_with("</runtime_context_v1")
        || is_wrapper_tag_line(&lowered)
}

fn assistant_text_is_material(text: &str) -> bool {
    let lowered = text.to_ascii_lowercase();
    assistant_reports_completion(&lowered)
        || assistant_reports_blocker(&lowered)
        || assistant_reports_next_step(&lowered)
        || assistant_reports_decision(&lowered)
        || assistant_reports_finding(&lowered)
}

fn is_wrapper_tag_line(line: &str) -> bool {
    wrapper_block_end_marker(line).is_some()
        || line == "</subagent_notification>"
        || line == "</turn_aborted>"
        || line == "</skill>"
}

fn wrapper_block_end_marker(line: &str) -> Option<&'static str> {
    if line.starts_with("<subagent_notification") {
        Some("</subagent_notification>")
    } else if line.starts_with("<turn_aborted") {
        Some("</turn_aborted>")
    } else if line.starts_with("<skill") {
        Some("</skill>")
    } else if line.starts_with("<runtime_context_v1") {
        Some("</runtime_context_v1>")
    } else {
        None
    }
}

fn assistant_reports_completion(lowered: &str) -> bool {
    let has_result_verb = lowered.contains("confirmed")
        || lowered.contains("verified")
        || lowered.contains("passed")
        || lowered.contains("fixed")
        || lowered.contains("resolved")
        || lowered.contains("rebuilt")
        || lowered.contains("implemented")
        || lowered.contains("suppressed");
    has_result_verb && has_concrete_subject(lowered)
}

fn assistant_reports_blocker(lowered: &str) -> bool {
    let has_blocker_verb = lowered.contains("blocker")
        || lowered.contains("failed")
        || lowered.contains("failure")
        || lowered.contains("error");
    has_blocker_verb && has_concrete_subject(lowered)
}

fn assistant_reports_next_step(lowered: &str) -> bool {
    let starts_like_next_step = lowered.starts_with("next i'll ")
        || lowered.starts_with("next i will ")
        || lowered.starts_with("i'll ")
        || lowered.starts_with("i will ");
    starts_like_next_step
        && [
            "fix ", "update ", "rebuild ", "tighten ", "filter ", "remove ", "gate ", "adjust ",
            "strip ", "verify ", "rerun ",
        ]
        .iter()
        .any(|verb| lowered.contains(verb))
}

fn assistant_reports_decision(lowered: &str) -> bool {
    (lowered.starts_with("decision:")
        || lowered.contains("keeping step")
        || lowered.contains("keep step")
        || lowered.contains("treat step"))
        && has_concrete_subject(lowered)
}

fn assistant_reports_finding(lowered: &str) -> bool {
    [
        "echoes",
        "shows",
        "includes",
        "misses",
        "overweights",
        "outranks",
        "leaks",
        "reflects",
    ]
    .iter()
    .any(|phrase| lowered.contains(phrase))
        && has_concrete_subject(lowered)
}

fn has_concrete_subject(lowered: &str) -> bool {
    lowered.contains("session brain")
        || lowered.contains("agenda")
        || lowered.contains("prompt")
        || lowered.contains("current ask")
        || lowered.contains("task path")
        || lowered.contains("worldview")
        || lowered.contains("open obligation")
        || lowered.contains("inspection command")
        || lowered.contains("cargo build")
        || lowered.contains("cargo test")
        || lowered.contains("build.rs")
        || lowered.contains("evidence.rs")
        || lowered.contains("messages.rs")
        || lowered.contains("src/")
}

fn extract_codex_content_text(content: Option<&Value>) -> Option<String> {
    let content = content?;
    if let Some(text) = content.as_str() {
        let trimmed = text.trim();
        return (!trimmed.is_empty()).then(|| trimmed.to_string());
    }
    let array = content.as_array()?;
    let mut parts = Vec::new();
    for item in array {
        if let Some(text) = item.get("text").and_then(Value::as_str) {
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                parts.push(trimmed.to_string());
                continue;
            }
        }
        if let Some(text) = item.get("content").and_then(Value::as_str) {
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                parts.push(trimmed.to_string());
            }
        }
    }
    (!parts.is_empty()).then(|| parts.join("\n"))
}

fn extract_claude_user_text(entry: &Value) -> Option<String> {
    let content = entry.pointer("/message/content")?;
    if let Some(text) = content.as_str() {
        let trimmed = text.trim();
        return (!trimmed.is_empty()).then(|| trimmed.to_string());
    }

    let array = content.as_array()?;
    let mut parts = Vec::new();
    for block in array {
        match block.get("type").and_then(Value::as_str) {
            Some("tool_result") => return None,
            Some("text") => {
                if let Some(text) = block.get("text").and_then(Value::as_str) {
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

fn extract_claude_assistant_text(entry: &Value) -> Option<String> {
    let content = entry.pointer("/message/content")?;
    let array = content.as_array()?;
    let mut parts = Vec::new();
    for block in array {
        if block.get("type").and_then(Value::as_str) != Some("text") {
            continue;
        }
        if let Some(text) = block.get("text").and_then(Value::as_str) {
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                parts.push(trimmed.to_string());
            }
        }
    }
    (!parts.is_empty()).then(|| parts.join("\n"))
}

fn normalized_project_root(path: &Path) -> String {
    normalize_windows_path_string(detect_project_root(path).to_string_lossy().as_ref())
}

fn project_matches(cwd: &str, project_root: &str) -> bool {
    let cwd_root = normalize_windows_path_string(
        detect_project_root(Path::new(cwd))
            .to_string_lossy()
            .as_ref(),
    );
    cwd_root == project_root
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::path::PathBuf;
    use std::time::Duration as StdDuration;

    fn fixture_path(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join("session_brain")
            .join(name)
    }

    #[test]
    fn latest_claude_session_checks_newest_files_first() {
        let temp = tempfile::tempdir().expect("tempdir");
        let sessions_root = temp.path().join("claude-projects");
        let project_root = temp.path().join("repo");
        fs::create_dir_all(&sessions_root).expect("sessions root");
        fs::create_dir_all(&project_root).expect("project root");

        let older_path = sessions_root.join("zzz-older.jsonl");
        fs::write(&older_path, [0xff, 0xfe]).expect("older invalid transcript");
        std::thread::sleep(StdDuration::from_millis(30));

        let newest_path = sessions_root.join("000-newest.jsonl");
        let newest = serde_json::json!({
            "type": "user",
            "cwd": project_root.to_string_lossy(),
            "message": { "content": "newest matching project" }
        });
        fs::write(&newest_path, format!("{newest}\n")).expect("newest transcript");

        let candidate = find_latest_claude_session_in_root(&project_root, &sessions_root)
            .expect("find latest")
            .expect("candidate");

        assert_eq!(candidate.session_id, "000-newest");
        assert_eq!(candidate.path, newest_path);
    }

    #[test]
    fn latest_codex_session_checks_newest_files_first() {
        let temp = tempfile::tempdir().expect("tempdir");
        let sessions_root = temp.path().join("codex-sessions");
        let project_root = temp.path().join("repo");
        fs::create_dir_all(&sessions_root).expect("sessions root");
        fs::create_dir_all(&project_root).expect("project root");

        let older_path = sessions_root.join("zzz-older.jsonl");
        fs::write(&older_path, [0xff, 0xfe]).expect("older invalid transcript");
        std::thread::sleep(StdDuration::from_millis(30));

        let newest_path = sessions_root.join("000-newest.jsonl");
        let newest = serde_json::json!({
            "timestamp": "2026-04-21T00:00:00Z",
            "type": "session_meta",
            "payload": {
                "id": "codex-newest",
                "cwd": project_root.to_string_lossy()
            }
        });
        fs::write(&newest_path, format!("{newest}\n")).expect("newest transcript");

        let candidate = find_latest_codex_session_in_root(&project_root, &sessions_root)
            .expect("find latest")
            .expect("candidate");

        assert_eq!(candidate.session_id, "codex-newest");
        assert_eq!(candidate.path, newest_path);
    }

    #[test]
    fn parse_codex_messages_keeps_user_and_material_assistant_text() {
        let temp = tempfile::NamedTempFile::new().expect("tempfile");
        let mut file = temp.reopen().expect("reopen");
        writeln!(
            file,
            "{{\"timestamp\":\"2026-04-15T10:00:00Z\",\"type\":\"session_meta\",\"payload\":{{\"id\":\"abc\",\"cwd\":\"C:\\\\repo\"}}}}"
        )
        .expect("meta");
        writeln!(
            file,
            "{{\"timestamp\":\"2026-04-15T10:00:01Z\",\"type\":\"event_msg\",\"payload\":{{\"type\":\"user_message\",\"message\":\"Ship the fix\"}}}}"
        )
        .expect("user");
        writeln!(
            file,
            "{{\"timestamp\":\"2026-04-15T10:00:02Z\",\"type\":\"response_item\",\"payload\":{{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{{\"text\":\"Verified agenda precedence rebuilt; cargo test passed.\"}}]}}}}"
        )
        .expect("assistant");

        let parsed = parse_codex_messages(temp.path(), "abc").expect("parse");
        assert_eq!(parsed.user.len(), 1);
        assert_eq!(parsed.assistant.len(), 1);
        assert_eq!(parsed.user[0].text, "Ship the fix");
    }

    #[test]
    fn parse_codex_messages_tracks_shell_function_call_workdir() {
        let temp = tempfile::NamedTempFile::new().expect("tempfile");
        let mut file = temp.reopen().expect("reopen");
        let meta = serde_json::json!({
            "timestamp": "2026-04-15T10:00:00Z",
            "type": "session_meta",
            "payload": { "id": "abc", "cwd": "C:\\Users\\OEM\\Projects" }
        });
        let call_args = serde_json::json!({
            "command": "cargo test",
            "workdir": "C:\\Users\\OEM\\Projects\\munin-memory\\.worktrees\\fix-session-lookup-mtime-sort"
        });
        let function_call = serde_json::json!({
            "timestamp": "2026-04-15T10:00:01Z",
            "type": "response_item",
            "payload": {
                "type": "function_call",
                "name": "shell_command",
                "arguments": call_args.to_string()
            }
        });
        let user = serde_json::json!({
            "timestamp": "2026-04-15T10:00:02Z",
            "type": "event_msg",
            "payload": { "type": "user_message", "message": "$munin-brain" }
        });
        writeln!(file, "{meta}").expect("meta");
        writeln!(file, "{function_call}").expect("function call");
        writeln!(file, "{user}").expect("user");

        let parsed = parse_codex_messages(temp.path(), "abc").expect("parse");

        assert_eq!(
            parsed.user[0].cwd.as_deref(),
            Some(
                "C:\\Users\\OEM\\Projects\\munin-memory\\.worktrees\\fix-session-lookup-mtime-sort"
            )
        );
    }

    #[test]
    fn parse_claude_messages_ignores_tool_results() {
        let temp = tempfile::NamedTempFile::new().expect("tempfile");
        let mut file = temp.reopen().expect("reopen");
        writeln!(
            file,
            "{{\"type\":\"user\",\"timestamp\":\"2026-04-15T10:00:01Z\",\"cwd\":\"C:\\\\repo\",\"message\":{{\"content\":[{{\"type\":\"tool_result\",\"tool_use_id\":\"x\",\"content\":\"ok\"}}]}}}}"
        )
        .expect("tool result");
        writeln!(
            file,
            "{{\"type\":\"assistant\",\"timestamp\":\"2026-04-15T10:00:02Z\",\"cwd\":\"C:\\\\repo\",\"message\":{{\"content\":[{{\"type\":\"text\",\"text\":\"I found the current bad output echoes the inspection command.\"}}]}}}}"
        )
        .expect("assistant");

        let parsed = parse_claude_messages(temp.path(), "claude-1").expect("parse");
        assert!(parsed.user.is_empty());
        assert_eq!(parsed.assistant.len(), 1);
    }

    #[test]
    fn codex_fixture_filters_agents_noise_and_keeps_real_ask() {
        let parsed = parse_codex_messages(
            &fixture_path("codex-transcript.jsonl"),
            "019d8e9e-de46-7952-8a3c-14e2c38c13c5",
        )
        .expect("parse fixture");

        assert!(parsed
            .user
            .iter()
            .any(|message| message.text.contains("Fix the Session Brain content")));
        assert!(!parsed
            .user
            .iter()
            .any(|message| message.text.contains("AUTONOMOUS CODING AGENT")));
        assert!(!parsed.assistant.iter().any(|message| message
            .text
            .contains("Continue the current task using the packet below")));
    }

    #[test]
    fn claude_fixture_filters_turn_abort_wrapper_and_preserves_rejection() {
        let parsed = parse_claude_messages(
            &fixture_path("claude-transcript.jsonl"),
            "claude-session-brain-1",
        )
        .expect("parse fixture");

        assert!(parsed.assistant.iter().any(|message| message
            .text
            .contains("current bad output echoes the inspection command")));
        assert!(parsed.user.iter().any(|message| message
            .text
            .contains("Do not add a second durable memory system")));
        assert!(!parsed
            .assistant
            .iter()
            .any(|message| message.text.contains("<context_packet")));
    }

    #[test]
    fn load_context_snapshot_messages_reads_referenced_snapshot() {
        let temp = tempfile::tempdir().expect("tempdir");
        let project_root = temp.path();
        let snapshot_dir = project_root.join(".codex").join("context");
        fs::create_dir_all(&snapshot_dir).expect("snapshot dir");
        let snapshot_path = snapshot_dir.join("session-brain-intake.md");
        fs::write(
            &snapshot_path,
            "# Task Statement\nFix the Session Brain content so it reflects the real session.",
        )
        .expect("write snapshot");

        let messages = vec![SessionBrainMessage {
            role: "user".to_string(),
            provider: SessionBrainProvider::Codex,
            session_id: Some("sess-1".to_string()),
            timestamp: Some("2026-04-15T00:00:00Z".to_string()),
            cwd: Some(project_root.display().to_string()),
            transcript_path: "C:/repo/session.jsonl".to_string(),
            record_type: "fixture".to_string(),
            line_number: 1,
            text: format!("Intake snapshot: {}", snapshot_path.display()),
            source_kind: "root".to_string(),
        }];

        let snapshots =
            load_context_snapshot_messages(project_root, &messages).expect("load snapshots");

        assert_eq!(snapshots.len(), 1);
        assert!(snapshots[0]
            .text
            .contains("Fix the Session Brain content so it reflects the real session."));
        assert_eq!(snapshots[0].record_type, "context-snapshot");
        assert_eq!(snapshots[0].source_kind, "snapshot");
        assert_eq!(snapshots[0].session_id.as_deref(), Some("sess-1"));
        assert_eq!(
            snapshots[0].timestamp.as_deref(),
            Some("2026-04-15T00:00:00Z")
        );
    }

    #[test]
    fn normalize_message_text_strips_skill_subagent_and_turn_abort_blocks() {
        let normalized = normalize_message_text(
            "Keep the real ask.\n<skill>\nname: ralph\n</skill>\n<subagent_notification>\n{\"status\":\"completed\"}\n</subagent_notification>\n<turn_aborted>\nThe user interrupted.\n</turn_aborted>\nStill keep this line.",
        );

        assert_eq!(normalized, "Keep the real ask.\nStill keep this line.");
    }

    #[test]
    fn decide_source_status_marks_recent_orchestrator_transcript_live() {
        let now = Utc::now();
        let recent = SystemTime::from(now - Duration::seconds(3));
        assert_eq!(decide_source_status(recent, true, now), "live");
    }

    #[test]
    fn decide_source_status_keeps_fallback_when_orchestrator_absent() {
        let now = Utc::now();
        let recent = SystemTime::from(now - Duration::seconds(3));
        assert_eq!(decide_source_status(recent, false, now), "fallback-latest");
    }

    #[test]
    fn decide_source_status_demotes_idle_orchestrator_transcript_to_fallback() {
        let now = Utc::now();
        let idle = SystemTime::from(now - Duration::seconds(180));
        assert_eq!(decide_source_status(idle, true, now), "fallback-latest");
    }

    #[test]
    fn decide_source_status_marks_old_transcript_stale_regardless_of_orchestrator() {
        let now = Utc::now();
        let old = SystemTime::from(now - Duration::hours(48));
        assert_eq!(decide_source_status(old, true, now), "stale");
        assert_eq!(decide_source_status(old, false, now), "stale");
    }

    #[test]
    fn assistant_materiality_requires_concrete_signal_not_keyword_bag() {
        assert!(assistant_text_is_material(
            "Verified agenda precedence rebuilt; cargo test passed."
        ));
        assert!(assistant_text_is_material(
            "I found the current bad output echoes the inspection command."
        ));
        assert!(!assistant_text_is_material(
            "Found it. Next steps and risks are noted."
        ));
    }
}
