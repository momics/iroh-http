## [unreleased]

### � Breaking Changes

- *(core)* Remove `raw_connect` from the public Rust surface and the `connect` Tauri command. The session API (`node.dial`/sessions) covers the same use case with a WebTransport-shaped surface; the JS adapters never wired the legacy upgrade entrypoint. Server-side `req.upgrade()` is unchanged.
- *(core)* Rename pure-Rust serve entries: `serve_service` → `serve`, `serve_service_with_events` → `serve_with_events` (ADR-014 D5). The FFI entries are demoted: `serve` → `ffi_serve`, `serve_with_callback` → `ffi_serve_with_callback`. All platform adapters (Node, Deno, Tauri) are updated. Pure-Rust callers should replace `iroh_http_core::serve_service(…)` with `iroh_http_core::serve(…)` and `iroh_http_core::serve_with_callback(…)` with `iroh_http_core::ffi_serve_with_callback(…)`.
### �🚀 Features

- *(node)* Napi-rs platform package split

### 🐛 Bug Fixes

- *(bench)* Uint8Array type cast for Deno strict check, add --skipLibCheck for node tsc
- *(ci)* Check no-default-features with compression kept on — tests discovery-off path only
### 🚜 Refactor

- *(deps)* Upgrade `iroh` from 0.98.x to 1.0.0. mDNS discovery now uses the standalone `iroh-mdns-address-lookup` crate (Iroh removed the `address-lookup-mdns` feature). `RecvStream::read_chunk` returns `Bytes` directly.
- *(core)* Replace hand-rolled `max_request_body_bytes` accounting in `FfiDispatcher::dispatch` with `tower_http::limit::RequestBodyLimitLayer`. Closes the last bespoke enforcement in the request path; per ADR-013, body-size enforcement now lives entirely in the standard tower-http stack. The layer rejects oversized requests with `413 Payload Too Large` (immediately for known `Content-Length`, or by erroring the wrapped `Limited` body mid-stream for chunked uploads).
### 📚 Documentation

- *(readmes)* Fix Windows platform support, add cross-runtime links, fix SPDX license parens; ci: add Windows to Node build matrix
- Professionalize READMEs and update roadmap

### ⚙️ Miscellaneous Tasks

- Skip workflows on docs/markdown-only changes, scope bench to code paths
- Fix Node version, napi artifacts, zig setup, crates ordering
## [0.1.3] - 2026-04-20

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

### ⚡ Performance

- Fix all findings from 03_review (P0–P3)
- Speed up CI + multi-platform node builds
- *(core)* Reduce allocation overhead in pool, pumps, base32, sweep
- *(core)* Deduplicate base32_encode call in server accept loop
- *(core)* Eliminate per-read allocation in pump_duplex
- *(#38)* Optimize Deno FFI hot path — binary sendChunk + dispatch macro

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
