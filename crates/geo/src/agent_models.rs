//! The moving models: four car bodies, a walker with limbs, and a crow.
//!
//! The same bodies serve two very different costs. The ambient pool
//! (`cartopolis`'s `systems::traffic`) is a fixed set of entities sharing **one
//! mesh per model**, uploaded once at startup, so a model costs its vertices
//! once and can afford hubs, mirrors and a grille. A parked car
//! (`furniture::push_parked_car`) is merged into its cell's mesh **once per
//! bay**, so it is built at [`Detail::Parked`]: the same silhouette, about 2×
//! the seven boxes it replaced rather than the moving model's 3.6×. Both being
//! one body is what keeps a car at the kerb and a car driving past it the same
//! car.
//!
//! Each model comes in parts because the parts are drawn with different
//! materials, and a shared mesh cannot vary anything per instance but its
//! material:
//!
//! * **body** — the painted panels, white vertex colour under the car's paint;
//! * **trim** — glass, tyres, bumpers, plates and headlamps, coloured per vertex
//!   under one neutral material, so a red car's windscreen is not red;
//! * **tail** — the rear lamps alone, so a braking car can swap its material.
//!
//! A walker is a torso (with the head), and a leg and an arm mesh each hung from
//! its pivot, so the client can swing them. The head stays the neutral grey of
//! [`crate::furniture::form::PERSON`], for the reason given there. A crow
//! (`systems::crows`) is a body and a head with two jointed wings ([`crow`]).
//!
//! All models: origin on the ground, +y up, **+z the front**.

/// One mesh's worth of flat-shaded triangles, in linear RGBA.
#[derive(Debug, Clone, Default)]
pub struct ModelMesh {
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub colors: Vec<[f32; 4]>,
    pub indices: Vec<u32>,
}

impl ModelMesh {
    /// A planar-ish quad `a b c d` (in order round its edge), facing `out`.
    ///
    /// Four vertices, not six: the two triangles share their diagonal. A parked
    /// car is multiplied by every bay in the lot, so the third saved here is a
    /// third of the lot's mesh.
    fn quad(&mut self, q: [[f32; 3]; 4], out: [f32; 3], rgba: [f32; 4]) {
        let mut n = cross(sub(q[1], q[0]), sub(q[2], q[0]));
        if dot(n, n) < 1e-12 {
            n = cross(sub(q[2], q[0]), sub(q[3], q[0]));
        }
        let len = dot(n, n).sqrt();
        if len < 1e-6 {
            return;
        }
        let mut n = [n[0] / len, n[1] / len, n[2] / len];
        let q = if dot(n, out) < 0.0 {
            n = [-n[0], -n[1], -n[2]];
            [q[0], q[3], q[2], q[1]]
        } else {
            q
        };
        let base = self.positions.len() as u32;
        for p in q {
            self.positions.push(p);
            self.normals.push(n);
            self.colors.push(rgba);
        }
        self.indices
            .extend([base, base + 1, base + 2, base, base + 2, base + 3]);
    }

    /// A flat rectangle on a face that looks along `±z`: a plate or a lamp
    /// lens, which is only ever seen from the side it faces.
    fn panel(&mut self, c: [f32; 3], half: [f32; 2], facing: f32, rgba: [f32; 4]) {
        let (x0, x1, y0, y1) = (
            c[0] - half[0],
            c[0] + half[0],
            c[1] - half[1],
            c[1] + half[1],
        );
        self.quad(
            [
                [x0, y0, c[2]],
                [x1, y0, c[2]],
                [x1, y1, c[2]],
                [x0, y1, c[2]],
            ],
            [0.0, 0.0, facing],
            rgba,
        );
    }

    /// A fan round `centre` through `ring` (closed), facing `out`.
    fn fan(&mut self, centre: [f32; 3], ring: &[[f32; 3]], out: [f32; 3], rgba: [f32; 4]) {
        let base = self.positions.len() as u32;
        let n = [out[0], out[1], out[2]];
        self.positions.push(centre);
        self.normals.push(n);
        self.colors.push(rgba);
        for p in ring {
            self.positions.push(*p);
            self.normals.push(n);
            self.colors.push(rgba);
        }
        let k = ring.len() as u32;
        for i in 0..k {
            let (a, b) = (base + 1 + i, base + 1 + (i + 1) % k);
            // Wound so the face looks along `out`.
            let c = cross(
                sub(ring[(i as usize + 1) % ring.len()], centre),
                sub(ring[i as usize], centre),
            );
            if dot(c, out) >= 0.0 {
                self.indices.extend([base, b, a]);
            } else {
                self.indices.extend([base, a, b]);
            }
        }
    }

    /// An axis-aligned box, centre and half extents.
    fn cuboid(&mut self, c: [f32; 3], h: [f32; 3], rgba: [f32; 4]) {
        self.frustum(
            [c[0], c[1] - h[1], c[2]],
            [h[0], h[2]],
            [c[0], c[1] + h[1], c[2]],
            [h[0], h[2]],
            rgba,
        );
    }

    /// A box whose bottom and top rectangles differ: a tapered limb or torso.
    /// `lo`/`hi` are the centres of the two faces, the pairs their half extents
    /// in x and z.
    fn frustum(
        &mut self,
        lo: [f32; 3],
        lo_h: [f32; 2],
        hi: [f32; 3],
        hi_h: [f32; 2],
        rgba: [f32; 4],
    ) {
        let corner =
            |c: [f32; 3], h: [f32; 2], sx: f32, sz: f32| [c[0] + sx * h[0], c[1], c[2] + sz * h[1]];
        let ring = |c, h| {
            [
                corner(c, h, -1.0, -1.0),
                corner(c, h, 1.0, -1.0),
                corner(c, h, 1.0, 1.0),
                corner(c, h, -1.0, 1.0),
            ]
        };
        let (b, t) = (ring(lo, lo_h), ring(hi, hi_h));
        let mid = [
            (lo[0] + hi[0]) * 0.5,
            (lo[1] + hi[1]) * 0.5,
            (lo[2] + hi[2]) * 0.5,
        ];
        for i in 0..4 {
            let j = (i + 1) % 4;
            let face = [b[i], b[j], t[j], t[i]];
            let centre = scale(add(add(face[0], face[1]), add(face[2], face[3])), 0.25);
            self.quad(face, sub(centre, mid), rgba);
        }
        self.quad(t, [0.0, 1.0, 0.0], rgba);
        self.quad(b, [0.0, -1.0, 0.0], rgba);
    }

