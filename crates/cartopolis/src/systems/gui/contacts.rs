//! Contacts and message requests: the screens and the state behind them
//! (`docs/notes/contacts.md`, design `docs/mockups/chat-world/v5-contacts`).
//!
//! A contact is somebody you accepted. Anybody else's first message arrives as
//! a **request**, held apart from your chats until you accept it. The server
//! enforces every rule (`cartopolis_simulator::contacts`); this module only
//! draws what it last said and asks it for changes, the way the block list
//! does. The screens:
//!
//!  * **The person card**, over the map, when an avatar is tapped: who it is,
//!    how near, whether the account is new, and *Say hi* / *Add contact*.
//!  * **The first-message sheet**, from *Say hi*: one line, text only.
//!  * In the Chats panel ([`Sub`]): the requests tray, one request, the
//!    contacts list, add-by-link-or-username, and the privacy setting.
//!
//! # How a first message travels
//!
//! *Say hi* sends a `RequestContact` and parks the line in [`Contacts::hellos`].
//! Only once the server's `ContactsState` lists the request as sent does it
//! start the private chat and hand the line over (`ChatState::private_hello`),
//! because the server refuses to hand out somebody's keys to a person with no
//! request to them — starting the chat first would race the request and lose.

use bevy::math::DVec3;
use bevy::prelude::*;
use big_space::prelude::GridCell;
use cartopolis_core::{
    ContactDto, ContactRequestDto, ContactSettings, MetDto, NearbyPersonDto, NetMessage, PersonDto,
    Relation, RequestPolicy,
};
use std::collections::HashSet;

use crate::components::{FreeCamera, RemoteAvatar};
use crate::resources::{OnlineSession, RightPanel};
use crate::systems::chat::{ChatNotice, ChatState, ReportDraft};
use crate::systems::chats::{avatar, glass_frame, raised_frame, ChatKind, ChatsView};
use crate::systems::egui_manager::EguiManager;
use crate::systems::{icons, theme};

/// Request ids for contact traffic live in their own range, so a refusal
/// (`ServerError`) finds its way here and not to the prim editor.
const REQUEST_BASE: u32 = 0xC0A7_0000;

/// Whether a request id belongs to contacts.
#[must_use]
pub fn is_contacts_request(request_id: u32) -> bool {
    request_id & 0xFFFF_0000 == REQUEST_BASE
}

/// How often the contacts screen asks who is near, while it is open.
const NEARBY_EVERY_S: f64 = 5.0;

/// How often a chat's World view asks where the people in it are. A marker
/// glides from each answer to the next over this long, so it moves at walking
/// pace rather than hopping.
pub const WHERE_EVERY_S: f64 = 2.0;

/// A move longer than this between two answers is a jump, not a walk: the
/// marker is put straight there rather than sliding across the country.
const WHERE_JUMP_M: f64 = 2_000.0;

/// Where one person in the open chat is, as the server last said
/// (`NetMessage::PeopleWhere`), with the answer before it to glide from.
#[derive(Clone, Debug, PartialEq)]
pub struct Whereabout {
    pub username: String,
    /// Their avatar's id, the same as `RemoteAvatar::sim_id`.
    pub avatar_id: u64,
    from: (f64, f64),
    to: (f64, f64),
    /// When `to` arrived, in `Time::elapsed_secs_f64`.
    arrived: f64,
}

impl Whereabout {
    /// Where to draw them at `now`: on the way from the last answer to the
    /// newest, reaching it as the next one is due.
    #[must_use]
    pub fn at(&self, now: f64) -> (f64, f64) {
        let t = ((now - self.arrived) / WHERE_EVERY_S).clamp(0.0, 1.0);
        (
            self.from.0 + (self.to.0 - self.from.0) * t,
            self.from.1 + (self.to.1 - self.from.1) * t,
        )
    }
}

/// Fold a `PeopleWhere` answer into what is known: anybody it leaves out is
/// gone (they went offline, hid it, or left the world), and anybody still there
/// glides on from wherever they are drawn now.
fn merge_whereabouts(
    known: &[Whereabout],
    people: Vec<cartopolis_core::PersonWhereDto>,
    now: f64,
) -> Vec<Whereabout> {
    people
        .into_iter()
        .map(|p| {
            let to = (p.latitude, p.longitude);
            let from = known
                .iter()
                .find(|w| w.username.eq_ignore_ascii_case(&p.username))
                .map(|w| w.at(now))
                .filter(|&(lat, lng)| {
                    cartopolis_core::geo::ground_distance_m(lat, lng, to.0, to.1) < WHERE_JUMP_M
                })
                .unwrap_or(to);
            Whereabout {
                username: p.username,
                avatar_id: p.avatar_id,
                from,
                to,
                arrived: now,
            }
        })
        .collect()
}

/// A tap this close to an avatar's feet, in points, is a tap on the avatar.
const TAP_RADIUS_PTS: f32 = 36.0;

/// Width of the person card and the first-message sheet on a wide window.
const CARD_WIDTH: f32 = 380.0;

/// Longest first message, in characters. A knock, not a letter.
const MAX_HELLO_CHARS: usize = 500;

/// Contact traffic the network system sorted out of the mailbox for
/// [`drive_contacts`], as it does for private chats.
#[derive(Resource, Default)]
pub struct ContactsInbox(pub Vec<NetMessage>);

impl ContactsInbox {
    /// Whether a message from the server is contact traffic.
    #[must_use]
    pub fn wants(msg: &NetMessage) -> bool {
        match msg {
            NetMessage::ContactsState { .. }
            | NetMessage::PersonInfo { .. }
            | NetMessage::NearbyPeople { .. }
            | NetMessage::PeopleWhere { .. } => true,
            NetMessage::ServerError { request_id, .. } => is_contacts_request(*request_id),
            _ => false,
        }
    }
}

/// Which contacts screen the Chats panel is showing instead of the list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Sub {
    /// Every waiting request.
    Requests,
    /// One request, from this person.
    Request(String),
    /// Your contacts, people near you, and requests you sent.
    Contacts,
    /// Your link and code, and finding somebody by username.
    Add,
    /// Who may send you a request.
    Privacy,
}

/// The person card, over the map.
#[derive(Clone, Debug, Default)]
pub struct Card {
    /// What the server said about them; `None` while it has not answered, or
    /// when there is nobody to show.
    pub person: Option<PersonDto>,
    pub looking: bool,
    /// The code from a contact link that opened this card, sent with a request.
    pub link_code: Option<String>,
    /// The ⋯ menu is open.
    pub menu: bool,
}

/// The first-message sheet.
#[derive(Clone, Debug, Default)]
pub struct Compose {
    pub username: String,
    pub text: String,
    pub link_code: Option<String>,
    claim_focus: bool,
}

/// A first message waiting for its request to be on record.
#[derive(Clone, Debug)]
struct Hello {
    request: u32,
    username: String,
    text: String,
}

/// Everything about contacts, as the server last said, plus what the screens
/// are in the middle of.
#[derive(Resource, Default)]
pub struct Contacts {
    pub contacts: Vec<ContactDto>,
    pub incoming: Vec<ContactRequestDto>,
    pub outgoing: Vec<ContactRequestDto>,
    pub settings: ContactSettings,
    /// A `ContactsState` has arrived since the session came up.
    pub loaded: bool,
    pub card: Option<Card>,
    pub compose: Option<Compose>,
    /// The username field on the Add screen, and what looking it up found:
    /// `None` not asked, `Some(None)` nobody.
    pub search: String,
    pub found: Option<Option<PersonDto>>,
    pub nearby: Vec<NearbyPersonDto>,
    /// The last refusal, shown where the person is looking.
    pub error: Option<String>,
    /// A contact link this launch was opened with: (username, code).
    pub pending_link: Option<(String, String)>,
    /// Set by a screen each frame it wants [`Self::nearby`] kept fresh.
    pub want_nearby: bool,
    /// Where the people in the chat whose World view is open are, for their
    /// markers (`chat_people`). Empty while no World view is open.
    pub whereabouts: Vec<Whereabout>,
    where_asked_at: Option<f64>,
    /// Whose whereabouts were last asked for, so a different chat asks at once.
    where_asked_for: Vec<String>,
    hellos: Vec<Hello>,
    outbox: Vec<NetMessage>,
    next_request: u32,
    card_request: Option<u32>,
    search_request: Option<u32>,
    nearby_asked_at: Option<f64>,
    /// Requests already seen, so a new one can be announced once.
    seen_incoming: HashSet<String>,
    /// A demo capture with no session behind it (`--ui contacts`).
    pub demo: bool,
}

