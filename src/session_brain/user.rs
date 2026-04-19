use super::types::SessionBrainUserContext;
use crate::core::memory_os::{MemoryOsFrictionReport, MemoryOsInspectionScope};
use crate::core::tracking::Tracker;
use crate::core::utils::truncate;

pub fn build_user_context(tracker: &Tracker) -> SessionBrainUserContext {
    let scope = MemoryOsInspectionScope::User;
    let friction = tracker.get_memory_os_friction_report(scope, None).ok();

    SessionBrainUserContext {
        brief: String::new(),
        overview: String::new(),
        profile: String::new(),
        friction: build_friction_line(friction.as_ref()),
    }
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
