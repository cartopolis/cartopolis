//! Bottom-right status overlay: wall-clock time, the camera's geographic
//! position and altitude, a compass, and two location actions — a copy button
//! takes the current place ("lat, lng, altitude, pitch°, yaw°" — see
//! [`format_place`]; pasteable into the search bar, or into a `?place=` URL on
//! the web) and ☆ bookmarks it (named by reverse-geocoded address; see
//! `bookmarks`).
//!
//! The clock shows real local time once [`WorldClock`] has learned the
//! region's UTC offset (from the transit feed), and UTC until then.
//!
//! Layout note: the overlay is an anchored, size-to-content [`egui::Area`].
//! Anything inside that reaches for the *available* width (a `right_to_left`
//! row) would let the box grow a little each frame — the Area sizes to its
//! content, and next frame that larger size becomes the available width, a
//! feedback loop that stretches the panel across the screen and pushes the
//! text off the top. So the content is pinned to [`HUD_WIDTH`] up front, which
//! bounds every inner row and keeps the box a fixed size.

use crate::resources::MapAnchor;
use web_time::Instant;

use bevy::prelude::*;
use big_space::prelude::GridCell;

use crate::components::FreeCamera;
use crate::resources::CameraState;
use crate::systems::bookmarks::{BookmarksState, PLACEHOLDER_NAME};
use crate::systems::egui_manager::EguiManager;
use crate::systems::geo_nav::{camera_geo, CameraPose};
use crate::systems::theme;
use crate::systems::transit_live::WorldClock;
use crate::systems::user_store::UserStore;

/// Fixed content width of the overlay (see the module note on why it must be
/// fixed rather than sized to content).
const HUD_WIDTH: f32 = 150.0;

/// Plain snapshot the overlay renders — no ECS types, so the layout is unit
/// testable.
#[derive(Clone, Default)]
pub struct HudData {
    pub time: String,
    /// Whether the clock is currently shown as UTC (drives the Local/UTC toggle
    /// label). True when the user forced UTC, or no local offset is known yet.
    pub utc: bool,
    /// Current sun-mode for the Auto/Day/Night switcher's active state.
    pub sun_mode: crate::resources::SunMode,
    /// Whether live (Auto) weather is on — drives the weather switcher's state.
    pub weather_live: bool,
    /// Forecast readout for the weather line: (compact, full-hover). `Some` only
    /// when Auto weather has a sample to show.
    pub weather_info: Option<(String, String)>,
    pub geo: Option<(f64, f64)>,
    pub altitude: Option<f64>,
    /// Show the copy button's "just copied" ✔ state. (`✔` is in the bundled
    /// font — see `theme::first_drawable`'s inventory — so it is safe to type.)
    pub copied: bool,
    /// Show the Set Home button's "just set" ✔ state.
    pub home_set: bool,
    /// Authenticated username, if online (ADR-016). Drives the profile chip;
    /// grows into a chat/presence affordance later.
    pub user: Option<String>,
}

/// Render a place the way the copy button copies it: "lat, lng, altm, pitch°, yaw°",
/// with the view dropped when there is no camera to read it from.
///
/// One function because this string is a small interchange format — the search
/// bar and the `?place=` launch URL both parse it back
/// ([`crate::systems::geo_nav::parse_location`]) — and a format only round-trips
/// if it has exactly one writer.
pub fn format_place(lat: f64, lng: f64, alt: f64, look: Option<(f32, f32)>) -> String {
    match look {
        Some((pitch, yaw)) => {
            format!("{lat:.5}, {lng:.5}, {alt:.0}m, {pitch:.1}°, {yaw:.1}°")
        }
        None => format!("{lat:.5}, {lng:.5}, {alt:.0}m"),
    }
}

/// What the user clicked in the overlay this frame.
#[derive(Default)]
pub struct HudActions {
    pub copy: bool,
    pub bookmark: bool,
    /// User clicked Set Home (save the current spot + view as Home).
    pub set_home: bool,
    /// User clicked the Local/UTC clock toggle.
    pub toggle_utc: bool,
    /// User picked a new sun mode from the Auto/Day/Night switcher.
    pub set_sun_mode: Option<crate::resources::SunMode>,
    /// User toggled live (Auto) weather on/off from the weather switcher.
    pub set_weather_live: Option<bool>,
}

/// Diameter (points) of the top-right compass overlay.
const COMPASS_SIZE: f32 = 46.0;

/// How the rose behaves this frame — one compass with three jobs.
struct Compass {
    /// Clicking does something: Orbit's north-up lock, or driver mode's map
    /// orientation cycle. Otherwise it is a passive indicator.
    interactive: bool,
    /// Held north-up — tints the ring amber and adds a padlock.
    locked: bool,
    /// Distance from the top of the screen. Driver mode stacks the rose *under*
    /// its manoeuvre card, which takes the corner the compass usually has.
    top: f32,
    /// Caption under the rose (driver mode's orientation name). `None` leaves the
    /// bare compass.
    caption: Option<&'static str>,
    hover: &'static str,
}

