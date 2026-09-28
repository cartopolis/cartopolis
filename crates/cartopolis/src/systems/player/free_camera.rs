use crate::components::FreeCamera;
use crate::grid::{WORLD_CELL_EDGE, WORLD_SWITCH_THRESHOLD};
use bevy::camera::Exposure;
use bevy::light::atmosphere::ScatteringMedium;
use bevy::light::{AmbientLight, Atmosphere, AtmosphereEnvironmentMapLight, NotShadowCaster};
use bevy::math::{DQuat, DVec3};
use bevy::pbr::AtmosphereSettings;
use bevy::post_process::bloom::Bloom;
use bevy::prelude::*;
use big_space::prelude::{BigSpaceCommands, FloatingOrigin, Grid, GridCell};
use cartopolis_core::motion;

use crate::resources::{AvatarState, CameraMode, CameraState};
use crate::systems::egui_manager::EguiManager;
use crate::systems::gizmo::GizmoState;

/// Builds the camera's [`DistanceFog`] for a dev-panel density in 0.0–1.0 and a
/// time of day. The falloff is exponential-squared so the near field stays clear
/// and geometry vanishes into a fog "wall"; the fog color follows the sky — bright
/// haze by day, warm-tinted through sunrise/sunset (fog is lit by the same
/// extinction-reddened sun the sky shows), near-black murk at night.
/// How much of the sun's transmittance colour the fog's sun-side glow keeps
/// (1 = the raw beam colour, 0 = white).
const GLOW_SATURATION: f32 = 0.5;

pub fn distance_fog(density: f32, hours: f32, decl_rad: f32) -> DistanceFog {
    use crate::utils::sun;

    let day = sun::daylight(hours, decl_rad); // smooth through twilight, not a hard ramp
    let night_color = Vec3::new(0.015, 0.02, 0.035);
    // Slightly desaturated toward white so the fog wall meets the Mie-hazed sky.
    let day_color = Vec3::new(0.71, 0.75, 0.83);
    // Tint by the sun's transmittance color while it sits near the horizon, so
    // dawn/dusk haze glows warm with the sky instead of snapping grey→black.
    let t = sun::sun_transmittance(hours, decl_rad);
    let tint = t / t.max_element().max(1e-4);
    // Modest warm tint only — the built-in atmosphere already colours the sky at
    // low sun, so a strong red fog on top read as an unrealistic blood-red dusk.
    let warm = 0.30 * sun::horizon_proximity(hours, decl_rad);
    let c = night_color.lerp(day_color, day) * Vec3::ONE.lerp(tint, warm);

    // Map density to a visibility distance on an exponential curve:
    // 0.15 → ~7 km haze, 0.5 → ~1.2 km fog, 1.0 → ~90 m pea soup.
    let falloff = if density > 0.001 {
        FogFalloff::from_visibility_squared(15_000.0 * 0.006_f32.powf(density))
    } else {
        FogFalloff::Linear {
            start: f32::MAX,
            end: f32::MAX,
        }
    };

    DistanceFog {
        color: Color::srgb(c.x, c.y, c.z),
        // Sun-side glow in the fog, colored like the sun itself (white by day,
        // orange as it sets); fades out through early twilight. The tint is
        // desaturated toward white and kept faint on purpose: at full
        // transmittance colour the glow is *pure* red at dusk, and with the
        // twilight exposure lift on top it flooded the sun half of the frame
        // with one saturated red sheet, ground included.
        directional_light_color: {
            let g = Vec3::ONE.lerp(tint, GLOW_SATURATION);
            Color::srgba(
                g.x,
                g.y,
                g.z,
                0.35 * sun::smoothstep(-0.08, 0.02, sun::sun_direction(hours, decl_rad).y),
            )
        },
        directional_light_exponent: 30.0,
        falloff,
    }
}

/// Ground fog is full-strength up to this altitude (rooftop / low-flight haze).
const FOG_FADE_LOW: f32 = 800.0;
/// …and has thinned to nothing by this altitude, so an orbit/high-flight view
/// looks down on the map through clear air instead of a grey wall.
const FOG_FADE_HIGH: f32 = 5000.0;

/// How present the atmospheric ground fog should be at a given camera altitude:
/// 1.0 near the ground, ramping to 0.0 by [`FOG_FADE_HIGH`]. Real haze is densest
/// near the surface; this mirrors that so climbing out reveals the world rather
/// than drowning it in fog (see `apply_distance_fog` / `update_horizon_haze`).
pub fn fog_altitude_factor(altitude_m: f32) -> f32 {
    1.0 - crate::utils::sun::smoothstep(FOG_FADE_LOW, FOG_FADE_HIGH, altitude_m)
}

/// Distance the horizon-haze shell stands in for: the far edge of the streamed
/// map (the coarsest clipmap ring, ~82 km out at mid latitude). A horizon ray
/// leaves the map there, and the shell's job is to carry the ground's own fog
/// past that edge. The exact value barely matters — the fog has saturated by
/// ~15 km at every density in play — it only has to be well beyond visibility.
pub const HORIZON_FOG_DISTANCE: f32 = 82_000.0;

/// Fog opacity at which the ground behind it carries no information left to see,
/// so the horizon shell may stand in front of it. Not 1.0: the exponential curves
/// only reach that asymptotically.
const FOG_SATURATED: f32 = 0.995;

/// The distance at which the fog reaches [`FOG_SATURATED`] — where the ground
/// stops contributing anything and the horizon shell can take over.
///
/// This is what decides how *far away* the shell sits, and the reason it must sit
/// close is Bevy's atmosphere. `render_sky` composites `scene × transmittance +
/// inscattering` over geometry, but the transmittance is computed for the true
/// distance while the in-scattering is clamped at `aerial_view_lut_max_distance`
/// (32 km). Ground beyond that is therefore dimmed without the airlight that
/// should replace it — measured at 882 m, saturated fog colour `#9DA4B2` came out
/// `#949297` between 30 and 150 km, a warm band along the horizon with a hard top
/// edge at the ground disc's rim. The shell can't cover it from 180 km, behind
/// that ground; from inside the saturation distance it can, and painting fog
/// colour over ground that is already ≥99.5% fog colour costs nothing. (Raising
/// the LUT distance instead does not work — verified, it only moves which slice
/// the clamp lands on.)
pub fn fog_saturation_distance(falloff: &FogFalloff) -> f32 {
    // ln(1 / (1 - f)) — the optical depth that reaches FOG_SATURATED.
    let depth = -(1.0 - FOG_SATURATED).ln();
    match falloff {
        FogFalloff::Linear { end, .. } => *end,
        FogFalloff::Exponential { density } => depth / density.max(f32::MIN_POSITIVE),
        FogFalloff::ExponentialSquared { density } => depth.sqrt() / density.max(f32::MIN_POSITIVE),
        FogFalloff::Atmospheric { extinction, .. } => {
            depth / extinction.max_element().max(f32::MIN_POSITIVE)
        }
    }
}

/// How opaque a [`DistanceFog`] is at `distance` — the same curves `bevy_pbr`
/// applies to geometry. Lets the horizon shell read its strength off the fog it
/// continues instead of approximating it: any mismatch is a visible line at the
/// horizon, since the ground on one side of it is saturated fog colour.
pub fn fog_factor_at(falloff: &FogFalloff, distance: f32) -> f32 {
    match falloff {
        // Fog-off is spelled `Linear { start: MAX, end: MAX }` (see
        // `distance_fog`), which lands on 0 here — the shell hides itself.
        FogFalloff::Linear { start, end } => {
            ((distance - start) / (end - start).max(f32::EPSILON)).clamp(0.0, 1.0)
        }
        FogFalloff::Exponential { density } => 1.0 - (-distance * density).exp(),
        FogFalloff::ExponentialSquared { density } => {
            let d = distance * density;
            1.0 - (-d * d).exp()
        }
        // Atmospheric extinction is per-channel; the shell carries one alpha, so
        // take the channel that closes first.
        FogFalloff::Atmospheric { extinction, .. } => {
            1.0 - (-distance * extinction.max_element()).exp()
        }
    }
}

// Simple constants
const FREE_CAMERA_SPEED: f32 = 45.0;
const FREE_CAMERA_SPEED_FAST: f32 = 180.0;
const MOUSE_SENSITIVITY: f32 = 0.002;
const MIN_CAMERA_HEIGHT: f32 = 0.5;

/// Altitude (m) at which the fly speeds above are taken literally; above it the
/// speed grows in proportion to altitude, so how fast the ground slides past
/// stays roughly constant as you climb (the Google Earth rule — a fixed 45 m/s
/// is a brisk walk over a city and a crawl over a continent).
const FREE_SPEED_ALT_REF: f32 = 120.0;
/// Ceiling on that scaling. Past ~240 km up the speed stops growing; without a
/// cap the 45,000 km space orbit would multiply it by six figures and a tap of W
/// would leave the solar system.
const FREE_SPEED_MAX_FACTOR: f32 = 2_000.0;
/// Fly inertia, as exponential-approach time constants (s): the velocity chases
/// the key wish quickly, and coasts to rest more slowly when the keys let go.
const FREE_ACCEL_TAU: f32 = 0.12;
const FREE_DECEL_TAU: f32 = 0.5;
/// Coasting stops for good below this fraction of the current fly speed. The
/// camera is the big_space floating origin, so a velocity that only ever halves
/// would rewrite its transform — and re-propagate every entity in the scene —
/// for the rest of the session.
const FREE_REST_FRACTION: f32 = 0.005;

/// Fly speed (m/s) at `altitude_m` above the surface, for a base speed of
/// `base` — [`FREE_SPEED_ALT_REF`] and [`FREE_SPEED_MAX_FACTOR`] applied.
fn fly_speed(base: f32, altitude_m: f32) -> f32 {
    base * (altitude_m / FREE_SPEED_ALT_REF).clamp(1.0, FREE_SPEED_MAX_FACTOR)
}

/// One exponential zoom step applied to the Earth-mode zoom *target* (the
/// camera glides toward it, so this never moves the view directly). Shared by
/// the wheel and the pinch, which differ only in where the factor comes from.
///
/// In space the factor scales the **altitude**, not the distance from the
/// Earth's centre: at the boundary a 10% step on the distance is 10% of
/// ~6,400 km — a 640 km plunge — instead of 10% of the height above the ground.
fn zoom_orbit_target(target: f64, factor: f64, space: bool, radius: f64, clamp: (f64, f64)) -> f64 {
    let (min_d, max_d) = clamp;
    if space {
        let alt = (target - radius).max(500.0);
        (radius + alt * factor).clamp(min_d, max_d)
    } else {
        (target * factor).clamp(min_d, max_d)
    }
}

/// How much closer (or further) one frame of a *held* on-screen zoom button is
/// worth: a multiplier on the distance, so the step is a fraction of where you
/// already are, exactly as the wheel's is.
///
/// Exponential in the frame time rather than linear, because the button is held
/// rather than clicked and the result must be the same journey whether the phone
/// is rendering at 20 fps or 60. `wish` is `+1` for closer and `-1` for further
/// ([`crate::systems::map_osd::OsdInput::zoom`]), so a returned factor below 1
/// closes the distance.
fn osd_zoom_factor(wish: f32, delta_time: f32) -> f32 {
    /// e-folds per second of hold: ~1.6× per second, which crosses a zoom level
    /// in a little under a second. Brisker than the wheel's per-notch step
    /// because a finger holds this rather than ratcheting it.
    const RATE: f32 = 0.5;
    if wish == 0.0 {
        return 1.0;
    }
    (-wish * RATE * delta_time).exp()
}

// ── Earth (orbit) mode tuning ───────────────────────────────────────────────
/// Closest the orbit camera may sit to its ground focus (metres).
const ORBIT_MIN_DISTANCE: f64 = 15.0;
/// Third-person follow-camera zoom range (metres from the ~1.8 m avatar). Min lets
/// you get close; max keeps the avatar readable rather than a distant speck.
///
/// The minimum is also the **threshold into first person**: asking to come closer
/// than this is what [`CameraState::first_person`] is set by, and `distance`
/// itself stays here while that lasts, so a zoom back out resumes the closest
/// third-person view rather than one the gesture happened to end on.
const AVATAR_CAM_MIN: f32 = 1.5;
const AVATAR_CAM_MAX: f32 = 25.0;

/// Where the follow camera's eye sits above the avatar's feet, and where it aims:
/// roughly the chest of a 1.8 m model. One constant for both, because they are the
/// same point — the boom pivots about it and the look target is it.
const FOLLOW_EYE_HEIGHT: f32 = 1.5;

/// …and where it sits in first person: the eyes of the same model, which is a
/// little higher than its chest. Deliberately not shared with
/// [`FOLLOW_EYE_HEIGHT`] even though the two are close — the third-person number
/// is the point a boom *pivots about* and can be tuned for framing, and this one
/// is a measurement of where a 1.8 m person's eyes are.
const FIRST_PERSON_EYE_HEIGHT: f32 = 1.62;

/// Whether the follow camera should be inside the avatar's head after a zoom
/// gesture that asked for `wish` metres while sitting at `distance`.
///
/// The whole of the threshold, in one place and with no hysteresis constant of
/// its own — which is the point. `distance` is clamped to
/// [`AVATAR_CAM_MIN`]..[`AVATAR_CAM_MAX`] and stays parked at the minimum for as
/// long as first person lasts, so "the gesture asked to come closer than the
/// closest third-person framing" and "the gesture asked to back out at all" are
/// both comparisons against that one number. A second threshold to leave by would
/// be a second number to keep in step with this one, and the pair would disagree
/// the first time either moved.
#[must_use]
fn first_person_after_zoom(current: bool, distance: f32, wish: f32) -> bool {
    if current {
        // Parked at the minimum, so any outward wish is strictly greater.
        wish <= distance
    } else {
        wish < AVATAR_CAM_MIN
    }
}

/// How far first person may look up and down, in radians from level.
///
/// Wider than the third-person tilt clamp, which stops well short of the vertical
/// because a boom swung overhead puts the camera under the pavement. Standing in
/// your own head there is no boom, so the only reason to stop short of straight up
/// is that the look direction is still decomposed into a bearing for the avatar's
/// facing, and exactly vertical has none.
const FIRST_PERSON_PITCH_MAX: f32 = 1.48; // ~85°

// ── Boom collision ───────────────────────────────────────────────────────────
/// Camera height (m) above which the boom stops being tested against building
/// footprints. The footprints are 2D — they carry no height at all — so above a
/// plausible eaves line a 2D hit means "there is a building over there", not "the
/// view is blocked", and acting on it would reel the camera in every time the
/// avatar walked past a block.
const CAMERA_COLLIDE_MAX_Y: f32 = 15.0;
/// Fraction of the boom kept clear of the wall it hit, so the near plane doesn't
/// end up inside the facade.
const CAMERA_COLLIDE_MARGIN: f32 = 0.12;
/// Shortest the boom may be pulled to, as a fraction. Zero would put the camera
/// inside the avatar's head; this leaves a close-in over-the-shoulder view.
const CAMERA_COLLIDE_MIN: f32 = 0.18;
/// The boom comes in fast (the wall is already in shot) and goes back out slowly
/// (or every doorway makes the camera lunge).
const CAMERA_COLLIDE_IN_RATE: f32 = 22.0;
const CAMERA_COLLIDE_OUT_RATE: f32 = 3.5;

// ── Landing dip ──────────────────────────────────────────────────────────────
/// How far (m) the camera drops on the hardest possible landing, and how fast it
/// recovers. Small: this is a suggestion of weight, and anything a player can
/// consciously see here reads as the camera being broken rather than the landing
/// being heavy.
const CAMERA_DIP_MAX: f32 = 0.35;
const CAMERA_DIP_RECOVERY: f32 = 7.0;

// ── Speed-coupled field of view ──────────────────────────────────────────────
/// Bevy's default perspective FOV (radians) — the resting value for the follow
/// camera, which widens from here with speed.
const FOLLOW_FOV_BASE: f32 = std::f32::consts::FRAC_PI_4;
/// How much wider the view gets at a full sprint (radians, ~7°). The oldest trick
/// there is for making speed felt rather than merely measured; kept small because
/// the map is a map, and a wide-angle city bends.
const FOLLOW_FOV_SPRINT_GAIN: f32 = 0.12;
/// The FOV lags the speed deliberately — arriving with the acceleration would make
/// it read as a lens fault rather than as effort.
const FOLLOW_FOV_RATE: f32 = 4.0;

/// How fast the follow rig eases onto its target (1/s).
///
/// Extracted from the literal it used to be because [`FOLLOW_LEAD_MAX`] has to
/// divide by exactly this number: a first-order lerp at rate `k` trails a target
/// moving at `v` by `v / k` metres for as long as the motion lasts, and leading
/// the target by that much is what cancels it.
const FOLLOW_RATE: f32 = 12.0;
/// Cap on that lead (metres).
///
/// Without a lead the rig trails the avatar by `speed / FOLLOW_RATE`: a hand's
/// width at a walk, ~3 m at the flight cap, and — because flight rides altitude
/// (`motion::fly_altitude_factor`) — over 50 m at the top of the ramp. The boom
/// itself only reaches [`AVATAR_CAM_MAX`], so past a few hundred metres per
/// second the avatar was not merely trailing, it was off the back of the frame,
/// and every change of pace swept it across the screen. Feeding the velocity
/// forward holds it exactly where the boom puts it at any speed.
///
/// The cap is for the *discontinuities*: a teleport, a reconciliation snap or the
/// frame a session ends can leave a velocity that never happened, and an
/// uncapped lead would fling the rig a kilometre. Well past the lead any real
/// flight asks for.
const FOLLOW_LEAD_MAX: f32 = 80.0;
/// Default follow distance — a comfortable third-person framing of the avatar.
pub const AVATAR_CAM_DEFAULT: f32 = 6.0;
/// How fast follow-and-rotate swings the boom round to the avatar's own facing,
/// per second (exponential).
///
/// Gentle on purpose: the avatar's facing is itself already the output of a turn
/// that eases (`avatar::advance_facing`), and the thing being tracked is a model
/// on screen, so a fast rig here would whip the whole view round on every change
/// of direction.
const FOLLOW_ROTATE_RATE: f32 = 8.0;
/// …and how fast it swings round to a **compass** bearing.
///
/// Twice as fast, because the target is a different kind of thing. A compass
/// bearing is not a smoothed consequence of the keys held — it is the user's own
/// hand, already gyro-fused by the platform, and the picture is expected to
/// follow it the way the view out of a window does. At 8/s the map trailed a
/// phone turned briskly by an eighth of a turn and took most of a second to
/// settle, which reads as the compass being slow rather than as the view being
/// smooth. Still an ease and not an assignment: sensor noise about a held bearing
/// would otherwise be a shimmer across the whole map.
const COMPASS_ROTATE_RATE: f32 = 16.0;
/// Wheel-notch zoom rate (fraction of the target distance per notch);
/// exponential so the step scales with altitude, like Google Earth. Kept gentle —
/// the glide (`ZOOM_GLIDE_RATE`) supplies the sense of speed, so per-notch this
/// only needs to nudge; a big step here made each tick leap in height.
const ORBIT_ZOOM_SPEED: f32 = 0.1;
/// Radians of yaw/tilt per pixel of rotate-drag.
const ORBIT_ROT_SENS: f32 = 0.006;
/// Metres of focus travel per pixel of pan-drag, per metre of camera distance
/// (so a drag moves the ground under the cursor by roughly a constant amount).
const ORBIT_PAN_FACTOR: f64 = 0.0016;
/// Tilt clamp: just off the horizon up to nearly straight down.
const ORBIT_PITCH_MIN: f32 = 0.12;
const ORBIT_PITCH_MAX: f32 = std::f32::consts::FRAC_PI_2 - 0.02;
/// Surface-orbit distance is raised well past the space-entry altitude so zooming
/// out can actually cross into the space tier (ADR-025).
const ORBIT_MAX_DISTANCE_SURFACE: f64 = 250_000.0;

