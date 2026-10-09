//! The building card: what the registers say about the building you clicked.
//!
//! Everything on it was already streaming. `building_facts` has held the BAG's
//! year, use, unit count, floor areas, status, monument listing and district since
//! the facades learned to take their colour from an era; `lod22` measures the
//! shell's height, roof area and volume as it cuts the mesh. Until this card there
//! was nowhere to read any of it — the year reached the eye as a wall colour and
//! the rest reached nothing at all.
//!
//! **The roof area and the volume are measured here, not fetched.** 3DBAG
//! publishes both as attributes, so the obvious way to put them on this card is to
//! ship them from atlas's extractor (`pipeline/lod22.rs`) — a wire-format change
//! on both sides of a hand-synced seam, a pipeline re-run per city and a cache-key bump, to carry two numbers that are
//! functions of triangles the client already holds. `lod22::shell_stats` measures
//! them instead, in the pass that was already walking the soup. Nothing on this
//! card needs a new request, a new field on the wire or a re-run of anything.
//!
//! It is a floating `Area` above the dock, deliberately outside the
//! panel chain, and it follows `place_card`'s suppression rules for
//! `place_card`'s reasons — driver mode, the on-screen keyboard and (on a compact
//! viewport) an open panel each own the screen it would float over. Suppression
//! *hides* it: the building stays selected, and its glass stays on the map.

use crate::systems::egui_manager::EguiRef;
use bevy::prelude::*;

use crate::resources::RightPanel;
use crate::systems::building_facts::{use_labels, AddressFact, BuildingFact, BuildingFacts};
use crate::systems::building_select::BuildingSelection;
use crate::systems::data_layers::{open_url, open_url_supported};
use crate::systems::icons;
use crate::systems::lod22::{Lod22Buildings, ShellMeasure};
use crate::systems::theme;

/// Width of the card, points. The same as the place card's, so the two floating
/// surfaces that rise from the same edge are the same object at different times.
const CARD_WIDTH: f32 = 320.0;

/// How many addresses are listed before the rest become a count.
///
/// A block of flats has hundreds of them and this is a card, not a directory. Six
/// is enough to show a terrace's numbers and to make it obvious what the rest are.
const MAX_ADDRESSES: usize = 6;

/// The Kadaster's own viewer for a BAG object, which takes the identificatie
/// verbatim. The whole reason the id is carried as a string with its leading
/// zeros (`docs/notes/building-identity-and-bag.md`) is that it is the key every
/// Dutch register joins on; this is the one link that turns that claim into
/// something a person can press.
fn bag_viewer_url(id: &str) -> String {
    format!("https://bagviewer.kadaster.nl/lvbag/bag-viewer/?objectId={id}")
}

/// The building's page in the atlas floor-plan editor (`/building/{pand}`),
/// with the outline's centre as the hint it finds the building's cell by. The
/// editor shows a building to anybody and lets its owner change it.
pub(crate) fn floor_plan_url(id: &str, footprint: &[(f64, f64)]) -> String {
    let n = footprint.len().max(1) as f64;
    let (lat, lng) = footprint
        .iter()
        .fold((0.0, 0.0), |(a, b), p| (a + p.0, b + p.1));
    format!(
        "{}/building/{id}?lat={:.6}&lng={:.6}",
        crate::systems::place_link::WEB_ORIGIN,
        lat / n,
        lng / n
    )
}

/// The atlas web UI's Overrides tab with this building filled in
/// (`docs/notes/building-models.md`): where an operator replaces its shell with
/// a model, or restores it.
pub(crate) fn override_url(id: &str) -> String {
    format!(
        "{}/admin?override={id}",
        crate::systems::place_link::WEB_ORIGIN
    )
}

