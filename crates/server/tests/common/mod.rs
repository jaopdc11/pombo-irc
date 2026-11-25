//! Shared helpers for the server integration tests.
#![allow(dead_code)]

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use proto::{decode, encode, ClientMsg, RoomInfo, ServerMsg};
use server::{serve, ServerConfig};
use tokio::net::TcpStream;
use tokio::time::{timeout, Instant};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

const RECV_TIMEOUT: Duration = Duration::from_secs(2);
const NONE_WINDOW: Duration = Duration::from_millis(300);

/// Binds `127.0.0.1:0`, spawns `serve` and returns its `ws://` URL.
pub async fn start(cfg: ServerConfig) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(serve(listener, cfg));
    format!("ws://{addr}")
}

/// A raw WebSocket client speaking the wire protocol.
pub struct Raw {
    ws: Ws,
}

impl Raw {
    pub async fn connect(url: &str) -> Raw {
        let (ws, _) = connect_async(url).await.expect("connect");
        Raw { ws }
    }

    pub fn from_ws(ws: Ws) -> Raw {
        Raw { ws }
    }

    /// Connects and greets with `nick`, expecting `NickOk`.
    pub async fn hello(url: &str, nick: &str) -> Raw {
        let mut raw = Raw::connect(url).await;
        raw.send(ClientMsg::Hello {
            nick: nick.into(),
            version: proto::VERSION.into(),
        })
        .await;
        assert_eq!(raw.recv().await, ServerMsg::NickOk, "hello {nick}");
        raw
    }