// ── The opening view ────────────────────────────────────────────────────────
/// Boom length (m) of the view a fresh install opens on, and the tilt it opens
/// at. Together they put the eye ~1.5 km over the map anchor — high enough that
/// the whole city centre is in frame and the streaming has somewhere to go,
/// low enough that buildings are still buildings rather than a texture.
const START_ORBIT_DISTANCE: f64 = 1_800.0;
const START_ORBIT_PITCH: f32 = 0.96; // ~55° above the horizon

/// Seed Earth mode's pose so the first frame is already the opening view.
///
/// **Why the app opens in Earth mode at altitude at all.** It used to open in
/// Follow, at eye level, behind an avatar — which is the right camera for the
/// thing you do *second* and the wrong one for the thing every user does first,
/// which is work out where they are. On a touchscreen it is worse than wrong:
/// Follow's verbs are an orbit drag and a zoom, so the opening screen offered no
/// way to travel and no horizon to travel by. Earth mode is the map camera (drag
/// pans the ground, pinch zooms, twist turns) and the one every phone map app
/// has trained the user's fingers on.
///
/// The focus is the world origin because that *is* the map anchor
/// (`map_stream`'s bootstrap: Groningen's Grote Markt, or the last location a
/// returning user was at — `user_store::restore_start_location` teleports there
/// through the normal path and rewrites this pose on the way).
fn seed_start_view(camera_state: &mut CameraState) {
    camera_state.mode = CameraMode::Orbit;
    camera_state.orbit_focus = DVec3::ZERO;
    camera_state.orbit_distance = START_ORBIT_DISTANCE;
    camera_state.orbit_distance_target = START_ORBIT_DISTANCE;
    camera_state.orbit_yaw = 0.0;
    camera_state.orbit_pitch = START_ORBIT_PITCH;
    camera_state.orbit_space = false;
    // Both flags, and they say different things: `initialized` stops
    // `camera_controls` re-deriving the pose from the spawn transform (which
    // would throw this away on frame one), `has_pose` is what `enter_mode`
    // consults so returning to Earth mode later restores rather than re-seeds.
    camera_state.orbit_initialized = true;
    camera_state.orbit_has_pose = true;
}

// ── Space tier (ADR-025) ────────────────────────────────────────────────────
/// Climb past this altitude (m) and the orbit pivots the Earth's centre. Kept
/// just above the globe-show altitude so the globe appears essentially as you
/// switch to the centred, look-at-Earth space orbit.
const SPACE_ENTER_ALT: f64 = 55_000.0;
/// Drop back below this altitude and it returns to a ground-focus orbit
/// (hysteresis band with `SPACE_ENTER_ALT`).
const SPACE_EXIT_ALT: f64 = 48_000.0;
/// Farthest the space camera may pull back from the Earth centre (m).
const ORBIT_MAX_DISTANCE_SPACE: f64 = 45_000_000.0;
/// If the globe descent lands farther than this (horizontal metres) from the
/// current anchor, re-anchor the world to the sub-point instead of falling back
/// to the flat home plane at the wrong coordinates (ADR-025, anchor-less model).
const REANCHOR_DISTANCE: f64 = 120_000.0;
/// Exponential rate of the smooth zoom glide (per second): the wheel moves a
/// target distance, and the camera flies toward it at this rate.
const ZOOM_GLIDE_RATE: f64 = 6.0;
/// Exponential rate the compass "north up" reset eases the orbit view back to
/// north (per second) — brisk enough to feel instant, slow enough to read as a
/// turn rather than a snap.
const NORTH_RESET_RATE: f32 = 9.0;
/// Default camera near/far (restored outside the space view).
const DEFAULT_NEAR: f32 = 0.1;
const DEFAULT_FAR: f32 = 200_000.0;

/// How far past the geometric horizon the far plane is pushed in the band where
/// the globe is drawn under the flat tier.
///
/// Not a small slack: the Earth's *silhouette* is drawn by the far edges of the
/// tiles that straddle the horizon, and at 12 km up those are z9 tiles ~70 km
/// across. At 1.2 the far plane landed a few kilometres short of them and the
/// skyline came out as a sawtooth — the tile grid, clipped one tile at a time.
/// 1.5 clears the largest tile the split rule can leave at the limb at any
/// altitude in the band.
const HORIZON_FAR_MARGIN: f32 = 1.5;

/// Near plane as a fraction of altitude in that same band — see the call site.
const NEAR_PLANE_ALTITUDE_FRACTION: f32 = 0.004;

/// Distance to the true horizon from `altitude_m` above a sphere of `radius_m`:
/// the tangent length, `√(2Rh + h²)`.
///
/// The number the far plane has to clear for the planet to reach the skyline —
/// 160 km at 2 km up, 391 km at 12 km, 800 km at 50 km. Written out rather than
/// approximated as `√(2Rh)` because the `h²` term is 15 % of it by the top of
/// the band, and a far plane that falls short cuts the horizon off mid-air.
#[must_use]
pub fn horizon_distance_m(altitude_m: f32, radius_m: f32) -> f32 {
    let h = altitude_m.max(0.0) as f64;
    let r = radius_m.max(1.0) as f64;
    ((2.0 * r * h + h * h).max(0.0)).sqrt() as f32
}

/// The far plane for the band between `GLOBE_DRAW_ALT` and `GLOBE_SHOW_ALT`.
///
/// A function rather than three lines at the call site because anything that
/// stands *between* the ground and the far plane — the sky-dome shell, the star
/// and moon shells — has to be placed against this exact number, and the way
/// those get it wrong is by re-deriving it slightly differently.
#[must_use]
pub fn band_far_plane_m(altitude_m: f32, radius_m: f32) -> f32 {
    (horizon_distance_m(altitude_m, radius_m) * HORIZON_FAR_MARGIN).clamp(DEFAULT_FAR, 4_000_000.0)
}

/// How close to a pole the space orbit's sub-camera point may get (radians of
/// latitude). Dragging past the pole would swing the view through the point
/// where "north up" has no meaning and pop the horizon 180°; Google Earth stops
/// you just short of it, and so does this.
const GLOBE_LAT_LIMIT: f64 = 89.5 * std::f64::consts::PI / 180.0;

/// Screen-up for the space orbit: the planet's north axis projected into the
/// view plane, turned about the view axis by `heading` (0 = north up).
///
/// Deriving up every frame — instead of rotating a stored up vector along with
/// each drag — is what keeps the horizon level. Rotations on a sphere do not
/// commute, so an integrated up picks up a little roll from every drag and the
/// globe slowly ends up sideways with no way back but the compass button.
///
/// `fallback` (the previous frame's up) covers the one degenerate case: looking
/// straight down the polar axis, where north projects to nothing.
pub fn globe_up(dir: DVec3, north: DVec3, heading: f32, fallback: DVec3) -> DVec3 {
    let projected = north - dir * north.dot(dir);
    let base = if projected.length_squared() > 1e-12 {
        projected.normalize()
    } else {
        let f = fallback - dir * fallback.dot(dir);
        if f.length_squared() < 1e-12 {
            return fallback.normalize_or_zero();
        }
        f.normalize()
    };
    (DQuat::from_axis_angle(dir, heading as f64) * base).normalize()
}

/// The heading `up` represents for this view — the inverse of [`globe_up`], used
/// to carry the current orientation into the space tier without the view jumping.
pub fn globe_heading(dir: DVec3, north: DVec3, up: DVec3) -> f32 {
    let north_up = globe_up(dir, north, 0.0, up);
    let right = (-dir).cross(north_up);
    // Signed angle from north-up to the actual up, about the view axis. Negated
    // because `dir` points at the viewer, so a positive rotation about it carries
    // up toward screen-*left*.
    -(up.dot(right)).atan2(up.dot(north_up)) as f32
}

/// One drag step of the space orbit: horizontal spins the globe about the
/// screen-up axis, vertical tips it about screen-right — so the ground tracks
/// the cursor in both axes — and the result is held short of the poles by
/// [`GLOBE_LAT_LIMIT`]. Returns the new camera direction.
pub fn globe_spin(dir: DVec3, north: DVec3, up: DVec3, drag: Vec2, sens: f32) -> DVec3 {
    let right = (-dir).cross(up).normalize_or_zero();
    let lat = dir.dot(north).clamp(-1.0, 1.0).asin();

    // Bound the vertical rotation *before* applying it. Rotating θ about
    // screen-right moves the sub-camera point −θ in latitude, so this keeps it
    // inside ±[`GLOBE_LAT_LIMIT`]. Clamping only the result would not do: one
    // fast flick can carry the point clean over a pole, and it would land on the
    // far side within the limit — the globe appearing to jump hemispheres.
    let tip = (-(drag.y * sens) as f64).clamp(lat - GLOBE_LAT_LIMIT, lat + GLOBE_LAT_LIMIT);
    let rot =
        DQuat::from_axis_angle(up, -(drag.x * sens) as f64) * DQuat::from_axis_angle(right, tip);
    let spun = (rot * dir).normalize();

    // With a turned view (heading ≠ 0) screen-right is not perpendicular to the
    // polar axis, so the bound above is approximate; walk back any residue about
    // the same axis, whichever way actually reduces the latitude.
    let excess = spun.dot(north).clamp(-1.0, 1.0).asin().abs() - GLOBE_LAT_LIMIT;
    if excess <= 0.0 {
        return spun;
    }
    let a = (DQuat::from_axis_angle(right, excess) * spun).normalize();
    let b = (DQuat::from_axis_angle(right, -excess) * spun).normalize();
    if a.dot(north).abs() < b.dot(north).abs() {
        a
    } else {
        b
    }
}

/// The compass-locked variant of [`globe_spin`]: horizontal drag is a *pure
/// longitude* spin about the planet's true polar axis (so the poles stay pinned,
/// and you circle them in longitude but never travel over them), and vertical
/// drag walks the meridian in latitude, clamped short of the pole exactly as
/// [`globe_spin`] is. Unlike [`globe_spin`] — which rotates about the *screen*
/// axes for direct grab-the-globe manipulation and so lets a drag drift your
/// latitude and carry the view around a pole — this keeps north genuinely fixed,
/// which is what the compass lock is asking for. Returns the new camera direction.
pub fn globe_spin_polar(dir: DVec3, north: DVec3, drag: Vec2, sens: f32) -> DVec3 {
    let north = north.normalize_or_zero();
    // Horizontal: spin about the polar axis → longitude only, latitude untouched.
    let spun = (DQuat::from_axis_angle(north, -(drag.x * sens) as f64) * dir).normalize();
    // Vertical: tip along the meridian about the east tangent. Rotating `spun`
    // about `east` by θ moves its latitude by −θ, so clamp θ into
    // [lat − LIMIT, lat + LIMIT] to keep the result inside ±[`GLOBE_LAT_LIMIT`]
    // (same bound-before-apply as `globe_spin`, and exact here since heading is 0).
    let east = north.cross(spun);
    if east.length_squared() < 1e-12 {
        return spun; // looking straight down the axis; no meridian to walk
    }
    let east = east.normalize();
    let lat = spun.dot(north).clamp(-1.0, 1.0).asin();
    let tip = (-(drag.y * sens) as f64).clamp(lat - GLOBE_LAT_LIMIT, lat + GLOBE_LAT_LIMIT);
    (DQuat::from_axis_angle(east, tip) * spun).normalize()
}

/// Absolute world position of a big_space grid cell's origin.
pub(crate) fn cell_origin(cell: &GridCell) -> DVec3 {
    DVec3::new(cell.x as f64, cell.y as f64, cell.z as f64) * WORLD_CELL_EDGE as f64
}

/// Focus→camera offset for spherical orbit params (x = east, z = south, y = up).
fn spherical_offset(yaw: f32, pitch: f32, distance: f64) -> DVec3 {
    let (cp, sp) = (pitch.cos() as f64, pitch.sin() as f64);
    let (sy, cy) = (yaw.sin() as f64, yaw.cos() as f64);
    DVec3::new(sy * cp, sp, cy * cp) * distance
}

/// Set the camera near/far only when they actually change (avoids per-frame
/// change-detection churn on the projection).
fn set_projection(projection: &mut Mut<Projection>, near: f32, far: f32) {
    // Read through `Deref`, write through `DerefMut`, and never the other way
    // round. The guard below is old; what it did not do is stop the *tick* being
    // stamped, because the caller holds a `Mut<Projection>` and `&mut **projection`
    // marks the component changed the moment it is formed — before this `if` runs.
    // So the projection was `Changed` on every frame of the run, re-triggering
    // frustum recomputation and the camera's view-uniform extraction, and quietly
    // negating the stationary-frame optimisation the caller works hard for a dozen
    // lines above.
    let stale = matches!(
        &**projection,
        Projection::Perspective(p) if p.near != near || p.far != far
    );
    if !stale {
        return;
    }
    if let Projection::Perspective(p) = &mut **projection {
        p.near = near;
        p.far = far;
    }
}

/// Set the camera's vertical field of view, only when it actually changes — same
/// reason [`set_projection`] guards its write: the projection is change-detected,
/// and the follow camera would otherwise mark it dirty on every frame it moves.
fn set_fov(projection: &mut Mut<Projection>, fov: f32) {
    // Same shape, same reason as [`set_projection`] — and this one mattered more,
    // because the Follow arm calls it every frame with a speed-eased FOV.
    let stale = matches!(
        &**projection,
        Projection::Perspective(p) if (p.fov - fov).abs() > 1e-5
    );
    if !stale {
        return;
    }
    if let Projection::Perspective(p) = &mut **projection {
        p.fov = fov;
    }
}

/// Radius of the ground disc that carries the world out to the horizon past the
/// last streamed tile, in metres.
///
/// Named because it is the farthest thing in the flat tier, which makes it the
/// bound anything standing *behind* the world has to clear —
/// `sky_dome::SKY_DOME_RADIUS` is chosen against it, and there is a test.
pub const GROUND_DISC_RADIUS: f32 = 150_000.0;

/// How far the ground skirt sits below the anchor's ground (m) — see the spawn
/// site in [`setup_camera`] for why it is not the 5 m it used to be.
const GROUND_SKIRT_SINK: f32 = 25.0;

/// Radius the skirt's curve is built against.
///
/// A constant rather than `GlobeState::radius`, because the skirt is spawned
/// with the camera and the globe does not exist until an anchor does. The
/// geocentric radius runs 6,357–6,378 km, so the worst latitude disagreement is
/// 0.3 % of the 1.8 km drop at the rim — seven metres, which
/// [`GROUND_SKIRT_SINK`] covers. Nobody can see seven metres in a featureless
/// paper backstop 150 km away; what they can see is a straight rim where the
/// horizon should curve, which is what this fixes.
const SKIRT_EARTH_RADIUS: f32 = crate::systems::globe::GLOBE_EQUATORIAL_RADIUS_M;

/// The skirt as a spherical cap: a fan of rings that follow the Earth away from
/// the anchor instead of a flat disc lying on its tangent plane.
///
/// Rings are spaced by the **square root** of the radius fraction, so they crowd
/// toward the rim where the drop is changing fastest — an even spacing puts
/// nearly all the sag in the outermost ring, which is also the only part of the
/// skirt a low camera ever sees against the sky.
fn ground_skirt_mesh() -> Mesh {
    use bevy::asset::RenderAssetUsages;
    use bevy::render::mesh::{Indices, PrimitiveTopology};

    /// Rings out from the centre. 32 holds the worst inter-ring sag under a
    /// metre, which is well inside [`GROUND_SKIRT_SINK`].
    const RINGS: usize = 32;
    /// Segments around. The rim is the silhouette against the sky at low
    /// altitude, so this is chosen to keep it from reading as a polygon.
    const SEGMENTS: usize = 96;

    let mut positions = Vec::with_capacity(RINGS * SEGMENTS + 1);
    let mut normals = Vec::with_capacity(RINGS * SEGMENTS + 1);
    let mut uvs = Vec::with_capacity(RINGS * SEGMENTS + 1);
    positions.push([0.0, 0.0, 0.0]);
    normals.push([0.0, 1.0, 0.0]);
    uvs.push([0.5, 0.5]);
    for ring in 1..=RINGS {
        let t = (ring as f32 / RINGS as f32).sqrt();
        let r = GROUND_DISC_RADIUS * t;
        let drop = crate::systems::globe::curvature_drop_m(r, SKIRT_EARTH_RADIUS);
        for seg in 0..SEGMENTS {
            let a = seg as f32 / SEGMENTS as f32 * std::f32::consts::TAU;
            let (x, z) = (r * a.cos(), r * a.sin());
            positions.push([x, -drop, z]);
            // The sphere's own outward normal, so the skirt takes the light the
            // curved tiles over it take rather than staying lit as a flat plane.
            let n = Vec3::new(x, SKIRT_EARTH_RADIUS - drop, z).normalize_or(Vec3::Y);
            normals.push(n.to_array());
            uvs.push([0.5 + 0.5 * t * a.cos(), 0.5 + 0.5 * t * a.sin()]);
        }
    }
    let mut indices: Vec<u32> = Vec::new();
    // Centre fan.
    for seg in 0..SEGMENTS {
        let a = 1 + seg as u32;
        let b = 1 + ((seg + 1) % SEGMENTS) as u32;
        indices.extend_from_slice(&[0, b, a]);
    }
    for ring in 1..RINGS {
        let inner = 1 + ((ring - 1) * SEGMENTS) as u32;
        let outer = 1 + (ring * SEGMENTS) as u32;
        for seg in 0..SEGMENTS as u32 {
            let next = (seg + 1) % SEGMENTS as u32;
            let (a, b) = (inner + seg, inner + next);
            let (c, d) = (outer + seg, outer + next);
            // Wound to face up; a downward-wound ground is backface-culled and
            // reads as a hole, silently (the same trap `displaced_grid` names).
            indices.extend_from_slice(&[a, d, c, a, b, d]);
        }
    }
    Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::RENDER_WORLD,
    )
    .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions)
    .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, normals)
    .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, uvs)
    .with_inserted_indices(Indices::U32(indices))
}

/// Camera exposure (EV100) paired with the physical `RAW_SUNLIGHT` sun. Bevy's
/// atmosphere demo uses 13.0; higher = darker overall. This is the main knob for
/// balancing scene brightness against the (now physical) sky.
const ATMOSPHERE_EXPOSURE_EV100: f32 = 13.0;

/// Bloom strength on the ground. Low (Bevy's 0.15 default is tuned for
/// emissive-heavy scenes) so the daytime basemap doesn't smear.
pub const GROUND_BLOOM_INTENSITY: f32 = 0.12;
/// …and in space, where the frame is mostly black and the only bright things are
/// the sun disk, the star field and a thin sunlit limb. `apply_day_night_cycle`
/// switches between the two at the tier boundary.
pub const SPACE_BLOOM_INTENSITY: f32 = 0.35;

