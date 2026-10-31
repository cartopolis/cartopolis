//! **Scenario layers** over the map: a lens with a clock
//! (`docs/mockups/scenarios/`, `docs/notes/scenarios.md`). The engine is
//! `cartopolis_scenario`, which says what is true at a time on which board; this
//! draws one layer of it, and nothing here knows what a figure or a drone is.
//!
//! **One layer, every rung.** A layer stands on boards at several rungs of the
//! ladder (CBS figures per buurt, wijk, gemeente, provincie and country); the
//! camera's height picks the rung (`ladder::Rung::at_altitude`) and the engine
//! the nearest board it has, so zooming out turns buurten into wijken into
//! gemeenten without a switch.
//!
//! **The areas come from our host**, `{CARTO_3DBAG_URL}/areas/<set>.bin`, as
//! `atlas areas` writes them, fetched and decoded off the frame loop when a
//! layer is opened. The layer files are shipped (`assets/scenarios/`).
//!
//! **Painted, not meshed**, as the place names are (`place_names`'s module
//! note): an area's state has to read from a buurt's height to a continent's,
//! so it is drawn in screen space over whatever tier the map is in, projected
//! through [`place_pins::place_delta`] so the flat map and the globe agree.
//!
//! The frame says how real it is (v4-whole): published figures and replays in
//! green, an exercise striped yellow, Live red and from an official source only.
//! Opened from Layers ▸ Map ▸ Figures (`DataLayers::scenario`, `"<file>/<layer>"`),
//! or for a capture with `CARTO_SCENARIO=cbs-2024/share_65_plus`.

use crate::components::FreeCamera;
use crate::resources::MapAnchor;
use crate::systems::egui_manager::{EguiMut, WorldAnchored};
use crate::systems::globe::GlobeState;
use crate::systems::place_pins;
use crate::systems::theme::{self, scenario as ink};
use bevy::prelude::*;
use bevy::tasks::{IoTaskPool, Task};
use big_space::prelude::GridCell;
use cartopolis_scenario::board::{AreaSet, Board, LonLat};
use cartopolis_scenario::ladder::Rung;
use cartopolis_scenario::region::When;
use cartopolis_scenario::time::Time as Clock;
use cartopolis_scenario::{track, Confidence, Frame, Mode, Play, Scenario};
use std::collections::HashMap;

/// How far ahead a track's path and cone are drawn.
const AHEAD: Clock = Clock(150 * 60);

/// Triangulated areas kept at most; past it the cache starts again.
const MAX_FILLS: usize = 6_000;

/// A scenario file shipped with the client.
struct Shipped {
    id: &'static str,
    toml: &'static str,
}

const SHIPPED: &[Shipped] = &[Shipped {
    id: "cbs-2024",
    toml: include_str!("../../../../../assets/scenarios/cbs-2024.toml"),
}];

/// A layer the Layers panel offers: its key (`"<file>/<layer>"`) and label.
pub struct Offered {
    pub key: String,
    pub label: String,
}

/// Every layer of every shipped scenario, for the Layers panel.
pub fn offered() -> &'static [Offered] {
    static OFFERED: std::sync::OnceLock<Vec<Offered>> = std::sync::OnceLock::new();
    OFFERED.get_or_init(|| {
        let mut out = Vec::new();
        for s in SHIPPED {
            match Scenario::parse(s.toml) {
                Ok(sc) => out.extend(sc.region_states.iter().map(|r| Offered {
                    key: format!("{}/{}", s.id, r.id),
                    label: r.label.clone(),
                })),
                Err(e) => warn!("scenario {}: {e}", s.id),
            }
        }
        out
    })
}

/// The open layer.
struct Open {
    key: String,
    play: Play,
    /// Which region-state layer of the scenario is shown.
    layer: usize,
    t: Clock,
    /// The clock's fraction of a second carried between frames.
    carry: f64,
    playing: bool,
    speed: f64,
    frame: Frame,
    /// Triangles per (area set, area), made when an area first shows.
    fills: HashMap<(usize, usize), (Vec<LonLat>, Vec<u32>)>,
    /// The track whose card is open.
    selected: Option<usize>,
}

/// A layer whose areas are still on their way.
struct Loading {
    key: String,
    scenario: Scenario,
    layer: usize,
    tasks: Vec<Task<Result<AreaSet, String>>>,
    sets: Vec<AreaSet>,
}

/// The scenario layer on screen, if any.
#[derive(Resource, Default)]
pub struct ScenarioView {
    open: Option<Open>,
    loading: Option<Loading>,
    /// Why the last one would not open.
    error: Option<(String, String)>,
}

