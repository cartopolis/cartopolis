//! A person cut down for a crowd: the same MakeHuman body an avatar is, made
//! cheap enough to draw two hundred times.
//!
//! An avatar is ~33 000 triangles, seven textures and a skeleton of its own
//! animated every frame. A crowd figure is one mesh at two levels of detail
//! (a few thousand triangles near, a few hundred far), coloured per vertex
//! from those textures, and its walk is not a skeleton but a handful of
//! **baked poses**: every vertex's position and normal at each frame of one
//! walk cycle, plus one standing pose, which the client puts in a texture for
//! the vertex shader to play. Every walker wearing a body then shares one
//! mesh and one draw.
//!
//! The simplification is a half-edge collapse under quadric error (Garland
//! and Heckbert): a vertex is only ever merged into a neighbour, never moved,
//! so every vertex left is one of the body's own and keeps its own skin
//! weights — which is what lets the poses be skinned after the mesh is cut.

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};

use glam::{DVec3, Mat4, Quat, Vec3};

use crate::{HumanModel, PartKind, ProxyKind};

/// A part's texture, small and in the CPU's hands, to colour vertices from.
/// sRGB bytes, rows top first (a `v` of 0 is the top row, as the parts' UVs
/// are).
#[derive(Clone, Debug)]
pub struct Swatch {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<[u8; 4]>,
}

impl Swatch {
    /// The texel under `uv`, bilinear, in linear light (alpha as stored).
    #[must_use]
    pub fn sample(&self, uv: [f32; 2]) -> [f32; 4] {
        let (w, h) = (self.width.max(1) as i64, self.height.max(1) as i64);
        let x = uv[0].rem_euclid(1.0) * w as f32 - 0.5;
        let y = uv[1].rem_euclid(1.0) * h as f32 - 0.5;
        let (x0, y0) = (x.floor(), y.floor());
        let (fx, fy) = (x - x0, y - y0);
        let at = |dx: i64, dy: i64| {
            let px = (x0 as i64 + dx).rem_euclid(w);
            let py = (y0 as i64 + dy).clamp(0, h - 1);
            let t = self
                .rgba
                .get((py * w + px) as usize)
                .copied()
                .unwrap_or([255; 4]);
            [
                srgb_to_linear(t[0]),
                srgb_to_linear(t[1]),
                srgb_to_linear(t[2]),
                f32::from(t[3]) / 255.0,
            ]
        };
        let (a, b, c, d) = (at(0, 0), at(1, 0), at(0, 1), at(1, 1));
        std::array::from_fn(|k| {
            let top = a[k] + (b[k] - a[k]) * fx;
            let bottom = c[k] + (d[k] - c[k]) * fx;
            top + (bottom - top) * fy
        })
    }
}

fn srgb_to_linear(c: u8) -> f32 {
    let c = f32::from(c) / 255.0;
    if c <= 0.040_45 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// How one of the model's parts is painted: its texture, the colour over it
/// (linear), and which of the crowd's tint regions it is.
#[derive(Clone, Copy, Debug)]
pub struct PartPaint<'a> {
    pub swatch: Option<&'a Swatch>,
    pub tint: [f32; 3],
    pub region: Region,
}

/// What a vertex's colour means to the shader, carried in its alpha: its own
/// colour, or a grey to be multiplied by the walker's top or bottom colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Region {
    Own,
    Top,
    Bottom,
}

impl Region {
    /// The vertex colour's alpha the shader reads this region from.
    #[must_use]
    pub fn alpha(self) -> f32 {
        match self {
            Region::Own => 0.0,
            Region::Top => 0.5,
            Region::Bottom => 1.0,
        }
    }
}

/// The parts a crowd figure is made of. Brows and lashes are cut-out cards a
/// few pixels across at street distance, and glasses a wire frame; drawn
/// opaque by vertex colour they would be slabs, so they are left off.
#[must_use]
pub fn drawn(kind: PartKind) -> bool {
    !matches!(
        kind,
        PartKind::Proxy(ProxyKind::Eyebrows | ProxyKind::Eyelashes | ProxyKind::Glasses)
    )
}

/// One level of detail, posed.
#[derive(Clone, Debug, Default)]
pub struct CrowdMesh {
    /// Per vertex: linear RGB, and the [`Region`] in alpha.
    pub colours: Vec<[f32; 4]>,
    pub indices: Vec<u32>,
    /// Per pose, every vertex: the walk cycle's poses in order, then standing.
    pub positions: Vec<Vec<[f32; 3]>>,
    pub normals: Vec<Vec<[f32; 3]>>,
}

impl CrowdMesh {
    #[must_use]
    pub fn vertices(&self) -> usize {
        self.colours.len()
    }

    #[must_use]
    pub fn triangles(&self) -> usize {
        self.indices.len() / 3
    }
}

/// Where a rider meets a bicycle, at the bike's own scale 1, in the body's
/// frame (origin on the ground, `+z` forward). The client passes
/// `cartopolis_geo::agent_models::BIKE_FIT`; this crate does not depend on geo.
#[derive(Clone, Copy, Debug)]
pub struct Ride {
    /// The top of the saddle.
    pub saddle: Vec3,
    /// The crank axle.
    pub bottom_bracket: Vec3,
    /// Axle to pedal.
    pub crank: f32,
    /// Either pedal's distance from the middle.
    pub pedal_x: f32,
    /// One grip, at `+x`; the other mirrors it.
    pub grip: Vec3,
    /// Poses baked through one turn of the cranks.
    pub poses: usize,
}

/// A person as the crowd draws them.
#[derive(Clone, Debug)]
pub struct CrowdBody {
    /// Finest first, one per triangle budget asked for.
    pub levels: Vec<CrowdMesh>,
    /// Poses in the walk cycle; the standing pose is the one after them.
    pub walk_poses: usize,
    /// Poses through one turn of the cranks, after the standing pose; 0 when
    /// no [`Ride`] was asked for. Pose `k` has the right pedal at `k / n` of a
    /// turn from the top, going forward (see [`pedal_at`]).
    pub ride_poses: usize,
    /// The scale this body rides the bike at: the bike, and the [`Ride`]
    /// points it was posed onto, scaled to its legs.
    pub bike_scale: f32,
    /// Metres walked in one cycle of the captured walk, on this body.
    pub stride: f32,
    /// Top of the head above the soles, metres.
    pub height: f32,
}

/// The stride when the pack has no walk: the old box walker's, near enough.
const FALLBACK_STRIDE_M: f32 = 1.4;

