use anyhow::Result;
use chrono::{DateTime, Utc};
use rusqlite::params;
use serde::Serialize;
use std::collections::{BTreeMap, HashMap};

use super::{
    command_project_hint, project_filter_params, Tracker, COMMAND_TELEMETRY_FILTER_SQL,
    MUNIN_CANONICAL_INPUT_SQL, MUNIN_CANONICAL_OUTPUT_SQL, MUNIN_CANONICAL_SAVED_SQL,
};

/// Aggregated statistics across all recorded commands.
///
/// Provides overall metrics and breakdowns by command and by day.
/// Returned by [`Tracker::get_summary`].
#[derive(Debug)]
pub struct GainSummary {
    /// Total number of tracked shell commands recorded
    pub total_commands: usize,
    /// Total tracked events recorded (commands + context builds)
    pub tracked_events: usize,
    /// Total input tokens across tracked commands and context builds
    pub total_input: usize,
    /// Total output tokens across tracked commands and context builds
    pub total_output: usize,
    /// Total tokens saved across tracked commands and context builds
    pub total_saved: usize,
    /// Total command-only input tokens
    pub command_input_tokens: usize,
    /// Total command-only output tokens
    pub command_output_tokens: usize,
    /// Total command-only saved tokens
    pub command_saved_tokens: usize,
    /// Total context-only input tokens
    pub context_input_tokens: usize,
    /// Total context-only output tokens
    pub context_output_tokens: usize,
    /// Average savings percentage across tracked commands and context builds
    pub avg_savings_pct: f64,
    /// Total execution time across tracked shell commands (milliseconds)
    pub total_time_ms: u64,
    /// Average execution time per command (milliseconds)
    pub avg_time_ms: u64,
    /// Top 10 commands by tokens saved: (cmd, count, saved, avg_pct, avg_time_ms)
    pub by_command: Vec<(String, usize, usize, f64, u64)>,
    /// Richer per-command context for distinguishing tiny raw outputs from weak filtering.
    pub by_command_detail: Vec<CommandGainDetail>,
    /// Last 30 days of activity: (date, saved_tokens)
    pub by_day: Vec<(String, usize)>,
    /// Estimated savings from replay suppression/artifactization
    pub replay_suppression_saved: usize,
    /// Estimated savings from normal filtering/compression excluding replay suppression
    pub compression_saved: usize,
    /// Number of artifactized outputs emitted
    pub artifacts_created: usize,
    /// Number of repeated outputs collapsed to unchanged markers
    pub repeated_outputs_suppressed: usize,
    /// Number of changed outputs summarized as deltas
    pub changed_outputs_summarized: usize,
    /// Estimated savings from compiled context reuse
    pub context_reuse_saved: usize,
    /// Number of compiled context/resume payloads generated
    pub context_compilations: usize,
    /// Number of compiled context/resume payloads that reused non-empty state
    pub context_reuse_builds: usize,
    /// Number of live claim leases reused in compiled context
    pub claim_reuse_count: usize,
    /// Number of auto-synthesized failure obligations reused in compiled context/resume
    pub failure_reuse_count: usize,
}

#[derive(Debug, Clone, Default)]
pub struct ArtifactSummary {
    pub replay_suppression_saved: usize,
    pub artifacts_created: usize,
    pub repeated_outputs_suppressed: usize,
    pub changed_outputs_summarized: usize,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Default)]
pub struct ContextSummary {
    pub estimated_source_tokens: usize,
    pub rendered_tokens: usize,
    pub context_reuse_saved: usize,
    pub context_compilations: usize,
    pub claim_reuse_count: usize,
    /// Auto-synthesized obligations derived from active failures
    pub failure_reuse_count: usize,
}

/// Daily statistics for token savings and execution metrics.
///
/// Serializable to JSON for export via `context gain --daily --format json`.
///
/// # JSON Schema
///
/// ```json
/// {
///   "date": "2026-02-03",
///   "commands": 42,
///   "input_tokens": 15420,
///   "output_tokens": 3842,
///   "saved_tokens": 11578,
///   "savings_pct": 75.08,
///   "total_time_ms": 8450,
///   "avg_time_ms": 201
/// }
/// ```
#[derive(Debug, Serialize)]
pub struct DayStats {
    /// ISO date (YYYY-MM-DD)
    pub date: String,
    /// Number of tracked shell commands executed this day
    pub commands: usize,
    /// Number of context builds generated this day
    pub context_builds: usize,
    /// Number of tracked events this day (commands + context builds)
    pub tracked_events: usize,
    /// Total input tokens for this day across commands and context builds
    pub input_tokens: usize,
    /// Total output tokens for this day across commands and context builds
    pub output_tokens: usize,
    /// Total tokens saved this day across commands and context builds
    pub saved_tokens: usize,
    /// Savings percentage for this day
    pub savings_pct: f64,
    /// Total execution time for this day (milliseconds)
    pub total_time_ms: u64,
    /// Average execution time per command (milliseconds)
    pub avg_time_ms: u64,
}

/// Weekly statistics for token savings and execution metrics.
///
/// Serializable to JSON for export via `context gain --weekly --format json`.
/// Weeks start on Sunday (SQLite default).
#[derive(Debug, Serialize)]
pub struct WeekStats {
    /// Week start date (YYYY-MM-DD)
    pub week_start: String,
    /// Week end date (YYYY-MM-DD)
    pub week_end: String,
    /// Number of tracked shell commands executed this week
    pub commands: usize,
    /// Number of context builds generated this week
    pub context_builds: usize,
    /// Number of tracked events this week (commands + context builds)
    pub tracked_events: usize,
    /// Total input tokens for this week across commands and context builds
    pub input_tokens: usize,
    /// Total output tokens for this week across commands and context builds
    pub output_tokens: usize,
    /// Total tokens saved this week across commands and context builds
    pub saved_tokens: usize,
    /// Savings percentage for this week
    pub savings_pct: f64,
    /// Total execution time for this week (milliseconds)
    pub total_time_ms: u64,
    /// Average execution time per command (milliseconds)
    pub avg_time_ms: u64,
}

