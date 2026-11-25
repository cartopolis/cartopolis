//! The route planner drawn by the component library (`lib_host`), beside
//! egui's (`navigation::render_route_sheet`) — the fourth screen, after
//! Settings, Layers and Search. A bottom sheet over a map that stays
//! visible (`route-planner-sheet.md`): the travel modes, the one line the
//! planner exists to produce, the two ends as one card, the turns, and
//! what to do with the route along the foot.
//!
//! On with `CARTO_UI_LIB=route` (or `1`), on the web `?ui=lib`
//! (`&panel=route` opens it). egui's system steps aside (`RouteState::lib`)
//! and draws nothing — the sheet floats, so there is no strip to keep — but
//! still works out what only it can see (where "My position" is, the
//! keyboard's lift, the arrival clock: `RouteState::lib_view`), starts the
//! trip this sheet asks for (`RouteState::lib_trip`), and publishes the
//! sheet's rect for the pins. Every edit goes through the same
//! `RouteState` methods egui's sheet calls (`apply`, `edit_label`), so the
//! two editions cannot drift apart.

use std::cell::RefCell;
use std::rc::Rc;

use bevy::ecs::system::NonSendMarker;
use bevy::prelude::*;
// The library's, not Bevy's: an explicit import outranks the two globs.
use mithril::prelude::*;
use mithril::prelude::{Button, Event};

use crate::resources::RightPanel;
use crate::systems::egui_manager::EguiMut;
use crate::systems::geo_nav::{distance_label, GeoNavState, GeoSuggestion};
use crate::systems::lib_settings::LibScreens;
use crate::systems::navigation::{
    lib_sheet_max_height, lib_sheet_width, start_action, start_parts, Endpoint, PanelActions,
    RouteState, SheetDetent, SolveStatus, Trip, LIB_SHEET_HEADER_PTS,
};
use crate::systems::routing::{format_distance, format_duration, TravelMode};
use crate::systems::{icons, theme};

/// Where the sheet sits, in points (the library lays out in egui's scale).
#[derive(Clone, Copy, PartialEq, Default)]
struct Geom {
    left: f32,
    width: f32,
    bottom: f32,
    max_height: f32,
}

impl Geom {
    fn style(self) -> String {
        format!(
            "position: absolute; left: {:.0}px; bottom: {:.0}px; width: {:.0}px; max-height: {:.0}px; \
             display: flex; flex-direction: column; background: var(--m-bg); \
             border: 1px solid var(--m-border); border-radius: var(--m-radius-lg); box-shadow: var(--m-shadow-lg)",
            self.left, self.bottom, self.width, self.max_height
        )
    }
}

/// The header's one control besides ✕: open or fold the sheet, when it can.
#[derive(Clone, Copy, PartialEq, Default)]
struct Head {
    /// There is a route and no map pick armed, so the sheet may fold.
    can_fold: bool,
    full: bool,
}

/// The line under the modes.
#[derive(Clone, PartialEq, Default)]
enum Summary {
    /// The card below is the whole question (open, no route, idle or failed).
    #[default]
    Hidden,
    Fetching(usize, usize),
    Solving,
    Failed(String),
    Route {
        time: String,
        arrive: Option<String>,
        line: String,
    },
    Empty,
}

/// The start as the card shows it.
#[derive(Clone, PartialEq, Default)]
enum Start {
    /// A field, being changed or still empty.
    #[default]
    Open,
    /// One line, "My position" or the place; a tap opens it.
    Line { text: String, mine: bool },
}

/// One suggestion as drawn.
#[derive(Clone, PartialEq)]
struct Row {
    name: String,
    kind: Option<String>,
    distance: Option<String>,
    context: String,
}

impl Row {
    fn new(s: &GeoSuggestion, here: Option<(f64, f64)>) -> Self {
        Row {
            name: s.name.clone(),
            kind: s.kind.clone(),
            distance: here.map(|h| distance_label(h, (s.lat, s.lng))),
            context: s.context.clone(),
        }
    }
}

/// One turn as drawn.
#[derive(Clone, PartialEq)]
struct Step {
    glyph: &'static str,
    text: String,
    distance: Option<String>,
}

