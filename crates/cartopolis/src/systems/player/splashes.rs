//! Water that reacts: a splash where somebody goes in, a ripple at every
//! stroke, rings round a swimmer treading water, spray on a fountain jet, and
//! the sounds of all of them.
//!
//! # Where the water is, and who is in it
//!
//! Nothing here knows more about water than the avatar does. A body is in the
//! water when its own map says so (`AvatarState::swimming`,
//! `RemoteAvatar::swimming`, both from `TerrainField::swim_surface_at`), so
//! every viewer derives other people's splashes from its own map, as it
//! derives their swimming — nothing went on the wire.
//!
//! - **Going in** is the edge of that flag. How big the splash is comes from
//!   how fast the body met the water ([`entry_speed`]): the drop from where its
//!   feet were the frame before (a quay is a metre or two over a canal, a lawn
//!   sloping into a pond is nothing) and how fast it was going.
//! - **A stroke** is the moment the swim clip's hands shoot forward through the
//!   surface, [`cartopolis_human::build::SWIM_STROKE_PHASE`], read off the
//!   animation player that is playing it — the arms, not a count of seconds.
//! - **A fountain jet** is `amenity=fountain` in open water
//!   (`docs/notes/a-fountain-is-two-objects.md`); a swimmer reaching one is
//!   splashed once, on the way in.
//!
//! # How it is drawn
//!
//! With `systems::particles`' shader and its one shared quad pool: a particle
//! is a function of time, and an emitter uploads nothing after it is built
//! (`docs/notes/particles-are-a-function-of-time.md`). Spray is the ordinary
//! billboarded blob; the rings on the surface are the shader's flat-ring shape
//! (`ParticleMaterial::rings`), which no prim can ask for. A splash re-arms one
//! of a fixed set of burst emitters rather than spawning anything, so a crowd
//! jumping into a canal is a few uniform writes, not entity churn.
//!
//! **A moving emitter drags its rings with it**, because every particle is
//! placed from the emitter's *current* origin. So a swimmer's continuous rings
//! are only for treading water, where the emitter stands still; a moving
//! swimmer leaves a burst at each stroke where it happened, which is a wake.
//!
//! Visible only with Settings ▸ Graphics ▸ Splashes and spray
//! ([`AppPrefs::water_effects`]) — never switched by the frame rate. The
//! sounds follow the effects volume like every other sound.

// Without `audio` the sound maths has no caller outside the tests; see
// `soundscape`.
#![cfg_attr(not(feature = "audio"), allow(dead_code))]

use std::collections::HashMap;

use bevy::prelude::*;
use big_space::prelude::{BigSpace, Grid, GridCell};
use cartopolis_core::{motion, ParticleEffect};

use crate::components::{Avatar, FreeCamera, RemoteAvatar};
use crate::resources::{AppPrefs, AvatarState};
use crate::systems::anim_clock::AnimClock;
use crate::systems::avatar::AvatarAnimations;
use crate::systems::particles::{ParticleMaterial, ParticlePool};
use crate::systems::terrain::TerrainField;
use crate::systems::world_places::WorldPlaces;
use cartopolis_materials::splash::{SplashMaterial, SplashShape};

pub use cartopolis_human::build::SWIM_STROKE_PHASE;

// ---------------------------------------------------------------------------
// The pure part: sizes, timing, levels.
// ---------------------------------------------------------------------------

/// Below this entry speed (m/s) going in is a wade: the smallest splash.
pub const WADE_SPEED: f32 = 1.0;
/// At this entry speed and above, the biggest: a body off a high quay.
pub const PLUNGE_SPEED: f32 = 8.0;
/// The highest drop counted (m). Past it the splash is as big as it gets, and
/// a body placed in the water from far away (a teleport) is not a meteor.
const MAX_DROP_M: f32 = 6.0;

/// How fast a body meets the water (m/s): the speed of a fall from `drop_m`
/// above the surface, under the avatar's own gravity (`motion::GRAVITY`, the
/// fall a quay drop is drawn with offline), and its speed across the ground.
#[must_use]
pub fn entry_speed(drop_m: f32, horizontal: f32) -> f32 {
    let drop = if drop_m.is_finite() {
        drop_m.clamp(0.0, MAX_DROP_M)
    } else {
        0.0
    };
    let fall = (2.0 * motion::GRAVITY * drop).sqrt();
    fall.hypot(if horizontal.is_finite() {
        horizontal.max(0.0)
    } else {
        0.0
    })
}

/// How big a splash is, 0 (a wade) to 1 (a plunge), from the entry speed.
#[must_use]
pub fn splash_size(entry_speed: f32) -> f32 {
    ((entry_speed - WADE_SPEED) / (PLUNGE_SPEED - WADE_SPEED)).clamp(0.0, 1.0)
}

/// Whether a loop going forward from phase `prev` to `now` passed `at` — once
/// per pass, including across the loop's end. A loop standing still passes
/// nothing.
#[must_use]
pub fn crossed(prev: f32, now: f32, at: f32) -> bool {
    if now == prev {
        false
    } else if now > prev {
        prev < at && at <= now
    } else {
        // Wrapped through the loop's end.
        at > prev || at <= now
    }
}

/// How loud a water sound is from `d` metres, 0..1: full within `near`,
/// falling as a point source does (6 dB a doubling) past it, and faded to
/// nothing between half `reach` and `reach` so it never stops with a step.
#[must_use]
pub fn water_level(d: f32, near: f32, reach: f32) -> f32 {
    if !d.is_finite() || d >= reach {
        return 0.0;
    }
    let near = near.max(0.01);
    let point = near / d.max(near);
    let x = ((d - 0.5 * reach) / (0.5 * reach)).clamp(0.0, 1.0);
    let fade = 1.0 - x * x * (3.0 - 2.0 * x);
    point * fade
}

/// A fountain jet is heard within this (m)…
pub const FOUNTAIN_NEAR_M: f32 = 4.0;
/// …and not past this.
pub const FOUNTAIN_REACH_M: f32 = 60.0;
/// A splash or a stroke: full within this (m)…
pub const SPLASH_NEAR_M: f32 = 3.0;
/// …and not past this.
pub const SPLASH_REACH_M: f32 = 45.0;
/// Close water round a swimmer is heard within this.
pub const TREAD_REACH_M: f32 = 25.0;

/// A swimmer this close to a jet (m, across the water) is in it.
const JET_RADIUS_M: f32 = 1.0;
/// The jet's column tops out here over its base (`furniture::form::FOUNTAIN_JET`).
const JET_TOP_M: f32 = 2.6;

/// What a splash plays: the big plunge takes from this size up, the small ones
/// below; and how loud, before distance and the user's volume.
#[must_use]
pub fn splash_sound(size: f32) -> (bool, f32) {
    let s = size.clamp(0.0, 1.0);
    (s >= 0.45, 0.30 + 0.55 * s)
}

