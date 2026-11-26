//! The community screens on the component library, inside the Chats panel's
//! shell (`lib_chats`): Find (the directory), a community's Preview, an
//! Invite, Create and Edit, its page (the moderator's queue, the topics and
//! the public posts of `community_posts`, a thread, the post being written),
//! People, Post to and Activity — every screen egui's `communities::screen`
//! routes on `Communities::screen`.
//!
//! **One set of logic, two editions.** A tap is a `communities::Tap` or a
//! `community_posts::PostTap` (`ComposerAction` in the composer), done by
//! the same `communities::tap` / `community_posts::post_tap` egui's screens
//! call; what they answer (`communities::Out`) goes to
//! `chats::communities_out`, as egui's does. A field writes its value into
//! the world as it is typed (`onchange` → [`ChatsLive::later`]), and
//! [`mirror`] writes the world back into the field only where it moved
//! (cleared after a send), so the world is the one truth. What egui's
//! drawing did besides drawing — reading a community's posts the first time
//! its page shows, marking notices read while Activity shows — the mirror
//! does through the same functions (`community_posts::load`,
//! `read_notices`).
//!
//! **Elsewhere.** The picture picker and its upload are run by
//! `chats::drive_chats` (Choose sets `Communities::pick_picture`), and the
//! account-delete confirmation is `lib_dialogs`'.
//!
//! **Pictures** are `<img>`s from `data:` thumbnails (`pictures`,
//! `Communities::picture`): fetched from the picture store and cut to
//! twice the size they are drawn at on a worker, once, and published in
//! the `pics` signal as they are made. Until one is, a community's picture
//! is its initial (`lib_chats::disc`) and a post's photo or polaroid a tile
//! naming it. A post's photo is fitted into the card's width and egui's
//! 220 px height, its proportions kept rather than stretched as egui's
//! were.
//!
//! **What differs from egui's.** egui's context menu on a person is a row of
//! actions under the person, opened by a tap. A "tap again" confirmation is
//! a button that changes its words on the first press ([`confirm`]). The
//! public colour (`theme::PUBLIC`) is the library's info tone, the nearest
//! token. A screen's content is rebuilt when what it shows moves, so a field
//! inside a list (a post being edited or reported, the moderators' settings)
//! may lose the keyboard when the list changes under it; its text is kept.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::Arc;

use cartopolis_core::posts::{
    NoticeKind, PostDto, PostNotices, PostReport, PostThread, PublicInfo, ReportAction, SourceKind,
    WhoPosts, MAX_PICTURE_ALT, MAX_POST_TEXT,
};
use cartopolis_core::{
    CommunityDto, CommunityListing, CommunityMemberDto, CommunityRole, DirectoryOrder, JoinRule,
    ListingState,
};
// The library's, not Bevy's: an explicit import outranks the two globs.
use mithril::prelude::*;
use mithril::prelude::{Button, Event};
use uuid::Uuid;

use crate::systems::chats::{self, Actions, ChatsScreen};
use crate::systems::communities::{self as cm, Communities, CommunityOp, Out, Screen, Tap};
use crate::systems::community_posts::{self as cp, ComposerAction, PostTap, Tab};
use crate::systems::discs::ChatKind;
use crate::systems::icons;
use crate::systems::lib_chats::{
    back, body_style, disc, group, header, icon, note, picture_disc, ChatsLive, ChatsWorld,
};
use crate::systems::private_chat::channel_key;
use crate::systems::thumbs::{Fit, Thumb};

// ─── State ───────────────────────────────────────────────────────────────────

/// Which community screen shows: the router of [`screen`] follows it. A
/// change of route builds the screen again; anything else moves inside it.
#[derive(Clone, PartialEq, Default)]
enum Route {
    #[default]
    None,
    Find,
    Preview,
    Invite,
    Create,
    /// A community's page; `true` while it is the composer.
    Community(Uuid, bool),
    Edit(Uuid),
    People(Uuid),
    PostTo,
    Activity,
}

/// The starting or editing form, but its typed words.
#[derive(Clone, PartialEq, Default)]
struct FormV {
    /// Editing one this account owns: the danger zone shows.
    owner: bool,
    picture: bool,
    /// A picture is being picked or uploaded.
    busy: bool,
    /// This platform can pick a picture.
    can_pick: bool,
    note: Option<String>,
    /// Where it is pinned.
    place: Option<(f64, f64)>,
    /// The map knows where it is, so it can be pinned there.
    here: bool,
    /// How many topics the Edit form has.
    edits: usize,
}

/// A community's public half, as its page shows it.
#[derive(Clone, PartialEq, Default)]
struct PostsV {
    note: Option<String>,
    info: Option<PublicInfo>,
    feed: Vec<PostDto>,
    /// The open thread, once it arrived.
    thread: Option<PostThread>,
    /// The reply being answered, by its author's name.
    reply_to: Option<String>,
    /// A draft for this community waits behind Continue.
    draft_waiting: bool,
    on_map: bool,
    settings_open: bool,
    reports: Vec<PostReport>,
    menu_for: Option<Uuid>,
    editing: Option<Uuid>,
    reporting: Option<Uuid>,
    /// Posts opened past a warning or a sensitive picture.
    shown: Vec<Uuid>,
}

/// The post being written, but its words.
#[derive(Clone, PartialEq, Default)]
struct ComposeV {
    /// The place's name; `None` while the view is being taken.
    place: Option<String>,
    snapshot: bool,
    picture: bool,
    busy: bool,
    note: Option<String>,
    reach: String,
}

/// A community's page.
#[derive(Clone, PartialEq, Default)]
struct CommunityV {
    /// `None`: this account is no longer in it.
    c: Option<CommunityDto>,
    compose: Option<ComposeV>,
    reviewing: bool,
    /// Each topic's mute, in the topics' order.
    muted: Vec<bool>,
    posts: PostsV,
}

/// The people of a community.
#[derive(Clone, PartialEq)]
struct PeopleV {
    c: CommunityDto,
    /// This account's full address.
    me: String,
}

/// What the screen shows, but what is typed into it.
#[derive(Clone, PartialEq, Default)]
enum Page {
    #[default]
    Empty,
    Find(Vec<CommunityListing>),
    Preview(Option<CommunityListing>),
    Invite {
        code: String,
        listing: Option<CommunityListing>,
        refused: bool,
    },
    Form(FormV),
    Community(Box<CommunityV>),
    People(Option<Box<PeopleV>>),
    PostTo {
        place: Option<String>,
        /// Each community's id, name and picture.
        mine: Vec<(Uuid, String, Option<String>)>,
    },
    Activity(Option<PostNotices>),
}

/// A picture as drawn: its `data:` URL and the size it is drawn at, px.
#[derive(Clone, PartialEq)]
struct Shown {
    src: Arc<str>,
    w: f32,
    h: f32,
}

impl Shown {
    /// `t` fitted into a `max_w` × `max_h` box, its proportions kept.
    fn of(t: Thumb, max_w: f32, max_h: f32) -> Self {
        let (w, h) = t.fitted(max_w, max_h);
        Shown { src: t.url, w, h }
    }
}

/// The pictures the showing screen draws, once they are made
/// (`Communities::picture`): a picture still on its way is drawn as what
/// stood in for it before, a disc's initial or a tile naming it.
#[derive(Clone, PartialEq, Default)]
struct Pics {
    /// Community pictures, by hash and the disc's size in points.
    discs: BTreeMap<(String, u32), Arc<str>>,
    /// Post photos and polaroids, by hash, fitted to the feed.
    photos: BTreeMap<String, Shown>,
    polaroids: BTreeMap<String, Shown>,
    /// The post being written: its polaroid and its picture.
    draft_polaroid: Option<Shown>,
    draft_picture: Option<Shown>,
    /// The picture picked on the Create or Edit form, for its 48-point disc.
    form: Option<Arc<str>>,
}

impl Pics {
    /// A community picture for a `size`-point disc.
    fn disc(&self, hash: Option<&str>, size: u32) -> Option<&str> {
        let hash = hash?;
        self.discs.get(&(hash.to_string(), size)).map(|s| &**s)
    }
}

/// How high a post's photo is drawn at most, px: egui's 220.
const PHOTO_MAX_H: f32 = 220.0;
/// How wide a polaroid is drawn at most, px: egui's 420 in a post, 300
/// while it is being written. Its height follows its own proportions.
const POLAROID_MAX_W: f32 = 420.0;
const DRAFT_POLAROID_MAX_W: f32 = 300.0;
/// How high the picture of the post being written is drawn at most, px.
const DRAFT_PHOTO_MAX_H: f32 = 200.0;
/// The tallest a polaroid is let be drawn, px: a frame is about square.
const POLAROID_MAX_H: f32 = 520.0;

/// A thumbnail box for a picture drawn at most `w` × `h` points: twice
/// that in pixels, rounded up to 32 so a screen a few points wider does not
/// make a new thumbnail.
fn within(w: f32, h: f32) -> Fit {
    let px = |pts: f32| ((pts.max(16.0) * 2.0 / 32.0).ceil() as u32) * 32;
    Fit::Within(px(w), px(h))
}

/// The fields' models and the flags beside them, mirrored from the world.
struct Fields {
    query: Signal<String>,
    /// `DirectoryOrder` as [`ORDERS`]' index.
    order: Signal<usize>,
    invite: Signal<String>,
    join_message: Signal<String>,
    name: Signal<String>,
    about: Signal<String>,
    /// `JoinRule` as [`RULES`]' index.
    join: Signal<usize>,
    listed: Signal<bool>,
    members_start: Signal<bool>,
    place_name: Signal<String>,
    topics: Signal<String>,
    people: Signal<String>,
    /// The Edit form's topics: each one's name and whether it is archived.
    edits: RefCell<Vec<(Signal<String>, Signal<bool>)>>,
    add_people: Signal<String>,
    new_topic: Signal<String>,
    /// Posts (0) or Topics (1).
    tab: Signal<usize>,
    reply: Signal<String>,
    edit_text: Signal<String>,
    report_reason: Signal<String>,
    report_forward: Signal<bool>,
    handle: Signal<String>,
    follow: Signal<String>,
    /// `WhoPosts` as [`WHO`]'s index.
    who_posts: Signal<usize>,
    post_text: Signal<String>,
    warning_on: Signal<bool>,
    warning: Signal<String>,
    alt: Signal<String>,
    sensitive: Signal<bool>,
    /// What waits for something typed.
    create_off: Signal<bool>,
    topic_off: Signal<bool>,
    reply_off: Signal<bool>,
    save_off: Signal<bool>,
    follow_off: Signal<bool>,
    post_off: Signal<bool>,
    post_empty: Signal<bool>,
    sending: Signal<bool>,
    /// The community page's two forms.
    adding: Signal<bool>,
    starting: Signal<bool>,
    /// The invite link made here for the community showing.
    invite_link: Signal<Option<String>>,
    /// The post whose thread is open.
    thread: Signal<Option<Uuid>>,
}

/// The communities screens' own signals, kept in `ChatsLive::communities`.
#[derive(Clone)]
pub(crate) struct CommunitiesLive {
    route: Signal<Route>,
    page: Signal<Page>,
    f: Rc<Fields>,
    /// A confirmation pressed once, by its key: the second press does it.
    armed: Signal<Option<String>>,
    /// The person on People whose actions are open.
    person: Signal<Option<String>>,
    /// The pictures the screen draws, as they are made.
    pics: Signal<Pics>,
}

impl CommunitiesLive {
    /// Made when the panel mounts; [`mirror`] fills it before anything reads it.
    pub(crate) fn new(w: &ChatsWorld) -> Self {
        let _ = w;
        let s = || signal(String::new());
        let b = |v: bool| signal(v);
        CommunitiesLive {
            route: signal(Route::None),
            page: signal(Page::Empty),
            f: Rc::new(Fields {
                query: s(),
                order: signal(0),
                invite: s(),
                join_message: s(),
                name: s(),
                about: s(),
                join: signal(1),
                listed: b(true),
                members_start: b(true),
                place_name: s(),
                topics: s(),
                people: s(),
                edits: RefCell::new(Vec::new()),
                add_people: s(),
                new_topic: s(),
                tab: signal(0),
                reply: s(),
                edit_text: s(),
                report_reason: s(),
                report_forward: b(false),
                handle: s(),
                follow: s(),
                who_posts: signal(0),
                post_text: s(),
                warning_on: b(false),
                warning: s(),
                alt: s(),
                sensitive: b(false),
                create_off: b(true),
                topic_off: b(true),
                reply_off: b(true),
                save_off: b(true),
                follow_off: b(true),
                post_off: b(true),
                post_empty: b(true),
                sending: b(false),
                adding: b(false),
                starting: b(false),
                invite_link: signal(None),
                thread: signal(None),
            }),
            armed: signal(None),
            person: signal(None),
            pics: signal(Pics::default()),
        }
    }
}

const ORDERS: [(DirectoryOrder, &str); 3] = [
    (DirectoryOrder::Popular, "Popular"),
    (DirectoryOrder::Near, "Near you"),
    (DirectoryOrder::New, "New"),
];
const RULES: [(JoinRule, &str); 3] = [
    (JoinRule::Open, "Anyone"),
    (JoinRule::Ask, "Asks first"),
    (JoinRule::Invite, "Invite only"),
];
const WHO: [(WhoPosts, &str); 2] = [
    (WhoPosts::Members, "Every member"),
    (WhoPosts::Moderators, "Moderators only"),
];

/// `value`'s index in a segmented control's table.
fn index_of<T: PartialEq>(table: &[(T, &str)], value: &T) -> usize {
    table.iter().position(|(v, _)| v == value).unwrap_or(0)
}

// ─── The world → the signals ─────────────────────────────────────────────────

/// Write `value` into `s` only when it differs: a write re-runs every view
/// that reads it.
fn put<T: Clone + PartialEq + 'static>(s: &Signal<T>, value: T) {
    if s.get_untracked() != value {
        s.set(value);
    }
}

