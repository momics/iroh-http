## [0.6.1] - 2026-07-21

### 🐛 Bug Fixes

- *(android)* Manage legacy DNS-SD multicast lifecycle
- *(release)* Publish curated changelog notes
- *(android)* Fail closed without legacy multicast service
- *(core)* Disable QUIC GSO on Android
- *(release)* Preserve dependency graph during version bump

### 💼 Other

- *(deps)* Bump js-yaml to 4.3.0

### 📚 Documentation

- Align skill review and closure language
- Complete next-release readiness audit
- Format mobile setup guides
- Make mobile release gate executable
- *(core)* Link Android GSO removal tracker

### ⚙️ Miscellaneous Tasks

- Consolidate project agent skills
## [0.6.0] - 2026-07-18

### 🚀 Features

- *(ci)* Add workflow_dispatch tag input to re-benchmark a release (#253)
- *(tauri)* Make httpi:// scheme response cap configurable
- *(example)* Stream peer-hosted audio and images over httpi:// (#265)
- *(core)* Route self-requests via in-process loopback (#267)
- *(shared)* Add ServeHandle.close() and Symbol.asyncDispose
- *(adapter)* Unify fetch header/numeric validation across Node/Deno/Tauri
- *(adapter)* Unify fetch input validation across adapters
- *(adapter)* Unify serve/endpoint numeric caps across adapters
- *(adapter)* Unify compressionLevel validation + fix drainTimeout doc
- *(tauri)* Mobile AddressLookup bridge for iroh peer auto-dial (#310)
- *(discovery)* Replace swarm-discovery with standard DNS-SD (mdns-sd)
- *(discovery)* Add generic dnsSd advertise/browse surface
- *(discovery)* Add dnsSd examples, re-exports, and interop docs
- *(tauri)* Implement generic DNS-SD on mobile
- *(discovery)* Exclude self from browsePeers()
- *(example)* Add live discovered-peer list with copy button
- *(test)* Add cross-device interop Test tab (#340)
- *(core)* Split transport liveness from handle existence for #336
- *(tauri)* Clear stale mobile discovery on endpoint recovery (#336)
- *(core)* Add dialable_direct_address to IrohEndpoint
- *(adapters)* Expose discoveryInfo across shared, node, deno, tauri
- *(tauri)* Re-export TXT_KEY_ADDRESS and isDialableSocketAddr from guest-js
- *(examples/tauri)* Direct-dial testing mode, peer registry/picker, suite-runner UI
- *(discovery)* Advertise all routable direct addresses
- *(interop)* Add iroh-http result collector + harness submit
- *(interop)* Prefill collector id from VITE_IROH_COLLECTOR_ID

### 🐛 Bug Fixes

- *(ci)* Create local bench-results branch from FETCH_HEAD in bench archival
- Guard u64 handle precision loss at Deno rawFetch and Tauri FFI boundary (#254)
- *(tauri)* Drain pending handler promises on serve shutdown (#256)
- *(deno)* Drain serve teardown ops so leak sanitizer stays green (#257)
- *(shared,core)* Strip URL fragments and abort request stream on body-pipe failure
- *(core)* Log request path without query string
- *(core)* Enforce negotiated ALPN and bound request-head reads
- *(deno)* Verify native binary integrity before dlopen
- *(ci)* Pin publish to the artifact commit and verify on manual dispatch
- *(tauri)* Cap buffered httpi:// scheme handler response size
- *(shared)* Only skip finishBody on error for request uploads
- *(tauri)* Guard datagram base64 encode against oversize spread
- *(shared)* Redact SecretKey.toString() and add explicit secret-export methods
- *(core)* Surface stream error instead of clean EOF on over-limit bodies
- *(core)* Unsubscribe path-change watcher when iteration stops
- *(tauri)* Stop returning raw secret key from create_endpoint (#275)
- *(core)* Store local_service as a Weak to break the endpoint cycle (#282)
- *(core)* Distinguish body stream errors from BodyTooLarge
- *(shared)* Refine createNode secretKey typing and validate chunk size
- *(core)* Preserve BodyTooLarge across drain-time stream errors
- *(core)* Reuse watcher on repeated subscribe_path_changes (#298)
- *(tauri)* Add path-change subscription parity (#314)
- *(tauri)* Mobile mDNS browse falls back to instance name for node-id (#318)
- *(ci)* Build iroh-http-shared before tauri guest tests in check.sh (#321)
- *(tauri)* Make the plugin build for iOS/Android (#328)
- *(deno)* Emit browse event "type" field so discovered peers aren't mislabeled
- *(examples)* Declare _demo-printer._tcp Bonjour service for tauri dnssd demo
- *(examples)* Grant iroh-http:dns-sd capability in tauri example
- *(discovery)* Fix CI-blocking fmt and real DNS-SD peer bugs
- *(discovery)* Harden instance-label parsing, document iOS ServiceRecord gap
- *(example)* Make tauri example typecheck cleanly
- *(release)* Auto-revert CI artifact churn before version bump
- *(tauri)* Make Android Kotlin plugin compile
- *(tauri)* Make Android peers reachable and buildable
- *(shared)* Deliver serve response body when res.body is falsy (#338)
- *(tauri)* Add iOS get_dns_servers to restore Swift/Kotlin FFI parity
- *(tauri)* Invalidate stale pooled conns and replace stale endpoints
- *(tauri-example)* Whitelist _iroh-http-test._udp for iOS Bonjour
- *(tauri)* Base64 body chunks when the WebView rejects raw binary (#338)
- *(core)* Propagate bound QUIC port into advertised direct addresses (#346)
- *(discovery)* Drop bare port-less discovered addrs; robust iOS advertise (#346)
- *(tauri)* Advertise the real bound QUIC port, not the iOS placeholder (#346)
- *(core)* Close stale connections when serve loop is replaced
- *(test)* Honor skip-annotated compliance cases in mobile Test tab (#334, #340)
- *(tauri)* Chain wait_serve_stop after serve so restarts survive many cycles
- *(core)* Close active connections on graceful serve stop (drain-then-close)
- *(core)* Use saturating_add for DNS nameserver counter
- *(shared)* Preserve relay URLs in asIrohPeer (#350)
- *(discovery)* Advertise a dialable ip:port from the desktop advertiser (#350)
- *(tauri)* Harden Android DNS-SD browse for desktop dialing and rebinds (#350)
- *(tauri)* Serialize iOS browse/advertise id allocation (#350)
- *(core)* Transparently retry a fetch when a reused pooled connection was closed
- *(core)* Close teardown concurrency races in serve lifecycle
- *(core)* Gate retry on idempotency and scope pool eviction to the failed connection
- *(shared)* Reject non-decimal ports in isDialableSocketAddr
- *(discovery)* Skip link-local addresses in address selectors
- *(tauri)* Re-emit peer-browse rebinds under the same node id
- *(tauri)* Close browse/advertise sessions on terminal failure
- *(core)* Close residual drain-truncation + drain-wakeup races, harden pool eviction
- *(examples)* Restore clean Tauri example build and gate it in CI (#350)
- *(addr)* Treat port 1 as a placeholder and pair addresses within one IP family (#350)
- *(android)* Guard DNS-server lookup on API 21–22 (#350)
- *(android)* Use SRV fallback only without address TXT; make DNS-SD signatures injective (#350)
- *(shared)* Validate literal IPv4/IPv6 in isDialableSocketAddr (#350)
- *(tauri)* Abort lifecycle operations on stop and emit orphan/error signals once (#350)
- *(tauri)* Honor reconnect `auto: false` and expose an honest recovery callback
- *(core)* Scope timeout eviction without mutating the first request
- *(core)* Make pool compare-and-evict atomic
- *(core)* Disable HTTP/1 keep-alive to enforce one exchange per bidirectional stream
- *(core)* Scope serve-stop signals to a restart generation
- *(harness)* Make interop assertions reflect actual transport outcomes
- *(harness)* Advertise testing mode only after serve succeeds
- *(core)* [**breaking**] Make `DiscoveryOptions` non-exhaustive; reject all-invalid DNS nameservers
- *(harness)* Distinguish empty peer-picker states
- *(harness)* Target batch runs only at active testing-mode peers
- *(harness)* Guard testing-mode serving and show an honest exposure warning
- *(tauri)* Retire orphaned DNS-SD sessions after endpoint replacement
- *(harness)* Drop testing-mode peer allowlist and stop instance-rename eviction
- *(core)* Strip IPv6 zone suffix from DNS nameservers
- *(tauri)* Close lifecycle ownership races (#363)
- *(core)* Preserve transport request ownership (#362)
- *(discovery)* Preserve scoped address policy (#364)
- *(discovery)* Own native session state (#365, #366)
- *(discovery)* Preserve in-flight native operations
- *(discovery)* Close abandoned failed updates
- *(shared)* Preserve inbound request headers
- *(discovery)* Replace stale address snapshots
- *(transport)* Preserve discovered relay hints
- *(android)* Recover stalled dns-sd resolution
- *(core)* Await response delivery before graceful close
- *(discovery)* Refresh peer records after interface changes
- *(ci)* Satisfy strict arithmetic lint
- *(node)* Close an endpoint safely while serve registration is still in flight (#374)
- *(tauri)* Replay early serve-stop requests after registration completes (#375)
- *(core,adapters)* Give path-change subscriptions token-scoped ownership and safe resubscription (#375)
- *(deno)* Order serve registration, fetch, stop, and endpoint close without leaking operations (#376)
- *(node,tauri)* Treat a removed endpoint as closed when an asynchronous close waiter starts late (#378)

### 💼 Other

- *(node)* Regenerate napi loader from pinned CLI baseline
- *(deps)* Bump crossbeam-epoch to 0.9.20 (RUSTSEC-2026-0204)
- *(release)* Generate CHANGELOG.md during release via git-cliff
- *(example)* Bump example tauri crate to 2.11.5 to match @tauri-apps/api
- Bump yanked spin 0.9.8/0.10.0 to 0.9.9/0.10.1
- *(discovery)* Make engine-only mode dependency-free

### 🚜 Refactor

- *(deno)* Use @denosaurs/plug for native lib download/cache
- *(adapter)* Route undeliverable-request 503s through a shared helper
- *(core)* Centralize fetch cancel/timeout/token cleanup (#281, #283, #284)
- *(shared)* [**breaking**] Expose secretKey only when a key is supplied
- *(core)* [**breaking**] Remove Endpoint::secret_key_bytes raw-key accessor
- *(core)* Extract error/crypto/encoding/addr types out of lib.rs (#198)
- *(core)* [**breaking**] Remove the inert `maxServeErrors` option (#278)
- *(adapter)* Consolidate fetch/serve coercion behind coerce_* seam (#289)
- *(shared)* Unify JS option normalisers into iroh-http-shared (#312)
- *(deno)* Consolidate FFI chunk-read grow/retry into one readChunk (#313)
- *(adapter)* Add RequestTransport seam and deliver_request (#315)
- *(tauri)* Route request delivery through RequestTransport (#315)
- *(node)* Route request delivery through RequestTransport (#315)
- *(deno)* Route request delivery through RequestTransport (#315)
- *(tauri)* Inherit iroh via workspace dependency (#310)
- *(discovery)* [**breaking**] Flatten DNS-SD API onto node.advertise/browse
- *(discovery)* Replace internal DnsSd class with stateless functions
- *(tauri)* [**breaking**] Unify mdns + dns-sd permissions into iroh-http:discovery
- *(discovery)* Rename core seam to advertise_peer/browse_peers
- *(discovery)* [**breaking**] Name FFI/commands/permissions by peer-vs-generic axis
- *(discovery)* [**breaking**] Rename mobile native methods to peer-vs-generic axis
- *(example)* Align Tauri discovery UI names with peer-vs-generic axis
- *(discovery)* Add platform-neutral engine seam
- *(discovery)* Make engine types canonical
- *(discovery)* Add non-cancelling runtime sessions
- *(discovery)* Route desktop browse through engine
- *(discovery)* Canonicalize advertisement updates
- *(discovery)* Route desktop advertise through engine
- *(discovery)* Share canonical engine with mobile
- *(discovery)* Route mobile browse through engine
- *(discovery)* Route mobile advertise through engine
- *(discovery)* Consolidate mobile DNS-SD engine
- *(core)* Isolate scoped DNS compatibility shim

### 📚 Documentation

- *(adr-012)* Record bench-results branch, drop GitHub Pages references (#263)
- Correct maxRequestBodyBytes default and clarify wire vs decoded cap
- *(tauri)* Document native-held key and crypto permission on createNode
- Use real request-body limit option names
- Fix contradictory maxConcurrency default (64 → 1024)
- *(adr)* Accept ADR-009 and document RequestTransport seam (#315)
- *(adr)* Record the initial mDNS scope decision, superseded by the generic DNS-SD architecture
- *(discovery)* Add mobile mDNS/DNS-SD setup guide
- *(discovery)* Record DNS-SD harmonization and mobile parity
- *(discovery)* Align ADRs and mobile comments with peer-vs-generic names
- Clarify mobile discovery CI coverage vs on-device
- Rewrite the DNS-SD runbook for the Suite-runner UI and result schema
- *(shared)* Document discoveryInfo + make adapter exports symmetric
- Record PR 350 remediation audit
- *(discovery)* Define consolidation behavior contract
- *(discovery)* Define non-cancelling adapter contract

### ⚡ Performance

- *(deno)* Parse rawFetch payload once on the warm fetch path (#286)
- *(shared)* Honor configured maxChunkSizeBytes in pipeToWriter (#287)
- *(shared)* Harden Node Buffer.from base64 fast path (#280)

### 🎨 Styling

- Apply deno fmt to tauri README and example main.ts
- *(discovery)* Apply rustfmt to new DNS-SD sources
- *(example)* Deno fmt tauri example main.ts
- Apply deno fmt to interop harness and tauri guest-js
- Apply cargo fmt to serve_restart and lifecycle_recovery tests
- Normalize rustfmt on merged client and serve_restart tests
- *(core)* Apply rustfmt to client/serve_restart/lifecycle_recovery
- Deno fmt harness + discoveryInfo adapter imports
- *(tauri)* Apply deno fmt to discovery.test.ts import
- *(example)* Format interop imports

### 🧪 Testing

- *(core)* Add resilience tests — backpressure, leak, disconnect, accept flood (#258)
- *(tauri)* Rename inner test module to fix clippy::module_inception
- *(core)* Add coverage for untested FFI bridge modules (#261)
- *(deno)* Type withTimeout handle as ReturnType<typeof setTimeout>
- *(core)* Add regression for path-watcher leak/over-count (#298)
- *(deno)* Add regression for undeliverable response body hang (#315)
- *(tauri)* Add FFI string-parity contract test for mobile discovery
- *(tauri)* Add on-device DNS-SD verification runbook and Test-tab checks
- *(tauri)* Add regression for Android serve empty body (#338)
- *(core)* Recreate-same-key reachability through stale pool (#336)
- *(tauri)* Reconnect policy restarts serve cleanly on health failure (#336)
- *(tauri)* Add regression for Android body encoding over IPC (#338)
- *(core)* Add regression for port-less iOS direct address (#346)
- *(shared)* Add regression for bare port-less discovered address (#346)
- *(tauri)* Add regression for iOS placeholder advertise port (#346)
- *(shared)* Add regression for asIrohPeer dropping relay URLs (#350)
- *(interop)* Add structured reusable interop suite + headless node runner
- *(harness)* Wire the interop suite into CI and add a headless Deno runner
- *(ios)* Characterize DNS-SD lifecycle
- *(interop)* Isolate release transport gates
- *(shared)* Make timer type portable
- *(interop)* Isolate direct transport probes
- *(core)* Make scoped DNS proxy portable on Windows
- *(tauri)* Gate iOS DNS-SD bound-port advertising

### ⚙️ Miscellaneous Tasks

- *(core)* Use is_some() in channel bench drain loop (#259)
- *(examples)* Add peer allowlist and harden tauri example
- *(node)* Regenerate index.js bindings to match package version 0.5.2
- *(tauri)* Run compliance suites via WebDriver on Linux CI (#262)
- Exclude bench crates from version-bump policy
- *(node)* Restore napi loader boilerplate from main
- Add non-blocking cargo-llvm-cov coverage job (#206)
- Decouple publish from build (manual tag-promoted release) (#331)
- Harden tag-to-Build-run resolution in publish (#331)
- *(examples)* Pin deno example to 0.6.0 and refresh lockfiles
- *(discovery)* Prune stale deny.toml skips, cargo fmt tauri, add generic surface tests
- *(tauri)* Gitignore android .gradle build dir
- *(release)* Gate ci-churn regression test in CI
- *(tauri)* Compile mobile plugin native sources on PRs
- *(example)* Tracing to logcat and keyboard-aware viewport
- *(base)* Merge Android reachability/build + logcat tracing enablement (#338, #340)
- *(tauri)* Rustfmt mobile-gated advertise code (#350)
- *(example)* Refresh mobile lockfile
- *(security)* Remove resolved quick-xml advisory ignores
- *(release)* Harden tag promotion workflow
## [0.5.2] - 2026-06-26

### 🚀 Features

- *(examples)* Add Deno mDNS advertise/browse example

### 🐛 Bug Fixes

- *(examples)* Resolve @momics/iroh-http-shared/adapter in Tauri Vite config
- *(tauri)* Declare permission sets with [[set]] not [[permission]] (#246)
- *(tauri)* Make mdns_advertise async so it runs on the Tokio runtime (#247)
- *(tauri)* Register session_accept, try_next_chunk, start_transport_events in ACL (#248)
- *(examples)* Correct Tauri mDNS advertise/browse usage

### 💼 Other

- *(examples)* Update to iroh-http 0.5.1 and refresh lockfiles
- *(ci)* Adopt deno fmt and enforce formatting in CI (#249)

### 📚 Documentation

- *(shared)* Document node.browse and node.advertise (#249)
- *(api)* Document public TS API across node/deno/tauri (#249)
- *(tauri)* Fix literal \\n in README; make tests/http-compliance fmt scope explicit (#249)

### 🎨 Styling

- Format TS code with deno fmt (#249)

### 🧪 Testing

- *(tauri)* Add ACL permission-set regression guard (#246)
- *(tauri)* Guard command/permission-set parity (#248)

### ⚙️ Miscellaneous Tasks

- *(tauri)* Regenerate ACL schema after permission-set fix (#246)
- *(tauri)* Regenerate ACL artifacts for new commands (#248)
## [0.5.1] - 2026-06-25

### 🐛 Bug Fixes

- *(release)* Lowercase GitHub org momics in package metadata (#242)
- *(node)* Make mdns_advertise async to stop teardown SIGABRT (#243) (#244)

### ⚙️ Miscellaneous Tasks

- *(publish)* Use npm OIDC Trusted Publishing, regen node index.js to 0.5.0 (#241)
- *(bench)* Archive raw results to bench-results branch; drop gh-pages dashboard (#240)
- Release v0.5.1
## [0.5.0] - 2026-06-25

### 🚀 Features

- *(tauri)* Register httpi:// as custom URI scheme
- *(deps)* [**breaking**] Migrate to iroh 1.0 (#236)

### 🐛 Bug Fixes

- *(ci)* Update napi artifacts flags for @napi-rs/cli v3
- Remove endpoint from registry after close, not before
- *(node)* Add local u32-to-u64 handle indirection to preserve slotmap bits
- *(node)* Suppress mDNS shutdown panic that fails Node test runner
- *(core)* Prevent stop_serve race when abort fires before rawServe resolves
- *(core)* Prevent close/closed race on session handle removal
- *(node)* Tolerate missing endpoint in stop_serve and wait_serve_stop
- *(ci)* Version-bump policy matched context lines in package.json diffs
- *(tauri)* Remove needless Ok wrap in scheme 405 response
- *(ci)* Version-bump policy matched package-lock.json entries
- *(deps)* Clear undici & js-yaml npm audit advisories (#239)

### 📚 Documentation

- Rewrite READMEs for clarity and dedup shared API content

### ⚙️ Miscellaneous Tasks

- Fix Node.js extended tests hanging on open handles
- Regen napi bindings and fix Deno shutdown races (#231)
- Release v0.5.0
## [0.4.0] - 2026-05-02

### 🚀 Features

- [**breaking**] Drop raw_connect from public surface
- *(core)* Decompress inbound request bodies on serve path
- *(api)* [**breaking**] Rename node.connect→dial, node.sessions→incoming, session_*→Session::*
- *(api)* Node.dial() accepts httpi:// URLs (closes #166)
- *(compression)* [**breaking**] Make zstd always-on, drop build-time feature flag
- *(ffi)* Impl Sink<Bytes> for BodyWriter (#174)
- *(ffi)* Pass StackConfig knobs through FFI — fetch timeout/decompress, serve decompress, per-call response limit (#189)
- *(node)* [**breaking**] Migrate iroh-http-node from napi v2 to v3 (#146)

### 🐛 Bug Fixes

- *(core)* Align compression docs and implementation
- *(core)* Replace slab with slotmap in endpoint registry
- *(release)* Regenerate lockfile with all platform optional deps
- *(tauri)* Drop "connect" from build.rs command list
- *(deno)* Close_all signals serve queues so SIGINT exits cleanly
- *(deno)* Widen FFI endpoint handle to u64 to preserve slotmap version bits
- *(compression)* Align min_body_bytes default with docs (1 KiB) (closes #167)
- *(server)* Box ServeService and collapse pipeline assembly into one chain
- *(server)* Add max_request_body_decoded_bytes to guard against zstd bombs (#190)
- *(tauri)* Rename max_request_body_bytes in ServeArgs and ServeOptions mapping (#190 follow-up)
- *(server)* Poll_ready logs+Pending instead of swallowing; add clippy.toml disallowed-types
- *(ffi)* Forward fetch knobs in Node/Deno adapters; add AC regression tests (#189)
- *(ci)* Narrow version-bump-policy to actual version-field diffs (#196)
- *(api)* Tidy stale references after #194 rename
- *(shared)* Export FetchOptions from adapter barrel
- *(deny)* Ignore hickory CVEs blocked on iroh upstream
- *(audit)* Ignore hickory CVEs in cargo-audit config
- *(bench)* Replace serve() with ffi_serve() in bench fixtures
- *(shared)* Export FetchOptions from public index barrel
- *(node)* Make start_transport_events async to satisfy tokio runtime
- *(node)* Make raw_serve async to run inside tokio runtime
- *(bench)* Clean up formatting in QUIC stream and HTTP benchmarks
- *(core)* Resolve P3 issues #199, #200, #201, #202, #203
- *(build)* Auto-repair bare lockfile stubs in check-lockfile.mjs

### 🚜 Refactor

- *(core)* Introduce unified Body newtype (slice 1 of #159)
- *(core)* Make RequestService infallible (slice 2 of #159)
- *(core)* Collapse serve-loop wiring with option_layer (slice 3 of #159)
- *(core)* Split RequestService into FfiDispatcher + IrohHttpService (slice 4 of #159)
- *(core)* Rename TowerErrorHandler → HandleLayerError + ADR-013 doc (slice 6 of #159)
- *(core)* Use tower_http::RequestBodyLimitLayer for request-body size enforcement
- *(core)* Unify response body via MapResponseBodyLayer + Body::new
- *(core)* Extract config and stats types out of endpoint.rs
- *(server)* Use tower-http NotForContentType for compression skip rules (closes #172)
- *(server)* Extract per-bistream pipeline assembly into server_pipeline (closes #169)
- *(stream)* Impl http_body::Body for BodyReader directly (refs #170)
- *(endpoint)* Split EndpointInner into named subsystems (closes #171)
- *(server)* IrohHttpService.remote_node_id is Arc<String>, not Option<String>
- *(server_pipeline)* Extract build_stack so the guardrail tests the real chain
- *(core)* [**breaking**] Slice A — introduce mod http / mod ffi split + architecture test
- *(core)* Slice B — typed StackConfig + shared build_stack/build_client_stack (#184)
- *(core)* Slice C.0 — boxed-per-layer stack composition (#185)
- *(core)* Slice C.1+C.3 — pure-Rust serve(svc) + RemoteNodeId extension (#185)
- *(core)* Move FfiDispatcher into mod ffi (Slice C.2 of #185)
- *(core)* Delete dead duplex CONNECT/Upgrade branch (Slice C.5, closes #180)
- *(core)* Typed ConnectionTracker / RequestTracker (Slice C.4, closes #178)
- *(core)* Collapse src/ to endpoint.rs + http/ + ffi/ (#185)
- *(server)* Split http/server/mod.rs to ≤200 LoC (Slice C.7 of #185)
- *(core)* Pure-Rust http::fetch(addr, req, &cfg) -> Result<Response<Body>, FetchError> (Slice D, closes #186)
- *(client)* Finish Slice D acceptance — pumps to ffi, drop hyper string match, wire timeout/decompress through ffi::fetch (#186 review)
- *(ffi)* Slice E — move session under mod ffi, lock canonical layout (closes #187)
- *(api)* Rename serve_service → serve; demote FFI serve to ffi_serve (#194)
- *(endpoint)* Split endpoint.rs into endpoint/ subsystem modules (ADR-014 D1, #192)
- *(endpoint)* Extract lifecycle.rs; update ADR-014 status note (#192 follow-up)

### 📚 Documentation

- *(spec)* Add missing public types and Rust API coverage
- *(adr)* Draft ADR-014 runtime architecture
- *(adr)* Add ADR-013 lean on the ecosystem
- *(internals)* Add core-assessment-2026-04
- *(core)* Warn FFI adapters not to truncate slotmap handles to u32
- *(compression)* Document zstd-only policy and runtime support (closes #173)
- *(lib,session)* Regroup Session re-export under FFI banner; doc rationale (#187 review MAINT-1, INFO-1, TEST-2)
- *(adr-014)* Record D4 perf-vs-elegance split after Slice E
- *(adr-014,ffi)* Record D4 resolution; backlink pumps to #174
- *(adr-014,epic-182)* Recalibrate D1 LoC targets; record subsystem split gap (#195)
- *(guidelines)* Document FFI payload field naming convention; add handle doc comments (#143)

### ⚡ Performance

- *(build)* Add .cargo/config.toml, prefer cargo nextest, document linker and sccache (closes #168)
- *(node)* Use zero-copy buffer transfers in napi bridge

### 🎨 Styling

- *(core)* Rustfmt fixes after Slice C.2/C.5
- Move allow attr out of doc-comment block; drop trailing blank lines
- Rustfmt + napi v3 generated bindings

### 🧪 Testing

- *(core)* Regression test for #161 u32-handle truncation
- *(server_pipeline)* Adding a no-op layer is one append (closes #175)
- *(core)* Add layered channel and QUIC micro-benchmarks
- *(node)* Add FFI bridge isolation benchmark

### ⚙️ Miscellaneous Tasks

- Gate publish on extended tests passing
- *(core)* Cargo fmt
- Remove reviews/ — findings filed as issues #190-#195
- *(docs)* Remove stale Slice E comment, fix fetch intra-doc links, delete dead apply_handle_layer_error
- *(release)* Gate version bumps to chore-release commits; guard version.sh against dirty trees (#196)
- Update deno.lock
- Release v0.4.0

### ◀️ Revert

- *(release)* Roll workspace back to 0.3.4 to unbreak CI
## [0.3.4] - 2026-04-29

### 🚀 Features

- *(shared)* [**breaking**] Remove legacy two-argument fetch(peer, path) overload
- *(core)* Validate ALPN capabilities at bind() time
- *(core)* Expose BodyReader cancellation contract

### 🐛 Bug Fixes

- *(bench)* Exclude close() from cold-connect measurement
- *(tauri)* Wrap serve params in ServeArgs struct
- Update fetch URLs to use dynamic serverId for bench tests
- Update fetch URLs to use dynamic serverId in benchmark scripts
- *(ci)* Align local and github checks
- *(deno)* Re-add ignore flag to regression #115 test
- *(deno)* Cancellable yield for serve loop shutdown (#115)
- *(deno)* Keep serve registry entry alive until close_endpoint (#149)
- *(deno)* Remove x-503-source debug header from production responses

### 💼 Other

- Upgrade iroh to 0.98.1
- *(deps)* Bump napi-build from 1.2.1 to 2.3.1
- *(deps)* Bump criterion from 0.5.1 to 0.8.2
- *(deps)* Bump rand from 0.9.4 to 0.10.1

### 🚜 Refactor

- *(core)* [**breaking**] Mechanical cleanup — units, naming, type consistency
- [**breaking**] Move serve config from NodeOptions to ServeOptions
- Restructure JsNodeOptions and introduce JsServeOptions for improved server configuration

### 📚 Documentation

- Promote httpi:// URL form as primary fetch() input
- Correct ServeOptions default values

### ⚡ Performance

- Redesign benchmark suite with 9 consistent scenarios
- *(deno)* Reduce FFI overhead from ~40x to ~1.2x native (#126)
- *(node,tauri)* Add try_next_chunk sync fast path for body streaming
- *(tauri)* Use binary IPC for body chunks instead of base64
- *(shared)* Profile and optimize shared TypeScript fetch layer
- *(deno)* Benchmark JSON vs MessagePack for FFI boundary

### 🧪 Testing

- *(tauri)* Add Rust and guest-JS test coverage
- Assert response status in 32-stream regression test (#149)
- *(deno)* Separate concerns in regression tests and add #149 coverage
- Modularize and unify test suite naming and structure

### ⚙️ Miscellaneous Tasks

- Restrict extended tests to tags and manual dispatch
- *(bench)* Fix benchmark workflow — rename groups and create gh-pages
- *(bench)* Fix gh-pages push — add auto-push and skip-fetch
- *(bench)* Split release/main data paths and disable fail-on-alert
- Release v0.3.4
## [0.3.3] - 2026-04-25

### 🐛 Bug Fixes

- *(build)* Regenerate Tauri Cargo.lock during version bump
- *(ci)* Revert upload-artifact to v7 (v8 does not exist)
- *(ci)* Match cargo "already exists" in publish idempotency check

### ⚙️ Miscellaneous Tasks

- Release v0.3.3
- Skip verify for tauri-plugin-iroh-http publish
- Harden release pipeline
## [0.3.2] - 2026-04-25

### 🐛 Bug Fixes

- *(build)* Add version to all internal path deps for cargo publish

### ⚙️ Miscellaneous Tasks

- Release v0.3.2
## [0.3.1] - 2026-04-25

### 🐛 Bug Fixes

- *(deno)* Signal shutdown and drain queue before removing serve registry
- *(shared)* Await body pipe before respond on handler failure
- *(build)* Version script now updates all iroh-http dep versions in Tauri Cargo.toml
- *(build)* Regenerate lockfile to fix npm ci crash on versionless optional deps

### 📚 Documentation

- Rename explorations/ to adr/ and add ADRs 009-012
- *(tauri)* Update Cargo dependency version to 0.3 in README

### ⚙️ Miscellaneous Tasks

- *(cargo)* Migrate workspace members to version.workspace inheritance
- *(scripts)* Replace release/ folder with interactive release.sh
- Fail publish on real errors, only skip on already-published
- Release v0.3.1
## [0.3.0] - 2026-04-25

### 🚀 Features

- *(shared)* [**breaking**] Restructure NodeOptions and simplify ServeOptions (#108 #109 #111 #112)
- *(shared)* [**breaking**] EventSource abstraction — peerconnect/peerdisconnect/pathchange/diagnostics events (#116)
- *(shared)* [**breaking**] Remove duplex upgrade; expose node.sessions() (#117)
- *(node)* Add sessionAccept napi binding + node.sessions() e2e test
- *(tauri)* Add session_accept command + wire node.sessions()
- *(shared)* Add PublicKey.fromPeerId() and PublicKey.toURL()

### 🐛 Bug Fixes

- Update package dependencies to use versioned imports for examples
- Add version requirements to path deps in adapter and tauri crates
- *(shared)* Guard against double serve() on the same node (#114)
- *(shared)* Await loopDone before resolving finished (#115)
- *(dependencies)* Add @momics/iroh-http-deno@~0.2.1 to examples/deno dependencies
- *(shared)* Preserve RelayMode literal autocomplete via (string & {}) (#110)
- *(deno,node,shared)* Resolve #119 stale-handle race; document #122 deno-specific timeout
- *(deno)* Sync-poll nextRequest to avoid spawn_blocking deadlock (#122)

### 🚜 Refactor

- *(tauri)* Rename commands and restructure permissions into named sets
- *(shared)* Split keys.ts into PublicKey, SecretKey, base32 modules

### 📚 Documentation

- Enhance README with clearer explanation of iroh-http functionality
- Fix tauri package README and Cargo metadata
- Update observability and roadmap documentation for clarity and accuracy
- Add regression-first skill
- *(shared)* Add JSDoc to NodeOptions, RelayMode, and IrohFetchInit (#110)
- Update README files for clarity and consistency across packages

### 🧪 Testing

- *(core)* Fix response_header_bomb_rejected missing bind_addrs
- *(deno)* Add regression test for serve loop pending ops bug (#115)
- *(deno)* Add regression tests for double-serve guard (#114)
- *(node)* [**breaking**] Update e2e transport tests to diagnostics API
- Expand test suite with lifecycle, error handling, and stress tests (#121)

### ⚙️ Miscellaneous Tasks

- Upgrade actions to latest majors, slim verify, add publish target selector
- Pre-push cleanup — remove rawConnect dead code
- Update .gitignore to include .agents and skills-lock.json; modify VSCode settings for agent skills locations
- Release v0.3.0
## [0.2.1] - 2026-04-22

### 🐛 Bug Fixes

- Correct release process, remove Zig dep, fix bench registry leak
- Regenerate deno.lock for v0.2.0; version.sh now updates it too
- Version.sh trailing message points to release:tag instead of manual commit
- *(node)* Correct napi config field names (name/triples) and regenerate index.js
- *(ci)* Install Tauri GTK system deps in verify-release job
- *(release)* Add NPM_TOKEN for first-publish and publish shared to JSR
- *(release)* Use subshell for tsc so CWD stays at repo root
- Regenerate lockfile with root version field (npm 11 / Node 24)
- Resolve npm Invalid Version by omitting optional platform packages
- Add --omit=optional to remaining npm ci calls (bench, publish)

### ⚙️ Miscellaneous Tasks

- Update Cargo.lock for v0.2.0
- Release v0.2.1
- *(ci)* Disable tag-triggered bench workflow (hangs)
- Use OIDC Trusted Publisher for npm, bump Node to 24, fix crates idempotency
- *(release)* Add job timeouts and fix apt-get hanging on aarch64 cross-compile
- Split release (build) and publish into separate workflows
- Rename release.yml → build.yml, update workflow_run reference
- Fix build.yml name field to Build
## [0.2.0] - 2026-04-22

### 🚀 Features

- Add iroh-http shared library and Tauri plugin for peer-to-peer HTTP
- Enhance FfiResponse with URL and update related components for improved response handling
- *(permissions)* Add serve command permissions and schema
- Improve request handling and error management in fetch and serve functions
- Align iroh-http API with WebTransport spec
- Integrate structured error handling and key management
- Implement fetch cancellation support
- Add structured Rust errors to iroh-http-core
- Add performance review for IPC and streaming patterns with findings and recommendations
- Implement QPACK header compression for improved efficiency
- Initialize Tauri example with TypeScript, Vite, and Rust integration
- Add directAddrs, relayMode, disable_networking + comprehensive tests
- Connection pool with stream multiplexing (patch 12)
- QPACK header compression (patch 13)
- P2P security hardening (patch 14)
- Graceful shutdown (patch 15)
- Add platform guidelines for JavaScript, Python, Rust, and Tauri
- Add default headers support for iroh-http
- Add bidirectional streams support in IrohSession
- Add server limits configuration options for TypeScript `ServeOptions`
- Add peer exchange recipe for decentralized introductions
- Move bidirectional streams to IrohSession (patch 22)
- WebTransport compatibility — uni streams, datagrams, close info (patch 27)
- Add comprehensive codebase review for patches 12–28 and feature specs
- Add detailed code architecture review for iroh-http-core focusing on composition, build-vs-buy, and Rust quality
- Expose maxHeaderBytes in TypeScript NodeOptions
- Add Python .pyi type stubs for IDE support
- Add __aenter__/__aexit__ to Python IrohNode and IrohSession
- Add Python mDNS browse/advertise support
- Patch 29 — NodeOptions developer experience improvements
- Add examples/deno workspace and dependencies; update iroh-http-shared dependencies
- Validate httpi:// URL scheme in core and all platform bindings
- Implement systemic review framework with detailed streams and templates
- Complete systemic review and evidence documentation for all streams
- Restructure server accept loop using tower::Service
- Document rework phases and architectural changes in README and architecture files; implement generational handle management with slotmap; enhance FFI input validation with structured error handling
- Add implementation notes for critical path patterns and decisions in the rework process
- Update FFI handle type from u32 to u64 across multiple components; enhance documentation for handle management and concurrency controls
- Add architecture and principles documentation, along with initial implementation notes and change logs
- Add API surface parity analysis and repository audit documentation
- Add Deno smoke test failure documentation and review findings
- Add issue documentation for endpoint handle management and cancellation semantics
- Add issue documentation for mobile plugin structure and dependencies
- Add issue tracking for Python and Tauri plugin improvements
- *(docs)* Add issue templates for various inconsistencies and improvements
- Consolidate and refactor endpoint backpressure configuration
- Add release.sh for local cross-platform release pipeline
- Add issue templates for fetch signature, request URL scheme, connection statistics, trailer headers, first request error, and header renaming
- Fetch() accepts httpi:// URL with peer ID in hostname (closes #1)
- Export IrohRequest type with typed trailers property (closes #4)
- Add QUIC connection stats to peerStats() — rttMs, bytesSent, bytesReceived (closes #3)
- Add Windows x86_64 target to node build
- *(release)* Add --only=node|deno|all flag for scoped releases
- *(release)* Modular release pipeline with individual step scripts
- *(core)* Add max_total_connections limit to prevent Sybil exhaustion
- *(core,node,tauri,deno,shared)* Resolve issue #44 — endpoint stats + tracing
- *(serve)* Peer connection lifecycle events (onPeerConnect/onPeerDisconnect)
- *(#52)* Support request trailers on client fetch
- *(#49)* Add benchmark infrastructure
- *(shared)* Enforce incoming peer verification via verifyNodeId (#76)
- *(#49)* Extend benchmark infrastructure — latency benches, CI regression workflow (#75)
- *(node)* Napi-rs platform package split
- *(core)* [**breaking**] Remove trailer header support
- *(streaming)* Add sweep_interval_ms tuning option and sweep_now()
- Add structured evaluation prompt for iroh-http library

### 🐛 Bug Fixes

- Update status header in 02_review.md to reflect integration
- Resolve TS build errors and clean up stale files
- Change URL scheme to httpi://, fix serve bugs
- Complete all P0 and P1 review items
- Apply custom dns_discovery URL in endpoint bind
- Use @momics/iroh-http-shared import path in Tauri guest-js
- *(pool)* LRU eviction — evict least-recently-used connection
- Update patch status from open to applied
- Resolve all clippy warnings (-D warnings) across workspace
- Base32 padding in shared keys.ts, missing pool_idle_timeout_ms in tauri plugin
- Per-call FFI output buffer to prevent concurrent call data races in deno adapter
- Skip sendTrailers when response has no Trailer header (handle already removed by server)
- Use ticket() in Deno tests to connect via relay instead of QUIC loopback
- Bind test endpoints to 127.0.0.1 loopback
- Clamp hyper max_buf_size to 8192 minimum, enforce header limit post-parse
- Make pool_different_peers test robust under parallel load
- Resolve all P1/P2 findings from third review
- Address all 4 audit findings
- Update Deno dispatch callers and Tauri BigInt cast for Result API changes
- Convert bigint handles to number for napi-rs compatibility in bridge and session functions
- Add missing types entry in tsconfig for Node.js compatibility
- Enable Deno types for tests/http-compliance in VS Code settings
- Systematic resolution of 61/62 tracked issues
- Reopen multiple issues to address ongoing concerns and updates
- Resolve ISS-027 through ISS-033
- Update status of TAURI-006 to wont-fix and clarify package naming resolution
- Resolve build errors from ISS-027-033 changes
- Silence cfg-gated unused variable warnings; add --allow-sys to deno build
- Resolve TAURI-007–012 and ISS-008 — full mobile layer + error taxonomy
- Add missing AndroidManifest.xml and ios/.gitignore per official Tauri template
- Use publicKey instead of removed nodeId in e2e test
- *(deno)* Resolve serve-loop async op leaks in smoke tests
- *(python)* Remove .venv from project tree; use maturin build + cache venv
- *(B-ISS-040)* Add missing error codes to classifyByCode
- *(PY-010)* Remove unsafe raw pointer in path_changes async iterator
- *(A-ISS-034, A-ISS-035)* Clamp channel_capacity and max_chunk_size to minimum of 1
- *(A-ISS-040)* Warn when second endpoint's backpressure config is ignored
- *(A-ISS-049)* Document serve callback contract in architecture.md
- *(DENO-005)* Resolve endpoint handle to internal endpoint_idx for fetch tokens
- *(DENO-007)* Cache overflow responses to avoid re-dispatching on buffer resize
- *(B-ISS-041)* Remove unimplemented debugHeaders from default-headers doc
- *(B-ISS-042)* Update JS guidelines error table to match errors.ts
- *(NODE-005)* Pin @momics/iroh-http-shared to ^0.1.0
- *(NODE-006)* Wrap rawRespond calls in try/catch to prevent unhandled rejections
- *(PY-011)* Enter Tokio runtime before calling core serve()
- *(PY-012)* Replace Notify with watch<bool> for persistent closed state
- *(PY-013)* Return self from __aenter__ and signal closed state in __aexit__
- *(TAURI-013)* Buffer surplus mobile mDNS events to prevent data loss
- *(TAURI-014)* Pass node identity metadata to mobile mDNS advertise
- *(P2)* Session_ready validation, extract_path query, dns_discovery default, stale QPACK refs, header naming
- *(P2)* Serve error propagation, f64 validation, type stubs, compression level
- *(A-ISS-048)* Change IrohEndpoint::bind() to return Result<_, CoreError>
- *(A-ISS-038)* Document mDNS session lifecycle limitation
- *(P3)* Stale doc references, pool retry docs, compression artifact, README options
- *(A-ISS-041)* Extract shared endpoint registry to iroh-http-core
- *(A-ISS-042)* Deduplicate normaliseRelayMode and base64 helpers to shared
- *(A-ISS-046)* Extract ServerLimits struct from duplicated NodeOptions/ServeOptions fields
- *(B-ISS-044)* Document Python adapter feature gaps and API differences
- *(A-ISS-050)* Update sign-verify documentation to include Python functions and cross-platform encryption/decryption
- *(DENO-008)* Document dispatch.rs structure as platform-necessary
- *(A-ISS-050)* Update sign-verify documentation to remove Python raw functions and clarify encrypt/decrypt usage
- *(NODE-008)* Document platform support matrix and add os/cpu fields
- *(PY-015)* Document adapter thickness, add missing create_node params
- *(A-ISS-050)* Export PublicKey/SecretKey from all adapters, remove raw crypto exports
- *(docs)* Update architecture documentation to reflect changes in error handling functions and connection pool behavior
- *(test)* Capture expected error logs in error classification tests
- Add missing license field in deno.jsonc
- Update dependencies in deno.lock and ensure nodeModulesDir is set to none
- Update version to 0.1.1 and adjust publish include paths in deno.jsonc; implement library caching and download logic in adapter.ts
- Update URL handling in makeServe to preserve original payload URL
- Replace 'invalid endpoint handle' with INVALID_HANDLE code + clearer message (closes #5)
- Update settings for TypeScript SDK path and add Windows support in package-lock
- Resolve 18 open issues (#7–#25)
- *(node)* Thread endpointHandle through bridge, session, and body-handle napi calls
- *(node)* Switch Windows cross-compile to MSVC target with cargo-xwin
- *(build)* Stream build output instead of buffering through tail
- *(deno)* Thread endpointHandle through bridge, nextChunk FFI, and respond calls
- *(release)* Scope shared publish by platform, fix output capture hang
- *(release)* Stream publish output through tee to unblock interactive prompts
- *(release)* Use deno publish instead of npx jsr publish for shared JSR
- *(shared)* Add license field to jsr.jsonc for JSR publish
- *(shared)* Consolidate jsr.jsonc into deno.json, add license field
- *(shared)* Use MIT license for JSR compatibility
- *(release)* Proper 2FA error detection, scope shared publish by platform
- *(core)* Eliminate redundant memcpy in pump_quic_recv_to_body
- *(core)* Widen close_code to u64 and clamp drain semaphore
- *(core)* Add InsertGuard to prevent handle leaks on partial allocation
- *(core)* Strip URI fragment identifiers in extract_path
- *(core)* Return error for out-of-range session close codes
- *(core)* Widen CoreError::invalid_handle parameter from u32 to u64
- *(shared)* Include "Internal Server Error" body in default 500 response
- *(shared)* Prevent AbortSignal listener leak in fetch()
- *(core,deno,shared)* Resolve issues #7, #12, #39, #58
- *(discovery,shared)* Resolve issues #45, #48, #61, #62
- *(core,node,tauri,shared)* Resolve issue #59 — ServeHandle.finished reflects real loop termination
- *(core,node,tauri,deno,shared)* Resolve issue #60 — node.closed on native endpoint shutdown
- *(core,node,adapter)* Correct 8 FFI boundary safety and correctness issues (#68)
- *(node)* Harden FFI boundary — input validation, panic safety, structured errors (#73)
- *(node)* Destructure 3-tuple from BigInt::get_u64() for napi 2.16
- *(stream,core)* Resolve #82, #83, #84 — trailer leak, compression default, stream quality
- *(#79,#80,#46,#81)* Tauri leak, pool docs, CI targets, Tower layers
- *(issue-templates)* Update acceptance criteria descriptions for bug and feature templates
- *(bench)* Uint8Array type cast for Deno strict check, add --skipLibCheck for node tsc
- *(ci)* Check no-default-features with compression kept on — tests discovery-off path only
- *(ci)* Resolve three CI failures on v0.1.4 main push
- Resolve strict lint and adapter type regressions
- Update mdns dispatch functions to use parameterized input and improve error handling
- *(tauri)* Resolve clippy errors blocking CI
- *(bench)* Update mitata v1 API and enter tokio runtime in criterion benches
- *(bench)* Fix bench regressions and add smoke-test gate
- *(core)* Skip body channel allocation for null-body status codes
- *(deno)* Pass endpointHandle in all session FFI calls; reduce concurrent stress test
- *(test)* Add missing routes to cross-runtime Deno server
- *(deno)* Add sanitizeOps: false to SecretKey toBytes round-trip test
- *(shared)* Await nativeClosed in close() to drain waitEndpointClosed FFI op
- *(test)* Strip NODE_OPTIONS for deno processes; grant all perms in deno tasks
- *(deno,core)* Eliminate null-ptr UB and global concurrency bypass
- *(core)* Raise DEFAULT_CONCURRENCY from 64 to 1024
- *(tests)* Parse --pairs flag correctly and guard against zero pairs
- *(core)* Clean up fetch cancel-token on early error; fix flaky pool test
- *(tauri)* Respond 503 on channel send failure in serve callback
- *(deno)* Distinguish stream error from EOF in nextChunk
- *(shared)* Track dist/ in git and add drift-check to check.sh
- *(tauri)* Make bridge, allocBodyWriter, and session fns endpoint-scoped
- *(tauri)* Only close endpoints when the last window is destroyed
- *(client)* Classify hyper header-overflow errors as HeaderTooLarge
- *(client)* Drain body frames after overflow so QUIC stream closes cleanly
- *(ci)* Initialise FAILURES=() to avoid unbound variable under bash set -u
- *(tauri)* Publish pk/relay TXT records in mobile mDNS advertise
- *(tauri)* Publish pk/relay TXT records in mobile mDNS advertise (#88)
- *(core)* Set 16 MiB default for max_request_body_bytes and bound response body to 256 MiB
- *(node,deno,tauri)* Wire maxTotalConnections; add graceful shutdown hooks; harden adapters
- *(ci)* Correct deny.toml schema and remove unresolvable Tauri deny step
- *(deno)* Restore missing `} as const);` closing Deno.dlopen call
- *(deno)* Migrate to IrohAdapter and IrohNode._create
- *(ci)* Bump rustls-webpki to 0.103.13 (RUSTSEC-2026-0104); harden check.sh

### 💼 Other

- Wire up iroh-http in all four examples
- Update status to done for patches 17, 18, and 19
- Strip and inject iroh-node-id in Rust core
- Wire npm run build through scripts/build.sh for full end-to-end build
- Replace monolithic build.sh with per-platform npm scripts
- Node integration tests — 14 tests, fix relays null + PKCS8 keys
- Deno integration tests — 23 tests (was 17)
- Python integration tests — 6 new tests (3 active, 3 skipped)
- CI — add Python job, Node compliance, cross-runtime gate
- Regression test policy — template + copilot instructions
- *(discovery)* Replace n0-future with workspace futures crate
- Add tauri clippy step to check.sh
- Delegate ci steps to npm scripts, remove duplication

### 🚜 Refactor

- Separate public and internal type exports in iroh-http-shared
- Rust quality improvements batch
- Clean up unused code and improve documentation across multiple crates
- Improve developer experience for `NodeOptions` by merging keys and reorganizing structure
- Replace hand-rolled TS base32 with @scure/base; fix Python serve() docstring
- Overhaul architecture and dependencies for hyper integration
- Reorganize endpoint commands for better readability
- Simplify handle conversions in bridge and session functions
- Move build logic into packages
- Replace global static registries with per-endpoint HandleStore
- Tighten compression policy and extract adapter error helpers
- *(core)* Deduplicate box_body, duplex pump, and add extract_path tests
- Remove rate limiting middleware and update related documentation
- *(shared)* Replace buildNode() positional args with config object
- *(core)* Macro-generate handle_to_*_key and enum-based InsertGuard
- *(#42)* Group NodeOptions fields into semantic sub-structs
- *(shared)* [**breaking**] Remove verifyNodeId, use Peer-Id for access control
- *(shared)* Align TypeScript layer with WHATWG class model
- Replace long-polling transport events with push-based delivery

### 📚 Documentation

- Update README files to clarify differences from regular HTTP and enhance usage instructions
- Add build & versioning guide
- Update Python create_node() docstring with all 16 parameters
- Mark all P2 items complete in review files
- Update h3 upgrade path to reflect noq fork
- Reorganise and professionalise documentation
- Add standards compliance section with detailed explanations of compliance and deviations
- Update default permissions documentation and schema descriptions
- Add exploration templates and new exploration documents for peer identity, capability URLs, connectivity, and security models
- *(A-ISS-051)* Create specification.md as normative interface contract
- Rewrite TEST_PLAN.md based on bug profile analysis; create 6 implementation issues
- Update roadmap with current state + open-source checklist
- Unify npm scripts and update README development section
- Enrich copilot-instructions with principles and architecture brief
- *(scripts)* Update prereqs with LLVM, PATH note, and modular publish commands
- Update roadmap with pre-open-source checklist items
- Handle lifecycle (#41), stale connections (#53), troubleshooting and tuning guides (#55)
- *(readmes)* Fix Windows platform support, add cross-runtime links, fix SPDX license parens; ci: add Windows to Node build matrix
- Professionalize READMEs and update roadmap
- *(security)* Document secret_key_bytes() sensitivity and key-persistence workflow
- *(security)* Surface open-endpoint risk in serve() docs and quick-start examples
- *(security)* Add peer-id check to package README Usage examples
- Add threat model and document key revocation limitation
- Fix maxConcurrency default from 64 to 1024 and clarify option semantics

### ⚡ Performance

- Fix all findings from 03_review (P0–P3)
- Speed up CI + multi-platform node builds
- *(core)* Reduce allocation overhead in pool, pumps, base32, sweep
- *(core)* Deduplicate base32_encode call in server accept loop
- *(core)* Eliminate per-read allocation in pump_duplex
- *(#38)* Optimize Deno FFI hot path — binary sendChunk + dispatch macro

### 🎨 Styling

- Cargo fmt integration test formatting

### 🧪 Testing

- Add Node.js smoke test
- Add integration tests for server limits and edge cases
- Add e2e regression tests for concurrent buffer race and trailer handle bug
- Speed up integration tests, add cross-runtime compliance harness
- Add true cross-runtime interop compliance harness
- Wire all test suites into npm test and deno task test
- *(TEST-004)* Add Rust core edge-case tests
- Add property tests, fuzz targets, and nightly fuzz CI
- *(core)* Replace wall-clock sleeps with deterministic synchronization
- *(#39)* Expand test coverage — sessions, stress, compliance cases

### ⚙️ Miscellaneous Tasks

- Untrack .vscode/settings.json (already in .gitignore)
- Ignore .vscode/* (keep extensions/tasks/launch), fix shared require path
- Add version bump and local build scripts
- Update .gitignore and add protocol documentation
- Remove deprecated caching, capability tokens, and group messaging documentation
- Update status to done for discovery and observability patches
- Update status to done for integration test suite patch
- Update lockfiles after @scure/base addition; gitignore .so and .venv
- Exclude workspace/old_references and target from deno test discovery
- Fix deno test discovery — add test include, update task for renamed smoke.test.ts
- Cleanup — remove workspace/, framing crate, promote docs
- Remove obsolete rework_01 documentation and implementation notes
- Untrack temp/ and gitignore it
- Format all Rust and TypeScript source
- Add Cargo.lock for iroh-http-py standalone workspace
- Remove VS Code configuration files from .gitignore
- Close 67 issues — fixes complete
- Remove redundant --exclude iroh-http-py from build script
- Remove name field from example deno.jsonc to silence Deno warning
- Suppress stdout in require() smoke test to prevent output bleed
- Show smoke test output on failure, suppress on success
- Gitignore .tauri, fetch Swift API via setup-ios.sh
- Remove setup-ios.sh — .tauri is populated by the consumer app
- Remove duplicate issue B-ISS-045 regarding backpressure config
- Mark TEST-001 through TEST-006 as fixed
- Remove Python adapter (iroh-http-py)
- Remove local issues/ folder and TEST_PLAN.md (tracked on GitHub)
- Add manage-issues skill and GitHub issue templates
- Condense copilot-instructions.md — flat reference list, link to manage-issues skill
- Disable CI/release workflows (running locally for now)
- Add git-conventions skill for Conventional Commits
- Auto-approve release script in terminal tools
- Update tauri lockfile
- Update deno lockfile
- *(node)* Apply cargo fmt
- *(release)* Drop crates.io publish — core/discovery are internal crates
- *(release)* Publish tauri-plugin-iroh-http to crates.io
- *(release)* Modularize publish step via npm run publish:* scripts
- Apply cargo fmt --all
- Bump deno.jsonc version to 0.1.3
- Enable push/PR triggers and add feature matrix coverage
- *(security)* Add security hardening infrastructure (#74)
- *(oss-prep)* Pre-open-source cleanup
- *(oss-prep)* CI release pipeline, experimental warning, Deno artifact URL
- *(bench)* Remove pull_request trigger — benchmarks run on main push and tags only
- *(.gitignore)* Enhance environment and secrets protection by adding additional patterns
- *(release)* Switch to OIDC trusted publishing — no tokens needed
- *(release)* Add CARGO_REGISTRY_TOKEN env to publish-crates job
- Skip workflows on docs/markdown-only changes, scope bench to code paths
- Fix Node version, napi artifacts, zig setup, crates ordering
- Bump version to 0.1.4
- Pre-commit hook for cargo fmt, faster audit tooling, leaner job matrix
- Update CI workflows and add pre-push check script
- Replace macos-13 runners with macos-latest cross-compilation
- Bump version to 0.1.5
- Fix two release workflow failures
- Bump version to 0.1.6
- Fix cargo-audit missing and deno bench hanging
- Update lock files after bench fixes
- Update deno.lock
- *(node)* Regenerate napi bindings after trailer removal
- Add fix-issues skill for systematic issue resolution workflow
- Add single npm run ci command and unify check.sh coverage
- Fix tag glob patterns, npm install, and add cargo-deny
- *(release)* Add verify-release gate and remove --no-verify
- Bump actions/checkout and actions/setup-node to v5
- Tighten deny.toml advisory policy and add Tauri to CI matrix
- *(check)* Add cargo deny step to check.sh
- Bump actions/cache, upload-artifact, and download-artifact off Node 20
- *(deny)* Suppress known iroh-internal duplicate-version warnings
- Release v0.2.0