/// Draw the card for the selected building.
pub fn render_building_card(
    egui: EguiRef,
    mut selection: ResMut<BuildingSelection>,
    facts: Res<BuildingFacts>,
    lod: Res<Lod22Buildings>,
    right_panel: Res<RightPanel>,
    menu: Res<crate::systems::ui::MainMenu>,
    driver: Res<crate::systems::driver::DriverState>,
    keyboard: Res<crate::systems::onscreen_keyboard::OnScreenKeyboard>,
    mut cutaway: ResMut<crate::systems::cutaway::Cutaway>,
    identity: Res<crate::resources::AuthIdentity>,
) {
    let Some(id) = selection.selected.clone() else {
        return;
    };
    // The dock's suppression rules, and the place card's — each of these owns the
    // bottom of the screen, and an `Area` cannot be put behind a panel. Hidden,
    // not cleared: the building is still the subject, and its glass is still on
    // the map, when the keyboard or the panel goes away.
    if driver.active || menu.open || keyboard.is_open() {
        return;
    }
    if egui.compact() && !right_panel.is(RightPanel::None) {
        return;
    }
    if right_panel.is(RightPanel::Route) {
        return;
    }

    // A building whose cell has streamed out from under the selection. The card
    // goes rather than showing a row of blanks — and so does the selection, since
    // there is nothing left to draw glass around either.
    let Some(fact) = facts.building(&id).cloned() else {
        selection.clear();
        return;
    };
    let shell = lod.shell_measure(&id);
    let addresses: Vec<AddressFact> = facts
        .addresses()
        .filter(|a| a.pand.as_deref() == Some(&*id))
        .cloned()
        .collect();

    let bottom = crate::systems::dock::reserved_bottom() + 8.0;
    let width = CARD_WIDTH.min(egui.viewport_pts().x - 2.0 * theme::OSD_MARGIN);
    let ctx = egui.ctx();
    let mut close = false;
    let mut open: Option<String> = None;
    let mut look: Option<bool> = None;
    // A building has an inside when the game can build one: an entrance to come
    // in by (the generated layout needs it), or a design its owner drew.
    let has_inside = !fact.doors.is_empty() || crate::systems::building_designs::get(&id).is_some();
    let looking = cutaway.looking.as_deref() == Some(&*id);
    egui::Area::new(egui::Id::new("building_card"))
        .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -bottom))
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.set_width(width);
                header(ui, &fact, &addresses, &mut close);
                ui.add_space(4.0);
                egui::ScrollArea::vertical()
                    // A block of flats has more rows than a phone has screen. The
                    // card keeps its own height and scrolls inside it rather than
                    // growing until it swallows the map — the failure
                    // `driver`'s bottom bar hit when it sized to content.
                    .max_height(260.0)
                    .show(ui, |ui| {
                        rows(ui, &fact, shell, &addresses);
                    });
                ui.add_space(6.0);
                // The two ways into the building: look inside it here, or open its
                // floor plan in the editor — where its owner changes it, and
                // anybody else sees what they drew.
                ui.horizontal(|ui| {
                    if has_inside {
                        let text = if looking {
                            "Stop looking inside"
                        } else {
                            "Look inside"
                        };
                        if ui.add(egui::Button::selectable(looking, text)).clicked() {
                            look = Some(!looking);
                        }
                    }
                    if ui
                        .add_enabled(
                            open_url_supported(),
                            egui::Button::new(format!("Floor plan {}", icons::OPEN_EXTERNAL)),
                        )
                        .on_hover_text("Open this building in the floor-plan editor")
                        .clicked()
                    {
                        open = Some(floor_plan_url(&id, &fact.footprint));
                    }
                    // Only for an account the server lets replace buildings; it
                    // says so after sign-in and again when its roles change.
                    if identity.may(cartopolis_core::permissions::Permission::BuildingOverride)
                        && ui
                            .add_enabled(
                                open_url_supported(),
                                egui::Button::new(format!(
                                    "Replace with a model {}",
                                    icons::OPEN_EXTERNAL
                                )),
                            )
                            .on_hover_text(
                                "Open this building in the Overrides tab, to put a model in its place",
                            )
                            .clicked()
                    {
                        open = Some(override_url(&id));
                    }
                });
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    let can_open = open_url_supported();
                    /// Shown on hover where the button is dead, so the greying is
                    /// explained rather than merely observed — `data_layers`'
                    /// POI popup states the same thing the same way.
                    const NO_BROWSER: &str = "Opening links isn't available on this platform";
                    if ui
                        .add_enabled(
                            can_open,
                            egui::Button::new(format!("BAG viewer {}", icons::OPEN_EXTERNAL)),
                        )
                        .on_disabled_hover_text(NO_BROWSER)
                        .clicked()
                    {
                        open = Some(bag_viewer_url(&id));
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        // The id itself, small and selectable — it is the join to
                        // every other Dutch register and to any BIM model of this
                        // building, and somebody will want to copy it.
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(&*id)
                                    .size(theme::SIZE_TINY)
                                    .color(theme::TEXT_DIM),
                            )
                            .truncate(),
                        );
                    });
                });
            });
        });

    if let Some(url) = open {
        open_url(&url);
    }
    match look {
        Some(true) => cutaway.look_inside(id.clone()),
        Some(false) => cutaway.stop_looking(),
        None => {}
    }
    if close {
        selection.clear();
    }
}

