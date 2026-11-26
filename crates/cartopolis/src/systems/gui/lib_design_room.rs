//! The design room's panels drawn by the component library (`lib_host`),
//! beside egui's (`systems::design_room`): the way out, the tool column,
//! the outliner, the Scripts panel and the bar along the bottom
//! (`docs/mockups/design-room/v4-editor.png`, `design-room.md`).
//!
//! On with `CARTO_UI_LIB=design_room` (or `1`), on the web `?ui=lib`.
//! egui's systems step aside (`DesignRoom::lib`) but keep what they paint
//! over the room: the white veil on the way in and out, the navigation
//! gizmo (`render_design_room_views`), the measurements and the scripts'
//! glow — canvases, egui's until the canvas step. Every button goes through
//! the same `DesignRoom` methods and `design_room` functions egui's call
//! (`keep`, `place_kept`, `group_picked`, `duplicate`, `edit_script` …), so
//! the two editions cannot drift apart.
//!
//! **Dark whatever the theme**, as egui's are (`room_panel`): over the
//! white floor a light panel loses its edges. The library's theme is one
//! for every screen it draws, so while the room is open every library
//! screen is dark; leaving the room sets the app's theme back.
//!
//! **Placement** is egui's: the way out top left, the tool column under
//! it, the outliner beside the column, Scripts top right under the gizmo,
//! the bar centred over the dock.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::rc::Rc;

use bevy::ecs::system::NonSendMarker;
use bevy::prelude::*;
// The library's, not Bevy's: an explicit import outranks the two globs.
use mithril::prelude::*;
use mithril::prelude::{Button, Event};

use crate::components::{Prim, Selected};
use crate::resources::{EditDialogState, GameState, MapAnchor, OnlineSession, RightPanel};
use crate::systems::design_room::{
    self, drafts_line, kept_parts, outline, outliner_note, parts_note, script_rows, scripts_note,
    size_line, DesignRoom, Outline, ScriptRow, ScriptState, SelectedPart, BACK_TIP, BAR_W,
    DUPLICATE_TIP, GROUP_TIP, NAV_GIZMO_H, OUTLINER_W, SCRIPTS_W, SNAP_TIP, TOOLS, TOOL_COLUMN_W,
    UNGROUP_TIP,
};
use crate::systems::gizmo::{GizmoMode, GizmoState};
use crate::systems::lib_place_card::icon;
use crate::systems::lib_settings::LibScreens;
use crate::systems::script_editor::ScriptEditorState;
use crate::systems::ui_input::UiMetrics;
use crate::systems::{icons, model_assets, theme};

/// The pieces' names in the library's host.
const BACK: &str = "design_room_back";
const TOOLS_KEY: &str = "design_room_tools";
const OUTLINER: &str = "design_room_outliner";
const SCRIPTS: &str = "design_room_scripts";
const BAR: &str = "design_room_bar";
/// Mounted for a moment on the way out, to set the app's theme back.
const THEME: &str = "design_room_theme";

/// Points from the top of the screen to the tool column and the outliner:
/// under the way out.
const UNDER_BACK_PTS: f32 = 56.0;

/// What a tap asked for; done by the system.
#[derive(Clone)]
enum Act {
    Leave,
    AddPart,
    Duplicate,
    PlaceDrafts,
    Keep,
    PlaceKept(i64, String),
    Forget(i64, String),
    Pick(Vec<i64>, bool),
    Group,
    Ungroup(i64, String),
    EditScript(i64),
    Tool(GizmoMode),
    Snap,
}

#[derive(Clone, PartialEq)]
struct BarState {
    at: String,
    note: String,
    signed_in: bool,
    /// The selected part: its name and size.
    chosen: Option<(String, String)>,
    drafts: Option<String>,
    busy: bool,
    designs: Vec<(i64, String, String)>,
    status: Option<String>,
}

#[derive(Clone, PartialEq)]
struct OutlinerState {
    at: String,
    note: String,
    lines: Vec<Outline>,
    picked: BTreeSet<i64>,
    signed_in: bool,
    busy: bool,
}

