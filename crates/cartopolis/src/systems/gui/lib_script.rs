//! The script editor drawn by the component library (`lib_host`), beside
//! egui's (`script_editor::render_script_panel`): the prim and whether its
//! script runs, the region's headroom, the API reference, the code, Save &
//! run, Stop, Touch, and the console.
//!
//! On with `CARTO_UI_LIB=script` (or `1`), on the web `?ui=lib`
//! (`&panel=script` opens it). egui's panel steps aside
//! (`ScriptEditorState::lib`) keeping an empty strip. The buffer, the fetch,
//! the guard against a broadcast eating unsaved work and every message sent
//! are `ScriptEditorState`'s, which both editions call, so the two cannot
//! drift apart.
//!
//! Drawn differently: the code is a plain `FieldTextarea` in the monospace
//! family, **without syntax highlighting** — a textarea draws one colour, and
//! the library has no code editor (egui's colours through `js_highlight`);
//! and a command's summary in the reference is its tooltip on the web and
//! not shown natively, as egui shows it only on hover.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use bevy::ecs::system::NonSendMarker;
use bevy::prelude::*;
use cartopolis_core::ScriptLevel;
// The library's, not Bevy's: an explicit import outranks the two globs.
use mithril::prelude::*;
use mithril::prelude::{Button, Event};

use crate::resources::{OnlineSession, RightPanel};
use crate::systems::lib_settings::LibScreens;
use crate::systems::script_editor::{budget_line, ScriptEditorState, PANEL_WIDTH};
use crate::systems::ui_input::UiMetrics;
use crate::systems::{icons, theme};

/// The key this screen mounts under.
const KEY: &str = "script";

/// What decides the body's shape: drawn again only when one of these
/// changes, never while typing, so the code field keeps the keyboard.
#[derive(Clone, PartialEq, Default)]
enum Gate {
    /// No server: a script is server state.
    #[default]
    Offline,
    /// Nothing chosen yet.
    NoPrim,
    /// The prim's source is still on its way.
    Loading,
    /// The prim's source is in the buffer.
    Ready(i64),
}

/// The line about the prim, and the headroom under it.
#[derive(Clone, PartialEq, Default)]
struct Head {
    name: String,
    id: i64,
    running: bool,
    dirty: bool,
    budget: Option<String>,
}

/// One console line, as drawn.
#[derive(Clone, PartialEq)]
struct Line {
    level: ScriptLevel,
    text: String,
}

/// What a tap asked for; done by the system through `ScriptEditorState`.
#[derive(Clone, Copy)]
enum Act {
    Save,
    Stop,
    Touch,
    Clear,
    Template,
    Close,
}

#[derive(Clone)]
struct Live {
    gate: Signal<Gate>,
    head: Signal<Head>,
    source: Signal<String>,
    /// The buffer is empty: "Start from a template" is offered.
    empty: Signal<bool>,
    console: Signal<Vec<Line>>,
    /// Stop pressed once, waiting for the second press.
    armed: Signal<bool>,
    /// The console as last drawn: lines ever pushed, lines held, and whose.
    seen: Rc<Cell<(u64, usize, Option<i64>)>>,
    acts: Rc<RefCell<Vec<Act>>>,
    /// Laid out for a phone.
    compact: bool,
}

impl Live {
    fn act(&self, a: Act) -> Option<Handler> {
        let acts = self.acts.clone();
        Some(Rc::new(move |_: &Event| acts.borrow_mut().push(a)) as Handler)
    }
}

thread_local! {
    static MOUNTED: RefCell<Option<Live>> = const { RefCell::new(None) };
}

fn gate_of(editor: &ScriptEditorState, online: bool) -> Gate {
    match (online, editor.prim_id) {
        (false, _) => Gate::Offline,
        (true, None) => Gate::NoPrim,
        (true, Some(id)) if editor.fetched => Gate::Ready(id),
        (true, Some(_)) => Gate::Loading,
    }
}

