use anyhow::Result;
use chrono::Utc;
use rusqlite::params;
use serde_json::Value;
use std::collections::HashSet;

use super::{resolved_project_path, Tracker};
use crate::core::utils::truncate;

const SESSION_SUMMARY_RECALL_MAX_CHARS: usize = 1600;

impl Tracker {
    pub fn get_memory_os_recall_report(
        &self,
        scope: crate::core::memory_os::MemoryOsInspectionScope,
        project_path: Option<&str>,
        query: &str,
    ) -> Result<crate::core::memory_os::MemoryOsRecallReport> {
        let tokens = query_tokens(query);
        let checkpoints = self.load_memory_os_checkpoint_captures(scope, project_path)?;
        let resolved_project = project_path.map(|path| resolved_project_path(Some(path)));
        let mut matches = Vec::new();

        for checkpoint in checkpoints {
            let project_bonus = resolved_project
                .as_deref()
                .map(|project| project == checkpoint.project_path)
                .unwrap_or(false);
            let source_ref = format!("checkpoint:{}", checkpoint.capture.packet_id);
            let mut candidates = Vec::new();
            if let Some(goal) = checkpoint.capture.goal.as_deref() {
                candidates.push((
                    "goal".to_string(),
                    goal.to_string(),
                    vec![format!(
                        "checkpoint goal captured at {}",
                        checkpoint.capture.generated_at
                    )],
                    12,
                ));
            }
            if let Some(recommendation) =
                checkpoint.capture.reentry.current_recommendation.as_deref()
            {
                candidates.push((
                    "reentry".to_string(),
                    recommendation.to_string(),
                    vec![format!(
                        "recommended command: {}",
                        checkpoint.capture.reentry.recommended_command
                    )],
                    10,
                ));
            }
            candidates.push((
                "question".to_string(),
                checkpoint.capture.reentry.first_question.clone(),
                vec![format!(
                    "first verification: {}",
                    checkpoint.capture.reentry.first_verification
                )],
                8,
            ));
            for item in &checkpoint.capture.selected_items {
                candidates.push((
                    item.kind.clone(),
                    item.summary.clone(),
                    item.provenance.clone(),
                    item.score / 100,
                ));
                if let Some(subject) = item.subject.as_deref() {
                    candidates.push((
                        "subject".to_string(),
                        subject.to_string(),
                        item.provenance.clone(),
                        item.score / 120,
                    ));
                }
            }

            for (source_kind, text, evidence, base_score) in candidates {
                let overlap = token_overlap(&tokens, &text);
                if overlap == 0 {
                    continue;
                }
                let project_score = if project_bonus { 8 } else { 0 };
                let recency_score =
                    if checkpoint.captured_at > Utc::now() - chrono::Duration::days(14) {
                        4
                    } else {
                        0
                    };
                let score = base_score + (overlap as i64 * 12) + project_score + recency_score;
                let max_answer_chars = if source_kind == "session-summary" {
                    SESSION_SUMMARY_RECALL_MAX_CHARS
                } else {
                    360
                };
                let answer = truncate(text.trim(), max_answer_chars);
                let title = title_from_text(&answer);
                matches.push(crate::core::memory_os::MemoryOsRecallMatch {
                    title,
                    answer,
                    score,
                    source_kind,
                    source_ref: source_ref.clone(),
                    project_path: checkpoint.project_path.clone(),
                    evidence: evidence
                        .into_iter()
                        .map(|item| truncate(item.trim(), 180))
                        .collect(),
                });
            }
        }

        let (project_exact, project_glob) = if let Some(project) = resolved_project.as_deref() {
            (Some(project.to_string()), Some(format!("{project}*")))
        } else {
            (None, None)
        };
        let mut stmt = self.conn.prepare(
            "SELECT project_path, committed_at, event_kind, payload_json
             FROM memory_os_journal_events
             WHERE (?1 IS NULL OR project_path = ?1 OR project_path GLOB ?2)
             ORDER BY committed_at DESC, journal_seq DESC
             LIMIT 200",
        )?;
        let rows = stmt.query_map(params![project_exact, project_glob], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?;
        for row in rows {
            let (project_path, committed_at, event_kind, payload_json) = row?;
            let value = serde_json::from_str::<Value>(&payload_json).unwrap_or(Value::Null);
            let mut snippets = Vec::new();
            collect_json_strings(&value, &mut snippets);
            for snippet in snippets.into_iter().take(12) {
                let overlap = token_overlap(&tokens, &snippet);
                if overlap == 0 {
                    continue;
                }
                let project_score = resolved_project
                    .as_deref()
                    .map(|project| project == project_path)
                    .unwrap_or(false) as i64
                    * 8;
                let answer = truncate(snippet.trim(), 360);
                matches.push(crate::core::memory_os::MemoryOsRecallMatch {
                    title: title_from_text(&answer),
                    answer,
                    score: 6 + (overlap as i64 * 10) + project_score,
                    source_kind: event_kind.clone(),
                    source_ref: format!("journal:{committed_at}"),
                    project_path: project_path.clone(),
                    evidence: vec![format!("journal event kind: {event_kind}")],
                });
            }
        }

        matches.sort_by(|left, right| {
            right
                .score
                .cmp(&left.score)
                .then(left.title.cmp(&right.title))
                .then(right.source_ref.cmp(&left.source_ref))
        });
        matches.dedup_by(|left, right| {
            left.answer == right.answer && left.source_ref == right.source_ref
        });
        let mut seen_session_summaries = HashSet::new();
        matches.retain(|item| {
            if item.source_kind != "session-summary" {
                return true;
            }
            let Some(session_id) = item
                .evidence
                .iter()
                .find_map(|line| line.strip_prefix("session-id:"))
            else {
                return true;
            };
            seen_session_summaries.insert(format!("{}:{}", item.project_path, session_id))
        });
        matches.truncate(5);
        let no_match_reason = if matches.is_empty() {
            Some("No compiled Memory OS evidence matched the query; raw overview fallback was intentionally not used.".to_string())
        } else {
            None
        };
        Ok(crate::core::memory_os::MemoryOsRecallReport {
            generated_at: Utc::now().to_rfc3339(),
            scope,
            query: query.trim().to_string(),
            matches,
            no_match_reason,
        })
    }
}

fn query_tokens(query: &str) -> Vec<String> {
    query
        .split(|ch: char| !ch.is_alphanumeric())
        .map(|part| part.trim().to_lowercase())
        .filter(|part| part.len() > 2)
        .collect()
}

fn token_overlap(tokens: &[String], text: &str) -> usize {
    if tokens.is_empty() {
        return 0;
    }
    let lowered = text.to_lowercase();
    tokens
        .iter()
        .filter(|token| lowered.contains(token.as_str()))
        .count()
}

fn title_from_text(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.len() <= 80 {
        return trimmed.to_string();
    }
    format!("{}...", trimmed.chars().take(77).collect::<String>())
}

fn collect_json_strings(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::String(text) => {
            let trimmed = text.trim();
            if trimmed.len() > 12 {
                out.push(trimmed.to_string());
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_json_strings(item, out);
            }
        }
        Value::Object(map) => {
            for value in map.values() {
                collect_json_strings(value, out);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::memory_os::{
        MemoryOsCheckpointCapture, MemoryOsCheckpointReentry, MemoryOsCheckpointTelemetry,
        MemoryOsInspectionScope, MemoryOsPacketSelection,
    };
    use rusqlite::params;

    fn capture(summary: &str) -> MemoryOsCheckpointCapture {
        MemoryOsCheckpointCapture {
            packet_id: "packet-1".to_string(),
            generated_at: "2026-04-18T00:00:00Z".to_string(),
            preset: "continue".to_string(),
            intent: "continue".to_string(),
            profile: "compact".to_string(),
            goal: Some("Ship Munin resolver after compiler truth".to_string()),
            budget: 1600,
            estimated_tokens: 200,
            estimated_source_tokens: 400,
            pager_manifest_hash: "hash".to_string(),
            recall_mode: "off".to_string(),
            recall_used: false,
            recall_reason: "not requested".to_string(),
            telemetry: MemoryOsCheckpointTelemetry {
                current_fact_count: 1,
                recent_change_count: 1,
                live_claim_count: 0,
                open_obligation_count: 0,
                artifact_handle_count: 0,
                failure_count: 0,
            },
            selected_items: vec![MemoryOsPacketSelection {
                section: "memory".to_string(),
                kind: "decision".to_string(),
                summary: summary.to_string(),
                token_estimate: 20,
                score: 900,
                artifact_id: Some("artifact-1".to_string()),
                subject: Some("resolver".to_string()),
                provenance: vec!["checkpoint evidence".to_string()],
            }],
            exclusions: Vec::new(),
            reentry: MemoryOsCheckpointReentry {
                recommended_command: "cargo test".to_string(),
                current_recommendation: Some("Verify compiler truth first".to_string()),
                first_question: "What is the next compiler truth move?".to_string(),
                first_verification: "cargo test".to_string(),
            },
        }
    }

    #[test]
    fn recall_returns_topic_match_without_overview_fallback() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let tracker = Tracker::new_at_path(&tmp.path().join("history.db")).expect("tracker");
        insert_checkpoint(
            &tracker,
            "C:/repo",
            &capture("Resolver comes after recall and Session Brain truth."),
        );

        let report = tracker
            .get_memory_os_recall_report(MemoryOsInspectionScope::User, None, "resolver recall")
            .expect("recall");
        assert!(!report.matches.is_empty());
        assert!(report.matches[0].answer.contains("Resolver"));
        assert!(report.no_match_reason.is_none());
    }

    #[test]
    fn recall_returns_attached_session_summary_bullets() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let tracker = Tracker::new_at_path(&tmp.path().join("history.db")).expect("tracker");
        let mut capture = capture("Unrelated checkpoint about packaging.");
        capture.packet_id = "packet-session-summary".to_string();
        capture.selected_items = vec![MemoryOsPacketSelection {
            section: "session_summary".to_string(),
            kind: "session-summary".to_string(),
            summary: [
                "- Session summary: codex session `abc` for `C:/repo` started 2026-04-18T00:00:00Z.",
                "- User asked first: build a comprehensive session summary tool within Munin that preserves the real user ask, project, source, command results, corrections, and handoff context for future recall.",
                "- Latest user ask: attach the session summary output to Munin recall so historical sessions return a compact human-readable answer instead of raw transcript noise.",
                "- Work performed: 4 command(s) captured, 3 succeeded, 1 failed, 0 had unknown outcome, and the failed command was represented as outcome metadata rather than raw build output.",
                "- Important handoff: verify summary recall against current repo state before acting and keep exactly five bullet points visible in recall output.",
            ]
            .join("\n"),
            token_estimate: 80,
            score: 1800,
            artifact_id: Some("session-summary:codex:abc".to_string()),
            subject: Some("session-summary:codex:abc".to_string()),
            provenance: vec!["session:codex".to_string()],
        }];

        insert_checkpoint(&tracker, "C:/repo", &capture);

        let report = tracker
            .get_memory_os_recall_report(
                MemoryOsInspectionScope::User,
                None,
                "comprehensive session summary munin recall",
            )
            .expect("recall");

        assert!(!report.matches.is_empty());
        assert_eq!(report.matches[0].source_kind, "session-summary");
        assert_eq!(
            report.matches[0]
                .answer
                .lines()
                .filter(|line| line.starts_with("- "))
                .count(),
            5
        );
        assert!(report.matches[0].answer.contains("Munin recall"));
    }

    #[test]
    fn recall_dedupes_session_summaries_across_schema_versions() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let tracker = Tracker::new_at_path(&tmp.path().join("history.db")).expect("tracker");
        let mut old_capture = capture("Unrelated checkpoint about packaging.");
        old_capture.packet_id = "onboarding-memory-os-session-onboarding-v13-codex-abc".to_string();
        old_capture.selected_items = vec![MemoryOsPacketSelection {
            section: "session_summary".to_string(),
            kind: "session-summary".to_string(),
            summary: [
                "- Session summary: codex session `abc` for `C:/repo` started 2026-04-18T00:00:00Z.",
                "- User asked first: duplicate session summary recall.",
                "- Latest user ask: old schema still had noisy command output.",
                "- Work performed: 1 command(s) captured, 1 succeeded, 0 failed, 0 had unknown outcome.",
                "- Important handoff: the latest successful command was `context ls C:/repo/logs/*.log 2>&1`; rerun only after checking current state.",
            ]
            .join("\n"),
            token_estimate: 80,
            score: 1800,
            artifact_id: Some("session-summary:codex:abc".to_string()),
            subject: Some("session-summary:codex:abc".to_string()),
            provenance: vec!["session:codex".to_string(), "session-id:abc".to_string()],
        }];
        let mut new_capture = old_capture.clone();
        new_capture.packet_id = "onboarding-memory-os-session-onboarding-v14-codex-abc".to_string();
        new_capture.selected_items[0].summary = [
            "- Session summary: codex session `abc` for `C:/repo` started 2026-04-18T00:00:00Z.",
            "- User asked first: duplicate session summary recall.",
            "- Latest user ask: new schema keeps command noise compact.",
            "- Work performed: 1 command(s) captured, 1 succeeded, 0 failed, 0 had unknown outcome.",
            "- Important handoff: the latest successful command was `ls`; rerun only after checking current state.",
        ]
        .join("\n");

        insert_checkpoint(&tracker, "C:/repo", &old_capture);
        insert_checkpoint(&tracker, "C:/repo", &new_capture);

        let report = tracker
            .get_memory_os_recall_report(
                MemoryOsInspectionScope::User,
                None,
                "duplicate session summary recall",
            )
            .expect("recall");

        let session_summary_matches = report
            .matches
            .iter()
            .filter(|item| item.source_kind == "session-summary")
            .collect::<Vec<_>>();
        assert_eq!(session_summary_matches.len(), 1);
        assert!(session_summary_matches[0]
            .source_ref
            .contains("memory-os-session-onboarding-v14"));
        assert!(session_summary_matches[0]
            .answer
            .contains("command noise compact"));
        assert!(!session_summary_matches[0].answer.contains("logs/*.log"));
    }

