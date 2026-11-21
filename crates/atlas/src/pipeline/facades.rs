//! Wall colour and material, measured from a municipality's textured 3D city
//! model (`facades/14/{x}/{y}.json`, `cartopolis_geo::formats::facades`).
//!
//! 3DBAG gives a building's shape and nothing about its walls, and no open
//! national source photographs Dutch facades. Groningen's own city model does:
//! the gemeente had its buildings textured from oblique aerial photographs and
//! publishes them as 3D Tiles behind its viewer (3dgroningen.nl, layer *Panden -
//! Getextureerd*), every building carrying its BAG pand identificatie. This
//! reads those textures and keeps two facts per building — a colour and a
//! material — never the picture, for the reasons the roofs give
//! (`roofs.rs`): the photograph's sun is not the engine's, and a photo wall
//! beside a lit one reads as two games.
//!
//! **Much of a wall's texture is not that wall.** Where the camera's view of a
//! wall was blocked — by the building across a narrow street, by a lower roof in
//! front — the model still textures it, with whatever the photograph showed
//! there: a roof, the street, a crown. Measured over 30 walls in the centre on
//! 2026-11-18, about one in three showed a clean facade. And on the clean ones
//! brick, white frames, stone sills, windows and shadow are one mixture: a
//! median of the texels came out grey for every brick wall, and the largest
//! colour cluster came out as the shadow. So the texels are not trusted until a
//! network has said which of them are wall.
//!
//! **The network** is a SegFormer-B0 fine-tuned on the CMP facade database
//! (`Xpitfire/segformer-finetuned-segments-cmp-facade` on Hugging Face, MIT),
//! exported to ONNX at a fixed 512² input and run by tract
//! (`crates/atlas/models/facade_segformer_b0.onnx`). It labels every pixel of a
//! rectified facade photograph as facade, window, door, cornice, sill and so
//! on. It was trained on street photographs, not on oblique aerials; on the
//! city model's clean walls it finds the windows well, and on the walls that
//! are really a roof it says "facade" too — which is why a wall also needs
//! windows to count. At 256² its windows agreed with the 512² ones at an IoU
//! of 0.25, so it runs at the size it was trained at.
//!
//! Per run:
//!
//!   1. Walk the tileset to the leaves over the bbox at [`LEVEL_ERROR`] (~0.1 m
//!      a texel), accumulating each tile's transform to ECEF.
//!   2. Per leaf, read the GLB: the opaque triangles whose normal is horizontal
//!      are walls, grouped per building into planes, and each plane is
//!      rectified — sampled from its texture onto a grid [`RES`] metres a
//!      pixel along and up the wall.
//!   3. Each building's [`WALLS_PER_BUILDING`] largest planes go through the
//!      network. A plane counts when the network calls enough of it facade and
//!      enough of it window ([`Verdict::counts`]).
//!   4. The facade-labelled pixels of the planes that count give the building's
//!      colour (a trimmed median) and its material (a vote).
//!
//! The leaves are fetched and never kept: the city at this level is ~22 GB,
//! which is a source cache nobody wants for a month.

use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

use cartopolis_geo::formats::coverage::Manifest;
use cartopolis_geo::formats::facades::{Cell as FacadeCell, Facade, Material, RoofFace, Windows};
use serde_json::Value;

use super::http::{cached, evict, fetch};
use super::model_roofs;
use super::rd::wgs84_bbox;
use super::roofs::lod22_cells;
use super::tile::{cell_path, sorted_cells, CACHE_ZOOM};
use super::{in_order, Count, Ctx, DatasetResult, Outcome, PipelineError, Progress};

/// The textured buildings of Groningen's city model.
const TILESET: &str = "https://www.3dgroningen.nl/datasource-data/open/\
                       5801f042-5f31-4bfb-af91-a63e79e925e7/tileset.json";

const ATTRIBUTION: &str = "Gevels gemeten uit het 3D-stadsmodel © Gemeente Groningen \
                           (3dgroningen.nl); building geometry © 3DBAG by TU Delft, CC BY 4.0";

/// The geometric error of the level read: 1.5 is the tileset's z18, ~0.1 m a
/// texel, where a window is ten pixels wide. At z17 (3.0) it is five, and the
/// network was trained on windows tens of pixels wide.
const LEVEL_ERROR: f64 = 1.6;

/// The model's host answered 502 to sixteen requests at once (2026-11-18), so
/// this asks it at most two at a time whatever atlas's upstream setting is.
const MAX_FETCHES: usize = 2;

/// A triangle whose normal is within this of horizontal (|n·up|) is a wall.
const WALL_UP: f64 = 0.15;

/// Metres a rectified pixel spans, along and up the wall: the texture's own.
const RES: f64 = 0.1;

/// The smallest plane worth a look, in metres either way: below this a wall is
/// a dormer cheek or a chimney.
const MIN_WALL_M: f64 = 3.0;

/// A plane longer than this many rectified pixels either way is sampled
/// coarser, so a 200 m block is not a 2,000-pixel image the network squashes
/// to 512 anyway.
const MAX_WALL_PX: f64 = 1024.0;

/// Planes per building sent through the network, largest first. Each costs
/// ~0.4 s of CPU; two is most of what a building shows the street.
const WALLS_PER_BUILDING: usize = 4;

/// Texels a building keeps at most: a stride sample, so a block of flats does
/// not hold a hundred thousand of them through the whole run.
const SAMPLE_CAP: usize = 4096;

/// Facade texels a measurement needs (~3 m² of wall at z18).
const MIN_TEXELS: usize = 300;

/// The share of the facade texels dropped from the dark end — the shadow under
/// the eaves and a balcony — and from the bright end, glints.
const DARK_TRIM: f64 = 0.15;
const BRIGHT_TRIM: f64 = 0.05;

/// The network, exported once from PyTorch (the module note says which).
const MODEL: &[u8] = include_bytes!("../../models/facade_segformer_b0.onnx");
/// Its input edge, and the edge of the label grid it answers with.
const NET_IN: usize = 512;
pub(super) const NET_OUT: usize = 128;
/// The ImageNet normalisation it was trained with.
const MEAN: [f32; 3] = [0.485, 0.456, 0.406];
const STD: [f32; 3] = [0.229, 0.224, 0.225];

// ─── The tileset ─────────────────────────────────────────────────────────────

/// A column-major 4×4, as 3D Tiles writes `transform`.
type Mat4 = [f64; 16];

const IDENTITY: Mat4 = [
    1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
];

fn mul(a: &Mat4, b: &Mat4) -> Mat4 {
    let mut r = [0.0; 16];
    for c in 0..4 {
        for row in 0..4 {
            r[c * 4 + row] = (0..4).map(|k| a[k * 4 + row] * b[c * 4 + k]).sum();
        }
    }
    r
}

/// One tile whose content is read: where it is, and how its glTF reaches ECEF.
#[derive(Clone, Debug)]
struct Leaf {
    url: String,
    m: Mat4,
    /// `[west, south, east, north]` in radians.
    region: [f64; 4],
}

/// `rel` resolved against the directory of `base`.
fn join(base: &str, rel: &str) -> String {
    if rel.starts_with("http://") || rel.starts_with("https://") {
        return rel.to_string();
    }
    let (origin, path) = match base
        .find("://")
        .and_then(|i| base[i + 3..].find('/').map(|j| i + 3 + j))
    {
        Some(i) => (&base[..i], &base[i..]),
        None => (base, "/"),
    };
    // The base's own directory, then the relative path, through one
    // normaliser: a base can carry `..` itself, since it is how the tileset
    // writes every child path.
    let dir = path.rsplit_once('/').map_or("", |(d, _)| d);
    let mut parts: Vec<&str> = Vec::new();
    for seg in dir.split('/').chain(rel.split('/')) {
        match seg {
            "." | "" => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    format!("{origin}/{}", parts.join("/"))
}

fn region_of(node: &Value) -> Option<[f64; 4]> {
    let r = node["boundingVolume"]["region"].as_array()?;
    Some([
        r.first()?.as_f64()?,
        r.get(1)?.as_f64()?,
        r.get(2)?.as_f64()?,
        r.get(3)?.as_f64()?,
    ])
}

fn overlaps(a: [f64; 4], b: [f64; 4]) -> bool {
    !(a[2] < b[0] || a[0] > b[2] || a[3] < b[1] || a[1] > b[3])
}

/// A GET of the model's host, retried as the other sources are, gunzipped when
/// the body is a gzip file (the tileset JSON is served that way).
fn get_model(ctx: &Ctx, url: &str, keep: bool) -> Outcome<Vec<u8>> {
    let mut last = String::new();
    for attempt in 0..4u64 {
        ctx.check()?;
        let got = if keep {
            cached(ctx, url, || fetch(url, Duration::from_secs(120)))
        } else {
            fetch(url, Duration::from_secs(120))
        };
        match got.and_then(|b| gunzip_if(&b)) {
            Ok(b) => return Ok(b),
            Err(e) => {
                evict(ctx, url);
                last = e;
                if attempt == 3 {
                    break;
                }
                ctx.sleep(Duration::from_secs(3 * (attempt + 1)))?;
            }
        }
    }
    Err(PipelineError::Failed(format!("{url}: {last}")))
}

fn gunzip_if(b: &[u8]) -> Result<Vec<u8>, String> {
    use std::io::Read;
    if b.starts_with(&[0x1f, 0x8b]) {
        let mut out = Vec::new();
        flate2::read::GzDecoder::new(b)
            .read_to_end(&mut out)
            .map_err(|e| format!("gunzip: {e}"))?;
        Ok(out)
    } else {
        Ok(b.to_vec())
    }
}

/// Every leaf over `bbox` (radians), in the order the tree lists them.
fn leaves(ctx: &Ctx, bbox: [f64; 4]) -> Outcome<Vec<Leaf>> {
    let mut out = Vec::new();
    tileset(ctx, TILESET, &IDENTITY, bbox, &mut out)?;
    Ok(out)
}

fn tileset(ctx: &Ctx, url: &str, m: &Mat4, bbox: [f64; 4], out: &mut Vec<Leaf>) -> Outcome<()> {
    let body = get_model(ctx, url, true)?;
    let doc: Value = serde_json::from_slice(&body).map_err(|e| {
        evict(ctx, url);
        PipelineError::Failed(format!("{url}: {e}"))
    })?;
    node(ctx, &doc["root"], url, m, bbox, out)
}

fn node(
    ctx: &Ctx,
    n: &Value,
    base: &str,
    parent: &Mat4,
    bbox: [f64; 4],
    out: &mut Vec<Leaf>,
) -> Outcome<()> {
    let m = match n["transform"].as_array() {
        Some(t) if t.len() == 16 => {
            let mut local = [0.0; 16];
            for (i, v) in t.iter().enumerate() {
                local[i] = v.as_f64().unwrap_or(0.0);
            }
            mul(parent, &local)
        }
        _ => *parent,
    };
    let Some(region) = region_of(n) else {
        return Ok(());
    };
    if !overlaps(region, bbox) {
        return Ok(());
    }
    let kids = n["children"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default();
    let fine = n["geometricError"].as_f64().unwrap_or(f64::MAX) <= LEVEL_ERROR;
    if let Some(uri) = n["content"]["uri"]
        .as_str()
        .or_else(|| n["content"]["url"].as_str())
    {
        let target = join(base, uri);
        if uri.ends_with(".json") {
            tileset(ctx, &target, &m, bbox, out)?;
        } else if fine || kids.is_empty() {
            out.push(Leaf {
                url: target,
                m,
                region,
            });
            return Ok(());
        }
    }
    for k in kids {
        node(ctx, k, base, &m, bbox, out)?;
    }
    Ok(())
}

// ─── One GLB ─────────────────────────────────────────────────────────────────

/// The parts of a GLB this reads.
struct Glb {
    json: Value,
    bin: Vec<u8>,
}

impl Glb {
    fn parse(b: &[u8]) -> Result<Self, String> {
        let u32_at = |i: usize| -> Result<u32, String> {
            b.get(i..i + 4)
                .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
                .ok_or_else(|| "truncated GLB".to_string())
        };
        if b.get(..4) != Some(b"glTF") {
            return Err("not a GLB".into());
        }
        let jl = u32_at(12)? as usize;
        let json: Value = serde_json::from_slice(b.get(20..20 + jl).ok_or("truncated GLB JSON")?)
            .map_err(|e| format!("GLB JSON: {e}"))?;
        let bin_at = 20 + jl;
        let bin = if b.len() >= bin_at + 8 {
            let bl = u32_at(bin_at)? as usize;
            b.get(bin_at + 8..bin_at + 8 + bl)
                .ok_or("truncated GLB BIN")?
                .to_vec()
        } else {
            Vec::new()
        };
        Ok(Self { json, bin })
    }

    fn view(&self, i: usize) -> Option<&[u8]> {
        let v = &self.json["bufferViews"][i];
        let off = v["byteOffset"].as_u64().unwrap_or(0) as usize;
        let len = v["byteLength"].as_u64()? as usize;
        self.bin.get(off..off + len)
    }

    /// An accessor's values, every component widened to `f64`.
    fn accessor(&self, i: usize) -> Option<(Vec<f64>, usize)> {
        let a = &self.json["accessors"][i];
        let n = match a["type"].as_str()? {
            "SCALAR" => 1,
            "VEC2" => 2,
            "VEC3" => 3,
            "VEC4" => 4,
            _ => return None,
        };
        let ct = a["componentType"].as_u64()?;
        let size = match ct {
            5126 | 5125 => 4,
            5123 | 5122 => 2,
            5121 | 5120 => 1,
            _ => return None,
        };
        let count = a["count"].as_u64()? as usize;
        let bv = &self.json["bufferViews"][a["bufferView"].as_u64()? as usize];
        let base = bv["byteOffset"].as_u64().unwrap_or(0) as usize
            + a["byteOffset"].as_u64().unwrap_or(0) as usize;
        let stride = bv["byteStride"].as_u64().map_or(size * n, |s| s as usize);
        let mut out = Vec::with_capacity(count * n);
        for k in 0..count {
            for c in 0..n {
                let p = base + k * stride + c * size;
                let s = self.bin.get(p..p + size)?;
                out.push(match ct {
                    5126 => f64::from(f32::from_le_bytes([s[0], s[1], s[2], s[3]])),
                    5125 => f64::from(u32::from_le_bytes([s[0], s[1], s[2], s[3]])),
                    5123 => f64::from(u16::from_le_bytes([s[0], s[1]])),
                    5122 => f64::from(i16::from_le_bytes([s[0], s[1]])),
                    5121 => f64::from(s[0]),
                    _ => f64::from(s[0] as i8),
                });
            }
        }
        Some((out, n))
    }

    /// The first property table's `id` column: one string per feature.
    fn feature_ids(&self) -> Vec<String> {
        let table = &self.json["extensions"]["EXT_structural_metadata"]["propertyTables"][0];
        let count = table["count"].as_u64().unwrap_or(0) as usize;
        let prop = &table["properties"]["id"];
        let (Some(vals), Some(offs)) = (
            prop["values"].as_u64().and_then(|i| self.view(i as usize)),
            prop["stringOffsets"]
                .as_u64()
                .and_then(|i| self.view(i as usize)),
        ) else {
            return Vec::new();
        };
        let off = |i: usize| -> Option<usize> {
            offs.get(i * 4..i * 4 + 4)
                .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]) as usize)
        };
        (0..count)
            .map(|i| {
                off(i)
                    .zip(off(i + 1))
                    .and_then(|(a, b)| vals.get(a..b))
                    .map(|s| String::from_utf8_lossy(s).into_owned())
                    .unwrap_or_default()
            })
            .collect()
    }
}