impl ScenarioView {
    fn key(&self) -> Option<&str> {
        self.open
            .as_ref()
            .map(|o| o.key.as_str())
            .or(self.loading.as_ref().map(|l| l.key.as_str()))
            .or(self.error.as_ref().map(|e| e.0.as_str()))
    }
}

/// One area set, fetched from our host and decoded off the frame loop.
async fn fetch_areas(name: String) -> Result<AreaSet, String> {
    let base = crate::systems::osm_buildings::tdbag_url();
    let url = format!("{}/areas/{name}.bin", base.trim_end_matches('/'));
    let bytes = crate::platform::http::fetch(crate::platform::http::Request::get(url)).await?;
    AreaSet::decode(&bytes).map_err(|e| format!("{name}: {e}"))
}

fn start(key: &str) -> Result<Loading, String> {
    let (file, layer_id) = key
        .split_once('/')
        .ok_or_else(|| format!("{key:?}: expected <file>/<layer>"))?;
    let shipped = SHIPPED
        .iter()
        .find(|s| s.id == file)
        .ok_or_else(|| format!("no scenario {file:?}"))?;
    let scenario = Scenario::parse(shipped.toml).map_err(|e| e.to_string())?;
    let layer = scenario
        .region_states
        .iter()
        .position(|r| r.id == layer_id)
        .ok_or_else(|| format!("{file:?} has no layer {layer_id:?}"))?;
    let tasks = scenario
        .boards()
        .into_iter()
        .filter_map(|b| match b {
            Board::Areas(name) => Some(IoTaskPool::get().spawn(fetch_areas(name))),
            _ => None,
        })
        .collect();
    Ok(Loading {
        key: key.to_owned(),
        scenario,
        layer,
        tasks,
        sets: Vec::new(),
    })
}

/// Open what the Layers panel asks for, load its areas, follow the camera's
/// rung, and run the clock.
pub fn drive_scenario(
    layers: Res<crate::systems::data_layers::DataLayers>,
    mut view: ResMut<ScenarioView>,
    time: Res<Time>,
    globe: Res<GlobeState>,
    camera: Query<(&GridCell, &Transform), With<FreeCamera>>,
) {
    let wanted = layers.scenario.as_deref();
    if view.key() != wanted {
        // Only on a change, so this is false on every frame but the switch's.
        *view = ScenarioView::default();
        if let Some(key) = wanted {
            match start(key) {
                Ok(l) => view.loading = Some(l),
                Err(e) => {
                    warn!("scenario {key}: {e}");
                    view.error = Some((key.to_owned(), e));
                }
            }
        }
    }
    if let Some(l) = view.loading.as_mut() {
        let mut failed = None;
        l.tasks.retain_mut(|task| {
            if !task.is_finished() {
                return true;
            }
            match crate::platform::http::poll_task(task) {
                Some(Ok(set)) => l.sets.push(set),
                Some(Err(e)) => failed = Some(e),
                None => return true,
            }
            false
        });
        if let Some(e) = failed {
            let key = l.key.clone();
            warn!("scenario {key}: {e}");
            *view = ScenarioView {
                error: Some((key, e)),
                ..default()
            };
        } else if l.tasks.is_empty() {
            let Some(l) = view.loading.take() else { return };
            let t = l.scenario.opens;
            match Play::new(l.scenario, l.sets) {
                Ok(play) => {
                    let frame = play.at(t);
                    view.open = Some(Open {
                        key: l.key,
                        play,
                        layer: l.layer,
                        t,
                        carry: 0.0,
                        playing: false,
                        speed: 60.0,
                        frame,
                        fills: HashMap::new(),
                        selected: None,
                    });
                }
                Err(e) => view.error = Some((l.key, e.to_string())),
            }
        }
    }
    let Some(o) = view.open.as_mut() else {
        return;
    };
    let mut stale = false;
    if let Ok((cell, tf)) = camera.single() {
        stale |= o
            .play
            .set_rung(Rung::at_altitude(globe.altitude_m(cell, tf)));
    }
    if o.playing {
        let step = time.delta_secs_f64() * o.speed + o.carry;
        let whole = step.floor();
        o.carry = step - whole;
        o.t = o.t.plus(Clock(whole as i64));
        if o.t >= o.play.scenario.clock.end {
            o.t = o.play.scenario.clock.end;
            o.playing = false;
        }
    }
    if stale || o.frame.t != o.t {
        o.frame = o.play.at(o.t);
    }
}

