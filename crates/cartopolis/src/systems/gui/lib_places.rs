//! The Places panel drawn by the component library (`lib_host`): the saved
//! places in their folders, a tap to fly there, and each row's actions —
//! route, photo, update, rename, move, delete — opened under it. The state
//! behind it is `bookmarks` (`BookmarksState`); everything a row does goes
//! through its `Actions` and `BookmarksState::apply_actions`.
//!
//! On with `CARTO_UI_LIB=places` (or `1`), on the web `?ui=lib`
//! (`&panel=places` opens it).
//!
//! A row is moved by "Move to" in its actions rather than dragged onto a
//! folder, and a folder's colour is picked from the palette new folders are
//! given (`FOLDER_PALETTE`) rather than from a free colour picker. A place's
//! photo is its thumbnail in the row ([`PlaceThumbs`]: the stored square,
//! cut to twice `photo::THUMB_POINTS` on a worker and kept by its stem), a
//! photo glyph until that is made; tapping it (or "View the photo") opens
//! it full size (`Actions::view_photo`), drawn by `lib_picture_viewer`.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;
use std::sync::Arc;

use bevy::ecs::system::NonSendMarker;
use bevy::prelude::*;
// The library's, not Bevy's: an explicit import outranks the two globs.
use mithril::prelude::*;
use mithril::prelude::{Button, Event};

use crate::grid::AnchoredCamera;
use crate::resources::{CameraState, MapAnchor, RightPanel};
use crate::systems::bookmarks::{current_view, rows, Actions, BookmarksState, Item, Row};
use crate::systems::geo_nav::GeoNavState;
use crate::systems::lib_settings::LibScreens;
use crate::systems::ui_input::UiMetrics;
use crate::systems::user_store::{UserStore, FOLDER_PALETTE};
use crate::systems::{icons, theme};

/// A folder as a place to move something to.
#[derive(Clone, PartialEq)]
struct Folder {
    id: u64,
    parent: Option<u64>,
    depth: usize,
    name: String,
}

/// The tree as drawn: what the panel compares from frame to frame.
#[derive(Clone, PartialEq, Default)]
struct Listing {
    rows: Vec<Row>,
    /// Every folder in tree order, collapsed or not: where a row can move.
    folders: Vec<Folder>,
    /// Each place's folder, for its "Move to".
    homes: Vec<(u64, Option<u64>)>,
}

impl Listing {
    fn of(store: &UserStore) -> Self {
        let mut folders = Vec::new();
        all_folders(store, None, 0, &mut folders);
        Listing {
            rows: rows(store),
            folders,
            homes: store
                .data
                .bookmarks
                .iter()
                .map(|b| (b.id, b.folder))
                .collect(),
        }
    }

    /// The folder `item` is in; `None` is the top level.
    fn parent(&self, item: Item) -> Option<u64> {
        match item {
            Item::Place(id) => self
                .homes
                .iter()
                .find(|(p, _)| *p == id)
                .and_then(|(_, f)| *f),
            Item::Folder(id) => self
                .folders
                .iter()
                .find(|f| f.id == id)
                .and_then(|f| f.parent),
        }
    }

    /// `id` is the folder `ancestor` or inside it. Bounded by the folder
    /// count, as `UserStore::is_descendant` is, against a cycle in a
    /// hand-edited file.
    fn within(&self, id: u64, ancestor: u64) -> bool {
        let mut at = Some(id);
        for _ in 0..=self.folders.len() {
            match at {
                Some(f) if f == ancestor => return true,
                Some(f) => {
                    at = self
                        .folders
                        .iter()
                        .find(|x| x.id == f)
                        .and_then(|x| x.parent)
                }
                None => return false,
            }
        }
        false
    }
}

/// Every folder under `parent`, depth first (as `bookmarks::flatten`, but
/// into collapsed folders too).
fn all_folders(store: &UserStore, parent: Option<u64>, depth: usize, out: &mut Vec<Folder>) {
    if depth > 32 {
        return;
    }
    for f in store.data.folders.iter().filter(|f| f.parent == parent) {
        out.push(Folder {
            id: f.id,
            parent: f.parent,
            depth,
            name: f.name.clone(),
        });
        all_folders(store, Some(f.id), depth + 1, out);
    }
}

/// The panel's state outside the tree.
#[derive(Clone, PartialEq, Default)]
struct Status {
    /// The camera is placed: there is somewhere to save, and to update to.
    has_place: bool,
    /// The name of the place a photo is being waited for.
    capturing: Option<String>,
    /// Why the last photo failed.
    photo_error: Option<String>,
}

