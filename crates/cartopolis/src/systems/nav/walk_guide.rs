//! Walking a planned route **in the world** (`docs/mockups/routing/v1`, frames
//! E and G).
//!
//! Driver mode is navigation along a real road with a device in your hand: it
//! strips the map, hides the avatar behind an arrow and locks a chase camera,
//! which is right at 50 km/h and wrong for a walk through a city somebody built
//! to be looked at. So Walk keeps everything — the avatar, the buildings, the
//! people, the chat, the follow camera — and adds two small boxes: the next turn
//! at the top, and what is left with what you can do about it at the bottom.
//!
//! **Three ways along the route, all of them the avatar's own movement.**
//!
//! - *Walk*: you walk it. The route is drawn in the street and the card counts
//!   down to the next turn.
//! - *Walk me there*: the avatar follows the route by itself, steered through
//!   [`RouteWalk`] — the same channel an agent's walk uses, so it runs through the
//!   avatar's ramp, collision and (online) the intent a person's walk does, and
//!   anybody at the keys wins for that frame.
//! - *Skip*: a teleport to the next turn, halfway, or the arrival. At 1.8 m/s a
//!   2 km walk is twenty real minutes, and an avatar world should not make you
//!   spend them.
//!
//! **Leaving the route asks for a new one**, from where the avatar stands, under
//! the same cooldown driver mode uses and for the same reason: a start that snaps
//! onto the far side of a canal can leave the avatar still "off" the new route.
//!
//! It needs an avatar, which only a session has; the planner says so instead of
//! offering a Walk that cannot move anything.

use bevy::prelude::*;

use crate::components::Avatar;
use crate::resources::{AvatarState, CameraMode, CameraState, MapAnchor, RightPanel};
use crate::systems::avatar::RouteWalk;
use crate::systems::driver::{
    countdown, point_along, project_onto_route, DriverState, Progress, RouteGeom,
};
use crate::systems::geo_nav::{GeoNavState, Teleport};
use crate::systems::icons;
use crate::systems::locomotion::Gait;
use crate::systems::navigation::RouteState;
use crate::systems::routing::{format_distance, format_duration, Maneuver};
use crate::systems::theme;

/// Within this of the end of the route, the walk is over.
const ARRIVE_M: f64 = 15.0;

/// Further than this from the route counts as having left it. Tighter than
/// driver mode's 60 m: an avatar's position is exact, a phone's fix is not.
const OFF_ROUTE_M: f64 = 35.0;

/// Seconds between two reroutes, so a start that snaps onto another street
/// cannot re-solve every frame.
const REROUTE_COOLDOWN_S: f32 = 10.0;

/// How far ahead along the route "Walk me there" aims. Far enough that the
/// avatar cuts a vertex rather than stepping onto it and turning on the spot,
/// near enough that it does not cut a corner through a building.
const AIM_AHEAD_M: f64 = 6.0;

/// Seconds with no progress before "Walk me there" gives up: something is in
/// the way that the route goes through and the avatar cannot.
const STUCK_AFTER_S: f32 = 4.0;

/// Where the bottom bar sits above the dock.
pub(crate) const BAR_GAP: f32 = 10.0;

/// The bottom bar's widest, points, before the screen narrows it.
pub(crate) const BAR_WIDTH: f32 = 420.0;

/// How far in from the left edge the next-turn card stands: clear of the
/// column of round buttons that comes down that edge (`dock`).
pub(crate) const TURN_INSET: f32 = theme::OSD_MARGIN + theme::ROUND_BUTTON_SIZE + 10.0;

/// The next-turn card's widest content, points.
pub(crate) const TURN_WIDTH: f32 = 260.0;

/// How far below the top edge the "Walking you there" chip stands.
pub(crate) const AUTO_TOP: f32 = theme::OSD_MARGIN + 52.0;

/// A tap on the walking chrome (`lib_walk_guide`), done by
/// [`WalkGuide::apply`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum WalkAct {
    /// Stop, or Done once arrived.
    Stop,
    /// "Walk me there" on or off.
    ToggleAuto,
    /// The Skip list open or shut.
    ToggleSkip,
    /// Jump to this far along the route, metres.
    SkipTo(f64),
}

