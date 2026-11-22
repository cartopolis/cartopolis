//! Touch input — the finger equivalents of the mouse verbs.
//!
//! **Why this file has to exist at all.** winit's web backend routes every
//! pointer whose `pointerType` is `"touch"` to `WindowEvent::Touch` and emits
//! **no** `CursorMoved` and **no** `MouseInput` for it (see its
//! `platform_impl/web/event_loop/window_target.rs`: the mouse path and the touch
//! path are separate callbacks, and only the mouse one synthesises a cursor).
//! Bevy passes that straight through as [`TouchInput`]. So a client that only
//! reads `ButtonInput<MouseButton>` + `CursorMoved` — which is every input
//! consumer we had — is not *degraded* on a phone, it is completely inert: no
//! camera, no buttons, no panels, nothing. The same is true of a native
//! touchscreen; the web is just where it is guaranteed to be hit.
//!
//! Two bridges, deliberately separate because they answer different questions:
//!
//! 1. **Mouse emulation** (here) — the primary finger drives `ButtonInput<
//!    MouseButton>::Left` and the window's cursor position, so every existing
//!    click consumer (`picking`, `gizmo`, `navigation`'s route pick,
//!    `data_layers`' map pick, the globe's zoom-toward-cursor) works untouched.
//!    A long press stands in for the right button, i.e. the context menu.
//! 2. **Gestures** ([`TouchGestures`]) — pinch, twist and the two-finger drag
//!    have no mouse equivalent to emulate, so they are recognised into a
//!    resource and read by `free_camera::camera_controls` per camera mode.
//!
//! egui is a *third* bridge and is not here: it reads the ordered window-event
//! stream directly, so its touch handling lives beside the rest of that stream
//! in `egui_manager::pointer_events`.
//!
//! Deltas are in **logical points**, the same unit `CursorMoved::position` uses,
//! so a finger and a mouse move the camera by the same amount per point and the
//! existing sensitivity constants carry over unchanged.
//!
//! ## A finger belongs to one thing at a time ([`UiClaimRegions`])
//!
//! egui has exactly **one** pointer, and both bridges above were written around
//! it: the pointer-holding finger drives the widgets, and everything that reads
//! the world asks `egui_wants_pointer_input()` — one boolean, for the whole
//! screen — before acting. That is right for a mouse and wrong for a hand: with a
//! thumb resting on the on-screen stick (`systems::map_osd`) the boolean is true
//! for as long as it rests there, so a *second* finger anywhere else on the glass
//! could not turn the view. The stick was a modal control by accident — the one
//! thing you hold down continuously, and holding it froze the camera.
//!
//! So the glass is partitioned instead of arbitrated. Whoever draws an on-screen
//! control publishes its rects into [`UiClaimRegions`]; a finger that **lands**
//! inside one is claimed by the UI for as long as it stays down (it keeps the
//! claim when it slides out of the well, exactly as an egui drag keeps its
//! widget), and every claimed finger is then invisible to the gesture recogniser
//! and to the emulated mouse. What is left is the map's, and
//! [`TouchGestures::ui_blocked`] is what the camera asks instead of the bare
//! `egui_wants_pointer_input()`: a pointer egui only "wants" because a claimed
//! thumb is holding it does not speak for the fingers beside it.
//!
//! The claimed fingers are not thrown away — [`TouchGestures::ui_held`] and
//! [`TouchGestures::ui_released`] hand them back to the control that owns them,
//! which is how `map_osd` steers the stick from a finger that egui's single
//! pointer has never heard of.

use bevy::input::mouse::MouseButtonInput;
use bevy::input::touch::Touches;
use bevy::input::ButtonState;
use bevy::prelude::*;

/// How far (points) a finger may travel and still count as held in place. Used
/// for the long-press verdict; generous, because a finger resting on glass
/// wanders a few points on its own.
const HOLD_SLOP_PTS: f32 = 14.0;

/// How long the primary finger must stay put before it counts as a right-click.
const LONG_PRESS_SECS: f32 = 0.55;

/// How long after a tap lifts a second one still counts as a double tap.
///
/// Comfortably under [`LONG_PRESS_SECS`], because the two verbs must not be
/// reachable by the same pair of timings: a finger that dawdles is asking for
/// the context menu, not for the second half of a double tap.
const DOUBLE_TAP_SECS: f32 = 0.4;

/// How far apart (points) the two taps may land and still be one double tap.
/// Wider than [`HOLD_SLOP_PTS`] — a finger lifts and comes back down, and it
/// does not come back to the same pixel.
const DOUBLE_TAP_SLOP_PTS: f32 = 44.0;

