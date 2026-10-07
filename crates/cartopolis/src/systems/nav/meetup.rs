//! Meeting a friend: a place, a time and who's coming, inside a private chat
//! (`docs/mockups/routing/v1` section 3, built on chat-world v4's screen 6).
//!
//! A meet-up is a point with a time ([`cartopolis_core::point::Meetup`]), sealed
//! like every other point, so the server never learns where anyone is meeting.
//! This module is the part that needs the world rather than the chat:
//!
//! - **Where to propose.** `+ → Meet up` asks for a place. When the other person
//!   is *already* sharing their location with this chat, the proposal is the
//!   point halfway along the walking route between you — equal walking times, by
//!   construction. Otherwise it is where the camera is, as "Send this view" is.
//!   Nobody is asked for their location just to be invited somewhere: that was
//!   the option chosen on 2026-10-06, over asking first.
//! - **Ending the share on arrival.** "I'm going" turns the location share on
//!   before the start (`ChatState::tick_meetups`); arriving at the place — a
//!   walk or a drive whose destination is the meet-up — turns it off and tells the
//!   chat, so nobody has to remember to stop sharing.

use bevy::prelude::*;
use std::sync::{Arc, Mutex};

use cartopolis_core::point::{Meetup, Point};

use crate::grid::AnchoredCamera;
use crate::resources::{MapAnchor, OsmTileUrlTemplate};
use crate::systems::chat::ChatState;
use crate::systems::driver::{metres_between, point_along, DriverState, RouteGeom};
use crate::systems::geo_nav::camera_geo;
use crate::systems::globe::GlobeState;
use crate::systems::navigation::{self, DetachedSolve, RouteState};
use crate::systems::routing::TravelMode;
use crate::systems::walk_guide::WalkGuide;

/// A proposal starts at least this far ahead, so there is time to get there.
const LEAD_MS: u64 = 30 * 60_000;

/// …rounded up to the next quarter hour, the way people name a time.
const ROUND_MS: u64 = 15 * 60_000;

/// Arriving within this of a meet-up you are going to ends its share.
/// Generous for the reason driver mode's arrival radius is: a route ends at the
/// nearest path, which can be a building's depth from the spot.
const ARRIVED_M: f64 = 60.0;

/// How long to wait for the geocoder to name the spot before proposing it
/// unnamed. The card still reads — "Meet here" and the map.
const NAME_WAIT_S: f32 = 4.0;

/// The default start of a new meet-up: [`LEAD_MS`] from now, rounded up to the
/// quarter hour.
#[must_use]
pub fn default_start_ms(now_ms: u64) -> u64 {
    (now_ms + LEAD_MS).div_ceil(ROUND_MS) * ROUND_MS
}

/// When a meet-up is, in words: "Today 19:00", "Tomorrow 09:30", or the date.
///
/// `offset_s` is the local UTC offset (`transit_live::WorldClock`); `None`
/// prints UTC and says so, as the HUD clock does.
#[must_use]
pub fn when_label(start_ms: u64, now_ms: u64, offset_s: Option<i32>) -> String {
    let off = i64::from(offset_s.unwrap_or(0)) * 1000;
    let local = |ms: u64| (ms as i64 + off).max(0) as u64;
    let (start, now) = (local(start_ms), local(now_ms));
    let day = |ms: u64| ms / 86_400_000;
    let secs = (start / 1000) % 86_400;
    let clock = format!("{:02}:{:02}", secs / 3600, (secs % 3600) / 60);
    let utc = if offset_s.is_none() { " UTC" } else { "" };
    match day(start).saturating_sub(day(now)) {
        0 => format!("Today {clock}{utc}"),
        1 => format!("Tomorrow {clock}{utc}"),
        _ => {
            // The date part of the RFC 3339 form of the *local* time.
            let date = cartopolis_core::point::rfc3339_utc(start);
            format!("{} {clock}{utc}", &date[..10])
        }
    }
}

/// A proposal being put together: waiting for the halfway route, then for a
/// name. Public only because a system's `Local` must be.
pub struct Pending {
    key: String,
    solve: Option<(DetachedSolve, (f64, f64), (f64, f64))>,
    spot: Option<(f64, f64)>,
    name: Arc<Mutex<Option<Option<String>>>>,
    asked_name: bool,
    waited_s: f32,
}

