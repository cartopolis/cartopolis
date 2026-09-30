//! The street objects an operator may replace with a model.
//!
//! # Why this vocabulary is here and not in the client
//!
//! A bin, a lamppost and a bench are drawn by the client from a national or a
//! municipal register — nobody placed them, so nobody can select one and swap it.
//! What *can* be said is "on this server, a lamppost looks like this model", and
//! that is an operator's statement about the whole world rather than a player's
//! about one object.
//!
//! Saying it needs one word both ends agree on. The forms themselves are
//! `cartopolis_geo::furniture::FurnitureKind`, which the simulator cannot see —
//! `cartopolis_geo` is the client's map pipeline and the server has no map at all
//! — so the *slugs* live here, in the crate both already depend on, and
//! `FurnitureKind::slug` maps onto them. A test in `cartopolis_geo` pins the two
//! lists equal; without it a renamed form compiles on both sides and arrives as a
//! placeholder that silently overrides nothing.
//!
//! # The list is closed, and that is the point
//!
//! Free text would mean a curator typing `lamppost` where the client says `lamp`
//! and getting no error and no lamppost. The server refuses a slug that is not
//! here, which is the same rule the shot manifest and `--expect` follow: an
//! unknown key is a hard error, because a setting that quietly does nothing is
//! worse than one that was refused.

/// Every form a placeholder model may replace, as it travels over HTTP.
///
/// Ordered as a person reads them — the things a street is full of first — since
/// this is also the order the operator GUI lists them in.
pub const PLACEHOLDER_KINDS: &[&str] = &[
    "lamp",
    "bench",
    "bin",
    "recycling",
    "shelter",
    "picnic-table",
    "cabinet",
    "board",
    "fountain",
    "artwork",
    "swing",
    "car",
    "bicycle",
    "person",
    "boat",
];

/// Whether a slug names a form that may be overridden.
#[must_use]
pub fn is_placeholder_kind(slug: &str) -> bool {
    PLACEHOLDER_KINDS.contains(&slug)
}

/// How far a placeholder model may be scaled, either way.
///
/// A bound rather than a free number for the reason `MAX_SEAT_OFFSET_M` has one:
/// the server cannot see the model or the street, so the one thing it can do is
/// refuse a value that would put a 400 m bench through a city.
pub const MIN_PLACEHOLDER_SCALE: f32 = 0.05;
/// See [`MIN_PLACEHOLDER_SCALE`].
pub const MAX_PLACEHOLDER_SCALE: f32 = 20.0;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_slugs_are_unique_and_lower_case() {
        let mut seen = std::collections::HashSet::new();
        for slug in PLACEHOLDER_KINDS {
            assert!(seen.insert(*slug), "{slug} is listed twice");
            assert_eq!(*slug, slug.to_lowercase(), "{slug} is not lower case");
            assert!(!slug.is_empty());
        }
    }

    #[test]
    fn an_unknown_slug_is_refused() {
        assert!(is_placeholder_kind("lamp"));
        assert!(!is_placeholder_kind("lamppost"));
        assert!(!is_placeholder_kind(""));
    }

    /// The atlas web UI's copy, `PLACEHOLDER_KINDS` in
    /// `crates/atlas/web/src/lib/library.ts`, is what the operator's dropdown
    /// offers. A slug missing there cannot be set; one only there is refused by
    /// the server. Same order too, since both are the order a person reads.
    #[test]
    fn the_web_copy_lists_the_same_slugs() {
        let web = crate::ts_mirror::string_array(
            include_str!("../../atlas/web/src/lib/library.ts"),
            "PLACEHOLDER_KINDS",
        )
        .expect("`PLACEHOLDER_KINDS` is missing from atlas/web/src/lib/library.ts");
        assert_eq!(
            web, PLACEHOLDER_KINDS,
            "atlas/web/src/lib/library.ts's PLACEHOLDER_KINDS has drifted from \
             cartopolis_core::placeholders::PLACEHOLDER_KINDS"
        );
    }
}