/// Two-finger metrics below this separation (points) are refused: as the fingers
/// meet, the pinch ratio and especially the twist angle go to infinity on
/// sub-pixel noise.
const MIN_PAIR_SEPARATION_PTS: f32 = 24.0;

/// Per-frame pinch ratio clamp. A frame dropped mid-gesture (very ordinary on
/// the web, where a tile decode can eat 100 ms) otherwise arrives as one huge
/// ratio and teleports the camera.
const MAX_PINCH_PER_FRAME: f32 = 1.6;

/// The parts of the glass an on-screen control owns, in **window** logical
/// points (what [`Touches`] reports), rewritten every frame by whoever draws
/// them — `systems::map_osd` is the only writer so far.
///
/// Read one frame late: the controls are drawn in `Update` and this is consumed
/// in `PreUpdate`. That is harmless because every region here is anchored to a
/// window edge and does not move; the only stale frame is the one a control
/// appears or disappears on, where a finger landing in that exact millisecond is
/// attributed to whichever of the two the previous frame believed. Publishing
/// from the draw is still the right way round — the rects are the layout's, and
/// a second copy of the anchor arithmetic over here is how the hit area and the
/// picture drift apart.
///
/// **One writer, who rewrites the whole list.** A second publisher (the dock, the
/// HUD) would need slots keyed by owner, and is not worth adding until something
/// besides the on-screen controls is held down *while the other hand works* —
/// which is the only situation any of this exists for. What the other chrome
/// gets instead is the status quo: with a claimed thumb down, a tap on it is a
/// free finger, so it reaches neither the widget (egui's pointer is elsewhere)
/// nor, a tap carrying no drag, the camera in any visible amount.
#[derive(Resource, Default, Debug, Clone, PartialEq)]
pub struct UiClaimRegions {
    pub rects: Vec<Rect>,
}

impl UiClaimRegions {
    /// Does any region contain this point?
    fn claims(&self, at: Vec2) -> bool {
        self.rects.iter().any(|r| r.contains(at))
    }
}

/// This frame's recognised touch gestures, in logical points.
///
/// Every field is rewritten each frame by [`recognize_touch_gestures`]; there is
/// no accumulation, so a consumer that skips a frame simply misses that frame's
/// motion rather than receiving it late.
///
/// The camera reads these *inside* the same branches that gate the mouse on
/// `egui_wants_pointer_input()`, so a finger that egui has claimed (a panel
/// button, a slider) never also moves the world — the gate works for touch for
/// the same reason it works for the mouse, because the egui bridge holds a
/// pointer button down for the whole gesture. It goes through
/// [`TouchGestures::ui_blocked`] rather than being asked directly, because that
/// one boolean cannot answer for two fingers at once (see the module note).
#[derive(Resource, Debug)]
pub struct TouchGestures {
    /// Fingers down this frame that the UI has **not** claimed — the ones this
    /// resource speaks for. A thumb parked on the on-screen stick is not counted
    /// here, which is what lets the finger beside it be an ordinary one-finger
    /// drag rather than half of a pinch.
    pub fingers: usize,
    /// One-finger drag this frame. Zero unless exactly one finger is down — a
    /// second finger arriving ends the drag rather than jumping it to the
    /// centroid.
    pub drag: Vec2,
    /// Motion of the two-finger centroid this frame, while the pair is a
    /// map gesture (`PairLock::Map`): the pan that steers a pinch. Zero while it is
    /// a tilt, which reads [`TouchGestures::tilt`] instead.
    pub two_drag: Vec2,
    /// Spread ratio this frame — `>1` fingers moving apart (zoom in), `<1`
    /// together. Exactly `1.0` when there is no pair, which is the identity for
    /// every consumer, so no consumer needs to branch on "is there a pinch".
    pub pinch: f32,
    /// Twist this frame in radians, **positive clockwise on screen** (screen y
    /// points down). Chosen to carry the same sign as a rightward turn-drag, so
    /// the two gestures agree about which way "turn the view right" is.
    pub twist: f32,
    /// Vertical motion of the pair this frame (points, down positive), while
    /// the pair is a tilt (`PairLock::Tilt`) — both fingers side by side,
    /// sliding up or down together. Zero otherwise.
    pub tilt: f32,
    /// What the current pair has been recognised as. See [`PairLock`].
    lock: PairLock,

