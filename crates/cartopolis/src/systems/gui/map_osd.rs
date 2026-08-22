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
//!     camera; the same bottom-right pair, zooming the camera on foot and
//!     climbing in flight; and a jump/flight pair stacked above it. Until this
//!     existed the avatar was
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
//! The camera-mode switcher is *not* here: it is the dock's mode disc
//! (`dock::mode_disc`). It sat in this corner briefly, and what that cost was
//! the corner itself — every pair on this edge stacked one control higher for
//! a disc that is chrome, not a touch control.
//!
//! ## Two rules it obeys, both learned elsewhere in this tree
//!
//! **It draws into floating `Area`s, never the shared root `Ui`**, so it stays
//! out of the Phase 7b panel chain (like the dock and the HUD) — but for the
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
//! [`TouchGestures::ui_held`]: crate::systems::touch::TouchGestures::ui_held
//! [`TouchGestures::ui_released`]: crate::systems::touch::TouchGestures::ui_released

use bevy::prelude::*;

use crate::resources::{CameraMode, CameraState, RightPanel};
use crate::systems::egui_manager::EguiManager;
use crate::systems::theme;

/// Diameter (points) of one round control — [`theme::ROUND_BUTTON_SIZE`], the
/// one disc that floats over this map. 60 dp on the touch tier, where
/// `EguiManager::ui_scale` is ≥1.5 (`docs/notes/touch-input.md`), so it clears
/// the 48 dp guideline with room to spare; it was 44 for the same reason and all
/// that bought was a fourth diameter on a screen that only needed one.
const CONTROL_SIZE: f32 = theme::ROUND_BUTTON_SIZE;
/// Radius (points) of the thumbstick's well, and of the knob inside it.
const STICK_RADIUS: f32 = 54.0;
const STICK_KNOB_RADIUS: f32 = 22.0;

/// Distance from the bottom of the window to the bottom of a cluster: clear of
/// the dock, which owns the bottom edge, plus the standard edge inset.
fn bottom_offset() -> f32 {
    theme::OSD_MARGIN + crate::systems::dock::reserved_bottom()
}

/// Height of a two-button stack.
fn pair_height() -> f32 {
    CONTROL_SIZE * 2.0 + theme::OSD_GAP
}

/// Bottom offset of the jump/flight pair: a group gap above the zoom/climb pair,
/// which owns the corner. Named rather than inlined so the test below measures
/// the same arithmetic the anchor uses.
fn actions_bottom_offset() -> f32 {
    bottom_offset() + pair_height() + theme::OSD_GROUP_GAP
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
        Some(Cluster::Fly | Cluster::Walk) => bottom_offset() + STICK_RADIUS * 2.0,
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
/// between them presses (or worse, *clicks*) both at once, which for the action
/// pair would be a jump and a flight toggle from one lift.
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
    /// Fly translation wish in the camera's horizontal frame: `x` = strafe
    /// right, `y` = forward. Inside the unit disc, and *proportional*: this is
    /// the difference between a stick and four keys.
    pub fly: Vec2,
    /// Fly vertical wish: `+1` climbing, `-1` descending.
    pub climb: f32,
    /// Follow-mode edges: a jump this frame, and a request to toggle flight.
    ///
    /// **Edges, not held states**, and the distinction is the same one the wire
    /// makes (`ClientIntent::jump`, protocol 13): a dropped movement frame is
    /// corrected by the next one, while a dropped jump simply did not happen. So
    /// these are true for exactly the frame the button was released on, and the
    /// consumer must act on them rather than sample them.
    pub jump: bool,
    pub fly_toggle: bool,
    /// Follow-mode travel wish, in the same camera-horizontal frame as [`fly`]
    /// and read by `avatar::handle_avatar_movement` (which then owns it in every
    /// run mode — the online path consumes the same ramp through
    /// `network::send_network_intent`).
    ///
    /// A separate field rather than a second meaning for `fly`: the two have
    /// different consumers — one moves the camera, one moves a body the server
    /// may be authoritative for — and one field with a mode-dependent meaning is
    /// how a future edit gets to fly the camera by walking.
    ///
    /// [`fly`]: Self::fly
    pub walk: Vec2,
}