#[derive(Clone, PartialEq)]
struct ScriptsState {
    at: String,
    note: String,
    signed_in: bool,
    rows: Vec<ScriptRow>,
    /// The selected part has no script yet: New script on it.
    new_on: Option<(i64, String)>,
    log: Vec<(String, bool)>,
}

#[derive(Clone, PartialEq)]
struct ToolsState {
    at: String,
    mode: GizmoMode,
    snap: bool,
}

/// What lives across mounts: the taps, the two text fields and what they
/// last showed, whether their buttons can go, and who is signed in.
#[derive(Clone)]
struct Fields {
    acts: Rc<RefCell<Vec<Act>>>,
    name: Signal<String>,
    keep_ready: Signal<bool>,
    group: Signal<String>,
    group_ready: Signal<bool>,
    signed_in: Signal<bool>,
}

impl Fields {
    fn on(&self, a: Act) -> Handler {
        let acts = self.acts.clone();
        Rc::new(move |_: &Event| acts.borrow_mut().push(a.clone())) as Handler
    }

    fn act(&self, a: Act) -> Option<Handler> {
        Some(self.on(a))
    }
}

struct Screens {
    f: Fields,
    name_seen: String,
    group_seen: String,
    back: Option<Signal<String>>,
    tools: Option<Signal<ToolsState>>,
    outliner: Option<Signal<OutlinerState>>,
    scripts: Option<Signal<ScriptsState>>,
    bar: Option<Signal<BarState>>,
}

impl Screens {
    fn any(&self) -> bool {
        self.back.is_some()
            || self.tools.is_some()
            || self.outliner.is_some()
            || self.scripts.is_some()
            || self.bar.is_some()
    }
}

thread_local! {
    static MOUNTED: RefCell<Option<Screens>> = const { RefCell::new(None) };
}

/// Write a signal only when the value moved: a write re-runs what reads it.
fn put<T: Clone + PartialEq + 'static>(s: &Signal<T>, v: T) {
    if s.peek(|now| *now != v) {
        s.set(v);
    }
}

/// Mount, update or take down one piece, dark: `want` is what it should
/// show, `None` for nothing.
fn keep<T: Clone + PartialEq + 'static>(
    slot: &mut Option<Signal<T>>,
    key: &str,
    want: Option<T>,
    draw: impl FnOnce(Signal<T>) -> View,
) {
    match (want, slot.as_ref()) {
        (None, None) => {}
        (None, Some(_)) => {
            *slot = None;
            crate::systems::lib_host::clear_keyed(key);
        }
        (Some(v), None) => {
            let s = signal(v);
            crate::systems::lib_host::mount_keyed(
                key,
                draw(s.clone()),
                true,
                theme::touch_layout(),
            );
            *slot = Some(s);
        }
        (Some(v), Some(s)) => put(s, v),
    }
}

