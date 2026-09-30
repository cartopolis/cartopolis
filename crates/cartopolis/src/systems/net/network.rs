//! Client half of the sim link (ADR-008, ADR-009).
//!
//! The TCP carrier — `--connect`, and a loopback dev sim — is here. The browser's
//! WebSocket carrier is in the sibling `ws_transport`, and everything above
//! [`start_link`] is shared: both fill the same mailbox, drive the same
//! [`LinkStatus`], and speak the same frames.

use crate::components::{
    Avatar, NeedsMeshRebuild, NeedsTextureRefresh, Prim, PrimGeo, PrimShape, RemoteAvatar,
};
use crate::resources::{
    AuthIdentity, AvatarState, ConnectAddr, ConnectAuth, GameState, LocalAvatarSimId,
    NetworkMailbox, NetworkSyncState, OnlineSession, PrimTextureCache, TextureLibrary,
};
use crate::systems::user_store::UserStore;
use bevy::ecs::system::SystemParam;

/// Cross-cutting writeback for `apply_network_snapshot` (auth identity, token
/// persistence, chat, the script editor), grouped so the system stays under
/// Bevy's system-parameter count limit (ADR-016 / ADR-028).
///
/// The grouping is a budget, not a theme: everything here is a resource this one
/// system writes on behalf of some other part of the client, and each addition
/// buys back a parameter slot the system does not have.
#[derive(SystemParam)]
pub struct AuthWriteback<'w> {
    identity: ResMut<'w, AuthIdentity>,
    connect_auth: Option<Res<'w, ConnectAuth>>,
    connect_addr: Option<Res<'w, ConnectAddr>>,
    user_store: ResMut<'w, UserStore>,
    chat: ResMut<'w, crate::systems::chat::ChatState>,
    script: ResMut<'w, crate::systems::script_editor::ScriptEditorState>,
    /// Voice arrives on the same mailbox as everything else, so it is decoded
    /// here rather than in a system of its own — a second drain of one queue
    /// would race for the messages.
    voice: ResMut<'w, crate::systems::voice::VoiceState>,
    /// Private-chat traffic, handed on to `private_chat::drive_private_chats`
    /// for the same reason voice is decoded here: one drain of one queue.
    private: ResMut<'w, crate::systems::private_chat::PrivateInbox>,
    /// Call signalling, handed on to `calls::drive_calls` likewise.
    calls: ResMut<'w, crate::systems::calls::CallInbox>,
    /// Contacts and message requests, handed on to `contacts::drive_contacts`.
    contacts: ResMut<'w, crate::systems::contacts::ContactsInbox>,
}
use crate::systems::locomotion;
use crate::systems::model_assets;
use bevy::asset::RenderAssetUsages;
use bevy::prelude::*;
use bevy::render::render_resource::TextureFormat;
#[cfg(not(target_arch = "wasm32"))]
use bytes::Bytes;
use cartopolis_core::{
    motion, snap_yaw_continuation, wrap_angle_pi, AuthCredential, AvatarStateDto, NetMessage,
    PrimDto,
};
#[cfg(not(target_arch = "wasm32"))]
use futures_channel::mpsc::UnboundedReceiver;
#[cfg(not(target_arch = "wasm32"))]
use futures_util::{SinkExt, StreamExt};
use std::collections::HashSet;
use std::sync::{mpsc, Arc, Mutex};
#[cfg(not(target_arch = "wasm32"))]
use tokio::net::TcpStream;
#[cfg(not(target_arch = "wasm32"))]
use tokio_util::codec::{Framed, LengthDelimitedCodec};
// The wire codec itself: only the native build owns a socket to speak it on.
#[cfg(not(target_arch = "wasm32"))]
use cartopolis_core::{decode_app_frame, encode_app_frame, PROTOCOL_VERSION};

#[cfg(not(target_arch = "wasm32"))]
const MAX_FRAME: usize = 32 * 1024 * 1024;

/// How far a connection attempt has got. Written by the network thread, read once
/// a frame by `session::drive_session` — which is what lets the sign-in screen
/// show a spinner and then the *reason* a login failed, rather than the old
/// behaviour of printing to a terminal nobody has open.
///
/// Every variant is live on every target now. It used to carry a
/// `cfg_attr(wasm32, allow(dead_code))`, because the web build had no transport
/// and never got past `start_link`'s wasm arm — that arm is a real WebSocket
/// carrier since, and sets all six.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LinkState {
    /// Opening the socket.
    Connecting,
    /// Socket up, credential sent, waiting for `ServerHelloAck`.
    Authenticating,
    Online,
    /// The link ended on the way: a socket that would not open, or one the
    /// network took away. Carries a message fit to put in front of a user.
    Failed(String),
    /// The server answered and refused every credential offered.
    ///
    /// Apart from [`Self::Failed`] because the two call for opposite responses: a
    /// lost socket is worth trying again (`session` does, on its own), while a
    /// refused credential is refused the same way every time — retrying it is a
    /// loop that ends with nothing but a longer wait for the same message.
    Rejected(String),
    /// An established session ended cleanly (server shut down, or we asked).
    Closed,
}

/// Shared handle on a link's [`LinkState`].
#[derive(Clone)]
pub struct LinkStatus(Arc<Mutex<LinkState>>);

impl LinkStatus {
    /// `pub(crate)` so `session`'s tests can stand in for a network thread.
    pub(crate) fn new() -> Self {
        Self(Arc::new(Mutex::new(LinkState::Connecting)))
    }

    pub fn get(&self) -> LinkState {
        self.0
            .lock()
            .map(|s| s.clone())
            .unwrap_or(LinkState::Failed("link status poisoned".into()))
    }

    /// `pub(crate)` rather than private: the wasm carrier lives in a sibling
    /// module (`ws_transport`) and drives the same state machine the native
    /// thread below does.
    pub(crate) fn set(&self, state: LinkState) {
        if let Ok(mut s) = self.0.lock() {
            *s = state;
        }
    }
}

/// Turn what the user typed into a WebSocket URL.
///
/// The sign-in field says "Server" and people put a host in it — sometimes with a
/// scheme, sometimes with a port — and on the deployed site the right answer is
/// "wherever this page came from". Four shapes are accepted:
///
/// - `wss://sim.example/ws` — used as given; an explicit URL is an instruction.
/// - `https://sim.example` / `http://sim.example` — scheme mapped, `/ws` appended.
/// - `sim.example:4747` — scheme inherited from the page, `/ws` appended.
/// - empty — this page's own origin, which is what the deployed client wants.
///
/// Inheriting the scheme is the load-bearing case: a browser refuses a `ws://`
/// socket from an `https://` page as mixed content, and refuses it the way it
/// refuses everything else — as an opaque connection failure, not as a policy
/// decision anyone can read.
///
/// Lives here, not in `ws_transport`, so it is compiled and tested on the native
/// lane — that module is `cfg(wasm32)` and no CI lane runs a test there, so a
/// unit test inside it would be a test nobody ever runs. `secure_page` and
/// `page_host` are arguments for the same reason: they are the only two facts
/// this needs from a browser, so passing them in is what makes the rest pure.
///
/// It carried a `cfg_attr(not(wasm32), allow(dead_code))` while the browser was
/// its only caller. [`resolve_target`] calls it on native now, so the allow is
/// gone — and note *why* it was needed, because the trap recurs: under `--test`
/// the lib is compiled as a test **binary**, where "public API" stops meaning
/// anything, so a `pub` item whose only real caller sits behind a `cfg` is dead
/// code and `unused = "deny"` makes that an error. `cargo check` passes and
/// `cargo test` does not. Same blind spot CLAUDE.md flags for egui and the
/// building geometry: run the tests, not just a check.
pub fn ws_url(addr: &str, secure_page: bool, page_host: &str) -> String {
    let addr = addr.trim();
    if addr.starts_with("ws://") || addr.starts_with("wss://") {
        return addr.to_string();
    }
    let page_scheme = if secure_page { "wss" } else { "ws" };

    let (scheme, host) = if let Some(rest) = addr.strip_prefix("https://") {
        ("wss", rest)
    } else if let Some(rest) = addr.strip_prefix("http://") {
        ("ws", rest)
    } else if addr.is_empty() {
        (page_scheme, page_host)
    } else {
        (page_scheme, addr)
    };

    let host = host.trim_end_matches('/');
    // A path the caller supplied is theirs; only a bare host gets `/ws`.
    if host.contains('/') {
        format!("{scheme}://{host}")
    } else {
        format!("{scheme}://{host}/ws")
    }
}

