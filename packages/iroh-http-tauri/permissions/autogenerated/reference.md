## Default Permission

Node lifecycle and introspection. Covers createNode() and close(). Add iroh-http:fetch and/or iroh-http:serve for HTTP capability.

#### This default permission set includes the following:

- `allow-create-endpoint`
- `allow-close-endpoint`
- `allow-wait-endpoint-closed`
- `allow-ping`
- `allow-node-addr`
- `allow-node-ticket`
- `allow-home-relay`
- `allow-peer-info`
- `allow-peer-stats`
- `allow-endpoint-stats`
- `allow-start-transport-events`

## Permission Table

<table>
<tr>
<th>Identifier</th>
<th>Description</th>
</tr>


<tr>
<td>

`iroh-http:allow-cancel-in-flight`

</td>
<td>

Enables the cancel_in_flight command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`iroh-http:deny-cancel-in-flight`

</td>
<td>

Denies the cancel_in_flight command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`iroh-http:allow-cancel-request`

</td>
<td>

Enables the cancel_request command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`iroh-http:deny-cancel-request`

</td>
<td>

Denies the cancel_request command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`iroh-http:allow-close-endpoint`

</td>
<td>

Enables the close_endpoint command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`iroh-http:deny-close-endpoint`

</td>
<td>

Denies the close_endpoint command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`iroh-http:allow-create-body-writer`

</td>
<td>

Enables the create_body_writer command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`iroh-http:deny-create-body-writer`

</td>
<td>

Denies the create_body_writer command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`iroh-http:allow-create-endpoint`

</td>
<td>

Enables the create_endpoint command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`iroh-http:deny-create-endpoint`

</td>
<td>

Denies the create_endpoint command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`iroh-http:allow-create-fetch-token`

</td>
<td>

Enables the create_fetch_token command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`iroh-http:deny-create-fetch-token`

</td>
<td>

Denies the create_fetch_token command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`iroh-http:allow-endpoint-stats`

</td>
<td>

Enables the endpoint_stats command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`iroh-http:deny-endpoint-stats`

</td>
<td>

Denies the endpoint_stats command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`iroh-http:allow-fetch`

</td>
<td>

Enables the fetch command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`iroh-http:deny-fetch`

</td>
<td>

Denies the fetch command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`iroh-http:allow-finish-body`

</td>
<td>

Enables the finish_body command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`iroh-http:deny-finish-body`

</td>
<td>

Denies the finish_body command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`iroh-http:allow-generate-secret-key`

</td>
<td>

Enables the generate_secret_key command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`iroh-http:deny-generate-secret-key`

</td>
<td>

Denies the generate_secret_key command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`iroh-http:allow-home-relay`

</td>
<td>

Enables the home_relay command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`iroh-http:deny-home-relay`

</td>
<td>

Denies the home_relay command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`iroh-http:allow-mdns-advertise`

</td>
<td>

Enables the mdns_advertise command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`iroh-http:deny-mdns-advertise`

</td>
<td>

Denies the mdns_advertise command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`iroh-http:allow-mdns-advertise-close`

</td>
<td>

Enables the mdns_advertise_close command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`iroh-http:deny-mdns-advertise-close`

</td>
<td>

Denies the mdns_advertise_close command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`iroh-http:allow-mdns-browse`

</td>
<td>

Enables the mdns_browse command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`iroh-http:deny-mdns-browse`

</td>
<td>

Denies the mdns_browse command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`iroh-http:allow-mdns-browse-close`

</td>
<td>

Enables the mdns_browse_close command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`iroh-http:deny-mdns-browse-close`

</td>
<td>

Denies the mdns_browse_close command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`iroh-http:allow-mdns-next-event`

</td>
<td>

Enables the mdns_next_event command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`iroh-http:deny-mdns-next-event`

</td>
<td>

Denies the mdns_next_event command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`iroh-http:allow-next-chunk`

</td>
<td>

Enables the next_chunk command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`iroh-http:deny-next-chunk`

</td>
<td>

Denies the next_chunk command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`iroh-http:allow-node-addr`

</td>
<td>

Enables the node_addr command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`iroh-http:deny-node-addr`

</td>
<td>

Denies the node_addr command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`iroh-http:allow-node-ticket`

</td>
<td>

Enables the node_ticket command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`iroh-http:deny-node-ticket`

</td>
<td>

Denies the node_ticket command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`iroh-http:allow-peer-info`

</td>
<td>

Enables the peer_info command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`iroh-http:deny-peer-info`

</td>
<td>

Denies the peer_info command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`iroh-http:allow-peer-stats`

</td>
<td>

Enables the peer_stats command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`iroh-http:deny-peer-stats`

</td>
<td>

Denies the peer_stats command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`iroh-http:allow-ping`

</td>
<td>

Enables the ping command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`iroh-http:deny-ping`

</td>
<td>

Denies the ping command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`iroh-http:allow-public-key-verify`

</td>
<td>

Enables the public_key_verify command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`iroh-http:deny-public-key-verify`

</td>
<td>