/// The foot, with a route.
#[derive(Clone, PartialEq)]
struct Foot {
    /// "Walk" beside the primary: walking it yourself.
    walk: bool,
    /// On foot, where the primary is "Walk me there".
    on_foot: bool,
    /// The primary's glyph and words, or (`Err`) why there is none.
    start: Result<(&'static str, &'static str), (&'static str, &'static str)>,
}

/// What a tap asked for; done by the system, against the route as it is then.
#[derive(Clone, Copy)]
enum Act {
    Mode(TravelMode),
    Swap,
    Clear,
    Drive,
    Walk(bool),
    Step(usize),
    UseHere,
    Pick(Endpoint),
    Choose(Endpoint, usize),
    ClearEnd(Endpoint),
    EditStart,
    Escape(Endpoint),
    Fold,
    Close,
}

#[derive(Clone)]
struct Live {
    geom: Signal<Geom>,
    head: Signal<Head>,
    mode: Signal<TravelMode>,
    durations: Signal<[Option<f64>; 3]>,
    summary: Signal<Summary>,
    picking: Signal<Option<Endpoint>>,
    full: Signal<bool>,
    start: Signal<Start>,
    from_text: Signal<String>,
    to_text: Signal<String>,
    /// Per end (start, destination): holds text, its lookup runs.
    ends: Signal<[(bool, bool); 2]>,
    /// The end the fill chips are for, and whether "My position" is offered.
    chips: Signal<Option<(Endpoint, bool)>>,
    suggestions: Signal<Option<(Endpoint, Vec<Row>)>>,
    failure: Signal<Option<String>>,
    steps: Signal<Vec<Step>>,
    foot: Signal<Option<Foot>>,
    acts: Rc<RefCell<Vec<Act>>>,
}

impl Live {
    fn act(&self, a: Act) -> Option<Handler> {
        let acts = self.acts.clone();
        Some(Rc::new(move |_: &Event| acts.borrow_mut().push(a)) as Handler)
    }
}

/// The mounted sheet, and the field texts last seen on both sides: a field
/// that moved since is the person typing, a label that moved is the route
/// (a pick, a reverse lookup, a swap).
struct Mounted {
    live: Live,
    seen: [String; 2],
}

thread_local! {
    static MOUNTED: RefCell<Option<Mounted>> = const { RefCell::new(None) };
}

/// The modes in the order the segmented control shows them: the default and
/// what needs least first, as egui's chips.
const MODES: [TravelMode; 3] = [TravelMode::Foot, TravelMode::Bike, TravelMode::Car];

const ENDS: [Endpoint; 2] = [Endpoint::From, Endpoint::To];

fn index(end: Endpoint) -> usize {
    match end {
        Endpoint::From => 0,
        Endpoint::To => 1,
    }
}

/// Write a signal only when the value moved: a write re-runs what reads it.
fn put<T: Clone + PartialEq + 'static>(s: &Signal<T>, v: T) {
    if s.peek(|now| *now != v) {
        s.set(v);
    }
}

fn geom_of(route: &RouteState, viewport: [f32; 2]) -> Geom {
    let width = lib_sheet_width(viewport[0]);
    let bottom = route.lib_view.bottom;
    Geom {
        left: ((viewport[0] - width) / 2.0).max(0.0),
        width,
        bottom,
        max_height: lib_sheet_max_height(viewport[1], bottom),
    }
}

/// The summary, as egui's `summary_line` decides it.
fn summary_of(route: &RouteState) -> Summary {
    let body_says_it = route.detent() == SheetDetent::Full
        && route.route.is_none()
        && matches!(route.status, SolveStatus::Idle | SolveStatus::Failed(_));
    if body_says_it {
        return Summary::Hidden;
    }
    match (&route.status, &route.route) {
        (SolveStatus::Fetching(done, total), _) => Summary::Fetching(*done, *total),
        (SolveStatus::Solving, _) => Summary::Solving,
        (SolveStatus::Failed(msg), _) => Summary::Failed(msg.clone()),
        (SolveStatus::Idle, Some(r)) => Summary::Route {
            time: format_duration(r.duration_s),
            arrive: route.lib_view.arrive.clone(),
            line: route.destination_line(r),
        },
        (SolveStatus::Idle, None) => Summary::Empty,
    }
}