/// Mounts, bridges and unmounts the room's panels. `PostUpdate`, before
/// `DriveUi`, as every library screen.
#[allow(clippy::too_many_arguments)]
pub fn drive_lib_design_room(
    _main: NonSendMarker,
    lib: Res<LibScreens>,
    mut room: ResMut<DesignRoom>,
    mut edit_dialog: ResMut<EditDialogState>,
    mut game_state: ResMut<GameState>,
    map_anchor: Res<MapAnchor>,
    online: Option<Res<OnlineSession>>,
    selected: Query<SelectedPart, With<Selected>>,
    mut gizmo: ResMut<GizmoState>,
    mut editor: ResMut<ScriptEditorState>,
    mut right_panel: ResMut<RightPanel>,
    prims: Query<&Prim>,
    metrics: Res<UiMetrics>,
) {
    if !lib.design_room {
        return;
    }
    if !room.lib {
        room.lib = true;
    }
    let size = metrics.viewport_pts();
    // The library's px are egui's points on the Bevy backend; as DOM they
    // are the page's (`lib_place_card::Placement::over_the_dock`).
    let k = if cfg!(all(feature = "ui-dom", target_arch = "wasm32")) {
        metrics.ui_scale()
    } else {
        1.0
    };
    let chosen = selected.single().ok();
    MOUNTED.with(|m| {
        let mut m = m.borrow_mut();
        let s = m.get_or_insert_with(|| Screens {
            f: Fields {
                acts: Rc::new(RefCell::new(Vec::new())),
                name: signal(room.name.clone()),
                keep_ready: signal(false),
                group: signal(room.group_name.clone()),
                group_ready: signal(false),
                signed_in: signal(false),
            },
            name_seen: room.name.clone(),
            group_seen: room.group_name.clone(),
            back: None,
            tools: None,
            outliner: None,
            scripts: None,
            bar: None,
        });

        // The fields → the room, as egui's `changed()`; the room's moved
        // elsewhere (a new room clears the name) → the fields.
        let typed = s.f.name.get_untracked();
        if typed != s.name_seen {
            s.name_seen = typed.clone();
            room.name = typed;
        } else if room.name != s.name_seen {
            s.name_seen = room.name.clone();
            s.f.name.set(room.name.clone());
        }
        let typed = s.f.group.get_untracked();
        if typed != s.group_seen {
            s.group_seen = typed.clone();
            room.group_name = typed;
        } else if room.group_name != s.group_seen {
            s.group_seen = room.group_name.clone();
            s.f.group.set(room.group_name.clone());
        }

        // The taps of the frame, through the methods egui's buttons call.
        let acts: Vec<Act> = std::mem::take(&mut *s.f.acts.borrow_mut());
        for a in acts {
            match a {
                Act::Leave => room.close(),
                Act::AddPart => {
                    design_room::add_part(&room, &map_anchor, &mut edit_dialog, &mut game_state)
                }
                Act::Duplicate => design_room::duplicate(online.as_deref(), chosen),
                Act::PlaceDrafts => room.place_drafts_here(),
                Act::Keep => {
                    if room.can_keep() {
                        room.keep();
                    }
                }
                Act::PlaceKept(id, name) => room.place_kept(id, name),
                Act::Forget(id, name) => room.forget_kept(id, name),
                Act::Pick(ids, on) => room.pick(&ids, on),
                Act::Group => {
                    if room.can_group() {
                        room.group_picked();
                    }
                }
                Act::Ungroup(id, name) => room.dissolve(id, name),
                Act::EditScript(id) => {
                    design_room::edit_script(&mut editor, &mut right_panel, id);
                }
                Act::Tool(mode) => {
                    if gizmo.mode != mode {
                        gizmo.mode = mode;
                    }
                }
                Act::Snap => room.no_snap = !room.no_snap,
            }
        }

        let open = room.spot.is_some();
        let signed_in = online.is_some() && model_assets::have_endpoint();
        put(&s.f.signed_in, signed_in);
        put(&s.f.keep_ready, room.can_keep());
        put(&s.f.group_ready, room.can_group());
        let f = s.f.clone();
        let m_ = theme::OSD_MARGIN;
        let was_up = s.any();

        let back = open.then(|| {
            format!(
                "position: absolute; left: {:.0}px; top: {:.0}px",
                m_ * k,
                m_ * k
            )
        });
        keep(&mut s.back, BACK, back, |st| back_view(st, &f));

        let tools = open.then(|| ToolsState {
            at: format!(
                "position: absolute; left: {:.0}px; top: {:.0}px",
                m_ * k,
                (m_ + UNDER_BACK_PTS) * k
            ),
            mode: gizmo.mode,
            snap: !room.no_snap,
        });
        keep(&mut s.tools, TOOLS_KEY, tools, |st| tools_view(st, &f));

        let show_outliner = open && !room.tree.is_empty();
        // At most down to above the bar (about 200 pt), and scrolling past
        // that: a stall's parts ran out of the panel and over everything
        // under it.
        let outliner_max =
            (size.y - (m_ + UNDER_BACK_PTS) - crate::systems::dock::reserved_bottom() - 220.0)
                .max(160.0);
        let outliner = show_outliner.then(|| OutlinerState {
            at: format!(
                "position: absolute; left: {:.0}px; top: {:.0}px; width: {:.0}px; \
                 max-height: {:.0}px; overflow-y: auto",
                (m_ + TOOL_COLUMN_W) * k,
                (m_ + UNDER_BACK_PTS) * k,
                OUTLINER_W.min(size.x * 0.45) * k,
                outliner_max * k
            ),
            note: outliner_note(&room.tree),
            lines: outline(&room.tree),
            picked: room.picked.clone(),
            signed_in,
            busy: room.busy,
        });
        keep(&mut s.outliner, OUTLINER, outliner, |st| {
            outliner_view(st, &f)
        });

        // A right panel (the script editor itself) takes that side. On a
        // phone the outliner and this do not fit side by side (egui's
        // covered this one): the outliner first, this when it is not up.
        let narrow = size.x < crate::systems::egui_manager::COMPACT_WIDTH_PTS;
        let scripts = (open && right_panel.is(RightPanel::None) && !(narrow && show_outliner))
            .then(|| {
                let rows = script_rows(&room, &prims, &editor);
                ScriptsState {
                    at: format!(
                        "position: absolute; right: {:.0}px; top: {:.0}px; width: {:.0}px",
                        m_ * k,
                        (m_ + NAV_GIZMO_H) * k,
                        SCRIPTS_W.min(size.x * 0.45) * k
                    ),
                    note: scripts_note(rows.len()),
                    signed_in: online.is_some(),
                    new_on: chosen
                        .map(|(p, ..)| p)
                        .filter(|p| room.parts.contains(&p.id) && !p.scripted)
                        .map(|p| (p.id, p.name.clone())),
                    rows,
                    log: room
                        .script_log
                        .iter()
                        .rev()
                        .take(6)
                        .map(|l| (l.text.clone(), l.error))
                        .collect(),
                }
            });
        keep(&mut s.scripts, SCRIPTS, scripts, |st| scripts_view(st, &f));

        let bar = open.then(|| {
            let width = BAR_W.min(size.x - 2.0 * m_).max(0.0);
            BarState {
                at: format!(
                    "position: absolute; left: {:.0}px; bottom: {:.0}px; width: {:.0}px",
                    ((size.x - width) / 2.0).max(0.0) * k,
                    (crate::systems::dock::reserved_bottom() + 8.0) * k,
                    width * k
                ),
                note: parts_note(room.parts.len()),
                signed_in,
                chosen: chosen.map(|(p, tf, ..)| (p.name.clone(), size_line(tf))),
                drafts: (!room.drafts.is_empty()).then(|| drafts_line(room.drafts.len())),
                busy: room.busy,
                designs: room
                    .designs
                    .iter()
                    .map(|d| (d.id, d.name.clone(), kept_parts(d.parts)))
                    .collect(),
                status: if room.busy {
                    Some("Working…".to_owned())
                } else {
                    (!room.status.is_empty()).then(|| room.status.clone())
                },
            }
        });
        keep(&mut s.bar, BAR, bar, |st| bar_view(st, &f));

        // Out of the room: the app's own theme back for every library
        // screen still up.
        if was_up && !s.any() {
            let p = theme::p();
            crate::systems::lib_host::mount_keyed(
                THEME,
                View::Empty,
                p.ink == theme::DARK.ink,
                theme::touch_layout(),
            );
            crate::systems::lib_host::clear_keyed(THEME);
        }
    });
}

