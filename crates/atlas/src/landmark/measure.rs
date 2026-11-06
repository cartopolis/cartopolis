//! Measuring a building in metres: a cut through the laser's surface, and how
//! far each wall of a kit stands from the laser's wall.
//!
//! `facts.md` gives a builder the plan and the heights the laser clusters at;
//! what it could not give was a *section* — the drawing an architect reads
//! sizes off: where a ledge steps in, how high the eaves are, how a roof
//! pitches — or a per-wall answer to "which face is wrong, and by how much".
//! `score` compares from above only, as areas per height band, which says a
//! stage is too wide but not which side. Both answers here are in the kit's own
//! local frame, so a number read off one goes straight into the kit.

use std::fmt::Write as _;
use std::path::Path;

use super::kit::{Kit, Piece};
use crate::pipeline::models::{Plane, Surface};

/// A building as these tools see it: its frame, its outline, the laser round
/// it, and the kit's pieces when a kit was given.
pub struct Site {
    pub name: String,
    pub pand: String,
    pub plane: Plane,
    /// Compass bearing of local `+y`, as a kit's `rotation_deg`.
    pub rotation_deg: f64,
    /// The register outline, local metres.
    pub outline: Vec<[f64; 2]>,
    pub dsm: Surface,
    pub ground: f64,
    /// The kit's pieces, east/north/up of [`Self::plane`] (as `Kit::mesh`
    /// gives them). Empty without a kit.
    pub pieces: Vec<Piece>,
    pub kit: Option<Kit>,
}

impl Site {
    /// A kit file (`*.json`), in its own frame, or a BAG identificatie, in the
    /// frame `gather` would give it. `margin` metres of laser round the outline.
    pub fn load(target: &str, margin: f64) -> Result<Self, String> {
        let (kit, pand) = if target.ends_with(".json") {
            let text = std::fs::read_to_string(target).map_err(|e| format!("{target}: {e}"))?;
            let kit = Kit::parse(&text)?;
            let pand = kit.pand.clone();
            (Some(kit), pand)
        } else {
            (None, target.trim().to_string())
        };
        let ll = crate::pipeline::models::bag_outline(&pand)?;
        let (plane, rotation_deg, name) = match &kit {
            Some(k) => (
                Plane::at(k.origin.0, k.origin.1),
                k.rotation_deg,
                k.name.clone(),
            ),
            None => {
                let f = super::gather::frame(&ll);
                (f.plane, f.rotation_deg, format!("Building {pand}"))
            }
        };
        let mut site = Self {
            name,
            pand,
            plane,
            rotation_deg,
            outline: Vec::new(),
            dsm: Surface::from_raster(plane, [0.0, 0.0], 1.0, 0, Vec::new()),
            ground: 0.0,
            pieces: Vec::new(),
            kit: None,
        };
        site.outline = ll
            .iter()
            .map(|p| site.to_local(plane.to_en(p.0, p.1)))
            .collect();
        let reach = site
            .outline
            .iter()
            .map(|q| q[0].hypot(q[1]))
            .fold(0.0, f64::max);
        let (dsm, dtm) = super::gather::surfaces(plane, (reach + margin).ceil())?;
        let outline_en: Vec<[f64; 2]> = site.outline.iter().map(|&q| site.to_en(q)).collect();
        site.ground = super::gather::ground(&dtm, &outline_en)
            .ok_or("no bare-earth heights round the building")?;
        site.dsm = dsm;
        if let Some(k) = &kit {
            site.pieces = k.mesh()?;
        }
        site.kit = kit;
        Ok(site)
    }

    /// Local metres to east/north, as `Kit::to_en`.
    #[must_use]
    pub fn to_en(&self, q: [f64; 2]) -> [f64; 2] {
        let (s, c) = self.rotation_deg.to_radians().sin_cos();
        [q[0] * c + q[1] * s, -q[0] * s + q[1] * c]
    }

    /// East/north to local metres.
    #[must_use]
    pub fn to_local(&self, en: [f64; 2]) -> [f64; 2] {
        let (s, c) = self.rotation_deg.to_radians().sin_cos();
        [en[0] * c - en[1] * s, en[0] * s + en[1] * c]
    }

