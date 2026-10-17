//! Motion capture into [`SourceClip`]s: reading BVH, cutting a seamless loop
//! out of a long take, and taking the travel out so the avatar walks on the
//! spot while the game moves it.
//!
//! Raw capture is not a game clip. A take of someone walking is twenty seconds
//! of them crossing a room, turning at the walls, so a clip is *cut* from it:
//! the stretch of one or two strides whose last pose best matches its first
//! ([`find_loop`]), the small remaining mismatch spread over the whole stretch
//! so the seam cannot be seen, the travel subtracted, and the heading turned to
//! face +Z. What the take travelled is kept as the clip's speed, so the client
//! can play it at the rate that matches the avatar's ground speed.
//!
//! The sources are the 100STYLE dataset (Ian Mason, CC BY 4.0; Xsens, 60 fps)
//! and the CMU Graphics Lab Motion Capture Database (free for all uses; the
//! cgspeed BVH conversion, 120 fps). Both rest in a T-pose, which is what the
//! retarget matches against MakeHuman's own ([`crate::rig::retarget`]).

use std::path::PathBuf;

use glam::{Quat, Vec3};

use crate::{Bone, SourceClip};

pub type Result<T> = std::result::Result<T, String>;

/// A take: a skeleton and its local pose at every frame.
#[derive(Clone, Debug)]
pub struct Motion {
    pub names: Vec<String>,
    pub parents: Vec<Option<usize>>,
    /// Rest local translations (BVH `OFFSET`).
    pub offsets: Vec<Vec3>,
    /// Where a joint's `End Site` is, from the joint: the tip of a hand, a
    /// head or a toe, which gives a bone with no child joint a direction.
    pub ends: Vec<Option<Vec3>>,
    pub fps: f32,
    /// Per frame, per joint: local translation and rotation.
    pub frames: Vec<Vec<(Vec3, Quat)>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ch {
    Xpos,
    Ypos,
    Zpos,
    Xrot,
    Yrot,
    Zrot,
}

/// Parse a BVH file. Rotations compose in the order the channels are listed
/// (the BVH convention); a joint with position channels takes them as its
/// whole local translation, which is how both the root's absolute position and
/// Xsens's per-joint positions are written.
pub fn parse_bvh(text: &str) -> Result<Motion> {
    let mut tokens = text.split_whitespace().peekable();
    let mut names = Vec::new();
    let mut parents: Vec<Option<usize>> = Vec::new();
    let mut offsets = Vec::new();
    let mut ends: Vec<Option<Vec3>> = Vec::new();
    let mut channels: Vec<Vec<Ch>> = Vec::new();
    // `None` marks an End Site's brace, which has no joint of its own.
    let mut stack: Vec<Option<usize>> = Vec::new();
    let num = |s: Option<&str>| -> Result<f32> {
        s.ok_or("bvh: ended early")?
            .parse::<f32>()
            .map_err(|e| format!("bvh: {e}"))
    };
    let mut pending: Option<Option<usize>> = None;
    loop {
        let Some(tok) = tokens.next() else {
            return Err("bvh: no MOTION section".into());
        };
        match tok {
            "HIERARCHY" => {}
            "ROOT" | "JOINT" => {
                let name = tokens.next().ok_or("bvh: joint without a name")?;
                let parent = stack.iter().rev().flatten().next().copied();
                names.push(name.to_string());
                parents.push(parent);
                offsets.push(Vec3::ZERO);
                ends.push(None);
                channels.push(Vec::new());
                pending = Some(Some(names.len() - 1));
            }
            "End" => {
                tokens.next(); // "Site"
                pending = Some(None);
            }
            "{" => stack.push(pending.take().ok_or("bvh: a brace with no joint")?),
            "}" => {
                stack.pop();
            }
            "OFFSET" => {
                let v = Vec3::new(
                    num(tokens.next())?,
                    num(tokens.next())?,
                    num(tokens.next())?,
                );
                match stack.last() {
                    Some(Some(j)) => offsets[*j] = v,
                    // An End Site: its joint is the nearest real one below it.
                    Some(None) => {
                        if let Some(j) = stack.iter().rev().flatten().next() {
                            ends[*j].get_or_insert(v);
                        }
                    }
                    None => {}
                }
            }
            "CHANNELS" => {
                let n = num(tokens.next())? as usize;
                let j = stack
                    .last()
                    .copied()
                    .flatten()
                    .ok_or("bvh: channels outside a joint")?;
                for _ in 0..n {
                    let c = match tokens.next().ok_or("bvh: ended in CHANNELS")? {
                        "Xposition" => Ch::Xpos,
                        "Yposition" => Ch::Ypos,
                        "Zposition" => Ch::Zpos,
                        "Xrotation" => Ch::Xrot,
                        "Yrotation" => Ch::Yrot,
                        "Zrotation" => Ch::Zrot,
                        other => return Err(format!("bvh: unknown channel {other}")),
                    };
                    channels[j].push(c);
                }
            }
            "MOTION" => break,
            other => return Err(format!("bvh: unexpected {other:?} in the hierarchy")),
        }
    }
    // Frames: N / Frame Time: t
    let mut frame_count = 0usize;
    let mut frame_time = 0.0f32;
    while let Some(tok) = tokens.peek().copied() {
        match tok {
            "Frames:" => {
                tokens.next();
                frame_count = num(tokens.next())? as usize;
            }
            "Frame" => {
                tokens.next(); // "Time:"
                tokens.next();
                frame_time = num(tokens.next())?;
                break;
            }
            _ => {
                tokens.next();
            }
        }
    }
    if frame_time <= 0.0 {
        return Err("bvh: no frame time".into());
    }
    let mut frames = Vec::with_capacity(frame_count);
    for _ in 0..frame_count {
        let mut pose = Vec::with_capacity(names.len());
        for j in 0..names.len() {
            let mut t = offsets[j];
            let mut moved = false;
            let mut r = Quat::IDENTITY;
            for &c in &channels[j] {
                let v = num(tokens.next())?;
                match c {
                    Ch::Xpos | Ch::Ypos | Ch::Zpos => {
                        if !moved {
                            t = Vec3::ZERO;
                            moved = true;
                        }
                        match c {
                            Ch::Xpos => t.x = v,
                            Ch::Ypos => t.y = v,
                            _ => t.z = v,
                        }
                    }
                    Ch::Xrot => r *= Quat::from_rotation_x(v.to_radians()),
                    Ch::Yrot => r *= Quat::from_rotation_y(v.to_radians()),
                    Ch::Zrot => r *= Quat::from_rotation_z(v.to_radians()),
                }
            }
            pose.push((t, r.normalize()));
        }
        frames.push(pose);
    }
    Ok(Motion {
        names,
        parents,
        offsets,
        ends,
        fps: 1.0 / frame_time,
        frames,
    })
}

impl Motion {
    /// World translation and rotation of every joint, from local ones.
    #[must_use]
    pub fn world(&self, local: &[(Vec3, Quat)]) -> Vec<(Vec3, Quat)> {
        let mut out: Vec<(Vec3, Quat)> = Vec::with_capacity(local.len());
        for (j, &(t, r)) in local.iter().enumerate() {
            out.push(match self.parents[j] {
                Some(p) => {
                    let (pt, pr) = out[p];
                    (pt + pr * t, (pr * r).normalize())
                }
                None => (t, r),
            });
        }
        out
    }

