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
//! It follows `place_card`'s suppression rules for `place_card`'s reasons —
//! driver mode, the on-screen keyboard and (on a compact viewport) an open
//! panel each own the screen it would float over. Suppression *hides* it: the
//! building stays selected, and its glass stays on the map.
//!
//! The component library draws it (`lib_building_card`);
//! [`drive_building_card`] works out what the card shows ([`CardContent`])
//! and does the verb the library's card asks for, through [`run`].

use std::sync::Arc;

use bevy::prelude::*;

use crate::resources::RightPanel;
use crate::systems::building_facts::{use_labels, AddressFact, BuildingFact, BuildingFacts};
use crate::systems::building_select::BuildingSelection;
use crate::systems::data_layers::{open_url, open_url_supported};
use crate::systems::lod22::{Lod22Buildings, ShellMeasure};
use crate::systems::ui_input::UiMetrics;

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
pub(crate) fn bag_viewer_url(id: &str) -> String {
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

/// A verb on the building card.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CardAction {
    /// Send the building to a chat or community (`bridges`, the app only).
    Share,
    /// Look inside it, or stop looking.
    Inside,
    /// Its floor plan in the atlas editor.
    Plan,
    /// Its Overrides tab, to put a model in its place.
    Replace,
    /// The facade measured again from street photos (`facade_check`).
    Wrong,
    /// The facade measured from a photo the user takes.
    Photo,
    /// Stop taking that photo.
    CancelPhoto,
    /// The Kadaster's own record of it.
    Record,
}

