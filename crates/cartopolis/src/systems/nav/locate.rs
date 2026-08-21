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
//! **It is deliberately not driver mode.** `systems::driver` does all of this and
//! a great deal more — it needs a solved route, strips the map to a navigation
//! display, takes the camera rig, narrates manoeuvres and reroutes when you leave
//! the line. What is here is the part of it that makes sense with no destination
//! at all: a map that knows where you are standing and which way you are facing.
//! The two never run together (driver mode wins, and this stands down), so there
//! is exactly one owner of the avatar's position at any moment.
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

use bevy::prelude::*;
use big_space::prelude::GridCell;

use crate::components::Avatar;
use crate::platform::location::LocationWatch;
use crate::platform::orientation::{DeviceHeading, OrientationWatch};
use crate::resources::{AvatarState, CameraMode, CameraState, MapAnchor};
use crate::systems::driver::DriverState;
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
        tracing::info!("stopped tracking this device");
    }
}

/// Follow the device: fixes onto the avatar, heading onto the camera.
pub fn track_device_pose(
    mut tracking: ResMut<LocateTracking>,
    mut camera_state: ResMut<CameraState>,
    mut avatar_state: ResMut<AvatarState>,
    mut avatars: Query<(&mut GridCell, &mut Transform), With<Avatar>>,
    map_anchor: Res<MapAnchor>,
    mut nav: ResMut<GeoNavState>,
    driver: Res<DriverState>,
) {
    // Three ways this ends without the button being pressed again.
    //
    // Driver mode owns the avatar's position, the camera rig and its own pair of
    // watches. Two systems writing one position from two subscriptions is not a
    // conflict that resolves visibly — it is a fix from whichever ran last — so
    // this one stands down rather than being merged with it.
    //
    // Leaving Avatar mode (Tab, the mode disc) ends it too, and that is the
    // reason this system never *writes* the mode: forcing it back each frame would
    // make the camera switcher silently inoperative while a sensor happened to be
    // on, with nothing on screen to explain it. The switch the user reached for
    // wins, and the button goes dark to say the tracking went with it.
    if driver.active || avatars.is_empty() || camera_state.mode != CameraMode::Avatar {
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
        } else {
            crate::systems::driver::place_avatar(
                anchor,
                fix.lat,
                fix.lng,
                &mut avatar_state,
                &mut avatars,
            );
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

    /// Entering twice must not open a second pair of subscriptions, and leaving
    /// must drop both — the whole unsubscribe protocol is these `Option`s.
    #[test]
    fn the_switch_owns_exactly_one_pair_of_watches() {
        let mut t = LocateTracking::default();
        t.enter();
        assert!(t.active && t.watch.is_some() && t.compass.is_some());
        t.enter();
        assert!(t.active);
        t.exit();
        assert!(!t.active && t.watch.is_none() && t.compass.is_none());
        // And leaving twice is not an error either — `render_map_controls` and
        // `track_device_pose` can both decide the mode is over in the same frame.
        t.exit();
        assert!(!t.active);
    }
}
