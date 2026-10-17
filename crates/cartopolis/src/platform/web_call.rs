//! A call in the browser (`docs/notes/calls.md`): the Rust half of
//! `web/webcall.js`, with the same shape as `cartopolis_calls::session` so
//! `systems::calls` drives both through the same few calls.
//!
//! Everything media is the browser's own — `RTCPeerConnection`, the microphone
//! and camera, Opus and VP9 (VP8 where it has no VP9 encoder) — and the frames are sealed and opened by
//! `web/call-worker.js` with the chat's keys, byte for byte as the native
//! client does (`web/sframe.js`, held to `cartopolis_calls::keys` by shared
//! test vectors). This module only carries signalling and keys in and events
//! and pictures out.
//!
//! The JavaScript objects live in a thread-local table keyed by a number, as
//! `platform::audio`'s web capture does: a Bevy resource must be `Send`, and a
//! `JsValue` is not.

use crate::platform::sync::{Arc, Mutex};
use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};

use cartopolis_core::CallTrack;
use wasm_bindgen::prelude::*;

#[wasm_bindgen(module = "/web/webcall.js")]
extern "C" {
    #[wasm_bindgen(js_name = webCallSupported)]
    fn web_call_supported() -> bool;

    type WebCall;

    #[wasm_bindgen(constructor)]
    fn new(on_event: &js_sys::Function) -> WebCall;
    #[wasm_bindgen(method)]
    fn start(this: &WebCall) -> js_sys::Promise;
    #[wasm_bindgen(method)]
    fn joined(this: &WebCall, answer: &str) -> js_sys::Promise;
    #[wasm_bindgen(method)]
    fn offered(this: &WebCall, sdp: &str, tracks: &JsValue) -> js_sys::Promise;
    #[wasm_bindgen(method)]
    fn keys(this: &WebCall, epoch: f64, key: &[u8], index: u32);
    #[wasm_bindgen(method, js_name = setMuted)]
    fn set_muted(this: &WebCall, muted: bool);
    #[wasm_bindgen(method, js_name = setCamera)]
    fn set_camera(this: &WebCall, on: bool) -> js_sys::Promise;
    #[wasm_bindgen(method)]
    fn pictures(this: &WebCall) -> js_sys::Array;
    #[wasm_bindgen(method)]
    fn close(this: &WebCall);
}

/// What the browser's call reports, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// The up connection's offer, for `CallJoin`.
    Offer(String),
    /// The down connection's answer, for `CallAnswer`.
    Answer(String),
    Connected,
    Failed(String),
    /// Something to tell the person; the call carries on.
    Notice(String),
    /// The camera could not be opened, so it is off.
    CameraOff,
}

/// One picture from somebody in the call, as RGBA.
pub struct Picture {
    pub participant: u64,
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

struct Live {
    call: WebCall,
    _on_event: Closure<dyn FnMut(String, String)>,
}

thread_local! {
    static LIVE: RefCell<HashMap<u32, Live>> = RefCell::new(HashMap::new());
    static NEXT: RefCell<u32> = const { RefCell::new(0) };
}

/// A call in the browser. Dropping it hangs up: the microphone and camera
/// close, and so do both connections.
pub struct WebSession {
    id: u32,
    events: Arc<Mutex<VecDeque<Event>>>,
}

impl WebSession {
    /// Whether this browser can take part in an encrypted call: WebRTC, a
    /// media-devices API and one of the two encoded-transform APIs.
    #[must_use]
    pub fn supported() -> bool {
        web_call_supported()
    }

    /// Open the microphone and make the up connection's offer; it arrives as
    /// [`Event::Offer`].
    #[must_use]
    pub fn start() -> Self {
        let events: Arc<Mutex<VecDeque<Event>>> = Arc::default();
        let sink = events.clone();
        let on_event =
            Closure::<dyn FnMut(String, String)>::new(move |kind: String, value: String| {
                let ev = match kind.as_str() {
                    "offer" => Event::Offer(value),
                    "answer" => Event::Answer(value),
                    "connected" => Event::Connected,
                    "failed" => Event::Failed(value),
                    "notice" => Event::Notice(value),
                    "camera-off" => Event::CameraOff,
                    _ => return,
                };
                if let Ok(mut q) = sink.lock() {
                    q.push_back(ev);
                }
            });
        let call = WebCall::new(on_event.as_ref().unchecked_ref());
        let id = NEXT.with(|n| {
            let mut n = n.borrow_mut();
            *n += 1;
            *n
        });
        detach(call.start());
        LIVE.with(|l| {
            l.borrow_mut().insert(
                id,
                Live {
                    call,
                    _on_event: on_event,
                },
            )
        });
        Self { id, events }
    }

    fn with(&self, f: impl FnOnce(&WebCall)) {
        LIVE.with(|l| {
            if let Some(live) = l.borrow().get(&self.id) {
                f(&live.call);
            }
        });
    }

    /// The forwarder's answer to our offer.
    pub fn joined(&self, answer: &str) {
        self.with(|c| detach(c.joined(answer)));
    }

    /// The forwarder's offer for the down connection; the answer arrives as
    /// [`Event::Answer`].
    pub fn offered(&self, sdp: &str, tracks: &[CallTrack]) {
        let list = js_sys::Array::new();
        for t in tracks {
            let o = js_sys::Object::new();
            let _ = js_sys::Reflect::set(&o, &"mid".into(), &t.mid.as_str().into());
            let _ = js_sys::Reflect::set(&o, &"participant".into(), &(t.participant as f64).into());
            let video = t.kind != cartopolis_core::CallTrackKind::Audio;
            let _ = js_sys::Reflect::set(&o, &"video".into(), &video.into());
            list.push(&o);
        }
        self.with(|c| detach(c.offered(sdp, &list)));
    }

    /// The chat's keys for an epoch.
    pub fn epoch(&self, epoch: u64, base_key: &[u8], own_index: u32) {
        // An epoch is a count of the chat's membership changes: an f64 holds
        // it exactly far past anything a chat will reach.
        self.with(|c| c.keys(epoch as f64, base_key, own_index));
    }

    pub fn set_muted(&self, muted: bool) {
        self.with(|c| c.set_muted(muted));
    }

    pub fn set_camera(&self, on: bool) {
        self.with(|c| detach(c.set_camera(on)));
    }

    /// Everything reported since the last look.
    #[must_use]
    pub fn events(&self) -> Vec<Event> {
        self.events
            .lock()
            .map(|mut q| q.drain(..).collect())
            .unwrap_or_default()
    }

    /// New pictures since the last look, one per person at most.
    #[must_use]
    pub fn pictures(&self) -> Vec<Picture> {
        let mut out = Vec::new();
        self.with(|c| {
            for item in c.pictures().iter() {
                let row: js_sys::Array = item.unchecked_into();
                let data: js_sys::Uint8ClampedArray = row.get(3).unchecked_into();
                out.push(Picture {
                    participant: row.get(0).as_f64().unwrap_or(0.0) as u64,
                    width: row.get(1).as_f64().unwrap_or(0.0) as u32,
                    height: row.get(2).as_f64().unwrap_or(0.0) as u32,
                    rgba: data.to_vec(),
                });
            }
        });
        out
    }
}

impl Drop for WebSession {
    fn drop(&mut self) {
        if let Some(live) = LIVE.with(|l| l.borrow_mut().remove(&self.id)) {
            live.call.close();
        }
    }
}

/// Run a promise to completion and ignore its value: every outcome worth
/// knowing is reported through the event callback.
fn detach(promise: js_sys::Promise) {
    wasm_bindgen_futures::spawn_local(async move {
        let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
    });
}
