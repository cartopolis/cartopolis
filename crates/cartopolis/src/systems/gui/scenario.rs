//! **Scenario layers** over the map: a lens with a clock
//! (`docs/mockups/scenarios/`, `docs/notes/scenarios.md`). The engine is
//! `cartopolis_scenario`, which says what is true at a time on which board; this
//! draws one layer of it, and nothing here knows what a figure or a drone is.
//!
//! **One layer, every rung.** A layer stands on boards at several rungs of the
//! ladder (CBS figures per buurt, wijk, gemeente, provincie and country); each
//! frame the screen's scale picks the finest board whose areas are still wide
//! enough to tell apart (`Play::set_scale`), so zooming out turns buurten into
//! wijken into gemeenten without a switch, and a phone that sees the whole
//! country sees its municipalities.
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
//!
//! **A lens, not a layer** (`docs/mockups/lenses/`). Layers are switches about
//! how the world looks, set and left; a figure is something you read, one at a
//! time, with a source and a mode. So it has its own disc under Layers
//! (`systems::dock`) and one surface of its own ([`Sheet`]): a bottom sheet in
//! portrait, over a map that stays in view while you pick, and a column on the
//! left otherwise, clear of the right-hand panels. The figures in it are radio
//! rows that show their value under the crosshair, so one is picked by reading,
//! not by trying. A lens that is on needs no sheet (v3): the disc is lit, and
//! a pick, the ✕ or a drag on the map puts the sheet away. It writes `DataLayers::scenario` (`"<file>/<layer>"`), which
//! is what persists; for a capture, `CARTO_SCENARIO=cbs-2024/share_65_plus` and
//! `CARTO_LENS=figures|place|shelf`.

use crate::components::FreeCamera;
use crate::resources::MapAnchor;
use crate::systems::egui_manager::{EguiMut, WorldAnchored};
use crate::systems::globe::GlobeState;
use crate::systems::motion;
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

const SHIPPED: &[Shipped] = &[
    Shipped {
        id: "cbs-2024",
        toml: include_str!("../../../../../assets/scenarios/cbs-2024.toml"),
    },
    Shipped {
        id: "ses-2023",
        toml: include_str!("../../../../../assets/scenarios/ses-2023.toml"),
    },
    Shipped {
        id: "pbl-2050",
        toml: include_str!("../../../../../assets/scenarios/pbl-2050.toml"),
    },
    Shipped {
        id: "wlo-2060",
        toml: include_str!("../../../../../assets/scenarios/wlo-2060.toml"),
    },
];

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

/// A lens on the shelf: one shipped scenario file.
struct Shelved {
    id: &'static str,
    name: String,
    summary: String,
    question: Option<String>,
    mode: Mode,
    figures: usize,
    /// The key it opens at: its first figure.
    first: String,
}

/// Every shipped scenario with something to show, for the shelf.
fn shelf() -> &'static [Shelved] {
    static SHELF: std::sync::OnceLock<Vec<Shelved>> = std::sync::OnceLock::new();
    SHELF.get_or_init(|| {
        SHIPPED
            .iter()
            .filter_map(|s| {
                let sc = Scenario::parse(s.toml).ok()?;
                let first = sc.region_states.first()?;
                Some(Shelved {
                    id: s.id,
                    first: format!("{}/{}", s.id, first.id),
                    figures: sc.region_states.len(),
                    name: sc.meta.name,
                    summary: sc.meta.summary,
                    question: sc.meta.question,
                    mode: sc.meta.mode,
                })
            })
            .collect()
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
    /// The colour each area is drawn in now, eased towards its state's, so a
    /// year played or a figure switched flows over rather than snapping.
    colours: HashMap<(usize, usize), egui::Rgba>,
    /// The band picked on the scale: every other band fades back.
    focus: Option<usize>,
    /// The band under the pointer on the scale, last frame: a preview of the
    /// pick.
    peek: Option<usize>,
}

impl Open {
    /// The band the map singles out: one being pointed at, else the picked one.
    fn singled(&self) -> Option<usize> {
        self.peek.or(self.focus)
    }

    /// The band of the open figure at a place, for the marker on its scale.
    fn band_at(&self, layer: usize, p: LonLat) -> Option<usize> {
        let a = self.play.layer_areas(layer)?.area_at(p)?;
        self.frame
            .regions
            .get(layer)?
            .iter()
            .find(|r| r.area == Some(a))
            .map(|r| r.state)
    }
}

/// How fast a drawn colour closes on its state's: most of the way in a
/// fifth of a second.
const EASE_PER_S: f32 = 12.0;

/// A layer whose areas are still on their way.
struct Loading {
    key: String,
    scenario: Scenario,
    layer: usize,
    tasks: Vec<Task<Result<AreaSet, String>>>,
    sets: Vec<AreaSet>,
}

/// How much of the lens is up (`docs/mockups/lenses/`): the one surface a
/// lens has, a bottom sheet in portrait and a column on the left otherwise.
/// A lens that is on needs none of it (v3): its colours are on the map, its
/// mode's frame round it, and the lens disc is lit.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Sheet {
    /// Nothing up: where a lens rests, and where none is open.
    #[default]
    Hidden,
    /// The open figure's key and the lens's figures to pick one from, with
    /// its clock and its tracks.
    Figures,
    /// The place under the crosshair, every figure of the lens for it.
    Place,
    /// The lenses there are, to open one.
    Shelf,
}

impl Sheet {
    /// `CARTO_LENS=figures|place|shelf`, for a capture.
    fn from_env() -> Sheet {
        match crate::env_value("CARTO_LENS").as_deref() {
            Some("figures") => Sheet::Figures,
            Some("place") => Sheet::Place,
            Some("shelf") => Sheet::Shelf,
            _ => Sheet::Hidden,
        }
    }

    /// What is drawn, given whether a lens is open: with none open there is
    /// only the shelf.
    fn shown(self, lens: bool) -> Sheet {
        match (self, lens) {
            (Sheet::Shelf, _) => Sheet::Shelf,
            (_, false) => Sheet::Hidden,
            (s, true) => s,
        }
    }
}

/// The scenario layer on screen, if any.
#[derive(Resource)]
pub struct ScenarioView {
    open: Option<Open>,
    loading: Option<Loading>,
    /// Why the last one would not open.
    error: Option<(String, String)>,
    /// How much of the lens is up. Kept when the lens or its figure changes.
    sheet: Sheet,
    /// The finger on the sheet's grabber (`motion::grabber`).
    sheet_drag: motion::SheetDrag,
}

impl Default for ScenarioView {
    fn default() -> Self {
        Self {
            open: None,
            loading: None,
            error: None,
            sheet: Sheet::from_env(),
            sheet_drag: motion::SheetDrag::default(),
        }
    }
}

/// The lens disc under Layers (`systems::dock`). With a lens open it raises
/// its sheet, or hides it, the lens staying on; with none it opens the
/// shelf, or puts it away.
pub fn press_lens_disc(view: &mut ScenarioView, lens: bool) {
    view.sheet = match (view.sheet.shown(lens), lens) {
        (Sheet::Hidden, true) => Sheet::Figures,
        (Sheet::Hidden, false) => Sheet::Shelf,
        _ => Sheet::Hidden,
    };
}

/// Put up the shelf of lenses, whatever is open: the ring on a place's name
/// asks for it (`place_names`), to read that place through one.
pub fn open_shelf(view: &mut ScenarioView) {
    view.sheet = Sheet::Shelf;
}

/// Whether the lens disc wears its engaged plate: a lens is open, or the
/// shelf is up.
#[must_use]
pub fn lens_disc_on(view: &ScenarioView, lens: bool) -> bool {
    lens || view.sheet == Sheet::Shelf
}

/// Where the lens sat last frame, and whether along the bottom edge, for
/// `place_pins::map_area`. In egui's memory rather than a resource because
/// that function's five callers would each have to take one.
const RECT_ID: &str = "lens_rect";

/// The lens's rect last frame and whether it is the bottom sheet (else the
/// column on the left), while it is up.
#[must_use]
pub fn lens_rect(ctx: &egui::Context) -> Option<(egui::Rect, bool)> {
    ctx.data(|d| d.get_temp(egui::Id::new(RECT_ID)))
}

impl ScenarioView {
    /// Forget the open lens, keeping how much of the sheet is up.
    fn clear(&mut self) {
        self.open = None;
        self.loading = None;
        self.error = None;
    }

