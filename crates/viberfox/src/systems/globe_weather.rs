//! Live global cloud cover for the planet cloud layer (ADR-032).
//!
//! Fetches NOAA **GFS Total Cloud Cover** (entire atmosphere) from the NOMADS
//! GRIB filter, decodes the GRIB2 with `gribberish`, and folds it into the coarse
//! equirectangular texture that `shaders/globe_clouds.wgsl` samples as its
//! coverage field. Real weather sets *where* clouds are; the shader's noise still
//! synthesizes their shape and motion (so this is data, not a pasted satellite
//! photo — see the ADR).
//!
//! GFS is US-government **public domain** (no key, no attribution). We request the
//! 1.0° product's `f000` analysis, which carries a single global instantaneous
//! `TCDC:entire atmosphere` record — 360×181 points, north row first, percent.
//!
//! Same worker-thread + `ureq` shape as [`super::weather`]: one blocking fetch on a
//! background thread, results handed back through an `Arc<Mutex<_>>` and folded on
//! the main thread, where the pixels are written into the existing `Image` (no new
//! handle, no churn). Weight ramps 0→1 so the live field fades in over the
//! procedural fallback. Fetch/decode failures are logged and simply leave the
//! weight where it is — the shader keeps using its procedural bands.

use std::io::Read;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bevy::asset::RenderAssetUsages;
use bevy::image::{ImageAddressMode, ImageSampler, ImageSamplerDescriptor};
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};

/// GFS 1.0° grid: 360 longitudes (0°→359°E) × 181 latitudes (+90°→−90°).
const GRID_W: u32 = 360;
const GRID_H: u32 = 181;

/// NOMADS etiquette: identify the app (mirrors [`super::weather`]).
const USER_AGENT: &str =
    "viberfox/0.1 (https://code.garage44.eu/garage44/viberfox; jvanveen@protonmail.com)";

/// Re-fetch cadence. GFS runs 00/06/12/18 UTC; a 3 h check comfortably picks up a
/// new cycle soon after it posts without hammering NOMADS.
const REFRESH: Duration = Duration::from_secs(3 * 3600);

/// How fast the live field fades in over the procedural fallback (per second).
const WEIGHT_RAMP: f32 = 0.15;

/// A decoded cloud-cover grid, already packed as `R8` texels (row-major, north row
/// first, r = cloud fraction scaled 0..255). Ready to drop into the `Image`.
struct CloudGrid {
    texels: Vec<u8>,
}

type FetchResult = Result<CloudGrid, String>;

/// Live global cloud state + the texture the globe cloud material samples.
#[derive(Resource)]
pub struct GlobeCloudData {
    /// The coverage texture (created as a placeholder, overwritten in place).
    pub image: Handle<Image>,
    /// 0 = pure procedural fallback, ramps to 1 once real data has landed. Read by
    /// `update_globe_clouds` into the material's `frame_ref.w`.
    pub weight: f32,
    have_data: bool,
    results: Arc<Mutex<Vec<FetchResult>>>,
    worker: Option<Sender<()>>,
    in_flight: bool,
    last_poll: Option<Instant>,
}

impl GlobeCloudData {
    pub fn new(image: Handle<Image>) -> Self {
        Self {
            image,
            weight: 0.0,
            have_data: false,
            results: Arc::new(Mutex::new(Vec::new())),
            worker: None,
            in_flight: false,
            last_poll: None,
        }
    }
}

/// Longitude wraps at the 0°/360° seam (Repeat U); latitude clamps at the poles.
fn grid_sampler() -> ImageSampler {
    ImageSampler::Descriptor(ImageSamplerDescriptor {
        address_mode_u: ImageAddressMode::Repeat,
        address_mode_v: ImageAddressMode::ClampToEdge,
        address_mode_w: ImageAddressMode::ClampToEdge,
        ..ImageSamplerDescriptor::linear()
    })
}