    /// The rest pose: offsets and no rotation.
    #[must_use]
    pub fn rest(&self) -> Vec<(Vec3, Quat)> {
        self.offsets.iter().map(|&o| (o, Quat::IDENTITY)).collect()
    }

    pub fn joint(&self, name: &str) -> Option<usize> {
        self.names.iter().position(|n| n == name)
    }
}

/// The rig's bone for a capture joint, across the namings of the two sources:
/// Xsens's (100STYLE: `Chest2`, `LeftCollar`, `LeftShoulder`, `LeftElbow`,
/// `LeftWrist`, `LeftHip`, `LeftKnee`, `LeftAnkle`, `LeftToe`) and CMU's
/// (`LowerBack`, `Spine1`, `LeftArm`, `LeftForeArm`, `LeftUpLeg`, `LeftLeg`,
/// `LeftFoot`, `LeftToeBase`). Joints with no counterpart (CMU's `LHipJoint`,
/// Xsens's extra chest segment) map to nothing and only carry their children.
#[must_use]
pub fn rig_bone(name: &str) -> Option<String> {
    let (side, stem) = if let Some(s) = name.strip_prefix("Left") {
        (Some("l"), s)
    } else if let Some(s) = name.strip_prefix("Right") {
        (Some("r"), s)
    } else {
        (None, name)
    };
    let base = match (side.is_some(), stem) {
        (false, "Hips") => "pelvis",
        // Xsens: four chest segments onto three spine bones; the top one (where
        // the collars hang) is spine_03.
        (false, "Chest") | (false, "LowerBack") => "spine_01",
        (false, "Chest2") | (false, "Spine") => "spine_02",
        (false, "Chest4") | (false, "Spine1") => "spine_03",
        (false, "Neck") => "neck_01",
        (false, "Head") => "head",
        (true, "Collar") => "clavicle",
        // Xsens's Shoulder is the upper arm and CMU's the clavicle; `map_bones`
        // settles it from the skeleton.
        (true, "Shoulder") => return None,
        (true, "Arm") => "upperarm",
        (true, "Elbow") | (true, "ForeArm") => "lowerarm",
        (true, "Wrist") | (true, "Hand") => "hand",
        (true, "Hip") | (true, "UpLeg") => "thigh",
        (true, "Knee") | (true, "Leg") => "calf",
        (true, "Ankle") | (true, "Foot") => "foot",
        (true, "Toe") | (true, "ToeBase") => "ball",
        _ => return None,
    };
    Some(match side {
        Some(s) => format!("{base}_{s}"),
        None => base.to_string(),
    })
}

/// Map every rig bone to a capture joint. The one ambiguity — `LeftShoulder`
/// is Xsens's upper arm and CMU's clavicle — is settled by the skeleton: a
/// skeleton with `LeftCollar` is Xsens's.
#[must_use]
pub fn map_bones(m: &Motion, bones: &[Bone]) -> Vec<Option<usize>> {
    let xsens = m.names.iter().any(|n| n.ends_with("Collar"));
    let rig_name = |n: &str| -> Option<String> {
        for (prefix, side) in [("Left", "l"), ("Right", "r")] {
            if n == format!("{prefix}Shoulder") {
                return Some(if xsens {
                    format!("upperarm_{side}")
                } else {
                    format!("clavicle_{side}")
                });
            }
        }
        rig_bone(n)
    };
    let mut map = vec![None; bones.len()];
    for (j, n) in m.names.iter().enumerate() {
        if let Some(rig) = rig_name(n) {
            if let Some(b) = bones.iter().position(|b| b.name == rig) {
                map[b].get_or_insert(j);
            }
        }
    }
    map
}

/// How to cut a clip out of a take.
#[derive(Clone, Debug)]
pub struct Cut {
    /// The clip's name in the pack.
    pub name: String,
    pub file: PathBuf,
    /// Frames of the take to consider, in the take's own frame numbers.
    pub start: Option<usize>,
    pub end: Option<usize>,
    pub kind: CutKind,
    /// The take's true frame rate, when its file states another.
    pub fps: Option<f32>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CutKind {
    /// Played once (an emote): the frames as they are, faced forward.
    Once,
    /// A cycle between `min_s` and `max_s` seconds long, found in the take.
    Loop { min_s: f32, max_s: f32 },
}

/// A pose as something comparable across frames: every joint's position
/// relative to the hips, turned so the hips face the same way, plus the hips'
/// height. Units are the take's.
fn features(world: &[(Vec3, Quat)], root: usize) -> Vec<Vec3> {
    let (hp, hr) = world[root];
    let yaw = heading_of(hr);
    let unturn = Quat::from_rotation_y(-yaw);
    let mut f: Vec<Vec3> = world.iter().map(|(p, _)| unturn * (*p - hp)).collect();
    f.push(Vec3::new(0.0, hp.y, 0.0));
    f
}

/// The yaw (about +Y) of a rotation's forward (+Z) axis.
fn heading_of(r: Quat) -> f32 {
    let f = r * Vec3::Z;
    f.x.atan2(f.z)
}

/// Find the stretch `[a, a + len]` of `frames` (world poses) whose two ends
/// match best, `len` within `[min, max]` frames: positions and velocities of
/// the pose features, and the hips' change of heading.
#[must_use]
pub fn find_loop(
    worlds: &[Vec<(Vec3, Quat)>],
    root: usize,
    min: usize,
    max: usize,
) -> Option<(usize, usize)> {
    let n = worlds.len();
    if n < min + 2 {
        return None;
    }
    let feats: Vec<Vec<Vec3>> = worlds.iter().map(|w| features(w, root)).collect();
    let vel = |i: usize| -> Vec<Vec3> {
        let j = (i + 1).min(n - 1);
        feats[j]
            .iter()
            .zip(&feats[i])
            .map(|(a, b)| *a - *b)
            .collect()
    };
    let scale = feats[0]
        .iter()
        .map(|v| v.length())
        .fold(0.0f32, f32::max)
        .max(1.0e-3);
    // For a take that travels: the hips' path over the ground, and the pace
    // the performer usually keeps (the 75th percentile over cycle-long
    // windows). A take of walking is mostly walking but also turns at the
    // walls of the room, and a turn on the spot can match its ends as well as a
    // stride does; the pace and straightness terms keep the loop on the straight.
    let ground: Vec<Vec3> = worlds
        .iter()
        .map(|w| Vec3::new(w[root].0.x, 0.0, w[root].0.z))
        .collect();
    let mut walked = vec![0.0f32; n];
    for i in 1..n {
        walked[i] = walked[i - 1] + (ground[i] - ground[i - 1]).length();
    }
    let mut paces: Vec<f32> = (0..n - min)
        .map(|i| (ground[i + min] - ground[i]).length() / min as f32)
        .collect();
    paces.sort_by(f32::total_cmp);
    let usual = paces.get(paces.len() * 3 / 4).copied().unwrap_or(0.0);
    let travels = usual * min as f32 > 0.2 * scale;
    let mut best: Option<(f32, usize, usize)> = None;
    for a in 0..n - min - 1 {
        let va = vel(a);
        for len in min..=max.min(n - 2 - a) {
            let b = a + len;
            let pos: f32 = feats[a]
                .iter()
                .zip(&feats[b])
                .map(|(x, y)| (*x - *y).length_squared())
                .sum();
            let vb = vel(b);
            let v: f32 = va
                .iter()
                .zip(&vb)
                .map(|(x, y)| (*x - *y).length_squared())
                .sum();
            let turn = (heading_of(worlds[a][root].1) - heading_of(worlds[b][root].1))
                .sin()
                .abs();
            let mut cost = pos / (scale * scale) + 40.0 * v / (scale * scale) + 0.5 * turn;
            if travels {
                let net = (ground[b] - ground[a]).length();
                let path = (walked[b] - walked[a]).max(1.0e-6);
                let pace = net / len as f32;
                cost += 3.0 * ((pace - usual) / usual).powi(2) + 3.0 * (1.0 - net / path);
            }
            if best.is_none_or(|(c, _, _)| cost < c) {
                best = Some((cost, a, len));
            }
        }
    }
    best.map(|(_, a, len)| (a, len))
}

/// Cut `cut` out of the take `m` and express it the way the pack stores clips.
pub fn clip_from_motion(m: &Motion, cut: &Cut, bones: &[Bone], fps_out: f32) -> Result<SourceClip> {
    let map = map_bones(m, bones);
    let pelvis = bones
        .iter()
        .position(|b| b.name == "pelvis")
        .and_then(|p| map[p])
        .ok_or_else(|| format!("{}: no hips joint", cut.file.display()))?;
    if m.parents[pelvis].is_some() {
        return Err(format!("{}: the hips are not the root", cut.file.display()));
    }
    let joint = |rig: &str| {
        bones
            .iter()
            .position(|b| b.name == rig)
            .and_then(|b| map[b])
    };
    let (foot, toe) = (joint("foot_l"), joint("ball_l"));

    // The rest pose: which way it faces (toes ahead of the ankle), and the
    // hips' height above the ankles, which every distance is measured in.
    let rest = m.world(&m.rest());
    let (Some(foot), Some(toe)) = (foot, toe) else {
        return Err(format!("{}: no left foot and toe", cut.file.display()));
    };
    let rest_forward = {
        let d = rest[toe].0 - rest[foot].0;
        d.x.atan2(d.z)
    };
    let hip_height = (rest[pelvis].0.y - rest[foot].0.y).abs().max(1.0e-3);

    // The cut's own rate where the file's is wrong (CMU subjects 141 and 143
    // were captured at 60 fps and say 120).
    let fps = cut.fps.unwrap_or(m.fps);
    let first = cut.start.unwrap_or(0).min(m.frames.len());
    let last = cut.end.unwrap_or(m.frames.len()).min(m.frames.len());
    if last <= first + 1 {
        return Err(format!("{}: an empty cut", cut.file.display()));
    }
    let take: Vec<Vec<(Vec3, Quat)>> = m.frames[first..last].to_vec();

    // The stretch to keep.
    let (a, len) = match cut.kind {
        CutKind::Once => (0, take.len() - 1),
        CutKind::Loop { min_s, max_s } => {
            let worlds: Vec<_> = take.iter().map(|f| m.world(f)).collect();
            let min = (min_s * fps).round() as usize;
            let max = (max_s * fps).round() as usize;
            find_loop(&worlds, pelvis, min.max(2), max.max(min + 1))
                .ok_or_else(|| format!("{}: too short for a {min_s}–{max_s} s loop", cut.name))?
        }
    };
    let mut frames: Vec<Vec<(Vec3, Quat)>> = take[a..=a + len].to_vec();
    let secs = len as f32 / fps;

    // Travel, and the heading to turn to: the way it went if it went anywhere,
    // else the way the body faced at the start.
    let start = frames[0][pelvis].0;
    let travel = {
        let d = frames[len][pelvis].0 - start;
        Vec3::new(d.x, 0.0, d.z)
    };
    let looped = matches!(cut.kind, CutKind::Loop { .. });
    // A loop that goes anywhere is locomotion; a one-off has to go a long way
    // (the dive roll's four metres) before its travel is taken out, or the
    // drift of a wave would turn the body to face it.
    let pace = travel.length() / secs / hip_height;
    let moving = if looped { pace > 0.2 } else { pace > 1.0 };
    let heading = if moving {
        travel.x.atan2(travel.z)
    } else {
        heading_of(frames[0][pelvis].1)
    };
    // Face +Z: first turn the take to the rest pose's facing, then the rest
    // pose to +Z.
    let face_rest = if moving {
        Quat::from_rotation_y(rest_forward - heading)
    } else {
        // Standing still: line the hips up with the rest pose's hips.
        Quat::from_rotation_y(heading_of(rest[pelvis].1) - heading)
    };
    for (i, f) in frames.iter_mut().enumerate() {
        let (t, r) = f[pelvis];
        let mut t = face_rest * (t - Vec3::new(start.x, 0.0, start.z));
        if moving {
            t -= face_rest * travel * (i as f32 / len as f32);
        }
        f[pelvis] = (t, (face_rest * r).normalize());
    }
    // Close the loop: spread whatever still differs between the last frame and
    // the first over the whole stretch, so the last *is* the first.
    if looped {
        let end = frames[len].clone();
        let begin = frames[0].clone();
        for (i, f) in frames.iter_mut().enumerate() {
            let s = i as f32 / len as f32;
            for (j, (t, r)) in f.iter_mut().enumerate() {
                let fix = end[j].1.inverse() * begin[j].1;
                *r = (*r * Quat::IDENTITY.slerp(fix, s)).normalize();
                *t += (begin[j].0 - end[j].0) * s;
            }
        }
    }

    // Resample to the pack's rate.
    let out_frames = (secs * fps_out).round().max(1.0) as usize + 1;
    let sample = |t_s: f32| -> Vec<(Vec3, Quat)> {
        let x = (t_s * fps).clamp(0.0, len as f32);
        let i = (x.floor() as usize).min(len);
        let k = (i + 1).min(len);
        let s = x - i as f32;
        frames[i]
            .iter()
            .zip(&frames[k])
            .map(|(&(ta, ra), &(tb, rb))| (ta.lerp(tb, s), ra.slerp(rb, s).normalize()))
            .collect()
    };
    let turn = Quat::from_rotation_y(-rest_forward);
    let rest_rot: Vec<Quat> = rest.iter().map(|(_, r)| *r).collect();
    let mut rotations: Vec<Option<Vec<[f32; 4]>>> = map
        .iter()
        .map(|m| m.map(|_| Vec::with_capacity(out_frames)))
        .collect();
    let mut hips = Vec::with_capacity(out_frames);
    for f in 0..out_frames {
        let t_s = f as f32 / (out_frames - 1) as f32 * secs;
        let w = m.world(&sample(t_s));
        for (b, j) in map.iter().enumerate() {
            if let (Some(j), Some(track)) = (j, rotations[b].as_mut()) {
                let delta = w[*j].1 * rest_rot[*j].inverse();
                track.push((turn * delta * turn.inverse()).normalize().to_array());
            }
        }
        let offset = (w[pelvis].0 - Vec3::new(0.0, rest[pelvis].0.y, 0.0)) / hip_height;
        hips.push((turn * offset).to_array());
    }
    // Which way each bone points at rest: towards the joint of the rig child
    // that continues its chain (the unsided one, so the top of the spine aims
    // at the neck rather than a collarbone), else towards its End Site.
    let rest_dirs = bones
        .iter()
        .enumerate()
        .map(|(b, _)| {
            let j = map[b]?;
            let children: Vec<usize> = bones
                .iter()
                .enumerate()
                .filter(|(c, bone)| bone.parent == Some(b as u16) && map[*c].is_some())
                .map(|(c, _)| c)
                .collect();
            let sided =
                |c: &usize| bones[*c].name.ends_with("_l") || bones[*c].name.ends_with("_r");
            let child = children
                .iter()
                .find(|c| !sided(c))
                .or(children.first())
                .copied();
            let dir = match child {
                Some(c) => rest[map[c]?].0 - rest[j].0,
                None => rest[j].1 * m.ends[j]?,
            };
            (dir.length() > 1.0e-4).then(|| crate::RestDir {
                dir: (turn * dir).normalize().to_array(),
                child: child.map(|c| c as u16),
            })
        })
        .collect();
    Ok(SourceClip {
        name: cut.name.clone(),
        fps: (out_frames - 1) as f32 / secs,
        frames: out_frames as u32,
        rotations,
        hips,
        speed: if moving && looped { pace } else { 0.0 },
        looped,
        rest_dirs,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const BVH: &str = "HIERARCHY
ROOT Hips
{
  OFFSET 0 0 0
  CHANNELS 6 Xposition Yposition Zposition Zrotation Xrotation Yrotation
  JOINT LeftUpLeg
  {
    OFFSET 10 0 0
    CHANNELS 3 Zrotation Xrotation Yrotation
    End Site
    {
      OFFSET 0 -40 0
    }
  }
}
MOTION
Frames: 2
Frame Time: 0.5
0 90 0 0 0 0 0 0 0
1 90 2 0 90 0 0 0 0
";

    #[test]
    fn a_bvh_reads_its_hierarchy_and_frames() {
        let m = parse_bvh(BVH).unwrap();
        assert_eq!(m.names, ["Hips", "LeftUpLeg"]);
        assert_eq!(m.parents, [None, Some(0)]);
        assert_eq!(m.offsets[1], Vec3::new(10.0, 0.0, 0.0));
        assert_eq!(m.fps, 2.0);
        assert_eq!(m.frames.len(), 2);
        assert_eq!(
            m.frames[1][0].0,
            Vec3::new(1.0, 90.0, 2.0),
            "the root's position is absolute"
        );
        // 90° about X turns the hips' +Y onto +Z.
        let up = m.frames[1][0].1 * Vec3::Y;
        assert!((up - Vec3::Z).length() < 1.0e-5, "{up}");
        let w = m.world(&m.frames[1]);
        assert!(
            (w[1].0 - Vec3::new(11.0, 90.0, 2.0)).length() < 1.0e-4,
            "{}",
            w[1].0
        );
    }

    #[test]
    fn both_namings_reach_the_rig() {
        for (cap, rig) in [
            ("Hips", "pelvis"),
            ("Chest4", "spine_03"),
            ("Spine1", "spine_03"),
            ("LeftCollar", "clavicle_l"),
            ("RightElbow", "lowerarm_r"),
            ("LeftForeArm", "lowerarm_l"),
            ("RightUpLeg", "thigh_r"),
            ("LeftAnkle", "foot_l"),
            ("RightToeBase", "ball_r"),
        ] {
            assert_eq!(rig_bone(cap).as_deref(), Some(rig), "{cap}");
        }
        assert_eq!(rig_bone("LHipJoint"), None);
    }

    /// A take that repeats every 10 frames is found to loop at 10 (or a
    /// multiple), wherever the search starts.
    #[test]
    fn a_periodic_take_loops_at_its_period() {
        let mut m = parse_bvh(BVH).unwrap();
        m.fps = 30.0;
        m.frames = (0..80)
            .map(|i| {
                let phase = i as f32 / 10.0 * std::f32::consts::TAU;
                // The hips roll, which swings the leg joint about them; the
                // leg's own rotation would move nothing, having no child joint.
                vec![
                    (
                        Vec3::new(0.0, 90.0, i as f32),
                        Quat::from_rotation_z(0.4 * phase.sin()),
                    ),
                    (Vec3::new(10.0, 0.0, 0.0), Quat::IDENTITY),
                ]
            })
            .collect();
        let worlds: Vec<_> = m.frames.iter().map(|f| m.world(f)).collect();
        let (_, len) = find_loop(&worlds, 0, 7, 14).unwrap();
        assert_eq!(len, 10);
    }
}