    /// A cylinder lying along x: a wheel. `hub`, where given, is a smaller
    /// disc on the outward cap.
    #[allow(clippy::too_many_arguments)]
    fn wheel(
        &mut self,
        c: [f32; 3],
        r: f32,
        half_w: f32,
        outward: f32,
        sides: usize,
        tyre: [f32; 4],
        hub: Option<[f32; 4]>,
    ) {
        let at = |k: usize, x: f32, rr: f32| {
            let a = k as f32 / sides as f32 * std::f32::consts::TAU;
            [x, c[1] + rr * a.sin(), c[2] + rr * a.cos()]
        };
        let (xi, xo) = (c[0] - half_w * outward, c[0] + half_w * outward);
        for k in 0..sides {
            let k1 = (k + 1) % sides;
            let mid = (k as f32 + 0.5) / sides as f32 * std::f32::consts::TAU;
            let out = [0.0, mid.sin(), mid.cos()];
            self.quad(
                [at(k, xi, r), at(k1, xi, r), at(k1, xo, r), at(k, xo, r)],
                out,
                tyre,
            );
        }
        let side = [outward, 0.0, 0.0];
        let ring: Vec<[f32; 3]> = (0..sides).map(|k| at(k, xo, r)).collect();
        self.fan([xo, c[1], c[2]], &ring, side, tyre);
        if let Some(hub) = hub {
            let x = xo + 0.004 * outward;
            let ring: Vec<[f32; 3]> = (0..sides).map(|k| at(k, x, r * 0.62)).collect();
            self.fan([x, c[1], c[2]], &ring, side, hub);
        }
    }
}

fn add(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn scale(a: [f32; 3], s: f32) -> [f32; 3] {
    [a[0] * s, a[1] * s, a[2] * s]
}
fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

/// sRGB hex to linear RGBA, for the trim's vertex colours.
fn lin(rgb: u32) -> [f32; 4] {
    let [r, g, b] = crate::scatter::srgb_to_linear_rgb(rgb);
    [r, g, b, 1.0]
}

const WHITE: [f32; 4] = [1.0, 1.0, 1.0, 1.0];
/// Tinted glazing: dark, slightly blue, which is what a windscreen is from
/// outside in daylight.
const GLASS: u32 = 0x2C_33_3B;
const TYRE: u32 = 0x24_24_25;
const HUB: u32 = 0x9C_A0_A4;
/// Unpainted bumper and sill plastic.
const PLASTIC: u32 = 0x33_34_36;
/// Dutch plates are yellow, front and rear.
const PLATE: u32 = 0xE8_C2_2A;
const HEADLAMP: u32 = 0xE6_E8_EA;
const TAIL_LAMP: u32 = 0xB0_1A_16;

/// The body types the pool draws, in the proportions it draws them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CarBody {
    /// A small hatchback (Polo, Clio): the commonest car on a Dutch street.
    Hatchback,
    /// A saloon with a separate boot.
    Saloon,
    /// An estate or compact SUV: roof carried to a near-vertical tailgate.
    Estate,
    /// A delivery van: tall, short bonnet, no rear side glass.
    Van,
}

impl CarBody {
    pub const ALL: [CarBody; 4] = [Self::Hatchback, Self::Saloon, Self::Estate, Self::Van];

    /// Which body the `i`-th car of the pool gets. Fixed per car, so a car
    /// keeps its shape when it is recycled — roughly 40 / 25 / 23 / 12 %.
    #[must_use]
    pub fn for_index(i: usize) -> Self {
        match (i * 37) % 52 {
            0..=20 => Self::Hatchback,
            21..=33 => Self::Saloon,
            34..=45 => Self::Estate,
            _ => Self::Van,
        }
    }

    /// Bumper to bumper, metres.
    #[must_use]
    pub fn length(self) -> f32 {
        let s = self.stations();
        s[0].z - s[s.len() - 1].z
    }

    /// Whether this body is usually white: most vans are.
    #[must_use]
    pub fn fleet_white(self) -> bool {
        matches!(self, Self::Van)
    }

