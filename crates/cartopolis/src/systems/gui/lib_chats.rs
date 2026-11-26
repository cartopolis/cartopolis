//! The Chats panel (Community) drawn by the component library (`lib_host`),
//! beside egui's (`chats::render_chats_panel`): the panel's frame and its
//! routing, the list (the public tiles, the requests row, "Coming up", the
//! Activity entry, the conversations and communities), the New sheet and
//! the You page.
//!
//! On with `CARTO_UI_LIB=chats` (or `1`), on the web `?ui=lib`
//! (`&panel=chats` opens it). egui's panel steps aside (`ChatsView::lib`)
//! on the screens the library draws (`ChatsScreen::on_library`), keeping an
//! empty strip, and still does what only it can: the picture picker, the
//! full-size picture viewer, the report and account-delete dialogs. Every
//! screen of the panel is on the library: the contacts screens
//! (`lib_contacts`) and the calendar (`lib_calendar`) were the last.
//!
//! **One set of logic, two editions.** Which screen shows is
//! `ChatsView::screen`; the list's rows are `chats::list_model`; a tap fills
//! the same `chats::Actions` egui's drawing fills, and the frame ends in the
//! same `chats::settle` (→ `apply`). Nothing here decides what a tap means.
//!
//! **The shell is shared.** The conversation (`lib_chat_conversation`), the
//! communities screens (`lib_communities`), the contacts screens
//! (`lib_contacts`) and the calendar (`lib_calendar`) plug into [`ChatsLive`]: the
//! router calls their `screen`, the system calls their `mirror` every frame,
//! and they queue work with [`ChatsLive::on`] / [`ChatsLive::later`], which
//! hand them the whole [`ChatsWorld`] and the frame's `Actions`.
//!
//! The library has no image node yet, so every picture is a disc: a
//! person's or a community's picture (`faces::texture_for`,
//! `communities::picture`) is its coloured initial ([`disc`]), and the You
//! page's preview polaroid is a line saying a picture is ready.

use std::cell::RefCell;
use std::rc::Rc;

use bevy::ecs::system::{NonSendMarker, SystemParam};
use bevy::prelude::*;
// The library's, not Bevy's: an explicit import outranks the two globs.
use mithril::prelude::*;
use mithril::prelude::{Button, Event};

use crate::resources::{AuthIdentity, OnlineSession, RightPanel};
use crate::systems::calendar::{Calendar, Pick};
use crate::systems::chat::ChatState;
use crate::systems::chats::{self, Actions, ChatsScreen, ChatsView, NewChoice, NewKind, YouEntry};
use crate::systems::communities::Communities;
use crate::systems::discs::{disc_look, ChatKind};
use crate::systems::faces::{FaceChoice, Faces};
use crate::systems::lib_calendar::{self, CalendarLive};
use crate::systems::lib_chat_conversation::{self, ConversationLive};
use crate::systems::lib_communities::{self, CommunitiesLive};
use crate::systems::lib_contacts::{self, ContactsLive};
use crate::systems::lib_settings::LibScreens;
use crate::systems::ui_input::UiMetrics;
use crate::systems::{icons, theme};
use crate::utils::clock::unix_ms;

// ─── The world the panel works on ────────────────────────────────────────────

/// Every resource the Chats panel reads or changes, in one parameter: what
/// a queued tap ([`ChatsLive::on`]) and a screen's `mirror` are handed.
#[derive(SystemParam)]
pub struct ChatsWorld<'w> {
    pub chat: ResMut<'w, ChatState>,
    pub view: ResMut<'w, ChatsView>,
    pub online: Option<Res<'w, OnlineSession>>,
    pub right_panel: ResMut<'w, RightPanel>,
    pub nav: ResMut<'w, crate::systems::geo_nav::GeoNavState>,
    pub calls: ResMut<'w, crate::systems::calls::Calls>,
    pub contacts: ResMut<'w, crate::systems::contacts::Contacts>,
    pub identity: Res<'w, AuthIdentity>,
    pub communities: ResMut<'w, Communities>,
    pub faces: ResMut<'w, Faces>,
    pub calendar: ResMut<'w, Calendar>,
    pub menu: ResMut<'w, crate::systems::ui::MainMenu>,
    /// The pictures on their way out (`lib_chat_conversation`'s composer).
    pub pictures: Res<'w, crate::systems::chat_pictures::ChatPictures>,
    /// How far the on-screen keyboard reaches up: the composer lifts by it.
    pub keyboard: Option<Res<'w, crate::systems::onscreen_keyboard::KeyboardInset>>,
    // A screen that needs more (the conversation: `ChatPictures`, `Calls`'
    // state) adds its resource here. Every field must be read somewhere, or
    // `unused = "deny"` fails the build.
}

impl ChatsWorld<'_> {
    /// The screen the panel shows (`ChatsView::screen`).
    pub fn screen(&self) -> ChatsScreen {
        self.view.screen(&self.communities, &self.calendar)
    }

    /// Who is signed in: the username and the full address.
    pub fn me(&self) -> (String, String) {
        chats::who_am_i(&self.identity, self.view.demo)
    }

    /// End the frame as egui's panel does (`chats::settle`).
    fn settle(&mut self, act: Actions, now: u64) {
        chats::settle(
            act,
            &mut self.chat,
            &mut self.view,
            self.online.as_deref(),
            &mut self.right_panel,
            &mut self.nav,
            &mut self.calls,
            &mut self.communities,
            &mut self.calendar,
            &mut self.menu,
            now,
        );
    }
}

/// A tap, queued by a view and done by the system against the world as it
/// is then, filling the frame's `Actions` like egui's drawing does.
pub(crate) type Act = Rc<dyn for<'w> Fn(&mut ChatsWorld<'w>, &mut Actions)>;

// ─── The panel's live state ─────────────────────────────────────────────────

