//! Developer tools drawn by the component library (`lib_host`), beside
//! egui's (`dev_panel::render_dev_panel`): the tab row, then Frame, View,
//! Profiler, Switches, Precision or Midas — every readout, switch and button
//! egui has, as a plain sheet of sections.
//!
//! On with `CARTO_UI_LIB=dev_panel` (or `1`), on the web `?ui=lib`. egui's
//! system steps aside (`DevTools::lib`) but still measures everything —
//! the census on its throttle, the camera fix, the switches, the walk test —
//! and publishes it in `DevTools::lib_view`. A tap here is a `DevAct`,
//! handed back in `DevTools::lib_acts` and done by `dev_panel::apply`, the
//! code egui's controls run.
//!
//! **Midas** in the apps is `midas_link`'s: it publishes the tab as
//! `MidasTabView` (in `DevBody::Midas`) and does its taps, `MidasAct` in
//! `DevAct::Midas`, in `MidasLink::apply_tab`, the code egui's
//! `midas_link::midas_tab` runs. The fields keep their own words and the
//! acts carry them; the words field clears when a push goes
//! (`MidasTabView::pushed`). Release key is `lib_release_approval::
//! key_section_view`, which does its own taps. In the browser the tab is a
//! line.
//!
//! **Placement.** Where egui's window opens: `DEV_PANEL_LEFT` + `OSD_GAP`
//! in, `OSD_MARGIN` down, 300 wide. egui's window is draggable and
//! remembered; this sheet is fixed. Where that spot does not fit (a phone,
//! 275 pt), egui's window ran off the right edge; this one spans the width
//! less its margins instead. Its height stops above the dock, and the
//! body scrolls.

use std::cell::RefCell;
use std::rc::Rc;

use bevy::ecs::system::NonSendMarker;
use bevy::prelude::*;
// The library's, not Bevy's: an explicit import outranks the two globs.
use mithril::prelude::*;
use mithril::prelude::{Button, Event};

use crate::systems::dev_panel::{
    switches_on, DevAct, DevBody, DevPanelView, DevRow, DevSection, DevTab, DevTone, DevTools,
    PrecisionView, SwitchRow, CLOCK_NOTE, COPY_HELP, DEV_PANEL_LEFT, DEV_PANEL_WIDTH, GROUP_HELP,
    LOG_EMPTY_NOTE, NEEDS_A_PHONE, STAND_IN_NOTE, SWITCHES_NOTE, WALK_NOTE, WALK_STEPS,
};
use crate::systems::egui_manager::EguiRef;
use crate::systems::high_precision::HandWord;
use crate::systems::lib_place_card::icon;
use crate::systems::lib_settings::LibScreens;
#[cfg(not(target_arch = "wasm32"))]
use crate::systems::midas_link::{MidasAct, MidasTabView, PAIR_HOW, PAIR_NOTE, WORDS_HINT};
use crate::systems::{icons, theme};

/// The sheet's name in the library's host.
const KEY: &str = "dev_panel";

/// The readout name column, px: egui's `stat`.
const NAME_COLUMN: f32 = 88.0;

/// Where the sheet stands, px in the library's root.
#[derive(Clone, Copy, PartialEq, Default)]
struct Placement {
    left: f32,
    top: f32,
    width: f32,
    max_height: f32,
}

impl Placement {
    /// egui's opening spot, or the width less its margins where that spot
    /// runs off the screen. `k` is the library's px per egui point.
    fn of(viewport: egui::Vec2, k: f32) -> Self {
        let mut left = DEV_PANEL_LEFT + theme::OSD_GAP;
        let mut width = DEV_PANEL_WIDTH;
        if left + width + theme::OSD_MARGIN > viewport.x {
            left = theme::OSD_MARGIN;
            width = (viewport.x - 2.0 * theme::OSD_MARGIN).max(0.0);
        }
        let top = theme::OSD_MARGIN;
        let bottom = crate::systems::dock::reserved_bottom() + theme::OSD_MARGIN;
        Placement {
            left: left * k,
            top: top * k,
            width: width * k,
            max_height: (viewport.y - top - bottom).max(0.0) * k,
        }
    }

