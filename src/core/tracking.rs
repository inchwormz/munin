//! Token savings tracking and analytics system.
//!
//! This module provides comprehensive tracking of local agent and wrapper
//! executions, recording token savings, execution times, and providing
//! aggregation APIs for daily/weekly/monthly statistics.
//!
//! # Architecture
//!
//! - Storage: SQLite database (~/.local/share/munin/history.db by default)
//! - Retention: 90-day automatic cleanup
//! - Metrics: Input/output tokens, savings %, execution time
//!
//! # Quick Start
//!
//! ```no_run
//! use munin_memory::tracking::{TimedExecution, Tracker};
//!
//! // Track a command execution
//! let timer = TimedExecution::start();
//! let input = "raw output";
//! let output = "filtered output";
//! timer.track("ls -la", "context ls", input, output);
//!
//! // Query statistics
//! let tracker = Tracker::new().unwrap();
//! let summary = tracker.get_summary().unwrap();
//! println!("Saved {} tokens", summary.total_saved);
//! ```
//!
//! See [docs/tracking.md](../docs/tracking.md) for full documentation.

use anyhow::Result;
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use sha2::{Digest, Sha256};
#[cfg(test)]
use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Instant;

mod action_policy;
mod approval_jobs;
mod checkpoint;
mod claim_leases;
mod evidence;
mod gain_rollups;
mod journal;
mod kernel;
mod parse_failures;
mod policy_model;
mod promotion_gate;
mod read_model;
mod recall;
mod reports;
mod signals;
mod trust;

pub use self::approval_jobs::{ApprovalJobInput, ApprovalJobRecord, ApprovalJobStatus};
use self::checkpoint::MemoryOsCheckpointEnvelope;
pub use self::claim_leases::{
    ClaimLeaseConfidence, ClaimLeaseDependency, ClaimLeaseDependencyKind, ClaimLeaseRecord,
    ClaimLeaseStatus, ClaimLeaseType, UserDecisionRecord,
};
#[cfg(test)]
use self::gain_rollups::{AggregateStats, GainRollup};
#[allow(unused_imports)]
pub use self::gain_rollups::{
    ArtifactSummary, CommandGainDetail, ContextSummary, DayStats, GainSummary, MonthStats,
    WeekStats,
};
pub use self::journal::MemoryOsShadowEvent;
#[cfg(test)]
use self::kernel::{upsert_memory_os_open_loop, MemoryOsOpenLoopIdentity};
use self::parse_failures::is_assistant_housekeeping_command;
#[allow(unused_imports)]
pub use self::parse_failures::{
    record_parse_failure_silent, ParseFailureRecord, ParseFailureSummary,
};
#[allow(unused_imports)]
pub use self::policy_model::{MemoryOsAccessRule, MemoryOsPolicyModelInput};
pub use self::promotion_gate::{MemoryOsVerificationResultInput, MemoryOsVerificationStatus};
#[allow(unused_imports)]
pub use self::trust::{
    scan_memory_os_trust_payload, MemoryOsTrustDecision, MemoryOsTrustFinding,
    MemoryOsTrustFindingKind, MemoryOsTrustObservationInput, MemoryOsTrustScanSummary,
};

// Ã¢â€â‚¬Ã¢â€â‚¬ Project path helpers Ã¢â€â‚¬Ã¢â€â‚¬ // added: project-scoped tracking support

/// Get the canonical project path string for the current working directory.
fn current_project_path_string() -> String {
    crate::core::utils::current_project_root_string()
}

