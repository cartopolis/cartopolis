use bevy::math::{DQuat, DVec3};
use bevy::light::atmosphere::ScatteringMedium;
use bevy::camera::Exposure;
use bevy::post_process::bloom::Bloom;
use bevy::light::{AmbientLight, Atmosphere, AtmosphereEnvironmentMapLight, NotShadowCaster};
use bevy::pbr::AtmosphereSettings;
use bevy::prelude::*;
use big_space::prelude::{BigSpaceCommands, FloatingOrigin, Grid, GridCell};

use crate::resources::{AvatarState, CameraState, CameraMode};
use crate::systems::egui_manager::EguiManager;
use crate::systems::gizmo::GizmoState;
use crate::systems::rendering::RegionMesh;

#[derive(Component)]
pub struct FreeCamera;

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
        FogFalloff::ExponentialSquared { density } => {
            depth.sqrt() / density.max(f32::MIN_POSITIVE)
        }
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

// ── Earth (orbit) mode tuning ───────────────────────────────────────────────
/// Closest the orbit camera may sit to its ground focus (metres).
const ORBIT_MIN_DISTANCE: f64 = 15.0;
/// Third-person follow-camera zoom range (metres from the ~1.8 m avatar). Min lets
/// you get close; max keeps the avatar readable rather than a distant speck.
const AVATAR_CAM_MIN: f32 = 1.5;
const AVATAR_CAM_MAX: f32 = 25.0;
/// Default follow distance — a comfortable third-person framing of the avatar.
pub const AVATAR_CAM_DEFAULT: f32 = 6.0;
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
    let rot = DQuat::from_axis_angle(up, -(drag.x * sens) as f64)
        * DQuat::from_axis_angle(right, tip);
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
fn cell_origin(cell: &GridCell) -> DVec3 {
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
fn set_projection(projection: &mut Projection, near: f32, far: f32) {
    if let Projection::Perspective(p) = projection {
        if p.near != near || p.far != far {
            p.near = near;
            p.far = far;
        }
    }
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

/// Earth inner radius used by `Atmosphere::earth` (metres). The planet-centre
/// entity sits this far below the local ground so the atmosphere's surface aligns
/// with y = 0. Must match bevy_pbr's `Atmosphere::earth` constant.
const EARTH_ATMOSPHERE_INNER_RADIUS: f32 = 6_360_000.0;

/// Grid cell edge for the world `BigSpace` (ADR-019/021). Large enough that local
/// play rarely recenters; small enough to keep within-cell f32 precise.
pub const WORLD_CELL_EDGE: f32 = 10_000.0;
/// Hysteresis buffer past a cell edge before big_space recenters an entity.
pub const WORLD_SWITCH_THRESHOLD: f32 = 100.0;

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
) {
    camera_state.mode = CameraMode::Avatar;
    camera_state.distance = AVATAR_CAM_DEFAULT;
    camera_state.azimuth = 0.0;
    camera_state.pitch = std::f32::consts::PI / 6.0;

    // Earth-preset scattering medium for the built-in atmosphere (Bevy 0.19). Its
    // density is rewritten by altitude in `globe::update_globe_visibility` (thin
    // low down so the map reads, rising for the limb from orbit — see
    // `globe::ATMOSPHERE_DENSITY_NEAR`), so it's handed to that system as a shared
    // handle. The planet spawns with it below.
    let medium = scattering_media.add(ScatteringMedium::default());
    commands.insert_resource(crate::systems::globe::AtmosphereMedia { space: medium.clone() });

    // Atmosphere and its env-map IBL are chosen once here, at startup: they can't
    // be safely toggled at runtime (removing the components frees in-flight GPU
    // resources and hangs the device — see `globe::update_globe_visibility`). The
    // dev-panel checkboxes seed these but only take effect on the next launch; the
    // env flags force them off. Env-map IBL requires the atmosphere it bakes from.
    // The atmosphere is compute-based (bevy_pbr) and needs a fragment storage
    // buffer. That ruled it out under WebGL2, which has neither — but the web build
    // targets WebGPU now (ADR-033), where both exist, so this is no longer
    // platform-gated at all.
    let want_atmosphere =
        dev.atmosphere_enabled && !crate::env_flag("VIBERFOX_NO_ATMOSPHERE");
    let want_envmap =
        want_atmosphere && dev.envmap_enabled && !crate::env_flag("VIBERFOX_NO_ENVMAP");

    commands.spawn_big_space(
        Grid::new(WORLD_CELL_EDGE, WORLD_SWITCH_THRESHOLD),
        |root| {
            // The root must carry Visibility: renderable children inherit it, and a
            // missing parent Visibility triggers Bevy's B0004 warning per child.
            root.insert(Visibility::default());
            let mut cam = root.spawn_spatial((
                Camera3d::default(),
                // 2× MSAA instead of Bevy's implicit 4× default: roughly halves the
                // main-pass multisample cost for a marginal edge-quality change at
                // this scene's geometric density.
                //
                // Except on the web, where 2× is not a tuning choice but an invalid
                // one: WebGPU permits a sample count of only 1 or 4, so requesting 2
                // fails to create `main_texture_sampled` — and the failure is silent
                // in the worst way. Nothing renders (black canvas) and every queued
                // command is then dropped un-applied, so the console fills with
                // `CommandQueue has un-applied commands` warnings that describe the
                // symptom and never the cause. 4× is therefore the only anti-aliased
                // option on WebGPU; `Msaa::Off` is the cheaper one if the extra
                // resolve shows up in frame times.
                // ...and on the web that leaves no cheap anti-aliased option at all,
                // so take none. WebGPU's jump from 2× to 4× doubles the samples
                // rasterised for every triangle in a scene that is already hundreds
                // of tiles plus a thousand-odd buildings, and the browser build has
                // no frame budget to spend on edge quality yet. Raise this to
                // `Msaa::Sample4` once frame times allow — those are the only two
                // legal values here.
                if cfg!(target_arch = "wasm32") {
                    Msaa::Off
                } else {
                    Msaa::Sample2
                },
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
                Exposure { ev100: ATMOSPHERE_EXPOSURE_EV100 },
                // Bloom: the glare that makes an HDR scene read as bright. The
                // sun disk is the case that needs it — a physically-sized disc
                // is a few pixels across, and without glare around it the sky's
                // brightest object looks like a sticker. It also gives the night
                // city's window lights and the sunlit limb from orbit their
                // halo. Kept low (Bevy's default 0.15 is tuned for emissive-heavy
                // scenes) so the daytime basemap doesn't smear.
                Bloom {
                    intensity: GROUND_BLOOM_INTENSITY,
                    ..Bloom::NATURAL
                },
                AmbientLight::default(),
                FreeCamera,
                FloatingOrigin,
            ));
            // Atmospheric distance fog: distant buildings fade into haze, sun-side
            // glow, driven by the dev panel (apply_distance_fog). It is one more
            // fragment uniform in the mesh view, which overflowed WebGL2's cap of 14
            // — reached there because clustering fell back to uniforms. On WebGPU
            // clustering uses storage buffers, so the budget is no longer contested.
            cam.insert(distance_fog(dev.fog_density, dev.time_of_day, dev.sun_declination_rad));
            // Bevy 0.19 built-in atmosphere: the camera carries only the render
            // settings; the sky derives from the sun direction + camera altitude
            // over the planet entity below. `update_globe_visibility` no-ops
            // without it, which is what made dropping it a safe web workaround —
            // and what makes restoring it here uneventful.
            cam.insert(AtmosphereSettings::default());
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

            // A ground disc out to the horizon, in the OSM basemap's paper tone.
            // Without it, the world ends at the loaded tiles and the void below
            // the horizon reads as a black gulf; with it, DistanceFog fades the
            // ground into the haze so fog and sky meet seamlessly at any altitude.
            root.spawn_spatial((
                Mesh3d(meshes.add(Circle::new(150_000.0))),
                MeshMaterial3d(materials.add(StandardMaterial {
                    base_color: Color::srgb(0.83, 0.83, 0.80),
                    perceptual_roughness: 1.0,
                    reflectance: 0.0,
                    ..default()
                })),
                // Well below the whole tile pyramid (whose coarsest rings sit a few
                // metres down — see update_map_stream) so it never covers a tile.
                Transform::from_xyz(0.0, -5.0, 0.0)
                    .with_rotation(Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2)),
                NotShadowCaster,
                // From orbit this 150 km disc reads as a bright circle over the
                // anchor; hide it (and the tiles/regions) while the globe is up.
                crate::systems::globe::HideInSpace,
            ));
        },
    );

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
            crate::systems::globe::PlanetAtmosphere,
            Atmosphere::earth(medium),
            Transform::from_xyz(0.0, -EARTH_ATMOSPHERE_INNER_RADIUS, 0.0),
        ));
    }
}

