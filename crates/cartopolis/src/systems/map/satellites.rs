//! Satellites in orbit, drawn around the globe (Layers ▸ In orbit).
//!
//! The catalogue is CelesTrak's `active` group — every payload still reported
//! as working, about 16,000 of them in October 2026, two thirds Starlink — as
//! two-line element sets, read from **our own mirror** (atlas's
//! `/satellites/active.tle`, refreshed from CelesTrak every two hours), cached
//! for [`CATALOGUE_TTL`] and kept on a failed refresh. Not from CelesTrak
//! directly: it refuses an address that downloads too often, and every browser
//! user would share this server's (`atlas::satellites` says more). Positions are
//! computed here, not fetched: the `sgp4` crate is the standard propagator the
//! element sets are published for, in pure Rust, so the layer costs a few
//! downloads a day.
//!
//! Three things are load-bearing.
//!
//! **A snapshot, carried forward on the GPU.** Every [`REBUILD_SECS`] a worker
//! propagates the whole catalogue to one instant and writes one quad per
//! satellite carrying its position *and velocity* in the anchor's frame
//! (`build_snapshot`). The vertex stage moves each one on from that instant
//! under the Earth's gravity (`shaders/satellites.wgsl`), so they glide, and
//! nothing is uploaded between rebuilds but one uniform. Rebuilding every frame
//! would be ~2.5 MB of vertex data a frame — the upload churn
//! `docs/notes/upload-bursts.md` is about.
//!
//! **SGP4 answers in TEME, the map is Earth-fixed.** The globe here does not
//! turn; the sun moves round it. So a position is rotated about the pole by
//! Greenwich sidereal time into ECEF (`teme_to_ecef`), and its velocity loses
//! the Earth's rotation, or every satellite drifts westward at 465 m/s at the
//! equator between rebuilds. Polar motion and the TEME/true-of-date nicety are
//! left out: both are metres, and a point here is kilometres across.
//!
//! **Shown only from space**, i.e. above `GLOBE_SHOW_ALT`. From the ground the
//! far plane is 200 km and the scene is a city; a geostationary satellite is
//! 36,000 km up. While the layer is on, the far plane reaches past the
//! geostationary belt ([`SatelliteLayer::far_reach_m`]).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bevy::asset::RenderAssetUsages;
use bevy::light::{NotShadowCaster, NotShadowReceiver};
use bevy::math::DVec3;
use bevy::prelude::*;
use bevy::render::mesh::{Indices, PrimitiveTopology};
use bevy::tasks::IoTaskPool;
use big_space::prelude::{BigSpace, GridCell};
use cartopolis_core::geo::TangentFrame;
use web_time::{Instant, SystemTime, UNIX_EPOCH};

use crate::components::FreeCamera;
use crate::grid::AnchoredCamera;
use crate::platform::{http, storage};
use crate::systems::camera_attach::{AttachKey, AttachRange, CameraAttach};
use crate::systems::data_layers::DataLayers;
use crate::systems::egui_manager::EguiManager;
use crate::systems::globe::{GlobeState, GLOBE_SHOW_ALT};
use crate::systems::{icons, theme};

pub use cartopolis_materials::satellites::SatelliteMaterial;

/// Atlas's mirror of CelesTrak's `active` group, as three-line element sets (a
/// name line, then the two element lines): 2.7 MB of text, about 0.9 MB as Caddy
/// sends it compressed. The browser build asks its own origin, which is the same
/// server.
const CATALOGUE_URL: &str = "https://cartopolis.org/satellites/active.tle";
const CATALOGUE_URL_WEB: &str = "/satellites/active.tle";

/// Where the catalogue is kept between runs (`platform::storage`'s cache).
const CATALOGUE_KEY: &str = "satellites/active.tle";

/// How old the cached catalogue may be before it is fetched again. The mirror
/// refreshes every two hours; at six a low orbit is still only a few kilometres
/// from the truth, which is under a pixel from space.
const CATALOGUE_TTL: Duration = Duration::from_secs(6 * 3600);

/// How long a failed fetch waits before it is tried again.
const RETRY_AFTER: Duration = Duration::from_secs(300);

/// How often the snapshot is rebuilt, seconds. The shader's gravity step drifts
/// from SGP4 by about the Coriolis term, ½·2ωv·t² — 50 m for the space station
/// after ten seconds.
const REBUILD_SECS: f64 = 10.0;

/// The Earth's gravitational parameter, m³/s² (WGS-84).
const GM: f32 = 3.986_004_418e14;

/// The Earth's rotation rate, rad/s.
const EARTH_RATE: f64 = 7.292_115_9e-5;

/// Radius of the geostationary belt from the Earth's centre, metres, plus room
/// for the graveyard orbits a few hundred kilometres above it.
const GEO_REACH_M: f32 = 43_000_000.0;

/// Overall brightness of a point, as a display value.
const BRIGHTNESS: f32 = 1.6;

/// The altitude band the layer fades in over, above `GLOBE_SHOW_ALT`.
const FADE_IN_M: f32 = 100_000.0;

/// How far from a satellite's point a tap still selects it, window points.
const PICK_RADIUS_PTS: f32 = 14.0;

/// How long and how far a press may last and move and still be a tap.
const TAP_MAX_SECS: f32 = 0.35;
const TAP_SLOP_PTS: f32 = 6.0;

/// Points along the selected satellite's drawn path.
const TRACK_POINTS: usize = 240;

/// What a satellite is for, by its catalogue name — the colour it is drawn in
/// and the switch that hides it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SatKind {
    Station,
    Navigation,
    Observation,
    Starlink,
    Other,
}

/// One row per [`SatKind`], in its declaration order: the persisted key, the
/// panel's label, the linear colour and the point's diameter in pixels.
pub struct KindDef {
    pub key: &'static str,
    pub label: &'static str,
    pub color: [f32; 3],
    pub size_px: f32,
}

