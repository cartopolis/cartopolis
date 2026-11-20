//! What the camera sees of the air: fog by distance and altitude, the horizon's
//! colour and distance, bloom from the ground to space. Pure functions over
//! the sun and the camera's height, used by the sky and the globe as much as by
//! the camera, so they sit with the map rather than in `free_camera`.

use bevy::prelude::*;

/// The horizon the clear sky shows, by sun elevation (degrees): linear RGB
/// scene luminance just above the horizon, facing away from the sun and toward
/// it. Measured off bevy's atmosphere with no fog, no cloud and no tone curve,
/// the exposure divided back out (mac-2, Groningen from 40 m, 17 October,
/// 2026-11-07, `docs/notes/exposure-and-adaptation.md` has how).
///
/// The fog takes its colour from this, so a fogged distance meets the sky
/// behind it in the sky's own colour. It used to be a hand-tuned day grey, a
/// night blue and a gold cast, which drew a band of its own at the horizon:
/// grey at noon, salmon into a low sun and neon pink after sunset, where the
/// atmosphere on its own shows pale blue, gold and, away from the sun, rose
/// over the Earth's shadow.
pub(crate) const HORIZON_SKY: [(f32, [f32; 3], [f32; 3]); 21] = [
    (-41.7, [6.8, 11.6, 17.4], [6.7, 11.6, 17.5]),
    (-26.4, [4.8, 7.8, 10.9], [4.6, 7.7, 10.9]),
    (-17.3, [3.4, 5.3, 6.8], [3.3, 5.1, 6.7]),
    (-12.7, [2.7, 3.9, 4.8], [2.6, 3.8, 4.6]),
    (-10.4, [2.4, 3.3, 3.8], [3.9, 3.4, 3.7]),
    (-8.1, [2.9, 3.3, 3.4], [33.0, 9.5, 4.3]),
    (-6.3, [7.0, 5.9, 5.4], [142.0, 33.0, 8.5]),
    (-4.5, [21.0, 17.5, 18.0], [443.0, 112.0, 40.0]),
    (-3.1, [53.0, 43.0, 43.0], [846.0, 221.0, 72.0]),
    (-1.8, [93.0, 71.0, 67.0], [1510.0, 481.0, 184.0]),
    (-0.4, [473.0, 215.0, 227.0], [2501.0, 948.0, 407.0]),
    (0.9, [1442.0, 550.0, 439.0], [3756.0, 1840.0, 893.0]),
    (3.1, [3105.0, 1595.0, 938.0], [6462.0, 4486.0, 2778.0]),
    (5.2, [4485.0, 3071.0, 1770.0], [8118.0, 6438.0, 4167.0]),
    (7.3, [5553.0, 4506.0, 2777.0], [11072.0, 9390.0, 6571.0]),
    (9.3, [6347.0, 5750.0, 3850.0], [12931.0, 11658.0, 8691.0]),
    (13.2, [7476.0, 7653.0, 5855.0], [14073.0, 13727.0, 10948.0]),
    (16.7, [8156.0, 8943.0, 7467.0], [13532.0, 14045.0, 11812.0]),
    (22.5, [8731.0, 10303.0, 9446.0], [10514.0, 12086.0, 10892.0]),
    (27.7, [8069.0, 10036.0, 9961.0], [8432.0, 10535.0, 10415.0]),
    (60.0, [8069.0, 10036.0, 9961.0], [8432.0, 10535.0, 10415.0]),
];

/// [`HORIZON_SKY`] at this hour, interpolated in log space: (away from the
/// sun, toward it).
pub(crate) fn horizon_sky(hours: f32, decl_rad: f32) -> (Vec3, Vec3) {
    let el = crate::utils::sun::sun_direction(hours, decl_rad)
        .y
        .clamp(-1.0, 1.0)
        .asin()
        .to_degrees();
    let v = |c: [f32; 3]| Vec3::from_array(c).max(Vec3::splat(1e-3)).ln();
    let (first, last) = (HORIZON_SKY[0], HORIZON_SKY[HORIZON_SKY.len() - 1]);
    let (anti, sun) = if el <= first.0 {
        (v(first.1), v(first.2))
    } else if el >= last.0 {
        (v(last.1), v(last.2))
    } else {
        let i = HORIZON_SKY
            .iter()
            .position(|&(e, _, _)| e > el)
            .unwrap_or(HORIZON_SKY.len() - 1);
        let (e0, a0, s0) = HORIZON_SKY[i - 1];
        let (e1, a1, s1) = HORIZON_SKY[i];
        let t = (el - e0) / (e1 - e0);
        (v(a0).lerp(v(a1), t), v(s0).lerp(v(s1), t))
    };
    (anti.exp(), sun.exp())
}