/// The route of what `Communities::screen` says, while the panel shows it.
fn route_of(cs: &Communities, showing: bool) -> Route {
    if !showing {
        return Route::None;
    }
    match &cs.screen {
        None => Route::None,
        Some(Screen::Find) => Route::Find,
        Some(Screen::Preview(_)) => Route::Preview,
        Some(Screen::Invite(_)) => Route::Invite,
        Some(Screen::Create) => Route::Create,
        Some(Screen::Community(id)) => Route::Community(*id, cp::composing(cs, *id)),
        Some(Screen::Edit(id)) => Route::Edit(*id),
        Some(Screen::People(id)) => Route::People(*id),
        Some(Screen::PostTo) => Route::PostTo,
        Some(Screen::Activity) => Route::Activity,
    }
}

/// The world → [`CommunitiesLive`], every frame the panel is mounted.
pub(crate) fn mirror(live: &ChatsLive, w: &mut ChatsWorld, now: u64) {
    let _ = now;
    let l = &live.communities;
    let showing = w.screen() == ChatsScreen::Communities;
    let (_, me) = w.me();
    let panel = panel_width(live, w);
    let cs: &mut Communities = &mut w.communities;
    let route = route_of(cs, showing);
    // What egui's screens do while they are drawn, done here while the
    // library's are: read a page's posts the first time it shows, offer a
    // fediverse address, mark notices read, leave a page that is gone.
    match &route {
        Route::Community(id, false) => {
            if let Some(c) = cs.get(*id).cloned().filter(|c| c.role.is_some()) {
                cp::load(cs, *id);
                if c.role.is_some_and(CommunityRole::moderates) {
                    cp::suggest_first_handle(cs, &c);
                }
            }
        }
        Route::Edit(id) | Route::People(id) if cs.get(*id).is_none() => cs.screen = None,
        Route::Activity => cp::read_notices(cs),
        _ => {}
    }
    if l.route.get_untracked() != route {
        put(&l.armed, None);
        put(&l.person, None);
    }
    put(&l.route, route.clone());
    put(&l.pics, pictures(&route, cs, panel));
    put(&l.page, page_of(&route, cs, &me));
    fields(&l.f, &route, cs);
}

/// The panel's width, px: the screen's on a phone, 360 beside the map
/// (`lib_chats::panel`). On the web's DOM a px is a point times the UI's
/// scale, as `lib_post_composer` places itself.
fn panel_width(live: &ChatsLive, w: &ChatsWorld) -> f32 {
    if !live.compact {
        return 360.0;
    }
    let k = if cfg!(all(feature = "ui-dom", target_arch = "wasm32")) {
        w.metrics.ui_scale()
    } else {
        1.0
    };
    w.metrics.viewport_pts().x * k
}

/// The pictures `route`'s screen draws, asked for every frame it shows:
/// the first ask fetches and cuts one on a worker, and it is drawn from the
/// frame it is ready. `panel` is the panel's width, px, which a post's
/// photo is fitted to.
fn pictures(route: &Route, cs: &mut Communities, panel: f32) -> Pics {
    let mut pics = Pics::default();
    // The body's 20 px a side (`body_style`), and a card's 16 and border.
    let body = (panel - 40.0).max(120.0);
    let card = (body - 34.0).max(100.0);
    let mut discs: Vec<(Option<String>, u32)> = Vec::new();
    let mut polaroids: Vec<String> = Vec::new();
    let mut photos: Vec<String> = Vec::new();
    match route {
        Route::Find => discs.extend(cs.directory.iter().map(|x| (x.picture.clone(), 40))),
        // The hero's disc (`hero`).
        Route::Preview | Route::Invite => {
            discs.extend(cs.directory.iter().map(|x| (x.picture.clone(), 64)));
        }
        // The form's disc follows the picture picked (`form_page`).
        Route::Create | Route::Edit(_) => {
            let hash = cs.draft.picture.clone();
            pics.form = cs
                .picture(hash.as_deref(), Fit::Square(48 * 2))
                .map(|t| t.url);
        }
        // The communities to post in (`post_to_page`).
        Route::PostTo => discs.extend(
            cs.list
                .iter()
                .filter(|c| c.role.is_some())
                .map(|c| (c.settings.picture.clone(), 32)),
        ),
        Route::Community(id, composing) => {
            // The page's head (`page_head`), or the hero of one this
            // account only asked to join.
            let c = cs.get(*id);
            let size = if c.is_some_and(|c| c.role.is_none()) {
                64
            } else {
                40
            };
            discs.push((c.and_then(|c| c.settings.picture.clone()), size));
            if *composing {
                let d = cs.posts.draft.as_ref();
                let snapshot = d.and_then(|d| d.snapshot.clone());
                let picture = d.and_then(|d| d.picture.clone());
                let pw = body.min(DRAFT_POLAROID_MAX_W);
                pics.draft_polaroid = cs
                    .picture(snapshot.as_deref(), within(pw, POLAROID_MAX_H))
                    .map(|t| Shown::of(t, pw, POLAROID_MAX_H));
                pics.draft_picture = cs
                    .picture(picture.as_deref(), within(body, DRAFT_PHOTO_MAX_H))
                    .map(|t| Shown::of(t, body, DRAFT_PHOTO_MAX_H));
            } else {
                let p = &cs.posts;
                let thread = p
                    .thread
                    .as_ref()
                    .filter(|t| Some(t.post.id) == p.thread_open);
                let shown = p
                    .feeds
                    .get(id)
                    .into_iter()
                    .flatten()
                    .chain(thread.map(|t| &t.post))
                    .chain(thread.into_iter().flat_map(|t| t.replies.iter()));
                for post in shown {
                    polaroids.extend(post.snapshot.clone());
                    photos.extend(post.picture.clone());
                    photos.extend(post.more_pictures.iter().map(|x| x.hash.clone()));
                }
            }
        }
        _ => {}
    }
    for (hash, size) in discs {
        let Some(hash) = hash else { continue };
        if let Some(t) = cs.picture(Some(&hash), Fit::Square(size * 2)) {
            pics.discs.insert((hash, size), t.url);
        }
    }
    let pw = card.min(POLAROID_MAX_W);
    for h in polaroids {
        if let Some(t) = cs.picture(Some(&h), within(pw, POLAROID_MAX_H)) {
            pics.polaroids.insert(h, Shown::of(t, pw, POLAROID_MAX_H));
        }
    }
    for h in photos {
        if let Some(t) = cs.picture(Some(&h), within(card, PHOTO_MAX_H)) {
            pics.photos.insert(h, Shown::of(t, card, PHOTO_MAX_H));
        }
    }
    pics
}

/// What `route`'s screen shows.
fn page_of(route: &Route, cs: &Communities, me: &str) -> Page {
    match route {
        Route::None => Page::Empty,
        Route::Find => Page::Find(cs.directory.clone()),
        Route::Preview => {
            let id = match cs.screen {
                Some(Screen::Preview(id)) => Some(id),
                _ => None,
            };
            Page::Preview(cs.directory.iter().find(|l| Some(l.id) == id).cloned())
        }
        Route::Invite => Page::Invite {
            code: match &cs.screen {
                Some(Screen::Invite(code)) => code.clone(),
                _ => String::new(),
            },
            listing: cs.directory.first().cloned(),
            refused: cs.invite_refused,
        },
        Route::Create => Page::Form(form_v(cs, None)),
        Route::Edit(id) => Page::Form(form_v(cs, Some(*id))),
        Route::Community(id, _) => Page::Community(Box::new(community_v(cs, *id))),
        Route::People(id) => Page::People(cs.get(*id).cloned().map(|c| {
            Box::new(PeopleV {
                c,
                me: me.to_string(),
            })
        })),
        Route::PostTo => Page::PostTo {
            place: cs.posts.from_view.as_ref().map(|p| {
                p.name
                    .clone()
                    .unwrap_or_else(|| format!("{:.5}, {:.5}", p.latitude, p.longitude))
            }),
            mine: cs
                .list
                .iter()
                .filter(|c| c.role.is_some())
                .map(|c| (c.id, c.settings.name.clone(), c.settings.picture.clone()))
                .collect(),
        },
        Route::Activity => Page::Activity(cs.posts.notices.clone()),
    }
}

fn form_v(cs: &Communities, edit: Option<Uuid>) -> FormV {
    FormV {
        owner: edit
            .and_then(|id| cs.get(id))
            .is_some_and(|c| c.role == Some(CommunityRole::Owner)),
        picture: cs.draft.picture.is_some(),
        busy: cm::picture_busy(cs),
        can_pick: crate::platform::photo::is_available(),
        note: cs.picture_note.clone(),
        place: cs.draft.place.as_ref().map(|p| (p.latitude, p.longitude)),
        here: cs.here.is_some(),
        edits: if edit.is_some() {
            cs.draft.topic_edits.len()
        } else {
            0
        },
    }
}

fn community_v(cs: &Communities, id: Uuid) -> CommunityV {
    let c = cs.get(id).cloned();
    let p = &cs.posts;
    let mut shown: Vec<Uuid> = p.shown.iter().copied().collect();
    shown.sort();
    let compose = cp::composing(cs, id).then(|| {
        let d = p.draft.as_ref();
        let info = p.info.get(&id);
        ComposeV {
            place: d.and_then(|d| d.place.as_ref()).map(|p| {
                p.name
                    .clone()
                    .unwrap_or_else(|| format!("{:.5}, {:.5}", p.latitude, p.longitude))
            }),
            snapshot: d.is_some_and(|d| d.snapshot.is_some()),
            picture: d.is_some_and(|d| d.picture.is_some()),
            busy: !cs.picture_for_post_idle() || p.snapshot_busy,
            note: cs.picture_note.clone(),
            reach: cp::reach_words(
                info.is_some_and(|i| i.federating),
                info.map_or(0, |i| i.followers),
            ),
        }
    });
    CommunityV {
        muted: c
            .as_ref()
            .map(|c| {
                c.topics
                    .iter()
                    .map(|t| cs.is_muted(&channel_key(&t.group)))
                    .collect()
            })
            .unwrap_or_default(),
        c,
        compose,
        reviewing: cs.reviewing,
        posts: PostsV {
            note: p.note.clone(),
            info: p.info.get(&id).cloned(),
            feed: p.feeds.get(&id).cloned().unwrap_or_default(),
            thread: p
                .thread
                .clone()
                .filter(|t| Some(t.post.id) == p.thread_open),
            reply_to: p.reply_to.as_ref().map(|(_, name)| name.clone()),
            draft_waiting: p.draft.as_ref().is_some_and(|d| d.community == id),
            on_map: p.on_map == Some(id),
            settings_open: p.settings_open,
            reports: p.reports.get(&id).cloned().unwrap_or_default(),
            menu_for: p.menu_for,
            editing: p.editing.as_ref().map(|(post, _)| *post),
            reporting: p.reporting.as_ref().map(|r| r.0),
            shown,
        },
    }
}

/// The fields and the flags beside them, where the world moved.
fn fields(f: &Fields, route: &Route, cs: &Communities) {
    put(&f.query, cs.query.clone());
    put(
        &f.order,
        index_of(&ORDERS, &cs.order.unwrap_or(DirectoryOrder::Popular)),
    );
    put(&f.invite, cs.invite_input.clone());
    put(&f.join_message, cs.join_message.clone());
    let d = &cs.draft;
    put(&f.name, d.name.clone());
    put(&f.about, d.about.clone());
    put(&f.join, index_of(&RULES, &d.join));
    put(&f.listed, d.listed);
    put(&f.members_start, d.members_start_topics);
    put(
        &f.place_name,
        d.place.as_ref().map(|p| p.name.clone()).unwrap_or_default(),
    );
    put(&f.topics, d.topics.clone());
    put(&f.people, d.people.clone());
    {
        let mut edits = f.edits.borrow_mut();
        while edits.len() < d.topic_edits.len() {
            edits.push((signal(String::new()), signal(false)));
        }
        for ((name, archived), t) in edits.iter().zip(&d.topic_edits) {
            put(name, t.name.clone());
            put(archived, t.archived);
        }
    }
    put(&f.add_people, cs.add_people.clone());
    put(&f.new_topic, cs.new_topic.clone());
    put(&f.create_off, !cm::create_ready(cs));
    put(&f.topic_off, cs.new_topic.trim().is_empty());
    put(&f.adding, cs.adding);
    put(&f.starting, cs.starting_topic);
    let p = &cs.posts;
    put(&f.tab, usize::from(p.tab == Tab::Topics));
    put(&f.reply, p.reply.clone());
    put(&f.reply_off, !cp::reply_ready(cs));
    let editing = p
        .editing
        .as_ref()
        .map(|(_, t)| t.clone())
        .unwrap_or_default();
    put(&f.save_off, editing.trim().is_empty());
    put(&f.edit_text, editing);
    let (reason, forward) = p
        .reporting
        .as_ref()
        .map(|(_, r, fw)| (r.clone(), *fw))
        .unwrap_or_default();
    put(&f.report_reason, reason);
    put(&f.report_forward, forward);
    put(&f.handle, p.handle.clone());
    put(&f.follow, p.follow.clone());
    put(&f.follow_off, !cp::follow_ready(cs));
    put(&f.thread, p.thread_open);
    let shown = match route {
        Route::Community(id, _) | Route::Edit(id) | Route::People(id) => Some(*id),
        _ => None,
    };
    let who = shown
        .and_then(|id| p.info.get(&id))
        .map_or(WhoPosts::Members, |i| i.settings.who_posts);
    put(&f.who_posts, index_of(&WHO, &who));
    put(
        &f.invite_link,
        cs.invite
            .as_ref()
            .filter(|(i, _)| Some(*i) == shown)
            .map(|(_, code)| crate::systems::place_link::invite_link(code)),
    );
    let draft = p.draft.as_ref();
    put(
        &f.post_text,
        draft.map(|d| d.text.clone()).unwrap_or_default(),
    );
    put(&f.warning_on, draft.is_some_and(|d| d.warning.is_some()));
    put(
        &f.warning,
        draft.and_then(|d| d.warning.clone()).unwrap_or_default(),
    );
    put(&f.alt, draft.map(|d| d.alt.clone()).unwrap_or_default());
    put(&f.sensitive, draft.is_some_and(|d| d.sensitive));
    put(&f.post_off, !cp::ready_to_post(cs));
    put(
        &f.post_empty,
        draft.is_none_or(|d| d.text.trim().is_empty()),
    );
    put(&f.sending, p.sending);
}

