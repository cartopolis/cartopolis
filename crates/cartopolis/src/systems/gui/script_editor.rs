//! The in-world script editor: the panel that writes a prim's JavaScript, and
//! the console that is the only channel a broken script has.
//!
//! Source is fetched and stored on demand (`GetPrimScript` / `SetPrimScript`),
//! never carried in a `WorldSnapshot` — it is authoring data an order of magnitude
//! larger than the rest of a prim, and a snapshot goes to every observer twenty
//! times a second for the benefit of the one person with the editor open.
//!
//! The component library draws the panel (`systems::lib_script`); this is its
//! state and what it sends. **Saving is explicit** — there is no autosave, because storing a
//! script *restarts* it in-world, and a restart on every keystroke would be a
//! prim that never finishes running `on_start`. And the buffer is only replaced by
//! the server's copy when it matches what we last saved, so a `PrimScript`
//! broadcast (another editor saving the same prim) cannot silently eat unsaved
//! work.

use bevy::prelude::*;
use cartopolis_core::msg;
use cartopolis_core::{NetMessage, ScriptLevel};

use crate::resources::OnlineSession;

/// Console scrollback, per prim. A script capped at 64 commands a tick fills this
/// in a few seconds of shouting, and only the tail is of any use.
const MAX_CONSOLE_LINES: usize = 200;