/// The suggestions up, the start's before the destination's (egui can show
/// both as popups; one list under the card is enough for a tap).
fn suggestions_of(route: &RouteState) -> Option<(Endpoint, Vec<Row>)> {
    let here = route.lib_view.here;
    ENDS.into_iter().find_map(|end| {
        let list = route.waypoint(end).suggestions();
        (!list.is_empty()).then(|| (end, list.iter().map(|s| Row::new(s, here)).collect()))
    })
}

fn steps_of(route: &RouteState) -> Vec<Step> {
    route
        .route
        .as_ref()
        .map(|r| {
            r.steps
                .iter()
                .map(|s| Step {
                    glyph: icons::maneuver(s.maneuver),
                    text: s.instruction(),
                    distance: (s.distance_m >= 1.0).then(|| format_distance(s.distance_m)),
                })
                .collect()
        })
        .unwrap_or_default()
}

fn foot_of(route: &RouteState) -> Option<Foot> {
    route.route.as_ref()?;
    let (gps, avatar) = (route.lib_view.gps, route.lib_view.avatar);
    let parts = start_parts(route.mode, gps, avatar);
    Some(Foot {
        walk: route.mode == TravelMode::Foot && avatar,
        on_foot: route.mode == TravelMode::Foot,
        start: match start_action(route.mode, gps, avatar) {
            Ok(_) => Ok(parts),
            Err(_) => Err(parts),
        },
    })
}

/// Everything shown, from the route, into the signals that moved.
fn mirror(l: &Live, route: &RouteState, viewport: [f32; 2]) {
    put(&l.geom, geom_of(route, viewport));
    let full = route.detent() == SheetDetent::Full;
    put(
        &l.head,
        Head {
            can_fold: route.route.is_some() && route.picking.is_none(),
            full,
        },
    );
    put(&l.mode, route.mode);
    put(&l.durations, MODES.map(|m| route.duration_for(m)));
    put(&l.summary, summary_of(route));
    put(&l.picking, route.picking);
    put(&l.full, full);
    put(
        &l.start,
        if route.start_open() {
            Start::Open
        } else {
            Start::Line {
                text: route.start_text(),
                mine: route.from.label == crate::systems::navigation::HERE_LABEL,
            }
        },
    );
    put(
        &l.ends,
        ENDS.map(|end| {
            let wp = route.waypoint(end);
            (!wp.label.is_empty(), wp.looking_up())
        }),
    );
    put(
        &l.chips,
        route.fill_end().map(|end| {
            (
                end,
                end == Endpoint::From && route.lib_view.here.is_some() && !route.start_is_here(),
            )
        }),
    );
    put(&l.suggestions, suggestions_of(route));
    put(
        &l.failure,
        match (&route.status, full) {
            (SolveStatus::Failed(msg), true) => Some(msg.clone()),
            _ => None,
        },
    );
    put(&l.steps, if full { steps_of(route) } else { Vec::new() });
    put(&l.foot, foot_of(route));
}

