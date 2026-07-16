# Android native discovery contract tests

This host-only harness compiles the production `IrohHttpPlugin.kt` against
minimal Android/Tauri stubs, then drives deterministic `NsdManager` callbacks.
It covers:

- browse readiness and one-shot start failure;
- terminal-state poll consumption;
- peer and generic found → lost → late-resolve suppression;
- same-node, different-instance source identity and stale-handle isolation;
- retirement- and timeout-aware resolve queues that use fresh native clients
  when Android omits terminal callbacks, then fail explicitly after the bounded
  recovery budget is exhausted (an app restart resets that lifetime budget);
- plural peer address TXT validation and SRV/relay fallback;
- the exact 247-byte address TXT boundary and stable subset fitting;
- registration acknowledgement, API-21 and current-AOSP terminal listener
  ordering, autonomous delayed browse/unregister retries after repeated dispatch
  failures, generic advertisement updates, and callback/threaded update/stop
  races;
- generic explicit-address rejection.

Run it from the repository root:

```sh
packages/iroh-http-tauri/android-contract-tests/run.sh
```

The fake deliberately keeps listeners mapped until after terminal callbacks in
its API-21 mode, removes them before callbacks in its current-Android mode, and
does not remove a registration listener merely because `unregisterService()` was
called. Those transitions mirror the corresponding AOSP implementations; tests
should use the fake's terminal-callback helpers for each legitimate terminal
transition; direct terminal calls are reserved for deliberately stale or
duplicated callback probes.

This complements, rather than replaces, the Android Gradle compile and physical
device tests. In particular, real `NsdManager` multicast visibility, callback
threading and timing, recovery of the native resolver after a missing callback,
and OEM-specific registration failures still require an API 21+ device or
emulator on a multicast-capable network.
