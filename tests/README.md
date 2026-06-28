# iroh-http Test Suite

A comprehensive test suite for [`@momics/iroh-http`](https://github.com/Momics/iroh-http) covering HTTP compliance, node lifecycle, error handling, and stress testing across all supported runtimes (Node.js, Deno, Tauri).

## Test categories

### 1. HTTP compliance (`http-compliance/`)

Data-driven tests validating HTTP semantics (RFC 9110). Extends the upstream `tests/http-compliance/cases.json` (35 cases) to **102 test cases**:

| Category | Cases | What it tests |
|---|---|---|
| **Status Codes** | 22 | All major HTTP status codes across 2xx/3xx/4xx/5xx |
| **HTTP Methods** | 9 | GET, POST, PUT, PATCH, DELETE, OPTIONS, HEAD, custom verbs |
| **Body Handling** | 19 | Echo, UTF-8, emoji, JSON, newlines, special chars, sizes 1B→5MB |
| **Request Headers** | 9 | Custom, case-insensitive, empty, long (8KiB), content-type |
| **Response Headers** | 5 | Single/multiple headers, Content-Type forwarding |
| **Peer-Id Security** | 4 | Injection, anti-spoofing, consistency, format validation |
| **Path Handling** | 12 | Trailing slashes, query strings, URL encoding, dot segments |
| **Concurrent** | 4 | 3/5/10 parallel requests, concurrent POST with body |
| **Sequential** | 3 | Connection reuse, mixed methods, mixed status codes |
| **Content-Type** | 4 | JSON, text/plain, octet-stream, charset |
| **Streaming** | 4 | 64KB, 256KB, 1MB, 5MB streamed responses |
| **Edge Cases** | 7 | Unusual status codes, long paths, many headers |

### 2. Lifecycle (`lifecycle/`)

Imperative tests for node creation, shutdown, and resource management:

- `createNode()` returns valid node with publicKey, secretKey
- publicKey is consistent across accesses
- Two nodes get different keys
- `node.addr()` returns correct info
- `node.close()` resolves cleanly, `node.closed` promise settles
- Double close is idempotent (no crash)
- `serve()` returns handle, handler receives valid Request objects
- `fetch()` returns valid Response with readable body
- 10 sequential create/close cycles (leak detection)

### 3. Error handling (`errors/`)

Tests error scenarios and recovery:

- Handler throws → client gets error/500
- Handler returns rejected promise → client gets error
- Server stays alive after handler error (recovery)
- Fetch to unknown peer → rejection
- Fetch with pre-aborted signal → immediate rejection
- Fetch with mid-request abort → cancellation
- Handler returning non-Response → no crash
- Null body on 200 → no crash
- Peer-Id header cannot be spoofed by client

### 4. Stress (`stress/`)

Tests behavior under load:

- 50 concurrent GETs → all succeed
- 20 concurrent POSTs with body → all echo correctly
- 100 sequential GETs → connection reuse
- 1MB body round-trip with byte-level verification
- 5 concurrent 256KB transfers
- 20 rapid create/close cycles with fetch in each
- 5 independent node pairs running concurrently
- 30 concurrent mixed-method requests

## Architecture

```
tests/
├── run-all.sh                   # Run all suites for Node + Deno (+ --tauri)
├── README.md
│
├── suites/                      # Shared cross-runtime test suites
│   ├── lifecycle.mjs            # Node creation, shutdown, serve lifecycle
│   ├── errors.mjs               # Handler failures, unknown peers, abort
│   ├── stress.mjs               # Concurrent/sequential load, regressions
│   ├── events.mjs               # EventTarget, diagnostics
│   ├── sessions.mjs             # WebTransport sessions
│   ├── keys.mjs                 # PublicKey/SecretKey, sign/verify
│   └── discovery.mjs            # mDNS/DNS peer discovery
│
├── runners/                     # Thin per-runtime wrappers
│   ├── node.mjs                 # Binds suites to node:test + node:assert
│   ├── deno.ts                  # Binds suites to Deno.test + @std/assert
│   └── tauri.ts                 # Binds suites to Tauri webview
│
├── tauri-app/                   # Tauri host app that runs the shared suites
│   ├── index.html               #   inside a real WebKit webview, driven by
│   ├── src/main.ts              #   WebdriverIO + tauri-driver on Linux CI.
│   ├── wdio.conf.js             # WebdriverIO config (builds app, spawns driver)
│   ├── e2e/specs/               # WebDriver spec asserting zero failures
│   └── src-tauri/               # Minimal Tauri shell (+ exit_with_code command)
│
└── http-compliance/             # Data-driven HTTP tests
    ├── cases.json               # 102 test cases (RFC 9110)
    ├── handler.mjs              # Shared compliance server routes
    ├── assertions.mjs           # Shared assertion engine
    ├── run-node.mjs             # Node same-process runner
    ├── run-deno.ts              # Deno same-process runner
    ├── run-tauri.ts             # Tauri webview runner
    ├── run.sh                   # Cross-runtime orchestrator
    ├── runner.ts                # Deno compliance runner helper
    ├── server.mjs               # Standalone Node server
    ├── server.deno.ts           # Standalone Deno server
    ├── client.mjs               # Standalone Node client
    ├── client.deno.ts           # Standalone Deno client
    ├── tauri-test.html          # Tauri test entry page
    └── deno.json                # Deno workspace config
```

### Design principles

1. **DRY** — Shared suites in `tests/suites/` contain all cross-runtime test logic. Thin runners in `tests/runners/` bind them to each runtime's test API.

2. **Categorized** — Each test concern lives in its own suite file. Adapter-specific tests live in each package's `test/adapter.test.*`.

3. **CI-friendly** — All scripts exit with code 1 on failure. The `run-all.sh` orchestrator runs everything. The CI workflow runs each category as a separate step for clear failure attribution.

## Quick start

### Run everything

```bash
./tests/run-all.sh
```

### Run by runtime

```bash
./tests/run-all.sh --node       # Node only
./tests/run-all.sh --deno       # Deno only
./tests/run-all.sh --tauri      # Tauri webview (requires tauri-driver)
```

### Run individual test scripts

```bash
# Shared suites (Node)
node --test tests/runners/node.mjs

# Shared suites (Deno)
deno test -A tests/runners/deno.ts

# HTTP compliance (Node, same-process)
node tests/http-compliance/run-node.mjs --verbose

# HTTP compliance (Deno, same-process)
deno run -A tests/http-compliance/run-deno.ts

# Adapter-specific (Node)
node --test packages/iroh-http-node/test/adapter.test.mjs

# Adapter-specific (Deno)
deno test -A packages/iroh-http-deno/test/adapter.test.ts

# Cross-runtime compliance
bash tests/http-compliance/run.sh
```

### Tauri (in webview)

The shared suites also run inside a real Tauri webview via WebdriverIO +
`tauri-driver`. This is exercised automatically by the `test-tauri` job in
`.github/workflows/extended-tests.yml` and can be run locally on Linux:

```bash
# Prerequisites (Linux):
#   cargo install tauri-driver --locked
#   sudo apt-get install -y libwebkit2gtk-4.1-dev webkit2gtk-driver xvfb
./tests/run-all.sh --tauri
```

The legacy HTTP-compliance webview entry is still available for manual runs:

```bash
cd tauri && npm install
cargo tauri dev
# Navigate to /tests/http-compliance/tauri-test.html
```

URL params: `?filter=status&verbose=true&bail=true&ci=true`

### Filtering (HTTP compliance only)

```bash
node tests/http-compliance/run-node.mjs --filter status --verbose
node tests/http-compliance/run-node.mjs --bail
deno run -A tests/http-compliance/run-deno.ts --filter peer-id
```

## Test case format (HTTP compliance)

```jsonc
{
  "id": "unique-test-id",
  "description": "Human-readable description",
  "request": {
    "method": "GET",
    "path": "/route",
    "headers": { "x-custom": "value" },
    "body": null | "string" | { "fill": 65536 }
  },
  "response": {
    "status": 200,
    "bodyExact": "exact match",
    "bodyNotEmpty": true,
    "bodyNot": "must not equal",
    "bodyContains": "substring",
    "bodyMatchesRegex": "^[a-z]+$",
    "bodyLengthExact": 1024,
    "bodyMinLength": 1,
    "headers": { "content-type": "application/json" }
  }
}
```

### Extended fields (not in upstream)

| Field | Type | Description |
|---|---|---|
| `concurrent` | number | Run N copies of the request in parallel |
| `sequential` | number | Run N copies sequentially |
| `repeat` | number | Run N copies with optional body equality assertion |
| `requests` | array | Multi-step sequential test with per-step expectations |
| `assertAllBodiesEqual` | boolean | Assert all repeated responses have identical bodies |

## Contributing

When adding new cross-runtime tests, add them to the appropriate suite file
in `tests/suites/`. Both runners (`tests/runners/node.mjs` and
`tests/runners/deno.ts`) will automatically pick them up.

For adapter-specific tests (e.g., Deno FFI functions not available in Node),
add them to the package's `test/adapter.test.*` file.