    /// Cross-sections from the front bumper to the rear one. Between two
    /// stations the body is a loft; see [`Station`].
    fn stations(self) -> &'static [Station] {
        use Top::{Glass as G, Paint as P};
        match self {
            // 4.10 × 1.74 × 1.47 m.
            Self::Hatchback => {
                const S: &[Station] = &[
                    st(2.05, 0.32, 0.62, 0.62, 0.80, 0.80, P),
                    st(1.92, 0.22, 0.78, 0.78, 0.86, 0.86, P),
                    st(1.10, 0.22, 0.93, 0.93, 0.87, 0.87, G),
                    st(0.30, 0.22, 0.97, 1.46, 0.87, 0.67, P),
                    st(-1.30, 0.22, 0.99, 1.44, 0.87, 0.67, G),
                    st(-1.90, 0.24, 0.97, 1.04, 0.85, 0.72, P),
                    st(-2.05, 0.30, 0.90, 0.90, 0.80, 0.80, P),
                ];
                S
            }
            // 4.70 × 1.80 × 1.45 m.
            Self::Saloon => {
                const S: &[Station] = &[
                    st(2.35, 0.30, 0.60, 0.60, 0.82, 0.82, P),
                    st(2.20, 0.20, 0.74, 0.74, 0.89, 0.89, P),
                    st(1.05, 0.20, 0.88, 0.88, 0.90, 0.90, G),
                    st(0.15, 0.20, 0.93, 1.44, 0.90, 0.68, P),
                    st(-0.95, 0.20, 0.95, 1.42, 0.90, 0.68, G),
                    st(-1.65, 0.20, 0.97, 0.99, 0.89, 0.86, P),
                    st(-2.20, 0.22, 0.97, 0.97, 0.88, 0.88, P),
                    st(-2.35, 0.30, 0.85, 0.85, 0.82, 0.82, P),
                ];
                S
            }
            // 4.60 × 1.84 × 1.62 m.
            Self::Estate => {
                const S: &[Station] = &[
                    st(2.30, 0.36, 0.70, 0.70, 0.83, 0.83, P),
                    st(2.15, 0.26, 0.86, 0.86, 0.91, 0.91, P),
                    st(1.15, 0.26, 1.01, 1.01, 0.92, 0.92, G),
                    st(0.30, 0.26, 1.05, 1.62, 0.92, 0.72, P),
                    st(-2.05, 0.26, 1.07, 1.60, 0.92, 0.72, G),
                    st(-2.22, 0.28, 1.05, 1.18, 0.90, 0.80, P),
                    st(-2.30, 0.34, 0.98, 0.98, 0.86, 0.86, P),
                ];
                S
            }
            // 5.00 × 2.00 × 2.10 m.
            Self::Van => {
                const S: &[Station] = &[
                    st(2.50, 0.36, 0.78, 0.78, 0.92, 0.92, P),
                    st(2.38, 0.30, 0.98, 0.98, 0.99, 0.99, P),
                    st(1.80, 0.30, 1.12, 1.12, 1.00, 1.00, G),
                    st(1.05, 0.30, 1.18, 2.08, 1.00, 0.94, P),
                    st(0.25, 0.30, 1.18, 2.09, 1.00, 0.945, P),
                    st(-2.40, 0.30, 1.18, 2.10, 1.00, 0.95, P),
                    st(-2.50, 0.36, 1.18, 2.06, 0.98, 0.93, P),
                ];
                S
            }
        }
    }

    /// Whether the cabin side whose rear end is at `z0` is glazed. A van's
    /// are panels behind the cab doors.
    fn side_glass(self, z0: f32) -> bool {
        match self {
            Self::Van => z0 >= 0.2,
            _ => true,
        }
    }

    /// Where the wheels sit along the body, front and rear axle, and their radius.
    fn axles(self) -> (f32, f32, f32) {
        match self {
            Self::Hatchback => (1.30, -1.30, 0.31),
            Self::Saloon => (1.45, -1.40, 0.32),
            Self::Estate => (1.45, -1.40, 0.35),
            Self::Van => (1.65, -1.60, 0.36),
        }
    }

    /// The three meshes of this body, as a moving car draws it. See the module
    /// docs for why it is three.
    #[must_use]
    pub fn build(self) -> CarModel {
        self.build_at(Detail::Moving)
    }

    /// This body as a parked car: the same shape, cached, at [`Detail::Parked`].
    #[must_use]
    pub fn parked(self) -> &'static CarModel {
        static PARKED: std::sync::OnceLock<[CarModel; 4]> = std::sync::OnceLock::new();
        let all = PARKED.get_or_init(|| Self::ALL.map(|b| b.build_at(Detail::Parked)));
        &all[Self::ALL.iter().position(|b| *b == self).unwrap_or(0)]
    }

    /// The three meshes at a level of detail.
    #[must_use]
    pub fn build_at(self, detail: Detail) -> CarModel {
        let full = detail == Detail::Moving;
        let stations = self.stations();
        let mut body = ModelMesh::default();
        let mut trim = ModelMesh::default();
        let mut tail = ModelMesh::default();
        let glass = lin(GLASS);

        for pair in stations.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            for sx in [-1.0f32, 1.0] {
                // The lower body side: a vertical panel at the body's width.
                body.quad(
                    [
                        [sx * a.w_body, a.y_low, a.z],
                        [sx * b.w_body, b.y_low, b.z],
                        [sx * b.w_body, b.y_belt, b.z],
                        [sx * a.w_body, a.y_belt, a.z],
                    ],
                    [sx, 0.0, 0.0],
                    WHITE,
                );
                // The cabin side, leaning in from the belt to the roof edge.
                // Degenerate (and skipped) where neither end has a cabin.
                let side = [
                    [sx * a.w_body, a.y_belt, a.z],
                    [sx * b.w_body, b.y_belt, b.z],
                    [sx * b.w_roof, b.y_top, b.z],
                    [sx * a.w_roof, a.y_top, a.z],
                ];
                let out = [sx, 0.3, 0.0];
                if self.side_glass(b.z) {
                    trim.quad(side, out, glass);
                } else {
                    body.quad(side, out, WHITE);
                }
            }
            // The top: bonnet, windscreen, roof, rear window, boot lid.
            let top = [
                [-a.w_roof, a.y_top, a.z],
                [a.w_roof, a.y_top, a.z],
                [b.w_roof, b.y_top, b.z],
                [-b.w_roof, b.y_top, b.z],
            ];
            match a.top {
                Top::Glass => trim.quad(top, [0.0, 1.0, 0.0], glass),
                Top::Paint => body.quad(top, [0.0, 1.0, 0.0], WHITE),
            }
            // The underside, so a car seen from a low kerb is not hollow. A
            // parked one is never seen from below the kerb.
            if full {
                body.quad(
                    [
                        [-a.w_body, a.y_low, a.z],
                        [a.w_body, a.y_low, a.z],
                        [b.w_body, b.y_low, b.z],
                        [-b.w_body, b.y_low, b.z],
                    ],
                    [0.0, -1.0, 0.0],
                    WHITE,
                );
            }
        }
        // Nose and tail faces.
        for (s, out) in [(stations[0], 1.0f32), (stations[stations.len() - 1], -1.0)] {
            body.quad(
                [
                    [-s.w_body, s.y_low, s.z],
                    [s.w_body, s.y_low, s.z],
                    [s.w_body, s.y_belt, s.z],
                    [-s.w_body, s.y_belt, s.z],
                ],
                [0.0, 0.0, out],
                WHITE,
            );
            if s.y_top > s.y_belt + 1e-3 {
                body.quad(
                    [
                        [-s.w_body, s.y_belt, s.z],
                        [s.w_body, s.y_belt, s.z],
                        [s.w_roof, s.y_top, s.z],
                        [-s.w_roof, s.y_top, s.z],
                    ],
                    [0.0, 0.0, out],
                    WHITE,
                );
            }
        }

        let front = stations[0];
        let rear = stations[stations.len() - 1];
        let width = stations.iter().fold(0.0f32, |w, s| w.max(s.w_body));

        // Bumpers: dark plastic bands proud of each end, the full width.
        let plastic = lin(PLASTIC);
        for (s, dz) in [(front, 1.0f32), (rear, -1.0)] {
            trim.cuboid(
                [0.0, s.y_low + 0.08, s.z - dz * 0.06],
                [s.w_body + 0.02, 0.08, 0.10],
                plastic,
            );
            trim.panel(
                [0.0, s.y_low + 0.10, s.z + dz * 0.045],
                [0.26, 0.055],
                dz,
                lin(PLATE),
            );
        }
        // Sills: the dark strip between the wheels that sits a body off the road.
        let (fa, ra, r) = self.axles();
        for sx in [-1.0f32, 1.0].into_iter().filter(|_| full) {
            trim.cuboid(
                [sx * (width - 0.02), 0.27, (fa + ra) * 0.5],
                [0.04, 0.06, (fa - ra) * 0.5 - r - 0.05],
                plastic,
            );
        }
        // Headlamps on the nose face, a grille between them.
        let lamp_y = front.y_belt - 0.10;
        for sx in [-1.0f32, 1.0] {
            trim.panel(
                [sx * (front.w_body - 0.18), lamp_y, front.z + 0.01],
                [0.15, 0.055],
                1.0,
                lin(HEADLAMP),
            );
        }
        if full {
            trim.panel(
                [0.0, lamp_y - 0.02, front.z + 0.008],
                [front.w_body - 0.36, 0.04],
                1.0,
                plastic,
            );
        }
        // Mirrors at the foot of the windscreen.
        let cabin = stations.iter().position(|s| s.y_top > s.y_belt + 0.1);
        if let Some(screen) = cabin
            .and_then(|i| stations.get(i.checked_sub(1)?))
            .filter(|_| full)
        {
            for sx in [-1.0f32, 1.0] {
                trim.cuboid(
                    [
                        sx * (screen.w_body + 0.08),
                        screen.y_belt + 0.08,
                        screen.z + 0.05,
                    ],
                    [0.08, 0.06, 0.04],
                    plastic,
                );
            }
        }
        // Wheels, poking just outside the body side so the disc reads. A
        // parked car's are coarser and bare: across a car park an octagon is a
        // wheel and a hub is a pixel.
        let tyre = lin(TYRE);
        let (sides, hub) = if full {
            (12, Some(lin(HUB)))
        } else {
            (8, None)
        };
        for z in [fa, ra] {
            for sx in [-1.0f32, 1.0] {
                trim.wheel([sx * (width - 0.07), r, z], r, 0.11, sx, sides, tyre, hub);
            }
        }
        // Tail lamps: their own mesh, so braking can light them.
        let tail_y = rear.y_belt - 0.12;
        for sx in [-1.0f32, 1.0] {
            tail.panel(
                [sx * (rear.w_body - 0.14), tail_y, rear.z - 0.01],
                [0.13, 0.07],
                -1.0,
                lin(TAIL_LAMP),
            );
        }
        CarModel { body, trim, tail }
    }
}

