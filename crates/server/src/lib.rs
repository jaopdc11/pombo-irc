//! pombo-irc server: keeps named group rooms and forwards opaque bytes between their members.
//!
//! Everything lives in memory. The server never logs connections, nicks,
//! addresses or payloads.

mod conn;
mod hub;

use std::io;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::net::TcpListener;
use tokio::sync::mpsc;

/// Server limits and timeouts.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// How long a join request waits for the owner's answer.
    pub request_timeout: Duration,
    /// How long a new connection has to complete the handshake and send `Hello`.
    pub hello_timeout: Duration,
    /// How long a connection may stay open without being in a room.
    pub unpaired_timeout: Duration,
    /// Concurrent connections; sockets beyond this are closed on accept.
    pub max_connections: usize,
    /// Incoming frames per second allowed per connection.
    pub rate_per_sec: u32,
    /// Token bucket size for incoming frames.
    pub rate_burst: u32,
    /// Test hook: receives a copy of every forwarded `Relay` payload.
    #[doc(hidden)]
    pub relay_tap: Option<mpsc::UnboundedSender<Vec<u8>>>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        ServerConfig {
            request_timeout: Duration::from_secs(proto::REQUEST_TIMEOUT_SECS),
            hello_timeout: Duration::from_secs(10),
            unpaired_timeout: Duration::from_secs(60 * 60),
            max_connections: 512,
            rate_per_sec: 30,
            rate_burst: 60,
            relay_tap: None,
        }
    }
}

/// Decrements the live connection count when a connection task ends.
struct Slot(Arc<AtomicUsize>);

impl Drop for Slot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Serves WebSocket clients on `listener`. Runs until the listener fails.
pub async fn serve(listener: TcpListener, config: ServerConfig) -> io::Result<()> {
    let hub = Arc::new(Mutex::new(hub::Hub::new(config.relay_tap.clone())));
    let config = Arc::new(config);
    let live = Arc::new(AtomicUsize::new(0));
    loop {
        let tcp = match listener.accept().await {
            Ok((tcp, _addr)) => tcp,
            Err(e) if is_transient(&e) => {
                tokio::time::sleep(Duration::from_millis(50)).await;
                continue;
            }
            Err(e) => return Err(e),
        };
        if live.load(Ordering::Relaxed) >= config.max_connections {
            drop(tcp);
            continue;
        }
        live.fetch_add(1, Ordering::Relaxed);
        let slot = Slot(live.clone());
        let _ = tcp.set_nodelay(true);
        let hub = hub.clone();
        let config = config.clone();
        tokio::spawn(async move {
            let _slot = slot;
            conn::run(tcp, hub, config).await;
        });
    }
}

/// Accept errors that concern one socket or a passing resource shortage,
/// not the listener itself.
fn is_transient(e: &io::Error) -> bool {
    use io::ErrorKind::*;
    matches!(
        e.kind(),
        ConnectionAborted | ConnectionReset | ConnectionRefused | Interrupted | WouldBlock
    ) || matches!(
        e.raw_os_error(),
        // ENOMEM, ENFILE, EMFILE, ENOBUFS (Linux)
        Some(12 | 23 | 24 | 105)
    )
}