/// Linear colours, emissive (added to what is behind). The droplets are lit
/// water catching the sky; the shader brightens them as they leave.
const SPRAY: [f32; 3] = [0.95, 1.0, 1.05];
const SPRAY_END: [f32; 3] = [0.60, 0.66, 0.72];
/// A ripple only tilts the reflection a little: a faint brightening, not a
/// line (2026-11-16 — crisp white rings read as UI drawn on the water).
const RIPPLE: [f32; 3] = [0.45, 0.50, 0.55];
const RIPPLE_END: [f32; 3] = [0.30, 0.34, 0.38];
/// Droplets fall at the Earth's gravity: this is water, not the avatar.
const DROPLET_GRAVITY: f32 = -9.8;

/// Seconds a droplet thrown up at `speed` takes to come back down to where
/// it left, plus a little for the ones born lower in the source sphere.
fn flight_secs(speed: f32) -> f32 {
    2.0 * speed / -DROPLET_GRAVITY + 0.15
}

/// The spray thrown up by a body going in, at `size` 0..1.
#[must_use]
pub fn entry_spray(size: f32) -> ParticleEffect {
    let s = size.clamp(0.0, 1.0);
    let speed = 1.6 + 4.4 * s;
    ParticleEffect {
        // Many small drops, each its own size and streaked along its flight
        // (`ParticleMaterial::droplets`): one size of soft round blob read as
        // snow (2026-11-16).
        count: (110.0 + 140.0 * s) as u16,
        life: flight_secs(speed),
        speed,
        spread_deg: 22.0,
        gravity: DROPLET_GRAVITY,
        source_radius: 0.20 + 0.40 * s,
        size_start: 0.022 + 0.012 * s,
        size_end: 0.03,
        color_start: SPRAY,
        color_end: SPRAY_END,
        alpha: 0.85,
        turbulence: 0.05,
        burst: true,
        sequence: 0,
    }
}

/// The ripples spreading from where a body went in: two or three, faint,
/// widening and fading as they go.
#[must_use]
pub fn entry_rings(size: f32) -> ParticleEffect {
    let s = size.clamp(0.0, 1.0);
    ParticleEffect {
        count: (2.0 + 2.0 * s) as u16,
        life: 2.0 + 1.5 * s,
        speed: 0.7 + 1.0 * s,
        spread_deg: 0.0,
        gravity: 0.0,
        source_radius: 0.10 + 0.20 * s,
        size_start: 0.10,
        size_end: 0.25,
        color_start: RIPPLE,
        color_end: RIPPLE_END,
        alpha: 0.28,
        turbulence: 0.0,
        burst: true,
        sequence: 0,
    }
}

/// The beads the crown's fingers break into: slower and a little bigger than
/// the fine spray, thrown from the crown's ring. The crown itself is a sheet
/// ([`entry_sheets`]); drawn as big soft drops it read as snow (2026-11-16).
#[must_use]
pub fn entry_crown(size: f32) -> ParticleEffect {
    let s = size.clamp(0.0, 1.0);
    let speed = 1.2 + 2.4 * s;
    ParticleEffect {
        count: (120.0 + 130.0 * s) as u16,
        life: flight_secs(speed),
        speed,
        spread_deg: 34.0,
        gravity: DROPLET_GRAVITY,
        source_radius: 0.30 + 0.40 * s,
        size_start: 0.030 + 0.016 * s,
        size_end: 0.02,
        color_start: SPRAY,
        color_end: SPRAY_END,
        alpha: 0.75,
        turbulence: 0.08,
        burst: true,
        sequence: 0,
    }
}

/// Drops flung low and wide off the crown's outside, landing a metre or two
/// out: what makes the photo's splash spill over the water round it.
#[must_use]
pub fn entry_fling(size: f32) -> ParticleEffect {
    let s = size.clamp(0.0, 1.0);
    let speed = 1.8 + 2.6 * s;
    ParticleEffect {
        count: (90.0 + 150.0 * s) as u16,
        life: flight_secs(speed * 0.6),
        speed,
        spread_deg: 62.0,
        gravity: DROPLET_GRAVITY,
        source_radius: 0.25 + 0.35 * s,
        size_start: 0.018 + 0.010 * s,
        size_end: 0.014,
        color_start: SPRAY,
        color_end: SPRAY_END,
        alpha: 0.8,
        turbulence: 0.05,
        burst: true,
        sequence: 0,
    }
}

/// The little throw of water as the hands shoot forward.
#[must_use]
pub fn stroke_spray() -> ParticleEffect {
    let speed = 1.3;
    ParticleEffect {
        count: 30,
        life: flight_secs(speed),
        speed,
        spread_deg: 40.0,
        gravity: DROPLET_GRAVITY,
        source_radius: 0.30,
        size_start: 0.03,
        size_end: 0.02,
        color_start: SPRAY,
        color_end: SPRAY_END,
        alpha: 0.7,
        turbulence: 0.03,
        burst: true,
        sequence: 0,
    }
}

/// The ripple a stroke leaves round the head and hands: a few rings born over
/// the width of the reach.
#[must_use]
pub fn stroke_rings() -> ParticleEffect {
    ParticleEffect {
        count: 2,
        life: 2.0,
        speed: 0.45,
        spread_deg: 0.0,
        gravity: 0.0,
        source_radius: 0.35,
        size_start: 0.16,
        size_end: 0.20,
        color_start: RIPPLE,
        color_end: RIPPLE_END,
        alpha: 0.18,
        turbulence: 0.0,
        burst: true,
        sequence: 0,
    }
}

/// Treading water: gentle rings, all the time, round a body that stays put.
#[must_use]
pub fn tread_rings() -> ParticleEffect {
    ParticleEffect {
        count: 3,
        life: 2.4,
        speed: 0.35,
        spread_deg: 0.0,
        gravity: 0.0,
        source_radius: 0.25,
        size_start: 0.16,
        size_end: 0.18,
        color_start: RIPPLE,
        color_end: RIPPLE_END,
        alpha: 0.18,
        turbulence: 0.0,
        burst: false,
        sequence: 0,
    }
}

/// A fountain jet's plume: thrown up from the top of the column and falling
/// back round it into the water.
#[must_use]
pub fn jet_spray() -> ParticleEffect {
    let speed = 2.6;
    ParticleEffect {
        count: 256,
        // Up and back down past the column's top to the water under it.
        life: (speed + (speed * speed + 2.0 * -DROPLET_GRAVITY * JET_TOP_M).sqrt())
            / -DROPLET_GRAVITY,
        speed,
        spread_deg: 14.0,
        gravity: DROPLET_GRAVITY,
        source_radius: 0.08,
        size_start: 0.05,
        size_end: 0.07,
        color_start: SPRAY,
        color_end: SPRAY_END,
        alpha: 0.8,
        turbulence: 0.08,
        burst: false,
        sequence: 0,
    }
}