    /// The laser's height above the ground at a local point.
    #[must_use]
    pub fn laser(&self, q: [f64; 2]) -> Option<f64> {
        self.dsm
            .at(self.to_en(q))
            .map(|h| f64::from(h) - self.ground)
    }

    /// The kit's highest surface over a local point, above the ground: what
    /// the model shows from above there. `None` where it has nothing.
    #[must_use]
    pub fn model(&self, q: [f64; 2]) -> Option<f64> {
        top_at(&self.pieces, self.to_en(q))
    }
}

/// The highest non-vertical triangle over an east/north point.
fn top_at(pieces: &[Piece], p: [f64; 2]) -> Option<f64> {
    let mut best: Option<f64> = None;
    for piece in pieces {
        for t in &piece.t {
            let [a, b, c] = t.map(|i| piece.p[i as usize]);
            let det = (b[0] - a[0]) * (c[1] - a[1]) - (c[0] - a[0]) * (b[1] - a[1]);
            if det.abs() < 1e-9 {
                continue;
            }
            let l1 = ((b[1] - c[1]) * (p[0] - c[0]) + (c[0] - b[0]) * (p[1] - c[1])) / det;
            let l2 = ((c[1] - a[1]) * (p[0] - c[0]) + (a[0] - c[0]) * (p[1] - c[1])) / det;
            let l3 = 1.0 - l1 - l2;
            if l1 < -1e-9 || l2 < -1e-9 || l3 < -1e-9 {
                continue;
            }
            let z = l1 * a[2] + l2 * b[2] + l3 * c[2];
            best = Some(best.map_or(z, |m: f64| m.max(z)));
        }
    }
    best
}

/// `"x,y"` in local metres.
pub fn parse_xy(s: &str) -> Result<[f64; 2], String> {
    let v: Vec<f64> = s
        .split(',')
        .map(|t| t.trim().parse::<f64>())
        .collect::<Result<_, _>>()
        .map_err(|_| format!("`{s}` is not x,y"))?;
    match v.as_slice() {
        [x, y] => Ok([*x, *y]),
        _ => Err(format!("`{s}` is not x,y")),
    }
}

// ---------------------------------------------------------------------------
// The section
// ---------------------------------------------------------------------------

/// Sample spacing along a section, metres: half the laser's own pixel, so a
/// step is placed to a quarter of a metre.
const SECTION_STEP_M: f64 = 0.25;
/// A height change of at least this over one sample is a step (a wall, a
/// ledge), not a pitch.
const STEP_JUMP_M: f64 = 0.75;
/// Flat within this, for at least [`FLAT_MIN_M`], is a level: a flat roof, a
/// ledge, a gallery.
const FLAT_TOL_M: f64 = 0.3;
const FLAT_MIN_M: f64 = 1.5;
/// Pixels per metre in `section.png`, both ways, so it measures like a drawing.
const SECTION_PX_PER_M: f64 = 12.0;

/// One sample of a section: distance along the cut, laser and model heights.
pub struct Sample {
    pub t: f64,
    pub laser: Option<f64>,
    pub model: Option<f64>,
}

/// The samples along a cut through `through` (local), heading `bearing_deg`
/// in the local frame (0 along `+y`, 90 along `+x`), reaching `reach` metres
/// either side.
#[must_use]
pub fn section(site: &Site, through: [f64; 2], bearing_deg: f64, reach: f64) -> Vec<Sample> {
    let (s, c) = bearing_deg.to_radians().sin_cos();
    let n = (reach / SECTION_STEP_M).ceil() as i64;
    (-n..=n)
        .map(|k| {
            let t = k as f64 * SECTION_STEP_M;
            let q = [through[0] + t * s, through[1] + t * c];
            Sample {
                t,
                laser: site.laser(q),
                model: site.model(q).filter(|h| *h > 0.05),
            }
        })
        .collect()
}

