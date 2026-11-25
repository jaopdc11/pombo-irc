//! Client core: lobby connection and end-to-end encrypted group rooms.
//!
//! A background reader and writer own the WebSocket. Everything the UI awaits is a channel
//! receive or a timer, so [`Conn::next_event`] and [`Room::next`] are cancel-safe; all
//! processing and sending after a frame arrives is synchronous.

use std::collections::{HashMap, HashSet, VecDeque};
use std::time::Duration;

use e2e::{Envelope, Handshake, HandshakeRole, RoomCipher, RoomKey, RoomMsg, Session};
use futures_util::{SinkExt, StreamExt};
use proto::{valid_nick, valid_room_name, ClientMsg, RoomInfo, ServerMsg, MAX_FRAME, MAX_LINE};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::Message;
use zeroize::{Zeroize, Zeroizing};

/// Server used when `POMBO_URL` is not set.
const DEFAULT_URL: &str = "wss://pombo.api.jaopd.dev";
/// How long connecting and the nick check may take.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// How long [`Conn::enter`] waits for the handshake and the first room key.
const ENTER_TIMEOUT: Duration = Duration::from_secs(10);
/// How long the lobby waits for the server to answer a room list or a room creation.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(10);
/// How long the owner holds a newcomer's `joined` line waiting for its fingerprint.
const ANNOUNCE_WAIT: Duration = Duration::from_secs(10);
/// How long [`Room::leave`] waits for the close frame to go out.
const LEAVE_TIMEOUT: Duration = Duration::from_secs(1);

