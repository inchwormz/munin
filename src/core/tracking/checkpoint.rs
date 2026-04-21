use anyhow::Result;
use chrono::{DateTime, Utc};
use rusqlite::params;

use super::{
    current_project_path_string, hash_text, memory_os_scope_params, parse_rfc3339_to_utc,
    MemoryOsShadowEvent, Tracker,
};

#[derive(Debug, Clone)]
pub(super) struct MemoryOsCheckpointEnvelope {
    pub(super) project_path: String,
    pub(super) captured_at: DateTime<Utc>,
    pub(super) capture: crate::core::memory_os::MemoryOsCheckpointCapture,
}

impl Tracker {
    pub fn record_memory_os_packet_checkpoint(
        &self,
        input: &crate::core::memory_os::MemoryOsCheckpointCapture,
    ) -> Result<()> {
        let project_path = current_project_path_string();
        self.record_memory_os_packet_checkpoint_for_project(&project_path, input)
    }

    pub fn record_memory_os_packet_checkpoint_for_project(
        &self,
        project_path: &str,
        input: &crate::core::memory_os::MemoryOsCheckpointCapture,
    ) -> Result<()> {
        let flags = crate::core::config::memory_os();
        let cutover_checkpoint_write =
            flags.journal_v1 && flags.dual_write_v1 && flags.checkpoint_v1;
        let onboarding_read_model_write =
            flags.read_model_v1 && input.profile == "session-onboarding";
        if !(cutover_checkpoint_write || onboarding_read_model_write) {
            return Ok(());
        }

        let payload_json = serde_json::to_string(input)?;
        let target_refs = input
            .selected_items
            .iter()
            .filter_map(|item| item.subject.clone())
            .collect::<Vec<_>>();
        let payload_hash = hash_text(&payload_json);
        let payload_suffix = payload_hash.chars().take(12).collect::<String>();
        let event_id = if onboarding_read_model_write && !cutover_checkpoint_write {
            format!(
                "legacy-packet-{}-{}-{}",
                input.preset.as_str(),
                input.packet_id.as_str(),
                payload_suffix
            )
        } else {
            format!(
                "legacy-packet-{}-{}",
                input.preset.as_str(),
                input.packet_id.as_str()
            )
        };
        let idempotency_key = if onboarding_read_model_write && !cutover_checkpoint_write {
            format!(
                "legacy.packet:{}:{}",
                input.packet_id.as_str(),
                payload_suffix
            )
        } else {
            format!("legacy.packet:{}", input.packet_id.as_str())
        };

        let event = MemoryOsShadowEvent {
            event_id: event_id.clone(),
            stream_id: format!("legacy.packet:{}:{}", project_path, input.preset.as_str()),
            stream_revision: 0,
            expected_stream_revision: None,
            tx_index: 0,
            event_kind: format!("legacy.packet-checkpoint.{}", input.preset.as_str()),
            idempotency_key,
            idempotency_receipt_id: None,
            project_path: project_path.to_string(),
            scope_json: serde_json::json!({
                "repo_id": project_path,
                "branch_id": "",
                "worktree_id": "",
                "task_id": serde_json::Value::Null,
                "objective_id": serde_json::Value::Null,
                "session_id": serde_json::Value::Null,
                "agent_id": serde_json::Value::Null,
                "runtime_profile": &input.profile,
                "os_profile": std::env::consts::OS,
                "valid_from": &input.generated_at,
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
            target_refs_json: serde_json::to_string(&target_refs)?,
            payload_json,
            proof_refs_json: "[]".to_string(),
            precondition_hash: None,
            result_hash: Some(input.pager_manifest_hash.clone()),
            schema_fingerprint: "memoryos-packet-checkpoint-v1".into(),
        };
        if cutover_checkpoint_write {
            self.record_memory_os_shadow_event(event)?;
        } else {
            self.record_memory_os_shadow_event_unchecked(event)?;
        }

        if let Some(journal_seq) = self.memory_os_journal_seq_for_event(&event_id)? {
            let _ = self.record_memory_os_projection_checkpoint(
                "packet-checkpoints",
                project_path,
                journal_seq,
                journal_seq,
                "append",
            );
        }

        if flags.action_v1 && input.profile != "session-onboarding" {
            let trigger_item = input
                .selected_items
                .iter()
                .find(|item| item.section == "open_obligations")
                .or_else(|| {
                    input
                        .selected_items
                        .iter()
                        .find(|item| item.section == "current_failures")
                })
                .or_else(|| {
                    input
                        .selected_items
                        .iter()
                        .find(|item| item.section == "validated_claim_leases")
                })
                .or_else(|| input.selected_items.first());
            let cue = crate::core::memory_os::MemoryOsActionCue {
                cue_kind: "checkpoint-reentry".to_string(),
                packet_preset: Some(input.preset.clone()),
                intent: Some(input.intent.clone()),
                override_type: None,
                correction_shape: None,
                trigger_section: trigger_item.map(|item| item.section.clone()),
                trigger_subject: trigger_item.and_then(|item| item.subject.clone()),
                trigger_summary: trigger_item.map(|item| item.summary.clone()),
            };
            let action = crate::core::memory_os::MemoryOsAction {
                action_kind: "run_command".to_string(),
                command_sig: Some(input.reentry.recommended_command.clone()),
                recommendation: input.reentry.current_recommendation.clone(),
            };
            self.record_memory_os_action_observation(
                &project_path,
                "checkpoint-reentry",
                Some(&event_id),
                &cue,
                &action,
                &input.packet_id,
                &input.generated_at,
            )?;
        }

        Ok(())
    }

    pub(super) fn load_memory_os_checkpoint_captures(
        &self,
        scope: crate::core::memory_os::MemoryOsInspectionScope,
        project_path: Option<&str>,
    ) -> Result<Vec<MemoryOsCheckpointEnvelope>> {
        let (project_exact, project_glob, _) = memory_os_scope_params(scope, project_path);
        let mut stmt = self.conn.prepare(
            "SELECT project_path, committed_at, payload_json
             FROM memory_os_journal_events
             WHERE event_kind LIKE 'legacy.packet-checkpoint.%'
               AND (?1 IS NULL OR project_path = ?1 OR project_path GLOB ?2)
             ORDER BY committed_at DESC, journal_seq DESC",
        )?;
        let rows = stmt
            .query_map(params![project_exact, project_glob], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        let mut checkpoints = Vec::new();
        for (project_path, committed_at, payload_json) in rows {
            let Ok(capture) = serde_json::from_str::<
                crate::core::memory_os::MemoryOsCheckpointCapture,
            >(&payload_json) else {
                continue;
            };
            checkpoints.push(MemoryOsCheckpointEnvelope {
                project_path,
                captured_at: parse_rfc3339_to_utc(&committed_at),
                capture,
            });
        }
        Ok(checkpoints)
    }
}
