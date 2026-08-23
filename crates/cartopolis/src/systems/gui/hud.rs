//! Top-right status overlay: wall-clock time, the camera's geographic position
//! and altitude, the sun and weather switchers — and, in the opposite corner, the
//! compass and the locate button.
//!
//! **It is off by default** ([`StatusHud`]). It is an instrument panel — six rows
//! of numbers and chips over the top-right of a map — and the two *verbs* it used
//! to carry have gone where a reader will find them without it: ☆ bookmark into
//! the Places panel (which is where the place then appears), and Set Home and
//! Copy location into the main menu. What is left is a readout, and a readout
//! that is up whether or not anyone is reading it is just chrome over the map.
//!
//! **The switch is in the main menu, not on the map.** It was a small ⓘ disc at
//! the foot of the compass column, and it cost that column its only argument for
//! being a column: three discs the same size on one axis, all of them controls
//! *for the view*. A fourth at 28 pt, for a panel of numbers nobody flips while
//! moving, read as a mis-sized member of the set rather than as a different kind
//! of thing — the size was doing the work a position should have done. The main
//! menu is where the readout's own verbs already went, and where the other
//! inspection toggle (developer tools) lives.
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

use bevy::prelude::*;
use big_space::prelude::GridCell;

use crate::components::FreeCamera;
use crate::resources::CameraState;
use crate::systems::egui_manager::EguiManager;
use crate::systems::geo_nav::camera_geo;
use crate::systems::theme;
use crate::systems::transit_live::WorldClock;

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
    /// Authenticated username, if online (ADR-016). Drives the profile chip;
    /// grows into a chat/presence affordance later.
    pub user: Option<String>,
}

// `format_place` lived here — "53.21940, 6.56650, 120m, -12.5°, 45.0°", the
// readable spelling the copy button used to write. It has no writer any more:
// **Search ▸ Copy link** copies a URL (`systems::place_link`), whose value is the same
// five numbers without the decoration, and a format with no writer is a format
// that drifts. `geo_nav::parse_location` still *reads* the readable form, because
// people have that string in old chat messages and it is what a person types.

/// What the user clicked in the overlay this frame.
///
/// The location *actions* that used to be here — copy, bookmark, Set Home — are
/// gone: see the module note. What is left is the three switchers, which are
/// controls for the readout itself.
#[derive(Default)]
pub struct HudActions {
    /// User clicked the Local/UTC clock toggle.
    pub toggle_utc: bool,
    /// User picked a new sun mode from the Auto/Day/Night switcher.
    pub set_sun_mode: Option<crate::resources::SunMode>,
    /// User toggled live (Auto) weather on/off from the weather switcher.
    pub set_weather_live: Option<bool>,
}

/// Diameter (points) of the compass overlay — [`theme::ROUND_BUTTON_SIZE`], the
/// one disc this app draws over a map.
///
/// It was 46 against the dock's 40 and the on-screen controls' 44, which put
/// three diameters within one screen and left the top-left column visibly
/// ragged: the compass bulged 3 pt past the layers disc above it and the locate
/// button below, and the two of them were each centred on a different axis.
/// `theme::ROUND_BUTTON_SIZE`'s own doc claimed it "matches the compass disc"
/// while they were six points apart; now it does.
const COMPASS_SIZE: f32 = theme::ROUND_BUTTON_SIZE;