/// The next turn, as the card at the top left shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct WalkTurn {
    pub glyph: &'static str,
    /// How far to it ("in 40 m").
    pub countdown: String,
    /// What to do there, on which street.
    pub street: String,
}

/// What the walking chrome shows this frame, published by
/// [`publish_walk_guide`] for the library to draw (`lib_walk_guide`).
#[derive(Clone, Debug, PartialEq, Default)]
#[cfg_attr(not(feature = "ui-lib"), allow(dead_code))]
pub struct WalkView {
    /// The next-turn card, when it shows.
    pub turn: Option<WalkTurn>,
    /// "Walking you there · any key takes over".
    pub auto_chip: bool,
    pub arrived: bool,
    /// "Walk me there" is on: its button reads Pause.
    pub auto: bool,
    /// What is left ("12 min · 800 m left"); unused once arrived.
    pub left: String,
    pub destination: String,
    pub note: Option<&'static str>,
    /// The Skip list, open: each target's line and how far along it is.
    pub skip: Option<Vec<(String, f64)>>,
}

/// The walking guide's state. `active` is what everything keys off.
#[derive(Resource, Default)]
pub struct WalkGuide {
    pub active: bool,
    /// "Walk me there": the avatar is following the route by itself.
    pub auto: bool,
    pub progress: Option<Progress>,
    /// Latched on arrival until the route changes, as driver mode does: walking
    /// the last metres past the pin must not take "you've arrived" away again.
    pub arrived: bool,
    /// Why "Walk me there" stopped by itself, shown until the next press.
    pub note: Option<&'static str>,
    skip_open: bool,
    /// Start as soon as a route exists ([`Self::arm`]); `Some(auto)`.
    armed: Option<bool>,
    geom: Option<(u32, RouteGeom)>,
    cooldown: f32,
    stuck_s: f32,
    last_along: f64,
    /// What the library's walking chrome (`lib_walk_guide`) shows, published
    /// by [`publish_walk_guide`].
    pub lib_view: Option<WalkView>,
    /// Taps on the library's walking chrome, done by [`publish_walk_guide`]
    /// next frame.
    pub lib_acts: Vec<WalkAct>,
}

impl WalkGuide {
    /// Start walking the planned route, optionally hands-off.
    pub fn start(&mut self, auto: bool) {
        *self = Self {
            active: true,
            auto,
            ..Self::default()
        };
    }

    /// Start walking once there is a route to walk — for a scripted capture,
    /// whose endpoints still have to go through a solve (`--ui walk`).
    pub fn arm(&mut self, auto: bool) {
        *self = Self {
            armed: Some(auto),
            ..Self::default()
        };
    }

    pub fn stop(&mut self) {
        *self = Self::default();
    }

    /// Do a tap on the walking chrome (`lib_walk_guide`).
    pub fn apply(&mut self, act: WalkAct, nav: &mut GeoNavState) {
        match act {
            WalkAct::Stop => self.stop(),
            WalkAct::ToggleAuto => {
                self.auto = !self.auto;
                self.note = None;
                self.stuck_s = 0.0;
            }
            WalkAct::ToggleSkip => self.skip_open = !self.skip_open,
            WalkAct::SkipTo(along) => {
                self.skip_open = false;
                if let Some((_, geom)) = self.geom.as_ref() {
                    if let Some((p, _)) = point_along(geom, along) {
                        nav.pending_teleport = Some(Teleport::geo(p[0], p[1], None));
                    }
                }
            }
        }
    }
}