fn image_from_texels(texels: Vec<u8>) -> Image {
    let mut image = Image::new(
        Extent3d {
            width: GRID_W,
            height: GRID_H,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        texels,
        TextureFormat::R8Unorm,
        RenderAssetUsages::default(),
    );
    image.sampler = grid_sampler();
    image
}

/// Mid-grey grid at the live dimensions, so the material's texture binding is valid
/// from frame 0 (weight stays 0 until a fetch lands, so its value is unused).
pub fn placeholder_grid() -> Image {
    image_from_texels(vec![128u8; (GRID_W * GRID_H) as usize])
}

/// Fetch the global cloud field on a worker thread and fold it into the texture.
pub fn poll_globe_clouds(
    data: Option<ResMut<GlobeCloudData>>,
    mut images: ResMut<Assets<Image>>,
    time: Res<Time>,
) {
    let Some(mut data) = data else {
        return; // clouds disabled (VIBERFOX_NO_CLOUDS) — no resource inserted.
    };

    // Verification aid: block-fetch once so a `--shot` captures real data (the
    // async path below + the weight ramp otherwise land after the frame is
    // captured). Set `VIBERFOX_CLOUD_DATA_SYNC=1`. Like `VIBERFOX_CLOUD_TIME`.
    if !data.have_data && std::env::var("VIBERFOX_CLOUD_DATA_SYNC").is_ok() {
        match fetch_tcdc() {
            Ok(grid) => {
                if let Some(mut image) = images.get_mut(&data.image) {
                    image.data = Some(grid.texels);
                }
                data.have_data = true;
                data.weight = 1.0;
                data.last_poll = Some(Instant::now());
                tracing::info!("GFS global cloud cover loaded synchronously (verify)");
            }
            Err(e) => tracing::warn!(error = %e, "sync GFS cloud fetch failed"),
        }
    }

    // Start the fetch worker once.
    if data.worker.is_none() {
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        data.worker = Some(tx);
        let results = data.results.clone();
        std::thread::spawn(move || {
            while rx.recv().is_ok() {
                let fetched = fetch_tcdc();
                if let Ok(mut g) = results.lock() {
                    g.push(fetched);
                }
            }
        });
    }

    // Fold in completed fetches: overwrite the texture's pixels in place.
    let done: Vec<FetchResult> = data
        .results
        .lock()
        .map(|mut g| g.drain(..).collect())
        .unwrap_or_default();
    for result in done {
        data.in_flight = false;
        match result {
            Ok(grid) => {
                if let Some(mut image) = images.get_mut(&data.image) {
                    image.data = Some(grid.texels);
                }
                data.have_data = true;
                tracing::info!("GFS global cloud cover updated");
            }
            Err(e) => tracing::warn!(error = %e, "GFS cloud-cover fetch failed"),
        }
    }

    // Fade the live field in once we have data.
    if data.have_data && data.weight < 1.0 {
        data.weight = (data.weight + WEIGHT_RAMP * time.delta_secs()).min(1.0);
    }

    // Kick off a refetch on the cadence (or the first tick).
    let stale = data.last_poll.is_none_or(|t| t.elapsed() >= REFRESH);
    if data.in_flight || !stale {
        return;
    }
    data.last_poll = Some(Instant::now());
    data.in_flight = true;
    if let Some(worker) = &data.worker {
        let _ = worker.send(());
    }
}

/// The most recent GFS cycle guaranteed to have posted its `f000` file: floor "now
/// minus 6 h" to a 6-hour boundary. Returns `(YYYYMMDD, HH)`.
fn latest_cycle() -> (String, String) {
    const SIX_H: i64 = 6 * 3600;
    let now = jiff::Timestamp::now().as_second();
    let cycle = ((now - SIX_H).div_euclid(SIX_H)) * SIX_H;
    let z = jiff::Timestamp::from_second(cycle)
        .unwrap_or(jiff::Timestamp::UNIX_EPOCH)
        .to_zoned(jiff::tz::TimeZone::UTC);
    let d = z.date();
    (
        format!("{:04}{:02}{:02}", d.year(), d.month(), d.day()),
        format!("{:02}", z.hour()),
    )
}

/// Blocking fetch of one global `TCDC:entire atmosphere` field (worker thread).
fn fetch_tcdc() -> FetchResult {
    let (date, hh) = latest_cycle();
    // NOMADS GRIB filter, 1.0° product: subset server-side to just this one field
    // over the whole globe (omit `subregion`). ~70 KB.
    let url = format!(
        "https://nomads.ncep.noaa.gov/cgi-bin/filter_gfs_1p00.pl\
         ?dir=%2Fgfs.{date}%2F{hh}%2Fatmos\
         &file=gfs.t{hh}z.pgrb2.1p00.f000\
         &var_TCDC=on&lev_entire_atmosphere=on"
    );
    let resp = ureq::get(&url)
        .set("User-Agent", USER_AGENT)
        .timeout(Duration::from_secs(30))
        .call()
        .map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    resp.into_reader()
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() < 4 || &bytes[0..4] != b"GRIB" {
        // A missing cycle / bad params returns an HTML error, not GRIB.
        return Err(format!("not a GRIB2 body ({} bytes)", bytes.len()));
    }
    decode_tcdc(&bytes)
}

/// Decode the GRIB2 TCDC message into `R8` texels (percent → 0..255 fraction).
fn decode_tcdc(bytes: &[u8]) -> FetchResult {
    let messages: Vec<_> = gribberish::message::read_messages(bytes).collect();
    let msg = messages.first().ok_or_else(|| "no GRIB messages".to_string())?;

    let (ny, nx) = msg.grid_dimensions().map_err(|e| e.to_string())?;
    if nx != GRID_W as usize || ny != GRID_H as usize {
        return Err(format!("unexpected GFS grid {ny}×{nx}"));
    }
    // Flat Vec<f64>, GRIB scan order: row-major, north row first — the exact layout
    // the shader's equirectangular lookup expects, so no row flip is needed.
    let values = msg.data().map_err(|e| e.to_string())?;
    if values.len() != (GRID_W * GRID_H) as usize {
        return Err(format!("unexpected value count {}", values.len()));
    }
    let texels = values
        .iter()
        .map(|&v| {
            let frac = (v as f32 / 100.0).clamp(0.0, 1.0);
            (frac * 255.0 + 0.5) as u8
        })
        .collect();
    Ok(CloudGrid { texels })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cycle_is_a_six_hour_boundary_in_the_past() {
        let (date, hh) = latest_cycle();
        assert_eq!(date.len(), 8);
        let hour: i32 = hh.parse().unwrap();
        assert!(matches!(hour, 0 | 6 | 12 | 18), "cycle hour {hour} not a GFS run");
    }

    #[test]
    fn placeholder_is_full_grid() {
        let img = placeholder_grid();
        assert_eq!(img.width(), GRID_W);
        assert_eq!(img.height(), GRID_H);
    }

    #[test]
    fn non_grib_body_is_rejected() {
        assert!(decode_tcdc(b"<html>404</html>").is_err());
    }
}
