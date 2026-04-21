#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FallbackCommandKind {
    Munin,
    ExternalRawArchive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FallbackCommand {
    pub command: &'static str,
    pub kind: FallbackCommandKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LiveFallback {
    pub fallback_route: &'static str,
    pub fallback_command: &'static str,
    pub fallback_reason: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IntentRule {
    pub id: &'static str,
    pub route: &'static str,
    pub skill_name: &'static str,
    pub description: &'static str,
    pub description_expanded: &'static str,
    pub trigger_phrases: &'static [&'static str],
    pub negative_triggers: &'static [&'static str],
    pub primary_command: &'static str,
    pub fallback_command: Option<FallbackCommand>,
    pub reason: &'static str,
    pub output_contract: &'static [&'static str],
    pub trust_rules: &'static [&'static str],
    pub fallback_rules: &'static [&'static str],
    pub done_criteria: &'static [&'static str],
    pub what_not_to_do: &'static [&'static str],
    pub live_fallback: Option<LiveFallback>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UmbrellaSkill {
    pub name: &'static str,
    pub description: &'static str,
    pub description_expanded: &'static str,
}

pub const UMBRELLA_SKILL: UmbrellaSkill = UmbrellaSkill {
    name: "munin",
    description: "Use Munin when the user asks about active work, current session state, project memory, repeated mistakes, next steps, recall, proof, or health and the exact narrow Munin surface is unclear.",
    description_expanded: "The umbrella Munin skill is the resolver wrapper. Use it when the request is memory-shaped but spans multiple surfaces or the right narrow skill is not obvious.",
};