/// Mounts, bridges and unmounts the route sheet. `PostUpdate`, before
/// `DriveUi`, as every library screen.
#[allow(clippy::too_many_arguments)]
pub fn drive_lib_route(
    _main: NonSendMarker,
    lib: Res<LibScreens>,
    mut right_panel: ResMut<RightPanel>,
    mut route: ResMut<RouteState>,
    mut nav: ResMut<GeoNavState>,
    egui: EguiMut,
    mut opened: Local<bool>,
) {
    if !lib.route {
        return;
    }
    if !route.lib {
        route.lib = true;
    }
    if lib.open_route && !*opened {
        *opened = true;
        *right_panel = RightPanel::Route;
    }
    // A world click (or a map pick) under the sheet is the sheet's:
    // `lib_host::bridge_input` says so for every library screen.
    let open = right_panel.is(RightPanel::Route);
    let viewport = {
        let v = egui.viewport_pts();
        [v.x, v.y]
    };
    MOUNTED.with(|m| {
        let mut m = m.borrow_mut();
        match (open, m.as_mut()) {
            (false, None) => {}
            (false, Some(_)) => {
                *m = None;
                crate::systems::lib_host::clear_keyed("route");
            }
            (true, None) => {
                let live = Live {
                    geom: signal(geom_of(&route, viewport)),
                    head: signal(Head::default()),
                    mode: signal(route.mode),
                    durations: signal([None; 3]),
                    summary: signal(Summary::default()),
                    picking: signal(None),
                    full: signal(true),
                    start: signal(Start::default()),
                    from_text: signal(route.from.label.clone()),
                    to_text: signal(route.to.label.clone()),
                    ends: signal([(false, false); 2]),
                    chips: signal(None),
                    suggestions: signal(None),
                    failure: signal(None),
                    steps: signal(Vec::new()),
                    foot: signal(None),
                    acts: Rc::new(RefCell::new(Vec::new())),
                };
                mirror(&live, &route, viewport);
                let p = theme::p();
                crate::systems::lib_host::mount_keyed(
                    "route",
                    sheet(&live),
                    p.ink == theme::DARK.ink,
                    theme::touch_layout(),
                );
                write_html(&live);
                *m = Some(Mounted {
                    seen: [route.from.label.clone(), route.to.label.clone()],
                    live,
                });
            }
            (true, Some(mounted)) => {
                let live = mounted.live.clone();
                // The fields → the ends, as egui's `changed()`.
                for (end, text) in ENDS.into_iter().zip([&live.from_text, &live.to_text]) {
                    let typed = text.get_untracked();
                    let seen = &mut mounted.seen[index(end)];
                    if typed != *seen {
                        *seen = typed.clone();
                        route.edit_label(end, typed);
                    }
                }
                // The taps of the frame, against the route as it stands.
                let acts: Vec<Act> = std::mem::take(&mut *live.acts.borrow_mut());
                if !acts.is_empty() {
                    act(acts, &mut route, &mut nav, &mut right_panel);
                }
                // The ends moved elsewhere (a pick, a lookup, a swap) → the fields.
                for (end, text) in ENDS.into_iter().zip([&live.from_text, &live.to_text]) {
                    let label = route.waypoint(end).label.clone();
                    let seen = &mut mounted.seen[index(end)];
                    if *seen != label {
                        *seen = label.clone();
                        put(text, label);
                    }
                }
                // The answers land after the mount: the HTML beside a capture
                // follows the route as it solves.
                let dump = std::env::var_os("CARTO_UI_LIB_HTML").is_some();
                let shown = |l: &Live| {
                    (
                        l.summary.get_untracked(),
                        l.suggestions.get_untracked(),
                        l.steps.get_untracked(),
                    )
                };
                let before = dump.then(|| shown(&live));
                mirror(&live, &route, viewport);
                if before.is_some_and(|b| b != shown(&live)) {
                    write_html(&live);
                }
            }
        }
    });
}

/// The frame's taps, done through the same `RouteState::apply` egui's sheet
/// calls; a trip is left for egui's system, which holds what it moves.
fn act(
    acts: Vec<Act>,
    route: &mut RouteState,
    nav: &mut GeoNavState,
    right_panel: &mut RightPanel,
) {
    let mut asked = PanelActions::default();
    for a in acts {
        match a {
            Act::Mode(m) => asked.mode = Some(m),
            Act::Swap => asked.swap = true,
            Act::Clear => asked.clear = true,
            Act::Drive => route.lib_trip = Some(Trip::Drive),
            Act::Walk(auto) => route.lib_trip = Some(Trip::Walk { auto }),
            Act::Step(i) => asked.goto_step = Some(i),
            Act::UseHere => asked.use_here = Some(Endpoint::From),
            Act::Pick(end) => asked.pick = Some(end),
            Act::Choose(end, i) => {
                if let Some(s) = route.waypoint(end).suggestions().get(i) {
                    asked.chose = Some((end, s.clone()));
                }
            }
            Act::ClearEnd(end) => asked.cleared = Some(end),
            Act::EditStart => asked.edit_start = true,
            Act::Escape(end) => route.dismiss_suggestions(end),
            Act::Fold => {
                route.sheet = match route.sheet {
                    SheetDetent::Full => SheetDetent::Peek,
                    SheetDetent::Peek => SheetDetent::Full,
                };
            }
            Act::Close => *right_panel = RightPanel::None,
        }
    }
    let here = route.lib_view.here;
    route.apply(&asked, nav, here);
}