/// Monthly statistics for token savings and execution metrics.
///
/// Serializable to JSON for export via `context gain --monthly --format json`.
#[derive(Debug, Serialize)]
pub struct MonthStats {
    /// Month identifier (YYYY-MM)
    pub month: String,
    /// Number of tracked shell commands executed this month
    pub commands: usize,
    /// Number of context builds generated this month
    pub context_builds: usize,
    /// Number of tracked events this month (commands + context builds)
    pub tracked_events: usize,
    /// Total input tokens for this month across commands and context builds
    pub input_tokens: usize,
    /// Total output tokens for this month across commands and context builds
    pub output_tokens: usize,
    /// Total tokens saved this month across commands and context builds
    pub saved_tokens: usize,
    /// Savings percentage for this month
    pub savings_pct: f64,
    /// Total execution time for this month (milliseconds)
    pub total_time_ms: u64,
    /// Average execution time per command (milliseconds)
    pub avg_time_ms: u64,
}

/// Type alias for command statistics tuple: (command, count, saved_tokens, avg_savings_pct, avg_time_ms)
type CommandStats = (String, usize, usize, f64, u64);

#[derive(Debug, Clone, PartialEq)]
pub struct CommandGainDetail {
    pub command: String,
    pub count: usize,
    pub saved_tokens: usize,
    pub avg_savings_pct: f64,
    pub weighted_savings_pct: f64,
    pub avg_time_ms: u64,
    pub avg_input_tokens: usize,
    pub avg_output_tokens: usize,
    pub tiny_input_runs: usize,
    pub large_input_runs: usize,
    pub max_input_tokens: usize,
    pub max_saved_tokens: usize,
}

#[derive(Debug, Clone, Copy, Default)]
struct GainRollupOptions {
    by_command: bool,
    by_day: bool,
    by_week: bool,
    by_month: bool,
}

#[derive(Debug, Clone, Default)]
pub(super) struct AggregateStats {
    pub(super) command_count: usize,
    pub(super) context_builds: usize,
    pub(super) input_tokens: usize,
    pub(super) output_tokens: usize,
    pub(super) saved_tokens: usize,
    pub(super) total_time_ms: u64,
    pub(super) savings_pct_sum: f64,
    pub(super) tiny_input_runs: usize,
    pub(super) large_input_runs: usize,
    pub(super) max_input_tokens: usize,
    pub(super) max_saved_tokens: usize,
}

impl AggregateStats {
    fn add_command_row(
        &mut self,
        input_tokens: usize,
        output_tokens: usize,
        saved_tokens: usize,
        total_time_ms: u64,
    ) {
        self.command_count += 1;
        self.input_tokens += input_tokens;
        self.output_tokens += output_tokens;
        self.saved_tokens += saved_tokens;
        self.total_time_ms += total_time_ms;
        if input_tokens <= 50 {
            self.tiny_input_runs += 1;
        }
        if input_tokens >= 500 {
            self.large_input_runs += 1;
        }
        self.max_input_tokens = self.max_input_tokens.max(input_tokens);
        self.max_saved_tokens = self.max_saved_tokens.max(saved_tokens);
        self.savings_pct_sum += if input_tokens > 0 {
            (saved_tokens as f64 / input_tokens as f64) * 100.0
        } else {
            0.0
        };
    }

    fn add_context_row(&mut self, input_tokens: usize, output_tokens: usize, saved_tokens: usize) {
        self.context_builds += 1;
        self.input_tokens += input_tokens;
        self.output_tokens += output_tokens;
        self.saved_tokens += saved_tokens;
    }

    fn avg_command_savings_pct(&self) -> f64 {
        if self.command_count > 0 {
            self.savings_pct_sum / self.command_count as f64
        } else {
            0.0
        }
    }

    fn avg_time_ms(&self) -> u64 {
        if self.command_count > 0 {
            self.total_time_ms / self.command_count as u64
        } else {
            0
        }
    }

    fn avg_input_tokens(&self) -> usize {
        if self.command_count > 0 {
            self.input_tokens / self.command_count
        } else {
            0
        }
    }

    fn avg_output_tokens(&self) -> usize {
        if self.command_count > 0 {
            self.output_tokens / self.command_count
        } else {
            0
        }
    }

    fn aggregate_savings_pct(&self) -> f64 {
        if self.input_tokens > 0 {
            (self.saved_tokens as f64 / self.input_tokens as f64) * 100.0
        } else {
            0.0
        }
    }

    fn tracked_events(&self) -> usize {
        self.command_count + self.context_builds
    }
}

#[derive(Debug, Clone, Default)]
pub(super) struct GainRollup {
    total_commands: usize,
    total_input: usize,
    total_output: usize,
    total_saved: usize,
    total_time_ms: u64,
    command_input_tokens: usize,
    command_output_tokens: usize,
    command_saved_tokens: usize,
    context_estimated_source_tokens: usize,
    context_rendered_tokens: usize,
    context_reuse_saved: usize,
    context_compilations: usize,
    context_reuse_builds: usize,
    claim_reuse_count: usize,
    failure_reuse_count: usize,
    pub(super) by_command: HashMap<String, AggregateStats>,
    by_day: BTreeMap<String, AggregateStats>,
    by_week: BTreeMap<String, WeekAggregate>,
    by_month: BTreeMap<String, AggregateStats>,
}

#[derive(Debug, Clone, Default)]
struct WeekAggregate {
    week_end: String,
    stats: AggregateStats,
}

#[derive(Debug)]
struct GainRollupRow {
    day: String,
    week_start: String,
    week_end: String,
    month: String,
    context_cmd: Option<String>,
    is_command: bool,
    input_tokens: usize,
    output_tokens: usize,
    saved_tokens: usize,
    total_time_ms: u64,
    context_compilations: usize,
    claim_reuse_count: usize,
    failure_reuse_count: usize,
}

