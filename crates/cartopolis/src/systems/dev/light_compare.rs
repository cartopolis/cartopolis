//! `cartopolis compare`: a capture's light measured against a photograph of
//! the same view.
//!
//! The light used to be tuned by eye against other parts of the render, so every
//! fix moved an error somewhere next to it (`docs/notes/light-against-photographs.md`).
//! This is the other way: a manifest shot names a `reference` photograph (a
//! camera picture, whose pose the shot already is) and `[[shot.region]]`s — a
//! sunlit wall, a wall in shade, the sky overhead, the horizon. For each region
//! it reports how many stops brighter or darker it is than the `anchor` region,
//! in the photograph and in the capture, and how warm it is (log2 of red over
//! blue). Stops from an anchor, not absolute levels, because the phone chose its
//! own exposure and tone curve: what has to match is how the light divides
//! between sun, shade and sky, which no exposure setting changes.
//!
//! Run it after the captures, on the PNGs they left, without starting the app:
//!
//! ```text
//! cargo shots --shots docs/photos.toml --only light
//! cargo run -p cartopolis -- compare docs/photos.toml --only light
//! ```
//!
//! Beside each capture it writes `<shot>.compare.json` (every number) and
//! `<shot>.compare.jpg` (the photograph left, the capture right, each region
//! outlined in its colour from [`PALETTE`]).

use super::shot_presets::{default_manifest_path, Manifest, Preset, Region};
use clap::Parser;
use image::{Rgb, RgbImage};
use std::path::{Path, PathBuf};

/// Measure captures against the photographs their shots name.
#[derive(Parser, Debug)]
struct CompareArgs {
    /// The shot manifest; `docs/photos.toml` when omitted.
    manifest: Option<PathBuf>,
    /// Shots or groups to compare, comma-separated; every shot with a
    /// reference when omitted.
    #[arg(long, value_delimiter = ',')]
    only: Vec<String>,
}

/// The outline colours, in region order, and their names for the report.
pub const PALETTE: [(&str, [u8; 3]); 8] = [
    ("red", [230, 40, 40]),
    ("lime", [60, 230, 60]),
    ("blue", [40, 120, 255]),
    ("yellow", [250, 220, 30]),
    ("magenta", [230, 40, 230]),
    ("cyan", [30, 230, 230]),
    ("orange", [255, 140, 0]),
    ("white", [255, 255, 255]),
];

pub fn requested() -> bool {
    std::env::args_os().nth(1).is_some_and(|a| a == "compare")
}

/// Compare and exit: 0 when every shot was measured, 1 when a file was missing
/// or unreadable (the rest are still measured).
pub fn main() -> ! {
    let args = CompareArgs::parse_from(
        std::iter::once(std::ffi::OsString::from("cartopolis compare"))
            .chain(std::env::args_os().skip(2)),
    );
    let path = args
        .manifest
        .unwrap_or_else(|| default_manifest_path().with_file_name("photos.toml"));
    let manifest = match Manifest::load(&path) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };
    let shots: Vec<&Preset> = if args.only.is_empty() {
        manifest.shots.iter().collect()
    } else {
        match manifest.select(&args.only) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1);
            }
        }
    };
    let base = path.parent().unwrap_or(Path::new("."));
    let mut failed = false;
    let mut compared = 0;
    for shot in shots {
        let Some(reference) = &shot.reference else {
            continue;
        };
        let render_path = manifest.out_path(&path, shot);
        match compare_files(shot, &base.join(reference), &render_path) {
            Ok(report) => {
                print!("{}", report.table(&shot.name));
                compared += 1;
            }
            Err(e) => {
                eprintln!("{}: {e}", shot.name);
                failed = true;
            }
        }
    }
    if compared == 0 && !failed {
        eprintln!("no shot here names a reference photograph");
        failed = true;
    }
    std::process::exit(i32::from(failed));
}

fn compare_files(shot: &Preset, photo: &Path, render: &Path) -> Result<Report, String> {
    let open = |p: &Path| {
        image::open(p)
            .map(|i| i.to_rgb8())
            .map_err(|e| format!("cannot read {}: {e}", p.display()))
    };
    let photo_img = open(photo)?;
    let render_img = open(render)?;
    let report = Report::measure(shot, &photo_img, &render_img);
    let json = serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?;
    let stem = render.with_extension("");
    let json_path = PathBuf::from(format!("{}.compare.json", stem.display()));
    std::fs::write(&json_path, json)
        .map_err(|e| format!("cannot write {}: {e}", json_path.display()))?;
    let jpg_path = PathBuf::from(format!("{}.compare.jpg", stem.display()));
    side_by_side(&photo_img, &render_img, &shot.regions)
        .save(&jpg_path)
        .map_err(|e| format!("cannot write {}: {e}", jpg_path.display()))?;
    Ok(report)
}

