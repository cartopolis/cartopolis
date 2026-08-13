//! Where a saved place's photo actually lives.
//!
//! A photo is two entries in the Data namespace of [`crate::platform::storage`] —
//! `photos/<stem>.jpg` and `photos/<stem>.thumb.jpg` — and `user.json` stores only
//! the stem. That is one path on every target: a file under the app data root
//! natively, an IndexedDB record on the web.
//!
//! Keeping the bytes out of the store is not a tidiness preference:
//! `UserStore::save` re-serialises the whole document on every autosave tick and
//! skips the write only when the text is byte-identical to the last one, so a
//! base64 photo inside it would turn each tick into a multi-megabyte
//! serialise-and-compare.
//!
//! **Thumbnails are read synchronously and full photos are not.** A row draws its
//! thumbnail inside an egui pass, which cannot await, so on the web every
//! thumbnail is pulled into the synchronous view before the app is built
//! (`storage::preload(PHOTO_PREFIX)` in `run`) — a few kilobytes each. A
//! full-size photo is megabytes and only wanted when someone opens the viewer, so
//! it is fetched on demand and [`read_full`] answers [`Loading`] until it lands.
//! Natively that state never occurs and the call sites read the same.
//!
//! [`Loading`]: crate::platform::storage::Cached::Loading

use crate::platform::storage::{self, Cached, Namespace};

/// Key prefix every photo lives under.
pub const PHOTO_PREFIX: &str = "photos/";

/// Key suffix of the small square image. Together with [`PHOTO_PREFIX`] this is
/// what the web build preloads into the synchronous view at startup — the
/// thumbnails only, never the full-size photos beside them.
pub const THUMB_SUFFIX: &str = ".thumb.jpg";

/// Storage key of the full-size photo for `stem`.
fn full_key(stem: &str) -> String {
    format!("{PHOTO_PREFIX}{stem}.jpg")
}

/// Storage key of the thumbnail for `stem`.
fn thumb_key(stem: &str) -> String {
    format!("{PHOTO_PREFIX}{stem}{THUMB_SUFFIX}")
}

/// Reject a stem that is not one we would have generated.
///
/// Stems round-trip through `user.json`, which is a plain text file on the user's
/// disk that anything can edit — so a stem is *input*, and one containing `..` or
/// a separator would let a hand-edited store point the reader (and, worse, the
/// `remove` on delete) at a key outside the photo namespace. Natively that is a
/// path escape; on the web it is only a strange key, but there is no reason for
/// the two platforms to disagree about what a stem is. The generated form is
/// `place-<id>-<seq>`, so alphanumerics plus `-` and `_` is the whole alphabet we
/// ever need.
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
    storage::write_now(Namespace::Data, &full_key(stem), &prepared.full)?;
    storage::write_now(Namespace::Data, &thumb_key(stem), &prepared.thumb)
}

/// The small square image the Places row draws, or `None` if it is gone.
///
/// Synchronous on both platforms — see the module note on why the web preloads
/// these and not the full photos.
pub fn read_thumb(stem: &str) -> Option<Vec<u8>> {
    if !is_safe_stem(stem) {
        return None;
    }
    storage::read_now(Namespace::Data, &thumb_key(stem)).ready()
}

/// The full-size photo, for the viewer. `Loading` while the web build fetches it.
pub fn read_full(stem: &str) -> Cached {
    if !is_safe_stem(stem) {
        return Cached::Missing;
    }
    storage::read_now(Namespace::Data, &full_key(stem))
}

/// Forget a photo. Best-effort in both directions: an entry that is already gone
/// is not an error, and one that cannot be removed leaves an orphan rather than
/// blocking the delete of the place itself.
pub fn remove(stem: &str) {
    if !is_safe_stem(stem) {
        return;
    }
    storage::remove_now(Namespace::Data, &full_key(stem));
    storage::remove_now(Namespace::Data, &thumb_key(stem));
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
    fn a_stem_cannot_escape_the_photo_namespace() {
        // What a hand-edited (or migrated-from-elsewhere) user.json could contain.
        assert!(!is_safe_stem("../../../etc/passwd"));
        assert!(!is_safe_stem("sub/dir"));
        assert!(!is_safe_stem(".."));
        assert!(!is_safe_stem(""));
        assert!(!is_safe_stem(&"x".repeat(129)));
    }

    #[test]
    fn an_unsafe_stem_reads_as_absent_rather_than_reaching_the_store() {
        assert!(read_thumb("../secret").is_none());
        assert_eq!(read_full("../secret"), Cached::Missing);
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
    fn the_two_sizes_have_distinct_keys() {
        assert_eq!(full_key("place-1-1"), "photos/place-1-1.jpg");
        assert_eq!(thumb_key("place-1-1"), "photos/place-1-1.thumb.jpg");
    }
}