/// The BAG pand identificatie in a feature id: `id_bag_0014100010904448`.
fn pand_of(id: &str) -> Option<&str> {
    let p = id.strip_prefix("id_bag_").unwrap_or(id);
    (p.len() == 16 && p.bytes().all(|b| b.is_ascii_digit())).then_some(p)
}

/// ECEF up, east and north at a region's centre.
fn frame(region: [f64; 4]) -> [[f64; 3]; 3] {
    let lon = (region[0] + region[2]) / 2.0;
    let lat = (region[1] + region[3]) / 2.0;
    let up = [lat.cos() * lon.cos(), lat.cos() * lon.sin(), lat.sin()];
    let east = [-lon.sin(), lon.cos(), 0.0];
    let north = [-lat.sin() * lon.cos(), -lat.sin() * lon.sin(), lat.cos()];
    [up, east, north]
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

// ─── Walls, rectified ────────────────────────────────────────────────────────

/// One plane of one building's walls, as a photograph taken square-on.
struct WallImage {
    pand: String,
    /// `w`×`h`, row 0 at the top of the wall.
    img: image::RgbImage,
    /// Which pixels the wall's own triangles cover.
    mask: Vec<bool>,
    covered: usize,
    /// Its horizontal outward normal, (east, north).
    facing: [f64; 2],
    /// Metres one pixel spans.
    res: f64,
    /// Which pixels the flight's cameras could see ([`mark_hidden`]).
    visible: Vec<bool>,
    at: WallAt,
    /// Its leaf's ground, in the leaf's height, and its left and right ends
    /// at its foot as (latitude°, longitude°) — set by [`read_leaf`].
    ground: f64,
    ends: [(f64, f64); 2],
}

/// A wall triangle in the leaf's east-north-up metres, with its texture
/// coordinates and which texture they are in.
struct WallTri {
    p: [[f64; 3]; 3],
    uv: [[f64; 2]; 3],
    tex: usize,
}

/// The planes of one building's walls, keyed by facing (4° steps) and distance
/// from the leaf's origin along that facing (0.5 m steps): coplanar triangles
/// share a key, and a terrace's walls in one line are one plane.
type Planes = HashMap<(String, i64, i64), (Vec<WallTri>, [f64; 2])>;

fn edge(a: [f64; 2], b: [f64; 2], c: [f64; 2]) -> f64 {
    (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0])
}

/// Sample one plane onto a grid along and up it. `normal` is the plane's
/// horizontal facing (east, north).
fn rectify(
    pand: &str,
    tris: &[WallTri],
    normal: [f64; 2],
    textures: &[Option<image::RgbImage>],
) -> Option<WallImage> {
    // Along the wall as somebody standing in front of it reads it, left to
    // right: facing north, they look south, so "along" runs east to west.
    let t = [-normal[1], normal[0]];
    let along = |p: [f64; 3]| p[0] * t[0] + p[1] * t[1];
    let (mut s0, mut s1, mut h0, mut h1) = (f64::MAX, f64::MIN, f64::MAX, f64::MIN);
    for tri in tris {
        for p in tri.p {
            s0 = s0.min(along(p));
            s1 = s1.max(along(p));
            h0 = h0.min(p[2]);
            h1 = h1.max(p[2]);
        }
    }
    if s1 - s0 < MIN_WALL_M || h1 - h0 < MIN_WALL_M {
        return None;
    }
    let res = RES.max((s1 - s0).max(h1 - h0) / MAX_WALL_PX);
    let (w, h) = (
        ((s1 - s0) / res).ceil() as u32,
        ((h1 - h0) / res).ceil() as u32,
    );
    let mut img = image::RgbImage::new(w, h);
    let mut mask = vec![false; (w * h) as usize];
    let mut covered = 0usize;
    for tri in tris {
        let Some(tex) = textures.get(tri.tex).and_then(Option::as_ref) else {
            continue;
        };
        let (tw, th) = tex.dimensions();
        let q = tri.p.map(|p| [(along(p) - s0) / res, (h1 - p[2]) / res]);
        let area = edge(q[0], q[1], q[2]);
        if area.abs() < 1e-9 {
            continue;
        }
        let x0 = q
            .iter()
            .map(|p| p[0])
            .fold(f64::MAX, f64::min)
            .floor()
            .max(0.0) as u32;
        let x1 = (q
            .iter()
            .map(|p| p[0])
            .fold(f64::MIN, f64::max)
            .ceil()
            .max(0.0) as u32)
            .min(w);
        let y0 = q
            .iter()
            .map(|p| p[1])
            .fold(f64::MAX, f64::min)
            .floor()
            .max(0.0) as u32;
        let y1 = (q
            .iter()
            .map(|p| p[1])
            .fold(f64::MIN, f64::max)
            .ceil()
            .max(0.0) as u32)
            .min(h);
        for y in y0..y1 {
            for x in x0..x1 {
                let c = [f64::from(x) + 0.5, f64::from(y) + 0.5];
                let b0 = edge(q[1], q[2], c) / area;
                let b1 = edge(q[2], q[0], c) / area;
                let b2 = 1.0 - b0 - b1;
                if b0 < -1e-3 || b1 < -1e-3 || b2 < -1e-3 {
                    continue;
                }
                let u = b0 * tri.uv[0][0] + b1 * tri.uv[1][0] + b2 * tri.uv[2][0];
                let v = b0 * tri.uv[0][1] + b1 * tri.uv[1][1] + b2 * tri.uv[2][1];
                let tx = ((u * f64::from(tw)).floor().max(0.0) as u32).min(tw - 1);
                let ty = ((v * f64::from(th)).floor().max(0.0) as u32).min(th - 1);
                let i = (y * w + x) as usize;
                if !mask[i] {
                    covered += 1;
                }
                mask[i] = true;
                img.put_pixel(x, y, *tex.get_pixel(tx, ty));
            }
        }
    }
    let offset = tris
        .iter()
        .flat_map(|t| t.p)
        .map(|p| p[0] * normal[0] + p[1] * normal[1])
        .sum::<f64>()
        / (tris.len() * 3) as f64;
    let visible = vec![true; mask.len()];
    (covered > 0).then(|| WallImage {
        pand: pand.to_string(),
        img,
        mask,
        covered,
        facing: normal,
        res,
        visible,
        ground: 0.0,
        ends: [(0.0, 0.0); 2],
        at: WallAt {
            s0,
            top: h1,
            offset,
        },
    })
}

/// Where a rectified wall stands in its leaf's east-north-up frame: pixel
/// (x, y) is `along` = `s0 + (x + ½)·res` and height `top − (y + ½)·res`, on
/// the vertical plane `offset` metres out along its facing.
#[derive(Clone, Copy, Debug)]
struct WallAt {
    s0: f64,
    top: f64,
    offset: f64,
}

// ─── What the oblique camera could see ───────────────────────────────────────
//
// The city model textures a wall from whichever oblique photograph shows it
// best, and where none does — the building across a narrow street stands in
// the way, or a lower roof in front — it textures it anyway, with the roof or
// the street the photograph shows there. The 2026-11-18 runs showed that the
// network cannot tell those from facades: tiles and solar panels come out as
// "facade" with "windows" in rows. Our own LoD2.2 shells can: each wall pixel
// is tested against the buildings around it along the camera's line of sight,
// and only what the camera could see is judged.

/// The oblique cameras' elevation. Dutch oblique flights tilt ~45°; the
/// gemeente's own was flown with Beeldmateriaal-type cameras (Kavel 10).
const CAMERA_TAN: f64 = 1.0;
/// A wall pixel whose line of sight passes this far under a roof is hidden:
/// the shells and the model differ by a few decimetres.
const HIDE_MARGIN_M: f64 = 0.5;
/// How far out a line of sight is followed.
const SIGHT_M: f64 = 80.0;
/// The roof heights of the run's buildings, metres above their own base, on
/// a 1 m grid in blocks of [`BLOCK`]².
struct Heights {
    lat0: f64,
    lng0: f64,
    /// Metres per degree of longitude and latitude at `lat0`.
    kx: f64,
    ky: f64,
    blocks: HashMap<(i64, i64), Vec<f32>>,
}

const BLOCK: i64 = 64;

impl Heights {
    fn new(lat0: f64, lng0: f64) -> Self {
        Self {
            lat0,
            lng0,
            kx: 111_320.0 * lat0.to_radians().cos(),
            ky: 110_540.0,
            blocks: HashMap::new(),
        }
    }

    fn xy(&self, lat: f64, lng: f64) -> [f64; 2] {
        [(lng - self.lng0) * self.kx, (lat - self.lat0) * self.ky]
    }

    fn raise(&mut self, x: i64, y: i64, h: f32) {
        let (bx, by) = (x.div_euclid(BLOCK), y.div_euclid(BLOCK));
        let b = self
            .blocks
            .entry((bx, by))
            .or_insert_with(|| vec![0.0; (BLOCK * BLOCK) as usize]);
        let i = (y.rem_euclid(BLOCK) * BLOCK + x.rem_euclid(BLOCK)) as usize;
        b[i] = b[i].max(h);
    }

    /// The tallest roof over the 1 m cell holding (x, y), 0 off every building.
    fn at(&self, x: f64, y: f64) -> f32 {
        let (x, y) = (x.floor() as i64, y.floor() as i64);
        self.blocks
            .get(&(x.div_euclid(BLOCK), y.div_euclid(BLOCK)))
            .map_or(0.0, |b| {
                b[(y.rem_euclid(BLOCK) * BLOCK + x.rem_euclid(BLOCK)) as usize]
            })
    }