impl GainRollup {
    fn apply_row(&mut self, row: GainRollupRow, options: GainRollupOptions) {
        let GainRollupRow {
            day,
            week_start,
            week_end,
            month,
            context_cmd,
            is_command,
            input_tokens,
            output_tokens,
            saved_tokens,
            total_time_ms,
            context_compilations,
            claim_reuse_count,
            failure_reuse_count,
        } = row;

        self.total_input += input_tokens;
        self.total_output += output_tokens;
        self.total_saved += saved_tokens;
        if is_command {
            self.total_commands += 1;
            self.total_time_ms += total_time_ms;
            self.command_input_tokens += input_tokens;
            self.command_output_tokens += output_tokens;
            self.command_saved_tokens += saved_tokens;
        } else {
            self.context_estimated_source_tokens += input_tokens;
            self.context_rendered_tokens += output_tokens;
            self.context_reuse_saved += saved_tokens;
            if input_tokens > 0 {
                self.context_reuse_builds += 1;
            }
        }
        self.context_compilations += context_compilations;
        self.claim_reuse_count += claim_reuse_count;
        self.failure_reuse_count += failure_reuse_count;

        if options.by_day {
            let aggregate = self.by_day.entry(day).or_default();
            if is_command {
                aggregate.add_command_row(input_tokens, output_tokens, saved_tokens, total_time_ms);
            } else {
                aggregate.add_context_row(input_tokens, output_tokens, saved_tokens);
            }
        }

        if options.by_week {
            let week_end_for_insert = week_end.clone();
            self.by_week
                .entry(week_start)
                .and_modify(|aggregate| {
                    aggregate.week_end = week_end.clone();
                    if is_command {
                        aggregate.stats.add_command_row(
                            input_tokens,
                            output_tokens,
                            saved_tokens,
                            total_time_ms,
                        );
                    } else {
                        aggregate
                            .stats
                            .add_context_row(input_tokens, output_tokens, saved_tokens);
                    }
                })
                .or_insert_with(|| WeekAggregate {
                    week_end: week_end_for_insert,
                    stats: {
                        let mut stats = AggregateStats::default();
                        if is_command {
                            stats.add_command_row(
                                input_tokens,
                                output_tokens,
                                saved_tokens,
                                total_time_ms,
                            );
                        } else {
                            stats.add_context_row(input_tokens, output_tokens, saved_tokens);
                        }
                        stats
                    },
                });
        }

        if options.by_month {
            let aggregate = self.by_month.entry(month).or_default();
            if is_command {
                aggregate.add_command_row(input_tokens, output_tokens, saved_tokens, total_time_ms);
            } else {
                aggregate.add_context_row(input_tokens, output_tokens, saved_tokens);
            }
        }

        if options.by_command && is_command {
            if let Some(command) = context_cmd {
                self.by_command.entry(command).or_default().add_command_row(
                    input_tokens,
                    output_tokens,
                    saved_tokens,
                    total_time_ms,
                );
            }
        }
    }

    fn command_stats(&self) -> Vec<CommandStats> {
        let mut command_stats: Vec<_> = self
            .by_command
            .iter()
            .map(|(command, aggregate)| {
                (
                    command.clone(),
                    aggregate.command_count,
                    aggregate.saved_tokens,
                    aggregate.avg_command_savings_pct(),
                    aggregate.avg_time_ms(),
                )
            })
            .collect();

        command_stats.sort_by(|a, b| {
            b.2.cmp(&a.2)
                .then_with(|| a.0.cmp(&b.0))
                .then_with(|| a.1.cmp(&b.1))
        });
        command_stats.truncate(10);
        command_stats
    }

    pub(super) fn command_gain_details(&self) -> Vec<CommandGainDetail> {
        let mut details: Vec<_> = self
            .by_command
            .iter()
            .map(|(command, aggregate)| CommandGainDetail {
                command: command.clone(),
                count: aggregate.command_count,
                saved_tokens: aggregate.saved_tokens,
                avg_savings_pct: aggregate.avg_command_savings_pct(),
                weighted_savings_pct: aggregate.aggregate_savings_pct(),
                avg_time_ms: aggregate.avg_time_ms(),
                avg_input_tokens: aggregate.avg_input_tokens(),
                avg_output_tokens: aggregate.avg_output_tokens(),
                tiny_input_runs: aggregate.tiny_input_runs,
                large_input_runs: aggregate.large_input_runs,
                max_input_tokens: aggregate.max_input_tokens,
                max_saved_tokens: aggregate.max_saved_tokens,
            })
            .collect();

        details.sort_by(|left, right| {
            right
                .saved_tokens
                .cmp(&left.saved_tokens)
                .then_with(|| left.command.cmp(&right.command))
                .then_with(|| left.count.cmp(&right.count))
        });
        details.truncate(10);
        details
    }

    fn recent_days(&self, limit: usize) -> Vec<(String, usize)> {
        let mut days: Vec<_> = self
            .by_day
            .iter()
            .map(|(day, aggregate)| (day.clone(), aggregate.saved_tokens))
            .collect();
        if days.len() > limit {
            days = days.split_off(days.len() - limit);
        }
        days
    }

    fn day_stats(&self) -> Vec<DayStats> {
        Self::period_stats_from_map(&self.by_day, |date, aggregate| DayStats {
            date: date.clone(),
            commands: aggregate.command_count,
            context_builds: aggregate.context_builds,
            tracked_events: aggregate.tracked_events(),
            input_tokens: aggregate.input_tokens,
            output_tokens: aggregate.output_tokens,
            saved_tokens: aggregate.saved_tokens,
            savings_pct: aggregate.aggregate_savings_pct(),
            total_time_ms: aggregate.total_time_ms,
            avg_time_ms: aggregate.avg_time_ms(),
        })
    }

