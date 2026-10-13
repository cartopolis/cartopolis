//! Driver mode: the map turned into a navigation display.
//!
//! [`crate::systems::navigation`] plans a route; this drives one. The split is
//! deliberate and is the fix for the first cut, where *planning* stripped the map
//! bare: opening the planner turned off the buildings, the trees and the weather,
//! so the ordinary act of looking up a route wrecked the view you were looking at.
//! Planning now changes nothing. Driver mode — armed only once a route exists —
//! is what strips the map, locks the camera into a chase view, and swaps the
//! avatar for a heading arrow.
//!
//! The layout is a satnav's, taken from the two reference navigation views this
//! was specified against: the next manoeuvre on a card at the top left with the
//! compass under it, the trip — arrival clock, time left, distance left — as a
//! narrow strip down the right edge, and the current speed in a pill at the bottom
//! centre. The first cut put all three trip numbers in one wide bar across the
//! bottom, which read as a dashboard rather than as a navigation view.
//!
//! **The right strip is a route rail, not a column of numbers.** Under the trip
//! figures it runs the drive as one vertical track — the vehicle at the foot, the
//! destination capped at the top, and the services, saved places and other things
//! you will pass marked along it in proportion ([`route_rail`]). That is the
//! element both references build their right-hand side around, and it answers the
//! question the numbers cannot: not *how far* is left, but *what* is left. What
//! goes on it is [`crate::systems::route_ahead`]'s problem; placing it is this
//! module's, and the scale it is placed on is the interesting part
//! ([`RAIL_NEAR_SPAN_M`]).
//!
//! **Leaving the route asks for a new one.** Missing a turn is the normal case,
//! not a fault, so the display re-solves from where the vehicle actually is
//! instead of painting "Off route" and stopping there — which was giving up at
//! the one moment navigation is needed. Only the *start* moves; see
//! [`DriverState::reroute_ready`] for why the cooldown is load-bearing.
//!
//! **A reroute keeps the display working, and a failed one does not end the
//! trip.** Both used to be false, and both were reached by the everyday cases — an
//! underground car park, a missed motorway exit, a tile server having a moment.
//! Dropping the route to re-solve blanked the line, the rail, the distance and the
//! ETA for the whole corridor refetch, and if the solve then failed the tick saw no
//! route at all and left driver mode *silently*, the explanation being in a panel
//! the mode had closed. Now the route being replaced is parked in
//! `navigation::RouteState::held` and everything keeps measuring against it until a
//! replacement lands, and a failure paints itself on the manoeuvre card with Retry
//! beside it. The corridor cache also matches a *contained* corridor
//! (`navigation::covers`), which is what makes the ordinary missed turn re-solve in
//! one A* instead of a whole refetch.
//!
//! **The marker is drawn on the road, not at the fix** ([`SNAP_M`]). A 15 m urban
//! fix has the arrow wandering through the terrace beside the street while the card
//! says you are on route; every navigation display snaps. Only the drawing does —
//! progress, the off-route judgement and the reroute all keep the true position,
//! because a display that snapped those could no longer notice it was wrong.
//!
//! **The chrome says what the fix is worth** ([`GpsQuality`]). "The watch started
//! and nothing has arrived", "±50 m in a tunnel" and a metre-accurate lock all
//! rendered identically — as confident navigation — because `accuracy_m` was parsed
//! by both providers and read by nobody. A poor or stale fix also stands the reroute
//! down: leaving a tunnel otherwise triggers a full corridor refetch for a vehicle
//! that never left the road.
//!
//! **The guidance is audible** ([`next_cue`]). Three tones on the approach to a
//! manoeuvre, one on a reroute, one on arrival — on the bands [`countdown`] already
//! coarsens at, because those are the points at which something changes for the
//! driver. Not speech: there is no TTS in this tree and a tone is unambiguous at
//! 120 km/h with the window open.
//!
//! **Stop and Route live on the manoeuvre card**, which is the only box that is
//! always drawn. Stop was on the trip strip, and the strip stands down behind any
//! docked panel — on a phone, which has no Escape key, that could leave a drive
//! with no exit at all. Route reopens the planner without leaving the mode, which
//! is otherwise unreachable mid-drive: the dock, the main menu and the planner
//! are all suppressed, so there was no way to change the destination, read the turn
//! list or switch travel mode short of ending the trip.
//!
//! The speed pill carries the posted limit beside the speedometer, as a painted
//! road sign rather than a themed readout ([`speed_limit_sign`]) — same reasoning
//! as [`DRIVE_BLUE`]. It is deliberately absent where OSM has no `maxspeed`: the
//! road class the planner routes on is a cost estimate, not a legal limit, and a
//! sign confidently showing the wrong number is worse than no sign.
//!
//! **Where the position comes from.** Outside driver mode there is only one
//! answer: the avatar, which the user walks around. Inside it, the device's own
//! fix wins when there is one ([`crate::platform::location`]) — that is what
//! "navigating" means — and the avatar is moved to it, so the camera follow, the
//! arrow, and the progress readout all keep reading the avatar and need to know
//! nothing about GPS. The browser and Android have providers; on desktop there is
//! none, so the avatar simply stays under the user's control and driver mode
//! narrates *its* movement, which is also how it is testable without a dongle.
//!
//! **How the fix is applied depends on how far away it is** ([`GPS_TELEPORT_M`]).
//! A position write moves the avatar within the anchored frame; it does not move
//! the frame. So the first fix — the one that can land in a different town from
//! whatever the app was looking at — is applied as a teleport, which re-anchors
//! the map and brings the camera and every streamer with it.
//!
//! **A fix is applied over the frames that follow it, not on the one it lands
//! on** ([`FixGlide`]). The receiver reports about once a second and the screen
//! draws sixty times in that second, so placing the arrow the moment a report
//! arrives spends a whole second's movement on one frame in sixty — the display
//! is not short of information, it is delivering all of it at once. Every reading
//! taken downstream of the position is smoothed by this for free: the measured
//! course, the progress along the route and the chase camera all come off the
//! avatar's own transform.
//!
//! **Heading is a course over ground, not a facing.** The avatar's yaw is bound
//! to the camera azimuth by `avatar::handle_avatar_movement` (W walks into the
//! view), so reading it back would make the arrow point up the screen forever —
//! it would be north-up mode's own reflection. The arrow and the compass use the
//! smoothed direction of travel instead, falling back to the facing only when
//! standing still, exactly as ADR-026's live vehicles derive a bearing from
//! motion when the feed carries none.
//!
//! **The drive ends.** Reaching the destination is a state
//! ([`DriverState::arrived`]), not the distance quietly running out: without it
//! the card counted "Arrive · 0 m" for as long as you left it, kept re-solving
//! whenever the last few metres put you [`OFF_ROUTE_M`] from the line, and the
//! only way out was a button on a strip that a docked panel could hide. Arriving
//! now paints its own card, stops the reroute machinery, and offers the way out.
//!
//! **Distance to the turn is a countdown, not a measurement** ([`countdown`]).
//! Metres are what the projection produces; what a driver acts on is a ladder
//! that coarsens with distance and ends at *now*, so the number changes when
//! something has changed rather than every ten metres of a kilometre-long
//! straight. The card's bottom edge fills over the last [`APPROACH_M`] as well —
//! the same fact in the shape you can read without reading it.
//!
//! **The chase rig is not a fixed pose.** It stretches with speed, because at
//! 120 km/h the useful part of the picture is further away than it is at 30, and
//! it tightens onto a junction as the manoeuvre closes, because that is the one
//! moment the detail matters more than the distance ([`RIG_SPEED_STRETCH`],
//! [`RIG_JUNCTION_M`]). Both fold into the same eased target the orientation
//! writes, so they are camera moves and never cuts.
//!
//! **Locked is not frozen** ([`Peek`]). A driver looks ahead — at the junction
//! after this one, at where the car park actually is — and a rig that cannot be
//! moved at all makes that impossible. The wheel, the drag and the pinch are
//! handled *here* rather than by unlocking `free_camera`, because that system's
//! Avatar arm clamps the third-person zoom to 25 m and would drag a 240 m chase
//! view in every frame. They accumulate as offsets on the rig, a Re-centre
//! button appears, and after [`PEEK_TIMEOUT`] of no input they ease back to the
//! driving pose on their own.

use crate::components::DriverArrow;
use crate::grid::{WORLD_CELL_EDGE, WORLD_SWITCH_THRESHOLD};
use crate::systems::egui_manager::EguiRef;
use bevy::asset::RenderAssetUsages;
use bevy::math::DVec3;
use bevy::prelude::*;
use bevy::render::mesh::{Indices, PrimitiveTopology};
use big_space::prelude::{BigSpace, Grid, GridCell};
use web_time::Instant;

use crate::components::{Avatar, FreeCamera};
use crate::platform::location::{GpsFix, LocationWatch};
use crate::resources::{AvatarState, CameraMode, CameraState, MapAnchor, OnlineSession};
use crate::systems::egui_manager::EguiManager;
use crate::systems::geo_nav::{
    lat_lng_to_world, world_to_lat_lng, CameraPose, GeoNavState, Teleport,
};
use crate::systems::ground_stack::GroundLayer;
use crate::systems::icons;
use crate::systems::navigation::RouteState;
use crate::systems::route_ahead::{AheadKind, AheadStatus, RouteAhead};
use crate::systems::routing::{format_distance, format_duration, Maneuver, Route};
use crate::systems::theme;

// ─── Tuning ──────────────────────────────────────────────────────────────────

/// The locked chase rig, per map orientation: distance from the arrow in metres,
/// elevation above it in radians. The flat views sit high and steep so the road
/// ahead reads as a map; the perspective one drops behind and low, which is the
/// view you actually drive by.
///
/// Plus how far ahead of the arrow the camera *aims*, as a fraction of that
/// distance. A chase camera that looks straight at the vehicle centres it, and
/// then half the screen is road already driven; every navigation device puts the
/// marker in the lower third and spends the rest on what is coming. That is done
/// by moving the look target forward, not the camera (`free_camera`'s Follow arm
/// reads `CameraState::drive_lookahead`) — and it is fractional per orientation,
/// because the same offset that tips the low chase view down the road would tip
/// the flat map view off it and onto the horizon.
///
/// The fractions are **solved**, not guessed: [`marker_screen_y`] projects the
/// marker through the rig the way `free_camera` places it, and the test pins each
/// orientation into the lower third. The flat one was 0.30 and put the marker at
/// 83% down the screen — the bottom sixth, all but touching the speed pill, with
/// nothing visible beside or behind. A steep view magnifies a ground offset far
/// more than a low one does, which is exactly why the two cannot share a number
/// and why eyeballing one from the other went wrong.
const FLAT_DISTANCE: f32 = 240.0;
const FLAT_PITCH: f32 = 1.35; // ~77°
const FLAT_LOOKAHEAD: f32 = 0.18;
const PERSPECTIVE_DISTANCE: f32 = 170.0;
const PERSPECTIVE_PITCH: f32 = 0.38; // ~22°
const PERSPECTIVE_LOOKAHEAD: f32 = 0.55;

/// Exponential rate (per second) the chase rig eases its distance/tilt toward the
/// orientation's target, so cycling the compass reads as a camera move rather than
/// a cut.
const RIG_EASE_RATE: f32 = 5.0;

/// How much further back the rig sits at [`RIG_SPEED_REF_KMH`] than at a
/// standstill, as a fraction of the orientation's own distance.
///
/// A pose tuned for a 30 km/h street shows about four seconds of road at 120,
/// which is not enough to act on; one tuned for the motorway is a map of the
/// neighbourhood while you are looking for a house number. The rig rides the
/// speedometer between them instead of being tuned for one of the two.
const RIG_SPEED_STRETCH: f32 = 0.55;
const RIG_SPEED_REF_KMH: f32 = 120.0;

/// Approaching a manoeuvre, the rig closes to [`RIG_JUNCTION_TIGHTEN`] of its
/// speed-adjusted distance over the last [`RIG_JUNCTION_M`].
///
/// The junction is the one place the picture has to answer *which* of these
/// turnings, and it is also the one place the road ahead stops mattering. Not
/// applied to a `Continue` or a `Depart`: those are not moments, and zooming in
/// on one would make the view breathe for nothing.
const RIG_JUNCTION_M: f32 = 150.0;
const RIG_JUNCTION_TIGHTEN: f32 = 0.62;

/// How long a peeked camera holds before it eases back to the driving pose.
///
/// Long enough to look at a junction and think about it, short enough that a
/// nudge does not strand the view — the driver never has to remember to undo it,
/// which is what makes reaching for the rig safe in the first place.
const PEEK_TIMEOUT: f32 = 8.0;
/// Bounds on the peeked zoom, as a multiple of the rig's own distance.
const PEEK_ZOOM_MIN: f32 = 0.35;
const PEEK_ZOOM_MAX: f32 = 4.0;
/// Screen pixels to radians for a peek drag — `free_camera`'s orbit rate, so the
/// map turns under the finger at the same speed it does everywhere else.
const PEEK_DRAG_RATE: f32 = 0.005;

/// Below this ground speed (m/s) the course over ground is meaningless — a
/// stationary receiver has no direction — so the heading holds its last value.
const COURSE_MIN_SPEED: f32 = 0.4;

/// Exponential smoothing rate (per second) for the displayed speed. Raw
/// frame-to-frame deltas jitter by several km/h; a driver's speedo does not.
const SPEED_SMOOTH_RATE: f32 = 2.5;

/// Half-length and half-width of the position arrow, in metres. Sized against the
/// chase distance, not against a person: a 2 m marker at 200 m is a couple of
/// pixels, and this is the one thing on screen that must never be ambiguous. It
/// still has to read as a vehicle *on* a street, though — the first cut was three
/// times this and filled a sixth of the screen.
const ARROW_HALF_LENGTH: f32 = 7.0;
const ARROW_HALF_WIDTH: f32 = 4.0;
/// Height above the ground plane. Above the route line (0.5 m, see
/// `navigation::ROUTE_Y`) so the arrow reads as riding *on* it.
const ARROW_Y: f32 = 0.7;

/// How far past a manoeuvre's own position it stays "the next manoeuvre". Without
/// it the card flips to the following turn a metre before you reach the junction,
/// which is the one moment it must not.
const STEP_PASSED_M: f64 = 8.0;

/// Off-route distance beyond which the readout says so instead of quietly showing
/// a projection from hundreds of metres away.
const OFF_ROUTE_M: f64 = 60.0;

/// Within this of the end of the route, the drive is over — see
/// [`DriverState::arrived`]. Generous on purpose: the destination pin is where
/// the user pointed, the route ends at the nearest routable road to it, and the
/// two are a building's depth apart often enough that a tight radius would leave
/// a driver parked outside the address being told to keep going.
const ARRIVE_M: f64 = 30.0;

/// Inside this the countdown stops naming a distance and says the manoeuvre is
/// now. "10 m" is a measurement of something already happening.
const NOW_M: f64 = 15.0;

/// Over this there is nothing to prepare for, so the card's approach bar is
/// empty; it fills to the manoeuvre.
const APPROACH_M: f64 = 400.0;

/// Minimum seconds between two reroute requests. See
/// [`DriverState::reroute_ready`] — this is the loop breaker, not a politeness.
const REROUTE_COOLDOWN: f32 = 10.0;

/// How far off the line the vehicle may be and still have its marker drawn *on*
/// the route rather than at the raw fix.
///
/// A phone in an urban canyon reports 10–20 m of error, and unsnapped that is an
/// arrow wandering through the buildings beside the street while the card says you
/// are on route — which reads as a positioning bug rather than as the noise it is.
/// Every navigation display snaps; the number is the honest one, a little under
/// [`OFF_ROUTE_M`] so a marker never snaps onto a road the display is about to
/// declare you off. **Only the drawing snaps.** Progress, the off-route judgement
/// and the reroute all keep using the true position — snapping those would make the
/// display incapable of noticing that it was wrong, which is the one thing it must
/// always be able to do.
const SNAP_M: f64 = 25.0;

/// How far a device fix may be from where the avatar already stands before it is
/// applied as a **teleport** rather than as a position write.
///
/// A fix is not a step. The first one in particular arrives from a receiver that
/// knows nothing about where the app happens to be looking: you plan a route in
/// the city you were browsing, tap Drive, and the phone reports the town you are
/// actually sitting in. `place_avatar` writes flat-frame metres measured from the
/// **map anchor**, and nothing in that path moves the anchor — so a fix a few
/// hundred kilometres away used to drop the avatar (and, through the chase rig,
/// the camera) that far from the origin of a frame the tiles, buildings, terrain
/// and route line are all still built around. Every streamer then rebuilds a
/// pyramid at once, in one frame, on a phone GPU: exactly the mass mesh/texture
/// churn this project already documents as the way to lose the device — after
/// which `RenderErrorPolicy::StopRendering` freezes the picture for good and the
/// app has to be restarted.
///
/// A [`Teleport`] is the operation that means "the world is somewhere else now":
/// it re-anchors the map, drops the old tiles through the bounded despawn queue
/// and brings the camera with it. So the rule is the ordinary one for this
/// codebase — a *move* is a write, a *jump* is a teleport — and the threshold
/// only has to be past anything a vehicle could cover between two fixes (2 km at
/// 1 Hz is 7,200 km/h) while staying far inside the flat frame's honest range.
const GPS_TELEPORT_M: f64 = 2_000.0;

/// Above this reported accuracy (metres, 1σ) the fix cannot support a conclusion
/// about which street you are on, so the chrome says so and the reroute stands down.
///
/// Matched to [`OFF_ROUTE_M`] deliberately: "off route" means further from the line
/// than this, and a fix whose own error bar is that wide cannot tell the two apart.
/// Leaving a tunnel is the case that made it necessary — the first fixes out are
/// tens of metres wide, they land on the wrong side of a junction, and a full
/// corridor refetch fires for a vehicle that never left the road.
const GPS_POOR_M: f64 = OFF_ROUTE_M;

/// After this long with no *new* fix, the position on screen is a memory.
///
/// Detected by the fix changing rather than by a timestamp, because [`GpsFix`]
/// carries none — see [`GpsQuality`]. Generous, so a parked vehicle whose receiver
/// happens to repeat a coordinate is not accused of losing signal.
const GPS_STALE_S: f32 = 12.0;

/// …and for how long it has to hold before the card says so. Undebounced, the
/// banner fires on any transient: the frames between a teleport being *requested*
/// and the map re-anchoring under it read as hundreds of metres off route, and
/// "Off route" is the first thing a driver sees on entering.
const OFF_ROUTE_GRACE: f32 = 2.0;

// ─── State ───────────────────────────────────────────────────────────────────

/// How the map is held while driving — the three orientations every turn-by-turn
/// app offers, and what the compass button steps through.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum MapOrientation {
    /// The map turns with you: the course points up the screen, flat and from
    /// high up. The one that makes "next left" mean the left of the screen.
    HeadingUp,
    /// The map is held north-up and the arrow turns instead — for reading the map
    /// as a map.
    NorthUp,
    /// Heading-up, dropped behind and tilted into the road: the "3D" view, and
    /// the default because it is the one both reference screenshots show and the
    /// one a driver reads a junction from.
    #[default]
    Perspective,
}

impl MapOrientation {
    /// The compass cycle, in the order the button steps through.
    #[must_use]
    pub fn next(self) -> Self {
        match self {
            Self::HeadingUp => Self::NorthUp,
            Self::NorthUp => Self::Perspective,
            Self::Perspective => Self::HeadingUp,
        }
    }

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::HeadingUp => "Heading up",
            Self::NorthUp => "North up",
            Self::Perspective => "3D",
        }
    }

    /// `(distance, pitch, lookahead fraction)` for the chase rig.
    fn rig(self) -> (f32, f32, f32) {
        match self {
            Self::HeadingUp | Self::NorthUp => (FLAT_DISTANCE, FLAT_PITCH, FLAT_LOOKAHEAD),
            Self::Perspective => (
                PERSPECTIVE_DISTANCE,
                PERSPECTIVE_PITCH,
                PERSPECTIVE_LOOKAHEAD,
            ),
        }
    }
}

/// How much the position on screen can be trusted this frame.
///
/// **Nothing used to say.** `GpsFix::accuracy_m` was parsed by both providers and
/// read by nobody, `DriverState::gps_fix` was set and never rendered, and the only
/// thing that could produce a message was an explicit provider *error* — so "the
/// watch started and no fix has arrived", "50 m of error in a tunnel" and "a
/// metre, locked on" all displayed as identical, confident navigation. That is the
/// failure mode a navigation display can least afford: it is not wrong about where
/// you are, it is silent about not knowing.
///
/// [`Self::Stale`] is inferred from the fix not *changing*, which is a compromise:
/// [`GpsFix`] carries no timestamp, so there is nothing to age. A monotonic `at`
/// on the fix, stamped by each provider as it writes the slot, is the real answer
/// and is a `platform::location` change — see this module's commit for the note.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub enum GpsQuality {
    /// This build has no provider at all, so the avatar is the position and
    /// accuracy is not a concept. The desktop default — and not worth a chip,
    /// because it is the normal state of every desktop run.
    #[default]
    NoProvider,
    /// A watch is running and nothing has come back yet. A cold GNSS start is
    /// seconds to minutes on a phone, and that wait deserves to be visible.
    Waiting,
    /// The provider reported an error (a denied permission, most often).
    Error,
    /// Fixes have stopped arriving — see [`GPS_STALE_S`].
    Stale,
    /// Arriving, but no better than this many metres ([`GPS_POOR_M`]).
    Poor(f32),
    /// Arriving, at this reported accuracy in metres.
    ///
    /// **It carries the number even though nothing about driving needs it.**
    /// `Good` was a unit variant while the only consumer was [`Self::chip`],
    /// whose whole rule is that a healthy receiver says nothing. The locate
    /// button is the opposite case: the one thing a person standing still with
    /// the button lit wants to know is that the GPS *is* working, and "±4 m" is
    /// the only evidence of that which is not also true of a receiver that has
    /// never reported. A second field on the state holding the same metres would
    /// be two places to keep in step.
    Good(f32),
}

impl GpsQuality {
    /// Whether the position is too uncertain to conclude anything from.
    ///
    /// Consulted by the reroute, which is the expensive irreversible act: a full
    /// corridor refetch triggered by tunnel-mouth noise costs the seconds the
    /// driver needs for the junction they are actually approaching.
    #[must_use]
    pub fn unreliable(self) -> bool {
        matches!(self, Self::Poor(_) | Self::Stale)
    }

    /// What the chip says, or `None` when there is nothing worth saying. A chip
    /// that is always up is a chip nobody reads.
    #[must_use]
    pub fn chip(self) -> Option<String> {
        match self {
            Self::NoProvider | Self::Good(_) => None,
            Self::Waiting => Some("Waiting for GPS…".to_string()),
            Self::Error => None, // the provider's own message is shown instead
            Self::Stale => Some("GPS signal lost — showing the last position".to_string()),
            Self::Poor(m) => Some(format!("Poor GPS signal (±{m:.0} m)")),
        }
    }

