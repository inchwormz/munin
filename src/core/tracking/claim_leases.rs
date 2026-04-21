use anyhow::{anyhow, Result};
use chrono::{DateTime, Utc};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{
    current_project_path_string, hash_text, project_filter_params, MemoryOsShadowEvent, Tracker,
};

fn hash_claim_dependencies(dependencies: &[ClaimLeaseDependency]) -> String {
    let canonical = serde_json::to_string(dependencies).unwrap_or_default();
    let mut hasher = Sha256::new();
    hasher.update(canonical.as_bytes());
    format!("{:x}", hasher.finalize())
}

pub(super) fn normalize_claim_text(text: &str) -> String {
    let lowered = text.to_ascii_lowercase();
    let mut normalized = String::with_capacity(lowered.len());
    let mut last_was_sep = false;
    for ch in lowered.chars() {
        if ch.is_ascii_alphanumeric() {
            normalized.push(ch);
            last_was_sep = false;
        } else if !last_was_sep {
            normalized.push(' ');
            last_was_sep = true;
        }
    }
    normalized.trim().to_string()
}

fn parse_claim_lease_type(value: &str) -> ClaimLeaseType {
    match value {
        "decision" => ClaimLeaseType::Decision,
        "rejection" => ClaimLeaseType::Rejection,
        "hypothesis-tested" => ClaimLeaseType::HypothesisTested,
        "obligation" => ClaimLeaseType::Obligation,
        "benign-anomaly" => ClaimLeaseType::BenignAnomaly,
        _ => ClaimLeaseType::Decision,
    }
}

fn parse_claim_lease_confidence(value: &str) -> ClaimLeaseConfidence {
    match value {
        "low" => ClaimLeaseConfidence::Low,
        "high" => ClaimLeaseConfidence::High,
        _ => ClaimLeaseConfidence::Medium,
    }
}

