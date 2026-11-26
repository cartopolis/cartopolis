//! The avatar designer (the Appearance panel) drawn by the component library
//! (`lib_host`): the starters and "Surprise me", the Body, Face, Skin & hair
//! and Clothes tabs, Undo, Redo, Back to saved, the view of the model, and
//! Apply.
//!
//! `ui::drive_avatar_chooser` seeds the draft, dresses the avatar in it
//! (`HumanDraft`) and aims the camera, which it takes from
//! `AvatarChooserState::lib_close_up` and `lib_turn`; `panel_slots` keeps
//! the panel's room in the map's layout — a sheet over the bottom half on a
//! phone. Every change goes through the `DesignerState` methods of
//! `avatar_designer`.
//!
//! Drawn differently, because the library has no widget for them: a colour
//! is picked from the palette only, and a skin, an eye colour or a colour is a round swatch
//! button rather than a painted disc.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use bevy::ecs::system::NonSendMarker;
use bevy::prelude::*;
// The library's, not Bevy's: an explicit import outranks the two globs.
use mithril::prelude::*;
use mithril::prelude::{Button, Event};

use crate::resources::{AvatarChooserState, RightPanel};
use crate::systems::avatar_designer::{
    self as designer, BodyPart, DesignerState, DesignerTab, DesignerView, FacePart, FaceSlider,
    Macro,
};
use crate::systems::lib_settings::LibScreens;
use crate::systems::theme;
use crate::systems::ui_input::UiMetrics;
use crate::systems::user_store::{HumanLook, UserStore};

/// The key this screen mounts under.
const KEY: &str = "avatar";

/// A slider of the look: one of the body's whole-body sliders, a face or
/// body-part control (by its MakeHuman modifier), or the skin's tone.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Key {
    Macro(Macro),
    Shape(&'static str),
    Tone,
}

impl Key {
    fn read(self, look: &HumanLook) -> f64 {
        match self {
            Key::Macro(m) => f64::from(m.get(look)),
            Key::Shape(modifier) => f64::from(look.face_value(modifier)),
            Key::Tone => f64::from(look.skin_tone),
        }
    }

    fn write(self, look: &mut HumanLook, v: f64) {
        let v = v.round();
        match self {
            Key::Macro(m) => *m.get_mut(look) = v.clamp(0.0, 100.0) as u8,
            Key::Shape(modifier) => look.set_face(modifier, v.clamp(-100.0, 100.0) as i8),
            Key::Tone => look.skin_tone = v.clamp(-100.0, 100.0) as i8,
        }
    }

    /// The control's id, for its label.
    fn id(self) -> String {
        match self {
            Key::Macro(m) => format!("av-{m:?}").to_lowercase(),
            Key::Shape(modifier) => format!("av-{}", modifier.replace('/', "-")),
            Key::Tone => "av-tone".to_owned(),
        }
    }
}

/// One slider's model, made once at the mount so a drag is never re-rendered
/// from under the pointer, and the value last bridged, to tell a drag from a
/// change made elsewhere (an undo, a starter).
struct Slider {
    key: Key,
    model: Signal<f64>,
    last: Cell<f64>,
}

/// Every slider the panel can show.
fn sliders(look: &HumanLook) -> Vec<Slider> {
    let mut keys: Vec<Key> = designer::MACROS
        .iter()
        .map(|m| Key::Macro(m.which))
        .collect();
    for (_, list) in designer::FACE.iter() {
        keys.extend(list.iter().map(|s| Key::Shape(s.0)));
    }
    for (_, list) in designer::BODY.iter() {
        keys.extend(list.iter().map(|s| Key::Shape(s.0)));
    }
    keys.push(Key::Tone);
    keys.into_iter()
        .map(|key| {
            let v = key.read(look);
            Slider {
                key,
                model: signal(v),
                last: Cell::new(v),
            }
        })
        .collect()
}

/// The quick-pick rows of the Body tab.
#[derive(Clone, Copy)]
enum PickRow {
    Shape,
    Age,
    Build,
    Height,
}

