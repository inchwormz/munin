use crate::core::strategy;
use anyhow::Result;
use clap::ValueEnum;
use serde::Serialize;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum StrategyFormat {
    Text,
    Json,
    Prompt,
}

impl StrategyFormat {
    fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Json => "json",
            Self::Prompt => "prompt",
        }
    }
}

#[derive(Debug, Clone)]
pub struct StrategySetupRequest {
    pub scope: String,
    pub import_path: Option<PathBuf>,
    pub bootstrap_claude: bool,
    pub template: bool,
    pub format: StrategyFormat,
}

#[derive(Debug, Clone)]
pub struct StrategyReadRequest {
    pub scope: String,
    pub format: StrategyFormat,
}

pub fn run_setup(request: StrategySetupRequest) -> Result<()> {
    let report = strategy::setup(&strategy::StrategySetupOptions {
        scope: request.scope,
        import_path: request.import_path,
        bootstrap_claude: request.bootstrap_claude,
        template: request.template,
    })?;
    render_response(&report, request.format)
}

pub fn run_inspect(request: StrategyReadRequest) -> Result<()> {
    let report = strategy::inspect(&strategy::StrategyReadOptions {
        scope: request.scope,
    })?;
    render_response(&report, request.format)
}

pub fn run_status(request: StrategyReadRequest) -> Result<()> {
    let report = strategy::status(&strategy::StrategyReadOptions {
        scope: request.scope,
    })?;
    render_response(&report, request.format)
}

pub fn run_recommend(request: StrategyReadRequest) -> Result<()> {
    let report = strategy::recommend(&strategy::StrategyReadOptions {
        scope: request.scope,
    })?;
    match request.format {
        StrategyFormat::Text => println!("{}", render_recommend_text(&report)),
        StrategyFormat::Json => println!("{}", serde_json::to_string_pretty(&report)?),
        StrategyFormat::Prompt => println!("{}", render_prompt(&report)?),
    }
    Ok(())
}

fn render_response<T: Serialize>(report: &T, format: StrategyFormat) -> Result<()> {
    match format {
        StrategyFormat::Text => println!("{}", render_text(report)?),
        StrategyFormat::Json => println!("{}", serde_json::to_string_pretty(report)?),
        StrategyFormat::Prompt => println!("{}", render_prompt(report)?),
    }
    Ok(())
}

fn render_text<T: Serialize>(report: &T) -> Result<String> {
    Ok(serde_json::to_string_pretty(report)?)
}

fn render_recommend_text(report: &strategy::StrategyRecommendReport) -> String {
    let mut lines = vec![
        "Strategy Nudge".to_string(),
        "--------------".to_string(),
        format!("Scope: {}", report.scope_id),
    ];
    if report.nudges.is_empty() {
        lines.push("No strategic nudge is ready from the current evidence.".to_string());
    } else if metrics_snapshot_lacks_current_values(report) {
        lines.push(format!("1. Fill {} KPI metrics.", report.scope_id));
        lines.push(
            "   why now: Strategy has KPI targets, but current metric values are missing."
                .to_string(),
        );
        lines.push(format!(
            "   do next: munin metrics set <metric_key> <value> --scope {}",
            report.scope_id
        ));
        lines.push("   confidence: medium".to_string());
    } else {
        for (index, nudge) in report.nudges.iter().take(3).enumerate() {
            lines.push(format!("{}. {}", index + 1, nudge.task));
            lines.push(format!("   why now: {}", nudge.why_now));
            lines.push(format!("   confidence: {}", nudge.confidence));
            for evidence in nudge.evidence.iter().take(2) {
                lines.push(format!("   evidence: {}", evidence));
            }
        }
    }
    if !report.warnings.is_empty() {
        lines.push("Warnings:".to_string());
        for warning in report.warnings.iter().take(3) {
            lines.push(format!("- {}", warning));
        }
    }
    lines.join("\n")
}

fn metrics_snapshot_lacks_current_values(report: &strategy::StrategyRecommendReport) -> bool {
    report
        .warnings
        .iter()
        .any(|warning| warning.contains("no current values"))
}

fn render_prompt<T: Serialize>(report: &T) -> Result<String> {
    Ok(format!(
        "<strategy_report format=\"{}\">\n{}\n</strategy_report>",
        StrategyFormat::Prompt.as_str(),
        serde_json::to_string_pretty(report)?
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strategy_format_renders_expected_strings() {
        assert_eq!(StrategyFormat::Text.as_str(), "text");
        assert_eq!(StrategyFormat::Json.as_str(), "json");
        assert_eq!(StrategyFormat::Prompt.as_str(), "prompt");
    }

    #[test]
    fn prompt_wrapper_includes_strategy_report_tag() {
        let rendered = render_prompt(&serde_json::json!({ "scope": "sitesorted-business" }))
            .expect("prompt render");
        assert!(rendered.contains("<strategy_report"));
        assert!(rendered.contains("sitesorted-business"));
    }
}
