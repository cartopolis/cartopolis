//! A landmark written as stacked parts (`docs/notes/landmark-kit.md`).
//!
//! A kit file is what an agent (or a person) writes after reading a building's
//! measurements and photographs: each part a plan shape lofted through rings of
//! `(height, scale)`, with openings — arches, windows, clock faces — cut into its
//! vertical faces. [`Kit::mesh`] turns it into one coloured mesh per part and
//! material, in metres east/north/up of the kit's origin, which
//! [`super::glb`] writes as the GLB the override store already takes.

use std::collections::BTreeMap;

use serde::Deserialize;

/// The whole file.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Kit {
    pub name: String,
    /// The BAG identificatie of the building this replaces.
    pub pand: String,
    /// `(lat, lng)` of the local frame's `(0, 0)`, on the ground.
    pub origin: (f64, f64),
    /// Compass bearing, degrees clockwise from north, of the local `+y` axis —
    /// so a tower standing square to its street is written square.
    #[serde(default)]
    pub rotation_deg: f64,
    /// Where each number came from: a measurement, or a photograph and its
    /// licence. Not read by the code; it is the file's credit and its argument.
    #[serde(default)]
    pub sources: Vec<String>,
    pub materials: BTreeMap<String, Material>,
    pub parts: Vec<Part>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Material {
    /// sRGB, 0–255.
    pub rgb: [u8; 3],
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Part {
    pub name: String,
    pub material: String,
    pub shape: Shape,
    /// Where copies of the part stand, local metres; one at `(0, 0)` when empty.
    #[serde(default)]
    pub at: Vec<[f64; 2]>,
    /// The part's own turn about its centre, degrees counter-clockwise.
    #[serde(default)]
    pub rotate_deg: f64,
    /// `(height above ground, scale of the plan shape)`, bottom to top. Two
    /// rings at one height are a step (a ledge or a set-back); a last scale of
    /// 0 closes the part to a point (a spire).
    pub rings: Vec<[f64; 2]>,
    #[serde(default)]
    pub openings: Vec<Opening>,
    /// Whether avatars bump into it. Its openings' panels never do.
    #[serde(default = "yes")]
    pub solid: bool,
}

fn yes() -> bool {
    true
}

/// A plan shape, centred on the part's position.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Shape {
    /// `w` along local x, `d` along local y. Faces 0–3: south, east, north, west.
    Rect { w: f64, d: f64 },
    /// A regular polygon of `sides` with circumradius `r`, a flat face toward
    /// local south. Face 0 is that one, counting counter-clockwise.
    Ngon { sides: usize, r: f64 },
    /// Any simple polygon, counter-clockwise; face `i` runs from point `i`.
    Polygon { points: Vec<[f64; 2]> },
}

/// Openings cut into a part's vertical faces, evenly spaced along each face.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Opening {
    /// Which faces; every face when empty.
    #[serde(default)]
    pub faces: Vec<usize>,
    pub kind: OpeningKind,
    /// How many across each face.
    #[serde(default = "one")]
    pub count: usize,
    /// Width of each, metres. A circle's diameter.
    pub w: f64,
    /// Bottom and top above ground (a pointed arch's apex is its top).
    pub from: f64,
    pub to: f64,
    /// How far it is recessed; negative stands proud of the wall (a clock face).
    pub depth: f64,
    /// The material of its back (or front, when proud).
    pub material: String,
}

