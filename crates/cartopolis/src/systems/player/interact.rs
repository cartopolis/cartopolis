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
//! **The prompt is world-anchored but drawn in screen space**, so it obeys the
//! same rule the place pins do: it must cull itself against the map area, because
//! egui paints floating `Area`s above panels (see the CLAUDE.md entry). It is
//! ordered with the panel chain for that reason.

use bevy::prelude::*;

use crate::resources::{AvatarState, CameraMode, CameraState, MapAnchor, RightPanel, Seated};
use crate::systems::egui_manager::EguiManager;
use crate::systems::geo_nav::{world_to_lat_lng, GeoNavState};
use crate::systems::theme;
use crate::systems::world_places::{Interactable, SeatSpot, WorldPlaces};
use cartopolis_core::Emote;

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
}

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

impl InteractState {
    /// Act on whatever the prompt is offering. One definition, because there are
    /// two ways to ask for it — the E key and a tap on the prompt itself — and a
    /// second copy of "play the clip, remember the line, start its timer" is how
    /// they would come to behave differently.
    fn act(&mut self, avatar_state: &mut AvatarState) {
        // Sitting is the one interaction that is a *state* rather than a gesture,
        // so the same control leaves it. Asked for while seated, this is "stand
        // up" whatever the prompt is standing next to.
        if avatar_state.is_seated() {
            avatar_state.stand_up();
            self.last = None;
            self.last_ttl = 0.0;
            return;
        }
        let Some(target) = self.target.clone() else {
            return;
        };
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
            });
            // No receipt: the body is the feedback, and it stays there. A line
            // reading "Sit at Bench" fading out under a seated avatar would be
            // reporting an event that is still going on.
            self.last = None;
            self.last_ttl = 0.0;
            return;
        }
        // The same one-shot channel the emote keys use, so acting on the world and
        // emoting are the same motion as far as the animation and the wire are
        // concerned — which is exactly right, since `Interact` *is* the clip for
        // "use the thing in front of me".
        avatar_state.pending_emote = Some(Emote::Interact);
        self.last = Some(target.line());
        self.last_ttl = LAST_LINE_SECS;
    }
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
    mut state: ResMut<InteractState>,
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

    // Compare before building: this resource is read by the HUD, and rewriting it
    // every frame would mark it changed every frame. The comparison is against the
    // same three fields `line()` formats, so the update rule is unchanged — but
    // doing it this way allocates nothing in the steady state. Constructing the
    // `TargetInfo` first cost a `String` clone *plus* the two `format!`s `line()`
    // does on each side, every frame, and in a city the radius nearly always hits.
    let nearest = places.nearest(pos, INTERACT_RADIUS);
    let changed = match (&state.target, nearest) {
        (Some(a), Some(b)) => {
            a.verb != b.kind.verb() || a.detail != b.kind.detail() || a.name != b.name
        }
        (None, None) => false,
        _ => true,
    };
    if changed {
        state.target = nearest.map(|i| TargetInfo::of(i, pos));
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

    // Likewise: `nearest_address` hands back a `&str`, so compare through it and
    // only own the string on the frames the answer actually changes.
    let address = places.nearest_address(pos, ADDRESS_RADIUS);
    if state.address.as_deref() != address {
        state.address = address.map(str::to_owned);
    }
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
    egui_manager: Res<EguiManager>,
    edit_dialog: Res<crate::resources::EditDialogState>,
    mut avatar_state: ResMut<AvatarState>,
    mut state: ResMut<InteractState>,
) {
    if camera_state.mode != CameraMode::Avatar
        || egui_manager.ui_owns_keyboard(edit_dialog.visible, &keyboard)
    {
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
        state.act(&mut avatar_state);
    }
}

