# Contributing

Bug reports, fixes and improvements are welcome. Open an issue to discuss anything larger than
a small fix before you spend time on it.

## Build and test

You need Rust (stable, via [rustup](https://rustup.rs)) and a C compiler.

```sh
cargo build --workspace
cargo test --workspace
```

Before opening a pull request, make sure these pass:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

To try a change end to end, run a local relay and point two clients at it:

```sh
POMBO_BIND=127.0.0.1:7466 cargo run -p server
POMBO_URL=ws://127.0.0.1:7466 cargo run -p pombo-irc   # in two other terminals
```

## Layout

| Crate           | What it is                                                          |
|-----------------|---------------------------------------------------------------------|
| `crates/proto`  | Wire protocol types shared by client and relay.                     |
| `crates/e2e`    | Noise handshake, room keys, room message sealing, padding.          |
| `crates/server` | The relay, `pombo-irc-server`.                                      |
| `crates/client` | The terminal client, `pombo`.                                       |

## Ground rules

- **Keep the relay blind.** The server must never see plaintext or keys. Anything new that the
  relay needs to read should be justified in the pull request, and is a metadata change worth
  documenting in [docs/SECURITY.md](docs/SECURITY.md).
- **No logging.** The relay must not log connections, nicks, room names, addresses or payloads,
  and nothing, on either side, may be written to disk.
- **Protocol changes** go at the end of the enums in `crates/proto` and must be reflected in
  [docs/PROTOCOL.md](docs/PROTOCOL.md).
- **Text from the network** must pass through the client's sanitizer before it reaches the
  terminal.
- Keep dependencies few. Every new crate is code that runs next to the keys.

## Security issues

Do not open a public issue for a vulnerability. Report it privately through a
[GitHub security advisory](https://github.com/jaopdc11/pombo-irc/security/advisories/new).

## License

By contributing, you agree that your contribution is released under the project's
[license](LICENSE).
