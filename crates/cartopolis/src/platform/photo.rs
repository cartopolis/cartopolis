//! Getting a photo out of the device and into the client (ADR-030 seam).
//!
//! One shape for three very different mechanisms: a desktop file dialog, a
//! browser file input that a phone answers with its camera, and an Android
//! camera intent. The caller starts a [`Capture`] and polls it each frame until
//! it stops saying [`Outcome::Pending`]; `systems::bookmarks` is the only caller
//! and it programs against this, never against a platform.
//!
//! **Why polling rather than a callback or an `async fn`.** The Android arm has
//! no completion event to hand back. `android-activity` 0.6 exposes no
//! `onActivityResult`, and this APK is `hasCode="false"` — there is no `classes.dex`
//! in it and therefore nowhere to put the Java shim that would deliver one
//! (`docs/notes/android-build.md`). The camera app is launched fire-and-forget
//! and the only way to learn that it finished is to look at the `MediaStore` row
//! we handed it. Something has to drive that look, and a Bevy system already runs
//! every frame; the alternative — a thread sleeping in a loop — would hold an
//! `IoTaskPool` thread for as long as the user takes composing the shot.
//!
//! The desktop and web arms have real completion events, so their `poll` is just
//! a read of the slot the event filled.

use std::sync::{Arc, Mutex};

/// Where a capture has got to. `poll` hands `Ready` over exactly once.
pub enum Outcome {
    /// Still waiting on the user (or, on Android, on the camera app).
    Pending,
    /// Encoded image bytes, in whatever format the source produced.
    Ready(Vec<u8>),
    /// The user backed out. Not an error and nothing to report in the panel.
    ///
    /// `allow`: constructed by the desktop and web arms, which get a real
    /// dismissal event from the dialog. **Android cannot produce it** — a Back
    /// press out of the camera app is not reported to us at all (see the module
    /// note), so there a cancel is only ever the user pressing Cancel in the
    /// panel, which drops the whole request rather than resolving it. On that
    /// target the variant is genuinely unconstructed, and that asymmetry is a
    /// fact about the platform rather than a loose end.
    #[allow(dead_code)]
    Cancelled,
    /// A sentence to show the user.
    Failed(String),
}

/// The slot every arm fills. `None` while pending.
type Slot = Arc<Mutex<Option<Outcome>>>;

/// One in-flight request for an image.
pub struct Capture {
    slot: Slot,
    /// The platform's own handle, where it needs one to poll or to cancel.
    inner: imp::Inner,
}

impl Capture {
    /// Advance the capture and take its result if one has arrived.
    ///
    /// Cheap on every arm but Android, where it is throttled to
    /// [`android::CAMERA_POLL_INTERVAL`](crate::platform::android) internally.
    pub fn poll(&mut self) -> Outcome {
        self.inner.poll(&self.slot);
        match self.slot.lock() {
            Ok(mut slot) => slot.take().unwrap_or(Outcome::Pending),
            // A panic inside a capture closure would poison the lock; reporting it
            // as a failure ends the request instead of leaving a spinner forever.
            Err(_) => Outcome::Failed("the photo capture failed".into()),
        }
    }

    /// Give up on the capture and release whatever the platform is holding for it.
    pub fn cancel(&mut self) {
        self.inner.cancel();
    }
}

/// Whether this build can produce a photo at all. False on a target with no arm,
/// so the menu entry can be hidden rather than offering something inert.
pub fn is_available() -> bool {
    imp::AVAILABLE
}

/// What the menu entry should say — the phone opens a camera, the desktop opens a
/// file dialog, and calling both "Add photo" would be vague on both.
pub fn action_label() -> &'static str {
    imp::ACTION_LABEL
}

/// Ask the platform for an image.
pub fn start() -> Result<Capture, String> {
    let slot: Slot = Arc::new(Mutex::new(None));
    let inner = imp::start(slot.clone())?;
    Ok(Capture { slot, inner })
}

