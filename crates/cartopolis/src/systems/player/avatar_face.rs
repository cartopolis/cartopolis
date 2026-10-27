//! The avatar's face: an expression it pulls, a blink now and then, and a face
//! its owner's camera tracks (`docs/notes/avatar-faces.md`).
//!
//! The body carries the face's 52 units as morph targets
//! (`cartopolis_human::face`, `human_avatar::spawn_human`), weighted by the
//! holder's `MorphWeights`. This module decides those weights, once a frame,
//! from what the avatar's [`Face`] holds:
//! - a **tracked face** — the owner's own, from the camera (`face_tracking`),
//!   or another's as it arrives (`FaceFrameOf`) — shown as it is while fresh;
//! - otherwise an **expression** (`Expression`), eased in, held and eased out
//!   over `EXPRESSION_SECS`; the local one starts with [`StartExpression`],
//!   another's arrives in its snapshot as an emote does;
//! - and a **blink**, every few seconds, unless a tracked face blinks itself.

use bevy::mesh::morph::MorphWeights;
use bevy::prelude::*;
use cartopolis_core::{Expression, EXPRESSION_SECS, FACE_UNITS};
use cartopolis_human::face;

use crate::components::{Avatar, RemoteAvatar};
use crate::systems::avatar::AvatarHolder;

/// The local avatar's face starts an expression: read by the face here and by
/// the intent sender, which puts it on the wire.
#[derive(Message, Clone, Copy, Debug)]
pub(crate) struct StartExpression(pub Expression);

/// What one avatar's face is doing, on the avatar entity.
#[derive(Component, Debug)]
pub(crate) struct Face {
    /// The expression showing, and for how long it has.
    expression: Option<(Expression, f32)>,
    /// The latest tracked face and its age, seconds.
    tracked: Option<([f32; FACE_UNITS], f32)>,
    /// Seconds to the next blink, and how far into one this is.
    next_blink: f32,
    blink: Option<f32>,
    /// What is shown, eased toward what is wanted.
    shown: [f32; FACE_UNITS],
    /// xorshift32 state for the blinks' spacing, different per face so a
    /// crowd does not blink in step.
    seed: u32,
}

impl Default for Face {
    fn default() -> Self {
        use std::sync::atomic::{AtomicU32, Ordering};
        static FACES: AtomicU32 = AtomicU32::new(0x9e37_79b9);
        let seed = FACES.fetch_add(0x6d2b_79f5, Ordering::Relaxed) | 1;
        let mut face = Self {
            expression: None,
            tracked: None,
            next_blink: 0.0,
            blink: None,
            shown: [0.0; FACE_UNITS],
            seed,
        };
        face.next_blink = 1.0 + face.random() * 3.0;
        face
    }
}

impl Face {
    /// 0..1.
    fn random(&mut self) -> f32 {
        let mut x = self.seed;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.seed = x;
        (x >> 8) as f32 / (1u32 << 24) as f32
    }

    pub(crate) fn express(&mut self, expression: Expression) {
        self.expression = Some((expression, 0.0));
    }

    /// A tracked face, each weight 0–1.
    pub(crate) fn track(&mut self, weights: [f32; FACE_UNITS]) {
        self.tracked = Some((weights, 0.0));
    }

    /// A tracked face off the wire: 0–255 a weight, or empty when its owner
    /// stopped tracking.
    pub(crate) fn track_bytes(&mut self, bytes: &[u8]) {
        if bytes.len() != FACE_UNITS {
            self.tracked = None;
            return;
        }
        let mut w = [0.0; FACE_UNITS];
        for (w, b) in w.iter_mut().zip(bytes) {
            *w = f32::from(*b) / 255.0;
        }
        self.track(w);
    }