/// Arm the per-mode entry behaviour when switching *into* `next`. Called from both
/// the Tab cycle and the Camera menu so the two paths stay identical:
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

pub fn camera_mode_toggle(
    keyboard_input: Res<ButtonInput<KeyCode>>,
    mut camera_state: ResMut<CameraState>,
    avatars: Query<(), With<crate::components::Avatar>>,
) {
    // Follow (Avatar) is avatar-only — it exists only in the logged-in viewer that
    // has a local avatar. In anonymous mode there is none, so never offer or remain
    // in Follow; Fly and Earth stay available.
    let has_avatar = !avatars.is_empty();
    if !has_avatar && camera_state.mode == CameraMode::Avatar {
        camera_state.mode = CameraMode::Free;
    }
    if !keyboard_input.just_pressed(KeyCode::Tab) {
        return;
    }
    // Cycle Follow → Fly → Earth → Follow, skipping Follow when there's no avatar.
    let next = match camera_state.mode {
        CameraMode::Avatar => CameraMode::Free,
        CameraMode::Free => CameraMode::Orbit,
        CameraMode::Orbit if has_avatar => CameraMode::Avatar,
        CameraMode::Orbit => CameraMode::Free,
    };
    enter_mode(&mut camera_state, next);
    camera_state.mode = next;
    println!("Camera mode: {:?}", camera_state.mode);
}

