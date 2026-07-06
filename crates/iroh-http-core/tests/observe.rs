//! Regression tests for peer path-change observation (`subscribe_path_changes`).

use std::time::Duration;

use iroh_http_core::{IrohEndpoint, NetworkingOptions, NodeOptions};

async fn bind_disabled() -> IrohEndpoint {
    IrohEndpoint::bind(NodeOptions {
        networking: NetworkingOptions {
            disabled: true,
            ..Default::default()
        },
        ..Default::default()
    })
    .await
    .unwrap()
}

/// Regression: #298 — repeated `subscribe_path_changes` for the SAME peer leaks
/// a watcher task each call and over-counts `active_path_watchers`.
///
/// Root cause: each call unconditionally spawned a new watcher task and did
/// `active_path_watchers.fetch_add(1, …)`. Replacing the sender in `path_subs`
/// did not stop the previous watcher (it re-read the map key and saw the new,
/// non-closed sender), so old watchers never exited.
///
/// Fix: reuse the existing watcher for a peer that is already subscribed —
/// replace only the sender, and neither spawn a new task nor bump the gauge.
#[tokio::test]
async fn repeated_subscribe_same_peer_counts_one_watcher() {
    let ep = bind_disabled().await;
    let peer = "test-peer-node-id";

    // Subscribe three times for the same peer. Hold every receiver alive so
    // none of the senders report `is_closed()`.
    let _rx1 = ep.subscribe_path_changes(peer);
    let _rx2 = ep.subscribe_path_changes(peer);
    let _rx3 = ep.subscribe_path_changes(peer);

    let stats = ep.endpoint_stats();
    assert_eq!(
        stats.active_path_watchers, 1,
        "one live watcher per peer expected; got {} (leaked/over-counted watchers)",
        stats.active_path_watchers
    );
    assert_eq!(
        stats.active_path_subscriptions, 1,
        "one subscription entry per peer expected"
    );

    ep.close().await;
}

/// Regression: #298 — after unsubscribing, the watcher gauge must return to
/// zero (each live watcher counted once, decremented on unsubscribe/drop).
#[tokio::test]
async fn unsubscribe_decrements_watcher_gauge_to_zero() {
    let ep = bind_disabled().await;
    let peer = "test-peer-node-id";

    let rx1 = ep.subscribe_path_changes(peer);
    let rx2 = ep.subscribe_path_changes(peer);
    assert_eq!(ep.endpoint_stats().active_path_watchers, 1);

    // Drop receivers and unsubscribe so the watcher observes closure and exits.
    drop(rx1);
    drop(rx2);
    ep.unsubscribe_path_changes(peer);

    // Watcher wakes at most every ~200 ms; poll until it exits.
    let mut watchers = usize::MAX;
    for _ in 0..50 {
        watchers = ep.endpoint_stats().active_path_watchers;
        if watchers == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(
        watchers, 0,
        "watcher gauge should return to zero after unsubscribe, got {watchers}"
    );

    ep.close().await;
}
