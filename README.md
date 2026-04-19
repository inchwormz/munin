# Munin

Local memory system for agent-driven development.

Current testing build: `v0.5.0-beta.3`.

## What It Is

`munin` reads local Claude/Codex sessions, compiles startup memory, surfaces repeated friction, and keeps noisy shell output out of agent context.

## How It Works

Munin has three layers:

1. **Session ingestion** reads local Claude, Codex, and archived session data: prompts, assistant turns, shell commands, outcomes, corrections, working directories, and timestamps.
2. **Memory compilation** converts those raw sessions into a local Memory OS: evidence-backed facts, current project goals, open loops, repeated mistakes, command outcomes, strategy pressure, and proof rows.
3. **Proactivity** can evaluate strategy and continuity on a schedule, write a morning brief, queue an intervention, and optionally launch an agent session.
4. **Agent access** exposes the compiled memory through CLI surfaces and installed Codex/Claude skills, so agents query structured memory instead of searching raw transcripts.

No hosted service is required for the local CLI. The compiled state stays on the machine running Munin.

## Runtime Surfaces

Core surfaces:

- `munin resume --format prompt` for the startup memory brief
- `munin brain --format prompt` for the live Session Brain
- `munin nudge` for strategy-backed next-step recommendations
- `munin proactivity run --no-spawn` for the morning strategy/continuity evaluation
- `munin prove --last-resume` for replay/promotion proof
- `munin friction` for repeated correction and friction patterns
- `munin recall "topic"` for the compiled Memory OS read path
- `munin resolve "topic"` for routing a request to the right memory surface
- `munin metrics get --scope <scope>` for local strategy KPI readings
- `munin hygiene` for duplicate CLAUDE.md / AGENTS.md / CONTEXT.md guidance reports
- `munin doctor --scope user` for a fast Memory OS health check
- `munin install --check-resolvable` for skill/resolver validation

## Testing Build Status

[![CI](https://github.com/inchwormz/munin/actions/workflows/ci.yml/badge.svg)](https://github.com/inchwormz/munin/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/munin-memory.svg)](https://crates.io/crates/munin-memory)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)
[![GitHub stars](https://img.shields.io/github/stars/inchwormz/munin?style=social)](https://github.com/inchwormz/munin)

This is the first clean open-source testing build for GitHub. It is not a final public 1.0 launch.

What is ready:

- The `munin` binary builds and installs from this repo.
- Codex and Claude skill installation is generated from the same resolver table.
- Doctor release checks verify package guard, Session Brain freshness, recall wiring, and public docs command parity.
- Strategy KPI slots hydrate from an ingested strategy plan even before current values are filled.
- Promotion proof requires independent `test-private` and `adversarial-private` replay rows.

What still needs real-world proof before final `v0.5.0`:

- Fresh install from a brand-new checkout on another machine or clean user profile.
- A real Codex and Claude usage sprint using `resume`, `recall`, `nudge`, `prove`, `doctor`, and `hygiene`.
- README clarity for someone who has never seen the local project.
- A pass over public docs for private paths, stale command names, and confusing strategy-metrics language.

## License

Munin is licensed under the Apache 2.0 license.

The hosted product, when built, lives in a separate private repository and is not part of this Apache-licensed local CLI repo.

## Local Install

Install from the repository checkout during the testing phase:

```powershell
cargo install --path . --force
```

After the `munin-memory` crate is published for this testing build, the install command is:

```powershell
cargo install munin-memory
```

For local development from this working tree:

```powershell
cargo build --release
cargo test
```

## Local Commands

```powershell
munin --version
munin resume --format prompt
munin brain --format prompt
munin nudge
munin prove --last-resume
munin friction --agent codex --last 30d
munin recall "refund SLA"
munin resolve "what keeps going wrong?"
munin metrics get --scope sitesorted-business
munin proactivity status --scope sitesorted-business
munin hygiene --root . --format text
munin doctor --scope user
munin install --dry-run
munin install --check-resolvable
```

`munin install` archives legacy skill folders into `.munin-legacy` by default. Use `--keep-legacy` to leave them in place, or `--force` to refresh existing Munin skill files.

## Memory OS Read Priority

For opening or resuming work in the current repo, use:

```powershell
munin resume --format prompt
munin memory-os promotion --format text
```

For questions about the user, operating style, active work, or next steps, use compiled Memory OS projections first:

```powershell
munin memory-os overview --scope user --format text
munin memory-os profile --scope user --format text
munin memory-os friction --scope user --format text
```

Raw recall/session history is fallback provenance, not the default answer path.

## Project Shape

- `src/bin/munin.rs` - product CLI entrypoint
- `src/analytics/` - Memory OS read surfaces and session ingestion
- `src/core/` - tracking, strategy, Memory OS models, and projections
- `src/core/proactivity.rs` - morning proactivity queue, schedule, and run logic
- `src/session_brain/` - current-session startup brain
- `src/session_intelligence/` - local Claude/Codex session readers
- `src/proactivity_cmd.rs` - proactivity CLI rendering

`munin proactivity schedule-install` installs the morning runner for the current operating system. On Windows it uses Task Scheduler, on macOS it installs a LaunchAgent, and on Linux it installs a systemd user timer. New installs default to automatic spawning at the scheduled morning run.

## Notes

- The crate package is `munin-memory`; the installed command is `munin`.
- The GitHub repository is the source of truth for the open-source CLI.
- `munin` is the product-facing command.
