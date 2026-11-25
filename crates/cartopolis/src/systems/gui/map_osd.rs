//! On-screen navigation controls — the verbs a finger has no key for.
//!
//! **Why this exists.** Every camera in this client was reachable from a mouse
//! and a keyboard, and the touch bridges (`systems::touch`) gave the finger the
//! mouse's verbs but not the keyboard's. That is enough for Earth mode, whose
//! whole vocabulary is drag / pinch / twist and which every phone map has
//! already taught the user — and it is not enough for Fly, where translation was
//! WASD + E/Q, i.e. six keys a touchscreen does not have.
//!
//! What Fly had instead was a *gesture overload*: pinch flew forward and back, a
//! two-finger drag strafed and climbed, and only the **sign** of each was read,
//! so the smallest deliberate spread was a full-speed press of W held for as
//! long as the fingers stayed down. It flew, in the sense that the camera moved;
//! it was not steerable, and it collided with the one gesture whose meaning is
//! settled across every map application there is (pinch = zoom). Those mappings
//! are gone — see [`OsdInput`]'s consumers in `free_camera::camera_controls` —
//! and this module is what replaced them.
//!
//! ## The shape, and where it comes from
//!
//! Left thumb translates, right thumb changes height: the layout every
//! touchscreen 3D application converged on, for the mechanical reason that those
//! are where the thumbs already are when a phone is held in two hands.
//!
//! Both edges are anchored to the **bottom**, and grow upwards from the dock
//! line: the stick opposite a column of round controls, at the two places a
//! phone held in two hands already has a thumb. Nothing is anchored to the
//! middle of an edge — see [`vertical_pair`] for the version that was, and what
//! it cost.
//!
//!   - **Fly** — a thumbstick bottom-left (forward/back + strafe, *proportional*
//!     so a small push is a slow drift) and a climb/descend pair bottom-right.
//!   - **Follow** — the same thumbstick, walking the *avatar* rather than the
//!     camera; the bottom-right pair zooming the camera; and a jump disc
//!     stacked above it. Until this existed the avatar was
//!     the one thing on a phone that could not be moved at all: its whole
//!     vocabulary was WASD, which is six keys a touchscreen does not have and
//!     no gesture ever stood in for (`docs/notes/android-build.md` recorded it
//!     as a standing gap). The push is the throttle — see
//!     `locomotion::speed_for_throttle` — because there is no Shift or Alt to
//!     hold, so the gait ladder the keyboard climbs in rungs is a slide here.
//!   - **Earth** — a zoom pair on the right. The pinch already zooms it; the
//!     buttons exist for the one-handed case, which is most of the time a phone
//!     is held at all.
//!
//! The camera-mode switcher is *not* here: it is the mode disc in the map's
//! right-hand column (`dock::mode_disc`). It sat in this corner briefly, and what that cost was
//! the corner itself — every pair on this edge stacked one control higher for
//! a disc that is chrome, not a touch control.
//!
//! ## Two rules it obeys, both learned elsewhere in this tree
//!
//! **It draws into floating `Area`s, never the shared root `Ui`**, so it stays
//! out of the panel chain (like the dock and the HUD) — but for the
//! same reason egui paints those areas *over* a panel, it hides itself whenever
//! a right-hand panel is open rather than sitting on top of one, which is what
//! `hud::render_map_controls` does and for the same reason: an invisible control
//! that still eats taps is worse than no control.
//!
//! **It reports a wish, it does not move the camera.** `camera_controls` owns
//! the camera and already blends keys, mouse and gestures into one velocity with
//! one accelerate/glide curve; a second writer would fight it (and would have to
//! duplicate the altitude-scaled speed, the ground clamp and the big_space
//! re-anchoring). So this system writes [`OsdInput`] and stops.
//!
//! ## It reads fingers, not egui's pointer
//!
//! egui has exactly one pointer and the first finger down owns it, so an OSD
//! built out of ordinary egui responses is a **one-finger** OSD: hold the stick
//! and nothing else on the screen — the camera included — can be touched, and
//! land a finger on the map first and the stick is dead until you lift it. Both
//! halves of that were real, and the first is why this changed: pushing the stick
//! to walk is the one control you hold down continuously, and holding it froze
//! the view.
//!
//! So every control here is driven from the raw fingers `systems::touch` claims
//! for it ([`crate::systems::touch::UiClaimRegions`], published at the bottom of
//! [`render_map_osd`] out of the rects the layout actually produced), with the
//! egui response left in as the `or` arm — that is the mouse's path, and the only
//! one a desktop run has. A rate reads [`TouchGestures::ui_held`], an edge reads
//! [`TouchGestures::ui_released`]; both are already hit-testable rects, so it
//! costs a `contains` per control and no state anywhere.
//!
//! ## The library's edition
//!
//! With [`OsdLib::lib`] on (`systems::lib_map_osd`, the default) this system
//! makes no egui widget at all. It lays the controls out itself, by the
//! arithmetic egui's anchors did (`lib_pair`, `lib_stick`, … `lib_rail`;
//! `the_library_edition_lays_out_where_egui_did` holds the two together),
//! works them from Bevy's input ([`OsdHands`]) and publishes in
//! [`OsdLib::view`] what the library draws, rect for rect. Fingers come as
//! above. The mouse, which egui's widgets used to take, is taken by
//! `touch::claim_mouse` instead: a press landing on a control
//! ([`UiClaimRegions::pointer`]) is removed from `ButtonInput<MouseButton>`,
//! so picking, the gizmo and the camera's drag never see it, and handed back
//! here as one more claimed finger. A long press on the hand disc, egui's
//! `long_touched`, is timed here ([`LongPress`]); the rail's fades are
//! [`fade`], not egui's animations.
//!
//! [`TouchGestures::ui_held`]: crate::systems::touch::TouchGestures::ui_held
//! [`UiClaimRegions::pointer`]: crate::systems::touch::UiClaimRegions::pointer
//! [`TouchGestures::ui_released`]: crate::systems::touch::TouchGestures::ui_released

use crate::systems::egui_manager::EguiRef;
use bevy::prelude::*;

use crate::resources::{CameraMode, CameraState, RightPanel};
use crate::systems::theme;

/// Diameter (points) of one round control — [`theme::ROUND_BUTTON_SIZE`], the
/// one disc that floats over this map. 60 dp on the touch tier, where
/// `EguiManager::ui_scale` is ≥1.5 (`docs/notes/touch-input.md`), so it clears
/// the 48 dp guideline with room to spare; it was 44 for the same reason and all
/// that bought was a fourth diameter on a screen that only needed one.
const CONTROL_SIZE: f32 = theme::ROUND_BUTTON_SIZE;
/// Radius (points) of the thumbstick's well, and of the knob inside it.
pub(crate) const STICK_RADIUS: f32 = 54.0;
pub(crate) const STICK_KNOB_RADIUS: f32 = 22.0;

/// Distance from the bottom of the window to the bottom of a cluster: clear of
/// the dock, which owns the bottom edge, plus the standard edge inset.
fn bottom_offset() -> f32 {
    theme::OSD_MARGIN + crate::systems::dock::reserved_bottom()
}

/// Height of a two-button stack.
fn pair_height() -> f32 {
    CONTROL_SIZE * 2.0 + theme::OSD_GAP
}

/// Bottom offset of the jump disc: a group gap above the zoom/climb pair,
/// which owns the corner. Named rather than inlined so the test below measures
/// the same arithmetic the anchor uses.
fn actions_bottom_offset() -> f32 {
    bottom_offset() + pair_height() + theme::OSD_GROUP_GAP
}

/// Where the right edge's controls past the zoom/climb pair stand, as
/// `RIGHT_BOTTOM` anchor offsets: `x` in from the right edge, `y` up from the
/// bottom.
#[derive(Debug, Clone, Copy)]
struct Spots {
    /// The jump disc.
    actions: egui::Vec2,
    /// The face disc, one row above the jump disc and one column in.
    face: egui::Vec2,
    /// The hand button, under the face, beside the jump disc.
    hand: egui::Vec2,
}

/// Standing, the jump disc stands over the zoom/climb pair at the edge, and
/// the face and hand discs beside it, one column in (they stood
/// in tiers above the pair until 2026-11-11, which took the right edge up to
/// within 8 pt of the map's column of round buttons on a phone). Lying
/// (`dock::lying`, a phone on its side) the three pairs stand side by side
/// along the bottom, under the thumb, since there is no height for a stack.
/// One column is a disc and a group gap, not the plain one, because the miss
/// bands of two neighbours side by side must not meet (`reach`).
fn spots(lying: bool) -> Spots {
    let column = CONTROL_SIZE + theme::OSD_GROUP_GAP;
    let actions = if lying {
        egui::vec2(theme::OSD_MARGIN + column, bottom_offset())
    } else {
        egui::vec2(theme::OSD_MARGIN, actions_bottom_offset())
    };
    let beside = actions.x + column;
    Spots {
        actions,
        face: egui::vec2(beside, actions.y + CONTROL_SIZE + theme::OSD_GAP),
        hand: egui::vec2(beside, actions.y),
    }
}

/// How far up from the bottom the right edge's touch controls reach, at
/// most: the top of Follow's face disc, a pair's height above the jump disc's
/// foot. The map's column of round
/// buttons comes down from the top to meet it (`dock::right_column`), and the
/// two must not meet — which is what decides `dock::lying`.
#[must_use]
pub fn right_edge_reach(lying: bool) -> f32 {
    spots(lying).actions.y + pair_height()
}

/// How much of the bottom-**left** corner the thumbstick claims, in points, for
/// whatever else wants to sit above it — the scale bar
/// (`hud::render_map_controls`) is the only caller so far.
///
/// Measured from the window's bottom edge, so it already contains the dock's
/// own reservation and a caller adds nothing but its own gap. `0` means the
/// corner is the dock's alone, which is the desktop case and every touch case
/// where the controls are suppressed.
#[must_use]
pub fn bottom_left_reserved(cluster: Option<Cluster>) -> f32 {
    match cluster {
        Some(Cluster::Walk) => bottom_offset() + STICK_RADIUS * 2.0,
        // Earth mode has no stick, and neither has a run with no controls at all.
        Some(Cluster::Zoom) | None => crate::systems::dock::reserved_bottom(),
    }
}

/// Fraction of the stick's radius that reads as "still centred". A thumb resting
/// in the well wanders a few points, and without this the camera drifts for as
/// long as it rests there.
const STICK_DEADZONE: f32 = 0.14;

/// How far (egui points) a landing may miss a control and still work it.
///
/// A thumb pad is wider than a 40-pt disc, and a grab aimed at one lands a few
/// points off it often. Missed, that finger used to stay a *map* finger: nothing
/// claimed it, so it fed the gesture recogniser — one near-miss turned the view
/// under the thumb that "was on the stick", and two thumbs missing at once
/// (stick + climb is held exactly that way) made a **pinch**, i.e. the camera
/// zooming away in the middle of flying it. Between the two discs of a pair the
/// margin is not applied (see [`reach`]) — the gap stays dead rather than firing
/// both verbs at once.
const MISS_MARGIN: f32 = 8.0;

/// Padding (egui points) on the *published* claim regions — wider than
/// [`MISS_MARGIN`], because the two answer different questions. A finger inside
/// this band is taken away from the camera whether or not a control answers it:
/// a dead finger is a nuisance, a camera acting on a grab you meant for the
/// stick is worse. Bounded by [`theme::OSD_MARGIN`], the gap to the dock — a
/// claimed finger is invisible to the emulated mouse, so a band reaching the
/// dock would cost a second finger its tap there.
const CLAIM_PAD: f32 = 12.0;

/// A control's hit rect grown by [`MISS_MARGIN`] on its outer sides. The sides
/// facing a sibling disc stay exact — pad both discs of a pair and the gap
/// between them presses (or worse, *clicks*) both at once — two verbs from one
/// lift.
fn reach(rect: egui::Rect, grow_top: bool, grow_bottom: bool) -> egui::Rect {
    let top = if grow_top { MISS_MARGIN } else { 0.0 };
    let bottom = if grow_bottom { MISS_MARGIN } else { 0.0 };
    egui::Rect::from_min_max(
        egui::pos2(rect.min.x - MISS_MARGIN, rect.min.y - top),
        egui::pos2(rect.max.x + MISS_MARGIN, rect.max.y + bottom),
    )
}

/// What the on-screen controls asked for, this frame.
///
/// Rewritten in full every frame by [`render_map_osd`] — there is no
/// accumulation, so a consumer that misses a frame misses that frame's push
/// rather than receiving it late, exactly as `TouchGestures` behaves.
#[derive(Resource, Default, Debug, Clone, Copy, PartialEq)]
pub struct OsdInput {
    /// Zoom wish: `+1` while the "closer" button is held, `-1` while "further"
    /// is. A rate, not a step — the consumer scales it by the frame time, so a
    /// held button zooms smoothly instead of by notches.
    pub zoom: f32,
    /// Follow-mode edge: a jump this frame.
    ///
    /// **An edge, not a held state**, and the distinction is the same one the
    /// wire makes (`ClientIntent::jump`, protocol 13): a dropped movement frame is
    /// corrected by the next one, while a dropped jump simply did not happen. So
    /// this is true for exactly the frame the button was released on, and the
    /// consumer must act on it rather than sample it.
    pub jump: bool,
    /// Follow-mode travel wish in the camera's horizontal frame: `x` = strafe
    /// right, `y` = forward, inside the unit disc and *proportional* — the
    /// difference between a stick and four keys. Read by
    /// `avatar::handle_avatar_movement` (which then owns it in every run mode —
    /// the online path consumes the same ramp through
    /// `network::send_network_intent`), and at the wheel by
    /// `driving::drive` as the accelerator, the brake and the steering.
    ///
    /// There was a second stick, `fly`, for the Fly camera, until the camera
    /// stopped flying (2026-11-25): the car is how the avatar goes fast now.
    pub walk: Vec2,
    /// A height the zoom rail asked for (metres above the ground): a drag along
    /// it, or a tap on one of its rungs. Earth mode glides there and holds the
    /// goal until it arrives or another zoom takes over
    /// (`CameraState::rail_goal`).
    pub rail_altitude: Option<f64>,
}

/// The library's edition of these controls (`systems::lib_map_osd`). Read
/// as optional by [`render_map_osd`]; `drive_lib_map_osd` inserts it when
/// nothing registered it.
#[derive(Resource, Default)]
pub struct OsdLib {
    /// The library draws the controls, and egui makes none of them.
    pub lib: bool,
    /// What the library draws this frame, in egui points. Assigned only
    /// when it differs, so a resting OSD sends the library nothing.
    pub view: OsdView,
    /// The hand disc's menu is open (right-click or long press on it):
    /// [`render_map_osd`] opens it, the library's popover closes it
    /// (`lib_map_osd`). egui's own popup keeps its state in egui.
    pub hand_menu: bool,
}

/// Everything the controls show this frame, laid out as egui's anchors did.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct OsdView {
    /// The screen (`Context::content_rect`), for what stands right-aligned.
    pub screen: egui::Vec2,
    pub stick: Option<StickView>,
    /// The round controls: the climb or zoom pair, jump, face, hand.
    pub discs: Vec<DiscView>,
    /// The hand tracking's word, left of the hand disc.
    pub hand_word: Option<HandWordView>,
    /// The hand disc's menu, while it is open.
    pub hand_menu: Option<HandMenuView>,
    /// The expressions, opened from the face disc.
    pub faces: Option<FacesView>,
    pub rail: Option<RailFrame>,
    /// What moves on the rail every frame it moves, kept apart from
    /// [`RailFrame`] so a zoom restyles a thumb rather than a rail.
    pub rail_live: RailLive,
}

/// The thumbstick: the well's centre and the knob's, and whether a push is
/// on it (the knob's engaged colours).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StickView {
    pub centre: egui::Pos2,
    pub knob: egui::Pos2,
    pub engaged: bool,
}

/// A disc's ring when engaged ([`theme::map_disc`]'s `tone`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    /// `accent_ring`, every disc but the hand's.
    Ring,
    /// The hand following (`accent_bright`).
    Bright,
    /// The hand's camera busy (`warn`).
    Warn,
}

/// A disc as [`theme::map_disc`] draws it: where, and which of its three
/// plates (engaged, hovered, at rest).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DiscState {
    pub rect: egui::Rect,
    pub on: bool,
    pub hovered: bool,
}

/// One round control and the mark on it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DiscView {
    pub disc: DiscState,
    pub mark: Mark,
    pub tone: Tone,
}

/// A word right-aligned at `right`, centred on `y`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HandWordView {
    pub right: f32,
    pub y: f32,
    pub text: &'static str,
    pub tone: Tone,
}

/// The hand disc's menu (egui's context menu on it): the disc it hangs
/// from, which hand, whether Start again can be pressed, and what is wrong.
#[derive(Clone, Debug, PartialEq)]
pub struct HandMenuView {
    /// The hand disc, in egui points.
    pub disc: egui::Rect,
    pub left: bool,
    pub can_restart: bool,
    pub problem: Option<String>,
}

/// The expressions' frame and its rows.
#[derive(Clone, Debug, PartialEq)]
pub struct FacesView {
    pub frame: egui::Rect,
    pub rows: Vec<(egui::Rect, &'static str)>,
}

/// The zoom rail's parts, as laid out.
#[derive(Clone, Debug, PartialEq)]
pub struct RailFrame {
    /// The whole column: the scale disc, −, the track, +.
    pub column: egui::Rect,
    pub scale: crate::systems::zoom_rail::Scale,
    pub scale_disc: DiscState,
    /// The − (out, `[0]`) and the + (in, `[1]`), each with the scale it
    /// offers at the scale's end.
    pub ends: [(DiscState, Option<crate::systems::zoom_rail::Scale>); 2],
    pub track: egui::Rect,
    /// How many steps the track spans (a tick between each two).
    pub steps: usize,
    /// The scales' list, when open.
    pub list: Option<Vec<ListRow>>,
}

/// One row of the scales' list.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ListRow {
    pub rect: egui::Rect,
    pub scale: crate::systems::zoom_rail::Scale,
    pub on: bool,
    pub hovered: bool,
}

