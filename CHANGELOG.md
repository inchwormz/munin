# Changelog

## 0.5.2 - 2026-04-20

### Added

- `munin-proactive` skill and `/munin-proactive` (Claude) / `$munin-proactive` (Codex) slash command. Runs `munin proactivity run --no-spawn --format text` on demand so agents can invoke the morning strategic proactivity cycle at any time without waiting for the scheduled 8am task.

### Fixed

- `munin friction --format text` now renders up to 10 active friction fixes numbered `1.`–`10.` with full body, and lists any additional non-fixed items in an `Additional Active Friction (N more, awareness only)` block. The text surface previously capped at the first item even when 10+ active fixes existed in the JSON output.
- Dropped the silent `fixes.truncate(10)` in the friction fix builder so downstream consumers (including JSON) can see the full sorted list.

## 0.5.1 - 2026-04-20

### Changed

- New proactivity installs default to `auto_spawn = true`.
- `munin proactivity schedule-install` now writes `auto_spawn = true` into config when it installs the daemon.
- Proactivity scheduler support is platform-aware: Windows Task Scheduler, macOS LaunchAgent, and Linux systemd user timer.
- GitHub CI now runs Rust checks on Windows, macOS, and Linux.

## 0.5.0-beta.3 - 2026-04-20

### Changed

- Pre-release of 0.5.1. Same changes as 0.5.1 — platform-aware proactivity scheduler, auto_spawn default, GitHub CI matrix.

## 0.5.0-beta.2 - 2026-04-19

### Added

- `munin proactivity` now owns the morning strategy/continuity runner that previously lived under `context proactivity`.
- Proactivity schedule install/remove/status, queue claim/approve/reject/complete, and no-spawn run surfaces are available from the public Munin CLI.

### Changed

- Proactivity scheduled tasks now install as `Munin-Proactivity-*` tasks and remove legacy `Context-Munin-Proactivity-*` tasks during install/remove.
- Morning proactivity briefs now use a bounded compiled Memory OS overview instead of the heavier startup prompt renderer.

## 0.5.0-beta.1 - 2026-04-19

First clean Munin open-source testing build.

### Added

- Munin access-layer resolver and generated Codex/Claude skill contracts.
- Resolver fixture checks for the main memory surfaces.
- `munin hygiene` for duplicate guidance reports.
- `munin proactivity` for scheduled strategy/continuity evaluation, morning briefs, queue approval, and result capture.
- Strategy KPI metric read/write surfaces.
- Independent Memory OS promotion proof gate for `test-private` and `adversarial-private`.

### Fixed

- Strategy metrics now hydrate KPI slots from the ingested strategy plan before current values exist.
- Session Brain stale fallback status now renders as `stale`.
- Release Doctor rejects both `stale` and legacy `stale-fallback` Session Brain states.
- Relative project paths resolve to the project root before Memory OS filtering.
- Public site/docs command examples no longer point at stale wrapper commands.

### Known Limits

- This is a beta testing build, not a final public product launch.
- Fresh install still needs to be proven from a clean checkout/profile.
- Strategy KPI values are local user data and must be filled with real business numbers.
- Memory quality still needs a real work sprint across Codex and Claude before final `v0.5.0`.