/// How much of a car to build.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Detail {
    /// The moving pool: one mesh shared by every car, so detail is nearly free.
    Moving,
    /// A parked car, merged into its cell's mesh once **per bay**: the same
    /// body, without the underside, sills, grille, mirrors and hubs, and with
    /// octagonal wheels.
    Parked,
}

/// A car body's three meshes.
#[derive(Debug, Clone)]
pub struct CarModel {
    pub body: ModelMesh,
    pub trim: ModelMesh,
    pub tail: ModelMesh,
}

/// What the top surface between this station and the next one is made of.
#[derive(Debug, Clone, Copy)]
enum Top {
    Paint,
    Glass,
}

/// One cross-section of a car body.
///
/// The lower body is a vertical-sided panel from `y_low` to `y_belt` at half
/// width `w_body`; above it the cabin leans in to the roof at `y_top`, half
/// width `w_roof`. Where there is no cabin (bonnet, boot) `y_top == y_belt`
/// and the top surface is the panel's own.
#[derive(Debug, Clone, Copy)]
struct Station {
    z: f32,
    y_low: f32,
    y_belt: f32,
    y_top: f32,
    w_body: f32,
    w_roof: f32,
    top: Top,
}

const fn st(
    z: f32,
    y_low: f32,
    y_belt: f32,
    y_top: f32,
    w_body: f32,
    w_roof: f32,
    top: Top,
) -> Station {
    Station {
        z,
        y_low,
        y_belt,
        y_top,
        w_body,
        w_roof,
        top,
    }
}

// ─── Walkers ─────────────────────────────────────────────────────────────────

/// Hip joint: height above the ground and half the gap between the legs.
pub const HIP_Y: f32 = 0.92;
pub const HIP_X: f32 = 0.09;
/// Shoulder joint.
pub const SHOULDER_Y: f32 = 1.42;
pub const SHOULDER_X: f32 = 0.215;

