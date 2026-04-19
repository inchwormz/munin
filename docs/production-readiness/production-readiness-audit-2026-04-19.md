# Munin Production Readiness Audit

Generated: 2026-04-19

## Method

This audit inventoried local Munin planning/proof/customer artifacts across:

- `C:\Users\OEM\Projects\context`
- `C:\Users\OEM\Projects\context.omx-worktrees`
- `C:\Users\OEM\Projects\munin-memory`
- `C:\Users\OEM\Projects\munin-site`
- `C:\Users\OEM\Projects\strategy`
- `C:\Users\OEM\.gstack\projects`

Inventory outputs:

- `docs/production-readiness/munin-planning-doc-inventory.csv`
- `docs/production-readiness/munin-planning-doc-inventory-unique.csv`

Discovery found 2,061 candidate references and 873 unique content hashes. Most
are generated site artifacts, test fixtures, or duplicate worktree copies. The
canonical planning inputs used for this audit are listed below.

QMD recall was attempted, but the local Windows `qmd.ps1` shim currently fails
by trying to invoke `/bin/sh.exe`. This is itself a production-readiness tooling
gap for historical recall provenance.

## Canonical Planning Sources

| Area | Source |
|---|---|
| Production PRD | `context.omx-worktrees/plan-munin-production-prd/docs/plans/2026-04-18-munin-production-prd.md` |
| Selective GBrain PRD | `context.omx-worktrees/feat-munin-selective-gbrain-prd/docs/plans/2026-04-15-munin-selective-gbrain-prd.md` |
| Outside voice review | `context.omx-worktrees/feat-munin-selective-gbrain-prd/docs/plans/2026-04-15-munin-selective-gbrain-prd-outside-voice-brief.md` |
| Lifecycle model | `context.omx-worktrees/feat-munin-selective-gbrain-prd/docs/memory-os-lifecycle.md` |
| Promotion gate | `context/docs/memory-os-phase5-promotion.md` |
| Foundation / proof surface | `context/docs/memory-os-v1-foundation.md` |
| Architecture audit | `context/docs/audit/memory-os-audit-2026-04-17.md` |
| Native-vs-Munin benchmark ADR | `context.omx-worktrees/feat-munin-selective-gbrain-prd/docs/architecture/adr/0010-native-vs-munin-benchmark-runner.md` |
| Benchmark scripts | `context/scripts/benchmark-codex-munin-*` |
| Private beta / acquisition plan | `.gstack/projects/context/OEM-feat-content-hash-filter-trust-design-20260412-191927.md` |
| CEO crack retrospective | `.gstack/projects/Projects/ceo-plans/2026-04-18-munin-crack-retro.md` |
| Munin site copy | `munin-site/*.html` |
| Current source | `munin-memory` |

## Current Verified State

Commands run against the separated `munin-memory` project and installed binary:

| Check | Result |
|---|---|
| `cargo build` | pass |
| `cargo test` | 328 passed |
| `cargo install --path . --force` | installed `munin 0.5.0-beta.3` from `C:\Users\OEM\Projects\munin-memory` |
| `munin install --check-resolvable` | 31 Munin commands parsed successfully |
| `munin --version` | `munin 0.5.0-beta.3` |
| `munin resolve "memory hygiene"` | routes to `hygiene` |
| `munin hygiene --format text` | now skips worktree/runtime/cache dirs; dry-run only |
| `munin memory-os promotion --format text` | blocked: missing `test-private` and `adversarial-private` proof rows |
| `munin doctor --scope user --format text` | warn: metrics empty and proof incomplete |
| `munin doctor --release --repo-root munin-memory --site-root munin-site` | fails: `munin-site/docs.html` contains unsupported `munin init` |
| `munin metrics get --scope sitesorted-business` | no metric values recorded |
| `munin nudge --format text` | low-confidence "Instrument or measure `Build the proof engine`" |
| `munin recall "Munin resolver"` | poor relevance: returns Munin site EAG handoff records rather than resolver planning evidence |
| `munin brain --format text` from `munin-memory` | source status `none`, not live |

## What Is Done

1. **Source separation is real now.** `C:\Users\OEM\Projects\munin-memory` is a standalone git repo and source of truth for the `munin` CLI.
2. **Basic CLI health is good.** Build, tests, install, resolver parsing, and core command parsing pass.
3. **Hygiene is safer than before.** Worktrees/runtime/cache dirs are skipped; write-mode planning distinguishes inherited duplicates from sibling/cross-agent duplicates.
4. **Strict promotion gate exists and blocks honestly.** It does not silently claim cutover when required proof rows are missing.
5. **Strategy/nudge surface reports missing metrics instead of pretending certainty.**

## Production Blockers

### P0. Public docs currently fail release doctor

Evidence:

- `munin doctor --release --repo-root C:\Users\OEM\Projects\munin-memory --site-root C:\Users\OEM\Projects\munin-site --format text`
- Failure: `munin-site/docs.html contains unsupported munin init`

Additional site-copy mismatches:

- `features.html` mentions `munin gain`; this is not a current Munin command.
- homepage says proof-gated cutover uses held-out/adversarial splits while live promotion is currently blocked.
- docs still describe proof establishment with old Context commands in places.

Required fix:

- Rewrite website/docs around the current binary truth.
- Keep proof copy honest: "blocked until private/adversarial proof rows exist" unless the gate is green.
- Add a site/README command parity test that checks every `munin ...` example against `munin --help` or Clap parsing.

### P0. Promotion proof is not production-ready

Evidence:

`munin memory-os promotion --format text`:

- strict gate enabled
- read model enabled
- resume cutover blocked
- handoff cutover blocked
- required proof set: `proposed-kernel / test-private+adversarial-private`
- matching results: 0
- missing splits: `test-private`, `adversarial-private`

The old Phase 5 doc only required `dev-public`; current code correctly requires
private/adversarial proof. The docs and public copy need to catch up.

Required fix:

- Generate independent proposed-kernel results for `test-private` and `adversarial-private`.
- Ensure proof rows are written by a verified command path, not manually.
- Make `munin prove` show run ids, corpus hash, split, system, score, contamination status, and evidence path.
- Do not publish "proof-gated continuity" claims until this passes.

### P0. Native-vs-Munin benchmark is planned but not shipped in `munin-memory`

ADR 0010 defines the benchmark:

- baselines: native Claude Code, native Codex, Munin-hydrated, Munin-disabled control
- scenarios: long-session-drift, hot-resume, interrupted-debugging, branch-switch
- metrics: completion quality, verified-fact count, hallucination rate, token burn
- output: JSON scorecards and markdown summary with corpus hash and run date

Current state:

- benchmark corpus exists in old worktree fixtures
- output contract exists in old worktree docs
- `munin-memory` does not contain `bench_native_vs_munin` runner code
- `rg bench_native_vs_munin` in `munin-memory` only finds stale fixture references, not an executable runner

Required fix:

- Port/implement `src/bin/bench_native_vs_munin.rs` or an explicit `munin benchmark native-vs-munin` command.
- Move benchmark corpus into `munin-memory/tests/fixtures/bench_native_vs_munin/v1`.
- Support fixture-only validation before any live API/CLI baseline.
- Record corpus hash, model/version pins, launcher setup/teardown tokens, contamination checks, and fixed seed.

### P0. Recall is not acquisition-proof

The production PRD requires `munin recall "<topic>"` to be topic-specific.

Current evidence:

- `munin recall "Munin resolver"` returned Munin site EAG fidelity handoff records, not resolver planning decisions.
- `munin recall "native vs Munin benchmark runner"` returned broad onboarding checkpoint prompts, not ADR 0010 or runner artifacts.

Required fix:

- Improve recall ranking and source weighting so exact topic docs/checkpoints outrank generic site/build artifacts.
- Add golden recall tests:
  - `Munin resolver`
  - `native vs Munin benchmark runner`
  - `promotion proof`
  - `private beta validation`
- Show evidence path and why each match was selected.
- If no good topic evidence exists, say so instead of returning weak matches.

### P0. Customer/acquisition validation evidence is missing

The private beta plan says acquisition proof runs through users:

- recruit 5-20 serious AI coding power users
- retain at least 5 after two weeks as learning floor
- 10+ active for 2+ weeks as bar for external outreach
- collect measurable token savings from non-founder users
- collect at least 3 strong user quotes
- retain >50% after 2 weeks
- median token savings >=40%
- build a data deck with variance, retention, quotes, and before/after metrics

Current state:

- no external beta cohort evidence found
- no shareable `munin report` or equivalent beta report command found
- no customer quotes/case studies found
- site copy is product-marketing heavy but does not carry real user proof

Required fix:

- Build a local-first beta report artifact (`munin report --format html|json`) or equivalent.
- Create a beta evidence schema:
  - user id / pseudonym
  - install date
  - active days
  - sessions/week
  - token savings distribution
  - compaction/recovery events
  - qualitative quote
  - "would be upset if removed?" response
- Create a private beta packet:
  - install instructions
  - privacy statement
  - weekly check-in template
  - sample local report
  - consent wording for using anonymized metrics/quotes

### P1. Strategy metrics are empty

Evidence:

- `munin metrics get --scope sitesorted-business --format text`: no metric values recorded.
- `munin nudge --format text`: low-confidence setup nudge because no initiative status signal exists.
- `munin doctor`: warns strategy metrics are empty.

Required fix:

- Decide instrumentation source for each KPI.
- Populate `metrics.json` via `munin metrics set` or `munin metrics sync`.
- Add a golden nudge test where metrics are present and the recommendation is business-actionable, not instrumentation setup.

### P1. Session Brain live/current proof is weak

Evidence:

- `munin brain --format text` from `munin-memory` returned `session source: none`.
- `munin resume --format prompt` returned useful global strategy but did not reflect the newest production-readiness audit ask.

Required fix:

- Make Session Brain surface clearly distinguish:
  - live current session
  - compiled fallback
  - no live source
- Add smoke tests around source labels.
- Ensure production docs never imply "current live awareness" unless the source status is live.

### P1. Release doctor stops too early and has narrow checks

Release doctor caught `munin init`, which is useful, but it exits on the first
bad doc command. For production, it should aggregate all public-contract failures.

Required fix:

- Aggregate all unsupported site/README command claims.
- Include benchmark/proof/metrics status in release doctor.
- Include installed-binary provenance: path and source repo.

### P1. Old Context copies still contain historical Munin implementation and plans

The new source repo is now clean, but the old `context` repo/worktrees still
contain Munin-era code/plans. That is useful archive evidence but dangerous as
a future source of truth.

Required fix:

- Add a deprecation note in Context docs or remove old Munin command paths once safe.
- Make installed skill guidance always point to `C:\Users\OEM\Projects\munin-memory`.
- Treat old Context worktrees as archive/planning evidence only.

## Benchmark Creation Plan

### Stage 1: Fixture-only runner

Goal: prove the benchmark harness and scoring are deterministic without live
model/API variables.

Required artifacts:

- `tests/fixtures/bench_native_vs_munin/v1`
- `src/bin/bench_native_vs_munin.rs` or `munin benchmark native-vs-munin`
- JSON schema for trial output
- summary markdown renderer

Acceptance:

- `--validate-only` checks all fixtures, oracle refs, canary ids, and corpus hash.
- `--executor fixture` scores checked-in trial responses deterministically.
- `cargo test` covers all four scenarios and all four baseline slugs.

### Stage 2: Live baseline runner

Goal: run the four baselines with fair setup and contamination controls.

Baselines:

1. `native-claude-code`
2. `native-codex`
3. `munin-hydrated`
4. `munin-disabled-control`

Required run metadata:

- date/time
- model ids
- CLI versions
- corpus hash
- random seed
- launcher setup/teardown tokens
- prompt contamination status
- API/auth mode

Acceptance:

- one scenario can run live end-to-end
- output is written to `docs/bench/native_vs_munin/v1/<date>/`
- failed auth/rate-limit runs are explicit invalid runs, not zero-score runs

### Stage 3: Publication proof

Goal: publish one honest acquisition/customer proof artifact.

Acceptance:

- at least one full run across all scenarios/baselines
- contamination-free
- model/version pinned
- manual reviewer notes attached where stochastic model output needs adjudication
- homepage/resources page links to the run and caveats

## Customer Validation Content Needed

1. **Beta landing / invite copy**
   - narrow target: AI coding power users with compaction/token pain
   - honest promise: local memory + evidence layer, not magic autonomy
   - ask: install for one week, share local report and quote

2. **Privacy and consent note**
   - no phone-home
   - user manually shares reports
   - metrics anonymized before deck/public usage

3. **Weekly check-in form**
   - still using it?
   - sessions this week
   - one moment it helped
   - one moment it hurt
   - would you be upset if it disappeared?
   - share report screenshot/HTML

4. **Case study template**
   - before workflow
   - Munin setup
   - measurable delta
   - quote
   - caveat

5. **Acquisition proof deck outline**
   - problem
   - local-first architecture
   - benchmark results
   - beta cohort metrics
   - retention
   - quotes
   - why platform companies should buy/build-with rather than ignore

## Recommended Execution Order

1. Fix public docs parity and release doctor aggregation.
2. Port/implement native-vs-Munin fixture runner in `munin-memory`.
3. Generate missing promotion proof rows for `test-private` and `adversarial-private`.
4. Fix recall quality for named Munin topics.
5. Populate strategy metrics enough for meaningful nudge output.
6. Build local beta report artifact.
7. Recruit 5 initial beta users and collect week-one evidence.
8. Only then update homepage with benchmark/customer proof claims.

## Current Verdict

Munin is not production/acquisition-ready yet.

It is now in a much better engineering position because the source is separated,
the CLI builds/tests, and hygiene behavior is safer. The remaining blockers are
not "more architecture." They are proof and truthfulness:

- prove the memory gate on private/adversarial splits
- run an honest native-vs-Munin benchmark
- make recall/nudge outputs high-signal on real topics
- make docs match the binary
- collect external customer proof

Until those are done, Munin should be framed as a strong local prototype/private
beta candidate, not a production-ready acquisition asset.
