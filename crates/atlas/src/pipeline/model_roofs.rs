//! Roof coverings from the city model, for `facades.rs`, which reads its tiles.
//!
//! The roofs layer (`roofs.rs`) measures a roof's colour from the vertical
//! aerial photograph and calls every pitched roof "tile": at 8–25 cm a pixel
//! straight down, slate, glazed tiles and a field of solar panels are all a
//! dark roof. The city model's oblique photographs see each roof face at
//! ~10 cm and from the side its slope faces, which shows the solar panels as
//! panels and the red of a pantile as red. Per face of each building's roof
//! (all its planes facing one compass sector, or the flat ones) this keeps a
//! sample of texels and decides what covers it ([`covering`]): from the
//! photograph what can be seen — green, panels, the colour — and from the
//! building's year and monument status what cannot. The courses themselves,
//! 30 cm apart, are three texels at this level and were not measurable on the
//! 2026-11-19 sample, so no decision rests on them.

use cartopolis_geo::formats::facades::{Covering, RoofFace, FLAT};

/// A face whose normal is within this of vertical (`n·up`) is flat.
pub(super) const FLAT_UP: f64 = 0.95;
/// A triangle whose normal is at least this far up is a roof.
pub(super) const ROOF_UP: f64 = 0.15;

/// Texels a face keeps at most, as a stride sample.
const SAMPLE_CAP: usize = 4096;
/// Texels a face needs (~2 m² at z18).
const MIN_TEXELS: usize = 200;

/// The roof face of `normal` (east, north, up; unit): its compass sector, or
/// [`FLAT`].
pub(super) fn face_of(normal: [f64; 3]) -> u8 {
    if normal[2] >= FLAT_UP {
        FLAT
    } else {
        cartopolis_geo::formats::facades::sector(normal[0], normal[1])
    }
}

/// One face's texels, gathered over its planes and the tiles they are in.
#[derive(Clone, Debug)]
pub(super) struct FaceTexels {
    pub px: Vec<[u8; 3]>,
    pub n: usize,
    /// Of `n`, the texels on planes that show a field of solar panels
    /// ([`panel_grid`]).
    pub solar: usize,
    /// Every `stride`-th texel is kept; it doubles, and the sample is halved,
    /// whenever the sample reaches twice [`SAMPLE_CAP`] — so a big face is
    /// sampled evenly over all of it, not only its first planes.
    stride: usize,
}

impl Default for FaceTexels {
    fn default() -> Self {
        Self {
            px: Vec::new(),
            n: 0,
            solar: 0,
            stride: 1,
        }
    }
}

impl FaceTexels {
    pub fn add(&mut self, p: [u8; 3]) {
        self.n += 1;
        if self.n % self.stride == 0 {
            self.px.push(p);
            if self.px.len() >= SAMPLE_CAP * 2 {
                self.px = self.px.iter().step_by(2).copied().collect();
                self.stride *= 2;
            }
        }
    }

    pub fn merge(&mut self, other: FaceTexels) {
        self.n += other.n;
        self.solar += other.solar;
        self.px.extend(other.px);
        while self.px.len() >= SAMPLE_CAP * 2 {
            self.px = self.px.iter().step_by(2).copied().collect();
            self.stride *= 2;
        }
    }
}

/// A roof triangle in its leaf's east-north-up metres, with its texture
/// coordinates and texture.
pub(super) struct RoofTri {
    pub p: [[f64; 3]; 3],
    pub uv: [[f64; 2]; 3],
    pub tex: usize,
}

/// Metres a rectified roof pixel spans: the texture's own.
const RES: f64 = 0.1;
/// A roof plane longer than this many pixels either way is sampled coarser.
const MAX_PX: f64 = 1024.0;

fn edge(a: [f64; 2], b: [f64; 2], c: [f64; 2]) -> f64 {
    (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0])
}

