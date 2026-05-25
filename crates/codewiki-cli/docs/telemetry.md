# Telemetry Contract

## Summary

CodeWiki v1 ships with **zero telemetry by default**. No data is collected,
transmitted, or processed unless both conditions are true:

1. The binary is compiled with the `telemetry` Cargo feature enabled.
2. The operator explicitly sets `enabled = true` in `.codewiki/telemetry.toml`.

## Opt-in Mechanism

Telemetry state is stored in `.codewiki/telemetry.toml` at the project root.
The default file (created automatically on first `codewiki init`.) contains:

```toml
# CodeWiki telemetry configuration.
# Telemetry is disabled by default. Set enabled = true to opt in.
enabled = false
```

## v1 Behavior

- `codewiki install` does NOT prompt for telemetry consent.
- There is no consent dialog, banner, or first-run prompt related to telemetry.
- The `telemetry` Cargo feature is compile-in only; it is OFF in the official
  release binaries unless explicitly enabled by an operator building from source.

## What Would Be Collected (if enabled in a future release)

This section is intentionally left empty for v1. If telemetry is added in a
future release, this document will be updated to list:

- Event types collected
- Data retention policy
- Opt-out mechanism
- Privacy contact

## Feature Flag

The `telemetry` Cargo feature gates all telemetry code:

```
cargo build --features telemetry   # compile-in (operator builds only)
cargo build                        # telemetry code absent (default)
```

Even when compiled in, no data is sent unless `enabled = true` in
`.codewiki/telemetry.toml`.