/// The walk tick: measure progress, notice arrival and leaving the route, and
/// steer "Walk me there". After the avatar has moved, before the solver (so a
/// reroute asked for here is picked up the same frame).
#[allow(clippy::too_many_arguments)]
pub fn drive_walk_guide(
    mut guide: ResMut<WalkGuide>,
    mut route: ResMut<RouteState>,
    mut walk: ResMut<RouteWalk>,
    mut camera: ResMut<CameraState>,
    avatar: Res<AvatarState>,
    bodies: Query<(), With<Avatar>>,
    driver: Res<DriverState>,
    anchor: Res<MapAnchor>,
    time: Res<Time>,
) {
    let steer_off = |walk: &mut RouteWalk| {
        if walk.dir != Vec3::ZERO {
            *walk = RouteWalk::default();
        }
    };
    if let (Some(auto), Some(_)) = (guide.armed, route.displayed()) {
        guide.start(auto);
    }
    if !guide.active {
        steer_off(&mut walk);
        return;
    }
    // Driver mode, no avatar any more (signed out) or no route: the walk is over.
    let (Some(anchor), false, false) = (anchor.geo, bodies.is_empty(), driver.active) else {
        guide.stop();
        steer_off(&mut walk);
        return;
    };
    let Some(r) = route.displayed().cloned() else {
        guide.stop();
        steer_off(&mut walk);
        return;
    };
    let generation = route.generation;
    if guide.geom.as_ref().map(|(g, _)| *g) != Some(generation) {
        guide.geom = RouteGeom::build(&r).map(|g| (generation, g));
        guide.arrived = false;
        guide.last_along = 0.0;
    }
    let Some((_, geom)) = guide.geom.as_ref() else {
        steer_off(&mut walk);
        return;
    };
    let here_xz = Vec2::new(avatar.position.x, avatar.position.z);
    let (lat, lng) =
        crate::grid::world_to_lat_lng(anchor, f64::from(here_xz.x), f64::from(here_xz.y));
    let progress = project_onto_route(geom, &r, [lat, lng]);
    let aim = point_along(geom, progress.along_m + AIM_AHEAD_M).map(|(p, _)| p);
    let moved = progress.along_m - guide.last_along;
    guide.last_along = progress.along_m;
    guide.progress = Some(progress);
    let dt = time.delta_secs();
    guide.cooldown = (guide.cooldown - dt).max(0.0);

    if progress.remaining_m < ARRIVE_M {
        guide.arrived = true;
    }
    if guide.arrived {
        guide.auto = false;
        steer_off(&mut walk);
        return;
    }

    // Off the route: a new one from here, to the same destination, in walking.
    if progress.off_route_m > OFF_ROUTE_M && guide.cooldown <= 0.0 && route.held.is_none() {
        guide.cooldown = REROUTE_COOLDOWN_S;
        route.reroute_from(lat, lng);
    }

    if !guide.auto {
        steer_off(&mut walk);
        return;
    }
    // Walking only happens behind the avatar.
    if camera.mode != CameraMode::Avatar {
        camera.mode = CameraMode::Avatar;
    }
    // Stuck: no progress along the route for a while.
    if moved.abs() < 0.05 * f64::from(Gait::Walk.speed()) * f64::from(dt.max(1e-3)) {
        guide.stuck_s += dt;
    } else {
        guide.stuck_s = 0.0;
    }
    if guide.stuck_s > STUCK_AFTER_S {
        guide.auto = false;
        guide.stuck_s = 0.0;
        guide.note = Some("Something is in the way. Walk round it, or skip ahead.");
        steer_off(&mut walk);
        return;
    }
    let Some(aim) = aim else {
        steer_off(&mut walk);
        return;
    };
    let (ax, az) = crate::grid::lat_lng_to_world(anchor, aim[0], aim[1]);
    let dir = (Vec2::new(ax as f32, az as f32) - here_xz).normalize_or_zero();
    walk.dir = Vec3::new(dir.x, 0.0, dir.y);
    // The last few metres at a stroll, so arriving is a stop, not a run-on.
    walk.gait = if progress.remaining_m < 8.0 {
        Gait::Stroll
    } else {
        Gait::Walk
    };
}

