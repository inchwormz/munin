---
name: munin-friction
description: Show repeated Munin friction and correction patterns. Use when the user asks what keeps going wrong or what agents keep repeating.
---
# munin-friction

## When to use
Use this for recurring agent mistakes, repeated corrections, and fixed friction that should fade into the background.

## Primary command

```powershell
munin friction --last 30d --format text
```

Fallback command:

```powershell
munin doctor --scope user --format text
```

## How to read output
- top fixes
- status
- permanent fix
- evidence

## Trust
- `active` and `codified` are current friction.
- `monitoring` in New But Unproven Friction Points is a single or newly detected high-signal correction worth avoiding, not yet proven enough for a codified fix.
- `monitoring`, `fixed`, and `retired` are background unless the user asks for history.
- Every surfaced fix should include its permanent-fix pointer when present.

## Fallback
- If output is empty, stale, or generic, do not invent an answer.
- If output is empty, say there is no active friction in the requested window.
- If all matching items are fixed or retired, say no current issues and summarize the fixed count.
- If the report looks stale, run doctor before turning it into user guidance.
- If unsure this is the right skill, run `munin resolve "<ask>"` and follow its route.

## What not to do
- Do not invent friction when the filtered report is empty.
- Do not treat retired items as urgent new work.

## Done
You're done when the answer:
- Lists the top 1-3 active/codified issues and any monitoring one-off points when present.
- Includes each issue's permanent-fix pointer.
- Mentions retired/fixed items only when asked or when explaining empty current friction.
