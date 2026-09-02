//! Track the phone: where it is (GNSS) and which way it is pointing (compass).
//!
//! This is what the locate arrow in the left-hand column does. One switch, two
//! sensors, and the shortest path from either to the view:
//!
//! - the **position** moves the avatar, because the avatar is what the camera
//!   follows, what the streamers anchor around and what every readout already
//!   measures from. Writing the fix anywhere else would mean a second notion of
//!   "here". Online it is written *and sent* — the sim owns the avatar's
//!   translation, so a fix kept to this client was erased by the next snapshot
//!   and the button did nothing at all on a server (`driver::place_avatar`);
//! - the **heading** turns the view, by handing the follow camera a yaw to aim at
//!   ([`CameraState::follow_heading`]) instead of the avatar's own facing — and
//!   turns the avatar with it, because the camera sits *behind* the avatar and
//!   turning only the camera orbits the model, leaving the marker for "me"
//!   pointing somewhere I am not.
//!
//! **The button does not choose a camera mode.** It used to force
//! [`CameraMode::Avatar`] on the way in and stand down the moment the mode was
//! anything else, which made "where am I" a one-way door out of Fly and Earth:
//! you pressed it to see your position and lost the view you were in to get it.
//! Tracking is a *position* verb, so it now runs in whatever mode is on and moves
//! the camera rather than the switcher — Fly keeps its altitude and its look over
//! the fix, Earth re-centres its ground focus on it, Follow does what it always
//! did. Only the heading is mode-specific, and only because it has to be: the
//! rotate-follow rig ([`CameraState::follow_rotate`]) is the Follow camera's, so
//! outside Avatar mode there is nothing for a compass bearing to turn and the
//! view is left exactly where the user pointed it.
//!
//! **Position tracking snaps once and then follows by delta.** Pressing the
//! button is the "take me there" event, so the first fix places the camera
//! outright; after that the camera is moved by however far the fix moved, which
//! keeps the map sliding under a walk while leaving a pan or a fly free to change
//! where you are watching it from. Re-centring every frame instead would be a
//! camera the user cannot move while the button is lit, with nothing on screen to
//! say why.
//!
//! **It is deliberately not driver mode.** `systems::driver` does all of this and
//! a great deal more — it needs a solved route, strips the map to a navigation
//! display, takes the camera rig, narrates manoeuvres and reroutes when you leave
//! the line. What is here is the part of it that makes sense with no destination
//! at all: a map that knows where you are standing and which way you are facing.
//! The two never run together (driver mode wins, and this stands down), so there
//! is exactly one owner of the avatar's position at any moment. Driver mode is
//! also the one thing here that *does* take the camera mode, and it hands it back
//! on the way out — which is the difference between a mode and a switch.
//!
//! **The cycle is two states, not three.** The button used to step Off → Follow →
//! Follow-and-rotate, where the middle state followed the avatar without turning
//! with it. On a phone that middle state is the odd one out: what a person wants
//! from a locate button is "put me on the map, the right way up", and having to
//! press twice for it is a worse default than the state it skips — which is still
//! reachable, signed in, from Tab and the mode disc.
//!
//! **Turning the phone must not be a fight.** While a compass is reporting, the
//! follow-and-rotate flag is *held* rather than merely set: `free_camera` clears
//! it whenever the user rotate-drags the map, and a compass mode that switched
//! itself off the first time a thumb brushed the screen would read as broken. The
//! drag still turns the view — it is simply eased back to where the phone is
//! pointing, which is the honest answer to "what happens if I fight the compass".
//! On a platform with no compass at all (every desktop) the old behaviour stands:
//! a drag drops the rotate-follow, and dropping it ends the tracking.

use bevy::math::DVec3;
use bevy::prelude::*;
use big_space::prelude::{Grid, GridCell};

use crate::components::{Avatar, FreeCamera};
use crate::grid::{WORLD_CELL_EDGE, WORLD_SWITCH_THRESHOLD};
use crate::platform::location::{GpsFix, LocationWatch};
use crate::platform::orientation::{DeviceHeading, OrientationWatch};
use crate::resources::{AvatarState, CameraMode, CameraState, MapAnchor, OnlineSession};
use crate::systems::driver::DriverState;
use crate::systems::free_camera::cell_origin;
use crate::systems::geo_nav::{GeoNavState, Teleport};

/// The world-XZ yaw that puts a compass bearing up the screen.
///
/// The follow camera sits at `(sin az, ·, cos az) · d` from the avatar and looks
/// back at it, so what the user sees running up the screen is `−(sin az, cos az)`;
/// `free_camera` derives that azimuth as `yaw + π`. North is `−Z` and east is
/// `+X`, so a bearing `b` is the ground direction `(sin b, −cos b)`. Setting the
/// two equal gives `az = −b`, hence `yaw = π − b`.
///
/// Radians in, radians out. Worth its own function and its own test: it is one
/// subtraction, and every way of getting it wrong (a sign, a missing `π`) turns
/// into a map that reads plausibly and points somewhere else.
#[must_use]
pub fn yaw_from_bearing(bearing_rad: f32) -> f32 {
    (std::f32::consts::PI - bearing_rad).rem_euclid(std::f32::consts::TAU)
}