/// Where the heights jump: `(from t, to t, from height, to height)`.
///
/// Jumps the same way within half a metre of each other are one step, spread
/// over its `t` range: the laser smears a tall wall's face over several
/// pixels, and the Martinitoren's flank read as thirty "steps" a metre apart
/// before they were joined (2026-11-06).
fn steps(samples: &[Sample], pick: impl Fn(&Sample) -> Option<f64>) -> Vec<(f64, f64, f64, f64)> {
    let mut out: Vec<(f64, f64, f64, f64)> = Vec::new();
    for w in samples.windows(2) {
        let (a, b) = (pick(&w[0]).unwrap_or(0.0), pick(&w[1]).unwrap_or(0.0));
        if (b - a).abs() < STEP_JUMP_M {
            continue;
        }
        let t = (w[0].t + w[1].t) / 2.0;
        match out.last_mut() {
            Some(last) if t - last.1 <= 0.5 + 1e-9 && (b > a) == (last.3 > last.2) => {
                last.1 = t;
                last.3 = b;
            }
            _ => out.push((t, t, a, b)),
        }
    }
    out
}

fn step_line(s: (f64, f64, f64, f64)) -> String {
    if s.1 - s.0 < 1e-9 {
        format!("- t = {:+.2} m: {:.1} → {:.1} m", s.0, s.2, s.3)
    } else {
        format!(
            "- t {:+.2} … {:+.2} m: {:.1} → {:.1} m (spread over {:.2} m: a sloped face, or a \
             wall the laser smears)",
            s.0,
            s.1,
            s.2,
            s.3,
            s.1 - s.0
        )
    }
}

/// The level stretches: `(from t, to t, height)`, heights above a metre only.
fn levels(samples: &[Sample]) -> Vec<(f64, f64, f64)> {
    let mut out = Vec::new();
    let mut start = 0;
    while start < samples.len() {
        let Some(h0) = samples[start].laser else {
            start += 1;
            continue;
        };
        let mut end = start;
        let (mut lo, mut hi) = (h0, h0);
        while end + 1 < samples.len() {
            let Some(h) = samples[end + 1].laser else {
                break;
            };
            if h.max(hi) - h.min(lo) > FLAT_TOL_M {
                break;
            }
            lo = lo.min(h);
            hi = hi.max(h);
            end += 1;
        }
        let len = samples[end].t - samples[start].t;
        if len >= FLAT_MIN_M && hi > 1.0 {
            let mean = samples[start..=end]
                .iter()
                .filter_map(|s| s.laser)
                .sum::<f64>()
                / (end - start + 1) as f64;
            out.push((samples[start].t, samples[end].t, mean));
        }
        start = end + 1;
    }
    out
}

/// `section.md` and `section.png` into `out`; answers the headline.
pub fn section_cmd(
    target: &str,
    through: Option<&str>,
    bearing_deg: f64,
    out: &Path,
) -> Result<String, String> {
    let site = Site::load(target, 20.0)?;
    let through = through.map(parse_xy).transpose()?.unwrap_or([0.0, 0.0]);
    let reach = site
        .outline
        .iter()
        .map(|q| (q[0] - through[0]).hypot(q[1] - through[1]))
        .fold(0.0, f64::max)
        + 6.0;
    let samples = section(&site, through, bearing_deg, reach);
    std::fs::create_dir_all(out).map_err(|e| e.to_string())?;
    let png = section_png(&samples);
    png.save(out.join("section.png"))
        .map_err(|e| e.to_string())?;

    let mut md = String::new();
    let (s, c) = bearing_deg.to_radians().sin_cos();
    let _ = writeln!(
        md,
        "# {} — section through ({:.2}, {:.2}), heading {bearing_deg:.0}° in the local frame\n\n\
         `t` runs along the cut: local (x, y) = ({:.2} + {s:.3}·t, {:.2} + {c:.3}·t). \
         Heights are metres above the ground ({:.2} m NAP). The laser is AHN's surface \
         model at half a metre; it does not see a thin spire, and smears a wall over one \
         pixel.\n",
        site.name, through[0], through[1], through[0], through[1], site.ground
    );
    let _ = writeln!(
        md,
        "`section.png`: {SECTION_PX_PER_M:.0} px per metre both ways, `t` left to right, \
         the ground at the bottom; grid every metre, darker every 5 m and at t = 0. Grey: \
         the laser. Red line: the kit's top surface, where a kit was given.\n"
    );
    let _ = writeln!(md, "## Steps in the laser (walls, ledges)\n");
    for s in steps(&samples, |s| s.laser) {
        let _ = writeln!(md, "{}", step_line(s));
    }
    let _ = writeln!(md, "\n## Levels in the laser (flat for ≥ {FLAT_MIN_M} m)\n");
    for (a, b, h) in levels(&samples) {
        let _ = writeln!(
            md,
            "- t {a:+.2} … {b:+.2} m ({:.2} m long) at {h:.2} m",
            b - a
        );
    }
    if !site.pieces.is_empty() {
        let _ = writeln!(md, "\n## Steps in the kit\n");
        for s in steps(&samples, |s| s.model) {
            let _ = writeln!(md, "{}", step_line(s));
        }
    }
    let _ = writeln!(
        md,
        "\n## Every half metre\n\n| t m | laser m | kit m |\n|---|---|---|"
    );
    for s in samples.iter().step_by(2) {
        let f = |h: Option<f64>| h.map_or("—".to_string(), |h| format!("{h:.2}"));
        let _ = writeln!(md, "| {:+.2} | {} | {} |", s.t, f(s.laser), f(s.model));
    }
    std::fs::write(out.join("section.md"), &md).map_err(|e| e.to_string())?;
    let top = samples.iter().filter_map(|s| s.laser).fold(0.0, f64::max);
    Ok(format!(
        "wrote {}: section.md, section.png — {} steps, {} levels, laser top {top:.1} m",
        out.display(),
        steps(&samples, |s| s.laser).len(),
        levels(&samples).len()
    ))
}