/// Bloom strength at a given `space_factor` — 0 at the ground, 1 in deep space —
/// with the render budget applied.
///
/// The phone tier answers **exactly** `0.0`, and the exactness is the mechanism
/// rather than an approximation of "off": `bevy_post_process`'s bloom node
/// early-returns on `intensity == 0.0`, so a zero here skips all thirteen
/// downsample/upsample passes over the HDR target without removing the
/// component. That matters — `apply_day_night_cycle` and `shot_harness` both
/// query `&Bloom` as part of a tuple, so a camera without the component would
/// silently drop out of the day/night cycle and out of `ShotMetrics` rather than
/// merely rendering without glare.
#[must_use]
pub fn bloom_intensity(space_factor: f32) -> f32 {
    if !crate::quality::budget().bloom {
        return 0.0;
    }
    GROUND_BLOOM_INTENSITY.lerp(SPACE_BLOOM_INTENSITY, space_factor)
}

/// Earth inner radius used by `Atmosphere::earth` (metres). The planet-centre
/// entity sits this far below the local ground so the atmosphere's surface aligns
/// with y = 0. Must match bevy_pbr's `Atmosphere::earth` constant.
const EARTH_ATMOSPHERE_INNER_RADIUS: f32 = 6_360_000.0;

/// Adapters whose driver was **measured** to miscompile the atmosphere's
/// shaders, so Automatic sky picks the dome there. One entry per measurement —
/// never a family or a version range guessed from one. The evidence standard
/// for adding a line: an on-device probe result pinning the defect, recorded
/// with a date in `docs/notes/adreno-atmosphere-miscompile.md`. The evidence
/// for removing one: the atmosphere verified working on that adapter via the
/// `SkyPreference::Full` override, which deliberately bypasses this list.
fn driver_denies_atmosphere(adapter_name: &str) -> bool {
    // OnePlus 15, driver 0842.x, compiler E031.50.19.27, measured 2026-08-29:
    // black sky, sun disc at full strength. Since 2026-09-28 the suspect is
    // the driver's copy of the `Atmosphere` uniform passed by value, which the
    // patched shader no longer makes (`atmosphere_patch`, round 2). This line
    // goes once Sky ▸ Full shows a blue sky on the device.
    adapter_name.contains("Adreno (TM) 840")
}

/// Spawn the world [`BigSpace`] root with the camera as its [`FloatingOrigin`]
/// (ADR-019). Other content currently spawns at the origin (cell 0) and renders
/// correctly; geo-derived `GridCell` placement arrives with planet-scale streaming.
pub fn setup_camera(
    mut commands: Commands,
    mut camera_state: ResMut<CameraState>,
    mut scattering_media: ResMut<Assets<ScatteringMedium>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    dev: Res<crate::resources::DevPanelState>,
    // The sky and the env map are chosen from here now, not from the developer
    // panel — see `SkyPreference` and Settings ▸ Graphics. Both are startup-only
    // for the reason this function's sky block gives at length, so reading the
    // preference once, here, is the whole of applying it.
    prefs: Res<crate::resources::AppPrefs>,
    adapter: Res<bevy::render::renderer::RenderAdapterInfo>,
    render_device: Res<bevy::render::renderer::RenderDevice>,
    // The *requested* present mode, which is the one that decides whether anything
    // else may pace the loop — see `quality::Capabilities::vsync_paced`. Read off
    // the window rather than off `cfg!(target_os = …)` so a platform that changes
    // its mind (or a `--shot` run, which has no window at all) cannot leave a
    // second pacer running behind it.
    primary_window: Query<&Window, With<bevy::window::PrimaryWindow>>,
) {
    // Follow's own parameters are still seeded — they are what Tab lands on —
    // but the *opening* camera is Earth mode at altitude (see `seed_start_view`).
    camera_state.distance = AVATAR_CAM_DEFAULT;
    camera_state.azimuth = 0.0;
    camera_state.pitch = std::f32::consts::PI / 6.0;
    seed_start_view(&mut camera_state);

    // A software adapter (Mesa lavapipe in a container or CI) has the *same*
    // 1-or-4 sample-count restriction as WebGPU, and fails the same silent way —
    // see the `Msaa` comment below. Detect it rather than keying off the headless
    // run, so `cargo shots` on a real GPU keeps its 2×.
    let software_renderer = adapter.device_type == egui_wgpu::wgpu::DeviceType::Cpu;
    // The web and a software adapter both advertise sample counts of 1 and 4 only
    // — see the `Msaa` comment at the camera spawn.
    let restricted_sample_counts = cfg!(target_arch = "wasm32") || software_renderer;
    // Kept for `render_features::apply_render_features`, which has to answer the
    // same question every time the occlusion setting is flipped and cannot
    // re-derive it — the restriction is a fact about this surface as well as this
    // adapter.
    commands.insert_resource(crate::systems::render_features::RestrictedSampleCounts(
        restricted_sample_counts,
    ));
    let budget = crate::quality::budget();

    // Earth-preset scattering medium for the built-in atmosphere (Bevy 0.19). Its
    // density is rewritten by altitude in `globe::update_globe_visibility` (thin
    // low down so the map reads, rising for the limb from orbit — see
    // `globe::ATMOSPHERE_DENSITY_NEAR`), so it's handed to that system as a shared
    // handle. The planet spawns with it below.
    let medium = scattering_media.add(ScatteringMedium::default());
    commands.insert_resource(crate::systems::globe::AtmosphereMedia {
        space: medium.clone(),
    });

    // Atmosphere and its env-map IBL are chosen once here, at startup: they can't
    // be safely toggled at runtime (removing the components frees in-flight GPU
    // resources and hangs the device — see `globe::update_globe_visibility`). The
    // dev-panel checkboxes seed these but only take effect on the next launch; the
    // env flags force them off. Env-map IBL requires the atmosphere it bakes from.
    // The atmosphere is compute-based (bevy_pbr) and needs a fragment storage
    // buffer. That ruled it out under WebGL2, which has neither — but the web build
    // targets WebGPU now (ADR-033), where both exist, so this is no longer
    // platform-gated at all.
    //
    // The atmosphere itself stays on in every tier — it *is* the sky, and the
    // phone budget has nothing to gain by replacing it with a void (it also
    // A/B'd within noise of baseline, see `docs/notes/wasm-performance.md`). The
    // env map is the half that does not survive: it re-bakes a cubemap and its
    // mip chain every frame, and `dev.envmap_enabled` is already seeded from the
    // budget, so this needs no tier test of its own.
    // …and one thing the run must be *asked*, not assumed. Bevy 0.19's atmosphere
    // takes its coefficients from a `ScatteringMedium` asset, which bakes them into
    // two `Rgba32Float` lookup textures read through a `FilterMode::Linear` sampler
    // (`bevy_pbr::medium`). wgpu permits filtering a 32-bit float texture only when
    // the adapter offers `FLOAT32_FILTERABLE`. Desktop GPUs and lavapipe do; a great
    // many mobile ones do not, and the first phone this app ever ran on (Adreno 619
    // in a Fairphone 4, 2026-08-12) is one of them.
    //
    // The consequence is not a missing sky, it is **no app**: the bind group fails
    // validation, and Bevy's default error handler answers a validation error
    // by quitting. So the whole client died three seconds in, having rendered
    // nothing, with no Rust panic — the process aborted in libc on the way out
    // (`FORTIFY: pthread_mutex_lock called on a destroyed mutex`) which is
    // indistinguishable from an OOM kill unless you have the log.
    //
    // Gated on the **adapter**, not on `cfg!(target_os = "android")`, for the reason
    // the MSAA comment below gives at greater length: this is a property of the
    // device this run found, not of a class of machine. A phone with a Mali that
    // supports the feature keeps its sky, a desktop GPU that somehow lacks one
    // still starts, and nobody has to discover the rule again on the next platform.
    let float32_filterable = render_device
        .features()
        .contains(bevy::render::settings::WgpuFeatures::FLOAT32_FILTERABLE);
    if !float32_filterable {
        tracing::warn!(
            adapter = %adapter.name,
            "no FLOAT32_FILTERABLE on this adapter — bevy's atmosphere would fail \
             bind-group validation and take the whole app down with it; falling back \
             to the analytic sky dome"
        );
    }
    // Every capability question this run can ask, asked once and recorded, because
    // several places downstream have to get the *same* answer.
    crate::quality::init_capabilities(crate::quality::Capabilities {
        float32_filterable,
        software_renderer,
        // WebGL2 has no compute stage, so bevy's ambient-occlusion pass cannot
        // exist there whatever anyone asks for. Read off the adapter's downlevel
        // flags rather than off `cfg!(target_arch = "wasm32")`, for the reason the
        // sample-count comment gives at length: the web build targets WebGPU now,
        // and a platform test would refuse a pass the browser can actually run.
        compute_shaders: render_device.limits().max_compute_workgroup_size_x > 0,
        vsync_paced: matches!(
            primary_window.iter().next().map(|w| w.present_mode),
            Some(
                bevy::window::PresentMode::AutoVsync
                    | bevy::window::PresentMode::Fifo
                    | bevy::window::PresentMode::FifoRelaxed
            )
        ),
    });

    // Which sky this run gets, decided once and recorded, because three places
    // downstream have to make the *same* choice (`systems::sky_dome::SkyMode`).
    // `CARTO_NO_ATMOSPHERE` and the dev-panel toggle still mean no sky at all —
    // they are bisecting switches, and one that quietly substituted another sky
    // would bisect nothing. `CARTO_SKY_DOME` is the other direction: it forces the
    // fallback on hardware that could run the real thing, which is the only way to
    // look at what the phone sees without holding the phone.
    //
    // **The tier gets no say here any more, and that is the fix.** There used to be
    // a `Budget::sky_dome` beside the capability test — the tier's separate answer
    // to "should this machine *spend* a frame on the sky" — and the two were argued
    // for at length as different questions. They are, but only one of them could be
    // answered honestly: the budget is resolved before a renderer exists, so on
    // Android `sky_dome` meant "this is a Fairphone 4" for every handset ever made.
    // The patch for that was `gpu_affords_atmosphere`, a table of GPU families
    // (Adreno >= 700, Mali-G >= 700, Xclipse >= 900) — which is the same guess one
    // level down, wrong by default for every part not on the list. Both are gone.
    // The adapter is asked whether it can filter a float32 texture, and if it can,
    // it gets the sky it can draw. A device that should not spend the frame is put
    // on a lower rung deliberately (Settings ▸ Map, `CARTO_QUALITY`), which is a
    // person deciding rather than a table guessing.
    //
    // **The user's own answer comes first, and it can only ever narrow.**
    // `SkyPreference::Simple` picks the dome on hardware that could run the
    // atmosphere — the setting a device with a hostile driver most needs, and
    // until now reachable only through `CARTO_SKY_DOME`, i.e. through a shell
    // that Android does not have. `Full` cannot conjure an atmosphere the adapter
    // will not draw: it still has to pass `float32_filterable`, because the
    // alternative to refusing is the bind-group validation failure described
    // above, which takes the whole app down.
    // **…and one driver is denied by measurement, not by family.** The Adreno
    // 840's driver (the only 840 driver that exists, 0842.x, compiler E031.50)
    // draws the atmosphere as a black sky, measured over five instrumented
    // device rounds (2026-08-27..29, `diag/adreno-sky-probe`). Those rounds
    // were read at the time as a per-inline-site miscompile; re-read on
    // 2026-09-28 they say the shader saw the camera above the air, which a
    // by-value copy of the `Atmosphere` uniform would do on a driver that
    // lays the copy out wrong — and `atmosphere_patch` no longer makes one
    // (`docs/notes/adreno-atmosphere-miscompile.md`). Until the device shows
    // that working, Automatic picks the dome here. This is the opposite of the
    // deleted `gpu_affords_atmosphere` table: that predicted unmeasured
    // hardware from a family name, this records one measured defect on one
    // measured driver — and `SkyPreference::Full` still overrides it, so the
    // atmosphere is one settings flip (and a restart) away, no rebuild.
    let denied_driver = driver_denies_atmosphere(&adapter.name)
        && prefs.sky != crate::resources::SkyPreference::Full;
    let want_sky = !crate::env_flag("CARTO_NO_ATMOSPHERE");
    let asked_for_dome = prefs.sky == crate::resources::SkyPreference::Simple;
    let want_atmosphere = want_sky
        && float32_filterable
        && !asked_for_dome
        && !denied_driver
        && !crate::env_flag("CARTO_SKY_DOME");
    let want_envmap =
        want_atmosphere && prefs.sky_lighting_on() && !crate::env_flag("CARTO_NO_ENVMAP");
    let sky = if want_atmosphere {
        crate::systems::sky_dome::SkyMode::Atmosphere
    } else if want_sky {
        crate::systems::sky_dome::SkyMode::Dome
    } else {
        crate::systems::sky_dome::SkyMode::None
    };
    // **Unconditional, and it was not before.** The two lines this replaces each
    // fired only when the answer was *changed* from the tier's, on the reasoning
    // that an unchanged answer is not news. It is news when the sky is black: a
    // handset on the balanced rung took neither branch, so on the one platform with
    // no terminal to check, the log said nothing whatsoever about which sky had been
    // chosen — and `SkyMode::None` (the dev toggle, or a `debug.carto.no_atmosphere`
    // left set by an earlier bisect, which survives until the phone reboots) is
    // indistinguishable from a broken atmosphere without it.
    tracing::info!(
        ?sky,
        adapter = %adapter.name,
        float32_filterable,
        denied_driver,
        preference = ?prefs.sky,
        forced_dome = crate::env_flag("CARTO_SKY_DOME"),
        no_atmosphere = crate::env_flag("CARTO_NO_ATMOSPHERE"),
        "sky chosen"
    );
    commands.insert_resource(sky);

    commands.spawn_big_space(Grid::new(WORLD_CELL_EDGE, WORLD_SWITCH_THRESHOLD), |root| {
        // The root must carry Visibility: renderable children inherit it, and a
        // missing parent Visibility triggers Bevy's B0004 warning per child.
        root.insert(Visibility::default());
        let mut cam = root.spawn_spatial((
            Camera3d::default(),
            // The sample count is the tier's (`quality::Budget::msaa`, which
            // carries the whole argument: only 1× and 4× are portable, and an
            // unsupported request fails to create `main_texture_sampled` — the
            // canvas stays black and the only log line is `CommandQueue has
            // un-applied commands`, which describes the symptom and never the
            // cause), clamped by two things the tier cannot know.
            //
            // **The web** permits a sample count of only 1 or 4 whatever the
            // machine underneath is, so a browser in the desktop tier is still
            // `Off`: WebGPU's jump from 2× to 4× doubles the samples rasterised
            // for every triangle in a scene of hundreds of tiles plus a
            // thousand-odd buildings, and the browser build has no frame budget to
            // spend on edge quality yet. Raise it to `Msaa::Sample4` once frame
            // times allow — those are the only two legal values there.
            //
            // **A software adapter** (lavapipe in a container or CI) reports the
            // same [1, 4]. Both clamps stay here rather than moving into the
            // budget because both are properties of *this run's surface and
            // adapter*, not of the class of machine — which is also what lets
            // `cargo shots` on a real GPU keep its 2×.
            //
            // What did *not* used to be here is the tier: the choice was a bare
            // `cfg!(target_arch = "wasm32")`, so Android — which answers "native"
            // to every cfg in this tree — was handed a 2× that a Vulkan
            // implementation is under no obligation to support.
            //
            // There used to be a third clamp here — ambient occlusion, whose SSAO
            // node requires `Msaa::Off` — and with it a reconciliation in
            // `render_features` for when the setting was flipped mid-run. The
            // occlusion pass was removed on 2026-09-21, so the sample count is
            // decided once, here, and nothing moves it afterwards.
            budget.main_pass_msaa(restricted_sample_counts),
            // How a shadow edge is filtered. Stated rather than left to bevy's
            // required-component default for the same reason `tonemapping` below
            // is: the default is the expensive one. `Gaussian` takes nine taps
            // per shadowed fragment per light, and until this line existed every
            // tier took it — including the one a phone resolves to, where it is
            // nine taps over three and a half million pixels with the sun and the
            // moon both up. See `quality::Budget::shadow_filter`.
            //
            // Unlike the sample count above, this is not re-applied by
            // `render_features`: nothing can change it while the app runs, the
            // rung being constant for the process.
            budget.shadow_filter,
            // Explicit, because Bevy's required-component default is
            // `TonyMcMapface` and that samples a 3D LUT. On a tier with no `Hdr`
            // camera — no atmosphere, no bloom, i.e. the phone — bevy_pbr sets
            // `TONEMAP_IN_SHADER`, so the curve is not one full-screen pass but
            // work in every mesh fragment shader, multiplied by the ground
            // overdraw. See `quality::Budget::tonemapping`.
            budget.tonemapping,
            // Bevy's default far plane (1 km) clips the streamed tile pyramid
            // and the horizon ground disc; reversed-z depth keeps precision
            // fine at this range.
            Projection::Perspective(PerspectiveProjection {
                far: 200_000.0,
                ..default()
            }),
            Transform::from_xyz(0.0, 10.0, 20.0).looking_at(Vec3::ZERO, Vec3::Y),
            // Physically-based exposure to balance the RAW_SUNLIGHT sun so the
            // scene isn't blown out while the sky is lit correctly.
            Exposure {
                ev100: ATMOSPHERE_EXPOSURE_EV100,
            },
            AmbientLight::default(),
            FreeCamera,
            FloatingOrigin,
        ));
        // Bloom: the glare that makes an HDR scene read as bright. The sun disk is
        // the case that needs it — a physically-sized disc is a few pixels across,
        // and without glare around it the sky's brightest object looks like a
        // sticker. It also gives the night city's window lights and the sunlit limb
        // from orbit their halo. Kept low (Bevy's default 0.15 is tuned for
        // emissive-heavy scenes) so the daytime basemap doesn't smear.
        //
        // Omitted entirely on a tier that does not want it, rather than kept at
        // `intensity == 0.0`. A zero intensity does skip the thirteen
        // downsample/upsample passes — `bloom_intensity` documents that, and it is
        // true — but `Bloom` carries `#[require(Hdr)]`, so the component's mere
        // presence keeps the main texture at `Rgba16Float` whether or not the node
        // does any work. Skipping the passes while still paying 8 B/px for the
        // target is the worst of both.
        //
        // The reason it used to stay is real and is now handled at the query
        // instead: `apply_day_night_cycle` and `shot_harness` both name `&Bloom`
        // inside a tuple, and a camera missing it would silently drop out of the
        // day/night cycle and out of `ShotMetrics` — a lighting bug, not a
        // rendering one. Both read `Option<&Bloom>` now.
        if budget.bloom {
            cam.insert(Bloom {
                intensity: bloom_intensity(0.0),
                ..Bloom::NATURAL
            });
        }
        // Ambient occlusion, the edge filter that pays for it, and the light
        // shafts are **not** inserted here. All three are capability-and-
        // preference answers that can change while the app runs (Settings ▸
        // Graphics), so one system owns adding and removing them and this spawn
        // would only be a second place that could disagree with it — see
        // `systems::render_features`, which runs from `Startup` as well as on
        // change and so has them on before the first frame.
        // Atmospheric distance fog: distant buildings fade into haze, sun-side
        // glow, driven by the dev panel (apply_distance_fog). It is one more
        // fragment uniform in the mesh view, which overflowed WebGL2's cap of 14
        // — reached there because clustering fell back to uniforms. On WebGPU
        // clustering uses storage buffers, so the budget is no longer contested.
        cam.insert(distance_fog(
            dev.fog_density,
            dev.time_of_day,
            dev.sun_declination_rad,
        ));
        // Bevy 0.19 built-in atmosphere: the camera carries only the render
        // settings; the sky derives from the sun direction + camera altitude
        // over the planet entity below.
        //
        // This used to add that "`update_globe_visibility` no-ops without it,
        // which is what made dropping it a safe web workaround". It is not safe
        // and has not been for a while: that system is now the sole writer of the
        // globe surface's visibility, so a camera with no `AtmosphereSettings`
        // meant a planet that streamed every tile and drew none of them. It takes
        // the component optionally now — see its doc comment. Nothing here may
        // assume a system "no-ops" because of a component this gate withholds.
        //
        // The LUT sizes are the tier's. Nothing else here is: the sample counts,
        // the aerial-view range and the rendering method stay at Bevy's defaults
        // (`update_globe_visibility` owns the last two of those at runtime), so
        // the desktop tier restates exactly what `AtmosphereSettings::default()`
        // gave it. The reason the sizes are worth touching at all is that all four
        // LUTs are recomputed *every frame* — see `quality::AtmosphereLuts`.
        //
        // Gated on `want_atmosphere`, and the gate is about **bandwidth, not the
        // LUTs**. `AtmosphereSettings` carries `#[require(Hdr)]`, so its mere
        // presence makes the main texture `Rgba16Float` — 8 B/px of colour instead
        // of 4. On a tiled mobile GPU that is not a memory cost but a *binning*
        // one: 8 B colour plus depth overruns the on-chip tile budget, and the
        // driver answers by splitting the pass into more bins and replaying the
        // whole draw list for each. An adapter without `FLOAT32_FILTERABLE` (the
        // Adreno 619 in a Fairphone 4 is the first one we found) already renders
        // no sky at all, so before this gate it paid the doubled bandwidth *and*
        // the extra bins for an atmosphere that was switched off above.
        if want_atmosphere {
            let luts = budget.atmosphere_luts;
            cam.insert(AtmosphereSettings {
                transmittance_lut_size: luts.transmittance,
                multiscattering_lut_size: luts.multiscattering,
                sky_view_lut_size: luts.sky_view,
                aerial_view_lut_size: luts.aerial_view,
                ..default()
            });
        }
        if want_envmap {
            // Sky-driven image-based lighting: surfaces are lit by and reflect
            // the actual atmosphere. 256² IBL cubemap instead of the 512²
            // default quarters the per-frame environment-map compute bake;
            // ambient/diffuse IBL is low-frequency, so the drop is imperceptible.
            cam.insert(AtmosphereEnvironmentMapLight {
                size: UVec2::splat(256),
                ..default()
            });
        }

        // A ground skirt out to the horizon, in the OSM basemap's paper tone.
        // Without it, the world ends at the loaded tiles and the void below
        // the horizon reads as a black gulf; with it, DistanceFog fades the
        // ground into the haze so fog and sky meet seamlessly at any altitude.
        //
        // It is a spherical **cap**, not the flat circle it was until the ground
        // tiles started following the Earth's curve. A plane and a sphere that
        // touch at the anchor have separated by 1.8 km at this radius, so a flat
        // skirt under curved tiles is not a backstop at all — it rises through
        // them a few kilometres out and paints blank paper over the map.
        root.spawn_spatial((
            Mesh3d(meshes.add(ground_skirt_mesh())),
            MeshMaterial3d(materials.add(StandardMaterial {
                base_color: Color::srgb(0.83, 0.83, 0.80),
                perceptual_roughness: 1.0,
                reflectance: 0.0,
                ..default()
            })),
            // Well below the whole tile pyramid (whose coarsest rings sit a few
            // metres down — see update_map_stream) so it never covers a tile.
            // Deeper than the 5 m it used to sit at, because the cap is built
            // against a nominal Earth radius while the tiles use the anchor's
            // real one: 0.3 % of latitude spread over 1.8 km of drop is ~5 m of
            // disagreement at the rim, and the clearance has to swallow it.
            Transform::from_xyz(0.0, -GROUND_SKIRT_SINK, 0.0),
            NotShadowCaster,
            // From orbit this 150 km cap reads as a bright patch over the
            // anchor; hide it (and the streamed tiles) while the globe is up.
            crate::components::HideInSpace,
            crate::components::GroundSkirt,
        ));
    });

    // The built-in atmosphere is planetary-scale; its origin and radius come from
    // this entity's transform. Placed one Earth radius straight down so the surface
    // (`Atmosphere::earth` inner_radius) sits at the local ground plane (y = 0), so
    // the camera reads at its true altitude above it. A plain entity (not under the
    // BigSpace): the camera is the floating origin and stays near y = 0, so the sky's
    // altitude stays effectively constant — correct for the flat local world.
    // `update_atmosphere_origin` moves it to Earth's true render-space centre.
    // Spawned only when the atmosphere is enabled (startup choice — see above).
    if want_atmosphere {
        commands.spawn((
            crate::components::PlanetAtmosphere,
            Atmosphere::earth(medium),
            Transform::from_xyz(0.0, -EARTH_ATMOSPHERE_INNER_RADIUS, 0.0),
        ));
    }
}

