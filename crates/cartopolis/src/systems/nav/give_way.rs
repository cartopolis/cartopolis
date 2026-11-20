//! Traffic gives way to the player: cars and cyclists stop for an avatar on
//! the road ahead, stop at a zebra somebody is waiting at or crossing, and a
//! button on a signal pole holds the cars at red while the walker crosses
//! (`docs/notes/ambient-traffic.md`, "Traffic gives way to you").
//!
//! The decisions are pure functions here; `systems::traffic::drive_traffic`
//! calls them while it moves each vehicle, as it calls `crowd_react` for the
//! walkers, and reads the people from the same `CrowdPeople` list. The
//! crossings and signals are the street-detail layer's (`street_detail`,
//! OpenStreetMap): a zebra is a `highway=crossing` the data marks as one
//! (`street_detail::zebra_priority`), a button is a signal tagged
//! `button_operated=yes`. Nothing is placed that the data does not have.
//!
//! **Per viewer, nothing on the wire.** The traffic is invented on each
//! client, so a pressed button holds the cars this client draws: another
//! player standing beside you sees their own cars, held or not by their own
//! presses.

pub use crate::systems::street_detail::PED_RED_GLOW;
use bevy::prelude::*;
use big_space::prelude::{BigSpace, Grid};

use crate::systems::crowd_react::AVATAR_RADIUS_M;
use crate::systems::street_detail::{
    SignalPoint, StreetDetail, ZebraPoint, CAR_HEAD_Y, CAR_LAMP_DY, PED_HEAD_Y, PED_LAMP_DY,
    PED_LAMP_HALF,
};

// ── Somebody on the road ─────────────────────────────────────────────────────

/// A vehicle's footprint as the give-way sees it: half its width, and how far
/// its nose is in front of the centre the traffic moves.
#[derive(Clone, Copy, Debug)]
pub struct Body {
    pub half_width: f32,
    pub nose: f32,
}

/// The moving cars (`agent_models`' bodies are 1.7–1.9 m wide, 4.0–4.9 long).
pub const CAR_BODY: Body = Body {
    half_width: 0.95,
    nose: 2.3,
};
/// A city bike and its rider: the handlebar's half width, half the bike.
pub const BIKE_BODY: Body = Body {
    half_width: 0.35,
    nose: 0.95,
};

/// How far ahead a vehicle looks for somebody on its path, metres: a car at
/// 14 m/s stops in 33 m at [`YIELD_BRAKE`].
pub const YIELD_LOOKAHEAD_M: f32 = 40.0;
/// Clearance either side of the vehicle's own width that still counts as in
/// its path: a person half a metre off the wing is one a driver stops for.
pub const PATH_MARGIN_M: f32 = 0.45;
/// How far short of the person a stopped vehicle's nose waits, metres.
pub const STAND_OFF_M: f32 = 1.6;
/// The deceleration a stop for a person is planned at, m/s² — the signals'.
pub const YIELD_BRAKE: f32 = 3.0;
/// What a vehicle brakes at when the plan cannot be met — somebody stepped
/// out close in front — m/s². Near what a car's brakes give on a dry road.
pub const EMERGENCY_BRAKE: f32 = 8.0;

/// What the people on its path leave a vehicle.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Yield {
    /// The fastest it may go now: a braking curve ending [`STAND_OFF_M`]
    /// short of the nearest person in its path.
    pub limit: f32,
    /// The curve cannot be met at the planned rate: brake at
    /// [`EMERGENCY_BRAKE`].
    pub hard: bool,
    /// The most its centre may move this frame before its nose touches
    /// somebody — the last word, whatever the brakes managed.
    pub room: f32,
}

impl Yield {
    pub const CLEAR: Self = Self {
        limit: f32::MAX,
        hard: false,
        room: f32::MAX,
    };
}