// ─── Taps ────────────────────────────────────────────────────────────────────

/// A community tap, done as egui's are, its answer handed to the panel.
fn run(w: &mut ChatsWorld<'_>, a: &mut Actions, t: Tap) {
    let mut out = Out::default();
    cm::tap(&mut w.communities, t, &mut out);
    chats::communities_out(out, &mut w.communities, a);
}

/// A tap in the public half, likewise.
fn run_post(w: &mut ChatsWorld<'_>, a: &mut Actions, t: PostTap) {
    let mut out = Out::default();
    cp::post_tap(&mut w.communities, t, &mut out);
    chats::communities_out(out, &mut w.communities, a);
}

fn tap(l: &ChatsLive, t: Tap) -> Option<Handler> {
    l.on(move |w, a| run(w, a, t.clone()))
}

fn ptap(l: &ChatsLive, t: PostTap) -> Option<Handler> {
    l.on(move |w, a| run_post(w, a, t.clone()))
}

fn compose(l: &ChatsLive, action: ComposerAction) -> Option<Handler> {
    l.on(move |w, _| cp::composer_act(&mut w.communities, action))
}

/// A field's `onchange`: the value, into the world, with the frame's taps.
fn on_value<T: Clone + 'static>(
    l: &ChatsLive,
    f: impl for<'w> Fn(&mut ChatsWorld<'w>, T) + 'static,
) -> Option<Rc<dyn Fn(&T)>> {
    let l = l.clone();
    let f = Rc::new(f);
    Some(Rc::new(move |v: &T| {
        let (v, f) = (v.clone(), f.clone());
        let go = l.later(move |w, _| f(w, v.clone()));
        go();
    }))
}

/// A choice's `onchange` (a segmented control's index): the tap it means.
fn on_pick<T: Clone + 'static>(
    l: &ChatsLive,
    f: impl Fn(&T) -> Tap + 'static,
) -> Option<Rc<dyn Fn(&T)>> {
    let l = l.clone();
    Some(Rc::new(move |v: &T| {
        let t = f(v);
        let go = l.later(move |w, a| run(w, a, t.clone()));
        go();
    }))
}

/// The same for a tap in the public half.
fn on_post_pick<T: Clone + 'static>(
    l: &ChatsLive,
    f: impl Fn(&T) -> PostTap + 'static,
) -> Option<Rc<dyn Fn(&T)>> {
    let l = l.clone();
    Some(Rc::new(move |v: &T| {
        let t = f(v);
        let go = l.later(move |w, a| run_post(w, a, t.clone()));
        go();
    }))
}

/// Enter in a field does `t`.
fn on_enter(l: &ChatsLive, t: Tap) -> Option<Handler> {
    let go = l.later(move |w, a| run(w, a, t.clone()));
    Some(Rc::new(move |e: &Event| {
        if e.key.as_deref() == Some("Enter") {
            go();
        }
    }))
}

// ─── Pieces ──────────────────────────────────────────────────────────────────

const CARD: &str = "display: flex; flex-direction: column; gap: 12px; padding: 16px; border: 1px solid var(--m-border); border-radius: var(--m-radius-lg); background: var(--m-surface)";
const ROW: &str = "display: flex; align-items: center; flex-wrap: wrap; gap: 8px";
const BLOCK: &str = "display: flex; flex-direction: column; gap: 8px";

/// The ✕ at a header's end: back to the map, as World does.
fn close(l: &ChatsLive) -> View {
    view! {
        <Button variant=Variant::Ghost icon={icon(icons::CLEAR)} icon_only=true onclick={tap(l, Tap::Close)} title="Close: back to the map" />
    }
}

/// A screen's header with back and ✕.
fn head(l: &ChatsLive, title: impl Into<String>) -> View {
    let title: String = title.into();
    header(back(tap(l, Tap::Back), "Back"), title.into_view(), close(l))
}

/// A line in the library's info tone: what is public.
fn public(glyph: &'static str, text: impl Into<String>) -> View {
    let text: String = text.into();
    view! {
        <p style="display: flex; align-items: flex-start; gap: 8px; margin: 0; padding-inline: 4px; color: var(--m-info); font-size: var(--m-font-size-sm)">
            {icon(glyph)}<span style="flex: 1; min-width: 0">{text}</span>
        </p>
    }
}

/// A line with an icon, quiet.
fn line(glyph: &'static str, text: impl Into<String>) -> View {
    let text: String = text.into();
    view! {
        <p style="display: flex; align-items: center; gap: 8px; margin: 0; padding-inline: 4px; color: var(--m-text-dim); font-size: var(--m-font-size-sm)">
            {icon(glyph)}<span style="flex: 1; min-width: 0">{text}</span>
        </p>
    }
}

/// Words that matter, in the strong text colour.
fn said(text: impl Into<String>) -> View {
    let text: String = text.into();
    view! { <p style="margin: 0; color: var(--m-text); white-space: pre-wrap">{text}</p> }
}

/// Waiting for the server.
fn busy(text: &'static str) -> View {
    view! {
        <div style="display: flex; align-items: center; gap: 12px; padding: 8px 4px; color: var(--m-text-dim)">
            <Spinner size=Size::Sm />{text}
        </div>
    }
}

/// Nothing here, and what would be.
fn empty(title: &'static str, text: &'static str) -> View {
    view! {
        <div style="display: flex; flex-direction: column; align-items: center; gap: 6px; padding: 24px 16px; text-align: center">
            <span style="color: var(--m-text-strong); font-weight: 600">{title}</span>
            <span style="color: var(--m-text-dim); font-size: var(--m-font-size-sm)">{text}</span>
        </div>
    }
}

/// A row with a tappable main part (disc, name, line) and controls of its
/// own at the end, which a `ListItem` cannot hold without nesting buttons.
fn split_row(
    open: impl Fn(&Event) + 'static,
    face: View,
    title: String,
    sub: String,
    end: View,
) -> View {
    view! {
        <div style="display: flex; align-items: center; gap: 8px; padding: 8px 8px 8px 12px; border: 1px solid var(--m-border); border-radius: var(--m-radius-lg); background: var(--m-surface)">
            <button style="flex: 1; min-width: 0; display: flex; align-items: center; gap: 12px; margin: 0; padding: 4px 0; border: 0; background: none; color: var(--m-text); font: inherit; text-align: start; cursor: pointer" on:click={open}>
                {face}
                <span style="flex: 1; min-width: 0; display: flex; flex-direction: column; gap: 2px">
                    <span style="color: var(--m-text-strong); font-weight: 600; white-space: nowrap; text-overflow: ellipsis; overflow: hidden">{title}</span>
                    <span style="color: var(--m-text-dim); font-size: var(--m-font-size-sm)">{sub}</span>
                </span>
            </button>
            {end}
        </div>
    }
}

/// A post's photo (`rounded`, as egui's 10 px corners) or polaroid (its
/// frame square-cornered, as it was made), at the size it was fitted to.
/// `alt` is what it shows, in words.
fn shown_picture(s: &Shown, alt: &str, rounded: bool) -> View {
    picture_box(&s.src, s.w, s.h, rounded, alt, None)
}

/// A picture `w` × `h` px from its `data:` URL, with a remove in its
/// corner when `remove` is given: the picture on a post being written
/// (here and in `lib_post_composer`).
pub(crate) fn picture_box(
    src: &str,
    w: f32,
    h: f32,
    rounded: bool,
    alt: &str,
    remove: Option<Handler>,
) -> View {
    let radius = if rounded {
        "border-radius: var(--m-radius); "
    } else {
        ""
    };
    let img_style =
        format!("width: {w:.0}px; height: {h:.0}px; {radius}background: var(--m-surface-sunken)");
    let (src, alt) = (src.to_string(), alt.to_string());
    let img = view! { <img src={src} alt={alt.clone()} title={alt} style={img_style} /> };
    let remove: View = match remove {
        Some(go) => view! {
            <div style="position: absolute; top: 6px; right: 6px">
                <Button variant=Variant::Secondary size=Size::Sm icon={icon(icons::DELETE)} icon_only=true onclick={Some(go)} title="Remove it from the post" />
            </div>
        },
        None => View::Empty,
    };
    let frame = format!(
        "position: relative; flex: none; align-self: flex-start; width: {w:.0}px; height: {h:.0}px"
    );
    view! { <div style={frame}>{img}{remove}</div> }
}

/// A picture still on its way (or one that did not come): a tile that
/// names it.
fn picture_tile(glyph: &'static str, what: String) -> View {
    view! {
        <div style="display: flex; align-items: center; gap: 12px; min-height: 64px; padding: 12px 16px; border-radius: var(--m-radius); background: var(--m-surface-sunken); color: var(--m-text-dim)">
            <span style="color: var(--m-accent)">{icon(glyph)}</span>
            <span style="flex: 1; min-width: 0">{what}</span>
        </div>
    }
}

/// A button that asks to be pressed again before it does `go`: egui's
/// `theme::confirm_button`. `key` tells the confirmations on a screen apart.
fn confirm(
    l: &ChatsLive,
    key: String,
    label: String,
    again: &'static str,
    go: impl for<'w> Fn(&mut ChatsWorld<'w>, &mut Actions) + 'static,
) -> View {
    let armed = l.communities.armed.clone();
    let go = l.later(go);
    let press: Handler = {
        let (armed, key) = (armed.clone(), key.clone());
        Rc::new(move |_: &Event| {
            if armed.get_untracked().as_deref() == Some(key.as_str()) {
                armed.set(None);
                go();
            } else {
                armed.set(Some(key.clone()));
            }
        })
    };
    (move || -> View {
        let on = armed.get().as_deref() == Some(key.as_str());
        let (variant, words) = if on {
            (Variant::Danger, again.to_string())
        } else {
            (Variant::Secondary, label.clone())
        };
        view! { <Button variant={variant} size=Size::Sm onclick={Some(press.clone())}>{words}</Button> }
    })
    .into_view()
}

/// A segmented control over a table's labels, its value an index.
fn segmented(
    model: &Signal<usize>,
    labels: &[&str],
    onchange: Option<Rc<dyn Fn(&usize)>>,
    id: &'static str,
) -> View {
    let options: Vec<Opt<usize>> = labels
        .iter()
        .enumerate()
        .map(|(i, label)| Opt::new(i, *label))
        .collect();
    view! { <FieldSegmented model={model.clone()} options={options} onchange={onchange} id={id} /> }
}

/// A community's picture, its name, what it is for and a line of facts.
fn hero(
    name: &str,
    picture: Option<&str>,
    about: &str,
    facts: String,
    place: Option<String>,
) -> View {
    let about: View = if about.trim().is_empty() {
        View::Empty
    } else {
        let about = about.trim().to_string();
        view! { <p style="margin: 0; color: var(--m-text-dim)">{about}</p> }
    };
    let place: View = match place {
        Some(p) => line(icons::PIN, p),
        None => View::Empty,
    };
    let title = name.to_string();
    view! {
        <div style="display: flex; flex-direction: column; align-items: center; gap: 8px; padding-top: 4px; text-align: center">
            {picture_disc(name, ChatKind::Community, 64, picture)}
            <h2 style="margin: 0; font-size: 19px; font-weight: 600; color: var(--m-text-strong)">{title}</h2>
            {about}
            <span style="color: var(--m-text-dim); font-size: var(--m-font-size-sm)">{facts}</span>
            {place}
        </div>
    }
}

