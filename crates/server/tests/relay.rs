mod common;

use std::time::Duration;

use common::{config, knock, open_room, room, room_names, rooms, start, Raw};
use proto::{ClientMsg, ServerMsg};
use server::ServerConfig;
use tokio_tungstenite::tungstenite::Message;

fn relay(to: Option<&str>, bytes: &[u8]) -> ClientMsg {
    ClientMsg::Relay {
        to: to.map(str::to_owned),
        bytes: bytes.to_vec(),
    }
}

fn relayed(from: &str, bytes: &[u8]) -> ServerMsg {
    ServerMsg::Relay {
        from: from.into(),
        bytes: bytes.to_vec(),
    }
}

#[tokio::test]
async fn broadcast_and_direct_relay() {
    let url = start(config()).await;
    let mut m = room(&url, "sala", &["a", "b", "c"]).await;
    let mut k = open_room(&url, "k", "other").await;
    let mut outsider = Raw::hello(&url, "outsider").await;

    m[0].send(relay(None, &[1, 2, 3])).await;
    assert_eq!(m[1].recv().await, relayed("a", &[1, 2, 3]));
    assert_eq!(m[2].recv().await, relayed("a", &[1, 2, 3]));
    m[0].recv_none().await;

    m[0].send(relay(Some("b"), &[4])).await;
    assert_eq!(m[1].recv().await, relayed("a", &[4]));
    m[2].recv_none().await;
    m[0].recv_none().await;

    // To oneself, to someone offline, to a non-member: dropped.
    for to in ["a", "nobody", "outsider", "k"] {
        m[0].send(relay(Some(to), &[5])).await;
    }
    for r in &mut m {
        r.recv_none().await;
    }
    k.recv_none().await;
    outsider.recv_none().await;
}

#[tokio::test]
async fn relay_from_is_true_sender() {
    let url = start(config()).await;
    let mut m = room(&url, "sala", &["a", "b", "c"]).await;
    m[1].send(relay(None, &[1])).await;
    assert_eq!(m[0].recv().await, relayed("b", &[1]));
    assert_eq!(m[2].recv().await, relayed("b", &[1]));
    m[2].send(relay(Some("a"), &[2])).await;
    assert_eq!(m[0].recv().await, relayed("c", &[2]));
    m[1].recv_none().await;
}

#[tokio::test]
async fn relay_is_byte_exact_and_tapped() {
    let (tap_tx, mut tap_rx) = tokio::sync::mpsc::unbounded_channel();
    let url = start(ServerConfig {
        relay_tap: Some(tap_tx),
        ..config()
    })
    .await;
    let mut m = room(&url, "sala", &["owner", "j"]).await;
    let payload: Vec<u8> = (0..272).map(|_| rand::random::<u8>()).collect();
    m[1].send(relay(None, &payload)).await;
    assert_eq!(m[0].recv().await, relayed("j", &payload));
    let tapped = tokio::time::timeout(Duration::from_secs(2), tap_rx.recv())
        .await
        .expect("tap timed out")
        .expect("tap closed");
    assert_eq!(tapped, payload);
    m[1].recv_none().await;
}

#[tokio::test]
async fn relay_from_non_member_is_dropped() {
    let url = start(config()).await;
    let mut owner = open_room(&url, "owner", "sala").await;
    let mut lone = Raw::hello(&url, "lone").await;
    let mut j = knock(&url, "j", "sala", &mut owner).await;
    lone.send(relay(None, &[9, 9, 9])).await;
    lone.send(relay(Some("owner"), &[9])).await;
    j.send(relay(Some("owner"), &[9])).await;
    j.send(relay(None, &[9])).await;
    lone.recv_none().await;
    j.recv_none().await;
    owner.recv_none().await;
    assert_eq!(room_names(&mut lone).await, ["sala"]);
}