/// Where a jet's water comes down: rings on the pond round its foot.
#[must_use]
pub fn jet_rings() -> ParticleEffect {
    ParticleEffect {
        count: 4,
        life: 2.2,
        speed: 0.5,
        spread_deg: 0.0,
        gravity: 0.0,
        source_radius: 0.6,
        size_start: 0.16,
        size_end: 0.20,
        color_start: RIPPLE,
        color_end: RIPPLE_END,
        alpha: 0.18,
        turbulence: 0.0,
        burst: false,
        sequence: 0,
    }
}

// ---------------------------------------------------------------------------
// The events.
// ---------------------------------------------------------------------------

/// Something touched the water this frame. Read by the drawing and by the
/// sound; both decide for themselves whether they are on.
#[derive(Message, Clone, Copy, Debug)]
pub struct Splash {
    /// On the water's surface, anchored world metres.
    pub at: Vec3,
    pub kind: SplashKind,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SplashKind {
    /// A body went in, at this size (0..1).
    Entry { size: f32 },
    /// A swimmer's stroke.
    Stroke,
    /// A swimmer went into a fountain's jet.
    Jet,
}

/// What [`detect_splashes`] remembers of each body between frames.
#[derive(Clone, Copy, Default)]
pub struct Track {
    swimming: bool,
    feet_y: f32,
    phase: Option<f32>,
    in_jet: bool,
    /// A body that is in the water but whose feet have not yet gone through
    /// its surface: the splash's size, and frames waited.
    entering: Option<(f32, u8)>,
}

/// The feet this close over the surface (m) are through it.
const THROUGH_M: f32 = 0.05;
/// A body whose feet never reach the surface (a remote drawn without the
/// water's lift) splashes after this many frames anyway.
const THROUGH_WAIT_FRAMES: u8 = 45;

/// A body this frame, in the terms the detection needs.
struct Body {
    entity: Entity,
    /// Feet, anchored world metres.
    feet: Vec3,
    /// Across the ground, m/s.
    speed: f32,
    swimming: bool,
    /// Which way the body faces (yaw, radians; forward is `(sin, cos)` in XZ).
    yaw: f32,
}

/// How far ahead of the feet a stroke's head and hands meet the water (m): the
/// swimmer lies along its facing (`avatar::swim_lie`).
const STROKE_AHEAD_M: f32 = 0.9;

/// Find the splashes: bodies going in, strokes, swimmers reaching a jet.
pub fn detect_splashes(
    avatar_state: Res<AvatarState>,
    terrain: Res<TerrainField>,
    places: Res<WorldPlaces>,
    local: Query<(Entity, Option<&AvatarAnimations>), With<Avatar>>,
    remotes: Query<(Entity, &RemoteAvatar, &Transform, Option<&AvatarAnimations>)>,
    children: Query<&Children>,
    players: Query<(&AnimationPlayer, &AnimationTransitions)>,
    mut tracks: Local<HashMap<Entity, Track>>,
    mut out: MessageWriter<Splash>,
) {
    let mut bodies: Vec<(Body, Option<&AvatarAnimations>)> = Vec::new();
    if let Ok((entity, anims)) = local.single() {
        bodies.push((
            Body {
                entity,
                feet: avatar_state.display_position,
                speed: avatar_state.stride_speed(),
                swimming: avatar_state.swimming,
                yaw: avatar_state.facing_yaw,
            },
            anims,
        ));
    }
    for (entity, remote, tf, anims) in &remotes {
        bodies.push((
            Body {
                entity,
                feet: tf.translation,
                speed: Vec2::new(remote.net_velocity.x, remote.net_velocity.z).length(),
                swimming: remote.swimming,
                yaw: remote.net_yaw,
            },
            anims,
        ));
    }

    let mut seen: Vec<Entity> = Vec::with_capacity(bodies.len());
    for (body, anims) in bodies {
        seen.push(body.entity);
        let flat = Vec2::new(body.feet.x, body.feet.z);
        // The stroke's phase, only for a swimmer: walking the hierarchy for the
        // player is wasted on everyone standing on a street.
        let phase = if body.swimming {
            anims.and_then(|a| {
                children
                    .iter_descendants(body.entity)
                    .find_map(|c| players.get(c).ok().and_then(|(p, t)| a.swim_phase(p, t)))
            })
        } else {
            None
        };
        let surface = if body.swimming {
            terrain.swim_surface_at(flat.x, flat.y)
        } else {
            None
        };
        let in_jet = surface.is_some()
            && places
                .jets()
                .any(|j| j.distance_squared(flat) < JET_RADIUS_M * JET_RADIUS_M);

        let Some(prev) = tracks.get(&body.entity).copied() else {
            // First sight of this body: it is wherever it is, and did not
            // just arrive there.
            tracks.insert(
                body.entity,
                Track {
                    swimming: body.swimming,
                    feet_y: body.feet.y,
                    phase,
                    in_jet,
                    entering: None,
                },
            );
            continue;
        };

        let mut entering = None;
        if let Some(surface) = surface {
            // On the water as it is *drawn*: the sheet is never below the
            // ground under it (`map_geometry`'s water clamp), and beside a quay
            // that ground can be the bank's, a metre or more over the level —
            // a splash on the level there was buried (Noorderhaven, 2026-11-16).
            let drawn = surface.max(terrain.sunk_at(flat.x, flat.y));
            let on_water = Vec3::new(flat.x, drawn, flat.y);
            entering = prev.entering;
            if !prev.swimming {
                // The drop is from where the feet were *before* this frame:
                // the lift into the water eases, and a capture flown frame by
                // frame has already put them at the water by now.
                let size = splash_size(entry_speed(prev.feet_y - surface, body.speed));
                tracing::debug!(
                    target: "cartopolis::splashes",
                    x = flat.x,
                    z = flat.y,
                    drop = prev.feet_y - surface,
                    speed = body.speed,
                    size,
                    "splash: in"
                );
                entering = Some((size, 0));
            }
            // The splash is where the feet go through the surface, not where
            // the body crossed into the water: off a quay that is the wall's
            // own line, and half a splash there is drawn inside the wall
            // (2026-11-16). The lift sinks the feet at 4 m/s, so a walk off a
            // 1.4 m quay goes through about half a metre out.
            if let Some((size, waited)) = entering {
                if body.feet.y <= drawn + THROUGH_M || waited >= THROUGH_WAIT_FRAMES {
                    tracing::debug!(
                        target: "cartopolis::splashes",
                        x = flat.x,
                        z = flat.y,
                        size,
                        waited,
                        // The register's level the body swims at, and the height
                        // the sheet is drawn at here, which the splash stands on.
                        swim_level = surface,
                        drawn,
                        "splash: through"
                    );
                    out.write(Splash {
                        at: on_water,
                        kind: SplashKind::Entry { size },
                    });
                    entering = None;
                } else {
                    entering = Some((size, waited + 1));
                }
            }
            if let (Some(a), Some(b)) = (prev.phase, phase) {
                if crossed(a, b, SWIM_STROKE_PHASE) {
                    let ahead = Vec2::new(body.yaw.sin(), body.yaw.cos()) * STROKE_AHEAD_M;
                    tracing::debug!(target: "cartopolis::splashes", x = flat.x, z = flat.y, "splash: stroke");
                    out.write(Splash {
                        at: on_water + Vec3::new(ahead.x, 0.0, ahead.y),
                        kind: SplashKind::Stroke,
                    });
                }
            }
            if in_jet && !prev.in_jet {
                tracing::debug!(target: "cartopolis::splashes", x = flat.x, z = flat.y, "splash: jet");
                out.write(Splash {
                    at: on_water,
                    kind: SplashKind::Jet,
                });
            }
        }
        tracks.insert(
            body.entity,
            Track {
                swimming: body.swimming,
                feet_y: body.feet.y,
                phase,
                in_jet,
                entering,
            },
        );
    }
    tracks.retain(|e, _| seen.contains(e));
}

// ---------------------------------------------------------------------------
// The drawing.
// ---------------------------------------------------------------------------

/// Burst emitters, re-armed in turn: two a stroke (droplets and ripples),
/// four a body going in (spray, the crown's beads, the low fling, ripples),
/// so ten or more splashes on the water at once — a stroke's lasts two
/// seconds. Each is one draw of the shared pool and nothing uploaded; the
/// sheets ([`SHEET_SLOTS`]) are the rest of the cost.
const BURST_SLOTS: usize = 40;
/// Swimmers treading water drawn with rings at once.
const TREAD_SLOTS: usize = 8;
/// Fountain jets sprayed at once: the nearest to the camera.
const JET_SLOTS: usize = 6;
/// Past this a jet's spray is not drawn (m from the camera).
const JET_DRAW_M: f32 = 300.0;
/// The rings ride this far over the water (m), so the surface does not cut
/// through them where it is drawn a hair high.
const RING_LIFT_M: f32 = 0.10;

/// Marks the water's emitters (not a prim's).
#[derive(Component)]
pub struct WaterEmitter;

struct BurstSlot {
    entity: Entity,
    material: Handle<ParticleMaterial>,
    /// When it has finished, on the animation clock.
    ends: f32,
}

/// The emitters, built once the quad pool and the world's grid exist.
#[derive(Resource, Default)]
pub struct WaterFx {
    bursts: Vec<BurstSlot>,
    next: usize,
    tread: Vec<Entity>,
    jets: Vec<(Entity, Entity)>,
    /// The sheets of water a body going in throws up: crown, column, foam.
    sheets: Vec<[SheetSlot; 3]>,
    next_sheet: usize,
}

struct SheetSlot {
    entity: Entity,
    material: Handle<SplashMaterial>,
    ends: f32,
}

/// Sets of sheets (crown, column, foam) at once: six bodies going in within
/// the life of one, which is a crowd jumping off a quay together.
const SHEET_SLOTS: usize = 6;
/// The sheet mesh: segments round, rows up. 64 × 12 is 845 vertices and
/// 1,536 triangles, shared by every sheet and built once; the fine detail is
/// cut per pixel, so more vertices would buy nothing but the broad shape.
const SHEET_SEGMENTS: usize = 64;
const SHEET_ROWS: usize = 12;

/// The grid every splash sheet is drawn from: `position` (cos θ, v, sin θ),
/// `uv` (θ / 2π, v). The shape is all in `splash.wesl`.
fn sheet_mesh() -> Mesh {
    use bevy::asset::RenderAssetUsages;
    use bevy::mesh::{Indices, PrimitiveTopology};
    let (n, m) = (SHEET_SEGMENTS, SHEET_ROWS);
    let mut positions = Vec::with_capacity((n + 1) * (m + 1));
    let mut uvs = Vec::with_capacity((n + 1) * (m + 1));
    for j in 0..=m {
        let v = j as f32 / m as f32;
        for i in 0..=n {
            let u = i as f32 / n as f32;
            let a = u * std::f32::consts::TAU;
            positions.push([a.cos(), v, a.sin()]);
            uvs.push([u, v]);
        }
    }
    let mut indices = Vec::with_capacity(n * m * 6);
    for j in 0..m {
        for i in 0..n {
            let a = (j * (n + 1) + i) as u32;
            let b = a + (n + 1) as u32;
            indices.extend_from_slice(&[a, b, a + 1, a + 1, b, b + 1]);
        }
    }
    Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::RENDER_WORLD,
    )
    .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions)
    .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, uvs)
    .with_inserted_indices(Indices::U32(indices))
}

