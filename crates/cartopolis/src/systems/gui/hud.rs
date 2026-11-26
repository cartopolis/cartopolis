//! The map's view controls: the compass rose and the light disc in the left
//! column, the locate button in the right one (at the slot `dock` keeps for
//! it, since 2026-11-10), and the scale bar along the bottom edge.
//!
//! **The status readout that used to share this module is gone** (2026-09-10).
//! It was an anchored box in the top-right corner carrying the wall-clock time,
//! the camera's latitude, longitude and altitude, the avatar's street address,
//! the signed-in username, and chips for the sun mode and live weather. Every
//! one of those had by then acquired a second home that a reader finds without
//! being told about a toggle: the position, the altitude and the address are in
//! the Developer tools' View tab, which is where somebody asking "where is the
//! camera" is already looking, and the sun and weather switchers are in
//! Settings ▸ World, which is the only one of the two a phone could reach
//! comfortably. What was left was a second copy of a readout, drawn over the
//! map, behind a switch in a menu — which is chrome charging rent.
//!
//! What did *not* move is the compass and the locate button, and the reason is
//! the same one that put them in the opposite corner in the first place: they
//! are controls **for the view**, they act on the thing under them, and there is
//! no panel that could hold them without making a camera turn a trip into a
//! menu.

use bevy::prelude::*;
use big_space::prelude::GridCell;

use crate::components::FreeCamera;
use crate::resources::CameraState;
use crate::systems::driver::GpsQuality;
use crate::systems::theme;

/// What the view controls show this frame, for the library's edition
/// (`lib_chrome`, through `dock::ChromeLib`): [`drive_map_controls`]
/// decides all of it. Points from the screen's edges (the
/// library's px on the Bevy backend); `None` is a control not shown.
#[derive(Debug, Clone, Default, PartialEq)]
#[cfg_attr(not(feature = "ui-lib"), allow(dead_code))]
pub struct HudView {
    pub compass: Option<CompassView>,
    pub locate: Option<LocateView>,
    pub light: Option<LightView>,
    pub scale: Option<ScaleView>,
}

/// The compass rose: from the screen's left and top.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(not(feature = "ui-lib"), allow(dead_code))]
pub struct CompassView {
    pub left: f32,
    pub top: f32,
    /// The view's bearing, degrees clockwise from north, to half a degree.
    pub heading: f32,
    pub interactive: bool,
    pub locked: bool,
    pub caption: Option<&'static str>,
    pub hover: &'static str,
}

/// How the locate disc's ring reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(feature = "ui-lib"), allow(dead_code))]
pub enum LocateTone {
    /// Not tracking.
    Ring,
    /// Tracking, the receiver behaving.
    Bright,
    /// Tracking, the fix poor, stale or refused.
    Warn,
}

/// The locate disc: how far its right edge stands in from the screen's.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(not(feature = "ui-lib"), allow(dead_code))]
pub struct LocateView {
    pub right: f32,
    pub top: f32,
    /// In a lying row: the caption goes under the disc, not beside it.
    pub lying: bool,
    pub on: bool,
    pub tone: LocateTone,
    /// A fix is in hand: the arrow is filled.
    pub filled: bool,
    /// Waiting for a fix: an arc travels round the ring.
    pub searching: bool,
    pub caption: Option<String>,
    pub hover: String,
}

/// The light disc and, open, its three choices.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(not(feature = "ui-lib"), allow(dead_code))]
pub struct LightView {
    pub left: f32,
    pub top: f32,
    pub lying: bool,
    pub mode: crate::resources::SunMode,
    pub open: bool,
}

/// The scale bar: from the screen's left and bottom, its length and words.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(not(feature = "ui-lib"), allow(dead_code))]
pub struct ScaleView {
    pub left: f32,
    pub bottom: f32,
    pub pts: f32,
    pub label: String,
}

/// The light disc's choices, for the library's edition: each light and its
/// word, in the order the disc offers them.
#[cfg_attr(not(feature = "ui-lib"), allow(dead_code))]
pub const LIGHTS: [(crate::resources::SunMode, &str); 3] = LIGHT_CHOICES;

