//! Grading measured ground so it can be driven on: carve the water, fill the
//! holes, flatten the roads and ramp the approaches up to the bridges.
//!
//! **One implementation for two callers.** The client grades its anchor-centred
//! field where atlas has not graded the ground (`terrain::bake`), and atlas grades
//! its z14 tiles once, ahead of time, for everywhere it has
//! (`crates/atlas/src/pipeline/terrain.rs`,
//! `docs/notes/level-of-detail-tile-tree.md`). Both call [`grade`], so the ground a
//! phone bakes and the ground atlas serves cannot drift apart.
//!
//! Everything here works on a **square grid of absolute metres** (NAP), row-major
//! with x fastest, whose sample `(x, z)` stands at `origin + (x, z) * step` in some
//! flat metric frame. Which frame is the caller's business — the anchored world
//! for the client, a tile's own corner for atlas — as long as the water levels,
//! the road triangles and the deck plates are handed over in the same one.

use crate::vector_tiles::{BridgeDeck, SurfaceGroup, SurfaceMesh};

/// How far below its surface a canal's bed is taken to be, in metres.
///
/// **This one number is not measured, and it is the only one here that isn't.** AHN's
/// laser does not come back off water and the BGT records the surface, so no source
/// says how deep a canal is. The ground is therefore carved to the measured surface
/// *minus this*, purely so the water sheet has something to stand its own depth rung
/// on rather than lying coplanar with the bed — the z-fight the client spends a whole
/// depth ladder avoiding.
///
/// Deliberately shallow. Being wrong by a metre of invisible depth under an opaque
/// sheet costs nothing; carving a trench deeper than the quays are tall would show.
pub const WATER_BED_M: f32 = 0.5;

/// A square grid's placement: sample `(x, z)` is at `origin + (x, z) * step`.
#[derive(Clone, Copy, Debug)]
pub struct Lattice {
    pub n: usize,
    pub origin: [f32; 2],
    pub step: f32,
}

impl Lattice {
    #[must_use]
    pub fn at(&self, x: usize, z: usize) -> [f32; 2] {
        #[allow(clippy::cast_precision_loss)]
        [
            self.origin[0] + x as f32 * self.step,
            self.origin[1] + z as f32 * self.step,
        ]
    }

    /// The sample nearest a point, if it is on the grid.
    #[must_use]
    pub fn index_of(&self, p: [f32; 2]) -> Option<(usize, usize)> {
        let fx = ((p[0] - self.origin[0]) / self.step).round();
        let fz = ((p[1] - self.origin[1]) / self.step).round();
        #[allow(clippy::cast_precision_loss)]
        let n = self.n as f32;
        (fx >= 0.0 && fz >= 0.0 && fx < n && fz < n).then(|| (fx as usize, fz as usize))
    }
}

/// Lower every sample inside a registered water body to its level less the bed.
///
/// `level_at` answers the water surface (NAP) at a point in the grid's frame.
/// `keep_measured_above` is the half-metre tier's rule and only its: a real return
/// standing above the water is a quay or a jetty. A resampled grid's finite value
/// inside a canal is the banks averaged in, and is carved like a hole. Returns how
/// many samples changed.
pub fn carve(
    grid: &mut [f32],
    lattice: Lattice,
    level_at: &dyn Fn(f32, f32) -> Option<f32>,
    keep_measured_above: bool,
) -> usize {
    let n = lattice.n;
    let mut carved = 0;
    for z in 0..n {
        for x in 0..n {
            let i = z * n + x;
            let [wx, wz] = lattice.at(x, z);
            let Some(level) = level_at(wx, wz) else {
                continue;
            };
            let bed_m = level - WATER_BED_M;
            let before = grid[i];
            grid[i] = if before.is_finite() {
                if keep_measured_above && before > level {
                    before
                } else {
                    before.min(bed_m)
                }
            } else {
                bed_m
            };
            if before != grid[i] {
                carved += 1;
            }
        }
    }
    carved
}

