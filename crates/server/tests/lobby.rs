mod common;

use std::time::Duration;

use common::{admit, config, knock, open_room, room, room_names, rooms, start, Raw};
use proto::{
    decode, encode, ClientMsg, RoomInfo, ServerMsg, MAX_FRAME, MAX_LISTED_ROOMS, MAX_MEMBERS,
    MAX_NICK, MAX_ROOM_NAME,
};
use server::ServerConfig;

#[tokio::test]
async fn nick_ok_then_taken_then_freed() {
    let url = start(config()).await;
    let a = Raw::hello(&url, "user1").await;
    let mut b = Raw::connect(&url).await;
    b.send(ClientMsg::Hello {
        nick: "user1".into(),
    })
    .await;
    assert_eq!(b.recv().await, ServerMsg::NickTaken);
    b.closed().await;
    a.close().await;
    let _c = Raw::hello_eventually(&url, "user1").await;
}

#[tokio::test]
async fn invalid_nick() {
    let url = start(config()).await;
    let mut a = Raw::connect(&url).await;
    a.send(ClientMsg::Hello { nick: "a b".into() }).await;
    assert_eq!(a.recv().await, ServerMsg::NickInvalid);
    a.closed().await;
}

#[tokio::test]
async fn must_hello_first() {
    let url = start(config()).await;
    let mut a = Raw::connect(&url).await;
    a.send(ClientMsg::ListRooms).await;
    a.closed().await;
}

#[tokio::test]
async fn hello_timeout_closes() {
    let url = start(ServerConfig {
        hello_timeout: Duration::from_millis(200),
        ..config()
    })
    .await;
    let mut a = Raw::connect(&url).await;
    a.closed_within(Duration::from_secs(1)).await;
}

#[tokio::test]
async fn create_and_list_room() {
    let url = start(config()).await;
    let mut a = open_room(&url, "a", "zeta").await;
    let _b = open_room(&url, "b", "alfa").await;
    let mut c = Raw::hello(&url, "c").await;
    assert_eq!(
        rooms(&mut c).await,
        [
            RoomInfo {
                name: "alfa".into(),
                owner: "b".into(),
                members: 1,
                age_secs: 0,
            },
            RoomInfo {
                name: "zeta".into(),
                owner: "a".into(),
                members: 1,
                age_secs: 0,
            },
        ]
    );
    // The caller's own room is not listed.
    assert_eq!(room_names(&mut a).await, ["alfa"]);
    // Creating again while in a room is ignored.
    a.send(ClientMsg::CreateRoom {
        name: "other".into(),
    })
    .await;
    a.recv_none().await;
    assert_eq!(room_names(&mut c).await, ["alfa", "zeta"]);
}

#[tokio::test]
async fn room_name_taken_and_invalid() {
    let url = start(config()).await;
    let _a = open_room(&url, "a", "sala").await;
    let mut b = Raw::hello(&url, "b").await;
    b.send(ClientMsg::CreateRoom {
        name: "sala".into(),
    })
    .await;
    assert_eq!(b.recv().await, ServerMsg::RoomNameTaken);
    for bad in ["", "a b", "sala\x1b[2J", &"x".repeat(25)] {
        b.send(ClientMsg::CreateRoom { name: bad.into() }).await;
        assert_eq!(b.recv().await, ServerMsg::RoomNameInvalid, "{bad:?}");
    }
    // Still idle: a good name works.
    b.send(ClientMsg::CreateRoom {
        name: "sala-2".into(),
    })
    .await;
    assert_eq!(b.recv().await, ServerMsg::RoomCreated);
}

#[tokio::test]
async fn join_reaches_owner_and_room_stays_listed() {
    let url = start(config()).await;
    let mut owner = open_room(&url, "owner", "sala").await;
    let _j = knock(&url, "j", "sala", &mut owner).await;
    let mut k = Raw::hello(&url, "k").await;
    assert_eq!(room_names(&mut k).await, ["sala"]);
}

