//! A building's facade measured from one photograph somebody took of it with
//! their phone and sent from the building's card.
//!
//! A Mapillary photograph comes with a pose computed from its neighbours
//! (`mapillary.rs`); a phone's comes with a GPS fix that is five to ten metres
//! out between the houses of a city centre and, at best, a compass that is
//! twenty degrees out beside a steel-framed shopfront. Projected from that,
//! the building's outline lands on its neighbour. So the pose is **searched
//! for**: the 3DBAG shells of the street are drawn from every camera position
//! within the fix's error, every heading that looks at the building and every
//! tilt a person holds a phone at, and the one whose drawing agrees best with
//! what the facade network sees in the photograph — wall where the shells
//! are, sky above the roofline — is taken. The roofline of a street is as good
//! as a fingerprint: neighbours of different heights and roof shapes leave one
//! pose that fits. No compass is needed, since the person photographed the
//! building they had open.
//!
//! Then the building's own wall is measured as a street photograph's is: the
//! pixels the drawing puts on its shell, that the network calls wall, above
//! the parked cars and the people.
//!
//! The photograph is read and dropped: nothing of it is kept or shown
//! (`crate::facade_edits`).

use std::path::Path;

use cartopolis_geo::formats::facades::Facade;
use cartopolis_geo::formats::{bag, CACHE_ZOOM};

use super::facades::{class, labels, load_net, measure, Facts, NET_OUT};

/// A phone's height over the street, metres.
const CAMERA_M: f64 = 1.5;
/// How far around the building shells are drawn, metres.
const SCENE_M: f64 = 160.0;
/// The grid the search draws and compares on; the measurement uses the
/// network's own [`NET_OUT`].
const SEARCH_GRID: usize = 64;
/// Wall is measured from this high up: below it stand cars and people, which
/// the network, seeing them against a wall, calls wall.
const ABOVE_STREET_M: f64 = 3.0;
/// Wall pixels the photograph must give to count.
const MIN_PIXELS: usize = 800;
/// A third of what the drawing puts on the building must be its wall, or the
/// pose found is not the one it was taken from.
const MIN_WALL_SHARE: f64 = 0.3;
/// A pose whose drawing agrees with the photograph less than this, per
/// cell, is not where it was taken from. It does not tell a right pose from
/// a neighbour's — those fit 0.4–0.75 alike — only from no pose at all:
/// Mapillary's wide lenses came out at −0.01 and 0.26 (2026-11-19).
const MIN_FIT: f64 = 0.35;
/// A focal length when the photograph does not say: a phone's main camera,
/// 26 mm in 35 mm terms, over the 36 mm long side.
const PHONE_FOCAL: f64 = 26.0 / 36.0;

/// What the photograph and the phone say about where it was taken. The
/// photograph's own EXIF wins where it has the same fact: the phone's fix is
/// where it was when the photograph was sent, which can be another street.
#[derive(Clone, Copy, Debug, Default)]
pub struct Hint {
    pub lat: Option<f64>,
    pub lng: Option<f64>,
    /// The fix's accuracy, metres.
    pub accuracy_m: Option<f64>,
    /// Which way the camera pointed, degrees clockwise from north.
    pub heading: Option<f64>,
}

/// The building somebody photographed, with what the registers say about it.
pub struct Target {
    pub(super) ring: Vec<(f64, f64)>,
    pub(super) eave: f64,
    pub(super) facts: Facts,
}

fn read_site(site: &Path, rel: &str) -> Option<Vec<u8>> {
    std::fs::read(site.join(rel)).ok()
}

/// The lower quarter of a building's roof heights, as the client takes its
/// eaves; 10 m where its shell is not on this host.
fn eave_of(cells: &[cartopolis_geo::lod22_cell::Cell], pand: &str) -> f64 {
    let mut h: Vec<f32> = cells
        .iter()
        .flat_map(|c| c.elements.iter())
        .filter(|e| e.id == pand)
        .flat_map(|e| {
            e.t.iter()
                .filter(|t| t.3 == 1)
                .flat_map(|t| [t.0, t.1, t.2])
                .filter_map(|i| e.p.get(i as usize).map(|p| p.2))
                .collect::<Vec<_>>()
        })
        .collect();
    h.sort_by(f32::total_cmp);
    h.get(h.len() / 4).copied().map_or(10.0, f64::from)
}

/// The z14 cells around `near`, the one it is in first.
fn cells_around(near: (f64, f64)) -> Vec<(i64, i64)> {
    let (cx, cy) = cartopolis_core::world::lat_lng_to_tile(near.0, near.1, CACHE_ZOOM);
    let mut out = vec![(cx, cy)];
    for dy in -1..=1 {
        for dx in -1..=1 {
            if (dx, dy) != (0, 0) {
                out.push((cx + dx, cy + dy));
            }
        }
    }
    out
}

fn bag_cells(site: &Path, cells: &[(i64, i64)]) -> Vec<((i64, i64), bag::Cell)> {
    cells
        .iter()
        .filter_map(|&(x, y)| {
            let text = read_site(site, &format!("bag/{CACHE_ZOOM}/{x}/{y}.json"))?;
            Some(((x, y), bag::parse(std::str::from_utf8(&text).ok()?).ok()?))
        })
        .collect()
}

fn lod22_cells(site: &Path, cells: &[(i64, i64)]) -> Vec<cartopolis_geo::lod22_cell::Cell> {
    cells
        .iter()
        .filter_map(|&(x, y)| {
            let bytes = read_site(site, &format!("lod22/{CACHE_ZOOM}/{x}/{y}.bin"))?;
            cartopolis_geo::lod22_cell::decode(&bytes).ok()
        })
        .collect()
}

/// Building `pand` near (`near`), and the z14 cell its BAG record is filed
/// under: from the `bag` and `lod22` trees of the site at `site`.
pub fn target(site: &Path, pand: &str, near: (f64, f64)) -> Result<(Target, (i64, i64)), String> {
    let around = cells_around(near);
    let (cell, b) = bag_cells(site, &around)
        .into_iter()
        .find_map(|(c, cell)| {
            cell.buildings
                .unwrap_or_default()
                .into_iter()
                .find(|b| b.id == pand)
                .map(|b| (c, b))
        })
        .ok_or_else(|| format!("{pand} is not in this server's map data"))?;
    let eave = eave_of(&lod22_cells(site, &around), pand);
    Ok((
        Target {
            ring: b.fp,
            eave,
            facts: Facts {
                year: b.year,
                material: b.mat,
                monument: b.mon.is_some(),
                colour: b.col,
            },
        },
        cell,
    ))
}

