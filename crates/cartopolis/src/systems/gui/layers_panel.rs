//! The Layers panel: what `render_data_layers_panel` draws over
//! [`DataLayers`], which lives in `data_layers` with the streaming it drives.
//! A panel of its own so the layer state the map reads does not depend on
//! everything the panel shows.

use crate::grid::AnchoredCamera;
use crate::systems::data_layers::*;
use crate::systems::egui_manager::EguiMut;
use crate::systems::icons;
use crate::systems::theme;
use bevy::prelude::*;

/// The Buildings block of the Layers panel: the master checkbox plus one
/// checkbox per source, in precedence order — 3DBAG first (where it covers
/// and is on, it owns the cell), the OSM fallback beneath. The 3DBAG row only
/// appears once a coverage manifest has loaded (`tdbag_available`): before
/// that, or with no host, there is nothing to turn off.
///
/// Split out of [`render_data_layers_panel`] so a unit test can drive it with
/// synthetic clicks. Returns the 3DBAG checkbox's response for that test.
#[cfg(test)]
pub(crate) fn buildings_section(
    ui: &mut egui::Ui,
    dl: &mut DataLayers,
    tdbag_available: bool,
) -> Option<egui::Response> {
    crate::systems::theme::checkbox(
        ui,
        &mut dl.buildings_enabled,
        egui::RichText::new("Buildings"),
    );
    ui.indent("buildings", |ui| building_sources(ui, dl, tdbag_available))
        .inner
}

/// The Buildings tile's options: one checkbox per source, in precedence order —
/// 3DBAG first (where it covers and is on, it owns the cell), the OSM fallback
/// beneath. Returns the 3DBAG checkbox's response, for the click log and the
/// test.
fn building_sources(
    ui: &mut egui::Ui,
    dl: &mut DataLayers,
    tdbag_available: bool,
) -> Option<egui::Response> {
    {
        // Grayed out while the master is off: the sources are gated by it, so
        // an active-looking sub-checkbox would promise something it can't do.
        ui.add_enabled_ui(dl.buildings_enabled, |ui| {
            let tdbag = tdbag_available.then(|| {
                crate::systems::theme::checkbox(
                    ui,
                    &mut dl.tdbag_enabled,
                    egui::RichText::new("3DBAG (NL)").size(theme::SIZE_SUB),
                )
            });
            crate::systems::theme::checkbox(
                ui,
                &mut dl.osm_buildings_enabled,
                egui::RichText::new("OSM 3D buildings and windows").size(theme::SIZE_SUB),
            );
            tdbag
        })
        .inner
    }
}

/// What the Layers panel needs to know about the view: whether the camera is in
/// space, which decides which tiles can be seen from here.
#[derive(bevy::ecs::system::SystemParam)]
pub struct LayerView<'w, 's> {
    globe: Res<'w, crate::systems::globe::GlobeState>,
    camera: AnchoredCamera<'w, 's>,
}

impl LayerView<'_, '_> {
    fn in_space(&self) -> bool {
        self.camera
            .get()
            .is_some_and(|(c, t)| self.globe.in_space(c, t))
    }
}

/// Why a tile that needs the ground cannot show while the map is off.
const NEEDS_GROUND: &str = "Needs the ground: pick a map under Map";
/// Why a ground-level layer cannot show from orbit.
const NEAR_GROUND: &str = "Shown near the ground: zoom in";