    fn style(&self) -> String {
        format!(
            "position: absolute; left: {:.0}px; top: {:.0}px; width: {:.0}px; max-height: {:.0}px; \
             box-sizing: border-box; display: flex; flex-direction: column; \
             background: var(--m-surface); border: 1px solid var(--m-border); \
             border-radius: var(--m-radius-lg); box-shadow: var(--m-shadow-lg); overflow: hidden",
            self.left, self.top, self.width, self.max_height
        )
    }
}

type Acts = Rc<RefCell<Vec<DevAct>>>;

#[derive(Clone)]
struct Live {
    dark: bool,
    /// The library's px per egui point: 1 on the Bevy backend, the UI scale
    /// as DOM (`lib_place_card::Placement::over_the_dock` says why).
    k: f32,
    view: Signal<DevPanelView>,
    /// The tab alone, so a readout that moves does not rebuild the tab's
    /// layout (and the group field keeps its focus).
    tab: Signal<DevTab>,
    placement: Signal<Placement>,
    /// The shot preset's group field.
    group: Signal<String>,
    /// The precision log's tail, for the scroller to follow.
    log: Signal<Option<String>>,
    acts: Acts,
    #[cfg(not(target_arch = "wasm32"))]
    midas: MidasLive,
}

/// The Midas tab's own state, kept across its redraws.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone)]
struct MidasLive {
    /// Paired or not, alone: the view moving does not rebuild the form (and
    /// the fields keep their focus).
    paired: Signal<bool>,
    /// The words to send with a push, kept across tabs; cleared when one goes.
    words: Signal<String>,
    /// The newest Midas view, to fill the pairing fields from when they are
    /// drawn. Set before the tab moves, so a Midas body built on the switch
    /// finds it.
    latest: Rc<RefCell<Option<MidasTabView>>>,
}

#[cfg(not(target_arch = "wasm32"))]
impl MidasLive {
    fn of(v: &DevPanelView) -> Self {
        let m = midas_of(v);
        MidasLive {
            paired: signal(m.is_some_and(|m| m.paired)),
            words: signal(String::new()),
            latest: Rc::new(RefCell::new(m.cloned())),
        }
    }
}

impl Live {
    fn act(&self, a: DevAct) -> Option<Handler> {
        let acts = self.acts.clone();
        Some(Rc::new(move |_: &Event| acts.borrow_mut().push(a.clone())) as Handler)
    }
}

struct Mounted {
    live: Live,
    /// The group as last agreed between the field and `ScreenshotState`:
    /// the field moving from it is typing, the resource moving from it is
    /// the app's.
    group_seen: String,
    /// The Midas pushes last seen (`MidasTabView::pushed`), once the tab
    /// has been shown.
    #[cfg(not(target_arch = "wasm32"))]
    pushed_seen: Option<u32>,
}

thread_local! {
    static MOUNTED: RefCell<Option<Mounted>> = const { RefCell::new(None) };
}

/// The group a view carries, on the View tab.
fn group_of(v: &DevPanelView) -> Option<&String> {
    match &v.body {
        DevBody::View { group, .. } => Some(group),
        _ => None,
    }
}

/// The link a view carries, on the Midas tab.
#[cfg(not(target_arch = "wasm32"))]
fn midas_of(v: &DevPanelView) -> Option<&MidasTabView> {
    match &v.body {
        DevBody::Midas(m) => Some(m),
        _ => None,
    }
}

/// The log a view carries, on the Precision tab.
fn log_of(v: &DevPanelView) -> Option<String> {
    match &v.body {
        DevBody::Precision(p) => p.log.clone(),
        _ => None,
    }
}