/// The device-tracking switch, and the two subscriptions it owns.
///
/// The watches live here rather than being started once at boot so that nothing
/// polls a sensor no one is looking at — an open GNSS watch keeps the browser's
/// location indicator lit and the radio warm, and a magnetometer at 20 Hz is a
/// measurable share of a phone's idle power. Dropping the resource's `Option`s is
/// the whole unsubscribe protocol, on both platforms.
#[derive(Resource, Default)]
pub struct LocateTracking {
    /// The switch the locate button toggles, and the single source of truth for
    /// what that button draws. Deliberately *not* derived from `CameraState`: the
    /// camera can drop follow-and-rotate on its own (a drag), and a button that
    /// unlit itself while the GNSS radio was still running would be lying.
    pub active: bool,
    /// A device fix is driving the position this frame.
    pub gps_fix: bool,
    /// The device heading driving the view this frame. Also what the button's
    /// hover text reports, so a phone that is tracking but has no compass says so
    /// instead of looking identical to one that has.
    pub heading: Option<DeviceHeading>,
    /// Provider trouble worth telling the user about — a denied permission, a
    /// device with no magnetometer. Shown in the button's hover text.
    pub error: Option<String>,
    /// How much this frame's position can be trusted — the *same* classifier
    /// driver mode's chip reads (`driver::gps_quality`), for the reason its own
    /// note gives: the button and the navigation card answer one question about
    /// one watch, and two copies of the rule would eventually disagree on one
    /// screen at one moment.
    ///
    /// It exists because [`Self::active`] was doing two jobs and could only say
    /// one of them. The button lights on the *press* — before the permission
    /// dialog, before a cold GNSS start, and equally over a receiver that was
    /// refused — so "lit" never meant "working", and there was nowhere else to
    /// look: [`Self::gps_fix`] was set and rendered nowhere, and the accuracy was
    /// parsed by both providers and read by neither outside driver mode.
    pub gps: crate::systems::driver::GpsQuality,
    /// When the last *new* fix landed, for the staleness rung.
    ///
    /// A wall clock and not a frame count, because what it measures is the
    /// receiver going quiet rather than the app being busy. `GpsFix` carries no
    /// timestamp of its own — see `driver`'s note on why freshness is inferred
    /// from the value changing.
    last_fix_at: Option<web_time::Instant>,
    /// The press is still owed its jump: the next fix places the camera outright
    /// rather than nudging it. Armed by [`Self::enter`] and disarmed by whichever
    /// path actually moved the view, so a button pressed before the first fix
    /// arrives still centres when it does.
    recenter: bool,
    /// Where the last applied fix put the avatar, in absolute world metres
    /// (`GridCell` origin + local translation, so a cell boundary between two
    /// fixes is not a jump). The camera outside Avatar mode moves by the
    /// difference between successive fixes — see the module note on why that is
    /// not a re-centre every frame. `None` until the first fix, and reset
    /// whenever the world re-anchors under it (a teleport invalidates the frame
    /// this is measured in).
    last_place: Option<DVec3>,
    /// The last fix that was applied, so a report is acted on once rather than
    /// once per frame. `LocationWatch::latest` is a *level* — it answers with the
    /// provider's most recent value for as long as that is the most recent one —
    /// and the local write is idempotent, so this used not to matter. It does now
    /// that a fix also reaches the sim: without it every frame would send a
    /// placement (`driver::place_avatar`).
    last_fix: Option<GpsFix>,
    /// Was the camera in Avatar mode last frame? The rotate-follow rig is armed
    /// on the *transition* into it, so switching to Follow with the button lit
    /// turns the view exactly as pressing the button there would have — while a
    /// rotate-drag can still drop the rig (and, where there is no compass, end
    /// the tracking) without this re-arming it the frame after.
    in_follow: bool,
    watch: Option<LocationWatch>,
    compass: Option<OrientationWatch>,
}

impl LocateTracking {
    /// Start tracking. Idempotent, so a second press cannot open a second pair of
    /// subscriptions behind the first.
    pub fn enter(&mut self) {
        if self.active {
            return;
        }
        self.active = true;
        self.error = None;
        self.gps_fix = false;
        self.heading = None;
        self.recenter = true;
        self.last_place = None;
        self.last_fix = None;
        // Not `NoProvider`: the press is the moment the wait begins, and a button
        // that lit with no caption for the length of a cold GNSS start is the gap
        // this field was added to close. `track_device_pose` overwrites it on the
        // next frame with the real answer, including on a desktop.
        self.gps = crate::systems::driver::GpsQuality::Waiting;
        self.last_fix_at = None;
        // Unset rather than read off the camera: the first frame then treats
        // "already in Follow" as an arrival and arms the rig, so the button's own
        // press needs no special case beyond this one.
        self.in_follow = false;
        self.watch = Some(LocationWatch::start());
        self.compass = Some(OrientationWatch::start());
        tracing::info!(
            gps = LocationWatch::available(),
            compass = OrientationWatch::available(),
            "tracking this device"
        );
    }

    /// Put the switch into the state a live fix would have left it in, without a
    /// provider — the seam `--ui locate` uses to photograph the device arrow.
    ///
    /// **It exists because there is no other way to see this feature at all.**
    /// Every capture harness runs on a desktop or in a container, where
    /// [`LocationWatch::available`] is `false` and no coordinate is ever
    /// published; the arrow's projection, its silhouette and its rotation would
    /// otherwise ship having been reasoned about and never once looked at. What
    /// it writes is exactly what the provider writes and nothing more — the
    /// watches stay unopened, so this cannot be mistaken for the radio being on.
    pub fn seed_for_capture(&mut self, fix: GpsFix, heading: Option<DeviceHeading>) {
        self.active = true;
        self.gps_fix = true;
        self.last_fix = Some(fix);
        self.heading = heading;
        self.error = None;
        // The capture is of a *working* receiver, so it carries the accuracy the
        // seeded fix claims — otherwise `--ui locate` would photograph the button
        // in its searching state beside a marker that has plainly found itself.
        self.gps = crate::systems::driver::GpsQuality::Good(fix.accuracy_m as f32);
        self.last_fix_at = Some(web_time::Instant::now());
    }

    /// Stop tracking and drop both subscriptions.
    pub fn exit(&mut self) {
        if !self.active {
            return;
        }
        self.active = false;
        self.watch = None;
        self.compass = None;
        self.gps_fix = false;
        self.heading = None;
        self.error = None;
        self.recenter = false;
        self.last_place = None;
        self.last_fix = None;
        self.gps = crate::systems::driver::GpsQuality::default();
        self.last_fix_at = None;
        self.in_follow = false;
        tracing::info!("stopped tracking this device");
    }
}

/// One applied fix, in both of the frames the three camera arms need it in.
///
/// The two flat arms want anchored world metres; the globe wants the fix's place
/// *on the sphere*, which is a direction from the planet's centre and cannot be
/// recovered from the flat position — above [`GLOBE_SHOW_ALT`] the flat frame is
/// not what anything is drawn in. Carrying both is cheaper than making each arm
/// re-derive one from a fix it would otherwise not need to see.
///
/// `previous` is the last applied fix, or `None` for the press that started the
/// tracking — which is what makes that press a snap and every fix after it a
/// nudge (see the module note).
struct TrackedFix {
    /// Absolute anchored world metres: `GridCell` origin + local translation.
    place: DVec3,
    lat: f64,
    lng: f64,
    previous: Option<(DVec3, f64, f64)>,
}

impl TrackedFix {
    /// The previous fix in world metres, for the two flat arms.
    fn previous_place(&self) -> Option<DVec3> {
        self.previous.map(|(place, _, _)| place)
    }
}

/// The ground offset a watching point should keep from the tracked position.
///
/// `watcher` is whatever is doing the looking in world metres — the Fly camera,
/// or the Earth orbit's ground focus — and `previous` is the last tracked
/// position, or `None` for the press that started the tracking. `None` answers
/// `ZERO`, i.e. sit *on* the fix, which is what "take me to my location" means
/// and exactly what a teleport into either mode already does; otherwise the
/// watcher's own offset is carried, so the map slides under a walk without the
/// tracking confiscating the pan.
///
/// Y is dropped rather than carried: the two callers keep their height by
/// different means (an altitude, a distance along the orbit) and neither wants a
/// metre of avatar ground clearance folded into it. Pure, and in metres of the
/// anchored flat frame throughout, so the one thing that can be silently wrong
/// about it — a sign, or a frame — is testable without a `World`.
#[must_use]
fn tracked_ground_offset(watcher: DVec3, previous: Option<DVec3>) -> DVec3 {
    let Some(previous) = previous else {
        return DVec3::ZERO;
    };
    DVec3::new(watcher.x - previous.x, 0.0, watcher.z - previous.z)
}