/// Give way to the people on a vehicle's path. Each is `(ahead, lateral)`:
/// metres ahead of the vehicle's centre along its path, and metres off the
/// middle of its lane. A person beside the body (behind the nose, ahead of
/// the tail) and in its width stops it dead; one behind it is no concern.
#[must_use]
pub fn yield_to(people: impl IntoIterator<Item = (f32, f32)>, speed: f32, body: Body) -> Yield {
    let mut out = Yield::CLEAR;
    let in_path = body.half_width + AVATAR_RADIUS_M + PATH_MARGIN_M;
    for (ahead, lateral) in people {
        if lateral.abs() > in_path || ahead > YIELD_LOOKAHEAD_M {
            continue;
        }
        // Behind the tail: it is driving away from them.
        if ahead < -(body.nose + AVATAR_RADIUS_M) {
            continue;
        }
        // How far the centre may go before the nose meets them.
        let contact = (ahead - body.nose - AVATAR_RADIUS_M).max(0.0);
        let gap = (contact - STAND_OFF_M).max(0.0);
        out.room = out.room.min(contact);
        out.limit = out.limit.min((2.0 * YIELD_BRAKE * gap).sqrt());
        if speed * speed / (2.0 * YIELD_BRAKE) > gap + 0.5 {
            out.hard = true;
        }
    }
    out
}

// ── Zebras ───────────────────────────────────────────────────────────────────

/// Half the depth of the drawn stripes along the road (five 0.5 m bars on a
/// 0.9 m pitch, `street_detail`), metres.
pub const ZEBRA_HALF_DEPTH_M: f32 = 2.05;
/// How far back from the kerb somebody still counts as waiting to cross.
pub const KERB_REACH_M: f32 = 2.0;
/// How squarely somebody at the kerb must face the road to be waiting to
/// cross: within 60° of straight across.
pub const FACING_COS: f32 = 0.5;
/// Where a vehicle stops for a zebra: its nose this far short of the stripes.
pub const ZEBRA_STOP_SHORT_M: f32 = 1.0;
/// How far ahead a zebra is looked for, metres — the signals' reach.
pub const ZEBRA_LOOKAHEAD_M: f32 = 45.0;
/// The hardest a driver brakes for a zebra; past it the car is committed and
/// goes, as at an amber light. The person on it is still given way to by
/// [`yield_to`], which has no such limit.
pub const ZEBRA_HARD_BRAKE: f32 = 7.0;

/// Whether somebody at `at`, facing `facing`, is crossing `z` or waiting to:
/// on the stripes, or at the kerb at either end of them facing the road.
#[must_use]
pub fn waiting_at(z: &ZebraPoint, at: Vec2, facing: Vec2) -> bool {
    let rel = at - z.at;
    let along = rel.dot(z.road);
    if along.abs() > ZEBRA_HALF_DEPTH_M + 0.8 {
        return false;
    }
    let walk = Vec2::new(-z.road.y, z.road.x);
    let across = rel.dot(walk);
    if across.abs() <= z.half_width {
        return true;
    }
    if across.abs() > z.half_width + KERB_REACH_M {
        return false;
    }
    // At the kerb: facing into the road, toward the other side.
    facing.dot(-walk * across.signum()) >= FACING_COS
}

/// The fastest a vehicle may go for the zebras ahead of it: a braking curve
/// ending [`ZEBRA_STOP_SHORT_M`] before the stripes of one somebody is
/// crossing or waiting at, or no limit. Only zebras across its own road
/// (the road's direction near its own) and within `reach` of its line count,
/// as for the signals; one behind it never does.
#[must_use]
pub fn zebra_speed(
    zebras: &[ZebraPoint],
    people: &[(Vec2, Vec2)],
    pos: Vec2,
    travel: Vec2,
    speed: f32,
    reach: f32,
    body: Body,
) -> f32 {
    let mut limit = f32::MAX;
    for z in zebras {
        if z.road.dot(travel).abs() < 0.7 {
            continue;
        }
        let rel = z.at - pos;
        let ahead = rel.dot(travel);
        let lateral = (rel.x * travel.y - rel.y * travel.x).abs();
        if lateral > reach || ahead <= 0.0 || ahead > ZEBRA_LOOKAHEAD_M {
            continue;
        }
        if !people.iter().any(|&(at, f)| waiting_at(z, at, f)) {
            continue;
        }
        let gap = ahead - ZEBRA_HALF_DEPTH_M - ZEBRA_STOP_SHORT_M - body.nose;
        if gap < -1.0 {
            // Its nose is on the stripes: it goes, and `yield_to` keeps it
            // off whoever is on them.
            continue;
        }
        let gap = gap.max(0.0);
        if speed * speed / (2.0 * ZEBRA_HARD_BRAKE) > gap + 0.5 {
            continue;
        }
        limit = limit.min((2.0 * YIELD_BRAKE * gap).sqrt());
    }
    limit
}