/// What the light disc's hover says, for the library's edition.
#[cfg_attr(not(feature = "ui-lib"), allow(dead_code))]
#[must_use]
pub fn light_hint(mode: crate::resources::SunMode) -> &'static str {
    light_hover(mode)
}

/// Diameter (points) of the compass overlay — [`theme::ROUND_BUTTON_SIZE`], the
/// one disc this app draws over a map.
///
/// It was 46 against the dock's 40 and the on-screen controls' 44, which put
/// three diameters within one screen and left the top-left column visibly
/// ragged: the compass bulged 3 pt past the layers disc above it and the locate
/// button below, and the two of them were each centred on a different axis.
/// `theme::ROUND_BUTTON_SIZE`'s own doc claimed it "matches the compass disc"
/// while they were six points apart; now it does. (The library draws it at
/// that size, `lib_chrome::DISC`; the tests hold the column to it.)
#[cfg_attr(not(test), allow(dead_code))]
const COMPASS_SIZE: f32 = theme::ROUND_BUTTON_SIZE;

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
    /// Distance from the left of the screen: the margin, or a lying row's slot.
    left: f32,
    /// Caption under the rose (driver mode's orientation name). `None` leaves the
    /// bare compass.
    caption: Option<&'static str>,
    hover: &'static str,
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
/// `x` is the slot's own offset from its edge (`dock::left_slot`,
/// `dock::right_column`).
#[must_use]
fn column_x(x: f32, size: f32) -> f32 {
    x + (theme::ROUND_BUTTON_SIZE - size) * 0.5
}

/// Locate-button state. Two, not three: see the module note on
/// [`crate::systems::locate`] for why the plain follow-without-rotate rung came
/// out of the cycle. Derived from [`crate::systems::locate::LocateTracking`] —
/// not from the camera, which can drop follow-and-rotate on its own and would
/// leave the button unlit over a running GNSS radio.
///
/// **The signal is a separate axis and deliberately not a third variant here.**
/// What the button *does* is a two-state toggle; how well the receiver is doing
/// is a report, carried by [`crate::systems::driver::GpsQuality`] and passed to
/// [`LocateView`] beside this. Folding them into one enum would put "the user
/// pressed it" and "a satellite answered" on the same ladder, and the click
/// handler would then have to pick which rungs mean *on*.
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

/// The three lights the disc offers, in the order a theme switcher lists them.
/// A set time is Settings' alone: it needs a clock face to set.
const LIGHT_CHOICES: [(crate::resources::SunMode, &str); 3] = [
    (crate::resources::SunMode::Auto, "Auto"),
    (crate::resources::SunMode::Day, "Light"),
    (crate::resources::SunMode::Night, "Dark"),
];

