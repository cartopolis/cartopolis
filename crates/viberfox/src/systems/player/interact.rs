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

use crate::resources::{AvatarState, CameraMode, CameraState};
use crate::systems::egui_manager::EguiManager;
use crate::systems::theme;
use crate::systems::world_places::{Interactable, WorldPlaces};
use viberfox_core::Emote;

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
}

impl TargetInfo {
    fn of(i: &Interactable) -> Self {
        Self {
            name: i.name.clone(),
            glyph: i.kind.glyph(),
            verb: i.kind.verb(),
            detail: i.kind.detail(),
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

    let target = places.nearest(pos, INTERACT_RADIUS).map(TargetInfo::of);
    // Compare by line rather than assigning unconditionally: this resource is read
    // by the HUD, and rewriting it every frame would mark it changed every frame.
    let changed = match (&state.target, &target) {
        (Some(a), Some(b)) => a.line() != b.line(),
        (None, None) => false,
        _ => true,
    };
    if changed {
        state.target = target;
    }

    let address = places
        .nearest_address(pos, ADDRESS_RADIUS)
        .map(str::to_owned);
    if state.address != address {
        state.address = address;
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

    for (key, emote) in EMOTE_KEYS {
        if keyboard.just_pressed(key) {
            avatar_state.pending_emote = Some(emote);
        }
    }

    if keyboard.just_pressed(KeyCode::KeyE) {
        if let Some(target) = state.target.clone() {
            // The same one-shot channel the emote keys use, so acting on the world
            // and emoting are the same motion as far as the animation and the wire
            // are concerned — which is exactly right, since `Interact` *is* the
            // clip for "use the thing in front of me".
            avatar_state.pending_emote = Some(Emote::Interact);
            state.last = Some(target.line());
            state.last_ttl = LAST_LINE_SECS;
        }
    }
}

/// Draw the prompt over the map.
///
/// Anchored to the bottom-centre of the *map area* rather than the window, and
/// culled to it, for the reason `place_pins` documents: the shared root `Ui`'s
/// leftover rect is exactly the map, and drawing outside it puts an interactive
/// overlay on top of a panel. This runs inside the panel chain so that rect is
/// the real one.
pub fn render_interaction_prompt(
    mut egui_manager: ResMut<EguiManager>,
    camera_state: Res<CameraState>,
    state: Res<InteractState>,
) {
    if camera_state.mode != CameraMode::Avatar {
        return;
    }
    let Some(text) = state
        .target
        .as_ref()
        .map(|t| format!("{}  {}   [E]", t.glyph, t.line()))
        .or_else(|| state.last.clone())
    else {
        return;
    };

    let map = egui_manager.ui_mut().available_rect_before_wrap();
    if map.width() < 1.0 || map.height() < 1.0 {
        return;
    }
    // A prompt is a hint, not a control: it must never eat a click meant for the
    // map behind it.
    let pos = egui::pos2(map.center().x, map.bottom() - PROMPT_BOTTOM_MARGIN);
    egui::Area::new(egui::Id::new("interact_prompt"))
        .order(egui::Order::Background)
        .fixed_pos(pos)
        .pivot(egui::Align2::CENTER_BOTTOM)
        .interactable(false)
        .show(&egui_manager.ctx.clone(), |ui| {
            theme::overlay_frame().show(ui, |ui| {
                ui.label(egui::RichText::new(text).color(theme::TEXT).strong());
            });
        });
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
        TargetInfo::of(&Interactable {
            pos: Vec2::ZERO,
            name: name.to_string(),
            kind,
        })
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

    /// A named place carries its category, because the name alone rarely says
    /// what the thing is.
    #[test]
    fn a_place_prompt_names_the_category() {
        use crate::systems::places::PoiCategory;
        let cafe = target("Vismarkt", InteractKind::Place(PoiCategory::Drink));
        assert_eq!(cafe.line(), "Look at Vismarkt (café)");
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