/// The fingers the on-screen controls own this frame, in **egui** points.
///
/// A projection of `TouchGestures`' claimed sets through `EguiManager::ui_scale`,
/// made once per frame so no control has to know that the touchscreen and egui
/// measure in different points.
#[derive(Default)]
struct Fingers {
    held: Vec<(u64, egui::Pos2)>,
    released: Vec<egui::Pos2>,
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
            released: touch.ui_released().iter().map(|(_, p)| to_ui(*p)).collect(),
        }
    }

    /// Is a claimed finger resting in this rect? The held test, for a rate.
    fn pressing(&self, rect: egui::Rect) -> bool {
        self.held.iter().any(|(_, p)| rect.contains(*p))
    }

    /// Did a claimed finger lift inside this rect this frame? The edge test —
    /// the finger equivalent of `Response::clicked`.
    fn lifted_in(&self, rect: egui::Rect) -> bool {
        self.released.iter().any(|p| rect.contains(*p))
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
    /// Fly: the thumbstick and the climb/descend pair.
    Fly,
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
        Some(Cluster::Fly | Cluster::Walk) => {
            bottom_offset() + STICK_RADIUS * 2.0 + theme::OSD_GROUP_GAP
        }
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
    Some(match mode {
        CameraMode::Free => Cluster::Fly,
        CameraMode::Avatar => Cluster::Walk,
        CameraMode::Orbit => Cluster::Zoom,
    })
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
/// Ordered before `free_camera::camera_controls` in `lib.rs`, so a push is acted
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
pub fn render_map_osd(
    egui: Res<EguiManager>,
    mut osd: ResMut<OsdInput>,
    mut regions: ResMut<crate::systems::touch::UiClaimRegions>,
    mut stick_finger: Local<Option<u64>>,
    camera_state: Res<CameraState>,
    driver: Res<crate::systems::driver::DriverState>,
    right_panel: Res<RightPanel>,
    keyboard: Res<crate::systems::onscreen_keyboard::OnScreenKeyboard>,
    touch: Res<crate::systems::touch::TouchGestures>,
    avatar_state: Res<crate::resources::AvatarState>,
) {
    // Rebuilt from nothing every frame, including the frames that draw nothing —
    // otherwise switching to Earth mode mid-push would leave the last wish
    // latched and the camera would fly on with nothing on screen to stop it.
    // `set_if_neq` at the end so a resting control does not mark the resource
    // changed sixty times a second.
    let mut next = OsdInput::default();
    // The same applies to the claimed regions, and more sharply: a rect left
    // published over a control that is no longer drawn is a piece of the map
    // that silently swallows fingers.
    let mut areas: Vec<egui::Rect> = Vec::new();

    if let Some(cluster) = cluster_for(
        camera_state.mode,
        driver.active,
        !right_panel.is(RightPanel::None),
        keyboard.is_open(),
        theme::touch_layout() || touch.ever(),
    ) {
        let flying = avatar_state.is_flying;
        let ui_scale = egui.ui_scale();
        let fingers = Fingers::new(&touch, ui_scale);
        let ctx = egui.ctx();
        match cluster {
            Cluster::Fly => {
                next.climb =
                    vertical_pair(ctx, &fingers, &mut areas, "osd_climb", Mark::Up, Mark::Down);
                next.fly = thumbstick(ctx, &fingers, &mut stick_finger, &mut areas);
            }
            Cluster::Walk => {
                // The right-edge pair carries whichever vertical the avatar
                // actually has: on foot there is none, so it zooms the camera;
                // in flight it climbs and descends, which is the same control in
                // the same place as Fly mode's — a phone should not have to learn
                // two positions for one verb.
                if flying {
                    next.climb =
                        vertical_pair(ctx, &fingers, &mut areas, "osd_climb", Mark::Up, Mark::Down);
                } else {
                    next.zoom = vertical_pair(
                        ctx,
                        &fingers,
                        &mut areas,
                        "osd_zoom",
                        Mark::Plus,
                        Mark::Minus,
                    );
                }
                next.walk = thumbstick(ctx, &fingers, &mut stick_finger, &mut areas);
                let (jump, fly_toggle) = action_pair(ctx, &fingers, &mut areas, flying);
                next.jump = jump;
                next.fly_toggle = fly_toggle;
            }
            Cluster::Zoom => {
                next.zoom = vertical_pair(
                    ctx,
                    &fingers,
                    &mut areas,
                    "osd_zoom",
                    Mark::Plus,
                    Mark::Minus,
                );
            }
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
        });
    } else {
        *stick_finger = None;
        regions.set_if_neq(crate::systems::touch::UiClaimRegions::default());
    }
    osd.set_if_neq(next);
}

