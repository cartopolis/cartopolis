//! A pack and a look become a [`HumanModel`]: meshes, a skeleton, clips.
//!
//! Done once per person, when they appear or change how they look — never per
//! frame. The shaping and the fitting are a few passes over ~20 000 vertices;
//! after that the GPU skins the meshes and the CPU only poses 53 bones.

use std::collections::{HashMap, HashSet};

use glam::{Mat4, Vec3};
use serde::{Deserialize, Serialize};

use crate::{fit, macros, rig, Face, HumanPack, Phenotype, Proxy, ProxyKind, BODY_VERTICES};

/// How a person looks: the body's sliders, a skin, and what they wear (proxy
/// names in the pack).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Look {
    pub phenotype: Phenotype,
    pub skin: String,
    pub proxies: Vec<String>,
    /// An eye colour from [`HumanPack::eye_colours`], over the eyes' own.
    #[serde(default)]
    pub eyes: Option<String>,
    /// Face controls (`HumanPack::modifiers`) by name, each -1 … 1.
    #[serde(default)]
    pub modifiers: Vec<(String, f32)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PartKind {
    Body,
    Proxy(ProxyKind),
}

/// One drawable mesh, skinned to [`HumanModel::joints`]. Metres, our frame,
/// UVs with v down (wgpu's convention).
#[derive(Clone, Debug)]
pub struct Part {
    pub kind: PartKind,
    pub name: String,
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub uvs: Vec<[f32; 2]>,
    pub indices: Vec<u32>,
    pub joints: Vec<[u16; 4]>,
    pub weights: Vec<[f32; 4]>,
    pub texture: Option<String>,
    pub alpha: bool,
    /// Which of the proxy's tint regions this is (an outfit's top 0, bottom 1).
    pub region: u8,
    /// The colour that shows a neutral texture as authored; `None` when the
    /// texture is the original and takes no tint.
    pub default_tint: Option<[u8; 3]>,
}

/// One bone, as an engine wants it: rest transform relative to its parent, and
/// the inverse of its rest transform relative to the model.
#[derive(Clone, Debug)]
pub struct Joint {
    pub name: String,
    pub parent: Option<usize>,
    pub translation: [f32; 3],
    pub rotation: [f32; 4],
    pub inverse_bind: [f32; 16],
}

/// A library clip played on this body: per joint, its local rotation at each
/// frame; and the pelvis's local translation.
#[derive(Clone, Debug)]
pub struct RetargetedClip {
    pub name: String,
    pub fps: f32,
    /// Ground speed the clip was captured at, metres a second on this body; 0
    /// for a clip played on the spot.
    pub speed: f32,
    pub looped: bool,
    pub rotations: Vec<Vec<[f32; 4]>>,
    pub pelvis: Option<(usize, Vec<[f32; 3]>)>,
}

impl RetargetedClip {
    /// Seconds from the first sample to the last.
    #[must_use]
    pub fn duration(&self) -> f32 {
        let frames = self.rotations.first().map_or(0, Vec::len);
        frames.saturating_sub(1) as f32 / self.fps
    }
}

#[derive(Clone, Debug)]
pub struct HumanModel {
    pub parts: Vec<Part>,
    pub joints: Vec<Joint>,
    pub clips: Vec<RetargetedClip>,
    /// Top of the head above the soles, metres.
    pub height: f32,
    /// Targets the phenotype asked for that the pack does not carry — a pack
    /// cut down to a few bodies, asked for another.
    pub missing_targets: Vec<String>,
}