/// The rail's moving parts: how awake it is, the thumb, where a jump flies
/// to, the step's name and how far it is faded in, and how far each end's
/// push has filled its ring.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RailLive {
    pub opacity: f32,
    pub thumb_y: f32,
    pub ring_y: Option<f32>,
    pub name: Option<(&'static str, f32)>,
    pub push: [f32; 2],
}

/// Half a point: finer than the eye, coarse enough that a resting control
/// does not hand the library a new view every frame.
fn q(v: f32) -> f32 {
    (v * 2.0).round() / 2.0
}

fn q_pos(p: egui::Pos2) -> egui::Pos2 {
    egui::pos2(q(p.x), q(p.y))
}

/// A fade or a fill, in fiftieths.
fn q_unit(v: f32) -> f32 {
    (v * 50.0).round() / 50.0
}

/// The id the mouse's owned press goes by among the fingers
/// ([`Fingers::with_mouse`]): no touch id is this large.
const MOUSE: u64 = u64::MAX;

/// How far (egui points) the mouse may move between press and release and
/// still click — egui's own `max_click_dist`, which the zoom track's click
/// (to the step) and drag (to the height) were told apart by.
const CLICK_SLOP: f32 = 6.0;

/// The fingers the on-screen controls own this frame, in **egui** points.
///
/// A projection of `TouchGestures`' claimed sets through `EguiManager::ui_scale`,
/// made once per frame so no control has to know that the touchscreen and egui
/// measure in different points.
#[derive(Default)]
struct Fingers {
    held: Vec<(u64, egui::Pos2)>,
    released: Vec<(u64, egui::Pos2)>,
    /// Fingers down that no control claimed — the map's: a pan or a pinch.
    free: usize,
    /// Where the mouse's owned press landed, while it is held or lifting
    /// ([`MOUSE`]); the library's edition only.
    mouse_from: Option<egui::Pos2>,
}

impl Fingers {
    fn new(touch: &crate::systems::touch::TouchGestures, ui_scale: f32) -> Self {
        let to_ui = move |p: Vec2| egui::pos2(p.x / ui_scale, p.y / ui_scale);
        Self {
            held: touch
                .ui_held()
                .iter()
                .map(|(id, p)| (*id, to_ui(*p)))
                .collect(),
            released: touch
                .ui_released()
                .iter()
                .map(|(id, p)| (*id, to_ui(*p)))
                .collect(),
            free: touch.fingers,
            mouse_from: None,
        }
    }

    /// …and the mouse's press a control owns, as one more finger
    /// ([`MOUSE`]): the library's edition, which has no egui widget to take
    /// the mouse (`touch::claim_mouse`).
    fn with_mouse(touch: &crate::systems::touch::TouchGestures, ui_scale: f32) -> Self {
        let to_ui = move |p: Vec2| egui::pos2(p.x / ui_scale, p.y / ui_scale);
        let mut fingers = Self::new(touch, ui_scale);
        if let Some(at) = touch.ui_mouse() {
            fingers.held.push((MOUSE, to_ui(at)));
        }
        if let Some(at) = touch.ui_mouse_released() {
            fingers.released.push((MOUSE, to_ui(at)));
        }
        fingers.mouse_from = touch.ui_mouse_from().map(to_ui);
        fingers
    }

    /// Did a press lift inside this rect this frame, and is that a tap on
    /// it? A finger's lift is (it was claimed by landing on a control); the
    /// mouse's is a click only when its press landed inside too, as egui's
    /// `clicked`.
    fn tapped(&self, hit: egui::Rect) -> bool {
        self.released.iter().any(|(id, p)| {
            hit.contains(*p) && (*id != MOUSE || self.mouse_from.is_some_and(|f| hit.contains(f)))
        })
    }

    /// Is a claimed finger resting in this rect? The held test, for a rate.
    fn pressing(&self, rect: egui::Rect) -> bool {
        self.held.iter().any(|(_, p)| rect.contains(*p))
    }

    /// Did a claimed finger lift inside this rect this frame? The edge test —
    /// the finger equivalent of `Response::clicked`.
    fn lifted_in(&self, rect: egui::Rect) -> bool {
        self.released.iter().any(|(_, p)| rect.contains(*p))
    }

    fn at(&self, id: u64) -> Option<egui::Pos2> {
        self.held.iter().find(|(f, _)| *f == id).map(|(_, p)| *p)
    }

    /// The first claimed finger inside this rect, if any.
    fn inside(&self, rect: egui::Rect) -> Option<u64> {
        self.held
            .iter()
            .find(|(_, p)| rect.contains(*p))
            .map(|(id, _)| *id)
    }
}

/// Which cluster the current camera wants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cluster {
    /// Follow: the thumbstick (walking the avatar) and the zoom pair.
    Walk,
    /// Earth: the zoom pair. It already pinch-zooms; this is the one-handed path
    /// to the same thing.
    Zoom,
}

/// How much of the **bottom-left** corner the touch controls claim, in points —
/// what anything else anchored there has to sit above.
///
/// Exported for the same reason [`crate::systems::dock::reserved_bottom`] is:
/// the chat transcript wants this corner, the thumbstick is already in it, and a
/// second copy of the arithmetic is how the two would drift. The zoom cluster
/// claims nothing here — it is a pair on the right edge and the corner is free.
#[must_use]
pub fn reserved_left_bottom(cluster: Option<Cluster>) -> f32 {
    match cluster {
        Some(Cluster::Walk) => bottom_offset() + STICK_RADIUS * 2.0 + theme::OSD_GROUP_GAP,
        Some(Cluster::Zoom) | None => crate::systems::dock::reserved_bottom() + theme::OSD_MARGIN,
    }
}

/// Whether to draw anything, and what — the whole policy, as a pure function so
/// it can be tested without a window.
///
/// `touch_ui` is the caller's answer to "is a finger driving this run", which is
/// deliberately *not* re-derived here: `crate::quality` is the one detector in
/// this client (see its module note) and a second one is how the UI ends up
/// finger-sized on a desktop.
#[must_use]
pub fn cluster_for(
    mode: CameraMode,
    driving: bool,
    panel_open: bool,
    keyboard_open: bool,
    touch_ui: bool,
) -> Option<Cluster> {
    // Driver mode owns the whole screen and holds the camera itself — its own
    // chrome is the manoeuvre card and the rail, and a zoom button there would
    // be a control that visibly does nothing. The keyboard is the dock's rule
    // verbatim: it docks to the bottom edge, an `Area` drawn later paints over
    // an earlier one, and a stick left under the keys would take taps meant for
    // them.
    if !touch_ui || driving || panel_open || keyboard_open {
        return None;
    }
    // The placed view (`CameraMode::Free`) is looked around in with a finger
    // and moves nowhere, so it has no controls of its own.
    match mode {
        CameraMode::Free => None,
        CameraMode::Avatar => Some(Cluster::Walk),
        CameraMode::Orbit => Some(Cluster::Zoom),
    }
}

/// The stick's reading, given where the thumb is relative to the well's centre.
///
/// Screen y points down and forward is up-screen, so `y` is negated on the way
/// out. The magnitude is clamped to the well rather than the square that
/// contains it: a stick that is faster on the diagonal is a stick that steers
/// differently depending on which way you are already going.
#[must_use]
pub fn stick_wish(offset_pts: Vec2, radius: f32) -> Vec2 {
    let radius = radius.max(1.0);
    let mut wish = offset_pts / radius;
    let len = wish.length();
    if len < STICK_DEADZONE {
        return Vec2::ZERO;
    }
    if len > 1.0 {
        wish /= len;
    }
    Vec2::new(wish.x, -wish.y)
}

/// Draw the controls and record what they asked for.
///
/// Ordered before `free_camera::camera_controls` in `gui::plugin`, so a push is acted
/// on in the frame it was made rather than the next one — at 20 fps on a phone
/// one frame of input lag on a *held* control is visible as the camera lagging
/// the thumb.
///
/// [`OsdInput::walk`] is the exception, and structurally so: it is read by
/// `avatar::handle_avatar_movement`, which this system transitively runs *after*
/// (`drive_driver_mode` measures the drive after the avatar has moved, and this
/// runs after that, because whether driver mode owns the screen decides whether
/// anything is drawn at all). So the walk wish reaches the avatar on the
/// following frame. That is a constant ~50 ms on a phone, not jitter, and it is
/// the cheaper half of the trade: inverting either link would move the camera or
/// the driver tick to answer a translation control that cannot feel it.
#[allow(clippy::too_many_arguments)]
pub fn render_map_osd(
    egui: EguiRef,
    mut osd: ResMut<OsdInput>,
    mut regions: ResMut<crate::systems::touch::UiClaimRegions>,
    mut stick: Local<(Option<u64>, Option<egui::Pos2>)>,
    camera_state: Res<CameraState>,
    driver: Res<crate::systems::driver::DriverState>,
    right_panel: Res<RightPanel>,
    keyboard: Res<crate::systems::onscreen_keyboard::OnScreenKeyboard>,
    hands: OsdHands,
    #[cfg(feature = "comms")] camera: Res<crate::systems::camera_screen::CameraScreen>,
    mut body: BodyControls,
    mut rail: RailView,
    chrome: Res<crate::systems::dock::MapChrome>,
    #[cfg(feature = "comms")] sheet: Res<crate::systems::chat_overlay::ChatOverlay>,
    room: Res<crate::systems::design_room::DesignRoom>,
    mut lib: Option<ResMut<OsdLib>>,
) {
    let touch = &*hands.touch;
    let lib_on = lib.as_ref().is_some_and(|l| l.lib);
    // The hand's first-use card is the library's there, over everything;
    // the controls step aside under it as under a panel, or a tap on the
    // card would also press the control beneath it.
    let card_up = lib_on && body.card.0;
    // Rebuilt from nothing every frame, including the frames that draw nothing —
    // otherwise switching to Earth mode mid-push would leave the last wish
    // latched and the camera would fly on with nothing on screen to stop it.
    // `set_if_neq` at the end so a resting control does not mark the resource
    // changed sixty times a second.
    let mut next = OsdInput::default();
    // The app's camera and chat sheet (`comms`) cover the controls; the web
    // build has neither.
    #[cfg(feature = "comms")]
    let (camera_open, composing) = (camera.is_open(), sheet.composing);
    #[cfg(not(feature = "comms"))]
    let (camera_open, composing) = (false, false);
    // The same applies to the claimed regions, and more sharply: a rect left
    // published over a control that is no longer drawn is a piece of the map
    // that silently swallows fingers.
    let mut areas: Vec<egui::Rect> = Vec::new();
    let ui_scale = egui.ui_scale();
    let touch_ui = theme::touch_layout() || touch.ever();

    let cluster = cluster_for(
        camera_state.mode,
        driver.active,
        // The camera is the whole screen, exactly as a panel is. A chat's
        // World view is not: it keeps the touch controls.
        !right_panel.is(RightPanel::None) || camera_open || room.spot.is_some() || card_up,
        // The chat sheet on a phone takes the bottom of the screen as the
        // keys do, and the controls there would draw under it.
        keyboard.is_open() || (composing && egui.compact()),
        touch_ui,
    );
    if cluster.is_none() {
        *stick = (None, None);
    }

    // The zoom rail (`systems::zoom_rail`): Earth mode, wherever nothing covers
    // the map's right edge — the same rules as the clusters', less the touch
    // screen, since a desktop wants it too.
    let rail_on = camera_state.mode == CameraMode::Orbit
        && !driver.active
        && right_panel.is(RightPanel::None)
        && !camera_open
        && room.spot.is_none()
        && !keyboard.is_open()
        && !card_up;
    let rail_in = rail
        .altitude()
        .filter(|_| rail_on)
        .map(|a| a.max(camera_state.cosmos_alt))
        .map(|alt| RailIn {
            alt,
            // While a jump flies the rail shows the scale it passes, so the
            // way through every scale between is seen.
            scale: if camera_state.rail_goal.is_some() {
                crate::systems::zoom_rail::Scale::of(alt)
            } else {
                camera_state.zoom_scale
            },
            flying_to: camera_state.rail_goal,
            push: camera_state.edge_push_shown,
            globe_return: camera_state.globe_return_alt,
            top: chrome.right_bottom + theme::OSD_GROUP_GAP,
        });

    if let Some(lib) = lib.as_deref_mut().filter(|l| l.lib) {
        // The library's edition: laid out here, by the arithmetic egui's
        // anchors did, and worked by Bevy's own input — no egui widget.
        let screen = egui.viewport_pts();
        let input = hands.read(ui_scale);
        let mut view = OsdView {
            screen,
            ..Default::default()
        };
        if let Some(cluster) = cluster {
            let at = spots(crate::systems::dock::lying(screen));
            let s = &mut *stick;
            match cluster {
                Cluster::Walk => {
                    next.zoom = lib_pair(
                        screen,
                        &input,
                        &mut areas,
                        Mark::Plus,
                        Mark::Minus,
                        &mut view,
                    );
                    next.walk =
                        lib_stick(screen, &input, &mut s.0, &mut s.1, &mut areas, &mut view);
                    next.jump = lib_jump(screen, &input, &mut areas, at, &mut view);
                    if let Some(e) = lib_faces(
                        screen,
                        &input,
                        &mut areas,
                        &mut body.faces_open,
                        at,
                        &mut view,
                    ) {
                        body.faces
                            .write(crate::systems::avatar_face::StartExpression(e));
                    }
                    lib_hand(
                        screen,
                        &input,
                        &mut areas,
                        &mut body,
                        at,
                        &mut view,
                        &mut lib.hand_menu,
                    );
                }
                Cluster::Zoom => {}
            }
        }
        if let Some(r) = rail_in {
            let (zoom, goal) = lib_rail(
                screen,
                &input,
                &mut areas,
                &r,
                &mut rail.state,
                touch_ui,
                touch.active(),
                &mut view,
            );
            next.zoom = zoom;
            next.rail_altitude = goal;
        }
        if lib.view != view {
            lib.view = view;
        }
        regions.set_if_neq(crate::systems::touch::UiClaimRegions {
            rects: areas
                .iter()
                .map(|r| to_window(r.expand(CLAIM_PAD), ui_scale))
                .collect(),
            // No egui widget stands under these to take the mouse: the
            // press is taken out of the world's input and handed back here.
            pointer: areas.iter().map(|r| to_window(*r, ui_scale)).collect(),
        });
        osd.set_if_neq(next);
        return;
    }

    if let Some(cluster) = cluster {
        let fingers = Fingers::new(touch, ui_scale);
        let ctx = egui.ctx();
        let at = spots(crate::systems::dock::lying(egui.viewport_pts()));
        match cluster {
            Cluster::Walk => {
                // The avatar has no vertical of its own (it walks), so the
                // right-edge pair zooms the camera.
                next.zoom = vertical_pair(
                    ctx,
                    &fingers,
                    &mut areas,
                    "osd_zoom",
                    Mark::Plus,
                    Mark::Minus,
                );
                next.walk = {
                    let s = &mut *stick;
                    thumbstick(ctx, &fingers, &mut s.0, &mut s.1, &mut areas)
                };
                next.jump = jump_button(ctx, &fingers, &mut areas, at);
                if let Some(e) = face_picker(ctx, &fingers, &mut areas, &mut body.faces_open, at) {
                    body.faces
                        .write(crate::systems::avatar_face::StartExpression(e));
                }
                hand_button(ctx, &fingers, &mut areas, &mut body, at);
            }
            // The zoom rail below is Earth mode's control, on every device.
            Cluster::Zoom => {}
        }
    }

    if let Some(r) = rail_in {
        let fingers = Fingers::new(touch, ui_scale);
        let (zoom, goal) = zoom_rail_control(
            egui.ctx(),
            &fingers,
            &mut areas,
            &r,
            &mut rail.state,
            touch_ui,
            touch.active(),
        );
        next.zoom = zoom;
        next.rail_altitude = goal;
    }

    // Published in the window's points, which is what the touchscreen
    // reports; egui laid them out in its own (see `EguiManager::ui_scale`).
    // Padded by the miss band, so a grab that lands beside a control is
    // claimed rather than handed to the camera as a gesture.
    regions.set_if_neq(crate::systems::touch::UiClaimRegions {
        rects: areas
            .iter()
            .map(|r| to_window(r.expand(CLAIM_PAD), ui_scale))
            .collect(),
        // egui's widgets take the mouse themselves.
        pointer: Vec::new(),
    });
    osd.set_if_neq(next);
}

/// What the zoom rail reads: the camera's height, and how open it is.
#[derive(bevy::ecs::system::SystemParam)]
pub struct RailView<'w, 's> {
    globe: Res<'w, crate::systems::globe::GlobeState>,
    camera: Query<
        'w,
        's,
        (&'static big_space::prelude::GridCell, &'static Transform),
        With<crate::components::FreeCamera>,
    >,
    state: Local<'s, RailState>,
}

impl RailView<'_, '_> {
    /// The camera's height above the ground, metres, once the globe can say.
    fn altitude(&self) -> Option<f64> {
        let (cell, tf) = self.camera.single().ok()?;
        self.globe.ready.then(|| self.globe.altitude_m(cell, tf))
    }
}

