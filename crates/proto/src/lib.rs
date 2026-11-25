//! Wire protocol shared by the pombo-irc server and client.
//!
//! Every frame is one binary WebSocket message holding a single
//! `postcard`-encoded [`ClientMsg`] or [`ServerMsg`].

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

/// Largest WebSocket message either side accepts, in bytes.
pub const MAX_FRAME: usize = 8192;
/// Plaintext is padded to a multiple of this many bytes before encryption.
pub const PAD_BLOCK: usize = 256;
/// Largest chat line, in bytes of UTF-8.
pub const MAX_LINE: usize = 4000;
/// Seconds a join request waits for the owner before it counts as refused.
pub const REQUEST_TIMEOUT_SECS: u64 = 60;
/// Longest nick, in characters.
pub const MAX_NICK: usize = 16;
/// Most people in one room, owner included.
pub const MAX_MEMBERS: usize = 8;
/// Longest room name, in characters.
pub const MAX_ROOM_NAME: usize = 24;
/// Most rooms in one `Rooms` listing (the oldest ones), so the frame stays under `MAX_FRAME`.
pub const MAX_LISTED_ROOMS: usize = 100;

/// One open room as listed to people looking for one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomInfo {
    pub name: String,
    pub owner: String,
    /// People in the room, owner included.
    pub members: u8,
    /// Seconds since the room was created.
    pub age_secs: u64,
}

/// Messages a client sends to the server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClientMsg {
    Hello {
        nick: String,
    },
    CreateRoom {
        name: String,
    },
    ListRooms,
    Join {
        room: String,
    },
    /// Owner accepts the pending request from `from`.
    Accept {
        from: String,
    },
    /// Owner refuses the pending request from `from`.
    Refuse {
        from: String,
    },
    /// Opaque end-to-end payload for one member (`to`) or the whole room
    /// (`None`); the server forwards it untouched.
    Relay {
        to: Option<String>,
        bytes: Vec<u8>,
    },
}

/// Messages the server sends to a client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ServerMsg {
    NickOk,
    NickTaken,
    NickInvalid,
    RoomCreated,
    RoomNameTaken,
    RoomNameInvalid,
    Rooms(Vec<RoomInfo>),
    JoinRequest {
        from: String,
    },
    /// Owner side: the request from `from` was withdrawn, timed out or can
    /// no longer be accepted.
    RequestCancelled {
        from: String,
    },
    Refused,
    RoomGone,
    /// Joiner side: accepted into `room`. `members` is in join order,
    /// oldest first, and includes the joiner.
    Joined {
        room: String,
        owner: String,
        members: Vec<String>,
    },
    MemberJoined {
        nick: String,
    },
    MemberLeft {
        nick: String,
    },
    OwnerChanged {
        owner: String,
    },
    /// Opaque end-to-end payload from the member `from`.
    Relay {
        from: String,
        bytes: Vec<u8>,
    },
}

/// A frame that could not be decoded.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("malformed frame: {0}")]
pub struct ProtoError(String);

/// Encodes a message into one frame.
pub fn encode<T: Serialize>(msg: &T) -> Vec<u8> {
    postcard::to_stdvec(msg).expect("protocol types always serialize")
}

/// Decodes one frame. Trailing bytes are rejected.
pub fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, ProtoError> {
    match postcard::take_from_bytes::<T>(bytes) {
        Ok((msg, [])) => Ok(msg),
        Ok(_) => Err(ProtoError("trailing bytes".into())),
        Err(e) => Err(ProtoError(e.to_string())),
    }
}

/// A nick is 1 to 16 characters of `[A-Za-z0-9_-]`.
pub fn valid_nick(nick: &str) -> bool {
    valid_word(nick, MAX_NICK)
}

/// A room name is 1 to 24 characters of `[A-Za-z0-9_-]`.
pub fn valid_room_name(name: &str) -> bool {
    valid_word(name, MAX_ROOM_NAME)
}

fn valid_word(s: &str, max: usize) -> bool {
    (1..=max).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip<T>(msg: T)
    where
        T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug,
    {
        let bytes = encode(&msg);
        assert_eq!(decode::<T>(&bytes).unwrap(), msg);
    }

    #[test]
    fn roundtrip_every_variant() {
        for m in [
            ClientMsg::Hello {
                nick: "user1".into(),
            },
            ClientMsg::CreateRoom {
                name: "sala".into(),
            },
            ClientMsg::ListRooms,
            ClientMsg::Join {
                room: "sala".into(),
            },
            ClientMsg::Accept {
                from: "user2".into(),
            },
            ClientMsg::Refuse {
                from: "user2".into(),
            },
            ClientMsg::Relay {
                to: None,
                bytes: vec![0u8; 300],
            },
            ClientMsg::Relay {
                to: Some("user2".into()),
                bytes: vec![7u8; 3],
            },
        ] {
            roundtrip(m);
        }
        for m in [
            ServerMsg::NickOk,
            ServerMsg::NickTaken,
            ServerMsg::NickInvalid,
            ServerMsg::RoomCreated,
            ServerMsg::RoomNameTaken,
            ServerMsg::RoomNameInvalid,
            ServerMsg::Rooms(vec![
                RoomInfo {
                    name: "a".into(),
                    owner: "user1".into(),
                    members: 1,
                    age_secs: 3,
                },
                RoomInfo {
                    name: "sala-2".into(),
                    owner: "user2".into(),
                    members: 7,
                    age_secs: 3600,
                },
            ]),
            ServerMsg::JoinRequest {
                from: "user2".into(),
            },
            ServerMsg::RequestCancelled {
                from: "user2".into(),
            },
            ServerMsg::Refused,
            ServerMsg::RoomGone,
            ServerMsg::Joined {
                room: "sala".into(),
                owner: "user1".into(),
                members: vec!["user1".into(), "user2".into()],
            },
            ServerMsg::MemberJoined {
                nick: "user3".into(),
            },
            ServerMsg::MemberLeft {
                nick: "user3".into(),
            },
            ServerMsg::OwnerChanged {
                owner: "user2".into(),
            },
            ServerMsg::Relay {
                from: "user1".into(),
                bytes: vec![0u8; 300],
            },
        ] {
            roundtrip(m);
        }
    }

    #[test]
    fn decode_garbage_is_error() {
        assert!(decode::<ClientMsg>(&[0xff, 0xff, 0xff]).is_err());
        assert!(decode::<ServerMsg>(&[]).is_err());
    }

    #[test]
    fn nick_rules() {
        assert!(valid_nick("a"));
        assert!(valid_nick("user1"));
        assert!(valid_nick("a_b-C9"));
        assert!(valid_nick(&"x".repeat(16)));
        assert!(!valid_nick(""));
        assert!(!valid_nick(&"x".repeat(17)));
        assert!(!valid_nick("Zé"));
        assert!(!valid_nick("a b"));
        assert!(!valid_nick("\x1b[31m"));
    }

    #[test]
    fn room_name_rules() {
        assert!(valid_room_name("a"));
        assert!(valid_room_name("sala_dos-Pombos9"));
        assert!(valid_room_name(&"x".repeat(MAX_ROOM_NAME)));
        assert!(!valid_room_name(""));
        assert!(!valid_room_name(&"x".repeat(MAX_ROOM_NAME + 1)));
        assert!(!valid_room_name("sala dos pombos"));
        assert!(!valid_room_name("ção"));
        assert!(!valid_room_name("\x1b[31m"));
    }
}
