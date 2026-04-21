# Munin

Local memory for Claude Code and Codex.

Munin ingests Claude and Codex sessions and compiles them into local Memory OS
surfaces for live continuity, strategy, and recall.

It exposes that memory back to your agent through CLI commands, Claude slash
commands, and Codex skills. It is designed for developers who want agents to
remember active work, repeated mistakes, strategic priorities, and unfinished
tasks without sending that memory to a hosted service.

Current release: `v0.5.6`.

[![CI](https://github.com/inchwormz/munin-memory/actions/workflows/ci.yml/badge.svg)](https://github.com/inchwormz/munin-memory/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/munin-memory.svg)](https://crates.io/crates/munin-memory)
[![License: Apache-2.0](https://img.shields.io/badge/License-Apache--2.0-blue.svg)](LICENSE)
[![GitHub stars](https://img.shields.io/github/stars/inchwormz/munin-memory?style=social)](https://github.com/inchwormz/munin-memory)

Munin is open source under the Apache 2.0 license.

## Runtime Ownership

Munin is the product owner for runtime context. End users should not need the
`context` tool installed in order to use Munin's Session Brain, startup brief,
strategy surfaces, or Claude/Codex integration assets.

Architecture docs:

- [Runtime Context Ownership](docs/architecture/runtime-context-ownership.md)
- [Context Strangler Migration](docs/architecture/context-strangler-migration.md)

## What Munin Does

<img width="996" height="795" alt="image" src="https://github.com/user-attachments/assets/5e2d0faa-ee36-40e6-8c08-e31c59769f6f" />


Munin has five local layers:

1. **Session ingestion** reads first-class Claude and Codex sessions: prompts,
assistant turns, shell commands, outcomes, corrections, working directories, and
timestamps. Claude subagent internals are excluded by design.
2. **Memory compilation** converts those sessions into a local Memory OS:
evidence-backed facts, active projects, strategic plans, continuity commitments,
open loops, repeated friction, command outcomes, strategy context, and next steps.
3. **Strategy and proactivity** turn memory into concrete tasks. `munin nudge`
now combines strategy red/yellow items with continuity work from previous
completed sessions, active projects, and verified incomplete tasks. The morning
proactivity spawns a Claude session that can evaluate state on a schedule and start the next work
session and infer what tasks should be executed based on goal/project deltas. Where you are now,
where you want to be, what closes the delta? It will enable Claude/Codex to be proactive to help
you reach your goals.
5. **Agent access** installs Claude skills, Claude slash commands, Codex skills,
and a Codex plugin skill bundle so agents can query compiled memory instead of
trawling raw transcripts.
6. **Session Brain** compiles the current live session into a short working
memory layer: who the user is, what they care about, the current ask, active
strategy, blockers, and next actions. It is for long context windows where the
agent needs current-session oversight, not historical search.

No hosted service is required. The compiled state stays on the machine running
Munin.

## Install For A New User

Today, Munin distribution is binary-first. A Claude/Codex plugin marketplace package is
not live yet. Install the `munin` binary first, then ask Munin to install the
agent-facing skills and commands.

### Option A: Install From crates.io

```powershell
cargo install munin-memory --force
```

### Option B: Install From GitHub Source

```powershell
git clone https://github.com/inchwormz/munin-memory.git
cd munin-memory
cargo install --path . --force
```

Verify it:

```powershell
munin --version
munin install --check-resolvable
```

## Install Agent Skills

After the binary works, install the surfaces for the agent you use. The
supported runtime path is Munin-only.

### Claude Code

```powershell
munin install --claude --force
```

This writes:

- `~/.claude/skills/munin*/SKILL.md`
- `~/.claude/commands/munin*.md`

Restart Claude Code, then use slash commands:

```text
/munin-memory-os-ingest
/munin-doctor
/munin
/munin-nudge
/munin-proactive
/munin-recall topic <query>
/munin-strategy
```

### Codex

```powershell
munin install --codex --force
```

This writes:

- `~/.codex/skills/munin*/SKILL.md`
- `~/.codex/plugins/munin-memory/...`

Restart Codex, then use skills:

```text
$munin-memory-os-ingest
$munin-doctor
$munin
$munin-nudge
$munin-proactive
$munin-recall
$munin-strategy
```

### Both Agents

```powershell
munin install --force
```

By default this installs both Claude and Codex surfaces.

## First Run

Run ingestion once after installing agent surfaces:

```powershell
munin memory-os ingest --format text
```

From Claude:

```text
/munin-memory-os-ingest
```

From Codex:

```text
$munin-memory-os-ingest
```

The installed ingestion skill uses `--force` so demos and repeated test runs
show timing and corpus counts every time. Direct CLI ingestion without
`--force` is incremental and will say `current` when nothing new needs import.

Typical output:

```text
Memory OS Ingest
----------------
Elapsed: 46.70s
Mode: forced replay
Status: imported
Sessions processed: 2910
Shell executions ingested: 35065
Corrections ingested: 868
```

Then check health:

```powershell
munin doctor --scope user --format text
```

or from Claude/Codex:

```text
/munin-doctor
$munin-doctor
```

## Daily Usage

Use these surfaces at the start of work or when an agent gets lost:

```powershell
munin resume --format prompt
munin brain --format prompt
munin nudge --format text
munin recall --format text "<topic>"
munin memory-os brief --scope user --format text
munin memory-os overview --scope user --format text
munin memory-os friction --scope user --format text
```

The agent-facing equivalents are:

```text
/munin
/munin-brain
/munin-nudge
/munin-recall topic <query>
/munin-friction
```

and in Codex:

```text
$munin
$munin-brain
$munin-nudge
$munin-recall
$munin-friction
```

## What `munin nudge` Does

`munin nudge` returns concrete work, not just diagnosis. It combines:

- strategy items that are red/yellow or missing metrics
- logical next tasks from recent completed sessions and project activity
- continuity commitments from earlier work
- verified incomplete tasks and open obligations

Example shape:

```text
Strategy Nudge
--------------
Scope: sitesorted-business
Suggested task queue:
1. Address red-state `Outreach reply rate`
2. Resume incomplete work: Finish recording-ready Munin onboarding
3. Continue munin-memory: Update docs and verify install surfaces
Execution:
- Start with this intervention: Address red-state `Outreach reply rate`.
- Work it until implemented, verified, or blocked by a concrete recorded blocker.
```

## Strategy Planning

Munin also ships a strategy skill:

```text
/munin-strategy
$munin-strategy
```

Use it to create or update a One-Page Strategic Plan, bootstrap a strategy
kernel, or triage tasks against goals. After strategy setup, `munin nudge` uses
the strategy kernel and metrics snapshot to recommend the next task.

Useful CLI surfaces:

```powershell
munin strategy setup --scope <scope> --import <strategic-plan.context.json>
munin strategy status --scope <scope> --format text
munin strategy recommend --scope <scope> --format text
munin metrics set <metric_key> <value> --scope <scope>
```

## Install And Debug Commands

These commands are useful when testing a new install:

```powershell
munin install --dry-run
munin install --check-resolvable
munin install --claude --dry-run
munin install --claude --force
munin install --codex --dry-run
munin install --codex --force
```

Installed quick surfaces:

```text
/munin-install-check
/munin-install-claude-preview
/munin-install-claude
/munin-install-codex
/munin-memory-os-ingest
/munin-proactive
```

Codex equivalents use `$...`.

`munin install` archives old Munin skill folders into `.munin-legacy` by default.
Use `--keep-legacy` to leave them in place.

## Non-Technical Release Check

For a plain PASS/WARN/FAIL readout before trusting a local build, run:

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\munin-release-check.ps1
```

The check verifies the binary, install contract from the repo, install contract
from a temporary folder, memory health, promotion proof, and resolver routing.
`WARN` means Munin is mechanically usable but has a follow-up to review. `FAIL`
means fix the named item before trusting the release.

## Privacy And Storage

Munin reads local agent history and writes a local SQLite-backed Memory OS under
the platform local data directory. On Windows this is normally:

```text
%LOCALAPPDATA%\context\history.db
```

For testing or demos, you can isolate paths:

```powershell
$env:MUNIN_INSTALL_HOME = "C:\tmp\munin-home"
$env:MUNIN_SESSION_HOME = "C:\tmp\munin-home"
$env:MUNIN_DATA_DIR = "C:\tmp\munin-data"
```

## Current Distribution Status

The current supported install path is:

1. Install the binary from crates.io or GitHub source.
2. Run `munin install --claude --force`, `munin install --codex --force`, or
   `munin install --force`.
3. Restart the agent and use the installed skills/slash commands.

A GitHub-backed Claude plugin marketplace package is planned but not live in
this testing build.

## Development

Repository layout:

- `src/bin/munin.rs` - CLI entrypoint and skill/command installer
- `src/analytics/` - Memory OS read surfaces and session ingestion
- `src/core/` - durable tracking, Memory OS, strategy, proactivity, and resolver
- `src/session_brain/` - current-session summary
- `src/session_intelligence/` - local Claude/Codex session readers
- `src/assets/skills/` - bundled installable prose skills
- `tests/` - CLI, resolver, package, and fixture tests

`munin proactivity schedule-install` installs the morning runner for the current operating system. On Windows it uses Task Scheduler, on macOS it installs a LaunchAgent, and on Linux it installs a systemd user timer. New installs default to automatic spawning at the scheduled morning run.

## Notes

Validation:

```powershell
cargo fmt
cargo test
cargo build --bin munin
munin install --check-resolvable
```

Package note: the crate package is `munin-memory`; the installed command is
`munin`.
