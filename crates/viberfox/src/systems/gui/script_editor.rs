//! The in-world script editor: the panel that writes a prim's Luau source, and
//! the console that is the only channel a broken script has.
//!
//! Source is fetched and stored on demand (`GetPrimScript` / `SetPrimScript`),
//! never carried in a `WorldSnapshot` — it is authoring data an order of magnitude
//! larger than the rest of a prim, and a snapshot goes to every observer twenty
//! times a second for the benefit of the one person with the editor open.
//!
//! Three things about the layout are deliberate. The **console is docked to the
//! bottom and laid out first**, so the code area takes what is left: a compile
//! error that arrives while you are typing must not push the line you are typing
//! off the panel. **Saving is explicit** — there is no autosave, because storing a
//! script *restarts* it in-world, and a restart on every keystroke would be a
//! prim that never finishes running `on_start`. And the buffer is only replaced by
//! the server's copy when it matches what we last saved, so a `PrimScript`
//! broadcast (another editor saving the same prim) cannot silently eat unsaved
//! work.

use bevy::prelude::*;
use std::collections::HashMap;
use viberfox_core::{NetMessage, ScriptLevel};

use crate::resources::{OnlineSession, RightPanel};
use crate::systems::egui_manager::EguiManager;
use crate::systems::theme;

/// Console scrollback, per prim. A script capped at 64 commands a tick fills this
/// in a few seconds of shouting, and only the tail is of any use.
const MAX_CONSOLE_LINES: usize = 200;

