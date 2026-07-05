//! Research spike for issue #286 (perf-2): redundant JSON re-encode on the
//! Deno fetch warm path.
//!
//! The current warm path (verified against the tree on the branch this bench
//! was authored on) does the following for every `start_fetch` FFI call:
//!
//!   1. `packages/iroh-http-deno/src/lib.rs` ~L696
//!        let p: serde_json::Value = serde_json::from_slice(payload)?;   // parse #1
//!   2. `packages/iroh-http-deno/src/lib.rs` ~L706
//!        dispatch::dispatch("rawFetch", &serde_json::to_vec(&p)?)       // re-encode
//!   3. `packages/iroh-http-deno/src/dispatch.rs` ~L123
//!        let p: Value = serde_json::from_slice(payload)?;               // parse #2
//!   4. `packages/iroh-http-deno/src/dispatch.rs` ~L558 (raw_fetch)
//!        let args: RawFetchPayload = serde_json::from_value(p)?;        // Value -> struct
//!
//! i.e. the original request slice is parsed to a `Value`, serialized back to
//! bytes, re-parsed to a `Value`, then finally converted to the typed struct.
//!
//! The proposed optimization skips the round trip and parses the original
//! slice straight into the typed struct:
//!
//!        let args: RawFetchPayload = serde_json::from_slice(payload)?;  // single parse
//!
//! This bench measures both chains on a representative small fetch payload so
//! the orchestrator can evaluate the >=15% p50 gate. It performs no I/O and
//! deliberately does not touch any production crate.

use criterion::{criterion_group, criterion_main, Criterion};
use std::hint::black_box;
use serde::Deserialize;

/// Copied verbatim from `packages/iroh-http-deno/src/dispatch.rs` (~L541) so
/// this spike stays self-contained and requires no production changes.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
struct RawFetchPayload {
    endpoint_handle: u64,
    node_id: String,
    url: String,
    method: String,
    headers: Vec<Vec<String>>,
    req_body_handle: Option<u64>,
    fetch_token: Option<u64>,
    direct_addrs: Option<Vec<String>>,
    timeout_ms: Option<u64>,
    decompress: Option<bool>,
    max_response_body_bytes: Option<u64>,
}

/// A representative small fetch request as the JS side would encode it.
/// camelCase keys, a couple of headers, a node id, and a direct addr — the
/// typical warm-path GET.
fn sample_payload() -> Vec<u8> {
    let json = serde_json::json!({
        "endpointHandle": 1u64,
        "nodeId": "k51qzi5uqu5dktukr6z1i5f6 crushed placeholder node id abcdef0123456789",
        "url": "httpi://node/api/v1/resource?id=42",
        "method": "GET",
        "headers": [
            ["accept", "application/json"],
            ["user-agent", "iroh-http-deno/0.5.2"],
        ],
        "reqBodyHandle": serde_json::Value::Null,
        "fetchToken": 7u64,
        "directAddrs": ["127.0.0.1:4433"],
        "timeoutMs": 30000u64,
        "decompress": true,
        "maxResponseBodyBytes": serde_json::Value::Null,
    });
    serde_json::to_vec(&json).expect("static payload serializes")
}

/// Current chain: from_slice -> Value -> to_vec -> from_slice -> Value -> from_value::<RawFetchPayload>.
fn current_chain(payload: &[u8]) -> RawFetchPayload {
    // lib.rs: parse #1
    let p: serde_json::Value = serde_json::from_slice(payload).expect("parse #1");
    // lib.rs: re-encode
    let reencoded = serde_json::to_vec(&p).expect("re-encode");
    // dispatch.rs: parse #2
    let p2: serde_json::Value = serde_json::from_slice(&reencoded).expect("parse #2");
    // raw_fetch: Value -> struct
    serde_json::from_value(p2).expect("from_value")
}

/// Proposed chain: single from_slice::<RawFetchPayload>.
fn proposed_chain(payload: &[u8]) -> RawFetchPayload {
    serde_json::from_slice(payload).expect("from_slice struct")
}

fn bench_serde(c: &mut Criterion) {
    let payload = sample_payload();

    // Sanity: both chains produce an equivalent parse (compare a couple fields).
    let a = current_chain(&payload);
    let b = proposed_chain(&payload);
    assert_eq!(a.node_id, b.node_id);
    assert_eq!(a.url, b.url);
    assert_eq!(a.headers.len(), b.headers.len());

    let mut group = c.benchmark_group("deno_fetch_serde");
    group.bench_function("current_chain", |bch| {
        bch.iter(|| current_chain(black_box(&payload)))
    });
    group.bench_function("proposed_chain", |bch| {
        bch.iter(|| proposed_chain(black_box(&payload)))
    });
    group.finish();
}

criterion_group!(benches, bench_serde);
criterion_main!(benches);
