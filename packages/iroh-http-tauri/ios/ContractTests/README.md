# iOS discovery contract checks

Run the host-side generic DNS-SD lifecycle contract with:

```sh
packages/iroh-http-tauri/ios/ContractTests/run.sh
```

The harness compiles the production generic DNS-SD lifecycle reducer. It covers
readiness-gated start acknowledgement, one-shot start failure, found/change/lost
records, one-shot terminal polling, stale callback generations, explicit stop
acknowledgement, publish/stop races, and peer-shaped `pk`/`relay`/`address` TXT
records flowing through the generic record machinery without native
peer-specific parsing.

The canonical Rust discovery tests own peer TXT fitting and interpretation,
including the 255-byte entry limit, whole-member selection, malformed-address
isolation, relay handling, and instance-name fallback.

The repository's `scripts/ci-ios-swift-build.sh` separately compiles the full
plugin for the iOS 14 simulator SDK. Neither host check can prove Bonjour
registration, Local Network permission behavior, interface changes, or stale
callback suppression on a physical device; those remain device-integration
gates.