/// One roof plane's texels, and whether it is a field of panels: the plane is
/// sampled square-on, across the slope and up it ([`panel_grid`] needs its
/// lattice in metres).
pub(super) fn read_plane(
    tris: &[RoofTri],
    normal: [f64; 3],
    textures: &[Option<image::RgbImage>],
) -> FaceTexels {
    let (a, b) = if normal[2] >= FLAT_UP {
        ([1.0, 0.0, 0.0], [0.0, 1.0, 0.0])
    } else {
        let l = normal[0].hypot(normal[1]);
        let a = [-normal[1] / l, normal[0] / l, 0.0];
        // In the plane, up the slope.
        let b = [
            normal[1] * a[2] - normal[2] * a[1],
            normal[2] * a[0] - normal[0] * a[2],
            normal[0] * a[1] - normal[1] * a[0],
        ];
        (a, b)
    };
    let dot = |p: [f64; 3], q: [f64; 3]| p[0] * q[0] + p[1] * q[1] + p[2] * q[2];
    let (mut s0, mut s1, mut t0, mut t1) = (f64::MAX, f64::MIN, f64::MAX, f64::MIN);
    for tri in tris {
        for p in tri.p {
            (s0, s1) = (s0.min(dot(p, a)), s1.max(dot(p, a)));
            (t0, t1) = (t0.min(dot(p, b)), t1.max(dot(p, b)));
        }
    }
    let mut out = FaceTexels::default();
    if !(s1 > s0 && t1 > t0) {
        return out;
    }
    let res = RES.max((s1 - s0).max(t1 - t0) / MAX_PX);
    let (w, h) = (
        ((s1 - s0) / res).ceil() as usize,
        ((t1 - t0) / res).ceil() as usize,
    );
    let mut img: Vec<Option<[u8; 3]>> = vec![None; w * h];
    for tri in tris {
        let Some(tex) = textures.get(tri.tex).and_then(Option::as_ref) else {
            continue;
        };
        let (tw, th) = tex.dimensions();
        let q = tri
            .p
            .map(|p| [(dot(p, a) - s0) / res, (t1 - dot(p, b)) / res]);
        let area = edge(q[0], q[1], q[2]);
        if area.abs() < 1e-9 {
            continue;
        }
        let x0 = q
            .iter()
            .map(|p| p[0])
            .fold(f64::MAX, f64::min)
            .floor()
            .max(0.0) as usize;
        let x1 = (q
            .iter()
            .map(|p| p[0])
            .fold(f64::MIN, f64::max)
            .ceil()
            .max(0.0) as usize)
            .min(w);
        let y0 = q
            .iter()
            .map(|p| p[1])
            .fold(f64::MAX, f64::min)
            .floor()
            .max(0.0) as usize;
        let y1 = (q
            .iter()
            .map(|p| p[1])
            .fold(f64::MIN, f64::max)
            .ceil()
            .max(0.0) as usize)
            .min(h);
        for y in y0..y1 {
            for x in x0..x1 {
                let c = [x as f64 + 0.5, y as f64 + 0.5];
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
                img[y * w + x] = Some(tex.get_pixel(tx, ty).0);
            }
        }
    }
    for p in img.iter().flatten() {
        out.add(*p);
    }
    if res <= RES * 1.01 && panel_grid(&img, w, h) {
        out.solar = out.n;
    }
    out
}

/// The panels of a solar field: a panel is ~1.0 × 1.6–1.7 m, so in a plane
/// sampled at 10 cm its frames repeat every 9–18 pixels along both axes.
const PANEL_LAGS: std::ops::RangeInclusive<usize> = 9..=18;
/// How strongly a lattice must repeat (autocorrelation) to be one: noise over a
/// roof-sized plane reaches 0.3 at its best of ten lags; a field of panels 0.8.
const PANEL_R: f64 = 0.5;
/// The share of a field's texels that are the cells' dark blue-black.
const PANEL_DARK: f64 = 0.4;