#[tokio::test]
async fn three_member_room() {
    let url = start(config()).await;
    // `room` asserts Joined (members in join order) and MemberJoined to everyone else.
    let mut m = room(&url, "sala", &["own", "m1", "m2"]).await;
    for r in &mut m {
        r.recv_none().await;
    }
    let mut k = Raw::hello(&url, "k").await;
    let listed = rooms(&mut k).await;
    assert_eq!(listed.len(), 1);
    assert_eq!(
        (
            listed[0].name.as_str(),
            listed[0].owner.as_str(),
            listed[0].members
        ),
        ("sala", "own", 3)
    );
    // Members do not see their own room.
    assert!(room_names(&mut m[2]).await.is_empty());
}

#[tokio::test]
async fn refuse_returns_joiner_and_keeps_room() {
    let url = start(config()).await;
    let mut owner = open_room(&url, "owner", "sala").await;
    let mut j = knock(&url, "j", "sala", &mut owner).await;
    owner.send(ClientMsg::Refuse { from: "j".into() }).await;
    assert_eq!(j.recv().await, ServerMsg::Refused);
    owner.recv_none().await;
    let mut k = Raw::hello(&url, "k").await;
    assert_eq!(room_names(&mut k).await, ["sala"]);
    // The refused joiner may knock again.
    j.send(ClientMsg::Join {
        room: "sala".into(),
    })
    .await;
    assert_eq!(
        owner.recv().await,
        ServerMsg::JoinRequest { from: "j".into() }
    );
}

#[tokio::test]
async fn request_times_out() {
    let url = start(ServerConfig {
        request_timeout: Duration::from_millis(300),
        ..config()
    })
    .await;
    let mut owner = open_room(&url, "owner", "sala").await;
    let mut j = knock(&url, "j", "sala", &mut owner).await;
    assert_eq!(j.recv().await, ServerMsg::Refused);
    assert_eq!(
        owner.recv().await,
        ServerMsg::RequestCancelled { from: "j".into() }
    );
    let mut k = Raw::hello(&url, "k").await;
    assert_eq!(room_names(&mut k).await, ["sala"]);
    // A late answer to the expired request is ignored.
    owner.send(ClientMsg::Accept { from: "j".into() }).await;
    owner.recv_none().await;
    j.recv_none().await;
}

#[tokio::test]
async fn stacked_requests() {
    let url = start(config()).await;
    let mut members = vec![open_room(&url, "owner", "sala").await];
    let mut names = vec!["owner".to_owned()];
    let j1 = knock(&url, "j1", "sala", &mut members[0]).await;
    let mut j2 = knock(&url, "j2", "sala", &mut members[0]).await;
    let mut k = Raw::hello(&url, "k").await;
    assert_eq!(room_names(&mut k).await, ["sala"]);

    members[0]
        .send(ClientMsg::Refuse { from: "j2".into() })
        .await;
    assert_eq!(j2.recv().await, ServerMsg::Refused);

    admit("sala", &mut members, &mut names, j1, "j1").await;
    // Not full: still listed, with two people.
    assert_eq!(rooms(&mut k).await[0].members, 2);
    j2.recv_none().await;
}

#[tokio::test]
async fn accept_not_pending_is_ignored() {
    let url = start(config()).await;
    let mut m = room(&url, "sala", &["owner", "m1"]).await;
    let mut j = knock(&url, "j", "sala", &mut m[0]).await;
    let mut other = Raw::hello(&url, "other").await;
    for from in ["other", "nobody", "m1"] {
        m[0].send(ClientMsg::Accept { from: from.into() }).await;
    }
    m[0].send(ClientMsg::Refuse {
        from: "other".into(),
    })
    .await;
    // Only the owner answers requests.
    m[1].send(ClientMsg::Accept { from: "j".into() }).await;
    m[1].send(ClientMsg::Refuse { from: "j".into() }).await;
    for r in &mut m {
        r.recv_none().await;
    }
    j.recv_none().await;
    other.recv_none().await;
    // The real request is still pending.
    m[0].send(ClientMsg::Accept { from: "j".into() }).await;
    assert!(matches!(j.recv().await, ServerMsg::Joined { .. }));
}

#[tokio::test]
async fn join_missing_room() {
    let url = start(config()).await;
    let mut j = Raw::hello(&url, "j").await;
    j.send(ClientMsg::Join {
        room: "nobody".into(),
    })
    .await;
    assert_eq!(j.recv().await, ServerMsg::RoomGone);
    // A nick is not a room.
    let _idle = Raw::hello(&url, "idle").await;
    j.send(ClientMsg::Join {
        room: "idle".into(),
    })
    .await;
    assert_eq!(j.recv().await, ServerMsg::RoomGone);
}

