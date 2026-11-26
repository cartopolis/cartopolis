//! Contacts and message requests: the screens and the state behind them
//! (`docs/notes/contacts.md`, design `docs/mockups/chat-world/v5-contacts`).
//!
//! A contact is somebody you accepted. Anybody else's first message arrives as
//! a **request**, held apart from your chats until you accept it. The server
//! enforces every rule (`cartopolis_simulator::contacts`); this module only
//! keeps what it last said and asks it for changes, the way the block list
//! does. The screens, drawn by the component library (`lib_person_card`,
//! `lib_contacts`):
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

use crate::systems::ui_input::{UiInput, UiMetrics};
use crate::utils::clock::unix_ms;
use bevy::math::DVec3;
use bevy::prelude::*;
use big_space::prelude::GridCell;
use cartopolis_core::msg;
use cartopolis_core::{
    ContactDto, ContactRequestDto, ContactSettings, MetDto, NearbyPersonDto, NetMessage, PersonDto,
    Relation, RequestPolicy,
};
use std::collections::HashSet;

use crate::components::{FreeCamera, RemoteAvatar};
use crate::resources::{OnlineSession, RightPanel};
use crate::systems::chat::{ChatNotice, ChatState, ReportDraft};
use crate::systems::chats::ChatsView;

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

/// Most people one `WherePeople` is answered for: the simulator's
/// `MAX_WHERE_PEOPLE`, which drops the rest of a longer list.
const MAX_FRIENDS_ASKED: usize = 32;

/// Whom the Friends layer asks after: every contact, in a stable order so an
/// unchanged list is not mistaken for a new one and re-asked at once. Anybody
/// last said to be offline goes to the back, so a long list spends its places
/// on the people who can be on the map.
fn friends_to_ask(contacts: &[ContactDto]) -> Vec<String> {
    let mut sorted: Vec<&ContactDto> = contacts.iter().collect();
    sorted.sort_by_key(|c| (c.online == Some(false), c.username.to_lowercase()));
    let mut names: Vec<String> = sorted.into_iter().map(|c| c.username.clone()).collect();
    names.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
    names.truncate(MAX_FRIENDS_ASKED);
    names
}

/// A tap this close to an avatar's feet, in points, is a tap on the avatar.
const TAP_RADIUS_PTS: f32 = 36.0;

/// Longest first message, in characters. A knock, not a letter.
pub(crate) const MAX_HELLO_CHARS: usize = 500;

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
    /// The avatar a tap opened this card on, for the greetings it offers
    /// (protocol 49); `None` for a card opened by name.
    pub avatar_id: Option<u64>,
}

/// The first-message sheet.
#[derive(Clone, Debug, Default)]
pub struct Compose {
    pub username: String,
    pub text: String,
    pub link_code: Option<String>,
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
    /// What [`drive_person_card`] and the library's card and sheet
    /// (`lib_person_card`) pass each other.
    pub lib_view: PersonView,
}

/// A tap on the person card or the first-message sheet, done by
/// [`person_act`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PersonAction {
    /// ✕, or dismissing the card's error.
    Close,
    /// ⋯: Block and Report shown or hidden.
    ToggleMenu,
    /// Say hi: the first-message sheet.
    SayHi,
    /// Add contact: a request with no first message.
    Add,
    /// Message a contact: their chat.
    Message,
    /// See the request they sent you.
    SeeRequest,
    Block,
    Report,
    /// Offer a greeting to the avatar the card was opened on.
    Greet(cartopolis_core::Greeting),
    /// The sheet's Send request.
    Send,
    /// The sheet's Cancel, or dismissing its error.
    Cancel,
}