/// The rail's own state between frames.
#[derive(Default)]
pub struct RailState {
    /// The finger working the track, if one landed on it.
    finger: Option<RailFinger>,
    /// The claimed fingers down last frame, to tell a landing from a finger
    /// that slid in from the map.
    seen: Vec<u64>,
    /// The last time anything happened on the rail or the step changed.
    active_at: f64,
    /// The scale and step the camera was on last frame.
    last_step: (crate::systems::zoom_rail::Scale, usize),
    /// How far the step's name was faded in last frame, 0 to 1.
    shown: f32,
    /// Whether the scale list is open, and where it and its disc were drawn.
    list_open: bool,
    list_rect: Option<egui::Rect>,
    /// When each end (out, in) began to offer the next scale.
    edge_since: [Option<f64>; 2],
    /// When each claimed finger down landed, and the mouse was last pressed:
    /// a tap on an end counts only if it began after the end turned.
    landed_at: Vec<(u64, f64)>,
    mouse_pressed_at: f64,
    /// The library's edition fades without egui's animations: how far the
    /// rail's waking and the name's fade-in have run (0 to 1, before the
    /// easing), and the clock they last ran at.
    awake_run: f32,
    shown_run: f32,
    faded_at: f64,
}

/// A finger that landed on the track. It does nothing until it has stayed
/// [`RAIL_GRAB_SECS`] or moved [`RAIL_GRAB_SLOP`] alone on the glass: the first
/// finger of a pinch lands a few hundredths of a second before the second, and
/// it used to set the height before the pinch had begun.
struct RailFinger {
    id: u64,
    since: f64,
    from: egui::Pos2,
    live: bool,
}

/// Track length of the zoom rail, points: shorter where a phone's height is.
/// It spans one scale, four to eight steps, since 2026-11-07 (it was 170 and
/// 140 for all sixteen; asked "the slider may be a bit shorter").
const RAIL_TRACK: f32 = 140.0;
const RAIL_TRACK_TOUCH: f32 = 120.0;
/// The rail's discs: smaller than the map's other discs, since the rail is a
/// quiet control (`docs/mockups/zoom-ladder/`, v2 — "the zoom knob is massive").
const RAIL_DISC: f32 = 30.0;
/// How long a finger rests, or how far it moves, before the track takes it.
const RAIL_GRAB_SECS: f64 = 0.12;
const RAIL_GRAB_SLOP: f32 = 4.0;
/// How long the rail stays at full strength, with the rung's name beside the
/// thumb, after it was last used or the rung last changed, seconds; and how
/// faint the rail is after that.
const RAIL_AWAKE_SECS: f64 = 2.0;
const RAIL_REST_OPACITY: f32 = 0.55;
/// The gap between the rail's parts, and between the rows of its list.
const RAIL_SPACING: f32 = 6.0;

/// What the zoom rail is drawn for this frame.
struct RailIn {
    /// The camera's height, metres.
    alt: f64,
    /// The scale the rail spans: the one the zoom is held in, or while a jump
    /// flies, the one it is passing.
    scale: crate::systems::zoom_rail::Scale,
    /// Where a jump is flying to, if one is.
    flying_to: Option<f64>,
    /// A zoom pushing against the scale's end (`CameraState::edge_push_shown`).
    push: f32,
    /// Where a jump back to the globe lands (`CameraState::globe_return_alt`).
    globe_return: Option<f64>,
    /// Where the rail starts on a desktop: under the map's right-hand column
    /// of round buttons (`dock::MapChrome`), which shares its edge.
    top: f32,
}

/// The zoom rail on the right edge, for one scale (`systems::zoom_rail`): the
/// scale disc on top, − (out), the track, + (in). Returns the held zoom rate of
/// the two discs and a height to go to, if the track, the scale list or an
/// edge asked for one. While a jump flies (`flying_to`), a ring marks where it
/// is going.
///
/// **The scale disc** wears the current scale's icon; a tap opens the four
/// scales beside it, and a tap on one flies there (to its overview, or for the
/// globe back to the height a jump left it at). Until 2026-11-07 the one track
/// ran from the street to the universe (`docs/mockups/zoom-ladder/` v3, v4).
///
/// **At an end of the scale** the − (or +) becomes the next scale's button:
/// held, it is still the zoom, which pushes against the end and crosses after
/// half a second (`free_camera`, `EDGE_PUSH_SECS`), the ring round it filling
/// meanwhile; a tap crosses at once. A press that began while it was still −
/// is no tap: a held − that reaches the end must not jump when let go.
///
/// **It names one thing, and only while it moves**: the step the camera is
/// on, beside the thumb, fading out two seconds after the step last changed
/// or the rail was last touched. Until 2026-11-02 it also listed every rung
/// beside the track on hover, and on a touchscreen egui's hover is the first
/// finger down — a pinch near the right edge flashed the list up.
///
/// The track takes only a finger that **landed** on it and stayed alone; a
/// pinch's first finger landing there used to set the height before the
/// pinch began (`docs/notes/zoom-ladder.md`).
#[allow(clippy::too_many_arguments)]
fn zoom_rail_control(
    ctx: &egui::Context,
    fingers: &Fingers,
    areas: &mut Vec<egui::Rect>,
    rail: &RailIn,
    state: &mut RailState,
    touch_ui: bool,
    touching: bool,
) -> (f32, Option<f64>) {
    use crate::systems::zoom_rail::{altitude_at, index_of, position_of, step_altitude, Scale};
    let (alt, scale) = (rail.alt, rail.scale);
    let steps = scale.steps();
    let mut wish = 0.0;
    let mut goal = None;
    let now = ctx.input(|i| i.time);
    let track_len = if touch_ui {
        RAIL_TRACK_TOUCH
    } else {
        RAIL_TRACK
    };
    let current = (scale, index_of(scale, alt));
    if current != state.last_step || rail.flying_to.is_some() {
        state.last_step = current;
        state.active_at = now;
    }
    // Fingers that landed this frame, rather than slid in from the map.
    let landed: Vec<(u64, egui::Pos2)> = fingers
        .held
        .iter()
        .filter(|(id, _)| !state.seen.contains(id))
        .copied()
        .collect();
    state.seen = fingers.held.iter().map(|(id, _)| *id).collect();
    state.landed_at.retain(|(id, _)| {
        fingers.held.iter().any(|(f, _)| f == id) || fingers.released.iter().any(|(f, _)| f == id)
    });
    state
        .landed_at
        .extend(landed.iter().map(|(id, _)| (*id, now)));
    if !touching && ctx.input(|i| i.pointer.any_pressed()) {
        state.mouse_pressed_at = now;
    }
    // When each end began to offer the next scale: a press from before then
    // is the − or + held into it, not a tap on the scale.
    let pos_now = position_of(scale, alt);
    let edge = [
        (pos_now <= 0.005 || rail.push > 0.0) && scale.outward().is_some(),
        (pos_now >= 0.995 || rail.push < 0.0) && scale.inward().is_some(),
    ];
    for (i, on) in edge.into_iter().enumerate() {
        state.edge_since[i] = match (on, state.edge_since[i]) {
            (true, Some(t)) => Some(t),
            (true, None) => Some(now),
            (false, _) => None,
        };
    }
    // Esc, or a press anywhere but the list and its disc, folds the list.
    if state.list_open {
        let keep = state.list_rect;
        let outside = |p: egui::Pos2| !keep.is_some_and(|r| r.contains(p));
        let pressed_out = (!touching
            && ctx.input(|i| i.pointer.any_pressed())
            && ctx.pointer_interact_pos().is_some_and(outside))
            || landed.iter().any(|(_, p)| outside(*p));
        if pressed_out || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            state.list_open = false;
        }
    }
    // On a phone, the rail stands at the foot of the right edge, over the
    // World | Community switch, where the old + / − pair was and the thumb is:
    // the middle of the edge is where the other hand pinches (`touch-input.md`,
    // "Both edges anchor to the bottom"). A desktop hangs it under the column
    // of round buttons on the same edge (`dock::right_column`, 2026-11-10): in
    // the middle of the edge it ran into them on a short window.
    let anchor = if touch_ui {
        (
            egui::Align2::RIGHT_BOTTOM,
            egui::vec2(-theme::OSD_MARGIN, -bottom_offset()),
        )
    } else {
        (
            egui::Align2::RIGHT_TOP,
            egui::vec2(-theme::OSD_MARGIN, rail.top),
        )
    };
    let mut scale_disc = egui::Rect::NOTHING;
    let area = egui::Area::new(egui::Id::new("zoom_rail"))
        .anchor(anchor.0, anchor.1)
        .show(ctx, |ui| {
            ui.vertical_centered(|ui| {
                ui.spacing_mut().item_spacing.y = RAIL_SPACING;
                // A mouse over the rail wakes it; a finger has no hover (egui's
                // is the first finger down).
                let hovered = !touching
                    && ui
                        .ctx()
                        .pointer_hover_pos()
                        .is_some_and(|h| ui.max_rect().expand(8.0).contains(h));
                if hovered || state.list_open {
                    state.active_at = now;
                }
                let awake = crate::systems::motion::toggle(
                    ctx,
                    egui::Id::new("zoom_rail_awake"),
                    now - state.active_at < RAIL_AWAKE_SECS,
                    0.4,
                );
                let opacity = RAIL_REST_OPACITY + (1.0 - RAIL_REST_OPACITY) * awake;
                ui.multiply_opacity(opacity);
                let held = |resp: &egui::Response, hit: egui::Rect| {
                    (!touching
                        && resp.is_pointer_button_down_on()
                        && resp.interact_pointer_pos().is_some_and(|p| hit.contains(p)))
                        || fingers.pressing(hit)
                };
                // A tap that began after `since`: the mouse's click, or a
                // finger that landed and lifted inside `hit`.
                let tapped = |resp: &egui::Response, hit: egui::Rect, since: f64| {
                    (!touching && resp.clicked() && state.mouse_pressed_at >= since)
                        || fingers.released.iter().any(|(id, p)| {
                            hit.contains(*p)
                                && state.landed_at.iter().any(|(f, t)| f == id && *t >= since)
                        })
                };

                // The scale disc: the scale's icon, lilac past the Earth.
                let (resp, face) = theme::map_disc(
                    ui,
                    RAIL_DISC,
                    state.list_open,
                    theme::p().accent_ring,
                    egui::Sense::click(),
                );
                let ink = if state.list_open || scale == Scale::Globe || resp.hovered() {
                    face.ink
                } else {
                    theme::PUBLIC
                };
                paint_scale(ui.painter(), face.center, RAIL_DISC * 0.27, scale, ink);
                let resp = theme::named(ui, resp, scale.label());
                scale_disc = resp.rect;
                if resp.clicked() || fingers.lifted_in(reach(resp.rect, true, false)) {
                    state.list_open = !state.list_open;
                }

                // − at the top, or the next scale out at the end.
                let end_disc = |ui: &mut egui::Ui, up: bool| {
                    let next = if up { scale.outward() } else { scale.inward() }
                        .filter(|_| edge[usize::from(!up)]);
                    let (resp, face) = theme::map_disc(
                        ui,
                        RAIL_DISC,
                        false,
                        theme::p().accent_ring,
                        egui::Sense::click_and_drag(),
                    );
                    let p = ui.painter();
                    match next {
                        Some(to) => {
                            paint_scale(p, face.center, RAIL_DISC * 0.27, to, face.ink);
                            p.circle_stroke(
                                face.center,
                                face.radius + 1.5,
                                egui::Stroke::new(1.5, theme::p().accent),
                            );
                            let done = if up { rail.push } else { -rail.push }.max(0.0);
                            if done > 0.0 {
                                paint_arc(
                                    p,
                                    face.center,
                                    face.radius + 3.5,
                                    done,
                                    theme::p().cream,
                                );
                            }
                            // Its name beside it, with the way it goes.
                            let font = egui::FontId::proportional(theme::SIZE_SUB);
                            let text =
                                format!("{} {}", if up { "Out to" } else { "In to" }, to.label());
                            let at = egui::pos2(face.center.x - face.radius - 10.0, face.center.y);
                            let galley =
                                p.layout_no_wrap(text.clone(), font.clone(), theme::p().cream);
                            let tag = egui::Align2::RIGHT_CENTER
                                .anchor_size(at, galley.size())
                                .expand2(egui::vec2(7.0, 3.0));
                            p.rect_filled(
                                tag,
                                egui::CornerRadius::same(theme::RADIUS),
                                theme::p().map_fill,
                            );
                            p.rect_stroke(
                                tag,
                                egui::CornerRadius::same(theme::RADIUS),
                                egui::Stroke::new(1.0, theme::p().accent),
                                egui::StrokeKind::Inside,
                            );
                            p.text(at, egui::Align2::RIGHT_CENTER, text, font, theme::p().cream);
                        }
                        None => paint_mark(
                            p,
                            face.center,
                            RAIL_DISC * 0.2,
                            if up { Mark::Minus } else { Mark::Plus },
                            face.ink,
                        ),
                    }
                    let name = match next {
                        Some(to) => to.label(),
                        None if up => Mark::Minus.label(),
                        None => Mark::Plus.label(),
                    };
                    (theme::named(ui, resp, name), next)
                };
                let (resp, next) = end_disc(ui, true);
                let hit = reach(resp.rect, false, false);
                let pressed = held(&resp, hit);
                if pressed {
                    wish -= 1.0;
                }
                if let (Some(_), Some(since)) = (next, state.edge_since[0]) {
                    if tapped(&resp, hit, since) {
                        goal = scale.across(true);
                    }
                }

                let (rect, resp) = ui.allocate_exact_size(
                    egui::vec2(RAIL_DISC, track_len),
                    egui::Sense::click_and_drag(),
                );
                let resp = theme::named(ui, resp, "Zoom");
                let x = rect.center().x;
                let y_at = |pos: f32| rect.top() + pos * track_len;
                let grab = rect.expand(6.0);

                // The finger on the track: one that landed there, alone.
                if state.finger.is_none() {
                    if let Some((id, p)) = landed.iter().find(|(_, p)| grab.contains(*p)) {
                        state.finger = Some(RailFinger {
                            id: *id,
                            since: now,
                            from: *p,
                            live: false,
                        });
                    }
                }
                let (mut finger_at, mut tap) = (None, None);
                if let Some(mut f) = state.finger.take() {
                    let alone = fingers.free == 0 && fingers.held.len() <= 1;
                    if let Some(p) = fingers.at(f.id) {
                        // A second finger anywhere makes it a pinch, not the
                        // rail's: let go of it for good.
                        if alone {
                            f.live |= now - f.since >= RAIL_GRAB_SECS
                                || (p.y - f.from.y).abs() >= RAIL_GRAB_SLOP;
                            if f.live {
                                finger_at = Some(p);
                            }
                            state.finger = Some(f);
                        }
                    } else if alone && !f.live {
                        // Lifted before it took the track: a tap.
                        tap = fingers
                            .released
                            .iter()
                            .find(|(id, _)| *id == f.id)
                            .map(|(_, p)| *p);
                    }
                }
                // The mouse's own press; a finger's goes through the above,
                // because egui's one pointer is the first finger down.
                let pointer = resp
                    .interact_pointer_pos()
                    .filter(|_| !touching && resp.is_pointer_button_down_on())
                    .or(finger_at);
                let rail_at = |p: egui::Pos2| altitude_at(scale, (p.y - rect.top()) / track_len);
                if let Some(p) = pointer {
                    goal = Some(rail_at(p));
                    state.active_at = now;
                }
                // A tap (or a click) goes to the step it landed on, where its
                // places are a comfortable size; a drag goes to the height
                // under it.
                if !touching && resp.clicked() {
                    tap = resp.interact_pointer_pos();
                }
                if let Some(p) = tap {
                    goal = Some(step_altitude(scale, index_of(scale, rail_at(p))));
                    state.active_at = now;
                }

                // The track, lilac past the Earth (the colour the app uses for
                // what is public and beyond the street), plain on the globe.
                let p = ui.painter();
                let (top, bottom) = (egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom()));
                p.line_segment([top, bottom], egui::Stroke::new(4.0, theme::p().map_fill));
                let tone = if scale == Scale::Globe {
                    theme::p().text_dim
                } else {
                    theme::PUBLIC.gamma_multiply(0.8)
                };
                p.line_segment([top, bottom], egui::Stroke::new(1.5, tone));
                // A faint tick between steps: few enough now to count.
                for k in 1..steps.len() {
                    let y = y_at(k as f32 / steps.len() as f32);
                    p.line_segment(
                        [egui::pos2(x - 3.0, y), egui::pos2(x + 3.0, y)],
                        egui::Stroke::new(1.0, tone.gamma_multiply(0.6)),
                    );
                }
                let thumb_y = y_at(pos_now);
                if let Some(to) = rail.flying_to.filter(|to| Scale::of(*to) == scale) {
                    p.circle_stroke(
                        egui::pos2(x, y_at(position_of(scale, to))),
                        8.0,
                        egui::Stroke::new(2.0, theme::p().map_mark),
                    );
                }
                p.circle_filled(egui::pos2(x, thumb_y), 7.0, theme::p().map_mark);
                p.circle_stroke(
                    egui::pos2(x, thumb_y),
                    9.5,
                    egui::Stroke::new(3.0, theme::p().map_rim),
                );

                // The step's name beside the thumb, while the rail is awake
                // and no end is offering the next scale there.
                let shown = crate::systems::motion::toggle(
                    ctx,
                    egui::Id::new("zoom_rail_name"),
                    now - state.active_at < RAIL_AWAKE_SECS,
                    0.25,
                );
                state.shown = shown;
                if shown > 0.0 {
                    let label = steps[current.1].label;
                    let font = egui::FontId::proportional(theme::SIZE_SUB);
                    let at = egui::pos2(x - 16.0, thumb_y);
                    let mut p = ui.painter().clone();
                    p.multiply_opacity(shown);
                    let galley =
                        p.layout_no_wrap(label.to_string(), font.clone(), theme::p().cream);
                    let tag = egui::Align2::RIGHT_CENTER
                        .anchor_size(at, galley.size())
                        .expand2(egui::vec2(7.0, 3.0));
                    p.rect_filled(
                        tag,
                        egui::CornerRadius::same(theme::RADIUS),
                        theme::p().map_fill,
                    );
                    p.text(
                        at,
                        egui::Align2::RIGHT_CENTER,
                        label,
                        font,
                        theme::p().cream,
                    );
                }

                let (resp, next) = end_disc(ui, false);
                let hit = reach(resp.rect, false, true);
                let pressed = held(&resp, hit);
                if pressed {
                    wish += 1.0;
                }
                if let (Some(_), Some(since)) = (next, state.edge_since[1]) {
                    if tapped(&resp, hit, since) {
                        goal = scale.across(false);
                    }
                }
            });
        });
    // The four scales, opened from the disc: beside it, outermost on top.
    state.list_rect = None;
    if state.list_open {
        let list = egui::Area::new(egui::Id::new("zoom_scales"))
            .order(egui::Order::Foreground)
            .pivot(egui::Align2::RIGHT_TOP)
            .fixed_pos(scale_disc.left_top() + egui::vec2(-8.0, 0.0))
            .show(ctx, |ui| {
                ui.spacing_mut().item_spacing.y = RAIL_SPACING;
                for s in Scale::ALL.into_iter().rev() {
                    let font = egui::FontId::proportional(theme::SIZE_BODY);
                    let galley = ui.painter().layout_no_wrap(
                        s.label().to_string(),
                        font.clone(),
                        theme::p().cream,
                    );
                    let h = RAIL_DISC + 4.0;
                    let w = galley.size().x + h + 24.0;
                    let (rect, resp) =
                        ui.allocate_exact_size(egui::vec2(w, h), egui::Sense::click());
                    let resp = theme::named(ui, resp, s.label());
                    let on = s == scale;
                    let p = ui.painter();
                    let fill = if resp.hovered() {
                        theme::p().surface_hover
                    } else {
                        theme::p().map_fill
                    };
                    let round = egui::CornerRadius::same(theme::RADIUS_PILL);
                    p.rect_filled(rect, round, fill);
                    p.rect_stroke(
                        rect,
                        round,
                        egui::Stroke::new(1.0, theme::p().map_rim),
                        egui::StrokeKind::Inside,
                    );
                    let c = egui::pos2(rect.right() - h / 2.0, rect.center().y);
                    let r = h / 2.0 - 3.0;
                    let (plate, ink) = if on {
                        (theme::p().accent, theme::p().on_accent)
                    } else {
                        (
                            theme::p().surface_raised,
                            if s == Scale::Globe {
                                theme::p().cream
                            } else {
                                theme::PUBLIC
                            },
                        )
                    };
                    p.circle_filled(c, r, plate);
                    paint_scale(p, c, r * 0.6, s, ink);
                    p.text(
                        egui::pos2(c.x - r - 10.0, c.y),
                        egui::Align2::RIGHT_CENTER,
                        s.label(),
                        font,
                        if on {
                            theme::p().cream
                        } else {
                            theme::p().map_ink
                        },
                    );
                    if resp.clicked() || fingers.lifted_in(rect) {
                        goal = Some(if on {
                            s.overview()
                        } else {
                            s.arrival(rail.globe_return)
                        });
                        state.list_open = false;
                        state.active_at = now;
                    }
                }
            });
        areas.push(list.response.rect);
        state.list_rect = Some(list.response.rect.union(scale_disc));
    }
    if wish != 0.0 {
        state.active_at = now;
    }
    // The name is not a control: only the rail itself is claimed, so a pinch
    // beside it keeps both its fingers.
    areas.push(area.response.rect);
    (wish, goal)
}