/// Arm the per-mode entry behaviour when switching *into* `next`. Called from both
/// the Tab cycle and the dock's mode disc (`dock::mode_disc`) so the two paths
/// stay identical:
/// - **Follow** snaps onto the avatar next frame (reposition reliably).
/// - **Fly** restores its last saved pose, or seeds the look from the live view if
///   it has none yet.
/// - **Earth** restores its remembered pose if it has one, else seeds from the view.
///
/// The camera transform itself is untouched here — `camera_controls` does the
/// placement — so this only flips the intent flags in [`CameraState`].
pub fn enter_mode(camera_state: &mut CameraState, next: CameraMode) {
    match next {
        CameraMode::Avatar => {
            camera_state.avatar_initialized = false;
            // Land in plain follow; the GPS/locate button owns the rotate sub-state.
            camera_state.follow_rotate = false;
        }
        CameraMode::Free => {
            camera_state.free_initialized = false;
            // Enter at rest — the restored pose is a placement, not a flight.
            camera_state.free_velocity = Vec3::ZERO;
        }
        CameraMode::Orbit => camera_state.orbit_initialized = camera_state.orbit_has_pose,
    }
}

/// Whether Follow is on offer *as a camera mode*: an avatar to follow, in a
/// signed-in session. Walking a body around is what a simulator is for —
/// offline is the map viewer, and its switcher offers Fly and Earth. One
/// predicate, two readers — the Tab cycle here and the dock's mode disc
/// (`dock::mode_disc`) — so the two never disagree about what is on the cycle.
///
/// This gates the *switcher*, not the camera: driver mode enters the follow
/// camera itself, offline included — navigation is a legitimate offline use, and
/// it stands down on its own — so the guard in [`camera_mode_toggle`] exempts it
/// rather than fighting it every frame. Device tracking (`systems::locate`) needs
/// no exemption: it runs in whichever mode is already on and moves the camera,
/// never the switcher.
///
/// The avatar check stays even though `main::spawn_avatar_entity` spawns one
/// unconditionally at `Startup`: it costs an `is_empty` and it is the only
/// thing standing between a future conditional spawn and a camera following
/// nothing.
#[must_use]
pub fn follow_available(session: &crate::systems::session::SessionState, has_avatar: bool) -> bool {
    session.is_online() && has_avatar
}

/// The mode a switch lands on from `mode`: Follow → Fly → Earth → Follow, with
/// Follow skipped when it is not on offer ([`follow_available`]). Pure, and
/// shared by the Tab cycle and the mode disc so the two never disagree about
/// what "next" means.
#[must_use]
pub fn next_camera_mode(mode: CameraMode, follow: bool) -> CameraMode {
    match mode {
        CameraMode::Avatar => CameraMode::Free,
        CameraMode::Free => CameraMode::Orbit,
        CameraMode::Orbit if follow => CameraMode::Avatar,
        CameraMode::Orbit => CameraMode::Free,
    }
}

pub fn camera_mode_toggle(
    keyboard_input: Res<ButtonInput<KeyCode>>,
    egui_manager: Res<crate::systems::egui_manager::EguiManager>,
    mut camera_state: ResMut<CameraState>,
    session: Res<crate::systems::session::SessionState>,
    driver: Res<crate::systems::driver::DriverState>,
    avatars: Query<(), With<crate::components::Avatar>>,
) {
    let follow = follow_available(&session, !avatars.is_empty());
    // Follow without a session (or an avatar) is dropped to Fly — this is also
    // what lands the camera in Fly when a live session ends, or on the first
    // frame of an offline start (the `CameraState` default is still Avatar).
    // Driver mode is exempt: it enters the follow camera itself, it is an offline
    // feature on purpose, and it stands down on its own, restoring the saved mode
    // on the way out. Device tracking (`systems::locate`) used to be exempted for
    // the same reason and no longer needs to be — the locate button takes no
    // camera mode at all now, so there is nothing here for it to fight.
    if !follow && !driver.active && camera_state.mode == CameraMode::Avatar {
        enter_mode(&mut camera_state, CameraMode::Free);
        camera_state.mode = CameraMode::Free;
    }
    if !keyboard_input.just_pressed(KeyCode::Tab) {
        return;
    }
    // **The only game binding in the client that had no guard at all.** Tab is
    // also egui's focus-traversal key, so pressing it while typing an address
    // into Search moved the focus *and* threw the camera into Fly mode — and
    // since Tab is now forwarded to egui unconditionally (`egui_manager`), the
    // overlap is live on every frame rather than only on focused ones. Same
    // predicate every other key in this file uses.
    if egui_manager.ui_owns_keyboard(false, &keyboard_input) {
        return;
    }
    let next = next_camera_mode(camera_state.mode, follow);
    enter_mode(&mut camera_state, next);
    camera_state.mode = next;
    println!("Camera mode: {:?}", camera_state.mode);
}

/// Land in Follow the moment a session comes up.
///
/// Signing in is the one thing in this client that adds a *body*: offline is the
/// map viewer and its switcher offers Fly and Earth only
/// ([`follow_available`]), so every sign-in used to finish with the camera still
/// in whichever map mode it happened to be in — the avatar the user had just
/// acquired standing somewhere off-screen, and Tab the only thing that said so.
/// `CameraState`'s own default has always been [`CameraMode::Avatar`]; this is
/// what makes that default reachable, since `camera_mode_toggle` drops it to Fly
/// on the first offline frame.
///
/// An **edge**, not a level: it fires on the frame Follow becomes available and
/// never again, so a user who then picks Earth stays in Earth for the rest of
/// the session. The `Local` is the previous answer rather than the previous
/// session status, because the avatar entity is half of it — the mode is no more
/// enterable without one than it is without a session.
///
/// Driver mode is exempt for the reason it is exempt in `camera_mode_toggle`: it
/// drives the follow camera itself and restores the pose it borrowed on the way
/// out, so writing the mode under it would be a no-op at best and would fight it
/// at worst.
///
/// It does overrule the camera mode `user_store::restore_saved_location` puts
/// back on a launch that signs in from a remembered token — the restore lands
/// early, while the handshake takes as long as a network round trip. That is the
/// intended reading of "signed in means following": the saved location still
/// decides *where* the world comes back, only not through which camera.
pub fn follow_on_sign_in(
    mut was_available: Local<bool>,
    session: Res<crate::systems::session::SessionState>,
    driver: Res<crate::systems::driver::DriverState>,
    mut camera_state: ResMut<CameraState>,
    avatars: Query<(), With<crate::components::Avatar>>,
) {
    let available = follow_available(&session, !avatars.is_empty());
    let arrived = available && !*was_available;
    *was_available = available;
    if !arrived || driver.active {
        return;
    }
    enter_mode(&mut camera_state, CameraMode::Avatar);
    camera_state.mode = CameraMode::Avatar;
}

/// Everything the user did this frame, in one parameter.
///
/// Bevy caps a system at 16 parameters and `camera_controls` was already at the
/// limit, so the touch bridge could not simply be added as a 17th. Grouping by
/// role rather than trimming: these five *are* the input, and the camera is the
/// only system that reads all of them together.
#[derive(bevy::ecs::system::SystemParam)]
pub struct CameraInput<'w, 's> {
    keyboard_input: Res<'w, ButtonInput<KeyCode>>,
    mouse_input: Res<'w, ButtonInput<MouseButton>>,
    cursor_moved_events: MessageReader<'w, 's, CursorMoved>,
    mouse_wheel_events: MessageReader<'w, 's, bevy::input::mouse::MouseWheel>,
    /// Pinch/twist/two-finger drag — the gestures with no mouse equivalent. The
    /// one-finger drag and every tap arrive through `mouse_input` above, which
    /// `systems::touch` drives from the primary finger.
    touch: Res<'w, crate::systems::touch::TouchGestures>,
    /// What the on-screen controls asked for (`systems::map_osd`) — the fly
    /// stick, the climb pair and the zoom pair. A finger's stand-in for the keys
    /// and the wheel, and the reason Fly mode no longer overloads the pinch.
    osd: Res<'w, crate::systems::map_osd::OsdInput>,
}

/// The follow rig's own smoothed state. One `Local` rather than three because
/// `camera_controls` is at Bevy's 16-parameter ceiling (the same reason
/// [`CameraInput`] exists), and because these three are meaningless apart: they
/// are all "where the follow camera has eased to".
struct RigSmoothing {
    /// Fraction of the boom currently extended (1.0 = no wall in the way).
    boom: f32,
    /// Metres the eye is currently dropped by a landing.
    dip: f32,
    /// Current vertical field of view (radians).
    fov: f32,
}

impl Default for RigSmoothing {
    fn default() -> Self {
        Self {
            boom: 1.0,
            dip: 0.0,
            fov: FOLLOW_FOV_BASE,
        }
    }
}

/// What the follow rig needs beyond the shared camera state: the world it can
/// collide against, and where it has eased to.
#[derive(bevy::ecs::system::SystemParam)]
pub struct FollowRig<'w, 's> {
    colliders: Res<'w, crate::systems::collision::BuildingColliders>,
    layers: Res<'w, crate::systems::data_layers::DataLayers>,
    smoothing: Local<'s, RigSmoothing>,
}