// ─── The scene ───────────────────────────────────────────────────────────────

/// The shells around a building in metres east, north and up of a local
/// origin.
pub(super) struct Scene {
    lat0: f64,
    lng0: f64,
    kx: f64,
    tris: Vec<([[f64; 3]; 3], bool)>,
    /// Every outline in the scene, where a camera cannot stand.
    rings: Vec<Vec<[f64; 2]>>,
    /// The photographed building.s middle: the origin, as it is built.
    centre: [f64; 2],
    eave: f64,
}

impl Scene {
    fn local(&self, lat: f64, lng: f64) -> [f64; 2] {
        [(lng - self.lng0) * self.kx, (lat - self.lat0) * 110_540.0]
    }

    fn inside_a_building(&self, p: [f64; 2]) -> bool {
        self.rings.iter().any(|r| inside(r, p[0], p[1]))
    }

    pub(super) fn load(site: &Path, pand: &str, target: &Target) -> Self {
        let (lat0, lng0) = centroid(&target.ring);
        let kx = 111_320.0 * lat0.to_radians().cos();
        let mut scene = Scene {
            lat0,
            lng0,
            kx,
            tris: Vec::new(),
            rings: Vec::new(),
            centre: [0.0, 0.0],
            eave: target.eave,
        };
        let around = cells_around((lat0, lng0));
        let near = |p: [f64; 2]| p[0].hypot(p[1]) < SCENE_M;
        for cell in lod22_cells(site, &around) {
            for e in &cell.elements {
                let p: Vec<[f64; 3]> =
                    e.p.iter()
                        .map(|&(la, lo, h)| {
                            let [x, y] = scene.local(la, lo);
                            [x, y, f64::from(h)]
                        })
                        .collect();
                if !p.iter().any(|q| near([q[0], q[1]])) {
                    continue;
                }
                let own = e.id == pand;
                for &(a, b, c, _) in &e.t {
                    let (Some(a), Some(b), Some(c)) =
                        (p.get(a as usize), p.get(b as usize), p.get(c as usize))
                    else {
                        continue;
                    };
                    scene.tris.push(([*a, *b, *c], own));
                }
            }
        }
        for (_, cell) in bag_cells(site, &around) {
            for b in cell.buildings.unwrap_or_default() {
                let ring: Vec<[f64; 2]> =
                    b.fp.iter().map(|&(la, lo)| scene.local(la, lo)).collect();
                if ring.iter().any(|&q| near(q)) {
                    scene.rings.push(ring);
                }
            }
        }
        scene
    }
}

fn centroid(ring: &[(f64, f64)]) -> (f64, f64) {
    let n = ring.len().max(1) as f64;
    (
        ring.iter().map(|p| p.0).sum::<f64>() / n,
        ring.iter().map(|p| p.1).sum::<f64>() / n,
    )
}

fn inside(poly: &[[f64; 2]], x: f64, y: f64) -> bool {
    let mut c = false;
    let n = poly.len();
    for i in 0..n {
        let (a, b) = (poly[i], poly[(i + 1) % n]);
        if (a[1] > y) != (b[1] > y) && x < (b[0] - a[0]) * (y - a[1]) / (b[1] - a[1]) + a[0] {
            c = !c;
        }
    }
    c
}

// ─── Drawing it from a pose ──────────────────────────────────────────────────

/// Where a camera stood and how it was held: metres east and north of the
/// scene's origin, heading clockwise from north and tilt up from level in
/// degrees, the picture turned clockwise by `roll` degrees (a phone is
/// seldom held level), focal length over the picture's long side.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Pose {
    pub e: f64,
    pub n: f64,
    pub yaw: f64,
    pub pitch: f64,
    pub roll: f64,
    pub focal: f64,
}

/// What a drawn cell shows.
const GROUND: u8 = 0;
const SKY: u8 = 1;
const OTHER: u8 = 2;
const TARGET: u8 = 3;