/// Projects a place to the screen for this frame's camera.
struct Projector<'a> {
    globe: &'a GlobeState,
    anchor: (f64, f64, u32),
    cam_world: bevy::math::DVec3,
    in_space: bool,
    terrain: &'a crate::systems::terrain::TerrainField,
    cam: &'a Camera,
    view: GlobalTransform,
    ui_scale: f32,
}

impl Projector<'_> {
    fn screen(&self, p: LonLat) -> Option<egui::Pos2> {
        let delta = place_pins::place_delta(
            self.globe,
            self.anchor,
            self.cam_world,
            self.in_space,
            p[1],
            p[0],
            self.terrain,
        )?;
        let s = self
            .cam
            .world_to_viewport(&self.view, delta.as_vec3())
            .ok()?;
        Some(egui::pos2(s.x / self.ui_scale, s.y / self.ui_scale))
    }

    /// A line through the points, broken where one does not project.
    fn runs(&self, pts: &[LonLat]) -> Vec<Vec<egui::Pos2>> {
        let mut out = vec![Vec::new()];
        for p in pts {
            match self.screen(*p) {
                Some(s) => out.last_mut().into_iter().for_each(|r| r.push(s)),
                None => {
                    if out.last().is_some_and(|r| !r.is_empty()) {
                        out.push(Vec::new());
                    }
                }
            }
        }
        out.retain(|r| r.len() > 1);
        out
    }
}

fn rgb(c: cartopolis_scenario::region::Rgb, alpha: f32) -> egui::Color32 {
    let [r, g, b] = c.0;
    egui::Color32::from_rgba_unmultiplied(r, g, b, (alpha * 255.0) as u8)
}

/// The ground a view can see, as a box of degrees round the camera: a tilted
/// view sees a few heights out, and an area wholly outside is not projected.
fn view_box(lat: f64, lng: f64, alt_m: f64) -> [f64; 4] {
    let r_km = (alt_m * 4.0 / 1000.0).max(20.0);
    let dlat = r_km / 110.574;
    let dlng = r_km / (111.320 * lat.to_radians().cos().max(0.05));
    [lng - dlng, lat - dlat, lng + dlng, lat + dlat]
}