/// `CARTO_UI_LIB_HTML=<path>`: the sheet as HTML, for the browser beside a
/// capture.
fn write_html(l: &Live) {
    if let Some(path) = std::env::var_os("CARTO_UI_LIB_HTML") {
        if let Err(e) = std::fs::write(&path, html(l)) {
            warn!(?path, error = %e, "lib route: could not write the HTML");
        }
    }
}

/// The sheet's HTML from the library's string renderer.
fn html(l: &Live) -> String {
    use mithril::view::{mount, StringRenderer};
    let r = Rc::new(RefCell::new(StringRenderer::new()));
    let root = r.borrow().root();
    let mounted = mount(sheet(l), &r, &root, None);
    let out = r.borrow().html();
    drop(mounted);
    out
}

fn icon(glyph: &'static str) -> View {
    view! {
        <span style="font-family: 'Material Symbols Rounded'; font-size: 20px; line-height: 1">{glyph}</span>
    }
}

/// A glyph turned: the icon font has chevrons left and right only.
fn icon_turned(glyph: &'static str, deg: i32) -> View {
    let style = format!(
        "font-family: 'Material Symbols Rounded'; font-size: 20px; line-height: 1; transform: rotate({deg}deg)"
    );
    view! { <span style={style}>{glyph}</span> }
}

/// A group's heading, as the library's section titles are set.
fn group(text: &'static str) -> View {
    view! {
        <p style="margin: 8px 0 0; padding-inline: 4px; color: var(--m-text-dim); font-size: var(--m-font-size-sm); font-weight: 650; letter-spacing: 0.06em; text-transform: uppercase">{text}</p>
    }
}

/// A quiet line of text.
fn note(text: String, colour: &'static str) -> View {
    let style = format!(
        "margin: 0; padding-inline: 4px; color: {colour}; font-size: var(--m-font-size-sm)"
    );
    view! { <p style={style}>{text}</p> }
}

/// Busy, with what it is doing.
fn busy(text: String) -> View {
    view! {
        <div style="display: flex; align-items: center; gap: 8px; padding-inline: 4px; color: var(--m-text-dim); font-size: var(--m-font-size-sm)">
            <Spinner size=Size::Sm />{text}
        </div>
    }
}

/// The marker beside an end: a ring for the start, a dot for the
/// destination — the colours of the pins on the map, by the theme's names.
fn marker(end: Endpoint) -> View {
    let style = match end {
        Endpoint::From => {
            "flex: none; width: 12px; height: 12px; border-radius: var(--m-radius-full); border: 2px solid var(--m-success)"
        }
        Endpoint::To => {
            "flex: none; width: 12px; height: 12px; border-radius: var(--m-radius-full); background: var(--m-danger); border: 2px solid var(--m-bg)"
        }
    };
    view! { <span style={style}></span> }
}

/// The rail's dots between the two markers.
fn rail_dots() -> View {
    const DOT: &str =
        "width: 3px; height: 3px; border-radius: var(--m-radius-full); background: var(--m-text-faint)";
    view! {
        <div style="display: flex; flex-direction: column; align-items: center; gap: 3px; width: 16px; padding: 2px 0">
            <span style={DOT}></span>
            <span style={DOT}></span>
            <span style={DOT}></span>
        </div>
    }
}

/// One end as a field: Escape folds its suggestions, a ✕ empties it, a
/// spinner while its lookup runs.
fn end_field(l: &Live, end: Endpoint) -> View {
    let (model, placeholder, id) = match end {
        Endpoint::From => (l.from_text.clone(), "Choose a start", "rt-from"),
        Endpoint::To => (l.to_text.clone(), "Where to?", "rt-to"),
    };
    let keys: Handler = {
        let acts = l.acts.clone();
        Rc::new(move |e: &Event| {
            if e.key.as_deref() == Some("Escape") {
                acts.borrow_mut().push(Act::Escape(end));
            }
        })
    };
    let suffix = {
        let l = l.clone();
        move || -> View {
            let (has_text, looking) = l.ends.get()[index(end)];
            let spin: View = if looking {
                view! { <Spinner size=Size::Sm /> }
            } else {
                View::Empty
            };
            let clear: View = if has_text {
                view! {
                    <Button variant=Variant::Ghost size=Size::Sm icon={icon(icons::CLEAR)} icon_only=true onclick={l.act(Act::ClearEnd(end))} title="Clear" />
                }
            } else {
                View::Empty
            };
            view! { {spin}{clear} }
        }
    };
    view! {
        <div style="flex: 1; min-width: 0">
            <FieldText model={model} placeholder={placeholder} autocomplete="off" onkeydown={Some(keys)} suffix={suffix.into_view()} id={id} />
        </div>
    }
}