/// The starting point offered for a prim with no script. It touches every part of
/// the model — all three handlers, reading and writing the prim, chat, and the
/// clock — so the first thing an author reads is the shape of a script rather than
/// a blank page.
///
/// The hover is deliberately **vertical**. A script's `set_pos` writes the prim's
/// sim position, but horizontal placement is re-derived from the geo anchor
/// (`prim_geo::place_geo_prims`), so an orbiting demo would quietly snap back the
/// next time the map re-anchors. Height has no such owner and stays put.
const TEMPLATE: &str = "\
// Handlers are plain functions. The host calls them; you define them.
function on_start() {
    log(\"hello from \" + get_name())
    set_timer_event(0.1)
}

function on_touch(id, name) {
    say(name + \" touched me\")
    set_color(Math.random(), Math.random(), Math.random())
}

function on_timer() {
    // Bob gently, using the script's own clock.
    const [x, y, z] = get_pos()
    set_pos(x, y + Math.sin(get_time() * 2) * 0.01, z)
}
";

/// One line of script output, as received.
pub struct ConsoleLine {
    pub prim_id: i64,
    pub level: ScriptLevel,
    pub text: String,
}

/// The world's budget, as last reported by the server.
#[derive(Clone, Copy, Default)]
pub struct WorldBudget {
    pub prims_used: u32,
    pub prims_max: u32,
    pub scripts_used: u32,
    pub scripts_max: u32,
    pub script_memory_bytes: u32,
}

#[derive(Resource, Default)]
pub struct ScriptEditorState {
    /// The prim being edited. `None` = the panel has nothing to show.
    pub prim_id: Option<i64>,
    /// Editable buffer.
    pub source: String,
    /// The source as the server last confirmed it. Doubles as the unsaved-changes
    /// test and as the guard against a broadcast overwriting local edits.
    pub saved: String,
    /// Whether the server has answered our `GetPrimScript` for [`Self::prim_id`].
    /// Until it does the buffer is not the prim's script and must not be shown as
    /// one — an empty editor invites you to type into it and save over a script
    /// that is merely still in flight.
    pub fetched: bool,
    /// Set when the panel opens on a new prim; cleared once the request is sent.
    pub needs_fetch: bool,
    /// Whether the server holds a live instance for this prim.
    pub running: bool,
    pub console: Vec<ConsoleLine>,
    /// Lines ever pushed, so a reader can tell which of [`Self::console`] are
    /// new to it after the oldest have been dropped (the design room's log).
    pub pushed: u64,
    /// The world's budget, from `WorldCapacity`. `None` until the server has
    /// said — offline it never does, and the headroom line is simply absent.
    pub budget: Option<WorldBudget>,
}

impl ScriptEditorState {
    /// Point the editor at a prim, discarding whatever it held. Called from the
    /// context menu; the fetch itself is the panel's job, which is what keeps the
    /// menu free of a network dependency.
    pub fn open(&mut self, prim_id: i64) {
        if self.prim_id != Some(prim_id) {
            self.source.clear();
            self.saved.clear();
            self.running = false;
            self.fetched = false;
        }
        self.prim_id = Some(prim_id);
        self.needs_fetch = true;
    }

    /// Apply a `PrimScript` from the server.
    ///
    /// The buffer is only replaced when it holds no unsaved work: a broadcast
    /// arrives whenever *anyone* saves this prim, and typing is not something to
    /// lose to someone else's save. The console says so rather than the change
    /// passing unremarked.
    pub fn receive_source(&mut self, prim_id: i64, source: String, running: bool) {
        if self.prim_id != Some(prim_id) {
            return;
        }
        self.running = running;
        let dirty = self.fetched && self.source != self.saved;
        self.saved = source.clone();
        self.fetched = true;
        if dirty {
            if self.source != source {
                self.push(
                    prim_id,
                    ScriptLevel::Notice,
                    "this script changed on the server; your unsaved edits are kept".into(),
                );
            }
            return;
        }
        self.source = source;
    }

    pub fn push(&mut self, prim_id: i64, level: ScriptLevel, text: String) {
        self.pushed += 1;
        self.console.push(ConsoleLine {
            prim_id,
            level,
            text,
        });
        if self.console.len() > MAX_CONSOLE_LINES {
            let excess = self.console.len() - MAX_CONSOLE_LINES;
            self.console.drain(0..excess);
        }
    }

    /// Unsaved changes, for the Save button's emphasis and the header's dot.
    #[must_use]
    pub fn is_dirty(&self) -> bool {
        self.fetched && self.source != self.saved
    }

    // What the panel does; the component library draws it (`lib_script`).

    /// Ask for the prim's source, once per opening. From the panel rather
    /// than the context menu, so there is one place that knows how to fill
    /// the buffer.
    pub fn fetch_if_needed(&mut self, online: &OnlineSession) {
        if let (true, Some(prim_id)) = (self.needs_fetch, self.prim_id) {
            online.send(
                "get prim script",
                NetMessage::GetPrimScript(msg::GetPrimScript {
                    request_id: 0,
                    prim_id,
                }),
            );
            self.needs_fetch = false;
        }
    }

    /// "Save & run": store the script, which (re)starts it.
    pub fn save(&self, online: &OnlineSession) {
        let Some(prim_id) = self.prim_id else {
            return;
        };
        online.send(
            "set prim script",
            NetMessage::SetPrimScript(msg::SetPrimScript {
                request_id: 0,
                prim_id,
                source: self.source.clone(),
            }),
        );
    }

    /// "Stop": remove the script. Storing an empty script *is* removing it —
    /// one code path on the server, and one less thing that can disagree
    /// about whether a prim has a script.
    pub fn stop(&mut self, online: &OnlineSession) {
        let Some(prim_id) = self.prim_id else {
            return;
        };
        self.source.clear();
        online.send(
            "set prim script",
            NetMessage::SetPrimScript(msg::SetPrimScript {
                request_id: 0,
                prim_id,
                source: String::new(),
            }),
        );
    }

    /// "Touch": fire `on_touch`, as if you had clicked the prim.
    pub fn touch(&self, online: &OnlineSession) {
        let Some(prim_id) = self.prim_id else {
            return;
        };
        online.send(
            "touch prim",
            NetMessage::TouchPrim(msg::TouchPrim {
                request_id: 0,
                prim_id,
            }),
        );
    }

    /// "Start from a template".
    pub fn insert_template(&mut self) {
        self.source = TEMPLATE.to_string();
    }
}

/// The width of the panel, in points.
pub const PANEL_WIDTH: f32 = 420.0;

/// The region's headroom, as the line under the prim says it.
pub(crate) fn budget_line(b: WorldBudget) -> String {
    format!(
        "region: {} / {} scripts · {} / {} prims · {} each",
        b.scripts_used,
        b.scripts_max,
        b.prims_used,
        b.prims_max,
        memory_label(b.script_memory_bytes)
    )
}

/// What the headroom line means, on hover.
pub(crate) const BUDGET_HINT: &str =
    "What this region has left. Scripts and prims are budgeted per region, \
     and each script may take at most this much more memory at a time: one \
     that takes more is stopped.";

/// A script's memory ceiling, in the unit it is set in: "16 MiB", not "16384 KiB".
fn memory_label(bytes: u32) -> String {
    if bytes >= 1 << 20 && bytes % (1 << 20) == 0 {
        format!("{} MiB", bytes >> 20)
    } else {
        format!("{} KiB", bytes / 1024)
    }
}