/// How a native client should reach the address it was given.
///
/// The rule is **encrypted unless the traffic cannot leave the machine**. A
/// password and a session token cross this link, so plaintext over a network is
/// not a default anyone should be able to get by accident.
///
/// Native-only, along with [`resolve_target`] and `is_loopback`: the browser has
/// exactly one carrier and no choice to make, so on wasm these are not merely
/// unused, they are meaningless. `cfg` rather than `allow(dead_code)` — an item
/// that should not exist on a target is better absent than excused.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// Length-delimited TCP straight to a socket. Loopback only by default: this
    /// is a dev sim on this machine, where "unencrypted" means the bytes go from
    /// one end of a socket on this machine to the other. TLS there would mean a
    /// self-signed certificate and a disabled verifier, which is worse than
    /// nothing — it looks like security and is not — for traffic that anything
    /// able to read it could equally read out of this process's own memory.
    Tcp(String),
    /// The same wire inside WebSocket frames, over TLS unless the caller went out
    /// of their way. Identical to what the browser build speaks, so the server
    /// needs no third code path and Caddy terminates the TLS for both.
    WebSocket(String),
}

/// Decide how to reach `addr`, and say so in a way that can be unit-tested.
///
/// - `wss://…` / `ws://…` — used as given; an explicit URL is an instruction, and
///   `ws://` is the escape hatch for a private CA or a plaintext dev sim.
/// - `tcp://host:port` — plain TCP on purpose, for a LAN sim with no certificate.
/// - `127.0.0.1:x`, `localhost:x`, `[::1]:x` — TCP. A dev sim on this machine.
/// - anything else — `wss://host/ws`. **Bare hosts get TLS**, which is the whole
///   point: the address a person types is the address that ends up encrypted.
///
/// A bare host keeps whatever port it was given (`sim.example:8443` →
/// `wss://sim.example:8443/ws`), because a non-default port is a fact about the
/// deployment and dropping it would silently connect somewhere else.
#[cfg(not(target_arch = "wasm32"))]
pub fn resolve_target(addr: &str) -> Target {
    let addr = addr.trim();
    if addr.starts_with("ws://") || addr.starts_with("wss://") {
        return Target::WebSocket(addr.to_string());
    }
    if let Some(rest) = addr.strip_prefix("tcp://") {
        return Target::Tcp(rest.to_string());
    }
    if is_loopback(addr) {
        return Target::Tcp(addr.to_string());
    }
    // `ws_url` with `secure_page = true`: on native there is no page to inherit a
    // scheme from, and the answer we want for a bare host is the secure one.
    Target::WebSocket(ws_url(addr, true, ""))
}

/// Whether this `host:port` names this machine.
///
/// Text, not a DNS resolution: this runs before connecting and must not block,
/// and a name that *resolves* to loopback but was typed as a public host is a
/// case where the person's intent was the network. A local dev sim is a literal
/// `127.0.0.1:port` from its own bound listener, which is what this recognises.
#[cfg(not(target_arch = "wasm32"))]
fn is_loopback(addr: &str) -> bool {
    let host = match addr.rsplit_once(':') {
        // `[::1]:4747` — strip the brackets IPv6 needs when a port follows.
        Some((h, _)) => h,
        None => addr,
    };
    is_loopback_host(host)
}

/// Whether a bare host — no port — names this machine.
///
/// Split out of [`is_loopback`] and compiled on **every** target because the
/// sign-in card asks the same question on the web: "is this address one where a
/// port number still means a socket?" It is the predicate that decides whether
/// `normalize_server` may complete an address with the default sim port, and a
/// wasm build that could not ask it would be back to appending `:4747` to a
/// hostname it is about to reach over `wss`.
#[must_use]
pub fn is_loopback_host(host: &str) -> bool {
    let host = host.trim_start_matches('[').trim_end_matches(']');
    host == "127.0.0.1" || host == "localhost" || host == "::1" || host.starts_with("127.")
}

/// Everything one connection attempt produces. The caller inserts these as
/// resources — this function deliberately takes no `Commands`, because it runs
/// both at startup and from the sign-in screen, and only the caller knows which
/// of the surrounding resources it is also replacing.
pub struct Link {
    pub mailbox: NetworkMailbox,
    pub session: OnlineSession,
    pub status: LinkStatus,
}

/// Open a session to `addr`, presenting `auth` (and a persisted `token` first,
/// when there is one for this server).
///
/// The tile template is *borrowed*, not replaced: the resource is inserted once at
/// startup and its `Arc` is what every streamer reads, so handing the thread the
/// live one means a mid-session sign-in updates the basemap in place instead of
/// swapping a resource half the map already captured. A local `CARTO_TILE_URL`
/// still wins over whatever the server advertises — it exists precisely to point a
/// dev client at a different basemap than the sim's default (see `client_loop`).
pub fn start_link(
    addr: &str,
    auth: &ConnectAuth,
    token: Option<String>,
    tile_template: Arc<Mutex<String>>,
) -> Link {
    let (out_tx, out_rx) = mpsc::channel::<NetMessage>();
    let (intent_tx, intent_rx) = futures_channel::mpsc::unbounded();
    let connect_to = addr.to_string();
    let status = LinkStatus::new();

    // Prefer a persisted session token for this server (silent reconnect), with
    // the resolved credential as fallback in case the token is stale (ADR-016).
    let (primary, fallback) = match token {
        Some(token) => (AuthCredential::Token(token), Some(auth.credential.clone())),
        None => (auth.credential.clone(), None),
    };

    // The sim transport is tokio/TCP — native only. The web build is Phase-1
    // Viewer (no sim; ADR-031/033), so on wasm this never runs (the sign-in screen
    // offers no sim there); the moved values are consumed below so setup still
    // compiles.
    #[cfg(not(target_arch = "wasm32"))]
    {
        let status_for_thread = status.clone();
        std::thread::spawn(move || {
            let rt = match tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    status_for_thread.set(LinkState::Failed(format!("tokio runtime: {e}")));
                    return;
                }
            };
            match rt.block_on(client_loop(
                connect_to,
                primary,
                fallback,
                out_tx,
                intent_rx,
                tile_template,
                status_for_thread.clone(),
            )) {
                // The loop only returns `Ok` when the stream ended — either the
                // server hung up or our `OnlineSession` was dropped (a sign-out).
                Ok(()) => status_for_thread.set(LinkState::Closed),
                Err(e) => {
                    tracing::warn!("network client ended: {e:#}");
                    let state = match e.downcast_ref::<Rejected>() {
                        Some(Rejected(message)) => LinkState::Rejected(message.clone()),
                        None => LinkState::Failed(friendly_error(&e)),
                    };
                    status_for_thread.set(state);
                }
            }
        });
    }
    // The browser has no TCP socket, so it speaks the same wire over a WebSocket
    // (`systems::ws_transport`, and `cartopolis_simulator::http` at the other
    // end). No thread and no runtime here: `spawn_local` puts the loop on the one
    // thread the page has.
    #[cfg(target_arch = "wasm32")]
    super::ws_transport::start(
        connect_to,
        primary,
        fallback,
        out_tx,
        intent_rx,
        tile_template,
        status.clone(),
    );

    Link {
        mailbox: NetworkMailbox::new(out_rx),
        session: OnlineSession { intent_tx },
        status,
    }
}

/// Every credential was refused — the error `client_loop` ends on when the server
/// answered and said no, so `start_link` can report [`LinkState::Rejected`]
/// rather than a [`LinkState::Failed`] that would be tried again.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug)]
struct Rejected(String);

#[cfg(not(target_arch = "wasm32"))]
impl std::fmt::Display for Rejected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl std::error::Error for Rejected {}

/// A connection error as a person should read it. `anyhow`'s chain is the right
/// thing in a log and the wrong thing in a modal: "Connection refused (os error
/// 111)" is the whole message, and the sign-in screen has no room for the rest.
#[cfg(not(target_arch = "wasm32"))]
fn friendly_error(e: &anyhow::Error) -> String {
    if let Some(io) = e.downcast_ref::<std::io::Error>() {
        return match io.kind() {
            std::io::ErrorKind::ConnectionRefused => "No simulator is listening there".to_string(),
            std::io::ErrorKind::TimedOut => "The server did not answer".to_string(),
            _ => io.to_string(),
        };
    }
    e.to_string()
}

/// One open connection to a simulator, whichever way it was reached.
///
/// An enum rather than a boxed trait object because there are exactly two
/// carriers and the loop below both *opens* and drives them — the same reason
/// the server states its transport as a trait and this states it as a sum. The
/// wire inside is identical, so everything past `connect` is written once.
#[cfg(not(target_arch = "wasm32"))]
enum Wire {
    Tcp(Framed<TcpStream, LengthDelimitedCodec>),
    /// `MaybeTls` because a `ws://` escape hatch reuses this arm unencrypted; the
    /// scheme in the URL is what decides, and tungstenite does the deciding.
    WebSocket(
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    ),
}