/// Build the person `look` describes.
#[must_use]
pub fn build(pack: &HumanPack, look: &Look) -> HumanModel {
    // Shape, in the OBJ frame the targets and the fitting rules are written in.
    let mut base: Vec<Vec3> = pack.vertices.iter().map(|&v| Vec3::from_array(v)).collect();
    let mut missing_targets = Vec::new();
    let by_name: HashMap<&str, &crate::Target> =
        pack.targets.iter().map(|t| (t.name.as_str(), t)).collect();
    // The face controls on top of the body: each picks its negative or its
    // positive targets by sign, at the slider's size.
    let mut wanted = macros::macro_targets(&look.phenotype);
    for (name, v) in &look.modifiers {
        let Some(m) = pack.modifiers.iter().find(|m| &m.name == name) else {
            missing_targets.push(name.clone());
            continue;
        };
        let side = if *v < 0.0 { &m.negative } else { &m.positive };
        let w = v.abs().min(1.0);
        if w > 0.0 {
            wanted.extend(side.iter().map(|t| (t.clone(), w)));
        }
    }
    for (name, w) in wanted {
        match by_name.get(name.as_str()) {
            Some(t) => {
                for (i, d) in t.offsets() {
                    base[i] += w * d;
                }
            }
            None => missing_targets.push(name),
        }
    }
    let proxies: Vec<&Proxy> = look
        .proxies
        .iter()
        .filter_map(|n| pack.proxies.iter().find(|p| &p.name == n))
        .collect();
    let fitted: Vec<Vec<Vec3>> = proxies.iter().map(|p| fit::fit(p, &base)).collect();

    // Metres, standing on y = 0.
    let floor = base[..BODY_VERTICES.min(base.len())]
        .iter()
        .map(|v| v.y)
        .fold(f32::INFINITY, f32::min);
    let to_metres = |v: Vec3| (v - Vec3::Y * floor) * 0.1;
    let positions: Vec<Vec3> = base.iter().map(|&v| to_metres(v)).collect();
    let height = positions[..BODY_VERTICES.min(positions.len())]
        .iter()
        .map(|v| v.y)
        .fold(0.0, f32::max);

    // The skeleton, placed in this body.
    let rest = rig::rest_pose(pack, &positions);
    let tpose = rig::tpose_world(pack, &rest);
    let joints: Vec<Joint> = pack
        .bones
        .iter()
        .enumerate()
        .map(|(i, b)| {
            let parent = b.parent.map(usize::from);
            let (translation, rotation) = match parent {
                Some(p) => (
                    rest[p].rotation.inverse() * (rest[i].head - rest[p].head),
                    (rest[p].rotation.inverse() * rest[i].rotation).normalize(),
                ),
                None => (rest[i].head, rest[i].rotation),
            };
            let bind = Mat4::from_rotation_translation(rest[i].rotation, rest[i].head);
            Joint {
                name: b.name.clone(),
                parent,
                translation: translation.to_array(),
                rotation: rotation.to_array(),
                inverse_bind: bind.inverse().to_cols_array(),
            }
        })
        .collect();

    // The skin, less what the clothes cover.
    let covered: HashSet<u32> = proxies
        .iter()
        .flat_map(|p| p.delete_verts.iter().copied())
        .collect();
    let body_faces: Vec<Face> = pack
        .body_faces
        .iter()
        .filter(|f| !corners(f).all(|v| covered.contains(&v)))
        .copied()
        .collect();
    let skin = pack.skins.iter().find(|s| s.name == look.skin);
    let mut parts = vec![mesh_part(
        PartKind::Body,
        "body",
        &positions,
        &pack.uvs,
        &body_faces,
        |v| pack.weights[v].clone(),
        skin.map(|s| s.texture.clone()),
        false,
    )];
    let eye_texture = look
        .eyes
        .as_ref()
        .and_then(|e| pack.eye_colours.iter().find(|c| &c.name == e))
        .map(|c| c.texture.clone());
    for (proxy, fitted) in proxies.iter().zip(&fitted) {
        let at: Vec<Vec3> = fitted.iter().map(|&v| to_metres(v)).collect();
        let texture = match proxy.kind {
            ProxyKind::Eyes => eye_texture.clone().or_else(|| proxy.texture.clone()),
            _ => proxy.texture.clone(),
        };
        // One mesh per tint region, so each takes its own colour.
        let regions = proxy.region_colours.len().max(1) as u8;
        for region in 0..regions {
            let faces: Vec<Face> = if proxy.face_regions.is_empty() {
                proxy.faces.clone()
            } else {
                proxy
                    .faces
                    .iter()
                    .zip(&proxy.face_regions)
                    .filter(|(_, r)| **r == region)
                    .map(|(f, _)| *f)
                    .collect()
            };
            if faces.is_empty() {
                continue;
            }
            let mut part = mesh_part(
                PartKind::Proxy(proxy.kind),
                &proxy.name,
                &at,
                &proxy.uvs,
                &faces,
                |v| fit::blend_weights(&proxy.refs[v], &pack.weights),
                texture.clone(),
                proxy.alpha,
            );
            part.region = region;
            part.default_tint = proxy.region_colours.get(region as usize).copied();
            parts.push(part);
        }
    }

    // The pelvis above the ankles: the unit a clip's hip motion and speed
    // are stored in (`SourceClip::hips`).
    let hip_height = pack
        .bone("pelvis")
        .zip(pack.bone("foot_l"))
        .map_or(1.0, |(p, f)| rest[p].head.y - rest[f].head.y);
    let clips = pack
        .clips
        .iter()
        .map(|c| {
            let r = rig::retarget(pack, &rest, &tpose, c, hip_height);
            RetargetedClip {
                name: c.name.clone(),
                fps: c.fps,
                speed: c.speed * hip_height,
                looped: c.looped,
                rotations: r
                    .local
                    .iter()
                    .map(|frames| frames.iter().map(|q| q.to_array()).collect())
                    .collect(),
                pelvis: pack
                    .bone("pelvis")
                    .map(|p| (p, r.pelvis.iter().map(|v| v.to_array()).collect())),
            }
        })
        .collect();

    HumanModel {
        parts,
        joints,
        clips,
        height,
        missing_targets,
    }
}