/// A scale's icon, painted like the [`Mark`]s: the globe a meridian and an
/// equator in a ring; the solar system a Sun in two orbits with a planet; the
/// Milky Way a core with two arms; the universe a web of points.
fn paint_scale(
    p: &egui::Painter,
    c: egui::Pos2,
    s: f32,
    scale: crate::systems::zoom_rail::Scale,
    ink: egui::Color32,
) {
    use crate::systems::zoom_rail::Scale;
    let line = egui::Stroke::new((s * 0.16).max(1.3), ink);
    let thin = egui::Stroke::new((s * 0.11).max(1.0), ink.gamma_multiply(0.75));
    match scale {
        Scale::Globe => {
            p.circle_stroke(c, s, line);
            p.add(egui::Shape::ellipse_stroke(
                c,
                egui::vec2(s * 0.45, s),
                line,
            ));
            p.line_segment([c - egui::vec2(s, 0.0), c + egui::vec2(s, 0.0)], line);
        }
        Scale::SolarSystem => {
            p.circle_filled(c, s * 0.3, ink);
            p.circle_stroke(c, s * 0.62, thin);
            p.circle_stroke(c, s, thin);
            p.circle_filled(c + egui::vec2(s * 0.71, -s * 0.71), s * 0.2, ink);
            p.circle_filled(c + egui::vec2(-s * 0.44, s * 0.44), s * 0.15, ink);
        }
        Scale::MilkyWay => {
            p.circle_filled(c, s * 0.22, ink);
            // Two arms, each a quarter turn more of a widening spiral.
            for turn in [0.0_f32, std::f32::consts::PI] {
                let points: Vec<egui::Pos2> = (0..=16)
                    .map(|k| {
                        let t = k as f32 / 16.0;
                        let a = turn + t * 1.6 * std::f32::consts::PI;
                        let r = s * (0.3 + 0.7 * t);
                        c + egui::vec2(a.cos(), a.sin()) * r
                    })
                    .collect();
                p.add(egui::Shape::line(points, line));
            }
        }
        Scale::Universe => {
            let at = [
                (-0.7, -0.5),
                (0.55, -0.75),
                (0.0, 0.0),
                (-0.75, 0.65),
                (0.7, 0.7),
                (0.95, -0.05),
            ];
            let pt = |(x, y): (f32, f32)| c + egui::vec2(x, y) * s;
            for i in [0, 1, 3, 4, 5] {
                p.line_segment([pt(at[2]), pt(at[i])], thin);
            }
            p.line_segment([pt(at[0]), pt(at[1])], thin);
            for (i, a) in at.iter().enumerate() {
                p.circle_filled(pt(*a), s * if i == 2 { 0.22 } else { 0.15 }, ink);
            }
        }
    }
}

/// A ring from the top round clockwise, `done` of the way.
fn paint_arc(p: &egui::Painter, c: egui::Pos2, r: f32, done: f32, ink: egui::Color32) {
    let n = (48.0 * done).ceil().max(2.0) as usize;
    let points: Vec<egui::Pos2> = (0..=n)
        .map(|k| {
            let a =
                -std::f32::consts::FRAC_PI_2 + std::f32::consts::TAU * done * k as f32 / n as f32;
            c + egui::vec2(a.cos(), a.sin()) * r
        })
        .collect();
    p.add(egui::Shape::line(points, egui::Stroke::new(2.0, ink)));
}

/// An egui rect in the window's own logical points.
fn to_window(r: egui::Rect, ui_scale: f32) -> Rect {
    Rect::from_corners(
        Vec2::new(r.min.x, r.min.y) * ui_scale,
        Vec2::new(r.max.x, r.max.y) * ui_scale,
    )
}

/// The jump, bottom-right — the verb a phone had no key for.
///
/// True on the frame its button was *released*. This is the one part of this
/// module that is not a rate: a jump is an event, and
/// `avatar::handle_avatar_movement` consumes it as an edge. `clicked()` rather
/// than `is_pointer_button_down_on` for exactly that reason — the held test that
/// suits a rate would fire a jump every frame of a press.
///
/// Bottom-**right**, opposite the stick, because that is where the other thumb
/// is: the same left-translates / right-acts split the module opens with. It
/// stands *above* the zoom pair, which is the bottom-most thing on that edge in
/// every mode — see [`vertical_pair`] for why the pair rather than this owns the
/// corner. (A flight toggle stood under it until avatar flight was removed,
/// 2026-11-14.)
fn jump_button(
    ctx: &egui::Context,
    fingers: &Fingers,
    areas: &mut Vec<egui::Rect>,
    at: Spots,
) -> bool {
    let mut jump = false;
    let area = egui::Area::new(egui::Id::new("osd_actions"))
        .anchor(egui::Align2::RIGHT_BOTTOM, -at.actions)
        .show(ctx, |ui| {
            // A claimed finger lifting inside the disc is the same event
            // `clicked()` reports, which is why the two are `or`-ed and not
            // chosen between: on a phone the first arm fires, on a desktop (or
            // when this finger also happens to hold egui's pointer) the second
            // does, and a jump is idempotent either way.
            let resp = round_button(ui, Mark::Jump, false);
            jump = resp.clicked() || fingers.lifted_in(reach(resp.rect, true, true));
        });
    areas.push(area.response.rect);
    jump
}

/// The glyphs, painted rather than typed. `✕` taught this lesson once already
/// (`theme::close_button`): the bundled brand font carries no symbol set, so a
/// `+` or a `▲` in a `RichText` is a gamble on a font subset, while two line
/// segments land on the pixel grid at any scale. The library's edition
/// (`lib_map_osd`) builds the same strokes out of thin boxes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mark {
    Plus,
    Minus,
    /// Jump: a chevron with a bar under it — an arrow leaving the ground.
    Jump,
    /// A face: the expressions (`avatar_face`), a ring with two eyes and a
    /// smile.
    Face,
    /// An open hand: the phone in your hand moves your avatar's
    /// (`held_phone`).
    Hand,
}

/// What the avatar does with your body in Follow — its face and its hand —
/// bundled so `render_map_osd` stays under Bevy's sixteen parameters.
#[derive(bevy::ecs::system::SystemParam)]
pub struct BodyControls<'w, 's> {
    faces: MessageWriter<'w, crate::systems::avatar_face::StartExpression>,
    faces_open: Local<'s, bool>,
    high: ResMut<'w, crate::systems::high_precision::HighPrecision>,
    held: Res<'w, crate::systems::held_phone::HeldPhone>,
    store: ResMut<'w, crate::systems::user_store::UserStore>,
    card: ResMut<'w, crate::systems::held_phone::HandCard>,
    /// A finger resting on the hand disc, for its long press (the library's
    /// edition; egui's has `Response::long_touched`).
    hand_press: Local<'s, LongPress>,
}

/// The hand button, above the face disc (`docs/mockups/hand-tracking/`): while
/// it is on, the phone in your hand is the phone in your avatar's, and moving
/// it moves the arm (`systems::high_precision` follows the phone,
/// `systems::held_phone` poses the arm). A tap turns it on or off; a long
/// press (or a right-click) picks the hand and starts the tracking again; the
/// word to its left is what the tracking needs from you.
///
/// Only where it can work — a phone's camera and motion sensors — or where a
/// capture holds a phone (`CARTO_HELD_PHONE`), so it can be photographed.
fn hand_button(
    ctx: &egui::Context,
    fingers: &Fingers,
    areas: &mut Vec<egui::Rect>,
    body: &mut BodyControls,
    at: Spots,
) {
    use crate::systems::held_phone::Side;
    use crate::systems::high_precision::{HandWord, HighPrecision};
    let stand_in = body.held.stand_in().is_some();
    if !HighPrecision::available() && !stand_in {
        return;
    }
    let word = body
        .high
        .hand_word()
        .or_else(|| stand_in.then_some(HandWord::Following));
    let on = word.is_some();
    let tone = match word {
        Some(HandWord::Following) => theme::p().accent_bright,
        Some(HandWord::CameraBusy) => theme::p().warn,
        _ => theme::p().accent_ring,
    };
    // Beside the jump disc rather than in a tier above it: the right
    // edge's top half is the map's column of round buttons now
    // (`dock::right_column`).
    let area = egui::Area::new(egui::Id::new("osd_hand"))
        .anchor(egui::Align2::RIGHT_BOTTOM, -at.hand)
        .show(ctx, |ui| {
            let (resp, face) = theme::map_disc(ui, CONTROL_SIZE, on, tone, egui::Sense::click());
            let resp = theme::named(ui, resp, Mark::Hand.label());
            paint_mark(
                ui.painter(),
                face.center,
                CONTROL_SIZE * 0.22,
                Mark::Hand,
                face.ink,
            );
            if let Some(word) = word {
                crate::systems::theme::halo_text_aligned(
                    ui.painter(),
                    egui::pos2(face.center.x - face.radius - 8.0, face.center.y),
                    egui::Align2::RIGHT_CENTER,
                    word.text(),
                    tone,
                );
            }
            let tapped = resp.clicked() || fingers.lifted_in(reach(resp.rect, true, false));
            if tapped && !resp.long_touched() && HighPrecision::available() {
                body.tap_hand();
            }
            resp.context_menu(|ui| {
                ui.label(egui::RichText::new(HAND_MENU_TITLE).strong());
                ui.horizontal(|ui| {
                    for (side, label) in [(Side::Left, "Left"), (Side::Right, "Right")] {
                        if ui.selectable_label(body.held.side == side, label).clicked() {
                            body.set_hand_side(side == Side::Left);
                        }
                    }
                });
                if ui
                    .add_enabled_ui(body.high.on, |ui| {
                        theme::secondary_button(ui, "", HAND_MENU_RESTART, 0.0)
                    })
                    .inner
                    .clicked()
                {
                    body.restart_hand();
                    ui.close();
                }
                if let Some(problem) = body.high.problem() {
                    ui.label(egui::RichText::new(problem).color(theme::p().warn));
                }
                ui.label(
                    egui::RichText::new(HAND_MENU_NOTE)
                        .size(theme::SIZE_CHIP)
                        .color(theme::p().text_dim),
                );
            });
        });
    areas.push(area.response.rect);
    if body.card.0 {
        hand_card(ctx, body);
    }
}

/// The hand button's first tap: what it uses and who feels it, said once
/// before Android asks for the camera. A hand report is a precise position
/// (`docs/notes/hands-and-tones.md`), so this is the consent, and "Use my
/// hand" is remembered (`HandPrefs::agreed`).
fn hand_card(ctx: &egui::Context, body: &mut BodyControls) {
    let modal = egui::Modal::new(egui::Id::new("hand_card")).show(ctx, |ui| {
        ui.set_max_width(420.0);
        ui.label(
            egui::RichText::new(HAND_CARD_TITLE)
                .size(theme::SIZE_TITLE)
                .strong(),
        );
        ui.add_space(theme::OSD_GAP);
        for line in HAND_CARD_LINES {
            ui.label(line);
            ui.add_space(theme::OSD_GAP * 0.5);
        }
        ui.add_space(theme::OSD_GAP);
        ui.horizontal(|ui| {
            if theme::secondary_button(ui, "", HAND_CARD_DECLINE, 140.0).clicked() {
                body.decline_hand();
            }
            if theme::primary_button_sized(ui, HAND_CARD_ACCEPT, 160.0).clicked() {
                body.accept_hand();
            }
        });
    });
    if modal.should_close() {
        body.decline_hand();
    }
}

/// The hand card's words, one copy for both editions (`lib_map_osd`).
pub const HAND_CARD_TITLE: &str = "Your phone, in your avatar's hand";
pub const HAND_CARD_LINES: [&str; 3] = [
    "The back camera and the motion sensors follow the phone, to about \
     a centimetre. Nothing the camera sees is kept or sent.",
    "Things that sense hands, such as a theremin, feel where your hand \
     is while you are within a few metres of them.",
    "Later, people near you will see your arm move: the same switch.",
];
pub const HAND_CARD_DECLINE: &str = "Not now";
pub const HAND_CARD_ACCEPT: &str = "Use my hand";

/// The hand menu's words, one copy for both editions (`lib_map_osd`).
pub const HAND_MENU_TITLE: &str = "Your hand";
pub const HAND_MENU_RESTART: &str = "Start again";
pub const HAND_MENU_NOTE: &str =
    "Uses the back camera and the motion sensors. Nothing the camera sees is kept.";

impl BodyControls<'_, '_> {
    /// The hand disc's tap: on or off once agreed, else the card that asks.
    fn tap_hand(&mut self) {
        if self.high.on || self.store.data.hand.agreed {
            self.high.on = !self.high.on;
        } else {
            self.card.0 = true;
        }
    }

    /// Whether the first-use card is up.
    #[must_use]
    #[cfg_attr(not(feature = "ui-lib"), allow(dead_code))]
    pub fn hand_card_open(&self) -> bool {
        self.card.0
    }

    /// The card's "Use my hand": remembered, and the tracking on.
    pub fn accept_hand(&mut self) {
        self.store.data.hand.agreed = true;
        self.store.dirty = true;
        self.high.on = true;
        self.card.0 = false;
    }

    /// The card's "Not now", its backdrop or Escape.
    pub fn decline_hand(&mut self) {
        self.card.0 = false;
    }

    /// The hand menu's Left or Right: which hand holds the phone,
    /// remembered.
    pub fn set_hand_side(&mut self, left: bool) {
        self.store.data.hand.left = left;
        self.store.dirty = true;
    }

    /// The hand menu's Start again: the tracking starts over.
    pub fn restart_hand(&mut self) {
        self.high.restart();
    }
}