/// Mounts, bridges and unmounts the panel. `PostUpdate`, before `DriveUi`,
/// as every library screen.
pub fn drive_lib_dev_panel(
    _main: NonSendMarker,
    lib: Res<LibScreens>,
    mut tools: ResMut<DevTools>,
    egui: EguiRef,
) {
    if !lib.dev_panel {
        return;
    }
    if !tools.lib {
        tools.lib = true;
    }
    let dark = theme::p().ink == theme::DARK.ink;
    let k = if cfg!(all(feature = "ui-dom", target_arch = "wasm32")) {
        egui.ui_scale()
    } else {
        1.0
    };
    let placement = Placement::of(egui.viewport_pts(), k);
    MOUNTED.with(|m| {
        let mut m = m.borrow_mut();
        // The taps of the frame, and the group as typed, for egui's system
        // to do next frame.
        if let Some(mounted) = m.as_mut() {
            let mut acts: Vec<DevAct> = std::mem::take(&mut *mounted.live.acts.borrow_mut());
            let typed = mounted.live.group.get_untracked();
            if typed != mounted.group_seen {
                acts.push(DevAct::Group(typed.clone()));
                mounted.group_seen = typed;
            }
            if !acts.is_empty() {
                tools.lib_acts.extend(acts);
            }
        }
        // Closed (or on Midas in the apps), or a new theme or scale: down
        // (to mount again).
        let stale = m
            .as_ref()
            .is_some_and(|x| tools.lib_view.is_none() || x.live.dark != dark || x.live.k != k);
        if stale {
            *m = None;
            crate::systems::lib_host::clear_keyed(KEY);
        }
        let Some(view) = tools.lib_view.clone() else {
            return;
        };
        match m.as_mut() {
            None => {
                let group = group_of(&view).cloned().unwrap_or_default();
                #[cfg(not(target_arch = "wasm32"))]
                let pushed_seen = midas_of(&view).map(|m| m.pushed);
                let live = Live {
                    dark,
                    k,
                    #[cfg(not(target_arch = "wasm32"))]
                    midas: MidasLive::of(&view),
                    tab: signal(view.tab),
                    log: signal(log_of(&view)),
                    view: signal(view),
                    placement: signal(placement),
                    group: signal(group.clone()),
                    acts: Rc::new(RefCell::new(Vec::new())),
                };
                crate::systems::lib_host::mount_keyed(
                    KEY,
                    view_of(&live),
                    dark,
                    theme::touch_layout(),
                );
                if let Some(path) = std::env::var_os("CARTO_UI_LIB_HTML") {
                    if let Err(e) = std::fs::write(&path, html(&live)) {
                        warn!(?path, error = %e, "lib dev panel: could not write the HTML");
                    }
                }
                *m = Some(Mounted {
                    live,
                    group_seen: group,
                    #[cfg(not(target_arch = "wasm32"))]
                    pushed_seen,
                });
            }
            Some(x) => {
                // The link first, so a Midas body built when the tab moves
                // below finds it.
                #[cfg(not(target_arch = "wasm32"))]
                if let Some(now) = midas_of(&view) {
                    let midas = &x.live.midas;
                    *midas.latest.borrow_mut() = Some(now.clone());
                    if x.pushed_seen.is_some_and(|seen| seen != now.pushed) {
                        midas.words.set(String::new());
                    }
                    x.pushed_seen = Some(now.pushed);
                    if midas.paired.get_untracked() != now.paired {
                        midas.paired.set(now.paired);
                    }
                }
                // The app moved the group (or the View tab came back): into
                // the field, unless the person is mid-way through typing.
                if let Some(group) = group_of(&view) {
                    if *group != x.group_seen && x.live.group.get_untracked() == x.group_seen {
                        x.live.group.set(group.clone());
                        x.group_seen = group.clone();
                    }
                }
                if x.live.tab.get_untracked() != view.tab {
                    x.live.tab.set(view.tab);
                }
                let log = log_of(&view);
                if x.live.log.peek(|now| *now != log) {
                    x.live.log.set(log);
                }
                if x.live.view.peek(|now| *now != view) {
                    x.live.view.set(view);
                }
                if x.live.placement.get_untracked() != placement {
                    x.live.placement.set(placement);
                }
            }
        }
    });
}

/// The HTML from the library's string renderer, for the browser beside a
/// capture.
fn html(l: &Live) -> String {
    use mithril::view::{mount, StringRenderer};
    let r = Rc::new(RefCell::new(StringRenderer::new()));
    let root = r.borrow().root();
    let mounted = mount(view_of(l), &r, &root, None);
    let out = r.borrow().html();
    drop(mounted);
    out
}