#[tokio::test]
async fn one_request_at_a_time_and_not_from_a_room() {
    let url = start(config()).await;
    let mut a = open_room(&url, "a", "sala-a").await;
    let mut b = open_room(&url, "b", "sala-b").await;
    let mut j = knock(&url, "j", "sala-a", &mut a).await;
    j.send(ClientMsg::Join {
        room: "sala-b".into(),
    })
    .await;
    // A member cannot knock elsewhere.
    a.send(ClientMsg::Join {
        room: "sala-b".into(),
    })
    .await;
    b.recv_none().await;
    j.recv_none().await;
    a.recv_none().await;
}

#[tokio::test]
async fn joiner_leaves_while_pending() {
    let url = start(config()).await;
    let mut owner = open_room(&url, "owner", "sala").await;
    let j = knock(&url, "j", "sala", &mut owner).await;
    j.close().await;
    assert_eq!(
        owner.recv().await,
        ServerMsg::RequestCancelled { from: "j".into() }
    );
    let _j_again = Raw::hello_eventually(&url, "j").await;
}

#[tokio::test]
async fn owner_leaves_oldest_inherits() {
    let url = start(config()).await;
    let mut m = room(&url, "sala", &["own", "m1", "m2", "m3"]).await;
    let own = m.remove(0);
    own.close().await;
    for r in &mut m {
        assert_eq!(r.recv().await, ServerMsg::MemberLeft { nick: "own".into() });
        assert_eq!(
            r.recv().await,
            ServerMsg::OwnerChanged { owner: "m1".into() }
        );
    }
    let mut k = Raw::hello(&url, "k").await;
    let listed = rooms(&mut k).await;
    assert_eq!((listed[0].owner.as_str(), listed[0].members), ("m1", 3));
    // Requests now go to the new owner.
    k.send(ClientMsg::Join {
        room: "sala".into(),
    })
    .await;
    assert_eq!(
        m[0].recv().await,
        ServerMsg::JoinRequest { from: "k".into() }
    );
    m[1].recv_none().await;
    m[0].send(ClientMsg::Accept { from: "k".into() }).await;
    assert_eq!(
        k.recv().await,
        ServerMsg::Joined {
            room: "sala".into(),
            owner: "m1".into(),
            members: vec!["m1".into(), "m2".into(), "m3".into(), "k".into()],
        }
    );
}

#[tokio::test]
async fn member_leaves_owner_stays() {
    let url = start(config()).await;
    let mut m = room(&url, "sala", &["own", "m1", "m2"]).await;
    let m1 = m.remove(1);
    m1.close().await;
    for r in &mut m {
        assert_eq!(r.recv().await, ServerMsg::MemberLeft { nick: "m1".into() });
        r.recv_none().await;
    }
}

#[tokio::test]
async fn owner_change_refuses_pending() {
    let url = start(config()).await;
    let mut m = room(&url, "sala", &["own", "m1"]).await;
    let mut j1 = knock(&url, "j1", "sala", &mut m[0]).await;
    let mut j2 = knock(&url, "j2", "sala", &mut m[0]).await;
    m.remove(0).close().await;
    assert_eq!(
        m[0].recv().await,
        ServerMsg::MemberLeft { nick: "own".into() }
    );
    assert_eq!(
        m[0].recv().await,
        ServerMsg::OwnerChanged { owner: "m1".into() }
    );
    assert_eq!(j1.recv().await, ServerMsg::Refused);
    assert_eq!(j2.recv().await, ServerMsg::Refused);
    m[0].recv_none().await;
    // They may knock again, now at the new owner.
    j1.send(ClientMsg::Join {
        room: "sala".into(),
    })
    .await;
    assert_eq!(
        m[0].recv().await,
        ServerMsg::JoinRequest { from: "j1".into() }
    );
}