/// What the light disc's caption says it does.
fn light_hover(mode: crate::resources::SunMode) -> &'static str {
    use crate::resources::SunMode;
    match mode {
        SunMode::Auto => "Light: Auto, the real sun and moon",
        SunMode::Day => "Light: Light, day wherever you look",
        SunMode::Night => "Light: Dark, night all round",
        SunMode::Manual => "Light: a time you set (Settings ▸ World)",
    }
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
            // The position first: it is what the button is for, and until this
            // line the caption reported only the *compass*, so a phone with a
            // perfect fix and no magnetometer said "Following — this device has
            // no compass" and never once mentioned the GPS.
            let mut s = match tracking.gps {
                // Which receiver, when it is the external one: "RTK fixed" is
                // the thing a surveyor holding one wants confirmed.
                GpsQuality::Good(m) => match crate::platform::gnss_receiver::fix_label() {
                    Some(fix) => format!(
                        "Tracking this device — {fix}, position to {}",
                        crate::systems::driver::format_accuracy(m)
                    ),
                    None => format!(
                        "Tracking this device — position to {}",
                        crate::systems::driver::format_accuracy(m)
                    ),
                },
                GpsQuality::Poor(m) => format!(
                    "Tracking this device — position only to {}",
                    crate::systems::driver::format_accuracy(m)
                ),
                GpsQuality::Waiting => "Tracking this device — waiting for a fix".to_string(),
                GpsQuality::Stale => {
                    "Tracking this device — no fix recently, showing the last one".to_string()
                }
                // `NoProvider` is every desktop, where the button follows the
                // avatar and there is no receiver to describe; `Error` has the
                // provider's own sentence appended below and must not be
                // pre-empted by a vaguer one.
                GpsQuality::NoProvider | GpsQuality::Error => "Tracking this device".to_string(),
            };
            s.push_str(" — ");
            s.push_str(&match tracking.heading {
                Some(h) => {
                    let mut line = format!("heading {}°", h.bearing_deg().round());
                    // Only iOS reports one, and only once the magnetometer has
                    // settled. A wide figure here is the explanation for a map
                    // that is pointing *nearly* the right way.
                    if let Some(a) = h.accuracy_deg {
                        line.push_str(&format!(" ±{}°", a.round()));
                    }
                    line
                }
                None if OrientationWatch::available() => "waiting for the compass".to_string(),
                None => "this device has no compass".to_string(),
            });
            if let Some(error) = &tracking.error {
                // Appended rather than replacing: the mode really is on, and a
                // caption that said only "denied" over a lit button would leave
                // nothing to explain what the button is still doing. The warning
                // sign in front of it is the icon font's: a missing glyph is a
                // tofu box, a worse sign of trouble than none.
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

/// The left-hand map controls: the compass rose, and the GPS/locate button under
/// it.
///
/// Its own system rather than part of the status readout that used to live in
/// this module, because it answers to a different owner in each mode — the Orbit
/// camera's north lock normally, driver mode's orientation cycle while
/// navigating. The readout is gone now (see the module note) and this is what
/// the module is.
#[allow(clippy::too_many_arguments)]
pub fn drive_map_controls(
    metrics: Res<crate::systems::ui_input::UiMetrics>,
    mut camera_state: ResMut<CameraState>,
    mut driver: ResMut<crate::systems::driver::DriverState>,
    globe: Res<crate::systems::globe::GlobeState>,
    right_panel: Res<crate::resources::RightPanel>,
    mut tracking: ResMut<crate::systems::locate::LocateTracking>,
    camera: Query<(&GridCell, &Transform, &Projection), With<FreeCamera>>,
    avatars: Query<(), With<crate::components::Avatar>>,
    keyboard: Res<crate::systems::onscreen_keyboard::OnScreenKeyboard>,
    touch: Res<crate::systems::touch::TouchGestures>,
    #[cfg(feature = "comms")] camera_screen: Res<crate::systems::camera_screen::CameraScreen>,
    mut high: ResMut<crate::systems::high_precision::HighPrecision>,
    mut dev: ResMut<crate::resources::DevPanelState>,
    room: Res<crate::systems::design_room::DesignRoom>,
    // Whether the light disc's three choices are open beside it.
    mut light_open: Local<bool>,
    // What the library draws (`lib_chrome`) goes in `lib.hud`; its taps come
    // back in `lib.acts`.
    mut chrome: ResMut<crate::systems::dock::MapChrome>,
) {
    let acts = chrome
        .lib
        .take_acts(crate::systems::dock::ChromeAct::is_hud);
    let tapped = |a: crate::systems::dock::ChromeAct| acts.contains(&a);
    let mut out = HudView::default();
    'draw: {
        // The design room is a white room, with its own bar and nothing else.
        if room.spot.is_some() {
            break 'draw;
        }
        // The camera is the whole screen. A chat's World view keeps these: its
        // header is one slim pill at the top centre (`chats::render_chat_world`).
        #[cfg(feature = "comms")]
        let camera_open = camera_screen.is_open();
        #[cfg(not(feature = "comms"))]
        let camera_open = false;
        if camera_open && !driver.active {
            break 'draw;
        }
        // A right-docked panel (Layers, Bookmarks, AI) sits opaque over the map, and
        // the chrome is drawn over panels, so the only way to keep these *behind*
        // one is to drop them while it is open. Driver
        // mode is the exception: the compass is part of its chrome, and the planner it
        // was started from is closed anyway.
        //
        // `RightPanel` is the whole answer. This used to also OR in `DataLayers::
        // panel_open` and `BookmarksState::panel_open` — the booleans the panels
        // carried before the shared dock existed. None of them is
        // cleared anywhere, and the old status readout *set* the bookmarks one when
        // you saved a place, so a single press of ☆ hid the compass and the locate
        // button for the rest of the session. Nothing writes them now
        // except the `--shot` harness, which sets `RightPanel` alongside them from the
        // same `--ui` list, so the scripted chrome is unchanged.
        if !right_panel.is(crate::resources::RightPanel::None) && !driver.active {
            break 'draw;
        }
        let Ok((cell, tf, projection)) = camera.single() else {
            break 'draw;
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
            break 'draw;
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
            let viewport_h = metrics.viewport_pts().y;
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
                // Bottom-left, where a web map has always put a scale: the
                // bottom-right corner is the on-screen controls'.
                out.scale = Some(ScaleView {
                    left: theme::OSD_MARGIN,
                    bottom: reserved + theme::OSD_GAP,
                    pts,
                    label: format_scale(metres),
                });
            }
        }

        // The compass: what it shows in `out`; answers whether it was pressed.
        let compass = |cfg: &Compass, out: &mut HudView| -> bool {
            out.compass = Some(CompassView {
                left: cfg.left,
                top: cfg.top,
                heading: (heading * 2.0).round() / 2.0,
                interactive: cfg.interactive,
                locked: cfg.locked,
                caption: cfg.caption,
                hover: cfg.hover,
            });
            cfg.interactive && tapped(crate::systems::dock::ChromeAct::Compass)
        };

        if driver.active {
            // Driving: the rose steps through the navigation orientations
            // (`driver::MapOrientation`), and stacks under the manoeuvre card that has
            // taken the corner.
            use crate::systems::driver::{CARD_HEIGHT, CARD_MARGIN};
            let cfg = Compass {
                interactive: true,
                locked: driver.orientation == crate::systems::driver::MapOrientation::NorthUp,
                top: CARD_MARGIN + CARD_HEIGHT + 8.0,
                left: theme::OSD_MARGIN,
                caption: Some(driver.orientation.label()),
                hover: "Change the map orientation (heading up / north up / 3D)",
            };
            if compass(&cfg, &mut out) {
                driver.orientation = driver.orientation.next();
            }
            break 'draw;
        }

        // Clicking toggles the north-up lock in globe (Orbit) mode only; fly/avatar are
        // free-form, so there the rose is a passive indicator.
        let orbit = matches!(camera_state.mode, crate::resources::CameraMode::Orbit);
        let locked = orbit && camera_state.orbit_north_lock;
        let view = metrics.viewport_pts();
        let slot = crate::systems::dock::left_slot(view, 0);
        let cfg = Compass {
            interactive: orbit,
            locked,
            // Under the search bar, at the head of the left column; lying, at the
            // head of the left row beside it (`dock::left_slot`).
            top: slot.y,
            left: slot.x,
            caption: None,
            hover: if locked {
                "North-up locked — click to release"
            } else {
                "Lock the view north-up"
            },
        };
        if compass(&cfg, &mut out) {
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
        let col = crate::systems::dock::right_column(view, true, false);
        if let Some(at) = col.locate.filter(|_| !avatars.is_empty()) {
            let state = if tracking.active {
                LocateState::Tracking
            } else {
                LocateState::Off
            };
            let hover = locate_hover(state, &tracking);
            let on = matches!(state, LocateState::Tracking);
            let gps = tracking.gps;
            out.locate = Some(LocateView {
                right: column_x(at.x, LOCATE_SIZE),
                top: at.y,
                lying: col.lying,
                on,
                // The ring: amber while the receiver is behaving, the warning
                // ochre while it is not (a poor fix is a caveat, not a fault).
                tone: match (on, gps) {
                    (false, _) => LocateTone::Ring,
                    (true, GpsQuality::Error | GpsQuality::Stale | GpsQuality::Poor(_)) => {
                        LocateTone::Warn
                    }
                    (true, _) => LocateTone::Bright,
                },
                // Filled means a fix is in hand, never the press.
                filled: gps.has_position(),
                searching: matches!(gps, GpsQuality::Waiting),
                // The caption is the half that reaches a phone: no hover there.
                caption: gps.caption().filter(|_| on),
                hover,
            });
            if tapped(crate::systems::dock::ChromeAct::Locate) {
                match state {
                    // Recenter, in the view the user is already in. The button used to
                    // force Follow on the way in, which made "where am I" a one-way
                    // door out of Fly and Earth; `systems::locate` moves the camera
                    // that is there instead, and owns the rotate-follow rig too — it
                    // has to, because arriving in Follow later (Tab, the mode disc)
                    // must arm the same thing this press would have.
                    LocateState::Off => {
                        tracking.enter();
                        tracking.enter_walk_view(&mut camera_state);
                    }
                    LocateState::Tracking => {
                        camera_state.follow_rotate = false;
                        tracking.leave_walk_view(&mut camera_state);
                        tracking.exit();
                        // The camera mode rides on the button's fixes; switching
                        // the button off ends it too.
                        high.on = false;
                    }
                }
            }
        }

        // The light disc, under the lens in the left column: Auto, Light or
        // Dark, as a theme switcher offers them (`docs/mockups/zoom-ladder/`,
        // v2). The same setting as Settings ▸ World's Light row, which also holds
        // a set time.
        if tapped(crate::systems::dock::ChromeAct::LightDisc) {
            *light_open = !*light_open;
        }
        let chosen = acts.iter().find_map(|a| match a {
            crate::systems::dock::ChromeAct::Light(m) => Some(*m),
            _ => None,
        });
        if chosen.is_some() {
            *light_open = false;
        }
        let at = crate::systems::dock::left_slot(view, 2);
        out.light = Some(LightView {
            left: column_x(at.x, theme::ROUND_BUTTON_SIZE),
            top: at.y,
            lying: crate::systems::dock::lying(view),
            mode: chosen.unwrap_or(dev.sun_mode),
            open: *light_open,
        });
        if let Some(mode) = chosen {
            if dev.sun_mode != mode {
                dev.sun_mode = mode;
            }
        }
    }
    crate::systems::dock::set_if_changed(&mut chrome.lib.hud, out);
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

    /// **A column is one column: one axis, one diameter.** Both halves of that
    /// are load-bearing and both were broken once: a disc anchored flush to its
    /// own margin sat 3 pt off the axis of the discs below it, and the compass
    /// was 46 pt against everything else's 40, so the stack bulged.
    ///
    /// Measured through [`column_x`] and the theme's own inset rather than
    /// against a literal. The left column (compass, light) and the right one
    /// (locate, anchored from the right edge by the same offset) share it, as
    /// `dock`'s discs do: they are anchored at [`theme::OSD_MARGIN`] too.
    #[test]
    fn the_columns_are_one_axis_and_one_diameter() {
        let axis = theme::OSD_MARGIN + theme::ROUND_BUTTON_SIZE * 0.5;
        for size in [COMPASS_SIZE, LOCATE_SIZE] {
            assert_eq!(
                size,
                theme::ROUND_BUTTON_SIZE,
                "the column is one diameter; {size} is not it"
            );
            let centre = column_x(theme::OSD_MARGIN, size) + size * 0.5;
            assert!(
                (centre - axis).abs() < 1e-3,
                "a {size} pt disc centres at {centre}, not {axis}"
            );
        }
    }

    /// The left column under the search bar — compass, lens, light — and the
    /// locate disc in the right one take the slots `dock` hands out, so they
    /// start under the bar and never sit on a neighbour.
    #[test]
    fn the_view_controls_take_their_slots() {
        let dock = crate::systems::dock::reserved_top();
        let view = Vec2::new(274.0, 600.0);
        let compass = crate::systems::dock::left_slot(view, 0).y;
        let light = crate::systems::dock::left_slot(view, 2).y;
        assert!(compass >= dock);
        assert!(light >= compass + 2.0 * (COMPASS_SIZE + theme::OSD_GAP));
        let col = crate::systems::dock::right_column(view, true, true);
        let locate = col.locate.expect("an avatar has a locate slot").y;
        assert!(locate >= col.mode.y + theme::ROUND_BUTTON_SIZE);
        assert!(col.camera.unwrap().y >= locate + LOCATE_SIZE + theme::OSD_GAP);
        // Lying, the same slots along a row, Locate's beside the mode's with
        // room for both their words under them.
        let view = Vec2::new(924.0, 424.0);
        let col = crate::systems::dock::right_column(view, true, true);
        let locate = col.locate.unwrap();
        assert!(col.lying && locate.y == col.mode.y);
        assert!(locate.x >= col.mode.x + LOCATE_SIZE + theme::OSD_GAP);
        let compass = crate::systems::dock::left_slot(view, 0);
        let light = crate::systems::dock::left_slot(view, 2);
        assert!(light.x >= compass.x + 2.0 * (COMPASS_SIZE + theme::OSD_GAP));
    }

    /// The locate button's caption must tell the four situations apart. They look
    /// identical on the button — one amber arrow — so the caption is the only place
    /// "tracking, and the compass is working" is distinguishable from "tracking,
    /// but nothing is turning the map and here is why".
    #[test]
    fn the_locate_caption_says_which_of_the_four_it_is() {
        use crate::platform::orientation::{DeviceAttitude, OrientationWatch};
        use crate::systems::locate::LocateTracking;

        let mut t = LocateTracking::default();
        let off = locate_hover(LocateState::Off, &t);
        assert!(off.contains("compass") || off.contains("heading"), "{off}");

        // Tracking with a heading: the bearing is in the text, and the accuracy
        // beside it when the platform stated one. Tipped well off vertical, so
        // screen-up has a horizontal part and the bearing is a fair question —
        // see `DeviceAttitude::bearing_deg`.
        t.heading = Some(DeviceAttitude {
            accuracy_deg: Some(15.0),
            ..DeviceAttitude::pointing(123.4, -60.0, 0.0)
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

    /// The hover text must name the **position** as well as the compass.
    ///
    /// It used to describe only the heading, so a phone holding a four-metre fix
    /// and lacking a magnetometer read "Following — this device has no compass":
    /// a caption that mentions the one sensor that is not working and never the
    /// one that is. That is the same silence the button's own caption exists to
    /// break, one layer up.
    #[test]
    fn the_locate_caption_reports_the_position_not_only_the_compass() {
        use crate::systems::locate::LocateTracking;

        let mut t = LocateTracking::default();
        t.gps = GpsQuality::Good(4.0);
        let good = locate_hover(LocateState::Tracking, &t);
        assert!(good.contains("±4 m"), "{good}");

        t.gps = GpsQuality::Waiting;
        let waiting = locate_hover(LocateState::Tracking, &t);
        assert!(waiting.contains("waiting for a fix"), "{waiting}");

        t.gps = GpsQuality::Poor(90.0);
        let poor = locate_hover(LocateState::Tracking, &t);
        assert!(poor.contains("only to ±90 m"), "{poor}");

        t.gps = GpsQuality::Stale;
        let stale = locate_hover(LocateState::Tracking, &t);
        assert!(stale.contains("last one"), "{stale}");
    }

    /// The disc fills on a *fix*, never on the press.
    ///
    /// This is the whole complaint the three rungs answer: the button lit the
    /// instant it was tapped — ahead of the permission dialog and ahead of a cold
    /// GNSS start that can run to minutes — so the one moment somebody is asking
    /// "is this working?" was the one moment the button could not tell them.
    #[test]
    fn the_button_fills_only_once_a_fix_is_in_hand() {
        for searching in [GpsQuality::Waiting, GpsQuality::Error, GpsQuality::Stale] {
            assert!(
                !searching.has_position(),
                "{searching:?} would draw as a solid arrow"
            );
        }
        // Poor fills too, and says so in the ring rather than the icon: the fix is
        // genuinely in hand, it is simply loose — and an icon that changed shape
        // with the signal would read as a different button in every tunnel.
        assert!(GpsQuality::Poor(90.0).has_position());
        assert!(GpsQuality::Good(4.0).has_position());
        // And a desktop fills, which is the case a fill derived from the accuracy
        // would have got backwards: no receiver is not the same as still looking.
        assert!(GpsQuality::NoProvider.has_position());
        assert!(GpsQuality::NoProvider.accuracy_m().is_none());
    }
}
