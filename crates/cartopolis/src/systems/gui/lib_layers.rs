//! The Layers panel drawn by the component library (`lib_host`), beside
//! egui's `layers_panel` — phase 5 of `docs/mockups/component-library`,
//! the second screen after Settings, and the first drawn as the mockup
//! (`layers-and-ring`, v1) had it: six round category buttons with counts,
//! the layers as tiles that are each their own switch, chips for the kinds
//! in a legend, three choice cards for the basemap.
//!
//! On with `CARTO_UI_LIB=layers` (or `1`), on the web `?ui=lib`
//! (`&panel=layers` opens it). egui's panel steps aside (`DataLayers::lib`)
//! keeping an empty panel of the same width. State is bridged as Settings'
//! is: every switch a signal diffed against `DataLayers` each frame, the
//! resource assigned only when something differs.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use bevy::ecs::system::NonSendMarker;
use bevy::prelude::*;
// The library's, not Bevy's: an explicit import outranks the two globs.
use mithril::prelude::*;
use mithril::prelude::{Button, Event};

use crate::resources::{GlobeCloudMode, RightPanel};
use crate::systems::data_layers::{DataLayers, LAYER_DEFS};
use crate::systems::layer_state::LayerCategory;
use crate::systems::layers_panel::PLANS_TILE;
use crate::systems::lib_settings::LibScreens;
use crate::systems::plans::Period;
use crate::systems::ui_input::UiMetrics;
use crate::systems::{icons, theme};

const NEEDS_GROUND: &str = "Needs the ground: pick a map under Map";
const NEAR_GROUND: &str = "Shown near the ground: zoom in";
const FROM_SPACE: &str = "Seen from space: zoom out to see it";

/// A colour as CSS, from the client's palette.
fn css(c: egui::Color32) -> String {
    format!("#{:02x}{:02x}{:02x}", c.r(), c.g(), c.b())
}

/// The signals the panel draws from.
#[derive(Clone)]
struct Live {
    category: Signal<LayerCategory>,
    /// Whose options are open, by tile label.
    open_tile: Signal<Option<&'static str>>,
    in_space: Signal<bool>,
    tdbag_available: Signal<bool>,
    map_enabled: Signal<bool>,
    aerial_enabled: Signal<bool>,
    street_labels: Signal<bool>,
    place_names: Signal<bool>,
    roads_textured: Signal<bool>,
    water_shading: Signal<bool>,
    globe_clouds: Signal<GlobeCloudMode>,
    night_side: Signal<bool>,
    city_lights: Signal<bool>,
    hex_grid: Signal<bool>,
    buildings_enabled: Signal<bool>,
    tdbag_enabled: Signal<bool>,
    osm_buildings_enabled: Signal<bool>,
    trees_enabled: Signal<bool>,
    tree_models_enabled: Signal<bool>,
    grass_enabled: Signal<bool>,
    furniture_enabled: Signal<bool>,
    street_detail_enabled: Signal<bool>,
    flood_defences: Signal<bool>,
    layers: Vec<Signal<bool>>,
    friends_enabled: Signal<bool>,
    community_posts_enabled: Signal<bool>,
    plans_enabled: Signal<bool>,
    plans_kinds: Vec<Signal<bool>>,
    plans_council: Signal<bool>,
    plans_works: Signal<bool>,
    plans_period: Signal<Period>,
    traffic_enabled: Signal<bool>,
    people_enabled: Signal<bool>,
    cyclists_enabled: Signal<bool>,
    birds_enabled: Signal<bool>,
    vehicles_enabled: Signal<bool>,
    sewers: Signal<bool>,
    underground: Vec<Signal<bool>>,
    satellites_enabled: Signal<bool>,
    satellite_kinds: Vec<Signal<bool>>,
    scenario_label: Signal<Option<String>>,
    close: Rc<Cell<bool>>,
    reset: Rc<Cell<bool>>,
    to_about: Rc<Cell<bool>>,
}

