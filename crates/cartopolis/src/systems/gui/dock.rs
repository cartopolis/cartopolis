//! The map's chrome: one **World | Community** switch along the bottom, a
//! **search bar with your face** across the top, and the map's round buttons
//! in a column down the right (`docs/mockups/redesign`, v2, 2026-11-10).
//!
//! The app has two spaces. *World* is the map, with round buttons on it and
//! cards along the bottom; *Community* is the people — chats, groups, the
//! calendar — on a solid page (the `RightPanel::Chat` panel: the whole screen
//! on a phone, a side panel on a desktop). One switch moves between them, in
//! the same place in both, and your face sits top right in both: it opens the
//! sheet that holds everything about you and the app (`ui::drive_main_menu`),
//! which replaced the ☰ menu.
//!
//! **What it replaced.** A dock of up to six labelled discs along the bottom
//! (Search, Say, the view mode, Camera, Saved, Menu), the layers and lens discs
//! in the top-left corner, and a Community | World switch at the top of the
//! map that only showed signed in. The person called the navigation not
//! intuitive (2026-11-08): nothing said how the world and the community hang
//! together, and the two were mixed in one row. Now:
//!
//!   - **Search** is the pill across the top, as every map app has it
//!     (`Ctrl+L` still opens it). It opens the Search panel; the field and
//!     the hits live there.
//!   - **Saved places** is a row of your face's sheet: it is yours, not the
//!     map's.
//!   - **Layers, the view mode, Locate and Camera** are a column on the right
//!     under the search bar ([`right_column`]); Locate is drawn by `hud`, at
//!     the slot this module gives it.
//!   - **The compass** stays top left, under the bar, with the lens and the
//!     light discs below it ([`left_slot`]), and **Say** — the nearby-chat
//!     composer, only in a session — under those: a world thing, kept on the
//!     map, on the left where the transcript it opens is.
//!
//! **The switch is always there**, signed out too: Community without an
//! account is the card that says what signing in is for (`systems::welcome`,
//! [`crate::systems::welcome::Welcome::community`]), never a dead end. On a
//! phone a panel is the whole screen and the map's chrome drops while one is
//! open (the chrome is drawn over panels) — except the switch over
//! Community, which is the way back.
//!
//! **Drawn by the library** (`lib_chrome`): [`drive_dock`] decides what
//! shows, where and in which state, publishes it in [`MapChrome::lib`] and
//! acts on the taps handed back. It reads the map's rect (what the side
//! panels left, `UiInput`), so on a desktop the bar and the switch sit over
//! the map that is left, not under the panel.

use bevy::prelude::*;

use crate::resources::{OnlineSession, RightPanel};
use crate::systems::theme;
use crate::systems::ui_input::{UiInput, UiMetrics};

/// Height of the World | Community switch: a 48 pt touch row.
pub(crate) const SWITCH_H: f32 = 48.0;
/// The switch's widest, on a desktop.
pub(crate) const SWITCH_MAX_W: f32 = 264.0;
/// Height of the search bar across the top.
pub(crate) const SEARCH_H: f32 = 44.0;
/// The search bar's widest, on a desktop. It is anchored at the map's top
/// right, so your face is at its end there too.
const SEARCH_MAX_W: f32 = 420.0;
/// Diameter of your face at the end of the search bar.
pub(crate) const FACE: f32 = 36.0;
/// Room under the view-mode disc for its word ("Fly", "Earth").
pub(crate) const MODE_WORD_H: f32 = 16.0;

/// How much of the bottom edge the switch claims, in points.
///
/// Exported so that anything else anchored down there — the cards, the
/// thumbstick, the scale bar, the composer — sits *above* it rather than
/// guessing. A second copy of this arithmetic is how two of them drift.
#[must_use]
pub fn reserved_bottom() -> f32 {
    theme::OSD_MARGIN + SWITCH_H
}

/// How much of the top edge the search bar claims: where both columns of map
/// buttons start.
#[must_use]
pub fn reserved_top() -> f32 {
    theme::OSD_MARGIN + SEARCH_H + theme::OSD_MARGIN
}

/// Whether the map's round buttons **lie down**: two rows in the search bar's
/// band, beside the bar, instead of two columns under it.
///
/// A phone on its side is ~420 pt tall (the OnePlus 15, 2026-11-12), and the
/// right column (down to 268 pt) and Follow's touch controls (266 pt up from
/// the bottom) need 552 between them: the column stood on the jump and
/// flight discs, the locate caption under the face disc, and Say under the
/// scale bar. Lying down, the columns take no height under the bar at all,
/// and the touch controls stand side by side in the bottom corners
/// (`map_osd::Spots`), where the thumbs of a phone held in two hands are.
/// Only when there is room beside the bar for both rows (`ROWS_MIN_W`): the
/// gate's 640×360 px phone smoke at 1.5 is 427 pt wide, and lying there left
/// the bar a negative width, which the layout panicked on.
#[must_use]
pub fn lying(view: Vec2) -> bool {
    view.x >= ROWS_MIN_W
        && view.y
            < column(true, true).bottom
                + theme::OSD_GROUP_GAP
                + crate::systems::map_osd::right_edge_reach(false)
}