/// `POMBO_URL`, or the compiled-in server.
#[must_use]
pub fn server_url() -> String {
    std::env::var("POMBO_URL")
        .ok()
        .filter(|u| !u.is_empty())
        .unwrap_or_else(|| DEFAULT_URL.to_owned())
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConnectError {
    #[error("nick taken")]
    NickTaken,
    #[error("nick must be 1-16 letters, digits, _ or -")]
    NickInvalid,
    #[error("network: {0}")]
    Network(String),
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CoreError {
    #[error("disconnected")]
    Disconnected,
    #[error("line too long")]
    TooLong,
    #[error("room key not ready")]
    NotReady,
    #[error("protocol: {0}")]
    Protocol(String),
    #[error("server did not answer")]
    Timeout,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CreateError {
    #[error("name taken")]
    NameTaken,
    #[error("name must be 1-24 letters, digits, _ or -")]
    NameInvalid,
    #[error(transparent)]
    Core(CoreError),
}

/// What can happen while waiting in the lobby after [`Conn::join`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LobbyEvent {
    Refused,
    RoomGone,
    /// Accepted. `members` is in join order, oldest first, and includes me.
    Joined {
        room: String,
        owner: String,
        members: Vec<String>,
    },
}

/// What can happen inside a room.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoomEvent {
    Line {
        from: String,
        text: Zeroizing<String>,
    },
    Typing {
        from: String,
    },
    /// Owner only.
    JoinRequest {
        from: String,
    },
    /// Owner only.
    RequestCancelled {
        from: String,
    },
    /// `fingerprint` is set only on the owner's side (the newcomer's fingerprint with the owner).
    MemberJoined {
        nick: String,
        fingerprint: Option<String>,
    },
    MemberLeft {
        nick: String,
    },
    OwnerChanged {
        owner: String,
    },
    /// A pairwise session completed outside a join announcement. Owner side: `nick` is a
    /// member that handshook with me after I inherited the room (or after its `joined` line was
    /// already shown). Member side: `nick` is the new owner after an owner change.
    Fingerprint {
        nick: String,
        fingerprint: String,
    },
    /// A new room key is in use.
    Rekeyed {
        epoch: u32,
    },
    /// The server connection is gone. Reported once; later calls fail with `Disconnected`.
    ConnectionLost,
}

fn proto_err(what: &str) -> CoreError {
    CoreError::Protocol(what.to_owned())
}

fn check_nick(nick: &str) -> Result<(), CoreError> {
    if valid_nick(nick) {
        Ok(())
    } else {
        Err(proto_err("invalid nick from server"))
    }
}

/// The socket: a writer task fed by `out`, a reader task feeding `inbox`.
struct Link {
    out: Option<mpsc::UnboundedSender<ClientMsg>>,
    inbox: mpsc::UnboundedReceiver<ServerMsg>,
    reader: JoinHandle<()>,
    writer: Option<JoinHandle<()>>,
}

impl Link {
    async fn open(url: &str) -> Result<Link, String> {
        // rustls needs a process-wide provider; a second install just fails, which is fine.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let config = WebSocketConfig::default()
            .max_message_size(Some(MAX_FRAME))
            .max_frame_size(Some(MAX_FRAME));
        let (ws, _resp) = tokio_tungstenite::connect_async_with_config(url, Some(config), true)
            .await
            .map_err(|e| e.to_string())?;
        let (mut sink, mut stream) = ws.split();
        let (out_tx, mut out_rx) = mpsc::unbounded_channel::<ClientMsg>();
        let (in_tx, in_rx) = mpsc::unbounded_channel::<ServerMsg>();
        let writer = tokio::spawn(async move {
            while let Some(msg) = out_rx.recv().await {
                if sink
                    .send(Message::Binary(proto::encode(&msg).into()))
                    .await
                    .is_err()
                {
                    return;
                }
            }
            let _ = sink.close().await;
        });
        let reader = tokio::spawn(async move {
            while let Some(frame) = stream.next().await {
                match frame {
                    Ok(Message::Binary(bytes)) => match proto::decode::<ServerMsg>(&bytes) {
                        Ok(msg) => {
                            if in_tx.send(msg).is_err() {
                                return;
                            }
                        }
                        Err(_) => return,
                    },
                    Ok(Message::Ping(_) | Message::Pong(_) | Message::Frame(_)) => {}
                    Ok(Message::Text(_) | Message::Close(_)) | Err(_) => return,
                }
            }
        });
        Ok(Link {
            out: Some(out_tx),
            inbox: in_rx,
            reader,
            writer: Some(writer),
        })
    }

    fn send(&self, msg: ClientMsg) -> Result<(), CoreError> {
        self.out
            .as_ref()
            .ok_or(CoreError::Disconnected)?
            .send(msg)
            .map_err(|_| CoreError::Disconnected)
    }

    /// Cancel-safe. `None` once the connection is gone.
    async fn recv(&mut self) -> Option<ServerMsg> {
        self.inbox.recv().await
    }

    /// Sends the close frame and waits briefly for it to go out.
    async fn close(mut self) {
        self.out = None;
        if let Some(writer) = self.writer.take() {
            let _ = tokio::time::timeout(LEAVE_TIMEOUT, writer).await;
        }
    }
}

impl Drop for Link {
    fn drop(&mut self) {
        self.reader.abort();
        // The writer ends on its own once `out` is dropped, sending a close frame.
    }
}

/// A connection in the lobby, with its nick accepted by the server.
pub struct Conn {
    link: Link,
    nick: String,
}

impl Conn {
    /// Connects and claims `nick`. The nick is checked locally before touching the network.
    pub async fn connect(url: &str, nick: &str) -> Result<Conn, ConnectError> {
        if !valid_nick(nick) {
            return Err(ConnectError::NickInvalid);
        }
        let attempt = async {
            let mut link = Link::open(url).await.map_err(ConnectError::Network)?;
            link.send(ClientMsg::Hello {
                nick: nick.to_owned(),
            })
            .map_err(|_| ConnectError::Network("connection closed".into()))?;
            loop {
                match link.recv().await {
                    Some(ServerMsg::NickOk) => return Ok(link),
                    Some(ServerMsg::NickTaken) => return Err(ConnectError::NickTaken),
                    Some(ServerMsg::NickInvalid) => return Err(ConnectError::NickInvalid),
                    Some(_) => {}
                    None => return Err(ConnectError::Network("connection closed".into())),
                }
            }
        };
        let link = tokio::time::timeout(CONNECT_TIMEOUT, attempt)
            .await
            .map_err(|_| ConnectError::Network("timed out".into()))??;
        Ok(Conn {
            link,
            nick: nick.to_owned(),
        })
    }

    #[must_use]
    pub fn nick(&self) -> &str {
        &self.nick
    }

    /// Open rooms that are not full, sorted by name.
    pub async fn list_rooms(&mut self) -> Result<Vec<RoomInfo>, CoreError> {
        self.link.send(ClientMsg::ListRooms)?;
        let deadline = Instant::now() + ANSWER_TIMEOUT;
        loop {
            let msg = tokio::time::timeout_at(deadline, self.link.recv())
                .await
                .map_err(|_| CoreError::Timeout)?;
            match msg {
                Some(ServerMsg::Rooms(rooms)) => {
                    for r in &rooms {
                        check_nick(&r.owner)?;
                        if !valid_room_name(&r.name) {
                            return Err(proto_err("invalid room name from server"));
                        }
                    }
                    return Ok(rooms);
                }
                Some(_) => {}
                None => return Err(CoreError::Disconnected),
            }
        }
    }

    /// Creates the room `name` and enters it as owner. On failure the connection comes back.
    pub async fn create_room(mut self, name: &str) -> Result<Room, (Conn, CreateError)> {
        if !valid_room_name(name) {
            return Err((self, CreateError::NameInvalid));
        }
        if let Err(e) = self.link.send(ClientMsg::CreateRoom {
            name: name.to_owned(),
        }) {
            return Err((self, CreateError::Core(e)));
        }
        let deadline = Instant::now() + ANSWER_TIMEOUT;
        loop {
            let Ok(msg) = tokio::time::timeout_at(deadline, self.link.recv()).await else {
                return Err((self, CreateError::Core(CoreError::Timeout)));
            };
            match msg {
                Some(ServerMsg::RoomCreated) => break,
                Some(ServerMsg::RoomNameTaken) => return Err((self, CreateError::NameTaken)),
                Some(ServerMsg::RoomNameInvalid) => return Err((self, CreateError::NameInvalid)),
                Some(_) => {}
                None => return Err((self, CreateError::Core(CoreError::Disconnected))),
            }
        }
        let me = self.nick;
        let key = RoomKey::generate(0);
        Ok(Room {
            link: self.link,
            name: name.to_owned(),
            owner: me.clone(),
            members: vec![me.clone()],
            me,
            cipher: Some(RoomCipher::new(key.clone())),
            owner_key: Some(key),
            retired_epoch: None,
            owner_hs: None,
            owner_session: None,
            fingerprint: None,
            owner_fp_event: false,
            peers: HashMap::new(),
            announce: HashMap::new(),
            rekey_wait: HashSet::new(),
            pending: VecDeque::new(),
            lost: false,
        })
    }

    /// Knocks on `room`. The answer comes from [`Conn::next_event`].
    pub async fn join(&mut self, room: &str) -> Result<(), CoreError> {
        self.link.send(ClientMsg::Join {
            room: room.to_owned(),
        })
    }

    /// Cancel-safe. Waits for the answer to [`Conn::join`].
    pub async fn next_event(&mut self) -> Result<LobbyEvent, CoreError> {
        loop {
            match self.link.recv().await {
                Some(ServerMsg::Refused) => return Ok(LobbyEvent::Refused),
                Some(ServerMsg::RoomGone) => return Ok(LobbyEvent::RoomGone),
                Some(ServerMsg::Joined {
                    room,
                    owner,
                    members,
                }) => {
                    if !valid_room_name(&room) {
                        return Err(proto_err("invalid room name from server"));
                    }
                    check_nick(&owner)?;
                    for m in &members {
                        check_nick(m)?;
                    }
                    return Ok(LobbyEvent::Joined {
                        room,
                        owner,
                        members,
                    });
                }
                Some(_) => {}
                None => return Err(CoreError::Disconnected),
            }
        }
    }

    /// Enters the room from a `Joined` event: handshake with the owner, then waits for the
    /// first room key (10 s).
    pub async fn enter(self, joined: LobbyEvent) -> Result<Room, CoreError> {
        let LobbyEvent::Joined {
            room,
            owner,
            members,
        } = joined
        else {
            return Err(proto_err("enter needs a Joined event"));
        };
        let mut room = Room {
            link: self.link,
            name: room,
            me: self.nick,
            owner,
            members,
            cipher: None,
            owner_key: None,
            retired_epoch: None,
            owner_hs: None,
            owner_session: None,
            fingerprint: None,
            owner_fp_event: false,
            peers: HashMap::new(),
            announce: HashMap::new(),
            rekey_wait: HashSet::new(),
            pending: VecDeque::new(),
            lost: false,
        };
        if !room.members.contains(&room.me) {
            room.members.push(room.me.clone());
        }
        if room.is_owner() {
            room.become_owner();
        } else {
            room.start_handshake()?;
        }
        let ready = async {
            while room.cipher.is_none() {
                room.step().await?;
                if room.lost {
                    return Err(CoreError::Disconnected);
                }
            }
            Ok(())
        };
        tokio::time::timeout(ENTER_TIMEOUT, ready)
            .await
            .map_err(|_| proto_err("timed out waiting for the room key"))??;
        Ok(room)
    }
}

/// A room I am in. All key handling is internal.
pub struct Room {
    link: Link,
    name: String,
    me: String,
    owner: String,
    members: Vec<String>,
    /// Seals and opens room messages. `None` until a key arrives, and again from an owner change
    /// until the new owner's key arrives: the departed owner's key is never used again.
    cipher: Option<RoomCipher>,
    /// Owner side: the key in use, handed to members that handshake after I inherited the room.
    owner_key: Option<RoomKey>,
    /// Epoch of the key dropped at the last owner change; a key with this epoch is not accepted.
    retired_epoch: Option<u32>,
    /// Member side: handshake in flight toward the owner.
    owner_hs: Option<Handshake>,
    /// Member side: session with the owner, used to receive keys.
    owner_session: Option<Session>,
    fingerprint: Option<String>,
    /// Member side: report the next completed owner session as a `Fingerprint` event.
    owner_fp_event: bool,
    /// Owner side: sessions with members. At most one per member per owner.
    peers: HashMap<String, Session>,
    /// Owner side: members whose `MemberJoined` is held until their handshake gives a fingerprint,
    /// with the moment it is shown anyway.
    announce: HashMap<String, Instant>,
    /// Owner side, after inheriting the room: members already in the room that get the key in
    /// use (no rekey) once their handshake completes.
    rekey_wait: HashSet<String>,
    pending: VecDeque<RoomEvent>,
    lost: bool,
}

impl Room {
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub fn me(&self) -> &str {
        &self.me
    }

    #[must_use]
    pub fn owner(&self) -> &str {
        &self.owner
    }

    /// Members in join order, oldest first, me included.
    #[must_use]
    pub fn members(&self) -> &[String] {
        &self.members
    }

    #[must_use]
    pub fn is_owner(&self) -> bool {
        self.owner == self.me
    }

    /// My fingerprint with the current owner; `None` for the owner or while it is being made.
    #[must_use]
    pub fn fingerprint(&self) -> Option<&str> {
        self.fingerprint.as_deref()
    }

    /// Epoch of the room key in use, if any.
    #[must_use]
    pub fn epoch(&self) -> Option<u32> {
        self.cipher.as_ref().map(RoomCipher::epoch)
    }

    /// Test hook: whether the room key in use opens `relayed` (a `Relay` payload) as sent by
    /// `from`.
    #[doc(hidden)]
    #[must_use]
    pub fn can_open(&self, from: &str, relayed: &[u8]) -> bool {
        let (Some(cipher), Ok(Envelope::Room(sealed))) = (&self.cipher, Envelope::decode(relayed))
        else {
            return false;
        };
        cipher.open(from, &sealed).is_ok()
    }

    /// Sends a line to the whole room. Over `MAX_LINE` bytes nothing is sent.
    pub async fn send_line(&mut self, text: &str) -> Result<(), CoreError> {
        if text.len() > MAX_LINE {
            return Err(CoreError::TooLong);
        }
        let mut msg = RoomMsg::Line(text.to_owned());
        let sent = self.send_room(&msg);
        if let RoomMsg::Line(copy) = &mut msg {
            copy.zeroize();
        }
        self.checked(sent)
    }

    pub async fn send_typing(&mut self) -> Result<(), CoreError> {
        let sent = self.send_room(&RoomMsg::Typing);
        self.checked(sent)
    }

    /// Owner: accepts the pending request from `from`.
    pub async fn accept(&mut self, from: &str) -> Result<(), CoreError> {
        let sent = self.link.send(ClientMsg::Accept {
            from: from.to_owned(),
        });
        self.checked(sent)
    }

    /// Owner: refuses the pending request from `from`.
    pub async fn refuse(&mut self, from: &str) -> Result<(), CoreError> {
        let sent = self.link.send(ClientMsg::Refuse {
            from: from.to_owned(),
        });
        self.checked(sent)
    }

    /// Test hook: sends a fresh handshake to the owner, as a relay injecting one under my nick
    /// would.
    #[doc(hidden)]
    pub fn restart_handshake(&mut self) -> Result<(), CoreError> {
        self.start_handshake()
    }

    /// Cancel-safe. The next thing that happened in the room.
    pub async fn next(&mut self) -> Result<RoomEvent, CoreError> {
        loop {
            if let Some(ev) = self.pending.pop_front() {
                return Ok(ev);
            }
            if self.lost {
                return Err(CoreError::Disconnected);
            }
            if let Err(e) = self.step().await {
                self.fail();
                return Err(e);
            }
        }
    }

    /// Leaves the room by closing the connection.
    pub async fn leave(self) {
        self.link.close().await;
    }

    /// Any error other than `TooLong`/`NotReady` ends the room: keys go, the connection closes.
    fn checked<T>(&mut self, r: Result<T, CoreError>) -> Result<T, CoreError> {
        if matches!(&r, Err(e) if !matches!(e, CoreError::TooLong | CoreError::NotReady)) {
            self.fail();
        }
        r
    }

    /// Drops every key and session.
    fn discard_keys(&mut self) {
        self.cipher = None;
        self.owner_key = None;
        self.owner_session = None;
        self.owner_hs = None;
        self.peers.clear();
        self.rekey_wait.clear();
    }

    /// Gives up on the room: keys go and the connection closes, so the server sees me leave.
    fn fail(&mut self) {
        self.discard_keys();
        self.lost = true;
        // Dropping the sender makes the writer send a close frame and stop.
        self.link.out = None;
        self.link.reader.abort();
    }

    fn send_room(&mut self, msg: &RoomMsg) -> Result<(), CoreError> {
        let cipher = self.cipher.as_ref().ok_or(CoreError::NotReady)?;
        let sealed = cipher.seal(&self.me, msg).map_err(|e| match e {
            e2e::E2eError::TooLong => CoreError::TooLong,
            other => CoreError::Protocol(other.to_string()),
        })?;
        self.link.send(ClientMsg::Relay {
            to: None,
            bytes: Envelope::Room(sealed).encode(),
        })
    }

    fn relay_to(&self, to: &str, env: &Envelope) -> Result<(), CoreError> {
        self.link.send(ClientMsg::Relay {
            to: Some(to.to_owned()),
            bytes: env.encode(),
        })
    }

    /// Waits for one frame (or a held `joined` line falling due) and processes it into
    /// `pending`. Cancel-safe: the only awaits are a channel receive and a timer.
    async fn step(&mut self) -> Result<(), CoreError> {
        let msg = match self.announce.values().min().copied() {
            Some(deadline) => tokio::select! {
                m = self.link.recv() => m,
                () = tokio::time::sleep_until(deadline) => {
                    self.announce_due();
                    return Ok(());
                }
            },
            None => self.link.recv().await,
        };
        match msg {
            Some(msg) => self.handle(msg),
            None => {
                self.lost = true;
                self.discard_keys();
                self.pending.push_back(RoomEvent::ConnectionLost);
                Ok(())
            }
        }
    }

    /// Owner: shows the `joined` lines whose handshake did not come in time, without fingerprint.
    fn announce_due(&mut self) {
        let now = Instant::now();
        let mut due: Vec<String> = self
            .announce
            .iter()
            .filter(|(_, at)| **at <= now)
            .map(|(nick, _)| nick.clone())
            .collect();
        due.sort_by_key(|n| self.members.iter().position(|m| m == n));
        for nick in due {
            self.announce.remove(&nick);
            self.pending.push_back(RoomEvent::MemberJoined {
                nick,
                fingerprint: None,
            });
        }
    }

    fn handle(&mut self, msg: ServerMsg) -> Result<(), CoreError> {
        match msg {
            ServerMsg::JoinRequest { from } => {
                check_nick(&from)?;
                if self.is_owner() {
                    self.pending.push_back(RoomEvent::JoinRequest { from });
                }
            }
            ServerMsg::RequestCancelled { from } => {
                check_nick(&from)?;
                if self.is_owner() {
                    self.pending.push_back(RoomEvent::RequestCancelled { from });
                }
            }
            ServerMsg::MemberJoined { nick } => {
                check_nick(&nick)?;
                if !self.members.contains(&nick) {
                    self.members.push(nick.clone());
                }
                if self.is_owner() {
                    self.announce.insert(nick, Instant::now() + ANNOUNCE_WAIT);
                } else {
                    self.pending.push_back(RoomEvent::MemberJoined {
                        nick,
                        fingerprint: None,
                    });
                }
            }
            ServerMsg::MemberLeft { nick } => {
                check_nick(&nick)?;
                self.members.retain(|m| *m != nick);
                self.peers.remove(&nick);
                self.rekey_wait.remove(&nick);
                if self.announce.remove(&nick).is_some() {
                    self.pending.push_back(RoomEvent::MemberJoined {
                        nick: nick.clone(),
                        fingerprint: None,
                    });
                }
                self.pending
                    .push_back(RoomEvent::MemberLeft { nick: nick.clone() });
                if self.is_owner() && nick != self.me {
                    self.rekey()?;
                }
            }
            ServerMsg::OwnerChanged { owner } => {
                check_nick(&owner)?;
                self.owner.clone_from(&owner);
                self.fingerprint = None;
                // The departed owner's key is never used again, to seal or to open: until the
                // new owner's key arrives (or I make it), nothing is sent.
                if let Some(c) = self.cipher.take() {
                    self.retired_epoch = Some(c.epoch());
                }
                self.discard_keys();
                self.pending.push_back(RoomEvent::OwnerChanged { owner });
                if self.is_owner() {
                    self.owner_fp_event = false;
                    self.become_owner();
                } else {
                    self.owner_fp_event = true;
                    self.start_handshake()?;
                }
            }
            ServerMsg::Relay { from, bytes } => {
                check_nick(&from)?;
                self.relay_from(&from, &bytes)?;
            }
            // Lobby answers have no meaning inside a room.
            _ => {}
        }
        Ok(())
    }

    /// Handles an opaque payload. Anything that fails to decrypt or is out of place is dropped.
    fn relay_from(&mut self, from: &str, bytes: &[u8]) -> Result<(), CoreError> {
        if from == self.me || !self.members.iter().any(|m| m == from) {
            return Ok(());
        }
        let Ok(env) = Envelope::decode(bytes) else {
            return Ok(());
        };
        match env {
            Envelope::Room(sealed) => {
                let Some(cipher) = &self.cipher else {
                    return Ok(());
                };
                match cipher.open(from, &sealed) {
                    Ok(RoomMsg::Line(text)) => self.pending.push_back(RoomEvent::Line {
                        from: from.to_owned(),
                        text: Zeroizing::new(text),
                    }),
                    Ok(RoomMsg::Typing) => self.pending.push_back(RoomEvent::Typing {
                        from: from.to_owned(),
                    }),
                    Err(_) => {}
                }
            }
            Envelope::Handshake(msg) => {
                if self.is_owner() {
                    self.answer_handshake(from, &msg)?;
                } else if from == self.owner {
                    self.finish_handshake(&msg);
                }
            }
            Envelope::Key(ct) => {
                if from != self.owner {
                    return Ok(());
                }
                let Some(session) = &mut self.owner_session else {
                    return Ok(());
                };
                let Ok(key) = session.open_key(&ct) else {
                    return Ok(());
                };
                let epoch = key.epoch();
                let retired = self.retired_epoch;
                match &mut self.cipher {
                    None if retired == Some(epoch) => return Ok(()),
                    None => self.cipher = Some(RoomCipher::new(key)),
                    Some(c) if epoch != c.epoch() => c.rotate(key),
                    Some(_) => return Ok(()),
                }
                self.pending.push_back(RoomEvent::Rekeyed { epoch });
            }
        }
        Ok(())
    }

    /// Owner: answers a member's handshake, then rekeys (or, for a member that was already in
    /// the room when I inherited it, hands over the key in use).
    ///
    /// Only a member without a session with me gets an answer: a second handshake "from" an
    /// established member (which a relay can inject at any time) is dropped, so it can neither
    /// replace the session whose fingerprint was compared nor receive the next key.
    fn answer_handshake(&mut self, from: &str, msg: &[u8]) -> Result<(), CoreError> {
        if self.peers.contains_key(from) {
            return Ok(());
        }
        let mut hs = Handshake::new(HandshakeRole::Responder);
        if hs.read_message(msg).is_err() {
            return Ok(());
        }
        let Ok(reply) = hs.write_message() else {
            return Ok(());
        };
        let Ok(session) = hs.into_session() else {
            return Ok(());
        };
        self.relay_to(from, &Envelope::Handshake(reply))?;
        let fingerprint = session.fingerprint().to_owned();
        self.peers.insert(from.to_owned(), session);
        if self.announce.remove(from).is_some() {
            self.pending.push_back(RoomEvent::MemberJoined {
                nick: from.to_owned(),
                fingerprint: Some(fingerprint),
            });
        } else {
            self.pending.push_back(RoomEvent::Fingerprint {
                nick: from.to_owned(),
                fingerprint,
            });
        }
        if self.rekey_wait.remove(from) {
            self.send_key(from)
        } else {
            self.rekey()
        }
    }

    /// Owner: sends the key in use to `to` over its session.
    fn send_key(&mut self, to: &str) -> Result<(), CoreError> {
        let (Some(key), Some(session)) = (&self.owner_key, self.peers.get_mut(to)) else {
            return Ok(());
        };
        let ct = session
            .seal_key(key)
            .map_err(|e| CoreError::Protocol(e.to_string()))?;
        self.link.send(ClientMsg::Relay {
            to: Some(to.to_owned()),
            bytes: Envelope::Key(ct).encode(),
        })
    }

    /// Member: starts the handshake toward the current owner.
    fn start_handshake(&mut self) -> Result<(), CoreError> {
        let mut hs = Handshake::new(HandshakeRole::Initiator);
        let first = hs
            .write_message()
            .map_err(|e| CoreError::Protocol(e.to_string()))?;
        self.owner_hs = Some(hs);
        let owner = self.owner.clone();
        self.relay_to(&owner, &Envelope::Handshake(first))
    }

    /// Member: completes the handshake with the owner's reply.
    fn finish_handshake(&mut self, msg: &[u8]) {
        let Some(mut hs) = self.owner_hs.take() else {
            return;
        };
        if hs.read_message(msg).is_err() {
            return;
        }
        if let Ok(session) = hs.into_session() {
            let fingerprint = session.fingerprint().to_owned();
            if std::mem::take(&mut self.owner_fp_event) {
                self.pending.push_back(RoomEvent::Fingerprint {
                    nick: self.owner.clone(),
                    fingerprint: fingerprint.clone(),
                });
            }
            self.fingerprint = Some(fingerprint);
            self.owner_session = Some(session);
        }
    }

    /// Epoch for the next key: one past the key in use, or past the one dropped at the last
    /// owner change.
    fn next_epoch(&self) -> u32 {
        self.cipher
            .as_ref()
            .map(RoomCipher::epoch)
            .or(self.retired_epoch)
            .map_or(0, |e| e.wrapping_add(1))
    }

    /// Becomes owner: a fresh key right away (the old owner's is gone), handed to each member
    /// already in the room as its handshake completes.
    fn become_owner(&mut self) {
        self.rekey_wait = self
            .members
            .iter()
            .filter(|m| **m != self.me)
            .cloned()
            .collect();
        let epoch = self.next_epoch();
        let key = RoomKey::generate(epoch);
        self.cipher = Some(RoomCipher::new(key.clone()));
        self.owner_key = Some(key);
        self.pending.push_back(RoomEvent::Rekeyed { epoch });
    }

    /// Owner: a fresh key, epoch + 1, sent to every member with a session (join order).
    fn rekey(&mut self) -> Result<(), CoreError> {
        let epoch = self.next_epoch();
        let key = RoomKey::generate(epoch);
        for m in &self.members {
            if let Some(session) = self.peers.get_mut(m) {
                let ct = session
                    .seal_key(&key)
                    .map_err(|e| CoreError::Protocol(e.to_string()))?;
                self.link.send(ClientMsg::Relay {
                    to: Some(m.clone()),
                    bytes: Envelope::Key(ct).encode(),
                })?;
            }
        }
        match &mut self.cipher {
            Some(c) => c.rotate(key.clone()),
            None => self.cipher = Some(RoomCipher::new(key.clone())),
        }
        self.owner_key = Some(key);
        self.pending.push_back(RoomEvent::Rekeyed { epoch });
        Ok(())
    }
}