/// A room panel's frame at `at`: the library's surface, dark.
fn panel_style(at: &str) -> String {
    format!(
        "{at}; display: flex; flex-direction: column; gap: 12px; padding: 16px; \
         background: var(--m-surface); border: 1px solid var(--m-border); \
         border-radius: var(--m-radius-lg); box-shadow: var(--m-shadow-lg)"
    )
}

/// A panel's title row: its icon on a small accent plate, the title, a
/// dim note on the right, a rule under it.
fn title_row(glyph: &'static str, title: &'static str, note: String) -> View {
    view! {
        <div style="display: flex; align-items: center; gap: 12px; padding-bottom: 12px; border-bottom: 1px solid var(--m-border)">
            <span style="display: flex; align-items: center; justify-content: center; width: 28px; height: 28px; border-radius: var(--m-radius); background: var(--m-accent-soft); color: var(--m-accent-text)">{icon(glyph)}</span>
            <span style="flex: 1; min-width: 0; font-size: 17px; font-weight: 600; color: var(--m-text-strong)">{title}</span>
            <span style="color: var(--m-text-dim); font-size: var(--m-font-size-sm)">{note}</span>
        </div>
    }
}

fn dim(text: String) -> View {
    view! {
        <p style="margin: 0; color: var(--m-text-dim); font-size: var(--m-font-size-sm); line-height: 1.45">{text}</p>
    }
}

