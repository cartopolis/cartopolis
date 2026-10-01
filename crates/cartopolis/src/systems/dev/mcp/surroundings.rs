//! **What an agent knows about where it is**, without taking a picture.
//!
//! A person standing on a street reads it at a glance: the building in front of
//! them, the café across the square, a bench, somebody they know. An agent had
//! two ways to find any of that out — guess from coordinates, or spend a capture
//! and a large slice of its context on an image of it. Everything here is already
//! loaded for the person at the keyboard (the house-number readout, the walk-up
//! prompt, the building card), so the words cost a lookup and nothing more.
//!
//! The other half is what it keeps between conversations: places it has named
//! and notes it has written. Both are per account and server, in the `Data`
//! namespace beside the list of things it built (`agent::AgentWorld::builds_key`),
//! because the bot's own instructions are a read-only file and nothing else in
//! it survives a restart.

use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::systems::building_facts::{use_labels, BuildingFacts};
use crate::systems::world_places::{InteractKind, WorldPlaces};

/// Metres per degree of latitude, the flat small-angle figure the readouts use.
const M_PER_DEG: f64 = 111_320.0;

/// The radius `look_around` describes unless asked for another, and its cap.
pub const DEFAULT_LOOK_M: f32 = 60.0;
pub const MAX_LOOK_M: f32 = 300.0;

/// Most entries of one kind a description lists. The rest are counted: a city
/// square has a hundred things on it and an agent wants the nearest few.
const LIST_MAX: usize = 10;

// ---------------------------------------------------------------------------
// Directions
// ---------------------------------------------------------------------------

/// Compass bearing in degrees (0 north, clockwise) of a step in the anchored
/// world frame, where +x is east and **+z is south**.
#[must_use]
pub fn bearing_deg(dx_east: f32, dz_south: f32) -> f32 {
    dx_east.atan2(-dz_south).to_degrees().rem_euclid(360.0)
}

/// The eight-point compass word for a bearing.
#[must_use]
pub fn compass(bearing: f32) -> &'static str {
    const POINTS: [&str; 8] = ["N", "NE", "E", "SE", "S", "SW", "W", "NW"];
    POINTS[(((bearing + 22.5).rem_euclid(360.0)) / 45.0) as usize % 8]
}

/// `"12 m NE"` for a step from `from` to `to`, both anchored world XZ.
#[must_use]
pub fn how_far(from: Vec2, to: Vec2) -> (f32, &'static str) {
    let d = to - from;
    (d.length(), compass(bearing_deg(d.x, d.y)))
}

/// Metres east and south from one lat/lng to another — the world frame's axes.
#[must_use]
pub fn geo_step(from: (f64, f64), to: (f64, f64)) -> Vec2 {
    let scale = from.0.to_radians().cos();
    Vec2::new(
        ((to.1 - from.1) * M_PER_DEG * scale) as f32,
        (-(to.0 - from.0) * M_PER_DEG) as f32,
    )
}

/// Whether a point is inside a lat/lng ring (even-odd; open or closed ring).
#[must_use]
pub fn inside_ring(p: (f64, f64), ring: &[(f64, f64)]) -> bool {
    let mut inside = false;
    let n = ring.len();
    if n < 3 {
        return false;
    }
    let mut j = n - 1;
    for i in 0..n {
        let (yi, xi) = ring[i];
        let (yj, xj) = ring[j];
        if (yi > p.0) != (yj > p.0) && p.1 < (xj - xi) * (p.0 - yi) / (yj - yi) + xi {
            inside = !inside;
        }
        j = i;
    }
    inside
}

fn ring_centre(ring: &[(f64, f64)]) -> Option<(f64, f64)> {
    if ring.is_empty() {
        return None;
    }
    let n = ring.len() as f64;
    let (a, b) = ring
        .iter()
        .fold((0.0, 0.0), |(a, b), (lat, lng)| (a + lat, b + lng));
    Some((a / n, b / n))
}

// ---------------------------------------------------------------------------
// The clock and the sky
// ---------------------------------------------------------------------------

