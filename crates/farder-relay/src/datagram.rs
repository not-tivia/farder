//! Voice-datagram forwarding and routing for relayed connections (Phase 5a).
//!
//! The relay forwards encrypted media datagrams BLIND between a server and its
//! relayed clients, using a per-client `u32` routing handle:
//!   - forward (client -> relay -> server): prefix each datagram with the
//!     source client's handle (4 bytes, big-endian).
//!   - route   (server -> relay -> client): read the destination handle prefix,
//!     strip it, and deliver the payload to that client's connection.

use crate::router::SharedState;
use quinn::Connection;
use tracing::debug;

/// Forward every datagram the client sends to the destination server, tagged
/// with the client's routing handle. Ends when the client connection closes.
pub async fn forward_client_datagrams(client_conn: Connection, server_conn: Connection, handle: u32) {
    loop {
        match client_conn.read_datagram().await {
            Ok(dg) => {
                let mut tagged = Vec::with_capacity(4 + dg.len());
                tagged.extend_from_slice(&handle.to_be_bytes());
                tagged.extend_from_slice(&dg);
                // Best-effort: drop if the server can't take datagrams.
                if let Err(e) = server_conn.send_datagram(tagged.into()) {
                    debug!("drop forwarded datagram (handle {}): {}", handle, e);
                }
            }
            Err(_) => break, // client gone
        }
    }
}

/// Route every datagram the server sends to the client named by its 4-byte
/// big-endian handle prefix. Unknown/closed handles are dropped. Ends when the
/// server connection closes.
pub async fn route_server_datagrams(server_conn: Connection, state: SharedState) {
    loop {
        match server_conn.read_datagram().await {
            Ok(dg) => {
                if dg.len() < 4 {
                    continue; // malformed; no handle prefix
                }
                let handle = u32::from_be_bytes([dg[0], dg[1], dg[2], dg[3]]);
                let payload = dg.slice(4..);
                let client = { state.clients.read().await.get(&handle).cloned() };
                if let Some(c) = client {
                    if let Err(e) = c.send_datagram(payload) {
                        debug!("drop routed datagram (handle {}): {}", handle, e);
                    }
                }
            }
            Err(_) => break, // server gone
        }
    }
}