pub fn camera_controls(
    input: CameraInput,
    mut rig: FollowRig,
    mut camera_query: Query<(&mut GridCell, &mut Transform, &mut Projection), With<FreeCamera>>,
    mut camera_state: ResMut<CameraState>,
    avatar_state: Res<AvatarState>,
    time: Res<Time>,
    gizmo_state: Res<GizmoState>,
    egui_manager: Res<EguiManager>,
    globe: Res<crate::systems::globe::GlobeState>,
    windows: Query<&Window>,
    // Read-only view of the same camera for viewport→ray conversion (disjoint
    // components from the mutable query above).
    cam_render: Query<(&Camera, &GlobalTransform), With<FreeCamera>>,
    // Follow mode places the camera in the avatar's `GridCell` (its
    // `display_position` is cell-local). `Without<FreeCamera>` proves disjoint
    // access from the mutable camera query's `&mut GridCell`/`&mut Transform`.
    // The `Transform` gives the avatar's authoritative render yaw for the
    // follow-and-rotate chase (valid in both online and offline run modes).
    avatar_cells: Query<
        (&GridCell, &Transform),
        (With<crate::components::Avatar>, Without<FreeCamera>),
    >,
    terrain: Res<crate::systems::terrain::TerrainField>,
) {
    // Unpacked rather than used through `input.` so the body reads the same as
    // it did when these were five separate parameters.
    let CameraInput {
        keyboard_input,
        mouse_input,
        mut cursor_moved_events,
        mut mouse_wheel_events,
        touch,
        osd,
    } = input;

    if camera_query.is_empty() {
        return;
    }

    let Ok((mut camera_cell_mut, mut camera_transform_mut, mut projection)) =
        camera_query.single_mut()
    else {
        return;
    };
    // Work on copies of the camera's Transform + GridCell and write them back at the
    // end only if they actually changed. The follow-lerp / look_at / orbit placement
    // below rewrote both every frame even when the view was perfectly still.
    //
    // **Not** because a moved floating origin re-propagates the scene — this comment
    // used to say that, and it is wrong. `LocalFloatingOrigin::compute_all` seeds the
    // origin grid from the camera's *cell* (`grid/local_origin.rs`), so moving the
    // camera a metre recomputes exactly one `GlobalTransform`: its own. Crossing a
    // `WORLD_CELL_EDGE` boundary is the expensive event, and at 10 km that is ~40
    // minutes of walking (`docs/notes/render-performance-survey.md` has the rest, and
    // the measured conclusion that big_space is not this client's bottleneck).
    //
    // The guard is still worth keeping for the ordinary reason: a `Changed<Transform>`
    // on the camera is read by a great many client systems and by Bevy's own light and
    // cluster paths, and firing it on a stationary frame makes all of them do work for
    // a view that did not move.
    let prev_transform = *camera_transform_mut;
    let prev_cell = *camera_cell_mut;
    let mut camera_transform = prev_transform;
    let mut camera_cell = prev_cell;
    let delta_time = time.delta().as_secs_f32();

    // Keep the near/far at their ground defaults unless the space tier overrides
    // them below (Avatar/Free never touch the projection otherwise).
    set_projection(&mut projection, DEFAULT_NEAR, DEFAULT_FAR);
    // The speed-coupled FOV belongs to Follow alone; leaving it applied would carry
    // a sprint's wide lens into Fly and Earth. Reset the eased value too, so
    // returning to Follow starts from rest instead of unwinding a stale one.
    if camera_state.mode != CameraMode::Avatar {
        rig.smoothing.fov = FOLLOW_FOV_BASE;
        set_fov(&mut projection, FOLLOW_FOV_BASE);
    }

    // A finger holds the emulated left button down for the whole gesture
    // (`systems::touch`) but sends no `CursorMoved` with it, so each mode's
    // mouse-drag arm is entered and finds nothing to read. Drop the drag anchor
    // here instead: left stale, it would turn the *next* real mouse drag into a
    // single jump from wherever the last one ended.
    if touch.active() {
        camera_state.pan_offset = None;
    }

    match camera_state.mode {
        CameraMode::Avatar => {
            // Third-person camera following avatar (smoothed when online — see `AvatarState::display_position`)
            // Plus the seat: sitting slides the *model* off the avatar's own
            // position by up to a metre (`AvatarState::seat_shift`), and a camera
            // that kept framing the position would spend the sit looking at the
            // pavement beside the bench.
            let avatar_pos = avatar_state.display_position + avatar_state.seat_shift();

            // Driver mode owns the rig (`systems::driver`): distance, tilt and
            // azimuth are written from the map orientation every frame, so the
            // wheel, the drag *and* the third-person zoom clamp are all skipped —
            // the clamp tops out at 25 m, a quarter of the chase distance a
            // navigation view needs, and would drag it back in every frame.
            let locked = camera_state.drive_locked;
            if locked {
                // Driver mode owns the rig and draws its own marker in place of
                // the avatar; there is no first person inside it, and leaving the
                // flag set would hide the avatar behind the arrow for the rest of
                // the drive.
                camera_state.first_person = false;
            } else {
                // Proportional wheel zoom by notch *sign* — robust to line- vs
                // pixel-delta scrolling (raw `event.y` is ±1 on some backends, ±120 on
                // others; scaling it directly made one notch jump tens of metres). Sane
                // range for a ~1.8 m avatar; scroll in/out from the default.
                //
                // Accumulated as a *wish* rather than written straight to
                // `distance`, because the interesting part of this gesture is what
                // it asks for past the end of the range: the clamp used to absorb
                // that silently, and it is now the signal that the user wants to
                // come closer than third person can.
                let mut wish = camera_state.distance;
                for event in mouse_wheel_events.read() {
                    if event.y != 0.0 {
                        let factor = if event.y > 0.0 { 1.0 / 1.15 } else { 1.15 };
                        wish *= factor;
                    }
                }
                // Pinch is the wheel's continuous twin: spreading the fingers
                // pulls the avatar closer, exactly as scrolling in does.
                if touch.pair() && !touch.ui_blocked(egui_manager.ctx.egui_wants_pointer_input()) {
                    wish /= touch.pinch;
                }
                // …and the on-screen pair is the wheel's *held* twin, for the
                // hand that only has one thumb free (`systems::map_osd`).
                wish *= osd_zoom_factor(osd.zoom, delta_time);

                // Asking to get closer than the closest third-person framing is
                // the gesture: step inside the avatar rather than absorb the last
                // part of it into the clamp. One notch back out leaves again.
                let was_first_person = camera_state.first_person;
                camera_state.first_person =
                    first_person_after_zoom(camera_state.first_person, camera_state.distance, wish);
                if was_first_person && !camera_state.first_person {
                    // First person looks much further up and down than a boom can
                    // be swung (`FIRST_PERSON_PITCH_MAX`), so a tilt carried out
                    // of it unchanged would leave the third-person camera parked
                    // outside its own range — hanging almost directly overhead —
                    // until the next drag happened to clamp it. Bring it back on
                    // the frame the boom returns.
                    camera_state.pitch = camera_state
                        .pitch
                        .clamp(-std::f32::consts::PI / 3.0, std::f32::consts::PI / 2.5);
                }
                camera_state.distance = wish.clamp(AVATAR_CAM_MIN, AVATAR_CAM_MAX);
            }
            let first_person = camera_state.first_person;
            // The phone aims the view only from inside the head, and only while
            // the locate button is lit — `locate::track_device_pose` decides that
            // and writes the rotation; this is its consumer.
            let device_rotation = if first_person {
                camera_state.device_rotation
            } else {
                None
            };

            // How far the view may tilt. First person looks nearly all the way up
            // and down; the third-person clamp stops short of both because the
            // boom would swing under the pavement or overhead.
            let (pitch_min, pitch_max) = if first_person {
                (-FIRST_PERSON_PITCH_MAX, FIRST_PERSON_PITCH_MAX)
            } else {
                (-std::f32::consts::PI / 3.0, std::f32::consts::PI / 2.5)
            };

            // Handle mouse drag for rotation (suppressed while egui, gizmo, or Ctrl has the pointer).
            //
            // …and suppressed while the device is aiming the camera. Everywhere
            // else a drag is allowed to fight the compass and be eased back (the
            // rotate-follow rig's documented behaviour), but here the view *is*
            // the phone's own attitude: a drag could only push the picture off the
            // thing the phone is pointed at, and the sensor would drag it back the
            // same frame. Zooming out is the way to take the camera back.
            let ctrl = keyboard_input.pressed(KeyCode::ControlLeft)
                || keyboard_input.pressed(KeyCode::ControlRight);
            if locked
                || device_rotation.is_some()
                || gizmo_state.active_axis.is_some()
                || egui_manager.ctx.egui_wants_pointer_input()
                || ctrl
            {
                camera_state.pan_offset = None;
                cursor_moved_events.clear();
            } else if mouse_input.pressed(MouseButton::Left) {
                for event in cursor_moved_events.read() {
                    if let Some(last_pos) = camera_state.pan_offset {
                        let delta = event.position - last_pos;
                        if delta != Vec2::ZERO {
                            // Manually orbiting drops follow-and-rotate — the phone-map
                            // convention: dragging the map yourself stops it following
                            // your heading.
                            camera_state.follow_rotate = false;
                        }
                        camera_state.azimuth -= delta.x * 0.005;
                        camera_state.pitch -= delta.y * 0.005;
                        camera_state.pitch = camera_state.pitch.clamp(pitch_min, pitch_max);
                    }
                    camera_state.pan_offset = Some(event.position);
                }
            } else if mouse_input.just_released(MouseButton::Left) {
                camera_state.pan_offset = None;
            }

            // Touch: one finger orbits (the left-drag above), two fingers twist
            // the azimuth. Nearly the mouse's gate — a finger egui has claimed
            // holds a pointer button down, so `egui_wants_pointer_input()`
            // answers for it too — but routed through `ui_blocked`, because that
            // one boolean cannot answer for two fingers at once: the whole point
            // of the on-screen stick is that you hold it *while* turning the
            // view, and while it is held egui wants the pointer for as long as
            // the thumb rests there (`systems::touch`).
            if !locked
                && device_rotation.is_none()
                && gizmo_state.active_axis.is_none()
                && !touch.ui_blocked(egui_manager.ctx.egui_wants_pointer_input())
            {
                if touch.drag != Vec2::ZERO {
                    camera_state.follow_rotate = false;
                    camera_state.azimuth -= touch.drag.x * 0.005;
                    camera_state.pitch =
                        (camera_state.pitch - touch.drag.y * 0.005).clamp(pitch_min, pitch_max);
                }
                if touch.twist != 0.0 {
                    camera_state.follow_rotate = false;
                    camera_state.azimuth -= touch.twist;
                }
            }

            // Follow-and-rotate (the GPS button, pressed in this mode): glue the camera behind the
            // avatar's heading so it swings around as the avatar turns. The avatar's
            // render yaw `face` is authoritative in every run mode (the movement
            // systems write `Quat::from_rotation_y(face)` each frame), so read it off
            // the avatar transform rather than the mode-specific yaw fields. The camera
            // sits directly behind when `azimuth ≡ face + π` (its look direction then
            // equals the avatar's facing). Ease along the shortest arc so a turn glides.
            if camera_state.follow_rotate && device_rotation.is_none() {
                // The compass wins when there is one (`systems::locate` writes it
                // while the phone is being tracked); otherwise the avatar's own
                // facing, as before. Same convention, so this is a choice of
                // source and not a second code path.
                //
                // Skipped outright while the device is aiming the camera: there
                // the phone's whole attitude is the answer, and easing the azimuth
                // toward a bearing derived from the same sensor would be two rigs
                // turning one view.
                let heading = camera_state.follow_heading;
                if let Ok((_, avatar_tf)) = avatar_cells.single() {
                    let face =
                        heading.unwrap_or_else(|| avatar_tf.rotation.to_euler(EulerRot::YXZ).0);
                    let target = face + std::f32::consts::PI;
                    let mut d = (target - camera_state.azimuth).rem_euclid(std::f32::consts::TAU);
                    if d > std::f32::consts::PI {
                        d -= std::f32::consts::TAU;
                    }
                    // The compass gets the faster of the two rates — see
                    // [`COMPASS_ROTATE_RATE`] for why the two targets deserve
                    // different treatment rather than one shared number.
                    let rate = if heading.is_some() {
                        COMPASS_ROTATE_RATE
                    } else {
                        FOLLOW_ROTATE_RATE
                    };
                    let a = 1.0 - (-rate * delta_time).exp();
                    // Snap the last sliver so azimuth (and thus the whole follow rig)
                    // actually settles when standing still, instead of creeping toward
                    // the target forever and pinning the floating-origin propagation.
                    camera_state.azimuth += if d.abs() < 1e-3 { d } else { d * a };
                }
            }

            // The device aims the camera: read the rig's own angles back out of
            // its rotation, so everything downstream of them — the avatar's
            // facing, and the third-person view a zoom-out returns to — stays in
            // step with where the phone is pointed. The rotation itself is what
            // the camera is given below; these are derived from it and never the
            // other way round.
            if let Some(rot) = device_rotation {
                let forward = rot * Vec3::NEG_Z;
                let flat = Vec2::new(forward.x, forward.z);
                // A phone pointed at the sky or at its own feet is looking along
                // the one direction that has no bearing. Hold the last one rather
                // than let the azimuth spin through the pole — the picture is
                // taken from the rotation and is unaffected either way, so this
                // only decides which way the avatar is left facing.
                if flat.length_squared() > 1e-6 {
                    camera_state.azimuth = (-flat.x).atan2(-flat.y);
                }
                camera_state.pitch =
                    (-forward.y.clamp(-1.0, 1.0).asin()).clamp(pitch_min, pitch_max);
            }

            // Calculate camera position. The eye height is held out of the boom:
            // the boom is what shortens when a wall gets between the camera and
            // the avatar, and scaling the eye offset with it would sink the view
            // into the pavement as it came in.
            let horizontal_distance = camera_state.distance * camera_state.pitch.cos();
            let vertical_offset = camera_state.distance * camera_state.pitch.sin();
            let eye = Vec3::Y * FOLLOW_EYE_HEIGHT;
            let boom = Vec3::new(
                camera_state.azimuth.sin() * horizontal_distance,
                vertical_offset,
                camera_state.azimuth.cos() * horizontal_distance,
            );

            // Pull the boom in when a facade would come between the camera and the
            // avatar. The footprints carry no height (`systems::collision`), so
            // this only applies while the camera is low enough for a 2D answer to
            // be the right one — a high, steep view is looking over the roofs, and
            // testing it against footprints would yank the camera in every time the
            // avatar walked past a block.
            let mut boom_scale = 1.0_f32;
            if !locked
                && !first_person
                && rig.layers.buildings_enabled
                && !rig.colliders.is_empty()
                && (eye + boom).y < CAMERA_COLLIDE_MAX_Y
            {
                if let Ok((cell, _)) = avatar_cells.single() {
                    let base = Vec2::new(
                        cell.x as f32 * WORLD_CELL_EDGE,
                        cell.z as f32 * WORLD_CELL_EDGE,
                    );
                    let from = base + Vec2::new(avatar_pos.x, avatar_pos.z);
                    let to = from + Vec2::new(boom.x, boom.z);
                    if let Some(t) = rig.colliders.segment_fraction(from, to) {
                        boom_scale = (t - CAMERA_COLLIDE_MARGIN).clamp(CAMERA_COLLIDE_MIN, 1.0);
                    }
                }
            }
            // Ease the boom rather than snapping it. Coming *in* is urgent — the
            // wall is already in shot — so it is much the faster of the two; going
            // back out is eased hard, or every doorway the avatar passes makes the
            // camera lunge.
            let ease = if boom_scale < rig.smoothing.boom {
                1.0 - (-CAMERA_COLLIDE_IN_RATE * delta_time).exp()
            } else {
                1.0 - (-CAMERA_COLLIDE_OUT_RATE * delta_time).exp()
            };
            rig.smoothing.boom += (boom_scale - rig.smoothing.boom) * ease;
            if (boom_scale - rig.smoothing.boom).abs() < 1e-3 {
                rig.smoothing.boom = boom_scale;
            }

            // A landing shortens the eye height for a moment: the camera keeps
            // travelling down for an instant after the avatar has stopped, which is
            // what makes an arrival read as having weight. Scaled by the impact
            // speed, so stepping off a kerb is not the same event as falling off a
            // roof.
            if avatar_state.landing_impact > 0.0 {
                rig.smoothing.dip = (avatar_state.landing_impact / motion::TERMINAL_FALL_SPEED
                    * CAMERA_DIP_MAX)
                    .min(CAMERA_DIP_MAX)
                    .max(rig.smoothing.dip);
            }
            rig.smoothing.dip *= (-CAMERA_DIP_RECOVERY * delta_time).exp();
            if rig.smoothing.dip < 1e-3 {
                rig.smoothing.dip = 0.0;
            }

            // First person has no boom at all: the eye *is* the avatar's, so the
            // offset is the head height and the landing dip, which is the one part
            // of the rig that still means something from inside.
            let camera_offset = if first_person {
                Vec3::Y * (FIRST_PERSON_EYE_HEIGHT - rig.smoothing.dip)
            } else {
                eye + boom * rig.smoothing.boom - Vec3::Y * rig.smoothing.dip
            };

            // Lead the rig by exactly the lag its own ease will introduce
            // (`FOLLOW_LEAD_MAX`), so the avatar sits where the boom puts it at a
            // walk and at 600 m/s alike. Only the *eye* leads — `look_world`
            // below still aims at the avatar's real position, so the framing
            // moves and the subject does not.
            // Driver mode is exempt, as it is from the speed-coupled FOV: its rig
            // is written from the map orientation every frame and it already aims
            // ahead of the vehicle by a deliberate amount
            // (`CameraState::drive_lookahead`). Leading it as well would compose
            // two framings nobody chose together.
            // First person is exempt for a third reason: the lead exists to cancel
            // the follow ease's lag, and there is no ease to cancel when the eye
            // is glued to the head. Leading it would put the camera in front of
            // the avatar and look back through it.
            let lead = if locked || first_person {
                Vec3::ZERO
            } else {
                (avatar_state.velocity / FOLLOW_RATE).clamp_length_max(FOLLOW_LEAD_MAX)
            };
            let mut target_position = avatar_pos + camera_offset + lead;

            // Prevent camera from going below ground
            let min_height = MIN_CAMERA_HEIGHT;
            if target_position.y < min_height {
                target_position.y = min_height;
            }

            // Place the rig in *absolute* space rather than lerping the raw
            // `Transform.translation`. The avatar's `display_position` is local to
            // the avatar's `GridCell`; the camera, being the big_space floating
            // origin, can sit in a completely different cell (Orbit pushes it far).
            // Working through world metres — pivot the follow on the avatar's cell,
            // split the result back to a cell — is what makes "switch back to
            // Follow" reliably re-frame the avatar instead of stranding the camera a
            // grid cell away (the old code only rewrote `translation`, never the
            // cell). geo_nav's teleport uses the same avatar-cell convention.
            let avatar_cell = avatar_cells.single().map(|(c, _)| *c).unwrap_or(prev_cell);
            let avatar_origin = cell_origin(&avatar_cell);
            let target_world = avatar_origin + target_position.as_dvec3();
            let cam_world = cell_origin(&camera_cell) + camera_transform.translation.as_dvec3();
            let new_world = if first_person {
                // No ease from inside the head. The follow lerp exists to keep a
                // boom from snapping about as its subject moves; applied to the
                // eye itself it is the view sliding behind your own steps, which
                // reads as motion sickness rather than as smoothing. The avatar's
                // `display_position` is already smoothed on its own account
                // (online interpolation, the terrain lift), so what this tracks is
                // not jittery to begin with.
                camera_state.avatar_initialized = true;
                target_world
            } else if camera_state.avatar_initialized {
                // Framerate-independent follow smoothing, in world space.
                let cam_alpha = 1.0 - (-FOLLOW_RATE * delta_time).exp();
                let w = cam_world.lerp(target_world, cam_alpha as f64);
                // Snap once essentially settled, else an asymptotic lerp keeps the
                // camera transform changing by a hair while standing still — which
                // fires `Changed<Transform>` on the floating origin every frame, and
                // that is what the write-back guard at the top of this function
                // exists to avoid.
                if w.distance_squared(target_world) < 1e-6 {
                    target_world
                } else {
                    w
                }
            } else {
                // Entering Follow: snap straight onto the avatar (the reposition).
                camera_state.avatar_initialized = true;
                target_world
            };
            let grid = Grid::new(WORLD_CELL_EDGE, WORLD_SWITCH_THRESHOLD);
            let (new_cell, local) = grid.translation_to_grid(new_world);
            camera_cell = new_cell;
            camera_transform.translation = local;
            if first_person {
                // `look_at` cannot express this view: the eye and the subject are
                // the same point, so there is no target to aim at — the direction
                // is the state, not a consequence of it.
                //
                // The device's rotation is used whole when there is one, roll
                // included. That is the difference between a view that turns with
                // the phone and one that *is* the phone: tip it and the horizon
                // tips with it, which is what makes a building stay put on screen
                // while the hand holding it moves. Without a device it is the same
                // two angles the boom was built from, so crossing the threshold
                // keeps looking exactly where it was looking.
                camera_transform.rotation = device_rotation.unwrap_or_else(|| {
                    Quat::from_euler(
                        EulerRot::YXZ,
                        camera_state.azimuth,
                        -camera_state.pitch,
                        0.0,
                    )
                });
            } else {
                // Driver mode aims down the road rather than at the vehicle, which
                // is what puts the arrow in the lower third of the frame instead of
                // dead centre (`CameraState::drive_lookahead`). The camera itself
                // stays where the rig put it — only the look target moves. Taken as
                // a world-XZ vector rather than a distance along this camera's
                // forward: in a north-up drive those two point in different
                // directions, and the forward one aims north while you drive south.
                // Zero outside driver mode, so no branch on the mode is needed —
                // and driver mode never reaches the arm above, having cleared
                // `first_person` as it took the rig.
                let aim = camera_state.drive_lookahead;
                let ahead = Vec3::new(aim.x, 0.0, aim.y);
                let look_world =
                    avatar_origin + (avatar_pos + Vec3::Y * FOLLOW_EYE_HEIGHT + ahead).as_dvec3();
                camera_transform.look_at((look_world - cell_origin(&new_cell)).as_vec3(), Vec3::Y);
            }

            // Widen the lens with speed. Driver mode is exempt: its rig is written
            // from the map orientation every frame and a breathing FOV under a
            // north-up navigation view reads as a glitch, not as pace.
            let want_fov = if locked {
                FOLLOW_FOV_BASE
            } else {
                let t = (avatar_state.ground_speed / crate::systems::locomotion::RUN_SPEED)
                    .clamp(0.0, 1.0);
                FOLLOW_FOV_BASE + FOLLOW_FOV_SPRINT_GAIN * t
            };
            rig.smoothing.fov +=
                (want_fov - rig.smoothing.fov) * (1.0 - (-FOLLOW_FOV_RATE * delta_time).exp());
            if (want_fov - rig.smoothing.fov).abs() < 1e-4 {
                rig.smoothing.fov = want_fov;
            }
            set_fov(&mut projection, rig.smoothing.fov);
        }
        CameraMode::Free => {
            // Free camera mode - simple FPS style
            // On entry, either restore the last fly pose (return to where you left
            // Fly, since Orbit/Follow clobber the shared camera transform) or, the
            // first time ever, keep the current position and seed the look from the
            // live view so there's no jump.
            if !camera_state.free_initialized {
                if let Some((cell, translation, rotation)) = camera_state.free_saved {
                    camera_cell = cell;
                    camera_transform.translation = translation;
                    camera_state.free_camera_rotation = rotation;
                } else {
                    let euler = camera_transform.rotation.to_euler(EulerRot::YXZ);
                    camera_state.free_camera_rotation = Vec2::new(euler.1, euler.0);
                }
                camera_state.free_initialized = true;
            }
            let mut rotation_delta = Vec2::ZERO;

            // Mouse look (left mouse button — right is reserved for context menu).
            // Suppressed while egui, gizmo, or Ctrl has the pointer.
            let ctrl = keyboard_input.pressed(KeyCode::ControlLeft)
                || keyboard_input.pressed(KeyCode::ControlRight);
            if gizmo_state.active_axis.is_some()
                || egui_manager.ctx.egui_wants_pointer_input()
                || ctrl
            {
                camera_state.pan_offset = None;
                cursor_moved_events.clear();
            } else if mouse_input.pressed(MouseButton::Left) {
                for event in cursor_moved_events.read() {
                    if let Some(last_pos) = camera_state.pan_offset {
                        let delta = event.position - last_pos;
                        rotation_delta.x += delta.x;
                        rotation_delta.y += delta.y;
                    }
                    camera_state.pan_offset = Some(event.position);
                }
            } else {
                camera_state.pan_offset = None;
            }

            // Touch look: one finger is the mouse-look drag. The two-finger
            // gestures fly instead, and are handled with the keys below. Through
            // `ui_blocked` rather than egui's bare verdict, so the thumb on the
            // fly stick does not also veto the finger turning the view.
            let touch_owns_pointer = gizmo_state.active_axis.is_none()
                && !touch.ui_blocked(egui_manager.ctx.egui_wants_pointer_input());
            if touch_owns_pointer {
                rotation_delta += touch.drag;
            }

            // Update rotation from mouse input
            if rotation_delta.length() > 0.0 {
                camera_state.free_camera_rotation.y -= rotation_delta.x * MOUSE_SENSITIVITY;
                camera_state.free_camera_rotation.x -= rotation_delta.y * MOUSE_SENSITIVITY;
                camera_state.free_camera_rotation.x = camera_state
                    .free_camera_rotation
                    .x
                    .max(-std::f32::consts::PI / 2.0 + 0.1)
                    .min(std::f32::consts::PI / 2.0 - 0.1);
            }

            // Apply rotation
            let rotation_quat = Quat::from_euler(
                EulerRot::YXZ,
                camera_state.free_camera_rotation.y,
                camera_state.free_camera_rotation.x,
                0.0,
            );
            camera_transform.rotation = rotation_quat;

            // Movement - calculate forward/right from rotation, keep vertical independent
            let forward = rotation_quat * -Vec3::Z;
            let right = rotation_quat * Vec3::X;

            // Flatten to horizontal plane for WASD movement
            // `normalize_or_zero`, not `normalize`: at a pitch of exactly ±90°
            // the horizontal projection is the zero vector, and a NaN here
            // poisons the transform and every metric derived from it for the
            // rest of the run. The pose paths clamp the pitch (`CameraPose::
            // apply`), so this is the belt to that suspender.
            let forward_horizontal = Vec3::new(forward.x, 0.0, forward.z).normalize_or_zero();
            let right_horizontal = Vec3::new(right.x, 0.0, right.z).normalize_or_zero();

            let base_speed = if keyboard_input.pressed(KeyCode::ShiftLeft)
                || keyboard_input.pressed(KeyCode::ShiftRight)
            {
                FREE_CAMERA_SPEED_FAST
            } else {
                FREE_CAMERA_SPEED
            };
            // Speed rides altitude. Read it geocentrically (`GlobeState::altitude_m`) —
            // the local `y` is only altitude near the anchor and goes negative on the
            // far side of the globe, where a y-based scale would collapse to the
            // ground speed exactly where the distances are largest.
            let altitude = globe.altitude_m(&camera_cell, &camera_transform) as f32;
            let speed = fly_speed(base_speed, altitude);

            let mut move_direction = Vec3::ZERO;

            if keyboard_input.pressed(KeyCode::KeyW) {
                move_direction += forward_horizontal;
            }
            if keyboard_input.pressed(KeyCode::KeyS) {
                move_direction -= forward_horizontal;
            }
            if keyboard_input.pressed(KeyCode::KeyA) {
                move_direction -= right_horizontal;
            }
            if keyboard_input.pressed(KeyCode::KeyD) {
                move_direction += right_horizontal;
            }

            // Vertical movement — ALWAYS world-space, never affects rotation.
            // E up, Q down: the editor flycam convention (Unreal, Unity, Godot,
            // Blender), and *only* those two. Space used to be a second "up",
            // which made the vertical axis read as three keys with one of them
            // duplicated and left the most reachable key on the board spent on an
            // alias; it is a discrete verb now (see `avatar::handle_avatar_movement`).
            // Shift is the speed boost and *only* that — it used to also mean
            // "down, if no WASD is held", so holding it to cross a city dropped you
            // out of the sky the moment you let go of W. Ctrl, the obvious
            // alternative, isn't available: `EguiManager::ui_owns_keyboard` reads
            // any Ctrl press as a UI chord.
            if keyboard_input.pressed(KeyCode::KeyE) {
                move_direction += Vec3::Y;
            }
            if keyboard_input.pressed(KeyCode::KeyQ) {
                move_direction -= Vec3::Y;
            }

            // Suppress keyboard-driven movement while the UI owns the keyboard (text
            // field focused or a Ctrl chord like Ctrl+Shift+A).
            if egui_manager.ui_owns_keyboard(false, &keyboard_input) {
                move_direction = Vec3::ZERO;
            }

            // Touch flight: the on-screen stick and climb pair
            // (`systems::map_osd`), which stand in for WASD and E/Q. They join
            // `move_direction` rather than nudging the translation, so the same
            // accelerate/glide curve applies and a released control coasts to a
            // stop instead of stopping dead.
            //
            // **This used to be the two-finger gestures, and they are gone on
            // purpose.** Pinch was W/S and the two-finger drag was A/D + E/Q,
            // with only the *sign* of each read — so the smallest deliberate
            // spread was a full-speed press of W held for as long as the fingers
            // stayed down, and the one gesture whose meaning is settled
            // everywhere else in this client (pinch = zoom) meant something
            // different in this one mode. The stick's magnitude is kept instead
            // (below), which is the whole difference between steering and
            // pointing.
            move_direction += forward_horizontal * osd.fly.y + right_horizontal * osd.fly.x;
            move_direction += Vec3::Y * osd.climb;

            // The keys steer a velocity, not the translation: it chases the wish on
            // an exponential approach (frame-rate independent), so a release glides
            // to a stop instead of the camera stopping dead mid-flight. Braking is
            // deliberately slower than accelerating — the glide is the point, and a
            // symmetric curve makes the start feel mushy to get it.
            //
            // The throttle is what makes a half-pushed stick fly at half speed.
            // A key contributes a unit vector, so a single key is throttle 1 and
            // any combination of them saturates at 1 — i.e. the keyboard behaves
            // exactly as it did before the stick existed.
            let throttle = move_direction.length().min(1.0);
            let wish = if throttle > 0.0 {
                move_direction.normalize() * speed * throttle
            } else {
                Vec3::ZERO
            };
            let tau = if wish == Vec3::ZERO {
                FREE_DECEL_TAU
            } else {
                FREE_ACCEL_TAU
            };
            let blend = 1.0 - (-delta_time / tau).exp();
            let mut velocity =
                camera_state.free_velocity + (wish - camera_state.free_velocity) * blend;
            if wish == Vec3::ZERO
                && velocity.length_squared() <= (speed * FREE_REST_FRACTION).powi(2)
            {
                velocity = Vec3::ZERO;
            }
            camera_state.free_velocity = velocity;
            if velocity != Vec3::ZERO {
                camera_transform.translation += velocity * delta_time;
            }

            // Prevent going below ground — including the measured ground, or flying
            // low over the Veluwe puts the camera inside the hill.
            let ground_height = terrain.ground_at(
                camera_cell.x as f32 * WORLD_CELL_EDGE + camera_transform.translation.x,
                camera_cell.z as f32 * WORLD_CELL_EDGE + camera_transform.translation.z,
            );
            let min_height = ground_height + MIN_CAMERA_HEIGHT;
            let lift = ground_lift(
                camera_cell.y as f64,
                camera_transform.translation.y,
                min_height,
            );
            if lift > 0.0 {
                camera_transform.translation.y += lift;
                // Kill the downward glide at the floor, or a coast into the ground
                // keeps pressing against the clamp long after the key is released.
                camera_state.free_velocity.y = camera_state.free_velocity.y.max(0.0);
            }

            // Remember this pose so re-entering Fly returns here.
            camera_state.free_saved = Some((
                camera_cell,
                camera_transform.translation,
                camera_state.free_camera_rotation,
            ));
        }
        CameraMode::Orbit => {
            // "Earth" mode: a Google-Earth-style orbit. Near the ground it pivots
            // a fixed ground point (left-drag pans, middle/Ctrl+left rotates+tilts,
            // wheel zooms). Zoom out far enough and it hands off to the space tier
            // (ADR-025): the pivot becomes the Earth's centre and left-drag spins
            // the whole globe. The altitude drives map LOD, so pulling back
            // coarsens the imagery and, past the show altitude, reveals the planet.

            // On entry, seed the ground focus/distance/angles from wherever the
            // camera was looking, so switching modes doesn't jump the view.
            if !camera_state.orbit_initialized {
                let cam_world = cell_origin(&camera_cell) + camera_transform.translation.as_dvec3();
                let forward = (camera_transform.rotation * Vec3::NEG_Z).as_dvec3();
                // Intersect the view ray with the ground plane (y = 0); if the
                // camera looks level or up, drop the focus straight below it.
                let focus = if forward.y < -1e-3 {
                    let t = cam_world.y / -forward.y;
                    let hit = cam_world + forward * t;
                    DVec3::new(hit.x, 0.0, hit.z)
                } else {
                    DVec3::new(cam_world.x, 0.0, cam_world.z)
                };
                let offset = cam_world - focus;
                let dist = offset
                    .length()
                    .clamp(ORBIT_MIN_DISTANCE, ORBIT_MAX_DISTANCE_SURFACE);
                camera_state.orbit_focus = focus;
                camera_state.orbit_distance = dist;
                camera_state.orbit_distance_target = dist;
                camera_state.orbit_yaw = (offset.x).atan2(offset.z) as f32;
                camera_state.orbit_pitch = ((offset.y / dist).clamp(-1.0, 1.0) as f32)
                    .asin()
                    .clamp(ORBIT_PITCH_MIN, ORBIT_PITCH_MAX);
                camera_state.orbit_space = false;
                camera_state.orbit_initialized = true;
            }

            let center = globe.center_local.as_dvec3();
            let radius = globe.radius as f64;

            // Surface↔space handoff. Reparameterise about the new pivot from the
            // unchanged camera world position, so the camera does not move; the
            // zoom target carries over as a ratio so an in-flight glide continues
            // smoothly through the transition.
            if globe.ready {
                let cam_world = if camera_state.orbit_space {
                    center + camera_state.orbit_dir * camera_state.orbit_distance
                } else {
                    camera_state.orbit_focus
                        + spherical_offset(
                            camera_state.orbit_yaw,
                            camera_state.orbit_pitch,
                            camera_state.orbit_distance,
                        )
                };
                let alt = (cam_world - center).length() - radius;
                if !camera_state.orbit_space && alt > SPACE_ENTER_ALT {
                    let off = cam_world - center;
                    let d = off.length();
                    let ratio = camera_state.orbit_distance_target / camera_state.orbit_distance;
                    camera_state.orbit_space = true;
                    camera_state.orbit_distance = d;
                    camera_state.orbit_distance_target = d * ratio;
                    // Carry the current orientation up into the space tier: the
                    // direction from the Earth's centre, plus the heading that
                    // reproduces the camera's present up, so the view doesn't jump.
                    camera_state.orbit_dir = off / d;
                    let up = (camera_transform.rotation * Vec3::Y).as_dvec3();
                    camera_state.orbit_heading = globe_heading(
                        camera_state.orbit_dir,
                        globe.north_axis_local().as_dvec3(),
                        up,
                    );
                    camera_state.orbit_up = up;
                } else if camera_state.orbit_space && alt < SPACE_EXIT_ALT {
                    // How far the descent point is from the home anchor (origin).
                    let horiz = (cam_world.x * cam_world.x + cam_world.z * cam_world.z).sqrt();
                    if horiz > REANCHOR_DISTANCE && camera_state.reanchor_request.is_none() {
                        // Descending onto a different part of the globe: re-anchor
                        // the world to the sub-point (handled by a teleport next
                        // frame) rather than snapping to the home plane far away.
                        let dir = (cam_world - center).normalize_or_zero().as_vec3();
                        let (slat, slng) = crate::systems::globe::sub_point(
                            globe.anchor_lat,
                            globe.anchor_lng,
                            dir,
                        );
                        camera_state.reanchor_request = Some((slat, slng, alt.max(SPACE_EXIT_ALT)));
                        // Stay in space this frame; the teleport repositions us.
                    } else if horiz <= REANCHOR_DISTANCE {
                        // Near home: hand back to the ground-focus orbit as before.
                        let focus = DVec3::new(cam_world.x, 0.0, cam_world.z);
                        let off = cam_world - focus;
                        let d = off.length().max(ORBIT_MIN_DISTANCE);
                        let ratio =
                            camera_state.orbit_distance_target / camera_state.orbit_distance;
                        camera_state.orbit_space = false;
                        camera_state.orbit_focus = focus;
                        camera_state.orbit_distance = d;
                        camera_state.orbit_distance_target = (d * ratio).max(ORBIT_MIN_DISTANCE);
                        camera_state.orbit_yaw = off.x.atan2(off.z) as f32;
                        camera_state.orbit_pitch = ((off.y / d).clamp(-1.0, 1.0) as f32)
                            .asin()
                            .clamp(ORBIT_PITCH_MIN, ORBIT_PITCH_MAX);
                    }
                }
            }
            let space = camera_state.orbit_space;

            // Gather drag deltas, split by gesture. In space, left-drag spins the
            // globe (rotate); on the surface it pans the map, and rotate is on the
            // middle button or Ctrl+left. Suppressed while egui/gizmo owns pointer.
            let mut rot_delta = Vec2::ZERO;
            let mut pan_delta = Vec2::ZERO;
            // Space tier only: horizontal drag of the "turn the view" gesture,
            // which spins the compass rather than the planet.
            let mut heading_delta = 0.0_f32;
            if gizmo_state.active_axis.is_some()
                || touch.ui_blocked(egui_manager.ctx.egui_wants_pointer_input())
            {
                camera_state.pan_offset = None;
                cursor_moved_events.clear();
            } else {
                let ctrl = keyboard_input.pressed(KeyCode::ControlLeft)
                    || keyboard_input.pressed(KeyCode::ControlRight);
                // Google Earth's split: plain drag moves you over the planet,
                // middle-drag (or Ctrl+drag) turns the *view* — compass heading in
                // space, yaw/tilt on the surface. In space that keeps plain
                // left-drag as "grab the globe and spin it" with north still up.
                let turning = mouse_input.pressed(MouseButton::Middle)
                    || (ctrl && mouse_input.pressed(MouseButton::Left));
                let rotating = turning || (space && mouse_input.pressed(MouseButton::Left));
                let panning = !rotating && !space && mouse_input.pressed(MouseButton::Left);
                if rotating || panning {
                    for event in cursor_moved_events.read() {
                        if let Some(last) = camera_state.pan_offset {
                            let delta = event.position - last;
                            if turning && space {
                                heading_delta += delta.x;
                            } else if rotating {
                                rot_delta += delta;
                            } else {
                                pan_delta += delta;
                            }
                        }
                        camera_state.pan_offset = Some(event.position);
                    }
                } else {
                    camera_state.pan_offset = None;
                    cursor_moved_events.clear();
                }

                // Touch, mapped onto the same three deltas so everything below
                // — sensitivity, the north lock, the auto-tilt floor — applies
                // unchanged. The split follows the phone map convention rather
                // than the mouse one: one finger is the *plain* drag (pan on the
                // surface, spin the globe in space), and the turn/tilt gesture
                // that the mouse puts on the middle button is the two-finger
                // twist and the two-finger vertical drag.
                //
                // Only `two_drag.y` is used: a twist that isn't perfectly
                // centred moves the centroid sideways, so a horizontal reading
                // would double-count every rotation as a pan.
                //
                // The twist divides by `ORBIT_ROT_SENS` so the multiplication
                // by it below cancels: a twist is measured in radians and lands
                // as that many radians of view rotation, 1:1 with the fingers,
                // while a drag stays in points and keeps the mouse's tuning.
                if touch.pair() {
                    if space {
                        heading_delta += touch.twist / ORBIT_ROT_SENS;
                    } else {
                        rot_delta.x += touch.twist / ORBIT_ROT_SENS;
                        rot_delta.y += touch.two_drag.y;
                    }
                } else if space {
                    rot_delta += touch.drag;
                } else {
                    pan_delta += touch.drag;
                }
            }

            // Wheel zoom: exponential toward/away from the pivot; clamps differ by
            // tier (ground metres vs orbit distance from the Earth centre).
            let (min_d, max_d) = if space {
                // Down to the surface (alt 0); the exit handoff fires at
                // SPACE_EXIT_ALT well before the camera reaches it.
                (radius, ORBIT_MAX_DISTANCE_SPACE)
            } else {
                (ORBIT_MIN_DISTANCE, ORBIT_MAX_DISTANCE_SURFACE)
            };
            // The wheel moves a *target*; the camera glides toward it below, so
            // zoom reads as a smooth fly instead of a per-notch height jump.
            for event in mouse_wheel_events.read() {
                let factor = (1.0 - event.y * ORBIT_ZOOM_SPEED).clamp(0.2, 5.0) as f64;
                camera_state.orbit_distance_target = zoom_orbit_target(
                    camera_state.orbit_distance_target,
                    factor,
                    space,
                    radius,
                    (min_d, max_d),
                );
            }
            // Pinch is the wheel without notches: the ratio the fingers moved
            // apart by *is* the factor, so spreading them by half brings the
            // ground half as close. Sharing `zoom_orbit_target` with the wheel is
            // what keeps the space tier's "a step is a fraction of the altitude,
            // not of the Earth's radius" rule applying to the fingers too.
            if touch.pair() && !touch.ui_blocked(egui_manager.ctx.egui_wants_pointer_input()) {
                camera_state.orbit_distance_target = zoom_orbit_target(
                    camera_state.orbit_distance_target,
                    1.0 / touch.pinch as f64,
                    space,
                    radius,
                    (min_d, max_d),
                );
            }
            // The on-screen pair, for the hand that cannot spare a second finger
            // (`systems::map_osd`). Through the same `zoom_orbit_target`, so it
            // inherits the space tier's "a step is a fraction of the altitude"
            // rule along with the wheel and the pinch.
            if osd.zoom != 0.0 {
                camera_state.orbit_distance_target = zoom_orbit_target(
                    camera_state.orbit_distance_target,
                    f64::from(osd_zoom_factor(osd.zoom, delta_time)),
                    space,
                    radius,
                    (min_d, max_d),
                );
            }
            let glide = 1.0 - (-ZOOM_GLIDE_RATE * delta_time as f64).exp();
            let old_d = camera_state.orbit_distance;
            camera_state.orbit_distance =
                (old_d + (camera_state.orbit_distance_target - old_d) * glide).clamp(min_d, max_d);
            // Per-frame zoom fraction (< 1 while flying in), which also paces the
            // cursor homing so it tracks the glide rather than the wheel notches.
            let zoom_factor = camera_state.orbit_distance / old_d;

            // Zoom-toward-cursor (space): scrolling in homes the orbit onto the
            // point under the cursor — cast the cursor ray onto the sphere and
            // pull the pivot direction toward the hit by the zoom fraction. This
            // is how you dive back to a specific spot on the globe (centre-locked
            // zoom made re-finding a place nearly impossible).
            if space && zoom_factor < 0.999 && !egui_manager.ctx.egui_wants_pointer_input() {
                let cursor_hit = windows
                    .iter()
                    .next()
                    .and_then(|w| w.cursor_position())
                    .zip(cam_render.single().ok())
                    .and_then(|(cursor, (cam, cam_gt))| {
                        // Proven ray construction (same call picking uses). The ray
                        // is in render space — world shifted by the camera's grid
                        // cell — so shift the sphere centre instead of the ray.
                        let ray = cam.viewport_to_world(cam_gt, cursor).ok()?;
                        let center_render = center - cell_origin(&camera_cell);
                        let origin = ray.origin.as_dvec3();
                        let dir = ray.direction.as_vec3().as_dvec3();
                        let oc = origin - center_render;
                        let b = oc.dot(dir);
                        let disc = b * b - (oc.length_squared() - radius * radius);
                        if disc < 0.0 {
                            return None; // cursor misses the globe
                        }
                        let t = -b - disc.sqrt();
                        // Direction from the centre is translation-invariant, so
                        // it's valid in the world frame too.
                        (t > 0.0).then(|| (origin + dir * t - center_render).normalize())
                    });
                if let Some(hit_dir) = cursor_hit {
                    // Pull the orbit toward the cursor point at the pace of the
                    // glide. Only the direction moves — up is re-derived from
                    // north below, so zooming never rolls the view.
                    let blend = ((1.0 - zoom_factor) * 1.5).clamp(0.0, 0.5);
                    camera_state.orbit_dir =
                        camera_state.orbit_dir.lerp(hit_dir, blend).normalize();
                }
            }

            // Compass lock: the view is *held* north-up, so the turn gesture is
            // inert and the ease below is re-armed every frame.
            let north_lock = camera_state.orbit_north_lock;

            // Space: turn the view (compass heading) without moving the camera.
            if space && heading_delta != 0.0 && !north_lock {
                camera_state.orbit_heading -= heading_delta * ORBIT_ROT_SENS;
                camera_state.orbit_north_reset = false;
            }

            // Apply rotation. Space: grab-and-spin the globe — horizontal drag
            // rotates the orbit about the screen-up axis, vertical about
            // screen-right, so the ground follows the cursor on both axes. Up is
            // re-derived from north below rather than rotated along, so the
            // horizon stays level and the view never rolls off north by itself.
            // Surface: the yaw/pitch turntable as before.
            if rot_delta != Vec2::ZERO {
                if space {
                    // Grab-the-globe: scale radians-per-pixel so the ground under
                    // the cursor tracks the drag at any distance. Two regimes,
                    // take whichever is smaller:
                    //  - near: the visible ground patch subtends ~fov·(alt/R) of
                    //    central angle, so rotation shrinks with altitude (the old
                    //    far-field-only formula stayed too fast in the mid-range);
                    //  - far: the whole globe spans 2·asin(R/d) of the view, so a
                    //    full-window drag sweeps about one globe-height.
                    let h = windows.iter().next().map(|w| w.height()).unwrap_or(800.0);
                    let fov = match &*projection {
                        Projection::Perspective(p) => p.fov,
                        _ => std::f32::consts::FRAC_PI_4,
                    };
                    let d = camera_state.orbit_distance;
                    let near_arc = fov * (((d - radius) / radius).max(0.0) as f32);
                    let far_arc = 2.4 * ((radius / d).clamp(0.0, 1.0) as f32).asin();
                    let sens = (1.2 * near_arc.min(far_arc) / h).clamp(2e-6, ORBIT_ROT_SENS);
                    // Compass locked: spin about the true polar axis (north pinned
                    // up, longitude-only horizontal, latitude clamped short of the
                    // pole) instead of the screen-relative grab-the-globe rotate,
                    // so the view can no longer be carried around the poles.
                    camera_state.orbit_dir = if north_lock {
                        globe_spin_polar(
                            camera_state.orbit_dir,
                            globe.north_axis_local().as_dvec3(),
                            rot_delta,
                            sens,
                        )
                    } else {
                        globe_spin(
                            camera_state.orbit_dir,
                            globe.north_axis_local().as_dvec3(),
                            camera_state.orbit_up,
                            rot_delta,
                            sens,
                        )
                    };
                } else {
                    if !north_lock {
                        camera_state.orbit_yaw -= rot_delta.x * ORBIT_ROT_SENS;
                    }
                    camera_state.orbit_pitch = (camera_state.orbit_pitch
                        + rot_delta.y * ORBIT_ROT_SENS)
                        .clamp(ORBIT_PITCH_MIN, ORBIT_PITCH_MAX);
                }
            }

            // Compass "north up" reset. Eases the view back to north without
            // moving the pivot: on the surface, yaw → the nearest multiple of a
            // full turn; in space, heading → 0. Any manual rotate this frame
            // cancels it (grabbing the globe wins) — unless the compass is
            // locked, in which case the ease is re-armed instead, so spinning
            // the globe can never carry the poles off vertical.
            if rot_delta != Vec2::ZERO && !north_lock {
                camera_state.orbit_north_reset = false;
            }
            if north_lock {
                camera_state.orbit_north_reset = true;
            }
            if camera_state.orbit_north_reset {
                let k = (1.0 - (-NORTH_RESET_RATE * delta_time).exp()).clamp(0.0, 1.0);
                if space {
                    // Take the short way round: a heading of 350° eases to 360°,
                    // not backwards through 180°.
                    let tau = std::f32::consts::TAU;
                    let target = (camera_state.orbit_heading / tau).round() * tau;
                    camera_state.orbit_heading += (target - camera_state.orbit_heading) * k;
                    if (target - camera_state.orbit_heading).abs() < 1e-4 {
                        camera_state.orbit_heading = 0.0;
                        camera_state.orbit_north_reset = false;
                    }
                } else {
                    let target = (camera_state.orbit_yaw / std::f32::consts::TAU).round()
                        * std::f32::consts::TAU;
                    camera_state.orbit_yaw += (target - camera_state.orbit_yaw) * k;
                    if (target - camera_state.orbit_yaw).abs() < 1e-3 {
                        camera_state.orbit_yaw = target;
                        camera_state.orbit_north_reset = false;
                    }
                }
            }

            // Google-Earth-style auto-tilt (surface orbit): pulling back raises a
            // pitch floor toward top-down on a log ramp (2 km → 60 km), so zoom-out
            // always lifts you *over* the map instead of leaving the camera parked
            // at a grazing angle staring at the horizon/atmosphere. Tilting freely
            // is still allowed up close, where oblique views are useful.
            if !space {
                let t = ((camera_state.orbit_distance / 2_000.0).ln()
                    / (60_000.0f64 / 2_000.0).ln())
                .clamp(0.0, 1.0) as f32;
                let floor = ORBIT_PITCH_MIN + (1.30 - ORBIT_PITCH_MIN) * t;
                if camera_state.orbit_pitch < floor {
                    camera_state.orbit_pitch = floor;
                }
            }

            let (yaw, pitch) = (camera_state.orbit_yaw, camera_state.orbit_pitch);
            let offset = spherical_offset(yaw, pitch, camera_state.orbit_distance);

            // Pan (surface only): grab-and-drag the ground — the map follows the
            // cursor on BOTH axes. The basis comes straight from the orbit yaw
            // (exact even at near-nadir tilt, where a flattened camera-forward
            // degenerates): `fwd_flat` is the horizontal camera→focus direction,
            // i.e. "screen-up on the ground".
            if pan_delta != Vec2::ZERO {
                let (sy, cy) = (camera_state.orbit_yaw.sin(), camera_state.orbit_yaw.cos());
                let fwd_flat = Vec3::new(-sy, 0.0, -cy);
                let right = Vec3::new(cy, 0.0, -sy);
                let scale = (camera_state.orbit_distance * ORBIT_PAN_FACTOR) as f32;
                // Drag right → rig moves left → content follows the cursor right;
                // drag down → rig moves screen-up on the ground → content slides
                // down. (The vertical axis used to move the rig the other way —
                // grab horizontally but inverted vertically, which is why panning
                // felt "vice versa" up close while the space globe felt right.)
                let shift = right * (-pan_delta.x * scale) + fwd_flat * (pan_delta.y * scale);
                camera_state.orbit_focus += shift.as_dvec3();
                camera_state.orbit_focus.y = 0.0;
            }

            // Place the camera: pivot + offset → big_space cell + local, then look
            // at the pivot (ground focus, or the Earth centre in space). In space
            // the up vector is derived fresh from the planet's north axis and the
            // heading, so the horizon is level by construction — no drift to
            // re-orthonormalize away, and no pole lock either (the sub-camera
            // latitude is held short of the pole by `globe_spin`).
            let (pivot, cam_world, look_up) = if space {
                let dir = camera_state.orbit_dir;
                let up = globe_up(
                    dir,
                    globe.north_axis_local().as_dvec3(),
                    camera_state.orbit_heading,
                    camera_state.orbit_up,
                );
                camera_state.orbit_up = up;
                (
                    center,
                    center + dir * camera_state.orbit_distance,
                    up.as_vec3(),
                )
            } else {
                (
                    camera_state.orbit_focus,
                    camera_state.orbit_focus + offset,
                    Vec3::Y,
                )
            };
            let grid = Grid::new(WORLD_CELL_EDGE, WORLD_SWITCH_THRESHOLD);
            let (new_cell, local) = grid.translation_to_grid(cam_world);
            camera_cell = new_cell;
            camera_transform.translation = local;
            let pivot_local = (pivot - cell_origin(&new_cell)).as_vec3();
            camera_transform.look_at(pivot_local, look_up);

            // Earth now holds a real pose; re-entering restores it instead of
            // reseeding from the current view (see `enter_mode`).
            camera_state.orbit_has_pose = true;
        }
    }

    // Depth range for the space tier — applied in EVERY camera mode, not just
    // Orbit. The projection is reset to the ground defaults each frame (near 0.1,
    // far 200 km, above); once the camera climbs high enough that the globe (or
    // the far ground) sits beyond that 200 km far plane, everything gets clipped
    // and the screen goes black. This was previously only fixed inside the Orbit
    // arm, so flying up in Free/Avatar mode (or any high view) clipped the whole
    // world. Push the far plane past the far limb and raise the near plane with
    // altitude so nothing relevant clips, keyed off the true geocentric altitude.
    if globe.ready {
        let cam_world = cell_origin(&camera_cell) + camera_transform.translation.as_dvec3();
        let center = globe.center_local.as_dvec3();
        let dist_center = (cam_world - center).length();
        let altitude = dist_center - globe.radius as f64;
        if altitude as f32 > crate::systems::globe::GLOBE_SHOW_ALT {
            // Keep `near` below the star/moon shells (~150 km) so they stay visible
            // from deep space; reversed-z keeps depth precise even with a small
            // near against the far-limb `far`.
            let near = (altitude as f32 * 0.01).clamp(100.0, 50_000.0);
            let far = (dist_center as f32 + globe.radius * 2.5).clamp(DEFAULT_FAR, 60_000_000.0);
            set_projection(&mut projection, near, far);
        } else if altitude as f32 > crate::systems::globe::GLOBE_DRAW_ALT {
            // The band where both tiers are drawn. The 200 km default far plane
            // was set for a world that ended at the 150 km ground skirt; the
            // planet does not end, and its horizon is already 391 km away from
            // 12 km up. Left at 200 km the globe is simply sliced off in a
            // straight line partway to the horizon — which looks exactly like
            // the flat edge this whole change is removing.
            let far = band_far_plane_m(altitude as f32, globe.radius);
            // Reversed-z spends its precision near the near plane, so a far
            // plane twenty times further out has to be paid for at the front.
            // Tied to altitude because that *is* the distance to the nearest
            // thing the camera can see down here: at 2 km the ground is 2 km
            // away and 8 m of near plane clips nothing, and by 50 km it has
            // grown to 200 m without ever passing the ground.
            let near = (altitude as f32 * NEAR_PLANE_ALTITUDE_FRACTION).clamp(DEFAULT_NEAR, 200.0);
            set_projection(&mut projection, near, far);
        }
    }

    // Commit the pose only if it actually changed. This is the payoff of the working
    // copies above: on a still frame nothing differs, the FloatingOrigin stays
    // unchanged, and big_space skips the whole-scene GlobalTransform recompute.
    if camera_transform != prev_transform {
        *camera_transform_mut = camera_transform;
    }
    if camera_cell != prev_cell {
        *camera_cell_mut = camera_cell;
    }
}