/// Seconds since the Unix epoch, now.
#[must_use]
pub fn now_unix() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}

/// The sun's elevation in degrees at a place and moment.
#[must_use]
pub fn sun_elevation_deg(lat: f64, lng: f64, unix: f64) -> f32 {
    let hours = crate::utils::sun::solar_time_of_day(unix, lng);
    let day_of_year = calendar_day(unix);
    let decl = crate::utils::sun::solar_declination_rad(day_of_year);
    let phi = (lat as f32).to_radians();
    let h = ((hours - 12.0) * 15.0).to_radians();
    (phi.sin() * decl.sin() + phi.cos() * decl.cos() * h.cos())
        .clamp(-1.0, 1.0)
        .asin()
        .to_degrees()
}

/// Day of the year (1–366) for a Unix time, UTC.
fn calendar_day(unix: f64) -> f32 {
    let days = (unix / 86_400.0).floor() as i64;
    // Civil-from-days (Howard Hinnant), enough to find 1 January of the year.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy_march = doe - (365 * yoe + yoe / 4 - yoe / 100);
    // `doy_march` counts from 1 March; shift to 1 January.
    let y = yoe + era * 400;
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let jan_feb = 31 + if leap { 29 } else { 28 };
    let doy = if doy_march >= 306 {
        doy_march - 306
    } else {
        doy_march + jan_feb
    };
    (doy + 1) as f32
}

/// One phrase for how light it is.
#[must_use]
pub fn daylight(elevation: f32) -> &'static str {
    match elevation {
        e if e > 6.0 => "daylight",
        e if e > -0.833 => "the sun is low",
        e if e > -6.0 => "twilight",
        e if e > -12.0 => "dusk or dawn, nearly dark",
        _ => "night",
    }
}

/// `"14:20 UTC; local sun time 14:46, daylight (sun 31° up)"`.
#[must_use]
pub fn clock_line(at: Option<(f64, f64)>, unix: f64) -> (String, Value) {
    let utc = (unix / 3600.0).rem_euclid(24.0);
    let hm = |h: f64| format!("{:02}:{:02}", h.floor() as u32, ((h.fract()) * 60.0) as u32);
    let mut text = format!("It is {} UTC", hm(utc));
    let mut data = json!({ "utc": hm(utc) });
    if let Some((lat, lng)) = at {
        let solar = f64::from(crate::utils::sun::solar_time_of_day(unix, lng));
        let el = sun_elevation_deg(lat, lng, unix);
        text.push_str(&format!(
            "; sun time here {}, {} (sun {:.0}° {})",
            hm(solar),
            daylight(el),
            el.abs(),
            if el >= 0.0 { "up" } else { "below the horizon" }
        ));
        data["sun_time"] = json!(hm(solar));
        data["sun_elevation_deg"] = json!(el);
        data["light"] = json!(daylight(el));
    }
    (text, data)
}

/// The latest weather sample, in words.
#[must_use]
pub fn weather_line(obs: &crate::systems::weather::Observation) -> (String, Value) {
    let sky = match obs.cloud_cover {
        c if c < 0.15 => "clear",
        c if c < 0.5 => "partly cloudy",
        c if c < 0.85 => "mostly cloudy",
        _ => "overcast",
    };
    let mut text = format!("{:.0} °C, {sky}", obs.air_temp_c);
    if obs.precipitation > 0.05 {
        text.push_str(if obs.precipitation > 0.5 {
            ", raining"
        } else {
            ", light rain"
        });
    }
    if obs.fog_density > 0.3 {
        text.push_str(", foggy");
    }
    text.push_str(&format!(", wind {:.0} m/s", obs.wind_ms));
    if let Some(from) = obs.wind_from_deg {
        text.push_str(&format!(" from the {}", compass(from)));
    }
    (
        text,
        json!({
            "air_temp_c": obs.air_temp_c,
            "cloud_cover": obs.cloud_cover,
            "precipitation": obs.precipitation,
            "fog": obs.fog_density,
            "wind_ms": obs.wind_ms,
            "wind_from_deg": obs.wind_from_deg,
        }),
    )
}