/// The three sheets of a body going in at `size` (0..1), started at `start`
/// on the shader clock, with a `seed` for the ragged edges: a crown rising
/// in a quarter second and collapsing over about half a second, a column up
/// the middle only for a real drop (it is what a feet-first entry from
/// height sends up), and foam left on the surface.
#[must_use]
pub fn entry_sheets(size: f32, start: f32, seed: f32) -> [SplashMaterial; 3] {
    let s = size.clamp(0.0, 1.0);
    let column = ((s - 0.4) / 0.6).clamp(0.0, 1.0);
    [
        // Wider than it is high, as in the photo: about two metres across and
        // a metre high for a drop off a quay, knee-high for a wade.
        SplashMaterial::new(
            SplashShape::Crown,
            start,
            0.20 + 0.08 * s,
            0.45 + 0.25 * s,
            0.22 + 0.20 * s,
            0.28 + 0.50 * s,
            0.35 + 0.95 * s,
            seed,
        ),
        SplashMaterial::new(
            SplashShape::Column,
            start + 0.10,
            0.26,
            0.40,
            0.10 + 0.05 * s,
            0.12,
            // Nothing to draw without a drop: a zero height is a flat ring.
            (0.9 + 1.4 * s) * column,
            seed + 0.37,
        ),
        SplashMaterial::new(
            SplashShape::Foam,
            start + 0.05,
            0.01,
            1.6 + 1.4 * s,
            0.40 + 0.30 * s,
            0.50 + 0.90 * s,
            0.0,
            seed + 0.71,
        ),
    ]
}

fn spawn_emitter<M: Material>(
    commands: &mut Commands,
    root: Entity,
    mesh: &Handle<Mesh>,
    material: Handle<M>,
) -> Entity {
    commands
        .spawn((
            Mesh3d(mesh.clone()),
            MeshMaterial3d(material),
            Transform::default(),
            GridCell::default(),
            ChildOf(root),
            Visibility::Hidden,
            // The quads are placed in the vertex stage, metres from the mesh's
            // own vertices; see `particles::sync_particle_emitters`, which has
            // both reasons for these two.
            bevy::camera::visibility::NoFrustumCulling,
            bevy::light::NotShadowCaster,
            WaterEmitter,
        ))
        .id()
}