/// A disc above the hand button, beside the jump disc, that opens a column of expressions
/// beside it; a tap on one pulls that face and closes it. Returns the face
/// picked this frame.
fn face_picker(
    ctx: &egui::Context,
    fingers: &Fingers,
    areas: &mut Vec<egui::Rect>,
    open: &mut bool,
    at: Spots,
) -> Option<cartopolis_core::Expression> {
    let above = at.face.y;
    let area = egui::Area::new(egui::Id::new("osd_face"))
        .anchor(egui::Align2::RIGHT_BOTTOM, -at.face)
        .show(ctx, |ui| {
            let resp = round_button(ui, Mark::Face, *open);
            if resp.clicked() || fingers.lifted_in(reach(resp.rect, true, false)) {
                *open = !*open;
            }
        });
    areas.push(area.response.rect);
    if !*open {
        return None;
    }
    let mut picked = None;
    let list = egui::Area::new(egui::Id::new("osd_faces"))
        .anchor(
            egui::Align2::RIGHT_BOTTOM,
            egui::vec2(-(at.face.x + CONTROL_SIZE + theme::OSD_GAP), -above),
        )
        .show(ctx, |ui| {
            theme::overlay_frame().show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = FACE_ROW_GAP;
                for e in cartopolis_core::Expression::ALL {
                    let resp = theme::secondary_button(ui, "", expression_label(e), FACE_ROW_W);
                    if resp.clicked() || fingers.lifted_in(resp.rect) {
                        picked = Some(e);
                    }
                }
            });
        });
    areas.push(list.response.rect);
    if picked.is_some() {
        *open = false;
    }
    picked
}

fn expression_label(e: cartopolis_core::Expression) -> &'static str {
    use cartopolis_core::Expression as E;
    match e {
        E::Happy => "Smile",
        E::Laugh => "Laugh",
        E::Sad => "Sad",
        E::Angry => "Angry",
        E::Surprised => "Surprised",
        E::Afraid => "Afraid",
        E::Disgusted => "Disgusted",
        E::Wink => "Wink",
    }
}

/// Two round buttons at the bottom of the right edge. Returns `+1` while the top
/// one is held, `-1` while the bottom one is, `0` otherwise — *held*, not
/// clicked, because these are rates.
///
/// **The pair is the bottom-most thing on that edge in every mode**, and that is
/// the whole of its position rule. It used to be anchored `RIGHT_CENTER`, i.e.
/// halfway up the window, while the jump pair sat in the corner below it:
/// two clusters of identical discs on one edge at two unrelated heights, the
/// busier of them out of a thumb's reach on a phone held in one hand, and — since
/// the scale bar was also anchored to that corner — one of them overlapping a
/// readout it painted over. Bottom-anchored, the right edge is a single column
/// that grows upwards, mirroring the stick opposite it, and the pair keeps one
/// position across Fly, Follow and Earth: a phone should not have to learn two
/// positions for one verb.
fn vertical_pair(
    ctx: &egui::Context,
    fingers: &Fingers,
    areas: &mut Vec<egui::Rect>,
    id: &str,
    top: Mark,
    bottom: Mark,
) -> f32 {
    let mut wish = 0.0;
    let area = egui::Area::new(egui::Id::new(id))
        .anchor(
            egui::Align2::RIGHT_BOTTOM,
            egui::vec2(-theme::OSD_MARGIN, -bottom_offset()),
        )
        .show(ctx, |ui| {
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = theme::OSD_GAP;
                // A claimed finger resting on the disc is the held state, whether
                // or not egui's one pointer is that finger — so zooming with one
                // thumb no longer forbids the other from turning the view. The
                // egui arm is bounded to the same rect: `is_pointer_button_down_on`
                // holds a `click_and_drag` widget for as long as the press that
                // started on it stays down, so a thumb that slid *off* the disc
                // kept zooming until it lifted.
                let held = |resp: &egui::Response, hit: egui::Rect| {
                    (resp.is_pointer_button_down_on()
                        && resp.interact_pointer_pos().is_some_and(|p| hit.contains(p)))
                        || fingers.pressing(hit)
                };
                let resp = round_button(ui, top, false);
                if held(&resp, reach(resp.rect, true, false)) {
                    wish += 1.0;
                }
                let resp = round_button(ui, bottom, false);
                if held(&resp, reach(resp.rect, false, true)) {
                    wish -= 1.0;
                }
            });
        });
    areas.push(area.response.rect);
    wish
}

/// One round control. Returns its `Response`, so the caller picks the test that
/// suits it: a rate wants `is_pointer_button_down_on` (a finger drifting a point
/// or two while held produces a *drag*, and `clicked()` would drop the whole
/// push — the same reason the on-screen keyboard dismisses on the pointer), while
/// an edge like a jump wants `clicked()`, or it fires every frame of the press.
///
/// The plate is [`theme::map_disc`], which is what makes these the same object as
/// the dock discs eight points below them and the compass in the far corner.
/// They were painted here instead, one shade off on every state — `TEXT_DIM` ink
/// at rest against the dock's `TEXT`, no lift on hover, and their own answer to
/// what a pressed ring is.
fn round_button(ui: &mut egui::Ui, mark: Mark, on: bool) -> egui::Response {
    let (resp, face) = theme::map_disc(
        ui,
        CONTROL_SIZE,
        on,
        theme::p().accent_ring,
        egui::Sense::click_and_drag(),
    );
    paint_mark(
        ui.painter(),
        face.center,
        CONTROL_SIZE * 0.22,
        mark,
        face.ink,
    );
    theme::named(ui, resp, mark.label())
}

impl Mark {
    /// What the control does, for egui's widget info (`theme::named`).
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Plus => "Zoom in",
            Self::Minus => "Zoom out",
            Self::Jump => "Jump",
            Self::Face => "Expressions",
            Self::Hand => "Hand",
        }
    }
}

fn paint_mark(p: &egui::Painter, c: egui::Pos2, s: f32, mark: Mark, ink: egui::Color32) {
    let stroke = egui::Stroke::new(2.0, ink);
    match mark {
        Mark::Plus => {
            p.line_segment([c - egui::vec2(s, 0.0), c + egui::vec2(s, 0.0)], stroke);
            p.line_segment([c - egui::vec2(0.0, s), c + egui::vec2(0.0, s)], stroke);
        }
        Mark::Minus => {
            p.line_segment([c - egui::vec2(s, 0.0), c + egui::vec2(s, 0.0)], stroke);
        }
        // A chevron over a baseline: the ground, and something leaving it.
        Mark::Jump => {
            let (dx, dy) = (s, s * 0.55);
            p.line_segment(
                [c + egui::vec2(-dx, 0.0), c + egui::vec2(0.0, -dy * 1.6)],
                stroke,
            );
            p.line_segment(
                [c + egui::vec2(0.0, -dy * 1.6), c + egui::vec2(dx, 0.0)],
                stroke,
            );
            p.line_segment([c + egui::vec2(-dx, dy), c + egui::vec2(dx, dy)], stroke);
        }
        Mark::Face => {
            p.circle_stroke(c, s * 1.1, stroke);
            for side in [-1.0_f32, 1.0] {
                p.circle_filled(c + egui::vec2(side * s * 0.4, -s * 0.3), 1.8, ink);
            }
            let smile: Vec<egui::Pos2> = (0..=8)
                .map(|i| {
                    let a = std::f32::consts::PI * (0.2 + 0.6 * i as f32 / 8.0);
                    c + egui::vec2(a.cos() * s * 0.6, a.sin() * s * 0.6)
                })
                .collect();
            p.add(egui::Shape::line(smile, stroke));
        }
        // A palm and four fingers, the thumb out to the side.
        Mark::Hand => {
            let palm = egui::Rect::from_center_size(
                c + egui::vec2(0.0, s * 0.45),
                egui::vec2(s * 1.3, s * 1.0),
            );
            p.rect_stroke(palm, s * 0.3, stroke, egui::StrokeKind::Middle);
            for (i, len) in [0.9_f32, 1.1, 1.05, 0.8].iter().enumerate() {
                let x = palm.left() + palm.width() * (0.14 + 0.24 * i as f32);
                p.line_segment(
                    [
                        egui::pos2(x, palm.top()),
                        egui::pos2(x, palm.top() - s * len),
                    ],
                    stroke,
                );
            }
            p.line_segment(
                [
                    egui::pos2(palm.left(), palm.center().y),
                    egui::pos2(palm.left() - s * 0.5, palm.top() - s * 0.1),
                ],
                stroke,
            );
        } // Chevrons, not filled triangles: at 44 pt a solid arrowhead reads as a
          // blob, and the same two segments scale to any control size.
    }
}

/// Which finger drives the stick this frame, given the one that drove it last.
///
/// Two rules, and the order between them is the whole of it: a finger already
/// steering keeps the stick until it *lifts* — not until it leaves the well,
/// because pushing past the rim is how full throttle is asked for — and only
/// when there is no such finger does the stick adopt one from inside its own
/// area.
///
/// `area` is the square the well is drawn in plus the miss band
/// ([`MISS_MARGIN`]), not the disc: a finger the UI has taken out of the
/// camera's hands but which no control then reads is a finger that does nothing
/// at all. The corners are a few points past the rim and `stick_wish` clamps
/// them back onto it.
#[must_use]
fn stick_owner(current: Option<u64>, fingers: &Fingers, area: egui::Rect) -> Option<u64> {
    current
        .filter(|id| fingers.at(*id).is_some())
        .or_else(|| fingers.inside(area))
}

/// How wide the band the stick floats in is, in wells (2 × [`STICK_RADIUS`]),
/// and the most of the screen's width it may take.
const STICK_FLOAT_WELLS: f32 = 2.2;
const STICK_FLOAT_MAX_FRACTION: f32 = 0.45;

/// Where the well stands for a thumb landing at `at` in the band `zone`: under
/// the thumb, kept whole inside the band.
#[must_use]
fn stick_origin(zone: egui::Rect, at: egui::Pos2) -> egui::Pos2 {
    let x_lo = zone.left() + STICK_RADIUS;
    let x_hi = (zone.right() - STICK_RADIUS).max(x_lo);
    egui::pos2(at.x.clamp(x_lo, x_hi), zone.center().y)
}

/// The translation stick, bottom-left. Returns its reading in the unit disc.
///
/// Bottom-*left* because the dock (the panel toggles) is bottom-centre and the
/// climb pair is right-of-centre: the three do not overlap at any window size,
/// which a corner anchor cannot promise on its own.
///
/// **It floats** (2026-11-14): a thumb landing anywhere along a band
/// [`STICK_FLOAT_WELLS`] wells wide takes the stick, and the well moves under
/// it, as in phone games. The fixed well had to be hit within its 108 points
/// without looking, and a thumb that missed it turned the camera instead of
/// walking — the owner found the phone's controls awkward. At rest, and for a
/// mouse, the well stands at the band's left end as it always did.
fn thumbstick(
    ctx: &egui::Context,
    fingers: &Fingers,
    owner: &mut Option<u64>,
    origin: &mut Option<egui::Pos2>,
    areas: &mut Vec<egui::Rect>,
) -> Vec2 {
    let mut wish = Vec2::ZERO;
    let size = STICK_RADIUS * 2.0;
    let width = stick_band(ctx.content_rect().width());
    let area = egui::Area::new(egui::Id::new("osd_stick"))
        .anchor(
            egui::Align2::LEFT_BOTTOM,
            egui::vec2(theme::OSD_MARGIN, -bottom_offset()),
        )
        .show(ctx, |ui| {
            let (zone, _) = ui.allocate_exact_size(egui::vec2(width, size), egui::Sense::hover());
            let rest = egui::Rect::from_min_size(zone.min, egui::Vec2::splat(size));
            // The mouse works the well where it rests; only a finger floats it.
            let resp = ui.interact(
                rest,
                egui::Id::new("osd_stick_well"),
                egui::Sense::click_and_drag(),
            );
            let resp = theme::named(ui, resp, "Move");
            // The miss band: a grab that lands just past the band still takes
            // the stick (nothing borders it but map), and `stick_wish` clamps
            // the reading back onto the rim.
            let had = *owner;
            *owner = stick_owner(*owner, fingers, zone.expand(MISS_MARGIN));
            if *owner != had {
                *origin = owner
                    .and_then(|id| fingers.at(id))
                    .map(|at| stick_origin(zone, at));
            }
            let centre = origin.filter(|_| owner.is_some()).unwrap_or(rest.center());
            // The mouse is the `or` arm, and on a desktop it is the only one.
            // `interact_pointer_pos` is the position of the pointer that owns
            // *this* widget's drag, so it cannot be stolen by a finger elsewhere
            // either.
            let at = owner.and_then(|id| fingers.at(id)).or_else(|| {
                resp.is_pointer_button_down_on()
                    .then(|| resp.interact_pointer_pos())
                    .flatten()
            });
            if let Some(at) = at {
                wish = stick_wish(
                    Vec2::new(at.x - centre.x, at.y - centre.y),
                    STICK_RADIUS - STICK_KNOB_RADIUS * 0.5,
                );
            }
            let p = ui.painter();
            // The well is a *guide*, not a plate: it is two and a half discs
            // across, and at the opacity the buttons use it was the heaviest
            // thing on the screen — a black hole in the bottom-left corner of the
            // map, larger and darker than every control it shares an edge with
            // put together. [`theme::Palette::overlay_soft`] keeps the map visible through
            // it; the hairline is what says where the travel ends.
            p.circle_filled(centre, STICK_RADIUS, theme::p().overlay_soft);
            p.circle_stroke(
                centre,
                STICK_RADIUS,
                egui::Stroke::new(1.5, theme::p().overlay_line),
            );
            // The knob shows where the thumb has taken it, which is the whole of
            // the feedback: without it a proportional stick is indistinguishable
            // from an on/off pad and the user has no way to learn the gradient.
            // It is the *disc*, so it carries the disc's rest and engaged
            // colours — the same two plates every other control on this screen
            // switches between.
            let travel = STICK_RADIUS - STICK_KNOB_RADIUS - 2.0;
            let knob = centre + egui::vec2(wish.x, -wish.y) * travel;
            let (fill, ring) = if wish == Vec2::ZERO {
                (theme::p().overlay, theme::p().overlay_line)
            } else {
                (theme::p().accent_muted, theme::p().accent_ring)
            };
            p.circle_filled(knob, STICK_KNOB_RADIUS, fill);
            p.circle_stroke(knob, STICK_KNOB_RADIUS, egui::Stroke::new(1.5, ring));
        });
    areas.push(area.response.rect);
    wish
}

/// How wide the band the stick floats in is, on a screen `width` points
/// wide: [`STICK_FLOAT_WELLS`] wells, no more than
/// [`STICK_FLOAT_MAX_FRACTION`] of the screen, never less than one well.
fn stick_band(width: f32) -> f32 {
    let size = STICK_RADIUS * 2.0;
    (size * STICK_FLOAT_WELLS)
        .min(width * STICK_FLOAT_MAX_FRACTION)
        .max(size)
}

// ─── The library's edition: no egui ──────────────────────────────────────────
//
// The same controls with the same rules, laid out by the arithmetic egui's
// anchors did (`the_library_edition_lays_out_where_egui_did` holds the two
// together) and worked by Bevy's input: the claimed fingers as ever, the
// mouse's press on a control as one more of them (`touch::claim_mouse`), its
// cursor for hover, the keyboard for Escape. Nothing here touches egui but
// `EguiManager`'s sizes, so it goes on working when egui does not.

/// The expressions' column, as `theme::overlay_frame` and
/// `theme::secondary_button` lay it out: buttons this wide, this far apart,
/// inside the frame's 14 × 12 margin and its hairline.
const FACE_ROW_W: f32 = 120.0;
const FACE_ROW_GAP: f32 = 4.0;
const FACE_FRAME_PAD: egui::Vec2 = egui::vec2(15.0, 13.0);

/// A row of the expressions: `secondary_button`'s height.
fn face_row_h() -> f32 {
    theme::PRIMARY_BUTTON_HEIGHT.max(theme::row_height())
}

/// Bevy's input, as the library's edition reads it.
#[derive(bevy::ecs::system::SystemParam)]
pub struct OsdHands<'w, 's> {
    touch: Res<'w, crate::systems::touch::TouchGestures>,
    mouse: Res<'w, ButtonInput<MouseButton>>,
    keys: Res<'w, ButtonInput<KeyCode>>,
    windows: Query<'w, 's, &'static Window>,
    time: Res<'w, Time<bevy::time::Real>>,
}

impl OsdHands<'_, '_> {
    /// This frame's input, in egui points.
    fn read(&self, ui_scale: f32) -> LibInput {
        let to_ui = move |p: Vec2| egui::pos2(p.x / ui_scale, p.y / ui_scale);
        let cursor = self.windows.iter().next().and_then(Window::cursor_position);
        LibInput {
            fingers: Fingers::with_mouse(&self.touch, ui_scale),
            // A finger has no hover, and the cursor a finger leaves behind
            // (`touch::emulate_mouse` moves it) is not one.
            hover: cursor.filter(|_| !self.touch.ever()).map(to_ui),
            // A press no control took — the map's, a finger's tap included.
            press: cursor
                .filter(|_| self.mouse.just_pressed(MouseButton::Left))
                .map(to_ui),
            secondary: self.touch.ui_secondary().map(to_ui),
            escape: self.keys.just_pressed(KeyCode::Escape),
            now: self.time.elapsed_secs_f64(),
            ui_scale,
        }
    }
}

/// One frame of input for the library's edition, egui points.
struct LibInput {
    /// The claimed fingers, and the mouse's owned press as [`MOUSE`].
    fingers: Fingers,
    /// The mouse's cursor, for hover.
    hover: Option<egui::Pos2>,
    /// A `Left` press no control owns, this frame.
    press: Option<egui::Pos2>,
    /// A `Right` press a control owns, this frame.
    secondary: Option<egui::Pos2>,
    escape: bool,
    /// Seconds, on the real clock.
    now: f64,
    ui_scale: f32,
}

impl LibInput {
    fn hovers(&self, rect: egui::Rect) -> bool {
        self.hover.is_some_and(|h| rect.contains(h))
    }

