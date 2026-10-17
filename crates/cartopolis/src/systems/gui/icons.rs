//! The app's icon set: **Material Symbols Rounded**, subset and bundled.
//!
//! # Why a font of our own
//!
//! Every icon in this client used to be a Unicode symbol picked out of whatever
//! egui's bundled font stack (`Ubuntu-Light` + `NotoEmoji` + `emoji-icon-font`)
//! happened to cover, chosen through a *preference ladder* — `["🔍", "⌕", "S"]` —
//! that fell back to a letter when the font could not draw the glyph. That
//! machinery worked, in the sense that nothing drew an empty box; what it could
//! not do is look like an icon set. The ladder's survivors were a dharma wheel
//! for a station, crossed swords for a restaurant, a rook for a hotel and an
//! atom for a school, at three different optical weights, half of them absent on
//! any given platform so the app's chrome changed shape between machines.
//!
//! An icon *font we ship* removes the whole class of problem: the glyph is
//! present by construction, it is drawn on one grid at one weight, and it means
//! what it depicts. `theme::first_drawable` went with the ladders — the fallback
//! it existed to provide has nothing left to guard.
//!
//! # The subset, and how to add an icon
//!
//! Upstream's variable font is 15 MB, which is not something to `include_bytes!`
//! into an APK for forty pictures. [`FONT`] is a static instance (`wght` 500,
//! `FILL` 0, `GRAD` 0, `opsz` 24 — outlined, to sit beside Plus Jakarta Sans
//! SemiBold) subset to exactly the glyphs named below, and it costs about 20 kB.
//!
//! **This table is the input to that subset.** `tools/subset-icon-font.py`
//! parses the `icons!` block for `(codepoint, upstream name)` pairs, checks each
//! pair against Google's published `.codepoints` manifest — a wrong `\u{…}`
//! would otherwise ship a plausible-looking *different* icon — and rewrites
//! `assets/fonts/MaterialSymbolsRounded-subset.ttf`. To add an icon: add the
//! line, run the script, commit both. `tests::every_icon_has_a_glyph` fails if
//! you forget the second step.

use crate::systems::furniture::FurnitureKind;
use crate::systems::places::PoiCategory;
use crate::systems::routing::{Maneuver, TravelMode};

/// The subset font, embedded at compile time.
///
/// Like the brand font and the app icon, this is a **Git LFS object**: a clone
/// without `git lfs install` embeds a 130-byte pointer file here and epaint
/// panics on it (see the repo `CLAUDE.md`).
pub const FONT: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../assets/fonts/MaterialSymbolsRounded-subset.ttf"
));

/// The name `theme::fonts` registers [`FONT`] under, and the name it appends to
/// every family's fallback list.
///
/// It names font *data*, not a family: `FontFamily::Name(FONT_NAME)` is unbound
/// and panics in epaint the first frame it is drawn. Draw an icon with
/// `FontId::proportional`, where the fallback list finds it.
pub const FONT_NAME: &str = "MaterialSymbols";

/// One row of the table, for the test that checks the shipped font against it.
///
/// `#[cfg(test)]`, along with [`ALL`]: nothing in the running app reads the
/// table — it reads the constants — and `tools/subset-icon-font.py` parses the
/// macro's *source*, not this const, so it needs neither at run time.
#[cfg(test)]
pub struct Icon {
    /// The Rust constant's name, for a failure message that names the line.
    pub konst: &'static str,
    /// The glyph itself — one private-use codepoint.
    pub glyph: &'static str,
    /// Google's name for it, which is what the `.codepoints` manifest is keyed
    /// by and what you search for on fonts.google.com/icons.
    pub upstream: &'static str,
}