impl Live {
    fn new(dl: &DataLayers, in_space: bool, tdbag_available: bool) -> Self {
        let sig = |v: bool| signal(v);
        Live {
            category: signal(dl.category),
            open_tile: signal(dl.open_tile),
            in_space: signal(in_space),
            tdbag_available: signal(tdbag_available),
            map_enabled: sig(dl.map_enabled),
            aerial_enabled: sig(dl.aerial_enabled),
            street_labels: sig(dl.street_labels),
            place_names: sig(dl.place_names),
            roads_textured: sig(dl.roads_textured),
            water_shading: sig(dl.water_shading),
            globe_clouds: signal(dl.globe_clouds),
            night_side: sig(dl.night_side),
            city_lights: sig(dl.city_lights),
            hex_grid: sig(dl.hex_grid),
            buildings_enabled: sig(dl.buildings_enabled),
            tdbag_enabled: sig(dl.tdbag_enabled),
            osm_buildings_enabled: sig(dl.osm_buildings_enabled),
            trees_enabled: sig(dl.trees_enabled),
            tree_models_enabled: sig(dl.tree_models_enabled),
            grass_enabled: sig(dl.grass_enabled),
            furniture_enabled: sig(dl.furniture_enabled),
            street_detail_enabled: sig(dl.street_detail_enabled),
            flood_defences: sig(dl.flood_defences),
            layers: dl.layers.iter().map(|l| sig(l.enabled)).collect(),
            friends_enabled: sig(dl.friends_enabled),
            community_posts_enabled: sig(dl.community_posts_enabled),
            plans_enabled: sig(dl.plans_enabled),
            plans_kinds: dl.plans_kinds.iter().map(|v| sig(*v)).collect(),
            plans_council: sig(dl.plans_council),
            plans_works: sig(dl.plans_works),
            plans_period: signal(dl.plans_period),
            traffic_enabled: sig(dl.traffic_enabled),
            people_enabled: sig(dl.people_enabled),
            cyclists_enabled: sig(dl.cyclists_enabled),
            birds_enabled: sig(dl.birds_enabled),
            vehicles_enabled: sig(dl.vehicles_enabled),
            sewers: sig(dl.sewers),
            underground: dl.underground.iter().map(|v| sig(*v)).collect(),
            satellites_enabled: sig(dl.satellites_enabled),
            satellite_kinds: dl.satellite_kinds.iter().map(|v| sig(*v)).collect(),
            scenario_label: signal(scenario_label(dl)),
            close: Rc::new(Cell::new(false)),
            reset: Rc::new(Cell::new(false)),
            to_about: Rc::new(Cell::new(false)),
        }
    }

    /// The plain switches, as the resource's fields.
    fn bool_fields<'a>(
        &'a self,
        dl: &'a mut DataLayers,
    ) -> Vec<(&'static str, &'a Signal<bool>, &'a mut bool)> {
        vec![
            ("map_enabled", &self.map_enabled, &mut dl.map_enabled),
            (
                "aerial_enabled",
                &self.aerial_enabled,
                &mut dl.aerial_enabled,
            ),
            ("street_labels", &self.street_labels, &mut dl.street_labels),
            ("place_names", &self.place_names, &mut dl.place_names),
            (
                "roads_textured",
                &self.roads_textured,
                &mut dl.roads_textured,
            ),
            ("water_shading", &self.water_shading, &mut dl.water_shading),
            ("night_side", &self.night_side, &mut dl.night_side),
            ("city_lights", &self.city_lights, &mut dl.city_lights),
            ("hex_grid", &self.hex_grid, &mut dl.hex_grid),
            (
                "buildings_enabled",
                &self.buildings_enabled,
                &mut dl.buildings_enabled,
            ),
            ("tdbag_enabled", &self.tdbag_enabled, &mut dl.tdbag_enabled),
            (
                "osm_buildings_enabled",
                &self.osm_buildings_enabled,
                &mut dl.osm_buildings_enabled,
            ),
            ("trees_enabled", &self.trees_enabled, &mut dl.trees_enabled),
            (
                "tree_models_enabled",
                &self.tree_models_enabled,
                &mut dl.tree_models_enabled,
            ),
            ("grass_enabled", &self.grass_enabled, &mut dl.grass_enabled),
            (
                "furniture_enabled",
                &self.furniture_enabled,
                &mut dl.furniture_enabled,
            ),
            (
                "street_detail_enabled",
                &self.street_detail_enabled,
                &mut dl.street_detail_enabled,
            ),
            (
                "flood_defences",
                &self.flood_defences,
                &mut dl.flood_defences,
            ),
            (
                "friends_enabled",
                &self.friends_enabled,
                &mut dl.friends_enabled,
            ),
            (
                "community_posts_enabled",
                &self.community_posts_enabled,
                &mut dl.community_posts_enabled,
            ),
            ("plans_enabled", &self.plans_enabled, &mut dl.plans_enabled),
            ("plans_council", &self.plans_council, &mut dl.plans_council),
            ("plans_works", &self.plans_works, &mut dl.plans_works),
            (
                "traffic_enabled",
                &self.traffic_enabled,
                &mut dl.traffic_enabled,
            ),
            (
                "people_enabled",
                &self.people_enabled,
                &mut dl.people_enabled,
            ),
            (
                "cyclists_enabled",
                &self.cyclists_enabled,
                &mut dl.cyclists_enabled,
            ),
            ("birds_enabled", &self.birds_enabled, &mut dl.birds_enabled),
            (
                "vehicles_enabled",
                &self.vehicles_enabled,
                &mut dl.vehicles_enabled,
            ),
            ("sewers", &self.sewers, &mut dl.sewers),
            (
                "satellites_enabled",
                &self.satellites_enabled,
                &mut dl.satellites_enabled,
            ),
        ]
    }
}