    /// Fingers an on-screen control owns, and where each is now — claimed at the
    /// moment it landed inside a [`UiClaimRegions`] rect and held until it
    /// lifts. Id-sorted, like every other list here.
    ui_held: Vec<(u64, Vec2)>,
    /// Claimed fingers that lifted **this frame**, at their last position, so a
    /// control whose verb is an edge (a jump, a toggle) can see the release it
    /// would otherwise have to learn from egui's pointer.
    ui_released: Vec<(u64, Vec2)>,

    // ── recogniser state ────────────────────────────────────────────────
    /// Last frame's finger positions, id-sorted. Every finger, claimed or not:
    /// this is what says which ids are *new* this frame, and a claim is decided
    /// on the frame a finger lands.
    ///
    /// Kept here rather than read from [`bevy::input::touch::Touch::delta`],
    /// which is only refreshed on frames that carry touch *events*: hold a
    /// finger still for a frame and `delta()` hands back the previous frame's
    /// delta a second time, which reads as the map continuing to slide under a
    /// motionless finger.
    seen: Vec<(u64, Vec2)>,
    /// Which finger is standing in for the mouse. The first one down; it keeps
    /// the role until it lifts, so a pinch never re-targets the cursor.
    primary: Option<u64>,
    /// Where and when the primary finger went down, and how far it has strayed.
    press: Option<Press>,
    /// A `Left` release owed to the next frame — see [`emulate_mouse`].
    deferred_release: bool,
    /// Was the `Left` press `ButtonInput` reports as `just_pressed` **this**
    /// frame a finger's, and was it the second tap of a double tap?
    ///
    /// These are the previous frame's `pending_*`, shifted into view at the top
    /// of [`recognize_touch_gestures`]. They have to be shifted because the
    /// press is *written* one frame before `ButtonInput` reports it (see
    /// [`emulate_mouse`]'s ordering note), and the consumer asking "was that
    /// click a finger's?" asks on the frame `just_pressed` fires.
    touch_click: bool,
    double_tap: bool,
    /// The same two verdicts about the press written this frame.
    pending_click: bool,
    pending_double: bool,
    /// Where and when the last tap lifted. A tap is a press that stayed put and
    /// let go before the long press claimed it; a drag or a menu clears it, so
    /// tap-drag-tap is not a double tap.
    last_tap: Option<(Vec2, f32)>,
    /// Seconds since the recogniser started, accumulated from the frame delta.
    /// Its own clock rather than `Time`'s, so the state machine stays a pure
    /// function of the frames handed to it.
    clock: f32,
    /// Has a finger touched the glass at all this session? A latch, never
    /// cleared.
    ///
    /// It exists because the *device* question and the *layout* question have
    /// different right answers. `quality::is_phone` (through
    /// `theme::touch_layout`) decides the layout, and deliberately says no to a
    /// large tablet and to a touchscreen laptop — both of which render at desktop
    /// resolution and should not get finger-sized widgets. But a run that has
    /// seen a finger genuinely has no keyboard verbs available to it, whatever
    /// its resolution, so the on-screen controls (`systems::map_osd`) appear the
    /// moment one is used. This is not a second detector answering the same
    /// question: it answers "has this session been touched", which no media
    /// query can.
    ever_touched: bool,
}

#[derive(Debug, Clone, Copy)]
struct Press {
    at: Vec2,
    held: f32,
    strayed: f32,
    long_press_sent: bool,
    /// Set once the `Left` press has actually been written, so the matching
    /// release can never precede it.
    pressed: bool,
    /// Set when this press *completed* a double tap. Its lift then records no
    /// new tap, so a finger drumming on the glass fires the verb on the second
    /// beat and not on every beat after it.
    paired: bool,
}

impl Default for TouchGestures {
    fn default() -> Self {
        Self {
            fingers: 0,
            drag: Vec2::ZERO,
            two_drag: Vec2::ZERO,
            // Multiplicative identity, not zero — see the field's doc.
            pinch: 1.0,
            twist: 0.0,
            tilt: 0.0,
            lock: PairLock::Off,
            ui_held: Vec::new(),
            ui_released: Vec::new(),
            seen: Vec::new(),
            primary: None,
            press: None,
            deferred_release: false,
            touch_click: false,
            double_tap: false,
            pending_click: false,
            pending_double: false,
            last_tap: None,
            clock: 0.0,
            ever_touched: false,
        }
    }
}

impl TouchGestures {
    /// True when a finger is on the glass. The camera clears its mouse drag
    /// anchor on these frames: the emulated `Left` button is held for the whole
    /// gesture but no `CursorMoved` accompanies it, so an anchor left over from
    /// the last real mouse drag would survive to jump the view on the next one.
    pub fn active(&self) -> bool {
        self.fingers > 0 || !self.ui_held.is_empty()
    }

