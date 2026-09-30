//! **The order of a client frame, as a type.** Every `Update` system the client
//! registers is in exactly one [`ClientPhase`], and the phases run in the order
//! they are declared — `configure_sets(Update, (...).chain())` in
//! [`ClientPhasePlugin`], the one place that order is stated.
//!
//! | Phase | What runs there | Pinned by |
//! |-------|-----------------|-----------|
//! | [`Input`](ClientPhase::Input) | What the server, the session, the person and any script asked for this frame: the network drain, the session driver, the picking/gizmo/prim-edit input, the harness and MCP camera operators, the restores from `user.json`, and the half of the UI that can *raise* a teleport — the dock, the cards, the main menu, the sign-in screen and the head of the panel chain | the main menu → sign-in → `drive_session` → `keep_avatar_across_reconnect` → `apply_teleport` edge |
//! | [`Anchor`](ClientPhase::Anchor) | Where this frame is: `geo_nav::apply_teleport`, the map re-anchor, the basemap source / cache retears, and the harness steps that must follow a teleport | `apply_teleport`, `map_stream::apply_reanchor` |
//! | [`Stream`](ClientPhase::Stream) | Everything that streams, measures or bakes world content around that anchor: tiles, terrain, buildings, detail layers, transit, weather, the server's prims | `map_stream::update_map_stream` |
//! | [`Move`](ClientPhase::Move) | The avatar: movement, collision, the intent sent to the sim, the drive tick and the device tracking that decide who owns it | `avatar::handle_avatar_movement` |
//! | [`Camera`](ClientPhase::Camera) | The camera and the few things that write what it consumes (the driver's chase rig, the on-screen controls) | `free_camera::camera_controls` |
//! | [`Present`](ClientPhase::Present) | Everything drawn against the placed camera: sky and globe tracking, labels, the tail of the panel chain, overlays, audio, the capture shutter and the end-of-frame bookkeeping | — |
//!
//! **The phases are derived from the edges, not the other way round.** Before
//! they existed the order was ~230 explicit `.after`/`.before` links plus
//! fifteen `// Phase N` comments that the scheduler never read (and whose
//! numbers did not describe the order either — "Phase 4" picking ran after
//! "Phase 6" gizmos). The phase boundaries sit on the three systems every other
//! edge hangs off: the teleport, the avatar's movement and the camera. Each
//! system was placed in a phase its explicit edges allow, and every explicit edge
//! is kept beside it — within a phase they are still the whole ordering, and
//! across phases they are redundant but kept, because they say *why*.
//!
//! Two consequences worth knowing:
//!
//! - **The panel chain spans two phases.** Its head (main menu … route sheet)
//!   has to run before the teleport and the camera — the menu opens the sign-in
//!   screen whose session driver brings back the avatar the teleport moves, and
//!   the route sheet's Start button feeds the drive tick that steers the camera —
//!   while its tail (chat bubbles, place pins, the locate arrow) projects against
//!   the placed camera. So `gui::plugin` declares it as two chained halves,
//!   chained again to each other: still one total order, in two sets.
//! - **The phases add ordering that the edges did not have.** A streamer used to
//!   be free to overlap the camera systems; now the whole `Stream` phase finishes
//!   before `Move` starts. That is the intent — a collider, a terrain field or a
//!   building that streamed in this frame is there for this frame's movement —
//!   and it is cheap, because the streamers' heavy work is on the worker pool,
//!   not in the systems.
//!
//! Only `Update` is phased. `First`, `PreUpdate`, `PostUpdate`, `Last`, `Startup`
//! and the render sub-app keep the explicit edges they always had.

use bevy::prelude::*;

/// See the module note for what runs in each and why the order is this one.
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ClientPhase {
    Input,
    Anchor,
    Stream,
    Move,
    Camera,
    Present,
}

/// Declares the [`ClientPhase`] order. Added before any group plugin, so a
/// system's `in_set` always names a configured set.
pub struct ClientPhasePlugin;

impl Plugin for ClientPhasePlugin {
    fn build(&self, app: &mut App) {
        app.configure_sets(
            Update,
            (
                ClientPhase::Input,
                ClientPhase::Anchor,
                ClientPhase::Stream,
                ClientPhase::Move,
                ClientPhase::Camera,
                ClientPhase::Present,
            )
                .chain(),
        );
    }
}
