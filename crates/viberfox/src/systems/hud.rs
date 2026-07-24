//! Bottom-right status overlay: wall-clock time, the camera's geographic
//! position and altitude, a compass, and two location actions — ⧉ copies the
//! current location ("lat, lng, altitude", pasteable into the search bar) and
//! ☆ bookmarks it (named by reverse-geocoded address; see `bookmarks`).
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

use web_time::Instant;

use bevy::prelude::*;
use big_space::prelude::GridCell;

use crate::resources::CameraState;
use crate::systems::bookmarks::{BookmarksState, PLACEHOLDER_NAME};
use crate::systems::egui_manager::EguiManager;
use crate::systems::free_camera::FreeCamera;
use crate::systems::geo_nav::{camera_geo, CameraPose};
use crate::systems::map_stream::MapStream;
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
    pub heading: Option<f32>,
    /// Show the copy button's "just copied" ✔ state.
    pub copied: bool,
    /// Show the Set Home button's "just set" ✔ state.
    pub home_set: bool,
    /// Authenticated username, if online (ADR-016). Drives the profile chip;
    /// grows into a chat/presence affordance later.
    pub user: Option<String>,
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

/// Draws the top-right compass rose (Organic-Maps style): the red needle and
/// the "N" point to geographic north as it appears on screen. In globe (Orbit)
/// mode it is a toggle — clicking locks the view north-up (and unlocking
/// releases it); in the free-form fly/avatar modes it is a passive indicator.
/// `locked` tints the ring and adds a lock glyph. Returns true when the user
/// clicked it (only possible when `interactive`).
fn compass_overlay(ctx: &egui::Context, heading_deg: f32, interactive: bool, locked: bool) -> bool {
    let mut clicked = false;
    // Amber, the same "held" accent the locked ring and glyph share.
    let lock_color = egui::Color32::from_rgb(240, 190, 90);
    egui::Area::new(egui::Id::new("compass_overlay"))
        // Top-left: the status HUD moved into the top-right corner, so the compass
        // takes the opposite one to keep both clear of the docked panels.
        .anchor(egui::Align2::LEFT_TOP, egui::vec2(12.0, 12.0))
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
            // Disc + ring (ring brightens on hover to signal it's clickable, and
            // goes amber while the view is locked north-up).
            p.circle_filled(c, r, egui::Color32::from_rgba_unmultiplied(10, 14, 22, 210));
            let ring = if locked {
                lock_color
            } else {
                egui::Color32::from_gray(if hovered { 210 } else { 130 })
            };
            p.circle_stroke(c, r, egui::Stroke::new(if locked { 2.0 } else { 1.5 }, ring));
            // Screen direction of north when screen-up is the view heading.
            let th = heading_deg.to_radians();
            let north = egui::vec2(-th.sin(), -th.cos());
            let side = egui::vec2(north.y, -north.x); // 90° clockwise from north
            let w = side * 4.5;
            // North half: red arrow; south half: light grey.
            p.add(egui::Shape::convex_polygon(
                vec![c + north * (r - 3.0), c + w, c - w],
                egui::Color32::from_rgb(230, 80, 70),
                egui::Stroke::NONE,
            ));
            p.add(egui::Shape::convex_polygon(
                vec![c - north * (r - 7.0), c + w, c - w],
                egui::Color32::from_gray(190),
                egui::Stroke::NONE,
            ));
            // "N" over the north tip.
            p.text(
                c + north * (r - 11.0),
                egui::Align2::CENTER_CENTER,
                "N",
                egui::FontId::proportional(10.0),
                egui::Color32::WHITE,
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
            if interactive {
                resp.clone().on_hover_text(if locked {
                    "North-up locked — click to release"
                } else {
                    "Lock the view north-up"
                });
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
/// grey outline when off, a filled blue arrow while following, and a filled amber
/// arrow while follow-and-rotate (the amber echoes the compass's north-up lock).
/// Drawn from primitives, like the compass — egui's bundled font carries few
/// emoji, so a glyph would risk rendering as tofu.
fn locate_overlay(ctx: &egui::Context, state: LocateState) -> bool {
    let mut clicked = false;
    let accent = egui::Color32::from_rgb(90, 170, 240);
    let rotate_accent = egui::Color32::from_rgb(240, 190, 90);
    egui::Area::new(egui::Id::new("locate_overlay"))
        // Directly under the compass (46 pt tall, anchored at (12, 12)), centred on
        // its column.
        .anchor(
            egui::Align2::LEFT_TOP,
            egui::vec2(12.0 + (COMPASS_SIZE - LOCATE_SIZE) * 0.5, 12.0 + COMPASS_SIZE + 8.0),
        )
        .show(ctx, |ui| {
            let (rect, resp) =
                ui.allocate_exact_size(egui::vec2(LOCATE_SIZE, LOCATE_SIZE), egui::Sense::click());
            let hovered = resp.hovered();
            let p = ui.painter();
            let c = rect.center();
            let r = LOCATE_SIZE * 0.5 - 2.0;
            p.circle_filled(c, r, egui::Color32::from_rgba_unmultiplied(10, 14, 22, 210));
            let (mark, ring, filled) = match state {
                LocateState::Off => (
                    egui::Color32::from_gray(if hovered { 220 } else { 175 }),
                    egui::Color32::from_gray(if hovered { 210 } else { 130 }),
                    false,
                ),
                LocateState::Follow => (accent, accent, true),
                LocateState::FollowRotate => (rotate_accent, rotate_accent, true),
            };
            p.circle_stroke(c, r, egui::Stroke::new(if filled { 2.0 } else { 1.5 }, ring));
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

/// Build the overlay's anchored box and return what was clicked. Pure UI over
/// [`HudData`], so it can be exercised headless (see the test below).
pub fn hud_area(ctx: &egui::Context, data: &HudData) -> HudActions {
    let mut actions = HudActions::default();
    let mono = |ui: &mut egui::Ui, text: String, size: f32, gray: u8| {
        ui.label(
            egui::RichText::new(text)
                .monospace()
                .size(size)
                .color(egui::Color32::from_gray(gray)),
        );
    };

    // Top-right: keeps clear of the bottom chat/action bar. The caller drops the
    // whole HUD while a right-docked panel is open, so the two never coexist here.
    egui::Area::new(egui::Id::new("status_hud"))
        .anchor(egui::Align2::RIGHT_TOP, egui::vec2(-10.0, 10.0))
        .show(ctx, |ui| {
            egui::Frame::NONE
                .fill(egui::Color32::from_rgba_unmultiplied(10, 14, 22, 200))
                .corner_radius(4.0)
                .inner_margin(egui::Margin::symmetric(9, 6))
                .show(ui, |ui| {
                    // Pin the width first — this is what stops the runaway.
                    ui.set_width(HUD_WIDTH);
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 2.0;
                        // Profile chip: a placeholder avatar dot with the account
                        // initial, plus the username. Extends into a presence/chat
                        // affordance later (ADR-016 / ADR-028).
                        if let Some(user) = &data.user {
                            ui.horizontal(|ui| {
                                let (rect, _) = ui.allocate_exact_size(
                                    egui::vec2(15.0, 15.0),
                                    egui::Sense::hover(),
                                );
                                let c = rect.center();
                                ui.painter().circle_filled(
                                    c,
                                    7.0,
                                    egui::Color32::from_rgb(86, 148, 232),
                                );
                                if let Some(ch) = user.chars().next() {
                                    ui.painter().text(
                                        c,
                                        egui::Align2::CENTER_CENTER,
                                        ch.to_uppercase().to_string(),
                                        egui::FontId::proportional(9.5),
                                        egui::Color32::WHITE,
                                    );
                                }
                                ui.label(
                                    egui::RichText::new(user)
                                        .size(12.5)
                                        .color(egui::Color32::from_gray(232)),
                                );
                            });
                        }
                        // Clock + a small Local/UTC toggle, then the sun-mode
                        // switcher (Auto/Day/Night) — a light/dark/auto control
                        // for the sky, the way a website theme toggle reads.
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 6.0;
                            mono(ui, data.time.clone(), 15.0, 235);
                            let zone = if data.utc { "UTC" } else { "Local" };
                            if ui
                                .add(egui::Button::new(egui::RichText::new(zone).size(10.5)).small())
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
                                            egui::RichText::new(label).size(11.0),
                                        )
                                        .on_hover_text(tip)
                                        .clicked()
                                    {
                                        actions.set_sun_mode = Some(mode);
                                    }
                                };
                            choice(ui, SunMode::Auto, "Auto", "Sun follows the real local time");
                            choice(ui, SunMode::Day, "☀", "Force daylight (noon)");
                            choice(ui, SunMode::Night, "🌙", "Force night (midnight)");
                        });
                        // Weather: an Auto/Manual toggle mirroring the sun switcher,
                        // plus the live forecast readout for the map location.
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 3.0;
                            if ui
                                .selectable_label(
                                    data.weather_live,
                                    egui::RichText::new("Auto").size(11.0),
                                )
                                .on_hover_text("Live weather for the map location (met.no)")
                                .clicked()
                            {
                                actions.set_weather_live = Some(true);
                            }
                            if ui
                                .selectable_label(
                                    !data.weather_live,
                                    egui::RichText::new("Manual").size(11.0),
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
                                    .size(11.0)
                                    .color(egui::Color32::from_gray(170)),
                            )
                            .on_hover_text(full);
                        }
                        if let Some((lat, lng)) = data.geo {
                            let ns = if lat >= 0.0 { 'N' } else { 'S' };
                            let ew = if lng >= 0.0 { 'E' } else { 'W' };
                            mono(ui, format!("{:.5}° {}", lat.abs(), ns), 11.5, 170);
                            mono(ui, format!("{:.5}° {}", lng.abs(), ew), 11.5, 170);
                        }
                        if let Some(alt) = data.altitude {
                            let alt = alt.max(0.0);
                            let text = if alt >= 1000.0 {
                                format!("alt {:.1} km", alt / 1000.0)
                            } else {
                                format!("alt {alt:.0} m")
                            };
                            mono(ui, text, 11.5, 170);
                        }
                        // Heading now lives in the top-right compass overlay.
                        // Location actions.
                        if data.geo.is_some() && data.altitude.is_some() {
                            ui.horizontal(|ui| {
                                if ui
                                    .add(egui::Button::new(if data.home_set { "✔" } else { "🏠" }))
                                    .on_hover_text(
                                        "Set Home (the nav bar's Home button returns here)",
                                    )
                                    .clicked()
                                {
                                    actions.set_home = true;
                                }
                                if ui
                                    .add(egui::Button::new(if data.copied { "✔" } else { "⧉" }))
                                    .on_hover_text(
                                        "Copy location (paste in the search bar to return)",
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
    stream: Res<MapStream>,
    mut camera_state: ResMut<CameraState>,
    globe: Res<crate::systems::globe::GlobeState>,
    mut store: ResMut<UserStore>,
    mut bookmarks: ResMut<BookmarksState>,
    data_layers: Res<crate::systems::data_layers::DataLayers>,
    ai: Res<crate::resources::AiAssistantState>,
    identity: Res<crate::resources::AuthIdentity>,
    camera: Query<(&GridCell, &Transform), With<FreeCamera>>,
    avatars: Query<(), With<crate::components::Avatar>>,
    right_panel: Res<crate::resources::RightPanel>,
    // (copy "✔", set-home "✔") flash timers, bundled into one `Local` to stay
    // within Bevy's 16-system-param limit.
    mut flash_at: Local<(Option<Instant>, Option<Instant>)>,
) {
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
                format!("{:.0}% cloud · {:.0}°C", o.cloud_cover * 100.0, o.air_temp_c),
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
            .zip(stream.geo_anchor())
            .map(|((cell, tf), anchor)| camera_geo(anchor, &globe, cell, tf)),
        altitude,
        // Over the globe the bearing is measured against the spin axis, not the
        // local flat frame (see `space_view_heading`).
        heading: cam.and_then(|(cell, tf)| {
            if globe.in_space(cell, tf) {
                space_view_heading(tf, globe.north_axis_local())
            } else {
                view_heading(tf)
            }
        }),
        copied: flash_at.0.is_some_and(|t| t.elapsed().as_secs_f32() < 1.5),
        home_set: flash_at.1.is_some_and(|t| t.elapsed().as_secs_f32() < 1.5),
        user: identity.username.clone(),
    };

    // Any docked right-hand panel covers this corner; `RightPanel` is the shared
    // state the newer panels use, the booleans the older ones do.
    let right_panel_open = data_layers.panel_open
        || bookmarks.panel_open
        || ai.open
        || !right_panel.is(crate::resources::RightPanel::None);

    // The status HUD now lives in the top-right corner (see `hud_area`), which any
    // right-docked panel covers — and while routing especially we want an
    // uncluttered map. So drop the HUD whenever a right panel is open, at any
    // width; egui paints floating Areas above panels, so it can't hide behind one.
    let actions = if right_panel_open {
        HudActions::default()
    } else {
        hud_area(egui.ctx_mut(), &data)
    };

    // Top-right compass. Clicking it toggles the north-up lock in globe (Orbit)
    // mode only; fly/avatar are free-form, so there it is a passive indicator.
    //
    // A right-docked panel (Layers, Bookmarks, AI) sits opaque over this same
    // corner. egui always paints floating Areas on top of panels (panels live
    // on the bottom-most background layer, and even `Order::Background` Areas
    // paint above it), so the only way to keep the compass *behind* the panel
    // is to drop it while one is open.
    if let Some(heading) = data.heading.filter(|_| !right_panel_open) {
        let orbit = matches!(camera_state.mode, crate::resources::CameraMode::Orbit);
        let locked = orbit && camera_state.orbit_north_lock;
        if compass_overlay(egui.ctx_mut(), heading, orbit, locked) {
            // Toggle the lock; engaging it also eases the view back to north
            // (the lock holds it there from then on).
            camera_state.orbit_north_lock = !locked;
            camera_state.orbit_north_reset = camera_state.orbit_north_lock;
        }
    }

    // GPS / locate button, stacked under the compass (Organic-Maps style). Only in
    // the viewer that owns a local avatar — Follow doesn't exist otherwise — and
    // dropped alongside the compass when a right panel covers the map.
    if !avatars.is_empty() && !right_panel_open {
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

    if actions.copy {
        if let (Some((lat, lng)), Some(alt)) = (data.geo, data.altitude) {
            crate::utils::clipboard::set_text(&format!("{lat:.5}, {lng:.5}, {alt:.0}"));
            flash_at.0 = Some(Instant::now());
        }
    }
    if actions.bookmark {
        if let (Some((lat, lng)), Some(alt)) = (data.geo, data.altitude) {
            // Capture the current view so the bookmark returns to it exactly.
            let pose = CameraPose::capture(&camera_state);
            let id = store.add_bookmark(PLACEHOLDER_NAME.to_string(), lat, lng, alt, pose);
            bookmarks.request_address(id, lat, lng);
            bookmarks.panel_open = true;
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
        assert!(h < 0.05 || h > 359.95, "space heading should be north-up, got {h}");
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
        ctx.set_visuals(egui::Visuals::dark());
        let data = HudData {
            time: "08:14:11".to_string(),
            utc: false,
            sun_mode: crate::resources::SunMode::Auto,
            weather_live: true,
            weather_info: Some(("60% cloud · 16°C".to_string(), "60% cloud · 16°C".to_string())),
            geo: Some((53.21952, 6.58220)),
            altitude: Some(17.0),
            heading: Some(247.0),
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
        assert!(last.height() < 180.0, "HUD too tall: {}", last.height());
        assert!(last.min.y >= 0.0, "HUD grew off the top: {}", last.min.y);
        assert!(screen.contains_rect(last), "HUD outside the screen: {last:?}");
        assert!(
            (last.width() - prev.width()).abs() < 0.5
                && (last.height() - prev.height()).abs() < 0.5,
            "HUD size still changing: {prev:?} -> {last:?}"
        );
    }
}