// ---------------------------------------------------------------------------
// Desktop: a file dialog.
// ---------------------------------------------------------------------------

#[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
mod imp {
    use super::{Outcome, Slot};
    use bevy::tasks::IoTaskPool;

    pub const AVAILABLE: bool = true;
    pub const ACTION_LABEL: &str = "Choose a photo…";

    /// Nothing to hold: the dialog owns itself and answers through the slot.
    pub struct Inner;

    impl Inner {
        pub fn poll(&mut self, _slot: &Slot) {}
        /// The dialog cannot be dismissed from here — closing it is the user's to
        /// do. Dropping the request just means the answer is discarded.
        pub fn cancel(&mut self) {}
    }

    pub fn start(slot: Slot) -> Result<Inner, String> {
        // `AsyncFileDialog`, not the blocking one, and on the IO pool rather than
        // a thread of its own: the blocking API must be called from the main
        // thread on macOS, which is a rule this call site cannot honour from
        // inside a Bevy system, while the async one dispatches correctly on every
        // backend. Under the `xdg-portal` backend (the one built here — see the
        // dependency note in Cargo.toml) it is a DBus round trip and holds no
        // pool thread while the dialog is open.
        IoTaskPool::get()
            .spawn(async move {
                let picked = rfd::AsyncFileDialog::new()
                    .set_title("Photo for this place")
                    // The filter is a convenience, not a guarantee: "All files"
                    // is always reachable, so `utils::photo::prepare` still has
                    // to survive being handed something that is not an image.
                    .add_filter("Images", &["jpg", "jpeg", "png", "webp", "bmp", "gif"])
                    .pick_file()
                    .await;
                let outcome = match picked {
                    Some(file) => Outcome::Ready(file.read().await),
                    None => Outcome::Cancelled,
                };
                if let Ok(mut slot) = slot.lock() {
                    *slot = Some(outcome);
                }
            })
            .detach();
        Ok(Inner)
    }
}

// ---------------------------------------------------------------------------
// Web: a file input, which a phone answers with its camera.
// ---------------------------------------------------------------------------

#[cfg(target_arch = "wasm32")]
mod imp {
    use super::{Outcome, Slot};
    // `JsCast` (for `dyn_into`/`unchecked_ref`) rides in on the prelude.
    use wasm_bindgen::prelude::*;

    pub const AVAILABLE: bool = true;
    pub const ACTION_LABEL: &str = "Take or choose a photo…";

    pub struct Inner;

    impl Inner {
        pub fn poll(&mut self, _slot: &Slot) {}
        pub fn cancel(&mut self) {}
    }

    /// Build a hidden `<input type="file">`, click it, and read the first file the
    /// user picks.
    ///
    /// `capture="environment"` is the whole mobile story: a phone browser reads it
    /// as "open the rear camera" and offers the gallery as the alternative, while
    /// a desktop browser ignores it and shows an ordinary file picker. That is the
    /// same element serving both, which is why the web arm needs no branch on
    /// screen size.
    ///
    /// Two constraints this cannot violate. The `click()` needs *transient user
    /// activation*, which a browser grants for about five seconds after a real
    /// gesture — reached here because the menu item was tapped a frame or two ago,
    /// but not reachable from a timer or a network callback. And the change
    /// listener has to outlive this function without being owned by the returned
    /// handle, because a `Closure` is not `Send` and the caller lives in a Bevy
    /// resource; `forget()` leaks a few hundred bytes per capture, which is the
    /// cheapest correct answer for something the user does by hand.
    pub fn start(slot: Slot) -> Result<Inner, String> {
        let document = web_sys::window()
            .and_then(|w| w.document())
            .ok_or_else(|| "no document to attach the file input to".to_string())?;
        let input: web_sys::HtmlInputElement = document
            .create_element("input")
            .map_err(|_| "could not create the file input".to_string())?
            .dyn_into()
            .map_err(|_| "the file input was not an input element".to_string())?;
        input.set_type("file");
        input.set_accept("image/*");
        let _ = input.set_attribute("capture", "environment");
        let _ = input.style().set_property("display", "none");

        let on_change = {
            let slot = slot.clone();
            let input_for_handler = input.clone();
            Closure::<dyn FnMut()>::new(move || {
                let file = input_for_handler.files().and_then(|files| files.get(0));
                // The element has done its job either way; leaving it in the DOM
                // would accumulate one per photo taken.
                input_for_handler.remove();
                let Some(file) = file else {
                    // A change event with no file is the browser telling us the
                    // picker was dismissed.
                    if let Ok(mut slot) = slot.lock() {
                        *slot = Some(Outcome::Cancelled);
                    }
                    return;
                };
                read_file(file, slot.clone());
            })
        };
        input.set_onchange(Some(on_change.as_ref().unchecked_ref()));
        on_change.forget();

        let body = document
            .body()
            .ok_or_else(|| "the page has no body".to_string())?;
        body.append_child(&input)
            .map_err(|_| "could not attach the file input".to_string())?;
        input.click();
        Ok(Inner)
    }

