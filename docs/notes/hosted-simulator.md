---
topic: deployment
triggers: [deploy-sim.yml, deploy-admin.yml, deploy-web.yml, http.rs, ws_transport.rs, "web.cartopolis.org", "hosted sim", "public server", caddy, systemd, admin, accounts, "--http-listen", "--create-admin", "/go", well-known]
updated: 2026-08-13
---

# The hosted simulator, and the two surfaces in front of it

There is a public simulator on `code.garage44.eu` (the same box as Forgejo,
nominatim, overpass, the tile server and the CI runner). It exposes three things
and each has a different audience:

| Address | Who | What |
|---|---|---|
| `wss://cartopolis.org/ws` | **every** client — desktop, Android, browser | the wire (ADR-008/009), one binary WS message per frame, TLS terminated by Caddy |
| `https://web.cartopolis.org` | operators | the account admin GUI — [`jeroen/cartopy`](https://code.garage44.eu/jeroen/cartopy), a separate repo |

Two more paths on `cartopolis.org` are Caddy routes rather than services, and both
belong to place links ([place-links.md](place-links.md)): `/go` is rewritten to
`index.html` so a shared link has something to open, and
`/.well-known/assetlinks.json` is what an Android phone reads to decide the app
may handle it. The second is published by `deploy-web.yml` out of the trunk dist —
`rsync --delete` removes anything under the web root that the build did not
produce, so a file placed there by hand survives exactly until the next deploy.

**There is no plaintext port.** The simulator's TCP listener is bound to
`127.0.0.1` and its HTTP listener to the docker0 gateway, so nothing it serves is
reachable from off the machine except through Caddy. The address to give a person
is `cartopolis.org` — no scheme, no port; `network::resolve_target` turns a bare
hostname into `wss://cartopolis.org/ws`.

## One connection loop, two carriers

`net::handle_connection` is generic over `net::FrameTransport` — anything that is
a `Stream` of frames and a `Sink` of them. TCP supplies that through
`LengthDelimitedCodec`; the browser supplies it through `http::WsFrames` over
axum's WebSocket. **The frame body is identical**, so a WS carrier is not a
protocol revision and `PROTOCOL_HISTORY` did not gain a row for it.

The alternative was a second copy of a 700-line select loop carrying its own rate
limiters, chat replay and avatar teardown. Two loops that must stay identical, and
nothing that says when they stop being — the transport refactor was cheaper than
the first divergence would have been.

`tests/http_surface.rs` pins the property that matters: a token minted over
WebSocket authenticates over TCP. They are one server sharing one session store,
not two that happen to share a database.

## Why the admin API is HTTP and not the wire

Account management is an operator action, not gameplay. Putting it on
`NetMessage` would run an authorisation decision through the same path as a
movement intent, and would make every client's protocol version move whenever the
*admin* surface changed. It is a separate surface with a separate audience; what
it shares with the game is the account table and the session map, which is the
point — an admin signs in with an ordinary account, and disabling someone takes
effect on their next handshake.

**Disabling does not evict a live session.** `Sessions` is an in-memory map keyed
by token with no principal index, so `disabled = 1` is a gate on the *next*
`ClientHello`. Kicking someone who is already connected needs their token
(`Sessions::revoke`) or a restart. Fixing it properly means indexing sessions by
principal; nobody has needed it yet.

## The last-admin guard

Demoting, disabling or deleting the last enabled admin is refused with a 409. It
is checked before the write rather than repaired after, because there is no repair
from a running server with no admin except shell access to the box — at which
point the way back in is `simulator --create-admin <user>` with
`CARTO_ADMIN_PASSWORD` set, which is idempotent and doubles as the password reset.

The password comes from the environment and not a flag on purpose: an argument is
in the process table and in the operator's shell history, and this is the one
credential that grants itself everything else.

## Deployment shape, and why it is not a container

The simulator is a **systemd unit running a native binary**, not a service in
`/data/stack/docker-compose.yml` like everything else on that host. The reason is
that the Forgejo runner and the host are the same machine: CI publishes with a
file copy, so there is no registry to push an image to and no image to build.
Forgejo itself is deployed the same way for the same reason, and Caddy reaches
both across the docker0 gateway (`172.17.0.1`).

CI cannot call `systemctl` — it runs in a container and giving a CI job the host's
init system would be a much larger grant than "may write these files". So
`deploy-sim.yml` writes the binary and touches `restart.stamp`;
`cartopolis-sim-reload.path` on the host watches that file and restarts the
service. The privilege stays on the host.

The binary is written to `simulator.new` and `mv`d into place. An interrupted copy
would otherwise leave a truncated file that `Restart=always` loops on forever.

## Measured / observed, 2026-08-13

- `axum` adds ~40 crates to the simulator's graph and nothing to the client's —
  the client has never depended on `cartopolis_simulator` except through the
  `solo` feature, which is off on both web and Android.
- The admin GUI builds to **35 KB** of JavaScript. That is what makes it a static
  site plus a Caddy route rather than a Bun service: there is no server-side
  rendering to do and no session to keep.
- `cargo check -p cartopolis --target wasm32-unknown-unknown` **fails on `mio`**
  unless you pass `--no-default-features`. The default features include `solo`,
  which pulls the simulator and therefore tokio. That is not a regression, it is
  how the web build has always been configured (`data-cargo-no-default-features`
  in `crates/cartopolis/index.html`) — but the naive check command looks like it
  found a real break. It also needs `RUSTUP_TOOLCHAIN=nightly`, because
  `wasm_thread` uses `#![feature(stdarch_wasm_atomic_wait)]`.

## Rejected: the admin GUI as a Bun service

Codebrew's own apps are Bun services — `Bun.serve`, a WebSocket API router, a
session middleware. Copying that here would have meant a second long-running
process on the box, a second thing to supervise and restart, and a second place
holding credentials, in order to serve 35 KB of static files and forward JSON to
a Rust process that was already answering HTTP. What was worth borrowing from
codebrew is its *front end*: the deepsignal store with its persistent/volatile
split, and the `App.init` + `components/{pages,elements}` layout. Those are in
[`jeroen/cartopy`](https://code.garage44.eu/jeroen/cartopy) and the backend is nowhere — the GUI is static
files Caddy serves, and Caddy proxies `/api/*` to this simulator.

The GUI lives in its own repository because it shares nothing with this one but
an HTTP contract: no crate, no build, no lockfile, no CI lane. The cost is the
one seam worth naming — its `src/types.ts` describes what `auth::AccountRow`
serialises, and with the two apart nothing but a person keeps them in step.

## Rejected: CORS on the simulator

The GUI is on `web.cartopolis.org` and the API is on the simulator, so the obvious
move is an `Access-Control-Allow-Origin` layer. Caddy proxies `/api/*` on the
admin hostname to the simulator instead, which makes every request same-origin —
there is then no preflight, no origin allowlist to keep in step with the
deployment, and no CORS misconfiguration that could expose the admin API to a
page the operator was reading. Same trick as `/up/nomads/` for NOAA and for the
same reason (ADR-034 calls one origin the fix).

## Still open

- **`web.cartopolis.org` needs an A record.** `cartopolis.org` has no wildcard —
  verified 2026-08-13, `sim.` and a random subdomain both NXDOMAIN — and DNS for
  this zone is not self-serve from this container. Until the record exists, Caddy
  retries the certificate with backoff and the rest of the file is unaffected.

## The CI registry cache could not be shared, and said so in three voices

Unrelated to the simulator, found while landing it, and recorded here because the
next person to see it will also be looking at a red lane and their own diff.

`ci.yml`'s two jobs both bind-mounted `/data/ci-cache/cargo-registry` onto
`/root/.cargo/registry`, under a comment claiming cargo's file lock made that safe
between concurrent jobs. It does not: the lock is `$CARGO_HOME/.package-cache`, a
file one level *above* the mount, so each container took a private lock over
shared data. Fixed 2026-08-13 by giving `targets` its own copy.

The reason it costs a day rather than a minute is that it never looks like a cache
problem. Three failures on `main` that day, all this one cause:

- `failed to unpack package libc v0.2.178 … .cargo-ok: File exists (os error 17)`
- `error[E0583]: file not found for module 'float'` — **inside glam's own source**
- `could not compile uuid … could not execute process 'sccache …' (never executed)`

`.cargo-ok` is written last, so an interrupted extract leaves a directory cargo can
neither use nor replace, and a concurrent reader sees a crate whose source really is
incomplete. It is intermittent, and it bites hardest immediately after a lockfile
change — which is exactly when a dependency error reads as "something I just did".

**Check the cache before the code.** `ls /data/ci-cache/cargo-registry/src/*/ | wc -l`
and look for a crate directory without a `.cargo-ok`.

## Every build uses TLS, and opting out takes a scheme

The first cut of this had desktop and Android clients on a public plaintext TCP
port, with the wire's own auth handshake — a password, and then a session token —
crossing the network unencrypted. That is now closed: `network::resolve_target`
maps a bare hostname to `wss://host/ws`, so **the easy thing to type is the
encrypted one**, and the client reuses the same carrier and the same Caddy
certificate the browser build already used.

Three rules, and the shape of them matters more than the list:

- a bare host, or an `https://` one → `wss://…/ws`
- `127.0.0.1` / `localhost` / `::1` / `127.*` → plain TCP
- `tcp://…` or `ws://…` → exactly that, unencrypted, because you said so

Loopback is exempt on purpose. Solo mode's sim is a listener on this machine that
this process just started; TLS to it would mean a self-signed certificate and a
verifier turned off to accept it, which is the *appearance* of security over
traffic that anything able to read could equally read out of this process's own
memory. Encryption there would make the code harder to audit and no safer.

The escape hatches exist because two real cases need them — a private CA
(`webpki-roots` is a compiled-in Mozilla trust store, so a corporate MITM root is
not in it) and a plaintext dev sim on a LAN. Both require typing a scheme. Nobody
reaches either by entering a hostname, which is the property
`a_remote_address_is_always_encrypted` exists to pin.

**rustls, not native-tls.** native-tls is OpenSSL on Linux and Android, and the
CI target lane checks `aarch64-linux-android` with nothing but the NDK's clang —
a vendored OpenSSL build script would break the gate that exists to catch exactly
this kind of dependency. `webpki-roots` over `rustls-native-certs` for the
matching reason: Android has no `/etc/ssl/certs`, and its trust store is a JNI
call this tree has no seam for.