/// What brings [`horizon_sky`] into the screen's units at this hour: the fog
/// is mixed in after exposure, so the sky's scene luminance takes the same
/// exposure the camera has.
pub(crate) fn horizon_to_screen(hours: f32, decl_rad: f32) -> f32 {
    let ev = crate::systems::sky::DAY_EXPOSURE_EV100
        - crate::systems::sky::eye_adaptation_stops(hours, decl_rad);
    1.0 / (1.2 * 2f32.powf(ev))
}

/// How much brighter the sun-side glow peaks, looking straight at the sun,
/// than at the measured sample of [`HORIZON_SKY`], which looks some way off it.
pub(crate) const FOG_GLOW_GAIN: f32 = 3.0;

/// How fast [`cloud_sky_fill`]'s daylight colour fades once the sun is down: by
/// e for every this much of the sun's sine below the horizon, about 6°.
///
/// It was 0.026 (1.5°), when the sky after sunset was drawn 2–3 stops too dark
/// (the atmosphere's sun dimmed twice, `sky::sun_light_in_atmosphere`). With the
/// sky lit right, the fast fade left the cloud fill far under the sky: at 17.6 h
/// in October the clouds were black on a lavender sky. At 6° they come out a
/// blue-grey a little brighter than the sky beside them, as in a photograph of
/// a sunset over Groningen (mac-2, 2026-11-07).
pub(crate) const DUSK_FADE_Y: f32 = 0.105;

/// The sky's light on the cloud deck's shaded side, in the screen's units: a
/// blue-white by day, a faint blue at night, a little gold near sunset.
///
/// The deck took this from the fog colour until 2026-11-07, when the fog began
/// to take the horizon's colour ([`HORIZON_SKY`]). The horizon away from the
/// sun goes dark over the Earth's shadow minutes after sunset, while the sky
/// over a deck stays lit, and with it as the fill every dusk cloud went brown.
pub fn cloud_sky_fill(hours: f32, decl_rad: f32) -> Vec3 {
    use crate::utils::sun;

    let day = sun::daylight(hours, decl_rad); // smooth through twilight, not a hard ramp
    let night_color = Vec3::new(0.015, 0.02, 0.035);
    let day_color = Vec3::new(0.71, 0.75, 0.83);
    let t = sun::sun_transmittance(hours, decl_rad);
    let tint = t / t.max_element().max(1e-4);
    // Written after exposure, so the daylight part rises by the eye adaptation,
    // up to its noon colour; the night colour is the night exposure's already.
    // Taken by the clock alone it fell to a sixteenth of the sky beside it at
    // sunset, and every cloud shadow went brown (2026-10-17).
    let lift = 2f32.powf(crate::systems::sky::eye_adaptation_stops(hours, decl_rad));
    let dusk = (sun::sun_direction(hours, decl_rad).y / DUSK_FADE_Y)
        .exp()
        .min(1.0);
    let linear = |c: Vec3| Color::srgb(c.x, c.y, c.z).to_linear().to_vec3();
    let (night_lin, day_lin) = (linear(night_color), linear(day_color));
    // A faint warm cast, the beam's colour taken to a low power so blue falls
    // more than green: gold, as a Cuyp evening is. Faint, because the shadowed
    // side of a cloud stays a cool blue-grey in the paintings and in
    // photographs; gold on a grey turned them olive (2026-10-18).
    let gold = Vec3::new(tint.x.powf(0.15), tint.y.powf(0.15), tint.z.powf(0.15));
    night_lin.lerp(day_lin, (day * lift).min(1.0) * dusk)
        * Vec3::ONE.lerp(gold, 0.3 * sun::horizon_proximity(hours, decl_rad))
}

