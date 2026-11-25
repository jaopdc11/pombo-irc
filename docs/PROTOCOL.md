# Protocol

This is the wire protocol between the pombo-irc client and relay, as implemented in
[`crates/proto`](../crates/proto/src/lib.rs) (control messages) and
[`crates/e2e`](../crates/e2e/src/lib.rs) (end-to-end payloads). The design rationale and threat
model are in [SECURITY.md](SECURITY.md).

There are two layers:

1. **Control messages** between a client and the relay. The relay reads and acts on them.
2. **End-to-end payloads** carried inside `Relay` messages. The relay forwards them untouched
   and cannot read them.

## Transport

- WebSocket, normally over TLS (`wss://`) through a reverse proxy. The relay itself speaks plain
  WebSocket on loopback.
- Every protocol message is exactly one **binary** WebSocket message. Text messages and close
  frames end the connection.
- Each message is one [`postcard`](https://docs.rs/postcard)-encoded `ClientMsg` or `ServerMsg`.
  Trailing bytes after the message are rejected.
- Maximum WebSocket message and frame size: **8 KiB** (`MAX_FRAME = 8192`), enforced by both
  sides.

### postcard encoding, briefly

Enum variants are encoded as a varint of the variant index (declaration order, starting at 0)
followed by their fields in order. Strings and byte vectors are a varint length followed by the
bytes. `Option` is a `0` or `1` byte, followed by the value when `1`. `u8` is one byte; wider
integers are varints. As a
consequence, **reordering or inserting variants changes the wire format**; new variants go at
the end.

## Limits

| Constant               | Value | Meaning                                                    |
|------------------------|-------|------------------------------------------------------------|
| `MAX_FRAME`            | 8192  | Largest WebSocket message, in bytes.                       |
| `MAX_NICK`             | 16    | Longest nick, in characters.                               |
| `MAX_ROOM_NAME`        | 24    | Longest room name, in characters.                          |
| `MAX_MEMBERS`          | 8     | Most people in one room, owner included.                   |
| `MAX_LISTED_ROOMS`     | 100   | Most rooms in one `Rooms` listing.                         |
| `REQUEST_TIMEOUT_SECS` | 60    | Seconds a knock waits for the owner before it is refused.  |
| `MAX_LINE`             | 4000  | Largest chat line, in bytes of UTF-8.                      |
| `PAD_BLOCK`            | 256   | End-to-end plaintext is padded to a multiple of this.      |

Nicks and room names are 1 to their maximum length of `[A-Za-z0-9_-]`. Nicks are unique among
connected clients; room names are unique among open rooms.

## Client to relay: `ClientMsg`

| # | Message                                    | Meaning                                                                                       |
|---|--------------------------------------------|-----------------------------------------------------------------------------------------------|
| 0 | `Hello { nick: String }`                   | Must be the first message on a connection. Claims a nick.                                     |
| 1 | `CreateRoom { name: String }`              | Create a room and become its owner. Only valid when in no room and not knocking.              |
| 2 | `ListRooms`                                | Ask for the list of rooms this client could join.                                             |
| 3 | `Join { room: String }`                    | Knock on a room. Only valid when in no room and not knocking. One knock at a time.            |
| 4 | `Accept { from: String }`                  | Owner only: accept the pending knock from `from`.                                             |
| 5 | `Refuse { from: String }`                  | Owner only: refuse the pending knock from `from`.                                             |
| 6 | `Relay { to: Option<String>, bytes: Vec<u8> }` | Forward an end-to-end payload to one member (`Some(nick)`) or the whole room (`None`).    |

Messages that are not valid in the current state (creating a room while in one, accepting a
knock that is not pending, relaying while not in a room, addressing someone who is not in your
room) are silently ignored.

There is no "leave" message. A client leaves a room, or withdraws a knock, by **closing the
connection**. The client opens a new connection when it returns to the nick prompt.

## Relay to client: `ServerMsg`

| #  | Message                                                    | Meaning                                                                                       |
|----|------------------------------------------------------------|-----------------------------------------------------------------------------------------------|
| 0  | `NickOk`                                                   | The nick in `Hello` is yours while you stay connected.                                         |
| 1  | `NickTaken`                                                | Someone online already uses that nick. The connection closes.                                  |
| 2  | `NickInvalid`                                              | The nick breaks the naming rule. The connection closes.                                        |
| 3  | `RoomCreated`                                              | Your room exists and you own it.                                                               |
| 4  | `RoomNameTaken`                                            | An open room already has that name.                                                            |
| 5  | `RoomNameInvalid`                                          | The name breaks the naming rule.                                                               |
| 6  | `Rooms(Vec<RoomInfo>)`                                     | Answer to `ListRooms`.                                                                         |
| 7  | `JoinRequest { from: String }`                             | Owner: `from` is knocking on your room.                                                        |
| 8  | `RequestCancelled { from: String }`                        | Owner: the knock from `from` was withdrawn, timed out, or can no longer be accepted.           |
| 9  | `Refused`                                                  | Knocker: your knock was refused, timed out, or the room filled up or closed.                   |
| 10 | `RoomGone`                                                 | Knocker: the room you knocked on does not exist or is full.                                    |
| 11 | `Joined { room: String, owner: String, members: Vec<String> }` | Knocker: you are in. `members` is in join order, oldest first, and includes you.          |
| 12 | `MemberJoined { nick: String }`                            | Existing members: `nick` was let in.                                                           |
| 13 | `MemberLeft { nick: String }`                              | Remaining members: `nick` left or disconnected.                                                |
| 14 | `OwnerChanged { owner: String }`                           | Remaining members: the owner left and `owner` (the oldest remaining member) now owns the room. |
| 15 | `Relay { from: String, bytes: Vec<u8> }`                   | An end-to-end payload from the member `from`. `from` is set by the relay, not by the sender.   |

`RoomInfo` is:

```rust
struct RoomInfo {
    name: String,
    owner: String,
    members: u8,    // people in the room, owner included
    age_secs: u64,  // seconds since the room was created
}
```

A `Rooms` listing contains open rooms that are not full and that the asking client is not
already in. If there are more than `MAX_LISTED_ROOMS`, the oldest ones are returned. The list is
sorted by name.

## Room lifecycle

### Connecting

1. The client opens the WebSocket and sends `Hello { nick }` within 10 seconds.
2. The relay answers `NickOk`, or `NickTaken` / `NickInvalid` and closes.

### Creating a room

`CreateRoom { name }` → `RoomCreated`, `RoomNameTaken` or `RoomNameInvalid`. On success the
creator is the owner and the only member, and the room appears in listings.

### Knocking

1. A client in no room sends `Join { room }`. If the room does not exist or is full, it gets
   `RoomGone`.
2. Otherwise the knock is pushed onto the room's **pending stack** and the owner gets
   `JoinRequest { from }`.
3. Pending knocks form a **LIFO stack**: the owner's client shows the most recent one, and
   `\y` / `\n` answer that one. After it is answered, the next most recent shows.
4. The owner answers with `Accept { from }` or `Refuse { from }`.

Outcomes:

| Event                                | Knocker gets                      | Owner gets                    | Others get                |
|--------------------------------------|-----------------------------------|-------------------------------|---------------------------|
| Owner accepts                        | `Joined { room, owner, members }` |                               | `MemberJoined { nick }`   |
| Owner refuses                        | `Refused`                         |                               |                           |
| No answer within 60 s                | `Refused`                         | `RequestCancelled { from }`   |                           |
| Knocker disconnects                  |                                   | `RequestCancelled { from }`   |                           |
| Room becomes full on an accept       | `Refused` (every other knocker)   | `RequestCancelled { from }` for each |                    |
| Owner leaves                         | `Refused` (every pending knocker) |                               |                           |
| Last member leaves (room closes)     | `Refused` (every pending knocker) |                               |                           |

When the owner leaves, pending knocks are refused rather than handed to the new owner, who has
not seen them.

### Leaving and ownership

A member leaves by closing its connection. The relay then:

- removes it from the room and sends `MemberLeft { nick }` to everyone remaining;
- if it was the owner, sends `OwnerChanged { owner }` to everyone remaining, where `owner` is the
  **oldest remaining member** (longest in the room), and refuses all pending knocks;
- if it was the last member, deletes the room.

If everyone else leaves, the remaining member owns an empty room, which is listed again.

### Relay timeouts and limits

These are relay defaults (`ServerConfig` in [`crates/server/src/lib.rs`](../crates/server/src/lib.rs)):

| Limit                     | Default    | Effect                                                         |
|---------------------------|------------|----------------------------------------------------------------|
| `Hello` deadline          | 10 s       | WebSocket handshake plus `Hello` must complete in this time.   |
| Knock timeout             | 60 s       | Unanswered knock counts as refused.                            |
| Time outside a room       | 1 hour     | A connection not in a room one hour after connecting is closed. |
| Concurrent connections    | 512        | Connections beyond this are closed on accept.                  |
| Incoming rate             | 30/s, burst 60 | Exceeding it closes the connection.                        |
| Ping                      | every 30 s | A ping not answered by the next one closes the connection.     |
| Outgoing queue            | 512 messages | A client that cannot keep up is disconnected.                |

Any malformed message closes the connection.

The relay also answers a plain `GET /saude` HTTP request with `200 ok`, for deployment health
checks. Everything else must be a WebSocket upgrade.

## End-to-end layer

Everything below travels inside `Relay.bytes` and is opaque to the relay.

### Envelope

The first byte of every payload says what it carries:

| Tag | Envelope    | Addressed to         | Body                                         |
|-----|-------------|----------------------|----------------------------------------------|
| 0   | `Handshake` | one peer (`Some`)    | a Noise handshake message                    |
| 1   | `Key`       | one member (`Some`)  | a Noise transport message carrying a room key |
| 2   | `Room`      | whole room (`None`)  | a sealed room message                        |

An unknown tag is dropped.

### Padding

`pad(p)` = `len(p)` as u16 big-endian ‖ `p` ‖ zero bytes, up to the next multiple of 256 bytes.
`unpad` rejects buffers whose length is not a positive multiple of 256 or whose length prefix
points past the end.

### Handshake

`Noise_NN_25519_ChaChaPoly_BLAKE2s`, empty payloads. The member is the initiator and the owner
the responder.

1. Member → owner: `Handshake(msg1)` (`-> e`, 32 bytes).
2. Owner → member: `Handshake(msg2)` (`<- e, ee`, 48 bytes).

Both sides then switch to transport mode and compute the fingerprint: the first 8 bytes of the
handshake hash as uppercase hex, `XXXX-XXXX-XXXX-XXXX`.

A member starts a handshake with the owner when it enters a room and again whenever the owner
changes. The owner answers a handshake only from a member it has no session with yet.

### Key delivery

After answering a newcomer's handshake, the owner generates a new room key and sends it to
every member that has a session, newcomer included. It does the same when someone leaves.

```text
Key body = NoiseTransport( pad( epoch: u32 BE ‖ key: 32 bytes ) )
```

The padded plaintext is 256 bytes, so the body is 272 bytes (256 + 16-byte tag). A member accepts
a key only from the current owner, over its session with that owner.

On an owner change, every client drops the key in use and marks its epoch as retired. The new
owner generates a key with the next epoch and sends it to each member as soon as that member's
new handshake completes. A key with the retired epoch is refused.

### Room message

```text
Room body = epoch: u32 BE ‖ nonce: 24 bytes ‖ XChaCha20-Poly1305(key[epoch], nonce, aad, pad(inner))
aad       = epoch: u32 BE ‖ sender nick (UTF-8 bytes)
inner     = 0x00 ‖ UTF-8 text      (chat line, text at most 4000 bytes)
          | 0x01                   (typing indicator, nothing after the tag)
```

- The nonce is 24 random bytes per message.
- The receiver uses the `from` nick the relay attached to the `Relay` message as the sender in
  `aad`. A message sealed under another nick does not open.
- Sizes: a typing indicator or a line up to 253 bytes produces a 300-byte body
  (4 + 24 + 256 + 16). The largest line produces 4 + 24 + 4096 + 16 = 4140 bytes.
- Bodies outside those bounds are rejected before decryption.

Receivers keep the current key and, until the first message under the current epoch opens, the
previous one. Each nonce opens at most once per epoch; replays are rejected. Messages under any
other epoch are rejected.

### Typing indicators

The client sends a typing indicator at most every 2 seconds while the user is typing a line
(not a `\` command). It is an ordinary room message, the same size as a short line.
