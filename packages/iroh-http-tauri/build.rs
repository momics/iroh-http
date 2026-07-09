fn main() {
    tauri_plugin::Builder::new(&[
        // Endpoint lifecycle
        "create_endpoint",
        "close_endpoint",
        "ping",
        "wait_endpoint_closed",
        // Address introspection
        "node_addr",
        "node_ticket",
        "home_relay",
        "peer_info",
        "peer_stats",
        "endpoint_stats",
        // HTTP client (internal streaming primitives bundled into iroh-http:fetch)
        "fetch",
        "create_fetch_token",
        "cancel_in_flight",
        "cancel_request",
        "create_body_writer",
        "next_chunk",
        "try_next_chunk",
        "send_chunk",
        "finish_body",
        // HTTP server (internal primitives bundled into iroh-http:serve)
        "serve",
        "stop_serve",
        "wait_serve_stop",
        "respond_to_request",
        // Session / duplex streams (iroh-http:connect)
        "session_connect",
        "session_accept",
        "session_create_bidi_stream",
        "session_next_bidi_stream",
        "session_close",
        "session_closed",
        "session_create_uni_stream",
        "session_next_uni_stream",
        "session_send_datagram",
        "session_recv_datagram",
        "session_max_datagram_size",
        // Crypto utilities (iroh-http:crypto)
        "secret_key_sign",
        "public_key_verify",
        "generate_secret_key",
        // Local-network discovery — iroh peer (iroh-http:discovery)
        "browse_peers",
        "browse_peers_next",
        "browse_peers_close",
        "advertise_peer",
        "advertise_peer_close",
        // Local-network discovery — generic DNS-SD (iroh-http:discovery)
        "advertise",
        "advertise_close",
        "browse",
        "browse_next",
        "browse_close",
        // Transport / path-change events (bundled into iroh-http:default)
        "start_transport_events",
        "next_path_change",
        "unsubscribe_path_changes",
    ])
    .android_path("android")
    .ios_path("ios")
    .build();
}
