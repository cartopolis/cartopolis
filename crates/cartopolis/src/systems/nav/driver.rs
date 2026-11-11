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
//! The layout is Organic Maps' (2026-11-02, the reference the user named after
//! the satnav layout was judged not pretty): one turn card across the top — the
//! manoeuvre on a sign-coloured plate, the distance, the street — and one trip bar
//! across the bottom with Stop, the arrival clock, time and distance left, and the
//! route menu, the speed in a disc above its left end. The map between them is
//! left to the map. It replaced a satnav arrangement with a route rail of services
//! down the right edge, which put a column of badges beside a column of numbers
//! and read as a dashboard; the reference puts nothing on the map but the route.
//! The services corridor query ([`crate::systems::route_ahead`]) still runs, for
//! the speed limit it carries.
//!
//! **Overview** pulls the rig back until the rest of the route is on screen
//! ([`OverviewFrame`]): judging a route mid-drive is part of driving it.
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
//! **Stop is always reachable.** It is on the trip bar, and the bar stands down
//! behind any docked panel — so while one is open the card, the only box always
//! drawn, carries Stop in its last row. A phone has no Escape key, and a drive
//! with no exit was reached once already. The bar's menu reopens the planner
//! without leaving the mode, which is otherwise unreachable mid-drive: the dock,
//! the main menu and the planner are all suppressed.
//!
//! The speed disc carries the posted limit over it, as a painted road sign
//! rather than a themed readout ([`speed_limit_sign`]) — same reasoning as
//! [`DRIVE_BLUE`]. It is deliberately absent where OSM has no `maxspeed`: the
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
use cartopolis_core::motion;
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
use crate::systems::route_ahead::RouteAhead;
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
/// The arrow's dark rim, as a scale of the kite it surrounds, and how far the
/// white face floats over it ([`arrow_mesh`]). A few centimetres is thousands of
/// depth quanta at chase range, so the two never fight.
const ARROW_RIM_SCALE: f32 = 1.3;
const ARROW_FACE_LIFT: f32 = 0.06;
/// Face and rim, as linear vertex colours (the material is white and unlit, so
/// these are what is drawn). The rim is the route casing's navy.
const ARROW_FACE_RGBA: [f32; 4] = [1.0, 1.0, 1.0, 1.0];
const ARROW_RIM_RGBA: [f32; 4] = [0.006, 0.040, 0.150, 1.0];

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

/// How far ahead along the route the snapped arrow takes its bearing from.
const HEADING_AHEAD_M: f64 = 12.0;

/// How fast the drawn arrow turns toward its target heading, per second. Quick
/// enough to follow a junction turn within a few tenths of a second; slow enough
/// that a jump between the snapped and the measured yaw is a turn, not a cut.
const ARROW_TURN_RATE: f32 = 10.0;

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
    ///
    /// **The default**, since driving keeps the 3D buildings (2026-11-02): from
    /// the tilted 3D view at 22° the first facade beside the street filled the
    /// screen and hid the route, the arrow and the junction outright. Seeing down
    /// a street between 20 m buildings takes a camera looking down steeper than
    /// about 70°, which this one (77°) is.
    #[default]
    HeadingUp,
    /// The map is held north-up and the arrow turns instead — for reading the map
    /// as a map.
    NorthUp,
    /// Heading-up, dropped behind and tilted into the road: the "3D" view a driver
    /// reads an open junction from — and the one the buildings block in a city.
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

/// What the route overview frames: the box around the vehicle and the rest of
/// the route, in world-XZ metres from the vehicle.
///
/// Fitted by the rig rather than by a teleport: driver mode holds the camera on
/// the vehicle (Follow, `drive_locked`), and a framing teleport there would take
/// the avatar with it. So the overview is the chase rig pulled back and laid
/// nearly flat, looking at the box's centre — the same look-ahead vector every
/// orientation already aims through.
#[derive(Clone, Copy, Debug, PartialEq)]
struct OverviewFrame {
    /// The box's centre, from the vehicle.
    centre: Vec2,
    /// Half the box's width and depth.
    half: Vec2,
}

/// How flat the overview looks down, in radians (~83°). Not quite nadir, so a
/// heading-up overview still says which way is ahead.
const OVERVIEW_PITCH: f32 = 1.45;
/// Margin round the fitted route, and the furthest the overview pulls back.
const OVERVIEW_MARGIN: f32 = 1.15;
const OVERVIEW_MAX_DISTANCE: f32 = 25_000.0;

impl OverviewFrame {
    fn around(points: impl Iterator<Item = Vec2>) -> Option<Self> {
        let (mut lo, mut hi) = (Vec2::splat(f32::MAX), Vec2::splat(f32::MIN));
        for p in points {
            lo = lo.min(p);
            hi = hi.max(p);
        }
        (lo.x <= hi.x).then(|| Self {
            centre: (lo + hi) * 0.5,
            half: (hi - lo) * 0.5,
        })
    }

    /// Rig distance that fits the box on a viewport of `aspect` (width over
    /// height), for a 45° vertical field of view.
    ///
    /// Fitted on the box's radius, so it holds whichever way the heading-up
    /// view is turned; and the camera stands over the vehicle rather than over
    /// the centre, so the centre's offset is added to what has to fit.
    fn distance(self, aspect: f32) -> f32 {
        let tan_half = (std::f32::consts::FRAC_PI_8).tan();
        let radius = self.half.length() + 0.25 * self.centre.length();
        let fit = radius / (tan_half * aspect.clamp(0.3, 1.0));
        (fit * OVERVIEW_MARGIN).clamp(FLAT_DISTANCE, OVERVIEW_MAX_DISTANCE)
    }
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
    /// The route overview is up: the rig pulls back and flattens until the rest
    /// of the route is on screen ([`OverviewFrame`]). A toggle, not a peek — it
    /// stays until the driver taps it away, because judging the route (is that
    /// the motorway, where does it cross the river) is not a glance.
    pub overview: bool,
    /// What the overview has to fit, measured this frame by `drive_driver_mode`.
    overview_frame: Option<OverviewFrame>,
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
    /// …and with the overview up once it does (the `--ui overview` preset).
    armed_overview: bool,
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
        self.overview = false;
        self.overview_frame = None;
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