    /// The line under the locate button — the terse form of the same fact.
    ///
    /// **Not [`Self::chip`], and the difference is the whole point.** The chip is
    /// an exception report: it appears when something is wrong and is silent when
    /// the receiver is healthy, which is right for a navigation card that already
    /// has a manoeuvre on it. Under the locate button that rule produces a lit
    /// button with nothing beside it in *every* state — searching, locked on,
    /// permission denied — which is the complaint this exists to answer. So this
    /// one always says something while tracking, and says the accuracy when there
    /// is nothing wrong.
    ///
    /// `None` only where there is genuinely nothing to report: a build with no
    /// provider at all, where the button follows the avatar and an accuracy would
    /// be an invented number.
    #[must_use]
    pub fn caption(self) -> Option<String> {
        match self {
            Self::NoProvider => None,
            Self::Waiting => Some("Searching…".to_string()),
            // Short because it sits under a 40 pt disc; the provider's own
            // sentence is still on the hover text and in driver mode's card.
            Self::Error => Some("No GPS".to_string()),
            Self::Stale => Some("No signal".to_string()),
            Self::Poor(m) | Self::Good(m) => Some(format_accuracy(m)),
        }
    }

    /// The reported accuracy in metres, where the state has one.
    #[must_use]
    pub const fn accuracy_m(self) -> Option<f32> {
        match self {
            Self::Poor(m) | Self::Good(m) => Some(m),
            _ => None,
        }
    }

    /// Whether a usable position is in hand right now — what the locate button
    /// fills its arrow on.
    ///
    /// **Not `accuracy_m().is_some()`, and the difference is a whole platform.**
    /// [`Self::NoProvider`] has no accuracy because there is no receiver to
    /// report one, not because it is still looking: on a desktop the button
    /// follows the avatar and always knows exactly where that is. Deriving the
    /// fill from the accuracy would have drawn every desktop run in the searching
    /// state forever, waiting for a satellite that was never coming.
    ///
    /// [`Self::Poor`] counts. The fix is genuinely in hand and merely loose,
    /// which the ring's colour and the halo's size both say; a hollow arrow there
    /// would claim there is no position at all.
    #[must_use]
    pub const fn has_position(self) -> bool {
        !matches!(self, Self::Waiting | Self::Error | Self::Stale)
    }
}

/// A horizontal accuracy, written the way a map app writes it.
///
/// Rounded to whole metres below a kilometre and to kilometres above, because the
/// figure is a confidence and not a measurement: "±1,247 m" claims four
/// significant digits about a number whose own error bar is most of it. A
/// kilometre-scale accuracy is what a network-derived fix reports indoors, so it
/// is a state a phone reaches often rather than a curiosity.
///
/// Below a metre it is centimetres, which only an RTK receiver reaches
/// (`platform::gnss_receiver`): whole metres would round a fixed solution's
/// ±2 cm to "±0 m", which reads as a fault.
#[must_use]
pub fn format_accuracy(metres: f32) -> String {
    if metres >= 1000.0 {
        format!("±{:.0} km", metres / 1000.0)
    } else if metres >= 0.995 {
        format!("±{metres:.0} m")
    } else {
        format!("±{:.0} cm", (metres * 100.0).max(1.0))
    }
}

/// Classify a fix. Pure, so the states that only occur on a phone in a tunnel are
/// checkable on a desktop with no receiver.
///
/// Shared with [`crate::systems::locate`] rather than copied into it: the locate
/// button and the navigation card answer the same question about the same watch,
/// and two classifiers would eventually disagree about whether the GPS is
/// working — on one screen, at the same moment.
pub(crate) fn gps_quality(
    available: bool,
    error: bool,
    accuracy_m: Option<f64>,
    since_fix_s: Option<f32>,
) -> GpsQuality {
    if error {
        return GpsQuality::Error;
    }
    if !available {
        return GpsQuality::NoProvider;
    }
    let Some(accuracy_m) = accuracy_m else {
        return GpsQuality::Waiting;
    };
    if since_fix_s.is_some_and(|s| s > GPS_STALE_S) {
        return GpsQuality::Stale;
    }
    if accuracy_m > GPS_POOR_M {
        return GpsQuality::Poor(accuracy_m as f32);
    }
    GpsQuality::Good(accuracy_m as f32)
}

/// Whether the guidance cues sound — the field a Settings row binds to.
///
/// A newtype rather than a bare `bool` for one reason: its default is *on*, and
/// [`DriverState`] keeps its derived `Default` (two dozen fields, every one of
/// which wants the zero value). Eyes-off-road guidance that has to be switched on
/// before it works is guidance nobody has.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct GuidanceSound(pub bool);

impl Default for GuidanceSound {
    fn default() -> Self {
        Self(true)
    }
}

/// What the camera was doing before driver mode took it, so leaving hands it back
/// exactly as it was. The same restore-on-exit contract as `data_layers`' minimal
/// mode, which driver mode also drives.
#[derive(Clone, Copy)]
struct CameraSave {
    mode: CameraMode,
    distance: f32,
    pitch: f32,
    azimuth: f32,
    follow_rotate: bool,
    /// Whether the camera was inside the avatar's head
    /// ([`CameraState::first_person`]). Driver mode clears it on the way in —
    /// it draws its own marker in place of the avatar and there is no first
    /// person inside that — so without this line a drive would quietly cost the
    /// user a view they had zoomed into, exactly as it used to cost them Earth
    /// mode. Same argument as [`DriverState::pose_before_drive`], one field
    /// along.
    first_person: bool,
}

/// The rig offsets a user's own camera input has accumulated, and when it last
/// arrived. Identity is "the driving pose", which is what a default must be.
///
/// Kept as *offsets* rather than as an absolute pose so everything the rig does
/// on its own — the orientation cycle, the speed stretch, the junction tighten —
/// keeps working underneath a peek instead of being suspended by it.
#[derive(Clone, Copy)]
struct Peek {
    /// Multiplier on the rig's distance.
    zoom: f32,
    /// Added to the orientation's azimuth, in radians.
    azimuth: f32,
    /// Added to the orientation's pitch, in radians.
    pitch: f32,
    /// When the last input arrived; [`PEEK_TIMEOUT`] is measured from it.
    since: Option<Instant>,
    /// Where the mouse was on the previous frame of a drag.
    drag: Option<Vec2>,
}

impl Default for Peek {
    fn default() -> Self {
        Self {
            zoom: 1.0,
            azimuth: 0.0,
            pitch: 0.0,
            since: None,
            drag: None,
        }
    }
}

impl Peek {
    /// Whether the user is still holding the view away from the driving pose.
    fn held(self) -> bool {
        self.since
            .is_some_and(|t| t.elapsed().as_secs_f32() < PEEK_TIMEOUT)
    }
}

/// Where along the route the driver is. Recomputed each frame from one position;
/// see [`project_onto_route`], which is where all of it actually happens.
#[derive(Clone, Copy, Debug, Default)]
pub struct Progress {
    /// Metres from the route's start to the projected position. The chrome shows
    /// what is *left*, but this is the projection's primary result — everything
    /// else here is derived from it, and it is what the tests pin down.
    #[allow(dead_code)]
    pub along_m: f64,
    /// Metres from the position to the nearest point of the route.
    pub off_route_m: f64,
    /// Metres left to drive.
    pub remaining_m: f64,
    /// Seconds left, at the pace the planner solved for.
    pub remaining_s: f64,
    /// Index into [`Route::steps`] of the manoeuvre being driven toward.
    pub step: usize,
    /// Metres to that manoeuvre.
    pub step_distance_m: f64,
    /// Whether that manoeuvre is something you *do* — a turn or the arrival — as
    /// against carrying on down the same road. The rig tightens onto the first
    /// kind and leaves the second alone (see [`RIG_JUNCTION_M`]), which is the
    /// difference between a view that closes in when it matters and one that
    /// breathes in and out for every vertex of a curve.
    pub turn_ahead: bool,
}

/// The route measured out along its own polyline: cumulative metres per vertex,
/// and where each step's manoeuvre falls on it.
///
/// Built once per route rather than per frame, and it is what makes the readout
/// independent of the planner's own distance bookkeeping — every number comes off
/// the same polyline the line on the map is drawn from.
///
/// Public because [`crate::systems::route_ahead`] measures the road ahead against
/// it too. Sharing the geometry rather than rebuilding it there is the point: a
/// rail marker at "9.4 km" and a readout saying "9.4 km to go" have to mean the
/// same thing, which they only do while both are projected onto *this* polyline —
/// pins stripped, cumulative metres per vertex.
pub struct RouteGeom {
    /// `[lat, lng]` per vertex, road portion only (the pin lead-ins are not driven).
    pub points: Vec<[f64; 2]>,
    /// Metres from the start to each vertex; `cum[0] == 0`.
    pub cum: Vec<f64>,
    pub total_m: f64,
    /// Metres from the start to each step's manoeuvre, one per [`Route::steps`].
    pub step_along: Vec<f64>,
}

/// Driver mode's own state. `active` is the switch everything else keys off —
/// `data_layers::apply_minimal_mode` strips the map from it, `free_camera` locks
/// the rig from `CameraState::drive_locked`, which this sets.
#[derive(Resource, Default)]
pub struct DriverState {
    pub active: bool,
    pub orientation: MapOrientation,
    /// Live progress, `None` until a position has been projected onto the route.
    pub progress: Option<Progress>,
    /// Displayed ground speed in m/s (smoothed; see [`SPEED_SMOOTH_RATE`]).
    pub speed_ms: f32,
    /// Course over ground in radians, as a world-XZ direction angle — the same
    /// convention as the avatar's yaw, so `Quat::from_rotation_y(course)` turns
    /// the arrow the right way.
    pub course: f32,
    /// Provider trouble worth telling the user about (a denied permission).
    pub gps_error: Option<String>,
    /// Set while a device fix — rather than the avatar — is driving the position.
    pub gps_fix: bool,
    /// How much this frame's position can be trusted. Drawn as a chip, and read by
    /// the reroute before it spends a corridor fetch on it.
    pub gps: GpsQuality,
    /// Whether the guidance cues sound. Public so a Settings row can bind to it.
    pub cues: GuidanceSound,
    /// The solver's own message when a reroute could not be answered.
    ///
    /// Kept here rather than read off `SolveStatus` at the point of drawing so that
    /// the card and the retry are looking at the same fact — and so that pressing
    /// Retry can clear it in the frame the request goes out, rather than leaving
    /// "no route" on screen beside a spinner.
    pub solve_error: Option<String>,
    /// The Retry button was pressed. An edge, consumed by `drive_driver_mode`,
    /// which owns every reroute — the HUD is not the place to reach into
    /// `RouteState`.
    retry_requested: bool,
    /// Where to draw the vehicle marker instead of the raw position, as an offset
    /// in world XZ metres, plus the yaw of the road it was snapped to. `None`
    /// beyond [`SNAP_M`], where the raw fix is the honest thing to draw.
    snap: Option<(Vec2, f32)>,
    /// The last *distinct* fix, and when it arrived — the only handle on staleness
    /// there is, `GpsFix` carrying no timestamp.
    last_fix: Option<GpsFix>,
    last_fix_at: Option<Instant>,
    /// A reroute is in flight — the card says so instead of naming a manoeuvre off
    /// a route the vehicle has already left.
    pub rerouting: bool,
    /// The destination has been reached ([`ARRIVE_M`]). Latched rather than
    /// recomputed: driving the last few metres *past* the pin would otherwise
    /// take the arrival card away again, and the last thing a navigation display
    /// should do at the end of a trip is change its mind about having finished.
    ///
    /// It also switches the reroute machinery off. Parking at the destination
    /// puts you tens of metres from the line by design, and a display that
    /// answered that by planning a fresh route to where you already are would be
    /// hard to read as anything but broken.
    pub arrived: bool,
    /// The user's own camera offsets on top of the rig — see [`Peek`].
    peek: Peek,
    /// Since when the position has been further than [`OFF_ROUTE_M`] from the
    /// route; the banner waits [`OFF_ROUTE_GRACE`] on this. `None` while on route.
    off_route_since: Option<Instant>,
    /// When the last reroute was requested; [`REROUTE_COOLDOWN`] is measured from
    /// it. `None` until one has been.
    reroute_at: Option<Instant>,
    /// `RouteState::generation` at the moment a reroute was asked for; the request
    /// is outstanding until it changes. `None` when none is.
    reroute_gen: Option<u32>,

    /// The device-location subscription. `Some` only while driving, so nothing
    /// keeps the browser's location indicator lit after you stop.
    watch: Option<LocationWatch>,
    geom: Option<RouteGeom>,
    /// `RouteState::generation` the cached [`RouteGeom`] was built from.
    geom_generation: Option<u32>,
    camera_save: Option<CameraSave>,
    /// Previous position and when it was sampled, for the measured speed/course.
    last_sample: Option<(DVec3, Instant)>,
    /// Spreads the step between two fixes over the frames between them — see
    /// [`FixGlide`]. Not `pub`: it is where the marker *is*, and everything else
    /// already reads that off the avatar.
    glide: FixGlide,
    /// Set on entry: the avatar is put on the route's start before the first frame
    /// of driving (see `drive_driver_mode`).
    snap_pending: bool,
    /// "Start driving as soon as there *is* a route." Entering needs a solved one,
    /// and a solve is a worker round-trip, so a caller that asks for both at once
    /// — the `--ui driver` shot preset seeds endpoints and cannot wait — arms this
    /// instead of racing the solver.
    armed: bool,
}

impl DriverState {
    /// Arm driver mode. Callers gate this on a solved route — the button is only
    /// drawn beside one — but it is re-checked every frame in `drive_driver_mode`,
    /// which drops out of driver mode the moment the route goes away.
    pub fn enter(&mut self) {
        if self.active {
            return;
        }
        self.active = true;
        self.snap_pending = true;
        self.off_route_since = None;
        self.reroute_at = None;
        self.reroute_gen = None;
        self.rerouting = false;
        self.arrived = false;
        self.peek = Peek::default();
        self.progress = None;
        self.speed_ms = 0.0;
        self.last_sample = None;
        self.glide.reset();
        self.gps_error = None;
        self.gps_fix = false;
        self.gps = GpsQuality::default();
        self.last_fix = None;
        self.last_fix_at = None;
        self.snap = None;
        self.solve_error = None;
        self.retry_requested = false;
        self.watch = Some(LocationWatch::start());
        tracing::info!(gps = LocationWatch::available(), "driver mode on");
    }

    /// Ask for driver mode without a route yet: it starts the moment one solves.
    pub fn arm(&mut self) {
        self.armed = true;
    }

    /// The camera pose as it was *before* driver mode took the rig, or `None`
    /// when driver mode is not holding it.
    ///
    /// What this is for: `user_store::remember_location` autosaves the camera
    /// every five seconds, and while driving the live `CameraState` is the chase
    /// rig — Follow, a fixed distance, a computed azimuth. Save that and the next
    /// launch comes back in Follow behind an avatar, having quietly forgotten
    /// that the user was looking at the world in Earth mode when they pressed
    /// Start. Driver mode already restores this pose on the way out; this makes a
    /// process that never gets a way out — the phone killing a backgrounded app —
    /// restore it too. Same principle as `data_layers::sky_prefs`, which likewise
    /// lets the store see past driver mode's temporary clear sky.
    #[must_use]
    pub fn pose_before_drive(&self, live: &CameraState) -> Option<CameraPose> {
        let save = self.camera_save?;
        let mut pose = CameraPose::capture(live);
        pose.mode = crate::systems::geo_nav::mode_name(save.mode).to_string();
        pose.distance = save.distance;
        pose.pitch = save.pitch;
        pose.azimuth = save.azimuth;
        Some(pose)
    }

    /// Leave driver mode. The camera restore lives in `steady_driver_camera`,
    /// which owns `CameraState`; this only drops the subscription and the flag.
    pub fn exit(&mut self) {
        if !self.active {
            return;
        }
        self.active = false;
        self.armed = false;
        self.watch = None;
        self.progress = None;
        self.glide.reset();
        self.gps_fix = false;
        self.gps = GpsQuality::default();
        self.snap = None;
        self.solve_error = None;
        self.retry_requested = false;
        self.arrived = false;
        self.peek = Peek::default();
        tracing::info!("driver mode off");
    }
}

// ─── Progress along the route ────────────────────────────────────────────────

/// Metres between two `[lat, lng]` points, on the local tangent plane.
///
/// Equirectangular rather than haversine on purpose: over the tens of metres
/// between consecutive route vertices the two agree to well under a millimetre,
/// and this is called for every vertex of the route on every frame.
pub(crate) fn metres_between(a: [f64; 2], b: [f64; 2]) -> f64 {
    let mid = (a[0] + b[0]) * 0.5;
    let dx = (b[1] - a[1]) * 111_320.0 * mid.to_radians().cos();
    let dy = (b[0] - a[0]) * 110_540.0;
    (dx * dx + dy * dy).sqrt()
}

impl RouteGeom {
    /// Measure a route out along its own polyline.
    ///
    /// The first and last points of `Route::points` are the *pins* — where the
    /// user aimed — and the lead-in from a pin to the road is not something you
    /// drive (it is the walk across the pavement, and it is drawn as a thin
    /// connector for exactly that reason). So the driven polyline is the road
    /// portion, which is also what the fat route line covers.
    pub fn build(route: &Route) -> Option<Self> {
        let n = route.points.len();
        if n < 4 {
            // Fewer than two road vertices between the pins: nothing to drive.
            return None;
        }
        let points: Vec<[f64; 2]> = route.points[1..n - 1].to_vec();
        let mut cum = Vec::with_capacity(points.len());
        let mut total = 0.0;
        cum.push(0.0);
        for w in points.windows(2) {
            total += metres_between(w[0], w[1]);
            cum.push(total);
        }

        // Each step's manoeuvre is a point *on* this polyline, so its distance
        // along it is found the same way a position is: by projection. Deriving it
        // from the steps' own `distance_m` instead would drift against the
        // polyline whenever the two disagree, and then the card would count down
        // to a turn that is not where it says it is.
        let step_along = route
            .steps
            .iter()
            .map(|s| nearest_on_polyline(&points, &cum, s.at).0)
            .collect();

        Some(Self {
            points,
            cum,
            total_m: total,
            step_along,
        })
    }
}

