use super::types::SessionBrainUserContext;
use crate::core::memory_os::{
    MemoryOsFrictionReport, MemoryOsInspectionScope, MemoryOsNarrativeFinding,
    MemoryOsOnboardingState, MemoryOsProfileReport,
};
use crate::core::tracking::Tracker;
use crate::core::utils::truncate;

pub fn build_user_context(tracker: &Tracker) -> SessionBrainUserContext {
    if std::env::var_os("MUNIN_SESSION_BRAIN_FULL_USER_CONTEXT").is_some() {
        return build_full_user_context(tracker);
    }

    build_default_user_context(tracker)
}

fn build_fast_user_context(tracker: &Tracker) -> SessionBrainUserContext {
    let onboarding = tracker.get_memory_os_onboarding_state_fast().ok();

    SessionBrainUserContext {
        brief: String::new(),
        overview: build_fast_overview_line(onboarding.as_ref()),
        profile: String::new(),
        friction: String::new(),
    }
}

fn build_default_user_context(tracker: &Tracker) -> SessionBrainUserContext {
    let scope = MemoryOsInspectionScope::User;
    let profile = tracker.get_memory_os_profile_report(scope, None).ok();
    let mut context = SessionBrainUserContext {
        brief: build_brief_line(profile.as_ref()),
        overview: build_overview_line(profile.as_ref()),
        profile: build_profile_line(profile.as_ref()),
        friction: String::new(),
    };
    dedupe_user_context_fields(&mut context);
    if user_context_has_content(&context) {
        context
    } else {
        build_fast_user_context(tracker)
    }
}

fn build_full_user_context(tracker: &Tracker) -> SessionBrainUserContext {
    let scope = MemoryOsInspectionScope::User;
    let profile = tracker.get_memory_os_profile_report(scope, None).ok();
    let friction = tracker.get_memory_os_friction_report(scope, None).ok();

    let mut context = SessionBrainUserContext {
        brief: build_brief_line(profile.as_ref()),
        overview: build_overview_line(profile.as_ref()),
        profile: build_profile_line(profile.as_ref()),
        friction: build_friction_line(friction.as_ref()),
    };
    dedupe_user_context_fields(&mut context);
    context
}

fn user_context_has_content(context: &SessionBrainUserContext) -> bool {
    [
        &context.brief,
        &context.overview,
        &context.profile,
        &context.friction,
    ]
    .iter()
    .any(|value| {
        let trimmed = value.trim();
        !trimmed.is_empty() && !trimmed.ends_with(": none")
    })
}

fn build_fast_overview_line(onboarding: Option<&MemoryOsOnboardingState>) -> String {
    let Some(onboarding) = onboarding else {
        return String::new();
    };
    truncate(
        &format!(
            "Compiled Memory OS has indexed {} sessions / {} shell executions. If this is empty, run `munin memory-os ingest --force` before expecting user/project history.",
            onboarding.sessions_processed, onboarding.shells_ingested
        ),
        360,
    )
}

fn build_brief_line(profile: Option<&MemoryOsProfileReport>) -> String {
    let parts = stable_user_model_findings(profile)
        .into_iter()
        .take(2)
        .map(finding_to_line)
        .collect::<Vec<_>>();
    truncate(&parts.join(" | "), 320)
}

fn dedupe_user_context_fields(context: &mut SessionBrainUserContext) {
    if same_user_context_line(&context.overview, &context.brief) {
        context.overview.clear();
    }
    if same_user_context_line(&context.profile, &context.brief)
        || same_user_context_line(&context.profile, &context.overview)
    {
        context.profile.clear();
    }
}

fn same_user_context_line(left: &str, right: &str) -> bool {
    let left = left.trim();
    let right = right.trim();
    !left.is_empty() && !right.is_empty() && left.eq_ignore_ascii_case(right)
}

fn build_overview_line(profile: Option<&MemoryOsProfileReport>) -> String {
    let mut parts = stable_user_model_findings(profile)
        .into_iter()
        .take(5)
        .map(finding_to_line)
        .collect::<Vec<_>>();
    dedupe_lines(&mut parts);
    truncate(&parts.join(" | "), 520)
}

fn build_profile_line(profile: Option<&MemoryOsProfileReport>) -> String {
    let Some(profile) = profile else {
        return String::new();
    };
    let parts = stable_user_model_findings(Some(profile))
        .into_iter()
        .take(4)
        .map(finding_to_line)
        .collect::<Vec<_>>();
    truncate(&parts.join(" | "), 360)
}

