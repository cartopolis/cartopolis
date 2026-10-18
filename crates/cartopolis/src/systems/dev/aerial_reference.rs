//! The aerial photograph of what a capture looks at, written beside it.
//!
//! **A render of a real place can be plausible and wrong**, and nothing in the
//! render says which. On 2026-09-28 the unified tunnels walled a cycle path across
//! the park beside Groningen's ring for a hundred metres: every number in the
//! capture's metrics was self-consistent, the picture looked like a sunken path,
//! and only an aerial photograph of the spot — fetched by hand, three tiles at a
//! time — showed a path at grade across the grass. So the shot harness writes
//! that photograph itself (`--aerial-reference`, or `aerial_reference = true` in
//! a manifest), twice over, from PDOK's Luchtfoto RGB:
//!
//! - **`<shot>.aerial.jpg`, from the capture's own camera**: the same position,
//!   orientation, field of view and size, every pixel's ray followed down to the
//!   ground and the photograph sampled where it lands. Roads, cuttings, water and
//!   openings sit exactly where the render put its own, so the two can be laid
//!   side by side or flicked between. What stands up — buildings, trees, walls —
//!   is flattened onto the ground the way an orthophoto always flattens it, and
//!   the sky is a plain colour: this is a check of the *ground plan*, which is
//!   what a structure below the ground either gets right or does not.
//! - **`<shot>.aerial-map.jpg`, from above**: the ground from the camera to the
//!   point it is aimed at, camera as a red dot, line of sight red, aim a yellow
//!   cross — where the capture was taken from, at a glance.
//!
//! The imagery is the same layer the aerial basemap draws (`systems::aerial`),
//! with the same envelope; outside it nothing is written, and a failure to fetch
//! is logged rather than failing the capture it would only have been a reference
//! for. Native only, and off the frame: the tiles are fetched on a thread of its
//! own, so a capture never waits on PDOK.

use std::path::{Path, PathBuf};

use image::{Rgb, RgbImage};

/// Tile edge in pixels.
const TILE_PX: f64 = 256.0;

/// The finest zoom the references are drawn at: about 18 cm a pixel at
/// Groningen's latitude, well past what the 25 cm layer resolves.
const MAX_ZOOM: u32 = 19;

/// The coarsest they step down to.
const MIN_ZOOM: u32 = 14;

/// The widest the top-down map may be, pixels, before it drops a zoom level.
const MAX_SIDE_PX: f64 = 1400.0;

/// The widest the mosaic behind the camera-matched view may be, pixels.
const MAX_MOSAIC_PX: f64 = 3072.0;

/// How much ground to show around the camera and the aim, metres.
const MARGIN_M: f64 = 50.0;

/// How far along the ground the camera-matched view is drawn at least, metres;
/// beyond its reach it fades into haze, as a render's own distance does.
const MIN_REACH_M: f32 = 900.0;

/// The reach in camera heights. A fixed 900 m drew a straight-down capture from
/// 1.5 km as a disc in a grey frame, since its corners are ~1.3 km out; six
/// heights covers the corners of any view down to ~10° below the horizon.
const REACH_PER_HEIGHT: f32 = 6.0;

/// The farthest any reference reaches, metres — at [`MIN_ZOOM`] a mosaic of
/// [`MAX_MOSAIC_PX`] spans ~18 km at Groningen's latitude.
const MAX_REACH_M: f32 = 12_000.0;

/// How the photograph is layered from the camera outwards: one mosaic per
/// fraction of the reach, each at the finest zoom that fits it, so the ground
/// under a low camera is not drawn at the zoom the horizon needed.
const LAYERS: [f32; 3] = [1.0, 0.25, 0.0625];

/// Smallest layer worth its own mosaic, metres.
const MIN_LAYER_M: f32 = 60.0;

/// Tries per tile; PDOK answers the odd tile with a 502.
const TILE_TRIES: u32 = 3;

/// What a reference is drawn around: where the camera is and where it looks, as
/// latitude and longitude, and — for the camera-matched view — the camera itself.
#[derive(Clone, Copy, Debug)]
pub struct View {
    pub camera: (f64, f64),
    pub aim: (f64, f64),
    pub pose: Option<Pose>,
}

