//! Getting a `.glb` off the device and into the client, so it can be uploaded.
//!
//! The same shape as [`crate::platform::photo`] and for the same reason — a
//! caller starts a pick and polls it until it stops saying `Pending` — but a
//! separate module rather than a parameter on that one, because the platforms do
//! not line up. A photo on Android is a camera intent; a model on Android is a
//! document picker, which needs an activity result this dex-light APK cannot
//! receive. So the arms are genuinely different, and the honest answer on Android
//! is [`is_available`] returning false.
//!
//! Where a phone gets its models is the shared library instead: browsing and
//! placing work everywhere, and only *adding* to the library needs a desktop or a
//! browser.

use std::sync::{Arc, Mutex};

/// Where a pick has got to. `poll` hands `Ready` over exactly once.
pub enum Outcome {
    Pending,
    /// The file's bytes, unexamined. Whether it is a model at all is the
    /// simulator's to decide — it is the one that has to parse it anyway, and a
    /// second opinion here would be a second set of limits to keep in step.
    Ready(Vec<u8>),
    /// The user backed out. Not an error.
    #[allow(dead_code)]
    Cancelled,
    /// A sentence to show the user.
    Failed(String),
}

type Slot = Arc<Mutex<Option<Outcome>>>;

/// One in-flight request for a file.
pub struct Pick {
    slot: Slot,
    #[allow(dead_code)]
    inner: imp::Inner,
}

impl Pick {
    pub fn poll(&mut self) -> Outcome {
        match self.slot.lock() {
            Ok(mut slot) => slot.take().unwrap_or(Outcome::Pending),
            Err(_) => Outcome::Failed("the file picker failed".into()),
        }
    }
}

/// Whether this build can pick a file at all, so a button can be hidden rather
/// than offering something inert.
#[must_use]
pub fn is_available() -> bool {
    imp::AVAILABLE
}

/// Ask the platform for a file.
pub fn start() -> Result<Pick, String> {
    let slot: Slot = Arc::new(Mutex::new(None));
    let inner = imp::start(slot.clone())?;
    Ok(Pick { slot, inner })
}

// ---------------------------------------------------------------------------
// Desktop: a file dialog.
// ---------------------------------------------------------------------------

#[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
mod imp {
    use super::{Outcome, Slot};
    use bevy::tasks::IoTaskPool;

    pub const AVAILABLE: bool = true;

    pub struct Inner;

    /// `AsyncFileDialog` for the reason `photo`'s arm gives: the blocking API must
    /// be called from the main thread on macOS, which a Bevy system cannot
    /// promise.
    pub fn start(slot: Slot) -> Result<Inner, String> {
        IoTaskPool::get()
            .spawn(async move {
                let picked = rfd::AsyncFileDialog::new()
                    .set_title("Model to upload")
                    // A convenience, not a guarantee — "All files" is always
                    // reachable — which is another reason the server does the
                    // deciding.
                    .add_filter("Binary glTF", &["glb"])
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
// Web: a file input.
// ---------------------------------------------------------------------------

#[cfg(target_arch = "wasm32")]
mod imp {
    use super::{Outcome, Slot};
    use wasm_bindgen::prelude::*;

    pub const AVAILABLE: bool = true;

    pub struct Inner;

    /// The same two constraints `photo`'s web arm documents: `click()` needs the
    /// transient user activation a real gesture leaves behind, and the change
    /// listener has to outlive this function without being owned by the handle,
    /// because a `Closure` is not `Send` and the caller lives in a Bevy resource.
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
        input.set_accept(".glb,model/gltf-binary");
        let _ = input.style().set_property("display", "none");

        let on_change = {
            let slot = slot.clone();
            let input_for_handler = input.clone();
            Closure::<dyn FnMut()>::new(move || {
                let file = input_for_handler.files().and_then(|files| files.get(0));
                input_for_handler.remove();
                let Some(file) = file else {
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

    /// `File` → bytes through a `FileReader`, matching `photo`'s arm:
    /// `arrayBuffer()` would be shorter but its promise is missing on some
    /// browsers this build still runs on.
    fn read_file(file: web_sys::File, slot: Slot) {
        let reader = match web_sys::FileReader::new() {
            Ok(reader) => reader,
            Err(_) => {
                if let Ok(mut slot) = slot.lock() {
                    *slot = Some(Outcome::Failed("could not read the file".into()));
                }
                return;
            }
        };
        let on_load = {
            let slot = slot.clone();
            let reader_for_handler = reader.clone();
            Closure::<dyn FnMut()>::new(move || {
                let outcome = match reader_for_handler.result() {
                    Ok(value) => Outcome::Ready(js_sys::Uint8Array::new(&value).to_vec()),
                    Err(_) => Outcome::Failed("the file could not be read".into()),
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
                    *slot = Some(Outcome::Failed("the file could not be read".into()));
                }
            })
        };
        reader.set_onload(Some(on_load.as_ref().unchecked_ref()));
        reader.set_onerror(Some(on_error.as_ref().unchecked_ref()));
        on_load.forget();
        on_error.forget();
        let _ = reader.read_as_array_buffer(&file);
    }
}

// ---------------------------------------------------------------------------
// Android: no arm.
// ---------------------------------------------------------------------------

/// A document picker returns its answer through `onActivityResult`, which
/// `android-activity` 0.6 does not surface and this APK has no Java class to
/// receive (`docs/notes/android-build.md`). The camera trick `photo` uses does not
/// generalise — it works by watching the MediaStore row the camera writes, and
/// nothing writes one for a file the user merely chose.
///
/// So the button is hidden on a phone rather than doing nothing when pressed.
#[cfg(target_os = "android")]
mod imp {
    use super::Slot;

    pub const AVAILABLE: bool = false;

    pub struct Inner;

    pub fn start(_slot: Slot) -> Result<Inner, String> {
        Err("this build cannot choose a file".into())
    }
}