/// Move the Fly camera / the Earth orbit onto a tracked position. Returns whether
/// there was a camera to move (so the press that armed the re-centre is only
/// counted as spent once it actually landed).
///
/// Three arms, because the three views are parameterised differently and each
/// already has a blessed "go here" recipe in
/// [`crate::systems::geo_nav::apply_teleport`]: Fly is a world transform and
/// keeps its altitude and its look, Earth is a ground focus that
/// `camera_controls` repositions from next, and the globe is a direction from the
/// planet's centre ([`CameraState::orbit_dir`]) that the same system rebuilds the
/// whole pose from.
///
/// **The globe used to be left alone, and that was wrong.** It returned `true`
/// without touching anything, on the argument that up there the orbit pivots the
/// Earth's centre rather than a ground focus, so a *near* fix — the avatar is
/// already under you — has nothing to move, and a far one would go through the
/// teleport anyway. Both halves fail together: the fix is near by construction
/// once the avatar is tracking it, so the teleport never fires, and "nothing to
/// move" is only true if you are already looking at the right part of the planet.
/// Pressing the button on the globe therefore did nothing at all, silently, and
/// spent the armed re-centre doing it — so dropping back to the surface
/// afterwards did not centre either. From orbit, "where am I" is a question about
/// which way the planet is facing, and that is exactly what `orbit_dir` says.
fn move_camera_to_place(
    camera_state: &mut CameraState,
    cameras: &mut Query<(&mut GridCell, &mut Transform), (With<FreeCamera>, Without<Avatar>)>,
    globe: &crate::systems::globe::GlobeState,
    fix: &TrackedFix,
) -> bool {
    let place = fix.place;
    let previous = fix.previous_place();
    match camera_state.mode {
        CameraMode::Avatar => false,
        CameraMode::Free => {
            let Ok((mut cell, mut tf)) = cameras.single_mut() else {
                return false;
            };
            let camera = cell_origin(&cell) + tf.translation.as_dvec3();
            let offset = tracked_ground_offset(camera, previous);
            // Altitude and look are the user's; only the ground under them moves.
            let target = DVec3::new(place.x + offset.x, camera.y, place.z + offset.z);
            let grid = Grid::new(WORLD_CELL_EDGE, WORLD_SWITCH_THRESHOLD);
            let (new_cell, local) = grid.translation_to_grid(target);
            *cell = new_cell;
            tf.translation = local;
            // Land at rest, the same reason a teleport does: fly inertia carried
            // through a re-centre slides the camera straight back off the fix.
            camera_state.free_velocity = Vec3::ZERO;
            true
        }
        CameraMode::Orbit if camera_state.orbit_space => {
            // Nothing to point at until the sphere is built: `direction_local`
            // answers `+Y` for every coordinate while `ready` is false, which
            // would swing the globe to the anchor's north pole and call it the
            // user's position. Report it as not landed so the armed re-centre
            // survives to the frame the globe exists.
            if !globe.ready {
                return false;
            }
            camera_state.orbit_dir = swing_globe(
                camera_state.orbit_dir,
                globe,
                (fix.lat, fix.lng),
                fix.previous.map(|(_, lat, lng)| (lat, lng)),
            );
            true
        }
        CameraMode::Orbit => {
            // An orbit that has not been seeded yet is about to be, from the view
            // ray, by `camera_controls` later this frame — so a focus written now
            // would simply be overwritten. Report it as not landed and let the
            // armed re-centre try again next frame, when the seed is in.
            if !camera_state.orbit_initialized {
                return false;
            }
            let offset = tracked_ground_offset(camera_state.orbit_focus, previous);
            camera_state.orbit_focus = DVec3::new(place.x + offset.x, 0.0, place.z + offset.z);
            true
        }
    }
}

// ─── The device arrow ────────────────────────────────────────────────────────

/// Half the arrow's length in screen points, and half its width. Fixed on the
/// screen rather than sized in metres for the reason the saved-place pins are
/// (`systems::place_pins`): this marker has to stay findable at 2 m and at
/// 20,000 km, and no size in metres does both.
const ARROW_HALF_LENGTH: f32 = 11.0;
const ARROW_HALF_WIDTH: f32 = 6.3;

/// How far ahead of the fix the heading is probed, as a fraction of the camera's
/// distance to it.
///
/// The arrow's screen angle is measured by projecting a *second* point in the
/// heading direction and taking the direction between the two on screen. Doing it
/// that way rather than from the camera's yaw is what makes one piece of code
/// serve the tilted Earth orbit, the free-flying camera and the globe alike — the
/// projection already knows about the tier, the tilt and the perspective, and a
/// hand-rolled angle would have to learn all three again and be wrong on the
/// sphere anyway.
///
/// It has to be a *fraction* of the viewing distance and not a distance: 30 m
/// ahead is several points of screen at street level and a millionth of a pixel
/// from orbit, where it would be pure floating-point noise and the arrow would
/// spin.
const HEADING_PROBE_FRACTION: f64 = 0.03;
/// …bounded, so the probe is neither shorter than GNSS noise nor a quarter of the
/// way round the planet (where a great-circle step stops being a local heading).
const HEADING_PROBE_MIN_M: f64 = 15.0;
const HEADING_PROBE_MAX_M: f64 = 300_000.0;
/// Below this the two projected points are the same pixel and their difference is
/// rounding, not a bearing. The arrow falls back to a plain disc.
const HEADING_MIN_SCREEN: f32 = 0.75;