impl Contacts {
    fn request_id(&mut self) -> u32 {
        self.next_request = self.next_request.wrapping_add(1) & 0xFFFF;
        REQUEST_BASE | self.next_request
    }

    /// Ask to become contacts with `username`.
    pub fn request(&mut self, username: &str, link_code: Option<String>) -> u32 {
        let request_id = self.request_id();
        self.error = None;
        self.outbox.push(NetMessage::RequestContact {
            request_id,
            username: username.to_string(),
            link_code,
        });
        request_id
    }

    /// Ask to become contacts with `username`, with a first message that goes
    /// into the private chat once the request is on record.
    pub fn say_hi(&mut self, username: &str, text: &str, link_code: Option<String>) {
        let text: String = text.trim().chars().take(MAX_HELLO_CHARS).collect();
        let request = self.request(username, link_code);
        if !text.is_empty() {
            self.hellos.push(Hello {
                request,
                username: username.to_string(),
                text,
            });
        }
    }

    pub fn answer(&mut self, username: &str, accept: bool) {
        let request_id = self.request_id();
        self.outbox.push(NetMessage::AnswerContact {
            request_id,
            username: username.to_string(),
            accept,
        });
        // Gone from the tray at once: the server's next state confirms it.
        self.incoming
            .retain(|r| !r.username.eq_ignore_ascii_case(username));
    }

    /// Stop being contacts, or withdraw a request you sent.
    pub fn remove(&mut self, username: &str) {
        let request_id = self.request_id();
        self.outbox.push(NetMessage::RemoveContact {
            request_id,
            username: username.to_string(),
        });
    }

    fn push_settings(&mut self, reset_link: bool) {
        let request_id = self.request_id();
        let s = &self.settings;
        self.outbox.push(NetMessage::SetContactSettings {
            request_id,
            policy: s.policy,
            show_online: s.show_online,
            notify_requests: s.notify_requests,
            separate_new_accounts: s.separate_new_accounts,
            reset_link,
        });
    }

    /// Open the person card for the avatar with this sim id.
    pub fn open_card_for_avatar(&mut self, avatar_id: u64) {
        let request_id = self.request_id();
        self.card_request = Some(request_id);
        self.card = Some(Card {
            looking: true,
            ..Card::default()
        });
        self.outbox.push(NetMessage::LookupPerson {
            request_id,
            username: None,
            avatar_id: Some(avatar_id),
        });
    }

    /// Open the person card for a username (a contact link, a nearby row).
    pub fn open_card_for_name(&mut self, username: &str, link_code: Option<String>) {
        let request_id = self.request_id();
        self.card_request = Some(request_id);
        self.card = Some(Card {
            looking: true,
            link_code,
            ..Card::default()
        });
        self.outbox.push(NetMessage::LookupPerson {
            request_id,
            username: Some(username.to_string()),
            avatar_id: None,
        });
    }

    fn search(&mut self) {
        let name = self.search.trim().to_string();
        if name.is_empty() {
            return;
        }
        // A pasted contact link is a name and a code.
        if let Some((user, code)) = crate::systems::place_link::contact_from_link(&name) {
            self.search.clear();
            self.open_card_for_name(&user, Some(code));
            return;
        }
        let request_id = self.request_id();
        self.search_request = Some(request_id);
        self.found = None;
        self.outbox.push(NetMessage::LookupPerson {
            request_id,
            username: Some(name),
            avatar_id: None,
        });
    }

    /// The waiting request from `username`, if there is one.
    #[must_use]
    pub fn incoming_from(&self, username: &str) -> Option<&ContactRequestDto> {
        self.incoming
            .iter()
            .find(|r| r.username.eq_ignore_ascii_case(username))
    }

    /// Whether a request to `username` is waiting (as far as you can know).
    #[must_use]
    pub fn sent_to(&self, username: &str) -> bool {
        self.outgoing
            .iter()
            .any(|r| r.username.eq_ignore_ascii_case(username))
    }

    #[must_use]
    pub fn is_contact(&self, username: &str) -> bool {
        self.contacts
            .iter()
            .any(|c| c.username.eq_ignore_ascii_case(username))
    }

    /// The channel keys of private chats that are still requests to *you*:
    /// the chat list leaves them out, the requests tray shows them.
    #[must_use]
    pub fn request_channels(&self, chat: &ChatState) -> HashSet<String> {
        self.incoming
            .iter()
            .filter_map(|r| chat.direct.get(&r.username.to_lowercase()).cloned())
            .collect()
    }

    /// This account's contact link.
    #[must_use]
    pub fn my_link(&self, me: &str) -> String {
        crate::systems::place_link::contact_link(me, &self.settings.link_code)
    }
}

