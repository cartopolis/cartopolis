//! The Models panel drawn by the component library (`lib_host`), beside
//! egui's (`model_library::render_model_library_panel`): your inventory and
//! the shared library, a search over both, Place, Share, Keep and Remove on
//! each model, and adding one.
//!
//! On with `CARTO_UI_LIB=models` (or `1`), on the web `?ui=lib`
//! (`&panel=models` opens it). egui's panel steps aside
//! (`ModelLibraryState::lib`) keeping an empty strip; the requests, their
//! answers and the file dialog stay `ModelLibraryState`'s
//! (`poll_model_library`), and every button calls the same
//! `ModelLibraryState` method egui's does, so the two cannot drift apart.
//!
//! Drawn differently: a model has no picture (neither edition has one — the
//! server cannot render, `model-assets-and-inventory.md`), so each row
//! leads with a placeholder tile of the Models glyph; and the details asked
//! for an upload open above the lists once a file is chosen, rather than in
//! the panel's foot, so a phone's keyboard does not cover them.

use std::cell::RefCell;
use std::rc::Rc;

use bevy::ecs::system::NonSendMarker;
use bevy::prelude::*;
// The library's, not Bevy's: an explicit import outranks the two globs.
use mithril::prelude::*;
use mithril::prelude::{Button, Event};

use crate::platform::model_file;
use crate::resources::{AvatarState, GameState, OnlineSession, RightPanel};
use crate::systems::egui_manager::EguiRef;
use crate::systems::lib_settings::LibScreens;
use crate::systems::model_library::{
    licence_label, matches, unavailable, upload_format_hint, Licence, ModelLibraryState, Tab,
    PANEL_WIDTH,
};
use crate::systems::{icons, theme};

/// The key this screen mounts under.
const KEY: &str = "models";

/// A line of your inventory, as drawn.
#[derive(Clone, PartialEq)]
struct Mine {
    id: i64,
    name: String,
    asset: String,
    sub: String,
    shared: bool,
    /// The licence recorded at upload lets it be handed on.
    may_share: bool,
}

/// An entry of the shared library, as drawn.
#[derive(Clone, PartialEq)]
struct Shared {
    asset: String,
    name: String,
    sub: String,
    attribution: String,
}

/// Both lists: what the panel compares from frame to frame.
#[derive(Clone, PartialEq, Default)]
struct Listing {
    mine: Vec<Mine>,
    shared: Vec<Shared>,
}

impl Listing {
    fn of(state: &ModelLibraryState) -> Self {
        Listing {
            mine: state
                .items
                .iter()
                .map(|item| Mine {
                    id: item.id,
                    name: item.name.clone(),
                    asset: item.asset.clone(),
                    sub: if item.folder.is_empty() {
                        format!("{} tri", item.triangles)
                    } else {
                        format!("{} tri · {}", item.triangles, item.folder)
                    },
                    shared: item.shared.is_some(),
                    may_share: item.licence != "other",
                })
                .collect(),
            shared: state
                .entries
                .iter()
                .map(|entry| Shared {
                    asset: entry.asset.clone(),
                    name: entry.name.clone(),
                    sub: format!(
                        "{} · {} tri · {}",
                        entry.category,
                        entry.triangles,
                        licence_label(&entry.licence)
                    ),
                    attribution: entry.attribution.clone(),
                })
                .collect(),
        }
    }
}

/// The panel's state outside the lists.
#[derive(Clone, PartialEq, Default)]
struct Status {
    /// Why there is nothing to show, when there is not.
    unavailable: Option<&'static str>,
    busy: bool,
    line: String,
    /// A file is chosen and waits for its details.
    has_file: bool,
}

impl Status {
    fn of(state: &ModelLibraryState, unavailable: Option<&'static str>) -> Self {
        Status {
            unavailable,
            busy: state.busy,
            line: state.status.clone(),
            has_file: state.has_file(),
        }
    }
}

/// What a tap asked for; done by the system through `ModelLibraryState`.
#[derive(Clone)]
enum Act {
    Refresh,
    Pick,
    Upload,
    Place(String, String),
    Share(i64, String),
    Unshare(i64, String),
    Discard(i64, String),
    Keep(String, String),
    Close,
}