/// The neutral head of [`crate::furniture::form::PERSON`].
const HEAD: u32 = crate::vector_tiles::palette::FURNITURE_METAL;
const SHOE: u32 = 0x2A_28_27;

/// A 1.75 m walker in four meshes: the torso and head, one leg hung from its
/// hip and one arm hung from its shoulder. Each limb's origin is its joint, so
/// a rotation about x swings it.
#[derive(Debug, Clone)]
pub struct WalkerModel {
    pub torso: ModelMesh,
    pub leg: ModelMesh,
    pub arm: ModelMesh,
}

#[must_use]
pub fn walker() -> WalkerModel {
    let mut torso = ModelMesh::default();
    // Hips to shoulders, narrowing to the waist and out again.
    torso.frustum(
        [0.0, HIP_Y - 0.04, 0.0],
        [0.17, 0.11],
        [0.0, 1.16, 0.0],
        [0.16, 0.10],
        WHITE,
    );
    torso.frustum(
        [0.0, 1.16, 0.0],
        [0.16, 0.10],
        [0.0, 1.47, 0.0],
        [0.205, 0.11],
        WHITE,
    );
    // Neck and head, the head a little forward of the spine.
    let head = lin(HEAD);
    torso.cuboid([0.0, 1.51, 0.01], [0.05, 0.04, 0.05], head);
    torso.frustum(
        [0.0, 1.54, 0.02],
        [0.075, 0.09],
        [0.0, 1.74, 0.01],
        [0.08, 0.095],
        head,
    );

    let mut leg = ModelMesh::default();
    // Thigh and shin as one taper, then a shoe that points forward.
    leg.frustum(
        [0.0, -0.84, 0.0],
        [0.055, 0.06],
        [0.0, 0.0, 0.0],
        [0.08, 0.09],
        WHITE,
    );
    leg.cuboid([0.0, -0.87, 0.04], [0.055, 0.035, 0.13], lin(SHOE));

    let mut arm = ModelMesh::default();
    arm.frustum(
        [0.0, -0.60, 0.0],
        [0.04, 0.045],
        [0.0, 0.03, 0.0],
        [0.055, 0.06],
        WHITE,
    );
    arm.cuboid([0.0, -0.65, 0.0], [0.035, 0.05, 0.04], head);

    WalkerModel { torso, leg, arm }
}

/// One full gait cycle (two steps), metres. A step is about 0.7 m at an
/// ordinary walking pace.
pub const STRIDE_M: f32 = 1.4;

/// The limb angles and body lift at `phase` (radians, one cycle per
/// [`STRIDE_M`]) for a walker going at `pace` of its normal speed.
///
/// Returns `(left leg, right leg, left arm, right arm, lift)`: the legs swing
/// in opposition, each arm against the leg on its own side, and the body is
/// highest when the legs pass each other. At `pace` 0 everything rests at zero,
/// so a walker that stops stands straight rather than freezing mid-stride.
#[must_use]
pub fn gait(phase: f32, pace: f32) -> (f32, f32, f32, f32, f32) {
    let k = pace.clamp(0.0, 1.3);
    let swing = phase.sin() * 0.42 * k;
    let arm = phase.sin() * 0.30 * k;
    let lift = (2.0 * phase).cos() * 0.5 + 0.5;
    (swing, -swing, -arm, arm, (lift - 1.0) * 0.035 * k)
}

// ─── Crows ───────────────────────────────────────────────────────────────────

/// The crow's neck joint on its body, which is where the head mesh's origin
/// hangs. The body's origin is on the ground between the feet, `+z` the beak.
pub const CROW_NECK: [f32; 3] = [0.0, 0.205, 0.105];
/// The `+x` wing's shoulder on the body; the `-x` one is its mirror.
pub const CROW_SHOULDER: [f32; 3] = [0.034, 0.205, 0.045];
/// How far along its own `±x` the inner wing reaches: where the outer one hangs.
pub const CROW_ARM_M: f32 = 0.19;

/// Plumage. Not black but a very dark blue-grey, which is what a crow is in
/// daylight; pure black reads as a hole in the picture rather than a bird.
const CROW_FEATHER: u32 = 0x1B_1C_21;
/// The flight feathers and the tail, a shade lighter, so a spread wing reads
/// against the body it crosses.
const CROW_PRIMARY: u32 = 0x23_25_2A;
const CROW_BEAK: u32 = 0x16_16_18;
const CROW_LEG: u32 = 0x1C_1C_1E;

/// A carrion crow, low-poly, about 46 cm beak to tail and 95 cm across the
/// wings — the bird a Dutch lawn actually has.
///
/// Six meshes, because six things move: the body (with the tail and legs), the
/// head on its neck, and each wing in two segments — the arm from the shoulder
/// and the hand hung from the arm's tip. One flat panel per wing would flap like
/// a hinged board; the hand trailing the arm through the beat is most of what
/// makes it a wingbeat. `[0]` is the `+x` side, `[1]` its mirror. Each part's
/// origin is its joint, and the wings span along their own `±x` with the plane
/// of the feathers in x–z, so a rotation about z raises them.
#[derive(Debug, Clone)]
pub struct CrowModel {
    pub body: ModelMesh,
    pub head: ModelMesh,
    pub arm: [ModelMesh; 2],
    pub hand: [ModelMesh; 2],
}

impl ModelMesh {
    /// One flat-shaded triangle, wound and lit to face along `out`.
    fn tri(&mut self, t: [[f32; 3]; 3], out: [f32; 3], rgba: [f32; 4]) {
        let n = cross(sub(t[1], t[0]), sub(t[2], t[0]));
        let len = dot(n, n).sqrt();
        if len < 1e-9 {
            return;
        }
        let (n, t) = if dot(n, out) < 0.0 {
            (scale(n, -1.0 / len), [t[0], t[2], t[1]])
        } else {
            (scale(n, 1.0 / len), t)
        };
        let base = self.positions.len() as u32;
        for p in t {
            self.positions.push(p);
            self.normals.push(n);
            self.colors.push(rgba);
        }
        self.indices.extend([base, base + 1, base + 2]);
    }

