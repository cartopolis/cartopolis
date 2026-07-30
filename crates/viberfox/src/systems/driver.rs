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
//! The layout is a satnav's, taken from the two reference screenshots this was
//! specified against (Organic Maps' navigation view and a TomTom unit): the next
//! manoeuvre on a card at the top left with the compass under it, the trip —
//! arrival clock, time left, distance left — as a narrow strip down the right
//! edge, and the current speed in a pill at the bottom centre. Nothing else. The
//! first cut put all three trip numbers in one wide bar across the bottom, which
//! read as a dashboard rather than as a navigation view.
//!
//! **Where the position comes from.** Outside driver mode there is only one
//! answer: the avatar, which the user walks around. Inside it, the device's own
//! fix wins when there is one ([`crate::platform::location`]) — that is what
//! "navigating" means — and the avatar is moved to it, so the camera follow, the
//! arrow, and the progress readout all keep reading the avatar and need to know
//! nothing about GPS. Today only the web build has a provider; on desktop the
//! avatar simply stays under the user's control and driver mode narrates *its*
//! movement, which is also how it is testable without a dongle.
//!
//! **Heading is a course over ground, not a facing.** The avatar's yaw is bound
//! to the camera azimuth by `avatar::handle_avatar_movement` (W walks into the
//! view), so reading it back would make the arrow point up the screen forever —
//! it would be north-up mode's own reflection. The arrow and the compass use the
//! smoothed direction of travel instead, falling back to the facing only when
//! standing still, exactly as ADR-026's live vehicles derive a bearing from
//! motion when the feed carries none.

use bevy::math::DVec3;
use bevy::prelude::*;
use bevy::asset::RenderAssetUsages;
use bevy::render::mesh::{Indices, PrimitiveTopology};
use big_space::prelude::{BigSpace, Grid, GridCell};
use web_time::Instant;

use crate::components::Avatar;
use crate::platform::location::LocationWatch;
use crate::resources::{AvatarState, CameraMode, CameraState};
use crate::systems::egui_manager::EguiManager;
use crate::systems::theme;
use crate::systems::free_camera::{WORLD_CELL_EDGE, WORLD_SWITCH_THRESHOLD};
use crate::systems::geo_nav::{lat_lng_to_world, world_to_lat_lng, GeoNavState, Teleport};
use crate::systems::map_stream::MapStream;
use crate::systems::navigation::RouteState;
use crate::systems::routing::{format_distance, format_duration, Maneuver, Route};
use crate::systems::vector_tiles;

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
const FLAT_DISTANCE: f32 = 240.0;
const FLAT_PITCH: f32 = 1.35; // ~77°
const FLAT_LOOKAHEAD: f32 = 0.30;
const PERSPECTIVE_DISTANCE: f32 = 170.0;
const PERSPECTIVE_PITCH: f32 = 0.38; // ~22°
const PERSPECTIVE_LOOKAHEAD: f32 = 0.55;

/// Exponential rate (per second) the chase rig eases its distance/tilt toward the
/// orientation's target, so cycling the compass reads as a camera move rather than
/// a cut.
const RIG_EASE_RATE: f32 = 5.0;

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

/// …and for how long it has to hold before the card says so. Undebounced, the
/// banner fires on any transient: the frames between a teleport being *requested*
/// and the map re-anchoring under it read as hundreds of metres off route, and
/// "Off route" is the first thing a driver sees on entering.
const OFF_ROUTE_GRACE: f32 = 2.0;

// ─── State ───────────────────────────────────────────────────────────────────

/// How the map is held while driving — Organic Maps' navigation cycle, and what
/// the compass button steps through.
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
}