    /// Show another figure of the open lens's own file without fetching its
    /// areas again: every figure of a file stands on the same boards.
    fn switch_figure(&mut self, wanted: &str) -> bool {
        let Some(o) = self.open.as_mut() else {
            return false;
        };
        let (Some((file, id)), Some((open_file, _))) =
            (wanted.split_once('/'), o.key.split_once('/'))
        else {
            return false;
        };
        if file != open_file {
            return false;
        }
        let Some(i) = o
            .play
            .scenario
            .region_states
            .iter()
            .position(|r| r.id == id)
        else {
            return false;
        };
        o.layer = i;
        o.key = wanted.to_owned();
        // Another figure has other bands; the colours ease over on their own.
        o.focus = None;
        o.peek = None;
        true
    }

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
) {
    let wanted = layers.scenario.as_deref();
    // Only on a change, so this is false on every frame but the switch's.
    if view.key() != wanted && !wanted.is_some_and(|w| view.switch_figure(w)) {
        view.clear();
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
            view.clear();
            view.error = Some((key, e));
        } else if l.tasks.is_empty() {
            let Some(l) = view.loading.take() else { return };
            let t = l.scenario.opens;
            let speed = default_speed(&l.scenario);
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
                        speed,
                        frame,
                        fills: HashMap::new(),
                        selected: None,
                        colours: HashMap::new(),
                        // `CARTO_LENS_BAND=2` singles out a band, for a capture.
                        focus: crate::env_value("CARTO_LENS_BAND").and_then(|b| b.parse().ok()),
                        peek: None,
                    });
                }
                Err(e) => view.error = Some((l.key, e.to_string())),
            }
        }
    }
    let Some(o) = view.open.as_mut() else {
        return;
    };
    // The board each layer is read on follows the screen's scale, which
    // `render_scenario_map` measures; this only runs the clock.
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
    if o.frame.t != o.t {
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
    // Read each layer on the finest board it can show at this scale: the
    // span of a hundredth of a degree of latitude at the view's middle.
    let km_per_px = {
        let d = 0.01;
        let (a, b) = (
            proj.screen([cam_lng, cam_lat - d]),
            proj.screen([cam_lng, cam_lat + d]),
        );
        match (a, b) {
            (Some(a), Some(b)) if a.distance(b) > 0.01 => {
                2.0 * d * 110.574 / f64::from(a.distance(b))
            }
            // Straight overhead from far out a point can fail to project:
            // a 60° view sees about its own height across 1,000 points.
            _ => alt / 1000.0 / 1000.0,
        }
    };
    if o.play.set_scale(km_per_px) {
        o.frame = o.play.at(o.t);
    }
    // The map's width, but its whole height: the strips the dock reserves
    // are for taps, and a fill stopping short of them reads as a gap. The
    // lens's own column is taken out of the map too, but the map runs on
    // under it, so the fills do: else a bare band stands down the left.
    let mut x_range = map_rect.x_range();
    if let Some((lens, false)) = lens_rect(&ctx) {
        if map_rect.left() >= lens.right() - 1.0 {
            x_range.min = ctx.viewport_rect().left();
        }
    }
    let painter = ctx
        .layer_painter(egui::LayerId::new(
            egui::Order::Background,
            egui::Id::new("scenario_map"),
        ))
        .with_clip_rect(egui::Rect::from_x_y_ranges(
            x_range,
            ctx.viewport_rect().y_range(),
        ));

    // Region states, under everything else.
    if o.fills.len() > MAX_FILLS {
        o.fills.clear();
    }
    if o.colours.len() > MAX_FILLS {
        o.colours.clear();
    }
    // The pointer, where it rests on the bare map: a finger has no hover, and
    // over a disc or a card the map is not what it points at.
    let pointer = ctx.pointer_hover_pos().filter(|&at| {
        !theme::touch_layout()
            && map_rect.contains(at)
            && ctx
                .layer_id_at(at)
                .is_none_or(|l| l.order == egui::Order::Background)
    });
    let ease = 1.0 - (-EASE_PER_S * ctx.input(|i| i.stable_dt).min(0.1)).exp();
    let mut easing = false;
    let singled = o.singled();
    let li = o.layer;
    if let (Some(places), Some(set)) = (o.frame.regions.get(li), o.play.layer_areas(li)) {
        let set_key = o
            .play
            .areas
            .iter()
            .position(|a| a.name == set.name)
            .unwrap_or(0);
        let layer = &o.play.scenario.region_states[li];
        // The area under the pointer, and its band.
        let mut hovered: Option<(usize, usize)> = None;
        for p in places {
            let Some(a) = p.area else { continue };
            let area = &set.areas[a];
            let b = area.bbox;
            if b[2] < seen[0] || b[0] > seen[2] || b[3] < seen[1] || b[1] > seen[3] {
                continue;
            }
            let state = &layer.states[p.state];
            let passed = state.when == Some(When::Passed);
            let alpha = match singled {
                // A band picked on the scale: the rest stay as a hint of
                // where they are, no more.
                Some(band) if band != p.state => 0.08,
                _ if passed => 0.30,
                _ => 0.70,
            };
            let target = egui::Rgba::from(rgb(state.colour, alpha));
            // A new area fades in from nothing, so a finer board arriving
            // on a zoom comes up rather than popping.
            let drawn = o
                .colours
                .entry((set_key, a))
                .or_insert(egui::Rgba::TRANSPARENT);
            *drawn = eased(*drawn, target, ease, &mut easing);
            let fill = egui::Color32::from(*drawn);
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
                if hovered.is_none() && pointer.is_some_and(|q| in_triangle(q, &pts)) {
                    hovered = Some((a, p.state));
                }
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
                    painter.add(egui::Shape::line(
                        run,
                        egui::Stroke::new(0.8, ink::AREA_EDGE),
                    ));
                }
            }
        }
        // The place being read stands out: the one under the pointer, else
        // the one under the crosshair, whose figure the key gives.
        let marked = hovered
            .map(|(a, _)| a)
            .or_else(|| set.area_at([cam_lng, cam_lat]));
        if let Some(a) = marked {
            for ring in set.areas[a].polygons.iter().flatten() {
                let mut closed = ring.clone();
                closed.extend(ring.first().copied());
                for run in proj.runs(&closed) {
                    painter.add(egui::Shape::line(
                        run.clone(),
                        egui::Stroke::new(4.5, ink::AREA_HALO),
                    ));
                    painter.add(egui::Shape::line(
                        run,
                        egui::Stroke::new(2.0, theme::p().cream),
                    ));
                }
            }
        }
        if let (Some((a, band)), Some(at)) = (hovered, pointer) {
            let value = shown(o, li, set, a, true);
            area_tooltip(
                &ctx,
                at,
                &set.areas[a].name,
                value,
                &layer.states[band],
                set.rung,
            );
        }
    }
    if easing {
        ctx.request_repaint();
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

/// A drawn colour a step closer to its target, `ease` of the way; `moving`
/// is raised while it is still visibly short of it.
fn eased(now: egui::Rgba, target: egui::Rgba, ease: f32, moving: &mut bool) -> egui::Rgba {
    let (a, b) = (now.to_array(), target.to_array());
    if a.iter().zip(&b).all(|(x, y)| (x - y).abs() < 0.004) {
        return target;
    }
    *moving = true;
    let c: [f32; 4] = std::array::from_fn(|i| a[i] + (b[i] - a[i]) * ease);
    egui::Rgba::from_rgba_premultiplied(c[0], c[1], c[2], c[3])
}

/// Between two chrome tones, `t` of the way, for a hover that eases in.
fn lerp_colour(a: egui::Color32, b: egui::Color32, t: f32) -> egui::Color32 {
    a.lerp_to_gamma(b, t.clamp(0.0, 1.0))
}

/// Whether a point lies in a screen triangle, either winding.
fn in_triangle(q: egui::Pos2, t: &[egui::Pos2]) -> bool {
    let side = |a: egui::Pos2, b: egui::Pos2| (b - a).x * (q - a).y - (b - a).y * (q - a).x;
    let (d0, d1, d2) = (side(t[0], t[1]), side(t[1], t[2]), side(t[2], t[0]));
    let neg = d0 < 0.0 || d1 < 0.0 || d2 < 0.0;
    let pos = d0 > 0.0 || d1 > 0.0 || d2 > 0.0;
    !(neg && pos)
}

/// The card beside the pointer over an area: its name, the figure there and
/// the band that colours it.
fn area_tooltip(
    ctx: &egui::Context,
    at: egui::Pos2,
    name: &str,
    value: Option<String>,
    band: &cartopolis_scenario::region::State,
    rung: Rung,
) {
    egui::Area::new(egui::Id::new("lens_area_tip"))
        .order(egui::Order::Tooltip)
        .fixed_pos(at + egui::vec2(18.0, 18.0))
        .interactable(false)
        .show(ctx, |ui| {
            theme::overlay_frame().show(ui, |ui| {
                // Pinned, or a size-to-content `Area` grows every frame.
                ui.set_max_width(220.0);
                ui.label(
                    egui::RichText::new(name)
                        .size(theme::SIZE_BODY)
                        .color(theme::p().cream),
                );
                ui.label(
                    egui::RichText::new(value.unwrap_or_else(|| "not published".to_owned()))
                        .size(theme::SIZE_HEADING)
                        .strong()
                        .color(theme::p().cream),
                );
                ui.horizontal(|ui| {
                    let (r, _) =
                        ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
                    ui.painter()
                        .rect_filled(r, theme::RADIUS_SM, rgb(band.colour, 1.0));
                    dim(ui, format!("{} · {}", band.label, rung.label()));
                });
            });
        });
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
    } else if v.abs() < 1.0 && v != 0.0 {
        // A score of a few tenths (SES-WOA) needs its second decimal.
        format!("{v:.2}")
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

/// Widest the bottom sheet's content gets: past it a tablet in portrait
/// would spend its whole width on a list of six rows.
const SHEET_MAX_W: f32 = 460.0;
/// The column's width in landscape and on a desktop, frame included.
const COLUMN_W: f32 = 320.0;
/// Most of the viewport height the sheet claims for a lens's figures or a
/// place; the shelf, being a choice made before looking, takes more.
const SHEET_SHARE: f32 = 0.55;
const SHELF_SHARE: f32 = 0.8;
/// What the parts above and below a scrolling body cost, roughly: only where
/// the body starts to scroll depends on it.
const HEADER_PTS: f32 = 96.0;
/// The body never shrinks below this, however short the viewport.
const BODY_MIN_PTS: f32 = 120.0;
/// What the open figure's key costs at the head of the figures: its line,
/// its scale and the place row.
const KEY_PTS: f32 = 110.0;
/// A figure's row, and the scale drawn small in it.
const ROW_PTS: f32 = 34.0;
const MINI_RAMP: egui::Vec2 = egui::vec2(40.0, 6.0);

/// Where the lens is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shape {
    /// Along the bottom edge, over a map that stays in view above it: a phone
    /// in portrait, where a panel would be the whole screen.
    Sheet,
    /// A column on the left, beside the top-left discs: landscape and wider,
    /// clear of the right-hand panels.
    Column,
}

fn shape(viewport: egui::Vec2, compact: bool) -> Shape {
    if compact || viewport.y > viewport.x {
        Shape::Sheet
    } else {
        Shape::Column
    }
}

/// What the person asked of the lens this frame, applied after it is drawn.
#[derive(Default)]
struct Asked {
    sheet: Option<Sheet>,
    /// A figure to show, `"<file>/<layer>"`.
    open: Option<String>,
    close: bool,
}

/// The lens (`docs/mockups/lenses/`): the coloured frame round the map that
/// says how real it is, and the one surface that holds everything else —
/// the figure and its scale, the figures to pick from, the place under the
/// crosshair, the clock, and the shelf of lenses.
#[allow(clippy::too_many_arguments)]
pub fn render_scenario_chrome(
    mut egui: EguiMut,
    mut view: ResMut<ScenarioView>,
    mut layers: ResMut<crate::systems::data_layers::DataLayers>,
    right_panel: Res<crate::resources::RightPanel>,
    globe: Res<GlobeState>,
    camera: Query<(&GridCell, &Transform), With<FreeCamera>>,
    map_anchor: Res<MapAnchor>,
    place_card: Res<crate::systems::place_card::PlaceCard>,
    menu: Res<crate::systems::ui::MainMenu>,
    keyboard: Res<crate::systems::onscreen_keyboard::OnScreenKeyboard>,
    driver: Res<crate::systems::driver::DriverState>,
) {
    let ctx = egui.ctx_mut().clone();
    let lens = layers.scenario.is_some();
    let mut shown = view.sheet.shown(lens);
    // A lens that would not open says so, whether its sheet is up or not.
    if lens && shown == Sheet::Hidden && view.error.is_some() {
        shown = Sheet::Figures;
    }
    // Pointing at a band lasts while the scale is drawn under it; the map,
    // which painted already, catches up with a change next frame.
    let peeked = view.open.as_mut().and_then(|o| o.peek.take());
    if let Some(o) = view.open.as_ref() {
        paint_frame(&ctx, o.play.scenario.meta.mode);
        if o.playing {
            ctx.request_repaint();
        }
    }
    let viewport = egui.viewport_pts();
    let shape = shape(viewport, egui.compact());
    // The dock's own reasons to stand aside, and on the bottom edge the
    // other things that live there: hidden, never closed.
    let hidden = shown == Sheet::Hidden
        || driver.active
        || menu.open
        || (egui.compact() && !right_panel.is(crate::resources::RightPanel::None))
        || (shape == Shape::Sheet
            && (keyboard.is_open()
                || right_panel.is(crate::resources::RightPanel::Route)
                || place_card.place.is_some()));
    if hidden {
        ctx.data_mut(|d| d.remove::<(egui::Rect, bool)>(egui::Id::new(RECT_ID)));
        if peeked.is_some() {
            ctx.request_repaint();
        }
        return;
    }
    // A drag that starts on the map is someone looking at the map: the
    // sheet goes, the lens staying on.
    if egui.map_dragged() {
        view.sheet = Sheet::Hidden;
        ctx.data_mut(|d| d.remove::<(egui::Rect, bool)>(egui::Id::new(RECT_ID)));
        return;
    }

    let here = map_anchor.geo.and_then(|anchor| {
        let (cell, tf) = camera.single().ok()?;
        let (lat, lng) = crate::systems::geo_nav::camera_geo(anchor, &globe, cell, tf);
        Some([lng, lat])
    });
    let frame = theme::overlay_frame();
    let inset = frame.inner_margin.sum().x + 2.0 * frame.stroke.width;
    let bottom = crate::systems::dock::reserved_bottom() + theme::OSD_MARGIN;
    let (area, width, body_h) = match shape {
        Shape::Sheet => {
            let share = if shown == Sheet::Shelf {
                SHELF_SHARE
            } else {
                SHEET_SHARE
            };
            (
                egui::Area::new(egui::Id::new("lens"))
                    .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -bottom)),
                (viewport.x - 2.0 * theme::OSD_MARGIN - inset).min(SHEET_MAX_W),
                (viewport.y * share - bottom - HEADER_PTS).max(BODY_MIN_PTS),
            )
        }
        Shape::Column => {
            let x = theme::OSD_MARGIN + theme::ROUND_BUTTON_SIZE + theme::OSD_GAP;
            (
                egui::Area::new(egui::Id::new("lens"))
                    .anchor(egui::Align2::LEFT_TOP, egui::vec2(x, theme::OSD_MARGIN)),
                COLUMN_W - inset,
                (viewport.y - theme::OSD_MARGIN - bottom - HEADER_PTS).max(BODY_MIN_PTS),
            )
        }
    };

    let mut asked = Asked::default();
    let ScenarioView {
        open,
        loading,
        error,
        sheet_drag,
        ..
    } = &mut *view;
    let shown_area = area.order(egui::Order::Foreground).show(&ctx, |ui| {
        frame.show(ui, |ui| {
            // Pinned before anything is laid out: a floating `Area` whose
            // content reaches for the available width grows every frame.
            ui.set_width(width);
            if shape == Shape::Sheet {
                asked.sheet = grabber(ui, sheet_drag);
            }
            match (shown, open.as_mut()) {
                (Sheet::Shelf, _) => shelf_body(ui, layers.scenario.as_deref(), body_h, &mut asked),
                (Sheet::Figures, Some(o)) => figures_body(ui, o, here, body_h, &mut asked),
                (Sheet::Place, Some(o)) => place_body(ui, o, here, body_h, &mut asked),
                _ => waiting_body(
                    ui,
                    loading.is_some(),
                    error.as_ref().map(|e| e.1.as_str()),
                    &mut asked,
                ),
            }
        });
    });
    let rect = shown_area.response.rect;
    ctx.data_mut(|d| d.insert_temp(egui::Id::new(RECT_ID), (rect, shape == Shape::Sheet)));
    if view.open.as_ref().and_then(|o| o.peek) != peeked {
        ctx.request_repaint();
    }

    // A pick is made to be seen: a lens off the shelf or a figure from the
    // list hides the sheet, over the map it changed. The lit disc raises it.
    if asked.open.is_some() {
        asked.sheet = Some(Sheet::Hidden);
    }
    if let Some(key) = asked.open {
        if layers.scenario.as_deref() != Some(key.as_str()) {
            layers.scenario = Some(key);
        }
    }
    if asked.close {
        layers.scenario = None;
        view.sheet = Sheet::Hidden;
    } else if let Some(s) = asked.sheet {
        view.sheet = s;
    }
}