/// How many layers are on in each category, in `LayerCategory::ALL` order.
fn category_counts(dl: &DataLayers) -> [usize; 6] {
    let n = |xs: &[bool]| xs.iter().filter(|x| **x).count();
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

/// The icon for an underground layer, by its key.
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

/// The tiles of the open category, and the options of the tile whose ⋯ is open.
fn category_body(ui: &mut egui::Ui, dl: &mut DataLayers, in_space: bool, tdbag_available: bool) {
    use crate::systems::layer_tiles::{basemap_choice, tile_grid, Basemap, LayerCategory, Tile};

    let ground = (!dl.map_enabled).then_some(NEEDS_GROUND);
    let low = in_space.then_some(NEAR_GROUND);
    let tile = |icon, label, nl, more, unseen| Tile {
        icon,
        label,
        nl,
        more,
        unseen,
    };
    let open = dl.open_tile;
    let open_index = |labels: &[&str]| open.and_then(|o| labels.iter().position(|l| *l == o));

    match dl.category {
        LayerCategory::Map => {
            let current = if !dl.map_enabled {
                Basemap::None
            } else if dl.aerial_enabled {
                Basemap::Aerial
            } else {
                Basemap::Drawn
            };
            if let Some(pick) = basemap_choice(ui, current) {
                match pick {
                    Basemap::Drawn => {
                        dl.map_enabled = true;
                        dl.aerial_enabled = false;
                    }
                    Basemap::Aerial => {
                        dl.map_enabled = true;
                        dl.aerial_enabled = true;
                    }
                    Basemap::None => dl.map_enabled = false,
                }
            }
            ui.add_space(8.0);
            ui.label(
                egui::RichText::new("On the map")
                    .size(theme::SIZE_SUB)
                    .color(theme::p().text_dim),
            );
            ui.add_space(4.0);
            tile_grid(
                ui,
                &mut [
                    (
                        tile(icons::LABEL, "Street names", false, false, ground.or(low)),
                        &mut dl.street_labels,
                    ),
                    (
                        tile(icons::LABEL, "Place names", false, false, None),
                        &mut dl.place_names,
                    ),
                    (
                        tile(icons::ROAD, "Road markings", false, false, low),
                        &mut dl.roads_textured,
                    ),
                    (
                        tile(icons::WATER, "Moving water", false, false, low),
                        &mut dl.water_shading,
                    ),
                ],
                None,
            );
            // The globe, seen from above (`docs/mockups/macro-view/`, v3).
            ui.add_space(8.0);
            ui.label(
                egui::RichText::new("From above")
                    .size(theme::SIZE_SUB)
                    .color(theme::p().text_dim),
            );
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new("Clouds")
                    .size(theme::SIZE_TINY)
                    .color(theme::p().text_dim),
            );
            {
                use crate::resources::GlobeCloudMode;
                theme::segmented(
                    ui,
                    &mut dl.globe_clouds,
                    &[
                        (GlobeCloudMode::Off, "Off"),
                        (GlobeCloudMode::Light, "Light"),
                        (GlobeCloudMode::Full, "Full"),
                    ],
                );
            }
            ui.add_space(4.0);
            tile_grid(
                ui,
                &mut [
                    (
                        tile(icons::CAT_ORBIT, "Night side", false, false, None),
                        &mut dl.night_side,
                    ),
                    (
                        tile(icons::STAR, "City lights", false, false, None),
                        &mut dl.city_lights,
                    ),
                    (
                        tile(icons::TUNE, "Hex grid", false, false, None),
                        &mut dl.hex_grid,
                    ),
                ],
                None,
            );
            // Figures moved to the lens (`systems::scenario`, its disc under
            // this panel's): something read, one at a time, not a switch. One
            // line here says which is open, for whoever looks for it here.
            if let Some(open) = dl.scenario.as_deref().and_then(|k| {
                crate::systems::scenario::offered()
                    .iter()
                    .find(|o| o.key == k)
            }) {
                ui.add_space(8.0);
                ui.label(
                    egui::RichText::new(format!("{}  Lens open: {}", icons::POLL, open.label))
                        .size(theme::SIZE_SUB)
                        .color(theme::p().text),
                );
                ui.label(
                    egui::RichText::new("Change it with the lens button, under this panel's")
                        .size(theme::SIZE_TINY)
                        .color(theme::p().text_dim),
                );
            }
        }
        LayerCategory::City => {
            let labels = ["Buildings"];
            let resp = tile_grid(
                ui,
                &mut [
                    (
                        tile(icons::BUILDINGS, "Buildings", false, true, low),
                        &mut dl.buildings_enabled,
                    ),
                    (
                        tile(icons::LEISURE, "Trees", false, false, low),
                        &mut dl.trees_enabled,
                    ),
                    (
                        tile(icons::LEISURE, "Tree models", false, false, low),
                        &mut dl.tree_models_enabled,
                    ),
                    (
                        tile(icons::GRASS, "Grass", false, false, low),
                        &mut dl.grass_enabled,
                    ),
                    (
                        tile(icons::BENCH, "Street furniture", false, false, low),
                        &mut dl.furniture_enabled,
                    ),
                    (
                        tile(icons::SIGNPOST, "Street detail", false, false, low),
                        &mut dl.street_detail_enabled,
                    ),
                    (
                        tile(icons::FLOOD, "Flood defences", true, false, ground.or(low)),
                        &mut dl.flood_defences,
                    ),
                ],
                open_index(&labels),
            );
            toggle_open(dl, resp.more.map(|_| "Buildings"));
            if dl.open_tile == Some("Buildings") {
                options_box(ui, "Buildings: sources", |ui| {
                    // One line per click, so a live session can tell "the click
                    // never reached the widget" from "it toggled and something
                    // reverted it" without a debugger (#12 reported it stuck).
                    if building_sources(ui, dl, tdbag_available).is_some_and(|r| r.clicked()) {
                        tracing::info!(tdbag = dl.tdbag_enabled, "3DBAG checkbox clicked");
                    }
                });
            }
        }
        LayerCategory::Places => {
            let mut names: Vec<&'static str> = LAYER_DEFS.iter().map(|d| d.name).collect();
            names.extend(["Friends", "Community posts", PLANS_TILE]);
            let icons_for = |key: &str| match key {
                "transit" => icons::TRANSPORT,
                "shops" => icons::SHOP,
                _ => icons::PIN,
            };
            let (overlays, friends, posts, plans) = (
                &mut dl.layers,
                &mut dl.friends_enabled,
                &mut dl.community_posts_enabled,
                &mut dl.plans_enabled,
            );
            let mut tiles: Vec<(Tile<'_>, &mut bool)> = LAYER_DEFS
                .iter()
                .zip(overlays.iter_mut())
                .map(|(def, layer)| {
                    (
                        tile(icons_for(def.key), def.name, false, true, low),
                        &mut layer.enabled,
                    )
                })
                .collect();
            tiles.push((tile(icons::GROUP, "Friends", false, false, None), friends));
            tiles.push((
                tile(icons::SHARE, "Community posts", false, false, None),
                posts,
            ));
            tiles.push((tile(icons::FLAG, PLANS_TILE, true, true, low), plans));
            let resp = tile_grid(ui, &mut tiles, open_index(&names));
            drop(tiles);
            toggle_open(dl, resp.more.and_then(|i| names.get(i).copied()));
            if dl.open_tile == Some(PLANS_TILE) {
                plans_options(ui, dl);
            }
            if let Some(def) = dl
                .open_tile
                .and_then(|o| LAYER_DEFS.iter().find(|d| d.name == o))
            {
                options_box(ui, def.name, |ui| {
                    for (label, style) in def.categories {
                        let c = egui::Color32::from_rgb(
                            (style.color[0] * 255.0) as u8,
                            (style.color[1] * 255.0) as u8,
                            (style.color[2] * 255.0) as u8,
                        );
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new(icons::DOT)
                                    .size(theme::SIZE_CHIP)
                                    .color(c),
                            );
                            ui.label(egui::RichText::new(*label).size(theme::SIZE_SUB));
                        });
                    }
                });
            }
        }
        LayerCategory::Moving => {
            ui.label(
                egui::RichText::new(
                    "Traffic, people, cyclists and birds are made up; buses and trains are real.",
                )
                .size(theme::SIZE_SUB)
                .color(theme::p().text_dim),
            );
            ui.add_space(4.0);
            tile_grid(
                ui,
                &mut [
                    (
                        tile(icons::CAR, "Traffic", false, false, low),
                        &mut dl.traffic_enabled,
                    ),
                    (
                        tile(icons::WALK, "People", false, false, low),
                        &mut dl.people_enabled,
                    ),
                    (
                        tile(icons::BIKE, "Cyclists", false, false, low),
                        &mut dl.cyclists_enabled,
                    ),
                    (
                        tile(icons::CROW, "Birds", false, false, low),
                        &mut dl.birds_enabled,
                    ),
                    (
                        tile(icons::BUS, "Live buses & trains", true, false, low),
                        &mut dl.vehicles_enabled,
                    ),
                ],
                None,
            );
        }
        LayerCategory::Under => {
            let unseen = ground.or(low);
            let (sewers, underground) = (&mut dl.sewers, &mut dl.underground);
            let mut tiles: Vec<(Tile<'_>, &mut bool)> = vec![(
                tile(icons::PLUMBING, "Sewers", false, false, unseen),
                sewers,
            )];
            for (def, on) in crate::systems::underground::LAYERS
                .iter()
                .zip(underground.iter_mut())
            {
                tiles.push((
                    tile(underground_icon(def.key), def.label, false, false, unseen),
                    on,
                ));
            }
            tile_grid(ui, &mut tiles, None);
        }
        LayerCategory::Orbit => {
            let from_ground = (!in_space).then_some("Seen from space: zoom out to see it");
            let labels = ["Satellites"];
            let resp = tile_grid(
                ui,
                &mut [(
                    tile(icons::CAT_ORBIT, "Satellites", false, true, from_ground),
                    &mut dl.satellites_enabled,
                )],
                open_index(&labels),
            );
            toggle_open(dl, resp.more.map(|_| "Satellites"));
            if dl.open_tile == Some("Satellites") {
                let enabled = dl.satellites_enabled;
                options_box(ui, "Satellites: kinds", |ui| {
                    ui.add_enabled_ui(enabled, |ui| {
                        ui.horizontal_wrapped(|ui| {
                            for (def, on) in crate::systems::satellites::KINDS
                                .iter()
                                .zip(dl.satellite_kinds.iter_mut())
                            {
                                let [r, g, b] = def.color.map(|c| (c.min(1.0) * 255.0) as u8);
                                crate::systems::layer_tiles::swatch_chip(
                                    ui,
                                    on,
                                    egui::Color32::from_rgb(r, g, b),
                                    def.label,
                                );
                            }
                        });
                    });
                });
            }
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new("Tap a satellite to follow it or see its path.")
                    .size(theme::SIZE_SUB)
                    .color(theme::p().text_dim),
            );
        }
    }
}