/// The side of the person card only [`drive_person_card`] works out — the
/// session, the driver bar, the panels, the on-screen keyboard — for the
/// library's card.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PersonView {
    /// Neither the card nor the sheet shows (no session, driving).
    pub hidden: bool,
    /// The card waits behind an open panel on a phone (the sheet does not).
    pub card_hidden: bool,
    /// Signed in: greetings can be offered.
    pub online: bool,
    /// How far the on-screen keyboard lifts the sheet, points.
    pub lift: f32,
    /// A tap the library's card handed back, done by [`drive_person_card`].
    pub action: Option<PersonAction>,
}

/// One of the small facts on a person card.
#[derive(Clone, Debug, PartialEq)]
pub struct Fact {
    /// Led by the pin icon (where or how near).
    pub pin: bool,
    pub text: String,
    /// The amber "new account" fact.
    pub new: bool,
}

/// The facts a person card shows about `p`, both editions.
#[must_use]
pub fn person_facts(p: &PersonDto, now: u64) -> Vec<Fact> {
    let mut out = Vec::new();
    if let Some(d) = p.distance_m {
        out.push(Fact {
            pin: true,
            text: format!("{d:.0} m from you"),
            new: false,
        });
    } else if let Some(m) = &p.met {
        out.push(Fact {
            pin: true,
            text: met_text(m, now),
            new: false,
        });
    }
    out.push(Fact {
        pin: false,
        text: joined(p.joined_ms, p.new_account, now),
        new: p.new_account,
    });
    out.push(Fact {
        pin: false,
        text: in_common_text(&p.in_common),
        new: false,
    });
    out
}

/// The line under a person's name: how they stand with you.
#[must_use]
pub fn relation_word(relation: Relation) -> &'static str {
    match relation {
        Relation::Contact => "Contact",
        Relation::RequestSent => "Request sent · waiting",
        Relation::RequestReceived => "Sent you a request",
        Relation::Me => "You",
        Relation::Nobody => "Not a contact",
    }
}

/// Whether the card offers Say hi and Add contact to `p`.
#[must_use]
pub fn may_ask(p: &PersonDto, card: &Card) -> bool {
    p.relation == Relation::Nobody && (p.may_request || card.link_code.is_some())
}

/// The quick first lines the sheet offers.
pub const QUICK_HELLOS: [&str; 3] = ["👋 Hi!", "Nice build", "Want to explore together?"];

