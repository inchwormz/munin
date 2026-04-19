use super::types::{SessionBrainGuidance, SessionBrainUserContext};

pub fn build_guidance(user: &SessionBrainUserContext) -> SessionBrainGuidance {
    let mut retrieval_hints = vec![
        "Start from current-session messages and active agenda before repo-wide search."
            .to_string(),
        "Prefer the current project capsule and strategy context over broader user memory."
            .to_string(),
        "Use Memory OS read surfaces before raw recall for stable user profile and friction questions."
            .to_string(),
        "Treat successful shell history as noise unless it changed state or verified a fact."
            .to_string(),
    ];

    if !user.friction.is_empty() {
        retrieval_hints
            .push("Pay attention to the friction summary before widening scope.".to_string());
    }

    SessionBrainGuidance {
        retrieval_hints,
        avoid: vec![
            "Hidden or internal reasoning.".to_string(),
            "Raw transcript dumps.".to_string(),
            "Hook or progress chatter.".to_string(),
            "Letting user-global memory override the active project.".to_string(),
            "Treating old goals as current after an interruption.".to_string(),
        ],
    }
}