fn now_ms() -> u64 {
    web_time::SystemTime::now()
        .duration_since(web_time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// "5 min ago", "yesterday": how long ago a meeting or a request was.
#[must_use]
pub fn ago(at_ms: u64, now_ms: u64) -> String {
    let mins = now_ms.saturating_sub(at_ms) / 60_000;
    match mins {
        0 => "just now".into(),
        1..=59 => format!("{mins} min ago"),
        60..=1439 => format!("{} h ago", mins / 60),
        1440..=2879 => "yesterday".into(),
        _ => format!("{} days ago", mins / 1440),
    }
}

/// "Joined 3 days ago" / "Member since 2025", for the card and the request.
#[must_use]
pub fn joined(joined_ms: u64, new_account: bool, now_ms: u64) -> String {
    if new_account {
        let days = now_ms.saturating_sub(joined_ms) / 86_400_000;
        match days {
            0 => "New · joined today".into(),
            1 => "New · joined yesterday".into(),
            d => format!("New · joined {d} days ago"),
        }
    } else {
        // Civil year from days since the epoch (Hinnant's algorithm): a
        // calendar crate for one number would be a dependency for nothing.
        let days = (joined_ms / 86_400_000) as i64 + 719_468;
        let era = days.div_euclid(146_097);
        let doe = days - era * 146_097;
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let year = yoe + era * 400 + i64::from(mp >= 10);
        format!("Member since {year}")
    }
}

/// "You were near each other · 5 min ago".
#[must_use]
pub fn met_text(met: &MetDto, now_ms: u64) -> String {
    format!("Near each other · {}", ago(met.at_ms, now_ms))
}

/// The contact link a launch was opened with, if any: the Android intent
/// (read once — `platform::android::launch_url` says why), or `CARTO_CONTACT`
/// (`?contact=` on the web).
fn launch_contact() -> Option<(String, String)> {
    #[cfg(target_os = "android")]
    if let Some(url) = crate::platform::android::launch_url() {
        if let Some(found) = crate::systems::place_link::contact_from_link(&url) {
            return Some(found);
        }
    }
    let raw = crate::env_value("CARTO_CONTACT")?;
    crate::systems::place_link::contact_from_link(&raw).or_else(|| {
        // The bare query value: `name~code`.
        let (user, code) = raw.split_once('~')?;
        Some((user.to_string(), code.to_string()))
    })
}

/// Apply what the server said, start the chats first messages were waiting
/// for, keep the nearby list fresh, and send what the screens asked for.
pub fn drive_contacts(
    online: Option<Res<OnlineSession>>,
    time: Res<Time>,
    mut inbox: ResMut<ContactsInbox>,
    mut contacts: ResMut<Contacts>,
    mut chat: ResMut<ChatState>,
    private: Res<crate::systems::private_chat::PrivateChats>,
    mut launched: Local<bool>,
) {
    if !*launched {
        *launched = true;
        contacts.pending_link = launch_contact();
    }
    let Some(online) = online else {
        inbox.0.clear();
        if contacts.loaded && !contacts.demo {
            let link = contacts.pending_link.take();
            *contacts = Contacts {
                pending_link: link,
                ..Contacts::default()
            };
        }
        return;
    };
    for msg in inbox.0.drain(..) {
        match msg {
            NetMessage::ContactsState {
                contacts: list,
                incoming,
                outgoing,
                settings,
            } => {
                let first = !contacts.loaded;
                let fresh: Vec<String> = incoming
                    .iter()
                    .filter(|r| !contacts.seen_incoming.contains(&r.username.to_lowercase()))
                    .map(|r| r.username.clone())
                    .collect();
                if !first && settings.notify_requests {
                    if let Some(who) = fresh.first() {
                        chat.notice = Some(ChatNotice {
                            text: format!("{who} sent you a message request."),
                            error: false,
                        });
                    }
                }
                contacts.seen_incoming =
                    incoming.iter().map(|r| r.username.to_lowercase()).collect();
                contacts.contacts = list;
                contacts.incoming = incoming;
                contacts.outgoing = outgoing;
                contacts.settings = settings;
                contacts.loaded = true;
                refresh_card_relation(&mut contacts);
            }
            NetMessage::PersonInfo { request_id, person } => {
                if contacts.card_request == Some(request_id) {
                    contacts.card_request = None;
                    if let Some(card) = contacts.card.as_mut() {
                        card.looking = false;
                        card.person = person;
                    }
                } else if contacts.search_request == Some(request_id) {
                    contacts.search_request = None;
                    contacts.found = Some(person);
                }
            }
            NetMessage::NearbyPeople { people, .. } => contacts.nearby = people,
            NetMessage::PeopleWhere { people, .. } => {
                let now = time.elapsed_secs_f64();
                contacts.whereabouts = merge_whereabouts(&contacts.whereabouts, people, now);
            }
            NetMessage::ServerError {
                request_id,
                message,
                ..
            } => {
                contacts.hellos.retain(|h| h.request != request_id);
                if contacts.card_request == Some(request_id) {
                    contacts.card_request = None;
                    if let Some(card) = contacts.card.as_mut() {
                        card.looking = false;
                    }
                }
                if contacts.search_request == Some(request_id) {
                    contacts.search_request = None;
                    contacts.found = Some(None);
                }
                contacts.error = Some(message);
            }
            _ => {}
        }
    }

    // First messages whose request is on record now: start the chat and hand
    // the line over.
    let ready: Vec<Hello> = {
        let c = &*contacts;
        c.hellos
            .iter()
            .filter(|h| c.sent_to(&h.username) || c.is_contact(&h.username))
            .cloned()
            .collect()
    };
    if !ready.is_empty() {
        contacts
            .hellos
            .retain(|h| !ready.iter().any(|r| r.request == h.request));
        for h in ready {
            // The canonical spelling of the name, as the server listed it.
            let name = contacts
                .outgoing
                .iter()
                .map(|r| &r.username)
                .chain(contacts.contacts.iter().map(|c| &c.username))
                .find(|n| n.eq_ignore_ascii_case(&h.username))
                .cloned()
                .unwrap_or(h.username);
            chat.private_start.push(name.clone());
            chat.private_hello.push((name, h.text));
        }
    }

    // A contact link this launch was opened with: once signed in, its card.
    if contacts.loaded {
        if let Some((user, code)) = contacts.pending_link.take() {
            contacts.open_card_for_name(&user, Some(code));
        }
    }

    let now = time.elapsed_secs_f64();
    if contacts.want_nearby
        && contacts
            .nearby_asked_at
            .is_none_or(|t| now - t >= NEARBY_EVERY_S)
    {
        contacts.nearby_asked_at = Some(now);
        let request_id = contacts.request_id();
        contacts
            .outbox
            .push(NetMessage::GetNearbyPeople { request_id });
    }
    contacts.want_nearby = false;

    // A private chat's World view: where the people in it are, asked again
    // every few seconds while it is open and forgotten the moment it closes.
    let wanted = chat
        .world
        .as_deref()
        .filter(|k| ChatState::is_private(k))
        .map(|k| private.people_in(k))
        .unwrap_or_default();
    if wanted.is_empty() {
        contacts.whereabouts.clear();
        contacts.where_asked_at = None;
        contacts.where_asked_for.clear();
    } else if wanted != contacts.where_asked_for
        || contacts
            .where_asked_at
            .is_none_or(|t| now - t >= WHERE_EVERY_S)
    {
        if wanted != contacts.where_asked_for {
            contacts.whereabouts.clear();
        }
        contacts.where_asked_at = Some(now);
        contacts.where_asked_for = wanted.clone();
        let request_id = contacts.request_id();
        contacts.outbox.push(NetMessage::WherePeople {
            request_id,
            usernames: wanted,
        });
    }

    for msg in std::mem::take(&mut contacts.outbox) {
        online.send("contacts", msg);
    }
}

/// After a new state, the relation on an open card follows it — accepting a
/// request from the card's own person, say.
fn refresh_card_relation(contacts: &mut Contacts) {
    let Some(name) = contacts
        .card
        .as_ref()
        .and_then(|c| c.person.as_ref())
        .map(|p| p.username.clone())
    else {
        return;
    };
    let rel = if contacts.is_contact(&name) {
        Some(Relation::Contact)
    } else if contacts.incoming_from(&name).is_some() {
        Some(Relation::RequestReceived)
    } else if contacts.sent_to(&name) {
        Some(Relation::RequestSent)
    } else {
        None
    };
    if let (Some(rel), Some(p)) = (rel, contacts.card.as_mut().and_then(|c| c.person.as_mut())) {
        p.relation = rel;
        p.may_request = false;
    }
}

/// A tap on another person's avatar opens their card.
///
/// After `picking::prim_picking` and before `building_select::pick_building`:
/// a prim in front of the avatar keeps the tap, and a tap the avatar took is
/// not also a tap on the building behind it.
#[allow(clippy::too_many_arguments)]
pub fn tap_avatar(
    egui: Res<EguiManager>,
    mouse: Res<ButtonInput<MouseButton>>,
    windows: Query<&Window>,
    touch: Res<crate::systems::touch::TouchGestures>,
    camera: Query<(&Camera, &GridCell, &Transform), With<FreeCamera>>,
    remotes: Query<(&RemoteAvatar, &Transform), Without<FreeCamera>>,
    online: Option<Res<OnlineSession>>,
    mut prim_click: ResMut<crate::systems::picking::PrimClick>,
    mut contacts: ResMut<Contacts>,
) {
    if online.is_none() || !mouse.just_pressed(MouseButton::Left) || prim_click.took() {
        return;
    }
    let Some(cursor) = windows.iter().next().and_then(Window::cursor_position) else {
        return;
    };
    if egui.owns_click(touch.touch_click(), Some(cursor)) {
        return;
    }
    let Ok((cam, cell, tf)) = camera.single() else {
        return;
    };
    let edge = crate::grid::WORLD_CELL_EDGE as f64;
    let cam_world = DVec3::new(
        cell.x as f64 * edge + f64::from(tf.translation.x),
        cell.y as f64 * edge + f64::from(tf.translation.y),
        cell.z as f64 * edge + f64::from(tf.translation.z),
    );
    let view = GlobalTransform::from(Transform::from_rotation(tf.rotation));
    let scale = egui.ui_scale();
    // Nearest avatar to the tap, measured at mid-body so a tap on the head or
    // the feet both count.
    let hit = remotes
        .iter()
        .filter_map(|(r, rtf)| {
            let at = rtf.translation.as_dvec3() + DVec3::Y * 0.9 - cam_world;
            let screen = cam.world_to_viewport(&view, at.as_vec3()).ok()?;
            let d = screen.distance(cursor) / scale;
            (d <= TAP_RADIUS_PTS).then_some((r.sim_id, d))
        })
        .min_by(|a, b| a.1.total_cmp(&b.1));
    if let Some((id, _)) = hit {
        prim_click.claim();
        contacts.compose = None;
        contacts.open_card_for_avatar(id);
    }
}

// ─── Over the map: the person card and the first-message sheet ─────────────

/// The person card and the first-message sheet. In the panel chain, next to
/// the other chat overlays, because the sheet owns a text field the
/// on-screen keyboard has to find.
#[allow(clippy::too_many_arguments)]
pub fn render_person_card(
    egui: ResMut<EguiManager>,
    mut contacts: ResMut<Contacts>,
    mut chat: ResMut<ChatState>,
    mut view: ResMut<ChatsView>,
    mut right_panel: ResMut<RightPanel>,
    online: Option<Res<OnlineSession>>,
    driver: Res<crate::systems::driver::DriverState>,
    keyboard: Res<crate::systems::onscreen_keyboard::OnScreenKeyboard>,
) {
    if (online.is_none() && !contacts.demo) || driver.active {
        return;
    }
    let compact = egui.compact();
    let width = CARD_WIDTH.min(egui.viewport_pts().x - 2.0 * theme::OSD_MARGIN);
    let lift = crate::systems::chat_overlay::keyboard_reserve(&egui, keyboard.is_open());
    let ctx = egui.ctx().clone();
    let now = now_ms();

    if contacts.compose.is_some() {
        let mut send = false;
        let mut cancel = false;
        let error = contacts.error.clone();
        let Some(compose) = contacts.compose.as_mut() else {
            return;
        };
        egui::Area::new(egui::Id::new("contacts_compose"))
            .order(egui::Order::Foreground)
            .anchor(
                egui::Align2::CENTER_BOTTOM,
                egui::vec2(0.0, -(crate::systems::dock::reserved_bottom() + 8.0 + lift)),
            )
            .show(&ctx, |ui| {
                egui::Frame::NONE
                    .fill(theme::SURFACE)
                    .stroke(egui::Stroke::new(1.0, theme::OVERLAY_LINE))
                    .corner_radius(egui::CornerRadius::same(22))
                    .inner_margin(egui::Margin::same(14))
                    .show(ui, |ui| {
                        ui.set_width(width);
                        ui.horizontal(|ui| {
                            avatar(ui, &compose.username, ChatKind::Private, 34.0);
                            ui.vertical(|ui| {
                                ui.label(
                                    egui::RichText::new(format!(
                                        "First message to {}",
                                        compose.username
                                    ))
                                    .size(17.0)
                                    .color(theme::CREAM),
                                );
                                ui.label(
                                    egui::RichText::new("Goes to their requests")
                                        .size(12.5)
                                        .color(theme::TEXT_DIM),
                                );
                            });
                        });
                        ui.add_space(8.0);
                        let field = crate::systems::text_touch::field(
                            ui,
                            egui::TextEdit::multiline(&mut compose.text)
                                .id(egui::Id::new("contacts_compose_text"))
                                .desired_rows(3)
                                .desired_width(f32::INFINITY)
                                .char_limit(MAX_HELLO_CHARS)
                                .hint_text("Say hi"),
                        );
                        if !compose.claim_focus {
                            compose.claim_focus = true;
                            field.request_focus();
                        }
                        ui.add_space(6.0);
                        ui.horizontal_wrapped(|ui| {
                            for quick in ["👋 Hi!", "Nice build", "Want to explore together?"] {
                                if theme::chip(ui, quick, false).clicked() {
                                    compose.text = quick.to_string();
                                }
                            }
                        });
                        ui.add_space(6.0);
                        raised_frame().show(ui, |ui| {
                            ui.horizontal_wrapped(|ui| {
                                ui.label(
                                    egui::RichText::new(icons::LOCK)
                                        .color(theme::TEXT_DIM)
                                        .size(14.0),
                                );
                                ui.label(
                                    egui::RichText::new(format!(
                                        "Encrypted, text only. {} sees this, your name and \
                                         where you met. No points, pictures or locations \
                                         until they accept.",
                                        compose.username
                                    ))
                                    .size(12.5)
                                    .color(theme::TEXT_DIM),
                                );
                            });
                        });
                        if let Some(e) = &error {
                            ui.add_space(6.0);
                            if theme::inline_error(ui, e) {
                                cancel = true;
                            }
                        }
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            if theme::action_button(ui, "Cancel").clicked() {
                                cancel = true;
                            }
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    let ready = !compose.text.trim().is_empty();
                                    if theme::action_enabled_because(
                                        ui,
                                        ready,
                                        format!("{} Send request", icons::SEND),
                                        "Write something first",
                                    )
                                    .clicked()
                                        && ready
                                    {
                                        send = true;
                                    }
                                },
                            );
                        });
                    });
            });
        if send {
            if let Some(c) = contacts.compose.take() {
                contacts.say_hi(&c.username, &c.text, c.link_code);
                contacts.card = None;
            }
        } else if cancel {
            contacts.compose = None;
            contacts.error = None;
        }
        return;
    }

    let Some(card) = contacts.card.clone() else {
        return;
    };
    // On a phone an open panel is the whole screen; the card waits behind it.
    if compact && !right_panel.is(RightPanel::None) {
        return;
    }
    let mut close = false;
    let mut hi: Option<(String, Option<String>)> = None;
    let mut add: Option<(String, Option<String>)> = None;
    let mut open_chat: Option<String> = None;
    let mut open_request: Option<String> = None;
    let mut block: Option<String> = None;
    let mut report: Option<String> = None;
    let mut toggle_menu = false;
    let error = contacts.error.clone();
    egui::Area::new(egui::Id::new("contacts_person_card"))
        .order(egui::Order::Foreground)
        .anchor(
            egui::Align2::CENTER_BOTTOM,
            egui::vec2(0.0, -(crate::systems::dock::reserved_bottom() + 8.0)),
        )
        .show(&ctx, |ui| {
            glass_frame().show(ui, |ui| {
                ui.set_width(width);
                let Some(p) = card.person.as_ref() else {
                    ui.horizontal(|ui| {
                        if card.looking {
                            ui.spinner();
                            ui.label(egui::RichText::new("Looking…").color(theme::TEXT_DIM));
                        } else {
                            ui.label(
                                egui::RichText::new(
                                    error.as_deref().unwrap_or("Nobody to show here."),
                                )
                                .color(theme::TEXT_DIM),
                            );
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            close = theme::close_button(ui).clicked();
                        });
                    });
                    return;
                };
                ui.horizontal(|ui| {
                    avatar(ui, &p.username, ChatKind::Private, 48.0);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        close = theme::close_button(ui).clicked();
                        if theme::kebab_button(ui).clicked() {
                            toggle_menu = true;
                        }
                        ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                            ui.label(
                                egui::RichText::new(&p.username)
                                    .size(18.0)
                                    .color(theme::CREAM),
                            );
                            ui.label(
                                egui::RichText::new(match p.relation {
                                    Relation::Contact => "Contact",
                                    Relation::RequestSent => "Request sent · waiting",
                                    Relation::RequestReceived => "Sent you a request",
                                    Relation::Me => "You",
                                    Relation::Nobody => "Not a contact",
                                })
                                .size(12.5)
                                .color(theme::TEXT_DIM),
                            );
                        });
                    });
                });
                ui.add_space(6.0);
                ui.horizontal_wrapped(|ui| {
                    if let Some(d) = p.distance_m {
                        fact(ui, &format!("{} {:.0} m from you", icons::PIN, d), false);
                    } else if let Some(m) = &p.met {
                        fact(ui, &format!("{} {}", icons::PIN, met_text(m, now)), false);
                    }
                    fact(ui, &joined(p.joined_ms, p.new_account, now), p.new_account);
                    fact(ui, &in_common_text(&p.in_common), false);
                });
                if card.menu {
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        if theme::action_button(ui, format!("{} Block", icons::CLEAR)).clicked() {
                            block = Some(p.username.clone());
                        }
                        if theme::action_button(ui, format!("{} Report…", icons::FLAG)).clicked()
                        {
                            report = Some(p.username.clone());
                        }
                    });
                }
                if let Some(e) = &error {
                    ui.add_space(4.0);
                    if theme::inline_error(ui, e) {
                        close = true;
                    }
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let half = (ui.available_width() - 8.0) / 2.0;
                    match p.relation {
                        Relation::Nobody if p.may_request || card.link_code.is_some() => {
                            if theme::primary_button_sized(
                                ui,
                                &format!("{} Say hi", icons::CHAT),
                                half,
                            )
                            .clicked()
                            {
                                hi = Some((p.username.clone(), card.link_code.clone()));
                            }
                            if theme::primary_button_sized(
                                ui,
                                &format!("{} Add contact", icons::ADD),
                                half,
                            )
                            .clicked()
                            {
                                add = Some((p.username.clone(), card.link_code.clone()));
                            }
                        }
                        Relation::Nobody => {
                            theme::note(ui, "Not taking requests from you right now.");
                        }
                        Relation::Contact => {
                            if theme::primary_button_sized(
                                ui,
                                &format!("{} Message", icons::CHAT),
                                ui.available_width(),
                            )
                            .clicked()
                            {
                                open_chat = Some(p.username.clone());
                            }
                        }
                        Relation::RequestReceived => {
                            if theme::primary_button_sized(
                                ui,
                                "See their request",
                                ui.available_width(),
                            )
                            .clicked()
                            {
                                open_request = Some(p.username.clone());
                            }
                        }
                        Relation::RequestSent => {
                            theme::note(ui, "They'll see your request in their tray.");
                        }
                        Relation::Me => {}
                    }
                });
            });
        });

    if toggle_menu {
        if let Some(c) = contacts.card.as_mut() {
            c.menu = !c.menu;
        }
    }
    if close {
        contacts.card = None;
        contacts.error = None;
    }
    if let Some((name, code)) = hi {
        contacts.error = None;
        contacts.compose = Some(Compose {
            username: name,
            link_code: code,
            ..Compose::default()
        });
    }
    if let Some((name, code)) = add {
        contacts.request(&name, code);
    }
    if let Some(name) = open_chat {
        contacts.card = None;
        open_direct(&mut chat, &mut view, &mut right_panel, &name);
    }
    if let Some(name) = open_request {
        contacts.card = None;
        view.open = None;
        view.sub = Some(Sub::Request(name));
        *right_panel = RightPanel::Chat;
    }
    if let Some(name) = block {
        contacts.card = None;
        if let Some(online) = online.as_deref() {
            online.send(
                "set blocked",
                NetMessage::SetBlocked {
                    request_id: 0,
                    username: name,
                    blocked: true,
                },
            );
        }
    }
    if let Some(name) = report {
        contacts.card = None;
        chat.report = Some(ReportDraft {
            username: name,
            message: None,
            reason: cartopolis_core::ReportReason::Harassment,
            note: String::new(),
            asset: None,
        });
    }
}