// ---------------------------------------------------------------------------
// What it keeps: named places and notes
// ---------------------------------------------------------------------------

/// A place the agent named.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SavedPlace {
    pub name: String,
    pub lat: f64,
    pub lng: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// A note the agent wrote for itself.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Note {
    pub id: u32,
    /// Unix seconds.
    pub at: u64,
    /// What or whom it is about: a person's name, a place, a build.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub about: Option<String>,
    pub text: String,
}

/// Most places and notes kept, and the longest note. A notebook that grows
/// without bound is read back in full one day and fills a context window.
pub const MAX_PLACES: usize = 200;
pub const MAX_NOTES: usize = 1000;
pub const MAX_NOTE_CHARS: usize = 2000;

/// Read a JSON list from the `Data` namespace, or an empty one.
#[must_use]
pub fn load_list<T: serde::de::DeserializeOwned>(key: &str) -> Vec<T> {
    crate::platform::storage::read_now(crate::platform::storage::Namespace::Data, key)
        .ready()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// Write a JSON list to the `Data` namespace.
pub fn save_list<T: Serialize>(key: &str, items: &[T]) {
    let bytes = serde_json::to_vec(items).unwrap_or_default();
    if let Err(e) =
        crate::platform::storage::write_now(crate::platform::storage::Namespace::Data, key, &bytes)
    {
        tracing::warn!(error = %e, key, "mcp: could not save");
    }
}

/// Find a saved place by name, without case.
#[must_use]
pub fn find_saved<'a>(places: &'a [SavedPlace], name: &str) -> Option<&'a SavedPlace> {
    let name = name.trim();
    places.iter().find(|p| p.name.eq_ignore_ascii_case(name))
}

/// Notes matching an `about` and a word, newest first.
#[must_use]
pub fn recall<'a>(
    notes: &'a [Note],
    about: Option<&str>,
    search: Option<&str>,
    limit: usize,
) -> Vec<&'a Note> {
    let search = search.map(str::to_lowercase);
    let mut out: Vec<&Note> = notes
        .iter()
        .filter(|n| {
            about.is_none_or(|a| {
                n.about
                    .as_deref()
                    .is_some_and(|b| b.eq_ignore_ascii_case(a.trim()))
            })
        })
        .filter(|n| {
            search.as_deref().is_none_or(|s| {
                n.text.to_lowercase().contains(s)
                    || n.about
                        .as_deref()
                        .is_some_and(|a| a.to_lowercase().contains(s))
            })
        })
        .collect();
    out.sort_by(|a, b| b.at.cmp(&a.at).then(b.id.cmp(&a.id)));
    out.truncate(limit);
    out
}

// ---------------------------------------------------------------------------
// Looking around
// ---------------------------------------------------------------------------

/// An object somebody built, as `look_around` sees it.
pub struct SeenPrim {
    pub id: i64,
    pub name: String,
    pub at: Vec2,
    pub scripted: bool,
    pub mine: bool,
}

/// Everything `look_around` reads, gathered by the caller.
pub struct Scene<'a> {
    /// The avatar, in anchored world XZ and on Earth.
    pub here: Vec2,
    pub geo: (f64, f64),
    pub radius: f32,
    pub places: &'a WorldPlaces,
    pub facts: &'a BuildingFacts,
    pub prims: Vec<SeenPrim>,
    /// `(distance, name)` of the people near, named or "someone".
    pub people: Vec<(f32, String)>,
}