#[derive(Clone)]
struct Live {
    listing: Signal<Listing>,
    status: Signal<Status>,
    /// Apart from `status`, so the search field is drawn again only when
    /// these change, not with every status line.
    unavailable: Signal<Option<&'static str>>,
    has_file: Signal<bool>,
    tab: Signal<Tab>,
    search: Signal<String>,
    upload_name: Signal<String>,
    upload_licence: Signal<Licence>,
    upload_attribution: Signal<String>,
    upload_share: Signal<bool>,
    acts: Rc<RefCell<Vec<Act>>>,
    /// Laid out for a phone.
    compact: bool,
}

impl Live {
    fn act(&self, a: Act) -> Option<Handler> {
        let acts = self.acts.clone();
        Some(Rc::new(move |_: &Event| acts.borrow_mut().push(a.clone())) as Handler)
    }
}

thread_local! {
    static MOUNTED: RefCell<Option<Live>> = const { RefCell::new(None) };
}

/// Mounts, bridges and unmounts the Models panel. `PostUpdate`, before
/// `DriveUi`, as every library screen.
#[allow(clippy::too_many_arguments)]
pub fn drive_lib_models(
    _main: NonSendMarker,
    lib: Res<LibScreens>,
    mut right_panel: ResMut<RightPanel>,
    mut state: ResMut<ModelLibraryState>,
    mut game_state: ResMut<GameState>,
    avatar_state: Res<AvatarState>,
    online: Option<Res<OnlineSession>>,
    egui: EguiRef,
    mut opened: Local<bool>,
) {
    if !lib.models {
        return;
    }
    if !state.lib {
        state.lib = true;
    }
    if lib.open_models && !*opened {
        *opened = true;
        *right_panel = RightPanel::Models;
    }
    let open = right_panel.is(RightPanel::Models);
    let unavailable = unavailable(online.is_some());
    MOUNTED.with(|m| {
        let mut m = m.borrow_mut();
        match (open, m.as_mut()) {
            (false, None) => {}
            (false, Some(_)) => {
                *m = None;
                crate::systems::lib_host::clear_keyed(KEY);
            }
            (true, None) => {
                state.ensure_loaded(unavailable.is_none());
                let live = Live {
                    listing: signal(Listing::of(&state)),
                    status: signal(Status::of(&state, unavailable)),
                    unavailable: signal(unavailable),
                    has_file: signal(state.has_file()),
                    tab: signal(state.tab),
                    search: signal(state.search.clone()),
                    upload_name: signal(state.upload_name.clone()),
                    upload_licence: signal(state.upload_licence),
                    upload_attribution: signal(state.upload_attribution.clone()),
                    upload_share: signal(state.upload_share),
                    acts: Rc::new(RefCell::new(Vec::new())),
                    compact: egui.compact(),
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
                // The fields → the state, where they moved.
                let tab = live.tab.get_untracked();
                if state.tab != tab {
                    state.tab = tab;
                }
                if live.search.peek(|s| *s != state.search) {
                    state.search = live.search.get_untracked();
                }
                if live.upload_name.peek(|s| *s != state.upload_name) {
                    state.upload_name = live.upload_name.get_untracked();
                }
                if live
                    .upload_attribution
                    .peek(|s| *s != state.upload_attribution)
                {
                    state.upload_attribution = live.upload_attribution.get_untracked();
                }
                let licence = live.upload_licence.get_untracked();
                if state.upload_licence != licence {
                    state.upload_licence = licence;
                }
                let share = live.upload_share.get_untracked();
                if state.upload_share != share {
                    state.upload_share = share;
                }
                // Not merely hidden: a box left ticked from a previous answer
                // would be a promise the server is about to refuse.
                if state.upload_share && !state.upload_licence.may_share() {
                    state.upload_share = false;
                }
                // The taps of the frame.
                let acts: Vec<Act> = std::mem::take(&mut *live.acts.borrow_mut());
                for a in acts {
                    match a {
                        Act::Refresh => state.refresh(),
                        Act::Pick => state.start_pick(),
                        Act::Upload => state.start_upload(),
                        Act::Place(hash, name) => {
                            state.place(&mut game_state, &avatar_state, &hash, &name)
                        }
                        Act::Share(id, name) => state.share_item(id, name),
                        Act::Unshare(id, name) => state.unshare_item(id, name),
                        Act::Discard(id, name) => state.discard_item(id, name),
                        Act::Keep(hash, name) => state.take_entry(hash, name),
                        Act::Close => *right_panel = RightPanel::None,
                    }
                }
                state.ensure_loaded(unavailable.is_none());
                mirror(live, &state, unavailable);
            }
        }
    });
}

/// The state → the panel's signals, where they moved.
fn mirror(live: &Live, state: &ModelLibraryState, unavailable: Option<&'static str>) {
    let listing = Listing::of(state);
    if live.listing.get_untracked() != listing {
        live.listing.set(listing);
        // The lists land after the mount: the HTML beside a capture follows.
        write_html(live);
    }
    let status = Status::of(state, unavailable);
    if live.status.get_untracked() != status {
        live.status.set(status);
    }
    if live.unavailable.get_untracked() != unavailable {
        live.unavailable.set(unavailable);
    }
    if live.has_file.get_untracked() != state.has_file() {
        live.has_file.set(state.has_file());
    }
    if live.tab.get_untracked() != state.tab {
        live.tab.set(state.tab);
    }
    if live.search.peek(|s| *s != state.search) {
        live.search.set(state.search.clone());
    }
    // An upload clears the name and the credit line.
    if live.upload_name.peek(|s| *s != state.upload_name) {
        live.upload_name.set(state.upload_name.clone());
    }
    if live
        .upload_attribution
        .peek(|s| *s != state.upload_attribution)
    {
        live.upload_attribution
            .set(state.upload_attribution.clone());
    }
    if live.upload_licence.get_untracked() != state.upload_licence {
        live.upload_licence.set(state.upload_licence);
    }
    if live.upload_share.get_untracked() != state.upload_share {
        live.upload_share.set(state.upload_share);
    }
}

/// `CARTO_UI_LIB_HTML=<path>`: the panel as HTML, for the browser beside a
/// capture.
fn write_html(l: &Live) {
    if let Some(path) = std::env::var_os("CARTO_UI_LIB_HTML") {
        if let Err(e) = std::fs::write(&path, html(l)) {
            warn!(?path, error = %e, "lib models: could not write the HTML");
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

/// A model: its placeholder tile, its name and what it is, and its buttons
/// under them.
fn model_row(first: bool, name: &str, sub: &str, extra: View, buttons: View) -> View {
    let style = format!(
        "display: flex; flex-direction: column; gap: 12px; padding: 14px 16px;{}",
        if first {
            ""
        } else {
            " border-top: 1px solid var(--m-border);"
        },
    );
    let (name, sub) = (name.to_owned(), sub.to_owned());
    view! {
        <div style={style}>
            <div style="display: flex; align-items: center; gap: 12px">
                <span style="flex: none; display: inline-grid; place-items: center; width: 44px; height: 44px; border-radius: var(--m-radius); background: var(--m-surface-raised); color: var(--m-accent)">{icon(icons::CABINET)}</span>
                <div style="flex: 1; min-width: 0; display: flex; flex-direction: column; gap: 2px">
                    <span style="font-weight: 600; color: var(--m-text-strong)">{name}</span>
                    <span style="color: var(--m-text-dim); font-size: var(--m-font-size-sm)">{sub}</span>
                </div>
            </div>
            {extra}
            <div style="display: flex; flex-wrap: wrap; align-items: center; gap: 8px">{buttons}</div>
        </div>
    }
}

fn mine_row(l: &Live, first: bool, m: &Mine) -> View {
    let share: View = if m.shared {
        view! {
            <Button variant=Variant::Ghost size=Size::Sm onclick={l.act(Act::Unshare(m.id, m.name.clone()))} title="Take it out of the shared library; anyone who already took a copy keeps theirs">"Unshare"</Button>
        }
    } else if m.may_share {
        view! {
            <Button variant=Variant::Ghost size=Size::Sm onclick={l.act(Act::Share(m.id, m.name.clone()))} title="Publish it to everyone on this server">"Share"</Button>
        }
    } else {
        // The licence recorded at upload decides. The server refuses either
        // way; the panel's job is not to offer a button that is going to be
        // refused, and to say why it is not offering it.
        view! {
            <Button variant=Variant::Ghost size=Size::Sm disabled=true title="You said this came from somewhere else, so it cannot be handed on">"Share"</Button>
        }
    };
    let buttons = view! {
        <Button size=Size::Sm onclick={l.act(Act::Place(m.asset.clone(), m.name.clone()))} title="Put it in the world, in front of you">"Place"</Button>
        {share}
        <span style="flex: 1"></span>
        <Button variant=Variant::Ghost size=Size::Sm icon={icon(icons::DELETE)} icon_only=true onclick={l.act(Act::Discard(m.id, m.name.clone()))} title="Remove it from your inventory" />
    };
    model_row(first, &m.name, &m.sub, View::Empty, buttons)
}

fn shared_row(l: &Live, first: bool, s: &Shared) -> View {
    // The credit line travels with the model. A CC-BY licence requires
    // that, and a list of models is where a person meets one.
    let credit: View = if s.attribution.is_empty() {
        View::Empty
    } else {
        note(s.attribution.clone())
    };
    let buttons = view! {
        <Button size=Size::Sm onclick={l.act(Act::Place(s.asset.clone(), s.name.clone()))} title="Put it in the world, in front of you">"Place"</Button>
        <Button variant=Variant::Ghost size=Size::Sm onclick={l.act(Act::Keep(s.asset.clone(), s.name.clone()))} title="Copy it into your inventory">"Keep"</Button>
    };
    model_row(first, &s.name, &s.sub, credit, buttons)
}

/// The details an upload asks for, once a file is chosen.
fn upload_form(l: &Live) -> View {
    let licences: Vec<Opt<Licence>> = [Licence::Own, Licence::Cc0, Licence::CcBy, Licence::Other]
        .into_iter()
        .map(|licence| Opt::new(licence, licence.label()))
        .collect();
    let follow = {
        let l = l.clone();
        move || -> View {
            let licence = l.upload_licence.get();
            let credit: View = if licence.needs_attribution() {
                view! {
                    <FieldText model={l.upload_attribution.clone()} placeholder="Credit line, e.g. “Bench by A. N. Other”" label="Credit" id="md-credit" />
                }
            } else {
                View::Empty
            };
            let share: View = if licence.may_share() {
                view! {
                    <FieldCheckbox model={l.upload_share.clone()} label="Share it with everyone on this server" id="md-share" />
                }
            } else {
                note("You will be able to use this, but not share it.")
            };
            view! { {credit}{share} }
        }
    };
    view! {
        <div style="display: flex; flex-direction: column; gap: 16px; padding: 16px; border: 1px solid var(--m-border); border-radius: var(--m-radius); background: var(--m-surface)">
            {group("Upload")}
            <FieldText model={l.upload_name.clone()} placeholder="What is it?" label="Name" id="md-name" />
            <FieldRadio model={l.upload_licence.clone()} options={licences} label="Where did it come from?" id="md-licence" />
            {follow}
            <div><Button variant=Variant::Primary icon={icon(icons::UPLOAD)} onclick={l.act(Act::Upload)}>"Upload"</Button></div>
        </div>
    }
}

/// The panel: a header with refresh and ✕, Mine | Library, the search, the
/// list, and adding a model with the status line at the foot.
fn panel(l: &Live) -> View {
    let frame = if l.compact {
        "position: absolute; left: 0; right: 0; top: 0; bottom: 0; display: flex; flex-direction: column; background: var(--m-bg)".to_owned()
    } else {
        format!(
            "position: absolute; right: 0; top: 0; bottom: 0; width: {PANEL_WIDTH}px; display: flex; flex-direction: column; background: var(--m-bg); border-left: 1px solid var(--m-border)"
        )
    };
    let tabs: Vec<Opt<Tab>> = vec![
        Opt::new(Tab::Mine, "Mine"),
        Opt::new(Tab::Library, "Library"),
    ];
    let body = {
        let l = l.clone();
        move || -> View {
            if let Some(why) = l.unavailable.get() {
                return note(why);
            }
            let upload = {
                let l = l.clone();
                move || -> View {
                    if l.has_file.get() {
                        upload_form(&l)
                    } else {
                        View::Empty
                    }
                }
            };
            let list = {
                let l = l.clone();
                move || -> View {
                    let listing = l.listing.get();
                    let needle = l.search.get().to_lowercase();
                    let rows: Vec<View> = match l.tab.get() {
                        Tab::Mine => {
                            if listing.mine.is_empty() {
                                return note("Nothing here yet.");
                            }
                            listing
                                .mine
                                .iter()
                                .filter(|m| matches(&m.name, &needle))
                                .enumerate()
                                .map(|(i, m)| mine_row(&l, i == 0, m))
                                .collect()
                        }
                        Tab::Library => {
                            if listing.shared.is_empty() {
                                return note("Nobody has shared a model yet.");
                            }
                            listing
                                .shared
                                .iter()
                                .filter(|s| matches(&s.name, &needle))
                                .enumerate()
                                .map(|(i, s)| shared_row(&l, i == 0, s))
                                .collect()
                        }
                    };
                    if rows.is_empty() {
                        return note("Nothing matches the search.");
                    }
                    view! { <List>{rows}</List> }
                }
            };
            let tabs = tabs.clone();
            view! {
                {upload}
                <FieldSegmented model={l.tab.clone()} options={tabs} id="md-tab" />
                <FieldText model={l.search.clone()} r#type=TextType::Search placeholder="Search" autocomplete="off" prefix={icon(icons::SEARCH)} id="md-search" />
                {list}
            }
        }
    };
    let foot = {
        let l = l.clone();
        move || -> View {
            let status = l.status.get();
            if status.unavailable.is_some() {
                return View::Empty;
            }
            let add: View = if !model_file::is_available() {
                // Android: no document picker (`platform::model_file`). Said,
                // rather than left as a panel that silently cannot do the
                // thing the panel is named after.
                note("Adding a model needs a desktop or a browser. Anything in the library can be placed from here.")
            } else if status.has_file {
                View::Empty
            } else {
                // What to export before opening the dialog, because a
                // refusal after a 12 MB upload is a slow way to learn it.
                view! {
                    <div><Button variant=Variant::Primary icon={icon(icons::ADD)} onclick={l.act(Act::Pick)}>"Add a model…"</Button></div>
                    {note(upload_format_hint())}
                }
            };
            let line: View = if status.busy {
                view! {
                    <div style="display: flex; align-items: center; gap: 8px; color: var(--m-text-dim); font-size: var(--m-font-size-sm)">
                        <Spinner size=Size::Sm />"Working…"
                    </div>
                }
            } else if status.line.is_empty() {
                View::Empty
            } else {
                note(status.line)
            };
            view! {
                <div style="flex: none; display: flex; flex-direction: column; gap: 12px; padding: 16px 20px 24px; border-top: 1px solid var(--m-border)">
                    {add}
                    {line}
                </div>
            }
        }
    };
    let refresh = {
        let l = l.clone();
        move || -> View {
            if l.status.get().unavailable.is_some() {
                View::Empty
            } else {
                view! {
                    <Button variant=Variant::Ghost icon={icon(icons::REFRESH)} icon_only=true onclick={l.act(Act::Refresh)} title="Fetch both lists again" />
                }
            }
        }
    };
    let close = l.act(Act::Close);
    view! {
        <div style={frame}>
            <div style="display: flex; align-items: center; gap: 12px; min-height: 56px; padding: 0 12px 0 20px; border-bottom: 1px solid var(--m-border)">
                <span style="color: var(--m-accent)">{icon(RightPanel::Models.icon())}</span>
                <h1 style="flex: 1; margin: 0; font-size: 19px; font-weight: 600; color: var(--m-text-strong)">{RightPanel::Models.label()}</h1>
                {refresh}
                <CloseButton onclick={close} />
            </div>
            <div style="flex: 1; min-height: 0; overflow-y: auto; padding: 20px 20px 28px; display: flex; flex-direction: column; gap: 16px">
                {body}
            </div>
            {foot}
        </div>
    }
}