/// Where you are, and which way you are pointing, for every view that isn't
/// standing behind the avatar.
///
/// **Follow mode needs no marker and does not get one:** the avatar *is* the
/// marker there — it is placed on the fix and turned by the compass, which is the
/// whole of `track_device_pose` above. Fly, Earth and the globe are the views
/// where tracking was on, working, and completely invisible: the camera moved to
/// sit over the fix and then nothing said which patch of roof was you.
///
/// **The accuracy halo is drawn in every mode including Follow**, and that is not
/// an inconsistency with the paragraph above. An avatar standing on the fix says
/// *where*; it cannot say *how well*, and a fix good to four metres and one good
/// to four hundred put the avatar down with identical confidence. That was the
/// state of the whole feature until this: the button lit, the position moved, and
/// nothing anywhere on screen distinguished a receiver locked on from one that
/// had been refused permission.
///
/// **Painted, not meshed, and the reasons are `place_pins`' verbatim.** A marker
/// sized in metres vanishes from altitude and is a tower at street level; this one
/// must read from both. It also has to work in the space tier, where the flat
/// frame the geometry would live in does not exist. So it is projected and drawn
/// in screen points, through the same two helpers the saved-place pins use.
///
/// **It paints and never senses.** The pins each own an `Area` because they are
/// clickable, and that is why they must *cull* rather than clip — a clipped-away
/// pin is still an interactive rectangle stealing taps. This arrow answers a
/// question and offers no action, so it is a bare painter: clipping it is enough,
/// and it can never take a tap meant for the dock underneath it.
#[allow(clippy::too_many_arguments)]
pub fn render_locate_arrow(
    mut egui: ResMut<crate::systems::egui_manager::EguiManager>,
    tracking: Res<LocateTracking>,
    camera_state: Res<CameraState>,
    cameras: Query<(&Camera, &GridCell, &Transform), With<FreeCamera>>,
    globe: Res<crate::systems::globe::GlobeState>,
    map_anchor: Res<MapAnchor>,
    terrain: Res<crate::systems::terrain::TerrainField>,
    keyboard: Res<crate::systems::onscreen_keyboard::OnScreenKeyboard>,
    route: Res<crate::systems::navigation::RouteState>,
    driver: Res<DriverState>,
) {
    // Driver mode draws its own arrow, in the world, and owns the screen besides.
    // Two "you are here" markers is one too many.
    if !tracking.active || driver.active {
        return;
    }
    // Follow mode keeps its exemption from the *marker* and loses it for the
    // halo. The avatar is the marker there — placed on the fix and turned by the
    // compass — but nothing about the avatar says how well the fix placed it, so
    // that view had no on-map evidence of the receiver working at all.
    let want_marker = camera_state.mode != CameraMode::Avatar;
    let (Some(fix), Some(anchor)) = (tracking.last_fix, map_anchor.geo) else {
        return;
    };
    let Ok((cam, cam_cell, cam_tf)) = cameras.single() else {
        return;
    };

    let map_rect = crate::systems::place_pins::map_area(&mut egui, &keyboard, &route);
    if map_rect.width() < 4.0 * ARROW_HALF_LENGTH || map_rect.height() < 4.0 * ARROW_HALF_LENGTH {
        return; // panels have taken the screen (compact layout)
    }
    let ui_scale = egui.ui_scale();
    let ctx = egui.ctx_mut().clone();

    let edge = WORLD_CELL_EDGE as f64;
    let cam_world = DVec3::new(
        cam_cell.x as f64 * edge + cam_tf.translation.x as f64,
        cam_cell.y as f64 * edge + cam_tf.translation.y as f64,
        cam_cell.z as f64 * edge + cam_tf.translation.z as f64,
    );
    let in_space = globe.in_space(cam_cell, cam_tf);
    // This frame's camera basis, rebuilt from `Transform` rather than read off
    // `GlobalTransform` — big_space propagates in `PostUpdate` while this runs in
    // `Update`, so the global one is a frame stale and the marker would swim over
    // a map that is standing still. `place_pins` documents the whole trap.
    let cam_view = GlobalTransform::from(Transform::from_rotation(cam_tf.rotation));
    let to_ui = |p: Vec2| egui::pos2(p.x / ui_scale, p.y / ui_scale);

    let project = |lat: f64, lng: f64| -> Option<egui::Pos2> {
        let delta = crate::systems::place_pins::place_delta(
            &globe, anchor, cam_world, in_space, lat, lng, &terrain,
        )?;
        cam.world_to_viewport(&cam_view, delta.as_vec3())
            .ok()
            .map(|p| to_ui(p))
    };
    let Some(here) = project(fix.lat, fix.lng) else {
        return; // over the horizon, behind the camera, or off the flat frame
    };
    if !map_rect.contains(here) {
        return;
    }

    // The compass first and the GNSS course second. The compass is where the
    // phone is *pointing*, which is what a "you are here" arrow means and what
    // the same reading is already doing to the avatar's facing in Follow mode;
    // the course over ground is the honest fallback for a device with no
    // magnetometer, and it is the better answer while actually travelling.
    let bearing = tracking
        .heading
        .map(|h| h.bearing_deg)
        .or(fix.heading_deg)
        .filter(|b| b.is_finite());
    let facing = bearing.and_then(|b| {
        let distance = crate::systems::place_pins::place_delta(
            &globe, anchor, cam_world, in_space, fix.lat, fix.lng, &terrain,
        )?
        .length();
        let probe =
            (distance * HEADING_PROBE_FRACTION).clamp(HEADING_PROBE_MIN_M, HEADING_PROBE_MAX_M);
        let (lat, lng) = step_along_bearing(fix.lat, fix.lng, b, probe);
        let ahead = project(lat, lng)?;
        let dir = ahead - here;
        (dir.length() >= HEADING_MIN_SCREEN).then(|| dir.normalized())
    });

    // The accuracy halo, measured the same way the heading is: project a second
    // point the reported radius away and take the distance between them on
    // screen. Doing the arithmetic in *screen* points rather than converting
    // metres by a scale factor is what makes one expression right in the tilted
    // Earth orbit, the free camera and the globe alike — the projection already
    // knows about the tier and the perspective foreshortening, and a hand-rolled
    // metres-per-point would have to learn both again. Due east, arbitrarily:
    // the halo is a circle, so any bearing gives the same radius, and the small
    // error from projecting a circle as one is far below the accuracy it draws.
    let accuracy_px = tracking.gps.accuracy_m().and_then(|m| {
        let (lat, lng) = step_along_bearing(fix.lat, fix.lng, 90.0, f64::from(m));
        Some((project(lat, lng)? - here).length())
    });
    let painter = ctx
        .layer_painter(egui::LayerId::new(
            egui::Order::Middle,
            egui::Id::new("locate_arrow"),
        ))
        .with_clip_rect(map_rect);
    paint_accuracy_halo(&painter, here, accuracy_px, map_rect);
    if want_marker {
        paint_device_marker(&painter, here, facing);
    }
}

/// The smallest halo worth drawing, in screen points. Below this it is inside the
/// marker and says nothing the marker does not — and a ring hugging the arrow
/// reads as part of the icon rather than as a claim about precision.
const HALO_MIN_PTS: f32 = ARROW_HALF_LENGTH * 1.6;
/// The largest, as a fraction of the map's shorter side. A network-derived fix
/// indoors reports kilometres, which at street zoom is a disc many screens wide:
/// drawn faithfully it is a flat wash of colour over the whole map, which reads as
/// a rendering fault rather than as uncertainty. Clamped, it reads as what it is —
/// "further than this view can show" — and the caption under the button carries
/// the number the clamp threw away.
const HALO_MAX_FRACTION: f32 = 0.45;

