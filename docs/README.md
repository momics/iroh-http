# Documentation

## Start Here

- [API overview](api-overview.md): full API reference for fetch, serve, sessions, crypto, mDNS (all runtimes)
- [Architecture](architecture.md): layer diagram, components, scope boundaries, concurrency model
- [Principles](principles.md): engineering values, design philosophy, self-evaluation checklist
- [Protocol](protocol.md): wire format, `httpi://` URL scheme, ALPN versioning
- [Threat model](threat-model.md): what the transport guarantees, what it does not, and required operator mitigations

## Specification

- [Adapter specification](specification.md): normative interface contract for all adapters (core types, error hierarchy, handle lifecycle, feature interfaces, conformance)

## Coding Guidelines

- [Rust](guidelines/rust.md): naming, visibility, error handling, async, testing for `iroh-http-core`
- [JavaScript / TypeScript](guidelines/javascript.md): types, errors, streaming, serve/fetch for adapters
- [Tauri](guidelines/tauri.md): invoke commands, channels for `iroh-http-tauri`
- [Mobile mDNS / DNS-SD setup](guidelines/mobile-mdns-setup.md): iOS `Info.plist` and Android manifest entries for local-network discovery

## Features

Individual feature specs. See [features/README.md](features/README.md) for the full list.

Compression · Discovery · Observability · Rate limiting · Server limits ·
Sign/verify · Streaming · Tickets · WebTransport

## Internals

Contributor-level deep dives. Start with [architecture.md](architecture.md) first.
See [internals/README.md](internals/README.md) for the full list.

HTTP engine · Resource handles · Connection pool · Wire format ·
[DNS-SD device verification](internals/dns-sd-device-verification.md)

## Recipes

Practical patterns built on iroh-http primitives. See [recipes/README.md](recipes/README.md)
for the full categorised list (28 patterns).

Local-first sync · Device handoff · Sealed messages · Capability tokens ·
Group messaging · Offline-first · and more.

## Reference

- [Architecture Decision Records](adr/): design decisions, resolved questions, and open investigations
- [Build & test](build-and-test.md): commands, CI gates, E2E test setup
- [Troubleshooting](troubleshooting.md): common errors, handle lifecycle diagnostics, mDNS issues, error code reference
- [Performance tuning](tuning.md): per-scenario `NodeOptions` guidance, memory sizing, compression settings
- [Design decisions](internals/design-decisions.md): rationale for hyper, slotmap, moka, compression, wire format
- [Roadmap](roadmap.md): v1.0 release checklist, embedded and HTTP/3 horizons