/// One disc's step along a row or a column.
const STEP: f32 = theme::ROUND_BUTTON_SIZE + theme::OSD_GAP;

/// The top of a disc in a lying row: centred on the search bar.
const ROW_TOP: f32 = theme::OSD_MARGIN + (SEARCH_H - theme::ROUND_BUTTON_SIZE) / 2.0;

/// The widest a lying row gets: four discs.
const ROW_W: f32 = 4.0 * STEP - theme::OSD_GAP;

/// The narrowest the search bar gets beside the rows: your face and the
/// magnifier, and 90 pt of words between them.
const SEARCH_MIN_W: f32 = FACE + 44.0 + 90.0;

/// The narrowest screen the rows lie down on: both rows and the narrowest bar
/// between the margins.
const ROWS_MIN_W: f32 =
    2.0 * theme::OSD_MARGIN + 2.0 * (ROW_W + theme::OSD_GROUP_GAP) + SEARCH_MIN_W;

/// The search bar's width over a map `map_w` wide. Lying, it keeps out of
/// both rows — at their fullest, so the bar does not jump when a session
/// brings Say or a fix brings Locate.
#[must_use]
pub fn search_width(view: Vec2, map_w: f32) -> f32 {
    let mut room = map_w - 2.0 * theme::OSD_MARGIN;
    if lying(view) {
        room -= 2.0 * (ROW_W + theme::OSD_GROUP_GAP);
    }
    room.min(SEARCH_MAX_W)
}

/// Where the `i`th disc of the left set goes, as a `LEFT_TOP` anchor offset:
/// the compass (0), the lens (1), the light (2) and Say (3). Standing, one
/// column under the bar; lying, one row from the corner in the bar's band.
#[must_use]
pub fn left_slot(view: Vec2, i: usize) -> Vec2 {
    if lying(view) {
        Vec2::new(theme::OSD_MARGIN + i as f32 * STEP, ROW_TOP)
    } else {
        Vec2::new(theme::OSD_MARGIN, reserved_top() + i as f32 * STEP)
    }
}

/// The left set's rect, its first `n` discs, for the overlays to keep off.
#[must_use]
pub fn left_rect(view: Vec2, n: usize) -> Rect {
    let first = left_slot(view, 0);
    let last = left_slot(view, n.max(1) - 1);
    Rect::from_corners(first, last + Vec2::splat(theme::ROUND_BUTTON_SIZE))
}

/// Where each disc of the right set goes, as a `RIGHT_TOP` anchor offset:
/// `x` is how far the disc's right edge stands in from the screen's.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RightColumn {
    pub layers: Vec2,
    pub mode: Vec2,
    /// `hud`'s locate disc, where there is an avatar to locate.
    pub locate: Option<Vec2>,
    pub camera: Option<Vec2>,
    /// The bottom of the last disc (and its words): what anything else on the
    /// right edge — the zoom rail, the floor picker — starts under.
    pub bottom: f32,
    /// A row in the bar's band rather than a column under it ([`lying`]):
    /// Locate's caption goes under its disc, not beside it.
    pub lying: bool,
}

impl RightColumn {
    /// How far in from the right edge something left of the set stands
    /// (the Midas disc): beside the column, or — lying — at the margin under
    /// the bar, which the row leaves free.
    #[must_use]
    // The Midas link is native only.
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    pub fn beside(&self) -> f32 {
        if self.lying {
            theme::OSD_MARGIN
        } else {
            theme::OSD_MARGIN + STEP
        }
    }

    /// The set's rect in a `view`, edge to edge of its discs and words.
    #[must_use]
    pub fn rect(&self, view: Vec2) -> Rect {
        let discs = [Some(self.layers), Some(self.mode), self.locate, self.camera];
        let (mut l, mut r) = (f32::MAX, f32::MIN);
        for d in discs.into_iter().flatten() {
            l = l.min(view.x - d.x - theme::ROUND_BUTTON_SIZE);
            r = r.max(view.x - d.x);
        }
        Rect::from_corners(Vec2::new(l, self.layers.y), Vec2::new(r, self.bottom))
    }
}