/// The sheet: the title and ✕, the tab row, and the tab's body, scrolling.
fn view_of(l: &Live) -> View {
    let frame = {
        let placement = l.placement.clone();
        move || placement.get().style()
    };
    let title = format!(
        "flex: 1; margin: 0; font-size: {}px; font-weight: 600; color: var(--m-text-strong)",
        theme::SIZE_TITLE
    );
    let tabs = {
        let l = l.clone();
        move || -> View {
            let now = l.tab.get();
            let buttons: Vec<View> = DevTab::ALL
                .iter()
                .map(|&t| {
                    let label = t.label();
                    if t == now {
                        view! { <Button variant=Variant::Primary size=Size::Sm onclick={l.act(DevAct::Tab(t))}>{label}</Button> }
                    } else {
                        view! { <Button variant=Variant::Ghost size=Size::Sm onclick={l.act(DevAct::Tab(t))}>{label}</Button> }
                    }
                })
                .collect();
            View::Fragment(buttons)
        }
    };
    let body = {
        let l = l.clone();
        move || -> View {
            match l.tab.get() {
                DevTab::Frame | DevTab::Profiler => readouts_tab(&l),
                DevTab::View => view_tab(&l),
                DevTab::Switches => switches_tab(&l),
                DevTab::Precision => precision_tab(&l),
                #[cfg(not(target_arch = "wasm32"))]
                DevTab::Midas => midas_tab(&l),
                #[cfg(target_arch = "wasm32")]
                DevTab::Midas => note_tab(&l),
            }
        }
    };
    // The sheet's own node listens (a click that does nothing), so the
    // pointer over its padding is the library's too.
    view! {
        <div style={frame} on:click={|_: &Event| {}}>
            <div style="display: flex; align-items: center; gap: 12px; padding: 12px 12px 8px 16px">
                <span style="color: var(--m-accent)">{icon(icons::TUNE)}</span>
                <h1 style={title}>"Developer tools"</h1>
                <CloseButton onclick={l.act(DevAct::Close)} />
            </div>
            <div style="display: flex; flex-wrap: wrap; gap: 4px; padding: 0 12px 10px; border-bottom: 1px solid var(--m-border)">
                {tabs}
            </div>
            <div style="flex: 1; min-height: 0; overflow-y: auto; padding: 14px 16px 18px; display: flex; flex-direction: column; gap: 16px">
                {body}
            </div>
        </div>
    }
}

/// A readout value's colour.
fn tone_css(tone: DevTone) -> &'static str {
    match tone {
        DevTone::Value => "var(--m-text)",
        DevTone::Dim => "var(--m-text-dim)",
        DevTone::Error => "var(--m-danger)",
    }
}

/// A quiet line, as `theme::note`.
fn note(text: impl Into<String>) -> View {
    let text: String = text.into();
    view! {
        <p style="margin: 0; color: var(--m-text-dim); font-size: var(--m-font-size-sm); font-style: italic; line-height: 1.4">{text}</p>
    }
}

/// One readout row: the name in a fixed column, the value monospaced (egui's
/// `stat`). A name led by two spaces is a sub-row, indented.
fn row_view(r: &DevRow) -> View {
    match r {
        DevRow::Stat { name, value, tone } => {
            let (indent, name) = match name.strip_prefix("  ") {
                Some(rest) => (12.0, rest.to_string()),
                None => (0.0, name.clone()),
            };
            let name_style = format!(
                "flex: none; box-sizing: border-box; width: {NAME_COLUMN}px; padding-left: {indent}px; \
                 color: var(--m-text-dim); font-size: {}px",
                theme::SIZE_SUB
            );
            let value_style = format!(
                "flex: 1; min-width: 0; font-family: var(--m-font-mono); font-size: {}px; color: {}",
                theme::SIZE_MONO,
                tone_css(*tone)
            );
            let value = value.clone();
            view! {
                <div style="display: flex; align-items: baseline; gap: 8px; min-height: 20px">
                    <span style={name_style}>{name}</span>
                    <span style={value_style}>{value}</span>
                </div>
            }
        }
        DevRow::Mono(text, tone) => {
            let style = format!(
                "font-family: var(--m-font-mono); font-size: {}px; color: {}",
                theme::SIZE_TINY,
                tone_css(*tone)
            );
            let text = text.clone();
            view! { <span style={style}>{text}</span> }
        }
        DevRow::Note(text) => note(text.clone()),
    }
}