/// The section drawn to scale: the laser filled grey, the kit as a red line.
fn section_png(samples: &[Sample]) -> image::RgbImage {
    let t0 = samples.first().map_or(0.0, |s| s.t);
    let t1 = samples.last().map_or(1.0, |s| s.t);
    let top = samples
        .iter()
        .flat_map(|s| [s.laser, s.model])
        .flatten()
        .fold(1.0, f64::max)
        + 2.0;
    let w = ((t1 - t0) * SECTION_PX_PER_M).ceil() as u32 + 1;
    let h = (top * SECTION_PX_PER_M).ceil() as u32 + 1;
    let mut img = image::RgbImage::from_pixel(w, h, image::Rgb([255, 255, 255]));
    let col = |t: f64| ((t - t0) * SECTION_PX_PER_M).round() as i64;
    let row = |z: f64| (f64::from(h - 1) - z * SECTION_PX_PER_M).round() as i64;
    // The grid: a metre light, five metres and t = 0 darker.
    for m in 0..=(top.ceil() as i64) {
        let shade = if m % 5 == 0 { 170 } else { 225 };
        let r = row(m as f64);
        for x in 0..w as i64 {
            put(&mut img, x, r, [shade; 3]);
        }
    }
    for m in (t0.floor() as i64)..=(t1.ceil() as i64) {
        let shade = if m == 0 {
            90
        } else if m % 5 == 0 {
            170
        } else {
            225
        };
        let x = col(m as f64);
        for y in 0..h as i64 {
            put(&mut img, x, y, [shade; 3]);
        }
    }
    for s in samples {
        if let Some(l) = s.laser {
            let x = col(s.t);
            for dx in 0..(SECTION_PX_PER_M * SECTION_STEP_M).ceil() as i64 {
                for y in row(l.max(0.0))..h as i64 {
                    put(&mut img, x + dx, y, [120, 120, 120]);
                }
            }
        }
    }
    for w2 in samples.windows(2) {
        if let (Some(a), Some(b)) = (w2[0].model, w2[1].model) {
            line(
                &mut img,
                [col(w2[0].t), row(a)],
                [col(w2[1].t), row(b)],
                [220, 30, 30],
                2,
            );
        }
    }
    img
}

pub(crate) fn put(img: &mut image::RgbImage, x: i64, y: i64, c: [u8; 3]) {
    if x >= 0 && y >= 0 && (x as u32) < img.width() && (y as u32) < img.height() {
        img.put_pixel(x as u32, y as u32, image::Rgb(c));
    }
}

