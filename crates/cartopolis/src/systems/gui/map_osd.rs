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
//! [`TouchGestures::ui_held`]: crate::systems::touch::TouchGestures::ui_held
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

/// Bottom offset of the face disc, a group gap above the jump/flight pair: the
/// top tier of Follow's right edge. The hand button stands beside it.
fn face_bottom_offset() -> f32 {
    actions_bottom_offset() + pair_height() + theme::OSD_GROUP_GAP
}

/// Right offset of the hand button: one disc and a gap in from the edge,
/// beside the face disc.
fn hand_right_offset() -> f32 {
    theme::OSD_MARGIN + CONTROL_SIZE + theme::OSD_GAP
}

/// How far up from the bottom the right edge's touch controls reach, at
/// most: Follow's face disc. The map's column of round buttons comes down
/// from the top to meet it (`dock::right_column`), and the two must not meet.
#[cfg(test)]
fn right_edge_reach() -> f32 {
    face_bottom_offset() + CONTROL_SIZE
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
    /// A height the zoom rail asked for (metres above the ground): a drag along
    /// it, or a tap on one of its rungs. Earth mode glides there and holds the
    /// goal until it arrives or another zoom takes over
    /// (`CameraState::rail_goal`).
    pub rail_altitude: Option<f64>,
}

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
        }
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
    mut stick_finger: Local<Option<u64>>,
    camera_state: Res<CameraState>,
    driver: Res<crate::systems::driver::DriverState>,
    right_panel: Res<RightPanel>,
    keyboard: Res<crate::systems::onscreen_keyboard::OnScreenKeyboard>,
    touch: Res<crate::systems::touch::TouchGestures>,
    avatar_state: Res<crate::resources::AvatarState>,
    camera: Res<crate::systems::camera_screen::CameraScreen>,
    mut body: BodyControls,
    mut rail: RailView,
    chrome: Res<crate::systems::dock::MapChrome>,
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
        // The camera is the whole screen, exactly as a panel is. A chat's
        // World view is not: it keeps the touch controls.
        !right_panel.is(RightPanel::None) || camera.is_open(),
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
                if let Some(e) = face_picker(ctx, &fingers, &mut areas, &mut body.faces_open) {
                    body.faces
                        .write(crate::systems::avatar_face::StartExpression(e));
                }
                hand_button(ctx, &fingers, &mut areas, &mut body);
            }
            // The zoom rail below is Earth mode's control, on every device.
            Cluster::Zoom => {}
        }
    } else {
        *stick_finger = None;
    }

    // The zoom rail (`systems::zoom_rail`): Earth mode, wherever nothing covers
    // the map's right edge — the same rules as the clusters', less the touch
    // screen, since a desktop wants it too.
    let rail_on = camera_state.mode == CameraMode::Orbit
        && !driver.active
        && right_panel.is(RightPanel::None)
        && !camera.is_open()
        && !keyboard.is_open();
    if rail_on {
        if let Some(alt) = rail.altitude().map(|a| a.max(camera_state.cosmos_alt)) {
            let fingers = Fingers::new(&touch, egui.ui_scale());
            let touch_ui = theme::touch_layout() || touch.ever();
            // While a jump flies the rail shows the scale it passes, so the
            // way through every scale between is seen.
            let scale = if camera_state.rail_goal.is_some() {
                crate::systems::zoom_rail::Scale::of(alt)
            } else {
                camera_state.zoom_scale
            };
            let input = RailIn {
                alt,
                scale,
                flying_to: camera_state.rail_goal,
                push: camera_state.edge_push_shown,
                globe_return: camera_state.globe_return_alt,
                top: chrome.right_bottom + theme::OSD_GROUP_GAP,
            };
            let (zoom, goal) = zoom_rail_control(
                egui.ctx(),
                &fingers,
                &mut areas,
                &input,
                &mut rail.state,
                touch_ui,
                touch.active(),
            );
            next.zoom = zoom;
            next.rail_altitude = goal;
        }
    }

    // Published in the window's points, which is what the touchscreen
    // reports; egui laid them out in its own (see `EguiManager::ui_scale`).
    // Padded by the miss band, so a grab that lands beside a control is
    // claimed rather than handed to the camera as a gesture.
    let ui_scale = egui.ui_scale();
    regions.set_if_neq(crate::systems::touch::UiClaimRegions {
        rects: areas
            .iter()
            .map(|r| to_window(r.expand(CLAIM_PAD), ui_scale))
            .collect(),
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
                ui.spacing_mut().item_spacing.y = 6.0;
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
                ui.multiply_opacity(RAIL_REST_OPACITY + (1.0 - RAIL_REST_OPACITY) * awake);
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
                if held(&resp, hit) {
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
                if held(&resp, hit) {
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
                ui.spacing_mut().item_spacing.y = 6.0;
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
                // light plate an open panel's dock disc wears. It was the one
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
    // Beside the face disc rather than above it: the right edge's top half is
    // the map's column of round buttons now (`dock::right_column`), and a
    // fourth tier here reached into it on a phone.
    let area = egui::Area::new(egui::Id::new("osd_hand"))
        .anchor(
            egui::Align2::RIGHT_BOTTOM,
            egui::vec2(-hand_right_offset(), -face_bottom_offset()),
        )
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
                crate::systems::hud::halo_text_aligned(
                    ui.painter(),
                    egui::pos2(face.center.x - face.radius - 8.0, face.center.y),
                    egui::Align2::RIGHT_CENTER,
                    word.text(),
                    tone,
                );
            }
            let tapped = resp.clicked() || fingers.lifted_in(reach(resp.rect, true, false));
            if tapped && !resp.long_touched() && HighPrecision::available() {
                if body.high.on || body.store.data.hand.agreed {
                    body.high.on = !body.high.on;
                } else {
                    body.card.0 = true;
                }
            }
            resp.context_menu(|ui| {
                ui.label(egui::RichText::new("Your hand").strong());
                ui.horizontal(|ui| {
                    for (side, label) in [(Side::Left, "Left"), (Side::Right, "Right")] {
                        if ui.selectable_label(body.held.side == side, label).clicked() {
                            body.store.data.hand.left = side == Side::Left;
                            body.store.dirty = true;
                        }
                    }
                });
                if ui
                    .add_enabled(body.high.on, egui::Button::new("Start again"))
                    .clicked()
                {
                    body.high.restart();
                    ui.close();
                }
                if let Some(problem) = body.high.problem() {
                    ui.label(egui::RichText::new(problem).color(theme::p().warn));
                }
                ui.label(
                    egui::RichText::new(
                        "Uses the back camera and the motion sensors. Nothing the \
                         camera sees is kept.",
                    )
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
            egui::RichText::new("Your phone, in your avatar's hand")
                .size(theme::SIZE_TITLE)
                .strong(),
        );
        ui.add_space(theme::OSD_GAP);
        for line in [
            "The back camera and the motion sensors follow the phone, to about \
             a centimetre. Nothing the camera sees is kept or sent.",
            "Things that sense hands, such as a theremin, feel where your hand \
             is while you are within a few metres of them.",
            "Later, people near you will see your arm move: the same switch.",
        ] {
            ui.label(line);
            ui.add_space(theme::OSD_GAP * 0.5);
        }
        ui.add_space(theme::OSD_GAP);
        ui.horizontal(|ui| {
            if theme::secondary_button(ui, "", "Not now", 140.0).clicked() {
                body.card.0 = false;
            }
            if theme::primary_button_sized(ui, "Use my hand", 160.0).clicked() {
                body.store.data.hand.agreed = true;
                body.store.dirty = true;
                body.high.on = true;
                body.card.0 = false;
            }
        });
    });
    if modal.should_close() {
        body.card.0 = false;
    }
}

/// A disc above the jump/flight pair that opens a column of expressions
/// beside it; a tap on one pulls that face and closes it. Returns the face
/// picked this frame.
fn face_picker(
    ctx: &egui::Context,
    fingers: &Fingers,
    areas: &mut Vec<egui::Rect>,
    open: &mut bool,
) -> Option<cartopolis_core::Expression> {
    let above = face_bottom_offset();
    let area = egui::Area::new(egui::Id::new("osd_face"))
        .anchor(
            egui::Align2::RIGHT_BOTTOM,
            egui::vec2(-theme::OSD_MARGIN, -above),
        )
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
            egui::vec2(
                -(hand_right_offset() + CONTROL_SIZE + theme::OSD_GAP),
                -above,
            ),
        )
        .show(ctx, |ui| {
            theme::overlay_frame().show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 4.0;
                for e in cartopolis_core::Expression::ALL {
                    let resp = ui.add(
                        egui::Button::new(expression_label(e)).min_size(egui::vec2(120.0, 32.0)),
                    );
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
    fn label(self) -> &'static str {
        match self {
            Self::Plus => "Zoom in",
            Self::Minus => "Zoom out",
            Self::Up => "Climb",
            Self::Down => "Descend",
            Self::Jump => "Jump",
            Self::Wings => "Fly",
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
        // A swept delta, drawn as its two halves: the dart has a notched tail,
        // so it is not convex, and `convex_polygon` is the only filled primitive
        // egui tessellates correctly. Each half is a triangle and each triangle
        // is convex, and they share the nose and the notch, so the seam between
        // them lands on the axis where a dart has a crease anyway.
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
        }
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
            let resp = theme::named(ui, resp, "Move");
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

    /// The right edge has two owners since 2026-11-10: the map's column of
    /// round buttons comes down from under the search bar
    /// (`dock::right_column`), and the touch controls grow up from the
    /// World | Community switch. On a phone (600 pt tall at 1.5, the shots'
    /// 412×900 px) they must not meet — neither Follow's tallest stack (the
    /// face and hand discs over the jump/flight pair over the zoom pair) nor
    /// Earth's rail.
    #[test]
    fn the_right_edge_controls_clear_the_column_of_round_buttons() {
        let phone_h = 600.0;
        let column = crate::systems::dock::right_column(true, true).bottom;
        let follow_top = phone_h - right_edge_reach();
        assert!(
            follow_top >= column + theme::OSD_GAP,
            "Follow's stack reaches {follow_top}, the column ends at {column}"
        );
        let rail = 3.0 * RAIL_DISC + RAIL_TRACK_TOUCH + 3.0 * 6.0;
        let rail_top = phone_h - bottom_offset() - rail;
        assert!(
            rail_top >= column + theme::OSD_GAP,
            "the rail reaches {rail_top}, the column ends at {column}"
        );
        // The hand button stands beside the face disc, a disc and a gap in.
        assert!(hand_right_offset() >= theme::OSD_MARGIN + CONTROL_SIZE + theme::OSD_GAP);
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

    /// Drives the zoom rail on a phone-sized touch screen, one frame at a time.
    struct Rail {
        ctx: egui::Context,
        state: RailState,
        t: f64,
        /// The camera's height the rail is drawn for.
        alt: f64,
    }

    impl Rail {
        fn new() -> Self {
            let mut rail = Rail {
                ctx: egui::Context::default(),
                state: RailState::default(),
                t: 0.0,
                alt: 1e6,
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
            };
            self.t += 1.0 / 60.0;
            let input = egui::RawInput {
                time: Some(self.t),
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(400.0, 800.0),
                )),
                ..Default::default()
            };
            let (mut goal, mut areas) = (None, Vec::new());
            let state = &mut self.state;
            let touching = !f.held.is_empty() || f.free > 0;
            let rail = RailIn {
                alt: self.alt,
                scale: crate::systems::zoom_rail::Scale::of(self.alt),
                flying_to: None,
                push: 0.0,
                globe_return: None,
                top: 0.0,
            };
            self.ctx.run_ui_unpainted(input, |ui| {
                goal = zoom_rail_control(ui.ctx(), &f, &mut areas, &rail, state, true, touching).1;
            });
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
                held: Vec::new(),
                released: vec![(7, egui::pos2(p.0, p.1))],
                free: 0,
            };
            goal.or(self.frame(&lift, 0))
        }
    }

    /// A pinch whose first finger lands on the track: the second lands two
    /// frames later on the map, and the rail must neither move the height nor
    /// wake its name. It did both until 2026-11-02.
    #[test]
    fn a_pinch_landing_on_the_rail_leaves_it_alone() {
        let mut rail = Rail::new();
        let p = rail.on_track(0.3);
        let on = fingers(&[(1, p)], &[]);
        assert_eq!(rail.frame(&on, 0), None);
        assert_eq!(rail.frame(&on, 0), None);
        for _ in 0..30 {
            assert_eq!(
                rail.frame(&on, 1),
                None,
                "the pinch's finger moved the height"
            );
        }
        assert_eq!(rail.frame(&fingers(&[], &[p]), 0), None);
        assert_eq!(rail.state.shown, 0.0, "a pinch woke the rail's name");
    }

    /// A finger that lands on the map and slides over the rail is the map's.
    #[test]
    fn a_finger_sliding_in_from_the_map_is_not_the_rails() {
        let mut rail = Rail::new();
        let (x, y) = rail.on_track(0.5);
        rail.frame(&fingers(&[(1, (x - 120.0, y))], &[]), 0);
        for _ in 0..20 {
            assert_eq!(rail.frame(&fingers(&[(1, (x, y))], &[]), 0), None);
        }
        assert_eq!(rail.state.shown, 0.0);
    }

    /// A finger meant for the rail lands on it alone and stays: after a
    /// moment it sets the height, and the rung's name comes out.
    #[test]
    fn a_finger_resting_on_the_track_takes_it() {
        let mut rail = Rail::new();
        let p = rail.on_track(0.5);
        let on = fingers(&[(1, p)], &[]);
        assert_eq!(rail.frame(&on, 0), None, "not on the frame it landed");
        let goal = (0..20).find_map(|_| rail.frame(&on, 0));
        assert!(goal.is_some(), "a resting finger never took the track");
        assert!(rail.state.shown > 0.0);
    }

    /// A quick tap on the track, lifted before it took the track, goes to the
    /// rung it tapped.
    #[test]
    fn a_tap_on_the_track_goes_there() {
        let mut rail = Rail::new();
        let p = rail.on_track(0.2);
        assert_eq!(rail.frame(&fingers(&[(1, p)], &[]), 0), None);
        let lift = Fingers {
            held: Vec::new(),
            released: vec![(1, egui::pos2(p.0, p.1))],
            free: 0,
        };
        let goal = rail.frame(&lift, 0).expect("the tap went nowhere");
        let up = rail.frame(&fingers(&[], &[]), 0);
        assert_eq!(up, None);
        assert!(goal > 1e6, "a tap high on the rail climbs: {goal}");
        // …to the step it landed on, not the point.
        use crate::systems::zoom_rail::{index_of, step_altitude, Scale};
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
        let mut rail = Rail::new();
        rail.alt = Scale::Globe.bounds().1 * 0.999;
        let minus = rail.disc(1.0);
        let goal = rail.tap(minus).expect("the tap went nowhere");
        assert_eq!(Scale::of(goal), Scale::SolarSystem);
    }

    /// A − held into the end of the scale is the zoom pushing on, not a tap
    /// on the next scale when it lets go.
    #[test]
    fn a_minus_held_into_the_end_does_not_jump_when_let_go() {
        use crate::systems::zoom_rail::Scale;
        let mut rail = Rail::new();
        let minus = rail.disc(1.0);
        let on = fingers(&[(7, minus)], &[]);
        rail.frame(&on, 0);
        rail.alt = Scale::Globe.bounds().1;
        for _ in 0..5 {
            assert_eq!(rail.frame(&on, 0), None);
        }
        let lift = Fingers {
            held: Vec::new(),
            released: vec![(7, egui::pos2(minus.0, minus.1))],
            free: 0,
        };
        assert_eq!(rail.frame(&lift, 0), None, "letting go crossed");
    }

    /// The scale disc opens the four scales; a tap on one flies to it, and the
    /// list folds.
    #[test]
    fn the_scale_disc_opens_the_scales() {
        use crate::systems::zoom_rail::Scale;
        let mut rail = Rail::new();
        let disc = rail.disc(0.0);
        assert_eq!(rail.tap(disc), None);
        assert!(rail.state.list_open, "the list did not open");
        // The list's rows run down from the disc's top, outermost first.
        let row = |k: f32| {
            (
                disc.0 - RAIL_DISC - 40.0,
                disc.1 - RAIL_DISC / 2.0 + 2.0 + RAIL_DISC / 2.0 + k * (RAIL_DISC + 10.0),
            )
        };
        let goal = rail.tap(row(0.0)).expect("the row went nowhere");
        assert_eq!(goal, Scale::Universe.overview());
        assert!(!rail.state.list_open, "the list stayed open");
    }
}