/// The lens open under Map, if one is, by label.
fn scenario_label(dl: &DataLayers) -> Option<String> {
    dl.scenario.as_deref().and_then(|k| {
        crate::systems::scenario::offered()
            .iter()
            .find(|o| o.key == k)
            .map(|o| o.label.to_string())
    })
}

/// What is on per category, for the badges (`layers_panel::category_counts`).
fn counts(dl: &DataLayers) -> [u64; 6] {
    let n = |xs: &[bool]| xs.iter().filter(|x| **x).count() as u64;
    let places: Vec<bool> = dl
        .layers
        .iter()
        .map(|l| l.enabled)
        .chain([
            dl.friends_enabled,
            dl.community_posts_enabled,
            dl.plans_enabled,
        ])
        .collect();
    let under: Vec<bool> = std::iter::once(dl.sewers)
        .chain(dl.underground.iter().copied())
        .collect();
    [
        n(&[
            dl.street_labels,
            dl.place_names,
            dl.roads_textured,
            dl.water_shading,
        ]),
        n(&[
            dl.buildings_enabled,
            dl.trees_enabled,
            dl.tree_models_enabled,
            dl.grass_enabled,
            dl.furniture_enabled,
            dl.street_detail_enabled,
            dl.flood_defences,
        ]),
        n(&places),
        n(&[
            dl.traffic_enabled,
            dl.people_enabled,
            dl.cyclists_enabled,
            dl.birds_enabled,
            dl.vehicles_enabled,
        ]),
        n(&under),
        n(&[dl.satellites_enabled]),
    ]
}

/// The mounted panel: its signals, the last synced state, and the counts
/// signal the strip reads.
struct Mounted {
    l: Live,
    counts: Signal<[u64; 6]>,
    last_bools: Vec<bool>,
    last_layers: Vec<bool>,
    last_plans_kinds: Vec<bool>,
    last_underground: Vec<bool>,
    last_satellite_kinds: Vec<bool>,
    last_category: LayerCategory,
    last_open_tile: Option<&'static str>,
    last_clouds: GlobeCloudMode,
    last_period: Period,
}

impl Mounted {
    fn new(dl: &mut DataLayers, in_space: bool, tdbag_available: bool) -> Self {
        let l = Live::new(dl, in_space, tdbag_available);
        let last_bools = l.bool_fields(dl).iter().map(|(_, _, v)| **v).collect();
        Mounted {
            counts: signal(counts(dl)),
            last_bools,
            last_layers: dl.layers.iter().map(|l| l.enabled).collect(),
            last_plans_kinds: dl.plans_kinds.to_vec(),
            last_underground: dl.underground.to_vec(),
            last_satellite_kinds: dl.satellite_kinds.to_vec(),
            last_category: dl.category,
            last_open_tile: dl.open_tile,
            last_clouds: dl.globe_clouds,
            last_period: dl.plans_period,
            l,
        }
    }

    /// Signals that moved write the resource; fields that moved write the
    /// signals. Answers whether the resource changed.
    fn sync(&mut self, dl: &mut DataLayers) -> bool {
        let mut changed = false;
        macro_rules! one {
            ($sig:expr, $field:expr, $last:expr) => {{
                let s = $sig.get();
                if s != $last {
                    $field = s.clone();
                    $last = s;
                    changed = true;
                } else if $field != $last {
                    $sig.set($field.clone());
                    $last = $field.clone();
                }
            }};
        }
        {
            let l = self.l.clone();
            for (i, (_, sig, field)) in l.bool_fields(dl).into_iter().enumerate() {
                let s = sig.get();
                if s != self.last_bools[i] {
                    *field = s;
                    self.last_bools[i] = s;
                    changed = true;
                } else if *field != self.last_bools[i] {
                    sig.set(*field);
                    self.last_bools[i] = *field;
                }
            }
        }
        macro_rules! list {
            ($sigs:expr, $fields:expr, $last:expr, $get:expr, $set:expr) => {
                for (i, sig) in $sigs.iter().enumerate() {
                    let s = sig.get();
                    let f = $get(&$fields[i]);
                    if s != $last[i] {
                        $set(&mut $fields[i], s);
                        $last[i] = s;
                        changed = true;
                    } else if f != $last[i] {
                        sig.set(f);
                        $last[i] = f;
                    }
                }
            };
        }
        list!(
            self.l.layers,
            dl.layers,
            self.last_layers,
            |l: &crate::systems::layer_state::LayerRuntime| l.enabled,
            |l: &mut crate::systems::layer_state::LayerRuntime, v| l.enabled = v
        );
        list!(
            self.l.plans_kinds,
            dl.plans_kinds,
            self.last_plans_kinds,
            |v: &bool| *v,
            |v: &mut bool, s| *v = s
        );
        list!(
            self.l.underground,
            dl.underground,
            self.last_underground,
            |v: &bool| *v,
            |v: &mut bool, s| *v = s
        );
        list!(
            self.l.satellite_kinds,
            dl.satellite_kinds,
            self.last_satellite_kinds,
            |v: &bool| *v,
            |v: &mut bool, s| *v = s
        );
        one!(self.l.category, dl.category, self.last_category);
        one!(self.l.open_tile, dl.open_tile, self.last_open_tile);
        one!(self.l.globe_clouds, dl.globe_clouds, self.last_clouds);
        one!(self.l.plans_period, dl.plans_period, self.last_period);
        let c = counts(dl);
        if self.counts.get() != c {
            self.counts.set(c);
        }
        let label = scenario_label(dl);
        if self.l.scenario_label.get() != label {
            self.l.scenario_label.set(label);
        }
        changed
    }
}