/// Describe the scene in words, with the same facts as data.
#[must_use]
pub fn describe(scene: &Scene) -> (String, Value) {
    let mut text = String::new();
    let mut data = json!({ "lat": scene.geo.0, "lng": scene.geo.1, "radius_m": scene.radius });

    // ── Where: the building you are in or the address in front of you.
    let inside = scene
        .facts
        .near(scene.geo.0, scene.geo.1, 80.0)
        .into_iter()
        .find(|(_, b)| inside_ring(scene.geo, &b.footprint))
        .map(|(id, _)| id.to_string());
    let address = scene
        .facts
        .nearest_address(scene.geo.0, scene.geo.1, 35.0)
        .and_then(crate::systems::building_facts::AddressFact::label)
        .or_else(|| {
            scene
                .places
                .nearest_address(scene.here, 35.0)
                .map(str::to_string)
        });
    match (&inside, &address) {
        (Some(_), Some(a)) => text.push_str(&format!("You are inside the building at {a}. ")),
        (Some(_), None) => text.push_str("You are inside a building. "),
        (None, Some(a)) => text.push_str(&format!("You are by {a}. ")),
        (None, None) => text.push_str("No address is known here. "),
    }
    data["address"] = json!(address);
    data["inside_building"] = json!(inside);

    // ── People.
    if !scene.people.is_empty() {
        let list: Vec<String> = scene
            .people
            .iter()
            .take(LIST_MAX)
            .map(|(d, n)| format!("{n} {d:.0} m"))
            .collect();
        text.push_str(&format!("\nPeople: {}.", list.join(", ")));
    }
    data["people"] = json!(scene
        .people
        .iter()
        .take(LIST_MAX)
        .map(|(d, n)| json!({ "name": n, "distance_m": d }))
        .collect::<Vec<_>>());

    // ── Objects people built.
    let mut prims: Vec<(&SeenPrim, f32, &str)> = scene
        .prims
        .iter()
        .map(|p| {
            let (d, dir) = how_far(scene.here, p.at);
            (p, d, dir)
        })
        .filter(|(_, d, _)| *d <= scene.radius)
        .collect();
    prims.sort_by(|a, b| a.1.total_cmp(&b.1));
    if !prims.is_empty() {
        let list: Vec<String> = prims
            .iter()
            .take(LIST_MAX)
            .map(|(p, d, dir)| {
                let mut s = format!("\"{}\" (#{}) {d:.0} m {dir}", p.name, p.id);
                if p.scripted {
                    s.push_str(", scripted — `touch` presses it");
                }
                if p.mine {
                    s.push_str(", yours");
                }
                s
            })
            .collect();
        text.push_str(&format!("\nObjects: {}.", list.join("; ")));
        if prims.len() > LIST_MAX {
            text.push_str(&format!(" And {} more.", prims.len() - LIST_MAX));
        }
    }
    data["objects"] = json!(prims
        .iter()
        .take(LIST_MAX)
        .map(|(p, d, dir)| json!({
            "id": p.id, "name": p.name, "distance_m": d, "direction": dir,
            "scripted": p.scripted, "yours": p.mine,
        }))
        .collect::<Vec<_>>());

    // ── Named places: shops, cafés, stops.
    let mut named: Vec<(&str, Option<&str>, f32, &str, Vec2)> = Vec::new();
    // Street furniture by kind: (name, count, nearest distance, its direction).
    let mut furniture: Vec<(String, usize, f32, &str)> = Vec::new();
    for (i, _) in scene.places.within(scene.here, scene.radius) {
        let (d, dir) = how_far(scene.here, i.pos);
        match i.kind {
            InteractKind::Place { .. } => named.push((&i.name, i.kind.detail(), d, dir, i.pos)),
            InteractKind::Prop(_) => match furniture.iter_mut().find(|f| f.0 == i.name) {
                Some(f) => {
                    f.1 += 1;
                    if d < f.2 {
                        f.2 = d;
                        f.3 = dir;
                    }
                }
                None => furniture.push((i.name.clone(), 1, d, dir)),
            },
        }
    }
    named.sort_by(|a, b| a.2.total_cmp(&b.2));
    if !named.is_empty() {
        let list: Vec<String> = named
            .iter()
            .take(LIST_MAX)
            .map(|(n, kind, d, dir, _)| match kind {
                Some(k) => format!("{n} ({k}) {d:.0} m {dir}"),
                None => format!("{n} {d:.0} m {dir}"),
            })
            .collect();
        text.push_str(&format!("\nPlaces: {}.", list.join("; ")));
        if named.len() > LIST_MAX {
            text.push_str(&format!(" And {} more.", named.len() - LIST_MAX));
        }
    }
    data["places"] = json!(named
        .iter()
        .take(LIST_MAX)
        .map(|(n, kind, d, dir, _)| json!({
            "name": n, "kind": kind, "distance_m": d, "direction": dir,
        }))
        .collect::<Vec<_>>());
    furniture.sort_by(|a, b| a.2.total_cmp(&b.2));
    if !furniture.is_empty() {
        let list: Vec<String> = furniture
            .iter()
            .map(|(n, count, d, dir)| {
                if *count == 1 {
                    format!("{} {d:.0} m {dir}", n.to_lowercase())
                } else {
                    format!("{count} × {} (nearest {d:.0} m {dir})", n.to_lowercase())
                }
            })
            .collect();
        text.push_str(&format!("\nOn the street: {}.", list.join(", ")));
    }
    data["street_furniture"] = json!(furniture
        .iter()
        .map(|(n, count, d, dir)| json!({
            "kind": n, "count": count, "nearest_m": d, "direction": dir,
        }))
        .collect::<Vec<_>>());

    // ── Buildings, nearest first, with what the register says.
    let mut buildings: Vec<(f32, &str, String, Value)> = scene
        .facts
        .near(scene.geo.0, scene.geo.1, f64::from(scene.radius) + 30.0)
        .into_iter()
        .filter_map(|(id, b)| {
            let centre = ring_centre(&b.footprint)?;
            let step = geo_step(scene.geo, centre);
            let d = step.length();
            (d <= scene.radius).then_some((d, id, b, step))
        })
        .map(|(d, id, b, step)| {
            let dir = compass(bearing_deg(step.x, step.y));
            let addr = scene
                .facts
                .addresses()
                .iter()
                .filter(|a| a.pand.as_deref() == Some(id))
                .find_map(crate::systems::building_facts::AddressFact::label);
            let uses = use_labels(b.uses);
            let mut s = match &addr {
                Some(a) => format!("{a}"),
                None => "a building".to_string(),
            };
            s.push_str(&format!(" {d:.0} m {dir}"));
            if !uses.is_empty() {
                s.push_str(&format!(", {}", uses.join(" and ")));
            }
            if let Some(y) = b.year {
                s.push_str(&format!(", built {y}"));
            }
            if b.units > 1 {
                s.push_str(&format!(", {} units", b.units));
            }
            if b.monument.is_some() {
                s.push_str(", a national monument");
            }
            if !b.status.standing() {
                s.push_str(", out of use");
            }
            let v = json!({
                "id": id, "address": addr, "distance_m": d, "direction": dir,
                "uses": uses, "year": b.year, "units": b.units,
                "monument": b.monument.is_some(),
                "monument_category": b.monument_category.as_deref(),
                "district": b.district.as_deref(),
            });
            (d, id, s, v)
        })
        .collect();
    buildings.sort_by(|a, b| a.0.total_cmp(&b.0));
    if !buildings.is_empty() {
        let list: Vec<&str> = buildings
            .iter()
            .take(LIST_MAX)
            .map(|(_, _, s, _)| s.as_str())
            .collect();
        text.push_str(&format!("\nBuildings: {}.", list.join("; ")));
        if buildings.len() > LIST_MAX {
            text.push_str(&format!(" And {} more.", buildings.len() - LIST_MAX));
        }
        if let Some(district) = buildings
            .iter()
            .find_map(|(_, id, _, _)| scene.facts.building(id).and_then(|b| b.district.clone()))
        {
            text.push_str(&format!(
                "\nThis is in the protected historic district {district}."
            ));
        }
    } else if scene.facts.is_empty() {
        text.push_str(
            "\nNo building register is loaded here (it covers the Netherlands only, or \
             has not arrived yet).",
        );
    }
    data["buildings"] = json!(buildings
        .iter()
        .take(LIST_MAX)
        .map(|(_, _, _, v)| v.clone())
        .collect::<Vec<_>>());

    if named.is_empty() && furniture.is_empty() && prims.is_empty() && buildings.is_empty() {
        text.push_str(&format!(
            "\nNothing is loaded within {:.0} m — the map may still be arriving.",
            scene.radius
        ));
    }
    (text, data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_compass_reads_the_world_frame() {
        // +x east, +z south.
        assert_eq!(compass(bearing_deg(0.0, -1.0)), "N");
        assert_eq!(compass(bearing_deg(1.0, 0.0)), "E");
        assert_eq!(compass(bearing_deg(0.0, 1.0)), "S");
        assert_eq!(compass(bearing_deg(-1.0, 0.0)), "W");
        assert_eq!(compass(bearing_deg(1.0, -1.0)), "NE");
        assert_eq!(compass(bearing_deg(-1.0, 1.0)), "SW");
        assert_eq!(compass(359.0), "N");
    }

    #[test]
    fn a_geo_step_agrees_with_the_world_frame() {
        let from = (53.2, 6.5);
        let north = geo_step(from, (53.2001, 6.5));
        assert!(
            north.y < 0.0 && north.x.abs() < 1e-3,
            "north is -z: {north}"
        );
        let east = geo_step(from, (53.2, 6.5001));
        assert!(east.x > 0.0 && east.y.abs() < 1e-3, "east is +x: {east}");
        assert!((north.length() - 11.13).abs() < 0.05);
    }

    #[test]
    fn a_point_is_inside_a_ring_either_way_round() {
        let square = [(0.0, 0.0), (0.0, 1.0), (1.0, 1.0), (1.0, 0.0)];
        let mut reversed = square;
        reversed.reverse();
        for ring in [&square[..], &reversed[..]] {
            assert!(inside_ring((0.5, 0.5), ring));
            assert!(!inside_ring((1.5, 0.5), ring));
        }
    }

    #[test]
    fn the_calendar_day_is_the_calendars() {
        // 2026-01-01T00:00Z and 2026-10-01T12:00Z.
        assert_eq!(calendar_day(1_767_225_600.0), 1.0);
        assert_eq!(calendar_day(1_790_856_000.0), 274.0);
        // 2024 is a leap year: 31 December is day 366.
        assert_eq!(calendar_day(1_735_603_200.0), 366.0);
    }

    #[test]
    fn the_sun_is_up_at_noon_and_down_at_midnight_in_groningen() {
        // 2026-06-21, solar noon at 6.57°E is about 11:34 UTC.
        let noon = 1_782_000_000.0 - (1_782_000_000.0 % 86_400.0) + 11.57 * 3600.0;
        let el = sun_elevation_deg(53.22, 6.57, noon);
        assert!((55.0..65.0).contains(&el), "June noon elevation {el}");
        let el = sun_elevation_deg(53.22, 6.57, noon + 12.0 * 3600.0);
        assert!(el < -10.0, "June midnight elevation {el}");
        assert_eq!(daylight(30.0), "daylight");
        assert_eq!(daylight(-20.0), "night");
    }

    #[test]
    fn recall_filters_by_subject_and_word_newest_first() {
        let note = |id, at, about: Option<&str>, text: &str| Note {
            id,
            at,
            about: about.map(str::to_string),
            text: text.to_string(),
        };
        let notes = vec![
            note(1, 10, Some("jeroen"), "likes the Vismarkt"),
            note(2, 30, Some("Jeroen"), "built a fountain"),
            note(3, 20, None, "the beacon answers on channel 7"),
        ];
        let got: Vec<u32> = recall(&notes, Some("jeroen"), None, 10)
            .iter()
            .map(|n| n.id)
            .collect();
        assert_eq!(got, vec![2, 1]);
        let got: Vec<u32> = recall(&notes, None, Some("BEACON"), 10)
            .iter()
            .map(|n| n.id)
            .collect();
        assert_eq!(got, vec![3]);
        assert_eq!(recall(&notes, None, None, 2).len(), 2);
    }

    #[test]
    fn saved_places_are_found_without_case() {
        let places = vec![SavedPlace {
            name: "Home".into(),
            lat: 1.0,
            lng: 2.0,
            note: None,
        }];
        assert!(find_saved(&places, " home ").is_some());
        assert!(find_saved(&places, "work").is_none());
    }
}