/// Take the avatar's own model out of shot while the camera is inside its head,
/// and put it back on the way out.
///
/// The same two-level write `driver::update_driver_arrow` does and for the same
/// reason, which its doc comment gives in full: the avatar entity carries no
/// `Visibility` of its own, so each model holder under it is its own propagation
/// root and hiding the parent alone changes nothing. Reconciled every frame rather
/// than on the edge, because the holders are spawned asynchronously and an
/// appearance change respawns them — and only the *direct* children, because
/// deeper glTF nodes carry a permanent `Hidden` from the slot mask and restoring
/// those to `Inherited` would un-mask a mixed avatar.
///
/// It stands down entirely while driver mode is on rather than co-operating with
/// it: two systems writing one entity's visibility from two conditions is a fight
/// nobody can read off a screen. Driver mode also clears
/// [`CameraState::first_person`] as it takes the rig, so by the time it hands the
/// avatar back there is nothing here left wanting it hidden.
pub fn update_first_person_avatar(
    mut commands: Commands,
    camera_state: Res<CameraState>,
    driver: Res<crate::systems::driver::DriverState>,
    avatars: Query<Entity, With<crate::components::Avatar>>,
    children: Query<&Children>,
    visibility: Query<&Visibility>,
    mut hidden: Local<bool>,
) {
    if driver.active {
        return;
    }
    let want = camera_state.first_person && camera_state.mode == CameraMode::Avatar;
    if !want && !*hidden {
        return;
    }
    *hidden = want;
    let v = if want {
        Visibility::Hidden
    } else {
        Visibility::Inherited
    };
    for avatar in avatars.iter() {
        let mut write = |e: Entity| {
            if visibility.get(e) != Ok(&v) {
                commands.entity(e).try_insert(v);
            }
        };
        write(avatar);
        for holder in children.get(avatar).into_iter().flatten() {
            write(*holder);
        }
    }
}