    /// Every roof triangle of the cells, rasterised at its own height.
    fn of_cells(cells: &[(super::tile::Cell, cartopolis_geo::lod22_cell::Cell)]) -> Self {
        let centre = cells
            .first()
            .and_then(|(_, c)| c.elements.first())
            .and_then(|e| e.p.first())
            .map_or((53.2, 6.56), |p| (p.0, p.1));
        let mut hf = Self::new(centre.0, centre.1);
        for (_, cell) in cells {
            for e in &cell.elements {
                let pts: Vec<([f64; 2], f64)> =
                    e.p.iter()
                        .map(|&(la, lo, h)| (hf.xy(la, lo), f64::from(h)))
                        .collect();
                for &(a, b, c, roof) in &e.t {
                    if roof != 1 {
                        continue;
                    }
                    let (Some(a), Some(b), Some(c)) = (
                        pts.get(a as usize),
                        pts.get(b as usize),
                        pts.get(c as usize),
                    ) else {
                        continue;
                    };
                    let q = [a.0, b.0, c.0];
                    let area = edge(q[0], q[1], q[2]);
                    if area.abs() < 1e-9 {
                        continue;
                    }
                    let lo =
                        |k: usize| q.iter().map(|p| p[k]).fold(f64::MAX, f64::min).floor() as i64;
                    let hi =
                        |k: usize| q.iter().map(|p| p[k]).fold(f64::MIN, f64::max).ceil() as i64;
                    for y in lo(1)..hi(1) {
                        for x in lo(0)..hi(0) {
                            let p = [x as f64 + 0.5, y as f64 + 0.5];
                            let b0 = edge(q[1], q[2], p) / area;
                            let b1 = edge(q[2], q[0], p) / area;
                            let b2 = 1.0 - b0 - b1;
                            if b0 < -0.05 || b1 < -0.05 || b2 < -0.05 {
                                continue;
                            }
                            hf.raise(x, y, (b0 * a.1 + b1 * b.1 + b2 * c.1) as f32);
                        }
                    }
                }
            }
        }
        hf
    }

    /// Whether a camera looking down at [`CAMERA_TAN`] from the horizontal
    /// direction `toward` (unit, x east y north) sees the point (x, y) at
    /// `z` metres above the ground.
    fn sees(&self, x: f64, y: f64, z: f64, toward: [f64; 2]) -> bool {
        // From 1.5 m out: the model's walls and the shells' differ by up to a
        // metre, and the wall's own roof must not hide it.
        let mut t = 1.5;
        while t < SIGHT_M {
            let ray = z + t * CAMERA_TAN;
            let roof = f64::from(self.at(x + toward[0] * t, y + toward[1] * t));
            if roof > ray + HIDE_MARGIN_M {
                return false;
            }
            t += 0.5;
        }
        true
    }
}

/// WGS84 ECEF to (latitude°, longitude°).
fn geodetic(e: [f64; 3]) -> (f64, f64) {
    const A: f64 = 6_378_137.0;
    const E2: f64 = 6.694_379_990_14e-3;
    let lon = e[1].atan2(e[0]);
    let p = (e[0] * e[0] + e[1] * e[1]).sqrt();
    let mut lat = e[2].atan2(p * (1.0 - E2));
    for _ in 0..5 {
        let n = A / (1.0 - E2 * lat.sin().powi(2)).sqrt();
        let h = p / lat.cos() - n;
        lat = e[2].atan2(p * (1.0 - E2 * n / (n + h)));
    }
    (lat.to_degrees(), lon.to_degrees())
}

/// The cardinal directions a camera of the flight can have looked from at a
/// wall facing `facing`: those within 60° of its facing. A wall is hidden at
/// a point when any of them is blocked there, since which photograph the model
/// took its texture from is not recorded.
fn cameras(facing: [f64; 2]) -> Vec<[f64; 2]> {
    [[0.0, 1.0], [1.0, 0.0], [0.0, -1.0], [-1.0, 0.0]]
        .into_iter()
        .filter(|d| d[0] * facing[0] + d[1] * facing[1] >= 0.5)
        .collect()
}

/// Mark the pixels of `wall` the flight's cameras could not see. `to_world`
/// takes a point of the leaf's east-north-up frame to (x, y) metres of
/// `heights`, and `ground` is the leaf's ground in that frame's height.
fn mark_hidden(
    wall: &mut WallImage,
    heights: &Heights,
    ground: f64,
    to_world: &dyn Fn([f64; 3]) -> [f64; 2],
) {
    let (w, h) = wall.img.dimensions();
    let n = wall.facing;
    let t = [-n[1], n[0]];
    let dirs = cameras(n);
    if dirs.is_empty() {
        return;
    }
    // Tested every half metre and spread to the pixels between: a roof edge
    // does not need finding to the texel.
    let step = ((0.5 / wall.res).round() as u32).max(1);
    let mut y = 0;
    while y < h {
        let mut x = 0;
        while x < w {
            let s = wall.at.s0 + (f64::from(x) + 0.5 * f64::from(step)) * wall.res;
            let up = wall.at.top - (f64::from(y) + 0.5 * f64::from(step)) * wall.res;
            // A hair out from the wall, so the wall's own roof edge is not in
            // the way of its own top row.
            let p = [
                t[0] * s + n[0] * (wall.at.offset + 0.3),
                t[1] * s + n[1] * (wall.at.offset + 0.3),
                up,
            ];
            let [wx, wy] = to_world(p);
            let seen = dirs.iter().all(|&d| heights.sees(wx, wy, up - ground, d));
            if !seen {
                for yy in y..(y + step).min(h) {
                    for xx in x..(x + step).min(w) {
                        wall.visible[(yy * w + xx) as usize] = false;
                    }
                }
            }
            x += step;
        }
        y += step;
    }
}

/// What one leaf gives: each building's [`WALLS_PER_BUILDING`] largest wall
/// planes, rectified, and a texel sample of each face of its roof.
struct LeafRead {
    walls: Vec<WallImage>,
    roofs: HashMap<(String, u8), model_roofs::FaceTexels>,
}