/// The route measured out along its own polyline: cumulative metres per vertex,
/// and where each step's manoeuvre falls on it.
///
/// Built once per route rather than per frame, and it is what makes the readout
/// independent of the planner's own distance bookkeeping — every number comes off
/// the same polyline the line on the map is drawn from.
struct RouteGeom {
    /// `[lat, lng]` per vertex, road portion only (the pin lead-ins are not driven).
    points: Vec<[f64; 2]>,
    /// Metres from the start to each vertex; `cum[0] == 0`.
    cum: Vec<f64>,
    total_m: f64,
    /// Metres from the start to each step's manoeuvre, one per [`Route::steps`].
    step_along: Vec<f64>,
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
    /// Since when the position has been further than [`OFF_ROUTE_M`] from the
    /// route; the banner waits [`OFF_ROUTE_GRACE`] on this. `None` while on route.
    off_route_since: Option<Instant>,

    /// The device-location subscription. `Some` only while driving, so nothing
    /// keeps the browser's location indicator lit after you stop.
    watch: Option<LocationWatch>,
    geom: Option<RouteGeom>,
    /// `RouteState::generation` the cached [`RouteGeom`] was built from.
    geom_generation: Option<u32>,
    camera_save: Option<CameraSave>,
    /// Previous position and when it was sampled, for the measured speed/course.
    last_sample: Option<(DVec3, Instant)>,
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
        self.progress = None;
        self.speed_ms = 0.0;
        self.last_sample = None;
        self.gps_error = None;
        self.gps_fix = false;
        self.watch = Some(LocationWatch::start());
        tracing::info!(gps = LocationWatch::available(), "driver mode on");
    }

    /// Ask for driver mode without a route yet: it starts the moment one solves.
    pub fn arm(&mut self) {
        self.armed = true;
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
        self.gps_fix = false;
        tracing::info!("driver mode off");
    }
}

// ─── Progress along the route ────────────────────────────────────────────────

/// Metres between two `[lat, lng]` points, on the local tangent plane.
///
/// Equirectangular rather than haversine on purpose: over the tens of metres
/// between consecutive route vertices the two agree to well under a millimetre,
/// and this is called for every vertex of the route on every frame.
fn metres_between(a: [f64; 2], b: [f64; 2]) -> f64 {
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
    fn build(route: &Route) -> Option<Self> {
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

        Some(Self { points, cum, total_m: total, step_along })
    }
}