/// Paint the open layer's region states, and any tracks, over the map.
#[allow(clippy::too_many_arguments)]
pub fn render_scenario_map(
    mut egui: EguiMut,
    mut view: ResMut<ScenarioView>,
    globe: Res<GlobeState>,
    camera: Query<(&Camera, &GridCell, &Transform), With<FreeCamera>>,
    terrain: Res<crate::systems::terrain::TerrainField>,
    map_anchor: Res<MapAnchor>,
    keyboard: Res<crate::systems::onscreen_keyboard::OnScreenKeyboard>,
    route: Res<crate::systems::navigation::RouteState>,
) {
    let mut anchored = WorldAnchored::new(egui.ctx(), "scenario_map");
    let Some(o) = view.open.as_mut() else {
        return;
    };
    let Some(anchor) = map_anchor.geo else {
        return;
    };
    let Ok((cam, cell, tf)) = camera.single() else {
        return;
    };
    let alt = globe.altitude_m(cell, tf);
    let (cam_lat, cam_lng) = crate::systems::geo_nav::camera_geo(anchor, &globe, cell, tf);
    let in_space = globe.in_space(cell, tf);
    let seen = if in_space {
        [-180.0, -90.0, 180.0, 90.0]
    } else {
        view_box(cam_lat, cam_lng, alt)
    };
    let map_rect = place_pins::map_area(&mut egui, &keyboard, &route);
    let ui_scale = egui.ui_scale();
    let ctx = egui.ctx_mut().clone();
    let proj = Projector {
        globe: &globe,
        anchor,
        cam_world: crate::grid::anchored_position_f64(cell, tf.translation),
        in_space,
        terrain: &terrain,
        cam,
        // This frame's camera basis (`docs/notes/screen-overlay-projection.md`).
        view: GlobalTransform::from(Transform::from_rotation(tf.rotation)),
        ui_scale,
    };
    let painter = ctx
        .layer_painter(egui::LayerId::new(
            egui::Order::Background,
            egui::Id::new("scenario_map"),
        ))
        // The map's width, but its whole height: the strips the dock reserves
        // are for taps, and a fill stopping short of them reads as a gap.
        .with_clip_rect(egui::Rect::from_x_y_ranges(
            map_rect.x_range(),
            ctx.viewport_rect().y_range(),
        ));

    // Region states, under everything else.
    if o.fills.len() > MAX_FILLS {
        o.fills.clear();
    }
    let li = o.layer;
    if let (Some(places), Some(set)) = (o.frame.regions.get(li), o.play.layer_areas(li)) {
        let set_key = o
            .play
            .areas
            .iter()
            .position(|a| a.name == set.name)
            .unwrap_or(0);
        let layer = &o.play.scenario.region_states[li];
        for p in places {
            let Some(a) = p.area else { continue };
            let area = &set.areas[a];
            let b = area.bbox;
            if b[2] < seen[0] || b[0] > seen[2] || b[3] < seen[1] || b[1] > seen[3] {
                continue;
            }
            let state = &layer.states[p.state];
            let passed = state.when == Some(When::Passed);
            let fill = rgb(state.colour, if passed { 0.30 } else { 0.70 });
            let edge = rgb(state.colour, 0.9);
            let (verts, idx) = o
                .fills
                .entry((set_key, a))
                .or_insert_with(|| area.triangles());
            let screen: Vec<Option<egui::Pos2>> = verts.iter().map(|v| proj.screen(*v)).collect();
            let mut mesh = egui::Mesh::default();
            let mut at: HashMap<u32, u32> = HashMap::new();
            for tri in idx.chunks_exact(3) {
                let Some(pts) = tri
                    .iter()
                    .map(|&i| screen[i as usize])
                    .collect::<Option<Vec<_>>>()
                else {
                    continue;
                };
                let mut ids = [0u32; 3];
                for (k, (&i, pos)) in tri.iter().zip(pts).enumerate() {
                    ids[k] = *at.entry(i).or_insert_with(|| {
                        mesh.colored_vertex(pos, fill);
                        mesh.vertices.len() as u32 - 1
                    });
                }
                mesh.add_triangle(ids[0], ids[1], ids[2]);
            }
            if !mesh.is_empty() {
                painter.add(egui::Shape::mesh(mesh));
            }
            for ring in area.polygons.iter().flatten() {
                let mut closed = ring.clone();
                closed.extend(ring.first().copied());
                for run in proj.runs(&closed) {
                    painter.add(egui::Shape::line(run, egui::Stroke::new(0.8, edge)));
                }
            }
        }
    }

    // Tracks: the cone, the path behind and ahead, the times along it, where
    // some stopped, and the head with how many still move.
    let clock = o.play.scenario.clock;
    for tn in &o.frame.tracks {
        let i = tn.track;
        let Some(head) = tn.head else { continue };
        let tr = &o.play.scenario.tracks[i];
        let ahead = o.play.ahead(i, o.t, AHEAD, Clock(5 * 60));
        let mut cone = egui::Mesh::default();
        let mut left: Vec<LonLat> = Vec::new();
        let mut right: Vec<LonLat> = Vec::new();
        for a in &ahead {
            left.push(track::offset(a.at, a.heading - 90.0, a.cone_km));
            right.push(track::offset(a.at, a.heading + 90.0, a.cone_km));
        }
        for k in 1..ahead.len() {
            let q = [left[k - 1], right[k - 1], right[k], left[k]].map(|p| proj.screen(p));
            if let [Some(a), Some(b), Some(c), Some(d)] = q {
                let base = cone.vertices.len() as u32;
                for p in [a, b, c, d] {
                    cone.colored_vertex(p, ink::CONE);
                }
                cone.add_triangle(base, base + 1, base + 2);
                cone.add_triangle(base, base + 2, base + 3);
            }
        }
        if !cone.is_empty() {
            painter.add(egui::Shape::mesh(cone));
        }
        let outline: Vec<LonLat> = left.iter().chain(right.iter().rev()).copied().collect();
        for run in proj.runs(&outline) {
            painter.add(egui::Shape::line(
                run,
                egui::Stroke::new(1.0, ink::CONE_EDGE),
            ));
        }
        for run in proj.runs(&o.play.behind(i, o.t)) {
            painter.add(egui::Shape::line(
                run.clone(),
                egui::Stroke::new(5.0, ink::TRACK_GLOW),
            ));
            painter.add(egui::Shape::line(run, egui::Stroke::new(1.6, ink::TRACK)));
        }
        let ahead_pts: Vec<LonLat> = ahead.iter().map(|a| a.at).collect();
        for run in proj.runs(&ahead_pts) {
            painter.extend(egui::Shape::dashed_line(
                &run,
                egui::Stroke::new(1.6, ink::TRACK),
                3.0,
                6.0,
            ));
        }
        // A time every half hour on the clock.
        let first = Clock((o.t.0 + 1799).div_euclid(1800) * 1800);
        for a in o
            .play
            .ahead(i, first, AHEAD.minus(first.minus(o.t)), Clock(1800))
        {
            let Some(s) = proj.screen(a.at) else { continue };
            painter.circle_filled(s, 2.6, ink::TRACK);
            painter.text(
                s - egui::vec2(0.0, 8.0),
                egui::Align2::CENTER_BOTTOM,
                clock.label(a.t),
                egui::FontId::proportional(theme::SIZE_TINY),
                ink::TRACK,
            );
        }
        for &(when, _) in tr.down.iter().filter(|d| d.0 <= o.t && d.0 >= tr.t0) {
            let Some(p) = tr.position(when).and_then(|p| proj.screen(p)) else {
                continue;
            };
            let d = egui::vec2(4.0, 4.0);
            let stroke = egui::Stroke::new(1.8, ink::INTERCEPT);
            painter.line_segment([p - d, p + d], stroke);
            painter.line_segment(
                [p + egui::vec2(4.0, -4.0), p + egui::vec2(-4.0, 4.0)],
                stroke,
            );
        }
        let Some(h) = proj.screen(head) else { continue };
        anchored.at(h);
        painter.circle_filled(h, 18.0, ink::HEAD);
        for k in 0..7 {
            let a = k as f32 * 2.4;
            let r = 3.0 + (k % 3) as f32 * 3.2;
            painter.circle_filled(h + egui::vec2(a.cos(), a.sin()) * r, 2.2, ink::HEAD_DOT);
        }
        let label = match tn.moving {
            Some(n) if tr.count.is_some() => format!("{} · {n}", tr.id),
            _ => tr.id.clone(),
        };
        let galley = painter.layout_no_wrap(
            label,
            egui::FontId::proportional(theme::SIZE_CHIP),
            ink::TRACK,
        );
        let chip = egui::Rect::from_min_size(
            h + egui::vec2(14.0, 10.0),
            galley.size() + theme::CHIP_PADDING * 2.0,
        );
        let selected = o.selected == Some(i);
        painter.rect(
            chip,
            egui::CornerRadius::same(theme::RADIUS_SM),
            theme::LABEL_CHIP,
            egui::Stroke::new(if selected { 2.0 } else { 1.0 }, ink::HEAD_RIM),
            egui::StrokeKind::Inside,
        );
        painter.galley(chip.min + theme::CHIP_PADDING, galley, ink::TRACK);
    }
}

