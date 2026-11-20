//! Mapillary's street photographs of a building, for a building somebody has
//! said the city model got wrong (the building card's "Facade wrong"), or
//! beside a photograph they took themselves.
//!
//! The city model's oblique photographs see a wall from above, once, in
//! whatever sun the flight had: a facade that faced away from it comes out in
//! the sky's blue. A street photograph sees it the way somebody walking past
//! does. Mapillary publishes, for most of its photographs, a pose computed
//! from the overlap between them (`computed_geometry`, `computed_rotation`)
//! and the lens (`camera_parameters`): this module finds the ones that face
//! the building and downloads them, and `street_photo` checks each pose
//! against what the photograph shows and measures the wall.
//!
//! What it gives is CC BY-SA 4.0, © Mapillary contributors: kept marked with
//! its source (`formats::facades::Facade::s`), and credited in the app.

use std::time::Duration;

use cartopolis_geo::formats::facades::Facade;
use serde_json::Value;

const GRAPH: &str = "https://graph.mapillary.com";

/// How far around the building photographs are looked for, metres.
const SEARCH_M: f64 = 120.0;
/// Nearer than this a photograph sees a doorway, farther a street.
const NEAR_M: f64 = 4.0;
const FAR_M: f64 = 110.0;
/// Photographs used at most, the nearest first.
const MAX_PHOTOS: usize = 8;
/// A camera's height over the street, metres.
const CAMERA_M: f64 = 2.0;

/// One photograph and how it was taken.
#[derive(Clone, Debug)]
struct Shot {
    id: String,
    lat: f64,
    lon: f64,
    /// World (east, north, up) to camera (right, down, forward), as OpenSfM
    /// writes it: an angle-axis vector.
    rot: [f64; 3],
    focal: f64,
    k1: f64,
    k2: f64,
    url: String,
    captured: i64,
}

fn token() -> Result<String, String> {
    std::env::var("ATLAS_MAPILLARY_TOKEN")
        .map_err(|_| "ATLAS_MAPILLARY_TOKEN is not set".to_string())
}

fn get(url: &str) -> Result<Vec<u8>, String> {
    let response = ureq::AgentBuilder::new()
        .user_agent("cartopolis-atlas (+https://cartopolis.org)")
        .build()
        .get(url)
        .timeout(Duration::from_secs(60))
        .call()
        .map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut response.into_reader(), &mut bytes)
        .map_err(|e| e.to_string())?;
    Ok(bytes)
}