impl Status {
    fn of(b: &BookmarksState, store: &UserStore, has_place: bool) -> Self {
        Status {
            has_place,
            capturing: b.capturing_for().map(|id| {
                store
                    .data
                    .bookmarks
                    .iter()
                    .find(|p| p.id == id)
                    .map_or_else(|| "this place".to_string(), |p| p.name.clone())
            }),
            photo_error: b.photo_error().map(str::to_owned),
        }
    }
}

/// What a tap asked for; done by the system, against the store as it is then.
#[derive(Clone, Copy)]
enum Act {
    NewPlace,
    NewFolder,
    Jump(u64),
    Route(u64),
    Update(u64),
    TakePhoto(u64),
    ViewPhoto(u64),
    RemovePhoto(u64),
    DeletePlace(u64),
    DeleteFolder(u64),
    Fold(u64),
    Recolor(u64, [f32; 3]),
    Move(Item, Option<u64>),
    /// Commit the name typed into the rename field.
    Rename(Item),
    CancelPhoto,
    DismissPhotoError,
    Close,
}

/// A destructive entry pressed once, waiting for the second press
/// (`theme::confirm_button`).
#[derive(Clone, Copy, PartialEq)]
enum Armed {
    DeletePlace(u64),
    DeleteFolder(u64),
    RemovePhoto(u64),
}

#[derive(Clone)]
struct Live {
    listing: Signal<Listing>,
    status: Signal<Status>,
    /// The photos' thumbnails made so far, by stem, as `data:` URLs.
    thumbs: Signal<BTreeMap<String, Arc<str>>>,
    /// The row whose actions are open under it.
    menu: Signal<Option<Item>>,
    /// The row being renamed, and the name as typed.
    editing: Signal<Option<Item>>,
    rename: Signal<String>,
    armed: Signal<Option<Armed>>,
    acts: Rc<RefCell<Vec<Act>>>,
    /// Laid out for a phone (the HTML written beside a capture needs it).
    compact: bool,
}

impl Live {
    fn act(&self, a: Act) -> Option<Handler> {
        let acts = self.acts.clone();
        Some(Rc::new(move |_: &Event| acts.borrow_mut().push(a)) as Handler)
    }

    /// An entry of a row's actions: does `a` and folds the actions away, as
    /// egui's menu closes on a choice.
    fn menu_act(&self, a: Act) -> Option<Handler> {
        let (acts, menu, armed) = (self.acts.clone(), self.menu.clone(), self.armed.clone());
        Some(Rc::new(move |_: &Event| {
            armed.set(None);
            menu.set(None);
            acts.borrow_mut().push(a);
        }) as Handler)
    }
}

thread_local! {
    static MOUNTED: RefCell<Option<Live>> = const { RefCell::new(None) };
}

/// How many frames a thumbnail that could not be read waits before it is
/// read again: on the web it may still be loading, natively it is gone,
/// and a file read a frame for every such row is not free.
const THUMB_RETRY_FRAMES: u64 = 120;

/// The places' thumbnails: `photo_store::read_thumb`'s square JPEG, cut to
/// twice `photo::THUMB_POINTS` on a worker (`thumbs`), once per photo.
#[derive(Default)]
pub struct PlaceThumbs {
    thumbs: crate::systems::thumbs::Thumbs,
    /// Stems whose thumbnail was not there, and the frame it was looked for.
    missing: HashMap<String, u64>,
    frame: u64,
}

impl PlaceThumbs {
    /// Every place's thumbnail that is made, asking for the rest.
    fn of(&mut self, store: &UserStore) -> BTreeMap<String, Arc<str>> {
        use crate::systems::thumbs::{Fit, Start};
        self.frame += 1;
        let frame = self.frame;
        let edge = (crate::utils::photo::THUMB_POINTS * 2.0).round() as u32;
        let mut out = BTreeMap::new();
        for stem in store.data.bookmarks.iter().filter_map(|b| b.photo.as_ref()) {
            let missing = &mut self.missing;
            let made = self.thumbs.get(stem, Fit::Square(edge), || {
                if missing
                    .get(stem)
                    .is_some_and(|at| frame < at + THUMB_RETRY_FRAMES)
                {
                    return Start::Wait;
                }
                match crate::utils::photo_store::read_thumb(stem) {
                    Some(jpeg) => {
                        missing.remove(stem);
                        Start::Bytes(jpeg)
                    }
                    None => {
                        missing.insert(stem.clone(), frame);
                        Start::Wait
                    }
                }
            });
            if let Some(t) = made {
                out.insert(stem.clone(), t.url);
            }
        }
        out
    }
}