/// Readout sections, each a library `Section` of rows.
fn sections_view(sections: &[DevSection]) -> View {
    let views: Vec<View> = sections
        .iter()
        .map(|s| {
            let rows: Vec<View> = s.rows.iter().map(row_view).collect();
            let title = s.title;
            view! {
                <Section title={title}>
                    <div style="display: flex; flex-direction: column; gap: 2px">{rows}</div>
                </Section>
            }
        })
        .collect();
    View::Fragment(views)
}

/// Frame and Profiler: readouts, following the view.
fn readouts_tab(l: &Live) -> View {
    let view = l.view.clone();
    (move || -> View {
        match &view.get().body {
            DevBody::Readouts(sections) => sections_view(sections),
            _ => View::Empty,
        }
    })
    .into_view()
}

/// View: the camera and the avatar, then the shot preset's group and Copy.
fn view_tab(l: &Live) -> View {
    let readouts = {
        let view = l.view.clone();
        move || -> View {
            match &view.get().body {
                DevBody::View { sections, .. } => sections_view(sections),
                _ => View::Empty,
            }
        }
    };
    let copy = {
        let l = l.clone();
        move || -> View {
            let (can_copy, copied) = match &l.view.get().body {
                DevBody::View {
                    can_copy, copied, ..
                } => (*can_copy, *copied),
                _ => (false, false),
            };
            let copied: View = if copied {
                view! { <span style="color: var(--m-text-dim); font-size: var(--m-font-size-sm)">"copied"</span> }
            } else {
                View::Empty
            };
            view! {
                <div style="display: flex; align-items: center; gap: 10px">
                    <Button size=Size::Sm icon={icon(icons::COPY)} disabled={!can_copy} title={COPY_HELP} onclick={l.act(DevAct::CopyPreset)}>"Copy as preset"</Button>
                    {copied}
                </div>
            }
        }
    };
    // egui's hover text is the field's help here: a phone has no hover.
    view! {
        {readouts}
        <Section title="Shot presets">
            <div style="display: flex; flex-direction: column; gap: 10px">
                <FieldText model={l.group.clone()} label="group" placeholder="blank = README set" autocomplete="off" help={GROUP_HELP} id="dev-shot-group" />
                {copy}
            </div>
        </Section>
        {note(CLOCK_NOTE)}
    }
}

/// Switches: how many are on, then each as a switch with what it does.
fn switches_tab(l: &Live) -> View {
    let l = l.clone();
    (move || -> View {
        let DevBody::Switches { on, rows } = l.view.get().body else {
            return View::Empty;
        };
        // The count, at the top: these are read once at startup.
        let head: View = if on == 0 {
            note(SWITCHES_NOTE)
        } else {
            let style = format!(
                "margin: 0; color: var(--m-accent); font-size: {}px; line-height: 1.4",
                theme::SIZE_SUB
            );
            let line = switches_on(on);
            view! { <p style={style}>{line}</p> }
        };
        let switches: Vec<View> = rows.iter().map(|r| switch_row(&l, r)).collect();
        view! {
            {head}
            <div style="display: flex; flex-direction: column; gap: 12px">{switches}</div>
        }
    })
    .into_view()
}

/// One `CARTO_*` switch.
fn switch_row(l: &Live, r: &SwitchRow) -> View {
    let name = r.name;
    let change: Rc<dyn Fn(&bool)> = {
        let acts = l.acts.clone();
        Rc::new(move |v: &bool| acts.borrow_mut().push(DevAct::Switch(name, *v)))
    };
    let id = format!("dev-{}", name.to_lowercase().replace('_', "-"));
    let (label, help) = (r.label, r.help);
    view! {
        <FieldSwitch model={signal(r.on)} label={label} help={help} onchange={Some(change)} id={id} />
    }
}