impl HumanModel {
    pub fn joint(&self, name: &str) -> Option<usize> {
        self.joints.iter().position(|j| j.name == name)
    }

    /// Every joint's position relative to the model at one frame of a clip
    /// (`None`: the rest pose). For checking a clip, not for drawing it.
    #[must_use]
    pub fn joint_positions(&self, clip: Option<(usize, usize)>) -> Vec<Vec3> {
        let mut world: Vec<(Vec3, glam::Quat)> = Vec::with_capacity(self.joints.len());
        for (i, j) in self.joints.iter().enumerate() {
            let mut t = Vec3::from_array(j.translation);
            let mut r = glam::Quat::from_array(j.rotation);
            if let Some((c, f)) = clip {
                let c = &self.clips[c];
                if let Some(q) = c.rotations.get(i).and_then(|r| r.get(f)) {
                    r = glam::Quat::from_array(*q);
                }
                if let Some((p, at)) = &c.pelvis {
                    if *p == i {
                        t = Vec3::from_array(at[f]);
                    }
                }
            }
            world.push(match j.parent {
                Some(p) => {
                    let (pt, pr) = world[p];
                    (pt + pr * t, pr * r)
                }
                None => (t, r),
            });
        }
        world.into_iter().map(|(t, _)| t).collect()
    }
}

fn corners(f: &Face) -> impl Iterator<Item = u32> + '_ {
    f.v.iter().copied().filter(|&v| v != Face::NONE)
}