fn hash_text(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn parse_rfc3339_to_utc(value: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(value)
        .map(|timestamp| timestamp.with_timezone(&Utc))
        .unwrap_or_else(|_| Utc::now())
}

/// Build SQL filter params for project-scoped queries.
/// Returns (exact_match, glob_prefix) for WHERE clause.
/// Uses GLOB instead of LIKE to avoid `_` and `%` in paths acting as wildcards. // changed: GLOB
fn project_filter_params(project_path: Option<&str>) -> (Option<String>, Option<String>) {
    match project_path {
        Some(p) => (
            Some(p.to_string()),
            Some(format!("{}{}*", p, std::path::MAIN_SEPARATOR)), // changed: GLOB pattern with * wildcard
        ),
        None => (None, None),
    }
}

fn resolved_project_path(project_path: Option<&str>) -> String {
    let Some(value) = project_path else {
        return current_project_path_string();
    };
    let trimmed = value.trim();
    if trimmed.is_empty()
        || trimmed.starts_with("recall://")
        || trimmed.starts_with("session://")
        || trimmed.starts_with("memory://")
    {
        return trimmed.to_string();
    }
    let raw = Path::new(trimmed);
    if raw.is_absolute() {
        return trimmed.to_string();
    }
    let path = std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(raw);
    if !path.exists() {
        return trimmed.to_string();
    }
    let canonical = path.canonicalize().unwrap_or(path);
    let root = crate::core::utils::detect_project_root(&canonical);
    crate::core::utils::normalize_windows_path_string(root.to_string_lossy().as_ref())
}

fn push_unique_string(values: &mut Vec<String>, value: String) {
    if !values.iter().any(|existing| existing == &value) {
        values.push(value);
    }
}

fn project_repo_hint(project_path: &str) -> Option<String> {
    Path::new(project_path)
        .file_name()
        .and_then(|name| name.to_str())
        .map(|name| name.to_string())
}

fn command_project_hint(project_path: Option<&str>) -> Option<String> {
    project_path.map(|path| path.to_string())
}

fn memory_os_scope_params(
    scope: crate::core::memory_os::MemoryOsInspectionScope,
    project_path: Option<&str>,
) -> (Option<String>, Option<String>, Option<String>) {
    match scope {
        crate::core::memory_os::MemoryOsInspectionScope::User => (None, None, None),
        crate::core::memory_os::MemoryOsInspectionScope::Project => {
            let resolved = resolved_project_path(project_path);
            let (exact, glob) = project_filter_params(Some(&resolved));
            (exact, glob, Some(resolved))
        }
    }
}

fn scope_project_path_or_current(
    scope: crate::core::memory_os::MemoryOsInspectionScope,
    project_path: Option<&str>,
) -> Option<String> {
    match scope {
        crate::core::memory_os::MemoryOsInspectionScope::User => None,
        crate::core::memory_os::MemoryOsInspectionScope::Project => {
            Some(resolved_project_path(project_path))
        }
    }
}

fn memory_os_repo_label(project_path: &str) -> String {
    if let Ok(user_profile) = std::env::var("USERPROFILE") {
        let user_root = PathBuf::from(&user_profile);
        let workspace_root = user_root.join("Projects");
        let project = Path::new(project_path);
        if project == workspace_root {
            return "workspace-root".to_string();
        }
        if project == user_root {
            return "home-root".to_string();
        }
        if let Ok(relative) = project.strip_prefix(&workspace_root) {
            let components = relative
                .components()
                .filter_map(|component| component.as_os_str().to_str())
                .collect::<Vec<_>>();
            if !components.is_empty() {
                return components.into_iter().take(2).collect::<Vec<_>>().join("/");
            }
        }
    }
    if let Some(hint) = project_repo_hint(project_path) {
        return hint;
    }
    if project_path.starts_with("recall://") || project_path.starts_with("session://") {
        return project_path.to_string();
    }
    project_path.to_string()
}

fn compact_display_text(text: &str, max_len: usize) -> String {
    let compact = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.len() <= max_len {
        compact
    } else {
        let mut truncated = compact
            .chars()
            .take(max_len.saturating_sub(3))
            .collect::<String>();
        truncated.push_str("...");
        truncated
    }
}

use super::constants::{DEFAULT_HISTORY_DAYS, HISTORY_DB};

const MAINTENANCE_KEY_CLEANUP_OLD: &str = "cleanup_old";
const CLEANUP_INTERVAL_SECS: i64 = 12 * 60 * 60;
const WORKING_MEMORY_BURY_MIN_AGE_HOURS: i64 = 48;
const COMMAND_TELEMETRY_FILTER_SQL: &str = r#"lower(replace(original_cmd, '\', '/')) NOT LIKE '%/.claude/statusline.js%'
    AND lower(replace(original_cmd, '\', '/')) NOT LIKE '%/.claude/hooks/%'
    AND lower(replace(original_cmd, '\', '/')) NOT LIKE '%/.claude/scripts/localhost-registry.js%'
    AND lower(replace(original_cmd, '\', '/')) NOT LIKE 'bash -c file="$claude_file_path";%'
    AND lower(replace(original_cmd, '\', '/')) NOT LIKE 'bash -c if echo "$claude_file_path"%'"#;
const MUNIN_CANONICAL_INPUT_SQL: &str = "COALESCE(canonical_input_tokens, CASE WHEN (current_fact_count + recent_change_count + live_claim_count + open_obligation_count + artifact_handle_count + failure_count) = 0 THEN 0 ELSE estimated_source_tokens END)";
const MUNIN_CANONICAL_OUTPUT_SQL: &str = "COALESCE(canonical_output_tokens, CASE WHEN (current_fact_count + recent_change_count + live_claim_count + open_obligation_count + artifact_handle_count + failure_count) = 0 THEN 0 ELSE MIN(rendered_tokens, estimated_source_tokens) END)";
const MUNIN_CANONICAL_SAVED_SQL: &str = "MAX(COALESCE(canonical_input_tokens, CASE WHEN (current_fact_count + recent_change_count + live_claim_count + open_obligation_count + artifact_handle_count + failure_count) = 0 THEN 0 ELSE estimated_source_tokens END) - COALESCE(canonical_output_tokens, CASE WHEN (current_fact_count + recent_change_count + live_claim_count + open_obligation_count + artifact_handle_count + failure_count) = 0 THEN 0 ELSE MIN(rendered_tokens, estimated_source_tokens) END), 0)";

fn context_event_has_reused_state(stats: &ContextEventStats) -> bool {
    stats.current_fact_count > 0
        || stats.recent_change_count > 0
        || stats.live_claim_count > 0
        || stats.open_obligation_count > 0
        || stats.artifact_handle_count > 0
        || stats.failure_count > 0
}

/// Main tracking interface for recording and querying command history.
///
/// Manages SQLite database connection and provides methods for:
/// - Recording command executions with token counts and timing
/// - Querying aggregated statistics (summary, daily, weekly, monthly)
/// - Retrieving recent command history
///
/// # Database Location
///
/// - Linux: `~/.local/share/context/history.db`
/// - macOS: `~/Library/Application Support/context/history.db`
/// - Windows: `%LOCALAPPDATA%\context\history.db`
///
/// # Examples
///
/// ```no_run
/// use munin_memory::tracking::Tracker;
///
/// let tracker = Tracker::new()?;
/// tracker.record("ls -la", "context ls", 1000, 200, 50)?;
///
/// let summary = tracker.get_summary()?;
/// println!("Total saved: {} tokens", summary.total_saved);
/// # Ok::<(), anyhow::Error>(())
/// ```
pub struct Tracker {
    conn: Connection,
}

/// Individual command record from tracking history.
///
/// Contains timestamp, command name, and savings metrics for a single execution.
#[allow(dead_code)]
#[derive(Debug)]
pub struct CommandRecord {
    /// UTC timestamp when command was executed
    pub timestamp: DateTime<Utc>,
    /// Context command that was executed (e.g., "context ls")
    pub context_cmd: String,
    /// Estimated raw input tokens for the original command output
    pub input_tokens: usize,
    /// Estimated filtered output tokens for the recorded command output
    pub output_tokens: usize,
    /// Number of tokens saved (input - output)
    pub saved_tokens: usize,
    /// Savings percentage ((saved / input) * 100)
    pub savings_pct: f64,
}

/// Deterministic worldview observation persisted for prompt compilation.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct WorldviewRecord {
    /// UTC timestamp when the observation was recorded
    pub timestamp: DateTime<Utc>,
    /// Observation family (`read`, `grep`, `diff`, `git-status`, ...)
    pub event_type: String,
    /// Stable subject key for deduping current state
    pub subject_key: String,
    /// Command signature that produced the observation
    pub command_sig: String,
    /// Human-readable summary for prompt compilation
    pub summary: String,
    /// State fingerprint used to detect new/changed/unchanged
    pub fingerprint: String,
    /// Status transition classification (`new`, `changed`, `unchanged`)
    pub status: String,
    /// Optional artifact handle associated with this observation
    pub artifact_id: Option<String>,
    /// Structured payload JSON for machine consumption
    pub payload_json: String,
}

#[derive(Debug, Clone, Copy)]
pub struct ContextEventStats {
    pub rendered_tokens: usize,
    pub estimated_source_tokens: usize,
    pub current_fact_count: usize,
    pub recent_change_count: usize,
    pub live_claim_count: usize,
    pub open_obligation_count: usize,
    pub artifact_handle_count: usize,
    pub failure_count: usize,
}

#[derive(Debug, Clone, Default)]
pub struct ContextRuntimeInfo {
    pub source: String,
    pub session_id: Option<String>,
    pub thread_id: Option<String>,
    pub turn_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ContextSelectedItemRecord {
    pub item_id: String,
    pub section: String,
    pub kind: String,
    pub summary: String,
    pub token_estimate: usize,
    pub score: i64,
    pub artifact_id: Option<String>,
    pub subject: Option<String>,
    pub provenance: Vec<String>,
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct ContextItemEventRow {
    pub timestamp: DateTime<Utc>,
    pub project_path: String,
    pub event_type: String,
    pub packet_id: String,
    pub runtime_source: String,
    pub runtime_session_id: Option<String>,
    pub runtime_thread_id: Option<String>,
    pub runtime_turn_id: Option<String>,
    pub item_id: String,
    pub section: String,
    pub kind: String,
    pub summary: String,
    pub artifact_id: Option<String>,
    pub subject: Option<String>,
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct ArtifactEventRecord {
    pub timestamp: DateTime<Utc>,
    pub project_path: String,
    pub command_sig: String,
    pub artifact_id: String,
    pub source_layer: String,
    pub event_type: String,
    pub input_tokens: usize,
    pub output_tokens: usize,
    pub saved_tokens: usize,
}

impl Tracker {
    /// Create a new tracker instance.
    ///
    /// Opens or creates the SQLite database at the platform-specific location.
    /// Automatically creates the `commands` table if it doesn't exist and runs
    /// any necessary schema migrations.
    ///
    /// # Errors
    ///
    /// Returns error if:
    /// - Cannot determine database path
    /// - Cannot create parent directories
    /// - Cannot open/create SQLite database
    /// - Schema creation/migration fails
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use munin_memory::tracking::Tracker;
    ///
    /// let tracker = Tracker::new()?;
    /// # Ok::<(), anyhow::Error>(())
    /// ```
    pub fn new() -> Result<Self> {
        let db_path = get_db_path()?;
        Self::open_at_path(&db_path)
    }

    fn open_at_path(db_path: &Path) -> Result<Self> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let conn = Connection::open(db_path)?;
        // WAL mode + busy_timeout for concurrent access (multiple Claude Code instances).
        // Non-fatal: NFS/read-only filesystems may not support WAL.
        let _ = conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA busy_timeout=5000;",
        );
        conn.execute(
            "CREATE TABLE IF NOT EXISTS commands (
                id INTEGER PRIMARY KEY,
                timestamp TEXT NOT NULL,
                original_cmd TEXT NOT NULL,
                context_cmd TEXT NOT NULL,
                input_tokens INTEGER NOT NULL,
                output_tokens INTEGER NOT NULL,
                saved_tokens INTEGER NOT NULL,
                savings_pct REAL NOT NULL
            )",
            [],
        )?;

        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_timestamp ON commands(timestamp)",
            [],
        )?;

        // Migration: add exec_time_ms column if it doesn't exist
        let _ = conn.execute(
            "ALTER TABLE commands ADD COLUMN exec_time_ms INTEGER DEFAULT 0",
            [],
        );
        // Migration: add project_path column with DEFAULT '' for new rows // changed: added DEFAULT
        let _ = conn.execute(
            "ALTER TABLE commands ADD COLUMN project_path TEXT DEFAULT ''",
            [],
        );
        // One-time migration: normalize NULLs from pre-default schema // changed: guarded with EXISTS
        let has_nulls: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM commands WHERE project_path IS NULL)",
                [],
                |row| row.get(0),
            )
            .unwrap_or(false);
        if has_nulls {
            let _ = conn.execute(
                "UPDATE commands SET project_path = '' WHERE project_path IS NULL",
                [],
            );
        }
        // Index for fast project-scoped gain queries // added
        let _ = conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_project_path_timestamp ON commands(project_path, timestamp)",
            [],
        );

        conn.execute(
            "CREATE TABLE IF NOT EXISTS parse_failures (
                id INTEGER PRIMARY KEY,
                timestamp TEXT NOT NULL,
                raw_command TEXT NOT NULL,
                error_message TEXT NOT NULL,
                fallback_succeeded INTEGER NOT NULL DEFAULT 0
            )",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_pf_timestamp ON parse_failures(timestamp)",
            [],
        )?;

        conn.execute(
            "CREATE TABLE IF NOT EXISTS artifact_events (
                id INTEGER PRIMARY KEY,
                timestamp TEXT NOT NULL,
                project_path TEXT NOT NULL DEFAULT '',
                command_sig TEXT NOT NULL,
                artifact_id TEXT NOT NULL,
                source_layer TEXT NOT NULL,
                event_type TEXT NOT NULL,
                input_tokens INTEGER NOT NULL,
                output_tokens INTEGER NOT NULL,
                saved_tokens INTEGER NOT NULL
            )",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_artifact_timestamp ON artifact_events(timestamp)",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_artifact_project_timestamp ON artifact_events(project_path, timestamp)",
            [],
        )?;

        conn.execute(
            "CREATE TABLE IF NOT EXISTS worldview_events (
                id INTEGER PRIMARY KEY,
                timestamp TEXT NOT NULL,
                project_path TEXT NOT NULL DEFAULT '',
                event_type TEXT NOT NULL,
                subject_key TEXT NOT NULL,
                command_sig TEXT NOT NULL,
                summary TEXT NOT NULL,
                fingerprint TEXT NOT NULL,
                status TEXT NOT NULL,
                artifact_id TEXT,
                payload_json TEXT NOT NULL
            )",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_worldview_project_timestamp ON worldview_events(project_path, timestamp)",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_worldview_subject_timestamp ON worldview_events(project_path, subject_key, timestamp)",
            [],
        )?;

        conn.execute(
            "CREATE TABLE IF NOT EXISTS context_events (
                id INTEGER PRIMARY KEY,
                timestamp TEXT NOT NULL,
                project_path TEXT NOT NULL DEFAULT '',
                event_type TEXT NOT NULL,
                rendered_tokens INTEGER NOT NULL,
                estimated_source_tokens INTEGER NOT NULL,
                saved_tokens INTEGER NOT NULL,
                canonical_input_tokens INTEGER,
                canonical_output_tokens INTEGER,
                current_fact_count INTEGER NOT NULL,
                recent_change_count INTEGER NOT NULL,
                live_claim_count INTEGER NOT NULL,
                open_obligation_count INTEGER NOT NULL,
                artifact_handle_count INTEGER NOT NULL,
                failure_count INTEGER NOT NULL
            )",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_context_project_timestamp ON context_events(project_path, timestamp)",
            [],
        )?;
        conn.execute(
            "CREATE TABLE IF NOT EXISTS context_item_events (
                id INTEGER PRIMARY KEY,
                timestamp TEXT NOT NULL,
                project_path TEXT NOT NULL DEFAULT '',
                event_type TEXT NOT NULL,
                packet_id TEXT NOT NULL,
                runtime_source TEXT NOT NULL DEFAULT '',
                runtime_session_id TEXT,
                runtime_thread_id TEXT,
                runtime_turn_id TEXT,
                item_id TEXT NOT NULL,
                section TEXT NOT NULL,
                kind TEXT NOT NULL,
                summary TEXT NOT NULL,
                token_estimate INTEGER NOT NULL,
                score INTEGER NOT NULL,
                artifact_id TEXT,
                subject TEXT,
                provenance_json TEXT NOT NULL
            )",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_context_items_project_timestamp ON context_item_events(project_path, timestamp)",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_context_items_thread_timestamp ON context_item_events(runtime_thread_id, timestamp)",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_context_items_packet_id ON context_item_events(packet_id)",
            [],
        )?;
        let _ = conn.execute(
            "ALTER TABLE context_events ADD COLUMN canonical_input_tokens INTEGER",
            [],
        );
        let _ = conn.execute(
            "ALTER TABLE context_events ADD COLUMN canonical_output_tokens INTEGER",
            [],
        );
        conn.execute(
            &format!(
                "UPDATE context_events
                 SET canonical_input_tokens = {input_sql},
                     canonical_output_tokens = {output_sql},
                     saved_tokens = {saved_sql}
                 WHERE canonical_input_tokens IS NULL
                    OR canonical_output_tokens IS NULL",
                input_sql = MUNIN_CANONICAL_INPUT_SQL,
                output_sql = MUNIN_CANONICAL_OUTPUT_SQL,
                saved_sql = MUNIN_CANONICAL_SAVED_SQL,
            ),
            [],
        )?;

        conn.execute(
            "CREATE TABLE IF NOT EXISTS claim_leases (
                id INTEGER PRIMARY KEY,
                timestamp TEXT NOT NULL,
                project_path TEXT NOT NULL DEFAULT '',
                claim_type TEXT NOT NULL,
                claim_text TEXT NOT NULL,
                rationale_capsule TEXT,
                confidence TEXT NOT NULL,
                status TEXT NOT NULL,
                scope_key TEXT,
                dependencies_json TEXT NOT NULL,
                dependency_fingerprint TEXT NOT NULL,
                evidence_json TEXT NOT NULL,
                source_kind TEXT NOT NULL,
                review_after TEXT,
                expires_at TEXT,
                last_reviewed_at TEXT,
                demotion_reason TEXT
            )",
            [],
        )?;
        let _ = conn.execute("ALTER TABLE claim_leases ADD COLUMN review_after TEXT", []);
        let _ = conn.execute("ALTER TABLE claim_leases ADD COLUMN expires_at TEXT", []);
        let _ = conn.execute(
            "ALTER TABLE claim_leases ADD COLUMN last_reviewed_at TEXT",
            [],
        );
        let _ = conn.execute(
            "ALTER TABLE claim_leases ADD COLUMN demotion_reason TEXT",
            [],
        );
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_claim_leases_project_timestamp ON claim_leases(project_path, timestamp)",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_claim_leases_project_status_timestamp ON claim_leases(project_path, status, timestamp)",
            [],
        )?;

        conn.execute(
            "CREATE TABLE IF NOT EXISTS user_decisions (
                id INTEGER PRIMARY KEY,
                timestamp TEXT NOT NULL,
                project_path TEXT NOT NULL DEFAULT '',
                decision_key TEXT NOT NULL,
                value_text TEXT NOT NULL,
                fingerprint TEXT NOT NULL
            )",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_user_decisions_project_key_timestamp ON user_decisions(project_path, decision_key, timestamp)",
            [],
        )?;

        conn.execute(
            "CREATE TABLE IF NOT EXISTS approval_jobs (
                job_id TEXT PRIMARY KEY,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                project_path TEXT NOT NULL DEFAULT '',
                scope TEXT NOT NULL,
                scope_target TEXT,
                local_date TEXT NOT NULL,
                item_id TEXT,
                item_kind TEXT NOT NULL,
                title TEXT NOT NULL,
                summary TEXT NOT NULL,
                status TEXT NOT NULL,
                source_kind TEXT NOT NULL,
                provider TEXT,
                continuity_active INTEGER NOT NULL DEFAULT 0,
                expected_effect TEXT,
                queue_path TEXT,
                result_path TEXT,
                evidence_json TEXT NOT NULL,
                review_after TEXT,
                expires_at TEXT,
                last_reviewed_at TEXT,
                closure_reason TEXT
            )",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_approval_jobs_project_status_updated ON approval_jobs(project_path, status, updated_at)",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_approval_jobs_scope_date ON approval_jobs(scope, local_date, item_kind, item_id)",
            [],
        )?;

        conn.execute(
            "CREATE TABLE IF NOT EXISTS maintenance_state (
                key TEXT PRIMARY KEY,
                value_text TEXT NOT NULL
            )",
            [],
        )?;

        conn.execute(
            "CREATE TABLE IF NOT EXISTS memory_os_journal_events (
                journal_seq INTEGER PRIMARY KEY AUTOINCREMENT,
                event_id TEXT NOT NULL UNIQUE,
                stream_id TEXT NOT NULL,
                stream_revision INTEGER NOT NULL,
                expected_stream_revision INTEGER,
                tx_index INTEGER NOT NULL DEFAULT 0,
                occurred_at TEXT NOT NULL,
                committed_at TEXT NOT NULL,
                event_kind TEXT NOT NULL,
                idempotency_key TEXT NOT NULL,
                idempotency_receipt_id TEXT,
                project_path TEXT NOT NULL DEFAULT '',
                scope_json TEXT NOT NULL,
                actor_json TEXT NOT NULL,
                target_refs_json TEXT NOT NULL,
                payload_json TEXT NOT NULL,
                proof_refs_json TEXT NOT NULL,
                precondition_hash TEXT,
                result_hash TEXT,
                schema_fingerprint TEXT NOT NULL
            )",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_memory_os_journal_stream_revision ON memory_os_journal_events(stream_id, stream_revision)",
            [],
        )?;
        conn.execute(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_memory_os_journal_stream_revision_unique ON memory_os_journal_events(stream_id, stream_revision)",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_memory_os_journal_project_seq ON memory_os_journal_events(project_path, journal_seq)",
            [],
        )?;

        conn.execute(
            "CREATE TABLE IF NOT EXISTS memory_os_idempotency_receipts (
                idempotency_key TEXT PRIMARY KEY,
                payload_hash TEXT NOT NULL,
                first_event_id TEXT NOT NULL,
                created_at TEXT NOT NULL
            )",
            [],
        )?;

        conn.execute(
            "CREATE TABLE IF NOT EXISTS memory_os_projection_checkpoints (
                id INTEGER PRIMARY KEY,
                projection_name TEXT NOT NULL,
                project_path TEXT NOT NULL DEFAULT '',
                from_seq INTEGER NOT NULL,
                to_seq INTEGER NOT NULL,
                rebuild_kind TEXT NOT NULL,
                created_at TEXT NOT NULL
            )",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_memory_os_projection_checkpoints_project_name ON memory_os_projection_checkpoints(project_path, projection_name, created_at)",
            [],
        )?;

        conn.execute(
            "CREATE TABLE IF NOT EXISTS memory_os_verification_results (
                id INTEGER PRIMARY KEY,
                verification_result_id TEXT NOT NULL UNIQUE,
                proof_id TEXT NOT NULL,
                scope_json TEXT NOT NULL,
                verifier_id TEXT NOT NULL,
                verifier_version TEXT NOT NULL,
                trusted_root_id TEXT,
                trusted_producer_ids_json TEXT NOT NULL,
                materials_hashes_json TEXT NOT NULL,
                products_hashes_json TEXT NOT NULL,
                verification_time TEXT NOT NULL,
                result TEXT NOT NULL,
                reason TEXT,
                attestation_kind TEXT NOT NULL
            )",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_memory_os_verification_proof ON memory_os_verification_results(proof_id, verification_time)",
            [],
        )?;

        conn.execute(
            "CREATE TABLE IF NOT EXISTS memory_os_policy_models (
                id INTEGER PRIMARY KEY,
                policy_model_id TEXT NOT NULL UNIQUE,
                version TEXT NOT NULL,
                description TEXT NOT NULL,
                created_at TEXT NOT NULL
            )",
            [],
        )?;

        conn.execute(
            "CREATE TABLE IF NOT EXISTS memory_os_access_rules (
                id INTEGER PRIMARY KEY,
                access_rule_id TEXT NOT NULL UNIQUE,
                policy_model_id TEXT NOT NULL,
                subject_predicate TEXT NOT NULL,
                object_predicate TEXT NOT NULL,
                environment_predicate TEXT NOT NULL,
                action TEXT NOT NULL,
                effect TEXT NOT NULL,
                priority INTEGER NOT NULL
            )",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_memory_os_access_rules_policy ON memory_os_access_rules(policy_model_id, priority)",
            [],
        )?;

        conn.execute(
            "CREATE TABLE IF NOT EXISTS memory_os_trust_observations (
                id INTEGER PRIMARY KEY,
                observation_id TEXT NOT NULL UNIQUE,
                project_path TEXT NOT NULL DEFAULT '',
                target_kind TEXT NOT NULL,
                target_ref TEXT NOT NULL,
                action_kind TEXT NOT NULL,
                decision TEXT NOT NULL,
                reason_json TEXT NOT NULL,
                read_seq_cut INTEGER,
                policy_model_id TEXT,
                sensitivity_class TEXT NOT NULL,
                contains_secret INTEGER NOT NULL DEFAULT 0,
                contains_pii INTEGER NOT NULL DEFAULT 0,
                must_not_packetize INTEGER NOT NULL DEFAULT 0,
                taint_state TEXT NOT NULL,
                observed_at TEXT NOT NULL
            )",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_memory_os_trust_observations_target ON memory_os_trust_observations(project_path, target_kind, target_ref, observed_at)",
            [],
        )?;

        conn.execute(
            "CREATE TABLE IF NOT EXISTS memory_os_action_observations (
                observation_id TEXT PRIMARY KEY,
                project_path TEXT NOT NULL DEFAULT '',
                source_kind TEXT NOT NULL,
                source_event_id TEXT,
                cue_fingerprint TEXT NOT NULL,
                action_fingerprint TEXT NOT NULL,
                cue_json TEXT NOT NULL,
                action_json TEXT NOT NULL,
                source_ref TEXT NOT NULL,
                observed_at TEXT NOT NULL
            )",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_memory_os_action_observations_project ON memory_os_action_observations(project_path, cue_fingerprint, action_fingerprint, observed_at)",
            [],
        )?;

        conn.execute(
            "CREATE TABLE IF NOT EXISTS memory_os_action_executions (
                execution_id TEXT PRIMARY KEY,
                project_path TEXT NOT NULL DEFAULT '',
                execution_kind TEXT NOT NULL,
                command_sig TEXT NOT NULL,
                subject_ref TEXT,
                exit_code INTEGER NOT NULL,
                observed_at TEXT NOT NULL
            )",
            [],
        )?;
        let _ = conn.execute(
            "ALTER TABLE memory_os_action_executions ADD COLUMN subject_ref TEXT",
            [],
        );
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_memory_os_action_executions_project ON memory_os_action_executions(project_path, command_sig, observed_at)",
            [],
        )?;

        Ok(Self { conn })
    }

    #[cfg(test)]
    pub(crate) fn new_at_path(db_path: &Path) -> Result<Self> {
        Self::open_at_path(db_path)
    }

    /// Record a command execution with token counts and timing.
    ///
    /// Calculates savings metrics and stores the record in the database.
    /// Automatically cleans up records older than 90 days after insertion.
    ///
    /// # Arguments
    ///
    /// - `original_cmd`: The standard command (e.g., "ls -la")
    /// - `context_cmd`: The Context command used (e.g., "context ls")
    /// - `input_tokens`: Estimated tokens from standard command output
    /// - `output_tokens`: Actual tokens from Context output
    /// - `exec_time_ms`: Execution time in milliseconds
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use munin_memory::tracking::Tracker;
    ///
    /// let tracker = Tracker::new()?;
    /// tracker.record("ls -la", "context ls", 1000, 200, 50)?;
    /// # Ok::<(), anyhow::Error>(())
    /// ```
    pub fn record(
        &self,
        original_cmd: &str,
        context_cmd: &str,
        input_tokens: usize,
        output_tokens: usize,
        exec_time_ms: u64,
    ) -> Result<()> {
        if is_assistant_housekeeping_command(original_cmd) {
            return Ok(());
        }

        let saved = input_tokens.saturating_sub(output_tokens);
        let pct = if input_tokens > 0 {
            (saved as f64 / input_tokens as f64) * 100.0
        } else {
            0.0
        };

        let project_path = current_project_path_string(); // added: record cwd

        self.conn.execute(
            "INSERT INTO commands (timestamp, original_cmd, context_cmd, project_path, input_tokens, output_tokens, saved_tokens, savings_pct, exec_time_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)", // added: project_path
            params![
                Utc::now().to_rfc3339(),
                original_cmd,
                context_cmd,
                project_path, // added
                input_tokens as i64,
                output_tokens as i64,
                saved as i64,
                pct,
                exec_time_ms as i64
            ],
        )?;

        self.cleanup_old_if_due()?;
        Ok(())
    }

    pub fn record_artifact_event(
        &self,
        command_sig: &str,
        artifact_id: &str,
        source_layer: &str,
        event_type: &str,
        input_tokens: usize,
        output_tokens: usize,
    ) -> Result<()> {
        let saved = input_tokens.saturating_sub(output_tokens);
        let project_path = current_project_path_string();
        self.conn.execute(
            "INSERT INTO artifact_events (timestamp, project_path, command_sig, artifact_id, source_layer, event_type, input_tokens, output_tokens, saved_tokens)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                Utc::now().to_rfc3339(),
                project_path,
                command_sig,
                artifact_id,
                source_layer,
                event_type,
                input_tokens as i64,
                output_tokens as i64,
                saved as i64,
            ],
        )?;
        self.cleanup_old_if_due()?;
        Ok(())
    }

    pub fn record_context_event(&self, event_type: &str, stats: ContextEventStats) -> Result<()> {
        let canonical_input_tokens = if context_event_has_reused_state(&stats) {
            stats.estimated_source_tokens
        } else {
            0
        };
        let canonical_output_tokens = stats.rendered_tokens.min(canonical_input_tokens);
        let saved = canonical_input_tokens.saturating_sub(canonical_output_tokens);
        let project_path = current_project_path_string();
        self.conn.execute(
            "INSERT INTO context_events (
                timestamp, project_path, event_type, rendered_tokens, estimated_source_tokens,
                saved_tokens, canonical_input_tokens, canonical_output_tokens,
                current_fact_count, recent_change_count, live_claim_count,
                open_obligation_count, artifact_handle_count, failure_count
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            params![
                Utc::now().to_rfc3339(),
                project_path,
                event_type,
                stats.rendered_tokens as i64,
                stats.estimated_source_tokens as i64,
                saved as i64,
                canonical_input_tokens as i64,
                canonical_output_tokens as i64,
                stats.current_fact_count as i64,
                stats.recent_change_count as i64,
                stats.live_claim_count as i64,
                stats.open_obligation_count as i64,
                stats.artifact_handle_count as i64,
                stats.failure_count as i64,
            ],
        )?;
        let runtime_context_id = self.conn.last_insert_rowid();
        let _ = self.record_memory_os_shadow_event(MemoryOsShadowEvent {
            event_id: format!("munin-runtime-context-{runtime_context_id}"),
            stream_id: format!("munin.runtime-context:{}", project_path),
            stream_revision: 0,
            expected_stream_revision: None,
            tx_index: 0,
            event_kind: format!("munin.runtime-context-event.{}", event_type),
            idempotency_key: format!("munin.runtime-context:rowid:{runtime_context_id}"),
            idempotency_receipt_id: None,
            project_path: project_path.clone(),
            scope_json: serde_json::json!({
                "repo_id": project_path,
                "branch_id": "",
                "worktree_id": "",
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
            target_refs_json: "[]".to_string(),
            payload_json: serde_json::json!({
                "rendered_tokens": stats.rendered_tokens,
                "estimated_source_tokens": stats.estimated_source_tokens,
                "current_fact_count": stats.current_fact_count,
                "recent_change_count": stats.recent_change_count,
                "live_claim_count": stats.live_claim_count,
                "open_obligation_count": stats.open_obligation_count,
                "artifact_handle_count": stats.artifact_handle_count,
                "failure_count": stats.failure_count
            })
            .to_string(),
            proof_refs_json: "[]".to_string(),
            precondition_hash: None,
            result_hash: None,
            schema_fingerprint: "memoryos-shadow-v1".into(),
        });
        self.cleanup_old_if_due()?;
        Ok(())
    }

    pub fn record_context_item_events(
        &self,
        event_type: &str,
        packet_id: &str,
        runtime: &ContextRuntimeInfo,
        items: &[ContextSelectedItemRecord],
    ) -> Result<()> {
        if items.is_empty() {
            return Ok(());
        }

        let timestamp = Utc::now().to_rfc3339();
        let project_path = current_project_path_string();
        let mut stmt = self.conn.prepare(
            "INSERT INTO context_item_events (
                timestamp, project_path, event_type, packet_id,
                runtime_source, runtime_session_id, runtime_thread_id, runtime_turn_id,
                item_id, section, kind, summary, token_estimate, score,
                artifact_id, subject, provenance_json
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
        )?;

        for item in items {
            stmt.execute(params![
                timestamp,
                project_path,
                event_type,
                packet_id,
                runtime.source,
                runtime.session_id,
                runtime.thread_id,
                runtime.turn_id,
                item.item_id,
                item.section,
                item.kind,
                item.summary,
                item.token_estimate as i64,
                item.score,
                item.artifact_id,
                item.subject,
                serde_json::to_string(&item.provenance)?,
            ])?;
        }

        self.cleanup_old_if_due()?;
        Ok(())
    }

    pub fn record_worldview_event(
        &self,
        event_type: &str,
        subject_key: &str,
        command_sig: &str,
        summary: &str,
        fingerprint: &str,
        artifact_id: Option<&str>,
        payload_json: &str,
    ) -> Result<String> {
        let project_path = current_project_path_string();
        self.record_worldview_event_for_project(
            &project_path,
            event_type,
            subject_key,
            command_sig,
            summary,
            fingerprint,
            artifact_id,
            payload_json,
        )
    }

    pub(crate) fn record_worldview_event_for_project(
        &self,
        project_path: &str,
        event_type: &str,
        subject_key: &str,
        command_sig: &str,
        summary: &str,
        fingerprint: &str,
        artifact_id: Option<&str>,
        payload_json: &str,
    ) -> Result<String> {
        self.insert_worldview_event(
            project_path,
            event_type,
            subject_key,
            command_sig,
            summary,
            fingerprint,
            artifact_id,
            payload_json,
        )
    }

    fn insert_worldview_event(
        &self,
        project_path: &str,
        event_type: &str,
        subject_key: &str,
        command_sig: &str,
        summary: &str,
        fingerprint: &str,
        artifact_id: Option<&str>,
        payload_json: &str,
    ) -> Result<String> {
        let previous_fingerprint: Option<String> = self
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
            .ok();

        let status = match previous_fingerprint.as_deref() {
            None => "new",
            Some(previous) if previous == fingerprint => "unchanged",
            Some(_) => "changed",
        };

        self.conn.execute(
            "INSERT INTO worldview_events (
                timestamp, project_path, event_type, subject_key, command_sig,
                summary, fingerprint, status, artifact_id, payload_json
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                Utc::now().to_rfc3339(),
                project_path,
                event_type,
                subject_key,
                command_sig,
                summary,
                fingerprint,
                status,
                artifact_id,
                payload_json,
            ],
        )?;
        let munin_worldview_id = self.conn.last_insert_rowid();
        let _ = self.record_memory_os_shadow_event(MemoryOsShadowEvent {
            event_id: format!("munin-worldview-{munin_worldview_id}"),
            stream_id: format!("munin.worldview:{}:{}", project_path, subject_key),
            stream_revision: 0,
            expected_stream_revision: None,
            tx_index: 0,
            event_kind: format!("munin.worldview-event.{}", event_type),
            idempotency_key: format!("munin.worldview:rowid:{munin_worldview_id}"),
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
            target_refs_json: serde_json::json!([subject_key]).to_string(),
            payload_json: serde_json::json!({
                "command_sig": command_sig,
                "summary": summary,
                "fingerprint": fingerprint,
                "artifact_id": artifact_id,
                "payload_json": payload_json
            })
            .to_string(),
            proof_refs_json: "[]".to_string(),
            precondition_hash: previous_fingerprint.map(|p| hash_text(&p)),
            result_hash: Some(hash_text(fingerprint)),
            schema_fingerprint: "memoryos-shadow-v1".into(),
        });
        let trust_payload = format!("{summary}\n{payload_json}");
        let _ = self.observe_memory_os_payload_for_project(
            project_path,
            "worldview",
            subject_key,
            "packetize",
            &trust_payload,
        );

        self.cleanup_old_if_due()?;
        Ok(status.to_string())
    }

    pub fn record_worldview_replay_event_for_project(
        &self,
        project_path: &str,
        event_type: &str,
        subject_key: &str,
        command_sig: &str,
        summary: &str,
        fingerprint: &str,
        payload_json: &str,
    ) -> Result<String> {
        let existing: Option<String> = self
            .conn
            .query_row(
                "SELECT status
                 FROM worldview_events
                 WHERE project_path = ?1
                   AND event_type = ?2
                   AND subject_key = ?3
                   AND command_sig = ?4
                   AND fingerprint = ?5
                   AND payload_json = ?6
                 LIMIT 1",
                params![
                    project_path,
                    event_type,
                    subject_key,
                    command_sig,
                    fingerprint,
                    payload_json
                ],
                |row| row.get(0),
            )
            .ok();
        if let Some(status) = existing {
            return Ok(status);
        }

        self.record_worldview_event_for_project(
            project_path,
            event_type,
            subject_key,
            command_sig,
            summary,
            fingerprint,
            None,
            payload_json,
        )
    }

    pub(super) fn cleanup_old_if_due(&self) -> Result<()> {
        let now = Utc::now();
        if let Some(last_run) = self.read_maintenance_timestamp(MAINTENANCE_KEY_CLEANUP_OLD)? {
            if (now - last_run).num_seconds() < CLEANUP_INTERVAL_SECS {
                return Ok(());
            }
        }

        self.cleanup_old_at(now)?;
        self.write_maintenance_timestamp(MAINTENANCE_KEY_CLEANUP_OLD, now)?;
        Ok(())
    }

    fn read_maintenance_timestamp(&self, key: &str) -> Result<Option<DateTime<Utc>>> {
        let value_text = self
            .conn
            .query_row(
                "SELECT value_text FROM maintenance_state WHERE key = ?1",
                params![key],
                |row| row.get::<_, String>(0),
            )
            .ok();

        value_text
            .map(|value| {
                chrono::DateTime::parse_from_rfc3339(&value)
                    .map(|parsed| parsed.with_timezone(&Utc))
                    .map_err(Into::into)
            })
            .transpose()
    }

    fn write_maintenance_timestamp(&self, key: &str, timestamp: DateTime<Utc>) -> Result<()> {
        self.conn.execute(
            "INSERT INTO maintenance_state (key, value_text)
             VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value_text = excluded.value_text",
            params![key, timestamp.to_rfc3339()],
        )?;
        Ok(())
    }

    fn cleanup_old_at(&self, now: DateTime<Utc>) -> Result<()> {
        let cutoff = now - chrono::Duration::days(DEFAULT_HISTORY_DAYS);
        self.conn.execute(
            "DELETE FROM commands WHERE timestamp < ?1",
            params![cutoff.to_rfc3339()],
        )?;
        self.conn.execute(
            "DELETE FROM parse_failures WHERE timestamp < ?1",
            params![cutoff.to_rfc3339()],
        )?;
        self.conn.execute(
            "DELETE FROM artifact_events WHERE timestamp < ?1",
            params![cutoff.to_rfc3339()],
        )?;
        self.conn.execute(
            "DELETE FROM context_events WHERE timestamp < ?1",
            params![cutoff.to_rfc3339()],
        )?;
        self.conn.execute(
            "DELETE FROM context_item_events WHERE timestamp < ?1",
            params![cutoff.to_rfc3339()],
        )?;
        self.conn.execute(
            "DELETE FROM worldview_events WHERE timestamp < ?1",
            params![cutoff.to_rfc3339()],
        )?;
        Ok(())
    }

    pub fn get_artifact_events_filtered_between(
        &self,
        project_path: Option<&str>,
        since: chrono::DateTime<chrono::Utc>,
        until: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<ArtifactEventRecord>> {
        let (project_exact, project_glob) = project_filter_params(project_path);
        let mut stmt = self.conn.prepare(
            "SELECT timestamp, project_path, command_sig, artifact_id, source_layer, event_type, input_tokens, output_tokens, saved_tokens
             FROM artifact_events
             WHERE timestamp >= ?3
               AND timestamp <= ?4
               AND (?1 IS NULL OR project_path = ?1 OR project_path GLOB ?2)
             ORDER BY timestamp ASC, id ASC",
        )?;
        let rows = stmt.query_map(
            params![
                project_exact,
                project_glob,
                since.to_rfc3339(),
                until.to_rfc3339()
            ],
            |row| {
                Ok(ArtifactEventRecord {
                    timestamp: parse_rfc3339_to_utc(&row.get::<_, String>(0)?),
                    project_path: row.get(1)?,
                    command_sig: row.get(2)?,
                    artifact_id: row.get(3)?,
                    source_layer: row.get(4)?,
                    event_type: row.get(5)?,
                    input_tokens: row.get::<_, i64>(6)? as usize,
                    output_tokens: row.get::<_, i64>(7)? as usize,
                    saved_tokens: row.get::<_, i64>(8)? as usize,
                })
            },
        )?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn get_context_item_events_filtered(
        &self,
        project_path: Option<&str>,
        since: Option<DateTime<Utc>>,
    ) -> Result<Vec<ContextItemEventRow>> {
        let (project_exact, project_glob) = project_filter_params(project_path);
        let query = if since.is_some() {
            "SELECT timestamp, project_path, event_type, packet_id, runtime_source,
                    runtime_session_id, runtime_thread_id, runtime_turn_id,
                    item_id, section, kind, summary, artifact_id, subject
             FROM context_item_events
             WHERE timestamp >= ?3
               AND (?1 IS NULL OR project_path = ?1 OR project_path GLOB ?2)
             ORDER BY timestamp ASC, id ASC"
        } else {
            "SELECT timestamp, project_path, event_type, packet_id, runtime_source,
                    runtime_session_id, runtime_thread_id, runtime_turn_id,
                    item_id, section, kind, summary, artifact_id, subject
             FROM context_item_events
             WHERE (?1 IS NULL OR project_path = ?1 OR project_path GLOB ?2)
             ORDER BY timestamp ASC, id ASC"
        };

        let mut stmt = self.conn.prepare(query)?;
        let mut rows = if let Some(cutoff) = since {
            stmt.query_map(
                params![project_exact, project_glob, cutoff.to_rfc3339()],
                |row| {
                    Ok(ContextItemEventRow {
                        timestamp: parse_rfc3339_to_utc(&row.get::<_, String>(0)?),
                        project_path: row.get(1)?,
                        event_type: row.get(2)?,
                        packet_id: row.get(3)?,
                        runtime_source: row.get(4)?,
                        runtime_session_id: row.get(5)?,
                        runtime_thread_id: row.get(6)?,
                        runtime_turn_id: row.get(7)?,
                        item_id: row.get(8)?,
                        section: row.get(9)?,
                        kind: row.get(10)?,
                        summary: row.get(11)?,
                        artifact_id: row.get(12)?,
                        subject: row.get(13)?,
                    })
                },
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?
        } else {
            stmt.query_map(params![project_exact, project_glob], |row| {
                Ok(ContextItemEventRow {
                    timestamp: parse_rfc3339_to_utc(&row.get::<_, String>(0)?),
                    project_path: row.get(1)?,
                    event_type: row.get(2)?,
                    packet_id: row.get(3)?,
                    runtime_source: row.get(4)?,
                    runtime_session_id: row.get(5)?,
                    runtime_thread_id: row.get(6)?,
                    runtime_turn_id: row.get(7)?,
                    item_id: row.get(8)?,
                    section: row.get(9)?,
                    kind: row.get(10)?,
                    summary: row.get(11)?,
                    artifact_id: row.get(12)?,
                    subject: row.get(13)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
        };

        rows.sort_by(|left, right| {
            left.timestamp
                .cmp(&right.timestamp)
                .then_with(|| left.packet_id.cmp(&right.packet_id))
                .then_with(|| left.item_id.cmp(&right.item_id))
        });
        Ok(rows)
    }

    /// Get recent command history.
    ///
    /// Returns up to `limit` most recent command records, ordered by timestamp (newest first).
    ///
    /// # Arguments
    ///
    /// - `limit`: Maximum number of records to return
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use munin_memory::tracking::Tracker;
    ///
    /// let tracker = Tracker::new()?;
    /// let recent = tracker.get_recent(10)?;
    /// for cmd in recent {
    ///     println!("{}: {} saved {:.1}%",
    ///         cmd.timestamp, cmd.context_cmd, cmd.savings_pct);
    /// }
    /// # Ok::<(), anyhow::Error>(())
    /// ```
    #[allow(dead_code)]
    pub fn get_recent(&self, limit: usize) -> Result<Vec<CommandRecord>> {
        self.get_recent_filtered(limit, None) // delegate to filtered variant
    }

    /// Get recent command history filtered by project path. // added
    pub fn get_recent_filtered(
        &self,
        limit: usize,
        project_path: Option<&str>,
    ) -> Result<Vec<CommandRecord>> {
        let (project_exact, project_glob) = project_filter_params(project_path); // added
        let project_hint = command_project_hint(project_path);
        let query = format!(
            "SELECT timestamp, context_cmd, input_tokens, output_tokens, saved_tokens, savings_pct
             FROM commands
             WHERE ((?1 IS NULL OR project_path = ?1 OR project_path GLOB ?2)
               OR instr(original_cmd, ?3) > 0
               OR instr(context_cmd, ?3) > 0)
               AND {}
             ORDER BY timestamp DESC
             LIMIT ?4",
            COMMAND_TELEMETRY_FILTER_SQL
        );
        let mut stmt = self.conn.prepare(&query)?;

        let rows = stmt.query_map(
            params![project_exact, project_glob, project_hint, limit as i64],
            |row| {
                Ok(CommandRecord {
                    timestamp: DateTime::parse_from_rfc3339(&row.get::<_, String>(0)?)
                        .map(|dt| dt.with_timezone(&Utc))
                        .unwrap_or_else(|_| Utc::now()),
                    context_cmd: row.get(1)?,
                    input_tokens: row.get::<_, i64>(2)? as usize,
                    output_tokens: row.get::<_, i64>(3)? as usize,
                    saved_tokens: row.get::<_, i64>(4)? as usize,
                    savings_pct: row.get(5)?,
                })
            },
        )?;

        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn get_recent_filtered_since(
        &self,
        limit: usize,
        project_path: Option<&str>,
        since: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<CommandRecord>> {
        let cutoff = since.to_rfc3339();
        let (project_exact, project_glob) = project_filter_params(project_path);
        let project_hint = command_project_hint(project_path);
        let query = format!(
            "SELECT timestamp, context_cmd, input_tokens, output_tokens, saved_tokens, savings_pct
             FROM commands
             WHERE timestamp >= ?4
               AND ((?1 IS NULL OR project_path = ?1 OR project_path GLOB ?2)
               OR instr(original_cmd, ?3) > 0
               OR instr(context_cmd, ?3) > 0)
               AND {}
             ORDER BY timestamp DESC
             LIMIT ?5",
            COMMAND_TELEMETRY_FILTER_SQL
        );
        let mut stmt = self.conn.prepare(&query)?;

        let rows = stmt.query_map(
            params![
                project_exact,
                project_glob,
                project_hint,
                cutoff,
                limit as i64
            ],
            |row| {
                Ok(CommandRecord {
                    timestamp: DateTime::parse_from_rfc3339(&row.get::<_, String>(0)?)
                        .map(|dt| dt.with_timezone(&Utc))
                        .unwrap_or_else(|_| Utc::now()),
                    context_cmd: row.get(1)?,
                    input_tokens: row.get::<_, i64>(2)? as usize,
                    output_tokens: row.get::<_, i64>(3)? as usize,
                    saved_tokens: row.get::<_, i64>(4)? as usize,
                    savings_pct: row.get(5)?,
                })
            },
        )?;

        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn get_worldview_events_filtered(
        &self,
        limit: usize,
        project_path: Option<&str>,
    ) -> Result<Vec<WorldviewRecord>> {
        let (project_exact, project_glob) = project_filter_params(project_path);
        let mut stmt = self.conn.prepare(
            "SELECT timestamp, event_type, subject_key, command_sig, summary, fingerprint, status, artifact_id, payload_json
             FROM worldview_events
             WHERE (?1 IS NULL OR project_path = ?1 OR project_path GLOB ?2)
             ORDER BY timestamp DESC
             LIMIT ?3",
        )?;

        let rows = stmt.query_map(params![project_exact, project_glob, limit as i64], |row| {
            Ok(WorldviewRecord {
                timestamp: DateTime::parse_from_rfc3339(&row.get::<_, String>(0)?)
                    .map(|dt| dt.with_timezone(&Utc))
                    .unwrap_or_else(|_| Utc::now()),
                event_type: row.get(1)?,
                subject_key: row.get(2)?,
                command_sig: row.get(3)?,
                summary: row.get(4)?,
                fingerprint: row.get(5)?,
                status: row.get(6)?,
                artifact_id: row.get(7)?,
                payload_json: row.get(8)?,
            })
        })?;

        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }
}

fn get_db_path() -> Result<PathBuf> {
    // Priority 1: Environment variable MUNIN_DB_PATH
    if let Ok(custom_path) = std::env::var("MUNIN_DB_PATH") {
        return Ok(PathBuf::from(custom_path));
    }

    // Priority 2: Configuration file
    if let Ok(config) = crate::core::config::Config::load() {
        if let Some(db_path) = config.tracking.database_path {
            return Ok(db_path);
        }
    }

    // Priority 3: Default platform-specific Munin location.
    Ok(crate::core::config::context_data_dir()?.join(HISTORY_DB))
}

/// Estimate token count from text using ~4 chars = 1 token heuristic.
///
/// This is a fast approximation suitable for tracking purposes.
/// For precise counts, integrate with your LLM's tokenizer API.
///
/// # Formula
///
/// `tokens = ceil(chars / 4)`
///
/// # Examples
///
/// ```
/// use munin_memory::tracking::estimate_tokens;
///
/// assert_eq!(estimate_tokens(""), 0);
/// assert_eq!(estimate_tokens("abcd"), 1);  // 4 chars = 1 token
/// assert_eq!(estimate_tokens("abcde"), 2); // 5 chars = ceil(1.25) = 2
/// assert_eq!(estimate_tokens("hello world"), 3); // 11 chars = ceil(2.75) = 3
/// ```
pub fn estimate_tokens(text: &str) -> usize {
    // ~4 chars per token on average
    (text.len() as f64 / 4.0).ceil() as usize
}

/// Helper struct for timing command execution
/// Helper for timing command execution and tracking results.
///
/// Preferred API for tracking commands. Automatically measures execution time
/// and records token savings. Use instead of the deprecated [`track`] function.
///
/// # Examples
///
/// ```no_run
/// use munin_memory::tracking::TimedExecution;
///
/// # fn execute_standard_command() -> anyhow::Result<String> { Ok(String::new()) }
/// # fn execute_munin_command() -> anyhow::Result<String> { Ok(String::new()) }
/// let timer = TimedExecution::start();
/// let input = execute_standard_command()?;
/// let output = execute_munin_command()?;
/// timer.track("ls -la", "munin resume", &input, &output);
/// # Ok::<(), anyhow::Error>(())
/// ```
pub struct TimedExecution {
    start: Instant,
}

impl TimedExecution {
    /// Start timing a command execution.
    ///
    /// Creates a new timer that starts measuring elapsed time immediately.
    /// Call [`track`](Self::track) or [`track_passthrough`](Self::track_passthrough)
    /// when the command completes.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use munin_memory::tracking::TimedExecution;
    ///
    /// let timer = TimedExecution::start();
    /// // ... execute command ...
    /// timer.track("cmd", "context cmd", "input", "output");
    /// ```
    pub fn start() -> Self {
        Self {
            start: Instant::now(),
        }
    }

    /// Track the command with elapsed time and token counts.
    ///
    /// Records the command execution with:
    /// - Elapsed time since [`start`](Self::start)
    /// - Token counts estimated from input/output strings
    /// - Calculated savings metrics
    ///
    /// # Arguments
    ///
    /// - `original_cmd`: Standard command (e.g., "ls -la")
    /// - `context_cmd`: Context command used (e.g., "context ls")
    /// - `input`: Standard command output (for token estimation)
    /// - `output`: Context command output (for token estimation)
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use munin_memory::tracking::TimedExecution;
    ///
    /// let timer = TimedExecution::start();
    /// let input = "long output...";
    /// let output = "short output";
    /// timer.track("ls -la", "context ls", input, output);
    /// ```
    pub fn track(&self, original_cmd: &str, context_cmd: &str, input: &str, output: &str) {
        let elapsed_ms = self.start.elapsed().as_millis() as u64;
        let input_tokens = estimate_tokens(input);
        let output_tokens = estimate_tokens(output);

        if let Ok(tracker) = Tracker::new() {
            let _ = tracker.record(
                original_cmd,
                context_cmd,
                input_tokens,
                output_tokens,
                elapsed_ms,
            );
        }
    }

    /// Track passthrough commands (timing-only, no token counting).
    ///
    /// For commands that stream output or run interactively where output
    /// cannot be captured. Records execution time but sets tokens to 0
    /// (does not dilute savings statistics).
    ///
    /// # Arguments
    ///
    /// - `original_cmd`: Standard command (e.g., "git tag --list")
    /// - `context_cmd`: Context command used (e.g., "context git tag --list")
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use munin_memory::tracking::TimedExecution;
    ///
    /// let timer = TimedExecution::start();
    /// // ... execute streaming command ...
    /// timer.track_passthrough("git tag", "context git tag");
    /// ```
    pub fn track_passthrough(&self, original_cmd: &str, context_cmd: &str) {
        let elapsed_ms = self.start.elapsed().as_millis() as u64;
        // input_tokens=0, output_tokens=0 won't dilute savings statistics
        if let Ok(tracker) = Tracker::new() {
            let _ = tracker.record(original_cmd, context_cmd, 0, 0, elapsed_ms);
        }
    }
}

/// Format OsString args for tracking display.
///
/// Joins arguments with spaces, converting each to UTF-8 (lossy).
/// Useful for displaying command arguments in tracking records.
///
/// # Examples
///
/// ```
/// use std::ffi::OsString;
/// use munin_memory::tracking::args_display;
///
/// let args = vec![OsString::from("status"), OsString::from("--short")];
/// assert_eq!(args_display(&args), "status --short");
/// ```
pub fn args_display(args: &[OsString]) -> String {
    args.iter()
        .map(|a| a.to_string_lossy())
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use tempfile::TempDir;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn temp_tracker() -> (TempDir, Tracker) {
        let tmp = TempDir::new().expect("temp dir");
        let db_path = tmp.path().join("tracking.db");
        let tracker = Tracker::new_at_path(&db_path).expect("tracker");
        (tmp, tracker)
    }

    #[test]
    fn resolved_project_path_canonicalizes_dot_and_relative_paths() {
        let _guard = ENV_LOCK.lock().expect("env lock");
        let original = std::env::current_dir().expect("cwd");
        let tmp = TempDir::new().expect("temp dir");
        std::fs::create_dir_all(tmp.path().join(".git")).expect("git marker");
        std::fs::create_dir_all(tmp.path().join("child")).expect("child dir");
        std::env::set_current_dir(tmp.path().join("child")).expect("set cwd");
        let expected_root = crate::core::utils::normalize_windows_path_string(
            tmp.path()
                .canonicalize()
                .expect("canonical temp")
                .to_string_lossy()
                .as_ref(),
        );
        let dot = resolved_project_path(Some("."));
        let parent = resolved_project_path(Some(".."));
        let implicit = resolved_project_path(None);
        std::env::set_current_dir(original).expect("restore cwd");

        assert_eq!(dot, expected_root);
        assert_eq!(parent, expected_root);
        assert_eq!(implicit, expected_root);
    }

    fn insert_command_row(tracker: &Tracker, timestamp: DateTime<Utc>, original_cmd: &str) {
        insert_command_metrics_row(tracker, timestamp, original_cmd, 10, 5, 5, 1);
    }

    fn insert_command_metrics_row(
        tracker: &Tracker,
        timestamp: DateTime<Utc>,
        original_cmd: &str,
        input_tokens: i64,
        output_tokens: i64,
        saved_tokens: i64,
        exec_time_ms: i64,
    ) {
        tracker
            .conn
            .execute(
                "INSERT INTO commands (
                    timestamp, original_cmd, context_cmd, project_path, input_tokens,
                    output_tokens, saved_tokens, savings_pct, exec_time_ms
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    timestamp.to_rfc3339(),
                    original_cmd,
                    format!("context {original_cmd}"),
                    "",
                    input_tokens,
                    output_tokens,
                    saved_tokens,
                    if input_tokens > 0 {
                        (saved_tokens as f64 / input_tokens as f64) * 100.0
                    } else {
                        0.0
                    },
                    exec_time_ms
                ],
            )
            .expect("insert command row");
    }

    fn command_row_count(tracker: &Tracker) -> i64 {
        tracker
            .conn
            .query_row("SELECT COUNT(*) FROM commands", [], |row| row.get(0))
            .expect("count commands")
    }

    fn insert_artifact_event_row(
        tracker: &Tracker,
        timestamp: DateTime<Utc>,
        event_type: &str,
        input_tokens: i64,
        output_tokens: i64,
    ) {
        tracker
            .conn
            .execute(
                "INSERT INTO artifact_events (
                    timestamp, project_path, command_sig, artifact_id, source_layer,
                    event_type, input_tokens, output_tokens, saved_tokens
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    timestamp.to_rfc3339(),
                    "",
                    "context cargo test --all",
                    format!("artifact-{event_type}"),
                    "test",
                    event_type,
                    input_tokens,
                    output_tokens,
                    input_tokens.saturating_sub(output_tokens),
                ],
            )
            .expect("insert artifact event row");
    }

    // 1. estimate_tokens Ã¢â‚¬â€ verify ~4 chars/token ratio
    #[test]
    fn test_estimate_tokens() {
        assert_eq!(estimate_tokens(""), 0);
        assert_eq!(estimate_tokens("abcd"), 1); // 4 chars = 1 token
        assert_eq!(estimate_tokens("abcde"), 2); // 5 chars = ceil(1.25) = 2
        assert_eq!(estimate_tokens("a"), 1); // 1 char = ceil(0.25) = 1
        assert_eq!(estimate_tokens("12345678"), 2); // 8 chars = 2 tokens
    }

    // 2. args_display Ã¢â‚¬â€ format OsString vec
    #[test]
    fn test_args_display() {
        let args = vec![OsString::from("status"), OsString::from("--short")];
        assert_eq!(args_display(&args), "status --short");
        assert_eq!(args_display(&[]), "");

        let single = vec![OsString::from("log")];
        assert_eq!(args_display(&single), "log");
    }

    // 3. Tracker::record + get_recent Ã¢â‚¬â€ round-trip DB
    #[test]
    fn test_tracker_record_and_recent() {
        let (_tmp, tracker) = temp_tracker();

        // Use unique test identifier to avoid conflicts with other tests
        let test_cmd = format!("context git status test_{}", std::process::id());

        tracker
            .record("git status", &test_cmd, 100, 20, 50)
            .expect("Failed to record");

        let recent = tracker.get_recent(10).expect("Failed to get recent");

        // Find our specific test record
        let test_record = recent
            .iter()
            .find(|r| r.context_cmd == test_cmd)
            .expect("Test record not found in recent commands");

        assert_eq!(test_record.saved_tokens, 80);
        assert_eq!(test_record.savings_pct, 80.0);
    }

    // 4. track_passthrough doesn't dilute stats (input=0, output=0)
    #[test]
    fn test_track_passthrough_no_dilution() {
        let (_tmp, tracker) = temp_tracker();

        // Use unique test identifiers
        let pid = std::process::id();
        let cmd1 = format!("context cmd1_test_{}", pid);
        let cmd2 = format!("context cmd2_passthrough_test_{}", pid);

        // Record one real command with 80% savings
        tracker
            .record("cmd1", &cmd1, 1000, 200, 10)
            .expect("Failed to record cmd1");

        // Record passthrough (0, 0)
        tracker
            .record("cmd2", &cmd2, 0, 0, 5)
            .expect("Failed to record passthrough");

        // Verify both records exist in recent history
        let recent = tracker.get_recent(20).expect("Failed to get recent");

        let record1 = recent
            .iter()
            .find(|r| r.context_cmd == cmd1)
            .expect("cmd1 record not found");
        let record2 = recent
            .iter()
            .find(|r| r.context_cmd == cmd2)
            .expect("passthrough record not found");

        // Verify cmd1 has 80% savings
        assert_eq!(record1.saved_tokens, 800);
        assert_eq!(record1.savings_pct, 80.0);

        // Verify passthrough has 0% savings
        assert_eq!(record2.saved_tokens, 0);
        assert_eq!(record2.savings_pct, 0.0);

        // This validates that passthrough (0 input, 0 output) doesn't dilute stats
        // because the savings calculation is correct for both cases
    }

    // 5. TimedExecution::track records with exec_time > 0
    #[test]
    fn test_timed_execution_records_time() {
        let _guard = ENV_LOCK.lock().expect("env lock");
        let tmp = TempDir::new().expect("temp dir");
        let db_path = tmp.path().join("tracking.db");
        std::env::set_var("MUNIN_DB_PATH", &db_path);

        let timer = TimedExecution::start();
        std::thread::sleep(std::time::Duration::from_millis(10));
        timer.track("test cmd", "context test", "raw input data", "filtered");

        // Verify via DB that record exists
        let tracker = Tracker::new_at_path(&db_path).expect("tracker");
        let recent = tracker.get_recent(5).expect("Failed to get recent");
        assert!(recent.iter().any(|r| r.context_cmd == "context test"));

        std::env::remove_var("MUNIN_DB_PATH");
    }

    // 6. TimedExecution::track_passthrough records with 0 tokens
    #[test]
    fn test_timed_execution_passthrough() {
        let _guard = ENV_LOCK.lock().expect("env lock");
        let tmp = TempDir::new().expect("temp dir");
        let db_path = tmp.path().join("tracking.db");
        std::env::set_var("MUNIN_DB_PATH", &db_path);

        let timer = TimedExecution::start();
        timer.track_passthrough("git tag", "context git tag (passthrough)");

        let tracker = Tracker::new_at_path(&db_path).expect("tracker");
        let recent = tracker.get_recent(5).expect("Failed to get recent");

        let pt = recent
            .iter()
            .find(|r| r.context_cmd.contains("passthrough"))
            .expect("Passthrough record not found");

        // savings_pct should be 0 for passthrough
        assert_eq!(pt.savings_pct, 0.0);
        assert_eq!(pt.saved_tokens, 0);

        std::env::remove_var("MUNIN_DB_PATH");
    }

    // 7. get_db_path respects environment variable MUNIN_DB_PATH
    #[test]
    fn test_custom_db_path_env() {
        use std::env;
        let _guard = ENV_LOCK.lock().expect("env lock");

        let custom_path = env::temp_dir().join("munin_test_custom.db");
        env::set_var("MUNIN_DB_PATH", &custom_path);

        let db_path = get_db_path().expect("Failed to get db path");
        assert_eq!(db_path, custom_path);

        env::remove_var("MUNIN_DB_PATH");
    }

    // 8. get_db_path falls back to default when no custom config
    #[test]
    fn test_default_db_path() {
        use std::env;
        let _guard = ENV_LOCK.lock().expect("env lock");

        // Ensure no env var is set
        env::remove_var("MUNIN_DB_PATH");

        let db_path = get_db_path().expect("Failed to get db path");
        assert_eq!(
            db_path.file_name().and_then(|name| name.to_str()),
            Some("history.db")
        );
        assert_eq!(
            db_path
                .parent()
                .and_then(|parent| parent.file_name())
                .and_then(|name| name.to_str()),
            Some("munin")
        );
    }

    // 9. project_filter_params uses GLOB pattern with * wildcard // added
    #[test]
    fn test_project_filter_params_glob_pattern() {
        let (exact, glob) = project_filter_params(Some("/home/user/project"));
        assert_eq!(exact.unwrap(), "/home/user/project");
        // Must use * (GLOB) not % (LIKE) for subdirectory prefix matching
        let glob_val = glob.unwrap();
        assert!(glob_val.ends_with('*'), "GLOB pattern must end with *");
        assert!(!glob_val.contains('%'), "Must not contain LIKE wildcard %");
        assert_eq!(
            glob_val,
            format!("/home/user/project{}*", std::path::MAIN_SEPARATOR)
        );
    }

    // 10. project_filter_params returns None for None input // added
    #[test]
    fn test_project_filter_params_none() {
        let (exact, glob) = project_filter_params(None);
        assert!(exact.is_none());
        assert!(glob.is_none());
    }

    // 11. GLOB pattern safe with underscores in path names // added
    #[test]
    fn test_project_filter_params_underscore_safe() {
        // In LIKE, _ matches any single char; in GLOB, _ is literal
        let (exact, glob) = project_filter_params(Some("/home/user/my_project"));
        assert_eq!(exact.unwrap(), "/home/user/my_project");
        let glob_val = glob.unwrap();
        // _ must be preserved literally (GLOB treats _ as literal, LIKE does not)
        assert!(glob_val.contains("my_project"));
        assert_eq!(
            glob_val,
            format!("/home/user/my_project{}*", std::path::MAIN_SEPARATOR)
        );
    }

    #[test]
    fn test_cleanup_old_if_due_prunes_stale_rows_and_sets_marker() {
        let (_tmp, tracker) = temp_tracker();
        let stale = Utc::now() - chrono::Duration::days(DEFAULT_HISTORY_DAYS + 1);
        insert_command_row(&tracker, stale, "stale-command");

        tracker.cleanup_old_if_due().expect("cleanup due");

        assert_eq!(command_row_count(&tracker), 0);
        assert!(tracker
            .read_maintenance_timestamp(MAINTENANCE_KEY_CLEANUP_OLD)
            .expect("read marker")
            .is_some());
    }

    #[test]
    fn test_cleanup_old_if_due_skips_recent_marker_until_interval_expires() {
        let (_tmp, tracker) = temp_tracker();
        let stale = Utc::now() - chrono::Duration::days(DEFAULT_HISTORY_DAYS + 1);
        insert_command_row(&tracker, stale, "stale-command");
        tracker
            .write_maintenance_timestamp(MAINTENANCE_KEY_CLEANUP_OLD, Utc::now())
            .expect("write fresh marker");

        tracker.cleanup_old_if_due().expect("skip recent cleanup");
        assert_eq!(command_row_count(&tracker), 1);

        let overdue = Utc::now() - chrono::Duration::seconds(CLEANUP_INTERVAL_SECS + 1);
        tracker
            .write_maintenance_timestamp(MAINTENANCE_KEY_CLEANUP_OLD, overdue)
            .expect("write overdue marker");
        tracker
            .cleanup_old_if_due()
            .expect("cleanup after interval");
        assert_eq!(command_row_count(&tracker), 0);
    }

    #[test]
    fn test_cleanup_old_if_due_keeps_durable_claims_and_user_decisions() {
        let (_tmp, tracker) = temp_tracker();
        let stale = Utc::now() - chrono::Duration::days(DEFAULT_HISTORY_DAYS + 1);
        tracker
            .conn
            .execute(
                "INSERT INTO claim_leases (
                    timestamp, project_path, claim_type, claim_text, rationale_capsule, confidence,
                    status, scope_key, dependencies_json, dependency_fingerprint, evidence_json, source_kind
                 ) VALUES (?1, '', 'decision', 'Durable claim', NULL, 'high', 'live', NULL, '[]', 'fp', '[]', 'test')",
                params![stale.to_rfc3339()],
            )
            .expect("insert claim");
        tracker
            .conn
            .execute(
                "INSERT INTO user_decisions (timestamp, project_path, decision_key, value_text, fingerprint)
                 VALUES (?1, '', 'queue-policy', 'manual-approval', 'fp')",
                params![stale.to_rfc3339()],
            )
            .expect("insert user decision");

        tracker.cleanup_old_if_due().expect("cleanup due");

        let claim_count: i64 = tracker
            .conn
            .query_row("SELECT COUNT(*) FROM claim_leases", [], |row| row.get(0))
            .expect("claim count");
        let decision_count: i64 = tracker
            .conn
            .query_row("SELECT COUNT(*) FROM user_decisions", [], |row| row.get(0))
            .expect("decision count");
        assert_eq!(claim_count, 1);
        assert_eq!(decision_count, 1);
    }

    #[test]
    fn test_gain_summary_ignores_assistant_housekeeping_commands() {
        let (_tmp, tracker) = temp_tracker();
        let now = Utc::now();
        insert_command_row(
            &tracker,
            now,
            "node C:/Users/OEM/.claude/hooks/context-context-hint.js",
        );
        insert_command_row(&tracker, now, "cargo test --all");

        let summary = tracker.get_summary_filtered(None).expect("summary");
        assert_eq!(summary.total_commands, 1);
        assert_eq!(summary.total_input, 10);
        assert_eq!(summary.total_output, 5);
        assert_eq!(summary.total_saved, 5);
        assert_eq!(summary.by_command.len(), 1);
        assert_eq!(summary.by_command[0].0, "context cargo test --all");
    }

    #[test]
    fn test_gain_summary_filters_sql_housekeeping_commands() {
        let (_tmp, tracker) = temp_tracker();
        let ts = DateTime::parse_from_rfc3339("2026-01-01T00:00:00+00:00")
            .expect("timestamp")
            .with_timezone(&Utc);
        insert_command_row(
            &tracker,
            ts,
            r#"bash -c FILE="$CLAUDE_FILE_PATH"; cat "$CLAUDE_FILE_PATH""#,
        );
        insert_command_row(
            &tracker,
            ts,
            r#"bash -c if echo "$CLAUDE_FILE_PATH" | grep -q ".md"; then cat "$CLAUDE_FILE_PATH"; fi"#,
        );
        insert_command_row(&tracker, ts, "cargo test --all");

        let summary = tracker.get_summary_filtered(None).expect("summary");
        assert_eq!(summary.total_commands, 1);
        assert_eq!(summary.total_input, 10);
        assert_eq!(summary.total_output, 5);
        assert_eq!(summary.total_saved, 5);
        assert_eq!(summary.by_command.len(), 1);
        assert_eq!(summary.by_command[0].0, "context cargo test --all");

        let recent = tracker.get_recent_filtered(10, None).expect("recent");
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].context_cmd, "context cargo test --all");

        let cutoff = ts - chrono::Duration::minutes(1);
        assert_eq!(tracker.count_commands_since(cutoff).expect("count"), 1);
        assert_eq!(tracker.top_commands(10).expect("top"), vec!["cargo"]);
        assert_eq!(tracker.total_tokens_saved().expect("saved"), 5);
        assert_eq!(tracker.overall_savings_pct().expect("pct"), 50.0);
    }

    #[test]
    fn test_worldview_status_transitions() {
        let (_tmp, tracker) = temp_tracker();
        let project = "C:/tmp/project";

        let first = tracker
            .record_worldview_event_for_project(
                project,
                "read",
                "file:demo.rs",
                "context read demo.rs",
                "first summary",
                "hash-a",
                None,
                "{}",
            )
            .expect("first");
        let second = tracker
            .record_worldview_event_for_project(
                project,
                "read",
                "file:demo.rs",
                "context read demo.rs",
                "second summary",
                "hash-a",
                None,
                "{}",
            )
            .expect("second");
        let third = tracker
            .record_worldview_event_for_project(
                project,
                "read",
                "file:demo.rs",
                "context read demo.rs",
                "third summary",
                "hash-b",
                None,
                "{}",
            )
            .expect("third");

        assert_eq!(first, "new");
        assert_eq!(second, "unchanged");
        assert_eq!(third, "changed");
    }

    #[test]
    fn test_get_worldview_events_filtered_roundtrip() {
        let (_tmp, tracker) = temp_tracker();
        let project = "C:/tmp/project";
        tracker
            .record_worldview_event_for_project(
                project,
                "grep",
                "grep:src:Tracker",
                "context grep Tracker",
                "grep summary",
                "hash-c",
                Some("@context/a_demo"),
                "{\"matches\":3}",
            )
            .expect("record");

        let events = tracker
            .get_worldview_events_filtered(10, Some(project))
            .expect("events");
        let event = events.first().expect("event");
        assert_eq!(event.event_type, "grep");
        assert_eq!(event.subject_key, "grep:src:Tracker");
        assert_eq!(event.artifact_id.as_deref(), Some("@context/a_demo"));
    }

    #[test]
    fn test_context_event_summary_roundtrip() {
        let (_tmp, tracker) = temp_tracker();
        tracker
            .record_context_event(
                "context",
                ContextEventStats {
                    rendered_tokens: 100,
                    estimated_source_tokens: 400,
                    current_fact_count: 3,
                    recent_change_count: 2,
                    live_claim_count: 1,
                    open_obligation_count: 0,
                    artifact_handle_count: 1,
                    failure_count: 0,
                },
            )
            .expect("context event");
        tracker
            .record_context_event(
                "resume",
                ContextEventStats {
                    rendered_tokens: 120,
                    estimated_source_tokens: 520,
                    current_fact_count: 4,
                    recent_change_count: 2,
                    live_claim_count: 2,
                    open_obligation_count: 1,
                    artifact_handle_count: 1,
                    failure_count: 2,
                },
            )
            .expect("resume event");

        let summary = tracker
            .get_context_summary_filtered(None)
            .expect("context summary");
        assert_eq!(summary.context_compilations, 2);
        assert_eq!(summary.context_reuse_saved, 700);
        assert_eq!(summary.claim_reuse_count, 3);
        assert_eq!(summary.failure_reuse_count, 2);
    }

    #[test]
    fn test_context_event_empty_packet_does_not_count_as_reuse() {
        let (_tmp, tracker) = temp_tracker();
        tracker
            .record_context_event(
                "context",
                ContextEventStats {
                    rendered_tokens: 670,
                    estimated_source_tokens: 243,
                    current_fact_count: 0,
                    recent_change_count: 0,
                    live_claim_count: 0,
                    open_obligation_count: 0,
                    artifact_handle_count: 0,
                    failure_count: 0,
                },
            )
            .expect("empty context event");

        let summary = tracker
            .get_context_summary_filtered(None)
            .expect("context summary");
        assert_eq!(summary.context_compilations, 1);
        assert_eq!(summary.estimated_source_tokens, 0);
        assert_eq!(summary.rendered_tokens, 0);
        assert_eq!(summary.context_reuse_saved, 0);

        let gain = tracker.get_summary_filtered(None).expect("gain summary");
        assert_eq!(gain.total_commands, 0);
        assert_eq!(gain.tracked_events, 1);
        assert_eq!(gain.total_input, 0);
        assert_eq!(gain.total_output, 0);
        assert_eq!(gain.total_saved, 0);
        assert_eq!(gain.context_reuse_saved, 0);
        assert_eq!(gain.avg_time_ms, 0);
    }

    #[test]
    fn test_context_event_migration_backfills_canonical_accounting() {
        let tmp = TempDir::new().expect("temp dir");
        let db_path = tmp.path().join("tracking.db");

        {
            let tracker = Tracker::new_at_path(&db_path).expect("tracker");
            tracker
                .conn
                .execute(
                    "INSERT INTO context_events (
                        timestamp, project_path, event_type, rendered_tokens, estimated_source_tokens,
                        saved_tokens, current_fact_count, recent_change_count, live_claim_count,
                        open_obligation_count, artifact_handle_count, failure_count
                    ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                    params![
                        "2026-01-01T00:01:00+00:00",
                        "",
                        "continue",
                        670_i64,
                        243_i64,
                        0_i64,
                        1_i64,
                        0_i64,
                        0_i64,
                        0_i64,
                        0_i64,
                        0_i64
                    ],
                )
                .expect("legacy context row");
        }

        let tracker = Tracker::new_at_path(&db_path).expect("reopened tracker");
        let (canonical_input, canonical_output, saved_tokens): (Option<i64>, Option<i64>, i64) =
            tracker
                .conn
                .query_row(
                    "SELECT canonical_input_tokens, canonical_output_tokens, saved_tokens
                     FROM context_events
                     LIMIT 1",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .expect("canonical row");
        assert_eq!(canonical_input, Some(243));
        assert_eq!(canonical_output, Some(243));
        assert_eq!(saved_tokens, 0);

        let summary = tracker
            .get_context_summary_filtered(None)
            .expect("context summary");
        assert_eq!(summary.estimated_source_tokens, 243);
        assert_eq!(summary.rendered_tokens, 243);
        assert_eq!(summary.context_reuse_saved, 0);
    }

    #[test]
    fn test_gain_aggregates_separate_command_counts_from_context_events() {
        let (_tmp, tracker) = temp_tracker();
        let project_path = current_project_path_string();
        insert_command_metrics_row(
            &tracker,
            DateTime::parse_from_rfc3339("2026-01-01T00:00:00+00:00")
                .expect("timestamp")
                .with_timezone(&Utc),
            "cargo test --all",
            1000,
            250,
            750,
            25,
        );
        tracker
            .conn
            .execute(
                "INSERT INTO context_events (
                    timestamp, project_path, event_type, rendered_tokens, estimated_source_tokens,
                    saved_tokens, current_fact_count, recent_change_count, live_claim_count,
                    open_obligation_count, artifact_handle_count, failure_count
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                params![
                    "2026-01-01T00:01:00+00:00",
                    project_path,
                    "continue",
                    100_i64,
                    400_i64,
                    300_i64,
                    1_i64,
                    1_i64,
                    0_i64,
                    0_i64,
                    0_i64,
                    0_i64
                ],
            )
            .expect("context row");

        let summary = tracker.get_summary_filtered(None).expect("summary");
        assert_eq!(summary.total_commands, 1);
        assert_eq!(summary.tracked_events, 2);
        assert_eq!(summary.total_input, 1400);
        assert_eq!(summary.total_output, 350);
        assert_eq!(summary.total_saved, 1050);
        assert_eq!(summary.context_reuse_saved, 300);
        assert_eq!(summary.context_compilations, 1);
        assert_eq!(summary.avg_time_ms, 25);

        let days = tracker.get_all_days_filtered(None).expect("days");
        assert_eq!(days.len(), 1);
        assert_eq!(days[0].commands, 1);
        assert_eq!(days[0].context_builds, 1);
        assert_eq!(days[0].tracked_events, 2);
        assert_eq!(days[0].input_tokens, 1400);
        assert_eq!(days[0].output_tokens, 350);
        assert_eq!(days[0].saved_tokens, 1050);
        assert_eq!(days[0].avg_time_ms, 25);

        let weeks = tracker.get_by_week_filtered(None).expect("weeks");
        assert_eq!(weeks.len(), 1);
        assert_eq!(weeks[0].commands, 1);
        assert_eq!(weeks[0].context_builds, 1);
        assert_eq!(weeks[0].tracked_events, 2);
        assert_eq!(weeks[0].saved_tokens, 1050);
        assert_eq!(weeks[0].avg_time_ms, 25);

        let months = tracker.get_by_month_filtered(None).expect("months");
        assert_eq!(months.len(), 1);
        assert_eq!(months[0].commands, 1);
        assert_eq!(months[0].context_builds, 1);
        assert_eq!(months[0].tracked_events, 2);
        assert_eq!(months[0].saved_tokens, 1050);
        assert_eq!(months[0].avg_time_ms, 25);

        assert_eq!(tracker.total_tokens_saved().expect("saved"), 1050);
        assert_eq!(tracker.overall_savings_pct().expect("pct"), 75.0);
        let since = Utc::now() - chrono::Duration::days(1);
        assert_eq!(tracker.tokens_saved_24h(since).expect("24h saved"), 0);
    }

    #[test]
    fn test_gain_summary_since_cutoff_separates_command_and_context_totals() {
        let (_tmp, tracker) = temp_tracker();
        let project_path = current_project_path_string();
        let cutoff = DateTime::parse_from_rfc3339("2026-01-01T12:00:00+00:00")
            .expect("cutoff")
            .with_timezone(&Utc);

        insert_command_metrics_row(
            &tracker,
            DateTime::parse_from_rfc3339("2026-01-01T11:59:00+00:00")
                .expect("before")
                .with_timezone(&Utc),
            "cargo test --all",
            10,
            5,
            5,
            1,
        );
        insert_command_metrics_row(
            &tracker,
            DateTime::parse_from_rfc3339("2026-01-01T12:01:00+00:00")
                .expect("after")
                .with_timezone(&Utc),
            "cargo clippy --all-targets",
            400,
            100,
            300,
            40,
        );
        tracker
            .conn
            .execute(
                "INSERT INTO context_events (
                    timestamp, project_path, event_type, rendered_tokens, estimated_source_tokens,
                    saved_tokens, current_fact_count, recent_change_count, live_claim_count,
                    open_obligation_count, artifact_handle_count, failure_count
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                params![
                    "2026-01-01T12:02:00+00:00",
                    project_path,
                    "continue",
                    20_i64,
                    120_i64,
                    100_i64,
                    1_i64,
                    0_i64,
                    0_i64,
                    0_i64,
                    0_i64,
                    0_i64
                ],
            )
            .expect("context row");

        let summary = tracker
            .get_summary_filtered_since(None, cutoff)
            .expect("summary since");
        assert_eq!(summary.total_commands, 1);
        assert_eq!(summary.total_input, 520);
        assert_eq!(summary.total_output, 120);
        assert_eq!(summary.total_saved, 400);
        assert_eq!(summary.context_compilations, 1);
        assert_eq!(summary.context_reuse_saved, 100);
        assert_eq!(summary.avg_time_ms, 40);

        let context = tracker
            .get_context_summary_filtered_since(None, cutoff)
            .expect("context since");
        assert_eq!(context.context_compilations, 1);
        assert_eq!(context.estimated_source_tokens, 120);
        assert_eq!(context.rendered_tokens, 20);
        assert_eq!(context.context_reuse_saved, 100);

        assert_eq!(tracker.tokens_saved_24h(cutoff).expect("24h saved"), 400);
    }

    #[test]
    fn test_gain_summary_between_uses_artifact_saved_tokens_for_replay_breakdown() {
        let (_tmp, tracker) = temp_tracker();
        let since = DateTime::parse_from_rfc3339("2026-01-01T12:00:00+00:00")
            .expect("since")
            .with_timezone(&Utc);
        let until = DateTime::parse_from_rfc3339("2026-01-01T13:00:00+00:00")
            .expect("until")
            .with_timezone(&Utc);

        insert_command_metrics_row(
            &tracker,
            DateTime::parse_from_rfc3339("2026-01-01T12:10:00+00:00")
                .expect("command ts")
                .with_timezone(&Utc),
            "cargo test --all",
            200,
            100,
            100,
            25,
        );
        insert_artifact_event_row(
            &tracker,
            DateTime::parse_from_rfc3339("2026-01-01T12:11:00+00:00")
                .expect("artifact new ts")
                .with_timezone(&Utc),
            "new",
            10,
            5,
        );
        insert_artifact_event_row(
            &tracker,
            DateTime::parse_from_rfc3339("2026-01-01T12:12:00+00:00")
                .expect("artifact unchanged ts")
                .with_timezone(&Utc),
            "unchanged",
            20,
            0,
        );
        insert_artifact_event_row(
            &tracker,
            DateTime::parse_from_rfc3339("2026-01-01T12:13:00+00:00")
                .expect("artifact delta ts")
                .with_timezone(&Utc),
            "delta",
            20,
            10,
        );

        let summary = tracker
            .get_summary_filtered_between(None, since, until)
            .expect("summary between");

        assert_eq!(summary.total_commands, 1);
        assert_eq!(summary.command_saved_tokens, 100);
        assert_eq!(summary.replay_suppression_saved, 35);
        assert_eq!(summary.compression_saved, 65);
        assert_eq!(summary.artifacts_created, 1);
        assert_eq!(summary.repeated_outputs_suppressed, 1);
        assert_eq!(summary.changed_outputs_summarized, 1);
    }

    #[test]
    fn test_claim_lease_requires_dependency_resolution() {
        let (_tmp, tracker) = temp_tracker();
        let project = "C:/tmp/project";
        tracker
            .record_worldview_event_for_project(
                project,
                "cargo-test",
                "cargo-test:C:/tmp/project",
                "context cargo test",
                "cargo test: 10 passed (1 suite, <time>)",
                "hash-a",
                None,
                "{}",
            )
            .expect("worldview");

        let claim_id = tracker
            .create_claim_lease_for_project(
                project,
                ClaimLeaseType::Decision,
                "Cargo tests are currently green.",
                Some("Verified by the latest cargo test worldview fact."),
                ClaimLeaseConfidence::High,
                Some("verification"),
                &[ClaimLeaseDependency {
                    kind: ClaimLeaseDependencyKind::WorldviewSubject,
                    key: "cargo-test:C:/tmp/project".to_string(),
                    fingerprint: None,
                }],
                r#"["cargo test"]"#,
                "test",
            )
            .expect("claim");

        let claims = tracker
            .get_claim_leases_filtered(10, Some(project), Some(&[ClaimLeaseStatus::Live]))
            .expect("claims");
        let claim = claims
            .iter()
            .find(|claim| claim.id == claim_id)
            .expect("claim row");
        assert_eq!(claim.status, ClaimLeaseStatus::Live);
        assert_eq!(claim.claim_type, ClaimLeaseType::Decision);
        assert_eq!(claim.dependencies.len(), 1);
        assert_eq!(claim.dependencies[0].fingerprint.as_deref(), Some("hash-a"));
    }

    #[test]
    fn test_claim_lease_turns_stale_when_dependency_changes() {
        let (_tmp, tracker) = temp_tracker();
        let project = "C:/tmp/project";
        tracker
            .record_worldview_event_for_project(
                project,
                "cargo-test",
                "cargo-test:C:/tmp/project",
                "context cargo test",
                "cargo test: 10 passed (1 suite, <time>)",
                "hash-a",
                None,
                "{}",
            )
            .expect("first worldview");

        tracker
            .create_claim_lease_for_project(
                project,
                ClaimLeaseType::BenignAnomaly,
                "The current test suite state is understood.",
                None,
                ClaimLeaseConfidence::Medium,
                Some("verification"),
                &[ClaimLeaseDependency {
                    kind: ClaimLeaseDependencyKind::WorldviewSubject,
                    key: "cargo-test:C:/tmp/project".to_string(),
                    fingerprint: None,
                }],
                r#"["@context/a_demo"]"#,
                "test",
            )
            .expect("claim");

        tracker
            .record_worldview_event_for_project(
                project,
                "cargo-test",
                "cargo-test:C:/tmp/project",
                "context cargo test",
                "cargo test: 9 passed, 1 failed (1 suite, <time>)",
                "hash-b",
                None,
                "{}",
            )
            .expect("changed worldview");

        tracker
            .refresh_claim_lease_statuses(Some(project))
            .expect("refresh");

        let live_claims = tracker
            .get_claim_leases_filtered(10, Some(project), Some(&[ClaimLeaseStatus::Live]))
            .expect("live claims");
        assert!(live_claims.is_empty(), "claim should no longer be live");

        let stale_claims = tracker
            .get_claim_leases_filtered(10, Some(project), Some(&[ClaimLeaseStatus::Stale]))
            .expect("stale claims");
        assert_eq!(stale_claims.len(), 1);
        assert_eq!(stale_claims[0].status, ClaimLeaseStatus::Stale);

        tracker
            .record_worldview_event_for_project(
                project,
                "cargo-test",
                "cargo-test:C:/tmp/project",
                "context cargo test",
                "cargo test: 10 passed (1 suite, <time>)",
                "hash-a",
                None,
                "{}",
            )
            .expect("restored worldview");
        tracker
            .refresh_claim_lease_statuses(Some(project))
            .expect("refresh after restore");

        let stale_after_restore = tracker
            .get_claim_leases_filtered(10, Some(project), Some(&[ClaimLeaseStatus::Stale]))
            .expect("stale after restore");
        assert_eq!(stale_after_restore.len(), 1);
        assert_eq!(stale_after_restore[0].status, ClaimLeaseStatus::Stale);
    }

    #[test]
    fn test_claim_lease_turns_stale_when_user_decision_changes() {
        let (_tmp, tracker) = temp_tracker();
        let project = "C:/tmp/project";
        tracker
            .set_user_decision_for_project(
                project,
                "ui-direction",
                "Keep the existing v5 foundation.",
            )
            .expect("decision");

        tracker
            .create_claim_lease_for_project(
                project,
                ClaimLeaseType::Decision,
                "UI direction is fixed to the current foundation.",
                None,
                ClaimLeaseConfidence::High,
                Some("product"),
                &[ClaimLeaseDependency {
                    kind: ClaimLeaseDependencyKind::UserDecision,
                    key: "ui-direction".to_string(),
                    fingerprint: None,
                }],
                r#"["user decision"]"#,
                "test",
            )
            .expect("claim");

        tracker
            .set_user_decision_for_project(
                project,
                "ui-direction",
                "Move to a new experimental shell.",
            )
            .expect("decision update");
        tracker
            .refresh_claim_lease_statuses(Some(project))
            .expect("refresh");

        let stale_claims = tracker
            .get_claim_leases_filtered(10, Some(project), Some(&[ClaimLeaseStatus::Stale]))
            .expect("stale claims");
        assert_eq!(stale_claims.len(), 1);
        assert_eq!(stale_claims[0].status, ClaimLeaseStatus::Stale);
    }

    #[test]
    fn test_claim_lease_turns_stale_when_review_is_due() {
        let (_tmp, tracker) = temp_tracker();
        let project = "C:/tmp/project";
        tracker
            .record_worldview_event_for_project(
                project,
                "cargo-test",
                "cargo-test:C:/tmp/project",
                "context cargo test",
                "cargo test: 10 passed (1 suite, <time>)",
                "hash-a",
                None,
                "{}",
            )
            .expect("worldview");

        let claim_id = tracker
            .create_claim_lease_for_project(
                project,
                ClaimLeaseType::Decision,
                "The current test suite state is still trusted.",
                None,
                ClaimLeaseConfidence::High,
                Some("verification"),
                &[ClaimLeaseDependency {
                    kind: ClaimLeaseDependencyKind::WorldviewSubject,
                    key: "cargo-test:C:/tmp/project".to_string(),
                    fingerprint: None,
                }],
                r#"["cargo test"]"#,
                "test",
            )
            .expect("claim");
        tracker
            .conn
            .execute(
                "UPDATE claim_leases SET review_after = ?1 WHERE id = ?2",
                params![
                    (Utc::now() - chrono::Duration::days(1)).to_rfc3339(),
                    claim_id
                ],
            )
            .expect("set review_after");

        tracker
            .refresh_claim_lease_statuses(Some(project))
            .expect("refresh");

        let stale_claims = tracker
            .get_claim_leases_filtered(10, Some(project), Some(&[ClaimLeaseStatus::Stale]))
            .expect("stale claims");
        assert_eq!(stale_claims.len(), 1);
        assert_eq!(
            stale_claims[0].demotion_reason.as_deref(),
            Some("review_due")
        );
    }

    #[test]
    fn test_claim_lease_turns_stale_when_expired() {
        let (_tmp, tracker) = temp_tracker();
        let project = "C:/tmp/project";
        tracker
            .record_worldview_event_for_project(
                project,
                "cargo-test",
                "cargo-test:C:/tmp/project",
                "context cargo test",
                "cargo test: 10 passed (1 suite, <time>)",
                "hash-a",
                None,
                "{}",
            )
            .expect("worldview");

        let claim_id = tracker
            .create_claim_lease_for_project(
                project,
                ClaimLeaseType::Decision,
                "The current test suite state is still trusted.",
                None,
                ClaimLeaseConfidence::High,
                Some("verification"),
                &[ClaimLeaseDependency {
                    kind: ClaimLeaseDependencyKind::WorldviewSubject,
                    key: "cargo-test:C:/tmp/project".to_string(),
                    fingerprint: None,
                }],
                r#"["cargo test"]"#,
                "test",
            )
            .expect("claim");
        tracker
            .conn
            .execute(
                "UPDATE claim_leases SET expires_at = ?1 WHERE id = ?2",
                params![
                    (Utc::now() - chrono::Duration::days(1)).to_rfc3339(),
                    claim_id
                ],
            )
            .expect("set expires_at");

        tracker
            .refresh_claim_lease_statuses(Some(project))
            .expect("refresh");

        let stale_claims = tracker
            .get_claim_leases_filtered(10, Some(project), Some(&[ClaimLeaseStatus::Stale]))
            .expect("stale claims");
        assert_eq!(stale_claims.len(), 1);
        assert_eq!(stale_claims[0].demotion_reason.as_deref(), Some("expired"));
    }

    #[test]
    fn test_superseded_claim_stays_superseded() {
        let (_tmp, tracker) = temp_tracker();
        let project = "C:/tmp/project";
        tracker
            .record_worldview_event_for_project(
                project,
                "cargo-test",
                "cargo-test:C:/tmp/project",
                "context cargo test",
                "cargo test: 10 passed (1 suite, <time>)",
                "hash-a",
                None,
                "{}",
            )
            .expect("worldview");

        let claim_id = tracker
            .create_claim_lease_for_project(
                project,
                ClaimLeaseType::Rejection,
                "Do not retry the rejected approach.",
                None,
                ClaimLeaseConfidence::Medium,
                Some("architecture"),
                &[ClaimLeaseDependency {
                    kind: ClaimLeaseDependencyKind::WorldviewSubject,
                    key: "cargo-test:C:/tmp/project".to_string(),
                    fingerprint: None,
                }],
                r#"["rejection evidence"]"#,
                "test",
            )
            .expect("claim");

        assert!(tracker
            .supersede_claim_lease_for_project(project, claim_id)
            .expect("supersede"));
        tracker
            .refresh_claim_lease_statuses(Some(project))
            .expect("refresh");

        let superseded_claims = tracker
            .get_claim_leases_filtered(10, Some(project), Some(&[ClaimLeaseStatus::Superseded]))
            .expect("superseded claims");
        assert_eq!(superseded_claims.len(), 1);
        assert_eq!(superseded_claims[0].status, ClaimLeaseStatus::Superseded);
    }

    #[test]
    fn test_project_scoped_supersede_does_not_touch_other_project_claim() {
        let (_tmp, tracker) = temp_tracker();
        let project_a = "C:/tmp/project-a";
        let project_b = "C:/tmp/project-b";

        tracker
            .record_worldview_event_for_project(
                project_a,
                "cargo-test",
                "cargo-test:C:/tmp/project-a",
                "context cargo test",
                "cargo test: 3 passed (1 suite, <time>)",
                "hash-a",
                None,
                "{}",
            )
            .expect("worldview a");
        tracker
            .record_worldview_event_for_project(
                project_b,
                "cargo-test",
                "cargo-test:C:/tmp/project-b",
                "context cargo test",
                "cargo test: 4 passed (1 suite, <time>)",
                "hash-b",
                None,
                "{}",
            )
            .expect("worldview b");

        let claim_id = tracker
            .create_claim_lease_for_project(
                project_b,
                ClaimLeaseType::Decision,
                "Project B claim",
                None,
                ClaimLeaseConfidence::Medium,
                None,
                &[ClaimLeaseDependency {
                    kind: ClaimLeaseDependencyKind::WorldviewSubject,
                    key: "cargo-test:C:/tmp/project-b".to_string(),
                    fingerprint: None,
                }],
                r#"["project-b"]"#,
                "test",
            )
            .expect("claim b");

        assert!(!tracker
            .supersede_claim_lease_for_project(project_a, claim_id)
            .expect("scoped supersede"));

        let live_claims = tracker
            .get_claim_leases_filtered(10, Some(project_b), Some(&[ClaimLeaseStatus::Live]))
            .expect("live claims b");
        assert_eq!(live_claims.len(), 1);
        assert_eq!(live_claims[0].status, ClaimLeaseStatus::Live);
    }

    #[test]
    fn test_get_claim_leases_filtered_filters_before_limit() {
        let (_tmp, tracker) = temp_tracker();
        let project = "C:/tmp/project";
        tracker
            .record_worldview_event_for_project(
                project,
                "cargo-test",
                "cargo-test:C:/tmp/project",
                "context cargo test",
                "cargo test: 5 passed (1 suite, <time>)",
                "hash-a",
                None,
                "{}",
            )
            .expect("worldview");

        tracker
            .create_claim_lease_for_project(
                project,
                ClaimLeaseType::Decision,
                "Older live claim",
                None,
                ClaimLeaseConfidence::Medium,
                None,
                &[ClaimLeaseDependency {
                    kind: ClaimLeaseDependencyKind::WorldviewSubject,
                    key: "cargo-test:C:/tmp/project".to_string(),
                    fingerprint: None,
                }],
                r#"["live"]"#,
                "test",
            )
            .expect("older live claim");
        let newer_claim_id = tracker
            .create_claim_lease_for_project(
                project,
                ClaimLeaseType::Decision,
                "Newest superseded claim",
                None,
                ClaimLeaseConfidence::Medium,
                None,
                &[ClaimLeaseDependency {
                    kind: ClaimLeaseDependencyKind::WorldviewSubject,
                    key: "cargo-test:C:/tmp/project".to_string(),
                    fingerprint: None,
                }],
                r#"["new-superseded"]"#,
                "test",
            )
            .expect("new superseded");
        tracker
            .supersede_claim_lease_for_project(project, newer_claim_id)
            .expect("supersede newer");

        let live_claims = tracker
            .get_claim_leases_filtered(1, Some(project), Some(&[ClaimLeaseStatus::Live]))
            .expect("filtered live claims");
        assert_eq!(live_claims.len(), 1);
        assert_eq!(live_claims[0].claim_text, "Older live claim");
    }

    #[test]
    fn test_get_user_decisions_filtered_returns_latest_per_key() {
        let (_tmp, tracker) = temp_tracker();
        let project = "C:/tmp/project";
        tracker
            .set_user_decision_for_project(project, "ui-direction", "Keep the existing shell.")
            .expect("decision one");
        tracker
            .set_user_decision_for_project(project, "ui-direction", "Move to the new shell.")
            .expect("decision two");
        tracker
            .set_user_decision_for_project(project, "risk-posture", "Conservative.")
            .expect("decision three");

        let decisions = tracker
            .get_user_decisions_filtered(10, Some(project))
            .expect("decisions");
        assert_eq!(decisions.len(), 2);
        assert!(decisions
            .iter()
            .any(|decision| decision.key == "ui-direction"
                && decision.value_text == "Move to the new shell."));
    }

    // 12. record_parse_failure + get_parse_failure_summary roundtrip
    #[test]
    fn test_parse_failure_roundtrip() {
        let (_tmp, tracker) = temp_tracker();
        let test_cmd = format!("git -C /path status test_{}", std::process::id());

        tracker
            .record_parse_failure(&test_cmd, "unrecognized subcommand", true)
            .expect("Failed to record parse failure");

        let summary = tracker
            .get_parse_failure_summary()
            .expect("Failed to get summary");

        assert!(summary.total >= 1);
        assert!(summary
            .top_commands
            .iter()
            .any(|(raw_command, _count)| raw_command == &test_cmd));
    }

    #[test]
    fn test_parse_failure_suppresses_successful_agent_housekeeping() {
        let (_tmp, tracker) = temp_tracker();
        let pid = std::process::id();
        let statusline_cmd = format!(
            "node C:/Users/OEM/.claude/statusline.js suppress_statusline_{}",
            pid
        );
        let hook_cmd = format!(
            "node C:/Users/OEM/.claude/hooks/sms-guard.js suppress_hook_{}",
            pid
        );
        let bash_hook_cmd = format!(
            "bash -c FILE=\"$CLAUDE_FILE_PATH\"; if [[ \"$FILE\" == *.ts ]]; then true; fi suppress_bash_{}",
            pid
        );
        let registry_cmd = format!(
            "node C:/Users/OEM/.claude/scripts/localhost-registry.js --list suppress_registry_{}",
            pid
        );

        tracker
            .record_parse_failure(&statusline_cmd, "unrecognized subcommand", true)
            .expect("statusline suppression");
        tracker
            .record_parse_failure(&hook_cmd, "unrecognized subcommand", true)
            .expect("hook suppression");
        tracker
            .record_parse_failure(&bash_hook_cmd, "unrecognized subcommand", true)
            .expect("bash hook suppression");
        tracker
            .record_parse_failure(&registry_cmd, "unrecognized subcommand", true)
            .expect("registry suppression");

        let summary = tracker
            .get_parse_failure_summary()
            .expect("Failed to get summary");

        assert_eq!(summary.total, 0);
    }

    #[test]
    fn test_parse_failure_summary_hides_legacy_successful_agent_housekeeping() {
        let (_tmp, tracker) = temp_tracker();
        let pid = std::process::id();
        let legacy_hook_cmd = format!(
            "node C:/Users/OEM/.claude/hooks/sms-guard.js legacy_hook_{}",
            pid
        );
        let actionable_cmd = format!("missing-command-{}", pid);

        tracker
            .conn
            .execute(
                "INSERT INTO parse_failures (timestamp, raw_command, error_message, fallback_succeeded)
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    Utc::now().to_rfc3339(),
                    legacy_hook_cmd,
                    "unrecognized subcommand",
                    1,
                ],
            )
            .expect("legacy insert");
        tracker
            .conn
            .execute(
                "INSERT INTO parse_failures (timestamp, raw_command, error_message, fallback_succeeded)
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    Utc::now().to_rfc3339(),
                    actionable_cmd,
                    "unrecognized subcommand",
                    0,
                ],
            )
            .expect("actionable insert");

        let summary = tracker
            .get_parse_failure_summary()
            .expect("Failed to get summary");

        assert_eq!(summary.total, 1);
        assert_eq!(summary.recovery_rate, 0.0);
        assert!(summary
            .recent_unrecovered
            .iter()
            .any(|r| r.raw_command == actionable_cmd));
        assert!(!summary
            .recent_unrecovered
            .iter()
            .any(|r| r.raw_command == legacy_hook_cmd));
    }

    #[test]
    fn test_parse_failure_keeps_failed_agent_housekeeping() {
        let (_tmp, tracker) = temp_tracker();
        let test_cmd = format!(
            "node C:/Users/OEM/.claude/hooks/sms-guard.js failed_hook_{}",
            std::process::id()
        );

        tracker
            .record_parse_failure(&test_cmd, "unrecognized subcommand", false)
            .expect("failed hook record");

        let summary = tracker
            .get_parse_failure_summary()
            .expect("Failed to get summary");

        assert!(summary
            .recent_unrecovered
            .iter()
            .any(|r| r.raw_command == test_cmd));
    }

    // 13. recovery_rate calculation
    #[test]
    fn test_parse_failure_recovery_rate() {
        let (_tmp, tracker) = temp_tracker();
        let pid = std::process::id();

        // 2 successes, 1 failure
        tracker
            .record_parse_failure(&format!("cmd_ok1_{}", pid), "err", true)
            .unwrap();
        tracker
            .record_parse_failure(&format!("cmd_ok2_{}", pid), "err", true)
            .unwrap();
        tracker
            .record_parse_failure(&format!("cmd_fail_{}", pid), "err", false)
            .unwrap();

        let summary = tracker.get_parse_failure_summary().unwrap();
        assert_eq!(summary.total, 3);
        assert!((summary.recovery_rate - 66.666_666_666_666_66).abs() < 0.001);
    }

    #[test]
    fn test_memory_os_shadow_dual_write_records_context_event() {
        let _guard = ENV_LOCK.lock().expect("env lock");
        let tmp = tempfile::tempdir().expect("tempdir");
        let db_path = tmp.path().join("history.db");
        let tracker = Tracker::new_at_path(&db_path).expect("tracker");

        std::env::set_var("MUNIN_MEMORYOS_JOURNAL_V1", "true");
        std::env::set_var("MUNIN_MEMORYOS_DUAL_WRITE_V1", "true");

        tracker
            .record_context_event(
                "resume",
                ContextEventStats {
                    rendered_tokens: 120,
                    estimated_source_tokens: 480,
                    current_fact_count: 2,
                    recent_change_count: 1,
                    live_claim_count: 1,
                    open_obligation_count: 1,
                    artifact_handle_count: 0,
                    failure_count: 0,
                },
            )
            .expect("context event");

        let count: i64 = tracker
            .conn
            .query_row("SELECT COUNT(*) FROM memory_os_journal_events", [], |row| {
                row.get(0)
            })
            .expect("journal count");
        let receipts: i64 = tracker
            .conn
            .query_row(
                "SELECT COUNT(*) FROM memory_os_idempotency_receipts",
                [],
                |row| row.get(0),
            )
            .expect("receipt count");
        assert_eq!(count, 1);
        assert_eq!(receipts, 1);

        std::env::remove_var("MUNIN_MEMORYOS_JOURNAL_V1");
        std::env::remove_var("MUNIN_MEMORYOS_DUAL_WRITE_V1");
    }

    #[test]
    fn test_scan_memory_os_trust_payload_detects_secret_and_pii() {
        let summary = scan_memory_os_trust_payload(
            "Contact alice@example.com and rotate sk-12345678901234567890 immediately",
        );
        assert!(summary.contains_secret);
        assert!(summary.contains_pii);
        assert!(summary.must_not_packetize);
        assert!(summary
            .findings
            .iter()
            .any(|f| matches!(f.kind, MemoryOsTrustFindingKind::Secret)));
        assert!(summary
            .findings
            .iter()
            .any(|f| matches!(f.kind, MemoryOsTrustFindingKind::Pii)));
    }

    #[test]
    fn test_scan_memory_os_trust_payload_ignores_bare_secret_words_in_prose() {
        let summary = scan_memory_os_trust_payload(
            "This guide explains when the word secret appears in documentation and why password policies matter.",
        );
        assert!(!summary.contains_secret);
        assert!(!summary.must_not_packetize);
        assert!(summary.findings.is_empty());
    }

    #[test]
    fn test_scan_memory_os_trust_payload_detects_secret_assignment() {
        let summary = scan_memory_os_trust_payload(r#"api_key = "abc123def456ghi789""#);
        assert!(summary.contains_secret);
        assert!(summary.must_not_packetize);
        assert!(summary
            .findings
            .iter()
            .any(|f| matches!(f.kind, MemoryOsTrustFindingKind::Secret)));
    }

    #[test]
    fn test_record_memory_os_verification_result_roundtrip() {
        let (_tmp, tracker) = temp_tracker();
        let input = MemoryOsVerificationResultInput {
            verification_result_id: "verify-001".into(),
            proof_id: "proof-001".into(),
            scope_json: "{\"repo_id\":\"context\"}".into(),
            verifier_id: "verifier".into(),
            verifier_version: "v1".into(),
            trusted_root_id: Some("root-001".into()),
            trusted_producer_ids: vec!["producer-001".into()],
            materials_hashes: vec!["mat-001".into()],
            products_hashes: vec!["prod-001".into()],
            verification_time: Utc::now().to_rfc3339(),
            result: MemoryOsVerificationStatus::Verified,
            reason: None,
            attestation_kind: "schema_validated".into(),
        };

        tracker
            .record_memory_os_verification_result(&input)
            .expect("verification result");

        let count: i64 = tracker
            .conn
            .query_row(
                "SELECT COUNT(*) FROM memory_os_verification_results WHERE verification_result_id = 'verify-001'",
                [],
                |row| row.get(0),
            )
            .expect("verification count");
        assert_eq!(count, 1);
    }

    #[test]
    fn test_upsert_memory_os_policy_model_writes_rules() {
        let (_tmp, tracker) = temp_tracker();
        let input = MemoryOsPolicyModelInput {
            policy_model_id: "policy-001".into(),
            version: "v1".into(),
            description: "seed policy".into(),
            created_at: Utc::now().to_rfc3339(),
            rules: vec![MemoryOsAccessRule {
                access_rule_id: "rule-001".into(),
                subject_predicate: "actor_kind == 'agent'".into(),
                object_predicate: "sensitivity_class != 'secret'".into(),
                environment_predicate: "intent_scope == 'resume'".into(),
                action: "packetize".into(),
                effect: "allow".into(),
                priority: 10,
            }],
        };

        tracker
            .upsert_memory_os_policy_model(&input)
            .expect("policy model");

        let count: i64 = tracker
            .conn
            .query_row(
                "SELECT COUNT(*) FROM memory_os_access_rules WHERE policy_model_id = 'policy-001'",
                [],
                |row| row.get(0),
            )
            .expect("policy rule count");
        assert_eq!(count, 1);
    }

    #[test]
    fn test_upsert_memory_os_policy_model_replaces_removed_rules() {
        let (_tmp, tracker) = temp_tracker();
        let created_at = Utc::now().to_rfc3339();
        let initial = MemoryOsPolicyModelInput {
            policy_model_id: "policy-001".into(),
            version: "v1".into(),
            description: "seed policy".into(),
            created_at: created_at.clone(),
            rules: vec![
                MemoryOsAccessRule {
                    access_rule_id: "rule-001".into(),
                    subject_predicate: "actor_kind == 'agent'".into(),
                    object_predicate: "sensitivity_class != 'secret'".into(),
                    environment_predicate: "intent_scope == 'resume'".into(),
                    action: "packetize".into(),
                    effect: "allow".into(),
                    priority: 10,
                },
                MemoryOsAccessRule {
                    access_rule_id: "rule-002".into(),
                    subject_predicate: "actor_kind == 'user'".into(),
                    object_predicate: "true".into(),
                    environment_predicate: "true".into(),
                    action: "read".into(),
                    effect: "allow".into(),
                    priority: 20,
                },
            ],
        };
        tracker
            .upsert_memory_os_policy_model(&initial)
            .expect("seed policy");

        let replacement = MemoryOsPolicyModelInput {
            policy_model_id: "policy-001".into(),
            version: "v2".into(),
            description: "replacement policy".into(),
            created_at,
            rules: vec![MemoryOsAccessRule {
                access_rule_id: "rule-002".into(),
                subject_predicate: "actor_kind == 'user'".into(),
                object_predicate: "scope == 'repo'".into(),
                environment_predicate: "true".into(),
                action: "read".into(),
                effect: "allow".into(),
                priority: 30,
            }],
        };
        tracker
            .upsert_memory_os_policy_model(&replacement)
            .expect("replace policy");

        let count: i64 = tracker
            .conn
            .query_row(
                "SELECT COUNT(*) FROM memory_os_access_rules WHERE policy_model_id = 'policy-001'",
                [],
                |row| row.get(0),
            )
            .expect("policy rule count");
        assert_eq!(count, 1);

        let remaining_rule: String = tracker
            .conn
            .query_row(
                "SELECT access_rule_id FROM memory_os_access_rules WHERE policy_model_id = 'policy-001'",
                [],
                |row| row.get(0),
            )
            .expect("remaining rule");
        assert_eq!(remaining_rule, "rule-002");
    }

    #[test]
    fn test_record_memory_os_trust_observation_roundtrip() {
        let (_tmp, tracker) = temp_tracker();
        let input = MemoryOsTrustObservationInput {
            observation_id: "trust-001".into(),
            target_kind: "claim".into(),
            target_ref: "claim-001".into(),
            action_kind: "packetize".into(),
            decision: MemoryOsTrustDecision::Review,
            reason_json: "{\"reason\":\"observe-only\"}".into(),
            read_seq_cut: Some(42),
            policy_model_id: Some("policy-001".into()),
            sensitivity_class: "internal".into(),
            contains_secret: false,
            contains_pii: false,
            must_not_packetize: false,
            taint_state: "clean".into(),
            observed_at: Utc::now().to_rfc3339(),
        };

        tracker
            .record_memory_os_trust_observation(&input)
            .expect("trust observation");

        let count: i64 = tracker
            .conn
            .query_row(
                "SELECT COUNT(*) FROM memory_os_trust_observations WHERE observation_id = 'trust-001'",
                [],
                |row| row.get(0),
        )
        .expect("trust observation count");
        assert_eq!(count, 1);
    }

    #[test]
    fn test_worldview_event_triggers_trust_observation_in_observe_only_mode() {
        let _guard = ENV_LOCK.lock().expect("env lock");
        let (_tmp, tracker) = temp_tracker();
        std::env::set_var("MUNIN_MEMORYOS_TRUST_V1", "true");

        tracker
            .record_worldview_event_for_project(
                "project-a",
                "read",
                "subject-a",
                "context read file",
                "API secret seen",
                "fingerprint-a",
                None,
                "{\"content\":\"sk-12345678901234567890\"}",
            )
            .expect("worldview event");

        let count: i64 = tracker
            .conn
            .query_row(
                "SELECT COUNT(*) FROM memory_os_trust_observations WHERE target_kind = 'worldview'",
                [],
                |row| row.get(0),
            )
            .expect("trust observation count");
        assert_eq!(count, 1);

        std::env::remove_var("MUNIN_MEMORYOS_TRUST_V1");
    }

    #[test]
    fn test_tracker_bootstraps_memory_os_action_tables() {
        let (_tmp, tracker) = temp_tracker();
        for table in [
            "memory_os_action_observations",
            "memory_os_action_executions",
        ] {
            let exists: i64 = tracker
                .conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
                    [table],
                    |row| row.get(0),
                )
                .expect("sqlite_master query");
            assert_eq!(exists, 1, "expected table {table} to exist");
        }
    }

    #[test]
    fn test_record_memory_os_projection_checkpoint_roundtrip() {
        let (_tmp, tracker) = temp_tracker();
        tracker
            .record_memory_os_projection_checkpoint("claims", "project-a", 1, 5, "full")
            .expect("projection checkpoint");

        let count: i64 = tracker
            .conn
            .query_row(
                "SELECT COUNT(*) FROM memory_os_projection_checkpoints WHERE projection_name = 'claims'",
                [],
                |row| row.get(0),
            )
            .expect("projection checkpoint count");
        assert_eq!(count, 1);
    }

    #[test]
    fn test_get_memory_os_project_snapshot_reports_counts() {
        let _guard = ENV_LOCK.lock().expect("env lock");
        let (_tmp, tracker) = temp_tracker();
        let project_path = current_project_path_string();
        std::env::set_var("MUNIN_MEMORYOS_JOURNAL_V1", "true");
        std::env::set_var("MUNIN_MEMORYOS_DUAL_WRITE_V1", "true");
        std::env::set_var("MUNIN_MEMORYOS_TRUST_V1", "true");

        tracker
            .record_context_event(
                "resume",
                ContextEventStats {
                    rendered_tokens: 10,
                    estimated_source_tokens: 40,
                    current_fact_count: 1,
                    recent_change_count: 0,
                    live_claim_count: 0,
                    open_obligation_count: 0,
                    artifact_handle_count: 0,
                    failure_count: 0,
                },
            )
            .expect("context event");
        tracker
            .record_memory_os_projection_checkpoint("claims", &project_path, 1, 1, "incremental")
            .expect("projection checkpoint");
        tracker
            .record_memory_os_verification_result(&MemoryOsVerificationResultInput {
                verification_result_id: "verify-local".into(),
                proof_id: "proof-local".into(),
                scope_json: serde_json::json!({
                    "project_path": project_path,
                    "repo_id": "context"
                })
                .to_string(),
                verifier_id: "unit-test".into(),
                verifier_version: "v1".into(),
                trusted_root_id: Some("local-root".into()),
                trusted_producer_ids: vec!["context".into()],
                materials_hashes: vec!["m1".into()],
                products_hashes: vec!["p1".into()],
                verification_time: Utc::now().to_rfc3339(),
                result: MemoryOsVerificationStatus::Verified,
                reason: None,
                attestation_kind: "unit".into(),
            })
            .expect("local verification result");
        tracker
            .record_memory_os_verification_result(&MemoryOsVerificationResultInput {
                verification_result_id: "verify-other".into(),
                proof_id: "proof-other".into(),
                scope_json: serde_json::json!({
                    "project_path": "C:/Users/OEM/Projects/other",
                    "repo_id": "other"
                })
                .to_string(),
                verifier_id: "unit-test".into(),
                verifier_version: "v1".into(),
                trusted_root_id: Some("local-root".into()),
                trusted_producer_ids: vec!["context".into()],
                materials_hashes: vec!["m2".into()],
                products_hashes: vec!["p2".into()],
                verification_time: Utc::now().to_rfc3339(),
                result: MemoryOsVerificationStatus::Verified,
                reason: None,
                attestation_kind: "unit".into(),
            })
            .expect("other verification result");

        let snapshot = tracker
            .get_memory_os_project_snapshot(None)
            .expect("project snapshot");
        assert!(snapshot.journal_event_count >= 1);
        assert!(snapshot.last_journal_seq.is_some());
        assert_eq!(snapshot.verification_result_count, 1);
        assert_eq!(snapshot.projection_checkpoints.len(), 1);
        assert_eq!(snapshot.project_path, project_path);

        std::env::remove_var("MUNIN_MEMORYOS_JOURNAL_V1");
        std::env::remove_var("MUNIN_MEMORYOS_DUAL_WRITE_V1");
        std::env::remove_var("MUNIN_MEMORYOS_TRUST_V1");
    }

    #[test]
    fn test_memory_os_promotion_report_blocks_without_matching_proof() {
        let _guard = ENV_LOCK.lock().expect("env lock");
        let (_tmp, tracker) = temp_tracker();

        std::env::set_var("MUNIN_MEMORYOS_READ_MODEL_V1", "true");
        std::env::set_var("MUNIN_MEMORYOS_RESUME_V1", "true");
        std::env::set_var("MUNIN_MEMORYOS_HANDOFF_V1", "true");
        std::env::set_var("MUNIN_MEMORYOS_STRICT_PROMOTION_V1", "true");

        let report = tracker
            .get_memory_os_promotion_report()
            .expect("promotion report");

        assert!(!report.eligible);
        assert!(!report.resume_cutover_ready);
        assert_eq!(report.matching_result_count, 0);
        assert_eq!(
            report.missing_required_splits,
            vec![
                "test-private".to_string(),
                "adversarial-private".to_string()
            ]
        );
        assert!(report
            .decision_summary
            .contains("missing independent proposed-kernel proof"));

        std::env::remove_var("MUNIN_MEMORYOS_READ_MODEL_V1");
        std::env::remove_var("MUNIN_MEMORYOS_RESUME_V1");
        std::env::remove_var("MUNIN_MEMORYOS_HANDOFF_V1");
        std::env::remove_var("MUNIN_MEMORYOS_STRICT_PROMOTION_V1");
    }

    #[test]
    fn test_memory_os_promotion_report_requires_independent_proof_set() {
        let _guard = ENV_LOCK.lock().expect("env lock");
        let (_tmp, tracker) = temp_tracker();

        std::env::set_var("MUNIN_MEMORYOS_READ_MODEL_V1", "true");
        std::env::set_var("MUNIN_MEMORYOS_RESUME_V1", "true");
        std::env::set_var("MUNIN_MEMORYOS_HANDOFF_V1", "true");
        std::env::set_var("MUNIN_MEMORYOS_STRICT_PROMOTION_V1", "true");

        tracker
            .record_memory_os_verification_result(&MemoryOsVerificationResultInput {
                verification_result_id: "verify-old".into(),
                proof_id: "proof-old".into(),
                scope_json: serde_json::json!({
                    "root": "tests/fixtures/replay_eval",
                    "split": "dev-public",
                    "system": "proposed-kernel",
                    "independent": false,
                    "contamination_free": true
                })
                .to_string(),
                verifier_id: "replay-eval".into(),
                verifier_version: "v1".into(),
                trusted_root_id: None,
                trusted_producer_ids: Vec::new(),
                materials_hashes: Vec::new(),
                products_hashes: Vec::new(),
                verification_time: "2026-04-13T00:00:00Z".into(),
                result: MemoryOsVerificationStatus::Verified,
                reason: Some("old verified".into()),
                attestation_kind: "replay-eval".into(),
            })
            .expect("old verification result");
        tracker
            .record_memory_os_verification_result(&MemoryOsVerificationResultInput {
                verification_result_id: "verify-hidden".into(),
                proof_id: "proof-hidden".into(),
                scope_json: serde_json::json!({
                    "root": "tests/fixtures/replay_eval",
                    "split": "test-private",
                    "system": "proposed-kernel",
                    "proof_tier": "independent",
                    "independent": true,
                    "contamination_free": true
                })
                .to_string(),
                verifier_id: "replay-eval".into(),
                verifier_version: "v1".into(),
                trusted_root_id: None,
                trusted_producer_ids: Vec::new(),
                materials_hashes: Vec::new(),
                products_hashes: Vec::new(),
                verification_time: "2026-04-13T00:01:00Z".into(),
                result: MemoryOsVerificationStatus::Verified,
                reason: Some("hidden verified".into()),
                attestation_kind: "replay-eval".into(),
            })
            .expect("hidden verification result");

        let report = tracker
            .get_memory_os_promotion_report()
            .expect("promotion report");

        assert!(!report.eligible);
        assert_eq!(report.matching_result_count, 1);
        assert_eq!(
            report.missing_required_splits,
            vec!["adversarial-private".to_string()]
        );
        assert!(report.decision_summary.contains("missing independent"));

        tracker
            .record_memory_os_verification_result(&MemoryOsVerificationResultInput {
                verification_result_id: "verify-adversarial-contaminated".into(),
                proof_id: "proof-adversarial".into(),
                scope_json: serde_json::json!({
                    "root": "tests/fixtures/replay_eval",
                    "split": "adversarial-private",
                    "system": "proposed-kernel",
                    "proof_tier": "independent",
                    "independent": true,
                    "contamination_free": false,
                    "contaminated": true
                })
                .to_string(),
                verifier_id: "replay-eval".into(),
                verifier_version: "v1".into(),
                trusted_root_id: None,
                trusted_producer_ids: Vec::new(),
                materials_hashes: Vec::new(),
                products_hashes: Vec::new(),
                verification_time: "2026-04-13T00:02:00Z".into(),
                result: MemoryOsVerificationStatus::Verified,
                reason: Some("contaminated".into()),
                attestation_kind: "replay-eval".into(),
            })
            .expect("contaminated adversarial result");

        let contaminated = tracker
            .get_memory_os_promotion_report()
            .expect("promotion report");
        assert!(!contaminated.eligible);
        assert!(contaminated.decision_summary.contains("contamination-free"));

        tracker
            .record_memory_os_verification_result(&MemoryOsVerificationResultInput {
                verification_result_id: "verify-adversarial-clean".into(),
                proof_id: "proof-adversarial-clean".into(),
                scope_json: serde_json::json!({
                    "root": "tests/fixtures/replay_eval",
                    "split": "adversarial-private",
                    "system": "proposed-kernel",
                    "proof_tier": "independent",
                    "independent": true,
                    "contamination_free": true
                })
                .to_string(),
                verifier_id: "replay-eval".into(),
                verifier_version: "v1".into(),
                trusted_root_id: None,
                trusted_producer_ids: Vec::new(),
                materials_hashes: Vec::new(),
                products_hashes: Vec::new(),
                verification_time: "2026-04-13T00:03:00Z".into(),
                result: MemoryOsVerificationStatus::Verified,
                reason: Some("clean".into()),
                attestation_kind: "replay-eval".into(),
            })
            .expect("clean adversarial result");

        let report = tracker
            .get_memory_os_promotion_report()
            .expect("promotion report");
        assert!(report.eligible);
        assert!(report.resume_cutover_ready);
        assert_eq!(report.required_results.len(), 2);

        std::env::remove_var("MUNIN_MEMORYOS_READ_MODEL_V1");
        std::env::remove_var("MUNIN_MEMORYOS_RESUME_V1");
        std::env::remove_var("MUNIN_MEMORYOS_HANDOFF_V1");
        std::env::remove_var("MUNIN_MEMORYOS_STRICT_PROMOTION_V1");
    }

    #[test]
    fn checkpoint_older_than_working_memory_window_is_buried() {
        let checkpoint = MemoryOsCheckpointEnvelope {
            project_path: "C:/repo".to_string(),
            captured_at: Utc::now() - chrono::Duration::hours(72),
            capture: crate::core::memory_os::MemoryOsCheckpointCapture {
                packet_id: "pkt-stale".into(),
                generated_at: "2026-04-13T00:00:00Z".into(),
                preset: "continue".into(),
                intent: "continue".into(),
                profile: "codex-default".into(),
                goal: Some("Resume stale task".into()),
                budget: 1600,
                estimated_tokens: 100,
                estimated_source_tokens: 200,
                pager_manifest_hash: "manifest".into(),
                recall_mode: "off".into(),
                recall_used: false,
                recall_reason: "disabled".into(),
                telemetry: crate::core::memory_os::MemoryOsCheckpointTelemetry {
                    current_fact_count: 0,
                    recent_change_count: 0,
                    live_claim_count: 0,
                    open_obligation_count: 1,
                    artifact_handle_count: 0,
                    failure_count: 0,
                },
                selected_items: vec![crate::core::memory_os::MemoryOsPacketSelection {
                    section: "open_obligations".into(),
                    kind: "obligation".into(),
                    summary: "Finish the old thing".into(),
                    token_estimate: 10,
                    score: 90,
                    artifact_id: None,
                    subject: Some("claim:old".into()),
                    provenance: vec![],
                }],
                exclusions: vec![],
                reentry: crate::core::memory_os::MemoryOsCheckpointReentry {
                    recommended_command: "munin resume --format prompt".into(),
                    current_recommendation: Some("Finish the old thing".into()),
                    first_question: "Still relevant?".into(),
                    first_verification: "Check it.".into(),
                },
            },
        };

        assert!(read_model::checkpoint_should_bury_from_working_memory(
            &checkpoint
        ));
    }

    #[test]
    fn memory_os_open_loop_identity_uses_subject_then_checkpoint_composite_before_summary() {
        let mut open_loops = Vec::new();
        let mut index = HashMap::new();
        let subject_item = crate::core::memory_os::MemoryOsPacketSelection {
            section: "open_obligations".into(),
            kind: "obligation".into(),
            summary: "Re-run the verification matrix".into(),
            token_estimate: 10,
            score: 90,
            artifact_id: None,
            subject: Some("claim:verification-matrix".into()),
            provenance: vec!["claim-lease".into()],
        };

        upsert_memory_os_open_loop(
            &mut open_loops,
            &mut index,
            subject_item.summary.clone(),
            "follow_up",
            "open",
            "medium",
            MemoryOsOpenLoopIdentity::from_packet_item("event-a", "packet-a", &subject_item),
            "event-a",
            "2026-04-13T00:00:00Z",
        );
        upsert_memory_os_open_loop(
            &mut open_loops,
            &mut index,
            subject_item.summary.clone(),
            "follow_up",
            "open",
            "high",
            MemoryOsOpenLoopIdentity::from_packet_item("event-b", "packet-b", &subject_item),
            "event-b",
            "2026-04-13T01:00:00Z",
        );
        assert_eq!(open_loops.len(), 1);
        assert_eq!(open_loops[0].source_event_ids.len(), 2);
        assert_eq!(open_loops[0].severity, "high");

        let subjectless_item = crate::core::memory_os::MemoryOsPacketSelection {
            subject: None,
            provenance: vec!["worldview:a".into()],
            ..subject_item.clone()
        };
        let other_subjectless_item = crate::core::memory_os::MemoryOsPacketSelection {
            subject: None,
            provenance: vec!["worldview:b".into()],
            ..subject_item
        };
        upsert_memory_os_open_loop(
            &mut open_loops,
            &mut index,
            subjectless_item.summary.clone(),
            "follow_up",
            "open",
            "medium",
            MemoryOsOpenLoopIdentity::from_packet_item("event-c", "packet-c", &subjectless_item),
            "event-c",
            "2026-04-13T02:00:00Z",
        );
        upsert_memory_os_open_loop(
            &mut open_loops,
            &mut index,
            other_subjectless_item.summary.clone(),
            "follow_up",
            "open",
            "medium",
            MemoryOsOpenLoopIdentity::from_packet_item(
                "event-d",
                "packet-d",
                &other_subjectless_item,
            ),
            "event-d",
            "2026-04-13T03:00:00Z",
        );

        assert_eq!(open_loops.len(), 3);
    }

    #[test]
    fn test_session_onboarding_checkpoint_writes_when_read_model_enabled() {
        let _guard = ENV_LOCK.lock().expect("env lock");
        let (_tmp, tracker) = temp_tracker();
        std::env::set_var("MUNIN_MEMORYOS_JOURNAL_V1", "false");
        std::env::set_var("MUNIN_MEMORYOS_DUAL_WRITE_V1", "false");
        std::env::set_var("MUNIN_MEMORYOS_CHECKPOINT_V1", "false");
        std::env::set_var("MUNIN_MEMORYOS_ACTION_V1", "false");
        std::env::set_var("MUNIN_MEMORYOS_READ_MODEL_V1", "true");

        tracker
            .record_memory_os_packet_checkpoint_for_project(
                "C:/repo",
                &crate::core::memory_os::MemoryOsCheckpointCapture {
                    packet_id: "onboarding-codex-read-model-001".into(),
                    generated_at: "2026-04-17T00:00:00Z".into(),
                    preset: "resume".into(),
                    intent: "diagnose".into(),
                    profile: "session-onboarding".into(),
                    goal: Some("Keep Memory OS session imports current.".into()),
                    budget: 1600,
                    estimated_tokens: 0,
                    estimated_source_tokens: 0,
                    pager_manifest_hash: "manifest-read-model".into(),
                    recall_mode: "off".into(),
                    recall_used: false,
                    recall_reason: "session-onboarding".into(),
                    telemetry: crate::core::memory_os::MemoryOsCheckpointTelemetry {
                        current_fact_count: 0,
                        recent_change_count: 0,
                        live_claim_count: 0,
                        open_obligation_count: 0,
                        artifact_handle_count: 0,
                        failure_count: 0,
                    },
                    selected_items: Vec::new(),
                    exclusions: Vec::new(),
                    reentry: crate::core::memory_os::MemoryOsCheckpointReentry {
                        recommended_command: "munin resume --format prompt".into(),
                        current_recommendation: Some(
                            "Keep Memory OS session imports current.".into(),
                        ),
                        first_question: "What still matters from this session?".into(),
                        first_verification: "Verify the import checkpoint is visible.".into(),
                    },
                },
            )
            .expect("checkpoint");

        let count: i64 = tracker
            .conn
            .query_row(
                "SELECT COUNT(*) FROM memory_os_journal_events WHERE event_id LIKE 'legacy-packet-resume-onboarding-codex-read-model-001-%'",
                [],
                |row| row.get(0),
            )
            .expect("count");
        assert_eq!(count, 1);

        std::env::remove_var("MUNIN_MEMORYOS_JOURNAL_V1");
        std::env::remove_var("MUNIN_MEMORYOS_DUAL_WRITE_V1");
        std::env::remove_var("MUNIN_MEMORYOS_CHECKPOINT_V1");
        std::env::remove_var("MUNIN_MEMORYOS_ACTION_V1");
        std::env::remove_var("MUNIN_MEMORYOS_READ_MODEL_V1");
    }

    #[test]
    fn test_memory_os_project_kernel_rebuilds_claims_open_loops_and_checkpoints() {
        let _guard = ENV_LOCK.lock().expect("env lock");
        let (_tmp, tracker) = temp_tracker();
        let project_path = current_project_path_string();
        std::env::set_var("MUNIN_MEMORYOS_JOURNAL_V1", "true");
        std::env::set_var("MUNIN_MEMORYOS_DUAL_WRITE_V1", "true");
        std::env::set_var("MUNIN_MEMORYOS_CHECKPOINT_V1", "true");

        tracker
            .record_memory_os_shadow_event(MemoryOsShadowEvent {
                event_id: "legacy-claim-1".into(),
                stream_id: "legacy.claim:1".into(),
                stream_revision: 0,
                expected_stream_revision: None,
                tx_index: 0,
                event_kind: "legacy.claim-lease-created".into(),
                idempotency_key: "legacy.claim:rowid:1".into(),
                idempotency_receipt_id: None,
                project_path: project_path.clone(),
                scope_json: "{}".into(),
                actor_json: "{}".into(),
                target_refs_json: "[]".into(),
                payload_json: serde_json::json!({
                    "claim_type": "decision",
                    "claim_text": "Replay eval contract is the next hard gate",
                    "confidence": "high",
                    "scope_key": "resume",
                    "source_kind": "manual"
                })
                .to_string(),
                proof_refs_json: "[]".into(),
                precondition_hash: None,
                result_hash: Some(hash_text("Replay eval contract is the next hard gate")),
                schema_fingerprint: "memoryos-shadow-v1".into(),
            })
            .expect("decision shadow event");
        tracker
            .record_memory_os_shadow_event(MemoryOsShadowEvent {
                event_id: "legacy-claim-2".into(),
                stream_id: "legacy.claim:2".into(),
                stream_revision: 0,
                expected_stream_revision: None,
                tx_index: 0,
                event_kind: "legacy.claim-lease-created".into(),
                idempotency_key: "legacy.claim:rowid:2".into(),
                idempotency_receipt_id: None,
                project_path: project_path.clone(),
                scope_json: "{}".into(),
                actor_json: "{}".into(),
                target_refs_json: "[]".into(),
                payload_json: serde_json::json!({
                    "claim_type": "obligation",
                    "claim_text": "Re-run the flaky resume test",
                    "confidence": "medium",
                    "scope_key": "resume",
                    "source_kind": "manual"
                })
                .to_string(),
                proof_refs_json: "[]".into(),
                precondition_hash: None,
                result_hash: Some(hash_text("Re-run the flaky resume test")),
                schema_fingerprint: "memoryos-shadow-v1".into(),
            })
            .expect("obligation shadow event");

        tracker
            .record_memory_os_packet_checkpoint(
                &crate::core::memory_os::MemoryOsCheckpointCapture {
                    packet_id: "pkt-123".into(),
                    generated_at: "2026-04-13T00:00:00Z".into(),
                    preset: "resume".into(),
                    intent: "diagnose".into(),
                    profile: "codex-default".into(),
                    goal: Some("Resume the memory OS tranche".into()),
                    budget: 1600,
                    estimated_tokens: 240,
                    estimated_source_tokens: 480,
                    pager_manifest_hash: "manifest-123".into(),
                    recall_mode: "off".into(),
                    recall_used: false,
                    recall_reason: "disabled".into(),
                    telemetry: crate::core::memory_os::MemoryOsCheckpointTelemetry {
                        current_fact_count: 1,
                        recent_change_count: 1,
                        live_claim_count: 1,
                        open_obligation_count: 1,
                        artifact_handle_count: 0,
                        failure_count: 1,
                    },
                    selected_items: vec![
                        crate::core::memory_os::MemoryOsPacketSelection {
                            section: "open_obligations".into(),
                            kind: "obligation".into(),
                            summary: "Re-run the flaky resume test".into(),
                            token_estimate: 18,
                            score: 90,
                            artifact_id: None,
                            subject: Some("claim:resume-test".into()),
                            provenance: vec!["claim-lease".into()],
                        },
                        crate::core::memory_os::MemoryOsPacketSelection {
                            section: "current_failures".into(),
                            kind: "failure".into(),
                            summary: "Replay harness validate path was missing".into(),
                            token_estimate: 14,
                            score: 80,
                            artifact_id: None,
                            subject: Some("failure:validate".into()),
                            provenance: vec!["worldview".into()],
                        },
                    ],
                    exclusions: vec!["recent_commands budget".into()],
                    reentry: crate::core::memory_os::MemoryOsCheckpointReentry {
                        recommended_command: "munin resume --format prompt".into(),
                        current_recommendation: Some("Re-run the flaky resume test".into()),
                        first_question:
                            "Is this still the right next move: Re-run the flaky resume test?"
                                .into(),
                        first_verification:
                            "Re-check failure state: Replay harness validate path was missing"
                                .into(),
                    },
                },
            )
            .expect("packet checkpoint");

        let kernel = tracker
            .get_memory_os_project_kernel(None)
            .expect("memory os kernel");
        assert_eq!(kernel.project_path, project_path);
        assert_eq!(kernel.claims.len(), 1);
        assert!(kernel.claims[0]
            .claim_text
            .contains("Replay eval contract is the next hard gate"));
        assert!(kernel
            .open_loops
            .iter()
            .any(|loop_item| loop_item.summary == "Re-run the flaky resume test"));
        assert!(kernel
            .open_loops
            .iter()
            .any(|loop_item| loop_item.summary == "Replay harness validate path was missing"));
        assert_eq!(kernel.checkpoints.len(), 1);
        assert_eq!(kernel.checkpoints[0].preset, "resume");
        assert_eq!(
            kernel.checkpoints[0].current_recommendation.as_deref(),
            Some("Re-run the flaky resume test")
        );

        std::env::remove_var("MUNIN_MEMORYOS_JOURNAL_V1");
        std::env::remove_var("MUNIN_MEMORYOS_DUAL_WRITE_V1");
        std::env::remove_var("MUNIN_MEMORYOS_CHECKPOINT_V1");
    }

    #[test]
    fn test_memory_os_action_candidates_promote_repeated_checkpoint_reentry() {
        let _guard = ENV_LOCK.lock().expect("env lock");
        let (_tmp, tracker) = temp_tracker();
        std::env::set_var("MUNIN_MEMORYOS_JOURNAL_V1", "true");
        std::env::set_var("MUNIN_MEMORYOS_DUAL_WRITE_V1", "true");
        std::env::set_var("MUNIN_MEMORYOS_CHECKPOINT_V1", "true");
        std::env::set_var("MUNIN_MEMORYOS_ACTION_V1", "true");

        let generated_at = Utc::now().to_rfc3339();
        for packet_id in ["pkt-201", "pkt-202"] {
            tracker
                .record_memory_os_packet_checkpoint(
                    &crate::core::memory_os::MemoryOsCheckpointCapture {
                        packet_id: packet_id.into(),
                        generated_at: generated_at.clone(),
                        preset: "resume".into(),
                        intent: "diagnose".into(),
                        profile: "codex-default".into(),
                        goal: Some("Resume the action-memory tranche".into()),
                        budget: 1600,
                        estimated_tokens: 220,
                        estimated_source_tokens: 460,
                        pager_manifest_hash: format!("manifest-{packet_id}"),
                        recall_mode: "off".into(),
                        recall_used: false,
                        recall_reason: "disabled".into(),
                        telemetry: crate::core::memory_os::MemoryOsCheckpointTelemetry {
                            current_fact_count: 1,
                            recent_change_count: 1,
                            live_claim_count: 0,
                            open_obligation_count: 1,
                            artifact_handle_count: 0,
                            failure_count: 0,
                        },
                        selected_items: vec![crate::core::memory_os::MemoryOsPacketSelection {
                            section: "open_obligations".into(),
                            kind: "obligation".into(),
                            summary: "Re-run the flaky resume test".into(),
                            token_estimate: 18,
                            score: 90,
                            artifact_id: None,
                            subject: Some("claim:resume-test".into()),
                            provenance: vec!["claim-lease".into()],
                        }],
                        exclusions: Vec::new(),
                        reentry: crate::core::memory_os::MemoryOsCheckpointReentry {
                            recommended_command: "munin resume --format prompt".into(),
                            current_recommendation: Some("Re-run the flaky resume test".into()),
                            first_question:
                                "Is this still the right next move: Re-run the flaky resume test?"
                                    .into(),
                            first_verification:
                                "Verify the flaky resume test still fails before editing".into(),
                        },
                    },
                )
                .expect("packet checkpoint");
        }

        tracker
            .record_memory_os_action_execution(
                "context-user-command",
                "munin resume --format prompt",
                Some("claim:resume-test"),
                0,
            )
            .expect("first execution");
        tracker
            .record_memory_os_action_execution(
                "context-user-command",
                "munin resume --format prompt",
                Some("claim:resume-test"),
                0,
            )
            .expect("second execution");

        let candidates = tracker
            .get_memory_os_action_candidates(None)
            .expect("action candidates");
        assert_eq!(candidates.len(), 1);
        assert_eq!(
            candidates[0].action.command_sig.as_deref(),
            Some("munin resume --format prompt")
        );
        assert_eq!(candidates[0].precedent_count, 2);
        assert_eq!(candidates[0].success_count, 2);
        assert_eq!(candidates[0].failure_count, 0);
        assert_eq!(candidates[0].status, "promotable");
        assert_eq!(candidates[0].confidence, "high");
        assert_eq!(candidates[0].actuator_type, "run_command");
        assert!(candidates[0].review_after.is_some());
        assert!(candidates[0].expires_at.is_some());
        assert_eq!(candidates[0].aging_status, "fresh");

        std::env::remove_var("MUNIN_MEMORYOS_JOURNAL_V1");
        std::env::remove_var("MUNIN_MEMORYOS_DUAL_WRITE_V1");
        std::env::remove_var("MUNIN_MEMORYOS_CHECKPOINT_V1");
        std::env::remove_var("MUNIN_MEMORYOS_ACTION_V1");
    }

    #[test]
    fn test_memory_os_action_policy_view_keeps_learned_candidates_visible() {
        let _guard = ENV_LOCK.lock().expect("env lock");
        let (_tmp, tracker) = temp_tracker();
        let project = current_project_path_string();
        std::env::set_var("MUNIN_MEMORYOS_ACTION_V1", "true");

        let cue = crate::core::memory_os::MemoryOsActionCue {
            cue_kind: "learned-habit".to_string(),
            packet_preset: None,
            intent: Some("munin-learn".to_string()),
            override_type: None,
            correction_shape: None,
            trigger_section: Some("hook-rule".to_string()),
            trigger_subject: Some("Repeat correction: git status".to_string()),
            trigger_summary: Some(
                "git status kept turning into context git status after shell mistakes.".to_string(),
            ),
        };
        let action = crate::core::memory_os::MemoryOsAction {
            action_kind: "hook-rule".to_string(),
            command_sig: Some("context git status".to_string()),
            recommendation: None,
        };
        for idx in 0..3 {
            tracker
                .record_memory_os_action_observation_for_project(
                    &project,
                    "learned-correction-rule",
                    &cue,
                    &action,
                    &format!("learned-correction-{idx}"),
                    &format!("2026-04-14T0{}:00:00Z", idx + 1),
                )
                .expect("learned observation");
        }

        let report = tracker
            .get_memory_os_action_policy_view_report(
                crate::core::memory_os::MemoryOsInspectionScope::Project,
                Some(&project),
            )
            .expect("action policy");

        assert_eq!(report.candidate_count, 1);
        assert_eq!(report.candidates[0].status, "promotable");
        assert_eq!(report.candidates[0].confidence, "medium");
        assert_eq!(report.candidates[0].actuator_type, "hook-rule");
        assert!(report.rules.iter().any(|rule| {
            rule.action_kind == "hook-rule"
                && rule
                    .suggested_command
                    .as_deref()
                    .is_some_and(|command| command == "context git status")
                && rule.review_after.is_some()
                && rule.expires_at.is_some()
        }));

        std::env::remove_var("MUNIN_MEMORYOS_ACTION_V1");
    }

    #[test]
    fn test_memory_os_action_policy_view_scopes_command_defaults_to_project_and_keeps_serving_rules_global(
    ) {
        let _guard = ENV_LOCK.lock().expect("env lock");
        let (_tmp, tracker) = temp_tracker();
        std::env::set_var("MUNIN_MEMORYOS_JOURNAL_V1", "true");
        std::env::set_var("MUNIN_MEMORYOS_DUAL_WRITE_V1", "true");
        std::env::set_var("MUNIN_MEMORYOS_CHECKPOINT_V1", "true");
        std::env::set_var("MUNIN_MEMORYOS_ACTION_V1", "true");

        for packet_id in ["pkt-301", "pkt-302"] {
            tracker
                .record_memory_os_packet_checkpoint(
                    &crate::core::memory_os::MemoryOsCheckpointCapture {
                        packet_id: packet_id.into(),
                        generated_at: "2026-04-12T00:00:00Z".into(),
                        preset: "resume".into(),
                        intent: "diagnose".into(),
                        profile: "codex-default".into(),
                        goal: Some("Resume the action-policy tranche".into()),
                        budget: 1600,
                        estimated_tokens: 220,
                        estimated_source_tokens: 460,
                        pager_manifest_hash: format!("manifest-{packet_id}"),
                        recall_mode: "off".into(),
                        recall_used: false,
                        recall_reason: "disabled".into(),
                        telemetry: crate::core::memory_os::MemoryOsCheckpointTelemetry {
                            current_fact_count: 1,
                            recent_change_count: 1,
                            live_claim_count: 0,
                            open_obligation_count: 1,
                            artifact_handle_count: 0,
                            failure_count: 0,
                        },
                        selected_items: vec![crate::core::memory_os::MemoryOsPacketSelection {
                            section: "open_obligations".into(),
                            kind: "obligation".into(),
                            summary: "Re-run the flaky resume test".into(),
                            token_estimate: 18,
                            score: 90,
                            artifact_id: None,
                            subject: Some("claim:resume-test".into()),
                            provenance: vec!["claim-lease".into()],
                        }],
                        exclusions: Vec::new(),
                        reentry: crate::core::memory_os::MemoryOsCheckpointReentry {
                            recommended_command: "munin resume --format prompt".into(),
                            current_recommendation: Some("Re-run the flaky resume test".into()),
                            first_question:
                                "Is this still the right next move: Re-run the flaky resume test?"
                                    .into(),
                            first_verification:
                                "Verify the flaky resume test still fails before editing".into(),
                        },
                    },
                )
                .expect("packet checkpoint");
        }

        tracker
            .record_memory_os_action_execution(
                "context-user-command",
                "munin resume --format prompt",
                Some("claim:resume-test"),
                0,
            )
            .expect("first execution");
        tracker
            .record_memory_os_action_execution(
                "context-user-command",
                "munin resume --format prompt",
                Some("claim:resume-test"),
                0,
            )
            .expect("second execution");

        let user_report = tracker
            .get_memory_os_action_policy_view_report(
                crate::core::memory_os::MemoryOsInspectionScope::User,
                None,
            )
            .expect("action policy view");

        assert!(!user_report
            .rules
            .iter()
            .any(|rule| rule.action_kind == "command-default"
                && rule.suggested_command.as_deref() == Some("munin resume --format prompt")));
        assert!(user_report
            .rules
            .iter()
            .any(|rule| rule.action_kind == "serving-policy"
                && rule.summary.contains("Memory OS projections first")));
        assert!(user_report.assertion_count > 0);

        let project_report = tracker
            .get_memory_os_action_policy_view_report(
                crate::core::memory_os::MemoryOsInspectionScope::Project,
                Some(&current_project_path_string()),
            )
            .expect("project action policy view");
        assert!(project_report
            .rules
            .iter()
            .any(|rule| rule.action_kind == "run_command"
                && rule.suggested_command.as_deref() == Some("munin resume --format prompt")));

        std::env::remove_var("MUNIN_MEMORYOS_JOURNAL_V1");
        std::env::remove_var("MUNIN_MEMORYOS_DUAL_WRITE_V1");
        std::env::remove_var("MUNIN_MEMORYOS_CHECKPOINT_V1");
        std::env::remove_var("MUNIN_MEMORYOS_ACTION_V1");
    }

    #[test]
    fn test_memory_os_action_policy_view_filters_sensitive_commands() {
        let _guard = ENV_LOCK.lock().expect("env lock");
        let (_tmp, tracker) = temp_tracker();
        std::env::set_var("MUNIN_MEMORYOS_JOURNAL_V1", "true");
        std::env::set_var("MUNIN_MEMORYOS_DUAL_WRITE_V1", "true");
        std::env::set_var("MUNIN_MEMORYOS_CHECKPOINT_V1", "true");
        std::env::set_var("MUNIN_MEMORYOS_ACTION_V1", "true");

        tracker
            .record_memory_os_packet_checkpoint(
                &crate::core::memory_os::MemoryOsCheckpointCapture {
                    packet_id: "pkt-sensitive".into(),
                    generated_at: "2026-04-12T00:00:00Z".into(),
                    preset: "resume".into(),
                    intent: "diagnose".into(),
                    profile: "codex-default".into(),
                    goal: Some("Sensitive action policy".into()),
                    budget: 1600,
                    estimated_tokens: 220,
                    estimated_source_tokens: 460,
                    pager_manifest_hash: "manifest-sensitive".into(),
                    recall_mode: "off".into(),
                    recall_used: false,
                    recall_reason: "disabled".into(),
                    telemetry: crate::core::memory_os::MemoryOsCheckpointTelemetry {
                        current_fact_count: 1,
                        recent_change_count: 1,
                        live_claim_count: 0,
                        open_obligation_count: 1,
                        artifact_handle_count: 0,
                        failure_count: 0,
                    },
                    selected_items: vec![crate::core::memory_os::MemoryOsPacketSelection {
                        section: "open_obligations".into(),
                        kind: "obligation".into(),
                        summary: "Re-run the sensitive command".into(),
                        token_estimate: 18,
                        score: 90,
                        artifact_id: None,
                        subject: Some("claim:sensitive".into()),
                        provenance: vec!["claim-lease".into()],
                    }],
                    exclusions: Vec::new(),
                    reentry: crate::core::memory_os::MemoryOsCheckpointReentry {
                        recommended_command:
                            "context proxy powershell.exe -NoProfile -Command \"$env:FIRECRAWL_API_KEY='abc'\""
                                .into(),
                        current_recommendation: Some("Re-run the sensitive command".into()),
                        first_question: "Sensitive?".into(),
                        first_verification: "Verify sensitive".into(),
                    },
                },
            )
            .expect("packet checkpoint");

        tracker
            .record_memory_os_action_execution(
                "context-user-command",
                "context proxy powershell.exe -NoProfile -Command \"$env:FIRECRAWL_API_KEY='abc'\"",
                Some("claim:sensitive"),
                0,
            )
            .expect("sensitive execution");
        tracker
            .record_memory_os_action_execution(
                "context-user-command",
                "context proxy powershell.exe -NoProfile -Command \"$env:FIRECRAWL_API_KEY='abc'\"",
                Some("claim:sensitive"),
                0,
            )
            .expect("sensitive execution again");

        let report = tracker
            .get_memory_os_action_policy_view_report(
                crate::core::memory_os::MemoryOsInspectionScope::Project,
                Some(&current_project_path_string()),
            )
            .expect("project action policy view");

        assert!(!report.rules.iter().any(|rule| {
            rule.action_kind == "command-default"
                && rule
                    .suggested_command
                    .as_deref()
                    .unwrap_or_default()
                    .contains("FIRECRAWL_API_KEY")
        }));

        std::env::remove_var("MUNIN_MEMORYOS_JOURNAL_V1");
        std::env::remove_var("MUNIN_MEMORYOS_DUAL_WRITE_V1");
        std::env::remove_var("MUNIN_MEMORYOS_CHECKPOINT_V1");
        std::env::remove_var("MUNIN_MEMORYOS_ACTION_V1");
    }

    #[test]
    fn test_memory_os_action_policy_view_includes_approval_jobs_without_hook_capabilities() {
        let (_tmp, tracker) = temp_tracker();
        let project = current_project_path_string();
        tracker
            .upsert_approval_job_for_project(
                &project,
                &ApprovalJobInput {
                    job_id: "approval-sitesorted-2026-04-14-kpi-paying-customers".to_string(),
                    scope: "project".to_string(),
                    scope_target: Some(project.clone()),
                    local_date: "2026-04-14".to_string(),
                    item_id: Some("kpi-paying-customers".to_string()),
                    item_kind: "kpi".to_string(),
                    title: "Recover paying customers KPI".to_string(),
                    summary: "Fresh red KPI requires explicit approval.".to_string(),
                    status: ApprovalJobStatus::Queued,
                    source_kind: "strategy-nudge".to_string(),
                    provider: Some("codex".to_string()),
                    continuity_active: false,
                    expected_effect: Some("Investigate conversion blockers.".to_string()),
                    queue_path: Some("C:/tmp/proactivity/queue/job.json".to_string()),
                    result_path: None,
                    evidence_json: r#"["Current value: 1","Target: 10"]"#.to_string(),
                    review_after: None,
                    expires_at: None,
                },
            )
            .expect("approval job");

        let report = tracker
            .get_memory_os_action_policy_view_report(
                crate::core::memory_os::MemoryOsInspectionScope::Project,
                Some(&project),
            )
            .expect("action policy");

        assert_eq!(report.approvals_count, 1);
        assert_eq!(report.approvals[0].status, "queued");
        assert!(report.hook_capabilities.is_empty());
    }

    #[test]
    fn test_command_gain_details_capture_tiny_vs_large_runs() {
        let mut rollup = GainRollup::default();
        rollup.by_command.insert(
            "context grep".to_string(),
            AggregateStats {
                command_count: 3,
                input_tokens: 1040,
                output_tokens: 270,
                saved_tokens: 770,
                total_time_ms: 90,
                savings_pct_sum: 134.0,
                tiny_input_runs: 2,
                large_input_runs: 1,
                max_input_tokens: 1000,
                max_saved_tokens: 750,
                ..AggregateStats::default()
            },
        );

        let details = rollup.command_gain_details();
        assert_eq!(details.len(), 1);
        assert_eq!(details[0].command, "context grep");
        assert_eq!(details[0].tiny_input_runs, 2);
        assert_eq!(details[0].large_input_runs, 1);
        assert_eq!(details[0].max_input_tokens, 1000);
        assert!(details[0].weighted_savings_pct > 70.0);
    }

    #[test]
    fn test_memory_os_promoted_assertions_keep_project_scope_target() {
        let (_tmp, tracker) = temp_tracker();
        let project = "C:/tmp/project";
        tracker
            .record_worldview_event_for_project(
                project,
                "cargo-test",
                "cargo-test:C:/tmp/project",
                "context cargo test",
                "cargo test: 10 passed (1 suite, <time>)",
                "hash-a",
                None,
                "{}",
            )
            .expect("worldview");

        let claim_id = tracker
            .create_claim_lease_for_project(
                project,
                ClaimLeaseType::Decision,
                "Cargo tests are currently green.",
                Some("Verified by the latest cargo test worldview fact."),
                ClaimLeaseConfidence::High,
                None,
                &[ClaimLeaseDependency {
                    kind: ClaimLeaseDependencyKind::WorldviewSubject,
                    key: "cargo-test:C:/tmp/project".to_string(),
                    fingerprint: None,
                }],
                r#"["cargo test"]"#,
                "test",
            )
            .expect("claim");

        let assertions = tracker
            .get_memory_os_promoted_assertions(
                crate::core::memory_os::MemoryOsInspectionScope::Project,
                Some(project),
                10,
            )
            .expect("assertions");
        let assertion = assertions
            .iter()
            .find(|record| record.assertion_id == format!("claim-lease:{claim_id}"))
            .expect("matching assertion");

        assert_eq!(assertion.scope, "project");
        assert_eq!(assertion.scope_target.as_deref(), Some(project));
    }

    #[test]
    fn test_latest_memory_os_action_subject_prefers_newest_matching_command() {
        let _guard = ENV_LOCK.lock().expect("env lock");
        let (_tmp, tracker) = temp_tracker();
        std::env::set_var("MUNIN_MEMORYOS_JOURNAL_V1", "true");
        std::env::set_var("MUNIN_MEMORYOS_DUAL_WRITE_V1", "true");
        std::env::set_var("MUNIN_MEMORYOS_CHECKPOINT_V1", "true");
        std::env::set_var("MUNIN_MEMORYOS_ACTION_V1", "true");

        tracker
            .record_memory_os_packet_checkpoint(
                &crate::core::memory_os::MemoryOsCheckpointCapture {
                    packet_id: "pkt-old".into(),
                    generated_at: "2026-04-12T00:00:00Z".into(),
                    preset: "resume".into(),
                    intent: "diagnose".into(),
                    profile: "codex-default".into(),
                    goal: Some("Old subject".into()),
                    budget: 1600,
                    estimated_tokens: 220,
                    estimated_source_tokens: 460,
                    pager_manifest_hash: "manifest-old".into(),
                    recall_mode: "off".into(),
                    recall_used: false,
                    recall_reason: "disabled".into(),
                    telemetry: crate::core::memory_os::MemoryOsCheckpointTelemetry {
                        current_fact_count: 1,
                        recent_change_count: 0,
                        live_claim_count: 0,
                        open_obligation_count: 1,
                        artifact_handle_count: 0,
                        failure_count: 0,
                    },
                    selected_items: vec![crate::core::memory_os::MemoryOsPacketSelection {
                        section: "open_obligations".into(),
                        kind: "obligation".into(),
                        summary: "Older subject".into(),
                        token_estimate: 10,
                        score: 50,
                        artifact_id: None,
                        subject: Some("claim:older".into()),
                        provenance: vec!["claim-lease".into()],
                    }],
                    exclusions: Vec::new(),
                    reentry: crate::core::memory_os::MemoryOsCheckpointReentry {
                        recommended_command: "munin resume --format prompt".into(),
                        current_recommendation: Some("Older subject".into()),
                        first_question: "Older?".into(),
                        first_verification: "Verify older".into(),
                    },
                },
            )
            .expect("old checkpoint");

        tracker
            .record_memory_os_packet_checkpoint(
                &crate::core::memory_os::MemoryOsCheckpointCapture {
                    packet_id: "pkt-new".into(),
                    generated_at: "2026-04-12T01:00:00Z".into(),
                    preset: "resume".into(),
                    intent: "diagnose".into(),
                    profile: "codex-default".into(),
                    goal: Some("New subject".into()),
                    budget: 1600,
                    estimated_tokens: 220,
                    estimated_source_tokens: 460,
                    pager_manifest_hash: "manifest-new".into(),
                    recall_mode: "off".into(),
                    recall_used: false,
                    recall_reason: "disabled".into(),
                    telemetry: crate::core::memory_os::MemoryOsCheckpointTelemetry {
                        current_fact_count: 1,
                        recent_change_count: 0,
                        live_claim_count: 0,
                        open_obligation_count: 1,
                        artifact_handle_count: 0,
                        failure_count: 0,
                    },
                    selected_items: vec![crate::core::memory_os::MemoryOsPacketSelection {
                        section: "open_obligations".into(),
                        kind: "obligation".into(),
                        summary: "Newer subject".into(),
                        token_estimate: 10,
                        score: 60,
                        artifact_id: None,
                        subject: Some("claim:newer".into()),
                        provenance: vec!["claim-lease".into()],
                    }],
                    exclusions: Vec::new(),
                    reentry: crate::core::memory_os::MemoryOsCheckpointReentry {
                        recommended_command: "munin resume --format prompt".into(),
                        current_recommendation: Some("Newer subject".into()),
                        first_question: "Newer?".into(),
                        first_verification: "Verify newer".into(),
                    },
                },
            )
            .expect("new checkpoint");

        let subject = tracker
            .latest_memory_os_action_subject("munin resume --format prompt")
            .expect("latest subject");
        assert_eq!(subject.as_deref(), Some("claim:newer"));

        std::env::remove_var("MUNIN_MEMORYOS_JOURNAL_V1");
        std::env::remove_var("MUNIN_MEMORYOS_DUAL_WRITE_V1");
        std::env::remove_var("MUNIN_MEMORYOS_CHECKPOINT_V1");
        std::env::remove_var("MUNIN_MEMORYOS_ACTION_V1");
    }

    #[test]
    fn test_latest_memory_os_action_subject_does_not_fall_back_past_newer_subjectless_observation()
    {
        let _guard = ENV_LOCK.lock().expect("env lock");
        let (_tmp, tracker) = temp_tracker();
        std::env::set_var("MUNIN_MEMORYOS_JOURNAL_V1", "true");
        std::env::set_var("MUNIN_MEMORYOS_DUAL_WRITE_V1", "true");
        std::env::set_var("MUNIN_MEMORYOS_CHECKPOINT_V1", "true");
        std::env::set_var("MUNIN_MEMORYOS_ACTION_V1", "true");

        tracker
            .record_memory_os_packet_checkpoint(
                &crate::core::memory_os::MemoryOsCheckpointCapture {
                    packet_id: "pkt-subject".into(),
                    generated_at: "2026-04-12T00:00:00Z".into(),
                    preset: "resume".into(),
                    intent: "diagnose".into(),
                    profile: "codex-default".into(),
                    goal: Some("Older subject".into()),
                    budget: 1600,
                    estimated_tokens: 220,
                    estimated_source_tokens: 460,
                    pager_manifest_hash: "manifest-subject".into(),
                    recall_mode: "off".into(),
                    recall_used: false,
                    recall_reason: "disabled".into(),
                    telemetry: crate::core::memory_os::MemoryOsCheckpointTelemetry {
                        current_fact_count: 1,
                        recent_change_count: 0,
                        live_claim_count: 0,
                        open_obligation_count: 1,
                        artifact_handle_count: 0,
                        failure_count: 0,
                    },
                    selected_items: vec![crate::core::memory_os::MemoryOsPacketSelection {
                        section: "open_obligations".into(),
                        kind: "obligation".into(),
                        summary: "Older subject".into(),
                        token_estimate: 10,
                        score: 50,
                        artifact_id: None,
                        subject: Some("claim:older".into()),
                        provenance: vec!["claim-lease".into()],
                    }],
                    exclusions: Vec::new(),
                    reentry: crate::core::memory_os::MemoryOsCheckpointReentry {
                        recommended_command: "munin resume --format prompt".into(),
                        current_recommendation: Some("Older subject".into()),
                        first_question: "Older?".into(),
                        first_verification: "Verify older".into(),
                    },
                },
            )
            .expect("older subject checkpoint");

        tracker
            .record_memory_os_packet_checkpoint(
                &crate::core::memory_os::MemoryOsCheckpointCapture {
                    packet_id: "pkt-subjectless".into(),
                    generated_at: "2026-04-12T01:00:00Z".into(),
                    preset: "resume".into(),
                    intent: "diagnose".into(),
                    profile: "codex-default".into(),
                    goal: Some("Newer subjectless".into()),
                    budget: 1600,
                    estimated_tokens: 220,
                    estimated_source_tokens: 460,
                    pager_manifest_hash: "manifest-subjectless".into(),
                    recall_mode: "off".into(),
                    recall_used: false,
                    recall_reason: "disabled".into(),
                    telemetry: crate::core::memory_os::MemoryOsCheckpointTelemetry {
                        current_fact_count: 1,
                        recent_change_count: 0,
                        live_claim_count: 0,
                        open_obligation_count: 1,
                        artifact_handle_count: 0,
                        failure_count: 0,
                    },
                    selected_items: vec![crate::core::memory_os::MemoryOsPacketSelection {
                        section: "open_obligations".into(),
                        kind: "obligation".into(),
                        summary: "Newer subjectless".into(),
                        token_estimate: 10,
                        score: 60,
                        artifact_id: None,
                        subject: None,
                        provenance: vec!["worldview".into()],
                    }],
                    exclusions: Vec::new(),
                    reentry: crate::core::memory_os::MemoryOsCheckpointReentry {
                        recommended_command: "munin resume --format prompt".into(),
                        current_recommendation: Some("Newer subjectless".into()),
                        first_question: "Newer?".into(),
                        first_verification: "Verify newer".into(),
                    },
                },
            )
            .expect("newer subjectless checkpoint");

        let subject = tracker
            .latest_memory_os_action_subject("munin resume --format prompt")
            .expect("latest subject");
        assert_eq!(subject, None);

        std::env::remove_var("MUNIN_MEMORYOS_JOURNAL_V1");
        std::env::remove_var("MUNIN_MEMORYOS_DUAL_WRITE_V1");
        std::env::remove_var("MUNIN_MEMORYOS_CHECKPOINT_V1");
        std::env::remove_var("MUNIN_MEMORYOS_ACTION_V1");
    }
}
