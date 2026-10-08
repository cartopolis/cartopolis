//! The image half: pyramids, FAST corners, and pyramidal Lucas–Kanade tracking.
//!
//! Written here rather than taken from a crate because what is needed is small
//! and specific — greyscale `u8` at 640×480, a few hundred points a frame — and
//! the image crates that do it bring OpenCV or a whole computer-vision stack to
//! a phone build that compiles everything through the NDK's clang.

use glam::Vec2;

/// A greyscale image, row-major, one byte per pixel.
#[derive(Debug, Clone)]
pub struct Gray {
    pub w: usize,
    pub h: usize,
    pub px: Vec<u8>,
}

impl Gray {
    #[must_use]
    pub fn new(w: usize, h: usize, px: Vec<u8>) -> Self {
        assert_eq!(px.len(), w * h, "image size and pixel count disagree");
        Self { w, h, px }
    }

    #[inline]
    fn get(&self, x: usize, y: usize) -> f32 {
        f32::from(self.px[y * self.w + x])
    }

    /// Bilinear sample at a sub-pixel position, clamped to the edge.
    #[must_use]
    pub fn sample(&self, x: f32, y: f32) -> f32 {
        let x = x.clamp(0.0, (self.w - 1) as f32);
        let y = y.clamp(0.0, (self.h - 1) as f32);
        let (x0, y0) = (x.floor() as usize, y.floor() as usize);
        let (x1, y1) = ((x0 + 1).min(self.w - 1), (y0 + 1).min(self.h - 1));
        let (fx, fy) = (x - x0 as f32, y - y0 as f32);
        let top = self.get(x0, y0) * (1.0 - fx) + self.get(x1, y0) * fx;
        let bottom = self.get(x0, y1) * (1.0 - fx) + self.get(x1, y1) * fx;
        top * (1.0 - fy) + bottom * fy
    }

    /// Half the size, each pixel the mean of a 2×2 block.
    #[must_use]
    pub fn half(&self) -> Self {
        let (w, h) = (self.w / 2, self.h / 2);
        let mut px = Vec::with_capacity(w * h);
        for y in 0..h {
            for x in 0..w {
                let s = u16::from(self.px[2 * y * self.w + 2 * x])
                    + u16::from(self.px[2 * y * self.w + 2 * x + 1])
                    + u16::from(self.px[(2 * y + 1) * self.w + 2 * x])
                    + u16::from(self.px[(2 * y + 1) * self.w + 2 * x + 1]);
                px.push((s / 4) as u8);
            }
        }
        Self { w, h, px }
    }
}

/// An image and its halvings, finest first.
#[derive(Debug, Clone)]
pub struct Pyramid {
    pub levels: Vec<Gray>,
}

impl Pyramid {
    /// `levels` images, stopping early if a level would be smaller than 32 px.
    #[must_use]
    pub fn new(image: Gray, levels: usize) -> Self {
        let mut out = vec![image];
        while out.len() < levels {
            let last = out.last().expect("one level");
            if last.w < 64 || last.h < 64 {
                break;
            }
            out.push(last.half());
        }
        Self { levels: out }
    }
}

/// The Bresenham circle of radius 3 FAST tests around a candidate.
const CIRCLE: [(i32, i32); 16] = [
    (0, -3),
    (1, -3),
    (2, -2),
    (3, -1),
    (3, 0),
    (3, 1),
    (2, 2),
    (1, 3),
    (0, 3),
    (-1, 3),
    (-2, 2),
    (-3, 1),
    (-3, 0),
    (-3, -1),
    (-2, -2),
    (-1, -3),
];

/// A corner FAST found, with its strength (the threshold it would still pass).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Corner {
    pub at: Vec2,
    pub score: f32,
}

