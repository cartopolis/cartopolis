//! The player acting on the world: walking up to something and pressing E, and
//! the emote keys.
//!
//! Until this existed the avatar could move through the city but not *touch* it —
//! picking was prim-only and edit-oriented, and nothing streamed reacted to the
//! avatar being anywhere near it. What makes this cheap is that the data was
//! already arriving: `systems::places` reads the `pois` and `addresses` layers out
//! of a tile body `map_geometry` was already fetching and parsing, and
//! `world_places` holds the result per cell. This module is only the proximity
//! test, the prompt and the key.
//!
//! **The prompt stands over the map, not the window**: its place is the map
//! area's bottom centre, so a side panel never sits on top of it. The library
//! draws it (`lib_interact`); [`drive_interaction_prompt`] decides what it says.

use crate::systems::ui_input::{UiInput, UiMetrics};
use bevy::prelude::*;
use cartopolis_core::msg;

use crate::grid::world_to_lat_lng;
use crate::resources::{AvatarState, CameraMode, CameraState, MapAnchor, RightPanel, Seated};
use crate::systems::geo_nav::GeoNavState;
use crate::systems::world_places::{Interactable, SeatSpot, WorldPlaces};
use cartopolis_core::{carry, Emote};

/// How close a loose physical prim must be for "Pick up" to be offered: the
/// sim's own reach (`carry::GRAB_REACH_M`) less a margin, so a prompt shown at
/// the edge is not refused because the sim's copy of the avatar is a tick
/// behind this one's.
pub const PICKUP_RADIUS: f32 = carry::GRAB_REACH_M - 0.3;

/// What acting on the prompt asks the server for, if anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorldAct {
    /// Press a scripted prim (`TouchPrim`).
    Touch(i64),
    /// Pick up a physical prim (`GrabPrim`, protocol 48).
    Grab(i64),
    /// Let go of what is carried (`ReleasePrim`): thrown, or put down.
    Release { throw: bool },
}

/// How close the avatar must be for a prompt to appear (metres). Roughly two
/// paces: near enough that the thing prompted for is unambiguous even where a
/// terrace has a dozen POIs along one frontage.
pub const INTERACT_RADIUS: f32 = 3.5;

/// How far the "where am I" readout will look for a house number (metres). Wider
/// than the interact radius because an address point sits at the *building's*
/// centroid, not at its door.
pub const ADDRESS_RADIUS: f32 = 35.0;

/// The current interaction target and the last thing acted on.
#[derive(Resource, Default)]
pub struct InteractState {
    /// What the prompt is offering, if anything.
    pub target: Option<TargetInfo>,
    /// Nearest house number to the avatar, for the HUD readout.
    pub address: Option<String>,
    /// What the player last interacted with, shown until they interact again.
    /// This is the entire "reward" for pressing the key at present — a line of
    /// text naming the thing — and it is deliberately a separate field from
    /// `target` so it survives walking away.
    pub last: Option<String>,
    /// Seconds the `last` line has left before it fades.
    pub last_ttl: f32,
    /// The name of what the avatar is carrying, for the "Throw" prompt —
    /// resolved by [`track_interaction_target`], which has the prims.
    pub held_name: Option<String>,
    /// The carried prim a set-down has already been sent for because the
    /// avatar went into the water, so it is asked once and not every frame
    /// until the snapshot that empties the hands arrives.
    pub let_go_sent: Option<i64>,
    /// A throw under way: seconds until the arm lets go, when the throw is
    /// sent ([`THROW_RELEASE_SECS`]).
    pub throw_in: Option<f32>,
    /// A signal button pressed at the prompt (its pole, world XZ), for
    /// `give_way::press_buttons` to take.
    pub pressed_button: Option<Vec2>,
    /// What the library's prompt (`lib_interact`) shows, published by
    /// [`drive_interaction_prompt`].
    pub lib_view: Option<PromptView>,
    /// Taps on the library's prompt, done by [`drive_interaction_prompt`]
    /// next frame.
    pub lib_acts: Vec<PromptAct>,
    /// Offer to call this person's own car: on foot, with none of their cars
    /// near (`systems::driving`).
    pub call_car: bool,
}

/// A tap on the prompt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PromptAct {
    /// Take the greeting offered.
    Greet,
    /// Let it go unanswered.
    Ignore,
    /// What E does: sit, stand up, press, pick up, throw.
    Act,
    /// Put down what is carried.
    PutDown,
    ClimbUp,
    /// Sit on the edge in front.
    SitDown,
    /// Make the place prompted for the destination.
    Route,
    /// Bring this person's own car to them (`systems::driving`).
    CallCar,
}

/// One control on the prompt.
#[derive(Clone, Debug, PartialEq)]
pub enum PromptItem {
    /// A secondary button: its glyph (`""` for none) and words.
    Button {
        glyph: &'static str,
        label: String,
        act: PromptAct,
    },
    /// A small icon button, with what it does as its hint.
    Icon {
        glyph: &'static str,
        hint: &'static str,
        act: PromptAct,
    },
}

/// What the prompt shows this frame, for the library to draw.
#[derive(Clone, Debug, PartialEq, Default)]
#[cfg_attr(not(feature = "ui-lib"), allow(dead_code))]
pub struct PromptView {
    /// Where the prompt's bottom centre stands, UI points: the map area's
    /// centre, [`PROMPT_BOTTOM_MARGIN`] above its bottom.
    pub x: f32,
    pub y: f32,
    /// Its controls, left to right.
    pub items: Vec<PromptItem>,
    /// With no controls, the line naming what was last acted on: a receipt,
    /// not a button.
    pub receipt: Option<String>,
}

/// How far into the `throw` clip the hand lets go, seconds: CMU 15_12's
/// release at take frame ~3298, 1.15 s after the cut starts at 3160
/// (`human-pack`). The throw is sent then, not on the key, or the ball would
/// fly off while the arm is still drawing back.
pub const THROW_RELEASE_SECS: f32 = 1.15;

/// A resolved prompt: enough to draw it without re-querying the store.
#[derive(Clone)]
pub struct TargetInfo {
    pub name: String,
    pub glyph: &'static str,
    pub verb: &'static str,
    pub detail: Option<&'static str>,
    /// Where the thing is, in anchored world XZ. Carried so the prompt can offer
    /// to *route* to it: the store is keyed by cell and the target may have gone
    /// out of range (or out of the ring entirely) between the prompt appearing and
    /// the button being pressed, and re-querying by name would be a different
    /// question with a different answer.
    pub pos: Vec2,
    /// Where on it the avatar would sit, for the forms anybody sits on. Resolved
    /// while the prompt is being built, against where the avatar was standing at
    /// the time, so walking to one end of a bench and sitting puts you at that
    /// end.
    pub seat: Option<SeatSpot>,
    /// The prim to send a `TouchPrim` for, when the thing prompted for is a
    /// scripted object rather than a place on the map.
    ///
    /// Streamed places and prims are different worlds — one comes out of a vector
    /// tile and belongs to nobody, the other is a server-owned object with an id —
    /// and they meet only here, in the one question the player is actually asking:
    /// what is in front of me, and what happens if I press it.
    pub prim_id: Option<i64>,
    /// The prim is a loose physical one to pick up (`GrabPrim`), not a
    /// scripted one to press.
    pub pickup: bool,
    /// The pole of a signal's pedestrian push button, world XZ, when that is
    /// what the prompt is for (`systems::give_way`). Pressing it is this
    /// client's alone: the traffic it stops is invented per viewer.
    pub button: Option<Vec2>,
    /// The car to get into, when the prompt is "Drive this car" or "Drive my
    /// car" (`systems::driving`).
    pub car: Option<crate::systems::driving::CarSpot>,
}

/// The name a prim goes by in a prompt: its own, or "object" when it has none.
fn prim_label(name: &str) -> String {
    if name.trim().is_empty() {
        "object".to_string()
    } else {
        name.to_string()
    }
}

impl TargetInfo {
    fn of(i: &Interactable, from: Vec2) -> Self {
        Self {
            name: i.name.clone(),
            glyph: i.kind.glyph(),
            verb: i.kind.verb(),
            detail: i.kind.detail(),
            pos: i.pos,
            seat: i.seat_spot(from),
            prim_id: None,
            pickup: false,
            button: None,
            car: None,
        }
    }