/// The world the drawing reads, bundled for the parameter cap.
#[derive(bevy::ecs::system::SystemParam)]
pub struct WaterScene<'w, 's> {
    prefs: Res<'w, AppPrefs>,
    layers: Res<'w, crate::systems::layer_state::DataLayers>,
    avatar_state: Res<'w, AvatarState>,
    terrain: Res<'w, TerrainField>,
    places: Res<'w, WorldPlaces>,
    camera: crate::grid::AnchoredCamera<'w, 's>,
    remotes: Query<'w, 's, (&'static RemoteAvatar, &'static Transform), Without<WaterEmitter>>,
    local: Query<'w, 's, (), With<Avatar>>,
}

impl WaterScene<'_, '_> {
    /// Bodies treading water: in it and not going anywhere. Feet, anchored.
    ///
    /// `local_moved` is how far the own avatar was drawn moving since the last
    /// frame: a body carried without a pace of its own (a capture flown frame
    /// by frame, a GPS walk) is not treading water, and rings dragged along
    /// with it would say it was.
    fn treading(&self, local_moved: f32) -> Vec<Vec3> {
        let mut out = Vec::new();
        let s = &self.avatar_state;
        if !self.local.is_empty()
            && s.swimming
            && s.stride_speed() < crate::systems::locomotion::MOVING_EPS
            && local_moved < 0.02
        {
            out.push(s.display_position);
        }
        for (r, tf) in &self.remotes {
            let speed = Vec2::new(r.net_velocity.x, r.net_velocity.z).length();
            if r.swimming && speed < crate::systems::locomotion::MOVING_EPS {
                out.push(tf.translation);
            }
        }
        out
    }

    /// The water under a jet, or the ground where no level is registered.
    fn jet_base(&self, p: Vec2) -> f32 {
        self.terrain
            .water_surface_at(p.x, p.y)
            .unwrap_or_else(|| self.terrain.ground_at(p.x, p.y))
    }
}

/// Place one emitter at an anchored position, shown.
/// How far foam lying level at `at` stays on the water as drawn, in each of
/// eight directions (angle `i`/8 of a turn from +x towards +z, as the sheet
/// mesh runs). Beside a quay the drawn sheet climbs to the bank
/// (`map_geometry`'s clamp above the ground): level foam there cut into it
/// along straight lines, and foam tilted to follow it stood up as a white
/// wedge where the slope was read off the bank (Noorderhaven, 2026-11-16).
/// So the foam stays level and fades out before the sheet leaves it.
fn foam_reach(terrain: &TerrainField, at: Vec3) -> [f32; 8] {
    const STEP_M: f32 = 0.2;
    const MAX_M: f32 = 4.0;
    /// The sheet this far off the foam's level (m) is no longer under it.
    const OFF_M: f32 = 0.08;
    std::array::from_fn(|i| {
        let a = i as f32 * std::f32::consts::TAU / 8.0;
        let dir = Vec2::new(a.cos(), a.sin());
        let mut reach = 0.0;
        while reach < MAX_M {
            let p = Vec2::new(at.x, at.z) + dir * (reach + STEP_M);
            let Some(level) = terrain.swim_surface_at(p.x, p.y) else {
                break;
            };
            if (level.max(terrain.sunk_at(p.x, p.y)) - at.y).abs() > OFF_M {
                break;
            }
            reach += STEP_M;
        }
        reach.max(0.3)
    })
}

fn place(
    emitters: &mut Query<
        (&mut Transform, &mut GridCell, &mut Visibility),
        (With<WaterEmitter>, Without<FreeCamera>),
    >,
    grid: &Grid,
    entity: Entity,
    at: Option<Vec3>,
) {
    let Ok((mut tf, mut cell, mut vis)) = emitters.get_mut(entity) else {
        return;
    };
    match at {
        Some(p) => {
            let (c, local) = grid.translation_to_grid(p.as_dvec3());
            if *cell != c {
                *cell = c;
            }
            if tf.translation != local {
                tf.translation = local;
            }
            if *vis != Visibility::Inherited {
                *vis = Visibility::Inherited;
            }
        }
        None => {
            if *vis != Visibility::Hidden {
                *vis = Visibility::Hidden;
            }
        }
    }
}