/// The colour and words of a mode's frame.
fn mode_frame(mode: Mode) -> (egui::Color32, &'static str) {
    match mode {
        Mode::Live => (ink::LIVE, "Live"),
        Mode::Replay => (ink::REPLAY, "Replay"),
        Mode::Exercise => (ink::EXERCISE, "Exercise"),
        Mode::Sandbox => (ink::SANDBOX, "Sandbox"),
    }
}

/// Diagonal stripes along a rect, for an exercise's frame.
fn stripes(painter: &egui::Painter, r: egui::Rect, colour: egui::Color32) {
    let painter = painter.with_clip_rect(r);
    let step = 12.0;
    let mut x = r.left() - r.height();
    while x < r.right() {
        painter.line_segment(
            [
                egui::pos2(x, r.bottom()),
                egui::pos2(x + r.height(), r.top()),
            ],
            egui::Stroke::new(4.0, colour),
        );
        x += step;
    }
}

/// Width of the layer's cards.
const CARD_W: f32 = 250.0;

/// A number as a person reads it: whole above ten with its thousands
/// grouped, one decimal below.
fn figure(v: f32, unit: &str) -> String {
    let n = if v.abs() >= 10.0 {
        let digits = format!("{:.0}", v.abs());
        let mut grouped = String::new();
        for (i, c) in digits.chars().enumerate() {
            if i > 0 && (digits.len() - i) % 3 == 0 {
                grouped.push(',');
            }
            grouped.push(c);
        }
        if v < 0.0 {
            format!("-{grouped}")
        } else {
            grouped
        }
    } else {
        format!("{v:.1}")
    };
    match unit {
        "%" => format!("{n} %"),
        "k€" => format!("€{n}k"),
        "" => n,
        u => format!("{n} {u}"),
    }
}