pub const KINDS: [KindDef; 5] = [
    KindDef {
        key: "station",
        label: "Crewed stations",
        color: [1.0, 0.82, 0.45],
        size_px: 9.0,
    },
    KindDef {
        key: "navigation",
        label: "Navigation (GPS, Galileo…)",
        color: [0.35, 1.0, 0.45],
        size_px: 5.0,
    },
    KindDef {
        key: "observation",
        label: "Weather & Earth observation",
        color: [1.0, 0.5, 0.18],
        size_px: 5.0,
    },
    KindDef {
        key: "starlink",
        label: "Starlink",
        color: [0.33, 0.45, 0.75],
        size_px: 3.0,
    },
    KindDef {
        key: "other",
        label: "Everything else",
        color: [0.75, 0.75, 0.8],
        size_px: 4.0,
    },
];

impl SatKind {
    fn def(self) -> &'static KindDef {
        &KINDS[self as usize]
    }
}

/// Sort a satellite into a [`SatKind`] by its catalogue name.
///
/// Names are the only classification CelesTrak's `active` group carries, and
/// they are consistent enough within a programme to be relied on: every GPS
/// satellite is `NAVSTAR …` or `GPS …`, every Galileo `GSAT0…`. What this gets
/// wrong goes to `Other`, which is drawn, just in grey.
#[must_use]
pub fn classify(name: &str) -> SatKind {
    let n = name.trim().to_ascii_uppercase();
    let starts = |p: &str| n.starts_with(p);
    let has = |p: &str| n.contains(p);
    if starts("STARLINK") {
        SatKind::Starlink
    } else if starts("ISS ")
        || starts("ISS(")
        || starts("CSS ")
        || starts("CSS(")
        || has("SOYUZ-MS")
        || has("PROGRESS-MS")
        || has("SHENZHOU")
        || has("TIANZHOU")
        || has("DRAGON")
    {
        SatKind::Station
    } else if starts("NAVSTAR")
        || starts("GPS ")
        || starts("GSAT0")
        || starts("GALILEO")
        || starts("BEIDOU")
        || has("GLONASS")
        || starts("QZS")
        || starts("IRNSS")
        || starts("NVS-")
    {
        SatKind::Navigation
    } else if [
        "NOAA",
        "GOES",
        "METEOSAT",
        "METOP",
        "HIMAWARI",
        "FENGYUN",
        "FY-",
        "SENTINEL",
        "LANDSAT",
        "TERRA",
        "AQUA",
        "SUOMI",
        "JPSS",
        "ELEKTRO",
        "METEOR",
        "GOSAT",
        "WORLDVIEW",
        "PLEIADES",
        "SPOT ",
        "GCOM",
        "DMSP",
        "INSAT-3D",
        "GEO-KOMPSAT",
        "SWOT",
        "CRYOSAT",
        "SMAP",
        "ICESAT",
    ]
    .iter()
    .any(|p| starts(p))
    {
        SatKind::Observation
    } else {
        SatKind::Other
    }
}

/// One satellite of the catalogue, ready to propagate.
pub struct Satellite {
    pub name: String,
    pub norad: u64,
    pub kind: SatKind,
    /// The element set's epoch, Unix seconds.
    epoch_unix: f64,
    /// Revolutions per day, from the element set.
    revs_per_day: f64,
    constants: sgp4::Constants,
}

impl Satellite {
    /// Position (m) and velocity (m/s) in ECEF at `unix` seconds, or `None` when
    /// SGP4 refuses — a decayed orbit, or an element set too stale to trust.
    #[must_use]
    pub fn ecef_at(&self, unix: f64) -> Option<(DVec3, DVec3)> {
        let minutes = (unix - self.epoch_unix) / 60.0;
        let p = self
            .constants
            .propagate(sgp4::MinutesSinceEpoch(minutes))
            .ok()?;
        let r = DVec3::from_array(p.position) * 1000.0;
        let v = DVec3::from_array(p.velocity) * 1000.0;
        if !r.is_finite() || !v.is_finite() {
            return None;
        }
        Some(teme_to_ecef(r, v, gmst(unix)))
    }
}

/// Read a three-line element-set file. A record that does not parse is
/// skipped, not fatal: one malformed line in 16,000 must not empty the sky.
#[must_use]
pub fn parse_catalogue(text: &str) -> Vec<Satellite> {
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim_end)
        .filter(|l| !l.trim().is_empty())
        .collect();
    let mut out = Vec::with_capacity(lines.len() / 3);
    let mut i = 0;
    while i + 2 < lines.len() {
        let (name, l1, l2) = (lines[i], lines[i + 1], lines[i + 2]);
        if !(l1.starts_with("1 ") && l2.starts_with("2 ")) {
            // Out of step: move on one line and look for the next record.
            i += 1;
            continue;
        }
        i += 3;
        let name = name.trim().trim_start_matches("0 ").to_string();
        let Ok(elements) =
            sgp4::Elements::from_tle(Some(name.clone()), l1.as_bytes(), l2.as_bytes())
        else {
            continue;
        };
        let Ok(constants) = sgp4::Constants::from_elements(&elements) else {
            continue;
        };
        let epoch_unix = elements.datetime.and_utc().timestamp_micros() as f64 / 1e6;
        out.push(Satellite {
            kind: classify(&name),
            name,
            norad: elements.norad_id,
            epoch_unix,
            revs_per_day: elements.mean_motion,
            constants,
        });
    }
    out
}

/// Greenwich mean sidereal time at `unix` seconds, radians (the IAU 1982
/// expression SGP4's own initialisation uses).
#[must_use]
pub fn gmst(unix: f64) -> f64 {
    let julian_years = (unix / 86_400.0 + 2_440_587.5 - 2_451_545.0) / 365.25;
    sgp4::iau_epoch_to_sidereal_time(julian_years)
}