/// Mounts, bridges and unmounts the Places panel. `PostUpdate`, before
/// `DriveUi`, as every library screen.
#[allow(clippy::too_many_arguments)]
pub fn drive_lib_places(
    _main: NonSendMarker,
    lib: Res<LibScreens>,
    mut right_panel: ResMut<RightPanel>,
    mut bookmarks: ResMut<BookmarksState>,
    mut store: ResMut<UserStore>,
    mut nav: ResMut<GeoNavState>,
    camera: AnchoredCamera,
    camera_state: Res<CameraState>,
    map_anchor: Res<MapAnchor>,
    metrics: Res<UiMetrics>,
    mut opened: Local<bool>,
    mut thumbs: Local<PlaceThumbs>,
) {
    if !lib.places {
        return;
    }
    if lib.open_places && !*opened {
        *opened = true;
        *right_panel = RightPanel::Places;
    }
    let open = right_panel.is(RightPanel::Places);
    MOUNTED.with(|m| {
        let mut m = m.borrow_mut();
        match (open, m.as_mut()) {
            (false, None) => {}
            (false, Some(_)) => {
                *m = None;
                crate::systems::lib_host::clear_keyed("places");
            }
            (true, None) => {
                let current = current_view(&camera, &camera_state, &map_anchor);
                let live = Live {
                    listing: signal(Listing::of(&store)),
                    status: signal(Status::of(&bookmarks, &store, current.is_some())),
                    thumbs: signal(thumbs.of(&store)),
                    menu: signal(None),
                    editing: signal(None),
                    rename: signal(String::new()),
                    armed: signal(None),
                    acts: Rc::new(RefCell::new(Vec::new())),
                    compact: metrics.compact(),
                };
                let p = theme::p();
                crate::systems::lib_host::mount_keyed(
                    "places",
                    panel(&live),
                    p.ink == theme::DARK.ink,
                    theme::touch_layout(),
                );
                write_html(&live);
                *m = Some(live);
            }
            (true, Some(live)) => {
                let current = current_view(&camera, &camera_state, &map_anchor);
                let has_place = current.is_some();
                let acts: Vec<Act> = std::mem::take(&mut *live.acts.borrow_mut());
                if !acts.is_empty() {
                    let mut out = Actions::default();
                    let mut close = false;
                    for a in acts {
                        close |= collect(a, live, &store, &mut out);
                    }
                    let jumped = bookmarks.apply_actions(&mut store, &mut nav, current, out);
                    if jumped || close {
                        *right_panel = RightPanel::None;
                    }
                }
                mirror(live, &bookmarks, &store, has_place);
                let made = thumbs.of(&store);
                if live.thumbs.get_untracked() != made {
                    live.thumbs.set(made);
                }
            }
        }
    });
}

/// The store and the photo state → the panel's signals, where they moved.
fn mirror(live: &Live, bookmarks: &BookmarksState, store: &UserStore, has_place: bool) {
    let listing = Listing::of(store);
    if live.listing.get_untracked() != listing {
        live.listing.set(listing);
        // An address resolving renames a row after the mount: the HTML
        // beside a capture follows it.
        write_html(live);
    }
    let status = Status::of(bookmarks, store, has_place);
    if live.status.get_untracked() != status {
        live.status.set(status);
    }
}

/// One tap, as the `Actions` egui's rows fill. True for the ✕.
fn collect(a: Act, l: &Live, store: &UserStore, out: &mut Actions) -> bool {
    let place = |id: u64| store.data.bookmarks.iter().find(|b| b.id == id);
    match a {
        Act::NewPlace => out.new_place = true,
        Act::NewFolder => out.new_folder = true,
        Act::Jump(id) => {
            if let Some(b) = place(id) {
                out.jump(&b.name, (b.lat, b.lng, b.altitude));
            }
        }
        Act::Route(id) => {
            if let Some(b) = place(id) {
                out.route = Some((b.name.clone(), b.lat, b.lng));
            }
        }
        Act::Update(id) => out.update = Some(id),
        Act::TakePhoto(id) => out.take_photo = Some(id),
        Act::ViewPhoto(id) => {
            if let Some(b) = place(id) {
                if let Some(stem) = &b.photo {
                    out.view_photo = Some((id, b.name.clone(), stem.clone()));
                }
            }
        }
        Act::RemovePhoto(id) => out.clear_photo = Some(id),
        Act::DeletePlace(id) => out.delete_place = Some(id),
        Act::DeleteFolder(id) => out.delete_folder = Some(id),
        Act::Fold(id) => out.toggle_collapse = Some(id),
        Act::Recolor(id, c) => out.recolor = Some((id, c)),
        Act::Move(item, target) => out.drop = Some((item, target)),
        Act::Rename(item) => out.rename = Some((item, l.rename.get_untracked())),
        Act::CancelPhoto => out.cancel_photo = true,
        Act::DismissPhotoError => out.dismiss_photo_error = true,
        Act::Close => return true,
    }
    false
}

