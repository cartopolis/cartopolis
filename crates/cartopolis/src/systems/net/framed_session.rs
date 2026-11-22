//! **Signed in by the page around it**: the web build's session, when
//! cartopolis.org draws it as its World tab (`docs/notes/web-client-is-the-world.md`).
//!
//! The web build has no sign-in screen of its own (`comms` is off there): the
//! page is where a person signs in, chats and calls, and this client is only the
//! world. So the page hands it the session token it holds, and this opens the
//! world's connection with it — the same token the page's chat worker says
//! hello with, so the World tab is the same account as the page without a
//! second sign-in, and without a second set of chat keys.
//!
//! The protocol is two messages on `window.postMessage`, beside
//! `place_link::FRAMED_PLACE_MESSAGE`:
//!
//! - **`{type: "cartopolis.ready"}`, this client to its parent**, once it
//!   listens: the page answers with the session it has, so a frame that loaded
//!   after the page posted is not left signed out.
//! - **`{type: "cartopolis.session", token}`, the page to this client**, in
//!   answer and whenever the page signs in or out: a token signs the world in,
//!   `null` signs it out.
//!
//! Only a message from this page's own origin is read, and the ready message is
//! posted to that origin only, so another site that frames the viewer can
//! neither sign it in nor learn that it is there. The token is never written to
//! this client's storage (`persist_token: false`): the page owns it, and signs
//! the world out by saying so.

use bevy::prelude::*;
use cartopolis_core::AuthCredential;

use crate::systems::session::{self, SessionRequest, SessionState};
use crate::systems::user_store::UserStore;

/// The page's message carrying its session (see the module note).
pub const FRAMED_SESSION_MESSAGE: &str = "cartopolis.session";
/// This client's message asking the page for it.
pub const FRAMED_READY_MESSAGE: &str = "cartopolis.ready";

/// Follow the page's session: sign in with the token it posts, out when it
/// posts none. A token already in use is not signed in again, so the page may
/// post the same one as often as it likes.
pub fn follow_framed_session(
    mut listening: Local<bool>,
    mut current: Local<Option<String>>,
    mut session: ResMut<SessionState>,
    mut store: ResMut<UserStore>,
) {
    if !*listening {
        *listening = true;
        framed::listen();
        framed::say_ready();
    }
    let Some(token) = framed::take() else {
        return;
    };
    if token == *current {
        return;
    }
    match &token {
        Some(token) => {
            info!("signing the world in with the page's session");
            session.request(SessionRequest::Remote {
                server: framed::host(),
                credential: AuthCredential::Token(token.clone()),
                persist_token: false,
            });
        }
        None => {
            info!("the page signed out; so does the world");
            session::sign_out(&mut session, &mut store);
        }
    }
    *current = token;
}

mod framed {
    use std::sync::Mutex;
    use wasm_bindgen::prelude::Closure;
    use wasm_bindgen::{JsCast, JsValue};

    /// The newest session posted and not yet followed: `Some(None)` is a
    /// sign-out.
    static LATEST: Mutex<Option<Option<String>>> = Mutex::new(None);

    pub fn take() -> Option<Option<String>> {
        LATEST.lock().ok()?.take()
    }

    /// The page's own host: the server this client reaches is the one that
    /// served it (`network::ws_url` makes it `wss://host/ws`).
    pub fn host() -> String {
        web_sys::window()
            .and_then(|w| w.location().host().ok())
            .unwrap_or_default()
    }

    /// Ask the framing page for its session. Nothing happens when this client
    /// is not framed: its parent is then itself, and it does not listen for
    /// its own `ready`.
    pub fn say_ready() {
        let Some(window) = web_sys::window() else {
            return;
        };
        let origin = window.location().origin().unwrap_or_default();
        let Ok(Some(parent)) = window.parent() else {
            return;
        };
        let message = js_sys::Object::new();
        let _ = js_sys::Reflect::set(
            &message,
            &JsValue::from_str("type"),
            &JsValue::from_str(super::FRAMED_READY_MESSAGE),
        );
        let _ = parent.post_message(&message, &origin);
    }

    /// Listen for the page's session on the window for the life of the page.
    /// Leaked on purpose, as `place_link`'s listener is: there is one, and a
    /// `Closure` dropped while registered would be a dangling callback.
    pub fn listen() {
        let Some(window) = web_sys::window() else {
            return;
        };
        let origin = window.location().origin().unwrap_or_default();
        let listener =
            Closure::<dyn FnMut(web_sys::MessageEvent)>::new(move |e: web_sys::MessageEvent| {
                if e.origin() != origin {
                    return;
                }
                let data = e.data();
                let get = |name: &str| js_sys::Reflect::get(&data, &JsValue::from_str(name)).ok();
                if get("type").and_then(|v| v.as_string()).as_deref()
                    != Some(super::FRAMED_SESSION_MESSAGE)
                {
                    return;
                }
                let token = get("token")
                    .and_then(|v| v.as_string())
                    .filter(|t| !t.is_empty());
                if let Ok(mut slot) = LATEST.lock() {
                    *slot = Some(token);
                }
            });
        let target: &web_sys::EventTarget = window.as_ref();
        let _ =
            target.add_event_listener_with_callback("message", listener.as_ref().unchecked_ref());
        listener.forget();
    }
}