/// Read one leaf ([`LeafRead`]).
fn read_leaf(leaf: &Leaf, body: &[u8], heights: &Heights) -> Result<LeafRead, String> {
    let glb = Glb::parse(body)?;
    let ids = glb.feature_ids();
    let textures: Vec<Option<image::RgbImage>> = glb.json["images"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .map(|im| {
            let bytes = glb.view(im["bufferView"].as_u64()? as usize)?;
            image::load_from_memory(bytes).ok().map(|i| i.to_rgb8())
        })
        .collect();
    let [up, east, north] = frame(leaf.region);
    let m = leaf.m;
    // glTF is y-up; 3D Tiles turns it z-up before the tile's transform. Points
    // are taken relative to the tile's own translation, which is near them.
    let rotate = |v: [f64; 3]| -> [f64; 3] {
        let z = [v[0], -v[2], v[1]];
        [
            m[0] * z[0] + m[4] * z[1] + m[8] * z[2],
            m[1] * z[0] + m[5] * z[1] + m[9] * z[2],
            m[2] * z[0] + m[6] * z[1] + m[10] * z[2],
        ]
    };
    let enu = |v: [f64; 3]| -> [f64; 3] {
        let e = rotate(v);
        [dot(e, east), dot(e, north), dot(e, up)]
    };
    let mut planes: Planes = HashMap::new();
    let mut roof_planes: HashMap<(String, u8, i64, i64), (Vec<model_roofs::RoofTri>, [f64; 3])> =
        HashMap::new();
    let prims = glb.json["meshes"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .flat_map(|mesh| mesh["primitives"].as_array().cloned().unwrap_or_default());
    for p in prims {
        let Some(mat) = p["material"]
            .as_u64()
            .map(|i| &glb.json["materials"][i as usize])
        else {
            continue;
        };
        // The BLEND and MASK materials are trees and railings.
        if mat["alphaMode"].as_str().unwrap_or("OPAQUE") != "OPAQUE" {
            continue;
        }
        let Some(tex) = mat["pbrMetallicRoughness"]["baseColorTexture"]["index"]
            .as_u64()
            .and_then(|t| glb.json["textures"][t as usize]["source"].as_u64())
        else {
            continue;
        };
        let attr = |k: &str| {
            p["attributes"][k]
                .as_u64()
                .and_then(|i| glb.accessor(i as usize))
        };
        let (Some((pos, 3)), Some((normals, 3)), Some((uvs, 2)), Some((features, 1))) = (
            attr("POSITION"),
            attr("NORMAL"),
            attr("TEXCOORD_0"),
            attr("_FEATURE_ID_0"),
        ) else {
            continue;
        };
        let indices: Vec<usize> = match p["indices"].as_u64().and_then(|i| glb.accessor(i as usize))
        {
            Some((ix, 1)) => ix.iter().map(|&v| v as usize).collect(),
            _ => (0..pos.len() / 3).collect(),
        };
        for tri in indices.chunks_exact(3) {
            let [a, b, c] = [tri[0], tri[1], tri[2]];
            let top = a.max(b).max(c);
            if top * 3 + 2 >= pos.len()
                || top * 3 + 2 >= normals.len()
                || top * 2 + 1 >= uvs.len()
                || a >= features.len()
            {
                continue;
            }
            let n = rotate([normals[a * 3], normals[a * 3 + 1], normals[a * 3 + 2]]);
            let len = dot(n, n).sqrt();
            if len < 1e-6 {
                continue;
            }
            let Some(pand) = ids.get(features[a] as usize).and_then(|id| pand_of(id)) else {
                continue;
            };
            let n_up = dot(n, up) / len;
            // A roof: its texels go to its face's sample, from the texture
            // directly — a covering needs no rectifying.
            if n_up >= model_roofs::ROOF_UP {
                let normal = [dot(n, east) / len, dot(n, north) / len, n_up];
                let face = model_roofs::face_of(normal);
                let pts = [a, b, c].map(|i| enu([pos[i * 3], pos[i * 3 + 1], pos[i * 3 + 2]]));
                let offset = pts[0][0] * normal[0] + pts[0][1] * normal[1] + pts[0][2] * normal[2];
                // One plane: one facing, one pitch (5° steps), one offset.
                let key = (
                    pand.to_string(),
                    face,
                    (n_up.acos().to_degrees() / 5.0).round() as i64,
                    (offset / 0.5).round() as i64,
                );
                roof_planes
                    .entry(key)
                    .or_insert_with(|| (Vec::new(), normal))
                    .0
                    .push(model_roofs::RoofTri {
                        p: pts,
                        uv: [a, b, c].map(|i| [uvs[i * 2], uvs[i * 2 + 1]]),
                        tex: tex as usize,
                    });
                continue;
            }
            if n_up.abs() > WALL_UP {
                continue;
            }
            let (ne, nn) = (dot(n, east), dot(n, north));
            let h = (ne * ne + nn * nn).sqrt();
            let facing = [ne / h, nn / h];
            let pts = [a, b, c].map(|i| enu([pos[i * 3], pos[i * 3 + 1], pos[i * 3 + 2]]));
            let offset = pts[0][0] * facing[0] + pts[0][1] * facing[1];
            let az = facing[0].atan2(facing[1]).to_degrees();
            let key = (
                pand.to_string(),
                (az / 4.0).round() as i64,
                (offset / 0.5).round() as i64,
            );
            planes
                .entry(key)
                .or_insert_with(|| (Vec::new(), facing))
                .0
                .push(WallTri {
                    p: pts,
                    uv: [a, b, c].map(|i| [uvs[i * 2], uvs[i * 2 + 1]]),
                    tex: tex as usize,
                });
        }
    }
    // Each roof plane read square-on, into its face's sample.
    let mut roofs: HashMap<(String, u8), model_roofs::FaceTexels> = HashMap::new();
    for ((pand, face, _, _), (tris, normal)) in roof_planes {
        let t = model_roofs::read_plane(&tris, normal, &textures);
        if t.n > 0 {
            roofs.entry((pand, face)).or_default().merge(t);
        }
    }
    let mut by_pand: HashMap<String, Vec<WallImage>> = HashMap::new();
    for ((pand, _, _), (tris, facing)) in &planes {
        if let Some(w) = rectify(pand, tris, *facing, &textures) {
            by_pand.entry(pand.clone()).or_default().push(w);
        }
    }
    // The leaf's ground: its lowest wall foot. Groningen's centre is flat to a
    // metre or two, which is inside what the line-of-sight test can tell.
    let ground = planes
        .values()
        .flat_map(|(tris, _)| tris.iter().flat_map(|t| t.p.iter().map(|p| p[2])))
        .fold(f64::MAX, f64::min);
    let origin = [m[12], m[13], m[14]];
    let to_world = |p: [f64; 3]| -> [f64; 2] {
        let e = [0, 1, 2].map(|k| origin[k] + east[k] * p[0] + north[k] * p[1] + up[k] * p[2]);
        let (lat, lng) = geodetic(e);
        heights.xy(lat, lng)
    };
    let mut out = Vec::new();
    for (_, mut walls) in by_pand {
        walls.sort_by(|a, b| b.covered.cmp(&a.covered));
        walls.truncate(WALLS_PER_BUILDING);
        for w in &mut walls {
            mark_hidden(w, heights, ground, &to_world);
            // Its two ends at its foot, for the client to find the wall of its
            // own shell this one is.
            let n = w.facing;
            let t = [-n[1], n[0]];
            let width = f64::from(w.img.width()) * w.res;
            let end = |s: f64| {
                let p = [
                    t[0] * s + n[0] * w.at.offset,
                    t[1] * s + n[1] * w.at.offset,
                    ground,
                ];
                geodetic(
                    [0, 1, 2].map(|k| origin[k] + east[k] * p[0] + north[k] * p[1] + up[k] * p[2]),
                )
            };
            w.ground = ground;
            w.ends = [end(w.at.s0), end(w.at.s0 + width)];
        }
        out.extend(walls);
    }
    // In a fixed order, so a run's output does not depend on hash order.
    out.sort_by(|a, b| a.pand.cmp(&b.pand).then(b.covered.cmp(&a.covered)));
    Ok(LeafRead { walls: out, roofs })
}

// ─── The network ─────────────────────────────────────────────────────────────

/// The CMP classes the network answers with, by index.
pub(super) mod class {
    pub const UNKNOWN: u8 = 0;
    pub const BACKGROUND: u8 = 1;
    pub const FACADE: u8 = 2;
    pub const WINDOW: u8 = 3;
    pub const DOOR: u8 = 4;
    pub const CORNICE: u8 = 5;
    pub const SILL: u8 = 6;
    pub const BALCONY: u8 = 7;
    pub const BLIND: u8 = 8;
    pub const MOLDING: u8 = 9;
    pub const DECO: u8 = 10;
    pub const PILLAR: u8 = 11;
    pub const SHOP: u8 = 12;
}

pub(super) type Net = std::sync::Arc<tract_onnx::prelude::TypedRunnableModel>;

pub(super) fn load_net() -> Result<Net, String> {
    use tract_onnx::prelude::*;
    tract_onnx::onnx()
        .model_for_read(&mut std::io::Cursor::new(MODEL))
        .and_then(|m| m.into_optimized())
        .and_then(|m| m.into_runnable())
        .map_err(|e| format!("facade network: {e:#}"))
}

/// The network's label for every cell of a [`NET_OUT`]² grid over `img`
/// (stretched square, as it was trained).
pub(super) fn labels(net: &Net, img: &image::RgbImage) -> Result<Vec<u8>, String> {
    use tract_onnx::prelude::*;
    let sq = image::imageops::resize(
        img,
        NET_IN as u32,
        NET_IN as u32,
        image::imageops::FilterType::Triangle,
    );
    let input = tract_ndarray::Array4::from_shape_fn((1, 3, NET_IN, NET_IN), |(_, c, y, x)| {
        let v = f32::from(sq.get_pixel(x as u32, y as u32).0[c]) / 255.0;
        (v - MEAN[c]) / STD[c]
    });
    let out = net
        .run(tvec!(Tensor::from(input).into()))
        .map_err(|e| format!("facade network: {e:#}"))?;
    let logits = out[0]
        .to_plain_array_view::<f32>()
        .map_err(|e| format!("facade network: {e:#}"))?;
    let shape = logits.shape().to_vec();
    if shape.len() != 4 || shape[2] != NET_OUT || shape[3] != NET_OUT {
        return Err(format!("facade network answered {shape:?}"));
    }
    let classes = shape[1];
    let mut best = vec![0u8; NET_OUT * NET_OUT];
    for y in 0..NET_OUT {
        for x in 0..NET_OUT {
            let mut top = (0usize, f32::MIN);
            for k in 0..classes {
                let v = logits[[0, k, y, x]];
                if v > top.1 {
                    top = (k, v);
                }
            }
            best[y * NET_OUT + x] = top.0 as u8;
        }
    }
    Ok(best)
}

/// What the network made of one wall.
#[derive(Debug, Default, PartialEq)]
struct Verdict {
    /// Covered pixels.
    covered: usize,
    /// Of those: the wall itself and its trim, windows (and their blinds),
    /// doors and shopfronts, and what it called background.
    wall: usize,
    window: usize,
    door: usize,
    background: usize,
    /// Pixels the cameras could not see ([`mark_hidden`]), which are in none
    /// of the counts above.
    hidden: usize,
    /// The facade-labelled pixels themselves.
    facade_px: Vec<[u8; 3]>,
    /// The window-sized blobs of window, in metres from the wall's top left.
    panes: Vec<Pane>,
}

/// One window, as a box on the wall: centre and size in metres, x from the
/// wall's left as seen from in front, y down from its top.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Pane {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

/// The sizes a blob of "window" must have to be one: smaller is a glint or a
/// vent, larger a glass roof or a shopfront.
/// The least share of a wall that is window for it to be a facade.
const MIN_WINDOW_SHARE: f64 = 0.08;

const PANE_W: (f64, f64) = (0.4, 3.5);
const PANE_H: (f64, f64) = (0.5, 3.2);

/// Two panes whose centres are within this height of each other are on one
/// storey.
const ROW_TOLERANCE_M: f64 = 0.6;

impl Verdict {
    /// A wall the network saw as a facade with windows in it, the windows in
    /// storeys: at least two rows of two, or one row of four. A roof seen in a
    /// wall's texture comes back mostly "facade" too, and its skylights,
    /// dormers and solar panels as "window" — 3 of 9 walls passing the first
    /// two tests on the 2026-11-18 run were roofs — but those lie scattered
    /// down the slope, never in storeys. A blank party wall fails as well and
    /// is no loss: it is the street front that shows.
    fn counts(&self) -> bool {
        self.fails().is_none()
    }

    /// Why it does not count, for the run's tally of where buildings go.
    fn fails(&self) -> Option<&'static str> {
        let n = self.covered.max(1) as f64;
        // Most of it seen at all: a wall the cameras saw only the top of is
        // textured mostly with whatever stood in front.
        if self.hidden as f64 > n || self.covered < 1000 {
            Some("mostly hidden from the cameras")
        } else if self.background as f64 / n > 0.35 {
            Some("mostly background")
        } else if self.wall as f64 / n < 0.45 {
            Some("too little facade")
        // A real facade is a tenth to a third window; a roof in a wall's
        // texture with a dormer in it came in at 5 % (2026-11-19, pand
        // 0014100010923283 on the Grote Markt).
        } else if self.window as f64 / n < MIN_WINDOW_SHARE {
            Some("no windows")
        } else if self.window as f64 / n > 0.40 {
            // A glass roof or a field of solar panels in a wall's texture
            // is "window" from end to end; a facade is mostly wall. A curtain
            // wall goes too, and is rare where the model is.
            Some("mostly glass")
        } else if !in_storeys(&self.panes) {
            Some("windows not in storeys")
        } else {
            None
        }
    }
}

/// Values within `tol` of their neighbour grouped, each group's median, in
/// order.
fn clusters(mut v: Vec<f64>, tol: f64) -> Vec<f64> {
    v.sort_by(f64::total_cmp);
    let mut groups: Vec<Vec<f64>> = Vec::new();
    for x in v {
        match groups.last_mut() {
            Some(g) if x - g[g.len() - 1] < tol => g.push(x),
            _ => groups.push(vec![x]),
        }
    }
    groups.into_iter().map(|g| g[g.len() / 2]).collect()
}

fn median_of(mut v: Vec<f64>) -> Option<f64> {
    v.sort_by(f64::total_cmp);
    v.get(v.len() / 2).copied()
}

/// The lattice a wall's windows stand on: the panes' median size, the median
/// step between columns and between storeys, and where the first column and
/// the lowest row are. A facade is rarely a perfect grid — a wider shop bay, a
/// stair window between floors — and the client draws a lattice, so this is
/// the lattice most of its windows sit on. `None` with no step between two
/// columns to measure.
fn lattice(panes: &[Pane], wall: &WallImage) -> Option<Windows> {
    if panes.len() < 3 {
        return None;
    }
    let w = median_of(panes.iter().map(|p| p.w).collect())?;
    let h = median_of(panes.iter().map(|p| p.h).collect())?;
    let cols = clusters(panes.iter().map(|p| p.x).collect(), (w * 0.5).max(0.4));
    let rows = clusters(panes.iter().map(|p| p.y).collect(), ROW_TOLERANCE_M);
    let steps = |c: &[f64], lo: f64, hi: f64| -> Vec<f64> {
        c.windows(2)
            .map(|p| p[1] - p[0])
            .filter(|d| (lo..=hi).contains(d))
            .collect()
    };
    let bay = median_of(steps(&cols, w + 0.15, 5.1))?;
    let st = median_of(steps(&rows, 2.2, 5.1)).unwrap_or(0.0);
    let lowest = *rows.last()?;
    let highest = rows[0];
    // Storeys from the lowest row seen to the highest, on the measured step.
    let n = if st > 0.0 {
        ((lowest - highest) / st).round() as u32 + 1
    } else {
        rows.len() as u32
    };
    let top = wall.at.top - wall.ground;
    let cm = |v: f64| ((v * 100.0).round() / 100.0) as f32;
    Some(Windows {
        a: wall.ends[0],
        b: wall.ends[1],
        bay: cm(bay),
        h: cm(h),
        n: n.clamp(1, 15) as u8,
        st: cm(st),
        w: cm(w),
        x0: cm(cols[0]),
        z0: cm((top - lowest).max(0.5)),
    })
}

/// The panes grouped into rows by height, each row's pane count.
fn rows(panes: &[Pane]) -> Vec<usize> {
    let mut ys: Vec<f64> = panes.iter().map(|p| p.y).collect();
    ys.sort_by(f64::total_cmp);
    let mut rows: Vec<(f64, usize)> = Vec::new();
    for y in ys {
        match rows.last_mut() {
            Some((top, n)) if y - *top < ROW_TOLERANCE_M => *n += 1,
            _ => rows.push((y, 1)),
        }
    }
    rows.into_iter().map(|(_, n)| n).collect()
}

fn in_storeys(panes: &[Pane]) -> bool {
    let r = rows(panes);
    r.iter().filter(|&&n| n >= 2).count() >= 2 || r.iter().any(|&n| n >= 4)
}

/// The window-sized blobs of `window` in the network's grid over a wall
/// `width`×`height` metres; `valid` says which cells are on the wall.
fn panes(is_window: &[bool], valid: &[bool], width: f64, height: f64) -> Vec<Pane> {
    let (cw, ch) = (width / NET_OUT as f64, height / NET_OUT as f64);
    let mut seen = vec![false; NET_OUT * NET_OUT];
    let mut out = Vec::new();
    let mut stack = Vec::new();
    for start in 0..NET_OUT * NET_OUT {
        if seen[start] || !is_window[start] || !valid[start] {
            continue;
        }
        let (mut x0, mut x1, mut y0, mut y1, mut n) = (NET_OUT, 0, NET_OUT, 0, 0usize);
        seen[start] = true;
        stack.push(start);
        while let Some(i) = stack.pop() {
            let (x, y) = (i % NET_OUT, i / NET_OUT);
            (x0, x1, y0, y1) = (x0.min(x), x1.max(x), y0.min(y), y1.max(y));
            n += 1;
            let mut push = |j: usize| {
                if !seen[j] && is_window[j] && valid[j] {
                    seen[j] = true;
                    stack.push(j);
                }
            };
            if x > 0 {
                push(i - 1);
            }
            if x + 1 < NET_OUT {
                push(i + 1);
            }
            if y > 0 {
                push(i - NET_OUT);
            }
            if y + 1 < NET_OUT {
                push(i + NET_OUT);
            }
        }
        let (bw, bh) = (x1 - x0 + 1, y1 - y0 + 1);
        let (w, h) = (bw as f64 * cw, bh as f64 * ch);
        let fill = n as f64 / (bw * bh) as f64;
        if (PANE_W.0..=PANE_W.1).contains(&w) && (PANE_H.0..=PANE_H.1).contains(&h) && fill >= 0.5 {
            out.push(Pane {
                x: (x0 + x1 + 1) as f64 / 2.0 * cw,
                y: (y0 + y1 + 1) as f64 / 2.0 * ch,
                w,
                h,
            });
        }
    }
    out
}

fn judge(wall: &WallImage, grid: &[u8]) -> Verdict {
    let (w, h) = wall.img.dimensions();
    let mut v = Verdict::default();
    for y in 0..h {
        let gy = (y as usize * NET_OUT) / h as usize;
        for x in 0..w {
            let i = (y * w + x) as usize;
            if !wall.mask[i] {
                continue;
            }
            if !wall.visible[i] {
                v.hidden += 1;
                continue;
            }
            let gx = (x as usize * NET_OUT) / w as usize;
            let px = wall.img.get_pixel(x, y).0;
            v.covered += 1;
            match grid[gy * NET_OUT + gx] {
                class::FACADE => {
                    v.wall += 1;
                    if usable(px) {
                        v.facade_px.push(px);
                    }
                }
                class::CORNICE | class::SILL | class::MOLDING | class::DECO | class::PILLAR => {
                    v.wall += 1
                }
                class::WINDOW | class::BLIND => v.window += 1,
                class::DOOR | class::SHOP => v.door += 1,
                class::UNKNOWN | class::BACKGROUND => v.background += 1,
                _ => {}
            }
        }
    }
    // The grid cells on the wall, by the pixel at each cell's centre.
    let valid: Vec<bool> = (0..NET_OUT * NET_OUT)
        .map(|i| {
            let (gx, gy) = (i % NET_OUT, i / NET_OUT);
            let x = ((gx * 2 + 1) * w as usize / (NET_OUT * 2)).min(w as usize - 1);
            let y = ((gy * 2 + 1) * h as usize / (NET_OUT * 2)).min(h as usize - 1);
            wall.mask[y * w as usize + x] && wall.visible[y * w as usize + x]
        })
        .collect();
    let is_window: Vec<bool> = grid
        .iter()
        .map(|&c| c == class::WINDOW || c == class::BLIND)
        .collect();
    v.panes = panes(
        &is_window,
        &valid,
        f64::from(w) * wall.res,
        f64::from(h) * wall.res,
    );
    v
}

/// `ATLAS_FACADES_DEBUG=<dir>`: each judged wall beside the network's labels
/// (yellow wall, blue window, red door, black background), named by building
/// and whether it counted — the way to judge a run by eye.
fn write_debug(dir: &std::path::Path, wall: &WallImage, grid: &[u8], counted: bool, seq: usize) {
    let (w, h) = wall.img.dimensions();
    let mut out = image::RgbImage::new(w * 2, h);
    for y in 0..h {
        let gy = (y as usize * NET_OUT) / h as usize;
        for x in 0..w {
            let i = (y * w + x) as usize;
            let px = wall.img.get_pixel(x, y).0;
            out.put_pixel(x, y, image::Rgb(px));
            if !wall.mask[i] {
                continue;
            }
            // What the cameras could not see, hatched grey.
            if !wall.visible[i] {
                let g = if (x + y) % 8 < 4 { 90 } else { 40 };
                out.put_pixel(w + x, y, image::Rgb([g, g, g]));
                continue;
            }
            let gx = (x as usize * NET_OUT) / w as usize;
            let tint: [u8; 3] = match grid[gy * NET_OUT + gx] {
                class::FACADE
                | class::CORNICE
                | class::SILL
                | class::MOLDING
                | class::DECO
                | class::PILLAR => [230, 180, 60],
                class::WINDOW | class::BLIND => [40, 140, 255],
                class::DOOR | class::SHOP => [255, 60, 60],
                class::BALCONY => [200, 0, 200],
                _ => [0, 0, 0],
            };
            let mixed =
                [0, 1, 2].map(|k| ((f32::from(px[k]) * 0.4) + f32::from(tint[k]) * 0.6) as u8);
            out.put_pixel(w + x, y, image::Rgb(mixed));
        }
    }
    let _ = std::fs::create_dir_all(dir);
    let name = format!(
        "{}_{seq:02}_{}.png",
        wall.pand,
        if counted { "yes" } else { "no" }
    );
    let _ = out.save(dir.join(name));
}

// ─── Measuring ───────────────────────────────────────────────────────────────

fn luma(p: [u8; 3]) -> f64 {
    0.2126 * f64::from(p[0]) + 0.7152 * f64::from(p[1]) + 0.0722 * f64::from(p[2])
}

/// Texels that are not wall at all: the atlas's black padding and vegetation
/// standing in front.
fn usable(p: [u8; 3]) -> bool {
    let [r, g, b] = p.map(i32::from);
    let padding = r + g + b < 24;
    let hi = r.max(g).max(b);
    let sat = if hi > 0 {
        f64::from(hi - r.min(g).min(b)) / f64::from(hi)
    } else {
        0.0
    };
    let green = g > r && g > b && sat > 0.15;
    !padding && !green
}

/// Keep at most `cap` of `px`, evenly strided.
fn stride_into(into: &mut Vec<[u8; 3]>, px: &[[u8; 3]], cap: usize) {
    let room = cap.saturating_sub(into.len());
    if room == 0 || px.is_empty() {
        return;
    }
    let step = px.len().div_ceil(room).max(1);
    into.extend(px.iter().step_by(step).copied());
}

pub(super) fn srgb_to_linear(c: f64) -> f64 {
    let c = c / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// What one texel looks like it is made of. Four answers, not three: a pale
/// warm texel — salmon, pink, cream, the sunlit face of a light brick — may be
/// brick or a warm render, and the photograph alone cannot say which. The
/// first measure called all of those render, and so measured the 1996 pink
/// brick block on the Grote Markt (927611) as render (2026-11-19).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Texel {
    /// Saturated, warm and not pale: brick.
    Brick,
    /// Pale and warm: brick or render.
    WarmPale,
    /// Pale and without colour: render.
    Render,
    /// Grey or dark without colour: concrete, stone, metal.
    Panel,
}

fn kind_of(p: [u8; 3]) -> Texel {
    let c = p.map(|v| srgb_to_linear(f64::from(v)));
    let luma = 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
    let hi = c[0].max(c[1]).max(c[2]);
    let sat = (hi - c[0].min(c[1]).min(c[2])) / hi.max(1e-4);
    let warm = c[0] >= c[2] * 1.12;
    if warm && sat > 0.25 && luma < 0.38 {
        Texel::Brick
    } else if warm && sat > 0.12 {
        Texel::WarmPale
    } else if luma > 0.35 {
        Texel::Render
    } else {
        Texel::Panel
    }
}

/// What the registers say about a building that bears on its walls.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct Facts {
    pub(super) year: Option<u16>,
    /// OpenStreetMap's `building:material`, where somebody mapped it.
    pub(super) material: Option<String>,
    /// A rijksmonument.
    pub(super) monument: bool,
    /// OpenStreetMap's `building:colour`, a name or `#rrggbb`.
    pub(super) colour: Option<String>,
}

