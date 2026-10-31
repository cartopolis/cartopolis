//! Place names over the map from a distance: cities and towns as banners with
//! their population, district names in small capitals, stations as a round
//! icon (`docs/mockups/macro-view/`, v4, decided 2026-10-30).
//!
//! **Painted, not meshed**, for the reason the saved-place pins are
//! (`place_pins`'s module note): a city is not a surface a name can be written
//! on, and a name has to read from 2 km and from 200 km. Below
//! [`MIN_ALTITUDE_M`] the street names on the ground take over
//! (`systems::street_labels`), and these step aside.
//!
//! **From the tiles the map already loads.** Shortbread's `place_labels` and
//! `public_transport` layers ride in the same vector tiles as the ground
//! (`vector_tiles::extract_place_labels`), fetched through the shared byte cache,
//! so a tile the basemap has fetched costs a parse and no download. Each kind of
//! name lives in the tiles of one zoom ([`LEVELS`]); which of those are kept
//! follows the camera's ground point and height.
//!
//! **Which names show** follows distance from the camera, kind by kind
//! ([`shows`]), and then room: names are placed biggest place first and one that
//! would overlap a placed one is left out.

use crate::components::FreeCamera;
use crate::resources::MapAnchor;
use crate::systems::egui_manager::{EguiMut, WorldAnchored};
use crate::systems::globe::GlobeState;
use crate::systems::theme;
use crate::systems::vector_tiles::{PlaceKind, PlaceLabel};
use bevy::prelude::*;
use bevy::tasks::{IoTaskPool, Task};
use big_space::prelude::GridCell;
use cartopolis_core::TileKey;
use std::collections::{HashMap, HashSet};

/// Under this camera height no place names are drawn: the street names on the
/// ground are the map's text there.
const MIN_ALTITUDE_M: f64 = 1_500.0;

/// Over this camera height none are either: past the flat map, the globe's own
/// names are a later step.
const MAX_ALTITUDE_M: f64 = 250_000.0;

/// One zoom of tiles names are read from: the zoom, and the camera heights it is
/// kept between. Shortbread carries towns from z10, districts at z12 and
/// stations from z13, each thinned on the way out.
const LEVELS: &[(u32, f64, f64)] = &[
    (10, MIN_ALTITUDE_M, MAX_ALTITUDE_M),
    (12, MIN_ALTITUDE_M, 30_000.0),
    (13, MIN_ALTITUDE_M, 20_000.0),
];

/// Tiles kept per level at most, in each direction from the one under the
/// camera — so at most 9×9 a level.
const MAX_TILE_RADIUS: i64 = 4;

/// Fetches in flight at once.
const MAX_IN_FLIGHT: usize = 6;

/// One place, where it is.
#[derive(Debug, Clone)]
struct Place {
    name: String,
    kind: PlaceKind,
    population: u32,
    lat: f64,
    lng: f64,
}

/// The names read from the tiles around the camera.
#[derive(Resource, Default)]
pub struct PlaceNames {
    tiles: HashMap<TileKey, Vec<Place>>,
    pending: HashMap<TileKey, Task<Vec<Place>>>,
    /// Tiles that came back empty or failed, so they are not asked again while
    /// they stay wanted.
    empty: HashSet<TileKey>,
}

