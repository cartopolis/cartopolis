//! The browser's transport to a simulator.
//!
//! A page cannot open a TCP socket, so the web build had no way to reach a sim at
//! all and `start_link`'s wasm arm did nothing but report that. This is the other
//! half of `cartopolis_simulator::http`: the same wire (ADR-008/009) carried as
//! one **binary** WebSocket message per app frame instead of a length-delimited
//! stream. The frame body is byte-identical, so `PROTOCOL_VERSION` does not move
//! and a browser client is the same client to the server as a desktop one.
//!
//! # Shape
//!
//! The native path spawns a thread with a tokio runtime and writes the handshake
//! as straight-line async code. Neither of those exists here: there is one thread,
//! and `WebSocket` is a callback API. So the callbacks do nothing but push into a
//! channel, and `run` reads that channel — which buys back the straight-line
//! shape, and with it the ability to keep the two handshakes visibly the same
//! sequence rather than a state machine that has to be compared against one.
//!
//! # The closures are held, not forgotten
//!
//! `Closure::forget` is the usual way to keep a JS callback alive and it leaks the
//! closure for the life of the page. This client can sign in and out repeatedly —
//! that is the whole point of `systems::session` — so a leak per connection is a
//! leak per sign-in. They are owned by the future instead, which drops them when
//! the socket does.

use cartopolis_core::{
    decode_app_frame, encode_app_frame, AuthCredential, NetMessage, PROTOCOL_VERSION,
};
use futures_channel::mpsc::UnboundedReceiver;
use futures_util::{FutureExt, StreamExt};
use js_sys::Uint8Array;
use std::sync::{mpsc, Arc, Mutex};
use wasm_bindgen::closure::Closure;
use wasm_bindgen::JsCast;
use web_sys::{BinaryType, MessageEvent, WebSocket};

use super::network::{LinkState, LinkStatus};

/// What a socket's callbacks report, in the order the browser produced it.
enum Event {
    Open,
    Frame(Vec<u8>),
    /// The socket failed or closed. Carries a message fit for the sign-in card.
    Gone(String),
}

/// A live socket plus the callbacks that feed it. Dropping this closes the socket
/// and releases the closures — order matters only in that both must happen.
struct Socket {
    ws: WebSocket,
    rx: UnboundedReceiver<Event>,
    /// Held for their side effects. Named with a leading underscore because
    /// nothing reads them: they exist so the browser can call them.
    _on_open: Closure<dyn FnMut()>,
    _on_message: Closure<dyn FnMut(MessageEvent)>,
    _on_error: Closure<dyn FnMut(web_sys::Event)>,
    _on_close: Closure<dyn FnMut(web_sys::CloseEvent)>,
}

impl Drop for Socket {
    fn drop(&mut self) {
        // Clear the handlers before closing: a close fired into a callback whose
        // channel has already been dropped is harmless but noisy, and on a
        // reconnect the noise arrives interleaved with the new socket's events.
        self.ws.set_onopen(None);
        self.ws.set_onmessage(None);
        self.ws.set_onerror(None);
        self.ws.set_onclose(None);
        let _ = self.ws.close();
    }
}

