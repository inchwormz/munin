use anyhow::Result;
use chrono::{DateTime, Utc};
use rusqlite::params;
use serde::{Deserialize, Serialize};

use super::{parse_rfc3339_to_utc, project_filter_params, Tracker};

fn parse_approval_job_status(value: &str) -> ApprovalJobStatus {
    match value {
        "approved" => ApprovalJobStatus::Approved,
        "rejected" => ApprovalJobStatus::Rejected,
        "deferred" => ApprovalJobStatus::Deferred,
        "suppressed" => ApprovalJobStatus::Suppressed,
        "completed" => ApprovalJobStatus::Completed,
        "failed" => ApprovalJobStatus::Failed,
        _ => ApprovalJobStatus::Queued,
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ApprovalJobStatus {
    Queued,
    Approved,
    Rejected,
    Deferred,
    Suppressed,
    Completed,
    Failed,
}

impl std::fmt::Display for ApprovalJobStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let value = match self {
            Self::Queued => "queued",
            Self::Approved => "approved",
            Self::Rejected => "rejected",
            Self::Deferred => "deferred",
            Self::Suppressed => "suppressed",
            Self::Completed => "completed",
            Self::Failed => "failed",
        };
        write!(f, "{value}")
    }
}

#[derive(Debug, Clone)]
pub struct ApprovalJobRecord {
    pub job_id: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub project_path: String,
    pub scope: String,
    pub scope_target: Option<String>,
    pub local_date: String,
    pub item_id: Option<String>,
    pub item_kind: String,
    pub title: String,
    pub summary: String,
    pub status: ApprovalJobStatus,
    pub source_kind: String,
    pub provider: Option<String>,
    pub continuity_active: bool,
    pub expected_effect: Option<String>,
    pub queue_path: Option<String>,
    pub result_path: Option<String>,
    pub evidence_json: String,
    pub review_after: Option<String>,
    pub expires_at: Option<String>,
    pub last_reviewed_at: Option<String>,
    pub closure_reason: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ApprovalJobInput {
    pub job_id: String,
    pub scope: String,
    pub scope_target: Option<String>,
    pub local_date: String,
    pub item_id: Option<String>,
    pub item_kind: String,
    pub title: String,
    pub summary: String,
    pub status: ApprovalJobStatus,
    pub source_kind: String,
    pub provider: Option<String>,
    pub continuity_active: bool,
    pub expected_effect: Option<String>,
    pub queue_path: Option<String>,
    pub result_path: Option<String>,
    pub evidence_json: String,
    pub review_after: Option<String>,
    pub expires_at: Option<String>,
}

pub fn friction_fix_item_ids_related(left: Option<&str>, right: Option<&str>) -> bool {
    if left == right {
        return true;
    }
    let (Some(left), Some(right)) = (left, right) else {
        return false;
    };
    friction_fix_family(left)
        .zip(friction_fix_family(right))
        .is_some_and(|(left_family, right_family)| left_family == right_family)
}

fn friction_fix_family(item_id: &str) -> Option<&'static str> {
    match item_id {
        "friction:user-command-noise"
        | "friction:cli-syntax-drift"
        | "friction:path-assumption-drift"
        | "friction:execution-assumption-drift"
        | "friction:tool-availability-drift" => Some("command-friction"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_friction_item_ids_share_a_completion_family() {
        assert!(friction_fix_item_ids_related(
            Some("friction:user-command-noise"),
            Some("friction:path-assumption-drift")
        ));
        assert!(friction_fix_item_ids_related(
            Some("friction:cli-syntax-drift"),
            Some("friction:execution-assumption-drift")
        ));
        assert!(!friction_fix_item_ids_related(
            Some("friction:cli-syntax-drift"),
            Some("friction:autonomy-polling")
        ));
    }
}

impl Tracker {
    pub fn upsert_approval_job_for_project(
        &self,
        project_path: &str,
        input: &ApprovalJobInput,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT INTO approval_jobs (
                job_id, created_at, updated_at, project_path, scope, scope_target,
                local_date, item_id, item_kind, title, summary, status, source_kind,
                provider, continuity_active, expected_effect, queue_path, result_path,
                evidence_json, review_after, expires_at, last_reviewed_at, closure_reason
            ) VALUES (
                ?1, ?2, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15,
                ?16, ?17, ?18, ?19, ?20, ?21, NULL
            )
            ON CONFLICT(job_id) DO UPDATE SET
                updated_at = excluded.updated_at,
                project_path = excluded.project_path,
                scope = excluded.scope,
                scope_target = excluded.scope_target,
                local_date = excluded.local_date,
                item_id = excluded.item_id,
                item_kind = excluded.item_kind,
                title = excluded.title,
                summary = excluded.summary,
                status = excluded.status,
                source_kind = excluded.source_kind,
                provider = excluded.provider,
                continuity_active = excluded.continuity_active,
                expected_effect = excluded.expected_effect,
                queue_path = COALESCE(excluded.queue_path, approval_jobs.queue_path),
                result_path = COALESCE(excluded.result_path, approval_jobs.result_path),
                evidence_json = excluded.evidence_json,
                review_after = COALESCE(excluded.review_after, approval_jobs.review_after),
                expires_at = COALESCE(excluded.expires_at, approval_jobs.expires_at)",
            params![
                input.job_id,
                now,
                project_path,
                input.scope,
                input.scope_target,
                input.local_date,
                input.item_id,
                input.item_kind,
                input.title,
                input.summary,
                input.status.to_string(),
                input.source_kind,
                input.provider,
                input.continuity_active,
                input.expected_effect,
                input.queue_path,
                input.result_path,
                input.evidence_json,
                input.review_after,
                input.expires_at,
                now,
            ],
        )?;
        Ok(())
    }

    pub fn get_approval_job(&self, job_id: &str) -> Result<Option<ApprovalJobRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT job_id, created_at, updated_at, project_path, scope, scope_target,
                    local_date, item_id, item_kind, title, summary, status, source_kind,
                    provider, continuity_active, expected_effect, queue_path, result_path,
                    evidence_json, review_after, expires_at, last_reviewed_at, closure_reason
             FROM approval_jobs
             WHERE job_id = ?1
             LIMIT 1",
        )?;
        let mut rows = stmt.query(params![job_id])?;
        let Some(row) = rows.next()? else {
            return Ok(None);
        };
        Ok(Some(ApprovalJobRecord {
            job_id: row.get(0)?,
            created_at: parse_rfc3339_to_utc(&row.get::<_, String>(1)?),
            updated_at: parse_rfc3339_to_utc(&row.get::<_, String>(2)?),
            project_path: row.get(3)?,
            scope: row.get(4)?,
            scope_target: row.get(5)?,
            local_date: row.get(6)?,
            item_id: row.get(7)?,
            item_kind: row.get(8)?,
            title: row.get(9)?,
            summary: row.get(10)?,
            status: parse_approval_job_status(&row.get::<_, String>(11)?),
            source_kind: row.get(12)?,
            provider: row.get(13)?,
            continuity_active: row.get::<_, i64>(14)? != 0,
            expected_effect: row.get(15)?,
            queue_path: row.get(16)?,
            result_path: row.get(17)?,
            evidence_json: row.get(18)?,
            review_after: row.get(19)?,
            expires_at: row.get(20)?,
            last_reviewed_at: row.get(21)?,
            closure_reason: row.get(22)?,
        }))
    }

    pub fn set_approval_job_status(
        &self,
        job_id: &str,
        status: ApprovalJobStatus,
        queue_path: Option<&str>,
        result_path: Option<&str>,
        closure_reason: Option<&str>,
    ) -> Result<bool> {
        let updated = self.conn.execute(
            "UPDATE approval_jobs
             SET status = ?1,
                 updated_at = ?2,
                 queue_path = COALESCE(?3, queue_path),
                 result_path = COALESCE(?4, result_path),
                 last_reviewed_at = ?2,
                 closure_reason = COALESCE(?5, closure_reason)
             WHERE job_id = ?6",
            params![
                status.to_string(),
                Utc::now().to_rfc3339(),
                queue_path,
                result_path,
                closure_reason,
                job_id,
            ],
        )?;
        Ok(updated > 0)
    }

    pub fn get_approval_jobs_filtered(
        &self,
        limit: usize,
        project_path: Option<&str>,
        statuses: Option<&[ApprovalJobStatus]>,
    ) -> Result<Vec<ApprovalJobRecord>> {
        let (project_exact, project_glob) = project_filter_params(project_path);
        let status_values = statuses
            .map(|values| values.iter().map(ToString::to_string).collect::<Vec<_>>())
            .unwrap_or_default();
        let query = match status_values.as_slice() {
            [] => {
                "SELECT job_id, created_at, updated_at, project_path, scope, scope_target,
                        local_date, item_id, item_kind, title, summary, status, source_kind,
                        provider, continuity_active, expected_effect, queue_path, result_path,
                        evidence_json, review_after, expires_at, last_reviewed_at, closure_reason
                 FROM approval_jobs
                 WHERE (?1 IS NULL OR project_path = ?1 OR project_path GLOB ?2)
                 ORDER BY updated_at DESC
                 LIMIT ?3"
            }
            [_] => {
                "SELECT job_id, created_at, updated_at, project_path, scope, scope_target,
                        local_date, item_id, item_kind, title, summary, status, source_kind,
                        provider, continuity_active, expected_effect, queue_path, result_path,
                        evidence_json, review_after, expires_at, last_reviewed_at, closure_reason
                 FROM approval_jobs
                 WHERE (?1 IS NULL OR project_path = ?1 OR project_path GLOB ?2)
                   AND status = ?3
                 ORDER BY updated_at DESC
                 LIMIT ?4"
            }
            _ => {
                "SELECT job_id, created_at, updated_at, project_path, scope, scope_target,
                        local_date, item_id, item_kind, title, summary, status, source_kind,
                        provider, continuity_active, expected_effect, queue_path, result_path,
                        evidence_json, review_after, expires_at, last_reviewed_at, closure_reason
                 FROM approval_jobs
                 WHERE (?1 IS NULL OR project_path = ?1 OR project_path GLOB ?2)
                   AND status IN (?3, ?4, ?5, ?6)
                 ORDER BY updated_at DESC
                 LIMIT ?7"
            }
        };
        let mut stmt = self.conn.prepare(query)?;
        let map_row = |row: &rusqlite::Row<'_>| {
            Ok(ApprovalJobRecord {
                job_id: row.get(0)?,
                created_at: parse_rfc3339_to_utc(&row.get::<_, String>(1)?),
                updated_at: parse_rfc3339_to_utc(&row.get::<_, String>(2)?),
                project_path: row.get(3)?,
                scope: row.get(4)?,
                scope_target: row.get(5)?,
                local_date: row.get(6)?,
                item_id: row.get(7)?,
                item_kind: row.get(8)?,
                title: row.get(9)?,
                summary: row.get(10)?,
                status: parse_approval_job_status(&row.get::<_, String>(11)?),
                source_kind: row.get(12)?,
                provider: row.get(13)?,
                continuity_active: row.get::<_, i64>(14)? != 0,
                expected_effect: row.get(15)?,
                queue_path: row.get(16)?,
                result_path: row.get(17)?,
                evidence_json: row.get(18)?,
                review_after: row.get(19)?,
                expires_at: row.get(20)?,
                last_reviewed_at: row.get(21)?,
                closure_reason: row.get(22)?,
            })
        };

        let rows = match status_values.as_slice() {
            [] => stmt.query_map(params![project_exact, project_glob, limit as i64], map_row)?,
            [status_one] => stmt.query_map(
                params![project_exact, project_glob, status_one, limit as i64],
                map_row,
            )?,
            [status_one, status_two, status_three, status_four, ..] => stmt.query_map(
                params![
                    project_exact,
                    project_glob,
                    status_one,
                    status_two,
                    status_three,
                    status_four,
                    limit as i64
                ],
                map_row,
            )?,
            values => {
                let mut padded = values.to_vec();
                while padded.len() < 4 {
                    padded.push(values.last().cloned().unwrap_or_default());
                }
                stmt.query_map(
                    params![
                        project_exact,
                        project_glob,
                        padded[0].clone(),
                        padded[1].clone(),
                        padded[2].clone(),
                        padded[3].clone(),
                        limit as i64
                    ],
                    map_row,
                )?
            }
        };

        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }
}
