# Local Install

Use this repo as the Munin CLI source of truth.

Current testing build: `v0.5.0-beta.1`.

## Build

```powershell
cargo build
cargo test
```

## Install

```powershell
cargo install --path . --force
```

## Verify

```powershell
munin --version
munin resolve "what keeps going wrong?"
munin hygiene --format text
munin install --check-resolvable
```

`context` remains the shell wrapper. This repo owns the `munin` memory CLI and
the memory-specific read, proof, recall, hygiene, strategy, and install surfaces.