thread_local! {
    static MOUNTED: RefCell<Option<Mounted>> = const { RefCell::new(None) };
}

/// Mounts, syncs and unmounts the panel as `RightPanel::Layers` comes and
/// goes. `PostUpdate`, before the library applies the frame's operations.
#[allow(clippy::too_many_arguments)]
pub fn drive_lib_layers(
    _main: NonSendMarker,
    lib: Res<LibScreens>,
    mut right_panel: ResMut<RightPanel>,
    mut dl: ResMut<DataLayers>,
    metrics: Res<UiMetrics>,
    layer_view: crate::systems::layers_panel::LayerView,
    mut opened: Local<bool>,
) {
    if !lib.layers {
        return;
    }
    if !dl.lib {
        dl.lib = true;
    }
    if lib.open_layers && !*opened {
        *opened = true;
        *right_panel = RightPanel::Layers;
    }
    let open = right_panel.is(RightPanel::Layers);
    let in_space = layer_view.in_space();
    let tdbag_available = crate::systems::osm_buildings::tdbag_attribution().is_some();
    MOUNTED.with(|m| {
        let mut m = m.borrow_mut();
        match (open, m.as_mut()) {
            (false, None) => {}
            (false, Some(_)) => {
                *m = None;
                crate::systems::lib_host::clear_keyed("layers");
            }
            (true, None) => {
                // A category asked for by name (`CARTO_UI_LIB_PAGE=city`,
                // `&page=city`), for a capture.
                if let Some(cat) = lib.page.as_deref().and_then(|p| {
                    LayerCategory::ALL
                        .into_iter()
                        .find(|c| c.label().eq_ignore_ascii_case(p))
                }) {
                    if dl.category != cat {
                        dl.category = cat;
                    }
                }
                let mounted = Mounted::new(dl.bypass_change_detection(), in_space, tdbag_available);
                let compact = metrics.compact();
                let p = theme::p();
                crate::systems::lib_host::mount_keyed(
                    "layers",
                    panel(&mounted.l, &mounted.counts, compact),
                    p.ink == theme::DARK.ink,
                    theme::touch_layout(),
                );
                if let Some(path) = std::env::var_os("CARTO_UI_LIB_HTML") {
                    let html = html(&mounted.l, &mounted.counts, compact);
                    if let Err(e) = std::fs::write(&path, html) {
                        warn!(?path, error = %e, "lib layers: could not write the HTML");
                    }
                }
                *m = Some(mounted);
            }
            (true, Some(mounted)) => {
                if mounted.l.reset.take() {
                    reset(&mut dl);
                }
                // Written through `bypass_change_detection` and marked only
                // when something differs: `DataLayers::is_changed` restarts
                // streaming work.
                let inner = dl.bypass_change_detection();
                let changed = mounted.sync(inner);
                if changed {
                    dl.set_changed();
                }
                if mounted.l.in_space.get() != in_space {
                    mounted.l.in_space.set(in_space);
                }
                if mounted.l.tdbag_available.get() != tdbag_available {
                    mounted.l.tdbag_available.set(tdbag_available);
                }
                if mounted.l.close.take() {
                    *right_panel = RightPanel::None;
                }
                if mounted.l.to_about.take() {
                    *right_panel = RightPanel::About;
                }
            }
        }
    });
}

