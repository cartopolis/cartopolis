//! Building designs: where they are kept, how they are published, and the
//! starting design for a building nobody has drawn yet
//! (`docs/notes/building-designs.md`, `cartopolis_geo::building_design`).
//!
//! **Atlas keeps the design; the simulator decides who may change it.** A design
//! is map data — the client streams it beside the 3DBAG shells and the BAG
//! outlines atlas already writes — while ownership is an account fact, and the
//! accounts are the simulator's. So a save asks the simulator
//! (`/api/buildings/{pand}/access`, [`crate::auth`]) and believes the answer, the
//! same way the pipeline routes ask whether a bearer is an admin.
//!
//! Two trees, on purpose:
//!
//! * `data_dir/designs/{pand}.json` is the **source of truth**, one file per
//!   building, plus every published revision under `history/` — a person's work
//!   is not something a bad save should be able to lose;
//! * `site_dir/designs/{z}/{x}/{y}.json` is what Caddy **serves**: every design
//!   whose origin lies in that cell, and a `coverage.json` naming each covered
//!   cell with its revision. The revision is what lets a client hold a design in
//!   its cache forever and still see an edit: the cache key carries it.
//!
//! Both are rewritten by rename, never in place, so a client fetching while a
//! person saves gets the old file or the new one and never half of each.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use cartopolis_geo::building_design::{
    self as bd, BuildingDesign, DesignSource, DESIGN_ZOOM, FORMAT_VERSION,
};
use serde::Serialize;

/// The cell every BAG and LoD2.2 file is filed under.
const CELL_ZOOM: u32 = 14;

/// What the registers say about one building, for the editor to draw under the
/// plan and for [`Designs::generated`] to lay a design out from.
#[derive(Clone, Debug, Serialize)]
pub struct Facts {
    pub pand: String,
    /// The BAG outline, `[lat, lng]`.
    pub outline: Vec<[f64; 2]>,
    /// The entrances, already moved onto a facade nobody stands behind.
    pub doors: Vec<[f64; 2]>,
    /// The shell's eaves datum, metres, when the LoD2.2 cell is on this host.
    pub height: Option<f32>,
    /// Neighbouring outlines within a few metres, so the editor can show the
    /// party walls.
    pub neighbours: Vec<Vec<[f64; 2]>>,
}

pub struct Designs {
    data_dir: PathBuf,
    site_dir: PathBuf,
    /// One save at a time: a save rewrites a shared cell file and the shared
    /// manifest, and two interleaved would lose one of them.
    lock: Mutex<()>,
}

/// A BAG identificatie is sixteen digits. Anything else is refused before it is
/// near a path.
#[must_use]
pub fn valid_pand(pand: &str) -> bool {
    (4..=20).contains(&pand.len()) && pand.bytes().all(|b| b.is_ascii_digit())
}

impl Designs {
    pub fn new(data_dir: &Path, site_dir: &Path) -> Self {
        Self {
            data_dir: data_dir.join("designs"),
            site_dir: site_dir.to_path_buf(),
            lock: Mutex::new(()),
        }
    }

    fn current_path(&self, pand: &str) -> PathBuf {
        self.data_dir.join(format!("{pand}.json"))
    }

    /// The stored design for a building, if anybody saved one.
    #[must_use]
    pub fn stored(&self, pand: &str) -> Option<BuildingDesign> {
        if !valid_pand(pand) {
            return None;
        }
        let text = std::fs::read_to_string(self.current_path(pand)).ok()?;
        BuildingDesign::from_json(&text).ok()
    }

