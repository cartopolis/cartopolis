//! Track the phone: where it is (GNSS) and which way it is pointing (compass).
//!
//! This is what the locate arrow in the left-hand column does. One switch, two
//! sensors, and the shortest path from either to the view:
//!
//! - the **position** moves the avatar, because the avatar is what the camera
//!   follows, what the streamers anchor around and what every readout already
//!   measures from. Writing the fix anywhere else would mean a second notion of
//!   "here";
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
use crate::platform::location::LocationWatch;
use crate::platform::orientation::{DeviceHeading, OrientationWatch};
use crate::resources::{AvatarState, CameraMode, CameraState, MapAnchor};
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
        self.in_follow = false;
        tracing::info!("stopped tracking this device");
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
/// Two arms, because the two modes are parameterised differently and each already
/// has a blessed "go here" recipe in [`crate::systems::geo_nav::apply_teleport`]:
/// Fly is a world transform and keeps its altitude and its look, Earth is a
/// ground focus that `camera_controls` repositions from next. The space tier is
/// left alone on purpose — up there the orbit is about the Earth's centre and the
/// ground focus is not read, so a fix that is *near* (the avatar is already under
/// you) has nothing to move; a far one goes through the teleport, which drops out
/// of space onto the target.
fn move_camera_to_place(
    camera_state: &mut CameraState,
    cameras: &mut Query<(&mut GridCell, &mut Transform), (With<FreeCamera>, Without<Avatar>)>,
    place: DVec3,
    previous: Option<DVec3>,
) -> bool {
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
        CameraMode::Orbit => {
            if camera_state.orbit_space {
                return true;
            }
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
            nav.pending_teleport = Some(Teleport::geo(fix.lat, fix.lng, None));
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
            );
            // Where that put the avatar, in absolute world metres — recorded in
            // every mode, not only the two that read it below. Recording it only
            // while the camera needed it would leave a spell in Follow as a gap,
            // and the first fix after it would hand the delta a `place` from
            // before the gap and throw the view that far across the map.
            if let Ok((cell, tf)) = avatars.single() {
                let place = cell_origin(cell) + tf.translation.as_dvec3();
                let previous = tracking.last_place.replace(place);
                // Follow mode's rig is already glued to the avatar; the other two
                // are not, so they are moved here — snapped on the press that
                // armed `recenter`, nudged by the fix's own movement after that.
                if camera_state.mode != CameraMode::Avatar {
                    let shift = if tracking.recenter { None } else { previous };
                    if move_camera_to_place(&mut camera_state, &mut cameras, place, shift) {
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
    let error = tracking
        .watch
        .as_ref()
        .and_then(LocationWatch::error)
        .or_else(|| tracking.compass.as_ref().and_then(OrientationWatch::error));
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
