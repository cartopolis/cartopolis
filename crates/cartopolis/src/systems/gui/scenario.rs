//! **Scenario layers** over the map: a lens with a clock
//! (`docs/mockups/scenarios/`, `docs/notes/scenarios.md`). The engine is
//! `cartopolis_scenario`, which says what is true at a time on which board; this
//! runs one layer of it, and nothing here knows what a figure or a drone is.
//!
//! **One layer, every rung.** A layer stands on boards at several rungs of the
//! ladder (CBS figures per buurt, wijk, gemeente, provincie and country); each
//! frame the screen's scale picks the finest board whose areas are still wide
//! enough to tell apart (`Play::set_scale`, [`scale_scenario`]), so zooming out
//! turns buurten into wijken into gemeenten without a switch, and a phone that
//! sees the whole country sees its municipalities.
//!
//! **The areas come from our host**, `{CARTO_3DBAG_URL}/areas/<set>.bin`, as
//! `atlas areas` writes them, fetched and decoded off the frame loop when a
//! layer is opened. The layer files are shipped (`assets/scenarios/`).
//!
//! **Meshed by the GPU, answered here.** The areas are `systems::lens_areas`:
//! each board meshed once for the frame the map is in and coloured by a texel
//! an area, so a moving map costs the CPU nothing for them. What is painted
//! here ([`render_scenario_map`], egui's until the library has a canvas),
//! projected through [`place_pins::place_delta`] so the flat map and the
//! globe agree, is only what answers: the outline of the area being read, its
//! card, and a scenario's tracks.
//!
//! The frame says how real it is (v4-whole): published figures and replays in
//! green, an exercise striped yellow, Live red and from an official source only
//! ([`ScenarioView::mode_frame`], painted by [`render_scenario_map`]).
//!
//! **A lens, not a layer** (`docs/mockups/lenses/`). Layers are switches about
//! how the world looks, set and left; a figure is something you read, one at a
//! time, with a source and a mode. So it has its own disc under Layers
//! (`systems::dock`) and one surface of its own ([`Sheet`]): a bottom sheet in
//! portrait, over a map that stays in view while you pick, and a column on the
//! left otherwise, clear of the right-hand panels. The figures in it are radio
//! rows that show their value under the crosshair, so one is picked by reading,
//! not by trying. A lens that is on needs no sheet (v3): the disc is lit, and
//! the ✕ or a drag on the map puts the sheet away, leaving a key strip — the
//! figure and its scale — that raises it again. A pick keeps the sheet up, and
//! its three views (figures, this place, all lenses) are one control under its
//! header. The library draws it (`lib_scenario`) from the [`LensView`] that
//! [`drive_scenario_lens`] publishes, and its taps come back through
//! [`ScenarioView::act`]. It writes `DataLayers::scenario`
//! (`"<file>/<layer>"`), which is what persists; for a capture,
//! `CARTO_SCENARIO=cbs-2024/share_65_plus` and `CARTO_LENS=figures|place|shelf`.

use crate::components::FreeCamera;
use crate::resources::MapAnchor;
use crate::systems::egui_manager::{EguiMut, WorldAnchored};
use crate::systems::globe::GlobeState;
use crate::systems::place_pins;
use crate::systems::theme::{self, scenario as ink};
use crate::systems::ui_input::{UiInput, UiMetrics};
use bevy::prelude::*;
use bevy::tasks::{IoTaskPool, Task};
use big_space::prelude::GridCell;
use cartopolis_scenario::board::{AreaSet, Board, LonLat};
use cartopolis_scenario::ladder::Rung;
use cartopolis_scenario::region::When;
use cartopolis_scenario::time::Time as Clock;
use cartopolis_scenario::{track, Confidence, Frame, Mode, Play, Scenario};

/// How far ahead a track's path and cone are drawn.
const AHEAD: Clock = Clock(150 * 60);

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
    /// The track whose card is open.
    selected: Option<usize>,
    /// The band picked on the scale: every other band fades back.
    focus: Option<usize>,
}

impl Open {
    /// The band the map singles out: the picked one.
    fn singled(&self) -> Option<usize> {
        self.focus
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

    /// The colour each area of the open figure's board is going to, as
    /// `lens_areas` eases and draws it: its band's, and how strongly — a band
    /// singled out on the scale leaves the rest as a hint of where they are,
    /// and a state already passed reads fainter.
    fn targets(&self, set: &AreaSet) -> Vec<[f32; 4]> {
        let mut out = vec![[0.0; 4]; set.areas.len()];
        let Some(places) = self.frame.regions.get(self.layer) else {
            return out;
        };
        let layer = &self.play.scenario.region_states[self.layer];
        let singled = self.singled();
        for p in places {
            let (Some(a), Some(state)) = (p.area, layer.states.get(p.state)) else {
                continue;
            };
            let alpha = match singled {
                Some(band) if band != p.state => 0.08,
                _ if state.when == Some(When::Passed) => 0.30,
                _ => 0.70,
            };
            let [r, g, b] = state.colour.0;
            if let Some(t) = out.get_mut(a) {
                *t = [
                    f32::from(r) / 255.0,
                    f32::from(g) / 255.0,
                    f32::from(b) / 255.0,
                    alpha,
                ];
            }
        }
        out
    }
}

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
    /// The lens as the library draws it (`lib_scenario`): what is up, where
    /// and for which place, worked out by [`drive_scenario_lens`].
    pub lib_view: LensView,
}

impl Default for ScenarioView {
    fn default() -> Self {
        Self {
            open: None,
            loading: None,
            error: None,
            sheet: Sheet::from_env(),
            lib_view: LensView::default(),
        }
    }
}

