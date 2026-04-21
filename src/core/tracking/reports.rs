use anyhow::Result;
use chrono::Utc;

use super::read_model::{
    build_memory_os_active_work, build_memory_os_autonomy_findings,
    build_memory_os_epistemic_findings, build_memory_os_operating_style,
    build_memory_os_preferences, build_memory_os_recurring_themes,
    build_memory_os_semantic_fact_findings, build_memory_os_source_behavior,
    build_memory_os_top_projects, build_memory_os_user_prose_findings,
    correction_pattern_total_count,
};
use super::signals::{
    build_memory_os_behavior_changes, build_memory_os_friction_fixes,
    build_memory_os_friction_triggers, build_memory_os_imported_sources,
    build_memory_os_misunderstandings, count_user_prose_signals, detect_user_prose_durable_fixes,
    memory_os_serving_policy_lines,
};
use super::{scope_project_path_or_current, Tracker};

impl Tracker {
    pub fn get_memory_os_history_totals_all(
        &self,
    ) -> Result<crate::core::memory_os::MemoryOsHistoryTotals> {
        let journal_event_count: i64 =
            self.conn
                .query_row("SELECT COUNT(*) FROM memory_os_journal_events", [], |row| {
                    row.get(0)
                })?;
        let checkpoint_count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM memory_os_journal_events
             WHERE event_kind LIKE 'legacy.packet-checkpoint.%'",
            [],
            |row| row.get(0),
        )?;
        Ok(crate::core::memory_os::MemoryOsHistoryTotals {
            journal_event_count: journal_event_count.max(0) as usize,
            checkpoint_count: checkpoint_count.max(0) as usize,
        })
    }

    pub fn get_memory_os_onboarding_state_fast(
        &self,
    ) -> Result<crate::core::memory_os::MemoryOsOnboardingState> {
        let onboarding_status =
            crate::analytics::session_backfill::get_memory_os_session_backfill_status()?;
        let history_totals = self.get_memory_os_history_totals_all()?;
        let imported_sources = onboarding_status
            .imported_source_counts
            .iter()
            .map(
                |(source, sessions)| crate::core::memory_os::MemoryOsImportedSourceSummary {
                    source: source.clone(),
                    sessions: *sessions,
                    shell_executions: 0,
                },
            )
            .collect::<Vec<_>>();
        Ok(crate::core::memory_os::MemoryOsOnboardingState {
            schema_version: onboarding_status.schema_version,
            status: onboarding_status.status,
            started_at: onboarding_status.started_at,
            completed_at: onboarding_status.completed_at,
            sessions_processed: onboarding_status.sessions_processed,
            shells_ingested: onboarding_status.shells_ingested,
            corrections_ingested: onboarding_status.corrections_ingested,
            imported_sources,
            checkpoint_count: history_totals.checkpoint_count,
            journal_event_count: history_totals.journal_event_count,
        })
    }

    pub fn get_memory_os_overview_report(
        &self,
        scope: crate::core::memory_os::MemoryOsInspectionScope,
        project_path: Option<&str>,
    ) -> Result<crate::core::memory_os::MemoryOsOverviewReport> {
        let onboarding_status =
            crate::analytics::session_backfill::get_memory_os_session_backfill_status()?;
        let history_totals = self.get_memory_os_history_totals_all()?;
        let checkpoints = self.load_memory_os_checkpoint_captures(scope, project_path)?;
        let replay_shells = self.load_memory_os_replay_shells(scope, project_path)?;
        let correction_patterns = self.get_memory_os_correction_patterns(scope, project_path)?;
        let imported_sources = build_memory_os_imported_sources(
            &onboarding_status.imported_source_counts,
            &replay_shells,
        );
        let imported_sessions = imported_sources.iter().map(|source| source.sessions).sum();
        let imported_shell_executions = imported_sources
            .iter()
            .map(|source| source.shell_executions)
            .sum();
        let scoped_journal_count = match scope_project_path_or_current(scope, project_path) {
            Some(ref project) => {
                self.get_memory_os_project_snapshot(Some(project))?
                    .journal_event_count as usize
            }
            None => history_totals.journal_event_count,
        };
        let top_projects = build_memory_os_top_projects(&replay_shells, &checkpoints);
        let user_memory = build_user_memory_findings(&checkpoints, 12);
        let active_user_prose = active_user_prose_findings(&user_memory);
        let active_work = merge_memory_os_findings(
            active_user_prose,
            build_memory_os_active_work(&checkpoints, &replay_shells, &top_projects),
            8,
        );
        let top_action_memory_candidates = match scope {
            crate::core::memory_os::MemoryOsInspectionScope::User => {
                self.get_memory_os_action_candidates_all()?
            }
            crate::core::memory_os::MemoryOsInspectionScope::Project => {
                self.get_memory_os_action_candidates(project_path)?
            }
        };

        Ok(crate::core::memory_os::MemoryOsOverviewReport {
            generated_at: Utc::now().to_rfc3339(),
            scope,
            imported_sessions,
            imported_shell_executions,
            imported_sources: imported_sources.clone(),
            top_projects,
            top_correction_patterns: correction_patterns.into_iter().take(8).collect(),
            active_work,
            top_action_memory_candidates: top_action_memory_candidates
                .into_iter()
                .take(8)
                .collect(),
            onboarding: crate::core::memory_os::MemoryOsOnboardingState {
                schema_version: onboarding_status.schema_version,
                status: onboarding_status.status,
                started_at: onboarding_status.started_at,
                completed_at: onboarding_status.completed_at,
                sessions_processed: imported_sessions,
                shells_ingested: imported_shell_executions,
                corrections_ingested: correction_pattern_total_count(
                    &self.get_memory_os_correction_patterns(scope, project_path)?,
                ),
                imported_sources,
                checkpoint_count: checkpoints.len(),
                journal_event_count: scoped_journal_count,
            },
            serving_policy: memory_os_serving_policy_lines(),
        })
    }

    pub fn get_memory_os_profile_report(
        &self,
        scope: crate::core::memory_os::MemoryOsInspectionScope,
        project_path: Option<&str>,
    ) -> Result<crate::core::memory_os::MemoryOsProfileReport> {
        let onboarding_status =
            crate::analytics::session_backfill::get_memory_os_session_backfill_status()?;
        let checkpoints = self.load_memory_os_checkpoint_captures(scope, project_path)?;
        let replay_shells = self.load_memory_os_replay_shells(scope, project_path)?;
        let correction_patterns = self.get_memory_os_correction_patterns(scope, project_path)?;
        let correction_observations =
            self.load_memory_os_correction_observations(scope, project_path)?;
        let imported_sources = build_memory_os_imported_sources(
            &onboarding_status.imported_source_counts,
            &replay_shells,
        );
        let imported_sessions = imported_sources.iter().map(|source| source.sessions).sum();
        let by_source =
            build_memory_os_source_behavior(&imported_sources, &correction_observations);
        let top_projects = build_memory_os_top_projects(&replay_shells, &checkpoints);
        let user_memory = build_user_memory_findings(&checkpoints, 12);
        let active_user_prose = active_user_prose_findings(&user_memory);
        let active_work = merge_memory_os_findings(
            active_user_prose,
            build_memory_os_active_work(&checkpoints, &replay_shells, &top_projects),
            8,
        );
        let preferences = build_memory_os_preferences(&user_memory);

        Ok(crate::core::memory_os::MemoryOsProfileReport {
            generated_at: Utc::now().to_rfc3339(),
            scope,
            imported_sessions,
            by_source: by_source.clone(),
            preferences,
            operating_style: build_memory_os_operating_style(&by_source),
            autonomy_tendencies: build_memory_os_autonomy_findings(&by_source, &active_work),
            epistemic_preferences: build_memory_os_epistemic_findings(&correction_patterns),
            recurring_themes: build_memory_os_recurring_themes(
                &top_projects,
                &user_memory,
                &active_work,
            ),
            friction_triggers: build_memory_os_friction_triggers(&correction_patterns),
        })
    }

    pub fn get_memory_os_friction_report(
        &self,
        scope: crate::core::memory_os::MemoryOsInspectionScope,
        project_path: Option<&str>,
    ) -> Result<crate::core::memory_os::MemoryOsFrictionReport> {
        let onboarding_status =
            crate::analytics::session_backfill::get_memory_os_session_backfill_status()?;
        let checkpoints = self.load_memory_os_checkpoint_captures(scope, project_path)?;
        let replay_shells = self.load_memory_os_replay_shells(scope, project_path)?;
        let correction_patterns = self.get_memory_os_correction_patterns(scope, project_path)?;
        let correction_observations =
            self.load_memory_os_correction_observations(scope, project_path)?;
        let imported_sources = build_memory_os_imported_sources(
            &onboarding_status.imported_source_counts,
            &replay_shells,
        );
        let by_source =
            build_memory_os_source_behavior(&imported_sources, &correction_observations);
        let redirects = self.build_memory_os_redirect_summary(scope, project_path, &checkpoints)?;
        let likely_misunderstandings = build_memory_os_misunderstandings(&correction_patterns);
        let prose_signal_counts = count_user_prose_signals(&checkpoints);
        let durable_fixes = detect_user_prose_durable_fixes(project_path);
        let autonomy_status = super::signals::autonomy_polling_friction_status(
            prose_signal_counts.latest_autonomy_at,
            durable_fixes.autonomy_polling.as_ref(),
            Utc::now(),
        );
        let behavior_changes = build_memory_os_behavior_changes(
            &by_source,
            &redirects,
            prose_signal_counts.autonomy,
            Some(autonomy_status.as_str()),
        );
        let new_unproven_friction =
            super::signals::build_memory_os_new_unproven_friction(&checkpoints);
        let top_fixes = build_memory_os_friction_fixes(
            &correction_patterns,
            &likely_misunderstandings,
            &behavior_changes,
            &redirects,
            &checkpoints,
            &durable_fixes,
        );

        Ok(crate::core::memory_os::MemoryOsFrictionReport {
            generated_at: Utc::now().to_rfc3339(),
            scope,
            top_fixes,
            new_unproven_friction,
            by_source,
            redirects,
            repeated_corrections: correction_patterns.into_iter().take(8).collect(),
            likely_misunderstandings,
            behavior_changes,
        })
    }
}