/// Build the emitters, arm a burst for each splash, and keep the treading
/// rings and the jets' plumes where they belong.
pub fn draw_splashes(
    mut commands: Commands,
    pool: Option<Res<ParticlePool>>,
    mut materials: ResMut<Assets<ParticleMaterial>>,
    mut sheet_materials: ResMut<Assets<SplashMaterial>>,
    mut meshes: ResMut<Assets<Mesh>>,
    clock: Res<AnimClock>,
    roots: Query<(Entity, &Grid), With<BigSpace>>,
    mut fx: ResMut<WaterFx>,
    mut splashes: MessageReader<Splash>,
    scene: WaterScene,
    mut emitters: Query<
        (&mut Transform, &mut GridCell, &mut Visibility),
        (With<WaterEmitter>, Without<FreeCamera>),
    >,
    mut last_local: Local<Option<Vec3>>,
) {
    let (Some(pool), Ok((root, grid))) = (pool, roots.single()) else {
        splashes.clear();
        return;
    };
    let now = clock.elapsed_secs();
    let shader_now = clock.elapsed_secs_wrapped();

    if fx.bursts.is_empty() {
        // Once. Every burst its own material, because each is armed at its own
        // moment; the treading rings and the jets share theirs, because they
        // are the same formula everywhere and only the place differs.
        let mesh = pool.mesh();
        for _ in 0..BURST_SLOTS {
            let material = materials.add(ParticleMaterial::from_effect(&entry_spray(0.0), -1.0e6));
            let entity = spawn_emitter(&mut commands, root, &mesh, material.clone());
            fx.bursts.push(BurstSlot {
                entity,
                material,
                ends: f32::MIN,
            });
        }
        let tread = materials.add(ParticleMaterial::rings(&tread_rings(), 0.0));
        fx.tread = (0..TREAD_SLOTS)
            .map(|_| spawn_emitter(&mut commands, root, &mesh, tread.clone()))
            .collect();
        let spray = materials.add(ParticleMaterial::droplets(&jet_spray(), 0.0));
        let rings = materials.add(ParticleMaterial::rings(&jet_rings(), 0.0));
        fx.jets = (0..JET_SLOTS)
            .map(|_| {
                (
                    spawn_emitter(&mut commands, root, &mesh, spray.clone()),
                    spawn_emitter(&mut commands, root, &mesh, rings.clone()),
                )
            })
            .collect();
        let sheet = meshes.add(sheet_mesh());
        fx.sheets = (0..SHEET_SLOTS)
            .map(|_| {
                entry_sheets(0.0, -1.0e6, 0.0).map(|m| {
                    let material = sheet_materials.add(m);
                    SheetSlot {
                        entity: spawn_emitter(&mut commands, root, &sheet, material.clone()),
                        material,
                        ends: f32::MIN,
                    }
                })
            })
            .collect();
        // Spawned by command: placed from the next frame.
        splashes.clear();
        return;
    }

    let on = scene.prefs.water_effects;
    let fx = &mut *fx;

    // Splashes: arm the next slots in turn. The oldest is the one reused, so a
    // burst is cut short only when two dozen have started since.
    for splash in splashes.read() {
        if !on {
            continue;
        }
        // Droplets and the ripples; where a body went in, also the drops shed
        // off the crown and the sheets of water (crown, column, foam) below.
        let entry = match splash.kind {
            SplashKind::Entry { size } => Some(size),
            SplashKind::Jet => Some(0.35),
            SplashKind::Stroke => None,
        };
        let parts: Vec<(ParticleEffect, bool)> = match entry {
            Some(size) => vec![
                (entry_spray(size), false),
                (entry_crown(size), false),
                (entry_fling(size), false),
                (entry_rings(size), true),
            ],
            None => vec![(stroke_spray(), false), (stroke_rings(), true)],
        };
        if let Some(size) = entry {
            let i = fx.next_sheet;
            fx.next_sheet = (fx.next_sheet + 1) % fx.sheets.len().max(1);
            // A seed from the place, so two splashes are not the same shape.
            let seed = (splash.at.x * 0.731 + splash.at.z * 0.377).fract().abs();
            let reach = foam_reach(&scene.terrain, splash.at);
            let sheets = entry_sheets(size, shader_now, seed);
            for (slot, mut m) in fx.sheets[i].iter_mut().zip(sheets) {
                slot.ends = now + (m.timing.x - shader_now) + m.life();
                // The foam lies on the water; the crown and column rise from it.
                let foam = m.timing.w > 1.5;
                let lift = if foam { RING_LIFT_M } else { 0.0 };
                if foam {
                    m = m.with_reach(reach);
                }
                if let Some(mut mat) = sheet_materials.get_mut(&slot.material) {
                    *mat = m;
                }
                place(
                    &mut emitters,
                    grid,
                    slot.entity,
                    Some(splash.at + Vec3::Y * lift),
                );
            }
        }
        for (effect, flat) in parts {
            let i = fx.next;
            fx.next = (fx.next + 1) % fx.bursts.len();
            let slot = &mut fx.bursts[i];
            if let Some(mut m) = materials.get_mut(&slot.material) {
                *m = if flat {
                    ParticleMaterial::rings(&effect, shader_now)
                } else {
                    ParticleMaterial::droplets(&effect, shader_now)
                };
            }
            slot.ends = now + effect.life;
            let lift = if flat { RING_LIFT_M } else { 0.0 };
            place(
                &mut emitters,
                grid,
                slot.entity,
                Some(splash.at + Vec3::Y * lift),
            );
        }
    }
    // A burst that has run its course is hidden: its quads have collapsed to
    // nothing already, and a hidden entity is not drawn at all.
    for slot in &fx.bursts {
        if !on || now > slot.ends {
            place(&mut emitters, grid, slot.entity, None);
        }
    }
    for slot in fx.sheets.iter().flatten() {
        if !on || now > slot.ends {
            place(&mut emitters, grid, slot.entity, None);
        }
    }

    // Treading water: one emitter per body that is, on the water at its feet.
    let here = scene.avatar_state.display_position;
    let moved = last_local.map_or(0.0, |p| p.distance(here));
    *last_local = Some(here);
    let treading = if on {
        scene.treading(moved)
    } else {
        Vec::new()
    };
    for (i, entity) in fx.tread.iter().enumerate() {
        let at = treading.get(i).and_then(|feet| {
            // On the sheet as drawn, as a splash is (`detect_splashes`).
            let s = scene
                .terrain
                .swim_surface_at(feet.x, feet.z)?
                .max(scene.terrain.sunk_at(feet.x, feet.z));
            Some(Vec3::new(feet.x, s + RING_LIFT_M, feet.z))
        });
        place(&mut emitters, grid, *entity, at);
    }

    // The jets nearest the camera, while the street furniture is drawn: the
    // plume belongs to the column the furniture layer draws.
    let mut jets: Vec<(f32, Vec2)> = Vec::new();
    if on && scene.layers.furniture_enabled {
        if let Some((cell, tf)) = scene.camera.get() {
            let eye = crate::grid::anchored_xz(cell, tf.translation);
            jets = scene
                .places
                .jets()
                .map(|j| (j.distance(eye), j))
                .filter(|(d, _)| *d < JET_DRAW_M)
                .collect();
            jets.sort_by(|a, b| a.0.total_cmp(&b.0));
        }
    }
    for (i, (spray, rings)) in fx.jets.iter().enumerate() {
        let base = jets
            .get(i)
            .map(|(_, j)| Vec3::new(j.x, scene.jet_base(*j), j.y));
        place(
            &mut emitters,
            grid,
            *spray,
            base.map(|b| b + Vec3::Y * JET_TOP_M),
        );
        place(
            &mut emitters,
            grid,
            *rings,
            base.map(|b| b + Vec3::Y * RING_LIFT_M),
        );
    }
}

// ---------------------------------------------------------------------------
// The sound.
// ---------------------------------------------------------------------------

#[cfg(feature = "audio")]
pub use playback::*;

#[cfg(feature = "audio")]
mod playback {
    use super::*;
    use bevy::audio::{
        AudioPlayer, AudioSink, AudioSource, PlaybackMode, PlaybackSettings, Volume,
    };

    use crate::resources::{CameraMode, CameraState};

    /// Takes on disk under `assets/sounds/water/` (`LICENSE.md` there has
    /// where each is from).
    pub const BIG_TAKES: usize = 3;
    pub const SMALL_TAKES: usize = 3;
    pub const STROKE_TAKES: usize = 6;

    /// A stroke against a splash: a hand through the surface, not a body.
    const STROKE_VOLUME: f32 = 0.28;
    /// The loops are −20 LUFS like the city's beds; these set them under it.
    const TREAD_VOLUME: f32 = 0.45;
    const FOUNTAIN_VOLUME: f32 = 0.6;
    /// How fast a loop follows its level, per second.
    const LOOP_RATE: f32 = 1.5;
    /// No more than this many one-shots start in one frame: a crowd going in at
    /// once is not a wall of sound.
    const MAX_PER_FRAME: usize = 3;

    #[derive(Resource)]
    pub struct WaterSounds {
        big: Vec<Handle<AudioSource>>,
        small: Vec<Handle<AudioSource>>,
        strokes: Vec<Handle<AudioSource>>,
        pick: usize,
    }