/// One of the small grey facts on a card (`.facts div`); `new` is the amber
/// "new account" one.
fn fact(ui: &mut egui::Ui, text: &str, new: bool) {
    let (fill, ink) = if new {
        (
            egui::Color32::from_rgba_unmultiplied(0xFF, 0xD2, 0x7A, 36),
            egui::Color32::from_rgb(0xFF, 0xD2, 0x7A),
        )
    } else {
        (theme::SURFACE_RAISED, theme::TEXT)
    };
    egui::Frame::NONE
        .fill(fill)
        .corner_radius(egui::CornerRadius::same(9))
        .inner_margin(egui::Margin::symmetric(9, 3))
        .show(ui, |ui| {
            // One line: in a wrapped row a chip that wraps its own text
            // collapses into a column one letter wide.
            ui.add(
                egui::Label::new(egui::RichText::new(text).size(12.0).color(ink))
                    .wrap_mode(egui::TextWrapMode::Extend),
            );
        });
}

fn in_common_text(names: &[String]) -> String {
    match names {
        [] => "No contacts in common".into(),
        [one] => format!("1 contact in common: {one}"),
        [a, rest @ ..] => format!("{} contacts in common: {a}, …", rest.len() + 1),
    }
}

/// Open the direct chat with `name` in the Chats panel, starting it if this
/// device has none.
fn open_direct(
    chat: &mut ChatState,
    view: &mut ChatsView,
    right_panel: &mut RightPanel,
    name: &str,
) {
    view.sub = None;
    *right_panel = RightPanel::Chat;
    match chat.direct.get(&name.to_lowercase()).cloned() {
        Some(key) => {
            view.open_chat(&key);
            chat.focus_channel(&key);
        }
        None => chat.private_start.push(name.to_string()),
    }
}

