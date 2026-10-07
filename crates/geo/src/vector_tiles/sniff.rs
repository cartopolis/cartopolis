//! Recognising a tile body before parsing it: whether an undecoded MVT
//! mentions water or rail, and whether the bytes are a raster image rather
//! than an MVT at all.

use super::*;

/// The layers whose presence means the cell holds water of any kind:
/// [`WATER_LAYERS`] plus `water_lines`, which is drawn as a ribbon rather than
/// filled and so is deliberately not in that list.
pub const WATER_HINT_LAYERS: [&str; 3] = ["ocean", "water_polygons", "water_lines"];

/// Does this **undecoded** tile body mention a water layer at all?
///
/// A conservative pre-filter, not a parse, and the asymmetry is the whole point:
/// a false positive costs a caller one request it did not need, and a false
/// negative is impossible, because an MVT stores each layer's name verbatim as a
/// UTF-8 string in the body — a layer that exists cannot fail to be mentioned.
///
/// It exists because the decode belongs on a worker and this question has to be
/// answered before it. `map_geometry` fetches a cell's tile on the I/O pool and
/// hands the bytes to [`platform::workers`] to parse (ADR-033), so a consumer
/// that wants to skip a *network request* for a dry cell — `systems::nav_marks`
/// is the one — cannot wait for [`TileData::parse`] without moving a ~120 ms
/// decode onto the frame loop on wasm.
///
/// [`platform::workers`]: https://code.garage44.eu/jeroen/cartopolis
#[must_use]
pub fn body_mentions_water(body: &[u8]) -> bool {
    WATER_HINT_LAYERS
        .iter()
        .any(|name| contains_bytes(body, name.as_bytes()))
}

/// The `streets` `kind` values that are track rather than road. Every one of
/// them contains `rail` except `tram`, which is why [`body_mentions_rail`] tests
/// for two needles and not four.
const RAIL_HINTS: [&str; 2] = ["rail", "tram"];

/// Does this **undecoded** tile body mention a railway at all?
///
/// [`body_mentions_water`]'s sibling, for [`crate::crossings`], and it carries
/// the same asymmetry with one difference worth stating: water is a **layer
/// name**, while rail is a `kind` **value** inside `streets`. Both are stored
/// verbatim as UTF-8 in the body — a layer in its name table, a value in its
/// value table — so a false negative is impossible either way: a tile holding a
/// `kind = rail` street cannot fail to contain the bytes `rail`.
///
/// The false *positives* differ, and are wider here. `light_rail` contains
/// `rail`, which is wanted; a street **named** "Railroad Avenue", a POI called
/// "Tramhalte" or an attribute key nobody here reads all match too, and are not.
/// Each of those costs one request that comes back with no crossings in it —
/// which is the trade this predicate exists to make, and the right way round: a
/// missed railway would silently empty a layer, and a spare request over a street
/// with `tram` in its name would not.
#[must_use]
pub fn body_mentions_rail(body: &[u8]) -> bool {
    RAIL_HINTS
        .iter()
        .any(|name| contains_bytes(body, name.as_bytes()))
}

/// Naive substring search. The bodies are at most [`MAX_TILE_BYTES`] and the
/// needles are three short distinctive words, so this is a few hundred
/// microseconds and needs no dependency.
fn contains_bytes(hay: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() || hay.len() < needle.len() {
        return false;
    }
    let first = needle[0];
    hay[..=hay.len() - needle.len()]
        .iter()
        .enumerate()
        .any(|(i, b)| *b == first && &hay[i..i + needle.len()] == needle)
}

/// True if these bytes look like an image the `image` crate should decode, rather
/// than an MVT body. Sniffing beats configuration here: the tile URL template is a
/// plain `{z}/{x}/{y}` string that carries no format, and mixing raster and vector
/// sources (VersaTiles' satellite layer *is* raster) then needs no extra plumbing.
#[must_use]
pub fn looks_like_raster(bytes: &[u8]) -> bool {
    bytes.starts_with(b"\x89PNG\r\n\x1a\n")
        || bytes.starts_with(&[0xFF, 0xD8, 0xFF])
        || (bytes.len() > 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP")
}

#[cfg(test)]
mod tests {
    use super::*;

    // A real z14 tile over Groningen, from the VersaTiles Shortbread source
    // (now self-hosted at tiles.cartopolis.org; the archive is identical).
    use crate::testdata::GRONINGEN_Z14;

    /// The pre-filter `systems::nav_marks` skips a network request on, checked
    /// against the two answers that matter: a real canal city says yes, and a
    /// body with no water layer in it says no.
    ///
    /// The direction that must never be wrong is the second one — a false
    /// negative would silently un-draw a fairway's marks, while a false positive
    /// only costs a request.
    #[test]
    fn the_water_probe_reads_an_undecoded_body() {
        assert!(
            body_mentions_water(GRONINGEN_Z14),
            "a canal city's tile carries water layers"
        );
        let dry = synth::tile(&[synth::line_layer(
            "streets",
            4096,
            &[(100, 100), (900, 900)],
        )]);
        assert!(
            TileData::parse(&dry)
                .expect("dry tile parses")
                .layer("streets")
                .is_some(),
            "the synthetic tile is a tile"
        );
        assert!(
            !body_mentions_water(&dry),
            "a body with no water layer must not be probed as wet"
        );
    }

    /// The rail pre-filter `systems::crossings` skips a network request on.
    ///
    /// The direction that must never be wrong is the negative one, and it is
    /// harder to be sure of here than for water: rail is a `kind` **value**
    /// rather than a layer name, so this builds a tile whose *only* mention of a
    /// railway is that value — no layer called anything rail-ish, no other tag —
    /// and checks the probe still finds it. A body with an ordinary street in it
    /// and nothing else must answer no, or the layer would fetch the register
    /// over the whole planet.
    #[test]
    fn the_rail_probe_reads_a_kind_value_in_an_undecoded_body() {
        let railway = synth::tile(&[synth::tagged_line_layer(
            "streets",
            4096,
            ("kind", "rail"),
            &[(100, 100), (900, 900)],
        )]);
        assert!(
            TileData::parse(&railway)
                .expect("the railway tile parses")
                .layer("streets")
                .is_some(),
            "the synthetic tile is a tile"
        );
        assert!(
            body_mentions_rail(&railway),
            "a kind = rail street must never be probed as railless"
        );
        // …and the same for a tram, which is the one rail kind whose name does
        // not contain `rail` and so needs a needle of its own.
        let tramway = synth::tile(&[synth::tagged_line_layer(
            "streets",
            4096,
            ("kind", "tram"),
            &[(100, 100), (900, 900)],
        )]);
        assert!(body_mentions_rail(&tramway), "a tram is track too");

        let roads = synth::tile(&[synth::tagged_line_layer(
            "streets",
            4096,
            ("kind", "residential"),
            &[(100, 100), (900, 900)],
        )]);
        assert!(
            !body_mentions_rail(&roads),
            "a body with no railway in it must not be probed as railed"
        );
    }

    #[test]
    fn raster_sniffing() {
        assert!(looks_like_raster(b"\x89PNG\r\n\x1a\n____"));
        assert!(looks_like_raster(&[0xFF, 0xD8, 0xFF, 0xE0]));
        assert!(!looks_like_raster(GRONINGEN_Z14));
    }
}