    /// Like `hello`, but retries while the nick is still held by a
    /// connection the server has not finished cleaning up.
    pub async fn hello_eventually(url: &str, nick: &str) -> Raw {
        let deadline = Instant::now() + RECV_TIMEOUT;
        loop {
            let mut raw = Raw::connect(url).await;
            raw.send(ClientMsg::Hello {
                nick: nick.into(),
                version: proto::VERSION.into(),
            })
            .await;
            match raw.recv().await {
                ServerMsg::NickOk => return raw,
                ServerMsg::NickTaken if Instant::now() < deadline => {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                other => panic!("hello {nick}: unexpected {other:?}"),
            }
        }
    }

    pub async fn send(&mut self, msg: ClientMsg) {
        self.ws
            .send(Message::Binary(encode(&msg).into()))
            .await
            .expect("send");
    }

    /// Sends a message ignoring errors (the server may already have closed).
    pub async fn send_lossy(&mut self, msg: ClientMsg) {
        let _ = self.ws.send(Message::Binary(encode(&msg).into())).await;
    }

    pub async fn send_raw(&mut self, msg: Message) {
        let _ = self.ws.send(msg).await;
    }

    /// Next server message within 2 s; panics on timeout or close.
    pub async fn recv(&mut self) -> ServerMsg {
        let deadline = Instant::now() + RECV_TIMEOUT;
        loop {
            let next = tokio::time::timeout_at(deadline, self.ws.next())
                .await
                .expect("timed out waiting for a server message");
            match next {
                Some(Ok(Message::Binary(b))) => return decode(&b).expect("decode"),
                Some(Ok(Message::Ping(_) | Message::Pong(_))) => continue,
                other => panic!("connection ended while waiting: {other:?}"),
            }
        }
    }

    /// Asserts that nothing arrives (and the connection stays open) for 300 ms.
    pub async fn recv_none(&mut self) {
        let deadline = Instant::now() + NONE_WINDOW;
        loop {
            match tokio::time::timeout_at(deadline, self.ws.next()).await {
                Err(_) => return,
                Ok(Some(Ok(Message::Ping(_) | Message::Pong(_)))) => continue,
                Ok(Some(Ok(Message::Binary(b)))) => {
                    panic!("unexpected message: {:?}", decode::<ServerMsg>(&b))
                }
                Ok(other) => panic!("connection ended unexpectedly: {other:?}"),
            }
        }
    }

    /// Asserts the server closes the connection within `within`, with no
    /// further protocol message.
    pub async fn closed_within(&mut self, within: Duration) {
        self.wait_closed(within, false).await;
    }

    /// Asserts the server closes the connection within 2 s, with no further
    /// protocol message.
    pub async fn closed(&mut self) {
        self.wait_closed(RECV_TIMEOUT, false).await;
    }

    /// Asserts the server closes the connection within 2 s, discarding any
    /// messages that arrive first.
    pub async fn drain_until_closed(&mut self) {
        self.wait_closed(RECV_TIMEOUT, true).await;
    }

    async fn wait_closed(&mut self, within: Duration, discard: bool) {
        let deadline = Instant::now() + within;
        loop {
            match tokio::time::timeout_at(deadline, self.ws.next()).await {
                Err(_) => panic!("connection still open after {within:?}"),
                Ok(None | Some(Err(_)) | Some(Ok(Message::Close(_)))) => return,
                Ok(Some(Ok(Message::Binary(b)))) if !discard => {
                    panic!("unexpected message: {:?}", decode::<ServerMsg>(&b))
                }
                Ok(Some(Ok(_))) => continue,
            }
        }
    }

    /// Closes the connection from the client side.
    pub async fn close(mut self) {
        let _ = timeout(Duration::from_secs(1), self.ws.close(None)).await;
    }
}

/// Lists rooms as `raw` sees them.
pub async fn rooms(raw: &mut Raw) -> Vec<RoomInfo> {
    raw.send(ClientMsg::ListRooms).await;
    match raw.recv().await {
        ServerMsg::Rooms(rooms) => rooms,
        other => panic!("expected Rooms, got {other:?}"),
    }
}

/// Lists rooms and returns just the names, in server order.
pub async fn room_names(raw: &mut Raw) -> Vec<String> {
    rooms(raw).await.into_iter().map(|r| r.name).collect()
}

/// Greets as `owner` and creates the room `name`, expecting `RoomCreated`.
pub async fn open_room(url: &str, owner: &str, name: &str) -> Raw {
    let mut o = Raw::hello(url, owner).await;
    o.send(ClientMsg::CreateRoom { name: name.into() }).await;
    assert_eq!(o.recv().await, ServerMsg::RoomCreated, "create {name}");
    o
}

/// Greets as `nick` and knocks on `room`; asserts the owner sees the request.
pub async fn knock(url: &str, nick: &str, room: &str, owner: &mut Raw) -> Raw {
    let mut j = Raw::hello(url, nick).await;
    j.send(ClientMsg::Join { room: room.into() }).await;
    assert_eq!(
        owner.recv().await,
        ServerMsg::JoinRequest { from: nick.into() }
    );
    j
}

/// The owner (`members[0]`) accepts the pending `nick`; asserts the joiner's
/// `Joined` and every current member's `MemberJoined`, then adds the joiner
/// to `members` and `names`.
pub async fn admit(
    room: &str,
    members: &mut Vec<Raw>,
    names: &mut Vec<String>,
    mut joiner: Raw,
    nick: &str,
) {
    members[0]
        .send(ClientMsg::Accept { from: nick.into() })
        .await;
    names.push(nick.to_owned());
    assert_eq!(
        joiner.recv().await,
        ServerMsg::Joined {
            room: room.into(),
            owner: names[0].clone(),
            members: names.clone(),
        }
    );
    for m in members.iter_mut() {
        assert_eq!(
            m.recv().await,
            ServerMsg::MemberJoined { nick: nick.into() }
        );
    }
    members.push(joiner);
}

/// Builds the room `name` owned by `nicks[0]` with the others accepted in
/// order. Returns the connections in the same order.
pub async fn room(url: &str, name: &str, nicks: &[&str]) -> Vec<Raw> {
    let mut members = vec![open_room(url, nicks[0], name).await];
    let mut names = vec![nicks[0].to_owned()];
    for &nick in &nicks[1..] {
        let j = knock(url, nick, name, &mut members[0]).await;
        admit(name, &mut members, &mut names, j, nick).await;
    }
    members
}

/// Default config, as the tests' baseline.
pub fn config() -> ServerConfig {
    ServerConfig::default()
}
