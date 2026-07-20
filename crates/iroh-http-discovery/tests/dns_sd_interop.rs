//! End-to-end DNS-SD interop test for the desktop discovery stack.
//!
//! Advertises a node via [`iroh_http_discovery::advertise_peer`] and confirms a
//! second endpoint discovers it via [`iroh_http_discovery::browse_peers`]. This
//! exercises the real `mdns-sd` wire path (PTR + SRV + TXT + A/AAAA) that the
//! rework in issue #329 introduced.
//!
//! Ignored by default: it needs a working multicast-capable network interface,
//! which is not guaranteed in CI sandboxes. Run locally with:
//!
//! ```sh
//! cargo test -p iroh-http-discovery --test dns_sd_interop -- --ignored --nocapture
//! ```

use std::time::Duration;

use iroh::{Endpoint, RelayMode};
use iroh_http_discovery::{advertise_peer, browse_peers};

async fn minimal_endpoint() -> Endpoint {
    Endpoint::builder(iroh::endpoint::presets::Minimal)
        .relay_mode(RelayMode::Disabled)
        .alpns(vec![b"iroh-http-discovery-test/0".to_vec()])
        .bind()
        .await
        .expect("bind minimal endpoint")
}

#[tokio::test]
#[ignore = "requires a multicast-capable network interface; run with --ignored"]
async fn advertised_node_is_discovered_over_dns_sd() {
    // A unique-ish service name so concurrent test runs don't collide.
    let service = "irohhttpitest";

    let adv_ep = minimal_endpoint().await;
    // Discovery reports node ids as lowercase base32 (the DNS-SD label / `pk`
    // TXT), not iroh's 64-char hex `Display`, so derive the same encoding here.
    let adv_id = base32::encode(
        base32::Alphabet::Rfc4648Lower { padding: false },
        adv_ep.id().as_bytes(),
    );
    let _advertise = advertise_peer(&adv_ep, service).expect("start advertise");

    let browse_ep = minimal_endpoint().await;
    let session = browse_peers(&browse_ep, service)
        .await
        .expect("start browse");

    let discovered = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            match session.next_event().await {
                Some(ev) if ev.is_active && ev.node_id == adv_id => {
                    return Some(ev);
                }
                Some(_) => continue,
                None => return None,
            }
        }
    })
    .await
    .expect("timed out waiting for discovery");

    let ev = discovered.expect("browse stream closed before discovering the advertised node");
    assert_eq!(ev.node_id, adv_id);
    assert!(
        !ev.addrs.is_empty(),
        "discovered peer should carry at least one address, got {:?}",
        ev.addrs
    );
}