/// Turn `+ → Meet up` requests into a draft meet-up above the chat's composer.
#[allow(clippy::too_many_arguments)]
pub fn answer_meetup_requests(
    mut chat: ResMut<ChatState>,
    globe: Res<GlobeState>,
    camera: AnchoredCamera,
    map_anchor: Res<MapAnchor>,
    tile_url: Res<OsmTileUrlTemplate>,
    time: Res<Time>,
    mut pending: Local<Vec<Pending>>,
) {
    let here = camera
        .get()
        .zip(map_anchor.geo)
        .map(|((cell, tf), anchor)| camera_geo(anchor, &globe, cell, tf));
    for key in std::mem::take(&mut chat.meetup_requests) {
        let Some(here) = here else {
            chat.notice = Some(crate::systems::chat::ChatNotice {
                text: "There is no place on the map to meet at yet.".into(),
                error: true,
            });
            continue;
        };
        // Somebody already sharing with this chat: halfway along the walk.
        let them = chat
            .locations
            .iter()
            .find(|l| l.channel == key)
            .map(|l| (l.latitude, l.longitude));
        let template = tile_url.0.lock().map(|g| g.clone()).unwrap_or_default();
        pending.retain(|p| p.key != key);
        pending.push(match them {
            Some(them) => Pending {
                key,
                solve: Some((
                    navigation::solve_detached(here, them, TravelMode::Foot, template),
                    here,
                    them,
                )),
                spot: None,
                name: Arc::new(Mutex::new(None)),
                asked_name: false,
                waited_s: 0.0,
            },
            None => Pending {
                key,
                solve: None,
                spot: Some(here),
                name: Arc::new(Mutex::new(None)),
                asked_name: false,
                waited_s: 0.0,
            },
        });
    }

    let dt = time.delta_secs();
    let mut ready = Vec::new();
    pending.retain_mut(|p| {
        if let Some((solve, a, b)) = &p.solve {
            let landed = solve.lock().ok().and_then(|mut g| g.take());
            match landed {
                None => return true,
                Some(result) => {
                    p.spot = Some(
                        result
                            .ok()
                            .and_then(|r| halfway(&r))
                            .unwrap_or(((a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0)),
                    );
                    p.solve = None;
                }
            }
        }
        let Some((lat, lng)) = p.spot else {
            return true;
        };
        if !p.asked_name {
            p.asked_name = true;
            let sink = p.name.clone();
            navigation::spawn_place_name(lat, lng, move |name| {
                if let Ok(mut g) = sink.lock() {
                    *g = Some(name);
                }
            });
        }
        p.waited_s += dt;
        let named = p.name.lock().ok().and_then(|mut g| g.take());
        match named {
            Some(name) => {
                ready.push((p.key.clone(), lat, lng, name));
                false
            }
            None if p.waited_s > NAME_WAIT_S => {
                ready.push((p.key.clone(), lat, lng, None));
                false
            }
            None => true,
        }
    });
    let now = now_ms();
    for (key, lat, lng, name) in ready {
        chat.meetup_draft = Some((
            key,
            Point {
                latitude: lat,
                longitude: lng,
                // The first part of the address is the street or the place.
                name: name.map(|n| n.split(',').next().unwrap_or(&n).trim().to_string()),
                meetup: Some(Meetup {
                    start_ms: default_start_ms(now),
                    going: false,
                }),
                ..Point::default()
            },
        ));
    }
}

/// The point halfway along a route, by distance — which on foot both ways is
/// halfway by time.
fn halfway(route: &crate::systems::routing::Route) -> Option<(f64, f64)> {
    let geom = RouteGeom::build(route)?;
    point_along(&geom, geom.total_m / 2.0).map(|(p, _)| (p[0], p[1]))
}

/// End the share of a meet-up the moment you arrive at it, and say so in the
/// chat. Arriving is the walk's or the drive's own latched arrival, at a
/// destination near the meet-up.
pub fn finish_on_arrival(
    mut chat: ResMut<ChatState>,
    walk: Res<WalkGuide>,
    driver: Res<DriverState>,
    route: Res<RouteState>,
) {
    if chat.meetups_going.is_empty() || !(walk.arrived || driver.arrived) {
        return;
    }
    let Some(to) = route.to.coord else {
        return;
    };
    let (here, rest): (Vec<_>, Vec<_>) = std::mem::take(&mut chat.meetups_going)
        .into_iter()
        .partition(|(_, p)| metres_between([to.0, to.1], [p.latitude, p.longitude]) < ARRIVED_M);
    chat.meetups_going = rest;
    for (key, _) in here {
        chat.stop_sharing(&key);
        chat.private_outbox.push((key, "I'm here".to_string()));
    }
}

fn now_ms() -> u64 {
    web_time::SystemTime::now()
        .duration_since(web_time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    // 2026-10-06T17:00:00Z.
    const FIVE_PM: u64 = 1_791_306_000_000;

    #[test]
    fn a_proposal_starts_half_an_hour_out_on_a_quarter_hour() {
        // 17:00 → 17:30; 17:01 → 17:45; 17:20 → 18:00.
        assert_eq!(default_start_ms(FIVE_PM), FIVE_PM + 30 * 60_000);
        assert_eq!(default_start_ms(FIVE_PM + 60_000), FIVE_PM + 45 * 60_000);
        assert_eq!(
            default_start_ms(FIVE_PM + 20 * 60_000),
            FIVE_PM + 60 * 60_000
        );
    }

    #[test]
    fn the_time_reads_in_local_words() {
        let cest = Some(2 * 3600);
        assert_eq!(when_label(FIVE_PM, FIVE_PM, cest), "Today 19:00");
        assert_eq!(
            when_label(FIVE_PM + 86_400_000, FIVE_PM, cest),
            "Tomorrow 19:00"
        );
        assert_eq!(when_label(FIVE_PM, FIVE_PM, None), "Today 17:00 UTC");
        assert_eq!(
            when_label(FIVE_PM + 3 * 86_400_000, FIVE_PM, cest),
            "2026-10-09 19:00"
        );
        // 23:30 local tonight is still today, though it is tomorrow in UTC-less
        // arithmetic only if the offset were ignored.
        assert_eq!(
            when_label(FIVE_PM + 4 * 3600_000 + 30 * 60_000, FIVE_PM, cest),
            "Today 23:30"
        );
    }
}