/// A text field bridged to a `String` in the world, both ways: what is
/// typed goes to the world, and a change made elsewhere (cleared after a
/// send) comes back to the field.
#[derive(Clone)]
pub struct Typed {
    /// The field's model.
    pub model: Signal<String>,
    /// The value both sides last agreed on.
    last: Rc<RefCell<String>>,
}

impl Typed {
    pub fn new(value: &str) -> Self {
        Typed {
            model: signal(value.to_string()),
            last: Rc::new(RefCell::new(value.to_string())),
        }
    }

    /// What was typed since the last frame, for the world — `None` when
    /// nothing was, or when the world moved too (the world wins).
    pub fn typed(&self, world: &str) -> Option<String> {
        let typed = self.model.get_untracked();
        let mut last = self.last.borrow_mut();
        (typed != *last && world == *last).then(|| {
            last.clone_from(&typed);
            typed
        })
    }

    /// The world's value, into the field when it moved there.
    pub fn follow(&self, world: &str) {
        let moved = *self.last.borrow() != world;
        if moved {
            *self.last.borrow_mut() = world.to_string();
            if self.model.get_untracked() != world {
                self.model.set(world.to_string());
            }
        }
    }
}

/// Who is signed in, as the panel shows them.
#[derive(Clone, PartialEq, Default)]
pub struct Me {
    pub name: String,
    /// `jeroen@cartopolis.org`.
    pub address: String,
    /// The orange dot on your face: a request or a report waits.
    pub waiting: bool,
    /// Your face's sheet is open.
    pub menu_open: bool,
}

/// One row of the list, as drawn.
#[derive(Clone, PartialEq)]
pub struct RowV {
    pub key: String,
    pub name: String,
    pub kind: ChatKind,
    pub preview: String,
    pub time: String,
    pub unread: usize,
    pub community: Option<uuid::Uuid>,
}

impl RowV {
    fn of(r: &chats::Row) -> Self {
        RowV {
            key: r.key.clone(),
            name: r.name.clone(),
            kind: r.kind,
            preview: r.preview.clone(),
            time: r.time.clone(),
            unread: r.unread,
            community: r.community,
        }
    }
}

/// The message-requests row.
#[derive(Clone, PartialEq)]
pub struct RequestsV {
    pub title: String,
    pub sub: String,
    pub first: String,
    pub count: usize,
}

/// One row of "Coming up".
#[derive(Clone, PartialEq)]
pub struct SoonV {
    pub title: String,
    /// "18:00 today · Sanne".
    pub line: String,
    pub meetup: bool,
    /// The answer pill's word, when it shows one.
    pub answer: Option<&'static str>,
    pub pick: Pick,
}

/// The "Coming up" card.
#[derive(Clone, PartialEq)]
pub struct ComingV {
    pub label: &'static str,
    pub soon: Vec<SoonV>,
}

/// The list below the search field, as drawn.
#[derive(Clone, PartialEq, Default)]
pub struct ListState {
    pub requests: Option<RequestsV>,
    pub public: Vec<RowV>,
    pub rows: Vec<RowV>,
    /// A view waits to be sent: a private row is where it goes.
    pub sending: bool,
    pub searching: bool,
    /// "Coming up", unless searching or sending.
    pub coming: Option<ComingV>,
    /// The way into Activity: its words and whether anything is new.
    pub activity: Option<(String, bool)>,
}

/// The You page, as drawn.
#[derive(Clone, PartialEq, Default)]
pub struct YouState {
    pub contacts: usize,
    pub requests: usize,
    pub policy: &'static str,
    /// Reports waiting, for an account that may run something.
    pub admin: Option<usize>,
    pub faces: Vec<FaceChoice>,
    pub faces_enabled: bool,
    /// A new picture waits for Use this or Again.
    pub preview: bool,
    pub note: Option<String>,
}

/// The mounted panel: its signals, mirrored from the world every frame, and
/// the taps queued since the last one. Cloned into every reactive closure;
/// all of it is handles.
#[derive(Clone)]
pub(crate) struct ChatsLive {
    /// Which screen shows: the router follows it.
    pub screen: Signal<ChatsScreen>,
    pub me: Signal<Me>,
    /// `ChatState::notice`: (text, is an error).
    pub notice: Signal<Option<(String, bool)>>,
    /// The view waiting to be sent, by name.
    pub sending: Signal<Option<String>>,
    pub list: Signal<ListState>,
    pub you: Signal<YouState>,
    /// The New sheet, and which of its forms, while it is open.
    pub sheet: Signal<Option<NewKind>>,
    /// Start / Create group cannot be pressed yet.
    pub person_off: Signal<bool>,
    pub group_off: Signal<bool>,
    pub search: Typed,
    pub point_text: Typed,
    pub private_input: Typed,
    pub group_name: Typed,
    pub group_members: Typed,
    /// Laid out for a phone: the whole screen, not a 360 px panel.
    pub compact: bool,
    /// What the World | Community switch over a phone's page needs at the
    /// bottom of a list (px); 0 on a desktop.
    pub bottom: f32,
    /// The conversation's own state (`lib_chat_conversation`).
    pub conversation: ConversationLive,
    /// The communities screens' own state (`lib_communities`).
    pub communities: CommunitiesLive,
    /// The contacts screens' own state (`lib_contacts`).
    pub contacts: ContactsLive,
    /// The calendar's own state (`lib_calendar`).
    pub calendar: CalendarLive,
    acts: Rc<RefCell<Vec<Act>>>,
}

