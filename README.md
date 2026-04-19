# Munin

Local memory system for agent-driven development.

Current testing build: `v0.5.0-beta.1`.

## What It Is

`munin` reads local Claude/Codex sessions, compiles startup memory, surfaces repeated friction, and keeps noisy shell output out of agent context. It is the memory product, not the old command-wrapper package.

Core surfaces:

- `munin resume --format prompt` for the startup memory brief
- `munin brain --format prompt` for the live Session Brain
- `munin nudge` for strategy-backed next-step recommendations
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
munin hygiene --root . --format text
munin doctor --scope user
munin install --dry-run
munin install --check-resolvable
```

`munin install` archives old Munin/context-wrapper skill names into `.munin-legacy` by default. Use `--keep-legacy` if you want the old skill folders left in place, or `--force` to refresh existing Munin skill files.

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
- `src/session_brain/` - current-session startup brain
- `src/session_intelligence/` - local Claude/Codex session readers

## Notes

- The crate package is `munin-memory`; the installed command is `munin`.
- Treat the current working tree as the source of truth.
- `munin` is the product-facing command.