/// The right set: Layers, the view mode (its word under it), Locate and
/// Camera — the mockup's four. Standing, a column under the search bar;
/// lying, a row left of it, Layers nearest the bar so that no disc moves
/// when Locate or Camera comes or goes.
#[must_use]
pub fn right_column(view: Vec2, has_locate: bool, has_camera: bool) -> RightColumn {
    if !lying(view) {
        return column(has_locate, has_camera);
    }
    let bar = search_width(view, view.x);
    let at = |i: f32| {
        Vec2::new(
            theme::OSD_MARGIN + bar + theme::OSD_GROUP_GAP + i * STEP,
            ROW_TOP,
        )
    };
    RightColumn {
        layers: at(0.0),
        mode: at(1.0),
        locate: has_locate.then(|| at(2.0)),
        camera: has_camera.then(|| at(if has_locate { 3.0 } else { 2.0 })),
        // The mode's word, and Locate's caption, under the row.
        bottom: ROW_TOP + theme::ROUND_BUTTON_SIZE + MODE_WORD_H,
        lying: true,
    }
}

/// The right column standing: under the search bar, top down.
fn column(has_locate: bool, has_camera: bool) -> RightColumn {
    let disc = theme::ROUND_BUTTON_SIZE;
    let at = |top: f32| Vec2::new(theme::OSD_MARGIN, top);
    let layers = reserved_top();
    let mode = layers + disc + theme::OSD_GAP;
    let mut next = mode + disc + MODE_WORD_H + theme::OSD_GAP;
    let locate = has_locate.then(|| {
        let top = next;
        // Its caption ("±4 m", "Searching…") sits to the left of the disc
        // (`hud`), not under it: the room under it is what lets Follow's
        // touch controls and this column share a phone's right edge.
        next += disc + theme::OSD_GAP;
        at(top)
    });
    let camera = has_camera.then(|| {
        let top = next;
        next += disc + theme::OSD_GAP;
        at(top)
    });
    RightColumn {
        layers: at(layers),
        mode: at(mode),
        locate,
        camera,
        bottom: next - theme::OSD_GAP,
        lying: false,
    }
}

/// What the chrome drew last frame, for what is drawn before it in the next:
/// the zoom rail and the floor picker start under the right column.
#[derive(Resource, Debug, Clone)]
pub struct MapChrome {
    pub right_bottom: f32,
    /// The map's buttons, both sets edge to edge of their discs and words,
    /// in UI points: what the map's overlays keep off
    /// (`place_pins::clear_of_chrome`). Empty while nothing is shown.
    pub holes: Vec<Rect>,
    /// What the library draws of the chrome (`lib_chrome`): the dock, the
    /// view controls (`hud`) and the floor picker.
    pub lib: ChromeLib,
}

impl Default for MapChrome {
    fn default() -> Self {
        Self {
            right_bottom: column(true, true).bottom,
            holes: Vec::new(),
            lib: ChromeLib::default(),
        }
    }
}

/// The bridge to the library's chrome (`lib_chrome`). The systems
/// ([`drive_dock`], `hud::drive_map_controls`,
/// `floor_picker::drive_floor_picker`) keep every decision — what shows,
/// where, in which state — and publish it here; the library draws it and
/// hands each tap back in `acts`, which the same systems act on next frame.
#[derive(Debug, Clone, Default)]
pub struct ChromeLib {
    pub dock: DockView,
    pub hud: crate::systems::hud::HudView,
    pub floors: Option<crate::systems::floor_picker::FloorsView>,
    /// Taps on the library's chrome, not yet acted on.
    pub acts: Vec<ChromeAct>,
}

impl ChromeLib {
    /// The acts `mine` claims, taken out of the queue.
    pub fn take_acts(&mut self, mine: fn(&ChromeAct) -> bool) -> Vec<ChromeAct> {
        let mut out = Vec::new();
        self.acts.retain(|a| {
            if mine(a) {
                out.push(*a);
                false
            } else {
                true
            }
        });
        out
    }
}

/// Assign only when the value moved: the library re-renders a piece when
/// what it is handed differs.
pub fn set_if_changed<T: PartialEq>(slot: &mut T, value: T) {
    if *slot != value {
        *slot = value;
    }
}

/// A tap on the library's chrome.
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(not(all(feature = "ui-lib", feature = "comms")), allow(dead_code))]
pub enum ChromeAct {
    /// A side of the World | Community switch.
    Space(Space),
    Search,
    Face,
    /// The way back from Show in world, where the search field was.
    Back,
    Layers,
    Mode,
    Camera,
    Lens,
    Say,
    // `hud`'s.
    Compass,
    Locate,
    /// The light disc: open or close its choices.
    LightDisc,
    Light(crate::resources::SunMode),
    // The floor picker's.
    FloorPick(Option<usize>),
    FloorClose,
    FloorPlan,
}

impl ChromeAct {
    #[must_use]
    pub fn is_dock(&self) -> bool {
        matches!(
            self,
            Self::Space(_)
                | Self::Search
                | Self::Face
                | Self::Back
                | Self::Layers
                | Self::Mode
                | Self::Camera
                | Self::Lens
                | Self::Say
        )
    }