    #[derive(Component)]
    pub struct TreadLoop;
    #[derive(Component)]
    pub struct FountainLoop;

    /// Load the takes and start both loops, silent, for the ambience's reason:
    /// a loop started when it is wanted has an attack.
    pub fn setup_water_sounds(mut commands: Commands, assets: Res<AssetServer>) {
        let takes = |stem: &str, n: usize| -> Vec<Handle<AudioSource>> {
            (1..=n)
                .map(|k| assets.load(format!("sounds/water/{stem}-{k}.ogg")))
                .collect()
        };
        commands.insert_resource(WaterSounds {
            big: takes("splash-big", BIG_TAKES),
            small: takes("splash-small", SMALL_TAKES),
            strokes: takes("stroke", STROKE_TAKES),
            pick: 0,
        });
        for (file, marker) in [("tread", 0), ("fountain", 1)] {
            let player = (
                AudioPlayer::<AudioSource>(assets.load(format!("sounds/water/{file}.ogg"))),
                PlaybackSettings {
                    mode: PlaybackMode::Loop,
                    volume: Volume::Linear(0.0),
                    ..default()
                },
            );
            if marker == 0 {
                commands.spawn((player, TreadLoop));
            } else {
                commands.spawn((player, FountainLoop));
            }
        }
    }

    /// Where the listener is: the avatar, while the camera follows it. The
    /// other modes look at the map from above, and a splash heard there is a
    /// poltergeist (`ambience::update_bed_volumes`).
    fn ear(camera: &CameraState, avatar: &AvatarState) -> Option<Vec3> {
        (camera.mode == CameraMode::Avatar).then_some(avatar.display_position)
    }

    /// Play each splash and stroke within earshot.
    pub fn play_splash_sounds(
        mut commands: Commands,
        camera: Res<CameraState>,
        avatar: Res<AvatarState>,
        prefs: Res<AppPrefs>,
        clock: Res<AnimClock>,
        sources: Res<Assets<AudioSource>>,
        mut sounds: ResMut<WaterSounds>,
        mut splashes: MessageReader<Splash>,
    ) {
        let Some(ear) = ear(&camera, &avatar) else {
            splashes.clear();
            return;
        };
        let gain = prefs.effects_gain();
        let mut started = 0;
        for splash in splashes.read() {
            if started >= MAX_PER_FRAME {
                break;
            }
            let level = water_level(splash.at.distance(ear), SPLASH_NEAR_M, SPLASH_REACH_M);
            if level <= 0.0 {
                continue;
            }
            sounds.pick = sounds.pick.wrapping_add(1);
            let pick = sounds.pick;
            let (takes, volume, what) = match splash.kind {
                SplashKind::Entry { size } => {
                    let (big, v) = splash_sound(size);
                    if big {
                        (&sounds.big, v, "big")
                    } else {
                        (&sounds.small, v, "small")
                    }
                }
                SplashKind::Jet => (&sounds.small, splash_sound(0.35).1, "small"),
                SplashKind::Stroke => (&sounds.strokes, STROKE_VOLUME, "stroke"),
            };
            let Some(clip) = takes
                .get(pick % takes.len().max(1))
                .filter(|h| sources.contains(*h))
            else {
                continue;
            };
            let volume = volume * level * gain;
            // What a recorded soundtrack is rebuilt from, as the ambience's
            // samples are (`RUST_LOG=cartopolis::splashes=debug`); `clock` is
            // a `--video-fps` movie's own time.
            tracing::debug!(
                target: "cartopolis::splashes",
                clock = clock.elapsed_secs(),
                sound = what,
                take = pick % takes.len().max(1) + 1,
                volume,
                "splash sound"
            );
            commands.spawn((
                AudioPlayer(clip.clone()),
                PlaybackSettings {
                    mode: PlaybackMode::Despawn,
                    volume: Volume::Linear(volume),
                    // A little apart each time, as the footsteps are.
                    speed: 0.94 + 0.12 * ((pick as f32 * 0.618_034).fract()),
                    ..default()
                },
            ));
            started += 1;
        }
    }

