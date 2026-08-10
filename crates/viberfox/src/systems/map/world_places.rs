//! What the streamed world holds that an avatar can touch: the props standing in
//! its streets, the named places, and the house numbers.
//!
//! This is the ECS half of [`crate::systems::places`] and the furniture
//! extraction — the same shape as [`crate::systems::street_labels`]'s store, and
//! for the same reason. The extraction happens on the tile worker as part of the
//! one parse per cell; this holds the results, keyed by cell, so that retiring a
//! cell retires everything in it. A place that outlived its cell would be a
//! prompt for a shop that is no longer drawn, at a position the anchor has since
//! moved out from under.
//!
//! Positions arriving here are **tile-local**; they are converted to the anchored
//! world-XZ frame on insertion (the same frame `collision::BuildingColliders` and
//! `geo_nav` use), because every consumer wants them in that frame and converting
//! once per cell beats converting per query.

use bevy::prelude::*;
use std::collections::HashMap;

use crate::systems::furniture::{FurnitureInstance, FurnitureKind};
use crate::systems::places::{Address, Poi, PoiCategory};

/// One cell's worth of interactable content, straight off the worker in
/// tile-local metres.
#[derive(Default)]
pub struct CellPlaces {
    pub furniture: Vec<FurnitureInstance>,
    pub pois: Vec<Poi>,
    pub addresses: Vec<Address>,
}

/// A thing in the world the avatar can walk up to, in anchored world XZ.
#[derive(Clone)]
pub struct Interactable {
    pub pos: Vec2,
    /// What the prompt says, e.g. "Vismarkt" or "Bench".
    pub name: String,
    pub kind: InteractKind,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum InteractKind {
    /// A named POI from the `pois` layer.
    Place(PoiCategory),
    /// A piece of street furniture. Carries its form rather than a name, because
    /// it has none — a bench is "Bench".
    Prop(FurnitureKind),
}

impl InteractKind {
    #[must_use]
    pub fn glyph(self) -> &'static str {
        match self {
            Self::Place(c) => c.glyph(),
            Self::Prop(_) => "🪑",
        }
    }

    /// The verb the prompt offers. Props you *use*; places you *enter* or look at.
    #[must_use]
    pub fn verb(self) -> &'static str {
        match self {
            Self::Place(_) => "Look at",
            Self::Prop(FurnitureKind::Bench | FurnitureKind::PicnicTable) => "Sit at",
            Self::Prop(FurnitureKind::Board) => "Read",
            Self::Prop(FurnitureKind::Fountain) => "Watch",
            Self::Prop(_) => "Use",
        }
    }

    /// Descriptive suffix for the prompt, or `None` when the name already says it
    /// (a bench is called "Bench" — "Bench (bench)" reads as a stutter).
    #[must_use]
    pub fn detail(self) -> Option<&'static str> {
        match self {
            Self::Place(c) => Some(c.label()),
            Self::Prop(_) => None,
        }
    }
}

/// Display name for a furniture form. These stand in for the name the `pois`
/// feature does not have.
fn prop_name(kind: FurnitureKind) -> &'static str {
    match kind {
        FurnitureKind::Bench => "Bench",
        FurnitureKind::Bin => "Litter bin",
        FurnitureKind::Recycling => "Recycling point",
        FurnitureKind::Shelter => "Shelter",
        FurnitureKind::PicnicTable => "Picnic table",
        FurnitureKind::Cabinet => "Post box",
        FurnitureKind::Board => "Information board",
        FurnitureKind::Fountain => "Fountain",
        FurnitureKind::Artwork => "Artwork",
        FurnitureKind::Swing => "Playground",
    }
}

/// Radius (m) each furniture form occupies for collision. The meshes are built
/// from `furniture::parts`, but a single circle is the right collider for all of
/// them: they are small, roughly symmetric, and the avatar only ever needs to not
/// walk through one. A polygon per bin would cost far more than it bought.
fn prop_radius(kind: FurnitureKind) -> f32 {
    match kind {
        FurnitureKind::Bin | FurnitureKind::Cabinet => 0.35,
        FurnitureKind::Bench | FurnitureKind::Board => 0.6,
        FurnitureKind::Recycling | FurnitureKind::PicnicTable | FurnitureKind::Artwork => 0.9,
        FurnitureKind::Shelter => 1.4,
        FurnitureKind::Fountain => 2.0,
        // The swing is a token for a whole play area whose geometry we don't
        // have, so it is deliberately *not* solid: it stands at the centroid of a
        // polygon, and a collider there would be an invisible post in the middle
        // of a park.
        FurnitureKind::Swing => 0.0,
    }
}