    /// Advance by `dt` seconds and return what the face should show.
    fn step(&mut self, dt: f32) -> [f32; FACE_UNITS] {
        let mut want = [0.0; FACE_UNITS];
        let tracking = match &mut self.tracked {
            Some((w, age)) if *age < TRACKED_FRESH_SECS => {
                *age += dt;
                want = *w;
                true
            }
            _ => {
                self.tracked = None;
                false
            }
        };
        if let Some((e, t)) = &mut self.expression {
            *t += dt;
            let k = envelope(*t);
            if k <= 0.0 && *t > EXPRESSION_SECS {
                self.expression = None;
            } else if !tracking {
                if let Some(r) = face::recipe(e.name()) {
                    for (i, w) in face::weights(r).iter().enumerate() {
                        want[i] = w * k;
                    }
                }
            }
        }
        // Eased: a tracked face only a little, to take the camera's jitter
        // out; an expression more, so it reads as a face moving, not a switch.
        let tau = if tracking { 0.04 } else { 0.08 };
        let a = 1.0 - (-dt / tau).exp();
        for (s, w) in self.shown.iter_mut().zip(want) {
            *s += (w - *s) * a;
        }
        let mut out = self.shown;
        // The blink, unless a tracked face does its own; not eased, a lid
        // drops faster than any smoothing lets it.
        if !tracking {
            self.next_blink -= dt;
            if self.blink.is_none() && self.next_blink <= 0.0 {
                self.blink = Some(0.0);
                self.next_blink = 2.0 + self.random() * 4.0;
            }
            if let Some(t) = &mut self.blink {
                *t += dt;
                let closed = blink_closure(*t);
                if *t >= BLINK_SECS {
                    self.blink = None;
                }
                for unit in ["eyeBlinkLeft", "eyeBlinkRight"] {
                    if let Some(i) = face::unit(unit) {
                        out[i] = out[i].max(closed);
                    }
                }
            }
        }
        out
    }
}

/// How long a tracked face holds after its last frame: a few frames lost on a
/// busy link should not drop a face back to neutral, a stopped camera should.
const TRACKED_FRESH_SECS: f32 = 0.5;

/// A blink: the lid shuts in 60 ms and opens in 110.
const BLINK_SECS: f32 = 0.17;

fn blink_closure(t: f32) -> f32 {
    if t < 0.06 {
        t / 0.06
    } else {
        (1.0 - (t - 0.06) / 0.11).clamp(0.0, 1.0)
    }
}

/// An expression's strength `t` seconds in: up in a quarter second, held, and
/// let go over the last 0.6 s of `EXPRESSION_SECS`.
fn envelope(t: f32) -> f32 {
    let rise = (t / 0.25).clamp(0.0, 1.0);
    let fall = ((EXPRESSION_SECS - t) / 0.6).clamp(0.0, 1.0);
    let k = rise.min(fall);
    k * k * (3.0 - 2.0 * k)
}

/// Every avatar gets a face.
pub(crate) fn add_faces(
    mut commands: Commands,
    avatars: Query<Entity, (Or<(With<Avatar>, With<RemoteAvatar>)>, Without<Face>)>,
) {
    for e in &avatars {
        commands.entity(e).try_insert(Face::default());
    }
}

/// Faces tracked by other people's cameras, latest first, by sim avatar id:
/// filled by the network as `FaceFrameOf` arrives, drained here.
#[derive(Resource, Default)]
pub(crate) struct IncomingFaces(pub std::collections::HashMap<u64, Vec<u8>>);

/// What each face starts this frame: the local avatar's expressions, other
/// avatars' as their snapshots bring them, and other people's tracked faces.
pub(crate) fn feed_faces(
    mut started: MessageReader<StartExpression>,
    mut incoming: ResMut<IncomingFaces>,
    mut local: Query<&mut Face, (With<Avatar>, Without<RemoteAvatar>)>,
    mut remote: Query<(&mut Face, &mut RemoteAvatar), Without<Avatar>>,
) {
    for StartExpression(e) in started.read() {
        for mut face in &mut local {
            face.express(*e);
        }
    }
    for (mut face, mut r) in &mut remote {
        if let Some(e) = r.pending_expression.take() {
            face.express(e);
        }
        if let Some(bytes) = incoming.0.remove(&r.sim_id) {
            face.track_bytes(&bytes);
        }
    }
    // Faces for avatars not drawn here are worth nothing later.
    incoming.0.clear();
}