/// Project a point onto a polyline: `(metres along, metres off)`.
///
/// Segment-wise, not vertex-wise. A vertex-only search reports you as "at" the
/// last junction until you are nearer the next one, which on a 200 m straight
/// freezes the remaining distance and then drops it 200 m at once.
pub fn nearest_on_polyline(points: &[[f64; 2]], cum: &[f64], at: [f64; 2]) -> (f64, f64) {
    let mut best = (0.0, f64::MAX);
    for (i, w) in points.windows(2).enumerate() {
        let (a, b) = (w[0], w[1]);
        // Work in local metres about `a`, so the projection is a plain 2D one.
        let mid = (a[0] + b[0]) * 0.5;
        let mx = 111_320.0 * mid.to_radians().cos();
        let seg = ((b[1] - a[1]) * mx, (b[0] - a[0]) * 110_540.0);
        let pt = ((at[1] - a[1]) * mx, (at[0] - a[0]) * 110_540.0);
        let len2 = seg.0 * seg.0 + seg.1 * seg.1;
        let t = if len2 > 1e-9 {
            ((pt.0 * seg.0 + pt.1 * seg.1) / len2).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let (dx, dy) = (pt.0 - seg.0 * t, pt.1 - seg.1 * t);
        let off = (dx * dx + dy * dy).sqrt();
        if off < best.1 {
            best = (cum[i] + t * len2.sqrt(), off);
        }
    }
    best
}

/// Where a position sits on the route, and what it still has to do.
///
/// Pure over the measured geometry so the arithmetic that produces the bottom bar
/// is testable without a camera, a window, or a fix.
pub(crate) fn project_onto_route(geom: &RouteGeom, route: &Route, at: [f64; 2]) -> Progress {
    let (along, off) = nearest_on_polyline(&geom.points, &geom.cum, at);
    let remaining_m = (geom.total_m - along).max(0.0);
    // Time is scaled from the planner's own answer rather than from the current
    // speed: the planner knows the speed limits of the road ahead, and a driver
    // stopped at a light does not have an infinite ETA.
    let fraction = if geom.total_m > 1.0 {
        remaining_m / geom.total_m
    } else {
        0.0
    };
    let step = geom
        .step_along
        .iter()
        .position(|&s| s > along - STEP_PASSED_M)
        .unwrap_or(route.steps.len().saturating_sub(1));
    let step_distance_m = geom
        .step_along
        .get(step)
        .map_or(0.0, |&s| (s - along).max(0.0));
    let turn_ahead = route
        .steps
        .get(step)
        .is_some_and(|s| !matches!(s.maneuver, Maneuver::Continue | Maneuver::Depart));
    Progress {
        along_m: along,
        off_route_m: off,
        remaining_m,
        remaining_s: route.duration_s * fraction,
        step,
        step_distance_m,
        turn_ahead,
    }
}

// ─── Systems ─────────────────────────────────────────────────────────────────

/// The driver-mode tick: hold or drop the mode, fold in a device fix, and measure
/// speed, course and progress from wherever the position ended up.
#[allow(clippy::too_many_arguments)]
pub fn drive_driver_mode(
    mut driver: ResMut<DriverState>,
    mut route: ResMut<RouteState>,
    mut nav: ResMut<GeoNavState>,
    time: Res<Time>,
    mut avatar_state: ResMut<AvatarState>,
    mut avatars: Query<(&mut GridCell, &mut Transform), (With<Avatar>, Without<FreeCamera>)>,
    map_anchor: Res<MapAnchor>,
    // Online, a device fix has to reach the sim or the next snapshot undoes it —
    // see [`place_avatar`], which is where that happens for both callers.
    online: Option<Res<OnlineSession>>,
) {
    // `route` is mutable only so leaving the route can ask for a new one (see the
    // reroute at the end of this system); everything else here reads it.
    if driver.armed && route.route.is_some() {
        driver.armed = false;
        driver.enter();
    }
    // The mode exists only for a route to drive. Clearing one, or editing an
    // endpoint, drops straight out — better than driving a line that is gone.
    // A solve *in flight* is not gone, though: switching travel mode mid-drive
    // re-runs the search over the cached graph, and being thrown out of navigation
    // for the frames that takes would be absurd.
    //
    // **Nor is a reroute that failed.** `RouteState::displayed` still answers with
    // the route being replaced (`held`), and that is deliberately what this tests:
    // an underground car park, a missed motorway exit and a tile-server hiccup all
    // end a solve with `route == None`, and the previous behaviour was to leave
    // navigation entirely — silently, because the message lives in the Route panel,
    // which driver mode closed on the way in and whose dock is hidden while driving.
    // The drive continues, the card says what happened, and the driver decides.
    let solving = matches!(
        route.status,
        crate::systems::navigation::SolveStatus::Fetching(..)
            | crate::systems::navigation::SolveStatus::Solving
    );
    if driver.active && route.displayed().is_none() && !solving {
        driver.exit();
    }
    if !driver.active {
        driver.geom = None;
        driver.geom_generation = None;
        return;
    }
    let Some(r) = route.displayed() else {
        return;
    };
    let Some(anchor) = map_anchor.geo else {
        return;
    };
    if driver.geom_generation != Some(route.generation) {
        driver.geom = RouteGeom::build(r);
        driver.geom_generation = Some(route.generation);
        // A different route is a different trip. Arriving is latched against the
        // route it was measured on, so a reroute — or a destination changed
        // mid-drive — has to clear it, or the display would open the new one with
        // the previous one's arrival card still up.
        driver.arrived = false;
    }

    // A device fix wins over the avatar: that is what navigating is. Desktop has
    // no provider, so there this arm never fires and the avatar keeps its position
    // — driver mode then narrates the user's own movement, which is exactly what
    // makes it usable without a dongle.
    let now = Instant::now();
    let fix = driver.watch.as_ref().and_then(LocationWatch::latest);
    driver.gps_error = driver.watch.as_ref().and_then(LocationWatch::error);
    driver.gps_fix = fix.is_some();

    // ── How much the fix is worth ──
    //
    // Freshness is measured by the fix *changing*, not by its age: `GpsFix` has no
    // timestamp to age. A receiver that keeps reporting the same coordinate to the
    // millimetre has either stopped or is parked, and the two are indistinguishable
    // from here — hence a generous `GPS_STALE_S`, and a message that says the
    // position is the last known one rather than claiming the receiver is broken.
    //
    // Whether this is a report we have not applied yet. The provider republishes
    // the same value until the next one lands, so this is what separates "one
    // fix" from "sixty frames looking at one fix" — see [`place_avatar`], which
    // is only allowed to reach the sim on the former.
    let fix_is_new = fix.is_some_and(|f| driver.last_fix != Some(f));
    if let Some(f) = fix {
        if fix_is_new {
            driver.last_fix = Some(f);
            driver.last_fix_at = Some(now);
        }
    }
    driver.gps = gps_quality(
        LocationWatch::available(),
        driver.gps_error.is_some(),
        fix.map(|f| f.accuracy_m),
        driver
            .last_fix_at
            .map(|t| now.duration_since(t).as_secs_f32()),
    );

    if let Some(fix) = fix {
        // Near or far — see [`GPS_TELEPORT_M`]. Measured against where the avatar
        // stands rather than against the previous fix, because the thing that must
        // not happen is the *world* being left behind, and the avatar's position is
        // what the map, the camera and every streamer are built around.
        if fix_is_far(anchor, fix, &avatars) {
            tracing::info!(
                lat = fix.lat,
                lng = fix.lng,
                "device fix is far from the anchored world — teleporting rather than warping the avatar"
            );
            // The world is about to be re-anchored under the marker, so the frame
            // the glide's last position was measured in stops existing.
            driver.glide.reset();
            nav.pending_teleport = Some(Teleport::geo(fix.lat, fix.lng, None).silently());
        } else {
            // Spread the step between two fixes over the frames between them —
            // see [`FixGlide`]. Everything downstream reads the avatar's own
            // position, so the arrow, the chase camera, the measured course and
            // the progress along the route all become continuous together.
            let (wx, wz) = lat_lng_to_world(anchor, fix.lat, fix.lng);
            let (gx, gz) = driver
                .glide
                .advance((wx, wz), fix_is_new, now, time.delta_secs());
            place_avatar(
                gx,
                gz,
                &mut avatar_state,
                &mut avatars,
                online.as_deref(),
                fix_is_new,
            );
        }
        driver.snap_pending = false;
    } else if driver.snap_pending {
        // No provider: start the drive at the route's start rather than wherever
        // the avatar happened to be standing, which is usually a different city.
        // A teleport (not a bare position write) because it re-anchors the map and
        // brings the camera with it.
        driver.snap_pending = false;
        if let Some(start) = driver.geom.as_ref().and_then(|g| g.points.first()).copied() {
            driver.glide.reset();
            nav.pending_teleport = Some(Teleport::geo(start[0], start[1], None).silently());
        }
    }

    // Speed and course from successive positions. Absolute world metres (cell +
    // local): the avatar's translation is cell-relative, and crossing a cell
    // boundary would otherwise read as a jump of one cell edge per frame.
    let Ok((cell, tf)) = avatars.single() else {
        return;
    };
    let edge = f64::from(WORLD_CELL_EDGE);
    let here = DVec3::new(
        cell.x as f64 * edge + f64::from(tf.translation.x),
        0.0,
        cell.z as f64 * edge + f64::from(tf.translation.z),
    );
    let (mut measured_speed, mut measured_course) = (None, None);
    if let Some((prev, t)) = driver.last_sample {
        let dt = (now - t).as_secs_f32();
        let step = here - prev;
        // A teleport (entering the mode, a re-anchor) moves the position by more
        // than any vehicle could in one frame; sampling that would show 4000 km/h.
        let plausible = dt > 1e-3 && step.length() < f64::from(dt) * 200.0;
        if plausible {
            let speed = (step.length() / f64::from(dt)) as f32;
            measured_speed = Some(speed);
            if speed > COURSE_MIN_SPEED {
                measured_course = Some(f32::atan2(step.x as f32, step.z as f32));
            }
        }
    }
    driver.last_sample = Some((here, now));

    // The device's own speed/course beat a measured one when it reports them: a
    // receiver derives them from Doppler, not from two positions and a stopwatch.
    let fix_speed = fix.and_then(|f| f.speed_ms).map(|s| s as f32);
    let fix_course = fix
        .and_then(|f| f.heading_deg)
        .map(|deg| (deg as f32).to_radians());
    let target_speed = fix_speed.or(measured_speed).unwrap_or(driver.speed_ms);
    let alpha = 1.0 - (-SPEED_SMOOTH_RATE * time.delta().as_secs_f32()).exp();
    driver.speed_ms += (target_speed - driver.speed_ms) * alpha;
    if driver.speed_ms.abs() < 0.05 {
        driver.speed_ms = 0.0;
    }
    if let Some(course) = fix_course.or(measured_course) {
        driver.course = course;
    } else if driver.last_sample.is_some() && driver.progress.is_none() {
        // Nothing has moved yet: face along the route rather than at an arbitrary
        // compass point, so the first frame of driving is already oriented.
        if let Some(g) = driver.geom.as_ref() {
            if let (Some(a), Some(b)) = (g.points.first(), g.points.get(1)) {
                let (ax, az) = lat_lng_to_world(anchor, a[0], a[1]);
                let (bx, bz) = lat_lng_to_world(anchor, b[0], b[1]);
                driver.course = f32::atan2((bx - ax) as f32, (bz - az) as f32);
            }
        }
    }

    let (lat, lng) = world_to_lat_lng(anchor, here.x, here.z);
    let progress = driver
        .geom
        .as_ref()
        .map(|g| project_onto_route(g, r, [lat, lng]));

    // ── Where the marker is *drawn* ──
    //
    // On the road, when the fix is close enough to it to make that a statement
    // rather than a guess (`SNAP_M`). This is the one number on screen that is
    // read continuously and never consciously, and an arrow that wanders through
    // the terrace beside the street undoes the display's credibility about
    // everything else it says. Beyond the threshold the raw position is drawn,
    // which is the honest picture of being somewhere the route does not go.
    //
    // The snapped *yaw* is only taken when the road agrees with the measured
    // course. On a route driven backwards — a wrong turn into a one-way, the frames
    // after a U-turn — the segment points where you are being sent rather than
    // where you are going, and an arrow contradicting the view out of the window is
    // worse than one that is merely noisy.
    let snap = progress.and_then(|p| {
        if p.off_route_m > SNAP_M {
            return None;
        }
        let geom = driver.geom.as_ref()?;
        let (on, ahead) = point_along(geom, p.along_m)?;
        let (sx, sz) = lat_lng_to_world(anchor, on[0], on[1]);
        let (ax, az) = lat_lng_to_world(anchor, ahead[0], ahead[1]);
        let seg_yaw = f32::atan2((ax - sx) as f32, (az - sz) as f32);
        let offset = Vec2::new((sx - here.x) as f32, (sz - here.z) as f32);
        Some((offset, snapped_yaw(driver.course, seg_yaw)))
    });
    driver.snap = snap;
    // ── The end of the trip ──
    //
    // Latched (see `DriverState::arrived`): once, on the frame the destination
    // comes within reach, and not unlatched by driving past it. Being off the
    // line does not count — the last few metres of a route commonly are, and a
    // vehicle 200 m away on a parallel street has not arrived at anything.
    if let Some(p) = progress {
        if p.remaining_m <= ARRIVE_M && p.off_route_m <= OFF_ROUTE_M {
            if !driver.arrived {
                tracing::info!("arrived");
            }
            driver.arrived = true;
        }
    }

    // Debounce the off-route judgement (see `OFF_ROUTE_GRACE`). A queued teleport
    // is not "off route" either: the position it will land at has not been applied
    // yet, and the anchor it is measured against is about to move. Nor is having
    // parked at the destination, which is what `arrived` is doing in this test.
    let off = progress.is_some_and(|p| p.off_route_m > OFF_ROUTE_M) && !driver.arrived;
    if !off || nav.pending_teleport.is_some() {
        driver.off_route_since = None;
    } else if driver.off_route_since.is_none() {
        driver.off_route_since = Some(now);
    }
    driver.progress = progress;

    // ── Leaving the route asks for a new one, it is not an error ──
    //
    // The first cut painted "Off route — 140 m from the route" and stopped there,
    // which is the one moment a navigation display is *needed* and the only one
    // where it gave up: the driver is somewhere the app did not expect and the
    // manoeuvre card has nothing useful to say. Missing a turn is the normal case,
    // not a fault, so the answer is a new route from where you actually are.
    // A reroute is "in flight" until a *different* route lands, tracked by
    // generation rather than by `SolveStatus`. Status alone would also be true for
    // every unrelated re-solve — switching travel mode mid-drive re-runs the
    // search — and the card would announce "Rerouting… from your current position"
    // for something the user had just asked for on purpose.
    if driver.reroute_gen.is_some_and(|g| g != route.generation) {
        driver.reroute_gen = None;
    }
    driver.rerouting = driver.reroute_gen.is_some();

    // A solve that failed is the reroute's other outcome, and it has to be visible
    // *here* — the Route panel's status line is unreachable mid-drive.
    //
    // It stops being news the moment the vehicle is back on the line it is
    // following: a driver who has rejoined the route by hand does not need a card
    // about the route they no longer want. Keyed on the measured distance rather
    // than on `off_route()`, whose grace period would otherwise put two seconds of
    // "Off route" between the failure and the card that explains it.
    let back_on_route = progress.is_some_and(|p| p.off_route_m <= OFF_ROUTE_M);
    driver.solve_error = match &route.status {
        crate::systems::navigation::SolveStatus::Failed(msg)
            if !driver.rerouting && !back_on_route =>
        {
            Some(msg.clone())
        }
        _ => None,
    };

    // Retry is the same request without the cooldown: the driver has looked at the
    // failure and asked again, which is exactly the case the loop breaker is not
    // for. The automatic attempt keeps its floor and keeps running underneath — a
    // car that failed to reroute in a tunnel gets a fresh route on its own once the
    // fix comes back, without anyone pressing anything.
    let retry = std::mem::take(&mut driver.retry_requested);
    let automatic = driver.off_route()
        && !driver.rerouting
        && driver.reroute_ready(now)
        // A fix whose error bar is as wide as the off-route threshold cannot tell
        // "left the route" from "left the tunnel". Rerouting on it spends a whole
        // corridor refetch on noise, at the moment the driver can least afford the
        // display to be busy.
        && !driver.gps.unreliable();
    if retry || automatic {
        driver.reroute_at = Some(now);
        // Only the start moves. The destination is what the user asked for and a
        // reroute must never quietly change it — and re-anchoring the start is
        // exactly what the solver keys on, so this is the whole request.
        //
        // `reroute_from`, not `set_endpoint_at`: it parks the current route in
        // `RouteState::held` so the line, the rail, the distance and the ETA all
        // keep coming from it while the corridor is refetched. The generation is
        // deliberately *not* bumped by it, which is what leaves the drawn line
        // alone — so this is captured as the value that holds until the solve lands
        // and bumps it.
        route.reroute_from(lat, lng);
        driver.reroute_gen = Some(route.generation);
        // Clear the debounce with it: the position is no longer off *this* route
        // because there is about to be a different one, and leaving it armed would
        // fire a second reroute the instant the cooldown expired.
        driver.off_route_since = None;
        driver.rerouting = true;
        driver.solve_error = None;
        tracing::info!(lat, lng, retry, "rerouting from the current position");
    }
}

/// The point `along` metres into a measured polyline, and the far end of the
/// segment it falls on — enough to place the marker and to aim it.
///
/// Separate from [`nearest_on_polyline`], which answers "how far along and how far
/// off" and throws the projected point away. It is the same arithmetic run
/// forwards, and doing it this way round means the snapped marker sits at exactly
/// the distance the card is counting down from, rather than at a second, nearly
/// equal projection of its own.
pub(crate) fn point_along(geom: &RouteGeom, along: f64) -> Option<([f64; 2], [f64; 2])> {
    if geom.points.len() < 2 {
        return None;
    }
    let along = along.clamp(0.0, geom.total_m);
    let mut i = 0;
    while i + 2 < geom.points.len() && geom.cum[i + 1] < along {
        i += 1;
    }
    let (a, b) = (geom.points[i], geom.points[i + 1]);
    let seg = geom.cum[i + 1] - geom.cum[i];
    let t = if seg > 1e-6 {
        ((along - geom.cum[i]) / seg).clamp(0.0, 1.0)
    } else {
        0.0
    };
    Some(([a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t], b))
}

/// The yaw to draw the marker at: the road's, unless the vehicle is plainly not
/// going that way.
///
/// A quarter turn is the cut-off, so ordinary noise about the course — which is
/// smoothed, and holds its last value at a standstill — still snaps to the street,
/// while a reversal does not.
fn snapped_yaw(course: f32, segment: f32) -> f32 {
    let mut delta = (segment - course).rem_euclid(std::f32::consts::TAU);
    if delta > std::f32::consts::PI {
        delta -= std::f32::consts::TAU;
    }
    if delta.abs() <= std::f32::consts::FRAC_PI_2 {
        segment
    } else {
        course
    }
}

impl DriverState {
    /// Whether the chrome should say so — off the route, and for long enough that
    /// it is not a transient (see [`OFF_ROUTE_GRACE`]).
    #[must_use]
    pub fn off_route(&self) -> bool {
        self.off_route_since
            .is_some_and(|t| t.elapsed().as_secs_f32() > OFF_ROUTE_GRACE)
    }

    /// Whether enough time has passed since the last reroute to ask for another.
    ///
    /// The cooldown is what stops a reroute *loop*. A solve can legitimately come
    /// back with a route the vehicle is still 60 m from — the snapped start is on
    /// the nearest routable road, and on a dual carriageway or beside a closed
    /// street that road can be the other one — and without a floor between
    /// attempts the display would re-solve every frame it stayed there, hammering
    /// the tile fetch and never settling on a line to draw.
    #[must_use]
    fn reroute_ready(&self, now: Instant) -> bool {
        self.reroute_at
            .is_none_or(|t| now.duration_since(t).as_secs_f32() > REROUTE_COOLDOWN)
    }
}

/// Put the avatar (and so the arrow, the camera and the progress readout) at a
/// position within the current anchor frame.
///
/// Shared with [`crate::systems::locate`], which follows a device fix with no
/// route under it: the near/far rule below is about the *world* being anchored,
/// not about navigating, so both callers need the identical pair.
///
/// The `Without<FreeCamera>` in the filter is for that caller's benefit — it also
/// holds the camera, and Bevy needs the two `&mut Transform` queries to be
/// provably disjoint. Costs this one nothing: an avatar is never a camera.
///
/// **Online, writing the position here is not enough, and doing only that is why
/// device tracking did nothing on a server.** The sim owns the avatar's
/// translation — `network::apply_network_snapshot` assigns
/// [`AvatarState::position`] from every snapshot and
/// `avatar::smooth_online_avatar_display` puts the transform back on it — so a
/// fix written locally was erased twenty times a second and the avatar stayed
/// exactly where the sim had last put it. On foot that reads as an arrow that
/// never leaves the block; on a train it is an avatar that never leaves the
/// station, until the accumulated gap crosses [`GPS_TELEPORT_M`] and the far arm
/// jumps it two kilometres in one go.
///
/// So the sim is told, with the one verb the wire has for placing an avatar
/// ([`cartopolis_core::NetMessage::Teleport`], protocol 17) — the same message
/// `geo_nav::apply_teleport` sends for a searched place or a bookmark, and for
/// the same reason. It carries the **cell-local** translation, because that is
/// the frame the sim's positions are in (`apply_local_avatar_pose_full` assigns a
/// snapshot position straight into `Transform::translation`); sending absolute
/// world metres would put the avatar a cell edge away.
///
/// [`AvatarState::teleport_hold`] goes with it, exactly as it does there: the
/// snapshots already in flight still carry the old position, and adopting one
/// would yank the avatar back for a frame or two before the sim's confirmation
/// lands (`network::adopt_local_pose`). The echo — or the two-second timeout —
/// clears it, so a lost message costs one fix rather than the rest of the trip.
///
/// The message goes only on the frame a *new* fix arrived (`fix_is_new`), and
/// telling which frame that is is the caller's job because only the caller knows. Both
/// of them re-apply the latest fix every frame — `LocationWatch::latest` answers
/// with the last value it was given, not with an event — so a session passed in
/// on every frame would put sixty teleports a second on a wire that refuses
/// intents closer together than 50 ms. One placement per report is what makes it
/// an unrated message rather than a stream, which is also why the sim does not
/// rate-limit teleports.
/// **The position is in absolute anchored world metres, not degrees**, because
/// both callers put it through [`FixGlide`] first and the glide works in metres —
/// degrees are anisotropic, so interpolating in them would move the marker faster
/// east-west than north-south by the cosine of the latitude. `lat_lng_to_world`
/// is one line at each call site and there is nothing to gain by doing it twice.
pub(crate) fn place_avatar(
    wx: f64,
    wz: f64,
    avatar_state: &mut AvatarState,
    avatars: &mut Query<(&mut GridCell, &mut Transform), (With<Avatar>, Without<FreeCamera>)>,
    online: Option<&OnlineSession>,
    fix_is_new: bool,
) {
    let (cell, local) = placement(wx, wz, avatar_state.position.y);
    // Online, `position` is the sim's flat plane and what is drawn stands on the
    // measured ground (`avatar::smooth_online_avatar_display` adds the lifts).
    // Writing the flat position as the display dropped the avatar — and the
    // follow camera with it — to the flat plane on every frame this ran last,
    // and back up on every frame the smoother did: a vertical stutter over any
    // relief. Offline the transform already carries the ground (`position.y` is
    // the lifted height `placement` keeps), so there is nothing to add.
    let display = if online.is_some() {
        local + Vec3::Y * (avatar_state.terrain_lift + avatar_state.floor_lift)
    } else {
        local
    };
    if let Ok((mut c, mut tf)) = avatars.single_mut() {
        *c = cell;
        tf.translation = display;
    }
    avatar_state.position = local;
    avatar_state.display_position = display;
    if let Some(sess) = online.filter(|_| fix_is_new) {
        sess.send(
            "device fix",
            cartopolis_core::NetMessage::Teleport {
                request_id: 0,
                position: local,
            },
        );
        avatar_state.teleport_hold = Some(Instant::now());
    }
}

/// The shortest fix interval the glide will believe, in seconds, and the longest.
///
/// The floor stops a provider that reports twice in one millisecond — the
/// framework does coalesce and re-deliver — from setting a cadence the glide then
/// tries to hold. The ceiling is what makes a *missed* report behave sensibly:
/// without it, one two-second gap teaches the glide to spend two seconds on every
/// later fix, and the marker settles into a permanent crawl behind a receiver that
/// has gone back to reporting normally.
const GLIDE_MIN_INTERVAL_S: f32 = 0.15;
const GLIDE_MAX_INTERVAL_S: f32 = 1.5;

/// Ground speed (m/s) above which a step between two fixes is not travel.
///
/// A re-anchor moves the whole world frame by kilometres and a teleport by more;
/// both reach here as an enormous jump in the target, and gliding through one
/// would send the marker sailing across the map for a second. The same rule
/// `drive_driver_mode` uses on its own samples, and for the same reason: past this
/// the only honest thing to do is arrive.
const GLIDE_IMPLAUSIBLE_MS: f64 = 200.0;

/// Where the marker is drawn *between* two fixes.
///
/// **A receiver reports about once a second and a screen draws sixty times in
/// that second, so without this the map moves in one visible jerk per fix.** That
/// is what "the update rate seems low" is: not a lack of information — the fixes
/// are as frequent as the hardware makes them — but all of the frame's worth of
/// movement being spent on one frame in sixty. The polling underneath stays as
/// fast as it can (`platform::android::POLL_INTERVAL`); this only decides how the
/// distance between two real reports is spread over the frames between them.
///
/// **It interpolates and does not extrapolate, and that choice costs one fix
/// interval of lag.** The marker is aimed to arrive at fix *N*'s position at the
/// moment fix *N+1* is expected, so it is always showing where the device was
/// about half a second ago rather than guessing where it is now. Dead reckoning
/// along the reported course and speed would have no lag at all and is what a
/// racing display would do — and it overshoots every stop, every junction and
/// every corner, then has to be pulled back. Being consistently half a second
/// behind is a smaller lie than being intermittently ten metres wrong in a
/// direction you are no longer travelling.
///
/// The arrival deadline is what makes the motion *even*. An exponential ease
/// toward the fix — the obvious first cut — covers most of the gap in the first
/// few frames and then crawls, so each fix reads as a lurch followed by a pause,
/// which is the original complaint with extra steps.
#[derive(Default)]
pub(crate) struct FixGlide {
    /// Where the marker is being drawn, in absolute anchored world metres. `None`
    /// before the first fix, which is what makes that one a placement rather than
    /// a slide.
    shown: Option<(f64, f64)>,
    /// When the last *new* fix landed, and the cadence measured from the one
    /// before it. Together they say how long is left to arrive.
    fix_at: Option<Instant>,
    interval_s: f32,
}

impl FixGlide {
    /// Forget where the marker was. Called wherever the frame it was measured in
    /// stops existing — a teleport, a re-anchor, entering or leaving the mode —
    /// so the next fix is placed outright instead of glided to from a coordinate
    /// that now means somewhere else.
    pub(crate) fn reset(&mut self) {
        self.shown = None;
        self.fix_at = None;
        self.interval_s = 0.0;
    }

    /// Advance one frame toward `target` (absolute anchored world metres) and
    /// return where to draw. `is_new_fix` distinguishes a report we have not seen
    /// from the provider republishing the last one, which it does on every frame
    /// (`LocationWatch::latest` is a level, not an event).
    pub(crate) fn advance(
        &mut self,
        target: (f64, f64),
        is_new_fix: bool,
        now: Instant,
        dt: f32,
    ) -> (f64, f64) {
        if is_new_fix {
            // The cadence is measured rather than assumed: it differs between the
            // web and Android, between providers, and between a cold start and a
            // warm one, and a hard-coded second would be wrong on all of them.
            if let Some(previous) = self.fix_at {
                let measured = now.duration_since(previous).as_secs_f32();
                self.interval_s = measured.clamp(GLIDE_MIN_INTERVAL_S, GLIDE_MAX_INTERVAL_S);
            }
            self.fix_at = Some(now);
        }
        let Some(shown) = self.shown else {
            // The first fix of a session, or the first after a reset: arrive.
            self.shown = Some(target);
            return target;
        };
        let gap = ((target.0 - shown.0).powi(2) + (target.1 - shown.1).powi(2)).sqrt();
        let elapsed = self
            .fix_at
            .map_or(0.0, |t| now.duration_since(t).as_secs_f32());
        // Not travel — see [`GLIDE_IMPLAUSIBLE_MS`]. Measured against the time the
        // gap opened up in, so the rule is a speed and not a distance: 30 m is an
        // ordinary second on a motorway and an impossible one on a pavement.
        if gap > f64::from(elapsed.max(self.interval_s).max(dt)) * GLIDE_IMPLAUSIBLE_MS {
            self.shown = Some(target);
            return target;
        }
        // How much of what is left to cover belongs to this frame. Once the next
        // fix is overdue this is 1 and the marker simply sits on the last known
        // position, which is the honest picture of a receiver that has gone quiet.
        let remaining = (self.interval_s - elapsed).max(dt);
        let step = f64::from((dt / remaining).clamp(0.0, 1.0));
        let next = (
            shown.0 + (target.0 - shown.0) * step,
            shown.1 + (target.1 - shown.1) * step,
        );
        self.shown = Some(next);
        next
    }
}

/// Split a position in absolute anchored world metres into the big_space cell and
/// the local translation that stand for it, at standing height.
///
/// Pure, and split out of [`place_avatar`] for one reason: the local half is what
/// goes on the wire, and a version of this that handed the sim `wx`/`wz` instead
/// would look right, place the avatar correctly on the client that sent it, and
/// be a cell edge — ten kilometres — out for the sim and for everyone else.
#[must_use]
fn placement(wx: f64, wz: f64, y: f32) -> (GridCell, Vec3) {
    let grid = Grid::new(WORLD_CELL_EDGE, WORLD_SWITCH_THRESHOLD);
    grid.translation_to_grid(DVec3::new(wx, f64::from(y.max(0.05)), wz))
}

/// Is this fix further from the avatar than [`GPS_TELEPORT_M`]?
///
/// Both sides are absolute world metres in the anchored flat frame — cell edge
/// plus local translation for the avatar, [`lat_lng_to_world`] for the fix — so
/// they are directly comparable and a cell boundary between them is not a jump.
/// An avatar that cannot be read at all counts as far: there is nothing to write
/// a position into, and a teleport is the operation that creates the state the
/// write would need.
pub(crate) fn fix_is_far(
    anchor: (f64, f64, u32),
    fix: GpsFix,
    avatars: &Query<(&mut GridCell, &mut Transform), (With<Avatar>, Without<FreeCamera>)>,
) -> bool {
    let Ok((cell, tf)) = avatars.single() else {
        return true;
    };
    let edge = f64::from(WORLD_CELL_EDGE);
    let here = DVec3::new(
        (cell.x as f64).mul_add(edge, f64::from(tf.translation.x)),
        0.0,
        (cell.z as f64).mul_add(edge, f64::from(tf.translation.z)),
    );
    gps_gap_m(anchor, fix.lat, fix.lng, here) > GPS_TELEPORT_M
}

/// Ground distance, in metres, between a `[lat, lng]` and a point already in the
/// anchored world frame. Split out of [`fix_is_far`] so the units — the one thing
/// that can silently be wrong about a threshold — are testable without a `World`.
fn gps_gap_m(anchor: (f64, f64, u32), lat: f64, lng: f64, here: DVec3) -> f64 {
    let (fx, fz) = lat_lng_to_world(anchor, lat, lng);
    (here.x - fx).hypot(here.z - fz)
}

/// The camera input driver mode handles itself. Same readers
/// `free_camera::CameraInput` carries, for the same gestures — see
/// [`steady_driver_camera`] for why they cannot simply be left to that system.
#[derive(bevy::ecs::system::SystemParam)]
pub struct DriveInput<'w, 's> {
    mouse: Res<'w, ButtonInput<MouseButton>>,
    cursor: MessageReader<'w, 's, bevy::window::CursorMoved>,
    wheel: MessageReader<'w, 's, bevy::input::mouse::MouseWheel>,
    touch: Res<'w, crate::systems::touch::TouchGestures>,
}

/// Hold the chase rig while driving, and hand the camera back on the way out.
///
/// The rig is *held*, not merely set on entry: `free_camera::camera_controls`
/// reads `CameraState` fresh every frame, and driver mode's contract is that the
/// view does not wander. `drive_locked` is what tells that system to ignore the
/// wheel and the drag; everything else here is the pose it should hold — the
/// orientation's own distance and tilt, stretched by speed and closed onto the
/// junction ahead.
///
/// **The user's own camera input is handled here rather than by clearing
/// `drive_locked`.** Handing the gestures back to `free_camera` looks like the
/// smaller change and is not one: its Avatar arm clamps the third-person zoom to
/// `AVATAR_CAM_MAX`, a tenth of a navigation chase distance, so the first frame
/// of a peek would slam the camera onto the roof of the car. Driver mode owns the
/// rig, so it owns the rig's input; what arrives here is accumulated as offsets
/// ([`Peek`]) that expire.
pub fn steady_driver_camera(
    mut input: DriveInput,
    mut driver: ResMut<DriverState>,
    mut camera_state: ResMut<CameraState>,
    egui: EguiRef,
    time: Res<Time>,
    avatars: Query<(), With<Avatar>>,
) {
    if !driver.active {
        // Nothing here consumes them while the mode is off, and Bevy's messages
        // live two frames — an unread wheel notch would otherwise be delivered as
        // the first gesture of the *next* drive and peek the camera on entry.
        input.wheel.clear();
        input.cursor.clear();
        // Leaving: put back exactly what was taken, once.
        if let Some(save) = driver.camera_save.take() {
            camera_state.drive_locked = false;
            camera_state.drive_lookahead = Vec2::ZERO;
            camera_state.mode = save.mode;
            camera_state.distance = save.distance;
            camera_state.pitch = save.pitch;
            camera_state.azimuth = save.azimuth;
            camera_state.follow_rotate = save.follow_rotate;
            camera_state.first_person = save.first_person;
            crate::systems::free_camera::enter_mode(&mut camera_state, save.mode);
        }
        return;
    }
    // Driving needs something to chase. Without a local avatar (the anonymous
    // viewer has none) Follow does not exist, so neither does driver mode.
    if avatars.is_empty() {
        driver.exit();
        return;
    }
    if driver.camera_save.is_none() {
        driver.camera_save = Some(CameraSave {
            mode: camera_state.mode,
            distance: camera_state.distance,
            pitch: camera_state.pitch,
            azimuth: camera_state.azimuth,
            follow_rotate: camera_state.follow_rotate,
            first_person: camera_state.first_person,
        });
        crate::systems::free_camera::enter_mode(&mut camera_state, CameraMode::Avatar);
        camera_state.mode = CameraMode::Avatar;
    }
    camera_state.drive_locked = true;
    // `follow_rotate` is off because driver mode aims the rig itself, from the
    // course over ground — the built-in chase reads the avatar's *facing*, which
    // is a reflection of the camera azimuth and would hold whatever angle it was
    // handed (see the module note).
    camera_state.follow_rotate = false;

    let ease = 1.0 - (-RIG_EASE_RATE * time.delta().as_secs_f32()).exp();
    let gesture = read_gesture(&mut input, &egui, &mut driver.peek.drag);
    apply_gesture(&mut driver.peek, gesture, ease);

    let (base, base_pitch, lookahead) = driver.orientation.rig();
    let distance = base * rig_scale(driver.speed_ms, driver.progress) * driver.peek.zoom;
    let pitch = (base_pitch + driver.peek.pitch).clamp(0.12, 1.50);
    camera_state.distance += (distance - camera_state.distance) * ease;
    camera_state.pitch += (pitch - camera_state.pitch) * ease;
    // Aim down the road the vehicle is on, in world XZ. Along the **course**, not
    // along the camera's forward: the two agree in the heading-up views and do not
    // in north-up, where the camera holds north and aiming along it pushed the
    // look point north while you drove south — the road ahead then ran off the
    // bottom of the screen. Eased through the offset itself, so switching
    // orientation slides the aim point along with the tilt instead of snapping the
    // horizon.
    let aim =
        Vec2::new(driver.course.sin(), driver.course.cos()) * camera_state.distance * lookahead;
    let toward_aim = (aim - camera_state.drive_lookahead) * ease;
    camera_state.drive_lookahead += toward_aim;

    // Screen-up direction. The camera sits at `(sin az, ., cos az) * d` from the
    // avatar and looks back at it, so its forward on the ground is
    // `-(sin az, cos az)`: for the course to point up the screen that must equal
    // the course direction, and for north-up it must be −Z.
    let target = driver.peek.azimuth
        + match driver.orientation {
            MapOrientation::NorthUp => 0.0,
            _ => {
                let dir = Vec2::new(driver.course.sin(), driver.course.cos());
                f32::atan2(-dir.x, -dir.y)
            }
        };
    let mut delta = (target - camera_state.azimuth).rem_euclid(std::f32::consts::TAU);
    if delta > std::f32::consts::PI {
        delta -= std::f32::consts::TAU;
    }
    // Same shortest-arc ease (and the same settle snap) the follow-and-rotate
    // chase uses, so a turn glides and a straight road stops moving the camera at
    // all — a rig that creeps forever pins the floating-origin propagation.
    let a = 1.0 - (-8.0_f32 * time.delta().as_secs_f32()).exp();
    camera_state.azimuth += if delta.abs() < 1e-3 { delta } else { delta * a };
}

/// One frame of camera input, reduced to the three things a rig can be moved by.
///
/// Separated from the readers so the peek arithmetic below is a pure function
/// over it — the interesting behaviour (accumulating, clamping, expiring, easing
/// home) is then testable without a window, a mouse or a route.
#[derive(Clone, Copy, PartialEq, Debug)]
struct Gesture {
    /// Multiplier on the peeked zoom.
    zoom: f32,
    /// Radians to turn the view by.
    turn: f32,
    /// Radians to tilt it by.
    tilt: f32,
}

impl Default for Gesture {
    /// Nothing happened — and for the zoom that is `1.0`, not `0.0`.
    fn default() -> Self {
        Self {
            zoom: 1.0,
            turn: 0.0,
            tilt: 0.0,
        }
    }
}

impl Gesture {
    fn idle(self) -> bool {
        (self.zoom - 1.0).abs() < 1e-4 && self.turn == 0.0 && self.tilt == 0.0
    }
}

/// Collect this frame's camera input, and keep the drag anchor.
///
/// The drag is gated on egui having the pointer and the wheel is not, which is
/// `free_camera`'s own split and is not an oversight: `egui_wants_pointer_input()`
/// is only meaningful while a pointer button is *down* (see the client's known
/// issues), so it answers correctly for a drag and would answer `true` over the
/// bare map for a wheel notch, swallowing every zoom.
fn read_gesture(input: &mut DriveInput, egui: &EguiManager, drag: &mut Option<Vec2>) -> Gesture {
    let mut g = Gesture::default();
    for event in input.wheel.read() {
        if event.y != 0.0 {
            // By notch *sign*, not by magnitude: raw wheel deltas are ±1 on some
            // backends and ±120 on others.
            g.zoom *= if event.y > 0.0 { 1.0 / 1.15 } else { 1.15 };
        }
    }

    let ui_has_pointer = egui.ctx.egui_wants_pointer_input();
    if input.touch.pair() {
        // A pinch is the wheel's continuous twin — spreading the fingers pulls
        // the view in, which is `1/pinch` on a distance.
        g.zoom /= input.touch.pinch;
        g.turn -= input.touch.twist;
        g.tilt -= input.touch.two_drag.y * PEEK_DRAG_RATE;
    } else if input.touch.drag != Vec2::ZERO && !ui_has_pointer {
        g.turn -= input.touch.drag.x * PEEK_DRAG_RATE;
        g.tilt -= input.touch.drag.y * PEEK_DRAG_RATE;
    }

    if input.mouse.pressed(MouseButton::Left) && !ui_has_pointer {
        for event in input.cursor.read() {
            if let Some(last) = *drag {
                let d = event.position - last;
                g.turn -= d.x * PEEK_DRAG_RATE;
                g.tilt -= d.y * PEEK_DRAG_RATE;
            }
            *drag = Some(event.position);
        }
    } else {
        *drag = None;
        input.cursor.clear();
    }
    g
}

/// Fold a gesture into the peek offsets, or ease them home when none arrives.
///
/// Easing back rather than snapping, and easing the *offsets* rather than the
/// camera, is what makes a peek recoverable without the driver doing anything: the
/// rig underneath keeps tracking the course the whole time, so when the offsets
/// reach identity the view is already where driving wants it.
fn apply_gesture(peek: &mut Peek, g: Gesture, ease: f32) {
    if !g.idle() {
        peek.since = Some(Instant::now());
        peek.zoom = (peek.zoom * g.zoom).clamp(PEEK_ZOOM_MIN, PEEK_ZOOM_MAX);
        peek.azimuth += g.turn;
        peek.pitch += g.tilt;
        return;
    }
    if peek.held() {
        return;
    }
    peek.zoom += (1.0 - peek.zoom) * ease;
    peek.azimuth += (0.0 - peek.azimuth) * ease;
    peek.pitch += (0.0 - peek.pitch) * ease;
    // Snap the last sliver, so `since` can be dropped and the camera actually
    // settles instead of creeping toward the pose forever — the floating-origin
    // propagation is pinned by a rig that never stops moving.
    if (peek.zoom - 1.0).abs() < 1e-3 && peek.azimuth.abs() < 1e-3 && peek.pitch.abs() < 1e-3 {
        *peek = Peek::default();
    }
}

/// Where the marker lands on screen for a rig, as a fraction from the top of the
/// viewport (0.5 is dead centre, 1.0 the bottom edge).
///
/// This is the framing rule made checkable. "The marker sits in the lower third"
/// is the one thing about a navigation camera that must be true and the one thing
/// this project cannot screenshot on the machine it is built on — but it is
/// arithmetic, not a matter of taste, so it is a test rather than a look.
///
/// **It mirrors `free_camera`'s Follow arm and has to keep mirroring it**: camera
/// at `avatar + (sin az · d cos p, d sin p + EYE, cos az · d cos p)`, look target
/// at `avatar + EYE·Y + aim`. Solved in the plane containing both, which is legal
/// because the aim is horizontal and the camera offset has no component across it
/// once the azimuth is the course. Change the placement over there and this goes
/// stale silently — the test would keep passing against the old geometry.
#[cfg(test)]
fn marker_screen_y(distance: f32, pitch: f32, aim_m: f32, fov_y: f32) -> f32 {
    /// The 1.5 m both the camera offset and the look target are raised by.
    const EYE: f32 = 1.5;
    // Camera and target in the (forward, up) plane, with the avatar at the origin
    // and "forward" the direction the aim points.
    let cam = Vec2::new(-distance * pitch.cos(), distance * pitch.sin() + EYE);
    let target = Vec2::new(aim_m, EYE);
    let dir = (target - cam).normalize();
    // Screen-up: perpendicular to the view direction, in the same plane.
    let up = Vec2::new(-dir.y, dir.x);
    let up = if up.y < 0.0 { -up } else { up };
    let to_marker = -cam; // the avatar's ground point
    let depth = to_marker.dot(dir);
    let offset = to_marker.dot(up);
    // Perspective divide, then NDC (−1 top, +1 bottom) to a fraction from the top.
    0.5 - 0.5 * (offset / depth) / (fov_y * 0.5).tan()
}

/// What to multiply the orientation's chase distance by: further back at speed,
/// closer in on the approach to a manoeuvre.
///
/// Pure over the two facts it reads so the curve can be checked at the speeds and
/// distances driving happens at, rather than by flying a route and squinting.
fn rig_scale(speed_ms: f32, progress: Option<Progress>) -> f32 {
    let kmh = speed_ms * 3.6;
    let stretch = 1.0 + RIG_SPEED_STRETCH * (kmh / RIG_SPEED_REF_KMH).clamp(0.0, 1.0);
    let tighten = match progress {
        Some(p) if p.turn_ahead => {
            let t = (p.step_distance_m as f32 / RIG_JUNCTION_M).clamp(0.0, 1.0);
            RIG_JUNCTION_TIGHTEN + (1.0 - RIG_JUNCTION_TIGHTEN) * t
        }
        _ => 1.0,
    };
    stretch * tighten
}

impl DriverState {
    /// The user is holding the camera away from the driving pose, so the chrome
    /// should offer to put it back.
    #[must_use]
    pub fn peeking(&self) -> bool {
        self.peek.held()
    }

    /// Give the rig back — the button, and what the timeout does on its own.
    pub fn recentre(&mut self) {
        self.peek.since = None;
    }

    /// Ask for a failed reroute to be tried again. An edge for `drive_driver_mode`
    /// to act on, so every path that touches `RouteState` stays in one system.
    pub fn retry_route(&mut self) {
        self.retry_requested = true;
    }
}

// ─── The arrow ───────────────────────────────────────────────────────────────

/// Material for the arrow, built once (mirrors `navigation::RouteMaterials`).
#[derive(Resource)]
pub struct ArrowMaterials {
    fill: Handle<StandardMaterial>,
}

/// Swap the avatar for a heading arrow while driving, and put it back after.
///
/// The arrow is a sibling of the avatar rather than a child of it: the avatar is
/// hidden, and a child would inherit that and vanish too.
///
/// **The avatar's own entity is not enough to hide it.** It is spawned with only
/// `Avatar` + `Transform` (`main::spawn_avatar_entity`) — no `Visibility` — while
/// each model holder under it *is* spawned with one. A holder whose parent carries
/// no visibility components is its own propagation root, so `Hidden` on the parent
/// changed nothing and the character kept standing inside the arrow. Both levels
/// are written, which is correct whichever way the hierarchy is built.
#[allow(clippy::too_many_arguments)]
pub fn update_driver_arrow(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    driver: Res<DriverState>,
    palette: Option<Res<ArrowMaterials>>,
    bigspace: Query<Entity, With<BigSpace>>,
    avatars: Query<(Entity, &GridCell, &Transform), With<Avatar>>,
    children: Query<&Children>,
    visibility: Query<&Visibility>,
    mut arrows: Query<
        (Entity, &mut GridCell, &mut Transform),
        (With<DriverArrow>, Without<Avatar>),
    >,
    mut hidden: Local<bool>,
) {
    // Reconciled every frame while driving, not once on the edge: the model holders
    // are spawned asynchronously (`avatar::spawn_avatar` waits on the glTF), so an
    // edge-triggered hide would miss any that arrive later — and an appearance
    // change respawns them mid-drive. Compare before writing, so a steady state
    // costs nothing and doesn't re-trigger visibility propagation every frame.
    //
    // Direct children only, and deliberately: deeper inside the glTF, individual
    // nodes carry a permanent `Hidden` from the slot mask (`avatar.rs` — that is
    // how a mixed head/body/legs avatar shows one of each), and restoring those to
    // `Inherited` would un-mask them.
    let set_visibility = |commands: &mut Commands, v: Visibility| {
        for (avatar, _, _) in avatars.iter() {
            let mut write = |e: Entity| {
                if visibility.get(e) != Ok(&v) {
                    commands.entity(e).insert(v);
                }
            };
            write(avatar);
            for holder in children.get(avatar).into_iter().flatten() {
                write(*holder);
            }
        }
    };

    if !driver.active {
        if *hidden {
            *hidden = false;
            set_visibility(&mut commands, Visibility::Inherited);
            for (e, _, _) in arrows.iter() {
                commands.entity(e).despawn();
            }
        }
        return;
    }
    let Ok((_, cell, tf)) = avatars.single() else {
        return;
    };
    *hidden = true;
    set_visibility(&mut commands, Visibility::Hidden);

    let fill = match &palette {
        Some(p) => p.fill.clone(),
        None => {
            // Unlit for the same reason the route line is: this is a UI affordance
            // drawn into the scene, and it has to stay legible at night. Its place
            // above the route line — which is itself above the ground stack
            // (`navigation::ROUTE_Y` explains that fight in full) — is the top of
            // `ground_stack::GroundLayer`, not a number chosen here.
            let fill = materials.add(StandardMaterial {
                base_color: Color::srgb(0.20, 0.60, 0.98),
                unlit: true,
                depth_bias: GroundLayer::DriverArrow.depth_bias(),
                cull_mode: None,
                ..default()
            });
            commands.insert_resource(ArrowMaterials { fill: fill.clone() });
            fill
        }
    };

    // Drawn on the road when the fix is close enough to it to justify saying so —
    // `drive_driver_mode` decides that and hands over an offset in world XZ metres
    // (see `SNAP_M`). An offset rather than an absolute position because the
    // avatar's transform is cell-relative under `big_space`, and a snap is metres:
    // adding it here cannot cross a cell boundary the avatar has not crossed.
    let (offset, yaw) = match driver.snap {
        Some((offset, yaw)) => (Vec3::new(offset.x, 0.0, offset.y), yaw),
        None => (Vec3::ZERO, driver.course),
    };
    let rotation = Quat::from_rotation_y(yaw);
    let translation = tf.translation + offset + Vec3::Y * ARROW_Y;
    match arrows.single_mut() {
        Ok((_, mut acell, mut atf)) => {
            *acell = *cell;
            atf.translation = translation;
            atf.rotation = rotation;
        }
        Err(_) => {
            let Ok(root) = bigspace.single() else {
                return;
            };
            commands.spawn((
                Mesh3d(meshes.add(arrow_mesh())),
                MeshMaterial3d(fill),
                Transform {
                    translation,
                    rotation,
                    ..default()
                },
                *cell,
                ChildOf(root),
                bevy::camera::visibility::NoFrustumCulling,
                // Unculled means submitted to every cascade of every light, so
                // this one flat kite would otherwise be re-rasterised into up to
                // eight shadow maps while driving. It is a navigation mark, not a
                // thing in the world; nothing should be darker under it.
                bevy::light::NotShadowCaster,
                DriverArrow,
                crate::components::HideInSpace,
            ));
        }
    }
}

/// A flat navigation kite in the XZ plane, pointing along +Z — the avatar's own
/// forward, so the arrow can take a yaw straight from the course.
///
/// The same silhouette as the locate button's icon, which is the point: the
/// button and the thing it centres on read as one object.
fn arrow_mesh() -> Mesh {
    let (l, w) = (ARROW_HALF_LENGTH, ARROW_HALF_WIDTH);
    // Nose, left tail, the notch between them, right tail.
    let points = [
        Vec2::new(0.0, l),
        Vec2::new(-w, -0.72 * l),
        Vec2::new(0.0, -0.30 * l),
        Vec2::new(w, -0.72 * l),
    ];
    let mut positions: Vec<[f32; 3]> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();
    // Two triangles about the notch, each wound to face +Y or it is backface-culled
    // into invisibility — the trap `navigation::ribbon_faces_up` guards for the
    // route line.
    for (a, b, c) in [
        (points[0], points[1], points[2]),
        (points[0], points[2], points[3]),
    ] {
        let up = (b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x) < 0.0;
        let (b, c) = if up { (b, c) } else { (c, b) };
        let base = positions.len() as u32;
        for p in [a, b, c] {
            positions.push([p.x, 0.0, p.y]);
        }
        indices.extend([base, base + 1, base + 2]);
    }
    Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    )
    .with_inserted_attribute(
        Mesh::ATTRIBUTE_NORMAL,
        vec![[0.0, 1.0, 0.0]; positions.len()],
    )
    .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions)
    .with_inserted_indices(Indices::U32(indices))
}