/// The capture's camera, in the anchored world frame the harness works in.
#[derive(Clone, Copy, Debug)]
pub struct Pose {
    /// The map anchor the world frame is measured from
    /// (`geo_nav::world_to_lat_lng`).
    pub anchor: (f64, f64, u32),
    pub position: [f32; 3],
    pub right: [f32; 3],
    pub up: [f32; 3],
    pub forward: [f32; 3],
    /// Vertical field of view, radians.
    pub fov_y: f32,
    /// The capture's size in pixels.
    pub size: (u32, u32),
    /// The height of the ground the photograph is laid on, world `y`: the
    /// terrain under the aim.
    pub ground_y: f32,
}

/// Write the references for the capture at `capture` on a thread of their own —
/// `<stem>.aerial.jpg` from the camera when `view` has one, and
/// `<stem>.aerial-map.jpg` from above. Nothing is written outside the imagery's
/// envelope.
pub fn spawn_write(capture: PathBuf, view: View) {
    if !crate::systems::aerial::covers(view.aim.0, view.aim.1) {
        tracing::info!(path = %capture.display(), "aerial reference: outside the imagery, none written");
        return;
    }
    let handle = std::thread::spawn(move || {
        let save = |img: Result<RgbImage, String>, path: &Path| match img {
            Ok(img) => match img.save(path) {
                Ok(()) => tracing::info!(path = %path.display(), "aerial reference: saved"),
                Err(e) => {
                    tracing::warn!(path = %path.display(), error = %e, "aerial reference: not saved")
                }
            },
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "aerial reference: not fetched")
            }
        };
        if let Some(pose) = view.pose {
            save(from_camera(pose), &capture.with_extension("aerial.jpg"));
        }
        save(from_above(view), &capture.with_extension("aerial-map.jpg"));
    });
    PENDING
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push(handle);
}

/// References still being fetched.
static PENDING: crate::platform::sync::Mutex<Vec<std::thread::JoinHandle<()>>> =
    crate::platform::sync::Mutex::new(Vec::new());

/// How long [`wait_all`] gives the references still being fetched before the
/// harness exits without them.
///
/// A reference is a mosaic of up to ~150 tiles fetched one after another, each
/// allowed the request timeout and [`TILE_TRIES`] attempts, so an unbounded join
/// against a slow or stalled PDOK is a process that has written its capture and
/// then never exits — which is what a scripted run on the Deck did (2026-10-08).
const WAIT_ALL_DEADLINE: std::time::Duration = std::time::Duration::from_secs(90);

/// Wait for every reference still being fetched, for at most
/// [`WAIT_ALL_DEADLINE`]. The harness calls this before it exits: the last
/// capture's reference is started a frame before the run ends, and a process that
/// leaves takes its threads with it. A reference still unfinished at the deadline
/// is abandoned with a warning — the capture itself is already on disk.
pub fn wait_all() {
    let handles = std::mem::take(
        &mut *PENDING
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
    );
    let deadline = std::time::Instant::now() + WAIT_ALL_DEADLINE;
    for h in handles {
        while !h.is_finished() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        if h.is_finished() {
            let _ = h.join();
        } else {
            tracing::warn!(
                deadline_s = WAIT_ALL_DEADLINE.as_secs(),
                "aerial reference: still fetching at exit, abandoned"
            );
        }
    }
}

/// A point in world pixels at zoom `z` (Web Mercator, north-west origin).
fn world_px(lat: f64, lng: f64, z: u32) -> (f64, f64) {
    let scale = TILE_PX * f64::from(1u32 << z);
    let x = (lng + 180.0) / 360.0 * scale;
    let phi = lat.to_radians();
    let y = (1.0 - (phi.tan() + 1.0 / phi.cos()).ln() / std::f64::consts::PI) / 2.0 * scale;
    (x, y)
}

/// Metres per pixel at zoom `z` and latitude `lat`.
fn metres_per_px(lat: f64, z: u32) -> f64 {
    40_075_016.686 * lat.to_radians().cos() / (TILE_PX * f64::from(1u32 << z))
}