/// TEME position and velocity → Earth-fixed, by a rotation of `gmst` about the
/// pole (`theta`, Greenwich sidereal time), with the frame's own rotation
/// taken out of the velocity.
#[must_use]
pub fn teme_to_ecef(r: DVec3, v: DVec3, theta: f64) -> (DVec3, DVec3) {
    let (s, c) = theta.sin_cos();
    let rot = |a: DVec3| DVec3::new(c * a.x + s * a.y, -s * a.x + c * a.y, a.z);
    let r_ecef = rot(r);
    let omega = DVec3::new(0.0, 0.0, EARTH_RATE);
    let v_ecef = rot(v) - omega.cross(r_ecef);
    (r_ecef, v_ecef)
}

/// ECEF → the anchor's Bevy-local frame (x east, y up, z south), as
/// `globe::geodetic_to_local` places everything else on the planet.
fn ecef_to_local(frame: &TangentFrame, p: DVec3) -> Vec3 {
    let enu = frame.world_to_local(p);
    Vec3::new(enu.x as f32, enu.z as f32, -enu.y as f32)
}

/// An ECEF *direction* (a velocity) in the same frame.
fn ecef_dir_to_local(frame: &TangentFrame, d: DVec3) -> Vec3 {
    Vec3::new(
        d.dot(frame.east) as f32,
        d.dot(frame.up) as f32,
        -d.dot(frame.north) as f32,
    )
}

/// The current time, Unix seconds.
fn now_unix() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}

/// One satellite in a snapshot: which catalogue entry, and where it was.
#[derive(Clone, Copy)]
struct Placed {
    index: u32,
    position: Vec3,
    velocity: Vec3,
}

/// A worker's answer: the mesh attributes for every shown satellite at `unix`,
/// in the frame of globe build `revision`.
struct Snapshot {
    unix: f64,
    revision: u32,
    mask: u8,
    positions: Vec<[f32; 3]>,
    velocities: Vec<[f32; 3]>,
    uvs: Vec<[f32; 2]>,
    colors: Vec<[f32; 4]>,
    indices: Vec<u32>,
    placed: Vec<Placed>,
}

/// Propagate the whole catalogue to `unix` and lay out the quads. Pure CPU work
/// for a worker: ~16,000 SGP4 calls.
fn build_snapshot(
    catalogue: &[Satellite],
    unix: f64,
    anchor: (f64, f64),
    revision: u32,
    mask: u8,
) -> Snapshot {
    let frame = TangentFrame::new(anchor.0, anchor.1, 0.0);
    let mut s = Snapshot {
        unix,
        revision,
        mask,
        positions: Vec::new(),
        velocities: Vec::new(),
        uvs: Vec::new(),
        colors: Vec::new(),
        indices: Vec::new(),
        placed: Vec::new(),
    };
    for (i, sat) in catalogue.iter().enumerate() {
        if mask & (1 << sat.kind as u8) == 0 {
            continue;
        }
        let Some((r, v)) = sat.ecef_at(unix) else {
            continue;
        };
        let p = ecef_to_local(&frame, r);
        let vel = ecef_dir_to_local(&frame, v);
        let def = sat.kind.def();
        let base = s.positions.len() as u32;
        for corner in [[-1.0, -1.0], [1.0, -1.0], [1.0, 1.0], [-1.0, 1.0]] {
            s.positions.push(p.to_array());
            s.velocities.push(vel.to_array());
            s.uvs.push(corner);
            s.colors
                .push([def.color[0], def.color[1], def.color[2], def.size_px]);
        }
        s.indices
            .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
        s.placed.push(Placed {
            index: i as u32,
            position: p,
            velocity: vel,
        });
    }
    s
}

fn snapshot_mesh(s: Snapshot) -> (Mesh, Vec<Placed>) {
    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::RENDER_WORLD,
    );
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, s.positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, s.velocities);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, s.uvs);
    mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, s.colors);
    mesh.insert_indices(Indices::U32(s.indices));
    (mesh, s.placed)
}

/// What the workers hand back.
enum Inbox {
    Catalogue(Option<Arc<Vec<Satellite>>>),
    Snapshot(Snapshot),
}

/// Marker for the one entity that draws every satellite.
#[derive(Component)]
pub struct SatelliteCloud;

#[derive(Resource, Default)]
pub struct SatelliteLayer {
    catalogue: Option<Arc<Vec<Satellite>>>,
    /// When the catalogue in hand was fetched (or read), so it is refreshed
    /// after [`CATALOGUE_TTL`] in a long session.
    loaded_at: Option<Instant>,
    loading: bool,
    failed_at: Option<Instant>,
    building: bool,
    inbox: Arc<Mutex<Vec<Inbox>>>,
    entity: Option<Entity>,
    material: Option<Handle<SatelliteMaterial>>,
    /// The snapshot being drawn: its instant, frame, filter and contents.
    shown_unix: f64,
    shown_revision: u32,
    shown_mask: u8,
    placed: Vec<Placed>,
    /// Whether the layer is on — what [`Self::far_reach_m`] answers from.
    active: bool,
    /// The satellite a tap picked, by catalogue index.
    pub selected: Option<usize>,
    /// Where the tap that picked it landed, window points: the menu opens there.
    /// `None` once the menu is closed, while the selection (and its drawn path)
    /// can stay.
    menu_at: Option<Vec2>,
    /// Whether the selected satellite's path for the next orbit is drawn. On when
    /// one is picked; the ring's Path flips it.
    show_path: bool,
    /// Whether the full card (the ring's More) is up.
    card_open: bool,
    /// Where the left button went down, for telling a tap from a drag.
    press: Option<(Vec2, f32)>,
}

impl SatelliteLayer {
    /// How far from the Earth's centre the far plane must reach for this layer,
    /// metres — 0 while it is off.
    #[must_use]
    pub fn far_reach_m(&self) -> f32 {
        if self.active {
            GEO_REACH_M
        } else {
            0.0
        }
    }

    /// Satellites in the snapshot being drawn — `satellites_drawn` in
    /// `--dump-state`.
    #[must_use]
    pub fn drawn(&self) -> usize {
        if self.active {
            self.placed.len()
        } else {
            0
        }
    }