/// Everything walkable-up-to in the streamed world, keyed by the cell that
/// supplied it so a retired cell takes its own content with it.
#[derive(Resource, Default)]
pub struct WorldPlaces {
    cells: HashMap<(i64, i64), Vec<Interactable>>,
    /// Solid props as `(centre, radius)` in anchored world XZ, kept per cell for
    /// the same reason.
    props: HashMap<(i64, i64), Vec<(Vec2, f32)>>,
    /// House-number points per cell, for the "where am I" readout.
    addresses: HashMap<(i64, i64), Vec<(Vec2, String)>>,
    /// Bumped whenever the contents change, so consumers that cache a nearest
    /// answer know to recompute rather than diffing the maps.
    pub revision: u32,
}

impl WorldPlaces {
    pub fn insert_cell(&mut self, cell: (i64, i64), wx: f32, wz: f32, places: CellPlaces) {
        let origin = Vec2::new(wx, wz);
        let mut interactables: Vec<Interactable> = Vec::new();
        let mut props: Vec<(Vec2, f32)> = Vec::new();

        for f in &places.furniture {
            let pos = origin + Vec2::new(f.x, f.z);
            let radius = prop_radius(f.kind);
            if radius > 0.0 {
                props.push((pos, radius));
            }
            interactables.push(Interactable {
                pos,
                name: prop_name(f.kind).to_string(),
                kind: InteractKind::Prop(f.kind),
            });
        }
        for p in places.pois {
            interactables.push(Interactable {
                pos: origin + Vec2::new(p.x, p.z),
                name: p.name,
                kind: InteractKind::Place(p.category),
            });
        }

        let addresses = places
            .addresses
            .into_iter()
            .map(|a| {
                let label = match a.street {
                    Some(street) => format!("{street} {}", a.housenumber),
                    None => a.housenumber,
                };
                (origin + Vec2::new(a.x, a.z), label)
            })
            .collect();

        self.cells.insert(cell, interactables);
        self.props.insert(cell, props);
        self.addresses.insert(cell, addresses);
        self.revision = self.revision.wrapping_add(1);
    }

    pub fn remove_cell(&mut self, cell: &(i64, i64)) {
        let had = self.cells.remove(cell).is_some()
            | self.props.remove(cell).is_some()
            | self.addresses.remove(cell).is_some();
        if had {
            self.revision = self.revision.wrapping_add(1);
        }
    }

    pub fn clear(&mut self) {
        if self.cells.is_empty() && self.props.is_empty() && self.addresses.is_empty() {
            return;
        }
        self.cells.clear();
        self.props.clear();
        self.addresses.clear();
        self.revision = self.revision.wrapping_add(1);
    }

    /// Every solid prop, for the collider index.
    pub fn props(&self) -> impl Iterator<Item = &(Vec2, f32)> {
        self.props.values().flatten()
    }

