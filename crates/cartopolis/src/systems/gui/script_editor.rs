//! The in-world script editor: the panel that writes a prim's JavaScript, and
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

use crate::systems::egui_manager::EguiMut;
use bevy::prelude::*;
use cartopolis_core::msg;
use cartopolis_core::{NetMessage, ScriptLevel};

use crate::resources::{OnlineSession, RightPanel};
use crate::systems::icons;
use crate::systems::theme;

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
    /// The component library draws the panel (`lib_script`): egui's keeps
    /// an empty strip of its width.
    pub lib: bool,
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

    // What the panel does, for both its editions: egui's
    // (`render_script_panel`) and the component library's (`lib_script`),
    // so the two cannot drift apart.

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

/// The width of the panel, egui's and the library's, in points.
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

/// The collapsed API reference: every call and every handler, grouped.
///
/// Rendered straight from [`cartopolis_core::SCRIPT_API`], which the *server* is
/// tested against — so this list cannot drift into describing functions that do
/// not exist. Collapsed by default because it is a reminder, not a tutorial: the
/// question it answers is "what was that called again", and the answer should not
/// cost the code area half its height on every open.
fn reference_ui(ui: &mut egui::Ui) {
    egui::CollapsingHeader::new("Commands")
        .default_open(false)
        .show(ui, |ui| {
            ui.label(
                egui::RichText::new(
                    "Handlers you define — the host calls these when something happens:",
                )
                .color(theme::p32().text_dim)
                .small(),
            );
            for f in cartopolis_core::SCRIPT_EVENTS {
                entry_ui(ui, f);
            }
            ui.add_space(4.0);
            for group in cartopolis_core::ScriptGroup::all() {
                let mut any = false;
                for f in cartopolis_core::api_in_group(*group) {
                    if !any {
                        ui.label(
                            egui::RichText::new(group.label())
                                .strong()
                                .color(theme::p32().accent_text)
                                .small(),
                        );
                        any = true;
                    }
                    entry_ui(ui, f);
                }
            }
        });
}

/// One reference row: the signature in code type, its summary on hover.
///
/// The summary is a tooltip rather than a second line because the panel is as
/// narrow as 320 pt on a phone, where two lines per call turns a 25-entry list
/// into a scroll of prose.
fn entry_ui(ui: &mut egui::Ui, f: &cartopolis_core::ScriptFn) {
    ui.label(
        egui::RichText::new(f.signature)
            .monospace()
            .small()
            .color(theme::p32().text),
    )
    .on_hover_text(f.summary);
}

/// Colour for a console line. Errors are the reason the console exists, so they
/// are the one thing that must not read as ordinary output.
fn level_color(level: ScriptLevel) -> egui::Color32 {
    match level {
        ScriptLevel::Info => theme::p32().text,
        ScriptLevel::Notice => theme::p32().text_dim,
        ScriptLevel::Error => theme::p32().error,
    }
}

/// A script's memory ceiling, in the unit it is set in: "16 MiB", not "16384 KiB".
fn memory_label(bytes: u32) -> String {
    if bytes >= 1 << 20 && bytes % (1 << 20) == 0 {
        format!("{} MiB", bytes >> 20)
    } else {
        format!("{} KiB", bytes / 1024)
    }
}