/// The zoom, and the pixel box at it, that shows every point of `pts` with
/// `margin_m` around them in no more than `max_side` pixels a side.
fn frame_points(pts: &[(f64, f64)], margin_m: f64, max_side: f64) -> (u32, [f64; 4]) {
    let lat = pts.first().map_or(52.0, |p| p.0);
    let mut z = MAX_ZOOM;
    loop {
        let pad = margin_m / metres_per_px(lat, z);
        let mut b = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
        for &(la, ln) in pts {
            let (x, y) = world_px(la, ln, z);
            b = [b[0].min(x), b[1].min(y), b[2].max(x), b[3].max(y)];
        }
        let b = [b[0] - pad, b[1] - pad, b[2] + pad, b[3] + pad];
        if (b[2] - b[0]).max(b[3] - b[1]) <= max_side || z == MIN_ZOOM {
            return (z, b);
        }
        z -= 1;
    }
}

/// The top-down frame for `view`: both ends with [`MARGIN_M`] around them.
fn frame(view: View) -> (u32, [f64; 4]) {
    frame_points(&[view.camera, view.aim], MARGIN_M, MAX_SIDE_PX)
}

/// One tile of the photograph, tried [`TILE_TRIES`] times.
fn fetch_tile(template: &str, z: u32, tx: u32, ty: u32) -> Result<RgbImage, String> {
    let url = template
        .replace("{z}", &z.to_string())
        .replace("{x}", &tx.to_string())
        .replace("{y}", &ty.to_string());
    let mut last = String::new();
    for attempt in 0..TILE_TRIES {
        if attempt > 0 {
            std::thread::sleep(std::time::Duration::from_millis(400 * u64::from(attempt)));
        }
        let got = futures_lite::future::block_on(crate::platform::http::fetch(
            crate::platform::http::Request::get(url.clone()),
        ))
        .and_then(|bytes| {
            image::load_from_memory(&bytes)
                .map(|i| i.to_rgb8())
                .map_err(|e| format!("tile {z}/{tx}/{ty}: {e}"))
        });
        match got {
            Ok(tile) => return Ok(tile),
            Err(e) => last = e,
        }
    }
    Err(last)
}

/// The photograph over the pixel box `[x0, y0, x1, y1]` at zoom `z`, stitched
/// from its tiles.
///
/// **A tile that will not come is a grey square, not a lost reference**: one 502
/// out of dozens used to discard the whole picture. Only a mosaic with no tile
/// at all is an error.
fn mosaic(z: u32, [x0, y0, x1, y1]: [f64; 4]) -> Result<RgbImage, String> {
    let (tx0, ty0) = ((x0 / TILE_PX).floor() as u32, (y0 / TILE_PX).floor() as u32);
    let (tx1, ty1) = ((x1 / TILE_PX).floor() as u32, (y1 / TILE_PX).floor() as u32);
    let (w, h) = ((x1 - x0).ceil() as u32, (y1 - y0).ceil() as u32);
    let mut out = RgbImage::from_pixel(w, h, Rgb([40, 40, 40]));
    let template = crate::systems::aerial::template();
    let (mut got, mut missing, mut last) = (0u32, 0u32, String::new());
    for ty in ty0..=ty1 {
        for tx in tx0..=tx1 {
            let tile = match fetch_tile(&template, z, tx, ty) {
                Ok(tile) => {
                    got += 1;
                    tile
                }
                Err(e) => {
                    missing += 1;
                    last = e;
                    continue;
                }
            };
            let (ox, oy) = (f64::from(tx) * TILE_PX - x0, f64::from(ty) * TILE_PX - y0);
            for (px, py, c) in tile.enumerate_pixels() {
                let (dx, dy) = (ox + f64::from(px), oy + f64::from(py));
                if dx >= 0.0 && dy >= 0.0 && (dx as u32) < w && (dy as u32) < h {
                    out.put_pixel(dx as u32, dy as u32, *c);
                }
            }
        }
    }
    if got == 0 {
        return Err(last);
    }
    if missing > 0 {
        tracing::warn!(zoom = z, missing, error = %last, "aerial reference: tiles missing, drawn grey");
    }
    Ok(out)
}