fn merge_memory_os_findings(
    primary: Vec<crate::core::memory_os::MemoryOsNarrativeFinding>,
    fallback: Vec<crate::core::memory_os::MemoryOsNarrativeFinding>,
    limit: usize,
) -> Vec<crate::core::memory_os::MemoryOsNarrativeFinding> {
    let mut merged = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for finding in primary.into_iter().chain(fallback) {
        let key = finding.summary.to_ascii_lowercase();
        if seen.insert(key) {
            merged.push(finding);
            if merged.len() >= limit {
                break;
            }
        }
    }
    merged
}

fn build_user_memory_findings(
    checkpoints: &[super::MemoryOsCheckpointEnvelope],
    limit: usize,
) -> Vec<crate::core::memory_os::MemoryOsNarrativeFinding> {
    merge_memory_os_findings(
        build_memory_os_semantic_fact_findings(checkpoints, limit),
        build_memory_os_user_prose_findings(checkpoints, limit),
        limit,
    )
}

fn active_user_prose_findings(
    user_prose: &[crate::core::memory_os::MemoryOsNarrativeFinding],
) -> Vec<crate::core::memory_os::MemoryOsNarrativeFinding> {
    let current = user_prose
        .iter()
        .filter(|finding| finding.title == "Current work")
        .cloned()
        .collect::<Vec<_>>();
    if !current.is_empty() {
        return current;
    }

    user_prose
        .iter()
        .filter(|finding| finding.title == "Memory OS direction")
        .cloned()
        .take(1)
        .collect()
}