    /// Store and publish a design. Returns it as published (with its revision).
    ///
    /// The revision is the publish time in milliseconds, forced past the previous
    /// one, so it orders saves and is also legible in a listing.
    pub fn save(&self, mut design: BuildingDesign) -> Result<BuildingDesign, String> {
        if !valid_pand(&design.pand) {
            return Err("that is not a BAG building number".into());
        }
        design.version = FORMAT_VERSION;
        if design.source == DesignSource::Generated {
            design.source = DesignSource::Editor;
        }
        design.validate()?;
        let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        let previous = self.stored(&design.pand);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as u64);
        design.revision = now.max(previous.as_ref().map_or(0, |p| p.revision + 1));
        let text = serde_json::to_vec_pretty(&design).map_err(|e| e.to_string())?;
        let history = self
            .data_dir
            .join("history")
            .join(&design.pand)
            .join(format!("{}.json", design.revision));
        write_atomic(&history, &text).map_err(|e| format!("could not keep the history: {e}"))?;
        write_atomic(&self.current_path(&design.pand), &text)
            .map_err(|e| format!("could not store the design: {e}"))?;
        // A design whose origin moved cells (it was re-placed) must leave the
        // cell it used to be published in.
        let mut cells = vec![cell_of(design.origin)];
        if let Some(p) = &previous {
            cells.push(cell_of(p.origin));
        }
        cells.dedup();
        for cell in cells {
            self.publish_cell(cell)
                .map_err(|e| format!("stored, but could not publish: {e}"))?;
        }
        Ok(design)
    }

    /// Forget a building's design, so it goes back to the generated one. The
    /// history is kept.
    pub fn remove(&self, pand: &str) -> Result<bool, String> {
        if !valid_pand(pand) {
            return Ok(false);
        }
        let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        let Some(previous) = self.stored(pand) else {
            return Ok(false);
        };
        std::fs::remove_file(self.current_path(pand)).map_err(|e| e.to_string())?;
        self.publish_cell(cell_of(previous.origin))
            .map_err(|e| e.to_string())?;
        Ok(true)
    }

    /// Every stored design.
    fn all(&self) -> Vec<BuildingDesign> {
        let Ok(dir) = std::fs::read_dir(&self.data_dir) else {
            return Vec::new();
        };
        dir.filter_map(Result::ok)
            .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
            .filter_map(|e| std::fs::read_to_string(e.path()).ok())
            .filter_map(|t| BuildingDesign::from_json(&t).ok())
            .collect()
    }

    /// Rewrite one served cell and the manifest.
    ///
    /// A full scan of the stored designs per save: there are as many as people
    /// have drawn, which is hundreds before it is anything, and the scan is what
    /// keeps the served tree a pure function of the stored one.
    ///
    /// The stamping — written with the cell, only ever forward — is
    /// `crate::stamped`'s, shared with the building overrides.
    fn publish_cell(&self, cell: (i64, i64)) -> std::io::Result<()> {
        let all = self.all();
        let here: Vec<&BuildingDesign> = all.iter().filter(|d| cell_of(d.origin) == cell).collect();
        let body = if here.is_empty() {
            None
        } else {
            Some(serde_json::to_vec(&serde_json::json!({ "designs": here }))?)
        };
        let newest = here.iter().map(|d| d.revision).max().unwrap_or(0);
        let mut extra = serde_json::Map::new();
        extra.insert(
            "attribution".into(),
            "Building designs by their owners".into(),
        );
        crate::stamped::publish_cell(
            &self.site_dir.join("designs"),
            DESIGN_ZOOM,
            cell,
            body.as_deref(),
            newest,
            extra,
        )
    }

    /// What the registers on this host say about a building. `near` is any point
    /// in or by it (the address a claim was made through will do); its cell and
    /// the eight around it are searched, because a building is filed in one cell
    /// and the point need not be in the same one.
    #[must_use]
    pub fn facts(&self, pand: &str, near: (f64, f64)) -> Option<Facts> {
        if !valid_pand(pand) {
            return None;
        }
        let (cx, cy) = cartopolis_core::world::lat_lng_to_tile(near.0, near.1, CELL_ZOOM);
        for (dx, dy) in [
            (0, 0),
            (-1, 0),
            (1, 0),
            (0, -1),
            (0, 1),
            (-1, -1),
            (1, -1),
            (-1, 1),
            (1, 1),
        ] {
            let cell = (cx + dx, cy + dy);
            let Some(bag) = self.bag_cell(cell) else {
                continue;
            };
            let Some(me) = bag.iter().find(|b| b.id == pand) else {
                continue;
            };
            if me.fp.len() < 3 {
                return None;
            }
            let outline: Vec<[f64; 2]> = me.fp.iter().map(|&(a, b)| [a, b]).collect();
            let centre = centroid(&outline);
            let neighbours: Vec<Vec<[f64; 2]>> = bag
                .iter()
                .filter(|b| b.id != pand && b.fp.len() >= 3)
                .map(|b| b.fp.iter().map(|&(a, o)| [a, o]).collect::<Vec<_>>())
                .filter(|ring: &Vec<[f64; 2]>| {
                    ring.iter().any(|p| metres_between(*p, centre) < 60.0)
                })
                .collect();
            // A party wall is a wall with a neighbour *beside* it. An outline
            // that contains this building is not that: the register files some
            // structures over or under another (Prinsenstraat 24–26 sits wholly
            // inside a 365 m² outline from 2000), and counting one as a
            // neighbour makes every wall a party wall and loses the door.
            let beside: Vec<Vec<[f64; 2]>> = neighbours
                .iter()
                .filter(|r| !point_in_latlng_ring(r, centre))
                .cloned()
                .collect();
            // Where no facade will take an entrance, the registered point is kept
            // — the client does the same, and the generator then snaps it to the
            // nearest wall within reach.
            let doors = me
                .doors
                .iter()
                .map(|&(a, b)| door_on_facade(&outline, [a, b], &beside).unwrap_or([a, b]))
                .collect();
            return Some(Facts {
                pand: pand.to_string(),
                height: self.shell_height(cell, pand),
                outline,
                doors,
                neighbours,
            });
        }
        None
    }

    /// The design a building gets when nobody has drawn one: what the client
    /// builds for it, and where the editor starts.
    #[must_use]
    pub fn generated(&self, facts: &Facts) -> Option<BuildingDesign> {
        let origin = centroid(&facts.outline);
        let outline: Vec<[f32; 2]> = facts
            .outline
            .iter()
            .map(|p| bd::to_local(origin, p[0], p[1]))
            .collect();
        let doors: Vec<[f32; 2]> = facts
            .doors
            .iter()
            .map(|p| bd::to_local(origin, p[0], p[1]))
            .collect();
        // With windows: this is where an owner starts, and their first save
        // makes these outer walls the building's facade.
        bd::generate(&facts.pand, origin, &outline, facts.height, &doors).map(|mut d| {
            d.with_facade_windows();
            d
        })
    }

    fn bag_cell(&self, (x, y): (i64, i64)) -> Option<Vec<cartopolis_geo::formats::bag::Building>> {
        let path = self.site_dir.join(format!("bag/{CELL_ZOOM}/{x}/{y}.json"));
        let text = std::fs::read_to_string(path).ok()?;
        cartopolis_geo::formats::bag::parse(&text).ok()?.buildings
    }

    /// The shell's eaves datum from the LoD2.2 cell, binary first as the client
    /// reads it.
    fn shell_height(&self, (x, y): (i64, i64), pand: &str) -> Option<f32> {
        let root = self.site_dir.join(format!("lod22/{CELL_ZOOM}/{x}"));
        let cell = std::fs::read(root.join(format!("{y}.bin")))
            .ok()
            .and_then(|b| cartopolis_geo::lod22_cell::decode(&b).ok())
            .or_else(|| {
                std::fs::read_to_string(root.join(format!("{y}.json")))
                    .ok()
                    .and_then(|t| cartopolis_geo::formats::lod22::parse(&t).ok())
            })?;
        let mut heights = Vec::new();
        let mut roof = Vec::new();
        for e in cell.elements.iter().filter(|e| e.id == pand) {
            heights.extend(e.p.iter().map(|p| p.2));
            for &(i, j, k, is_roof) in &e.t {
                if is_roof == 1 {
                    for ix in [i, j, k] {
                        if let Some(p) = e.p.get(ix as usize) {
                            roof.push(p.2);
                        }
                    }
                }
            }
        }
        bd::shell_datum_height(&heights, &roof)
    }
}