fn head_of(editor: &ScriptEditorState, prims: &Query<&crate::components::Prim>) -> Head {
    let id = editor.prim_id.unwrap_or(0);
    Head {
        name: prims
            .iter()
            .find(|p| p.id == id)
            .map_or_else(|| "—".to_string(), |p| p.name.clone()),
        id,
        running: editor.running,
        dirty: editor.is_dirty(),
        budget: editor.budget.map(budget_line),
    }
}

fn console_of(editor: &ScriptEditorState) -> Vec<Line> {
    editor
        .console
        .iter()
        .filter(|l| Some(l.prim_id) == editor.prim_id)
        .map(|l| Line {
            level: l.level,
            text: l.text.clone(),
        })
        .collect()
}

/// Mounts, bridges and unmounts the script editor. `PostUpdate`, before
/// `DriveUi`, as every library screen.
#[allow(clippy::too_many_arguments)]
pub fn drive_lib_script(
    _main: NonSendMarker,
    lib: Res<LibScreens>,
    mut right_panel: ResMut<RightPanel>,
    mut editor: ResMut<ScriptEditorState>,
    online: Option<Res<OnlineSession>>,
    prims: Query<&crate::components::Prim>,
    metrics: Res<UiMetrics>,
    mut opened: Local<bool>,
) {
    if !lib.script {
        return;
    }
    if !editor.lib {
        editor.lib = true;
    }
    if lib.open_script && !*opened {
        *opened = true;
        *right_panel = RightPanel::Script;
    }
    let open = right_panel.is(RightPanel::Script);
    if open && editor.needs_fetch {
        if let Some(online) = online.as_deref() {
            editor.fetch_if_needed(online);
        }
    }
    MOUNTED.with(|m| {
        let mut m = m.borrow_mut();
        match (open, m.as_mut()) {
            (false, None) => {}
            (false, Some(_)) => {
                *m = None;
                crate::systems::lib_host::clear_keyed(KEY);
            }
            (true, None) => {
                let live = Live {
                    gate: signal(gate_of(&editor, online.is_some())),
                    head: signal(head_of(&editor, &prims)),
                    source: signal(editor.source.clone()),
                    empty: signal(editor.source.is_empty()),
                    console: signal(console_of(&editor)),
                    armed: signal(false),
                    seen: Rc::new(Cell::new((
                        editor.pushed,
                        editor.console.len(),
                        editor.prim_id,
                    ))),
                    acts: Rc::new(RefCell::new(Vec::new())),
                    compact: metrics.compact(),
                };
                let p = theme::p();
                crate::systems::lib_host::mount_keyed(
                    KEY,
                    panel(&live),
                    p.ink == theme::DARK.ink,
                    theme::touch_layout(),
                );
                write_html(&live);
                *m = Some(live);
            }
            (true, Some(live)) => {
                // The code field → the buffer, as typed. Only once the
                // buffer is the prim's: before, it is not the script, and
                // typing there would save over one still in flight.
                if editor.fetched && live.source.peek(|s| *s != editor.source) {
                    editor.source = live.source.get_untracked();
                }
                let acts: Vec<Act> = std::mem::take(&mut *live.acts.borrow_mut());
                for a in acts {
                    match a {
                        Act::Clear => editor.console.clear(),
                        Act::Template => editor.insert_template(),
                        Act::Close => *right_panel = RightPanel::None,
                        Act::Save => {
                            if let Some(online) = online.as_deref() {
                                editor.save(online);
                            }
                        }
                        Act::Stop => {
                            if let Some(online) = online.as_deref() {
                                editor.stop(online);
                            }
                        }
                        Act::Touch => {
                            if let Some(online) = online.as_deref() {
                                editor.touch(online);
                            }
                        }
                    }
                }
                mirror(live, &editor, online.is_some(), &prims);
            }
        }
    });
}