/// Keep the tiles the names come from around the camera.
pub fn stream_place_names(
    mut names: ResMut<PlaceNames>,
    map_anchor: Res<MapAnchor>,
    globe: Res<GlobeState>,
    camera: Query<(&GridCell, &Transform), With<FreeCamera>>,
    layers: Res<crate::systems::data_layers::DataLayers>,
    tile_url: Res<crate::resources::OsmTileUrlTemplate>,
) {
    // Finished fetches first, whatever the camera does.
    let done: Vec<TileKey> = names
        .pending
        .iter_mut()
        .filter_map(|(k, t)| t.is_finished().then_some(*k))
        .collect();
    for key in done {
        if let Some(mut task) = names.pending.remove(&key) {
            if let Some(places) = crate::platform::http::poll_task(&mut task) {
                if places.is_empty() {
                    names.empty.insert(key);
                } else {
                    names.tiles.insert(key, places);
                }
            }
        }
    }
    let Some(anchor) = map_anchor.geo else {
        return;
    };
    let Ok((cell, tf)) = camera.single() else {
        return;
    };
    let alt = globe.altitude_m(cell, tf);
    if !layers.place_names || !(MIN_ALTITUDE_M..MAX_ALTITUDE_M).contains(&alt) {
        return;
    }
    let (lat, lng) = crate::systems::geo_nav::camera_geo(anchor, &globe, cell, tf);
    let mut wanted: HashSet<TileKey> = HashSet::new();
    for &(z, lo, hi) in LEVELS {
        if !(lo..hi).contains(&alt) {
            continue;
        }
        let tile_m = cartopolis_core::world::tile_to_meters(z, lat);
        // A straight-down view is about 1.5 × the height across; a tilted one
        // sees further, and those names are the horizon's clutter anyway.
        let r = ((alt * 1.2 / tile_m).ceil() as i64).clamp(1, MAX_TILE_RADIUS);
        let (cx, cy) = cartopolis_core::world::lat_lng_to_tile(lat, lng, z);
        let n = 1i64 << z;
        for dy in -r..=r {
            for dx in -r..=r {
                let y = cy + dy;
                if !(0..n).contains(&y) {
                    continue;
                }
                wanted.insert(TileKey::new((cx + dx).rem_euclid(n), y, z));
            }
        }
    }
    names.tiles.retain(|k, _| wanted.contains(k));
    names.empty.retain(|k| wanted.contains(k));
    names.pending.retain(|k, _| wanted.contains(k));
    let template = tile_url.0.lock().map(|g| g.clone()).unwrap_or_default();
    if cartopolis_core::tile_source_kind(&template) != cartopolis_core::TileSourceKind::Vector {
        return;
    }
    let mut missing: Vec<TileKey> = wanted
        .into_iter()
        .filter(|k| {
            !names.tiles.contains_key(k)
                && !names.pending.contains_key(k)
                && !names.empty.contains(k)
        })
        .collect();
    // Coarse first: the towns before the districts.
    missing.sort_by_key(|k| k.z);
    for key in missing {
        if names.pending.len() >= MAX_IN_FLIGHT {
            break;
        }
        let template = template.clone();
        names
            .pending
            .insert(key, IoTaskPool::get().spawn(fetch_places(key, template)));
    }
}

/// One tile's names, as places on the globe.
async fn fetch_places(key: TileKey, template: String) -> Vec<Place> {
    let Ok(bytes) = crate::systems::tile_loader::load_tile_image(&key, &template).await else {
        return Vec::new();
    };
    crate::platform::workers::compute("place_names", move || {
        let Ok(tile) = crate::systems::tile_loader::parse_shared(key, &bytes) else {
            return Vec::new();
        };
        crate::systems::vector_tiles::extract_place_labels(&tile)
            .into_iter()
            .map(|l: PlaceLabel| {
                let (lat, lng) = cartopolis_core::world::tile_frac_to_lat_lng(
                    key.x as f64 + f64::from(l.at[0]),
                    key.y as f64 + f64::from(l.at[1]),
                    key.z,
                );
                Place {
                    name: l.name,
                    kind: l.kind,
                    population: l.population,
                    lat,
                    lng,
                }
            })
            .collect()
    })
    .await
    .unwrap_or_default()
}

/// Whether a place of `kind` is named from `distance_m` away.
fn shows(kind: PlaceKind, population: u32, distance_m: f64) -> bool {
    match kind {
        PlaceKind::City => true,
        // A small town only up close; a big one as far as a city.
        PlaceKind::Town => population >= 40_000 || distance_m < 35_000.0,
        PlaceKind::Suburb => distance_m < 150_000.0,
        PlaceKind::Quarter => distance_m < 15_000.0,
        PlaceKind::Neighbourhood => distance_m < 6_000.0,
        PlaceKind::Station => distance_m < 15_000.0,
    }
}

/// "296k", "1.2M".
fn population_label(n: u32) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", f64::from(n) / 1e6)
    } else {
        format!("{}k", (f64::from(n) / 1e3).round())
    }
}