/// What a mapped colour says about a wall: pale and colourless (render,
/// stone), or warm (brick).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tone {
    Pale,
    Warm,
}

fn mapped_tone(colour: Option<&str>) -> Option<Tone> {
    let c = colour?.trim().to_ascii_lowercase();
    if let Some(hex) = c.strip_prefix('#').filter(|h| h.len() == 6) {
        let v = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok().map(f64::from);
        let (r, g, b) = (v(0)?, v(2)?, v(4)?);
        let hi = r.max(g).max(b).max(1.0);
        let sat = (hi - r.min(g).min(b)) / hi;
        return if sat > 0.25 && r > b {
            Some(Tone::Warm)
        } else if sat < 0.15 && hi > 150.0 {
            Some(Tone::Pale)
        } else {
            None
        };
    }
    match c.as_str() {
        "white" | "gray" | "grey" | "lightgray" | "lightgrey" | "silver" | "beige" | "cream"
        | "ivory" | "whitesmoke" | "gainsboro" => Some(Tone::Pale),
        "red" | "brown" | "maroon" | "darkred" | "firebrick" | "sienna" | "brick" | "orange"
        | "saddlebrown" | "indianred" => Some(Tone::Warm),
        _ => None,
    }
}

/// How likely each material is before the photograph is looked at, as
/// (brick, render, panel). Dutch building stock is legible by age: before the
/// war almost everything is brick, the reconstruction added concrete, and
/// render is the minority in every decade. A material somebody mapped in
/// OpenStreetMap outweighs the decade.
fn prior(f: &Facts) -> [f64; 3] {
    match f.material.as_deref() {
        Some("brick") => return [0.9, 0.05, 0.05],
        Some("plaster" | "render" | "stucco" | "cement_block") => return [0.05, 0.9, 0.05],
        Some("concrete" | "glass" | "metal" | "stone" | "steel" | "sandstone" | "limestone") => {
            return [0.05, 0.05, 0.9]
        }
        _ => {}
    }
    // A colour somebody mapped says less than a material, and more than a
    // decade: the stadhuis on the Grote Markt (1813, plastered) is mapped
    // "gray", and its cream render reads in the photograph exactly as a pale
    // pink brick does (2026-11-19).
    match mapped_tone(f.colour.as_deref()) {
        Some(Tone::Pale) => return [0.2, 0.6, 0.2],
        Some(Tone::Warm) => return [0.85, 0.1, 0.05],
        None => {}
    }
    match f.year {
        Some(y) if y < 1940 => [0.75, 0.2, 0.05],
        Some(y) if y < 1965 => [0.55, 0.25, 0.2],
        Some(y) if y < 1990 => [0.45, 0.2, 0.35],
        Some(_) => [0.45, 0.3, 0.25],
        None => [0.4, 0.35, 0.25],
    }
}

/// How much of a pale warm texel's vote goes to brick: most light walls with
/// colour in this country are brick, and the rest are cream render.
const WARM_PALE_BRICK: f64 = 0.7;

/// A building's facade from its facade-labelled texels and what the registers
/// say about it, or `None` with too few texels.
pub(super) fn measure(px: &[[u8; 3]], total: usize, facts: &Facts) -> Option<Facade> {
    let mut v: Vec<[u8; 3]> = px.iter().copied().filter(|&p| usable(p)).collect();
    v.sort_by(|a, b| luma(*a).total_cmp(&luma(*b)));
    let lo = (v.len() as f64 * DARK_TRIM) as usize;
    let hi = (v.len() as f64 * (1.0 - BRIGHT_TRIM)) as usize;
    let kept = v.get(lo..hi)?;
    if kept.len() < MIN_TEXELS {
        return None;
    }
    // The material by vote over the kept texels rather than from the median
    // colour — a brick wall's median is pulled toward its mortar, its white
    // frames and its stone sills — weighed against the prior.
    let mut votes = [0.0f64; 3];
    for &p in kept {
        match kind_of(p) {
            Texel::Brick => votes[0] += 1.0,
            Texel::WarmPale => {
                votes[0] += WARM_PALE_BRICK;
                votes[1] += 1.0 - WARM_PALE_BRICK;
            }
            Texel::Render => votes[1] += 1.0,
            Texel::Panel => votes[2] += 1.0,
        }
    }
    let n = kept.len() as f64;
    let p = prior(facts);
    let score = |k: usize| p[k] * (votes[k] / n + 0.05);
    let m = match (0..3).max_by(|&a, &b| score(a).total_cmp(&score(b))) {
        Some(0) => Material::Brick,
        Some(1) => Material::Render,
        _ => Material::Panel,
    };
    // …and the colour of that material's own texels: the shader draws the
    // mortar and the frames itself, so the colour it is given is the
    // brick's. With too few of them, the whole wall's.
    let own: Vec<[u8; 3]> = kept
        .iter()
        .copied()
        .filter(|&q| match (m, kind_of(q)) {
            (Material::Brick, Texel::Brick | Texel::WarmPale) => true,
            (Material::Render, Texel::Render | Texel::WarmPale) => true,
            (Material::Panel, Texel::Panel) => true,
            _ => false,
        })
        .collect();
    // A brick wall's own colour is its bricks', not the pale mortar and
    // reveals a 10 cm texel blends them with: where a fifth of its texels
    // are clearly brick, those alone. The hotel on the Grote Markt is a
    // saturated red; with its pale texels in it measured a grey-brown.
    let fired: Vec<[u8; 3]> = own
        .iter()
        .copied()
        .filter(|&q| m == Material::Brick && kind_of(q) == Texel::Brick)
        .collect();
    let from = if fired.len() * 5 >= own.len() && fired.len() * 3 >= MIN_TEXELS {
        &fired[..]
    } else if own.len() * 3 >= MIN_TEXELS {
        &own[..]
    } else {
        kept
    };
    let channel = |k: usize| {
        let mut c: Vec<u8> = from.iter().map(|q| q[k]).collect();
        c.sort_unstable();
        c[c.len() / 2]
    };
    Some(Facade {
        c: [channel(0), channel(1), channel(2)],
        m,
        n: total.min(u32::MAX as usize) as u32,
        s: None,
    })
}

pub(super) fn linear_to_srgb(c: f64) -> u8 {
    let c = c.clamp(0.0, 1.0);
    let s = if c <= 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    };
    (s * 255.0).round() as u8
}

/// The albedo a facing's whitest facade is taken to have: fresh white render.
/// Everything else facing that way on the leaf is measured against it.
const WHITE_ALBEDO: f64 = 0.8;

/// Compass sectors a leaf's light is read in, 45° each.
const SECTORS: usize = 8;

/// The least a sector's whitest walls may be lit, linear, for them to stand for
/// white: in the shade a white render reads ~0.1–0.2.
const WHITE_FLOOR: f64 = 0.06;

/// Buildings a sector's white must be found among, so one pale grey building
/// alone on its side of a leaf is not taken for white.
const WHITE_BUILDINGS: usize = 3;

/// A facing whose white is at least this share of the leaf's brightest is lit
/// by the sun, and measured; the rest are lit by the sky, and are not
/// ([`Light::white_for`]).
const SUNLIT_SHARE: f64 = 0.5;

/// The sector a horizontal facing (east, north) is in, 0 north and clockwise.
fn sector(facing: [f64; 2]) -> usize {
    let deg = facing[0].atan2(facing[1]).to_degrees().rem_euclid(360.0);
    ((deg / (360.0 / SECTORS as f64)).round() as usize) % SECTORS
}

/// How one leaf's photographs light its walls, facing by facing. The texture of
/// a sunlit wall is its albedo times a sun that overexposes and a haze that
/// greys it; a shaded one is lit by the blue sky; one the sun grazes, by some
/// of each. Measured on the 2026-11-18 run, a pink brick terrace on the sun
/// side came out (211, 185, 168) and a red brick block on the shade side
/// (78, 85, 89); measured against one white per side (2026-11-19), walls the
/// sun grazed came out near black. So the haze comes off first, and then each
/// wall is measured against the whitest walls facing its own way.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Light {
    /// Linear, per channel: the haze over the leaf's darkest facade texels.
    veil: [f64; 3],
    /// Linear, haze off: the whitest walls facing each sector, where it has
    /// white walls to read.
    whites: [Option<[f64; 3]>; SECTORS],
}