/// The header: the street the building is on, or the register's own word for it.
///
/// The close button is laid out **first**, because a truncating label asks for
/// every point available and the other way round it pushes the ✕ off the card —
/// the rule every header row in this tree follows.
fn header(ui: &mut egui::Ui, fact: &BuildingFact, addresses: &[AddressFact], close: &mut bool) {
    let title = street_line(addresses).unwrap_or_else(|| "Building".to_string());
    let subtitle = {
        let uses = use_labels(fact.uses);
        if uses.is_empty() {
            String::new()
        } else {
            uses.join(" · ")
        }
    };
    ui.horizontal(|ui| {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            *close = theme::close_button(ui).clicked();
            let size = egui::vec2(ui.available_width(), ui.spacing().interact_size.y);
            ui.allocate_ui_with_layout(
                size,
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| {
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(&title).strong().size(theme::SIZE_SUB),
                        )
                        .truncate()
                        .selectable(false),
                    );
                },
            );
        });
    });
    if !subtitle.is_empty() {
        ui.add(
            egui::Label::new(
                egui::RichText::new(&subtitle)
                    .size(theme::SIZE_SUB)
                    .color(theme::TEXT_DIM),
            )
            .truncate()
            .selectable(false),
        );
    }
}

/// "Herestraat 12–20" out of the addresses registered against this building.
///
/// The range rather than the list, because a terrace block's header is its
/// frontage and the individual doors are further down the card. `None` for a
/// building with no addressed unit in it at all — a shed, a church hall, a
/// substation — which is a real and common answer.
///
/// **A building can front more than one street**, and a city block routinely
/// fronts four. The street with the most doors in it wins and the range is taken
/// from *that street's* numbers only — the first capture of this card read "Oude
/// Ebbingestraat 4a–211" for a block on the corner of the Grote Markt, where 211
/// is a Grote Markt number and no such address exists. The other frontages are in
/// the address list further down, which is where a second street belongs.
fn street_line(addresses: &[AddressFact]) -> Option<String> {
    let mut tally: Vec<(&str, usize)> = Vec::new();
    for a in addresses {
        let Some(street) = a.street.as_deref() else {
            continue;
        };
        match tally.iter_mut().find(|(s, _)| *s == street) {
            Some((_, n)) => *n += 1,
            // Ties go to whichever the register listed first, which is stable
            // across rebuilds because the cell file's order is.
            None => tally.push((street, 1)),
        }
    }
    let street = tally.iter().max_by_key(|(_, n)| *n).map(|(s, _)| *s)?;
    let mut numbers: Vec<&str> = addresses
        .iter()
        .filter(|a| a.street.as_deref() == Some(street))
        .filter_map(|a| a.number.as_deref())
        .collect();
    numbers.sort_by_key(|n| {
        (
            n.chars()
                .take_while(char::is_ascii_digit)
                .collect::<String>()
                .parse::<u32>()
                .unwrap_or(u32::MAX),
            n.to_string(),
        )
    });
    Some(match (numbers.first(), numbers.last()) {
        (Some(a), Some(b)) if a == b => format!("{street} {a}"),
        (Some(a), Some(b)) => format!("{street} {a}–{b}"),
        _ => street.to_string(),
    })
}

/// Every fact, one row each. A fact the register does not hold gets no row —
/// a card of "unknown"s says less than a short card.
fn rows(
    ui: &mut egui::Ui,
    fact: &BuildingFact,
    shell: Option<ShellMeasure>,
    addresses: &[AddressFact],
) {
    egui::Grid::new("building_facts")
        .num_columns(2)
        .spacing([12.0, 4.0])
        .striped(true)
        .show(ui, |ui| {
            if let Some(year) = fact.year {
                row(ui, "Built", &year.to_string());
            }
            if let Some(status) = fact.status.label() {
                row(ui, "Status", status);
            }
            if fact.units > 0 {
                row(ui, "Units", &fact.units.to_string());
            }
            // The register's floor area is a *range* over the units inside, which
            // is the honest shape: a block with a shop and twelve flats has no one
            // number. Collapsed when the ends agree.
            match (fact.area_m2, fact.area_max_m2) {
                (Some(a), Some(b)) if a != b => row(ui, "Floor area", &format!("{a}–{b} m²")),
                (Some(a), _) => row(ui, "Floor area", &format!("{a} m²")),
                _ => {}
            }
            if let Some(m) = shell {
                row(ui, "Height", &format!("{:.1} m", m.datum_m));
                // The sloping area, so a pitched roof measures more than the
                // footprint under it — the same quantity 3DBAG's flat and pitched
                // columns add up to.
                row(ui, "Roof area", &format!("{:.0} m²", m.roof_area_m2));
                row(ui, "Volume", &format!("{:.0} m³", m.volume_m3));
            }
            if fact.monument.is_some() {
                // The register's own category, in its own language. It is the
                // difference between "a listed building" and "a listed church",
                // which is most of what a person wants from the word.
                let what = fact.monument_category.as_deref().map_or_else(
                    || "national monument".to_string(),
                    |c| format!("national monument · {c}"),
                );
                row(ui, "Listed", &what);
            }
            if let Some(district) = &fact.district {
                row(ui, "District", district);
            }
            if !addresses.is_empty() {
                row(ui, "Addresses", &address_list(addresses));
            }
        });
}