/// Project a point onto a polyline: `(metres along, metres off)`.
///
/// Segment-wise, not vertex-wise. A vertex-only search reports you as "at" the
/// last junction until you are nearer the next one, which on a 200 m straight
/// freezes the remaining distance and then drops it 200 m at once.
fn nearest_on_polyline(points: &[[f64; 2]], cum: &[f64], at: [f64; 2]) -> (f64, f64) {
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
fn project_onto_route(geom: &RouteGeom, route: &Route, at: [f64; 2]) -> Progress {
    let (along, off) = nearest_on_polyline(&geom.points, &geom.cum, at);
    let remaining_m = (geom.total_m - along).max(0.0);
    // Time is scaled from the planner's own answer rather than from the current
    // speed: the planner knows the speed limits of the road ahead, and a driver
    // stopped at a light does not have an infinite ETA.
    let fraction = if geom.total_m > 1.0 { remaining_m / geom.total_m } else { 0.0 };
    let step = geom
        .step_along
        .iter()
        .position(|&s| s > along - STEP_PASSED_M)
        .unwrap_or(route.steps.len().saturating_sub(1));
    let step_distance_m = geom.step_along.get(step).map_or(0.0, |&s| (s - along).max(0.0));
    Progress {
        along_m: along,
        off_route_m: off,
        remaining_m,
        remaining_s: route.duration_s * fraction,
        step,
        step_distance_m,
    }
}

// ─── Systems ─────────────────────────────────────────────────────────────────

/// The driver-mode tick: hold or drop the mode, fold in a device fix, and measure
/// speed, course and progress from wherever the position ended up.
#[allow(clippy::too_many_arguments)]
pub fn drive_driver_mode(
    mut driver: ResMut<DriverState>,
    route: Res<RouteState>,
    mut nav: ResMut<GeoNavState>,
    stream: Res<MapStream>,
    time: Res<Time>,
    mut avatar_state: ResMut<AvatarState>,
    mut avatars: Query<(&mut GridCell, &mut Transform), With<Avatar>>,
) {
    if driver.armed && route.route.is_some() {
        driver.armed = false;
        driver.enter();
    }
    // The mode exists only for a solved route. Clearing one, or editing an
    // endpoint, drops straight out — better than driving a line that is gone.
    // A solve *in flight* is not gone, though: switching travel mode mid-drive
    // re-runs the search over the cached graph, and being thrown out of navigation
    // for the frames that takes would be absurd.
    let solving = matches!(
        route.status,
        crate::systems::navigation::SolveStatus::Fetching(..)
            | crate::systems::navigation::SolveStatus::Solving
    );
    if driver.active && route.route.is_none() && !solving {
        driver.exit();
    }
    if !driver.active {
        driver.geom = None;
        driver.geom_generation = None;
        return;
    }
    let Some(r) = route.route.as_ref() else {
        return;
    };
    let Some(anchor) = stream.geo_anchor() else {
        return;
    };
    if driver.geom_generation != Some(route.generation) {
        driver.geom = RouteGeom::build(r);
        driver.geom_generation = Some(route.generation);
    }

    // A device fix wins over the avatar: that is what navigating is. Only the web
    // build has a provider today, so on desktop this arm never fires and the
    // avatar keeps its position — driver mode then narrates the user's own
    // movement, which is exactly what makes it usable without a dongle.
    let fix = driver.watch.as_ref().and_then(LocationWatch::latest);
    driver.gps_error = driver.watch.as_ref().and_then(LocationWatch::error);
    driver.gps_fix = fix.is_some();
    if let Some(fix) = fix {
        place_avatar(anchor, fix.lat, fix.lng, &mut avatar_state, &mut avatars);
        driver.snap_pending = false;
    } else if driver.snap_pending {
        // No provider: start the drive at the route's start rather than wherever
        // the avatar happened to be standing, which is usually a different city.
        // A teleport (not a bare position write) because it re-anchors the map and
        // brings the camera with it.
        driver.snap_pending = false;
        if let Some(start) = driver.geom.as_ref().and_then(|g| g.points.first()).copied() {
            nav.pending_teleport = Some(Teleport::geo(start[0], start[1], None));
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
    let now = Instant::now();
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
    // Debounce the off-route judgement (see `OFF_ROUTE_GRACE`). A queued teleport
    // is not "off route" either: the position it will land at has not been applied
    // yet, and the anchor it is measured against is about to move.
    let off = progress.is_some_and(|p| p.off_route_m > OFF_ROUTE_M);
    if !off || nav.pending_teleport.is_some() {
        driver.off_route_since = None;
    } else if driver.off_route_since.is_none() {
        driver.off_route_since = Some(now);
    }
    driver.progress = progress;
}

impl DriverState {
    /// Whether the chrome should say so — off the route, and for long enough that
    /// it is not a transient (see [`OFF_ROUTE_GRACE`]).
    #[must_use]
    pub fn off_route(&self) -> bool {
        self.off_route_since
            .is_some_and(|t| t.elapsed().as_secs_f32() > OFF_ROUTE_GRACE)
    }
}

/// Put the avatar (and so the arrow, the camera and the progress readout) at a
/// geographic position within the current anchor frame.
fn place_avatar(
    anchor: (f64, f64, u32),
    lat: f64,
    lng: f64,
    avatar_state: &mut AvatarState,
    avatars: &mut Query<(&mut GridCell, &mut Transform), With<Avatar>>,
) {
    let (wx, wz) = lat_lng_to_world(anchor, lat, lng);
    let grid = Grid::new(WORLD_CELL_EDGE, WORLD_SWITCH_THRESHOLD);
    let (cell, local) = grid.translation_to_grid(DVec3::new(
        wx,
        f64::from(avatar_state.position.y.max(0.05)),
        wz,
    ));
    if let Ok((mut c, mut tf)) = avatars.single_mut() {
        *c = cell;
        tf.translation = local;
    }
    avatar_state.position = local;
    avatar_state.display_position = local;
}

/// Hold the chase rig while driving, and hand the camera back on the way out.
///
/// The rig is *held*, not merely set on entry: `free_camera::camera_controls`
/// reads `CameraState` fresh every frame, and driver mode's contract is that the
/// view does not wander. `drive_locked` is what tells that system to ignore the
/// wheel and the drag; everything else here is the pose it should hold.
pub fn steady_driver_camera(
    mut driver: ResMut<DriverState>,
    mut camera_state: ResMut<CameraState>,
    time: Res<Time>,
    avatars: Query<(), With<Avatar>>,
) {
    if !driver.active {
        // Leaving: put back exactly what was taken, once.
        if let Some(save) = driver.camera_save.take() {
            camera_state.drive_locked = false;
            camera_state.mode = save.mode;
            camera_state.distance = save.distance;
            camera_state.pitch = save.pitch;
            camera_state.azimuth = save.azimuth;
            camera_state.follow_rotate = save.follow_rotate;
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

    let (distance, pitch, lookahead) = driver.orientation.rig();
    let ease = 1.0 - (-RIG_EASE_RATE * time.delta().as_secs_f32()).exp();
    camera_state.distance += (distance - camera_state.distance) * ease;
    camera_state.pitch += (pitch - camera_state.pitch) * ease;
    // Eased through the *fraction*, so switching orientation slides the aim point
    // along with the tilt instead of snapping the horizon.
    let aim = camera_state.distance * lookahead;
    camera_state.drive_lookahead += (aim - camera_state.drive_lookahead) * ease;

    // Screen-up direction. The camera sits at `(sin az, ., cos az) * d` from the
    // avatar and looks back at it, so its forward on the ground is
    // `-(sin az, cos az)`: for the course to point up the screen that must equal
    // the course direction, and for north-up it must be −Z.
    let target = match driver.orientation {
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

// ─── The arrow ───────────────────────────────────────────────────────────────

/// The position marker drawn while driving.
#[derive(Component)]
pub struct DriverArrow;

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
    mut arrows: Query<(Entity, &mut GridCell, &mut Transform), (With<DriverArrow>, Without<Avatar>)>,
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
            // drawn into the scene, and it has to stay legible at night. The depth
            // bias puts it above the route line, which is itself above the ground
            // stack (`navigation::ROUTE_Y` explains that fight in full). Rung 24 on
            // the shared ladder — the step is what keeps a rung clear of the
            // rounding noise, see `vector_tiles::DEPTH_BIAS_STEP`.
            let fill = materials.add(StandardMaterial {
                base_color: Color::srgb(0.20, 0.60, 0.98),
                unlit: true,
                depth_bias: 24.0 * vector_tiles::DEPTH_BIAS_STEP,
                cull_mode: None,
                ..default()
            });
            commands.insert_resource(ArrowMaterials { fill: fill.clone() });
            fill
        }
    };

    let rotation = Quat::from_rotation_y(driver.course);
    let translation = tf.translation + Vec3::Y * ARROW_Y;
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
                Transform { translation, rotation, ..default() },
                *cell,
                ChildOf(root),
                bevy::camera::visibility::NoFrustumCulling,
                DriverArrow,
                crate::systems::globe::HideInSpace,
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
    Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default())
        .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, vec![[0.0, 1.0, 0.0]; positions.len()])
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions)
        .with_inserted_indices(Indices::U32(indices))
}

// ─── The navigation chrome ───────────────────────────────────────────────────

/// Width of the manoeuvre card, and the vertical space it claims. Fixed rather
/// than sized to content so the compass beneath it has a place to be — the same
/// reason the status HUD pins its width (`hud::HUD_WIDTH`).
pub const CARD_WIDTH: f32 = 190.0;
pub const CARD_HEIGHT: f32 = 108.0;
/// Where the card sits, and therefore where everything under it stacks from.
pub const CARD_MARGIN: f32 = 12.0;

/// Width of the trip strip down the right-hand edge.
const STRIP_WIDTH: f32 = 96.0;

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
pub fn render_driver_hud(
    mut egui: ResMut<EguiManager>,
    mut driver: ResMut<DriverState>,
    route: Res<RouteState>,
    clock: Res<crate::systems::transit_live::WorldClock>,
) {
    if !driver.active {
        return;
    }
    let Some(r) = route.route.as_ref() else {
        return;
    };
    let progress = driver.progress.unwrap_or_default();
    let off_route = driver.off_route();
    let step = r.steps.get(progress.step);
    let ctx = egui.ctx_mut().clone();
    let panel = |fill: egui::Color32| {
        egui::Frame::NONE
            .fill(fill)
            .corner_radius(8.0)
            .inner_margin(egui::Margin::symmetric(10, 8))
    };

    // ── The manoeuvre card ──
    egui::Area::new(egui::Id::new("driver_maneuver"))
        .anchor(egui::Align2::LEFT_TOP, egui::vec2(CARD_MARGIN, CARD_MARGIN))
        .show(&ctx, |ui| {
            panel(if off_route { theme::ERROR } else { DRIVE_BLUE })
                .show(ui, |ui| {
                    ui.set_width(CARD_WIDTH - 20.0);
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 2.0;
                        if off_route {
                            ui.label(big("Off route", 20.0, theme::INK));
                            ui.label(small(
                                &format!("{} from the route", format_distance(progress.off_route_m)),
                                theme::INK,
                            ));
                            return;
                        }
                        // Glyph and distance on one line, street name under it —
                        // the satnav arrangement, and it keeps the card short
                        // enough to leave the compass its place below.
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 8.0;
                            let maneuver = step.map_or(Maneuver::Continue, |s| s.maneuver);
                            ui.label(big(maneuver_glyph(&ctx, maneuver), 34.0, theme::CREAM));
                            ui.label(big(
                                &format_distance(progress.step_distance_m),
                                24.0,
                                theme::CREAM,
                            ));
                        });
                        if let Some(name) = step.and_then(|s| s.name.as_deref()) {
                            ui.label(egui::RichText::new(name).size(12.5).color(theme::CREAM));
                        }
                    });
                });
        });

    // ── Permission trouble. Nothing else would ever say why the arrow is not
    // moving on a build that does have a provider. ──
    if let Some(err) = driver.gps_error.clone() {
        egui::Area::new(egui::Id::new("driver_gps_error"))
            .anchor(
                egui::Align2::LEFT_BOTTOM,
                egui::vec2(CARD_MARGIN, -PILL_BOTTOM),
            )
            .show(&ctx, |ui| {
                panel(theme::OVERLAY).show(ui, |ui| {
                    ui.set_width(260.0);
                    ui.label(small(&format!("{err} — following the avatar"), theme::WARN));
                });
            });
    }

    // ── The trip strip, right edge ──
    let mut exit = false;
    let (hh, mm, _) = clock.hms().0;
    let arrival = arrival_clock(hh, mm, progress.remaining_s);
    egui::Area::new(egui::Id::new("driver_strip"))
        .anchor(
            egui::Align2::RIGHT_TOP,
            egui::vec2(-CARD_MARGIN, CARD_MARGIN),
        )
        .show(&ctx, |ui| {
            panel(theme::OVERLAY).show(ui, |ui| {
                ui.set_width(STRIP_WIDTH - 20.0);
                ui.vertical_centered(|ui| {
                    ui.spacing_mut().item_spacing.y = 1.0;
                    // Arrival first: on a drive of any length it is the number
                    // being planned around, which is why a satnav puts it top.
                    ui.label(big(&arrival, 24.0, theme::CREAM));
                    ui.label(small("arrival", theme::TEXT_DIM));
                    ui.add_space(6.0);
                    ui.label(big(&format_distance(progress.remaining_m), 21.0, theme::TEXT));
                    ui.label(small(&format_duration(progress.remaining_s), theme::TEXT_DIM));
                    ui.add_space(8.0);
                    if ui
                        .add(egui::Button::new(egui::RichText::new("✕ Stop").size(12.5)))
                        .on_hover_text("Leave driver mode and put the map back")
                        .clicked()
                    {
                        exit = true;
                    }
                });
            });
        });

    // ── The speed pill, bottom centre ──
    egui::Area::new(egui::Id::new("driver_speed"))
        .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -PILL_BOTTOM))
        .show(&ctx, |ui| {
            egui::Frame::NONE
                .fill(theme::OVERLAY)
                .corner_radius(18.0)
                .inner_margin(egui::Margin::symmetric(16, 6))
                .show(ui, |ui| {
                    ui.set_width(84.0);
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 5.0;
                        ui.label(big(
                            &format!("{:.0}", driver.speed_ms * 3.6),
                            22.0,
                            theme::CREAM,
                        ));
                        ui.label(small("km/h", theme::TEXT_DIM));
                    });
                });
        });

    if exit {
        driver.exit();
    }
}