struct Camera {
    c: [f64; 3],
    r: [f64; 3],
    d: [f64; 3],
    f: [f64; 3],
    /// Pixels per unit of x/z.
    s: f64,
    w: f64,
    h: f64,
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

impl Pose {
    fn camera(&self, w: f64, h: f64) -> Camera {
        let (y, p) = (self.yaw.to_radians(), self.pitch.to_radians());
        let f = [y.sin() * p.cos(), y.cos() * p.cos(), p.sin()];
        let r = [y.cos(), -y.sin(), 0.0];
        // Down = forward × right.
        let d = [
            f[1] * r[2] - f[2] * r[1],
            f[2] * r[0] - f[0] * r[2],
            f[0] * r[1] - f[1] * r[0],
        ];
        // The picture turned about the line of sight.
        let (cr, sr) = (self.roll.to_radians().cos(), self.roll.to_radians().sin());
        let (r, d) = (
            [0, 1, 2].map(|i| r[i] * cr + d[i] * sr),
            [0, 1, 2].map(|i| d[i] * cr - r[i] * sr),
        );
        Camera {
            c: [self.e, self.n, CAMERA_M],
            r,
            d,
            f,
            s: w.max(h) * self.focal,
            w,
            h,
        }
    }
}

impl Camera {
    /// (right, down, forward) of a scene point.
    fn view(&self, p: [f64; 3]) -> [f64; 3] {
        let q = [p[0] - self.c[0], p[1] - self.c[1], p[2] - self.c[2]];
        [dot(self.r, q), dot(self.d, q), dot(self.f, q)]
    }
}

/// Nearer than this a triangle is cut where it crosses, metres.
const NEAR_Z: f64 = 0.3;

/// The scene from `pose` on a `g`×`g` grid stretched over a `w`×`h` picture,
/// as the network's labels are: what each cell shows, and for a building cell
/// how high up the wall it is.
fn draw(scene: &Scene, pose: &Pose, w: f64, h: f64, g: usize) -> (Vec<u8>, Vec<f32>) {
    let cam = pose.camera(w, h);
    let mut what = vec![GROUND; g * g];
    let mut depth = vec![f64::INFINITY; g * g];
    let mut up = vec![0.0f32; g * g];
    let gf = g as f64;
    for (tri, own) in &scene.tris {
        // Each vertex in view space, with its height to carry along.
        let v: Vec<([f64; 3], f64)> = tri.iter().map(|&p| (cam.view(p), p[2])).collect();
        if v.iter().all(|(q, _)| q[2] < NEAR_Z) {
            continue;
        }
        // Clip against the near plane (Sutherland–Hodgman, one plane).
        let mut poly: Vec<([f64; 3], f64)> = Vec::with_capacity(4);
        for i in 0..3 {
            let (a, b) = (v[i], v[(i + 1) % 3]);
            let (ina, inb) = (a.0[2] >= NEAR_Z, b.0[2] >= NEAR_Z);
            if ina {
                poly.push(a);
            }
            if ina != inb {
                let t = (NEAR_Z - a.0[2]) / (b.0[2] - a.0[2]);
                let lerp = |x: f64, y: f64| x + (y - x) * t;
                poly.push((
                    [lerp(a.0[0], b.0[0]), lerp(a.0[1], b.0[1]), NEAR_Z],
                    lerp(a.1, b.1),
                ));
            }
        }
        // To grid coordinates, with 1/z and h/z for perspective-correct
        // interpolation.
        let s: Vec<[f64; 4]> = poly
            .iter()
            .map(|(q, hgt)| {
                let iz = 1.0 / q[2];
                [
                    (cam.w / 2.0 + cam.s * q[0] * iz) / cam.w * gf,
                    (cam.h / 2.0 + cam.s * q[1] * iz) / cam.h * gf,
                    iz,
                    hgt * iz,
                ]
            })
            .collect();
        for k in 1..s.len().saturating_sub(1) {
            fill(&s[0], &s[k], &s[k + 1], g, |i, iz, hz| {
                let z = 1.0 / iz;
                if z < depth[i] {
                    depth[i] = z;
                    what[i] = if *own { TARGET } else { OTHER };
                    up[i] = (hz * z) as f32;
                }
            });
        }
    }
    // Nothing drawn: sky where the ray through the cell rises, else street.
    for gy in 0..g {
        let yn = ((gy as f64 + 0.5) / gf * cam.h - cam.h / 2.0) / cam.s;
        for gx in 0..g {
            let xn = ((gx as f64 + 0.5) / gf * cam.w - cam.w / 2.0) / cam.s;
            let i = gy * g + gx;
            if what[i] == GROUND && cam.f[2] + xn * cam.r[2] + yn * cam.d[2] > 0.0 {
                what[i] = SKY;
            }
        }
    }
    (what, up)
}

/// Every grid cell whose centre is inside triangle a b c (grid x, grid y,
/// 1/z, h/z), with the interpolated 1/z and h/z.
fn fill(a: &[f64; 4], b: &[f64; 4], c: &[f64; 4], g: usize, mut put: impl FnMut(usize, f64, f64)) {
    let area = (b[0] - a[0]) * (c[1] - a[1]) - (c[0] - a[0]) * (b[1] - a[1]);
    if area.abs() < 1e-9 {
        return;
    }
    let lo_x = a[0].min(b[0]).min(c[0]).floor().max(0.0) as usize;
    let hi_x = (a[0].max(b[0]).max(c[0]).ceil() as i64).clamp(0, g as i64) as usize;
    let lo_y = a[1].min(b[1]).min(c[1]).floor().max(0.0) as usize;
    let hi_y = (a[1].max(b[1]).max(c[1]).ceil() as i64).clamp(0, g as i64) as usize;
    for y in lo_y..hi_y {
        let py = y as f64 + 0.5;
        for x in lo_x..hi_x {
            let px = x as f64 + 0.5;
            let w0 = ((b[0] - px) * (c[1] - py) - (c[0] - px) * (b[1] - py)) / area;
            let w1 = ((c[0] - px) * (a[1] - py) - (a[0] - px) * (c[1] - py)) / area;
            let w2 = 1.0 - w0 - w1;
            if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
                continue;
            }
            put(
                y * g + x,
                w0 * a[2] + w1 * b[2] + w2 * c[2],
                w0 * a[3] + w1 * b[3] + w2 * c[3],
            );
        }
    }
}

// ─── What the photograph shows ───────────────────────────────────────────────

/// The scene network: UperNet with a ConvNeXt-T backbone trained on ADE20K
/// (`openmmlab/upernet-convnext-tiny`, MIT), exported at 576² with its
/// logits pooled to [`SCENE_OUT`]², in half precision. The facade network
/// cannot do this part: trained on crops of one facade, it calls the street,
/// the trees and the sky facade too in a photograph of a street (tried
/// 2026-11-19), and the search then fills the picture with buildings.
///
/// 119 MB, so not built into the binary: read from `ATLAS_SCENE_MODEL`, else
/// from atlas's data directory (`models/`, [`use_models_in`]), else from the
/// checkout it was built in.
const SCENE_IN: usize = 576;
pub(super) const SCENE_OUT: usize = 144;

/// What a cell of a photograph shows, as far as placing the camera goes.
pub(super) mod seen {
    pub const UNKNOWN: u8 = 0;
    pub const BUILDING: u8 = 1;
    pub const SKY: u8 = 2;
    pub const GROUND: u8 = 3;
    /// Stands in front of whatever is behind it: a tree, a person, a car.
    pub const IN_FRONT: u8 = 4;
}

/// ADE20K's 150 classes in the few above.
fn seen_of(ade: usize) -> u8 {
    match ade {
        // wall, building, house, skyscraper, windowpane, door, column,
        // tower: what a facade is made of.
        0 | 1 | 8 | 14 | 25 | 42 | 48 | 84 => seen::BUILDING,
        2 => seen::SKY,
        // floor, road, grass, sidewalk, earth, field, path, step, land,
        // water, dirt track.
        3 | 6 | 9 | 11 | 13 | 21 | 29 | 52 | 91 | 94 | 121 => seen::GROUND,
        // tree, person, plant, car, fence, signboard, streetlight, pole,
        // awning, bicycle, bus, truck, van, umbrella-like canopy, bench,
        // flag, poster, traffic light, sculpture, fountain, railing.
        4 | 12 | 17 | 20 | 32 | 38 | 43 | 69 | 80 | 83 | 86 | 87 | 93 | 100 | 102 | 104 | 106
        | 116 | 127 | 132 | 136 | 149 => seen::IN_FRONT,
        _ => seen::UNKNOWN,
    }
}

/// The scene network's file name, in `assets/models/` and on a host in its
/// data directory's `models/`.
pub const SCENE_MODEL: &str = "scene_upernet_convnext_tiny_fp16.onnx";

static MODELS: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();

/// Where a deployed atlas keeps the networks too big for its binary: its
/// data directory's `models/`. Called once at start.
pub fn use_models_in(dir: &Path) {
    let _ = MODELS.set(dir.to_path_buf());
}

type SceneNet = std::sync::Arc<tract_onnx::prelude::TypedRunnableModel>;

