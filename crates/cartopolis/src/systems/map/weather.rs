//! Live weather from the MET Norway (Yr) location-forecast API.
//!
//! `api.met.no/weatherapi/locationforecast/2.0/complete` is a free, keyless,
//! **global** governmental source (the Norwegian Meteorological Institute; the
//! worldwide fields are ECMWF's HRES model interpolated to the point). It wants
//! two things and no money: a descriptive `User-Agent` identifying the app, and
//! coordinates truncated to ~4 decimals (finer just defeats their edge cache).
//!
//! We fetch for the current map anchor (`MapStream::geo_anchor`), re-fetching on
//! a re-anchor or every [`REFRESH`], and fold the sample onto the three knobs the
//! renderer already exposes — `DevPanelState::cloud_cover / fog_density /
//! precipitation`. It only writes those while [`WeatherState::live`] is on, so the
//! dev-panel sliders stay authoritative when the user is A/B'ing by hand.
//!
//! One async fetch spawned on Bevy's `IoTaskPool` (via `platform::http`), drained
//! on the main thread — the same fetch seam every other upstream now uses (ADR-033).
//! See [[../CLAUDE.md]] for the cloud/fog/rain plumbing this feeds.

use std::time::Duration;
use web_time::Instant;

use bevy::prelude::*;
use bevy::tasks::{IoTaskPool, Task};

use crate::platform::http;
use crate::resources::DevPanelState;
use crate::systems::map_stream::MapStream;

/// Re-fetch cadence. MET updates the global forecast ~4×/day and asks callers not
/// to poll faster than the data changes; 10 min is well within etiquette and keeps
/// the scene responsive after a re-anchor (which refetches immediately regardless).
const REFRESH: Duration = Duration::from_secs(600);

/// Hourly rainfall (mm) that maps to `precipitation = 1.0` (a heavy shower).
pub const RAIN_FULL_MM: f32 = 4.0;

/// Baseline atmospheric haze kept even on a clear, dry day so the sky doesn't go
/// glassy (matches the old fixed `fog_density` default of 0.15's low end).
const HAZE_FLOOR: f32 = 0.05;

/// One weather sample, already normalised to the scene's 0..1 knobs.
#[derive(Clone, Copy, Debug)]
pub struct Observation {
    pub cloud_cover: f32,
    pub fog_density: f32,
    pub precipitation: f32,
    pub air_temp_c: f32,
    pub wind_ms: f32,
    /// Where the 10 m wind comes *from*, in degrees clockwise from north — the
    /// meteorological convention MET reports it in. `None` when the sample had no
    /// direction (a dead calm can be reported without one).
    pub wind_from_deg: Option<f32>,
    pub lat: f64,
    pub lng: f64,
}

/// The surface wind when no sample says otherwise: a light breeze from the
/// south-west, the prevailing wind over the Netherlands. The water, the grass and
/// the clouds all fall back to it — they are the same wind.
pub const FALLBACK_WIND_FROM_DEG: f32 = 225.0;
pub const FALLBACK_WIND_MS: f32 = 3.0;

/// The surface wind as a velocity in the scene's world frame (x east, z south),
/// in metres per second — what the wind *does*, where [`Observation`] reports
/// where it comes from.
#[must_use]
pub fn surface_wind(weather: &WeatherState) -> Vec2 {
    let (from_deg, speed) =
        weather
            .latest
            .map_or((FALLBACK_WIND_FROM_DEG, FALLBACK_WIND_MS), |o| {
                (
                    o.wind_from_deg.unwrap_or(FALLBACK_WIND_FROM_DEG),
                    o.wind_ms.max(0.0),
                )
            });
    wind_velocity(from_deg, speed)
}

/// A wind reported as "from `from_deg`" at `speed` m/s, as a world-frame velocity.
///
/// The air moves *toward* `from_deg + 180°`. A bearing is clockwise from north,
/// so its east component is `sin` and its north component `cos`; the world's +z
/// is south, hence the sign.
#[must_use]
pub fn wind_velocity(from_deg: f32, speed: f32) -> Vec2 {
    let toward = (from_deg + 180.0).to_radians();
    Vec2::new(toward.sin(), -toward.cos()) * speed
}

type FetchResult = Result<Observation, String>;

/// Live-weather polling state plus the most recent sample (for the HUD).
#[derive(Resource, Default)]
pub struct WeatherState {
    /// When true, each fetch drives cloud/fog/rain; when false, the dev-panel
    /// sliders stay in charge and we still keep `latest` fresh for the readout.
    pub live: bool,
    /// The in-flight fetch, if any; polled each frame and cleared on completion.
    task: Option<Task<FetchResult>>,
    in_flight: bool,
    last_poll: Option<Instant>,
    /// Anchor generation the current sample was fetched for; a bump forces a refetch.
    fetched_gen: Option<u32>,
    pub latest: Option<Observation>,
}