// ─── In the Chats panel ─────────────────────────────────────────────────────

/// The requests row at the top of the chat list (`.reqrow`). Returns whether it
/// was tapped.
pub fn requests_row(ui: &mut egui::Ui, contacts: &Contacts) -> bool {
    let n = contacts.incoming.len();
    if n == 0 {
        return false;
    }
    let amber = egui::Color32::from_rgb(0xFF, 0xD2, 0x7A);
    let resp = egui::Frame::NONE
        .fill(egui::Color32::from_rgba_unmultiplied(0xFF, 0xD2, 0x7A, 36))
        .stroke(egui::Stroke::new(1.0, amber.gamma_multiply(0.35)))
        .corner_radius(egui::CornerRadius::same(16))
        .inner_margin(egui::Margin::symmetric(12, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                for r in contacts.incoming.iter().take(2) {
                    avatar(ui, &r.username, ChatKind::Private, 34.0);
                }
                ui.vertical(|ui| {
                    ui.label(
                        egui::RichText::new(if n == 1 {
                            "1 message request".to_string()
                        } else {
                            format!("{n} message requests")
                        })
                        .size(15.0)
                        .color(theme::CREAM),
                    );
                    let first = &contacts.incoming[0];
                    let more = if n > 1 {
                        format!(" · {} more", n - 1)
                    } else {
                        String::new()
                    };
                    ui.label(
                        egui::RichText::new(format!("{}{more}", first.username))
                            .size(12.5)
                            .color(theme::TEXT_DIM),
                    );
                });
            });
        })
        .response
        .interact(egui::Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    ui.add_space(6.0);
    resp.clicked()
}

/// What a contacts screen decided, acted on after drawing.
#[derive(Default)]
struct Act {
    back: bool,
    to: Option<Sub>,
    open_chat: Option<String>,
    open_channel: Option<String>,
    accept: Option<String>,
    decline: Option<String>,
    block: Option<String>,
    report: Option<(String, Option<String>)>,
    withdraw: Option<String>,
    remove: Option<String>,
    card: Option<String>,
    hi: Option<String>,
    add: Option<String>,
    settings: bool,
    reset_link: bool,
    search: bool,
    share_link: bool,
}

/// Draw one contacts screen in the Chats panel, and act on it.
#[allow(clippy::too_many_arguments)]
pub fn screen(
    ui: &mut egui::Ui,
    sub: &Sub,
    contacts: &mut Contacts,
    chat: &mut ChatState,
    view: &mut ChatsView,
    online: Option<&OnlineSession>,
    me: &str,
) {
    let mut act = Act::default();
    let now = now_ms();
    match sub {
        Sub::Requests => requests_screen(ui, contacts, &mut act, now),
        Sub::Request(name) => request_screen(ui, contacts, chat, name, &mut act, now),
        Sub::Contacts => contacts_screen(ui, contacts, &mut act, now),
        Sub::Add => add_screen(ui, contacts, me, &mut act),
        Sub::Privacy => privacy_screen(ui, contacts, &mut act),
    }

    if act.back {
        view.sub = match sub {
            Sub::Request(_) if contacts.incoming.len() > 1 => Some(Sub::Requests),
            Sub::Add | Sub::Privacy => Some(Sub::Contacts),
            _ => None,
        };
        contacts.error = None;
    }
    if let Some(to) = act.to {
        contacts.error = None;
        view.sub = Some(to);
    }
    if let Some(name) = act.accept {
        contacts.answer(&name, true);
        // The chat moves to your chats: open it, if it is here.
        view.sub = None;
        if let Some(key) = chat.direct.get(&name.to_lowercase()).cloned() {
            view.open_chat(&key);
            chat.focus_channel(&key);
        }
    }
    if let Some(name) = act.decline {
        contacts.answer(&name, false);
        if let Some(key) = chat.direct.get(&name.to_lowercase()).cloned() {
            chat.private_forget.push(key);
        }
        view.sub = (!contacts.incoming.is_empty()).then_some(Sub::Requests);
    }
    if let Some(name) = act.block {
        contacts.answer(&name, false);
        if let Some(key) = chat.direct.get(&name.to_lowercase()).cloned() {
            chat.private_forget.push(key);
        }
        if let Some(online) = online {
            online.send(
                "set blocked",
                NetMessage::SetBlocked {
                    request_id: 0,
                    username: name,
                    blocked: true,
                },
            );
        }
        view.sub = (!contacts.incoming.is_empty()).then_some(Sub::Requests);
    }
    if let Some((name, line)) = act.report {
        chat.report = Some(ReportDraft {
            username: name,
            message: line,
            reason: cartopolis_core::ReportReason::Harassment,
            note: String::new(),
            asset: None,
        });
    }
    if let Some(name) = act.withdraw.or(act.remove) {
        contacts.remove(&name);
    }
    if let Some(name) = act.open_chat {
        let mut panel = RightPanel::Chat;
        open_direct(chat, view, &mut panel, &name);
    }
    if let Some(key) = act.open_channel {
        view.sub = None;
        view.open_chat(&key);
        chat.focus_channel(&key);
    }
    if let Some(name) = act.card {
        contacts.open_card_for_name(&name, None);
    }
    if let Some(name) = act.hi {
        contacts.error = None;
        contacts.compose = Some(Compose {
            username: name,
            ..Compose::default()
        });
    }
    if let Some(name) = act.add {
        contacts.request(&name, None);
    }
    if act.settings || act.reset_link {
        contacts.push_settings(act.reset_link);
    }
    if act.search {
        contacts.search();
    }
    if act.share_link {
        let link = contacts.my_link(me);
        share(&format!("Add me on Cartopolis: {link}"));
        chat.notice = Some(ChatNotice {
            text: if cfg!(target_os = "android") {
                "Choose where to send your link.".into()
            } else {
                "Your contact link is on the clipboard.".into()
            },
            error: false,
        });
    }
}

/// Hand a text to the platform's share sheet where there is one, else the
/// clipboard.
fn share(text: &str) {
    #[cfg(target_os = "android")]
    {
        if crate::platform::android::share_text("Cartopolis", text).is_ok() {
            return;
        }
    }
    crate::utils::clipboard::set_text(text);
}