/// The top-down reference: the photograph around the camera and the aim,
/// marked. Blocks on the tiles: call it off the frame (the field check's aerial
/// panel uses it too).
pub fn from_above(view: View) -> Result<RgbImage, String> {
    let (z, b) = frame(view);
    let mut out = mosaic(z, b)?;
    let at = |ll: (f64, f64)| {
        let p = world_px(ll.0, ll.1, z);
        (p.0 - b[0], p.1 - b[1])
    };
    mark(&mut out, at(view.camera), at(view.aim));
    Ok(out)
}

/// Where the ray through pixel `(u, v)` of the capture meets the ground, world
/// XZ, and how far along the ground it is — `None` above the horizon.
fn ground_hit(pose: &Pose, u: f32, v: f32) -> Option<([f32; 2], f32)> {
    let (w, h) = (pose.size.0 as f32, pose.size.1 as f32);
    let t = (pose.fov_y * 0.5).tan();
    let (nx, ny) = ((2.0 * u / w - 1.0) * t * w / h, (1.0 - 2.0 * v / h) * t);
    let d = [
        pose.forward[0] + pose.right[0] * nx + pose.up[0] * ny,
        pose.forward[1] + pose.right[1] * nx + pose.up[1] * ny,
        pose.forward[2] + pose.right[2] * nx + pose.up[2] * ny,
    ];
    if d[1] >= -1e-5 {
        return None;
    }
    let s = (pose.ground_y - pose.position[1]) / d[1];
    if s <= 0.0 {
        return None;
    }
    let p = [pose.position[0] + d[0] * s, pose.position[2] + d[2] * s];
    let along = (p[0] - pose.position[0]).hypot(p[1] - pose.position[2]);
    Some((p, along))
}

/// How far along the ground the camera-matched view is drawn before it has faded
/// to haze: [`REACH_PER_HEIGHT`] camera heights, within
/// [`MIN_REACH_M`]..=[`MAX_REACH_M`].
fn reach(pose: &Pose) -> f32 {
    let height = (pose.position[1] - pose.ground_y).max(0.0);
    (height * REACH_PER_HEIGHT).clamp(MIN_REACH_M, MAX_REACH_M)
}