impl Socket {
    fn open(url: &str) -> Result<Self, String> {
        let ws = WebSocket::new(url).map_err(|e| describe(&e, "could not open a connection"))?;
        // Without this, a binary message arrives as a `Blob` and reading one is
        // asynchronous — every frame would need a round trip through a promise.
        ws.set_binary_type(BinaryType::Arraybuffer);

        let (tx, rx) = futures_channel::mpsc::unbounded();

        let open_tx = tx.clone();
        let on_open = Closure::<dyn FnMut()>::new(move || {
            let _ = open_tx.unbounded_send(Event::Open);
        });
        ws.set_onopen(Some(on_open.as_ref().unchecked_ref()));

        let msg_tx = tx.clone();
        let on_message = Closure::<dyn FnMut(MessageEvent)>::new(move |e: MessageEvent| {
            let data = e.data();
            // Anything that is not an ArrayBuffer is not ours. A text frame from a
            // misconfigured proxy is dropped rather than decoded as postcard,
            // which would fail with a message about the wire rather than about the
            // proxy.
            if let Some(buffer) = data.dyn_ref::<js_sys::ArrayBuffer>() {
                let _ = msg_tx.unbounded_send(Event::Frame(Uint8Array::new(buffer).to_vec()));
            }
        });
        ws.set_onmessage(Some(on_message.as_ref().unchecked_ref()));

        let err_tx = tx.clone();
        let on_error = Closure::<dyn FnMut(web_sys::Event)>::new(move |_| {
            // A browser deliberately tells a page nothing about *why* a WebSocket
            // failed (it would be a cross-origin probe), so there is no detail to
            // extract here and inventing one would be a lie. `onclose` follows and
            // its code is the only real information available.
            let _ = err_tx.unbounded_send(Event::Gone("could not reach that server".into()));
        });
        ws.set_onerror(Some(on_error.as_ref().unchecked_ref()));

        let close_tx = tx;
        let on_close =
            Closure::<dyn FnMut(web_sys::CloseEvent)>::new(move |e: web_sys::CloseEvent| {
                let _ = close_tx.unbounded_send(Event::Gone(close_reason(&e)));
            });
        ws.set_onclose(Some(on_close.as_ref().unchecked_ref()));

        Ok(Self {
            ws,
            rx,
            _on_open: on_open,
            _on_message: on_message,
            _on_error: on_error,
            _on_close: on_close,
        })
    }

    /// Copied out of wasm memory first. The threaded build's memory is a
    /// `SharedArrayBuffer`, and `WebSocket.send` refuses a view onto shared
    /// memory with a `TypeError` — which `send_with_u8_array` is, since it
    /// passes the slice in place. That failed the very first frame, the
    /// hello, so no browser could sign in.
    fn send(&self, frame: &[u8]) -> Result<(), String> {
        let copy = js_sys::Uint8Array::new_with_length(frame.len() as u32);
        copy.copy_from(frame);
        self.ws
            .send_with_array_buffer(&copy.buffer())
            .map_err(|e| describe(&e, "the connection dropped mid-send"))
    }

    async fn next(&mut self) -> Event {
        self.rx
            .next()
            .await
            // The sender lives in the closures this struct owns, so the channel
            // cannot close while the struct is alive.
            .unwrap_or_else(|| Event::Gone("the connection ended".into()))
    }
}

/// A `JsValue` error as a sentence, falling back to `context` when the browser
/// gave us nothing (which is most of the time).
fn describe(e: &wasm_bindgen::JsValue, context: &str) -> String {
    e.as_string().unwrap_or_else(|| context.to_string())
}

/// A close event as a person should read it.
///
/// 1006 is "abnormal closure" and is what a browser reports for *every* failure it
/// will not describe — refused, DNS, TLS, a proxy that does not speak WebSocket.
/// Reporting it as a number would be worse than reporting nothing.
fn close_reason(e: &web_sys::CloseEvent) -> String {
    let reason = e.reason();
    if !reason.is_empty() {
        return reason;
    }
    match e.code() {
        1000 | 1001 => "the server closed the connection".into(),
        1006 => "could not reach that server".into(),
        code => format!("the connection closed ({code})"),
    }
}

/// The page's own scheme and host, for [`super::network::ws_url`]'s two
/// browser-dependent answers.
fn page() -> (bool, String) {
    let window = web_sys::window();
    let secure = window
        .as_ref()
        .and_then(|w| w.location().protocol().ok())
        .map(|p| p == "https:")
        .unwrap_or(false);
    let host = window
        .and_then(|w| w.location().host().ok())
        .unwrap_or_default();
    (secure, host)
}

/// Open a session and pump it until either end hangs up. Mirrors
/// `network::client_loop`, which is the native half of the same handshake.
#[allow(clippy::too_many_arguments)]
pub fn start(
    addr: String,
    primary: AuthCredential,
    fallback: Option<AuthCredential>,
    out_tx: mpsc::Sender<NetMessage>,
    intent_rx: UnboundedReceiver<NetMessage>,
    tile_template: Arc<Mutex<String>>,
    status: LinkStatus,
) {
    wasm_bindgen_futures::spawn_local(async move {
        let (secure, host) = page();
        let url = super::network::ws_url(&addr, secure, &host);
        tracing::info!(%url, "websocket link");
        match run(
            url,
            primary,
            fallback,
            &out_tx,
            intent_rx,
            &tile_template,
            &status,
        )
        .await
        {
            Ok(()) => status.set(LinkState::Closed),
            Err(Ended::Failed(message)) => {
                tracing::warn!(%message, "websocket link ended");
                status.set(LinkState::Failed(message));
            }
            Err(Ended::Rejected(message)) => {
                tracing::warn!(%message, "websocket sign-in refused");
                status.set(LinkState::Rejected(message));
            }
        }
    });
}