/// A screen's title row: a back chevron, a title, and room on the right.
fn title_row(
    ui: &mut egui::Ui,
    title: &str,
    act: &mut Act,
    right: impl FnOnce(&mut egui::Ui, &mut Act),
) {
    ui.horizontal(|ui| {
        if ui
            .add(
                egui::Label::new(
                    egui::RichText::new(icons::CHEVRON_LEFT)
                        .size(26.0)
                        .color(theme::TEXT),
                )
                .sense(egui::Sense::click()),
            )
            .on_hover_text("Back")
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .clicked()
        {
            act.back = true;
        }
        ui.label(egui::RichText::new(title).size(22.0).color(theme::CREAM));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            right(ui, act);
        });
    });
    ui.add_space(8.0);
}

fn section_label(ui: &mut egui::Ui, text: &str, count: Option<usize>) {
    ui.add_space(10.0);
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(text.to_uppercase())
                .size(12.0)
                .color(theme::TEXT_DIM),
        );
        if let Some(n) = count {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    egui::RichText::new(n.to_string())
                        .size(12.0)
                        .color(theme::TEXT_DIM),
                );
            });
        }
    });
    ui.add_space(2.0);
}

/// One person row (`.crow`): avatar, name, a line under it, and whatever the
/// caller puts on the right. Returns the row's response, for a tap and a menu.
fn person_row(
    ui: &mut egui::Ui,
    name: &str,
    sub: &str,
    sub_color: egui::Color32,
    online: bool,
    right: impl FnOnce(&mut egui::Ui),
) -> egui::Response {
    let inner = ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(egui::Vec2::splat(44.0), egui::Sense::hover());
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect));
        avatar(&mut child, name, ChatKind::Private, 44.0);
        if online {
            let c = rect.right_bottom() - egui::vec2(6.0, 6.0);
            ui.painter().circle_filled(c, 7.0, theme::INK);
            ui.painter()
                .circle_filled(c, 5.0, egui::Color32::from_rgb(0x5F, 0xD3, 0x8D));
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            right(ui);
            ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                ui.label(egui::RichText::new(name).size(15.0).color(theme::CREAM));
                ui.label(egui::RichText::new(sub).size(12.5).color(sub_color));
            });
        });
    });
    ui.add_space(4.0);
    inner.response.interact(egui::Sense::click())
}

fn requests_screen(ui: &mut egui::Ui, contacts: &Contacts, act: &mut Act, now: u64) {
    title_row(ui, "Message requests", act, |_, _| {});
    theme::note(
        ui,
        "People who aren't contacts yet. They can't tell whether you've read a request.",
    );
    if contacts.incoming.is_empty() {
        theme::empty_state(
            ui,
            "No requests",
            "When somebody new says hi, it waits here.",
        );
        return;
    }
    let (new, rest): (Vec<&ContactRequestDto>, Vec<&ContactRequestDto>) = contacts
        .incoming
        .iter()
        .partition(|r| contacts.settings.separate_new_accounts && r.new_account);
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for (title, list) in [("Requests", &rest), ("From new accounts", &new)] {
                if list.is_empty() {
                    continue;
                }
                section_label(ui, title, Some(list.len()));
                for r in list.iter() {
                    let line = match &r.met {
                        Some(m) => met_text(m, now),
                        None => format!("Sent {}", ago(r.sent_at_ms, now)),
                    };
                    if person_row(ui, &r.username, &line, theme::TEXT_DIM, false, |_| {}).clicked()
                    {
                        act.to = Some(Sub::Request(r.username.clone()));
                    }
                }
            }
        });
}

fn request_screen(
    ui: &mut egui::Ui,
    contacts: &Contacts,
    chat: &ChatState,
    name: &str,
    act: &mut Act,
    now: u64,
) {
    title_row(ui, "Message request", act, |_, _| {});
    let Some(r) = contacts.incoming_from(name) else {
        theme::empty_state(ui, "No request", "It was answered, or withdrawn.");
        return;
    };
    ui.vertical_centered(|ui| {
        avatar(ui, &r.username, ChatKind::Private, 72.0);
        ui.label(
            egui::RichText::new(&r.username)
                .size(20.0)
                .color(theme::CREAM),
        );
    });
    ui.add_space(6.0);
    ui.horizontal_wrapped(|ui| {
        if let Some(m) = &r.met {
            fact(ui, &format!("{} {}", icons::PIN, met_text(m, now)), false);
        }
        fact(ui, &joined(r.joined_ms, r.new_account, now), r.new_account);
        fact(ui, &in_common_text(&r.in_common), false);
    });
    ui.add_space(8.0);
    ui.separator();
    // What they wrote, from the private chat that goes with the request. A
    // point is held back until you accept: from a stranger, a place and a
    // camera pose is exactly the kind of thing not to open unasked.
    let key = chat.direct.get(&r.username.to_lowercase());
    let lines: Vec<&crate::systems::chat::ChatLine> = key
        .and_then(|k| chat.channels.iter().find(|c| &c.key == k))
        .map(|c| c.lines.iter().collect())
        .unwrap_or_default();
    let bottom_h = 112.0;
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .max_height((ui.available_height() - bottom_h).max(60.0))
        .stick_to_bottom(true)
        .show(ui, |ui| {
            if lines.is_empty() {
                theme::note(ui, "No message: they only asked to be contacts.");
            }
            for l in &lines {
                if l.point.is_some() {
                    raised_frame().show(ui, |ui| {
                        ui.label(
                            egui::RichText::new(format!(
                                "{} A point is held back until you accept",
                                icons::PIN
                            ))
                            .size(12.5)
                            .color(theme::TEXT_DIM),
                        );
                    });
                } else {
                    let resp = egui::Frame::NONE
                        .fill(theme::SURFACE_RAISED)
                        .corner_radius(egui::CornerRadius {
                            nw: 18,
                            ne: 18,
                            sw: 6,
                            se: 18,
                        })
                        .inner_margin(egui::Margin::symmetric(12, 8))
                        .show(ui, |ui| {
                            ui.set_max_width(ui.available_width() * 0.8);
                            ui.label(egui::RichText::new(&l.body).size(14.5).color(theme::TEXT));
                        })
                        .response;
                    let _ = resp;
                }
                ui.add_space(4.0);
            }
        });
    ui.add_space(6.0);
    ui.vertical_centered(|ui| {
        theme::note(
            ui,
            &format!(
                "{} won't know you've seen this unless you accept.",
                r.username
            ),
        );
    });
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        let half = (ui.available_width() - 8.0) / 2.0;
        if theme::primary_button_sized(ui, &format!("{} Decline", icons::CLEAR), half).clicked() {
            act.decline = Some(r.username.clone());
        }
        if theme::primary_button_sized(ui, &format!("{} Accept", icons::CHECK), half).clicked() {
            act.accept = Some(r.username.clone());
        }
    });
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.add_space((ui.available_width() - 180.0).max(0.0) / 2.0);
        if theme::action_button(ui, "Block").clicked() {
            act.block = Some(r.username.clone());
        }
        if theme::action_button(ui, format!("{} Report", icons::FLAG)).clicked() {
            let line = lines.last().map(|l| l.body.clone());
            act.report = Some((r.username.clone(), line));
        }
    });
}