    /// Fingers an on-screen control owns, and where they are now (window points).
    /// Its own hit-testing is the control's business — this only says which
    /// fingers are *available* to it, i.e. the ones no gesture will also consume.
    pub fn ui_held(&self) -> &[(u64, Vec2)] {
        &self.ui_held
    }

    /// Claimed fingers that lifted this frame, at their last position — the
    /// release edge for a control whose verb is an event rather than a rate.
    pub fn ui_released(&self) -> &[(u64, Vec2)] {
        &self.ui_released
    }

    /// True when a finger the UI owns is down *beside* one it does not.
    ///
    /// This is the case `egui_wants_pointer_input()` gets wrong: egui's single
    /// pointer is held by the claimed thumb, so the boolean says "the UI has the
    /// pointer" on behalf of a finger that is not the one asking.
    fn escapes_ui(&self) -> bool {
        !self.ui_held.is_empty() && self.fingers > 0
    }

    /// Should a world consumer stand down, given egui's own verdict?
    ///
    /// The gate every camera gesture goes through instead of calling
    /// `egui_wants_pointer_input()` directly. Takes the verdict as a bool rather
    /// than an `egui::Context` so the rule stays a pure function.
    pub fn ui_blocked(&self, egui_wants_pointer: bool) -> bool {
        egui_wants_pointer && !self.escapes_ui()
    }

    /// True when exactly two fingers are down, i.e. the pinch/twist/tilt
    /// gesture owns the frame and the one-finger verb must not also fire.
    pub fn pair(&self) -> bool {
        self.fingers == 2
    }

    /// Has this session ever been touched? See [`TouchGestures::ever_touched`].
    pub fn ever(&self) -> bool {
        self.ever_touched
    }

    /// Did this frame's `just_pressed(Left)` come from a finger?
    ///
    /// It is per *press*, not per session: a touchscreen laptop driven with a
    /// mouse answers `false`, so a consumer that asks more of a finger than of a
    /// cursor does not also ask it of the cursor.
    pub fn touch_click(&self) -> bool {
        self.touch_click
    }

    /// …and was that press the second tap of a double tap?
    ///
    /// The finger's answer to "did you mean it?". A single tap is the cheapest
    /// thing a hand can do by accident — every pan and every pinch begins with
    /// one — so a verb that opens something over the map asks for two.
    pub fn double_tap(&self) -> bool {
        self.double_tap
    }
}

/// Recognise this frame's gestures and emulate the mouse from the primary
/// finger. Runs in `PreUpdate` after `InputSystems`, so [`Touches`] already
/// holds this frame's events.
pub fn recognize_touch_gestures(
    touches: Res<Touches>,
    time: Res<Time>,
    regions: Res<UiClaimRegions>,
    mut gestures: ResMut<TouchGestures>,
    mut mouse_button: MessageWriter<MouseButtonInput>,
    mut windows: Query<(Entity, &mut Window)>,
) {
    // Last frame's press is the one `ButtonInput` reports this frame, so its
    // verdicts come into view now — before anything below can write new ones.
    gestures.touch_click = std::mem::take(&mut gestures.pending_click);
    gestures.double_tap = std::mem::take(&mut gestures.pending_double);

    // A frame with no window at all (shutdown, or a `--shot` run, which has no
    // touchscreen by construction) has nothing to emulate against.
    let Some((window_entity, mut window)) = windows.iter_mut().next() else {
        return;
    };

    let previous = std::mem::take(&mut gestures.seen);
    let fingers = fingers_this_frame(
        &touches
            .iter()
            .map(|t| (t.id(), t.position()))
            .collect::<Vec<_>>(),
        &touches
            .iter_just_released()
            .map(|t| (t.id(), t.position()))
            .collect::<Vec<_>>(),
        &previous,
    );

    // Hand the UI its fingers first: what is left is the map's, and neither
    // bridge below ever sees the ones the UI took. `previous` is the *whole*
    // previous frame either way — it is only ever looked up by id, and it is
    // what says which fingers are new (i.e. which are claimable) this frame.
    let mut ui_held = std::mem::take(&mut gestures.ui_held);
    let mut ui_released = std::mem::take(&mut gestures.ui_released);
    let free = update_claims(
        &mut ui_held,
        &mut ui_released,
        &previous,
        &fingers,
        &regions,
    );

    let read = lock_pair(&mut gestures.lock, &free, recognize(&previous, &free));

    gestures.fingers = free.len();
    gestures.ever_touched |= !fingers.is_empty();
    gestures.drag = read.drag;
    gestures.pinch = read.pinch;
    gestures.twist = read.twist;
    gestures.two_drag = read.two_drag;
    gestures.tilt = read.tilt;
    gestures.ui_held = ui_held;
    gestures.ui_released = ui_released;

    let emulated = emulate_mouse(&mut gestures, &free, time.delta_secs());
    if let Some(aim) = emulated.cursor {
        // Physical pixels: that is the window's own storage, and
        // `cursor_position()` divides by the scale on the way back out.
        let physical = (aim * window.resolution.scale_factor()).as_dvec2();
        window.set_physical_cursor_position(Some(physical));
    }
    for (button, state) in emulated.buttons {
        mouse_button.write(MouseButtonInput {
            button,
            state,
            window: window_entity,
        });
    }

    gestures.seen = fingers;
}