/// The layer's own chrome: what it is and how real (top), its bands and the
/// place under the camera (right), the clock when it runs (bottom).
#[allow(clippy::too_many_arguments)]
pub fn render_scenario_chrome(
    mut egui: EguiMut,
    mut view: ResMut<ScenarioView>,
    mut layers: ResMut<crate::systems::data_layers::DataLayers>,
    right_panel: Res<crate::resources::RightPanel>,
    globe: Res<GlobeState>,
    camera: Query<(&GridCell, &Transform), With<FreeCamera>>,
    map_anchor: Res<MapAnchor>,
) {
    let ctx = egui.ctx_mut().clone();
    let mut leave = false;
    if view.open.is_none() {
        // Loading or failed: say so, with the way out.
        let words = match (&view.loading, &view.error) {
            (Some(_), _) => "Loading the areas…".to_owned(),
            (_, Some((_, e))) => format!("Could not open this layer: {e}"),
            _ => return,
        };
        egui::Area::new(egui::Id::new("scenario_title"))
            .order(egui::Order::Foreground)
            .anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, 14.0))
            .show(&ctx, |ui| {
                theme::overlay_frame().show(ui, |ui| {
                    ui.set_width(340.0);
                    ui.label(egui::RichText::new(words).size(theme::SIZE_SUB));
                    if theme::action_button(ui, "Close").clicked() {
                        leave = true;
                    }
                });
            });
        if leave {
            layers.scenario = None;
        }
        return;
    }
    let here = map_anchor.geo.and_then(|anchor| {
        let (cell, tf) = camera.single().ok()?;
        let (lat, lng) = crate::systems::geo_nav::camera_geo(anchor, &globe, cell, tf);
        Some([lng, lat])
    });
    let Some(o) = view.open.as_mut() else {
        return;
    };
    let screen = ctx.viewport_rect();
    let meta = &o.play.scenario.meta;
    let (colour, mode_word) = mode_frame(meta.mode);

    // The frame: a line round the whole map in the mode's colour, striped for
    // an exercise, on every screen.
    let edge = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Foreground,
        egui::Id::new("scenario_frame"),
    ));
    let band = if meta.mode == Mode::Exercise {
        6.0
    } else {
        3.0
    };
    for r in [
        egui::Rect::from_min_max(screen.min, egui::pos2(screen.max.x, screen.min.y + band)),
        egui::Rect::from_min_max(egui::pos2(screen.min.x, screen.max.y - band), screen.max),
        egui::Rect::from_min_max(screen.min, egui::pos2(screen.min.x + band, screen.max.y)),
        egui::Rect::from_min_max(egui::pos2(screen.max.x - band, screen.min.y), screen.max),
    ] {
        if meta.mode == Mode::Exercise {
            edge.rect_filled(r, 0.0, theme::INK);
            stripes(&edge, r, colour);
        } else {
            edge.rect_filled(r, 0.0, colour);
        }
    }

    // What it is, and how real.
    egui::Area::new(egui::Id::new("scenario_title"))
        .order(egui::Order::Foreground)
        .anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, 14.0))
        .show(&ctx, |ui| {
            theme::overlay_frame().show(ui, |ui| {
                ui.set_width(340.0);
                ui.horizontal(|ui| {
                    let (r, _) =
                        ui.allocate_exact_size(egui::vec2(74.0, 20.0), egui::Sense::hover());
                    if meta.mode == Mode::Exercise {
                        ui.painter().rect_filled(r, theme::RADIUS_SM, theme::INK);
                        stripes(ui.painter(), r, colour.gamma_multiply(0.55));
                    } else {
                        ui.painter().rect_filled(r, theme::RADIUS_SM, colour);
                    }
                    ui.painter().text(
                        r.center(),
                        egui::Align2::CENTER_CENTER,
                        mode_word,
                        egui::FontId::proportional(theme::SIZE_CHIP),
                        theme::INK,
                    );
                    ui.label(
                        egui::RichText::new(&meta.name)
                            .size(theme::SIZE_BODY)
                            .color(theme::TEXT),
                    );
                });
                let honest = match (meta.mode, meta.confidence) {
                    (Mode::Live, _) => "As published by the official source",
                    (_, Confidence::Invented) => "Not a real event · invented for an exercise",
                    (Mode::Replay, Confidence::Documented) => "Published figures, as documented",
                    _ => "Not a real event · a model's estimate",
                };
                ui.label(
                    egui::RichText::new(honest)
                        .size(theme::SIZE_SUB)
                        .color(theme::TEXT_DIM),
                );
                ui.horizontal(|ui| {
                    ui.menu_button(egui::RichText::new("Sources").size(theme::SIZE_SUB), |ui| {
                        ui.set_width(320.0);
                        for s in &meta.sources {
                            ui.label(egui::RichText::new(s).size(theme::SIZE_SUB));
                        }
                    });
                    if theme::action_button(ui, "Close layer").clicked() {
                        leave = true;
                    }
                });
            });
        });

    // The bands and the place under the camera, while no panel holds the right.
    if *right_panel == crate::resources::RightPanel::None {
        egui::Area::new(egui::Id::new("scenario_states"))
            .order(egui::Order::Foreground)
            .anchor(egui::Align2::RIGHT_TOP, egui::vec2(-18.0, 18.0))
            .show(&ctx, |ui| {
                theme::overlay_frame().show(ui, |ui| {
                    ui.set_width(CARD_W);
                    states_card(ui, o, here);
                });
            });
    }

    // The clock, when there is time to move through.
    if o.play.scenario.clock.end > Clock::ZERO {
        egui::Area::new(egui::Id::new("scenario_clock"))
            .order(egui::Order::Foreground)
            .anchor(
                egui::Align2::CENTER_BOTTOM,
                egui::vec2(0.0, -(crate::systems::dock::reserved_bottom() + 8.0)),
            )
            .show(&ctx, |ui| {
                theme::overlay_frame().show(ui, |ui| {
                    ui.set_width(420.0);
                    clock_bar(ui, o);
                });
            });
    }

    if o.playing {
        ctx.request_repaint();
    }
    if leave {
        layers.scenario = None;
    }
}