    /// The catalogue's size, or 0 before it has arrived.
    #[must_use]
    pub fn catalogued(&self) -> usize {
        self.catalogue.as_ref().map_or(0, |c| c.len())
    }

    fn selected_satellite(&self) -> Option<&Satellite> {
        self.catalogue.as_ref()?.get(self.selected?)
    }
}

impl crate::systems::stream_watch::StreamBusy for SatelliteLayer {
    const NAME: &'static str = "satellites";
    fn stream_busy(&self) -> bool {
        // Busy until the first snapshot is up, so a capture of the layer does
        // not fire at an empty sky. A failed download is not busy: it is an
        // answer, and the capture should say so with `satellites_drawn == 0`.
        self.active
            && (self.loading
                || self.building
                || (self.catalogue.is_some() && self.placed.is_empty()))
    }
}

/// The kinds the panel has switched on, as a bit per [`SatKind`].
fn kind_mask(dl: &DataLayers) -> u8 {
    dl.satellite_kinds
        .iter()
        .enumerate()
        .filter(|(_, on)| **on)
        .fold(0, |m, (i, _)| m | (1 << i))
}

/// The URL the catalogue is fetched from: our mirror, or `CARTO_SATELLITES_URL`.
fn catalogue_url() -> String {
    crate::env_value("CARTO_SATELLITES_URL").unwrap_or_else(|| {
        if cfg!(target_arch = "wasm32") {
            CATALOGUE_URL_WEB.to_string()
        } else {
            CATALOGUE_URL.to_string()
        }
    })
}

/// The catalogue text: the cache while it is fresh, else a fetch (cached on
/// success), else whatever stale copy there is.
async fn load_catalogue_text() -> Option<String> {
    let fresh = storage::age_secs(storage::Namespace::Cache, CATALOGUE_KEY)
        .await
        .is_some_and(|age| age < CATALOGUE_TTL.as_secs_f64());
    if fresh {
        if let Some(text) = storage::get_string(storage::Namespace::Cache, CATALOGUE_KEY).await {
            return Some(text);
        }
    }
    let url = catalogue_url();
    match http::fetch_string(http::Request::get(&url).timeout(Duration::from_secs(60))).await {
        Ok(text) if text.contains("\n1 ") => {
            storage::put(storage::Namespace::Cache, CATALOGUE_KEY, text.as_bytes()).await;
            Some(text)
        }
        Ok(_) => {
            tracing::warn!(%url, "satellite catalogue: answer is not element sets");
            storage::get_string(storage::Namespace::Cache, CATALOGUE_KEY).await
        }
        Err(e) => {
            tracing::warn!(%url, error = %e, "satellite catalogue fetch failed");
            storage::get_string(storage::Namespace::Cache, CATALOGUE_KEY).await
        }
    }
}

/// Fetch the catalogue, rebuild the snapshot when it is due, and swap a
/// finished one in.
#[allow(clippy::too_many_arguments)]
pub fn drive_satellites(
    mut commands: Commands,
    mut layer: ResMut<SatelliteLayer>,
    dl: Res<DataLayers>,
    globe: Res<GlobeState>,
    camera: AnchoredCamera,
    bigspace: Query<Entity, With<BigSpace>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<SatelliteMaterial>>,
    mut attach: ResMut<CameraAttach>,
) {
    let layer = layer.as_mut();
    layer.active = dl.satellites_enabled;

    // Drain what the workers finished.
    let arrived: Vec<Inbox> = layer
        .inbox
        .lock()
        .map(|mut g| std::mem::take(&mut *g))
        .unwrap_or_default();
    for msg in arrived {
        match msg {
            Inbox::Catalogue(cat) => {
                layer.loading = false;
                match cat {
                    Some(cat) if !cat.is_empty() => {
                        tracing::info!(satellites = cat.len(), "satellite catalogue loaded");
                        // `CARTO_FOLLOW_SATELLITE=<NORAD>`: ride one from the start — how a
                        // headless capture checks the camera attachment, which no
                        // script can otherwise tap its way into.
                        if let Some(sat) = crate::env_value("CARTO_FOLLOW_SATELLITE")
                            .and_then(|v| v.trim().parse::<u64>().ok())
                            .and_then(|id| cat.iter().find(|s| s.norad == id))
                        {
                            attach.attach(attach_key(sat), sat.name.clone(), ATTACH_RANGE);
                            layer.selected = cat.iter().position(|s| s.norad == sat.norad);
                            layer.show_path = true;
                        }
                        layer.catalogue = Some(cat);
                        layer.loaded_at = Some(Instant::now());
                        layer.failed_at = None;
                        // Indices into the old catalogue mean nothing now.
                        layer.selected = None;
                        layer.shown_unix = 0.0;
                    }
                    _ => layer.failed_at = Some(Instant::now()),
                }
            }
            Inbox::Snapshot(snap) => {
                layer.building = false;
                // Built for a frame the globe has since left: drop it, the next
                // rebuild is due at once.
                if snap.revision != globe.build_revision() {
                    layer.shown_unix = 0.0;
                    continue;
                }
                let (unix, revision, mask) = (snap.unix, snap.revision, snap.mask);
                let (mesh, placed) = snapshot_mesh(snap);
                layer.shown_unix = unix;
                layer.shown_revision = revision;
                layer.shown_mask = mask;
                // Nothing to draw (every kind switched off): no entity, rather than
                // one holding an empty mesh — the render world's mesh allocator
                // has no business with a zero-length vertex buffer.
                if placed.is_empty() {
                    if let Some(e) = layer.entity.take() {
                        commands.entity(e).try_despawn();
                    }
                    layer.placed.clear();
                    continue;
                }
                layer.placed = placed;
                let mesh = Mesh3d(meshes.add(mesh));
                match layer.entity {
                    // The old mesh goes with the handle it is replaced by; there is
                    // only ever one, so this is one free and one upload per rebuild.
                    Some(entity) => {
                        commands.entity(entity).try_insert(mesh);
                    }
                    None => {
                        let Ok(root) = bigspace.single() else {
                            continue;
                        };
                        let material = layer
                            .material
                            .get_or_insert_with(|| {
                                materials.add(SatelliteMaterial {
                                    motion: Vec4::ZERO,
                                    params: Vec4::new(GM, 0.0, 1.0, 0.0),
                                })
                            })
                            .clone();
                        layer.entity = Some(spawn_cloud(&mut commands, root, mesh, material));
                    }
                }
            }
        }
    }

    if !layer.active {
        if let Some(e) = layer.entity.take() {
            commands.entity(e).try_despawn();
        }
        if let Some(m) = layer.material.take() {
            materials.remove(&m);
        }
        layer.placed.clear();
        layer.selected = None;
        layer.shown_unix = 0.0;
        return;
    }

    // The catalogue: once, then again when it goes stale.
    let stale = layer.loaded_at.is_none_or(|t| t.elapsed() >= CATALOGUE_TTL);
    let backing_off = layer.failed_at.is_some_and(|t| t.elapsed() < RETRY_AFTER);
    if stale && !layer.loading && !backing_off {
        layer.loading = true;
        let inbox = layer.inbox.clone();
        IoTaskPool::get()
            .spawn(async move {
                let text = load_catalogue_text().await;
                crate::platform::workers::submit_ranked(
                    "satellites",
                    crate::platform::workers::FAR,
                    Box::new(move || {
                        let cat = text.map(|t| Arc::new(parse_catalogue(&t)));
                        if let Ok(mut g) = inbox.lock() {
                            g.push(Inbox::Catalogue(cat));
                        }
                    }),
                );
            })
            .detach();
    }

    if !globe.ready {
        return;
    }
    let in_space = camera.get().is_some_and(|(c, t)| globe.in_space(c, t));

    // A rebuild: due by the clock, or because the frame or the filter moved.
    let Some(catalogue) = layer.catalogue.clone() else {
        return;
    };
    let mask = kind_mask(&dl);
    let now = now_unix();
    let due = now - layer.shown_unix >= REBUILD_SECS
        || layer.shown_revision != globe.build_revision()
        || layer.shown_mask != mask;
    if !in_space || layer.building || !due {
        return;
    }
    layer.building = true;
    let inbox = layer.inbox.clone();
    let anchor = (globe.anchor_lat, globe.anchor_lng);
    let revision = globe.build_revision();
    // A little ahead, so the snapshot describes the moment it lands rather than
    // the moment it was asked for.
    let at = now + 0.5;
    crate::platform::workers::submit_ranked(
        "satellites",
        crate::platform::workers::FAR,
        Box::new(move || {
            let snap = build_snapshot(&catalogue, at, anchor, revision, mask);
            if let Ok(mut g) = inbox.lock() {
                g.push(Inbox::Snapshot(snap));
            }
        }),
    );
}