/// Every layer back to how it ships (`layers_panel`'s Reset).
fn reset(dl: &mut DataLayers) {
    let fresh = DataLayers::default();
    for (layer, def) in dl.layers.iter_mut().zip(fresh.layers.iter()) {
        layer.enabled = def.enabled;
    }
    dl.buildings_enabled = fresh.buildings_enabled;
    dl.tdbag_enabled = fresh.tdbag_enabled;
    dl.osm_buildings_enabled = fresh.osm_buildings_enabled;
    dl.map_enabled = fresh.map_enabled;
    dl.vehicles_enabled = fresh.vehicles_enabled;
    dl.trees_enabled = fresh.trees_enabled;
    dl.tree_models_enabled = fresh.tree_models_enabled;
    dl.grass_enabled = fresh.grass_enabled;
    dl.furniture_enabled = fresh.furniture_enabled;
    dl.roads_textured = fresh.roads_textured;
    dl.water_shading = fresh.water_shading;
    dl.street_labels = fresh.street_labels;
    dl.place_names = fresh.place_names;
    dl.globe_clouds = fresh.globe_clouds;
    dl.night_side = fresh.night_side;
    dl.city_lights = fresh.city_lights;
    dl.hex_grid = fresh.hex_grid;
    dl.scenario = fresh.scenario.clone();
    dl.flood_defences = fresh.flood_defences;
    dl.sewers = fresh.sewers;
    dl.underground = fresh.underground;
    dl.traffic_enabled = fresh.traffic_enabled;
    dl.people_enabled = fresh.people_enabled;
    dl.cyclists_enabled = fresh.cyclists_enabled;
    dl.birds_enabled = fresh.birds_enabled;
    dl.friends_enabled = fresh.friends_enabled;
    dl.community_posts_enabled = fresh.community_posts_enabled;
    dl.street_detail_enabled = fresh.street_detail_enabled;
    dl.aerial_enabled = fresh.aerial_enabled;
    dl.satellites_enabled = fresh.satellites_enabled;
    dl.satellite_kinds = fresh.satellite_kinds;
    dl.plans_enabled = fresh.plans_enabled;
    dl.plans_kinds = fresh.plans_kinds;
    dl.plans_council = fresh.plans_council;
    dl.plans_works = fresh.plans_works;
    dl.plans_period = fresh.plans_period;
}

/// The panel's HTML from the library's string renderer, for the browser
/// beside the capture.
fn html(l: &Live, counts: &Signal<[u64; 6]>, compact: bool) -> String {
    use mithril::view::{mount, StringRenderer};
    let r = Rc::new(RefCell::new(StringRenderer::new()));
    let root = r.borrow().root();
    let mounted = mount(panel(l, counts, compact), &r, &root, None);
    let out = r.borrow().html();
    drop(mounted);
    out
}

// ─── The view ────────────────────────────────────────────────────────────────

fn icon(glyph: &'static str) -> View {
    view! {
        <span style="font-family: 'Material Symbols Rounded'; font-size: 18px; line-height: 1">{glyph}</span>
    }
}

fn note(text: impl Into<String>) -> View {
    let text: String = text.into();
    view! {
        <p style="margin: 0; color: var(--m-text-dim); font-size: var(--m-font-size-sm); line-height: 1.4">{text}</p>
    }
}

fn small(text: impl Into<String>) -> View {
    let text: String = text.into();
    view! {
        <p style="margin: 0; color: var(--m-text-dim); font-size: 11px; line-height: 1.4">{text}</p>
    }
}

/// A group's heading inside a category, set as the library's section titles
/// are, with room above it so the groups read apart.
fn group(text: &'static str) -> View {
    view! {
        <p style="margin: 12px 0 0; color: var(--m-text-dim); font-size: var(--m-font-size-sm); font-weight: 650; letter-spacing: 0.06em; text-transform: uppercase">{text}</p>
    }
}

fn on(f: impl Fn() + 'static) -> Option<Handler> {
    Some(Rc::new(move |_: &Event| f()) as Handler)
}

/// A tile over a switch signal; `more` toggles its options open.
fn tile(
    l: &Live,
    model: &Signal<bool>,
    glyph: &'static str,
    label: &'static str,
    nl: bool,
    more: bool,
    unseen: Option<&'static str>,
) -> View {
    let open_tile = l.open_tile.clone();
    let more_handler = more.then(|| {
        let open_tile = open_tile.clone();
        Rc::new(move |_: &Event| {
            open_tile.update(|o| *o = if *o == Some(label) { None } else { Some(label) });
        }) as Handler
    });
    let is_open = l.open_tile.get() == Some(label);
    let badge: View = if nl { "NL".into_view() } else { View::Empty };
    let hint = unseen.map(str::to_owned);
    view! {
        <Tile model={model.clone()} icon={icon(glyph)} badge={badge} hint={hint} more={more_handler} open={is_open}>{label}</Tile>
    }
}

/// A box of options under a tile (`layers_panel::options_box`).
fn options_box(title: &'static str, body: View) -> View {
    view! {
        <div style="display: flex; flex-direction: column; gap: 8px; padding: 10px; background: var(--m-surface-raised); border: 1px solid var(--m-border); border-radius: var(--m-radius-lg)">
            <p style="margin: 0; font-size: var(--m-font-size-sm); font-weight: 600; color: var(--m-text-strong)">{title}</p>
            {body}
        </div>
    }
}

