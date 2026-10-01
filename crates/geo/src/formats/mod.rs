//! The per-cell files atlas writes and the client reads, as one definition.
//!
//! Every dataset atlas's `pipeline/` produces is a tree of z14 cells —
//! `<dataset>/14/{x}/{y}.json` — that a streamer in the client fetches and
//! parses. Until this module the writer was a `json!` in atlas and the reader a
//! private serde struct in the client, with no type between them, so a renamed
//! key compiled on both sides and arrived as a layer that drew nothing. Each
//! submodule here is one of those files, with `Serialize` for atlas and
//! `Deserialize` for the client on the **same** struct.
//!
//! # Byte compatibility
//!
//! The files are already on the host and in every installed client's cache, so
//! these types reproduce the old output exactly rather than improving on it:
//!
//! * **Fields are declared in alphabetical order.** The old writer built a
//!   `serde_json::Value`, whose map (without `preserve_order`, which nothing in
//!   this workspace enables) is a `BTreeMap` and so emits keys sorted; a struct
//!   emits them in declaration order. Serialise these types directly
//!   ([`to_vec`]) — never through `serde_json::to_value`, which would widen an
//!   `f32` to the `f64` nearest it and print `3.7` as `3.700000047683716`.
//! * **Optional keys are skipped when absent**, exactly where the old writer
//!   left them out, and written (even empty) where it always wrote them.
//! * **Readers stay lenient**: every field an older file may lack is
//!   `#[serde(default)]`, and trailing tuple elements that were appended to a
//!   row later default too.
//!
//! Each submodule's tests pin both directions against a literal captured from
//! the writer as it was: the literal parses to the expected value, and the value
//! serialises back to the literal byte for byte.

pub mod bag;
pub mod bgt;
pub mod dikes;
pub mod lod22;
pub mod osm_buildings;
pub mod roofs;
pub mod water;

/// The slippy-map zoom every per-cell tree on the data host is binned at.
///
/// **One number, read by both ends.** A cell numbered differently by the writer
/// and the reader is a cell the client fetches from an address nothing was
/// written to — a 404 per tile and an empty map, not an error — which is why
/// atlas's `pipeline::tile` and every client streamer of these trees take it
/// from here rather than each writing `14` beside a "must match" comment.
pub const CACHE_ZOOM: u32 = 14;

/// A `(latitude°, longitude°)` pair, which every one of these formats writes as
/// a two-element JSON array.
pub type LatLng = (f64, f64);

/// Serialise one of these documents the way the files are written: compact,
/// fields in declaration order.
///
/// # Errors
///
/// Only what `serde_json` itself can fail on — a map with non-string keys, which
/// none of these types has.
pub fn to_vec<T: serde::Serialize>(doc: &T) -> Result<Vec<u8>, serde_json::Error> {
    serde_json::to_vec(doc)
}

/// Parse one of these documents from text, with the error as a string — the
/// shape every client streamer's parse result already has.
///
/// # Errors
///
/// When the text is not JSON, or not this document.
pub fn from_str<'de, T: serde::Deserialize<'de>>(text: &'de str) -> Result<T, String> {
    serde_json::from_str(text).map_err(|e| e.to_string())
}

/// Whether a count is zero — for `skip_serializing_if` on a field the writer
/// omits when it is 0.
#[allow(clippy::trivially_copy_pass_by_ref)] // serde passes a reference
fn is_zero(v: &u32) -> bool {
    *v == 0
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_cache_zoom_is_fourteen() {
        // Changing it moves every cell on the host; this is a deliberate act.
        assert_eq!(super::CACHE_ZOOM, 14);
    }
}