/// The z14 cell a point is filed under.
#[must_use]
pub fn cell_of(origin: [f64; 2]) -> (i64, i64) {
    cartopolis_core::world::lat_lng_to_tile(origin[0], origin[1], DESIGN_ZOOM)
}

fn centroid(ring: &[[f64; 2]]) -> [f64; 2] {
    let n = ring.len().max(1) as f64;
    let (a, b) = ring
        .iter()
        .fold((0.0, 0.0), |(a, b), p| (a + p[0], b + p[1]));
    [a / n, b / n]
}

fn point_in_latlng_ring(ring: &[[f64; 2]], p: [f64; 2]) -> bool {
    let local: Vec<[f32; 2]> = ring.iter().map(|q| bd::to_local(p, q[0], q[1])).collect();
    bd::point_in_ring(&local, [0.0, 0.0])
}

fn metres_between(a: [f64; 2], b: [f64; 2]) -> f64 {
    let k = a[0].to_radians().cos();
    ((a[0] - b[0]) * bd::M_PER_DEG_LAT).hypot((a[1] - b[1]) * bd::M_PER_DEG_LAT * k)
}

/// Move a registered entrance onto the nearest wall whose outside is not inside a
/// neighbour — the party-wall rule the client applies
/// (`building_facts::place_doors_on_facades`), so the editor starts with the door
/// where the game would put it.
fn door_on_facade(
    outline: &[[f64; 2]],
    door: [f64; 2],
    neighbours: &[Vec<[f64; 2]>],
) -> Option<[f64; 2]> {
    let origin = outline[0];
    let ring: Vec<[f32; 2]> = outline
        .iter()
        .map(|p| bd::to_local(origin, p[0], p[1]))
        .collect();
    let others: Vec<Vec<[f32; 2]>> = neighbours
        .iter()
        .map(|r| r.iter().map(|p| bd::to_local(origin, p[0], p[1])).collect())
        .collect();
    let d = bd::to_local(origin, door[0], door[1]);
    let n = ring.len();
    let mut best: Option<(f32, [f32; 2])> = None;
    for i in 0..n {
        let (a, b) = (ring[i], ring[(i + 1) % n]);
        let ab = [b[0] - a[0], b[1] - a[1]];
        let len = ab[0].hypot(ab[1]);
        if len < 1.2 {
            continue;
        }
        let dir = [ab[0] / len, ab[1] / len];
        // Clear of the corners by half a doorway.
        let t = ((d[0] - a[0]) * dir[0] + (d[1] - a[1]) * dir[1]).clamp(0.6, len - 0.6);
        let on = [a[0] + dir[0] * t, a[1] + dir[1] * t];
        let normal = [-dir[1], dir[0]];
        let probe_l = [on[0] + normal[0] * 0.3, on[1] + normal[1] * 0.3];
        let probe_r = [on[0] - normal[0] * 0.3, on[1] - normal[1] * 0.3];
        let outside = if bd::point_in_ring(&ring, probe_l) {
            probe_r
        } else {
            probe_l
        };
        if others.iter().any(|o| bd::point_in_ring(o, outside)) {
            continue; // a party wall: the door would open into the neighbour
        }
        let dist = (on[0] - d[0]).hypot(on[1] - d[1]);
        if best.is_none_or(|(b, _)| dist < b) {
            best = Some((dist, on));
        }
    }
    best.map(|(_, p)| {
        let (lat, lng) = bd::to_lat_lng(origin, p);
        [lat, lng]
    })
}