    fn week_stats(&self) -> Vec<WeekStats> {
        self.by_week
            .iter()
            .map(|(week_start, aggregate)| WeekStats {
                week_start: week_start.clone(),
                week_end: aggregate.week_end.clone(),
                commands: aggregate.stats.command_count,
                context_builds: aggregate.stats.context_builds,
                tracked_events: aggregate.stats.tracked_events(),
                input_tokens: aggregate.stats.input_tokens,
                output_tokens: aggregate.stats.output_tokens,
                saved_tokens: aggregate.stats.saved_tokens,
                savings_pct: aggregate.stats.aggregate_savings_pct(),
                total_time_ms: aggregate.stats.total_time_ms,
                avg_time_ms: aggregate.stats.avg_time_ms(),
            })
            .collect()
    }

    fn month_stats(&self) -> Vec<MonthStats> {
        Self::period_stats_from_map(&self.by_month, |month, aggregate| MonthStats {
            month: month.clone(),
            commands: aggregate.command_count,
            context_builds: aggregate.context_builds,
            tracked_events: aggregate.tracked_events(),
            input_tokens: aggregate.input_tokens,
            output_tokens: aggregate.output_tokens,
            saved_tokens: aggregate.saved_tokens,
            savings_pct: aggregate.aggregate_savings_pct(),
            total_time_ms: aggregate.total_time_ms,
            avg_time_ms: aggregate.avg_time_ms(),
        })
    }

    fn period_stats_from_map<T, F>(map: &BTreeMap<String, AggregateStats>, mut convert: F) -> Vec<T>
    where
        F: FnMut(&String, &AggregateStats) -> T,
    {
        map.iter()
            .map(|(label, aggregate)| convert(label, aggregate))
            .collect()
    }
}

impl Tracker {
    /// Get overall summary statistics across all recorded commands.
    ///
    /// Returns aggregated metrics including:
    /// - Total commands, tokens (input/output/saved)
    /// - Average savings percentage and execution time
    /// - Top 10 commands by tokens saved
    /// - Last 30 days of activity
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use munin_memory::tracking::Tracker;
    ///
    /// let tracker = Tracker::new()?;
    /// let summary = tracker.get_summary()?;
    /// println!("Saved {} tokens ({:.1}%)",
    ///     summary.total_saved, summary.avg_savings_pct);
    /// # Ok::<(), anyhow::Error>(())
    /// ```
    #[allow(dead_code)]
    pub fn get_summary(&self) -> Result<GainSummary> {
        self.get_summary_filtered(None) // delegate to filtered variant
    }

    // Gain rollup query builder and collector for summary and period views.
    fn gain_rollup_query(include_since: bool, include_until: bool) -> String {
        let command_since_filter = if include_since {
            "timestamp >= ?4 AND "
        } else {
            ""
        };
        let context_since_filter = if include_since {
            "timestamp >= ?4 AND "
        } else {
            ""
        };
        let command_until_filter = if include_until {
            "timestamp <= ?5 AND "
        } else {
            ""
        };
        let context_until_filter = if include_until {
            "timestamp <= ?5 AND "
        } else {
            ""
        };

        format!(
            "SELECT day_bucket, week_start, week_end, month_bucket, context_cmd, is_command, input_tokens, output_tokens, saved_tokens, total_time_ms, context_compilations, claim_reuse_count, failure_reuse_count\n             FROM (\n                SELECT\n                    DATE(timestamp) AS day_bucket,\n                    DATE(timestamp, 'weekday 0', '-6 days') AS week_start,\n                    DATE(timestamp, 'weekday 0') AS week_end,\n                    strftime('%Y-%m', timestamp) AS month_bucket,\n                    context_cmd,\n                    1 AS is_command,\n                    input_tokens,\n                    output_tokens,\n                    saved_tokens,\n                    exec_time_ms AS total_time_ms,\n                    0 AS context_compilations,\n                    0 AS claim_reuse_count,\n                    0 AS failure_reuse_count\n                 FROM commands\n                 WHERE {command_since_filter}{command_until_filter}((?1 IS NULL OR project_path = ?1 OR project_path GLOB ?2)\n                   OR instr(original_cmd, ?3) > 0\n                   OR instr(context_cmd, ?3) > 0)\n                   AND {command_filter}\n                 UNION ALL\n                 SELECT\n                    DATE(timestamp) AS day_bucket,\n                    DATE(timestamp, 'weekday 0', '-6 days') AS week_start,\n                    DATE(timestamp, 'weekday 0') AS week_end,\n                    strftime('%Y-%m', timestamp) AS month_bucket,\n                    NULL AS context_cmd,\n                    0 AS is_command,\n                    {input_sql} AS input_tokens,\n                    {output_sql} AS output_tokens,\n                    {saved_sql} AS saved_tokens,\n                    0 AS total_time_ms,\n                    1 AS context_compilations,\n                    COALESCE(live_claim_count, 0) AS claim_reuse_count,\n                    COALESCE(failure_count, 0) AS failure_reuse_count\n                 FROM context_events\n                 WHERE {context_since_filter}{context_until_filter}(?1 IS NULL OR project_path = ?1 OR project_path GLOB ?2)\n             )",
            command_since_filter = command_since_filter,
            context_since_filter = context_since_filter,
            command_until_filter = command_until_filter,
            context_until_filter = context_until_filter,
            command_filter = COMMAND_TELEMETRY_FILTER_SQL,
            input_sql = MUNIN_CANONICAL_INPUT_SQL,
            output_sql = MUNIN_CANONICAL_OUTPUT_SQL,
            saved_sql = MUNIN_CANONICAL_SAVED_SQL,
        )
    }

