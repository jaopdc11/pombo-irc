//! Per-connection task: WebSocket handshake, hello, limits, ping and the
//! read/write loop. Logs nothing.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use proto::{decode, encode, valid_nick, ClientMsg, ServerMsg, MAX_FRAME};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::time::{interval_at, sleep_until, timeout, timeout_at, Instant};
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{accept_async_with_config, WebSocketStream};

use crate::hub::{ConnId, Hub, OUTBOX};
use crate::ServerConfig;

/// Interval between server pings; a ping unanswered for this long closes.
const PING_INTERVAL: Duration = Duration::from_secs(30);
/// Longest a single write to the socket may take.
const SEND_TIMEOUT: Duration = Duration::from_secs(30);
/// Longest the closing handshake may take.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(1);

type Ws = WebSocketStream<TcpStream>;
type Sink = SplitSink<Ws, Message>;
type Stream = SplitStream<Ws>;

pub(crate) fn lock(hub: &Mutex<Hub>) -> MutexGuard<'_, Hub> {
    hub.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Token bucket over incoming frames.
struct Bucket {
    tokens: f64,
    rate: f64,
    burst: f64,
    last: Instant,
}

impl Bucket {
    fn new(rate: u32, burst: u32) -> Self {
        Bucket {
            tokens: f64::from(burst),
            rate: f64::from(rate),
            burst: f64::from(burst),
            last: Instant::now(),
        }
    }

