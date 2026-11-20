//! The drivable network the map's cells leave behind, as data: what
//! `map_geometry` fills per cell and `traffic` drives on. Its methods are
//! `traffic`'s; the type sits here, below both, so the map need not import
//! the traffic it feeds.

use bevy::prelude::*;
use cartopolis_geo::traffic::{Flow, Pavements, Uses};
use std::collections::HashMap;

/// Every drivable line in the streamed world, keyed by the cell that supplied it
/// so a retired cell takes its roads with it — the same contract
/// `WorldPlaces` and `StreetLabelStore` hold, and for the same reason.
#[derive(Resource, Default)]
pub struct DriveNetwork {
    pub(crate) cells: HashMap<(i64, i64), Vec<WorldDriveLine>>,
    /// Bumped whenever the set of lines changes, so a car holding an index into
    /// a retired cell can notice rather than dereference it.
    pub revision: u32,
}

/// One drivable line placed in the anchored world-XZ frame.
pub struct WorldDriveLine {
    pub points: Vec<Vec2>,
    pub flow: Flow,
    pub speed: f32,
    pub half_width: f32,
    /// Who may use it — `drive` is the tile's answer ([`Self::kind_drives`]) less
    /// the streets closed to cars ([`close_car_free_lines`]).
    pub uses: Uses,
    /// Whether the tile's road kind takes cars, before any closure. Kept so a
    /// closure that streams out again reopens the line.
    pub kind_drives: bool,
    /// Whether the tile's road kind takes bikes, before a cycle track beside
    /// it takes them off it ([`ride_the_tracks`]). `uses.cycle` is the answer.
    pub kind_cycles: bool,
    /// Cumulative length at each point, so a car can be advanced without walking
    /// the line from the start every frame.
    pub cumulative: Vec<f32>,
    /// Carriageway height at each point through a structure below the ground,
    /// `NaN` on the ground — see [`cartopolis_geo::traffic::DriveLine::deck`].
    /// Parallel to `points`, and every operation on one must keep it so.
    pub deck: Vec<f32>,
    /// This way is flagged `bridge`, so its height comes from `DeckPlates` —
    /// which is measured, and so arrives some frames after the cell does.
    pub on_deck: bool,
    /// Which sides have a pavement (`cartopolis_geo::traffic::keep_walkers_off`).
    pub pavements: Pavements,
    /// The corners of the line's bounding box, so a proximity query can skip
    /// a line without reading its points.
    pub bounds: (Vec2, Vec2),
}
