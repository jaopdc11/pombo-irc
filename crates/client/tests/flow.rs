//! End-to-end flows against a real in-process server.

use std::time::Duration;

use pombo_irc::core::{Conn, ConnectError, CoreError, CreateError, LobbyEvent, Room, RoomEvent};
use proto::{MAX_LINE, PAD_BLOCK};
use server::ServerConfig;
use tokio::net::TcpListener;
use tokio::sync::mpsc;

const WAIT: Duration = Duration::from_secs(5);

async fn start(tap: Option<mpsc::UnboundedSender<Vec<u8>>>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let config = ServerConfig {
        relay_tap: tap,
        ..ServerConfig::default()
    };
    tokio::spawn(server::serve(listener, config));
    format!("ws://{addr}")
}

/// Skips events until one matches `pred`, which is returned.
async fn until(room: &mut Room, pred: impl Fn(&RoomEvent) -> bool) -> RoomEvent {
    tokio::time::timeout(WAIT, async {
        loop {
            let ev = room.next().await.expect("room event");
            if pred(&ev) {
                return ev;
            }
        }
    })
    .await
    .expect("event in time")
}

fn is_rekeyed(ev: &RoomEvent) -> bool {
    matches!(ev, RoomEvent::Rekeyed { .. })
}

async fn create(url: &str, nick: &str, name: &str) -> Room {
    let conn = Conn::connect(url, nick).await.unwrap();
    match conn.create_room(name).await {
        Ok(room) => room,
        Err((_, e)) => panic!("create_room: {e}"),
    }
}

/// `nick` knocks on the owner's room, gets accepted and enters. The owner is driven until the
/// newcomer's handshake rekeyed the room.
async fn join(url: &str, owner: &mut Room, nick: &str) -> Room {
    let mut conn = Conn::connect(url, nick).await.unwrap();
    conn.join(owner.name()).await.unwrap();
    let req = until(owner, |e| matches!(e, RoomEvent::JoinRequest { .. })).await;
    assert_eq!(
        req,
        RoomEvent::JoinRequest {
            from: nick.to_owned()
        }
    );
    owner.accept(nick).await.unwrap();
    let joined = conn.next_event().await.unwrap();
    let LobbyEvent::Joined { room, owner: o, .. } = &joined else {
        panic!("expected Joined, got {joined:?}");
    };
    assert_eq!(room, owner.name());
    assert_eq!(o, owner.me());
    let (entered, ev) = tokio::join!(conn.enter(joined), async {
        let joined = until(owner, |e| matches!(e, RoomEvent::MemberJoined { .. })).await;
        until(owner, is_rekeyed).await;
        joined
    });
    let room = entered.unwrap();
    let RoomEvent::MemberJoined {
        nick: n,
        fingerprint,
    } = ev
    else {
        unreachable!()
    };
    assert_eq!(n, nick);
    assert_eq!(fingerprint.as_deref(), room.fingerprint());
    room
}

/// Drains `room` until it is on `epoch`.
async fn settle(room: &mut Room, epoch: u32) {
    while room.epoch() != Some(epoch) {
        until(room, is_rekeyed).await;
    }
}

fn line(from: &str, text: &str) -> impl Fn(&RoomEvent) -> bool {
    let (from, text) = (from.to_owned(), text.to_owned());
    move |e| matches!(e, RoomEvent::Line { from: f, text: t } if *f == from && t.as_str() == text)
}

fn is_line(e: &RoomEvent) -> bool {
    matches!(e, RoomEvent::Line { .. })
}

async fn three(url: &str) -> (Room, Room, Room) {
    let mut a = create(url, "user1", "sala").await;
    let mut b = join(url, &mut a, "user2").await;
    let c = join(url, &mut a, "user3").await;
    settle(&mut b, 2).await;
    (a, b, c)
}

fn assert_fingerprint(fp: &str) {
    let groups: Vec<&str> = fp.split('-').collect();
    assert_eq!(groups.len(), 4, "{fp}");
    for g in groups {
        assert_eq!(g.len(), 4);
        assert!(g
            .chars()
            .all(|c| c.is_ascii_digit() || ('A'..='F').contains(&c)));
    }
}