/// Draws the top-left compass rose (Organic-Maps style): the accent orange needle
/// and the "N" point to geographic north as it appears on screen. In globe (Orbit)
/// mode it is a toggle — clicking locks the view north-up (and unlocking
/// releases it); while driving it steps through the map orientations; in the
/// free-form fly/avatar modes it is a passive indicator. Returns true when the
/// user clicked it (only possible when `cfg.interactive`).
fn compass_overlay(ctx: &egui::Context, heading_deg: f32, cfg: &Compass) -> bool {
    let (interactive, locked) = (cfg.interactive, cfg.locked);
    let mut clicked = false;
    // Amber, the same "held" accent the locked ring and glyph share — and the
    // brand's second accent, so it can never be mistaken for the accent orange
    // hover/pressed states the rest of the UI uses.
    let lock_color = theme::AMBER;
    egui::Area::new(egui::Id::new("compass_overlay"))
        // Top-left: the status HUD moved into the top-right corner, so the compass
        // takes the opposite one to keep both clear of the docked panels.
        .anchor(egui::Align2::LEFT_TOP, egui::vec2(12.0, cfg.top))
        .show(ctx, |ui| {
            let sense = if interactive {
                egui::Sense::click()
            } else {
                egui::Sense::hover()
            };
            let (rect, resp) =
                ui.allocate_exact_size(egui::vec2(COMPASS_SIZE, COMPASS_SIZE), sense);
            let hovered = interactive && resp.hovered();
            let p = ui.painter();
            let c = rect.center();
            let r = COMPASS_SIZE * 0.5 - 2.0;
            // Disc + ring (the ring takes the accent on hover to signal it's
            // clickable, and goes amber while the view is locked north-up).
            p.circle_filled(c, r, theme::OVERLAY);
            let ring = if locked {
                lock_color
            } else if hovered {
                theme::ACCENT
            } else {
                theme::OVERLAY_LINE
            };
            p.circle_stroke(
                c,
                r,
                egui::Stroke::new(if locked { 2.0 } else { 1.5 }, ring),
            );
            // Screen direction of north when screen-up is the view heading.
            let th = heading_deg.to_radians();
            let north = egui::vec2(-th.sin(), -th.cos());
            let side = egui::vec2(north.y, -north.x); // 90° clockwise from north
            let w = side * 4.5;
            // North half: the accent (a compass needle is red by convention,
            // and the brand's warm end is the closest thing to it we own); south
            // half: the muted body tone.
            p.add(egui::Shape::convex_polygon(
                vec![c + north * (r - 3.0), c + w, c - w],
                theme::ACCENT,
                egui::Stroke::NONE,
            ));
            p.add(egui::Shape::convex_polygon(
                vec![c - north * (r - 7.0), c + w, c - w],
                theme::TEXT_DIM,
                egui::Stroke::NONE,
            ));
            // "N" over the north tip.
            p.text(
                c + north * (r - 11.0),
                egui::Align2::CENTER_CENTER,
                "N",
                egui::FontId::proportional(theme::SIZE_TINY),
                theme::CREAM,
            );
            // Locked: a small padlock at the south edge, clear of the needle.
            // Drawn from primitives rather than an emoji — egui's bundled font
            // only carries a subset of them, and a missing glyph reads as tofu.
            if locked {
                let lc = egui::pos2(c.x, c.y + r - 7.0);
                p.rect_filled(
                    egui::Rect::from_center_size(lc, egui::vec2(7.0, 5.0)),
                    1.0,
                    lock_color,
                );
                // Shackle: half-circle rising out of the body.
                let shackle: Vec<egui::Pos2> = (0..=8)
                    .map(|i| {
                        let a = std::f32::consts::PI * (i as f32) / 8.0;
                        egui::pos2(lc.x - a.cos() * 2.3, lc.y - 2.5 - a.sin() * 2.3)
                    })
                    .collect();
                p.add(egui::Shape::line(
                    shackle,
                    egui::Stroke::new(1.2, lock_color),
                ));
            }
            // Orientation name under the rose while driving: the three views
            // differ by tilt as much as by heading, and "which one am I in" is not
            // a question a needle can answer.
            if let Some(caption) = cfg.caption {
                p.text(
                    egui::pos2(c.x, rect.bottom() + 7.0),
                    egui::Align2::CENTER_CENTER,
                    caption,
                    egui::FontId::proportional(theme::SIZE_TINY),
                    theme::TEXT_DIM,
                );
            }
            if interactive {
                resp.clone().on_hover_text(cfg.hover);
            }
            clicked = resp.clicked();
        });
    clicked
}

/// Diameter (points) of the GPS / locate button, stacked under the compass.
const LOCATE_SIZE: f32 = 40.0;

/// Locate-button state, mirroring Organic Maps' location-button cycle. Derived
/// from the camera each frame; a click advances it (see `render_hud`).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum LocateState {
    /// Not following the avatar (Fly/Earth camera) — a click recenters onto it.
    Off,
    /// Following the avatar (Avatar mode), free orbit.
    Follow,
    /// Following *and* rotating with the avatar's heading (chase cam).
    FollowRotate,
}

/// Draws the GPS/locate button beneath the compass (Organic-Maps style) and
/// returns true when clicked. The icon is a navigation "location arrow": a hollow
/// muted outline when off, a filled accent orange arrow while following, and a filled
/// amber arrow while follow-and-rotate (the amber echoes the compass's north-up
/// lock — both are *held* states).
/// Drawn from primitives, like the compass — egui's bundled font carries few
/// emoji, so a glyph would risk rendering as tofu.
fn locate_overlay(ctx: &egui::Context, state: LocateState) -> bool {
    let mut clicked = false;
    let accent = theme::ACCENT;
    let rotate_accent = theme::AMBER;
    egui::Area::new(egui::Id::new("locate_overlay"))
        // Directly under the compass, centred on its column.
        .anchor(
            egui::Align2::LEFT_TOP,
            egui::vec2(
                12.0 + (COMPASS_SIZE - LOCATE_SIZE) * 0.5,
                crate::systems::ui::MAP_CONTROLS_TOP + COMPASS_SIZE + 8.0,
            ),
        )
        .show(ctx, |ui| {
            let (rect, resp) =
                ui.allocate_exact_size(egui::vec2(LOCATE_SIZE, LOCATE_SIZE), egui::Sense::click());
            let hovered = resp.hovered();
            let p = ui.painter();
            let c = rect.center();
            let r = LOCATE_SIZE * 0.5 - 2.0;
            p.circle_filled(c, r, theme::OVERLAY);
            let (mark, ring, filled) = match state {
                LocateState::Off => (
                    if hovered {
                        theme::CREAM
                    } else {
                        theme::TEXT_DIM
                    },
                    if hovered {
                        theme::ACCENT
                    } else {
                        theme::OVERLAY_LINE
                    },
                    false,
                ),
                LocateState::Follow => (accent, accent, true),
                LocateState::FollowRotate => (rotate_accent, rotate_accent, true),
            };
            p.circle_stroke(
                c,
                r,
                egui::Stroke::new(if filled { 2.0 } else { 1.5 }, ring),
            );
            // Navigation "location arrow" (a kite), pointing up.
            let s = LOCATE_SIZE * 0.30;
            let arrow = vec![
                c + egui::vec2(0.0, -s),
                c + egui::vec2(-0.62 * s, 0.72 * s),
                c + egui::vec2(0.0, 0.30 * s),
                c + egui::vec2(0.62 * s, 0.72 * s),
            ];
            if filled {
                p.add(egui::Shape::convex_polygon(arrow, mark, egui::Stroke::NONE));
            } else {
                let mut outline = arrow;
                outline.push(outline[0]);
                p.add(egui::Shape::line(outline, egui::Stroke::new(1.6, mark)));
            }
            resp.clone().on_hover_text(match state {
                LocateState::Off => "Center on my location",
                LocateState::Follow => "Following — click to rotate with heading",
                LocateState::FollowRotate => "Follow & rotate — click to stop rotating",
            });
            clicked = resp.clicked();
        });
    clicked
}