    #[must_use]
    pub fn is_hud(&self) -> bool {
        matches!(
            self,
            Self::Compass | Self::Locate | Self::LightDisc | Self::Light(_)
        )
    }

    #[must_use]
    pub fn is_floor(&self) -> bool {
        matches!(
            self,
            Self::FloorPick(_) | Self::FloorClose | Self::FloorPlan
        )
    }
}

/// What the dock shows this frame, in UI points (the library's px on the
/// Bevy backend), for the library. `None` is a piece not shown.
#[derive(Debug, Clone, Default, PartialEq)]
#[cfg_attr(not(feature = "ui-lib"), allow(dead_code))]
pub struct DockView {
    pub switch: Option<SwitchView>,
    pub bar: Option<BarView>,
    pub right: Option<RightView>,
    pub left: Option<LeftView>,
}

/// The World | Community switch: from the screen's left, its width.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(not(feature = "ui-lib"), allow(dead_code))]
pub struct SwitchView {
    pub left: f32,
    pub width: f32,
    pub on: Space,
    pub unread: usize,
}

/// The search bar: how far its right end stands in from the screen's.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(not(feature = "ui-lib"), allow(dead_code))]
pub struct BarView {
    pub right: f32,
    pub width: f32,
    /// The Search panel is open: the ring is the accent.
    pub open: bool,
    /// After Show in world: the way back, in place of the field.
    pub back: Option<String>,
    pub face: Option<FaceView>,
}

/// Your face at the end of the bar.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(not(feature = "ui-lib"), allow(dead_code))]
pub struct FaceView {
    pub me: Option<String>,
    pub waiting: bool,
    pub open: bool,
}

/// The right set, at `RIGHT_TOP` offsets ([`RightColumn`]).
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(not(feature = "ui-lib"), allow(dead_code))]
pub struct RightView {
    pub layers: Vec2,
    pub mode: Vec2,
    pub mode_icon: &'static str,
    pub mode_word: &'static str,
    pub mode_hint: String,
    pub camera: Option<Vec2>,
    pub camera_on: bool,
}

/// The left set's discs this module draws, at `LEFT_TOP` offsets.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(not(feature = "ui-lib"), allow(dead_code))]
pub struct LeftView {
    pub lens: Vec2,
    pub lens_on: bool,
    pub say: Option<Vec2>,
    pub say_on: bool,
}

/// The two spaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Space {
    World,
    Community,
}

/// The people half of what the chrome reads, bundled under Bevy's
/// sixteen-parameter ceiling.
#[derive(bevy::ecs::system::SystemParam)]
pub struct People<'w> {
    chat: Res<'w, crate::systems::chat::ChatState>,
    identity: Res<'w, crate::resources::AuthIdentity>,
    welcome: ResMut<'w, crate::systems::welcome::Welcome>,
    #[cfg(feature = "comms")]
    talk: Talk<'w>,
}

/// What only the app's chrome has (`comms`): the Community side of the
/// switch, Say, the camera and the way back from Show in world. The web
/// build's World tab has none of them — the page around it is Community.
#[cfg(feature = "comms")]
#[derive(bevy::ecs::system::SystemParam)]
pub struct Talk<'w> {
    view: ResMut<'w, crate::systems::chats::ChatsView>,
    contacts: Res<'w, crate::systems::contacts::Contacts>,
    bridges: ResMut<'w, crate::systems::bridges::Bridges>,
    say: ResMut<'w, crate::systems::chat_overlay::ChatOverlay>,
    camera: ResMut<'w, crate::systems::camera_screen::CameraScreen>,
}

/// What owns the screen instead of the map's chrome, bundled for the same
/// reason.
#[derive(bevy::ecs::system::SystemParam)]
pub struct Covers<'w> {
    driver: Res<'w, crate::systems::driver::DriverState>,
    keyboard: Res<'w, crate::systems::onscreen_keyboard::OnScreenKeyboard>,
    walk: Res<'w, crate::systems::walk_guide::WalkGuide>,
    // The design room is a white room: nothing of the map's chrome in it.
    room: Res<'w, crate::systems::design_room::DesignRoom>,
}