/// How far (m) the Fly camera must be lifted to clear `min_height`, for a camera
/// sitting at `local_y` inside big_space cell index `cell_y`. Zero when it's
/// already above.
///
/// The comparison has to be made in **absolute** metres. `Transform::translation.y`
/// is only the offset within the current [`WORLD_CELL_EDGE`] (10 km) cell, so
/// testing it directly against a ground height put a hard floor 0.5 m above *every*
/// 10 km multiple: descend past 70 km and the camera pinned itself at 70,000.5 m
/// with "down" doing nothing. It went unnoticed while the fly speed was a flat
/// 45 m/s — nobody descends 70 km at walking pace — and surfaced the day speed
/// started scaling with altitude.
fn ground_lift(cell_y: f64, local_y: f32, min_height: f32) -> f32 {
    let absolute = cell_y * WORLD_CELL_EDGE as f64 + local_y as f64;
    (min_height as f64 - absolute).max(0.0) as f32
}

#[cfg(test)]
mod first_person_tests {
    use super::*;

    /// One notch in from the closest third-person framing steps inside the
    /// avatar, and one notch back out steps straight back. The wheel's own factor
    /// is used rather than an invented one, so the test fails if a notch ever
    /// becomes too small to cross the threshold in a single turn.
    #[test]
    fn a_notch_past_the_closest_view_enters_and_a_notch_out_leaves() {
        let inward = AVATAR_CAM_MIN / 1.15;
        assert!(
            first_person_after_zoom(false, AVATAR_CAM_MIN, inward),
            "zooming in at the minimum should enter first person"
        );
        let outward = AVATAR_CAM_MIN * 1.15;
        assert!(
            !first_person_after_zoom(true, AVATAR_CAM_MIN, outward),
            "zooming out should leave first person"
        );
    }

    /// A gesture that changes nothing must not change the state — which is the
    /// case every frame of a still camera, and the one that would make the view
    /// flicker between the two if the comparison were the wrong way round.
    #[test]
    fn standing_still_stays_where_it_is() {
        for d in [AVATAR_CAM_MIN, 6.0, AVATAR_CAM_MAX] {
            assert!(
                !first_person_after_zoom(false, d, d),
                "{d} entered by itself"
            );
        }
        assert!(
            first_person_after_zoom(true, AVATAR_CAM_MIN, AVATAR_CAM_MIN),
            "first person left by itself"
        );
    }

    /// Zooming out from the far end of the range must not enter first person on
    /// the way past, and zooming in from it must not either until it reaches the
    /// bottom — the threshold is about the *end* of the range, not about the
    /// direction of the gesture.
    #[test]
    fn the_middle_of_the_range_is_third_person() {
        assert!(!first_person_after_zoom(false, AVATAR_CAM_MAX, 4.0));
        assert!(!first_person_after_zoom(false, 4.0, 2.0));
    }

    /// The continuity claim the threshold rests on: crossing it must not swing
    /// the view. In third person the camera sits on a boom and looks back down
    /// it, and in first person it looks along the angles that boom was built
    /// from — so the two look directions have to be the same vector for the same
    /// `azimuth` and `pitch`, at every tilt.
    ///
    /// Without this the step inside would also be a jump to somewhere else, and
    /// the only place that would show is a phone in somebody's hand.
    #[test]
    fn stepping_inside_keeps_looking_the_same_way() {
        for azimuth in [0.0_f32, 1.0, 2.5, -2.0] {
            for pitch in [-0.5_f32, 0.0, 0.3, 1.2] {
                // Third person: the eye is out along the boom and aims back at
                // the avatar, so its look direction is the boom negated.
                let boom = Vec3::new(
                    azimuth.sin() * pitch.cos(),
                    pitch.sin(),
                    azimuth.cos() * pitch.cos(),
                );
                let third = -boom.normalize();
                // First person: the rotation `camera_controls` builds with no
                // device attached.
                let first = Quat::from_euler(EulerRot::YXZ, azimuth, -pitch, 0.0) * Vec3::NEG_Z;
                assert!(
                    (first - third).length() < 1e-5,
                    "az {azimuth} pitch {pitch}: inside looks {first:?}, outside {third:?}"
                );
            }
        }
    }