/// The one entity that draws every satellite. Kept while the layer is on and
/// there is something to draw; hidden below the space tier rather than torn down.
fn spawn_cloud(
    commands: &mut Commands,
    root: Entity,
    mesh: Mesh3d,
    material: Handle<SatelliteMaterial>,
) -> Entity {
    commands
        .spawn((
            mesh,
            MeshMaterial3d(material),
            Transform::default(),
            GridCell::default(),
            ChildOf(root),
            // The points are moved in the vertex stage and span the whole
            // geostationary belt, so the mesh's own bounds mean nothing; and
            // opting out of culling owes the shadow decision with it.
            bevy::camera::visibility::NoFrustumCulling,
            NotShadowCaster,
            NotShadowReceiver,
            Visibility::Hidden,
            SatelliteCloud,
        ))
        .id()
}

/// Each frame: how far the shader carries the snapshot, how bright, and whether
/// the layer shows at all.
pub fn update_satellite_material(
    layer: Res<SatelliteLayer>,
    globe: Res<GlobeState>,
    camera: AnchoredCamera,
    windows: Query<&Window>,
    mut materials: ResMut<Assets<SatelliteMaterial>>,
    mut clouds: Query<&mut Visibility, With<SatelliteCloud>>,
) {
    let (Some(handle), Ok(mut vis)) = (layer.material.as_ref(), clouds.single_mut()) else {
        return;
    };
    let altitude = camera
        .get()
        .map_or(0.0, |(c, t)| globe.altitude_m(c, t) as f32);
    // A snapshot from before a re-anchor is in the old frame: hidden for the
    // moment until its replacement lands, rather than drawn displaced.
    let shown = globe.ready
        && altitude > GLOBE_SHOW_ALT
        && !layer.placed.is_empty()
        && layer.shown_revision == globe.build_revision();
    let want = if shown {
        Visibility::Inherited
    } else {
        Visibility::Hidden
    };
    if *vis != want {
        *vis = want;
    }
    if !shown {
        return;
    }
    let fade = crate::utils::sun::smoothstep(GLOBE_SHOW_ALT, GLOBE_SHOW_ALT + FADE_IN_M, altitude);
    let scale = windows
        .iter()
        .next()
        .map_or(1.0, |w| w.resolution.scale_factor());
    if let Some(mut mat) = materials.get_mut(handle) {
        mat.motion = globe
            .center_local
            .extend((now_unix() - layer.shown_unix) as f32);
        mat.params = Vec4::new(GM, BRIGHTNESS * fade, scale, 0.0);
    }
}