/// Cut `model` down to each of `budgets` triangles (finest first) and bake
/// `walk_poses` poses of its walk and one of it standing — and, given a
/// [`Ride`], its poses pedalling a bike. `paint` has one entry per
/// `model.parts`.
#[must_use]
pub fn crowd_body(
    model: &HumanModel,
    paint: &[PartPaint],
    budgets: &[usize],
    walk_poses: usize,
    ride: Option<&Ride>,
) -> CrowdBody {
    let welded = weld(model, paint);
    let mut simplifier = Simplifier::new(&welded);
    let mut levels = Vec::with_capacity(budgets.len());
    for &budget in budgets {
        simplifier.reduce_to(budget);
        levels.push(simplifier.snapshot());
    }

    let mut poses = poses(model, walk_poses);
    let walk_poses = poses.len() - 1;
    let (ride_poses, bike_scale) = match ride {
        Some(ride) => {
            let (riding, scale) = ride_poses(model, ride, &poses[walk_poses]);
            let n = riding.len();
            poses.extend(riding);
            (n, scale)
        }
        None => (0, 1.0),
    };
    let skins: Vec<Vec<Mat4>> = poses.iter().map(|p| skin_matrices(model, p)).collect();
    let levels = levels
        .into_iter()
        .map(|(kept, colours, indices)| {
            let mut positions = Vec::with_capacity(skins.len());
            let mut normals = Vec::with_capacity(skins.len());
            for skin in &skins {
                let at: Vec<Vec3> = kept
                    .iter()
                    .map(|&v| {
                        let (j, w) = (&welded.joints[v], &welded.weights[v]);
                        let rest = welded.positions[v].as_vec3();
                        (0..4)
                            .filter(|&k| w[k] > 0.0)
                            .map(|k| skin[j[k] as usize].transform_point3(rest) * w[k])
                            .sum::<Vec3>()
                    })
                    .collect();
                normals.push(smooth_normals(&at, &indices));
                positions.push(at.iter().map(|p| p.to_array()).collect());
            }
            CrowdMesh {
                colours,
                indices,
                positions,
                normals,
            }
        })
        .collect();

    let stride = model
        .clips
        .iter()
        .find(|c| c.name == "walk" && c.speed > 0.0)
        .map_or(FALLBACK_STRIDE_M, |c| c.speed * c.duration());
    CrowdBody {
        levels,
        walk_poses,
        ride_poses,
        bike_scale,
        stride,
        height: model.height,
    }
}

/// Where a pedal is at `turn` radians from the top, forward, relative to the
/// crank axle: the right pedal at `turn`, the left half a turn on. `x` is the
/// pedal's side, `±1`.
#[must_use]
pub fn pedal_at(turn: f32, crank: f32) -> Vec3 {
    Vec3::new(0.0, crank * turn.cos(), crank * turn.sin())
}

/// How far the foot points its toe down at `turn` from the top, radians:
/// nearly level over the top, most at the bottom of the stroke — the ankling
/// a rider does without thinking.
fn ankling(turn: f32) -> f32 {
    0.22 - 0.12 * turn.cos()
}

/// How far the hip joint sits above the saddle, metres for a leg of
/// [`REFERENCE_LEG_M`]: the seat bones are a hand under the joint.
const HIP_OVER_SADDLE_M: f32 = 0.10;

/// The default man's leg, hip joint to ankle, straight; what
/// [`HIP_OVER_SADDLE_M`] is measured against.
const REFERENCE_LEG_M: f32 = 0.86;

/// How straight the leg is at the bottom of the stroke, as a share of its
/// length: a saddle set right leaves the knee a little bent there.
const LEG_AT_BOTTOM: f32 = 0.95;

/// How far the wrist is from the shoulder on the bars, as a share of the arm:
/// elbows soft, not locked.
const ARM_ON_BARS: f32 = 0.9;

/// The wrist's place relative to the middle of a grip: a hand's breadth back
/// along the bar and a little above it.
const WRIST_FROM_GRIP: Vec3 = Vec3::new(0.0, 0.025, -0.06);