/// FAST-9 corners: a pixel whose circle holds nine contiguous pixels all
/// brighter, or all darker, than it by more than `threshold`. Non-maximum
/// suppressed in a 3×3 neighbourhood.
#[must_use]
pub fn fast_corners(img: &Gray, threshold: u8, border: usize) -> Vec<Corner> {
    let border = border.max(3);
    if img.w <= 2 * border || img.h <= 2 * border {
        return Vec::new();
    }
    let t = i32::from(threshold);
    let mut scores = vec![0u16; img.w * img.h];
    for y in border..img.h - border {
        for x in border..img.w - border {
            let c = i32::from(img.px[y * img.w + x]);
            let ring: [i32; 16] = CIRCLE.map(|(dx, dy)| {
                i32::from(img.px[(y as i32 + dy) as usize * img.w + (x as i32 + dx) as usize])
            });
            // Quick reject: nine contiguous pixels of sixteen always span at
            // least two of the four compass points.
            let compass = [ring[0], ring[4], ring[8], ring[12]];
            let bright = compass.iter().filter(|&&v| v > c + t).count();
            let dark = compass.iter().filter(|&&v| v < c - t).count();
            if bright < 2 && dark < 2 {
                continue;
            }
            let score = fast_score(c, &ring, t);
            if score > 0 {
                scores[y * img.w + x] = score as u16;
            }
        }
    }
    let mut out = Vec::new();
    for y in border..img.h - border {
        for x in border..img.w - border {
            let s = scores[y * img.w + x];
            if s == 0 {
                continue;
            }
            let mut best = true;
            'n: for dy in -1i32..=1 {
                for dx in -1i32..=1 {
                    if (dx, dy) == (0, 0) {
                        continue;
                    }
                    let o = scores[(y as i32 + dy) as usize * img.w + (x as i32 + dx) as usize];
                    if o > s || (o == s && (dy, dx) < (0, 0)) {
                        best = false;
                        break 'n;
                    }
                }
            }
            if best {
                out.push(Corner {
                    at: Vec2::new(x as f32, y as f32),
                    score: f32::from(s),
                });
            }
        }
    }
    out
}

/// The corner's strength: the sum of how far the qualifying arc exceeds the
/// threshold, or 0 when no nine-pixel arc qualifies.
fn fast_score(c: i32, ring: &[i32; 16], t: i32) -> i32 {
    let mut best = 0;
    for sign in [1, -1] {
        let mut run = 0;
        let mut sum = 0;
        let mut best_here = 0;
        // Twice round, so an arc that wraps is seen whole.
        for i in 0..32 {
            let d = sign * (ring[i % 16] - c) - t;
            if d > 0 {
                run += 1;
                sum += d;
                if run >= 9 {
                    best_here = best_here.max(sum);
                }
                if run >= 16 {
                    break;
                }
            } else {
                run = 0;
                sum = 0;
            }
        }
        best = best.max(best_here);
    }
    best
}

/// The strongest corners, at most one per `cell`×`cell` square, skipping
/// squares that already hold one of `taken` — new features go where the tracker
/// has none, so the points stay spread across the frame.
#[must_use]
pub fn spread(
    corners: &[Corner],
    w: usize,
    h: usize,
    cell: usize,
    taken: &[Vec2],
    max: usize,
) -> Vec<Vec2> {
    let (cols, rows) = (w.div_ceil(cell), h.div_ceil(cell));
    let mut best: Vec<Option<Corner>> = vec![None; cols * rows];
    let mut occupied = vec![false; cols * rows];
    let index = |p: Vec2| {
        let (cx, cy) = (
            (p.x as usize / cell).min(cols - 1),
            (p.y as usize / cell).min(rows - 1),
        );
        cy * cols + cx
    };
    for p in taken {
        if p.x >= 0.0 && p.y >= 0.0 {
            occupied[index(*p)] = true;
        }
    }
    for c in corners {
        let i = index(c.at);
        if occupied[i] {
            continue;
        }
        if best[i].is_none_or(|b| c.score > b.score) {
            best[i] = Some(*c);
        }
    }
    let mut chosen: Vec<Corner> = best.into_iter().flatten().collect();
    chosen.sort_by(|a, b| b.score.total_cmp(&a.score));
    chosen.truncate(max);
    chosen.into_iter().map(|c| c.at).collect()
}