// ─── Scale bar ────────────────────────────────────────────────────────────────

/// The widest the bar may draw, in points. A scale bar is read by comparing it
/// with something on the map, so it wants to be long; it must also not become a
/// rule across the bottom of a phone.
const SCALE_MAX_PTS: f32 = 110.0;
/// Steepest the view may be for the bar to mean anything: below this depression
/// angle the centre ray runs most of the way to the horizon, where "one number
/// of metres per point" stops being true within the bar's own length.
const SCALE_MIN_DEPRESSION: f32 = 0.20; // ~11.5° below horizontal

/// Ground metres per screen point at the point the camera is looking at.
///
/// The bar answers "how big is what I am looking at", so it is measured where
/// the view centre meets the ground rather than under the camera: those are the
/// same thing looking straight down and an order of magnitude apart at a shallow
/// tilt. `depression` is the sine of the angle the view makes below the
/// horizontal, which is `-forward.y` for a normalised forward vector.
///
/// `None` when the view is too shallow to have a single scale ([`SCALE_MIN_DEPRESSION`]),
/// which is also every view that includes the horizon — the honest answer there
/// is no bar rather than a bar that is right for one row of pixels.
#[must_use]
pub fn ground_metres_per_point(
    altitude_m: f32,
    depression: f32,
    fov_y: f32,
    viewport_h_pts: f32,
) -> Option<f32> {
    if !(altitude_m.is_finite() && altitude_m > 0.0) || viewport_h_pts < 1.0 {
        return None;
    }
    if !(depression > SCALE_MIN_DEPRESSION) {
        return None;
    }
    // Distance along the view ray to the ground under the screen centre.
    let distance = altitude_m / depression;
    // The view plane's height at that distance, spread over the viewport. The
    // *horizontal* ground scale works out to the same number: the horizontal
    // extent is this times the aspect ratio, over that many more points.
    let extent = 2.0 * distance * (fov_y * 0.5).tan();
    let per_point = extent / viewport_h_pts;
    per_point.is_finite().then_some(per_point)
}

/// Pick the bar: the longest 1/2/5×10ⁿ metres that fits in `max_pts`, and how
/// many points it is.
///
/// 1-2-5 rather than "whatever fits" because a scale bar is read as a *number* —
/// 200 m can be held in the head and compared with the next view; 183 m is
/// noise wearing the same clothes.
#[must_use]
pub fn nice_scale_bar(metres_per_point: f32, max_pts: f32) -> Option<(f32, f32)> {
    if !(metres_per_point.is_finite() && metres_per_point > 0.0) || max_pts < 10.0 {
        return None;
    }
    let longest = metres_per_point * max_pts;
    // Walk the mantissa down from the decade above, so the first hit is the
    // largest of the three that fits.
    let decade = 10_f32.powf(longest.log10().floor());
    let metres = [5.0, 2.0, 1.0]
        .into_iter()
        .map(|m| m * decade)
        .find(|m| *m <= longest)?;
    let pts = metres / metres_per_point;
    (pts >= 10.0).then_some((metres, pts))
}

/// "500 m" / "2 km" — whole numbers only, which the 1-2-5 ladder guarantees.
#[must_use]
pub fn format_scale(metres: f32) -> String {
    if metres >= 1000.0 {
        format!("{} km", (metres / 1000.0).round() as i64)
    } else {
        format!("{} m", metres.round() as i64)
    }
}

/// Draw the bar in the bottom-right corner: a bracket and its length.
///
/// Bottom-**right** because the other three corners are taken (the address bar
/// and compass column top-left, the status readout top-right, the dock and the
/// fly stick along the bottom-left), and because it is where every phone map
/// puts it.
fn scale_bar_overlay(ctx: &egui::Context, metres: f32, pts: f32) {
    egui::Area::new(egui::Id::new("scale_bar"))
        .anchor(
            egui::Align2::RIGHT_BOTTOM,
            egui::vec2(-12.0, -(crate::systems::dock::reserved_bottom() + 6.0)),
        )
        // `Sense::hover` only: a scale bar is a readout, and an `Area` that
        // senses clicks over the map would eat taps meant for the world.
        .interactable(false)
        .show(ctx, |ui| {
            let (rect, _) = ui.allocate_exact_size(
                egui::vec2(pts, theme::SIZE_TINY + 10.0),
                egui::Sense::hover(),
            );
            let p = ui.painter();
            let base = rect.bottom();
            let ink = theme::CREAM;
            // A halo under the rule, not a filled box: the bar has to be legible
            // over both dark water and white paper, and a plate this small reads
            // as a piece of chrome that has come loose.
            let halo = egui::Stroke::new(3.0, theme::OVERLAY);
            let rule = egui::Stroke::new(1.5, ink);
            for stroke in [halo, rule] {
                p.line_segment(
                    [
                        egui::pos2(rect.left(), base),
                        egui::pos2(rect.right(), base),
                    ],
                    stroke,
                );
                for x in [rect.left(), rect.right()] {
                    p.line_segment([egui::pos2(x, base), egui::pos2(x, base - 5.0)], stroke);
                }
            }
            p.text(
                egui::pos2(rect.right(), base - 6.0),
                egui::Align2::RIGHT_BOTTOM,
                format_scale(metres),
                egui::FontId::proportional(theme::SIZE_TINY),
                ink,
            );
        });
}