/// How [`run`] can end badly — the web half of the native path's `Rejected`
/// error, kept apart for the same reason: a lost socket is retried by
/// `session`, a refused credential is not. `From<String>` so every `?` on a
/// transport error keeps reading as a failure.
enum Ended {
    Failed(String),
    Rejected(String),
}

impl From<String> for Ended {
    fn from(message: String) -> Self {
        Self::Failed(message)
    }
}

#[allow(clippy::too_many_arguments)]
async fn run(
    url: String,
    primary: AuthCredential,
    fallback: Option<AuthCredential>,
    out_tx: &mpsc::Sender<NetMessage>,
    mut intent_rx: UnboundedReceiver<NetMessage>,
    tile_template: &Arc<Mutex<String>>,
    status: &LinkStatus,
) -> Result<(), Ended> {
    // Primary then fallback, exactly as the native path: a stale persisted token
    // gets a fresh password login rather than locking the user out. Each attempt
    // needs its own socket — the server closes on a rejected credential.
    let attempts: Vec<AuthCredential> = std::iter::once(primary).chain(fallback).collect();
    let mut socket = None;
    let mut rejection: Option<String> = None;

    for credential in attempts {
        status.set(LinkState::Connecting);
        let mut candidate = Socket::open(&url)?;
        match candidate.next().await {
            Event::Open => {}
            Event::Gone(why) => return Err(why.into()),
            Event::Frame(_) => {
                return Err(String::from("the server spoke before the socket opened").into())
            }
        }

        let hello = encode_app_frame(&NetMessage::ClientHello {
            protocol_version: PROTOCOL_VERSION,
            auth: credential,
        })
        .map_err(|e| e.to_string())?;
        candidate.send(&hello)?;
        status.set(LinkState::Authenticating);

        let ack = loop {
            match candidate.next().await {
                Event::Frame(bytes) => {
                    break decode_app_frame(&bytes).map_err(|e| e.to_string())?
                }
                Event::Gone(why) => return Err(why.into()),
                Event::Open => continue,
            }
        };
        match &ack {
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
                socket = Some((candidate, ack));
                break;
            }
            NetMessage::ServerError { message, .. } => {
                tracing::warn!(%message, "auth attempt rejected");
                rejection = Some(message.clone());
                // `candidate` drops here, which closes the socket. The next
                // attempt opens its own.
                continue;
            }
            other => return Err(format!("unexpected first server message: {other:?}").into()),
        }
    }

    let (mut socket, ack) = socket.ok_or_else(|| {
        Ended::Rejected(rejection.unwrap_or_else(|| "authentication failed".to_string()))
    })?;
    status.set(LinkState::Online);
    if out_tx.send(ack).is_err() {
        return Ok(());
    }

    let catalog =
        encode_app_frame(&NetMessage::TextureCatalogRequest).map_err(|e| e.to_string())?;
    socket.send(&catalog)?;

    // `select` over the two directions, same as the native loop. `intent_rx`
    // ending is a sign-out: `session::teardown` drops `OnlineSession`, which drops
    // the sender, which lands here as `None` and closes the socket on the way out.
    loop {
        futures_util::select! {
            intent = intent_rx.next() => match intent {
                Some(msg) => {
                    let bytes = encode_app_frame(&msg).map_err(|e| e.to_string())?;
                    socket.send(&bytes)?;
                }
                None => return Ok(()),
            },
            event = socket.next().fuse() => match event {
                Event::Frame(bytes) => {
                    let msg = decode_app_frame(&bytes).map_err(|e| e.to_string())?;
                    if out_tx.send(msg).is_err() {
                        return Ok(());
                    }
                }
                // A close after we were online is not an error to put on a card:
                // `session` reports "Disconnected from …" for it already.
                Event::Gone(_) => return Ok(()),
                Event::Open => continue,
            },
        }
    }
}