#[tokio::test]
async fn three_people_chat() {
    let url = start(None).await;
    let (mut a, mut b, mut c) = three(&url).await;
    assert!(a.is_owner() && !b.is_owner() && !c.is_owner());
    assert_eq!(a.fingerprint(), None);
    assert_fingerprint(b.fingerprint().unwrap());
    assert_fingerprint(c.fingerprint().unwrap());
    assert_ne!(b.fingerprint(), c.fingerprint());
    assert_eq!(c.members(), ["user1", "user2", "user3"]);
    assert_eq!(b.members(), ["user1", "user2", "user3"]);
    assert_eq!(c.owner(), "user1");

    c.send_line("oi").await.unwrap();
    until(&mut a, line("user3", "oi")).await;
    until(&mut b, line("user3", "oi")).await;

    b.send_typing().await.unwrap();
    let typing = |e: &RoomEvent| matches!(e, RoomEvent::Typing { from } if from == "user2");
    until(&mut a, typing).await;
    until(&mut c, typing).await;

    a.send_line("tá na rede do cliente? 🐦").await.unwrap();
    until(&mut b, line("user1", "tá na rede do cliente? 🐦")).await;
    until(&mut c, line("user1", "tá na rede do cliente? 🐦")).await;
}

#[tokio::test]
async fn rekey_on_join_and_leave() {
    let url = start(None).await;
    let mut a = create(&url, "user1", "sala").await;
    assert_eq!(a.epoch(), Some(0));
    let mut b = join(&url, &mut a, "user2").await;
    assert_eq!((a.epoch(), b.epoch()), (Some(1), Some(1)));
    let c = join(&url, &mut a, "user3").await;
    settle(&mut b, 2).await;
    assert_eq!((a.epoch(), c.epoch()), (Some(2), Some(2)));

    c.leave().await;
    until(
        &mut a,
        |e| matches!(e, RoomEvent::MemberLeft { nick } if nick == "user3"),
    )
    .await;
    assert_eq!(
        until(&mut a, is_rekeyed).await,
        RoomEvent::Rekeyed { epoch: 3 }
    );
    assert_eq!(
        until(&mut b, is_rekeyed).await,
        RoomEvent::Rekeyed { epoch: 3 }
    );
    assert_eq!(b.members(), ["user1", "user2"]);
}

#[tokio::test]
async fn newcomer_cannot_open_old_message() {
    let (tap_tx, mut tap) = mpsc::unbounded_channel();
    let url = start(Some(tap_tx)).await;
    let mut a = create(&url, "user1", "sala").await;
    let mut b = join(&url, &mut a, "user2").await;
    while tap.try_recv().is_ok() {}

    b.send_line("antes").await.unwrap();
    until(&mut a, line("user2", "antes")).await;
    let old = tap.recv().await.unwrap();
    assert!(!a.can_open("user2", &old), "a replayed message is rejected");

    let c = join(&url, &mut a, "user3").await;
    assert!(
        !c.can_open("user2", &old),
        "newcomer cannot read older messages"
    );
}