/// Rodrigues: the rotation matrix of an angle-axis vector, rows.
fn rotation(v: [f64; 3]) -> [[f64; 3]; 3] {
    let th = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if th < 1e-12 {
        return [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    }
    let [x, y, z] = v.map(|c| c / th);
    let (c, s) = (th.cos(), th.sin());
    let k = 1.0 - c;
    [
        [c + x * x * k, x * y * k - z * s, x * z * k + y * s],
        [y * x * k + z * s, c + y * y * k, y * z * k - x * s],
        [z * x * k - y * s, z * y * k + x * s, c + z * z * k],
    ]
}

impl Shot {
    /// Where (latitude°, longitude°, metres over the street) falls in a
    /// `w`×`h` picture of this shot; `None` behind the camera.
    fn project(&self, lat: f64, lon: f64, up: f64, w: f64, h: f64) -> Option<[f64; 2]> {
        let kx = 111_320.0 * self.lat.to_radians().cos();
        let p = [
            (lon - self.lon) * kx,
            (lat - self.lat) * 110_540.0,
            up - CAMERA_M,
        ];
        let r = rotation(self.rot);
        let c = [0, 1, 2].map(|i| r[i][0] * p[0] + r[i][1] * p[1] + r[i][2] * p[2]);
        if c[2] < 0.5 {
            return None;
        }
        let (xn, yn) = (c[0] / c[2], c[1] / c[2]);
        let r2 = xn * xn + yn * yn;
        let d = 1.0 + self.k1 * r2 + self.k2 * r2 * r2;
        let s = w.max(h) * self.focal;
        Some([w / 2.0 + s * xn * d, h / 2.0 + s * yn * d])
    }
}

/// The photographs around a building that can show it: a computed pose, a
/// perspective lens, taken after it was built, at a useful distance and with
/// the building in front of the camera — the nearest from each place, so
/// that eight are eight views and not eight frames of one drive past.
///
/// Asked tile by tile: one request for the whole square answers 500 of the
/// thousands there, a different 500 each time, so the same building measured
/// twice came out from different photographs (2026-11-19).
fn shots_of(ring: &[(f64, f64)], built: Option<u16>) -> Result<Vec<Shot>, String> {
    let (clat, clon) = centroid(ring);
    let kx = 111_320.0 * clat.to_radians().cos();
    let token = urlencode(&token()?);
    let mut all: Vec<Value> = Vec::new();
    let n = (2.0 * SEARCH_M / TILE_M).ceil() as i32;
    for ty in 0..n {
        for tx in 0..n {
            let e0 = -SEARCH_M + f64::from(tx) * TILE_M;
            let n0 = -SEARCH_M + f64::from(ty) * TILE_M;
            let url = format!(
                "{GRAPH}/images?access_token={token}&fields=id,captured_at,camera_type,\
                 camera_parameters,computed_geometry,computed_rotation,thumb_2048_url\
                 &bbox={},{},{},{}&limit=2000",
                clon + e0 / kx,
                clat + n0 / 110_540.0,
                clon + (e0 + TILE_M) / kx,
                clat + (n0 + TILE_M) / 110_540.0
            );
            let doc: Value = serde_json::from_slice(&get(&url)?).map_err(|e| e.to_string())?;
            if let Some(data) = doc["data"].as_array() {
                all.extend(data.iter().cloned());
            }
        }
    }
    let since = built.map_or(0, |y| {
        // Milliseconds at the start of the year after it was built.
        (i64::from(y) + 1 - 1970) * 31_556_952_000
    });
    let dist = |s: &Shot| ((clon - s.lon) * kx).hypot((clat - s.lat) * 110_540.0);
    let mut out: Vec<Shot> = all
        .iter()
        .filter_map(|i| {
            if i["camera_type"].as_str()? != "perspective" {
                return None;
            }
            let g = i["computed_geometry"]["coordinates"].as_array()?;
            let rot = i["computed_rotation"].as_array()?;
            let cam = i["camera_parameters"].as_array()?;
            Some(Shot {
                id: i["id"].as_str()?.to_string(),
                lon: g.first()?.as_f64()?,
                lat: g.get(1)?.as_f64()?,
                rot: [
                    rot.first()?.as_f64()?,
                    rot.get(1)?.as_f64()?,
                    rot.get(2)?.as_f64()?,
                ],
                focal: cam.first()?.as_f64()?,
                k1: cam.get(1).and_then(Value::as_f64).unwrap_or(0.0),
                k2: cam.get(2).and_then(Value::as_f64).unwrap_or(0.0),
                url: i["thumb_2048_url"].as_str()?.to_string(),
                captured: i["captured_at"].as_i64()?,
            })
        })
        .filter(|s| s.captured >= since)
        // A wide lens bends straight walls more than a drawing without its
        // distortion can follow.
        .filter(|s| s.focal >= MIN_FOCAL)
        .filter(|s| (NEAR_M..=FAR_M).contains(&dist(s)))
        .filter(|s| {
            // A third of the building's corners, three metres up, inside the
            // frame.
            let inside = ring
                .iter()
                .filter(|&&(la, lo)| {
                    s.project(la, lo, 3.0, 2048.0, 1536.0)
                        .is_some_and(|[x, y]| {
                            (0.0..2048.0).contains(&x) && (0.0..1536.0).contains(&y)
                        })
                })
                .count();
            inside * 3 >= ring.len()
        })
        .collect();
    // The recent first, then the nearest: a building photographed in its
    // scaffolding, behind hoardings, is not the building (seen 2026-11-20).
    let newest = out.iter().map(|s| s.captured).max().unwrap_or(0);
    let old = |s: &Shot| s.captured < newest - RECENT_MS;
    out.sort_by(|a, b| {
        old(a)
            .cmp(&old(b))
            .then(dist(a).total_cmp(&dist(b)))
            .then_with(|| a.id.cmp(&b.id))
    });
    out.dedup_by(|a, b| a.id == b.id);
    let mut chosen: Vec<Shot> = Vec::new();
    for s in out {
        let apart = |c: &Shot| ((c.lon - s.lon) * kx).hypot((c.lat - s.lat) * 110_540.0);
        if chosen.iter().all(|c| apart(c) >= APART_M) {
            chosen.push(s);
        }
        if chosen.len() == MAX_PHOTOS {
            break;
        }
    }
    Ok(chosen)
}

/// The square around the building is asked for in tiles this wide, metres.
const TILE_M: f64 = 60.0;
/// Photographs within this of the newest are recent, milliseconds: three
/// years.
const RECENT_MS: i64 = 3 * 31_556_952_000;
/// Two photographs taken nearer than this are one view.
const APART_M: f64 = 6.0;
/// Narrower than this (focal length over the long side) a lens is a wide
/// action camera's.
const MIN_FOCAL: f64 = 0.55;

fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn centroid(ring: &[(f64, f64)]) -> (f64, f64) {
    let n = ring.len().max(1) as f64;
    (
        ring.iter().map(|p| p.0).sum::<f64>() / n,
        ring.iter().map(|p| p.1).sum::<f64>() / n,
    )
}

/// A photograph Mapillary has of a building, downloaded, with its pose.
pub(super) struct Photo {
    pub id: String,
    pub lat: f64,
    pub lon: f64,
    rot: [f64; 3],
    pub focal: f64,
    pub img: image::RgbImage,
}

impl Photo {
    /// Its heading (clockwise from north), tilt and roll, degrees, as
    /// `street_photo::Pose` holds them.
    pub fn angles(&self) -> (f64, f64, f64) {
        camera_angles(rotation(self.rot))
    }
}

/// The rows of an OpenSfM rotation are the camera's right, down and forward
/// in the world's east, north, up.
fn camera_angles(r: [[f64; 3]; 3]) -> (f64, f64, f64) {
    let (right, fwd) = (r[0], r[2]);
    let yaw = fwd[0].atan2(fwd[1]);
    let pitch = fwd[2].clamp(-1.0, 1.0).asin();
    let level = [yaw.cos(), -yaw.sin(), 0.0];
    let down = [
        fwd[1] * level[2] - fwd[2] * level[1],
        fwd[2] * level[0] - fwd[0] * level[2],
        fwd[0] * level[1] - fwd[1] * level[0],
    ];
    let dot = |a: [f64; 3], b: [f64; 3]| a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
    let roll = dot(right, down).atan2(dot(right, level));
    (yaw.to_degrees(), pitch.to_degrees(), roll.to_degrees())
}

/// Mapillary's photographs facing the building with outline `ring` (taken
/// after `built`), downloaded. One that will not download is left out.
pub(super) fn photos_of(ring: &[(f64, f64)], built: Option<u16>) -> Result<Vec<Photo>, String> {
    Ok(shots_of(ring, built)?
        .into_iter()
        .filter_map(|s| {
            let img = image::load_from_memory(&get(&s.url).ok()?).ok()?.to_rgb8();
            Some(Photo {
                id: s.id,
                lat: s.lat,
                lon: s.lon,
                rot: s.rot,
                focal: s.focal,
                img,
            })
        })
        .collect())
}

/// Measure building `pand` near (`near`) again from Mapillary, from the
/// `bag` and `lod22` trees of the site at `site`. `Ok(None)` when no
/// photograph shows enough of it. The z14 cell its BAG record is filed under
/// comes back with it, for `crate::facade_edits` to file the result under.
pub fn recheck(
    site: &std::path::Path,
    pand: &str,
    near: (f64, f64),
    log: &dyn Fn(String),
) -> Result<(Option<Facade>, (i64, i64)), String> {
    let m = super::street_photo::measure_photos(site, pand, near, None, true, log)?;
    Ok((m.facade, m.cell))
}

/// Whether Mapillary can be asked at all here.
#[must_use]
pub fn available() -> bool {
    token().is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The hotel on the Grote Markt in photograph 1080360643424316: its
    /// outline falls where the photograph shows it (checked by eye,
    /// 2026-11-19), around x 700–1210 of 2048.
    #[test]
    fn a_building_projects_into_its_photograph() {
        let shot = Shot {
            id: "1080360643424316".into(),
            lat: 53.218264359521,
            lon: 6.5672182814164,
            rot: [1.4830763399174, -0.89564190510705, 0.81940388210645],
            focal: 0.75466561990073,
            k1: 0.095786286487285,
            k2: -0.11405841757296,
            url: String::new(),
            captured: 0,
        };
        let [x, y] = shot
            .project(53.21888, 6.56876, 0.0, 2048.0, 1536.0)
            .unwrap();
        assert!((650.0..1250.0).contains(&x), "{x}");
        assert!((550.0..700.0).contains(&y), "{y}");
        // Higher is up the picture.
        let [_, y_top] = shot
            .project(53.21888, 6.56876, 20.0, 2048.0, 1536.0)
            .unwrap();
        assert!(y_top < y - 100.0);
        // Behind the camera is not in it.
        assert!(shot.project(53.2175, 6.5660, 0.0, 2048.0, 1536.0).is_none());
    }

    #[test]
    fn a_pose_turns_into_heading_tilt_and_roll() {
        // The hotel photograph above: it looks east-north-east across the
        // square, near enough level.
        let (yaw, pitch, roll) = camera_angles(rotation([
            1.4830763399174,
            -0.89564190510705,
            0.81940388210645,
        ]));
        assert!((40.0..80.0).contains(&yaw.rem_euclid(360.0)), "{yaw}");
        assert!(pitch.abs() < 15.0, "{pitch}");
        assert!(roll.abs() < 15.0, "{roll}");
    }
}