/// A group's heading, as the library's section titles are set.
fn group(text: &'static str) -> View {
    view! {
        <p style="margin: 4px 0 0; color: var(--m-text-dim); font-size: var(--m-font-size-sm); font-weight: 650; letter-spacing: 0.06em; text-transform: uppercase">{text}</p>
    }
}

/// A row shown only while signed in: a text field must not be re-drawn
/// while it is typed in, so it is never inside a part that changes.
fn when_signed_in(f: &Fields, body: View) -> View {
    let signed_in = f.signed_in.clone();
    let style = move || {
        if signed_in.get() {
            "display: flex; align-items: flex-end; gap: 8px"
        } else {
            "display: none"
        }
    };
    view! { <div style={style}>{body}</div> }
}

/// The one way out, always in the same place.
fn back_view(at: Signal<String>, f: &Fields) -> View {
    let style = move || at.get();
    view! {
        <div style={style} data-theme="dark">
            <Button icon={icon(icons::BACK)} onclick={f.act(Act::Leave)} title={BACK_TIP}>"Back to the map"</Button>
        </div>
    }
}

/// One tool: its key large, its word small, lit while it is the one.
fn tool(on: bool, key: &'static str, label: &'static str, tip: &'static str, tap: Handler) -> View {
    let style = format!(
        "display: flex; flex-direction: column; align-items: center; justify-content: center; gap: 2px; width: 44px; height: 48px; border-radius: var(--m-radius); cursor: pointer; {}",
        if on {
            "background: var(--m-accent); color: var(--m-on-accent)"
        } else {
            "background: var(--m-surface-raised); color: var(--m-text)"
        }
    );
    view! {
        <div style={style} title={tip} on:click={move |e: &Event| tap(e)}>
            <span style="font-size: 16px; font-weight: 600; line-height: 1">{key}</span>
            <span style="font-size: 10px; line-height: 1">{label}</span>
        </div>
    }
}

/// The tool column: move, turn, size; copy and the magnet.
fn tools_view(st: Signal<ToolsState>, f: &Fields) -> View {
    let frame = {
        let st = st.clone();
        move || {
            format!(
                "{}; display: flex; flex-direction: column; gap: 6px; padding: 8px; \
                 background: var(--m-surface); border: 1px solid var(--m-border); \
                 border-radius: var(--m-radius-lg); box-shadow: var(--m-shadow-lg)",
                st.get().at
            )
        }
    };
    let body = {
        let f = f.clone();
        move || -> View {
            let t = st.get();
            let modes: Vec<View> = TOOLS
                .iter()
                .map(|&(m, key, label, tip)| tool(t.mode == m, key, label, tip, f.on(Act::Tool(m))))
                .collect();
            view! {
                {modes}
                <div style="height: 1px; margin: 2px 0; background: var(--m-border)"></div>
                {tool(false, "D", "copy", DUPLICATE_TIP, f.on(Act::Duplicate))}
                {tool(t.snap, icons::FOLLOW, "snap", SNAP_TIP, f.on(Act::Snap))}
            }
        }
    };
    view! {
        <div style={frame} data-theme="dark" on:click={|_: &Event| {}}>{body}</div>
    }
}