fn scene_net() -> Result<SceneNet, String> {
    use tract_onnx::prelude::*;
    static NET: std::sync::OnceLock<Result<SceneNet, String>> = std::sync::OnceLock::new();
    NET.get_or_init(|| {
        let path = std::env::var_os("ATLAS_SCENE_MODEL")
            .map(std::path::PathBuf::from)
            .or_else(|| {
                MODELS
                    .get()
                    .map(|d| d.join(SCENE_MODEL))
                    .filter(|p| p.exists())
            })
            .unwrap_or_else(|| {
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../assets/models")
                    .join(SCENE_MODEL)
            });
        tract_onnx::onnx()
            .model_for_path(&path)
            .and_then(|m| m.into_typed())
            // Stored in half precision to halve the file; run in full. An
            // x86 host has no half-precision arithmetic, and emulating it took
            // the VPS 25 minutes for one photograph against the M5's ten
            // seconds (2026-11-20).
            .and_then(|m| {
                use tract_onnx::tract_core::transform::ModelTransform;
                tract_onnx::tract_core::floats::FloatPrecisionTranslator::new(
                    DatumType::F16,
                    DatumType::F32,
                )
                .transform_into(m)
            })
            .and_then(|m| m.into_optimized())
            .and_then(|m| m.into_runnable())
            .map_err(|e| format!("scene network {}: {e:#}", path.display()))
    })
    .clone()
}

/// What every cell of a [`SCENE_OUT`]² grid over `img` shows (stretched
/// square, as it was trained).
pub(super) fn scene_labels(img: &image::RgbImage) -> Result<Vec<u8>, String> {
    use tract_onnx::prelude::*;
    const MEAN: [f32; 3] = [0.485, 0.456, 0.406];
    const STD: [f32; 3] = [0.229, 0.224, 0.225];
    let net = scene_net()?;
    let sq = image::imageops::resize(
        img,
        SCENE_IN as u32,
        SCENE_IN as u32,
        image::imageops::FilterType::Triangle,
    );
    let input = tract_ndarray::Array4::from_shape_fn((1, 3, SCENE_IN, SCENE_IN), |(_, c, y, x)| {
        let v = f32::from(sq.get_pixel(x as u32, y as u32).0[c]) / 255.0;
        (v - MEAN[c]) / STD[c]
    });
    let out = net
        .run(tvec!(Tensor::from(input).into()))
        .map_err(|e| format!("scene network: {e:#}"))?;
    let logits = out[0]
        .to_plain_array_view::<f32>()
        .map_err(|e| format!("scene network output: {e:#}"))?;
    let classes = logits.shape()[1];
    Ok((0..SCENE_OUT * SCENE_OUT)
        .map(|i| {
            let (y, x) = (i / SCENE_OUT, i % SCENE_OUT);
            let best = (0..classes)
                .max_by(|&a, &b| logits[[0, a, y, x]].total_cmp(&logits[[0, b, y, x]]))
                .unwrap_or(0);
            seen_of(best)
        })
        .collect())
}

/// How well a drawing agrees with what the photograph shows: a drawn
/// building where it shows one, sky where it shows sky, street below them.
/// What stands in front — trees, people, cars — says nothing either way.
fn agreement(what: &[u8], lab: &[u8]) -> f64 {
    what.iter()
        .zip(lab)
        .map(|(&w, &l)| match (w, l) {
            (_, seen::UNKNOWN | seen::IN_FRONT) => 0.0,
            (TARGET | OTHER, seen::BUILDING) | (SKY, seen::SKY) => 1.0,
            (TARGET | OTHER | SKY, _) => -1.0,
            (_, seen::GROUND) => 0.5,
            (_, seen::SKY) => -1.0,
            _ => -0.5,
        })
        .sum()
}

/// A `n`² grid of labels at a coarser `g`²: the label at each coarse cell's
/// middle.
fn coarsen(lab: &[u8], n: usize, g: usize) -> Vec<u8> {
    (0..g * g)
        .map(|i| {
            let (y, x) = (i / g, i % g);
            let fy = ((2 * y + 1) * n) / (2 * g);
            let fx = ((2 * x + 1) * n) / (2 * g);
            lab[fy * n + fx]
        })
        .collect()
}

/// Where the search starts from: the fix in scene metres, how far round it
/// to look, the heading when something gave one, the lens.
#[derive(Clone, Copy, Debug)]
pub(super) struct Prior {
    pub at: [f64; 2],
    pub radius: f64,
    pub heading: Option<f64>,
    pub focal: f64,
}

/// How well the scene drawn from `pose` agrees with `coarse` (what the
/// photograph shows at [`SEARCH_GRID`]); `f64::MIN` when the building is not
/// a twentieth of the picture.
fn score(scene: &Scene, coarse: &[u8], w: f64, h: f64, pose: &Pose) -> f64 {
    let g = SEARCH_GRID;
    let (what, _) = draw(scene, pose, w, h, g);
    let seen = what.iter().filter(|&&x| x == TARGET).count();
    if seen * 20 < g * g {
        return f64::MIN;
    }
    agreement(&what, coarse)
}

