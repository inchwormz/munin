use super::types::SessionBrainStrategyContext;
use crate::core::config::{Config, StrategyScopeConfig};
use crate::core::strategy::{self, StrategyReadOptions};
use crate::core::utils::{detect_project_root, normalize_windows_path_string};
use anyhow::{Context, Result};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

pub fn build_strategy_context(project_root: &Path) -> Result<SessionBrainStrategyContext> {
    let config = Config::load().context("failed to load config.toml")?;
    for scope_id in resolve_relevant_scopes(&config, project_root) {
        if let Ok(context) = build_strategy_context_for_scope(scope_id) {
            return Ok(context);
        }
    }

    Ok(SessionBrainStrategyContext {
        summary: Vec::new(),
        source_paths: Vec::new(),
        planning_complete: false,
    })
}

fn build_strategy_context_for_scope(scope_id: String) -> Result<SessionBrainStrategyContext> {
    let status = strategy::status(&StrategyReadOptions {
        scope: scope_id.clone(),
    })?;
    let recommend = strategy::recommend(&StrategyReadOptions { scope: scope_id }).ok();

    let mut summary = Vec::new();
    if status.continuity.active {
        if let Some(detail) = status.continuity.summary.as_deref() {
            summary.push(format!("Continuity: {detail}"));
        } else {
            summary.push("Continuity is active for this project.".to_string());
        }
    }
    for item in status.items.iter().take(3) {
        summary.push(format!(
            "{} [{}]: {}",
            item.item_kind, item.status, item.title
        ));
    }
    if let Some(report) = recommend {
        for nudge in report.nudges.iter().take(2) {
            summary.push(format!("Next: {}", nudge.task));
        }
    }

    let mut source_paths = BTreeSet::new();
    source_paths.insert(normalize_windows_path_string(
        status.registry.artifact_path.to_string_lossy().as_ref(),
    ));
    source_paths.insert(normalize_windows_path_string(
        status.registry.metrics_path.to_string_lossy().as_ref(),
    ));
    source_paths.insert(normalize_windows_path_string(
        status.registry.storage_dir.to_string_lossy().as_ref(),
    ));
    if let Some(path) = status.registry.continuity_project_path.as_ref() {
        source_paths.insert(normalize_windows_path_string(
            path.to_string_lossy().as_ref(),
        ));
    }
    for path in &status.registry.signal_paths {
        source_paths.insert(normalize_windows_path_string(
            path.to_string_lossy().as_ref(),
        ));
    }

    Ok(SessionBrainStrategyContext {
        summary,
        source_paths: source_paths.into_iter().collect(),
        planning_complete: !status.items.is_empty(),
    })
}

fn resolve_relevant_scopes(config: &Config, project_root: &Path) -> Vec<String> {
    let project_root = normalized_project_root(project_root);
    let mut scopes = Vec::new();

    scopes.extend(
        config
            .strategy
            .scopes
            .iter()
            .filter_map(|(scope_id, scope)| {
                (scope.enabled && scope_matches_project(scope, &project_root))
                    .then(|| scope_id.clone())
            }),
    );

    scopes
}

fn scope_matches_project(scope: &StrategyScopeConfig, project_root: &str) -> bool {
    if let Some(path) = scope.continuity_project_path.as_deref() {
        return normalized_project_root(path) == project_root;
    }
    if let Some(path) = scope.artifact_path.as_deref() {
        return path_in_project(path, project_root);
    }
    false
}

fn path_in_project(path: &Path, project_root: &str) -> bool {
    let normalized = normalize_windows_path_string(path.to_string_lossy().as_ref());
    normalized.starts_with(project_root)
}

fn normalized_project_root(path: &Path) -> String {
    let detected = detect_project_root(path);
    let root = if detected.as_os_str().is_empty() {
        path
    } else {
        detected.as_path()
    };
    normalize_windows_path_string(root.to_string_lossy().as_ref())
}

#[allow(dead_code)]
fn as_path_buf(path: &Path) -> PathBuf {
    path.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::config::StrategyConfig;
    use std::collections::BTreeMap;

    #[test]
    fn relevant_scopes_do_not_include_non_matching_default_scope() {
        let mut scopes = BTreeMap::new();
        scopes.insert(
            "sitesorted-business".to_string(),
            StrategyScopeConfig {
                continuity_project_path: Some(PathBuf::from("C:/Users/OEM/Projects/sitesorted")),
                ..Default::default()
            },
        );
        let config = Config {
            strategy: StrategyConfig {
                enabled: true,
                default_scope: Some("sitesorted-business".to_string()),
                directory: None,
                scopes,
            },
            ..Default::default()
        };

        let relevant =
            resolve_relevant_scopes(&config, Path::new("C:/Users/OEM/Projects/munin-memory"));

        assert!(relevant.is_empty());
    }
}