impl ChatsLive {
    /// `f`, to queue later: for a callback that is not an event handler
    /// (an `Alert`'s dismiss, a key handler's Enter).
    pub(crate) fn later(
        &self,
        f: impl for<'w> Fn(&mut ChatsWorld<'w>, &mut Actions) + 'static,
    ) -> Rc<dyn Fn()> {
        let acts = self.acts.clone();
        let f: Act = Rc::new(f);
        Rc::new(move || acts.borrow_mut().push(f.clone()))
    }

    /// A component's handler that queues `f`.
    pub(crate) fn on(
        &self,
        f: impl for<'w> Fn(&mut ChatsWorld<'w>, &mut Actions) + 'static,
    ) -> Option<Handler> {
        let go = self.later(f);
        Some(Rc::new(move |_: &Event| go()) as Handler)
    }

    /// A raw element's `on:click` that queues `f`.
    pub(crate) fn tap(
        &self,
        f: impl for<'w> Fn(&mut ChatsWorld<'w>, &mut Actions) + 'static,
    ) -> impl Fn(&Event) + 'static {
        let go = self.later(f);
        move |_: &Event| go()
    }
}

thread_local! {
    static MOUNTED: RefCell<Option<ChatsLive>> = const { RefCell::new(None) };
}

// ─── The system ──────────────────────────────────────────────────────────────

/// Mounts, bridges and unmounts the Chats panel. `PostUpdate`, before
/// `DriveUi`, as every library screen.
pub fn drive_lib_chats(
    _main: NonSendMarker,
    lib: Res<LibScreens>,
    mut w: ChatsWorld,
    metrics: Res<UiMetrics>,
    mut opened: Local<bool>,
) {
    if !lib.chats {
        return;
    }
    if !w.view.lib {
        w.view.lib = true;
    }
    if lib.open_chats && !*opened {
        *opened = true;
        *w.right_panel = RightPanel::Chat;
    }
    // As egui's: the panel is there only online, or seeded for a capture.
    let open = w.right_panel.is(RightPanel::Chat)
        && (w.online.is_some() || w.view.demo)
        && w.screen().on_library();
    let now = unix_ms();
    let mounted = MOUNTED.with(|m| m.borrow().clone());
    match (open, mounted) {
        (false, None) => {}
        (false, Some(_)) => {
            MOUNTED.with(|m| *m.borrow_mut() = None);
            crate::systems::lib_host::clear_keyed("chats");
        }
        (true, None) => {
            let compact = metrics.compact();
            let live = ChatsLive {
                screen: signal(w.screen()),
                me: signal(Me::default()),
                notice: signal(None),
                sending: signal(None),
                list: signal(ListState::default()),
                you: signal(YouState::default()),
                sheet: signal(None),
                person_off: signal(true),
                group_off: signal(true),
                search: Typed::new(&w.view.search),
                point_text: Typed::new(&w.chat.point_text),
                private_input: Typed::new(&w.chat.private_input),
                group_name: Typed::new(&w.chat.group_name_input),
                group_members: Typed::new(&w.chat.group_members_input),
                compact,
                bottom: if compact {
                    crate::systems::dock::reserved_bottom()
                } else {
                    0.0
                },
                conversation: ConversationLive::new(&w),
                communities: CommunitiesLive::new(&w),
                contacts: ContactsLive::new(&w),
                calendar: CalendarLive::new(&w),
                acts: Rc::new(RefCell::new(Vec::new())),
            };
            // The signals take the world's values before anything reads them.
            mirror(&live, &mut w, now);
            let p = theme::p();
            crate::systems::lib_host::mount_keyed(
                "chats",
                panel(&live),
                p.ink == theme::DARK.ink,
                theme::touch_layout(),
            );
            write_html(&live);
            MOUNTED.with(|m| *m.borrow_mut() = Some(live));
        }
        (true, Some(live)) => {
            // The fields → the world, as egui's `TextEdit`s write it.
            if let Some(v) = live.search.typed(&w.view.search) {
                w.view.search = v;
            }
            if let Some(v) = live.point_text.typed(&w.chat.point_text) {
                w.chat.point_text = v;
            }
            if let Some(v) = live.private_input.typed(&w.chat.private_input) {
                w.chat.private_input = v;
            }
            if let Some(v) = live.group_name.typed(&w.chat.group_name_input) {
                w.chat.group_name_input = v;
            }
            if let Some(v) = live.group_members.typed(&w.chat.group_members_input) {
                w.chat.group_members_input = v;
            }
            // The taps of the frame, into one `Actions`, settled as egui's.
            let acts: Vec<Act> = std::mem::take(&mut *live.acts.borrow_mut());
            let mut act = Actions::default();
            for f in acts {
                f(&mut w, &mut act);
            }
            // Your face, where the list's header draws it, for its sheet
            // (`ui::MainMenu::face`): taken by the sheet every frame.
            if w.screen() == ChatsScreen::List {
                let vp = metrics.viewport_pts();
                act.face_rect = Some(egui::Rect::from_min_size(
                    egui::pos2(vp.x - 52.0, 8.0),
                    egui::vec2(40.0, 40.0),
                ));
            }
            w.settle(act, now);
            mirror(&live, &mut w, now);
            // A copy the conversation asked for: to the system clipboard, the
            // one egui's copies reach too (`end_egui_frame`).
            if let Some(text) = lib_chat_conversation::take_copy(&live) {
                crate::utils::clipboard::set_text(&text);
            }
        }
    }
}

/// Write `value` into `s` only when it differs: a write re-runs every view
/// that reads it.
fn set<T: Clone + PartialEq + 'static>(s: &Signal<T>, value: T) -> bool {
    let moved = s.get_untracked() != value;
    if moved {
        s.set(value);
    }
    moved
}