/// The starting point offered for a prim with no script. It touches every part of
/// the model — an event handler, the `vf` API, the prim's own state — so the first
/// thing an author reads is the shape of a script rather than a blank page.
const TEMPLATE: &str = "\
-- Handlers are plain functions: on_start, on_touch, on_timer.
function on_start()
    vf.log(\"hello from \" .. \"this prim\")
    vf.timer(1.0)
end

function on_touch(id, name)
    vf.say(name .. \" touched me\")
    vf.set_color(math.random(), math.random(), math.random())
end

function on_timer()
    local x, y, z = vf.get_pos()
    vf.set_pos(x, y + math.sin(os.clock()) * 0.02, z)
end
";

/// One line of script output, as received.
pub struct ConsoleLine {
    pub prim_id: i64,
    pub level: ScriptLevel,
    pub text: String,
}

/// A region's budget, as last reported by the server.
#[derive(Clone, Copy, Default)]
pub struct RegionBudget {
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
    /// Budgets by region id, from `RegionCapacity`.
    pub budgets: HashMap<i64, RegionBudget>,
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
}

/// Colour for a console line. Errors are the reason the console exists, so they
/// are the one thing that must not read as ordinary output.
fn level_color(level: ScriptLevel) -> egui::Color32 {
    match level {
        ScriptLevel::Info => theme::TEXT,
        ScriptLevel::Notice => theme::TEXT_DIM,
        ScriptLevel::Error => theme::ERROR,
    }
}

/// The script editor panel (right-hand slot, like the other panels).
///
/// Only shown online: a script is server state, and `--offline` has no server to
/// hold one. The context-menu entry that opens it is hidden on the same test.
pub fn render_script_panel(
    mut egui: ResMut<EguiManager>,
    mut editor: ResMut<ScriptEditorState>,
    online: Option<Res<OnlineSession>>,
    mut right_panel: ResMut<RightPanel>,
    prims: Query<&crate::components::Prim>,
) {
    let Some(online) = online else {
        return;
    };
    if !right_panel.is(RightPanel::Script) {
        return;
    }

    // The source request is made from here rather than from the context menu, so
    // there is one place that knows how to fill the buffer.
    if let (true, Some(prim_id)) = (editor.needs_fetch, editor.prim_id) {
        let _ = online.intent_tx.unbounded_send(NetMessage::GetPrimScript {
            request_id: 0,
            prim_id,
        });
        editor.needs_fetch = false;
    }

    let prim_id = editor.prim_id;
    let prim_name = prim_id
        .and_then(|id| prims.iter().find(|p| p.id == id).map(|p| p.name.clone()))
        .unwrap_or_else(|| "—".to_string());
    let budget = prim_id
        .and_then(|id| prims.iter().find(|p| p.id == id))
        .and_then(|p| editor.budgets.get(&p.region_id).copied());

    let mut close = false;
    let mut save = false;
    let mut stop = false;
    let mut touch = false;
    let mut clear_console = false;
    let mut insert_template = false;

    let compact = egui.compact();
    let viewport_w = egui.viewport_pts().x;
    let mut open = true;
    crate::systems::egui_manager::responsive_right_panel(
        "script_panel",
        compact,
        viewport_w,
        420.0,
    )
    .show_collapsible(egui.ui_mut(), &mut open, |ui| {
        let panel = RightPanel::Script;
        close = theme::panel_header(ui, panel.icons(), panel.label());

        let Some(prim_id) = prim_id else {
            ui.label(
                egui::RichText::new("Right-click a prim and choose Edit script.")
                    .color(theme::TEXT_DIM),
            );
            return;
        };

        ui.horizontal_wrapped(|ui| {
            ui.label(
                egui::RichText::new(&prim_name)
                    .strong()
                    .color(theme::FOX_TEXT),
            );
            ui.label(egui::RichText::new(format!("#{prim_id}")).color(theme::TEXT_DIM));
            // "Running" is the server's word for "I hold a compiled instance",
            // which is not the same as "the source in this box" — hence the
            // separate unsaved marker rather than one combined state.
            let (dot, tip) = if editor.running {
                (
                    "● running",
                    "the server is running a compiled copy of this script",
                )
            } else {
                ("○ stopped", "this prim has no script running")
            };
            ui.label(egui::RichText::new(dot).color(if editor.running {
                theme::FOX
            } else {
                theme::TEXT_DIM
            }))
            .on_hover_text(tip);
            if editor.is_dirty() {
                ui.label(egui::RichText::new("• unsaved").color(theme::AMBER));
            }
        });

        if let Some(b) = budget {
            ui.label(
                egui::RichText::new(format!(
                    "region: {} / {} scripts · {} / {} prims · {} KiB each",
                    b.scripts_used,
                    b.scripts_max,
                    b.prims_used,
                    b.prims_max,
                    b.script_memory_bytes / 1024
                ))
                .color(theme::TEXT_DIM)
                .small(),
            )
            .on_hover_text(
                "What this region has left. Scripts and prims are budgeted per region, \
                 and every script gets its own memory ceiling.",
            );
        }
        ui.separator();

        // Laid out bottom-up: the console takes a fixed share and the code area
        // gets the rest, so output arriving while you type never displaces the
        // line you are typing.
        egui::Panel::bottom("script_console")
            .show_separator_line(false)
            .frame(egui::Frame::NONE.outer_margin(egui::Margin {
                top: 6,
                ..Default::default()
            }))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    // Emphasised only while there is something to save: a Save
                    // button that always shouts stops meaning anything.
                    let save_label = if editor.is_dirty() {
                        egui::RichText::new("Save & run").strong()
                    } else {
                        egui::RichText::new("Save & run")
                    };
                    save = ui
                        .button(save_label)
                        .on_hover_text("Store the script and (re)start it — saving is a restart")
                        .clicked();
                    stop = ui
                        .button("Stop")
                        .on_hover_text("Remove the script from this prim")
                        .clicked();
                    touch = ui
                        .button("Touch")
                        .on_hover_text("Fire on_touch, as if you had clicked the prim")
                        .clicked();
                });
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("Console")
                            .color(theme::TEXT_DIM)
                            .small(),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        clear_console = ui.small_button("Clear").clicked();
                    });
                });
                egui::ScrollArea::vertical()
                    .stick_to_bottom(true)
                    .max_height(140.0)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        for line in editor.console.iter().filter(|l| l.prim_id == prim_id) {
                            ui.label(
                                egui::RichText::new(&line.text)
                                    .monospace()
                                    .small()
                                    .color(level_color(line.level)),
                            );
                        }
                    });
            });

        if !editor.fetched {
            ui.label(egui::RichText::new("Loading…").color(theme::TEXT_DIM));
            return;
        }

        if editor.source.is_empty() {
            insert_template = ui
                .button("Start from a template")
                .on_hover_text("Fill the editor with a script that shows every handler")
                .clicked();
        }
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.add(
                    egui::TextEdit::multiline(&mut editor.source)
                        .code_editor()
                        .desired_width(f32::INFINITY)
                        .desired_rows(18)
                        .hint_text("function on_touch(id, name)\n    vf.say(\"hi \" .. name)\nend"),
                );
            });
    });

    if close || !open {
        *right_panel = RightPanel::None;
    }
    if clear_console {
        editor.console.clear();
    }
    if insert_template {
        editor.source = TEMPLATE.to_string();
    }

    let Some(prim_id) = editor.prim_id else {
        return;
    };
    if save {
        let source = editor.source.clone();
        let _ = online.intent_tx.unbounded_send(NetMessage::SetPrimScript {
            request_id: 0,
            prim_id,
            source,
        });
    }
    if stop {
        // Storing an empty script *is* removing it — one code path on the server,
        // and one less thing that can disagree about whether a prim has a script.
        editor.source.clear();
        let _ = online.intent_tx.unbounded_send(NetMessage::SetPrimScript {
            request_id: 0,
            prim_id,
            source: String::new(),
        });
    }
    if touch {
        let _ = online.intent_tx.unbounded_send(NetMessage::TouchPrim {
            request_id: 0,
            prim_id,
        });
    }
}