/// Decide the switch, the search bar with your face, the right column and the
/// lens and Say discs — what shows, where, in which state — publish them for
/// the library (`lib_chrome`), and act on their taps.
#[allow(clippy::too_many_arguments)]
pub fn drive_dock(
    metrics: Res<UiMetrics>,
    ui_input: Res<UiInput>,
    mut right_panel: ResMut<RightPanel>,
    mut menu: ResMut<crate::systems::ui::MainMenu>,
    mut camera_state: ResMut<crate::resources::CameraState>,
    session: Res<crate::systems::session::SessionState>,
    online: Option<Res<OnlineSession>>,
    covers: Covers,
    avatars: Query<(), With<crate::components::Avatar>>,
    mut lens: ResMut<crate::systems::scenario::ScenarioView>,
    data_layers: Res<crate::systems::layer_state::DataLayers>,
    mut people: People,
    mut chrome: ResMut<MapChrome>,
) {
    #[cfg(feature = "comms")]
    let has_camera = crate::systems::camera_screen::available();
    #[cfg(not(feature = "comms"))]
    let has_camera = false;
    #[cfg(feature = "comms")]
    let camera_open = people.talk.camera.is_open();
    #[cfg(not(feature = "comms"))]
    let camera_open = false;
    let has_avatar = !avatars.is_empty();
    let viewport = metrics.viewport_pts();
    chrome.right_bottom = right_column(viewport, has_avatar, has_camera).bottom;
    let acts = chrome.lib.take_acts(ChromeAct::is_dock);
    let mut view = DockView::default();
    // Driving owns the screen, and so do the camera and the design room.
    if covers.driver.active || camera_open || covers.room.spot.is_some() {
        set_if_changed(&mut chrome.holes, Vec::new());
        menu.open = false;
        set_if_changed(&mut chrome.lib.dock, view);
        return;
    }
    let has_session = online.is_some();
    #[cfg(feature = "comms")]
    let demo = people.talk.view.demo;
    #[cfg(not(feature = "comms"))]
    let demo = false;
    let signed_in = has_session || demo;
    // A panel other than Community opening ends the signed-out Community card:
    // the person went somewhere else.
    if !right_panel.is(RightPanel::None) && !right_panel.is(RightPanel::Chat) {
        people.welcome.community = false;
    }
    let community_open = right_panel.is(RightPanel::Chat) && signed_in;
    let space = if community_open || people.welcome.community {
        Space::Community
    } else {
        Space::World
    };
    let compact = metrics.compact();
    // The map: what the side panels left, as of the last frame
    // (`UiInput`'s map rect, in window points — UI points times the UI
    // scale). On a phone with a panel up that is nothing, and the switch
    // centres on the screen.
    let map = map_rect(&ui_input, &metrics);
    let panel_open = !right_panel.is(RightPanel::None);
    let covered = compact && panel_open;
    let keyboard = covers.keyboard.is_open();

    let me = people
        .identity
        .username
        .clone()
        .filter(|n| !n.is_empty())
        .or_else(|| demo.then(|| "jeroen".to_string()));
    let me = me.filter(|_| signed_in);
    #[cfg(feature = "comms")]
    let waiting = signed_in
        && (!people.talk.contacts.incoming.is_empty()
            || people.talk.view.admin.is_some_and(|n| n > 0));
    #[cfg(not(feature = "comms"))]
    let waiting = false;

    // ── The switch ───────────────────────────────────────────────────────
    // Over Community on a phone too — it is the way back — but not inside a
    // chat, whose composer has the bottom edge, nor over the keys.
    // Nor under the map's chat sheet on a phone, which has it too.
    // The web build has no switch at all: the page around it is the switch.
    #[cfg(feature = "comms")]
    let switch_shown = !keyboard
        && !(compact && people.talk.say.composing)
        && (!covered || (community_open && people.talk.view.open.is_none()));
    #[cfg(not(feature = "comms"))]
    let switch_shown = false;
    let mut picked = None;
    if switch_shown {
        // Where the switch stands: centred on the map, or on the screen
        // under a full-screen panel.
        let (left, right) = if covered {
            (0.0, viewport.x)
        } else {
            (map.min.x, map.max.x)
        };
        let width = (right - left - 2.0 * theme::OSD_MARGIN).min(SWITCH_MAX_W);
        let centre = (left + right) / 2.0;
        #[cfg(feature = "comms")]
        let unread = if signed_in {
            people.talk.view.unread_total(&people.chat)
        } else {
            0
        };
        #[cfg(not(feature = "comms"))]
        let unread = 0;
        view.switch = Some(SwitchView {
            left: centre - width / 2.0,
            width,
            on: space,
            unread,
        });
    }

    // ── The search bar and your face ─────────────────────────────────────
    // Not over a full-screen panel, nor a chat's World view, whose header has
    // the top of the map, nor while walking a route, whose next turn has it.
    let bar_shown = !covered && !keyboard && people.chat.world.is_none() && !covers.walk.active;
    let mut search_clicked = false;
    let mut face_clicked = false;
    if bar_shown {
        let width = search_width(viewport, map.width());
        // On a desktop with Community open the panel wears your face at its
        // own top right; one face, not two.
        let with_face = !community_open;
        // After Show in world, the way back stands where the search field
        // was, with your face still at its end (bridge ②, N7).
        #[cfg(feature = "comms")]
        let back = people.talk.bridges.back.as_ref().map(|b| b.label.clone());
        #[cfg(not(feature = "comms"))]
        let back: Option<String> = None;
        // Where the face stands, for the sheet's tap-outside test
        // (`ui::MainMenu::face`): at the bar's right end, `SEARCH_H` across
        // beside the way back, `FACE` inside the pill.
        let right = map.max.x - theme::OSD_MARGIN;
        let mid = theme::OSD_MARGIN + SEARCH_H / 2.0;
        if with_face {
            let (size, inset) = if back.is_some() {
                (SEARCH_H, 0.0)
            } else {
                (FACE, 4.0)
            };
            menu.face = Some(Rect::from_center_size(
                Vec2::new(right - inset - size / 2.0, mid),
                Vec2::splat(size),
            ));
        }
        view.bar = Some(BarView {
            right: viewport.x - right,
            width,
            open: right_panel.is(RightPanel::Search),
            back,
            face: with_face.then(|| FaceView {
                me: me.clone(),
                waiting,
                open: menu.open,
            }),
        });
    }

    // ── The right column, and the lens and Say on the left ───────────────
    // Dropped while any panel is open, as the compass and the touch controls
    // are: on a desktop the panel docks to the right edge they stand on.
    let mut clicked: Option<RightPanel> = None;
    let mut camera_clicked = false;
    // Say is the app's only (`comms`).
    #[cfg_attr(not(feature = "comms"), allow(unused_mut))]
    let mut say_clicked = false;
    let mut holes = Vec::new();
    // The lens (`systems::scenario`) is on while a scenario is.
    let has_lens = data_layers.scenario.is_some();
    #[cfg(feature = "comms")]
    let composing = people.talk.say.composing;
    #[cfg(not(feature = "comms"))]
    let composing = false;
    if !panel_open && !keyboard {
        let col = right_column(viewport, has_avatar, has_camera);
        // What the map's overlays keep off (`place_pins::clear_of_chrome`):
        // both sets, edge to edge of their discs and words.
        holes.push(col.rect(viewport));
        // Say is the fourth disc on the left, in a session of the app.
        let say_disc = has_session && cfg!(feature = "comms");
        holes.push(left_rect(viewport, if say_disc { 4 } else { 3 }));
        let look = mode_look(&camera_state, &session, has_avatar);
        view.right = Some(RightView {
            layers: col.layers,
            mode: col.mode,
            mode_icon: look.icon,
            mode_word: look.word,
            mode_hint: look.hint,
            camera: col.camera,
            camera_on: camera_open,
        });
        // The lens: figures you read, one at a time, apart from the switches
        // Layers holds; under the compass. Say — the composer over the map
        // (`systems::chat_overlay`), only in a session — carries the accent
        // while the composer is up.
        view.left = Some(LeftView {
            lens: left_slot(viewport, 1),
            lens_on: crate::systems::scenario::lens_disc_on(&lens, has_lens),
            say: say_disc.then(|| left_slot(viewport, 3)),
            say_on: composing,
        });
    }

    set_if_changed(&mut chrome.holes, holes);
    set_if_changed(&mut chrome.lib.dock, view);

    // ── Acting on it ─────────────────────────────────────────────────────
    for act in acts {
        match act {
            // A tap on the side already chosen does nothing, except World,
            // which puts a side panel away.
            ChromeAct::Space(side) if side != space || side == Space::World => {
                picked = Some(side);
            }
            ChromeAct::Search => search_clicked = true,
            ChromeAct::Face => face_clicked = true,
            ChromeAct::Back => {
                #[cfg(feature = "comms")]
                {
                    people.talk.bridges.back_pressed = true;
                }
            }
            ChromeAct::Layers => clicked = Some(RightPanel::Layers),
            ChromeAct::Mode => step_mode(&mut camera_state, &session, has_avatar),
            ChromeAct::Camera => camera_clicked = true,
            ChromeAct::Lens => {
                crate::systems::scenario::press_lens_disc(&mut lens, has_lens);
                menu.open = false;
            }
            ChromeAct::Say => say_clicked = true,
            _ => {}
        }
    }
    match picked {
        Some(Space::World) => {
            // World is the map with no panel: a tap from anywhere comes back
            // to it, a side panel included.
            *right_panel = RightPanel::None;
            people.welcome.community = false;
            menu.open = false;
        }
        Some(Space::Community) => {
            if signed_in {
                *right_panel = RightPanel::Chat;
                #[cfg(feature = "comms")]
                {
                    people.talk.view.open = None;
                    people.talk.view.you = false;
                }
            } else {
                // Signed out, Community is what signing in is for, with the
                // way in and a way back to the map (`systems::welcome`).
                *right_panel = RightPanel::None;
                people.welcome.community = true;
            }
            menu.open = false;
        }
        None => {}
    }
    if search_clicked {
        right_panel.toggle(RightPanel::Search);
        menu.open = false;
    }
    if face_clicked {
        menu.open = !menu.open;
    }
    if let Some(panel) = clicked {
        right_panel.toggle(panel);
        menu.open = false;
    }
    #[cfg(feature = "comms")]
    if camera_clicked {
        people.talk.camera.request = Some(crate::systems::camera_screen::Target::Anywhere);
        menu.open = false;
    }
    #[cfg(feature = "comms")]
    if say_clicked {
        if people.talk.say.composing {
            people.talk.say.close();
        } else {
            people.talk.say.open();
        }
        menu.open = false;
    }
    #[cfg(not(feature = "comms"))]
    let _ = (camera_clicked, say_clicked);
}