/// Which fingers this frame has, id-sorted — `pressed`, plus any that landed
/// **and** lifted before this recogniser ever ran.
///
/// # Why the second list is not redundant
///
/// [`Touches::iter`] is a *level*: it yields `pressed`, and `bevy_input` removes
/// an id from `pressed` the instant it processes that id's `Ended` event. So a
/// frame whose event batch carries a `Started` and an `Ended` for one finger
/// leaves nothing behind but a `just_released` entry, and a recogniser that
/// polls the level alone never sees the tap at all — no press, no click, no
/// verdict, nothing in any log to say a finger was ever on the glass.
///
/// On a desktop that is unreachable, which is why it stood for so long: a 100 ms
/// tap spans six 60 Hz frames. On a handset drawing a city it is the ordinary
/// case — Android delivers touch at 120 Hz or better into a frame that may be
/// 40 ms long, and the *second* tap of a double tap is precisely the brisk one.
/// It is also why the panels kept working while the map stopped answering:
/// `egui_manager::pointer_events` reads the `TouchInput` **stream** in order and
/// so has always seen both edges. The asymmetry was the bug, not the device.
///
/// Reinstating the finger for one frame is enough, and deliberately less than
/// synthesising a whole press/release pair here: the state machine downstream
/// already separates a press from its release by a frame
/// (`TouchGestures::deferred_release`), so the finger is adopted and the `Left`
/// press written now, it is absent next frame, and the ordinary lift path writes
/// the release and records the tap.
///
/// Conditioned on the finger being **new**. A touch that was live last frame and
/// released this one is an ordinary lift; extending that by a frame would delay
/// every release on every platform to fix a case none of them has.
fn fingers_this_frame(
    pressed: &[(u64, Vec2)],
    just_released: &[(u64, Vec2)],
    previous: &[(u64, Vec2)],
) -> Vec<(u64, Vec2)> {
    let mut fingers = pressed.to_vec();
    for (id, at) in just_released {
        let known = previous.iter().any(|(seen, _)| seen == id)
            || fingers.iter().any(|(live, _)| live == id);
        if !known {
            fingers.push((*id, *at));
        }
    }
    // Id order, so "the pair" is the same two fingers frame to frame and the
    // angle between them doesn't flip sign when the iteration order changes.
    fingers.sort_unstable_by_key(|(id, _)| *id);
    fingers
}

/// Move this frame's fingers between the UI's set and the map's, and hand back
/// the map's. All three lists are id-sorted, and stay so.
///
/// A claim is decided **once, on the frame the finger lands**, and then held
/// until it lifts. Re-testing the region every frame would be the other obvious
/// rule and it is the wrong one twice over: a thumb pushed past the stick's rim
/// — which is the ordinary way to ask for full throttle — would hand itself to
/// the camera mid-push, and a finger dragging the map *into* the stick's corner
/// would be swallowed by it.
fn update_claims(
    held: &mut Vec<(u64, Vec2)>,
    released: &mut Vec<(u64, Vec2)>,
    previous: &[(u64, Vec2)],
    fingers: &[(u64, Vec2)],
    regions: &UiClaimRegions,
) -> Vec<(u64, Vec2)> {
    let position_of = |id: u64| fingers.iter().find(|(f, _)| *f == id).map(|(_, p)| *p);

    // Follow the fingers already claimed: still down means a new position, gone
    // means a release, reported for exactly the frame it happened on.
    released.clear();
    for (id, last) in std::mem::take(held) {
        match position_of(id) {
            Some(now) => held.push((id, now)),
            None => released.push((id, last)),
        }
    }

    // Claim the newcomers that landed on a control.
    for (id, pos) in fingers {
        let landed = !previous.iter().any(|(seen, _)| seen == id);
        if landed && regions.claims(*pos) {
            held.push((*id, *pos));
        }
    }
    held.sort_unstable_by_key(|(id, _)| *id);

    fingers
        .iter()
        .filter(|(id, _)| !held.iter().any(|(owned, _)| owned == id))
        .copied()
        .collect()
}