/// How strongly `profile` repeats at its best lag in `lags`, detrended over a
/// window of 4 m so a roof's shading gradient is not mistaken for a lattice.
fn repeat(profile: &[Option<f64>], lags: std::ops::RangeInclusive<usize>) -> f64 {
    let n = profile.len();
    let det: Vec<Option<f64>> = (0..n)
        .map(|i| {
            let v = profile[i]?;
            let (lo, hi) = (i.saturating_sub(20), (i + 20).min(n));
            let win: Vec<f64> = profile[lo..hi].iter().flatten().copied().collect();
            Some(v - win.iter().sum::<f64>() / win.len() as f64)
        })
        .collect();
    let mut best = 0.0f64;
    for lag in lags {
        if lag * 3 > n {
            break;
        }
        let (mut xy, mut xx, mut yy, mut c) = (0.0, 0.0, 0.0, 0usize);
        for i in 0..n - lag {
            if let (Some(a), Some(b)) = (det[i], det[i + lag]) {
                (xy, xx, yy, c) = (xy + a * b, xx + a * a, yy + b * b, c + 1);
            }
        }
        if c >= lag * 2 && xx > 0.0 && yy > 0.0 {
            best = best.max(xy / (xx * yy).sqrt());
        }
    }
    best
}

/// Whether a rectified plane is a field of solar panels: mostly the cells'
/// dark blue-black, and a lattice of the panels' frames repeating along both
/// axes. On the 2026-11-19 sample a colour test alone took every shaded slope
/// and every dark slate roof for panels — the sky lights them blue — and none
/// of them repeats at a panel's size.
pub(super) fn panel_grid(img: &[Option<[u8; 3]>], w: usize, h: usize) -> bool {
    let covered = img.iter().flatten().count();
    if covered < 400 {
        return false;
    }
    let dark = img.iter().flatten().filter(|&&p| is_cell(p)).count();
    if (dark as f64) < covered as f64 * PANEL_DARK {
        return false;
    }
    let mean = |it: &mut dyn Iterator<Item = [u8; 3]>| -> Option<f64> {
        let (s, n) = it.fold((0.0, 0usize), |(s, n), p| (s + luma(p), n + 1));
        (n * 3 >= w.min(h)).then(|| s / n as f64)
    };
    let rows: Vec<Option<f64>> = (0..h)
        .map(|y| mean(&mut (0..w).filter_map(|x| img[y * w + x])))
        .collect();
    let cols: Vec<Option<f64>> = (0..w)
        .map(|x| mean(&mut (0..h).filter_map(|y| img[y * w + x])))
        .collect();
    repeat(&rows, PANEL_LAGS) >= PANEL_R && repeat(&cols, PANEL_LAGS) >= PANEL_R
}

/// What the registers say about a building that bears on its roof.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct RoofFacts {
    pub year: Option<u16>,
    /// A rijksmonument: its roof is what it was, slate where it was slate.
    pub monument: bool,
}

fn luma(p: [u8; 3]) -> f64 {
    0.2126 * f64::from(p[0]) + 0.7152 * f64::from(p[1]) + 0.0722 * f64::from(p[2])
}

fn is_green(p: [u8; 3]) -> bool {
    let [r, g, b] = p.map(i32::from);
    g > r + 6 && g > b + 6
}

/// The dark blue-black of a photovoltaic cell — and of a shaded slate roof,
/// which is why it decides nothing without [`panel_grid`].
fn is_cell(p: [u8; 3]) -> bool {
    let [r, g, b] = p.map(i32::from);
    luma(p) < 90.0 && b >= r + 4 && b >= g
}

/// Share of a face's texels that must be on panel planes for it to be solar.
const SOLAR_SHARE: f64 = 0.5;

fn median(mut v: Vec<[u8; 3]>) -> [u8; 3] {
    [0, 1, 2].map(|k| {
        v.sort_by_key(|p| p[k]);
        v[v.len() / 2][k]
    })
}