#[tokio::test]
async fn last_member_leaves_deletes_room() {
    let url = start(config()).await;
    let mut m = room(&url, "sala", &["own", "m1"]).await;
    m.remove(1).close().await;
    assert_eq!(
        m[0].recv().await,
        ServerMsg::MemberLeft { nick: "m1".into() }
    );
    // Alone again: listed and waiting.
    let mut k = Raw::hello(&url, "k").await;
    assert_eq!(rooms(&mut k).await[0].members, 1);
    let mut j = knock(&url, "j", "sala", &mut m[0]).await;
    m.remove(0).close().await;
    assert_eq!(j.recv().await, ServerMsg::Refused);
    assert!(room_names(&mut k).await.is_empty());
    // The name is free again.
    k.send(ClientMsg::CreateRoom {
        name: "sala".into(),
    })
    .await;
    assert_eq!(k.recv().await, ServerMsg::RoomCreated);
    let _own_again = Raw::hello_eventually(&url, "own").await;
}

/// Nicks `own`, `m1`, `m2`, ... for a room of `n` people.
fn nicks(n: usize) -> Vec<String> {
    std::iter::once("own".to_owned())
        .chain((1..n).map(|i| format!("m{i}")))
        .collect()
}

#[tokio::test]
async fn full_room_not_listed_and_join_gone() {
    let url = start(config()).await;
    let names = nicks(MAX_MEMBERS);
    let names: Vec<&str> = names.iter().map(String::as_str).collect();
    let mut m = room(&url, "sala", &names).await;
    let _other = open_room(&url, "other", "other").await;
    let mut k = Raw::hello(&url, "k").await;
    assert_eq!(room_names(&mut k).await, ["other"]);
    k.send(ClientMsg::Join {
        room: "sala".into(),
    })
    .await;
    assert_eq!(k.recv().await, ServerMsg::RoomGone);
    m[0].recv_none().await;
    // Someone leaves: listed again.
    m.pop().unwrap().close().await;
    assert_eq!(
        m[0].recv().await,
        ServerMsg::MemberLeft {
            nick: format!("m{}", MAX_MEMBERS - 1)
        }
    );
    assert_eq!(room_names(&mut k).await, ["other", "sala"]);
}

#[tokio::test]
async fn full_room_refuses_pending() {
    let url = start(config()).await;
    let names = nicks(MAX_MEMBERS - 1);
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let mut m = room(&url, "sala", &refs).await;
    let mut names = names;
    let j1 = knock(&url, "j1", "sala", &mut m[0]).await;
    let mut j2 = knock(&url, "j2", "sala", &mut m[0]).await;
    let mut j3 = knock(&url, "j3", "sala", &mut m[0]).await;
    admit("sala", &mut m, &mut names, j1, "j1").await;
    assert_eq!(j2.recv().await, ServerMsg::Refused);
    assert_eq!(j3.recv().await, ServerMsg::Refused);
    let mut cancelled = vec![m[0].recv().await, m[0].recv().await];
    cancelled.sort_by_key(|c| format!("{c:?}"));
    assert_eq!(
        cancelled,
        [
            ServerMsg::RequestCancelled { from: "j2".into() },
            ServerMsg::RequestCancelled { from: "j3".into() },
        ]
    );
    m[0].recv_none().await;
}

#[tokio::test]
async fn long_room_list_is_capped_to_the_oldest() {
    let url = start(config()).await;
    let total = 150;
    let mut owners = Vec::with_capacity(total);
    // Names run backwards so name order and age order disagree.
    let name = |i: usize| format!("r{:0>width$}", total - 1 - i, width = MAX_ROOM_NAME - 1);
    for i in 0..total {
        let nick = format!("o{:0>width$}", i, width = MAX_NICK - 1);
        owners.push(open_room(&url, &nick, &name(i)).await);
    }
    let mut c = Raw::hello(&url, "c").await;
    let listed = rooms(&mut c).await;
    let frame = encode(&ServerMsg::Rooms(listed.clone()));
    assert!(frame.len() <= MAX_FRAME, "{} bytes", frame.len());
    assert_eq!(
        decode::<ServerMsg>(&frame).unwrap(),
        ServerMsg::Rooms(listed.clone())
    );
    let mut oldest: Vec<String> = (0..MAX_LISTED_ROOMS).map(name).collect();
    oldest.sort();
    let names: Vec<String> = listed.into_iter().map(|r| r.name).collect();
    assert_eq!(names, oldest);
}