/// A line round the whole map in the mode's colour, striped for an exercise,
/// on every screen: the one part of a lens that never folds away.
fn paint_frame(ctx: &egui::Context, mode: Mode) {
    let (colour, _) = mode_frame(mode);
    let screen = ctx.viewport_rect();
    let edge = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Foreground,
        egui::Id::new("scenario_frame"),
    ));
    let band = if mode == Mode::Exercise { 6.0 } else { 3.0 };
    for r in [
        egui::Rect::from_min_max(screen.min, egui::pos2(screen.max.x, screen.min.y + band)),
        egui::Rect::from_min_max(egui::pos2(screen.min.x, screen.max.y - band), screen.max),
        egui::Rect::from_min_max(screen.min, egui::pos2(screen.min.x + band, screen.max.y)),
        egui::Rect::from_min_max(egui::pos2(screen.max.x - band, screen.min.y), screen.max),
    ] {
        if mode == Mode::Exercise {
            edge.rect_filled(r, 0.0, theme::p().ink);
            stripes(&edge, r, colour);
        } else {
            edge.rect_filled(r, 0.0, colour);
        }
    }
}

/// The bar that says the sheet moves, and moves it (`motion::grabber`): a tap
/// or a pull down puts it away, an open lens staying on. One rest, so a pull
/// up only rubber-bands.
fn grabber(ui: &mut egui::Ui, drag: &mut motion::SheetDrag) -> Option<Sheet> {
    use motion::{Rest, SheetMove};
    match motion::grabber(ui, drag, Rest::Peek, &[Rest::Peek], "Put the sheet away") {
        SheetMove::Stay => None,
        SheetMove::To(_) | SheetMove::Close => Some(Sheet::Hidden),
    }
}