/// A listing's hero, its picture from `pics`.
fn listing_hero(l: &CommunityListing, pics: &Pics) -> View {
    hero(
        &l.name,
        pics.disc(l.picture.as_deref(), 64),
        &l.about,
        format!("{} people · {}", l.members, cm::rule_word(l.join)),
        l.place.as_ref().map(cm::place_words),
    )
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

// ─── The router ──────────────────────────────────────────────────────────────

/// Whichever community screen is up.
pub(crate) fn screen(live: &ChatsLive) -> View {
    let l = live.clone();
    (move || -> View {
        match l.communities.route.get() {
            Route::None => View::Empty,
            Route::Find => find_page(&l),
            Route::Preview => preview_page(&l),
            Route::Invite => invite_page(&l),
            Route::Create => form_page(&l, None),
            Route::Edit(id) => form_page(&l, Some(id)),
            Route::Community(id, true) => compose_page(&l, id),
            Route::Community(id, false) => community_page(&l, id),
            Route::People(id) => people_page(&l, id),
            Route::PostTo => post_to_page(&l),
            Route::Activity => activity_page(&l),
        }
    })
    .into_view()
}

/// A part of the screen that follows the page.
fn part(l: &ChatsLive, f: impl Fn(&ChatsLive, &Page) -> View + 'static) -> View {
    let l = l.clone();
    (move || -> View { f(&l, &l.communities.page.get()) }).into_view()
}

// ─── Find ────────────────────────────────────────────────────────────────────

fn find_page(l: &ChatsLive) -> View {
    let f = &l.communities.f;
    let query = on_value(l, |w, v: String| w.communities.query = v);
    // Enter asks in the order chosen, which the world holds.
    let enter = {
        let go = l.later(|w, a| {
            let order = w.communities.order.unwrap_or(DirectoryOrder::Popular);
            run(w, a, Tap::Find(order));
        });
        Some(Rc::new(move |e: &Event| {
            if e.key.as_deref() == Some("Enter") {
                go();
            }
        }) as Handler)
    };
    let order = on_pick(l, |i: &usize| Tap::Find(ORDERS[(*i).min(2)].0));
    let labels: Vec<&str> = ORDERS.iter().map(|(_, t)| *t).collect();
    let near = {
        let order = f.order.clone();
        move || -> View {
            if ORDERS[order.get().min(2)].0 == DirectoryOrder::Near {
                note("Communities pinned to a place, nearest to where the map is first.")
            } else {
                View::Empty
            }
        }
    };
    let listings = part(l, |l, page| {
        let Page::Find(listings) = page else {
            return View::Empty;
        };
        if listings.is_empty() {
            return empty(
                "No communities found",
                "Only communities that chose to be listed appear here. Start one with the + on the chat list.",
            );
        }
        let rows: Vec<View> = listings.iter().map(|x| listing_row(l, x)).collect();
        view! { <List>{rows}</List> }
    });
    let invite = on_value(l, |w, v: String| w.communities.invite_input = v);
    let body = body_style(l);
    view! {
        {head(l, "Find a community")}
        <div style={body}>
            <FieldText model={f.query.clone()} r#type=TextType::Search placeholder="Search names and descriptions" autocomplete="off" prefix={icon(icons::SEARCH)} onkeydown={enter} onchange={query} id="communities-find" />
            {segmented(&f.order, &labels, order, "communities-order")}
            {near}
            {listings}
            <div style={BLOCK}>
                {group("Have an invite?")}
                <div style="display: flex; align-items: flex-start; gap: 8px">
                    <div style="flex: 1; min-width: 0">
                        <FieldText model={f.invite.clone()} placeholder="Paste the link or the code" autocomplete="off" onkeydown={on_enter(l, Tap::OpenTypedInvite)} onchange={invite} id="communities-invite" />
                    </div>
                    <Button onclick={tap(l, Tap::OpenTypedInvite)}>"Open"</Button>
                </div>
            </div>
        </div>
    }
}

/// One listing in Find: its disc, name, what it is for, size and distance,
/// and where this account stands with it.
fn listing_row(l: &ChatsLive, x: &CommunityListing) -> View {
    let mut sub = format!(
        "{} · {}",
        if x.about.is_empty() {
            "No description"
        } else {
            &x.about
        },
        plural(x.members as usize, "person", "people"),
    );
    if let Some(d) = x.distance_m {
        sub = format!("{} · {sub}", cm::distance_words(d));
    }
    let (tone, word) = match x.mine {
        ListingState::Member => (Tone::Accent, "You're in"),
        ListingState::Asked => (Tone::Neutral, "Asked"),
        ListingState::Refused => (Tone::Danger, "Can't join"),
        ListingState::Outside => (
            match x.join {
                JoinRule::Open => Tone::Success,
                JoinRule::Ask => Tone::Warning,
                JoinRule::Invite => Tone::Neutral,
            },
            cm::rule_word(x.join),
        ),
    };
    let name = x.name.clone();
    let face = picture_disc(
        &x.name,
        ChatKind::Community,
        40,
        l.communities.pics.get().disc(x.picture.as_deref(), 40),
    );
    view! {
        <ListItem icon={face} sub={sub.into_view()} end={view! { <Badge tone={tone}>{word}</Badge> }} onclick={tap(l, Tap::Listing(x.id, x.mine))}>{name}</ListItem>
    }
}

// ─── Preview and Invite ──────────────────────────────────────────────────────

fn preview_page(l: &ChatsLive) -> View {
    let content = part(l, |l, page| {
        let Page::Preview(listing) = page else {
            return View::Empty;
        };
        let Some(x) = listing else {
            return note("This community is no longer listed.");
        };
        preview_body(l, x)
    });
    let body = body_style(l);
    view! {
        {head(l, "Community")}
        <div style={body}>{content}</div>
    }
}

fn preview_body(l: &ChatsLive, x: &CommunityListing) -> View {
    let id = x.id;
    let topics: View = if x.topics.is_empty() {
        View::Empty
    } else {
        let badges: Vec<View> = x
            .topics
            .iter()
            .map(|t| {
                let t = format!("# {t}");
                view! { <Badge>{t}</Badge> }
            })
            .collect();
        view! {
            <div style={BLOCK}>
                {group("Topics")}
                <div style={ROW}>{badges}</div>
            </div>
        }
    };
    let moderators: Vec<View> = x
        .moderators
        .iter()
        .map(|m| {
            let who = cm::shown(&m.address, "");
            view! {
                <ListItem icon={disc(&who, ChatKind::Private, 32)} sub={cm::role_word(m.role).into_view()} chevron=true onclick={tap(l, Tap::Message(m.address.clone()))}>{who.clone()}</ListItem>
            }
        })
        .collect();
    let act: View = match (x.mine, x.join) {
        (ListingState::Member, _) => view! {
            <Button variant=Variant::Primary onclick={tap(l, Tap::Show(id))}>"Open"</Button>
        },
        (ListingState::Asked, _) => view! {
            {note("You asked to join. A moderator will answer.")}
            <Button onclick={tap(l, Tap::Withdraw(id, true))}>"Withdraw the request"</Button>
        },
        (ListingState::Refused, _) => note("You can't ask to join this community again yet."),
        (ListingState::Outside, JoinRule::Invite) => {
            note("Only people a moderator adds can join this community.")
        }
        (ListingState::Outside, JoinRule::Open) => view! {
            <Button variant=Variant::Primary onclick={tap(l, Tap::Join(id))}>"Join"</Button>
        },
        (ListingState::Outside, JoinRule::Ask) => {
            let message = on_value(l, |w, v: String| w.communities.join_message = v);
            view! {
                <div style={CARD}>
                    <span style="color: var(--m-text-strong); font-weight: 600">"A moderator confirms new members."</span>
                    {note("Say who you are; they see your name and this message, nothing else.")}
                    <FieldText model={l.communities.f.join_message.clone()} placeholder="A line to the moderators (optional)" autocomplete="off" onchange={message} id="communities-join-message" />
                </div>
                <Button variant=Variant::Primary onclick={tap(l, Tap::AskToJoin(id))}>"Ask to join"</Button>
            }
        }
    };
    view! {
        {listing_hero(x, &l.communities.pics.get())}
        {topics}
        <div style={BLOCK}>
            {group("Moderators")}
            <List>{moderators}</List>
        </div>
        <div style="display: flex; flex-direction: column; gap: 12px">{act}</div>
    }
}

fn invite_page(l: &ChatsLive) -> View {
    let content = part(l, |l, page| {
        let Page::Invite {
            code,
            listing,
            refused,
        } = page
        else {
            return View::Empty;
        };
        let Some(x) = listing else {
            return if *refused {
                note("This invite does not work any more. Ask whoever sent it for a new one.")
            } else {
                busy("Looking the invite up…")
            };
        };
        let act: View = if x.mine == ListingState::Member {
            view! { <Button variant=Variant::Primary onclick={tap(l, Tap::Show(x.id))}>"Open"</Button> }
        } else {
            view! {
                {note("You were invited, so you join straight away.")}
                <Button variant=Variant::Primary onclick={tap(l, Tap::JoinInvite(code.clone(), x.id))}>"Join"</Button>
            }
        };
        view! {
            {listing_hero(x, &l.communities.pics.get())}
            <div style="display: flex; flex-direction: column; gap: 12px">{act}</div>
        }
    });
    let body = body_style(l);
    view! {
        {head(l, "Invitation")}
        <div style={body}>{content}</div>
    }
}

// ─── Create and Edit ─────────────────────────────────────────────────────────

/// Starting a community (`edit` `None`) or editing one: its settings, its
/// picture and place, then the first topics and people, or each topic.
fn form_page(l: &ChatsLive, edit: Option<Uuid>) -> View {
    let f = &l.communities.f;
    let name = on_value(l, |w, v: String| w.communities.draft.name = v);
    let about = on_value(l, |w, v: String| w.communities.draft.about = v);
    let join = on_value(l, |w, i: usize| {
        w.communities.draft.join = RULES[i.min(2)].0
    });
    let listed = on_value(l, |w, v: bool| w.communities.draft.listed = v);
    let members_start = on_value(l, |w, v: bool| w.communities.draft.members_start_topics = v);
    let rules: Vec<&str> = RULES.iter().map(|(_, t)| *t).collect();
    let rule_note = {
        let join = f.join.clone();
        move || -> View {
            note(match RULES[join.get().min(2)].0 {
                JoinRule::Open => "Anyone who finds it joins straight away.",
                JoinRule::Ask => "Anyone may ask; a moderator confirms.",
                JoinRule::Invite => "Only people a moderator adds.",
            })
        }
    };
    // The disc follows the name as it is typed, and shows the picture once
    // one is picked: only it is drawn again.
    let face = {
        let name = f.name.clone();
        let pics = l.communities.pics.clone();
        move || -> View {
            let n = name.get();
            let picture = pics.get().form;
            picture_disc(
                if n.trim().is_empty() { "?" } else { n.trim() },
                ChatKind::Community,
                48,
                picture.as_deref(),
            )
        }
    };
    let picture = part(l, |l, page| {
        let Page::Form(v) = page else {
            return View::Empty;
        };
        let choose: View = if v.can_pick {
            view! { <Button size=Size::Sm icon={icon(icons::CAMERA)} disabled={v.busy} onclick={tap(l, Tap::ChoosePicture)}>"Choose"</Button> }
        } else {
            View::Empty
        };
        let remove: View = if v.picture {
            view! { <Button size=Size::Sm variant=Variant::Ghost disabled={v.busy} onclick={tap(l, Tap::RemovePicture)}>"Remove"</Button> }
        } else {
            View::Empty
        };
        let has: View = if v.picture {
            note("A picture is set. It shows in the directory.")
        } else {
            View::Empty
        };
        let n: View = match &v.note {
            Some(n) => note(n.clone()),
            None => View::Empty,
        };
        view! {
            <div style={ROW}>{choose}{remove}</div>
            {has}
            {n}
        }
    });
    let place = part(l, |l, page| {
        let Page::Form(v) = page else {
            return View::Empty;
        };
        match v.place {
            Some((lat, lng)) => {
                let name = on_value(l, |w, v: String| {
                    if let Some(p) = w.communities.draft.place.as_mut() {
                        p.name = v;
                    }
                });
                view! {
                    <div style="display: flex; align-items: flex-start; gap: 8px">
                        <div style="flex: 1; min-width: 0">
                            <FieldText model={l.communities.f.place_name.clone()} placeholder="Name the place" autocomplete="off" prefix={icon(icons::PIN)} onchange={name} id="communities-place-name" />
                        </div>
                        <Button variant=Variant::Ghost onclick={tap(l, Tap::RemovePlace)}>"Remove"</Button>
                    </div>
                    {note(format!("Pinned at {lat:.4}, {lng:.4}. Near you lists it for people around here."))}
                }
            }
            None => {
                let pin: View = if v.here {
                    view! { <div style={ROW}><Button size=Size::Sm icon={icon(icons::PIN)} onclick={tap(l, Tap::PinPlace)}>"Pin it where the map is"</Button></div> }
                } else {
                    View::Empty
                };
                view! {
                    {pin}
                    {note("A community about somewhere can be pinned there, so people nearby find it.")}
                }
            }
        }
    });
    let rest: View = match edit {
        None => {
            let topics = on_value(l, |w, v: String| w.communities.draft.topics = v);
            let people = on_value(l, |w, v: String| w.communities.draft.people = v);
            view! {
                <div style={BLOCK}>
                    {group("Topics to start with")}
                    <FieldText model={f.topics.clone()} placeholder="general, meet-ups (separated by commas)" autocomplete="off" onchange={topics} id="communities-topics" />
                </div>
                <div style={BLOCK}>
                    {group("People")}
                    <FieldText model={f.people.clone()} placeholder="Usernames, separated by commas (optional)" autocomplete="off" onchange={people} id="communities-people" />
                </div>
                <Button variant=Variant::Primary disabled={f.create_off.clone()} onclick={tap(l, Tap::Create)} title="Give it a name first">"Start the community"</Button>
                {note("What is said in a community is end-to-end encrypted, like every private chat.")}
            }
        }
        Some(id) => {
            let topics = part(l, |l, page| {
                let Page::Form(v) = page else {
                    return View::Empty;
                };
                if v.edits == 0 {
                    return View::Empty;
                }
                let edits = l.communities.f.edits.borrow();
                let rows: Vec<View> = edits
                    .iter()
                    .take(v.edits)
                    .enumerate()
                    .map(|(i, (name, archived))| {
                        let rename = on_value(l, move |w, v: String| {
                            if let Some(t) = w.communities.draft.topic_edits.get_mut(i) {
                                t.name = v;
                            }
                        });
                        let archive = on_value(l, move |w, v: bool| {
                            if let Some(t) = w.communities.draft.topic_edits.get_mut(i) {
                                t.archived = v;
                            }
                        });
                        view! {
                            <div style="display: flex; align-items: center; gap: 12px">
                                <div style="flex: 1; min-width: 0">
                                    <FieldText model={name.clone()} autocomplete="off" prefix={"#".into_view()} onchange={rename} />
                                </div>
                                <FieldCheckbox model={archived.clone()} label="Archived" onchange={archive} />
                            </div>
                        }
                    })
                    .collect();
                view! {
                    <div style={BLOCK}>
                        {group("Topics")}
                        {rows}
                    </div>
                }
            });
            let danger = part(l, move |l, page| {
                let Page::Form(v) = page else {
                    return View::Empty;
                };
                if !v.owner {
                    return View::Empty;
                }
                view! {
                    <div style={BLOCK}>
                        {group("Danger")}
                        <div style={ROW}>
                            {confirm(l, format!("delete {id}"), "Delete the community".into(), "Delete it and every topic: tap again", move |w, a| run(w, a, Tap::Delete(id)))}
                        </div>
                    </div>
                }
            });
            view! {
                {topics}
                <Button variant=Variant::Primary onclick={tap(l, Tap::Save(id))}>"Save"</Button>
                {danger}
            }
        }
    };
    let title = if edit.is_some() {
        "Edit community"
    } else {
        "New community"
    };
    let body = body_style(l);
    view! {
        {head(l, title)}
        <div style={body}>
            <FieldText model={f.name.clone()} label="Name" placeholder="Garage 44" autocomplete="off" onchange={name} id="communities-name" />
            <FieldText model={f.about.clone()} label="What is it for? (optional)" placeholder="Building cartopolis together" autocomplete="off" onchange={about} id="communities-about" />
            <div style={BLOCK}>
                {group("Who can join")}
                {segmented(&f.join, &rules, join, "communities-join")}
                {rule_note}
            </div>
            <div style={BLOCK}>
                <FieldCheckbox model={f.listed.clone()} label="Show in Find a community" onchange={listed} id="communities-listed" />
                {note("Listed: its name, description, topic names, moderators and size are public on this server.")}
                <FieldCheckbox model={f.members_start.clone()} label="Members may start topics" onchange={members_start} id="communities-members-start" />
            </div>
            <div style={BLOCK}>
                {group("Picture")}
                <div style="display: flex; align-items: center; gap: 16px">
                    {face}
                    <div style="flex: 1; min-width: 0; display: flex; flex-direction: column; gap: 8px">{picture}</div>
                </div>
            </div>
            <div style={BLOCK}>
                {group("Place")}
                {place}
            </div>
            {rest}
        </div>
    }
}

// ─── A community's page ──────────────────────────────────────────────────────

/// The community and this account's role in it, when it is a member.
fn member(page: &Page) -> Option<(&CommunityV, &CommunityDto, CommunityRole)> {
    let Page::Community(v) = page else {
        return None;
    };
    let v: &CommunityV = v;
    let c = v.c.as_ref()?;
    Some((v, c, c.role?))
}

/// A community's page: its head, what it is for, the actions, the two
/// forms, a moderator's queue, the two halves and leaving.
fn community_page(l: &ChatsLive, id: Uuid) -> View {
    let top = part(l, |l, page| {
        let Page::Community(v) = page else {
            return header(back(tap(l, Tap::Back), "Back"), View::Empty, close(l));
        };
        match v.c.as_ref() {
            Some(c) if c.role.is_some() => page_head(l, c),
            _ => header(back(tap(l, Tap::Back), "Back"), View::Empty, close(l)),
        }
    });
    let upper = part(l, move |l, page| {
        let Page::Community(v) = page else {
            return View::Empty;
        };
        let Some(c) = v.c.as_ref() else {
            return note("You are no longer in this community.");
        };
        let Some(role) = c.role else {
            let pics = l.communities.pics.get();
            let picture = pics.disc(c.settings.picture.as_deref(), 64);
            return view! {
                {hero(&c.settings.name, picture, &c.settings.about, String::new(), None)}
                {note("You asked to join. A moderator will answer.")}
                <Button onclick={tap(l, Tap::Withdraw(id, false))}>"Withdraw the request"</Button>
            };
        };
        upper_part(l, v, c, role)
    });
    let two_forms = forms(l, id);
    let waiting = part(l, |l, page| match member(page) {
        Some((v, c, role)) if role.moderates() => queue(l, v, c),
        _ => View::Empty,
    });
    let halves = part(l, |l, page| match member(page) {
        Some((_, c, _)) => {
            let labels = [
                "Posts · public".to_string(),
                format!("Topics · {}", c.topics.len()),
            ];
            let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
            let change = on_post_pick(l, |i: &usize| {
                PostTap::Tab(if *i == 1 { Tab::Topics } else { Tab::Posts })
            });
            segmented(&l.communities.f.tab, &labels, change, "community-tabs")
        }
        None => View::Empty,
    });
    let tab = {
        let l = l.clone();
        move || -> View {
            if l.communities.f.tab.get() == 1 {
                part(&l, |l, page| match member(page) {
                    Some((v, c, _)) => topics_list(l, v, c),
                    None => View::Empty,
                })
            } else {
                posts_half(&l, id)
            }
        }
    };
    let foot = part(l, move |l, page| match member(page) {
        Some((_, _, CommunityRole::Owner)) => {
            note("You own this community. Hand it over on People before leaving.")
        }
        Some(_) => view! {
            <div style={ROW}>
                {confirm(l, format!("leave {id}"), "Leave the community".into(), "Leave: tap again", move |w, a| run(w, a, Tap::Leave(id)))}
            </div>
        },
        None => View::Empty,
    });
    // The posts and topics only for a member; the halves' parts say nothing
    // otherwise.
    let body = body_style(l);
    view! {
        {top}
        <div style={body}>
            {upper}
            {two_forms}
            {waiting}
            {halves}
            {tab}
            {foot}
        </div>
    }
}

/// The page's head: back, the community's disc, its name over a line of
/// facts, and the ✕ — one row, as the communities mockup draws it.
fn page_head(l: &ChatsLive, c: &CommunityDto) -> View {
    let place = c
        .settings
        .place
        .as_ref()
        .map(|p| format!(" · {}", cm::place_words(p)))
        .unwrap_or_default();
    let facts = format!(
        "{} · {} · {}{place}",
        plural(c.members.len(), "person", "people"),
        cm::rule_word(c.settings.join),
        if c.settings.listed {
            "listed"
        } else {
            "unlisted"
        }
    );
    let name = c.settings.name.clone();
    let face = picture_disc(
        &c.settings.name,
        ChatKind::Community,
        40,
        l.communities
            .pics
            .get()
            .disc(c.settings.picture.as_deref(), 40),
    );
    let lead = view! {
        {back(tap(l, Tap::Back), "Back")}
        {face}
    };
    let title = view! {
        <span style="display: flex; flex-direction: column; gap: 2px; min-width: 0">
            <span style="overflow: hidden; text-overflow: ellipsis; white-space: nowrap">{name}</span>
            <span style="overflow: hidden; text-overflow: ellipsis; white-space: nowrap; color: var(--m-text-dim); font-size: var(--m-font-size-sm); font-weight: 400">{facts}</span>
        </span>
    };
    header(lead, title, close(l))
}

/// What it is for and the row of actions.
fn upper_part(l: &ChatsLive, v: &CommunityV, c: &CommunityDto, role: CommunityRole) -> View {
    let id = c.id;
    let moderates = role.moderates();
    let about: View = if c.settings.about.trim().is_empty() {
        View::Empty
    } else {
        note(c.settings.about.trim().to_string())
    };
    let _ = v;
    let f = &l.communities.f;
    let toggle = |on: &Signal<bool>, glyph: &'static str, word: &'static str, t: Tap| -> View {
        let (on, l) = (on.clone(), l.clone());
        (move || -> View {
            let variant = if on.get() {
                Variant::Primary
            } else {
                Variant::Secondary
            };
            view! { <Button variant={variant} size=Size::Sm icon={icon(glyph)} onclick={tap(&l, t.clone())}>{word}</Button> }
        })
        .into_view()
    };
    let invite = if moderates {
        toggle(&f.adding, icons::ADD, "Invite", Tap::ToggleAdding)
    } else {
        View::Empty
    };
    let topic = if moderates || c.settings.members_start_topics {
        toggle(&f.starting, icons::ADD, "New topic", Tap::ToggleTopic)
    } else {
        View::Empty
    };
    let people = format!("People {}", c.members.len());
    let edit: View = if moderates {
        view! { <Button size=Size::Sm icon={icon(icons::SETTINGS)} onclick={tap(l, Tap::Edit(id))}>"Edit"</Button> }
    } else {
        View::Empty
    };
    view! {
        {about}
        <div style={ROW}>
            {invite}
            {topic}
            <Button size=Size::Sm icon={icon(icons::GROUP)} onclick={tap(l, Tap::People(id))}>{people}</Button>
            {edit}
        </div>
    }
}

/// Invite and New topic, each shown while its action is on. Their fields
/// stay put while the page moves under them.
fn forms(l: &ChatsLive, id: Uuid) -> View {
    let adding = {
        let l = l.clone();
        move || -> View {
            let f = &l.communities.f;
            if !f.adding.get() {
                return View::Empty;
            }
            let typed = on_value(&l, |w, v: String| w.communities.add_people = v);
            let link = {
                let l = l.clone();
                move || -> View {
                    match l.communities.f.invite_link.get() {
                        Some(link) => view! {
                            <span style="color: var(--m-text-strong); word-break: break-all">{link.clone()}</span>
                            <div style={ROW}>
                                <Button size=Size::Sm icon={icon(icons::COPY)} onclick={tap(&l, Tap::CopyInvite(link.clone()))}>"Copy"</Button>
                                <Button size=Size::Sm variant=Variant::Ghost onclick={tap(&l, Tap::Ops(vec![CommunityOp::CreateInvite(id)]))}>"New link"</Button>
                            </div>
                            {note(format!("Whoever has the link joins straight away, for {} days. People you removed with a block still can't.", cartopolis_core::COMMUNITY_INVITE_DAYS))}
                        },
                        None => view! {
                            <div style={ROW}>
                                <Button size=Size::Sm icon={icon(icons::SHARE)} onclick={tap(&l, Tap::Ops(vec![CommunityOp::CreateInvite(id)]))}>"Make an invite link"</Button>
                            </div>
                        },
                    }
                }
            };
            view! {
                <div style={CARD}>
                    <FieldText model={f.add_people.clone()} placeholder="Usernames, separated by commas" autocomplete="off" autofocus=true onkeydown={on_enter(&l, Tap::AddPeople(id))} onchange={typed} id="communities-add-people" />
                    {note("They'll be in every topic, and get the recent history from whoever adds them.")}
                    <div style={ROW}><Button size=Size::Sm variant=Variant::Primary onclick={tap(&l, Tap::AddPeople(id))}>"Add"</Button></div>
                    <div style="height: 1px; background: var(--m-border)"></div>
                    {link}
                </div>
            }
        }
    };
    let starting = {
        let l = l.clone();
        move || -> View {
            let f = &l.communities.f;
            if !f.starting.get() {
                return View::Empty;
            }
            let typed = on_value(&l, |w, v: String| w.communities.new_topic = v);
            view! {
                <div style={CARD}>
                    <FieldText model={f.new_topic.clone()} placeholder="Topic name, e.g. meet-ups" autocomplete="off" autofocus=true onkeydown={on_enter(&l, Tap::StartTopic(id))} onchange={typed} id="communities-new-topic" />
                    <div style={ROW}><Button size=Size::Sm variant=Variant::Primary disabled={f.topic_off.clone()} onclick={tap(&l, Tap::StartTopic(id))}>"Start topic"</Button></div>
                </div>
            }
        }
    };
    view! { {adding}{starting} }
}

/// What waits for a moderator, as one row that opens it: requests to join
/// and reports.
fn queue(l: &ChatsLive, v: &CommunityV, c: &CommunityDto) -> View {
    let id = c.id;
    let waiting = c.requests.len() + c.reports.len();
    if waiting == 0 {
        return View::Empty;
    }
    let mut parts = Vec::new();
    if !c.requests.is_empty() {
        parts.push(format!("{} asking to join", c.requests.len()));
    }
    if !c.reports.is_empty() {
        parts.push(plural(c.reports.len(), "report", "reports"));
    }
    let title = if waiting == 1 {
        "1 thing waits for you".to_string()
    } else {
        format!("{waiting} things wait for you")
    };
    let parts = parts.join(" · ");
    let label = if v.reviewing { "Hide" } else { "Review" };
    let row = view! {
        <div style="display: flex; align-items: center; gap: 12px; padding: 12px 12px 12px 16px; border: 1px solid var(--m-border); border-radius: var(--m-radius-lg); background: var(--m-warning-soft)">
            <span style="flex: 1; min-width: 0; display: flex; flex-direction: column; gap: 2px">
                <span style="color: var(--m-warning); font-weight: 600">{title}</span>
                <span style="color: var(--m-text-dim); font-size: var(--m-font-size-sm)">{parts}</span>
            </span>
            <Button size=Size::Sm onclick={tap(l, Tap::ToggleReview)}>{label}</Button>
        </div>
    };
    if !v.reviewing {
        return row;
    }
    let requests: View = if c.requests.is_empty() {
        View::Empty
    } else {
        let rows: Vec<View> = c
            .requests
            .iter()
            .map(|r| {
                let who = cm::shown(&r.address, &c.home);
                let said = if r.message.is_empty() {
                    "No message".to_string()
                } else {
                    format!("\u{201c}{}\u{201d}", r.message)
                };
                let decide = |yes: bool| {
                    tap(
                        l,
                        Tap::Ops(vec![CommunityOp::Decide(id, r.address.clone(), yes)]),
                    )
                };
                let end = view! {
                    <Button size=Size::Sm variant=Variant::Primary icon={icon(icons::CHECK)} onclick={decide(true)} title="Let them in">"Let in"</Button>
                    <Button variant=Variant::Ghost icon={icon(icons::CLEAR)} icon_only=true onclick={decide(false)} title="Turn down" />
                };
                split_row(|_: &Event| {}, disc(&who, ChatKind::Private, 32), who.clone(), said, end)
            })
            .collect();
        view! {
            <div style={BLOCK}>
                {group(format!("Asking to join ({})", c.requests.len()))}
                {rows}
                {note("Letting someone in adds them to every topic. A refusal stands for a week.")}
            </div>
        }
    };
    let reports: View = if c.reports.is_empty() {
        View::Empty
    } else {
        let cards: Vec<View> = c
            .reports
            .iter()
            .map(|r| {
                let what = format!(
                    "{} reported {} · {}",
                    cm::shown(&r.reporter, &c.home),
                    cm::shown(&r.reported, &c.home),
                    r.reason.label()
                );
                let quote: View = if r.line.is_empty() {
                    View::Empty
                } else {
                    said(format!("\u{201c}{}\u{201d}", r.line))
                };
                let extra: View = if r.note.is_empty() {
                    View::Empty
                } else {
                    note(r.note.clone())
                };
                let delete = tap(
                    l,
                    Tap::Ops(vec![
                        CommunityOp::ModerateDelete(r.group.clone(), r.seq),
                        CommunityOp::ResolveReport(id, r.id),
                    ]),
                );
                let remove = vec![
                    CommunityOp::Remove {
                        id,
                        who: r.reported.clone(),
                        topic: None,
                        block: false,
                    },
                    CommunityOp::ResolveReport(id, r.id),
                ];
                let dismiss = tap(l, Tap::Ops(vec![CommunityOp::ResolveReport(id, r.id)]));
                view! {
                    <div style={CARD}>
                        <span style="color: var(--m-text-dim); font-size: var(--m-font-size-sm)">{what}</span>
                        {quote}
                        {extra}
                        <div style={ROW}>
                            <Button size=Size::Sm icon={icon(icons::DELETE)} onclick={delete} title="Delete the message">"Message"</Button>
                            {confirm(l, format!("report remove {}", r.id), "Remove".into(), "Remove them?", move |w, a| run(w, a, Tap::Ops(remove.clone())))}
                            <Button size=Size::Sm variant=Variant::Ghost icon={icon(icons::CHECK)} onclick={dismiss} title="Nothing to do: close the report">"Dismiss"</Button>
                        </div>
                    </div>
                }
            })
            .collect();
        view! {
            <div style={BLOCK}>
                {group(format!("Reports ({})", c.reports.len()))}
                {cards}
            </div>
        }
    };
    view! { {row}{requests}{reports} }
}

/// The encrypted half: every topic, opened by a tap, muted on this device.
fn topics_list(l: &ChatsLive, v: &CommunityV, c: &CommunityDto) -> View {
    let rows: Vec<View> = c
        .topics
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let key = channel_key(&t.group);
            let sub = if t.archived {
                "Archived: its history stays, nothing new"
            } else if t.joined {
                "Open"
            } else {
                "Being added…"
            };
            let end: View = if t.joined {
                let muted = v.muted.get(i).copied().unwrap_or(false);
                view! {
                    <Button size=Size::Sm variant=Variant::Ghost onclick={tap(l, Tap::Mute(key.clone(), !muted))} title="Muted topics show no count. Only this device knows.">{if muted { "Unmute" } else { "Mute" }}</Button>
                }
            } else {
                View::Empty
            };
            let open = if t.joined {
                Some(l.later({
                    let key = key.clone();
                    move |w, a| run(w, a, Tap::Open(key.clone()))
                }))
            } else {
                None
            };
            split_row(
                move |_: &Event| {
                    if let Some(go) = &open {
                        go();
                    }
                },
                disc(&t.name, ChatKind::Topic, 32),
                format!("# {}", t.name),
                sub.to_string(),
                end,
            )
        })
        .collect();
    view! {
        {line(icons::LOCK, "Members only, end-to-end encrypted. Never on the fediverse.")}
        <div style={BLOCK}>{rows}</div>
        {note("Everyone here is in every topic. Somebody added later is given the last 300 messages of each by whoever adds them.")}
    }
}