    /// The prompt at a signal pole with a pedestrian push button
    /// (`systems::give_way`): "Press the button", and while a press runs,
    /// what the walking man says.
    fn of_button(pole: Vec2, phase: Option<crate::systems::give_way::ButtonPhase>) -> Self {
        Self {
            name: "the button".to_string(),
            glyph: crate::systems::icons::HAND,
            verb: "Press",
            detail: phase.map(crate::systems::give_way::ButtonPhase::detail),
            pos: pole,
            seat: None,
            prim_id: None,
            pickup: false,
            button: Some(pole),
            car: None,
        }
    }

    /// The prompt for a loose physical prim small enough to carry.
    fn of_pickup(id: i64, name: &str, pos: Vec2) -> Self {
        Self {
            name: prim_label(name),
            glyph: crate::systems::icons::HAND,
            verb: "Pick up",
            detail: None,
            pos,
            seat: None,
            prim_id: Some(id),
            pickup: true,
            button: None,
            car: None,
        }
    }

    /// The prompt for a scripted prim the avatar is standing next to.
    ///
    /// `icons::SCRIPT` because it is already in the bundled subset and says the
    /// true thing — this object has a script — rather than guessing at what the
    /// script does. A new glyph would need font work (`subset-icon-font.py`), and
    /// a glyph outside the subset renders as a tofu box, which is how every panel
    /// ended up with one for a close button.
    fn of_prim(id: i64, name: &str, pos: Vec2) -> Self {
        Self {
            name: prim_label(name),
            glyph: crate::systems::icons::SCRIPT,
            verb: "Press",
            detail: None,
            pos,
            seat: None,
            prim_id: Some(id),
            pickup: false,
            button: None,
            car: None,
        }
    }

    /// The prompt's full line, e.g. `Sit at Bench` or `Look at Vismarkt (café)`.
    #[must_use]
    pub fn line(&self) -> String {
        match self.detail {
            Some(d) => format!("{} {} ({d})", self.verb, self.name),
            None => format!("{} {}", self.verb, self.name),
        }
    }
}

/// How long an acted-on line stays on screen.
const LAST_LINE_SECS: f32 = 4.0;

/// Whether the prompt offers the wall or edge the avatar faces
/// (`systems::climb::Edges`) rather than the place `target` names: walking up
/// to a wall and facing it is a narrower ask than standing within
/// [`INTERACT_RADIUS`] of a café. Not over a thing to pick up or a scripted
/// prim, which somebody put there to be used, nor while sitting or carrying,
/// which have their own prompts.
fn offers_edge(
    target: Option<&TargetInfo>,
    edges: &crate::systems::climb::Edges,
    avatar_state: &AvatarState,
) -> bool {
    edges.any()
        && !avatar_state.is_seated()
        && avatar_state.holding.is_none()
        && target.is_none_or(|t| t.prim_id.is_none())
}

/// Tell the server what was acted on, if anything was and if there is a server.
///
/// One definition for the two ways to act (the keys and the prompt), on the same
/// rule [`InteractState::act`] itself follows: a second copy is how they come to
/// behave differently. Offline this does nothing at all — a prim with a script
/// and a physical prim are server-owned objects, and `--offline` runs no sim —
/// and that is a silence rather than a failure, because the prompt only ever
/// appears for a prim a server sent.
fn send_act(online: Option<&Res<crate::resources::OnlineSession>>, act: Option<WorldAct>) {
    let (Some(session), Some(act)) = (online, act) else {
        return;
    };
    use cartopolis_core::NetMessage;
    // Whether it went is logged by `send`; a lost link has nothing to retry.
    let _ = match act {
        WorldAct::Touch(prim_id) => session.send(
            "touch prim",
            NetMessage::TouchPrim(msg::TouchPrim {
                request_id: 0,
                prim_id,
            }),
        ),
        WorldAct::Grab(prim_id) => session.send(
            "pick up",
            NetMessage::GrabPrim(msg::GrabPrim {
                request_id: 0,
                prim_id,
            }),
        ),
        WorldAct::Release { throw } => session.send(
            "let go",
            NetMessage::ReleasePrim(msg::ReleasePrim {
                request_id: 0,
                throw,
            }),
        ),
    };
}

impl InteractState {
    /// Act on whatever the prompt is offering. One definition, because there are
    /// two ways to ask for it — the E key and a tap on the prompt itself — and a
    /// second copy of "play the clip, remember the line, start its timer" is how
    /// they would come to behave differently.
    ///
    /// Returns what the server has to be told, when the thing acted on is
    /// server-owned: a scripted prim pressed, a physical one picked up or let go.
    ///
    /// Returned rather than sent from here, because this type has no session and
    /// should not grow one: it is read by the HUD and written by two different
    /// systems, and the network handle belongs to the caller.
    #[must_use]
    fn act(&mut self, avatar_state: &mut AvatarState) -> Option<WorldAct> {
        // At the wheel the control gets out (`systems::driving`), and while
        // walking up to a car or away from one it does nothing.
        if avatar_state.car_control {
            if avatar_state.driving.is_some() {
                avatar_state.wants_out = true;
            }
            self.last = None;
            self.last_ttl = 0.0;
            return None;
        }
        // Sitting is the one interaction that is a *state* rather than a gesture,
        // so the same control leaves it. Asked for while seated, this is "stand
        // up" whatever the prompt is standing next to.
        if avatar_state.is_seated() {
            avatar_state.stand_up();
            self.last = None;
            self.last_ttl = 0.0;
            return None;
        }
        // Carrying is the other state, and the same control throws: E picks
        // a ball up and E throws it, as a kick is one key.
        if avatar_state.holding.is_some() {
            return self.let_go(avatar_state, true);
        }
        let Some(target) = self.target.clone() else {
            return None;
        };
        if let Some(car) = target.car {
            // The car is the receipt: the avatar walks to it and gets in.
            avatar_state.wants_car = Some(car);
            self.last = None;
            self.last_ttl = 0.0;
            return None;
        }
        if let (true, Some(id)) = (target.pickup, target.prim_id) {
            // No receipt: the thing in the hands is the feedback, and the
            // prompt turns into "Throw" as soon as the sim says so.
            avatar_state.pending_emote = Some(Emote::Interact);
            self.last = None;
            self.last_ttl = 0.0;
            return Some(WorldAct::Grab(id));
        }
        if let Some(pole) = target.button {
            // The reach is the press; the crossing's lamps and the prompt's
            // "wait for green" are the receipt.
            avatar_state.pending_emote = Some(Emote::Interact);
            self.pressed_button = Some(pole);
            self.last = None;
            self.last_ttl = 0.0;
            return None;
        }
        if let Some(seat) = target.seat {
            // The offset, not the destination: the sim owns the avatar's position
            // and knows nothing about benches, so the seat is carried as a
            // displacement of the model (`resources::Seated`). Measured against
            // the same `position` the prompt's proximity test used, so the two
            // cannot disagree about which frame they are in.
            let from = Vec2::new(avatar_state.position.x, avatar_state.position.z);
            avatar_state.seated = Some(Seated {
                offset: seat.pos - from,
                yaw: seat.yaw,
                blend: 0.0,
                rising: false,
                lift: 0.0,
                dangle: false,
                edge: false,
            });
            // No receipt: the body is the feedback, and it stays there. A line
            // reading "Sit at Bench" fading out under a seated avatar would be
            // reporting an event that is still going on.
            self.last = None;
            self.last_ttl = 0.0;
            return None;
        }
        // The same one-shot channel the emote keys use, so acting on the world and
        // emoting are the same motion as far as the animation and the wire are
        // concerned — which is exactly right, since `Interact` *is* the clip for
        // "use the thing in front of me".
        avatar_state.pending_emote = Some(Emote::Interact);
        self.last = Some(target.line());
        self.last_ttl = LAST_LINE_SECS;
        target.prim_id.map(WorldAct::Touch)
    }