/// What the sheet's fine print says about a first message to `name`.
#[must_use]
pub fn hello_fine_print(name: &str) -> String {
    format!(
        "Encrypted, text only. {name} sees this, your name and where you met. No points, \
         pictures or locations until they accept."
    )
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
        self.outbox
            .push(NetMessage::RequestContact(msg::RequestContact {
                request_id,
                username: username.to_string(),
                link_code,
            }));
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
        self.outbox
            .push(NetMessage::AnswerContact(msg::AnswerContact {
                request_id,
                username: username.to_string(),
                accept,
            }));
        // Gone from the tray at once: the server's next state confirms it.
        self.incoming
            .retain(|r| !r.username.eq_ignore_ascii_case(username));
    }

    /// Stop being contacts, or withdraw a request you sent.
    pub fn remove(&mut self, username: &str) {
        let request_id = self.request_id();
        self.outbox
            .push(NetMessage::RemoveContact(msg::RemoveContact {
                request_id,
                username: username.to_string(),
            }));
    }

    pub fn push_settings(&mut self, reset_link: bool) {
        let request_id = self.request_id();
        let s = &self.settings;
        self.outbox
            .push(NetMessage::SetContactSettings(msg::SetContactSettings {
                request_id,
                policy: s.policy,
                show_online: s.show_online,
                notify_requests: s.notify_requests,
                separate_new_accounts: s.separate_new_accounts,
                reset_link,
            }));
    }

    /// Open the person card for the avatar with this sim id.
    pub fn open_card_for_avatar(&mut self, avatar_id: u64) {
        let request_id = self.request_id();
        self.card_request = Some(request_id);
        self.card = Some(Card {
            looking: true,
            avatar_id: Some(avatar_id),
            ..Card::default()
        });
        self.outbox
            .push(NetMessage::LookupPerson(msg::LookupPerson {
                request_id,
                username: None,
                avatar_id: Some(avatar_id),
            }));
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
        self.outbox
            .push(NetMessage::LookupPerson(msg::LookupPerson {
                request_id,
                username: Some(username.to_string()),
                avatar_id: None,
            }));
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
        self.outbox
            .push(NetMessage::LookupPerson(msg::LookupPerson {
                request_id,
                username: Some(name),
                avatar_id: None,
            }));
    }

    /// A username looked up on the Add screen has not been answered yet.
    #[must_use]
    pub fn searching(&self) -> bool {
        self.found.is_none() && self.search_request.is_some()
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

/// "5 min ago", "yesterday": how long ago a meeting or a request was.
#[must_use]
/// A person row's second line, saying first that an AI agent speaks for the
/// account: the badge, in the one place a row has room for words.
fn agent_line(agent: bool, line: String) -> String {
    if agent {
        format!("AI agent · {line}")
    } else {
        line
    }
}

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
    layers: Res<crate::systems::layer_state::DataLayers>,
    mut new_link: ResMut<crate::systems::place_link::NewLink>,
    mut launched: Local<bool>,
) {
    if !*launched {
        *launched = true;
        contacts.pending_link = launch_contact();
    }
    // One tapped while the app already runs (`place_link::take_new_link`):
    // opened the same way, once signed in.
    if let Some(link) = new_link.contact.take() {
        contacts.pending_link = Some(link);
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
            .push(NetMessage::GetNearbyPeople(msg::GetNearbyPeople {
                request_id,
            }));
    }
    contacts.want_nearby = false;

    // A private chat's World view: where the people in it are, asked again
    // every few seconds while it is open and forgotten the moment it closes.
    // With no chat's view open, the Friends layer asks the same about every
    // contact, so they are on the map by default; the server still answers only
    // for those who show that they are online.
    let wanted = chat
        .world
        .as_deref()
        .filter(|k| ChatState::is_private(k))
        .map(|k| private.people_in(k))
        .unwrap_or_else(|| {
            if layers.friends_enabled {
                friends_to_ask(&contacts.contacts)
            } else {
                Vec::new()
            }
        });
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
        contacts
            .outbox
            .push(NetMessage::WherePeople(msg::WherePeople {
                request_id,
                usernames: wanted,
            }));
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
    ui_input: Res<UiInput>,
    metrics: Res<UiMetrics>,
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
    if ui_input.owns_click(touch.touch_click(), Some(cursor)) {
        return;
    }
    let Ok((cam, cell, tf)) = camera.single() else {
        return;
    };
    let cam_world = crate::grid::anchored_position_f64(cell, tf.translation);
    let view = GlobalTransform::from(Transform::from_rotation(tf.rotation));
    let scale = metrics.ui_scale();
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

/// The person card and the first-message sheet, drawn by the library
/// (`lib_person_card`): hands it the side only this system works out —
/// whether the session, the driver bar or an open panel hides them, how far
/// the on-screen keyboard lifts the sheet — and does the tap it handed back.
#[allow(clippy::too_many_arguments)]
pub fn drive_person_card(
    metrics: Res<UiMetrics>,
    mut contacts: ResMut<Contacts>,
    mut chat: ResMut<ChatState>,
    mut view: ResMut<ChatsView>,
    mut right_panel: ResMut<RightPanel>,
    online: Option<Res<OnlineSession>>,
    driver: Res<crate::systems::driver::DriverState>,
    keyboard: Res<crate::systems::onscreen_keyboard::OnScreenKeyboard>,
    mut greetings: ResMut<crate::systems::greet::Greetings>,
    mut interact: ResMut<crate::systems::interact::InteractState>,
) {
    let hidden = (online.is_none() && !contacts.demo) || driver.active;
    // On a phone an open panel is the whole screen; the card waits behind it.
    let card_hidden = metrics.compact() && !right_panel.is(RightPanel::None);
    // Read before taking, so a quiet frame leaves the resource unchanged.
    let action = if contacts.lib_view.action.is_some() {
        contacts.lib_view.action.take()
    } else {
        None
    };
    let view_now = PersonView {
        hidden,
        card_hidden,
        online: online.is_some(),
        lift: crate::systems::chat_overlay::keyboard_reserve(&metrics, keyboard.is_open()),
        action: None,
    };
    if contacts.lib_view != view_now {
        contacts.lib_view = view_now;
    }
    if let Some(a) = action {
        person_act(
            a,
            &mut contacts,
            &mut chat,
            &mut view,
            &mut right_panel,
            online.as_deref(),
            &mut greetings,
            &mut interact,
        );
    }
}

/// Do a tap on the person card or the first-message sheet: the library's
/// (`lib_person_card`) hands it over through [`PersonView::action`]. The
/// person is the one the card shows now.
#[allow(clippy::too_many_arguments)]
pub fn person_act(
    action: PersonAction,
    contacts: &mut Contacts,
    chat: &mut ChatState,
    view: &mut ChatsView,
    right_panel: &mut RightPanel,
    online: Option<&OnlineSession>,
    greetings: &mut crate::systems::greet::Greetings,
    interact: &mut crate::systems::interact::InteractState,
) {
    // The sheet's two taps need no card.
    match action {
        PersonAction::Send => {
            let ready = contacts
                .compose
                .as_ref()
                .is_some_and(|c| !c.text.trim().is_empty());
            if ready {
                if let Some(c) = contacts.compose.take() {
                    contacts.say_hi(&c.username, &c.text, c.link_code);
                    contacts.card = None;
                }
            }
            return;
        }
        PersonAction::Cancel => {
            contacts.compose = None;
            contacts.error = None;
            return;
        }
        PersonAction::Close => {
            contacts.card = None;
            contacts.error = None;
            return;
        }
        PersonAction::ToggleMenu => {
            if let Some(c) = contacts.card.as_mut() {
                c.menu = !c.menu;
            }
            return;
        }
        _ => {}
    }
    let Some(card) = contacts.card.clone() else {
        return;
    };
    let Some(p) = card.person.as_ref() else {
        return;
    };
    let name = p.username.clone();
    match action {
        PersonAction::SayHi => {
            contacts.error = None;
            contacts.compose = Some(Compose {
                username: name,
                link_code: card.link_code.clone(),
                ..Compose::default()
            });
        }
        PersonAction::Add => {
            contacts.request(&name, card.link_code.clone());
        }
        PersonAction::Message => {
            contacts.card = None;
            open_direct(chat, view, right_panel, &name);
        }
        PersonAction::SeeRequest => {
            contacts.card = None;
            view.open = None;
            view.sub = Some(Sub::Request(name));
            *right_panel = RightPanel::Chat;
        }
        PersonAction::Block => {
            contacts.card = None;
            if let Some(online) = online {
                online.send(
                    "set blocked",
                    NetMessage::SetBlocked(msg::SetBlocked {
                        request_id: 0,
                        username: name,
                        blocked: true,
                    }),
                );
            }
        }
        PersonAction::Report => {
            contacts.card = None;
            chat.report = Some(ReportDraft {
                username: name,
                message: None,
                reason: cartopolis_core::ReportReason::Harassment,
                note: String::new(),
                asset: None,
                community: None,
            });
        }
        PersonAction::Greet(greeting) => {
            if let (Some(id), Some(session)) = (card.avatar_id, online) {
                // The card goes, so the two can see each other; the prompt's
                // receipt line says what is waiting.
                contacts.card = None;
                greetings.offer(session, id, greeting);
                interact.last = Some(format!("{} — waiting for {name}", greeting.label()));
                interact.last_ttl = cartopolis_core::greet::OFFER_SECS;
            }
        }
        PersonAction::Send
        | PersonAction::Cancel
        | PersonAction::Close
        | PersonAction::ToggleMenu => {}
    }
}

/// A request's line in the tray: where you met, or when it was sent.
#[must_use]
pub(crate) fn request_line(r: &ContactRequestDto, now: u64) -> String {
    let line = match &r.met {
        Some(m) => met_text(m, now),
        None => format!("Sent {}", ago(r.sent_at_ms, now)),
    };
    agent_line(r.agent, line)
}

/// A contact's line in the list: online, added today where you met, or
/// how long ago.
#[must_use]
pub(crate) fn contact_line(c: &ContactDto, now: u64) -> String {
    let line = match (&c.met, c.online) {
        (_, Some(true)) => "online".to_string(),
        (Some(m), _) if now.saturating_sub(c.since_ms) < 86_400_000 => {
            format!("added today · {}", met_text(m, now).to_lowercase())
        }
        _ => format!("contact since {}", ago(c.since_ms, now)),
    };
    agent_line(c.agent, line)
}

/// An online contact's line, in "Online now".
#[must_use]
pub(crate) fn online_line(c: &ContactDto) -> String {
    agent_line(c.agent, "online".into())
}

/// What a username looked up on the Add screen is to you.
#[must_use]
pub(crate) fn found_line(p: &PersonDto) -> &'static str {
    match p.relation {
        Relation::Contact => "already a contact",
        Relation::RequestSent => "request sent · waiting",
        Relation::RequestReceived => "sent you a request",
        Relation::Me => "that's you",
        Relation::Nobody if p.may_request => "exact match",
        Relation::Nobody => "only takes requests from people they've met",
    }
}