/// The circle of "somewhere in here", around the point drawn as "exactly there".
///
/// **This is the one signal that says the receiver is working *and* how well**,
/// and it is the reason a map app's blue dot has had one since the first of them.
/// The marker alone cannot: a dot placed from a 4 m fix and a dot placed from a
/// four-minute-old kilometre-wide guess are the same dot, drawn with the same
/// confidence, and the second one is the state a phone is in indoors, in a tunnel
/// and for the whole of a cold start.
///
/// Nothing is drawn when the halo is smaller than the marker: a fix that good has
/// no uncertainty worth showing, and the absence *is* the report.
fn paint_accuracy_halo(
    painter: &egui::Painter,
    here: egui::Pos2,
    radius: Option<f32>,
    map_rect: egui::Rect,
) {
    use crate::systems::theme;
    let Some(radius) = radius.filter(|r| r.is_finite() && *r >= HALO_MIN_PTS) else {
        return;
    };
    let radius = radius.min(map_rect.width().min(map_rect.height()) * HALO_MAX_FRACTION);
    // A wash and a hairline rather than a filled disc: the halo lies over the map
    // the marker is being read against, and anything heavier hides the street the
    // fix is claiming to be on. The alpha is low enough that overlapping it with
    // the route line, a place pin or the avatar leaves all of them legible.
    painter.circle_filled(
        here,
        radius,
        egui::Color32::from_rgba_unmultiplied(
            theme::ACCENT.r(),
            theme::ACCENT.g(),
            theme::ACCENT.b(),
            26,
        ),
    );
    painter.circle_stroke(
        here,
        radius,
        egui::Stroke::new(1.0, theme::ACCENT_RING.gamma_multiply(0.55)),
    );
}

/// Paint the marker: the navigation kite when a heading is known, a plain disc
/// when it is not.
///
/// **The disc is not a lesser arrow, it is a different claim.** A phone with no
/// magnetometer that has not moved knows where it is and genuinely does not know
/// which way it faces; drawing an arrow anyway would point somewhere with total
/// confidence and be wrong most of the time. This is the same rule the GPS
/// accuracy chip follows in driver mode — say the weaker thing rather than
/// dress it up as the stronger one.
fn paint_device_marker(painter: &egui::Painter, here: egui::Pos2, facing: Option<egui::Vec2>) {
    use crate::systems::theme;
    // A soft contact shadow, so the marker reads as lying on the map rather than
    // floating over it — the pins do the same and for the same reason.
    painter.circle_filled(
        here,
        ARROW_HALF_LENGTH * 0.55,
        egui::Color32::from_black_alpha(50),
    );
    let Some(forward) = facing else {
        painter.circle_filled(here, ARROW_HALF_WIDTH * 0.72, theme::ACCENT);
        painter.circle_stroke(
            here,
            ARROW_HALF_WIDTH * 0.72,
            egui::Stroke::new(2.0, theme::CREAM),
        );
        return;
    };
    // Screen-space right, for a screen whose y grows downward.
    let right = egui::vec2(-forward.y, forward.x);
    let at = |along: f32, across: f32| here + forward * along + right * across;

    // The same silhouette as the locate button's icon and driver mode's kite
    // (`driver::arrow_mesh`) — nose, left tail, the notch between them, right
    // tail — which is the point: the button, the marker and the vehicle read as
    // one object rather than as three unrelated pointers.
    let (l, w) = (ARROW_HALF_LENGTH, ARROW_HALF_WIDTH);
    let nose = at(l, 0.0);
    let left = at(-0.72 * l, -w);
    let notch = at(-0.30 * l, 0.0);
    let right_tail = at(-0.72 * l, w);

    // Filled as two triangles about the notch rather than as one polygon: the
    // kite is *concave* there, and epaint's fill assumes convexity — a concave
    // `convex_polygon` renders as a bow tie. The two share the nose–notch edge,
    // so with one colour and no per-triangle stroke there is no seam.
    for tri in [[nose, left, notch], [nose, notch, right_tail]] {
        painter.add(egui::Shape::convex_polygon(
            tri.to_vec(),
            theme::ACCENT,
            egui::Stroke::NONE,
        ));
    }
    // The rim goes round the whole outline in one closed path, which is what
    // hides the shared edge the fill was split on.
    painter.add(egui::Shape::closed_line(
        vec![nose, left, notch, right_tail],
        egui::Stroke::new(1.6, theme::CREAM),
    ));
}

/// The point `distance_m` away from `(lat, lng)` along a great circle on the
/// given bearing (degrees clockwise from true north).
///
/// Spherical rather than a step in the flat frame, because the caller uses it in
/// both tiers and the space tier has no flat frame to step in. Earth is not a
/// sphere, but the error over the few hundred kilometres this is ever asked for
/// is far below one screen pixel of arrow rotation.
///
/// `pub(crate)` for the shot harness, which needs the same step to seed
/// `--ui locate` a fix at the point the scripted camera is looking at.
#[must_use]
pub(crate) fn step_along_bearing(
    lat: f64,
    lng: f64,
    bearing_deg: f64,
    distance_m: f64,
) -> (f64, f64) {
    const EARTH_R_M: f64 = 6_371_008.8;
    let (phi, lambda) = (lat.to_radians(), lng.to_radians());
    let theta = bearing_deg.to_radians();
    let delta = distance_m / EARTH_R_M;
    let (sin_phi, cos_phi) = phi.sin_cos();
    let (sin_d, cos_d) = delta.sin_cos();
    let sin_phi2 = sin_phi.mul_add(cos_d, cos_phi * sin_d * theta.cos());
    let phi2 = sin_phi2.clamp(-1.0, 1.0).asin();
    let lambda2 = lambda + (theta.sin() * sin_d * cos_phi).atan2(sin_phi.mul_add(-sin_phi2, cos_d));
    (
        phi2.to_degrees(),
        (lambda2.to_degrees() + 540.0).rem_euclid(360.0) - 180.0,
    )
}