/// Paint the names.
#[allow(clippy::too_many_arguments)]
pub fn render_place_names(
    mut egui: EguiMut,
    names: Res<PlaceNames>,
    globe: Res<GlobeState>,
    camera: Query<(&Camera, &GridCell, &Transform), With<FreeCamera>>,
    terrain: Res<crate::systems::terrain::TerrainField>,
    map_anchor: Res<MapAnchor>,
    keyboard: Res<crate::systems::onscreen_keyboard::OnScreenKeyboard>,
    route: Res<crate::systems::navigation::RouteState>,
    layers: Res<crate::systems::data_layers::DataLayers>,
    driver: Res<crate::systems::driver::DriverState>,
) {
    let mut anchored = WorldAnchored::new(egui.ctx(), "place_names");
    if driver.active || !layers.place_names || names.tiles.is_empty() {
        return;
    }
    let Some(anchor) = map_anchor.geo else {
        return;
    };
    let Ok((cam, cell, tf)) = camera.single() else {
        return;
    };
    let alt = globe.altitude_m(cell, tf);
    if !(MIN_ALTITUDE_M..MAX_ALTITUDE_M).contains(&alt) {
        return;
    }
    let map_rect = crate::systems::place_pins::map_area(&mut egui, &keyboard, &route);
    let ui_scale = egui.ui_scale();
    let ctx = egui.ctx_mut().clone();
    let cam_world = crate::grid::anchored_position_f64(cell, tf.translation);
    let in_space = globe.in_space(cell, tf);
    // This frame's camera basis, as the pins rebuild it
    // (`docs/notes/screen-overlay-projection.md`).
    let cam_view = GlobalTransform::from(Transform::from_rotation(tf.rotation));

    // Every place once (a town is in several zooms' tiles), nearest tile's copy.
    let mut seen: HashSet<(&str, PlaceKind)> = HashSet::new();
    let mut places: Vec<(&Place, egui::Pos2)> = Vec::new();
    let mut towns: Vec<&str> = Vec::new();
    for tile in names.tiles.values() {
        for p in tile {
            if !seen.insert((p.name.as_str(), p.kind)) {
                continue;
            }
            if matches!(p.kind, PlaceKind::City | PlaceKind::Town) {
                towns.push(p.name.as_str());
            }
            let Some(delta) = crate::systems::place_pins::place_delta(
                &globe, anchor, cam_world, in_space, p.lat, p.lng, &terrain,
            ) else {
                continue;
            };
            if !shows(p.kind, p.population, delta.length()) {
                continue;
            }
            let Ok(screen) = cam.world_to_viewport(&cam_view, delta.as_vec3()) else {
                continue;
            };
            let at = egui::pos2(screen.x / ui_scale, screen.y / ui_scale);
            if map_rect.shrink(8.0).contains(at) {
                places.push((p, at));
            }
        }
    }
    places.sort_by(|a, b| {
        a.0.kind
            .cmp(&b.0.kind)
            .then(b.0.population.cmp(&a.0.population))
            .then(a.0.name.cmp(&b.0.name))
    });

    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Middle,
        egui::Id::new("place_names"),
    ));
    let painter = painter.with_clip_rect(map_rect);
    let mut taken: Vec<egui::Rect> = Vec::new();
    let mut free = |r: egui::Rect| {
        if taken.iter().any(|t| t.intersects(r)) {
            false
        } else {
            taken.push(r);
            true
        }
    };
    for (p, at) in places {
        match p.kind {
            PlaceKind::City | PlaceKind::Town => {
                if banner(&painter, at, p, &mut free) {
                    anchored.at(at);
                }
            }
            PlaceKind::Station => {
                let named = alt < 20_000.0 && towns.iter().any(|t| p.name.starts_with(t));
                if station(&painter, at, p, named, &mut free) {
                    anchored.at(at);
                }
            }
            PlaceKind::Suburb | PlaceKind::Quarter | PlaceKind::Neighbourhood => {
                if district(&painter, at, p, &mut free) {
                    anchored.at(at);
                }
            }
        }
    }
}

/// The dark pill a banner and a station share.
const CHIP: egui::Color32 = theme::LABEL_CHIP;

/// A city or town: a dot where it is, and over it a pill with the population
/// and the name.
fn banner(
    painter: &egui::Painter,
    at: egui::Pos2,
    p: &Place,
    free: &mut impl FnMut(egui::Rect) -> bool,
) -> bool {
    let big = p.kind == PlaceKind::City;
    let name_size = if big { 13.5 } else { 12.0 };
    let font = egui::FontId::new(name_size, theme::brand_family());
    let name = painter.layout_no_wrap(p.name.clone(), font, theme::CREAM);
    let pop = (p.population > 0).then(|| {
        painter.layout_no_wrap(
            population_label(p.population),
            egui::FontId::new(name_size - 2.5, theme::brand_family()),
            theme::INK,
        )
    });
    let h = name_size + 9.5;
    let pop_w = pop.as_ref().map_or(0.0, |g| g.size().x + 12.0);
    let w = pop_w + name.size().x + if pop.is_some() { 15.0 } else { 18.0 };
    let rect =
        egui::Rect::from_min_size(egui::pos2(at.x - w / 2.0, at.y - h - 8.0), egui::vec2(w, h));
    if !free(rect.union(egui::Rect::from_center_size(at, egui::vec2(10.0, 10.0)))) {
        return false;
    }
    painter.circle(at, 4.0, theme::CREAM, egui::Stroke::new(1.5, theme::INK));
    painter.rect(
        rect,
        egui::CornerRadius::same(theme::RADIUS_PILL),
        CHIP,
        egui::Stroke::new(1.0, theme::OVERLAY_LINE),
        egui::StrokeKind::Inside,
    );
    let mut x = rect.min.x + 3.0;
    if let Some(pop) = pop {
        let badge =
            egui::Rect::from_min_size(egui::pos2(x, rect.min.y + 3.0), egui::vec2(pop_w, h - 6.0));
        painter.rect_filled(
            badge,
            egui::CornerRadius::same(theme::RADIUS_PILL),
            theme::ACCENT,
        );
        painter.galley(badge.center() - pop.size() / 2.0, pop, theme::INK);
        x = badge.max.x + 6.0;
    } else {
        x += 6.0;
    }
    let y = rect.center().y - name.size().y / 2.0;
    painter.galley(egui::pos2(x, y), name, theme::CREAM);
    true
}