/// Write by rename, so nobody reads half a file.
use crate::stamped::write_atomic;

/// The eastern house of [`two_houses`].
#[cfg(test)]
pub(crate) const EAST: &str = "0014100000000002";

#[cfg(test)]
/// A terrace of two houses in the site tree's own BAG format, side by side:
/// the shared wall is the west wall of the eastern one.
pub(crate) fn two_houses(root: &Path) {
    let (lat, lng): (f64, f64) = (53.2194, 6.5665);
    let m_lat = 1.0 / bd::M_PER_DEG_LAT;
    let m_lng = 1.0 / (bd::M_PER_DEG_LAT * lat.to_radians().cos());
    let rect = |x0: f64, x1: f64| -> Vec<[f64; 2]> {
        [(x0, 0.0), (x1, 0.0), (x1, 12.0), (x0, 12.0)]
            .iter()
            .map(|&(x, y)| [lat + y * m_lat, lng + x * m_lng])
            .collect()
    };
    let west = rect(0.0, 6.0);
    let east = rect(6.0, 12.0);
    // East's registered entrance sits 1.5 m in from the shared wall, which
    // is nearer than its own street wall: the party-wall trap.
    let door = [lat + 6.0 * m_lat, lng + 7.5 * m_lng];
    let cell = serde_json::json!({
        "buildings": [
            {"id": "0014100000000001", "fp": west, "doors": []},
            {"id": EAST, "fp": east, "doors": [door], "units": 1},
        ]
    });
    let (cx, cy) = cartopolis_core::world::lat_lng_to_tile(lat, lng, CELL_ZOOM);
    let path = root.join(format!("bag/{CELL_ZOOM}/{cx}/{cy}.json"));
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, serde_json::to_vec(&cell).unwrap()).unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_starting_design_has_its_door_off_the_party_wall() {
        let dir = tempfile::tempdir().unwrap();
        let site = dir.path().join("site");
        two_houses(&site);
        let designs = Designs::new(&dir.path().join("data"), &site);
        let facts = designs
            .facts("0014100000000002", (53.2194, 6.5665))
            .expect("the building is in the cell");
        assert_eq!(facts.neighbours.len(), 1);
        assert_eq!(facts.doors.len(), 1);
        let design = designs.generated(&facts).expect("a house holds a room");
        let door = design.entrances()[0];
        // Not on the shared (west) wall, whose local x is about -3 from the
        // centroid of a 6 m wide house.
        assert!(
            door[0] > -2.5,
            "the door was put on the party wall: {door:?}"
        );
    }

    /// A building the register files wholly inside another outline: that outline
    /// is not a neighbour, and must not turn every wall into a party wall.
    #[test]
    fn an_outline_around_the_building_is_not_a_party_wall() {
        let dir = tempfile::tempdir().unwrap();
        let site = dir.path().join("site");
        two_houses(&site);
        // Wrap both houses in one big outline, as the register sometimes does.
        let (cx, cy) = cartopolis_core::world::lat_lng_to_tile(53.2194, 6.5665, CELL_ZOOM);
        let path = site.join(format!("bag/{CELL_ZOOM}/{cx}/{cy}.json"));
        let mut cell: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let m = 1.0 / bd::M_PER_DEG_LAT;
        let k = 1.0 / (bd::M_PER_DEG_LAT * 53.2194f64.to_radians().cos());
        let big: Vec<[f64; 2]> = [(-20.0, -20.0), (40.0, -20.0), (40.0, 40.0), (-20.0, 40.0)]
            .iter()
            .map(|&(x, y)| [53.2194 + y * m, 6.5665 + x * k])
            .collect();
        cell["buildings"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({"id": "0014100000000009", "fp": big}));
        std::fs::write(&path, serde_json::to_vec(&cell).unwrap()).unwrap();
        let designs = Designs::new(&dir.path().join("data"), &site);
        let facts = designs.facts(EAST, (53.2194, 6.5665)).unwrap();
        let design = designs.generated(&facts).unwrap();
        assert_eq!(design.entrances().len(), 1, "the door was lost");
        assert!(
            design.entrances()[0][0] > -2.5,
            "the door is on the party wall"
        );
    }

    #[test]
    fn a_save_is_published_with_a_revision_and_a_removal_unpublishes_it() {
        let dir = tempfile::tempdir().unwrap();
        let site = dir.path().join("site");
        two_houses(&site);
        let designs = Designs::new(&dir.path().join("data"), &site);
        let facts = designs
            .facts("0014100000000002", (53.2194, 6.5665))
            .unwrap();
        let design = designs.generated(&facts).unwrap();

        let saved = designs.save(design.clone()).unwrap();
        assert!(saved.revision > 0);
        assert_eq!(saved.source, DesignSource::Editor);
        let again = designs.save(design).unwrap();
        assert!(again.revision > saved.revision, "revisions only go up");

        let (cx, cy) = cell_of(saved.origin);
        let manifest = || -> serde_json::Value {
            serde_json::from_slice(&std::fs::read(site.join("designs/coverage.json")).unwrap())
                .unwrap()
        };
        let first = manifest()["cells"][0].clone();
        assert_eq!((first[0].as_i64(), first[1].as_i64()), (Some(cx), Some(cy)));
        assert!(first[2].as_u64().unwrap() >= again.revision);
        let cell: serde_json::Value = serde_json::from_slice(
            &std::fs::read(site.join(format!("designs/14/{cx}/{cy}.json"))).unwrap(),
        )
        .unwrap();
        assert_eq!(cell["designs"].as_array().unwrap().len(), 1);
        assert_eq!(designs.stored("0014100000000002").unwrap(), again);

        assert!(designs.remove("0014100000000002").unwrap());
        assert!(!site.join(format!("designs/14/{cx}/{cy}.json")).exists());
        assert_eq!(manifest()["cells"].as_array().unwrap().len(), 0);
        assert!(designs.stored("0014100000000002").is_none());
        // Its history is kept.
        assert_eq!(
            std::fs::read_dir(dir.path().join("data/designs/history/0014100000000002"))
                .unwrap()
                .count(),
            2
        );
    }

    #[test]
    fn a_pand_that_is_not_a_number_never_reaches_a_path() {
        assert!(valid_pand("0014100010938796"));
        assert!(!valid_pand("../../etc/passwd"));
        assert!(!valid_pand(""));
        let dir = tempfile::tempdir().unwrap();
        let designs = Designs::new(dir.path(), dir.path());
        assert!(designs.stored("../x").is_none());
        let mut d = bd::generate(
            "../evil",
            [53.2, 6.5],
            &[[0.0, 0.0], [6.0, 0.0], [6.0, 6.0], [0.0, 6.0]],
            None,
            &[],
        )
        .unwrap();
        assert!(designs.save(d.clone()).is_err());
        d.pand = "0014100010938796".into();
        assert!(designs.save(d).is_ok());
    }
}
