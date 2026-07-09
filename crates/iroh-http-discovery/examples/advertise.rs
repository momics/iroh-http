//! Manual DNS-SD advertise harness for cross-device testing.
//!
//! Advertises a node on the local network and blocks until Ctrl-C, so you can
//! verify visibility from another device or with Apple's browser:
//!
//! ```sh
//! cargo run -p iroh-http-discovery --example advertise -- my-service
//! # in another terminal:
//! dns-sd -B _my-service._udp local
//! ```
//!
//! The default service name is `iroh-http`.

use std::time::Duration;

use iroh::{Endpoint, RelayMode};
use iroh_http_discovery::advertise_peer;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let service = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "iroh-http".into());

    let ep = Endpoint::builder(iroh::endpoint::presets::Minimal)
        .relay_mode(RelayMode::Disabled)
        .alpns(vec![b"iroh-http-discovery-example/0".to_vec()])
        .bind()
        .await?;

    // Give the endpoint a moment to discover its local socket addresses.
    tokio::time::sleep(Duration::from_millis(500)).await;

    let node_id = base32::encode(
        base32::Alphabet::Rfc4648Lower { padding: false },
        ep.id().as_bytes(),
    );

    let _session = advertise_peer(&ep, &service)?;

    println!("advertising _{service}._udp.local");
    println!("  node id : {node_id}");
    println!("  sockets : {:?}", ep.bound_sockets());
    println!("verify with: dns-sd -B _{service}._udp local");
    println!("press Ctrl-C to stop");

    tokio::signal::ctrl_c().await?;
    println!("\nstopping");
    Ok(())
}
