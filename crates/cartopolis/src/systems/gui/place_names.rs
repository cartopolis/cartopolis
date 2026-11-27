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
//!
//! **Countries from the globe up.** Over [`COUNTRY_MIN_ALTITUDE_M`] the towns
//! give way to the countries, from a table embedded in the client
//! (`cartopolis_geo::countries`), each named once the camera is under the
//! height its size earns ([`country_ceiling_m`]).
//!
//! **A name is a button.** Clicking or long-pressing one opens the map's ring of
//! actions on the place (`ui::drive_context_menu`): Go there, Route here and
//! Lenses, which centres the place and puts up the shelf of lenses.
//!
//! **Placed here, drawn by the library.** Which names show, where, and what
//! each looks like is decided here; the component library draws them
//! (`systems::lib_place_names`) from [`PlaceNames::lib_view`], and its taps
//! come back through [`PlaceNames::lib_tapped`] to open the ring.

use crate::components::FreeCamera;
use crate::resources::{ContextMenuState, MapAnchor, MapPlace};
use crate::systems::globe::GlobeState;
use crate::systems::place_pins::{text_size, Face};
use crate::systems::ui_input::{UiInput, UiMetrics};
use crate::systems::vector_tiles::{PlaceKind, PlaceLabel};
use bevy::prelude::*;
use bevy::tasks::{IoTaskPool, Task};
use big_space::prelude::GridCell;
use cartopolis_core::TileKey;
use std::collections::{HashMap, HashSet};

/// Under this camera height no place names are drawn: the street names on the
/// ground are the map's text there.
const MIN_ALTITUDE_M: f64 = 1_500.0;

/// Over this camera height no town is named: past the flat map the countries
/// take over ([`COUNTRY_MIN_ALTITUDE_M`]).
const MAX_ALTITUDE_M: f64 = 250_000.0;

/// From this camera height up the globe names countries instead of towns.
const COUNTRY_MIN_ALTITUDE_M: f64 = MAX_ALTITUDE_M;

/// The camera height a country whose `min_label` is 0 would be named from;
/// each zoom of `min_label` halves it, as a web map halves its scale. Russia
/// and France (1.7) are named from ~18,500 km, the Netherlands (4) from
/// 3,750 km, Luxembourg (5.7) from 1,150 km. At 80,000 km the whole globe
/// carried some 25 names and looked odd (asked 2026-11-18); now about a dozen.
const COUNTRY_SCALE_M: f64 = 60_000_000.0;

/// How far past the sunset line, as the cosine of the sun's angle, a
/// country is still named: a little into the dusk, where the city lights are.
const NIGHT_MARGIN: f32 = -0.1;

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
    /// A country's `min_label` (`cartopolis_geo::countries`); 0 for the rest.
    min_label: f32,
}

/// The countries, as places, once.
fn countries() -> &'static [Place] {
    static ALL: std::sync::OnceLock<Vec<Place>> = std::sync::OnceLock::new();
    ALL.get_or_init(|| {
        cartopolis_geo::countries::all()
            .iter()
            .map(|c| Place {
                name: c.name.clone(),
                kind: PlaceKind::Country,
                population: u32::try_from(c.population).unwrap_or(u32::MAX),
                lat: c.lat,
                lng: c.lng,
                min_label: c.min_label,
            })
            .collect()
    })
}

/// Whether ground whose outward normal is `normal` is on the globe's lit side
/// as it is drawn (`globe::update_globe_night`): a name over the night side
/// floated in the black. With the light set to Dark, which is night all round,
/// and with the night side switched off, everything is lit.
fn sunlit(night: &crate::systems::globe::GlobeNight, normal: Vec3) -> bool {
    // `terminator.w` is the light setting: 1 Light, whose line is where the
    // real sun sets; 2 Dark; 0 the clock, whose night is `night`.
    let mode = night.terminator.w.round() as i32;
    let sun = if mode == 1 {
        night.terminator.truncate()
    } else if mode == 0 && night.night.w > 0.0 {
        night.night.truncate()
    } else {
        return true;
    };
    normal.dot(sun) > NIGHT_MARGIN
}

/// The highest camera a country is named from.
fn country_ceiling_m(min_label: f32) -> f64 {
    COUNTRY_SCALE_M / 2f64.powf(f64::from(min_label))
}