/// `CARTO_UI_LIB_HTML=<path>`: the panel as HTML, for the browser beside a
/// capture.
fn write_html(l: &Live) {
    if let Some(path) = std::env::var_os("CARTO_UI_LIB_HTML") {
        if let Err(e) = std::fs::write(&path, html(l)) {
            warn!(?path, error = %e, "lib places: could not write the HTML");
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

/// Points of indentation per tree level.
const INDENT: usize = 16;

/// The middle of a row, around its `ListItem`: the row already pads its
/// sides and indents (`row_style`), so the item's own side padding comes
/// down to a hair. With both, a name had some 87 px between the marker and
/// the ⋯ and read "Laan Corpu…".
const ROW_BODY: &str = "flex: 1; min-width: 0; --m-space-4: 4px";

fn icon(glyph: &'static str) -> View {
    view! {
        <span style="font-family: 'Material Symbols Rounded'; font-size: 20px; line-height: 1">{glyph}</span>
    }
}

fn note(text: impl Into<String>) -> View {
    let text: String = text.into();
    view! {
        <p style="margin: 0; padding-inline: 4px; color: var(--m-text-dim); font-size: var(--m-font-size-sm); line-height: 1.5">{text}</p>
    }
}

/// A group's heading, as the library's section titles are set.
fn group(text: &'static str) -> View {
    view! {
        <p style="margin: 0; color: var(--m-text-dim); font-size: var(--m-font-size-sm); font-weight: 650; letter-spacing: 0.06em; text-transform: uppercase">{text}</p>
    }
}

/// A folder's colour as CSS. A content colour (the pins on the map), not
/// chrome, so it is the store's and not a token.
fn hex(c: [f32; 3]) -> String {
    format!(
        "#{:02x}{:02x}{:02x}",
        (c[0] * 255.0) as u8,
        (c[1] * 255.0) as u8,
        (c[2] * 255.0) as u8
    )
}

/// The style every row of the tree shares: indented by depth, a hairline
/// above all but the first.
fn row_style(depth: usize, first: bool) -> String {
    format!(
        "display: flex; align-items: center; gap: 4px; padding: 0 8px 0 {}px;{}",
        12 + depth * INDENT,
        if first {
            ""
        } else {
            " border-top: 1px solid var(--m-border);"
        },
    )
}

/// The ⋯ at a row's end: opens its actions under it, or folds them away.
fn kebab(l: &Live, item: Item, title: &'static str) -> View {
    let (menu, armed) = (l.menu.clone(), l.armed.clone());
    let toggle: Handler = Rc::new(move |_: &Event| {
        armed.set(None);
        menu.update(|m| *m = if *m == Some(item) { None } else { Some(item) });
    });
    view! {
        <Button variant=Variant::Ghost icon={icon(icons::MORE)} icon_only=true onclick={Some(toggle)} title={title} />
    }
}

/// A destructive entry that asks twice: the first press changes its words,
/// the second does it.
fn confirm_entry(
    l: &Live,
    key: Armed,
    glyph: &'static str,
    label: &'static str,
    confirm_label: &'static str,
    a: Act,
) -> View {
    let text = if l.armed.get() == Some(key) {
        confirm_label
    } else {
        label
    };
    let click: Handler = {
        let (armed, menu, acts) = (l.armed.clone(), l.menu.clone(), l.acts.clone());
        Rc::new(move |_: &Event| {
            if armed.get_untracked() == Some(key) {
                armed.set(None);
                menu.set(None);
                acts.borrow_mut().push(a);
            } else {
                armed.set(Some(key));
            }
        })
    };
    view! {
        <ListItem icon={icon(glyph)} tone={Some(Tone::Danger)} onclick={Some(click)}>{text}</ListItem>
    }
}

/// "Move to": the top level and every folder `item` may go into (a folder
/// not into itself or below itself). Stands in for egui's drag and drop.
fn move_to(l: &Live, listing: &Listing, item: Item) -> View {
    let current = listing.parent(item);
    let mut options = vec![Opt::new(None, "Top level")];
    for f in &listing.folders {
        if let Item::Folder(id) = item {
            if listing.within(f.id, id) {
                continue;
            }
        }
        options.push(Opt::new(
            Some(f.id),
            format!("{}{}", "· ".repeat(f.depth), f.name),
        ));
    }
    let onchange: Rc<dyn Fn(&Option<Option<u64>>)> = {
        let (acts, menu) = (l.acts.clone(), l.menu.clone());
        Rc::new(move |v: &Option<Option<u64>>| {
            if let Some(target) = *v {
                if target != current {
                    menu.set(None);
                    acts.borrow_mut().push(Act::Move(item, target));
                }
            }
        })
    };
    let id = match item {
        Item::Place(id) => format!("places-move-p{id}"),
        Item::Folder(id) => format!("places-move-f{id}"),
    };
    mithril::view::render(FieldSelect {
        model: signal(Some(current)),
        options,
        label: View::Text("Move to".to_owned()),
        id: Some(id),
        onchange: Some(onchange),
        ..Default::default()
    })
}

/// The box a row's actions open in, under the row.
fn actions_box(depth: usize, body: View) -> View {
    let style = format!(
        "display: flex; flex-direction: column; gap: 16px; padding: 16px 16px 20px {}px; background: var(--m-surface-raised); border-top: 1px solid var(--m-border)",
        12 + depth * INDENT,
    );
    view! { <div style={style}>{body}</div> }
}

/// The rename field in place of a row: Enter or ✔ keeps the name, Escape or
/// ✕ leaves it.
fn rename_row(l: &Live, item: Item, depth: usize, first: bool) -> View {
    let commit: Handler = {
        let (acts, editing) = (l.acts.clone(), l.editing.clone());
        Rc::new(move |_: &Event| {
            editing.set(None);
            acts.borrow_mut().push(Act::Rename(item));
        })
    };
    let cancel: Handler = {
        let editing = l.editing.clone();
        Rc::new(move |_: &Event| editing.set(None))
    };
    let keys: Handler = {
        let (commit, cancel) = (commit.clone(), cancel.clone());
        Rc::new(move |e: &Event| match e.key.as_deref() {
            Some("Enter") => commit(e),
            Some("Escape") => cancel(e),
            _ => {}
        })
    };
    let style = format!(
        "display: flex; align-items: center; gap: 8px; padding: 10px 8px 10px {}px;{}",
        12 + depth * INDENT,
        if first {
            ""
        } else {
            " border-top: 1px solid var(--m-border);"
        },
    );
    view! {
        <div style={style}>
            <div style={ROW_BODY}>
                <FieldText model={l.rename.clone()} autofocus=true onkeydown={Some(keys)} placeholder="Name" autocomplete="off" id="places-rename" />
            </div>
            <Button variant=Variant::Primary icon={icon(icons::CHECK)} icon_only=true onclick={Some(commit)} title="Apply" />
            <Button variant=Variant::Ghost icon={icon(icons::CLEAR)} icon_only=true onclick={Some(cancel)} title="Cancel" />
        </div>
    }
}

/// "Rename": the field takes the row, holding the name as it is.
fn start_rename(l: &Live, item: Item, name: &str) -> Option<Handler> {
    let (editing, rename, menu, armed) = (
        l.editing.clone(),
        l.rename.clone(),
        l.menu.clone(),
        l.armed.clone(),
    );
    let name = name.to_string();
    Some(Rc::new(move |_: &Event| {
        armed.set(None);
        menu.set(None);
        rename.set(name.clone());
        editing.set(Some(item));
    }) as Handler)
}

/// A folder's row: the fold arrow, its name in its pins' colour (a tap
/// folds it), how much is in it while folded, and its actions.
#[allow(clippy::too_many_arguments)]
fn folder_row(
    l: &Live,
    first: bool,
    id: u64,
    depth: usize,
    name: &str,
    color: [f32; 3],
    collapsed: bool,
    count: usize,
) -> View {
    let c = hex(color);
    let arrow = format!(
        "width: 7px; height: 7px; border-right: 2px solid var(--m-text-dim); border-bottom: 2px solid var(--m-text-dim); rotate: {}deg",
        if collapsed { -45 } else { 45 },
    );
    let fold = {
        let acts = l.acts.clone();
        move |_: &Event| acts.borrow_mut().push(Act::Fold(id))
    };
    let fold_title = if collapsed { "Expand" } else { "Collapse" };
    let sub: View = if collapsed && count > 0 {
        let s = if count == 1 {
            "1 place".to_string()
        } else {
            format!("{count} places")
        };
        s.into_view()
    } else {
        View::Empty
    };
    let swatch = format!(
        "display: inline-block; width: 12px; height: 12px; border-radius: 50%; background: {c}"
    );
    let swatch: View = view! { <span style={swatch}></span> };
    let title_style = format!("color: {c}; font-weight: 650");
    let name = name.to_string();
    view! {
        <div style={row_style(depth, first)}>
            <button title={fold_title} style="flex: none; display: inline-grid; place-items: center; width: 32px; height: 32px; margin: 0; padding: 0; border: 0; background: none; cursor: pointer" on:click={fold}>
                <span style={arrow}></span>
            </button>
            <div style={ROW_BODY}>
                <ListItem sub={sub} end={swatch} onclick={l.act(Act::Fold(id))}><span style={title_style}>{name}</span></ListItem>
            </div>
            {kebab(l, Item::Folder(id), "Actions for this folder")}
        </div>
    }
}

/// A folder's actions: its pins' colour, rename, move, delete.
fn folder_actions(
    l: &Live,
    listing: &Listing,
    id: u64,
    depth: usize,
    name: &str,
    color: [f32; 3],
) -> View {
    let swatches: Vec<View> = FOLDER_PALETTE
        .iter()
        .map(|c| {
            let on = hex(*c) == hex(color);
            let style = format!(
                "flex: none; width: 30px; height: 30px; margin: 0; padding: 0; border-radius: 50%; background: {}; border: 3px solid {}; cursor: pointer",
                hex(*c),
                if on {
                    "var(--m-text-strong)"
                } else {
                    "var(--m-surface-raised)"
                },
            );
            let (acts, c) = (l.acts.clone(), *c);
            let pick = move |_: &Event| acts.borrow_mut().push(Act::Recolor(id, c));
            view! { <button title="Pin colour for every place in this folder" style={style} on:click={pick}></button> }
        })
        .collect();
    let body = view! {
        <div style="display: flex; flex-direction: column; gap: 10px">
            {group("Pin colour")}
            <div style="display: flex; flex-wrap: wrap; gap: 8px">{swatches}</div>
        </div>
        <List>
            <ListItem icon={icon(icons::RENAME)} onclick={start_rename(l, Item::Folder(id), name)}>"Rename"</ListItem>
        </List>
        {move_to(l, listing, Item::Folder(id))}
        <List>
            {confirm_entry(l, Armed::DeleteFolder(id), icons::DELETE, "Delete this folder", "Delete it — its places move up", Act::DeleteFolder(id))}
        </List>
    };
    actions_box(depth, body)
}

/// A place's row: its pin in its folder's colour — or, with a photo, the
/// photo's thumbnail framed in that colour (a photo glyph until it is
/// read), which opens the photo — its name (a tap flies there) and its
/// actions. `photo`: the place has one, and its thumbnail's `data:` URL
/// once it is made.
fn place_row(
    l: &Live,
    first: bool,
    id: u64,
    depth: usize,
    name: &str,
    color: [f32; 3],
    photo: Option<Option<&str>>,
) -> View {
    let c = hex(color);
    // One fixed square either way, so a photo does not move the name
    // (`bookmarks::marker_slot`).
    let marker: View = if let Some(thumb) = photo {
        let style = format!(
            "flex: none; display: inline-grid; place-items: center; width: 32px; height: 32px; margin: 0; padding: 0; border: 1.5px solid {c}; border-radius: var(--m-radius); background: var(--m-surface-raised); color: var(--m-text); cursor: pointer; overflow: hidden"
        );
        let acts = l.acts.clone();
        let open = move |_: &Event| acts.borrow_mut().push(Act::ViewPhoto(id));
        // `photo::THUMB_POINTS` square inside the frame's border.
        let inside: View = match thumb {
            Some(src) => {
                let side = crate::utils::photo::THUMB_POINTS;
                let style = format!("width: {side:.0}px; height: {side:.0}px; border-radius: 4px");
                let src = src.to_string();
                view! { <img src={src} alt="The place's photo" style={style} /> }
            }
            None => icon(icons::UPLOAD),
        };
        view! { <button title="Open the photo" style={style} on:click={open}>{inside}</button> }
    } else {
        let style = format!(
            "flex: none; display: inline-grid; place-items: center; width: 32px; height: 32px; color: {c}"
        );
        view! { <span style={style}>{icon(icons::PIN)}</span> }
    };
    // A saved name is often a whole address line ("Laan Corpus den Hoorn,
    // Corpus den Hoorn"): the part before the first comma is the title and
    // the rest the line under it, which reads at a glance where one long
    // line wrapped over four.
    let (title, rest) = match name.split_once(", ") {
        Some((t, r)) => (t.to_string(), r.to_string()),
        None => (name.to_string(), String::new()),
    };
    let sub: View = if rest.is_empty() {
        View::Empty
    } else {
        rest.into_view()
    };
    view! {
        <div style={row_style(depth, first)}>
            {marker}
            <div style={ROW_BODY}>
                <ListItem sub={sub} onclick={l.act(Act::Jump(id))}>{title}</ListItem>
            </div>
            {kebab(l, Item::Place(id), "Actions for this place")}
        </div>
    }
}

/// A place's actions, ordered as egui's menu: route first, the photo, update
/// and rename, move, and delete last and asking twice.
#[allow(clippy::too_many_arguments)]
fn place_actions(
    l: &Live,
    listing: &Listing,
    status: &Status,
    id: u64,
    depth: usize,
    name: &str,
    (lat, lng, altitude): (f64, f64, f64),
    photo: bool,
) -> View {
    let where_ = format!("{lat:.5}, {lng:.5} · {altitude:.0} m");
    let take_photo: View = if crate::platform::photo::is_available() {
        let label = if photo {
            "Replace the photo…"
        } else {
            crate::platform::photo::action_label()
        };
        view! { <ListItem icon={icon(icons::CAMERA)} onclick={l.menu_act(Act::TakePhoto(id))}>{label}</ListItem> }
    } else {
        View::Empty
    };
    let photo_entries: View = if photo {
        view! {
            <ListItem icon={icon(icons::UPLOAD)} onclick={l.menu_act(Act::ViewPhoto(id))}>"View the photo"</ListItem>
            {confirm_entry(l, Armed::RemovePhoto(id), icons::DELETE, "Remove the photo", "Remove it — this cannot be undone", Act::RemovePhoto(id))}
        }
    } else {
        View::Empty
    };
    // Greyed rather than hidden before the camera is placed, as egui's is.
    let update: View = if status.has_place {
        view! { <ListItem icon={icon(icons::REFRESH)} onclick={l.menu_act(Act::Update(id))}>"Update to the current view"</ListItem> }
    } else {
        view! { <div style="opacity: 0.5"><ListItem icon={icon(icons::REFRESH)}>"Update to the current view"</ListItem></div> }
    };
    let body = view! {
        {note(where_)}
        <List>
            <ListItem icon={icon(RightPanel::Route.icon())} onclick={l.menu_act(Act::Route(id))}>"Route here"</ListItem>
            {take_photo}
            {photo_entries}
            {update}
            <ListItem icon={icon(icons::RENAME)} onclick={start_rename(l, Item::Place(id), name)}>"Rename"</ListItem>
        </List>
        {move_to(l, listing, Item::Place(id))}
        <List>
            {confirm_entry(l, Armed::DeletePlace(id), icons::DELETE, "Delete this place", "Delete it — this cannot be undone", Act::DeletePlace(id))}
        </List>
    };
    actions_box(depth, body)
}

/// The panel: a header with ✕, the two add buttons, the photo in flight or
/// its failure, and the tree.
fn panel(l: &Live) -> View {
    let frame = if l.compact {
        "position: absolute; left: 0; right: 0; top: 0; bottom: 0; display: flex; flex-direction: column; background: var(--m-bg)"
    } else {
        "position: absolute; right: 0; top: 0; bottom: 0; width: 300px; display: flex; flex-direction: column; background: var(--m-bg); border-left: 1px solid var(--m-border)"
    };
    // Two words with their icons, side by side: the word is what tells them
    // apart. "Place" is greyed rather than hidden before the world anchor
    // exists — there is nowhere to save *yet*.
    let adds = {
        let l = l.clone();
        move || -> View {
            let has_place = l.status.get().has_place;
            let title = if has_place {
                "Save where you are now, with this view"
            } else {
                "No position yet — the world is still loading"
            };
            view! {
                <div style="display: flex; gap: 8px">
                    <Button icon={icon(icons::BOOKMARK_ADD)} disabled={!has_place} onclick={l.act(Act::NewPlace)} title={title}>"Place"</Button>
                    <Button icon={icon(icons::NEW_FOLDER)} onclick={l.act(Act::NewFolder)} title="New folder — move places into it to group them">"Folder"</Button>
                </div>
            }
        }
    };
    // The capture is only seen on the way back from the camera, which is
    // when "did that work?" is the question — and the only place it can be
    // abandoned: on Android nothing says the user pressed Back.
    let photo = {
        let l = l.clone();
        move || -> View {
            let s = l.status.get();
            let waiting: View = match s.capturing {
                Some(name) => {
                    let text = format!("Waiting for a photo for {name}…");
                    view! {
                        <div style="display: flex; align-items: center; gap: 10px; color: var(--m-text-dim); font-size: var(--m-font-size-sm)">
                            <Spinner size=Size::Sm />
                            <span style="flex: 1; min-width: 0">{text}</span>
                            <Button variant=Variant::Ghost size=Size::Sm onclick={l.act(Act::CancelPhoto)}>"Cancel"</Button>
                        </div>
                    }
                }
                None => View::Empty,
            };
            let failed: View = match s.photo_error {
                Some(msg) => {
                    let acts = l.acts.clone();
                    let dismiss: Rc<dyn Fn()> =
                        Rc::new(move || acts.borrow_mut().push(Act::DismissPhotoError));
                    view! { <Alert tone=Tone::Warning ondismiss={Some(dismiss)}>{msg}</Alert> }
                }
                None => View::Empty,
            };
            view! { {waiting}{failed} }
        }
    };
    let tree = {
        let l = l.clone();
        move || -> View {
            let listing = l.listing.get();
            let status = l.status.get();
            let thumbs = l.thumbs.get();
            let menu = l.menu.get();
            let editing = l.editing.get();
            if listing.rows.is_empty() {
                return note(
                    "No places yet — \"Place\" saves where you are now, and \
                     \"Folder\" makes somewhere to sort them into.",
                );
            }
            let mut out: Vec<View> = Vec::new();
            for (i, row) in listing.rows.iter().enumerate() {
                let first = i == 0;
                match row {
                    Row::Folder {
                        id,
                        depth,
                        name,
                        color,
                        collapsed,
                        count,
                    } => {
                        let item = Item::Folder(*id);
                        if editing == Some(item) {
                            out.push(rename_row(&l, item, *depth, first));
                            continue;
                        }
                        out.push(folder_row(
                            &l, first, *id, *depth, name, *color, *collapsed, *count,
                        ));
                        if menu == Some(item) {
                            out.push(folder_actions(&l, &listing, *id, *depth, name, *color));
                        }
                    }
                    Row::Place {
                        id,
                        depth,
                        name,
                        color,
                        lat,
                        lng,
                        altitude,
                        photo,
                    } => {
                        let item = Item::Place(*id);
                        if editing == Some(item) {
                            out.push(rename_row(&l, item, *depth, first));
                            continue;
                        }
                        out.push(place_row(
                            &l,
                            first,
                            *id,
                            *depth,
                            name,
                            *color,
                            photo.as_ref().map(|stem| thumbs.get(stem).map(|t| &**t)),
                        ));
                        if menu == Some(item) {
                            out.push(place_actions(
                                &l,
                                &listing,
                                &status,
                                *id,
                                *depth,
                                name,
                                (*lat, *lng, *altitude),
                                photo.is_some(),
                            ));
                        }
                    }
                }
            }
            view! { <List>{out}</List> }
        }
    };
    let close = l.act(Act::Close);
    view! {
        <div style={frame}>
            <div style="display: flex; align-items: center; gap: 12px; min-height: 56px; padding: 0 12px 0 20px; border-bottom: 1px solid var(--m-border)">
                <span style="color: var(--m-accent)">{icon(RightPanel::Places.icon())}</span>
                <h1 style="flex: 1; margin: 0; font-size: 19px; font-weight: 600; color: var(--m-text-strong)">"Places"</h1>
                <CloseButton onclick={close} />
            </div>
            <div style="flex: 1; min-height: 0; overflow-y: auto; padding: 20px 20px 28px; display: flex; flex-direction: column; gap: 20px">
                {adds}
                {photo}
                {tree}
            </div>
        </div>
    }
}