/// The satnav blue the manoeuvre card is painted in. Not a theme colour: the
/// palette is the fox's warm one, and a *direction* instruction is the one piece
/// of chrome that has to read as a road sign rather than as part of the app.
const DRIVE_BLUE: egui::Color32 = egui::Color32::from_rgb(0x14, 0x6E, 0xD2);

/// A readout value.
fn big(text: &str, size: f32, color: egui::Color32) -> egui::RichText {
    egui::RichText::new(text).size(size).strong().color(color)
}

/// Its caption.
fn small(text: &str, color: egui::Color32) -> egui::RichText {
    egui::RichText::new(text).size(11.0).color(color)
}

/// Wall-clock time of arrival, `remaining` seconds after `h:m`.
fn arrival_clock(h: u32, m: u32, remaining_s: f64) -> String {
    let mins = (i64::from(h) * 60 + i64::from(m) + (remaining_s / 60.0).round() as i64)
        .rem_euclid(24 * 60);
    format!("{:02}:{:02}", mins / 60, mins % 60)
}

/// The manoeuvre's arrow, resolved against the font that is actually loaded —
/// egui ships a thin emoji subset and a missing glyph renders as an empty box
/// (`navigation::Icons` documents the probing this shares).
fn maneuver_glyph(ctx: &egui::Context, maneuver: Maneuver) -> &'static str {
    let candidates = maneuver.icon_candidates();
    let font = egui::TextStyle::Body.resolve(&ctx.style_of(ctx.theme()));
    ctx.fonts_mut(|f| {
        candidates
            .iter()
            .copied()
            .find(|c| f.has_glyphs(&font, c))
            .unwrap_or_else(|| candidates.last().copied().unwrap_or("?"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::systems::routing::{RouteStep, TravelMode};

    /// A straight 1 km route due east, with a turn in the middle, in metres that
    /// are easy to check by hand.
    fn fixture() -> Route {
        // ~0.001° of longitude at 53°N is ~67 m; use latitude steps instead, where
        // a degree is a constant 110.54 km, so the expected distances are exact.
        let pts: Vec<[f64; 2]> = (0..=10).map(|i| [53.0 + f64::from(i) * 0.001, 6.0]).collect();
        let mut points = vec![[52.9995, 6.0]]; // start pin
        points.extend(pts.iter().copied());
        points.push([53.0105, 6.0]); // end pin
        Route {
            mode: TravelMode::Car,
            points,
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
        assert!((geom.step_along[1] - 552.7).abs() < 1.0, "{:?}", geom.step_along);
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
        assert!((p.remaining_m - 829.05).abs() < 2.0, "remaining {}", p.remaining_m);
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
    /// print "25:10".
    #[test]
    fn arrival_wraps_past_midnight() {
        assert_eq!(arrival_clock(18, 57, 6.0 * 60.0), "19:03");
        assert_eq!(arrival_clock(23, 50, 30.0 * 60.0), "00:20");
        assert_eq!(arrival_clock(9, 0, 0.0), "09:00");
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
        assert!((max_z - ARROW_HALF_LENGTH).abs() < 1e-3, "nose is not at +Z: {max_z}");
        assert!(positions.iter().all(|p| p[1] == 0.0), "the arrow must be flat");

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
}