/// What covers one face, and its colour; `None` with too few texels. A
/// pitched face's covering is only its own guess: [`roof_of`] decides it for
/// the building from its best-lit face.
pub(super) fn covering(face: u8, t: &FaceTexels, facts: RoofFacts) -> Option<RoofFace> {
    let px: Vec<[u8; 3]> =
        t.px.iter()
            .copied()
            .filter(|p| p.iter().map(|&v| u32::from(v)).sum::<u32>() >= 24)
            .collect();
    if px.len() < MIN_TEXELS {
        return None;
    }
    let n = t.n.min(u32::MAX as usize) as u32;
    let face_of = |c, k| Some(RoofFace { c, d: face, k, n });
    if t.solar as f64 >= t.n as f64 * SOLAR_SHARE {
        return face_of(
            median(px.into_iter().filter(|&p| is_cell(p)).collect()),
            Covering::Solar,
        );
    }
    let green = px.iter().filter(|&&p| is_green(p)).count();
    if green * 2 > px.len() {
        return face_of(
            median(px.into_iter().filter(|&p| is_green(p)).collect()),
            Covering::Green,
        );
    }
    // The covering's own colour: the darkest quarter (shadows, gutters) and
    // the brightest twentieth (glints, skylights) left out, as the roofs layer
    // does, and no moss in it.
    let mut rest: Vec<[u8; 3]> = px.into_iter().filter(|&p| !is_green(p)).collect();
    if rest.len() < MIN_TEXELS / 2 {
        return None;
    }
    rest.sort_by(|a, b| luma(*a).total_cmp(&luma(*b)));
    let (lo, hi) = (rest.len() / 4, rest.len() - rest.len() / 20);
    let c = median(rest[lo..hi.max(lo + 1)].to_vec());
    let l = luma(c);
    let k = if face == FLAT {
        if l < 75.0 {
            Covering::Bitumen
        } else if l < 150.0 {
            Covering::Gravel
        } else {
            Covering::Light
        }
    } else {
        let [r, g, b] = c.map(f64::from);
        let hi = r.max(g).max(b).max(1.0);
        let sat = (hi - r.min(g).min(b)) / hi;
        // Before 1930, and on a monument whatever its year, a dark roof is
        // slate; after, it is a glazed or dark-fired tile. Concrete tiles came
        // in after the war. Zinc and copper are not told apart from a pale
        // slate in the sun at this resolution, so they are not guessed.
        let old = facts.monument || facts.year.is_some_and(|y| y < 1930);
        if r > b * 1.15 && sat > 0.2 {
            Covering::Pantile
        } else if l < 90.0 {
            if old {
                Covering::Slate
            } else {
                Covering::Glazed
            }
        } else if facts.year.is_some_and(|y| y >= 1955) && !old {
            Covering::Concrete
        } else {
            Covering::Slate
        }
    };
    face_of(c, k)
}

