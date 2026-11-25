//! The Search panel drawn by the component library (`lib_host`), beside
//! egui's (`geo_nav::render_search_panel`) — phase 5 of
//! `docs/mockups/component-library`, the third screen, and the first with a
//! text field that drives the world: type, Enter flies to the top hit, a row
//! flies there or routes to it, Back, Forward and Home above, and the three
//! things to do with this view below.
//!
//! On with `CARTO_UI_LIB=search` (or `1`), on the web `?ui=lib`
//! (`&panel=search` opens it). egui's panel steps aside (`GeoNavState::lib`)
//! keeping an empty strip, and still runs the view actions, since it holds
//! the camera they need (`GeoNavState::lib_view`). Everything the field and
//! the rows do goes through the same `GeoNavState` methods egui's calls, so
//! the two editions cannot drift apart.

use std::cell::RefCell;
use std::rc::Rc;

use bevy::ecs::system::NonSendMarker;
use bevy::prelude::*;
// The library's, not Bevy's: an explicit import outranks the two globs.
use mithril::prelude::*;
use mithril::prelude::{Button, Event};

use crate::resources::RightPanel;
use crate::systems::egui_manager::EguiMut;
use crate::systems::geo_nav::{
    distance_label, GeoNavState, GeoSuggestion, SearchList, SearchPick, ViewAction,
};
use crate::systems::lib_settings::LibScreens;
use crate::systems::user_store::UserStore;
use crate::systems::{icons, theme};

/// One row as drawn: what the panel compares from frame to frame.
#[derive(Clone, PartialEq)]
struct Row {
    name: String,
    kind: Option<String>,
    /// The distance from the camera, when the camera is placed.
    distance: Option<String>,
    context: String,
    /// The answer is for an earlier query (drawn dimmed).
    dim: bool,
}

impl Row {
    fn new(s: &GeoSuggestion, viewer: Option<(f64, f64)>, dim: bool) -> Self {
        Row {
            name: s.name.clone(),
            kind: s.kind.clone(),
            distance: viewer.map(|here| distance_label(here, (s.lat, s.lng))),
            context: s.context.clone(),
            dim,
        }
    }
}

/// The list under the field, as drawn.
#[derive(Clone, PartialEq, Default)]
struct Listing {
    shown: bool,
    hits: Vec<Row>,
    recents: Vec<Row>,
    status: Option<String>,
    error: bool,
}

impl Listing {
    fn of(list: Option<&SearchList>, viewer: Option<(f64, f64)>) -> Self {
        let Some(l) = list else {
            return Listing::default();
        };
        Listing {
            shown: true,
            hits: l
                .hits
                .iter()
                .map(|(s, dim)| Row::new(s, viewer, *dim))
                .collect(),
            recents: l
                .recents
                .iter()
                .map(|(_, s)| Row::new(s, viewer, false))
                .collect(),
            status: l.status.clone(),
            error: l.error,
        }
    }
}

/// What a tap asked for; done by the system, against the list as it is then.
#[derive(Clone, Copy)]
enum Act {
    Hit(usize),
    Recent(usize),
    RouteHit(usize),
    RouteRecent(usize),
    Submit,
    Escape,
    Back,
    Forward,
    Home,
    View(ViewAction),
    DismissError,
    Close,
}

/// Back, Forward, Home available; the view known, shareable, and the action
/// that just fired.
#[derive(Clone, Copy, PartialEq, Default)]
struct Nav {
    back: bool,
    forward: bool,
    home: bool,
    place_known: bool,
    can_share: bool,
    done: Option<ViewAction>,
}