/// The bottom edge of the top-left disc column: layers disc
/// ([`crate::systems::dock::reserved_top`]), compass, locate button.
///
/// **Anything else anchored to the top-left corner has to start below this**, and
/// the frame overlay is why the figure is a function rather than a comment. It
/// was anchored at a hardcoded `y = 40` — written when a menu *bar* owned the top
/// of the screen — and the column that replaced the bar draws straight over it,
/// so the frame rate somebody switched on to read was behind two discs.
///
/// The locate button is counted even when it is not drawn (it needs a local
/// avatar). A readout that changes position depending on whether an avatar exists
/// is worse than one that leaves a gap: the gap is stable and the jump is not.
#[must_use]
pub fn left_column_bottom() -> f32 {
    crate::systems::dock::reserved_top() + COMPASS_SIZE + theme::OSD_GAP + theme::ROUND_BUTTON_SIZE
}

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
        .anchor(
            egui::Align2::LEFT_TOP,
            egui::vec2(theme::OSD_MARGIN, cfg.top),
        )
        .show(ctx, |ui| {
            let sense = if interactive {
                egui::Sense::click()
            } else {
                egui::Sense::hover()
            };
            // The plate is `theme::map_disc`'s: the same fill, ring, hover and
            // focus states the dock discs and the on-screen controls wear.
            //
            // `on` is deliberately false even when locked. The engaged plate is
            // the muted orange a *panel* wears when it is open, and the needle it
            // has to carry is itself orange — the rose would lose its one piece
            // of colour vocabulary to the state of a lock. North-up keeps the
            // treatment it was designed with instead: the amber ring the palette
            // reserves for a held state, plus the padlock below.
            let (resp, face) = theme::map_disc(ui, COMPASS_SIZE, false, lock_color, sense);
            let p = ui.painter();
            let c = face.center;
            let r = face.radius;
            if locked {
                p.circle_stroke(c, r, egui::Stroke::new(2.0, lock_color));
            }
            // Screen direction of north when screen-up is the view heading.
            let th = heading_deg.to_radians();
            let north = egui::vec2(-th.sin(), -th.cos());
            let side = egui::vec2(north.y, -north.x); // 90° clockwise from north
            let w = side * (r * 0.24);
            // Every dimension is a fraction of the disc, so [`COMPASS_SIZE`] is
            // the one knob — the offsets used to be absolute points tuned to a
            // 46 pt rose and would have crowded the needle into the "N" the
            // moment the disc joined the rest of the family at 40.
            //
            // North half: the accent (a compass needle is red by convention,
            // and the brand's warm end is the closest thing to it we own); south
            // half: the muted body tone.
            p.add(egui::Shape::convex_polygon(
                vec![c + north * (r * 0.86), c + w, c - w],
                theme::ACCENT,
                egui::Stroke::NONE,
            ));
            p.add(egui::Shape::convex_polygon(
                vec![c - north * (r * 0.64), c + w, c - w],
                theme::TEXT_DIM,
                egui::Stroke::NONE,
            ));
            // "N" over the north tip.
            p.text(
                c + north * (r * 0.47),
                egui::Align2::CENTER_CENTER,
                "N",
                egui::FontId::proportional(theme::SIZE_TINY),
                face.ink,
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
                    egui::pos2(c.x, c.y + r + 9.0),
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

/// Diameter (points) of the GPS / locate button, stacked under the compass —
/// the same disc as everything else in this column.
const LOCATE_SIZE: f32 = theme::ROUND_BUTTON_SIZE;

/// Left offset of a disc of `size` in the top-left column, so that everything
/// stacked there shares one centre line.
///
/// One function because there were three copies of this arithmetic against two
/// different reference widths, and the layers disc at the head of the column had
/// no copy at all — it was anchored flush to the margin, which left it 3 pt off
/// the axis of the three discs under it.
#[must_use]
fn column_x(size: f32) -> f32 {
    theme::OSD_MARGIN + (theme::ROUND_BUTTON_SIZE - size) * 0.5
}

/// Locate-button state. Two, not three: see the module note on
/// [`crate::systems::locate`] for why the plain follow-without-rotate rung came
/// out of the cycle. Derived from [`crate::systems::locate::LocateTracking`] —
/// not from the camera, which can drop follow-and-rotate on its own and would
/// leave the button unlit over a running GNSS radio.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum LocateState {
    /// Not tracking — a click starts it.
    Off,
    /// Tracking the device: its position moves the avatar, its compass turns the
    /// view. Where there is no device provider (desktop) this is the avatar's own
    /// position and facing, which is the same promise against the only sensors
    /// that exist there.
    Tracking,
}

/// Draws the GPS/locate button beneath the compass (Organic-Maps style) and
/// returns true when clicked. The icon is a navigation "location arrow": a hollow
/// muted outline when off and a filled amber arrow while tracking.
/// Drawn from primitives, like the compass — egui's bundled font carries few
/// emoji, so a glyph would risk rendering as tofu.
///
/// `hover` is the whole caption, because what tracking *means* depends on what
/// the device could actually answer with: a phone reporting a heading, a phone
/// that has refused the permission and a desktop that has no sensor at all are
/// three different situations behind one lit button, and the tooltip is where
/// they are told apart.
fn locate_overlay(ctx: &egui::Context, top: f32, state: LocateState, hover: &str) -> bool {
    let mut clicked = false;
    egui::Area::new(egui::Id::new("locate_overlay"))
        // Directly under the compass, centred on its column.
        .anchor(
            egui::Align2::LEFT_TOP,
            egui::vec2(column_x(LOCATE_SIZE), top),
        )
        .show(ctx, |ui| {
            // Tracking is an engaged state, so it takes the plate the dock's
            // open panels and every other engaged control take, and the reserved
            // amber ring — which is exactly how the compass says "north-up held"
            // one disc above. Both are states the app is *holding* for you.
            let (tone, on) = match state {
                LocateState::Off => (theme::ACCENT_RING, false),
                LocateState::Tracking => (theme::AMBER, true),
            };
            let (resp, face) = theme::map_disc(ui, LOCATE_SIZE, on, tone, egui::Sense::click());
            let p = ui.painter();
            let c = face.center;
            // Navigation "location arrow" (a kite), pointing up.
            let s = LOCATE_SIZE * 0.30;
            let arrow = vec![
                c + egui::vec2(0.0, -s),
                c + egui::vec2(-0.62 * s, 0.72 * s),
                c + egui::vec2(0.0, 0.30 * s),
                c + egui::vec2(0.62 * s, 0.72 * s),
            ];
            if on {
                p.add(egui::Shape::convex_polygon(
                    arrow,
                    face.ink,
                    egui::Stroke::NONE,
                ));
            } else {
                let mut outline = arrow;
                outline.push(outline[0]);
                p.add(egui::Shape::line(outline, egui::Stroke::new(1.6, face.ink)));
            }
            resp.clone().on_hover_text(hover);
            clicked = resp.clicked();
        });
    clicked
}

/// The locate button's caption: what pressing it will do, or — while it is lit —
/// what the device is actually telling us.
///
/// Pure over the tracking state so the four situations it distinguishes can be
/// exercised without a phone (see the tests). The lit ones report the compass
/// bearing when there is one, because "tracking" and "tracking, but the compass
/// hasn't reported yet" look identical on the button and feel identical to a user
/// wondering why the map is not turning.
fn locate_hover(state: LocateState, tracking: &crate::systems::locate::LocateTracking) -> String {
    use crate::platform::orientation::OrientationWatch;
    match state {
        LocateState::Off if OrientationWatch::available() => {
            "Track this device — my position, and the compass turns the map".to_string()
        }
        LocateState::Off => "Center on my location and follow my heading".to_string(),
        LocateState::Tracking => {
            let mut s = match tracking.heading {
                Some(h) => {
                    let mut line =
                        format!("Tracking this device — heading {}°", h.bearing_deg.round());
                    // Only iOS reports one, and only once the magnetometer has
                    // settled. A wide figure here is the explanation for a map
                    // that is pointing *nearly* the right way.
                    if let Some(a) = h.accuracy_deg {
                        line.push_str(&format!(" ±{}°", a.round()));
                    }
                    line
                }
                None if OrientationWatch::available() => {
                    "Tracking this device — waiting for the compass".to_string()
                }
                None => "Following — this device has no compass".to_string(),
            };
            if let Some(error) = &tracking.error {
                // Appended rather than replacing: the mode really is on, and a
                // caption that said only "denied" over a lit button would leave
                // nothing to explain what the button is still doing. The warning
                // sign in front of it had to wait for the icon font — `⚠` was in
                // none of the fonts egui ships, and a tofu box is a worse sign of
                // trouble than none.
                s.push('\n');
                s.push_str(crate::systems::icons::WARNING);
                s.push(' ');
                s.push_str(error);
            }
            s.push_str("\nClick to stop");
            s
        }
    }
}

/// Whether the status readout is on screen. Off by default — see the module note.
/// Switched from the main menu (`ui::render_main_menu`), not from the map.
#[derive(Resource, Default)]
pub struct StatusHud {
    pub open: bool,
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

/// Draw the bar in the bottom-left corner: a bracket and its length.
///
/// It was bottom-**right**, and that corner is the on-screen controls' — the
/// climb/zoom pair and the jump/flight pair both stack up from the dock line
/// there, and the bar was drawn 10 pt into the lowest of them. Two overlapping
/// `Area`s is not merely untidy: the bar is `interactable(false)`, so the
/// collision was invisible until a thumb landed on the overlap and pressed a
/// button it could not see the bottom of.
///
/// Bottom-left is where a web map has always put a scale, and it is free at
/// both tiers: the dock is *centred* on the bottom edge, so a desktop has the
/// whole corner, and on the touch tier `bottom_left_reserved` lifts the bar over
/// the thumbstick rather than guessing at it.
fn scale_bar_overlay(ctx: &egui::Context, metres: f32, pts: f32, bottom_reserved: f32) {
    egui::Area::new(egui::Id::new("scale_bar"))
        .anchor(
            egui::Align2::LEFT_BOTTOM,
            egui::vec2(theme::OSD_MARGIN, -(bottom_reserved + theme::OSD_GAP)),
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
            // The label sits over the left end now that the bar is anchored
            // there — over the *far* end it reads as belonging to whatever is
            // beyond it rather than to the rule under it.
            p.text(
                egui::pos2(rect.left(), base - 6.0),
                egui::Align2::LEFT_BOTTOM,
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
    egui: Res<EguiManager>,
    mut camera_state: ResMut<CameraState>,
    mut driver: ResMut<crate::systems::driver::DriverState>,
    globe: Res<crate::systems::globe::GlobeState>,
    right_panel: Res<crate::resources::RightPanel>,
    mut tracking: ResMut<crate::systems::locate::LocateTracking>,
    camera: Query<(&GridCell, &Transform, &Projection), With<FreeCamera>>,
    avatars: Query<(), With<crate::components::Avatar>>,
    keyboard: Res<crate::systems::onscreen_keyboard::OnScreenKeyboard>,
    touch: Res<crate::systems::touch::TouchGestures>,
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
            // What the bottom-left corner already holds. `cluster_for` is a pure
            // function of the same state `map_osd` reads, so asking it here costs
            // two resources and cannot disagree with what is actually drawn —
            // where a second copy of the rule ("is there a stick right now?")
            // would drift the first time either side gained a suppression.
            let reserved = crate::systems::map_osd::bottom_left_reserved(
                crate::systems::map_osd::cluster_for(
                    camera_state.mode,
                    driver.active,
                    !right_panel.is(crate::resources::RightPanel::None),
                    keyboard.is_open(),
                    theme::touch_layout() || touch.ever(),
                ),
            );
            scale_bar_overlay(egui.ctx(), metres, pts, reserved);
        }
    }

    if driver.active {
        // Driving: the rose steps through the navigation orientations
        // (`driver::MapOrientation`), and stacks under the manoeuvre card that has
        // taken the corner.
        use crate::systems::driver::{CARD_HEIGHT, CARD_MARGIN};
        let cfg = Compass {
            interactive: true,
            locked: driver.orientation == crate::systems::driver::MapOrientation::NorthUp,
            top: CARD_MARGIN + CARD_HEIGHT + 8.0,
            caption: Some(driver.orientation.label()),
            hover: "Change the map orientation (heading up / north up / 3D)",
        };
        if compass_overlay(egui.ctx(), heading, &cfg) {
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
        // Below the layers disc, which owns the very top of this column
        // (`dock::reserved_top`) — the address bar's mark used to sit there.
        top: crate::systems::dock::reserved_top(),
        caption: None,
        hover: if locked {
            "North-up locked — click to release"
        } else {
            "Lock the view north-up"
        },
    };
    if compass_overlay(egui.ctx(), heading, &cfg) {
        // Toggle the lock; engaging it also eases the view back to north (the lock
        // holds it there from then on).
        camera_state.orbit_north_lock = !locked;
        camera_state.orbit_north_reset = camera_state.orbit_north_lock;
    }

    // GPS / locate button, stacked under the compass (Organic-Maps style). Only in
    // the viewer that owns a local avatar — and deliberately *not* gated on a
    // session, unlike the mode switcher's Follow: "where am I on the map" is a
    // traditional map-viewer verb and offline is exactly where a viewer lives.
    // It takes no camera mode of its own (`systems::locate`), so there is nothing
    // for the offline mode guard to exempt: the button answers where you are in
    // whatever view you were already using. Not drawn while driving: the arrow is
    // already centred and the camera is held, so there is nothing left for it to
    // do.
    // The bottom edge of what the column has drawn so far; each control below
    // adds its own gap before its own height.
    let column = crate::systems::dock::reserved_top() + COMPASS_SIZE;
    if !avatars.is_empty() {
        let state = if tracking.active {
            LocateState::Tracking
        } else {
            LocateState::Off
        };
        let hover = locate_hover(state, &tracking);
        if locate_overlay(egui.ctx(), column + theme::OSD_GAP, state, &hover) {
            match state {
                // Recenter, in the view the user is already in. The button used to
                // force Follow on the way in, which made "where am I" a one-way
                // door out of Fly and Earth; `systems::locate` moves the camera
                // that is there instead, and owns the rotate-follow rig too — it
                // has to, because arriving in Follow later (Tab, the mode disc)
                // must arm the same thing this press would have.
                LocateState::Off => tracking.enter(),
                LocateState::Tracking => {
                    camera_state.follow_rotate = false;
                    tracking.exit();
                }
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
                // 20 pt is 30 dp on a phone. It predates
                // `theme::MIN_TOUCH_TARGET_DP` and it is applied to the two
                // switchers *and* the clock toggle, so the whole box was under
                // the guideline on the tier that most needs it.
                ui.spacing_mut().interact_size.y = if theme::touch_layout() {
                    theme::row_height()
                } else {
                    20.0
                };
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
                        // `theme::chip`, not `Button::small()`: `small()` zeroes
                        // the vertical padding *and* skips the `interact_size`
                        // floor, so this was a ~13.5 pt widget — 20 dp under a
                        // finger against a 48 dp guideline. A chip rather than a
                        // plain button because that is what the two rows under it
                        // are, and this box is small enough that a third control
                        // style in it is visible as one.
                        if theme::chip(ui, zone, false)
                            .on_hover_text("Toggle the clock between local time and UTC")
                            .clicked()
                        {
                            actions.toggle_utc = true;
                        }
                    });
                    // **Both switchers are named.** They were two anonymous rows
                    // whose first chip in each read "Auto", one above the other,
                    // with nothing on screen to say that one was the sun and the
                    // other the weather — and the tooltip that did say so is the
                    // one affordance a touch screen has no way to reach.
                    let caption = |ui: &mut egui::Ui, text: &str| {
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(text)
                                    .size(theme::SIZE_TINY)
                                    .color(theme::TEXT_DIM),
                            )
                            .selectable(false),
                        );
                    };
                    ui.horizontal(|ui| {
                        use crate::resources::SunMode;
                        ui.spacing_mut().item_spacing.x = 3.0;
                        caption(ui, "Sun");
                        let mut choice =
                            |ui: &mut egui::Ui, mode: SunMode, label: &str, tip: &str| {
                                if theme::chip(ui, label, data.sun_mode == mode)
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
                        caption(ui, "Sky");
                        if theme::chip(ui, "Auto", data.weather_live)
                            .on_hover_text("Live weather for the map location (met.no)")
                            .clicked()
                        {
                            actions.set_weather_live = Some(true);
                        }
                        if theme::chip(ui, "Manual", !data.weather_live)
                            // "▸" would tofu — it is not in the bundled font.
                            .on_hover_text(
                                "Hold the weather; the sliders are in Settings, under World",
                            )
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
                    // Heading is the compass's, in the opposite corner; the
                    // location actions are the main menu's and the Places panel's
                    // (see the module note).
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
    egui: Res<EguiManager>,
    mut clock: ResMut<WorldClock>,
    mut dev: ResMut<crate::resources::DevPanelState>,
    mut weather: ResMut<crate::systems::weather::WeatherState>,
    globe: Res<crate::systems::globe::GlobeState>,
    identity: Res<crate::resources::AuthIdentity>,
    driver: Res<crate::systems::driver::DriverState>,
    camera: Query<(&GridCell, &Transform), With<FreeCamera>>,
    right_panel: Res<crate::resources::RightPanel>,
    status: Res<StatusHud>,
    map_anchor: Res<MapAnchor>,
) {
    // Off unless the ⓘ disc has been pressed (see the module note). Before the
    // panel gate and the data below, both of which cost work every frame.
    if !status.open {
        return;
    }
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
    // The clock ticks and the altitude changes while the camera flies, neither of
    // which is egui input — without this the readout is frozen at whatever it said
    // when the pointer last moved. See `EguiManager::animating`.
    egui.animating();
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
        user: identity.username.clone(),
    };

    let actions = hud_area(egui.ctx(), &data);

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

    // `copied_place_parses_back` lived here, over `format_place`. Both moved:
    // the copy button writes a link now, so the round trip it has to survive is
    // `place_link::web_link` → `place_link::place_from_link`, and the test that
    // pins it lives beside them.

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

    /// **The column is one column: one axis, one diameter.** Both halves of that
    /// are load-bearing and both were broken. The layers disc at the head is
    /// `dock`'s, anchored flush to its own margin, so it sat 3 pt off the axis of
    /// the discs below it; and the compass was 46 pt against everything else's
    /// 40, so the stack visibly bulged in the middle.
    ///
    /// It is measured through [`column_x`] and the theme's own inset rather than
    /// against a literal, so a future change to either moves the whole column or
    /// fails here; a hard-coded 12 would have gone on agreeing with a stale
    /// number. `dock::the_column_is_one_column` is the other half, asserting the
    /// layers disc's margin is that same inset.
    #[test]
    fn the_left_column_is_one_axis_and_one_diameter() {
        let axis = theme::OSD_MARGIN + theme::ROUND_BUTTON_SIZE * 0.5;
        for size in [COMPASS_SIZE, LOCATE_SIZE] {
            assert_eq!(
                size,
                theme::ROUND_BUTTON_SIZE,
                "the column is one diameter; {size} is not it"
            );
            let centre = column_x(size) + size * 0.5;
            assert!(
                (centre - axis).abs() < 1e-3,
                "a {size} pt disc centres at {centre}, not {axis}"
            );
        }
    }

    /// The column's gaps say what belongs together: the compass and the locate
    /// arrow both steer the *view* and sit a plain gap apart, while the layers
    /// disc above them opens a panel and is a group gap out. With one gap for
    /// both, the discs read as an undifferentiated strip of chrome.
    #[test]
    fn the_column_groups_the_view_controls() {
        let compass_top = crate::systems::dock::reserved_top();
        let locate_top = compass_top + COMPASS_SIZE + theme::OSD_GAP;
        let above_compass = compass_top - (theme::OSD_MARGIN + theme::ROUND_BUTTON_SIZE);
        assert!(
            locate_top - (compass_top + COMPASS_SIZE) < above_compass,
            "the gap under the layers disc must exceed the one inside the pair"
        );
        // …and nothing overlaps its neighbour, which hand-placed anchors do not
        // guarantee on their own.
        assert!(locate_top >= compass_top + COMPASS_SIZE);
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

    /// The locate button's caption must tell the four situations apart. They look
    /// identical on the button — one amber arrow — so the caption is the only place
    /// "tracking, and the compass is working" is distinguishable from "tracking,
    /// but nothing is turning the map and here is why".
    #[test]
    fn the_locate_caption_says_which_of_the_four_it_is() {
        use crate::platform::orientation::{DeviceHeading, OrientationWatch};
        use crate::systems::locate::LocateTracking;

        let mut t = LocateTracking::default();
        let off = locate_hover(LocateState::Off, &t);
        assert!(off.contains("compass") || off.contains("heading"), "{off}");

        // Tracking with a heading: the bearing is in the text, and the accuracy
        // beside it when the platform stated one.
        t.heading = Some(DeviceHeading {
            bearing_deg: 123.4,
            accuracy_deg: Some(15.0),
        });
        let with = locate_hover(LocateState::Tracking, &t);
        assert!(with.contains("123°"), "{with}");
        assert!(with.contains("±15°"), "{with}");
        assert!(with.contains("stop"), "{with}");

        // Tracking with no heading yet. What that *means* depends on whether this
        // build has a compass at all, which is exactly the distinction the caption
        // exists to carry — so the assertion follows the same cfg the code does.
        t.heading = None;
        let without = locate_hover(LocateState::Tracking, &t);
        if OrientationWatch::available() {
            assert!(without.contains("waiting"), "{without}");
        } else {
            assert!(without.contains("no compass"), "{without}");
        }

        // An error is appended, never substituted: the mode is still on.
        t.error = Some("Location permission denied".to_string());
        let failed = locate_hover(LocateState::Tracking, &t);
        assert!(failed.contains("Location permission denied"), "{failed}");
        assert!(failed.contains("stop"), "{failed}");
    }
}
