# Local Install

Use this repo as the Munin CLI source of truth.

Current testing build: `v0.5.1`.

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

End users should treat `munin` as the only supported runtime-context CLI.
Context may still exist as an internal local developer tool during migration,
but it is not part of the supported Munin install path.

See also:

- [Runtime Context Ownership](docs/architecture/runtime-context-ownership.md)
- [Context Strangler Migration](docs/architecture/context-strangler-migration.md)