/// What one frame's finger positions say, given the previous frame's. The pure
/// half of [`recognize_touch_gestures`] — both id lists must be id-sorted.
#[derive(Debug, PartialEq)]
struct Read {
    drag: Vec2,
    two_drag: Vec2,
    pinch: f32,
    twist: f32,
    /// Only ever set by [`lock_pair`]; [`recognize`] reports the pair's motion
    /// in `two_drag` and leaves the verdict to it.
    tilt: f32,
    /// The distance between the pair's fingers this frame (points), zero
    /// without a pair — what turns a spread ratio and a twist angle into
    /// distances a finger travelled, for [`lock_pair`].
    separation: f32,
}

impl Default for Read {
    /// "Nothing happened this frame" — note `pinch`, which is a ratio and whose
    /// null value is 1, not 0. See [`TouchGestures::pinch`].
    fn default() -> Self {
        Self {
            drag: Vec2::ZERO,
            two_drag: Vec2::ZERO,
            pinch: 1.0,
            twist: 0.0,
            tilt: 0.0,
            separation: 0.0,
        }
    }
}

fn recognize(previous: &[(u64, Vec2)], fingers: &[(u64, Vec2)]) -> Read {
    let delta_of = |id: u64, pos: Vec2| {
        previous
            .iter()
            .find(|(prev_id, _)| *prev_id == id)
            .map(|(_, prev)| pos - *prev)
    };

    let mut read = Read::default();

    match fingers {
        // One finger: a drag, and only if we saw this same finger last frame —
        // the frame it lands on has no delta, and reporting one from thin air
        // would flick the map on every tap.
        [(id, pos)] => read.drag = delta_of(*id, *pos).unwrap_or(Vec2::ZERO),
        // Exactly two: the pinch/twist/tilt gesture. Three or more is left
        // inert rather than guessed at — nothing in the client wants a
        // three-finger verb, and picking two of them would make the reading
        // depend on which fingers happened to be lowest-id.
        [(id_a, a), (id_b, b)] => {
            let (Some(da), Some(db)) = (delta_of(*id_a, *a), delta_of(*id_b, *b)) else {
                return read;
            };
            let (now, was) = (*b - *a, (*b - db) - (*a - da));
            if now.length() < MIN_PAIR_SEPARATION_PTS || was.length() < MIN_PAIR_SEPARATION_PTS {
                return read;
            }
            read.pinch =
                (now.length() / was.length()).clamp(1.0 / MAX_PINCH_PER_FRAME, MAX_PINCH_PER_FRAME);
            // Shortest way round, so a twist across the atan2 seam is a small
            // step and not a full turn of the view.
            read.twist = wrap_pi(now.to_angle() - was.to_angle());
            read.two_drag = (da + db) * 0.5;
            read.separation = now.length();
        }
        _ => {}
    }
    read
}

/// How far (points) a pair must move — together, apart or round — before it is
/// told to be a tilt or a map gesture. A few frames of an ordinary pinch; held
/// back rather than guessed, and handed over whole once decided, so nothing the
/// fingers did in the meantime is lost.
const PAIR_LOCK_PTS: f32 = 10.0;

/// What a two-finger gesture is, decided once per pair.
///
/// Two readings want the same motion. A pinch is steered by moving the fingers
/// *together* while they spread — the map follows the point between them, as in
/// every phone map — and the tilt is the very same together-motion, made
/// vertically. Until 2026-11-22 the camera took both: the centroid's vertical
/// motion tilted, its horizontal motion was dropped, and a pinch went to the
/// middle of the screen whatever the fingers did, so a zoom could not be aimed.
///
/// So the pair is classified once, the way phone maps do it: **two fingers side
/// by side sliding up or down together, without spreading or turning, is a
/// tilt**; anything else is the map gesture (zoom, turn and pan at once). The
/// verdict holds until the pair breaks up.
#[derive(Debug, Clone, Copy, PartialEq)]
enum PairLock {
    /// No pair down.
    Off,
    /// A pair down, not yet moved far enough to tell; what it did so far.
    Deciding {
        ids: (u64, u64),
        drag: Vec2,
        pinch: f32,
        twist: f32,
    },
    Tilt {
        ids: (u64, u64),
    },
    Map {
        ids: (u64, u64),
    },
}