/// What somebody wrote with their request: the lines of the private chat
/// that goes with it.
#[must_use]
pub(crate) fn request_lines<'a>(
    chat: &'a ChatState,
    username: &str,
) -> Vec<&'a crate::systems::chat::ChatLine> {
    chat.direct
        .get(&username.to_lowercase())
        .and_then(|k| chat.channels.iter().find(|c| &c.key == k))
        .map(|c| c.lines.iter().collect())
        .unwrap_or_default()
}

pub(crate) fn in_common_text(names: &[String]) -> String {
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

/// What a contacts screen decided: the library's taps (`lib_contacts`) fill
/// one, and [`act`] does it.
#[derive(Default)]
pub(crate) struct Act {
    pub(crate) back: bool,
    pub(crate) to: Option<Sub>,
    pub(crate) open_chat: Option<String>,
    pub(crate) open_channel: Option<String>,
    pub(crate) accept: Option<String>,
    pub(crate) decline: Option<String>,
    pub(crate) block: Option<String>,
    pub(crate) report: Option<(String, Option<String>)>,
    pub(crate) withdraw: Option<String>,
    pub(crate) remove: Option<String>,
    pub(crate) card: Option<String>,
    pub(crate) hi: Option<String>,
    pub(crate) add: Option<String>,
    pub(crate) settings: bool,
    pub(crate) reset_link: bool,
    pub(crate) search: bool,
    pub(crate) share_link: bool,
}

/// Do what a contacts screen (`lib_contacts`) asked for.
pub(crate) fn act(
    sub: &Sub,
    act: Act,
    contacts: &mut Contacts,
    chat: &mut ChatState,
    view: &mut ChatsView,
    online: Option<&OnlineSession>,
    me: &str,
) {
    if act.back && view.sub_root.as_ref() == Some(sub) {
        // Opened from the You page: back goes there.
        view.sub = None;
        view.sub_root = None;
        contacts.error = None;
    } else if act.back {
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
                NetMessage::SetBlocked(msg::SetBlocked {
                    request_id: 0,
                    username: name,
                    blocked: true,
                }),
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
            community: None,
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
pub(crate) fn share(text: &str) {
    #[cfg(target_os = "android")]
    {
        if crate::platform::android::share_text("Cartopolis", text).is_ok() {
            return;
        }
    }
    crate::utils::clipboard::set_text(text);
}

/// A request policy in a few words, as the You page's row says it.
#[must_use]
pub fn policy_word(policy: RequestPolicy) -> &'static str {
    match policy {
        RequestPolicy::Anyone => "Anyone",
        RequestPolicy::Near => "People you've been near",
        RequestPolicy::LinkOnly => "Only with my link",
    }
}

// ─── A scripted capture's stand-in ─────────────────────────────────────────

/// Fill the screens with the design's people for `--ui contacts` and friends:
/// the same switch `ChatsView::demo` is, so the screens draw with no session.
pub fn seed_demo(contacts: &mut Contacts, chat: &mut ChatState) {
    let now_ms = unix_ms();
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
                meetup: None,
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
            agent: false,
        },
        ContactDto {
            username: "anouk".into(),
            since_ms: now_ms - 3_600_000,
            met,
            online: Some(false),
            agent: false,
        },
        ContactDto {
            username: "tom".into(),
            since_ms: now_ms - 90 * 86_400_000,
            met: None,
            online: Some(true),
            agent: false,
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
            agent: false,
        },
        ContactRequestDto {
            username: "sanne_k".into(),
            sent_at_ms: now_ms - 600_000,
            met: None,
            new_account: true,
            joined_ms: now_ms - 3 * 86_400_000,
            in_common: vec![],
            agent: false,
        },
    ];
    contacts.outgoing = vec![ContactRequestDto {
        username: "bas_builds".into(),
        sent_at_ms: now_ms - 120_000,
        met,
        new_account: false,
        joined_ms: now_ms - 200 * 86_400_000,
        in_common: vec!["tom".into()],
        agent: false,
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
    let now_ms = unix_ms();
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

/// Three friends around Groningen's centre for `--ui friends`: the Friends
/// layer's markers with no chat open, which a scripted run has no server for.
pub fn seed_demo_friends(contacts: &mut Contacts) {
    let at = |username: &str, avatar_id, lat, lng| Whereabout {
        username: username.into(),
        avatar_id,
        from: (lat, lng),
        to: (lat, lng),
        arrived: 0.0,
    };
    contacts.whereabouts = vec![
        at("Sanne", 2, 53.2154, 6.5671),
        at("Joris", 3, 53.2190, 6.5630),
        at("Mila", 4, 53.2172, 6.5728),
    ];
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
    fn the_friends_layer_asks_after_online_contacts_first_and_at_most_a_page() {
        let c = |name: &str, online| ContactDto {
            username: name.into(),
            since_ms: 0,
            met: None,
            online,
            agent: false,
        };
        let list = vec![
            c("Bas", Some(false)),
            c("anna", None),
            c("Joris", Some(true)),
        ];
        assert_eq!(friends_to_ask(&list), vec!["anna", "Joris", "Bas"]);
        let many: Vec<ContactDto> = (0..50).map(|i| c(&format!("p{i:02}"), None)).collect();
        assert_eq!(friends_to_ask(&many).len(), MAX_FRIENDS_ASKED);
    }

    #[test]
    fn contact_requests_have_their_own_id_range() {
        let mut c = Contacts::default();
        let id = c.request("bas", None);
        assert!(is_contacts_request(id));
        assert!(!cartopolis_chat::is_private_request(id));
    }

    #[test]
    fn a_first_message_waits_until_its_request_is_on_record() {
        let mut c = Contacts::default();
        c.say_hi("bas", "hi there", None);
        assert_eq!(c.hellos.len(), 1);
        assert!(matches!(
            c.outbox[0],
            NetMessage::RequestContact(msg::RequestContact { .. })
        ));
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