// ── The button ───────────────────────────────────────────────────────────────

/// After a press: the wait before the cars' light turns amber, seconds.
/// A Dutch push-button crossing answers in a few seconds to well over a
/// minute; this is the short end, because nobody waits a minute in a game.
pub const BUTTON_WAIT_S: f32 = 3.0;
/// Amber for the cars.
pub const CARS_AMBER_S: f32 = 3.0;
/// All red before the walker's green: the cars clear the crossing.
pub const ALL_RED_S: f32 = 1.5;
/// The walker's green.
pub const WALK_S: f32 = 8.0;
/// The walker's red again, the cars still held while the crossing clears.
pub const CLEAR_S: f32 = 3.5;
/// Signals of the pressed one's road within this of it are held with it: the
/// stop lines of both approaches and the poles at either kerb, metres.
pub const PRESS_REACH_M: f32 = 30.0;
/// How near a button pole the avatar must stand for the prompt, metres.
pub const BUTTON_REACH_M: f32 = 2.5;

/// Where a press is in its cycle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ButtonPhase {
    /// Pressed; nothing has changed yet.
    Waiting,
    /// The cars' light is amber.
    CarsAmber,
    /// The cars' light is red, the walker's not yet green.
    AllRed,
    /// The walker's green.
    Walk,
    /// The walker's red again; the cars still wait.
    Clearing,
}

impl ButtonPhase {
    /// `since` seconds after a press, or `None` once the cycle is over.
    #[must_use]
    pub fn at(since: f32) -> Option<Self> {
        let mut t = since;
        for (phase, len) in [
            (Self::Waiting, BUTTON_WAIT_S),
            (Self::CarsAmber, CARS_AMBER_S),
            (Self::AllRed, ALL_RED_S),
            (Self::Walk, WALK_S),
            (Self::Clearing, CLEAR_S),
        ] {
            if (0.0..len).contains(&t) {
                return Some(phase);
            }
            t -= len;
        }
        None
    }

    /// Whether the cars on the pressed road are held: amber and red count,
    /// as at any light (`traffic::signal_speed`).
    #[must_use]
    pub fn cars_stop(self) -> bool {
        self != Self::Waiting
    }

    /// Whether the walking man is green.
    #[must_use]
    pub fn walk(self) -> bool {
        self == Self::Walk
    }

    /// What the prompt says about it.
    #[must_use]
    pub fn detail(self) -> &'static str {
        if self.walk() {
            "walk"
        } else {
            "wait for green"
        }
    }
}

/// One press: the signal pressed (its stop line), its phase group, and when.
#[derive(Clone, Copy, Debug)]
pub struct Press {
    pub at: Vec2,
    pub group: u8,
    pub t0: f32,
}

/// The presses under way, on the moving world's clock (`anim_clock`).
#[derive(Resource, Default)]
pub struct SignalButtons {
    presses: Vec<Press>,
}

impl SignalButtons {
    /// The press covering a signal at `at` in `group`, if one is.
    fn covering(&self, at: Vec2, group: u8) -> Option<&Press> {
        self.presses
            .iter()
            .find(|p| p.group == group && p.at.distance(at) <= PRESS_REACH_M)
    }

    /// Press the button on `s` at `now`. A press while the crossing is
    /// already cycling does nothing, as a second press of a real one does.
    /// Returns whether it started a cycle.
    pub fn press(&mut self, s: &SignalPoint, now: f32) -> bool {
        if let Some(p) = self.covering(s.at, s.group) {
            if ButtonPhase::at(now - p.t0).is_some() {
                return false;
            }
        }
        self.presses
            .retain(|p| p.group != s.group || p.at.distance(s.at) > PRESS_REACH_M);
        self.presses.push(Press {
            at: s.at,
            group: s.group,
            t0: now,
        });
        true
    }