/// Each pick's label and whether the look holds its values.
fn picks(look: &HumanLook, row: PickRow) -> Vec<(&'static str, bool)> {
    match row {
        PickRow::Shape => designer::SHAPE_PICKS
            .iter()
            .map(|(l, [v])| (*l, look.body_shape == *v))
            .collect(),
        PickRow::Age => designer::AGE_PICKS
            .iter()
            .map(|(l, [v])| (*l, look.age == *v))
            .collect(),
        PickRow::Build => designer::BUILD_PICKS
            .iter()
            .map(|(l, [w, m])| (*l, look.weight == *w && look.muscle == *m))
            .collect(),
        PickRow::Height => designer::HEIGHT_PICKS
            .iter()
            .map(|(l, [v])| (*l, look.height == *v))
            .collect(),
    }
}

fn pick(look: &mut HumanLook, row: PickRow, i: usize) {
    match row {
        PickRow::Shape => {
            if let Some((_, [v])) = designer::SHAPE_PICKS.get(i) {
                look.body_shape = *v;
            }
        }
        PickRow::Age => {
            if let Some((_, [v])) = designer::AGE_PICKS.get(i) {
                look.age = *v;
            }
        }
        PickRow::Build => {
            if let Some((_, [w, m])) = designer::BUILD_PICKS.get(i) {
                look.weight = *w;
                look.muscle = *m;
            }
        }
        PickRow::Height => {
            if let Some((_, [v])) = designer::HEIGHT_PICKS.get(i) {
                look.height = *v;
            }
        }
    }
}

/// The lists worn as extras, at most one of each.
#[derive(Clone, Copy)]
enum Extra {
    Beard,
    Glasses,
    Top,
    Bottom,
}

impl Extra {
    fn list(self) -> &'static [(&'static str, &'static str)] {
        match self {
            Extra::Beard => &designer::BEARDS,
            Extra::Glasses => &designer::GLASSES,
            Extra::Top => &designer::TOPS,
            Extra::Bottom => &designer::BOTTOMS,
        }
    }
}

/// The colours a look can leave as they are.
#[derive(Clone, Copy)]
enum Tint {
    Hair,
    Top,
    Bottom,
}

impl Tint {
    fn get(self, look: &HumanLook) -> Option<[u8; 3]> {
        match self {
            Tint::Hair => look.hair_colour,
            Tint::Top => look.top_colour,
            Tint::Bottom => look.bottom_colour,
        }
    }

    fn set(self, look: &mut HumanLook, c: Option<[u8; 3]>) {
        match self {
            Tint::Hair => look.hair_colour = c,
            Tint::Top => look.top_colour = c,
            Tint::Bottom => look.bottom_colour = c,
        }
    }
}