/// Where the lens stands this frame, for `place_pins::map_area`: its slot in
/// UI points, and whether it is the bottom sheet (else the column on the
/// left); `None` while nothing of it is up. The slot the lens may fill, not
/// the height it took.
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq)]
pub struct LensSlot(pub Option<(Rect, bool)>);

/// What a tap on the lens asked for: it goes through [`ScenarioView::act`].
#[derive(Clone, Debug, PartialEq)]
pub enum LensAct {
    /// Show this much of the lens.
    Sheet(Sheet),
    /// Show this figure, `"<file>/<layer>"`.
    Open(String),
    /// The lens off.
    Close,
    /// Single a band out on the map, or let it go.
    Band(usize),
    /// Play the clock, or pause it.
    Play,
    /// Play at this many clock seconds a second.
    Speed(f64),
    /// Move the clock: whole years on a clock of years, else minutes.
    Seek(f64),
    /// Pick a track, or let it go.
    Track(usize),
}

impl Open {
    /// What a tap on the open lens's own controls asked for.
    fn act(&mut self, a: &LensAct) {
        match *a {
            LensAct::Band(b) => self.focus = if self.focus == Some(b) { None } else { Some(b) },
            LensAct::Play => {
                if !self.playing && self.t >= self.play.scenario.clock.end {
                    self.t = Clock::ZERO;
                }
                self.playing = !self.playing;
            }
            LensAct::Speed(s) => self.speed = s,
            LensAct::Seek(v) => {
                self.t = if self.play.scenario.clock.start_year.is_some() {
                    Clock(v.round() as i64 * cartopolis_scenario::time::YEAR.0)
                } else {
                    Clock::from_minutes(v)
                };
                self.carry = 0.0;
            }
            LensAct::Track(i) => {
                self.selected = if self.selected == Some(i) {
                    None
                } else {
                    Some(i)
                };
            }
            LensAct::Sheet(_) | LensAct::Open(_) | LensAct::Close => {}
        }
    }

    /// Where the clock stands on its rail: whole years, or minutes.
    fn rail_value(&self) -> f64 {
        if self.play.scenario.clock.start_year.is_some() {
            self.t.0.div_euclid(cartopolis_scenario::time::YEAR.0) as f64
        } else {
            self.t.minutes()
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
        true
    }

    /// Do what a tap on the lens asked. A new figure or the lens off is a
    /// write of `DataLayers::scenario`, handed back for the caller to make
    /// (`Some(None)` turns the lens off), so that a tap that changes only the
    /// sheet leaves `DataLayers` unchanged.
    #[must_use]
    pub fn act(&mut self, a: LensAct, wanted: Option<&str>) -> Option<Option<String>> {
        match a {
            LensAct::Sheet(s) => {
                self.sheet = s;
                None
            }
            LensAct::Open(key) => (wanted != Some(key.as_str())).then_some(Some(key)),
            LensAct::Close => {
                self.sheet = Sheet::Hidden;
                wanted.is_some().then_some(None)
            }
            other => {
                if let Some(o) = self.open.as_mut() {
                    o.act(&other);
                }
                None
            }
        }
    }

    fn key(&self) -> Option<&str> {
        self.open
            .as_ref()
            .map(|o| o.key.as_str())
            .or(self.loading.as_ref().map(|l| l.key.as_str()))
            .or(self.error.as_ref().map(|e| e.0.as_str()))
    }

    /// What `lens_areas` draws this frame: the open lens's boards, the one
    /// the scale picked, and its areas' colours.
    pub(crate) fn lens_paint(&self) -> Option<crate::systems::lens_areas::LensPaint<'_>> {
        let o = self.open.as_ref()?;
        let set = o.play.layer_areas(o.layer);
        let active = set.and_then(|s| o.play.areas.iter().position(|a| std::ptr::eq(&**a, s)));
        let [r, g, b, a] = ink::AREA_EDGE.to_array();
        Some(crate::systems::lens_areas::LensPaint {
            lens: file_of(o),
            sets: &o.play.areas,
            active,
            targets: set.map(|s| o.targets(s)).unwrap_or_default(),
            edge: [r, g, b, a].map(|c| f32::from(c) / 255.0),
        })
    }

    /// The open lens's mode, for the line round the whole map in its colour
    /// (striped for an exercise): the one part of a lens that never folds
    /// away. `None` while no lens is open.
    // Waits for the mode frame's port to the library (`lib_scenario`).
    #[allow(dead_code)]
    #[must_use]
    pub fn mode_frame(&self) -> Option<ModeChip> {
        self.open
            .as_ref()
            .map(|o| mode_chip_of(o.play.scenario.meta.mode))
    }

    /// Whether the lens wants frames coming while nothing else moves: its
    /// clock plays, or its colours are still easing (`lens_areas`).
    #[must_use]
    pub fn animating(&self) -> bool {
        self.open.as_ref().is_some_and(|o| o.playing) || crate::systems::lens_areas::easing()
    }
}