    /// Where the cycle covering a signal at `at` in `group` is, at `now`.
    #[must_use]
    pub fn phase(&self, at: Vec2, group: u8, now: f32) -> Option<ButtonPhase> {
        self.covering(at, group)
            .and_then(|p| ButtonPhase::at(now - p.t0))
    }

    /// Whether a press holds a signal at red for the cars at `now`.
    #[must_use]
    pub fn holds(&self, at: Vec2, group: u8, now: f32) -> bool {
        self.phase(at, group, now)
            .is_some_and(ButtonPhase::cars_stop)
    }

    /// Drop the presses whose cycle has run out.
    pub fn expire(&mut self, now: f32) {
        self.presses
            .retain(|p| ButtonPhase::at(now - p.t0).is_some() || now < p.t0);
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.presses.is_empty()
    }
}

/// The nearest button within [`BUTTON_REACH_M`] of `pos`: of the drawn pole
/// or of the mapped node, whichever is nearer. The pole is drawn beside the
/// node by a width the tags only estimate (3.1 m off on a footway), and a
/// pedestrian signal is mapped at the kerb where its button is, so standing
/// at the node is standing at the button.
#[must_use]
pub fn button_near(detail: &StreetDetail, pos: Vec2) -> Option<SignalPoint> {
    let reach = |s: &SignalPoint| s.pole.distance(pos).min(s.at.distance(pos));
    detail
        .signals_near(pos, BUTTON_REACH_M + 8.0)
        .filter(|s| s.button && reach(s) <= BUTTON_REACH_M)
        .min_by(|a, b| reach(a).total_cmp(&reach(b)))
}

/// Take the button presses the prompt made (`interact::InteractState`), and
/// `CARTO_PRESS_BUTTON=<s>`: the avatar presses the nearest button within
/// 12 m every that many seconds of the moving world's clock — the capture's
/// hook, since a scripted run has nobody to press E.
pub fn press_buttons(
    time: Res<crate::systems::anim_clock::AnimClock>,
    detail: Res<StreetDetail>,
    mut interact: ResMut<crate::systems::interact::InteractState>,
    mut avatar: ResMut<crate::resources::AvatarState>,
    mut buttons: ResMut<SignalButtons>,
    mut every: Local<Option<Option<f32>>>,
    mut next: Local<f32>,
) {
    let now = time.elapsed_secs();
    if let Some(pole) = interact.pressed_button.take() {
        let pos = Vec2::new(avatar.position.x, avatar.position.z);
        let pressed = detail
            .signals_near(pos, BUTTON_REACH_M + 12.0)
            .find(|s| s.button && s.pole.distance(pole) < 0.05);
        if let Some(s) = pressed {
            if buttons.press(&s, now) {
                info!("signal button pressed at ({:.1}, {:.1})", s.at.x, s.at.y);
            }
        }
    }
    let every = *every.get_or_insert_with(|| {
        crate::debug_switches::env_value("CARTO_PRESS_BUTTON")
            .and_then(|v| v.parse::<f32>().ok())
            .filter(|s| *s > 1.0)
    });
    if let Some(every) = every {
        if *next == 0.0 {
            *next = every;
        }
        if now >= *next {
            *next = now + every;
            let pos = Vec2::new(avatar.position.x, avatar.position.z);
            let nearest = detail
                .signals_near(pos, 20.0)
                .filter(|s| s.button && s.pole.distance(pos) <= 12.0)
                .min_by(|a, b| a.pole.distance(pos).total_cmp(&b.pole.distance(pos)));
            match nearest {
                Some(s) => {
                    avatar.pending_emote = Some(cartopolis_core::Emote::Interact);
                    let started = buttons.press(&s, now);
                    info!(
                        "CARTO_PRESS_BUTTON: pressed at {now:.1} s, pole {:.1} m off, cycle started: {started}",
                        s.pole.distance(pos)
                    );
                }
                None => info!("CARTO_PRESS_BUTTON: no button pole within 12 m at {now:.1} s"),
            }
        }
    }
    if !buttons.is_empty() {
        buttons.expire(now);
    }
}