pub const INTENT_RULES: &[IntentRule] = &[
    IntentRule {
        id: "project_resume",
        route: "resume",
        skill_name: "munin-resume",
        description: "Open the compiled Munin startup brief for this project. Use when starting, resuming, or handing off work.",
        description_expanded: "Use this for project continuity when the user wants the best compiled startup truth rather than the current transcript window.",
        trigger_phrases: &["resume", "start work", "handoff", "startup brief", "pick up", "continue project"],
        negative_triggers: &["current session", "live window", "what was i doing"],
        primary_command: "munin resume --format prompt",
        fallback_command: Some(FallbackCommand {
            command: "munin doctor --scope user --format text",
            kind: FallbackCommandKind::Munin,
        }),
        reason: "The request asks for project continuity from compiled Memory OS truth.",
        output_contract: &["startup brief", "active work", "next steps", "watchouts"],
        trust_rules: &[
            "Treat the brief as compiled startup truth, not raw transcript search.",
            "Prefer prose sections over diagnostic tables when answering the user.",
            "Use the current project, active work, next step, and watchout sections before raw recall.",
        ],
        fallback_rules: &[
            "If the brief is empty or mismatched to the project, run `munin doctor --scope user --format text` before answering.",
            "If the corpus is empty, say Munin has no usable imported corpus yet and suggest running install/import setup.",
        ],
        done_criteria: &[
            "Names the active project or says no active project was found.",
            "Gives one useful next step.",
            "Includes one watchout when available.",
        ],
        what_not_to_do: &[
            "Do not dump the full startup prompt.",
            "Do not open raw transcripts unless the compiled brief is insufficient.",
        ],
        live_fallback: None,
    },
    IntentRule {
        id: "session_brain",
        route: "brain",
        skill_name: "munin-brain",
        description: "Inspect Munin Session Brain for the current live session. Use for current-window state, agenda, blockers, or active context.",
        description_expanded: "Use this when the user asks what is happening in the current session, what they were doing, or whether the live window is available.",
        trigger_phrases: &["what was i doing", "where was i", "what am i doing", "current session", "live window", "session brain", "current ask", "blockers"],
        negative_triggers: &["resume project", "startup brief"],
        primary_command: "munin brain --format prompt",
        fallback_command: Some(FallbackCommand {
            command: "munin resume --format prompt",
            kind: FallbackCommandKind::Munin,
        }),
        reason: "The request asks about the current session window.",
        output_contract: &["sourceStatus", "session id", "current ask", "agenda", "blockers"],
        trust_rules: &[
            "Check `sourceStatus` first.",
            "Treat the answer as current only when `sourceStatus` is `live`.",
            "For `fallback-latest` or `stale`, name the session id and transcript modified time.",
        ],
        fallback_rules: &[
            "If Session Brain is fallback or stale, prefer `munin resume --format prompt` for project continuity.",
            "Never present fallback transcript state as the user's live current ask.",
        ],
        done_criteria: &[
            "States whether the source is live, fallback, stale, or absent.",
            "Answers the current-session question from agenda/state when live.",
            "Offers resume as the safer alternative when not live.",
        ],
        what_not_to_do: &[
            "Do not say 'you are currently' when sourceStatus is not live.",
            "Do not hide the freshness label.",
        ],
        live_fallback: Some(LiveFallback {
            fallback_route: "resume",
            fallback_command: "munin resume --format prompt",
            fallback_reason: "Session Brain is not live, so compiled project resume is the safer continuity surface.",
        }),
    },
    IntentRule {
        id: "repeated_friction",
        route: "friction",
        skill_name: "munin-friction",
        description: "Show repeated Munin friction and correction patterns. Use when the user asks what keeps going wrong or what agents keep repeating.",
        description_expanded: "Use this for recurring agent mistakes, repeated corrections, and fixed friction that should fade into the background.",
        trigger_phrases: &["friction", "what keeps going wrong", "keeps going wrong", "wrong", "repeated", "repeated correction", "agent keeps", "keep repeating", "what agents keep repeating"],
        negative_triggers: &["next step", "what should i do"],
        primary_command: "munin friction --last 30d --format text",
        fallback_command: Some(FallbackCommand {
            command: "munin doctor --scope user --format text",
            kind: FallbackCommandKind::Munin,
        }),
        reason: "The request asks about repeated mistakes or correction patterns.",
        output_contract: &["top fixes", "status", "permanent fix", "evidence"],
        trust_rules: &[
            "`active` and `codified` are current friction.",
            "`monitoring` in New But Unproven Friction Points is a single or newly detected high-signal correction worth avoiding, not yet proven enough for a codified fix.",
            "`monitoring`, `fixed`, and `retired` are background unless the user asks for history.",
            "Every surfaced fix should include its permanent-fix pointer when present.",
        ],
        fallback_rules: &[
            "If output is empty, say there is no active friction in the requested window.",
            "If all matching items are fixed or retired, say no current issues and summarize the fixed count.",
            "If the report looks stale, run doctor before turning it into user guidance.",
        ],
        done_criteria: &[
            "Lists the top 1-3 active/codified issues and any monitoring one-off points when present.",
            "Includes each issue's permanent-fix pointer.",
            "Mentions retired/fixed items only when asked or when explaining empty current friction.",
        ],
        what_not_to_do: &[
            "Do not invent friction when the filtered report is empty.",
            "Do not treat retired items as urgent new work.",
        ],
        live_fallback: None,
    },
    IntentRule {
        id: "next_step",
        route: "nudge",
        skill_name: "munin-nudge",
        description: "Show Munin strategy-backed next-step nudges. Use when the user asks what to do next or wants proactive guidance.",
        description_expanded: "Use this when the user wants one useful next move based on strategy, metrics, and continuity evidence.",
        trigger_phrases: &["next step", "what should i do", "nudge", "strategy", "focus", "next move", "what now"],
        negative_triggers: &["what went wrong", "friction"],
        primary_command: "munin nudge --format text",
        fallback_command: Some(FallbackCommand {
            command: "munin metrics get --scope default --format text",
            kind: FallbackCommandKind::Munin,
        }),
        reason: "The request asks for a strategy-backed next move.",
        output_contract: &["numbered moves", "why now", "confidence", "evidence"],
        trust_rules: &[
            "Default text output should be the top 1-3 prose moves.",
            "If metrics are empty, expect one setup nudge rather than many generic instrumentation items.",
            "Treat evidence and confidence as more important than item count.",
        ],
        fallback_rules: &[
            "If output is only generic instrumentation noise, say metrics are missing and show the exact `munin metrics set <key> <value> --scope <scope>` shape.",
            "If no nudge is ready, say no strategic nudge is ready from current evidence.",
        ],
        done_criteria: &[
            "Gives 1-3 numbered moves.",
            "Explains why now.",
            "Includes evidence or says what metric is missing.",
        ],
        what_not_to_do: &[
            "Do not pass a long generic instrumentation dump to the user.",
            "Do not give a next move without evidence.",
        ],
        live_fallback: None,
    },
    IntentRule {
        id: "proof_gate",
        route: "prove",
        skill_name: "munin-prove",
        description: "Show Munin proof and promotion status for the Memory OS read path. Use before trusting promoted memory or before release checks.",
        description_expanded: "Use this when the user asks whether the memory read path is proven, promoted, blocked, or safe to trust.",
        trigger_phrases: &["prove", "proof", "promotion", "promoted memory", "trusted", "trust this memory", "proof gate", "release proof"],
        negative_triggers: &["healthy", "doctor"],
        primary_command: "munin prove --format text",
        fallback_command: Some(FallbackCommand {
            command: "munin doctor --scope user --format text",
            kind: FallbackCommandKind::Munin,
        }),
        reason: "The request asks for proof or promotion status.",
        output_contract: &["gate status", "missing proof rows", "promotion state"],
        trust_rules: &[
            "A blocked proof gate is expected when replay rows are missing.",
            "Blocked is not broken.",
            "Promotion status is trust evidence, not a general health diagnosis.",
        ],
        fallback_rules: &[
            "If blocked, name the missing proof rows.",
            "If proof output is absent, run doctor to check substrate health.",
        ],
        done_criteria: &[
            "States green, blocked, or unknown.",
            "Names missing proof rows when blocked.",
            "Avoids claiming Memory OS is broken solely from blocked proof.",
        ],
        what_not_to_do: &[
            "Do not equate blocked proof with a broken product.",
            "Do not claim promotion trust without proof rows.",
        ],
        live_fallback: None,
    },
    IntentRule {
        id: "recall_topic",
        route: "recall",
        skill_name: "munin-recall",
        description: "Use Munin compiled memory for topic recall. Use when the user asks to remember prior work, decisions, preferences, or project context.",
        description_expanded: "Use this for topic-specific recall from compiled Memory OS evidence, with raw archive fallback only when compiled recall has zero matches.",
        trigger_phrases: &["remember", "recall", "prior work", "what did we decide", "decision", "past", "previous", "about"],
        negative_triggers: &["current session", "next step", "friction"],
        primary_command: "munin recall --format text \"<query>\"",
        fallback_command: Some(FallbackCommand {
            command: "qmd \"<query>\"",
            kind: FallbackCommandKind::ExternalRawArchive,
        }),
        reason: "The request asks for topic-specific compiled memory.",
        output_contract: &["topic", "matches", "source refs", "evidence"],
        trust_rules: &[
            "Trust topic-ranked compiled matches before raw archive search.",
            "Each hit should include source evidence.",
            "Do not silently fall back to overview.",
        ],
        fallback_rules: &[
            "If zero topic matches, say `no compiled matches for <query>` explicitly.",
            "Suggest `qmd \"<query>\"` only as an optional raw archive dig.",
        ],
        done_criteria: &[
            "Returns 3-8 topic-matched facts when available.",
            "Includes evidence pointers.",
            "Avoids raw transcript dumps.",
        ],
        what_not_to_do: &[
            "Do not dump the Memory OS overview as a recall answer.",
            "Do not hide a zero-match result.",
        ],
        live_fallback: None,
    },
    IntentRule {
        id: "memory_hygiene",
        route: "hygiene",
        skill_name: "munin-hygiene",
        description: "Audit and prune duplicated CLAUDE.md, AGENTS.md, CONTEXT.md, and related memory guidance. Use when memory files are bloated or repeating known rules.",
        description_expanded: "Use this when the user wants agent memory stores cleaned up without losing important scoped guidance. The command reports duplicates by default and only writes with explicit --write backups.",
        trigger_phrases: &["memory hygiene", "prune claude", "prune agents", "clean claude.md", "clean agents.md", "dedupe memory", "duplicated memory", "repeating what's already known"],
        negative_triggers: &["is munin healthy", "doctor"],
        primary_command: "munin hygiene --format text",
        fallback_command: Some(FallbackCommand {
            command: "munin hygiene --format text --include-codex",
            kind: FallbackCommandKind::Munin,
        }),
        reason: "The request asks to manage duplicated agent memory guidance files.",
        output_contract: &["files scanned", "duplicate groups", "planned removals", "warnings", "backups"],
        trust_rules: &[
            "Default output is a dry-run report; no files are changed unless --write is present.",
            "Cross-agent duplicates are report-only because CLAUDE.md and AGENTS.md may both need the same instruction for different tools.",
            "Write mode prunes exact normalized duplicates only and creates .munin-bak backups.",
        ],
        fallback_rules: &[
            "If important duplication is cross-agent, summarize it and ask for a policy decision before removal.",
            "If no memory files are found, point the user to the root that was scanned.",
            "Use --include-codex when the suspected duplicates live under .codex memory files.",
        ],
        done_criteria: &[
            "Reports which memory files were scanned.",
            "Separates auto-prunable exact duplicates from cross-agent report-only duplicates.",
            "If --write was used, names backup files and confirms what was removed.",
        ],
        what_not_to_do: &[
            "Do not semantically rewrite guidance without review.",
            "Do not remove cross-agent duplicate rules automatically.",
            "Do not delete scoped AGENTS.md or CLAUDE.md files wholesale.",
        ],
        live_fallback: None,
    },
    IntentRule {
        id: "health_check",
        route: "doctor",
        skill_name: "munin-doctor",
        description: "Diagnose Munin Memory OS health. Use when the user asks whether Munin is healthy, stale, empty, or release-ready.",
        description_expanded: "Use fast doctor for substrate health and release doctor for public-contract checks.",
        trigger_phrases: &["doctor", "healthy", "health", "stale", "empty corpus", "db readable", "database readable", "release ready", "can i trust"],
        negative_triggers: &["proof gate", "promotion"],
        primary_command: "munin doctor --scope user --format text",
        fallback_command: Some(FallbackCommand {
            command: "munin doctor --scope user --release --repo-root . --format text",
            kind: FallbackCommandKind::Munin,
        }),
        reason: "The request asks for Memory OS substrate health or release readiness.",
        output_contract: &["overall status", "corpus", "source health", "recommended permanent fix"],
        trust_rules: &[
            "Fast doctor checks substrate health: corpus present, DB readable, imports recent.",
            "Fast doctor does not prove every surface answer is high quality.",
            "Release doctor checks public-contract gates when repo/site inputs are provided.",
        ],
        fallback_rules: &[
            "If doctor is healthy but answers are bad, run the surface-specific skill.",
            "For release readiness, run release doctor with explicit repo and site roots.",
        ],
        done_criteria: &[
            "States substrate status.",
            "Names a specific issue if unhealthy.",
            "Separates fast health from release-quality proof.",
        ],
        what_not_to_do: &[
            "Do not use fast doctor as proof that recall or nudge answers are good.",
            "Do not skip surface-specific checks when the complaint is about a bad answer.",
        ],
        live_fallback: None,
    },
];

pub fn intent_by_route(route: &str) -> Option<&'static IntentRule> {
    INTENT_RULES.iter().find(|rule| rule.route == route)
}

pub fn intent_by_skill_name(skill_name: &str) -> Option<&'static IntentRule> {
    INTENT_RULES
        .iter()
        .find(|rule| rule.skill_name == skill_name)
}