/// One lifted bridge deck, as the grading needs it.
pub struct DeckPlate<'a> {
    /// The outline, in its cell's own tile-local metres.
    pub deck: &'a BridgeDeck,
    /// That cell's origin in the grid's frame.
    pub world: [f32; 2],
    /// Metres above the ground at the abutment (DSM over the deck less the DTM
    /// around it).
    pub height: f32,
    /// The deck's measured top (NAP, the median of the surface over it), where the
    /// survey has one: what an approach is graded **to**.
    ///
    /// Not the ground plus [`Self::height`]: the survey's ground at an abutment is
    /// routinely the real embankment already most of the way up, and adding the
    /// clearance on top of it graded approaches 4–5 m above the deck they meet
    /// (Emmaviaduct north, 11.4–12.8 m against a 7.81 m top, 2026-09-23).
    pub top: Option<f32>,
    /// The top along the deck's length, in its cell's tile-local frame, where the
    /// survey covers it well enough (`deck_profile`). Where set it answers the top
    /// under each sample instead of [`Self::top`] — a sloped deck meets each of its
    /// approaches at its own end's height.
    pub profile: Option<&'a crate::deck_profile::DeckProfile>,
    /// The deck's own street either side of its span, tile-local, abutment first
    /// (`vector_tiles::Approach`).
    pub approaches: &'a [Vec<[f32; 2]>],
}

impl DeckPlate<'_> {
    /// The measured top over a tile-local point (NAP), if the survey has one.
    #[must_use]
    pub fn top_at(&self, local: [f32; 2]) -> Option<f32> {
        match self.profile {
            Some(p) => Some(p.at([f64::from(local[0]), f64::from(local[1])])),
            None => self.top,
        }
    }
}

/// How many samples into a deck from its abutment the ground is held at the
/// deck's top: the abutment the structure rests on. Two at atlas's 2.86 m is
/// about six metres, which the client's 3.125 m lattice cannot interpolate away
/// at the joint. Three held nine metres under a sixty-metre deck and crowded the
/// road passing beneath it, whose ground and ribbon then followed a drop steep
/// enough to part: the verge poked through the Europaweg (2026-09-27).
pub const ABUTMENT_SAMPLES: usize = 2;

/// The eight samples around `i` on an `n`-wide grid; `i` must not be on its
/// border.
fn neighbours8(i: usize, n: usize) -> [usize; 8] {
    [
        i - 1,
        i + 1,
        i - n,
        i + n,
        i - n - 1,
        i - n + 1,
        i + n - 1,
        i + n + 1,
    ]
}

/// [`deck_masks`]: where the approaches run, how high the structure stands over
/// the ground (`NaN` off every deck), and its measured top in NAP (`NaN` where
/// the survey has none).
pub struct DeckMasks {
    pub ramp: Vec<bool>,
    pub lift: Vec<f32>,
    pub top: Vec<f32>,
}