    /// `File` → bytes, through a `FileReader`. `arrayBuffer()` would be shorter but
    /// its promise is not available on every browser this build still runs on.
    fn read_file(file: web_sys::File, slot: Slot) {
        let reader = match web_sys::FileReader::new() {
            Ok(reader) => reader,
            Err(_) => {
                if let Ok(mut slot) = slot.lock() {
                    *slot = Some(Outcome::Failed("could not read the photo".into()));
                }
                return;
            }
        };
        let on_load = {
            let slot = slot.clone();
            let reader_for_handler = reader.clone();
            Closure::<dyn FnMut()>::new(move || {
                let outcome = match reader_for_handler.result() {
                    Ok(value) => {
                        let bytes = js_sys::Uint8Array::new(&value).to_vec();
                        Outcome::Ready(bytes)
                    }
                    Err(_) => Outcome::Failed("the photo could not be read".into()),
                };
                if let Ok(mut slot) = slot.lock() {
                    *slot = Some(outcome);
                }
            })
        };
        let on_error = {
            let slot = slot.clone();
            Closure::<dyn FnMut()>::new(move || {
                if let Ok(mut slot) = slot.lock() {
                    *slot = Some(Outcome::Failed("the photo could not be read".into()));
                }
            })
        };
        reader.set_onload(Some(on_load.as_ref().unchecked_ref()));
        reader.set_onerror(Some(on_error.as_ref().unchecked_ref()));
        on_load.forget();
        on_error.forget();
        if reader.read_as_array_buffer(&file).is_err() {
            if let Ok(mut slot) = slot.lock() {
                *slot = Some(Outcome::Failed("the photo could not be read".into()));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Android: a camera intent, watched through the MediaStore row it writes into.
// ---------------------------------------------------------------------------

#[cfg(target_os = "android")]
mod imp {
    use super::{Outcome, Slot};

    pub const AVAILABLE: bool = true;
    pub const ACTION_LABEL: &str = "Take a photo…";

    pub struct Inner {
        capture: crate::platform::android::CameraCapture,
    }

    impl Inner {
        pub fn poll(&mut self, slot: &Slot) {
            let Some(outcome) = self.capture.poll() else {
                return;
            };
            if let Ok(mut slot) = slot.lock() {
                *slot = Some(match outcome {
                    Ok(bytes) => Outcome::Ready(bytes),
                    Err(message) => Outcome::Failed(message),
                });
            }
        }

        pub fn cancel(&mut self) {
            self.capture.discard();
        }
    }

    pub fn start(_slot: Slot) -> Result<Inner, String> {
        crate::platform::android::start_camera_capture().map(|capture| Inner { capture })
    }
}