/// The mode as a chip: what kind of truth the colours are.
fn mode_chip(ui: &mut egui::Ui, mode: Mode) {
    let (colour, word) = mode_frame(mode);
    // As wide as its word, a pill like every other chip.
    let font = egui::FontId::proportional(theme::SIZE_CHIP);
    let w = ui
        .painter()
        .layout_no_wrap(word.to_owned(), font, theme::p().ink)
        .size()
        .x;
    let (r, _) = ui.allocate_exact_size(egui::vec2(w + 18.0, 20.0), egui::Sense::hover());
    if mode == Mode::Exercise {
        ui.painter()
            .rect_filled(r, theme::RADIUS_SM, theme::p().ink);
        stripes(ui.painter(), r, colour.gamma_multiply(0.55));
    } else {
        ui.painter().rect_filled(r, theme::RADIUS_PILL, colour);
    }
    ui.painter().text(
        r.center(),
        egui::Align2::CENTER_CENTER,
        word,
        egui::FontId::proportional(theme::SIZE_CHIP),
        theme::p().ink,
    );
}

/// How real the figures are, in a few words.
fn honest(meta: &cartopolis_scenario::Meta) -> &'static str {
    match (meta.mode, meta.confidence) {
        (Mode::Live, _) => "As published by the official source",
        (_, Confidence::Invented) => "Not a real event · invented for an exercise",
        (Mode::Replay, Confidence::Documented) => "Published figures, as documented",
        _ => "Not a real event · a model's estimate",
    }
}

/// The scale's bar, and the room over it for the marker.
const LEGEND_BAR_PTS: f32 = 12.0;
const LEGEND_MARK_PTS: f32 = 8.0;

/// The open figure's bands as one bar, light to dark, that answers: pointing
/// at a band singles its areas out on the map, a tap keeps them so (a second
/// tap lets go), and a marker stands over the band of the place under the
/// crosshair. Under it, the first and last band's words, or the singled one's.
fn legend(ui: &mut egui::Ui, o: &mut Open, here: Option<LonLat>) {
    let n = o.play.scenario.region_states[o.layer].states.len();
    if n == 0 {
        return;
    }
    let (r, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), LEGEND_MARK_PTS + LEGEND_BAR_PTS + 4.0),
        egui::Sense::hover(),
    );
    let bar = egui::Rect::from_min_size(
        egui::pos2(r.left(), r.top() + LEGEND_MARK_PTS + 2.0),
        egui::vec2(r.width(), LEGEND_BAR_PTS),
    );
    let w = bar.width() / n as f32;
    let singled = o.singled();
    let (mut peek, mut tapped) = (None, None);
    let states = &o.play.scenario.region_states[o.layer].states;
    for (i, s) in states.iter().enumerate() {
        let seg = egui::Rect::from_min_size(
            egui::pos2(bar.left() + w * i as f32, bar.top()),
            egui::vec2(w, bar.height()),
        );
        let id = ui.id().with(("lens_band", i));
        // Taller than it looks, so a finger finds it.
        let resp = ui.interact(seg.expand2(egui::vec2(0.0, 8.0)), id, egui::Sense::click());
        resp.widget_info(|| {
            egui::WidgetInfo::selected(
                egui::WidgetType::Button,
                ui.is_enabled(),
                singled == Some(i),
                &s.label,
            )
        });
        let resp = resp.on_hover_text(&s.label);
        if resp.hovered() {
            peek = Some(i);
        }
        if resp.clicked() {
            tapped = Some(i);
        }
        let ctx = ui.ctx();
        let lit = motion::toggle(ctx, id.with("lit"), singled.is_none_or(|b| b == i), 0.15);
        let up = motion::toggle(ctx, id.with("up"), singled == Some(i), 0.15);
        let seg = seg.expand2(egui::vec2(0.0, 2.5 * up));
        let round = (LEGEND_BAR_PTS * 0.5) as u8;
        let mut corners = egui::CornerRadius::ZERO;
        if i == 0 {
            corners.nw = round;
            corners.sw = round;
        }
        if i + 1 == n {
            corners.ne = round;
            corners.se = round;
        }
        ui.painter()
            .rect_filled(seg, corners, rgb(s.colour, 0.2 + 0.8 * lit));
        if up > 0.01 {
            ui.painter().rect_stroke(
                seg,
                corners,
                egui::Stroke::new(1.5 * up, theme::p().cream),
                egui::StrokeKind::Outside,
            );
        }
    }
    // The marker glides to the band of the place under the crosshair.
    if let Some(band) = here.and_then(|p| o.band_at(o.layer, p)) {
        let target = bar.left() + w * (band as f32 + 0.5);
        let x = motion::value(ui.ctx(), ui.id().with("lens_mark"), target, 0.25);
        let tip = bar.top() - 2.0;
        ui.painter().add(egui::Shape::convex_polygon(
            vec![
                egui::pos2(x - 5.0, tip - 6.0),
                egui::pos2(x + 5.0, tip - 6.0),
                egui::pos2(x, tip),
            ],
            theme::p().cream,
            egui::Stroke::new(1.0, theme::p().ink),
        ));
    }
    let tiny = |t: &str, c| egui::RichText::new(t).size(theme::SIZE_TINY).color(c);
    ui.horizontal(|ui| match singled {
        Some(b) => {
            ui.label(tiny(&states[b].label, theme::p().cream));
            if o.focus.is_some() {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let all = ui.add(
                        egui::Label::new(tiny("Show all", theme::p().accent))
                            .sense(egui::Sense::click()),
                    );
                    if all.clicked() {
                        tapped = o.focus;
                    }
                });
            }
        }
        None => {
            ui.label(tiny(&states[0].label, theme::p().text_dim));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(tiny(&states[n - 1].label, theme::p().text_dim));
            });
        }
    });
    // The map paints before this, so it sees a pick only next frame.
    if tapped.is_some() {
        ui.ctx().request_repaint();
    }
    o.peek = peek;
    if let Some(t) = tapped {
        o.focus = if o.focus == Some(t) { None } else { Some(t) };
    }
}

fn paint_ramp(p: &egui::Painter, r: egui::Rect, states: &[cartopolis_scenario::region::State]) {
    let n = states.len().max(1) as f32;
    let w = r.width() / n;
    let round = (r.height() * 0.5) as u8;
    for (i, s) in states.iter().enumerate() {
        let seg = egui::Rect::from_min_size(
            egui::pos2(r.left() + w * i as f32, r.top()),
            egui::vec2(w, r.height()),
        );
        let mut corners = egui::CornerRadius::ZERO;
        if i == 0 {
            corners.nw = round;
            corners.sw = round;
        }
        if i + 1 == states.len() {
            corners.ne = round;
            corners.se = round;
        }
        p.rect_filled(seg, corners, rgb(s.colour, 1.0));
    }
}

