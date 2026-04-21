use super::types::{SessionBrainGuidance, SessionBrainUserContext};

pub fn build_guidance(user: &SessionBrainUserContext) -> SessionBrainGuidance {
    let mut retrieval_hints = vec![
        "Start from current-session messages and active agenda before repo-wide search."
            .to_string(),
        "Prefer the current project capsule and strategy context over broader user memory."
            .to_string(),
        "Use Memory OS read surfaces before raw recall for user/profile/current-work questions."
            .to_string(),
        "Treat successful shell history as noise unless it changed state or verified a fact."
            .to_string(),
    ];

    if friction_needs_context_reversal_guard(&user.friction) {
        retrieval_hints.insert(
            0,
            "If the newest user message reverses direction or may belong to another terminal, ask one concise clarification before editing."
                .to_string(),
        );
    }

    if !user.friction.is_empty() {
        retrieval_hints
            .push("Pay attention to the friction summary before widening scope.".to_string());
    }

    let mut avoid = vec![
        "Hidden or internal reasoning.".to_string(),
        "Raw transcript dumps.".to_string(),
        "Hook or progress chatter.".to_string(),
        "Letting user-global memory override the active project.".to_string(),
        "Treating old goals as current after an interruption.".to_string(),
    ];
    if friction_needs_context_reversal_guard(&user.friction) {
        avoid.insert(
            0,
            "Editing after an abrupt context reversal before confirming the user meant this terminal."
                .to_string(),
        );
    }

    SessionBrainGuidance {
        retrieval_hints,
        avoid,
    }
}

fn friction_needs_context_reversal_guard(friction: &str) -> bool {
    let lowered = friction.to_ascii_lowercase();
    lowered.contains("wrong-terminal")
        || lowered.contains("wrong terminal")
        || lowered.contains("context slip")
        || lowered.contains("clarify before reversing")
        || lowered.contains("ask one concise clarifying question")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guidance_turns_context_reversal_friction_into_clarification_guard() {
        let user = SessionBrainUserContext {
            brief: String::new(),
            overview: String::new(),
            profile: String::new(),
            friction: "Clarify before reversing direction on likely wrong-terminal context slips: ask one concise clarifying question before editing.".to_string(),
        };

        let guidance = build_guidance(&user);

        assert!(guidance
            .retrieval_hints
            .iter()
            .any(|hint| hint.contains("ask one concise clarification before editing")));
        assert!(guidance
            .avoid
            .iter()
            .any(|avoid| avoid.contains("before confirming the user meant this terminal")));
    }
}