/// The world → the panel's signals, where they moved.
fn mirror(live: &ChatsLive, w: &mut ChatsWorld, now: u64) {
    let screen = w.screen();
    set(&live.screen, screen.clone());
    let (name, address) = w.me();
    set(
        &live.me,
        Me {
            name,
            address,
            waiting: chats::face_waiting(&w.contacts, &w.view),
            menu_open: w.menu.open,
        },
    );
    set(
        &live.notice,
        w.chat.notice.as_ref().map(|n| (n.text.clone(), n.error)),
    );
    set(&live.sending, chats::sending_name(&w.chat));
    live.search.follow(&w.view.search);
    live.point_text.follow(&w.chat.point_text);
    live.private_input.follow(&w.chat.private_input);
    live.group_name.follow(&w.chat.group_name_input);
    live.group_members.follow(&w.chat.group_members_input);
    let sheet = (w.view.new_chat && screen == ChatsScreen::List).then_some(w.view.new_kind);
    set(&live.sheet, sheet);
    set(&live.person_off, !chats::new_person_ready(&w.chat));
    set(&live.group_off, !chats::new_group_ready(&w.chat));
    let mut moved = false;
    match &screen {
        ChatsScreen::List => moved |= set(&live.list, list_state(w, now)),
        ChatsScreen::You => moved |= set(&live.you, you_state(w)),
        _ => {}
    }
    lib_chat_conversation::mirror(live, w, now);
    lib_communities::mirror(live, w, now);
    moved |= lib_contacts::mirror(live, w, now);
    moved |= lib_calendar::mirror(live, w, now);
    // Rows land after the mount (a picture resolving, a line arriving): the
    // HTML beside a capture follows them.
    if moved {
        write_html(live);
    }
}

/// The list as it stands, from the same model egui draws.
fn list_state(w: &mut ChatsWorld, now: u64) -> ListState {
    let model = chats::list_model(None, &w.chat, &w.view, &w.contacts, &mut w.communities);
    let quiet = !model.searching && !model.sending;
    let coming = quiet.then(|| {
        let c = crate::systems::calendar::coming_up_list(&w.calendar, &w.chat, &w.communities, now);
        ComingV {
            label: c.label,
            soon: c
                .soon
                .iter()
                .map(|s| SoonV {
                    title: s.item.title(),
                    line: format!(
                        "{} {} · {}",
                        chats::clock_time(s.item.start_ms),
                        s.day,
                        s.chat
                    ),
                    meetup: s.item.is_meetup(),
                    answer: crate::systems::calendar::shows_answer(&s.item, now)
                        .then(|| crate::systems::calendar::answer_word(&s.item)),
                    pick: s.item.pick(),
                })
                .collect(),
        }
    });
    let activity = if quiet {
        crate::systems::community_posts::activity_label(&mut w.communities)
    } else {
        None
    };
    ListState {
        requests: chats::requests_summary(&w.contacts).map(|(title, sub, people)| RequestsV {
            title,
            sub,
            first: people.first().cloned().unwrap_or_default(),
            count: w.contacts.incoming.len(),
        }),
        public: model.public.iter().map(RowV::of).collect(),
        rows: model.rows.iter().map(RowV::of).collect(),
        sending: model.sending,
        searching: model.searching,
        coming,
        activity,
    }
}

fn you_state(w: &ChatsWorld) -> YouState {
    let (me, _) = w.me();
    YouState {
        contacts: w.contacts.contacts.len(),
        requests: w.contacts.incoming.len(),
        policy: crate::systems::contacts::policy_word(w.contacts.settings.policy),
        admin: w.view.admin,
        faces: w.faces.choices(&me),
        faces_enabled: w.faces.choices_enabled(),
        preview: w.faces.preview.is_some(),
        note: w.faces.note.clone(),
    }
}

/// `CARTO_UI_LIB_HTML=<path>`: the panel as HTML, for the browser beside a
/// capture.
fn write_html(l: &ChatsLive) {
    if let Some(path) = std::env::var_os("CARTO_UI_LIB_HTML") {
        if let Err(e) = std::fs::write(&path, html(l)) {
            warn!(?path, error = %e, "lib chats: could not write the HTML");
        }
    }
}

/// The panel's HTML from the library's string renderer.
fn html(l: &ChatsLive) -> String {
    use mithril::view::{mount, StringRenderer};
    let r = Rc::new(RefCell::new(StringRenderer::new()));
    let root = r.borrow().root();
    let mounted = mount(panel(l), &r, &root, None);
    let out = r.borrow().html();
    drop(mounted);
    out
}

// ─── Pieces every Chats screen uses ──────────────────────────────────────────

/// An icon glyph, 20 px.
pub(crate) fn icon(glyph: &'static str) -> View {
    view! {
        <span style="font-family: 'Material Symbols Rounded'; font-size: 20px; line-height: 1">{glyph}</span>
    }
}

/// A group's heading, as lib_search sets them.
pub(crate) fn group(text: impl Into<String>) -> View {
    let text: String = text.into();
    view! {
        <p style="margin: 8px 0 0; padding-inline: 4px; color: var(--m-text-dim); font-size: var(--m-font-size-sm); font-weight: 650; letter-spacing: 0.06em; text-transform: uppercase">{text}</p>
    }
}

/// A quiet line of explanation.
pub(crate) fn note(text: impl Into<String>) -> View {
    let text: String = text.into();
    view! {
        <p style="margin: 0; padding-inline: 4px; color: var(--m-text-dim); font-size: var(--m-font-size-sm)">{text}</p>
    }
}