impl PairLock {
    fn ids(&self) -> Option<(u64, u64)> {
        match *self {
            PairLock::Off => None,
            PairLock::Deciding { ids, .. } | PairLock::Tilt { ids } | PairLock::Map { ids } => {
                Some(ids)
            }
        }
    }
}

/// Gate a frame's [`Read`] through the pair's [`PairLock`]: a tilt reports only
/// `tilt`, a map gesture only pinch, twist and `two_drag`, and a pair still
/// being decided reports nothing until the verdict, which then hands over all
/// it held back at once. One-finger reads pass through untouched.
fn lock_pair(lock: &mut PairLock, fingers: &[(u64, Vec2)], read: Read) -> Read {
    let [(id_a, a), (id_b, b)] = fingers else {
        *lock = PairLock::Off;
        return read;
    };
    let ids = (*id_a, *id_b);
    if lock.ids() != Some(ids) {
        *lock = PairLock::Deciding {
            ids,
            drag: Vec2::ZERO,
            pinch: 1.0,
            twist: 0.0,
        };
    }
    let held = |two_drag: Vec2, pinch: f32, twist: f32, tilt: f32| Read {
        two_drag,
        pinch,
        twist,
        tilt,
        ..read
    };
    match *lock {
        PairLock::Off => read,
        PairLock::Tilt { .. } => held(Vec2::ZERO, 1.0, 0.0, read.two_drag.y),
        PairLock::Map { .. } => read,
        PairLock::Deciding {
            ids,
            drag,
            pinch,
            twist,
        } => {
            let (drag, pinch, twist) =
                (drag + read.two_drag, pinch * read.pinch, twist + read.twist);
            // How far the fingers went, each way of moving them, in points.
            let gap = read.separation.max(MIN_PAIR_SEPARATION_PTS);
            let spread = (gap - gap / pinch).abs();
            let round = twist.abs() * gap * 0.5;
            let moved = drag.length().max(spread).max(round);
            if moved < PAIR_LOCK_PTS {
                *lock = PairLock::Deciding {
                    ids,
                    drag,
                    pinch,
                    twist,
                };
                return held(Vec2::ZERO, 1.0, 0.0, 0.0);
            }
            let side_by_side = (*b - *a).x.abs() >= (*b - *a).y.abs();
            let vertical = drag.y.abs() > 2.0 * drag.x.abs();
            let rigid = spread.max(round) < 0.5 * drag.length();
            if side_by_side && vertical && rigid {
                *lock = PairLock::Tilt { ids };
                held(Vec2::ZERO, 1.0, 0.0, drag.y)
            } else {
                *lock = PairLock::Map { ids };
                held(drag, pinch, twist, 0.0)
            }
        }
    }
}

/// Drive the emulated mouse from the primary finger: the window cursor tracks
/// it, `Left` is held for the length of the gesture, and holding still long
/// enough additionally fires `Right` (the context menu).
///
/// Ordering note. This runs *after* `InputSystems`, so the `MouseButtonInput`
/// messages written here reach `ButtonInput<MouseButton>` on the **next** frame,
/// one frame after the cursor position moved with the finger. That is a
/// deliberate trade — running before `InputSystems` instead would land the
/// button in the same frame but would have to read the raw event stream rather
/// than the settled [`Touches`] set, and duplicate the pair bookkeeping above —
/// and the lag is invisible at any frame rate a phone renders at.
///
/// Press and release are never written in the same frame ([`Press::pressed`] +
/// `deferred_release`). A tap short enough to start and finish inside one frame
/// — not rare on the web, where a frame can run 100 ms — would otherwise arrive
/// as `just_pressed` on a frame where egui has already seen the pointer *lift*,
/// and `egui_wants_pointer_input()` reports "egui wants it" whenever no pointer
/// button is down. The tap would be swallowed, intermittently and only on slow
/// frames.
///
/// Returns what to *do* rather than doing it, so the state machine — which is
/// where the ordering rules above live — is unit-testable without a `World`.
#[derive(Debug, Default, PartialEq)]
struct Emulated {
    /// In order. Bevy applies them in order too, so a press followed by a
    /// release within one frame settles as "not pressed, but `just_pressed`".
    buttons: Vec<(MouseButton, ButtonState)>,
    /// Where to put the window cursor, in logical points.
    cursor: Option<Vec2>,
}