/// The reference from the capture's own camera: every pixel's ray followed down
/// to the ground and the photograph sampled where it lands.
fn from_camera(pose: Pose) -> Result<RgbImage, String> {
    let ll = |p: [f32; 2]| {
        crate::systems::geo_nav::world_to_lat_lng(pose.anchor, f64::from(p[0]), f64::from(p[1]))
    };
    let reach_m = reach(&pose);
    // The ground the view reaches, from a coarse grid of rays, and the camera's
    // own foot.
    let (w, h) = pose.size;
    let foot = ll([pose.position[0], pose.position[2]]);
    let mut hits: Vec<((f64, f64), f32)> = Vec::new();
    for j in 0..=32 {
        for i in 0..=32 {
            let (u, v) = (w as f32 * i as f32 / 32.0, h as f32 * j as f32 / 32.0);
            if let Some((p, along)) = ground_hit(&pose, u, v) {
                if along <= reach_m {
                    hits.push((ll(p), along));
                }
            }
        }
    }
    // One mosaic per layer, coarsest first; a finer layer that lands on the same
    // zoom as the one before it adds nothing and is skipped.
    let mut layers: Vec<(u32, [f64; 4], RgbImage)> = Vec::new();
    for (k, frac) in LAYERS.iter().enumerate() {
        let layer_m = reach_m * frac;
        if k > 0 && layer_m < MIN_LAYER_M {
            break;
        }
        let pts: Vec<(f64, f64)> = std::iter::once(foot)
            .chain(hits.iter().filter(|(_, a)| *a <= layer_m).map(|(p, _)| *p))
            .collect();
        if k > 0 && pts.len() < 2 {
            break;
        }
        let (z, b) = frame_points(&pts, 20.0, MAX_MOSAIC_PX);
        if layers.last().is_some_and(|l| l.0 >= z) {
            continue;
        }
        match mosaic(z, b) {
            Ok(photo) => layers.push((z, b, photo)),
            // The coarsest layer is the picture; a finer one only sharpens it.
            Err(e) if layers.is_empty() => return Err(e),
            Err(e) => {
                tracing::warn!(zoom = z, error = %e, "aerial reference: finer layer not fetched")
            }
        }
    }
    let sky = |v: f32| {
        let k = (v / h as f32).clamp(0.0, 1.0);
        Rgb([
            (150.0 + 60.0 * k) as u8,
            (185.0 + 40.0 * k) as u8,
            (225.0 + 20.0 * k) as u8,
        ])
    };
    let haze = Rgb([200u8, 210, 220]);
    let mut out = RgbImage::new(w, h);
    for v in 0..h {
        for u in 0..w {
            let (fu, fv) = (u as f32 + 0.5, v as f32 + 0.5);
            let Some((p, along)) = ground_hit(&pose, fu, fv) else {
                out.put_pixel(u, v, sky(fv));
                continue;
            };
            let (lat, lng) = ll(p);
            // The finest layer that holds this point.
            let c = layers
                .iter()
                .rev()
                .find_map(|(z, b, photo)| {
                    let (x, y) = world_px(lat, lng, *z);
                    let (sx, sy) = (x - b[0], y - b[1]);
                    (sx >= 0.0
                        && sy >= 0.0
                        && sx < f64::from(photo.width())
                        && sy < f64::from(photo.height()))
                    .then(|| *photo.get_pixel(sx as u32, sy as u32))
                })
                .unwrap_or(haze);
            // Fade into haze towards the edge of what was fetched, as distance
            // fades a render.
            let f = ((along - reach_m * 0.7) / (reach_m * 0.3)).clamp(0.0, 1.0);
            let mix = |a: u8, b: u8| (f32::from(a) * (1.0 - f) + f32::from(b) * f) as u8;
            out.put_pixel(
                u,
                v,
                Rgb([mix(c[0], haze[0]), mix(c[1], haze[1]), mix(c[2], haze[2])]),
            );
        }
    }
    Ok(out)
}

const RED: Rgb<u8> = Rgb([230, 40, 30]);
const YELLOW: Rgb<u8> = Rgb([255, 220, 0]);