impl WeatherState {
    /// Construct with live application on/off (from the `--weather-live` flag).
    pub fn with_live(live: bool) -> Self {
        Self {
            live,
            ..Default::default()
        }
    }

    /// True once a sample has landed — the shot harness waits on this so a
    /// `--weather-live` capture reflects real conditions, not the defaults.
    pub fn ready(&self) -> bool {
        self.latest.is_some()
    }
}

/// Fetch the anchor's weather on a worker thread and (when live) apply it.
pub fn poll_weather(
    mut weather: ResMut<WeatherState>,
    mut dev: ResMut<DevPanelState>,
    stream: Res<MapStream>,
) {
    // Fold in the completed fetch, if this frame's poll finds it ready.
    if let Some(task) = weather.task.as_mut() {
        if let Some(result) = http::poll_task(task) {
            weather.task = None;
            weather.in_flight = false;
            match result {
                Ok(obs) => {
                    weather.latest = Some(obs);
                    if weather.live {
                        apply(&obs, &mut dev);
                    }
                }
                Err(e) => tracing::warn!(error = %e, "met.no weather fetch failed"),
            }
        }
    }

    // Keep the live knobs matched to the latest sample while live is on — every
    // frame, not just when a fresh fetch lands. Ticking "Live" on then applies
    // the reading we already have (the `latest` readout) instead of leaving the
    // sliders stale until the next refresh cadence. Guarded so it only marks the
    // resource changed when a value actually moves.
    if weather.live {
        if let Some(obs) = weather.latest {
            if dev.cloud_cover != obs.cloud_cover
                || dev.fog_density != obs.fog_density
                || dev.precipitation != obs.precipitation
            {
                apply(&obs, &mut dev);
            }
        }
    }

    let Some((lat, lng, _zoom)) = stream.geo_anchor() else {
        return;
    };

    // Refetch when the anchor moved, on the refresh cadence, or on the first tick.
    let anchor_moved = weather.fetched_gen != Some(stream.anchor_generation);
    let stale = weather.last_poll.is_none_or(|t| t.elapsed() >= REFRESH);
    if weather.in_flight || !(anchor_moved || stale) {
        return;
    }

    weather.last_poll = Some(Instant::now());
    weather.fetched_gen = Some(stream.anchor_generation);
    weather.in_flight = true;
    weather.task = Some(IoTaskPool::get().spawn(fetch_weather(lat, lng)));
}

/// Write a sample onto the renderer's live knobs.
fn apply(obs: &Observation, dev: &mut DevPanelState) {
    dev.cloud_cover = obs.cloud_cover;
    dev.fog_density = obs.fog_density;
    dev.precipitation = obs.precipitation;
}

/// Fetch and parse one MET `complete` forecast. Runs inside an `IoTaskPool` task;
/// the shared [`http::USER_AGENT`] carries the contact MET's terms require.
async fn fetch_weather(lat: f64, lng: f64) -> FetchResult {
    // MET asks for ≤4-decimal coordinates to keep their cache effective.
    let url = format!(
        "https://api.met.no/weatherapi/locationforecast/2.0/complete?lat={lat:.4}&lon={lng:.4}"
    );
    let body = http::fetch_string(http::Request::get(url).timeout(Duration::from_secs(15))).await?;
    parse_weather(&body, lat, lng)
}