/// Precision: the indoor walk test, the stand-in hand, and the log.
fn precision_tab(l: &Live) -> View {
    let walk = {
        let l = l.clone();
        move || -> View {
            match l.view.get().body {
                DevBody::Precision(p) => walk_test(&l, &p),
                _ => View::Empty,
            }
        }
    };
    let stand_in = {
        let l = l.clone();
        move || -> View {
            let DevBody::Precision(p) = l.view.get().body else {
                return View::Empty;
            };
            let change: Rc<dyn Fn(&bool)> = {
                let acts = l.acts.clone();
                Rc::new(move |v: &bool| acts.borrow_mut().push(DevAct::StandIn(*v)))
            };
            view! {
                <FieldSwitch model={signal(p.stand_in)} label="Stand-in hand" help={STAND_IN_NOTE} onchange={Some(change)} id="dev-stand-in" />
            }
        }
    };
    let log = {
        let l = l.clone();
        move || -> View {
            // Whether there is a log, not what it says: the text follows in
            // the scroller below without rebuilding the buttons.
            if l.log.get().is_none() {
                return note(LOG_EMPTY_NOTE);
            }
            log_view(&l)
        }
    };
    view! {
        <Section title="Indoor walk test">
            <div style="display: flex; flex-direction: column; gap: 8px">{walk}</div>
        </Section>
        <div style="border-top: 1px solid var(--m-border); padding-top: 14px">{stand_in}</div>
        <div style="border-top: 1px solid var(--m-border); padding-top: 14px; display: flex; flex-direction: column; gap: 8px">{log}</div>
    }
}

/// The walk test: the steps and Start before it runs; what the tracking
/// says, Round and Stop while it does.
fn walk_test(l: &Live, p: &PrecisionView) -> View {
    let Some(word) = p.word else {
        let steps: Vec<View> = WALK_STEPS
            .iter()
            .map(|&s| {
                view! { <p style="margin: 0; color: var(--m-text); line-height: 1.4">{s}</p> }
            })
            .collect();
        // Drawn on a desktop too, greyed, so a capture shows the test as a
        // phone does.
        let unavailable: View = if p.available {
            View::Empty
        } else {
            note(NEEDS_A_PHONE)
        };
        return view! {
            {steps}
            <div style="padding-top: 4px">
                <Button variant=Variant::Primary disabled={!p.available} onclick={l.act(DevAct::Start)}>"Start"</Button>
            </div>
            {unavailable}
            {note(WALK_NOTE)}
        };
    };
    let tone = match word {
        HandWord::Following => "var(--m-accent)",
        HandWord::CameraBusy => "var(--m-warning)",
        _ => "var(--m-text-dim)",
    };
    let word_style = format!(
        "margin: 0; font-size: {}px; font-weight: 600; color: {tone}",
        theme::SIZE_TITLE
    );
    let text = word.text();
    let problem: View = match &p.problem {
        Some(problem) => {
            let problem = problem.clone();
            view! { <p style="margin: 0; color: var(--m-warning); line-height: 1.4">{problem}</p> }
        }
        None => View::Empty,
    };
    let running: View = match &p.running {
        Some(r) => note(r.clone()),
        None => View::Empty,
    };
    let next = format!("Round {}", p.rounds + 1);
    view! {
        <p style={word_style}>{text}</p>
        {problem}
        {running}
        <div style="display: flex; flex-wrap: wrap; gap: 8px; padding-top: 4px">
            <Button variant=Variant::Primary onclick={l.act(DevAct::Round)}>{next}</Button>
            <Button variant=Variant::Secondary onclick={l.act(DevAct::Stop)}>"Stop"</Button>
        </div>
    }
}

