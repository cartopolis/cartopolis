//! Named pieces of world the server can install into itself.
//!
//! # Why the content is here and not in a file next to the tool
//!
//! It began as a Lua file in the repository and a Python script that read it,
//! built a prim specification and posted it. That is wrong twice over. The
//! *definition* of a demo — which prims, where, what shape, which script — is a
//! statement about this world, so it belongs to the thing that owns the world;
//! and a deployed simulator has no repository checked out beside it, so content
//! kept in the tree is content a running server cannot install.
//!
//! So the scripts are `include_str!`-ed and travel inside the binary, and the
//! specifications are ordinary Rust beside them. `tools/author.py install
//! firebird` is now one HTTP call with a name in it; everything it used to
//! decide is decided here.
//!
//! # A pack is a canned request, not a second write path
//!
//! Each prim is a [`PrimSpec`] — the *same* type an HTTP body deserialises into —
//! and installing one runs it through the same handler an author's `POST` does.
//! There is one code path that creates a prim, and a pack cannot drift from it or
//! quietly acquire abilities the API does not have.
//!
//! # Installing twice replaces, and it matches on provenance
//!
//! Installing a pack removes whatever that pack installed last time, because the
//! alternative — a second firebird circling the same city every time somebody
//! re-ran the command — is the more surprising of the two.
//!
//! What it matches on is `prims.origin` (`V14__prim_origin.sql`), and that column
//! exists because the obvious cheaper thing does not work. The first version
//! matched the prim *names* a pack declares, and the pack broke it itself: the
//! beacon's script renames it to whatever its label currently says, so the second
//! install found nothing called "Beacon", left the old one standing and added
//! another. A script may rewrite any field a client can see; provenance is a fact
//! about the row and is not one of them.

use crate::authoring::{ParticleSpec, PrimSpec};

/// A named piece of world.
pub struct Pack {
    /// What `POST /api/admin/content/{name}` is keyed on.
    pub name: &'static str,
    /// One line, for the listing.
    pub summary: &'static str,
    /// What it installs. Order matters only in that they are created in it.
    pub prims: Vec<PrimSpec>,
}

/// Groningen's Grote Markt — the anchor of the seeded region, and where the
/// beacon stands.
const MARKT_LAT: f64 = 53.2194;
const MARKT_LNG: f64 = 6.5665;

/// Every pack this build carries.
#[must_use]
pub fn packs() -> Vec<Pack> {
    vec![firebird()]
}

/// One pack by name.
#[must_use]
pub fn pack(name: &str) -> Option<Pack> {
    packs().into_iter().find(|p| p.name == name)
}

/// The firebird and the beacon that calls it.
///
/// Two prims and two scripts, because the pair is the demonstration: a button
/// cannot reach the thing it controls except over a channel (`vfSend` /
/// `on_message`), and that is the only way any prim reaches any other.
fn firebird() -> Pack {
    Pack {
        name: "firebird",
        summary: "A burning thing that circles the city, and a beacon on the \
                  Grote Markt that calls it in and sends it back out.",
        prims: vec![
            PrimSpec {
                name: Some("Firebird".into()),
                shape: Some("cylinder".into()),
                latitude: Some(MARKT_LAT),
                longitude: Some(MARKT_LNG),
                altitude: Some(45.0),
                scale: Some([1.6, 0.9, 1.6]),
                color: Some([0.09, 0.07, 0.06]),
                // Alight in the very first snapshot that carries it, before its
                // own `on_start` has run — and a demonstration that an emitter is
                // an ordinary authored field rather than something only a script
                // can set. Only what differs from the fire preset is named, which
                // is the same way `vfParticles` takes a rules table.
                particles: Some(Some(ParticleSpec {
                    count: Some(120),
                    speed: Some(2.2),
                    source_radius: Some(0.6),
                    size_start: Some(1.1),
                    turbulence: Some(1.2),
                    ..ParticleSpec::default()
                })),
                script: Some(include_str!("../content/firebird.lua").into()),
                ..PrimSpec::default()
            },
            PrimSpec {
                name: Some("Beacon".into()),
                shape: Some("box".into()),
                // A few metres off the anchor, so it is beside the middle of the
                // square rather than exactly on the point every other thing in
                // this repository uses as its example coordinate.
                latitude: Some(53.21935),
                longitude: Some(6.56675),
                altitude: Some(0.75),
                scale: Some([1.1, 1.5, 0.16]),
                color: Some([0.10, 0.13, 0.16]),
                script: Some(include_str!("../content/beacon.lua").into()),
                ..PrimSpec::default()
            },
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_pack_is_named_and_reachable() {
        for p in packs() {
            assert!(!p.name.is_empty());
            assert!(!p.prims.is_empty(), "{} installs nothing", p.name);
            assert!(pack(p.name).is_some(), "{} is not findable by name", p.name);
        }
        assert!(pack("no-such-pack").is_none());
    }

    /// Not load-bearing for reinstall any more (that matches on `prims.origin`),
    /// but a prim nobody named is one a player cannot refer to — and the beacon's
    /// name is its label.
    #[test]
    fn every_prim_in_a_pack_is_named() {
        for p in packs() {
            for prim in &p.prims {
                assert!(
                    prim.name.as_ref().is_some_and(|n| !n.trim().is_empty()),
                    "a prim in {} has no name",
                    p.name
                );
            }
        }
    }

    /// The scripts are embedded, not read from disk — a deployed server has no
    /// repository beside it. An empty one means the `include_str!` path moved and
    /// somebody pointed it at a stub.
    #[test]
    fn every_pack_prim_carries_its_script() {
        for p in packs() {
            for prim in &p.prims {
                let source = prim.script.as_deref().unwrap_or_default();
                assert!(
                    source.len() > 200,
                    "{}'s {:?} has no script worth the name",
                    p.name,
                    prim.name
                );
            }
        }
    }

    /// The pair only works because they agree on a channel, and nothing but this
    /// test would notice one of them being edited.
    #[test]
    fn the_beacon_and_the_firebird_are_on_the_same_channel() {
        let channel = "local CHANNEL = 4747";
        let pack = pack("firebird").unwrap();
        for prim in &pack.prims {
            assert!(
                prim.script.as_deref().unwrap_or_default().contains(channel),
                "{:?} is not on the channel the other one is",
                prim.name
            );
        }
    }
}