/// The panel: a header with Reset and ✕, the category strip, the category's
/// body, the credits.
fn panel(l: &Live, counts: &Signal<[u64; 6]>, compact: bool) -> View {
    let frame = if compact {
        "position: absolute; left: 0; right: 0; top: 0; bottom: 0; display: flex; flex-direction: column; background: var(--m-bg)"
    } else {
        "position: absolute; right: 0; top: 0; bottom: 0; width: 300px; display: flex; flex-direction: column; background: var(--m-bg); border-left: 1px solid var(--m-border)"
    };
    let close = {
        let close = l.close.clone();
        on(move || close.set(true))
    };
    let reset = {
        let reset = l.reset.clone();
        on(move || reset.set(true))
    };
    let strip = {
        let (category, counts) = (l.category.clone(), counts.clone());
        move || -> View {
            let c = counts.get();
            let tabs: Vec<IconTab<LayerCategory>> = LayerCategory::ALL
                .iter()
                .enumerate()
                .map(|(i, cat)| IconTab::new(*cat, icon(cat.icon()), cat.label()).badge(c[i]))
                .collect();
            view! { <IconTabs model={category.clone()} tabs={tabs} /> }
        }
    };
    let subtitle = {
        let category = l.category.clone();
        move || -> View { note(category.get().subtitle()) }
    };
    let body = {
        let l = l.clone();
        move || -> View {
            match l.category.get() {
                LayerCategory::Map => map(&l),
                LayerCategory::City => city(&l),
                LayerCategory::Places => places(&l),
                LayerCategory::Moving => moving(&l),
                LayerCategory::Under => under(&l),
                LayerCategory::Orbit => orbit(&l),
            }
        }
    };
    let about = {
        let to_about = l.to_about.clone();
        on(move || to_about.set(true))
    };
    let credits = credits();
    view! {
        <div style={frame}>
            <div style="display: flex; align-items: center; gap: 12px; min-height: 56px; padding: 0 12px 0 20px; border-bottom: 1px solid var(--m-border)">
                <span style="color: var(--m-accent)">{icon(RightPanel::Layers.icon())}</span>
                <h1 style="flex: 1; margin: 0; font-size: 19px; font-weight: 600; color: var(--m-text-strong)">"Layers"</h1>
                <Button variant=Variant::Ghost size=Size::Sm onclick={reset} title="Switch every layer back to how it ships">"Reset"</Button>
                <CloseButton onclick={close} />
            </div>
            <div style="flex: 1; min-height: 0; overflow-y: auto; padding: 20px 20px 28px; display: flex; flex-direction: column; gap: 16px">
                {strip}
                {subtitle}
                {body}
                <div style="display: flex; flex-direction: column; gap: 4px; margin-top: 8px; padding-top: 8px; border-top: 1px solid var(--m-border)">
                    {credits}
                    <div><Button variant=Variant::Ghost size=Size::Sm onclick={about}>"Full credits and licences…"</Button></div>
                </div>
            </div>
        </div>
    }
}

/// The attributions the egui panel lists, as small lines.
fn credits() -> View {
    let mut lines: Vec<String> = [
        crate::systems::osm_buildings::tdbag_attribution(),
        crate::systems::building_facts::bag_attribution(),
        crate::systems::surveyed::bgt_attribution(),
        crate::systems::municipal::attribution(),
        crate::systems::crops::brp_attribution(),
        crate::systems::dikes::dike_attribution(),
        crate::systems::sewers::sewer_attribution(),
    ]
    .into_iter()
    .flatten()
    .collect();
    lines.extend(crate::systems::underground::attributions());
    lines.extend(
        [
            crate::systems::lod3::lod3_attribution(),
            crate::systems::roof_colours::roofs_attribution(),
            crate::systems::facade_colours::facades_attribution(),
        ]
        .into_iter()
        .flatten(),
    );
    if crate::systems::facade_colours::edits_present() {
        lines.push(
            "Facade corrections from street photos © Mapillary contributors (CC BY-SA 4.0) and players"
                .to_owned(),
        );
    }
    lines.push(
        "Data © OpenStreetMap contributors (ODbL), via vector tiles + Overpass API".to_owned(),
    );
    lines.push("Land cover © ESA WorldCover 2021 (CC BY 4.0), via VersaTiles".to_owned());
    lines.push("Live departures & vehicles: OVapi (v0 / gtfs.ovapi.nl)".to_owned());
    lines.push("Level crossings: ProRail Spoorwegen, served by PDOK (CC0)".to_owned());
    let views: Vec<View> = lines.into_iter().map(small).collect();
    view! { {views} }
}