    fn take(&mut self) -> bool {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last).as_secs_f64();
        self.last = now;
        self.tokens = (self.tokens + elapsed * self.rate).min(self.burst);
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

/// Runs disconnect cleanup in the hub even if the task is dropped.
struct Registered {
    hub: Arc<Mutex<Hub>>,
    id: ConnId,
}

impl Drop for Registered {
    fn drop(&mut self) {
        lock(&self.hub).remove(self.id);
    }
}

pub(crate) async fn run(tcp: TcpStream, hub: Arc<Mutex<Hub>>, cfg: Arc<ServerConfig>) {
    let start = Instant::now();
    let hello_deadline = start + cfg.hello_timeout;
    let mut tcp = tcp;
    match timeout_at(hello_deadline, is_health_check(&tcp)).await {
        Ok(Some(true)) => return answer_health_check(&mut tcp).await,
        Ok(Some(false)) => {}
        _ => return,
    }
    let ws_config = WebSocketConfig::default()
        .max_message_size(Some(MAX_FRAME))
        .max_frame_size(Some(MAX_FRAME));
    let Ok(Ok(ws)) = timeout_at(
        hello_deadline,
        accept_async_with_config(tcp, Some(ws_config)),
    )
    .await
    else {
        return;
    };
    let (mut sink, mut stream) = ws.split();
    let mut bucket = Bucket::new(cfg.rate_per_sec, cfg.rate_burst);

    let Some((nick, version)) = read_hello(&mut stream, &mut bucket, hello_deadline).await else {
        return finish(sink, None).await;
    };
    if version != proto::VERSION {
        return finish(sink, Some(ServerMsg::UpdateRequired)).await;
    }
    if !valid_nick(&nick) {
        return finish(sink, Some(ServerMsg::NickInvalid)).await;
    }
    let (tx, mut rx) = mpsc::channel(OUTBOX);
    let Some(id) = lock(&hub).register(nick, tx) else {
        return finish(sink, Some(ServerMsg::NickTaken)).await;
    };
    let guard = Registered {
        hub: hub.clone(),
        id,
    };

    let mut ping = interval_at(start + PING_INTERVAL, PING_INTERVAL);
    let mut awaiting_pong = false;
    let roomless_deadline = start + cfg.unpaired_timeout;
    let mut roomless_armed = true;

    loop {
        tokio::select! {
            incoming = stream.next() => {
                let Some(Ok(msg)) = incoming else { break };
                if !bucket.take() {
                    break;
                }
                match msg {
                    Message::Binary(bytes) => {
                        let Ok(msg) = decode::<ClientMsg>(&bytes) else { break };
                        let mut h = lock(&hub);
                        if let Some(seq) = h.handle(id, msg) {
                            // Spawned under the lock so the hub owns the abort
                            // handle before anything can settle the request.
                            let hub = hub.clone();
                            let after = cfg.request_timeout;
                            let task = tokio::spawn(async move {
                                tokio::time::sleep(after).await;
                                lock(&hub).request_timeout(id, seq);
                            });
                            h.set_timer(id, seq, task.abort_handle());
                        }
                        drop(h);
                    }
                    Message::Pong(_) => awaiting_pong = false,
                    Message::Ping(_) => {}
                    Message::Text(_) | Message::Close(_) | Message::Frame(_) => break,
                }
            }
            out = rx.recv() => {
                let Some(msg) = out else { break };
                if !send(&mut sink, Message::Binary(encode(&msg).into())).await {
                    break;
                }
            }
            _ = ping.tick() => {
                if awaiting_pong {
                    break;
                }
                awaiting_pong = true;
                if !send(&mut sink, Message::Ping(Vec::new().into())).await {
                    break;
                }
            }
            // Members leave a room only by disconnecting, so one check suffices.
            _ = sleep_until(roomless_deadline), if roomless_armed => {
                if lock(&hub).in_room(id) {
                    roomless_armed = false;
                } else {
                    break;
                }
            }
        }
    }
    drop(guard);
    finish(sink, None).await;
}

/// Reads frames until the first protocol message; it must be `Hello`.
async fn read_hello(
    stream: &mut Stream,
    bucket: &mut Bucket,
    deadline: Instant,
) -> Option<(String, String)> {
    loop {
        let Ok(Some(Ok(msg))) = timeout_at(deadline, stream.next()).await else {
            return None;
        };
        if !bucket.take() {
            return None;
        }
        match msg {
            Message::Binary(bytes) => {
                return match decode::<ClientMsg>(&bytes) {
                    Ok(ClientMsg::Hello { nick, version }) => Some((nick, version)),
                    _ => None,
                };
            }
            Message::Ping(_) | Message::Pong(_) => continue,
            _ => return None,
        }
    }
}

/// Writes one message within `SEND_TIMEOUT`.
async fn send(sink: &mut Sink, msg: Message) -> bool {
    matches!(timeout(SEND_TIMEOUT, sink.send(msg)).await, Ok(Ok(())))
}

/// Sends an optional last message, then closes the socket.
async fn finish(mut sink: Sink, last: Option<ServerMsg>) {
    let _ = timeout(CLOSE_TIMEOUT, async {
        if let Some(msg) = last {
            sink.send(Message::Binary(encode(&msg).into())).await?;
        }
        sink.close().await
    })
    .await;
}

/// Path answered with a plain `200 ok` for the deploy health check.
const HEALTH_REQUEST: &[u8] = b"GET /saude ";

/// Peeks at the first bytes: `Some(true)` for a health check, `Some(false)`
/// for anything else (the WebSocket handshake), `None` if the peer closed.
async fn is_health_check(tcp: &TcpStream) -> Option<bool> {
    let mut buf = [0u8; HEALTH_REQUEST.len()];
    loop {
        let n = tcp.peek(&mut buf).await.ok()?;
        if n == 0 {
            return None;
        }
        if buf[..n] != HEALTH_REQUEST[..n] {
            return Some(false);
        }
        if n == buf.len() {
            return Some(true);
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

async fn answer_health_check(tcp: &mut TcpStream) {
    let mut discard = [0u8; 1024];
    let _ = timeout(Duration::from_secs(1), tcp.read(&mut discard)).await;
    let reply = b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok";
    let _ = timeout(Duration::from_secs(1), tcp.write_all(reply)).await;
    let _ = tcp.shutdown().await;
}