/// The pose the photograph was taken from: the drawing that agrees best with
/// `lab`, over every place within the fix's error a person could stand,
/// every heading that looks at the building, every tilt and a phone held
/// askew. `None` when no pose puts the building in the picture.
fn search(scene: &Scene, lab: &[u8], w: f64, h: f64, prior: Prior) -> Option<(Pose, f64)> {
    let Prior {
        at,
        radius,
        heading,
        focal,
    } = prior;
    let coarse = coarsen(lab, SCENE_OUT, SEARCH_GRID);
    let step = 2.5;
    let n = (radius / step).ceil() as i64;
    let places: Vec<[f64; 2]> = (-n..=n)
        .flat_map(|iy| (-n..=n).map(move |ix| [at[0] + ix as f64 * step, at[1] + iy as f64 * step]))
        .filter(|p| {
            let dist = (scene.centre[0] - p[0]).hypot(scene.centre[1] - p[1]);
            (p[0] - at[0]).hypot(p[1] - at[1]) <= radius
                && !scene.inside_a_building(*p)
                && (4.0..150.0).contains(&dist)
        })
        .collect();
    // Every place on a core of its own: each is some three thousand drawings.
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get());
    let mut found: Vec<(Pose, f64)> = std::thread::scope(|sc| {
        places
            .chunks(places.len().div_ceil(threads).max(1))
            .map(|chunk| {
                let coarse = &coarse;
                sc.spawn(move || {
                    let mut out = Vec::new();
                    for p in chunk {
                        let to = [scene.centre[0] - p[0], scene.centre[1] - p[1]];
                        let (mid, span) = match heading {
                            Some(hd) => (hd, 30.0),
                            None => (to[0].atan2(to[1]).to_degrees(), 40.0),
                        };
                        let mut yaw = mid - span;
                        while yaw <= mid + span {
                            // A phone held 10° askew puts the roofline across
                            // the picture, and searched only level the true
                            // place scores worse than a neighbour's (tried
                            // 2026-11-19).
                            for (pitch, roll) in [-5.0, 1.0, 7.0, 13.0, 19.0, 25.0]
                                .into_iter()
                                .flat_map(|p| [-8.0, 0.0, 8.0].map(|r| (p, r)))
                            {
                                let pose = Pose {
                                    e: p[0],
                                    n: p[1],
                                    yaw,
                                    pitch,
                                    roll,
                                    focal,
                                };
                                let s = score(scene, coarse, w, h, &pose);
                                if s > f64::MIN {
                                    out.push((pose, s));
                                }
                            }
                            yaw += 4.0;
                        }
                    }
                    out
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .flat_map(|t| t.join().unwrap_or_default())
            .collect()
    });
    found.sort_by(|a, b| b.1.total_cmp(&a.1));
    found.truncate(8);
    found
        .into_iter()
        .map(|(pose, best)| climb(scene, &coarse, w, h, pose, best, at, radius * 1.3))
        .max_by(|a, b| a.1.total_cmp(&b.1))
}

/// Walk `pose` uphill one step at a time, staying within `reach` metres of
/// `at`; `best` is its own score.
#[allow(clippy::too_many_arguments)]
fn climb(
    scene: &Scene,
    coarse: &[u8],
    w: f64,
    h: f64,
    mut pose: Pose,
    mut best: f64,
    at: [f64; 2],
    reach: f64,
) -> (Pose, f64) {
    let steps = [1.0, 1.0, 1.0, 1.5, 1.5, 0.05];
    for _ in 0..60 {
        let mut moved = false;
        for (k, &st) in steps.iter().enumerate() {
            for sign in [-1.0, 1.0] {
                let mut q = pose;
                match k {
                    0 => q.e += sign * st,
                    1 => q.n += sign * st,
                    2 => q.yaw += sign * st,
                    3 => q.pitch += sign * st,
                    4 => q.roll += sign * st,
                    _ => q.focal *= 1.0 + sign * st,
                }
                if scene.inside_a_building([q.e, q.n])
                    || q.roll.abs() > 15.0
                    || (q.e - at[0]).hypot(q.n - at[1]) > reach
                    || !(0.45..1.4).contains(&q.focal)
                {
                    continue;
                }
                let s = score(scene, coarse, w, h, &q);
                if s > best {
                    best = s;
                    pose = q;
                    moved = true;
                }
            }
        }
        if !moved {
            break;
        }
    }
    (pose, best)
}

/// A pose somebody else computed — Mapillary's, from its neighbours —
/// checked against the photograph and moved a few metres and degrees where
/// the drawing agrees better. `None` when it does not put the building in it.
fn refine(scene: &Scene, lab: &[u8], w: f64, h: f64, pose: Pose) -> Option<(Pose, f64)> {
    let coarse = coarsen(lab, SCENE_OUT, SEARCH_GRID);
    let s = score(scene, &coarse, w, h, &pose);
    (s > f64::MIN)
        .then(|| climb(scene, &coarse, w, h, pose, s, [pose.e, pose.n], 4.0))
        // Mapillary computed its lens; a climb that had to stretch it far
        // was fitting a wrong pose.
        .filter(|(p, _)| (p.focal / pose.focal - 1.0).abs() <= 0.15)
}

// ─── EXIF ────────────────────────────────────────────────────────────────────

/// What a photograph's EXIF says about where it was taken and with what lens.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Exif {
    lat: Option<f64>,
    lng: Option<f64>,
    heading: Option<f64>,
    /// Focal length in 35 mm terms, millimetres.
    focal35: Option<f64>,
}

/// The few tags wanted from a TIFF-structured EXIF block (with or without
/// its `Exif\0\0` preamble). Anything malformed reads as absent.
fn read_exif(raw: &[u8]) -> Exif {
    let t = raw.strip_prefix(b"Exif\0\0").unwrap_or(raw);
    let le = match t.get(..2) {
        Some(b"II") => true,
        Some(b"MM") => false,
        _ => return Exif::default(),
    };
    let u16_at = |o: usize| -> Option<u32> {
        let b = t.get(o..o + 2)?;
        Some(u32::from(if le {
            u16::from_le_bytes([b[0], b[1]])
        } else {
            u16::from_be_bytes([b[0], b[1]])
        }))
    };
    let u32_at = |o: usize| -> Option<u32> {
        let b = t.get(o..o + 4)?;
        let a = [b[0], b[1], b[2], b[3]];
        Some(if le {
            u32::from_le_bytes(a)
        } else {
            u32::from_be_bytes(a)
        })
    };
    // Every entry of the IFD at `o`: tag, type, count, value-or-offset field.
    let entries = |o: usize| -> Vec<(u32, u32, u32, usize)> {
        let Some(n) = u16_at(o) else {
            return Vec::new();
        };
        (0..n as usize)
            .filter_map(|i| {
                let e = o + 2 + i * 12;
                Some((u16_at(e)?, u16_at(e + 2)?, u32_at(e + 4)?, e + 8))
            })
            .collect()
    };
    let rational = |o: usize| -> Option<f64> {
        let (n, d) = (u32_at(o)?, u32_at(o + 4)?);
        (d != 0).then(|| f64::from(n) / f64::from(d))
    };
    // A RATIONAL's value is always at an offset.
    let rationals = |field: usize, count: u32| -> Option<Vec<f64>> {
        let at = u32_at(field)? as usize;
        (0..count as usize).map(|i| rational(at + i * 8)).collect()
    };
    let ascii = |field: usize| t.get(field).copied();
    let Some(ifd0) = u32_at(4) else {
        return Exif::default();
    };
    let mut out = Exif::default();
    for (tag, _, _, field) in entries(ifd0 as usize) {
        match tag {
            0x8769 => {
                if let Some(sub) = u32_at(field) {
                    for (tag, _, _, field) in entries(sub as usize) {
                        if tag == 0xA405 {
                            out.focal35 = u16_at(field).map(f64::from).filter(|&f| f > 0.0);
                        }
                    }
                }
            }
            0x8825 => {
                let Some(gps) = u32_at(field) else { continue };
                let (mut lat, mut lng, mut lat_ref, mut lng_ref) = (None, None, b'N', b'E');
                for (tag, _, count, field) in entries(gps as usize) {
                    let dms = || {
                        rationals(field, count)
                            .filter(|v| v.len() == 3)
                            .map(|v| v[0] + v[1] / 60.0 + v[2] / 3600.0)
                    };
                    match tag {
                        1 => lat_ref = ascii(field).unwrap_or(b'N'),
                        2 => lat = dms(),
                        3 => lng_ref = ascii(field).unwrap_or(b'E'),
                        4 => lng = dms(),
                        0x11 => out.heading = rationals(field, 1).and_then(|v| v.first().copied()),
                        _ => {}
                    }
                }
                out.lat = lat.map(|v| if lat_ref == b'S' { -v } else { v });
                out.lng = lng.map(|v| if lng_ref == b'W' { -v } else { v });
            }
            _ => {}
        }
    }
    out
}

/// A photograph decoded upright, with its EXIF.
fn decode(bytes: &[u8]) -> Result<(image::RgbImage, Exif), String> {
    use image::ImageDecoder;
    let mut decoder = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| e.to_string())?
        .into_decoder()
        .map_err(|e| format!("not a photograph: {e}"))?;
    let orientation = decoder
        .orientation()
        .unwrap_or(image::metadata::Orientation::NoTransforms);
    let exif = decoder
        .exif_metadata()
        .ok()
        .flatten()
        .map(|raw| read_exif(&raw))
        .unwrap_or_default();
    let mut img = image::DynamicImage::from_decoder(decoder).map_err(|e| e.to_string())?;
    img.apply_orientation(orientation);
    // The network sees 512² and the search 64²: a phone's 12 megapixels would
    // only cost the measurement time.
    let img = if img.width().max(img.height()) > 1600 {
        img.resize(1600, 1600, image::imageops::FilterType::Triangle)
    } else {
        img
    };
    Ok((img.to_rgb8(), exif))
}

// ─── Measuring ───────────────────────────────────────────────────────────────

/// One photograph to measure from, and where to start looking for the place
/// it was taken.
pub(super) struct Shot {
    /// For the log and the debug pictures.
    pub name: String,
    pub img: image::RgbImage,
    pub start: Start,
}

pub(super) enum Start {
    /// Somebody's phone: search round its fix.
    Search(Prior),
    /// Mapillary's computed pose: checked, and moved a little.
    Known(Pose),
}

/// Pixels a photograph gives to the pool at most, so that each counts as
/// much as the next (a near one has ten times the wall of a far one).
const SHARE: usize = 4000;

/// The wall pixels of the building in one photograph, read against the
/// photograph's own white where it has one. `None` when no pose puts the
/// building in it or too little of its wall shows.
fn wall_of(
    scene: &Scene,
    shot: &Shot,
    log: &dyn Fn(String),
) -> Result<Option<Vec<[u8; 3]>>, String> {
    let img = &shot.img;
    let shows = scene_labels(img)?;
    let (w, h) = (f64::from(img.width()), f64::from(img.height()));
    let found = match &shot.start {
        Start::Search(prior) => search(scene, &shows, w, h, *prior),
        Start::Known(pose) => refine(scene, &shows, w, h, *pose),
    };
    let Some((pose, fit)) = found else {
        log(format!("{}: no pose puts the building in it", shot.name));
        return Ok(None);
    };
    let fit = fit / (SEARCH_GRID * SEARCH_GRID) as f64;
    if fit < MIN_FIT {
        log(format!("{}: the best pose fits only {fit:.2}", shot.name));
        return Ok(None);
    }
    log(format!(
        "{}: from {:.1} m E {:.1} m N of the building, heading {:.0}°, tilt {:.0}°, roll {:.0}°, focal {:.2}; fit {:.2}",
        shot.name,
        pose.e,
        pose.n,
        pose.yaw.rem_euclid(360.0),
        pose.pitch,
        pose.roll,
        pose.focal,
        fit
    ));
    // The building's own wall: drawn on its shell, above the street, shown
    // as a building by the scene network and as wall — not a window, not a
    // sign — by the facade network.
    let (what, up) = draw(scene, &pose, w, h, SCENE_OUT);
    let floor = ABOVE_STREET_M.min(scene.eave * 0.5) as f32;
    let own = |i: usize| what[i] == TARGET && up[i] >= floor;
    let drawn = (0..what.len()).filter(|&i| own(i)).count();
    let building = (0..what.len())
        .filter(|&i| own(i) && shows[i] == seen::BUILDING)
        .count();
    let facade = labels(&load_net()?, img)?;
    let cell_of = |x: u32, y: u32, n: usize| {
        ((y as usize * n) / img.height() as usize) * n + (x as usize * n) / img.width() as usize
    };
    let debug = std::env::var_os("ATLAS_PHOTO_DEBUG").map(std::path::PathBuf::from);
    let mut shown = debug.as_ref().map(|_| img.clone());
    let mut px = Vec::new();
    let mut whites = Vec::new();
    for y in 0..img.height() {
        for x in 0..img.width() {
            let i = cell_of(x, y, SCENE_OUT);
            let p = img.get_pixel(x, y).0;
            let mine = own(i) && shows[i] == seen::BUILDING;
            if mine {
                whites.push(p);
            }
            if mine && facade[cell_of(x, y, NET_OUT)] == class::FACADE {
                px.push(p);
                if let Some(s) = shown.as_mut() {
                    s.put_pixel(x, y, image::Rgb([p[0] / 2, p[1] / 2 + 120, p[2] / 2]));
                }
            } else if let Some(s) = shown.as_mut() {
                let tint = match what[i] {
                    TARGET => Some([255, 0, 255]),
                    OTHER => Some([0, 160, 255]),
                    _ => None,
                };
                if let (Some(t), true) = (tint, (x + y) % 6 == 0) {
                    s.put_pixel(x, y, image::Rgb(t));
                }
            }
        }
    }
    if let (Some(dir), Some(s)) = (debug, shown) {
        let _ = std::fs::create_dir_all(&dir);
        let _ = s.save(dir.join(format!("{}.jpg", shot.name)));
    }
    log(format!(
        "{}: {} wall pixels; {building} of the {drawn} cells drawn on its wall show a building",
        shot.name,
        px.len()
    ));
    if px.len() < MIN_PIXELS || (building as f64) < drawn as f64 * MIN_WALL_SHARE {
        return Ok(None);
    }
    // The light: a photograph's exposure is set on the whole scene, so a wall
    // in shade comes out dark — the same brick read 81,69,62 in one and
    // 130,112,96 in another (2026-11-19). Its own white — the frames, the
    // sills, the cornices, in the same light — says how dark, where it has
    // one; what is left is evened out over several photographs.
    let px = match white_of(&whites) {
        Some(white) => px.iter().map(|&p| under(p, white)).collect(),
        None => px,
    };
    let step = (px.len() / SHARE).max(1);
    Ok(Some(px.into_iter().step_by(step).collect()))
}

/// What a measurement from photographs came to.
#[derive(Debug)]
pub struct Measured {
    pub facade: Option<Facade>,
    /// The z14 cell the building's BAG record is filed under.
    pub cell: (i64, i64),
    /// Photographs it rests on.
    pub photos: usize,
}

/// Building `pand` near (`near`) measured from photographs of it: one a
/// person took (`own`, with what their phone said), and — `mapillary` —
/// Mapillary's street photographs of it, each counting the same. One
/// photograph's light is that afternoon's; several even it out.
pub fn measure_photos(
    site: &Path,
    pand: &str,
    near: (f64, f64),
    own: Option<(&[u8], Hint)>,
    mapillary: bool,
    log: &dyn Fn(String),
) -> Result<Measured, String> {
    let (target, cell) = target(site, pand, near)?;
    let scene = Scene::load(site, pand, &target);
    let mut shots = Vec::new();
    if let Some((bytes, hint)) = own {
        shots.push(own_shot(&scene, bytes, hint, log)?);
    }
    if mapillary {
        match super::mapillary::photos_of(&target.ring, target.facts.year) {
            Ok(found) => {
                log(format!("{} Mapillary photographs face it", found.len()));
                for m in found {
                    let [e, n] = scene.local(m.lat, m.lon);
                    let (yaw, pitch, roll) = m.angles();
                    shots.push(Shot {
                        name: format!("mapillary-{}", m.id),
                        img: m.img,
                        start: Start::Known(Pose {
                            e,
                            n,
                            yaw,
                            pitch,
                            roll,
                            focal: m.focal,
                        }),
                    });
                }
            }
            // Without them the person's own photograph still counts.
            Err(e) if own.is_some() => log(format!("Mapillary: {e}")),
            Err(e) => return Err(e),
        }
    }
    let mut pool = Vec::new();
    let mut photos = 0;
    let mut kinds = (false, false);
    for shot in &shots {
        match wall_of(&scene, shot, log) {
            Ok(Some(px)) => {
                pool.extend(px);
                photos += 1;
                if shot.name.starts_with("mapillary") {
                    kinds.1 = true;
                } else {
                    kinds.0 = true;
                }
            }
            Ok(None) => {}
            Err(e) => log(format!("{}: {e}", shot.name)),
        }
    }
    let source = match kinds {
        (true, true) => "photo+mapillary",
        (true, false) => "photo",
        _ => "mapillary",
    };
    let facade = (photos > 0)
        .then(|| measure(&pool, pool.len(), &target.facts))
        .flatten()
        .map(|f| Facade {
            s: Some(source.into()),
            ..f
        });
    log(format!("{pand}: {photos} photographs: {facade:?}"));
    Ok(Measured {
        facade,
        cell,
        photos,
    })
}

/// A person's photograph as a [`Shot`]: decoded upright, its search started
/// from its own EXIF where it has a fix, else from the phone's.
fn own_shot(scene: &Scene, bytes: &[u8], hint: Hint, log: &dyn Fn(String)) -> Result<Shot, String> {
    let (img, exif) = decode(bytes)?;
    let (lat, lng, accuracy) =
        match (exif.lat, exif.lng, hint.lat, hint.lng) {
            // A camera's own fix is as good as the phone's, and taken when the
            // photograph was.
            (Some(la), Some(lo), _, _) => (la, lo, 10.0),
            (_, _, Some(la), Some(lo)) => (la, lo, hint.accuracy_m.unwrap_or(10.0)),
            _ => return Err(
                "The photo has no location: take it with location on, or send it from your phone"
                    .into(),
            ),
        };
    let at = scene.local(lat, lng);
    let away = (at[0] - scene.centre[0]).hypot(at[1] - scene.centre[1]);
    if away > 200.0 {
        return Err(format!(
            "The photo was taken {away:.0} m from this building; send one taken in front of it"
        ));
    }
    let focal = exif.focal35.map_or(PHONE_FOCAL, |f| f / 36.0);
    let heading = exif.heading.or(hint.heading);
    log(format!(
        "own photograph {}×{}, from {lat:.6},{lng:.6} ±{accuracy:.0} m, heading {heading:?}, focal {focal:.2}",
        img.width(),
        img.height()
    ));
    Ok(Shot {
        name: "own".into(),
        img,
        start: Start::Search(Prior {
            at,
            radius: (accuracy * 1.5).clamp(6.0, 25.0),
            heading,
            focal,
        }),
    })
}

/// The white of a wall's own photograph: the brightest few of its colourless
/// pixels, in linear light. `None` with too few, or none much brighter than
/// the wall — nothing white in it to go by.
fn white_of(px: &[[u8; 3]]) -> Option<[f64; 3]> {
    use super::facades::srgb_to_linear;
    let luma = |p: &[u8; 3]| {
        0.2126 * f64::from(p[0]) + 0.7152 * f64::from(p[1]) + 0.0722 * f64::from(p[2])
    };
    let mut grey: Vec<[u8; 3]> = px
        .iter()
        .copied()
        .filter(|p| {
            let (hi, lo) = (p[0].max(p[1]).max(p[2]), p[0].min(p[1]).min(p[2]));
            hi > 60 && f64::from(hi - lo) / f64::from(hi) < 0.12
        })
        .collect();
    if grey.len() < 200 {
        return None;
    }
    grey.sort_by(|a, b| luma(a).total_cmp(&luma(b)));
    let top = &grey[grey.len() * 95 / 100..];
    let mut all: Vec<f64> = px.iter().map(luma).collect();
    all.sort_by(f64::total_cmp);
    let median = all[all.len() / 2];
    let white = top.iter().map(luma).sum::<f64>() / top.len() as f64;
    if white < median * 1.3 {
        return None;
    }
    let n = top.len() as f64;
    Some([0, 1, 2].map(|c| {
        top.iter()
            .map(|p| srgb_to_linear(f64::from(p[c])))
            .sum::<f64>()
            / n
    }))
}

/// A pixel as it would read if `white` were fresh white render
/// ([`super::facades`]' white albedo).
fn under(p: [u8; 3], white: [f64; 3]) -> [u8; 3] {
    use super::facades::{linear_to_srgb, srgb_to_linear};
    [0, 1, 2].map(|c| linear_to_srgb(srgb_to_linear(f64::from(p[c])) / white[c].max(1e-3) * 0.8))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A street of two boxes, 10 m and 20 m high, side by side 20 m north of
    /// the origin, the taller one the target.
    fn street() -> Scene {
        let mut tris = Vec::new();
        let boxed =
            |x0: f64, x1: f64, top: f64, own: bool, tris: &mut Vec<([[f64; 3]; 3], bool)>| {
                let (y0, y1) = (20.0, 30.0);
                let c = [[x0, y0], [x1, y0], [x1, y1], [x0, y1]];
                for i in 0..4 {
                    let (a, b) = (c[i], c[(i + 1) % 4]);
                    let q = [
                        [a[0], a[1], 0.0],
                        [b[0], b[1], 0.0],
                        [b[0], b[1], top],
                        [a[0], a[1], top],
                    ];
                    tris.push(([q[0], q[1], q[2]], own));
                    tris.push(([q[0], q[2], q[3]], own));
                }
                tris.push(([[x0, y0, top], [x1, y0, top], [x1, y1, top]], own));
                tris.push(([[x0, y0, top], [x1, y1, top], [x0, y1, top]], own));
            };
        boxed(-15.0, 0.0, 10.0, false, &mut tris);
        boxed(0.0, 15.0, 20.0, true, &mut tris);
        Scene {
            lat0: 53.0,
            lng0: 6.5,
            kx: 111_320.0 * 53f64.to_radians().cos(),
            tris,
            rings: vec![
                vec![[-15.0, 20.0], [0.0, 20.0], [0.0, 30.0], [-15.0, 30.0]],
                vec![[0.0, 20.0], [15.0, 20.0], [15.0, 30.0], [0.0, 30.0]],
            ],
            centre: [7.5, 25.0],
            eave: 20.0,
        }
    }

    #[test]
    fn a_wall_ahead_is_drawn_where_it_stands() {
        let scene = street();
        let pose = Pose {
            e: 0.0,
            n: 0.0,
            yaw: 0.0,
            pitch: 10.0,
            roll: 0.0,
            focal: 0.7,
        };
        let (what, up) = draw(&scene, &pose, 1000.0, 1000.0, 64);
        // Looking north along x = 0: the low building left, the target right.
        let at = |gx: usize, gy: usize| what[gy * 64 + gx];
        assert_eq!(at(16, 40), OTHER);
        assert_eq!(at(48, 40), TARGET);
        // Above the low one is sky well before it is above the high one.
        assert_eq!(at(16, 10), SKY);
        assert_eq!(at(48, 10), TARGET);
        // And down at the bottom, the street.
        assert_eq!(at(32, 63), GROUND);
        assert!(up[40 * 64 + 48] > 0.0 && up[40 * 64 + 48] < 20.0);
    }

    #[test]
    fn the_search_finds_the_pose_a_picture_was_drawn_from() {
        let scene = street();
        let truth = Pose {
            e: 3.0,
            n: 2.0,
            yaw: 8.0,
            pitch: 12.0,
            roll: 4.0,
            focal: 0.72,
        };
        let (what, _) = draw(&scene, &truth, 1200.0, 900.0, SCENE_OUT);
        // What the scene network would say, with a tree in front of the
        // lower building.
        let lab: Vec<u8> = what
            .iter()
            .enumerate()
            .map(|(i, &w)| match w {
                _ if (20..40).contains(&(i % SCENE_OUT)) && i / SCENE_OUT > 70 => seen::IN_FRONT,
                TARGET | OTHER => seen::BUILDING,
                SKY => seen::SKY,
                _ => seen::GROUND,
            })
            .collect();
        // A fix 6 m out, no compass.
        let (found, _) = search(
            &scene,
            &lab,
            1200.0,
            900.0,
            Prior {
                at: [-2.0, -2.0],
                radius: 10.0,
                heading: None,
                focal: 0.72,
            },
        )
        .unwrap();
        assert!(
            (found.e - truth.e).hypot(found.n - truth.n) < 3.0,
            "{found:?}"
        );
        assert!((found.yaw - truth.yaw).abs() < 6.0, "{found:?}");
        assert!((found.pitch - truth.pitch).abs() < 5.0, "{found:?}");
        assert!((found.roll - truth.roll).abs() < 3.0, "{found:?}");
    }

    #[test]
    fn exif_gps_and_focal_length_are_read() {
        // A little-endian TIFF: IFD0 with an Exif pointer and a GPS pointer.
        let mut t: Vec<u8> = b"II*\0".to_vec();
        t.extend(8u32.to_le_bytes());
        let entry = |t: &mut Vec<u8>, tag: u16, ty: u16, count: u32, value: u32| {
            t.extend(tag.to_le_bytes());
            t.extend(ty.to_le_bytes());
            t.extend(count.to_le_bytes());
            t.extend(value.to_le_bytes());
        };
        // IFD0 at 8: two entries → ends at 8 + 2 + 24 + 4 = 38.
        let exif_ifd = 38u32;
        // Exif IFD: one entry → 38 + 2 + 12 + 4 = 56.
        let gps_ifd = 56u32;
        // GPS IFD: four entries → 56 + 2 + 48 + 4 = 110; rationals after.
        let lat_at = 110u32;
        let lng_at = lat_at + 24;
        t.extend(2u16.to_le_bytes());
        entry(&mut t, 0x8769, 4, 1, exif_ifd);
        entry(&mut t, 0x8825, 4, 1, gps_ifd);
        t.extend(0u32.to_le_bytes());
        t.extend(1u16.to_le_bytes());
        entry(&mut t, 0xA405, 3, 1, 26);
        t.extend(0u32.to_le_bytes());
        t.extend(4u16.to_le_bytes());
        entry(&mut t, 1, 2, 2, u32::from(b'N'));
        entry(&mut t, 2, 5, 3, lat_at);
        entry(&mut t, 3, 2, 2, u32::from(b'E'));
        entry(&mut t, 4, 5, 3, lng_at);
        t.extend(0u32.to_le_bytes());
        for (n, d) in [(53, 1), (13, 1), (900, 100), (6, 1), (34, 1), (0, 1)] {
            t.extend((n as u32).to_le_bytes());
            t.extend((d as u32).to_le_bytes());
        }
        let mut raw = b"Exif\0\0".to_vec();
        raw.extend(&t);
        let e = read_exif(&raw);
        assert_eq!(e.focal35, Some(26.0));
        assert!((e.lat.unwrap() - (53.0 + 13.0 / 60.0 + 9.0 / 3600.0)).abs() < 1e-9);
        assert!((e.lng.unwrap() - (6.0 + 34.0 / 60.0)).abs() < 1e-9);
        assert_eq!(read_exif(b"nonsense"), Exif::default());
    }
}