/// A screen's header: `lead` (a back arrow, an icon), the title, and `end`
/// (buttons) at the right. Every Chats screen's header is this one.
pub(crate) fn header(lead: View, title: View, end: View) -> View {
    // A sub-page (a back arrow first) sets its title at 17 px and keeps its
    // buttons close, as the other sub-pages do: with 12 px gaps and three
    // buttons a phone left "Contacts" two letters.
    let sub = !matches!(lead, View::Empty);
    let (bar, size) = if sub {
        (
            "display: flex; align-items: center; gap: 4px; flex: none; min-height: 56px; padding: 0 8px; border-bottom: 1px solid var(--m-border)",
            "flex: 1; min-width: 0; margin: 0 0 0 4px; font-size: 17px; font-weight: 600; color: var(--m-text-strong); white-space: nowrap; text-overflow: ellipsis; overflow: hidden",
        )
    } else {
        (
            "display: flex; align-items: center; gap: 12px; flex: none; min-height: 56px; padding: 0 12px 0 20px; border-bottom: 1px solid var(--m-border)",
            "flex: 1; min-width: 0; margin: 0; font-size: 19px; font-weight: 600; color: var(--m-text-strong); white-space: nowrap; text-overflow: ellipsis; overflow: hidden",
        )
    };
    view! {
        <div style={bar}>
            {lead}
            <h1 style={size}>{title}</h1>
            {end}
        </div>
    }
}

/// A back arrow for a header's `lead`.
pub(crate) fn back(onclick: Option<Handler>, title: &'static str) -> View {
    view! {
        <Button variant=Variant::Ghost icon={icon(icons::BACK)} icon_only=true onclick={onclick} title={title} />
    }
}

/// A screen's scrolling body's style: roomy, a column; on a phone it keeps
/// clear of the World | Community switch at the bottom.
pub(crate) fn body_style(l: &ChatsLive) -> String {
    format!(
        "flex: 1; min-height: 0; overflow-y: auto; padding: 20px 20px {}px; display: flex; flex-direction: column; gap: 16px",
        28.0 + l.bottom
    )
}

/// A chat's disc with no picture: its initial (or icon) on its colour, round
/// for a person and a rounded square for a community — `discs::disc_look`,
/// the same look egui paints. The tone is the person's or the community's
/// own (`discs::AVATAR_TONES`), content rather than chrome, so it is not a
/// theme token. A picture is drawn as this disc too (no image node yet).
pub(crate) fn disc(name: &str, kind: ChatKind, size: u32) -> View {
    let look = disc_look(name, kind);
    let radius = if look.square {
        format!("{}px", (size as f32 * 0.29).round())
    } else {
        "var(--m-radius-full)".to_string()
    };
    let font = if look.icon {
        "font-family: 'Material Symbols Rounded'; "
    } else {
        ""
    };
    let style = format!(
        "flex: none; display: flex; align-items: center; justify-content: center; width: {size}px; height: {size}px; border-radius: {radius}; background: #{:02x}{:02x}{:02x}; color: var(--m-on-accent); {font}font-size: {}px; font-weight: 600; line-height: 1",
        look.tone.r(),
        look.tone.g(),
        look.tone.b(),
        (size as f32 * look.scale).round(),
    );
    view! { <span style={style}>{look.text}</span> }
}

/// An unread count, as a pill.
fn count(n: usize) -> View {
    if n == 0 {
        return View::Empty;
    }
    let text = if n > 99 {
        "99+".to_string()
    } else {
        n.to_string()
    };
    view! { <Badge tone=Tone::Accent>{text}</Badge> }
}

// ─── The frame and the router ────────────────────────────────────────────────

/// The panel: the right-hand 360 px on a desktop, the whole screen on a
/// phone, with whichever screen shows in it.
fn panel(l: &ChatsLive) -> View {
    let frame = if l.compact {
        "position: absolute; left: 0; right: 0; top: 0; bottom: 0; display: flex; flex-direction: column; background: var(--m-bg)"
    } else {
        "position: absolute; right: 0; top: 0; bottom: 0; width: 360px; display: flex; flex-direction: column; background: var(--m-bg); border-left: 1px solid var(--m-border)"
    };
    let route = {
        let l = l.clone();
        move || -> View {
            match l.screen.get() {
                ChatsScreen::List => list_page(&l),
                ChatsScreen::You => you_page(&l),
                ChatsScreen::Chat(key) => lib_chat_conversation::screen(&l, &key),
                ChatsScreen::Communities => lib_communities::screen(&l),
                ChatsScreen::Contacts(sub) => lib_contacts::screen(&l, &sub),
                ChatsScreen::Calendar => lib_calendar::screen(&l),
            }
        }
    };
    view! { <div style={frame}>{route}</div> }
}

// ─── The list ────────────────────────────────────────────────────────────────

/// Your face at the right of the list's header: the same sheet as the
/// search bar's face (`ui::render_main_menu`), with the orange dot when
/// something waits for you.
fn face_button(l: &ChatsLive, me: &Me) -> View {
    let dot: View = if me.waiting {
        view! {
            <span style="position: absolute; right: 0; top: 0; width: 12px; height: 12px; border-radius: var(--m-radius-full); background: var(--m-accent); border: 2px solid var(--m-bg)"></span>
        }
    } else {
        View::Empty
    };
    let ring = if me.menu_open {
        "border: 2px solid var(--m-accent);"
    } else {
        "border: 0;"
    };
    let style = format!(
        "position: relative; flex: none; width: 40px; height: 40px; margin: 0; padding: 0; {ring} border-radius: var(--m-radius-full); background: none; cursor: pointer"
    );
    let open = l.tap(|_, a| a.face = true);
    let name = if me.name.is_empty() {
        "?"
    } else {
        me.name.as_str()
    };
    view! {
        <button title="You: your profile, settings and sign out" style={style} on:click={open}>
            {disc(name, ChatKind::Private, 40)}
            {dot}
        </button>
    }
}