/// One area set, fetched from our host and decoded off the frame loop.
async fn fetch_areas(name: String) -> Result<AreaSet, String> {
    let base = crate::services::tdbag_url();
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
    layers: Res<crate::systems::layer_state::DataLayers>,
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
                        selected: None,
                        // `CARTO_LENS_BAND=2` singles out a band, for a capture.
                        focus: crate::env_value("CARTO_LENS_BAND").and_then(|b| b.parse().ok()),
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
    // `scale_scenario` measures; this only runs the clock.
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
    /// Where a place stands on screen, in UI points.
    fn screen(&self, p: LonLat) -> Option<Vec2> {
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
        Some(s / self.ui_scale)
    }

    /// The place on the ground a screen point (UI points) looks at: on the
    /// sphere in space, else on the flat map's plane (its curve is left out;
    /// the pointer's area is read from a height where it is nothing).
    fn ground(&self, at: Vec2) -> Option<LonLat> {
        let ray = self
            .cam
            .viewport_to_world(&self.view, at * self.ui_scale)
            .ok()?;
        let (o, d) = (ray.origin.as_dvec3(), ray.direction.as_vec3().as_dvec3());
        if self.in_space {
            let c = self.globe.center_local.as_dvec3() - self.cam_world;
            let r = f64::from(self.globe.radius);
            let oc = o - c;
            let b = oc.dot(d);
            let disc = b * b - (oc.length_squared() - r * r);
            let t = -b - disc.max(0.0).sqrt();
            if disc < 0.0 || t <= 0.0 {
                return None;
            }
            let dir = (o + d * t - c).normalize().as_vec3();
            let (lat, lng) =
                crate::systems::globe::sub_point(self.globe.anchor_lat, self.globe.anchor_lng, dir);
            return Some([lng, lat]);
        }
        let height = self.cam_world.y + o.y;
        if d.y > -1e-6 || height <= 0.0 {
            return None;
        }
        let t = height / -d.y;
        let (lat, lng) = crate::grid::world_to_lat_lng(
            self.anchor,
            self.cam_world.x + o.x + d.x * t,
            self.cam_world.z + o.z + d.z * t,
        );
        Some([lng, lat])
    }

    /// Where a place stands on screen, as egui's point.
    fn pos(&self, p: LonLat) -> Option<egui::Pos2> {
        self.screen(p).map(|s| egui::pos2(s.x, s.y))
    }

    /// A line through the points, broken where one does not project.
    fn runs(&self, pts: &[LonLat]) -> Vec<Vec<egui::Pos2>> {
        let mut out = vec![Vec::new()];
        for p in pts {
            match self.pos(*p) {
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

/// Read each layer of the open lens on the finest board it can show at the
/// screen's scale, which `lens_areas` then draws.
pub fn scale_scenario(
    mut view: ResMut<ScenarioView>,
    globe: Res<GlobeState>,
    camera: Query<(&Camera, &GridCell, &Transform), With<FreeCamera>>,
    terrain: Res<crate::systems::terrain::TerrainField>,
    map_anchor: Res<MapAnchor>,
    metrics: Res<UiMetrics>,
) {
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
    let proj = Projector {
        globe: &globe,
        anchor,
        cam_world: crate::grid::anchored_position_f64(cell, tf.translation),
        in_space: globe.in_space(cell, tf),
        terrain: &terrain,
        cam,
        // This frame's camera basis (`docs/notes/screen-overlay-projection.md`).
        view: GlobalTransform::from(Transform::from_rotation(tf.rotation)),
        ui_scale: metrics.ui_scale(),
    };
    // The span of a hundredth of a degree of latitude at the view's middle.
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
}

// ---------------------------------------------------------------------------
// Over the map, still egui's: the mode's frame, the area under the pointer
// with its card, and a scenario's tracks. The library has no canvas for
// them yet.
// ---------------------------------------------------------------------------

fn rgb(c: cartopolis_scenario::region::Rgb, alpha: f32) -> egui::Color32 {
    let [r, g, b] = c.0;
    egui::Color32::from_rgba_unmultiplied(r, g, b, (alpha * 255.0) as u8)
}

/// Paint the open lens's mode frame round the map, the area being read and
/// its card, and any tracks. The areas themselves are the GPU's
/// (`systems::lens_areas`), on the board [`scale_scenario`] picked.
#[allow(clippy::too_many_arguments)]
pub fn render_scenario_map(
    mut egui: EguiMut,
    view: Res<ScenarioView>,
    slot: Res<LensSlot>,
    globe: Res<GlobeState>,
    camera: Query<(&Camera, &GridCell, &Transform), With<FreeCamera>>,
    terrain: Res<crate::systems::terrain::TerrainField>,
    map_anchor: Res<MapAnchor>,
    keyboard: Res<crate::systems::onscreen_keyboard::OnScreenKeyboard>,
    route: Res<crate::systems::navigation::RouteState>,
) {
    let mut anchored = WorldAnchored::new(egui.ctx(), "scenario_map");
    let Some(o) = view.open.as_ref() else {
        return;
    };
    let ctx = egui.ctx_mut().clone();
    paint_frame(&ctx, o.play.scenario.meta.mode);
    // A playing clock, or a colour still easing in `lens_areas`, keeps the
    // frames coming.
    if view.animating() {
        ctx.request_repaint();
    }
    let Some(anchor) = map_anchor.geo else {
        return;
    };
    let Ok((cam, cell, tf)) = camera.single() else {
        return;
    };
    let (cam_lat, cam_lng) = crate::systems::geo_nav::camera_geo(anchor, &globe, cell, tf);
    let map_rect = place_pins::map_area(&mut egui, &keyboard, &route, &slot);
    let proj = Projector {
        globe: &globe,
        anchor,
        cam_world: crate::grid::anchored_position_f64(cell, tf.translation),
        in_space: globe.in_space(cell, tf),
        terrain: &terrain,
        cam,
        // This frame's camera basis (`docs/notes/screen-overlay-projection.md`).
        view: GlobalTransform::from(Transform::from_rotation(tf.rotation)),
        ui_scale: egui.ui_scale(),
    };
    // The map's width, but its whole height: the strips the dock reserves
    // are for taps, and a fill stopping short of them reads as a gap. The
    // lens's own column is taken out of the map too, but the map runs on
    // under it, so the fills do: else a bare band stands down the left.
    let mut x_range = map_rect.x_range();
    if let Some((lens, false)) = slot.0 {
        if map_rect.left() >= lens.max.x - 1.0 {
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

    // The pointer, where it rests on the bare map: a finger has no hover, and
    // over a disc or a card the map is not what it points at.
    let pointer = ctx.pointer_hover_pos().filter(|&at| {
        !theme::touch_layout()
            && map_rect.contains(at)
            && ctx
                .layer_id_at(at)
                .is_none_or(|l| l.order == egui::Order::Background)
    });
    let li = o.layer;
    if let (Some(places), Some(set)) = (o.frame.regions.get(li), o.play.layer_areas(li)) {
        let layer = &o.play.scenario.region_states[li];
        // The area under the pointer, and its band.
        let hovered = pointer
            .and_then(|at| proj.ground(Vec2::new(at.x, at.y)))
            .and_then(|p| set.area_at(p))
            .and_then(|a| {
                places
                    .iter()
                    .find(|p| p.area == Some(a))
                    .map(|p| (a, p.state))
            });
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
                        egui::Stroke::new(2.0, theme::p32().cream),
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
            let q = [left[k - 1], right[k - 1], right[k], left[k]].map(|p| proj.pos(p));
            if let [Some(a), Some(b), Some(c), Some(d)] = q {
                let base = cone.vertices.len() as u32;
                for p in [a, b, c, d] {
                    cone.colored_vertex(p, ink::CONE.into());
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
            let Some(s) = proj.pos(a.at) else { continue };
            painter.circle_filled(s, 2.6, ink::TRACK);
            painter.text(
                s - egui::vec2(0.0, 8.0),
                egui::Align2::CENTER_BOTTOM,
                clock.label(a.t),
                egui::FontId::proportional(theme::SIZE_TINY),
                ink::TRACK.into(),
            );
        }
        for &(when, _) in tr.down.iter().filter(|d| d.0 <= o.t && d.0 >= tr.t0) {
            let Some(p) = tr.position(when).and_then(|p| proj.pos(p)) else {
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
        let Some(h) = proj.pos(head) else { continue };
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
            ink::TRACK.into(),
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
        painter.galley(chip.min + theme::CHIP_PADDING, galley, ink::TRACK.into());
    }
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
                        .color(theme::p32().cream),
                );
                ui.label(
                    egui::RichText::new(value.unwrap_or_else(|| "not published".to_owned()))
                        .size(theme::SIZE_HEADING)
                        .strong()
                        .color(theme::p32().cream),
                );
                ui.horizontal(|ui| {
                    let (r, _) =
                        ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
                    ui.painter()
                        .rect_filled(r, theme::RADIUS_SM, rgb(band.colour, 1.0));
                    ui.label(
                        egui::RichText::new(format!("{} · {}", band.label, rung.label()))
                            .size(theme::SIZE_SUB)
                            .color(theme::p32().text_dim),
                    );
                });
            });
        });
}

/// A line round the whole map in the mode's colour, striped for an exercise,
/// on every screen: the one part of a lens that never folds away.
fn paint_frame(ctx: &egui::Context, mode: Mode) {
    let colour = mode_frame(mode).0.to_egui();
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
            edge.rect_filled(r, 0.0, theme::p32().ink);
            stripes(&edge, r, colour);
        } else {
            edge.rect_filled(r, 0.0, colour);
        }
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

/// The colour and words of a mode's frame.
fn mode_frame(mode: Mode) -> (theme::Rgba8, &'static str) {
    match mode {
        Mode::Live => (ink::LIVE, "Live"),
        Mode::Replay => (ink::REPLAY, "Replay"),
        Mode::Exercise => (ink::EXERCISE, "Exercise"),
        Mode::Sandbox => (ink::SANDBOX, "Sandbox"),
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
const HEADER_PTS: f32 = 132.0;
/// The body never shrinks below this, however short the viewport.
const BODY_MIN_PTS: f32 = 120.0;
/// Widest the key strip gets: a name and a scale need no more.
const STRIP_MAX_W: f32 = 340.0;

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

fn shape(viewport: Vec2, compact: bool) -> Shape {
    if compact || viewport.y > viewport.x {
        Shape::Sheet
    } else {
        Shape::Column
    }
}

/// Put the lens away for this frame: nothing of it up, no slot for the pins.
fn put_away(view: &mut ScenarioView, slot: &mut LensSlot) {
    if slot.0.is_some() {
        slot.0 = None;
    }
    if view.lib_view.geom.is_some() {
        view.lib_view = LensView::default();
    }
}

/// The lens (`docs/mockups/lenses/`): works out whether it stands aside (the
/// dock's reasons, the keyboard, the route sheet, the place card), puts the
/// sheet away when a drag starts on the map, finds the place under the
/// crosshair, publishes the lens's slot for the pins ([`LensSlot`]) and the
/// whole lens as data ([`ScenarioView::lib_view`]) for `lib_scenario` to
/// draw. Its taps come back through [`ScenarioView::act`].
#[allow(clippy::too_many_arguments)]
pub fn drive_scenario_lens(
    metrics: Res<UiMetrics>,
    input: Res<UiInput>,
    mut view: ResMut<ScenarioView>,
    mut slot: ResMut<LensSlot>,
    layers: Res<crate::systems::layer_state::DataLayers>,
    right_panel: Res<crate::resources::RightPanel>,
    globe: Res<GlobeState>,
    camera: Query<(&GridCell, &Transform), With<FreeCamera>>,
    map_anchor: Res<MapAnchor>,
    place_card: Res<crate::systems::place_card::PlaceCard>,
    menu: Res<crate::systems::ui::MainMenu>,
    keyboard: Res<crate::systems::onscreen_keyboard::OnScreenKeyboard>,
    driver: Res<crate::systems::driver::DriverState>,
) {
    let lens = layers.scenario.is_some();
    let mut shown = view.sheet.shown(lens);
    // A lens that would not open says so, whether its sheet is up or not.
    if lens && shown == Sheet::Hidden && view.error.is_some() {
        shown = Sheet::Figures;
    }
    let viewport = metrics.viewport_pts();
    let compact = metrics.compact();
    let shape = shape(viewport, compact);
    // The dock's own reasons to stand aside, and on the bottom edge the
    // other things that live there: hidden, never closed.
    let hidden = driver.active
        || menu.open
        || (compact && !right_panel.is(crate::resources::RightPanel::None))
        || (shape == Shape::Sheet
            && (keyboard.is_open()
                || right_panel.is(crate::resources::RightPanel::Route)
                || place_card.place.is_some()));
    // With its sheet put away, a lens that is on keeps only its key strip.
    let strip = shown == Sheet::Hidden;
    if hidden || (strip && view.open.is_none()) {
        put_away(&mut view, &mut slot);
        return;
    }
    // A drag that starts on the map is someone looking at the map: the
    // sheet goes, the lens staying on — and its key strip with it.
    if !strip && input.map_dragged() {
        view.sheet = Sheet::Hidden;
        put_away(&mut view, &mut slot);
        return;
    }

    let here = map_anchor.geo.and_then(|anchor| {
        let (cell, tf) = camera.single().ok()?;
        let (lat, lng) = crate::systems::geo_nav::camera_geo(anchor, &globe, cell, tf);
        Some([lng, lat])
    });
    let bottom = crate::systems::dock::reserved_bottom() + theme::OSD_MARGIN;
    let part = match (strip, shown, view.open.is_some()) {
        (true, ..) => LensPart::Strip,
        (_, Sheet::Shelf, _) => LensPart::Shelf,
        (_, Sheet::Figures, true) => LensPart::Figures,
        (_, Sheet::Place, true) => LensPart::Place,
        _ => LensPart::Waiting,
    };
    let geom = lib_geom(shape, strip, shown == Sheet::Shelf, viewport, bottom);
    // The pins' slot: the room the lens may fill, not the height it took.
    let tall = if strip {
        STRIP_EST_PTS
    } else {
        geom.max_height
    };
    let top = if geom.sheet {
        viewport.y - geom.edge - tall
    } else {
        geom.edge
    };
    let rect = Rect::from_corners(
        Vec2::new(geom.left, top),
        Vec2::new(geom.left + geom.width, top + tall),
    );
    let now = Some((rect, geom.sheet));
    if slot.0 != now {
        slot.0 = now;
    }
    // The library's px are UI points natively, the page's as DOM.
    let k = if cfg!(all(feature = "ui-dom", target_arch = "wasm32")) {
        metrics.ui_scale()
    } else {
        1.0
    };
    let published = view.lens_view(part, layers.scenario.as_deref(), here, geom.scaled(k));
    if view.lib_view != published {
        view.lib_view = published;
    }
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

/// The open lens's file, which every figure's key starts with.
fn file_of(o: &Open) -> &str {
    o.key.split_once('/').map_or(o.key.as_str(), |(f, _)| f)
}

/// The figure a lens on the shelf opens at: the one it had, or its first.
fn shelf_opens(open_key: Option<&str>, s: &Shelved, is_open: bool) -> String {
    match open_key {
        Some(k) if is_open => k.to_owned(),
        _ => s.first.clone(),
    }
}

/// What a lens on its way, or one that would not open, says.
fn waiting_words(loading: bool, error: Option<&str>) -> String {
    match (loading, error) {
        (_, Some(e)) => format!("Could not open this lens: {e}"),
        (true, _) => "Loading the areas…".to_owned(),
        _ => "Opening…".to_owned(),
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

// ---------------------------------------------------------------------------
// The lens as the library draws it (`lib_scenario`)
// ---------------------------------------------------------------------------

/// What the key strip takes over the map, roughly: the pins' rect only.
const STRIP_EST_PTS: f32 = 96.0;

/// Where the library's lens stands.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LensGeom {
    pub left: f32,
    pub width: f32,
    pub max_height: f32,
    /// Along the bottom edge, `edge` above it; else a column hanging `edge`
    /// below the top.
    pub sheet: bool,
    pub edge: f32,
}

impl LensGeom {
    /// In the library's px: UI points natively, `k` times them as DOM.
    fn scaled(self, k: f32) -> Self {
        LensGeom {
            left: self.left * k,
            width: self.width * k,
            max_height: self.max_height * k,
            sheet: self.sheet,
            edge: self.edge * k,
        }
    }
}

/// Where the lens stands, in points, its frame included.
fn lib_geom(shape: Shape, strip: bool, shelf: bool, viewport: Vec2, bottom: f32) -> LensGeom {
    let top = crate::systems::dock::reserved_top();
    let column_x = theme::OSD_MARGIN + theme::ROUND_BUTTON_SIZE + theme::OSD_GAP;
    let column_h = (viewport.y - top - bottom).max(BODY_MIN_PTS);
    match (shape, strip) {
        (Shape::Sheet, _) => {
            let max_w = if strip { STRIP_MAX_W } else { SHEET_MAX_W };
            let width = (viewport.x - 2.0 * theme::OSD_MARGIN).min(max_w).max(0.0);
            let share = if shelf { SHELF_SHARE } else { SHEET_SHARE };
            LensGeom {
                left: ((viewport.x - width) / 2.0).max(0.0),
                width,
                max_height: (viewport.y * share - bottom).max(BODY_MIN_PTS + HEADER_PTS),
                sheet: true,
                edge: bottom,
            }
        }
        (Shape::Column, true) => LensGeom {
            left: column_x,
            width: STRIP_MAX_W,
            max_height: column_h,
            sheet: false,
            edge: top,
        },
        (Shape::Column, false) => LensGeom {
            left: column_x,
            width: COLUMN_W,
            max_height: column_h,
            sheet: false,
            edge: top,
        },
    }
}

/// Which of the lens's surfaces is up.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LensPart {
    /// The key strip over the map, the sheet put away.
    #[default]
    Strip,
    Figures,
    Place,
    Shelf,
    /// On its way, or would not open.
    Waiting,
}

/// A mode as a chip: what kind of truth the colours are.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ModeChip {
    pub word: &'static str,
    pub colour: theme::Rgba8,
    /// An exercise: outlined in the library's chip, striped in the frame.
    pub striped: bool,
}

fn mode_chip_of(mode: Mode) -> ModeChip {
    let (colour, word) = mode_frame(mode);
    ModeChip {
        word,
        colour,
        striped: mode == Mode::Exercise,
    }
}

/// The lens's header: its mode and its name.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LensHead {
    pub mode: Option<ModeChip>,
    pub name: String,
}

/// One band of a figure's scale.
#[derive(Clone, Debug, PartialEq)]
pub struct LensBand {
    pub label: String,
    pub colour: [u8; 3],
}

/// The open figure's key: its title, how real it is, its scale.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LensKey {
    /// The figure and its year.
    pub title: String,
    /// How real, per what, when.
    pub honest: String,
    pub bands: Vec<LensBand>,
    /// The band the map singles out.
    pub singled: Option<usize>,
    /// It was picked: "Show all" lets it go.
    pub focus: Option<usize>,
    /// The band of the place under the crosshair.
    pub marker: Option<usize>,
}

/// The place under the crosshair, as a row that opens the place.
#[derive(Clone, Debug, PartialEq)]
pub struct LensHere {
    pub name: String,
    pub value: String,
    pub below: String,
    pub edge: Option<[u8; 3]>,
}

/// The clock's head: play, where it stands, the speeds.
#[derive(Clone, Debug, PartialEq)]
pub struct LensClock {
    pub playing: bool,
    pub label: String,
    pub speeds: Vec<(f64, &'static str)>,
    pub speed: f64,
}

/// The clock's rail: its length, the forecast's share of it, its ends.
#[derive(Clone, Debug, PartialEq)]
pub struct LensRail {
    pub max: f64,
    /// Where the forecast starts, as a share of the rail.
    pub forecast: Option<f32>,
    pub start: String,
    pub end: String,
    pub forecast_from: Option<String>,
}

/// One figure to pick.
#[derive(Clone, Debug, PartialEq)]
pub struct LensFigure {
    /// A heading over it: the first of its group.
    pub group: Option<String>,
    pub on: bool,
    pub label: String,
    pub ramp: Vec<[u8; 3]>,
    pub value: Option<String>,
    pub band: Option<usize>,
    pub key: String,
}

/// One band of a derived state, with how many areas are in it.
#[derive(Clone, Debug, PartialEq)]
pub struct LensCount {
    pub label: String,
    pub colour: [u8; 3],
    pub count: usize,
    pub on: bool,
}

/// One track to pick.
#[derive(Clone, Debug, PartialEq)]
pub struct LensTrack {
    pub label: String,
    pub selected: bool,
    pub index: usize,
}

/// The picked track: what its cone reaches, and when.
#[derive(Clone, Debug, PartialEq)]
pub struct LensPicked {
    pub line: String,
    pub reach: Vec<(String, String)>,
    pub cone: String,
}

/// Under the figures: a derived state's bands, the attribution, what is next
/// to be reached, the tracks.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LensScale {
    pub counted: Option<String>,
    pub counts: Vec<LensCount>,
    pub attribution: Option<String>,
    pub next: Vec<(String, String)>,
    pub has_tracks: bool,
    pub tracks: Vec<LensTrack>,
    pub picked: Option<LensPicked>,
}

/// One row of the place's figures.
#[derive(Clone, Debug, PartialEq)]
pub struct LensPlaceRow {
    pub on: bool,
    pub label: String,
    pub value: String,
    pub context: String,
    pub key: String,
}

/// The place under the crosshair, every figure of the lens for it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LensPlace {
    pub name: String,
    pub found: bool,
    pub about: String,
    pub compare: &'static str,
    pub rows: Vec<LensPlaceRow>,
    pub attribution: String,
}

/// One lens on the shelf.
#[derive(Clone, Debug, PartialEq)]
pub struct LensShelved {
    pub name: String,
    pub summary: String,
    pub mode: ModeChip,
    pub figures: String,
    pub is_open: bool,
    /// The figure it opens at.
    pub opens: String,
}

/// The lenses there are, under the question each answers.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LensShelf {
    /// A lens is open: the views' control is shown.
    pub tabs: bool,
    pub groups: Vec<(String, Vec<LensShelved>)>,
}

/// The lens as [`drive_scenario_lens`] works it out for the library: only
/// the fields of the part that is up are filled.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LensView {
    /// Where it stands; `None` while nothing of it is up.
    pub geom: Option<LensGeom>,
    pub part: LensPart,
    pub head: LensHead,
    pub key: LensKey,
    pub here: Option<LensHere>,
    pub clock: Option<LensClock>,
    pub rail: Option<LensRail>,
    /// Where the clock stands on the rail.
    pub t: f64,
    pub figures: Vec<LensFigure>,
    pub scale: LensScale,
    pub sources: Vec<String>,
    pub place: LensPlace,
    pub shelf: LensShelf,
    pub waiting: String,
}

impl ScenarioView {
    /// The lens as the library draws it.
    fn lens_view(
        &self,
        part: LensPart,
        open_key: Option<&str>,
        here: Option<LonLat>,
        geom: LensGeom,
    ) -> LensView {
        let mut v = LensView {
            geom: Some(geom),
            part,
            ..LensView::default()
        };
        match (part, self.open.as_ref()) {
            (LensPart::Shelf, _) => v.shelf = shelf_of(open_key),
            (LensPart::Waiting, _) | (_, None) => {
                v.part = LensPart::Waiting;
                v.waiting = waiting_words(
                    self.loading.is_some(),
                    self.error.as_ref().map(|e| e.1.as_str()),
                );
            }
            (part, Some(o)) => {
                let meta = &o.play.scenario.meta;
                v.head = LensHead {
                    mode: Some(mode_chip_of(meta.mode)),
                    name: meta.name.clone(),
                };
                v.key = key_of(o, here);
                if part == LensPart::Figures {
                    v.here = here_of(o, here);
                    if o.play.scenario.clock.end > Clock::ZERO {
                        let (clock, rail) = clock_of(o);
                        v.clock = Some(clock);
                        v.rail = Some(rail);
                        v.t = o.rail_value();
                    }
                    v.figures = figures_of(o, here);
                    v.scale = scale_of(o);
                    v.sources = meta.sources.clone();
                }
                if part == LensPart::Place {
                    v.place = place_of(o, here);
                }
            }
        }
        v
    }
}

/// The open figure's key.
fn key_of(o: &Open, here: Option<LonLat>) -> LensKey {
    let layer = &o.play.scenario.region_states[o.layer];
    let mut title = layer.label.clone();
    let mut per = o
        .play
        .layer_areas(o.layer)
        .map_or("nothing to show at this height".to_owned(), |s| {
            format!("per {}", s.rung.label())
        });
    if let Some(w) = when_label(o) {
        title = format!("{title} · {w}");
        per = format!("{per} · {w}");
    }
    LensKey {
        title,
        honest: format!("{} · {per}", honest(&o.play.scenario.meta)),
        bands: layer
            .states
            .iter()
            .map(|s| LensBand {
                label: s.label.clone(),
                colour: s.colour.0,
            })
            .collect(),
        singled: o.singled(),
        focus: o.focus,
        marker: here.and_then(|p| o.band_at(o.layer, p)),
    }
}

/// The place under the crosshair and the open figure's value there.
fn here_of(o: &Open, here: Option<LonLat>) -> Option<LensHere> {
    let (name, value) = here.and_then(|p| value_at(o, o.layer, p))?;
    let rung = o.play.layer_areas(o.layer).map_or("", |s| s.rung.label());
    let below = match here.and_then(|p| range_at(o, o.layer, p)) {
        Some(r) => format!("{rung} · {r}"),
        None => format!("{rung} · every figure here"),
    };
    Some(LensHere {
        name: name.to_owned(),
        value: value.unwrap_or_else(|| "not published".to_owned()),
        below,
        edge: here
            .and_then(|q| o.band_at(o.layer, q))
            .map(|b| o.play.scenario.region_states[o.layer].states[b].colour.0),
    })
}

/// The clock's head and rail.
fn clock_of(o: &Open) -> (LensClock, LensRail) {
    let clock = o.play.scenario.clock;
    let end = clock.end;
    let forecast = o.play.scenario.forecast.filter(|&f| f < end);
    let max = if clock.start_year.is_some() {
        end.0.div_euclid(cartopolis_scenario::time::YEAR.0) as f64
    } else {
        end.minutes()
    };
    (
        LensClock {
            playing: o.playing,
            label: clock.label(o.t),
            speeds: speeds(o).to_vec(),
            speed: o.speed,
        },
        LensRail {
            max,
            forecast: forecast.map(|f| f.0.max(0) as f32 / end.0.max(1) as f32),
            start: clock.label(Clock::ZERO),
            end: clock.label(end),
            forecast_from: forecast.map(|f| format!("forecast from {}", clock.label(f))),
        },
    )
}

/// Every figure of the lens to pick one from.
fn figures_of(o: &Open, here: Option<LonLat>) -> Vec<LensFigure> {
    let file = file_of(o);
    let mut group: Option<&str> = None;
    let mut out = Vec::new();
    for (i, layer) in o.play.scenario.region_states.iter().enumerate() {
        let mut heading = None;
        if layer.group.as_deref() != group {
            group = layer.group.as_deref();
            heading = group.map(str::to_owned);
        }
        out.push(LensFigure {
            group: heading,
            on: i == o.layer,
            label: layer.label.clone(),
            ramp: layer.states.iter().map(|s| s.colour.0).collect(),
            value: here.and_then(|p| value_at(o, i, p)).and_then(|(_, v)| v),
            band: here.and_then(|p| o.band_at(i, p)),
            key: format!("{file}/{}", layer.id),
        });
    }
    out
}

/// Under the figures: a derived state's bands, the attribution, what is next
/// to be reached, the tracks.
fn scale_of(o: &Open) -> LensScale {
    let clock = o.play.scenario.clock;
    let li = o.layer;
    let layer = &o.play.scenario.region_states[li];
    let set = o.play.layer_areas(li);
    let counted = layer.field.is_none();
    let places = o
        .frame
        .regions
        .get(li)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let mut scale = LensScale {
        counted: counted.then(|| format!("Areas · {}", layer.label)),
        attribution: set.map(|s| s.attribution.clone()),
        has_tracks: !o.play.scenario.tracks.is_empty(),
        ..LensScale::default()
    };
    if counted {
        scale.counts = layer
            .states
            .iter()
            .enumerate()
            .map(|(si, s)| LensCount {
                label: s.label.clone(),
                colour: s.colour.0,
                count: places.iter().filter(|p| p.state == si).count(),
                on: o.focus == Some(si),
            })
            .collect();
    }
    if let Some(set) = set {
        let mut next: Vec<(Clock, &str)> = places
            .iter()
            .filter(|p| layer.states[p.state].within.is_some())
            .filter_map(|p| Some((p.eta?, set.areas[p.area?].name.as_str())))
            .collect();
        next.sort();
        scale.next = next
            .iter()
            .take(6)
            .map(|(eta, name)| {
                let mins = eta.minus(o.t).minutes().round().max(0.0);
                ((*name).to_owned(), format!("{mins:.0} min"))
            })
            .collect();
    }
    scale.tracks = o
        .frame
        .tracks
        .iter()
        .map(|tn| {
            let tr = &o.play.scenario.tracks[tn.track];
            let what = match (tn.moving, tr.count) {
                (Some(n), Some(c)) => format!("{n} of {c}"),
                (Some(_), None) => "moving".to_owned(),
                (None, _) if o.t < tr.t0 => format!("from {}", clock.label(tr.t0)),
                (None, _) => "ended".to_owned(),
            };
            LensTrack {
                label: format!("{} · {what}", tr.id),
                selected: o.selected == Some(tn.track),
                index: tn.track,
            }
        })
        .collect();
    scale.picked = o.selected.map(|i| {
        let tr = &o.play.scenario.tracks[i];
        let reach = match set {
            Some(set) => o
                .play
                .cone_reaches(i, li, o.t, Clock(3 * 3600))
                .iter()
                .take(5)
                .map(|(a, t)| (set.areas[*a].name.clone(), format!("~{}", clock.label(*t))))
                .collect(),
            None => Vec::new(),
        };
        LensPicked {
            line: format!(
                "{} · from {} · {:.0} km/h",
                tr.id,
                clock.label(tr.t0),
                tr.speed_kmh
            ),
            reach,
            cone: format!("The cone widens {:.0} km an hour.", tr.cone_km_per_h),
        }
    });
    scale
}

/// The place under the crosshair with every figure of the lens.
fn place_of(o: &Open, here: Option<LonLat>) -> LensPlace {
    let found = here.and_then(|p| {
        let set = o.play.layer_areas(o.layer)?;
        Some((p, set, set.area_at(p)?))
    });
    let Some((p, set, a)) = found else {
        return LensPlace {
            name: "No area here".to_owned(),
            ..LensPlace::default()
        };
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
    let file = file_of(o);
    let rows = o
        .play
        .scenario
        .region_states
        .iter()
        .enumerate()
        .map(|(i, layer)| {
            let town = value_on_rung(o, i, p, Rung::Municipality);
            let land = value_on_rung(o, i, p, Rung::Country);
            LensPlaceRow {
                on: i == o.layer,
                label: layer.label.clone(),
                value: value_at(o, i, p)
                    .and_then(|(_, v)| v)
                    .unwrap_or_else(|| "–".into()),
                context: match (set.rung >= Rung::Municipality, town, land) {
                    (true, Some(t), Some(l)) => format!("{t} · {l}"),
                    (_, _, Some(l)) => l,
                    _ => String::new(),
                },
                key: format!("{file}/{}", layer.id),
            }
        })
        .collect();
    LensPlace {
        name: set.areas[a].name.clone(),
        found: true,
        about: about.join(" · "),
        compare: if set.rung >= Rung::Municipality {
            "town · country"
        } else {
            "country"
        },
        rows,
        attribution: set.attribution.clone(),
    }
}

/// The shelf, grouped under the question each lens answers.
fn shelf_of(open_key: Option<&str>) -> LensShelf {
    let open_file = open_key.and_then(|k| k.split_once('/')).map(|(f, _)| f);
    let mut questions: Vec<Option<&str>> = Vec::new();
    for s in shelf() {
        if !questions.contains(&s.question.as_deref()) {
            questions.push(s.question.as_deref());
        }
    }
    let groups = questions
        .into_iter()
        .map(|q| {
            let cards = shelf()
                .iter()
                .filter(|s| s.question.as_deref() == q)
                .map(|s| {
                    let is_open = open_file == Some(s.id);
                    let n = if s.figures == 1 {
                        "1 figure".to_owned()
                    } else {
                        format!("{} figures", s.figures)
                    };
                    LensShelved {
                        name: s.name.clone(),
                        summary: s.summary.clone(),
                        mode: mode_chip_of(s.mode),
                        figures: if is_open { format!("{n} · open") } else { n },
                        is_open,
                        opens: shelf_opens(open_key, s, is_open),
                    }
                })
                .collect();
            (q.unwrap_or("More").to_owned(), cards)
        })
        .collect();
    LensShelf {
        tabs: open_key.is_some(),
        groups,
    }
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
        assert_eq!(shape(Vec2::new(390.0, 844.0), true), Shape::Sheet);
        assert_eq!(shape(Vec2::new(820.0, 1180.0), false), Shape::Sheet);
        assert_eq!(shape(Vec2::new(844.0, 390.0), false), Shape::Column);
        assert_eq!(shape(Vec2::new(1600.0, 1000.0), false), Shape::Column);
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