// ── Drawing a pressed crossing ───────────────────────────────────────────────

const AMBER_GLOW: LinearRgba = LinearRgba::rgb(8.0, 4.5, 0.6);
const GREEN_GLOW: LinearRgba = LinearRgba::rgb(0.8, 8.0, 1.2);

/// The lamps a pressed crossing draws over the shared ones. The signal heads
/// are six shared materials cycled by one clock (`update_signal_cycle`), so
/// a single crossing cannot change colour through them; while a press runs,
/// each held head gets a dark face with its own lamp lit over it, and each
/// button pole's walking man its own red and green. One cube mesh and four
/// materials, made on the first press; a few entities per press, despawned
/// when it ends. Nothing is uploaded after the first.
#[derive(Default)]
pub struct Overlays {
    parts: Option<OverlayParts>,
    /// The entities drawn, and the presses (by `t0`) they belong to.
    drawn: Vec<(f32, Vec<(Entity, Lamp)>)>,
}

struct OverlayParts {
    cube: Handle<Mesh>,
    dark: Handle<StandardMaterial>,
    red: Handle<StandardMaterial>,
    amber: Handle<StandardMaterial>,
    green: Handle<StandardMaterial>,
}

/// What one overlay entity shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Lamp {
    /// A car head's dark face.
    CarFace,
    /// The car head's amber lamp, lit while the cars are told to stop.
    CarAmber,
    /// The car head's red lamp, lit while they are held after that.
    CarRed,
    /// The walking man's red lamp.
    PedRed,
    /// The walking man's green lamp.
    PedGreen,
}

impl Lamp {
    fn material(self, phase: ButtonPhase, p: &OverlayParts) -> Handle<StandardMaterial> {
        match self {
            Self::CarFace => p.dark.clone(),
            Self::CarAmber if phase == ButtonPhase::CarsAmber => p.amber.clone(),
            Self::CarRed if phase != ButtonPhase::CarsAmber => p.red.clone(),
            Self::CarAmber | Self::CarRed => p.dark.clone(),
            Self::PedRed if phase.walk() => p.dark.clone(),
            Self::PedRed => p.red.clone(),
            Self::PedGreen if phase.walk() => p.green.clone(),
            Self::PedGreen => p.dark.clone(),
        }
    }
}

