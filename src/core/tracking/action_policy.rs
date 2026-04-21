use anyhow::Result;
use chrono::{Duration, Utc};
use rusqlite::params;
use std::collections::HashMap;

use super::{
    compact_display_text, current_project_path_string, hash_text, parse_rfc3339_to_utc,
    project_filter_params, push_unique_string, resolved_project_path,
    scope_project_path_or_current, Tracker,
};

#[derive(Debug, Clone)]
struct MemoryOsActionObservationRow {
    observation_id: String,
    source_kind: String,
    cue_fingerprint: String,
    action_fingerprint: String,
    cue_json: String,
    action_json: String,
    source_ref: String,
    observed_at: String,
}

#[derive(Debug, Clone)]
struct MemoryOsActionExecutionRow {
    execution_id: String,
    execution_kind: String,
    command_sig: String,
    subject_ref: Option<String>,
    exit_code: i32,
    observed_at: String,
}

fn action_policy_strength_rank(value: &str) -> i32 {
    match value {
        "strong-default" => 3,
        "default" => 2,
        "caution" => 1,
        _ => 0,
    }
}

fn action_candidate_title(
    cue: &crate::core::memory_os::MemoryOsActionCue,
    action: &crate::core::memory_os::MemoryOsAction,
) -> String {
    cue.trigger_summary
        .clone()
        .or_else(|| cue.trigger_subject.clone())
        .or_else(|| action.command_sig.clone())
        .or_else(|| action.recommendation.clone())
        .unwrap_or_else(|| format!("Observed {}", action.action_kind))
}

fn action_candidate_summary(
    cue: &crate::core::memory_os::MemoryOsActionCue,
    action: &crate::core::memory_os::MemoryOsAction,
    precedent_count: usize,
    success_count: usize,
    failure_count: usize,
) -> String {
    let trigger = cue
        .trigger_summary
        .as_deref()
        .or(cue.trigger_subject.as_deref())
        .unwrap_or("observed cue");
    let effect = action
        .command_sig
        .as_deref()
        .or(action.recommendation.as_deref())
        .unwrap_or("recommended follow-up");
    format!(
        "{} -> {} (precedents {}, success {}, failure {})",
        compact_display_text(trigger, 120),
        compact_display_text(effect, 120),
        precedent_count,
        success_count,
        failure_count
    )
}

fn action_candidate_status(
    source_kind: &str,
    action: &crate::core::memory_os::MemoryOsAction,
    precedent_count: usize,
    success_count: usize,
    failure_count: usize,
) -> &'static str {
    if failure_count > success_count && failure_count > 0 {
        return "degraded";
    }
    let learned_or_non_command =
        source_kind.starts_with("learned-") || action.command_sig.is_none();
    if learned_or_non_command {
        if precedent_count >= 3 && failure_count == 0 {
            "promotable"
        } else {
            "candidate"
        }
    } else if precedent_count >= 2 && success_count >= 2 && failure_count == 0 {
        "promotable"
    } else {
        "candidate"
    }
}

fn action_candidate_confidence(
    status: &str,
    precedent_count: usize,
    success_count: usize,
    failure_count: usize,
) -> &'static str {
    if status == "degraded" || failure_count > success_count {
        "low"
    } else if success_count >= 2 || precedent_count >= 4 {
        "high"
    } else if success_count >= 1 || precedent_count >= 3 {
        "medium"
    } else {
        "low"
    }
}

fn action_lifecycle_defaults(
    action_kind: &str,
    last_observed_at: &str,
) -> (Option<String>, Option<String>, Option<String>, String) {
    let observed_at = parse_rfc3339_to_utc(last_observed_at);
    let now = Utc::now();
    let (review_after, expires_at, lifecycle_policy) = match action_kind {
        "strategy-queue" => (
            Some((observed_at + Duration::days(1)).to_rfc3339()),
            Some((observed_at + Duration::days(7)).to_rfc3339()),
            None,
        ),
        "read-path-policy" | "behavior-change" => (
            Some((observed_at + Duration::days(14)).to_rfc3339()),
            Some((observed_at + Duration::days(45)).to_rfc3339()),
            None,
        ),
        "serving-policy" => (None, None, Some("non-expiring-policy".to_string())),
        _ => (
            Some((observed_at + Duration::days(7)).to_rfc3339()),
            Some((observed_at + Duration::days(30)).to_rfc3339()),
            None,
        ),
    };

    let aging_status = if lifecycle_policy.is_some() {
        "non-expiring-policy".to_string()
    } else if expires_at
        .as_deref()
        .map(parse_rfc3339_to_utc)
        .is_some_and(|expires| now >= expires)
    {
        "expired".to_string()
    } else if review_after
        .as_deref()
        .map(parse_rfc3339_to_utc)
        .is_some_and(|review| now >= review)
    {
        "review-due".to_string()
    } else {
        "fresh".to_string()
    };

    (review_after, expires_at, lifecycle_policy, aging_status)
}