/// Builds the camera's [`DistanceFog`] for a dev-panel density in 0.0–1.0 and a
/// time of day. The falloff is exponential-squared so the near field stays clear
/// and geometry vanishes into a fog "wall"; the colour is the clear sky's own
/// horizon ([`HORIZON_SKY`]), with the sky's brightening toward the sun as the
/// fog's sun-side glow.
pub fn distance_fog(density: f32, hours: f32, decl_rad: f32) -> DistanceFog {
    let (anti, toward) = horizon_sky(hours, decl_rad);
    let c = anti * horizon_to_screen(hours, decl_rad);
    // Bevy adds the glow as `pow(cos, exponent) × light colour × exposure ×
    // this`, and the light is the sun at its own illuminance: dividing that
    // back out leaves the measured excess of the sky toward the sun.
    let (_, sun_lux) = crate::systems::sky::sun_light_in_atmosphere(hours, decl_rad, 0.0);
    let gain = crate::env_value("S640_GLOW")
        .and_then(|v| v.parse::<f32>().ok())
        .unwrap_or(FOG_GLOW_GAIN);
    let glow = (toward - anti).max(Vec3::ZERO) * gain / sun_lux.max(1.0);
    let glow_alpha = if sun_lux > 1.0 { 1.0 } else { 0.0 };

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
        color: Color::linear_rgb(c.x, c.y, c.z),
        directional_light_color: Color::linear_rgba(glow.x, glow.y, glow.z, glow_alpha),
        directional_light_exponent: 30.0,
        falloff,
    }
}

/// Ground fog is full-strength up to this altitude (rooftop / low-flight haze).
pub(crate) const FOG_FADE_LOW: f32 = 800.0;
/// …and has thinned to nothing by this altitude, so an orbit/high-flight view
/// looks down on the map through clear air instead of a grey wall.
pub(crate) const FOG_FADE_HIGH: f32 = 5000.0;

/// How present the atmospheric ground fog should be at a given camera altitude:
/// 1.0 near the ground, ramping to 0.0 by [`FOG_FADE_HIGH`]. Real haze is densest
/// near the surface; this mirrors that so climbing out reveals the world rather
/// than drowning it in fog (see `apply_distance_fog` / `update_horizon_haze`).
pub fn fog_altitude_factor(altitude_m: f32) -> f32 {
    1.0 - crate::utils::math::smoothstep(FOG_FADE_LOW, FOG_FADE_HIGH, altitude_m)
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
pub(crate) const FOG_SATURATED: f32 = 0.995;

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

/// How far past the geometric horizon the far plane is pushed in the band where
/// the globe is drawn under the flat tier.
///
/// Not a small slack: the Earth's *silhouette* is drawn by the far edges of the
/// tiles that straddle the horizon, and at 12 km up those are z9 tiles ~70 km
/// across. At 1.2 the far plane landed a few kilometres short of them and the
/// skyline came out as a sawtooth — the tile grid, clipped one tile at a time.
/// 1.5 clears the largest tile the split rule can leave at the limb at any
/// altitude in the band.
pub(crate) const HORIZON_FAR_MARGIN: f32 = 1.5;

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

/// Bloom strength on the ground. Low (Bevy's 0.15 default is tuned for
/// emissive-heavy scenes) so the daytime basemap doesn't smear.
pub const GROUND_BLOOM_INTENSITY: f32 = 0.12;

/// …and in space, where the frame is mostly black and the only bright things are
/// the sun disk, the star field and a thin sunlit limb. `apply_day_night_cycle`
/// switches between the two at the tier boundary.
pub const SPACE_BLOOM_INTENSITY: f32 = 0.35;

/// Bloom strength at a given `space_factor` — 0 at the ground, 1 in deep space.
///
/// Whether there is any bloom at all is not decided here: it is the presence of
/// the `Bloom` component, which `render_features::apply_render_features` adds and
/// removes from `AppPrefs::bloom_on`. This used to answer 0.0 on a tier without
/// bloom, which made the preference unable to turn it on.
#[must_use]
pub fn bloom_intensity(space_factor: f32) -> f32 {
    GROUND_BLOOM_INTENSITY.lerp(SPACE_BLOOM_INTENSITY, space_factor)
}
