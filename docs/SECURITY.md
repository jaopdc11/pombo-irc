# Security

This document describes what pombo-irc protects, what it does not, and how it does it. The wire
format referenced here is specified in [PROTOCOL.md](PROTOCOL.md).

## Goals and non-goals

pombo-irc aims to keep the **content** of a conversation (messages and typing indicators)
confidential and authentic among the people in a room, against the relay that carries it and
anyone on the network path, and to leave nothing behind on disk.

It does not try to:

- hide **who** is talking to whom from the relay operator (see [What the relay sees](#what-the-relay-sees));
- provide long-term identities, accounts or authentication of people;
- protect against a compromised endpoint. Anyone in the room can read the room, and a machine
  with malware on it is not a safe place to chat.

## Threat model

### Parties

- **Members** run the client in a room. One of them is the **owner**: the creator, or after
  the owner leaves, the oldest remaining member.
- **The relay** is the server. It accepts WebSocket connections, keeps rooms in memory and
  forwards payloads between room members.
- **The network** between clients and relay, including the TLS-terminating reverse proxy in
  front of the relay.

### What the relay sees

The relay has to route, so it necessarily sees:

- nicks and room names;
- who is in which room, who owns it, and who knocks on which room and when;
- the timing and size of every payload (sizes are padded, see below);
- whether a payload is a handshake, a key delivery or a room message (the one-byte envelope
  tag is not encrypted), and whether it is addressed to one member or to the whole room;
- connection addresses. Behind a reverse proxy the relay process itself sees only the proxy;
  the proxy sees client IP addresses.

The relay keeps all of this in memory only and writes nothing to disk. The server prints one
line at startup and fatal errors, nothing else. That is a property of the code in this
repository; you are trusting the operator of the relay you connect to to run it unmodified and
with proxy access logs off.

### What the relay never sees

- message text or typing indicators in the clear;
- room keys, or any key that could open room messages;
- Noise session keys.

### Attacker capabilities considered

- A passive network observer sees a TLS connection to the relay's host and its traffic volume.
- A **malicious relay** (or whoever controls it) can drop, delay, reorder, replay, duplicate
  and inject payloads, lie about room membership and ownership, and substitute its own Noise
  handshakes. Confidentiality against it rests on the cryptography below plus, for the
  handshake, on fingerprint comparison.
- A **malicious member** has the room key while they are in the room and can read everything
  sent during that time. That is inherent to group chat.

## Cryptographic design

### Pairwise sessions: Noise NN

Every member runs a `Noise_NN_25519_ChaChaPoly_BLAKE2s` handshake with the current owner,
using the [`snow`](https://crates.io/crates/snow) crate. The member is the initiator and the
owner the responder. Both sides use fresh ephemeral X25519 keys for every handshake; there are
no static keys.

The resulting session is used **only** to deliver room keys from the owner to that member.
Chat messages never travel over it.

`NN` has no authentication by itself: either side could be talking to an active attacker.
That is what the fingerprint is for.

### Room keys and epochs

A room key is 32 random bytes from the operating system RNG, tagged with a 32-bit **epoch**.
The owner generates a new key with `epoch + 1`:

- every time someone joins (after their handshake completes);
- every time someone leaves;
- when ownership passes to a new owner (see below).

The owner sends each new key to every member over that member's pairwise session. On the
wire, a key delivery is `pad(epoch ‖ key)` encrypted as one Noise transport message.

Consequences:

- A newcomer only ever receives keys generated after they joined, so they cannot read anything
  sent earlier.
- After someone leaves, the next key is never sent to them, so they cannot read anything sent
  under it.

### Room messages: XChaCha20-Poly1305

Room messages (chat lines and typing indicators) are sealed with the current room key using
XChaCha20-Poly1305:

- a random 24-byte nonce per message, from the operating system RNG (XChaCha's 192-bit nonce
  makes random nonces safe without coordination between members);
- associated data `epoch (u32 big-endian) ‖ sender nick`;
- plaintext padded to a multiple of 256 bytes (2-byte length prefix, payload, zero fill), so a
  short line and a typing indicator are the same size on the wire.

The relay attaches the nick of the connection that actually sent each payload, and the
receiving client opens the message with that nick as the sender in the associated data. A
member who seals a message claiming to be someone else produces a message that fails to open
for everyone, because the relay labels it with the real sender.

### Replay and epoch handling

Each client keeps the **current** key and, briefly, the **previous** one:

- A nonce opens at most once per epoch. A replayed message is rejected.
- After a rotation, messages under the previous epoch still open, so messages that were in
  flight during the rekey are not lost. The previous key is dropped as soon as the first
  message under the current epoch opens. Anything older than the previous epoch is rejected.

### Owner change

When the owner leaves, the relay names the oldest remaining member as the new owner. Every
client then:

1. discards the room key in use immediately and remembers its epoch as **retired**. Nothing
   is sent until a new key is in place;
2. if it is the new owner, generates a fresh key with the epoch after the retired one;
3. if it is a member, runs a fresh Noise handshake with the new owner, who sends it the new key.

A key whose epoch equals the retired one is refused, so the departed owner's key can never be
reinstated. Each member gets a new fingerprint with the new owner.

### Handshake injection

The owner answers a handshake only from a member that does not already have a session with
it. A second handshake that appears to come from an established member, which a malicious relay
could inject at any time, is ignored. It can neither replace the session whose fingerprint was
already compared nor receive the next room key.

Clients also drop: payloads from nicks that are not members of the room, key deliveries from
anyone but the current owner, handshake replies from anyone but the current owner, and any
payload that fails to decode or decrypt.

## Fingerprints

When a handshake completes, both sides derive a fingerprint from the Noise handshake hash: the
first 8 bytes, shown as `XXXX-XXXX-XXXX-XXXX`.

- A member sees their fingerprint with the owner in the status bar.
- The owner sees each member's fingerprint on that member's `joined` line.
- After an owner change, every member has a new fingerprint with the new owner.

If the relay performs a man-in-the-middle attack, it runs one handshake with the member and
another with the owner, and the two fingerprints differ. **Compare the fingerprint with the
owner over another channel** (in person, by phone, on a channel you already trust). If they
match, nobody sits between you. pombo-irc has no long-term identities by design, so this
comparison is the only defense against an active relay.

### Why 64 bits and not 32

A relay attempting a man in the middle controls one ephemeral key in each of the two
handshakes. Once the handshake with the owner is fixed, it can try ephemeral keys in the
handshake with the member until the first bytes of both handshake hashes match, and then show
both victims the same fingerprint. With a 32-bit fingerprint that takes about 2^32 attempts,
which is cheap for a GPU; the client's handshake timeout is not a margin worth relying on. With
64 bits it takes about 2^64 attempts, which is out of reach. Sixteen hex digits are still short
enough to read aloud.

## Known limits

- **Insider plus colluding relay can forge a nick.** All members share the room key. A member
  acting alone cannot impersonate another member, because the relay labels each payload with
  its true sender. A member who colludes with the relay can: the relay can label that member's
  message with someone else's nick, and it will open correctly. Signing every message per member
  would close this; it is not implemented.
- **Metadata is visible to the relay.** See [What the relay sees](#what-the-relay-sees). Padding
  hides exact lengths, not volume or timing. Typing indicators are encrypted, but their timing
  reveals that someone is typing.
- **A short window around a leave.** A message sealed by a member who has not yet received the
  post-leave key is still under the old epoch. An honest relay no longer forwards anything to
  the departed member; a malicious relay could forward those few in-flight messages to them.
- **Noise keys are not `mlock`ed.** Handshake and transport state live inside the `snow`
  library, where the client cannot lock them. They can reach swap. Use encrypted swap, or none.
- **Connection limits are per connection, not per host.** The relay caps total connections and
  rate-limits each connection, but behind a reverse proxy every connection comes from the
  proxy's address, so one host can open many connections. Per-address limits belong in the
  proxy.
- **Terminal scrollback is best effort.** Leaving a room clears the screen and sends the
  "clear scrollback" sequence, which most terminal emulators honor. Terminal multiplexers
  (tmux, screen), session recorders and terminals configured to log output may keep a copy.
- **Ephemeral means unverifiable identity.** A nick is whatever someone typed; nicks are unique
  only among people online at the same moment.

## Terminal escape sanitization

Everything that comes from the network and reaches the screen (nicks, room names, owner names,
message text) passes through one sanitizer before display. It removes:

- every ESC-introduced sequence: CSI, OSC, DCS, SOS, PM and APC strings up to BEL or ST,
  and two- and three-byte escapes;
- their single-byte C1 equivalents;
- all other C0 and C1 control characters and DEL;
- Unicode bidirectional overrides and isolates, which could reorder what is shown.

Tabs become a space, and an unterminated sequence swallows the rest of the input rather than
leaking into the terminal. A remote message therefore cannot move the cursor, change the window
title, create hyperlinks, query the terminal, or rewrite earlier lines. In addition, nicks and
room names are restricted to `[A-Za-z0-9_-]` by the relay, and the client checks every nick the
relay hands it against the same rule.

## Memory hardening

"Nothing on disk" leaks in practice through core dumps and swap. At startup the client:

- marks the process non-dumpable (`prctl(PR_SET_DUMPABLE, 0)`), which also blocks other
  processes of the same user from attaching with `ptrace`;
- sets `RLIMIT_CORE` to 0.

While running it:

- `mlock`s the input buffer, best effort (failure is not fatal);
- keeps room keys and decrypted text in buffers that are zeroized when dropped, and wipes the
  input line after each submit;
- discards all key material when you leave a room or the connection drops.

The relay's example systemd unit sets `LimitCORE=0` and runs the server under a dynamic,
unprivileged user with a read-only view of the system.

## Reporting a vulnerability

Please report security issues privately through a
[GitHub security advisory](https://github.com/jaopdc11/pombo-irc/security/advisories/new) on
this repository. Do not open a public issue for them.

Include what you found, how to reproduce it, and what an attacker gains. You will get an answer
as soon as possible, and a fix will be coordinated with you before details are made public.