#[derive(Clone)]
struct Live {
    query: Signal<String>,
    listing: Signal<Listing>,
    searching: Signal<bool>,
    nav: Signal<Nav>,
    acts: Rc<RefCell<Vec<Act>>>,
    /// Laid out for a phone (the HTML written beside a capture needs it).
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

/// The panel's state each frame, from `GeoNavState` and the Home.
fn nav_of(nav: &GeoNavState, has_home: bool) -> Nav {
    Nav {
        back: nav.can_go_back(),
        forward: nav.can_go_forward(),
        home: has_home,
        place_known: nav.lib_view.place_known,
        can_share: nav.lib_view.can_share,
        done: nav.lib_view.done,
    }
}

/// Mounts, bridges and unmounts the Search panel. `PostUpdate`, before
/// `DriveUi`, as every library screen.
#[allow(clippy::too_many_arguments)]
pub fn drive_lib_search(
    _main: NonSendMarker,
    lib: Res<LibScreens>,
    mut right_panel: ResMut<RightPanel>,
    mut nav: ResMut<GeoNavState>,
    store: Res<UserStore>,
    egui: EguiMut,
    mut opened: Local<bool>,
) {
    if !lib.search {
        return;
    }
    if !nav.lib {
        nav.lib = true;
    }
    if lib.open_search && !*opened {
        *opened = true;
        *right_panel = RightPanel::Search;
    }
    let open = right_panel.is(RightPanel::Search);
    let has_home = store.data.home.is_some();
    // Ctrl+L with the panel already up: mount it again, so the field takes
    // the keyboard as it does on opening.
    let refocus = open && nav.focus_search && MOUNTED.with(|m| m.borrow().is_some());
    if refocus {
        MOUNTED.with(|m| *m.borrow_mut() = None);
        crate::systems::lib_host::clear_keyed("search");
    }
    MOUNTED.with(|m| {
        let mut m = m.borrow_mut();
        match (open, m.as_mut()) {
            (false, None) => {}
            (false, Some(_)) => {
                *m = None;
                crate::systems::lib_host::clear_keyed("search");
                if nav.search_has_focus {
                    nav.search_has_focus = false;
                }
            }
            (true, None) => {
                let viewer = nav.lib_view.viewer;
                let live = Live {
                    query: signal(nav.query.clone()),
                    listing: signal(Listing::of(nav.search_list().as_ref(), viewer)),
                    searching: signal(nav.searching()),
                    nav: signal(nav_of(&nav, has_home)),
                    acts: Rc::new(RefCell::new(Vec::new())),
                    compact: egui.compact(),
                };
                // The field takes the keyboard only when asked (Ctrl+L, a
                // scripted `--ui search`): on a phone a panel opened by a tap
                // would otherwise throw the keyboard over its recents.
                let focus = std::mem::take(&mut nav.focus_search);
                let compact = live.compact;
                let p = theme::p();
                crate::systems::lib_host::mount_keyed(
                    "search",
                    panel(&live, compact, focus),
                    p.ink == theme::DARK.ink,
                    theme::touch_layout(),
                );
                if let Some(path) = std::env::var_os("CARTO_UI_LIB_HTML") {
                    if let Err(e) = std::fs::write(&path, html(&live, compact)) {
                        warn!(?path, error = %e, "lib search: could not write the HTML");
                    }
                }
                *m = Some(live);
            }
            (true, Some(live)) => {
                // The field → the query, as egui's `changed()`.
                let typed = live.query.get_untracked();
                if typed != nav.query {
                    nav.query = typed;
                    nav.query_edited();
                }
                // The taps of the frame, against the list as it stands.
                let acts: Vec<Act> = std::mem::take(&mut *live.acts.borrow_mut());
                for a in acts {
                    act(a, &mut nav, &store, &mut right_panel);
                }
                // The query moved elsewhere (a pick names the place) → the field.
                if live.query.get_untracked() != nav.query {
                    live.query.set(nav.query.clone());
                }
                let listing = Listing::of(nav.search_list().as_ref(), nav.lib_view.viewer);
                if live.listing.get_untracked() != listing {
                    live.listing.set(listing);
                    // The answers land after the mount: the HTML beside a
                    // capture follows them.
                    if let Some(path) = std::env::var_os("CARTO_UI_LIB_HTML") {
                        let _ = std::fs::write(&path, html(live, live.compact));
                    }
                }
                let searching = nav.searching();
                if live.searching.get_untracked() != searching {
                    live.searching.set(searching);
                }
                let n = nav_of(&nav, has_home);
                if live.nav.get_untracked() != n {
                    live.nav.set(n);
                }
                // The auto-hiding chrome stays up while the field is typed in.
                let focused = egui.overlay_owns_keyboard();
                if nav.search_has_focus != focused {
                    nav.search_has_focus = focused;
                }
            }
        }
    });
}

/// One tap, done.
fn act(a: Act, nav: &mut GeoNavState, store: &UserStore, right_panel: &mut RightPanel) {
    let list = nav.search_list();
    let hit = |i: usize| {
        list.as_ref()
            .and_then(|l| l.hits.get(i))
            .map(|(s, _)| s.clone())
    };
    let recent = |i: usize| list.as_ref().and_then(|l| l.recents.get(i)).cloned();
    match a {
        Act::Hit(i) => {
            if let Some(s) = hit(i) {
                nav.pick(SearchPick::Hit(s));
            }
        }
        Act::RouteHit(i) => {
            if let Some(s) = hit(i) {
                nav.pick(SearchPick::Route(s));
            }
        }
        Act::Recent(i) => {
            if let Some((idx, _)) = recent(i) {
                nav.pick(SearchPick::Recent(idx));
            }
        }
        Act::RouteRecent(i) => {
            if let Some((_, s)) = recent(i) {
                nav.pick(SearchPick::Route(s));
            }
        }
        Act::Submit => nav.submit_query(),
        Act::Escape => nav.dismiss_list(),
        Act::Back => {
            if nav.can_go_back() {
                nav.go_back();
                nav.close_suggestions();
            }
        }
        Act::Forward => {
            if nav.can_go_forward() {
                nav.go_forward();
                nav.close_suggestions();
            }
        }
        Act::Home => {
            if let Some(h) = store.data.home.as_ref() {
                nav.go_home(h);
                nav.close_suggestions();
            }
        }
        Act::View(v) => nav.lib_view.action = Some(v),
        Act::DismissError => nav.dismiss_error(),
        Act::Close => {
            *right_panel = RightPanel::None;
            nav.close_suggestions();
        }
    }
}

/// The panel's HTML from the library's string renderer, for the browser
/// beside the capture.
fn html(l: &Live, compact: bool) -> String {
    use mithril::view::{mount, StringRenderer};
    let r = Rc::new(RefCell::new(StringRenderer::new()));
    let root = r.borrow().root();
    let mounted = mount(panel(l, compact, false), &r, &root, None);
    let out = r.borrow().html();
    drop(mounted);
    out
}

fn icon(glyph: &'static str) -> View {
    view! {
        <span style="font-family: 'Material Symbols Rounded'; font-size: 20px; line-height: 1">{glyph}</span>
    }
}

/// A group's heading, as the library's section titles are set.
fn group(text: &'static str) -> View {
    view! {
        <p style="margin: 8px 0 0; padding-inline: 4px; color: var(--m-text-dim); font-size: var(--m-font-size-sm); font-weight: 650; letter-spacing: 0.06em; text-transform: uppercase">{text}</p>
    }
}

/// A destination: the row flies there, the button beside it routes there.
fn row(r: &Row, first: bool, pick: Option<Handler>, route: Option<Handler>) -> View {
    // Each piece its own unbroken span, so a narrow row moves a whole piece
    // to the next line rather than breaking "150 km SW" in two.
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
    let style = format!(
        "display: flex; align-items: center; gap: 4px; padding-right: 8px;{}{}",
        if first {
            ""
        } else {
            " border-top: 1px solid var(--m-border);"
        },
        if r.dim { " opacity: 0.55;" } else { "" },
    );
    view! {
        <div style={style}>
            <div style="flex: 1; min-width: 0">
                <ListItem sub={sub} onclick={pick}>{title}</ListItem>
            </div>
            <Button variant=Variant::Ghost icon={icon(RightPanel::Route.icon())} icon_only=true onclick={route} title="Route here — make this the destination" />
        </div>
    }
}

/// The panel: a header with ✕, Back · Forward · Home, the field, the list,
/// and what to do with this view at the foot.
fn panel(l: &Live, compact: bool, focus: bool) -> View {
    let frame = if compact {
        "position: absolute; left: 0; right: 0; top: 0; bottom: 0; display: flex; flex-direction: column; background: var(--m-bg)"
    } else {
        "position: absolute; right: 0; top: 0; bottom: 0; width: 320px; display: flex; flex-direction: column; background: var(--m-bg); border-left: 1px solid var(--m-border)"
    };
    let history = {
        let l = l.clone();
        move || -> View {
            let n = l.nav.get();
            view! {
                <div style="display: flex; gap: 8px">
                    <Button icon={icon(icons::BACK)} icon_only=true disabled={!n.back} onclick={l.act(Act::Back)} title="Back to the previous location" />
                    <Button icon={icon(icons::FORWARD)} icon_only=true disabled={!n.forward} onclick={l.act(Act::Forward)} title="Forward to the next location" />
                    <Button icon={icon(icons::HOME)} icon_only=true disabled={!n.home} onclick={l.act(Act::Home)} title="Go to your Home location" />
                </div>
            }
        }
    };
    let keys: Handler = {
        let acts = l.acts.clone();
        Rc::new(move |e: &Event| match e.key.as_deref() {
            Some("Enter") => acts.borrow_mut().push(Act::Submit),
            Some("Escape") => acts.borrow_mut().push(Act::Escape),
            _ => {}
        })
    };
    let busy = {
        let searching = l.searching.clone();
        move || -> View {
            if searching.get() {
                view! {
                    <div style="display: flex; align-items: center; gap: 8px; color: var(--m-text-dim); font-size: var(--m-font-size-sm)">
                        <Spinner size=Size::Sm />"Searching…"
                    </div>
                }
            } else {
                View::Empty
            }
        }
    };
    let list = {
        let l = l.clone();
        move || -> View {
            let listing = l.listing.get();
            if !listing.shown {
                return View::Empty;
            }
            let hits: Vec<View> = listing
                .hits
                .iter()
                .enumerate()
                .map(|(i, r)| row(r, i == 0, l.act(Act::Hit(i)), l.act(Act::RouteHit(i))))
                .collect();
            let hits: View = if hits.is_empty() {
                View::Empty
            } else {
                view! { <List>{hits}</List> }
            };
            let recents: View = if listing.recents.is_empty() {
                View::Empty
            } else {
                let rows: Vec<View> = listing
                    .recents
                    .iter()
                    .enumerate()
                    .map(|(i, r)| row(r, i == 0, l.act(Act::Recent(i)), l.act(Act::RouteRecent(i))))
                    .collect();
                view! {
                    {group("Recent")}
                    <List>{rows}</List>
                }
            };
            let status: View = match (&listing.status, listing.error) {
                (Some(msg), true) => {
                    let acts = l.acts.clone();
                    let dismiss: Rc<dyn Fn()> =
                        Rc::new(move || acts.borrow_mut().push(Act::DismissError));
                    let msg = msg.clone();
                    view! { <Alert tone=Tone::Danger ondismiss={Some(dismiss)}>{msg}</Alert> }
                }
                (Some(msg), false) => {
                    let msg = msg.clone();
                    view! {
                        <p style="margin: 0; padding-inline: 4px; color: var(--m-text-dim); font-size: var(--m-font-size-sm)">{msg}</p>
                    }
                }
                (None, _) => View::Empty,
            };
            view! {
                {hits}
                {recents}
                {status}
            }
        }
    };
    let foot = {
        let l = l.clone();
        move || -> View {
            let n = l.nav.get();
            let entry = |action: ViewAction, glyph: &'static str, label: &'static str| -> View {
                let glyph = if n.done == Some(action) {
                    icons::CHECK
                } else {
                    glyph
                };
                let click = if n.place_known {
                    l.act(Act::View(action))
                } else {
                    None
                };
                let style = if n.place_known { "" } else { "opacity: 0.5" };
                view! {
                    <div style={style}><ListItem icon={icon(glyph)} onclick={click}>{label}</ListItem></div>
                }
            };
            let share = if n.can_share {
                entry(ViewAction::Share, icons::SHARE, "Share this view…")
            } else {
                View::Empty
            };
            view! {
                <List>
                    {entry(ViewAction::SetHome, icons::HOME, "Set Home here")}
                    {entry(ViewAction::CopyLink, icons::COPY, "Copy link")}
                    {share}
                </List>
            }
        }
    };
    let query = l.query.clone();
    let close = l.act(Act::Close);
    view! {
        <div style={frame}>
            <div style="display: flex; align-items: center; gap: 12px; min-height: 56px; padding: 0 12px 0 20px; border-bottom: 1px solid var(--m-border)">
                <span style="color: var(--m-accent)">{icon(RightPanel::Search.icon())}</span>
                <h1 style="flex: 1; margin: 0; font-size: 22px; font-weight: 600; color: var(--m-text-strong)">"Search"</h1>
                <CloseButton onclick={close} />
            </div>
            <div style="flex: 1; min-height: 0; overflow-y: auto; padding: 20px 20px 16px; display: flex; flex-direction: column; gap: 16px">
                {history}
                <FieldText model={query} r#type=TextType::Search placeholder="Search place, or paste \"lat, lng\"…" autocomplete="off" autofocus={focus} onkeydown={Some(keys)} prefix={icon(icons::SEARCH)} id="search-q" />
                {busy}
                {list}
            </div>
            <div style="flex: none; padding: 12px 20px 24px; border-top: 1px solid var(--m-border)">
                {foot}
            </div>
        </div>
    }
}