#[tokio::test]
async fn owner_leaves_and_chat_continues() {
    let url = start(None).await;
    let (a, mut b, mut c) = three(&url).await;
    let old_fp = c.fingerprint().unwrap().to_owned();
    a.leave().await;

    let owner_changed =
        |e: &RoomEvent| matches!(e, RoomEvent::OwnerChanged { owner } if owner == "user2");
    let is_fp = |e: &RoomEvent| matches!(e, RoomEvent::Fingerprint { .. });
    let (eb, ec) = tokio::join!(
        async {
            until(
                &mut b,
                |e| matches!(e, RoomEvent::MemberLeft { nick } if nick == "user1"),
            )
            .await;
            until(&mut b, owner_changed).await;
            // The new owner never seals with the departed owner's key: it made its own already.
            assert_eq!(b.epoch(), Some(3));
            let rekeyed = until(&mut b, is_rekeyed).await;
            (rekeyed, until(&mut b, is_fp).await)
        },
        async {
            until(&mut c, owner_changed).await;
            // The departed owner's key is gone; nothing is sent until the new key arrives.
            assert_eq!(c.epoch(), None);
            assert_eq!(c.send_line("cedo").await, Err(CoreError::NotReady));
            let fp = until(&mut c, is_fp).await;
            (until(&mut c, is_rekeyed).await, fp)
        }
    );
    assert_eq!(eb.0, RoomEvent::Rekeyed { epoch: 3 });
    assert_eq!(ec.0, RoomEvent::Rekeyed { epoch: 3 });
    let RoomEvent::Fingerprint { nick, fingerprint } = eb.1 else {
        unreachable!()
    };
    assert_eq!(nick, "user3");
    assert_eq!(Some(fingerprint.as_str()), c.fingerprint());
    let RoomEvent::Fingerprint { nick, fingerprint } = ec.1 else {
        unreachable!()
    };
    assert_eq!(nick, "user2");
    assert_eq!(Some(fingerprint.as_str()), c.fingerprint());
    let (eb, ec) = (eb.0, ec.0);
    assert_eq!(eb, RoomEvent::Rekeyed { epoch: 3 });
    assert_eq!(ec, RoomEvent::Rekeyed { epoch: 3 });
    assert!(b.is_owner());
    assert_eq!(b.fingerprint(), None);
    assert_eq!(c.owner(), "user2");
    assert_eq!(c.members(), ["user2", "user3"]);
    let new_fp = c.fingerprint().unwrap();
    assert_fingerprint(new_fp);
    assert_ne!(new_fp, old_fp);

    c.send_line("seguimos").await.unwrap();
    until(&mut b, line("user3", "seguimos")).await;
    b.send_line("sim").await.unwrap();
    until(&mut c, line("user2", "sim")).await;
}

#[tokio::test]
async fn second_handshake_from_member_is_ignored() {
    let url = start(None).await;
    let mut a = create(&url, "user1", "sala").await;
    let mut b = join(&url, &mut a, "user2").await;
    let fp = b.fingerprint().unwrap().to_owned();
    assert_eq!((a.epoch(), b.epoch()), (Some(1), Some(1)));
    until(&mut b, is_rekeyed).await;

    // What a relay injecting a handshake "from user2" would deliver to the owner.
    b.restart_handshake().unwrap();
    let quiet = tokio::time::timeout(Duration::from_millis(300), a.next()).await;
    assert!(quiet.is_err(), "owner reacted: {quiet:?}");
    let quiet = tokio::time::timeout(Duration::from_millis(300), b.next()).await;
    assert!(quiet.is_err(), "member got something: {quiet:?}");
    assert_eq!((a.epoch(), b.epoch()), (Some(1), Some(1)));
    assert_eq!(b.fingerprint(), Some(fp.as_str()));

    b.send_line("ainda aqui").await.unwrap();
    until(&mut a, line("user2", "ainda aqui")).await;
}

#[tokio::test]
async fn server_never_sees_plaintext() {
    let (tap_tx, mut tap) = mpsc::unbounded_channel();
    let url = start(Some(tap_tx)).await;
    let (mut a, mut b, mut c) = three(&url).await;
    let secret = "segredo-canario-123";
    b.send_line(secret).await.unwrap();
    until(&mut a, line("user2", secret)).await;
    until(&mut c, line("user2", secret)).await;
    a.send_line(&format!("{secret} {}", "x".repeat(300)))
        .await
        .unwrap();
    until(&mut b, is_line).await;

    let mut payloads = Vec::new();
    while let Ok(p) = tap.try_recv() {
        payloads.push(p);
    }
    assert!(payloads.len() > 4);
    let mut room_msgs = 0;
    for p in &payloads {
        assert!(
            !p.windows(secret.len()).any(|w| w == secret.as_bytes()),
            "plaintext leaked to the server"
        );
        if p[0] == 2 {
            room_msgs += 1;
            // tag ‖ epoch ‖ nonce ‖ padded ciphertext ‖ tag
            let body = p.len() - 1 - 4 - 24 - 16;
            assert_eq!(body % PAD_BLOCK, 0, "room payload not padded: {}", p.len());
        }
    }
    assert!(room_msgs >= 2);
}