/// The walking chrome's state (`lib_walk_guide` draws it): the next turn at
/// the top left, and the bar above the dock with what is left and what to do
/// about it. Does the taps the library handed back, through
/// [`WalkGuide::apply`], and publishes what to show in [`WalkGuide::lib_view`].
pub fn publish_walk_guide(
    mut guide: ResMut<WalkGuide>,
    route: Res<RouteState>,
    right_panel: Res<RightPanel>,
    mut nav: ResMut<GeoNavState>,
) {
    // The library's taps of last frame, before anything is worked out.
    if !guide.lib_acts.is_empty() {
        let acts = std::mem::take(&mut guide.lib_acts);
        if guide.active {
            for a in acts {
                guide.apply(a, &mut nav);
            }
        }
    }
    let shown = guide
        .active
        .then(|| route.displayed().zip(guide.progress))
        .flatten();
    let Some((r, progress)) = shown else {
        if guide.lib_view.is_some() {
            guide.lib_view = None;
        }
        return;
    };
    let destination = if route.to.label.is_empty() {
        "Destination".to_string()
    } else {
        route.to.label.clone()
    };
    // The next turn: not once arrived, nor under an open panel.
    let turn = (!guide.arrived && right_panel.is(RightPanel::None)).then(|| {
        let step = r.steps.get(progress.step);
        let maneuver = step.map_or(Maneuver::Continue, |s| s.maneuver);
        WalkTurn {
            glyph: icons::maneuver(maneuver),
            countdown: countdown(progress.step_distance_m),
            street: step
                .map(|s| s.instruction())
                .unwrap_or_else(|| "Follow the route".to_string()),
        }
    });
    let left = format!(
        "{} · {} left",
        format_duration(progress.remaining_s),
        format_distance(progress.remaining_m)
    );

    let view = WalkView {
        turn,
        auto_chip: guide.auto,
        arrived: guide.arrived,
        auto: guide.auto,
        left,
        destination,
        note: guide.note,
        skip: (guide.skip_open && !guide.arrived)
            .then(|| skip_targets(r, &guide, progress))
            .flatten(),
    };
    if guide.lib_view.as_ref() != Some(&view) {
        guide.lib_view = Some(view);
    }
}

/// The places Skip can jump to: the next turn, halfway, and the arrival —
/// each as its line ("Halfway   ·   400 m") and how far along the route it
/// is. `None` before the route's geometry is built.
fn skip_targets(
    r: &crate::systems::routing::Route,
    guide: &WalkGuide,
    progress: Progress,
) -> Option<Vec<(String, f64)>> {
    let (_, geom) = guide.geom.as_ref()?;
    let mut targets: Vec<(String, f64)> = Vec::new();
    let next_turn = geom
        .step_along
        .iter()
        .enumerate()
        .skip(progress.step)
        .find(|(i, &a)| {
            a > progress.along_m + 5.0
                && r.steps.get(*i).is_some_and(|s| {
                    !matches!(
                        s.maneuver,
                        Maneuver::Continue | Maneuver::Depart | Maneuver::Arrive
                    )
                })
        });
    if let Some((i, &a)) = next_turn {
        let name = r.steps[i]
            .name
            .clone()
            .unwrap_or_else(|| "next turn".to_string());
        targets.push((format!("Next turn · {name}"), a));
    }
    let half = progress.along_m + (geom.total_m - progress.along_m) / 2.0;
    if geom.total_m - progress.along_m > 200.0 {
        targets.push(("Halfway".to_string(), half));
    }
    targets.push(("Arrive".to_string(), geom.total_m));
    Some(
        targets
            .into_iter()
            .map(|(label, along)| {
                let gap = format_distance((along - progress.along_m).max(0.0));
                (format!("{label}   ·   {gap}"), along)
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starting_resets_everything_but_the_switches() {
        let mut g = WalkGuide {
            arrived: true,
            note: Some("x"),
            skip_open: true,
            ..WalkGuide::default()
        };
        g.start(true);
        assert!(g.active && g.auto);
        assert!(!g.arrived && g.note.is_none() && !g.skip_open);
        g.stop();
        assert!(!g.active && !g.auto);
    }
}