/// A straight line `width` pixels thick.
pub(crate) fn line(img: &mut image::RgbImage, a: [i64; 2], b: [i64; 2], c: [u8; 3], width: i64) {
    let n = (b[0] - a[0]).abs().max((b[1] - a[1]).abs()).max(1);
    for k in 0..=n {
        let x = a[0] + (b[0] - a[0]) * k / n;
        let y = a[1] + (b[1] - a[1]) * k / n;
        for dx in 0..width {
            for dy in 0..width {
                put(img, x + dx - width / 2, y + dy - width / 2, c);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Walls against the laser
// ---------------------------------------------------------------------------

/// How far one wall of the kit stands from the laser's wall.
pub struct WallFit {
    pub piece: String,
    /// Compass bearing (local frame) the wall faces.
    pub faces_deg: f64,
    /// The wall's middle, local metres, and its height range above the ground.
    pub at: [f64; 2],
    pub from_m: f64,
    pub to_m: f64,
    pub width_m: f64,
    /// Points that found the laser's edge.
    pub samples: usize,
    /// Median distance to move the wall outward (negative: inward), metres.
    pub move_m: f64,
    /// Half the spread of the middle half of the samples, metres.
    pub spread_m: f64,
}

/// Samples on a wall, metres apart.
const WALL_STEP_M: f64 = 0.5;
/// How far either side of a wall the laser's edge is looked for.
const WALL_SEARCH_M: f64 = 4.0;
/// A wall narrower than this, or with fewer points that found the laser's
/// edge, is a detail the half-metre laser cannot place: a pinnacle's face, a
/// clock dial, a buttress's return. The Martinitoren's read as moves of 3 m.
const MIN_WALL_WIDTH_M: f64 = 1.0;
const MIN_WALL_POINTS: usize = 10;

/// The kit's tops on a 10 cm grid, east/north: what stands where, from above,
/// to tell a face the kit hides behind another of its own.
struct Covers {
    min: [f64; 2],
    nx: usize,
    ny: usize,
    top: Vec<f64>,
}

impl Covers {
    const STEP: f64 = 0.1;

    fn new(pieces: &[Piece]) -> Self {
        let (mut lo, mut hi) = ([f64::INFINITY; 2], [f64::NEG_INFINITY; 2]);
        for p in pieces.iter().flat_map(|p| &p.p) {
            lo = [lo[0].min(p[0]), lo[1].min(p[1])];
            hi = [hi[0].max(p[0]), hi[1].max(p[1])];
        }
        if !lo[0].is_finite() {
            return Self {
                min: [0.0; 2],
                nx: 0,
                ny: 0,
                top: Vec::new(),
            };
        }
        let nx = ((hi[0] - lo[0]) / Self::STEP).ceil() as usize + 1;
        let ny = ((hi[1] - lo[1]) / Self::STEP).ceil() as usize + 1;
        Self {
            min: lo,
            nx,
            ny,
            top: super::score::tops(pieces, lo, nx, ny, Self::STEP),
        }
    }

    /// Whether the kit stands at least `z` high over an east/north point.
    fn at(&self, p: [f64; 2], z: f64) -> bool {
        let c = ((p[0] - self.min[0]) / Self::STEP).floor();
        let r = ((p[1] - self.min[1]) / Self::STEP).floor();
        if c < 0.0 || r < 0.0 || c as usize >= self.nx || r as usize >= self.ny {
            return false;
        }
        self.top[r as usize * self.nx + c as usize] >= z - 0.05
    }
}

/// Every vertical face of the kit, grouped into walls (same piece, facing
/// within 5°, in one plane within 25 cm), measured against the laser: from
/// each point on the wall, along its outward normal, where the laser stops
/// standing at least as high as that point.
///
/// **Triangles face outward** (the GLB's front faces, `glb.rs`), so a
/// normal's horizontal part points out of the building.
#[must_use]
pub fn walls(site: &Site) -> Vec<WallFit> {
    struct Group {
        piece: usize,
        normal: [f64; 2],
        offset: f64,
        points: Vec<[f64; 3]>,
    }
    let mut groups: Vec<Group> = Vec::new();
    for (k, piece) in site.pieces.iter().enumerate() {
        // Only the shell: an opening's panel (`solid: false`) stands behind
        // the wall it is cut in, where the laser never reaches, and the
        // Martinitoren's were told to move 3 m.
        if !piece.solid {
            continue;
        }
        for t in &piece.t {
            let [a, b, c] = t.map(|i| piece.p[i as usize]);
            let u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
            let v = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
            let n = [
                u[1] * v[2] - u[2] * v[1],
                u[2] * v[0] - u[0] * v[2],
                u[0] * v[1] - u[1] * v[0],
            ];
            let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
            if len < 1e-9 || (n[2] / len).abs() > 0.2 {
                continue;
            }
            let h = n[0].hypot(n[1]);
            let normal = [n[0] / h, n[1] / h];
            let offset = normal[0] * a[0] + normal[1] * a[1];
            let pts = sample_triangle(a, b, c);
            let found = groups.iter_mut().find(|g| {
                g.piece == k
                    && g.normal[0] * normal[0] + g.normal[1] * normal[1] > 5f64.to_radians().cos()
                    && (g.offset - offset).abs() < 0.25
            });
            match found {
                Some(g) => g.points.extend(pts),
                None => groups.push(Group {
                    piece: k,
                    normal,
                    offset,
                    points: pts,
                }),
            }
        }
    }
    let covers = Covers::new(&site.pieces);
    let mut out = Vec::new();
    for g in groups {
        let top = g
            .points
            .iter()
            .map(|p| p[2])
            .fold(f64::NEG_INFINITY, f64::max);
        let bottom = g.points.iter().map(|p| p[2]).fold(f64::INFINITY, f64::min);
        let mut moves: Vec<f64> = g
            .points
            .iter()
            // The laser blurs a wall's top edge and the ground clutters its
            // foot: judge the wall between them.
            .filter(|p| p[2] > 1.0 && p[2] < top - 0.5)
            // A face the kit itself stands in front of — the panel at the
            // back of an opening, a wall behind a balustrade — is not one the
            // laser can see: it would be measured against the face in front.
            .filter(|p| !covers.at([p[0] + g.normal[0] * 0.2, p[1] + g.normal[1] * 0.2], p[2]))
            .filter_map(|p| laser_edge(site, *p, g.normal))
            .collect();
        if moves.len() < MIN_WALL_POINTS {
            continue;
        }
        moves.sort_by(f64::total_cmp);
        let q = |f: f64| moves[((moves.len() - 1) as f64 * f).round() as usize];
        let along = [-g.normal[1], g.normal[0]];
        let ts: Vec<f64> = g
            .points
            .iter()
            .map(|p| p[0] * along[0] + p[1] * along[1])
            .collect();
        let (t0, t1) = (
            ts.iter().copied().fold(f64::INFINITY, f64::min),
            ts.iter().copied().fold(f64::NEG_INFINITY, f64::max),
        );
        if t1 - t0 < MIN_WALL_WIDTH_M {
            continue;
        }
        let n = g.points.len() as f64;
        let mid_en = [
            g.points.iter().map(|p| p[0]).sum::<f64>() / n,
            g.points.iter().map(|p| p[1]).sum::<f64>() / n,
        ];
        let normal_local = site.to_local(g.normal);
        out.push(WallFit {
            piece: site.pieces[g.piece].name.clone(),
            faces_deg: normal_local[0]
                .atan2(normal_local[1])
                .to_degrees()
                .rem_euclid(360.0),
            at: site.to_local(mid_en),
            from_m: bottom,
            to_m: top,
            width_m: t1 - t0,
            samples: moves.len(),
            move_m: q(0.5),
            spread_m: (q(0.75) - q(0.25)) / 2.0,
        });
    }
    out.sort_by(|a, b| b.move_m.abs().total_cmp(&a.move_m.abs()));
    out
}

/// Points over a triangle, [`WALL_STEP_M`] apart along both its edges from `a`.
fn sample_triangle(a: [f64; 3], b: [f64; 3], c: [f64; 3]) -> Vec<[f64; 3]> {
    let d = |p: [f64; 3], q: [f64; 3]| {
        ((p[0] - q[0]).powi(2) + (p[1] - q[1]).powi(2) + (p[2] - q[2]).powi(2)).sqrt()
    };
    let n = ((d(a, b).max(d(a, c)) / WALL_STEP_M).ceil() as usize).max(1);
    let mut out = Vec::new();
    for i in 0..=n {
        for j in 0..=(n - i) {
            let (u, v) = (
                (i as f64 + 0.33) / (n as f64 + 1.0),
                (j as f64 + 0.33) / (n as f64 + 1.0),
            );
            if u + v > 1.0 {
                continue;
            }
            out.push([
                a[0] + u * (b[0] - a[0]) + v * (c[0] - a[0]),
                a[1] + u * (b[1] - a[1]) + v * (c[1] - a[1]),
                a[2] + u * (b[2] - a[2]) + v * (c[2] - a[2]),
            ]);
        }
    }
    out
}

/// From a point on a wall (east/north/up), how far along the outward normal
/// the laser stops standing as high as the point: the laser's wall, as a
/// distance to move this one by. `None` when no edge is found in reach.
fn laser_edge(site: &Site, p: [f64; 3], normal: [f64; 2]) -> Option<f64> {
    let stands = |s: f64| {
        site.dsm
            .at([p[0] + normal[0] * s, p[1] + normal[1] * s])
            .map(|h| f64::from(h) - site.ground >= p[2] - 0.5)
    };
    let n = (WALL_SEARCH_M / 0.25) as i64;
    // Walk outward from the inside, and inward from the outside, and take the
    // crossing nearest the wall: a second building a few metres off must not
    // be read as this one's wall.
    let mut best: Option<f64> = None;
    for k in -n..n {
        let (s0, s1) = (k as f64 * 0.25, (k + 1) as f64 * 0.25);
        if let (Some(true), Some(false)) = (stands(s0), stands(s1)) {
            let s = (s0 + s1) / 2.0;
            if best.is_none_or(|b| s.abs() < b.abs()) {
                best = Some(s);
            }
        }
    }
    best
}

/// The walls as a section of `score.md`.
#[must_use]
pub fn walls_report(fits: &[WallFit]) -> String {
    let mut md = String::from(
        "## Walls against the laser\n\n\
         For every wall of the kit, how far to move it **outward** (negative: inward) to \
         stand where the laser's wall is: the median over points between a metre up and \
         half a metre under its top, each looking along the wall's normal for where the \
         laser stops standing as high. The laser's pixel is half a metre, so read moves \
         under 0.25 m as right; a large `spread` is a wall the laser sees as stepped or \
         leaning, not one plane.\n\n\
         | piece | faces | middle (local x, y) | height m | width m | points | move m | spread m |\n\
         |---|---|---|---|---|---|---|---|\n",
    );
    for f in fits {
        let _ = writeln!(
            md,
            "| {} | {:.0}° | {:.1}, {:.1} | {:.1}–{:.1} | {:.1} | {} | {:+.2} | {:.2} |",
            f.piece,
            f.faces_deg,
            f.at[0],
            f.at[1],
            f.from_m,
            f.to_m,
            f.width_m,
            f.samples,
            f.move_m,
            f.spread_m
        );
    }
    if let Some(d) = shift(fits) {
        let _ = writeln!(
            md,
            "\n**Taken together**, the walls say the whole kit stands ({:+.2}, {:+.2}) m off \
             in local x, y: the laser's walls are that far the other way. Moving every part \
             by ({:+.2}, {:+.2}) first leaves each wall's own error in the table; a shift \
             under 0.25 m is within the laser's pixel.",
            -d[0], -d[1], d[0], d[1]
        );
    }
    md
}

/// The one move of the whole kit that best explains every wall's move: the
/// least-squares `(dx, dy)` with `move ≈ normal · (dx, dy)`, each wall
/// weighted by its width. A kit drawn right but placed off shows as every
/// south face wanting out and every north face wanting in, by the same amount
/// (the Martinitoren, 2026-11-06); this says so in one line rather than
/// leaving a builder to resize every stage. `None` when the walls face too few
/// ways to tell.
#[must_use]
pub fn shift(fits: &[WallFit]) -> Option<[f64; 2]> {
    let (mut a, mut b, mut c, mut rx, mut ry) = (0.0, 0.0, 0.0, 0.0, 0.0);
    for f in fits {
        let (s, co) = f.faces_deg.to_radians().sin_cos();
        let (nx, ny, w) = (s, co, f.width_m);
        a += w * nx * nx;
        b += w * nx * ny;
        c += w * ny * ny;
        rx += w * nx * f.move_m;
        ry += w * ny * f.move_m;
    }
    let det = a * c - b * b;
    // Walls all facing one way (or two opposite ways) leave a direction
    // unmeasured: the normal equations are near singular.
    if det.abs() < 1e-3 * (a + c).powi(2).max(1e-9) {
        return None;
    }
    Some([(c * rx - b * ry) / det, (a * ry - b * rx) / det])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A site over a flat laser that saw a box `half` metres either side and
    /// `height` high, with `kit` (a box `kit_half` either side) in it.
    fn site(half: f64, height: f32, kit_half: f64) -> Site {
        let plane = Plane::at(53.2194, 6.5682);
        let rd = cartopolis_geo::ahn::wgs84_to_rd(53.2194, 6.5682);
        let n = 160;
        let step = 0.5;
        let min = [rd[0] - 40.0, rd[1] - 40.0];
        let mut h = vec![7.0f32; n * n];
        for row in 0..n {
            for col in 0..n {
                let e = min[0] + (col as f64 + 0.5) * step - rd[0];
                let nn = min[1] + step * n as f64 - (row as f64 + 0.5) * step - rd[1];
                if e.abs() < half && nn.abs() < half {
                    h[row * n + col] = 7.0 + height;
                }
            }
        }
        let kit = Kit::parse(&format!(
            r#"{{"name": "t", "pand": "0014100010924795", "origin": [53.2194, 6.5682],
                "materials": {{"b": {{"rgb": [1, 2, 3]}}}},
                "parts": [{{"name": "p", "material": "b",
                            "shape": {{"rect": {{"w": {w}, "d": {w}}}}},
                            "rings": [[0, 1], [10, 1]]}}]}}"#,
            w = kit_half * 2.0
        ))
        .unwrap();
        Site {
            name: "t".into(),
            pand: kit.pand.clone(),
            plane,
            rotation_deg: 0.0,
            outline: vec![[-half, -half], [half, -half], [half, half], [-half, half]],
            dsm: Surface::from_raster(plane, min, step, n, h),
            ground: 7.0,
            pieces: kit.mesh().unwrap(),
            kit: Some(kit),
        }
    }

    #[test]
    fn a_section_finds_the_walls_and_the_roof() {
        let s = site(5.0, 12.0, 5.0);
        let samples = section(&s, [0.0, 0.0], 90.0, 10.0);
        let st = steps(&samples, |x| x.laser);
        assert_eq!(st.len(), 2, "{st:?}");
        assert!(
            (st[0].0 + 5.0).abs() <= 0.5 && (st[1].0 - 5.0).abs() <= 0.5,
            "{st:?}"
        );
        let lv = levels(&samples);
        assert_eq!(lv.len(), 1);
        assert!(
            (lv[0].2 - 12.0).abs() < 0.01 && (lv[0].1 - lv[0].0) > 8.5,
            "{lv:?}"
        );
        let model = steps(&samples, |x| x.model);
        assert_eq!(model.len(), 2, "the kit's own box: {model:?}");
    }

    #[test]
    fn a_kit_too_narrow_is_told_to_move_every_wall_out() {
        let s = site(6.0, 12.0, 5.0);
        let fits = walls(&s);
        assert_eq!(fits.len(), 4, "four walls");
        for f in &fits {
            assert!(
                (f.move_m - 1.0).abs() <= 0.3,
                "{} {}",
                f.faces_deg,
                f.move_m
            );
        }
        let right = site(5.0, 12.0, 5.0);
        for f in walls(&right) {
            assert!(f.move_m.abs() <= 0.3, "{} {}", f.faces_deg, f.move_m);
        }
        // Too narrow on every side is not a kit standing off.
        let d = shift(&fits).unwrap();
        assert!(d[0].abs() < 0.2 && d[1].abs() < 0.2, "{d:?}");
    }

    #[test]
    fn walls_that_agree_are_read_as_one_shift() {
        let wall = |faces_deg: f64, move_m: f64| WallFit {
            piece: "p".into(),
            faces_deg,
            at: [0.0, 0.0],
            from_m: 0.0,
            to_m: 10.0,
            width_m: 10.0,
            samples: 100,
            move_m,
            spread_m: 0.1,
        };
        // East wants out 0.5, west in 0.5; north in 0.3, south out 0.3: the
        // kit belongs 0.5 m east and 0.3 m south of where it is.
        let fits = [
            wall(90.0, 0.5),
            wall(270.0, -0.5),
            wall(0.0, -0.3),
            wall(180.0, 0.3),
        ];
        let d = shift(&fits).unwrap();
        assert!(
            (d[0] - 0.5).abs() < 1e-9 && (d[1] + 0.3).abs() < 1e-9,
            "{d:?}"
        );
        assert!(shift(&[wall(90.0, 0.5), wall(270.0, -0.5)]).is_none());
    }
}