/// The editor → the panel's signals, where they moved.
fn mirror(
    live: &Live,
    editor: &ScriptEditorState,
    online: bool,
    prims: &Query<&crate::components::Prim>,
) {
    let gate = gate_of(editor, online);
    if live.gate.get_untracked() != gate {
        live.gate.set(gate);
    }
    let head = head_of(editor, prims);
    if live.head.get_untracked() != head {
        live.head.set(head);
    }
    // The server's copy landed, a template, a Stop: the buffer → the field.
    if live.source.peek(|s| *s != editor.source) {
        live.source.set(editor.source.clone());
    }
    let empty = editor.source.is_empty();
    if live.empty.get_untracked() != empty {
        live.empty.set(empty);
    }
    // Rebuilt only when a line came, went or the prim changed: the console
    // holds two hundred lines.
    let seen = (editor.pushed, editor.console.len(), editor.prim_id);
    if live.seen.get() != seen {
        live.seen.set(seen);
        let lines = console_of(editor);
        if live.console.get_untracked() != lines {
            live.console.set(lines);
        }
    }
}

/// `CARTO_UI_LIB_HTML=<path>`: the panel as HTML, for the browser beside a
/// capture.
fn write_html(l: &Live) {
    if let Some(path) = std::env::var_os("CARTO_UI_LIB_HTML") {
        if let Err(e) = std::fs::write(&path, html(l)) {
            warn!(?path, error = %e, "lib script: could not write the HTML");
        }
    }
}

/// The panel's HTML from the library's string renderer.
fn html(l: &Live) -> String {
    use mithril::view::{mount, StringRenderer};
    let r = Rc::new(RefCell::new(StringRenderer::new()));
    let root = r.borrow().root();
    let mounted = mount(panel(l), &r, &root, None);
    let out = r.borrow().html();
    drop(mounted);
    out
}

// ─── The view ────────────────────────────────────────────────────────────────

fn icon(glyph: &'static str) -> View {
    view! {
        <span style="font-family: 'Material Symbols Rounded'; font-size: 20px; line-height: 1">{glyph}</span>
    }
}

fn note(text: impl Into<String>) -> View {
    let text: String = text.into();
    view! {
        <p style="margin: 0; color: var(--m-text-dim); font-size: var(--m-font-size-sm); line-height: 1.5">{text}</p>
    }
}

/// A group's heading, as the library's section titles are set.
fn group(text: &'static str) -> View {
    view! {
        <p style="margin: 0; color: var(--m-text-dim); font-size: var(--m-font-size-sm); font-weight: 650; letter-spacing: 0.06em; text-transform: uppercase">{text}</p>
    }
}

/// A console line's colour. Errors are the reason the console exists, so
/// they are the one thing that must not read as ordinary output.
fn level_colour(level: ScriptLevel) -> &'static str {
    match level {
        ScriptLevel::Info => "var(--m-text)",
        ScriptLevel::Notice => "var(--m-text-dim)",
        ScriptLevel::Error => "var(--m-danger)",
    }
}

/// The prim, whether the server runs its script, unsaved edits, and the
/// region's headroom.
fn head(l: &Live) -> View {
    let l = l.clone();
    let draw = move || -> View {
        let h = l.head.get();
        // "Running" is the server's word for "I hold a compiled instance",
        // which is not the same as "the source in this box" — hence the
        // separate unsaved marker rather than one combined state.
        let state: View = if h.running {
            view! {
                <span title="The server is running a compiled copy of this script" style="display: inline-flex; align-items: center; gap: 4px; color: var(--m-accent-text)">{icon(icons::RUNNING)}"running"</span>
            }
        } else {
            view! {
                <span title="This prim has no script running" style="display: inline-flex; align-items: center; gap: 4px; color: var(--m-text-dim)">{icon(icons::STOPPED)}"stopped"</span>
            }
        };
        let dirty: View = if h.dirty {
            view! { <Badge tone=Tone::Warning>"unsaved"</Badge> }
        } else {
            View::Empty
        };
        let budget: View = match h.budget {
            Some(line) => view! {
                <p title={crate::systems::script_editor::BUDGET_HINT} style="margin: 0; color: var(--m-text-dim); font-size: var(--m-font-size-sm)">{line}</p>
            },
            None => View::Empty,
        };
        let id = format!("#{}", h.id);
        view! {
            <div style="display: flex; flex-direction: column; gap: 6px">
                <div style="display: flex; flex-wrap: wrap; align-items: center; gap: 4px 12px">
                    <span style="font-weight: 600; color: var(--m-text-strong)">{h.name}</span>
                    <span style="color: var(--m-text-dim)">{id}</span>
                    {state}
                    {dirty}
                </div>
                {budget}
            </div>
        }
    };
    draw.into_view()
}