/// The outliner: the parts in their groups, a tick on each for Group, and
/// Ungroup on each group.
fn outliner_view(st: Signal<OutlinerState>, f: &Fields) -> View {
    let frame = {
        let st = st.clone();
        move || panel_style(&st.get().at)
    };
    let title = {
        let st = st.clone();
        move || title_row(icons::LAYERS, "Outliner", st.get().note)
    };
    let list = {
        let st = st.clone();
        let f = f.clone();
        move || -> View {
            let o = st.get();
            let rows: Vec<View> = o
                .lines
                .iter()
                .map(|line| match line {
                    Outline::Group {
                        depth,
                        id,
                        name,
                        members,
                    } => {
                        let all = members.iter().all(|m| o.picked.contains(m));
                        let some = !all && members.iter().any(|m| o.picked.contains(m));
                        let ids = members.clone();
                        let acts = f.acts.clone();
                        let tick: Rc<dyn Fn(&bool)> = Rc::new(move |v: &bool| {
                            acts.borrow_mut().push(Act::Pick(ids.clone(), *v));
                        });
                        let ungroup: View = if o.signed_in {
                            view! {
                                <Button variant=Variant::Ghost size=Size::Sm disabled={o.busy} onclick={f.act(Act::Ungroup(*id, name.clone()))} title={UNGROUP_TIP}>"Ungroup"</Button>
                            }
                        } else {
                            View::Empty
                        };
                        let style = format!(
                            "display: flex; align-items: center; gap: 8px; min-height: 36px; padding-left: {}px",
                            depth * 16
                        );
                        let count = members.len().to_string();
                        view! {
                            <div style={style}>
                                <FieldCheckbox model={signal(all)} indeterminate={some} onchange={Some(tick)} />
                                <span style="color: var(--m-warning)">{icon(icons::LAYERS)}</span>
                                <span style="flex: 1; min-width: 0; color: var(--m-text-strong)">{name.clone()}<span style="margin-left: 8px; color: var(--m-text-dim); font-size: var(--m-font-size-sm)">{count}</span></span>
                                {ungroup}
                            </div>
                        }
                    }
                    Outline::Part {
                        depth,
                        id,
                        name,
                        draft,
                    } => {
                        let on = o.picked.contains(id);
                        let ids = vec![*id];
                        let acts = f.acts.clone();
                        let tick: Rc<dyn Fn(&bool)> = Rc::new(move |v: &bool| {
                            acts.borrow_mut().push(Act::Pick(ids.clone(), *v));
                        });
                        let badge: View = if *draft {
                            view! { <Badge tone=Tone::Warning>"draft"</Badge> }
                        } else {
                            View::Empty
                        };
                        // A ticked row is lit, as the sheet's selected one is.
                        let style = format!(
                            "display: flex; align-items: center; gap: 8px; min-height: 36px; padding: 0 8px 0 {}px; border-radius: var(--m-radius); {}",
                            depth * 16 + 4,
                            if on {
                                "background: var(--m-accent-soft)"
                            } else {
                                ""
                            }
                        );
                        view! {
                            <div style={style}>
                                <FieldCheckbox model={signal(on)} onchange={Some(tick)} />
                                <span style="color: var(--m-text-dim)">{icon(icons::BUILD)}</span>
                                <span style="flex: 1; min-width: 0; color: var(--m-text)">{name.clone()}</span>
                                {badge}
                            </div>
                        }
                    }
                })
                .collect();
            view! {
                <div style="max-height: 340px; overflow-y: auto; display: flex; flex-direction: column; gap: 2px">{rows}</div>
            }
        }
    };
    let make = {
        let f = f.clone();
        move || -> View {
            view! {
                <Button icon={icon(icons::LAYERS)} disabled={!f.group_ready.get()} onclick={f.act(Act::Group)} title={GROUP_TIP}>"Group"</Button>
            }
        }
    };
    let field = when_signed_in(
        f,
        view! {
            <div style="flex: 1; min-width: 0">
                <FieldText model={f.group.clone()} placeholder="Group name" autocomplete="off" id="dr-group" />
            </div>
            {make}
        },
    );
    view! {
        <div style={frame} data-theme="dark" on:click={|_: &Event| {}}>
            {title}
            {list}
            {field}
        </div>
    }
}