/// Select rustls' crypto provider, exactly once, before the first TLS handshake.
///
/// rustls 0.23 ships no default. It derives a process-wide provider from its own
/// features and **panics** when that is ambiguous — none enabled, or both `ring`
/// and `aws-lc-rs`. The panic happens at the first connection, not at startup and
/// not at compile time, so the failure mode is a client that builds, runs, draws
/// a map, and then dies the moment somebody signs in.
///
/// Doing it explicitly rather than relying on the feature being unambiguous: this
/// is a workspace, feature unification is not local, and a dependency added years
/// from now could turn one clean answer into two. `install_default` returns `Err`
/// if a provider is already installed, which is not a problem — somebody got there
/// first and any provider will serve — so the result is deliberately dropped.
#[cfg(not(target_arch = "wasm32"))]
fn install_crypto_provider() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

/// Turn Nagle's algorithm off, on whichever socket a carrier ended up with.
///
/// **Nagle is the wrong trade for this connection in both directions.** It holds a
/// small write back until the previous one is acknowledged, to save a header on
/// bulk transfers made of many tiny writes. Nothing here is that: an intent is one
/// complete message the sim wants now, and a snapshot is one complete message the
/// prediction wants now. On a link with real round-trip time — a phone on mobile
/// data, which is what this client is deployed for — what the delay buys instead
/// is intents leaving in clumps and snapshots arriving in clumps, and
/// `avatar::smooth_online_avatar_display` can only read a clump as a stall
/// followed by a jump.
///
/// Best-effort by design: a socket that will not take the option is still a
/// working socket, and there is nothing useful to do about it but say so.
#[cfg(not(target_arch = "wasm32"))]
fn set_nodelay(stream: &TcpStream) {
    if let Err(e) = stream.set_nodelay(true) {
        tracing::warn!("set_nodelay failed; this connection will be Nagle-delayed: {e:#}");
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Wire {
    async fn connect(target: &Target) -> anyhow::Result<Self> {
        match target {
            Target::Tcp(addr) => {
                let stream = TcpStream::connect(addr).await?;
                set_nodelay(&stream);
                Ok(Self::Tcp(Framed::new(
                    stream,
                    LengthDelimitedCodec::builder()
                        .max_frame_length(MAX_FRAME)
                        .little_endian()
                        .new_codec(),
                )))
            }
            Target::WebSocket(url) => {
                // Before any TLS: see the function's note. A `ws://` URL does not
                // need it, but calling it unconditionally is one branch fewer to
                // get wrong and it costs a `Once` check.
                install_crypto_provider();
                // The same ceiling the TCP codec enforces, so neither carrier is
                // the more permissive one.
                let config = tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default()
                    .max_message_size(Some(MAX_FRAME))
                    .max_frame_size(Some(MAX_FRAME));
                let (socket, _) =
                    tokio_tungstenite::connect_async_with_config(url.as_str(), Some(config), false)
                        .await?;
                match socket.get_ref() {
                    tokio_tungstenite::MaybeTlsStream::Plain(tcp) => set_nodelay(tcp),
                    tokio_tungstenite::MaybeTlsStream::Rustls(tls) => {
                        set_nodelay(tls.get_ref().0);
                    }
                    // `MaybeTlsStream` is `#[non_exhaustive]`: a variant added by a
                    // future tungstenite is a socket we cannot reach, not a reason
                    // to fail the connection.
                    _ => {}
                }
                Ok(Self::WebSocket(socket))
            }
        }
    }

    async fn send(&mut self, frame: Vec<u8>) -> anyhow::Result<()> {
        match self {
            Self::Tcp(framed) => framed.send(Bytes::from(frame)).await?,
            Self::WebSocket(socket) => {
                socket
                    .send(tokio_tungstenite::tungstenite::Message::Binary(
                        frame.into(),
                    ))
                    .await?
            }
        }
        Ok(())
    }

    /// The next app frame, or `None` when the peer hung up.
    ///
    /// Cancel-safe, which is load-bearing: the main loop polls this in a
    /// `select!` arm alongside the intent channel, and a lost frame would be a
    /// lost snapshot. Both inner `next()`s are cancel-safe and this adds no state
    /// around the await.
    async fn next(&mut self) -> Option<anyhow::Result<Vec<u8>>> {
        use tokio_tungstenite::tungstenite::Message;
        match self {
            Self::Tcp(framed) => framed
                .next()
                .await
                .map(|r| r.map(|b| b.to_vec()).map_err(anyhow::Error::from)),
            Self::WebSocket(socket) => loop {
                return match socket.next().await {
                    None => None,
                    Some(Err(e)) => Some(Err(e.into())),
                    Some(Ok(Message::Binary(bytes))) => Some(Ok(bytes.to_vec())),
                    Some(Ok(Message::Close(_))) => None,
                    // Ping/Pong/Text are not ours. A proxy's keepalive must not
                    // be decoded as postcard and reported as a wire error.
                    Some(Ok(_)) => continue,
                };
            },
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
async fn client_loop(
    addr: String,
    primary: AuthCredential,
    fallback: Option<AuthCredential>,
    out_tx: mpsc::Sender<NetMessage>,
    mut intent_rx: UnboundedReceiver<NetMessage>,
    tile_template: Arc<Mutex<String>>,
    status: LinkStatus,
) -> anyhow::Result<()> {
    let target = resolve_target(&addr);
    match &target {
        Target::WebSocket(url) => tracing::info!(%url, "sim link (tls terminated by the server)"),
        Target::Tcp(a) => tracing::info!(addr = %a, "sim link (plain tcp — loopback or explicit)"),
    }

    // Try the primary credential, then the fallback (a stale persisted token
    // gets a fresh password login rather than locking the user out). Each attempt
    // needs its own connection — the server closes on auth failure.
    let attempts: Vec<AuthCredential> = std::iter::once(primary).chain(fallback).collect();
    let mut authed: Option<(Wire, NetMessage)> = None;
    // The last rejection, so "wrong password" reaches the sign-in screen instead
    // of the generic "authentication failed" the retry loop ends on.
    let mut rejection: Option<String> = None;
    for (i, credential) in attempts.into_iter().enumerate() {
        status.set(LinkState::Connecting);
        let mut framed = Wire::connect(&target).await?;
        let hello = encode_app_frame(&NetMessage::ClientHello {
            protocol_version: PROTOCOL_VERSION,
            auth: credential,
        })?;
        framed.send(hello).await?;
        status.set(LinkState::Authenticating);

        let ack_bytes = framed
            .next()
            .await
            .transpose()?
            .ok_or_else(|| anyhow::anyhow!("closed before ServerHelloAck"))?;
        let ack_msg = decode_app_frame(&ack_bytes)?;
        match &ack_msg {
            NetMessage::ServerHelloAck {
                tick_hz,
                your_avatar_id,
                osm_tile_url_template,
                ..
            } => {
                if crate::resources::tile_url_override().is_none() {
                    if let Ok(mut g) = tile_template.lock() {
                        *g = osm_tile_url_template.clone();
                    }
                }
                tracing::info!(tick_hz, your_avatar_id, "handshake ok");
                authed = Some((framed, ack_msg));
                break;
            }
            NetMessage::ServerError { message, .. } => {
                tracing::warn!("auth attempt {i} rejected: {message}");
                rejection = Some(message.clone());
                continue;
            }
            other => anyhow::bail!("unexpected first server message: {other:?}"),
        }
    }
    let (mut framed, ack_msg) = match authed {
        Some(authed) => authed,
        None => {
            return Err(
                Rejected(rejection.unwrap_or_else(|| "authentication failed".to_string())).into(),
            )
        }
    };
    status.set(LinkState::Online);
    if out_tx.send(ack_msg).is_err() {
        return Ok(());
    }

    // Request the texture catalog immediately after handshake.
    let cat_req = encode_app_frame(&NetMessage::TextureCatalogRequest)?;
    framed.send(cat_req).await?;

    loop {
        tokio::select! {
            biased;
            msg = intent_rx.next() => {
                match msg {
                    Some(m) => {
                        let b = encode_app_frame(&m)?;
                        framed.send(b).await?;
                    }
                    None => break,
                }
            }
            frame = framed.next() => {
                match frame {
                    None => break,
                    Some(Err(e)) => return Err(e),
                    Some(Ok(bytes)) => {
                        let m = decode_app_frame(&bytes)?;
                        if out_tx.send(m).is_err() {
                            break;
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

/// Apply network messages (authoritative when online).
pub fn apply_network_snapshot(
    mut commands: Commands,
    mailbox: Option<Res<NetworkMailbox>>,
    mut sync: Option<ResMut<NetworkSyncState>>,
    mut game_state: ResMut<GameState>,
    mut avatar_state: ResMut<AvatarState>,
    mut local_sim_id: ResMut<LocalAvatarSimId>,
    // Excludes avatars so the `&mut Transform` access is provably disjoint from
    // `avatar_tf` below (prims never carry Avatar/RemoteAvatar).
    mut prim_entities: Query<
        (Entity, &mut Prim, &mut Transform),
        (Without<Avatar>, Without<RemoteAvatar>),
    >,
    mut avatar_tf: Query<&mut Transform, (With<Avatar>, Without<RemoteAvatar>)>,
    mut remote_avatars: Query<(Entity, &mut RemoteAvatar), Without<Avatar>>,
    mut texture_lib: ResMut<TextureLibrary>,
    mut texture_cache: ResMut<PrimTextureCache>,
    mut images: ResMut<Assets<Image>>,
    online: Option<Res<OnlineSession>>,
    mut auth: AuthWriteback,
) {
    let Some(mb) = mailbox else {
        return;
    };
    // Drain first, then apply, so stale snapshots can be coalesced. After a
    // frame hitch the backlog holds one 20 Hz snapshot per missed tick, each a
    // full-state replacement — only the newest carries information, and
    // applying the stale ones (entity despawn/respawn on the first, a remote-
    // avatar sync per repeat) lengthens the very hitch that built the backlog,
    // which then rebuilds it. Everything that is not a snapshot still applies
    // in arrival order; dropping a snapshot that a later snapshot supersedes
    // cannot reorder anything a client observes.
    let mut batch: Vec<NetMessage> = Vec::new();
    {
        let rx = mb.lock_rx();
        while let Ok(msg) = rx.try_recv() {
            batch.push(msg);
        }
    }
    mb.received
        .fetch_add(batch.len() as u64, std::sync::atomic::Ordering::Relaxed);
    let last_snapshot = batch
        .iter()
        .rposition(|m| matches!(m, NetMessage::WorldSnapshot { .. }));
    let mut skipped: u32 = 0;
    for (i, msg) in batch.into_iter().enumerate() {
        if matches!(msg, NetMessage::WorldSnapshot { .. }) && Some(i) != last_snapshot {
            skipped += 1;
            continue;
        }
        // Before the match: a private-chat refusal is a `ServerError` whose 403
        // would otherwise land in the prim-edit arm below and deselect a prim.
        if crate::systems::private_chat::PrivateInbox::wants(&msg) {
            auth.private.0.push(msg);
            continue;
        }
        // The same for a call's refusal, and for the rest of its signalling.
        if crate::systems::calls::CallInbox::wants(&msg) {
            auth.calls.0.push(msg);
            continue;
        }
        if crate::systems::contacts::ContactsInbox::wants(&msg) {
            auth.contacts.0.push(msg);
            continue;
        }
        match msg {
            NetMessage::ServerHelloAck {
                your_avatar_id,
                principal_id,
                username,
                session_token,
                api_base_url,
                ..
            } => {
                local_sim_id.0 = Some(your_avatar_id);
                tracing::info!(%username, "authenticated");
                auth.identity.principal_id = Some(principal_id);
                auth.identity.username = Some(username);
                auth.chat.my_id = Some(your_avatar_id);
                // Where this server's model store is, so `sim://` paths resolve
                // (protocol v22). The server's own answer wins; a server that says
                // nothing leaves the client to derive one from the address it was
                // given, which is right whenever the wire and the API share a
                // hostname and impossible when they do not.
                let api_base = if api_base_url.is_empty() {
                    auth.connect_addr
                        .as_ref()
                        .and_then(|a| model_assets::derive_api_base(&a.0))
                } else {
                    Some(api_base_url)
                };
                match api_base {
                    Some(base) if !session_token.is_empty() => {
                        model_assets::set_endpoint(&base, &session_token);
                    }
                    _ => {
                        // Said once, at the point it is decided. Otherwise the
                        // symptom is every model prim drawing nothing, with the
                        // explanation nowhere.
                        tracing::info!(
                            "this server did not say where its model store is;                              prims wearing a model will not be drawn"
                        );
                        model_assets::clear_endpoint();
                    }
                }
                // Persist the token for silent reconnect to external sims (ADR-016).
                if let (Some(cauth), Some(caddr)) = (&auth.connect_auth, &auth.connect_addr) {
                    if cauth.persist_token && !session_token.is_empty() {
                        auth.user_store.set_session_token(&caddr.0, &session_token);
                    }
                }
            }
            NetMessage::WorldSnapshot {
                prims,
                avatars,
                tick,
            } => {
                tracing::debug!(tick, "world snapshot");
                let repeat_tick = sync.as_ref().is_some_and(|s| s.received_initial_world);

                if repeat_tick {
                    apply_local_avatar_pose(&avatars, local_sim_id.0, &mut avatar_state);
                    sync_remote_avatar_entities(
                        &mut commands,
                        &avatars,
                        local_sim_id.0,
                        &mut remote_avatars,
                    );
                    continue;
                }

                // `try_*` throughout this loop, and not as hygiene: the mailbox
                // can still hold a backlog when the link has already failed — a
                // suspended app drains nothing for as long as it is off screen —
                // and `session::teardown` despawns exactly these entities in the
                // same frame, with no ordering between the two systems. Whichever
                // runs first, the other is holding a live view of entities that are
                // already queued for despawn. Gone is the outcome both wanted.
                let prim_es: Vec<Entity> = prim_entities.iter().map(|(e, _, _)| e).collect();
                for e in prim_es {
                    commands.entity(e).try_despawn();
                }
                let remote_es: Vec<Entity> = remote_avatars.iter().map(|(e, _)| e).collect();
                for e in remote_es {
                    commands.entity(e).try_despawn();
                }
                game_state.prims_loaded = false;

                for p in prims {
                    spawn_prim_from_dto(&mut commands, p);
                }
                game_state.prims_loaded = true;

                apply_local_avatar_pose_full(
                    &avatars,
                    local_sim_id.0,
                    &mut avatar_state,
                    &mut avatar_tf,
                );
                sync_remote_avatar_entities(
                    &mut commands,
                    &avatars,
                    local_sim_id.0,
                    &mut remote_avatars,
                );

                if let Some(s) = sync.as_mut() {
                    s.received_initial_world = true;
                }
            }
            NetMessage::PrimRemoved { id } => {
                for (e, p, _) in prim_entities.iter() {
                    if p.id == id {
                        commands.entity(e).try_despawn();
                        break;
                    }
                }
            }
            NetMessage::PrimUpsert { prim } => {
                // Don't disrupt the prim the local user is actively manipulating: the
                // local copy is authoritative until deselect (and was applied locally
                // when the edit was made). Other prims reflect the server immediately.
                if game_state.selected_prim_id == Some(prim.id) {
                    continue;
                }
                let mut found = false;
                for (entity, mut p, mut tf) in prim_entities.iter_mut() {
                    if p.id == prim.id {
                        *p = prim_from_dto(&prim);
                        *tf = transform_from_dto(&prim);
                        // Geometry may have changed → rebuild mesh; texture/surface →
                        // refresh material (refresh_prim_textures re-applies both).
                        commands
                            .entity(entity)
                            .try_insert(NeedsMeshRebuild)
                            .try_insert(NeedsTextureRefresh);
                        // ADR-029: keep the geo anchor in sync so place_geo_prims
                        // re-derives the cell/local transform for the new position.
                        commands.entity(entity).try_insert(PrimGeo {
                            latitude: prim.geo.latitude,
                            longitude: prim.geo.longitude,
                        });
                        found = true;
                        break;
                    }
                }
                if !found {
                    spawn_prim_from_dto(&mut commands, prim);
                }
            }
            NetMessage::TextureCatalog { textures } => {
                texture_lib.entries = textures.clone();
                if let Some(sess) = online.as_ref() {
                    for entry in &textures {
                        if !texture_cache.handles.contains_key(&entry.id)
                            && !texture_cache.pending.contains(&entry.id)
                        {
                            texture_cache.pending.insert(entry.id.clone());
                            sess.send(
                                "texture request",
                                NetMessage::TextureRequest {
                                    request_id: 0,
                                    texture_id: entry.id.clone(),
                                },
                            );
                        }
                    }
                }
                tracing::info!(count = textures.len(), "texture catalog received");
            }
            NetMessage::TextureData {
                texture_id,
                png_bytes,
                ..
            } => {
                texture_cache.pending.remove(&texture_id);
                match decode_png_to_image(&png_bytes) {
                    Some(img) => {
                        let handle = images.add(img);
                        texture_cache.handles.insert(texture_id.clone(), handle);
                        for (entity, prim, _) in prim_entities.iter() {
                            if prim.texture_id.as_deref() == Some(&texture_id) {
                                commands.entity(entity).try_insert(NeedsTextureRefresh);
                            }
                        }
                        tracing::debug!(id = %texture_id, "texture loaded");
                    }
                    None => tracing::warn!(id = %texture_id, "failed to decode texture PNG"),
                }
            }
            NetMessage::TextureNotFound { texture_id, .. } => {
                texture_cache.pending.remove(&texture_id);
                tracing::warn!(id = %texture_id, "texture not found on server");
            }
            NetMessage::ChatMessage {
                channel,
                sender_id,
                sender_name,
                body,
                distance_m,
                ..
            } => {
                // `sender_id` is carried through rather than dropped: it is the
                // same number as `RemoteAvatar::sim_id`, which is what lets a
                // speech bubble find the person who said it without matching on
                // a display name.
                auth.chat
                    .receive(&channel, sender_id, sender_name, body, distance_m);
            }
            // An edit the server would not allow. The dialog and the gizmo both
            // applied it locally already — for the feel of it — so the client is
            // currently drawing something that never happened, and nothing would
            // correct it: `PrimUpsert` is only broadcast when something changes,
            // and a refusal changes nothing.
            //
            // The server sends the authoritative prim straight after this, and the
            // deselect here is what lets it land: upserts for the prim the local
            // user is manipulating are deliberately skipped a few arms down, so
            // without this the correction would be the one message ignored.
            NetMessage::ServerError { code, message, .. } if code == 403 => {
                tracing::warn!(%message, "the server refused an edit");
                game_state.selected_prim_id = None;
            }
            NetMessage::ServerError {
                request_id,
                message,
                ..
            } if request_id == crate::resources::DELETE_ACCOUNT_REQUEST => {
                tracing::warn!(%message, "account deletion refused");
                auth.identity.delete_error = Some(message);
            }
            NetMessage::VoiceData {
                sender_id,
                seq,
                payload,
                distance_m,
                ..
            } => {
                // `channel` is deliberately ignored: the server only ever fans
                // voice out on `voice:local` and room channels, and what decides
                // how loud it is played is `distance_m`, not the channel's name.
                auth.voice.receive(sender_id, seq, &payload, distance_m);
            }
            NetMessage::BlockedList { usernames } => {
                auth.chat.blocked = usernames;
            }
            NetMessage::ReportAck { accepted, .. } => {
                auth.chat.notice = Some(crate::systems::chat::ChatNotice {
                    text: if accepted {
                        "Report sent. The people who run this world will see it."
                    } else {
                        "That report could not be filed."
                    }
                    .to_string(),
                    error: !accepted,
                });
            }
            NetMessage::AccountDeleted => {
                // The socket is closing behind this; the session teardown that
                // follows is the ordinary disconnect path.
                tracing::info!("account deleted; the server has closed the session");
                auth.identity.deleted = true;
                auth.identity.username = None;
                auth.identity.principal_id = None;
            }
            NetMessage::PrimScript {
                prim_id,
                source,
                running,
            } => {
                auth.script.receive_source(prim_id, source, running);
            }
            NetMessage::ScriptOutput {
                prim_id,
                level,
                text,
                ..
            } => {
                // Kept for every prim, not just the open one: output is broadcast,
                // and the panel filters. A script that faults while you are looking
                // at a different one still has its error waiting when you get there.
                auth.script.push(prim_id, level, text);
            }
            NetMessage::WorldCapacity {
                prims_used,
                prims_max,
                scripts_used,
                scripts_max,
                script_memory_bytes,
            } => {
                auth.script.budget = Some(crate::systems::script_editor::WorldBudget {
                    prims_used,
                    prims_max,
                    scripts_used,
                    scripts_max,
                    script_memory_bytes,
                });
            }
            _ => {}
        }
    }
    if skipped > 0 {
        tracing::debug!(skipped, "coalesced stale world snapshots");
    }
}

/// Updates authoritative position and replicated yaw. Does not touch [`AvatarState::online_tank_yaw`]
/// (snapshot reconciliation there would cancel A/D before the server applies the next intent).
/// How far a snapshot's local-avatar position may sit from the teleported one
/// and still count as the sim's echo of the teleport. Generous: the avatar can
/// legitimately walk a few metres between the jump and the confirmation.
const TELEPORT_ECHO_M: f32 = 25.0;
/// How long a sent teleport may hold snapshots off before authority wins again.
/// Long enough for a round trip and a tick on a slow link; short enough that a
/// lost message costs one visible correction, not a permanent divergence.
const TELEPORT_HOLD_S: f32 = 2.0;

/// Whether this snapshot's local-avatar pose should be adopted, given a
/// possibly-outstanding teleport (`AvatarState::teleport_hold`). A snapshot
/// sent before the sim processed the jump still carries the old position, and
/// adopting it would yank the avatar back across the world for a frame or two.
/// The first snapshot that lands near the teleported position is the echo and
/// clears the hold; so does the timeout.
fn adopt_local_pose(avatar_state: &mut AvatarState, snapshot_pos: bevy::math::Vec3) -> bool {
    let Some(at) = avatar_state.teleport_hold else {
        return true;
    };
    let stale = snapshot_pos.distance(avatar_state.position) > TELEPORT_ECHO_M
        && at.elapsed().as_secs_f32() < TELEPORT_HOLD_S;
    if stale {
        return false;
    }
    avatar_state.teleport_hold = None;
    true
}

fn apply_local_avatar_pose(
    avatars: &[AvatarStateDto],
    local_id: Option<u64>,
    avatar_state: &mut AvatarState,
) {
    let Some(a) = local_avatar_dto(avatars, local_id) else {
        return;
    };
    if !adopt_local_pose(avatar_state, a.position) {
        return;
    }
    avatar_state.position = a.position;
    avatar_state.snapshot_age = 0.0;
    avatar_state.sim_facing_yaw = snap_yaw_continuation(avatar_state.sim_facing_yaw, a.yaw);
}

fn apply_local_avatar_pose_full(
    avatars: &[AvatarStateDto],
    local_id: Option<u64>,
    avatar_state: &mut AvatarState,
    avatar_tf: &mut Query<&mut Transform, (With<Avatar>, Without<RemoteAvatar>)>,
) {
    let Some(a) = local_avatar_dto(avatars, local_id) else {
        return;
    };
    if !adopt_local_pose(avatar_state, a.position) {
        return;
    }
    avatar_state.position = a.position;
    avatar_state.display_position = a.position;
    avatar_state.snapshot_age = 0.0;
    avatar_state.sim_facing_yaw = snap_yaw_continuation(avatar_state.sim_facing_yaw, a.yaw);
    if let Ok(mut tf) = avatar_tf.single_mut() {
        tf.translation = a.position;
        // Keep the locally-turned facing (`avatar::handle_avatar_movement` eases it
        // toward the travel direction and sends it as `display_yaw`); adopting the
        // snapshot's yaw here would replay our own turn a tick late.
        tf.rotation = Quat::from_rotation_y(avatar_state.facing_yaw);
    }
}

fn local_avatar_dto<'a>(
    avatars: &'a [AvatarStateDto],
    local_id: Option<u64>,
) -> Option<&'a AvatarStateDto> {
    if let Some(id) = local_id {
        avatars.iter().find(|a| a.id == id)
    } else {
        avatars.first()
    }
}

fn sync_remote_avatar_entities(
    commands: &mut Commands,
    avatars: &[AvatarStateDto],
    local_id: Option<u64>,
    remote_query: &mut Query<(Entity, &mut RemoteAvatar), Without<Avatar>>,
) {
    let expected: HashSet<u64> = avatars
        .iter()
        .filter(|a| Some(a.id) != local_id)
        .map(|a| a.id)
        .collect();

    let stale: Vec<Entity> = remote_query
        .iter()
        .filter(|(_, r)| !expected.contains(&r.sim_id))
        .map(|(e, _)| e)
        .collect();
    for e in stale {
        commands.entity(e).try_despawn();
    }

    // One pass to index, one lookup per snapshot row — the inner rescan per row
    // was O(avatars × remote entities) at snapshot rate.
    let by_id: std::collections::HashMap<u64, Entity> =
        remote_query.iter().map(|(e, r)| (r.sim_id, e)).collect();
    for a in avatars {
        if Some(a.id) == local_id {
            continue;
        }
        let mut found = false;
        if let Some(&entity) = by_id.get(&a.id) {
            if let Ok((_, mut r)) = remote_query.get_mut(entity) {
                r.net_position = a.position;
                r.net_velocity = a.velocity;
                r.net_age = 0.0;
                r.net_yaw = snap_yaw_continuation(r.net_yaw, a.yaw);
                // A level, not an edge: the sim re-sends it in every snapshot for
                // as long as the sit lasts, and the *blend* onto it is this
                // client's own business (`avatar::update_avatar_model_pose`).
                r.seated = a.seated.map(Vec2::from);
                // Fire only on a sequence we haven't seen: the sim re-sends a live
                // emote in every snapshot for a couple of seconds, so diffing on
                // the emote itself would restart the clip 20 times a second.
                if let Some(e) = a.emote {
                    if r.last_emote_sequence != Some(e.sequence) {
                        r.last_emote_sequence = Some(e.sequence);
                        r.pending_emote = Some(e.emote);
                    }
                }
                found = true;
            }
        }
        if !found {
            let y = snap_yaw_continuation(0.0, a.yaw);
            commands.spawn((
                RemoteAvatar {
                    sim_id: a.id,
                    net_position: a.position,
                    net_yaw: y,
                    net_velocity: a.velocity,
                    net_age: 0.0,
                    // An avatar that comes into view mid-emote plays it: that is
                    // the whole reason the sim holds it on the wire.
                    pending_emote: a.emote.map(|e| e.emote),
                    last_emote_sequence: a.emote.map(|e| e.sequence),
                    // An avatar already sitting when it comes into range is drawn
                    // sitting, and slides onto the seat from here like everyone
                    // else — a fraction of a second of one stranger arriving
                    // upright is cheaper than a special case.
                    seated: a.seated.map(Vec2::from),
                    seat_blend: 0.0,
                },
                Transform::from_translation(a.position)
                    .with_rotation(Quat::from_rotation_y(wrap_angle_pi(y)))
                    .with_scale(Vec3::splat(0.02)),
            ));
        }
    }
}

fn prim_from_dto(p: &PrimDto) -> Prim {
    Prim {
        id: p.id,
        name: p.name.clone(),
        shape: PrimShape::from_str(&p.shape),
        color: Color::srgb(p.color[0], p.color[1], p.color[2]),
        texture_id: p.texture_id.clone(),
        model: p.model.clone(),
        path_cut_begin: p.path_cut_begin,
        path_cut_end: p.path_cut_end,
        hollow: p.hollow,
        twist_begin: p.twist_begin,
        twist_end: p.twist_end,
        taper_x: p.taper_x,
        taper_y: p.taper_y,
        top_shear_x: p.top_shear_x,
        top_shear_y: p.top_shear_y,
        slice_begin: p.slice_begin,
        slice_end: p.slice_end,
        surface: p.surface,
        particles: p.particles,
        scripted: p.scripted,
        video: p.video.clone(),
    }
}

fn transform_from_dto(p: &PrimDto) -> Transform {
    Transform::from_translation(p.position)
        .with_rotation(Quat::from_euler(
            EulerRot::XYZ,
            p.rotation.x,
            p.rotation.y,
            p.rotation.z,
        ))
        .with_scale(p.scale)
}

/// Spawn a prim entity from a DTO with its absolute geo anchor (ADR-029);
/// `prim_geo::place_geo_prims` derives the `GridCell` + local `Transform` from
/// the current OSM anchor.
fn spawn_prim_from_dto(commands: &mut Commands, p: PrimDto) {
    let geo = PrimGeo {
        latitude: p.geo.latitude,
        longitude: p.geo.longitude,
    };
    commands.spawn((prim_from_dto(&p), transform_from_dto(&p), geo));
}

fn decode_png_to_image(png_bytes: &[u8]) -> Option<Image> {
    let dynamic = image::load_from_memory(png_bytes).ok()?;
    let rgba = dynamic.to_rgba8();
    let (w, h) = rgba.dimensions();
    // Mipmapped + repeat addressing (repeat_u/v > 1 tiles). MAIN_WORLD keeps the
    // mip-0 CPU bytes available for `create_egui_texture_handles`.
    Some(crate::systems::rendering::mipmapped_image(
        w,
        h,
        rgba.into_raw(),
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::MAIN_WORLD | RenderAssetUsages::RENDER_WORLD,
        crate::systems::rendering::repeat_linear_sampler(),
    ))
}

/// Creates egui thumbnail handles for any texture in the library that doesn't have one yet.
/// Runs every Update frame; no-ops once all entries are covered.
pub fn create_egui_texture_handles(
    mut texture_lib: ResMut<TextureLibrary>,
    texture_cache: Res<PrimTextureCache>,
    images: Res<Assets<Image>>,
    egui_manager: Res<crate::systems::egui_manager::EguiManager>,
) {
    let entries: Vec<String> = texture_lib
        .entries
        .iter()
        .filter(|e| !texture_lib.egui_handles.contains_key(&e.id))
        .map(|e| e.id.clone())
        .collect();

    for id in entries {
        let Some(img_handle) = texture_cache.handles.get(&id) else {
            continue;
        };
        let Some(img) = images.get(img_handle) else {
            continue;
        };
        let Some(data) = &img.data else {
            continue;
        };
        let w = img.width();
        let h = img.height();
        // `data` now holds the full mip chain; the thumbnail only needs mip 0
        // (the first w·h·4 bytes).
        let mip0_len = (w * h * 4) as usize;
        if data.len() < mip0_len {
            continue;
        }
        let pixels: Vec<egui::Color32> = data[..mip0_len]
            .chunks_exact(4)
            .map(|p| egui::Color32::from_rgba_unmultiplied(p[0], p[1], p[2], p[3]))
            .collect();
        let color_image = egui::ColorImage::new([w as usize, h as usize], pixels);
        let handle = egui_manager
            .ctx
            .load_texture(&id, color_image, egui::TextureOptions::LINEAR);
        texture_lib.egui_handles.insert(id, handle);
    }
}

// The sim's intent → velocity scaling used to be mirrored here as four private
// constants, with a comment asking whoever changed one to change the sim too. It
// is `cartopolis_core::motion` now, shared with the sim and the offline path, so a
// mismatch is no longer something to remember — see that module's header.

/// Resend interval for an unchanged intent. The intent is absolute state, so it
/// only has to be *re*sent to cover a dropped/stale reading; changes go out the
/// frame they happen. The window runs `AutoNoVsync`, so the old per-frame send
/// pushed several hundred `ClientIntent` frames a second at a 20 Hz sim — one
/// socket write each, and (the client loop's `select!` is `biased` toward the
/// intent channel) enough to starve the branch that reads snapshots back.
const INTENT_KEEPALIVE: f32 = 1.0 / 30.0;
/// Wish-dir / yaw deltas below these count as "unchanged" for the resend test.
const INTENT_EPS_DIR: f32 = 1e-3;
const INTENT_EPS_YAW: f32 = 0.01;

/// Last intent handed to the sim, for the change test above.
#[derive(Clone, Copy)]
pub struct SentIntent {
    move_x: f32,
    move_z: f32,
    display_yaw: f32,
    flying: bool,
    fly_up: bool,
    fly_down: bool,
    /// The seat, only so the change test can see it start and stop.
    seated: Option<[f32; 2]>,
}

/// Whether an intent carries an *edge* — something that happens once and is lost
/// rather than delayed if the frame is skipped. Edges bypass both the
/// send-on-change test and the keepalive throttle.
fn intent_has_edge(jump: bool, emote: Option<cartopolis_core::Emote>) -> bool {
    jump || emote.is_some()
}

pub fn send_network_intent(
    online: Option<Res<OnlineSession>>,
    keyboard_input: Res<ButtonInput<KeyCode>>,
    camera_state: Res<crate::resources::CameraState>,
    mut avatar_state: ResMut<AvatarState>,
    egui_manager: Res<crate::systems::egui_manager::EguiManager>,
    edit_dialog: Res<crate::resources::EditDialogState>,
    colliders: Res<crate::systems::collision::BuildingColliders>,
    layers: Res<crate::systems::data_layers::DataLayers>,
    time: Res<Time>,
    osd: Res<crate::systems::map_osd::OsdInput>,
    map_anchor: Res<crate::resources::MapAnchor>,
    mut last_sent: Local<Option<SentIntent>>,
    mut since_send: Local<f32>,
) {
    let Some(sess) = online else {
        return;
    };
    if camera_state.mode != crate::resources::CameraMode::Avatar {
        // Not driving the avatar: stop predicting so it doesn't drift.
        avatar_state.predicted_velocity = Vec3::ZERO;
        // Force a send when the avatar camera comes back, whatever the state then.
        *last_sent = None;
        return;
    }

    // When the UI owns the keyboard (text field, edit dialog, Ctrl chord), send a zero
    // intent rather than skipping: the sim persists the last velocity, so we must
    // actively tell it to stop instead of leaving the avatar drifting.
    // WASD is not read here any more: `handle_avatar_movement` owns the horizontal
    // model (direction and ramped speed alike) in every run mode, and this system
    // consumes its output. Two systems reading the same keys and deriving the same
    // velocity independently is exactly how the offline and online paths came to
    // behave differently.
    let blocked = egui_manager.ui_owns_keyboard(edit_dialog.visible, &keyboard_input);
    // R up, Q down — the Fly camera's keys (see `avatar::handle_avatar_movement`,
    // which must stay in step: it drives the same avatar in `--offline`). E moved
    // to "interact" when the world gained things to interact with.
    // …and the on-screen climb pair beside them, so a phone can fly a
    // server-authoritative avatar the same way a keyboard does
    // (`systems::map_osd`). `handle_avatar_movement` reads the identical pair for
    // the offline path; the two must stay in step or flight behaves differently
    // depending on how the client was launched.
    let fly_up = !blocked && (keyboard_input.pressed(KeyCode::KeyR) || osd.climb > 0.0);
    let fly_down = !blocked && (keyboard_input.pressed(KeyCode::KeyQ) || osd.climb < 0.0);

    // The edges. Both are set by other systems (`handle_avatar_movement` for the
    // jump, the interaction/emote UI for the emote) and consumed here, so they
    // reach the sim exactly once. The jump is granted only from the ground —
    // mirroring `SimWorld::step`, which makes the same check authoritatively.
    let jump = !blocked && avatar_state.wants_jump && avatar_state.grounded;
    avatar_state.wants_jump = false;
    let emote = if blocked {
        avatar_state.pending_emote = None;
        None
    } else {
        avatar_state.pending_emote
        // Left in place for `update_avatar_animation` to take — the local avatar
        // plays its own emote immediately rather than waiting for it to come
        // back off the wire.
    };

    // The ramped speed and the held travel direction both come from
    // `avatar::handle_avatar_movement`, which runs first and owns the speed model
    // in every run mode. Recomputing them here is what would let prediction and
    // the offline path drift: there is one ramp, and this is its consumer.
    //
    // Note the direction is `move_dir`, not the live wish: on the frame a key
    // comes up the wish is already zero, and the avatar still has speed to shed
    // along the heading it had.
    let speed = avatar_state.ground_speed;
    let mut v = avatar_state.move_dir * locomotion::wish_for_speed(speed);
    // Building collision (the sim is authoritative but has no building geometry —
    // that streams only on the client): steer the walk velocity so the avatar
    // slides along walls and can't enter a building, and auto-walk it out if it
    // ended up inside one (e.g. teleported in — no interiors yet). A building is in
    // the way only at its own height (`collision::Body`), so a flight is steered
    // by a facade and clears a roof, and a fall past a wall is not steered around
    // a building the avatar is nowhere near the height of.
    //
    // The height is measured on the sim's flat plane and lifted by the two things
    // only the client knows — `terrain_lift` (the measured ground, bridge decks
    // included) and `floor_lift` (the storey) — into the frame the footprints'
    // extents are in. Reading `display_position` here would be the same thing
    // computed the other way round, but `position` is what the sim believes and
    // `avatar::smooth_online_avatar_display` may still be easing the display.
    // (Comparing the *flat* height against an airborne threshold was the old
    // bug: with the Height layer on, relief of ~2 m read as airborne on every
    // frame and steering never ran.)
    // Inside a tunnel the surface footprints are not what is around the avatar, and
    // the tunnel's walls are — the same exclusive pair the offline path resolves
    // (`avatar::handle_avatar_movement`, `systems::tunnels`). Steered rather than
    // snapped for the reason this whole branch exists: the sim owns the position
    // online and would overwrite a snap with the next snapshot.
    let in_bore = crate::systems::tunnels::inside(avatar_state.floor_lift);
    if in_bore || (layers.buildings_enabled && !colliders.is_empty()) {
        let pos = Vec2::new(avatar_state.position.x, avatar_state.position.z);
        // The wish is a saturated throttle; the sim multiplies a flier's by the
        // altitude factor, and how far ahead the probe must look depends on the
        // speed that actually results.
        let lift = if avatar_state.is_flying {
            motion::fly_altitude_factor(avatar_state.position.y)
        } else {
            1.0
        };
        let wish = Vec2::new(v.x, v.z) * lift;
        let body = crate::systems::collision::Body::new(
            avatar_state.position.y + avatar_state.terrain_lift + avatar_state.floor_lift,
            avatar_state.terrain_lift + avatar_state.floor_lift,
        );
        let steered = if in_bore {
            colliders.steer_in_bore(pos, wish)
        } else {
            colliders.steer_velocity(pos, wish, body)
        } / lift;
        v.x = steered.x;
        v.z = steered.y;
    }
    // The facing is turned toward the travel direction by
    // `avatar::handle_avatar_movement` (which runs first); replicate that, so other
    // clients see the avatar turn exactly as its owner does.
    let display_yaw = avatar_state.facing_yaw;

    // Predict the world velocity the sim will derive from this intent, through the
    // sim's own function rather than a copy of its arithmetic
    // (`motion::sim_speed_for_wish`). Client-side prediction integrates this for
    // smooth motion between the 20 Hz snapshots (`smooth_online_avatar_display`).
    let mag = Vec2::new(v.x, v.z).length();
    // The avatar's height on the flat plane the sim simulates: the display, with
    // the two things only the client knows about taken back off (`terrain_lift`
    // is the measured ground, `floor_lift` the storey it is standing on). This is
    // the frame every number below is in.
    //
    // **Predicted, not authoritative, and that is the point.** Both the altitude
    // factor and the gravity arc used to read a height that only advances at the
    // sim's 20 Hz tick — a staircase. Flight rides altitude, so a climb re-scaled
    // the whole velocity twenty times a second in steps: at a kilometre up each
    // snapshot moved the height by ~10 m, which is an 8 % jump in *horizontal*
    // speed as well, and the follow camera leads by that same velocity
    // (`free_camera::FOLLOW_LEAD_MAX`), so the view lurched with it. The
    // authoritative height was chosen to stop a high flight rubber-banding, and
    // it does not: reconciliation compares positions, and a factor derived from a
    // height that is itself reconciled converges on the sim's answer anyway —
    // continuously, instead of in twenty steps a second.
    let flat_y =
        (avatar_state.display_position.y - avatar_state.terrain_lift - avatar_state.floor_lift)
            .max(motion::GROUND_Y);
    // Flight rides altitude, exactly as `SimWorld::step` applies it at
    // integration.
    let lift = if avatar_state.is_flying {
        motion::fly_altitude_factor(flat_y)
    } else {
        1.0
    };
    let horizontal = if mag > 1e-4 {
        Vec2::new(v.x, v.z) / mag * motion::sim_speed_for_wish(mag) * lift
    } else {
        Vec2::ZERO
    };
    // Vertical mirrors `SimWorld::step`: thrust while flying, otherwise the
    // gravity arc. Predicting the arc as well is what keeps a jump smooth — with a
    // flat fall prediction the whole airborne phase was reconciliation dragging
    // the display back a snapshot at a time.
    let vy = if avatar_state.is_flying {
        avatar_state.vertical_velocity = 0.0;
        avatar_state.grounded = false;
        // The wire carries two booleans, so the *ramp* is the one part of the
        // vertical model the client has to reproduce rather than receive. Same
        // function the sim runs in `step` and the offline path integrates, which
        // is what keeps a climb from rubber-banding.
        let target = if fly_up {
            motion::FLY_VERTICAL_SPEED
        } else if fly_down {
            -motion::FLY_VERTICAL_SPEED
        } else {
            0.0
        };
        avatar_state.fly_vertical = motion::step_fly_vertical(
            avatar_state.fly_vertical,
            target,
            time.delta_secs().max(1e-4),
        );
        avatar_state.fly_vertical * lift
    } else {
        // Leaving flight drops the thrust outright, as `SimWorld::apply_intent`
        // does: an avatar that isn't flying is on the gravity arc.
        avatar_state.fly_vertical = 0.0;
        // Mirror the sim's grant rule: only a grounded avatar jumps.
        if jump {
            avatar_state.vertical_velocity = motion::JUMP_SPEED;
        }
        // `flat_y`, so the storey comes off as well as the ground. It did not,
        // and standing on an upper floor therefore predicted a fall from the
        // height of that floor every frame — a correction the reconciler then
        // had to undo, for as long as you stood indoors above the ground one.
        let step = motion::step_fall(
            flat_y,
            avatar_state.vertical_velocity,
            motion::GROUND_Y,
            time.delta_secs().max(1e-4),
        );
        avatar_state.vertical_velocity = step.vertical_velocity;
        avatar_state.grounded = step.grounded;
        avatar_state.landing_impact = step.landing_speed.unwrap_or(0.0);
        step.vertical_velocity
    };
    avatar_state.predicted_velocity = Vec3::new(horizontal.x, vy, horizontal.y);

    // Send on change (so a key edge reaches the sim the frame it happens) or on
    // the keepalive tick — not every frame; see `INTENT_KEEPALIVE`.
    *since_send += time.delta_secs();
    // The seat as the sim will hold it: the *whole* offset, not the fraction of
    // it the slide has covered so far. Each viewer eases onto it itself, so
    // sending the eased value would be sending an animation twenty times a second
    // to say something one field already says.
    let seated = avatar_state
        .seated
        .filter(|s| !s.rising)
        .map(|s| s.offset.to_array());
    let intent = SentIntent {
        move_x: v.x,
        move_z: v.z,
        display_yaw,
        flying: avatar_state.is_flying,
        fly_up,
        fly_down,
        seated,
    };
    let changed = last_sent.map_or(true, |p| {
        (p.move_x - intent.move_x).abs() > INTENT_EPS_DIR
            || (p.move_z - intent.move_z).abs() > INTENT_EPS_DIR
            || wrap_angle_pi(p.display_yaw - intent.display_yaw).abs() > INTENT_EPS_YAW
            || p.flying != intent.flying
            || p.fly_up != intent.fly_up
            || p.fly_down != intent.fly_down
            // Sitting down and standing up are changes, so they reach the sim on
            // the frame they happen rather than on the next keepalive.
            || p.seated.is_some() != intent.seated.is_some()
    });
    // An edge is never throttled: a skipped jump is a jump that did not happen,
    // where a skipped movement frame is one the next intent corrects anyway.
    if !changed && *since_send < INTENT_KEEPALIVE && !intent_has_edge(jump, emote) {
        return;
    }
    *since_send = 0.0;
    *last_sent = Some(intent);

    // Where the avatar actually is on Earth (protocol 20). The sim cannot work
    // this out: its avatar coordinates are metres in *this* client's anchored
    // frame, and every client anchors somewhere else, so an absolute position is
    // the only thing a prim's `vfSensor` can measure against a prim's own
    // `GeoAnchor`. `None` before the map has anchored — a sensor then does not see
    // this avatar, which beats inventing a position for it.
    //
    // Sent with the intent rather than on a clock of its own because it is a
    // *property of the same movement*: an intent that reports where the avatar is
    // going and not where it is would be two facts arriving out of step.
    let geo = map_anchor.geo.map(|anchor| {
        let (lat, lng) = crate::systems::geo_nav::world_to_lat_lng(
            anchor,
            f64::from(avatar_state.position.x),
            f64::from(avatar_state.position.z),
        );
        cartopolis_core::GeoAnchor {
            latitude: lat,
            longitude: lng,
        }
    });
    let _ = sess.intent_tx.unbounded_send(NetMessage::ClientIntent {
        request_id: 0,
        move_x: v.x,
        move_z: v.z,
        display_yaw,
        flying: avatar_state.is_flying,
        fly_up,
        fly_down,
        jump,
        emote,
        seated,
        geo,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The four shapes a person can put in the sign-in screen's Server field.
    ///
    /// These run on the native lane, which is the whole reason [`ws_url`] lives in
    /// this file rather than beside its only caller in the wasm-only
    /// `ws_transport`: no CI lane runs a test compiled for wasm32, so a test there
    /// is a test nobody ever runs.
    #[test]
    fn an_explicit_websocket_url_is_used_as_given() {
        assert_eq!(
            ws_url("wss://sim.example/ws", true, "page.example"),
            "wss://sim.example/ws"
        );
        assert_eq!(
            ws_url("ws://localhost:4748/ws", false, "page.example"),
            "ws://localhost:4748/ws"
        );
        // Whitespace is what a paste out of a chat window looks like.
        assert_eq!(
            ws_url("  wss://sim.example/ws  ", true, "page.example"),
            "wss://sim.example/ws"
        );
    }

    #[test]
    fn an_http_scheme_maps_to_its_websocket_one() {
        assert_eq!(
            ws_url("https://sim.example", false, "page.example"),
            "wss://sim.example/ws"
        );
        assert_eq!(
            ws_url("http://sim.example:4748", true, "page.example"),
            "ws://sim.example:4748/ws"
        );
        // A trailing slash is a host, not a path.
        assert_eq!(
            ws_url("https://sim.example/", true, "page.example"),
            "wss://sim.example/ws"
        );
    }

    /// A bare host inherits the page's scheme. This is the load-bearing one: a
    /// browser refuses a `ws://` socket opened from an `https://` page as mixed
    /// content, and refuses it as an opaque connection failure.
    #[test]
    fn a_bare_host_inherits_the_pages_scheme() {
        assert_eq!(
            ws_url("sim.example:4747", true, "page.example"),
            "wss://sim.example:4747/ws"
        );
        assert_eq!(
            ws_url("sim.example:4747", false, "page.example"),
            "ws://sim.example:4747/ws"
        );
    }

    /// An empty address is this page's own origin — what the deployed client uses,
    /// where Caddy proxies `/ws` on the same hostname that served the module.
    #[test]
    fn an_empty_address_is_the_pages_own_origin() {
        assert_eq!(
            ws_url("", true, "cartopolis.org"),
            "wss://cartopolis.org/ws"
        );
        assert_eq!(
            ws_url("   ", false, "localhost:8080"),
            "ws://localhost:8080/ws"
        );
    }

    /// **The security property, as a test.** Anything that could leave this
    /// machine is encrypted, and it takes a deliberate act to opt out. If this
    /// test ever needs relaxing, that is the moment to think hard.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_remote_address_is_always_encrypted() {
        for addr in [
            "cartopolis.org",
            "sim.example.org",
            "sim.example.org:8443",
            "192.0.2.10:4747",
            "https://sim.example.org",
        ] {
            match resolve_target(addr) {
                Target::WebSocket(url) => assert!(
                    url.starts_with("wss://"),
                    "{addr} resolved to an unencrypted {url}"
                ),
                Target::Tcp(a) => panic!("{addr} resolved to plaintext tcp {a}"),
            }
        }
    }

    /// Loopback stays plain TCP: that is a dev sim on this machine, and TLS to
    /// yourself means a self-signed certificate and a disabled verifier — the
    /// appearance of security over traffic that never touches a network.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn loopback_stays_plain_tcp() {
        for addr in [
            "127.0.0.1:4747",
            "localhost:4747",
            "[::1]:4747",
            "127.0.0.53:9999",
        ] {
            assert_eq!(
                resolve_target(addr),
                Target::Tcp(addr.to_string()),
                "{addr} should be reached directly"
            );
        }
    }

    /// Two escape hatches, both explicit, because a private CA and a plaintext
    /// dev sim are real and neither should require editing this file. Nobody
    /// reaches either by typing a hostname.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn opting_out_of_tls_takes_a_scheme() {
        assert_eq!(
            resolve_target("tcp://sim.lan:4747"),
            Target::Tcp("sim.lan:4747".to_string())
        );
        assert_eq!(
            resolve_target("ws://sim.lan:4748/ws"),
            Target::WebSocket("ws://sim.lan:4748/ws".to_string())
        );
        // And an explicit wss URL is passed through untouched.
        assert_eq!(
            resolve_target("wss://sim.example/ws"),
            Target::WebSocket("wss://sim.example/ws".to_string())
        );
    }

    /// A non-default port is a fact about the deployment; dropping it would
    /// quietly connect somewhere else.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_port_survives_the_upgrade_to_tls() {
        assert_eq!(
            resolve_target("sim.example:8443"),
            Target::WebSocket("wss://sim.example:8443/ws".to_string())
        );
    }

    /// A path the caller wrote is theirs — appending `/ws` to it would build a URL
    /// nobody asked for.
    #[test]
    fn a_supplied_path_is_left_alone() {
        assert_eq!(
            ws_url("https://sim.example/carto/ws", true, "page.example"),
            "wss://sim.example/carto/ws"
        );
    }
}