pub fn camera_controls(
    keyboard_input: Res<ButtonInput<KeyCode>>,
    mouse_input: Res<ButtonInput<MouseButton>>,
    mut cursor_moved_events: MessageReader<CursorMoved>,
    mut mouse_wheel_events: MessageReader<bevy::input::mouse::MouseWheel>,
    mut camera_query: Query<(&mut GridCell, &mut Transform, &mut Projection), With<FreeCamera>>,
    mut camera_state: ResMut<CameraState>,
    avatar_state: Res<AvatarState>,
    time: Res<Time>,
    region_mesh_query: Query<&GlobalTransform, With<RegionMesh>>,
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
    avatar_cells: Query<(&GridCell, &Transform), (With<crate::components::Avatar>, Without<FreeCamera>)>,
    terrain: Res<crate::systems::terrain::TerrainField>,
) {
    if camera_query.is_empty() {
        return;
    }

    let Ok((mut camera_cell_mut, mut camera_transform_mut, mut projection)) =
        camera_query.single_mut()
    else {
        return;
    };
    // Work on copies of the camera's Transform + GridCell and write them back at the
    // end only if they actually changed. The camera is the big_space FloatingOrigin:
    // any change to its Transform/GridCell forces big_space to recompute EVERY
    // entity's GlobalTransform that frame (O(scene), see grid/propagation.rs). The
    // follow-lerp / look_at / orbit placement below rewrote these every frame even
    // when the view was perfectly still, which pinned that whole-scene propagation on
    // permanently and is the dominant CPU cost of a stationary frame.
    let prev_transform = *camera_transform_mut;
    let prev_cell = *camera_cell_mut;
    let mut camera_transform = prev_transform;
    let mut camera_cell = prev_cell;
    let delta_time = time.delta().as_secs_f32();

    // Keep the near/far at their ground defaults unless the space tier overrides
    // them below (Avatar/Free never touch the projection otherwise).
    set_projection(&mut projection, DEFAULT_NEAR, DEFAULT_FAR);

    match camera_state.mode {
        CameraMode::Avatar => {
            // Third-person camera following avatar (smoothed when online — see `AvatarState::display_position`)
            let avatar_pos = avatar_state.display_position;

            // Driver mode owns the rig (`systems::driver`): distance, tilt and
            // azimuth are written from the map orientation every frame, so the
            // wheel, the drag *and* the third-person zoom clamp are all skipped —
            // the clamp tops out at 25 m, a quarter of the chase distance a
            // navigation view needs, and would drag it back in every frame.
            let locked = camera_state.drive_locked;
            if !locked {
                // Proportional wheel zoom by notch *sign* — robust to line- vs
                // pixel-delta scrolling (raw `event.y` is ±1 on some backends, ±120 on
                // others; scaling it directly made one notch jump tens of metres). Sane
                // range for a ~1.8 m avatar; scroll in/out from the default.
                for event in mouse_wheel_events.read() {
                    if event.y != 0.0 {
                        let factor = if event.y > 0.0 { 1.0 / 1.15 } else { 1.15 };
                        camera_state.distance *= factor;
                    }
                }
                camera_state.distance =
                    camera_state.distance.clamp(AVATAR_CAM_MIN, AVATAR_CAM_MAX);
            }

            // Handle mouse drag for rotation (suppressed while egui, gizmo, or Ctrl has the pointer).
            let ctrl = keyboard_input.pressed(KeyCode::ControlLeft)
                || keyboard_input.pressed(KeyCode::ControlRight);
            if locked
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
                            // Manually orbiting drops follow-and-rotate — the same as
                            // Organic Maps stopping heading-follow the moment you drag
                            // the map yourself.
                            camera_state.follow_rotate = false;
                        }
                        camera_state.azimuth -= delta.x * 0.005;
                        camera_state.pitch -= delta.y * 0.005;
                        camera_state.pitch = camera_state.pitch
                            .max(-std::f32::consts::PI / 3.0)
                            .min(std::f32::consts::PI / 2.5);
                    }
                    camera_state.pan_offset = Some(event.position);
                }
            } else if mouse_input.just_released(MouseButton::Left) {
                camera_state.pan_offset = None;
            }

            // Follow-and-rotate (GPS button state 3): glue the camera behind the
            // avatar's heading so it swings around as the avatar turns. The avatar's
            // render yaw `face` is authoritative in every run mode (the movement
            // systems write `Quat::from_rotation_y(face)` each frame), so read it off
            // the avatar transform rather than the mode-specific yaw fields. The camera
            // sits directly behind when `azimuth ≡ face + π` (its look direction then
            // equals the avatar's facing). Ease along the shortest arc so a turn glides.
            if camera_state.follow_rotate {
                if let Ok((_, avatar_tf)) = avatar_cells.single() {
                    let face = avatar_tf.rotation.to_euler(EulerRot::YXZ).0;
                    let target = face + std::f32::consts::PI;
                    let mut d = (target - camera_state.azimuth).rem_euclid(std::f32::consts::TAU);
                    if d > std::f32::consts::PI {
                        d -= std::f32::consts::TAU;
                    }
                    let a = 1.0 - (-8.0_f32 * delta_time).exp();
                    // Snap the last sliver so azimuth (and thus the whole follow rig)
                    // actually settles when standing still, instead of creeping toward
                    // the target forever and pinning the floating-origin propagation.
                    camera_state.azimuth += if d.abs() < 1e-3 { d } else { d * a };
                }
            }

            // Calculate camera position
            let horizontal_distance = camera_state.distance * camera_state.pitch.cos();
            let vertical_offset = camera_state.distance * camera_state.pitch.sin();

            let camera_offset = Vec3::new(
                camera_state.azimuth.sin() * horizontal_distance,
                vertical_offset + 1.5,
                camera_state.azimuth.cos() * horizontal_distance,
            );

            let mut target_position = avatar_pos + camera_offset;

            // Prevent camera from going below ground
            let ground_height = get_ground_height(target_position, &region_mesh_query);
            let min_height = ground_height + MIN_CAMERA_HEIGHT;
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
            let new_world = if camera_state.avatar_initialized {
                // Framerate-independent follow smoothing, in world space.
                let cam_alpha = 1.0 - (-12.0_f32 * delta_time).exp();
                let w = cam_world.lerp(target_world, cam_alpha as f64);
                // Snap once essentially settled, else the floating origin (and the
                // whole-scene propagation) keeps changing by a hair while standing.
                if w.distance_squared(target_world) < 1e-6 { target_world } else { w }
            } else {
                // Entering Follow: snap straight onto the avatar (the reposition).
                camera_state.avatar_initialized = true;
                target_world
            };
            let grid = Grid::new(WORLD_CELL_EDGE, WORLD_SWITCH_THRESHOLD);
            let (new_cell, local) = grid.translation_to_grid(new_world);
            camera_cell = new_cell;
            camera_transform.translation = local;
            // Driver mode aims down the road rather than at the vehicle, which is
            // what puts the arrow in the lower third of the frame instead of dead
            // centre (`CameraState::drive_lookahead`). The camera itself stays where
            // the rig put it — only the look target moves.
            let ahead = if camera_state.drive_locked {
                crate::systems::avatar::camera_plane_forward(camera_state.azimuth)
                    * camera_state.drive_lookahead
            } else {
                Vec3::ZERO
            };
            let look_world = avatar_origin + (avatar_pos + Vec3::Y * 1.5 + ahead).as_dvec3();
            camera_transform.look_at((look_world - cell_origin(&new_cell)).as_vec3(), Vec3::Y);
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

            // Update rotation from mouse input
            if rotation_delta.length() > 0.0 {
                camera_state.free_camera_rotation.y -= rotation_delta.x * MOUSE_SENSITIVITY;
                camera_state.free_camera_rotation.x -= rotation_delta.y * MOUSE_SENSITIVITY;
                camera_state.free_camera_rotation.x = camera_state.free_camera_rotation.x
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
            let forward_horizontal = Vec3::new(forward.x, 0.0, forward.z).normalize();
            let right_horizontal = Vec3::new(right.x, 0.0, right.z).normalize();

            let base_speed = if keyboard_input.pressed(KeyCode::ShiftLeft) || keyboard_input.pressed(KeyCode::ShiftRight) {
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

            // The keys steer a velocity, not the translation: it chases the wish on
            // an exponential approach (frame-rate independent), so a release glides
            // to a stop instead of the camera stopping dead mid-flight. Braking is
            // deliberately slower than accelerating — the glide is the point, and a
            // symmetric curve makes the start feel mushy to get it.
            let wish = if move_direction.length() > 0.0 {
                move_direction.normalize() * speed
            } else {
                Vec3::ZERO
            };
            let tau = if wish == Vec3::ZERO { FREE_DECEL_TAU } else { FREE_ACCEL_TAU };
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
            let ground_height = get_ground_height(camera_transform.translation, &region_mesh_query)
                + terrain.at(
                    camera_cell.x as f32 * WORLD_CELL_EDGE + camera_transform.translation.x,
                    camera_cell.z as f32 * WORLD_CELL_EDGE + camera_transform.translation.z,
                );
            let min_height = ground_height + MIN_CAMERA_HEIGHT;
            let lift = ground_lift(camera_cell.y as f64, camera_transform.translation.y, min_height);
            if lift > 0.0 {
                camera_transform.translation.y += lift;
                // Kill the downward glide at the floor, or a coast into the ground
                // keeps pressing against the clamp long after the key is released.
                camera_state.free_velocity.y = camera_state.free_velocity.y.max(0.0);
            }

            // Remember this pose so re-entering Fly returns here.
            camera_state.free_saved =
                Some((camera_cell, camera_transform.translation, camera_state.free_camera_rotation));
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
                let dist = offset.length().clamp(ORBIT_MIN_DISTANCE, ORBIT_MAX_DISTANCE_SURFACE);
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
                        camera_state.reanchor_request =
                            Some((slat, slng, alt.max(SPACE_EXIT_ALT)));
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
            if gizmo_state.active_axis.is_some() || egui_manager.ctx.egui_wants_pointer_input() {
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
            // In space the factor applies to the *altitude*, not the distance
            // from the Earth's centre — otherwise one notch near the surface is
            // 10% of ~6,400 km (a violent 640 km plunge) instead of 10% of the
            // height above the ground.
            for event in mouse_wheel_events.read() {
                let factor = (1.0 - event.y * ORBIT_ZOOM_SPEED).clamp(0.2, 5.0) as f64;
                camera_state.orbit_distance_target = if space {
                    let alt = (camera_state.orbit_distance_target - radius).max(500.0);
                    (radius + alt * factor).clamp(min_d, max_d)
                } else {
                    (camera_state.orbit_distance_target * factor).clamp(min_d, max_d)
                };
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
                    let far_arc =
                        2.4 * ((radius / d).clamp(0.0, 1.0) as f32).asin();
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
                let shift =
                    right * (-pan_delta.x * scale) + fwd_flat * (pan_delta.y * scale);
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

fn get_ground_height(position: Vec3, region_mesh_query: &Query<&GlobalTransform, With<RegionMesh>>) -> f32 {
    let mut min_height: f32 = 0.0;
    for region_transform in region_mesh_query.iter() {
        let region_y: f32 = region_transform.translation().y;
        let region_top: f32 = region_y + 0.05;
        if position.x >= region_transform.translation().x - 150.0
            && position.x <= region_transform.translation().x + 150.0
            && position.z >= region_transform.translation().z - 150.0
            && position.z <= region_transform.translation().z + 150.0 {
            min_height = min_height.max(region_top);
        }
    }
    min_height
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Bevy-local north for these tests: the anchor frame's +Y (see
    /// `GlobeState::north_axis_local`, which returns the real thing in-app).
    const NORTH: DVec3 = DVec3::Y;

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
            (fly_speed(FREE_CAMERA_SPEED, FREE_SPEED_ALT_REF * 10.0)
                - FREE_CAMERA_SPEED * 10.0)
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
            assert!(up.dot(NORTH) > 0.0, "north must be up, not down, at lat {lat}");
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
        assert!(latitude(down) > 0.5, "drag down should walk the sub-point north");

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
}