/// The left-hand map controls: the compass rose, and the GPS/locate button under
/// it.
///
/// Its own system rather than part of [`render_hud`] because it answers to a
/// different owner in each mode — the Orbit camera's north lock normally, driver
/// mode's orientation cycle while navigating — and because `render_hud` was
/// already at Bevy's 16-parameter ceiling.
#[allow(clippy::too_many_arguments)]
pub fn render_map_controls(
    mut egui: ResMut<EguiManager>,
    mut camera_state: ResMut<CameraState>,
    mut driver: ResMut<crate::systems::driver::DriverState>,
    globe: Res<crate::systems::globe::GlobeState>,
    right_panel: Res<crate::resources::RightPanel>,
    camera: Query<(&GridCell, &Transform, &Projection), With<FreeCamera>>,
    avatars: Query<(), With<crate::components::Avatar>>,
) {
    // A right-docked panel (Layers, Bookmarks, AI) sits opaque over the map. egui
    // always paints floating Areas on top of panels (panels live on the bottom-most
    // background layer, and even `Order::Background` Areas paint above it), so the
    // only way to keep these *behind* one is to drop them while it is open. Driver
    // mode is the exception: the compass is part of its chrome, and the planner it
    // was started from is closed anyway.
    //
    // `RightPanel` is the whole answer. This used to also OR in `DataLayers::
    // panel_open`, `BookmarksState::panel_open` and `AiAssistantState::open` — the
    // booleans the panels carried before the shared dock existed. None of them is
    // cleared anywhere, and `render_hud` *set* the bookmarks one when you saved a
    // place, so a single press of ☆ hid the compass, the locate button and the
    // whole status overlay for the rest of the session. Nothing writes them now
    // except the `--shot` harness, which sets `RightPanel` alongside them from the
    // same `--ui` list, so the scripted chrome is unchanged.
    if !right_panel.is(crate::resources::RightPanel::None) && !driver.active {
        return;
    }
    let Ok((cell, tf, projection)) = camera.single() else {
        return;
    };
    let in_space = globe.in_space(cell, tf);
    // Over the globe the bearing is measured against the spin axis, not the local
    // flat frame (see `space_view_heading`).
    let heading = if in_space {
        space_view_heading(tf, globe.north_axis_local())
    } else {
        view_heading(tf)
    };
    let Some(heading) = heading else {
        return;
    };

    // The scale bar, whenever the view has a single ground scale to state.
    // Skipped in the space tier on purpose: up there the local flat frame is not
    // the ground, the visible arc is a fraction of a sphere, and a straight bar
    // of metres would be wrong by however much of the limb is in shot.
    if !in_space && !driver.active {
        let fov_y = match projection {
            Projection::Perspective(p) => Some(p.fov),
            _ => None,
        };
        let viewport_h = egui.viewport_pts().y;
        // `-forward.y` is the sine of the view's depression below horizontal —
        // 1 looking straight down, 0 at the horizon.
        let depression = -(tf.rotation * Vec3::NEG_Z).normalize().y;
        if let Some((metres, pts)) = fov_y
            .and_then(|fov| {
                ground_metres_per_point(
                    globe.altitude_m(cell, tf) as f32,
                    depression,
                    fov,
                    viewport_h,
                )
            })
            .and_then(|per_pt| nice_scale_bar(per_pt, SCALE_MAX_PTS))
        {
            scale_bar_overlay(egui.ctx_mut(), metres, pts);
        }
    }

    if driver.active {
        // Driving: the rose steps through Organic Maps' navigation orientations,
        // and stacks under the manoeuvre card that has taken the corner.
        use crate::systems::driver::{CARD_HEIGHT, CARD_MARGIN};
        let cfg = Compass {
            interactive: true,
            locked: driver.orientation == crate::systems::driver::MapOrientation::NorthUp,
            top: CARD_MARGIN + CARD_HEIGHT + 8.0,
            caption: Some(driver.orientation.label()),
            hover: "Change the map orientation (heading up / north up / 3D)",
        };
        if compass_overlay(egui.ctx_mut(), heading, &cfg) {
            driver.orientation = driver.orientation.next();
        }
        return;
    }

    // Clicking toggles the north-up lock in globe (Orbit) mode only; fly/avatar are
    // free-form, so there the rose is a passive indicator.
    let orbit = matches!(camera_state.mode, crate::resources::CameraMode::Orbit);
    let locked = orbit && camera_state.orbit_north_lock;
    let cfg = Compass {
        interactive: orbit,
        locked,
        // Below the address bar's logo card, which owns this corner permanently
        // now (`ui::MAP_CONTROLS_TOP`) — the mark used to sit on the compass.
        top: crate::systems::ui::MAP_CONTROLS_TOP,
        caption: None,
        hover: if locked {
            "North-up locked — click to release"
        } else {
            "Lock the view north-up"
        },
    };
    if compass_overlay(egui.ctx_mut(), heading, &cfg) {
        // Toggle the lock; engaging it also eases the view back to north (the lock
        // holds it there from then on).
        camera_state.orbit_north_lock = !locked;
        camera_state.orbit_north_reset = camera_state.orbit_north_lock;
    }

    // GPS / locate button, stacked under the compass (Organic-Maps style). Only in
    // the viewer that owns a local avatar — Follow doesn't exist otherwise. Not
    // drawn while driving: the arrow is already centred and the camera is held, so
    // there is nothing left for it to do.
    if !avatars.is_empty() {
        use crate::resources::CameraMode;
        let state = match (camera_state.mode, camera_state.follow_rotate) {
            (CameraMode::Avatar, true) => LocateState::FollowRotate,
            (CameraMode::Avatar, false) => LocateState::Follow,
            _ => LocateState::Off,
        };
        if locate_overlay(egui.ctx_mut(), state) {
            match state {
                // Recenter: enter Follow and snap the rig onto the avatar (the same
                // proven path Tab / the Camera menu use).
                LocateState::Off => {
                    crate::systems::free_camera::enter_mode(&mut camera_state, CameraMode::Avatar);
                    camera_state.mode = CameraMode::Avatar;
                }
                LocateState::Follow => camera_state.follow_rotate = true,
                LocateState::FollowRotate => camera_state.follow_rotate = false,
            }
        }
    }
}