/// Where the globe's orbit should point after a tracked fix.
///
/// `orbit_dir` is the unit direction from the planet's centre to the camera, so
/// the sub-camera point is what the user is looking at. The press that starts the
/// tracking (`previous` is `None`) swings it *onto* the fix — that is what "take
/// me to my location" means, and it is the only way the globe can answer the
/// question at all.
///
/// Every fix after it is applied as the **rotation from the old fix to the new
/// one**, not as a re-centre. That is the same rule the two flat arms follow, in
/// the geometry the globe is in: a walk across a city carries the view across it
/// too, while a drag that spun the planet to look at somewhere else is kept — the
/// view is turned by however far you moved, not confiscated. Re-centring every
/// fix instead would be a globe the user cannot rotate while the button is lit.
///
/// Pure, and separated from the arm that calls it for the usual reason: a sign or
/// a reversed rotation here reads as a plausible globe pointing at the wrong
/// hemisphere, and that is testable without a `World`.
#[must_use]
fn swing_globe(
    orbit_dir: DVec3,
    globe: &crate::systems::globe::GlobeState,
    fix: (f64, f64),
    previous: Option<(f64, f64)>,
) -> DVec3 {
    let to = globe.direction_local(fix.0, fix.1).as_dvec3();
    let Some((plat, plng)) = previous else {
        return to;
    };
    let from = globe.direction_local(plat, plng).as_dvec3();
    // `from_rotation_arc` is undefined for a pair that is already identical or
    // exactly opposed, and two consecutive fixes are routinely the former — a
    // stationary receiver reports the same coordinate to the millimetre. Leave
    // the view alone rather than feeding it a degenerate rotation.
    let turn = to - from;
    if turn.length_squared() < 1e-18 {
        return orbit_dir;
    }
    let rotation = bevy::math::DQuat::from_rotation_arc(from, to);
    (rotation * orbit_dir).normalize_or_zero()
}