/// One mesh from indexed faces whose corners carry separate position and UV
/// indices: a vertex per distinct (position, UV) pair, normals smoothed per
/// *position* so a UV seam is not a shading seam.
#[allow(clippy::too_many_arguments)]
fn mesh_part(
    kind: PartKind,
    name: &str,
    positions: &[Vec3],
    uvs: &[[f32; 2]],
    faces: &[Face],
    influences: impl Fn(usize) -> Vec<(u16, f32)>,
    texture: Option<String>,
    alpha: bool,
) -> Part {
    let mut normal_at = vec![Vec3::ZERO; positions.len()];
    for f in faces {
        for t in f.triangles() {
            let [a, b, c] = t.map(|k| positions[f.v[k] as usize]);
            // Area-weighted: the cross product's length is twice the area.
            let n = (b - a).cross(c - a);
            for k in t {
                normal_at[f.v[k] as usize] += n;
            }
        }
    }
    let mut index_of: HashMap<(u32, u32), u32> = HashMap::new();
    let mut part = Part {
        kind,
        name: name.to_string(),
        positions: Vec::new(),
        normals: Vec::new(),
        uvs: Vec::new(),
        indices: Vec::new(),
        joints: Vec::new(),
        weights: Vec::new(),
        texture,
        alpha,
        region: 0,
        default_tint: None,
    };
    for f in faces {
        for t in f.triangles() {
            for k in t {
                let key = (f.v[k], f.uv[k]);
                let ix = *index_of.entry(key).or_insert_with(|| {
                    let v = f.v[k] as usize;
                    let uv = uvs.get(f.uv[k] as usize).copied().unwrap_or_default();
                    let (j, w) = fit::top_four(influences(v));
                    part.positions.push(positions[v].to_array());
                    part.normals
                        .push(normal_at[v].normalize_or(Vec3::Y).to_array());
                    part.uvs.push([uv[0], 1.0 - uv[1]]);
                    part.joints.push(j);
                    part.weights.push(w);
                    (part.positions.len() - 1) as u32
                });
                part.indices.push(ix);
            }
        }
    }
    part
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A quad whose corners share positions but not UVs splits into separate
    /// vertices, and both triangles face +Y.
    #[test]
    fn a_uv_seam_splits_vertices_but_not_normals() {
        let positions = vec![
            Vec3::ZERO,
            Vec3::new(0.0, 0.0, 1.0),
            Vec3::new(1.0, 0.0, 1.0),
            Vec3::new(1.0, 0.0, 0.0),
        ];
        let uvs = vec![[0.0, 0.0], [0.0, 1.0], [1.0, 1.0], [1.0, 0.0], [0.5, 0.5]];
        let faces = vec![
            Face {
                v: [0, 1, 2, 3],
                uv: [0, 1, 2, 3],
            },
            Face {
                v: [0, 1, 2, Face::NONE],
                uv: [4, 1, 2, Face::NONE],
            },
        ];
        let part = mesh_part(
            PartKind::Body,
            "t",
            &positions,
            &uvs,
            &faces,
            |_| vec![(0, 1.0)],
            None,
            false,
        );
        assert_eq!(part.indices.len(), 9);
        assert_eq!(part.positions.len(), 5, "corner 0 twice, for its two UVs");
        for n in &part.normals {
            assert!((Vec3::from_array(*n) - Vec3::Y).length() < 1.0e-5, "{n:?}");
        }
        assert_eq!(part.uvs[1], [0.0, 0.0], "v flips to wgpu's convention");
    }

    /// The pack the client ships, or `None` in a checkout without Git LFS
    /// (where the file is a 130-byte pointer).
    fn shipped_pack() -> Option<HumanPack> {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../assets/models/avatar/human/default.humanpack"
        );
        let bytes = std::fs::read(path).ok().filter(|b| b.len() > 4096)?;
        Some(HumanPack::decode(&bytes).expect("the shipped pack decodes"))
    }

    fn man(pack: &HumanPack) -> HumanModel {
        build(
            pack,
            &Look {
                phenotype: Phenotype::male(),
                skin: "young_caucasian_male".into(),
                proxies: [
                    "high-poly",
                    "eyebrow001",
                    "eyelashes01",
                    "short02",
                    "male_casualsuit01",
                    "shoes06",
                ]
                .map(String::from)
                .to_vec(),
                eyes: None,
                modifiers: vec![],
            },
        )
    }

    /// A man of a man's height, standing on the ground, his skeleton inside
    /// him: the head above the hips above the knees above the toes.
    #[test]
    fn the_shipped_pack_makes_a_man_who_stands_up() {
        let Some(pack) = shipped_pack() else { return };
        let m = man(&pack);
        assert!(m.missing_targets.is_empty(), "{:?}", m.missing_targets);
        assert!((1.6..1.95).contains(&m.height), "height {}", m.height);
        // The outfit is two meshes, its top and its bottom, tinted apart.
        assert_eq!(
            m.parts.len(),
            8,
            "the body, five proxies and the outfit's two halves"
        );
        let rest = m.joint_positions(None);
        let y = |n: &str| rest[m.joint(n).unwrap()].y;
        assert!(y("head") > y("pelvis") && y("pelvis") > y("calf_l") && y("calf_l") > y("ball_l"));
        assert!(
            y("ball_l") < 0.12,
            "toes on the ground, got {}",
            y("ball_l")
        );
        // Left is +x for a body facing +z.
        assert!(rest[m.joint("hand_l").unwrap()].x > 0.2);
    }

    /// A face control moves the face, and only it: a wider nose changes the
    /// skin by millimetres around the nose and leaves the feet where they were.
    #[test]
    fn a_face_control_moves_the_face() {
        let Some(pack) = shipped_pack() else { return };
        let plain = man(&pack);
        let mut look = Look {
            phenotype: Phenotype::male(),
            skin: "young_caucasian_male".into(),
            proxies: vec![],
            eyes: None,
            modifiers: vec![("nose/nose-scale-horiz-decr-incr".into(), 1.0)],
        };
        let wide = build(&pack, &look);
        assert!(
            wide.missing_targets.is_empty(),
            "{:?}",
            wide.missing_targets
        );
        look.modifiers.clear();
        let base = build(&pack, &look);
        let (a, b) = (&base.parts[0].positions, &wide.parts[0].positions);
        let moved: Vec<f32> = a
            .iter()
            .zip(b)
            .map(|(p, q)| (Vec3::from_array(*p) - Vec3::from_array(*q)).length())
            .collect();
        let most = moved.iter().copied().fold(0.0, f32::max);
        assert!(most > 0.002, "the nose widens by millimetres, got {most}");
        let lowest = |m: &HumanModel| {
            m.parts[0]
                .positions
                .iter()
                .map(|p| p[1])
                .fold(f32::MAX, f32::min)
        };
        assert!(
            (lowest(&base) - lowest(&wide)).abs() < 1.0e-4,
            "the feet stay on the ground"
        );
        let _ = plain;
    }

    /// Retargeting through the T-pose leaves the arms where the library put
    /// them — by the sides when idle, not held out as in either rest pose —
    /// and keeps the feet on the ground through a walk.
    #[test]
    fn the_shipped_clips_play_like_people() {
        let Some(pack) = shipped_pack() else { return };
        let m = man(&pack);
        let clip = |name: &str| m.clips.iter().position(|c| c.name == name).unwrap();
        let idle = m.joint_positions(Some((clip("idle"), 0)));
        let (hand, shoulder) = (
            idle[m.joint("hand_l").unwrap()],
            idle[m.joint("upperarm_l").unwrap()],
        );
        assert!(
            shoulder.y - hand.y > 0.4,
            "idle hands hang below the shoulders: hand {hand}, shoulder {shoulder}"
        );
        assert!(
            hand.x < 0.45,
            "and close to the body, not out to the side: {hand}"
        );

        let walk = clip("walk");
        let frames = m.clips[walk].rotations[0].len();
        let mut lowest = f32::INFINITY;
        let mut pelvis = (f32::INFINITY, f32::NEG_INFINITY);
        for f in 0..frames {
            let p = m.joint_positions(Some((walk, f)));
            lowest = lowest.min(
                p[m.joint("foot_l").unwrap()]
                    .y
                    .min(p[m.joint("foot_r").unwrap()].y),
            );
            let py = p[m.joint("pelvis").unwrap()].y;
            pelvis = (pelvis.0.min(py), pelvis.1.max(py));
        }
        assert!(
            lowest < 0.15 && lowest > -0.05,
            "an ankle reaches the ground: {lowest}"
        );
        assert!(
            pelvis.0 > 0.75 && pelvis.1 < 1.1,
            "the hips stay at hip height: {pelvis:?}"
        );
    }

    /// The punch closes the hand into a fist and the wave opens it: the
    /// fingertips come in toward the thumb's root — the palm side, so a curl
    /// the wrong way round would move them away — and the wave's are further
    /// out than at rest.
    #[test]
    fn a_punch_makes_a_fist_and_a_wave_an_open_hand() {
        let Some(pack) = shipped_pack() else { return };
        let m = man(&pack);
        let clip = |name: &str| m.clips.iter().position(|c| c.name == name).unwrap();
        let reach = |pose: Option<(usize, usize)>| {
            let p = m.joint_positions(pose);
            let palm = p[m.joint("thumb_01_l").unwrap()];
            ["index_03_l", "middle_03_l", "ring_03_l", "pinky_03_l"]
                .iter()
                .map(|n| p[m.joint(n).unwrap()].distance(palm))
                .sum::<f32>()
        };
        let rest = reach(None);
        let fist = reach(Some((clip("punch"), 0)));
        let open = reach(Some((clip("wave"), 0)));
        assert!(fist < rest * 0.8, "a fist: {fist} against {rest} at rest");
        assert!(open > rest, "an open hand: {open} against {rest} at rest");
    }

    /// Standing clips look ahead, not at the pavement (`rig::level_gaze`).
    #[test]
    fn standing_clips_look_ahead() {
        let Some(pack) = shipped_pack() else { return };
        let m = man(&pack);
        let head = m.joint("head").unwrap();
        // The joints' world rotations for a frame of `clip`.
        let world = |c: Option<usize>, f: usize| {
            let mut w = vec![glam::Quat::IDENTITY; m.joints.len()];
            for (i, j) in m.joints.iter().enumerate() {
                let r = c.map_or(j.rotation, |c| m.clips[c].rotations[i][f]);
                let r = glam::Quat::from_array(r);
                w[i] = j.parent.map_or(r, |p| w[p] * r);
            }
            w
        };
        let rest = world(None, 0)[head];
        for name in ["idle", "walk", "wave", "sit"] {
            let c = m.clips.iter().position(|c| c.name == name).unwrap();
            let frames = m.clips[c].rotations[head].len();
            let mean = (0..frames)
                .map(|f| {
                    (world(Some(c), f)[head] * rest.inverse() * glam::Vec3::Z)
                        .y
                        .asin()
                })
                .sum::<f32>()
                / frames as f32;
            assert!(
                (mean.to_degrees() + 6.0).abs() < 1.0,
                "{name} looks {:.1}° from level",
                mean.to_degrees()
            );
        }
    }
}