/// Half-width of the Lucas–Kanade window: 9×9 pixels.
const HALF: i32 = 4;
/// Gauss–Newton steps per level.
const ITERATIONS: usize = 12;

/// Track `points` from `prev` to `next` by pyramidal Lucas–Kanade, coarse to
/// fine, then track each result back and keep it only if it returns within a
/// pixel of where it started — the check that throws out points that slid off
/// an edge or into a repeated texture. `None` for a point that was lost.
#[must_use]
pub fn track(prev: &Pyramid, next: &Pyramid, points: &[Vec2]) -> Vec<Option<Vec2>> {
    points
        .iter()
        .map(|&p| {
            let fwd = track_one(prev, next, p, p)?;
            let back = track_one(next, prev, fwd, p)?;
            (back.distance(p) < 1.0).then_some(fwd)
        })
        .collect()
}

fn track_one(from: &Pyramid, to: &Pyramid, p: Vec2, guess: Vec2) -> Option<Vec2> {
    let levels = from.levels.len().min(to.levels.len());
    let top = (levels - 1) as i32;
    let mut d = (guess - p) / 2f32.powi(top);
    for level in (0..levels).rev() {
        let scale = 2f32.powi(level as i32);
        let a = &from.levels[level];
        let b = &to.levels[level];
        let q = p / scale;
        // The template's gradients and the structure tensor, once per level.
        let mut grads = Vec::with_capacity(((2 * HALF + 1) * (2 * HALF + 1)) as usize);
        let (mut gxx, mut gxy, mut gyy) = (0.0f32, 0.0f32, 0.0f32);
        for wy in -HALF..=HALF {
            for wx in -HALF..=HALF {
                let (x, y) = (q.x + wx as f32, q.y + wy as f32);
                let ix = (a.sample(x + 1.0, y) - a.sample(x - 1.0, y)) * 0.5;
                let iy = (a.sample(x, y + 1.0) - a.sample(x, y - 1.0)) * 0.5;
                gxx += ix * ix;
                gxy += ix * iy;
                gyy += iy * iy;
                grads.push((a.sample(x, y), ix, iy));
            }
        }
        let det = gxx * gyy - gxy * gxy;
        if det < 1e-3 {
            return None;
        }
        for _ in 0..ITERATIONS {
            let (mut bx, mut by) = (0.0f32, 0.0f32);
            let mut k = 0;
            for wy in -HALF..=HALF {
                for wx in -HALF..=HALF {
                    let (t, ix, iy) = grads[k];
                    k += 1;
                    let diff = t - b.sample(q.x + d.x + wx as f32, q.y + d.y + wy as f32);
                    bx += diff * ix;
                    by += diff * iy;
                }
            }
            let step = Vec2::new((gyy * bx - gxy * by) / det, (gxx * by - gxy * bx) / det);
            d += step;
            if step.length_squared() < 0.0004 {
                break;
            }
        }
        if level > 0 {
            d *= 2.0;
        }
    }
    let out = p + d;
    let fine = &to.levels[0];
    let margin = HALF as f32 + 1.0;
    (out.x >= margin
        && out.y >= margin
        && out.x < fine.w as f32 - margin
        && out.y < fine.h as f32 - margin)
        .then_some(out)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A smooth random texture: blobs of varying brightness, so it has corners
    /// and gradients everywhere and no repeating pattern.
    pub(crate) fn texture(w: usize, h: usize, seed: u64, shift: Vec2) -> Gray {
        let mut s = seed;
        let mut rnd = || {
            s = s
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (s >> 33) as f32 / (1u64 << 31) as f32
        };
        let blobs: Vec<(f32, f32, f32, f32)> = (0..400)
            .map(|_| {
                (
                    rnd() * w as f32,
                    rnd() * h as f32,
                    3.0 + rnd() * 10.0,
                    rnd() * 2.0 - 1.0,
                )
            })
            .collect();
        let mut px = Vec::with_capacity(w * h);
        for y in 0..h {
            for x in 0..w {
                let (fx, fy) = (x as f32 - shift.x, y as f32 - shift.y);
                let mut v = 128.0;
                for &(bx, by, r, a) in &blobs {
                    let d2 = (fx - bx).powi(2) + (fy - by).powi(2);
                    if d2 < 9.0 * r * r {
                        v += a * 90.0 * (-d2 / (r * r)).exp();
                    }
                }
                px.push(v.clamp(0.0, 255.0) as u8);
            }
        }
        Gray::new(w, h, px)
    }

    #[test]
    fn fast_finds_the_corners_of_a_square() {
        let (w, h) = (64, 64);
        let mut px = vec![20u8; w * h];
        for y in 20..44 {
            for x in 20..44 {
                px[y * w + x] = 220;
            }
        }
        let corners = fast_corners(&Gray::new(w, h, px), 40, 3);
        for want in [(20.0, 20.0), (43.0, 20.0), (20.0, 43.0), (43.0, 43.0)] {
            assert!(
                corners
                    .iter()
                    .any(|c| c.at.distance(Vec2::new(want.0, want.1)) <= 1.5),
                "no corner near {want:?}: {corners:?}"
            );
        }
        // Not along the edges.
        assert!(corners.iter().all(|c| {
            let near = |v: f32| (v - 20.0).abs() <= 2.0 || (v - 43.0).abs() <= 2.0;
            near(c.at.x) && near(c.at.y)
        }));
    }

    #[test]
    fn spreading_keeps_one_per_cell_and_skips_taken_cells() {
        let corners = [
            Corner {
                at: Vec2::new(5.0, 5.0),
                score: 1.0,
            },
            Corner {
                at: Vec2::new(6.0, 6.0),
                score: 9.0,
            },
            Corner {
                at: Vec2::new(25.0, 5.0),
                score: 3.0,
            },
            Corner {
                at: Vec2::new(5.0, 25.0),
                score: 4.0,
            },
        ];
        let picked = spread(
            &corners,
            40,
            40,
            20,
            &[Vec2::new(30.0, 30.0), Vec2::new(4.0, 30.0)],
            10,
        );
        assert_eq!(picked, vec![Vec2::new(6.0, 6.0), Vec2::new(25.0, 5.0)]);
    }

    #[test]
    fn lucas_kanade_follows_a_subpixel_shift() {
        let shift = Vec2::new(3.3, -1.7);
        let a = texture(160, 120, 7, Vec2::ZERO);
        let b = texture(160, 120, 7, shift);
        let (pa, pb) = (Pyramid::new(a.clone(), 3), Pyramid::new(b, 3));
        let corners = fast_corners(&a, 15, 12);
        let pts = spread(&corners, 160, 120, 16, &[], 40);
        assert!(pts.len() >= 15, "only {} corners", pts.len());
        let tracked = track(&pa, &pb, &pts);
        let mut ok = 0;
        for (p, t) in pts.iter().zip(&tracked) {
            if let Some(t) = t {
                ok += 1;
                assert!((*t - *p - shift).length() < 0.15, "{p} -> {t}");
            }
        }
        assert!(ok * 10 >= pts.len() * 8, "kept {ok} of {}", pts.len());
    }

    #[test]
    fn lucas_kanade_follows_a_large_shift_through_the_pyramid() {
        let shift = Vec2::new(14.0, 9.0);
        let a = texture(320, 240, 3, Vec2::ZERO);
        let b = texture(320, 240, 3, shift);
        let (pa, pb) = (Pyramid::new(a.clone(), 4), Pyramid::new(b, 4));
        let pts = spread(&fast_corners(&a, 15, 30), 320, 240, 24, &[], 60);
        let tracked = track(&pa, &pb, &pts);
        let good: Vec<_> = pts
            .iter()
            .zip(&tracked)
            .filter_map(|(p, t)| t.map(|t| (t - *p - shift).length()))
            .collect();
        assert!(
            good.len() * 10 >= pts.len() * 7,
            "kept {} of {}",
            good.len(),
            pts.len()
        );
        assert!(good.iter().all(|e| *e < 0.3), "{good:?}");
    }
}