// ─── The public half ─────────────────────────────────────────────────────────

/// The Posts half: a thread when one is open, else the feed.
fn posts_half(l: &ChatsLive, id: Uuid) -> View {
    let l = l.clone();
    (move || -> View {
        match l.communities.f.thread.get() {
            Some(post) => thread_view(&l, id, post),
            None => part(&l, |l, page| match member(page) {
                Some((v, c, role)) => feed(l, &v.posts, c, role),
                None => View::Empty,
            }),
        }
    })
    .into_view()
}

/// What went wrong, dismissable.
fn posts_note(l: &ChatsLive, p: &PostsV) -> View {
    match &p.note {
        Some(n) => {
            let dismiss = Some(l.later(|w, a| run_post(w, a, PostTap::DismissNote)));
            let n = n.clone();
            view! { <Alert tone=Tone::Danger ondismiss={dismiss}>{n}</Alert> }
        }
        None => View::Empty,
    }
}

/// The feed: who reads it, Write, the map and settings, the reports, and
/// every post.
fn feed(l: &ChatsLive, p: &PostsV, c: &CommunityDto, role: CommunityRole) -> View {
    let id = c.id;
    let note_v = posts_note(l, p);
    let Some(info) = p.info.as_ref() else {
        return view! { {note_v}{busy("Reading the posts…")} };
    };
    if !info.settings.enabled {
        let settings = if role.moderates() {
            settings_form(l, c, info)
        } else {
            View::Empty
        };
        return view! {
            {note_v}
            {note("This community has no public posts. With them on, anybody can read what members post here, on the map and on the fediverse (Mastodon, Lemmy and others), and reply.")}
            {settings}
        };
    }
    let address: View = match &info.address {
        Some(a) => note(format!("@{a}")),
        None => View::Empty,
    };
    let write: View = if !info.may_post {
        View::Empty
    } else if p.draft_waiting {
        view! {
            <div style="display: flex; gap: 8px">
                <div style="flex: 1; display: flex; flex-direction: column">
                    <Button variant=Variant::Primary icon={icon(icons::RENAME)} onclick={ptap(l, PostTap::ContinueDraft)}>"Continue your post"</Button>
                </div>
                <Button variant=Variant::Ghost icon={icon(icons::CLEAR)} icon_only=true onclick={ptap(l, PostTap::DiscardDraft)} title="Discard the draft" />
            </div>
        }
    } else {
        view! { <Button variant=Variant::Primary icon={icon(icons::RENAME)} onclick={ptap(l, PostTap::Write(id))}>"Write a post"</Button> }
    };
    let pins = if p.on_map {
        "Pinned on the map"
    } else {
        "Show on the map"
    };
    let pins_variant = if p.on_map {
        Variant::Primary
    } else {
        Variant::Secondary
    };
    let settings_button: View = if role.moderates() {
        let variant = if p.settings_open {
            Variant::Primary
        } else {
            Variant::Ghost
        };
        view! { <Button variant={variant} icon={icon(icons::SETTINGS)} icon_only=true onclick={ptap(l, PostTap::ToggleSettings)} title="Settings" /> }
    } else {
        View::Empty
    };
    let settings = if p.settings_open && role.moderates() {
        settings_form(l, c, info)
    } else {
        View::Empty
    };
    let reports = if role.moderates() {
        reports_list(l, id, &p.reports)
    } else {
        View::Empty
    };
    let now = cp::now_ms();
    let cards: View = if p.feed.is_empty() {
        empty(
            "No posts yet",
            if info.may_post {
                "Write the first: it is pinned where you are looking."
            } else {
                "Posts from members and from the fediverse appear here."
            },
        )
    } else {
        let cards: Vec<View> = p
            .feed
            .iter()
            .map(|post| post_card(l, p, c, post, now, true))
            .collect();
        view! { <div style="display: flex; flex-direction: column; gap: 12px">{cards}</div> }
    };
    view! {
        {note_v}
        {public(icons::SHARE, format!("Anyone can read these, on any server · {}", plural(info.followers as usize, "follower", "followers")))}
        {address}
        {write}
        <div style="display: flex; align-items: center; gap: 8px">
            <div style="flex: 1; display: flex; flex-direction: column">
                <Button variant={pins_variant} icon={icon(icons::PIN)} onclick={ptap(l, PostTap::TogglePins(id))} title="Pin this community's posts where they were written">{pins}</Button>
            </div>
            <Button variant=Variant::Ghost icon={icon(icons::REFRESH)} icon_only=true onclick={ptap(l, PostTap::Refresh(id))} title="Read again" />
            {settings_button}
        </div>
        {settings}
        {reports}
        {cards}
    }
}