macro_rules! icons {
    ($($(#[$doc:meta])* $name:ident = $glyph:literal, $upstream:literal;)*) => {
        $($(#[$doc])* pub const $name: &str = $glyph;)*

        /// Every icon in the table, in declaration order.
        #[cfg(test)]
        pub const ALL: &[Icon] = &[$(
            Icon { konst: stringify!($name), glyph: $glyph, upstream: $upstream },
        )*];
    };
}

icons! {
    // ─── The right-hand panels (dock disc, menu entry, panel header) ─────────
    /// Search panel.
    SEARCH = "\u{ef7a}", "search";
    /// Map layers panel.
    LAYERS = "\u{e53b}", "layers";
    /// Saved places panel, and a saved place anywhere else.
    BOOKMARK = "\u{e8e7}", "bookmark";
    /// Route planner panel.
    ///
    /// The signpost, not `route`'s winding road: at dock size — a 20 pt glyph in
    /// a 44 pt disc — the road's switchbacks collapse into a squiggle, while an
    /// arrow on a post still reads as an arrow on a post.
    ROUTE = "\u{e52e}", "directions";
    /// Nearby chat panel.
    CHAT = "\u{e0c9}", "chat";
    /// Appearance (avatar) panel.
    APPEARANCE = "\u{f0d3}", "person";
    /// Prim script editor panel.
    SCRIPT = "\u{e86f}", "code";
    /// Settings panel.
    SETTINGS = "\u{e8b8}", "settings";
    /// About panel.
    ABOUT = "\u{e88e}", "info";

    // ─── Ordinary actions ────────────────────────────────────────────────────
    /// The app menu — the dock disc that opens it.
    ///
    /// A hamburger, not the app mark. The mark is the brand, and a logo where
    /// every neighbour is a picture of what it does reads as decoration rather
    /// than as the fifth button in the row: nothing about it says "there is a
    /// menu behind this".
    MENU = "\u{e5d2}", "menu";
    /// History back.
    BACK = "\u{e5c4}", "arrow_back";
    /// History forward.
    FORWARD = "\u{e5c8}", "arrow_forward";
    /// Fly to the home view.
    HOME = "\u{e9b2}", "home";
    /// Confirm, and the tick a "done" flash shows.
    CHECK = "\u{e668}", "check";
    /// Copy to the clipboard.
    COPY = "\u{e14d}", "content_copy";
    /// A session key / token — and signing in with one.
    KEY = "\u{e73c}", "key";
    /// A private, end-to-end encrypted chat (`systems::private_chat`).
    LOCK = "\u{e899}", "lock";
    // The messenger's composer (`chats`): "+" for a place or a location, and
    // send.
    ADD = "\u{e145}", "add";
    SEND = "\u{e163}", "send";
    /// Leave the world and go back to the offline map.
    LOGOUT = "\u{e9ba}", "logout";
    /// Take a photo, or a screenshot slot.
    CAMERA = "\u{e412}", "photo_camera";
    /// Delete, and the litter bin as a piece of street furniture.
    DELETE = "\u{e92e}", "delete";
    /// Rename, edit a field in place.
    RENAME = "\u{f097}", "edit";
    /// Re-take, reload, re-fetch.
    REFRESH = "\u{e5d5}", "refresh";
    /// A point on the map: a place row's thumbnail stand-in, an unclassified POI.
    PIN = "\u{f1db}", "place";
    /// Save the current view as a place.
    BOOKMARK_ADD = "\u{e598}", "bookmark_add";
    /// Group saved places under a new folder.
    NEW_FOLDER = "\u{e2cc}", "create_new_folder";
    /// Open in the system browser.
    OPEN_EXTERNAL = "\u{e89e}", "open_in_new";
    /// A small round mark — the layers legend's colour key.
    ///
    /// A **ring**, not a disc, and there is no disc to be had: the subset is
    /// instanced at `FILL` 0, which is what makes the set outlined, and every
    /// solid shape in it (`circle`, `fiber_manual_record`, `square`) comes out
    /// hollow. The colour is on the stroke, which reads at legend size.
    DOT = "\u{ef4a}", "circle";
    /// Start (audio preview, a script).
    PLAY = "\u{e037}", "play_arrow";
    /// Stop.
    STOP = "\u{e047}", "stop";
    /// Microphone.
    MIC = "\u{e31d}", "mic";
    /// A muted microphone (a call's mute button, while muted).
    MIC_OFF = "\u{e02b}", "mic_off";
    /// A video camera (a call's camera button, while on).
    VIDEOCAM = "\u{e04b}", "videocam";
    /// A camera that is off.
    VIDEOCAM_OFF = "\u{e04c}", "videocam_off";
    /// End a call.
    CALL_END = "\u{f0bc}", "call_end";
    /// Answer a call.
    CALL = "\u{f0d4}", "call";
    /// A raised hand in a call: the key that raises it, and the badge on a tile.
    HAND = "\u{e764}", "back_hand";
    /// Somebody is sharing a screen in a call.
    SCREEN_SHARE = "\u{e0e2}", "screen_share";
    /// Everybody in a call (the people sheet).
    PEOPLE = "\u{ea21}", "group";
    /// Spotlight somebody in a call for everyone.
    STAR = "\u{f09a}", "star";
    /// Take somebody out of a call.
    PERSON_REMOVE = "\u{ef66}", "person_remove";
    /// A call's host, and a host's controls.
    HOST = "\u{f650}", "shield_person";
    /// A script that is running.
    RUNNING = "\u{e1c4}", "play_circle";
    /// A script that is not.
    STOPPED = "\u{ef71}", "stop_circle";
    /// A marked spot the camera can fly to.
    FLAG = "\u{f0c6}", "flag";
    /// Edit a body of text (a prim's script, a description).
    NOTE = "\u{e745}", "edit_note";
    /// Adjust the properties of a thing — the prim edit dialog.
    TUNE = "\u{e429}", "tune";
    /// Materials and colours — the texture library.
    PALETTE = "\u{e40a}", "palette";
    /// Something is wrong but the feature is still on — a denied permission, a
    /// sensor that will not answer.
    WARNING = "\u{f083}", "warning";

    // ─── Route planning ──────────────────────────────────────────────────────
    /// Swap the two endpoints.
    SWAP = "\u{e8d5}", "swap_vert";
    /// Pick an endpoint by clicking the map.
    PICK_ON_MAP = "\u{e567}", "add_location";
    /// Use the current position.
    HERE = "\u{e55c}", "my_location";
    /// Clear a field, dismiss a card.
    CLEAR = "\u{e5cd}", "close";
    /// The start of a route.
    START = "\u{e57b}", "trip_origin";
    /// The end of one — a chequered flag.
    FINISH = "\u{f06e}", "sports_score";

    // ─── Travel modes ────────────────────────────────────────────────────────
    /// By car.
    CAR = "\u{eff7}", "directions_car";
    /// By bike.
    BIKE = "\u{e52f}", "directions_bike";
    /// On foot.
    WALK = "\u{e536}", "directions_walk";

    // ─── Turn-by-turn maneuvers ──────────────────────────────────────────────
    /// Carry straight on.
    CONTINUE = "\u{eb95}", "straight";
    /// Bear left.
    SLIGHT_LEFT = "\u{eba4}", "turn_slight_left";
    /// Turn left.
    LEFT = "\u{eba6}", "turn_left";
    /// Hard left.
    SHARP_LEFT = "\u{eba7}", "turn_sharp_left";
    /// Bear right.
    SLIGHT_RIGHT = "\u{eb9a}", "turn_slight_right";
    /// Turn right.
    RIGHT = "\u{ebab}", "turn_right";
    /// Hard right.
    SHARP_RIGHT = "\u{ebaa}", "turn_sharp_right";
    /// Turn back.
    U_TURN = "\u{eba1}", "u_turn_left";

    // ─── What is coming up on the route ──────────────────────────────────────
    /// Fuel station.
    FUEL = "\u{e546}", "local_gas_station";
    /// Charging point.
    CHARGING = "\u{e56d}", "ev_station";
    /// Parking.
    PARKING = "\u{e54f}", "local_parking";
    /// A café — a rest stop on the road, and the drink POI category.
    DRINK = "\u{eb44}", "local_cafe";

    // ─── The on-screen keyboard ──────────────────────────────────────────────
    /// Backspace. Not `keyboard_backspace`, which is a plain left arrow and
    /// says "go back" on a keyboard where the arrow keys are two cells away.
    BACKSPACE = "\u{e14a}", "backspace";
    /// Enter / accept.
    ENTER = "\u{e31b}", "keyboard_return";
    /// Shift.
    SHIFT = "\u{e318}", "keyboard_capslock";
    /// Put the keyboard away.
    HIDE_KEYBOARD = "\u{e31a}", "keyboard_hide";
    /// Move the caret left.
    CHEVRON_LEFT = "\u{e5cb}", "chevron_left";
    /// Move the caret right.
    CHEVRON_RIGHT = "\u{e5cc}", "chevron_right";

    // ─── Camera modes ────────────────────────────────────────────────────────
    /// Follow the avatar.
    CAM_FOLLOW = "\u{e56a}", "person_pin_circle";
    /// Free flight.
    CAM_FLY = "\u{e539}", "flight";
    /// The whole Earth.
    CAM_EARTH = "\u{e80b}", "public";

    // ─── POI categories (the interact prompt) ────────────────────────────────
    /// Somewhere to eat.
    FOOD = "\u{e56c}", "restaurant";
    /// A shop.
    SHOP = "\u{f1cc}", "shopping_bag";
    /// A theatre, a museum, a gallery.
    CULTURE = "\u{ea66}", "theater_comedy";
    /// A hotel or hostel.
    LODGING = "\u{e549}", "hotel";
    /// A pharmacy, a doctor, a hospital.
    HEALTH = "\u{e548}", "local_hospital";
    /// A school or university.
    EDUCATION = "\u{e80c}", "school";
    /// A station or stop.
    TRANSPORT = "\u{e570}", "train";
    /// A park, a pitch, a playground.
    LEISURE = "\u{ea63}", "park";
    /// A place of worship.
    WORSHIP = "\u{eaae}", "church";
    /// A bank.
    MONEY = "\u{e84f}", "account_balance";

    // ─── Street furniture (the interact prompt) ──────────────────────────────
    /// A bench.
    BENCH = "\u{efed}", "chair";
    /// A bottle bank.
    RECYCLING = "\u{e760}", "recycling";
    /// A shelter.
    SHELTER = "\u{f201}", "roofing";
    /// A picnic table.
    PICNIC_TABLE = "\u{eac6}", "table_restaurant";
    /// A post box, a vending machine, a parcel locker.
    CABINET = "\u{e1a1}", "inventory_2";
    /// An information board.
    BOARD = "\u{ef42}", "article";
    /// A fountain.
    FOUNTAIN = "\u{e798}", "water_drop";
    /// A sculpture on a plinth.
    ARTWORK = "\u{e3ae}", "brush";
    /// A playground. `toys` is the Material name a playground reaches for first
    /// and it draws a toy *car*, which on a map is a road, not a swing.
    SWING = "\u{ea52}", "attractions";
    /// The Layers sheet's six categories (`data_layers::LayerCategory`).
    CAT_MAP = "\u{e55b}", "map";
    CAT_CITY = "\u{e7f1}", "location_city";
    CAT_MOVING = "\u{e940}", "commute";
    CAT_UNDER = "\u{f200}", "foundation";
    CAT_ORBIT = "\u{eb3a}", "satellite_alt";
    /// Layer tiles.
    BUILDINGS = "\u{e7ee}", "domain";
    GRASS = "\u{f205}", "grass";
    SIGNPOST = "\u{eb91}", "signpost";
    FLOOD = "\u{ebe6}", "flood";
    LABEL = "\u{e893}", "label";
    ROAD = "\u{ef3b}", "add_road";
    WATER = "\u{f084}", "water";
    PLUMBING = "\u{f107}", "plumbing";
    LANDSCAPE = "\u{e564}", "landscape";
    BOREHOLE = "\u{e258}", "vertical_align_bottom";
    HEAT_PUMP = "\u{ec18}", "heat_pump";
    VALVE = "\u{e224}", "valve";
    EARTHQUAKE = "\u{f64f}", "earthquake";
    DANGER = "\u{e99a}", "dangerous";
    BUS = "\u{eff6}", "directions_bus";
    GROUP = "\u{ea21}", "group";
    AERIAL = "\u{e562}", "satellite";
    NO_GROUND = "\u{e53c}", "layers_clear";
    /// The ring of actions (`action_ring`).
    SHARE = "\u{e80d}", "share";
    BUILD = "\u{e146}", "add_box";
    TOUCH = "\u{e913}", "touch_app";
    FOLLOW = "\u{e3b4}", "center_focus_strong";
    ORBIT_PATH = "\u{f0c1}", "conversion_path";
    MORE = "\u{e5d3}", "more_horiz";
}

/// The icon for a POI category.
///
/// Lives here rather than on `PoiCategory` (where it was, as
/// `PoiCategory::glyph`) because the glyph is a codepoint in a font **this
/// crate** bundles, and `cartopolis_geo` has no business knowing which one. The
/// category is the durable fact; the picture is a rendering decision.
#[must_use]
pub fn poi(category: PoiCategory) -> &'static str {
    match category {
        PoiCategory::Food => FOOD,
        PoiCategory::Drink => DRINK,
        PoiCategory::Shop => SHOP,
        PoiCategory::Culture => CULTURE,
        PoiCategory::Lodging => LODGING,
        PoiCategory::Health => HEALTH,
        PoiCategory::Education => EDUCATION,
        PoiCategory::Transport => TRANSPORT,
        PoiCategory::Leisure => LEISURE,
        PoiCategory::Worship => WORSHIP,
        PoiCategory::Money => MONEY,
        PoiCategory::Place => PIN,
    }
}

/// The icon for a piece of street furniture. See [`poi`] for why it is here.
#[must_use]
pub fn furniture(kind: FurnitureKind) -> &'static str {
    match kind {
        FurnitureKind::Bench => BENCH,
        FurnitureKind::Bin => DELETE,
        FurnitureKind::Recycling => RECYCLING,
        FurnitureKind::Shelter => SHELTER,
        FurnitureKind::PicnicTable => PICNIC_TABLE,
        FurnitureKind::Cabinet => CABINET,
        FurnitureKind::Board => BOARD,
        FurnitureKind::Fountain => FOUNTAIN,
        FurnitureKind::Artwork => ARTWORK,
        FurnitureKind::Swing => SWING,
        // A street light is solid but not interactable (`prop_interactable`), so
        // this is never drawn either. `CABINET`'s box-on-a-post is the nearest
        // thing in the subset and costs no font work.
        FurnitureKind::Lamp => CABINET,
        // Parked vehicles are not interactables (`world_places::CellPlaces`), so
        // these are never drawn. The travel-mode icons already in the subset are
        // what they would use, so the arms cost nothing and no font work.
        FurnitureKind::Car => CAR,
        FurnitureKind::Bicycle => BIKE,
        FurnitureKind::Person => WALK,
        FurnitureKind::Boat => TRANSPORT,
        // Solid and not interactable, like a street light: never drawn.
        FurnitureKind::TrafficLight => CABINET,
        FurnitureKind::Yacht | FurnitureKind::Houseboat => TRANSPORT,
        // Solid and not interactable, like a street light: never drawn.
        FurnitureKind::BikeRack => BIKE,
        FurnitureKind::Toilet => SHELTER,
    }
}

/// The icon for a turn instruction. See [`poi`] for why it is here.
///
/// The sharp variants get their own arrows rather than sharing the plain ones:
/// the set has them, and "turn left" against "hard left" is exactly the
/// distinction a driver needs at the junction rather than in the text below it.
#[must_use]
pub fn maneuver(maneuver: Maneuver) -> &'static str {
    match maneuver {
        Maneuver::Depart => START,
        Maneuver::Continue => CONTINUE,
        Maneuver::SlightLeft => SLIGHT_LEFT,
        Maneuver::Left => LEFT,
        Maneuver::SharpLeft => SHARP_LEFT,
        Maneuver::SlightRight => SLIGHT_RIGHT,
        Maneuver::Right => RIGHT,
        Maneuver::SharpRight => SHARP_RIGHT,
        Maneuver::UTurn => U_TURN,
        Maneuver::Arrive => FINISH,
    }
}

/// The icon for a travel mode. See [`poi`] for why it is here.
#[must_use]
pub fn travel_mode(mode: TravelMode) -> &'static str {
    match mode {
        TravelMode::Car => CAR,
        TravelMode::Bike => BIKE,
        TravelMode::Foot => WALK,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::systems::egui_manager::UnpaintedPasses as _;

    /// **Every icon in the table must be in the shipped subset.**
    ///
    /// The table and the font are two files that have to agree, and the way they
    /// stop agreeing is that somebody adds a row and does not re-run
    /// `tools/subset-icon-font.py` — which is invisible until the icon is looked
    /// at on a screen, since a missing glyph is an empty box and nothing logs.
    #[test]
    fn every_icon_has_a_glyph() {
        let ctx = egui::Context::default();
        crate::systems::theme::install(&ctx);
        // Inside a pass, like the other font tests: the fonts are built on the
        // first frame, and asking before one has run answers about a stack that
        // does not exist yet.
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(400.0, 300.0));
        let _ = ctx.run_ui_unpainted(
            egui::RawInput {
                screen_rect: Some(screen),
                ..Default::default()
            },
            |ui| {
                let font = egui::TextStyle::Body.resolve(ui.style());
                let missing: Vec<&str> = ui.ctx().fonts_mut(|f| {
                    ALL.iter()
                        .filter(|i| !f.has_glyphs(&font, i.glyph))
                        .map(|i| i.konst)
                        .collect()
                });
                assert!(
                    missing.is_empty(),
                    "the subset font has no glyph for {missing:?} — re-run \
                     `tools/subset-icon-font.py` and commit \
                     assets/fonts/MaterialSymbolsRounded-subset.ttf with the table",
                );
            },
        );
    }

    /// One codepoint per icon, and no two icons sharing one by accident.
    ///
    /// Sharing is *sometimes* right — a litter bin is a delete icon — so this
    /// checks the constant names of any pair that collides, and the deliberate
    /// ones are listed here. It is cheap insurance against a copy-pasted row
    /// that silently draws the row above it.
    #[test]
    fn every_icon_is_one_codepoint_and_deliberately_shared() {
        const SHARED: &[&[&str]] = &[];
        for icon in ALL {
            assert_eq!(
                icon.glyph.chars().count(),
                1,
                "{} is {} characters, not one",
                icon.konst,
                icon.glyph.chars().count()
            );
        }
        for (i, a) in ALL.iter().enumerate() {
            for b in &ALL[i + 1..] {
                let excused = SHARED
                    .iter()
                    .any(|g| g.contains(&a.konst) && g.contains(&b.konst));
                assert!(
                    a.glyph != b.glyph || excused,
                    "{} and {} are the same glyph ({} / {}) — reuse one constant, \
                     or list the pair in SHARED",
                    a.konst,
                    b.konst,
                    a.upstream,
                    b.upstream,
                );
            }
        }
    }
}