/// What a tap asked for; done by the system, on the draft as it is then.
#[derive(Clone, Copy)]
enum Act {
    Undo,
    Redo,
    BackToSaved,
    Starter(usize),
    Surprise,
    Pick(PickRow, usize),
    AnotherFace,
    SimilarFace,
    AverageFace,
    AverageBodyParts,
    Skin(&'static str),
    Eyes(&'static str),
    Hair(Option<&'static str>),
    Brows(&'static str),
    Extra(Extra, Option<&'static str>),
    Colour(Tint, Option<[u8; 3]>),
    Separates,
    Outfit(&'static str),
    Shoes(&'static str),
    Hat(Option<&'static str>),
    Apply,
    Close,
}

/// What the panel shows outside the look.
#[derive(Clone, Copy, PartialEq, Default)]
struct Status {
    can_undo: bool,
    can_redo: bool,
    /// The draft differs from the saved look: "Back to saved" is offered.
    saved_differs: bool,
    /// The draft differs from the saved appearance: Apply is live.
    changed: bool,
}

#[derive(Clone)]
struct Live {
    look: Signal<HumanLook>,
    status: Signal<Status>,
    tab: Signal<DesignerTab>,
    view: Signal<DesignerView>,
    face: Signal<FacePart>,
    body: Signal<BodyPart>,
    fine_tune: Signal<bool>,
    body_parts: Signal<bool>,
    sliders: Rc<Vec<Slider>>,
    acts: Rc<RefCell<Vec<Act>>>,
    /// A sheet over the bottom half, for a phone.
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

/// Mounts, bridges and unmounts the avatar designer. `PostUpdate`, before
/// `DriveUi`, as every library screen.
#[allow(clippy::too_many_arguments)]
pub fn drive_lib_avatar(
    _main: NonSendMarker,
    lib: Res<LibScreens>,
    mut right_panel: ResMut<RightPanel>,
    mut chooser: ResMut<AvatarChooserState>,
    mut store: ResMut<UserStore>,
    metrics: Res<UiMetrics>,
    time: Res<Time>,
    mouse: Option<Res<ButtonInput<MouseButton>>>,
    touches: Option<Res<Touches>>,
    // The designer's tab, view and undo history.
    mut state: Local<DesignerState>,
    mut opened: Local<bool>,
) {
    if !lib.avatar {
        return;
    }
    if lib.open_avatar && !*opened {
        *opened = true;
        *right_panel = RightPanel::Appearance;
    }
    // A tab asked for at startup (`--ui appearance,designer-face`).
    if let Some(tab) = chooser.open_tab {
        chooser.open_tab = None;
        state.tab = tab;
    }
    // `ui::drive_avatar_chooser` seeds the draft in `Update`, before this.
    let open =
        right_panel.is(RightPanel::Appearance) && chooser.seeded && chooser.draft.human.is_some();
    // A drag is one undo step, recorded when the pointer lets go.
    let dragging = mouse.is_some_and(|m| m.pressed(MouseButton::Left))
        || touches.is_some_and(|t| t.iter().next().is_some());
    let now = time.elapsed_secs_f64();
    MOUNTED.with(|m| {
        let mut m = m.borrow_mut();
        match (open, m.as_mut()) {
            (false, None) => {}
            (false, Some(_)) => {
                *m = None;
                crate::systems::lib_host::clear_keyed(KEY);
            }
            (true, None) => {
                // `open` holds only with a look in the draft.
                let look = chooser
                    .draft
                    .human
                    .clone()
                    .unwrap_or_else(|| HumanLook::default_for(true));
                let live = Live {
                    status: signal(status_of(&state, &chooser, &store, &look)),
                    tab: signal(state.tab),
                    view: signal(state.view),
                    face: signal(state.face),
                    body: signal(state.body),
                    fine_tune: signal(false),
                    body_parts: signal(false),
                    sliders: Rc::new(sliders(&look)),
                    look: signal(look),
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
                let Some(before) = chooser.draft.human.clone() else {
                    return;
                };
                let mut look = before.clone();
                // A slider moved since the last frame → the look.
                for s in live.sliders.iter() {
                    let v = s.model.get_untracked();
                    if v != s.last.get() {
                        s.key.write(&mut look, v);
                        s.last.set(v);
                    }
                }
                // The taps of the frame.
                let saved = store.data.appearance.human.clone();
                let acts: Vec<Act> = std::mem::take(&mut *live.acts.borrow_mut());
                let (mut apply, mut close) = (false, false);
                for a in acts {
                    match a {
                        Act::Apply => apply = true,
                        Act::Close => close = true,
                        a => act(a, &mut look, &mut state, saved.as_ref(), now),
                    }
                }
                // The tab, the view and the parts → the designer.
                let tab = live.tab.get_untracked();
                if state.tab != tab {
                    state.tab = tab;
                }
                let view = live.view.get_untracked();
                if state.view != view {
                    state.view = view;
                }
                let face = live.face.get_untracked();
                if state.face != face {
                    state.face = face;
                }
                let body = live.body.get_untracked();
                if state.body != body {
                    state.body = body;
                }
                state.record(&look, dragging);
                if look != before {
                    chooser.draft.human = Some(look.clone());
                }
                if apply {
                    designer::apply_draft(&chooser, &mut store);
                }
                if close {
                    *right_panel = RightPanel::None;
                    chooser.seeded = false;
                }
                mirror(live, &look, &state, &chooser, &store);
            }
        }
    });
    // Where the camera looks, for `ui::drive_avatar_chooser` to aim by.
    let close_up = state.close_up();
    if chooser.lib_close_up != close_up {
        chooser.lib_close_up = close_up;
    }
    let turn = state.view.turn();
    if chooser.lib_turn != turn {
        chooser.lib_turn = turn;
    }
}

fn status_of(
    state: &DesignerState,
    chooser: &AvatarChooserState,
    store: &UserStore,
    look: &HumanLook,
) -> Status {
    Status {
        can_undo: state.can_undo(),
        can_redo: state.can_redo(),
        saved_differs: store
            .data
            .appearance
            .human
            .as_ref()
            .is_some_and(|s| s != look),
        changed: chooser.draft != store.data.appearance,
    }
}

/// The look, the sliders, the tabs and the status → the panel's signals,
/// where they moved.
fn mirror(
    live: &Live,
    look: &HumanLook,
    state: &DesignerState,
    chooser: &AvatarChooserState,
    store: &UserStore,
) {
    if live.look.peek(|l| l != look) {
        live.look.set(look.clone());
    }
    for s in live.sliders.iter() {
        let v = s.key.read(look);
        if s.model.get_untracked() != v {
            s.model.set(v);
        }
        s.last.set(v);
    }
    if live.tab.get_untracked() != state.tab {
        live.tab.set(state.tab);
    }
    if live.view.get_untracked() != state.view {
        live.view.set(state.view);
    }
    let status = status_of(state, chooser, store, look);
    if live.status.get_untracked() != status {
        live.status.set(status);
    }
}

/// One tap, done through the designer's own functions.
fn act(
    a: Act,
    look: &mut HumanLook,
    state: &mut DesignerState,
    saved: Option<&HumanLook>,
    now: f64,
) {
    match a {
        Act::Undo => state.undo(look),
        Act::Redo => state.redo(look),
        Act::BackToSaved => {
            if let Some(saved) = saved {
                state.back_to_saved(look, saved);
            }
        }
        Act::Starter(i) => {
            if let Some(s) = designer::STARTERS.get(i) {
                *look = s.look();
            }
        }
        Act::Surprise => state.surprise(look, now),
        Act::Pick(row, i) => pick(look, row, i),
        Act::AnotherFace => state.another_face(look, now),
        Act::SimilarFace => state.similar_face(look, now),
        Act::AverageFace => designer::clear_face(look),
        Act::AverageBodyParts => designer::average_body_parts(look),
        Act::Skin(name) => look.skin = name.into(),
        Act::Eyes(name) => look.eyes = name.into(),
        Act::Hair(name) => look.hair = name.map(Into::into),
        Act::Brows(name) => look.eyebrows = name.into(),
        Act::Extra(kind, name) => designer::set_extra(look, kind.list(), name),
        Act::Colour(tint, c) => tint.set(look, c),
        Act::Separates => {
            if !look.outfit.is_empty() {
                designer::wear_separates(look);
            }
        }
        Act::Outfit(name) => designer::wear_outfit(look, name),
        Act::Shoes(name) => look.shoes = name.into(),
        Act::Hat(name) => look.hat = name.map(Into::into),
        // The system's own.
        Act::Apply | Act::Close => {}
    }
}

/// `CARTO_UI_LIB_HTML=<path>`: the panel as HTML, for the browser beside a
/// capture.
fn write_html(l: &Live) {
    if let Some(path) = std::env::var_os("CARTO_UI_LIB_HTML") {
        if let Err(e) = std::fs::write(&path, html(l)) {
            warn!(?path, error = %e, "lib avatar: could not write the HTML");
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

fn note(text: &'static str) -> View {
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

/// A heading over its controls.
fn block(title: &'static str, body: View) -> View {
    view! {
        <div style="display: flex; flex-direction: column; gap: 10px">
            {group(title)}
            {body}
        </div>
    }
}

/// A run of chips (or swatches) that wraps.
fn wrap(items: Vec<View>) -> View {
    view! { <div style="display: flex; flex-wrap: wrap; align-items: center; gap: 8px">{items}</div> }
}

/// A chip that picks: pressed when `on`; a tap does `a`. The row it is in
/// is drawn again when the look moves, so a tap that changes nothing (the
/// chosen one again) only has to stay pressed.
fn chip(l: &Live, label: &str, on: bool, a: Act) -> View {
    let model = signal(on);
    let (acts, pressed) = (l.acts.clone(), model.clone());
    let change: Rc<dyn Fn(&bool)> = Rc::new(move |_: &bool| {
        pressed.set(true);
        acts.borrow_mut().push(a);
    });
    let label = label.to_owned();
    view! { <Chip model={model} onchange={Some(change)}>{label}</Chip> }
}

/// A row of chips over the look, drawn again whenever it moves.
fn chips(l: &Live, row: impl Fn(&HumanLook) -> Vec<(&'static str, bool, Act)> + 'static) -> View {
    let l = l.clone();
    let draw = move || -> View {
        let look = l.look.get();
        let items: Vec<View> = row(&look)
            .into_iter()
            .map(|(label, on, a)| chip(&l, label, on, a))
            .collect();
        wrap(items)
    };
    draw.into_view()
}

/// A row of chips for one of `list` as a plain choice of the panel (a face
/// part, a body part), not of the look.
fn part_chips<K: Copy + PartialEq + 'static>(
    model: &Signal<K>,
    list: &'static [(K, &'static str)],
) -> View {
    let model = model.clone();
    let draw = move || -> View {
        let current = model.get();
        let items: Vec<View> = list
            .iter()
            .map(|(k, label)| {
                let k = *k;
                let on = signal(current == k);
                let choose = model.clone();
                let change: Rc<dyn Fn(&bool)> = Rc::new(move |_: &bool| choose.set(k));
                let label = (*label).to_owned();
                view! { <Chip model={on} onchange={Some(change)}>{label}</Chip> }
            })
            .collect();
        wrap(items)
    };
    draw.into_view()
}

/// A round colour button. The colours are the look's, not the interface's,
/// so they are data here rather than tokens.
fn swatch(l: &Live, c: [u8; 3], chosen: bool, title: &str, a: Act) -> View {
    let style = format!(
        "flex: none; width: 36px; height: 36px; margin: 0; padding: 0; border-radius: 50%; cursor: pointer; background: rgb({}, {}, {}); border: {}",
        c[0],
        c[1],
        c[2],
        if chosen {
            "3px solid var(--m-accent)"
        } else {
            "1px solid var(--m-border)"
        },
    );
    let acts = l.acts.clone();
    let click = move |_: &Event| acts.borrow_mut().push(a);
    let title = title.to_owned();
    view! { <button title={title} style={style} on:click={click}></button> }
}

/// "As it is" and the palette for one of the look's colours.
fn colours(l: &Live, what: &'static str, palette: &'static [[u8; 3]], tint: Tint) -> View {
    let l = l.clone();
    let draw = move || -> View {
        let look = l.look.get();
        let value = tint.get(&look);
        let mut items = vec![chip(
            &l,
            "As it is",
            value.is_none(),
            Act::Colour(tint, None),
        )];
        for c in palette {
            items.push(swatch(
                &l,
                *c,
                value == Some(*c),
                what,
                Act::Colour(tint, Some(*c)),
            ));
        }
        wrap(items)
    };
    draw.into_view()
}

/// The swatches for a choice by name (skin, eyes).
fn named_swatches(
    l: &Live,
    list: &'static [(&'static str, [u8; 3])],
    chosen: fn(&HumanLook) -> &str,
    act: fn(&'static str) -> Act,
) -> View {
    let l = l.clone();
    let draw = move || -> View {
        let look = l.look.get();
        let current = chosen(&look).to_owned();
        let items: Vec<View> = list
            .iter()
            .map(|(name, c)| swatch(&l, *c, current == *name, name, act(*name)))
            .collect();
        wrap(items)
    };
    draw.into_view()
}

/// One slider: its label, the word for its value beside the track, and what
/// its two ends mean under it.
fn slider(
    l: &Live,
    key: Key,
    label: &'static str,
    range: (f64, f64),
    says: Rc<dyn Fn(f64) -> String>,
    ends: (&'static str, &'static str),
) -> View {
    let Some(s) = l.sliders.iter().find(|s| s.key == key) else {
        return View::Empty;
    };
    let help = view! {
        <span style="display: flex; justify-content: space-between; gap: 12px"><span>{ends.0}</span><span>{ends.1}</span></span>
    };
    view! {
        <FieldSlider model={s.model.clone()} min={range.0} max={range.1} step=1.0 format={Some(says)} label={label} help={help} id={key.id()} />
    }
}

/// A part's face or body sliders.
fn shape_sliders(l: &Live, list: &'static [FaceSlider]) -> View {
    let items: Vec<View> = list
        .iter()
        .map(|FaceSlider(modifier, label, ends, one_sided)| {
            let ends = *ends;
            let says: Rc<dyn Fn(f64) -> String> =
                Rc::new(move |v: f64| designer::shape_says(v.round() as i32, ends));
            let lo = if *one_sided { 0.0 } else { -100.0 };
            let shown = if *one_sided { ("", ends.1) } else { ends };
            slider(l, Key::Shape(*modifier), *label, (lo, 100.0), says, shown)
        })
        .collect();
    view! { <div style="display: flex; flex-direction: column; gap: 16px">{items}</div> }
}

fn body_tab(l: &Live) -> View {
    let row = |title: &'static str, which: PickRow| -> View {
        block(
            title,
            chips(l, move |look| {
                picks(look, which)
                    .into_iter()
                    .enumerate()
                    .map(|(i, (label, on))| (label, on, Act::Pick(which, i)))
                    .collect()
            }),
        )
    };
    let fine: Vec<View> = designer::MACROS
        .iter()
        .map(|m| {
            let says: Rc<dyn Fn(f64) -> String> = {
                let words = m.words;
                Rc::new(move |v: f64| {
                    let v = v.round().clamp(0.0, 255.0) as u8;
                    match words {
                        Some(words) => designer::word(v, words),
                        None => v.to_string(),
                    }
                })
            };
            slider(
                l,
                Key::Macro(m.which),
                m.label,
                (f64::from(m.range.0), f64::from(m.range.1)),
                says,
                m.ends,
            )
        })
        .collect();
    let part_sliders = {
        let l = l.clone();
        move || -> View {
            let part = l.body.get();
            match designer::BODY.iter().find(|(p, _)| *p == part) {
                Some((_, list)) => shape_sliders(&l, *list),
                None => View::Empty,
            }
        }
    };
    let average = {
        let l = l.clone();
        move || -> View {
            if designer::body_parts_moved(&l.look.get()) {
                view! {
                    <div><Button variant=Variant::Secondary size=Size::Sm onclick={l.act(Act::AverageBodyParts)}>"Average body parts"</Button></div>
                }
            } else {
                View::Empty
            }
        }
    };
    view! {
        {row("Body shape", PickRow::Shape)}
        {row("Age", PickRow::Age)}
        {row("Build", PickRow::Build)}
        {row("Height", PickRow::Height)}
        <Collapse model={l.fine_tune.clone()} title="Fine-tune">
            <div style="display: flex; flex-direction: column; gap: 16px; padding-top: 8px">{fine}</div>
        </Collapse>
        <Collapse model={l.body_parts.clone()} title="Body parts">
            <div style="display: flex; flex-direction: column; gap: 16px; padding-top: 8px">
                {part_chips(&l.body, &designer::BODY_PARTS)}
                {part_sliders}
                {average}
            </div>
        </Collapse>
    }
}

fn face_tab(l: &Live) -> View {
    // Whole faces first: most people want a different face, not a narrower
    // nose.
    let buttons = {
        let l = l.clone();
        move || -> View {
            let average: View = if designer::has_face(&l.look.get()) {
                view! { <Button size=Size::Sm onclick={l.act(Act::AverageFace)}>"Average face"</Button> }
            } else {
                View::Empty
            };
            view! {
                <div style="display: flex; flex-wrap: wrap; gap: 8px">
                    <Button size=Size::Sm onclick={l.act(Act::AnotherFace)}>"Another face"</Button>
                    <Button size=Size::Sm onclick={l.act(Act::SimilarFace)}>"Similar face"</Button>
                    {average}
                </div>
            }
        }
    };
    let part_sliders = {
        let l = l.clone();
        move || -> View {
            let part = l.face.get();
            match designer::FACE.iter().find(|(p, _)| *p == part) {
                Some((_, list)) => shape_sliders(&l, *list),
                None => View::Empty,
            }
        }
    };
    view! {
        {buttons}
        {part_chips(&l.face, &designer::FACE_PARTS)}
        {part_sliders}
    }
}

fn skin_and_hair_tab(l: &Live) -> View {
    let tone: Rc<dyn Fn(f64) -> String> = Rc::new(|v: f64| designer::tone_says(v.round() as i32));
    let hair = chips(l, |look| {
        let mut row = vec![("None", look.hair.is_none(), Act::Hair(None))];
        row.extend(designer::HAIR.iter().map(|(name, label)| {
            (
                *label,
                look.hair.as_deref() == Some(*name),
                Act::Hair(Some(*name)),
            )
        }));
        row
    });
    let brows = chips(l, |look| {
        designer::EYEBROWS
            .iter()
            .map(|(name, label)| (*label, look.eyebrows == *name, Act::Brows(*name)))
            .collect()
    });
    view! {
        {block("Skin", named_swatches(l, &designer::SKINS, |look| look.skin.as_str(), Act::Skin))}
        {slider(l, Key::Tone, "Tone", (-100.0, 100.0), tone, ("Darker", "Lighter"))}
        {note("Your age and body shape choose among the family's skins; the tone shades it.")}
        {block("Eyes", named_swatches(l, &designer::EYES, |look| look.eyes.as_str(), Act::Eyes))}
        {block("Hair", hair)}
        {colours(l, "hair", &designer::HAIR_COLOURS, Tint::Hair)}
        {block("Brows", brows)}
        {block("Beard", extra_chips(l, Extra::Beard, true))}
        {note("Brows, lashes and beard take the hair's colour.")}
    }
}

/// Chips for one of an extras list: with "None" first, or (a top, a bottom
/// with separates) exactly one.
fn extra_chips(l: &Live, kind: Extra, none: bool) -> View {
    chips(l, move |look| {
        let worn = designer::extra_of(look, kind.list());
        let mut row = Vec::new();
        if none {
            row.push(("None", worn.is_none(), Act::Extra(kind, None)));
        }
        row.extend(
            kind.list()
                .iter()
                .map(|(name, label)| (*label, worn == Some(*name), Act::Extra(kind, Some(*name)))),
        );
        row
    })
}

fn clothes_tab(l: &Live) -> View {
    let outfits = chips(l, |look| {
        let mut row = vec![("Separates", look.outfit.is_empty(), Act::Separates)];
        row.extend(
            designer::OUTFITS
                .iter()
                .map(|(name, label)| (*label, look.outfit == *name, Act::Outfit(*name))),
        );
        row
    });
    let dresses = chips(l, |look| {
        designer::DRESSES
            .iter()
            .map(|(name, label)| (*label, look.outfit == *name, Act::Outfit(*name)))
            .collect()
    });
    // With separates, which top and bottom; either way, their colours.
    let separates = |kind: Extra| -> View {
        let l = l.clone();
        let draw = move || -> View {
            if l.look.with(|look| look.outfit.is_empty()) {
                extra_chips(&l, kind, false)
            } else {
                View::Empty
            }
        };
        draw.into_view()
    };
    let shoes = chips(l, |look| {
        designer::SHOES
            .iter()
            .map(|(name, label)| (*label, look.shoes == *name, Act::Shoes(*name)))
            .collect()
    });
    let hats = chips(l, |look| {
        let mut row = vec![("None", look.hat.is_none(), Act::Hat(None))];
        row.extend(designer::HATS.iter().map(|(name, label)| {
            (
                *label,
                look.hat.as_deref() == Some(*name),
                Act::Hat(Some(*name)),
            )
        }));
        row
    });
    let top = view! {
        {separates(Extra::Top)}
        {colours(l, "top", &designer::CLOTH_COLOURS, Tint::Top)}
    };
    let bottom = view! {
        {separates(Extra::Bottom)}
        {colours(l, "bottom", &designer::CLOTH_COLOURS, Tint::Bottom)}
    };
    view! {
        {block("Outfit", outfits)}
        {block("Dresses", dresses)}
        {block("Top", top)}
        {block("Bottom", bottom)}
        {block("Shoes", shoes)}
        {block("Hat", hats)}
        {block("Glasses", extra_chips(l, Extra::Glasses, true))}
        {note("Everything worn stretches with the body, so it still fits when the sliders move.")}
    }
}

/// The panel: a header with ✕, Undo · Redo · Back to saved, the view, the
/// starters, the tabs, and Apply at the foot.
fn panel(l: &Live) -> View {
    let frame = if l.compact {
        // A sheet over the bottom half, as egui's: a panel over the whole
        // screen would hide the avatar it is shaping.
        "position: absolute; left: 0; right: 0; bottom: 0; height: 50%; display: flex; flex-direction: column; background: var(--m-bg); border-top: 1px solid var(--m-border)".to_owned()
    } else {
        format!(
            "position: absolute; right: 0; top: 0; bottom: 0; width: {}px; display: flex; flex-direction: column; background: var(--m-bg); border-left: 1px solid var(--m-border)",
            designer::LIB_PANEL_WIDTH
        )
    };
    let history = {
        let l = l.clone();
        move || -> View {
            let s = l.status.get();
            let back: View = if s.saved_differs {
                view! { <Button variant=Variant::Ghost size=Size::Sm onclick={l.act(Act::BackToSaved)}>"Back to saved"</Button> }
            } else {
                View::Empty
            };
            view! {
                <div style="display: flex; flex-wrap: wrap; gap: 8px">
                    <Button variant=Variant::Ghost size=Size::Sm disabled={!s.can_undo} onclick={l.act(Act::Undo)}>"Undo"</Button>
                    <Button variant=Variant::Ghost size=Size::Sm disabled={!s.can_redo} onclick={l.act(Act::Redo)}>"Redo"</Button>
                    {back}
                </div>
            }
        }
    };
    let views: Vec<Opt<DesignerView>> = designer::VIEWS
        .iter()
        .map(|(v, label)| Opt::new(*v, *label))
        .collect();
    let tabs: Vec<Opt<DesignerTab>> = designer::TABS
        .iter()
        .map(|(t, label)| Opt::new(*t, *label))
        .collect();
    let starters = chips(l, |look| {
        let mut row: Vec<(&'static str, bool, Act)> = designer::STARTERS
            .iter()
            .enumerate()
            .map(|(i, s)| (s.label, *look == s.look(), Act::Starter(i)))
            .collect();
        row.push(("Surprise me", false, Act::Surprise));
        row
    });
    let body = {
        let l = l.clone();
        move || -> View {
            match l.tab.get() {
                DesignerTab::Body => body_tab(&l),
                DesignerTab::Face => face_tab(&l),
                DesignerTab::SkinAndHair => skin_and_hair_tab(&l),
                DesignerTab::Clothes => clothes_tab(&l),
            }
        }
    };
    let foot = {
        let l = l.clone();
        move || -> View {
            let changed = l.status.get().changed;
            let said: View = if changed {
                View::Empty
            } else {
                note("This is your current avatar.")
            };
            view! {
                <div style="display: flex; align-items: center; gap: 16px">
                    <Button variant=Variant::Primary disabled={!changed} onclick={l.act(Act::Apply)}>"Apply"</Button>
                    {said}
                </div>
            }
        }
    };
    let close = l.act(Act::Close);
    view! {
        <div style={frame}>
            <div style="display: flex; align-items: center; gap: 12px; min-height: 56px; padding: 0 12px 0 20px; border-bottom: 1px solid var(--m-border)">
                <span style="color: var(--m-accent)">{icon(RightPanel::Appearance.icon())}</span>
                <h1 style="flex: 1; margin: 0; font-size: 19px; font-weight: 600; color: var(--m-text-strong)">{RightPanel::Appearance.label()}</h1>
                <CloseButton onclick={close} />
            </div>
            <div style="flex: 1; min-height: 0; overflow-y: auto; padding: 20px 20px 28px; display: flex; flex-direction: column; gap: 20px">
                {history}
                <FieldSegmented model={l.view.clone()} options={views} label="View" size=Some(Size::Sm) id="av-view" />
                {block("Start from", starters)}
                {note("Then change anything below.")}
                <FieldSegmented model={l.tab.clone()} options={tabs} size=Some(Size::Sm) id="av-tab" />
                <div style="display: flex; flex-direction: column; gap: 20px">{body}</div>
            </div>
            <div style="flex: none; padding: 12px 20px 24px; border-top: 1px solid var(--m-border)">
                {foot}
            </div>
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every slider writes back what it reads, so a value bridged from the
    /// look is never taken for a drag.
    #[test]
    fn a_slider_writes_back_what_it_reads() {
        let mut look = HumanLook::default_for(false);
        look.set_face("nose/nose-hump-decr-incr", -30);
        look.skin_tone = 12;
        for s in sliders(&look) {
            let v = s.key.read(&look);
            let mut again = look.clone();
            s.key.write(&mut again, v);
            assert_eq!(again, look, "{:?}", s.key);
        }
    }

    /// The quick picks say chosen for the look they set.
    #[test]
    fn a_pick_is_chosen_once_made() {
        let mut look = HumanLook::default_for(true);
        for row in [
            PickRow::Shape,
            PickRow::Age,
            PickRow::Build,
            PickRow::Height,
        ] {
            for i in 0..picks(&look, row).len() {
                pick(&mut look, row, i);
                assert!(picks(&look, row)[i].1);
            }
        }
    }
}