    /// A disc at `rect`, engaged when `on`, its plate lit under the cursor.
    fn disc(&self, rect: egui::Rect, on: bool) -> DiscState {
        DiscState {
            rect,
            on,
            hovered: !on && self.hovers(rect),
        }
    }
}

/// Where an egui `Area` anchored `Align2::RIGHT_BOTTOM` at `-off` stands:
/// its right edge `off.x` in from the screen's, its foot `off.y` up from the
/// bottom.
fn from_right_bottom(screen: egui::Vec2, off: egui::Vec2, size: egui::Vec2) -> egui::Rect {
    egui::Rect::from_min_size(
        egui::pos2(screen.x - off.x - size.x, screen.y - off.y - size.y),
        size,
    )
}

/// egui's `motion::toggle` without egui: `run` goes to 1 (on) or 0 over
/// `secs`, and the value is it eased out.
fn fade(run: &mut f32, on: bool, secs: f32, dt: f32) -> f32 {
    let step = dt / secs.max(1e-3);
    *run = if on {
        (*run + step).min(1.0)
    } else {
        (*run - step).max(0.0)
    };
    match *run {
        r if r <= 0.0 => 0.0,
        r if r >= 1.0 => 1.0,
        r => crate::systems::motion::ease_out(r),
    }
}

/// [`vertical_pair`]: two discs at the foot of the right edge, held for a
/// rate.
fn lib_pair(
    screen: egui::Vec2,
    input: &LibInput,
    areas: &mut Vec<egui::Rect>,
    top: Mark,
    bottom: Mark,
    view: &mut OsdView,
) -> f32 {
    let area = from_right_bottom(
        screen,
        egui::vec2(theme::OSD_MARGIN, bottom_offset()),
        egui::vec2(CONTROL_SIZE, pair_height()),
    );
    let disc = egui::Vec2::splat(CONTROL_SIZE);
    let upper = egui::Rect::from_min_size(area.min, disc);
    let lower = egui::Rect::from_min_size(
        area.min + egui::vec2(0.0, CONTROL_SIZE + theme::OSD_GAP),
        disc,
    );
    // The sides facing each other stay exact (`reach`).
    let up = input.fingers.pressing(reach(upper, true, false));
    let down = input.fingers.pressing(reach(lower, false, true));
    for (rect, mark, on) in [(upper, top, up), (lower, bottom, down)] {
        view.discs.push(DiscView {
            disc: input.disc(rect, on),
            mark,
            tone: Tone::Ring,
        });
    }
    areas.push(area);
    match (up, down) {
        (true, false) => 1.0,
        (false, true) => -1.0,
        _ => 0.0,
    }
}

/// [`thumbstick`]: a finger floats the well along its band; the mouse
/// works it where it rests, from inside it.
fn lib_stick(
    screen: egui::Vec2,
    input: &LibInput,
    owner: &mut Option<u64>,
    origin: &mut Option<egui::Pos2>,
    areas: &mut Vec<egui::Rect>,
    view: &mut OsdView,
) -> Vec2 {
    let fingers = &input.fingers;
    let size = STICK_RADIUS * 2.0;
    let zone = egui::Rect::from_min_size(
        egui::pos2(theme::OSD_MARGIN, screen.y - bottom_offset() - size),
        egui::vec2(stick_band(screen.x), size),
    );
    let rest = egui::Rect::from_min_size(zone.min, egui::Vec2::splat(size));
    let had = *owner;
    *owner = owner.filter(|id| fingers.at(*id).is_some()).or_else(|| {
        fingers
            .held
            .iter()
            .find(|(id, p)| {
                if *id == MOUSE {
                    rest.contains(*p)
                } else {
                    zone.expand(MISS_MARGIN).contains(*p)
                }
            })
            .map(|(id, _)| *id)
    });
    if *owner != had {
        *origin = owner
            .filter(|id| *id != MOUSE)
            .and_then(|id| fingers.at(id))
            .map(|at| stick_origin(zone, at));
    }
    let centre = origin.filter(|_| owner.is_some()).unwrap_or(rest.center());
    let wish = owner
        .and_then(|id| fingers.at(id))
        .map_or(Vec2::ZERO, |at| {
            stick_wish(
                Vec2::new(at.x - centre.x, at.y - centre.y),
                STICK_RADIUS - STICK_KNOB_RADIUS * 0.5,
            )
        });
    let travel = STICK_RADIUS - STICK_KNOB_RADIUS - 2.0;
    let knob = centre + egui::vec2(wish.x, -wish.y) * travel;
    view.stick = Some(StickView {
        centre: q_pos(centre),
        knob: q_pos(knob),
        engaged: wish != Vec2::ZERO,
    });
    areas.push(zone);
    wish
}

/// [`jump_button`]: true on the frame a press on the disc lifts.
fn lib_jump(
    screen: egui::Vec2,
    input: &LibInput,
    areas: &mut Vec<egui::Rect>,
    at: Spots,
    view: &mut OsdView,
) -> bool {
    let rect = from_right_bottom(screen, at.actions, egui::Vec2::splat(CONTROL_SIZE));
    let hit = reach(rect, true, true);
    view.discs.push(DiscView {
        disc: input.disc(rect, input.fingers.pressing(hit)),
        mark: Mark::Jump,
        tone: Tone::Ring,
    });
    areas.push(rect);
    input.fingers.tapped(hit)
}

/// [`face_picker`]: the face disc, and the expressions beside it while open.
fn lib_faces(
    screen: egui::Vec2,
    input: &LibInput,
    areas: &mut Vec<egui::Rect>,
    open: &mut bool,
    at: Spots,
    view: &mut OsdView,
) -> Option<cartopolis_core::Expression> {
    let fingers = &input.fingers;
    let rect = from_right_bottom(screen, at.face, egui::Vec2::splat(CONTROL_SIZE));
    let hit = reach(rect, true, false);
    if fingers.tapped(hit) {
        *open = !*open;
    }
    view.discs.push(DiscView {
        disc: input.disc(rect, *open || fingers.pressing(hit)),
        mark: Mark::Face,
        tone: Tone::Ring,
    });
    areas.push(rect);
    if !*open {
        return None;
    }
    let all = cartopolis_core::Expression::ALL;
    let n = all.len() as f32;
    let row_h = face_row_h();
    let inner = egui::vec2(FACE_ROW_W, n * row_h + (n - 1.0).max(0.0) * FACE_ROW_GAP);
    let frame = from_right_bottom(
        screen,
        egui::vec2(at.face.x + CONTROL_SIZE + theme::OSD_GAP, at.face.y),
        inner + FACE_FRAME_PAD * 2.0,
    );
    let mut picked = None;
    let mut rows = Vec::new();
    for (i, e) in all.iter().copied().enumerate() {
        let row = egui::Rect::from_min_size(
            frame.min + FACE_FRAME_PAD + egui::vec2(0.0, i as f32 * (row_h + FACE_ROW_GAP)),
            egui::vec2(FACE_ROW_W, row_h),
        );
        if fingers.tapped(row) {
            picked = Some(e);
        }
        rows.push((row, expression_label(e)));
    }
    areas.push(frame);
    view.faces = Some(FacesView { frame, rows });
    if picked.is_some() {
        *open = false;
    }
    picked
}

/// A finger resting on the hand disc: egui's `long_touched`, for the claimed
/// finger egui's one pointer may not be.
#[derive(Default)]
pub struct LongPress {
    /// The finger that landed on the disc.
    finger: Option<Resting>,
    /// The claimed fingers down last frame, to tell a landing.
    seen: Vec<u64>,
}

struct Resting {
    id: u64,
    since: f64,
    from: egui::Pos2,
    strayed: f32,
    fired: bool,
}

impl LongPress {
    /// One frame: did the press fire now, and which finger's lift is spent
    /// (it opened the menu, so its lift is no tap).
    fn step(
        &mut self,
        fingers: &Fingers,
        hit: egui::Rect,
        now: f64,
        slop: f32,
    ) -> (bool, Option<u64>) {
        let landed = fingers
            .held
            .iter()
            .find(|(id, p)| *id != MOUSE && !self.seen.contains(id) && hit.contains(*p))
            .copied();
        self.seen = fingers.held.iter().map(|(id, _)| *id).collect();
        let (mut fired, mut spent) = (false, None);
        if let Some(mut f) = self.finger.take() {
            match fingers.at(f.id) {
                Some(p) => {
                    f.strayed = f.strayed.max(p.distance(f.from));
                    if !f.fired
                        && f.strayed <= slop
                        && now - f.since >= f64::from(crate::systems::touch::LONG_PRESS_SECS)
                    {
                        f.fired = true;
                        fired = true;
                    }
                    self.finger = Some(f);
                }
                None => {
                    if f.fired {
                        spent = Some(f.id);
                    }
                }
            }
        }
        if self.finger.is_none() {
            if let Some((id, from)) = landed {
                self.finger = Some(Resting {
                    id,
                    since: now,
                    from,
                    strayed: 0.0,
                    fired: false,
                });
            }
        }
        (fired, spent)
    }
}

/// [`hand_button`]: a tap turns the hand on or off; a long press or a
/// right-click opens its menu (`menu`, which the library's popover closes).
fn lib_hand(
    screen: egui::Vec2,
    input: &LibInput,
    areas: &mut Vec<egui::Rect>,
    body: &mut BodyControls,
    at: Spots,
    view: &mut OsdView,
    menu: &mut bool,
) {
    use crate::systems::held_phone::Side;
    use crate::systems::high_precision::{HandWord, HighPrecision};
    let stand_in = body.held.stand_in().is_some();
    if !HighPrecision::available() && !stand_in {
        return;
    }
    let fingers = &input.fingers;
    let word = body
        .high
        .hand_word()
        .or_else(|| stand_in.then_some(HandWord::Following));
    let on = word.is_some();
    let tone = match word {
        Some(HandWord::Following) => Tone::Bright,
        Some(HandWord::CameraBusy) => Tone::Warn,
        _ => Tone::Ring,
    };
    let rect = from_right_bottom(screen, at.hand, egui::Vec2::splat(CONTROL_SIZE));
    let hit = reach(rect, true, false);
    // The slop is the touchscreen's, in its own points.
    let slop = crate::systems::touch::HOLD_SLOP_PTS / input.ui_scale.max(f32::EPSILON);
    let (long, spent) = body.hand_press.step(fingers, hit, input.now, slop);
    let tapped = fingers.released.iter().any(|(id, p)| {
        hit.contains(*p)
            && Some(*id) != spent
            && (*id != MOUSE || fingers.mouse_from.is_some_and(|f| hit.contains(f)))
    });
    if tapped && HighPrecision::available() {
        body.tap_hand();
    }
    if long || input.secondary.is_some_and(|p| rect.contains(p)) {
        *menu = true;
    }
    view.discs.push(DiscView {
        disc: input.disc(rect, on || fingers.pressing(hit)),
        mark: Mark::Hand,
        tone,
    });
    let (c, radius) = (rect.center(), CONTROL_SIZE * 0.5 - 1.0);
    view.hand_word = word.map(|w| HandWordView {
        right: c.x - radius - 8.0,
        y: c.y,
        text: w.text(),
        tone,
    });
    if *menu {
        view.hand_menu = Some(HandMenuView {
            disc: rect,
            left: body.held.side == Side::Left,
            can_restart: body.high.on,
            problem: body.high.problem(),
        });
    }
    areas.push(rect);
}

/// The scales' list row width: the name at `SIZE_BODY`, the disc and the
/// padding, as egui's rows. Without egui's fonts the name is measured at
/// 0.55 em a letter, and every row takes the widest's width, which keeps
/// their discs in one column whatever the font the library draws with.
fn list_row_width(h: f32) -> f32 {
    use crate::systems::zoom_rail::Scale;
    let widest = Scale::ALL
        .iter()
        .map(|s| s.label().chars().count())
        .max()
        .unwrap_or(0);
    widest as f32 * theme::SIZE_BODY * 0.55 + h + 24.0
}