fn map(l: &Live) -> View {
    let ground = (!l.map_enabled.get()).then_some(NEEDS_GROUND);
    let low = l.in_space.get().then_some(NEAR_GROUND);
    // The ground itself: one choice of three, as choice tiles over the two
    // switches they stand for.
    let choice = |label: &'static str, glyph: &'static str, map: bool, aerial: bool| -> View {
        let selected =
            signal(l.map_enabled.get() == map && (!map || l.aerial_enabled.get() == aerial));
        let (m, a) = (l.map_enabled.clone(), l.aerial_enabled.clone());
        let pick: Rc<dyn Fn(&bool)> = Rc::new(move |_| {
            m.set(map);
            a.set(aerial);
        });
        view! { <Tile model={selected} icon={icon(glyph)} choice=true onchange={Some(pick)}>{label}</Tile> }
    };
    let clouds = opts(&[
        (GlobeCloudMode::Off, "Off"),
        (GlobeCloudMode::Light, "Light"),
        (GlobeCloudMode::Full, "Full"),
    ]);
    let lens: View = match l.scenario_label.get() {
        Some(label) => view! {
            {note(format!("Lens open: {label}"))}
            {small("Change it with the lens button, under this panel's")}
        },
        None => View::Empty,
    };
    view! {
        <TileGrid>
            {choice("Drawn map", icons::CAT_MAP, true, false)}
            {choice("Aerial photo", icons::AERIAL, true, true)}
            {choice("No ground", icons::NO_GROUND, false, false)}
        </TileGrid>
        {group("On the map")}
        <TileGrid>
            {tile(l, &l.street_labels, icons::LABEL, "Street names", false, false, ground.or(low))}
            {tile(l, &l.place_names, icons::LABEL, "Place names", false, false, None)}
            {tile(l, &l.roads_textured, icons::ROAD, "Road markings", false, false, low)}
            {tile(l, &l.water_shading, icons::WATER, "Moving water", false, false, low)}
        </TileGrid>
        {group("From above")}
        <FieldSegmented model={l.globe_clouds.clone()} options={clouds} label="Clouds" size=Some(Size::Sm) id="ly-clouds" />
        <TileGrid>
            {tile(l, &l.night_side, icons::CAT_ORBIT, "Night side", false, false, None)}
            {tile(l, &l.city_lights, icons::STAR, "City lights", false, false, None)}
            {tile(l, &l.hex_grid, icons::TUNE, "Hex grid", false, false, None)}
        </TileGrid>
        {lens}
    }
}

fn city(l: &Live) -> View {
    let low = l.in_space.get().then_some(NEAR_GROUND);
    let ground = (!l.map_enabled.get()).then_some(NEEDS_GROUND);
    let sources: View = if l.open_tile.get() == Some("Buildings") {
        let off = !l.buildings_enabled.get();
        let tdbag: View = if l.tdbag_available.get() {
            view! { <FieldCheckbox model={l.tdbag_enabled.clone()} label="3DBAG (NL)" id="ly-tdbag" disabled={off} /> }
        } else {
            View::Empty
        };
        options_box(
            "Buildings: sources",
            view! {
                {tdbag}
                <FieldCheckbox model={l.osm_buildings_enabled.clone()} label="OSM 3D buildings and windows" id="ly-osm" disabled={off} />
            },
        )
    } else {
        View::Empty
    };
    view! {
        <TileGrid>
            {tile(l, &l.buildings_enabled, icons::BUILDINGS, "Buildings", false, true, low)}
            {tile(l, &l.trees_enabled, icons::LEISURE, "Trees", false, false, low)}
            {tile(l, &l.tree_models_enabled, icons::LEISURE, "Tree models", false, false, low)}
            {tile(l, &l.grass_enabled, icons::GRASS, "Grass", false, false, low)}
            {tile(l, &l.furniture_enabled, icons::BENCH, "Street furniture", false, false, low)}
            {tile(l, &l.street_detail_enabled, icons::SIGNPOST, "Street detail", false, false, low)}
            {tile(l, &l.flood_defences, icons::FLOOD, "Flood defences", true, false, ground.or(low))}
        </TileGrid>
        {sources}
    }
}