    /// A body of revolution along `z`, hexagonal in section: each station is
    /// `(z, y, half width, half height, colour of the band behind it)`, and a
    /// station of zero size closes the end to a point.
    fn loft(&mut self, stations: &[(f32, f32, f32, f32, [f32; 4])]) {
        const SIDES: usize = 6;
        let ring = |(z, y, w, h, _): (f32, f32, f32, f32, [f32; 4])| {
            std::array::from_fn::<[f32; 3], SIDES, _>(|k| {
                // A vertex on top, so the back has a ridge and the belly a keel.
                let a = std::f32::consts::FRAC_PI_2 + k as f32 * std::f32::consts::TAU / 6.0;
                [w * a.cos(), y + h * a.sin(), z]
            })
        };
        for pair in stations.windows(2) {
            let (a, b) = (ring(pair[0]), ring(pair[1]));
            let axis = [
                0.0,
                (pair[0].1 + pair[1].1) * 0.5,
                (pair[0].0 + pair[1].0) * 0.5,
            ];
            let rgba = pair[0].4;
            for k in 0..SIDES {
                let j = (k + 1) % SIDES;
                for t in [[a[k], a[j], b[j]], [a[k], b[j], b[k]]] {
                    let c = scale(add(add(t[0], t[1]), t[2]), 1.0 / 3.0);
                    self.tri(t, sub(c, axis), rgba);
                }
            }
        }
    }

    /// A flat polygon seen from both sides — a wing, a tail, a foot — fanned
    /// from `hub`, which every edge of `poly` must be visible from.
    fn plate(&mut self, hub: [f32; 3], poly: &[[f32; 3]], rgba: [f32; 4]) {
        for i in 0..poly.len() {
            let t = [hub, poly[i], poly[(i + 1) % poly.len()]];
            let n = cross(sub(t[1], t[0]), sub(t[2], t[0]));
            self.tri(t, n, rgba);
            self.tri(t, scale(n, -1.0), rgba);
        }
    }
}

#[must_use]
pub fn crow() -> CrowModel {
    let feather = lin(CROW_FEATHER);
    let primary = lin(CROW_PRIMARY);

    let mut body = ModelMesh::default();
    // Rump to the base of the neck: a deep chest, a back that falls to the tail.
    body.loft(&[
        (-0.125, 0.150, 0.0, 0.0, feather),
        (-0.090, 0.148, 0.030, 0.028, feather),
        (-0.030, 0.152, 0.052, 0.050, feather),
        (0.035, 0.160, 0.056, 0.058, feather),
        (0.085, 0.180, 0.045, 0.048, feather),
        (0.120, 0.200, 0.026, 0.028, feather),
        (0.135, 0.205, 0.0, 0.0, feather),
    ]);
    // The tail: a wedge that widens to its tip and droops a little.
    body.plate(
        [0.0, 0.137, -0.17],
        &[
            [-0.022, 0.150, -0.085],
            [0.022, 0.150, -0.085],
            [0.044, 0.124, -0.245],
            [0.0, 0.120, -0.255],
            [-0.044, 0.124, -0.245],
        ],
        primary,
    );
    let leg = lin(CROW_LEG);
    for x in [-0.022, 0.022] {
        body.frustum(
            [x, 0.0, 0.006],
            [0.006, 0.006],
            [x, 0.112, 0.0],
            [0.010, 0.014],
            leg,
        );
        // Three toes forward and one back, flat on the ground.
        body.plate(
            [x, 0.004, 0.012],
            &[
                [x, 0.004, -0.032],
                [x + 0.022, 0.004, 0.040],
                [x, 0.004, 0.056],
                [x - 0.022, 0.004, 0.040],
            ],
            leg,
        );
    }

    let mut head = ModelMesh::default();
    // Crown to the tip of a heavy beak, which is the crow's silhouette.
    head.loft(&[
        (-0.040, 0.000, 0.0, 0.0, feather),
        (-0.025, 0.004, 0.026, 0.028, feather),
        (0.012, 0.012, 0.030, 0.031, feather),
        (0.040, 0.006, 0.018, 0.019, lin(CROW_BEAK)),
        (0.100, -0.004, 0.0, 0.0, lin(CROW_BEAK)),
    ]);

    // The arm: from the shoulder, a little arched along its leading edge.
    let arm_poly = [
        [0.0, 0.0, 0.060],
        [0.10, 0.008, 0.062],
        [CROW_ARM_M, 0.0, 0.045],
        [CROW_ARM_M, 0.0, -0.085],
        [0.0, 0.0, -0.100],
    ];
    let arm_hub = [0.09, 0.004, -0.015];
    // The hand, ending in the fingered primaries a crow is told apart by.
    let hand_poly = [
        [0.0, 0.0, 0.045],
        [0.14, 0.0, 0.025],
        [0.235, 0.0, -0.015],
        [0.205, 0.0, -0.035],
        [0.225, 0.0, -0.050],
        [0.185, 0.0, -0.065],
        [0.200, 0.0, -0.080],
        [0.150, 0.0, -0.085],
        [0.0, 0.0, -0.085],
    ];
    let hand_hub = [0.05, 0.0, -0.02];
    let mirror = |p: [f32; 3]| [-p[0], p[1], p[2]];
    let side = |hub: [f32; 3], poly: &[[f32; 3]], sign: f32, rgba| {
        let flip = |p: [f32; 3]| if sign < 0.0 { mirror(p) } else { p };
        let mut m = ModelMesh::default();
        let poly: Vec<[f32; 3]> = poly.iter().map(|&p| flip(p)).collect();
        m.plate(flip(hub), &poly, rgba);
        m
    };
    CrowModel {
        body,
        head,
        arm: [
            side(arm_hub, &arm_poly, 1.0, feather),
            side(arm_hub, &arm_poly, -1.0, feather),
        ],
        hand: [
            side(hand_hub, &hand_poly, 1.0, primary),
            side(hand_hub, &hand_poly, -1.0, primary),
        ],
    }
}