/// [`zoom_rail_control`], rule for rule: the scale disc and its list, −,
/// the track, +.
#[allow(clippy::too_many_arguments)]
fn lib_rail(
    screen: egui::Vec2,
    input: &LibInput,
    areas: &mut Vec<egui::Rect>,
    rail: &RailIn,
    state: &mut RailState,
    touch_ui: bool,
    touching: bool,
    view: &mut OsdView,
) -> (f32, Option<f64>) {
    use crate::systems::zoom_rail::{altitude_at, index_of, position_of, step_altitude, Scale};
    let fingers = &input.fingers;
    let now = input.now;
    let (alt, scale) = (rail.alt, rail.scale);
    let steps = scale.steps();
    let mut wish = 0.0;
    let mut goal = None;
    let mut live = RailLive::default();
    let track_len = if touch_ui {
        RAIL_TRACK_TOUCH
    } else {
        RAIL_TRACK
    };
    let current = (scale, index_of(scale, alt));
    if current != state.last_step || rail.flying_to.is_some() {
        state.last_step = current;
        state.active_at = now;
    }
    // Presses that landed this frame, rather than slid in from the map.
    let landed: Vec<(u64, egui::Pos2)> = fingers
        .held
        .iter()
        .filter(|(id, _)| !state.seen.contains(id))
        .copied()
        .collect();
    state.seen = fingers.held.iter().map(|(id, _)| *id).collect();
    state.landed_at.retain(|(id, _)| {
        fingers.held.iter().any(|(f, _)| f == id) || fingers.released.iter().any(|(f, _)| f == id)
    });
    state
        .landed_at
        .extend(landed.iter().map(|(id, _)| (*id, now)));
    // When each end began to offer the next scale: a press from before then
    // is the − or + held into it, not a tap on the scale.
    let pos_now = position_of(scale, alt);
    let edge = [
        (pos_now <= 0.005 || rail.push > 0.0) && scale.outward().is_some(),
        (pos_now >= 0.995 || rail.push < 0.0) && scale.inward().is_some(),
    ];
    for (i, on) in edge.into_iter().enumerate() {
        state.edge_since[i] = match (on, state.edge_since[i]) {
            (true, Some(t)) => Some(t),
            (true, None) => Some(now),
            (false, _) => None,
        };
    }
    // Esc, or a press anywhere but the list and its disc, folds the list.
    if state.list_open {
        let keep = state.list_rect;
        let outside = |p: egui::Pos2| !keep.is_some_and(|r| r.contains(p));
        if input.press.is_some_and(|p| outside(p))
            || landed.iter().any(|(_, p)| outside(*p))
            || input.escape
        {
            state.list_open = false;
        }
    }

    // Where egui's area stood: at the foot of the right edge on a phone,
    // under the map's column of round buttons on a desktop.
    let disc = egui::Vec2::splat(RAIL_DISC);
    let size = egui::vec2(RAIL_DISC, 3.0 * RAIL_DISC + track_len + 3.0 * RAIL_SPACING);
    let column = if touch_ui {
        from_right_bottom(screen, egui::vec2(theme::OSD_MARGIN, bottom_offset()), size)
    } else {
        egui::Rect::from_min_size(
            egui::pos2(screen.x - theme::OSD_MARGIN - RAIL_DISC, rail.top),
            size,
        )
    };
    let pitch = RAIL_DISC + RAIL_SPACING;
    let scale_disc = egui::Rect::from_min_size(column.min, disc);
    let minus = egui::Rect::from_min_size(column.min + egui::vec2(0.0, pitch), disc);
    let track = egui::Rect::from_min_size(
        column.min + egui::vec2(0.0, 2.0 * pitch),
        egui::vec2(RAIL_DISC, track_len),
    );
    let plus =
        egui::Rect::from_min_size(egui::pos2(column.min.x, track.max.y + RAIL_SPACING), disc);

    // A mouse over the rail wakes it; a finger has no hover.
    if (!touching && input.hovers(column.expand(8.0))) || state.list_open {
        state.active_at = now;
    }
    let dt = (now - state.faded_at).clamp(0.0, 0.25) as f32;
    state.faded_at = now;
    let awake = fade(
        &mut state.awake_run,
        now - state.active_at < RAIL_AWAKE_SECS,
        0.4,
        dt,
    );
    live.opacity = q_unit(RAIL_REST_OPACITY + (1.0 - RAIL_REST_OPACITY) * awake);

    // A tap that began after `since`: a press that landed and lifted inside
    // `hit` — the mouse's only if it landed there too.
    let tapped = |hit: egui::Rect, since: f64| {
        fingers.released.iter().any(|(id, p)| {
            hit.contains(*p)
                && (*id != MOUSE || fingers.mouse_from.is_some_and(|f| hit.contains(f)))
                && state.landed_at.iter().any(|(f, t)| f == id && *t >= since)
        })
    };

    // The scale disc.
    let scale_hit = reach(scale_disc, true, false);
    if fingers.tapped(scale_hit) {
        state.list_open = !state.list_open;
    }
    let scale_state = input.disc(scale_disc, state.list_open || fingers.pressing(scale_disc));

    // − at the top, or the next scale out at the end; + at the bottom.
    let end = |rect: egui::Rect, up: bool| {
        let i = usize::from(!up);
        let next = if up { scale.outward() } else { scale.inward() }.filter(|_| edge[i]);
        let hit = reach(rect, false, !up);
        let pressed = fingers.pressing(hit);
        let tap = next.is_some() && state.edge_since[i].is_some_and(|since| tapped(hit, since));
        (input.disc(rect, pressed), next, pressed, tap)
    };
    let (out_disc, out_next, pressed, tap) = end(minus, true);
    if pressed {
        wish -= 1.0;
    }
    live.push[0] = q_unit(out_next.map_or(0.0, |_| rail.push.max(0.0)));
    if tap {
        goal = scale.across(true);
    }

    // The track: a finger that landed there, alone; the mouse at once.
    let y_at = |pos: f32| track.top() + pos * track_len;
    let grab = track.expand(6.0);
    if state.finger.is_none() {
        if let Some((id, p)) = landed.iter().find(|(_, p)| grab.contains(*p)) {
            state.finger = Some(RailFinger {
                id: *id,
                since: now,
                from: *p,
                live: *id == MOUSE,
            });
        }
    }
    let (mut drag_at, mut tap_at) = (None, None);
    if let Some(mut f) = state.finger.take() {
        let alone = fingers.free == 0 && fingers.held.len() <= 1;
        match fingers.at(f.id) {
            Some(p) if f.id == MOUSE => {
                drag_at = Some(p);
                state.finger = Some(f);
            }
            Some(p) => {
                // A second finger anywhere makes it a pinch, not the
                // rail's: let go of it for good.
                if alone {
                    f.live |=
                        now - f.since >= RAIL_GRAB_SECS || (p.y - f.from.y).abs() >= RAIL_GRAB_SLOP;
                    if f.live {
                        drag_at = Some(p);
                    }
                    state.finger = Some(f);
                }
            }
            // The mouse's click, as egui's: a press that did not move.
            None if f.id == MOUSE => {
                tap_at = fingers
                    .released
                    .iter()
                    .find(|(id, _)| *id == MOUSE)
                    .map(|(_, p)| *p)
                    .filter(|p| p.distance(f.from) < CLICK_SLOP);
            }
            // Lifted before it took the track: a tap.
            None if alone && !f.live => {
                tap_at = fingers
                    .released
                    .iter()
                    .find(|(id, _)| *id == f.id)
                    .map(|(_, p)| *p);
            }
            None => {}
        }
    }
    let rail_at = |p: egui::Pos2| altitude_at(scale, (p.y - track.top()) / track_len);
    if let Some(p) = drag_at {
        goal = Some(rail_at(p));
        state.active_at = now;
    }
    // A tap goes to the step it landed on, where its places are a
    // comfortable size; a drag goes to the height under it.
    if let Some(p) = tap_at {
        goal = Some(step_altitude(scale, index_of(scale, rail_at(p))));
        state.active_at = now;
    }
    let thumb_y = y_at(pos_now);
    live.thumb_y = q(thumb_y);
    live.ring_y = rail
        .flying_to
        .filter(|to| Scale::of(*to) == scale)
        .map(|to| q(y_at(position_of(scale, to))));
    // The step's name beside the thumb, while the rail is awake.
    let shown = fade(
        &mut state.shown_run,
        now - state.active_at < RAIL_AWAKE_SECS,
        0.25,
        dt,
    );
    state.shown = shown;
    live.name = (shown > 0.0).then(|| (steps[current.1].label, q_unit(shown)));

    let (in_disc, in_next, pressed, tap) = end(plus, false);
    if pressed {
        wish += 1.0;
    }
    live.push[1] = q_unit(in_next.map_or(0.0, |_| (-rail.push).max(0.0)));
    if tap {
        goal = scale.across(false);
    }

    // The four scales, opened from the disc: beside it, outermost on top.
    state.list_rect = None;
    let mut list = None;
    if state.list_open {
        let h = RAIL_DISC + 4.0;
        let w = list_row_width(h);
        let n = Scale::ALL.len() as f32;
        let frame = egui::Rect::from_min_size(
            egui::pos2(scale_disc.left() - 8.0 - w, scale_disc.top()),
            egui::vec2(w, n * h + (n - 1.0) * RAIL_SPACING),
        );
        let mut rows = Vec::new();
        for (k, s) in Scale::ALL.into_iter().rev().enumerate() {
            let rect = egui::Rect::from_min_size(
                frame.min + egui::vec2(0.0, k as f32 * (h + RAIL_SPACING)),
                egui::vec2(w, h),
            );
            let on = s == scale;
            rows.push(ListRow {
                rect,
                scale: s,
                on,
                hovered: input.hovers(rect),
            });
            if fingers.tapped(rect) {
                goal = Some(if on {
                    s.overview()
                } else {
                    s.arrival(rail.globe_return)
                });
                state.list_open = false;
                state.active_at = now;
            }
        }
        list = Some(rows);
        areas.push(frame);
        state.list_rect = Some(frame.union(scale_disc));
    }
    if wish != 0.0 {
        state.active_at = now;
    }
    // The name is not a control: only the rail itself is claimed, so a pinch
    // beside it keeps both its fingers.
    areas.push(column);
    view.rail = Some(RailFrame {
        column,
        scale,
        scale_disc: scale_state,
        ends: [(out_disc, out_next), (in_disc, in_next)],
        track,
        steps: steps.len(),
        list,
    });
    view.rail_live = live;
    (wish, goal)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Claimed fingers at `(id, position)`, in egui points.
    fn fingers(held: &[(u64, (f32, f32))], released: &[(f32, f32)]) -> Fingers {
        Fingers {
            held: held
                .iter()
                .map(|(id, (x, y))| (*id, egui::pos2(*x, *y)))
                .collect(),
            released: released
                .iter()
                .map(|(x, y)| (0, egui::pos2(*x, *y)))
                .collect(),
            free: 0,
            mouse_from: None,
        }
    }

    /// The square the well is drawn in, as `thumbstick` allocates it.
    fn well() -> egui::Rect {
        egui::Rect::from_center_size(
            egui::pos2(100.0, 700.0),
            egui::Vec2::splat(STICK_RADIUS * 2.0),
        )
    }

    /// The stick takes the finger that lands in its well — no egui pointer
    /// involved, which is what lets a second finger turn the view at the same
    /// time.
    #[test]
    fn the_stick_adopts_a_finger_that_lands_in_the_well() {
        let held = fingers(&[(3, (110.0, 710.0))], &[]);
        assert_eq!(stick_owner(None, &held, well()), Some(3));
        // A finger elsewhere on the glass is not its business, even though it is
        // claimed by *some* control.
        let elsewhere = fingers(&[(3, (400.0, 300.0))], &[]);
        assert_eq!(stick_owner(None, &elsewhere, well()), None);
    }

    /// Pushing past the rim is how full throttle is asked for, so the claim
    /// survives leaving the well and ends only when the finger lifts.
    #[test]
    fn the_stick_holds_its_finger_past_the_rim_and_drops_it_on_lift() {
        let strayed = fingers(&[(3, (300.0, 500.0))], &[]);
        assert_eq!(stick_owner(Some(3), &strayed, well()), Some(3));
        assert_eq!(stick_owner(Some(3), &fingers(&[], &[]), well()), None);
    }

    /// A second finger in the well while the first still steers changes nothing
    /// — a stick with two owners would jump between them every frame.
    #[test]
    fn the_stick_does_not_change_hands_mid_push() {
        let both = fingers(&[(3, (300.0, 500.0)), (4, (105.0, 705.0))], &[]);
        assert_eq!(stick_owner(Some(3), &both, well()), Some(3));
    }

    /// The round controls hit-test the same fingers: a rate reads the held set,
    /// an edge the released one, and neither is the other's answer.
    #[test]
    fn a_disc_reads_a_held_finger_and_a_lifted_one_separately() {
        let disc = egui::Rect::from_min_size(egui::pos2(300.0, 700.0), egui::Vec2::splat(60.0));
        let resting = fingers(&[(1, (330.0, 730.0))], &[]);
        assert!(resting.pressing(disc));
        assert!(!resting.lifted_in(disc), "still down is not a release");
        let lifted = fingers(&[], &[(330.0, 730.0)]);
        assert!(!lifted.pressing(disc));
        assert!(lifted.lifted_in(disc));
        // …and a finger on the neighbouring disc is neither.
        let neighbour = fingers(&[(1, (330.0, 630.0))], &[(330.0, 630.0)]);
        assert!(!neighbour.pressing(disc) && !neighbour.lifted_in(disc));
    }

    /// The claimed regions are published in the window's points; egui laid them
    /// out in its own, and on the touch tier those differ by ~1.5×. Publishing
    /// the egui rect raw would put the hit area at two thirds of the control.
    #[test]
    fn the_published_region_is_scaled_out_of_egui_points() {
        let r = to_window(
            egui::Rect::from_min_max(egui::pos2(10.0, 20.0), egui::pos2(30.0, 60.0)),
            1.5,
        );
        assert_eq!(r.min, Vec2::new(15.0, 30.0));
        assert_eq!(r.max, Vec2::new(45.0, 90.0));
    }

    /// Follow gets the stick; Earth, whose whole vocabulary is
    /// drag/pinch/twist, gets the zoom pair alone; the placed view, which
    /// moves nowhere, gets nothing.
    #[test]
    fn the_translating_modes_get_the_stick() {
        let touch = |mode| cluster_for(mode, false, false, false, true);
        assert_eq!(touch(CameraMode::Free), None);
        assert_eq!(touch(CameraMode::Avatar), Some(Cluster::Walk));
        assert_eq!(touch(CameraMode::Orbit), Some(Cluster::Zoom));
    }

    /// A mouse-driven run gets no on-screen controls at all: they would be
    /// chrome over the map for a device that has a wheel and a keyboard.
    #[test]
    fn a_pointer_run_draws_nothing() {
        assert_eq!(
            cluster_for(CameraMode::Free, false, false, false, false),
            None
        );
    }

    /// All three suppressions exist because egui paints a floating `Area` over
    /// whatever is under it — a control left visible there would take taps meant
    /// for the panel, the keys or the driver chrome beneath.
    #[test]
    fn driving_a_panel_or_the_keyboard_suppresses_the_controls() {
        assert_eq!(
            cluster_for(CameraMode::Free, true, false, false, true),
            None
        );
        assert_eq!(
            cluster_for(CameraMode::Free, false, true, false, true),
            None
        );
        assert_eq!(
            cluster_for(CameraMode::Free, false, false, true, true),
            None
        );
    }

    /// The right edge is **one** column that grows upwards from the dock line,
    /// not two clusters at unrelated heights. Both pairs are anchored
    /// `RIGHT_BOTTOM`, so this measures the two spans they claim above the
    /// bottom edge and asserts they are disjoint with exactly a group gap
    /// between them — the arithmetic the two anchors are written from.
    #[test]
    fn the_right_edge_clusters_stack_without_overlapping() {
        let pair = (bottom_offset(), bottom_offset() + pair_height());
        let actions = (
            actions_bottom_offset(),
            actions_bottom_offset() + CONTROL_SIZE,
        );
        assert!(
            actions.0 >= pair.1,
            "the jump disc starts at {} inside a zoom pair reaching {}",
            actions.0,
            pair.1
        );
        assert!((actions.0 - pair.1 - theme::OSD_GROUP_GAP).abs() < 1e-3);
        // …and the bottom-most of them clears the dock, which owns that edge.
        assert!(pair.0 >= crate::systems::dock::reserved_bottom());
    }

    /// The right edge has two owners since 2026-11-10: the map's column of
    /// round buttons comes down from under the search bar
    /// (`dock::right_column`), and the touch controls grow up from the
    /// World | Community switch. They must not meet — neither Follow's
    /// tallest stack nor Earth's rail — on a phone upright (274 × 600 pt, the
    /// shots' 412×900 px at 1.5) or on its side (the OnePlus 15's 2772×1272
    /// px at 3 px a point, where the column stood on the jump disc until
    /// 2026-11-12).
    #[test]
    fn the_right_edge_controls_clear_the_column_of_round_buttons() {
        for view in [egui::vec2(274.0, 600.0), egui::vec2(924.0, 424.0)] {
            let lying = crate::systems::dock::lying(view);
            let column = crate::systems::dock::right_column(view, true, true).bottom;
            let follow_top = view.y - right_edge_reach(lying);
            // Real room, not a hairline: the group gap (2026-11-11, it was 8 pt).
            assert!(
                follow_top >= column + theme::OSD_GROUP_GAP,
                "{view:?}: Follow's stack reaches {follow_top}, the column ends at {column}"
            );
            let rail = 3.0 * RAIL_DISC + RAIL_TRACK_TOUCH + 3.0 * 6.0;
            let rail_top = view.y - bottom_offset() - rail;
            assert!(
                rail_top >= column + theme::OSD_GROUP_GAP,
                "{view:?}: the rail reaches {rail_top}, the column ends at {column}"
            );
            // The face and hand discs stand beside the jump disc, their miss
            // bands apart from its own; lying, the jump disc stands beside the
            // zoom pair the same way.
            let at = spots(lying);
            assert!(at.face.x - at.actions.x - CONTROL_SIZE > 2.0 * MISS_MARGIN);
            assert!(at.face.y + CONTROL_SIZE <= right_edge_reach(lying));
            assert!(at.hand.y >= at.actions.y);
            if lying {
                assert!(at.actions.x - theme::OSD_MARGIN - CONTROL_SIZE > 2.0 * MISS_MARGIN);
                // …and all three pairs keep clear of the switch between them.
                let switch_right = (view.x + crate::systems::dock::SWITCH_MAX_W) / 2.0;
                let reach_in = at.face.x + CONTROL_SIZE + MISS_MARGIN;
                assert!(view.x - reach_in > switch_right, "{view:?}");
            }
        }
        // A phone upright keeps its column, and on its side lies down.
        assert!(!crate::systems::dock::lying(egui::vec2(274.0, 600.0)));
        assert!(crate::systems::dock::lying(egui::vec2(924.0, 424.0)));
        // The shortest phone on its side (274 pt) still has room for Follow's
        // pairs under the rows; Earth's rail does not fit there yet.
        let short = egui::vec2(600.0, 274.0);
        let rows = crate::systems::dock::right_column(short, true, true).bottom;
        assert!(short.y - right_edge_reach(true) >= rows + theme::OSD_GROUP_GAP);
    }

    /// What the bottom-left corner reserves has to cover the stick actually
    /// drawn there, or the scale bar that reads this lands on it — which is the
    /// collision that moved the bar off the opposite corner in the first place.
    /// The well stands under the thumb that took it, along the band, and never
    /// past either end of it — the band's height is the well's, so it does not
    /// move up or down.
    #[test]
    fn the_stick_floats_to_the_thumb_inside_its_band() {
        let size = STICK_RADIUS * 2.0;
        let zone = egui::Rect::from_min_size(egui::pos2(12.0, 500.0), egui::vec2(size * 2.2, size));
        let mid = stick_origin(zone, egui::pos2(130.0, 520.0));
        assert_eq!(mid, egui::pos2(130.0, zone.center().y));
        assert_eq!(
            stick_origin(zone, egui::pos2(0.0, 560.0)).x,
            zone.left() + STICK_RADIUS
        );
        assert_eq!(
            stick_origin(zone, egui::pos2(900.0, 560.0)).x,
            zone.right() - STICK_RADIUS
        );
        // A band no wider than the well keeps it where it rests.
        let narrow = egui::Rect::from_min_size(zone.min, egui::Vec2::splat(size));
        assert_eq!(
            stick_origin(narrow, egui::pos2(300.0, 0.0)),
            narrow.center()
        );
    }

    #[test]
    fn the_bottom_left_reservation_covers_the_stick() {
        let stick_top = bottom_offset() + STICK_RADIUS * 2.0;
        for cluster in [Cluster::Walk] {
            assert!(
                bottom_left_reserved(Some(cluster)) >= stick_top,
                "{cluster:?} draws a stick reaching {stick_top} and reserves {}",
                bottom_left_reserved(Some(cluster))
            );
        }
        // Earth mode draws no stick, and a pointer run draws nothing at all —
        // in both the corner is the dock's alone, and reserving the stick's
        // space anyway would float the scale bar over empty map.
        let bare = crate::systems::dock::reserved_bottom();
        assert_eq!(bottom_left_reserved(Some(Cluster::Zoom)), bare);
        assert_eq!(bottom_left_reserved(None), bare);
    }

    /// A grab that lands a few points beside the well is the ordinary way a
    /// thumb misses; it must work the stick, not become a camera gesture.
    #[test]
    fn a_near_miss_beside_the_well_still_takes_the_stick() {
        let w = well();
        let beside = fingers(&[(3, (w.min.x - MISS_MARGIN * 0.5, w.center().y))], &[]);
        assert_eq!(
            stick_owner(None, &beside, w.expand(MISS_MARGIN)),
            Some(3),
            "the miss band adopts"
        );
        let far = fingers(&[(3, (w.min.x - MISS_MARGIN * 3.0, w.center().y))], &[]);
        assert_eq!(stick_owner(None, &far, w.expand(MISS_MARGIN)), None);
    }

    /// The miss band grows a disc's outer sides only: the gap between the two
    /// discs of a pair stays dead, because a margin there would press (or click)
    /// both verbs from one finger.
    #[test]
    fn the_miss_band_stops_at_a_siblings_edge() {
        let disc = egui::Rect::from_min_size(egui::pos2(300.0, 700.0), egui::Vec2::splat(40.0));
        let top = reach(disc, true, false);
        assert_eq!(top.min.y, disc.min.y - MISS_MARGIN, "outer side forgives");
        assert_eq!(top.max.y, disc.max.y, "sibling side is exact");
        assert_eq!(top.min.x, disc.min.x - MISS_MARGIN);
        assert_eq!(top.max.x, disc.max.x + MISS_MARGIN);
        let bottom = reach(disc, false, true);
        assert_eq!(bottom.min.y, disc.min.y);
        assert_eq!(bottom.max.y, disc.max.y + MISS_MARGIN);
    }

    /// The bands must fit the gaps they sit in: two padded neighbours in the
    /// right-edge column may not overlap (one lift would fire two verbs), and
    /// the claim padding may not reach the dock (a claimed finger is invisible
    /// to the emulated mouse, so it would cost a dock disc its tap).
    #[test]
    fn the_miss_bands_fit_between_the_clusters_and_off_the_dock() {
        assert!(MISS_MARGIN * 2.0 < theme::OSD_GROUP_GAP);
        assert!(CLAIM_PAD <= theme::OSD_MARGIN);
    }

    #[test]
    fn the_stick_is_centred_inside_its_deadzone() {
        assert_eq!(stick_wish(Vec2::new(3.0, 3.0), 50.0), Vec2::ZERO);
    }

    /// Up-screen is forward, and the reading is proportional rather than a
    /// direction — half a push is half the speed.
    #[test]
    fn pushing_the_stick_up_is_forward_and_proportional() {
        let half = stick_wish(Vec2::new(0.0, -25.0), 50.0);
        assert!((half.y - 0.5).abs() < 1e-5, "half push = {half:?}");
        assert_eq!(half.x, 0.0);
    }

    /// Clamped to the well, not to the square around it: otherwise the diagonal
    /// is √2 faster than any axis and the stick steers differently depending on
    /// which way you already point.
    #[test]
    fn the_stick_clamps_to_the_disc() {
        let corner = stick_wish(Vec2::new(200.0, -200.0), 50.0);
        assert!((corner.length() - 1.0).abs() < 1e-5, "{corner:?}");
        assert!((corner.x - corner.y).abs() < 1e-5, "still on the diagonal");
    }

    /// The screen the rail harness draws on, egui points.
    const SCREEN: egui::Vec2 = egui::vec2(400.0, 800.0);

    /// Drives the zoom rail on a phone-sized touch screen, one frame at a
    /// time: egui's edition, or the library's (`lib`).
    struct Rail {
        ctx: egui::Context,
        state: RailState,
        t: f64,
        /// The camera's height the rail is drawn for.
        alt: f64,
        /// The library's edition, and what it was handed last frame.
        lib: bool,
        view: OsdView,
    }

    impl Rail {
        fn new(lib: bool) -> Self {
            let mut rail = Rail {
                ctx: egui::Context::default(),
                state: RailState::default(),
                t: 0.0,
                alt: 1e6,
                lib,
                view: OsdView::default(),
            };
            // Let the layout settle, and the rail fall asleep, before any
            // finger arrives.
            for _ in 0..60 * 3 {
                rail.frame(&fingers(&[], &[]), 0);
            }
            rail
        }

        /// One frame at 60 fps with these fingers and `free` map fingers down:
        /// the height the rail asked for, if any.
        fn frame(&mut self, f: &Fingers, free: usize) -> Option<f64> {
            self.run(f, free).0
        }

        fn run(&mut self, f: &Fingers, free: usize) -> (Option<f64>, egui::Rect) {
            use crate::systems::egui_manager::UnpaintedPasses;
            let f = Fingers {
                held: f.held.clone(),
                released: f.released.clone(),
                free,
                mouse_from: f.mouse_from,
            };
            self.t += 1.0 / 60.0;
            let (mut goal, mut areas) = (None, Vec::new());
            let touching = f.held.iter().any(|(id, _)| *id != MOUSE) || f.free > 0;
            let rail = RailIn {
                alt: self.alt,
                scale: crate::systems::zoom_rail::Scale::of(self.alt),
                flying_to: None,
                push: 0.0,
                globe_return: None,
                top: 0.0,
            };
            if self.lib {
                let input = LibInput {
                    fingers: f,
                    hover: None,
                    press: None,
                    secondary: None,
                    escape: false,
                    now: self.t,
                    ui_scale: 1.0,
                };
                self.view = OsdView::default();
                goal = lib_rail(
                    SCREEN,
                    &input,
                    &mut areas,
                    &rail,
                    &mut self.state,
                    true,
                    touching,
                    &mut self.view,
                )
                .1;
            } else {
                let input = egui::RawInput {
                    time: Some(self.t),
                    screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, SCREEN)),
                    ..Default::default()
                };
                let state = &mut self.state;
                self.ctx.run_ui_unpainted(input, |ui| {
                    goal =
                        zoom_rail_control(ui.ctx(), &f, &mut areas, &rail, state, true, touching).1;
                });
            }
            // The rail is the last area claimed; the scale list, if open,
            // comes before it.
            (goal, *areas.last().unwrap())
        }

        /// A point on the track, `at` of the way down it: under the scale
        /// disc and the −.
        fn on_track(&mut self, at: f32) -> (f32, f32) {
            let r = self.run(&fingers(&[], &[]), 0).1;
            let (top, bottom) = (
                r.top() + 2.0 * RAIL_DISC + 12.0,
                r.bottom() - RAIL_DISC - 6.0,
            );
            (r.right() - RAIL_DISC / 2.0, top + (bottom - top) * at)
        }

        /// The middle of the disc `k` from the top: 0 the scale disc, 1 the −.
        fn disc(&mut self, k: f32) -> (f32, f32) {
            let r = self.run(&fingers(&[], &[]), 0).1;
            (
                r.right() - RAIL_DISC / 2.0,
                r.top() + RAIL_DISC / 2.0 + k * (RAIL_DISC + 6.0),
            )
        }

        /// A finger that lands at `p`, stays two frames, and lifts.
        fn tap(&mut self, p: (f32, f32)) -> Option<f64> {
            let on = fingers(&[(7, p)], &[]);
            let mut goal = self.frame(&on, 0);
            goal = goal.or(self.frame(&on, 0));
            let lift = Fingers {
                released: vec![(7, egui::pos2(p.0, p.1))],
                ..Default::default()
            };
            goal.or(self.frame(&lift, 0))
        }
    }

    /// Both editions, for the rail's behaviour tests.
    const EDITIONS: [bool; 2] = [false, true];

    /// A pinch whose first finger lands on the track: the second lands two
    /// frames later on the map, and the rail must neither move the height nor
    /// wake its name. It did both until 2026-11-02.
    #[test]
    fn a_pinch_landing_on_the_rail_leaves_it_alone() {
        for lib in EDITIONS {
            let mut rail = Rail::new(lib);
            let p = rail.on_track(0.3);
            let on = fingers(&[(1, p)], &[]);
            assert_eq!(rail.frame(&on, 0), None);
            assert_eq!(rail.frame(&on, 0), None);
            for _ in 0..30 {
                assert_eq!(
                    rail.frame(&on, 1),
                    None,
                    "lib {lib}: the pinch's finger moved the height"
                );
            }
            assert_eq!(rail.frame(&fingers(&[], &[p]), 0), None);
            assert_eq!(
                rail.state.shown, 0.0,
                "lib {lib}: a pinch woke the rail's name"
            );
        }
    }

    /// A finger that lands on the map and slides over the rail is the map's.
    #[test]
    fn a_finger_sliding_in_from_the_map_is_not_the_rails() {
        for lib in EDITIONS {
            let mut rail = Rail::new(lib);
            let (x, y) = rail.on_track(0.5);
            rail.frame(&fingers(&[(1, (x - 120.0, y))], &[]), 0);
            for _ in 0..20 {
                assert_eq!(rail.frame(&fingers(&[(1, (x, y))], &[]), 0), None);
            }
            assert_eq!(rail.state.shown, 0.0, "lib {lib}");
        }
    }

    /// A finger meant for the rail lands on it alone and stays: after a
    /// moment it sets the height, and the rung's name comes out.
    #[test]
    fn a_finger_resting_on_the_track_takes_it() {
        for lib in EDITIONS {
            let mut rail = Rail::new(lib);
            let p = rail.on_track(0.5);
            let on = fingers(&[(1, p)], &[]);
            assert_eq!(rail.frame(&on, 0), None, "not on the frame it landed");
            let goal = (0..20).find_map(|_| rail.frame(&on, 0));
            assert!(
                goal.is_some(),
                "lib {lib}: a resting finger never took the track"
            );
            assert!(rail.state.shown > 0.0, "lib {lib}");
        }
    }

    /// A quick tap on the track, lifted before it took the track, goes to the
    /// rung it tapped.
    #[test]
    fn a_tap_on_the_track_goes_there() {
        use crate::systems::zoom_rail::{index_of, step_altitude, Scale};
        for lib in EDITIONS {
            let mut rail = Rail::new(lib);
            let p = rail.on_track(0.2);
            assert_eq!(rail.frame(&fingers(&[(1, p)], &[]), 0), None);
            let lift = Fingers {
                released: vec![(1, egui::pos2(p.0, p.1))],
                ..Default::default()
            };
            let goal = rail.frame(&lift, 0).expect("the tap went nowhere");
            let up = rail.frame(&fingers(&[], &[]), 0);
            assert_eq!(up, None);
            assert!(
                goal > 1e6,
                "lib {lib}: a tap high on the rail climbs: {goal}"
            );
            // …to the step it landed on, not the point.
            assert_eq!(
                goal,
                step_altitude(Scale::Globe, index_of(Scale::Globe, goal))
            );
        }
    }

    /// The library's edition lays the rail out where egui's area stood, the
    /// parts in order down it, and hands the library all of it.
    #[test]
    fn the_library_edition_lays_the_rail_out_where_egui_did() {
        let mut egui_rail = Rail::new(false);
        let (_, egui_column) = egui_rail.run(&fingers(&[], &[]), 0);
        let mut rail = Rail::new(true);
        let (_, column) = rail.run(&fingers(&[], &[]), 0);
        assert!(
            (column.min - egui_column.min).length() < 0.5
                && (column.max - egui_column.max).length() < 0.5,
            "the library's rail {column:?}, egui's {egui_column:?}"
        );
        let v = rail.view.clone();
        let f = v.rail.expect("no rail handed over");
        assert_eq!(f.column, column);
        let [(minus, _), (plus, _)] = f.ends;
        assert!(f.scale_disc.rect.max.y <= minus.rect.min.y);
        assert!(minus.rect.max.y <= f.track.min.y);
        assert!(f.track.max.y <= plus.rect.min.y);
        assert!(column.contains_rect(f.track) && column.contains_rect(plus.rect));
        let y = v.rail_live.thumb_y;
        assert!(y >= f.track.top() - 0.5 && y <= f.track.bottom() + 0.5);
        assert!(
            v.rail_live.opacity > 0.0,
            "a resting rail is faint, not gone"
        );
    }

    /// The mouse takes the track the moment it presses there (no pinch to
    /// wait out), and a click — a press that did not move — goes to the step.
    #[test]
    fn the_mouse_drags_the_track_at_once_and_clicks_to_a_step() {
        use crate::systems::zoom_rail::{index_of, step_altitude, Scale};
        let mut rail = Rail::new(true);
        let (x, y) = rail.on_track(0.5);
        let press = |p: (f32, f32)| Fingers {
            held: vec![(MOUSE, egui::pos2(p.0, p.1))],
            mouse_from: Some(egui::pos2(x, y)),
            ..Default::default()
        };
        assert!(
            rail.frame(&press((x, y)), 0).is_some(),
            "the press did nothing"
        );
        let dragged = rail
            .frame(&press((x, y - 30.0)), 0)
            .expect("the drag did nothing");
        let lift = Fingers {
            released: vec![(MOUSE, egui::pos2(x, y - 30.0))],
            mouse_from: Some(egui::pos2(x, y)),
            ..Default::default()
        };
        assert_eq!(rail.frame(&lift, 0), None, "a drag's lift is no click");
        assert!(dragged > 0.0);

        let (x, y) = rail.on_track(0.2);
        rail.frame(&press((x, y)), 0);
        let click = Fingers {
            released: vec![(MOUSE, egui::pos2(x, y))],
            mouse_from: Some(egui::pos2(x, y)),
            ..Default::default()
        };
        let goal = rail.frame(&click, 0).expect("the click went nowhere");
        assert_eq!(
            goal,
            step_altitude(Scale::Globe, index_of(Scale::Globe, goal))
        );
    }

    /// At the top of the globe the − is the solar system: a tap there goes
    /// over the line.
    #[test]
    fn a_tap_on_the_end_crosses_into_the_next_scale() {
        use crate::systems::zoom_rail::Scale;
        for lib in EDITIONS {
            let mut rail = Rail::new(lib);
            rail.alt = Scale::Globe.bounds().1 * 0.999;
            let minus = rail.disc(1.0);
            let goal = rail.tap(minus).expect("the tap went nowhere");
            assert_eq!(Scale::of(goal), Scale::SolarSystem, "lib {lib}");
        }
    }

    /// A − held into the end of the scale is the zoom pushing on, not a tap
    /// on the next scale when it lets go.
    #[test]
    fn a_minus_held_into_the_end_does_not_jump_when_let_go() {
        use crate::systems::zoom_rail::Scale;
        for lib in EDITIONS {
            let mut rail = Rail::new(lib);
            let minus = rail.disc(1.0);
            let on = fingers(&[(7, minus)], &[]);
            rail.frame(&on, 0);
            rail.alt = Scale::Globe.bounds().1;
            for _ in 0..5 {
                assert_eq!(rail.frame(&on, 0), None);
            }
            let lift = Fingers {
                released: vec![(7, egui::pos2(minus.0, minus.1))],
                ..Default::default()
            };
            assert_eq!(rail.frame(&lift, 0), None, "lib {lib}: letting go crossed");
        }
    }

    /// The scale disc opens the four scales; a tap on one flies to it, and the
    /// list folds.
    #[test]
    fn the_scale_disc_opens_the_scales() {
        use crate::systems::zoom_rail::Scale;
        for lib in EDITIONS {
            let mut rail = Rail::new(lib);
            let disc = rail.disc(0.0);
            assert_eq!(rail.tap(disc), None);
            assert!(rail.state.list_open, "lib {lib}: the list did not open");
            // The list's rows run down from the disc's top, outermost first.
            let row = |k: f32| {
                (
                    disc.0 - RAIL_DISC - 40.0,
                    disc.1 - RAIL_DISC / 2.0 + 2.0 + RAIL_DISC / 2.0 + k * (RAIL_DISC + 10.0),
                )
            };
            let goal = rail.tap(row(0.0)).expect("the row went nowhere");
            assert_eq!(goal, Scale::Universe.overview(), "lib {lib}");
            assert!(!rail.state.list_open, "lib {lib}: the list stayed open");
        }
    }

    /// The library's edition puts the pair, the stick and the jump disc
    /// where egui's anchors put them — the rects the claims are published
    /// from, so a finger lands where it always did.
    #[test]
    fn the_library_edition_lays_out_where_egui_did() {
        use crate::systems::egui_manager::UnpaintedPasses;
        let ctx = egui::Context::default();
        let none = Fingers::default();
        let at = spots(false);
        let mut egui_areas = Vec::new();
        // A few passes, for egui's areas to learn their sizes.
        for t in 0..4 {
            egui_areas.clear();
            let input = egui::RawInput {
                time: Some(f64::from(t) / 60.0),
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, SCREEN)),
                ..Default::default()
            };
            ctx.run_ui_unpainted(input, |ui| {
                let ctx = ui.ctx();
                vertical_pair(
                    ctx,
                    &none,
                    &mut egui_areas,
                    "osd_zoom",
                    Mark::Plus,
                    Mark::Minus,
                );
                let (mut owner, mut origin) = (None, None);
                thumbstick(ctx, &none, &mut owner, &mut origin, &mut egui_areas);
                jump_button(ctx, &none, &mut egui_areas, at);
            });
        }
        let input = LibInput {
            fingers: Fingers::default(),
            hover: None,
            press: None,
            secondary: None,
            escape: false,
            now: 0.0,
            ui_scale: 1.0,
        };
        let (mut areas, mut view) = (Vec::new(), OsdView::default());
        lib_pair(
            SCREEN,
            &input,
            &mut areas,
            Mark::Plus,
            Mark::Minus,
            &mut view,
        );
        let (mut owner, mut origin) = (None, None);
        lib_stick(
            SCREEN,
            &input,
            &mut owner,
            &mut origin,
            &mut areas,
            &mut view,
        );
        lib_jump(SCREEN, &input, &mut areas, at, &mut view);
        assert_eq!(areas.len(), egui_areas.len());
        for (lib, egui) in areas.iter().zip(&egui_areas) {
            assert!(
                (lib.min - egui.min).length() < 0.5 && (lib.max - egui.max).length() < 0.5,
                "the library's {lib:?}, egui's {egui:?}"
            );
        }
        // The discs it hands the library are the ones it hit-tests.
        assert_eq!(view.discs.len(), 3);
        assert_eq!(view.discs[2].disc.rect, areas[2]);
    }

    /// A long press on the hand disc fires once, after the hold, and its lift
    /// is spent — not also a tap; a finger that wanders is no long press.
    #[test]
    fn a_resting_finger_is_a_long_press_and_its_lift_no_tap() {
        let hit = egui::Rect::from_min_size(egui::pos2(300.0, 700.0), egui::Vec2::splat(40.0));
        let secs = f64::from(crate::systems::touch::LONG_PRESS_SECS);
        let mut lp = LongPress::default();
        let on = fingers(&[(4, (320.0, 720.0))], &[]);
        assert_eq!(lp.step(&on, hit, 0.0, 14.0), (false, None));
        assert_eq!(lp.step(&on, hit, secs * 0.5, 14.0), (false, None));
        assert_eq!(lp.step(&on, hit, secs + 0.01, 14.0), (true, None));
        assert_eq!(
            lp.step(&on, hit, secs + 0.2, 14.0),
            (false, None),
            "fires once"
        );
        let lift = Fingers {
            released: vec![(4, egui::pos2(320.0, 720.0))],
            ..Default::default()
        };
        assert_eq!(lp.step(&lift, hit, secs + 0.3, 14.0), (false, Some(4)));

        let mut lp = LongPress::default();
        lp.step(&fingers(&[(5, (320.0, 720.0))], &[]), hit, 0.0, 14.0);
        let wandered = fingers(&[(5, (320.0, 740.0))], &[]);
        assert_eq!(lp.step(&wandered, hit, secs + 0.1, 14.0), (false, None));
    }
}