fn parse_claim_lease_status(value: &str) -> ClaimLeaseStatus {
    match value {
        "stale" => ClaimLeaseStatus::Stale,
        "superseded" => ClaimLeaseStatus::Superseded,
        _ => ClaimLeaseStatus::Live,
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ClaimLeaseType {
    Decision,
    Rejection,
    HypothesisTested,
    Obligation,
    BenignAnomaly,
}

impl std::fmt::Display for ClaimLeaseType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let value = match self {
            Self::Decision => "decision",
            Self::Rejection => "rejection",
            Self::HypothesisTested => "hypothesis-tested",
            Self::Obligation => "obligation",
            Self::BenignAnomaly => "benign-anomaly",
        };
        write!(f, "{value}")
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ClaimLeaseConfidence {
    Low,
    Medium,
    High,
}

impl std::fmt::Display for ClaimLeaseConfidence {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let value = match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        };
        write!(f, "{value}")
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ClaimLeaseStatus {
    Live,
    Stale,
    Superseded,
}

impl std::fmt::Display for ClaimLeaseStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let value = match self {
            Self::Live => "live",
            Self::Stale => "stale",
            Self::Superseded => "superseded",
        };
        write!(f, "{value}")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ClaimLeaseDependencyKind {
    WorldviewSubject,
    Artifact,
    UserDecision,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ClaimLeaseDependency {
    pub kind: ClaimLeaseDependencyKind,
    pub key: String,
    pub fingerprint: Option<String>,
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct ClaimLeaseRecord {
    pub id: i64,
    pub timestamp: DateTime<Utc>,
    pub project_path: String,
    pub claim_type: ClaimLeaseType,
    pub claim_text: String,
    pub rationale_capsule: Option<String>,
    pub confidence: ClaimLeaseConfidence,
    pub status: ClaimLeaseStatus,
    pub scope_key: Option<String>,
    pub dependencies: Vec<ClaimLeaseDependency>,
    pub dependency_fingerprint: String,
    pub evidence_json: String,
    pub source_kind: String,
    pub review_after: Option<String>,
    pub expires_at: Option<String>,
    pub last_reviewed_at: Option<String>,
    pub demotion_reason: Option<String>,
}

#[derive(Debug, Clone)]
pub struct UserDecisionRecord {
    pub key: String,
    pub value_text: String,
    pub fingerprint: String,
    pub updated_at: DateTime<Utc>,
}

impl Tracker {
    pub fn create_claim_lease(
        &self,
        claim_type: ClaimLeaseType,
        claim_text: &str,
        rationale_capsule: Option<&str>,
        confidence: ClaimLeaseConfidence,
        scope_key: Option<&str>,
        dependencies: &[ClaimLeaseDependency],
        evidence_json: &str,
        source_kind: &str,
    ) -> Result<i64> {
        let project_path = current_project_path_string();
        self.create_claim_lease_for_project(
            &project_path,
            claim_type,
            claim_text,
            rationale_capsule,
            confidence,
            scope_key,
            dependencies,
            evidence_json,
            source_kind,
        )
    }

    pub(crate) fn create_claim_lease_for_project(
        &self,
        project_path: &str,
        claim_type: ClaimLeaseType,
        claim_text: &str,
        rationale_capsule: Option<&str>,
        confidence: ClaimLeaseConfidence,
        scope_key: Option<&str>,
        dependencies: &[ClaimLeaseDependency],
        evidence_json: &str,
        source_kind: &str,
    ) -> Result<i64> {
        if dependencies.is_empty() {
            return Err(anyhow!(
                "claim leases require at least one explicit dependency"
            ));
        }

        let resolved_dependencies = self.resolve_claim_dependencies(project_path, dependencies)?;
        let dependency_fingerprint = hash_claim_dependencies(&resolved_dependencies);
        let dependencies_json = serde_json::to_string(&resolved_dependencies)?;

        self.conn.execute(
            "INSERT INTO claim_leases (
                timestamp, project_path, claim_type, claim_text, rationale_capsule,
                confidence, status, scope_key, dependencies_json, dependency_fingerprint,
                evidence_json, source_kind
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                Utc::now().to_rfc3339(),
                project_path,
                claim_type.to_string(),
                claim_text,
                rationale_capsule,
                confidence.to_string(),
                ClaimLeaseStatus::Live.to_string(),
                scope_key,
                dependencies_json,
                dependency_fingerprint,
                evidence_json,
                source_kind,
            ],
        )?;
        let claim_id = self.conn.last_insert_rowid();
        let _ = self.record_memory_os_shadow_event(MemoryOsShadowEvent {
            event_id: format!("legacy-claim-{claim_id}"),
            stream_id: format!("legacy.claim:{}", claim_id),
            stream_revision: 0,
            expected_stream_revision: None,
            tx_index: 0,
            event_kind: "legacy.claim-lease-created".into(),
            idempotency_key: format!("legacy.claim:rowid:{claim_id}"),
            idempotency_receipt_id: None,
            project_path: project_path.to_string(),
            scope_json: serde_json::json!({
                "repo_id": project_path,
                "branch_id": "",
                "worktree_id": scope_key.unwrap_or(""),
                "task_id": serde_json::Value::Null,
                "objective_id": serde_json::Value::Null,
                "session_id": serde_json::Value::Null,
                "agent_id": serde_json::Value::Null,
                "runtime_profile": "munin-runtime",
                "os_profile": std::env::consts::OS,
                "valid_from": Utc::now().to_rfc3339(),
                "valid_until": serde_json::Value::Null
            })
            .to_string(),
            actor_json: serde_json::json!({
                "actor_id": "munin",
                "actor_kind": "system",
                "origin_agent_id": serde_json::Value::Null,
                "trust_domain": "local_core"
            })
            .to_string(),
            target_refs_json: serde_json::json!([claim_id]).to_string(),
            payload_json: serde_json::json!({
                "claim_type": claim_type.to_string(),
                "claim_text": claim_text,
                "rationale_capsule": rationale_capsule,
                "confidence": confidence.to_string(),
                "scope_key": scope_key,
                "dependencies_json": dependencies_json,
                "dependency_fingerprint": dependency_fingerprint,
                "evidence_json": evidence_json,
                "source_kind": source_kind
            })
            .to_string(),
            proof_refs_json: "[]".to_string(),
            precondition_hash: None,
            result_hash: Some(hash_text(claim_text)),
            schema_fingerprint: "memoryos-shadow-v1".into(),
        });
        let trust_payload = format!("{claim_text}\n{evidence_json}");
        let _ = self.observe_memory_os_payload_for_project(
            project_path,
            "claim-lease",
            &claim_id.to_string(),
            "promote",
            &trust_payload,
        );

        self.cleanup_old_if_due()?;
        Ok(claim_id)
    }

    pub fn supersede_claim_lease_for_project(&self, project_path: &str, id: i64) -> Result<bool> {
        let updated = self.conn.execute(
            "UPDATE claim_leases
             SET status = ?1
             WHERE id = ?2 AND project_path = ?3",
            params![ClaimLeaseStatus::Superseded.to_string(), id, project_path],
        )?;
        Ok(updated > 0)
    }

    pub fn set_user_decision(&self, decision_key: &str, value_text: &str) -> Result<i64> {
        let project_path = current_project_path_string();
        self.set_user_decision_for_project(&project_path, decision_key, value_text)
    }

    pub(crate) fn set_user_decision_for_project(
        &self,
        project_path: &str,
        decision_key: &str,
        value_text: &str,
    ) -> Result<i64> {
        let fingerprint = hash_text(value_text);
        self.conn.execute(
            "INSERT INTO user_decisions (timestamp, project_path, decision_key, value_text, fingerprint)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                Utc::now().to_rfc3339(),
                project_path,
                decision_key,
                value_text,
                fingerprint,
            ],
        )?;
        self.cleanup_old_if_due()?;
        Ok(self.conn.last_insert_rowid())
    }

    fn resolve_claim_dependencies(
        &self,
        project_path: &str,
        dependencies: &[ClaimLeaseDependency],
    ) -> Result<Vec<ClaimLeaseDependency>> {
        let mut resolved = dependencies.to_vec();
        for dependency in &mut resolved {
            if dependency.key.trim().is_empty() {
                return Err(anyhow!("claim lease dependency keys cannot be empty"));
            }
            match dependency.kind {
                ClaimLeaseDependencyKind::WorldviewSubject => {
                    dependency.fingerprint = Some(
                        self.latest_worldview_fingerprint_for_project(
                            project_path,
                            &dependency.key,
                        )?
                        .ok_or_else(|| {
                            anyhow!(
                                "no worldview fact found for dependency '{}'",
                                dependency.key
                            )
                        })?,
                    );
                }
                ClaimLeaseDependencyKind::Artifact => {
                    dependency.fingerprint = Some(
                        self.resolve_artifact_dependency_fingerprint(&dependency.key)?
                            .ok_or_else(|| {
                                anyhow!(
                                    "artifact dependency '{}' could not be resolved",
                                    dependency.key
                                )
                            })?,
                    );
                }
                ClaimLeaseDependencyKind::UserDecision => {
                    dependency.fingerprint = Some(
                        self.latest_user_decision_fingerprint_for_project(
                            project_path,
                            &dependency.key,
                        )?
                        .ok_or_else(|| {
                            anyhow!("no user decision found for dependency '{}'", dependency.key)
                        })?,
                    );
                }
            }
        }

        resolved.sort_by(|left, right| {
            format!("{:?}:{}", left.kind, left.key).cmp(&format!("{:?}:{}", right.kind, right.key))
        });
        Ok(resolved)
    }

    fn latest_worldview_fingerprint_for_project(
        &self,
        project_path: &str,
        subject_key: &str,
    ) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT fingerprint
                 FROM worldview_events
                 WHERE project_path = ?1 AND subject_key = ?2
                 ORDER BY timestamp DESC
                 LIMIT 1",
                params![project_path, subject_key],
                |row| row.get(0),
            )
            .ok())
    }

    fn latest_user_decision_fingerprint_for_project(
        &self,
        project_path: &str,
        decision_key: &str,
    ) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT fingerprint
                 FROM user_decisions
                 WHERE project_path = ?1 AND decision_key = ?2
                 ORDER BY timestamp DESC
                 LIMIT 1",
                params![project_path, decision_key],
                |row| row.get(0),
            )
            .ok())
    }

    fn resolve_artifact_dependency_fingerprint(&self, artifact_id: &str) -> Result<Option<String>> {
        if cfg!(test) {
            return Ok(Some(artifact_id.to_string()));
        }
        if crate::core::artifacts::load_artifact_text(artifact_id).is_ok() {
            Ok(Some(artifact_id.to_string()))
        } else {
            Ok(None)
        }
    }

    pub fn refresh_claim_lease_statuses(&self, project_path: Option<&str>) -> Result<()> {
        let (project_exact, project_glob) = project_filter_params(project_path);
        let now = Utc::now();
        let mut stmt = self.conn.prepare(
            "SELECT id, project_path, dependencies_json, dependency_fingerprint, status, review_after, expires_at
             FROM claim_leases
             WHERE (?1 IS NULL OR project_path = ?1 OR project_path GLOB ?2)",
        )?;

        let rows = stmt.query_map(params![project_exact, project_glob], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, Option<String>>(6)?,
            ))
        })?;

        for row in rows {
            let (
                id,
                row_project_path,
                dependencies_json,
                dependency_fingerprint,
                status,
                review_after,
                expires_at,
            ) = row?;
            let current_status = parse_claim_lease_status(&status);
            if matches!(
                current_status,
                ClaimLeaseStatus::Superseded | ClaimLeaseStatus::Stale
            ) {
                continue;
            }

            let dependencies: Vec<ClaimLeaseDependency> =
                serde_json::from_str(&dependencies_json).unwrap_or_default();
            let current_fingerprint =
                self.current_claim_dependency_fingerprint(&row_project_path, &dependencies)?;
            let expired = expires_at
                .as_deref()
                .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
                .map(|value| value.with_timezone(&Utc) <= now)
                .unwrap_or(false);
            let review_overdue = review_after
                .as_deref()
                .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
                .map(|value| value.with_timezone(&Utc) <= now)
                .unwrap_or(false);
            let (next_status, demotion_reason) = if expired {
                (ClaimLeaseStatus::Stale, Some("expired".to_string()))
            } else if review_overdue {
                (ClaimLeaseStatus::Stale, Some("review_due".to_string()))
            } else if current_fingerprint == dependency_fingerprint {
                (ClaimLeaseStatus::Live, None)
            } else {
                (
                    ClaimLeaseStatus::Stale,
                    Some("dependency_drift".to_string()),
                )
            };

            if next_status != current_status {
                self.conn.execute(
                    "UPDATE claim_leases
                     SET status = ?1,
                         last_reviewed_at = ?2,
                         demotion_reason = ?3
                     WHERE id = ?4",
                    params![
                        next_status.to_string(),
                        now.to_rfc3339(),
                        demotion_reason,
                        id
                    ],
                )?;
            } else {
                self.conn.execute(
                    "UPDATE claim_leases SET last_reviewed_at = ?1 WHERE id = ?2",
                    params![now.to_rfc3339(), id],
                )?;
            }
        }

        Ok(())
    }

    fn current_claim_dependency_fingerprint(
        &self,
        project_path: &str,
        dependencies: &[ClaimLeaseDependency],
    ) -> Result<String> {
        let mut current = Vec::with_capacity(dependencies.len());
        for dependency in dependencies {
            let fingerprint = match dependency.kind {
                ClaimLeaseDependencyKind::WorldviewSubject => {
                    self.latest_worldview_fingerprint_for_project(project_path, &dependency.key)?
                }
                ClaimLeaseDependencyKind::Artifact => {
                    self.resolve_artifact_dependency_fingerprint(&dependency.key)?
                }
                ClaimLeaseDependencyKind::UserDecision => self
                    .latest_user_decision_fingerprint_for_project(project_path, &dependency.key)?,
            };
            current.push(ClaimLeaseDependency {
                kind: dependency.kind.clone(),
                key: dependency.key.clone(),
                fingerprint,
            });
        }
        current.sort_by(|left, right| {
            format!("{:?}:{}", left.kind, left.key).cmp(&format!("{:?}:{}", right.kind, right.key))
        });
        Ok(hash_claim_dependencies(&current))
    }

    pub fn get_claim_leases_filtered(
        &self,
        limit: usize,
        project_path: Option<&str>,
        statuses: Option<&[ClaimLeaseStatus]>,
    ) -> Result<Vec<ClaimLeaseRecord>> {
        let (project_exact, project_glob) = project_filter_params(project_path);
        let status_values = statuses
            .map(|values| values.iter().map(ToString::to_string).collect::<Vec<_>>())
            .unwrap_or_default();

        let query = match status_values.as_slice() {
            [] => {
                "SELECT id, timestamp, project_path, claim_type, claim_text, rationale_capsule, confidence, status,
                        scope_key, dependencies_json, dependency_fingerprint, evidence_json, source_kind,
                        review_after, expires_at, last_reviewed_at, demotion_reason
                 FROM claim_leases
                 WHERE (?1 IS NULL OR project_path = ?1 OR project_path GLOB ?2)
                 ORDER BY timestamp DESC
                 LIMIT ?3"
            }
            [_] => {
                "SELECT id, timestamp, project_path, claim_type, claim_text, rationale_capsule, confidence, status,
                        scope_key, dependencies_json, dependency_fingerprint, evidence_json, source_kind,
                        review_after, expires_at, last_reviewed_at, demotion_reason
                 FROM claim_leases
                 WHERE (?1 IS NULL OR project_path = ?1 OR project_path GLOB ?2)
                   AND status = ?3
                 ORDER BY timestamp DESC
                 LIMIT ?4"
            }
            [_, _] => {
                "SELECT id, timestamp, project_path, claim_type, claim_text, rationale_capsule, confidence, status,
                        scope_key, dependencies_json, dependency_fingerprint, evidence_json, source_kind,
                        review_after, expires_at, last_reviewed_at, demotion_reason
                 FROM claim_leases
                 WHERE (?1 IS NULL OR project_path = ?1 OR project_path GLOB ?2)
                   AND status IN (?3, ?4)
                 ORDER BY timestamp DESC
                 LIMIT ?5"
            }
            _ => {
                "SELECT id, timestamp, project_path, claim_type, claim_text, rationale_capsule, confidence, status,
                        scope_key, dependencies_json, dependency_fingerprint, evidence_json, source_kind,
                        review_after, expires_at, last_reviewed_at, demotion_reason
                 FROM claim_leases
                 WHERE (?1 IS NULL OR project_path = ?1 OR project_path GLOB ?2)
                   AND status IN (?3, ?4, ?5)
                 ORDER BY timestamp DESC
                 LIMIT ?6"
            }
        };

        let mut stmt = self.conn.prepare(query)?;
        let map_row = |row: &rusqlite::Row<'_>| {
            Ok(ClaimLeaseRecord {
                id: row.get(0)?,
                timestamp: DateTime::parse_from_rfc3339(&row.get::<_, String>(1)?)
                    .map(|dt| dt.with_timezone(&Utc))
                    .unwrap_or_else(|_| Utc::now()),
                project_path: row.get(2)?,
                claim_type: parse_claim_lease_type(&row.get::<_, String>(3)?),
                claim_text: row.get(4)?,
                rationale_capsule: row.get(5)?,
                confidence: parse_claim_lease_confidence(&row.get::<_, String>(6)?),
                status: parse_claim_lease_status(&row.get::<_, String>(7)?),
                scope_key: row.get(8)?,
                dependencies: serde_json::from_str(&row.get::<_, String>(9)?).unwrap_or_default(),
                dependency_fingerprint: row.get(10)?,
                evidence_json: row.get(11)?,
                source_kind: row.get(12)?,
                review_after: row.get(13)?,
                expires_at: row.get(14)?,
                last_reviewed_at: row.get(15)?,
                demotion_reason: row.get(16)?,
            })
        };

        let rows = match status_values.as_slice() {
            [] => stmt.query_map(params![project_exact, project_glob, limit as i64], map_row)?,
            [status_one] => stmt.query_map(
                params![project_exact, project_glob, status_one, limit as i64],
                map_row,
            )?,
            [status_one, status_two] => stmt.query_map(
                params![
                    project_exact,
                    project_glob,
                    status_one,
                    status_two,
                    limit as i64
                ],
                map_row,
            )?,
            [status_one, status_two, status_three, ..] => stmt.query_map(
                params![
                    project_exact,
                    project_glob,
                    status_one,
                    status_two,
                    status_three,
                    limit as i64
                ],
                map_row,
            )?,
        };

        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn get_user_decisions_filtered(
        &self,
        limit: usize,
        project_path: Option<&str>,
    ) -> Result<Vec<UserDecisionRecord>> {
        let (project_exact, project_glob) = project_filter_params(project_path);
        let mut stmt = self.conn.prepare(
            "SELECT decision_key, value_text, fingerprint, timestamp
             FROM user_decisions
             WHERE (?1 IS NULL OR project_path = ?1 OR project_path GLOB ?2)
             ORDER BY timestamp DESC",
        )?;

        let rows = stmt.query_map(params![project_exact, project_glob], |row| {
            Ok(UserDecisionRecord {
                key: row.get(0)?,
                value_text: row.get(1)?,
                fingerprint: row.get(2)?,
                updated_at: DateTime::parse_from_rfc3339(&row.get::<_, String>(3)?)
                    .map(|dt| dt.with_timezone(&Utc))
                    .unwrap_or_else(|_| Utc::now()),
            })
        })?;

        let mut latest_by_key = std::collections::HashSet::new();
        let mut latest = Vec::new();
        for row in rows {
            let decision = row?;
            if latest_by_key.insert(decision.key.clone()) {
                latest.push(decision);
                if latest.len() >= limit {
                    break;
                }
            }
        }

        Ok(latest)
    }
}