/// The two grids an embankment is built from: where the approach roads run, and
/// how high the structure they climb to stands (`NaN` off every deck).
///
/// **The approaches are a mask of their own, not part of the road mask.** A raster
/// mask is one connected shape and a gradient travels through everything touching
/// it, including the street passing *under* the viaduct; the approach is separable
/// only because it is the same street feature as the span.
#[must_use]
pub fn deck_masks(plates: &[DeckPlate<'_>], lattice: Lattice) -> DeckMasks {
    let n = lattice.n;
    let step = lattice.step;
    let mut ramp = vec![false; n * n];
    let mut lift = vec![f32::NAN; n * n];
    let mut top = vec![f32::NAN; n * n];
    if n < 3 || plates.is_empty() || step <= 0.0 {
        return DeckMasks { ramp, lift, top };
    }
    let axis = |v: f32, o: f32| -> Option<usize> {
        let k = ((v - o) / step).round();
        #[allow(clippy::cast_precision_loss)]
        (k >= 0.0 && k < n as f32).then_some(k as usize)
    };
    let [ox, oz] = lattice.origin;
    for plate in plates {
        let (lo, hi) = (plate.deck.min, plate.deck.max);
        if lo[0] > hi[0] {
            continue;
        }
        let (wx, wz) = (plate.world[0], plate.world[1]);
        // A deck reaching past the grid's edge is not rasterised: the client's
        // field tapers to nothing there, and atlas grades a window wider than the
        // tile it keeps.
        if let (Some(x0), Some(x1), Some(z0), Some(z1)) = (
            axis(wx + lo[0] - step, ox),
            axis(wx + hi[0] + step, ox),
            axis(wz + lo[1] - step, oz),
            axis(wz + hi[1] + step, oz),
        ) {
            for z in z0..=z1 {
                for x in x0..=x1 {
                    let [px, pz] = lattice.at(x, z);
                    let local = [px - wx, pz - wz];
                    if plate.deck.contains(local) {
                        let i = z * n + x;
                        // The tallest wins: at an interchange the approach climbs
                        // to the top deck.
                        if !(lift[i] >= plate.height) {
                            lift[i] = plate.height;
                            top[i] = plate.top_at(local).unwrap_or(f32::NAN);
                        }
                    }
                }
            }
        }
        // A centre line has no width and a sample is only marked when something
        // covers it, so each run is walked as a corridor one step either side. The
        // dilation in `grade` gives it the embankment's real width.
        for run in plate.approaches {
            for w in run.windows(2) {
                let a = [wx + w[0][0], wz + w[0][1]];
                let b = [wx + w[1][0], wz + w[1][1]];
                let len = ((b[0] - a[0]).powi(2) + (b[1] - a[1]).powi(2)).sqrt();
                if len < 1e-3 {
                    continue;
                }
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let steps = (len / step).ceil() as usize + 1;
                for k in 0..=steps {
                    #[allow(clippy::cast_precision_loss)]
                    let t = k as f32 / steps as f32;
                    let p = [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t];
                    let (Some(gx), Some(gz)) = (axis(p[0], ox), axis(p[1], oz)) else {
                        continue;
                    };
                    for dz in -1i64..=1 {
                        for dx in -1i64..=1 {
                            let (x, z) = (gx as i64 + dx, gz as i64 + dz);
                            if x >= 0 && z >= 0 && (x as usize) < n && (z as usize) < n {
                                ramp[z as usize * n + x as usize] = true;
                            }
                        }
                    }
                }
            }
        }
    }
    // A sample the structure stands on is not approach: the abutment is the ramp
    // sample *beside* the deck.
    for i in 0..n * n {
        if lift[i].is_finite() {
            ramp[i] = false;
        }
    }
    DeckMasks { ramp, lift, top }
}

/// How far below a road's own height the ground under it is laid, metres: the
/// road's surface is drawn at its height and the ground just under it, so the two
/// never fight for the same depth.
pub const UNDER_ROAD_M: f32 = 0.05;

/// The verge either side of a carriageway that is held at the road's height before
/// the ground starts to fall away, metres.
pub const VERGE_M: f32 = 1.0;

/// The shoulder an embankment or a cutting blends back to the measured ground
/// over, per metre it has to make up: one in two, the slope of a Dutch road
/// embankment.
pub const SHOULDER_RUN: f32 = 2.0;

/// Shortest and longest shoulder, metres.
pub const SHOULDER_MIN_M: f32 = 2.0;
pub const SHOULDER_MAX_M: f32 = 30.0;

/// One road the ground is conformed to: its densified centre line, its height
/// at each point, and half its width.
pub struct ConformRoad<'a> {
    pub points: &'a [[f32; 2]],
    pub ys: &'a [f32],
    pub half_w: f32,
}