/// The names read from the tiles around the camera.
#[derive(Resource, Default)]
pub struct PlaceNames {
    tiles: HashMap<TileKey, Vec<Place>>,
    pending: HashMap<TileKey, Task<Vec<Place>>>,
    /// Tiles that came back empty or failed, so they are not asked again while
    /// they stay wanted.
    empty: HashSet<TileKey>,
    /// The names the library draws this frame (`lib_place_names`), written
    /// by [`lay_out_place_names`].
    pub lib_view: Vec<NameItem>,
    /// The key of the name the library's tap landed on ([`NameItem::key`]),
    /// opened next frame by [`lay_out_place_names`].
    pub lib_tapped: Option<String>,
}

/// One name where it stands this frame, for the library.
#[cfg_attr(not(feature = "ui-lib"), allow(dead_code))]
#[derive(Clone, Debug)]
pub struct NameItem {
    /// The place's kind and name: what the library keys its node by.
    pub key: String,
    /// The place's point, in UI points (the library's px natively).
    pub at: Vec2,
    pub look: NameLook,
}

/// What a name looks like, its geometry relative to its point as measured
/// here: everything about it that does not move with the camera.
#[derive(Clone, Debug, PartialEq)]
pub enum NameLook {
    /// A country, city or town: the pill (left, top, width, height from the
    /// point), the population's badge in it, and the dot a town has.
    Banner {
        name: String,
        size: f32,
        population: Option<String>,
        pill: [f32; 4],
        badge: f32,
        dot: bool,
    },
    /// A station's disc, and its name beside it when it is a town's own,
    /// with the width and height the name takes.
    Station {
        name: Option<String>,
        width: f32,
        height: f32,
    },
    /// A district in spaced capitals, centred on its point: its size, and
    /// the width and height it takes.
    District {
        text: String,
        size: f32,
        width: f32,
        height: f32,
    },
}

/// A place's key in the library's list: one per name and kind.
fn name_key(p: &Place) -> String {
    format!("{:?}:{}", p.kind, p.name)
}

/// Keep the tiles the names come from around the camera.
pub fn stream_place_names(
    mut names: ResMut<PlaceNames>,
    map_anchor: Res<MapAnchor>,
    globe: Res<GlobeState>,
    camera: Query<(&GridCell, &Transform), With<FreeCamera>>,
    layers: Res<crate::systems::layer_state::DataLayers>,
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
                    min_label: 0.0,
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
        // Named by camera height, not distance ([`country_ceiling_m`]).
        PlaceKind::Country => true,
        PlaceKind::City => true,
        // A small town only up close; a big one as far as a city.
        PlaceKind::Town => population >= 40_000 || distance_m < 35_000.0,
        PlaceKind::Suburb => distance_m < 150_000.0,
        PlaceKind::Quarter => distance_m < 15_000.0,
        PlaceKind::Neighbourhood => distance_m < 6_000.0,
        PlaceKind::Station => distance_m < 15_000.0,
    }
}

/// "296k", "1.2M", "1.4B".
fn population_label(n: u32) -> String {
    if n >= 1_000_000_000 {
        format!("{:.1}B", f64::from(n) / 1e9)
    } else if n >= 1_000_000 {
        format!("{:.1}M", f64::from(n) / 1e6)
    } else {
        format!("{}k", (f64::from(n) / 1e3).round())
    }
}