/// The collapsed API reference: every call and every handler, grouped,
/// from `cartopolis_core::SCRIPT_API`, which the server is tested against.
fn reference() -> View {
    let entry = |f: &'static cartopolis_core::ScriptFn| -> View {
        view! {
            <p title={f.summary} style="margin: 0; font-family: var(--m-font-mono); font-size: var(--m-font-size-sm); color: var(--m-text)">{f.signature}</p>
        }
    };
    let mut items: Vec<View> = vec![note(
        "Handlers you define — the host calls these when something happens:",
    )];
    items.extend(cartopolis_core::SCRIPT_EVENTS.iter().map(entry));
    for g in cartopolis_core::ScriptGroup::all() {
        let calls: Vec<View> = cartopolis_core::api_in_group(*g).map(entry).collect();
        if calls.is_empty() {
            continue;
        }
        let label = g.label();
        items.push(view! {
            <p style="margin: 8px 0 0; font-weight: 600; font-size: var(--m-font-size-sm); color: var(--m-accent-text)">{label}</p>
        });
        items.extend(calls);
    }
    let open = signal(false);
    view! {
        <Collapse model={open} title="Commands">
            <div style="display: flex; flex-direction: column; gap: 4px; padding-top: 8px">{items}</div>
        </Collapse>
    }
}

/// The code: a monospace textarea (no colours — see the module's doc).
fn code(l: &Live, prim: i64) -> View {
    let template = {
        let l = l.clone();
        move || -> View {
            if l.empty.get() {
                view! {
                    <div><Button size=Size::Sm onclick={l.act(Act::Template)} title="Fill the editor with a script that shows every handler">"Start from a template"</Button></div>
                }
            } else {
                View::Empty
            }
        }
    };
    view! {
        {template}
        <div style="font-family: var(--m-font-mono); font-size: var(--m-font-size-sm)">
            <FieldTextarea model={l.source.clone()} rows=18 placeholder="function on_touch(id, name) {\n    say(\"hi \" + name)\n}" id={format!("sc-source-{prim}")} />
        </div>
    }
}