fn looks_sensitive_command(command: &str) -> bool {
    let lowered = command.to_ascii_lowercase();
    let sensitive_markers = [
        "api_key",
        "apikey",
        "token",
        "secret",
        "password",
        "authorization",
        "bearer ",
        ".env",
        "firecrawl_api_key",
        "resend_api_key",
    ];
    sensitive_markers
        .iter()
        .any(|marker| lowered.contains(marker))
}

impl Tracker {
    pub(super) fn record_memory_os_action_observation(
        &self,
        project_path: &str,
        source_kind: &str,
        source_event_id: Option<&str>,
        cue: &crate::core::memory_os::MemoryOsActionCue,
        action: &crate::core::memory_os::MemoryOsAction,
        source_ref: &str,
        observed_at: &str,
    ) -> Result<()> {
        let cue_json = serde_json::to_string(cue)?;
        let action_json = serde_json::to_string(action)?;
        let cue_fingerprint = hash_text(&cue_json);
        let action_fingerprint = hash_text(&action_json);
        let observation_id = format!(
            "action-observation-{}",
            hash_text(&format!(
                "{}:{}:{}:{}:{}",
                project_path, source_kind, source_ref, cue_fingerprint, action_fingerprint
            ))
        );

        self.conn.execute(
            "INSERT OR IGNORE INTO memory_os_action_observations (
                observation_id, project_path, source_kind, source_event_id,
                cue_fingerprint, action_fingerprint, cue_json, action_json, source_ref, observed_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                observation_id,
                project_path,
                source_kind,
                source_event_id,
                cue_fingerprint,
                action_fingerprint,
                cue_json,
                action_json,
                source_ref,
                observed_at,
            ],
        )?;
        Ok(())
    }

    pub fn record_memory_os_action_observation_for_project(
        &self,
        project_path: &str,
        source_kind: &str,
        cue: &crate::core::memory_os::MemoryOsActionCue,
        action: &crate::core::memory_os::MemoryOsAction,
        source_ref: &str,
        observed_at: &str,
    ) -> Result<()> {
        let flags = crate::core::config::memory_os();
        if !flags.action_v1 {
            return Ok(());
        }

        self.record_memory_os_action_observation(
            project_path,
            source_kind,
            None,
            cue,
            action,
            source_ref,
            observed_at,
        )
    }

    pub fn record_memory_os_action_execution(
        &self,
        execution_kind: &str,
        command_sig: &str,
        subject_ref: Option<&str>,
        exit_code: i32,
    ) -> Result<()> {
        let project_path = current_project_path_string();
        self.record_memory_os_action_execution_for_project(
            &project_path,
            execution_kind,
            command_sig,
            subject_ref,
            exit_code,
        )
    }

    pub fn record_memory_os_action_execution_for_project(
        &self,
        project_path: &str,
        execution_kind: &str,
        command_sig: &str,
        subject_ref: Option<&str>,
        exit_code: i32,
    ) -> Result<()> {
        let observed_at = Utc::now().to_rfc3339();
        self.record_memory_os_action_execution_at_for_project(
            project_path,
            execution_kind,
            command_sig,
            subject_ref,
            exit_code,
            &observed_at,
        )
    }

    pub fn record_memory_os_action_execution_at_for_project(
        &self,
        project_path: &str,
        execution_kind: &str,
        command_sig: &str,
        subject_ref: Option<&str>,
        exit_code: i32,
        observed_at: &str,
    ) -> Result<()> {
        let flags = crate::core::config::memory_os();
        if !flags.action_v1 {
            return Ok(());
        }

        let execution_id = format!(
            "action-execution-{}",
            hash_text(&format!(
                "{}:{}:{}:{}:{}:{}",
                project_path,
                execution_kind,
                command_sig,
                subject_ref.unwrap_or(""),
                exit_code,
                observed_at
            ))
        );
        self.conn.execute(
            "INSERT OR IGNORE INTO memory_os_action_executions (
                execution_id, project_path, execution_kind, command_sig, subject_ref, exit_code, observed_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                execution_id,
                project_path,
                execution_kind,
                command_sig,
                subject_ref,
                exit_code,
                observed_at,
            ],
        )?;
        Ok(())
    }

    pub fn latest_memory_os_action_subject(&self, command_sig: &str) -> Result<Option<String>> {
        let project_path = current_project_path_string();
        let expected = canonical_memory_os_command_sig(command_sig);
        let mut stmt = self.conn.prepare(
            "SELECT cue_json, action_json
             FROM memory_os_action_observations
             WHERE project_path = ?1
             ORDER BY observed_at DESC",
        )?;
        let rows = stmt
            .query_map(params![project_path], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        for (cue_json, action_json) in rows {
            let cue: crate::core::memory_os::MemoryOsActionCue = serde_json::from_str(&cue_json)?;
            let action: crate::core::memory_os::MemoryOsAction =
                serde_json::from_str(&action_json)?;
            if action
                .command_sig
                .as_deref()
                .map(canonical_memory_os_command_sig)
                .as_deref()
                == Some(expected.as_str())
            {
                return Ok(cue.trigger_subject);
            }
        }

        Ok(None)
    }

    pub fn get_memory_os_action_candidates(
        &self,
        project_path: Option<&str>,
    ) -> Result<Vec<crate::core::memory_os::MemoryOsActionPolicyCandidate>> {
        self.get_memory_os_action_candidates_scoped(project_path, false)
    }

    pub fn get_memory_os_action_candidates_all(
        &self,
    ) -> Result<Vec<crate::core::memory_os::MemoryOsActionPolicyCandidate>> {
        self.get_memory_os_action_candidates_scoped(None, true)
    }

    pub fn get_memory_os_action_policy_view_report(
        &self,
        scope: crate::core::memory_os::MemoryOsInspectionScope,
        project_path: Option<&str>,
    ) -> Result<crate::core::memory_os::MemoryOsActionPolicyViewReport> {
        let candidates = match scope {
            crate::core::memory_os::MemoryOsInspectionScope::User => {
                self.get_memory_os_action_candidates_all()?
            }
            crate::core::memory_os::MemoryOsInspectionScope::Project => {
                self.get_memory_os_action_candidates(project_path)?
            }
        };
        let friction = self.get_memory_os_friction_report(scope, project_path)?;
        let overview = self.get_memory_os_overview_report(scope, project_path)?;
        let approvals = match scope {
            crate::core::memory_os::MemoryOsInspectionScope::User => {
                self.get_approval_jobs_filtered(50, None, None)?
            }
            crate::core::memory_os::MemoryOsInspectionScope::Project => {
                self.get_approval_jobs_filtered(50, project_path, None)?
            }
        };

        let scope_label = match scope {
            crate::core::memory_os::MemoryOsInspectionScope::User => "global".to_string(),
            crate::core::memory_os::MemoryOsInspectionScope::Project => "project".to_string(),
        };
        let scope_target = scope_project_path_or_current(scope, project_path);

        let mut assertions = Vec::new();
        let mut rules = Vec::new();

        for candidate in &candidates {
            let command_sig = candidate.action.command_sig.clone();
            if candidate.status != "promotable"
                || command_sig.as_deref().is_some_and(|command| {
                    command.contains('\n') || looks_sensitive_command(command)
                })
            {
                continue;
            }
            let assertion_id = format!("action-assertion:{}", candidate.candidate_id);
            assertions.push(crate::core::memory_os::MemoryOsActionPolicyAssertion {
                assertion_id: assertion_id.clone(),
                source_kind: candidate.source_kind.clone(),
                summary: candidate.title.clone(),
                scope: scope_label.clone(),
                scope_target: scope_target.clone(),
                supporting_evidence: candidate.source_refs.clone(),
            });
            let strength = match candidate.confidence.as_str() {
                "high" => "strong-default",
                "medium" => "default",
                _ => "caution",
            };
            rules.push(crate::core::memory_os::MemoryOsActionPolicyRule {
                rule_id: format!("action-policy:{}", candidate.candidate_id),
                title: candidate.title.clone(),
                summary: candidate.summary.clone(),
                action_kind: candidate.actuator_type.clone(),
                strength: strength.to_string(),
                confidence: candidate.confidence.clone(),
                scope: scope_label.clone(),
                scope_target: scope_target.clone(),
                target_agent: None,
                suggested_command: command_sig.clone(),
                recommendation: candidate.action.recommendation.clone(),
                review_after: candidate.review_after.clone(),
                expires_at: candidate.expires_at.clone(),
                lifecycle_policy: candidate.lifecycle_policy.clone(),
                aging_status: candidate.aging_status.clone(),
                trigger_assertion_ids: vec![assertion_id],
                supporting_evidence: candidate.source_refs.clone(),
            });
        }

        for change in &friction.behavior_changes {
            let assertion_id = format!(
                "action-assertion:{}",
                hash_text(&format!(
                    "{}:{}:{}",
                    change.target_agent, change.change, change.rationale
                ))
            );
            assertions.push(crate::core::memory_os::MemoryOsActionPolicyAssertion {
                assertion_id: assertion_id.clone(),
                source_kind: "behavior-change".to_string(),
                summary: change.change.clone(),
                scope: scope_label.clone(),
                scope_target: scope_target.clone(),
                supporting_evidence: change.evidence.clone(),
            });
            let strength = if change.target_agent == "both" || change.target_agent == "codex" {
                "strong-default"
            } else {
                "default"
            };
            rules.push(crate::core::memory_os::MemoryOsActionPolicyRule {
                rule_id: format!(
                    "action-policy:{}",
                    hash_text(&format!(
                        "{}:{}:{}",
                        change.target_agent, change.change, change.rationale
                    ))
                ),
                title: format!("Behavior change for {}", change.target_agent),
                summary: change.change.clone(),
                action_kind: "behavior-change".to_string(),
                strength: strength.to_string(),
                confidence: "medium".to_string(),
                scope: scope_label.clone(),
                scope_target: scope_target.clone(),
                target_agent: Some(change.target_agent.clone()),
                suggested_command: None,
                recommendation: Some(change.rationale.clone()),
                review_after: Some((Utc::now() + Duration::days(14)).to_rfc3339()),
                expires_at: Some((Utc::now() + Duration::days(45)).to_rfc3339()),
                lifecycle_policy: None,
                aging_status: "fresh".to_string(),
                trigger_assertion_ids: vec![assertion_id],
                supporting_evidence: change.evidence.clone(),
            });
        }

        for (index, policy) in overview.serving_policy.iter().enumerate() {
            let assertion_id = format!(
                "action-assertion:serving:{}",
                hash_text(&format!("{index}:{policy}"))
            );
            assertions.push(crate::core::memory_os::MemoryOsActionPolicyAssertion {
                assertion_id: assertion_id.clone(),
                source_kind: "serving-policy".to_string(),
                summary: policy.clone(),
                scope: scope_label.clone(),
                scope_target: scope_target.clone(),
                supporting_evidence: Vec::new(),
            });
            rules.push(crate::core::memory_os::MemoryOsActionPolicyRule {
                rule_id: format!(
                    "action-policy:serving:{}",
                    hash_text(&format!("{index}:{policy}"))
                ),
                title: "Serving policy".to_string(),
                summary: policy.clone(),
                action_kind: "serving-policy".to_string(),
                strength: "strong-default".to_string(),
                confidence: "high".to_string(),
                scope: scope_label.clone(),
                scope_target: scope_target.clone(),
                target_agent: Some("both".to_string()),
                suggested_command: None,
                recommendation: None,
                review_after: None,
                expires_at: None,
                lifecycle_policy: Some("non-expiring-policy".to_string()),
                aging_status: "non-expiring-policy".to_string(),
                trigger_assertion_ids: vec![assertion_id],
                supporting_evidence: Vec::new(),
            });
        }

        rules.sort_by(|left, right| {
            action_policy_strength_rank(&right.strength)
                .cmp(&action_policy_strength_rank(&left.strength))
                .then(left.action_kind.cmp(&right.action_kind))
                .then(left.title.cmp(&right.title))
        });
        assertions.sort_by(|left, right| left.summary.cmp(&right.summary));
        assertions.dedup_by(|left, right| left.assertion_id == right.assertion_id);
        rules.dedup_by(|left, right| left.rule_id == right.rule_id);
        let approval_records = approvals
            .into_iter()
            .map(|record| crate::core::memory_os::MemoryOsApprovalJobRecord {
                job_id: record.job_id,
                source_kind: record.source_kind,
                status: record.status.to_string(),
                title: record.title,
                summary: record.summary,
                scope: record.scope,
                scope_target: record.scope_target,
                item_id: record.item_id,
                item_kind: record.item_kind,
                local_date: record.local_date,
                expected_effect: record.expected_effect,
                queue_path: record.queue_path,
                result_path: record.result_path,
                created_at: record.created_at.to_rfc3339(),
                updated_at: record.updated_at.to_rfc3339(),
                review_after: record.review_after,
                expires_at: record.expires_at,
                last_reviewed_at: record.last_reviewed_at,
                closure_reason: record.closure_reason,
                supporting_evidence: serde_json::from_str::<Vec<String>>(&record.evidence_json)
                    .unwrap_or_default(),
            })
            .collect::<Vec<_>>();
        let hook_capabilities = Vec::new();

        Ok(crate::core::memory_os::MemoryOsActionPolicyViewReport {
            generated_at: Utc::now().to_rfc3339(),
            scope,
            candidate_count: candidates.len(),
            candidates,
            behavior_change_count: friction.behavior_changes.len(),
            assertion_count: assertions.len(),
            assertions,
            approvals_count: approval_records.len(),
            approvals: approval_records,
            hook_capabilities,
            rules,
        })
    }

    fn get_memory_os_action_candidates_scoped(
        &self,
        project_path: Option<&str>,
        include_all_projects: bool,
    ) -> Result<Vec<crate::core::memory_os::MemoryOsActionPolicyCandidate>> {
        let (project_exact, project_glob) = if include_all_projects {
            (None, None)
        } else {
            let resolved_project_path = resolved_project_path(project_path);
            project_filter_params(Some(&resolved_project_path))
        };
        let mut observation_stmt = self.conn.prepare(
            "SELECT observation_id, source_kind, cue_fingerprint, action_fingerprint,
                    cue_json, action_json, source_ref, observed_at
             FROM memory_os_action_observations
             WHERE (?1 IS NULL OR project_path = ?1 OR project_path GLOB ?2)
               AND NOT (source_kind = 'checkpoint-reentry' AND source_ref LIKE 'onboarding-%')
             ORDER BY observed_at ASC",
        )?;
        let observations = observation_stmt
            .query_map(params![project_exact, project_glob], |row| {
                Ok(MemoryOsActionObservationRow {
                    observation_id: row.get(0)?,
                    source_kind: row.get(1)?,
                    cue_fingerprint: row.get(2)?,
                    action_fingerprint: row.get(3)?,
                    cue_json: row.get(4)?,
                    action_json: row.get(5)?,
                    source_ref: row.get(6)?,
                    observed_at: row.get(7)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        let mut execution_stmt = self.conn.prepare(
            "SELECT execution_id, execution_kind, command_sig, subject_ref, exit_code, observed_at
             FROM memory_os_action_executions
             WHERE (?1 IS NULL OR project_path = ?1 OR project_path GLOB ?2)
             ORDER BY observed_at ASC",
        )?;
        let executions = execution_stmt
            .query_map(params![project_exact, project_glob], |row| {
                Ok(MemoryOsActionExecutionRow {
                    execution_id: row.get(0)?,
                    execution_kind: row.get(1)?,
                    command_sig: row.get(2)?,
                    subject_ref: row.get(3)?,
                    exit_code: row.get(4)?,
                    observed_at: row.get(5)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        #[derive(Debug)]
        struct CandidateAccumulator {
            source_kind: String,
            cue: crate::core::memory_os::MemoryOsActionCue,
            action: crate::core::memory_os::MemoryOsAction,
            precedent_count: usize,
            success_count: usize,
            failure_count: usize,
            last_observed_at: String,
            last_executed_at: Option<String>,
            source_refs: Vec<String>,
        }

        let mut candidates: HashMap<(String, String), CandidateAccumulator> = HashMap::new();
        for row in &observations {
            let cue: crate::core::memory_os::MemoryOsActionCue =
                serde_json::from_str(&row.cue_json)?;
            let action: crate::core::memory_os::MemoryOsAction =
                serde_json::from_str(&row.action_json)?;
            let key = (row.cue_fingerprint.clone(), row.action_fingerprint.clone());
            let entry = candidates
                .entry(key)
                .or_insert_with(|| CandidateAccumulator {
                    source_kind: row.source_kind.clone(),
                    cue: cue.clone(),
                    action: action.clone(),
                    precedent_count: 0,
                    success_count: 0,
                    failure_count: 0,
                    last_observed_at: row.observed_at.clone(),
                    last_executed_at: None,
                    source_refs: Vec::new(),
                });
            entry.precedent_count += 1;
            if row.observed_at > entry.last_observed_at {
                entry.last_observed_at = row.observed_at.clone();
            }
            push_unique_string(&mut entry.source_refs, row.source_ref.clone());
            push_unique_string(&mut entry.source_refs, row.observation_id.clone());
        }

        let mut execution_indexes = vec![false; executions.len()];
        for row in &observations {
            let cue: crate::core::memory_os::MemoryOsActionCue =
                serde_json::from_str(&row.cue_json)?;
            let action: crate::core::memory_os::MemoryOsAction =
                serde_json::from_str(&row.action_json)?;
            let Some(command_sig) = action.command_sig.as_deref() else {
                continue;
            };
            let key = (row.cue_fingerprint.clone(), row.action_fingerprint.clone());
            let Some(candidate) = candidates.get_mut(&key) else {
                continue;
            };

            if let Some((index, execution)) =
                executions.iter().enumerate().find(|(index, execution)| {
                    !execution_indexes[*index]
                        && execution.execution_kind != "context-meta-command"
                        && canonical_memory_os_command_sig(&execution.command_sig)
                            == canonical_memory_os_command_sig(command_sig)
                        && match cue.trigger_subject.as_deref() {
                            Some(expected_subject) => {
                                execution.subject_ref.as_deref() == Some(expected_subject)
                            }
                            None => true,
                        }
                        && execution.observed_at >= row.observed_at
                })
            {
                execution_indexes[index] = true;
                if execution.exit_code == 0 {
                    candidate.success_count += 1;
                } else {
                    candidate.failure_count += 1;
                }
                candidate.last_executed_at = Some(execution.observed_at.clone());
                push_unique_string(&mut candidate.source_refs, execution.execution_id.clone());
            }
        }

        let mut result = candidates
            .into_iter()
            .map(|((cue_fingerprint, action_fingerprint), candidate)| {
                let status = action_candidate_status(
                    &candidate.source_kind,
                    &candidate.action,
                    candidate.precedent_count,
                    candidate.success_count,
                    candidate.failure_count,
                );
                let confidence = action_candidate_confidence(
                    status,
                    candidate.precedent_count,
                    candidate.success_count,
                    candidate.failure_count,
                );
                let title = action_candidate_title(&candidate.cue, &candidate.action);
                let summary = action_candidate_summary(
                    &candidate.cue,
                    &candidate.action,
                    candidate.precedent_count,
                    candidate.success_count,
                    candidate.failure_count,
                );
                let (review_after, expires_at, lifecycle_policy, aging_status) =
                    action_lifecycle_defaults(
                        &candidate.action.action_kind,
                        &candidate.last_observed_at,
                    );
                crate::core::memory_os::MemoryOsActionPolicyCandidate {
                    candidate_id: format!(
                        "action-candidate-{}",
                        hash_text(&format!("{cue_fingerprint}:{action_fingerprint}"))
                    ),
                    source_kind: candidate.source_kind,
                    actuator_type: candidate.action.action_kind.clone(),
                    autonomy_level: "suggest".to_string(),
                    status: status.to_string(),
                    title,
                    summary,
                    confidence: confidence.to_string(),
                    cue: candidate.cue,
                    action: candidate.action,
                    precedent_count: candidate.precedent_count,
                    success_count: candidate.success_count,
                    failure_count: candidate.failure_count,
                    last_observed_at: candidate.last_observed_at,
                    last_executed_at: candidate.last_executed_at,
                    review_after,
                    expires_at,
                    lifecycle_policy,
                    aging_status,
                    source_refs: candidate.source_refs,
                }
            })
            .collect::<Vec<_>>();
        result.sort_by(|left, right| {
            right
                .success_count
                .cmp(&left.success_count)
                .then(right.precedent_count.cmp(&left.precedent_count))
                .then(right.last_observed_at.cmp(&left.last_observed_at))
        });
        Ok(result)
    }
}

fn canonical_memory_os_command_sig(command: &str) -> String {
    match command.trim() {
        "context context" | "context context --format prompt" | "context resume" => {
            "munin resume --format prompt".to_string()
        }
        other => other.to_string(),
    }
}