// ─── The navigation chrome ───────────────────────────────────────────────────

/// Width of the manoeuvre card, and the vertical space it claims. Fixed rather
/// than sized to content so the compass beneath it has a place to be — the same
/// reason the status HUD pins its width (`hud::HUD_WIDTH`).
pub const CARD_WIDTH: f32 = 190.0;
/// …including the control row along its bottom, which is why this is 146 and not
/// the 118 of the instruction alone. `hud` stacks the compass from it, so the
/// number and the card cannot drift apart.
pub const CARD_HEIGHT: f32 = 146.0;
/// Where the card sits, and therefore where everything under it stacks from.
pub const CARD_MARGIN: f32 = 12.0;

/// Width of the trip strip down the right-hand edge. Wide enough for the route
/// rail's badges to sit on the track without touching its edges.
const STRIP_WIDTH: f32 = 116.0;

/// Height of the route rail's track. The one dimension the widget's usefulness
/// scales with — everything on the route between here and the destination has to
/// find a distinct pixel on it.
///
/// Shorter on a phone, where the strip is stacked *under* the card rather than
/// beside it (see [`render_driver_hud`]) and the two share one column of screen.
const RAIL_HEIGHT: f32 = 236.0;
const RAIL_HEIGHT_COMPACT: f32 = 132.0;

