# Self-hosting the relay

The relay, `pombo-irc-server`, is a single binary with no configuration file, no database and
no state on disk. It listens for plain WebSocket connections on loopback; a reverse proxy in
front of it terminates TLS on port 443.

The templates referenced here live in [`deploy/`](../deploy/). Review and adapt them before
installing; they name the original author's host.

## Requirements

- A Linux host with a public address and a DNS name pointing at it.
- A reverse proxy that handles TLS and WebSocket upgrades. The examples use
  [Caddy](https://caddyserver.com), which obtains certificates automatically.
- Rust to build, on the host or on another Linux machine. The relay has no C dependencies; only
  the client needs a C compiler.

## Build

```sh
git clone https://github.com/jaopdc11/pombo-irc.git
cd pombo-irc
cargo build --release --locked -p server
sudo install -m 755 target/release/pombo-irc-server /usr/local/bin/pombo-irc-server
```

A static build, for copying to another machine:

```sh
rustup target add x86_64-unknown-linux-musl
cargo build --release --locked -p server --target x86_64-unknown-linux-musl
```

## Configuration

The relay reads two environment variables:

| Variable     | Default          | Meaning                                                              |
|--------------|------------------|----------------------------------------------------------------------|
| `POMBO_BIND` | `127.0.0.1:7466` | Address and port to listen on.                                       |
| `PORT`       | unset            | Port to listen on at `127.0.0.1`. Used only when `POMBO_BIND` is unset, for process managers that assign a port. |

Keep the relay on loopback. It speaks plain WebSocket; exposing it directly would send nicks and
room names over the network unencrypted.

Limits (connection cap, rate limit, timeouts) are compiled-in defaults in `ServerConfig`,
[`crates/server/src/lib.rs`](../crates/server/src/lib.rs). They are listed in
[PROTOCOL.md](PROTOCOL.md#relay-timeouts-and-limits).

## Run it under systemd

[`deploy/pombo-irc-server.service`](../deploy/pombo-irc-server.service) runs the relay under a
dynamic unprivileged user, with a read-only view of the system, no core dumps and no network
families other than IPv4 and IPv6.

```sh
sudo cp deploy/pombo-irc-server.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now pombo-irc-server
systemctl status pombo-irc-server
```

Change `Environment=POMBO_BIND=...` in the unit if you want another port.

## Put it behind TLS

### Caddy

[`deploy/Caddyfile.snippet`](../deploy/Caddyfile.snippet) is a complete site block. Replace the
host name with yours:

```caddyfile
chat.example.org {
	reverse_proxy 127.0.0.1:7466
}
```

Append it to your Caddyfile and reload Caddy. Caddy proxies WebSocket upgrades without extra
configuration and does not write access logs unless a `log` directive asks it to. **Do not add
one.**

### nginx

If you use nginx instead:

```nginx
server {
    listen 443 ssl http2;
    server_name chat.example.org;
    # ssl_certificate / ssl_certificate_key as usual

    access_log off;

    location / {
        proxy_pass http://127.0.0.1:7466;
        proxy_http_version 1.1;
        proxy_set_header Upgrade $http_upgrade;
        proxy_set_header Connection "upgrade";
    }
}
```

The relay pings every client every 30 seconds, so nginx's default 60-second read timeout is
enough.

### Health check

The relay answers `GET /saude` with `200 ok`, so a load balancer or deploy tool can check it
without opening a WebSocket:

```sh
curl https://chat.example.org/saude
```

## Point clients at your relay

Per invocation, with an environment variable:

```sh
POMBO_URL=wss://chat.example.org pombo
```

Or change the compiled-in default. It is the `DEFAULT_URL` constant used by `server_url()` in
[`crates/client/src/core.rs`](../crates/client/src/core.rs):

```rust
/// Server used when `POMBO_URL` is not set.
const DEFAULT_URL: &str = "wss://chat.example.org";
```

Then rebuild and reinstall the client with `./install.sh`. If you distribute that client to
other people, read [FORKING.md](FORKING.md) first.

## Logging policy

pombo-irc's privacy depends on the relay keeping nothing, so the whole chain must agree:

- **The relay** prints one line at startup (`pombo-irc-server listening on ...`) and fatal
  errors. It never logs connections, nicks, room names, addresses or payloads.
- **The reverse proxy** must have access logging off. This is where client IP addresses and
  connection times would otherwise end up.
- **journald** receives only what the relay prints, which is the startup line.
- **Do not** put the relay behind tools that record traffic, such as request-logging CDNs,
  WAFs in logging mode, or packet captures.

Error logs of the proxy can still contain client addresses when a connection fails. Set their
level to what you are comfortable keeping, and rotate them aggressively.

## Abuse limits

The relay caps total connections and rate-limits each connection, but behind a proxy every
connection comes from the proxy's own address, so the relay cannot limit per client. If you need
per-address limits (connections per IP, requests per second), configure them in the proxy or in
the firewall.

## Upgrading

The relay has no state worth keeping: rebuild, replace the binary and restart the service.
Rooms in progress end; clients see the connection drop and return to the nick prompt.

Clients and relay must speak the same protocol. If you change
[`crates/proto`](../crates/proto/src/lib.rs), ship the new client and relay together.
