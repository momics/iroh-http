#![allow(clippy::disallowed_types)]
//! Body channel micro-benchmarks — isolates the mpsc channel + handle store
//! overhead from the QUIC transport.
//!
//! Run:
//!   cargo bench -p iroh-http-core --bench channel
//!
//! These benchmarks measure:
//!   1. Raw channel throughput (write → read, no handle store)
//!   2. Handle-store-mediated throughput (send_chunk → next_chunk via handles)
//!   3. Handle lifecycle cost (alloc → insert → remove at scale)
//!
//! Comparing these to the full `throughput` bench reveals how much time is
//! spent in the channel vs. the QUIC + HTTP layers.

use bytes::Bytes;
use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use http_body_util::BodyExt;
use iroh_http_core::{make_body_channel, Body, IrohEndpoint, NetworkingOptions, NodeOptions};

fn local_opts() -> NodeOptions {
    NodeOptions {
        networking: NetworkingOptions {
            disabled: true,
            bind_addrs: vec!["127.0.0.1:0".into()],
            ..Default::default()
        },
        ..Default::default()
    }
}

// ── bench 1: raw channel write→read (no handle store) ────────────────────────

fn bench_raw_channel(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let mut group = c.benchmark_group("channel/raw");

    for size in [1_024usize, 64 * 1_024, 1_024 * 1_024] {
        group.throughput(Throughput::Bytes(size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, &sz| {
            let chunk = Bytes::from(vec![0x42u8; sz]);
            b.to_async(&rt).iter(|| {
                let chunk = chunk.clone();
                async move {
                    let (writer, reader) = make_body_channel();
                    writer.send_chunk(chunk).await.unwrap();
                    drop(writer); // signal EOF
                    let got = reader.next_chunk().await;
                    assert!(got.is_some());
                    let eof = reader.next_chunk().await;
                    assert!(eof.is_none());
                }
            });
        });
    }
    group.finish();
}

// ── bench 2: handle-store-mediated write→read ────────────────────────────────

fn bench_handle_store_channel(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let ep = rt.block_on(IrohEndpoint::bind(local_opts())).unwrap();

    let mut group = c.benchmark_group("channel/handle_store");

    for size in [1_024usize, 64 * 1_024, 1_024 * 1_024] {
        group.throughput(Throughput::Bytes(size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, &sz| {
            let chunk = Bytes::from(vec![0x42u8; sz]);
            let ep = ep.clone();
            b.to_async(&rt).iter(|| {
                let chunk = chunk.clone();
                let ep = ep.clone();
                async move {
                    let (writer, reader) = ep.handles().make_body_channel();
                    let wh = ep.handles().insert_writer(writer).unwrap();
                    let rh = ep.handles().insert_reader(reader).unwrap();

                    ep.handles().send_chunk(wh, chunk).await.unwrap();
                    ep.handles().finish_body(wh).unwrap();

                    // Drain all chunks (send_chunk splits at max_chunk_size=64KB)
                    while ep.handles().next_chunk(rh).await.unwrap().is_some() {}
                }
            });
        });
    }
    group.finish();
}

// ── bench 3: handle lifecycle at scale ───────────────────────────────────────

fn bench_handle_lifecycle(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let ep = rt.block_on(IrohEndpoint::bind(local_opts())).unwrap();

    let mut group = c.benchmark_group("handle_lifecycle");

    // Measure alloc+free with a warm slab (pre-fill then empty)
    group.bench_function("alloc_free_warm", |b| {
        // Pre-fill to warm the slab allocator
        let mut handles = Vec::new();
        for _ in 0..100 {
            let (h, _r) = ep.handles().alloc_body_writer().unwrap();
            handles.push(h);
        }
        for h in handles {
            ep.handles().finish_body(h).unwrap();
        }

        b.iter(|| {
            let (h, _reader) = ep.handles().alloc_body_writer().unwrap();
            ep.handles().finish_body(h).unwrap();
        });
    });

    // Measure alloc+insert_reader+cancel_reader round-trip
    group.bench_function("reader_insert_cancel", |b| {
        b.iter(|| {
            let (_writer, reader) = make_body_channel();
            let rh = ep.handles().insert_reader(reader).unwrap();
            ep.handles().cancel_reader(rh);
        });
    });

    group.finish();
}

// ── bench 4: streaming throughput (multi-chunk) ──────────────────────────────

fn bench_streaming(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let ep = rt.block_on(IrohEndpoint::bind(local_opts())).unwrap();

    let mut group = c.benchmark_group("channel/streaming");

    // 1 MB total as 16 × 64KB chunks — measures sustained channel throughput
    let total = 1_024 * 1_024;
    let chunk_size = 64 * 1_024;
    let num_chunks = total / chunk_size;
    group.throughput(Throughput::Bytes(total as u64));

    group.bench_function("1mb_as_64kb_chunks", |b| {
        let chunk = Bytes::from(vec![0x42u8; chunk_size]);
        let ep = ep.clone();
        b.to_async(&rt).iter(|| {
            let chunk = chunk.clone();
            let ep = ep.clone();
            async move {
                let (writer, reader) = ep.handles().make_body_channel();
                let wh = ep.handles().insert_writer(writer).unwrap();
                let rh = ep.handles().insert_reader(reader).unwrap();

                // Spawn writer
                let ep2 = ep.clone();
                let write_task = tokio::spawn(async move {
                    for _ in 0..num_chunks {
                        ep2.handles().send_chunk(wh, chunk.clone()).await.unwrap();
                    }
                    ep2.handles().finish_body(wh).unwrap();
                });

                // Read all chunks
                while ep.handles().next_chunk(rh).await.unwrap().is_some() {}

                write_task.await.unwrap();
            }
        });
    });

    group.finish();
}

// ── bench 5: BodyReader as http_body::Body ──────────────────────────────────

fn bench_body_reader_http_body(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let mut group = c.benchmark_group("channel/body_reader_http_body");

    for size in [1_024usize, 64 * 1_024, 1_024 * 1_024] {
        group.throughput(Throughput::Bytes(size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, &sz| {
            let chunk = Bytes::from(vec![0x42u8; sz]);
            b.to_async(&rt).iter(|| {
                let chunk = chunk.clone();
                async move {
                    let (writer, reader) = make_body_channel();
                    writer.send_chunk(chunk).await.unwrap();
                    drop(writer);

                    let collected = Body::new(reader).collect().await.unwrap().to_bytes();
                    assert_eq!(collected.len(), sz);
                }
            });
        });
    }

    group.finish();
}

// ── bench 5: raw QUIC stream (no HTTP framing) ──────────────────────────────

fn bench_raw_quic_stream(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();

    let (server_ep, client_ep, server_addrs) = rt.block_on(async {
        let server = IrohEndpoint::bind(local_opts()).await.unwrap();
        let client = IrohEndpoint::bind(local_opts()).await.unwrap();
        let addrs: Vec<std::net::SocketAddr> = server.raw().addr().ip_addrs().cloned().collect();
        (server, client, addrs)
    });

    let server_id = server_ep.raw().secret_key().public();
    let alpn: &[u8] = iroh_http_core::ALPN;

    // Accept loop: read everything from each bistream, then close it.
    let sep = server_ep.clone();
    let _guard = rt.enter();
    rt.spawn(async move {
        while let Some(conn) = sep.raw().accept().await {
            let conn = conn.await.unwrap();
            tokio::spawn(async move {
                loop {
                    let (mut send, mut recv) = match conn.accept_bi().await {
                        Ok(pair) => pair,
                        Err(_) => break,
                    };
                    tokio::spawn(async move {
                        // Read entire body
                        while recv.read_chunk(64 * 1024).await.unwrap_or(None).is_some() {}
                        // Send the same-size response (we just echo the byte count)
                        let _ = send.finish();
                    });
                }
            });
        }
    });

    // Pre-connect so the bench loop doesn't include QUIC handshake
    let conn = rt.block_on(async {
        let mut addr = iroh::EndpointAddr::from(server_id);
        for a in &server_addrs {
            addr = addr.with_ip_addr(*a);
        }
        client_ep.raw().connect(addr, alpn).await.unwrap()
    });

    let mut group = c.benchmark_group("quic_raw_stream");

    for size in [1_024usize, 64 * 1_024, 1_024 * 1_024] {
        group.throughput(Throughput::Bytes(size as u64));
        if size >= 1_024 * 1_024 {
            group.sample_size(50);
        }
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, &sz| {
            let data = vec![0x42u8; sz];
            let conn = conn.clone();
            b.to_async(&rt).iter(|| {
                let data = data.clone();
                let conn = conn.clone();
                async move {
                    let (mut send, mut recv) = conn.open_bi().await.unwrap();
                    send.write_all(&data).await.unwrap();
                    send.finish().unwrap();
                    // Wait for server to finish
                    while recv.read_chunk(64 * 1024).await.unwrap_or(None).is_some() {}
                }
            });
        });
    }

    group.finish();
}

// ── bench 6: full HTTP/1.1-over-QUIC (reuses connection) ─────────────────────
// Same as throughput bench's response_body but included here for direct
// comparison with raw QUIC stream numbers in the same run.

fn bench_http_over_quic(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let (server_ep, client_ep) = rt.block_on(async {
        let s = IrohEndpoint::bind(local_opts()).await.unwrap();
        let c = IrohEndpoint::bind(local_opts()).await.unwrap();
        (s, c)
    });

    let server_id = server_ep.node_id().to_string();
    let server_addrs: Vec<std::net::SocketAddr> =
        server_ep.raw().addr().ip_addrs().cloned().collect();

    let _guard = rt.enter();

    // Server: return N bytes in the response body
    let sep = server_ep.clone();
    iroh_http_core::ffi_serve(
        server_ep,
        iroh_http_core::ServeOptions::default(),
        move |payload: iroh_http_core::RequestPayload| {
            let sep2 = sep.clone();
            tokio::spawn(async move {
                let n: usize = payload
                    .url
                    .rsplit('/')
                    .next()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0);
                while sep2
                    .handles()
                    .next_chunk(payload.req_body_handle)
                    .await
                    .unwrap()
                    .is_some()
                {}
                iroh_http_core::respond(sep2.handles(), payload.req_handle, 200, vec![]).unwrap();
                if n > 0 {
                    sep2.handles()
                        .send_chunk(payload.res_body_handle, Bytes::from(vec![0u8; n]))
                        .await
                        .unwrap();
                }
                sep2.handles().finish_body(payload.res_body_handle).unwrap();
            });
        },
    );

    let mut group = c.benchmark_group("http_over_quic");

    for size in [1_024usize, 64 * 1_024, 1_024 * 1_024] {
        group.throughput(Throughput::Bytes(size as u64));
        if size >= 1_024 * 1_024 {
            group.sample_size(50);
        }
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, &sz| {
            let client = client_ep.clone();
            let id = server_id.clone();
            let addrs = server_addrs.clone();
            b.to_async(&rt).iter(|| {
                let client = client.clone();
                let id = id.clone();
                let addrs = addrs.clone();
                async move {
                    let url = format!("/bench/{sz}");
                    let res = iroh_http_core::fetch(
                        &client,
                        &id,
                        &url,
                        "GET",
                        &[],
                        None,
                        None,
                        Some(&addrs),
                        None,
                        true,
                        None,
                    )
                    .await
                    .unwrap();
                    while client
                        .handles()
                        .next_chunk(res.body_handle)
                        .await
                        .unwrap()
                        .is_some()
                    {}
                }
            });
        });
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_raw_channel,
    bench_handle_store_channel,
    bench_handle_lifecycle,
    bench_streaming,
    bench_body_reader_http_body,
    bench_raw_quic_stream,
    bench_http_over_quic,
);
criterion_main!(benches);
