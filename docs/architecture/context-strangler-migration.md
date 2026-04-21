# Context Strangler Migration

## Purpose

This document defines the internal migration path for removing Context from the
end-user runtime path while preserving local continuity during the cutover.

End users should install Munin only. Context may exist temporarily as an
internal legacy input source during migration.

## Migration Rule

Cut the edge hard, cut the substrate soft.

- Public/runtime-facing behavior moves to Munin first.
- Legacy Context reads remain temporarily available internally.
- Writers do not flip until readers can understand both worlds.

## Phases

### Phase 0: Boundary ADR + RuntimeContextPacketV1

- Define the target packet contract.
- Explicitly reject `CompiledContext` as the public runtime contract.
- Define non-live `brain` redirect semantics.

### Phase 1: Public Edge Cutover

- Munin docs become the source of truth.
- Munin install surfaces become the only supported Claude/Codex integration path.
- Runtime-facing assets call `munin` only.

### Phase 2: Reader-First Dual-Read

- Projections/readers accept both legacy Context-shaped identifiers and new
  Munin-shaped identifiers.
- Supported legacy read inputs include old:
  - event kinds
  - stream IDs
  - artifact IDs
  - reopen hints
  - runtime profiles
  - stored reentry/action commands

### Phase 3: Writer Cutover

- New writes switch to Munin-owned identifiers and roots.
- No active writer emits new Context-shaped runtime state.

### Phase 4: Import / Backfill

- Old local state is imported or normalized exactly once where needed.
- Duplicate suppression must prevent double-counting during mixed-read windows.

### Phase 5: Legacy Read Retirement

- Remove internal dual-read paths only after:
  - packet tests are green
  - migration tests are green
  - parity checks are green
  - release gates confirm no active `context` runtime references remain

## Migration Order Diagram

```text
        old Context-shaped state exists
                    │
                    ▼
          readers accept both formats
                    │
                    ▼
          new writers emit Munin only
                    │
                    ▼
           import / normalize old state
                    │
                    ▼
            retire legacy read paths
```

## Field-Level Cutover Checklist

Fields that must be audited and migrated together:

- artifact IDs / prefixes
- reopen hints
- stored reentry commands
- stored action commands
- `actor_id`
- `runtime_profile`
- stream IDs
- event IDs
- idempotency keys
- `schema_fingerprint`
- projection checkpoint names
- env vars
- cache/data roots

## Real Reader / Writer Areas

At minimum, the migration must account for:

- `src/core/tracking/checkpoint.rs`
- `src/core/tracking/claim_leases.rs`
- `src/core/tracking/read_model.rs`
- `src/core/tracking/kernel.rs`
- `src/core/tracking/evidence.rs`
- `src/core/tracking.rs`
- `src/core/artifacts.rs`
- `src/core/config.rs`
- `src/core/worldview.rs`
- `src/analytics/memory_os_cmd.rs`
- `src/session_brain/mod.rs`

## Runtime Asset Bundle

Runtime-facing Claude/Codex assets must move to a versioned bundle under
Munin-owned assets.

Target:

```text
src/assets/integrations/v1/
  claude/
  codex/
  manifest.json
  hashes.json
```

`munin install --check-resolvable` must verify:

- asset version
- asset hash parity
- no stray `context` commands in active runtime assets
- no stray Context-owned hook filenames in active runtime assets

## Failure Modes To Guard Against

```text
reader flipped too late
-> old state disappears after writer cutover

writer flipped too early
-> projections miss new runtime events

asset bundle drifts
-> Claude and Codex inject different behavior

non-live brain still renders agenda/state
-> stale context appears current
```

## Release Gates

Release must fail if:

- active docs still present Context as runtime owner
- active install surfaces emit Context commands
- packet shape tests fail
- non-live `brain` renders synthetic agenda/state
- migration duplicate-suppression tests fail
- benchmark gates fail