/// Draw the prompt over the map, and let it be *pressed*.
///
/// Anchored to the bottom-centre of the *map area* rather than the window, and
/// culled to it, for the reason `place_pins` documents: the shared root `Ui`'s
/// leftover rect is exactly the map, and drawing outside it puts an interactive
/// overlay on top of a panel. This runs inside the panel chain so that rect is
/// the real one.
///
/// It used to read `Sit at Bench   [E]` and be `interactable(false)` — a keyboard
/// hint on the one platform with no keyboard, advertising an action the prompt
/// itself could not take. The line is a button now, and the key hint only appears
/// on a viewport wide enough to imply a keyboard. The second button is the one
/// this module could not offer at all: a POI you walked up to is a *destination*,
/// and there was no way to make it one.
///
/// Only the buttons sense the pointer — the frame around them does not — so the
/// prompt still cannot eat a click meant for the map it floats over, which is the
/// property that made it non-interactive in the first place.
pub fn render_interaction_prompt(
    mut egui_manager: ResMut<EguiManager>,
    camera_state: Res<CameraState>,
    mut state: ResMut<InteractState>,
    mut avatar_state: ResMut<AvatarState>,
    mut nav: ResMut<GeoNavState>,
    map_anchor: Res<MapAnchor>,
    driver: Res<crate::systems::driver::DriverState>,
) {
    // Driver mode sets `CameraMode::Avatar` itself, so the mode test below is
    // true throughout a trip and this prompt kept appearing — "Sit at Bench" at
    // bottom-centre, in the speed pill's band, while driving past one.
    if driver.active {
        return;
    }
    if camera_state.mode != CameraMode::Avatar {
        return;
    }
    let target = state.target.clone();
    let receipt = state.last.clone();
    // A seated avatar always has a prompt, whatever it is standing next to —
    // getting up is the one thing it can do, and on a phone this button is the
    // only way to ask for it.
    let seated = avatar_state.is_seated();
    if target.is_none() && receipt.is_none() && !seated {
        return;
    }

    let compact = egui_manager.compact();
    let map = egui_manager.ui_mut().available_rect_before_wrap();
    if map.width() < 1.0 || map.height() < 1.0 {
        return;
    }
    let pos = egui::pos2(map.center().x, map.bottom() - PROMPT_BOTTOM_MARGIN);
    let mut acted = false;
    let mut routed = false;
    egui::Area::new(egui::Id::new("interact_prompt"))
        .order(egui::Order::Background)
        // A live prompt is a control; the receipt left behind after acting is not.
        .interactable(target.is_some() || seated)
        .fixed_pos(pos)
        .pivot(egui::Align2::CENTER_BOTTOM)
        .show(&egui_manager.ctx.clone(), |ui| {
            theme::overlay_frame().show(ui, |ui| {
                ui.horizontal(|ui| {
                    if seated {
                        // The glyph is the thing being sat on where there is one,
                        // so the button that put you there and the button that
                        // takes you off it are visibly the same control.
                        let glyph = target.as_ref().map_or("", |t| t.glyph);
                        let hint = if compact { "" } else { "   [E]" };
                        if ui
                            .button(
                                egui::RichText::new(format!("{glyph}  Stand up{hint}"))
                                    .color(theme::TEXT)
                                    .strong(),
                            )
                            .clicked()
                        {
                            acted = true;
                        }
                        return;
                    }
                    let Some(t) = &target else {
                        // Nothing in range: what is left is the line naming what
                        // was last acted on, which is a receipt and not a button.
                        if let Some(last) = &receipt {
                            ui.label(egui::RichText::new(last).color(theme::TEXT).strong());
                        }
                        return;
                    };
                    // The key hint is a fact about the *input*, not about the
                    // action, so it is only stated where a keyboard is plausible.
                    let hint = if compact { "" } else { "   [E]" };
                    if ui
                        .button(
                            egui::RichText::new(format!("{}  {}{hint}", t.glyph, t.line()))
                                .color(theme::TEXT)
                                .strong(),
                        )
                        .clicked()
                    {
                        acted = true;
                    }
                    // The Route panel's own icon, so the button and the panel it
                    // opens read as one thing.
                    if ui
                        .button(RightPanel::Route.icon())
                        .on_hover_text("Route here — make this the destination")
                        .clicked()
                    {
                        routed = true;
                    }
                });
            });
        });

    if acted {
        state.act(&mut avatar_state);
    }
    if routed {
        // The store holds anchored world XZ; the planner speaks lat/lng. Without
        // an anchor there is no mapping between them yet, and the button simply
        // does nothing rather than routing to null island.
        if let (Some(t), Some(anchor)) = (&target, map_anchor.geo) {
            let (lat, lng) = world_to_lat_lng(anchor, t.pos.x as f64, t.pos.y as f64);
            nav.route_to(t.name.clone(), lat, lng);
        }
    }
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
        state.act(&mut avatar);
        assert!(avatar.pending_emote.is_none());
        assert!(state.last.is_none());

        state.target = Some(target("Litter bin", InteractKind::Prop(FurnitureKind::Bin)));
        state.act(&mut avatar);
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

        state.act(&mut avatar);
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

        state.act(&mut avatar);
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
