#![allow(dead_code)]
//! Narrow durable-kernel projection scaffolding for Context Memory OS work.
//!
//! This module intentionally stays additive and detached from the current
//! worldview/compiler path. It provides the first typed read-model surface over
//! the new journal/trust foundation so future cutover work does not have to
//! overload `worldview.rs`.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectionCheckpointRef {
    pub projection_name: String,
    pub from_seq: i64,
    pub to_seq: i64,
    pub rebuild_kind: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryOsPacketSelection {
    pub section: String,
    pub kind: String,
    pub summary: String,
    pub token_estimate: usize,
    pub score: i64,
    pub artifact_id: Option<String>,
    pub subject: Option<String>,
    pub provenance: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryOsCheckpointTelemetry {
    pub current_fact_count: usize,
    pub recent_change_count: usize,
    pub live_claim_count: usize,
    pub open_obligation_count: usize,
    pub artifact_handle_count: usize,
    pub failure_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryOsCheckpointReentry {
    pub recommended_command: String,
    pub current_recommendation: Option<String>,
    pub first_question: String,
    pub first_verification: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryOsCheckpointCapture {
    pub packet_id: String,
    pub generated_at: String,
    pub preset: String,
    pub intent: String,
    pub profile: String,
    pub goal: Option<String>,
    pub budget: usize,
    pub estimated_tokens: usize,
    pub estimated_source_tokens: usize,
    pub pager_manifest_hash: String,
    pub recall_mode: String,
    pub recall_used: bool,
    pub recall_reason: String,
    pub telemetry: MemoryOsCheckpointTelemetry,
    pub selected_items: Vec<MemoryOsPacketSelection>,
    pub exclusions: Vec<String>,
    pub reentry: MemoryOsCheckpointReentry,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryOsClaimProjection {
    pub claim_id: String,
    pub source_event_id: String,
    pub journal_seq: i64,
    pub observed_at: String,
    pub claim_kind: String,
    pub claim_text: String,
    pub confidence: String,
    pub scope_key: Option<String>,
    pub source_kind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryOsOpenLoopProjection {
    pub open_loop_id: String,
    pub summary: String,
    pub loop_kind: String,
    pub status: String,
    pub severity: String,
    pub source_event_ids: Vec<String>,
    pub last_seen_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryOsCheckpointProjection {
    pub checkpoint_id: String,
    pub source_event_id: String,
    pub journal_seq: i64,
    pub captured_at: String,
    pub preset: String,
    pub intent: String,
    pub goal: Option<String>,
    pub current_recommendation: Option<String>,
    pub active_risks: Vec<String>,
    pub open_loop_summaries: Vec<String>,
    pub live_claim_summaries: Vec<String>,
    pub pager_manifest_hash: String,
    pub reentry: MemoryOsCheckpointReentry,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryOsActionCue {
    pub cue_kind: String,
    pub packet_preset: Option<String>,
    pub intent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub override_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correction_shape: Option<String>,
    pub trigger_section: Option<String>,
    pub trigger_subject: Option<String>,
    pub trigger_summary: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryOsAction {
    pub action_kind: String,
    pub command_sig: Option<String>,
    pub recommendation: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryOsActionPolicyCandidate {
    pub candidate_id: String,
    pub source_kind: String,
    pub actuator_type: String,
    pub autonomy_level: String,
    pub status: String,
    pub title: String,
    pub summary: String,
    pub confidence: String,
    pub cue: MemoryOsActionCue,
    pub action: MemoryOsAction,
    pub precedent_count: usize,
    pub success_count: usize,
    pub failure_count: usize,
    pub last_observed_at: String,
    pub last_executed_at: Option<String>,
    pub review_after: Option<String>,
    pub expires_at: Option<String>,
    pub lifecycle_policy: Option<String>,
    pub aging_status: String,
    pub source_refs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryOsActionPolicyRule {
    pub rule_id: String,
    pub title: String,
    pub summary: String,
    pub action_kind: String,
    pub strength: String,
    pub confidence: String,
    pub scope: String,
    pub scope_target: Option<String>,
    pub target_agent: Option<String>,
    pub suggested_command: Option<String>,
    pub recommendation: Option<String>,
    pub review_after: Option<String>,
    pub expires_at: Option<String>,
    pub lifecycle_policy: Option<String>,
    pub aging_status: String,
    pub trigger_assertion_ids: Vec<String>,
    pub supporting_evidence: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryOsActionPolicyAssertion {
    pub assertion_id: String,
    pub source_kind: String,
    pub summary: String,
    pub scope: String,
    pub scope_target: Option<String>,
    pub supporting_evidence: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryOsApprovalJobRecord {
    pub job_id: String,
    pub source_kind: String,
    pub status: String,
    pub title: String,
    pub summary: String,
    pub scope: String,
    pub scope_target: Option<String>,
    pub item_id: Option<String>,
    pub item_kind: String,
    pub local_date: String,
    pub expected_effect: Option<String>,
    pub queue_path: Option<String>,
    pub result_path: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub review_after: Option<String>,
    pub expires_at: Option<String>,
    pub last_reviewed_at: Option<String>,
    pub closure_reason: Option<String>,
    pub supporting_evidence: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryOsHookCapabilityRecord {
    pub surface: String,
    pub rewrite_support: String,
    pub ask_support: String,
    pub updated_input_support: bool,
    pub fallback_mode: String,
    pub status: String,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryOsActionPolicyViewReport {
    pub generated_at: String,
    pub scope: MemoryOsInspectionScope,
    pub candidate_count: usize,
    pub candidates: Vec<MemoryOsActionPolicyCandidate>,
    pub behavior_change_count: usize,
    pub assertion_count: usize,
    pub assertions: Vec<MemoryOsActionPolicyAssertion>,
    pub approvals_count: usize,
    pub approvals: Vec<MemoryOsApprovalJobRecord>,
    pub hook_capabilities: Vec<MemoryOsHookCapabilityRecord>,
    pub rules: Vec<MemoryOsActionPolicyRule>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryOsEvidenceEventRecord {
    pub evidence_id: String,
    pub lane: String,
    pub source_record_id: String,
    pub root_source_id: String,
    pub derivation_kind: String,
    pub project_path: String,
    pub event_kind: String,
    pub timestamp: String,
    pub summary: String,
    pub scope_hints: Vec<String>,
    pub supporting_refs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryOsPromotedAssertionRecord {
    pub assertion_id: String,
    pub category: String,
    pub statement: String,
    pub normalized_claim: String,
    pub scope: String,
    pub scope_target: Option<String>,
    pub status: String,
    pub promotion_basis: String,
    pub confidence: String,
    pub stability: String,
    pub first_promoted_at: String,
    pub last_confirmed_at: String,
    pub review_after: Option<String>,
    pub expires_at: Option<String>,
    pub last_reviewed_at: Option<String>,
    pub demotion_reason: Option<String>,
    pub supporting_evidence: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryOsProjectSnapshot {
    pub project_path: String,
    pub journal_event_count: i64,
    pub last_journal_seq: Option<i64>,
    pub verification_result_count: i64,
    pub trust_observation_count: i64,
    pub projection_checkpoints: Vec<ProjectionCheckpointRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryOsProjectKernel {
    pub project_path: String,
    pub last_journal_seq: Option<i64>,
    pub claims: Vec<MemoryOsClaimProjection>,
    pub open_loops: Vec<MemoryOsOpenLoopProjection>,
    pub checkpoints: Vec<MemoryOsCheckpointProjection>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryOsTrustTargetSummary {
    pub target_kind: String,
    pub observation_count: usize,
    pub must_not_packetize_count: usize,
    pub secret_count: usize,
    pub pii_count: usize,
    pub latest_observed_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryOsTrustObservationRecord {
    pub observation_id: String,
    pub project_path: String,
    pub target_kind: String,
    pub target_ref: String,
    pub action_kind: String,
    pub decision: String,
    pub reason_json: String,
    pub read_seq_cut: Option<i64>,
    pub policy_model_id: Option<String>,
    pub sensitivity_class: String,
    pub contains_secret: bool,
    pub contains_pii: bool,
    pub must_not_packetize: bool,
    pub taint_state: String,
    pub observed_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryOsTrustReport {
    pub generated_at: String,
    pub scope: MemoryOsInspectionScope,
    pub observation_count: usize,
    pub must_not_packetize_count: usize,
    pub secret_count: usize,
    pub pii_count: usize,
    pub by_target: Vec<MemoryOsTrustTargetSummary>,
    pub recent_observations: Vec<MemoryOsTrustObservationRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryOsPromotionResultRecord {
    pub verification_result_id: String,
    pub root: Option<String>,
    pub split: String,
    pub system: String,
    pub result: String,
    pub reason: Option<String>,
    pub verification_time: String,
    #[serde(default)]
    pub independent: bool,
    #[serde(default)]
    pub contamination_free: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryOsPromotionReport {
    pub generated_at: String,
    pub read_model_enabled: bool,
    pub resume_enabled: bool,
    pub handoff_enabled: bool,
    pub strict_gate_enabled: bool,
    pub eligible: bool,
    pub resume_cutover_ready: bool,
    pub handoff_cutover_ready: bool,
    pub required_split: String,
    pub required_system: String,
    pub matching_result_count: usize,
    pub missing_required_splits: Vec<String>,
    pub contaminated_result_count: usize,
    pub required_results: Vec<MemoryOsPromotionResultRecord>,
    pub decision_summary: String,
    pub latest_matching_result: Option<MemoryOsPromotionResultRecord>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum MemoryOsInspectionScope {
    User,
    Project,
}

impl std::fmt::Display for MemoryOsInspectionScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let value = match self {
            Self::User => "user",
            Self::Project => "project",
        };
        write!(f, "{value}")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryOsImportedSourceSummary {
    pub source: String,
    pub sessions: usize,
    pub shell_executions: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryOsProjectSummary {
    pub project_path: String,
    pub repo_label: String,
    pub sessions: usize,
    pub shell_executions: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryOsCorrectionPatternSummary {
    pub error_kind: String,
    pub wrong_command: String,
    pub corrected_command: String,
    pub count: usize,
    pub successful_replays: usize,
    pub failed_replays: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MemoryOsSourceBehaviorSummary {
    pub source: String,
    pub sessions: usize,
    pub shell_executions: usize,
    pub corrections: usize,
    pub redirects: usize,
    pub redirected_sessions: usize,
    pub successful_redirects: usize,
    pub shells_per_session: f64,
    pub corrections_per_100_shells: f64,
    pub redirects_per_session: f64,
    pub avg_commands_to_success_after_redirect: Option<f64>,
    pub avg_seconds_to_success_after_redirect: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryOsNarrativeFinding {
    pub title: String,
    pub summary: String,
    pub evidence: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryOsMisunderstandingPattern {
    pub label: String,
    pub summary: String,
    pub count: usize,
    pub examples: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryOsBehaviorChangeRecommendation {
    pub target_agent: String,
    pub change: String,
    pub rationale: String,
    pub evidence: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryOsFrictionFix {
    pub fix_id: String,
    pub title: String,
    pub impact: String,
    pub status: String,
    pub summary: String,
    pub permanent_fix: String,
    pub evidence: Vec<String>,
    pub score: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct MemoryOsRedirectSummary {
    pub redirects: usize,
    pub redirected_sessions: usize,
    pub redirects_with_resumed_shell: usize,
    pub redirects_with_success_after_resume: usize,
    pub avg_commands_to_success_after_redirect: Option<f64>,
    pub avg_seconds_to_success_after_redirect: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryOsOnboardingState {
    pub schema_version: String,
    pub status: String,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub sessions_processed: usize,
    pub shells_ingested: usize,
    pub corrections_ingested: usize,
    pub imported_sources: Vec<MemoryOsImportedSourceSummary>,
    pub checkpoint_count: usize,
    pub journal_event_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryOsHistoryTotals {
    pub journal_event_count: usize,
    pub checkpoint_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryOsOverviewReport {
    pub generated_at: String,
    pub scope: MemoryOsInspectionScope,
    pub imported_sessions: usize,
    pub imported_shell_executions: usize,
    pub imported_sources: Vec<MemoryOsImportedSourceSummary>,
    pub top_projects: Vec<MemoryOsProjectSummary>,
    pub top_correction_patterns: Vec<MemoryOsCorrectionPatternSummary>,
    pub active_work: Vec<MemoryOsNarrativeFinding>,
    pub top_action_memory_candidates: Vec<MemoryOsActionPolicyCandidate>,
    pub onboarding: MemoryOsOnboardingState,
    pub serving_policy: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MemoryOsProfileReport {
    pub generated_at: String,
    pub scope: MemoryOsInspectionScope,
    pub imported_sessions: usize,
    pub by_source: Vec<MemoryOsSourceBehaviorSummary>,
    pub preferences: Vec<MemoryOsNarrativeFinding>,
    pub operating_style: Vec<MemoryOsNarrativeFinding>,
    pub autonomy_tendencies: Vec<MemoryOsNarrativeFinding>,
    pub epistemic_preferences: Vec<MemoryOsNarrativeFinding>,
    pub recurring_themes: Vec<MemoryOsNarrativeFinding>,
    pub friction_triggers: Vec<MemoryOsNarrativeFinding>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MemoryOsFrictionReport {
    pub generated_at: String,
    pub scope: MemoryOsInspectionScope,
    pub top_fixes: Vec<MemoryOsFrictionFix>,
    pub new_unproven_friction: Vec<MemoryOsFrictionFix>,
    pub by_source: Vec<MemoryOsSourceBehaviorSummary>,
    pub redirects: MemoryOsRedirectSummary,
    pub repeated_corrections: Vec<MemoryOsCorrectionPatternSummary>,
    pub likely_misunderstandings: Vec<MemoryOsMisunderstandingPattern>,
    pub behavior_changes: Vec<MemoryOsBehaviorChangeRecommendation>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryOsRecallMatch {
    pub title: String,
    pub answer: String,
    pub score: i64,
    pub source_kind: String,
    pub source_ref: String,
    pub project_path: String,
    pub evidence: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryOsRecallReport {
    pub generated_at: String,
    pub scope: MemoryOsInspectionScope,
    pub query: String,
    pub matches: Vec<MemoryOsRecallMatch>,
    pub no_match_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MemoryOsBriefReport {
    pub generated_at: String,
    pub scope: MemoryOsInspectionScope,
    pub what_i_know: Vec<MemoryOsNarrativeFinding>,
    pub how_you_work: Vec<MemoryOsNarrativeFinding>,
    pub what_is_active: Vec<MemoryOsNarrativeFinding>,
    pub next_steps: Vec<MemoryOsNarrativeFinding>,
    pub watchouts: Vec<MemoryOsNarrativeFinding>,
}