    fn collect_gain_rollup(
        &self,
        project_path: Option<&str>,
        since: Option<DateTime<Utc>>,
        until: Option<DateTime<Utc>>,
        options: GainRollupOptions,
    ) -> Result<GainRollup> {
        let (project_exact, project_glob) = project_filter_params(project_path);
        let project_hint = command_project_hint(project_path);
        let query = Self::gain_rollup_query(since.is_some(), until.is_some());
        let mut stmt = self.conn.prepare(&query)?;
        let mut rollup = GainRollup::default();

        let mut map_row = |row: &rusqlite::Row<'_>| -> rusqlite::Result<GainRollupRow> {
            Ok(GainRollupRow {
                day: row.get(0)?,
                week_start: row.get(1)?,
                week_end: row.get(2)?,
                month: row.get(3)?,
                context_cmd: row.get(4)?,
                is_command: row.get::<_, i64>(5)? != 0,
                input_tokens: row.get::<_, i64>(6)? as usize,
                output_tokens: row.get::<_, i64>(7)? as usize,
                saved_tokens: row.get::<_, i64>(8)? as usize,
                total_time_ms: row.get::<_, i64>(9)? as u64,
                context_compilations: row.get::<_, i64>(10)? as usize,
                claim_reuse_count: row.get::<_, i64>(11)? as usize,
                failure_reuse_count: row.get::<_, i64>(12)? as usize,
            })
        };

        match (since, until) {
            (Some(cutoff), Some(end)) => {
                let rows = stmt.query_map(
                    params![
                        project_exact.as_deref(),
                        project_glob.as_deref(),
                        project_hint.as_deref(),
                        cutoff.to_rfc3339(),
                        end.to_rfc3339()
                    ],
                    &mut map_row,
                )?;
                for row in rows {
                    rollup.apply_row(row?, options);
                }
            }
            (Some(cutoff), None) => {
                let rows = stmt.query_map(
                    params![
                        project_exact.as_deref(),
                        project_glob.as_deref(),
                        project_hint.as_deref(),
                        cutoff.to_rfc3339()
                    ],
                    &mut map_row,
                )?;
                for row in rows {
                    rollup.apply_row(row?, options);
                }
            }
            (None, Some(end)) => {
                let rows = stmt.query_map(
                    params![
                        project_exact.as_deref(),
                        project_glob.as_deref(),
                        project_hint.as_deref(),
                        Option::<String>::None,
                        end.to_rfc3339()
                    ],
                    &mut map_row,
                )?;
                for row in rows {
                    rollup.apply_row(row?, options);
                }
            }
            (None, None) => {
                let rows = stmt.query_map(
                    params![
                        project_exact.as_deref(),
                        project_glob.as_deref(),
                        project_hint.as_deref()
                    ],
                    &mut map_row,
                )?;
                for row in rows {
                    rollup.apply_row(row?, options);
                }
            }
        }