/// One wing's joint angles, for the `+x` wing (the client mirrors them).
///
/// The `+x` shoulder of a folded wing: lower and a little in, so the folded
/// wing's leading edge lies along the back rather than standing above it.
pub const CROW_SHOULDER_FOLDED: [f32; 3] = [0.030, 0.172, 0.040];

/// One wing's joint angles, for the `+x` wing (the client mirrors them).
///
/// The arm is turned `Ry(sweep) · Rz(raise) · Rx(twist)` about its shoulder:
/// twisted about its own span, raised about the body's long axis, then swept
/// back. The hand is `Ry(hand_sweep) · Rz(hand_raise)` about the arm's tip, and
/// `hand_span` scales its length — a hand folding on the upstroke is one
/// shortening in span, which is all it is from any distance. `tuck` moves the
/// shoulder from [`CROW_SHOULDER`] (0) to [`CROW_SHOULDER_FOLDED`] (1).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WingPose {
    pub sweep: f32,
    pub raise: f32,
    pub twist: f32,
    pub hand_sweep: f32,
    pub hand_raise: f32,
    pub hand_span: f32,
    pub tuck: f32,
}

impl WingPose {
    /// Folded on a standing bird: swept back along the body and twisted so the
    /// feathers hang down the flank from a leading edge along the back — the
    /// two wings a ridge over the bird, the way a crow's wings sit. Without the
    /// twist the folded plates lay flat and wide across the back, and a crow
    /// seen from above was a dark mushroom. The hand is tucked short so the
    /// primaries end near the tail's tip.
    pub const FOLDED: Self = Self {
        sweep: 1.45,
        raise: -0.2,
        twist: -2.1,
        hand_sweep: 0.12,
        hand_raise: 0.0,
        hand_span: 0.42,
        tuck: 1.0,
    };

    #[must_use]
    pub fn lerp(self, to: Self, t: f32) -> Self {
        let t = t.clamp(0.0, 1.0);
        // Exact at both ends, so a fully folded or fully open wing is the pose.
        let l = |a: f32, b: f32| a * (1.0 - t) + b * t;
        Self {
            sweep: l(self.sweep, to.sweep),
            raise: l(self.raise, to.raise),
            twist: l(self.twist, to.twist),
            hand_sweep: l(self.hand_sweep, to.hand_sweep),
            hand_raise: l(self.hand_raise, to.hand_raise),
            hand_span: l(self.hand_span, to.hand_span),
            tuck: l(self.tuck, to.tuck),
        }
    }

    /// The `+x` shoulder for this pose.
    #[must_use]
    pub fn shoulder(self) -> [f32; 3] {
        let t = self.tuck.clamp(0.0, 1.0);
        std::array::from_fn(|i| CROW_SHOULDER[i] * (1.0 - t) + CROW_SHOULDER_FOLDED[i] * t)
    }
}