/// A figure's value at a place, as people write it, and the area it is in:
/// `None` off every area of its board at this height.
fn value_at(o: &Open, layer: usize, p: LonLat) -> Option<(&str, Option<String>)> {
    let set = o.play.layer_areas(layer)?;
    let a = set.area_at(p)?;
    Some((set.areas[a].name.as_str(), shown(o, layer, set, a, true)))
}

/// A layer's figure for one area at the clock's year, as it reads: a change
/// signed and in %, else in the field's unit (left off where `unit` is false
/// and the unit is not what makes it read as a number at all, €).
fn shown(o: &Open, layer: usize, set: &AreaSet, a: usize, unit: bool) -> Option<String> {
    let l = &o.play.scenario.region_states[layer];
    let year = o.play.scenario.clock.year_at(o.t);
    let v = l.figure(set, a, year)?;
    if l.change_since.is_some() {
        return Some(change(v));
    }
    let u = l
        .field
        .as_deref()
        .and_then(|k| set.field(k))
        .map_or("", |f| set.fields[f].unit.as_str());
    Some(figure(v, if unit || u == "k€" { u } else { "" }))
}

/// How sure the figure under the crosshair is, where its set says: `likely
/// 44,100 – 50,900`.
fn range_at(o: &Open, layer: usize, p: LonLat) -> Option<String> {
    let set = o.play.layer_areas(layer)?;
    let a = set.area_at(p)?;
    let l = &o.play.scenario.region_states[layer];
    let (lo, hi) = l.figure_range(set, a, o.play.scenario.clock.year_at(o.t))?;
    let read = |v| {
        if l.change_since.is_some() {
            change(v)
        } else {
            figure(v, "")
        }
    };
    let (lo, hi) = (read(lo), read(hi));
    // Narrower than the figure's own rounding says nothing.
    (lo != hi).then(|| format!("likely {lo} – {hi}"))
}

/// The same figure for the place's whole municipality or country, from the
/// board on that rung.
fn value_on_rung(o: &Open, layer: usize, p: LonLat, rung: Rung) -> Option<String> {
    let set = o.play.areas.iter().find(|s| s.rung == rung)?;
    shown(o, layer, set, set.area_at(p)?, false)
}

/// A change in %, always signed: `+4.2 %`, `−12 %`.
fn change(v: f32) -> String {
    let n = if v.abs() < 10.0 {
        format!("{:.1} %", v.abs())
    } else {
        figure(v.abs(), "%")
    };
    match v {
        v if v > 0.0 => format!("+{n}"),
        v if v < 0.0 => format!("\u{2212}{n}"),
        _ => n,
    }
}

/// How fast a scenario's clock plays when it opens: a year a second on a
/// clock of years, else a minute a second.
fn default_speed(s: &Scenario) -> f64 {
    if s.clock.start_year.is_some() {
        cartopolis_scenario::time::YEAR.0 as f64
    } else {
        60.0
    }
}

/// Where the open figure stands on its clock, for its key: the year, and
/// whether it is a model's from there.
fn when_label(o: &Open) -> Option<String> {
    let clock = o.play.scenario.clock;
    let year = clock.year_at(o.t)?;
    let forecast = o.play.scenario.forecast.is_some_and(|f| o.t >= f);
    let layer = &o.play.scenario.region_states[o.layer];
    let since = layer
        .change_since
        .map(|y| format!(", since {y}"))
        .unwrap_or_default();
    Some(if forecast {
        format!("{year} forecast{since}")
    } else {
        format!("{year}{since}")
    })
}

/// A header's title: `size` taken whatever its words, so the close button
/// after it keeps the corner, and the words start at the left (`add_sized`
/// centres them).
fn left_sized(ui: &mut egui::Ui, size: egui::Vec2, widget: impl egui::Widget) -> egui::Response {
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    // A child, not a scope: a scope moves the parent's cursor to where its
    // content ends, which put the close button right after the words.
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    child.add(widget)
}

/// The open lens's file, which every figure's key starts with.
fn file_of(o: &Open) -> &str {
    o.key.split_once('/').map_or(o.key.as_str(), |(f, _)| f)
}

/// The open figure's key, at the head of its sheet: how real it is, per what
/// and when, its scale, and its value under the crosshair as a row that opens
/// the place.
fn key_body(ui: &mut egui::Ui, o: &mut Open, here: Option<LonLat>, asked: &mut Asked) {
    let mut per = o
        .play
        .layer_areas(o.layer)
        .map_or("nothing to show at this height".to_owned(), |s| {
            format!("per {}", s.rung.label())
        });
    if let Some(w) = when_label(o) {
        per = format!("{per} · {w}");
    }
    dim(ui, format!("{} · {per}", honest(&o.play.scenario.meta)));
    ui.add_space(4.0);
    legend(ui, o, here);
    if let Some(r) = here_row(ui, o, here) {
        if r.clicked() {
            asked.sheet = Some(Sheet::Place);
        }
    }
}

/// The place under the crosshair and the open figure's value there, as a row
/// that opens the place's card.
fn here_row(ui: &mut egui::Ui, o: &Open, here: Option<LonLat>) -> Option<egui::Response> {
    let Some((name, value)) = here.and_then(|p| value_at(o, o.layer, p)) else {
        ui.add_space(4.0);
        dim(ui, "Nothing is published right under the crosshair");
        return None;
    };
    ui.add_space(6.0);
    let rung = o.play.layer_areas(o.layer).map_or("", |s| s.rung.label());
    let (r, resp) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 46.0), egui::Sense::click());
    let resp = theme::named(ui, resp, &name);
    let hot = motion::toggle(ui.ctx(), resp.id.with("hot"), resp.hovered(), 0.12);
    let p = ui.painter();
    p.rect(
        r,
        theme::RADIUS,
        lerp_colour(theme::p().surface_raised, theme::p().surface_hover, hot),
        egui::Stroke::new(
            1.0,
            lerp_colour(theme::p().surface_raised, theme::p().accent, hot),
        ),
        egui::StrokeKind::Inside,
    );
    // The place's own colour down its edge: the row and the outlined area
    // on the map are the same place.
    if let Some(band) = here.and_then(|q| o.band_at(o.layer, q)) {
        let colour = o.play.scenario.region_states[o.layer].states[band].colour;
        let edge = egui::Rect::from_min_size(
            r.left_top() + egui::vec2(5.0, 7.0),
            egui::vec2(4.0, r.height() - 14.0),
        );
        p.rect_filled(edge, theme::RADIUS_PILL, rgb(colour, 1.0));
    }
    let left = r.left() + 16.0;
    p.text(
        egui::pos2(left, r.top() + 7.0),
        egui::Align2::LEFT_TOP,
        name,
        egui::FontId::proportional(theme::SIZE_BODY),
        theme::p().cream,
    );
    let below = match here.and_then(|p| range_at(o, o.layer, p)) {
        Some(r) => format!("{rung} · {r}"),
        None => format!("{rung} · every figure here"),
    };
    p.text(
        egui::pos2(left, r.bottom() - 6.0),
        egui::Align2::LEFT_BOTTOM,
        below,
        egui::FontId::proportional(theme::SIZE_TINY),
        theme::p().text_dim,
    );
    // A chevron that leans in on hover: the row opens something.
    p.text(
        egui::pos2(r.right() - 8.0 + 2.0 * hot, r.center().y),
        egui::Align2::RIGHT_CENTER,
        crate::systems::icons::CHEVRON_RIGHT,
        egui::FontId::proportional(theme::SIZE_TITLE),
        lerp_colour(theme::p().text_dim, theme::p().cream, hot),
    );
    p.text(
        egui::pos2(r.right() - 30.0, r.center().y),
        egui::Align2::RIGHT_CENTER,
        value.unwrap_or_else(|| "not published".to_owned()),
        egui::FontId::proportional(theme::SIZE_TITLE),
        theme::p().cream,
    );
    Some(resp.on_hover_text("Every figure for this place"))
}