/// Follow the device: fixes onto the avatar, heading onto the camera.
#[allow(clippy::too_many_arguments)]
pub fn track_device_pose(
    mut tracking: ResMut<LocateTracking>,
    mut camera_state: ResMut<CameraState>,
    mut avatar_state: ResMut<AvatarState>,
    mut avatars: Query<(&mut GridCell, &mut Transform), (With<Avatar>, Without<FreeCamera>)>,
    mut cameras: Query<(&mut GridCell, &mut Transform), (With<FreeCamera>, Without<Avatar>)>,
    map_anchor: Res<MapAnchor>,
    mut nav: ResMut<GeoNavState>,
    driver: Res<DriverState>,
    // The sim owns the avatar's position online, so a fix that is only written
    // locally is erased by the next snapshot — `driver::place_avatar` is where the
    // fix is handed on, and it needs the session to do it.
    online: Option<Res<OnlineSession>>,
    // The globe's own frame. Only the space tier reads it, and only to turn a
    // (lat, lng) into a direction from the planet's centre.
    globe: Res<crate::systems::globe::GlobeState>,
) {
    // Two ways this ends without the button being pressed again.
    //
    // Driver mode owns the avatar's position, the camera rig and its own pair of
    // watches. Two systems writing one position from two subscriptions is not a
    // conflict that resolves visibly — it is a fix from whichever ran last — so
    // this one stands down rather than being merged with it.
    //
    // Changing the camera mode is *not* one of them any more: the button no
    // longer chooses a mode, so switching to Fly or Earth while it is lit is a
    // change of viewpoint and not a decision to stop tracking (see the module
    // note). This system still never *writes* the mode — the switch the user
    // reached for wins, and tracking follows it into whichever view it lands in.
    if driver.active || avatars.is_empty() {
        tracking.exit();
    }
    if !tracking.active {
        // The identity. Cleared on the way out — including an exit caused by
        // driver mode taking over — so the follow camera is never left aimed at a
        // heading nothing is updating any more. Guarded on it being set, because
        // touching `ResMut<CameraState>` is what marks the resource changed and
        // this system runs every frame of a session that never taps the button.
        if camera_state.follow_heading.is_some() {
            camera_state.follow_heading = None;
        }
        return;
    }

    // ── Position ──
    let fix = tracking.watch.as_ref().and_then(LocationWatch::latest);
    tracking.gps_fix = fix.is_some();
    // Read here rather than in the heading section below, where it used to live:
    // the classification a line further down needs to know about a refused
    // permission, and an error discovered after the position has been applied is
    // an error the button reports one frame late.
    let gps_error = tracking.watch.as_ref().and_then(LocationWatch::error);
    // A report we have not applied yet, as opposed to the same one being read
    // again — see [`LocateTracking::last_fix`].
    let fix_is_new = fix.is_some_and(|f| tracking.last_fix != Some(f));
    // Read before it is overwritten: the globe arm follows the *rotation between
    // two fixes*, which needs the one being replaced.
    let previous_geo = tracking.last_fix.map(|f| (f.lat, f.lng));
    let now = web_time::Instant::now();
    if fix_is_new {
        tracking.last_fix = fix;
        tracking.last_fix_at = Some(now);
    }
    // Freshness is inferred from the value *changing*, not from an age: `GpsFix`
    // carries no timestamp. `driver`'s note on the same line explains why that is
    // a compromise and why `GPS_STALE_S` is generous because of it.
    tracking.gps = crate::systems::driver::gps_quality(
        LocationWatch::available(),
        gps_error.is_some(),
        fix.map(|f| f.accuracy_m),
        tracking
            .last_fix_at
            .map(|t| now.duration_since(t).as_secs_f32()),
    );
    if let (Some(fix), Some(anchor)) = (fix, map_anchor.geo) {
        // Near or far, exactly as driver mode decides it: a *move* is a write and
        // a *jump* is a teleport, because nothing on the write path re-anchors the
        // map and the world is built around the avatar. Tapping the button in a
        // city you were only browsing is the everyday long jump.
        if crate::systems::driver::fix_is_far(anchor, fix, &avatars) {
            tracing::info!(
                lat = fix.lat,
                lng = fix.lng,
                "device fix is far from the anchored world — teleporting"
            );
            // Silent: a device fix past `GPS_TELEPORT_M` is the world catching up
            // with where the phone already is, and it can fire repeatedly.
            nav.pending_teleport = Some(Teleport::geo(fix.lat, fix.lng, None).silently());
            // `apply_teleport` brings the camera in every mode and re-anchors the
            // world under it, so the jump is paid for and the frame `last_place`
            // was measured in no longer exists.
            tracking.recenter = false;
            tracking.last_place = None;
        } else {
            crate::systems::driver::place_avatar(
                anchor,
                fix.lat,
                fix.lng,
                &mut avatar_state,
                &mut avatars,
                if fix_is_new { online.as_deref() } else { None },
            );
            // Where that put the avatar, in absolute world metres — recorded in
            // every mode, not only the two that read it below. Recording it only
            // while the camera needed it would leave a spell in Follow as a gap,
            // and the first fix after it would hand the delta a `place` from
            // before the gap and throw the view that far across the map.
            if let Ok((cell, tf)) = avatars.single() {
                let place = cell_origin(cell) + tf.translation.as_dvec3();
                let previous = tracking.last_place.replace(place);
                // Follow mode's rig is already glued to the avatar; the other
                // three views are not, so they are moved here — snapped on the
                // press that armed `recenter`, nudged by the fix's own movement
                // after that. Dropping the previous fix is what makes it a snap,
                // so both frames of it go at once or the globe would nudge while
                // the flat arms centred.
                if camera_state.mode != CameraMode::Avatar {
                    let previous = if tracking.recenter {
                        None
                    } else {
                        previous
                            .zip(previous_geo)
                            .map(|(p, (lat, lng))| (p, lat, lng))
                    };
                    let tracked = TrackedFix {
                        place,
                        lat: fix.lat,
                        lng: fix.lng,
                        previous,
                    };
                    if move_camera_to_place(&mut camera_state, &mut cameras, &globe, &tracked) {
                        tracking.recenter = false;
                    }
                }
            }
        }
    }

    // ── Heading ──
    let heading = tracking.compass.as_ref().and_then(OrientationWatch::latest);
    // Location first: a denied position is the more consequential of the two, and
    // the button's caption holds one message.
    let error = gps_error.or_else(|| tracking.compass.as_ref().and_then(OrientationWatch::error));
    tracking.heading = heading;
    tracking.error = error;

    // Only the Follow camera has a rotate sub-state to hand a bearing to. In Fly
    // and Earth the view direction is the user's own — a bearing there would spin
    // a camera nobody asked to turn, and `follow_rotate` would sit set behind the
    // mode waiting to surprise whoever switched back. Reported heading is still
    // read above, because the button's caption says what the compass answers
    // whether or not anything is consuming it.
    let in_follow = camera_state.mode == CameraMode::Avatar;
    // Arriving in Follow with the button lit arms the rig, which is what makes
    // "the rotation is kept in sync in Avatar mode" true however the mode was
    // reached — the button's own press, Tab, or the mode disc. On the transition
    // only: held every frame it would be a view the rotate-drag cannot escape,
    // and on a platform with no compass that drag is the documented way to stop.
    if in_follow && !tracking.in_follow {
        camera_state.follow_rotate = true;
    }
    tracking.in_follow = in_follow;
    if !in_follow {
        if camera_state.follow_heading.is_some() {
            camera_state.follow_heading = None;
        }
        return;
    }

    match heading {
        Some(h) => {
            // Held, not set — see the module note on why a drag must not end this.
            camera_state.follow_rotate = true;
            #[allow(clippy::cast_possible_truncation)]
            let bearing = (h.bearing_deg as f32).to_radians();
            let yaw = yaw_from_bearing(bearing);
            camera_state.follow_heading = Some(yaw);
            // The avatar turns with the phone too, and it has to: the camera is
            // *behind* the avatar, so turning only the camera orbits the model and
            // the marker for "me" ends up pointing somewhere I am not. This is the
            // same yaw and the same convention `free_camera` reads, so the two
            // cannot disagree.
            //
            // Only while standing still. `avatar::handle_avatar_movement` turns
            // the avatar toward where it is *travelling*, which is the right
            // answer whenever there is a walk input — the on-screen stick is
            // reachable on the same phone this compass is on — and overwriting it
            // every frame would leave the avatar walking sideways with the run
            // clip playing.
            if !avatar_state.is_walking {
                avatar_state.facing_yaw = yaw;
            }
        }
        None => {
            if camera_state.follow_heading.is_some() {
                camera_state.follow_heading = None;
            }
            // No compass on this platform at all: the button is the old
            // follow-and-rotate, so give it the old escape hatch — a rotate-drag
            // clears `follow_rotate` in `free_camera`, and that ends the tracking.
            // Where a compass *does* exist this must not fire: it would end the
            // mode during the seconds before the first sample arrives.
            if !OrientationWatch::available() && !camera_state.follow_rotate {
                tracking.exit();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The four cardinals, checked through the same arithmetic `free_camera` does
    /// — yaw → azimuth → the ground direction the camera looks along — so the test
    /// pins the thing that is actually visible rather than the intermediate.
    #[test]
    fn the_camera_looks_along_the_bearing() {
        for (bearing_deg, expected) in [
            (0.0_f32, Vec2::new(0.0, -1.0)), // north is −Z
            (90.0, Vec2::new(1.0, 0.0)),     // east is +X
            (180.0, Vec2::new(0.0, 1.0)),    // south
            (270.0, Vec2::new(-1.0, 0.0)),   // west
        ] {
            let az = yaw_from_bearing(bearing_deg.to_radians()) + std::f32::consts::PI;
            // `free_camera`'s boom is `(sin az, ·, cos az)` and it looks back at
            // the avatar, so screen-up over the ground is the negation.
            let looks = -Vec2::new(az.sin(), az.cos());
            assert!(
                (looks - expected).length() < 1e-5,
                "bearing {bearing_deg}° should look {expected:?}, looks {looks:?}"
            );
        }
    }

    #[test]
    fn the_yaw_stays_in_one_turn() {
        for deg in [-720.0_f32, -1.0, 0.0, 179.0, 360.0, 721.0] {
            let y = yaw_from_bearing(deg.to_radians());
            assert!((0.0..std::f32::consts::TAU).contains(&y), "{deg}° gave {y}");
        }
    }

    /// The press that starts the tracking is a "take me there": the camera sits
    /// on the fix, whatever ground offset it happened to have before.
    #[test]
    fn the_first_fix_puts_the_camera_on_it() {
        let watcher = DVec3::new(400.0, 1_200.0, -900.0);
        assert_eq!(tracked_ground_offset(watcher, None), DVec3::ZERO);
    }

    /// Every fix after it moves the camera by however far the fix moved, so a
    /// walk slides the map without the tracking taking the pan away.
    #[test]
    fn a_later_fix_carries_the_offset_the_user_chose() {
        // Watching from 300 m east and 50 m south of where the last fix landed.
        let previous = DVec3::new(100.0, 0.05, 100.0);
        let watcher = DVec3::new(400.0, 1_200.0, 150.0);
        let offset = tracked_ground_offset(watcher, Some(previous));
        assert_eq!(offset, DVec3::new(300.0, 0.0, 50.0));
        // …and applied to a fix 20 m north, the camera has moved exactly 20 m
        // north too: the offset it was given is intact.
        let place = DVec3::new(100.0, 0.05, 80.0);
        let target = place + offset;
        assert_eq!(target.x - watcher.x, 0.0);
        assert_eq!(target.z - watcher.z, -20.0);
    }

    /// Height is the caller's business — an altitude in Fly, a distance along the
    /// orbit in Earth — so the avatar's ground clearance must not leak into it.
    #[test]
    fn the_offset_is_flat() {
        let offset = tracked_ground_offset(
            DVec3::new(0.0, 1_200.0, 0.0),
            Some(DVec3::new(0.0, 0.05, 0.0)),
        );
        assert_eq!(offset.y, 0.0);
    }

    /// The four cardinals, in metres on the ground. A step north must move the
    /// latitude and leave the longitude alone; a step east must do the opposite.
    /// Every way of getting this wrong — a swapped pair, degrees where radians
    /// belong, a sign — points the arrow confidently in a direction the phone is
    /// not facing, which is worse than drawing no arrow at all.
    #[test]
    fn a_step_along_a_bearing_goes_where_it_says() {
        let (lat, lng) = (53.2194, 6.5665);
        let km = 1_000.0;

        let (n_lat, n_lng) = step_along_bearing(lat, lng, 0.0, km);
        assert!(n_lat > lat, "north must raise the latitude");
        assert!(
            (n_lng - lng).abs() < 1e-6,
            "north must not move the longitude"
        );
        // One kilometre is ~0.009° of latitude anywhere on Earth.
        assert!(
            (n_lat - lat - 0.00899).abs() < 1e-4,
            "1 km north read as {}° of latitude",
            n_lat - lat
        );

        let (e_lat, e_lng) = step_along_bearing(lat, lng, 90.0, km);
        assert!(e_lng > lng, "east must raise the longitude");
        assert!(
            (e_lat - lat).abs() < 1e-4,
            "east must barely move the latitude"
        );
        // …and a degree of longitude is shorter this far north, so the same
        // kilometre is a bigger step in degrees than it was in latitude.
        assert!(
            e_lng - lng > n_lat - lat,
            "at 53°N a kilometre east is more degrees than a kilometre north"
        );

        let (s_lat, _) = step_along_bearing(lat, lng, 180.0, km);
        assert!(s_lat < lat, "south must lower the latitude");
        let (_, w_lng) = step_along_bearing(lat, lng, 270.0, km);
        assert!(w_lng < lng, "west must lower the longitude");
    }

    /// Longitude must come back inside ±180 rather than running off the end of
    /// the world: a step east from Fiji is a valid coordinate, and every consumer
    /// downstream feeds it to a Mercator projection that has no idea what 181°
    /// means.
    #[test]
    fn a_step_across_the_date_line_wraps() {
        let (_, lng) = step_along_bearing(-17.7, 179.95, 90.0, 20_000.0);
        assert!(
            (-180.0..=180.0).contains(&lng),
            "longitude left the world at {lng}"
        );
        assert!(
            lng < 0.0,
            "east of 179.95° is the western hemisphere, got {lng}"
        );
    }

    /// A globe built around Groningen, near enough for the direction maths: the
    /// planet's centre is one geocentric radius straight down from the anchor, so
    /// the anchor's own direction is `+Y`.
    fn test_globe() -> crate::systems::globe::GlobeState {
        const R: f32 = 6_365_000.0;
        // Built by mutation rather than a struct literal: `GlobeState` owns
        // private streaming bookkeeping, so the literal form is not available
        // outside its own module.
        let mut globe = crate::systems::globe::GlobeState::default();
        globe.ready = true;
        globe.center_local = Vec3::new(0.0, -R, 0.0);
        globe.radius = R;
        globe.anchor_lat = 53.2194;
        globe.anchor_lng = 6.5665;
        globe
    }

    /// The press that starts the tracking swings the planet onto the fix — from
    /// orbit that is the only way the button can answer "where am I" at all, and
    /// it is what the globe arm used to skip entirely.
    #[test]
    fn the_first_fix_swings_the_globe_onto_it() {
        let globe = test_globe();
        // Looking at the other side of the planet to begin with.
        let looking_away = DVec3::new(0.0, -1.0, 0.0);
        let landed = swing_globe(
            looking_away,
            &globe,
            (globe.anchor_lat, globe.anchor_lng),
            None,
        );
        // The anchor sits one radius above the centre, so its direction is +Y.
        assert!(
            (landed - DVec3::Y).length() < 1e-6,
            "the globe should point at the anchor, points {landed:?}"
        );
    }

    /// Every fix after it turns the view by however far the fix moved and keeps
    /// whatever offset the user's own drag had put there — the globe's version of
    /// "the map slides under a walk without the tracking confiscating the pan".
    #[test]
    fn a_later_fix_turns_the_globe_without_taking_the_drag() {
        let globe = test_globe();
        let first = (53.0, 6.0);
        let second = (53.5, 6.0); // ~55 km north
        let d1 = globe.direction_local(first.0, first.1).as_dvec3();
        let d2 = globe.direction_local(second.0, second.1).as_dvec3();
        // The user has dragged the planet away from where the first fix was.
        let dragged = globe.direction_local(52.0, 5.0).as_dvec3();
        let offset_before = dragged.dot(d1).clamp(-1.0, 1.0).acos();

        let after = swing_globe(dragged, &globe, second, Some(first));

        let offset_after = after.dot(d2).clamp(-1.0, 1.0).acos();
        assert!(
            (offset_before - offset_after).abs() < 1e-6,
            "the drag's offset should survive: {offset_before} before, {offset_after} after"
        );
        // …and the turn really is the one between the two fixes: a view that was
        // sitting exactly on the old fix lands exactly on the new one. Stated
        // this way rather than as "the view turned by the angle the fix moved",
        // which is only true for a view perpendicular to the rotation axis —
        // every other view traces a smaller circle around it.
        let carried = swing_globe(d1, &globe, second, Some(first));
        assert!(
            (carried - d2).length() < 1e-6,
            "a view on the old fix should land on the new one, landed {carried:?}"
        );
        assert!(
            d2.dot(d1).clamp(-1.0, 1.0).acos() > 1e-4,
            "the two fixes must actually differ, or the test proves nothing"
        );
    }

    /// A receiver that keeps reporting the same coordinate must leave the view
    /// alone: `from_rotation_arc` is undefined for an identical pair, and a
    /// stationary phone reports one all day.
    #[test]
    fn an_unchanged_fix_leaves_the_globe_where_it_is() {
        let globe = test_globe();
        let here = (53.2194, 6.5665);
        let looking = globe.direction_local(40.0, -3.0).as_dvec3();
        let after = swing_globe(looking, &globe, here, Some(here));
        assert!(
            (after - looking).length() < 1e-9,
            "the view moved to {after:?}"
        );
    }

    /// Entering twice must not open a second pair of subscriptions, and leaving
    /// must drop both — the whole unsubscribe protocol is these `Option`s.
    #[test]
    fn the_switch_owns_exactly_one_pair_of_watches() {
        let mut t = LocateTracking::default();
        t.enter();
        assert!(t.active && t.watch.is_some() && t.compass.is_some());
        // And the press owes a jump — armed here, spent by whichever camera arm
        // lands it, so a button pressed before the first fix still centres.
        assert!(t.recenter && t.last_place.is_none());
        t.enter();
        assert!(t.active);
        t.exit();
        assert!(!t.active && t.watch.is_none() && t.compass.is_none());
        assert!(!t.recenter && t.last_place.is_none());
        // And leaving twice is not an error either — `render_map_controls` and
        // `track_device_pose` can both decide the mode is over in the same frame.
        t.exit();
        assert!(!t.active);
    }
}