#[tokio::test]
async fn refused_and_gone() {
    let url = start(None).await;
    let mut a = create(&url, "user1", "sala").await;
    let mut conn = Conn::connect(&url, "user2").await.unwrap();
    conn.join("sala").await.unwrap();
    until(
        &mut a,
        |e| matches!(e, RoomEvent::JoinRequest { from } if from == "user2"),
    )
    .await;
    a.refuse("user2").await.unwrap();
    assert_eq!(conn.next_event().await.unwrap(), LobbyEvent::Refused);

    conn.join("nope").await.unwrap();
    assert_eq!(conn.next_event().await.unwrap(), LobbyEvent::RoomGone);

    let rooms = conn.list_rooms().await.unwrap();
    assert_eq!(rooms.len(), 1);
    assert_eq!(
        (rooms[0].name.as_str(), rooms[0].owner.as_str()),
        ("sala", "user1")
    );
}

#[tokio::test]
async fn room_name_taken() {
    let url = start(None).await;
    let _a = create(&url, "user1", "sala").await;
    let conn = Conn::connect(&url, "user2").await.unwrap();
    let Err((conn, e)) = conn.create_room("sala").await else {
        panic!("duplicate name accepted");
    };
    assert_eq!(e, CreateError::NameTaken);
    let Err((conn, e)) = conn.create_room("sala dos pombos").await else {
        panic!("invalid name accepted");
    };
    assert_eq!(e, CreateError::NameInvalid);
    let room = conn.create_room("outra").await.ok().unwrap();
    assert!(room.is_owner());
    assert_eq!(room.members(), ["user2"]);

    assert!(matches!(
        Conn::connect(&url, "user1").await,
        Err(ConnectError::NickTaken)
    ));
    assert!(matches!(
        Conn::connect("ws://127.0.0.1:1", "a b").await,
        Err(ConnectError::NickInvalid)
    ));
}

#[tokio::test]
async fn line_too_long_sends_nothing() {
    let url = start(None).await;
    let mut a = create(&url, "user1", "sala").await;
    let mut b = join(&url, &mut a, "user2").await;
    assert_eq!(
        b.send_line(&"x".repeat(MAX_LINE + 1)).await,
        Err(CoreError::TooLong)
    );
    let quiet = tokio::time::timeout(Duration::from_millis(300), a.next()).await;
    assert!(quiet.is_err(), "something arrived: {quiet:?}");
    b.send_line(&"x".repeat(MAX_LINE)).await.unwrap();
    until(&mut a, is_line).await;
}

#[tokio::test]
async fn server_dies_mid_chat() {
    let (rt, addr) = std::thread::spawn(|| {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let listener = rt.block_on(TcpListener::bind("127.0.0.1:0")).unwrap();
        let addr = listener.local_addr().unwrap();
        rt.spawn(server::serve(listener, ServerConfig::default()));
        (rt, addr)
    })
    .join()
    .unwrap();
    let url = format!("ws://{addr}");
    let mut a = create(&url, "user1", "sala").await;
    let mut b = join(&url, &mut a, "user2").await;
    b.send_line("oi").await.unwrap();
    until(&mut a, line("user2", "oi")).await;

    rt.shutdown_background();
    for room in [&mut a, &mut b] {
        let ev = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let ev = room.next().await.unwrap();
                if ev == RoomEvent::ConnectionLost {
                    return ev;
                }
            }
        })
        .await
        .expect("connection lost in time");
        assert_eq!(ev, RoomEvent::ConnectionLost);
        assert_eq!(room.next().await, Err(CoreError::Disconnected));
        assert_eq!(room.send_line("depois").await, Err(CoreError::NotReady));
    }
}
