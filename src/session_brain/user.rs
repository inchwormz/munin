use super::types::SessionBrainUserContext;
use crate::core::memory_os::{
    MemoryOsFrictionReport, MemoryOsInspectionScope, MemoryOsNarrativeFinding,
    MemoryOsOnboardingState, MemoryOsOverviewReport, MemoryOsProfileReport,
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
    let overview = tracker.get_memory_os_overview_report(scope, None).ok();
    let profile = tracker.get_memory_os_profile_report(scope, None).ok();
    let context = SessionBrainUserContext {
        brief: build_brief_line(overview.as_ref(), profile.as_ref()),
        overview: build_overview_line(overview.as_ref(), profile.as_ref()),
        profile: build_profile_line(profile.as_ref()),
        friction: String::new(),
    };
    if user_context_has_content(&context) {
        context
    } else {
        build_fast_user_context(tracker)
    }
}

fn build_full_user_context(tracker: &Tracker) -> SessionBrainUserContext {
    let scope = MemoryOsInspectionScope::User;
    let overview = tracker.get_memory_os_overview_report(scope, None).ok();
    let profile = tracker.get_memory_os_profile_report(scope, None).ok();
    let friction = tracker.get_memory_os_friction_report(scope, None).ok();

    SessionBrainUserContext {
        brief: build_brief_line(overview.as_ref(), profile.as_ref()),
        overview: build_overview_line(overview.as_ref(), profile.as_ref()),
        profile: build_profile_line(profile.as_ref()),
        friction: build_friction_line(friction.as_ref()),
    }
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

fn build_brief_line(
    overview: Option<&MemoryOsOverviewReport>,
    profile: Option<&MemoryOsProfileReport>,
) -> String {
    let mut parts = Vec::new();
    if let Some(overview) = overview {
        parts.extend(overview.active_work.iter().take(2).map(finding_to_line));
    }
    if let Some(profile) = profile {
        parts.extend(profile.operating_style.iter().take(1).map(finding_to_line));
    }
    truncate(&parts.join(" | "), 320)
}

fn build_overview_line(
    overview: Option<&MemoryOsOverviewReport>,
    profile: Option<&MemoryOsProfileReport>,
) -> String {
    let mut parts = Vec::new();
    if let Some(overview) = overview {
        parts.extend(overview.active_work.iter().take(2).map(finding_to_line));
        parts.extend(overview.top_projects.iter().take(4).map(|project| {
            format!(
                "{}: {} sessions / {} shell executions",
                project.repo_label, project.sessions, project.shell_executions
            )
        }));
    }
    if let Some(profile) = profile {
        parts.extend(profile.recurring_themes.iter().take(3).map(finding_to_line));
    }
    dedupe_lines(&mut parts);
    truncate(&parts.join(" | "), 520)
}

fn build_profile_line(profile: Option<&MemoryOsProfileReport>) -> String {
    let Some(profile) = profile else {
        return String::new();
    };
    let parts = profile
        .operating_style
        .iter()
        .chain(profile.preferences.iter())
        .take(4)
        .map(finding_to_line)
        .collect::<Vec<_>>();
    truncate(&parts.join(" | "), 360)
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
