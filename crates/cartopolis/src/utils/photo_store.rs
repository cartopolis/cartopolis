//! Where a saved place's photo actually lives.
//!
//! A photo is two files under `photos/` in the app data root — `<stem>.jpg` and
//! `<stem>.thumb.jpg` — and `user.json` stores only the stem. Keeping the bytes
//! out of the store is not a tidiness preference: `UserStore::save` rewrites the
//! whole JSON on every autosave tick and skips the write only when the text is
//! byte-identical to the last one, so a base64 photo inside it would turn each
//! tick into a multi-megabyte serialise-and-compare.
//!
//! **The web build keeps photos in memory for the session and loses them on
//! reload.** That is not a gap opened here — it is what `user.json` itself does
//! on wasm (see `user_store::user_data_path`: there is no filesystem, every write
//! fails, and the app runs on defaults). A photo behaves exactly like the place
//! it is attached to, which is the only answer that isn't a surprise. When the
//! wasm half of `platform::storage` grows an IndexedDB implementation, this
//! module is the one place that has to learn about it.

/// File name of the full-size photo for `stem`.
fn full_name(stem: &str) -> String {
    format!("{stem}.jpg")
}

/// File name of the thumbnail for `stem`.
fn thumb_name(stem: &str) -> String {
    format!("{stem}.thumb.jpg")
}

/// Reject a stem that is not one we would have generated.
///
/// Stems round-trip through `user.json`, which is a plain text file on the user's
/// disk that anything can edit — so a stem is *input*, and one containing `..` or
/// a separator would let a hand-edited store point the reader (and, worse, the
/// `remove` on delete) at a path outside the photo directory. The generated form
/// is `place-<id>-<seq>`, so alphanumerics plus `-` and `_` is the whole alphabet
/// we ever need.
fn is_safe_stem(stem: &str) -> bool {
    !stem.is_empty()
        && stem.len() <= 128
        && stem
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Store both sizes under `stem`, replacing anything already there.
pub fn write(stem: &str, prepared: &crate::utils::photo::Prepared) -> Result<(), String> {
    if !is_safe_stem(stem) {
        return Err(format!("refusing to write a photo named {stem:?}"));
    }
    imp::write(&full_name(stem), &prepared.full)?;
    imp::write(&thumb_name(stem), &prepared.thumb)
}

/// The small square image the Places row draws, or `None` if it is gone.
pub fn read_thumb(stem: &str) -> Option<Vec<u8>> {
    is_safe_stem(stem).then(|| imp::read(&thumb_name(stem)))?
}

/// The full-size photo, for the viewer.
pub fn read_full(stem: &str) -> Option<Vec<u8>> {
    is_safe_stem(stem).then(|| imp::read(&full_name(stem)))?
}

/// Forget a photo. Best-effort in both directions: a file that is already gone is
/// not an error, and a file that cannot be removed leaves an orphan rather than
/// blocking the delete of the place itself.
pub fn remove(stem: &str) {
    if !is_safe_stem(stem) {
        return;
    }
    imp::remove(&full_name(stem));
    imp::remove(&thumb_name(stem));
}

#[cfg(not(target_arch = "wasm32"))]
mod imp {
    use std::path::PathBuf;

    /// `photos/` beside `user.json` — the *data* root, not the cache root: a
    /// photo the user took is theirs, and clearing the app's cache to reclaim
    /// tile space must not take it with them.
    fn dir() -> PathBuf {
        crate::utils::cache::app_data_root().join("photos")
    }

    pub fn write(name: &str, bytes: &[u8]) -> Result<(), String> {
        let dir = dir();
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("could not create {}: {e}", dir.display()))?;
        // Same atomic write the user store uses, for the same reason: Android
        // kills backgrounded processes as a matter of routine, and a torn JPEG is
        // a thumbnail that fails to decode on every launch afterwards.
        crate::utils::cache::write_atomic(&dir.join(name), bytes)
            .map_err(|e| format!("could not write {name}: {e}"))
    }

    pub fn read(name: &str) -> Option<Vec<u8>> {
        std::fs::read(dir().join(name)).ok()
    }

    pub fn remove(name: &str) {
        let _ = std::fs::remove_file(dir().join(name));
    }
}

#[cfg(target_arch = "wasm32")]
mod imp {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};

    /// Session-lifetime photo bytes. See the module note for why the web build
    /// forgets them on reload.
    fn store() -> &'static Mutex<HashMap<String, Vec<u8>>> {
        static STORE: OnceLock<Mutex<HashMap<String, Vec<u8>>>> = OnceLock::new();
        STORE.get_or_init(Default::default)
    }

    pub fn write(name: &str, bytes: &[u8]) -> Result<(), String> {
        match store().lock() {
            Ok(mut map) => {
                map.insert(name.to_string(), bytes.to_vec());
                Ok(())
            }
            Err(_) => Err("the photo store is poisoned".into()),
        }
    }

    pub fn read(name: &str) -> Option<Vec<u8>> {
        store().lock().ok()?.get(name).cloned()
    }

    pub fn remove(name: &str) {
        if let Ok(mut map) = store().lock() {
            map.remove(name);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_stems_are_accepted() {
        assert!(is_safe_stem("place-12-3"));
        assert!(is_safe_stem("place-0-0"));
    }

    #[test]
    fn a_stem_cannot_escape_the_photo_directory() {
        // What a hand-edited (or migrated-from-elsewhere) user.json could contain.
        assert!(!is_safe_stem("../../../etc/passwd"));
        assert!(!is_safe_stem("sub/dir"));
        assert!(!is_safe_stem(".."));
        assert!(!is_safe_stem(""));
        assert!(!is_safe_stem(&"x".repeat(129)));
    }

    #[test]
    fn an_unsafe_stem_reads_as_absent_rather_than_reaching_the_filesystem() {
        assert!(read_thumb("../secret").is_none());
        assert!(read_full("../secret").is_none());
        assert!(write(
            "../secret",
            &crate::utils::photo::Prepared {
                full: vec![1],
                thumb: vec![2],
            },
        )
        .is_err());
    }

    #[test]
    fn the_two_sizes_have_distinct_names() {
        assert_eq!(full_name("place-1-1"), "place-1-1.jpg");
        assert_eq!(thumb_name("place-1-1"), "place-1-1.thumb.jpg");
    }
}
