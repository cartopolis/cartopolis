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
//! egui paints floating `Area`s above panels (see
//! `docs/notes/egui-shared-root-ui.md`). It is ordered with the panel chain for
//! that reason.

use crate::systems::egui_manager::{EguiMut, EguiRef};
use bevy::prelude::*;

use crate::resources::{AvatarState, CameraMode, CameraState, MapAnchor, RightPanel, Seated};
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
    /// The prim to send a `TouchPrim` for, when the thing prompted for is a
    /// scripted object rather than a place on the map.
    ///
    /// Streamed places and prims are different worlds — one comes out of a vector
    /// tile and belongs to nobody, the other is a server-owned object with an id —
    /// and they meet only here, in the one question the player is actually asking:
    /// what is in front of me, and what happens if I press it.
    pub prim_id: Option<i64>,
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
            name: if name.trim().is_empty() {
                "object".to_string()
            } else {
                name.to_string()
            },
            glyph: crate::systems::icons::SCRIPT,
            verb: "Press",
            detail: None,
            pos,
            seat: None,
            prim_id: Some(id),
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

/// Tell the server a prim was pressed, if one was and if there is a server.
///
/// One definition for the two ways to press (the key and the prompt), on the same
/// rule [`InteractState::act`] itself follows: a second copy is how they come to
/// behave differently. Offline this does nothing at all — a prim with a script is
/// a server-owned object, and `--offline` runs no sim — and that is a silence
/// rather than a failure, because the prompt only ever appears for a prim a
/// server sent.
fn send_touch(online: Option<&Res<crate::resources::OnlineSession>>, prim_id: Option<i64>) {
    let (Some(session), Some(prim_id)) = (online, prim_id) else {
        return;
    };
    session.send(
        "touch prim",
        cartopolis_core::NetMessage::TouchPrim {
            request_id: 0,
            prim_id,
        },
    );
}

impl InteractState {
    /// Act on whatever the prompt is offering. One definition, because there are
    /// two ways to ask for it — the E key and a tap on the prompt itself — and a
    /// second copy of "play the clip, remember the line, start its timer" is how
    /// they would come to behave differently.
    ///
    /// Returns the prim that has to be told about it, when the thing acted on was
    /// a scripted object.
    ///
    /// Returned rather than sent from here, because this type has no session and
    /// should not grow one: it is read by the HUD and written by two different
    /// systems, and the network handle belongs to the caller.
    #[must_use]
    fn act(&mut self, avatar_state: &mut AvatarState) -> Option<i64> {
        // Sitting is the one interaction that is a *state* rather than a gesture,
        // so the same control leaves it. Asked for while seated, this is "stand
        // up" whatever the prompt is standing next to.
        if avatar_state.is_seated() {
            avatar_state.stand_up();
            self.last = None;
            self.last_ttl = 0.0;
            return None;
        }
        let Some(target) = self.target.clone() else {
            return None;
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
            return None;
        }
        // The same one-shot channel the emote keys use, so acting on the world and
        // emoting are the same motion as far as the animation and the wire are
        // concerned — which is exactly right, since `Interact` *is* the clip for
        // "use the thing in front of me".
        avatar_state.pending_emote = Some(Emote::Interact);
        self.last = Some(target.line());
        self.last_ttl = LAST_LINE_SECS;
        target.prim_id
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
    facts: Res<crate::systems::building_facts::BuildingFacts>,
    anchor: Res<MapAnchor>,
    prims: Query<(
        &crate::components::Prim,
        &crate::components::PrimGeo,
        &Transform,
    )>,
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
            .is_some_and(|t| t.prim_id == Some(hit.0) && t.name == hit.1);
        if !same {
            state.target = Some(TargetInfo::of_prim(hit.0, &hit.1, hit.2));
        }
        state.address = nearest_address(&facts, &places, &anchor, &avatar_state, pos);
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
        let (wx, wz) = crate::systems::geo_nav::lat_lng_to_world(geo, at.latitude, at.longitude);
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
    egui_manager: EguiRef,
    edit_dialog: Res<crate::resources::EditDialogState>,
    online: Option<Res<crate::resources::OnlineSession>>,
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
        send_touch(online.as_ref(), state.act(&mut avatar_state));
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
    mut egui_manager: EguiMut,
    camera_state: Res<CameraState>,
    mut state: ResMut<InteractState>,
    mut avatar_state: ResMut<AvatarState>,
    mut nav: ResMut<GeoNavState>,
    map_anchor: Res<MapAnchor>,
    driver: Res<crate::systems::driver::DriverState>,
    online: Option<Res<crate::resources::OnlineSession>>,
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
        send_touch(online.as_ref(), state.act(&mut avatar_state));
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
    use crate::systems::geo_nav::lat_lng_to_world;

    /// Groningen's Grote Markt, at the zoom the map anchors at.
    const ANCHOR: (f64, f64, u32) = (53.2194, 6.5665, 16);

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