/// `ride.poses` poses of `model` riding a bike scaled to its legs, from its
/// standing pose `standing`; and that scale.
///
/// No capture of anybody cycling is in the pack, and none is needed: a rider
/// is the standing person put on the saddle, the trunk leaned to the bars, and
/// each limb solved onto what it holds — the feet onto the pedals through a
/// turn of the cranks, the hands onto the grips — with the same two-bone
/// reach the phone in the hand uses (`reach`). The bike is scaled so the leg
/// is [`LEG_AT_BOTTOM`] straight at the bottom of the stroke, as a saddle set
/// to the rider leaves it.
fn ride_poses(model: &HumanModel, ride: &Ride, standing: &Pose) -> (Vec<Pose>, f32) {
    let n = ride.poses.max(1);
    let names = [
        "pelvis",
        "thigh_l",
        "calf_l",
        "foot_l",
        "thigh_r",
        "calf_r",
        "foot_r",
        "ball_r",
        "spine_01",
        "spine_02",
        "spine_03",
        "neck_01",
        "upperarm_l",
        "lowerarm_l",
        "hand_l",
        "upperarm_r",
        "lowerarm_r",
        "hand_r",
    ];
    let Some(j) = names
        .iter()
        .map(|n| model.joint(n))
        .collect::<Option<Vec<usize>>>()
    else {
        // A rig without the bones to ride with: it stands, on every pose.
        return ((0..n).map(|_| standing.clone()).collect(), 1.0);
    };
    let [pelvis, thigh_l, calf_l, foot_l, thigh_r, calf_r, foot_r, ball_r, spine_1, spine_2, spine_3, neck, upper_l, lower_l, hand_l, upper_r, lower_r, hand_r] =
        j[..]
    else {
        unreachable!("one index per name")
    };

    let base = fk(model, standing);
    let at = |i: usize| base[i].0;
    let leg = at(thigh_r).distance(at(calf_r)) + at(calf_r).distance(at(foot_r));
    let arm = at(upper_r).distance(at(lower_r)) + at(lower_r).distance(at(hand_r));
    let hip_mid = (at(thigh_l) + at(thigh_r)) * 0.5;
    let hip_lift = HIP_OVER_SADDLE_M * leg / REFERENCE_LEG_M;
    // Where the hips go on a bike at scale `s`, and how far each moves to get
    // there.
    let seat = |s: f32| ride.saddle * s + Vec3::new(0.0, hip_lift, 0.02 * s);
    // The foot as it stands: the ankle relative to the ball of the foot, and
    // the ball's height over the sole.
    let ankle_from_ball = at(foot_r) - at(ball_r);
    let sole = at(ball_r).y.max(0.0);
    let ankle_on = |pedal: Vec3, turn: f32| {
        pedal + Vec3::Y * sole + Quat::from_rotation_x(ankling(turn)) * ankle_from_ball
    };
    let pedal = |s: f32, turn: f32, side: f32| {
        ride.bottom_bracket * s
            + Vec3::X * (side * ride.pedal_x * s)
            + pedal_at(turn, ride.crank * s)
    };

    // The scale: the right leg's reach at the bottom of the stroke, solved by
    // halving — it only grows with the bike.
    let bottom = std::f32::consts::PI;
    let reach_at = |s: f32| {
        let hip = at(thigh_r) + (seat(s) - hip_mid);
        hip.distance(ankle_on(pedal(s, bottom, -1.0), bottom))
    };
    let (mut lo, mut hi) = (0.4f32, 1.6f32);
    for _ in 0..30 {
        let mid = 0.5 * (lo + hi);
        if reach_at(mid) > LEG_AT_BOTTOM * leg {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    let s = 0.5 * (lo + hi);

    // On the saddle.
    let mut seated = standing.clone();
    let pelvis_to = base[pelvis].0 + (seat(s) - hip_mid);
    let local = match model.joints[pelvis].parent {
        Some(p) => base[p].1.inverse() * (pelvis_to - base[p].0),
        None => pelvis_to,
    };
    seated.pelvis = Some((pelvis, local));

    // The lean that brings the hands to the bars with the elbows soft. The
    // right side stands for both; the bike is symmetric and so near enough is
    // the body.
    let wrist_at =
        |side: f32| Vec3::new(side * ride.grip.x, ride.grip.y, ride.grip.z) * s + WRIST_FROM_GRIP;
    let lean = |pose: &mut Pose, lean: f32| {
        for (joint, share) in [(spine_1, 0.5), (spine_2, 0.25), (spine_3, 0.25)] {
            turn_world(model, pose, joint, Quat::from_rotation_x(lean * share));
        }
        // The head comes most of the way back up: a rider looks down the road.
        turn_world(model, pose, neck, Quat::from_rotation_x(-lean * 0.7));
    };
    let best = (0..=45)
        .map(|deg| (deg as f32).to_radians())
        .min_by(|a, b| {
            let miss = |l: f32| {
                let mut p = seated.clone();
                lean(&mut p, l);
                let w = fk(model, &p);
                (w[upper_r].0.distance(wrist_at(-1.0)) - ARM_ON_BARS * arm).abs()
            };
            miss(*a).total_cmp(&miss(*b))
        })
        .unwrap_or(0.0);
    lean(&mut seated, best);

    let poses = (0..n)
        .map(|k| {
            let turn = k as f32 / n as f32 * std::f32::consts::TAU;
            let mut pose = seated.clone();
            // Each foot onto its pedal: the right at `turn`, the left half a
            // turn on, knees forward and a little out.
            for (side, thigh, calf, foot, turn) in [
                (1.0f32, thigh_r, calf_r, foot_r, turn),
                (-1.0, thigh_l, calf_l, foot_l, turn + std::f32::consts::PI),
            ] {
                // The rig's `_r` is the body's right, at `-x` (MakeHuman faces
                // `+z` with its left at `+x`).
                let x = -side;
                let w = fk(model, &pose);
                let target = ankle_on(pedal(s, turn, x), turn);
                let r = crate::reach::reach(
                    &crate::reach::Arm {
                        shoulder: w[thigh].0,
                        elbow: w[calf].0,
                        wrist: w[foot].0,
                    },
                    target,
                    Vec3::new(x * 0.15, 0.2, 1.0),
                );
                turn_world(model, &mut pose, thigh, r.upper);
                turn_world(model, &mut pose, calf, r.lower);
                // The foot keeps its standing turn, pitched by the ankling.
                let w = fk(model, &pose);
                let want = Quat::from_rotation_x(ankling(turn)) * base[foot].1;
                turn_world(model, &mut pose, foot, want * w[foot].1.inverse());
            }
            // The hands onto the grips, elbows out and down.
            for (side, upper, lower) in [(1.0f32, upper_r, lower_r), (-1.0, upper_l, lower_l)] {
                let x = -side;
                let w = fk(model, &pose);
                let r = crate::reach::reach(
                    &crate::reach::Arm {
                        shoulder: w[upper].0,
                        elbow: w[lower].0,
                        wrist: w[if side > 0.0 { hand_r } else { hand_l }].0,
                    },
                    wrist_at(x),
                    Vec3::new(x, -0.6, -0.3),
                );
                turn_world(model, &mut pose, upper, r.upper);
                turn_world(model, &mut pose, lower, r.lower);
            }
            pose
        })
        .collect();
    (poses, s)
}

/// Every joint's position and rotation relative to the model in `pose`.
fn fk(model: &HumanModel, pose: &Pose) -> Vec<(Vec3, Quat)> {
    let mut world: Vec<(Vec3, Quat)> = Vec::with_capacity(model.joints.len());
    for (i, j) in model.joints.iter().enumerate() {
        let t = match pose.pelvis {
            Some((p, at)) if p == i => at,
            _ => Vec3::from_array(j.translation),
        };
        let r = pose.rotations[i];
        world.push(match j.parent {
            Some(p) => {
                let (pt, pr) = world[p];
                (pt + pr * t, (pr * r).normalize())
            }
            None => (t, r),
        });
    }
    world
}

/// Turn `joint` by `turn` in the model's frame, about the joint itself: its
/// children go with it.
fn turn_world(model: &HumanModel, pose: &mut Pose, joint: usize, turn: Quat) {
    let w = fk(model, pose);
    let parent = model.joints[joint]
        .parent
        .map_or(Quat::IDENTITY, |p| w[p].1);
    pose.rotations[joint] = (parent.inverse() * turn * w[joint].1).normalize();
}

/// Every joint's local rotation, and the pelvis's translation, for one pose.
#[derive(Clone)]
struct Pose {
    rotations: Vec<Quat>,
    pelvis: Option<(usize, Vec3)>,
}

/// `walk_poses` evenly over the walk cycle, then the idle's first frame (the
/// rest pose when the pack has neither).
fn poses(model: &HumanModel, walk_poses: usize) -> Vec<Pose> {
    let rest = || Pose {
        rotations: model
            .joints
            .iter()
            .map(|j| Quat::from_array(j.rotation))
            .collect(),
        pelvis: None,
    };
    let at = |clip: &crate::RetargetedClip, t: f32| {
        let frames = clip.rotations.first().map_or(0, Vec::len);
        if frames == 0 {
            return rest();
        }
        let f = (t * clip.fps).clamp(0.0, (frames - 1) as f32);
        let (a, k) = (f.floor() as usize, f.fract());
        let b = (a + 1).min(frames - 1);
        Pose {
            rotations: model
                .joints
                .iter()
                .enumerate()
                .map(|(i, j)| match clip.rotations.get(i) {
                    Some(r) if r.len() == frames => {
                        Quat::from_array(r[a]).slerp(Quat::from_array(r[b]), k)
                    }
                    _ => Quat::from_array(j.rotation),
                })
                .collect(),
            pelvis: clip
                .pelvis
                .as_ref()
                .map(|(p, at)| (*p, Vec3::from_array(at[a]).lerp(Vec3::from_array(at[b]), k))),
        }
    };
    let find = |name: &str| model.clips.iter().find(|c| c.name == name);
    let mut out: Vec<Pose> = match find("walk") {
        // A loop's last frame is its first again, so the cycle is sampled
        // over `[0, duration)`.
        Some(walk) => (0..walk_poses)
            .map(|i| at(walk, walk.duration() * i as f32 / walk_poses as f32))
            .collect(),
        None => (0..walk_poses).map(|_| rest()).collect(),
    };
    out.push(find("idle").map_or_else(rest, |idle| at(idle, 0.0)));
    out
}

/// Per joint, the matrix taking a rest-pose vertex to `pose`.
fn skin_matrices(model: &HumanModel, pose: &Pose) -> Vec<Mat4> {
    let mut world: Vec<Mat4> = Vec::with_capacity(model.joints.len());
    for (i, j) in model.joints.iter().enumerate() {
        let t = match pose.pelvis {
            Some((p, at)) if p == i => at,
            _ => Vec3::from_array(j.translation),
        };
        let local = Mat4::from_rotation_translation(pose.rotations[i], t);
        world.push(j.parent.map_or(local, |p| world[p] * local));
    }
    world
        .iter()
        .zip(&model.joints)
        .map(|(w, j)| *w * Mat4::from_cols_array(&j.inverse_bind))
        .collect()
}

fn smooth_normals(at: &[Vec3], indices: &[u32]) -> Vec<[f32; 3]> {
    let mut sum = vec![Vec3::ZERO; at.len()];
    for t in indices.chunks_exact(3) {
        let [a, b, c] = [t[0], t[1], t[2]].map(|i| i as usize);
        let n = (at[b] - at[a]).cross(at[c] - at[a]);
        sum[a] += n;
        sum[b] += n;
        sum[c] += n;
    }
    sum.iter()
        .map(|n| n.try_normalize().unwrap_or(Vec3::Y).to_array())
        .collect()
}

/// The drawn parts as one mesh, each part's UV-seam copies of a vertex made
/// one again (the body's two parts, `body` and `face`, share positions and
/// are welded together); their colours averaged.
struct Welded {
    positions: Vec<DVec3>,
    joints: Vec<[u16; 4]>,
    weights: Vec<[f32; 4]>,
    /// Per vertex, its colour summed over its copies, and their count.
    colour: Vec<([f64; 4], f64)>,
    triangles: Vec<[u32; 3]>,
}

/// A cut-out face is kept when its corners are on average at least this
/// opaque: what the material's `Mask(0.5)` would have drawn.
const CUTOUT: f32 = 0.5;

fn weld(model: &HumanModel, paint: &[PartPaint]) -> Welded {
    let mut out = Welded {
        positions: Vec::new(),
        joints: Vec::new(),
        weights: Vec::new(),
        colour: Vec::new(),
        triangles: Vec::new(),
    };
    let mut index: HashMap<(usize, [u32; 3]), u32> = HashMap::new();
    for (i, (part, paint)) in model.parts.iter().zip(paint).enumerate() {
        if !drawn(part.kind) {
            continue;
        }
        let group = if part.kind == PartKind::Body {
            0
        } else {
            i + 1
        };
        let colours: Vec<[f32; 4]> = part
            .uvs
            .iter()
            .map(|&uv| {
                let t = paint.swatch.map_or([1.0; 4], |s| s.sample(uv));
                [
                    t[0] * paint.tint[0],
                    t[1] * paint.tint[1],
                    t[2] * paint.tint[2],
                    t[3],
                ]
            })
            .collect();
        let map: Vec<u32> = part
            .positions
            .iter()
            .enumerate()
            .map(|(v, p)| {
                *index
                    .entry((group, p.map(f32::to_bits)))
                    .or_insert_with(|| {
                        out.positions.push(Vec3::from_array(*p).as_dvec3());
                        out.joints.push(part.joints[v]);
                        out.weights.push(part.weights[v]);
                        out.colour.push(([0.0; 4], 0.0));
                        (out.positions.len() - 1) as u32
                    })
            })
            .collect();
        for t in part.indices.chunks_exact(3) {
            let corners = [t[0], t[1], t[2]].map(|v| v as usize);
            if part.alpha {
                let opacity: f32 = corners.iter().map(|&v| colours[v][3]).sum::<f32>() / 3.0;
                if opacity < CUTOUT {
                    continue;
                }
            }
            let tri = corners.map(|v| map[v]);
            if tri[0] == tri[1] || tri[1] == tri[2] || tri[0] == tri[2] {
                continue;
            }
            out.triangles.push(tri);
        }
        for (v, c) in colours.iter().enumerate() {
            let (sum, n) = &mut out.colour[map[v] as usize];
            for k in 0..3 {
                sum[k] += f64::from(c[k]);
            }
            sum[3] += f64::from(paint.region.alpha());
            *n += 1.0;
        }
    }
    out
}

/// A symmetric 4×4 error quadric, as its ten distinct terms.
#[derive(Clone, Copy, Debug, Default)]
struct Quadric([f64; 10]);

impl Quadric {
    /// The squared distance to the plane `n·x + d = 0`, times `weight`.
    fn plane(n: DVec3, d: f64, weight: f64) -> Self {
        let (a, b, c) = (n.x, n.y, n.z);
        Self(
            [
                a * a,
                a * b,
                a * c,
                a * d,
                b * b,
                b * c,
                b * d,
                c * c,
                c * d,
                d * d,
            ]
            .map(|q| q * weight),
        )
    }

    fn add(&mut self, o: &Self) {
        for (a, b) in self.0.iter_mut().zip(o.0) {
            *a += b;
        }
    }

    fn error(&self, p: DVec3) -> f64 {
        let q = &self.0;
        let (x, y, z) = (p.x, p.y, p.z);
        q[0] * x * x
            + 2.0 * q[1] * x * y
            + 2.0 * q[2] * x * z
            + 2.0 * q[3] * x
            + q[4] * y * y
            + 2.0 * q[5] * y * z
            + 2.0 * q[6] * y
            + q[7] * z * z
            + 2.0 * q[8] * z
            + q[9]
    }
}

/// How much more a boundary edge resists moving than the surface: a hem, or
/// the edge of the skin a garment hides, keeps its line.
const BOUNDARY_WEIGHT: f64 = 100.0;

/// A collapse is refused if it turns a face more than this far
/// (the cosine between its normal before and after).
const MIN_TURN_COS: f64 = 0.2;

/// One candidate collapse, `from` into `into`, as of both vertices'
/// versions; ordered cheapest first.
#[derive(Clone, Copy, Debug)]
struct Candidate {
    cost: f64,
    from: u32,
    into: u32,
    versions: (u32, u32),
}

impl PartialEq for Candidate {
    fn eq(&self, o: &Self) -> bool {
        self.cmp(o) == Ordering::Equal
    }
}
impl Eq for Candidate {}
impl PartialOrd for Candidate {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}
impl Ord for Candidate {
    fn cmp(&self, o: &Self) -> Ordering {
        // A max-heap, so the cheapest compares greatest.
        o.cost
            .total_cmp(&self.cost)
            .then_with(|| o.from.cmp(&self.from))
            .then_with(|| o.into.cmp(&self.into))
    }
}

struct Simplifier {
    positions: Vec<DVec3>,
    triangles: Vec<[u32; 3]>,
    live_triangles: usize,
    face_alive: Vec<bool>,
    /// Per vertex, the faces that use it (dead ones dropped lazily).
    faces_of: Vec<Vec<u32>>,
    alive: Vec<bool>,
    version: Vec<u32>,
    quadric: Vec<Quadric>,
    /// On a boundary edge: such a vertex only slides along the boundary.
    boundary: Vec<bool>,
    /// Edges with exactly one face, as (low, high).
    boundary_edges: std::collections::HashSet<(u32, u32)>,
    colour: Vec<([f64; 4], f64)>,
    heap: BinaryHeap<Candidate>,
}

fn edge(a: u32, b: u32) -> (u32, u32) {
    (a.min(b), a.max(b))
}

impl Simplifier {
    fn new(w: &Welded) -> Self {
        let n = w.positions.len();
        let mut faces_of = vec![Vec::new(); n];
        let mut quadric = vec![Quadric::default(); n];
        let mut edge_faces: HashMap<(u32, u32), u32> = HashMap::new();
        for (f, t) in w.triangles.iter().enumerate() {
            let [a, b, c] = t.map(|v| w.positions[v as usize]);
            let cross = (b - a).cross(c - a);
            let area = cross.length() * 0.5;
            let normal = cross.normalize_or_zero();
            let q = Quadric::plane(normal, -normal.dot(a), area);
            for &v in t {
                faces_of[v as usize].push(f as u32);
                quadric[v as usize].add(&q);
            }
            for k in 0..3 {
                *edge_faces.entry(edge(t[k], t[(k + 1) % 3])).or_default() += 1;
            }
        }
        let mut boundary = vec![false; n];
        let mut boundary_edges = std::collections::HashSet::new();
        for t in &w.triangles {
            let [a, b, c] = t.map(|v| w.positions[v as usize]);
            let normal = (b - a).cross(c - a).normalize_or_zero();
            for k in 0..3 {
                let (p, q) = (t[k], t[(k + 1) % 3]);
                let shared = edge_faces[&edge(p, q)];
                if shared == 1 {
                    // A plane through the edge, square to its face: moving
                    // off the boundary line costs, sliding along it does not.
                    let (pp, pq) = (w.positions[p as usize], w.positions[q as usize]);
                    let along = pq - pp;
                    let side = along.cross(normal).normalize_or_zero();
                    let weight = BOUNDARY_WEIGHT * along.length_squared();
                    let plane = Quadric::plane(side, -side.dot(pp), weight);
                    quadric[p as usize].add(&plane);
                    quadric[q as usize].add(&plane);
                    boundary[p as usize] = true;
                    boundary[q as usize] = true;
                    boundary_edges.insert(edge(p, q));
                } else if shared > 2 {
                    // Not a surface here: leave it as it is.
                    boundary[p as usize] = true;
                    boundary[q as usize] = true;
                }
            }
        }
        let mut s = Self {
            positions: w.positions.clone(),
            triangles: w.triangles.clone(),
            live_triangles: w.triangles.len(),
            face_alive: vec![true; w.triangles.len()],
            faces_of,
            alive: vec![true; n],
            version: vec![0; n],
            quadric,
            boundary,
            boundary_edges,
            colour: w.colour.clone(),
            heap: BinaryHeap::new(),
        };
        // Each surviving vertex's colour is the mean of everything merged
        // into it, weighted by how much surface each stood for.
        for v in 0..n {
            let area: f64 = s.faces_of[v]
                .iter()
                .map(|&f| s.area(f))
                .sum::<f64>()
                .max(1.0e-12);
            let (sum, count) = &mut s.colour[v];
            let k = area / count.max(1.0);
            for c in sum.iter_mut() {
                *c *= k;
            }
            *count = area;
        }
        for t in w.triangles.iter() {
            for k in 0..3 {
                s.push(t[k], t[(k + 1) % 3]);
                s.push(t[(k + 1) % 3], t[k]);
            }
        }
        s
    }

    fn area(&self, f: u32) -> f64 {
        let [a, b, c] = self.triangles[f as usize].map(|v| self.positions[v as usize]);
        (b - a).cross(c - a).length() * 0.5
    }

    fn push(&mut self, from: u32, into: u32) {
        let mut q = self.quadric[from as usize];
        q.add(&self.quadric[into as usize]);
        let cost = q.error(self.positions[into as usize]).max(0.0);
        self.heap.push(Candidate {
            cost,
            from,
            into,
            versions: (self.version[from as usize], self.version[into as usize]),
        });
    }

    fn live_faces(&self, v: u32) -> impl Iterator<Item = u32> + '_ {
        self.faces_of[v as usize]
            .iter()
            .copied()
            .filter(|&f| self.face_alive[f as usize])
    }

    fn neighbours(&self, v: u32) -> Vec<u32> {
        let mut n: Vec<u32> = self
            .live_faces(v)
            .flat_map(|f| self.triangles[f as usize])
            .filter(|&w| w != v)
            .collect();
        n.sort_unstable();
        n.dedup();
        n
    }

    /// Whether merging `from` into `into` keeps the surface a surface, facing
    /// the way it did.
    fn allowed(&self, from: u32, into: u32) -> bool {
        let (f, i) = (from as usize, into as usize);
        if self.boundary[f]
            && !(self.boundary[i] && self.boundary_edges.contains(&edge(from, into)))
        {
            return false;
        }
        let shared: Vec<u32> = self
            .live_faces(from)
            .filter(|&t| self.triangles[t as usize].contains(&into))
            .collect();
        if shared.is_empty() {
            return false;
        }
        // The link condition: the two may only have in common the vertices
        // opposite their shared edge, or the collapse pinches the surface.
        let nf = self.neighbours(from);
        let ni = self.neighbours(into);
        let common = nf.iter().filter(|v| ni.binary_search(v).is_ok()).count();
        if common != shared.len() {
            return false;
        }
        // A vertex left with fewer than three neighbours is a sliver.
        let merged = nf.iter().chain(&ni).filter(|&&v| v != from && v != into);
        let mut merged: Vec<u32> = merged.copied().collect();
        merged.sort_unstable();
        merged.dedup();
        if merged.len() < 3 {
            return false;
        }
        let to = self.positions[i];
        for t in self.live_faces(from) {
            let tri = self.triangles[t as usize];
            if tri.contains(&into) {
                continue;
            }
            let p = tri.map(|v| self.positions[v as usize]);
            let before = (p[1] - p[0]).cross(p[2] - p[0]);
            let q = tri.map(|v| {
                if v == from {
                    to
                } else {
                    self.positions[v as usize]
                }
            });
            let after = (q[1] - q[0]).cross(q[2] - q[0]);
            let (lb, la) = (before.length(), after.length());
            if la <= 1.0e-12 || before.dot(after) < MIN_TURN_COS * lb * la {
                return false;
            }
        }
        true
    }

    fn collapse(&mut self, from: u32, into: u32) {
        let faces: Vec<u32> = self.live_faces(from).collect();
        for t in faces {
            let tri = &mut self.triangles[t as usize];
            if tri.contains(&into) {
                self.face_alive[t as usize] = false;
                self.live_triangles -= 1;
            } else {
                for v in tri.iter_mut() {
                    if *v == from {
                        *v = into;
                    }
                }
                self.faces_of[into as usize].push(t);
            }
        }
        self.faces_of[from as usize].clear();
        self.alive[from as usize] = false;
        let q = self.quadric[from as usize];
        self.quadric[into as usize].add(&q);
        let (sum, n) = self.colour[from as usize];
        let (into_sum, into_n) = &mut self.colour[into as usize];
        for k in 0..4 {
            into_sum[k] += sum[k];
        }
        *into_n += n;
        // The boundary edges `from` had now run to `into`.
        let moved: Vec<(u32, u32)> = self
            .boundary_edges
            .iter()
            .filter(|&&(a, b)| a == from || b == from)
            .copied()
            .collect();
        for (a, b) in moved {
            self.boundary_edges.remove(&(a, b));
            let other = if a == from { b } else { a };
            if other != into {
                self.boundary_edges.insert(edge(other, into));
            }
        }
        self.version[into as usize] += 1;
        let live: Vec<u32> = self.live_faces(into).collect();
        self.faces_of[into as usize] = live;
        for w in self.neighbours(into) {
            self.push(into, w);
            self.push(w, into);
        }
    }

    fn reduce_to(&mut self, budget: usize) {
        while self.live_triangles > budget {
            let Some(c) = self.heap.pop() else { break };
            let (f, i) = (c.from as usize, c.into as usize);
            if !self.alive[f] || !self.alive[i] || c.versions != (self.version[f], self.version[i])
            {
                continue;
            }
            if self.allowed(c.from, c.into) {
                self.collapse(c.from, c.into);
            }
        }
    }

    /// The surviving vertices (indices into the welded mesh), their colours,
    /// and the triangles over them renumbered.
    fn snapshot(&self) -> (Vec<usize>, Vec<[f32; 4]>, Vec<u32>) {
        let mut renumber = vec![u32::MAX; self.positions.len()];
        let mut kept = Vec::new();
        let mut indices = Vec::with_capacity(self.live_triangles * 3);
        for (t, tri) in self.triangles.iter().enumerate() {
            if !self.face_alive[t] {
                continue;
            }
            for &v in tri {
                if renumber[v as usize] == u32::MAX {
                    renumber[v as usize] = kept.len() as u32;
                    kept.push(v as usize);
                }
                indices.push(renumber[v as usize]);
            }
        }
        let colours = kept
            .iter()
            .map(|&v| {
                let (sum, n) = self.colour[v];
                let n = n.max(1.0e-12);
                sum.map(|c| (c / n) as f32)
            })
            .collect();
        (kept, colours, indices)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A closed UV sphere as a welded mesh, one colour.
    fn sphere(rings: usize, segments: usize) -> Welded {
        let mut positions = vec![DVec3::Y];
        for r in 1..rings {
            let phi = std::f64::consts::PI * r as f64 / rings as f64;
            for s in 0..segments {
                let theta = std::f64::consts::TAU * s as f64 / segments as f64;
                positions.push(DVec3::new(
                    phi.sin() * theta.cos(),
                    phi.cos(),
                    phi.sin() * theta.sin(),
                ));
            }
        }
        positions.push(-DVec3::Y);
        let ring = |r: usize, s: usize| (1 + (r - 1) * segments + s % segments) as u32;
        let bottom = (positions.len() - 1) as u32;
        let mut triangles = Vec::new();
        for s in 0..segments {
            triangles.push([0, ring(1, s + 1), ring(1, s)]);
            triangles.push([bottom, ring(rings - 1, s), ring(rings - 1, s + 1)]);
        }
        for r in 1..rings - 1 {
            for s in 0..segments {
                let (a, b, c, d) = (
                    ring(r, s),
                    ring(r, s + 1),
                    ring(r + 1, s),
                    ring(r + 1, s + 1),
                );
                triangles.push([a, b, d]);
                triangles.push([a, d, c]);
            }
        }
        let n = positions.len();
        Welded {
            positions,
            joints: vec![[0; 4]; n],
            weights: vec![[1.0, 0.0, 0.0, 0.0]; n],
            colour: vec![([0.5, 0.25, 0.125, 0.0], 1.0); n],
            triangles,
        }
    }

    /// Every edge of a closed surface has two faces, and each pair of faces
    /// across it winds it in opposite directions.
    fn assert_closed(indices: &[u32]) {
        let mut directed: HashMap<(u32, u32), usize> = HashMap::new();
        for t in indices.chunks_exact(3) {
            for k in 0..3 {
                *directed.entry((t[k], t[(k + 1) % 3])).or_default() += 1;
            }
        }
        for (&(a, b), &n) in &directed {
            assert_eq!(n, 1, "edge {a}-{b} is wound {n} times one way");
            assert_eq!(directed.get(&(b, a)), Some(&1), "edge {a}-{b} has one face");
        }
    }

    #[test]
    fn a_sphere_cut_down_stays_closed_round_and_facing_out() {
        let w = sphere(24, 48);
        let before = w.triangles.len();
        let mut s = Simplifier::new(&w);
        s.reduce_to(200);
        let (kept, colours, indices) = s.snapshot();
        assert!(before > 2000);
        assert!(indices.len() / 3 <= 200, "{} triangles", indices.len() / 3);
        assert!(indices.len() / 3 > 150, "{} triangles", indices.len() / 3);
        assert_closed(&indices);
        for &v in &kept {
            assert!(
                (w.positions[v].length() - 1.0).abs() < 1.0e-9,
                "kept vertices are the sphere's own"
            );
        }
        for t in indices.chunks_exact(3) {
            let [a, b, c] = [t[0], t[1], t[2]].map(|i| w.positions[kept[i as usize]]);
            let n = (b - a).cross(c - a);
            assert!(n.dot((a + b + c) / 3.0) > 0.0, "a face turned inward");
        }
        for c in colours {
            assert!(
                (c[0] - 0.5).abs() < 1.0e-5 && (c[2] - 0.125).abs() < 1.0e-5,
                "{c:?}"
            );
        }
    }

    /// An open sheet's rim is its boundary: cut down, the rim's corners and
    /// its straight sides stay where they were.
    #[test]
    fn a_boundary_keeps_its_line() {
        let n = 21;
        let mut positions = Vec::new();
        for z in 0..n {
            for x in 0..n {
                let (fx, fz) = (x as f64 / (n - 1) as f64, z as f64 / (n - 1) as f64);
                positions.push(DVec3::new(fx, 0.05 * (fx * 6.0).sin(), fz));
            }
        }
        let at = |x: usize, z: usize| (z * n + x) as u32;
        let mut triangles = Vec::new();
        for z in 0..n - 1 {
            for x in 0..n - 1 {
                triangles.push([at(x, z), at(x, z + 1), at(x + 1, z + 1)]);
                triangles.push([at(x, z), at(x + 1, z + 1), at(x + 1, z)]);
            }
        }
        let v = positions.len();
        let w = Welded {
            positions,
            joints: vec![[0; 4]; v],
            weights: vec![[1.0, 0.0, 0.0, 0.0]; v],
            colour: vec![([1.0; 4], 1.0); v],
            triangles,
        };
        let mut s = Simplifier::new(&w);
        s.reduce_to(120);
        let (kept, _, indices) = s.snapshot();
        assert!(indices.len() / 3 <= 120);
        for corner in [at(0, 0), at(n - 1, 0), at(0, n - 1), at(n - 1, n - 1)] {
            assert!(
                kept.contains(&(corner as usize)),
                "corner {corner} collapsed"
            );
        }
        // Every kept vertex on the rim's z = 0 side is still on it.
        for &k in &kept {
            let p = w.positions[k];
            assert!((0.0..=1.0).contains(&p.x) && (0.0..=1.0).contains(&p.z));
        }
    }

    #[test]
    fn a_swatch_samples_in_linear_light() {
        let s = Swatch {
            width: 2,
            height: 1,
            rgba: vec![[255, 255, 255, 255], [0, 0, 0, 0]],
        };
        let left = s.sample([0.25, 0.5]);
        assert!((left[0] - 1.0).abs() < 1.0e-5, "{left:?}");
        let middle = s.sample([0.5, 0.5]);
        assert!((middle[0] - 0.5).abs() < 1.0e-5, "{middle:?}");
        let mid_grey = Swatch {
            width: 1,
            height: 1,
            rgba: vec![[188, 188, 188, 255]],
        };
        assert!((mid_grey.sample([0.3, 0.3])[0] - 0.5).abs() < 0.01);
    }

    fn shipped_pack() -> Option<crate::HumanPack> {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../assets/models/avatar/human/default.humanpack"
        );
        let bytes = std::fs::read(path).ok().filter(|b| b.len() > 4096)?;
        Some(crate::HumanPack::decode(&bytes).expect("the shipped pack decodes"))
    }

    fn woman(pack: &crate::HumanPack) -> HumanModel {
        crate::build(
            pack,
            &crate::Look {
                phenotype: crate::Phenotype {
                    gender: 0.15,
                    ..crate::Phenotype::default()
                },
                skin: "young_asian_female".into(),
                proxies: [
                    "high-poly",
                    "eyebrow001",
                    "eyelashes01",
                    "ponytail01",
                    "female_sportsuit01",
                    "shoes05",
                ]
                .map(String::from)
                .to_vec(),
                eyes: None,
                modifiers: vec![],
            },
        )
    }

    /// The shipped pack's person cut to the crowd's budgets: each level
    /// within its budget, standing on the ground at her own height in every
    /// pose, and walking (her feet apart mid-stride, together standing).
    #[test]
    fn a_shipped_person_cut_down_still_walks_on_the_ground() {
        let Some(pack) = shipped_pack() else { return };
        let m = woman(&pack);
        let paint: Vec<PartPaint> = m
            .parts
            .iter()
            .map(|_| PartPaint {
                swatch: None,
                tint: [0.5; 3],
                region: Region::Own,
            })
            .collect();
        let started = std::time::Instant::now();
        let body = crowd_body(&m, &paint, &[3000, 600], 16, None);
        let took = started.elapsed();
        let full: usize = m
            .parts
            .iter()
            .filter(|p| drawn(p.kind))
            .map(|p| p.indices.len() / 3)
            .sum();
        eprintln!(
            "crowd body: {full} triangles → {:?} in {took:?}; stride {:.2} m, height {:.2} m",
            body.levels
                .iter()
                .map(|l| (l.triangles(), l.vertices()))
                .collect::<Vec<_>>(),
            body.stride,
            body.height,
        );
        assert_eq!(body.walk_poses, 16);
        assert!((1.2..2.2).contains(&body.stride), "stride {}", body.stride);
        for (level, budget) in body.levels.iter().zip([3000, 600]) {
            assert!(
                level.triangles() <= budget,
                "{} > {budget}",
                level.triangles()
            );
            assert!(
                level.triangles() > budget * 3 / 4,
                "{} of {budget}",
                level.triangles()
            );
            assert_eq!(level.positions.len(), 17);
            for (p, pose) in level.positions.iter().enumerate() {
                let low = pose.iter().map(|v| v[1]).fold(f32::INFINITY, f32::min);
                let high = pose.iter().map(|v| v[1]).fold(f32::NEG_INFINITY, f32::max);
                assert!(low.abs() < 0.08, "pose {p}: lowest point {low:.3} m");
                assert!(
                    (high - body.height).abs() < 0.12,
                    "pose {p}: top {high:.3} of {:.3}",
                    body.height
                );
            }
            // Feet: the spread along z of what is below 15 cm.
            let spread = |pose: &Vec<[f32; 3]>| {
                let z: Vec<f32> = pose.iter().filter(|v| v[1] < 0.15).map(|v| v[2]).collect();
                z.iter().copied().fold(f32::NEG_INFINITY, f32::max)
                    - z.iter().copied().fold(f32::INFINITY, f32::min)
            };
            let striding = (0..16)
                .map(|p| spread(&level.positions[p]))
                .fold(0.0, f32::max);
            let standing = spread(&level.positions[16]);
            assert!(
                striding > standing + 0.2,
                "stride {striding:.2} m, standing {standing:.2} m"
            );
        }
    }

    /// The bike the client gives the riders (`cartopolis_geo::agent_models::BIKE_FIT`).
    fn city_bike() -> Ride {
        Ride {
            saddle: Vec3::new(0.0, 0.98, -0.36),
            bottom_bracket: Vec3::new(0.0, 0.28, -0.08),
            crank: 0.17,
            pedal_x: 0.12,
            grip: Vec3::new(0.27, 1.07, 0.12),
            poses: 8,
        }
    }

    /// A shipped person on the bike: the bike scaled to a plausible size for
    /// her, her hips on the saddle, each ankle on its pedal and each wrist on
    /// its grip in every pose, her feet going round, and nothing of her under
    /// the road. Checked on the joints, which is where the solve aims.
    #[test]
    fn a_shipped_person_rides_with_her_feet_on_the_pedals() {
        let Some(pack) = shipped_pack() else { return };
        let m = woman(&pack);
        let bike = city_bike();
        let walk = poses(&m, 16);
        let (riding, s) = ride_poses(&m, &bike, &walk[16]);
        eprintln!("bike scale {s:.3} for a {:.2} m rider", m.height);
        assert_eq!(riding.len(), 8);
        assert!((0.7..1.1).contains(&s), "bike scale {s}");
        let j = |n: &str| m.joint(n).expect(n);
        let ankle_from_ball = {
            let w = fk(&m, &walk[16]);
            (
                w[j("foot_r")].0 - w[j("ball_r")].0,
                w[j("ball_r")].0.y.max(0.0),
            )
        };
        let mut heights = Vec::new();
        for (k, pose) in riding.iter().enumerate() {
            let turn = k as f32 / 8.0 * std::f32::consts::TAU;
            let w = fk(&m, pose);
            let hips = (w[j("thigh_l")].0 + w[j("thigh_r")].0) * 0.5;
            assert!(
                (hips.y - bike.saddle.y * s).abs() < 0.2
                    && (hips.z - bike.saddle.z * s).abs() < 0.08,
                "pose {k}: hips at {hips:?}, saddle at {:?}",
                bike.saddle * s
            );
            for (foot, x, t) in [
                ("foot_r", -1.0, turn),
                ("foot_l", 1.0, turn + std::f32::consts::PI),
            ] {
                let pedal = bike.bottom_bracket * s
                    + Vec3::X * (x * bike.pedal_x * s)
                    + pedal_at(t, bike.crank * s);
                let want = pedal
                    + Vec3::Y * ankle_from_ball.1
                    + Quat::from_rotation_x(ankling(t)) * ankle_from_ball.0;
                let got = w[j(foot)].0;
                assert!(
                    got.distance(want) < 0.03,
                    "pose {k}: {foot} at {got:?}, its pedal wants {want:?}"
                );
            }
            heights.push(w[j("foot_r")].0.y);
            for (hand, x) in [("hand_r", -1.0), ("hand_l", 1.0)] {
                let want =
                    Vec3::new(x * bike.grip.x, bike.grip.y, bike.grip.z) * s + WRIST_FROM_GRIP;
                let got = w[j(hand)].0;
                assert!(
                    got.distance(want) < 0.04,
                    "pose {k}: {hand} at {got:?}, the grip wants {want:?}"
                );
            }
        }
        let span = heights.iter().copied().fold(f32::MIN, f32::max)
            - heights.iter().copied().fold(f32::MAX, f32::min);
        assert!(
            span > 1.6 * bike.crank * s,
            "the right foot rises and falls {span:.3} m"
        );

        // The baked body: no pose puts anything under the road, and the
        // riding poses lift the feet off it.
        let paint: Vec<PartPaint> = m
            .parts
            .iter()
            .map(|_| PartPaint {
                swatch: None,
                tint: [0.5; 3],
                region: Region::Own,
            })
            .collect();
        let body = crowd_body(&m, &paint, &[600], 16, Some(&bike));
        assert_eq!(body.ride_poses, 8);
        assert!((body.bike_scale - s).abs() < 1e-4);
        let level = &body.levels[0];
        assert_eq!(level.positions.len(), 16 + 1 + 8);
        for (p, pose) in level.positions[17..].iter().enumerate() {
            let low = pose.iter().map(|v| v[1]).fold(f32::INFINITY, f32::min);
            assert!(low > 0.02, "riding pose {p}: lowest point {low:.3} m");
        }
    }
}
