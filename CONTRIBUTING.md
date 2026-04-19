# Contributing

Munin welcomes focused pull requests that improve the local memory CLI, agent
skill contracts, Memory OS surfaces, strategy metrics, install flow, tests, or
documentation.

## Contributor Posture

- PRs are welcome.
- AI-assisted contributions are welcome.
- Maintainer review may use AI-assisted code review, but final responsibility
  stays with the maintainer.
- Keep changes small and explain the user impact.
- Do not add hosted-product, billing, account, server, or proprietary service
  code to this repository. Hosted product work belongs in a separate private
  repository.

## Developer Certificate of Origin

All commits must be signed off using the Developer Certificate of Origin:

```text
Signed-off-by: Your Name <you@example.com>
```

You can add this with:

```powershell
git commit -s
```

By signing off, you certify that you wrote the contribution or otherwise have
the right to submit it under the Apache 2.0 license.

## Local Checks

Run these before opening a pull request:

```powershell
cargo fmt --check
cargo test --quiet
cargo build --quiet --bin munin
munin install --check-resolvable
```

If `munin` is not installed yet, use:

```powershell
cargo run --quiet --bin munin -- install --check-resolvable
```

## Scope Boundaries

This repository owns the local Munin CLI and open-core surfaces:

- local memory read surfaces
- Session Brain
- Memory OS projections and proof
- strategy KPI metrics
- installed Codex/Claude skills
- memory hygiene

This repository does not own:

- hosted SaaS code
- billing
- cloud account services
- private customer data
- proprietary hosted workflows