/// An egui rect in the window's own logical points.
fn to_window(r: egui::Rect, ui_scale: f32) -> Rect {
    Rect::from_corners(
        Vec2::new(r.min.x, r.min.y) * ui_scale,
        Vec2::new(r.max.x, r.max.y) * ui_scale,
    )
}

/// Jump and the flight toggle, bottom-right — the verbs a phone had no key for.
///
/// Returns `(jump, fly_toggle)`, each true on the frame its button was
/// *released*. These are the one part of this module that is not a rate: a jump
/// is an event, and `avatar::handle_avatar_movement` consumes them as edges.
/// `clicked()` rather than `is_pointer_button_down_on` for exactly that reason —
/// the held test that suits a rate would fire a jump every frame of a press.
///
/// Bottom-**right**, opposite the stick, because that is where the other thumb
/// is: the same left-translates / right-acts split the module opens with. It
/// stacks *above* the zoom/climb pair, which is the bottom-most thing on that
/// edge in every mode — see [`vertical_pair`] for why the pair rather than this
/// owns the corner.
///
/// Flight is a toggle rather than a hold: holding a button for the duration of a
/// flight is not a control, it is a punishment, and the keyboard's own `F` is a
/// toggle too. The jump button disappears in flight — it would mean nothing
/// there, and a control that is visibly present and does nothing is worse than
/// one that is absent (the same argument this module's suppression rules make).
fn action_pair(
    ctx: &egui::Context,
    fingers: &Fingers,
    areas: &mut Vec<egui::Rect>,
    flying: bool,
) -> (bool, bool) {
    let (mut jump, mut fly_toggle) = (false, false);
    let area = egui::Area::new(egui::Id::new("osd_actions"))
        .anchor(
            egui::Align2::RIGHT_BOTTOM,
            egui::vec2(-theme::OSD_MARGIN, -actions_bottom_offset()),
        )
        .show(ctx, |ui| {
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = theme::OSD_GAP;
                if !flying {
                    // A claimed finger lifting inside the disc is the same event
                    // `clicked()` reports, which is why the two are `or`-ed and
                    // not chosen between: on a phone the first arm fires, on a
                    // desktop (or when this finger also happens to hold egui's
                    // pointer) the second does, and a jump is idempotent either
                    // way.
                    let resp = round_button(ui, Mark::Jump, false);
                    jump = resp.clicked() || fingers.lifted_in(reach(resp.rect, true, false));
                }
                // The flight toggle is a *state*, not a rate or an edge that
                // vanishes — so it wears the engaged plate while flying, the same
                // muted orange an open panel's dock disc wears. It was the one
                // control on the map whose on and off looked identical.
                // In flight the jump disc above is gone, so the top side is
                // outer too and may carry the margin.
                let resp = round_button(ui, Mark::Wings, flying);
                fly_toggle = resp.clicked() || fingers.lifted_in(reach(resp.rect, flying, true));
            });
        });
    areas.push(area.response.rect);
    (jump, fly_toggle)
}