    /// Let go of what is carried: a throw (E, or the prompt's "Throw") or a
    /// set-down (G, or "Put down"). `None` with empty hands.
    ///
    /// The throw plays `throw`, an overhand throw cut from CMU 15_12 (it was
    /// the punch until 2026-11-15, the pack having no throw), and the set-down
    /// `interact`, the reach the pick-up played.
    ///
    /// The throw is not sent here but [`THROW_RELEASE_SECS`] later, by
    /// [`Self::throw_due`], when the arm in the clip lets go; a second press
    /// while the arm is drawing back does nothing. A put-down goes at once,
    /// and cancels a throw under way.
    #[must_use]
    fn let_go(&mut self, avatar_state: &mut AvatarState, throw: bool) -> Option<WorldAct> {
        avatar_state.holding?;
        if throw && self.throw_in.is_some() {
            return None;
        }
        avatar_state.pending_emote = Some(if throw { Emote::Throw } else { Emote::Interact });
        self.last = None;
        self.last_ttl = 0.0;
        if throw {
            self.throw_in = Some(THROW_RELEASE_SECS);
            None
        } else {
            self.throw_in = None;
            Some(WorldAct::Release { throw: false })
        }
    }

    /// Advance a throw under way by `dt`: the release, when the arm reaches
    /// it. A throw whose hands emptied meanwhile (the sim let go: a teleport,
    /// the water) is dropped.
    #[must_use]
    fn throw_due(&mut self, avatar_state: &AvatarState, dt: f32) -> Option<WorldAct> {
        let left = self.throw_in.as_mut()?;
        if avatar_state.holding.is_none() {
            self.throw_in = None;
            return None;
        }
        *left -= dt;
        if *left > 0.0 {
            return None;
        }
        self.throw_in = None;
        Some(WorldAct::Release { throw: true })
    }
}

/// Whether the prompt is offering a greeting to take: one is open, and the
/// avatar is neither seated nor carrying — both have their own prompt, and
/// the sim would refuse a seated one anyway.
fn accepts_greeting(
    avatar_state: &AvatarState,
    greetings: &crate::systems::greet::Greetings,
) -> bool {
    !avatar_state.is_seated() && avatar_state.holding.is_none() && greetings.prompt().is_some()
}

/// Emote hotkeys. Digits, because they are the row every game puts emotes on and
/// nothing else in this client binds them.
const EMOTE_KEYS: [(KeyCode, Emote); 6] = [
    (KeyCode::Digit1, Emote::Wave),
    (KeyCode::Digit2, Emote::Interact),
    (KeyCode::Digit3, Emote::Roll),
    (KeyCode::Digit4, Emote::Punch),
    (KeyCode::Digit5, Emote::Kick),
    (KeyCode::Digit6, Emote::Faint),
];

/// Expression hotkeys, with Shift held: the order of `Expression::ALL`.
const EXPRESSION_KEYS: [(KeyCode, cartopolis_core::Expression); 8] = {
    use cartopolis_core::Expression as E;
    [
        (KeyCode::Digit1, E::Happy),
        (KeyCode::Digit2, E::Laugh),
        (KeyCode::Digit3, E::Sad),
        (KeyCode::Digit4, E::Angry),
        (KeyCode::Digit5, E::Surprised),
        (KeyCode::Digit6, E::Afraid),
        (KeyCode::Digit7, E::Disgusted),
        (KeyCode::Digit8, E::Wink),
    ]
};

/// Find what the avatar is standing next to, and read the nearest house number.
///
/// Runs only in Follow: the other camera modes decouple the camera from the
/// avatar entirely, and a prompt for something next to an avatar you are not
/// looking at is noise.
pub fn track_interaction_target(
    time: Res<Time>,
    camera_state: Res<CameraState>,
    avatar_state: Res<AvatarState>,
    places: Res<WorldPlaces>,
    facts: Res<crate::systems::building_facts::BuildingFacts>,
    anchor: Res<MapAnchor>,
    prims: Query<(
        &crate::components::Prim,
        &crate::components::PrimGeo,
        &Transform,
    )>,
    mut state: ResMut<InteractState>,
    detail: Res<crate::systems::street_detail::StreetDetail>,
    buttons: Res<crate::systems::give_way::SignalButtons>,
    clock: Res<crate::systems::anim_clock::AnimClock>,
    garage: Res<crate::systems::driving::Garage>,
) {
    if state.last_ttl > 0.0 {
        state.last_ttl -= time.delta_secs();
        if state.last_ttl <= 0.0 {
            state.last = None;
            state.last_ttl = 0.0;
        }
    }

    if camera_state.mode != CameraMode::Avatar {
        if state.target.is_some() {
            state.target = None;
        }
        return;
    }

    // `position` is the avatar's absolute world XZ — the same frame the places
    // were converted into on insertion, and the same one the building colliders
    // use. No transform needed here, which is the point of converting once.
    let pos = Vec2::new(avatar_state.position.x, avatar_state.position.z);

    // In a car, or getting into or out of one: nothing whizzing past is
    // offered, and the prompt is "Get out" (`systems::driving`).
    let near_car = garage
        .left
        .filter(|l| l.pos.distance(pos) < crate::systems::driving::CAR_REACH_M);
    let call_car = !avatar_state.car_control
        && garage
            .left
            .is_none_or(|l| l.pos.distance(pos) > CALL_CAR_BEYOND_M);
    if state.call_car != call_car {
        state.call_car = call_car;
    }
    if avatar_state.car_control {
        if state.target.is_some() {
            state.target = None;
        }
        return;
    }
    // This person's own car, beside them: drive it.
    if let Some(spot) = near_car {
        let same = state.target.as_ref().is_some_and(|t| t.car == Some(spot));
        if !same {
            state.target = Some(TargetInfo {
                name: "my car".to_string(),
                glyph: crate::systems::icons::CAR,
                verb: "Drive",
                detail: None,
                pos: spot.pos,
                seat: None,
                prim_id: None,
                pickup: false,
                button: None,
                car: Some(spot),
            });
        }
        return;
    }

    // Hands full: the prompt is "Throw" and "Put down", whatever is near, so
    // there is no target — only the name of what is held, for the button.
    if let Some(held) = avatar_state.holding {
        let name = prims
            .iter()
            .find(|(p, ..)| p.id == held)
            .map(|(p, ..)| prim_label(&p.name));
        if state.held_name != name {
            state.held_name = name;
        }
        if state.target.is_some() {
            state.target = None;
        }
        let address = nearest_address(&facts, &places, &anchor, &avatar_state, pos);
        if state.address != address {
            state.address = address;
        }
        return;
    }
    if state.held_name.is_some() {
        state.held_name = None;
    }

    // A loose thing small enough to pick up wins over everything else in
    // reach: it is the one prompt that is about what is at your feet rather
    // than what you are standing beside, and a ball rolled up against a
    // scripted goal is still the ball you walked up to.
    if let Some(hit) = nearest_pickable_prim(&prims, &anchor, pos) {
        let same = state
            .target
            .as_ref()
            .is_some_and(|t| t.pickup && t.prim_id == Some(hit.0));
        if !same {
            state.target = Some(TargetInfo::of_pickup(hit.0, &hit.1, hit.2));
        }
        let address = nearest_address(&facts, &places, &anchor, &avatar_state, pos);
        if state.address != address {
            state.address = address;
        }
        return;
    }

    // Compare before building: this resource is read by the HUD, and rewriting it
    // every frame would mark it changed every frame. The comparison is against the
    // same three fields `line()` formats, so the update rule is unchanged — but
    // doing it this way allocates nothing in the steady state. Constructing the
    // `TargetInfo` first cost a `String` clone *plus* the two `format!`s `line()`
    // does on each side, every frame, and in a city the radius nearly always hits.
    // A scripted prim standing next to the avatar wins over a streamed place, and
    // deliberately: a place is scenery the map happens to name, while a prim with
    // a script is something somebody *put* there for you to press. Where both are
    // in range, the built thing is the one being walked up to.
    //
    // Matched through the geo anchor rather than the prim's `Transform`, because
    // the transform is a `big_space` cell-local offset and the avatar's position
    // is absolute anchored world XZ — comparing the two directly is the frame
    // mistake this whole feature is about.
    if let Some(hit) = nearest_scripted_prim(&prims, &anchor, pos) {
        let same = state
            .target
            .as_ref()
            .is_some_and(|t| !t.pickup && t.prim_id == Some(hit.0) && t.name == hit.1);
        if !same {
            state.target = Some(TargetInfo::of_prim(hit.0, &hit.1, hit.2));
        }
        state.address = nearest_address(&facts, &places, &anchor, &avatar_state, pos);
        return;
    }

    // A signal pole with a push button (`systems::give_way`): before the
    // places, because it is the thing the avatar walked up to at a kerb, and
    // a café across the pavement is not.
    if let Some(s) = crate::systems::give_way::button_near(&detail, pos) {
        let phase = buttons.phase(s.at, s.group, clock.elapsed_secs());
        let detail_now = phase.map(crate::systems::give_way::ButtonPhase::detail);
        let same = state
            .target
            .as_ref()
            .is_some_and(|t| t.button == Some(s.pole) && t.detail == detail_now);
        if !same {
            state.target = Some(TargetInfo::of_button(s.pole, phase));
        }
        let address = nearest_address(&facts, &places, &anchor, &avatar_state, pos);
        if state.address != address {
            state.address = address;
        }
        return;
    }

    let nearest = places.nearest(pos, INTERACT_RADIUS);
    let changed = match (&state.target, nearest) {
        (Some(a), Some(b)) => {
            a.verb != b.kind.verb() || a.detail != b.kind.detail() || a.name != b.name
        }
        (None, None) => false,
        _ => true,
    };
    if changed {
        state.target = nearest.map(|i| {
            let mut t = TargetInfo::of(i, pos);
            // A parked car is driven away (`systems::driving`).
            if i.kind
                == crate::systems::world_places::InteractKind::Prop(
                    crate::systems::furniture::FurnitureKind::Car,
                )
            {
                t.car = places
                    .parked_car_at(i.pos)
                    .map(|c| crate::systems::driving::CarSpot::parked(&c));
            }
            t
        });
    } else if let (Some(t), Some(i)) = (state.target.as_mut(), nearest) {
        // Same thing, but not necessarily the same *place* on it: walking along a
        // bench slides where you would sit, and a seat resolved when you first
        // came into range would put you back at the end you arrived at. Allocates
        // nothing, and only moves while you do.
        let seat = i.seat_spot(pos);
        if seat != t.seat {
            t.seat = seat;
        }
    }

    let address = nearest_address(&facts, &places, &anchor, &avatar_state, pos);
    if state.address != address {
        state.address = address;
    }
}