/// The light in one rect of one picture.
#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct Patch {
    /// Mean relative luminance of the linear pixels (Rec. 709 weights).
    pub luminance: f32,
    /// The mean linear colour, back in sRGB 0–255, for reading by eye.
    pub srgb: [u8; 3],
    /// log2(red / blue) of the mean linear colour: positive is warm, negative cool.
    pub warmth: f32,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct RegionReport {
    pub name: String,
    pub outline: &'static str,
    pub photo: Patch,
    pub render: Patch,
    /// Stops from the anchor region, in the photograph and in the capture.
    pub photo_stops: f32,
    pub render_stops: f32,
    /// `render_stops − photo_stops`: positive means the capture has this place
    /// too bright for the light on the anchor.
    pub stops_error: f32,
    /// `render.warmth − photo.warmth`, in stops of red over blue.
    pub warmth_error: f32,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Report {
    pub anchor: Option<String>,
    pub regions: Vec<RegionReport>,
    /// Standard deviation of the gamma-encoded luma over the whole picture, the
    /// contrast figure `docs/notes/exposure-and-adaptation.md` compares.
    pub photo_contrast: f32,
    pub render_contrast: f32,
    /// Root mean square of `stops_error` over the regions other than the anchor.
    pub stops_rms: Option<f32>,
    /// Root mean square of `warmth_error` over every region.
    pub warmth_rms: Option<f32>,
}

impl Report {
    pub fn measure(shot: &Preset, photo: &RgbImage, render: &RgbImage) -> Self {
        let anchor_name = shot
            .anchor
            .clone()
            .or_else(|| shot.regions.first().map(|r| r.name.clone()));
        let patches: Vec<(Patch, Patch)> = shot
            .regions
            .iter()
            .map(|r| (patch(photo, r.photo), patch(render, r.render_rect())))
            .collect();
        let anchor = anchor_name
            .as_ref()
            .and_then(|a| shot.regions.iter().position(|r| &r.name == a))
            .map(|i| patches[i]);
        let stops = |p: Patch, a: Patch| (p.luminance.max(1e-6) / a.luminance.max(1e-6)).log2();
        let regions: Vec<RegionReport> = shot
            .regions
            .iter()
            .zip(&patches)
            .enumerate()
            .map(|(i, (r, &(p, q)))| {
                let (ps, rs) = anchor.map_or((0.0, 0.0), |(pa, ra)| (stops(p, pa), stops(q, ra)));
                RegionReport {
                    name: r.name.clone(),
                    outline: PALETTE[i % PALETTE.len()].0,
                    photo: p,
                    render: q,
                    photo_stops: ps,
                    render_stops: rs,
                    stops_error: rs - ps,
                    warmth_error: q.warmth - p.warmth,
                }
            })
            .collect();
        let rms = |v: Vec<f32>| {
            (!v.is_empty()).then(|| (v.iter().map(|e| e * e).sum::<f32>() / v.len() as f32).sqrt())
        };
        let stops_rms = rms(regions
            .iter()
            .filter(|r| Some(&r.name) != anchor_name.as_ref())
            .map(|r| r.stops_error)
            .collect());
        let warmth_rms = rms(regions.iter().map(|r| r.warmth_error).collect());
        Report {
            anchor: anchor_name,
            regions,
            photo_contrast: contrast(photo),
            render_contrast: contrast(render),
            stops_rms,
            warmth_rms,
        }
    }

    fn table(&self, name: &str) -> String {
        let mut out = format!(
            "{name}: anchor {}, contrast photo {:.3} render {:.3}\n",
            self.anchor.as_deref().unwrap_or("-"),
            self.photo_contrast,
            self.render_contrast
        );
        out.push_str(
            "  region           outline   stops photo  render  error   warmth photo  render  error\n",
        );
        for r in &self.regions {
            out.push_str(&format!(
                "  {:<16} {:<8} {:>12.2} {:>7.2} {:>+6.2} {:>13.2} {:>7.2} {:>+6.2}\n",
                r.name,
                r.outline,
                r.photo_stops,
                r.render_stops,
                r.stops_error,
                r.photo.warmth,
                r.render.warmth,
                r.warmth_error
            ));
        }
        if let (Some(s), Some(w)) = (self.stops_rms, self.warmth_rms) {
            out.push_str(&format!("  rms: stops {s:.2}, warmth {w:.2}\n"));
        }
        out
    }
}

fn srgb_to_linear(c: u8) -> f32 {
    let c = f32::from(c) / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_to_srgb(c: f32) -> u8 {
    let c = c.clamp(0.0, 1.0);
    let s = if c <= 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    };
    (s * 255.0).round() as u8
}

/// A fractional rect as whole pixels, at least one pixel each way.
fn pixel_rect(img: &RgbImage, [x, y, w, h]: [f32; 4]) -> (u32, u32, u32, u32) {
    let (iw, ih) = (img.width() as f32, img.height() as f32);
    let x0 = ((x * iw).floor() as u32).min(img.width().saturating_sub(1));
    let y0 = ((y * ih).floor() as u32).min(img.height().saturating_sub(1));
    let x1 = (((x + w) * iw).ceil() as u32).clamp(x0 + 1, img.width());
    let y1 = (((y + h) * ih).ceil() as u32).clamp(y0 + 1, img.height());
    (x0, y0, x1, y1)
}

/// The mean linear light of a rect: a mean of light, not of sRGB codes, since a
/// mix of sun and shade in one rect adds as light does.
pub fn patch(img: &RgbImage, rect: [f32; 4]) -> Patch {
    let (x0, y0, x1, y1) = pixel_rect(img, rect);
    let mut sum = [0.0f64; 3];
    for y in y0..y1 {
        for x in x0..x1 {
            let p = img.get_pixel(x, y);
            for c in 0..3 {
                sum[c] += f64::from(srgb_to_linear(p[c]));
            }
        }
    }
    let n = f64::from((x1 - x0) * (y1 - y0));
    let m = sum.map(|s| (s / n) as f32);
    Patch {
        luminance: 0.2126 * m[0] + 0.7152 * m[1] + 0.0722 * m[2],
        srgb: m.map(linear_to_srgb),
        warmth: (m[0].max(1e-6) / m[2].max(1e-6)).log2(),
    }
}

/// Standard deviation of Rec. 709 luma over the gamma-encoded picture.
pub fn contrast(img: &RgbImage) -> f32 {
    let n = f64::from(img.width() * img.height()).max(1.0);
    let luma = |p: &Rgb<u8>| {
        (0.2126 * f64::from(p[0]) + 0.7152 * f64::from(p[1]) + 0.0722 * f64::from(p[2])) / 255.0
    };
    // Two passes: the one-pass E[y²] − E[y]² leaves rounding where there is none.
    let mean = img.pixels().map(luma).sum::<f64>() / n;
    let var = img.pixels().map(|p| (luma(p) - mean).powi(2)).sum::<f64>() / n;
    var.sqrt() as f32
}

/// The photograph left, scaled to the capture's height, and the capture right,
/// each region outlined in its palette colour.
fn side_by_side(photo: &RgbImage, render: &RgbImage, regions: &[Region]) -> RgbImage {
    let h = render.height();
    let pw = (u64::from(photo.width()) * u64::from(h) / u64::from(photo.height().max(1))) as u32;
    let mut photo =
        image::imageops::resize(photo, pw.max(1), h, image::imageops::FilterType::Triangle);
    let mut render = render.clone();
    for (i, r) in regions.iter().enumerate() {
        let colour = Rgb(PALETTE[i % PALETTE.len()].1);
        outline(&mut photo, r.photo, colour);
        outline(&mut render, r.render_rect(), colour);
    }
    let mut out = RgbImage::new(photo.width() + render.width(), h);
    image::imageops::replace(&mut out, &photo, 0, 0);
    image::imageops::replace(&mut out, &render, i64::from(photo.width()), 0);
    out
}

fn outline(img: &mut RgbImage, rect: [f32; 4], colour: Rgb<u8>) {
    let (x0, y0, x1, y1) = pixel_rect(img, rect);
    for t in 0..2 {
        for x in x0..x1 {
            for y in [y0 + t, y1.saturating_sub(1 + t)] {
                if y < img.height() {
                    img.put_pixel(x, y, colour);
                }
            }
        }
        for y in y0..y1 {
            for x in [x0 + t, x1.saturating_sub(1 + t)] {
                if x < img.width() {
                    img.put_pixel(x, y, colour);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn preset(toml_text: &str) -> Preset {
        let m: Manifest = toml::from_str(toml_text).expect("parses");
        m.shots.into_iter().next().expect("one shot")
    }

    const SHOT: &str = r#"
[[shot]]
name = "street"
at = [53.2, 6.5, 1.6]
reference = "street.jpg"
anchor = "sun"
[[shot.region]]
name = "sun"
photo = [0.0, 0.0, 0.5, 1.0]
[[shot.region]]
name = "shade"
photo = [0.5, 0.0, 0.5, 1.0]
"#;

    /// Left half one grey, right half another.
    fn halves(left: u8, right: u8) -> RgbImage {
        RgbImage::from_fn(40, 20, |x, _| {
            let v = if x < 20 { left } else { right };
            Rgb([v, v, v])
        })
    }

    #[test]
    fn a_shade_one_stop_darker_reads_minus_one() {
        // sRGB 186 is linear ~0.49, 255 is 1.0: one stop apart, near enough.
        let img = halves(255, 186);
        let r = Report::measure(&preset(SHOT), &img, &img);
        assert!((r.regions[1].photo_stops + 1.0).abs() < 0.05, "{r:?}");
        assert!(r.regions[1].stops_error.abs() < 1e-6);
        assert_eq!(r.stops_rms, Some(0.0));
    }

    #[test]
    fn an_exposure_difference_alone_is_no_error() {
        // The capture two stops darker everywhere: the split between sun and
        // shade is the same, so nothing is wrong with the light.
        let photo = halves(255, 186);
        let render = halves(137, 99);
        let r = Report::measure(&preset(SHOT), &photo, &render);
        assert!(r.regions[1].stops_error.abs() < 0.1, "{r:?}");
    }

    #[test]
    fn a_shade_too_bright_in_the_capture_is_a_positive_error() {
        let photo = halves(255, 99);
        let render = halves(255, 200);
        let r = Report::measure(&preset(SHOT), &photo, &render);
        assert!(r.regions[1].stops_error > 1.0, "{r:?}");
    }

    #[test]
    fn warmth_is_red_over_blue() {
        let img = RgbImage::from_pixel(4, 4, Rgb([255, 128, 64]));
        assert!(patch(&img, [0.0, 0.0, 1.0, 1.0]).warmth > 2.0);
        let img = RgbImage::from_pixel(4, 4, Rgb([64, 128, 255]));
        assert!(patch(&img, [0.0, 0.0, 1.0, 1.0]).warmth < -2.0);
    }

    #[test]
    fn a_flat_picture_has_no_contrast() {
        assert!(contrast(&RgbImage::from_pixel(8, 8, Rgb([90, 90, 90]))) < 1e-6);
        assert!(contrast(&halves(0, 255)) > 0.45);
    }

    #[test]
    fn regions_without_a_photograph_are_refused() {
        let text = SHOT.replace("reference = \"street.jpg\"\n", "");
        let m: Manifest = toml::from_str(&text).expect("parses");
        assert!(m.validate().is_err());
    }

    #[test]
    fn a_rect_outside_the_picture_is_refused() {
        let text = SHOT.replace("[0.5, 0.0, 0.5, 1.0]", "[0.6, 0.0, 0.5, 1.0]");
        let m: Manifest = toml::from_str(&text).expect("parses");
        assert!(m.validate().unwrap_err().contains("must lie inside"));
    }

    #[test]
    fn an_anchor_that_is_no_region_is_refused() {
        let text = SHOT.replace("anchor = \"sun\"", "anchor = \"sunn\"");
        let m: Manifest = toml::from_str(&text).expect("parses");
        assert!(m.validate().unwrap_err().contains("not one of its regions"));
    }

    #[test]
    fn the_side_by_side_is_both_pictures_wide() {
        let photo = RgbImage::new(80, 40);
        let render = RgbImage::new(40, 20);
        let out = side_by_side(&photo, &render, &preset(SHOT).regions);
        assert_eq!((out.width(), out.height()), (80, 20));
    }
}