/// The log: Copy and Clear, and its tail, following the newest line.
fn log_view(l: &Live) -> View {
    let text = {
        let log = l.log.clone();
        move || -> View {
            let style = format!(
                "margin: 0; font-family: var(--m-font-mono); font-size: {}px; color: var(--m-text); white-space: pre-wrap",
                theme::SIZE_CHIP
            );
            let text = log.get().unwrap_or_default();
            view! { <p style={style}>{text}</p> }
        }
    };
    let at_end = {
        let log = l.log.clone();
        move || {
            log.get();
            true
        }
    };
    let scroller = mithril::view::el("div")
        .attr(
            "style",
            "max-height: 320px; overflow-y: auto; display: flex; flex-direction: column",
        )
        .prop_bool("@scroll-end", at_end)
        .child(text)
        .into_view();
    view! {
        <div style="display: flex; gap: 8px">
            <Button size=Size::Sm icon={icon(icons::COPY)} onclick={l.act(DevAct::CopyLog)}>"Copy"</Button>
            <Button size=Size::Sm variant=Variant::Ghost onclick={l.act(DevAct::ClearLog)}>"Clear"</Button>
        </div>
        {scroller}
    }
}

/// A tab that is one line (Midas in the browser).
#[cfg(target_arch = "wasm32")]
fn note_tab(l: &Live) -> View {
    let view = l.view.clone();
    (move || -> View {
        match view.get().body {
            DevBody::Note(text) => note(text),
            _ => View::Empty,
        }
    })
    .into_view()
}

// ─── Midas ───────────────────────────────────────────────────────────────────

/// A tap on the Midas tab, made when it is tapped (so it carries the fields'
/// words as they are then).
#[cfg(not(target_arch = "wasm32"))]
fn midas_tap(l: &Live, make: impl Fn() -> MidasAct + 'static) -> Option<Handler> {
    let acts = l.acts.clone();
    Some(Rc::new(move |_: &Event| acts.borrow_mut().push(DevAct::Midas(make()))) as Handler)
}

/// Midas in the apps: pairing until paired, then the link, Send to a
/// session, the release key and Unpair, as egui's `midas_link::midas_tab`.
#[cfg(not(target_arch = "wasm32"))]
fn midas_tab(l: &Live) -> View {
    let l = l.clone();
    (move || -> View {
        if l.midas.paired.get() {
            paired_view(&l)
        } else {
            pair_view(&l)
        }
    })
    .into_view()
}

/// The answer to the last Pair, Send or Refresh, following the view.
#[cfg(not(target_arch = "wasm32"))]
fn midas_message(l: &Live) -> View {
    let view = l.view.clone();
    (move || -> View {
        match view.get().body {
            DevBody::Midas(m) => match m.message {
                Some(text) => note(text),
                None => View::Empty,
            },
            _ => View::Empty,
        }
    })
    .into_view()
}

/// Not paired: what pairing does, the hub and the code, and Pair.
#[cfg(not(target_arch = "wasm32"))]
fn pair_view(l: &Live) -> View {
    let (hub, code) = match l.midas.latest.borrow().as_ref() {
        Some(m) => (m.hub_draft.clone(), m.code_draft.clone()),
        None => (
            crate::systems::midas_link::DEFAULT_HUB.to_string(),
            String::new(),
        ),
    };
    let (hub, code) = (signal(hub), signal(code));
    let pair = {
        let (hub, code) = (hub.clone(), code.clone());
        midas_tap(l, move || MidasAct::Pair {
            hub: hub.get_untracked(),
            code: code.get_untracked(),
        })
    };
    view! {
        <Section title="Midas">
            <div style="display: flex; flex-direction: column; gap: 10px">
                {note(PAIR_NOTE)}
                {note(PAIR_HOW)}
                <FieldText model={hub} label="hub" autocomplete="off" id="midas-hub" />
                <FieldText model={code} label="code" autocomplete="off" id="midas-code" />
                <div style="display: flex">
                    <Button variant=Variant::Primary onclick={pair}>"Pair"</Button>
                </div>
                {midas_message(l)}
            </div>
        </Section>
    }
}