/// One figure to pick: a radio, its name, its scale small, its value here.
/// One is on; tapping another moves it — a pick, never a tick.
fn figure_row(
    ui: &mut egui::Ui,
    on: bool,
    label: &str,
    states: &[cartopolis_scenario::region::State],
    value: Option<&str>,
    band: Option<usize>,
) -> egui::Response {
    let (r, resp) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), ROW_PTS),
        egui::Sense::click(),
    );
    resp.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::RadioButton, ui.is_enabled(), on, label)
    });
    let ctx = ui.ctx();
    let hot = motion::toggle(ctx, resp.id.with("hot"), resp.hovered(), 0.12);
    let lit = motion::toggle(ctx, resp.id.with("on"), on, 0.18);
    let p = ui.painter();
    let fill = lerp_colour(
        lerp_colour(egui::Color32::TRANSPARENT, theme::p().surface_hover, hot),
        theme::p().accent_muted,
        lit,
    );
    p.rect(
        r,
        theme::RADIUS,
        fill,
        egui::Stroke::new(1.5 * lit, theme::p().accent),
        egui::StrokeKind::Inside,
    );
    // The radio: its dot grows in as it is picked.
    let dot = egui::pos2(r.left() + 16.0, r.center().y);
    p.circle_stroke(
        dot,
        7.0,
        egui::Stroke::new(
            2.0,
            lerp_colour(theme::p().line, theme::p().accent, lit.max(hot * 0.5)),
        ),
    );
    if lit > 0.01 {
        p.circle_filled(dot, 3.5 * lit, theme::p().accent);
    }
    let value_left = match value {
        Some(v) => {
            let g = p.layout_no_wrap(
                v.to_owned(),
                egui::FontId::proportional(theme::SIZE_BODY),
                theme::p().cream,
            );
            let at = egui::pos2(
                r.right() - 10.0 - g.size().x,
                r.center().y - g.size().y * 0.5,
            );
            let left = at.x;
            p.galley(at, g, theme::p().cream);
            left
        }
        None => r.right() - 10.0,
    };
    let mini = egui::Rect::from_min_size(
        egui::pos2(
            value_left - 10.0 - MINI_RAMP.x,
            r.center().y - MINI_RAMP.y * 0.5,
        ),
        MINI_RAMP,
    );
    paint_ramp(p, mini, states);
    // Where the place here falls on this figure's scale.
    if let Some(b) = band {
        let w = mini.width() / states.len().max(1) as f32;
        let x = mini.left() + w * (b as f32 + 0.5);
        p.rect(
            egui::Rect::from_center_size(
                egui::pos2(x, mini.center().y),
                egui::vec2(3.0, mini.height() + 6.0),
            ),
            1.0,
            theme::p().cream,
            egui::Stroke::new(1.0, theme::p().ink),
            egui::StrokeKind::Outside,
        );
    }
    let text_left = r.left() + 32.0;
    let g = p.layout(
        label.to_owned(),
        egui::FontId::proportional(theme::SIZE_BODY),
        if on {
            theme::p().cream
        } else {
            theme::p().text
        },
        (mini.left() - 8.0 - text_left).max(40.0),
    );
    p.galley(
        egui::pos2(text_left, r.center().y - g.size().y * 0.5),
        g,
        theme::p().text,
    );
    resp
}

/// A heading over a group of figures.
fn group_label(ui: &mut egui::Ui, text: &str) {
    ui.add_space(6.0);
    ui.label(
        egui::RichText::new(text.to_uppercase())
            .size(theme::SIZE_TINY)
            .color(theme::p().text_dim),
    );
}

/// The lens's header: its mode, its name, and the way to put the sheet away.
fn lens_header(ui: &mut egui::Ui, o: &Open, asked: &mut Asked) {
    let meta = &o.play.scenario.meta;
    ui.horizontal(|ui| {
        mode_chip(ui, meta.mode);
        let w = ui.available_width() - theme::close_button_size() - ui.spacing().item_spacing.x;
        left_sized(
            ui,
            egui::vec2(w.max(40.0), theme::close_button_size()),
            egui::Label::new(
                egui::RichText::new(&meta.name)
                    .size(theme::SIZE_BODY)
                    .color(theme::p().text),
            )
            .truncate(),
        );
        if theme::close_button(ui)
            .on_hover_text("Hide; the lens stays on")
            .clicked()
        {
            asked.sheet = Some(Sheet::Hidden);
        }
    });
}

/// Figures: the open figure's key, then every figure of the lens to pick one
/// from, each with its value here, the clock and the tracks; at its foot the
/// way to another lens, and to none.
fn figures_body(
    ui: &mut egui::Ui,
    o: &mut Open,
    here: Option<LonLat>,
    body_h: f32,
    asked: &mut Asked,
) {
    lens_header(ui, o, asked);
    key_body(ui, o, here, asked);
    ui.add_space(4.0);
    egui::ScrollArea::vertical()
        .max_height((body_h - KEY_PTS).max(BODY_MIN_PTS))
        .auto_shrink([false, true])
        .show(ui, |ui| {
            if o.play.scenario.clock.end > Clock::ZERO {
                clock_bar(ui, o);
                ui.add_space(6.0);
            }
            let file = file_of(o).to_owned();
            let mut group: Option<&str> = None;
            for (i, layer) in o.play.scenario.region_states.iter().enumerate() {
                if layer.group.as_deref() != group {
                    group = layer.group.as_deref();
                    if let Some(g) = group {
                        group_label(ui, g);
                    }
                }
                let value = here.and_then(|p| value_at(o, i, p)).and_then(|(_, v)| v);
                if figure_row(
                    ui,
                    i == o.layer,
                    &layer.label,
                    &layer.states,
                    value.as_deref(),
                    here.and_then(|p| o.band_at(i, p)),
                )
                .clicked()
                {
                    asked.open = Some(format!("{file}/{}", layer.id));
                }
            }
            ui.add_space(8.0);
            scale_card(ui, o);
        });
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.menu_button(egui::RichText::new("Sources").size(theme::SIZE_SUB), |ui| {
            ui.set_width(300.0);
            for s in &o.play.scenario.meta.sources {
                ui.label(egui::RichText::new(s).size(theme::SIZE_SUB));
            }
        });
        if theme::action_button(ui, "All lenses").clicked() {
            asked.sheet = Some(Sheet::Shelf);
        }
        if theme::action_button(ui, "Lens off").clicked() {
            asked.close = true;
        }
    });
}

/// Place: the area under the crosshair with every figure of the lens, each
/// beside its municipality and its country — compared, never ranked. A row
/// puts that figure on the map.
fn place_body(
    ui: &mut egui::Ui,
    o: &mut Open,
    here: Option<LonLat>,
    body_h: f32,
    asked: &mut Asked,
) {
    let found = here.and_then(|p| {
        let set = o.play.layer_areas(o.layer)?;
        Some((p, set, set.area_at(p)?))
    });
    ui.horizontal(|ui| {
        let w = ui.available_width() - theme::close_button_size() - ui.spacing().item_spacing.x;
        let name = found.map_or("No area here", |(_, s, a)| s.areas[a].name.as_str());
        left_sized(
            ui,
            egui::vec2(w.max(40.0), theme::close_button_size()),
            egui::Label::new(
                egui::RichText::new(name)
                    .size(theme::SIZE_TITLE)
                    .color(theme::p().cream),
            )
            .truncate(),
        );
        if theme::close_button(ui)
            .on_hover_text("Back to the figures")
            .clicked()
        {
            asked.sheet = Some(Sheet::Figures);
        }
    });
    let Some((p, set, a)) = found else {
        dim(
            ui,
            "Nothing is published for the place under the crosshair. Move the map to one.",
        );
        return;
    };
    let mut about = vec![set.rung.label().to_owned()];
    if set.rung != Rung::Municipality {
        if let Some(town) = o
            .play
            .areas
            .iter()
            .find(|s| s.rung == Rung::Municipality)
            .and_then(|s| Some(s.areas[s.area_at(p)?].name.clone()))
        {
            about.push(town);
        }
    }
    if let Some(people) = set.field("people").and_then(|f| set.value(a, f)) {
        about.push(format!("{} people", figure(people, "")));
    }
    dim(ui, about.join(" · "));
    ui.add_space(6.0);
    let compare = if set.rung >= Rung::Municipality {
        "town · country"
    } else {
        "country"
    };
    place_row(ui, false, "", "here", compare, true);
    let file = file_of(o).to_owned();
    egui::ScrollArea::vertical()
        .max_height(body_h)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            for (i, layer) in o.play.scenario.region_states.iter().enumerate() {
                let value = value_at(o, i, p)
                    .and_then(|(_, v)| v)
                    .unwrap_or_else(|| "–".into());
                let town = value_on_rung(o, i, p, Rung::Municipality);
                let land = value_on_rung(o, i, p, Rung::Country);
                let context = match (set.rung >= Rung::Municipality, town, land) {
                    (true, Some(t), Some(l)) => format!("{t} · {l}"),
                    (_, _, Some(l)) => l,
                    _ => String::new(),
                };
                if place_row(ui, i == o.layer, &layer.label, &value, &context, false).clicked() {
                    asked.open = Some(format!("{file}/{}", layer.id));
                }
            }
        });
    ui.add_space(6.0);
    dim(ui, &set.attribution);
}