    /// Arm with the route overview up: entering resets the overview, so it has to
    /// be asked for on the far side of the edge.
    pub fn arm_overview(&mut self) {
        self.armed_overview = true;
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
        self.armed_overview = false;
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
        self.overview = false;
        self.overview_frame = None;
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
    nearest_in_window(points, cum, at, (f64::NEG_INFINITY, f64::INFINITY))
}

/// How far behind and ahead of the last projection [`project_near`] looks before
/// it gives up and scans the whole route.
///
/// Wide enough for a second of motorway driving many times over and for a fix
/// that jumped after a tunnel, which is all a position does between two frames.
const NEAR_BEHIND_M: f64 = 300.0;
const NEAR_AHEAD_M: f64 = 800.0;

/// [`nearest_on_polyline`], searching only the stretch of route around `hint`
/// metres along — the last frame's answer — and falling back to the whole line
/// when nothing in that window is within [`OFF_ROUTE_M`].
///
/// Two reasons, and the second is the one a driver sees. A whole-route scan is
/// every segment every frame, a `cos()` each, which is real time on a phone
/// once a route is a few thousand points long. And a global nearest jumps: on a
/// cloverleaf, or where the route comes back along the next street over, the
/// closest segment can be a leg ten minutes ahead, and the card, the rail and
/// the ETA all jump there with it for as long as the two lie closer than the
/// fix's noise.
pub(crate) fn project_near(geom: &RouteGeom, at: [f64; 2], hint: Option<f64>) -> (f64, f64) {
    let Some(hint) = hint else {
        return nearest_on_polyline(&geom.points, &geom.cum, at);
    };
    let window = (hint - NEAR_BEHIND_M, hint + NEAR_AHEAD_M);
    let near = nearest_in_window(&geom.points, &geom.cum, at, window);
    if near.1 <= OFF_ROUTE_M {
        near
    } else {
        nearest_on_polyline(&geom.points, &geom.cum, at)
    }
}

/// The projection onto the part of the polyline between `window.0` and
/// `window.1` metres along it.
///
/// Clipped to the window *within* a segment as well, not only by which segments
/// are visited: one long straight that merely starts inside the window would
/// otherwise let its far end — the stretch the window exists to exclude — win.
fn nearest_in_window(
    points: &[[f64; 2]],
    cum: &[f64],
    at: [f64; 2],
    window: (f64, f64),
) -> (f64, f64) {
    let mut best = (0.0, f64::MAX);
    let segments = points
        .len()
        .saturating_sub(1)
        .min(cum.len().saturating_sub(1));
    // Segment `i` spans `cum[i]..cum[i + 1]`: the first that ends inside the
    // window, through the last that starts inside it.
    let first = cum.partition_point(|&c| c < window.0).saturating_sub(1);
    let last = cum.partition_point(|&c| c <= window.1).min(segments);
    for i in first.min(last)..last {
        let (a, b) = (points[i], points[i + 1]);
        // Work in local metres about `a`, so the projection is a plain 2D one.
        let mid = (a[0] + b[0]) * 0.5;
        let mx = 111_320.0 * mid.to_radians().cos();
        let seg = ((b[1] - a[1]) * mx, (b[0] - a[0]) * 110_540.0);
        let pt = ((at[1] - a[1]) * mx, (at[0] - a[0]) * 110_540.0);
        let len2 = seg.0 * seg.0 + seg.1 * seg.1;
        let len = len2.sqrt();
        let (t_lo, t_hi) = if len > 1e-6 {
            (
                ((window.0 - cum[i]) / len).max(0.0),
                ((window.1 - cum[i]) / len).min(1.0),
            )
        } else {
            (0.0, 1.0)
        };
        if t_lo > t_hi {
            continue;
        }
        let t = if len2 > 1e-9 {
            ((pt.0 * seg.0 + pt.1 * seg.1) / len2).clamp(t_lo, t_hi)
        } else {
            0.0
        };
        let (dx, dy) = (pt.0 - seg.0 * t, pt.1 - seg.1 * t);
        let off = (dx * dx + dy * dy).sqrt();
        if off < best.1 {
            best = (cum[i] + t * len, off);
        }
    }
    best
}

/// Where a position sits on the route, and what it still has to do.
///
/// Pure over the measured geometry so the arithmetic that produces the bottom bar
/// is testable without a camera, a window, or a fix.
pub(crate) fn project_onto_route(geom: &RouteGeom, route: &Route, at: [f64; 2]) -> Progress {
    project_onto_route_near(geom, route, at, None)
}

/// [`project_onto_route`], searching around `hint` metres along first
/// ([`project_near`]) — what a tick that measured the route last frame passes.
pub(crate) fn project_onto_route_near(
    geom: &RouteGeom,
    route: &Route,
    at: [f64; 2],
    hint: Option<f64>,
) -> Progress {
    let (along, off) = project_near(geom, at, hint);
    let remaining_m = (geom.total_m - along).max(0.0);
    // Time is scaled from the planner's own answer rather than from the current
    // speed: the planner knows the speed limits of the road ahead, and a driver
    // stopped at a light does not have an infinite ETA.
    let fraction = if geom.total_m > 1.0 {
        remaining_m / geom.total_m
    } else {
        0.0
    };
    // The departure is never the step being driven toward: it sits at 0 m, so
    // choosing it put "◯ Now" on the card for the first frames of every drive —
    // an instruction to do nothing, at the one moment the driver looks for the
    // first real turn.
    let step = geom
        .step_along
        .iter()
        .zip(&route.steps)
        .position(|(&s, step)| s > along - STEP_PASSED_M && step.maneuver != Maneuver::Depart)
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
        driver.overview = std::mem::take(&mut driver.armed_overview);
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
        // Distances along the old line mean nothing on the new one, so the next
        // projection must not search around one ([`project_near`]).
        driver.progress = None;
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
        let reach = fix_reach(anchor, fix, &avatars);
        if reach == FixReach::Doubtful {
            // Far but unsure of itself: wait for a better fix (`FixReach`).
            if fix_is_new {
                tracing::info!(
                    accuracy_m = fix.accuracy_m,
                    "device fix is far but too uncertain to jump on — waiting for a better one"
                );
            }
        } else if reach == FixReach::Far {
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
                anchor,
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
        .map(|g| project_onto_route_near(g, r, [lat, lng], driver.progress.map(|p| p.along_m)));

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
        let (on, _) = point_along(geom, p.along_m)?;
        // The road's direction a few metres on, not the current segment's: the
        // noded graph cuts a street into sub-metre pieces, and an arrow taking each
        // one's bearing twitches at every vertex it crosses.
        let (ahead, _) = point_along(geom, p.along_m + HEADING_AHEAD_M)?;
        let ahead = if ahead == on {
            point_along(geom, p.along_m)?.1
        } else {
            ahead
        };
        let (sx, sz) = lat_lng_to_world(anchor, on[0], on[1]);
        let (ax, az) = lat_lng_to_world(anchor, ahead[0], ahead[1]);
        let seg_yaw = f32::atan2((ax - sx) as f32, (az - sz) as f32);
        let offset = Vec2::new((sx - here.x) as f32, (sz - here.z) as f32);
        Some((offset, snapped_yaw(driver.course, seg_yaw)))
    });
    driver.snap = snap;