/// What the card shows of one building, worked out here and drawn by
/// `lib_building_card`.
#[derive(Clone, PartialEq, Debug)]
pub struct CardContent {
    pub id: Arc<str>,
    /// The street line, or "Building".
    pub title: String,
    /// The register's uses, "" when it lists none.
    pub subtitle: String,
    /// Every fact the register holds, one name/value row each.
    pub rows: Vec<(&'static str, String)>,
    /// The verbs offered as the card's buttons, two to a row, in order.
    pub actions: Vec<CardAction>,
    /// Looking inside this building now.
    pub looking: bool,
    /// The facade is being measured (its buttons wait).
    pub busy: bool,
    /// The camera is out for the facade photo (Cancel is offered).
    pub capturing: bool,
    /// This platform can open a link.
    pub can_open: bool,
    /// What the facade measurement says, when it says something.
    pub note: Option<String>,
}

impl CardContent {
    #[must_use]
    pub fn of(
        id: &Arc<str>,
        fact: &BuildingFact,
        shell: Option<ShellMeasure>,
        addresses: &[AddressFact],
        cutaway: &crate::systems::cutaway::Cutaway,
        identity: &crate::resources::AuthIdentity,
        facade: &crate::systems::facade_check::FacadeCheck,
    ) -> Self {
        let (title, subtitle) = titles(fact, addresses);
        // A building has an inside when the game can build one: an entrance to
        // come in by (the generated layout needs it), or a design its owner drew.
        let has_inside =
            !fact.doors.is_empty() || crate::systems::building_designs::get(id).is_some();
        // One row of the place card's kind of button (N2): Share, Look inside,
        // Floor plan, and Replace for an account allowed to — two to a row,
        // equal widths, so nothing is bare text beside an outlined button and
        // nothing runs off a phone's card. Share hands the building to a chat,
        // which only the app has (`comms`): on the web the page around the
        // World tab does.
        let mut actions = if cfg!(feature = "comms") {
            vec![CardAction::Share]
        } else {
            Vec::new()
        };
        if has_inside {
            actions.push(CardAction::Inside);
        }
        actions.push(CardAction::Plan);
        if identity.may(cartopolis_core::permissions::Permission::BuildingOverride) {
            actions.push(CardAction::Replace);
        }
        // The facade measured again (`facade_check`): anybody signed in, since
        // atlas files who asked.
        if identity.username.is_some() && middle(fact).is_some() {
            actions.push(CardAction::Wrong);
            if crate::platform::photo::is_available() {
                actions.push(CardAction::Photo);
            }
        }
        CardContent {
            id: id.clone(),
            title,
            subtitle,
            rows: fact_rows(fact, shell, addresses),
            actions,
            looking: cutaway.looking.as_deref() == Some(&**id),
            busy: facade.busy(),
            capturing: facade.capturing(),
            can_open: open_url_supported(),
            note: facade.note(id).map(str::to_owned),
        }
    }
}

/// What [`drive_building_card`] and the library's card pass each other.
#[derive(Default, Clone, PartialEq, Debug)]
pub struct CardBridge {
    /// The building as shown; `None` while nothing is selected, or while
    /// something else owns the bottom of the screen.
    pub content: Option<CardContent>,
    /// A verb the library's card asked for, done by [`drive_building_card`].
    pub action: Option<CardAction>,
}

/// The building's middle: where Share points, and how atlas finds its cell.
fn middle(fact: &BuildingFact) -> Option<(f64, f64)> {
    (!fact.footprint.is_empty()).then(|| {
        let n = fact.footprint.len() as f64;
        let (lat, lng) = fact
            .footprint
            .iter()
            .fold((0.0, 0.0), |(a, b), p| (a + p.0, b + p.1));
        (lat / n, lng / n)
    })
}

/// The addresses registered against the building.
fn addresses_of(facts: &BuildingFacts, id: &str) -> Vec<AddressFact> {
    facts
        .addresses()
        .filter(|a| a.pand.as_deref() == Some(id))
        .cloned()
        .collect()
}

/// Do a verb on the building `id`. Share is the Community side's
/// ([`share_building`]), not done here.
pub fn run(
    action: CardAction,
    id: &Arc<str>,
    fact: &BuildingFact,
    cutaway: &mut crate::systems::cutaway::Cutaway,
    facade: &mut crate::systems::facade_check::FacadeCheck,
) {
    let link = |url: String| {
        if open_url_supported() {
            open_url(&url);
        }
    };
    match action {
        CardAction::Inside => {
            if cutaway.looking.as_deref() == Some(&**id) {
                cutaway.stop_looking();
            } else {
                cutaway.look_inside(id.clone());
            }
        }
        CardAction::Plan => link(floor_plan_url(id, &fact.footprint)),
        CardAction::Replace => link(override_url(id)),
        CardAction::Record => link(bag_viewer_url(id)),
        CardAction::Wrong => {
            if let Some(near) = middle(fact) {
                facade.recheck(id.clone(), near);
            }
        }
        CardAction::Photo => {
            if let Some(near) = middle(fact) {
                facade.photo(id.clone(), near);
            }
        }
        CardAction::CancelPhoto => facade.cancel(),
        CardAction::Share => {}
    }
}

/// Share: the building to the Share sheet (`bridges`), named by its first
/// address.
#[cfg(feature = "comms")]
pub fn share_building(
    fact: &BuildingFact,
    addresses: &[AddressFact],
    bridges: &mut crate::systems::bridges::Bridges,
) {
    let Some((lat, lng)) = middle(fact) else {
        return;
    };
    let name = addresses
        .iter()
        .find_map(AddressFact::label)
        .unwrap_or_else(|| "this building".to_string());
    bridges.share = Some(crate::systems::bridges::SharePlace {
        name,
        lat,
        lng,
        view: None,
    });
}

/// The selected building's card state, for the library's card: publishes
/// [`CardBridge::content`], does the verb asked for.
#[allow(clippy::too_many_arguments)]
pub fn drive_building_card(
    metrics: Res<UiMetrics>,
    mut selection: ResMut<BuildingSelection>,
    facts: Res<BuildingFacts>,
    lod: Res<Lod22Buildings>,
    right_panel: Res<RightPanel>,
    menu: Res<crate::systems::ui::MainMenu>,
    driver: Res<crate::systems::driver::DriverState>,
    keyboard: Res<crate::systems::onscreen_keyboard::OnScreenKeyboard>,
    mut cutaway: ResMut<crate::systems::cutaway::Cutaway>,
    identity: Res<crate::resources::AuthIdentity>,
    mut last_shown: Local<Option<Arc<str>>>,
    #[cfg(feature = "comms")] mut bridges: ResMut<crate::systems::bridges::Bridges>,
    mut facade: ResMut<crate::systems::facade_check::FacadeCheck>,
) {
    // The dock's suppression rules, and the place card's — each of these owns the
    // bottom of the screen. Hidden,
    // not cleared: the building is still the subject, and its glass is still on
    // the map, when the keyboard or the panel goes away.
    // The Share sheet stands where the card does, and is about it.
    #[cfg(feature = "comms")]
    let sharing = bridges.share.is_some();
    #[cfg(not(feature = "comms"))]
    let sharing = false;
    let hidden = driver.active
        || menu.open
        || keyboard.is_open()
        || (metrics.compact() && !right_panel.is(RightPanel::None))
        || right_panel.is(RightPanel::Route)
        || sharing;
    // A new building starts at the peek, however far the last one was pulled.
    if let Some(id) = selection.selected.clone() {
        if last_shown.as_deref() != Some(&*id) {
            selection.card_rest = Default::default();
            *last_shown = Some(id);
        }
    }

    // Hand the library's card what it shows, and do the verb it asked for.
    // Read before taking, so a quiet frame leaves the resource unchanged.
    let action = if selection.card_bridge.action.is_some() {
        selection.card_bridge.action.take()
    } else {
        None
    };
    let selected = selection.selected.clone();
    let fact = selected
        .as_deref()
        .and_then(|id| facts.building(id).cloned());
    // A building whose cell has streamed out from under the selection. The
    // card goes rather than showing a row of blanks — and so does the
    // selection, since there is nothing left to draw glass around either.
    if !hidden && selected.is_some() && fact.is_none() {
        selection.clear();
    }
    let shown = selected.zip(fact).map(|(id, fact)| {
        let addresses = addresses_of(&facts, &id);
        (id, fact, addresses)
    });
    let content = if hidden {
        None
    } else {
        shown.as_ref().map(|(id, fact, addresses)| {
            CardContent::of(
                id,
                fact,
                lod.shell_measure(id),
                addresses,
                &cutaway,
                &identity,
                &facade,
            )
        })
    };
    if selection.card_bridge.content != content {
        selection.card_bridge.content = content;
    }
    if let (Some(a), Some((id, fact, addresses))) = (action, shown.as_ref()) {
        run(a, id, fact, &mut cutaway, &mut facade);
        #[cfg(feature = "comms")]
        if a == CardAction::Share {
            share_building(fact, addresses, &mut bridges);
        }
        #[cfg(not(feature = "comms"))]
        let _ = addresses;
    }
}

/// The card's title and the line under it: the street the building is on
/// (or the register's own word for it), and the uses the register lists.
pub(crate) fn titles(fact: &BuildingFact, addresses: &[AddressFact]) -> (String, String) {
    let title = street_line(addresses).unwrap_or_else(|| "Building".to_string());
    let uses = use_labels(fact.uses);
    let subtitle = if uses.is_empty() {
        String::new()
    } else {
        uses.join(" · ")
    };
    (title, subtitle)
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
pub(crate) fn street_line(addresses: &[AddressFact]) -> Option<String> {
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

/// Every fact, one name/value row each. A fact the register does not hold
/// gets no row — a card of "unknown"s says less than a short card.
pub(crate) fn fact_rows(
    fact: &BuildingFact,
    shell: Option<ShellMeasure>,
    addresses: &[AddressFact],
) -> Vec<(&'static str, String)> {
    let mut out: Vec<(&'static str, String)> = Vec::new();
    if let Some(year) = fact.year {
        out.push(("Built", year.to_string()));
    }
    if let Some(status) = fact.status.label() {
        out.push(("Status", status.to_string()));
    }
    if fact.units > 0 {
        out.push(("Units", fact.units.to_string()));
    }
    // The register's floor area is a *range* over the units inside, which
    // is the honest shape: a block with a shop and twelve flats has no one
    // number. Collapsed when the ends agree.
    match (fact.area_m2, fact.area_max_m2) {
        (Some(a), Some(b)) if a != b => out.push(("Floor area", format!("{a}–{b} m²"))),
        (Some(a), _) => out.push(("Floor area", format!("{a} m²"))),
        _ => {}
    }
    if let Some(m) = shell {
        out.push(("Height", format!("{:.1} m", m.datum_m)));
        // The sloping area, so a pitched roof measures more than the
        // footprint under it — the same quantity 3DBAG's flat and pitched
        // columns add up to.
        out.push(("Roof area", format!("{:.0} m²", m.roof_area_m2)));
        out.push(("Volume", format!("{:.0} m³", m.volume_m3)));
    }
    if fact.monument.is_some() {
        // The register's own category, in its own language. It is the
        // difference between "a listed building" and "a listed church",
        // which is most of what a person wants from the word.
        let what = fact.monument_category.as_deref().map_or_else(
            || "national monument".to_string(),
            |c| format!("national monument · {c}"),
        );
        out.push(("Listed", what));
    }
    if let Some(district) = &fact.district {
        out.push(("District", district.to_string()));
    }
    if !addresses.is_empty() {
        out.push(("Addresses", address_list(addresses)));
    }
    out
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