/// The Scripts panel: every scripted part and how it stands, Edit, New
/// script for the selected part, and what they did.
fn scripts_view(st: Signal<ScriptsState>, f: &Fields) -> View {
    let frame = {
        let st = st.clone();
        move || panel_style(&st.get().at)
    };
    let body = {
        let f = f.clone();
        move || -> View {
            let s = st.get();
            let title = title_row(icons::SCRIPT, "Scripts", s.note.clone());
            if !s.signed_in {
                return view! {
                    {title}
                    {dim("Scripts run on the server: sign in to write one.".to_owned())}
                };
            }
            // Always there while the room is open, so it is found before it
            // is needed: empty, it says how a script starts.
            let empty: View = if s.rows.is_empty() && s.new_on.is_none() {
                dim(
                    "No scripts here yet. Select a part, and a New script button appears here."
                        .to_owned(),
                )
            } else {
                View::Empty
            };
            let rows: Vec<View> = s
                .rows
                .iter()
                .map(|r| {
                    let tone = match r.state {
                        ScriptState::Error => Tone::Danger,
                        ScriptState::Working => Tone::Success,
                        ScriptState::Running => Tone::Accent,
                    };
                    let why: View = match &r.why {
                        Some(w) => view! {
                            <p style="margin: 0; padding-left: 32px; color: var(--m-danger); font-size: 12px">{w.clone()}</p>
                        },
                        None => View::Empty,
                    };
                    view! {
                        <div style="display: flex; flex-direction: column; gap: 4px">
                            <div style="display: flex; align-items: center; gap: 8px; min-height: 36px">
                                <span style="color: var(--m-accent-text)">{icon(icons::SCRIPT)}</span>
                                <span style="flex: 1; min-width: 0; color: var(--m-text-strong)">{r.name.clone()}</span>
                                <Badge tone={tone}>{r.state.word()}</Badge>
                                <Button variant=Variant::Ghost size=Size::Sm onclick={f.act(Act::EditScript(r.id))} title="open it in the script editor">"Edit"</Button>
                            </div>
                            {why}
                        </div>
                    }
                })
                .collect();
            let new: View = match &s.new_on {
                Some((id, name)) => {
                    let label = format!("New script on {name}");
                    view! {
                        <Button icon={icon(icons::ADD)} onclick={f.act(Act::EditScript(*id))} title="write JavaScript that runs on the selected part">{label}</Button>
                    }
                }
                None => View::Empty,
            };
            let log: View = if s.log.is_empty() {
                View::Empty
            } else {
                let lines: Vec<View> = s
                    .log
                    .iter()
                    .map(|(text, error)| {
                        let style = format!(
                            "margin: 0; font-family: var(--m-font-mono); font-size: 12px; color: {}",
                            if *error {
                                "var(--m-danger)"
                            } else {
                                "var(--m-text)"
                            }
                        );
                        view! { <p style={style}>{text.clone()}</p> }
                    })
                    .collect();
                view! {
                    {group("What they did")}
                    <div style="display: flex; flex-direction: column; gap: 4px; padding: 8px 12px; border-radius: var(--m-radius); background: var(--m-surface-sunken)">{lines}</div>
                }
            };
            view! {
                {title}
                {empty}
                {rows}
                {new}
                {log}
            }
        }
    };
    view! {
        <div style={frame} data-theme="dark" on:click={|_: &Event| {}}>{body}</div>
    }
}

