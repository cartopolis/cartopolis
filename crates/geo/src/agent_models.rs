//! The moving traffic's own models: four car bodies and a walker with limbs.
//!
//! The parked cars ([`crate::furniture::form::CAR`]) are seven boxes because a
//! car park holds hundreds of them merged into one cell mesh, and every box is
//! multiplied by the lot. The ambient pool (`cartopolis`'s `systems::traffic`)
//! is the opposite case: a fixed set of entities sharing **one mesh per model**,
//! uploaded once at startup. A model there costs its vertices once, not once per
//! car, so it can afford a sloped windscreen, round wheels and lamps.
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
//! [`crate::furniture::form::PERSON`], for the reason given there.
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
    /// One flat triangle, wound so its front face points along `out`.
    fn tri(&mut self, a: [f32; 3], b: [f32; 3], c: [f32; 3], out: [f32; 3], rgba: [f32; 4]) {
        let n = cross(sub(b, a), sub(c, a));
        let len = dot(n, n).sqrt();
        if len < 1e-7 {
            return;
        }
        let mut n = [n[0] / len, n[1] / len, n[2] / len];
        let (b, c) = if dot(n, out) < 0.0 {
            n = [-n[0], -n[1], -n[2]];
            (c, b)
        } else {
            (b, c)
        };
        let base = self.positions.len() as u32;
        for p in [a, b, c] {
            self.positions.push(p);
            self.normals.push(n);
            self.colors.push(rgba);
        }
        self.indices.extend([base, base + 1, base + 2]);
    }

    /// A planar-ish quad `a b c d` (in order round its edge), facing `out`.
    fn quad(&mut self, q: [[f32; 3]; 4], out: [f32; 3], rgba: [f32; 4]) {
        self.tri(q[0], q[1], q[2], out, rgba);
        self.tri(q[0], q[2], q[3], out, rgba);
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

    /// A cylinder lying along x: a wheel. `hub` colours the outward cap.
    fn wheel(
        &mut self,
        c: [f32; 3],
        r: f32,
        half_w: f32,
        outward: f32,
        tyre: [f32; 4],
        hub: [f32; 4],
    ) {
        const SIDES: usize = 12;
        let at = |k: usize, x: f32| {
            let a = k as f32 / SIDES as f32 * std::f32::consts::TAU;
            [x, c[1] + r * a.sin(), c[2] + r * a.cos()]
        };
        let (xi, xo) = (c[0] - half_w * outward, c[0] + half_w * outward);
        for k in 0..SIDES {
            let k1 = (k + 1) % SIDES;
            let mid = (k as f32 + 0.5) / SIDES as f32 * std::f32::consts::TAU;
            let out = [0.0, mid.sin(), mid.cos()];
            self.quad([at(k, xi), at(k1, xi), at(k1, xo), at(k, xo)], out, tyre);
            let centre_o = [xo, c[1], c[2]];
            // The hub is a smaller disc on the outer face; the ring round it is tyre.
            let hub_at = |k: usize| {
                let p = at(k, xo);
                [
                    xo + 0.004 * outward,
                    c[1] + (p[1] - c[1]) * 0.62,
                    c[2] + (p[2] - c[2]) * 0.62,
                ]
            };
            let hub_c = [xo + 0.004 * outward, c[1], c[2]];
            self.tri(centre_o, at(k, xo), at(k1, xo), [outward, 0.0, 0.0], tyre);
            self.tri(hub_c, hub_at(k), hub_at(k1), [outward, 0.0, 0.0], hub);
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

    /// The three meshes of this body. See the module docs for why it is three.
    #[must_use]
    pub fn build(self) -> CarModel {
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
            // The underside, so a car seen from a low kerb is not hollow.
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
            trim.cuboid(
                [0.0, s.y_low + 0.10, s.z + dz * 0.05],
                [0.26, 0.055, 0.008],
                lin(PLATE),
            );
        }
        // Sills: the dark strip between the wheels that sits a body off the road.
        let (fa, ra, r) = self.axles();
        for sx in [-1.0f32, 1.0] {
            trim.cuboid(
                [sx * (width - 0.02), 0.27, (fa + ra) * 0.5],
                [0.04, 0.06, (fa - ra) * 0.5 - r - 0.05],
                plastic,
            );
        }
        // Headlamps on the nose face, a grille between them.
        let lamp_y = front.y_belt - 0.10;
        for sx in [-1.0f32, 1.0] {
            trim.cuboid(
                [sx * (front.w_body - 0.18), lamp_y, front.z + 0.005],
                [0.15, 0.055, 0.02],
                lin(HEADLAMP),
            );
        }
        trim.cuboid(
            [0.0, lamp_y - 0.02, front.z + 0.004],
            [front.w_body - 0.36, 0.04, 0.015],
            plastic,
        );
        // Mirrors at the foot of the windscreen.
        let cabin = stations.iter().position(|s| s.y_top > s.y_belt + 0.1);
        if let Some(screen) = cabin.and_then(|i| stations.get(i.checked_sub(1)?)) {
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
        // Wheels, poking just outside the body side so the disc reads.
        let (tyre, hub) = (lin(TYRE), lin(HUB));
        for z in [fa, ra] {
            for sx in [-1.0f32, 1.0] {
                trim.wheel([sx * (width - 0.07), r, z], r, 0.11, sx, tyre, hub);
            }
        }
        // Tail lamps: their own mesh, so braking can light them.
        let tail_y = rear.y_belt - 0.12;
        for sx in [-1.0f32, 1.0] {
            tail.cuboid(
                [sx * (rear.w_body - 0.14), tail_y, rear.z - 0.012],
                [0.13, 0.07, 0.02],
                lin(TAIL_LAMP),
            );
        }
        CarModel { body, trim, tail }
    }
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
}