/// Keep the overlays in step with the presses: spawn a press's when it first
/// holds the cars, swap their materials on a phase change, despawn them when
/// it ends.
#[allow(clippy::too_many_arguments)]
pub fn draw_buttons(
    mut commands: Commands,
    time: Res<crate::systems::anim_clock::AnimClock>,
    buttons: Res<SignalButtons>,
    detail: Res<StreetDetail>,
    roots: Query<(Entity, &Grid), With<BigSpace>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut lamps: Query<&mut MeshMaterial3d<StandardMaterial>>,
    mut overlays: Local<Overlays>,
) {
    let now = time.elapsed_secs();
    let Overlays { parts, drawn } = &mut *overlays;
    // Ended presses first.
    drawn.retain(|(t0, entities)| {
        let live = buttons.presses.iter().any(|p| p.t0 == *t0)
            && ButtonPhase::at(now - t0).is_some_and(|ph| ph != ButtonPhase::Waiting);
        if !live {
            for (e, _) in entities {
                commands.entity(*e).try_despawn();
            }
        }
        live
    });
    if buttons.presses.is_empty() {
        return;
    }
    let Ok((root, grid)) = roots.single() else {
        return;
    };
    let parts = parts.get_or_insert_with(|| {
        let lamp = |m: &mut Assets<StandardMaterial>, glow: LinearRgba| {
            m.add(StandardMaterial {
                base_color: Color::srgb(0.10, 0.10, 0.10),
                emissive: glow,
                perceptual_roughness: 0.3,
                ..default()
            })
        };
        OverlayParts {
            cube: meshes.add(Cuboid::new(1.0, 1.0, 1.0)),
            dark: lamp(&mut materials, LinearRgba::BLACK),
            red: lamp(&mut materials, PED_RED_GLOW),
            amber: lamp(&mut materials, AMBER_GLOW),
            green: lamp(&mut materials, GREEN_GLOW),
        }
    });
    for press in &buttons.presses {
        let Some(phase) = ButtonPhase::at(now - press.t0) else {
            continue;
        };
        if phase == ButtonPhase::Waiting {
            continue;
        }
        if let Some((_, entities)) = drawn.iter().find(|(t0, _)| *t0 == press.t0) {
            // Swap only what differs (a handle write, no upload).
            for (e, lamp) in entities {
                let want = lamp.material(phase, parts);
                if let Ok(mut m) = lamps.get_mut(*e) {
                    if m.0 != want {
                        m.0 = want;
                    }
                }
            }
            continue;
        }
        let mut entities = Vec::new();
        let held = detail
            .signals_near(press.at, PRESS_REACH_M)
            .filter(|s| s.group == press.group);
        for s in held {
            let rot = Quat::from_rotation_y(s.yaw);
            let base = Vec3::new(s.pole.x, s.ground, s.pole.y);
            let mut put = |local: Vec3, half: Vec3, lamp: Lamp| {
                let world = base + rot * local;
                let (cell, at) = grid.translation_to_grid(world.as_dvec3());
                let e = commands
                    .spawn((
                        Mesh3d(parts.cube.clone()),
                        MeshMaterial3d(lamp.material(phase, parts)),
                        Transform {
                            translation: at,
                            rotation: rot,
                            scale: half * 2.0,
                        },
                        cell,
                        ChildOf(root),
                        bevy::light::NotShadowCaster,
                    ))
                    .id();
                entities.push((e, lamp));
            };
            // The car head: a dark face over its three lamps, and its own
            // red and amber over the shared ones.
            put(
                Vec3::new(0.0, CAR_HEAD_Y, 0.115),
                Vec3::new(0.08, 0.36, 0.02),
                Lamp::CarFace,
            );
            for (dy, lamp) in [
                (CAR_LAMP_DY[0], Lamp::CarRed),
                (CAR_LAMP_DY[1], Lamp::CarAmber),
            ] {
                put(
                    Vec3::new(0.0, CAR_HEAD_Y + dy, 0.13),
                    Vec3::new(0.075, 0.075, 0.02),
                    lamp,
                );
            }
            if s.button {
                let half = Vec3::from_array(PED_LAMP_HALF) + Vec3::splat(0.01);
                put(
                    Vec3::new(0.0, PED_HEAD_Y + PED_LAMP_DY, 0.0),
                    half,
                    Lamp::PedRed,
                );
                put(
                    Vec3::new(0.0, PED_HEAD_Y - PED_LAMP_DY, 0.0),
                    half,
                    Lamp::PedGreen,
                );
            }
        }
        drawn.push((press.t0, entities));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DT: f32 = 1.0 / 60.0;

    /// Drive a car down +z at `v0` toward a person standing `ahead` metres in
    /// front, `lateral` off its lane, the way `drive_traffic` does: ease
    /// toward the limit at the car's rates (the emergency rate when the plan
    /// is short), and never move further than the room. The person leaves
    /// the road at `leaves_at` seconds. Returns the closest the nose came,
    /// the speed at the end, and where the car ended.
    fn drive_at(v0: f32, ahead: f32, lateral: f32, leaves_at: f32) -> (f32, f32, f32) {
        let mut z = 0.0_f32;
        let mut v = v0;
        let person_z = ahead;
        let mut closest = f32::MAX;
        let road_speed = v0.max(8.0);
        for i in 0..(30.0 / DT) as usize {
            let t = i as f32 * DT;
            let on_road = t < leaves_at;
            let people: Vec<(f32, f32)> = if on_road {
                vec![(person_z - z, lateral)]
            } else {
                Vec::new()
            };
            let y = yield_to(people, v, CAR_BODY);
            let want = road_speed.min(y.limit);
            let rate = if want > v {
                2.2
            } else if y.hard {
                EMERGENCY_BRAKE
            } else {
                5.5
            };
            v = (v + (want - v).clamp(-rate * DT, rate * DT)).max(0.0);
            z += (v * DT).min(y.room);
            if on_road {
                closest = closest.min(person_z - z - CAR_BODY.nose);
            }
        }
        (closest, v, z)
    }

    /// A car finding somebody standing in its lane 30 m ahead brakes on a
    /// curve, stops with its nose short of them, waits, and pulls away once
    /// they have gone.
    #[test]
    fn a_car_stops_short_of_a_person_and_pulls_away_after() {
        let (closest, v, z) = drive_at(11.0, 30.0, 0.3, 12.0);
        assert!(
            closest >= STAND_OFF_M + AVATAR_RADIUS_M - 0.1,
            "the nose came within {closest:.2} m"
        );
        assert!(
            closest < STAND_OFF_M + AVATAR_RADIUS_M + 0.6,
            "it stopped far short: {closest:.2}"
        );
        assert!(v > 7.0, "it pulled away again: {v:.2} m/s");
        assert!(z > 60.0, "and drove on: {z:.1} m");
    }

    /// Somebody stepping out 9 m in front of a car at 11 m/s — closer than
    /// the planned stop — is still never touched: the car brakes hard and
    /// the room clamp holds the rest.
    #[test]
    fn a_car_never_drives_through_somebody_who_steps_out() {
        for ahead in [4.0, 6.0, 9.0, 14.0] {
            let (closest, ..) = drive_at(11.0, ahead, 0.0, 100.0);
            assert!(
                closest >= AVATAR_RADIUS_M - 1e-3,
                "{ahead} m ahead: nose at {closest:.3}"
            );
        }
    }

    /// The opposite lane, the pavement and behind: no reason to stop.
    #[test]
    fn a_person_off_its_path_does_not_stop_a_car() {
        for (ahead, lateral) in [(20.0, 2.6), (20.0, -3.4), (-4.0, 0.0), (60.0, 0.0)] {
            assert_eq!(
                yield_to([(ahead, lateral)], 10.0, CAR_BODY),
                Yield::CLEAR,
                "{ahead}, {lateral}"
            );
        }
        // Beside its bonnet, in its width: it does not move.
        let y = yield_to([(1.0, 0.5)], 0.0, CAR_BODY);
        assert_eq!((y.limit, y.room), (0.0, 0.0));
    }

    /// A zebra across a north–south road at the origin, 6 m wide.
    fn zebra() -> ZebraPoint {
        ZebraPoint {
            at: Vec2::ZERO,
            road: Vec2::Y,
            half_width: 3.0,
        }
    }

    #[test]
    fn waiting_means_on_the_stripes_or_at_the_kerb_facing_the_road() {
        let z = zebra();
        // On it, whichever way.
        assert!(waiting_at(&z, Vec2::new(1.0, 0.5), Vec2::Y));
        // At the east kerb facing west, across: waiting.
        assert!(waiting_at(&z, Vec2::new(4.0, 0.0), Vec2::NEG_X));
        // The same spot facing along the pavement or away: not.
        assert!(!waiting_at(&z, Vec2::new(4.0, 0.0), Vec2::Y));
        assert!(!waiting_at(&z, Vec2::new(4.0, 0.0), Vec2::X));
        // Too far up the pavement or back from the kerb: not.
        assert!(!waiting_at(&z, Vec2::new(4.0, 5.0), Vec2::NEG_X));
        assert!(!waiting_at(&z, Vec2::new(7.5, 0.0), Vec2::NEG_X));
    }

    /// A car 40 m short of a zebra with somebody waiting at it brakes to stop
    /// before the stripes; nobody there, or a zebra on a cross street, or one
    /// behind it, and it does not.
    #[test]
    fn a_car_stops_before_a_zebra_somebody_waits_at() {
        let z = [zebra()];
        let north = Vec2::NEG_Y;
        let pos = Vec2::new(1.3, 40.0);
        let waiting = [(Vec2::new(4.0, 0.0), Vec2::NEG_X)];
        let v = zebra_speed(&z, &waiting, pos, north, 10.0, 4.0, CAR_BODY);
        let gap = 40.0 - ZEBRA_HALF_DEPTH_M - ZEBRA_STOP_SHORT_M - CAR_BODY.nose;
        assert!((v - (2.0 * YIELD_BRAKE * gap).sqrt()).abs() < 1e-3, "{v}");
        // At its stop it stays.
        let stopped = Vec2::new(1.3, 40.0 - gap);
        assert_eq!(
            zebra_speed(&z, &waiting, stopped, north, 0.0, 4.0, CAR_BODY),
            0.0
        );
        // Nobody waiting.
        let away = [(Vec2::new(4.0, 0.0), Vec2::X)];
        assert_eq!(
            zebra_speed(&z, &away, pos, north, 10.0, 4.0, CAR_BODY),
            f32::MAX
        );
        // The zebra across a cross street (its road runs east–west).
        let cross = [ZebraPoint {
            road: Vec2::X,
            ..zebra()
        }];
        assert_eq!(
            zebra_speed(&cross, &waiting, pos, north, 10.0, 4.0, CAR_BODY),
            f32::MAX
        );
        // Behind it.
        let past = Vec2::new(1.3, -5.0);
        assert_eq!(
            zebra_speed(&z, &waiting, past, north, 10.0, 4.0, CAR_BODY),
            f32::MAX
        );
        // Too close to stop at the planned rate: it goes, as on an amber.
        let close = Vec2::new(1.3, 8.0);
        assert_eq!(
            zebra_speed(&z, &waiting, close, north, 14.0, 4.0, CAR_BODY),
            f32::MAX
        );
    }

    #[test]
    fn the_button_cycle_holds_the_cars_then_lets_them_go() {
        assert_eq!(ButtonPhase::at(0.0), Some(ButtonPhase::Waiting));
        assert!(!ButtonPhase::Waiting.cars_stop());
        let walk_from = BUTTON_WAIT_S + CARS_AMBER_S + ALL_RED_S;
        assert_eq!(ButtonPhase::at(walk_from - 0.01), Some(ButtonPhase::AllRed));
        assert_eq!(ButtonPhase::at(walk_from + 0.01), Some(ButtonPhase::Walk));
        let end = walk_from + WALK_S + CLEAR_S;
        assert_eq!(ButtonPhase::at(end - 0.01), Some(ButtonPhase::Clearing));
        assert_eq!(ButtonPhase::at(end + 0.01), None);
        // About fifteen seconds of cars held, as asked.
        let held = end - BUTTON_WAIT_S;
        assert!((14.0..=18.0).contains(&held), "{held}");

        let pole = |x: f32| SignalPoint {
            at: Vec2::new(x, 0.0),
            group: 0,
            button: true,
            pole: Vec2::new(x, 1.0),
            ground: 0.0,
            yaw: 0.0,
        };
        let mut b = SignalButtons::default();
        assert!(b.press(&pole(0.0), 10.0));
        // A second press mid-cycle, here or at the far kerb, changes nothing.
        assert!(!b.press(&pole(8.0), 12.0));
        // The far kerb's stop line and the same road's: held; the cross
        // street's group, or a light 40 m on: not.
        let t = 10.0 + walk_from;
        assert!(b.holds(Vec2::new(8.0, 0.0), 0, t));
        assert!(!b.holds(Vec2::new(8.0, 0.0), 1, t));
        assert!(!b.holds(Vec2::new(40.0, 0.0), 0, t));
        assert!(!b.holds(Vec2::ZERO, 0, 10.5), "nothing during the wait");
        b.expire(10.0 + end + 0.1);
        assert!(b.is_empty());
        assert!(!b.holds(Vec2::ZERO, 0, 10.0 + end + 0.1));
        // And it can be pressed again.
        assert!(b.press(&pole(0.0), 40.0));
    }
}