/// The bar along the bottom: add a part, keep the design, place one kept
/// earlier. No ✕: the way out is top left, and Escape.
fn bar_view(st: Signal<BarState>, f: &Fields) -> View {
    let frame = {
        let st = st.clone();
        move || panel_style(&st.get().at)
    };
    let top = {
        let st = st.clone();
        let f = f.clone();
        move || -> View {
            let b = st.get();
            let title = title_row(icons::PALETTE, "Design room", b.note.clone());
            if !b.signed_in {
                return view! {
                    {title}
                    {dim("Sign in to build here and keep what you make.".to_owned())}
                };
            }
            let chosen: View = match &b.chosen {
                Some((name, size)) => view! {
                    <div style="display: flex; align-items: baseline; gap: 12px">
                        <span style="color: var(--m-text-strong)">{name.clone()}</span>
                        <span style="color: var(--m-accent-text)">{size.clone()}</span>
                    </div>
                },
                None => View::Empty,
            };
            let drafts: View = match &b.drafts {
                Some(line) => view! {
                    <div style="display: flex; align-items: center; flex-wrap: wrap; gap: 8px">
                        <Badge tone=Tone::Warning>"draft"</Badge>
                        <span style="flex: 1; min-width: 0; color: var(--m-text-dim); font-size: var(--m-font-size-sm)">{line.clone()}</span>
                        <Button variant=Variant::Primary disabled={b.busy} onclick={f.act(Act::PlaceDrafts)} title="put the drafts here in the world, for everybody">"Place in the street"</Button>
                    </div>
                },
                None => View::Empty,
            };
            view! {
                {title}
                <div style="display: flex; flex-wrap: wrap; gap: 8px">
                    <Button icon={icon(icons::ADD)} onclick={f.act(Act::AddPart)} title="a new box on the spot, in the editor">"Add a part"</Button>
                    <Button icon={icon(icons::COPY)} disabled={b.chosen.is_none()} onclick={f.act(Act::Duplicate)} title={DUPLICATE_TIP}>"Duplicate"</Button>
                </div>
                {chosen}
                {drafts}
            }
        }
    };
    let keep_button = {
        let f = f.clone();
        move || -> View {
            view! {
                <Button icon={icon(icons::BOOKMARK_ADD)} disabled={!f.keep_ready.get()} onclick={f.act(Act::Keep)} title="keep the objects here in your inventory, as one design">"Keep"</Button>
            }
        }
    };
    let field = when_signed_in(
        f,
        view! {
            <div style="flex: 1; min-width: 0">
                <FieldText model={f.name.clone()} placeholder="Name it to keep it" autocomplete="off" id="dr-name" />
            </div>
            {keep_button}
        },
    );
    let foot = {
        let f = f.clone();
        move || -> View {
            let b = st.get();
            if !b.signed_in {
                return View::Empty;
            }
            let designs: View = if b.designs.is_empty() {
                View::Empty
            } else {
                let rows: Vec<View> = b
                    .designs
                    .iter()
                    .map(|(id, name, parts)| {
                        let end: View = view! {
                            <span style="display: flex; gap: 4px">
                                <Button size=Size::Sm disabled={b.busy} onclick={f.act(Act::PlaceKept(*id, name.clone()))} title="a copy of it on this spot, as a draft">"Place here"</Button>
                                <Button variant=Variant::Ghost size=Size::Sm icon={icon(icons::DELETE)} icon_only=true onclick={f.act(Act::Forget(*id, name.clone()))} title="Remove it from your objects; copies already placed stay" />
                            </span>
                        };
                        view! {
                            <ListItem icon={icon(icons::CABINET)} sub={parts.clone().into_view()} end={end}>{name.clone()}</ListItem>
                        }
                    })
                    .collect();
                view! {
                    {group("Your objects")}
                    <div style="max-height: 160px; overflow-y: auto"><List>{rows}</List></div>
                }
            };
            let status: View = match b.status {
                Some(s) => dim(s),
                None => View::Empty,
            };
            view! { {designs}{status} }
        }
    };
    view! {
        <div style={frame} data-theme="dark" on:click={|_: &Event| {}}>
            {top}
            {field}
            {foot}
        </div>
    }
}