/// A tap on a satellite selects it; a tap anywhere else in space clears the
/// selection. A tap is a press and release close together in time and place,
/// so the drag that turns the globe never selects anything.
#[allow(clippy::too_many_arguments)]
pub fn pick_satellite(
    mut layer: ResMut<SatelliteLayer>,
    mouse: Res<ButtonInput<MouseButton>>,
    touch: Res<crate::systems::touch::TouchGestures>,
    time: Res<Time>,
    egui: Res<EguiManager>,
    windows: Query<&Window>,
    cameras: Query<(&Camera, &GlobalTransform), With<FreeCamera>>,
    clouds: Query<(&GlobalTransform, &Visibility), With<SatelliteCloud>>,
    globe: Res<GlobeState>,
    attach: Res<CameraAttach>,
) {
    let cursor = windows.iter().next().and_then(|w| w.cursor_position());
    if mouse.just_pressed(MouseButton::Left) {
        layer.press = cursor
            .filter(|_| !egui.owns_click(touch.touch_click(), cursor))
            .map(|c| (c, time.elapsed_secs()));
    }
    if !mouse.just_released(MouseButton::Left) {
        return;
    }
    let Some((at, when)) = layer.press.take() else {
        return;
    };
    let Some(cursor) = cursor else {
        return;
    };
    if touch.pair()
        || time.elapsed_secs() - when > TAP_MAX_SECS
        || cursor.distance(at) > TAP_SLOP_PTS
    {
        return;
    }
    let (Ok((camera, cam_gt)), Ok((cloud_gt, vis))) = (cameras.single(), clouds.single()) else {
        return;
    };
    if *vis == Visibility::Hidden {
        return;
    }
    let dt = (now_unix() - layer.shown_unix) as f32;
    let cam = cam_gt.translation();
    let centre = cloud_gt.transform_point(globe.center_local);
    let mut best: Option<(f32, usize)> = None;
    for p in &layer.placed {
        let world = cloud_gt.transform_point(p.position + p.velocity * dt);
        if behind_the_planet(cam, world, centre, globe.radius) {
            continue;
        }
        let Ok(screen) = camera.world_to_viewport(cam_gt, world) else {
            continue;
        };
        let d = screen.distance(cursor);
        if d <= PICK_RADIUS_PTS && best.is_none_or(|(bd, _)| d < bd) {
            best = Some((d, p.index as usize));
        }
    }
    match best {
        Some((_, i)) => {
            layer.selected = Some(i);
            layer.menu_at = Some(cursor);
            layer.show_path = true;
            layer.card_open = false;
        }
        None => {
            layer.menu_at = None;
            layer.card_open = false;
            // The satellite the camera rides keeps its path drawn.
            if !attach.key().is_some_and(|k| k.source == ATTACH_SOURCE) {
                layer.selected = None;
            }
        }
    }
}

/// Does the planet stand between the camera and `point`?
fn behind_the_planet(cam: Vec3, point: Vec3, centre: Vec3, radius: f32) -> bool {
    let d = point - cam;
    let len = d.length();
    if len <= 0.0 {
        return false;
    }
    let dir = d / len;
    let t = (centre - cam).dot(dir).clamp(0.0, len);
    (cam + dir * t).distance(centre) < radius
}

/// The selected satellite's path over the next orbit, as a line in the Earth's
/// frame — a ground-fixed path, so it does not close on itself: the planet has
/// turned under it by the time it comes round.
pub fn draw_selected_track(
    layer: Res<SatelliteLayer>,
    globe: Res<GlobeState>,
    clouds: Query<(&GlobalTransform, &Visibility), With<SatelliteCloud>>,
    mut gizmos: Gizmos,
) {
    let (Some(sat), Ok((gt, vis))) = (layer.selected_satellite(), clouds.single()) else {
        return;
    };
    if *vis == Visibility::Hidden || !globe.ready || !layer.show_path {
        return;
    }
    let frame = TangentFrame::new(globe.anchor_lat, globe.anchor_lng, 0.0);
    let period_s = period_minutes(sat) * 60.0;
    let now = now_unix();
    let def = sat.kind.def();
    let color = Color::linear_rgb(def.color[0], def.color[1], def.color[2]);
    let points = (0..=TRACK_POINTS).filter_map(|i| {
        let t = now + period_s * i as f64 / TRACK_POINTS as f64;
        let (r, _) = sat.ecef_at(t)?;
        Some(gt.transform_point(ecef_to_local(&frame, r)))
    });
    gizmos.linestrip(points, color);
}

/// The orbital period, minutes, from the mean motion the elements carry.
fn period_minutes(sat: &Satellite) -> f64 {
    if sat.revs_per_day > 0.0 {
        1440.0 / sat.revs_per_day
    } else {
        90.0
    }
}

/// The camera-attachment owner name for satellites; the id is the NORAD number.
pub const ATTACH_SOURCE: &str = "satellite";

/// Where the camera settles around a satellite, and how near and far it may go:
/// close enough that the satellite is the subject with the planet behind it,
/// out to past the geostationary belt.
const ATTACH_RANGE: AttachRange = AttachRange {
    start_m: 20_000.0,
    min_m: 200.0,
    max_m: 40_000_000.0,
};

fn attach_key(sat: &Satellite) -> AttachKey {
    AttachKey {
        source: ATTACH_SOURCE,
        id: sat.norad,
    }
}

/// While the camera rides a satellite, tell it where the satellite is — the
/// exact SGP4 answer for this instant, not the shader's step, so the camera
/// and the point agree to within metres. Lets go when the layer goes off.
pub fn report_attached_satellite(
    layer: Res<SatelliteLayer>,
    dl: Res<DataLayers>,
    globe: Res<GlobeState>,
    mut attach: ResMut<CameraAttach>,
) {
    let Some(key) = attach.key().filter(|k| k.source == ATTACH_SOURCE) else {
        return;
    };
    if !dl.satellites_enabled {
        attach.detach_if(key);
        return;
    }
    if !globe.ready {
        return;
    }
    let Some(sat) = layer
        .catalogue
        .as_ref()
        .and_then(|c| c.iter().find(|s| s.norad == key.id))
    else {
        return;
    };
    let Some((r, _)) = sat.ecef_at(now_unix()) else {
        // Decayed or past what its elements can say: nothing left to follow.
        attach.detach_if(key);
        return;
    };
    let frame = TangentFrame::new(globe.anchor_lat, globe.anchor_lng, 0.0);
    let p = ecef_to_local(&frame, r).as_dvec3();
    attach.report(key, p, p - globe.center_local.as_dvec3());
}