/// Choose, measure and place the names for the library, open the ring on
/// the one tapped, and fly the ring's Go there.
#[allow(clippy::too_many_arguments)]
pub fn lay_out_place_names(
    mut names: ResMut<PlaceNames>,
    globe: Res<GlobeState>,
    camera: Query<(&Camera, &GridCell, &Transform), With<FreeCamera>>,
    terrain: Res<crate::systems::terrain::TerrainField>,
    map_anchor: Res<MapAnchor>,
    keyboard: Res<crate::systems::onscreen_keyboard::OnScreenKeyboard>,
    lens: Res<crate::systems::scenario::LensSlot>,
    route: Res<crate::systems::navigation::RouteState>,
    layers: Res<crate::systems::layer_state::DataLayers>,
    driver: Res<crate::systems::driver::DriverState>,
    // A name that is clicked opens the map's ring on it (`ui::drive_context_menu`),
    // and the ring's Go there comes back here to be flown.
    mut menu: ResMut<ContextMenuState>,
    mut nav: ResMut<crate::systems::geo_nav::GeoNavState>,
    mut lenses: ResMut<crate::systems::scenario::ScenarioView>,
    // One parameter for five, under Bevy's cap of sixteen. The chrome is the
    // map's buttons, which a name keeps off (`place_pins::clear_of_chrome`);
    // the input and metrics say where the panels left the map
    // (`place_pins::map_area`).
    (night, room, chrome, input, metrics): (
        Res<crate::systems::globe::GlobeNight>,
        Res<crate::systems::design_room::DesignRoom>,
        Res<crate::systems::dock::MapChrome>,
        Res<UiInput>,
        Res<UiMetrics>,
    ),
) {
    if let Some((lat, lng, altitude, name)) = menu.go_there.take() {
        nav.pending_teleport = Some(crate::systems::geo_nav::Teleport::geo(
            lat,
            lng,
            Some(altitude),
        ));
        nav.pending_label = Some(name);
    }
    // Lenses: the place goes under the crosshair, which is the "here" a lens
    // reads, at the height the map is already at; then the shelf comes up.
    if let Some((lat, lng, name)) = menu.lenses_at.take() {
        nav.pending_teleport = Some(crate::systems::geo_nav::Teleport::geo(lat, lng, None));
        nav.pending_label = Some(name);
        crate::systems::scenario::open_shelf(&mut lenses);
    }
    // Cleared first: an early return below leaves the library no names,
    // which reads as them going.
    names.lib_view.clear();
    let tapped_key = names.lib_tapped.take();
    let mut lib_view: Vec<NameItem> = Vec::new();
    if driver.active || !layers.place_names || room.spot.is_some() {
        return;
    }
    let Some(anchor) = map_anchor.geo else {
        return;
    };
    let Ok((cam, cell, tf)) = camera.single() else {
        return;
    };
    let alt = globe.altitude_m(cell, tf);
    // Towns from the tiles below the globe, countries from the table above.
    let (sources, min_label): (Vec<&[Place]>, f32) = if alt >= COUNTRY_MIN_ALTITUDE_M {
        let ceiling = (COUNTRY_SCALE_M / alt).log2() as f32;
        (vec![countries()], ceiling)
    } else if alt >= MIN_ALTITUDE_M {
        (
            names.tiles.values().map(Vec::as_slice).collect(),
            f32::INFINITY,
        )
    } else {
        return;
    };
    let map_rect = crate::systems::place_pins::map_area(&input, &metrics, &keyboard, &route, &lens);
    let ui_scale = metrics.ui_scale();
    let cam_world = crate::grid::anchored_position_f64(cell, tf.translation);
    let in_space = globe.in_space(cell, tf);
    // This frame's camera basis, as the pins rebuild it
    // (`docs/notes/screen-overlay-projection.md`).
    let cam_view = GlobalTransform::from(Transform::from_rotation(tf.rotation));

    // Every place once (a town is in several zooms' tiles), nearest tile's copy.
    let mut seen: HashSet<(&str, PlaceKind)> = HashSet::new();
    let mut places: Vec<(&Place, Vec2)> = Vec::new();
    let mut towns: Vec<&str> = Vec::new();
    for tile in sources {
        for p in tile {
            if p.kind == PlaceKind::Country
                && (p.min_label > min_label || !sunlit(&night, globe.direction_local(p.lat, p.lng)))
            {
                continue;
            }
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
            let at = metrics.window_to_ui(screen);
            if map_rect.inflate(-8.0).contains(at)
                && crate::systems::place_pins::clear_of_chrome(&chrome.holes, at)
            {
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

    let mut taken: Vec<Rect> = Vec::new();
    let mut free = |r: Rect| {
        if taken.iter().any(|t| overlaps(*t, r)) {
            false
        } else {
            taken.push(r);
            true
        }
    };
    let mut opened: Option<(&Place, Vec2)> = None;
    for (p, at) in places {
        let look = match p.kind {
            PlaceKind::Country | PlaceKind::City | PlaceKind::Town => banner(at, p, &mut free),
            PlaceKind::Station => {
                let named = alt < 20_000.0 && towns.iter().any(|t| p.name.starts_with(t));
                station(at, p, named, &mut free)
            }
            PlaceKind::Suburb | PlaceKind::Quarter | PlaceKind::Neighbourhood => {
                district(at, p, &mut free)
            }
        };
        let Some(look) = look else {
            continue;
        };
        let key = name_key(p);
        if tapped_key.as_deref() == Some(key.as_str()) {
            opened = Some((p, at));
        }
        lib_view.push(NameItem { key, at, look });
    }
    if let Some((p, at)) = opened {
        menu.visible = true;
        // The ring stands on the place's dot, not wherever the pointer was.
        menu.screen_pos = Vec2::new(at.x * ui_scale, at.y * ui_scale);
        menu.hit_prim_id = None;
        menu.lat_lng = Some((p.lat, p.lng));
        menu.place = Some(MapPlace {
            name: p.name.clone(),
            detail: describe(p),
            view_altitude_m: view_altitude_m(p),
        });
    }
    names.lib_view = lib_view;
}

/// Whether two rects touch or overlap, edges included.
fn overlaps(a: Rect, b: Rect) -> bool {
    a.min.x <= b.max.x && b.min.x <= a.max.x && a.min.y <= b.max.y && b.min.y <= a.max.y
}

/// `r` grown by `by` on each side, across and down.
fn grown(r: Rect, by: Vec2) -> Rect {
    Rect::from_corners(r.min - by, r.max + by)
}

/// A rect from its top-left corner and size.
fn at_size(min: Vec2, size: Vec2) -> Rect {
    Rect::from_corners(min, min + size)
}

/// The pill under the ring: what the place is, and how many live there.
fn describe(p: &Place) -> String {
    let kind = match p.kind {
        PlaceKind::Country => "Country",
        PlaceKind::City => "City",
        PlaceKind::Town => "Town",
        PlaceKind::Suburb => "District",
        PlaceKind::Quarter => "Quarter",
        PlaceKind::Neighbourhood => "Neighbourhood",
        PlaceKind::Station => "Station",
    };
    if p.population > 0 {
        format!("{kind} · {} inhabitants", population_label(p.population))
    } else {
        kind.to_string()
    }
}

/// How high Go there leaves the camera: the whole of a country or a city in
/// view, a station's square in front of it.
fn view_altitude_m(p: &Place) -> f64 {
    match p.kind {
        // Under the height its name is written from, so the land fills the view.
        PlaceKind::Country => (country_ceiling_m(p.min_label) * 0.4).max(400_000.0),
        PlaceKind::City => 9_000.0,
        PlaceKind::Town => 4_500.0,
        PlaceKind::Suburb => 2_500.0,
        PlaceKind::Quarter => 1_500.0,
        PlaceKind::Neighbourhood => 900.0,
        PlaceKind::Station => 400.0,
    }
}

/// A city or town: a dot where it is, and over it a pill with the population
/// and the name. A country is the pill alone, a size up, centred on its
/// label point: it has no one spot a dot could mark. `None` when it has no
/// room.
fn banner(at: Vec2, p: &Place, free: &mut impl FnMut(Rect) -> bool) -> Option<NameLook> {
    let country = p.kind == PlaceKind::Country;
    let name_size = match p.kind {
        PlaceKind::Country => 15.0,
        PlaceKind::City => 13.5,
        _ => 12.0,
    };
    let name_w = text_size(&p.name, name_size, Face::Brand, 0.0).x;
    let pop = (p.population > 0).then(|| population_label(p.population));
    let h = name_size + 9.5;
    let pop_w = pop.as_ref().map_or(0.0, |label| {
        text_size(label, name_size - 2.5, Face::Brand, 0.0).x + 12.0
    });
    let w = pop_w + name_w + if pop.is_some() { 15.0 } else { 18.0 };
    // The pill relative to the point, so the look is bit-identical wherever
    // the point is: a round trip through screen coordinates differs in the
    // last bit as the map moves, and a look that differs rebuilds the
    // banner, which blanks it for a frame.
    let (left, top) = if country {
        (-w / 2.0, -h / 2.0)
    } else {
        (-w / 2.0, -h - 8.0)
    };
    let rect = at_size(at + Vec2::new(left, top), Vec2::new(w, h));
    let hit = if country {
        rect
    } else {
        rect.union(Rect::from_center_size(at, Vec2::splat(10.0)))
    };
    if !free(hit) {
        return None;
    }
    Some(NameLook::Banner {
        name: p.name.clone(),
        size: name_size,
        population: pop,
        pill: [left, top, w, h],
        badge: pop_w,
        dot: !country,
    })
}

/// A station: a dark disc with a ring and a train in it, and its name beside
/// it when it is a town's own station.
fn station(
    at: Vec2,
    p: &Place,
    named: bool,
    free: &mut impl FnMut(Rect) -> bool,
) -> Option<NameLook> {
    let r = 9.0;
    let size = if named {
        text_size(&p.name, 11.5, Face::Brand, 0.0)
    } else {
        Vec2::ZERO
    };
    let w = if named {
        2.0 * r + 4.0 + size.x
    } else {
        2.0 * r
    };
    let rect = at_size(Vec2::new(at.x - r, at.y - r), Vec2::new(w, 2.0 * r));
    if !free(rect) {
        return None;
    }
    Some(NameLook::Station {
        name: named.then(|| p.name.clone()),
        width: size.x,
        height: size.y,
    })
}

/// A district: its name in spaced capitals, centred where it is.
fn district(at: Vec2, p: &Place, free: &mut impl FnMut(Rect) -> bool) -> Option<NameLook> {
    let size = if p.kind == PlaceKind::Neighbourhood {
        9.5
    } else {
        10.5
    };
    let text = p.name.to_uppercase();
    let measured = text_size(&text, size, Face::Brand, 1.4);
    let rect = Rect::from_center_size(at, measured);
    // Wide room around a district name: a town is not a word search.
    if !free(grown(rect, Vec2::new(24.0, 10.0))) {
        return None;
    }
    // The measured size, not the rect's: that wobbles in the last bit with
    // `at`, and a look that differs rebuilds the name (see `banner`).
    Some(NameLook::District {
        text,
        size,
        width: measured.x,
        height: measured.y,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn populations_read_short() {
        assert_eq!(population_label(295_591), "296k");
        assert_eq!(population_label(1_150_000), "1.1M");
        assert_eq!(population_label(1_397_715_000), "1.4B");
    }

    #[test]
    fn small_towns_give_way_with_distance() {
        assert!(shows(PlaceKind::Town, 20_000, 10_000.0));
        assert!(!shows(PlaceKind::Town, 20_000, 60_000.0));
        assert!(shows(PlaceKind::Town, 60_000, 60_000.0));
        assert!(shows(PlaceKind::City, 0, 200_000.0));
        assert!(!shows(PlaceKind::Neighbourhood, 0, 10_000.0));
    }

    #[test]
    fn big_countries_are_named_from_further_out() {
        // France from the whole hemisphere, the Netherlands over Europe,
        // Luxembourg only over the Low Countries.
        assert!(country_ceiling_m(1.7) > 15_000_000.0);
        assert!((3_500_000.0..4_000_000.0).contains(&country_ceiling_m(4.0)));
        assert!(country_ceiling_m(5.7) < 1_500_000.0);
        let nl = countries()
            .iter()
            .find(|c| c.name == "Netherlands")
            .unwrap();
        assert_eq!(describe(nl), "Country · 17.3M inhabitants");
    }

    #[test]
    fn a_names_look_does_not_move_with_it() {
        // A look that differs rebuilds the name, which blanks it a frame: it
        // must be the same wherever on screen the place is.
        fn free(_: Rect) -> bool {
            true
        }
        for kind in [PlaceKind::Country, PlaceKind::City, PlaceKind::Suburb] {
            let p = Place {
                name: "Groningen".into(),
                kind,
                population: 233_000,
                lat: 53.22,
                lng: 6.57,
                min_label: 0.0,
            };
            let look = |at| match kind {
                PlaceKind::Suburb => district(at, &p, &mut free),
                _ => banner(at, &p, &mut free),
            };
            let a = look(Vec2::new(413.7, 201.3));
            assert!(a.is_some());
            for at in [Vec2::new(413.9, 201.31), Vec2::new(1771.137, 903.77)] {
                assert_eq!(a, look(at), "{kind:?} at {at}");
            }
        }
    }

    #[test]
    fn the_ring_says_what_the_place_is() {
        let place = |kind, population| Place {
            name: "Groningen".into(),
            kind,
            population,
            lat: 53.22,
            lng: 6.57,
            min_label: 0.0,
        };
        assert_eq!(
            describe(&place(PlaceKind::City, 233_000)),
            "City · 233k inhabitants"
        );
        assert_eq!(describe(&place(PlaceKind::Station, 0)), "Station");
        assert!(
            view_altitude_m(&place(PlaceKind::City, 0))
                > view_altitude_m(&place(PlaceKind::Station, 0))
        );
    }
}
