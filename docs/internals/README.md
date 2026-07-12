# Internals

Technical documentation for contributors to iroh-http-core.

| Document | What it covers |
|----------|----------------|
| [http-engine.md](http-engine.md) | hyper/tower integration, request lifecycle, body channel bridge, duplex |
| [resource-handles.md](resource-handles.md) | u64 slotmap handle system, registries, lifecycle, stale handle safety |
| [connection-pool.md](connection-pool.md) | moka-backed pool, single-flight, stale connection handling, ALPN segregation |
| [wire-format.md](wire-format.md) | Wire encoding, ALPN versioning, duplex handshake |
| [dns-sd-device-verification.md](dns-sd-device-verification.md) | On-device iOS/Android DNS-SD verification runbook + results matrix ([#334](https://github.com/Momics/iroh-http/issues/334)) |
| [core-assessment-2026-04.md](core-assessment-2026-04.md) | Per-file keep/replace/reshape verdicts driving the rework epic ([#156](https://github.com/Momics/iroh-http/issues/156)) |

Start with [../architecture.md](../architecture.md) for the component overview before diving into these.
