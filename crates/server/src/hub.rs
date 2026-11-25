//! In-memory room and relay hub.
//!
//! All state lives in one `Hub` behind a mutex. Every method is a plain state
//! transition that queues the resulting messages on the affected connections'
//! outboxes; nothing here blocks or awaits.

use std::collections::{BTreeMap, HashMap};
use std::time::Instant;

use proto::{valid_room_name, ClientMsg, RoomInfo, ServerMsg, MAX_LISTED_ROOMS, MAX_MEMBERS};
use tokio::sync::mpsc;
use tokio::task::AbortHandle;

pub(crate) type ConnId = u64;

/// Messages a connection may have queued before it counts as stuck.
pub(crate) const OUTBOX: usize = 512;

enum State {
    /// Greeted, in no room and not knocking.
    Idle,
    /// One outgoing join request to the room `room`. `timer` is its
    /// timeout task, aborted once the request is settled.
    Requesting {
        room: String,
        seq: u64,
        timer: Option<AbortHandle>,
    },
    /// Member of the room `room`.
    InRoom { room: String },
}

struct Entry {
    nick: String,
    /// `None` once the outbox overflowed; the connection then closes.
    tx: Option<mpsc::Sender<ServerMsg>>,
    state: State,
}

struct Room {
    created: Instant,
    /// Creation order; breaks ties between rooms created in the same instant.
    serial: u64,
    /// Join order, oldest first. Never empty; `members[0]` is the owner.
    members: Vec<ConnId>,
    /// Stack of `(joiner, request seq)`, most recent last.
    pending: Vec<(ConnId, u64)>,
}

impl Room {
    fn owner(&self) -> ConnId {
        self.members[0]
    }

    fn is_full(&self) -> bool {
        self.members.len() >= MAX_MEMBERS
    }
}

pub(crate) struct Hub {
    next_id: ConnId,
    next_seq: u64,
    next_room: u64,
    conns: HashMap<ConnId, Entry>,
    nicks: HashMap<String, ConnId>,
    /// Open rooms by name; the ordering gives the sorted room list.
    rooms: BTreeMap<String, Room>,
    tap: Option<mpsc::UnboundedSender<Vec<u8>>>,
}

impl Hub {
    pub(crate) fn new(tap: Option<mpsc::UnboundedSender<Vec<u8>>>) -> Self {
        Hub {
            next_id: 0,
            next_seq: 0,
            next_room: 0,
            conns: HashMap::new(),
            nicks: HashMap::new(),
            rooms: BTreeMap::new(),
            tap,
        }
    }

    /// Claims `nick` for a new connection and queues `NickOk`.
    /// Returns `None` when the nick is taken.
    pub(crate) fn register(&mut self, nick: String, tx: mpsc::Sender<ServerMsg>) -> Option<ConnId> {
        if self.nicks.contains_key(&nick) {
            return None;
        }
        let id = self.next_id;
        self.next_id += 1;
        self.nicks.insert(nick.clone(), id);
        self.conns.insert(
            id,
            Entry {
                nick,
                tx: Some(tx),
                state: State::Idle,
            },
        );
        self.send(id, ServerMsg::NickOk);
        Some(id)
    }

    /// Handles one message from a greeted connection. Returns the sequence
    /// number of a join request whose timeout the caller must schedule.
    pub(crate) fn handle(&mut self, id: ConnId, msg: ClientMsg) -> Option<u64> {
        match msg {
            ClientMsg::Hello { .. } => {}
            ClientMsg::CreateRoom { name } => self.create(id, name),
            ClientMsg::ListRooms => {
                let rooms = self.rooms_for(id);
                self.send(id, ServerMsg::Rooms(rooms));
            }
            ClientMsg::Join { room } => return self.join(id, &room),
            ClientMsg::Accept { from } => self.accept(id, &from),
            ClientMsg::Refuse { from } => self.refuse(id, &from),
            ClientMsg::Relay { to, bytes } => self.relay(id, to.as_deref(), bytes),
        }
        None
    }

    /// The request `seq` from `joiner` expired without an answer.
    pub(crate) fn request_timeout(&mut self, joiner: ConnId, seq: u64) {
        let Some(e) = self.conns.get(&joiner) else {
            return;
        };
        let State::Requesting { room, seq: s, .. } = &e.state else {
            return;
        };
        if *s != seq {
            return;
        }
        let (room, from) = (room.clone(), e.nick.clone());
        self.set_state(joiner, State::Idle);
        self.send(joiner, ServerMsg::Refused);
        if let Some(owner) = self.unstack(&room, joiner) {
            self.send(owner, ServerMsg::RequestCancelled { from });
        }
    }