    #[test]
    fn recall_reports_no_matches_instead_of_dumping_overview() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let tracker = Tracker::new_at_path(&tmp.path().join("history.db")).expect("tracker");
        insert_checkpoint(
            &tracker,
            "C:/repo",
            &capture("Unrelated checkpoint about packaging."),
        );

        let report = tracker
            .get_memory_os_recall_report(MemoryOsInspectionScope::User, None, "astronomy")
            .expect("recall");
        assert!(report.matches.is_empty());
        assert!(report
            .no_match_reason
            .as_deref()
            .unwrap_or_default()
            .contains("overview fallback"));
    }

    fn insert_checkpoint(
        tracker: &Tracker,
        project_path: &str,
        capture: &MemoryOsCheckpointCapture,
    ) {
        let payload = serde_json::to_string(capture).expect("payload");
        tracker
            .conn
            .execute(
                "INSERT INTO memory_os_journal_events (
                    event_id, stream_id, stream_revision, expected_stream_revision, tx_index,
                    occurred_at, committed_at, event_kind, idempotency_key, idempotency_receipt_id,
                    project_path, scope_json, actor_json, target_refs_json, payload_json,
                    proof_refs_json, precondition_hash, result_hash, schema_fingerprint
                ) VALUES (?1, ?2, 1, NULL, 0, ?3, ?3, ?4, ?1, NULL, ?5, '{}', '{}', '[]', ?6, '[]', NULL, NULL, ?7)",
                params![
                    format!("event-{}", capture.packet_id),
                    format!("stream-{}", capture.packet_id),
                    capture.generated_at,
                    "legacy.packet-checkpoint.test",
                    project_path,
                    payload,
                    "test-schema",
                ],
            )
            .expect("insert checkpoint");
    }
}