fn one() -> usize {
    1
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OpeningKind {
    Rect,
    /// A Gothic arch: two arcs of radius `w` meeting at the apex.
    Arch,
    /// A round arch: a semicircle on the jambs.
    Round,
    Circle,
}

/// The triangles of one part in one material, metres east/north/up of the
/// kit's origin, outward faces counter-clockwise.
#[derive(Clone, Debug, Default)]
pub struct Piece {
    pub name: String,
    pub material: String,
    pub solid: bool,
    pub p: Vec<[f64; 3]>,
    pub t: Vec<[u32; 3]>,
}

impl Piece {
    fn add(&mut self, q: [f64; 3]) -> u32 {
        self.p.push(q);
        (self.p.len() - 1) as u32
    }

    /// A planar polygon, outward counter-clockwise.
    fn fan(&mut self, ring: &[[f64; 3]]) {
        let base = self.p.len() as u32;
        self.p.extend_from_slice(ring);
        for i in 1..ring.len().saturating_sub(1) as u32 {
            self.t.push([base, base + i, base + i + 1]);
        }
    }

    fn quad(&mut self, a: [f64; 3], b: [f64; 3], c: [f64; 3], d: [f64; 3]) {
        let [a, b, c, d] = [a, b, c, d].map(|q| self.add(q));
        self.t.push([a, b, c]);
        self.t.push([a, c, d]);
    }
}

/// Segments per quarter circle in arches and discs.
const ARC_STEPS: usize = 6;

impl Kit {
    pub fn parse(text: &str) -> Result<Self, String> {
        let kit: Kit = serde_json::from_str(text).map_err(|e| format!("kit: {e}"))?;
        kit.check()?;
        Ok(kit)
    }

    fn check(&self) -> Result<(), String> {
        if !crate::designs::valid_pand(&self.pand) {
            return Err(format!("kit: {:?} is not a BAG building number", self.pand));
        }
        for part in &self.parts {
            let named = |m: &str| {
                self.materials
                    .contains_key(m)
                    .then_some(())
                    .ok_or_else(|| format!("part {}: no material {m:?}", part.name))
            };
            named(&part.material)?;
            if part.rings.len() < 2 {
                return Err(format!("part {}: needs two rings or more", part.name));
            }
            if part.rings.windows(2).any(|w| w[1][0] < w[0][0]) {
                return Err(format!("part {}: rings must go up", part.name));
            }
            for o in &part.openings {
                named(&o.material)?;
                if o.to <= o.from || o.w <= 0.0 {
                    return Err(format!("part {}: an opening has no size", part.name));
                }
                if vertical_span(&part.rings, o.from, o.to).is_none() {
                    return Err(format!(
                        "part {}: an opening at {}–{} m is not on a vertical face",
                        part.name, o.from, o.to
                    ));
                }
            }
        }
        Ok(())
    }

    /// Local metres to east/north.
    #[must_use]
    pub fn to_en(&self, q: [f64; 2]) -> [f64; 2] {
        let (s, c) = self.rotation_deg.to_radians().sin_cos();
        [q[0] * c + q[1] * s, -q[0] * s + q[1] * c]
    }

    /// Every part as pieces, one per (part, copy, material).
    pub fn mesh(&self) -> Result<Vec<Piece>, String> {
        let mut out = Vec::new();
        for part in &self.parts {
            let at = if part.at.is_empty() {
                vec![[0.0, 0.0]]
            } else {
                part.at.clone()
            };
            for (k, pos) in at.iter().enumerate() {
                let (s, c) = part.rotate_deg.to_radians().sin_cos();
                let plan: Vec<[f64; 2]> = plan(&part.shape)?
                    .into_iter()
                    .map(|q| {
                        self.to_en([q[0] * c - q[1] * s + pos[0], q[0] * s + q[1] * c + pos[1]])
                    })
                    .collect();
                let centre = self.to_en(*pos);
                let name = if at.len() > 1 {
                    format!("{} {k}", part.name)
                } else {
                    part.name.clone()
                };
                out.extend(loft(part, &name, &plan, centre)?);
            }
        }
        Ok(out)
    }
}

/// The plan polygon, counter-clockwise, about its own centre.
fn plan(shape: &Shape) -> Result<Vec<[f64; 2]>, String> {
    let ring = match shape {
        Shape::Rect { w, d } => {
            let (x, y) = (w / 2.0, d / 2.0);
            vec![[-x, -y], [x, -y], [x, y], [-x, y]]
        }
        Shape::Ngon { sides, r } => {
            if *sides < 3 {
                return Err("an ngon needs three sides".into());
            }
            let n = *sides as f64;
            // Face 0 flat toward −y: its vertices straddle −90°.
            (0..*sides)
                .map(|i| {
                    let a = -std::f64::consts::FRAC_PI_2 - std::f64::consts::PI / n
                        + i as f64 * std::f64::consts::TAU / n;
                    [r * a.cos(), r * a.sin()]
                })
                .collect()
        }
        Shape::Polygon { points } => {
            if points.len() < 3 {
                return Err("a polygon needs three points".into());
            }
            let area: f64 = (0..points.len())
                .map(|i| {
                    let (a, b) = (points[i], points[(i + 1) % points.len()]);
                    a[0] * b[1] - b[0] * a[1]
                })
                .sum();
            if area < 0.0 {
                points.iter().rev().copied().collect()
            } else {
                points.clone()
            }
        }
    };
    Ok(ring)
}

/// The ring index `k` such that rings `k` and `k + 1` are one vertical stretch
/// (same scale) holding `from..to`.
fn vertical_span(rings: &[[f64; 2]], from: f64, to: f64) -> Option<usize> {
    rings.windows(2).position(|w| {
        (w[0][1] - w[1][1]).abs() < 1e-9 && w[0][0] <= from + 1e-6 && to <= w[1][0] + 1e-6
    })
}

fn scaled(plan: &[[f64; 2]], centre: [f64; 2], s: f64) -> Vec<[f64; 2]> {
    plan.iter()
        .map(|q| {
            [
                centre[0] + (q[0] - centre[0]) * s,
                centre[1] + (q[1] - centre[1]) * s,
            ]
        })
        .collect()
}

/// The outline of one opening in face coordinates `(u, v)`: along the face from
/// its first corner, and height. Counter-clockwise.
fn outline(kind: OpeningKind, u: f64, w: f64, from: f64, to: f64) -> Vec<[f64; 2]> {
    let (l, r) = (u - w / 2.0, u + w / 2.0);
    match kind {
        OpeningKind::Rect => vec![[l, from], [r, from], [r, to], [l, to]],
        OpeningKind::Round => {
            let spring = (to - w / 2.0).max(from);
            let mut o = vec![[l, from], [r, from]];
            for i in 0..=2 * ARC_STEPS {
                let a = std::f64::consts::PI * i as f64 / (2 * ARC_STEPS) as f64;
                o.push([u + w / 2.0 * a.cos(), spring + (to - spring) * a.sin()]);
            }
            o
        }
        OpeningKind::Arch => {
            // Equilateral pointed arch: each side an arc of radius w centred on
            // the opposite springing point, rising w·√3/2 to the apex.
            let rise = w * 3f64.sqrt() / 2.0;
            let spring = (to - rise).max(from);
            let k = (to - spring) / rise;
            let mut o = vec![[l, from], [r, from]];
            // Right side: centred on the left springing point, 0° → 60°.
            for i in 0..=ARC_STEPS {
                let a = std::f64::consts::FRAC_PI_3 * i as f64 / ARC_STEPS as f64;
                o.push([l + w * a.cos(), spring + w * a.sin() * k]);
            }
            // Left side: centred on the right springing point, 120° → 180°,
            // skipping the apex already pushed.
            for i in 1..=ARC_STEPS {
                let a = 2.0 * std::f64::consts::FRAC_PI_3
                    + std::f64::consts::FRAC_PI_3 * i as f64 / ARC_STEPS as f64;
                o.push([r + w * a.cos(), spring + w * a.sin() * k]);
            }
            o
        }
        OpeningKind::Circle => {
            let (cv, rad) = ((from + to) / 2.0, w / 2.0);
            (0..4 * ARC_STEPS)
                .map(|i| {
                    let a = std::f64::consts::TAU * i as f64 / (4 * ARC_STEPS) as f64;
                    [u + rad * a.cos(), cv + rad * a.sin()]
                })
                .collect()
        }
    }
}

/// One part at one position: the loft, its caps, and its openings.
fn loft(
    part: &Part,
    name: &str,
    plan: &[[f64; 2]],
    centre: [f64; 2],
) -> Result<Vec<Piece>, String> {
    let n = plan.len();
    let mut wall = Piece {
        name: if part.solid {
            format!("{name} wall")
        } else {
            name.to_string()
        },
        material: part.material.clone(),
        solid: part.solid,
        ..Piece::default()
    };
    // Openings' panels, one piece per material.
    let mut panels: BTreeMap<String, Piece> = BTreeMap::new();
    let rings = &part.rings;
    // Bottom cap, facing down (clockwise seen from above).
    if rings[0][1] > 0.0 {
        let b = scaled(plan, centre, rings[0][1]);
        let ring: Vec<[f64; 3]> = b.iter().rev().map(|q| [q[0], q[1], rings[0][0]]).collect();
        wall.fan(&ring);
    }
    for k in 0..rings.len() - 1 {
        let ([h0, s0], [h1, s1]) = (rings[k], rings[k + 1]);
        let lo = scaled(plan, centre, s0);
        let hi = scaled(plan, centre, s1);
        if (h1 - h0).abs() < 1e-9 {
            // A step: the ring between two outlines at one height. Its facing
            // follows which is bigger — a ledge faces up, an overhang down.
            let (outer, inner, up) = if s0 > s1 {
                (&lo, &hi, true)
            } else {
                (&hi, &lo, false)
            };
            for i in 0..n {
                let j = (i + 1) % n;
                let q = |p: [f64; 2]| [p[0], p[1], h0];
                if up {
                    wall.quad(q(outer[i]), q(outer[j]), q(inner[j]), q(inner[i]));
                } else {
                    wall.quad(q(outer[j]), q(outer[i]), q(inner[i]), q(inner[j]));
                }
            }
            continue;
        }
        let vertical = (s0 - s1).abs() < 1e-9;
        for i in 0..n {
            let j = (i + 1) % n;
            let here: Vec<&Opening> = part
                .openings
                .iter()
                .filter(|o| vertical && vertical_span(rings, o.from, o.to) == Some(k))
                .filter(|o| o.faces.is_empty() || o.faces.contains(&i))
                .collect();
            if here.is_empty() {
                if s1 == 0.0 {
                    let tip = [centre[0], centre[1], h1];
                    let a = wall.add([lo[i][0], lo[i][1], h0]);
                    let b = wall.add([lo[j][0], lo[j][1], h0]);
                    let c = wall.add(tip);
                    wall.t.push([a, b, c]);
                } else {
                    wall.quad(
                        [lo[i][0], lo[i][1], h0],
                        [lo[j][0], lo[j][1], h0],
                        [hi[j][0], hi[j][1], h1],
                        [hi[i][0], hi[i][1], h1],
                    );
                }
                continue;
            }
            face_with_openings(&mut wall, &mut panels, lo[i], lo[j], h0, h1, &here)?;
        }
    }
    // Top cap, facing up, unless the part closed to a point.
    let [ht, st] = rings[rings.len() - 1];
    if st > 0.0 {
        let t = scaled(plan, centre, st);
        let ring: Vec<[f64; 3]> = t.iter().map(|q| [q[0], q[1], ht]).collect();
        wall.fan(&ring);
    }
    let mut out = vec![wall];
    out.extend(panels.into_values());
    Ok(out)
}

/// A vertical face from `a` to `b` (east/north), `h0..h1` high, with openings
/// cut through it: the wall with holes, each hole's reveal, and its panel set
/// `depth` behind the face (or in front of it).
fn face_with_openings(
    wall: &mut Piece,
    panels: &mut BTreeMap<String, Piece>,
    a: [f64; 2],
    b: [f64; 2],
    h0: f64,
    h1: f64,
    openings: &[&Opening],
) -> Result<(), String> {
    let along = [b[0] - a[0], b[1] - a[1]];
    let len = (along[0] * along[0] + along[1] * along[1]).sqrt();
    if len < 1e-6 {
        return Ok(());
    }
    let dir = [along[0] / len, along[1] / len];
    // Outward, for a counter-clockwise plan: the right of the edge.
    let out = [dir[1], -dir[0]];
    let at = |q: [f64; 2], off: f64| {
        [
            a[0] + dir[0] * q[0] + out[0] * off,
            a[1] + dir[1] * q[0] + out[1] * off,
            q[1],
        ]
    };
    let mut holes: Vec<Vec<[f64; 2]>> = Vec::new();
    for o in openings {
        for c in 0..o.count {
            let u = len * (c as f64 + 0.5) / o.count as f64;
            let ring = outline(
                o.kind,
                u,
                o.w.min(len / o.count as f64 * 0.95),
                o.from,
                o.to,
            );
            let ring: Vec<[f64; 2]> = ring
                .into_iter()
                .map(|q| {
                    [
                        q[0].clamp(0.01, len - 0.01),
                        q[1].clamp(h0 + 0.01, h1 - 0.01),
                    ]
                })
                .collect();
            let panel = panels.entry(o.material.clone()).or_insert_with(|| Piece {
                name: format!("{} {}", wall.name.trim_end_matches(" wall"), o.material),
                material: o.material.clone(),
                solid: false,
                ..Piece::default()
            });
            let d = -o.depth;
            // The panel, facing out.
            fill(panel, &ring, &[], |q| at(q, d))?;
            // The reveal: the hole's sides from the face to the panel. One
            // winding serves both cases: it faces into the hole when recessed
            // and out of the plate when proud.
            for e in 0..ring.len() {
                let (p, q) = (ring[e], ring[(e + 1) % ring.len()]);
                panel.quad(at(p, 0.0), at(q, 0.0), at(q, d), at(p, d));
            }
            holes.push(ring);
        }
    }
    // The wall: the face rectangle with every hole cut out.
    fill(
        wall,
        &[[0.0, h0], [len, h0], [len, h1], [0.0, h1]],
        &holes,
        |q| at(q, 0.0),
    )
}

/// Triangulate a polygon with holes in face coordinates `(u, v)` and add it,
/// every triangle counter-clockwise in `(u, v)` — which is outward.
fn fill(
    piece: &mut Piece,
    outer: &[[f64; 2]],
    holes: &[Vec<[f64; 2]>],
    place: impl Fn([f64; 2]) -> [f64; 3],
) -> Result<(), String> {
    let mut verts: Vec<[f64; 2]> = outer.to_vec();
    let mut starts = Vec::new();
    for h in holes {
        starts.push(verts.len());
        verts.extend(h.iter().rev());
    }
    let flat: Vec<f64> = verts.iter().flat_map(|q| [q[0], q[1]]).collect();
    let idx = earcutr::earcut(&flat, &starts, 2).map_err(|e| format!("{e:?}"))?;
    let base = piece.p.len() as u32;
    piece.p.extend(verts.iter().map(|&q| place(q)));
    for t in idx.chunks_exact(3) {
        let (p0, p1, p2) = (verts[t[0]], verts[t[1]], verts[t[2]]);
        let cross = (p1[0] - p0[0]) * (p2[1] - p0[1]) - (p1[1] - p0[1]) * (p2[0] - p0[0]);
        let [a, b, c] = [t[0], t[1], t[2]].map(|i| base + i as u32);
        piece
            .t
            .push(if cross >= 0.0 { [a, b, c] } else { [a, c, b] });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kit(parts: &str) -> Kit {
        Kit::parse(&format!(
            r#"{{"name": "t", "pand": "0014100010924795", "origin": [53.2, 6.5],
                "materials": {{"brick": {{"rgb": [150, 80, 60]}}, "dark": {{"rgb": [20, 20, 20]}}}},
                "parts": [{parts}]}}"#
        ))
        .unwrap()
    }

    /// Outward normals of a closed box point away from its centre.
    #[test]
    fn a_box_is_closed_and_faces_out() {
        let k = kit(
            r#"{"name": "b", "material": "brick", "shape": {"rect": {"w": 4, "d": 2}},
                        "rings": [[0, 1], [3, 1]]}"#,
        );
        let pieces = k.mesh().unwrap();
        assert_eq!(pieces.len(), 1);
        let p = &pieces[0];
        assert_eq!(p.t.len(), 12);
        for t in &p.t {
            let [a, b, c] = t.map(|i| p.p[i as usize]);
            let n = cross(sub(b, a), sub(c, a));
            let mid = [
                (a[0] + b[0] + c[0]) / 3.0,
                (a[1] + b[1] + c[1]) / 3.0,
                (a[2] + b[2] + c[2]) / 3.0,
            ];
            let from_centre = sub(mid, [0.0, 0.0, 1.5]);
            assert!(dot(n, from_centre) > 0.0, "{t:?} faces in");
        }
    }

    /// An arch cuts the wall: the face loses the arch's area, and the panel
    /// behind it has the same area again.
    #[test]
    fn an_arch_is_cut_and_backed() {
        let k = kit(
            r#"{"name": "s", "material": "brick", "shape": {"rect": {"w": 6, "d": 6}},
                        "rings": [[0, 1], [10, 1]],
                        "openings": [{"faces": [0], "kind": "arch", "w": 2, "from": 2, "to": 8,
                                      "depth": 0.5, "material": "dark"}]}"#,
        );
        let pieces = k.mesh().unwrap();
        let wall = pieces.iter().find(|p| p.material == "brick").unwrap();
        let panel = pieces.iter().find(|p| p.material == "dark").unwrap();
        let south: f64 = area_facing(wall, [0.0, -1.0, 0.0]);
        let back: f64 = area_facing(panel, [0.0, -1.0, 0.0]);
        assert!((south + back - 60.0).abs() < 1e-6, "{south} + {back}");
        assert!(back > 10.0 && back < 12.0, "{back}");
        assert!(wall.solid && !panel.solid);
    }

    /// A last scale of zero is a spire: no top cap, all sides meet at the tip.
    #[test]
    fn a_spire_closes_to_its_tip() {
        let k = kit(
            r#"{"name": "sp", "material": "brick", "shape": {"ngon": {"sides": 8, "r": 3}},
                        "rings": [[0, 1], [20, 0]]}"#,
        );
        let p = &k.mesh().unwrap()[0];
        let top = p.p.iter().map(|q| q[2]).fold(0.0, f64::max);
        assert_eq!(top, 20.0);
        assert_eq!(p.t.len(), 8 + 6);
    }

    #[test]
    fn an_opening_off_a_vertical_face_is_refused() {
        let text = r#"{"name": "t", "pand": "0014100010924795", "origin": [53.2, 6.5],
            "materials": {"b": {"rgb": [1, 2, 3]}},
            "parts": [{"name": "p", "material": "b", "shape": {"rect": {"w": 4, "d": 4}},
                       "rings": [[0, 1], [10, 0.5]],
                       "openings": [{"kind": "rect", "w": 1, "from": 2, "to": 4, "depth": 0.2,
                                     "material": "b"}]}]}"#;
        assert!(Kit::parse(text)
            .unwrap_err()
            .contains("not on a vertical face"));
    }

    fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
        [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
    }
    fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
        [
            a[1] * b[2] - a[2] * b[1],
            a[2] * b[0] - a[0] * b[2],
            a[0] * b[1] - a[1] * b[0],
        ]
    }
    fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
        a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
    }
    fn area_facing(p: &Piece, dir: [f64; 3]) -> f64 {
        p.t.iter()
            .map(|t| {
                let [a, b, c] = t.map(|i| p.p[i as usize]);
                let n = cross(sub(b, a), sub(c, a));
                let len = dot(n, n).sqrt();
                if len > 0.0 && dot(n, dir) / len > 0.99 {
                    len / 2.0
                } else {
                    0.0
                }
            })
            .sum()
    }
}
