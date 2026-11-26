//! **What the editor is doing, while it does it.**
//!
//! A client being driven from somewhere else is otherwise a client that moves on
//! its own: the camera flies, the map re-streams, the shutter fires, and the
//! person sitting in front of it has no idea whether that was the agent, a bug,
//! or their own last keystroke. So every call this client serves is announced
//! here, in a box over the map, as it happens — the tool, what it was asked for,
//! and what it answered.
//!
//! It is a readout and nothing else: no buttons, nothing in it listens, and
//! it stands down while a right-hand panel is open, as every other floating
//! box over the map does. [`publish_mcp_activity`] decides what shows and
//! `lib_mcp_overlay` draws it.

use bevy::prelude::*;

use super::{McpBridge, Outcome};
use crate::systems::theme;

/// How long a finished line stays up after it lands.
///
/// Long enough to read a sentence, short enough that a client left running does
/// not keep a stale log over its map. A line that is still *working* never
/// expires — that one is the answer to "why is it moving".
const LINGER_S: f64 = 12.0;

/// The box's width. Fixed, because its content is a log: lines arrive at
/// unpredictable lengths and a box that resized around each one would twitch its
/// way across the corner. Read by the library's log (`lib_mcp_overlay`), which
/// is not built on Android.
#[cfg_attr(target_os = "android", allow(dead_code))]
pub const WIDTH: f32 = 268.0;

/// The editor's log as the component library draws it
/// (`lib_mcp_overlay`): [`publish_mcp_activity`] decides what shows and
/// publishes it in `view`.
#[derive(Resource, Default)]
#[cfg_attr(not(feature = "ui-lib"), allow(dead_code))]
pub struct McpActivityLib {
    /// What shows this frame; `None` hides the box.
    pub view: Option<ActivityView>,
}

/// The box as plain data.
// No `Debug`: `Outcome` has none.
#[derive(Clone, PartialEq)]
#[cfg_attr(not(feature = "ui-lib"), allow(dead_code))]
pub struct ActivityView {
    /// How far below `OSD_MARGIN` it stands: under the Midas button while
    /// the link is up.
    pub below: f32,
    /// A call is waiting on the schedule: the spinner turns.
    pub busy: bool,
    /// Where the editor's socket listens.
    pub listen: String,
    /// The lines still up: (tool, what it was asked or answered, outcome).
    pub lines: Vec<(String, String, Outcome)>,
}

/// Decide what the editor's box shows this frame and publish it for
/// `lib_mcp_overlay`.
pub fn publish_mcp_activity(
    bridge: Res<McpBridge>,
    right_panel: Res<crate::resources::RightPanel>,
    driver: Res<crate::systems::driver::DriverState>,
    real: Res<Time<Real>>,
    link: Option<Res<crate::systems::midas_link::MidasLink>>,
    mut lib: Option<ResMut<McpActivityLib>>,
) {
    // Driver mode owns the screen, and the top-right corner is where its trip
    // strip goes. There is no ambient "driver mode is on" that hides a box
    // over the map, so anything new that draws there opts out by hand — see
    // `docs/notes/map-chrome.md`, which is a list of the things that did not.
    if !right_panel.is(crate::resources::RightPanel::None) || driver.active {
        publish(&mut lib, None);
        return;
    }
    let now = real.elapsed_secs_f64();
    let live: Vec<_> = bridge
        .log
        .iter()
        .filter(|e| e.outcome == Outcome::Working || now - e.at < LINGER_S)
        .collect();
    if live.is_empty() {
        publish(&mut lib, None);
        return;
    }
    // The Midas button holds the corner while the link is up; stand under it.
    let below = if link.is_some_and(|l| l.config.on && l.linked()) {
        theme::ROUND_BUTTON_SIZE + theme::OSD_GAP
    } else {
        0.0
    };
    let view = ActivityView {
        below,
        busy: bridge.busy(),
        listen: bridge.listen.display().to_string(),
        lines: live
            .iter()
            .map(|e| (e.label.clone(), e.detail.clone(), e.outcome))
            .collect(),
    };
    publish(&mut lib, Some(view));
}

/// Hand the library what shows, writing the resource only when it moved.
fn publish(lib: &mut Option<ResMut<McpActivityLib>>, view: Option<ActivityView>) {
    if let Some(l) = lib.as_mut() {
        if l.view != view {
            l.view = view;
        }
    }
}

/// How long after its last call the agent still counts as at work in the room.
const AI_ACTIVE_SECS: f64 = 15.0;

/// While an agent works in the room (an MCP client, or a session on the Midas
/// page through the device link), what the room's bar at the top says it is
/// doing: `(head, rest)`, `None` for no bar. The bar (`lib_design_room`) has
/// Pause; paused, the agent's building tools answer that the person paused it
/// (`mcp::agent`), so it waits and the person steers — in the room, or in the
/// session's chat. Fed by the same activity log the desktop's editor overlay
/// draws (`McpBridge::log`), so every tool shows here with no extra wiring.
#[cfg_attr(not(feature = "ui-lib"), allow(dead_code))]
#[must_use]
pub fn design_room_ai_line(
    bridge: Option<&McpBridge>,
    now: f64,
    paused: bool,
) -> Option<(String, String)> {
    if paused {
        return Some(("Paused".to_string(), "the AI waits for you".to_string()));
    }
    let latest = bridge?
        .log
        .iter()
        .filter(|e| e.outcome == Outcome::Working || now - e.at <= AI_ACTIVE_SECS)
        .max_by(|a, b| a.at.total_cmp(&b.at))?;
    let (head, rest) = if latest.outcome == Outcome::Working {
        (
            "The AI is building".to_string(),
            format!("{} {}", latest.label, latest.detail),
        )
    } else {
        (
            "The AI".to_string(),
            format!("{}: {}", latest.label, latest.detail),
        )
    };
    Some((head, rest.chars().take(56).collect()))
}
