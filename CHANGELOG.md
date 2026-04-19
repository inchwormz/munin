# Changelog

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