/// `CARTO_FACE=laugh`, or unit weights (`CARTO_FACE=jawOpen=1,eyeBlinkLeft=0.5`),
/// or both (`CARTO_FACE=sad,jawOpen=0.3`): every face held at that, still and
/// unblinking, for a capture, which cannot press a key at the right moment.
fn held_face() -> Option<[f32; FACE_UNITS]> {
    let spec = crate::debug_switches::env_value("CARTO_FACE")?;
    let mut out = [0.0; FACE_UNITS];
    for part in spec.split(',').map(str::trim) {
        match part.split_once('=') {
            Some((unit, w)) => {
                if let (Some(i), Ok(w)) = (face::unit(unit.trim()), w.trim().parse::<f32>()) {
                    out[i] = w.clamp(0.0, 1.0);
                }
            }
            None => {
                for (o, w) in out.iter_mut().zip(face::weights(face::recipe(part)?)) {
                    *o = o.max(w);
                }
            }
        }
    }
    Some(out)
}

/// Step every face and write its weights onto its body's holder — only when
/// they changed, since a written `MorphWeights` is uploaded again.
pub(crate) fn drive_faces(
    time: Res<Time>,
    mut faces: Query<&mut Face>,
    mut holders: Query<(&ChildOf, &mut MorphWeights), With<AvatarHolder>>,
    mut held: Local<Option<Option<[f32; FACE_UNITS]>>>,
) {
    let held = *held.get_or_insert_with(held_face);
    let dt = time.delta_secs().min(0.1);
    let mut stepped: std::collections::HashMap<Entity, [f32; FACE_UNITS]> =
        std::collections::HashMap::new();
    for (child_of, mut weights) in &mut holders {
        let avatar = child_of.parent();
        let want = match stepped.get(&avatar) {
            Some(w) => *w,
            None => {
                let Ok(mut face) = faces.get_mut(avatar) else {
                    continue;
                };
                let w = held.unwrap_or_else(|| face.step(dt));
                stepped.insert(avatar, w);
                w
            }
        };
        let current = weights.weights();
        let differs = current.len() != FACE_UNITS
            || current
                .iter()
                .zip(&want)
                .any(|(a, b)| (a - b).abs() > 1.0e-3);
        if differs {
            for (w, v) in weights.weights_mut().iter_mut().zip(want) {
                *w = v;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An expression eases in, holds at its recipe and lets go by the end.
    #[test]
    fn an_expression_rises_holds_and_lets_go() {
        let mut f = Face::default();
        f.next_blink = 100.0;
        f.express(Expression::Happy);
        let smile = face::unit("mouthSmileLeft").unwrap();
        let mut at = Vec::new();
        for _ in 0..(EXPRESSION_SECS * 60.0) as usize + 30 {
            at.push(f.step(1.0 / 60.0)[smile]);
        }
        let held = at[120];
        assert!((held - 0.75).abs() < 0.02, "held at the recipe: {held}");
        assert!(at[3] < held * 0.5, "eased in: {}", at[3]);
        assert!(at.last().copied().unwrap() < 0.01, "let go");
        assert!(f.expression.is_none());
    }

    /// A tracked face is shown as it is and suppresses the blink; it goes
    /// when its frames stop.
    #[test]
    fn a_tracked_face_shows_and_goes_stale() {
        let mut f = Face::default();
        f.next_blink = 0.0;
        let jaw = face::unit("jawOpen").unwrap();
        let blink = face::unit("eyeBlinkLeft").unwrap();
        let mut bytes = vec![0u8; FACE_UNITS];
        bytes[jaw] = 255;
        f.track_bytes(&bytes);
        let mut w = [0.0; FACE_UNITS];
        for _ in 0..20 {
            w = f.step(1.0 / 60.0);
        }
        assert!(w[jaw] > 0.95, "jaw {}", w[jaw]);
        assert_eq!(w[blink], 0.0, "no blink over a tracked face");
        for _ in 0..60 {
            w = f.step(1.0 / 60.0);
        }
        assert!(
            f.tracked.is_none() && w[jaw] < 0.2,
            "stale after half a second"
        );
    }

    /// The idle face blinks within a few seconds.
    #[test]
    fn an_idle_face_blinks() {
        let mut f = Face::default();
        let blink = face::unit("eyeBlinkRight").unwrap();
        let most = (0..60 * 8)
            .map(|_| f.step(1.0 / 60.0)[blink])
            .fold(0.0, f32::max);
        assert!(most > 0.9, "a lid closes: {most}");
    }
}