/// Parse a MET `locationforecast/2.0/complete` body into an [`Observation`].
///
/// The first `timeseries` entry is "now": `instant.details` carries the point-in-
/// time fields (cloud %, fog %, humidity, temp, wind); `next_1_hours` (falling back
/// to `next_6_hours`) carries the accumulated `precipitation_amount` in mm.
fn parse_weather(text: &str, lat: f64, lng: f64) -> FetchResult {
    let json: serde_json::Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    let now = &json["properties"]["timeseries"][0]["data"];
    let inst = &now["instant"]["details"];
    if inst.is_null() {
        return Err("met.no response has no instant details".into());
    }

    let cloud_frac = inst["cloud_area_fraction"].as_f64().unwrap_or(0.0) / 100.0;
    let fog_frac = inst["fog_area_fraction"].as_f64().unwrap_or(0.0) / 100.0;
    let humidity = inst["relative_humidity"].as_f64().unwrap_or(0.0);
    let air_temp_c = inst["air_temperature"].as_f64().unwrap_or(0.0) as f32;
    let wind_ms = inst["wind_speed"].as_f64().unwrap_or(0.0) as f32;
    let wind_from_deg = inst["wind_from_direction"].as_f64().map(|d| d as f32);

    let precip_mm = now["next_1_hours"]["details"]["precipitation_amount"]
        .as_f64()
        .or_else(|| now["next_6_hours"]["details"]["precipitation_amount"].as_f64())
        .unwrap_or(0.0) as f32;

    // Fog is usually reported 0 even in visible haze, so back it with humidity:
    // RH 70%→0, 100%→full haze contribution.
    let humid_haze = (((humidity - 70.0) / 30.0).clamp(0.0, 1.0) * 0.20) as f32;
    let fog_density = (HAZE_FLOOR + (fog_frac as f32).max(humid_haze)).clamp(0.0, 1.0);

    Ok(Observation {
        cloud_cover: (cloud_frac as f32).clamp(0.0, 1.0),
        fog_density,
        precipitation: (precip_mm / RAIN_FULL_MM).clamp(0.0, 1.0),
        air_temp_c,
        wind_ms,
        wind_from_deg,
        lat,
        lng,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_clear_dry_sample() {
        let body = r#"{"properties":{"timeseries":[{"data":{
            "instant":{"details":{
                "cloud_area_fraction":12.5,"fog_area_fraction":0.0,
                "relative_humidity":55.0,"air_temperature":21.0,"wind_speed":3.2}},
            "next_1_hours":{"details":{"precipitation_amount":0.0}}
        }}]}}"#;
        let o = parse_weather(body, 53.2194, 6.5665).unwrap();
        assert!((o.cloud_cover - 0.125).abs() < 1e-4);
        assert_eq!(o.precipitation, 0.0);
        // Clear + dry ⇒ just the haze floor.
        assert!((o.fog_density - HAZE_FLOOR).abs() < 1e-4);
        assert_eq!(o.air_temp_c, 21.0);
        // No direction in the sample is reported as none, not as north.
        assert_eq!(o.wind_from_deg, None);
    }

    #[test]
    fn reads_the_wind_direction() {
        let body = r#"{"properties":{"timeseries":[{"data":{
            "instant":{"details":{
                "cloud_area_fraction":50.0,"wind_speed":6.0,"wind_from_direction":250.5}}
        }}]}}"#;
        let o = parse_weather(body, 53.2194, 6.5665).unwrap();
        assert_eq!(o.wind_from_deg, Some(250.5));
    }

    /// A wind *from* a bearing blows *toward* the opposite one, and the world's +z
    /// is south — the two sign flips this is easiest to get wrong.
    #[test]
    fn a_reported_wind_blows_the_right_way() {
        let close = |v: Vec2, x: f32, z: f32| (v - Vec2::new(x, z)).length() < 1e-5;
        // From the west: the air moves east, +x.
        assert!(close(wind_velocity(270.0, 1.0), 1.0, 0.0));
        // From the north: the air moves south, +z.
        assert!(close(wind_velocity(0.0, 1.0), 0.0, 1.0));
        // From the south: toward the north, −z.
        assert!(close(wind_velocity(180.0, 2.0), 0.0, -2.0));
        // Speed is the length.
        assert!((wind_velocity(225.0, 7.5).length() - 7.5).abs() < 1e-5);
    }

    /// With no sample the scene still has a wind, and it is the prevailing one.
    #[test]
    fn no_sample_falls_back_to_a_south_westerly() {
        let v = surface_wind(&WeatherState::default());
        assert!((v.length() - FALLBACK_WIND_MS).abs() < 1e-5);
        // From the south-west: toward the north-east, +x and −z.
        assert!(v.x > 0.0 && v.y < 0.0);
    }

    #[test]
    fn wet_overcast_humid_sample_maxes_the_knobs() {
        let body = r#"{"properties":{"timeseries":[{"data":{
            "instant":{"details":{
                "cloud_area_fraction":100.0,"fog_area_fraction":40.0,
                "relative_humidity":100.0,"air_temperature":9.0,"wind_speed":8.0}},
            "next_1_hours":{"details":{"precipitation_amount":6.0}}
        }}]}}"#;
        let o = parse_weather(body, 0.0, 0.0).unwrap();
        assert_eq!(o.cloud_cover, 1.0);
        assert_eq!(o.precipitation, 1.0); // 6 mm > RAIN_FULL_MM, clamped
                                          // fog_area_fraction 0.40 dominates the humidity haze, plus the floor.
        assert!((o.fog_density - (HAZE_FLOOR + 0.40)).abs() < 1e-4);
    }

    #[test]
    fn falls_back_to_six_hour_precip() {
        let body = r#"{"properties":{"timeseries":[{"data":{
            "instant":{"details":{"cloud_area_fraction":50.0}},
            "next_6_hours":{"details":{"precipitation_amount":2.0}}
        }}]}}"#;
        let o = parse_weather(body, 0.0, 0.0).unwrap();
        assert!((o.precipitation - 0.5).abs() < 1e-4); // 2.0 / 4.0
    }

    #[test]
    fn rejects_empty_timeseries() {
        let body = r#"{"properties":{"timeseries":[]}}"#;
        assert!(parse_weather(body, 0.0, 0.0).is_err());
    }
}