/// The start: a field while it is open, else its one line, which a tap opens.
fn start_row(l: &Live) -> View {
    let l = l.clone();
    (move || -> View {
        match l.start.get() {
            Start::Open => end_field(&l, Endpoint::From),
            Start::Line { text, mine } => {
                let colour = if mine {
                    "color: var(--m-accent-text)"
                } else {
                    "color: var(--m-text)"
                };
                view! {
                    <div style="flex: 1; min-width: 0">
                        <ListItem onclick={l.act(Act::EditStart)} sub="Starting point — tap to change"><span style={colour}>{text}</span></ListItem>
                    </div>
                }
            }
        }
    })
    .into_view()
}

/// The two ends as one card: the markers down the left with the rail
/// between, the places beside them, one swap for the pair.
fn endpoint_card(l: &Live) -> View {
    view! {
        <div style="display: flex; align-items: center; gap: 12px">
            <div style="flex: 1; min-width: 0; display: flex; flex-direction: column; gap: 4px">
                <div style="display: flex; align-items: center; gap: 12px">
                    <div style="display: flex; justify-content: center; width: 16px">{marker(Endpoint::From)}</div>
                    {start_row(l)}
                </div>
                {rail_dots()}
                <div style="display: flex; align-items: center; gap: 12px">
                    <div style="display: flex; justify-content: center; width: 16px">{marker(Endpoint::To)}</div>
                    {end_field(l, Endpoint::To)}
                </div>
            </div>
            <Button variant=Variant::Ghost icon={icon(icons::SWAP)} icon_only=true onclick={l.act(Act::Swap)} title="Swap start and destination" />
        </div>
    }
}

/// The ways to fill an end without typing, under the card.
fn chips(l: &Live) -> View {
    let l = l.clone();
    (move || -> View {
        let Some((end, here)) = l.chips.get() else {
            return View::Empty;
        };
        let mine: View = if here {
            view! {
                <Button size=Size::Sm icon={icon(icons::HERE)} onclick={l.act(Act::UseHere)}>"My position"</Button>
            }
        } else {
            View::Empty
        };
        view! {
            <div style="display: flex; flex-wrap: wrap; gap: 8px">
                {mine}
                <Button size=Size::Sm icon={icon(icons::PICK_ON_MAP)} onclick={l.act(Act::Pick(end))} title="Tap the map to set this point">"Choose on map"</Button>
            </div>
        }
    })
    .into_view()
}

/// A suggestion: the name, what kind of place, how far and where.
fn suggestion(r: &Row, first: bool, pick: Option<Handler>) -> View {
    const PIECE: &str = "white-space: nowrap; margin-right: 8px";
    let title: View = match &r.kind {
        Some(kind) => {
            let kind = kind.clone();
            view! {
                <span style={PIECE}>{r.name.clone()}</span>
                <span style="white-space: nowrap; color: var(--m-text-dim); font-size: var(--m-font-size-sm); font-weight: 400">{kind}</span>
            }
        }
        None => r.name.clone().into_view(),
    };
    let distance: View = match &r.distance {
        Some(d) => view! {
            <span style="white-space: nowrap; margin-right: 8px; color: var(--m-accent-text)">{d.clone()}</span>
        },
        None => View::Empty,
    };
    let context: View = if r.context.is_empty() {
        View::Empty
    } else {
        view! { <span>{r.context.clone()}</span> }
    };
    let sub: View = view! { {distance}{context} };
    let style = if first {
        ""
    } else {
        "border-top: 1px solid var(--m-border)"
    };
    view! {
        <div style={style}><ListItem sub={sub} onclick={pick}>{title}</ListItem></div>
    }
}

fn suggestions(l: &Live) -> View {
    let l = l.clone();
    (move || -> View {
        let Some((end, rows)) = l.suggestions.get() else {
            return View::Empty;
        };
        let rows: Vec<View> = rows
            .iter()
            .enumerate()
            .map(|(i, r)| suggestion(r, i == 0, l.act(Act::Choose(end, i))))
            .collect();
        let heading = match end {
            Endpoint::From => "Start at",
            Endpoint::To => "Go to",
        };
        view! {
            {group(heading)}
            <List>{rows}</List>
        }
    })
    .into_view()
}