/// The Plans tile's label, which is also how its ⋯ is told open.
const PLANS_TILE: &str = "Plans";

/// Plans ⋯: which kinds of notice, whether the council, and how far back
/// (`systems::plans`; `docs/mockups/plans-and-decisions/v2-layer.png`).
fn plans_options(ui: &mut egui::Ui, dl: &mut DataLayers) {
    use crate::systems::plans::{Period, KINDS};
    let enabled = dl.plans_enabled;
    options_box(ui, "Plans: what is drawn", |ui| {
        ui.add_enabled_ui(enabled, |ui| {
            ui.horizontal_wrapped(|ui| {
                for ((_, label, _), on) in KINDS.iter().zip(dl.plans_kinds.iter_mut()) {
                    crate::systems::layer_tiles::swatch_chip(ui, on, theme::p().accent, label);
                }
                crate::systems::layer_tiles::swatch_chip(
                    ui,
                    &mut dl.plans_council,
                    theme::COUNCIL_LIGHT,
                    "Council",
                );
                crate::systems::layer_tiles::swatch_chip(
                    ui,
                    &mut dl.plans_works,
                    crate::systems::plans::WORK,
                    "Works",
                );
            });
            ui.add_space(6.0);
            let options: Vec<(Period, &str)> =
                Period::ALL.iter().map(|p| (*p, p.label())).collect();
            theme::segmented(ui, &mut dl.plans_period, &options);
        });
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new(
                "Orange: you can still respond. Yellow: works the city plans this \
                 year. Gemeente Groningen's official notices, council agendas and \
                 maintenance programme; council items are placed by the street \
                 names in their titles.",
            )
            .size(theme::SIZE_SUB)
            .color(theme::p().text_dim),
        );
    });
}