/// A row of the place card: a figure, its value here, and the same figure for
/// the town and the country; `head` draws the column names instead.
fn place_row(
    ui: &mut egui::Ui,
    on: bool,
    label: &str,
    value: &str,
    context: &str,
    head: bool,
) -> egui::Response {
    let h = if head { 16.0 } else { 30.0 };
    let sense = if head {
        egui::Sense::hover()
    } else {
        egui::Sense::click()
    };
    let (r, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), h), sense);
    if !head {
        resp.widget_info(|| {
            egui::WidgetInfo::selected(egui::WidgetType::Button, ui.is_enabled(), on, label)
        });
    }
    let p = ui.painter();
    if on {
        p.rect(
            r,
            theme::RADIUS,
            theme::p().accent_muted,
            egui::Stroke::new(1.5, theme::p().accent),
            egui::StrokeKind::Inside,
        );
    } else if resp.hovered() && !head {
        p.rect_filled(r, theme::RADIUS, theme::p().surface_hover);
    }
    let (size, ink) = if head {
        (theme::SIZE_TINY, theme::p().text_dim)
    } else {
        (theme::SIZE_BODY, theme::p().cream)
    };
    let context_w = 84.0;
    let value_right = r.right() - 10.0 - context_w - 8.0;
    p.text(
        egui::pos2(r.right() - 10.0, r.center().y),
        egui::Align2::RIGHT_CENTER,
        context,
        egui::FontId::proportional(if head {
            theme::SIZE_TINY
        } else {
            theme::SIZE_SUB
        }),
        theme::p().text_dim,
    );
    p.text(
        egui::pos2(value_right, r.center().y),
        egui::Align2::RIGHT_CENTER,
        value,
        egui::FontId::proportional(size),
        ink,
    );
    let g = p.layout(
        label.to_owned(),
        egui::FontId::proportional(theme::SIZE_SUB),
        if on {
            theme::p().cream
        } else {
            theme::p().text
        },
        (value_right - 64.0 - r.left() - 10.0).max(40.0),
    );
    p.galley(
        egui::pos2(r.left() + 10.0, r.center().y - g.size().y * 0.5),
        g,
        theme::p().text,
    );
    resp
}

/// Shelf: the lenses there are, under the question each answers. A lens
/// opens at the figure it had, or its first.
fn shelf_body(ui: &mut egui::Ui, open_key: Option<&str>, body_h: f32, asked: &mut Asked) {
    ui.horizontal(|ui| {
        let w = ui.available_width() - theme::close_button_size() - ui.spacing().item_spacing.x;
        left_sized(
            ui,
            egui::vec2(w.max(40.0), theme::close_button_size()),
            egui::Label::new(
                egui::RichText::new("Lenses")
                    .size(theme::SIZE_HEADING)
                    .color(theme::p().cream),
            ),
        );
        if theme::close_button(ui).clicked() {
            asked.sheet = Some(Sheet::Hidden);
        }
    });
    dim(
        ui,
        "Figures about places, read over the map. One lens at a time.",
    );
    let open_file = open_key.and_then(|k| k.split_once('/')).map(|(f, _)| f);
    egui::ScrollArea::vertical()
        .max_height(body_h)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            let mut questions: Vec<Option<&str>> = Vec::new();
            for s in shelf() {
                if !questions.contains(&s.question.as_deref()) {
                    questions.push(s.question.as_deref());
                }
            }
            for q in questions {
                ui.add_space(8.0);
                ui.label(
                    egui::RichText::new(q.unwrap_or("More"))
                        .size(theme::SIZE_BODY)
                        .color(theme::p().cream),
                );
                ui.add_space(2.0);
                for s in shelf().iter().filter(|s| s.question.as_deref() == q) {
                    let is_open = open_file == Some(s.id);
                    if shelf_card(ui, s, is_open).clicked() {
                        asked.open = Some(match open_key {
                            Some(k) if is_open => k.to_owned(),
                            _ => s.first.clone(),
                        });
                        asked.sheet = Some(Sheet::Figures);
                    }
                }
            }
        });
}

/// One lens on the shelf: its name, what it holds, and how real it is.
fn shelf_card(ui: &mut egui::Ui, s: &Shelved, is_open: bool) -> egui::Response {
    let (colour, _) = mode_frame(s.mode);
    let inner = egui::Frame::NONE
        .fill(theme::p().surface_raised)
        .stroke(if is_open {
            egui::Stroke::new(1.5, colour)
        } else {
            egui::Stroke::NONE
        })
        .corner_radius(theme::RADIUS_LG)
        .inner_margin(egui::Margin::same(10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(
                egui::RichText::new(&s.name)
                    .size(theme::SIZE_BODY)
                    .color(theme::p().cream),
            );
            dim(ui, &s.summary);
            ui.horizontal(|ui| {
                mode_chip(ui, s.mode);
                let n = if s.figures == 1 {
                    "1 figure".to_owned()
                } else {
                    format!("{} figures", s.figures)
                };
                dim(ui, if is_open { format!("{n} · open") } else { n });
            });
        });
    ui.add_space(6.0);
    let resp = inner.response.interact(egui::Sense::click());
    theme::named(ui, resp, &s.name)
}

/// A lens on its way, or one that would not open, with the way out.
fn waiting_body(ui: &mut egui::Ui, loading: bool, error: Option<&str>, asked: &mut Asked) {
    let words = match (loading, error) {
        (_, Some(e)) => format!("Could not open this lens: {e}"),
        (true, _) => "Loading the areas…".to_owned(),
        _ => "Opening…".to_owned(),
    };
    ui.horizontal(|ui| {
        let w = ui.available_width() - theme::close_button_size() - ui.spacing().item_spacing.x;
        left_sized(
            ui,
            egui::vec2(w.max(40.0), theme::close_button_size()),
            egui::Label::new(egui::RichText::new(words).size(theme::SIZE_SUB)).wrap(),
        );
        if theme::close_button(ui).clicked() {
            asked.close = true;
        }
    });
}

fn dim(ui: &mut egui::Ui, text: impl Into<String>) {
    ui.label(
        egui::RichText::new(text.into())
            .size(theme::SIZE_SUB)
            .color(theme::p().text_dim),
    );
}