fn failure(l: &Live) -> View {
    let failure = l.failure.clone();
    (move || -> View {
        match failure.get() {
            Some(msg) => view! { <Alert tone=Tone::Danger>{msg}</Alert> },
            None => View::Empty,
        }
    })
    .into_view()
}

/// The turns; each flies the camera to its manoeuvre.
fn turns(l: &Live) -> View {
    let l = l.clone();
    (move || -> View {
        let steps = l.steps.get();
        if steps.is_empty() {
            return View::Empty;
        }
        let rows: Vec<View> = steps
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let sub: View = match &s.distance {
                    Some(d) => d.clone().into_view(),
                    None => View::Empty,
                };
                let text = s.text.clone();
                let style = if i == 0 {
                    ""
                } else {
                    "border-top: 1px solid var(--m-border)"
                };
                view! {
                    <div style={style}><ListItem icon={icon(s.glyph)} sub={sub} onclick={l.act(Act::Step(i))}>{text}</ListItem></div>
                }
            })
            .collect();
        view! {
            {group("Turns")}
            <List>{rows}</List>
        }
    })
    .into_view()
}

/// The travel modes, the chosen one pressed, each with its time once known.
fn modes(l: &Live) -> View {
    let l = l.clone();
    (move || -> View {
        // An icon and the time, as egui's strip: three words-and-times
        // ("Walk 19 min") did not fit a phone, and the third fell off.
        let durations = l.durations.get();
        let current = l.mode.get();
        let buttons: Vec<View> = MODES
            .iter()
            .zip(durations)
            .map(|(m, d)| {
                let text = d.map(format_duration).unwrap_or_default();
                let variant = if *m == current {
                    Variant::Primary
                } else {
                    Variant::Ghost
                };
                let title = m.label().to_owned();
                view! {
                    <Button variant={variant} size=Size::Sm icon={icon(icons::travel_mode(*m))} onclick={l.act(Act::Mode(*m))} title={title}>{text}</Button>
                }
            })
            .collect();
        view! {
            <div style="display: flex; flex-wrap: wrap; gap: 8px">{buttons}</div>
        }
    })
    .into_view()
}

/// The one line the planner exists to produce, or what stands between us and it.
fn summary(l: &Live) -> View {
    let summary = l.summary.clone();
    (move || -> View {
        match summary.get() {
            Summary::Hidden => View::Empty,
            Summary::Fetching(done, total) if total > 0 => {
                busy(format!("Reading map data… {done} of {total}"))
            }
            Summary::Fetching(..) => busy("Reading map data…".to_string()),
            Summary::Solving => busy("Finding the best route…".to_string()),
            Summary::Failed(msg) => view! { <Alert tone=Tone::Danger>{msg}</Alert> },
            Summary::Empty => note(
                "Set a start and a destination to plan a route.".to_string(),
                "var(--m-text-dim)",
            ),
            Summary::Route { time, arrive, line } => {
                let arrive: View = match arrive {
                    Some(a) => view! {
                        <span style="color: var(--m-text)">{format!("arrive {a}")}</span>
                    },
                    None => View::Empty,
                };
                view! {
                    <div style="display: flex; flex-direction: column; gap: 4px; padding-inline: 4px">
                        <div style="display: flex; align-items: baseline; justify-content: space-between; gap: 12px">
                            <span style="font-size: 26px; font-weight: 600; color: var(--m-text-strong)">{time}</span>
                            {arrive}
                        </div>
                        <span style="white-space: nowrap; overflow: hidden; color: var(--m-text-dim); font-size: var(--m-font-size-sm)">{line}</span>
                    </div>
                }
            }
        }
    })
    .into_view()
}

/// The map-pick prompt: outside the folding body, because arming a pick is
/// what folds the sheet.
fn prompt(l: &Live) -> View {
    let picking = l.picking.clone();
    (move || -> View {
        match picking.get() {
            Some(Endpoint::From) => note(
                "Click the map to set the starting point…".to_string(),
                "var(--m-accent-text)",
            ),
            Some(Endpoint::To) => note(
                "Click the map to set the destination…".to_string(),
                "var(--m-accent-text)",
            ),
            None => View::Empty,
        }
    })
    .into_view()
}