impl Light {
    /// From the counted walls of one leaf: each one's building, facing and
    /// facade texels. `None` when no sector has a white.
    fn of(walls: &[(&str, [f64; 2], &[[u8; 3]])]) -> Option<Self> {
        let lin = |p: &[u8; 3]| p.map(|v| srgb_to_linear(f64::from(v)));
        let mut all: Vec<[f64; 3]> = walls
            .iter()
            .flat_map(|(_, _, px)| px.iter().map(lin))
            .collect();
        if all.is_empty() {
            return None;
        }
        let veil = [0, 1, 2].map(|c| {
            all.sort_by(|a, b| a[c].total_cmp(&b[c]));
            all[((all.len() - 1) as f64 * 0.005) as usize][c]
        });
        let luma = |p: &[f64; 3]| 0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2];
        let whites = std::array::from_fn(|s| {
            let facing: Vec<&(&str, [f64; 2], &[[u8; 3]])> =
                walls.iter().filter(|w| sector(w.1) == s).collect();
            let mut buildings: Vec<&str> = facing.iter().map(|w| w.0).collect();
            buildings.sort_unstable();
            buildings.dedup();
            if buildings.len() < WHITE_BUILDINGS {
                return None;
            }
            let mut px: Vec<[f64; 3]> = facing
                .iter()
                .flat_map(|w| w.2.iter().map(lin))
                .map(|l| [0, 1, 2].map(|c| (l[c] - veil[c]).max(0.0)))
                .collect();
            px.sort_by(|a, b| luma(a).total_cmp(&luma(b)));
            let top = &px[px.len() - px.len() / 33..];
            if top.len() < 30 {
                return None;
            }
            let white = [0, 1, 2].map(|c| {
                let mut v: Vec<f64> = top.iter().map(|p| p[c]).collect();
                v.sort_by(f64::total_cmp);
                v[v.len() / 2]
            });
            // …and near grey: a side with no white wall on it has brick or
            // stone as its lightest, and scaling that to white would whiten
            // every brick wall on it. A sky-lit white is bluish, a sunlit one
            // a little warm; neither is 1.8 times in one channel what it is in
            // another, which brick always is.
            let hi = white[0].max(white[1]).max(white[2]);
            let lo = white[0].min(white[1]).min(white[2]);
            (luma(&white) >= WHITE_FLOOR && hi <= lo * 1.8).then_some(white)
        });
        let light = Self { veil, whites };
        light.whites.iter().any(Option::is_some).then_some(light)
    }

    /// The white a wall facing `facing` is measured against: its own sector's,
    /// or a neighbouring sector's when its own has none — 45° turns the light
    /// on a wall less than a whole side does.
    ///
    /// Only for a facing the sun lights: one whose white is at least
    /// [`SUNLIT_SHARE`] of the leaf's brightest. Against a sky-lit white the
    /// 2026-11-19 run whitened a dark brick wall (218, 220, 218) and turned a
    /// red one blue-grey (65, 65, 72): a shaded side's whitest wall is as
    /// often a pale wall half in the sun as a white one in the shade, and
    /// nothing in the photograph tells which.
    fn white_for(&self, facing: [f64; 2]) -> Option<[f64; 3]> {
        let s = sector(facing);
        let luma = |w: &[f64; 3]| 0.2126 * w[0] + 0.7152 * w[1] + 0.0722 * w[2];
        let brightest = self.whites.iter().flatten().map(luma).fold(0.0, f64::max);
        self.whites[s]
            .or(self.whites[(s + 1) % SECTORS])
            .or(self.whites[(s + SECTORS - 1) % SECTORS])
            .filter(|w| luma(w) >= brightest * SUNLIT_SHARE)
    }

    /// A texel of a wall facing `facing` as the wall's own colour: haze off,
    /// scaled so its facing's white is [`WHITE_ALBEDO`] — which also takes out
    /// the blue of the sky on a shaded wall. `None` where no white faces its
    /// way.
    #[cfg(test)]
    fn correct(&self, p: [u8; 3], facing: [f64; 2]) -> Option<[u8; 3]> {
        Some(self.correct_to(p, self.white_for(facing)?))
    }

    /// A texel with the haze taken off and its tone kept: for a wall with no
    /// white of its facing to scale against. The haze is what greys an
    /// oblique photograph's colours — the hotel's red brick came out
    /// (148, 134, 124) with it left on (2026-11-19).
    fn dehaze(&self, p: [u8; 3]) -> [u8; 3] {
        let lin = p.map(|v| srgb_to_linear(f64::from(v)));
        let clear: [f64; 3] = [0, 1, 2].map(|c| (lin[c] - self.veil[c]).max(0.0));
        let y = |c: &[f64; 3]| 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
        let k = if y(&clear) > 1e-4 {
            y(&lin) / y(&clear)
        } else {
            1.0
        };
        clear.map(|c| linear_to_srgb(c * k))
    }

    /// [`Light::correct`] against a given white.
    fn correct_to(&self, p: [u8; 3], white: [f64; 3]) -> [u8; 3] {
        [0, 1, 2].map(|c| {
            let l = (srgb_to_linear(f64::from(p[c])) - self.veil[c]).max(0.0);
            linear_to_srgb(l / white[c].max(1e-3) * WHITE_ALBEDO)
        })
    }
}

/// A wall whose own facade texels have a median tone under this (sRGB luma)
/// and no warmth is in its photograph's shade.
const SHADE_LUMA: f64 = 85.0;

/// The median tone of a wall's facade texels, and whether that median texel
/// is warm (red over blue, as sunlit brick and render are and sky-lit walls
/// are not).
fn median_luma(px: &[[u8; 3]]) -> Option<(f64, bool)> {
    let mut v: Vec<[u8; 3]> = px.to_vec();
    v.sort_by(|a, b| luma(*a).total_cmp(&luma(*b)));
    let m = *v.get(v.len() / 2)?;
    Some((luma(m), i32::from(m[0]) > i32::from(m[2]) + 6))
}

/// Per building: each counted wall's corrected facade texels (a sample) with
/// how brightly its facing was lit, and the planes looked at and counted.
#[derive(Default)]
struct Seen {
    walls: Vec<(f64, Vec<[u8; 3]>, usize)>,
    looked: usize,
    counted: usize,
}

/// A lit facing this much dimmer than the building's best-lit one is left
/// out of its measurement.
const LIT_SHARE: f64 = 0.6;

/// A measured facade darker than this (sRGB luma) is a wall still in a cast
/// shadow the correction cannot see, not a wall that dark: the darkest real
/// ones measured, sooty brick, come out ~70.
const DARKEST_FACADE: f64 = 55.0;

/// How much bluer than red a measured facade may be before it is sky light.
const BLUEST_FACADE: i32 = 8;

impl Seen {
    /// The building's facade from its best-lit walls: a shaded wall's colour
    /// rests on the dimmest white of a leaf and on whatever shadow falls on
    /// it, so it is used only where nothing facing the light was seen.
    fn measure(&self, facts: &Facts) -> Option<Facade> {
        let best = self.walls.iter().map(|w| w.0).fold(0.0f64, f64::max);
        let mut px = Vec::new();
        let mut total = 0;
        for (lit, wall, n) in &self.walls {
            if *lit >= best * LIT_SHARE {
                stride_into(&mut px, wall, SAMPLE_CAP);
                total += n;
            }
        }
        // Nor one bluer than it is red: no Dutch facade is, and a wall the
        // sky lit is (160, 168, 184 for a monument on the Grote Markt).
        measure(&px, total, facts).filter(|f| {
            luma(f.c) >= DARKEST_FACADE && i32::from(f.c[2]) <= i32::from(f.c[0]) + BLUEST_FACADE
        })
    }
}