Denies the public_key_verify command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`iroh-http:allow-respond-to-request`

</td>
<td>

Enables the respond_to_request command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`iroh-http:deny-respond-to-request`

</td>
<td>

Denies the respond_to_request command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`iroh-http:allow-secret-key-sign`

</td>
<td>

Enables the secret_key_sign command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`iroh-http:deny-secret-key-sign`

</td>
<td>

Denies the secret_key_sign command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`iroh-http:allow-send-chunk`

</td>
<td>

Enables the send_chunk command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`iroh-http:deny-send-chunk`

</td>
<td>

Denies the send_chunk command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`iroh-http:allow-serve`

</td>
<td>

Enables the serve command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`iroh-http:deny-serve`

</td>
<td>

Denies the serve command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`iroh-http:allow-session-accept`

</td>
<td>

Enables the session_accept command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`iroh-http:deny-session-accept`

</td>
<td>

Denies the session_accept command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`iroh-http:allow-session-close`

</td>
<td>

Enables the session_close command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`iroh-http:deny-session-close`

</td>
<td>

Denies the session_close command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`iroh-http:allow-session-closed`

</td>
<td>

Enables the session_closed command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`iroh-http:deny-session-closed`

</td>
<td>

Denies the session_closed command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`iroh-http:allow-session-connect`

</td>
<td>

Enables the session_connect command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`iroh-http:deny-session-connect`

</td>
<td>

Denies the session_connect command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`iroh-http:allow-session-create-bidi-stream`

</td>
<td>

Enables the session_create_bidi_stream command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`iroh-http:deny-session-create-bidi-stream`

</td>
<td>

Denies the session_create_bidi_stream command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`iroh-http:allow-session-create-uni-stream`

</td>
<td>

Enables the session_create_uni_stream command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`iroh-http:deny-session-create-uni-stream`

</td>
<td>

Denies the session_create_uni_stream command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`iroh-http:allow-session-max-datagram-size`

</td>
<td>

Enables the session_max_datagram_size command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`iroh-http:deny-session-max-datagram-size`

</td>
<td>

Denies the session_max_datagram_size command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`iroh-http:allow-session-next-bidi-stream`

</td>
<td>

Enables the session_next_bidi_stream command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`iroh-http:deny-session-next-bidi-stream`

</td>
<td>

Denies the session_next_bidi_stream command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`iroh-http:allow-session-next-uni-stream`

</td>
<td>

Enables the session_next_uni_stream command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`iroh-http:deny-session-next-uni-stream`

</td>
<td>

Denies the session_next_uni_stream command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`iroh-http:allow-session-recv-datagram`

</td>
<td>

Enables the session_recv_datagram command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`iroh-http:deny-session-recv-datagram`

</td>
<td>

Denies the session_recv_datagram command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`iroh-http:allow-session-send-datagram`

</td>
<td>

Enables the session_send_datagram command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`iroh-http:deny-session-send-datagram`

</td>
<td>

Denies the session_send_datagram command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`iroh-http:allow-start-transport-events`

</td>
<td>

Enables the start_transport_events command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`iroh-http:deny-start-transport-events`

</td>
<td>

Denies the start_transport_events command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`iroh-http:allow-stop-serve`

</td>
<td>

Enables the stop_serve command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`iroh-http:deny-stop-serve`

</td>
<td>

Denies the stop_serve command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`iroh-http:allow-try-next-chunk`

</td>
<td>

Enables the try_next_chunk command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`iroh-http:deny-try-next-chunk`

</td>
<td>

Denies the try_next_chunk command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`iroh-http:allow-wait-endpoint-closed`

</td>
<td>

Enables the wait_endpoint_closed command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`iroh-http:deny-wait-endpoint-closed`

</td>
<td>

Denies the wait_endpoint_closed command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`iroh-http:allow-wait-serve-stop`

</td>
<td>

Enables the wait_serve_stop command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`iroh-http:deny-wait-serve-stop`

</td>
<td>

Denies the wait_serve_stop command without any pre-configured scope.

</td>
</tr>

<tr>
<td>

`iroh-http:connect`

</td>
<td>

Enables raw QUIC sessions — bidirectional streams and datagrams directly to a peer, outside of HTTP framing.

</td>
</tr>

<tr>
<td>

`iroh-http:crypto`

</td>
<td>

Enables low-level key operations — generate keypairs, sign with a secret key, and verify public key signatures.

</td>
</tr>

<tr>
<td>

`iroh-http:fetch`

</td>
<td>

Enables node.fetch() — send HTTP requests to remote peers. Includes all internal body-streaming primitives required for fetch to function.

</td>
</tr>

<tr>
<td>

`iroh-http:mdns`

</td>
<td>

Enables local peer discovery via mDNS — browse for peers on the local network and advertise this node.

</td>
</tr>

<tr>
<td>

`iroh-http:serve`

</td>
<td>

Enables node.serve() — accept incoming HTTP requests from remote peers. Includes all internal body-streaming primitives required for handlers to read and write bodies.

</td>
</tr>
</table>