        Ok(rollup)
    }

    /// Get summary statistics filtered by project path. // added
    ///
    /// When `project_path` is `Some`, matches the exact working directory
    /// or any subdirectory (prefix match with path separator).
    pub fn get_summary_filtered(&self, project_path: Option<&str>) -> Result<GainSummary> {
        let rollup = self.collect_gain_rollup(
            project_path,
            None,
            None,
            GainRollupOptions {
                by_command: true,
                by_day: true,
                ..GainRollupOptions::default()
            },
        )?;
        let artifact_summary = self.get_artifact_summary_filtered(project_path)?;
        let by_command = rollup.command_stats();
        let by_command_detail = rollup.command_gain_details();
        let by_day = rollup.recent_days(30);

        let avg_savings_pct = if rollup.total_input > 0 {
            (rollup.total_saved as f64 / rollup.total_input as f64) * 100.0
        } else {
            0.0
        };

        let avg_time_ms = if rollup.total_commands > 0 {
            rollup.total_time_ms / rollup.total_commands as u64
        } else {
            0
        };

        Ok(GainSummary {
            total_commands: rollup.total_commands,
            tracked_events: rollup.total_commands + rollup.context_compilations,
            total_input: rollup.total_input,
            total_output: rollup.total_output,
            total_saved: rollup.total_saved,
            command_input_tokens: rollup.command_input_tokens,
            command_output_tokens: rollup.command_output_tokens,
            command_saved_tokens: rollup.command_saved_tokens,
            context_input_tokens: rollup.context_estimated_source_tokens,
            context_output_tokens: rollup.context_rendered_tokens,
            avg_savings_pct,
            total_time_ms: rollup.total_time_ms,
            avg_time_ms,
            by_command,
            by_command_detail,
            by_day,
            replay_suppression_saved: artifact_summary.replay_suppression_saved,
            compression_saved: rollup
                .command_saved_tokens
                .saturating_sub(artifact_summary.replay_suppression_saved),
            artifacts_created: artifact_summary.artifacts_created,
            repeated_outputs_suppressed: artifact_summary.repeated_outputs_suppressed,
            changed_outputs_summarized: artifact_summary.changed_outputs_summarized,
            context_reuse_saved: rollup.context_reuse_saved,
            context_compilations: rollup.context_compilations,
            context_reuse_builds: rollup.context_reuse_builds,
            claim_reuse_count: rollup.claim_reuse_count,
            failure_reuse_count: rollup.failure_reuse_count,
        })
    }

    pub fn get_summary_filtered_since(
        &self,
        project_path: Option<&str>,
        since: chrono::DateTime<chrono::Utc>,
    ) -> Result<GainSummary> {
        let rollup = self.collect_gain_rollup(
            project_path,
            Some(since),
            None,
            GainRollupOptions {
                by_command: true,
                ..GainRollupOptions::default()
            },
        )?;
        let artifact_summary = self.get_artifact_summary_filtered_since(project_path, since)?;
        let by_command = rollup.command_stats();
        let by_command_detail = rollup.command_gain_details();

        let avg_savings_pct = if rollup.total_input > 0 {
            (rollup.total_saved as f64 / rollup.total_input as f64) * 100.0
        } else {
            0.0
        };

        let avg_time_ms = if rollup.total_commands > 0 {
            rollup.total_time_ms / rollup.total_commands as u64
        } else {
            0
        };

        Ok(GainSummary {
            total_commands: rollup.total_commands,
            tracked_events: rollup.total_commands + rollup.context_compilations,
            total_input: rollup.total_input,
            total_output: rollup.total_output,
            total_saved: rollup.total_saved,
            command_input_tokens: rollup.command_input_tokens,
            command_output_tokens: rollup.command_output_tokens,
            command_saved_tokens: rollup.command_saved_tokens,
            context_input_tokens: rollup.context_estimated_source_tokens,
            context_output_tokens: rollup.context_rendered_tokens,
            avg_savings_pct,
            total_time_ms: rollup.total_time_ms,
            avg_time_ms,
            by_command,
            by_command_detail,
            by_day: Vec::new(),
            replay_suppression_saved: artifact_summary.replay_suppression_saved,
            compression_saved: rollup
                .command_saved_tokens
                .saturating_sub(artifact_summary.replay_suppression_saved),
            artifacts_created: artifact_summary.artifacts_created,
            repeated_outputs_suppressed: artifact_summary.repeated_outputs_suppressed,
            changed_outputs_summarized: artifact_summary.changed_outputs_summarized,
            context_reuse_saved: rollup.context_reuse_saved,
            context_compilations: rollup.context_compilations,
            context_reuse_builds: rollup.context_reuse_builds,
            claim_reuse_count: rollup.claim_reuse_count,
            failure_reuse_count: rollup.failure_reuse_count,
        })
    }

    pub fn get_summary_filtered_between(
        &self,
        project_path: Option<&str>,
        since: chrono::DateTime<chrono::Utc>,
        until: chrono::DateTime<chrono::Utc>,
    ) -> Result<GainSummary> {
        let rollup = self.collect_gain_rollup(
            project_path,
            Some(since),
            Some(until),
            GainRollupOptions {
                by_command: true,
                ..GainRollupOptions::default()
            },
        )?;
        let artifact_summary =
            self.get_artifact_summary_filtered_between(project_path, since, until)?;
        let by_command = rollup.command_stats();
        let by_command_detail = rollup.command_gain_details();
        let avg_savings_pct = if rollup.total_input > 0 {
            (rollup.total_saved as f64 / rollup.total_input as f64) * 100.0
        } else {
            0.0
        };
        let avg_time_ms = if rollup.total_commands > 0 {
            rollup.total_time_ms / rollup.total_commands as u64
        } else {
            0
        };

        Ok(GainSummary {
            total_commands: rollup.total_commands,
            tracked_events: rollup.total_commands + rollup.context_compilations,
            total_input: rollup.total_input,
            total_output: rollup.total_output,
            total_saved: rollup.total_saved,
            command_input_tokens: rollup.command_input_tokens,
            command_output_tokens: rollup.command_output_tokens,
            command_saved_tokens: rollup.command_saved_tokens,
            context_input_tokens: rollup.context_estimated_source_tokens,
            context_output_tokens: rollup.context_rendered_tokens,
            context_reuse_saved: rollup.context_reuse_saved,
            compression_saved: rollup
                .command_saved_tokens
                .saturating_sub(artifact_summary.replay_suppression_saved),
            replay_suppression_saved: artifact_summary.replay_suppression_saved,
            avg_savings_pct,
            total_time_ms: rollup.total_time_ms,
            avg_time_ms,
            by_command,
            by_command_detail,
            by_day: Vec::new(),
            artifacts_created: artifact_summary.artifacts_created,
            repeated_outputs_suppressed: artifact_summary.repeated_outputs_suppressed,
            changed_outputs_summarized: artifact_summary.changed_outputs_summarized,
            context_compilations: rollup.context_compilations,
            context_reuse_builds: rollup.context_reuse_builds,
            claim_reuse_count: rollup.claim_reuse_count,
            failure_reuse_count: rollup.failure_reuse_count,
        })
    }

    /// Get daily statistics for all recorded days.
    ///
    /// Returns one [`DayStats`] per day with commands executed, tokens saved,
    /// and execution time metrics. Results are ordered chronologically (oldest first).
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use munin_memory::tracking::Tracker;
    ///
    /// let tracker = Tracker::new()?;
    /// let days = tracker.get_all_days()?;
    /// for day in days.iter().take(7) {
    ///     println!("{}: {} commands, {} tokens saved",
    ///         day.date, day.commands, day.saved_tokens);
    /// }
    /// # Ok::<(), anyhow::Error>(())
    /// ```
    pub fn get_all_days(&self) -> Result<Vec<DayStats>> {
        self.get_all_days_filtered(None) // delegate to filtered variant
    }

    /// Get daily statistics filtered by project path. // added
    pub fn get_all_days_filtered(&self, project_path: Option<&str>) -> Result<Vec<DayStats>> {
        let rollup = self.collect_gain_rollup(
            project_path,
            None,
            None,
            GainRollupOptions {
                by_day: true,
                ..GainRollupOptions::default()
            },
        )?;
        Ok(rollup.day_stats())
    }

    /// Get weekly statistics grouped by week.
    ///
    /// Returns one [`WeekStats`] per week with aggregated metrics.
    /// Weeks start on Sunday (SQLite default). Results ordered chronologically.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use munin_memory::tracking::Tracker;
    ///
    /// let tracker = Tracker::new()?;
    /// let weeks = tracker.get_by_week()?;
    /// for week in weeks {
    ///     println!("{} to {}: {} tokens saved",
    ///         week.week_start, week.week_end, week.saved_tokens);
    /// }
    /// # Ok::<(), anyhow::Error>(())
    /// ```
    pub fn get_by_week(&self) -> Result<Vec<WeekStats>> {
        self.get_by_week_filtered(None) // delegate to filtered variant
    }

    /// Get weekly statistics filtered by project path. // added
    pub fn get_by_week_filtered(&self, project_path: Option<&str>) -> Result<Vec<WeekStats>> {
        let rollup = self.collect_gain_rollup(
            project_path,
            None,
            None,
            GainRollupOptions {
                by_week: true,
                ..GainRollupOptions::default()
            },
        )?;
        Ok(rollup.week_stats())
    }

    /// Get monthly statistics grouped by month.
    ///
    /// Returns one [`MonthStats`] per month (YYYY-MM format) with aggregated metrics.
    /// Results ordered chronologically.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use munin_memory::tracking::Tracker;
    ///
    /// let tracker = Tracker::new()?;
    /// let months = tracker.get_by_month()?;
    /// for month in months {
    ///     println!("{}: {} tokens saved ({:.1}%)",
    ///         month.month, month.saved_tokens, month.savings_pct);
    /// }
    /// # Ok::<(), anyhow::Error>(())
    /// ```
    pub fn get_by_month(&self) -> Result<Vec<MonthStats>> {
        self.get_by_month_filtered(None) // delegate to filtered variant
    }

    /// Get monthly statistics filtered by project path. // added
    pub fn get_by_month_filtered(&self, project_path: Option<&str>) -> Result<Vec<MonthStats>> {
        let rollup = self.collect_gain_rollup(
            project_path,
            None,
            None,
            GainRollupOptions {
                by_month: true,
                ..GainRollupOptions::default()
            },
        )?;
        Ok(rollup.month_stats())
    }
    pub fn get_artifact_summary_filtered(
        &self,
        project_path: Option<&str>,
    ) -> Result<ArtifactSummary> {
        let (project_exact, project_glob) = project_filter_params(project_path);
        let mut stmt = self.conn.prepare(
            "SELECT event_type, COUNT(*), COALESCE(SUM(saved_tokens), 0)
             FROM artifact_events
             WHERE (?1 IS NULL OR project_path = ?1 OR project_path GLOB ?2)
             GROUP BY event_type",
        )?;

        let rows = stmt.query_map(params![project_exact, project_glob], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)? as usize,
                row.get::<_, i64>(2)? as usize,
            ))
        })?;

        let mut summary = ArtifactSummary::default();
        for row in rows {
            let (event_type, count, saved_tokens) = row?;
            summary.replay_suppression_saved += saved_tokens;
            match event_type.as_str() {
                "new" => summary.artifacts_created += count,
                "unchanged" => summary.repeated_outputs_suppressed += count,
                "delta" => summary.changed_outputs_summarized += count,
                _ => {}
            }
        }
        Ok(summary)
    }

    pub fn get_artifact_summary_filtered_since(
        &self,
        project_path: Option<&str>,
        since: chrono::DateTime<chrono::Utc>,
    ) -> Result<ArtifactSummary> {
        let cutoff = since.to_rfc3339();
        let (project_exact, project_glob) = project_filter_params(project_path);
        let mut stmt = self.conn.prepare(
            "SELECT event_type, COUNT(*), COALESCE(SUM(saved_tokens), 0)
             FROM artifact_events
             WHERE timestamp >= ?3
               AND (?1 IS NULL OR project_path = ?1 OR project_path GLOB ?2)
             GROUP BY event_type",
        )?;

        let rows = stmt.query_map(params![project_exact, project_glob, cutoff], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)? as usize,
                row.get::<_, i64>(2)? as usize,
            ))
        })?;

        let mut summary = ArtifactSummary::default();
        for row in rows {
            let (event_type, count, saved_tokens) = row?;
            summary.replay_suppression_saved += saved_tokens;
            match event_type.as_str() {
                "new" => summary.artifacts_created += count,
                "unchanged" => summary.repeated_outputs_suppressed += count,
                "delta" => summary.changed_outputs_summarized += count,
                _ => {}
            }
        }
        Ok(summary)
    }

    pub fn get_artifact_summary_filtered_between(
        &self,
        project_path: Option<&str>,
        since: chrono::DateTime<chrono::Utc>,
        until: chrono::DateTime<chrono::Utc>,
    ) -> Result<ArtifactSummary> {
        let (project_exact, project_glob) = project_filter_params(project_path);
        let mut stmt = self.conn.prepare(
            "SELECT event_type, COUNT(*), COALESCE(SUM(saved_tokens), 0)
             FROM artifact_events
             WHERE timestamp >= ?3
               AND timestamp <= ?4
               AND (?1 IS NULL OR project_path = ?1 OR project_path GLOB ?2)
             GROUP BY event_type",
        )?;
        let rows = stmt.query_map(
            params![
                project_exact,
                project_glob,
                since.to_rfc3339(),
                until.to_rfc3339()
            ],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)? as usize,
                    row.get::<_, i64>(2)? as usize,
                ))
            },
        )?;
        let mut summary = ArtifactSummary::default();
        for row in rows {
            let (event_type, count, saved) = row?;
            match event_type.as_str() {
                "new" => summary.artifacts_created += count,
                "unchanged" => summary.repeated_outputs_suppressed += count,
                "delta" => summary.changed_outputs_summarized += count,
                _ => {}
            }
            summary.replay_suppression_saved += saved;
        }
        Ok(summary)
    }

    #[allow(dead_code)]
    pub fn get_context_summary_filtered(
        &self,
        project_path: Option<&str>,
    ) -> Result<ContextSummary> {
        let (project_exact, project_glob) = project_filter_params(project_path);
        let query = format!(
            "SELECT
                COUNT(*),
                COALESCE(SUM({input_sql}), 0),
                COALESCE(SUM({output_sql}), 0),
                COALESCE(SUM({saved_sql}), 0),
                COALESCE(SUM(live_claim_count), 0),
                COALESCE(SUM(failure_count), 0)
             FROM context_events
             WHERE (?1 IS NULL OR project_path = ?1 OR project_path GLOB ?2)",
            input_sql = MUNIN_CANONICAL_INPUT_SQL,
            output_sql = MUNIN_CANONICAL_OUTPUT_SQL,
            saved_sql = MUNIN_CANONICAL_SAVED_SQL,
        );
        let (count, estimated_source_tokens, rendered_tokens, saved, claims, failures): (
            i64,
            i64,
            i64,
            i64,
            i64,
            i64,
        ) = self
            .conn
            .query_row(&query, params![project_exact, project_glob], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            })?;

        Ok(ContextSummary {
            estimated_source_tokens: estimated_source_tokens as usize,
            rendered_tokens: rendered_tokens as usize,
            context_reuse_saved: saved as usize,
            context_compilations: count as usize,
            claim_reuse_count: claims as usize,
            failure_reuse_count: failures as usize,
        })
    }

    #[allow(dead_code)]
    pub fn get_context_summary_filtered_since(
        &self,
        project_path: Option<&str>,
        since: chrono::DateTime<chrono::Utc>,
    ) -> Result<ContextSummary> {
        let cutoff = since.to_rfc3339();
        let (project_exact, project_glob) = project_filter_params(project_path);
        let query = format!(
            "SELECT
                COUNT(*),
                COALESCE(SUM({input_sql}), 0),
                COALESCE(SUM({output_sql}), 0),
                COALESCE(SUM({saved_sql}), 0),
                COALESCE(SUM(live_claim_count), 0),
                COALESCE(SUM(failure_count), 0)
             FROM context_events
             WHERE timestamp >= ?3
               AND (?1 IS NULL OR project_path = ?1 OR project_path GLOB ?2)",
            input_sql = MUNIN_CANONICAL_INPUT_SQL,
            output_sql = MUNIN_CANONICAL_OUTPUT_SQL,
            saved_sql = MUNIN_CANONICAL_SAVED_SQL,
        );
        let (count, estimated_source_tokens, rendered_tokens, saved, claims, failures): (
            i64,
            i64,
            i64,
            i64,
            i64,
            i64,
        ) = self.conn.query_row(
            &query,
            params![project_exact, project_glob, cutoff],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )?;

        Ok(ContextSummary {
            estimated_source_tokens: estimated_source_tokens as usize,
            rendered_tokens: rendered_tokens as usize,
            context_reuse_saved: saved as usize,
            context_compilations: count as usize,
            claim_reuse_count: claims as usize,
            failure_reuse_count: failures as usize,
        })
    }

    /// Count commands since a given timestamp (for telemetry).
    #[allow(dead_code)]
    pub fn count_commands_since(&self, since: chrono::DateTime<chrono::Utc>) -> Result<i64> {
        let ts = since.format("%Y-%m-%dT%H:%M:%S").to_string();
        let query = format!(
            "SELECT COUNT(*) FROM commands WHERE timestamp >= ?1 AND {}",
            COMMAND_TELEMETRY_FILTER_SQL
        );
        let count: i64 = self.conn.query_row(&query, params![ts], |row| row.get(0))?;
        Ok(count)
    }

    /// Get top N commands by frequency (for telemetry).
    #[allow(dead_code)]
    pub fn top_commands(&self, limit: usize) -> Result<Vec<String>> {
        let query = format!(
            "SELECT context_cmd, COUNT(*) as cnt FROM commands
             WHERE {}
             GROUP BY context_cmd ORDER BY cnt DESC LIMIT ?1",
            COMMAND_TELEMETRY_FILTER_SQL
        );
        let mut stmt = self.conn.prepare(&query)?;
        let rows = stmt.query_map(params![limit as i64], |row| {
            let cmd: String = row.get(0)?;
            // Extract just the command name (e.g. "context git status" Ã¢â€ â€™ "git")
            Ok(cmd.split_whitespace().nth(1).unwrap_or(&cmd).to_string())
        })?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    /// Get overall savings percentage (for telemetry).
    #[allow(dead_code)]
    pub fn overall_savings_pct(&self) -> Result<f64> {
        let query = format!(
            "SELECT COALESCE(SUM(input_tokens), 0), COALESCE(SUM(saved_tokens), 0)
             FROM (
                SELECT input_tokens, saved_tokens FROM commands WHERE {}
                UNION ALL
                SELECT {input_sql} AS input_tokens, {saved_sql} AS saved_tokens FROM context_events
              )",
            COMMAND_TELEMETRY_FILTER_SQL,
            input_sql = MUNIN_CANONICAL_INPUT_SQL,
            saved_sql = MUNIN_CANONICAL_SAVED_SQL,
        );
        let (total_input, total_saved): (i64, i64) = self
            .conn
            .query_row(&query, [], |row| Ok((row.get(0)?, row.get(1)?)))?;
        if total_input > 0 {
            Ok((total_saved as f64 / total_input as f64) * 100.0)
        } else {
            Ok(0.0)
        }
    }

    /// Get total tokens saved across all tracked commands (for telemetry).
    #[allow(dead_code)]
    pub fn total_tokens_saved(&self) -> Result<i64> {
        let query = format!(
            "SELECT COALESCE(SUM(saved_tokens), 0)
             FROM (
                SELECT saved_tokens FROM commands WHERE {}
                UNION ALL
                SELECT {saved_sql} AS saved_tokens FROM context_events
              )",
            COMMAND_TELEMETRY_FILTER_SQL,
            saved_sql = MUNIN_CANONICAL_SAVED_SQL,
        );
        let saved: i64 = self.conn.query_row(&query, [], |row| row.get(0))?;
        Ok(saved)
    }

    /// Get tokens saved in the last 24 hours (for telemetry).
    #[allow(dead_code)]
    pub fn tokens_saved_24h(&self, since: chrono::DateTime<chrono::Utc>) -> Result<i64> {
        let ts = since.format("%Y-%m-%dT%H:%M:%S").to_string();
        let query = format!(
            "SELECT COALESCE(SUM(saved_tokens), 0)
             FROM (
                SELECT timestamp, saved_tokens FROM commands WHERE {}
                UNION ALL
                SELECT timestamp, {saved_sql} AS saved_tokens FROM context_events
              )
             WHERE timestamp >= ?1",
            COMMAND_TELEMETRY_FILTER_SQL,
            saved_sql = MUNIN_CANONICAL_SAVED_SQL,
        );
        let saved: i64 = self.conn.query_row(&query, params![ts], |row| row.get(0))?;
        Ok(saved)
    }
}