/// How far the GPS chip clears the bottom edge.
///
/// Above the speed pill's band on purpose: the Re-centre button is anchored to the
/// bottom *right* at [`PILL_BOTTOM`], and a chip on the bottom left at the same
/// height shared a y-band with it — which on a 274 pt viewport is not a near miss
/// but an overlap, the chip being the wider of the two.
const CHIP_BOTTOM: f32 = PILL_BOTTOM + 36.0;

/// Where the two anchored boxes go, for a viewport width.
struct DriverLayout {
    card_width: f32,
    strip_width: f32,
    /// The strip's offset from the top, which on a narrow viewport is *below* the
    /// card rather than beside it.
    strip_top: f32,
    rail_height: f32,
}

/// Fit the chrome to the screen it is on.
///
/// **Nothing in this module read the viewport at all**, which is how a layout that
/// is unusable on the target device survived: every box is anchored to a corner,
/// and on a desktop the corners are far enough apart that no arithmetic is needed.
/// At the 274 pt an Android phone reports, the 190 pt card and the 116 pt strip
/// each fit and cannot both fit — anchored to opposite edges they overlapped by
/// 56 pt, one drawing over the other's numbers.
///
/// So on a compact viewport the strip stacks under the card instead of beside it.
/// Stacking down the *right* edge specifically: the column under the card on the
/// left belongs to the compass and the locate button (`hud::render_map_controls`
/// stacks them from [`CARD_HEIGHT`]). The rail loses height to pay for it, because
/// the strip now shares one screen's height with the card rather than owning its
/// own edge.
///
/// A pure function because "does the chrome fit" is arithmetic, and this is another
/// case of a thing that cannot be checked by looking on the machine it is built on.
fn driver_layout(viewport_x: f32, compact: bool) -> DriverLayout {
    let usable = (viewport_x - 2.0 * CARD_MARGIN).max(80.0);
    DriverLayout {
        card_width: CARD_WIDTH.min(usable),
        strip_width: STRIP_WIDTH.min(usable),
        strip_top: if compact {
            CARD_MARGIN + CARD_HEIGHT + 8.0
        } else {
            CARD_MARGIN
        },
        rail_height: if compact {
            RAIL_HEIGHT_COMPACT
        } else {
            RAIL_HEIGHT
        },
    }
}

/// Half-width of the track, and the radius of a marker badge sitting on it.
const RAIL_HALF_W: f32 = 4.0;
const RAIL_BADGE_R: f32 = 9.5;

/// Minimum vertical gap between two badges. Below this they overlap into an
/// unreadable stack, so the rail spreads them apart (and drops what will not fit)
/// rather than drawing a smear that is technically in proportion.
const RAIL_MIN_GAP: f32 = 21.0;

/// The rail's near field: the first [`RAIL_NEAR_SPAN_M`] of route get
/// [`RAIL_NEAR_FRACTION`] of the track.
///
/// A purely linear rail is the obvious design and it is the wrong one. On a 40 km
/// route the next three things you pass all land inside the bottom 2 % of the
/// track — one blurred clump — while the empty motorway stretch nobody needs to
/// read gets nine tenths of the pixels. Both reference layouts solve it the same
/// way, by giving what is about to happen a disproportionate share and compressing
/// the rest, so the rail stays readable at the only end where the answer changes
/// minute to minute. The break is *drawn* (a notch across the track) rather than
/// left implicit: a scale that changes silently halfway up is a lie about
/// distance, and a driver reads distance off this.
const RAIL_NEAR_SPAN_M: f64 = 2_000.0;
const RAIL_NEAR_FRACTION: f32 = 0.45;

/// How close the manoeuvre after this one has to be before the card previews it.
/// Two turns in quick succession is the case a card showing only the next one gets
/// wrong — you take the first and are already in the second before it appears.
const THEN_PREVIEW_M: f64 = 300.0;

/// Tolerance before the speedometer calls you over the limit, in km/h. Speed is
/// measured from successive positions and the limit comes off a map; flashing red
/// at 51 in a 50 on that evidence would be noise.
const OVER_LIMIT_TOLERANCE_KMH: f32 = 4.0;

/// How far the speed pill clears the bottom edge — over the chat/action bar,
/// which is docked there and would otherwise sit on top of it.
const PILL_BOTTOM: f32 = 62.0;

/// The navigation chrome, laid out the way a satnav lays it out: the next
/// manoeuvre on a card at the top left, the trip — arrival clock, time left,
/// distance left — as a compact strip down the right edge, and the current speed
/// in a pill at the bottom centre. The compass is drawn by
/// [`crate::systems::hud`], which already owns it; driver mode only moves it down
/// past the card.
///
/// **Every box here pins its own width.** An anchored, size-to-content
/// [`egui::Area`] whose content reaches for the *available* width grows a little
/// each frame — this frame's size becomes next frame's available width — and
/// stretches across the screen. `hud`'s module note documents the same trap; the
/// first cut of this bar hit it and swallowed the lower third of the window.
///
/// **The trip strip yields to a right-hand panel.** It is anchored to the right
/// edge, which is exactly where the docked panels are, and egui paints floating
/// `Area`s *above* panels — so the rail drew straight over the open Route panel
/// and its turn list. It cannot be put behind one (there is no z-order between an
/// `Area` and a panel to lose), so it stands down while one is open, the same
/// resolution `hud::render_map_controls` reached for the status box. The manoeuvre
/// card, the compass and the speed pill are all clear of that edge and stay.
pub fn render_driver_hud(
    egui: EguiRef,
    mut driver: ResMut<DriverState>,
    route: Res<RouteState>,
    ahead: Res<RouteAhead>,
    clock: Res<crate::systems::transit_live::WorldClock>,
    mut right_panel: ResMut<crate::resources::RightPanel>,
    data_layers: Res<crate::systems::data_layers::DataLayers>,
    bookmarks: Res<crate::systems::bookmarks::BookmarksState>,
) {
    if !driver.active {
        return;
    }
    // Every number on this screen — the distance to the next turn, the ETA, the
    // speed, the progress edge — is a function of where the vehicle is, and driving
    // produces no egui input at all. Without this the HUD is tessellated once and
    // then re-presented unchanged until something is tapped, i.e. it freezes at the
    // moment the driver stops touching the phone. See `EguiManager::animating`.
    egui.animating();
    // The route being *driven*, which during a reroute is the one being replaced.
    let Some(r) = route.displayed() else {
        return;
    };
    let progress = driver.progress.unwrap_or_default();
    let arrived = driver.arrived;
    let off_route = driver.off_route();
    let rerouting = driver.rerouting;
    let failed = driver.solve_error.clone();
    let step = r.steps.get(progress.step);
    // Phone layout. At 274 pt — the viewport an Android phone actually reports —
    // the card (190) and the strip (116) both fit the width and cannot both fit it
    // at once: right- and left-anchored, they overlapped by 56 pt, one drawing over
    // the other. Nothing here read the viewport at all, which is how that survived:
    // every box is anchored to a corner, and on a desktop the corners are far apart.
    let viewport_x = egui.viewport_pts().x;
    let DriverLayout {
        card_width,
        strip_width,
        strip_top,
        rail_height,
    } = driver_layout(viewport_x, egui.compact());
    // What any full-width box may claim, the chip included.
    let usable = (viewport_x - 2.0 * CARD_MARGIN).max(80.0);
    // Escape used to leave from here. It is `systems::dismiss`'s rung 8 now, for
    // the reason that module exists: this was the *only* Escape handler in the
    // client that actually worked, so it stepped past a menu or a dialog drawn
    // over the trip and ended the trip instead. Stop is still on the manoeuvre
    // card, which is always drawn — a touch device has no Escape key, and that
    // was already the real exit.
    let ctx = egui.ctx().clone();
    let panel = |fill: egui::Color32| {
        egui::Frame::NONE
            .fill(fill)
            .corner_radius(8.0)
            .inner_margin(egui::Margin::symmetric(10, 8))
    };

    // ── The manoeuvre card ──
    //
    // **Everything that must always be reachable lives on this box**, because this
    // box is the only one that is always drawn. The trip strip stands down whenever
    // a right-hand panel is open, and it used to carry Stop — so opening the Places
    // list on a phone, which has no Escape key to fall back on, left a drive with
    // no exit at all.
    let mut done = false;
    let mut retry = false;
    let mut open_planner = false;
    egui::Area::new(egui::Id::new("driver_maneuver"))
        .anchor(egui::Align2::LEFT_TOP, egui::vec2(CARD_MARGIN, CARD_MARGIN))
        .show(&ctx, |ui| {
            // Amber, not red, while the route is being replaced: leaving the line
            // is the normal case (you missed a turn), and the display is already
            // fixing it. Red is reserved for the state it cannot fix — which now
            // includes a reroute that came back empty. Green is the trip being
            // over — the one card that is not an instruction.
            panel(if arrived {
                ARRIVE_GREEN
            } else if rerouting {
                REROUTE_AMBER
            } else if off_route || failed.is_some() {
                theme::ERROR
            } else {
                DRIVE_BLUE
            })
            .show(ui, |ui| {
                ui.set_width(card_width - 20.0);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    if arrived {
                        // The card the whole drive was for. It names the place
                        // the user asked for rather than the road the route
                        // ended on — "Arrived" over a street name reads as one
                        // more instruction, and this is the opposite of one.
                        ui.label(big("Arrived", 21.0, theme::CREAM));
                        let where_to = route.to.label.trim();
                        if !where_to.is_empty() {
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(where_to).size(12.5).color(theme::CREAM),
                                )
                                .truncate(),
                            );
                        }
                    } else if let Some(err) = &failed {
                        // The state that used to end the drive without saying
                        // anything: `route` went to `None`, the tick dropped out of
                        // driver mode, and the explanation was in a panel nobody
                        // could see. The route being replaced is still drawn under
                        // this card, so "keep going on the old line" is a real
                        // option beside Retry and Stop.
                        ui.label(big("No new route", 18.0, theme::INK));
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(err.as_str())
                                    .size(11.5)
                                    .color(theme::INK),
                            )
                            .truncate(),
                        );
                        ui.label(small("still following the previous route", theme::INK));
                    } else if rerouting {
                        ui.label(big("Rerouting…", 19.0, theme::INK));
                        ui.label(small("from your current position", theme::INK));
                    } else if off_route {
                        // Reached only when a reroute cannot be asked for yet — the
                        // cooldown, or a fix too poor to act on.
                        ui.label(big("Off route", 20.0, theme::INK));
                        ui.label(small(
                            &format!("{} from the route", format_distance(progress.off_route_m)),
                            theme::INK,
                        ));
                    } else {
                        // Glyph and distance on one line, street name under it —
                        // the satnav arrangement, and it keeps the card short
                        // enough to leave the compass its place below.
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 8.0;
                            let maneuver = step.map_or(Maneuver::Continue, |s| s.maneuver);
                            ui.label(big(icons::maneuver(maneuver), 34.0, theme::CREAM));
                            ui.label(big(
                                &countdown(progress.step_distance_m),
                                24.0,
                                theme::CREAM,
                            ));
                        });
                        // Street name, and — when the manoeuvre after this one
                        // follows immediately — a preview of it on the same row.
                        // Right-aligned so it reads as a footnote to the turn
                        // rather than as a competing instruction, and inside the
                        // card so the compass below keeps its place.
                        ui.horizontal(|ui| {
                            if let Some(name) = step.and_then(|s| s.name.as_deref()) {
                                ui.label(egui::RichText::new(name).size(12.5).color(theme::CREAM));
                            }
                            let Some(next) = r.steps.get(progress.step + 1) else {
                                return;
                            };
                            if next_step_gap(r, progress) > THEN_PREVIEW_M {
                                return;
                            }
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    ui.spacing_mut().item_spacing.x = 3.0;
                                    ui.label(
                                        egui::RichText::new(icons::maneuver(next.maneuver))
                                            .size(16.0)
                                            .color(theme::CREAM),
                                    );
                                    ui.label(
                                        egui::RichText::new("then")
                                            .size(11.0)
                                            .color(theme::CREAM.gamma_multiply(0.75)),
                                    );
                                },
                            );
                        });
                        approach_bar(ui, progress.step_distance_m);
                    }

                    // ── The controls that are always there ──
                    //
                    // Stop, because a touch device has no Escape and the strip that
                    // used to carry it can be covered. Route, because mid-drive
                    // every other way back is suppressed — the dock, the main menu
                    // and the planner itself — so there was no way to change the
                    // destination, read the turn list or switch travel mode without
                    // first ending the trip.
                    ui.add_space(5.0);
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 5.0;
                        if ui
                            .add(egui::Button::new(
                                egui::RichText::new(if arrived { "Done" } else { "Stop" })
                                    .size(12.0),
                            ))
                            .on_hover_text("Leave driver mode and put the map back")
                            .clicked()
                        {
                            done = true;
                        }
                        if failed.is_some()
                            && ui
                                .add(egui::Button::new(egui::RichText::new("Retry").size(12.0)))
                                .on_hover_text("Ask for a route from here again")
                                .clicked()
                        {
                            retry = true;
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui
                                .add(egui::Button::new(egui::RichText::new("Route").size(12.0)))
                                .on_hover_text("Open the planner — destination, turn list, mode")
                                .clicked()
                            {
                                open_planner = true;
                            }
                        });
                    });
                });
            });
        });

    // ── What the position is worth, bottom left ──
    //
    // The provider's own message when there is one, otherwise the quality of the
    // fix itself (`GpsQuality`). Before this, "the watch has started and nothing has
    // arrived", "±50 m in a tunnel" and a metre-accurate lock all rendered as the
    // same confident arrow on the same confident card.
    let chip = driver
        .gps_error
        .as_ref()
        .map(|err| format!("{err} — following the avatar"))
        .or_else(|| driver.gps.chip());
    if let Some(chip) = chip {
        egui::Area::new(egui::Id::new("driver_gps_chip"))
            .anchor(
                egui::Align2::LEFT_BOTTOM,
                egui::vec2(CARD_MARGIN, -CHIP_BOTTOM),
            )
            .show(&ctx, |ui| {
                panel(theme::OVERLAY).show(ui, |ui| {
                    ui.set_width(260.0_f32.min(usable) - 20.0);
                    ui.add(
                        egui::Label::new(egui::RichText::new(chip).size(11.0).color(theme::WARN))
                            .truncate(),
                    );
                });
            });
    }

    // ── The trip strip, right edge — unless a panel is docked there ──
    //
    // `RightPanel` is the shared state; the two booleans are the older panels
    // that only publish their own (`hud` ORs the same three for the same reason).
    let right_panel_open = data_layers.panel_open
        || bookmarks.panel_open
        || !right_panel.is(crate::resources::RightPanel::None);

    let ((hh, mm, _), local_clock) = clock.hms();
    let arrival = arrival_clock(hh, mm, progress.remaining_s, local_clock);
    // Not an early `return`: the speed pill and its limit sign are drawn below and
    // sit at the bottom centre, nowhere near the panel.
    if !right_panel_open {
        egui::Area::new(egui::Id::new("driver_strip"))
            .anchor(egui::Align2::RIGHT_TOP, egui::vec2(-CARD_MARGIN, strip_top))
            .show(&ctx, |ui| {
                panel(theme::OVERLAY).show(ui, |ui| {
                    ui.set_width(strip_width - 20.0);
                    ui.vertical_centered(|ui| {
                        ui.spacing_mut().item_spacing.y = 1.0;
                        // Arrival first: on a drive of any length it is the number
                        // being planned around, which is why a satnav puts it top.
                        // A pace smaller when it carries the " UTC" suffix, which
                        // otherwise wraps onto a second line in a 96 pt column and
                        // pushes the rail down.
                        ui.label(big(
                            &arrival,
                            if local_clock { 24.0 } else { 18.0 },
                            theme::CREAM,
                        ));
                        if arrived {
                            // The same clock, now a fact rather than a forecast.
                            // What is left is nothing, and `format_duration`
                            // floors at "1 min" — a trip that is over must not
                            // still claim a minute of it.
                            ui.label(small("arrived", theme::TEXT_DIM));
                            ui.add_space(6.0);
                        } else {
                            ui.label(small("arrival", theme::TEXT_DIM));
                            ui.add_space(6.0);
                            ui.label(big(
                                &format_distance(progress.remaining_m),
                                21.0,
                                theme::TEXT,
                            ));
                            ui.label(small(
                                &format_duration(progress.remaining_s),
                                theme::TEXT_DIM,
                            ));
                        }
                        ui.add_space(8.0);
                        route_rail(ui, &ahead, progress, rail_height);
                        // Stop used to live here, and here is exactly where it
                        // could not be relied on — this whole strip stands down
                        // behind a docked panel. It is on the manoeuvre card now.
                    });
                });
            });
    }

    // ── The speed pill, bottom centre, with the limit beside it ──
    let speed_kmh = driver.speed_ms * 3.6;
    let limit = ahead.limit_at(progress.along_m);
    let over = limit.is_some_and(|l| speed_kmh > f32::from(l) + OVER_LIMIT_TOLERANCE_KMH);
    egui::Area::new(egui::Id::new("driver_speed"))
        .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -PILL_BOTTOM))
        .show(&ctx, |ui| {
            egui::Frame::NONE
                .fill(theme::OVERLAY)
                .corner_radius(18.0)
                .inner_margin(egui::Margin::symmetric(16, 6))
                .show(ui, |ui| {
                    ui.set_width(if limit.is_some() { 124.0 } else { 84.0 });
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 5.0;
                        // Over the limit turns the *speed* red, not the sign: the
                        // sign states a fact about the road and must read the same
                        // whatever the vehicle is doing.
                        ui.label(big(
                            &format!("{speed_kmh:.0}"),
                            22.0,
                            if over { theme::ERROR } else { theme::CREAM },
                        ));
                        ui.label(small("km/h", theme::TEXT_DIM));
                        if let Some(limit) = limit {
                            ui.add_space(4.0);
                            speed_limit_sign(ui, limit);
                        }
                    });
                });
        });

    // ── Re-centre, bottom right, only while the view is being held off the rig ──
    //
    // It is an offer, not a requirement — the peek expires on its own
    // ([`PEEK_TIMEOUT`]) — but a locked camera that has visibly stopped tracking
    // needs to say, in the frame it stops, that this is recoverable and how.
    // Bottom *right*: the left is the manoeuvre card's column and the centre is
    // the speed pill, and this must not move either of them while it comes and
    // goes.
    let mut recentre = false;
    if driver.peeking() {
        egui::Area::new(egui::Id::new("driver_recentre"))
            .anchor(
                egui::Align2::RIGHT_BOTTOM,
                egui::vec2(-CARD_MARGIN, -PILL_BOTTOM),
            )
            .show(&ctx, |ui| {
                egui::Frame::NONE
                    .fill(theme::OVERLAY)
                    .corner_radius(16.0)
                    .inner_margin(egui::Margin::symmetric(12, 6))
                    .show(ui, |ui| {
                        if ui
                            .add(egui::Button::new(
                                egui::RichText::new("Re-centre").size(12.5).strong(),
                            ))
                            .on_hover_text("Put the camera back behind the arrow")
                            .clicked()
                        {
                            recentre = true;
                        }
                    });
            });
    }
    if recentre {
        driver.recentre();
    }
    if retry {
        driver.retry_route();
    }
    // Opening the planner does not leave driver mode: the drive carries on behind
    // it, the strip stands down as it does for any docked panel, and the card —
    // Stop included — stays put.
    if open_planner {
        *right_panel = crate::resources::RightPanel::Route;
    }

    // Arriving and stopping are the same act as far as the mode is concerned —
    // the route line stays drawn either way, because clearing it is the planner's
    // business and a driver who has just parked may well want to see it.
    if done {
        driver.exit();
    }
}