    /// Attaches the timeout task of the request `seq` from `joiner`. Aborts
    /// it right away if that request is no longer outstanding.
    pub(crate) fn set_timer(&mut self, joiner: ConnId, seq: u64, timer: AbortHandle) {
        match self.conns.get_mut(&joiner).map(|e| &mut e.state) {
            Some(State::Requesting {
                seq: s, timer: t, ..
            }) if *s == seq => *t = Some(timer),
            _ => timer.abort(),
        }
    }

    /// Whether the connection is a member of a room.
    pub(crate) fn in_room(&self, id: ConnId) -> bool {
        matches!(
            self.conns.get(&id).map(|e| &e.state),
            Some(State::InRoom { .. })
        )
    }

    /// Disconnect cleanup: frees the nick, cancels the outgoing request or
    /// leaves the room, handing ownership on or deleting the room.
    pub(crate) fn remove(&mut self, id: ConnId) {
        let Some(entry) = self.conns.remove(&id) else {
            return;
        };
        self.nicks.remove(&entry.nick);
        match entry.state {
            State::Idle => {}
            State::Requesting { room, timer, .. } => {
                if let Some(timer) = timer {
                    timer.abort();
                }
                if let Some(owner) = self.unstack(&room, id) {
                    self.send(owner, ServerMsg::RequestCancelled { from: entry.nick });
                }
            }
            State::InRoom { room: name } => self.leave(id, entry.nick, name),
        }
    }

    /// Removes the already deregistered member `id` (called `nick`) from `name`.
    fn leave(&mut self, id: ConnId, nick: String, name: String) {
        let Some(room) = self.rooms.get_mut(&name) else {
            return;
        };
        let was_owner = room.owner() == id;
        room.members.retain(|&m| m != id);
        if room.members.is_empty() {
            let room = self.rooms.remove(&name).expect("room exists");
            self.refuse_all(room.pending);
            return;
        }
        let members = room.members.clone();
        let pending = if was_owner {
            std::mem::take(&mut room.pending)
        } else {
            Vec::new()
        };
        for &m in &members {
            self.send(m, ServerMsg::MemberLeft { nick: nick.clone() });
        }
        if was_owner {
            let owner = self.nick(members[0]);
            for &m in &members {
                self.send(
                    m,
                    ServerMsg::OwnerChanged {
                        owner: owner.clone(),
                    },
                );
            }
            self.refuse_all(pending);
        }
    }

    fn create(&mut self, id: ConnId, name: String) {
        if !matches!(self.conns.get(&id).map(|e| &e.state), Some(State::Idle)) {
            return;
        }
        if !valid_room_name(&name) {
            self.send(id, ServerMsg::RoomNameInvalid);
            return;
        }
        if self.rooms.contains_key(&name) {
            self.send(id, ServerMsg::RoomNameTaken);
            return;
        }
        self.rooms.insert(
            name.clone(),
            Room {
                created: Instant::now(),
                serial: self.next_room,
                members: vec![id],
                pending: Vec::new(),
            },
        );
        self.next_room += 1;
        self.set_state(id, State::InRoom { room: name });
        self.send(id, ServerMsg::RoomCreated);
    }

    /// Open rooms `id` could join: the `MAX_LISTED_ROOMS` oldest, sorted by name.
    fn rooms_for(&self, id: ConnId) -> Vec<RoomInfo> {
        let mut open: Vec<(&String, &Room)> = self
            .rooms
            .iter()
            .filter(|(_, r)| !r.is_full() && !r.members.contains(&id))
            .collect();
        if open.len() > MAX_LISTED_ROOMS {
            open.sort_unstable_by_key(|(_, r)| r.serial);
            open.truncate(MAX_LISTED_ROOMS);
            open.sort_unstable_by_key(|(name, _)| *name);
        }
        open.into_iter()
            .map(|(name, r)| RoomInfo {
                name: name.clone(),
                owner: self.nick(r.owner()),
                members: u8::try_from(r.members.len()).unwrap_or(u8::MAX),
                age_secs: r.created.elapsed().as_secs(),
            })
            .collect()
    }

    fn join(&mut self, id: ConnId, name: &str) -> Option<u64> {
        let from = match self.conns.get(&id) {
            Some(e) if matches!(e.state, State::Idle) => e.nick.clone(),
            _ => return None,
        };
        let seq = self.next_seq;
        let owner = match self.rooms.get_mut(name) {
            Some(room) if !room.is_full() => {
                room.pending.push((id, seq));
                room.owner()
            }
            _ => {
                self.send(id, ServerMsg::RoomGone);
                return None;
            }
        };
        self.next_seq += 1;
        self.set_state(
            id,
            State::Requesting {
                room: name.to_owned(),
                seq,
                timer: None,
            },
        );
        self.send(owner, ServerMsg::JoinRequest { from });
        Some(seq)
    }