/// Lay the ground to the roads: under each carriageway and its verge, just below
/// the road's own height; beyond, blending back to the measured ground over a
/// shoulder as wide as the height it makes up needs (`road_levels`).
///
/// **The opposite direction from [`grade`]'s**, which moved the road with the
/// ground: here the road's height is decided first, on the network, and the
/// ground follows it — so an embankment is the ground under a road that climbs,
/// and the joint with a deck is wherever the road meets it, not a raster
/// approximation of it. The nearest road decides each sample, so a junction is
/// the roads' shared height and two roads side by side each hold their own strip.
pub fn conform_to_roads(raw: &mut [f32], lattice: Lattice, roads: &[ConformRoad<'_>]) -> usize {
    let n = lattice.n;
    if n < 2 || raw.len() < n * n || lattice.step <= 0.0 {
        return 0;
    }
    // Per sample: the nearest road's (distance past its carriageway, height).
    let mut best: Vec<(f32, f32)> = vec![(f32::INFINITY, 0.0); n * n];
    let [ox, oz] = lattice.origin;
    let step = lattice.step;
    for road in roads {
        if road.points.len() != road.ys.len() {
            continue;
        }
        let reach = road.half_w + VERGE_M + SHOULDER_MAX_M;
        for (s, w) in road.points.windows(2).enumerate() {
            let (a, b) = (w[0], w[1]);
            let (ya, yb) = (road.ys[s], road.ys[s + 1]);
            let (dx, dz) = (b[0] - a[0], b[1] - a[1]);
            let len2 = dx * dx + dz * dz;
            #[allow(clippy::cast_possible_truncation)]
            let idx = |v: f32, o: f32| ((v - o) / step).floor() as i64;
            let (x0, x1) = (
                idx(a[0].min(b[0]) - reach, ox).max(0),
                idx(a[0].max(b[0]) + reach, ox).min(n as i64 - 1) + 1,
            );
            let (z0, z1) = (
                idx(a[1].min(b[1]) - reach, oz).max(0),
                idx(a[1].max(b[1]) + reach, oz).min(n as i64 - 1) + 1,
            );
            for z in z0..=z1.min(n as i64 - 1) {
                for x in x0..=x1.min(n as i64 - 1) {
                    #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
                    let i = z as usize * n + x as usize;
                    let [px, pz] = lattice.at(x as usize, z as usize);
                    let t = if len2 > 1e-9 {
                        (((px - a[0]) * dx + (pz - a[1]) * dz) / len2).clamp(0.0, 1.0)
                    } else {
                        0.0
                    };
                    let d = (px - a[0] - dx * t).hypot(pz - a[1] - dz * t) - road.half_w;
                    if d < best[i].0 {
                        best[i] = (d, ya + (yb - ya) * t);
                    }
                }
            }
        }
    }
    let mut changed = 0;
    for i in 0..n * n {
        let (d, y) = best[i];
        if !d.is_finite() {
            continue;
        }
        let road_ground = y - UNDER_ROAD_M;
        let natural = raw[i];
        let target = if d <= VERGE_M || !natural.is_finite() {
            road_ground
        } else {
            let shoulder = (SHOULDER_RUN * (natural - road_ground).abs())
                .clamp(SHOULDER_MIN_M, SHOULDER_MAX_M);
            let t = ((d - VERGE_M) / shoulder).clamp(0.0, 1.0);
            if t >= 1.0 {
                continue;
            }
            let s = t * t * (3.0 - 2.0 * t);
            road_ground + (natural - road_ground) * s
        };
        if raw[i] != target {
            raw[i] = target;
            changed += 1;
        }
    }
    changed
}

/// A cell's road-sheet triangles in its own tile-local XZ.
///
/// **The ribbons, not the tunnels.** A tunnel's carriageway is metres below the
/// surface and grading the ground down to meet it would dig a trench over every
/// bore.
#[must_use]
pub fn road_triangles(meshes: &[SurfaceMesh]) -> Vec<[[f32; 2]; 3]> {
    meshes
        .iter()
        .filter(|m| matches!(m.group, SurfaceGroup::Road(_) | SurfaceGroup::RoadCasing(_)))
        .flat_map(|m| {
            m.indices.chunks_exact(3).filter_map(move |t| {
                let q = |i: u32| m.positions.get(i as usize).map(|v| [v[0], v[2]]);
                Some([q(t[0])?, q(t[1])?, q(t[2])?])
            })
        })
        .collect()
}

/// Below this measured height a deck is laid **flat**, at the level of the roads
/// that reach it.
///
/// Not a tolerance on the measurement — the measurement is fine. A Dutch canal
/// bridge measures a metre or so above its quays (a deck does arch over the
/// water) while being level with the street it carries; lifting it built a slab
/// floating over its own approach roads. Above this, the clearance is a grade
/// separation — a viaduct over a rail yard measures 6.6 m over Groningen — and the
/// approach is ramped up to meet it.
pub const MIN_LIFT_M: f32 = 2.5;
/// Ceiling on a measured deck height, in metres. Beyond this the measurement is
/// not a bridge: it is a crane, a gantry or a stack of containers that the DSM
/// caught standing on the structure.
pub const MAX_DECK_M: f32 = 25.0;
/// How far outside a deck the ground beside it is measured.
pub const GROUND_RING_M: f64 = 30.0;

/// A deck's clearance from the median surface over it and the median ground
/// around it: `0.0` for a deck that is not lifted.
#[must_use]
pub fn deck_clearance(top: f32, ground: f32) -> f32 {
    let clearance = (top - ground).clamp(0.0, MAX_DECK_M);
    if clearance < MIN_LIFT_M {
        0.0
    } else {
        clearance
    }
}

/// The water level (NAP) at every sample of a lattice, `NaN` off the water, from
/// rings in the lattice's frame. Where rings overlap the **lowest** wins, as in the
/// client's `WaterLevelField::level_at`.
///
/// A scanline fill rather than a point test per sample: atlas grades windows of
/// ~800² samples against a few hundred rings, and even-odd crossings per row
/// visit each ring edge once per row it spans instead of once per sample.
#[must_use]
pub fn water_level_grid(rings: &[(f32, Vec<[f32; 2]>)], lattice: Lattice) -> Vec<f32> {
    let n = lattice.n;
    let mut out = vec![f32::NAN; n * n];
    let mut xs: Vec<f32> = Vec::new();
    for (level, ring) in rings {
        if ring.len() < 3 {
            continue;
        }
        let (mut lo, mut hi) = (f32::MAX, f32::MIN);
        for p in ring {
            lo = lo.min(p[1]);
            hi = hi.max(p[1]);
        }
        let z0 = (((lo - lattice.origin[1]) / lattice.step).ceil().max(0.0)) as usize;
        let z1 = ((hi - lattice.origin[1]) / lattice.step).floor();
        if z1 < 0.0 {
            continue;
        }
        let z1 = (z1 as usize).min(n.saturating_sub(1));
        for z in z0..=z1 {
            #[allow(clippy::cast_precision_loss)]
            let wz = lattice.origin[1] + z as f32 * lattice.step;
            xs.clear();
            for k in 0..ring.len() {
                let (a, b) = (ring[k], ring[(k + 1) % ring.len()]);
                // Half-open in z, the same rule as the client's `point_in_ring`.
                if (a[1] > wz) != (b[1] > wz) {
                    xs.push(a[0] + (wz - a[1]) / (b[1] - a[1]) * (b[0] - a[0]));
                }
            }
            xs.sort_by(f32::total_cmp);
            for pair in xs.chunks_exact(2) {
                let x0 = ((pair[0] - lattice.origin[0]) / lattice.step)
                    .ceil()
                    .max(0.0);
                let x1 = ((pair[1] - lattice.origin[0]) / lattice.step).floor();
                if x1 < x0 {
                    continue;
                }
                for x in (x0 as usize)..=(x1 as usize).min(n.saturating_sub(1)) {
                    let v = &mut out[z * n + x];
                    if !(*v <= *level) {
                        *v = *level;
                    }
                }
            }
        }
    }
    out
}

/// What [`grade`] did, for the logs and `--dump-state`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Graded {
    /// Samples the water carve rewrote.
    pub carved: usize,
    /// Samples pinned at a deck's abutment, and samples the embankment raised.
    pub abutments: usize,
    pub raised: usize,
}