    /// The nearest interactable within `radius` metres of `pos`, or `None`.
    ///
    /// Linear over the loaded ring, which is a few thousand items at most and is
    /// queried once a frame. An index would be the obvious next step if the ring
    /// grew, but it would also have to be rebuilt on every cell change, and the
    /// ring changes far more often than this is queried is large.
    #[must_use]
    pub fn nearest(&self, pos: Vec2, radius: f32) -> Option<&Interactable> {
        let r2 = radius * radius;
        self.cells
            .values()
            .flatten()
            .map(|i| (i, i.pos.distance_squared(pos)))
            .filter(|(_, d2)| *d2 <= r2)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(i, _)| i)
    }

    /// The nearest house number to `pos`, within `radius` metres.
    #[must_use]
    pub fn nearest_address(&self, pos: Vec2, radius: f32) -> Option<&str> {
        let r2 = radius * radius;
        self.addresses
            .values()
            .flatten()
            .map(|(p, label)| (label, p.distance_squared(pos)))
            .filter(|(_, d2)| *d2 <= r2)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(label, _)| label.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn furniture(x: f32, z: f32, kind: FurnitureKind) -> FurnitureInstance {
        FurnitureInstance {
            x,
            z,
            yaw: 0.0,
            kind,
            luma: 1.0,
        }
    }

    fn poi(x: f32, z: f32, name: &str) -> Poi {
        Poi {
            x,
            z,
            name: name.to_string(),
            category: PoiCategory::Drink,
        }
    }

    /// Tile-local positions become world positions at the cell's own origin —
    /// the conversion every consumer depends on and none of them repeats.
    #[test]
    fn cell_contents_land_at_the_cells_origin() {
        let mut places = WorldPlaces::default();
        places.insert_cell(
            (1, 2),
            1000.0,
            -500.0,
            CellPlaces {
                furniture: vec![furniture(10.0, 20.0, FurnitureKind::Bench)],
                pois: vec![poi(30.0, 40.0, "Vismarkt")],
                addresses: Vec::new(),
            },
        );
        let bench = places
            .nearest(Vec2::new(1010.0, -480.0), 1.0)
            .expect("the bench is where the cell put it");
        assert_eq!(bench.name, "Bench");
        let cafe = places
            .nearest(Vec2::new(1030.0, -460.0), 1.0)
            .expect("and so is the cafe");
        assert_eq!(cafe.name, "Vismarkt");
    }

    /// Retiring a cell takes its content with it. A prompt for a shop in a cell
    /// that is no longer drawn is the failure this prevents.
    #[test]
    fn retiring_a_cell_removes_its_places() {
        let mut places = WorldPlaces::default();
        places.insert_cell(
            (0, 0),
            0.0,
            0.0,
            CellPlaces {
                furniture: vec![furniture(1.0, 1.0, FurnitureKind::Bin)],
                pois: Vec::new(),
                addresses: Vec::new(),
            },
        );
        assert!(places.nearest(Vec2::new(1.0, 1.0), 2.0).is_some());
        assert_eq!(places.props().count(), 1);

        let before = places.revision;
        places.remove_cell(&(0, 0));
        assert!(places.nearest(Vec2::new(1.0, 1.0), 2.0).is_none());
        assert_eq!(places.props().count(), 0);
        assert_ne!(places.revision, before, "consumers must be told");
    }

    /// `nearest` really is nearest, and really does respect the radius.
    #[test]
    fn nearest_picks_the_closest_inside_the_radius() {
        let mut places = WorldPlaces::default();
        places.insert_cell(
            (0, 0),
            0.0,
            0.0,
            CellPlaces {
                furniture: Vec::new(),
                pois: vec![poi(3.0, 0.0, "near"), poi(9.0, 0.0, "far")],
                addresses: Vec::new(),
            },
        );
        assert_eq!(places.nearest(Vec2::ZERO, 20.0).unwrap().name, "near");
        assert_eq!(
            places.nearest(Vec2::new(10.0, 0.0), 5.0).unwrap().name,
            "far"
        );
        assert!(
            places.nearest(Vec2::new(100.0, 0.0), 5.0).is_none(),
            "nothing within the radius means no prompt at all"
        );
    }

    /// The playground token is not solid — it stands at the centroid of an area
    /// we have no geometry for, and a collider there is an invisible post in a
    /// park.
    #[test]
    fn the_playground_token_is_interactable_but_not_solid() {
        let mut places = WorldPlaces::default();
        places.insert_cell(
            (0, 0),
            0.0,
            0.0,
            CellPlaces {
                furniture: vec![furniture(0.0, 0.0, FurnitureKind::Swing)],
                pois: Vec::new(),
                addresses: Vec::new(),
            },
        );
        assert!(
            places.nearest(Vec2::ZERO, 1.0).is_some(),
            "still promptable"
        );
        assert_eq!(places.props().count(), 0, "but never a wall");
    }

    /// A street and a number make one label; a number on its own still answers.
    #[test]
    fn addresses_read_as_a_street_and_a_number() {
        let mut places = WorldPlaces::default();
        places.insert_cell(
            (0, 0),
            0.0,
            0.0,
            CellPlaces {
                furniture: Vec::new(),
                pois: Vec::new(),
                addresses: vec![
                    Address {
                        x: 0.0,
                        z: 0.0,
                        housenumber: "26".into(),
                        street: Some("Oude Kijk in 't Jatstraat".into()),
                    },
                    Address {
                        x: 50.0,
                        z: 0.0,
                        housenumber: "3".into(),
                        street: None,
                    },
                ],
            },
        );
        assert_eq!(
            places.nearest_address(Vec2::ZERO, 5.0),
            Some("Oude Kijk in 't Jatstraat 26")
        );
        assert_eq!(places.nearest_address(Vec2::new(50.0, 0.0), 5.0), Some("3"));
        assert_eq!(places.nearest_address(Vec2::new(500.0, 0.0), 5.0), None);
    }
}