#[tokio::test]
async fn member_disconnect_sends_member_left() {
    let url = start(config()).await;
    let mut m = room(&url, "sala", &["owner", "j"]).await;
    m.remove(1).close().await;
    assert_eq!(
        m[0].recv().await,
        ServerMsg::MemberLeft { nick: "j".into() }
    );
    // The owner's connection stays open and its nick stays taken.
    let mut dup = Raw::connect(&url).await;
    dup.send(ClientMsg::Hello {
        nick: "owner".into(),
        version: proto::VERSION.into(),
    })
    .await;
    assert_eq!(dup.recv().await, ServerMsg::NickTaken);
    let mut k = Raw::hello(&url, "k").await;
    assert_eq!(rooms(&mut k).await[0].members, 1);
    m[0].recv_none().await;
    m.remove(0).close().await;
    let _owner_again = Raw::hello_eventually(&url, "owner").await;
}

#[tokio::test]
async fn oversize_frame_closes() {
    let url = start(config()).await;
    let mut a = Raw::hello(&url, "a").await;
    a.send_raw(Message::Binary(vec![0u8; 9000].into())).await;
    a.closed().await;
}

#[tokio::test]
async fn garbage_frame_closes() {
    let url = start(config()).await;
    let mut a = Raw::hello(&url, "a").await;
    a.send_raw(Message::Binary(vec![0xff; 4].into())).await;
    a.closed().await;
}

#[tokio::test]
async fn text_frame_closes() {
    let url = start(config()).await;
    let mut a = Raw::hello(&url, "a").await;
    a.send_raw(Message::Text("hello".into())).await;
    a.closed().await;
}

#[tokio::test]
async fn rate_limit_closes() {
    let url = start(ServerConfig {
        rate_per_sec: 5,
        rate_burst: 5,
        ..config()
    })
    .await;
    let mut a = Raw::hello(&url, "a").await;
    for _ in 0..20 {
        a.send_lossy(ClientMsg::ListRooms).await;
    }
    a.drain_until_closed().await;
}

#[tokio::test]
async fn rate_limit_allows_steady_traffic() {
    let url = start(ServerConfig {
        rate_per_sec: 20,
        rate_burst: 5,
        ..config()
    })
    .await;
    let mut a = Raw::hello(&url, "a").await;
    for _ in 0..10 {
        assert!(room_names(&mut a).await.is_empty());
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test]
async fn unpaired_timeout_closes() {
    let url = start(ServerConfig {
        unpaired_timeout: Duration::from_millis(300),
        ..config()
    })
    .await;
    let mut idle = Raw::hello(&url, "idle").await;
    let mut owner = open_room(&url, "owner", "sala").await;
    // A pending request does not count as being in a room.
    let mut j = knock(&url, "j", "sala", &mut owner).await;
    idle.closed().await;
    j.closed().await;
}

#[tokio::test]
async fn room_members_outlive_unpaired_timeout() {
    let url = start(ServerConfig {
        unpaired_timeout: Duration::from_millis(500),
        ..config()
    })
    .await;
    let mut lone = open_room(&url, "lone", "empty").await;
    let mut m = room(&url, "sala", &["own", "j"]).await;
    tokio::time::sleep(Duration::from_millis(800)).await;
    m[1].send(relay(None, &[7])).await;
    assert_eq!(m[0].recv().await, relayed("j", &[7]));
    assert!(room_names(&mut lone).await.contains(&"sala".to_owned()));
}

#[tokio::test]
async fn connection_cap() {
    let url = start(ServerConfig {
        max_connections: 2,
        ..config()
    })
    .await;
    let _a = Raw::hello(&url, "a").await;
    let _b = Raw::hello(&url, "b").await;
    match tokio_tungstenite::connect_async(&url).await {
        Err(_) => {}
        Ok((ws, _)) => {
            let mut c = Raw::from_ws(ws);
            c.closed().await;
        }
    }
}

#[tokio::test]
async fn health_check_answers_ok() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let url = common::start(common::config()).await;
    let addr = url.trim_start_matches("ws://");
    let mut tcp = tokio::net::TcpStream::connect(addr).await.unwrap();
    tcp.write_all(b"GET /saude HTTP/1.1\r\nHost: x\r\n\r\n")
        .await
        .unwrap();
    let mut out = String::new();
    tcp.read_to_string(&mut out).await.unwrap();
    assert!(out.starts_with("HTTP/1.1 200 OK"), "{out}");
    assert!(out.ends_with("ok"));
}