/// The satnav blue the manoeuvre card is painted in. Not a theme colour: the
/// palette is the accent's warm one, and a *direction* instruction is the one piece
/// of chrome that has to read as a road sign rather than as part of the app.
const DRIVE_BLUE: egui::Color32 = egui::Color32::from_rgb(0x14, 0x6E, 0xD2);

/// The two colours of a European speed-limit sign. Same argument as
/// [`DRIVE_BLUE`], only harder: this *is* a road sign, and a road sign recoloured
/// into a brand palette stops being recognisable at a glance, which is the entire
/// function it performs.
const SIGN_WHITE: egui::Color32 = egui::Color32::from_rgb(0xF7, 0xF7, 0xF4);
const SIGN_RED: egui::Color32 = egui::Color32::from_rgb(0xD0, 0x21, 0x1C);

/// The card's colour once the trip is over. Same argument as [`DRIVE_BLUE`]: the
/// card is read as signage, and "you are there" is green everywhere signage
/// exists. Dark enough to carry `theme::CREAM` at the contrast the theme tests
/// hold the rest of the UI to.
const ARRIVE_GREEN: egui::Color32 = egui::Color32::from_rgb(0x1B, 0x7A, 0x48);

/// The card's colour while the route is being replaced. Signage again, and here
/// for a second reason: this was `theme::AMBER`, the palette's second accent,
/// which the monochrome chrome retired. `theme::WARN` could not take it over —
/// that is a *type* colour, pale enough to read on a dark panel, and this is a
/// card fill that has to carry `theme::CREAM`. So it is the amber a road sign
/// would use, dark enough to be a plate.
const REROUTE_AMBER: egui::Color32 = egui::Color32::from_rgb(0x8A, 0x5A, 0x0C);

/// The distance to the next manoeuvre, as a countdown rather than a measurement.
///
/// `format_distance` rounds to 10 m at every scale, which is right for a fact
/// being reported and wrong for a number being watched: on a kilometre of
/// motorway it repaints every few metres, so the one part of the card that should
/// only change when something has changed is the part that never stops. The ladder
/// coarsens with distance — the last hundred metres in tens, the rest of a
/// kilometre in fifties — and stops naming a distance at all inside [`NOW_M`],
/// where "10 m" describes a turn you are already taking.
pub(crate) fn countdown(m: f64) -> String {
    if m <= NOW_M {
        return "Now".to_string();
    }
    // Rounded first, *then* formatted: rounding 980 m up to the nearest 50 gives
    // 1000, and "1000 m" is not a thing anyone writes.
    let m = if m < 100.0 {
        (m / 10.0).round() * 10.0
    } else if m < 1000.0 {
        (m / 50.0).round() * 50.0
    } else {
        m
    };
    if m < 1000.0 {
        format!("{m:.0} m")
    } else if m < 10_000.0 {
        format!("{:.1} km", m / 1000.0)
    } else {
        format!("{:.0} km", m / 1000.0)
    }
}

/// The card's bottom edge, filling over the last [`APPROACH_M`] to the manoeuvre.
///
/// The countdown above it is exact and has to be read; this is the same fact in a
/// form that can be taken in without looking away from the road, which is the
/// whole reason a navigation display is not a table of numbers. Drawn as the edge
/// of the card rather than as a widget inside it so it costs no layout and cannot
/// push the compass below out of place.
fn approach_bar(ui: &mut egui::Ui, step_distance_m: f64) {
    let filled = 1.0 - (step_distance_m / APPROACH_M).clamp(0.0, 1.0) as f32;
    ui.add_space(4.0);
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 4.0), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 2.0, theme::INK.gamma_multiply(0.35));
    if filled > 0.0 {
        let mut fill = rect;
        fill.set_width(rect.width() * filled);
        painter.rect_filled(fill, 2.0, theme::CREAM);
    }
}

/// Metres between the manoeuvre being driven toward and the one after it.
///
/// Off the steps' own `distance_m` rather than the measured polyline: a step's
/// distance is the length of the leg *leading to* it, which is exactly this gap,
/// and it needs no geometry — so the card's preview keeps working on the frames
/// before a position has been projected.
fn next_step_gap(route: &Route, progress: Progress) -> f64 {
    route
        .steps
        .get(progress.step + 1)
        .map_or(f64::INFINITY, |s| s.distance_m)
}

/// The route rail: the drive from here to the destination as one vertical track,
/// with what you will pass marked along it in proportion.
///
/// Bottom is the vehicle, top is the destination, and the track carries only what
/// is *left* — a rail that also showed the road already driven would spend pixels
/// on the one part of the route that can no longer change. The scale is the
/// two-zone one [`RAIL_NEAR_SPAN_M`] documents.
///
/// Badges are laid out bottom-up and pushed apart to [`RAIL_MIN_GAP`], so a clump
/// of car parks in one street becomes a legible column rather than one smudge;
/// anything pushed past the destination cap is dropped and counted, because a
/// badge drawn over the flag would claim to be the end of the route.
fn route_rail(ui: &mut egui::Ui, ahead: &RouteAhead, progress: Progress, height: f32) {
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    let x = rect.center().x;
    let top = rect.top() + RAIL_BADGE_R;
    let bottom = rect.bottom() - RAIL_BADGE_R;
    let span = bottom - top;

    // The track. Drawn in the route's own blue so the rail and the line on the
    // map are visibly the same object.
    painter.rect_filled(
        egui::Rect::from_min_max(
            egui::pos2(x - RAIL_HALF_W, top),
            egui::pos2(x + RAIL_HALF_W, bottom),
        ),
        RAIL_HALF_W,
        DRIVE_BLUE,
    );

    let remaining = progress.remaining_m.max(1.0);
    let y_of = |d: f64| bottom - span * rail_t(d, remaining);

    // The scale break, when there is one — see `RAIL_NEAR_SPAN_M`.
    if remaining > RAIL_NEAR_SPAN_M {
        let y = y_of(RAIL_NEAR_SPAN_M);
        painter.line_segment(
            [
                egui::pos2(x - RAIL_HALF_W - 3.0, y),
                egui::pos2(x + RAIL_HALF_W + 3.0, y),
            ],
            egui::Stroke::new(1.0, theme::OVERLAY_LINE),
        );
    }

    // Markers, bottom-up, spread apart. `along_m` is measured from the start of
    // the route; the rail measures from the vehicle.
    let base = progress.along_m;
    let mut placed: Vec<(f32, &crate::systems::route_ahead::AheadItem)> = Vec::new();
    let mut last_y = f32::INFINITY;
    let mut dropped = 0usize;
    for item in ahead.ahead_of(base) {
        if item.kind == AheadKind::Destination {
            continue; // drawn as the cap, below
        }
        let y = y_of(item.along_m - base).min(last_y - RAIL_MIN_GAP);
        if y < top + RAIL_MIN_GAP {
            dropped += 1;
            continue;
        }
        last_y = y;
        placed.push((y, item));
    }

    for (y, item) in &placed {
        let c = egui::pos2(x, *y);
        painter.circle_filled(c, RAIL_BADGE_R, theme::SURFACE);
        painter.circle_stroke(c, RAIL_BADGE_R, egui::Stroke::new(1.0, theme::OVERLAY_LINE));
        painter.text(
            c,
            egui::Align2::CENTER_CENTER,
            item.kind.icon(),
            egui::FontId::proportional(11.0),
            theme::TEXT,
        );
        // A badge is 19 px across and the name never fits, so the name is a
        // tooltip. `interact` rather than a widget: the rail is one allocated
        // rect, and hover regions inside it are hit-tested by hand.
        let hit = egui::Rect::from_center_size(c, egui::Vec2::splat(RAIL_BADGE_R * 2.0));
        ui.interact(
            hit,
            ui.id().with(("rail", item.along_m.to_bits())),
            egui::Sense::hover(),
        )
        .on_hover_text(format!(
            "{}\n{} ahead · {} off the route",
            item.title(),
            format_distance(item.along_m - base),
            format_distance(item.off_m),
        ));
    }

    // The destination cap, and the vehicle at the foot.
    painter.circle_filled(egui::pos2(x, top), RAIL_BADGE_R + 1.0, theme::ACCENT);
    painter.text(
        egui::pos2(x, top),
        egui::Align2::CENTER_CENTER,
        AheadKind::Destination.icon(),
        egui::FontId::proportional(11.0),
        theme::INK,
    );
    painter.add(egui::Shape::convex_polygon(
        vec![
            egui::pos2(x, bottom - 7.0),
            egui::pos2(x - 6.0, bottom + 4.0),
            egui::pos2(x + 6.0, bottom + 4.0),
        ],
        theme::CREAM,
        egui::Stroke::NONE,
    ));

    // How many would not fit, parked beside the destination cap — against the end
    // of the rail rather than against whichever badge happens to be topmost, which
    // read as a count of *that* thing.
    if dropped > 0 {
        painter.text(
            egui::pos2(x + RAIL_BADGE_R + 5.0, top),
            egui::Align2::LEFT_CENTER,
            format!("+{dropped}"),
            egui::FontId::proportional(10.0),
            theme::TEXT_DIM,
        );
    }

    // What is next, spelled out under the rail — the rail says where things are,
    // this says what the nearest one is, which is the question actually being
    // asked while driving.
    match ahead.next_ahead(base) {
        Some(next) => {
            ui.label(big(
                &format_distance(next.along_m - base),
                15.0,
                theme::CREAM,
            ));
            // Truncated, not wrapped: a long POI name is three lines in a 96 px
            // column, and the strip is a fixed-height box with a button under it —
            // the wrap pushed Stop off the bottom of the panel.
            ui.add(
                egui::Label::new(
                    egui::RichText::new(next.title())
                        .size(10.5)
                        .color(theme::TEXT_DIM),
                )
                .truncate(),
            );
        }
        None if ahead.status == AheadStatus::Fetching => {
            ui.label(small("scanning route…", theme::TEXT_DIM));
        }
        None => {}
    }
}

/// Where a distance ahead sits on the rail: 0 at the vehicle, 1 at the
/// destination. See [`RAIL_NEAR_SPAN_M`] for why this is not `d / remaining`.
fn rail_t(d: f64, remaining: f64) -> f32 {
    let d = d.clamp(0.0, remaining);
    if remaining <= RAIL_NEAR_SPAN_M {
        return (d / remaining) as f32;
    }
    if d <= RAIL_NEAR_SPAN_M {
        return RAIL_NEAR_FRACTION * (d / RAIL_NEAR_SPAN_M) as f32;
    }
    RAIL_NEAR_FRACTION
        + (1.0 - RAIL_NEAR_FRACTION)
            * ((d - RAIL_NEAR_SPAN_M) / (remaining - RAIL_NEAR_SPAN_M)) as f32
}

/// A European speed-limit sign: the number in a red annulus. Painted rather than
/// composed from widgets because that is the only way to get the proportions of
/// the real sign, which is what makes it readable without being read.
fn speed_limit_sign(ui: &mut egui::Ui, limit: u16) {
    const R: f32 = 13.0;
    let (rect, _) = ui.allocate_exact_size(egui::Vec2::splat(R * 2.0), egui::Sense::hover());
    let c = rect.center();
    let painter = ui.painter();
    painter.circle_filled(c, R, SIGN_WHITE);
    painter.circle_stroke(c, R - 1.9, egui::Stroke::new(3.8, SIGN_RED));
    painter.text(
        c,
        egui::Align2::CENTER_CENTER,
        limit.to_string(),
        egui::FontId::proportional(if limit >= 100 { 10.0 } else { 12.0 }),
        egui::Color32::BLACK,
    );
}

/// A readout value.
fn big(text: &str, size: f32, color: egui::Color32) -> egui::RichText {
    egui::RichText::new(text).size(size).strong().color(color)
}

/// Its caption.
fn small(text: &str, color: egui::Color32) -> egui::RichText {
    egui::RichText::new(text).size(11.0).color(color)
}

/// Wall-clock time of arrival, `remaining` seconds after `h:m`.
///
/// **Suffixed when the clock is not local.** `WorldClock::hms` returns the time
/// *and* whether it is local — it is UTC before the anchor's timezone has been
/// resolved, and permanently so whenever the user has asked for UTC. Discarding
/// that second value, which this did, produces the one arrival time a driver
/// cannot detect as wrong: an hour or two out, in the right format, in a box whose
/// whole job is to be glanced at.
fn arrival_clock(h: u32, m: u32, remaining_s: f64, local: bool) -> String {
    let mins = (i64::from(h) * 60 + i64::from(m) + (remaining_s / 60.0).round() as i64)
        .rem_euclid(24 * 60);
    let suffix = if local { "" } else { " UTC" };
    format!("{:02}:{:02}{suffix}", mins / 60, mins % 60)
}

// ─── Audio guidance ──────────────────────────────────────────────────────────
//
// The single biggest thing missing from a display meant to be used with your eyes
// on the road: every instruction was silent, so acting on it meant looking at it.
// Three tones on the approach to a manoeuvre, one when the route is being
// replaced, one on arrival.
//
// **Deliberately not speech.** There is no TTS anywhere in this tree, native has
// none to borrow, and a synthesised voice reading street names badly is worse than
// a tone that means "your turn is coming" — the tone is unambiguous at 120 km/h
// with the window open, which is the whole test.
//
// The bands are `countdown`'s own ladder rather than a second set of numbers: the
// card coarsens at ~400 m, ~150 m and "Now" precisely because those are the points
// at which something changes for the driver, so a cue at each of them is a cue at
// each moment the card had something new to say.

/// Where the cues fire, in metres to the manoeuvre.
///
/// The `allow` here and on the items below is the price of keeping the cue logic
/// compiled and tested on a host that cannot build an audio backend — the same
/// bargain `soundscape` strikes in its module note, and the reason its synthesis
/// and rate maths are pure functions. `unused = "deny"` is a workspace lint, so
/// without it a `--no-default-features` build fails on the arithmetic rather than
/// on anything to do with sound.
#[cfg_attr(not(feature = "audio"), allow(dead_code))]
const CUE_PREPARE_M: f64 = 400.0;
#[cfg_attr(not(feature = "audio"), allow(dead_code))]
const CUE_APPROACH_M: f64 = 150.0;

/// How loud a cue is. Under the footsteps, over nothing: it has to be audible in a
/// car without being the loudest thing the application ever does.
#[cfg(feature = "audio")]
const CUE_VOLUME: f32 = 0.45;

/// Sample rate for the cue tones. Same 22.05 kHz `soundscape` synthesises at —
/// these are pure tones and it is twice what they need.
#[cfg_attr(not(feature = "audio"), allow(dead_code))]
const CUE_SAMPLE_RATE: u32 = 22_050;

/// How close the manoeuvre is, coarsened to the three points a driver acts on.
///
/// Ordered, and the order is load-bearing: a cue fires when the band gets *deeper*,
/// so a fix that jitters back over a threshold cannot re-fire the same tone.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
#[cfg_attr(not(feature = "audio"), allow(dead_code))]
enum CueBand {
    Prepare,
    Approach,
    Now,
}

/// Which band a distance falls in, or `None` when there is nothing to prepare for.
///
/// Gated on the manoeuvre being one you *do* — the same `turn_ahead` test the
/// camera's junction tighten uses, and for the same reason: the noded graph puts
/// `Continue` steps in the middle of a curve, and chiming at every vertex of a
/// bend would train the driver to ignore the chime.
#[cfg_attr(not(feature = "audio"), allow(dead_code))]
fn cue_band(step_distance_m: f64, turn_ahead: bool) -> Option<CueBand> {
    if !turn_ahead {
        return None;
    }
    if step_distance_m <= NOW_M {
        Some(CueBand::Now)
    } else if step_distance_m <= CUE_APPROACH_M {
        Some(CueBand::Approach)
    } else if step_distance_m <= CUE_PREPARE_M {
        Some(CueBand::Prepare)
    } else {
        None
    }
}

/// A sound the drive can make.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[cfg_attr(not(feature = "audio"), allow(dead_code))]
enum Cue {
    Prepare,
    Approach,
    Now,
    Reroute,
    Arrived,
}

/// What has already been announced. One `Local` on the playing system.
#[derive(Default)]
#[cfg_attr(not(feature = "audio"), allow(dead_code))]
// `pub` for the same reason as `CueClips`: it is a `Local` in
// `play_driver_cues`'s signature, which Bevy makes part of the public type.
pub struct CueMemory {
    active: bool,
    step: Option<usize>,
    band: Option<CueBand>,
    rerouting: bool,
    arrived: bool,
}

/// This frame's facts, as the cue logic needs them.
#[cfg_attr(not(feature = "audio"), allow(dead_code))]
struct CueInputs {
    active: bool,
    step: usize,
    band: Option<CueBand>,
    rerouting: bool,
    arrived: bool,
}

/// Decide what — if anything — to play, and remember it.
///
/// Pure over its own memory so the interesting parts are testable on a host that
/// cannot build an audio backend at all (`soundscape`'s module note explains why
/// this project takes that seriously): that a cue fires once per band per
/// manoeuvre, that a new manoeuvre re-arms the ladder, and that entering driver
/// mode is silent — the frame the mode turns on is the frame that knows least, and
/// a "Now" chime for whatever junction you happen to be beside is not a start to
/// a drive.
#[cfg_attr(not(feature = "audio"), allow(dead_code))]
fn next_cue(mem: &mut CueMemory, now: CueInputs) -> Option<Cue> {
    if !now.active {
        *mem = CueMemory::default();
        return None;
    }
    let entering = !mem.active;
    mem.active = true;
    if mem.step != Some(now.step) {
        mem.step = Some(now.step);
        mem.band = None;
    }
    let arrived = now.arrived && !mem.arrived;
    mem.arrived = now.arrived;
    let rerouting = now.rerouting && !mem.rerouting;
    mem.rerouting = now.rerouting;
    let band = match now.band {
        Some(b) if mem.band.is_none_or(|had| b > had) => {
            mem.band = Some(b);
            Some(b)
        }
        _ => None,
    };
    if entering {
        return None;
    }
    // Arrival wins: it is the end of the trip, and the last manoeuvre's "Now" and
    // the arrival land within a few metres of each other.
    if arrived {
        return Some(Cue::Arrived);
    }
    if rerouting {
        return Some(Cue::Reroute);
    }
    match band {
        Some(CueBand::Prepare) => Some(Cue::Prepare),
        Some(CueBand::Approach) => Some(Cue::Approach),
        Some(CueBand::Now) => Some(Cue::Now),
        None => None,
    }
}

/// The notes a cue is made of: `(hertz, seconds)`.
///
/// Rising as the manoeuvre closes and falling on a reroute, because a driver
/// learns these in one trip or not at all and the direction of the pitch is the
/// only part that carries meaning without being taught.
#[cfg_attr(not(feature = "audio"), allow(dead_code))]
fn cue_notes(cue: Cue) -> &'static [(f32, f32)] {
    match cue {
        Cue::Prepare => &[(880.0, 0.14)],
        Cue::Approach => &[(880.0, 0.11), (1174.7, 0.15)],
        Cue::Now => &[(1174.7, 0.09), (1567.9, 0.18)],
        // Down, and the only descending cue: something did not go to plan.
        Cue::Reroute => &[(880.0, 0.12), (587.3, 0.20)],
        Cue::Arrived => &[(880.0, 0.12), (1108.7, 0.12), (1318.5, 0.24)],
    }
}

/// A sequence of notes as a complete WAV file.
///
/// Synthesised rather than shipped, exactly as every other sound in this client is
/// (`soundscape`'s module note): `.gitattributes` tracks audio by extension, so a
/// shipped chime would be an LFS object with the documented silent-failure mode —
/// a clone without `git lfs install` gets a 130-byte pointer and no sound, with
/// nothing in the log. Four lines of arithmetic cannot fail that way.
///
/// Each note is enveloped with a 5 ms attack: the discontinuity at the start of a
/// tone is an audible click, and a click is exactly what a driver hears as a fault.
#[cfg_attr(not(feature = "audio"), allow(dead_code))]
fn cue_wav(notes: &[(f32, f32)]) -> Vec<u8> {
    let attack = (CUE_SAMPLE_RATE as f32 * 0.005) as usize;
    let mut samples: Vec<i16> = Vec::new();
    for &(freq, secs) in notes {
        let len = (CUE_SAMPLE_RATE as f32 * secs) as usize;
        for i in 0..len {
            let t = i as f32 / CUE_SAMPLE_RATE as f32;
            let env = if i < attack {
                i as f32 / attack.max(1) as f32
            } else {
                (-5.0 * (i - attack) as f32 / len.max(1) as f32).exp()
            };
            let v = (t * freq * std::f32::consts::TAU).sin() * env * 0.5;
            samples.push((v.clamp(-1.0, 1.0) * f32::from(i16::MAX)) as i16);
        }
    }

    // A 44-byte PCM header, hand-written for the same reason `soundscape::synth`
    // hand-writes its own: it is a fixed run of known constants, and an encoder
    // crate would be the larger dependency by far.
    let data_len = (samples.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes()); // PCM chunk size
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&CUE_SAMPLE_RATE.to_le_bytes());
    out.extend_from_slice(&(CUE_SAMPLE_RATE * 2).to_le_bytes()); // byte rate
    out.extend_from_slice(&2u16.to_le_bytes()); // block align
    out.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for s in &samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

/// The five clips, built on the first frame that needs one.
///
/// Lazily rather than in a startup system, because that would be a second
/// registration for a feature that is off on most launches — and because five
/// tenths of a second of 16-bit mono is ~20 KB, cheap enough that building it
/// during a drive is not a decision worth a frame budget.
#[cfg(feature = "audio")]
// `pub` only because it appears in `play_driver_cues`'s signature as a `Local`,
// and Bevy's system registration makes that part of the public type. Nothing
// outside this module has any reason to name it.
pub struct CueClips {
    prepare: Handle<bevy::audio::AudioSource>,
    approach: Handle<bevy::audio::AudioSource>,
    now: Handle<bevy::audio::AudioSource>,
    reroute: Handle<bevy::audio::AudioSource>,
    arrived: Handle<bevy::audio::AudioSource>,
}

#[cfg(feature = "audio")]
impl CueClips {
    fn build(sources: &mut Assets<bevy::audio::AudioSource>) -> Self {
        let mut clip = |cue: Cue| {
            sources.add(bevy::audio::AudioSource {
                bytes: cue_wav(cue_notes(cue)).into(),
            })
        };
        Self {
            prepare: clip(Cue::Prepare),
            approach: clip(Cue::Approach),
            now: clip(Cue::Now),
            reroute: clip(Cue::Reroute),
            arrived: clip(Cue::Arrived),
        }
    }

    fn of(&self, cue: Cue) -> Handle<bevy::audio::AudioSource> {
        match cue {
            Cue::Prepare => self.prepare.clone(),
            Cue::Approach => self.approach.clone(),
            Cue::Now => self.now.clone(),
            Cue::Reroute => self.reroute.clone(),
            Cue::Arrived => self.arrived.clone(),
        }
    }
}