/// The list: the name and your face, a notice, the view waiting to be sent,
/// the search field, and under it the requests, the public chats, what is
/// coming up and every conversation; the New sheet over it.
fn list_page(l: &ChatsLive) -> View {
    let head = {
        let l = l.clone();
        move || -> View {
            let me = l.me.get();
            let new = l.on(|w, _| {
                w.view.new_chat = true;
                w.view.new_kind = NewKind::Choose;
            });
            header(
                view! { <span style="color: var(--m-accent)">{icon(RightPanel::Chat.icon())}</span> },
                "Community".into_view(),
                view! {
                    <Button variant=Variant::Ghost icon={icon(icons::ADD)} icon_only=true onclick={new} title="New: a person, a group or a community" />
                    {face_button(&l, &me)}
                },
            )
        }
    };
    let notice = {
        let l = l.clone();
        move || -> View {
            let Some((text, error)) = l.notice.get() else {
                return View::Empty;
            };
            let dismiss = Some(l.later(|_, a| a.dismiss_notice = true));
            let tone = if error { Tone::Danger } else { Tone::Info };
            view! { <Alert tone={tone} ondismiss={dismiss}>{text}</Alert> }
        }
    };
    // Its own closure, so the list moving under it never rebuilds its field.
    let sending = {
        let l = l.clone();
        move || -> View {
            let Some(name) = l.sending.get() else {
                return View::Empty;
            };
            let cancel = l.on(|_, a| a.cancel_point = true);
            let title = format!("Send this view: {name}");
            view! {
                <div style="display: flex; flex-direction: column; gap: 12px; padding: 16px; border: 1px solid var(--m-border); border-radius: var(--m-radius-lg); background: var(--m-surface)">
                    <div style="display: flex; align-items: center; gap: 8px; color: var(--m-text-strong); font-weight: 600">
                        <span style="color: var(--m-accent)">{icon(icons::PIN)}</span>{title}
                    </div>
                    <FieldText model={l.point_text.model.clone()} placeholder="Add a message (optional)" id="chats-point-text" />
                    <div style="display: flex; align-items: center; gap: 12px">
                        <div style="flex: 1">{note("Tap a private chat below.")}</div>
                        <Button size=Size::Sm onclick={cancel}>"Cancel"</Button>
                    </div>
                </div>
            }
        }
    };
    let rows = {
        let l = l.clone();
        move || -> View { list_rows(&l, &l.list.get()) }
    };
    let sheet = {
        let l = l.clone();
        move || -> View {
            match l.sheet.get() {
                Some(kind) => new_sheet(&l, kind),
                None => View::Empty,
            }
        }
    };
    let search = l.search.model.clone();
    let body = body_style(l);
    view! {
        {head}
        <div style={body}>
            {notice}
            {sending}
            <FieldText model={search} r#type=TextType::Search placeholder="Search" autocomplete="off" prefix={icon(icons::SEARCH)} id="chats-search" />
            {rows}
        </div>
        {sheet}
    }
}

/// Everything under the search field.
fn list_rows(l: &ChatsLive, s: &ListState) -> View {
    let requests: View = match &s.requests {
        Some(r) => {
            let open = l.on(|_, a| a.sub = Some(crate::systems::contacts::Sub::Requests));
            let sub = r.sub.clone();
            let title = r.title.clone();
            view! {
                <List>
                    <ListItem icon={disc(&r.first, ChatKind::Private, 32)} sub={sub.into_view()} end={count(r.count)} chevron=true onclick={open}>{title}</ListItem>
                </List>
            }
        }
        None => View::Empty,
    };
    let public: View = if s.public.is_empty() {
        View::Empty
    } else {
        let tiles: Vec<View> = s.public.iter().map(|r| public_tile(l, r)).collect();
        view! { <div style="display: flex; gap: 12px">{tiles}</div> }
    };
    let coming: View = match &s.coming {
        Some(c) => coming_up(l, c),
        None => View::Empty,
    };
    let activity: View = match &s.activity {
        Some((label, new)) => {
            let open = l.on(|w, _| {
                w.communities.screen = Some(crate::systems::communities::Screen::Activity);
            });
            let variant = if *new {
                Variant::Primary
            } else {
                Variant::Secondary
            };
            let label = label.clone();
            view! {
                <div style="display: flex">
                    <Button variant={variant} size=Size::Sm icon={icon(icons::STAR)} onclick={open}>{label}</Button>
                </div>
            }
        }
        None => View::Empty,
    };
    let conversations: View = if s.rows.is_empty() {
        View::Empty
    } else {
        let rows: Vec<View> = s.rows.iter().map(|r| chat_row(l, r, s.sending)).collect();
        view! {
            {group("Conversations")}
            <List>{rows}</List>
        }
    };
    let empty: View = if s.rows.is_empty() && s.searching {
        note("Nothing here goes by that name.")
    } else if !s.rows.iter().any(|r| r.kind == ChatKind::Private) {
        note("Start a private chat with the + above. Only the people in it can read it.")
    } else {
        View::Empty
    };
    view! {
        {requests}
        {public}
        {coming}
        {activity}
        {conversations}
        {empty}
    }
}

/// Here or Everyone, half the list's width.
fn public_tile(l: &ChatsLive, r: &RowV) -> View {
    let key = r.key.clone();
    let open = l.tap(move |_, a| a.open = Some(key.clone()));
    let name = r.name.clone();
    let preview = r.preview.clone();
    view! {
        <button title={name.clone()} style="flex: 1; min-width: 0; display: flex; flex-direction: column; gap: 10px; margin: 0; padding: 14px; border: 1px solid var(--m-border); border-radius: var(--m-radius-lg); background: var(--m-surface); color: var(--m-text); text-align: start; font: inherit; cursor: pointer" on:click={open}>
            <span style="display: flex; align-items: center; gap: 10px; min-width: 0">
                {disc(&r.name, r.kind, 28)}
                <span style="flex: 1; min-width: 0; color: var(--m-text-strong); font-weight: 600; white-space: nowrap; text-overflow: ellipsis; overflow: hidden">{name}</span>
                {count(r.unread)}
            </span>
            <span style="color: var(--m-text-dim); font-size: var(--m-font-size-sm); white-space: nowrap; text-overflow: ellipsis; overflow: hidden">{preview}</span>
        </button>
    }
}