/// A station: a dark disc with a ring and a train in it, and its name beside
/// it when it is a town's own station.
fn station(
    painter: &egui::Painter,
    at: egui::Pos2,
    p: &Place,
    named: bool,
    free: &mut impl FnMut(egui::Rect) -> bool,
) -> bool {
    let r = 9.0;
    let name = named.then(|| {
        painter.layout_no_wrap(
            p.name.clone(),
            egui::FontId::new(11.5, theme::brand_family()),
            theme::CREAM,
        )
    });
    let w = name
        .as_ref()
        .map_or(2.0 * r, |g| 2.0 * r + 4.0 + g.size().x);
    let rect = egui::Rect::from_min_size(egui::pos2(at.x - r, at.y - r), egui::vec2(w, 2.0 * r));
    if !free(rect) {
        return false;
    }
    painter.circle(at, r, CHIP, egui::Stroke::new(1.6, theme::ACCENT));
    // The train: a body, a window line, two wheels.
    let s = egui::Stroke::new(1.3, theme::CREAM);
    let body = egui::Rect::from_min_max(at + egui::vec2(-3.5, -4.0), at + egui::vec2(3.5, 2.0));
    painter.rect_stroke(body, 1.0, s, egui::StrokeKind::Middle);
    painter.line_segment([at + egui::vec2(-3.5, 0.0), at + egui::vec2(3.5, 0.0)], s);
    painter.line_segment([at + egui::vec2(-2.0, 4.0), at + egui::vec2(-3.5, 6.0)], s);
    painter.line_segment([at + egui::vec2(2.0, 4.0), at + egui::vec2(3.5, 6.0)], s);
    if let Some(name) = name {
        let pos = egui::pos2(at.x + r + 4.0, at.y - name.size().y / 2.0);
        haloed(painter, pos, name);
    }
    true
}

/// A district: its name in spaced capitals, centred where it is.
fn district(
    painter: &egui::Painter,
    at: egui::Pos2,
    p: &Place,
    free: &mut impl FnMut(egui::Rect) -> bool,
) -> bool {
    let size = if p.kind == PlaceKind::Neighbourhood {
        9.5
    } else {
        10.5
    };
    let mut job = egui::text::LayoutJob::default();
    job.append(
        &p.name.to_uppercase(),
        0.0,
        egui::TextFormat {
            font_id: egui::FontId::new(size, theme::brand_family()),
            color: theme::CREAM,
            extra_letter_spacing: 1.4,
            ..Default::default()
        },
    );
    let galley = painter.layout_job(job);
    let rect = egui::Rect::from_center_size(at, galley.size());
    // Wide room around a district name: a town is not a word search.
    if !free(rect.expand2(egui::vec2(24.0, 10.0))) {
        return false;
    }
    haloed(painter, rect.min, galley);
    true
}

/// Text with a dark halo, so it reads over roofs, water and fields alike.
fn haloed(painter: &egui::Painter, pos: egui::Pos2, galley: std::sync::Arc<egui::Galley>) {
    let halo = egui::Color32::from_rgba_unmultiplied(0x1A, 0x22, 0x24, 200);
    for d in [
        egui::vec2(-1.2, 0.0),
        egui::vec2(1.2, 0.0),
        egui::vec2(0.0, -1.2),
        egui::vec2(0.0, 1.2),
        egui::vec2(-0.9, -0.9),
        egui::vec2(0.9, 0.9),
        egui::vec2(-0.9, 0.9),
        egui::vec2(0.9, -0.9),
    ] {
        painter.galley_with_override_text_color(pos + d, galley.clone(), halo);
    }
    painter.galley(pos, galley, theme::CREAM);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn populations_read_short() {
        assert_eq!(population_label(295_591), "296k");
        assert_eq!(population_label(1_150_000), "1.1M");
    }

    #[test]
    fn small_towns_give_way_with_distance() {
        assert!(shows(PlaceKind::Town, 20_000, 10_000.0));
        assert!(!shows(PlaceKind::Town, 20_000, 60_000.0));
        assert!(shows(PlaceKind::Town, 60_000, 60_000.0));
        assert!(shows(PlaceKind::City, 0, 200_000.0));
        assert!(!shows(PlaceKind::Neighbourhood, 0, 10_000.0));
    }
}