/// Carve, fill and grade a grid of measured ground, in place.
///
/// The order is the substance: the carve goes **before** the fill, or the fill
/// closes a canal with the average of its two quays; the grading goes **after** the
/// fill, because the relaxation holds unmarked samples fixed as its boundary and
/// needs them to be heights rather than the no-data under every building; and the
/// embankments are built **before** the relaxation and pinned against it, because a
/// corridor held at the ground along both sides cannot express a gradient along it.
///
/// `road` is the carriageway only; the margin is added here, once. `ramp`, `lift`
/// and `top` are [`deck_masks`]; pass them empty where there are no decks. `road`
/// empty skips the grading altogether.
pub fn grade(
    raw: &mut [f32],
    lattice: Lattice,
    level_at: &dyn Fn(f32, f32) -> Option<f32>,
    mut road: Vec<bool>,
    mut ramp: Vec<bool>,
    lift: &[f32],
    top: &[f32],
) -> Graded {
    let n = lattice.n;
    let step = lattice.step;
    let mut out = Graded {
        carved: carve(raw, lattice, level_at, false),
        ..Graded::default()
    };
    crate::ahn::fill_holes(raw, n, Some(crate::ahn::FILL_REACH_M / step));
    if road.len() != n * n {
        return out;
    }
    crate::road_grade::dilate_mask(&mut road, n, crate::road_grade::DEFAULT_MARGIN_SAMPLES);
    let mut pins = vec![f32::NAN; n * n];
    if ramp.len() == n * n && lift.len() == n * n {
        // The approach corridor as drawn, before it is widened: what touches the
        // deck here is the abutment, where the widened one also runs along the
        // deck's sides.
        let core = ramp.clone();
        crate::road_grade::dilate_mask(&mut ramp, n, crate::road_grade::DEFAULT_MARGIN_SAMPLES);
        // A dilated approach may have grown over the structure.
        for i in 0..n * n {
            if lift[i].is_finite() {
                ramp[i] = false;
            }
        }
        // The pin is the deck's measured top beside it where the survey has one
        // (`DeckPlate::top` says why not the clearance), else the ground here plus
        // the clearance it was measured against. A road already standing higher is
        // left alone by `ramp_from_pins` — it is in a cutting.
        let near = |i: usize, grid: &[f32]| {
            [i - 1, i + 1, i - n, i + n]
                .into_iter()
                .filter_map(|k| grid.get(k).copied())
                .filter(|v| v.is_finite())
                .fold(f32::NEG_INFINITY, f32::max)
        };
        for z in 1..n - 1 {
            for x in 1..n - 1 {
                let i = z * n + x;
                if !ramp[i] || !raw[i].is_finite() {
                    continue;
                }
                let (up, at) = (near(i, lift), near(i, top));
                if at.is_finite() {
                    pins[i] = at;
                    out.abutments += 1;
                } else if up.is_finite() {
                    pins[i] = raw[i] + up;
                    out.abutments += 1;
                }
            }
        }
        // **The abutment itself, under the deck's end.** Pinned only beside the
        // deck, the ramp's last sample is at the top while the next one in —
        // under the structure — is still the ground it spans, and everything
        // between the two is drawn by interpolating them: the last metres of road
        // bent down under the deck edge, visibly, at the Europaplein
        // (2026-09-27). A bridge rests on a solid abutment there, so the deck
        // samples within [`ABUTMENT_SAMPLES`] of an abutment pin are held at the
        // deck's own top, and the ground drops away beyond them. One sample was
        // not enough: the client resamples these tiles onto its own lattice, and
        // a plateau one sample wide is interpolated away into the drop beside it,
        // which left the approach a few tenths below the deck at its joint.
        let mut band: Vec<usize> = (1..n - 1)
            .flat_map(|z| (1..n - 1).map(move |x| z * n + x))
            .filter(|&i| {
                lift[i].is_finite()
                    && neighbours8(i, n)
                        .into_iter()
                        .any(|k| core[k] && pins[k].is_finite())
            })
            .collect();
        let mut held = vec![false; n * n];
        for layer in 0..ABUTMENT_SAMPLES {
            if layer > 0 {
                band = band
                    .iter()
                    .flat_map(|&i| [i - 1, i + 1, i - n, i + n])
                    .filter(|&k| k % n != 0 && k % n != n - 1 && k / n != 0 && k / n != n - 1)
                    .filter(|&k| lift[k].is_finite() && !held[k])
                    .collect();
                band.sort_unstable();
                band.dedup();
            }
            for &i in &band {
                if held[i] {
                    continue;
                }
                held[i] = true;
                let at = match top.get(i) {
                    Some(&t) if t.is_finite() => t,
                    _ => raw[i] + lift[i],
                };
                if at.is_finite() {
                    pins[i] = at;
                    raw[i] = at;
                }
            }
        }
    }
    out.raised = crate::road_grade::ramp_from_pins(
        raw,
        n,
        &ramp,
        &mut pins,
        step,
        crate::road_grade::MAX_APPROACH_GRADE,
    );
    // Pinned, not plain: every sample the ramp raised became a pin, so the
    // relaxation smooths the rest of the corridor and leaves the grade in place.
    crate::road_grade::flatten_pinned(raw, n, &road, &pins, crate::road_grade::DEFAULT_PASSES);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lattice(n: usize) -> Lattice {
        Lattice {
            n,
            origin: [-10.0, -10.0],
            step: 1.0,
        }
    }

    /// **An approach meets the deck's measured top, not the ground plus its
    /// clearance.** Where the survey's ground at the abutment is already the
    /// embankment (5 m here), adding the 6 m clearance again would grade the road
    /// to 11 m against a 7 m deck — what the Emmaviaduct's north approach did.
    /// Without a top the clearance is still the answer.
    #[test]
    fn an_approach_is_pinned_to_the_measured_top() {
        let l = lattice(31);
        let n = l.n;
        let road: Vec<bool> = (0..n * n).map(|i| (14..=16).contains(&(i / n))).collect();
        let ramp: Vec<bool> = (0..n * n).map(|i| i / n == 15 && i % n < 20).collect();
        let deck = |i: usize| (13..=17).contains(&(i / n)) && i % n >= 20;
        let lift: Vec<f32> = (0..n * n)
            .map(|i| if deck(i) { 6.0 } else { f32::NAN })
            .collect();
        let top: Vec<f32> = (0..n * n)
            .map(|i| if deck(i) { 7.0 } else { f32::NAN })
            .collect();
        let none = |_: f32, _: f32| None;
        let at_abutment = |top: &[f32]| {
            let mut g = vec![5.0f32; n * n];
            grade(&mut g, l, &none, road.clone(), ramp.clone(), &lift, top);
            g[15 * n + 19]
        };
        let with_top = at_abutment(&top);
        assert!((with_top - 7.0).abs() < 0.3, "{with_top}");
        let without = at_abutment(&[]);
        assert!(without > 10.0, "{without}");
    }

    /// **The road meets the deck level, not bending down under its edge.** The
    /// ground under a deck is whatever it spans — here a valley at 1 m under a
    /// 7 m top — and with only the samples beside the deck pinned, the field
    /// between the last ramp sample and the first deck sample was a slope, which
    /// the road's last metres were drawn on (the Europaplein, 2026-09-27). The
    /// abutment under the deck's end holds the top; the valley starts after it.
    #[test]
    fn the_ground_under_a_decks_end_is_the_abutment() {
        let l = lattice(31);
        let n = l.n;
        // The approach runs up to the deck; a street crosses under it further in.
        let road: Vec<bool> = (0..n * n)
            .map(|i| ((14..=16).contains(&(i / n)) && i % n < 20) || (24..=26).contains(&(i % n)))
            .collect();
        let ramp: Vec<bool> = (0..n * n).map(|i| i / n == 15 && i % n < 20).collect();
        let deck = |i: usize| (13..=17).contains(&(i / n)) && i % n >= 20;
        let lift: Vec<f32> = (0..n * n)
            .map(|i| if deck(i) { 6.0 } else { f32::NAN })
            .collect();
        let top: Vec<f32> = (0..n * n)
            .map(|i| if deck(i) { 7.0 } else { f32::NAN })
            .collect();
        let none = |_: f32, _: f32| None;
        let mut g = vec![1.0f32; n * n];
        grade(&mut g, l, &none, road, ramp, &lift, &top);
        let (last_ramp, first_deck) = (g[15 * n + 19], g[15 * n + 20]);
        assert!(
            (last_ramp - 7.0).abs() < 0.3,
            "ramp at the abutment {last_ramp}"
        );
        assert!(
            (first_deck - 7.0).abs() < 1e-4,
            "the ground under the deck's end is {first_deck}, so the road bends down to it"
        );
        // The whole abutment holds, deep enough to survive a resample…
        for x in 20..20 + ABUTMENT_SAMPLES {
            let held = g[15 * n + x];
            assert!(
                (held - 7.0).abs() < 1e-4,
                "sample {x} under the deck's end is {held}"
            );
        }
        // …and then the ground falls away to what the deck spans. It is held
        // from the abutment, not from the widened corridor, which also runs
        // along the deck's sides and would bury the road passing under it.
        let past = g[15 * n + 20 + ABUTMENT_SAMPLES];
        assert!(past < 7.0, "nothing falls away past the abutment: {past}");
        let span = g[15 * n + 27];
        assert!(span < 2.5, "the ground the deck spans was raised to {span}");
    }

    /// **The ground follows the road, not the other way round.** A road standing
    /// five metres over flat ground gets the ground just under it across its
    /// carriageway and verge, an embankment falling away beside it, and the
    /// measured ground untouched further out.
    #[test]
    fn the_ground_is_laid_to_the_road() {
        let l = Lattice {
            n: 101,
            origin: [-50.0, -50.0],
            step: 1.0,
        };
        let mut g = vec![0.0f32; l.n * l.n];
        let pts = [[-50.0, 0.0], [50.0, 0.0]];
        let ys = [5.0, 5.0];
        conform_to_roads(
            &mut g,
            l,
            &[ConformRoad {
                points: &pts,
                ys: &ys,
                half_w: 4.0,
            }],
        );
        let at = |z: usize| g[(50 + z) * l.n + 50];
        assert!(
            (at(0) - (5.0 - UNDER_ROAD_M)).abs() < 1e-4,
            "under the road: {}",
            at(0)
        );
        assert!(
            (at(5) - (5.0 - UNDER_ROAD_M)).abs() < 1e-4,
            "on the verge: {}",
            at(5)
        );
        assert!(at(9) > 0.5 && at(9) < 4.9, "no slope beside it: {}", at(9));
        assert!(at(40).abs() < 1e-6, "the ground far off moved: {}", at(40));
        for z in 5..45 {
            assert!(
                at(z + 1) <= at(z) + 1e-4,
                "the embankment rises away from the road at {z}"
            );
        }
    }

    #[test]
    fn water_is_carved_before_the_holes_are_filled() {
        let l = lattice(21);
        // Quays at 2 m either side of a canal the survey has no return from.
        let mut g: Vec<f32> = (0..l.n * l.n)
            .map(|i| {
                if (8..13).contains(&(i % l.n)) {
                    f32::NAN
                } else {
                    2.0
                }
            })
            .collect();
        let level = |x: f32, _: f32| (-2.5..=2.5).contains(&x).then_some(0.0);
        let out = grade(&mut g, l, &level, Vec::new(), Vec::new(), &[], &[]);
        assert!(out.carved > 0);
        let mid = g[10 * l.n + 10];
        assert!((mid - (0.0 - WATER_BED_M)).abs() < 1e-4, "{mid}");
        assert!(g.iter().all(|v| v.is_finite()));
    }

    #[test]
    fn water_rasterises_inside_the_ring_and_the_lower_level_wins() {
        let l = lattice(21);
        let square = |a: f32, b: f32| vec![[a, a], [b, a], [b, b], [a, b]];
        let rings = vec![(1.0, square(-5.5, 5.5)), (0.5, square(-2.5, 2.5))];
        let g = water_level_grid(&rings, l);
        let at = |x: f32, z: f32| {
            let (i, j) = l.index_of([x, z]).unwrap();
            g[j * l.n + i]
        };
        assert_eq!(at(0.0, 0.0), 0.5);
        assert_eq!(at(4.0, -4.0), 1.0);
        assert!(at(8.0, 0.0).is_nan());
        assert!(at(-6.0, 0.0).is_nan());
    }

    #[test]
    fn a_low_deck_is_not_lifted() {
        assert_eq!(deck_clearance(3.0, 1.5), 0.0);
        assert_eq!(deck_clearance(9.0, 1.0), 8.0);
        assert_eq!(deck_clearance(90.0, 1.0), MAX_DECK_M);
    }

    #[test]
    fn the_lattice_finds_its_own_samples() {
        let l = lattice(21);
        assert_eq!(l.index_of(l.at(3, 17)), Some((3, 17)));
        assert_eq!(l.index_of([-30.0, 0.0]), None);
    }
}