    /// …and the same continuity from the other side: the angles the device path
    /// reads back out of its rotation rebuild that rotation's own look direction.
    /// This is what keeps a zoom-out from a phone-aimed view landing on the
    /// third-person shot of the same street.
    #[test]
    fn the_angles_read_back_from_a_device_rotation_aim_the_same_way() {
        for (bearing, elevation) in [(0.0, 0.0), (45.0, 20.0), (200.0, -35.0), (310.0, 70.0)] {
            let rot =
                crate::platform::orientation::DeviceAttitude::pointing(bearing, elevation, 0.0)
                    .camera_rotation();
            let forward = rot * Vec3::NEG_Z;
            // The read-back in `camera_controls`.
            let flat = Vec2::new(forward.x, forward.z);
            let azimuth = (-flat.x).atan2(-flat.y);
            let pitch = -forward.y.clamp(-1.0, 1.0).asin();
            let rebuilt = Quat::from_euler(EulerRot::YXZ, azimuth, -pitch, 0.0) * Vec3::NEG_Z;
            assert!(
                (rebuilt - forward).length() < 1e-5,
                "{bearing}°/{elevation}°: rebuilt {rebuilt:?} from {forward:?}"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::Avatar;
    use crate::systems::driver::DriverState;
    use crate::systems::session::{SessionState, SessionStatus};

    /// Just enough world to run [`follow_on_sign_in`]: the three resources it
    /// reads and the avatar entity that is half of `follow_available`.
    fn sign_in_app() -> App {
        let mut app = App::new();
        app.init_resource::<CameraState>()
            .init_resource::<SessionState>()
            .init_resource::<DriverState>()
            .add_systems(Update, follow_on_sign_in);
        app.world_mut().spawn(Avatar);
        // Offline, and in the mode `camera_mode_toggle` drops an offline client
        // into — this is the state every sign-in actually starts from.
        app.world_mut().resource_mut::<CameraState>().mode = CameraMode::Free;
        app.update();
        app
    }

    fn go_online(app: &mut App) {
        app.world_mut().resource_mut::<SessionState>().status = SessionStatus::Online;
    }

    fn mode(app: &App) -> CameraMode {
        app.world().resource::<CameraState>().mode
    }

    #[test]
    fn a_sign_in_lands_in_follow() {
        let mut app = sign_in_app();
        assert_eq!(mode(&app), CameraMode::Free, "offline is the map viewer");
        go_online(&mut app);
        app.update();
        assert_eq!(mode(&app), CameraMode::Avatar);
    }

    /// The edge, not the level: having a session is not a reason to be dragged
    /// back to the avatar every frame.
    #[test]
    fn a_mode_picked_after_signing_in_is_left_alone() {
        let mut app = sign_in_app();
        go_online(&mut app);
        app.update();
        app.world_mut().resource_mut::<CameraState>().mode = CameraMode::Orbit;
        for _ in 0..3 {
            app.update();
        }
        assert_eq!(mode(&app), CameraMode::Orbit);
    }

    /// Driver mode drives the follow camera itself and restores the pose it
    /// borrowed on the way out; a sign-in must not write over that.
    #[test]
    fn driver_mode_keeps_the_camera_it_borrowed() {
        let mut app = sign_in_app();
        app.world_mut().resource_mut::<DriverState>().active = true;
        app.world_mut().resource_mut::<CameraState>().mode = CameraMode::Orbit;
        go_online(&mut app);
        app.update();
        assert_eq!(mode(&app), CameraMode::Orbit);
    }

    /// Signing out and back in is a sign-in, so it lands in Follow again.
    #[test]
    fn signing_back_in_lands_in_follow_again() {
        let mut app = sign_in_app();
        go_online(&mut app);
        app.update();
        app.world_mut().resource_mut::<SessionState>().status = SessionStatus::Offline;
        app.world_mut().resource_mut::<CameraState>().mode = CameraMode::Free;
        app.update();
        go_online(&mut app);
        app.update();
        assert_eq!(mode(&app), CameraMode::Avatar);
    }

    /// Bevy-local north for these tests: the anchor frame's +Y (see
    /// `GlobeState::north_axis_local`, which returns the real thing in-app).
    const NORTH: DVec3 = DVec3::Y;

    /// The deny entry matches the adapter string the device actually reports
    /// (see the `sky chosen` log lines in the note's evidence trail) and
    /// nothing wider — an 8-series sibling or a desktop GPU must not be
    /// swept in by a measurement made on one phone.
    #[test]
    fn the_driver_deny_list_matches_only_what_was_measured() {
        assert!(driver_denies_atmosphere("Adreno (TM) 840"));
        assert!(!driver_denies_atmosphere("Adreno (TM) 830"));
        assert!(!driver_denies_atmosphere("Adreno (TM) 750"));
        assert!(!driver_denies_atmosphere(
            "llvmpipe (LLVM 20.1.2, 256 bits)"
        ));
        assert!(!driver_denies_atmosphere("NVIDIA GeForce RTX 4070"));
    }

    /// Writing the *same* projection back must not mark the component changed.
    ///
    /// This is a change-detection test, so it needs a real `World`: the defect it
    /// guards was invisible to the value-level guard inside `set_projection`,
    /// because `&mut **projection` on a `Mut<T>` stamps the tick when the reference
    /// is *formed*, not when a field is assigned. `camera_controls` calls both
    /// setters unconditionally every frame, so the projection was permanently
    /// `Changed` — re-running frustum culling setup and the camera view-uniform
    /// extraction on frames where nothing had moved.
    #[test]
    fn rewriting_an_unchanged_projection_leaves_it_unchanged() {
        let mut world = World::new();
        let e = world
            .spawn(Projection::Perspective(PerspectiveProjection {
                near: DEFAULT_NEAR,
                far: DEFAULT_FAR,
                ..default()
            }))
            .id();
        // A freshly spawned component is always `Changed`; start from a clean slate
        // so what the test observes is this frame's writes only.
        world.clear_trackers();

        let mut projection = world.get_mut::<Projection>(e).expect("spawned above");
        set_projection(&mut projection, DEFAULT_NEAR, DEFAULT_FAR);
        set_fov(&mut projection, FOLLOW_FOV_BASE);
        drop(projection);
        assert!(
            !world
                .entity(e)
                .get_ref::<Projection>()
                .expect("spawned above")
                .is_changed(),
            "re-applying identical near/far (and an unchanged FOV) must not dirty              the projection -- camera_controls does exactly this on every still frame"
        );

        // …and a real change must still register, or the guard has gone too far and
        // the space tier's near/far override would never reach the renderer.
        let mut projection = world.get_mut::<Projection>(e).expect("spawned above");
        set_projection(&mut projection, DEFAULT_NEAR, DEFAULT_FAR * 4.0);
        drop(projection);
        assert!(
            world
                .entity(e)
                .get_ref::<Projection>()
                .expect("spawned above")
                .is_changed(),
            "a genuine near/far change must still mark the projection changed"
        );

        // The mechanism itself, pinned: forming `&mut **` on a `Mut<T>` marks the
        // component changed **even when nothing is assigned through it**. This is
        // the whole reason the setters above read through `Deref` first, and it is
        // what made the old value-guard ineffective. If a Bevy upgrade ever made
        // this false, the contortion in those setters could be dropped — and this
        // is the assertion that would say so.
        world.clear_trackers();
        let mut projection = world.get_mut::<Projection>(e).expect("spawned above");
        let _formed_but_unused = &mut *projection;
        drop(projection);
        assert!(
            world
                .entity(e)
                .get_ref::<Projection>()
                .expect("spawned above")
                .is_changed(),
            "forming a mutable deref of Mut<T> is what stamps the change tick; if \
             this no longer holds, set_projection/set_fov can go back to a plain \
             `&mut Projection`"
        );
    }

    #[test]
    fn the_ground_clamp_reads_altitude_across_cells_not_within_one() {
        // High up, whatever the cell-local y: nothing to clamp. A local y below the
        // ground height is perfectly normal seven cells (70 km) up — reading it as
        // an altitude is what pinned a descent at every 10 km boundary.
        assert_eq!(ground_lift(7.0, 200.0, 0.5), 0.0);
        assert_eq!(ground_lift(7.0, -4_000.0, 0.5), 0.0);
        assert_eq!(ground_lift(1.0, 0.0, 0.5), 0.0);
        // Only the real ground clamps, and by exactly the shortfall.
        assert_eq!(ground_lift(0.0, 0.2, 0.5), 0.3);
        assert!((ground_lift(-1.0, 9_999.0, 0.5) - 1.5).abs() < 1e-3);
    }

    #[test]
    fn fly_speed_rides_altitude_between_a_floor_and_a_ceiling() {
        // Down among the buildings the constant is taken literally — scaling it
        // *down* near the ground would make the last few metres of a descent
        // unusable.
        assert_eq!(fly_speed(FREE_CAMERA_SPEED, 0.0), FREE_CAMERA_SPEED);
        assert_eq!(fly_speed(FREE_CAMERA_SPEED, 5.0), FREE_CAMERA_SPEED);
        // Above the reference altitude it is proportional: ten times as high is
        // ten times as fast, so the ground slides past at a constant rate.
        assert!(
            (fly_speed(FREE_CAMERA_SPEED, FREE_SPEED_ALT_REF * 10.0) - FREE_CAMERA_SPEED * 10.0)
                .abs()
                < 1e-3
        );
        // …until the cap, which every orbital altitude shares — the 45,000 km
        // space orbit must not multiply the speed by six figures.
        let capped = FREE_CAMERA_SPEED * FREE_SPEED_MAX_FACTOR;
        assert_eq!(fly_speed(FREE_CAMERA_SPEED, 1.0e6), capped);
        assert_eq!(fly_speed(FREE_CAMERA_SPEED, 4.5e7), capped);
        // A negative altitude (below the surface, or the local-y fallback on the
        // far side before the globe is ready) falls on the floor, never negative.
        assert_eq!(fly_speed(FREE_CAMERA_SPEED, -500.0), FREE_CAMERA_SPEED);
    }

    fn latitude(dir: DVec3) -> f64 {
        dir.dot(NORTH).clamp(-1.0, 1.0).asin().to_degrees()
    }

    /// Heading 0 means north points up the screen, at any sub-camera latitude.
    #[test]
    fn globe_up_is_north_when_heading_is_zero() {
        for lat in [-80.0_f64, -30.0, 0.0, 45.0, 80.0] {
            let (s, c) = lat.to_radians().sin_cos();
            let dir = DVec3::new(c * 0.6, s, c * 0.8).normalize();
            let up = globe_up(dir, NORTH, 0.0, DVec3::Y);
            // Perpendicular to the view axis, and on north's side of it.
            assert!(up.dot(dir).abs() < 1e-9, "up must be perpendicular to dir");
            assert!(
                up.dot(NORTH) > 0.0,
                "north must be up, not down, at lat {lat}"
            );
        }
    }

    /// Over a pole north projects to nothing; the previous up carries over
    /// instead of the view snapping to some arbitrary axis.
    #[test]
    fn globe_up_falls_back_over_the_pole() {
        let up = globe_up(DVec3::Y, NORTH, 0.0, DVec3::Z);
        assert!(up.dot(DVec3::Y).abs() < 1e-9);
        assert!(up.dot(DVec3::Z) > 0.99);
    }

    /// `globe_heading` inverts `globe_up`, so entering the space tier can carry
    /// the current view over without a jump.
    #[test]
    fn globe_heading_inverts_globe_up() {
        let dir = DVec3::new(0.4, 0.5, 0.76).normalize();
        for heading in [-2.0_f32, -0.3, 0.0, 0.7, 2.5] {
            let up = globe_up(dir, NORTH, heading, DVec3::Y);
            let back = globe_heading(dir, NORTH, up);
            assert!(
                (back - heading).abs() < 1e-4,
                "heading {heading} round-tripped to {back}"
            );
        }
    }

    /// A drag spins the globe under the cursor: horizontal moves the sub-camera
    /// point sideways, vertical moves it in latitude — and the reverse drag
    /// puts it back (no drift).
    #[test]
    fn globe_spin_tracks_the_drag_and_is_reversible() {
        let dir = DVec3::new(0.0, 0.0, 1.0);
        let up = globe_up(dir, NORTH, 0.0, DVec3::Y);

        // Drag down (screen y grows downward) → the map is pulled down and the
        // camera walks north, exactly as grabbing the globe should feel.
        let down = globe_spin(dir, NORTH, up, Vec2::new(0.0, 20.0), 0.01);
        assert!(
            latitude(down) > 0.5,
            "drag down should walk the sub-point north"
        );

        // There and back again.
        let there = globe_spin(dir, NORTH, up, Vec2::new(15.0, 10.0), 0.01);
        let up2 = globe_up(there, NORTH, 0.0, up);
        let back = globe_spin(there, NORTH, up2, Vec2::new(-15.0, -10.0), 0.01);
        assert!(
            back.dot(dir) > 0.9999,
            "a drag and its inverse should return to the same point"
        );
    }

    /// The sub-camera point stops short of the pole however hard it is dragged,
    /// so the view never swings through the point where north-up is undefined.
    #[test]
    fn globe_spin_clamps_at_the_poles() {
        let mut dir = DVec3::new(0.0, 0.0, 1.0);
        for _ in 0..50 {
            let up = globe_up(dir, NORTH, 0.0, DVec3::Y);
            dir = globe_spin(dir, NORTH, up, Vec2::new(0.0, 100.0), 0.01);
            assert!(
                latitude(dir) <= GLOBE_LAT_LIMIT.to_degrees() + 1e-6,
                "latitude {} passed the limit",
                latitude(dir)
            );
        }
        // …and it actually got there, rather than stalling early.
        assert!(latitude(dir) > GLOBE_LAT_LIMIT.to_degrees() - 1.0);
    }

    /// Compass locked: a horizontal drag is a pure longitude spin — the
    /// sub-camera latitude never changes, so the view circles the pole but is
    /// never carried over or around it out of the north-up frame.
    #[test]
    fn globe_spin_polar_horizontal_holds_latitude() {
        let mut dir = DVec3::new(0.3, 0.6, 0.74).normalize();
        let lat0 = latitude(dir);
        for _ in 0..90 {
            dir = globe_spin_polar(dir, NORTH, Vec2::new(12.0, 0.0), 0.02);
            assert!(
                (latitude(dir) - lat0).abs() < 1e-6,
                "horizontal drag drifted latitude {} off {lat0}",
                latitude(dir)
            );
        }
    }

    /// Vertical drag still walks the meridian, and still stops short of the pole.
    #[test]
    fn globe_spin_polar_clamps_at_the_poles() {
        let mut dir = DVec3::new(0.0, 0.0, 1.0);
        for _ in 0..50 {
            dir = globe_spin_polar(dir, NORTH, Vec2::new(0.0, 100.0), 0.01);
            assert!(
                latitude(dir) <= GLOBE_LAT_LIMIT.to_degrees() + 1e-6,
                "latitude {} passed the limit",
                latitude(dir)
            );
        }
        assert!(latitude(dir) > GLOBE_LAT_LIMIT.to_degrees() - 1.0);
    }

    /// Repeated round-trip drags must not accumulate roll: this is the whole
    /// point of deriving up from north instead of rotating a stored vector.
    #[test]
    fn globe_view_does_not_roll_off_north() {
        let mut dir = DVec3::new(0.2, 0.3, 0.93).normalize();
        for i in 0..200 {
            let up = globe_up(dir, NORTH, 0.0, DVec3::Y);
            let drag = if i % 2 == 0 {
                Vec2::new(9.0, 7.0)
            } else {
                Vec2::new(-9.0, -7.0)
            };
            dir = globe_spin(dir, NORTH, up, drag, 0.01);
        }
        let up = globe_up(dir, NORTH, 0.0, DVec3::Y);
        let north_up = (NORTH - dir * NORTH.dot(dir)).normalize();
        assert!(
            up.dot(north_up) > 0.999999,
            "view rolled off north after repeated drags"
        );
    }

    /// The horizon shell is the ground's fog wall continued past the last tile,
    /// so its opacity has to *be* the fog's at that range. The hand-tuned peak it
    /// replaced (`0.35 * alt + 1.2 * density`) only reached 1.0 at density 0.54,
    /// while the ground is saturated fog colour from ~10 km out at any density —
    /// so every ordinary flight drew a step where the map ended.
    #[test]
    fn horizon_haze_matches_the_fog_wall_it_continues() {
        for density in [0.05_f32, 0.15, 0.3, 0.6, 1.0] {
            let fog = distance_fog(density, 12.0, 0.0);
            let peak = fog_factor_at(&fog.falloff, HORIZON_FOG_DISTANCE);
            assert!(
                peak > 0.999,
                "density {density}: horizon shell at {peak}, fog wall at 1.0"
            );
        }
        // Fog off is the one case that must not paint: no wall to continue, and
        // a shell that veils the horizon anyway is what the old formula did.
        let clear = distance_fog(0.0, 12.0, 0.0);
        assert_eq!(fog_factor_at(&clear.falloff, HORIZON_FOG_DISTANCE), 0.0);
    }

    /// The band between `GLOBE_DRAW_ALT` and `GLOBE_SHOW_ALT` is the one place
    /// the far plane is not simply "the flat tier's 200 km", because down there
    /// the world now ends at the planet's horizon rather than at a skirt.
    #[test]
    fn the_far_plane_reaches_the_horizon_across_the_whole_band() {
        const R: f32 = 6_371_000.0;
        // Known horizons, so a factor-of-two slip in the formula cannot pass.
        assert!((horizon_distance_m(2_000.0, R) - 159_600.0).abs() < 1_000.0);
        assert!((horizon_distance_m(12_000.0, R) - 391_000.0).abs() < 2_000.0);
        assert!((horizon_distance_m(50_000.0, R) - 800_000.0).abs() < 5_000.0);
        assert_eq!(horizon_distance_m(0.0, R), 0.0);
        assert_eq!(horizon_distance_m(-10.0, R), 0.0);

        // And the far plane the band actually sets clears every one of them.
        // The old flat-tier default does not, which is the bug: at 12 km it cut
        // the planet off 190 km short of the skyline.
        for alt in [
            crate::systems::globe::GLOBE_DRAW_ALT,
            12_000.0,
            crate::systems::globe::GLOBE_SHOW_ALT,
        ] {
            let horizon = horizon_distance_m(alt, R);
            let far = band_far_plane_m(alt, R);
            assert!(far > horizon, "{alt} m: far {far} < horizon {horizon}");
        }
        assert!(horizon_distance_m(12_000.0, R) > DEFAULT_FAR);
    }

    /// The skirt is the ground past the last tile, so it may not rise through
    /// the tiles on its way out. Now that they follow the Earth down, a flat
    /// disc would be above them everywhere past a couple of kilometres.
    #[test]
    fn the_ground_skirt_follows_the_curve_it_stands_in_for() {
        let mesh = ground_skirt_mesh();
        let positions = match mesh.attribute(Mesh::ATTRIBUTE_POSITION) {
            Some(bevy::render::mesh::VertexAttributeValues::Float32x3(p)) => p.clone(),
            _ => panic!("the skirt lost its positions"),
        };
        let mut rim_drop = 0.0_f32;
        for [x, y, z] in positions {
            assert!(y.is_finite() && x.is_finite() && z.is_finite());
            let r = Vec2::new(x, z).length();
            let want = crate::systems::globe::curvature_drop_m(r, SKIRT_EARTH_RADIUS);
            assert!((-y - want).abs() < 0.5, "{r} m out: {} vs {want}", -y);
            rim_drop = rim_drop.max(-y);
        }
        // It is a cap, not a disc: the rim really is well over a kilometre down.
        assert!(
            rim_drop > 1_500.0,
            "the skirt only fell {rim_drop} m over {GROUND_DISC_RADIUS} m"
        );
        // …and the sink clears the latitude spread between this nominal radius
        // and the anchor's real one (see `GROUND_SKIRT_SINK`).
        let spread = rim_drop * 0.004;
        assert!(
            GROUND_SKIRT_SINK > spread,
            "{GROUND_SKIRT_SINK} vs {spread}"
        );
    }

    /// The opening view: Earth mode, looking down at the map anchor from about a
    /// kilometre and a half. The altitude is the number that matters — the boom
    /// length on its own says nothing, since it is measured along a tilted line —
    /// and it has to land in the band where the streamer has detail to show and
    /// the whole centre is still in frame.
    #[test]
    fn the_app_opens_over_the_anchor_at_map_altitude() {
        let mut cs = CameraState::default();
        seed_start_view(&mut cs);
        assert!(matches!(cs.mode, CameraMode::Orbit));
        assert_eq!(
            cs.orbit_focus,
            DVec3::ZERO,
            "the anchor is the world origin"
        );
        // `orbit_initialized` is what stops `camera_controls` re-deriving this
        // pose from the spawn transform on the first frame and throwing it away.
        assert!(cs.orbit_initialized && cs.orbit_has_pose);
        let altitude = START_ORBIT_DISTANCE * f64::from(START_ORBIT_PITCH.sin());
        assert!(
            (1_200.0..2_000.0).contains(&altitude),
            "opening altitude {altitude:.0} m is outside the city-in-frame band"
        );
        // Well below the space handoff, or the app would open on the globe.
        assert!(altitude < SPACE_ENTER_ALT);
    }

    /// A held zoom button is a rate, so the same hold must be the same journey at
    /// 20 fps and at 60 — and pressing "closer" must close the distance.
    #[test]
    fn a_held_zoom_is_frame_rate_independent() {
        let slow = osd_zoom_factor(1.0, 1.0 / 20.0).powi(20);
        let fast = osd_zoom_factor(1.0, 1.0 / 60.0).powi(60);
        assert!((slow - fast).abs() < 1e-4, "{slow} vs {fast}");
        assert!(slow < 1.0, "closer must shrink the distance");
        assert!(osd_zoom_factor(-1.0, 1.0 / 60.0) > 1.0);
        assert_eq!(osd_zoom_factor(0.0, 1.0 / 60.0), 1.0);
    }
}
