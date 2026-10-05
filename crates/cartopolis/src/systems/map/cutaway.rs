//! Looking into a building: which one, and down to which storey
//! (`docs/notes/building-designs.md`).
//!
//! A building with an inside is only worth having if you can see the inside, and
//! a follow camera above an avatar in a 6 m room is looking at a ceiling — or,
//! once that is culled, at the floor of the storey above. So inside a building the
//! game does what a dollhouse does: the roof comes off and everything above the
//! storey you are on goes, and the camera looks down into the room. It is The
//! Sims' cutaway, and the reason it is the right answer here is the same one: the
//! camera is above and behind, and the thing to look at is below and in front.
//!
//! There are two ways in, and one state for both:
//!
//! * **walking in** — the building the avatar stands in is cut at the storey the
//!   avatar is on, and the cut follows them up the stairs;
//! * **the building card's Look inside** — any building with an inside, looked
//!   into from wherever the camera is, at the ground floor.
//!
//! Either way the floor picker (`gui::floor_picker`) lists the storeys by the
//! names the building's design gives them, so a storey picked there is the same
//! storey its owner drew in the editor. A storey picked by hand holds until the
//! building changes or the picker is told to follow the avatar again.

use std::sync::Arc;

use bevy::prelude::*;

use crate::resources::AvatarState;
use crate::systems::building_select::BuildingSelection;
use crate::systems::interiors::InteriorFloors;

/// What is being looked into, resolved once a frame by [`resolve_cutaway`].
#[derive(Resource, Default)]
pub struct Cutaway {
    /// The building the card asked to look inside, if any. Ends when the card's
    /// selection moves to another building or is cleared, or the picker closes.
    pub looking: Option<Arc<str>>,
    /// A storey picked in the floor picker, holding until the building changes.
    /// `None` follows the avatar (indoors) or shows the ground floor.
    pub picked: Option<usize>,
    /// The building cut away this frame: the one being looked into, else the one
    /// the avatar stands in.
    pub building: Option<Arc<str>>,
    /// The storey shown, and every storey's name.
    pub storey: usize,
    pub storeys: Vec<String>,
    /// The storey the avatar is on, when it is in [`Self::building`].
    pub avatar_storey: Option<usize>,
    /// The room the avatar is in, by the name its owner gave it.
    pub room: Option<String>,
    /// Whether the interior is a design somebody saved, rather than generated.
    pub designed: bool,
}

impl Cutaway {
    /// The cut `interiors` builds the mesh with: the building and the storey above
    /// which nothing of it is drawn.
    #[must_use]
    pub fn cut(&self) -> Option<(Arc<str>, usize)> {
        self.building.clone().map(|b| (b, self.storey))
    }

    /// Is anything cut away? For `--dump-state` and the shell.
    #[must_use]
    pub fn active(&self) -> bool {
        self.building.is_some()
    }

    /// Start looking inside a building from outside it.
    pub fn look_inside(&mut self, id: Arc<str>) {
        self.looking = Some(id);
        self.picked = None;
    }

    /// Stop looking from outside. Walking in still cuts the building you are in.
    pub fn stop_looking(&mut self) {
        self.looking = None;
        self.picked = None;
    }
}

/// Decide what is cut away this frame. After `track_indoors`, before the shells.
pub fn resolve_cutaway(
    selection: Res<BuildingSelection>,
    floors: Res<InteriorFloors>,
    avatar: Res<AvatarState>,
    mut cutaway: ResMut<Cutaway>,
) {
    // The card owns a look from outside: close the card, or pick another
    // building, and the look ends with it.
    if cutaway
        .looking
        .as_ref()
        .is_some_and(|l| selection.selected.as_ref() != Some(l))
    {
        cutaway.stop_looking();
    }
    let building = cutaway
        .looking
        .clone()
        .or_else(|| selection.indoors.clone());
    if building != cutaway.building {
        cutaway.picked = None;
    }
    let Some(id) = building.clone() else {
        if cutaway.building.is_some() || !cutaway.storeys.is_empty() {
            *cutaway = Cutaway::default();
        }
        return;
    };
    let storeys = floors.storey_names(&id).unwrap_or_default();
    let here = Vec2::new(avatar.position.x, avatar.position.z);
    let inside = selection.indoors.as_ref() == Some(&id);
    let avatar_storey = inside.then(|| floors.storey_at(&id, avatar.floor_lift));
    let last = storeys.len().saturating_sub(1);
    let storey = cutaway.picked.or(avatar_storey).unwrap_or(0).min(last);
    let room = avatar_storey.and_then(|k| floors.room_at(&id, here, k).map(str::to_string));
    let designed = floors.is_designed(&id);

    // Written field by field, and only when it moved: `interiors` rebuilds on a
    // change to the cut, and a resource touched every frame says it changed
    // every frame.
    let c = cutaway.bypass_change_detection();
    let changed = c.building != building
        || c.storey != storey
        || c.storeys != storeys
        || c.avatar_storey != avatar_storey
        || c.room != room
        || c.designed != designed;
    if changed {
        c.building = building;
        c.storey = storey;
        c.storeys = storeys;
        c.avatar_storey = avatar_storey;
        c.room = room;
        c.designed = designed;
        cutaway.set_changed();
    }
}