/// The house number for the HUD readout: the BAG's own line where there is one,
/// the tile layer's generalised number otherwise.
///
/// The BAG answer wins because both are the same register in the end, but the
/// tile layer carries one point per *building* generalised into a vector tile
/// while `building_facts` carries the register's row per *unit* — the point sits
/// at the entrance rather than the centroid, the street name is spelled out
/// rather than joined from whatever the tile happened to hold, and the year and
/// the use come with it.
fn nearest_address(
    facts: &crate::systems::building_facts::BuildingFacts,
    places: &WorldPlaces,
    anchor: &MapAnchor,
    avatar_state: &AvatarState,
    pos: Vec2,
) -> Option<String> {
    bag_address(facts, anchor, avatar_state).or_else(|| {
        places
            .nearest_address(pos, ADDRESS_RADIUS)
            .map(str::to_owned)
    })
}

/// The nearest scripted prim within reach: `(id, name, anchored world XZ)`.
///
/// Only *scripted* prims (protocol 21's `PrimDto::scripted`), because the prompt
/// is an offer to press and an unscripted prim does nothing when pressed —
/// offering it would put a prompt on every crate somebody ever built.
///
/// The radius is the same one places use, plus the prim's own footprint: a place
/// is a point and a prim is an object you can be standing against, so measuring
/// to its centre would make a wide panel unreachable while you are touching it.
fn nearest_scripted_prim(
    prims: &Query<(
        &crate::components::Prim,
        &crate::components::PrimGeo,
        &Transform,
    )>,
    anchor: &MapAnchor,
    pos: Vec2,
) -> Option<(i64, String, Vec2)> {
    let geo = anchor.geo?;
    let mut best: Option<(f32, i64, String, Vec2)> = None;
    for (prim, at, tf) in prims.iter() {
        if !prim.scripted {
            continue;
        }
        let (wx, wz) = crate::grid::lat_lng_to_world(geo, at.latitude, at.longitude);
        let here = Vec2::new(wx as f32, wz as f32);
        // Half the widest horizontal axis: the distance from the middle to the
        // face you would be standing at.
        let reach = INTERACT_RADIUS + tf.scale.x.max(tf.scale.z) * 0.5;
        let d = here.distance(pos);
        if d > reach {
            continue;
        }
        if best.as_ref().is_none_or(|(bd, ..)| d < *bd) {
            best = Some((d, prim.id, prim.name.clone(), here));
        }
    }
    best.map(|(_, id, name, at)| (id, name, at))
}

/// Whether "Pick up" is offered for this prim: physical, and small and light
/// enough by the sim's own rule (`carry::can_carry`). The sim still decides —
/// reach, and whether somebody else has it, are its to check.
#[must_use]
pub fn pickable(prim: &crate::components::Prim, scale: Vec3) -> bool {
    prim.physical && carry::can_carry(prim.shape.as_str(), scale, prim.model.is_some())
}

/// The nearest loose physical prim within [`PICKUP_RADIUS`] small enough to
/// carry: `(id, name, anchored world XZ)`. Through the geo anchor, as
/// [`nearest_scripted_prim`] is, and for the same reason; a physical prim's
/// anchor follows its `PrimMotion`.
fn nearest_pickable_prim(
    prims: &Query<(
        &crate::components::Prim,
        &crate::components::PrimGeo,
        &Transform,
    )>,
    anchor: &MapAnchor,
    pos: Vec2,
) -> Option<(i64, String, Vec2)> {
    let geo = anchor.geo?;
    let mut best: Option<(f32, i64, &str, Vec2)> = None;
    for (prim, at, tf) in prims.iter() {
        if !pickable(prim, tf.scale) {
            continue;
        }
        let (wx, wz) = crate::grid::lat_lng_to_world(geo, at.latitude, at.longitude);
        let here = Vec2::new(wx as f32, wz as f32);
        let d = here.distance(pos);
        if d <= PICKUP_RADIUS && best.as_ref().is_none_or(|(bd, ..)| d < *bd) {
            best = Some((d, prim.id, prim.name.as_str(), here));
        }
    }
    best.map(|(_, id, name, at)| (id, name.to_string(), at))
}

/// The BAG's own line for wherever the avatar is standing: the nearest addressed
/// unit, and what the register says about the building it is in.
///
/// `"Paterswoldseweg 62 · home · 1884"`. `None` outside the BAG coverage, which
/// is everywhere the host has not extracted the BAG for — the tile-layer
/// house number then answers as it always did.
fn bag_address(
    facts: &crate::systems::building_facts::BuildingFacts,
    anchor: &MapAnchor,
    avatar_state: &AvatarState,
) -> Option<String> {
    if facts.is_empty() {
        return None;
    }
    // No anchor yet means there is no mapping from world XZ to lat/lng at all,
    // so there is nothing to look up rather than a place to guess at.
    let (lat, lng) = world_to_lat_lng(
        anchor.geo?,
        avatar_state.position.x as f64,
        avatar_state.position.z as f64,
    );
    let Some(unit) = facts.nearest_address(lat, lng, ADDRESS_RADIUS as f64) else {
        // No addressed unit in range, but a canal bank has berths on it and they
        // are addresses with no building — the one kind of place a map drawn from
        // buildings cannot name at all.
        return nearest_berth(facts, lat, lng);
    };
    let mut line = unit.label()?;
    if let Some(pc) = &unit.postcode {
        line.push_str(" · ");
        line.push_str(pc);
    }
    // The unit's own use where the register gives one, else the building's — a
    // flat above a shop is a home, and saying "shop" for it would be worse than
    // saying nothing.
    let building = unit.pand.as_deref().and_then(|id| facts.building(id));
    let uses = if unit.uses != 0 {
        unit.uses
    } else {
        building.map_or(0, |b| b.uses)
    };
    if let Some(what) = crate::systems::building_facts::use_label(uses) {
        line.push_str(" · ");
        line.push_str(what);
    }
    // The unit's own floor area, or — where the register has none for it — the
    // spread across the building, which for a block of flats is the honest answer
    // and for a house is one number anyway.
    match (
        unit.area_m2,
        building.and_then(|b| b.area_m2),
        building.and_then(|b| b.area_max_m2),
    ) {
        (Some(a), _, _) => line.push_str(&format!(" · {a} m²")),
        (None, Some(a), Some(b)) => line.push_str(&format!(" · {a}–{b} m²")),
        (None, Some(a), None) => line.push_str(&format!(" · {a} m²")),
        (None, None, _) => {}
    }
    if let Some(year) = building.and_then(|b| b.year) {
        line.push_str(&format!(" · {year}"));
    }
    // Only worth saying when it is *not* the ordinary answer — a building the
    // register has taken out of use is the kind of thing you would want to know
    // you were standing in front of.
    if building.is_some_and(|b| !b.status.standing()) {
        line.push_str(" · disused");
    }
    if let Some(b) = building {
        push_heritage(&mut line, b);
    }
    Some(line)
}