/// What a tap on a satellite opens: the ring of actions round it
/// (`systems::action_ring`) — Follow, Path, More — and, from More, the card with
/// everything known about it.
pub fn render_satellite_menu(
    egui: Res<EguiManager>,
    mut layer: ResMut<SatelliteLayer>,
    mut attach: ResMut<CameraAttach>,
    clouds: Query<&Visibility, With<SatelliteCloud>>,
) {
    use crate::systems::action_ring::{self, RingAction, RingOutcome};

    let hidden = clouds.single().map_or(true, |v| *v == Visibility::Hidden);
    if layer.selected_satellite().is_none() || hidden {
        layer.menu_at = None;
        layer.card_open = false;
        return;
    }
    let Some(sat) = layer.selected_satellite() else {
        return;
    };
    let Some((r, v)) = sat.ecef_at(now_unix()) else {
        return;
    };
    let (lat, lng, height) = cartopolis_core::ecef_to_geodetic(r);
    let def = sat.kind.def();
    let name = sat.name.clone();
    let norad = sat.norad;
    let key = attach_key(sat);
    let period = period_minutes(sat);
    let speed_kms = v.length() / 1000.0;
    let riding = attach.is(key);

    if let Some(at) = layer.menu_at {
        let actions = [
            if riding {
                RingAction {
                    key: "follow",
                    icon: icons::FOLLOW,
                    label: "Let go",
                    hint: "Stop following this satellite",
                }
            } else {
                RingAction {
                    key: "follow",
                    icon: icons::FOLLOW,
                    label: "Follow",
                    hint: "Ride along with this satellite",
                }
            },
            RingAction {
                key: "path",
                icon: icons::ORBIT_PATH,
                label: "Path",
                hint: "Show or hide its path for the next orbit",
            },
            RingAction {
                key: "more",
                icon: icons::MORE,
                label: "More",
                hint: "Everything known about it",
            },
        ];
        let detail = format!(
            "{} · {:.0} km · {speed_kms:.2} km/s",
            def.label,
            height / 1000.0
        );
        let id = egui::Id::new("satellite_ring").with((norad, at.x.to_bits(), at.y.to_bits()));
        let ui_at = egui.window_to_ui(at);
        match action_ring::show(egui.ctx(), id, ui_at, &name, &detail, &actions) {
            RingOutcome::Open => {}
            RingOutcome::Dismissed => close_menu(&mut layer, &attach, key),
            RingOutcome::Chosen(chosen) => {
                layer.menu_at = None;
                match chosen {
                    "follow" if riding => attach.detach(),
                    "follow" => attach.attach(key, name.clone(), ATTACH_RANGE),
                    "path" => layer.show_path = !layer.show_path,
                    "more" => layer.card_open = true,
                    _ => {}
                }
                if !layer.card_open && !attach.is(key) && !layer.show_path {
                    layer.selected = None;
                }
            }
        }
        return;
    }

    if !layer.card_open {
        return;
    }
    let [sr, sg, sb] = def.color.map(|c| (c.min(1.0) * 255.0) as u8);
    let swatch = egui::Color32::from_rgb(sr, sg, sb);
    let bottom = crate::systems::dock::reserved_bottom() + 8.0;
    let width = 300.0_f32.min(egui.viewport_pts().x - 2.0 * theme::OSD_MARGIN);
    let mut close = false;
    let mut toggle_ride = false;
    egui::Area::new(egui::Id::new("satellite_card"))
        .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -bottom))
        .show(egui.ctx(), |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.set_width(width);
                ui.horizontal(|ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        close = theme::close_button(ui).clicked();
                        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                            let (rect, _) = ui
                                .allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
                            ui.painter().circle_filled(rect.center(), 4.5, swatch);
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(&name).size(theme::SIZE_TITLE).strong(),
                                )
                                .truncate(),
                            );
                        });
                    });
                });
                ui.label(
                    egui::RichText::new(def.label)
                        .size(theme::SIZE_SUB)
                        .color(theme::TEXT_DIM),
                );
                ui.add_space(4.0);
                egui::Grid::new("satellite_card_rows")
                    .num_columns(2)
                    .spacing([12.0, 2.0])
                    .show(ui, |ui| {
                        let row = |ui: &mut egui::Ui, k: &str, v: String| {
                            ui.label(egui::RichText::new(k).color(theme::TEXT_DIM));
                            ui.label(v);
                            ui.end_row();
                        };
                        row(ui, "Altitude", format!("{:.0} km", height / 1000.0));
                        row(ui, "Speed", format!("{speed_kms:.2} km/s"));
                        row(ui, "Over", format!("{lat:.2}°, {lng:.2}°"));
                        row(ui, "Orbit", format_period(period));
                        row(ui, "NORAD", norad.to_string());
                    });
                ui.add_space(6.0);
                let text = if riding { "Let go" } else { "Follow" };
                if theme::action_button(ui, &format!("{}  {text}", icons::FOLLOW)).clicked() {
                    toggle_ride = true;
                }
            });
        });
    if toggle_ride {
        if riding {
            attach.detach();
        } else {
            attach.attach(key, name, ATTACH_RANGE);
        }
    }
    if close {
        layer.card_open = false;
        close_menu(&mut layer, &attach, key);
    }
}

/// Close the ring, and let the selection go unless something still shows it:
/// the camera riding it, or its drawn path.
fn close_menu(layer: &mut SatelliteLayer, attach: &CameraAttach, key: AttachKey) {
    layer.menu_at = None;
    if !layer.card_open && !attach.is(key) && !layer.show_path {
        layer.selected = None;
    }
}

/// An orbital period as a person reads one: minutes for low orbits, hours
/// above them.
fn format_period(minutes: f64) -> String {
    if minutes < 180.0 {
        format!("once every {minutes:.0} min")
    } else {
        format!("once every {:.1} h", minutes / 60.0)
    }
}

