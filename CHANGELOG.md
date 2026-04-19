# Changelog

## 0.5.1-beta-3 - 2026-04-20

### Fixed

- `munin install --check-resolvable` now works from a crates.io install without requiring the source repository's test fixture directory.

## 0.5.1-beta-2 - 2026-04-20

### Changed

- README install flow now starts from the crates.io package and follows with `munin install --force` plus resolver validation.

## 0.5.1-beta-1 - 2026-04-20

### Fixed

- Session Brain now fails closed when a live Codex or Claude session id is present but its transcript is missing.
- Session Brain no longer falls back to another session's latest transcript in that missing-live-transcript state.
- Session Brain no longer injects broad Memory OS current-work/profile summaries that can look like cross-session leakage.

## 0.5.0-beta.1 - 2026-04-19

First clean Munin open-source testing build.

### Added

- Munin access-layer resolver and generated Codex/Claude skill contracts.
- Resolver fixture checks for the main memory surfaces.
- `munin hygiene` for duplicate guidance reports.
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