/// The map in UI points: what the side panels left at the end of the last
/// frame (`UiInput`'s map rect is in window points), or the whole viewport
/// before there is one.
fn map_rect(ui_input: &UiInput, metrics: &UiMetrics) -> Rect {
    let view = metrics.viewport_pts();
    let k = metrics.ui_scale().max(f32::EPSILON);
    ui_input
        .map_rect()
        .map_or(Rect::from_corners(Vec2::ZERO, view), |r| {
            Rect::from_corners(r.min / k, r.max / k)
        })
}

/// What the view-mode disc wears: the mode's glyph, its word under the disc,
/// and the hint that says where a tap goes.
pub struct ModeLook {
    pub icon: &'static str,
    pub word: &'static str,
    pub hint: String,
}

/// The view-mode disc's look in the current mode.
#[must_use]
pub fn mode_look(
    camera_state: &crate::resources::CameraState,
    session: &crate::systems::session::SessionState,
    has_avatar: bool,
) -> ModeLook {
    use crate::resources::CameraMode;
    use crate::systems::free_camera::{follow_available, next_camera_mode};
    use crate::systems::icons;
    let badge = |mode: CameraMode| match mode {
        CameraMode::Avatar => (icons::CAM_FOLLOW, "Follow"),
        // The placed view a shared place or a capture opens in: looked
        // around in, never flown (the camera stopped flying on 2026-11-25).
        CameraMode::Free => (icons::CAM_FLY, "View"),
        CameraMode::Orbit => (icons::CAM_EARTH, "Earth"),
    };
    let next = next_camera_mode(camera_state.mode, follow_available(session, has_avatar));
    let (icon, mut name) = badge(camera_state.mode);
    // First person is a sub-state of Follow, with Follow's glyph — but a
    // different view, and the disc is where that has to show.
    if camera_state.mode == CameraMode::Avatar && camera_state.first_person {
        name = "First person";
    }
    let (_, next_name) = badge(next);
    let word = if name == "First person" {
        "1st person"
    } else {
        name
    };
    ModeLook {
        icon,
        word,
        hint: format!("{name} view — switch to {next_name}"),
    }
}