/// The credit line for the Layers panel, while the layer is on.
#[must_use]
pub fn attribution(dl: &DataLayers) -> Option<&'static str> {
    dl.satellites_enabled
        .then_some("Satellite orbits: CelesTrak (celestrak.org), propagated with SGP4")
}

/// Persisted kind switches → the panel's array, defaulting each to on.
#[must_use]
pub fn kinds_from_prefs(saved: &HashMap<String, bool>) -> [bool; KINDS.len()] {
    std::array::from_fn(|i| saved.get(KINDS[i].key).copied().unwrap_or(true))
}

/// The panel's array → the persisted form.
#[must_use]
pub fn kinds_to_prefs(kinds: &[bool; KINDS.len()]) -> HashMap<String, bool> {
    KINDS
        .iter()
        .zip(kinds)
        .map(|(def, on)| (def.key.to_string(), *on))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ISS element set the `sgp4` crate's own examples use, epoch
    /// 2020-07-12 21:16:01 UTC.
    const ISS: &str = "ISS (ZARYA)
1 25544U 98067A   20194.88612269 -.00002218  00000-0 -31515-4 0  9992
2 25544  51.6461 221.2784 0001413  89.1723 280.4612 15.49507896236008
";

    fn iss() -> Satellite {
        parse_catalogue(ISS).pop().expect("the ISS parses")
    }

    #[test]
    fn a_three_line_set_parses_and_is_a_station() {
        let s = iss();
        assert_eq!(s.name, "ISS (ZARYA)");
        assert_eq!(s.norad, 25544);
        assert_eq!(s.kind, SatKind::Station);
    }

    #[test]
    fn a_bad_record_is_skipped_not_fatal() {
        let text = format!("BROKEN\n1 garbage\n2 garbage\n{ISS}");
        let cat = parse_catalogue(&text);
        assert_eq!(cat.len(), 1);
        assert_eq!(cat[0].norad, 25544);
    }

    #[test]
    fn the_iss_flies_at_its_altitude_and_speed() {
        let s = iss();
        let (r, v) = s.ecef_at(s.epoch_unix + 600.0).expect("propagates");
        let (lat, _, h) = cartopolis_core::ecef_to_geodetic(r);
        assert!((380_000.0..460_000.0).contains(&h), "altitude {h}");
        // Earth-fixed speed: the inertial 7.66 km/s less the ground's turn.
        assert!(
            (7_000.0..7_700.0).contains(&v.length()),
            "speed {}",
            v.length()
        );
        // An inclination of 51.6° bounds the latitude.
        assert!(lat.abs() <= 51.7, "latitude {lat}");
    }

    /// The Earth-fixed velocity is the derivative of the Earth-fixed position:
    /// a finite difference of two propagations must agree with it. This is the
    /// test that catches a forgotten (or doubled) ω × r.
    #[test]
    fn the_fixed_frame_velocity_matches_the_motion() {
        let s = iss();
        let t = s.epoch_unix + 1_200.0;
        let (r0, v) = s.ecef_at(t).unwrap();
        let (r1, _) = s.ecef_at(t + 1.0).unwrap();
        let fd = r1 - r0;
        assert!((fd - v).length() < 10.0, "fd {fd:?} vs v {v:?}");
    }

    /// The shader's second-order step stays within a few hundred metres of SGP4
    /// over one rebuild interval — far under a point's size from orbit.
    #[test]
    fn the_shader_step_tracks_sgp4_over_a_rebuild() {
        let s = iss();
        let t = s.epoch_unix + 3_000.0;
        let (r0, v0) = s.ecef_at(t).unwrap();
        let dt = REBUILD_SECS;
        let a = -r0 * (f64::from(GM) / r0.length().powi(3));
        let stepped = r0 + v0 * dt + a * (0.5 * dt * dt);
        let (truth, _) = s.ecef_at(t + dt).unwrap();
        assert!(
            stepped.distance(truth) < 300.0,
            "{}",
            stepped.distance(truth)
        );
    }

    #[test]
    fn names_sort_into_kinds() {
        assert_eq!(classify("STARLINK-1007"), SatKind::Starlink);
        assert_eq!(classify("CSS (TIANHE)"), SatKind::Station);
        assert_eq!(classify("NAVSTAR 81 (USA 319)"), SatKind::Navigation);
        assert_eq!(classify("GSAT0223 (GALILEO 27)"), SatKind::Navigation);
        assert_eq!(classify("NOAA 19"), SatKind::Observation);
        assert_eq!(classify("SENTINEL-2A"), SatKind::Observation);
        assert_eq!(classify("ONEWEB-0012"), SatKind::Other);
    }

    #[test]
    fn the_snapshot_filters_by_kind_and_lays_four_corners_each() {
        let cat = parse_catalogue(ISS);
        let t = cat[0].epoch_unix;
        let all = build_snapshot(&cat, t, (53.2, 6.56), 1, u8::MAX);
        assert_eq!(all.placed.len(), 1);
        assert_eq!(all.positions.len(), 4);
        assert_eq!(all.indices.len(), 6);
        let none = build_snapshot(&cat, t, (53.2, 6.56), 1, !(1 << SatKind::Station as u8));
        assert!(none.placed.is_empty());
    }

    #[test]
    fn kinds_round_trip_and_default_on() {
        let kinds = kinds_from_prefs(&HashMap::new());
        assert!(kinds.iter().all(|k| *k));
        let mut k = kinds;
        k[SatKind::Starlink as usize] = false;
        assert_eq!(kinds_from_prefs(&kinds_to_prefs(&k)), k);
    }

    #[test]
    fn the_planet_hides_the_far_side() {
        let centre = Vec3::ZERO;
        let cam = Vec3::new(0.0, 0.0, 20.0);
        assert!(behind_the_planet(
            cam,
            Vec3::new(0.0, 0.0, -20.0),
            centre,
            6.0
        ));
        assert!(!behind_the_planet(
            cam,
            Vec3::new(10.0, 0.0, 10.0),
            centre,
            6.0
        ));
    }
}
