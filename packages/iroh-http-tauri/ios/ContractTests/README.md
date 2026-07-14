# iOS discovery contract checks

Run the host-side peer TXT policy contract with:

```sh
packages/iroh-http-tauri/ios/ContractTests/run.sh
```

The harness compiles the same pure Swift helper used by the native plugin. It
locks down the 247-byte `address` value ceiling, whole-member selection, input
ordering, and the rule that a skipped long member must not hide a later shorter
one.

The repository's `scripts/ci-ios-swift-build.sh` separately compiles the full
plugin for the iOS 14 simulator SDK. Neither host check can prove Bonjour
registration, Local Network permission behavior, interface changes, or stale
callback suppression on a physical device; those remain device-integration
gates.