/// Sound the guidance cues.
///
/// Gated on the `audio` feature (which pulls `alsa-sys`, so a headless host builds
/// without it) and on [`DriverState::cues`], the field a Settings row binds to.
#[cfg(feature = "audio")]
pub fn play_driver_cues(
    mut commands: Commands,
    mut sources: ResMut<Assets<bevy::audio::AudioSource>>,
    driver: Res<DriverState>,
    prefs: Res<crate::resources::AppPrefs>,
    mut clips: Local<Option<CueClips>>,
    mut memory: Local<CueMemory>,
) {
    let progress = driver.progress.unwrap_or_default();
    let cue = next_cue(
        &mut memory,
        CueInputs {
            active: driver.active && driver.cues.0,
            step: progress.step,
            band: cue_band(progress.step_distance_m, progress.turn_ahead),
            rerouting: driver.rerouting,
            arrived: driver.arrived,
        },
    );
    let Some(cue) = cue else {
        return;
    };
    let clips = clips.get_or_insert_with(|| CueClips::build(&mut sources));
    commands.spawn((
        bevy::audio::AudioPlayer(clips.of(cue)),
        bevy::audio::PlaybackSettings {
            mode: bevy::audio::PlaybackMode::Despawn,
            // The cues are part of the soundscape the Settings slider governs:
            // a turn chime at full level over a quietened footstep is the same
            // complaint arriving from the other direction.
            volume: bevy::audio::Volume::Linear(CUE_VOLUME * prefs.effects_gain()),
            ..default()
        },
    ));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::systems::routing::{RouteStep, TravelMode};
    use std::time::Duration;

    /// A straight 1 km route due east, with a turn in the middle, in metres that
    /// are easy to check by hand.
    fn fixture() -> Route {
        // ~0.001° of longitude at 53°N is ~67 m; use latitude steps instead, where
        // a degree is a constant 110.54 km, so the expected distances are exact.
        let pts: Vec<[f64; 2]> = (0..=10)
            .map(|i| [53.0 + f64::from(i) * 0.001, 6.0])
            .collect();
        let mut points = vec![[52.9995, 6.0]]; // start pin
        points.extend(pts.iter().copied());
        points.push([53.0105, 6.0]); // end pin
        let levels = vec![0; points.len()];
        Route {
            mode: TravelMode::Car,
            points,
            levels,
            distance_m: 1105.4,
            duration_s: 120.0,
            steps: vec![
                RouteStep {
                    maneuver: Maneuver::Depart,
                    name: Some("First street".into()),
                    distance_m: 552.7,
                    duration_s: 60.0,
                    at: [53.0, 6.0],
                },
                RouteStep {
                    maneuver: Maneuver::Left,
                    name: Some("Second street".into()),
                    distance_m: 552.7,
                    duration_s: 60.0,
                    at: [53.005, 6.0],
                },
                RouteStep {
                    maneuver: Maneuver::Arrive,
                    name: None,
                    distance_m: 0.0,
                    duration_s: 0.0,
                    at: [53.01, 6.0],
                },
            ],
        }
    }

    /// The measured polyline must cover the *road*, not the pins: driving the
    /// lead-in from a pin (which may be across a canal — the case
    /// `navigation`'s thin connector exists for) is not part of the route.
    #[test]
    fn geometry_measures_the_road_only() {
        let route = fixture();
        let geom = RouteGeom::build(&route).expect("a routable fixture");
        assert_eq!(geom.points.len(), 11, "the two pins are excluded");
        assert!(
            (geom.total_m - 1105.4).abs() < 1.0,
            "0.01° of latitude is 1105.4 m, got {}",
            geom.total_m
        );
        // Steps land where their coordinates say, not where their distances do.
        assert!(geom.step_along[0].abs() < 1.0);
        assert!(
            (geom.step_along[1] - 552.7).abs() < 1.0,
            "{:?}",
            geom.step_along
        );
        assert!((geom.step_along[2] - 1105.4).abs() < 1.0);
    }

    /// Progress must fall out of the *projection*, not the nearest vertex: a
    /// position mid-segment reports mid-segment, or the remaining distance freezes
    /// on a long straight and then jumps.
    #[test]
    fn progress_projects_onto_the_segment() {
        let route = fixture();
        let geom = RouteGeom::build(&route).unwrap();
        // A quarter of the way along, 20 m off to the side.
        let side = 20.0 / (111_320.0 * 53.0_f64.to_radians().cos());
        let p = project_onto_route(&geom, &route, [53.0025, 6.0 + side]);
        assert!((p.along_m - 276.35).abs() < 2.0, "along {}", p.along_m);
        assert!((p.off_route_m - 20.0).abs() < 1.0, "off {}", p.off_route_m);
        assert!(
            (p.remaining_m - 829.05).abs() < 2.0,
            "remaining {}",
            p.remaining_m
        );
        // Time is scaled from the planner's answer, so three quarters left of 120 s.
        assert!((p.remaining_s - 90.0).abs() < 1.0, "eta {}", p.remaining_s);
    }

    /// The card must name the manoeuvre being *driven toward*, and must not flip
    /// to the next one the instant the junction is reached — that is the moment it
    /// is being read.
    #[test]
    fn the_next_manoeuvre_survives_arriving_at_it() {
        let route = fixture();
        let geom = RouteGeom::build(&route).unwrap();

        let early = project_onto_route(&geom, &route, [53.001, 6.0]);
        assert_eq!(early.step, 1, "the turn ahead, not the departure behind");
        assert!((early.step_distance_m - 442.16).abs() < 2.0);

        // Right at the turn: still the turn.
        let at_turn = project_onto_route(&geom, &route, [53.005, 6.0]);
        assert_eq!(at_turn.step, 1, "flipped away at the junction itself");

        // Well past it: the arrival.
        let past = project_onto_route(&geom, &route, [53.006, 6.0]);
        assert_eq!(past.step, 2);
    }

    /// Being off the route must be *measured*, not silently projected — the card
    /// switches to "Off route" from this number.
    #[test]
    fn off_route_distance_is_reported() {
        let route = fixture();
        let geom = RouteGeom::build(&route).unwrap();
        let far = 200.0 / (111_320.0 * 53.0_f64.to_radians().cos());
        let p = project_onto_route(&geom, &route, [53.005, 6.0 + far]);
        assert!(p.off_route_m > OFF_ROUTE_M, "off {}", p.off_route_m);
    }

    /// The compass cycle must be a cycle: three orientations and back, or the
    /// button strands the user in one of them.
    #[test]
    fn the_orientation_cycle_returns_to_its_start() {
        let start = MapOrientation::default();
        assert_eq!(start.next().next().next(), start);
        assert_ne!(start.next(), start);
    }

    /// Arrival time is a wall clock, so it has to wrap at midnight rather than
    /// print "25:10" — and it has to say when it is not the *local* wall clock,
    /// which is the one error in it a driver could not spot.
    #[test]
    fn arrival_wraps_past_midnight_and_admits_utc() {
        assert_eq!(arrival_clock(18, 57, 6.0 * 60.0, true), "19:03");
        assert_eq!(arrival_clock(23, 50, 30.0 * 60.0, true), "00:20");
        assert_eq!(arrival_clock(9, 0, 0.0, true), "09:00");
        // `WorldClock` is UTC until the anchor's timezone resolves, and stays UTC
        // whenever the user asked for it. An unlabelled time is then wrong by hours
        // in a format that looks exactly right.
        assert_eq!(arrival_clock(18, 57, 6.0 * 60.0, false), "19:03 UTC");
    }

    /// The arrow must point along +Z (the avatar's forward, so a course yaw turns
    /// it correctly) and face up, or it is backface-culled into nothing — the
    /// failure that cost the route line two debugging sessions.
    #[test]
    fn the_arrow_points_forward_and_faces_up() {
        let mesh = arrow_mesh();
        let positions = match mesh.attribute(Mesh::ATTRIBUTE_POSITION).unwrap() {
            bevy::render::mesh::VertexAttributeValues::Float32x3(v) => v.clone(),
            _ => panic!("positions are not Float32x3"),
        };
        let max_z = positions.iter().map(|p| p[2]).fold(f32::MIN, f32::max);
        assert!(
            (max_z - ARROW_HALF_LENGTH).abs() < 1e-3,
            "nose is not at +Z: {max_z}"
        );
        assert!(
            positions.iter().all(|p| p[1] == 0.0),
            "the arrow must be flat"
        );

        let Some(Indices::U32(indices)) = mesh.indices() else {
            panic!("expected u32 indices")
        };
        for t in indices.chunks_exact(3) {
            let (a, b, c) = (
                positions[t[0] as usize],
                positions[t[1] as usize],
                positions[t[2] as usize],
            );
            let cross = (b[2] - a[2]) * (c[0] - a[0]) - (b[0] - a[0]) * (c[2] - a[2]);
            assert!(cross > 0.0, "triangle {t:?} wound downward (cross {cross})");
        }
    }

    /// Entering must arm the mode and leaving must release the subscription — a
    /// watch left running keeps the browser's location indicator lit after the
    /// user has stopped navigating.
    #[test]
    fn entering_and_leaving_are_symmetric() {
        let mut driver = DriverState::default();
        driver.enter();
        assert!(driver.active && driver.watch.is_some() && driver.snap_pending);
        driver.exit();
        assert!(!driver.active && driver.watch.is_none() && driver.progress.is_none());
    }

    /// The rail's scale: pinned to its ends, monotonic in between, and — the part
    /// that is the whole point — giving the near field a share of the track far
    /// out of proportion to its share of the distance.
    #[test]
    fn the_rail_scale_favours_the_near_field_without_going_backwards() {
        // Short route: no break, plain linear.
        assert!((rail_t(0.0, 1_000.0) - 0.0).abs() < 1e-6);
        assert!((rail_t(500.0, 1_000.0) - 0.5).abs() < 1e-6);
        assert!((rail_t(1_000.0, 1_000.0) - 1.0).abs() < 1e-6);

        // Long route: the first RAIL_NEAR_SPAN_M — 5% of a 40 km drive — gets
        // RAIL_NEAR_FRACTION of the track. Linearly it would get 5% of it, and
        // every marker in it would land inside a dozen pixels.
        let remaining = 40_000.0;
        assert!((rail_t(0.0, remaining) - 0.0).abs() < 1e-6);
        assert!((rail_t(RAIL_NEAR_SPAN_M, remaining) - RAIL_NEAR_FRACTION).abs() < 1e-6);
        assert!((rail_t(remaining, remaining) - 1.0).abs() < 1e-6);
        let mut prev = -1.0;
        for i in 0..=400 {
            let t = rail_t(remaining * f64::from(i) / 400.0, remaining);
            assert!(t >= prev, "rail scale went backwards at {i}");
            prev = t;
        }

        // Past the destination and behind the vehicle both clamp onto the track —
        // an item projected a metre beyond the end must not paint off the widget.
        assert!((rail_t(remaining * 2.0, remaining) - 1.0).abs() < 1e-6);
        assert!((rail_t(-50.0, remaining) - 0.0).abs() < 1e-6);
    }

    /// The reroute cooldown is the loop breaker: a solve can legitimately return a
    /// route the vehicle is still off, and without a floor between attempts the
    /// display would re-solve every frame it stayed there.
    #[test]
    fn a_reroute_waits_out_its_cooldown() {
        let mut driver = DriverState::default();
        let now = Instant::now();
        assert!(
            driver.reroute_ready(now),
            "the first reroute is never blocked"
        );
        driver.reroute_at = Some(now);
        assert!(!driver.reroute_ready(now));
        assert!(!driver.reroute_ready(now + Duration::from_secs_f32(REROUTE_COOLDOWN * 0.5)));
        assert!(driver.reroute_ready(now + Duration::from_secs_f32(REROUTE_COOLDOWN + 0.1)));
    }

    /// Entering driver mode a second time must not inherit the previous drive's
    /// cooldown — that would block the first reroute of the new one.
    #[test]
    fn entering_clears_the_reroute_state() {
        let mut driver = DriverState::default();
        driver.enter();
        driver.reroute_at = Some(Instant::now());
        driver.reroute_gen = Some(7);
        driver.rerouting = true;
        driver.exit();
        driver.enter();
        assert!(driver.reroute_at.is_none() && driver.reroute_gen.is_none());
        assert!(!driver.rerouting);
        assert!(driver.reroute_ready(Instant::now()));
    }

    /// The end of the route has to land inside [`ARRIVE_M`], or the arrival state
    /// is unreachable and the card counts "Arrive · 0 m" forever — which is what
    /// it did.
    #[test]
    fn the_end_of_the_route_is_within_reach_of_arriving() {
        let route = fixture();
        let geom = RouteGeom::build(&route).unwrap();
        let at_end = project_onto_route(&geom, &route, [53.01, 6.0]);
        assert!(
            at_end.remaining_m <= ARRIVE_M,
            "remaining {} at the last road vertex",
            at_end.remaining_m
        );
        // …and a few hundred metres short of it is emphatically not arriving.
        let short = project_onto_route(&geom, &route, [53.007, 6.0]);
        assert!(
            short.remaining_m > ARRIVE_M,
            "remaining {}",
            short.remaining_m
        );
    }

    /// The rig only closes in on things you actually *do*. A `Continue` reached
    /// mid-leg would otherwise pull the camera in for a vertex of a curve, and
    /// the view would breathe its way along every bend.
    #[test]
    fn only_a_real_manoeuvre_counts_as_a_turn_ahead() {
        let mut route = fixture();
        let geom = RouteGeom::build(&route).unwrap();
        assert!(
            project_onto_route(&geom, &route, [53.001, 6.0]).turn_ahead,
            "a Left is a turn"
        );
        route.steps[1].maneuver = Maneuver::Continue;
        assert!(!project_onto_route(&geom, &route, [53.001, 6.0]).turn_ahead);
        // Arriving is one: the destination is exactly where the detail matters.
        assert!(project_onto_route(&geom, &route, [53.008, 6.0]).turn_ahead);
    }

    /// The countdown ladder: coarse far out, fine near, and "Now" at the junction
    /// — and never the "1000 m" that rounding to the nearest 50 produces if the
    /// unit is chosen before the rounding.
    #[test]
    fn the_countdown_coarsens_with_distance() {
        assert_eq!(countdown(0.0), "Now");
        assert_eq!(countdown(NOW_M), "Now");
        assert_eq!(countdown(24.0), "20 m");
        assert_eq!(countdown(96.0), "100 m");
        assert_eq!(countdown(230.0), "250 m");
        assert_eq!(countdown(980.0), "1.0 km");
        assert_eq!(countdown(4_240.0), "4.2 km");
        assert_eq!(countdown(23_400.0), "23 km");
        // Monotonic, so the number never counts *up* as the junction approaches.
        let mut prev = f64::NEG_INFINITY;
        for i in (0..20_000).step_by(7) {
            let m = f64::from(i);
            let shown = parse_countdown(&countdown(m));
            assert!(
                shown >= prev - 1.0,
                "counted up at {m} m: {shown} after {prev}"
            );
            prev = shown;
        }
    }

    /// Metres back out of a rendered countdown, for the monotonicity check above.
    fn parse_countdown(s: &str) -> f64 {
        if s == "Now" {
            return 0.0;
        }
        let (n, unit) = s.split_once(' ').unwrap();
        let n: f64 = n.parse().unwrap();
        if unit == "km" {
            n * 1000.0
        } else {
            n
        }
    }

    /// The rig rides the speedometer and closes onto a junction, and — the part
    /// worth pinning — it is exactly the orientation's own pose in neither case,
    /// so a standstill on a long leg is still the tuned view.
    #[test]
    fn the_rig_stretches_with_speed_and_tightens_onto_a_junction() {
        let far = Some(Progress {
            step_distance_m: 4_000.0,
            turn_ahead: true,
            ..default()
        });
        assert!(
            (rig_scale(0.0, far) - 1.0).abs() < 1e-6,
            "a standstill is the tuned pose"
        );
        assert!((rig_scale(0.0, None) - 1.0).abs() < 1e-6);

        // Speed only stretches, and it stops stretching past the reference.
        let at_ref = rig_scale(RIG_SPEED_REF_KMH / 3.6, far);
        assert!(
            (at_ref - (1.0 + RIG_SPEED_STRETCH)).abs() < 1e-5,
            "{at_ref}"
        );
        assert!(
            (rig_scale(300.0 / 3.6, far) - at_ref).abs() < 1e-5,
            "clamped past the reference"
        );
        assert!(rig_scale(50.0 / 3.6, far) > 1.0);

        // At the junction itself, the tightest the rig gets.
        let at_turn = Some(Progress {
            step_distance_m: 0.0,
            turn_ahead: true,
            ..default()
        });
        assert!((rig_scale(0.0, at_turn) - RIG_JUNCTION_TIGHTEN).abs() < 1e-6);
        // …but not for something that is not a manoeuvre.
        let straight_on = Some(Progress {
            step_distance_m: 0.0,
            turn_ahead: false,
            ..default()
        });
        assert!((rig_scale(0.0, straight_on) - 1.0).abs() < 1e-6);

        // Monotonic in the approach, so the zoom is a glide and never a bounce.
        let mut prev = 0.0;
        for i in 0..=200 {
            let d = RIG_JUNCTION_M * 2.0 * i as f32 / 200.0;
            let s = rig_scale(
                0.0,
                Some(Progress {
                    step_distance_m: f64::from(d),
                    turn_ahead: true,
                    ..default()
                }),
            );
            assert!(s >= prev - 1e-6, "not monotonic at {d} m");
            prev = s;
        }
    }

    /// Every orientation must put the marker in the lower third — far enough down
    /// that the screen is spent on the road ahead, not so far that it is under the
    /// speed pill with nothing visible beside it. The flat rigs failed this at 83%
    /// and nobody could see it, because the machine this is built on cannot take a
    /// screenshot and the number is not in any log.
    #[test]
    fn every_orientation_puts_the_marker_in_the_lower_third() {
        // Bevy's `PerspectiveProjection::default()`, which `setup_camera` keeps.
        const FOV_Y: f32 = std::f32::consts::FRAC_PI_4;
        // Comfortably inside the band, not merely on the right side of it: the
        // rig scale moves the distance by ±55% and the framing must not drift out.
        const BAND: std::ops::RangeInclusive<f32> = 0.64..=0.78;

        for orientation in [
            MapOrientation::Perspective,
            MapOrientation::HeadingUp,
            MapOrientation::NorthUp,
        ] {
            let (base, pitch, lookahead) = orientation.rig();
            // Across everything `rig_scale` can do: standing still on a long leg,
            // at the reference speed, and at the tightest a junction pulls it.
            for scale in [RIG_JUNCTION_TIGHTEN, 1.0, 1.0 + RIG_SPEED_STRETCH] {
                let d = base * scale;
                let y = marker_screen_y(d, pitch, d * lookahead, FOV_Y);
                assert!(
                    BAND.contains(&y),
                    "{orientation:?} at ×{scale}: marker at {y:.3} down the screen",
                );
            }
        }
    }

    /// The projection itself, against two cases that can be reasoned about without
    /// it — otherwise the framing test above is only checking its own arithmetic.
    #[test]
    fn the_marker_projection_agrees_with_its_degenerate_cases() {
        const FOV_Y: f32 = std::f32::consts::FRAC_PI_4;
        // Aiming at the vehicle all but centres it — "all but", because the look
        // target is the 1.5 m eye point and the marker is the ground under it, so
        // the marker sits a hair *below* centre and the hair shrinks with
        // distance. Never above it, at any tilt.
        let mut last = f32::INFINITY;
        for (d, p) in [(60.0, 0.9), (170.0, 0.38), (240.0, 1.35)] {
            let y = marker_screen_y(d, p, 0.0, FOV_Y);
            assert!((0.5..0.52).contains(&y), "d={d} p={p}: {y}");
            assert!(y < last, "the eye-height offset must shrink with distance");
            last = y;
        }
        // Aiming further ahead always pushes it further down, never back up.
        let mut prev = 0.0;
        for i in 0..=50 {
            let y = marker_screen_y(240.0, 1.35, 240.0 * 0.01 * i as f32, FOV_Y);
            assert!(y >= prev, "aiming further ahead moved the marker up at {i}");
            prev = y;
        }
    }

    /// A peek accumulates, clamps, and — the part that makes it safe to use while
    /// driving — puts itself back without being asked.
    #[test]
    fn a_peek_expires_and_eases_home() {
        let mut peek = Peek::default();
        assert!(!peek.held(), "the driving pose is not a peek");

        apply_gesture(
            &mut peek,
            Gesture {
                zoom: 1.5,
                turn: 0.4,
                tilt: 0.1,
            },
            0.5,
        );
        assert!(peek.held());
        assert!((peek.zoom - 1.5).abs() < 1e-6);
        assert!((peek.azimuth - 0.4).abs() < 1e-6);

        // Zoom is bounded whatever the wheel does — a chase view is not a globe.
        for _ in 0..100 {
            apply_gesture(
                &mut peek,
                Gesture {
                    zoom: 1.15,
                    ..default()
                },
                0.5,
            );
        }
        assert!((peek.zoom - PEEK_ZOOM_MAX).abs() < 1e-4, "{}", peek.zoom);
        for _ in 0..200 {
            apply_gesture(
                &mut peek,
                Gesture {
                    zoom: 1.0 / 1.15,
                    ..default()
                },
                0.5,
            );
        }
        assert!((peek.zoom - PEEK_ZOOM_MIN).abs() < 1e-4, "{}", peek.zoom);

        // An idle frame while it is still held changes nothing.
        let before = (peek.zoom, peek.azimuth, peek.pitch);
        apply_gesture(&mut peek, Gesture::default(), 0.5);
        assert_eq!((peek.zoom, peek.azimuth, peek.pitch), before);

        // Released (the button, or the timeout), idle frames ease it home and it
        // snaps to exactly the driving pose rather than creeping at it forever.
        peek.since = None;
        for _ in 0..200 {
            apply_gesture(&mut peek, Gesture::default(), 0.5);
        }
        assert_eq!(peek.zoom, 1.0);
        assert_eq!(peek.azimuth, 0.0);
        assert_eq!(peek.pitch, 0.0);
        assert!(peek.since.is_none());
    }

    /// Leaving must hand back a camera with no driver-mode state left on it. The
    /// lookahead is the one that bites: it is only consulted while driving, so a
    /// stale value sits there invisibly until the *next* drive starts with the aim
    /// point of the last one.
    #[test]
    fn leaving_clears_the_peek() {
        let mut driver = DriverState::default();
        driver.enter();
        apply_gesture(
            &mut driver.peek,
            Gesture {
                zoom: 2.0,
                turn: 1.0,
                tilt: 0.2,
            },
            0.5,
        );
        assert!(driver.peeking());
        driver.recentre();
        assert!(!driver.peeking(), "re-centring gives the rig back at once");
        driver.exit();
        assert_eq!(driver.peek.zoom, 1.0);
        assert_eq!(driver.peek.azimuth, 0.0);
    }

    /// The "then" preview keys off the gap to the manoeuvre *after* the next one,
    /// so it appears for two turns in quick succession and stays away otherwise.
    #[test]
    fn the_then_preview_only_fires_on_a_close_pair() {
        let mut route = fixture();
        let far = Progress {
            step: 0,
            ..default()
        };
        route.steps[1].distance_m = 900.0;
        assert!(next_step_gap(&route, far) > THEN_PREVIEW_M);
        route.steps[1].distance_m = 120.0;
        assert!(next_step_gap(&route, far) <= THEN_PREVIEW_M);
        // No step after the last one: never previews, never panics.
        let last = Progress {
            step: route.steps.len() - 1,
            ..default()
        };
        assert!(next_step_gap(&route, last).is_infinite());
    }

    /// The snapped marker has to land at exactly the distance the card is counting
    /// down from — the same projection, run forwards — or the arrow and the number
    /// disagree about where the vehicle is.
    #[test]
    fn the_marker_snaps_onto_the_route_at_its_own_distance() {
        let route = fixture();
        let geom = RouteGeom::build(&route).unwrap();

        // A quarter of the way along, 20 m off to the side: the projection says
        // 276 m along, and the snapped point is 276 m along the line itself.
        let side = 20.0 / (111_320.0 * 53.0_f64.to_radians().cos());
        let p = project_onto_route(&geom, &route, [53.0025, 6.0 + side]);
        let (on, _) = point_along(&geom, p.along_m).expect("a point on the line");
        assert!(
            (on[1] - 6.0).abs() < 1e-9,
            "the snapped point must be on the line, not beside it: {on:?}"
        );
        let (along_again, off_again) = nearest_on_polyline(&geom.points, &geom.cum, on);
        assert!((along_again - p.along_m).abs() < 0.5, "{along_again}");
        assert!(off_again < 0.01, "the snapped point is on the route by 0");

        // The ends clamp rather than running off the polyline.
        let start = point_along(&geom, -50.0).unwrap().0;
        assert!((start[0] - geom.points[0][0]).abs() < 1e-9, "{start:?}");
        let end = point_along(&geom, geom.total_m * 2.0).unwrap().0;
        let last = *geom.points.last().unwrap();
        assert!((end[0] - last[0]).abs() < 1e-9, "{end:?} vs {last:?}");
    }

    /// …but the marker must not be *aimed* by the road when the vehicle is plainly
    /// not going that way. A route driven backwards would otherwise show an arrow
    /// pointing at the windscreen's opposite.
    #[test]
    fn a_snapped_yaw_defers_to_a_contradicting_course() {
        use std::f32::consts::{FRAC_PI_2, PI};
        // Agreeing within a quarter turn: take the road's angle, which is the
        // whole point — it removes the jitter of a measured course.
        assert!((snapped_yaw(0.0, 0.2) - 0.2).abs() < 1e-6);
        assert!((snapped_yaw(0.0, -0.2) - -0.2).abs() < 1e-6);
        assert!((snapped_yaw(0.0, FRAC_PI_2 - 0.01) - (FRAC_PI_2 - 0.01)).abs() < 1e-6);
        // Reversed: keep the measured course.
        assert!((snapped_yaw(0.0, PI) - 0.0).abs() < 1e-6);
        assert!((snapped_yaw(0.0, PI - 0.1) - 0.0).abs() < 1e-6);
        // …and it must wrap, or a course near ±π compares against the wrong branch.
        assert!((snapped_yaw(PI - 0.05, -PI + 0.05) - (-PI + 0.05)).abs() < 1e-6);
    }

    /// The fix's quality has to be a *state*, because "no fix yet", "50 m in a
    /// tunnel" and "locked on" all rendered as the same confident navigation.
    #[test]
    fn a_fix_is_classified_before_it_is_believed() {
        // Desktop: no provider, so there is nothing to complain about and nothing
        // to suppress — driver mode narrates the avatar there.
        assert_eq!(
            gps_quality(false, false, None, None),
            GpsQuality::NoProvider
        );
        assert!(!GpsQuality::NoProvider.unreliable());
        assert!(GpsQuality::NoProvider.chip().is_none());

        // A watch running with nothing back yet is a wait worth showing.
        assert_eq!(gps_quality(true, false, None, None), GpsQuality::Waiting);
        assert!(GpsQuality::Waiting.chip().is_some());

        // A good fix says nothing at all *on the driving card* — and carries its
        // accuracy anyway, because the locate button's whole job is to say so.
        assert_eq!(
            gps_quality(true, false, Some(6.0), Some(0.5)),
            GpsQuality::Good(6.0)
        );
        assert!(GpsQuality::Good(6.0).chip().is_none());
        assert_eq!(GpsQuality::Good(6.0).caption().as_deref(), Some("±6 m"));

        // Poor and stale both stand the reroute down: a corridor refetch triggered
        // by tunnel-mouth noise costs seconds at the worst possible moment.
        let poor = gps_quality(true, false, Some(GPS_POOR_M + 20.0), Some(0.5));
        assert!(matches!(poor, GpsQuality::Poor(_)));
        assert!(poor.unreliable());
        assert!(poor.chip().unwrap().contains("80 m"));
        let stale = gps_quality(true, false, Some(5.0), Some(GPS_STALE_S + 1.0));
        assert_eq!(stale, GpsQuality::Stale);
        assert!(stale.unreliable());
        // An explicit provider error beats every inference about the fix.
        assert_eq!(
            gps_quality(true, true, Some(5.0), Some(0.1)),
            GpsQuality::Error
        );
    }

    /// The locate button's caption must say something in **every** tracking state.
    ///
    /// This is the inversion of the chip's rule and the reason the two are
    /// separate methods. A phone cannot reach a tooltip, so the caption is the
    /// only place the button's state is legible there — and a healthy receiver
    /// falling silent is exactly the complaint the caption exists to answer: a lit
    /// button with nothing beside it looks identical whether the GPS is locked on
    /// or was refused permission ten minutes ago.
    #[test]
    fn the_locate_caption_speaks_in_every_state_a_provider_can_reach() {
        for state in [
            GpsQuality::Waiting,
            GpsQuality::Error,
            GpsQuality::Stale,
            GpsQuality::Poor(80.0),
            GpsQuality::Good(4.0),
        ] {
            assert!(
                state.caption().is_some_and(|c| !c.is_empty()),
                "{state:?} left the button with no caption"
            );
        }
        // The one silence, and it is honest: a desktop has no receiver, so an
        // accuracy there would be a number nothing measured.
        assert!(GpsQuality::NoProvider.caption().is_none());

        // The accuracy is the positive evidence — "it is working, this well".
        assert_eq!(format_accuracy(0.014), "±1 cm");
        assert_eq!(format_accuracy(0.3), "±30 cm");
        assert_eq!(format_accuracy(0.999), "±1 m");
        assert_eq!(GpsQuality::Good(4.0).caption().as_deref(), Some("±4 m"));
        assert_eq!(GpsQuality::Poor(80.0).caption().as_deref(), Some("±80 m"));
        assert_eq!(GpsQuality::Good(4.0).accuracy_m(), Some(4.0));
        assert_eq!(GpsQuality::Waiting.accuracy_m(), None);
    }

    /// A kilometre-scale accuracy is an ordinary indoor reading, not a curiosity,
    /// and "±1247 m" claims four significant digits about a confidence interval.
    #[test]
    fn a_coarse_accuracy_is_written_in_kilometres() {
        assert_eq!(format_accuracy(4.4), "±4 m");
        assert_eq!(format_accuracy(999.0), "±999 m");
        assert_eq!(format_accuracy(1247.0), "±1 km");
        assert_eq!(format_accuracy(12_400.0), "±12 km");
    }

    /// The cue ladder: once per band per manoeuvre, re-armed by the next one, and
    /// silent on the frame driver mode starts — a "Now" chime for whatever junction
    /// you happen to be beside is not a way to begin a drive.
    /// The rule that decides a write from a teleport, in the units it is written
    /// in. `GPS_TELEPORT_M` is metres and `lat_lng_to_world` answers in metres of
    /// the anchored flat frame, and a threshold that silently compared degrees —
    /// or tile fractions — would read as "always near", i.e. as exactly the
    /// avatar-warping behaviour this replaced, with nothing to show for itself.
    #[test]
    fn a_distant_fix_is_a_teleport_and_a_nearby_one_is_not() {
        // Groningen, at the zoom the map anchors on.
        let anchor = (53.2194, 6.5665, 14);
        let here = DVec3::ZERO;

        // The anchor itself is zero metres away, by definition.
        assert!(gps_gap_m(anchor, anchor.0, anchor.1, here) < 1.0);

        // ~111 m north (0.001° of latitude) is an ordinary step between two fixes
        // and must stay a position write.
        let step = gps_gap_m(anchor, anchor.0 + 0.001, anchor.1, here);
        assert!(
            (80.0..160.0).contains(&step),
            "0.001° of latitude should read as ~111 m, got {step}"
        );
        assert!(step < GPS_TELEPORT_M);

        // Amsterdam is a different city and must not be reached by writing a
        // number into the avatar's transform.
        let city = gps_gap_m(anchor, 52.3676, 4.9041, here);
        assert!(
            city > GPS_TELEPORT_M,
            "a fix 150 km away must teleport, gap was {city} m"
        );

        // Symmetric in the avatar's own position: an avatar standing 10 km east of
        // the anchor is far from a fix *at* the anchor, which is the case that
        // matters — the map is built around the anchor, not around the fix.
        let away = DVec3::new(10_000.0, 0.0, 0.0);
        assert!(gps_gap_m(anchor, anchor.0, anchor.1, away) > GPS_TELEPORT_M);
    }

    /// What goes on the wire when a device fix is applied online is the **local**
    /// half of the split, and this is the test that says so. The sim assigns a
    /// snapshot position straight into `Transform::translation`, so handing it the
    /// absolute world metres instead would look right on the client that sent it
    /// and put the avatar a whole cell edge — ten kilometres — out for the sim and
    /// for every other viewer. The two halves are only distinguishable once the
    /// position is past a cell boundary, which is exactly the case a long drive
    /// reaches and a test at the origin never would.
    #[test]
    fn the_wire_gets_the_cell_local_half_of_a_fix() {
        // Inside the first cell: the local translation *is* the world position, so
        // this arm cannot tell a correct implementation from a wrong one.
        let (cell, local) = placement(1_500.0, -2_500.0, 0.0);
        assert_eq!((cell.x, cell.z), (0, 0));
        assert!((local.x - 1_500.0).abs() < 1e-3 && (local.z + 2_500.0).abs() < 1e-3);

        // Past the first cell in both axes — the case a long drive reaches within
        // the hour, and the only one where the two halves differ at all. Cells are
        // *centred* on their index, so the local translation runs to half an edge
        // either side rather than from zero to a whole one.
        let (cell, local) = placement(23_000.0, -14_000.0, 0.0);
        assert_eq!((cell.x, cell.z), (2, -1));
        assert!(
            local.x.abs() <= WORLD_CELL_EDGE && local.z.abs() <= WORLD_CELL_EDGE,
            "a local translation may never exceed the cell edge, got {local:?}"
        );
        // …and the two halves still add back up to where the fix actually was,
        // which is the property the sim depends on: it is handed `local` alone and
        // the client keeps `cell` to itself.
        let rebuilt_x =
            f64::from(cell.x as f32).mul_add(f64::from(WORLD_CELL_EDGE), f64::from(local.x));
        let rebuilt_z =
            f64::from(cell.z as f32).mul_add(f64::from(WORLD_CELL_EDGE), f64::from(local.z));
        assert!(
            (rebuilt_x - 23_000.0).abs() < 1e-2,
            "x rebuilt to {rebuilt_x}"
        );
        assert!(
            (rebuilt_z + 14_000.0).abs() < 1e-2,
            "z rebuilt to {rebuilt_z}"
        );
    }

    /// The avatar stands on the ground, whatever the last position said. A fix
    /// carries no height, so a `y` that had gone negative (a prediction dipping
    /// through the floor before a correction) must not be handed back to the sim
    /// as an underground placement.
    #[test]
    fn a_fix_never_places_the_avatar_below_the_ground() {
        let (_, local) = placement(0.0, 0.0, -12.0);
        assert!(local.y > 0.0, "y was {}", local.y);
    }

    #[test]
    fn a_cue_fires_once_per_band_and_re_arms_at_the_next_turn() {
        let mut mem = CueMemory::default();
        let at = |step: usize, m: f64| CueInputs {
            active: true,
            step,
            band: cue_band(m, true),
            rerouting: false,
            arrived: false,
        };

        // The first driving frame is silent, whatever it finds.
        assert_eq!(next_cue(&mut mem, at(0, 60.0)), None);
        // …and having recorded that band, it does not fire it late.
        assert_eq!(next_cue(&mut mem, at(0, 55.0)), None);
        // Deeper is a cue; the same band again is not, in either direction.
        assert_eq!(next_cue(&mut mem, at(0, 10.0)), Some(Cue::Now));
        assert_eq!(next_cue(&mut mem, at(0, 9.0)), None);
        assert_eq!(
            next_cue(&mut mem, at(0, 120.0)),
            None,
            "no cue counting back up"
        );

        // The next manoeuvre re-arms the whole ladder.
        assert_eq!(
            next_cue(&mut mem, at(1, 500.0)),
            None,
            "nothing to prepare for"
        );
        assert_eq!(next_cue(&mut mem, at(1, 380.0)), Some(Cue::Prepare));
        assert_eq!(next_cue(&mut mem, at(1, 300.0)), None);
        assert_eq!(next_cue(&mut mem, at(1, 140.0)), Some(Cue::Approach));
        assert_eq!(next_cue(&mut mem, at(1, 12.0)), Some(Cue::Now));

        // A `Continue` mid-leg is not a manoeuvre and must never chime — the noded
        // graph puts one at every vertex of a curve.
        assert_eq!(cue_band(10.0, false), None);
    }

    /// The two one-off cues are edges, not states: held for as long as the reroute
    /// takes, they would otherwise fire every frame of it.
    #[test]
    fn the_reroute_and_arrival_cues_fire_once_each() {
        let mut mem = CueMemory::default();
        let frame = |rerouting, arrived| CueInputs {
            active: true,
            step: 0,
            band: None,
            rerouting,
            arrived,
        };
        assert_eq!(next_cue(&mut mem, frame(false, false)), None, "entering");
        assert_eq!(next_cue(&mut mem, frame(true, false)), Some(Cue::Reroute));
        assert_eq!(next_cue(&mut mem, frame(true, false)), None);
        assert_eq!(next_cue(&mut mem, frame(false, false)), None);
        assert_eq!(next_cue(&mut mem, frame(true, false)), Some(Cue::Reroute));

        assert_eq!(next_cue(&mut mem, frame(true, true)), Some(Cue::Arrived));
        assert_eq!(next_cue(&mut mem, frame(true, true)), None);

        // Leaving driver mode forgets everything, so the next drive starts silent
        // rather than announcing the previous trip's arrival.
        assert_eq!(
            next_cue(
                &mut mem,
                CueInputs {
                    active: false,
                    step: 0,
                    band: None,
                    rerouting: true,
                    arrived: true,
                }
            ),
            None
        );
        assert_eq!(
            next_cue(&mut mem, frame(true, true)),
            None,
            "entering again"
        );
    }

    /// Every cue must be a decodable WAV, since nothing here has a decoder to fail
    /// loudly — an unparseable clip is silence with no message anywhere.
    #[test]
    fn every_cue_is_a_well_formed_wav() {
        for cue in [
            Cue::Prepare,
            Cue::Approach,
            Cue::Now,
            Cue::Reroute,
            Cue::Arrived,
        ] {
            let wav = cue_wav(cue_notes(cue));
            assert_eq!(&wav[0..4], b"RIFF", "{cue:?}");
            assert_eq!(&wav[8..12], b"WAVE", "{cue:?}");
            let data_len = u32::from_le_bytes(wav[40..44].try_into().unwrap()) as usize;
            assert_eq!(wav.len(), 44 + data_len, "{cue:?}: header/payload disagree");
            assert_eq!(
                u32::from_le_bytes(wav[24..28].try_into().unwrap()),
                CUE_SAMPLE_RATE
            );
            // Long enough to be heard over a road, short enough not to still be
            // playing at the junction it announces.
            let secs = data_len as f32 / 2.0 / CUE_SAMPLE_RATE as f32;
            assert!((0.10..=0.60).contains(&secs), "{cue:?} runs {secs}s");
        }
        // The first sample is the attack's first step, never a full-amplitude
        // discontinuity — a click is what a driver hears as a fault.
        let wav = cue_wav(cue_notes(Cue::Now));
        assert_eq!(i16::from_le_bytes(wav[44..46].try_into().unwrap()), 0);
    }

    /// Guidance that has to be switched on before it works is guidance nobody has.
    #[test]
    fn the_guidance_cues_are_on_by_default() {
        assert!(DriverState::default().cues.0);
    }

    /// The chrome has to fit the screen it is on. At 274 pt — the viewport an
    /// Android phone reports — the card and the strip each fit the width and cannot
    /// both fit it, so anchored to opposite edges they overlapped by 56 pt.
    #[test]
    fn the_chrome_fits_every_viewport_it_is_drawn_on() {
        for width in [274.0_f32, 320.0, 400.0, 640.0, 800.0, 1280.0] {
            let compact = width < crate::systems::egui_manager::COMPACT_WIDTH_PTS;
            let l = driver_layout(width, compact);
            assert!(
                l.card_width <= width - 2.0 * CARD_MARGIN + 1e-3,
                "the card overflows {width} pt"
            );
            assert!(
                l.strip_width <= width - 2.0 * CARD_MARGIN + 1e-3,
                "the strip overflows {width} pt"
            );
            // Either they are side by side with room to spare, or the strip has
            // been stacked clear of the card.
            let side_by_side =
                CARD_MARGIN + l.card_width <= width - CARD_MARGIN - l.strip_width + 1e-3;
            let stacked = l.strip_top >= CARD_MARGIN + CARD_HEIGHT;
            assert!(
                side_by_side || stacked,
                "card and strip overlap at {width} pt: card {} strip {} top {}",
                l.card_width,
                l.strip_width,
                l.strip_top
            );
        }
        // The desktop layout is unchanged: full-size boxes, both at the top.
        let wide = driver_layout(1280.0, false);
        assert_eq!(wide.card_width, CARD_WIDTH);
        assert_eq!(wide.strip_width, STRIP_WIDTH);
        assert_eq!(wide.strip_top, CARD_MARGIN);
        assert_eq!(wide.rail_height, RAIL_HEIGHT);
    }

    /// The GPS chip must not share a y-band with the Re-centre button, which is
    /// anchored to the opposite bottom corner — on a narrow viewport that is an
    /// overlap, not a near miss.
    #[test]
    fn the_gps_chip_clears_the_bottom_row() {
        assert!(
            CHIP_BOTTOM > PILL_BOTTOM + 24.0,
            "the chip sits in the Re-centre button's row"
        );
    }

    /// The whole point of the glide: between two fixes the marker must cover the
    /// ground *evenly*. An exponential ease passes every "it gets there" test and
    /// still lurches, so this asserts the per-frame steps are alike — which is the
    /// property a person actually sees.
    #[test]
    fn a_glide_spreads_one_fix_evenly_over_the_frames_after_it() {
        let mut glide = FixGlide::default();
        let mut t = Instant::now();
        let dt = 1.0 / 60.0;
        let step = Duration::from_secs_f32(dt);

        // The first fix of a session is placed outright, so pressing the button
        // still centres rather than sliding in from wherever the avatar stood.
        assert_eq!(glide.advance((0.0, 0.0), true, t, dt), (0.0, 0.0));

        // A second fix, a second later and 10 m east. It teaches the glide the
        // cadence *and* is the first one gliding applies to: from here the marker
        // is aimed to arrive as the next report is due.
        t += Duration::from_secs_f32(1.0);
        let mut x = glide.advance((10.0, 0.0), true, t, dt).0;
        let first_step = x;
        assert!(
            first_step > 0.0 && first_step < 0.5,
            "one frame took {first_step} m of a 10 m fix — that is the lurch this exists to remove"
        );
        for frame in 1..30 {
            t += step;
            let next = glide.advance((10.0, 0.0), false, t, dt).0;
            let moved = next - x;
            assert!(
                (moved - first_step).abs() < first_step * 0.5,
                "frame {frame} moved {moved} m against a first frame of {first_step} m"
            );
            x = next;
        }
        assert!(
            x > 4.0 && x < 10.0,
            "half a second in, the marker should be about half way, not {x}"
        );

        // Once the next fix is overdue the marker sits on the last known position
        // rather than inventing one — a receiver that has gone quiet is a receiver
        // that has gone quiet.
        t += Duration::from_secs_f32(1.0);
        assert_eq!(glide.advance((10.0, 0.0), false, t, dt).0, 10.0);
    }

    /// A re-anchor and a teleport both arrive here as an enormous jump in the
    /// target. Gliding through one would sail the marker across the map.
    #[test]
    fn a_glide_arrives_rather_than_travelling_an_impossible_step() {
        let mut glide = FixGlide::default();
        let mut t = Instant::now();
        let dt = 1.0 / 60.0;
        glide.advance((0.0, 0.0), true, t, dt);
        t += Duration::from_secs_f32(1.0);
        glide.advance((5.0, 0.0), true, t, dt);
        t += Duration::from_secs_f32(1.0);
        // A whole grid cell away: not travel at any speed a vehicle reaches.
        assert_eq!(glide.advance((10_000.0, 0.0), true, t, dt), (10_000.0, 0.0));
    }

    /// A reset must make the next fix a placement again, or the marker glides in
    /// from a coordinate that belonged to a frame which no longer exists.
    #[test]
    fn a_reset_makes_the_next_fix_a_placement() {
        let mut glide = FixGlide::default();
        let t = Instant::now();
        let dt = 1.0 / 60.0;
        glide.advance((0.0, 0.0), true, t, dt);
        glide.reset();
        assert_eq!(glide.advance((60.0, 40.0), true, t, dt), (60.0, 40.0));
    }
}

/// Mirror the user's guidance preference into driver mode.
///
/// `DriverState` owns the flag the cue system reads, because that system runs
/// deep inside the driving tick and taking a second resource there would push it
/// over Bevy's system-parameter limit. So the preference is copied in rather than
/// read through.
pub fn sync_guidance_pref(prefs: Res<crate::resources::AppPrefs>, mut driver: ResMut<DriverState>) {
    if !prefs.is_changed() {
        return;
    }
    driver.cues = GuidanceSound(prefs.voice_guidance);
}