/// A building's roof, face by face. Its pitched faces share one covering —
/// a roof is laid in one material — decided on the best-lit of them: the
/// slope away from the sun is the same tiles in shade, and read on its own it
/// came out as dark slate under every red roof. Each pitched face keeps its
/// own colour where it is the best-lit one's covering; a face of panels stays
/// panels.
pub(super) fn roof_of(faces: Vec<RoofFace>) -> Vec<RoofFace> {
    let luma_of = |f: &RoofFace| luma(f.c);
    let best = faces
        .iter()
        .filter(|f| f.d != FLAT && !matches!(f.k, Covering::Solar | Covering::Green))
        .max_by(|a, b| luma_of(a).total_cmp(&luma_of(b)))
        .cloned();
    faces
        .into_iter()
        .map(|f| match &best {
            Some(b) if f.d != FLAT && !matches!(f.k, Covering::Solar | Covering::Green) => {
                RoofFace {
                    c: b.c,
                    k: b.k,
                    ..f
                }
            }
            _ => f,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn face(p: [u8; 3]) -> FaceTexels {
        let mut t = FaceTexels::default();
        for _ in 0..800 {
            t.add(p);
        }
        t
    }

    #[test]
    fn faces_are_sectors_or_flat() {
        assert_eq!(face_of([0.0, 0.0, 1.0]), FLAT);
        assert_eq!(face_of([0.0, -0.6, 0.8]), 4);
        assert_eq!(face_of([0.6, 0.0, 0.8]), 2);
    }

    #[test]
    fn red_is_pantile_and_panels_are_solar() {
        let f = RoofFacts::default();
        assert_eq!(
            covering(4, &face([150, 75, 55]), f).unwrap().k,
            Covering::Pantile
        );
        // Panels are what `panel_grid` found, not a colour: a dark blue face
        // with no lattice is a dark roof.
        let mut solar = face([28, 34, 58]);
        assert_ne!(covering(4, &solar, f).unwrap().k, Covering::Solar);
        solar.solar = solar.n;
        assert_eq!(covering(4, &solar, f).unwrap().k, Covering::Solar);
        assert_eq!(
            covering(FLAT, &face([60, 120, 50]), f).unwrap().k,
            Covering::Green
        );
    }

    #[test]
    fn a_dark_roof_is_slate_on_an_old_building_and_glazed_on_a_new_one() {
        let dark = face([62, 64, 68]);
        let old = RoofFacts {
            year: Some(1895),
            monument: false,
        };
        let new = RoofFacts {
            year: Some(1998),
            monument: false,
        };
        let listed = RoofFacts {
            year: Some(1998),
            monument: true,
        };
        assert_eq!(covering(4, &dark, old).unwrap().k, Covering::Slate);
        assert_eq!(covering(4, &dark, new).unwrap().k, Covering::Glazed);
        assert_eq!(covering(4, &dark, listed).unwrap().k, Covering::Slate);
        let grey = face([115, 117, 118]);
        assert_eq!(covering(4, &grey, new).unwrap().k, Covering::Concrete);
        // A pale grey roof in the sun is not guessed to be zinc.
        assert_eq!(
            covering(4, &face([175, 178, 180]), new).unwrap().k,
            Covering::Concrete
        );
    }

    #[test]
    fn a_roof_is_one_covering_read_off_its_best_lit_face() {
        let face = |d, c, k| RoofFace { c, d, k, n: 900 };
        let roof = roof_of(vec![
            face(4, [150, 75, 55], Covering::Pantile),
            face(0, [60, 62, 70], Covering::Slate),
            face(2, [30, 35, 60], Covering::Solar),
            face(FLAT, [60, 60, 60], Covering::Bitumen),
        ]);
        assert_eq!(roof[0].k, Covering::Pantile);
        assert_eq!((roof[1].k, roof[1].c), (Covering::Pantile, [150, 75, 55]));
        assert_eq!(roof[2].k, Covering::Solar);
        assert_eq!(roof[3].k, Covering::Bitumen);
    }

    /// A field of panels 1.0 × 1.6 m at 10 cm: dark blue cells inside light
    /// frames. A shaded slate roof of the same blue has no lattice.
    #[test]
    fn panels_repeat_and_shaded_slate_does_not() {
        let (w, h) = (80usize, 96usize);
        let field: Vec<Option<[u8; 3]>> = (0..w * h)
            .map(|i| {
                let (x, y) = (i % w, i / w);
                let frame = x % 10 == 0 || y % 16 == 0;
                Some(if frame { [170, 172, 175] } else { [28, 34, 60] })
            })
            .collect();
        assert!(panel_grid(&field, w, h));
        let slate: Vec<Option<[u8; 3]>> = (0..w * h)
            .map(|i| {
                Some([
                    50 + ((i as u64).wrapping_mul(2_654_435_761) >> 16) as u8 % 13,
                    55,
                    66,
                ])
            })
            .collect();
        assert!(!panel_grid(&slate, w, h));
    }

    #[test]
    fn flat_roofs_go_by_tone() {
        let f = RoofFacts::default();
        assert_eq!(
            covering(FLAT, &face([50, 50, 52]), f).unwrap().k,
            Covering::Bitumen
        );
        assert_eq!(
            covering(FLAT, &face([120, 118, 112]), f).unwrap().k,
            Covering::Gravel
        );
        assert_eq!(
            covering(FLAT, &face([200, 200, 198]), f).unwrap().k,
            Covering::Light
        );
    }

    #[test]
    fn too_few_texels_say_nothing() {
        let mut t = FaceTexels::default();
        for _ in 0..50 {
            t.add([150, 75, 55]);
        }
        assert!(covering(4, &t, RoofFacts::default()).is_none());
    }

    #[test]
    fn a_large_face_is_sampled_not_kept_whole() {
        let mut t = FaceTexels::default();
        for i in 0..100_000u32 {
            t.add([(i % 200) as u8, 0, 0]);
        }
        assert_eq!(t.n, 100_000);
        assert!(t.px.len() <= SAMPLE_CAP * 2, "{}", t.px.len());
        assert!(t.px.len() >= SAMPLE_CAP / 4, "{}", t.px.len());
    }
}
