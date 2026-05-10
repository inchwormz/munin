use crate::core::proactivity;
use anyhow::{Context, Result};
use clap::ValueEnum;
use serde::Serialize;
use serde_json::Value;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ProactivityFormat {
    Text,
    Json,
}

#[derive(Debug, Clone)]
pub struct ProactivityRunRequest {
    pub scope: Option<String>,
    pub provider: Option<crate::core::config::ProactivityProvider>,
    pub dry_run: bool,
    pub auto_spawn: bool,
    pub no_spawn: bool,
    pub format: ProactivityFormat,
}

#[derive(Debug, Clone)]
pub struct ProactivityScopeRequest {
    pub scope: Option<String>,
    pub format: ProactivityFormat,
}

#[derive(Debug, Clone)]
pub struct ProactivityScheduleInstallRequest {
    pub scope: Option<String>,
    pub provider: Option<crate::core::config::ProactivityProvider>,
    pub project_path: Option<PathBuf>,
    pub format: ProactivityFormat,
}

#[derive(Debug, Clone)]
pub struct ProactivityClaimRequest {
    pub job_id: String,
    pub format: ProactivityFormat,
}

#[derive(Debug, Clone)]
pub struct ProactivityApproveRequest {
    pub job_id: String,
    pub no_spawn: bool,
    pub format: ProactivityFormat,
}

#[derive(Debug, Clone)]
pub struct ProactivityCompleteRequest {
    pub job_id: String,
    pub status: proactivity::ProactivityTerminalStatus,
    pub summary: String,
    pub error: Option<String>,
    pub notes: Vec<String>,
    pub format: ProactivityFormat,
}

pub fn run(request: ProactivityRunRequest) -> Result<()> {
    let report = proactivity::run(&proactivity::ProactivityRunOptions {
        scope: request.scope,
        provider: request.provider,
        dry_run: request.dry_run,
        auto_spawn: request.auto_spawn,
        no_spawn: request.no_spawn,
    })?;
    render_response(&report, request.format)
}

pub fn sweep(request: ProactivityScopeRequest) -> Result<()> {
    let report = proactivity::sweep(&proactivity::ProactivityScopeOptions {
        scope: request.scope,
    })?;
    render_response(&report, request.format)
}

pub fn status(request: ProactivityScopeRequest) -> Result<()> {
    let report = proactivity::status(&proactivity::ProactivityScopeOptions {
        scope: request.scope,
    })?;
    render_response(&report, request.format)
}

pub fn schedule_install(request: ProactivityScheduleInstallRequest) -> Result<()> {
    let report = proactivity::install_schedule(&proactivity::ProactivityScheduleInstallOptions {
        scope: request.scope,
        provider: request.provider,
        project_path: request.project_path,
    })?;
    render_response(&report, request.format)
}

pub fn schedule_remove(request: ProactivityScopeRequest) -> Result<()> {
    let report = proactivity::remove_schedule(&proactivity::ProactivityScopeOptions {
        scope: request.scope,
    })?;
    render_response(&report, request.format)
}

pub fn claim(request: ProactivityClaimRequest) -> Result<()> {
    let report = proactivity::claim(&proactivity::ProactivityClaimOptions {
        job_id: request.job_id,
    })?;
    render_response(&report, request.format)
}

pub fn approve(request: ProactivityApproveRequest) -> Result<()> {
    let report = proactivity::approve(&proactivity::ProactivityApproveOptions {
        job_id: request.job_id,
        no_spawn: request.no_spawn,
    })?;
    render_response(&report, request.format)
}

pub fn complete(request: ProactivityCompleteRequest) -> Result<()> {
    let report = proactivity::complete(&proactivity::ProactivityCompleteOptions {
        job_id: request.job_id,
        status: request.status,
        summary: request.summary,
        error: request.error,
        notes: request.notes,
    })?;
    render_response(&report, request.format)
}

fn render_response<T: Serialize>(report: &T, format: ProactivityFormat) -> Result<()> {
    match format {
        ProactivityFormat::Text => println!("{}", render_text_response(report)?),
        ProactivityFormat::Json => println!("{}", serde_json::to_string_pretty(report)?),
    }
    Ok(())
}

fn render_text_response<T: Serialize>(report: &T) -> Result<String> {
    let value = serde_json::to_value(report).context("failed to render proactivity response")?;
    let Some(object) = value.as_object() else {
        return Ok(render_text_value(&value));
    };
    let mut lines = Vec::new();
    for (key, value) in object {
        lines.push(format!(
            "{}: {}",
            humanize_key(key),
            render_text_value(value)
        ));
    }
    Ok(lines.join("\n"))
}

fn humanize_key(key: &str) -> String {
    let mut text = key.replace('_', " ");
    if let Some(first) = text.get_mut(0..1) {
        first.make_ascii_uppercase();
    }
    text
}

fn render_text_value(value: &Value) -> String {
    match value {
        Value::Null => "none".to_string(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        Value::String(value) => value.clone(),
        Value::Array(values) => {
            if values.is_empty() {
                "none".to_string()
            } else if values.iter().all(|value| value.as_str().is_some()) {
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            } else {
                serde_json::to_string(value).unwrap_or_else(|_| "<unrenderable>".to_string())
            }
        }
        Value::Object(_) => {
            serde_json::to_string(value).unwrap_or_else(|_| "<unrenderable>".to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_response_is_not_json_object_dump() {
        let report = serde_json::json!({
            "generated_at": "2026-05-10T00:00:00Z",
            "scope_id": "sitesorted-business",
            "provider": "codex",
            "today_pending": false,
            "reasons": ["approval_queue_ready"]
        });

        let rendered = render_text_response(&report).expect("render text");

        assert!(rendered.starts_with("Generated at: 2026-05-10T00:00:00Z"));
        assert!(rendered.contains("Provider: codex"));
        assert!(rendered.contains("Reasons: approval_queue_ready"));
        assert!(!rendered.trim_start().starts_with('{'));
    }
}
