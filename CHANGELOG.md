# Changelog

## 0.5.10 - 2026-04-24

### Fixed

- Session Brain step parsing now normalizes punctuation and spacing in assistant/user step phrases, so decisions and rejections survive forms like `Keep-step 4` and `Step-4 rejected`.
- Assistant-side Session Brain materiality now ignores quoted, reported, and explicitly negated step phrases instead of misclassifying them as real decisions.

## 0.5.9 - 2026-04-22

### Fixed

- Session Brain now keeps current-session data relevant by filtering old task prose out of strategy and user operating model output, preserving exact-session current ask handling, and keeping strategy context project-scoped.
- Release Doctor now recursively audits public docs for unsupported or unresolvable `munin` command promises and reports all public-doc parity failures together.

## 0.5.8 - 2026-04-21

### Fixed

- Session Brain now treats only exact session-id transcript matches as live, so a newer same-project transcript from another terminal cannot appear as the current ask.
- Live user messages now suppress durable memory and strategy fallback even when the ask is not classified, preventing stale memory from replacing the active session.

## 0.5.7 - 2026-04-22

### Added

- `munin friction --format text` now includes a `New But Unproven Friction Points` section for high-signal corrections that should be avoided immediately, even before they are repeated enough to become codified friction fixes.

### Fixed

- Strategy discovery now treats `strategic-plan.context.json` as the authoritative OPSP artifact, including standalone `strategy/` folders and legacy Context strategy stores.
- `munin strategy status` and Session Brain can now read a formal OPSP JSON sidecar directly instead of falling back to Memory OS prose when no Munin registry/kernel wrapper is configured.

## 0.5.6 - 2026-04-21

### Added

- Added Munin-owned runtime context packets and installable Claude/Codex integration assets so end-user runtime context no longer depends on the local Context wrapper.
- Added architecture documentation for runtime context ownership and the Context strangler migration.
- Added `munin show` and `munin diff` artifact helpers plus Munin-native artifact IDs.
- Added the `scripts/munin-release-check.ps1` local release verification script.

### Changed

- `munin brain` now emits compiled public context only: no raw transcript paths, no raw message blocks, no stale progress chatter, and no repeated clipped subgoals.
- Session Brain now accepts partial strategy memory from compiled Memory OS when no complete formal strategy kernel is configured.
- Session Brain now defaults to compiled Memory OS overview/profile context, including project focus and user operating model, while keeping full heavyweight user context opt-in.
- Claude install now writes `munin-runtime.js`, prunes legacy `context-*.js` prompt hooks, and keeps unrelated hooks intact.
- Runtime data, config, artifact, and tee write paths now use Munin-owned roots and environment variables.

### Fixed

- Fixed package contents so `src/runtime_context/**` is included in crates.io packages.
- Fixed legacy `@context/a_` artifact fallback so it stays read-only and cannot write ref sidecars into Context roots.
- Fixed Session Brain project-root inference so the current invocation worktree wins over broader transcript roots.
- Fixed `install --check-resolvable` so it can run from arbitrary working directories.

## 0.5.5 - 2026-04-20

### Fixed

- Morning proactivity spawn on Windows no longer fragments the prompt. The spawn command quoted inner paths and placeholders (`"{brief_path}"`, `"{job_id}"`, `--error "<concrete error>"`) with double quotes, which `quote_for_cmd` then doubled to `""..."".` cmd interpreted each `""` as quote-close-then-open and split the prompt into 28+ separate tokens, so the spawned Claude session received stray args like `--error` and `--summary` and rejected them. The prompt now uses single quotes around placeholders, which cmd passes through verbatim, so the whole prompt reaches the spawned session as one intact argument.
- Claude proactivity launches now preserve the inherited Claude/Anthropic environment instead of clearing `CLAUDECODE`, `ANTHROPIC_API_KEY`, and `ANTHROPIC_AUTH_TOKEN` before starting the spawned session.
- Pairs with a companion fix to the user's `claude.ps1` wrapper (separate file, outside this repo) that dropped `[Parameter(ValueFromRemainingArguments)]` to keep the script a plain script instead of an advanced function — otherwise PowerShell matched `--error` against auto-added common parameters (`-ErrorAction`/`-ErrorVariable`) and crashed with AmbiguousParameter before the prompt ever reached claude.exe.

## 0.5.4 - 2026-04-20

### Changed

- `munin friction --format text` renames the raw-corrections block from "Correction Evidence" to "Corrections to Codify" and frames each repeat wrong->corrected pattern as an actionable candidate for a codified fix. Each item is numbered and carries an `uncodified` or `replaying-clean` status so agents can see which patterns still need a permanent rule.
- Correction pattern rendering now uses a shared-prefix-aware diff, so wrong/corrected commands with long common prefixes still show the part that actually differs instead of two identical truncations.

## 0.5.3 - 2026-04-20

### Changed

- Generated Munin umbrella skill now includes a step 0 that blocks auto-running `recall`, `doctor`, `proactivity`, or other status commands on a bare Munin invocation with no substantive ask. Agents stop and ask the user what to check instead.

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