/// A row's second line on one line, cut with "…" as egui's: a list
/// item's sub wraps, and a preview ran to three lines beside its time.
const ONE_LINE: &str =
    "display: block; white-space: nowrap; overflow: hidden; text-overflow: ellipsis";

/// One conversation or community: its disc, its name (a private chat's with
/// the padlock), its last line, the time and the unread count.
fn chat_row(l: &ChatsLive, r: &RowV, sending: bool) -> View {
    let (key, kind, community) = (r.key.clone(), r.kind, r.community);
    let open = l.on(move |_, a| chats::tap_row(&key, kind, community, sending, a));
    let name = r.name.clone();
    let title: View = if r.kind == ChatKind::Private {
        view! {
            <span style="font-family: 'Material Symbols Rounded'; font-size: 14px; color: var(--m-text-dim); margin-right: 6px">{icons::LOCK}</span>
            <span>{name}</span>
        }
    } else {
        name.into_view()
    };
    let (preview, colour) = if sending && r.kind == ChatKind::Private {
        (
            "Tap to send the view here".to_string(),
            "color: var(--m-accent-text); ",
        )
    } else if r.unread > 0 {
        (r.preview.clone(), "color: var(--m-text); ")
    } else {
        (r.preview.clone(), "")
    };
    let sub: View = view! { <span style={format!("{colour}{ONE_LINE}")}>{preview}</span> };
    let time = r.time.clone();
    let end: View = view! { <span>{time}</span>{count(r.unread)} };
    view! {
        <ListItem icon={disc(&r.name, r.kind, 32)} sub={sub} end={end} onclick={open}>{title}</ListItem>
    }
}

/// What is coming up from every chat, and the way into the calendar
/// (`calendar::coming_up`).
fn coming_up(l: &ChatsLive, c: &ComingV) -> View {
    let open = l.on(|_, a| a.calendar = true);
    let rows: View = if c.soon.is_empty() {
        View::Empty
    } else {
        let rows: Vec<View> = c
            .soon
            .iter()
            .map(|s| {
                let pick = s.pick.clone();
                let tap = l.on(move |_, a| a.calendar_pick = Some(pick.clone()));
                let glyph = if s.meetup {
                    icons::FLAG
                } else {
                    icons::VIDEOCAM
                };
                let end: View = match s.answer {
                    Some(word) => {
                        let tone = match word {
                            "Going" => Tone::Success,
                            "Maybe" => Tone::Warning,
                            "Answer" => Tone::Accent,
                            _ => Tone::Neutral,
                        };
                        view! { <Badge tone={tone}>{word}</Badge> }
                    }
                    None => View::Empty,
                };
                let (title, line) = (s.title.clone(), s.line.clone());
                let sub: View = view! { <span style={ONE_LINE}>{line}</span> };
                view! {
                    <ListItem icon={icon(glyph)} sub={sub} end={end} onclick={tap}>{title}</ListItem>
                }
            })
            .collect();
        view! { <List>{rows}</List> }
    };
    view! {
        <div style="display: flex; flex-direction: column; gap: 8px">
            <div style="display: flex; align-items: center; gap: 8px">
                <div style="flex: 1">{group(c.label)}</div>
                <Button variant=Variant::Ghost size=Size::Sm onclick={open}>"Calendar"</Button>
            </div>
            {rows}
        </div>
    }
}

// ─── The New sheet ───────────────────────────────────────────────────────────

/// The "+" sheet over the dimmed list: the four choices, or the form for a
/// person or a group (`chats::new_sheet`). A tap on the dimmed list closes it.
fn new_sheet(l: &ChatsLive, kind: NewKind) -> View {
    let close = l.tap(|w, _| w.view.new_chat = false);
    let to_choose = l.on(|w, _| w.view.new_kind = NewKind::Choose);
    let title = |t: &'static str| -> View {
        view! { <h2 style="margin: 0; font-size: 20px; font-weight: 600; color: var(--m-text-strong)">{t}</h2> }
    };
    let content: View = match kind {
        NewKind::Choose => {
            let choices: Vec<View> = NewChoice::ALL
                .into_iter()
                .map(|c| {
                    let pick = l.on(move |w, a| {
                        let mut k = w.view.new_kind;
                        chats::pick_new(c, &mut k, a);
                        if k != w.view.new_kind {
                            w.view.new_kind = k;
                        }
                    });
                    view! {
                        <ListItem icon={icon(c.icon())} sub={c.line().into_view()} chevron=true onclick={pick}>{c.title()}</ListItem>
                    }
                })
                .collect();
            view! {
                {title("New")}
                <List>{choices}</List>
                <p style="display: flex; align-items: center; gap: 6px; margin: 0; padding-inline: 4px; color: var(--m-text-dim); font-size: var(--m-font-size-sm)">
                    <span style="font-family: 'Material Symbols Rounded'; font-size: 16px; line-height: 1">{icons::LOCK}</span>
                    <span>{chats::NEW_NOTE}</span>
                </p>
            }
        }
        NewKind::Person => {
            let start = l.later(start_private);
            let keys: Handler = Rc::new(move |e: &Event| {
                if e.key.as_deref() == Some("Enter") {
                    start();
                }
            });
            let contacts = l.on(|_, a| a.sub = Some(crate::systems::contacts::Sub::Contacts));
            view! {
                {title("Message a person")}
                <FieldText model={l.private_input.model.clone()} placeholder="Username" autocomplete="off" autofocus=true onkeydown={Some(keys)} id="chats-new-private" />
                <div style="display: flex; gap: 8px; flex-wrap: wrap">
                    <Button onclick={to_choose}>"Back"</Button>
                    <Button onclick={contacts}>"Contacts"</Button>
                    <div style="flex: 1"></div>
                    <Button variant=Variant::Primary disabled={l.person_off.clone()} onclick={l.on(start_private)} title="Type who to write to first">"Start"</Button>
                </div>
            }
        }
        NewKind::Group => {
            let create = l.on(|w, a| {
                if chats::new_group_ready(&w.chat) {
                    a.create_group = true;
                }
            });
            view! {
                {title("Start a group")}
                <FieldText model={l.group_name.model.clone()} placeholder="Group name" autocomplete="off" autofocus=true id="chats-group-name" />
                <FieldText model={l.group_members.model.clone()} placeholder="Usernames, separated by commas" autocomplete="off" id="chats-group-members" />
                <div style="display: flex; gap: 8px">
                    <Button onclick={to_choose}>"Back"</Button>
                    <div style="flex: 1"></div>
                    <Button variant=Variant::Primary disabled={l.group_off.clone()} onclick={create} title="Give it a name and at least one person">"Create group"</Button>
                </div>
            }
        }
    };
    let sheet = format!(
        "position: relative; display: flex; flex-direction: column; gap: 16px; padding: 8px 20px {}px; background: var(--m-surface); border-top: 1px solid var(--m-border); border-radius: 24px 24px 0 0; box-shadow: var(--m-shadow-lg)",
        28.0 + l.bottom
    );
    view! {
        <div style="position: absolute; left: 0; right: 0; top: 0; bottom: 0; display: flex; flex-direction: column; justify-content: flex-end">
            <div title="Dismiss" style="position: absolute; left: 0; right: 0; top: 0; bottom: 0; background: var(--m-backdrop)" on:click={close}></div>
            <div style={sheet}>
                <div style="align-self: center; width: 40px; height: 5px; border-radius: 3px; background: var(--m-border-strong)"></div>
                {content}
            </div>
        </div>
    }
}