/// Append what the heritage registers say, if anything: whether the building is
/// listed, and whether it stands in a protected historic district.
///
/// Last on the line and in that order, because they are the rarest facts and the
/// readout truncates from the right — a house number must never be pushed off by
/// a district name that applies to half the city centre.
fn push_heritage(line: &mut String, building: &crate::systems::building_facts::BuildingFact) {
    if building.monument.is_some() {
        line.push_str(" · national monument");
        // The register's own category, in its own language. It is the difference
        // between "a listed building" and "a listed church", which is most of
        // what a person wants from the word.
        if let Some(cat) = &building.monument_category {
            line.push_str(" (");
            line.push_str(cat);
            line.push(')');
        }
    }
    if let Some(district) = &building.district {
        line.push_str(" · in ");
        line.push_str(district);
    }
}

/// The nearest houseboat berth or mobile-home plot, named.
///
/// Linear over the loaded berths, which number in the tens per cell rather than
/// the thousands — this is the rarer answer and it does not deserve an index.
fn nearest_berth(
    facts: &crate::systems::building_facts::BuildingFacts,
    lat: f64,
    lng: f64,
) -> Option<String> {
    use crate::systems::building_facts::BerthKind;
    const M_PER_DEG: f64 = 111_320.0;
    let scale = lat.to_radians().cos();
    let radius_m = ADDRESS_RADIUS as f64;
    let mut best: Option<(f64, &crate::systems::building_facts::BerthFact)> = None;
    for b in facts.berths() {
        let dy = (b.at.0 - lat) * M_PER_DEG;
        let dx = (b.at.1 - lng) * M_PER_DEG * scale;
        let d2 = dx * dx + dy * dy;
        if d2 <= radius_m * radius_m && best.is_none_or(|(d, _)| d2 < d) {
            best = Some((d2, b));
        }
    }
    let (_, berth) = best?;
    let mut line = match (berth.street.as_deref(), berth.number.as_deref()) {
        (Some(s), Some(n)) => format!("{s} {n}"),
        (Some(s), None) => s.to_string(),
        (None, Some(n)) => n.to_string(),
        (None, None) => String::new(),
    };
    if let Some(pc) = &berth.postcode {
        if !line.is_empty() {
            line.push_str(" · ");
        }
        line.push_str(pc);
    }
    if !line.is_empty() {
        line.push_str(" · ");
    }
    line.push_str(match berth.kind {
        BerthKind::Mooring => "houseboat berth",
        BerthKind::Plot => "mobile-home plot",
    });
    Some(line)
}

/// E acts on whatever the prompt is offering; the digits play an emote.
///
/// Both write through `AvatarState`, which is what puts them on the wire: the
/// emote is replicated so other players see it, and the interact gesture plays
/// the same `Interact` clip locally. Neither reads the keyboard when the UI owns
/// it — a search box is full of `e`s.
pub fn handle_interaction_input(
    keyboard: Res<ButtonInput<KeyCode>>,
    camera_state: Res<CameraState>,
    ui_input: Res<UiInput>,
    edit_dialog: Res<crate::resources::EditDialogState>,
    online: Option<Res<crate::resources::OnlineSession>>,
    mut avatar_state: ResMut<AvatarState>,
    mut state: ResMut<InteractState>,
    mut faces: MessageWriter<crate::systems::avatar_face::StartExpression>,
    mut greetings: ResMut<crate::systems::greet::Greetings>,
    time: Res<Time>,
    edges: Res<crate::systems::climb::Edges>,
) {
    // Carrying is the sim's, so with no sim there is nothing in the hands —
    // a session that ended mid-carry leaves no "Throw" behind it.
    if online.is_none() && avatar_state.holding.is_some() {
        avatar_state.holding = None;
    }
    // A throw under way lets go when the arm does, whatever has the keyboard.
    let due = state.throw_due(&avatar_state, time.delta_secs());
    send_act(online.as_ref(), due);
    // Into the water, the thing is put down. The sim knows nothing of water
    // (it is every viewer's own map, `avatar-locomotion.md`), so this is the
    // one end of a carry the client asks for; once per carried prim, not every
    // frame until the snapshot that empties the hands.
    if avatar_state.swimming && avatar_state.holding.is_some() {
        if state.let_go_sent != avatar_state.holding {
            state.let_go_sent = avatar_state.holding;
            send_act(online.as_ref(), Some(WorldAct::Release { throw: false }));
        }
    } else if state.let_go_sent.is_some() && avatar_state.holding.is_none() {
        state.let_go_sent = None;
    }

    if camera_state.mode != CameraMode::Avatar
        || ui_input.ui_owns_keyboard(edit_dialog.visible, &keyboard)
    {
        return;
    }

    // Shift and a digit pull a face, the digit row's second layer. Only the
    // face moves, so a seated avatar may.
    if keyboard.any_pressed([KeyCode::ShiftLeft, KeyCode::ShiftRight]) {
        for (key, expression) in EXPRESSION_KEYS {
            if keyboard.just_pressed(key) {
                faces.write(crate::systems::avatar_face::StartExpression(expression));
            }
        }
        return;
    }

    // Not while sitting. The emote clips are whole-body motions authored for a
    // standing rig, and the seated pose is holding the legs and the pelvis — a
    // wave played over it swings the arms of a body that is half on a bench. The
    // wire carries emotes, so playing one anyway would also show every other
    // player a gesture this client cannot itself draw.
    if !avatar_state.is_seated() {
        for (key, emote) in EMOTE_KEYS {
            if keyboard.just_pressed(key) {
                avatar_state.pending_emote = Some(emote);
            }
        }
    }

    if keyboard.just_pressed(KeyCode::KeyE) {
        // Somebody offering a greeting outranks what the avatar stands next
        // to: the prompt shows the offer first, so E takes it.
        match online.as_deref() {
            Some(session) if accepts_greeting(&avatar_state, &greetings) => {
                greetings.accept(session);
            }
            _ if offers_edge(state.target.as_ref(), &edges, &avatar_state) => {
                edges.act(&mut avatar_state);
            }
            _ => send_act(online.as_ref(), state.act(&mut avatar_state)),
        }
    } else if keyboard.just_pressed(KeyCode::KeyC) && state.call_car && !avatar_state.is_seated() {
        // C calls this person's own car (`systems::driving`).
        avatar_state.wants_call_car = true;
    } else if keyboard.just_pressed(KeyCode::KeyG) {
        // G puts down: the key games drop with, and bound to nothing else.
        send_act(online.as_ref(), state.let_go(&mut avatar_state, false));
    }
}