/// Under the figures: a derived state's bands with how many areas are in
/// each, what is next to be reached, and the tracks to pick one from. A
/// figure's bands are in its key, at the head of the sheet.
fn scale_card(ui: &mut egui::Ui, o: &mut Open) {
    let clock = o.play.scenario.clock;
    let li = o.layer;
    let layer = &o.play.scenario.region_states[li];
    let set = o.play.layer_areas(li);
    let counted = layer.field.is_none();
    if counted {
        group_label(ui, &format!("Areas · {}", layer.label));
        ui.add_space(4.0);
    }
    let places = o
        .frame
        .regions
        .get(li)
        .map(Vec::as_slice)
        .unwrap_or_default();
    // A band's row singles it out on the map, as the key's scale does.
    let mut tapped = None;
    for (si, s) in layer.states.iter().enumerate().filter(|_| counted) {
        let on = o.focus == Some(si);
        let row = ui.horizontal(|ui| {
            let (r, _) = ui.allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::hover());
            ui.painter().rect_filled(r, 2.0, rgb(s.colour, 0.85));
            if on {
                ui.painter().rect_stroke(
                    r,
                    2.0,
                    egui::Stroke::new(1.5, theme::p().cream),
                    egui::StrokeKind::Outside,
                );
            }
            ui.label(
                egui::RichText::new(&s.label)
                    .size(theme::SIZE_SUB)
                    .color(if on {
                        theme::p().cream
                    } else {
                        theme::p().text
                    }),
            );
            // How many regions are under threat: a derived state's count.
            let n = places.iter().filter(|p| p.state == si).count();
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(egui::RichText::new(n.to_string()).size(theme::SIZE_SUB));
            });
        });
        let resp = row.response.interact(egui::Sense::click());
        resp.widget_info(|| {
            egui::WidgetInfo::selected(egui::WidgetType::Button, ui.is_enabled(), on, &s.label)
        });
        let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
        if resp.clicked() {
            tapped = Some(si);
        }
    }
    if let Some(set) = set {
        ui.add_space(6.0);
        ui.label(
            egui::RichText::new(&set.attribution)
                .size(theme::SIZE_TINY)
                .color(theme::p().text_dim),
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
    if let Some(t) = tapped {
        o.focus = if o.focus == Some(t) { None } else { Some(t) };
        ui.ctx().request_repaint();
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

/// The speeds a clock plays at: clock seconds a second, and their words. A
/// clock of years steps through years; any other through minutes.
fn speeds(o: &Open) -> &'static [(f64, &'static str)] {
    const YEAR: f64 = cartopolis_scenario::time::YEAR.0 as f64;
    if o.play.scenario.clock.start_year.is_some() {
        &[(YEAR, "1 yr/s"), (5.0 * YEAR, "5 yr/s")]
    } else {
        &[(1.0, "1×"), (60.0, "60×"), (600.0, "600×")]
    }
}

/// The clock's round play button: a light plate with a triangle, or two
/// bars while it plays (the icon font has no pause).
fn play_button(ui: &mut egui::Ui, playing: bool) -> egui::Response {
    let d = theme::close_button_size().max(32.0);
    let (r, resp) = ui.allocate_exact_size(egui::vec2(d, d), egui::Sense::click());
    let resp = theme::named(ui, resp, if playing { "Pause" } else { "Play" });
    let hot = motion::toggle(ui.ctx(), resp.id.with("hot"), resp.hovered(), 0.12);
    let p = ui.painter();
    let c = r.center();
    p.circle_filled(
        c,
        d * 0.5,
        lerp_colour(theme::p().accent, theme::p().accent_ring, hot),
    );
    let s = d * 0.2;
    if playing {
        for dx in [-0.45, 0.45] {
            p.rect_filled(
                egui::Rect::from_center_size(
                    c + egui::vec2(dx * s, 0.0),
                    egui::vec2(s * 0.5, s * 1.7),
                ),
                1.0,
                theme::p().ink,
            );
        }
    } else {
        p.add(egui::Shape::convex_polygon(
            vec![
                c + egui::vec2(-0.6 * s, -s),
                c + egui::vec2(1.0 * s, 0.0),
                c + egui::vec2(-0.6 * s, s),
            ],
            theme::p().ink,
            egui::Stroke::NONE,
        ));
    }
    resp
}

fn clock_bar(ui: &mut egui::Ui, o: &mut Open) {
    let clock = o.play.scenario.clock;
    let end = clock.end;
    let years = clock.start_year.is_some();
    ui.horizontal(|ui| {
        if play_button(ui, o.playing)
            .on_hover_text(if o.playing { "Pause" } else { "Play" })
            .clicked()
        {
            if !o.playing && o.t >= end {
                o.t = Clock::ZERO;
            }
            o.playing = !o.playing;
        }
        ui.label(
            egui::RichText::new(clock.label(o.t))
                .size(theme::SIZE_HEADING)
                .strong()
                .color(theme::p().cream),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            for &(speed, word) in speeds(o).iter().rev() {
                if theme::chip(ui, word, o.speed == speed).clicked() {
                    o.speed = speed;
                }
            }
        });
    });
    // A slider's rail is `slider_width` long whatever room it is given.
    ui.spacing_mut().slider_width = ui.available_width();
    if years {
        // Whole years: a figure describes a year, never a moment in one.
        let year = cartopolis_scenario::time::YEAR.0;
        let mut y = o.t.0.div_euclid(year);
        let slider = egui::Slider::new(&mut y, 0..=end.0.div_euclid(year)).show_value(false);
        if ui.add(slider).changed() {
            o.t = Clock(y * year);
            o.carry = 0.0;
        }
    } else {
        let mut minutes = o.t.minutes() as f32;
        let slider = egui::Slider::new(&mut minutes, 0.0..=end.minutes() as f32).show_value(false);
        if ui.add(slider).changed() {
            o.t = Clock::from_minutes(f64::from(minutes));
            o.carry = 0.0;
        }
    }
    // Measured, then a model's: the forecast part of the rail hatched
    // (v5-areas), so a 2050 figure never reads as a count.
    if let Some(f) = o.play.scenario.forecast.filter(|&f| f < end) {
        let (r, _) =
            ui.allocate_exact_size(egui::vec2(ui.available_width(), 6.0), egui::Sense::hover());
        let split = r.left() + r.width() * (f.0.max(0) as f32 / end.0.max(1) as f32);
        let p = ui.painter();
        let measured = egui::Rect::from_min_max(r.min, egui::pos2(split, r.max.y));
        p.rect_filled(
            measured,
            egui::CornerRadius::same(theme::RADIUS_SM),
            theme::p().line,
        );
        let model = egui::Rect::from_min_max(egui::pos2(split, r.min.y), r.max);
        p.rect_filled(
            model,
            egui::CornerRadius::same(theme::RADIUS_SM),
            theme::p().surface_raised,
        );
        stripes(p, model, theme::p().line);
    }
    ui.horizontal(|ui| {
        dim(ui, clock.label(Clock::ZERO));
        if let Some(f) = o.play.scenario.forecast.filter(|&f| f < end) {
            dim(ui, format!("· forecast from {}", clock.label(f)));
        }
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

    /// The lens disc: a lens rests with no sheet up and the disc raises and
    /// hides its sheet, the lens staying on; with none open the disc is the
    /// shelf's switch.
    #[test]
    fn the_lens_disc_steps_the_sheet() {
        let mut view = ScenarioView {
            sheet: Sheet::Hidden,
            ..ScenarioView::default()
        };
        assert_eq!(view.sheet.shown(true), Sheet::Hidden);
        assert!(lens_disc_on(&view, true));
        press_lens_disc(&mut view, true);
        assert_eq!(view.sheet, Sheet::Figures);
        press_lens_disc(&mut view, true);
        assert_eq!(view.sheet, Sheet::Hidden);
        view.sheet = Sheet::Place;
        press_lens_disc(&mut view, true);
        assert_eq!(view.sheet, Sheet::Hidden);

        // Closed: the figures and the place have nothing to show.
        view.sheet = Sheet::Figures;
        assert_eq!(view.sheet.shown(false), Sheet::Hidden);
        press_lens_disc(&mut view, false);
        assert_eq!(view.sheet, Sheet::Shelf);
        assert!(lens_disc_on(&view, false));
        press_lens_disc(&mut view, false);
        assert_eq!(view.sheet.shown(false), Sheet::Hidden);
        assert!(!lens_disc_on(&view, false));
    }

    /// Portrait gets the sheet at any width, so the map stays in view above
    /// it; landscape and a desktop get the column.
    #[test]
    fn portrait_gets_the_sheet() {
        assert_eq!(shape(egui::vec2(390.0, 844.0), true), Shape::Sheet);
        assert_eq!(shape(egui::vec2(820.0, 1180.0), false), Shape::Sheet);
        assert_eq!(shape(egui::vec2(844.0, 390.0), false), Shape::Column);
        assert_eq!(shape(egui::vec2(1600.0, 1000.0), false), Shape::Column);
    }

    /// The shelf lists what is shipped, under the question it answers, and
    /// every figure of it sits under a heading.
    #[test]
    fn the_shelf_holds_the_shipped_lenses() {
        let cbs = shelf()
            .iter()
            .find(|s| s.id == "cbs-2024")
            .expect("cbs on the shelf");
        assert_eq!(cbs.question.as_deref(), Some("Who lives here?"));
        assert_eq!(cbs.first, "cbs-2024/share_65_plus");
        let of_cbs = offered()
            .iter()
            .filter(|o| o.key.starts_with("cbs-2024/"))
            .count();
        assert_eq!(cbs.figures, of_cbs);
        assert_eq!(shelf().len(), SHIPPED.len());
        for s in SHIPPED {
            let sc = Scenario::parse(s.toml).expect("parses");
            assert!(sc.meta.question.is_some(), "{}", s.id);
            assert!(
                sc.region_states.iter().all(|r| r.group.is_some()),
                "{}",
                s.id
            );
        }
    }

    /// The planning offices' lenses run on clocks of years, and the
    /// forecasts say where measurement ends.
    #[test]
    fn forecasts_play_in_years_from_where_measuring_ends() {
        for id in ["pbl-2050", "wlo-2060"] {
            let s = SHIPPED.iter().find(|s| s.id == id).expect(id);
            let sc = Scenario::parse(s.toml).expect("parses");
            assert_eq!(sc.clock.label(sc.clock.end), id[4..].to_string(), "{id}");
            assert!(sc.forecast.is_some(), "{id}");
            assert_eq!(default_speed(&sc), cartopolis_scenario::time::YEAR.0 as f64);
        }
    }

    #[test]
    fn a_change_is_signed() {
        assert_eq!(change(4.25), "+4.2 %");
        assert_eq!(change(-12.0), "\u{2212}12 %");
        assert_eq!(change(0.0), "0.0 %");
    }

    #[test]
    fn figures_read_as_people_write_them() {
        assert_eq!(figure(23.4, "%"), "23 %");
        assert_eq!(figure(412.0, "k€"), "€412k");
        assert_eq!(figure(7.25, "%"), "7.2 %");
        assert_eq!(figure(1200.0, ""), "1,200");
        assert_eq!(figure(17_942_942.0, ""), "17,942,942");
        assert_eq!(figure(412.0, ""), "412");
        assert_eq!(figure(-0.526, ""), "-0.53");
    }
}
