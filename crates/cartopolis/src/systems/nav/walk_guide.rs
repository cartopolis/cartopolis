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
use crate::systems::egui_manager::EguiManager;
use crate::systems::geo_nav::{self, GeoNavState, Teleport};
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
const BAR_GAP: f32 = 10.0;

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
    let (lat, lng) = geo_nav::world_to_lat_lng(anchor, f64::from(here_xz.x), f64::from(here_xz.y));
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
    let (ax, az) = geo_nav::lat_lng_to_world(anchor, aim[0], aim[1]);
    let dir = (Vec2::new(ax as f32, az as f32) - here_xz).normalize_or_zero();
    walk.dir = Vec3::new(dir.x, 0.0, dir.y);
    // The last few metres at a stroll, so arriving is a stop, not a run-on.
    walk.gait = if progress.remaining_m < 8.0 {
        Gait::Stroll
    } else {
        Gait::Walk
    };
}

/// What the HUD asks for this frame.
#[derive(Default)]
struct WalkActions {
    stop: bool,
    toggle_auto: bool,
    toggle_skip: bool,
    skip_to: Option<f64>,
}

/// The walking chrome: the next turn at the top left, and the bar above the
/// dock with what is left and what to do about it.
pub fn render_walk_guide(
    egui: Res<EguiManager>,
    mut guide: ResMut<WalkGuide>,
    route: Res<RouteState>,
    right_panel: Res<RightPanel>,
    mut nav: ResMut<GeoNavState>,
) {
    if !guide.active {
        return;
    }
    let Some(r) = route.displayed() else {
        return;
    };
    let Some(progress) = guide.progress else {
        return;
    };
    let ctx = egui.ctx();
    let viewport = egui.viewport_pts();
    let mut act = WalkActions::default();
    let destination = if route.to.label.is_empty() {
        "Destination".to_string()
    } else {
        route.to.label.clone()
    };

    // The next turn, top left — clear of the map controls column.
    if !guide.arrived && right_panel.is(RightPanel::None) {
        let step = r.steps.get(progress.step);
        let maneuver = step.map_or(Maneuver::Continue, |s| s.maneuver);
        let street = step
            .map(|s| s.instruction())
            .unwrap_or_else(|| "Follow the route".to_string());
        egui::Area::new(egui::Id::new("walk_guide_turn"))
            .anchor(
                egui::Align2::LEFT_TOP,
                egui::vec2(
                    theme::OSD_MARGIN + theme::ROUND_BUTTON_SIZE + 10.0,
                    theme::OSD_MARGIN,
                ),
            )
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                theme::overlay_frame().show(ui, |ui| {
                    ui.set_max_width(260.0);
                    ui.horizontal(|ui| {
                        let (rect, _) =
                            ui.allocate_exact_size(egui::vec2(38.0, 38.0), egui::Sense::hover());
                        ui.painter()
                            .rect_filled(rect, egui::CornerRadius::same(9), theme::BRAND);
                        ui.painter().text(
                            rect.center(),
                            egui::Align2::CENTER_CENTER,
                            icons::maneuver(maneuver),
                            egui::FontId::proportional(24.0),
                            egui::Color32::WHITE,
                        );
                        ui.vertical(|ui| {
                            ui.label(
                                egui::RichText::new(countdown(progress.step_distance_m))
                                    .size(theme::SIZE_HEADING)
                                    .color(theme::CREAM),
                            );
                            ui.label(
                                egui::RichText::new(street)
                                    .size(theme::SIZE_SUB)
                                    .color(theme::TEXT_DIM),
                            );
                        });
                    });
                });
            });
    }

    if guide.auto {
        egui::Area::new(egui::Id::new("walk_guide_auto"))
            .anchor(
                egui::Align2::CENTER_TOP,
                egui::vec2(0.0, theme::OSD_MARGIN + 52.0),
            )
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                theme::overlay_frame().show(ui, |ui| {
                    ui.label(
                        egui::RichText::new("Walking you there · any key takes over")
                            .size(theme::SIZE_SUB)
                            .color(theme::TEXT),
                    );
                });
            });
    }

    // The bar above the dock.
    let width = (viewport.x - 2.0 * theme::OSD_MARGIN).min(420.0);
    let bottom = crate::systems::dock::reserved_bottom() + BAR_GAP;
    egui::Area::new(egui::Id::new("walk_guide_bar"))
        .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -bottom))
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            theme::overlay_frame().show(ui, |ui| {
                ui.set_width(width - 24.0);
                if guide.skip_open && !guide.arrived {
                    skip_list(ui, r, &guide, progress, &mut act);
                    ui.add_space(6.0);
                }
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        if guide.arrived {
                            ui.label(
                                egui::RichText::new("You've arrived")
                                    .size(theme::SIZE_TITLE)
                                    .color(theme::CREAM),
                            );
                        } else {
                            ui.label(
                                egui::RichText::new(format!(
                                    "{} · {} left",
                                    format_duration(progress.remaining_s),
                                    format_distance(progress.remaining_m)
                                ))
                                .size(theme::SIZE_VALUE)
                                .color(theme::CREAM),
                            );
                        }
                        ui.label(
                            egui::RichText::new(&destination)
                                .size(theme::SIZE_SUB)
                                .color(theme::TEXT_DIM),
                        );
                        if let Some(note) = guide.note {
                            ui.label(
                                egui::RichText::new(note)
                                    .size(theme::SIZE_SUB)
                                    .color(theme::WARN),
                            );
                        }
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let stop = if guide.arrived { "Done" } else { "Stop" };
                        if theme::action_button(ui, stop).clicked() {
                            act.stop = true;
                        }
                        if !guide.arrived {
                            let auto = if guide.auto {
                                "Pause".to_string()
                            } else {
                                format!("{}  Auto", icons::PLAY)
                            };
                            if theme::action_button(ui, auto)
                                .on_hover_text("Walk me there: the avatar follows the route")
                                .clicked()
                            {
                                act.toggle_auto = true;
                            }
                            if theme::action_button(ui, format!("{}  Skip", icons::FORWARD))
                                .on_hover_text("Jump ahead along the route")
                                .clicked()
                            {
                                act.toggle_skip = true;
                            }
                        }
                    });
                });
            });
        });

    if act.stop {
        guide.stop();
    }
    if act.toggle_auto {
        guide.auto = !guide.auto;
        guide.note = None;
        guide.stuck_s = 0.0;
    }
    if act.toggle_skip {
        guide.skip_open = !guide.skip_open;
    }
    if let Some(along) = act.skip_to {
        guide.skip_open = false;
        if let Some((_, geom)) = guide.geom.as_ref() {
            if let Some((p, _)) = point_along(geom, along) {
                nav.pending_teleport = Some(Teleport::geo(p[0], p[1], None));
            }
        }
    }
}

/// The places Skip can jump to: the next turn, halfway, and the arrival.
fn skip_list(
    ui: &mut egui::Ui,
    r: &crate::systems::routing::Route,
    guide: &WalkGuide,
    progress: Progress,
    act: &mut WalkActions,
) {
    let Some((_, geom)) = guide.geom.as_ref() else {
        return;
    };
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
    ui.label(
        egui::RichText::new("Skip ahead to…")
            .size(theme::SIZE_SUB)
            .color(theme::TEXT_DIM),
    );
    for (label, along) in targets {
        let gap = format_distance((along - progress.along_m).max(0.0));
        if ui
            .add_sized(
                egui::vec2(ui.available_width(), theme::row_height()),
                egui::Button::new(format!("{label}   ·   {gap}")),
            )
            .clicked()
        {
            act.skip_to = Some(along);
        }
    }
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