/// A tap on the view-mode disc: the next mode on offer, as Tab steps.
pub fn step_mode(
    camera_state: &mut crate::resources::CameraState,
    session: &crate::systems::session::SessionState,
    has_avatar: bool,
) {
    use crate::systems::free_camera::{enter_mode, follow_available, next_camera_mode};
    let next = next_camera_mode(camera_state.mode, follow_available(session, has_avatar));
    enter_mode(camera_state, next);
    camera_state.mode = next;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The documented phone viewports (`ui-vocabulary.md`): 274 pt on an
    /// Android handset at `ui_scale` 1.5, ≈214 pt on the web phone tier — the
    /// narrower and the one layout tests keep forgetting. 600 pt is the
    /// height of the phone shots (412×900 px at 1.5).
    const PHONES: [f32; 2] = [274.0, 214.0];
    const PHONE_H: f32 = 600.0;
    /// A phone on its side: the OnePlus 15's 2772×1272 px at 3 px a point
    /// (2026-11-12).
    const LYING: Vec2 = Vec2::new(924.0, 424.0);

    /// The switch and the search bar are size-to-content `Area`s that nothing
    /// clamps; what they ask for is what they take, off the screen included.
    /// Both are measured from the viewport, so they fit by construction —
    /// this pins that the construction holds, margins and all.
    #[test]
    fn the_switch_and_the_bar_fit_a_phone() {
        for w in PHONES {
            let switch = (w - 2.0 * theme::OSD_MARGIN).min(SWITCH_MAX_W);
            assert!(switch + 2.0 * theme::OSD_MARGIN <= w, "{switch} in {w}");
            // Two segments that each still hold a word at the smallest size.
            assert!(
                (switch - 10.0) / 2.0 >= 80.0,
                "a segment of {switch} is too narrow"
            );
            let bar = (w - 2.0 * theme::OSD_MARGIN).min(SEARCH_MAX_W);
            assert!(bar + 2.0 * theme::OSD_MARGIN <= w);
            // The face and the words beside the magnifier.
            assert!(bar - FACE - 44.0 >= 90.0, "no room for the words in {bar}");
        }
    }

    /// A finger needs 48 dp; the switch is a 48 pt row, 72 dp on Android.
    #[test]
    fn the_switch_is_a_touch_row() {
        assert!(SWITCH_H - 8.0 >= theme::MIN_TOUCH_TARGET_DP / 1.5);
        assert!(FACE >= theme::MIN_TOUCH_TARGET_DP / 1.5);
    }

    /// The two edges everything else anchors against cover what is drawn on
    /// them, and the columns start under the bar.
    #[test]
    fn the_reserved_edges_clear_the_bar_and_the_switch() {
        assert!(reserved_top() >= theme::OSD_MARGIN + SEARCH_H);
        assert!(reserved_bottom() >= theme::OSD_MARGIN + SWITCH_H);
        let up = Vec2::new(PHONES[0], PHONE_H);
        assert_eq!(left_slot(up, 0).y, reserved_top());
        assert_eq!(right_column(up, true, true).layers.y, reserved_top());
    }

    /// The right set is discs that never touch, standing or lying: a word or
    /// a caption under a disc gets its own room before the next disc.
    #[test]
    fn the_right_column_does_not_overlap_itself() {
        for view in [Vec2::new(PHONES[0], PHONE_H), LYING] {
            for (locate, camera) in [(false, false), (true, false), (false, true), (true, true)] {
                let c = right_column(view, locate, camera);
                let mut discs = vec![c.layers, c.mode];
                discs.extend(c.locate);
                discs.extend(c.camera);
                for pair in discs.windows(2) {
                    // Down a column, or along a row away from the bar.
                    let step = (pair[1] - pair[0]).abs().max_element();
                    assert!(
                        step >= theme::ROUND_BUTTON_SIZE + theme::OSD_GAP,
                        "{view:?} {discs:?}"
                    );
                }
                for d in &discs {
                    assert!(c.bottom >= d.y + theme::ROUND_BUTTON_SIZE);
                }
            }
            // The mode disc's word sits in the gap under it.
            let c = right_column(view, true, true);
            assert!(c.bottom - c.mode.y >= theme::ROUND_BUTTON_SIZE + MODE_WORD_H);
            if !c.lying {
                assert!(c.locate.unwrap().y - c.mode.y >= theme::ROUND_BUTTON_SIZE + MODE_WORD_H);
            }
        }
    }

    /// Both sets, top and bottom, fit a phone with the switch under them:
    /// the left one (compass, lens, light, Say) ends well above the
    /// thumbstick's corner, and the right one ends above the tallest thing
    /// the right edge grows from the bottom (`map_osd`).
    #[test]
    fn the_columns_fit_a_phone_above_the_switch() {
        for view in [Vec2::new(PHONES[0], PHONE_H), LYING] {
            let left_bottom = left_rect(view, 4).max.y;
            let right_bottom = right_column(view, true, true).bottom;
            for (bottom, what) in [(left_bottom, "left"), (right_bottom, "right")] {
                assert!(
                    bottom + theme::OSD_GROUP_GAP < view.y - reserved_bottom(),
                    "the {what} set reaches {bottom} of {view:?}"
                );
            }
        }
    }

    /// Lying, the two rows and the search bar share the bar's band without
    /// touching: left row, then the right row, then the bar.
    #[test]
    fn the_rows_and_the_bar_share_the_top_band() {
        let view = LYING;
        assert!(lying(view));
        let left = left_rect(view, 4);
        let right = right_column(view, true, true).rect(view);
        let bar = search_width(view, view.x);
        let bar_left = view.x - theme::OSD_MARGIN - bar;
        assert!(
            left.max.x + theme::OSD_GROUP_GAP <= right.min.x,
            "{left:?} {right:?}"
        );
        assert!(
            right.max.x + theme::OSD_GROUP_GAP <= bar_left + 0.01,
            "{right:?} {bar_left}"
        );
        // Room for the words beside the magnifier, as upright.
        assert!(bar - FACE - 44.0 >= 90.0, "no room for the words in {bar}");
        // The discs sit in the bar's own band.
        assert!(right.min.y >= theme::OSD_MARGIN && left.min.y >= theme::OSD_MARGIN);
        assert!(left.max.y <= theme::OSD_MARGIN + SEARCH_H);
        // Too narrow for both rows beside a bar (the gate's phone smoke,
        // 640×360 px at 1.5), the columns stand, and the bar keeps its words.
        for view in [Vec2::new(427.0, 240.0), Vec2::new(597.0, 274.0)] {
            assert!(!lying(view), "{view:?}");
            assert!(search_width(view, view.x) >= SEARCH_MIN_W, "{view:?}");
        }
    }
}