/// Build the overlay's anchored box and return what was clicked. Pure UI over
/// [`HudData`], so it can be exercised headless (see the test below).
pub fn hud_area(ctx: &egui::Context, data: &HudData) -> HudActions {
    let mut actions = HudActions::default();
    let mono = |ui: &mut egui::Ui, text: String, size: f32, color: egui::Color32| {
        ui.label(
            egui::RichText::new(text)
                .monospace()
                .size(size)
                .color(color),
        );
    };

    // Top-right: keeps clear of the bottom chat/action bar. The caller drops the
    // whole HUD while a right-docked panel is open, so the two never coexist here.
    egui::Area::new(egui::Id::new("status_hud"))
        .anchor(egui::Align2::RIGHT_TOP, egui::vec2(-10.0, 10.0))
        .show(ctx, |ui| {
            theme::overlay_frame().show(ui, |ui| {
                // Pin the width first — this is what stops the runaway.
                ui.set_width(HUD_WIDTH);
                // Overlay density. The theme's rhythm is sized for a panel of
                // place rows and layer toggles; in a 150 pt corner readout the
                // same values add ~40 pt of air to a box that is mostly
                // numbers. These switchers are chips, not buttons — so tighten
                // locally, while keeping a 20 pt hit target.
                ui.spacing_mut().button_padding = egui::vec2(6.0, 2.0);
                ui.spacing_mut().interact_size.y = 20.0;
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 3.0;
                    // Profile chip: a placeholder avatar dot with the account
                    // initial, plus the username. Extends into a presence/chat
                    // affordance later (ADR-016 / ADR-028).
                    if let Some(user) = &data.user {
                        ui.horizontal(|ui| {
                            let (rect, _) = ui
                                .allocate_exact_size(egui::vec2(15.0, 15.0), egui::Sense::hover());
                            let c = rect.center();
                            ui.painter().circle_filled(c, 7.0, theme::ACCENT);
                            if let Some(ch) = user.chars().next() {
                                ui.painter().text(
                                    c,
                                    egui::Align2::CENTER_CENTER,
                                    ch.to_uppercase().to_string(),
                                    egui::FontId::proportional(theme::SIZE_TINY),
                                    // Ink on the accent, not white — the same
                                    // 6:1 pairing every filled orange uses.
                                    theme::INK,
                                );
                            }
                            ui.label(
                                egui::RichText::new(user)
                                    .size(theme::SIZE_SUB)
                                    .color(theme::CREAM),
                            );
                        });
                    }
                    // Clock + a small Local/UTC toggle, then the sun-mode
                    // switcher (Auto/Day/Night) — a light/dark/auto control
                    // for the sky, the way a website theme toggle reads.
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;
                        mono(ui, data.time.clone(), theme::SIZE_VALUE, theme::CREAM);
                        let zone = if data.utc { "UTC" } else { "Local" };
                        if ui
                            .add(
                                egui::Button::new(egui::RichText::new(zone).size(theme::SIZE_TINY))
                                    .small(),
                            )
                            .on_hover_text("Toggle the clock between local time and UTC")
                            .clicked()
                        {
                            actions.toggle_utc = true;
                        }
                    });
                    ui.horizontal(|ui| {
                        use crate::resources::SunMode;
                        ui.spacing_mut().item_spacing.x = 3.0;
                        let mut choice =
                            |ui: &mut egui::Ui, mode: SunMode, label: &str, tip: &str| {
                                if ui
                                    .selectable_label(
                                        data.sun_mode == mode,
                                        egui::RichText::new(label).size(theme::SIZE_CHIP),
                                    )
                                    .on_hover_text(tip)
                                    .clicked()
                                {
                                    actions.set_sun_mode = Some(mode);
                                }
                            };
                        choice(ui, SunMode::Auto, "Auto", "Sun follows the real local time");
                        // Words, not `☀`/`🌙`: neither is in the bundled font, so
                        // both were boxes — and next to a textual "Auto" the words
                        // read better anyway, on touch especially, where the
                        // explanatory tooltip can never appear.
                        choice(ui, SunMode::Day, "Day", "Force daylight (noon)");
                        choice(ui, SunMode::Night, "Night", "Force night (midnight)");
                    });
                    // Weather: an Auto/Manual toggle mirroring the sun switcher,
                    // plus the live forecast readout for the map location.
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 3.0;
                        if ui
                            .selectable_label(
                                data.weather_live,
                                egui::RichText::new("Auto").size(theme::SIZE_CHIP),
                            )
                            .on_hover_text("Live weather for the map location (met.no)")
                            .clicked()
                        {
                            actions.set_weather_live = Some(true);
                        }
                        if ui
                            .selectable_label(
                                !data.weather_live,
                                egui::RichText::new("Manual").size(theme::SIZE_CHIP),
                            )
                            .on_hover_text("Hold the weather; adjust it in the dev panel")
                            .clicked()
                        {
                            actions.set_weather_live = Some(false);
                        }
                    });
                    if let Some((short, full)) = &data.weather_info {
                        ui.label(
                            egui::RichText::new(short)
                                .monospace()
                                .size(theme::SIZE_CHIP)
                                .color(theme::TEXT_DIM),
                        )
                        .on_hover_text(full);
                    }
                    if let Some((lat, lng)) = data.geo {
                        let ns = if lat >= 0.0 { 'N' } else { 'S' };
                        let ew = if lng >= 0.0 { 'E' } else { 'W' };
                        mono(
                            ui,
                            format!("{:.5}° {}", lat.abs(), ns),
                            theme::SIZE_CHIP,
                            theme::TEXT_DIM,
                        );
                        mono(
                            ui,
                            format!("{:.5}° {}", lng.abs(), ew),
                            theme::SIZE_CHIP,
                            theme::TEXT_DIM,
                        );
                    }
                    if let Some(alt) = data.altitude {
                        let alt = alt.max(0.0);
                        let text = if alt >= 1000.0 {
                            format!("alt {:.1} km", alt / 1000.0)
                        } else {
                            format!("alt {alt:.0} m")
                        };
                        mono(ui, text, theme::SIZE_CHIP, theme::TEXT_DIM);
                    }
                    // Heading now lives in the top-right compass overlay.
                    // Location actions.
                    if data.geo.is_some() && data.altitude.is_some() {
                        ui.horizontal(|ui| {
                            if ui
                                .add(egui::Button::new(if data.home_set {
                                    theme::first_drawable(ui.ctx(), &["✔", "✓", "Set"])
                                } else {
                                    theme::first_drawable(ui.ctx(), &["🏠", "⌂", "Home"])
                                }))
                                .on_hover_text("Set Home (the nav bar's Home button returns here)")
                                .clicked()
                            {
                                actions.set_home = true;
                            }
                            // `⧉` (U+29C9) stood here and is in **none** of the
                            // four fonts in the Proportional stack, so the copy
                            // button has been an empty box since it was added —
                            // exactly what `first_drawable` exists to prevent,
                            // bypassed by typing the glyph straight in.
                            let copy_icon = if data.copied {
                                theme::first_drawable(ui.ctx(), &["✔", "✓", "OK"])
                            } else {
                                theme::first_drawable(ui.ctx(), &["📋", "⎘", "Copy"])
                            };
                            if ui
                                .add(egui::Button::new(copy_icon))
                                .on_hover_text(
                                    "Copy place — position and view (paste in the \
                                         search bar to return)",
                                )
                                .clicked()
                            {
                                actions.copy = true;
                            }
                            if ui
                                .add(egui::Button::new("☆"))
                                .on_hover_text("Bookmark this location")
                                .clicked()
                            {
                                actions.bookmark = true;
                            }
                        });
                    }
                });
            });
        });
    actions
}