/// Paired: the link and Send to a session, then Release key and Unpair.
#[cfg(not(target_arch = "wasm32"))]
fn paired_view(l: &Live) -> View {
    let status = {
        let l = l.clone();
        move || -> View {
            let DevBody::Midas(m) = l.view.get().body else {
                return View::Empty;
            };
            link_status(&l, &m)
        }
    };
    let send_to = {
        let l = l.clone();
        move || -> View {
            let DevBody::Midas(m) = l.view.get().body else {
                return View::Empty;
            };
            send_to_row(&l, &m)
        }
    };
    let send = |make: fn(String) -> MidasAct| -> Option<Handler> {
        let words = l.midas.words.clone();
        midas_tap(l, move || make(words.get_untracked()))
    };
    // Where releases are approved (the owner's Android phone): its own
    // section, its taps done there.
    let key: View = if crate::systems::release_approval::key_view().is_some() {
        let section = crate::systems::lib_release_approval::key_section_view();
        view! {
            <div style="border-top: 1px solid var(--m-border)">{section}</div>
        }
    } else {
        View::Empty
    };
    view! {
        <Section title="Midas">
            <div style="display: flex; flex-direction: column; gap: 8px">{status}</div>
        </Section>
        <Section title="Send to a session">
            <div style="display: flex; flex-direction: column; gap: 10px">
                {send_to}
                <FieldTextarea model={l.midas.words.clone()} placeholder={WORDS_HINT} rows={2u32} id="midas-words" />
                <div style="display: flex; flex-wrap: wrap; gap: 8px">
                    <Button size=Size::Sm onclick={send(MidasAct::SendLog)}>"Precision log"</Button>
                    <Button size=Size::Sm onclick={send(MidasAct::SendShot)}>"Screenshot"</Button>
                    <Button size=Size::Sm onclick={send(MidasAct::SendWords)}>"Words only"</Button>
                </div>
                {midas_message(l)}
            </div>
        </Section>
        {key}
        <div style="border-top: 1px solid var(--m-border); padding-top: 14px; display: flex">
            <Button size=Size::Sm onclick={l.act(DevAct::Midas(MidasAct::Unpair))}>"Unpair"</Button>
        </div>
    }
}

/// Which hub, as whom; Link on; what the link says; who asked last.
#[cfg(not(target_arch = "wasm32"))]
fn link_status(l: &Live, m: &MidasTabView) -> View {
    let change: Rc<dyn Fn(&bool)> = {
        let acts = l.acts.clone();
        Rc::new(move |v: &bool| acts.borrow_mut().push(DevAct::Midas(MidasAct::LinkOn(*v))))
    };
    let who = m.who.clone();
    let state: View = if m.state.is_empty() {
        View::Empty
    } else {
        note(m.state.clone())
    };
    let recent: View = match &m.recent {
        Some(r) => note(r.clone()),
        None => View::Empty,
    };
    view! {
        <p style="margin: 0; color: var(--m-text); line-height: 1.4; overflow-wrap: anywhere">{who}</p>
        <FieldSwitch model={signal(m.on)} label="Link on" onchange={Some(change)} id="midas-on" />
        {state}
        {recent}
    }
}

/// The session Send goes to, and Refresh.
#[cfg(not(target_arch = "wasm32"))]
fn send_to_row(l: &Live, m: &MidasTabView) -> View {
    let options: Vec<Opt<String>> = m
        .sessions
        .iter()
        .map(|(id, label)| Opt::new(id.clone(), label.clone()))
        .collect();
    let change: Rc<dyn Fn(&Option<String>)> = {
        let acts = l.acts.clone();
        Rc::new(move |v: &Option<String>| {
            acts.borrow_mut()
                .push(DevAct::Midas(MidasAct::SendTo(v.clone())))
        })
    };
    let select = mithril::view::render(FieldSelect {
        model: signal(m.send_to.clone()),
        options,
        placeholder: Some("choose…".to_owned()),
        id: Some("midas-send-to".to_owned()),
        onchange: Some(change),
        ..Default::default()
    });
    view! {
        <div style="display: flex; align-items: center; gap: 8px">
            <div style="flex: 1; min-width: 0">{select}</div>
            <Button size=Size::Sm onclick={l.act(DevAct::Midas(MidasAct::Refresh))}>"Refresh"</Button>
        </div>
    }
}