fn dim(ui: &mut egui::Ui, text: impl Into<String>) {
    ui.label(
        egui::RichText::new(text.into())
            .size(theme::SIZE_SUB)
            .color(theme::TEXT_DIM),
    );
}

fn states_card(ui: &mut egui::Ui, o: &mut Open, here: Option<LonLat>) {
    let clock = o.play.scenario.clock;
    let li = o.layer;
    let layer = &o.play.scenario.region_states[li];
    let set = o.play.layer_areas(li);
    let field = set.and_then(|s| Some((s, s.field(layer.field.as_deref()?)?)));
    ui.label(
        egui::RichText::new(&layer.label)
            .size(theme::SIZE_BODY)
            .color(theme::TEXT),
    );
    match set {
        Some(s) => {
            let year = field
                .and_then(|(s, f)| s.fields[f].year)
                .map(|y| format!(" · {y}"))
                .unwrap_or_default();
            dim(ui, format!("per {}{year}", s.rung.label()));
        }
        None => dim(ui, "Nothing to show at this height"),
    }
    ui.add_space(4.0);
    let places = o
        .frame
        .regions
        .get(li)
        .map(Vec::as_slice)
        .unwrap_or_default();
    for (si, s) in layer.states.iter().enumerate() {
        ui.horizontal(|ui| {
            let (r, _) = ui.allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::hover());
            ui.painter().rect_filled(r, 2.0, rgb(s.colour, 0.85));
            ui.label(egui::RichText::new(&s.label).size(theme::SIZE_SUB));
            // A count belongs to a derived state (how many regions are under
            // threat); a figure's bands are for reading, not ranking.
            if layer.field.is_none() {
                let n = places.iter().filter(|p| p.state == si).count();
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(egui::RichText::new(n.to_string()).size(theme::SIZE_SUB));
                });
            }
        });
    }
    // The place under the camera, and its figure.
    if let (Some(p), Some((set, f))) = (here, field) {
        if let Some(a) = set.area_at(p) {
            ui.add_space(6.0);
            dim(ui, "Here");
            let value = set.value(a, f).map_or("not published".to_owned(), |v| {
                figure(v, &set.fields[f].unit)
            });
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(&set.areas[a].name).size(theme::SIZE_SUB));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        egui::RichText::new(value)
                            .size(theme::SIZE_SUB)
                            .color(theme::TEXT),
                    );
                });
            });
            if let Some(people) = set.field("people").and_then(|pf| set.value(a, pf)) {
                dim(ui, format!("{} people live here", figure(people, "")));
            }
        }
    }
    if let Some(set) = set {
        ui.add_space(6.0);
        ui.label(
            egui::RichText::new(&set.attribution)
                .size(theme::SIZE_TINY)
                .color(theme::TEXT_DIM),
        );
    }
    // Soonest first, where a state says how long a region has.
    if let Some(set) = set {
        let mut next: Vec<(Clock, &str)> = places
            .iter()
            .filter(|p| layer.states[p.state].within.is_some())
            .filter_map(|p| Some((p.eta?, set.areas[p.area?].name.as_str())))
            .collect();
        next.sort();
        if !next.is_empty() {
            ui.add_space(6.0);
            dim(ui, "Next to be reached");
            for (eta, name) in next.iter().take(6) {
                let mins = eta.minus(o.t).minutes().round().max(0.0);
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(*name).size(theme::SIZE_SUB));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        dim(ui, format!("{mins:.0} min"));
                    });
                });
            }
        }
    }
    // The tracks, to pick one.
    if !o.play.scenario.tracks.is_empty() {
        ui.add_space(6.0);
        dim(ui, "Tracks");
    }
    let mut pick = None;
    for tn in &o.frame.tracks {
        let tr = &o.play.scenario.tracks[tn.track];
        let what = match (tn.moving, tr.count) {
            (Some(n), Some(c)) => format!("{n} of {c}"),
            (Some(_), None) => "moving".to_owned(),
            (None, _) if o.t < tr.t0 => format!("from {}", clock.label(tr.t0)),
            (None, _) => "ended".to_owned(),
        };
        let selected = o.selected == Some(tn.track);
        let r = ui.selectable_label(
            selected,
            egui::RichText::new(format!("{} · {what}", tr.id)).size(theme::SIZE_SUB),
        );
        if r.clicked() {
            pick = Some(if selected { None } else { Some(tn.track) });
        }
    }
    if let Some(p) = pick {
        o.selected = p;
    }
    // The picked track: what its cone reaches, and when.
    if let Some(i) = o.selected {
        let tr = &o.play.scenario.tracks[i];
        ui.add_space(6.0);
        ui.label(
            egui::RichText::new(format!(
                "{} · from {} · {:.0} km/h",
                tr.id,
                clock.label(tr.t0),
                tr.speed_kmh
            ))
            .size(theme::SIZE_SUB),
        );
        let reach = o.play.cone_reaches(i, li, o.t, Clock(3 * 3600));
        if let Some(set) = set {
            if !reach.is_empty() {
                dim(ui, "Its cone reaches");
            }
            for (a, t) in reach.iter().take(5) {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(&set.areas[*a].name).size(theme::SIZE_SUB));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        dim(ui, format!("~{}", clock.label(*t)));
                    });
                });
            }
        }
        dim(
            ui,
            format!("The cone widens {:.0} km an hour.", tr.cone_km_per_h),
        );
    }
}