/// The open wing at `phase` (radians, one wingbeat per turn) and `beat`, how
/// hard it is flapping: 0 is a glide — wings out, held in a shallow V, whatever
/// the phase — and 1 a full stroke, up to about 55° and down to about 30°.
///
/// The hand lags the arm, so the tip is still going down as the arm starts up,
/// and on the upstroke (rising, where `cos(phase) > 0`) the wing sweeps back
/// and shortens. That lag and that fold are the difference between a bird
/// rowing through the air and a board on a hinge.
#[must_use]
pub fn wingbeat(phase: f32, beat: f32) -> WingPose {
    let k = beat.clamp(0.0, 1.0);
    let rising = phase.cos().max(0.0);
    WingPose {
        sweep: 0.10 + k * 0.12 * rising,
        raise: 0.10 + k * (0.75 * phase.sin() + 0.08),
        twist: 0.0,
        hand_sweep: 0.10 + k * 0.30 * rising,
        hand_raise: 0.05 + k * 0.45 * (phase - 0.8).sin(),
        hand_span: 1.0 - k * 0.28 * rising,
        tuck: 0.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn closed_enough(m: &ModelMesh) {
        assert_eq!(m.positions.len(), m.normals.len());
        assert_eq!(m.positions.len(), m.colors.len());
        assert!(m.indices.iter().all(|&i| (i as usize) < m.positions.len()));
        for n in &m.normals {
            let len = dot(*n, *n).sqrt();
            assert!((len - 1.0).abs() < 1e-3, "normal not unit: {n:?}");
        }
    }

    #[test]
    fn every_body_builds_and_is_about_a_car() {
        for body in CarBody::ALL {
            let m = body.build();
            for part in [&m.body, &m.trim, &m.tail] {
                assert!(!part.indices.is_empty(), "{body:?} has an empty part");
                closed_enough(part);
            }
            let ys = m.body.positions.iter().map(|p| p[1]);
            let top = ys.fold(0.0f32, f32::max);
            let len = body.length();
            assert!((3.8..=5.2).contains(&len), "{body:?} is {len} m long");
            assert!((1.3..=2.2).contains(&top), "{body:?} is {top} m tall");
            // Nothing below the road but a tyre's contact patch.
            let low = m
                .trim
                .positions
                .iter()
                .map(|p| p[1])
                .fold(f32::MAX, f32::min);
            assert!(low > -1e-3, "{body:?} reaches {low} m under the road");
        }
    }

    /// The windscreen faces forward and up, not into the cabin: a normal
    /// flipped by a winding mistake makes the glass vanish under back-face
    /// culling, which no other test would notice.
    #[test]
    fn the_cabin_faces_outward() {
        let centre = [0.0, 0.8, 0.0];
        for body in CarBody::ALL {
            let m = body.build();
            let mut panes = 0;
            for (i, tri) in m.trim.indices.chunks(3).enumerate() {
                let v = tri[0] as usize;
                if m.trim.colors[v] != lin(GLASS) {
                    continue;
                }
                panes += 1;
                let (p, n) = (m.trim.positions[v], m.trim.normals[v]);
                assert!(
                    dot(n, sub(p, centre)) > -1e-3,
                    "{body:?} glass triangle {i} faces in"
                );
            }
            assert!(panes >= 6, "{body:?} has {panes} glass triangles");
        }
    }

    /// A parked car is merged once per bay, so its vertex count is the lot's.
    /// The seven-box form it replaced was 168 vertices; this pins the cost of
    /// the real shape near twice that, not the moving model's.
    #[test]
    fn a_parked_car_stays_cheap() {
        for body in CarBody::ALL {
            let count = |m: &CarModel| {
                [&m.body, &m.trim, &m.tail]
                    .iter()
                    .map(|p| p.positions.len())
                    .sum::<usize>()
            };
            let (parked, moving) = (count(body.parked()), count(&body.build()));
            println!("{body:?}: parked {parked} vertices, moving {moving}");
            assert!(parked <= 400, "{body:?} parks at {parked} vertices");
            assert!(parked < moving, "{body:?}");
        }
    }

    #[test]
    fn the_pool_gets_every_body() {
        let mut seen = [0usize; 4];
        for i in 0..180 {
            let k = CarBody::ALL
                .iter()
                .position(|b| *b == CarBody::for_index(i))
                .unwrap();
            seen[k] += 1;
        }
        assert!(seen.iter().all(|&n| n >= 15), "{seen:?}");
        assert!(seen[0] > seen[3], "more hatchbacks than vans: {seen:?}");
    }

    #[test]
    fn a_stopped_walker_stands_straight() {
        for phase in [0.0, 1.0, 2.5, 4.0] {
            let (l, r, la, ra, lift) = gait(phase, 0.0);
            assert_eq!((l, r, la, ra, lift), (0.0, 0.0, 0.0, 0.0, 0.0));
        }
        let (l, r, la, _, _) = gait(std::f32::consts::FRAC_PI_2, 1.0);
        assert!(l > 0.3 && r < -0.3, "legs swing in opposition");
        assert!(la * l < 0.0, "an arm swings against its own leg");
    }

    #[test]
    fn the_walker_is_a_person_high() {
        let w = walker();
        let top = w
            .torso
            .positions
            .iter()
            .map(|p| p[1])
            .fold(0.0f32, f32::max);
        assert!((1.7..1.8).contains(&top), "{top}");
        let foot = w
            .leg
            .positions
            .iter()
            .map(|p| p[1])
            .fold(f32::MAX, f32::min);
        assert!(
            (HIP_Y + foot).abs() < 0.03,
            "the foot reaches the ground: {}",
            HIP_Y + foot
        );
    }

    #[test]
    fn a_crow_is_crow_sized_and_stands_on_its_feet() {
        let c = crow();
        for part in [
            &c.body, &c.head, &c.arm[0], &c.arm[1], &c.hand[0], &c.hand[1],
        ] {
            assert!(!part.indices.is_empty());
            closed_enough(part);
        }
        let zs = || c.body.positions.iter().map(|p| p[2]);
        let length = zs().fold(f32::MIN, f32::max) - zs().fold(f32::MAX, f32::min)
            + (CROW_NECK[2] + 0.10 - 0.135);
        assert!((0.40..0.52).contains(&length), "{length} m beak to tail");
        let span_x = |m: &ModelMesh| m.positions.iter().map(|p| p[0].abs()).fold(0.0, f32::max);
        let span = 2.0 * (CROW_SHOULDER[0] + span_x(&c.arm[0]) + span_x(&c.hand[0]));
        assert!((0.85..1.05).contains(&span), "{span} m across the wings");
        let low = c
            .body
            .positions
            .iter()
            .map(|p| p[1])
            .fold(f32::MAX, f32::min);
        assert!(low.abs() < 1e-3, "the feet are on the ground: {low}");
    }

    /// The `-x` wing is the `+x` one mirrored, and each face of a plate is lit
    /// from its own side: a wing that vanishes from below under back-face
    /// culling is the failure this would otherwise hide.
    #[test]
    fn the_wings_mirror_and_show_both_faces() {
        let c = crow();
        for (a, b) in [(&c.arm[0], &c.arm[1]), (&c.hand[0], &c.hand[1])] {
            assert_eq!(a.positions.len(), b.positions.len());
            assert!(a.positions.iter().all(|p| p[0] >= -1e-6));
            assert!(b.positions.iter().all(|p| p[0] <= 1e-6));
            let up = a.normals.iter().filter(|n| n[1] > 0.9).count();
            let down = a.normals.iter().filter(|n| n[1] < -0.9).count();
            assert_eq!(up, down, "as many faces up as down");
        }
    }

    #[test]
    fn a_glide_holds_still_and_a_beat_strokes() {
        let glide = wingbeat(0.0, 0.0);
        for k in 0..16 {
            assert_eq!(
                wingbeat(k as f32 * 0.4, 0.0),
                glide,
                "a glide ignores the phase"
            );
        }
        let raises: Vec<f32> = (0..64)
            .map(|k| wingbeat(k as f32 / 64.0 * std::f32::consts::TAU, 1.0).raise)
            .collect();
        let (hi, lo) = (
            raises.iter().copied().fold(f32::MIN, f32::max),
            raises.iter().copied().fold(f32::MAX, f32::min),
        );
        assert!(hi > 0.8 && lo < -0.4, "stroke {lo}..{hi}");
        assert_eq!(WingPose::FOLDED.lerp(glide, 0.0), WingPose::FOLDED);
        assert_eq!(WingPose::FOLDED.lerp(glide, 1.0), glide);
    }
}
