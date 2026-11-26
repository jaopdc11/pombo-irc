<p align="center">
  <img src="docs/assets/logo.svg" alt="pombo-irc" width="760">
</p>

<p align="center">
  Ephemeral, end-to-end encrypted group chat for the terminal, over a blind relay.
</p>

---

pombo-irc is a throwaway chat room you open from a terminal, use for as long as the
conversation lasts, and close. Pick a nick, open a room or knock on one, and talk with up to
seven other people. The relay in the middle only forwards bytes it cannot read; it keeps
everything in memory and logs nothing. There are no accounts, no history and no files on disk.
When you leave, your keys are gone.

It is meant for the situations where you want a quick private channel with a few people and
nothing left behind afterwards: a working session, an incident call, a conversation you do not
want sitting in someone's chat history. *Pombo* is Portuguese for pigeon: it carries the message
without opening it.

## Features

- **Rooms of up to 8 people.** Rooms are named. The creator owns the room and decides who gets
  in; people knock, the owner answers.
- **End-to-end encryption.** Each member runs a Noise handshake with the owner, who hands out a
  shared room key. Messages are sealed with XChaCha20-Poly1305 and padded to hide their length.
- **Key rotation on every join and leave.** Newcomers cannot read what was said before they
  arrived; people who left cannot read what is said after.
- **Blind relay.** The server routes opaque payloads between room members. It never sees
  message content or keys, keeps all state in memory and writes no logs.
- **Nothing stored.** No accounts, no history, no configuration files. Leaving a room clears the
  screen and asks the terminal to drop its scrollback.
- **Fingerprints.** Every member can compare a short fingerprint with the owner over another
  channel to rule out a man in the middle.
- **Works on restrictive networks.** The client speaks WebSocket over TLS on port 443.
- **Hardened client.** Other processes of the same user cannot read its memory, crash dumps are
  disabled (Linux) or leave the heap out (Windows), the input buffer is locked in memory, and
  all text received from the network is stripped of terminal escape sequences before display.

## A session

The real interface is colored and laid out with panels and a status bar; this is the same
session as plain text. `ana` opens a room, `bruno` knocks, `ana` lets him in.

```text
$ pombo
nick › ana
  1  create room            2  enter room
room name › night-shift
✓ room night-shift open as ana  ·  you own this room
  ⠋ waiting for people to knock

▌ request  bruno wants to join
▌  \y  accept    \n  refuse
› \y
→ bruno joined  ·  fingerprint 7F3A-91C2-0B4E-D15A

bruno  21:04
│ fingerprint matches on my side

                                                       21:04  ana
                                   matches here too. go ahead ┃

← bruno left
› \q
```

On `bruno`'s side the room list shows `night-shift`, he picks it, waits for `ana` to answer,
and then sees the same fingerprint, `7F3A-91C2-0B4E-D15A`, pinned in his status bar.

## Install