fn contacts_screen(ui: &mut egui::Ui, contacts: &mut Contacts, act: &mut Act, now: u64) {
    contacts.want_nearby = true;
    title_row(ui, "Contacts", act, |ui, act| {
        if theme::round_button(ui, icons::SETTINGS, "Who can reach you", false).clicked() {
            act.to = Some(Sub::Privacy);
        }
        if theme::round_button(ui, icons::ADD, "Add a contact", false).clicked() {
            act.to = Some(Sub::Add);
        }
    });
    if let Some(e) = &contacts.error {
        if theme::inline_error(ui, e) {
            contacts.error = None;
        }
    }
    let empty = contacts.contacts.is_empty() && contacts.outgoing.is_empty();
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            if empty {
                theme::empty_state(
                    ui,
                    "No contacts yet",
                    "Say hi to someone near you, or send your link to a friend who's already here.",
                );
            }
            if !contacts.incoming.is_empty() && requests_row(ui, contacts) {
                act.to = Some(Sub::Requests);
            }
            let online: Vec<&ContactDto> = contacts
                .contacts
                .iter()
                .filter(|c| c.online == Some(true))
                .collect();
            if !online.is_empty() {
                section_label(ui, "Online now", Some(online.len()));
                for c in online {
                    if person_row(ui, &c.username, "online", theme::TEXT_DIM, true, |ui| {
                        ui.label(egui::RichText::new(icons::CHAT).color(theme::TEXT_DIM));
                    })
                    .clicked()
                    {
                        act.open_chat = Some(c.username.clone());
                    }
                }
            }
            if !contacts.contacts.is_empty() {
                section_label(ui, "All contacts", Some(contacts.contacts.len()));
                for c in &contacts.contacts {
                    let line = match (&c.met, c.online) {
                        (_, Some(true)) => "online".to_string(),
                        (Some(m), _) if now.saturating_sub(c.since_ms) < 86_400_000 => {
                            format!("added today · {}", met_text(m, now).to_lowercase())
                        }
                        _ => format!("contact since {}", ago(c.since_ms, now)),
                    };
                    let resp = person_row(
                        ui,
                        &c.username,
                        &line,
                        theme::TEXT_DIM,
                        c.online == Some(true),
                        |ui| {
                            ui.label(egui::RichText::new(icons::CHAT).color(theme::TEXT_DIM));
                        },
                    );
                    if resp.clicked() {
                        act.open_chat = Some(c.username.clone());
                    }
                    resp.context_menu(|ui| {
                        if ui.button("Message").clicked() {
                            act.open_chat = Some(c.username.clone());
                            ui.close();
                        }
                        if ui.button("Remove contact").clicked() {
                            act.remove = Some(c.username.clone());
                            ui.close();
                        }
                    });
                }
            }
            if !contacts.outgoing.is_empty() {
                section_label(ui, "Sent requests", Some(contacts.outgoing.len()));
                for r in &contacts.outgoing {
                    let mut withdraw = false;
                    person_row(
                        ui,
                        &r.username,
                        &format!("waiting · sent {}", ago(r.sent_at_ms, now)),
                        theme::LINE,
                        false,
                        |ui| {
                            withdraw = theme::action_button(ui, "Withdraw").clicked();
                        },
                    );
                    if withdraw {
                        act.withdraw = Some(r.username.clone());
                    }
                }
            }
            let near: Vec<&NearbyPersonDto> = contacts
                .nearby
                .iter()
                .filter(|p| p.relation == Relation::Nobody && p.may_request)
                .collect();
            if !near.is_empty() {
                section_label(ui, "Near you now", None);
                for p in near {
                    let mut hi = false;
                    let resp = person_row(
                        ui,
                        &p.username,
                        &format!("{:.0} m", p.distance_m),
                        theme::TEXT_DIM,
                        false,
                        |ui| {
                            hi = theme::action_button(ui, "Say hi").clicked();
                        },
                    );
                    if hi {
                        act.hi = Some(p.username.clone());
                    } else if resp.clicked() {
                        act.card = Some(p.username.clone());
                    }
                }
            }
            if empty {
                section_label(ui, "Places to meet people", None);
                theme::note(
                    ui,
                    "Here: whoever is talking around your avatar. Global: everyone on this \
                     server. Both are public.",
                );
                ui.add_space(8.0);
                if theme::primary_button_sized(
                    ui,
                    &format!("{} Share my link", icons::OPEN_EXTERNAL),
                    ui.available_width(),
                )
                .clicked()
                {
                    act.share_link = true;
                }
            }
            ui.add_space(40.0);
        });
}

fn add_screen(ui: &mut egui::Ui, contacts: &mut Contacts, me: &str, act: &mut Act) {
    title_row(ui, "Add a contact", act, |_, _| {});
    let link = contacts.my_link(me);
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.vertical_centered(|ui| {
                paint_qr(ui, &link, 190.0);
                ui.add_space(6.0);
                ui.label(egui::RichText::new(me).size(20.0).color(theme::CREAM));
            });
            ui.add_space(8.0);
            raised_frame().show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.add(
                        egui::Label::new(egui::RichText::new(&link).size(13.0).color(theme::TEXT))
                            .truncate(),
                    );
                });
                ui.horizontal(|ui| {
                    if theme::action_button(ui, format!("{} Copy", icons::COPY)).clicked() {
                        crate::utils::clipboard::set_text(&link);
                    }
                    if theme::action_button(ui, format!("{} Share", icons::OPEN_EXTERNAL)).clicked()
                    {
                        act.share_link = true;
                    }
                    if theme::confirm_button(
                        ui,
                        egui::Id::new("contacts_reset_link"),
                        &format!("{} Reset link", icons::REFRESH),
                        "Old links stop working",
                    ) {
                        act.reset_link = true;
                    }
                });
            });
            theme::note(
                ui,
                "Somebody who opens your link or scans this can send you a request, whatever \
                 your privacy setting. Reset link stops old copies working.",
            );
            ui.add_space(12.0);
            ui.label(egui::RichText::new("Or find a username").color(theme::CREAM));
            ui.horizontal(|ui| {
                let field = crate::systems::text_touch::field(
                    ui,
                    egui::TextEdit::singleline(&mut contacts.search)
                        .id(egui::Id::new("contacts_search"))
                        .desired_width(ui.available_width() * 0.65)
                        .hint_text("Exact username, or a contact link"),
                );
                let entered = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                let ready = !contacts.search.trim().is_empty();
                if (theme::action_enabled_because(ui, ready, "Find", "Type a username first")
                    .clicked()
                    || entered)
                    && ready
                {
                    act.search = true;
                }
            });
            match &contacts.found {
                None if contacts.search_request.is_some() => {
                    ui.spinner();
                }
                None => {}
                Some(None) => theme::note(ui, "Nobody by that name."),
                Some(Some(p)) => {
                    let line = match p.relation {
                        Relation::Contact => "already a contact",
                        Relation::RequestSent => "request sent · waiting",
                        Relation::RequestReceived => "sent you a request",
                        Relation::Me => "that's you",
                        Relation::Nobody if p.may_request => "exact match",
                        Relation::Nobody => "only takes requests from people they've met",
                    };
                    let p = p.clone();
                    let mut request = false;
                    person_row(ui, &p.username, line, theme::TEXT_DIM, false, |ui| {
                        if p.relation == Relation::Nobody && p.may_request {
                            request = theme::action_button(ui, "Request").clicked();
                        }
                    });
                    if request {
                        act.add = Some(p.username.clone());
                    }
                }
            }
            if let Some(e) = contacts.error.clone() {
                if theme::inline_error(ui, &e) {
                    contacts.error = None;
                }
            }
            theme::note(
                ui,
                "Only an exact username finds someone: there's no list of everybody to browse.",
            );
        });
}

fn privacy_screen(ui: &mut egui::Ui, contacts: &mut Contacts, act: &mut Act) {
    title_row(ui, "Privacy", act, |_, _| {});
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.label(egui::RichText::new("Who can send you a request").color(theme::CREAM));
            theme::note(
                ui,
                "Your contacts can always message you. Anyone with your link can always send \
                 a request.",
            );
            ui.add_space(4.0);
            let s = &mut contacts.settings;
            for (policy, title, sub) in [
                (RequestPolicy::Anyone, "Anyone", "including by username"),
                (
                    RequestPolicy::Near,
                    "People you've been near",
                    "in the world in the last hour, or who share a contact with you",
                ),
                (
                    RequestPolicy::LinkOnly,
                    "Only with my link",
                    "nobody finds you by name or in the world",
                ),
            ] {
                let on = s.policy == policy;
                let resp = egui::Frame::NONE
                    .fill(if on {
                        theme::BRAND.gamma_multiply(0.12)
                    } else {
                        theme::SURFACE
                    })
                    .stroke(egui::Stroke::new(
                        1.0,
                        if on {
                            theme::BRAND
                        } else {
                            theme::OVERLAY_LINE
                        },
                    ))
                    .corner_radius(egui::CornerRadius::same(12))
                    .inner_margin(egui::Margin::symmetric(12, 9))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            let (rect, _) = ui
                                .allocate_exact_size(egui::Vec2::splat(18.0), egui::Sense::hover());
                            let c = rect.center();
                            ui.painter().circle_stroke(
                                c,
                                8.0,
                                egui::Stroke::new(2.0, if on { theme::BRAND } else { theme::LINE }),
                            );
                            if on {
                                ui.painter().circle_filled(c, 4.5, theme::BRAND);
                            }
                            ui.vertical(|ui| {
                                ui.label(egui::RichText::new(title).size(14.0).color(theme::TEXT));
                                ui.label(
                                    egui::RichText::new(sub).size(12.0).color(theme::TEXT_DIM),
                                );
                            });
                        });
                    })
                    .response
                    .interact(egui::Sense::click());
                if resp.clicked() && !on {
                    s.policy = policy;
                    act.settings = true;
                }
                ui.add_space(4.0);
            }
            ui.add_space(8.0);
            ui.label(egui::RichText::new("Requests").color(theme::CREAM));
            if theme::checkbox(
                ui,
                &mut s.separate_new_accounts,
                "Requests from accounts younger than a week wait in their own section",
            )
            .changed()
            {
                act.settings = true;
            }
            if theme::checkbox(
                ui,
                &mut s.notify_requests,
                "Say so when a request arrives (off: they wait quietly in the tray)",
            )
            .changed()
            {
                act.settings = true;
            }
            ui.add_space(8.0);
            ui.label(egui::RichText::new("Contacts can see").color(theme::CREAM));
            if theme::checkbox(ui, &mut s.show_online, "That I'm online").changed() {
                act.settings = true;
            }
            ui.add_space(8.0);
            theme::note(
                ui,
                "Where your avatar is stays per chat and until a time you set, as before. \
                 Being a contact doesn't show it.",
            );
        });
}