/// Reports from other servers waiting for a moderator.
fn reports_list(l: &ChatsLive, id: Uuid, reports: &[PostReport]) -> View {
    if reports.is_empty() {
        return View::Empty;
    }
    let now = cp::now_ms();
    let cards: Vec<View> = reports
        .iter()
        .map(|r| {
            let who = format!("{} · {}", r.reporter, cp::ago(r.created_ms, now));
            let reason: View = if r.reason.is_empty() {
                View::Empty
            } else {
                said(r.reason.clone())
            };
            let post = format!("{}: {}", r.post.author.address, cp::first_line(&r.post.text, 120));
            let resolve = |action: ReportAction| {
                ptap(
                    l,
                    PostTap::Resolve {
                        community: id,
                        report: r.id,
                        action,
                    },
                )
            };
            view! {
                <div style={CARD}>
                    {public(icons::FLAG, who)}
                    {reason}
                    {note(post)}
                    <div style={ROW}>
                        <Button size=Size::Sm icon={icon(icons::DELETE)} onclick={resolve(ReportAction::Hide)}>"Hide the post"</Button>
                        <Button size=Size::Sm variant=Variant::Ghost onclick={resolve(ReportAction::Dismiss)}>"Leave it"</Button>
                    </div>
                </div>
            }
        })
        .collect();
    view! {
        <div style={BLOCK}>
            {group(format!("{} from other servers", plural(reports.len(), "report", "reports")))}
            {cards}
        </div>
    }
}

/// One post as a card: who and when, its warning, its words (or the edit),
/// its pictures as tiles, its place, likes and passes, and its actions.
/// `tappable`: its words and its replies open the thread.
fn post_card(
    l: &ChatsLive,
    v: &PostsV,
    c: &CommunityDto,
    p: &PostDto,
    now: i64,
    tappable: bool,
) -> View {
    let moderates = c.role.is_some_and(CommunityRole::moderates);
    let mut meta = if p.author.local {
        "member".to_string()
    } else {
        p.author.address.clone()
    };
    if let Some(via) = &p.via {
        meta.push_str(&format!(" · via {via}"));
    }
    meta.push_str(&format!(" · {}", cp::ago(p.created_ms, now)));
    if p.edited_ms.is_some() {
        meta.push_str(" · edited");
    }
    let meta_colour = if p.author.local {
        "color: var(--m-text-dim); font-size: var(--m-font-size-sm)"
    } else {
        "color: var(--m-info); font-size: var(--m-font-size-sm)"
    };
    let author = p.author.name.clone();
    let answers: View = match &p.answers {
        Some(to) => line(icons::CHAT, format!("to {to}")),
        None => View::Empty,
    };
    let shown = v.shown.contains(&p.id);
    // Behind its warning, the words and pictures wait.
    let hidden = p.warning.is_some() && !p.mine && !shown;
    let warning: View = match &p.warning {
        Some(w) => {
            let toggle: View = if p.mine {
                View::Empty
            } else {
                view! { <Button size=Size::Sm variant=Variant::Ghost onclick={ptap(l, PostTap::ToggleShown(p.id))}>{if hidden { "Show" } else { "Hide" }}</Button> }
            };
            let w = w.clone();
            view! {
                <div style="display: flex; align-items: center; gap: 8px; color: var(--m-warning)">
                    {icon(icons::WARNING)}
                    <span style="flex: 1; min-width: 0">{w}</span>
                    {toggle}
                </div>
            }
        }
        None => View::Empty,
    };
    let words: View = if hidden {
        View::Empty
    } else if v.editing == Some(p.id) {
        edit_box(l, c.id, p.id)
    } else {
        let text = p.text.clone();
        if tappable {
            let open = l.tap({
                let post = p.id;
                move |w, a| run_post(w, a, PostTap::OpenThread(post))
            });
            view! { <p style="margin: 0; color: var(--m-text); white-space: pre-wrap; cursor: pointer" on:click={open}>{text}</p> }
        } else {
            said(text)
        }
    };
    let pictures: View = if hidden {
        View::Empty
    } else if p.sensitive
        && !p.mine
        && !shown
        && (p.picture.is_some() || !p.more_pictures.is_empty())
    {
        view! {
            <div style={ROW}>
                <Button size=Size::Sm icon={icon(icons::WARNING)} onclick={ptap(l, PostTap::ToggleShown(p.id))}>"Sensitive photo: tap to show"</Button>
            </div>
        }
    } else {
        // Each picture once it is made; until then the tile that names it.
        // The polaroid keeps its own proportions and no rounding: its frame
        // is part of the picture.
        let pics = l.communities.pics.get();
        let mut tiles = Vec::new();
        if let Some(h) = &p.snapshot {
            tiles.push(match pics.polaroids.get(h) {
                Some(s) => shown_picture(s, "Polaroid of the view", false),
                None => picture_tile(icons::CAMERA, "Polaroid of the view".into()),
            });
        }
        let photos = p
            .picture
            .iter()
            .map(|h| (h, &p.picture_alt))
            .chain(p.more_pictures.iter().map(|x| (&x.hash, &x.alt)));
        for (h, alt) in photos {
            let alt = alt.clone().unwrap_or_else(|| "Photo".into());
            tiles.push(match pics.photos.get(h) {
                Some(s) => shown_picture(s, &alt, true),
                None => picture_tile(icons::UPLOAD, alt),
            });
        }
        tiles.into_view()
    };
    let place: View = match &p.place {
        Some(place) => {
            let label = place.name.clone().unwrap_or_else(|| "On the map".into());
            let point = cp::point_of(place, &p.text);
            view! {
                <span style="display: flex; align-items: center; gap: 4px; color: var(--m-text-dim); font-size: var(--m-font-size-sm)">{icon(icons::PIN)}{label}</span>
                <Button size=Size::Sm variant=Variant::Ghost icon={icon(icons::CAM_EARTH)} onclick={ptap(l, PostTap::ShowInWorld(c.id, point))} title="Open where it was written, on the map">"Show in world"</Button>
            }
        }
        None => {
            view! { <span style="color: var(--m-text-dim); font-size: var(--m-font-size-sm)">"No place: listed, not pinned"</span> }
        }
    };
    let likes = if p.dislikes > 0 {
        format!("{} · {} down", p.likes, p.dislikes)
    } else {
        p.likes.to_string()
    };
    let like_variant = if p.liked {
        Variant::Primary
    } else {
        Variant::Ghost
    };
    let like = view! {
        <Button size=Size::Sm variant={like_variant} icon={icon(icons::STAR)} onclick={ptap(l, PostTap::Like(p.id, !p.liked))} title={if p.liked { "Take your like back" } else { "Like" }}>{likes}</Button>
    };
    let pass: View = if p.parent.is_none() {
        let boosts = p.boosts.to_string();
        if p.mine {
            if p.boosts > 0 {
                view! { <span style="display: flex; align-items: center; gap: 4px; color: var(--m-text-dim); font-size: var(--m-font-size-sm)">{icon(icons::SHARE)}{boosts}</span> }
            } else {
                View::Empty
            }
        } else {
            let variant = if p.boosted {
                Variant::Primary
            } else {
                Variant::Ghost
            };
            view! {
                <Button size=Size::Sm variant={variant} icon={icon(icons::SHARE)} onclick={ptap(l, PostTap::Boost(p.id, !p.boosted))} title={if p.boosted { "Stop passing it on" } else { "Pass it on to the people who follow you" }}>{boosts}</Button>
            }
        }
    } else {
        let reply = PostTap::ReplyTo {
            post: p.id,
            name: p.author.name.clone(),
            address: p.author.address.clone(),
            local: p.author.local,
        };
        let title = format!("Answer {}", p.author.name);
        view! { <Button size=Size::Sm variant=Variant::Ghost icon={icon(icons::CHAT)} onclick={ptap(l, reply)} title={title}>"Reply"</Button> }
    };
    let replies: View = if p.parent.is_none() && tappable {
        let n = plural(p.replies as usize, "reply", "replies");
        view! { <Button size=Size::Sm variant=Variant::Ghost icon={icon(icons::CHAT)} onclick={ptap(l, PostTap::OpenThread(p.id))}>{n}</Button> }
    } else {
        View::Empty
    };
    let menu: View = if v.menu_for == Some(p.id) {
        let edit: View = if p.mine {
            view! { <Button size=Size::Sm icon={icon(icons::RENAME)} onclick={ptap(l, PostTap::Edit(p.id, p.text.clone()))}>"Edit"</Button> }
        } else {
            View::Empty
        };
        let delete: View = if p.mine {
            let (community, post) = (c.id, p.id);
            confirm(
                l,
                format!("post delete {post}"),
                "Delete".into(),
                "Delete: tap again",
                move |w, a| run_post(w, a, PostTap::Delete(community, post)),
            )
        } else {
            View::Empty
        };
        let hide: View = if moderates && !p.mine {
            view! { <Button size=Size::Sm icon={icon(icons::DELETE)} onclick={ptap(l, PostTap::Hide(c.id, p.id))}>"Hide from this community"</Button> }
        } else {
            View::Empty
        };
        let block: View = if moderates && !p.mine && !p.author.local {
            let words = format!("Block {}", p.author.address);
            view! { <Button size=Size::Sm icon={icon(icons::FLAG)} onclick={ptap(l, PostTap::Block(c.id, p.author.address.clone()))} title="They can't post here again, and what they wrote here is hidden">{words}</Button> }
        } else {
            View::Empty
        };
        let report: View = if p.mine {
            View::Empty
        } else {
            view! { <Button size=Size::Sm icon={icon(icons::FLAG)} onclick={ptap(l, PostTap::Report(p.id))} title="To this community's moderators">"Report"</Button> }
        };
        let server: View = if moderates && !p.author.local {
            note("A server that sends nothing but trouble is the server admin's to block, for every community at once.")
        } else {
            View::Empty
        };
        view! {
            <div style="height: 1px; background: var(--m-border)"></div>
            <div style={ROW}>{edit}{delete}{hide}{block}{report}</div>
            {server}
        }
    } else {
        View::Empty
    };
    let report_box: View = if v.reporting == Some(p.id) {
        report_box(l, p)
    } else {
        View::Empty
    };
    view! {
        <div style={CARD}>
            <div style="display: flex; align-items: center; gap: 12px">
                {disc(&p.author.name, ChatKind::Private, 32)}
                <span style="flex: 1; min-width: 0; display: flex; flex-direction: column; gap: 2px">
                    <span style="color: var(--m-text-strong); font-weight: 600; white-space: nowrap; text-overflow: ellipsis; overflow: hidden">{author}</span>
                    <span style={meta_colour}>{meta}</span>
                </span>
                <Button variant=Variant::Ghost icon={icon(icons::MORE)} icon_only=true onclick={ptap(l, PostTap::ToggleMenu(p.id))} title="More" />
            </div>
            {answers}
            {warning}
            {words}
            {pictures}
            <div style={ROW}>{place}{like}{pass}{replies}</div>
            {menu}
            {report_box}
        </div>
    }
}