/// What a solved route affords: Clear, Walk, and the primary act at the
/// thumb's end — or, where this device cannot, the reason in its place.
fn foot(l: &Live) -> View {
    let l = l.clone();
    (move || -> View {
        let Some(f) = l.foot.get() else {
            return View::Empty;
        };
        let walk: View = if f.walk {
            view! {
                <Button variant=Variant::Ghost onclick={l.act(Act::Walk(false))} title="Walk it yourself, with the route drawn in the street">"Walk"</Button>
            }
        } else {
            View::Empty
        };
        let start: View = match f.start {
            Ok((glyph, words)) => {
                let a = if f.on_foot {
                    Act::Walk(true)
                } else {
                    Act::Drive
                };
                view! {
                    <Button variant=Variant::Primary icon={icon(glyph)} onclick={l.act(a)}>{words}</Button>
                }
            }
            // Why it cannot start: a line of its own under the buttons, as
            // egui has it, not squeezed beside Clear.
            Err((glyph, words)) => view! {
                <div style="flex: 1 1 100%; min-width: 0; display: flex; align-items: center; gap: 8px; color: var(--m-text-dim); font-size: var(--m-font-size-sm)">
                    {icon(glyph)}<span>{words}</span>
                </div>
            },
        };
        view! {
            <div style="flex: none; display: flex; align-items: center; flex-wrap: wrap; gap: 8px; padding: 16px 20px 24px; border-top: 1px solid var(--m-border)">
                <Button variant=Variant::Ghost onclick={l.act(Act::Clear)}>"Clear"</Button>
                {walk}
                <div style="flex: 1"></div>
                {start}
            </div>
        }
    })
    .into_view()
}

/// The open sheet's body: the card, its chips and suggestions, why a route
/// failed, and the turns.
fn body(l: &Live) -> View {
    let l = l.clone();
    (move || -> View {
        if !l.full.get() {
            return View::Empty;
        }
        view! {
            {endpoint_card(&l)}
            {chips(&l)}
            {suggestions(&l)}
            {failure(&l)}
            {turns(&l)}
        }
    })
    .into_view()
}

/// The header: the planner's name, fold or open, ✕.
fn header(l: &Live) -> View {
    let fold = {
        let l = l.clone();
        move || -> View {
            let h = l.head.get();
            if !h.can_fold {
                return View::Empty;
            }
            let (deg, title) = if h.full {
                (90, "Fold the sheet to the route's summary")
            } else {
                (-90, "Open the sheet: the places and the turns")
            };
            view! {
                <Button variant=Variant::Ghost icon={icon_turned(icons::CHEVRON_RIGHT, deg)} icon_only=true onclick={l.act(Act::Fold)} title={title} />
            }
        }
    };
    let style = format!(
        "flex: none; display: flex; align-items: center; gap: 12px; min-height: {:.0}px; padding: 0 12px 0 20px; border-bottom: 1px solid var(--m-border)",
        LIB_SHEET_HEADER_PTS
    );
    view! {
        <div style={style}>
            <span style="color: var(--m-accent)">{icon(RightPanel::Route.icon())}</span>
            <h1 style="flex: 1; margin: 0; font-size: 19px; font-weight: 600; color: var(--m-text-strong)">"Route"</h1>
            {fold}
            <CloseButton onclick={l.act(Act::Close)} />
        </div>
    }
}

/// The sheet: header, then one scrolling body, then the foot.
fn sheet(l: &Live) -> View {
    let geom = l.geom.clone();
    // A click on the sheet's bare background is the sheet's: listening makes
    // the frame a node the pointer can be over (`UiPointer`), so the world
    // and a map pick yield to all of it, not only to its controls.
    let swallow = |_: &Event| {};
    view! {
        <div style={move || geom.get().style()} on:click={swallow}>
            {header(l)}
            <div style="flex: 0 1 auto; min-height: 0; overflow-y: auto; padding: 20px 20px 28px; display: flex; flex-direction: column; gap: 16px">
                {modes(l)}
                {summary(l)}
                {prompt(l)}
                {body(l)}
            </div>
            {foot(l)}
        </div>
    }
}