pombo-irc runs on Linux and Windows 10 or later. Building it needs [Rust](https://rustup.rs)
and a C compiler (the TLS library compiles some C).

### Linux

```sh
git clone https://github.com/jaopdc11/pombo-irc.git
cd pombo-irc
./install.sh
pombo
```

`install.sh` builds the client from source and installs it as `~/.local/bin/pombo`. If
`~/.local/bin` is not on your `PATH`, the script tells you what to add.

To install system-wide instead:

```sh
PREFIX=/usr/local sudo -E ./install.sh
```

The relay only accepts clients built from the same commit it runs. When it is updated, `pombo`
tells you so and refuses to start; update from the folder you cloned:

```sh
git pull && ./install.sh
```

`pombo --version` prints the commit your client was built from.

To uninstall (use the same `PREFIX` you installed with):

```sh
./install.sh --uninstall
```

To build by hand, `cargo build --release -p pombo-irc` produces `target/release/pombo`. A
fully static binary can be built with
`cargo build --release --target x86_64-unknown-linux-musl -p pombo-irc` after
`rustup target add x86_64-unknown-linux-musl`.

### Windows

Install [Rust](https://rustup.rs) with `rustup-init.exe` (let it set up the Visual Studio Build
Tools when it asks; they bring the linker and the C compiler) and [git](https://git-scm.com).
Then, in PowerShell:

```powershell
git clone https://github.com/jaopdc11/pombo-irc.git
cd pombo-irc
powershell -ExecutionPolicy Bypass -File .\install.ps1
```

`install.ps1` builds the client and installs it as `%LOCALAPPDATA%\Programs\pombo\pombo.exe`,
adding that folder to your user `PATH`. Open a new terminal and run `pombo`. To update, run
`git pull` and the script again; to uninstall, run it with `-Uninstall`. git is required: the
build stamps the commit into the binary, and the relay refuses clients without it.

Use [Windows Terminal](https://aka.ms/terminal) (the default on Windows 11). The legacy console
window works but draws the logo and panels poorly. Pasting several lines at once sends them as
separate messages, since the Windows console delivers a paste as individual key presses.

A Windows binary can also be cross-built from Linux with
[cargo-zigbuild](https://github.com/rust-cross/cargo-zigbuild):
`cargo zigbuild --release --target x86_64-pc-windows-gnu -p pombo-irc --bin pombo` after
`rustup target add x86_64-pc-windows-gnu`.

## Usage

Run `pombo`, type a nick (1 to 16 letters, digits, `_` or `-`), then choose:

1. **create room**: give it a name (1 to 24 letters, digits, `_` or `-`). You own it and wait
   for people to knock.
2. **enter room**: pick a room from the list and knock. The owner has 60 seconds to answer.

| Key or command  | Where             | What it does                                                      |
|-----------------|-------------------|-------------------------------------------------------------------|
| `↑` `↓`, number | room list         | Select a room; Enter knocks on it.                                |
| `r`             | room list         | Refresh the list.                                                 |
| `\y`            | in a room (owner) | Accept the newest knock.                                          |
| `\n`            | in a room (owner) | Refuse the newest knock.                                          |
| `\q`            | room, list, knock | Leave (only you leave) and return to the nick prompt.             |
| `Esc`           | anywhere          | Clear the input line.                                             |
| `Ctrl+C`        | anywhere          | Leave the room or lobby; at the nick prompt or menu, quit.        |

Knocks stack: the owner always sees the most recent one, with `+N more waiting` when several
are pending. A knock that is not answered within 60 seconds counts as refused. Full rooms are
not listed.

If the owner leaves, the oldest remaining member becomes the owner. If everyone else leaves,
the last person stays in the room as its owner and the room is listed again.

### Configuration

| Variable     | Used by | Default                         | Meaning                              |
|--------------|---------|---------------------------------|--------------------------------------|
| `POMBO_URL`  | client  | `wss://pombo.api.jaopd.dev` | Relay to connect to.                 |
| `POMBO_BIND` | server  | `127.0.0.1:7466`                | Address and port the relay binds.    |
| `PORT`       | server  | unset                           | Port on `127.0.0.1`, used only when `POMBO_BIND` is unset. |

```sh
POMBO_URL=wss://chat.example.org pombo
```

## How it works

```text
  member ──┐
  member ──┼── wss ──►  blind relay  ◄── wss ── owner
  member ──┘
            routes by nick and room; payloads are opaque bytes
```

- The relay knows nicks, room names and who is in which room, because it has to route. It
  forwards everything else as opaque bytes and adds only the sender's nick.
- When someone is let in, they run a Noise `NN` handshake with the owner through the relay.
  That pairwise session exists only to carry room keys.
- The owner generates a fresh 32-byte room key on every join and every leave and sends it to
  each member over their pairwise session.
- Room messages and typing indicators are sealed with the room key (XChaCha20-Poly1305) and
  broadcast through the relay.

The full wire format is in [docs/PROTOCOL.md](docs/PROTOCOL.md).

## Security model

The relay is trusted to route, not to read. It sees metadata (nicks, room names, membership,
timing, padded message sizes, connection addresses) but never message content or keys. A
malicious relay could attempt a man in the middle on the handshake with the owner; comparing
fingerprints out of band is what detects it. There are no long-term identities by design, so
the fingerprint is the only defense against that attack.

Read [docs/SECURITY.md](docs/SECURITY.md) for the threat model, the cryptographic design, the
known limits, and how to report a vulnerability.

## Self-hosting

The relay is a single binary, `pombo-irc-server`, meant to run on loopback behind a TLS reverse
proxy such as Caddy. It holds no state on disk and needs no database. Templates for a systemd
unit and a Caddy site block are in [`deploy/`](deploy/).

See [docs/SELF_HOSTING.md](docs/SELF_HOSTING.md) for building, deploying and pointing clients at
your relay.

## Forking

Forks are welcome. If you distribute a modified version, credit this project and point it at a
relay you run instead of the default one. [docs/FORKING.md](docs/FORKING.md) lists exactly what
to change.

## License

pombo-irc is released under the [pombo-irc License](LICENSE): the MIT License plus two
conditions, attribution and not using the original author's relay as the default server of a
modified version. Because of those conditions it is not an OSI-approved open source license.

Contributions are welcome; see [CONTRIBUTING.md](CONTRIBUTING.md).
