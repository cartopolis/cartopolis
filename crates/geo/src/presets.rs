//! Names for OSM tags, imported from iD's tagging schema.
//!
//! [`crate::places`] classifies a POI into one of twelve [`PoiCategory`] buckets
//! and that is all it ever knew: a bakery, a butcher and a phone shop are each
//! `Shop`, and the proximity prompt reads `Look at Bakkerij Klaas (shop)` for
//! all three. The specific tag was read, used to pick the bucket, and dropped.
//!
//! The vocabulary that names those things is not ours to write — it is
//! `@openstreetmap/id-tagging-schema`, the catalog behind the iD editor on
//! openstreetmap.org, maintained by the OSM community and ISC licensed.
//! `tools/id_presets.py` flattens it into [`preset_table::TABLE`]; this module
//! is the lookup over that table. 660 rows, ~32 KB of source, resolved at
//! compile time — no asset to ship, no fetch, no runtime parse, and it works
//! offline like every other compiled-in table here.
//!
//! # What this deliberately does not do
//!
//! It supplies a **label**, not an icon. iD's icons are maki/temaki sprite ids
//! and this client has no atlas to draw them from — its glyphs have to be
//! characters the bundled font stack actually contains, which is the whole
//! subject of [`PoiCategory::glyph`]'s doc comment and of the tofu bug that
//! preceded it. So the glyph still comes from the coarse category and only the
//! words get more specific. Pairing 660 labels with 12 glyphs is not a
//! compromise: the prompt names the place beside the mark, so the mark only has
//! to be distinguishable.
//!
//! [`PoiCategory`]: crate::places::PoiCategory
//! [`PoiCategory::glyph`]: crate::places::PoiCategory::glyph

use crate::preset_table::TABLE;

/// The human label for an OSM `key=value`, or `None` when the schema has no
/// single-tag preset for it.
///
/// Binary search over a table the generator sorts, which
/// [`tests::the_table_is_sorted_and_unique`] pins — an unsorted table would not
/// fail loudly, it would silently answer `None` for arbitrary rows.
#[must_use]
pub fn label_for(key: &str, value: &str) -> Option<&'static str> {
    TABLE
        .binary_search_by(|(k, v, _)| (*k, *v).cmp(&(key, value)))
        .ok()
        .map(|i| TABLE[i].2)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `label_for` is a binary search, so this is a correctness precondition and
    /// not a style rule. Also catches a duplicated `(key, value)`, which would
    /// make which label you get depend on where the search happened to land.
    #[test]
    fn the_table_is_sorted_and_unique() {
        for pair in TABLE.windows(2) {
            let (ak, av, _) = pair[0];
            let (bk, bv, _) = pair[1];
            assert!(
                (ak, av) < (bk, bv),
                "table out of order or duplicated at {ak}={av} / {bk}={bv} — rerun tools/id_presets.py"
            );
        }
    }

    /// The table is present and plausible. A regeneration that silently emitted
    /// nothing would otherwise show up only as every prompt quietly reverting to
    /// its coarse category, which reads as "the feature was reverted".
    #[test]
    fn the_table_is_populated() {
        assert!(
            TABLE.len() > 500,
            "only {} presets compiled in",
            TABLE.len()
        );
    }

    /// The specific thing this whole module exists for: three shops that used to
    /// be indistinguishable now have three different names.
    #[test]
    fn shops_are_no_longer_all_shop() {
        assert_eq!(label_for("shop", "bakery"), Some("Bakery"));
        assert_eq!(label_for("shop", "butcher"), Some("Butcher"));
        assert_eq!(label_for("amenity", "cafe"), Some("Cafe"));
        assert_eq!(label_for("tourism", "museum"), Some("Museum"));
    }

    /// An unknown value falls through rather than matching its neighbour — the
    /// failure a `binary_search` used as a prefix match would produce.
    #[test]
    fn an_unknown_tag_has_no_label() {
        assert_eq!(label_for("shop", "definitely_not_a_shop_type"), None);
        assert_eq!(label_for("not_a_key", "bakery"), None);
    }
}