/// The line of sight in red from the camera, a red dot at the camera and a
/// yellow cross at the aim.
fn mark(img: &mut RgbImage, camera: (f64, f64), aim: (f64, f64)) {
    let mut dot = |x: f64, y: f64, r: f64, c: Rgb<u8>| {
        let (w, h) = (img.width() as i64, img.height() as i64);
        let ri = r.ceil() as i64;
        for dy in -ri..=ri {
            for dx in -ri..=ri {
                if ((dx * dx + dy * dy) as f64) > r * r {
                    continue;
                }
                let (px, py) = (x as i64 + dx, y as i64 + dy);
                if px >= 0 && py >= 0 && px < w && py < h {
                    img.put_pixel(px as u32, py as u32, c);
                }
            }
        }
    };
    let len = (aim.0 - camera.0).hypot(aim.1 - camera.1).max(1.0);
    let steps = len.ceil() as usize;
    for k in 0..=steps {
        let t = k as f64 / steps as f64;
        dot(
            camera.0 + (aim.0 - camera.0) * t,
            camera.1 + (aim.1 - camera.1) * t,
            1.2,
            RED,
        );
    }
    for k in -10..=10 {
        let k = f64::from(k);
        dot(aim.0 + k, aim.1 + k, 1.2, YELLOW);
        dot(aim.0 + k, aim.1 - k, 1.2, YELLOW);
    }
    dot(camera.0, camera.1, 6.0, RED);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Both ends in frame with room around them, at the finest zoom that fits.
    #[test]
    fn a_close_view_is_framed_at_full_detail() {
        let view = View {
            camera: (53.204932, 6.572974),
            aim: (53.20513, 6.573829),
            pose: None,
        };
        let (z, [x0, y0, x1, y1]) = frame(view);
        assert_eq!(z, MAX_ZOOM);
        for (lat, lng) in [view.camera, view.aim] {
            let (x, y) = world_px(lat, lng, z);
            assert!(x > x0 && x < x1 && y > y0 && y < y1);
        }
        assert!((x1 - x0).max(y1 - y0) <= MAX_SIDE_PX);
    }

    /// A camera a few kilometres from its aim drops zoom rather than asking for
    /// a hundred tiles.
    #[test]
    fn a_far_view_drops_a_zoom() {
        let view = View {
            camera: (53.18, 6.52),
            aim: (53.2053, 6.575),
            pose: None,
        };
        let (z, [x0, y0, x1, y1]) = frame(view);
        assert!(z < MAX_ZOOM);
        assert!((x1 - x0).max(y1 - y0) <= MAX_SIDE_PX || z == MIN_ZOOM);
    }

    /// The camera is marked red and the aim yellow, where they are.
    #[test]
    fn the_camera_and_the_aim_are_marked() {
        let mut img = RgbImage::from_pixel(200, 200, Rgb([0, 0, 0]));
        mark(&mut img, (40.0, 40.0), (160.0, 160.0));
        assert_eq!(*img.get_pixel(40, 40), RED);
        assert_eq!(*img.get_pixel(100, 100), RED, "the line of sight is drawn");
        assert_eq!(*img.get_pixel(168, 152), YELLOW, "the aim is crossed");
    }

    /// **The camera-matched view looks where the camera looks.** Straight ahead
    /// and down 45°, from 10 m up, the middle of the picture meets the ground 10 m
    /// ahead; the top of a view pitched only slightly down is sky.
    #[test]
    fn the_rays_meet_the_ground_where_the_camera_looks() {
        let s = std::f32::consts::FRAC_1_SQRT_2;
        let pose = Pose {
            anchor: (53.2, 6.57, 14),
            position: [0.0, 10.0, 0.0],
            right: [1.0, 0.0, 0.0],
            up: [0.0, s, -s],
            forward: [0.0, -s, -s],
            fov_y: 60f32.to_radians(),
            size: (100, 100),
            ground_y: 0.0,
        };
        let (p, along) = ground_hit(&pose, 50.0, 50.0).expect("the middle hits the ground");
        assert!(p[0].abs() < 1e-3 && (p[1] + 10.0).abs() < 1e-3, "{p:?}");
        assert!((along - 10.0).abs() < 1e-3);
        let level = Pose {
            up: [0.0, 1.0, 0.0],
            forward: [0.0, -0.05, -1.0],
            ..pose
        };
        assert!(
            ground_hit(&level, 50.0, 0.0).is_none(),
            "the top of the view hit the ground"
        );
        assert!(ground_hit(&level, 50.0, 100.0).is_some());
    }

    /// **A high capture is photographed to its corners.** Straight down from
    /// 1.5 km at a 45° field of view, the corners of a 16:9 frame are ~1.3 km
    /// out along the ground; the reference must still be clear there, not haze.
    #[test]
    fn a_high_view_reaches_its_corners() {
        let pose = Pose {
            anchor: (53.2, 6.57, 14),
            position: [0.0, 1500.0, 0.0],
            right: [1.0, 0.0, 0.0],
            up: [0.0, 0.0, -1.0],
            forward: [0.0, -1.0, 0.0],
            fov_y: 45f32.to_radians(),
            size: (1280, 720),
            ground_y: 0.0,
        };
        let (_, corner) = ground_hit(&pose, 0.0, 0.0).expect("the corner hits the ground");
        assert!(
            corner > 900.0,
            "the old fixed reach was shorter than this: {corner}"
        );
        assert!(
            corner < reach(&pose) * 0.7,
            "the corner fades: {corner} vs {}",
            reach(&pose)
        );
        let low = Pose {
            position: [0.0, 10.0, 0.0],
            ..pose
        };
        assert_eq!(reach(&low), MIN_REACH_M, "a low camera keeps the floor");
    }
}