fn places(l: &Live) -> View {
    let low = l.in_space.get().then_some(NEAR_GROUND);
    let icon_for = |key: &str| match key {
        "transit" => icons::TRANSPORT,
        "shops" => icons::SHOP,
        _ => icons::PIN,
    };
    let overlays: Vec<View> = LAYER_DEFS
        .iter()
        .zip(l.layers.iter())
        .map(|(def, sig)| tile(l, sig, icon_for(def.key), def.name, false, true, low))
        .collect();
    let open = l.open_tile.get();
    let legend: View = match open.and_then(|o| LAYER_DEFS.iter().find(|d| d.name == o)) {
        Some(def) => {
            let rows: Vec<View> = def
                .categories
                .iter()
                .map(|(label, style)| {
                    let c = format!(
                        "#{:02x}{:02x}{:02x}",
                        (style.color[0] * 255.0) as u8,
                        (style.color[1] * 255.0) as u8,
                        (style.color[2] * 255.0) as u8
                    );
                    let dot = format!("display: inline-block; width: 9px; height: 9px; border-radius: 50%; background: {c}");
                    view! {
                        <p style="margin: 0; display: flex; align-items: center; gap: 8px; font-size: var(--m-font-size-sm)"><span style={dot}></span>{*label}</p>
                    }
                })
                .collect();
            options_box(def.name, view! { {rows} })
        }
        None => View::Empty,
    };
    let plans: View = if open == Some(PLANS_TILE) {
        let off = !l.plans_enabled.get();
        let kinds: Vec<View> = crate::systems::plans::KINDS
            .iter()
            .zip(l.plans_kinds.iter())
            .map(|((_, label, _), sig)| {
                view! { <Chip model={sig.clone()} color={css(theme::p().accent)}>{*label}</Chip> }
            })
            .collect();
        let periods: Vec<Opt<Period>> = Period::ALL
            .iter()
            .map(|p| Opt::new(*p, p.label()))
            .collect();
        options_box(
            "Plans: what is drawn",
            view! {
                <div style="display: flex; flex-wrap: wrap; gap: 6px">
                    {kinds}
                    <Chip model={l.plans_council.clone()} color={css(theme::COUNCIL_LIGHT)}>"Council"</Chip>
                    <Chip model={l.plans_works.clone()} color={css(crate::systems::plans::WORK)}>"Works"</Chip>
                </div>
                <FieldSegmented model={l.plans_period.clone()} options={periods} size=Some(Size::Sm) id="ly-period" disabled={off} />
                {small("Orange: you can still respond. Yellow: works the city plans this year. Gemeente Groningen's official notices, council agendas and maintenance programme; council items are placed by the street names in their titles.")}
            },
        )
    } else {
        View::Empty
    };
    view! {
        <TileGrid>
            {overlays}
            {tile(l, &l.friends_enabled, icons::GROUP, "Friends", false, false, None)}
            {tile(l, &l.community_posts_enabled, icons::SHARE, "Community posts", false, false, None)}
            {tile(l, &l.plans_enabled, icons::FLAG, PLANS_TILE, true, true, low)}
        </TileGrid>
        {legend}
        {plans}
    }
}

fn moving(l: &Live) -> View {
    let low = l.in_space.get().then_some(NEAR_GROUND);
    view! {
        {note("Traffic, people, cyclists and birds are made up; buses and trains are real.")}
        <TileGrid>
            {tile(l, &l.traffic_enabled, icons::CAR, "Traffic", false, false, low)}
            {tile(l, &l.people_enabled, icons::WALK, "People", false, false, low)}
            {tile(l, &l.cyclists_enabled, icons::BIKE, "Cyclists", false, false, low)}
            {tile(l, &l.birds_enabled, icons::CROW, "Birds", false, false, low)}
            {tile(l, &l.vehicles_enabled, icons::BUS, "Live buses & trains", true, false, low)}
        </TileGrid>
    }
}

fn underground_icon(key: &str) -> &'static str {
    match key {
        "geotop" => icons::LANDSCAPE,
        "boreholes" => icons::BOREHOLE,
        "groundwater" => icons::FOUNTAIN,
        "wko" => icons::HEAT_PUMP,
        "pipelines" => icons::VALVE,
        "gasfield" => icons::EARTHQUAKE,
        "contamination" => icons::DANGER,
        _ => icons::CAT_UNDER,
    }
}

fn under(l: &Live) -> View {
    let unseen = (!l.map_enabled.get())
        .then_some(NEEDS_GROUND)
        .or(l.in_space.get().then_some(NEAR_GROUND));
    let layers: Vec<View> = crate::systems::underground::LAYERS
        .iter()
        .zip(l.underground.iter())
        .map(|(def, sig)| {
            tile(
                l,
                sig,
                underground_icon(def.key),
                def.label,
                false,
                false,
                unseen,
            )
        })
        .collect();
    view! {
        <TileGrid>
            {tile(l, &l.sewers, icons::PLUMBING, "Sewers", false, false, unseen)}
            {layers}
        </TileGrid>
    }
}

fn orbit(l: &Live) -> View {
    let from_ground = (!l.in_space.get()).then_some(FROM_SPACE);
    let kinds: View = if l.open_tile.get() == Some("Satellites") {
        let chips: Vec<View> = crate::systems::satellites::KINDS
            .iter()
            .zip(l.satellite_kinds.iter())
            .map(|(def, sig)| {
                let [r, g, b] = def.color.map(|c| (c.min(1.0) * 255.0) as u8);
                let colour = format!("#{r:02x}{g:02x}{b:02x}");
                view! { <Chip model={sig.clone()} color={colour}>{def.label}</Chip> }
            })
            .collect();
        options_box(
            "Satellites: kinds",
            view! {
                <div style="display: flex; flex-wrap: wrap; gap: 6px">{chips}</div>
                {small("Satellites ··· open in place: tap a chip to hide that kind.")}
            },
        )
    } else {
        View::Empty
    };
    view! {
        <TileGrid>
            {tile(l, &l.satellites_enabled, icons::CAT_ORBIT, "Satellites", false, true, from_ground)}
        </TileGrid>
        {kinds}
    }
}

/// `[(value, label)]` as a segmented control's options.
fn opts<T: Clone + PartialEq + 'static>(items: &[(T, &str)]) -> Vec<Opt<T>> {
    items.iter().map(|(v, l)| Opt::new(v.clone(), *l)).collect()
}