    /// The room `owner` owns and its pending joiner called `from`, if any.
    fn pending_joiner(&self, owner: ConnId, from: &str) -> Option<(String, ConnId)> {
        let joiner = *self.nicks.get(from)?;
        let State::InRoom { room: name } = &self.conns.get(&owner)?.state else {
            return None;
        };
        let room = self.rooms.get(name)?;
        (room.owner() == owner && room.pending.iter().any(|&(j, _)| j == joiner))
            .then(|| (name.clone(), joiner))
    }

    fn accept(&mut self, owner: ConnId, from: &str) {
        let Some((name, joiner)) = self.pending_joiner(owner, from) else {
            return;
        };
        let room = self.rooms.get_mut(&name).expect("pending_joiner checked");
        room.pending.retain(|&(j, _)| j != joiner);
        room.members.push(joiner);
        let members = room.members.clone();
        let rest = if room.is_full() {
            std::mem::take(&mut room.pending)
        } else {
            Vec::new()
        };
        self.set_state(joiner, State::InRoom { room: name.clone() });
        let nicks: Vec<String> = members.iter().map(|&m| self.nick(m)).collect();
        self.send(
            joiner,
            ServerMsg::Joined {
                room: name,
                owner: nicks[0].clone(),
                members: nicks,
            },
        );
        for &m in members.iter().filter(|&&m| m != joiner) {
            self.send(
                m,
                ServerMsg::MemberJoined {
                    nick: from.to_owned(),
                },
            );
        }
        // Room is full: nobody else can get in.
        for (other, _) in rest {
            let nick = self.nick(other);
            self.set_state(other, State::Idle);
            self.send(other, ServerMsg::Refused);
            self.send(owner, ServerMsg::RequestCancelled { from: nick });
        }
    }

    fn refuse(&mut self, owner: ConnId, from: &str) {
        let Some((name, joiner)) = self.pending_joiner(owner, from) else {
            return;
        };
        self.unstack(&name, joiner);
        self.set_state(joiner, State::Idle);
        self.send(joiner, ServerMsg::Refused);
    }

    fn relay(&mut self, id: ConnId, to: Option<&str>, bytes: Vec<u8>) {
        let Some(e) = self.conns.get(&id) else {
            return;
        };
        let State::InRoom { room } = &e.state else {
            return;
        };
        let Some(room) = self.rooms.get(room) else {
            return;
        };
        let targets: Vec<ConnId> = match to {
            Some(nick) => self
                .nicks
                .get(nick)
                .copied()
                .filter(|t| *t != id && room.members.contains(t))
                .into_iter()
                .collect(),
            None => room.members.iter().copied().filter(|&m| m != id).collect(),
        };
        if targets.is_empty() {
            return;
        }
        let from = e.nick.clone();
        if let Some(tap) = &self.tap {
            let _ = tap.send(bytes.clone());
        }
        for t in targets {
            self.send(
                t,
                ServerMsg::Relay {
                    from: from.clone(),
                    bytes: bytes.clone(),
                },
            );
        }
    }

    /// Turns every joiner in `pending` back to idle with `Refused`.
    fn refuse_all(&mut self, pending: Vec<(ConnId, u64)>) {
        for (joiner, _) in pending {
            self.set_state(joiner, State::Idle);
            self.send(joiner, ServerMsg::Refused);
        }
    }

    /// Removes `joiner` from the stack of room `name`. Returns the room's
    /// owner if the joiner was there.
    fn unstack(&mut self, name: &str, joiner: ConnId) -> Option<ConnId> {
        let room = self.rooms.get_mut(name)?;
        let before = room.pending.len();
        room.pending.retain(|&(j, _)| j != joiner);
        (room.pending.len() != before).then(|| room.owner())
    }

    fn nick(&self, id: ConnId) -> String {
        self.conns
            .get(&id)
            .map(|e| e.nick.clone())
            .unwrap_or_default()
    }

    /// Replaces the state of `id`, aborting the timeout of a request it
    /// leaves behind.
    fn set_state(&mut self, id: ConnId, state: State) {
        if let Some(e) = self.conns.get_mut(&id) {
            if let State::Requesting {
                timer: Some(timer), ..
            } = std::mem::replace(&mut e.state, state)
            {
                timer.abort();
            }
        }
    }

    /// Queues `msg` for `id`. A full outbox drops the sender, which makes
    /// that connection close.
    fn send(&mut self, id: ConnId, msg: ServerMsg) {
        if let Some(e) = self.conns.get_mut(&id) {
            if let Some(tx) = &e.tx {
                if tx.try_send(msg).is_err() {
                    e.tx = None;
                }
            }
        }
    }
}
