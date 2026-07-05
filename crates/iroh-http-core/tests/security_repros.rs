#![allow(clippy::disallowed_types)] // integration tests exercise FFI body handles
mod common;

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use bytes::Bytes;
use iroh_http_core::{fetch, ffi_serve, respond, ErrorCode, RequestPayload, ServeOptions};

// Regression: #269 — over-limit request bodies must not look like clean EOF.
#[tokio::test]
async fn oversized_request_body_surfaces_error_and_content_length_gets_413() {
    let (server_ep, client_ep) = common::make_pair().await;
    let server_id = common::node_id(&server_ep);
    let addrs = common::server_addrs(&server_ep);
    let handler_calls = Arc::new(AtomicUsize::new(0));
    let handler_calls_for_assert = handler_calls.clone();

    ffi_serve(
        server_ep.clone(),
        ServeOptions {
            max_request_body_wire_bytes: Some(64),
            ..Default::default()
        },
        move |payload: RequestPayload| {
            handler_calls.fetch_add(1, Ordering::SeqCst);
            let server_ep = server_ep.clone();
            tokio::spawn(async move {
                let mut received = 0usize;
                let mut error_code = None;
                loop {
                    match server_ep
                        .handles()
                        .next_chunk(payload.req_body_handle)
                        .await
                    {
                        Ok(Some(chunk)) => received += chunk.len(),
                        Ok(None) => break,
                        Err(e) => {
                            error_code = Some(e.code);
                            break;
                        }
                    }
                }
                respond(
                    server_ep.handles(),
                    payload.req_handle,
                    200,
                    vec![("content-type".into(), "text/plain".into())],
                )
                .unwrap();
                server_ep
                    .handles()
                    .send_chunk(
                        payload.res_body_handle,
                        Bytes::from(format!("received={received};error={error_code:?}")),
                    )
                    .await
                    .unwrap();
                server_ep
                    .handles()
                    .finish_body(payload.res_body_handle)
                    .unwrap();
            });
        },
    );

    let (writer, reader) = iroh_http_core::make_body_channel();
    let send_task = tokio::spawn(async move {
        writer
            .send_chunk(Bytes::from(vec![b'a'; 64]))
            .await
            .unwrap();
        writer
            .send_chunk(Bytes::from(vec![b'b'; 64]))
            .await
            .unwrap();
        drop(writer);
    });

    let res = fetch(
        &client_ep,
        &server_id,
        "/streaming-upload",
        "POST",
        &[],
        Some(reader),
        None,
        Some(&addrs),
        None,
        true,
        None,
    )
    .await
    .expect("response head succeeds");
    send_task.await.unwrap();

    assert_eq!(res.status, 200);
    let chunk = client_ep
        .handles()
        .next_chunk(res.body_handle)
        .await
        .unwrap()
        .expect("response body chunk");
    let body = String::from_utf8(chunk.to_vec()).unwrap();
    assert_eq!(body, "received=64;error=Some(BodyTooLarge)");

    let before_declared_length_request = handler_calls_for_assert.load(Ordering::SeqCst);
    let (writer, reader) = iroh_http_core::make_body_channel();
    let send_task = tokio::spawn(async move {
        writer
            .send_chunk(Bytes::from(vec![b'c'; 64]))
            .await
            .unwrap();
        writer
            .send_chunk(Bytes::from(vec![b'd'; 64]))
            .await
            .unwrap();
        drop(writer);
    });
    let res = fetch(
        &client_ep,
        &server_id,
        "/declared-upload",
        "POST",
        &[("content-length".into(), "128".into())],
        Some(reader),
        None,
        Some(&addrs),
        None,
        true,
        None,
    )
    .await
    .expect("413 response should be generated before handler");
    send_task.await.unwrap();

    assert_eq!(res.status, 413);
    assert_eq!(
        handler_calls_for_assert.load(Ordering::SeqCst),
        before_declared_length_request,
        "Content-Length over the wire cap must not invoke the handler"
    );
}

// Regression: #271 — over-limit fetch response bodies must surface BodyTooLarge.
#[tokio::test]
async fn response_body_limit_surfaces_body_too_large() {
    let (server_ep, client_ep) = common::make_pair().await;
    let server_id = common::node_id(&server_ep);
    let addrs = common::server_addrs(&server_ep);

    ffi_serve(
        server_ep.clone(),
        ServeOptions::default(),
        move |payload: RequestPayload| {
            respond(
                server_ep.handles(),
                payload.req_handle,
                200,
                vec![("content-type".into(), "application/octet-stream".into())],
            )
            .unwrap();
            let server_ep = server_ep.clone();
            tokio::spawn(async move {
                server_ep
                    .handles()
                    .send_chunk(payload.res_body_handle, Bytes::from(vec![b'x'; 64]))
                    .await
                    .unwrap();
                server_ep
                    .handles()
                    .send_chunk(payload.res_body_handle, Bytes::from(vec![b'y'; 64]))
                    .await
                    .unwrap();
                server_ep
                    .handles()
                    .finish_body(payload.res_body_handle)
                    .unwrap();
            });
        },
    );

    let res = fetch(
        &client_ep,
        &server_id,
        "/download",
        "GET",
        &[],
        None,
        None,
        Some(&addrs),
        None,
        true,
        Some(64),
    )
    .await
    .expect("response head succeeds");
    assert_eq!(res.status, 200);

    let first = client_ep
        .handles()
        .next_chunk(res.body_handle)
        .await
        .unwrap()
        .expect("prefix within limit");
    assert_eq!(first.len(), 64);

    let err = client_ep
        .handles()
        .next_chunk(res.body_handle)
        .await
        .expect_err("overflow must surface as a stream error, not clean EOF");
    assert_eq!(err.code, ErrorCode::BodyTooLarge);
}