fn stable_user_model_findings(
    profile: Option<&MemoryOsProfileReport>,
) -> Vec<&MemoryOsNarrativeFinding> {
    let Some(profile) = profile else {
        return Vec::new();
    };
    profile
        .operating_style
        .iter()
        .chain(profile.preferences.iter())
        .chain(profile.epistemic_preferences.iter())
        .filter(|finding| stable_user_model_finding(finding))
        .collect()
}

fn stable_user_model_finding(finding: &MemoryOsNarrativeFinding) -> bool {
    matches!(
        finding.title.as_str(),
        "Working preference" | "Positive feedback"
    ) && !user_model_finding_is_task_specific(finding)
}

fn user_model_finding_is_task_specific(finding: &MemoryOsNarrativeFinding) -> bool {
    let text = format!("{} {}", finding.title, finding.summary).to_ascii_lowercase();
    [
        "task:",
        "format:",
        "unified diff",
        "do not edit files",
        "visual verdict",
        "local url",
        "current run",
        "current task",
        "this task",
        "review the current",
        "business strategy",
        "lead database",
        "builders",
        "plumbers",
        "electricians",
        "small businesses",
    ]
    .iter()
    .any(|needle| text.contains(needle))
}

fn build_friction_line(friction: Option<&MemoryOsFrictionReport>) -> String {
    let Some(friction) = friction else {
        return String::new();
    };
    let mut parts = friction
        .behavior_changes
        .iter()
        .take(2)
        .map(|change| format!("{}: {}", change.target_agent, change.change))
        .collect::<Vec<_>>();
    if parts.is_empty() {
        parts.extend(
            friction
                .likely_misunderstandings
                .iter()
                .take(2)
                .map(|item| format!("{}: {}", item.label, item.summary)),
        );
    }
    truncate(&parts.join(" | "), 360)
}

fn finding_to_line(finding: &MemoryOsNarrativeFinding) -> String {
    format!("{}: {}", finding.title, finding.summary)
}

fn dedupe_lines(items: &mut Vec<String>) {
    let mut seen = std::collections::HashSet::new();
    items.retain(|item| seen.insert(item.to_ascii_lowercase()));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::memory_os::{MemoryOsInspectionScope, MemoryOsProfileReport};

    fn finding(title: &str, summary: &str) -> MemoryOsNarrativeFinding {
        MemoryOsNarrativeFinding {
            title: title.to_string(),
            summary: summary.to_string(),
            evidence: Vec::new(),
        }
    }

    fn profile(preferences: Vec<MemoryOsNarrativeFinding>) -> MemoryOsProfileReport {
        MemoryOsProfileReport {
            generated_at: "2026-04-21T00:00:00Z".to_string(),
            scope: MemoryOsInspectionScope::User,
            imported_sessions: 1,
            by_source: Vec::new(),
            preferences,
            operating_style: Vec::new(),
            autonomy_tendencies: Vec::new(),
            epistemic_preferences: Vec::new(),
            recurring_themes: vec![finding(
                "Business strategy",
                "I want you to scrape all NZ builders/plumbers/electricians and create a lead database.",
            )],
            friction_triggers: Vec::new(),
        }
    }

    #[test]
    fn user_model_uses_stable_work_style_not_old_strategy_or_task_prose() {
        let profile = profile(vec![
            finding("Working preference", "Use the following visual verdict as data. Do not edit files"),
            finding(
                "Working preference",
                "The orchestrator needs firm direction to poll every 10 seconds and give approvals quickly.",
            ),
            finding("Product constraint", "I don't want the look of it to change."),
        ]);

        let overview = build_overview_line(Some(&profile));
        let profile_line = build_profile_line(Some(&profile));

        assert!(overview.contains("poll every 10 seconds"));
        assert!(profile_line.contains("poll every 10 seconds"));
        assert!(!overview.contains("lead database"));
        assert!(!overview.contains("visual verdict"));
        assert!(!overview.contains("look of it"));
    }

    #[test]
    fn user_context_dedupes_repeated_prompt_fields() {
        let mut context = SessionBrainUserContext {
            brief: "Working preference: poll every 10 seconds".to_string(),
            overview: "Working preference: poll every 10 seconds".to_string(),
            profile: "Working preference: poll every 10 seconds".to_string(),
            friction: String::new(),
        };

        dedupe_user_context_fields(&mut context);

        assert!(!context.brief.is_empty());
        assert!(context.overview.is_empty());
        assert!(context.profile.is_empty());
    }
}