/// Open the options of the tile whose ⋯ was tapped, or close them if they were
/// already open.
fn toggle_open(dl: &mut DataLayers, tapped: Option<&'static str>) {
    if let Some(label) = tapped {
        dl.open_tile = if dl.open_tile == Some(label) {
            None
        } else {
            Some(label)
        };
    }
}

/// The raised box a tile's options open in, under the grid.
fn options_box(ui: &mut egui::Ui, title: &str, body: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::new()
        .fill(theme::p().surface_raised)
        .stroke(egui::Stroke::new(1.0, theme::p().overlay_line))
        .corner_radius(12)
        .inner_margin(egui::Margin::same(10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(egui::RichText::new(title).size(theme::SIZE_SUB).strong());
            ui.add_space(4.0);
            body(ui);
        });
}

/// The Layers side panel (menu bar ▸ Layers): six categories by where a layer
/// is — Map, City, Places, Moving, Under, Orbit — each a grid of tiles that are
/// their own switches (`layer_tiles`, `docs/mockups/layers-and-ring/`), plus the
/// required attributions.
pub fn render_data_layers_panel(
    mut egui: EguiMut,
    mut dl: ResMut<DataLayers>,
    mut right_panel: ResMut<crate::resources::RightPanel>,
    layer_view: LayerView,
) {
    let open_before = right_panel.is(crate::resources::RightPanel::Layers);
    let mut open = open_before;
    let mut reset_layers = false;
    let mut close = false;
    let mut about = false;
    let compact = egui.compact();
    let viewport_w = egui.viewport_pts().x;
    crate::systems::egui_manager::responsive_right_panel(
        "layers_panel",
        compact,
        viewport_w,
        300.0,
    )
    .show_collapsible(egui.ui_mut(), &mut open, |ui| {
        let panel = crate::resources::RightPanel::Layers;
        // **The one dead end in the client.** These ~13 toggles persist to
        // `user.json`, so a map switched off by mistake — or by curiosity —
        // stays off across restarts, and the only way back was to remember
        // which of them had been on. Reset is a header action rather than a
        // footer button because the footer is inside the scroll area, i.e. past
        // the ~900 pt of rows that a blanked map gives you no reason to scroll
        // through.
        close = crate::systems::theme::panel_header_with(ui, panel.icon(), panel.label(), |ui| {
            if crate::systems::theme::header_action(ui, "Reset")
                .on_hover_text("Switch every layer back to how it ships")
                .clicked()
            {
                reset_layers = true;
            }
        });
        // The body is ~35 rows and ~900 pt tall against a 609 pt phone viewport
        // (411 dp at `ui_scale` 1.5), and a `Panel` does not scroll on its own —
        // it simply clips. Everything from "Height" down, including the only
        // switch that brings the street names back, was unreachable. The header
        // stays outside so the close button cannot scroll away with the content.
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let in_space = layer_view.in_space();
                let tdbag_available = crate::systems::osm_buildings::tdbag_attribution().is_some();
                let dl = &mut *dl;
                let counts = category_counts(dl);
                crate::systems::layer_tiles::category_strip(ui, &mut dl.category, counts);
                ui.add_space(2.0);
                ui.label(
                    egui::RichText::new(dl.category.subtitle())
                        .size(theme::SIZE_SUB)
                        .color(theme::p().text_dim),
                );
                ui.add_space(6.0);
                category_body(ui, dl, in_space, tdbag_available);
                ui.add_space(8.0);
                ui.separator();
                ui.add_space(4.0);
                // In the flow, not `bottom_up`: a bottom-anchored layout inside a scroll
                // area asks for the scroll viewport's height and pins the credits to the
                // wrong end of an unbounded canvas. The short form stays here because it
                // is the layer list's own source note; the full notices — and the ones
                // this panel never mentioned at all (VersaTiles, AHN, Quaternius, the
                // font) — are in About, which is where a licence looks for them.
                ui.add_space(6.0);
                if let Some(credit) = crate::systems::osm_buildings::tdbag_attribution() {
                    ui.small(credit);
                }
                if let Some(credit) = crate::systems::building_facts::bag_attribution() {
                    ui.small(credit);
                }
                if let Some(credit) = crate::systems::surveyed::bgt_attribution() {
                    ui.small(credit);
                }
                // The city registers' line. The only credit here that is a
                // **licence condition** and not a courtesy: the national registers
                // around it are CC0, a city's are CC BY, and the layer may not
                // ship without it.
                if let Some(credit) = crate::systems::municipal::attribution() {
                    ui.small(credit);
                }
                // Its own line: the crop register is the RVO's, not the
                // Kadaster's, and it carries its own (public domain) mark.
                if let Some(credit) = crate::systems::crops::brp_attribution() {
                    ui.small(credit);
                }
                // Its own line rather than folded into the BGT's: the defences
                // come from the water boards' registers, not the Kadaster's.
                if let Some(credit) = crate::systems::dikes::dike_attribution() {
                    ui.small(credit);
                }
                // The municipalities' sewer registers, through RIONED.
                if let Some(credit) = crate::systems::sewers::sewer_attribution() {
                    ui.small(credit);
                }
                for credit in crate::systems::underground::attributions() {
                    ui.small(credit);
                }
                // Unlike the two above, this is not one national dataset with one
                // licence: a building model belongs to whoever surveyed it, so the
                // line is whatever the importer was told to record.
                if let Some(credit) = crate::systems::lod3::lod3_attribution() {
                    ui.small(credit);
                }
                // The roof colours are measured from PDOK's aerial photograph,
                // which is CC BY: its own line, since no photo is shown.
                if let Some(credit) = crate::systems::roof_colours::roofs_attribution() {
                    ui.small(credit);
                }
                // The wall colours come from a municipality's own city model:
                // its line, as the manifest records it.
                if let Some(credit) = crate::systems::facade_colours::facades_attribution() {
                    ui.small(credit);
                }
                // A correction measured from Mapillary's street photographs is
                // CC BY-SA, a licence condition like the lines above; shown
                // wherever this host serves corrections at all.
                if crate::systems::facade_colours::edits_present() {
                    ui.small(
                        "Facade corrections from street photos © Mapillary contributors \
                         (CC BY-SA 4.0) and players",
                    );
                }
                ui.small(
                    "Data © OpenStreetMap contributors (ODbL), via vector tiles + Overpass API",
                );
                // CC BY, so a licence condition like the municipal lines above.
                // Static: every map drawn off the default host has it underneath.
                ui.small("Land cover © ESA WorldCover 2021 (CC BY 4.0), via VersaTiles");
                ui.small("Live departures & vehicles: OVapi (v0 / gtfs.ovapi.nl)");
                if let Some(credit) = crate::systems::satellites::attribution(&dl) {
                    ui.small(credit);
                }
                // Not gated on a coverage manifest the way the four above are:
                // this register is fetched by the client itself and has no
                // manifest to ask, so the line is static like the two around it.
                // CC0 compels no attribution — it is here because
                // `LICENSE-THIRD-PARTY.md` lists the source and this panel is the
                // layer list's own source note.
                ui.small("Level crossings: ProRail Spoorwegen, served by PDOK (CC0)");
                if theme::action_button(ui, "Full credits and licences…").clicked() {
                    about = true;
                }
            });
    });
    if reset_layers {
        // Field by field rather than `*dl = DataLayers::default()`: this resource
        // also holds the live cell streams, the marker index and the open POI
        // card, and replacing it wholesale would tear down streaming work that
        // has nothing to do with which layers are ticked. The toggles are the
        // only thing a reset is about.
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
        // The three that a "reset" used to leave standing. Traffic and people are
        // new here; `street_detail` was simply missed, and a reset that puts back
        // every checkbox but one is worse than none — the one it skips reads as a
        // toggle that has stuck.
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
    if about {
        *right_panel = crate::resources::RightPanel::About;
    } else if close || (open_before && !open) {
        *right_panel = crate::resources::RightPanel::None;
    }
}

// --- Worker-thread fetch + mesh build ---