fn clock_bar(ui: &mut egui::Ui, o: &mut Open) {
    let clock = o.play.scenario.clock;
    let end = clock.end;
    ui.horizontal(|ui| {
        let play_word = if o.playing { "Pause" } else { "Play" };
        if theme::action_button(ui, play_word).clicked() {
            if !o.playing && o.t >= end {
                o.t = Clock::ZERO;
            }
            o.playing = !o.playing;
        }
        ui.label(
            egui::RichText::new(clock.label(o.t))
                .size(theme::SIZE_TITLE)
                .color(theme::TEXT),
        );
        for &(speed, word) in &[(1.0, "1×"), (60.0, "60×"), (600.0, "600×")] {
            if ui
                .selectable_label(
                    o.speed == speed,
                    egui::RichText::new(word).size(theme::SIZE_SUB),
                )
                .clicked()
            {
                o.speed = speed;
            }
        }
    });
    let mut minutes = o.t.minutes() as f32;
    let slider = egui::Slider::new(&mut minutes, 0.0..=end.minutes() as f32).show_value(false);
    // A slider's rail is `slider_width` long whatever room it is given.
    ui.spacing_mut().slider_width = ui.available_width();
    if ui.add(slider).changed() {
        o.t = Clock::from_minutes(f64::from(minutes));
        o.carry = 0.0;
    }
    ui.horizontal(|ui| {
        dim(ui, clock.label(Clock::ZERO));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            dim(ui, clock.label(end));
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_shipped_layer_is_offered() {
        // `start` sets the area fetches going; here they only need a pool.
        IoTaskPool::get_or_init(bevy::tasks::TaskPool::new);
        let keys: Vec<&str> = offered().iter().map(|o| o.key.as_str()).collect();
        assert!(keys.contains(&"cbs-2024/share_65_plus"), "{keys:?}");
        for k in keys {
            assert!(start(k).is_ok(), "{k}");
        }
    }

    #[test]
    fn figures_read_as_people_write_them() {
        assert_eq!(figure(23.4, "%"), "23 %");
        assert_eq!(figure(412.0, "k€"), "€412k");
        assert_eq!(figure(7.25, "%"), "7.2 %");
        assert_eq!(figure(1200.0, ""), "1,200");
        assert_eq!(figure(17_942_942.0, ""), "17,942,942");
        assert_eq!(figure(412.0, ""), "412");
    }
}