/// Compass bearing (deg, 0 = north, 90 = east) of the direction that reads as
/// "up" on screen over the ground. North is −Z in world space (East +X, South
/// +Z). Uses the camera's forward when it is tilted toward the ground, and its
/// up vector when looking straight down — the two point the same way on the
/// horizontal plane and hand over smoothly, so the compass stays defined
/// through a top-down orbit and out in space (where the old forward-only
/// bearing went undefined at nadir).
fn view_heading(tf: &Transform) -> Option<f32> {
    let fwd = tf.rotation * Vec3::NEG_Z;
    let up = tf.rotation * Vec3::Y;
    let fwd_flat = Vec3::new(fwd.x, 0.0, fwd.z);
    let up_flat = Vec3::new(up.x, 0.0, up.z);
    let h = if fwd_flat.length_squared() >= up_flat.length_squared() {
        fwd_flat
    } else {
        up_flat
    };
    (h.length_squared() > 1e-9).then(|| h.x.atan2(-h.z).to_degrees().rem_euclid(360.0))
}

/// Compass bearing in the space tier, where [`view_heading`]'s flat-frame notion
/// of north (−Z) is simply the wrong question: thousands of kilometres from the
/// anchor, "north up" means the Earth's **spin axis** points up the screen, which
/// is what the globe camera aligns to (`free_camera::globe_up`). Measuring the
/// local frame instead made the needle read a nonzero heading with the view
/// perfectly north-up — the compass lock looked broken while it was working.
///
/// `north` is `GlobeState::north_axis_local()`. The bearing is the screen angle
/// from up, clockwise, so it matches the rose's drawing convention: project the
/// axis onto the view plane by taking its components along camera right and up.
/// `None` only when looking straight down the axis, where north-up is undefined.
fn space_view_heading(tf: &Transform, north: Vec3) -> Option<f32> {
    let x = north.dot(tf.rotation * Vec3::X);
    let y = north.dot(tf.rotation * Vec3::Y);
    (x * x + y * y > 1e-9).then(|| x.atan2(y).to_degrees().rem_euclid(360.0))
}