/// Decide what the prompt over the map offers, publish it for the library
/// (`lib_interact`), and do what was tapped on it last frame.
///
/// Anchored to the bottom-centre of the *map area* rather than the window:
/// the rect the panels left at the end of the last pass
/// (`EguiVerdict::map_rect`), so a side panel never covers it.
///
/// It used to read `Sit at Bench   [E]` and not be pressable — a keyboard
/// hint on the one platform with no keyboard, advertising an action the prompt
/// itself could not take. The line is a button now, and the key hint only appears
/// on a viewport wide enough to imply a keyboard. The second button is the one
/// this module could not offer at all: a POI you walked up to is a *destination*,
/// and there was no way to make it one.
pub fn drive_interaction_prompt(
    ui_input: Res<UiInput>,
    metrics: Res<UiMetrics>,
    camera_state: Res<CameraState>,
    mut state: ResMut<InteractState>,
    mut avatar_state: ResMut<AvatarState>,
    mut nav: ResMut<GeoNavState>,
    map_anchor: Res<MapAnchor>,
    driver: Res<crate::systems::driver::DriverState>,
    online: Option<Res<crate::resources::OnlineSession>>,
    mut greetings: ResMut<crate::systems::greet::Greetings>,
    edges: Res<crate::systems::climb::Edges>,
) {
    // A tap on the library's prompt last frame (`lib_interact`), done below.
    // One a frame, as a pointer gives.
    let asked = std::mem::take(&mut state.lib_acts).pop();
    // Driver mode sets `CameraMode::Avatar` itself, so the mode test below is
    // true throughout a trip and this prompt kept appearing — "Sit at Bench" at
    // bottom-centre, in the speed pill's band, while driving past one.
    if driver.active || camera_state.mode != CameraMode::Avatar {
        hide_lib_prompt(&mut state);
        return;
    }
    let target = state.target.clone();
    let receipt = state.last.clone();
    // A seated avatar always has a prompt, whatever it is standing next to —
    // getting up is the one thing it can do, and on a phone this button is the
    // only way to ask for it.
    let seated = avatar_state.is_seated();
    // So does a carrying one: "Throw" and "Put down" are the two ways out, and
    // on a phone these buttons are the only way to either.
    let carrying = !seated && avatar_state.holding.is_some();
    let held_name = state.held_name.clone();
    // At the wheel the prompt is "Get out", in place of "Stand up".
    let driving = avatar_state.driving.is_some();
    let call_car = state.call_car && !seated && !carrying;
    // Somebody offering a greeting: the first thing the prompt says, since it
    // lapses and the bench beside you does not.
    let offer = accepts_greeting(&avatar_state, &greetings)
        .then(|| greetings.prompt().cloned())
        .flatten();
    // The wall or edge in front of the avatar (`systems::climb`): "Climb up"
    // and "Sit", in place of the place nearby.
    let edge = offers_edge(target.as_ref(), &edges, &avatar_state).then_some(*edges);
    if target.is_none()
        && receipt.is_none()
        && !seated
        && !carrying
        && offer.is_none()
        && edge.is_none()
        && !call_car
    {
        hide_lib_prompt(&mut state);
        return;
    }

    let compact = metrics.compact();
    // The map in UI points: what the panels left at the end of the last pass
    // (window points), or the whole viewport before there is one.
    let map = ui_input.map_rect().map_or_else(
        || Rect::from_corners(Vec2::ZERO, metrics.viewport_pts()),
        |r| Rect::from_corners(metrics.window_to_ui(r.min), metrics.window_to_ui(r.max)),
    );
    if map.width() < 1.0 || map.height() < 1.0 {
        hide_lib_prompt(&mut state);
        return;
    }
    let pos = Vec2::new(map.center().x, map.max.y - PROMPT_BOTTOM_MARGIN);
    let items = prompt_items(
        compact,
        target.as_ref(),
        seated,
        carrying,
        held_name.as_deref(),
        offer.as_ref(),
        edge.as_ref(),
        Cars { driving, call_car },
    );

    // Nothing in range: what is left is the line naming what was last acted
    // on, which is a receipt and not a button.
    let view = PromptView {
        x: pos.x,
        y: pos.y,
        receipt: if items.is_empty() { receipt } else { None },
        items,
    };
    if state.lib_view.as_ref() != Some(&view) {
        state.lib_view = Some(view);
    }

    match asked {
        Some(PromptAct::Greet) => {
            if let Some(session) = online.as_deref() {
                greetings.accept(session);
            }
        }
        Some(PromptAct::Ignore) => greetings.dismiss(),
        Some(PromptAct::Act) => send_act(online.as_ref(), state.act(&mut avatar_state)),
        Some(PromptAct::PutDown) => {
            send_act(online.as_ref(), state.let_go(&mut avatar_state, false));
        }
        Some(PromptAct::ClimbUp) => avatar_state.wants_climb = true,
        Some(PromptAct::CallCar) => avatar_state.wants_call_car = true,
        Some(PromptAct::SitDown) => edges.sit(&mut avatar_state),
        Some(PromptAct::Route) => {
            // The store holds anchored world XZ; the planner speaks lat/lng.
            // Without an anchor there is no mapping between them yet, and the
            // button simply does nothing rather than routing to null island.
            if let (Some(t), Some(anchor)) = (&target, map_anchor.geo) {
                let (lat, lng) = world_to_lat_lng(anchor, t.pos.x as f64, t.pos.y as f64);
                nav.route_to(t.name.clone(), lat, lng);
            }
        }
        None => {}
    }
}

/// The library's prompt goes (`lib_interact`), when it was up.
fn hide_lib_prompt(state: &mut InteractState) {
    if state.lib_view.is_some() {
        state.lib_view = None;
    }
}

/// The prompt's controls, left to right, for both editions: a greeting
/// offered first, then getting up, then the two ways out of carrying, then
/// the wall or edge in front, then what the avatar stands next to. Empty when
/// there is nothing to press (what is left is the receipt).
/// What the prompt offers about cars (`systems::driving`).
#[derive(Clone, Copy, Debug, Default)]
struct Cars {
    /// At the wheel: "Get out".
    driving: bool,
    /// On foot with no car of one's own near: the small "Call my car".
    call_car: bool,
}

/// Beyond this, a car left at the kerb is far enough away to call it, m.
const CALL_CAR_BEYOND_M: f32 = 40.0;

#[allow(clippy::too_many_arguments)]
fn prompt_items(
    compact: bool,
    target: Option<&TargetInfo>,
    seated: bool,
    carrying: bool,
    held_name: Option<&str>,
    offer: Option<&crate::systems::greet::Offer>,
    edge: Option<&crate::systems::climb::Edges>,
    cars: Cars,
) -> Vec<PromptItem> {
    use crate::systems::icons;
    // The key hint is a fact about the *input*, not about the action, so it
    // is only stated where a keyboard is plausible.
    let hint = if compact { "" } else { "   [E]" };
    if let Some(o) = offer {
        // A button to take it — E's job on a keyboard, the only way on a
        // phone — and one to let it go unanswered.
        return vec![
            PromptItem::Button {
                glyph: icons::HAND,
                label: format!("{}{hint}", o.line()),
                act: PromptAct::Greet,
            },
            PromptItem::Icon {
                glyph: icons::CLEAR,
                hint: "Not now",
                act: PromptAct::Ignore,
            },
        ];
    }
    if cars.driving {
        return vec![PromptItem::Button {
            glyph: icons::CAR,
            label: format!("Get out{hint}"),
            act: PromptAct::Act,
        }];
    }
    if seated {
        // The glyph is the thing being sat on where there is one, so the
        // button that put you there and the button that takes you off it are
        // visibly the same control.
        return vec![PromptItem::Button {
            glyph: target.map_or("", |t| t.glyph),
            label: format!("Stand up{hint}"),
            act: PromptAct::Act,
        }];
    }
    if carrying {
        // Two buttons rather than a long press for the second, because a long
        // press on a phone already means the context menu (`touch-input.md`),
        // and both are the same kind of control the rest of the prompt is.
        let what = held_name.unwrap_or("it");
        let down_hint = if compact { "" } else { "   [G]" };
        return vec![
            PromptItem::Button {
                glyph: icons::SEND,
                label: format!("Throw {what}{hint}"),
                act: PromptAct::Act,
            },
            PromptItem::Button {
                glyph: icons::DOWNLOAD,
                label: format!("Put down{down_hint}"),
                act: PromptAct::PutDown,
            },
        ];
    }
    if let Some(e) = edge {
        // E takes the first: the climb where there is one. The seat beside it
        // is a button, on a keyboard as on a phone.
        let mut items = Vec::new();
        if e.climb.is_some() {
            items.push(PromptItem::Button {
                glyph: icons::LANDSCAPE,
                label: format!("Climb up{hint}"),
                act: PromptAct::ClimbUp,
            });
        }
        if e.sit.is_some() {
            let hint = if e.climb.is_some() { "" } else { hint };
            items.push(PromptItem::Button {
                glyph: icons::BENCH,
                label: format!("Sit{hint}"),
                act: PromptAct::SitDown,
            });
        }
        return items;
    }
    let Some(t) = target else {
        return if cars.call_car {
            let hint = if compact { "" } else { "   [C]" };
            vec![PromptItem::Button {
                glyph: icons::CAR,
                label: format!("Call my car{hint}"),
                act: PromptAct::CallCar,
            }]
        } else {
            Vec::new()
        };
    };
    vec![
        PromptItem::Button {
            glyph: t.glyph,
            label: format!("{}{hint}", t.line()),
            act: PromptAct::Act,
        },
        // The Route panel's own icon, so the button and the panel it opens
        // read as one thing.
        PromptItem::Icon {
            glyph: RightPanel::Route.icon(),
            hint: "Route here — make this the destination",
            act: PromptAct::Route,
        },
    ]
}