/// A post's words being edited, with Save and Cancel.
fn edit_box(l: &ChatsLive, community: Uuid, post: Uuid) -> View {
    let f = &l.communities.f;
    let typed = on_value(l, |w, v: String| {
        if let Some((_, text)) = w.communities.posts.editing.as_mut() {
            *text = v;
        }
    });
    view! {
        <FieldTextarea model={f.edit_text.clone()} rows={3_u32} maxlength={MAX_POST_TEXT as u32} onchange={typed} id="post-edit" />
        <div style={ROW}>
            <Button size=Size::Sm variant=Variant::Primary icon={icon(icons::RENAME)} disabled={f.save_off.clone()} onclick={ptap(l, PostTap::SaveEdit(community, post))}>"Save"</Button>
            <Button size=Size::Sm variant=Variant::Ghost onclick={ptap(l, PostTap::CancelEdit)}>"Cancel"</Button>
        </div>
    }
}

/// What somebody reporting a post says, and whether its own server hears.
fn report_box(l: &ChatsLive, p: &PostDto) -> View {
    let f = &l.communities.f;
    let reason = on_value(l, |w, v: String| {
        if let Some((_, r, _)) = w.communities.posts.reporting.as_mut() {
            *r = v;
        }
    });
    let forward: View = if p.author.local {
        View::Empty
    } else {
        let server = p
            .author
            .address
            .split('@')
            .nth(1)
            .unwrap_or("its server")
            .to_string();
        let change = on_value(l, |w, v: bool| {
            if let Some((_, _, fw)) = w.communities.posts.reporting.as_mut() {
                *fw = v;
            }
        });
        let label = format!("Also tell {server}, without your name");
        view! { <FieldCheckbox model={f.report_forward.clone()} label={label} onchange={change} id="post-report-forward" /> }
    };
    view! {
        <div style="height: 1px; background: var(--m-border)"></div>
        <FieldTextarea model={f.report_reason.clone()} rows={2_u32} maxlength={1000_u32} placeholder="What is wrong with it? (optional)" onchange={reason} id="post-report" />
        {forward}
        <div style={ROW}>
            <Button size=Size::Sm variant=Variant::Danger icon={icon(icons::FLAG)} onclick={ptap(l, PostTap::SendReport(p.id))}>"Report"</Button>
            <Button size=Size::Sm variant=Variant::Ghost onclick={ptap(l, PostTap::CancelReport)}>"Cancel"</Button>
        </div>
    }
}

/// An open thread: back to the posts, the post and its replies, and the
/// reply field, which stays put while the replies move.
fn thread_view(l: &ChatsLive, id: Uuid, post: Uuid) -> View {
    let f = &l.communities.f;
    let cards = part(l, |l, page| {
        let Some((v, c, _)) = member(page) else {
            return View::Empty;
        };
        let p = &v.posts;
        let Some(t) = p.thread.as_ref() else {
            return view! { {posts_note(l, p)}{busy("Reading the thread…")} };
        };
        let now = cp::now_ms();
        let replies: Vec<View> = t
            .replies
            .iter()
            .map(|r| post_card(l, p, c, r, now, false))
            .collect();
        view! {
            {posts_note(l, p)}
            {post_card(l, p, c, &t.post, now, false)}
            {group(plural(t.replies.len(), "reply", "replies"))}
            <div style="display: flex; flex-direction: column; gap: 12px">{replies}</div>
        }
    });
    let to = part(l, |l, page| {
        let Some(name) = member(page).and_then(|(v, _, _)| v.posts.reply_to.clone()) else {
            return View::Empty;
        };
        view! {
            <div style="display: flex; align-items: center; gap: 8px; color: var(--m-text-dim); font-size: var(--m-font-size-sm)">
                {icon(icons::CHAT)}
                <span style="flex: 1; min-width: 0">{format!("To {name}")}</span>
                <Button variant=Variant::Ghost icon={icon(icons::CLEAR)} icon_only=true onclick={ptap(l, PostTap::ReplyToPost)} title="Answer the post instead" />
            </div>
        }
    });
    let typed = on_value(l, |w, v: String| w.communities.posts.reply = v);
    let _ = id;
    view! {
        <div style="display: flex">
            <Button variant=Variant::Ghost size=Size::Sm icon={icon(icons::CHEVRON_LEFT)} onclick={ptap(l, PostTap::CloseThread)}>"Posts"</Button>
        </div>
        {cards}
        {to}
        <FieldTextarea model={f.reply.clone()} rows={2_u32} maxlength={MAX_POST_TEXT as u32} placeholder="Reply in public" onchange={typed} id="post-reply" />
        <Button variant=Variant::Primary icon={icon(icons::SEND)} disabled={f.reply_off.clone()} onclick={ptap(l, PostTap::SendReply(post))}>"Reply"</Button>
        {note("Replies are public, like the post, and reach its author wherever they are.")}
    }
}

/// A moderator's settings for the public half: the address, who posts,
/// on or off, and what the community follows elsewhere.
fn settings_form(l: &ChatsLive, c: &CommunityDto, info: &PublicInfo) -> View {
    let f = &l.communities.f;
    let id = c.id;
    let first = info.settings.handle.is_none();
    let address: View = if first {
        let typed = on_value(l, |w, v: String| w.communities.posts.handle = v);
        let home = c.home.clone();
        let shown = {
            let handle = f.handle.clone();
            move || -> View {
                note(format!(
                    "People elsewhere find it as @{}@{home}. It can't be changed later: their servers know the community by it.",
                    handle.get().trim()
                ))
            }
        };
        view! {
            <FieldText model={f.handle.clone()} label="The community's fediverse address" autocomplete="off" onchange={typed} id="post-handle" />
            {shown}
        }
    } else if let Some(address) = &info.address {
        let address = format!("@{address}");
        view! { <span style="color: var(--m-text-strong)">{address}</span> }
    } else {
        View::Empty
    };
    let who = on_post_pick(l, move |i: &usize| PostTap::Public {
        community: id,
        enabled: None,
        who: Some(WHO[(*i).min(1)].0),
    });
    let labels: Vec<&str> = WHO.iter().map(|(_, t)| *t).collect();
    let switch: View = if info.settings.enabled {
        view! { <Button size=Size::Sm onclick={ptap(l, PostTap::Public { community: id, enabled: Some(false), who: None })}>"Switch public posts off"</Button> }
    } else {
        view! { <Button size=Size::Sm variant=Variant::Primary icon={icon(icons::SHARE)} onclick={ptap(l, PostTap::Public { community: id, enabled: Some(true), who: None })}>"Switch public posts on"</Button> }
    };
    let follows: View = if !info.settings.enabled {
        View::Empty
    } else if !info.federating {
        view! {
            <div style="height: 1px; background: var(--m-border)"></div>
            {group("This community follows")}
            {note("This server does not talk to other servers, so it follows nothing.")}
        }
    } else {
        let rows: Vec<View> = info
            .follows
            .iter()
            .map(|x| {
                let kind = match x.kind {
                    SourceKind::Community => "community",
                    SourceKind::Account => "account",
                };
                let state = if x.accepted {
                    ""
                } else {
                    " · waiting for their server"
                };
                let sub = format!("{kind}{state}");
                let address = x.address.clone();
                view! {
                    <div style="display: flex; align-items: center; gap: 8px">
                        <span style="flex: 1; min-width: 0; display: flex; flex-direction: column; gap: 2px">
                            <span style="color: var(--m-info); overflow: hidden; text-overflow: ellipsis">{address}</span>
                            <span style="color: var(--m-text-dim); font-size: var(--m-font-size-sm)">{sub}</span>
                        </span>
                        <Button variant=Variant::Ghost icon={icon(icons::CLEAR)} icon_only=true onclick={ptap(l, PostTap::Unfollow(id, x.address.clone()))} title="Stop following" />
                    </div>
                }
            })
            .collect();
        let nothing: View = if info.follows.is_empty() {
            note("Nothing yet. Their posts would appear here, marked with where they came from.")
        } else {
            View::Empty
        };
        let typed = on_value(l, |w, v: String| w.communities.posts.follow = v);
        view! {
            <div style="height: 1px; background: var(--m-border)"></div>
            {group("This community follows")}
            {nothing}
            {rows}
            <FieldText model={f.follow.clone()} placeholder="groningen@feddit.nl, someone@mastodon.social, or a relay's address" autocomplete="off" onchange={typed} id="post-follow" />
            <div style={ROW}>
                <Button size=Size::Sm icon={icon(icons::ADD)} disabled={f.follow_off.clone()} onclick={ptap(l, PostTap::Follow(id))}>"Follow"</Button>
            </div>
        }
    };
    view! {
        <div style={CARD}>
            {address}
            {group("Who can post")}
            {segmented(&f.who_posts, &labels, who, "post-who")}
            <div style={ROW}>{switch}</div>
            {follows}
        </div>
    }
}

// ─── The post being written ──────────────────────────────────────────────────