/// The glyphs, painted rather than typed. `✕` taught this lesson once already
/// (`theme::close_button`): the bundled brand font carries no symbol set, so a
/// `+` or a `▲` in a `RichText` is a gamble on a font subset, while two line
/// segments land on the pixel grid at any scale.
#[derive(Clone, Copy)]
enum Mark {
    Plus,
    Minus,
    Up,
    Down,
    /// Jump: a chevron with a bar under it — an arrow leaving the ground.
    Jump,
    /// Flight: a swept delta, filled.
    ///
    /// It was two strokes rising from a common point over a bar, and on the
    /// screen that was an argument from the wrong premise — the marks around it
    /// are strokes, so a stroke mark was said to be the vocabulary. But it sits
    /// directly above the climb pair, whose marks are a `^` and a `v`, and two
    /// strokes rising from a point *is* a `v`. Four discs in a column then read
    /// as two up/down pairs, one of which silently did something else. A filled
    /// dart is the one shape in this set that cannot be read as a chevron, and
    /// being filled is also the honest signal: this is the only control here that
    /// holds a state rather than expressing a rate.
    Wings,
}

/// Two round buttons at the bottom of the right edge. Returns `+1` while the top
/// one is held, `-1` while the bottom one is, `0` otherwise — *held*, not
/// clicked, because these are rates.
///
/// **The pair is the bottom-most thing on that edge in every mode**, and that is
/// the whole of its position rule. It used to be anchored `RIGHT_CENTER`, i.e.
/// halfway up the window, while the jump/flight pair sat in the corner below it:
/// two clusters of identical discs on one edge at two unrelated heights, the
/// busier of them out of a thumb's reach on a phone held in one hand, and — since
/// the scale bar was also anchored to that corner — one of them overlapping a
/// readout it painted over. Bottom-anchored, the right edge is a single column
/// that grows upwards, mirroring the stick opposite it, and the pair keeps one
/// position across Fly, Follow, Follow-in-flight and Earth. That last part is the
/// same argument the module makes for swapping zoom for climb *inside* the pair:
/// a phone should not have to learn two positions for one verb.
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
/// at rest against the dock's `TEXT`, no lift on hover, `ACCENT_BRIGHT` for the
/// pressed ring against the dock's `ACCENT_RING` (which the palette notes
/// measures 4.2:1 on the muted fill where the bright one measures 2.7).
fn round_button(ui: &mut egui::Ui, mark: Mark, on: bool) -> egui::Response {
    let (resp, face) = theme::map_disc(
        ui,
        CONTROL_SIZE,
        on,
        theme::ACCENT_RING,
        egui::Sense::click_and_drag(),
    );
    paint_mark(
        ui.painter(),
        face.center,
        CONTROL_SIZE * 0.22,
        mark,
        face.ink,
    );
    resp
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
        // A swept delta, drawn as its two halves: the dart has a notched tail,
        // so it is not convex, and `convex_polygon` is the only filled primitive
        // egui tessellates correctly. Each half is a triangle and each triangle
        // is convex, and they share the nose and the notch, so the seam between
        // them lands on the axis where a dart has a crease anyway.
        Mark::Wings => {
            let (nose, tip, notch) = (s * 1.15, s * 0.75, s * 0.3);
            let apex = c + egui::vec2(0.0, -nose);
            let tail = c + egui::vec2(0.0, notch);
            for side in [-1.0_f32, 1.0] {
                p.add(egui::Shape::convex_polygon(
                    vec![apex, c + egui::vec2(side * s, tip), tail],
                    ink,
                    egui::Stroke::NONE,
                ));
            }
        }
        // Chevrons, not filled triangles: at 44 pt a solid arrowhead reads as a
        // blob, and the same two segments scale to any control size.
        Mark::Up => {
            let (dx, dy) = (s, s * 0.6);
            p.line_segment([c + egui::vec2(-dx, dy), c + egui::vec2(0.0, -dy)], stroke);
            p.line_segment([c + egui::vec2(0.0, -dy), c + egui::vec2(dx, dy)], stroke);
        }
        Mark::Down => {
            let (dx, dy) = (s, s * 0.6);
            p.line_segment([c + egui::vec2(-dx, -dy), c + egui::vec2(0.0, dy)], stroke);
            p.line_segment([c + egui::vec2(0.0, dy), c + egui::vec2(dx, -dy)], stroke);
        }
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

/// The translation stick, bottom-left. Returns its reading in the unit disc.
///
/// Bottom-*left* because the dock (the panel toggles) is bottom-centre and the
/// climb pair is right-of-centre: the three do not overlap at any window size,
/// which a corner anchor cannot promise on its own.
fn thumbstick(
    ctx: &egui::Context,
    fingers: &Fingers,
    owner: &mut Option<u64>,
    areas: &mut Vec<egui::Rect>,
) -> Vec2 {
    let mut wish = Vec2::ZERO;
    let size = STICK_RADIUS * 2.0;
    let area = egui::Area::new(egui::Id::new("osd_stick"))
        .anchor(
            egui::Align2::LEFT_BOTTOM,
            egui::vec2(theme::OSD_MARGIN, -bottom_offset()),
        )
        .show(ctx, |ui| {
            let (rect, resp) =
                ui.allocate_exact_size(egui::Vec2::splat(size), egui::Sense::click_and_drag());
            let centre = rect.center();
            // The miss band: a grab that lands just past the well still takes
            // the stick (nothing borders it but map), and `stick_wish` clamps
            // the reading back onto the rim.
            *owner = stick_owner(*owner, fingers, rect.expand(MISS_MARGIN));
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
            // put together. [`theme::OVERLAY_SOFT`] keeps the map visible through
            // it; the hairline is what says where the travel ends.
            p.circle_filled(centre, STICK_RADIUS, theme::OVERLAY_SOFT);
            p.circle_stroke(
                centre,
                STICK_RADIUS,
                egui::Stroke::new(1.5, theme::OVERLAY_LINE),
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
                (theme::OVERLAY, theme::OVERLAY_LINE)
            } else {
                (theme::ACCENT_MUTED, theme::ACCENT_RING)
            };
            p.circle_filled(knob, STICK_KNOB_RADIUS, fill);
            p.circle_stroke(knob, STICK_KNOB_RADIUS, egui::Stroke::new(1.5, ring));
        });
    areas.push(area.response.rect);
    wish
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
            released: released.iter().map(|(x, y)| egui::pos2(*x, *y)).collect(),
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

    /// The two modes that translate something get the stick; Earth, whose whole
    /// vocabulary is drag/pinch/twist, gets the zoom pair alone.
    #[test]
    fn the_translating_modes_get_the_stick() {
        let touch = |mode| cluster_for(mode, false, false, false, true);
        assert_eq!(touch(CameraMode::Free), Some(Cluster::Fly));
        assert_eq!(touch(CameraMode::Avatar), Some(Cluster::Walk));
        assert_eq!(touch(CameraMode::Orbit), Some(Cluster::Zoom));
    }

    /// Flight swaps the right-edge pair rather than adding a third column: on
    /// foot the avatar has no vertical, so that space zooms; in the air it is the
    /// same climb control Fly mode puts in the same place.
    #[test]
    fn follow_swaps_zoom_for_climb_in_flight() {
        // The cluster is the same either way — the swap is inside it, which is
        // what keeps the stick and the action pair from moving when you take off.
        assert_eq!(
            cluster_for(CameraMode::Avatar, false, false, false, true),
            Some(Cluster::Walk)
        );
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
            actions_bottom_offset() + pair_height(),
        );
        assert!(
            actions.0 >= pair.1,
            "the action pair starts at {} inside a zoom pair reaching {}",
            actions.0,
            pair.1
        );
        assert!((actions.0 - pair.1 - theme::OSD_GROUP_GAP).abs() < 1e-3);
        // …and the bottom-most of them clears the dock, which owns that edge.
        assert!(pair.0 >= crate::systems::dock::reserved_bottom());
    }

    /// What the bottom-left corner reserves has to cover the stick actually
    /// drawn there, or the scale bar that reads this lands on it — which is the
    /// collision that moved the bar off the opposite corner in the first place.
    #[test]
    fn the_bottom_left_reservation_covers_the_stick() {
        let stick_top = bottom_offset() + STICK_RADIUS * 2.0;
        for cluster in [Cluster::Fly, Cluster::Walk] {
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
}