/// The script editor panel (right-hand slot, like the other panels).
///
/// Only shown online: a script is server state, and `--offline` has no server to
/// hold one. The context-menu entry that opens it is hidden on the same test.
pub fn render_script_panel(
    mut egui: EguiMut,
    mut editor: ResMut<ScriptEditorState>,
    online: Option<Res<OnlineSession>>,
    mut right_panel: ResMut<RightPanel>,
    prims: Query<&crate::components::Prim>,
) {
    if !right_panel.is(RightPanel::Script) {
        return;
    }
    // The library's edition is showing (`lib_script`): keep an empty strip
    // of the panel's width, so the dock and the pins move aside and a click
    // there is not the world's. It fetches and sends itself.
    if editor.lib {
        let compact = egui.compact();
        let viewport_w = egui.viewport_pts().x;
        let mut open = true;
        let mut strip = crate::systems::egui_manager::responsive_right_panel(
            "script_panel",
            compact,
            viewport_w,
            PANEL_WIDTH,
        )
        .frame(egui::Frame::NONE)
        .show_separator_line(false);
        if !compact {
            strip = strip.resizable(false).exact_size(PANEL_WIDTH);
        }
        strip.show_collapsible(egui.ui_mut(), &mut open, |_ui| {});
        return;
    }
    let Some(online) = online else {
        return;
    };
    // Read before writing, so a quiet frame leaves the resource unchanged.
    if editor.needs_fetch {
        editor.fetch_if_needed(&online);
    }

    let prim_id = editor.prim_id;
    let prim_name = prim_id
        .and_then(|id| prims.iter().find(|p| p.id == id).map(|p| p.name.clone()))
        .unwrap_or_else(|| "—".to_string());
    let budget = editor.budget;

    let mut save = false;
    let mut stop = false;
    let mut touch = false;
    let mut clear_console = false;
    let mut insert_template = false;

    crate::systems::egui_manager::right_panel(
        &mut egui,
        &mut right_panel,
        RightPanel::Script,
        "script_panel",
        PANEL_WIDTH,
        |ui| {
            let Some(prim_id) = prim_id else {
                ui.label(
                    egui::RichText::new("Hold or right-click an object and choose Script.")
                        .color(theme::p32().text_dim),
                );
                return;
            };

            ui.horizontal_wrapped(|ui| {
                ui.label(
                    egui::RichText::new(&prim_name)
                        .strong()
                        .color(theme::p32().accent_text),
                );
                ui.label(egui::RichText::new(format!("#{prim_id}")).color(theme::p32().text_dim));
                // "Running" is the server's word for "I hold a compiled instance",
                // which is not the same as "the source in this box" — hence the
                // separate unsaved marker rather than one combined state.
                let (dot, tip) = if editor.running {
                    (
                        format!("{} running", icons::RUNNING),
                        "the server is running a compiled copy of this script",
                    )
                } else {
                    (
                        format!("{} stopped", icons::STOPPED),
                        "this prim has no script running",
                    )
                };
                ui.label(egui::RichText::new(dot).color(if editor.running {
                    theme::p32().accent
                } else {
                    theme::p32().text_dim
                }))
                .on_hover_text(tip);
                if editor.is_dirty() {
                    ui.label(egui::RichText::new("• unsaved").color(theme::p32().warn));
                }
            });

            if let Some(b) = budget {
                ui.label(
                    egui::RichText::new(budget_line(b))
                        .color(theme::p32().text_dim)
                        .small(),
                )
                .on_hover_text(BUDGET_HINT);
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
                            .on_hover_text(
                                "Store the script and (re)start it — saving is a restart",
                            )
                            .clicked();
                        // Confirmed: this deletes the script off the server and
                        // empties the buffer, and it sits between Save and Touch.
                        stop = theme::confirm_button(
                            ui,
                            ui.id().with("stop_script"),
                            "Stop",
                            "Remove the script?",
                        );
                        touch = ui
                            .button("Touch")
                            .on_hover_text("Fire on_touch, as if you had clicked the prim")
                            .clicked();
                    });
                    ui.horizontal(|ui| {
                        theme::section(ui, "Console");
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            clear_console = theme::action_button(ui, "Clear").clicked();
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
                ui.label(egui::RichText::new("Loading…").color(theme::p32().text_dim));
                return;
            }

            reference_ui(ui);

            if editor.source.is_empty() {
                insert_template = ui
                    .button("Start from a template")
                    .on_hover_text("Fill the editor with a script that shows every handler")
                    .clicked();
            }
            let mut layouter = |ui: &egui::Ui, buf: &dyn egui::TextBuffer, wrap_width: f32| {
                let font = egui::TextStyle::Monospace.resolve(ui.style());
                let job = crate::systems::js_highlight::layout_job(buf.as_str(), font, wrap_width);
                ui.fonts_mut(|f| f.layout_job(job))
            };
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    // The code box is dark in both themes: the code colours are one set.
                    theme::use_dark_visuals(ui);
                    ui.add(
                        egui::TextEdit::multiline(&mut editor.source)
                            .code_editor()
                            .layouter(&mut layouter)
                            .desired_width(f32::INFINITY)
                            .desired_rows(18)
                            .hint_text("function on_touch(id, name) {\n    say(\"hi \" + name)\n}"),
                    );
                });
        },
    );

    if clear_console {
        editor.console.clear();
    }
    if insert_template {
        editor.insert_template();
    }
    if save {
        editor.save(&online);
    }
    if stop {
        editor.stop(&online);
    }
    if touch {
        editor.touch(&online);
    }
}