fn emulate_mouse(
    gestures: &mut TouchGestures,
    fingers: &[(u64, Vec2)],
    delta_secs: f32,
) -> Emulated {
    let mut out = Emulated::default();
    gestures.clock += delta_secs;

    // A release owed from last frame, paid before anything else may press.
    let paid_release = gestures.deferred_release;
    if paid_release {
        gestures.deferred_release = false;
        out.buttons.push((MouseButton::Left, ButtonState::Released));
    }

    // Has the primary finger lifted (or been cancelled)?
    if let Some(id) = gestures.primary {
        if !fingers.iter().any(|(other, _)| *other == id) {
            gestures.primary = None;
            if let Some(lifted) = gestures.press.take() {
                if lifted.pressed {
                    gestures.deferred_release = true;
                }
                // Remember it as half of a double tap only if it was a tap: a
                // press that wandered is a drag, one the long press claimed is
                // a context menu, and one that already closed a pair is spent.
                // Every other case also *forgets* the tap before it, so
                // tap-drag-tap cannot pass for two in a row.
                let was_a_tap =
                    lifted.strayed <= HOLD_SLOP_PTS && !lifted.long_press_sent && !lifted.paired;
                gestures.last_tap = was_a_tap.then_some((lifted.at, gestures.clock));
            }
        }
    }

    // Adopt a new primary finger — but only while the emulated button is
    // genuinely idle. A release written this frame has not reached
    // `ButtonInput` yet, so pressing on top of it would deliver press-then-
    // release and leave the button *up* with a finger still on the glass. That
    // is the lift-one-finger-of-a-pinch case: the survivor takes the pointer,
    // but two frames later. `first()` is the lowest id, which for a fresh
    // gesture is the finger that landed first.
    if gestures.primary.is_none() && !paid_release && !gestures.deferred_release {
        if let Some((id, pos)) = fingers.first().copied() {
            gestures.primary = Some(id);
            gestures.press = Some(Press {
                at: pos,
                held: 0.0,
                strayed: 0.0,
                long_press_sent: false,
                pressed: false,
                paired: false,
            });
        }
    }

    let Some(primary_id) = gestures.primary else {
        return out;
    };
    let Some(pos) = fingers
        .iter()
        .find(|(id, _)| *id == primary_id)
        .map(|(_, p)| *p)
    else {
        return out;
    };

    // The cursor follows the finger on the very frame it lands, so the button
    // that arrives next frame is already aimed. Under a two-finger gesture it
    // follows the *centroid* instead — no button is coming, and the one thing
    // that reads the cursor mid-pinch is the globe's zoom-toward-cursor, which
    // wants the point between the fingers, not one of them.
    let aim = match fingers {
        [(_, a), (_, b)] => (*a + *b) * 0.5,
        _ => pos,
    };
    out.cursor = Some(aim);

    // A copy, written back on every path out: the double-tap verdict below
    // reads the rest of `gestures`, which a live `as_mut` borrow would forbid.
    let Some(mut press) = gestures.press else {
        return out;
    };
    press.held += delta_secs;
    press.strayed = press.strayed.max(pos.distance(press.at));

    if !press.pressed {
        press.pressed = true;
        gestures.pending_click = true;
        // Decided on the press rather than on the lift, so the second tap acts
        // the moment it lands — a verb that waited for the lift would feel
        // slower than the single tap it replaced.
        gestures.pending_double = gestures.last_tap.is_some_and(|(was, when)| {
            gestures.clock - when <= DOUBLE_TAP_SECS
                && press.at.distance(was) <= DOUBLE_TAP_SLOP_PTS
        });
        press.paired = gestures.pending_double;
        gestures.press = Some(press);
        out.buttons.push((MouseButton::Left, ButtonState::Pressed));
        return out;
    }

    // Long press → the right button, once. `Left` stays down underneath: the
    // context menu opens on `just_pressed(Right)` and nothing reads the two as
    // a chord, and synthesising a `Left` release here would read as a click on
    // whatever the menu is about to cover.
    //
    // One finger only. A pinch held steady strays no further than a resting
    // thumb does, so without this a paused two-finger gesture pops the context
    // menu open under the fingers.
    if fingers.len() == 1
        && !press.long_press_sent
        && press.held >= LONG_PRESS_SECS
        && press.strayed <= HOLD_SLOP_PTS
    {
        press.long_press_sent = true;
        out.buttons.push((MouseButton::Right, ButtonState::Pressed));
        out.buttons
            .push((MouseButton::Right, ButtonState::Released));
    }
    gestures.press = Some(press);
    out
}

/// Wrap an angle difference into `(-π, π]`.
fn wrap_pi(a: f32) -> f32 {
    (a + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI
}

#[cfg(test)]
mod tests;
