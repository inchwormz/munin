# Runtime Context Ownership

## Purpose

This document defines the target ownership boundary for runtime context in
Munin.

Munin is the product that end users install. End users do not need Context.
Claude and Codex integrations must call `munin` only.

## Source Of Truth

Munin owns:

- Session Brain semantics
- startup/resume semantics
- runtime packet contract
- freshness labels and fallback rules
- runtime cache roots and schema names
- Claude and Codex integration assets
- product-facing documentation

Context does not own end-user runtime context behavior. During migration it may
exist only as an internal legacy input source.

## Runtime Surfaces

Munin exposes two human-facing continuity surfaces today:

- `munin brain --format prompt`
- `munin resume --format prompt`

Target shape:

```text
                 ┌──────────────────────────────┐
                 │         munin CLI            │
                 │  brain / resume / install    │
                 │  future shared packet entry  │
                 └──────────────┬───────────────┘
                                │
                                ▼
                  ┌────────────────────────────┐
                  │ RuntimeContextPacketV1     │
                  │ - surface_kind             │
                  │ - source_mode              │
                  │ - freshness                │
                  │ - provenance refs          │
                  │ - agenda/state or redirect │
                  │ - project capsule          │
                  │ - strategy summary         │
                  │ - user model summary       │
                  └──────────────┬─────────────┘
                                 │
                ┌────────────────┴───────────────┐
                │                                │
                ▼                                ▼
       Claude integration asset         Codex integration asset
       (Munin-owned)                    (Munin-owned)
```

## Packet Contract

The runtime packet must not reuse `CompiledContext` as the public runtime
contract. `CompiledContext` remains a legacy adapter input during migration.

`RuntimeContextPacketV1` is the target shared model for both `brain` and
`resume`.

Required fields:

- `surface_kind`
- `source_mode`
- `freshness`
- `session_id`
- `project_root`
- `provenance`
- `agenda`
- `state`
- `project`
- `strategy`
- `user_operating_model`
- `guidance`

## Freshness Rules

`brain` supports:

- `live`
- `fallback-latest`
- `stale`
- `none`

Non-live `brain` is redirect-only. It must not emit synthetic current-session
agenda or blocker state as if it were live.

Expected behavior:

```text
live            -> live agenda/state allowed
fallback-latest -> provenance + redirect to resume
stale           -> provenance + redirect to resume
none            -> provenance + redirect to resume
```

## Hybrid Freshness Model

The runtime packet uses two layers:

```text
per-turn live layer
- current ask
- agenda
- blockers
- project root
- freshness/provenance

cached slow layer
- user operating model
- strategy summary
- slow Memory OS-derived summaries
```

The live layer is recomputed every runtime call. The cached layer is stored in a
Munin-owned cache root and refreshed on bounded TTL / invalidation rules.

## Ownership Matrix

Munin-owned fields and identifiers include:

- packet wrapper tags
- artifact IDs and prefixes
- reopen hints
- stored reentry and action commands
- `actor_id`
- `runtime_profile`
- stream IDs
- event IDs
- idempotency keys
- `schema_fingerprint`
- projection checkpoint names
- runtime env vars
- runtime cache roots

No active runtime-facing field should use `context` naming after cutover.

## Runtime Integration Rule

Runtime integrations are thin. They call the Munin CLI only. They do not
reimplement timeout, freshness, or fallback policy.

```text
Claude/Codex asset
    -> call munin CLI
    -> read runtime packet
    -> hand packet to runtime
```

## Performance Gates

- `munin brain --format prompt`
  - warm median <= 250 ms
  - warm p95 <= 500 ms
- `munin resume --format prompt`
  - warm median <= 400 ms
  - warm p95 <= 800 ms
- `munin install --check-resolvable`
  - <= 2 s

## What This Document Is Not

- It is not the migration checklist.
- It does not describe Context wrapper internals.
- It does not define token-savings or shell-wrapper behavior.