/// The addresses in the building, capped — see [`MAX_ADDRESSES`].
fn address_list(addresses: &[AddressFact]) -> String {
    let mut shown: Vec<String> = addresses
        .iter()
        .take(MAX_ADDRESSES)
        .filter_map(AddressFact::label)
        .collect();
    if addresses.len() > shown.len() {
        shown.push(format!("+{} more", addresses.len() - shown.len()));
    }
    shown.join("\n")
}

/// One name/value row, in the Grid the caller opened.
fn row(ui: &mut egui::Ui, name: &str, value: &str) {
    ui.label(
        egui::RichText::new(name)
            .size(theme::SIZE_SUB)
            .color(theme::TEXT_DIM),
    );
    ui.label(egui::RichText::new(value).size(theme::SIZE_SUB));
    ui.end_row();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn address(street: &str, number: &str) -> AddressFact {
        AddressFact {
            at: (53.0, 6.0),
            pand: Some(Arc::from("0014100010903891")),
            street: Some(Arc::from(street)),
            number: Some(number.to_string()),
            postcode: None,
            uses: 0,
            area_m2: None,
        }
    }

    #[test]
    fn a_terrace_reads_as_a_range() {
        let block = [address("Herestraat", "12"), address("Herestraat", "20")];
        assert_eq!(street_line(&block).as_deref(), Some("Herestraat 12–20"));
    }

    /// House numbers are strings — `12A`, `3-bis` — so sorting them as text puts
    /// 100 before 20 and the header reads "Herestraat 100–99".
    #[test]
    fn house_numbers_sort_numerically() {
        let block = [
            address("Herestraat", "100"),
            address("Herestraat", "20"),
            address("Herestraat", "9"),
        ];
        assert_eq!(street_line(&block).as_deref(), Some("Herestraat 9–100"));
    }

    #[test]
    fn one_address_is_not_a_range() {
        let block = [address("Herestraat", "12")];
        assert_eq!(street_line(&block).as_deref(), Some("Herestraat 12"));
    }

    /// A shed, a church hall, a substation: the register addresses nothing in it.
    #[test]
    fn a_building_with_no_address_has_no_street_line() {
        assert_eq!(street_line(&[]), None);
    }

    /// A corner block fronts two streets, and mixing their numbers invents an
    /// address. The first capture of this card did exactly that.
    #[test]
    fn a_corner_block_ranges_over_one_street_only() {
        let block = [
            address("Oude Ebbingestraat", "4a"),
            address("Oude Ebbingestraat", "16"),
            address("Oude Ebbingestraat", "18"),
            address("Grote Markt", "211"),
        ];
        assert_eq!(
            street_line(&block).as_deref(),
            Some("Oude Ebbingestraat 4a–18")
        );
    }

    #[test]
    fn the_address_list_is_capped() {
        let block: Vec<AddressFact> = (1..=20)
            .map(|n| address("Herestraat", &n.to_string()))
            .collect();
        let listed = address_list(&block);
        assert_eq!(listed.lines().count(), MAX_ADDRESSES + 1);
        assert!(listed.ends_with("+14 more"), "{listed}");
    }

    #[test]
    fn the_viewer_url_carries_the_id_verbatim() {
        // Leading zeros are load-bearing: they name the municipality.
        assert!(bag_viewer_url("0014100010903891").ends_with("=0014100010903891"));
    }

    #[test]
    fn the_override_link_names_the_building_the_web_ui_reads() {
        // `requestedPand` in the web UI's lib/overrides.ts reads `override`.
        assert_eq!(
            override_url("0014100010953806"),
            "https://cartopolis.org/admin?override=0014100010953806"
        );
    }

    #[test]
    fn only_a_granted_account_may_replace_buildings() {
        use cartopolis_core::permissions::Permission;
        let mut who = crate::resources::AuthIdentity::default();
        assert!(!who.may(Permission::BuildingOverride));
        who.permissions = vec!["prim_create".into(), "building_override".into()];
        assert!(who.may(Permission::BuildingOverride));
    }
}