pub fn run(ctx: &Ctx) -> Outcome<DatasetResult> {
    let cells = lod22_cells(ctx);
    if cells.is_empty() {
        return Err(
            "no LoD2.2 cells in the site tree for this area — run lod22 first"
                .to_string()
                .into(),
        );
    }
    let net = load_net()?;
    let heights = Heights::of_cells(&cells);
    ctx.log(format!(
        "{} blocks of building heights",
        heights.blocks.len()
    ));
    let [min_lng, min_lat, max_lng, max_lat] = wgs84_bbox(ctx.bbox);
    let bbox = [min_lng, min_lat, max_lng, max_lat].map(f64::to_radians);
    ctx.progress(Progress {
        phase: Some("tileset".into()),
        ..Default::default()
    });
    let leaves = leaves(ctx, bbox)?;
    ctx.log(format!("{} model tiles over the area", leaves.len()));
    if leaves.is_empty() {
        return Err("the Groningen city model has no tiles over this area"
            .to_string()
            .into());
    }
    let debug = std::env::var_os("ATLAS_FACADES_DEBUG").map(std::path::PathBuf::from);
    let trace = std::env::var("ATLAS_FACADES_TRACE").ok();
    let threads = std::thread::available_parallelism().map_or(2, |n| n.get());
    let started = std::time::Instant::now();

    let mut seen: HashMap<String, Seen> = HashMap::new();
    let (mut failed, mut done, mut judged, mut unlit) = (0usize, 0u64, 0usize, 0usize);
    let mut refused: BTreeMap<&str, usize> = BTreeMap::new();
    let mut roof_faces: HashMap<(String, u8), model_roofs::FaceTexels> = HashMap::new();
    let mut windows: HashMap<String, Vec<Windows>> = HashMap::new();
    in_order(
        ctx,
        &leaves,
        ctx.upstream().min(MAX_FETCHES),
        |leaf| {
            get_model(ctx, &leaf.url, false)
                .and_then(|b| read_leaf(leaf, &b, &heights).map_err(PipelineError::Failed))
        },
        |leaf, walls| {
            done += 1;
            ctx.progress(Progress {
                phase: Some("tiles".into()),
                done: Some(done),
                total: Some(Some(leaves.len() as u64)),
            });
            let read = match walls {
                Ok(w) => w,
                Err(PipelineError::Cancelled) => return Err(PipelineError::Cancelled),
                Err(e) => {
                    // One unreadable tile is a few dozen buildings keeping
                    // their generated walls, not a failed run.
                    failed += 1;
                    ctx.log(format!("{}: {e}", leaf.url));
                    return Ok(());
                }
            };
            ctx.check()?;
            for (key, t) in read.roofs {
                roof_faces.entry(key).or_default().merge(t);
            }
            let walls = read.walls;
            // The network is the run's cost, and tract runs one inference on
            // one thread: the leaf's walls go out over every core.
            let grids: Vec<Result<Vec<u8>, String>> = std::thread::scope(|s| {
                let chunk = walls.len().div_ceil(threads).max(1);
                let handles: Vec<_> = walls
                    .chunks(chunk)
                    .map(|part| {
                        let net = &net;
                        s.spawn(move || {
                            part.iter().map(|w| labels(net, &w.img)).collect::<Vec<_>>()
                        })
                    })
                    .collect();
                handles
                    .into_iter()
                    .flat_map(|h| h.join().unwrap_or_default())
                    .collect()
            });
            let mut verdicts = Vec::with_capacity(walls.len());
            for (seq, (wall, grid)) in walls.iter().zip(grids).enumerate() {
                let grid = grid.map_err(PipelineError::Failed)?;
                let verdict = judge(wall, &grid);
                judged += 1;
                if let Some(dir) = &debug {
                    write_debug(dir, wall, &grid, verdict.counts(), seq);
                }
                verdicts.push(verdict);
            }
            let counted_walls: Vec<(&str, [f64; 2], &[[u8; 3]])> = walls
                .iter()
                .zip(&verdicts)
                .filter(|(_, v)| v.counts())
                .map(|(w, v)| (w.pand.as_str(), w.facing, v.facade_px.as_slice()))
                .collect();
            let light = Light::of(&counted_walls);
            if light.is_none() {
                unlit += 1;
            }
            for (wall, verdict) in walls.iter().zip(&verdicts) {
                let slot = seen.entry(wall.pand.clone()).or_default();
                slot.looked += 1;
                // `ATLAS_FACADES_TRACE=<pand>`: every decision on that
                // building's walls, for asking why it came out as it did.
                if trace.as_deref() == Some(wall.pand.as_str()) {
                    let med = |px: &[[u8; 3]]| {
                        let mut v = px.to_vec();
                        v.sort_by(|a, b| luma(*a).total_cmp(&luma(*b)));
                        v.get(v.len() / 2).copied()
                    };
                    ctx.log(format!(
                        "trace {}: facing {:?} ({}°), {}×{} px, covered {} hidden {} wall {} \
                         window {} bg {}, panes {}, fails {:?}, white {:?}, raw facade {:?}",
                        wall.pand,
                        wall.facing.map(|v| (v * 100.0).round() / 100.0),
                        (wall.facing[0]
                            .atan2(wall.facing[1])
                            .to_degrees()
                            .rem_euclid(360.0)) as i32,
                        wall.img.width(),
                        wall.img.height(),
                        verdict.covered,
                        verdict.hidden,
                        verdict.wall,
                        verdict.window,
                        verdict.background,
                        verdict.panes.len(),
                        verdict.fails(),
                        light.as_ref().map(|l| l.white_for(wall.facing)),
                        med(&verdict.facade_px),
                    ));
                }
                if let Some(why) = verdict.fails() {
                    *refused.entry(why).or_default() += 1;
                    continue;
                }
                // A wall the network read in storeys has its lattice, sunlit or
                // not: where windows are does not depend on the light.
                if let Some(l) = lattice(&verdict.panes, wall) {
                    windows.entry(wall.pand.clone()).or_default().push(l);
                }
                let Some(light) = light else {
                    *refused.entry("no white on its tile").or_default() += 1;
                    continue;
                };
                // Whether this wall's own photograph had it in shade. The model
                // takes each wall from whichever oblique photograph shows it,
                // flown at its own hour, so a whole facing is not lit alike:
                // on the Grote Markt the hotel's front facing 243° was sunlit
                // red (116, 97, 91) while its back facing 69° was shaded
                // blue-grey (59, 60, 65) — and a test by facing had it the
                // other way round (2026-11-19).
                let raw = median_luma(&verdict.facade_px);
                if raw.is_some_and(|(l, warm)| l < SHADE_LUMA && !warm) {
                    *refused.entry("in its photograph's shade").or_default() += 1;
                    continue;
                }
                // Against its facing's white where it has one; where it has
                // none the texels as photographed. Scaled to another facing's
                // white instead — lit by another photograph's sun — the
                // light brick of Hooghoudt (187, 184, 177) came out white
                // (245, 239, 238) on 2026-11-19.
                // A white dimmer than the wall itself was lit by other light — a
                // shaded wall of the same facing in another photograph — and
                // scaled a sunlit 1929 front (179, 162, 144) to (255, 248, 214).
                let wall_lin = raw.map_or(0.0, |(l, _)| srgb_to_linear(l));
                let white = light
                    .white_for(wall.facing)
                    .filter(|w| 0.2126 * w[0] + 0.7152 * w[1] + 0.0722 * w[2] >= wall_lin);
                let fixed: Vec<[u8; 3]> = match white {
                    Some(white) => verdict
                        .facade_px
                        .iter()
                        .map(|&p| light.correct_to(p, white))
                        .collect(),
                    None => verdict.facade_px.iter().map(|&p| light.dehaze(p)).collect(),
                };
                if fixed.is_empty() {
                    *refused.entry("no white facing its way").or_default() += 1;
                    continue;
                }
                slot.counted += 1;
                // How well lit the wall itself was: its own facade's tone.
                let lit = raw.map_or(0.0, |(l, _)| l);
                let n = fixed.len();
                let mut sample = Vec::new();
                stride_into(&mut sample, &fixed, SAMPLE_CAP);
                slot.walls.push((lit, sample, n));
            }
            if done % 25 == 0 {
                ctx.log(format!(
                    "{done} of {} tiles, {judged} walls judged in {:.0?}",
                    leaves.len(),
                    started.elapsed()
                ));
            }
            Ok(())
        },
    )?;
    if failed * 10 > leaves.len() {
        return Err(format!("{failed} of {} model tiles could not be read", leaves.len()).into());
    }
    // What the BAG run wrote about each building: its year, and a material
    // somebody mapped. Without a `bag` run the prior is the unknown decade's.
    let mut facts: HashMap<String, Facts> = HashMap::new();
    for (cell, _) in &cells {
        let Some(cell_facts) = ctx
            .read_site(&format!("bag/{CACHE_ZOOM}/{}/{}.json", cell.0, cell.1))
            .and_then(|b| String::from_utf8(b).ok())
            .and_then(|t| cartopolis_geo::formats::bag::parse(&t).ok())
        else {
            continue;
        };
        for b in cell_facts.buildings.unwrap_or_default() {
            facts.insert(
                b.id,
                Facts {
                    year: b.year,
                    material: b.mat,
                    monument: b.mon.is_some(),
                    colour: b.col,
                },
            );
        }
    }
    ctx.log(format!("register facts for {} buildings", facts.len()));
    let unknown = Facts::default();
    let facades: BTreeMap<&str, Facade> = seen
        .iter()
        .filter_map(|(pand, s)| {
            Some((
                pand.as_str(),
                s.measure(facts.get(pand).unwrap_or(&unknown))?,
            ))
        })
        .collect();
    if let Some(s) = trace.as_deref().and_then(|p| seen.get(p).map(|s| (p, s))) {
        let (pand, s) = s;
        let f = facts.get(pand).unwrap_or(&unknown);
        let all: Vec<[u8; 3]> = s.walls.iter().flat_map(|w| w.1.iter().copied()).collect();
        ctx.log(format!(
            "trace {pand}: {} walls counted, lit {:?}, measured {:?}, unfiltered {:?}, facts {:?}",
            s.walls.len(),
            s.walls
                .iter()
                .map(|w| (w.0 * 1000.0).round() / 1000.0)
                .collect::<Vec<_>>(),
            s.measure(f),
            measure(&all, all.len(), f),
            f,
        ));
    }
    let counted = seen.values().filter(|s| s.counted > 0).count();
    ctx.log(format!(
        "{judged} walls of {} buildings judged in {:.0?}: {counted} buildings with a facade the \
         network read, {} measured; {unlit} tiles had no white to measure against",
        seen.len(),
        started.elapsed(),
        facades.len()
    ));
    ctx.log(format!("walls refused: {refused:?}"));

    // Each roof face's covering, by building.
    let mut roofs: HashMap<&str, Vec<RoofFace>> = HashMap::new();
    for ((pand, face), t) in &roof_faces {
        let f = facts.get(pand).unwrap_or(&unknown);
        let rf = model_roofs::RoofFacts {
            year: f.year,
            monument: f.monument,
        };
        if let Some(c) = model_roofs::covering(*face, t, rf) {
            roofs.entry(pand.as_str()).or_default().push(c);
        }
    }
    for faces in roofs.values_mut() {
        *faces = model_roofs::roof_of(std::mem::take(faces));
        faces.sort_by_key(|f| f.d);
    }
    ctx.log(format!(
        "{} roof faces of {} buildings sampled, {} buildings' roofs measured",
        roof_faces.len(),
        roof_faces
            .keys()
            .map(|k| &k.0)
            .collect::<std::collections::HashSet<_>>()
            .len(),
        roofs.len()
    ));

    let (mut written, mut placed, mut roofed) = (Vec::new(), 0usize, 0usize);
    let mut by_material = [0usize; 3];
    let mut by_covering: BTreeMap<String, usize> = BTreeMap::new();
    for (cell, lod) in &cells {
        let mut doc = FacadeCell::default();
        for e in &lod.elements {
            if let Some(f) = facades.get(e.id.as_str()) {
                doc.facades.entry(e.id.clone()).or_insert_with(|| f.clone());
            }
            if let Some(r) = roofs.get(e.id.as_str()) {
                doc.roofs.entry(e.id.clone()).or_insert_with(|| r.clone());
            }
            if let Some(w) = windows.get(&e.id) {
                doc.windows.entry(e.id.clone()).or_insert_with(|| w.clone());
            }
        }
        if doc.facades.is_empty() && doc.roofs.is_empty() && doc.windows.is_empty() {
            continue;
        }
        placed += doc.facades.len();
        roofed += doc.roofs.len();
        for f in doc.facades.values() {
            by_material[f.m as usize] += 1;
        }
        for face in doc.roofs.values().flatten() {
            *by_covering
                .entry(format!("{:?}", face.k).to_lowercase())
                .or_default() += 1;
        }
        ctx.write_doc(&cell_path("facades", *cell), &doc)?;
        written.push(*cell);
    }
    ctx.write_coverage(
        "facades/coverage.json",
        &Manifest::new(CACHE_ZOOM, sorted_cells(written.iter().copied()))
            .described(
                ctx.name.as_str(),
                ATTRIBUTION,
                "3dgroningen.nl Panden - Getextureerd; CMP facade SegFormer-B0",
                ctx.bbox,
            )
            .with("building_count", placed),
    )?;
    ctx.log(format!(
        "done: {placed} facades across {} cells ({} brick, {} render, {} panel)",
        written.len(),
        by_material[0],
        by_material[1],
        by_material[2]
    ));
    ctx.log(format!("roofs: {roofed} buildings, faces {by_covering:?}"));
    let mut counts = vec![
        Count::new("facades", placed),
        Count::new("walls judged", judged),
        Count::new("brick", by_material[0]),
        Count::new("render", by_material[1]),
        Count::new("panel", by_material[2]),
        Count::new("roofs", roofed),
    ];
    counts.extend(
        by_covering
            .iter()
            .map(|(k, n)| Count::new(&format!("{k} faces"), *n)),
    );
    Ok(DatasetResult {
        cells: written.len(),
        counts,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_tile_paths_resolve_like_a_browser() {
        let base = "https://h.nl/a/3/4/2.json";
        assert_eq!(
            join(base, "../../6/33/20.json"),
            "https://h.nl/a/6/33/20.json"
        );
        assert_eq!(
            join("https://h.nl/a/tileset.json", "3/4/2.json"),
            "https://h.nl/a/3/4/2.json"
        );
        assert_eq!(
            join(
                "https://h.nl/a/3/4/../../6/33/20.json",
                "../../9/265/166.json"
            ),
            "https://h.nl/a/9/265/166.json"
        );
    }

    #[test]
    fn transforms_compose_parent_first() {
        let mut t = IDENTITY;
        t[12] = 10.0;
        let mut u = IDENTITY;
        u[12] = -3.0;
        u[13] = 2.0;
        let m = mul(&t, &u);
        assert_eq!((m[12], m[13], m[14]), (7.0, 2.0, 0.0));
    }

    #[test]
    fn a_pand_id_is_sixteen_digits() {
        assert_eq!(pand_of("id_bag_0014100010904448"), Some("0014100010904448"));
        assert_eq!(pand_of("0014100010904448"), Some("0014100010904448"));
        assert_eq!(pand_of("id_bag_123"), None);
        assert_eq!(pand_of("tree_7"), None);
    }

    /// A north-facing wall 6 m long and 4 m high, as two triangles over a
    /// texture whose left half is red and right half blue.
    fn north_wall() -> (Vec<WallTri>, Vec<Option<image::RgbImage>>) {
        let tex = image::RgbImage::from_fn(10, 10, |x, _| {
            if x < 5 {
                image::Rgb([200, 0, 0])
            } else {
                image::Rgb([0, 0, 200])
            }
        });
        // Seen from the north, west is on the right: x = -3 at u = 1.
        let (w, e) = (-3.0, 3.0);
        let tris = vec![
            WallTri {
                p: [[e, 0.0, 0.0], [w, 0.0, 0.0], [w, 0.0, 4.0]],
                uv: [[0.0, 1.0], [1.0, 1.0], [1.0, 0.0]],
                tex: 0,
            },
            WallTri {
                p: [[e, 0.0, 0.0], [w, 0.0, 4.0], [e, 0.0, 4.0]],
                uv: [[0.0, 1.0], [1.0, 0.0], [0.0, 0.0]],
                tex: 0,
            },
        ];
        (tris, vec![Some(tex)])
    }

    #[test]
    fn a_wall_is_rectified_as_seen_from_in_front() {
        let (tris, tex) = north_wall();
        let w = rectify("p", &tris, [0.0, 1.0], &tex).unwrap();
        assert_eq!(w.img.dimensions(), (60, 40));
        assert_eq!(w.covered, 60 * 40);
        // East is on the viewer's left, and it carries the texture's left.
        assert_eq!(w.img.get_pixel(5, 20).0, [200, 0, 0]);
        assert_eq!(w.img.get_pixel(55, 20).0, [0, 0, 200]);
    }

    #[test]
    fn a_wall_too_small_is_not_looked_at() {
        let (mut tris, tex) = north_wall();
        for t in &mut tris {
            for p in &mut t.p {
                p[2] *= 0.5;
            }
        }
        assert!(rectify("p", &tris, [0.0, 1.0], &tex).is_none());
    }

    #[test]
    fn a_wall_counts_with_facade_and_windows_and_not_without() {
        let pane = |x, y| Pane {
            x,
            y,
            w: 1.0,
            h: 1.5,
        };
        let storeys = vec![
            pane(1.0, 2.0),
            pane(3.0, 2.1),
            pane(1.0, 5.0),
            pane(3.0, 5.0),
        ];
        let v = |wall, window, background| Verdict {
            covered: 1000,
            wall,
            window,
            background,
            panes: storeys.clone(),
            ..Default::default()
        };
        assert!(v(700, 200, 50).counts());
        // Skylights down a slope: windows, and no two at one height.
        let scattered = Verdict {
            panes: vec![
                pane(1.0, 1.0),
                pane(3.0, 2.5),
                pane(2.0, 4.0),
                pane(4.0, 5.5),
            ],
            ..v(700, 200, 50)
        };
        assert!(!scattered.counts());
        // A roof in a wall's texture: "facade", and no windows.
        assert!(!v(950, 10, 40).counts());
        // The street seen past a lower building.
        assert!(!v(450, 100, 450).counts());
    }

    /// Two storeys of three windows on a 12.8 × 12.8 m wall (10 cm a cell),
    /// and a speck and a band that are not windows.
    #[test]
    fn panes_are_window_sized_blobs_in_metres() {
        let mut win = vec![false; NET_OUT * NET_OUT];
        let mut put = |x0: usize, y0: usize, w: usize, h: usize| {
            for y in y0..y0 + h {
                for x in x0..x0 + w {
                    win[y * NET_OUT + x] = true;
                }
            }
        };
        for row in [20, 60] {
            for col in [10, 50, 90] {
                put(col, row, 12, 16);
            }
        }
        put(5, 110, 2, 2); // 20 cm: a glint
        put(0, 100, 128, 6); // 12.8 m: a band of glass
        let valid = vec![true; NET_OUT * NET_OUT];
        let p = panes(&win, &valid, 12.8, 12.8);
        assert_eq!(p.len(), 6, "{p:?}");
        assert!((p[0].w - 1.2).abs() < 1e-9 && (p[0].h - 1.6).abs() < 1e-9);
        assert!((p[0].x - 1.6).abs() < 1e-9 && (p[0].y - 2.8).abs() < 1e-9);
        assert_eq!(rows(&p), vec![3, 3]);
        assert!(in_storeys(&p));
        // Off the wall, nothing counts.
        assert!(panes(&win, &vec![false; NET_OUT * NET_OUT], 12.8, 12.8).is_empty());
    }

    /// Two storeys of three windows, 2.5 m apart and 3.2 m up, on a wall
    /// whose top stands 12.8 m over the ground; one window a little off.
    #[test]
    fn a_walls_windows_are_fitted_with_a_lattice() {
        let wall = WallImage {
            pand: "p".into(),
            img: image::RgbImage::new(128, 128),
            mask: vec![true; 128 * 128],
            covered: 128 * 128,
            facing: [0.0, 1.0],
            res: 0.1,
            visible: vec![true; 128 * 128],
            at: WallAt {
                s0: 0.0,
                top: 12.8,
                offset: 0.0,
            },
            ground: 0.0,
            ends: [(53.2, 6.56), (53.2, 6.5602)],
        };
        let pane = |x, y| Pane {
            x,
            y,
            w: 1.2,
            h: 1.6,
        };
        let panes = vec![
            pane(1.6, 2.8),
            pane(4.1, 2.8),
            pane(6.6, 2.9),
            pane(1.6, 6.0),
            pane(4.15, 6.0),
            pane(6.6, 6.0),
        ];
        let l = lattice(&panes, &wall).unwrap();
        assert!((l.bay - 2.5).abs() < 0.06, "{l:?}");
        assert!((l.st - 3.2).abs() < 0.11, "{l:?}");
        assert!((l.x0 - 1.6).abs() < 0.01 && (l.w - 1.2).abs() < 0.01 && (l.h - 1.6).abs() < 0.01);
        // The lowest row's centre, 6.0 m down from a top 12.8 m up.
        assert!((l.z0 - 6.8).abs() < 0.01, "{l:?}");
        assert_eq!(l.n, 2, "two storeys seen");
        // One column is no lattice.
        assert!(lattice(&[pane(1.6, 2.8), pane(1.6, 6.0), pane(1.6, 9.2)], &wall).is_none());
    }

    #[test]
    fn ecef_comes_back_as_the_place_it_was() {
        let (lat, lng, h) = (53.2194f64, 6.5665f64, 12.0);
        let (a, e2) = (6_378_137.0, 6.694_379_990_14e-3);
        let (p, l) = (lat.to_radians(), lng.to_radians());
        let n = a / (1.0 - e2 * p.sin().powi(2)).sqrt();
        let e = [
            (n + h) * p.cos() * l.cos(),
            (n + h) * p.cos() * l.sin(),
            (n * (1.0 - e2) + h) * p.sin(),
        ];
        let (la, lo) = geodetic(e);
        assert!(
            (la - lat).abs() < 1e-9 && (lo - lng).abs() < 1e-9,
            "{la} {lo}"
        );
    }

    /// A 10 m block 5–15 m north of a wall that faces north: its lower
    /// storeys are hidden from a camera in the north and its top is not, and
    /// a camera in the south sees all of it.
    #[test]
    fn a_building_across_the_street_hides_the_lower_storeys() {
        let mut hf = Heights::new(53.2, 6.56);
        for y in 5..15 {
            for x in -20..20 {
                hf.raise(x, y, 10.0);
            }
        }
        let north = [0.0, 1.0];
        assert!(!hf.sees(0.0, 0.0, 2.0, north));
        assert!(hf.sees(0.0, 0.0, 9.0, north));
        assert!(hf.sees(0.0, 0.0, 2.0, [0.0, -1.0]));
        // Cameras that could have looked at a wall facing north-east.
        let ne = [std::f64::consts::FRAC_1_SQRT_2; 2];
        assert_eq!(cameras(ne).len(), 2);
        assert_eq!(cameras(north), vec![[0.0, 1.0]]);
    }

    /// Three buildings on each side of a leaf whose sun stands in the south.
    fn leaf<'a>(
        south: &'a [[u8; 3]],
        north: &'a [[u8; 3]],
    ) -> Vec<(&'a str, [f64; 2], &'a [[u8; 3]])> {
        let mut w = Vec::new();
        for b in ["a", "b", "c"] {
            w.push((b, [0.0, -1.0], south));
            w.push((b, [0.0, 1.0], north));
        }
        w
    }

    fn sat(p: [u8; 3]) -> f64 {
        let hi = f64::from(*p.iter().max().unwrap());
        (hi - f64::from(*p.iter().min().unwrap())) / hi
    }

    #[test]
    fn the_light_is_read_off_each_facings_white_and_taken_out() {
        let (south, north) = ([0.0, -1.0], [0.0, 1.0]);
        let mut sun: Vec<[u8; 3]> = vec![[235, 233, 226]; 400];
        sun.extend(vec![[205u8, 168, 150]; 400]);
        let mut shade: Vec<[u8; 3]> = vec![[150, 160, 180]; 400];
        shade.extend(vec![[88u8, 70, 68]; 400]);
        shade.extend(vec![[30u8, 32, 36]; 30]);
        let walls = leaf(&sun, &shade);
        let light = Light::of(&walls).unwrap();
        let expect = linear_to_srgb(WHITE_ALBEDO);
        // The sunlit side's whitest wall becomes the white it is taken to be…
        let w = light.correct([235, 233, 226], south).unwrap();
        assert!(w.iter().all(|&c| c.abs_diff(expect) <= 2), "{w:?}");
        // …the sunlit brick stays brick: the warm white takes some of its
        // warmth, which the sun lent it, and no more…
        let b = light.correct([205, 168, 150], south).unwrap();
        assert!(b[0] > b[2] + 30 && sat(b) > 0.2, "{b:?}");
        // …and the shaded side, whose white is under half the sunlit one's,
        // is not measured at all.
        assert!(light.whites[0].is_some());
        assert_eq!(light.correct([88, 70, 68], north), None);
        // A wall the sun grazes from the south-east takes the south's white.
        assert!(light.white_for([1.0, 0.0]).is_none());
        assert!(light.white_for([0.7, -0.7]).is_some());
    }

    #[test]
    fn a_side_whose_lightest_wall_is_brick_has_no_white() {
        let sun: Vec<[u8; 3]> = vec![[235, 233, 226]; 800];
        let brick: Vec<[u8; 3]> = vec![[150, 80, 60]; 800];
        let light = Light::of(&leaf(&sun, &brick)).unwrap();
        assert!(light.correct([150, 80, 60], [0.0, 1.0]).is_none());
    }

    #[test]
    fn a_building_is_measured_on_its_best_lit_walls() {
        let sunlit = vec![[160u8, 100, 80]; 600];
        let shaded = vec![[50u8, 52, 58]; 600];
        let both = Seen {
            walls: vec![(0.8, sunlit.clone(), 600), (0.2, shaded.clone(), 600)],
            ..Default::default()
        };
        let f = both.measure(&Facts::default()).unwrap();
        assert_eq!((f.c, f.m), ([160, 100, 80], Material::Brick));
        // Seen only in a shadow that left it near black: not measured.
        let dark = Seen {
            walls: vec![(0.2, shaded, 600)],
            ..Default::default()
        };
        assert!(dark.measure(&Facts::default()).is_none());
    }

    #[test]
    fn one_building_alone_is_no_white() {
        let grey: Vec<[u8; 3]> = vec![[120, 122, 125]; 2000];
        let walls = vec![("a", [0.0, -1.0], grey.as_slice())];
        assert!(Light::of(&walls).is_none());
    }

    /// The network loads from the embedded bytes and answers a grid over any
    /// image — the check that the ONNX file in LFS is the network and not a
    /// pointer file, and that tract can run every op in it.
    #[test]
    fn the_network_runs() {
        let net = load_net().unwrap();
        let img = image::RgbImage::from_pixel(64, 48, image::Rgb([150, 80, 60]));
        let grid = labels(&net, &img).unwrap();
        assert_eq!(grid.len(), NET_OUT * NET_OUT);
        assert!(grid.iter().all(|&c| c <= class::SHOP));
    }

    #[test]
    fn texels_read_as_brick_render_panel_or_either() {
        assert_eq!(kind_of([150, 70, 50]), Texel::Brick);
        assert_eq!(kind_of([235, 232, 225]), Texel::Render);
        assert_eq!(kind_of([120, 122, 124]), Texel::Panel);
        // The sunlit pink brick of 927611 on the Grote Markt.
        assert_eq!(kind_of([215, 180, 165]), Texel::WarmPale);
    }

    /// A pale pink wall with white bands: brick on a 1996 block, and render
    /// on one somebody mapped as plaster.
    #[test]
    fn a_pale_warm_wall_is_decided_by_what_the_registers_say() {
        let mut px = vec![[215u8, 180, 165]; 600];
        px.extend(vec![[235u8, 233, 228]; 300]);
        let built = |year| Facts {
            year: Some(year),
            material: None,
            monument: false,
            colour: None,
        };
        let f = measure(&px, 900, &built(1996)).unwrap();
        assert_eq!(f.m, Material::Brick);
        assert!(
            f.c[0] > f.c[2] + 30,
            "the brick's own colour, not the bands': {:?}",
            f.c
        );
        let plaster = Facts {
            year: Some(1996),
            material: Some("plaster".into()),
            monument: false,
            colour: None,
        };
        assert_eq!(measure(&px, 900, &plaster).unwrap().m, Material::Render);
        // A clean white wall stays render whatever its age.
        let white = vec![[236u8, 234, 230]; 900];
        assert_eq!(
            measure(&white, 900, &built(1890)).unwrap().m,
            Material::Render
        );
        // The stadhuis: cream render that reads as pale brick, built 1813,
        // mapped "gray".
        let cream = vec![[200u8, 192, 180]; 900];
        let stadhuis = Facts {
            year: Some(1813),
            monument: true,
            colour: Some("gray".into()),
            ..Default::default()
        };
        assert_eq!(measure(&cream, 900, &stadhuis).unwrap().m, Material::Render);
        assert_eq!(
            measure(&cream, 900, &built(1813)).unwrap().m,
            Material::Brick
        );
    }

    #[test]
    fn mapped_colours_say_pale_or_warm() {
        assert_eq!(mapped_tone(Some("gray")), Some(Tone::Pale));
        assert_eq!(mapped_tone(Some("#9c6c62")), Some(Tone::Warm));
        assert_eq!(mapped_tone(Some("#e8e4dc")), Some(Tone::Pale));
        assert_eq!(mapped_tone(Some("#3a3a3a")), None);
        assert_eq!(mapped_tone(Some("green")), None);
        assert_eq!(mapped_tone(None), None);
    }

    #[test]
    fn a_brick_wall_with_windows_measures_as_its_brick() {
        // Three parts brick, one part dark glass, a few glints.
        let mut px = Vec::new();
        for i in 0..900 {
            px.push(match i % 10 {
                0..=5 => [140, 72, 55],
                6 | 7 => [40, 45, 50],
                8 => [150, 80, 60],
                _ => [250, 250, 250],
            });
        }
        let f = measure(&px, 900, &Facts::default()).unwrap();
        assert_eq!(f.m, Material::Brick);
        assert!(f.c[0] >= 140 && f.c[0] <= 150, "{:?}", f.c);
    }

    #[test]
    fn too_few_texels_measure_nothing() {
        assert!(measure(&[[140, 72, 55]; 100], 100, &Facts::default()).is_none());
        assert!(measure(&[[0, 0, 0]; 5000], 5000, &Facts::default()).is_none());
    }
}