/// "Message a person": start the chat with the name typed, once there is one.
fn start_private(w: &mut ChatsWorld, a: &mut Actions) {
    if chats::new_person_ready(&w.chat) {
        a.start_private = Some(w.chat.private_input.trim().to_string());
    }
}

// ─── The You page ────────────────────────────────────────────────────────────

/// The You page (`chats::you_screen`): your disc and the ways to change it,
/// your name and address, your link and avatar, your contacts, requests and
/// privacy, and Settings, Admin and Sign out.
fn you_page(l: &ChatsLive) -> View {
    let lead = back(l.on(|_, a| a.you = Some(false)), "Back to the list");
    let content = {
        let l = l.clone();
        move || -> View { you_body(&l, &l.me.get(), &l.you.get()) }
    };
    let body = body_style(l);
    view! {
        {header(lead, "You".into_view(), View::Empty)}
        <div style={body}>{content}</div>
    }
}

fn you_body(l: &ChatsLive, me: &Me, y: &YouState) -> View {
    let entry = |e: YouEntry| l.on(move |_, a| chats::pick_you(e, a));
    let choices: Vec<View> = y
        .faces
        .iter()
        .map(|&c| {
            let pick = l.on(move |w, a| w.faces.choose(c, &mut a.panel));
            view! {
                <Button size=Size::Sm icon={icon(c.icon())} disabled={!y.faces_enabled} onclick={pick}>{c.label()}</Button>
            }
        })
        .collect();
    // The preview is a picture, which the library cannot draw yet.
    let ready: View = if y.preview {
        note("Your new picture is ready: use it?")
    } else {
        View::Empty
    };
    let face_note: View = match &y.note {
        Some(n) => note(n.clone()),
        None => View::Empty,
    };
    let disc_name = if me.name.is_empty() {
        "?"
    } else {
        me.name.as_str()
    };
    let (name, address) = (me.name.clone(), me.address.clone());
    let policy = y.policy;
    let admin: View = match y.admin {
        Some(waiting) => view! {
            <ListItem icon={icon(RightPanel::Admin.icon())} end={count(waiting)} tone={Some(Tone::Warning)} chevron=true onclick={entry(YouEntry::Admin)}>{RightPanel::Admin.label()}</ListItem>
        },
        None => View::Empty,
    };
    let contacts = y.contacts.to_string();
    view! {
        <div style="display: flex; flex-direction: column; align-items: center; gap: 12px; text-align: center">
            {disc(disc_name, ChatKind::Private, 96)}
            {ready}
            <div style="display: flex; flex-wrap: wrap; justify-content: center; gap: 8px">{choices}</div>
            {face_note}
            <div style="display: flex; flex-direction: column; gap: 4px">
                <h2 style="margin: 0; font-size: 19px; font-weight: 600; color: var(--m-text-strong)">{name}</h2>
                <span style="color: var(--m-text-dim); font-size: var(--m-font-size-sm)">{address}</span>
            </div>
            <div style="display: flex; flex-wrap: wrap; justify-content: center; gap: 8px">
                <Button icon={icon(icons::SHARE)} onclick={entry(YouEntry::ShareLink)}>"Share my link"</Button>
                <Button icon={icon(icons::APPEARANCE)} onclick={entry(YouEntry::Avatar)}>"Avatar"</Button>
            </div>
        </div>
        <List>
            <ListItem icon={icon(icons::GROUP)} end={contacts.into_view()} chevron=true onclick={entry(YouEntry::Contacts)}>"Contacts"</ListItem>
            <ListItem icon={icon(icons::CHAT)} end={count(y.requests)} chevron=true onclick={entry(YouEntry::Requests)}>"Message requests"</ListItem>
            <ListItem icon={icon(icons::LOCK)} sub={policy.into_view()} chevron=true onclick={entry(YouEntry::Privacy)}>"Who can message me"</ListItem>
        </List>
        <List>
            <ListItem icon={icon(icons::SETTINGS)} chevron=true onclick={entry(YouEntry::Settings)}>"Settings"</ListItem>
            {admin}
            <ListItem icon={icon(icons::LOGOUT)} onclick={entry(YouEntry::SignOut)}>"Sign out"</ListItem>
        </List>
    }
}