/// Save & run, Stop (asking twice), Touch, and the console under them.
fn foot(l: &Live) -> View {
    let buttons = {
        let l = l.clone();
        move || -> View {
            let dirty = l.head.get().dirty;
            let armed = l.armed.get();
            // Emphasised only while there is something to save: a Save
            // button that always shouts stops meaning anything.
            let save_variant = if dirty {
                Variant::Primary
            } else {
                Variant::Secondary
            };
            let stop: Handler = {
                let (acts, armed_sig) = (l.acts.clone(), l.armed.clone());
                Rc::new(move |_: &Event| {
                    if armed_sig.get_untracked() {
                        armed_sig.set(false);
                        acts.borrow_mut().push(Act::Stop);
                    } else {
                        armed_sig.set(true);
                    }
                })
            };
            let stop_label = if armed { "Remove the script?" } else { "Stop" };
            let stop_variant = if armed {
                Variant::Danger
            } else {
                Variant::Secondary
            };
            view! {
                <div style="display: flex; flex-wrap: wrap; gap: 8px">
                    <Button variant={save_variant} icon={icon(icons::PLAY)} onclick={l.act(Act::Save)} title="Store the script and (re)start it — saving is a restart">"Save & run"</Button>
                    <Button variant={stop_variant} icon={icon(icons::STOP)} onclick={Some(stop)}>{stop_label}</Button>
                    <Button icon={icon(icons::TOUCH)} onclick={l.act(Act::Touch)} title="Fire on_touch, as if you had clicked the prim">"Touch"</Button>
                </div>
            }
        }
    };
    let lines = {
        let l = l.clone();
        move || -> View {
            let lines = l.console.get();
            if lines.is_empty() {
                return note("Nothing yet.");
            }
            let items: Vec<View> = lines
                .into_iter()
                .map(|line| {
                    let style = format!(
                        "margin: 0; white-space: pre-wrap; color: {}",
                        level_colour(line.level)
                    );
                    view! { <p style={style}>{line.text}</p> }
                })
                .collect();
            view! { {items} }
        }
    };
    view! {
        <div style="flex: none; display: flex; flex-direction: column; gap: 12px; padding: 16px 20px 24px; border-top: 1px solid var(--m-border)">
            {buttons}
            <div style="display: flex; align-items: center; gap: 8px">
                <div style="flex: 1">{group("Console")}</div>
                <Button variant=Variant::Ghost size=Size::Sm onclick={l.act(Act::Clear)}>"Clear"</Button>
            </div>
            <div style="max-height: 140px; overflow-y: auto; display: flex; flex-direction: column; gap: 2px; font-family: var(--m-font-mono); font-size: var(--m-font-size-sm)">
                {lines}
            </div>
        </div>
    }
}

/// The panel: a header with ✕, the prim, the reference and the code, and
/// the buttons and the console at the foot.
fn panel(l: &Live) -> View {
    let frame = if l.compact {
        "position: absolute; left: 0; right: 0; top: 0; bottom: 0; display: flex; flex-direction: column; background: var(--m-bg)".to_owned()
    } else {
        format!(
            "position: absolute; right: 0; top: 0; bottom: 0; width: {PANEL_WIDTH}px; display: flex; flex-direction: column; background: var(--m-bg); border-left: 1px solid var(--m-border)"
        )
    };
    let body = {
        let l = l.clone();
        move || -> View {
            match l.gate.get() {
                Gate::Offline => {
                    note("A script lives on the server, so it is written while signed in.")
                }
                Gate::NoPrim => note("Hold or right-click an object and choose Script."),
                Gate::Loading => view! {
                    {head(&l)}
                    <div style="display: flex; align-items: center; gap: 8px; color: var(--m-text-dim); font-size: var(--m-font-size-sm)">
                        <Spinner size=Size::Sm />"Loading…"
                    </div>
                },
                Gate::Ready(prim) => view! {
                    {head(&l)}
                    {reference()}
                    {code(&l, prim)}
                },
            }
        }
    };
    let footer = {
        let l = l.clone();
        move || -> View {
            match l.gate.get() {
                Gate::Ready(_) | Gate::Loading => foot(&l),
                Gate::Offline | Gate::NoPrim => View::Empty,
            }
        }
    };
    let close = l.act(Act::Close);
    view! {
        <div style={frame}>
            <div style="display: flex; align-items: center; gap: 12px; min-height: 56px; padding: 0 12px 0 20px; border-bottom: 1px solid var(--m-border)">
                <span style="color: var(--m-accent)">{icon(RightPanel::Script.icon())}</span>
                <h1 style="flex: 1; margin: 0; font-size: 19px; font-weight: 600; color: var(--m-text-strong)">{RightPanel::Script.label()}</h1>
                <CloseButton onclick={close} />
            </div>
            <div style="flex: 1; min-height: 0; overflow-y: auto; padding: 20px 20px 28px; display: flex; flex-direction: column; gap: 16px">
                {body}
            </div>
            {footer}
        </div>
    }
}