/// Writing a post is the community's page: its header with Post, then the
/// words first, where, what goes with it, and who it reaches.
fn compose_page(l: &ChatsLive, id: Uuid) -> View {
    let f = &l.communities.f;
    let post = {
        let l = l.clone();
        move || -> View {
            let f = &l.communities.f;
            let label = if f.sending.get() {
                "Posting…"
            } else {
                "Post"
            };
            view! {
                <Button variant=Variant::Primary icon={icon(icons::SEND)} disabled={f.post_off.clone()} onclick={compose(&l, ComposerAction::Post)} title="Post it: public, with this place">{label}</Button>
            }
        }
    };
    let text = on_value(l, |w, v: String| {
        if let Some(d) = w.communities.posts.draft.as_mut() {
            d.text = v;
        }
    });
    let warning_on = on_value(l, |w, v: bool| {
        if let Some(d) = w.communities.posts.draft.as_mut() {
            if d.warning.is_some() != v {
                d.warning = v.then(String::new);
            }
        }
    });
    let place = part(l, |l, page| {
        let Some(v) = (match page {
            Page::Community(v) => v.compose.clone(),
            _ => None,
        }) else {
            return View::Empty;
        };
        let what: View = match v.place {
            Some(name) => view! {
                <div style="flex: 1; min-width: 0; display: flex; flex-direction: column; gap: 2px">
                    <span style="overflow: hidden; white-space: nowrap; text-overflow: ellipsis; color: var(--m-text-strong)">{name}</span>
                    <span style="color: var(--m-text-dim); font-size: var(--m-font-size-sm)">"Pinned where you are looking"</span>
                </div>
            },
            None => view! {
                <div style="flex: 1; min-width: 0; display: flex; align-items: center; gap: 8px; color: var(--m-text-dim)">
                    <Spinner size=Size::Sm />"Taking the view…"
                </div>
            },
        };
        let attachment = |glyph: &'static str, what: &'static str, a: ComposerAction| -> View {
            view! {
                <div style="display: flex; align-items: center; gap: 12px; padding: 8px 8px 8px 16px; border-radius: var(--m-radius); background: var(--m-surface-sunken)">
                    <span style="color: var(--m-accent)">{icon(glyph)}</span>
                    <span style="flex: 1; min-width: 0; color: var(--m-text)">{what}</span>
                    <Button variant=Variant::Ghost icon={icon(icons::DELETE)} icon_only=true onclick={compose(l, a)} title="Remove it from the post" />
                </div>
            }
        };
        // Each as it will look, with its own remove, once it is made; the
        // row naming it until then.
        let pics = l.communities.pics.get();
        let snapshot = match (v.snapshot, &pics.draft_polaroid) {
            (true, Some(s)) => picture_box(
                &s.src,
                s.w,
                s.h,
                false,
                "Polaroid of the view",
                compose(l, ComposerAction::RemoveSnapshot),
            ),
            (true, None) => attachment(
                icons::CAMERA,
                "Polaroid of the view",
                ComposerAction::RemoveSnapshot,
            ),
            (false, _) => View::Empty,
        };
        let picture = match (v.picture, &pics.draft_picture) {
            (true, Some(s)) => picture_box(
                &s.src,
                s.w,
                s.h,
                true,
                "Picture",
                compose(l, ComposerAction::RemovePicture),
            ),
            (true, None) => attachment(icons::UPLOAD, "Picture", ComposerAction::RemovePicture),
            (false, _) => View::Empty,
        };
        let (camera, upload) = if v.picture {
            ("Retake", "Replace")
        } else {
            ("Camera", "Upload")
        };
        let n: View = match v.note {
            Some(n) => note(n),
            None => View::Empty,
        };
        view! {
            // The place and its note across the whole width, the button
            // under them: beside them it left the name a few letters.
            <div style="display: flex; flex-direction: column; align-items: flex-start; gap: 8px; padding: 12px 12px 12px 16px; border: 1px solid var(--m-border); border-radius: var(--m-radius-lg); background: var(--m-surface)">
                <div style="align-self: stretch; display: flex; align-items: center; gap: 12px">
                    <span style="flex: none; color: var(--m-accent)">{icon(icons::PIN)}</span>
                    {what}
                </div>
                <Button size=Size::Sm icon={icon(icons::HERE)} onclick={compose(l, ComposerAction::UseView)} title="Close the panel, look somewhere else, come back and press this">"Use view"</Button>
            </div>
            {snapshot}
            {picture}
            <div style={ROW}>
                <Button icon={icon(icons::CAMERA)} disabled={v.busy} onclick={compose(l, ComposerAction::Camera)} title="Take a picture for this post">{camera}</Button>
                <Button icon={icon(icons::UPLOAD)} disabled={v.busy} onclick={compose(l, ComposerAction::Upload)} title="A picture from this device">{upload}</Button>
            </div>
            {n}
            {public(icons::SHARE, v.reach.clone())}
        }
    });
    // Each optional field its own reactive node, so the others (and the
    // keyboard on one) are left alone when it comes or goes.
    let warning = {
        let l = l.clone();
        move || -> View {
            let f = &l.communities.f;
            if !f.warning_on.get() {
                return View::Empty;
            }
            let typed = on_value(&l, |w, v: String| {
                if let Some(d) = w.communities.posts.draft.as_mut() {
                    if let Some(x) = d.warning.as_mut() {
                        *x = v;
                    }
                }
            });
            view! {
                <FieldText model={f.warning.clone()} placeholder="What readers see before they open it" autocomplete="off" onchange={typed} id="post-warning" />
            }
        }
    };
    let picture_extras = part(l, |l, page| {
        let has =
            matches!(page, Page::Community(v) if v.compose.as_ref().is_some_and(|c| c.picture));
        if !has {
            return View::Empty;
        }
        let f = &l.communities.f;
        let alt = on_value(l, |w, v: String| {
            if let Some(d) = w.communities.posts.draft.as_mut() {
                d.alt = v;
            }
        });
        let sensitive = on_value(l, |w, v: bool| {
            if let Some(d) = w.communities.posts.draft.as_mut() {
                d.sensitive = v;
            }
        });
        view! {
            <FieldTextarea model={f.alt.clone()} rows={2_u32} maxlength={MAX_PICTURE_ALT as u32} placeholder="What the photo shows, in words" onchange={alt} id="post-alt" />
            <FieldCheckbox model={f.sensitive.clone()} label="Sensitive: blur the photo until opened" onchange={sensitive} id="post-sensitive" />
        }
    });
    let hint = {
        let empty = f.post_empty.clone();
        move || -> View {
            if empty.get() {
                note("Write something, and Post above goes live.")
            } else {
                View::Empty
            }
        }
    };
    let _ = id;
    let body = body_style(l);
    view! {
        {header(back(tap(l, Tap::Back), "Back"), "New post".into_view(), view! { {post}{close(l)} })}
        <div style={body}>
            // The field counts its own characters (`maxlength`).
            <FieldTextarea model={f.post_text.clone()} rows={4_u32} maxlength={MAX_POST_TEXT as u32} placeholder="What's happening here?" onchange={text} id="post-text" />
            {place}
            <FieldCheckbox model={f.warning_on.clone()} label="Content warning" onchange={warning_on} id="post-warning-on" />
            {warning}
            {picture_extras}
            {hint}
        </div>
    }
}

// ─── People ──────────────────────────────────────────────────────────────────

fn people_page(l: &ChatsLive, id: Uuid) -> View {
    let content = {
        let l = l.clone();
        move || -> View {
            let page = l.communities.page.get();
            let open = l.communities.person.get();
            let Page::People(Some(v)) = &page else {
                return View::Empty;
            };
            let section = |title: &'static str, running: bool| -> View {
                let rows: Vec<View> =
                    v.c.members
                        .iter()
                        .filter(|m| m.role.moderates() == running)
                        .map(|m| person_row(&l, v, m, open.as_deref() == Some(m.address.as_str())))
                        .collect();
                view! {
                    <div style={BLOCK}>
                        {group(title)}
                        <List>{rows}</List>
                    </div>
                }
            };
            view! {
                {section("Running it", true)}
                {section("Members", false)}
                {note("Tap a person for what you can do. Only the owner appoints moderators.")}
            }
        }
    };
    let _ = id;
    let body = body_style(l);
    view! {
        {head(l, "People")}
        <div style={body}>{content}</div>
    }
}

/// One person, and under them, once tapped, what this account may do.
fn person_row(l: &ChatsLive, v: &PeopleV, m: &CommunityMemberDto, open: bool) -> View {
    let c = &v.c;
    let id = c.id;
    let who = cm::shown(&m.address, &c.home);
    let is_me = m.address.eq_ignore_ascii_case(&v.me);
    let sub = if is_me { "You" } else { cm::role_word(m.role) };
    if is_me {
        return view! {
            <ListItem icon={disc(&who, ChatKind::Private, 32)} sub={sub.into_view()}>{who.clone()}</ListItem>
        };
    }
    let toggle = {
        let (person, address) = (l.communities.person.clone(), m.address.clone());
        Some(Rc::new(move |_: &Event| {
            let now = person.get_untracked();
            person.set(if now.as_deref() == Some(address.as_str()) {
                None
            } else {
                Some(address.clone())
            });
        }) as Handler)
    };
    let row = view! {
        <ListItem icon={disc(&who, ChatKind::Private, 32)} sub={sub.into_view()} end={icon(icons::MORE)} onclick={toggle}>{who.clone()}</ListItem>
    };
    if !open {
        return row;
    }
    let mine = c.role.unwrap_or(CommunityRole::Member);
    let op = |o: CommunityOp| tap(l, Tap::Ops(vec![o]));
    let mut acts: Vec<View> = vec![view! {
        <Button size=Size::Sm icon={icon(icons::CHAT)} onclick={tap(l, Tap::Message(m.address.clone()))}>"Message"</Button>
    }];
    if mine == CommunityRole::Owner {
        if m.role == CommunityRole::Member {
            acts.push(view! { <Button size=Size::Sm icon={icon(icons::HOST)} onclick={op(CommunityOp::SetRole(id, m.address.clone(), CommunityRole::Moderator))}>"Make moderator"</Button> });
        }
        if m.role == CommunityRole::Moderator {
            acts.push(view! { <Button size=Size::Sm icon={icon(icons::HOST)} onclick={op(CommunityOp::SetRole(id, m.address.clone(), CommunityRole::Member))}>"Stop being a moderator"</Button> });
        }
        acts.push(view! { <Button size=Size::Sm icon={icon(icons::KEY)} onclick={op(CommunityOp::SetRole(id, m.address.clone(), CommunityRole::Owner))}>"Hand the community over"</Button> });
    }
    let may_remove = match m.role {
        CommunityRole::Owner => false,
        CommunityRole::Moderator => mine == CommunityRole::Owner,
        CommunityRole::Member => mine.moderates(),
    };
    let removals: View = if may_remove {
        let remove = |topic: Option<Vec<u8>>, block: bool| {
            op(CommunityOp::Remove {
                id,
                who: m.address.clone(),
                topic,
                block,
            })
        };
        let mut rows: Vec<View> = c
            .topics
            .iter()
            .filter(|t| !t.archived)
            .map(|t| {
                let words = format!("Remove from # {}", t.name);
                view! { <Button size=Size::Sm variant=Variant::Ghost icon={icon(icons::PERSON_REMOVE)} onclick={remove(Some(t.group.clone()), false)}>{words}</Button> }
            })
            .collect();
        rows.push(view! { <Button size=Size::Sm variant=Variant::Danger icon={icon(icons::PERSON_REMOVE)} onclick={remove(None, false)}>"Remove from the community"</Button> });
        rows.push(view! { <Button size=Size::Sm variant=Variant::Danger icon={icon(icons::PERSON_REMOVE)} onclick={remove(None, true)}>"Remove and block from rejoining"</Button> });
        view! { <div style={ROW}>{rows}</div> }
    } else {
        View::Empty
    };
    view! {
        {row}
        <div style="display: flex; flex-direction: column; gap: 8px; padding: 8px 12px 12px">
            <div style={ROW}>{acts}</div>
            {removals}
        </div>
    }
}

// ─── Post to and Activity ────────────────────────────────────────────────────

/// "Post this view": which community it goes to.
fn post_to_page(l: &ChatsLive) -> View {
    let content = part(l, |l, page| {
        let Page::PostTo { place, mine } = page else {
            return View::Empty;
        };
        let place: View = match place {
            Some(p) => line(icons::PIN, p.clone()),
            None => View::Empty,
        };
        let list: View = if mine.is_empty() {
            note("You're not in a community yet. Find one, or start one, first.")
        } else {
            let pics = l.communities.pics.get();
            let rows: Vec<View> = mine
                .iter()
                .map(|(id, name, picture)| {
                    let face = picture_disc(
                        name,
                        ChatKind::Community,
                        32,
                        pics.disc(picture.as_deref(), 32),
                    );
                    view! {
                        <ListItem icon={face} chevron=true onclick={ptap(l, PostTap::PostIn(*id))}>{name.clone()}</ListItem>
                    }
                })
                .collect();
            view! { <List>{rows}</List> }
        };
        view! {
            {place}
            {list}
            <div style={ROW}><Button onclick={ptap(l, PostTap::CancelPostTo)}>"Cancel"</Button></div>
        }
    });
    let body = body_style(l);
    view! {
        {head(l, "Post this view publicly, to")}
        <div style={body}>{content}</div>
    }
}

/// Replies, mentions, likes, boosts and followers on this account's public
/// posts; a notice about a post opens it on its community's page.
fn activity_page(l: &ChatsLive) -> View {
    let content = part(l, |l, page| {
        let Page::Activity(notices) = page else {
            return View::Empty;
        };
        let Some(list) = notices else {
            return busy("Reading what happened…");
        };
        if list.items.is_empty() {
            return note(
                "Nothing yet. Replies, likes and followers on your public posts show here.",
            );
        }
        let now = cp::now_ms();
        let rows: Vec<View> = list
            .items
            .iter()
            .map(|n| {
                let (glyph, words) = match n.kind {
                    NoticeKind::Reply => (icons::CHAT, "replied"),
                    NoticeKind::Mention => (icons::LABEL, "mentioned you"),
                    NoticeKind::Like => (icons::STAR, "likes your post"),
                    NoticeKind::Boost => (icons::SHARE, "passed on your post"),
                    NoticeKind::Follow => (icons::FOLLOW, "follows you"),
                };
                let mut when = cp::ago(n.created_ms, now);
                if !n.who.local {
                    when = format!("{} · {when}", n.who.address);
                }
                let sub = match &n.excerpt {
                    Some(e) => format!("{when}\n{e}"),
                    None => when,
                };
                let title = format!("{} {words}", n.who.name);
                let weight = if n.read { "" } else { "font-weight: 700" };
                let open = match (n.post, n.community) {
                    (Some(post), Some(community)) => ptap(l, PostTap::Notice(community, post)),
                    _ => None,
                };
                view! {
                    <ListItem icon={disc(&n.who.name, ChatKind::Private, 32)} sub={sub.into_view()} end={icon(glyph)} onclick={open}>
                        <span style={weight}>{title}</span>
                    </ListItem>
                }
            })
            .collect();
        view! { <List>{rows}</List> }
    });
    let body = body_style(l);
    view! {
        {head(l, "Activity")}
        <div style={body}>
            {note("Replies, mentions, likes and followers on your public posts, here and on other servers.")}
            {content}
        </div>
    }
}