    // What the overview has to fit: the rest of the route, around the vehicle.
    // Measured every frame (it is a few hundred points, thinned) so the overview
    // shrinks as the route is driven, and only while it is up.
    driver.overview_frame = if driver.overview {
        let along = progress.map_or(0.0, |p| p.along_m);
        driver.geom.as_ref().and_then(|g| {
            let start = g.cum.partition_point(|&c| c < along);
            let ahead = g.points.get(start..)?;
            let stride = (ahead.len() / 300).max(1);
            let pts = ahead.iter().step_by(stride).chain(ahead.last()).map(|p| {
                let (x, z) = lat_lng_to_world(anchor, p[0], p[1]);
                Vec2::new((x - here.x) as f32, (z - here.z) as f32)
            });
            OverviewFrame::around(std::iter::once(Vec2::ZERO).chain(pts))
        })
    } else {
        None
    };
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
    anchor: (f64, f64, u32),
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
        // As a place on Earth (protocol 47): the fix's own latitude and
        // longitude, through the glide and back.
        let (latitude, longitude) = world_to_lat_lng(anchor, wx, wz);
        sess.send(
            "device fix",
            cartopolis_core::NetMessage::Teleport {
                request_id: 0,
                geo: cartopolis_core::GeoAnchor {
                    latitude,
                    longitude,
                },
                alt_m: local.y,
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
    /// Where the marker was drawn last frame, if anywhere — what a caller measures
    /// this frame's step from.
    pub(crate) fn shown(&self) -> Option<(f64, f64)> {
        self.shown
    }

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
/// the local translation that stand for it, at the height `y` the avatar already
/// has — a fix carries no height, so it moves the avatar across the ground and
/// leaves its height alone.
///
/// **The floor is the sim's own, [`motion::GROUND_Y`], and not a centimetre above
/// it.** It was `0.05` — the fox-era `GROUND_HEIGHT` — while the sim and the
/// prediction stand an avatar at exactly zero. So every frame of a GPS walk put the
/// avatar five centimetres up, `network::send_network_intent` predicted the fall
/// from there, and the frame that fall crossed the floor reported a landing.
/// At 120 Hz that is a 6 m/s touchdown several times a second: the landing thud
/// and the camera's landing dip, over and over, for as long as the phone moved.
///
/// Pure, and split out of [`place_avatar`] for one reason: the local half is what
/// goes on the wire, and a version of this that handed the sim `wx`/`wz` instead
/// would look right, place the avatar correctly on the client that sent it, and
/// be a cell edge — ten kilometres — out for the sim and for everyone else.
#[must_use]
fn placement(wx: f64, wz: f64, y: f32) -> (GridCell, Vec3) {
    let grid = Grid::new(WORLD_CELL_EDGE, WORLD_SWITCH_THRESHOLD);
    grid.translation_to_grid(DVec3::new(wx, f64::from(y.max(motion::GROUND_Y)), wz))
}

/// What a fix asks of the avatar: a write, a jump, or nothing yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FixReach {
    /// Within [`GPS_TELEPORT_M`]: write the position (`place_avatar`).
    Near,
    /// Further, and sure of it: teleport.
    Far,
    /// Further, but the fix's own uncertainty is at least half the gap — a cold
    /// network fix, or a cached one widened by its age (`stale_accuracy_m`). It
    /// says too little about where the device is to move the person kilometres
    /// on, so it is left for the next, better one.
    Doubtful,
}

/// Is this fix further from the avatar than [`GPS_TELEPORT_M`], and sure enough
/// of itself to jump on?
///
/// Both sides are absolute world metres in the anchored flat frame — cell edge
/// plus local translation for the avatar, [`lat_lng_to_world`] for the fix — so
/// they are directly comparable and a cell boundary between them is not a jump.
/// An avatar that cannot be read at all counts as far: there is nothing to write
/// a position into, and a teleport is the operation that creates the state the
/// write would need.
///
/// A far jump also wants the fix to be sure of itself (`accuracy_m` under half
/// the gap). Every far fix used to jump, silently, so a stale cached position
/// could carry the avatar off to wherever the phone last was, which reads as the
/// avatar landing somewhere at random.
pub(crate) fn fix_reach(
    anchor: (f64, f64, u32),
    fix: GpsFix,
    avatars: &Query<(&mut GridCell, &mut Transform), (With<Avatar>, Without<FreeCamera>)>,
) -> FixReach {
    let Ok((cell, tf)) = avatars.single() else {
        return FixReach::Far;
    };
    let edge = f64::from(WORLD_CELL_EDGE);
    let here = DVec3::new(
        (cell.x as f64).mul_add(edge, f64::from(tf.translation.x)),
        0.0,
        (cell.z as f64).mul_add(edge, f64::from(tf.translation.z)),
    );
    reach_of(gps_gap_m(anchor, fix.lat, fix.lng, here), fix.accuracy_m)
}

/// [`fix_reach`]'s decision, from the gap and the fix's accuracy alone.
fn reach_of(gap_m: f64, accuracy_m: f64) -> FixReach {
    if gap_m <= GPS_TELEPORT_M {
        FixReach::Near
    } else if accuracy_m.is_finite() && accuracy_m * 2.0 < gap_m {
        FixReach::Far
    } else {
        FixReach::Doubtful
    }
}

/// Ground distance, in metres, between a `[lat, lng]` and a point already in the
/// anchored world frame. Split out of [`fix_reach`] so the units — the one thing
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
    // The overview replaces the chase pose outright: pulled back to fit the rest
    // of the route, laid nearly flat, aimed at the middle of it. Peeking is
    // meaningless over it, so a gesture is read (to keep the drag anchor honest)
    // and then dropped.
    let overview = driver
        .overview_frame
        .filter(|_| driver.overview)
        .map(|frame| {
            let v = egui.viewport_pts();
            let aspect = if v.y > 0.0 { v.x / v.y } else { 1.0 };
            (frame.distance(aspect), frame.centre)
        });
    if overview.is_some() {
        driver.peek = Peek::default();
    }
    let distance = match overview {
        Some((d, _)) => d,
        None => base * rig_scale(driver.speed_ms, driver.progress) * driver.peek.zoom,
    };
    let pitch = match overview {
        Some(_) => OVERVIEW_PITCH,
        None => (base_pitch + driver.peek.pitch).clamp(0.12, 1.50),
    };
    camera_state.distance += (distance - camera_state.distance) * ease;
    camera_state.pitch += (pitch - camera_state.pitch) * ease;
    // Aim down the road the vehicle is on, in world XZ. Along the **course**, not
    // along the camera's forward: the two agree in the heading-up views and do not
    // in north-up, where the camera holds north and aiming along it pushed the
    // look point north while you drove south — the road ahead then ran off the
    // bottom of the screen. Eased through the offset itself, so switching
    // orientation slides the aim point along with the tilt instead of snapping the
    // horizon.
    let aim = match overview {
        Some((_, centre)) => centre,
        None => {
            Vec2::new(driver.course.sin(), driver.course.cos()) * camera_state.distance * lookahead
        }
    };
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
    time: Res<Time>,
    mut drawn_yaw: Local<Option<f32>>,
    camera_state: Res<CameraState>,
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
        *drawn_yaw = None;
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
                // White, so the mesh's vertex colours are what is drawn
                // (`arrow_mesh`: a white face on a navy rim).
                base_color: Color::WHITE,
                unlit: true,
                // Not hazed into the view like the city around it: at the 3D
                // view's shallow angle the fog had already greyed the arrow
                // into the street it stands on. Same for the route line.
                fog_enabled: false,
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
    // Eased toward the target rather than set: the target switches between the
    // road's bearing and the measured course as the snap engages and lets go, and
    // the measured course itself steps once a fix. Shortest way round.
    let yaw = match *drawn_yaw {
        Some(prev) => {
            let mut delta = (yaw - prev).rem_euclid(std::f32::consts::TAU);
            if delta > std::f32::consts::PI {
                delta -= std::f32::consts::TAU;
            }
            let k = 1.0 - (-ARROW_TURN_RATE * time.delta_secs()).exp();
            prev + delta * k
        }
        None => yaw,
    };
    *drawn_yaw = Some(yaw);
    let rotation = Quat::from_rotation_y(yaw);
    // Grown with the rig once it pulls back past the flat chase distance — the
    // overview looks down from kilometres, where a 14 m kite is a speck — and
    // never shrunk below its own size.
    let scale = Vec3::splat((camera_state.distance / FLAT_DISTANCE).max(1.0));
    let translation = tf.translation + offset + Vec3::Y * ARROW_Y;
    match arrows.single_mut() {
        Ok((_, mut acell, mut atf)) => {
            *acell = *cell;
            atf.translation = translation;
            atf.rotation = rotation;
            atf.scale = scale;
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
                    scale,
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
///
/// **White on a dark rim, in one mesh by vertex colour.** It was a blue kite, a
/// shade off the route's own blue and lying on top of it, so the one thing a
/// driver looks for on the map was the one thing that did not stand out from
/// it. Every satnav draws the vehicle as the brightest mark on screen with a
/// dark edge; the rim is what separates it from the route line *and* from a
/// pale street. The face sits [`ARROW_FACE_LIFT`] over the rim rather than
/// fighting it at the same height.
fn arrow_mesh() -> Mesh {
    let mut positions: Vec<[f32; 3]> = Vec::new();
    let mut colors: Vec<[f32; 4]> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();
    for (scale, y, color) in [
        (ARROW_RIM_SCALE, 0.0, ARROW_RIM_RGBA),
        (1.0, ARROW_FACE_LIFT, ARROW_FACE_RGBA),
    ] {
        let (l, w) = (ARROW_HALF_LENGTH * scale, ARROW_HALF_WIDTH * scale);
        // Nose, left tail, the notch between them, right tail. The rim is the
        // same kite grown about its centre, nudged back so its nose leads the
        // face's by as much as its tails trail.
        let shift = -(scale - 1.0) * 0.14 * ARROW_HALF_LENGTH;
        let points = [
            Vec2::new(0.0, l + shift),
            Vec2::new(-w, -0.72 * l + shift),
            Vec2::new(0.0, -0.30 * l + shift),
            Vec2::new(w, -0.72 * l + shift),
        ];
        // Two triangles about the notch, each wound to face +Y or it is
        // backface-culled into invisibility — the trap
        // `navigation::ribbon_faces_up` guards for the route line.
        for (a, b, c) in [
            (points[0], points[1], points[2]),
            (points[0], points[2], points[3]),
        ] {
            let up = (b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x) < 0.0;
            let (b, c) = if up { (b, c) } else { (c, b) };
            let base = positions.len() as u32;
            for p in [a, b, c] {
                positions.push([p.x, y, p.y]);
                colors.push(color);
            }
            indices.extend([base, base + 1, base + 2]);
        }
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
    .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, colors)
    .with_inserted_indices(Indices::U32(indices))
}

// ─── The navigation chrome ───────────────────────────────────────────────────
//
// Laid out the way Organic Maps lays out its navigation, which is the reference
// the user named when the satnav layout was judged not pretty (2026-11-02): one
// turn card across the top, one trip bar across the bottom, the speed in a disc
// above the bar's left end — and the map between them left to the map. The rail
// of services that ran down the right edge is gone: a column of P badges beside a
// column of numbers is what made the screen read as a dashboard, and nothing in
// the reference stands on the map but the route.

/// Width of the turn card on a wide viewport. On a compact one it spans the
/// screen less its margins, as the reference's does on a phone.
pub const CARD_WIDTH: f32 = 340.0;
/// The card's height — fixed, and the same in every state, because `hud` stacks
/// the compass under it from this number and a card that grew with its words
/// would slide the compass about mid-drive.
pub const CARD_HEIGHT: f32 = 96.0;
/// Where the card sits, and therefore where everything under it stacks from.
pub const CARD_MARGIN: f32 = 12.0;

/// The manoeuvre plate at the card's left edge: the turn glyph, big, on a
/// sign-coloured square — the one part of the card read without reading.
const TILE: f32 = 64.0;

/// The trip bar along the bottom: as wide as the screen allows, up to this.
const BAR_MAX_WIDTH: f32 = 520.0;
/// Its height, which everything stacked above it (the speed disc, the overview
/// button, the GPS note) clears.
const BAR_HEIGHT: f32 = 62.0;

/// The speed disc's diameter.
const SPEED_DISC: f32 = 58.0;

/// How close the manoeuvre after this one has to be before the card previews it.
/// Two turns in quick succession is the case a card showing only the next one gets
/// wrong — you take the first and are already in the second before it appears.
const THEN_PREVIEW_M: f64 = 300.0;

/// Tolerance before the speedometer calls you over the limit, in km/h. Speed is
/// measured from successive positions and the limit comes off a map; flashing red
/// at 51 in a 50 on that evidence would be noise.
const OVER_LIMIT_TOLERANCE_KMH: f32 = 4.0;

/// Where the boxes go, for a viewport width.
struct DriverLayout {
    card_width: f32,
    bar_width: f32,
}

/// Fit the chrome to the screen it is on. A pure function because "does the
/// chrome fit" is arithmetic, and the machine this is built on cannot look.
fn driver_layout(viewport_x: f32, compact: bool) -> DriverLayout {
    let usable = (viewport_x - 2.0 * CARD_MARGIN).max(80.0);
    DriverLayout {
        card_width: if compact {
            usable
        } else {
            CARD_WIDTH.min(usable)
        },
        bar_width: BAR_MAX_WIDTH.min(usable),
    }
}

/// How far the trip bar's bottom edge clears the viewport's.
fn bar_bottom() -> f32 {
    theme::OSD_MARGIN + 4.0
}

/// Where the things stacked over the bar's two ends sit: their bottom edge.
fn above_bar() -> f32 {
    bar_bottom() + BAR_HEIGHT + theme::OSD_GAP
}

/// The navigation chrome: the turn card at the top, the trip bar at the bottom,
/// the speed above the bar's left end, Overview / Re-centre above its right.
/// The compass is drawn by [`crate::systems::hud`], which already owns it;
/// driver mode only moves it down past the card.
///
/// **Every box here pins its own width.** An anchored, size-to-content
/// [`egui::Area`] whose content reaches for the *available* width grows a little
/// each frame and stretches across the screen (`map-chrome.md`).
///
/// **The bar stands down behind a docked panel, and Stop moves onto the card.**
/// egui paints floating `Area`s above panels, so a bar left up would be drawn
/// over the planner's turn list or the Places panel; but Stop has to stay
/// reachable on a phone, which has no Escape key. So while a panel is open the
/// card grows a Stop button in its last row, in the height it always reserves.
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
    // Every number on this screen is a function of where the vehicle is, and
    // driving produces no egui input at all. Without this the HUD is tessellated
    // once and then re-presented unchanged until something is tapped
    // (`EguiManager::animating`).
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
    let viewport_x = egui.viewport_pts().x;
    let DriverLayout {
        card_width,
        bar_width,
    } = driver_layout(viewport_x, egui.compact());
    let usable = (viewport_x - 2.0 * CARD_MARGIN).max(80.0);
    // `RightPanel` is the shared state; the two booleans are the older panels
    // that only publish their own (`hud` ORs the same three for the same reason).
    let panel_open = data_layers.panel_open
        || bookmarks.panel_open
        || !right_panel.is(crate::resources::RightPanel::None);
    let ctx = egui.ctx().clone();

    let mut done = false;
    let mut retry = false;
    let mut open_planner = false;
    let mut toggle_overview = false;
    let mut recentre = false;

    // ── The turn card ──
    //
    // The plate's colour is the state, in signage colours rather than the app's:
    // blue for an instruction, amber while the route is being replaced (leaving
    // the line is the normal case, and the display is already fixing it), red for
    // what it cannot fix, green for being there. The card itself is the app's
    // glass, so it reads as part of the same screen as everything else.
    let (plate, glyph, title, sub): (egui::Color32, &str, String, String) = if arrived {
        (
            ARRIVE_GREEN,
            icons::FINISH,
            "Arrived".to_string(),
            route.to.label.trim().to_string(),
        )
    } else if let Some(err) = &failed {
        (
            OFF_ROUTE_RED,
            icons::WARNING,
            "No new route".to_string(),
            err.clone(),
        )
    } else if rerouting {
        (
            REROUTE_AMBER,
            icons::REFRESH,
            "Rerouting…".to_string(),
            "from where you are".to_string(),
        )
    } else if off_route {
        // Reached only when a reroute cannot be asked for yet — the cooldown, or
        // a fix too poor to act on.
        (
            OFF_ROUTE_RED,
            icons::WARNING,
            "Off route".to_string(),
            format!("{} from the route", format_distance(progress.off_route_m)),
        )
    } else {
        let maneuver = step.map_or(Maneuver::Continue, |s| s.maneuver);
        (
            DRIVE_BLUE,
            icons::maneuver(maneuver),
            countdown(progress.step_distance_m),
            step.and_then(|s| s.name.clone()).unwrap_or_default(),
        )
    };
    let instructing = !arrived && failed.is_none() && !rerouting && !off_route;
    let then = r
        .steps
        .get(progress.step + 1)
        .filter(|_| instructing && next_step_gap(r, progress) <= THEN_PREVIEW_M)
        .map(|s| s.maneuver);

    egui::Area::new(egui::Id::new("driver_maneuver"))
        .anchor(egui::Align2::LEFT_TOP, egui::vec2(CARD_MARGIN, CARD_MARGIN))
        .show(&ctx, |ui| {
            let frame = theme::overlay_frame().inner_margin(egui::Margin::same(10));
            let inset = frame.inner_margin.sum() + egui::Vec2::splat(2.0 * frame.stroke.width);
            frame.show(ui, |ui| {
                ui.set_width(card_width - inset.x);
                ui.set_height(CARD_HEIGHT - inset.y);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 12.0;
                    let (tile, _) =
                        ui.allocate_exact_size(egui::Vec2::splat(TILE), egui::Sense::hover());
                    ui.painter().rect_filled(tile, 12.0, plate);
                    ui.painter().text(
                        tile.center(),
                        egui::Align2::CENTER_CENTER,
                        glyph,
                        egui::FontId::proportional(40.0),
                        theme::p().cream,
                    );
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 2.0;
                        ui.horizontal(|ui| {
                            ui.label(big(
                                &title,
                                if instructing { 30.0 } else { 22.0 },
                                theme::p().cream,
                            ));
                            // The turn after this one, when it follows at once —
                            // right-aligned, a footnote to the instruction.
                            if let Some(next) = then {
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        ui.spacing_mut().item_spacing.x = 3.0;
                                        ui.label(
                                            egui::RichText::new(icons::maneuver(next))
                                                .size(18.0)
                                                .color(theme::p().text),
                                        );
                                        ui.label(small("then", theme::p().text_dim));
                                    },
                                );
                            }
                        });
                        if !sub.is_empty() {
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(sub.as_str())
                                        .size(theme::SIZE_BODY)
                                        .color(theme::p().text),
                                )
                                .truncate(),
                            );
                        }
                        let buttons = failed.is_some() || panel_open;
                        if buttons {
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 6.0;
                                if failed.is_some()
                                    && theme::action_button(ui, "Retry")
                                        .on_hover_text("Ask for a route from here again")
                                        .clicked()
                                {
                                    retry = true;
                                }
                                if panel_open
                                    && theme::action_button(
                                        ui,
                                        if arrived { "Done" } else { "Stop" },
                                    )
                                    .on_hover_text("Leave navigation and put the map back")
                                    .clicked()
                                {
                                    done = true;
                                }
                            });
                        } else if instructing {
                            approach_bar(ui, progress.step_distance_m);
                        }
                    });
                });
            });
        });

    // Nothing else while a panel is docked: the bar, the speed and the buttons
    // above the bar are all on the bottom edge the planner sheet rises from, and
    // the card carries Stop meanwhile.
    if !panel_open {
        // ── The trip bar, bottom centre ──
        let ((hh, mm, _), local_clock) = clock.hms();
        let arrival = arrival_clock(hh, mm, progress.remaining_s, local_clock);
        let driven = if r.distance_m > 1.0 {
            (1.0 - progress.remaining_m / r.distance_m).clamp(0.0, 1.0) as f32
        } else {
            0.0
        };
        egui::Area::new(egui::Id::new("driver_bar"))
            .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -bar_bottom()))
            .show(&ctx, |ui| {
                let frame = theme::overlay_frame().inner_margin(egui::Margin::symmetric(8, 8));
                let inset = frame.inner_margin.sum() + egui::Vec2::splat(2.0 * frame.stroke.width);
                let shown = frame.show(ui, |ui| {
                    ui.set_width(bar_width - inset.x);
                    ui.set_height(BAR_HEIGHT - inset.y);
                    ui.horizontal_centered(|ui| {
                        let gap = ui.spacing().item_spacing.x;
                        if theme::round_button(
                            ui,
                            if arrived { icons::CHECK } else { icons::CLEAR },
                            if arrived { "Done" } else { "Stop navigation" },
                            false,
                        )
                        .clicked()
                        {
                            done = true;
                        }
                        let stats = (ui.available_width() - theme::ROUND_BUTTON_SIZE - 3.0 * gap)
                            .max(90.0)
                            / 3.0;
                        let time_left = if arrived {
                            "—".to_string()
                        } else {
                            format_duration(progress.remaining_s)
                        };
                        let to_go = if arrived {
                            "—".to_string()
                        } else {
                            format_distance(progress.remaining_m)
                        };
                        for (value, caption) in [
                            (
                                arrival.as_str(),
                                if arrived { "arrived" } else { "arrival" },
                            ),
                            (time_left.as_str(), "left"),
                            (to_go.as_str(), "to go"),
                        ] {
                            ui.allocate_ui_with_layout(
                                egui::vec2(stats, BAR_HEIGHT - inset.y),
                                egui::Layout::top_down(egui::Align::Center),
                                |ui| {
                                    ui.spacing_mut().item_spacing.y = 0.0;
                                    ui.label(big(value, 19.0, theme::p().cream));
                                    ui.label(small(caption, theme::p().text_dim));
                                },
                            );
                        }
                        if theme::round_button(
                            ui,
                            icons::MENU,
                            "Route — destination, turn list, travel mode",
                            false,
                        )
                        .clicked()
                        {
                            open_planner = true;
                        }
                    });
                });
                // How much of the trip is behind you, as a hairline along the
                // bar's top edge — the reference's progress line, drawn on the
                // frame rather than laid out so it costs the bar no height.
                let rect = shown.response.rect.shrink2(egui::vec2(14.0, 0.0));
                let y = rect.top() + 1.5;
                let p = ui.painter();
                p.line_segment(
                    [egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)],
                    egui::Stroke::new(3.0, theme::p().overlay_line),
                );
                if driven > 0.0 {
                    p.line_segment(
                        [
                            egui::pos2(rect.left(), y),
                            egui::pos2(rect.left() + rect.width() * driven, y),
                        ],
                        egui::Stroke::new(3.0, ROUTE_BLUE),
                    );
                }
            });

        // ── The speed, above the bar's left end, with the limit over it ──
        let speed_kmh = driver.speed_ms * 3.6;
        let limit = ahead.limit_at(progress.along_m);
        let over = limit.is_some_and(|l| speed_kmh > f32::from(l) + OVER_LIMIT_TOLERANCE_KMH);
        let left = ((viewport_x - bar_width) * 0.5).max(CARD_MARGIN);
        egui::Area::new(egui::Id::new("driver_speed"))
            .anchor(egui::Align2::LEFT_BOTTOM, egui::vec2(left, -above_bar()))
            .show(&ctx, |ui| {
                ui.set_width(SPEED_DISC);
                ui.vertical_centered(|ui| {
                    ui.spacing_mut().item_spacing.y = 6.0;
                    if let Some(limit) = limit {
                        speed_limit_sign(ui, limit);
                    }
                    let (rect, _) =
                        ui.allocate_exact_size(egui::Vec2::splat(SPEED_DISC), egui::Sense::hover());
                    let p = ui.painter();
                    let c = rect.center();
                    p.circle_filled(c, SPEED_DISC * 0.5, theme::p().overlay);
                    p.circle_stroke(
                        c,
                        SPEED_DISC * 0.5,
                        egui::Stroke::new(1.0, theme::p().overlay_line),
                    );
                    // Over the limit turns the *speed* red, not the sign: the sign
                    // states a fact about the road and reads the same whatever the
                    // vehicle is doing.
                    p.text(
                        c - egui::vec2(0.0, 5.0),
                        egui::Align2::CENTER_CENTER,
                        format!("{speed_kmh:.0}"),
                        egui::FontId::new(22.0, theme::brand_family()),
                        if over { SIGN_RED } else { theme::p().cream },
                    );
                    p.text(
                        c + egui::vec2(0.0, 13.0),
                        egui::Align2::CENTER_CENTER,
                        "km/h",
                        egui::FontId::proportional(theme::SIZE_TINY),
                        theme::p().text_dim,
                    );
                });
            });

        // ── Overview and Re-centre, above the bar's right end ──
        //
        // Overview is the route judged rather than driven: the rig pulls back
        // until the rest of it is on screen ([`OverviewFrame`]). Re-centre comes
        // up beside it only while the view is held off the rig — an offer, not
        // the mechanism; the peek expires on its own ([`PEEK_TIMEOUT`]).
        let right = ((viewport_x - bar_width) * 0.5).max(CARD_MARGIN);
        let peeking = driver.peeking();
        let overview = driver.overview;
        egui::Area::new(egui::Id::new("driver_view_buttons"))
            .anchor(egui::Align2::RIGHT_BOTTOM, egui::vec2(-right, -above_bar()))
            .show(&ctx, |ui| {
                ui.horizontal(|ui| {
                    if peeking
                        && theme::round_button(
                            ui,
                            icons::HERE,
                            "Put the camera back behind the arrow",
                            false,
                        )
                        .clicked()
                    {
                        recentre = true;
                    }
                    if theme::round_button(
                        ui,
                        icons::CAT_MAP,
                        if overview {
                            "Back to following the road"
                        } else {
                            "See the rest of the route"
                        },
                        overview,
                    )
                    .clicked()
                    {
                        toggle_overview = true;
                    }
                });
            });

        // ── What the position is worth, centred above the bar ──
        //
        // The provider's own message when there is one, otherwise the quality of
        // the fix itself (`GpsQuality`) — only when there is something to say, so
        // a good lock adds nothing to the screen.
        let chip = driver
            .gps_error
            .as_ref()
            .map(|err| format!("{err} — following the avatar"))
            .or_else(|| driver.gps.chip());
        if let Some(chip) = chip {
            let width = (usable - 2.0 * (SPEED_DISC + theme::OSD_GAP)).clamp(80.0, 260.0);
            egui::Area::new(egui::Id::new("driver_gps_chip"))
                .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -above_bar()))
                .show(&ctx, |ui| {
                    theme::chip_frame().show(ui, |ui| {
                        ui.set_width(width);
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(chip)
                                    .size(theme::SIZE_CHIP)
                                    .color(theme::DARK.warn),
                            )
                            .truncate(),
                        );
                    });
                });
        }
    }

    if recentre {
        driver.recentre();
    }
    if toggle_overview {
        driver.overview = !driver.overview;
        driver.recentre();
    }
    if retry {
        driver.retry_route();
    }
    // Opening the planner does not leave driver mode: the drive carries on behind
    // it, the bar stands down as it does for any docked panel, and the card —
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