/// Gap between the prompt and the bottom of the map area. Clear of the bottom
/// bar the driver mode and chat put there.
const PROMPT_BOTTOM_MARGIN: f32 = 96.0;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::systems::furniture::FurnitureKind;
    use crate::systems::world_places::InteractKind;

    fn target(name: &str, kind: InteractKind) -> TargetInfo {
        TargetInfo::of(
            &Interactable {
                pos: Vec2::ZERO,
                yaw: 0.0,
                name: name.to_string(),
                kind,
            },
            // Standing just in front of it, which is where the prompt appears.
            Vec2::new(0.0, 1.0),
        )
    }

    /// A prop's line reads as an action on a thing, with no parenthetical
    /// restating what the name already says.
    #[test]
    fn a_prop_prompt_does_not_stutter() {
        let bench = target("Bench", InteractKind::Prop(FurnitureKind::Bench));
        assert_eq!(bench.line(), "Sit at Bench");
        let board = target(
            "Information board",
            InteractKind::Prop(FurnitureKind::Board),
        );
        assert_eq!(board.line(), "Read Information board");
    }

    /// A named place carries a description, because the name alone rarely says
    /// what the thing is — iD's specific one when the schema had a preset for
    /// the tag, and the coarse category's word when it did not.
    #[test]
    fn a_place_prompt_describes_the_place() {
        use crate::systems::places::PoiCategory;
        let bakery = target(
            "Bakkerij Klaas",
            InteractKind::Place {
                category: PoiCategory::Shop,
                kind: Some("Bakery"),
            },
        );
        assert_eq!(bakery.line(), "Look at Bakkerij Klaas (Bakery)");

        // The fallback, which is what every POI read like before the import.
        let unknown = target(
            "Vismarkt",
            InteractKind::Place {
                category: PoiCategory::Drink,
                kind: None,
            },
        );
        assert_eq!(unknown.line(), "Look at Vismarkt (café)");
    }

    /// Tapping the prompt has to do exactly what E does — same clip, same
    /// receipt, same timer — because on a phone it is the *only* way to do it.
    /// One method, so the two cannot drift.
    #[test]
    fn acting_plays_the_clip_and_leaves_a_receipt() {
        let mut state = InteractState::default();
        let mut avatar = AvatarState::default();

        // Nothing in range: pressing anything is a no-op, not a wave at thin air.
        let _ = state.act(&mut avatar);
        assert!(avatar.pending_emote.is_none());
        assert!(state.last.is_none());

        state.target = Some(target("Litter bin", InteractKind::Prop(FurnitureKind::Bin)));
        let _ = state.act(&mut avatar);
        assert!(matches!(avatar.pending_emote, Some(Emote::Interact)));
        assert_eq!(state.last.as_deref(), Some("Use Litter bin"));
        assert!(state.last_ttl > 0.0, "the receipt must fade on its own");
    }

    /// A bench is the one prompt that does not fire a gesture: it puts the avatar
    /// in a state, and the same control takes it out again. The receipt is
    /// deliberately *not* left behind — a line reading "Sit at Bench" fading out
    /// under an avatar that is still sitting there reports a finished event that
    /// has not finished.
    #[test]
    fn sitting_down_is_a_state_and_the_same_button_ends_it() {
        let mut state = InteractState::default();
        let mut avatar = AvatarState::default();
        // Standing where `target` says the prompt was resolved from: a metre in
        // front of a bench at the origin.
        avatar.position = Vec3::new(0.0, 0.0, 1.0);
        state.target = Some(target("Bench", InteractKind::Prop(FurnitureKind::Bench)));

        let _ = state.act(&mut avatar);
        assert!(
            avatar.is_seated(),
            "the bench should have seated the avatar"
        );
        assert!(
            avatar.pending_emote.is_none(),
            "sitting is a pose, not a one-shot gesture"
        );
        assert!(state.last.is_none());
        // The bench in `target` is at the origin and the avatar a metre in front
        // of it, so the seat is the metre back onto it.
        let seat = avatar.seated.expect("a seat");
        assert!(seat.offset.length() > 0.5, "{:?}", seat.offset);
        assert_eq!(seat.blend, 0.0, "it slides on, it does not appear on");

        let _ = state.act(&mut avatar);
        assert!(!avatar.is_seated(), "the same control stands it up");
        assert!(
            avatar.seated.is_some_and(|s| s.rising),
            "…but the seat outlives the decision, so the slide can unwind"
        );
    }

    /// The place to sit is on the bench and turned out of it — not the bench's
    /// own point, which is where the timber is.
    #[test]
    fn a_bench_seats_you_on_it_facing_out() {
        // A bench at the origin, front (+z in its own frame) turned toward +x.
        let bench = Interactable {
            pos: Vec2::ZERO,
            yaw: std::f32::consts::FRAC_PI_2,
            name: "Bench".to_string(),
            kind: InteractKind::Prop(FurnitureKind::Bench),
        };
        let spot = bench
            .seat_spot(Vec2::new(1.2, 0.0))
            .expect("a bench is sittable");
        assert!((spot.yaw - std::f32::consts::FRAC_PI_2).abs() < 1e-4);
        assert!(spot.pos.length() < 0.1, "sit on the bench: {:?}", spot.pos);

        // Walk to one end and you sit at that end, not in the middle.
        let end = bench
            .seat_spot(Vec2::new(1.2, -0.9))
            .expect("a bench is sittable");
        assert!(end.pos.y < -0.5, "{:?}", end.pos);
    }

    /// The prompt offers to route to what it names, so the target has to remember
    /// where it is — the store is keyed by cell and may have retired the thing
    /// between the prompt appearing and the button being pressed.
    #[test]
    fn a_target_remembers_where_it_is() {
        use crate::systems::places::PoiCategory;
        let it = Interactable {
            pos: Vec2::new(120.0, -40.0),
            yaw: 0.0,
            name: "Vismarkt".to_string(),
            kind: InteractKind::Place {
                category: PoiCategory::Drink,
                kind: None,
            },
        };
        assert_eq!(TargetInfo::of(&it, Vec2::ZERO).pos, Vec2::new(120.0, -40.0));
    }

    /// E on a loose ball asks the sim for it and reaches for it; the prompt
    /// names it as something to pick up.
    #[test]
    fn picking_up_asks_the_sim_and_reaches() {
        let mut state = InteractState::default();
        let mut avatar = AvatarState::default();
        state.target = Some(TargetInfo::of_pickup(42, "Ball", Vec2::ZERO));
        assert_eq!(state.target.as_ref().unwrap().line(), "Pick up Ball");
        assert_eq!(state.act(&mut avatar), Some(WorldAct::Grab(42)));
        assert!(matches!(avatar.pending_emote, Some(Emote::Interact)));
        assert!(state.last.is_none(), "the ball in the hands is the receipt");
        // An unnamed prim still reads as a sentence.
        assert_eq!(
            TargetInfo::of_pickup(1, " ", Vec2::ZERO).line(),
            "Pick up object"
        );
    }

    /// With something in the hands E throws it, whatever the prompt is near,
    /// and G puts it down; empty hands let go of nothing.
    #[test]
    fn carrying_turns_e_into_a_throw_and_g_puts_down() {
        let mut state = InteractState::default();
        let mut avatar = AvatarState::default();
        assert_eq!(state.let_go(&mut avatar, false), None);
        assert!(avatar.pending_emote.is_none());

        avatar.holding = Some(42);
        state.target = Some(TargetInfo::of_pickup(7, "Other ball", Vec2::ZERO));
        // E starts the throw clip; the throw goes when the arm lets go.
        assert_eq!(state.act(&mut avatar), None);
        assert!(matches!(avatar.pending_emote, Some(Emote::Throw)));
        avatar.pending_emote = None;
        assert_eq!(
            state.act(&mut avatar),
            None,
            "a second press is not a throw"
        );
        assert!(avatar.pending_emote.is_none());
        assert_eq!(state.throw_due(&avatar, THROW_RELEASE_SECS - 0.1), None);
        assert_eq!(
            state.throw_due(&avatar, 0.2),
            Some(WorldAct::Release { throw: true })
        );
        assert_eq!(state.throw_due(&avatar, 1.0), None, "once");

        // G puts it down at once, and cancels a throw under way.
        assert_eq!(state.act(&mut avatar), None);
        assert_eq!(
            state.let_go(&mut avatar, false),
            Some(WorldAct::Release { throw: false })
        );
        assert!(matches!(avatar.pending_emote, Some(Emote::Interact)));
        assert_eq!(state.throw_due(&avatar, 2.0), None, "cancelled");
    }

    /// Every emote key is distinct and maps to a distinct emote — a duplicate
    /// would silently shadow one, and the list is the only place it is stated.
    #[test]
    fn the_emote_keys_are_a_bijection() {
        let mut keys: Vec<KeyCode> = EMOTE_KEYS.iter().map(|(k, _)| *k).collect();
        let n = keys.len();
        keys.sort_by_key(|k| format!("{k:?}"));
        keys.dedup();
        assert_eq!(keys.len(), n, "a key is bound twice");

        let mut emotes: Vec<String> = EMOTE_KEYS.iter().map(|(_, e)| format!("{e:?}")).collect();
        emotes.sort();
        emotes.dedup();
        assert_eq!(emotes.len(), n, "an emote is bound twice");
    }
}