    /// The loops' levels, eased: the water round a swimmer and the nearest
    /// fountain jet.
    #[allow(clippy::too_many_arguments)]
    pub fn update_water_loops(
        time: Res<Time>,
        camera: Res<CameraState>,
        avatar: Res<AvatarState>,
        prefs: Res<AppPrefs>,
        places: Res<WorldPlaces>,
        remotes: Query<(&RemoteAvatar, &Transform)>,
        mut tread: Query<&mut AudioSink, (With<TreadLoop>, Without<FountainLoop>)>,
        mut fountain: Query<&mut AudioSink, (With<FountainLoop>, Without<TreadLoop>)>,
        clock: Res<AnimClock>,
        mut levels: Local<[f32; 2]>,
        mut logged: Local<f32>,
    ) {
        let (tread_target, fountain_target) = match ear(&camera, &avatar) {
            Some(ear) => {
                let own: f32 = if avatar.swimming { 1.0 } else { 0.0 };
                let others = remotes
                    .iter()
                    .filter(|(r, _)| r.swimming)
                    .map(|(_, tf)| water_level(tf.translation.distance(ear), 2.0, TREAD_REACH_M))
                    .fold(0.0f32, f32::max);
                let flat = Vec2::new(ear.x, ear.z);
                let jets = places
                    .jets()
                    .map(|j| water_level(j.distance(flat), FOUNTAIN_NEAR_M, FOUNTAIN_REACH_M))
                    .fold(0.0f32, f32::max);
                (own.max(others), jets)
            }
            None => (0.0, 0.0),
        };
        let k = 1.0 - (-LOOP_RATE * time.delta_secs()).exp();
        levels[0] += (tread_target - levels[0]) * k;
        levels[1] += (fountain_target - levels[1]) * k;
        let gain = prefs.effects_gain();
        if let Ok(mut sink) = tread.single_mut() {
            sink.set_volume(Volume::Linear(levels[0] * TREAD_VOLUME * gain));
        }
        if let Ok(mut sink) = fountain.single_mut() {
            sink.set_volume(Volume::Linear(levels[1] * FOUNTAIN_VOLUME * gain));
        }
        // Four times a second on the movie's clock, for a rebuilt soundtrack.
        let now = clock.elapsed_secs();
        if (now - *logged).abs() >= 0.25 {
            *logged = now;
            tracing::debug!(
                target: "cartopolis::splashes",
                clock = now,
                tread = levels[0] * TREAD_VOLUME * gain,
                fountain = levels[1] * FOUNTAIN_VOLUME * gain,
                "water loops"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::math::loop_phase;

    #[test]
    fn a_drop_off_a_quay_splashes_bigger_than_wading_in() {
        // Walking down a bank into a pond: no drop, a walk.
        let wade = splash_size(entry_speed(0.0, 1.4));
        // Running in off the same bank.
        let run_in = splash_size(entry_speed(0.0, 3.5));
        // Walking off a 1.5 m quay.
        let quay = splash_size(entry_speed(1.5, 1.4));
        assert!(wade < 0.1, "a wade is {wade}");
        assert!(run_in > wade && run_in < quay, "{wade} {run_in} {quay}");
        assert!(quay > 0.9, "a quay drop is {quay}");
        // Nothing past the plunge, and nothing below the wade.
        assert_eq!(splash_size(entry_speed(100.0, 50.0)), 1.0);
        assert_eq!(splash_size(entry_speed(0.0, 0.0)), 0.0);
    }

    #[test]
    fn the_entry_speed_is_the_fall_and_the_run_together() {
        let fall = (2.0 * motion::GRAVITY * 1.0f32).sqrt();
        assert!((entry_speed(1.0, 0.0) - fall).abs() < 1e-4);
        assert!((entry_speed(0.0, 3.0) - 3.0).abs() < 1e-4);
        assert!((entry_speed(1.0, 3.0) - fall.hypot(3.0)).abs() < 1e-4);
        // Climbing *up* into water (a lift easing from below) is no fall,
        // and garbage is no splash.
        assert_eq!(entry_speed(-2.0, 0.0), 0.0);
        assert_eq!(entry_speed(f32::NAN, f32::INFINITY), 0.0);
    }

    #[test]
    fn a_bigger_splash_throws_more_water_higher_and_spreads_wider() {
        let (small, big) = (entry_spray(0.0), entry_spray(1.0));
        assert!(big.count > small.count && big.speed > small.speed);
        assert!(entry_rings(1.0).reach_m() > entry_rings(0.0).reach_m());
        for e in [
            small,
            big,
            entry_rings(0.5),
            entry_crown(1.0),
            entry_fling(1.0),
            stroke_spray(),
            stroke_rings(),
            tread_rings(),
            jet_spray(),
            jet_rings(),
        ] {
            assert_eq!(e, e.sanitized(), "every effect is already in range: {e:?}");
        }
        // Droplets come back down: a spray that rises forever is smoke.
        assert!(big.gravity < 0.0 && jet_spray().gravity < 0.0);
    }

    /// The crown rises in about a quarter second and has fallen back within
    /// about a second; the column comes only with a real drop, and the foam
    /// outlasts both.
    #[test]
    fn a_crown_rises_fast_and_falls_and_only_a_drop_sends_up_a_column() {
        for s in [0.0, 0.5, 1.0] {
            let [crown, column, foam] = entry_sheets(s, 10.0, 0.3);
            assert!(
                (0.15..0.32).contains(&crown.timing.y),
                "rise {}",
                crown.timing.y
            );
            assert!(crown.life() < 1.1, "crown lasts {}", crown.life());
            assert!(foam.life() > crown.life());
            assert!(
                column.timing.x > crown.timing.x,
                "the column follows the crown"
            );
            assert!(crown.shape.z > 0.0);
        }
        assert_eq!(
            entry_sheets(0.3, 0.0, 0.0)[1].shape.z,
            0.0,
            "a wade has no column"
        );
        assert!(entry_sheets(1.0, 0.0, 0.0)[1].shape.z > entry_sheets(1.0, 0.0, 0.0)[0].shape.z);
    }

    /// Ripples are a faint disturbance of the reflection — a few at a time,
    /// low in contrast and widening — not lines drawn on the water, which is
    /// what the first, crisp white rings looked like (2026-11-16).
    #[test]
    fn ripples_are_few_faint_and_widen() {
        for r in [entry_rings(1.0), stroke_rings(), tread_rings(), jet_rings()] {
            assert!(r.count <= 4, "{} rings at once", r.count);
            assert!(r.alpha <= 0.3, "a ring at alpha {}", r.alpha);
            assert!(r.size_end > r.size_start, "the band narrows: {r:?}");
            assert!(r.color_start.iter().all(|c| *c < 0.6), "a white ring");
        }
        // Droplets are small: a spray of blobs as big as a hand is snow.
        for d in [entry_spray(1.0), stroke_spray(), jet_spray()] {
            assert!(d.size_start <= 0.07, "a drop {} m across", d.size_start);
        }
    }

    #[test]
    fn a_stroke_is_heard_once_a_loop_where_the_hands_come_forward() {
        let at = SWIM_STROKE_PHASE;
        // Thirty frames a second through two strokes of 1.76 s.
        let secs = 1.76;
        let mut strokes = 0;
        let mut prev = loop_phase(0.0, secs);
        for f in 1..=(2.0 * secs * 30.0) as usize {
            let now = loop_phase(f as f32 / 30.0, secs);
            if crossed(prev, now, at) {
                strokes += 1;
                assert!((now - at).rem_euclid(1.0) < 0.05, "late: {now}");
            }
            prev = now;
        }
        assert_eq!(strokes, 2);
        // Across the loop's end, and not when it stands still.
        assert!(crossed(0.9, 0.05, 0.95));
        assert!(crossed(0.9, 0.05, 0.02));
        assert!(!crossed(0.9, 0.05, 0.5));
        assert!(!crossed(0.4, 0.4, 0.4));
        assert!(crossed(0.94, 0.96, 0.95) && !crossed(0.96, 0.99, 0.95));
    }

    #[test]
    fn the_phase_is_the_seek_over_the_loop() {
        assert!((loop_phase(0.88, 1.76) - 0.5).abs() < 1e-5);
        assert!((loop_phase(1.76 * 3.0 + 0.44, 1.76) - 0.25).abs() < 1e-4);
        assert_eq!(loop_phase(1.0, 0.0), 0.0);
    }

    #[test]
    fn a_fountain_fades_with_distance_and_stops_at_its_reach() {
        let at = |d| water_level(d, FOUNTAIN_NEAR_M, FOUNTAIN_REACH_M);
        assert_eq!(at(0.0), 1.0);
        assert_eq!(at(FOUNTAIN_NEAR_M), 1.0);
        // A point source past the near field: half the level at twice the
        // distance, before the fade begins.
        assert!((at(2.0 * FOUNTAIN_NEAR_M) - 0.5).abs() < 1e-4);
        let mut last = 1.0;
        for d in 1..=70 {
            let l = at(d as f32);
            assert!(l <= last + 1e-6, "louder at {d} m");
            last = l;
        }
        assert_eq!(at(FOUNTAIN_REACH_M), 0.0);
        assert!(at(FOUNTAIN_REACH_M - 0.5) < 0.01, "no step at the edge");
        assert_eq!(at(f32::NAN), 0.0);
    }

    #[test]
    fn a_plunge_plays_the_big_takes_louder() {
        let (big, loud) = splash_sound(1.0);
        let (small, quiet) = splash_sound(0.0);
        assert!(big && !small && loud > quiet);
        assert!(loud <= 1.0);
    }
}