/// The satnav blue the manoeuvre plate is painted in. Not a theme colour: the
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
/// exists. Dark enough to carry `theme::Palette::cream` at the contrast the theme tests
/// hold the rest of the UI to.
const ARRIVE_GREEN: egui::Color32 = egui::Color32::from_rgb(0x1B, 0x7A, 0x48);

/// The card's colour while the route is being replaced. Signage again, and here
/// for a second reason: this was `theme::AMBER`, the palette's second accent,
/// which the monochrome chrome retired. `theme::Palette::warn` could not take it over —
/// that is a *type* colour, pale enough to read on a dark panel, and this is a
/// card fill that has to carry `theme::Palette::cream`. So it is the amber a road sign
/// would use, dark enough to be a plate.
const REROUTE_AMBER: egui::Color32 = egui::Color32::from_rgb(0x8A, 0x5A, 0x0C);

/// The plate for what the display cannot fix — off the route with no reroute yet,
/// or a reroute that came back empty. A sign red dark enough to carry
/// `theme::Palette::cream`; `theme::Palette::error` is a pale *text* colour and cannot be a plate.
const OFF_ROUTE_RED: egui::Color32 = egui::Color32::from_rgb(0xA3, 0x2A, 0x22);

/// The route line's own blue (`navigation`'s fill), for the trip bar's progress
/// line: the bar and the line on the map are the same object.
const ROUTE_BLUE: egui::Color32 = egui::Color32::from_rgb(0x29, 0x87, 0xE6);

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
    painter.rect_filled(rect, 2.0, theme::p().ink.gamma_multiply(0.35));
    if filled > 0.0 {
        let mut fill = rect;
        fill.set_width(rect.width() * filled);
        painter.rect_filled(fill, 2.0, theme::p().cream);
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
pub(crate) fn arrival_clock(h: u32, m: u32, remaining_s: f64, local: bool) -> String {
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

    /// At the start of a drive the card names the first real manoeuvre, never the
    /// departure it is standing on ("◯ Now").
    #[test]
    fn the_card_never_names_the_departure() {
        let route = fixture();
        let geom = RouteGeom::build(&route).unwrap();
        let p = project_onto_route(&geom, &route, route.steps[0].at);
        assert_eq!(route.steps[0].maneuver, Maneuver::Depart);
        assert_eq!(p.step, 1, "the card opened on the departure");
        assert!(p.step_distance_m > NOW_M, "and counted it down as Now");
    }

    /// A route that comes back past itself — out along one street and home along
    /// the next one over — must keep the vehicle on the leg it is driving. Nearest
    /// over the whole line puts a fix nearer the return leg ten minutes ahead.
    #[test]
    fn a_hint_keeps_the_projection_on_its_own_leg() {
        let east = |m: f64| m / (111_320.0 * 53.0_f64.to_radians().cos());
        let north = |m: f64| m / 110_540.0;
        // North 1 km, east 30 m, back south 1 km.
        let points = vec![
            [53.0, 6.0],
            [53.0 + north(1000.0), 6.0],
            [53.0 + north(1000.0), 6.0 + east(30.0)],
            [53.0, 6.0 + east(30.0)],
        ];
        let mut cum = vec![0.0];
        for w in points.windows(2) {
            cum.push(cum.last().unwrap() + metres_between(w[0], w[1]));
        }
        let geom = RouteGeom {
            total_m: *cum.last().unwrap(),
            points,
            cum,
            step_along: Vec::new(),
        };
        // Half-way up the first leg, drifted 20 m toward the return leg.
        let at = [53.0 + north(500.0), 6.0 + east(20.0)];

        let (anywhere, _) = project_near(&geom, at, None);
        assert!(
            anywhere > 1200.0,
            "the global nearest is the way back: {anywhere}"
        );

        let (near, off) = project_near(&geom, at, Some(490.0));
        assert!((near - 500.0).abs() < 2.0, "along {near}");
        assert!((off - 20.0).abs() < 1.0, "off {off}");
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
        // Two flat layers: the navy rim at 0 and the white face over it.
        assert!(
            positions
                .iter()
                .all(|p| p[1] == 0.0 || p[1] == ARROW_FACE_LIFT),
            "the arrow must be two flat layers"
        );
        let nose = |y: f32| {
            positions
                .iter()
                .filter(|p| p[1] == y)
                .map(|p| p[2])
                .fold(f32::MIN, f32::max)
        };
        let (face, rim) = (nose(ARROW_FACE_LIFT), nose(0.0));
        assert!(
            (face - ARROW_HALF_LENGTH).abs() < 1e-3,
            "nose is not at +Z: {face}"
        );
        assert!(
            rim > face,
            "the rim must lead the face's nose: {rim} vs {face}"
        );
        let tail = |y: f32| {
            positions
                .iter()
                .filter(|p| p[1] == y)
                .map(|p| p[2])
                .fold(f32::MAX, f32::min)
        };
        assert!(tail(0.0) < tail(ARROW_FACE_LIFT), "…and trail its tails");

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
    fn a_far_fix_jumps_only_when_it_is_sure_of_itself() {
        // Near is a write whatever the accuracy: there is no jump to doubt.
        assert_eq!(reach_of(150.0, 5_000.0), FixReach::Near);
        // A good fix in another city is a jump.
        assert_eq!(reach_of(180_000.0, 12.0), FixReach::Far);
        // A ten-minute-old cached fix (`stale_accuracy_m`: + 600 s × 13 m/s)
        // five kilometres off says too little to move the person on.
        assert_eq!(reach_of(5_000.0, 7_800.0), FixReach::Doubtful);
        // So does a fix with no accuracy worth the name.
        assert_eq!(reach_of(50_000.0, f64::NAN), FixReach::Doubtful);
        assert_eq!(reach_of(50_000.0, f64::INFINITY), FixReach::Doubtful);
    }

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
        assert!(local.y >= motion::GROUND_Y, "y was {}", local.y);
    }

    /// …and never *above* it either. An avatar standing on the sim's floor must
    /// be placed on that floor: lifting it even five centimetres makes the
    /// prediction drop it back every frame, and every drop is a landing — the
    /// thud and the camera dip that played continuously while GPS walked the
    /// avatar. Run through the same fall the prediction runs, so the property
    /// pinned is "no landing", not a number.
    #[test]
    fn a_fix_leaves_a_standing_avatar_standing() {
        let (_, local) = placement(120.0, -40.0, motion::GROUND_Y);
        assert_eq!(local.y, motion::GROUND_Y);
        let mut y = local.y;
        let mut vy = 0.0;
        for _ in 0..240 {
            // Every frame re-places the avatar, as `locate` does between fixes.
            y = placement(120.0, -40.0, y).1.y;
            let step = motion::step_fall(y, vy, motion::GROUND_Y, 1.0 / 120.0);
            assert_eq!(step.landing_speed, None, "a standing avatar landed");
            assert!(step.grounded);
            y = step.y;
            vy = step.vertical_velocity;
        }
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

    /// The chrome has to fit the screen it is on, down to the 274 pt an Android
    /// phone reports: the card and the bar inside the margins, and the three
    /// things stacked over the bar's ends — the speed disc, the view buttons and
    /// the GPS note between them — side by side without touching.
    #[test]
    fn the_chrome_fits_every_viewport_it_is_drawn_on() {
        for width in [274.0_f32, 320.0, 400.0, 640.0, 800.0, 1280.0] {
            let compact = width < crate::systems::egui_manager::COMPACT_WIDTH_PTS;
            let l = driver_layout(width, compact);
            let usable = width - 2.0 * CARD_MARGIN + 1e-3;
            assert!(l.card_width <= usable, "the card overflows {width} pt");
            assert!(l.bar_width <= usable, "the bar overflows {width} pt");
            // Speed disc left, two round buttons right, the note's floor between.
            let row = SPEED_DISC + 80.0 + 2.0 * theme::ROUND_BUTTON_SIZE + 4.0 * theme::OSD_GAP;
            assert!(
                row <= l.bar_width + 1e-3,
                "the row over the bar overlaps at {width} pt"
            );
        }
        // A phone's card spans the screen, as the reference's does; a desktop's
        // is a card in the corner.
        assert_eq!(
            driver_layout(400.0, true).card_width,
            400.0 - 2.0 * CARD_MARGIN
        );
        assert_eq!(driver_layout(1280.0, false).card_width, CARD_WIDTH);
    }

    /// The overview fits the rest of the route: further back for a longer one,
    /// further on a portrait phone than on a landscape window, never closer than
    /// the flat chase view and never past its ceiling.
    #[test]
    fn the_overview_pulls_back_to_fit_the_route() {
        let frame = |half: f32| {
            OverviewFrame::around([Vec2::ZERO, Vec2::new(0.0, 2.0 * half)].into_iter()).unwrap()
        };
        let short = frame(300.0).distance(1.6);
        let long = frame(3_000.0).distance(1.6);
        assert!(long > short, "{long} vs {short}");
        assert!(
            frame(3_000.0).distance(0.45) > long,
            "a portrait screen fits less"
        );
        assert!(frame(1.0).distance(1.6) >= FLAT_DISTANCE);
        assert!(frame(500_000.0).distance(0.45) <= OVERVIEW_MAX_DISTANCE);
        // The centre is where the route is, not where the vehicle is.
        assert_eq!(frame(300.0).centre, Vec2::new(0.0, 300.0));
        assert!(OverviewFrame::around(std::iter::empty()).is_none());
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