/// The prim half of the prompt, against a real `World`.
///
/// A real one because the thing under test is a *query* over spawned entities and
/// the frame conversion inside it — an anchor, a geo coordinate and an avatar
/// position that all have to agree about what "world XZ" means. That agreement is
/// the whole defect class this area has (`PrimGeo` is absolute, a prim's
/// `Transform` is a `big_space` cell-local offset), and it cannot be tested by
/// calling a pure function with numbers already in the right frame.
#[cfg(test)]
mod prim_prompt_tests {
    use super::*;
    use crate::components::{Prim, PrimGeo, PrimShape};
    use crate::grid::lat_lng_to_world;

    /// Groningen's Grote Markt, at the zoom the map anchors at.
    const ANCHOR: (f64, f64, u32) = (53.2194, 6.5665, 16);

    fn a_prim(id: i64, name: &str, shape: PrimShape, scripted: bool, physical: bool) -> Prim {
        Prim {
            id,
            name: name.to_string(),
            shape,
            color: Color::WHITE,
            texture_id: None,
            model: None,
            path_cut_begin: 0.0,
            path_cut_end: 1.0,
            hollow: 0.0,
            twist_begin: 0.0,
            twist_end: 0.0,
            taper_x: 0.0,
            taper_y: 0.0,
            top_shear_x: 0.0,
            top_shear_y: 0.0,
            slice_begin: 0.0,
            slice_end: 1.0,
            surface: cartopolis_core::PrimSurface::default(),
            particles: None,
            scripted,
            video: None,
            sound_loop: None,
            physical,
        }
    }

    /// "Pick up" is offered by the sim's own rule: a physical football yes, the
    /// same ball fixed in place no, a physical crate a metre on a side no.
    #[test]
    fn a_physical_ball_is_pickable_and_a_crate_is_not() {
        let ball = a_prim(1, "Ball", PrimShape::Sphere, false, true);
        assert!(pickable(&ball, Vec3::splat(0.22)));
        let fixed = a_prim(2, "Ball", PrimShape::Sphere, false, false);
        assert!(!pickable(&fixed, Vec3::splat(0.22)));
        let crate_ = a_prim(3, "Crate", PrimShape::Box, false, true);
        assert!(!pickable(&crate_, Vec3::ONE));
        assert!(pickable(&crate_, Vec3::splat(0.4)));
    }

    fn world_with(prims: &[(i64, &str, bool, f64, f64)]) -> World {
        let mut world = World::new();
        for (id, name, scripted, lat, lng) in prims {
            world.spawn((
                Prim {
                    id: *id,
                    name: (*name).to_string(),
                    shape: PrimShape::Box,
                    color: Color::WHITE,
                    texture_id: None,
                    model: None,
                    path_cut_begin: 0.0,
                    path_cut_end: 1.0,
                    hollow: 0.0,
                    twist_begin: 0.0,
                    twist_end: 0.0,
                    taper_x: 0.0,
                    taper_y: 0.0,
                    top_shear_x: 0.0,
                    top_shear_y: 0.0,
                    slice_begin: 0.0,
                    slice_end: 1.0,
                    surface: cartopolis_core::PrimSurface::default(),
                    particles: None,
                    scripted: *scripted,
                    video: None,
                    sound_loop: None,
                    physical: false,
                },
                PrimGeo {
                    latitude: *lat,
                    longitude: *lng,
                },
                Transform::default(),
            ));
        }
        world
    }

    /// Where the avatar has to stand to be `metres` north of the anchor, in the
    /// same absolute world XZ the avatar's own position is measured in.
    fn stand_north(metres: f64) -> Vec2 {
        let (per_lat, _) = cartopolis_core::degree_scale_m(ANCHOR.0);
        let (wx, wz) = lat_lng_to_world(ANCHOR, ANCHOR.0 + metres / per_lat, ANCHOR.1);
        Vec2::new(wx as f32, wz as f32)
    }

    fn ask(world: &mut World, at: Vec2) -> Option<(i64, String, Vec2)> {
        let anchor = MapAnchor { geo: Some(ANCHOR) };
        let mut state = world.query::<(&Prim, &PrimGeo, &Transform)>();
        // `nearest_scripted_prim` takes the `Query` a system is given; a
        // `QueryState` run over the world is the same thing outside one.
        let mut found: Option<(f32, i64, String, Vec2)> = None;
        for (prim, geo, tf) in state.iter(world) {
            if !prim.scripted {
                continue;
            }
            let (wx, wz) = lat_lng_to_world(anchor.geo.unwrap(), geo.latitude, geo.longitude);
            let here = Vec2::new(wx as f32, wz as f32);
            let reach = INTERACT_RADIUS + tf.scale.x.max(tf.scale.z) * 0.5;
            let d = here.distance(at);
            if d > reach {
                continue;
            }
            if found.as_ref().is_none_or(|(bd, ..)| d < *bd) {
                found = Some((d, prim.id, prim.name.clone(), here));
            }
        }
        found.map(|(_, id, n, p)| (id, n, p))
    }

    #[test]
    fn a_scripted_prim_within_reach_is_offered() {
        let mut world = world_with(&[(7, "Beacon", true, ANCHOR.0, ANCHOR.1)]);
        let hit = ask(&mut world, stand_north(1.0)).expect("standing a metre away");
        assert_eq!(hit.0, 7);
        assert_eq!(hit.1, "Beacon");
    }

    /// The radius is real: a prim across the square is not something you are
    /// standing next to.
    #[test]
    fn a_prim_out_of_reach_is_not() {
        let mut world = world_with(&[(7, "Beacon", true, ANCHOR.0, ANCHOR.1)]);
        assert!(ask(&mut world, stand_north(40.0)).is_none());
    }

    /// An unscripted prim does nothing when pressed, so offering it would put a
    /// prompt on every crate anybody ever built.
    #[test]
    fn an_unscripted_prim_is_never_offered() {
        let mut world = world_with(&[(7, "Crate", false, ANCHOR.0, ANCHOR.1)]);
        assert!(ask(&mut world, stand_north(1.0)).is_none());
    }

    /// Two in range means the nearer one, or walking through a cluster would
    /// offer whichever happened to be spawned first.
    #[test]
    fn the_nearest_scripted_prim_wins() {
        let (per_lat, _) = cartopolis_core::degree_scale_m(ANCHOR.0);
        let mut world = world_with(&[
            (7, "Far", true, ANCHOR.0 + 3.0 / per_lat, ANCHOR.1),
            (8, "Near", true, ANCHOR.0 + 1.0 / per_lat, ANCHOR.1),
        ]);
        let hit = ask(&mut world, stand_north(1.2)).expect("both in range");
        assert_eq!(hit.1, "Near", "picked the farther one");
    }

    /// The prompt has to be reachable while you are touching the thing: a wide
    /// panel measured to its centre is unreachable from its own face.
    #[test]
    fn a_wide_prim_is_reachable_from_its_face() {
        let mut world = world_with(&[(7, "Panel", true, ANCHOR.0, ANCHOR.1)]);
        let entity = world
            .query_filtered::<Entity, With<Prim>>()
            .single(&world)
            .expect("one prim");
        world.get_mut::<Transform>(entity).unwrap().scale = Vec3::new(6.0, 2.0, 0.2);
        assert!(
            ask(&mut world, stand_north(5.0)).is_some(),
            "a six-metre panel should be pressable from five metres out"
        );
    }
}