/// Draw the status overlay and apply its copy / bookmark actions.
#[allow(clippy::too_many_arguments)]
pub fn render_hud(
    mut egui: ResMut<EguiManager>,
    mut clock: ResMut<WorldClock>,
    mut dev: ResMut<crate::resources::DevPanelState>,
    mut weather: ResMut<crate::systems::weather::WeatherState>,
    camera_state: Res<CameraState>,
    globe: Res<crate::systems::globe::GlobeState>,
    mut store: ResMut<UserStore>,
    // Immutable now that the `panel_open` latch is gone: `request_address` takes
    // `&self` and queues through interior mutability.
    bookmarks: ResMut<BookmarksState>,
    identity: Res<crate::resources::AuthIdentity>,
    driver: Res<crate::systems::driver::DriverState>,
    camera: Query<(&GridCell, &Transform), With<FreeCamera>>,
    mut right_panel: ResMut<crate::resources::RightPanel>,
    // (copy "✔", set-home "✔") flash timers, bundled into one `Local` to stay
    // within Bevy's 16-system-param limit.
    mut flash_at: Local<(Option<Instant>, Option<Instant>)>,
    map_anchor: Res<MapAnchor>,
) {
    // The status HUD lives in the top-right corner (see `hud_area`), which any
    // right-docked panel covers — and while driving the map has to stay clear for
    // the navigation chrome. So drop the HUD whenever a right panel is open, at any
    // width; egui paints floating Areas above panels, so it can't hide behind one.
    //
    // The gate comes *before* the data, not after: building `HudData` is four
    // `format!`s and a username clone, and every one of them was thrown away
    // unread on every frame with a panel open. See `render_map_controls` for why
    // `RightPanel` is the only operand.
    if !right_panel.is(crate::resources::RightPanel::None) || driver.active {
        return;
    }

    let ((h, m, s), is_local) = clock.hms();
    let cam = camera.single().ok();
    // Geocentric altitude (the local-y "altitude" is wrong away from the anchor).
    let altitude = cam.map(|(cell, tf)| globe.altitude_m(cell, tf));
    let data = HudData {
        // The Local/UTC label now lives in the toggle beside the clock, so the
        // time reads bare.
        time: format!("{h:02}:{m:02}:{s:02}"),
        utc: !is_local,
        sun_mode: dev.sun_mode,
        weather_live: weather.live,
        // Show the forecast only in Auto (in Manual the sim isn't using it).
        weather_info: weather.live.then(|| weather.latest).flatten().map(|o| {
            (
                format!(
                    "{:.0}% cloud · {:.0}°C",
                    o.cloud_cover * 100.0,
                    o.air_temp_c
                ),
                format!(
                    "{:.0}% cloud · {:.1} mm/h · {:.0}°C · {:.0} m/s wind",
                    o.cloud_cover * 100.0,
                    o.precipitation * crate::systems::weather::RAIN_FULL_MM,
                    o.air_temp_c,
                    o.wind_ms,
                ),
            )
        }),
        geo: cam
            .zip(map_anchor.geo)
            .map(|((cell, tf), anchor)| camera_geo(anchor, &globe, cell, tf)),
        altitude,
        // The heading is not here: it is the compass's, and the compass is
        // `render_map_controls`, which reads the camera itself.
        copied: flash_at.0.is_some_and(|t| t.elapsed().as_secs_f32() < 1.5),
        home_set: flash_at.1.is_some_and(|t| t.elapsed().as_secs_f32() < 1.5),
        user: identity.username.clone(),
    };

    let actions = hud_area(egui.ctx_mut(), &data);

    if actions.copy {
        if let (Some((lat, lng)), Some(alt)) = (data.geo, data.altitude) {
            // Copy the view, not just the spot: the look angles come off the
            // live camera transform (as `ui.rs`'s preset copy does), so they are
            // right in any camera mode, and pasting the string back — into the
            // search bar or a `?place=` URL — reproduces this exact framing.
            let look = cam.map(|(_, tf)| {
                let (yaw, pitch, _) = tf.rotation.to_euler(EulerRot::YXZ);
                (pitch.to_degrees(), yaw.to_degrees())
            });
            crate::utils::clipboard::set_text(&format_place(lat, lng, alt, look));
            flash_at.0 = Some(Instant::now());
        }
    }
    if actions.bookmark {
        if let (Some((lat, lng)), Some(alt)) = (data.geo, data.altitude) {
            // Capture the current view so the bookmark returns to it exactly.
            let pose = CameraPose::capture(&camera_state);
            let id = store.add_bookmark(PLACEHOLDER_NAME.to_string(), lat, lng, alt, pose);
            bookmarks.request_address(id, lat, lng);
            // Show the new place straight away — it lands at the top level and is
            // usually dragged into a folder next. `RightPanel` is the only state
            // that opens it; the `bookmarks.panel_open` that used to be set here as
            // well was a one-way latch nobody ever cleared, and setting it was what
            // made one ☆ press take the HUD and the compass away permanently.
            *right_panel = crate::resources::RightPanel::Places;
        }
    }
    if actions.set_home {
        if let (Some((lat, lng)), Some(alt)) = (data.geo, data.altitude) {
            // Capture the current view so Home returns to it exactly.
            store.set_home(lat, lng, alt, CameraPose::capture(&camera_state));
            flash_at.1 = Some(Instant::now());
        }
    }
    if actions.toggle_utc {
        clock.show_utc = !clock.show_utc;
    }
    if let Some(mode) = actions.set_sun_mode {
        dev.sun_mode = mode;
    }
    if let Some(live) = actions.set_weather_live {
        weather.live = live;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Straight down from 1 km with a 45° lens: the view is 2·1000·tan(22.5°)
    /// ≈ 828 m tall, so an 800 pt viewport is a hair over a metre per point.
    #[test]
    fn the_ground_scale_is_the_view_extent_over_the_viewport() {
        let per_pt = ground_metres_per_point(1000.0, 1.0, std::f32::consts::FRAC_PI_4, 800.0)
            .expect("straight down has a scale");
        assert!((per_pt - 0.828 / 0.8).abs() < 0.01, "{per_pt}");
        // Tilt halves the depression and the ground point doubles its distance,
        // so everything there is half as big on screen.
        let tilted = ground_metres_per_point(1000.0, 0.5, std::f32::consts::FRAC_PI_4, 800.0)
            .expect("a 30° view still has one");
        assert!((tilted / per_pt - 2.0).abs() < 1e-3, "{tilted} vs {per_pt}");
    }

    /// A view that includes the horizon has no single scale — the far end of the
    /// bar would be kilometres out of step with the near end — so it draws none
    /// rather than one that is right for a single row of pixels.
    #[test]
    fn a_horizon_view_has_no_scale() {
        assert_eq!(
            ground_metres_per_point(1000.0, 0.02, std::f32::consts::FRAC_PI_4, 800.0),
            None
        );
        // …and neither does a camera underground, or a viewport of nothing.
        assert_eq!(
            ground_metres_per_point(0.0, 1.0, std::f32::consts::FRAC_PI_4, 800.0),
            None
        );
        assert_eq!(
            ground_metres_per_point(1000.0, 1.0, std::f32::consts::FRAC_PI_4, 0.0),
            None
        );
    }

    /// 1-2-5 and nothing else: the bar is read as a number to carry to the next
    /// view, and 183 m is noise wearing the same clothes as 200 m.
    #[test]
    fn the_bar_is_the_longest_one_two_or_five_that_fits() {
        // 110 pt at 1 m/pt holds 110 m → 100 m.
        let (metres, pts) = nice_scale_bar(1.0, 110.0).unwrap();
        assert_eq!(metres, 100.0);
        assert!((pts - 100.0).abs() < 1e-3);
        // 110 pt at 2 m/pt holds 220 m → 200 m.
        assert_eq!(nice_scale_bar(2.0, 110.0).unwrap().0, 200.0);
        // …and at 5 m/pt, 550 m → 500 m, not 1 km.
        assert_eq!(nice_scale_bar(5.0, 110.0).unwrap().0, 500.0);
        for scale in [0.05_f32, 0.3, 1.0, 7.0, 40.0, 900.0, 12_000.0] {
            let (metres, pts) = nice_scale_bar(scale, 110.0).expect("every scale gets a bar");
            let mantissa = metres / 10_f32.powf(metres.log10().floor());
            assert!(
                [1.0, 2.0, 5.0].iter().any(|m| (m - mantissa).abs() < 1e-3),
                "{metres} m is not a 1-2-5 number"
            );
            assert!(pts <= 110.0 + 1e-3, "{pts} pt overruns the bar");
        }
    }

    #[test]
    fn the_label_is_metres_below_a_kilometre_and_kilometres_above() {
        assert_eq!(format_scale(200.0), "200 m");
        assert_eq!(format_scale(500.0), "500 m");
        assert_eq!(format_scale(1000.0), "1 km");
        assert_eq!(format_scale(20_000.0), "20 km");
    }

    /// The copy button's string is only useful if it parses back — this is the
    /// contract between the ⧉ button, the search bar, and the `?place=` URL.
    #[test]
    fn copied_place_parses_back() {
        use crate::systems::geo_nav::parse_location;

        let text = format_place(53.2194, 6.5665, 120.0, Some((-12.5, 45.0)));
        assert_eq!(text, "53.21940, 6.56650, 120m, -12.5°, 45.0°");
        let fix = parse_location(&text).expect("copied place parses");
        assert!((fix.lat - 53.2194).abs() < 1e-5 && (fix.lng - 6.5665).abs() < 1e-5);
        assert_eq!(fix.altitude, Some(120.0));
        assert_eq!(fix.look, Some((-12.5, 45.0)));

        // Southern/western and view-less places too (negatives keep their sign;
        // no camera means no look angles rather than zeroed ones).
        let plain = format_place(-33.9249, -18.4241, 1500.0, None);
        let fix = parse_location(&plain).expect("plain place parses");
        assert!((fix.lat + 33.9249).abs() < 1e-5 && (fix.lng + 18.4241).abs() < 1e-5);
        assert_eq!(fix.look, None);
    }

    /// With the globe camera north-up over a part of the planet far from the
    /// anchor, the compass must read 0 — the lock holds that view, so anything
    /// else means the needle is lying about a working lock. The flat-frame
    /// bearing is measured too, since it reading far off here is exactly why
    /// space needs its own heading.
    #[test]
    fn space_compass_reads_zero_when_north_is_up() {
        use crate::systems::globe::GlobeState;
        use bevy::math::DVec3;

        let mut globe = GlobeState::default();
        globe.radius = 6_378_137.0;
        globe.anchor_lat = 53.2194;
        globe.anchor_lng = 6.5665;
        globe.center_local = Vec3::new(0.0, -globe.radius, 0.0);
        globe.ready = true;

        let north = globe.north_axis_local();
        // Camera 3,000 km over the equator at 100°E — half a planet from the
        // anchor, where the two frames disagree most.
        let dir = globe.direction_local(0.0, 100.0).as_dvec3();
        let up = crate::systems::free_camera::globe_up(dir, north.as_dvec3(), 0.0, DVec3::Y);
        let cam = globe.center_local.as_dvec3() + dir * (globe.radius as f64 + 3_000_000.0);
        let tf =
            Transform::from_translation(cam.as_vec3()).looking_at(globe.center_local, up.as_vec3());

        let h = space_view_heading(&tf, north).expect("heading defined off the polar axis");
        assert!(
            h < 0.05 || h > 359.95,
            "space heading should be north-up, got {h}"
        );
        let flat = view_heading(&tf).expect("flat bearing defined");
        assert!(
            flat.min(360.0 - flat) > 5.0,
            "flat-frame bearing should disagree here (got {flat}) — if it doesn't, \
             this test no longer covers the mismatch it was written for"
        );
    }

    /// The overlay must not grow across frames: a size-to-content anchored
    /// Area with an unbounded inner row would stretch a little each frame
    /// (the bug this guards against). Render several frames alongside the side
    /// panels that shrink the central area, and assert the box stays put.
    #[test]
    fn hud_box_does_not_grow() {
        let ctx = egui::Context::default();
        // The real theme, not `Visuals::dark()`: the box's size is mostly spacing
        // and padding, so measuring it under egui's stock style would guard a
        // layout the app never renders.
        theme::install(&ctx);
        let data = HudData {
            time: "08:14:11".to_string(),
            utc: false,
            sun_mode: crate::resources::SunMode::Auto,
            weather_live: true,
            weather_info: Some((
                "60% cloud · 16°C".to_string(),
                "60% cloud · 16°C".to_string(),
            )),
            geo: Some((53.21952, 6.58220)),
            altitude: Some(17.0),
            copied: false,
            home_set: false,
            user: Some("dev".to_string()),
        };
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1323.0, 819.0));

        let mut sizes = Vec::new();
        for _ in 0..10 {
            let mut raw = egui::RawInput::default();
            raw.screen_rect = Some(screen);
            // egui 0.35: `Context::run` is `run_ui` and panels are shown inside a
            // root `Ui` rather than on the `Context` (`SidePanel` → `Panel`,
            // `default_width` → `default_size`) — same layout as `egui_manager`
            // drives in the live app.
            let out = ctx.run_ui(raw, |ui| {
                // Side panels present, like the live app.
                egui::Panel::right("layers_panel")
                    .default_size(260.0)
                    .show(ui, |ui| {
                        ui.heading("Layers");
                    });
                egui::Panel::right("bookmarks_panel")
                    .default_size(280.0)
                    .show(ui, |ui| {
                        ui.heading("Places");
                    });
                hud_area(&ui.ctx().clone(), &data);
            });
            // The HUD's frame is the largest translucent rect (the side panels
            // are opaque). `from_rgba_unmultiplied` premultiplies internally,
            // and the frame fades in over the first frames, so match the alpha
            // band loosely and take the biggest.
            let rect = out
                .shapes
                .iter()
                .filter_map(|cs| match &cs.shape {
                    egui::Shape::Rect(rs) if (1..250).contains(&rs.fill.a()) => {
                        Some(cs.shape.visual_bounding_rect())
                    }
                    _ => None,
                })
                .max_by(|a, b| (a.area()).total_cmp(&b.area()));
            sizes.push(rect.unwrap_or(egui::Rect::NOTHING));
        }

        // On the settled frames the box must be compact, on-screen, and stable
        // — the regression being an anchored Area that grows every frame.
        let last = *sizes.last().unwrap();
        let prev = sizes[sizes.len() - 2];
        assert!(last.width() < 220.0, "HUD too wide: {}", last.width());
        // 194 pt under the brand theme (was ~173 on egui's stock spacing — the
        // roomier rhythm is the point, and `hud_area` already tightens the box's
        // own padding back down). The bound is a compactness sanity check, not a
        // pixel pin: it is the runaway, caught by the stability assert below,
        // that this test exists for.
        assert!(last.height() < 200.0, "HUD too tall: {}", last.height());
        assert!(last.min.y >= 0.0, "HUD grew off the top: {}", last.min.y);
        assert!(
            screen.contains_rect(last),
            "HUD outside the screen: {last:?}"
        );
        assert!(
            (last.width() - prev.width()).abs() < 0.5
                && (last.height() - prev.height()).abs() < 0.5,
            "HUD size still changing: {prev:?} -> {last:?}"
        );
    }
}
