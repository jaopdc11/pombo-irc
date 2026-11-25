# Forking pombo-irc

Forks are welcome: change the interface, add features, run it for your team, package it for your
distribution. The [license](../LICENSE) is MIT with two extra conditions. This page explains
what they mean in practice and exactly what to change.

## What the license asks of you

If you distribute a modified version of pombo-irc, in source or binary form:

1. **Keep the credit.** Keep the `LICENSE` file with its copyright notice, and credit the
   original project in your README or in an about screen, for example:

   ```text
   Based on pombo-irc by jaopdc11, https://github.com/jaopdc11/pombo-irc
   ```

2. **Change the default server.** Your version must not use the original author's relay (any
   host under `jaopd.dev`) as its default or built-in server. Point it at a relay you operate,
   or make users configure one.

Using pombo-irc, or modifying it for yourself without distributing it, triggers neither
condition. Redistributing an unmodified copy only requires keeping the notice and credit, which
the original files already contain.

The original relay is a small, personal service sized for the original client's users. A fork
that ships with it as the default would send all of that fork's users there, which is why the
second condition exists.

## What to change

### Required: the default server

The client's compiled-in relay is the `DEFAULT_URL` constant in
[`crates/client/src/core.rs`](../crates/client/src/core.rs), read by `server_url()`:

```rust
/// Server used when `POMBO_URL` is not set.
const DEFAULT_URL: &str = "wss://pombo.api.jaopd.dev";
```

Set it to your relay, for example `wss://chat.example.org`. If you would rather not have a
default at all, change `server_url()` to fail with a clear message when `POMBO_URL` is unset.

Also update the places that mention the original host:

- [`deploy/Caddyfile.snippet`](../deploy/Caddyfile.snippet): the site address;
- `README.md`: the configuration table.

You can find every mention with:

```sh
grep -rn "jaopd.dev" --exclude-dir=target .
```

### Required: run your own relay

Changing the URL is not enough on its own; something has to answer there. Follow
[SELF_HOSTING.md](SELF_HOSTING.md) to build and deploy `pombo-irc-server` behind TLS.

### Optional: name and branding

None of this is required, but it avoids confusing your users with the original project:

- the command name: `[[bin]] name` in [`crates/client/Cargo.toml`](../crates/client/Cargo.toml)
  and the `pombo` references in [`install.sh`](../install.sh);
- the logo and tagline: [`crates/client/src/ui/logo.rs`](../crates/client/src/ui/logo.rs) and
  [`docs/assets/logo.svg`](assets/logo.svg);
- the colors: [`crates/client/src/ui/theme.rs`](../crates/client/src/ui/theme.rs);
- the `pombo-irc` brand chip in the status bar, in
  [`crates/client/src/ui/app.rs`](../crates/client/src/ui/app.rs);
- the clone URL and project name throughout `README.md`.

## Compatibility

A client and a relay interoperate only if they speak the same protocol
([PROTOCOL.md](PROTOCOL.md)). If you change [`crates/proto`](../crates/proto/src/lib.rs) or the
end-to-end format in [`crates/e2e`](../crates/e2e/src/lib.rs), your client works only with your
relay, which you are running anyway.

## Security fixes

If you find a security issue that also affects this project, please report it upstream through
a private [GitHub security advisory](https://github.com/jaopdc11/pombo-irc/security/advisories/new).