/// Draw `text` as a QR code, `size` points square, on a white card.
fn paint_qr(ui: &mut egui::Ui, text: &str, size: f32) {
    let Ok(code) = qrcodegen::QrCode::encode_text(text, qrcodegen::QrCodeEcc::Medium) else {
        return;
    };
    let (rect, _) = ui.allocate_exact_size(egui::Vec2::splat(size), egui::Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, egui::CornerRadius::same(16), egui::Color32::WHITE);
    // Four modules of quiet zone, as the standard asks.
    let n = code.size();
    let quiet = 4;
    let module = size / (n + 2 * quiet) as f32;
    for y in 0..n {
        for x in 0..n {
            if code.get_module(x, y) {
                let min =
                    rect.min + egui::vec2((x + quiet) as f32 * module, (y + quiet) as f32 * module);
                p.rect_filled(
                    egui::Rect::from_min_size(min, egui::Vec2::splat(module + 0.3)),
                    egui::CornerRadius::ZERO,
                    theme::INK,
                );
            }
        }
    }
}

// ─── A scripted capture's stand-in ─────────────────────────────────────────

/// Fill the screens with the design's people for `--ui contacts` and friends:
/// the same switch `ChatsView::demo` is, so the screens draw with no session.
pub fn seed_demo(contacts: &mut Contacts, chat: &mut ChatState) {
    let now_ms = now_ms();
    // The request's own chat: one line and a point, held back.
    let key = "dm:je01";
    if !chat.channels.iter().any(|c| c.key == key) {
        chat.open_channel(key, &crate::systems::private_chat::private_label("jeroen"));
        chat.receive(
            key,
            7,
            "jeroen".into(),
            "Hi! It's on until 17:00, the cheese stall is at the north end".into(),
            None,
        );
        chat.receive_point(
            key,
            7,
            "jeroen".into(),
            "Vismarkt".into(),
            None,
            Some(cartopolis_core::point::Point {
                latitude: 53.2166,
                longitude: 6.5683,
                name: Some("Vismarkt".into()),
                text: None,
                view: None,
            }),
        );
    }
    chat.direct.insert("jeroen".into(), key.into());
    let met = Some(MetDto {
        at_ms: now_ms - 5 * 60_000,
        latitude: 53.2166,
        longitude: 6.5683,
    });
    contacts.demo = true;
    contacts.loaded = true;
    contacts.contacts = vec![
        ContactDto {
            username: "eva".into(),
            since_ms: now_ms - 40 * 86_400_000,
            met: None,
            online: Some(true),
        },
        ContactDto {
            username: "anouk".into(),
            since_ms: now_ms - 3_600_000,
            met,
            online: Some(false),
        },
        ContactDto {
            username: "tom".into(),
            since_ms: now_ms - 90 * 86_400_000,
            met: None,
            online: Some(true),
        },
    ];
    contacts.incoming = vec![
        ContactRequestDto {
            username: "jeroen".into(),
            sent_at_ms: now_ms - 60_000,
            met,
            new_account: false,
            joined_ms: now_ms - 400 * 86_400_000,
            in_common: vec![],
        },
        ContactRequestDto {
            username: "sanne_k".into(),
            sent_at_ms: now_ms - 600_000,
            met: None,
            new_account: true,
            joined_ms: now_ms - 3 * 86_400_000,
            in_common: vec![],
        },
    ];
    contacts.outgoing = vec![ContactRequestDto {
        username: "bas_builds".into(),
        sent_at_ms: now_ms - 120_000,
        met,
        new_account: false,
        joined_ms: now_ms - 200 * 86_400_000,
        in_common: vec!["tom".into()],
    }];
    contacts.nearby = vec![NearbyPersonDto {
        username: "marit".into(),
        distance_m: 30.0,
        relation: Relation::Nobody,
        may_request: true,
    }];
    contacts.settings = ContactSettings {
        policy: RequestPolicy::Near,
        show_online: true,
        notify_requests: false,
        separate_new_accounts: true,
        link_code: "3f9a1c7e22b0".into(),
    };
}

/// The person card as it looks for a stranger standing nearby (`--ui
/// personcard`).
pub fn seed_demo_card(contacts: &mut Contacts) {
    let now_ms = now_ms();
    contacts.card = Some(Card {
        person: Some(PersonDto {
            username: "sanne_k".into(),
            joined_ms: now_ms - 3 * 86_400_000,
            new_account: true,
            in_common: vec![],
            relation: Relation::Nobody,
            met: None,
            distance_m: Some(12.0),
            may_request: true,
        }),
        ..Card::default()
    });
}

/// Where the demo chat's Sanne is standing for `--ui chatworld`: by the café
/// her last point names, so the marker and the point pin share the frame.
pub fn seed_demo_whereabouts(contacts: &mut Contacts) {
    let at = (53.2154, 6.5671);
    contacts.whereabouts = vec![Whereabout {
        username: "Sanne".into(),
        avatar_id: 2,
        from: at,
        to: at,
        arrived: 0.0,
    }];
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_answer_glides_on_from_where_the_marker_is_and_a_jump_does_not() {
        let dto = |lat: f64, lng: f64| cartopolis_core::PersonWhereDto {
            username: "sanne".into(),
            avatar_id: 2,
            latitude: lat,
            longitude: lng,
        };
        let first = merge_whereabouts(&[], vec![dto(53.0, 6.0)], 0.0);
        assert_eq!(first[0].at(0.0), (53.0, 6.0));
        // ~110 m north: a walk, so it glides over one ask interval.
        let walk = merge_whereabouts(&first, vec![dto(53.001, 6.0)], 10.0);
        assert_eq!(walk[0].at(10.0), (53.0, 6.0));
        let (mid, _) = walk[0].at(10.0 + WHERE_EVERY_S / 2.0);
        assert!((mid - 53.0005).abs() < 1e-9, "{mid}");
        assert_eq!(walk[0].at(10.0 + WHERE_EVERY_S), (53.001, 6.0));
        // A hundred kilometres is a teleport: straight there.
        let jump = merge_whereabouts(&walk, vec![dto(54.0, 6.0)], 20.0);
        assert_eq!(jump[0].at(20.0), (54.0, 6.0));
        // Somebody the answer leaves out is gone.
        assert!(merge_whereabouts(&jump, vec![], 30.0).is_empty());
    }

    #[test]
    fn contact_requests_have_their_own_id_range() {
        let mut c = Contacts::default();
        let id = c.request("bas", None);
        assert!(is_contacts_request(id));
        assert!(!crate::systems::private_chat::is_private_request(id));
    }

    #[test]
    fn a_first_message_waits_until_its_request_is_on_record() {
        let mut c = Contacts::default();
        c.say_hi("bas", "hi there", None);
        assert_eq!(c.hellos.len(), 1);
        assert!(matches!(c.outbox[0], NetMessage::RequestContact { .. }));
        assert!(!c.sent_to("bas"));
    }

    #[test]
    fn a_new_account_says_so_and_an_old_one_its_year() {
        let now = 1_800_000_000_000; // 2027-01-15
        assert_eq!(
            joined(now - 3 * 86_400_000, true, now),
            "New · joined 3 days ago"
        );
        assert_eq!(joined(1_735_689_600_000, false, now), "Member since 2025");
    }

    #[test]
    fn ago_reads_like_a_person_says_it() {
        let now = 10_000_000_000;
        assert_eq!(ago(now - 5 * 60_000, now), "5 min ago");
        assert_eq!(ago(now - 26 * 3_600_000, now), "yesterday");
    }
}
