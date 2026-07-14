# iOS discovery contract checks

Run the host-side peer TXT policy contract with:

```sh
packages/iroh-http-tauri/ios/ContractTests/run.sh
```

The harness compiles the same pure Swift helper used by the native plugin. It
locks down the 247-byte `address` value ceiling, whole-member selection, input
ordering, and the rule that a skipped long member must not hide a later shorter
one.

It also runs a deterministic executable specification for the generic DNS-SD
lifecycle. That contract covers readiness-gated start acknowledgement, one-shot
start failure, found/change/lost records, one-shot terminal polling, stale
callback generations, explicit stop acknowledgement, and publish/stop races.
The lifecycle reducer is currently test-only because the production state is
private and coupled directly to `NWBrowser`, `NetService`, and Tauri `Invoke`.
During the DNS-SD consolidation it should be replaced by the extracted
production reducer while retaining the contract cases unchanged.

The repository's `scripts/ci-ios-swift-build.sh` separately compiles the full
plugin for the iOS 14 simulator SDK. Neither host check can prove Bonjour
registration, Local Network permission behavior, interface changes, or stale
callback suppression on a physical device; those remain device-integration
gates.
