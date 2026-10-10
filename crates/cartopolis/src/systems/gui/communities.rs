//! Communities in the Chats panel (`docs/notes/communities.md`, the design is
//! `docs/mockups/communities/` v1–v3).
//!
//! A community is a named set of people with a few topics, and being in it is
//! being in every topic. The topics themselves are ordinary private chats —
//! `chats` draws them as it draws any other — so this module draws only what is
//! around them: the communities in the list, the screens for finding, starting,
//! running and editing one, and the people in it with their roles.
//!
//! Like the rest of the panel it never touches the engine. Everything it wants
//! done goes on [`Communities::ops`], which `private_chat::drive_private_chats`
//! drains into `cartopolis_chat::Engine`, and everything it shows is the
//! latest [`Communities::list`] the server pushed.

use bevy::prelude::*;
use bevy::tasks::IoTaskPool;
use cartopolis_core::{
    CommunityDto, CommunityListing, CommunityRole, CommunitySettings, DirectoryOrder, JoinRule,
    ListingState, TopicDto,
};
use std::sync::{Arc, Mutex};
use uuid::Uuid;

use crate::systems::chat::ChatState;
use crate::systems::chats::{avatar, raised_frame, ChatKind};
use crate::systems::private_chat::{channel_key, group_of};
use crate::systems::{icons, theme};

/// Something for the engine to do, queued by the panel.
#[derive(Debug, Clone)]
pub enum CommunityOp {
    Create {
        settings: CommunitySettings,
        topics: Vec<String>,
        people: Vec<String>,
    },
    Edit(Uuid, CommunitySettings),
    Find(String, DirectoryOrder),
    Join(Uuid, String),
    Leave(Uuid),
    Decide(Uuid, String, bool),
    Add(Uuid, String),
    SetRole(Uuid, String, CommunityRole),
    Remove {
        id: Uuid,
        who: String,
        topic: Option<Vec<u8>>,
        block: bool,
    },
    Delete(Uuid),
    StartTopic(Uuid, String),
    EditTopic {
        id: Uuid,
        group: Vec<u8>,
        name: String,
        position: u32,
        archived: bool,
    },
    ModerateDelete(Vec<u8>, u64),
    ResolveReport(Uuid, i64),
}

/// The screen drawn instead of the chat list.
#[derive(Debug, Clone, PartialEq)]
pub enum Screen {
    Find,
    Preview(Uuid),
    Community(Uuid),
    Edit(Uuid),
    People(Uuid),
    Create,
}

/// A form being filled in: starting or editing a community.
#[derive(Debug, Clone)]
pub struct Draft {
    pub name: String,
    pub about: String,
    pub join: JoinRule,
    pub listed: bool,
    pub members_start_topics: bool,
    /// Starting: the first topics and people, comma separated.
    pub topics: String,
    pub people: String,
    /// Editing: each topic as it will be saved.
    pub topic_edits: Vec<TopicDto>,
}

impl Default for Draft {
    fn default() -> Self {
        Self {
            name: String::new(),
            about: String::new(),
            join: JoinRule::Ask,
            listed: true,
            members_start_topics: true,
            topics: "general".into(),
            people: String::new(),
            topic_edits: Vec::new(),
        }
    }
}

impl Draft {
    fn of(c: &CommunityDto) -> Self {
        Self {
            name: c.settings.name.clone(),
            about: c.settings.about.clone(),
            join: c.settings.join,
            listed: c.settings.listed,
            members_start_topics: c.settings.members_start_topics,
            topics: String::new(),
            people: String::new(),
            topic_edits: c.topics.clone(),
        }
    }

    fn settings(&self) -> CommunitySettings {
        CommunitySettings {
            name: self.name.trim().to_string(),
            about: self.about.trim().to_string(),
            join: self.join,
            listed: self.listed,
            members_start_topics: self.members_start_topics,
        }
    }
}

/// An admin's account action finished, in words for the notice line.
type AdminInbox = Arc<Mutex<Vec<String>>>;

#[derive(Resource, Default)]
pub struct Communities {
    /// Every community this account is in or waiting for, as last pushed.
    pub list: Vec<CommunityDto>,
    /// The last directory answer.
    pub directory: Vec<CommunityListing>,
    /// For the engine, drained every frame.
    pub ops: Vec<CommunityOp>,
    /// The screen in front of the list, if any.
    pub screen: Option<Screen>,
    /// A community this device just started: open it when it arrives.
    pub created: Option<Uuid>,
    /// An admin asked to delete this person's account; waiting for "yes".
    pub confirm_delete: Option<String>,
    draft: Draft,
    query: String,
    order: Option<DirectoryOrder>,
    /// What is typed into the community screen's fields.
    join_message: String,
    new_topic: String,
    add_people: String,
    /// Which of the community screen's two forms is open.
    adding: bool,
    starting_topic: bool,
    admin: AdminInbox,
    /// Seeded for a scripted capture (`seed_demo`), to be cleared after.
    demo: bool,
}

impl Communities {
    #[must_use]
    pub fn get(&self, id: Uuid) -> Option<&CommunityDto> {
        self.list.iter().find(|c| c.id == id)
    }

    /// The community and topic a chat key is, if it is a topic.
    #[must_use]
    pub fn topic(&self, key: &str) -> Option<(&CommunityDto, &TopicDto)> {
        let group = group_of(key)?;
        self.list
            .iter()
            .find_map(|c| c.topics.iter().find(|t| t.group == group).map(|t| (c, t)))
    }

    /// Whether a chat key is a topic of a community.
    #[must_use]
    pub fn is_topic(&self, key: &str) -> bool {
        self.topic(key).is_some()
    }

    /// Open a community's screen.
    pub fn show(&mut self, id: Uuid) {
        self.adding = false;
        self.starting_topic = false;
        self.screen = Some(Screen::Community(id));
    }

    /// Open the directory, asking for the most popular.
    pub fn find(&mut self) {
        self.screen = Some(Screen::Find);
        let order = self.order.unwrap_or(DirectoryOrder::Popular);
        self.order = Some(order);
        self.ops.push(CommunityOp::Find(self.query.clone(), order));
    }

    /// Open the form for starting one.
    pub fn start(&mut self) {
        self.draft = Draft::default();
        self.screen = Some(Screen::Create);
    }

    /// Lines an admin's account action wants on the notice line.
    pub fn take_admin_results(&mut self) -> Vec<String> {
        self.admin
            .lock()
            .map(|mut v| std::mem::take(&mut *v))
            .unwrap_or_default()
    }

    /// Suspend (`delete == false`) or delete somebody's account, as the web
    /// admin page does: find the account by name, then change it. The server
    /// checks the permission; this only asks.
    pub fn admin_act(&self, username: String, delete: bool) {
        let inbox = self.admin.clone();
        IoTaskPool::get()
            .spawn(async move {
                let said = admin_request(&username, delete).await;
                if let Ok(mut v) = inbox.lock() {
                    v.push(said);
                }
            })
            .detach();
    }
}

async fn admin_request(username: &str, delete: bool) -> String {
    use crate::systems::model_assets::{api_delete, api_get, api_patch};
    let name = username.split('@').next().unwrap_or(username);
    let body = match api_get("/api/admin/accounts").await {
        Ok(b) => b,
        Err(e) => return format!("Could not reach the account list: {e}"),
    };
    let id = serde_json::from_str::<serde_json::Value>(&body)
        .ok()
        .and_then(|v| {
            v.get("accounts")?.as_array()?.iter().find_map(|a| {
                let u = a.get("username")?.as_str()?;
                u.eq_ignore_ascii_case(name)
                    .then(|| a.get("id")?.as_str().map(str::to_string))
                    .flatten()
            })
        });
    let Some(id) = id else {
        return format!("No account called {name} on this server.");
    };
    let path = format!("/api/admin/accounts/{id}");
    let done = if delete {
        api_delete(&path)
            .await
            .map(|()| format!("Deleted {name}'s account."))
    } else {
        api_patch(&path, r#"{"disabled":true}"#.into())
            .await
            .map(|_| format!("Suspended {name}: they can't sign in until an admin lifts it."))
    };
    done.unwrap_or_else(|e| format!("Not done: {e}"))
}

/// What the community screens asked the panel to do.
#[derive(Default)]
pub struct Out {
    /// Open this chat.
    pub open: Option<String>,
    /// Message this person.
    pub message: Option<String>,
}

fn role_word(r: CommunityRole) -> &'static str {
    match r {
        CommunityRole::Owner => "Owner",
        CommunityRole::Moderator => "Moderator",
        CommunityRole::Member => "Member",
    }
}

fn rule_word(r: JoinRule) -> &'static str {
    match r {
        JoinRule::Open => "Open",
        JoinRule::Ask => "Asks first",
        JoinRule::Invite => "Invite only",
    }
}

fn rule_tone(r: JoinRule) -> egui::Color32 {
    match r {
        JoinRule::Open => egui::Color32::from_rgb(0x9F, 0xD3, 0xB4),
        JoinRule::Ask => theme::WARN,
        JoinRule::Invite => theme::TEXT_DIM,
    }
}

/// `name@server` as a person on this server reads it.
fn shown(address: &str, home: &str) -> String {
    cartopolis_core::address::display(address, home).to_string()
}

fn people(s: &str) -> Vec<String> {
    s.split([',', ' ', '\n'])
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(str::to_string)
        .collect()
}

/// The back chevron and a title, as every sub-screen of the panel has.
fn title_row(ui: &mut egui::Ui, title: &str) -> bool {
    let mut back = false;
    ui.horizontal(|ui| {
        back = ui
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
            .clicked();
        ui.label(
            egui::RichText::new(title)
                .font(egui::FontId::new(
                    theme::SIZE_PANEL_TITLE,
                    theme::brand_family(),
                ))
                .color(theme::CREAM),
        );
    });
    ui.add_space(8.0);
    back
}

/// A tappable card row: a rounded-square avatar, a name and a line under it,
/// and something on the right.
fn card_row(
    ui: &mut egui::Ui,
    salt: impl std::hash::Hash + std::fmt::Debug,
    name: &str,
    kind: ChatKind,
    sub: &str,
    sub_tone: egui::Color32,
    right: impl FnOnce(&mut egui::Ui),
) -> egui::Response {
    let id = egui::Id::new(("community_row", salt));
    let hovered = ui.ctx().read_response(id).is_some_and(|r| r.hovered());
    let inner = egui::Frame::NONE
        .fill(if hovered {
            theme::SURFACE_HOVER
        } else {
            crate::systems::chats::ROW_CARD
        })
        .corner_radius(egui::CornerRadius::same(14))
        .inner_margin(egui::Margin::symmetric(8, 7))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                avatar(ui, name, kind, 40.0);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    right(ui);
                    ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(name).size(15.0).color(theme::CREAM),
                            )
                            .truncate(),
                        );
                        ui.add(
                            egui::Label::new(egui::RichText::new(sub).size(12.5).color(sub_tone))
                                .wrap(),
                        );
                    });
                });
            });
        });
    ui.add_space(6.0);
    ui.interact(inner.response.rect, id, egui::Sense::click())
}

fn badge(ui: &mut egui::Ui, text: &str, tone: egui::Color32) {
    egui::Frame::NONE
        .fill(tone.gamma_multiply(0.18))
        .corner_radius(egui::CornerRadius::same(8))
        .inner_margin(egui::Margin::symmetric(7, 2))
        .show(ui, |ui| {
            ui.label(egui::RichText::new(text).size(11.5).color(tone));
        });
}

// ── The communities band of the chat list ─────────────────────────────────

/// What a tap in the list's communities band asked for.
pub enum ListPick {
    Open(String),
    Community(Uuid),
}

/// The communities band of the chat list: each community, and under it the
/// topics this account is in. `unread` counts a chat's unread lines.
pub fn list_band(
    ui: &mut egui::Ui,
    communities: &Communities,
    chat: &ChatState,
    unread: &dyn Fn(&str) -> usize,
) -> Option<ListPick> {
    if communities.list.is_empty() {
        return None;
    }
    let mut pick = None;
    theme::section(ui, "Communities");
    for c in &communities.list {
        let sub = match c.role {
            None => "Waiting for a moderator".to_string(),
            Some(_) => format!(
                "{} {} · {} {}",
                c.members.len(),
                if c.members.len() == 1 {
                    "person"
                } else {
                    "people"
                },
                c.topics.iter().filter(|t| !t.archived).count(),
                if c.topics.len() == 1 {
                    "topic"
                } else {
                    "topics"
                },
            ),
        };
        let waiting: usize = c
            .topics
            .iter()
            .map(|t| unread(&channel_key(&t.group)))
            .sum::<usize>()
            + c.requests.len();
        let row = card_row(
            ui,
            ("band", c.id),
            &c.settings.name,
            ChatKind::Community,
            &sub,
            if c.role.is_none() {
                theme::WARN
            } else {
                theme::TEXT_DIM
            },
            |ui| {
                if waiting > 0 {
                    badge(ui, &waiting.to_string(), theme::BRAND);
                }
            },
        );
        if row.clicked() {
            pick = Some(ListPick::Community(c.id));
        }
        for t in c.topics.iter().filter(|t| t.joined && !t.archived) {
            let key = channel_key(&t.group);
            let n = unread(&key);
            let last = chat
                .channels
                .iter()
                .find(|ch| ch.key == key)
                .and_then(|ch| ch.lines.last())
                .map(|l| {
                    if l.removed {
                        "Message removed by a moderator".to_string()
                    } else {
                        format!("{}: {}", l.sender, l.body)
                    }
                })
                .unwrap_or_default();
            let resp = ui
                .horizontal(|ui| {
                    ui.add_space(52.0);
                    let text = egui::RichText::new(format!("# {}", t.name))
                        .size(14.5)
                        .color(if n > 0 { theme::CREAM } else { theme::TEXT });
                    let r = ui.add(egui::Label::new(text).sense(egui::Sense::click()));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if n > 0 {
                            badge(ui, &n.to_string(), theme::BRAND);
                        }
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(last).size(12.0).color(theme::TEXT_DIM),
                            )
                            .truncate(),
                        );
                    });
                    r
                })
                .inner;
            if resp.clicked() {
                pick = Some(ListPick::Open(key));
            }
            ui.add_space(4.0);
        }
        ui.add_space(4.0);
    }
    pick
}

// ── The screens ───────────────────────────────────────────────────────────

/// Draw whichever community screen is up. `me` is this account's full
/// address.
pub fn screen(ui: &mut egui::Ui, communities: &mut Communities, me: &str) -> Out {
    let mut out = Out::default();
    let Some(screen) = communities.screen.clone() else {
        return out;
    };
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| match screen {
            Screen::Find => find_screen(ui, communities),
            Screen::Preview(id) => preview_screen(ui, communities, id, &mut out),
            Screen::Create => create_screen(ui, communities),
            Screen::Community(id) => community_screen(ui, communities, id, me, &mut out),
            Screen::Edit(id) => edit_screen(ui, communities, id),
            Screen::People(id) => people_screen(ui, communities, id, me, &mut out),
        });
    out
}

fn find_screen(ui: &mut egui::Ui, cs: &mut Communities) {
    if title_row(ui, "Find a community") {
        cs.screen = None;
        return;
    }
    let field = crate::systems::text_touch::field(
        ui,
        egui::TextEdit::singleline(&mut cs.query)
            .id(egui::Id::new("communities_find"))
            .desired_width(ui.available_width())
            .hint_text("Search names and descriptions"),
    );
    let mut order = cs.order.unwrap_or(DirectoryOrder::Popular);
    let changed = theme::segmented(
        ui,
        &mut order,
        &[
            (DirectoryOrder::Popular, "Popular"),
            (DirectoryOrder::New, "New"),
        ],
    );
    if changed || (field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))) {
        cs.order = Some(order);
        cs.ops.push(CommunityOp::Find(cs.query.clone(), order));
    }
    ui.add_space(6.0);
    if cs.directory.is_empty() {
        theme::empty_state(
            ui,
            "No communities found",
            "Only communities that chose to be listed appear here. Start one with the + on the chat list.",
        );
    }
    for l in cs.directory.clone() {
        let sub = format!(
            "{} · {} {}",
            if l.about.is_empty() {
                "No description"
            } else {
                &l.about
            },
            l.members,
            if l.members == 1 { "person" } else { "people" },
        );
        let resp = card_row(
            ui,
            ("find", l.id),
            &l.name,
            ChatKind::Community,
            &sub,
            theme::TEXT_DIM,
            |ui| match l.mine {
                ListingState::Member => badge(ui, "You're in", theme::ACCENT),
                ListingState::Asked => badge(ui, "Asked", theme::TEXT_DIM),
                ListingState::Refused => badge(ui, "Can't join", theme::ERROR),
                ListingState::Outside => badge(ui, rule_word(l.join), rule_tone(l.join)),
            },
        );
        if resp.clicked() {
            cs.join_message.clear();
            cs.screen = Some(match l.mine {
                ListingState::Member => Screen::Community(l.id),
                _ => Screen::Preview(l.id),
            });
        }
    }
}

fn hero(ui: &mut egui::Ui, name: &str, about: &str, line: &str) {
    ui.vertical_centered(|ui| {
        avatar(ui, name, ChatKind::Community, 64.0);
        ui.add_space(6.0);
        ui.label(egui::RichText::new(name).size(21.0).color(theme::CREAM));
        if !about.is_empty() {
            ui.add(
                egui::Label::new(egui::RichText::new(about).size(13.5).color(theme::TEXT_DIM))
                    .wrap(),
            );
        }
        if !line.is_empty() {
            ui.label(egui::RichText::new(line).size(12.5).color(theme::TEXT_DIM));
        }
    });
    ui.add_space(8.0);
}

fn preview_screen(ui: &mut egui::Ui, cs: &mut Communities, id: Uuid, out: &mut Out) {
    if title_row(ui, "Community") {
        cs.screen = Some(Screen::Find);
        return;
    }
    let Some(l) = cs.directory.iter().find(|l| l.id == id).cloned() else {
        theme::note(ui, "This community is no longer listed.");
        return;
    };
    hero(
        ui,
        &l.name,
        &l.about,
        &format!("{} people · {}", l.members, rule_word(l.join)),
    );
    if !l.topics.is_empty() {
        theme::section(ui, "Topics");
        ui.horizontal_wrapped(|ui| {
            for t in &l.topics {
                badge(ui, &format!("# {t}"), theme::TEXT_DIM);
            }
        });
    }
    theme::section(ui, "Moderators");
    for m in &l.moderators {
        let who = shown(&m.address, "");
        let r = card_row(
            ui,
            ("mod", &m.address),
            &who,
            ChatKind::Private,
            role_word(m.role),
            theme::TEXT_DIM,
            |_| {},
        );
        if r.clicked() {
            out.message = Some(m.address.clone());
        }
    }
    ui.add_space(8.0);
    match (l.mine, l.join) {
        (ListingState::Member, _) => {
            if theme::primary_button_sized(ui, "Open", ui.available_width()).clicked() {
                cs.show(id);
            }
        }
        (ListingState::Asked, _) => {
            theme::note(ui, "You asked to join. A moderator will answer.");
            if theme::action_button(ui, "Withdraw the request").clicked() {
                cs.ops.push(CommunityOp::Leave(id));
                cs.screen = Some(Screen::Find);
            }
        }
        (ListingState::Refused, _) => {
            theme::note(ui, "You can't ask to join this community again yet.");
        }
        (ListingState::Outside, JoinRule::Invite) => {
            theme::note(ui, "Only people a moderator adds can join this community.");
        }
        (ListingState::Outside, JoinRule::Open) => {
            if theme::primary_button_sized(ui, "Join", ui.available_width()).clicked() {
                cs.ops.push(CommunityOp::Join(id, String::new()));
                cs.show(id);
            }
        }
        (ListingState::Outside, JoinRule::Ask) => {
            raised_frame().show(ui, |ui| {
                ui.label(
                    egui::RichText::new("A moderator confirms new members.").color(theme::CREAM),
                );
                theme::note(
                    ui,
                    "Say who you are; they see your name and this message, nothing else.",
                );
                crate::systems::text_touch::field(
                    ui,
                    egui::TextEdit::singleline(&mut cs.join_message)
                        .id(egui::Id::new("communities_join_message"))
                        .desired_width(ui.available_width())
                        .hint_text("A line to the moderators (optional)"),
                );
            });
            ui.add_space(6.0);
            if theme::primary_button_sized(ui, "Ask to join", ui.available_width()).clicked() {
                cs.ops
                    .push(CommunityOp::Join(id, cs.join_message.trim().to_string()));
                cs.join_message.clear();
                cs.screen = None;
            }
        }
    }
}

fn settings_form(ui: &mut egui::Ui, d: &mut Draft) {
    ui.label(
        egui::RichText::new("Name")
            .size(12.5)
            .color(theme::TEXT_DIM),
    );
    crate::systems::text_touch::field(
        ui,
        egui::TextEdit::singleline(&mut d.name)
            .id(egui::Id::new("communities_name"))
            .desired_width(ui.available_width())
            .hint_text("Garage 44"),
    );
    ui.add_space(4.0);
    ui.label(
        egui::RichText::new("What is it for? (optional)")
            .size(12.5)
            .color(theme::TEXT_DIM),
    );
    crate::systems::text_touch::field(
        ui,
        egui::TextEdit::singleline(&mut d.about)
            .id(egui::Id::new("communities_about"))
            .desired_width(ui.available_width())
            .hint_text("Building cartopolis together"),
    );
    theme::section(ui, "Who can join");
    theme::segmented(
        ui,
        &mut d.join,
        &[
            (JoinRule::Open, "Anyone"),
            (JoinRule::Ask, "Asks first"),
            (JoinRule::Invite, "Invite only"),
        ],
    );
    theme::note(
        ui,
        match d.join {
            JoinRule::Open => "Anyone who finds it joins straight away.",
            JoinRule::Ask => "Anyone may ask; a moderator confirms.",
            JoinRule::Invite => "Only people a moderator adds.",
        },
    );
    ui.add_space(4.0);
    ui.checkbox(&mut d.listed, "Show in Find a community");
    theme::note(
        ui,
        "Listed: its name, description, topic names, moderators and size are public on this server.",
    );
    ui.checkbox(&mut d.members_start_topics, "Members may start topics");
}

fn create_screen(ui: &mut egui::Ui, cs: &mut Communities) {
    if title_row(ui, "New community") {
        cs.screen = None;
        return;
    }
    settings_form(ui, &mut cs.draft);
    theme::section(ui, "Topics to start with");
    crate::systems::text_touch::field(
        ui,
        egui::TextEdit::singleline(&mut cs.draft.topics)
            .id(egui::Id::new("communities_topics"))
            .desired_width(ui.available_width())
            .hint_text("general, meet-ups (separated by commas)"),
    );
    theme::section(ui, "People");
    crate::systems::text_touch::field(
        ui,
        egui::TextEdit::singleline(&mut cs.draft.people)
            .id(egui::Id::new("communities_people"))
            .desired_width(ui.available_width())
            .hint_text("Usernames, separated by commas (optional)"),
    );
    ui.add_space(10.0);
    let ready = !cs.draft.name.trim().is_empty();
    if theme::action_enabled_because(ui, ready, "Start the community", "Give it a name first")
        .clicked()
        && ready
    {
        let topics: Vec<String> = cs
            .draft
            .topics
            .split(',')
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(str::to_string)
            .collect();
        cs.ops.push(CommunityOp::Create {
            settings: cs.draft.settings(),
            topics,
            people: people(&cs.draft.people),
        });
        cs.screen = None;
    }
    theme::note(
        ui,
        "What is said in a community is end-to-end encrypted, like every private chat.",
    );
}

#[allow(clippy::too_many_lines)]
fn community_screen(ui: &mut egui::Ui, cs: &mut Communities, id: Uuid, me: &str, out: &mut Out) {
    if title_row(ui, "") {
        cs.screen = None;
        return;
    }
    let Some(c) = cs.get(id).cloned() else {
        theme::note(ui, "You are no longer in this community.");
        return;
    };
    let Some(role) = c.role else {
        hero(ui, &c.settings.name, &c.settings.about, "");
        theme::note(ui, "You asked to join. A moderator will answer.");
        if theme::action_button(ui, "Withdraw the request").clicked() {
            cs.ops.push(CommunityOp::Leave(id));
            cs.screen = None;
        }
        return;
    };
    let moderates = role.moderates();
    hero(
        ui,
        &c.settings.name,
        &c.settings.about,
        &format!(
            "{} people · {} · {}",
            c.members.len(),
            rule_word(c.settings.join),
            if c.settings.listed {
                "listed"
            } else {
                "unlisted"
            }
        ),
    );
    ui.horizontal_wrapped(|ui| {
        if moderates && theme::action_button(ui, format!("{} Add people", icons::ADD)).clicked() {
            cs.adding = !cs.adding;
            cs.starting_topic = false;
        }
        if (moderates || c.settings.members_start_topics)
            && theme::action_button(ui, format!("{} New topic", icons::ADD)).clicked()
        {
            cs.starting_topic = !cs.starting_topic;
            cs.adding = false;
        }
        if theme::action_button(ui, format!("{} {}", icons::GROUP, c.members.len())).clicked() {
            cs.screen = Some(Screen::People(id));
        }
        if moderates && theme::action_button(ui, format!("{} Edit", icons::SETTINGS)).clicked() {
            cs.draft = Draft::of(&c);
            cs.screen = Some(Screen::Edit(id));
        }
    });
    if cs.adding {
        raised_frame().show(ui, |ui| {
            crate::systems::text_touch::field(
                ui,
                egui::TextEdit::singleline(&mut cs.add_people)
                    .id(egui::Id::new("communities_add_people"))
                    .desired_width(ui.available_width())
                    .hint_text("Usernames, separated by commas"),
            );
            theme::note(
                ui,
                "They'll be in every topic, and read from now on, not what was said before.",
            );
            if theme::action_button(ui, "Add").clicked() {
                for p in people(&cs.add_people) {
                    cs.ops.push(CommunityOp::Add(id, p));
                }
                cs.add_people.clear();
                cs.adding = false;
            }
        });
    }
    if cs.starting_topic {
        raised_frame().show(ui, |ui| {
            crate::systems::text_touch::field(
                ui,
                egui::TextEdit::singleline(&mut cs.new_topic)
                    .id(egui::Id::new("communities_new_topic"))
                    .desired_width(ui.available_width())
                    .hint_text("Topic name, e.g. meet-ups"),
            );
            if theme::action_button(ui, "Start topic").clicked() && !cs.new_topic.trim().is_empty()
            {
                cs.ops
                    .push(CommunityOp::StartTopic(id, cs.new_topic.trim().to_string()));
                cs.new_topic.clear();
                cs.starting_topic = false;
            }
        });
    }
    if moderates && !c.requests.is_empty() {
        theme::section(ui, &format!("Asking to join ({})", c.requests.len()));
        for r in &c.requests {
            let who = shown(&r.address, &c.home);
            let line = if r.message.is_empty() {
                "No message".to_string()
            } else {
                format!("\u{201c}{}\u{201d}", r.message)
            };
            card_row(
                ui,
                ("req", &r.address),
                &who,
                ChatKind::Private,
                &line,
                theme::TEXT_DIM,
                |ui| {
                    if theme::action_button(ui, format!("{} Let in", icons::CHECK)).clicked() {
                        cs.ops
                            .push(CommunityOp::Decide(id, r.address.clone(), true));
                    }
                    if theme::action_button(ui, icons::CLEAR)
                        .on_hover_text("Turn down")
                        .clicked()
                    {
                        cs.ops
                            .push(CommunityOp::Decide(id, r.address.clone(), false));
                    }
                },
            );
        }
        theme::note(
            ui,
            "Letting someone in adds them to every topic. A refusal stands for a week.",
        );
    }
    if moderates && !c.reports.is_empty() {
        theme::section(ui, &format!("Reports ({})", c.reports.len()));
        for r in &c.reports {
            raised_frame().show(ui, |ui| {
                ui.label(
                    egui::RichText::new(format!(
                        "{} reported {} · {}",
                        shown(&r.reporter, &c.home),
                        shown(&r.reported, &c.home),
                        r.reason.label()
                    ))
                    .size(12.5)
                    .color(theme::TEXT_DIM),
                );
                if !r.line.is_empty() {
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(format!("\u{201c}{}\u{201d}", r.line))
                                .color(theme::CREAM),
                        )
                        .wrap(),
                    );
                }
                if !r.note.is_empty() {
                    theme::note(ui, &r.note);
                }
                ui.horizontal_wrapped(|ui| {
                    if theme::action_button(ui, "Delete the message").clicked() {
                        cs.ops
                            .push(CommunityOp::ModerateDelete(r.group.clone(), r.seq));
                        cs.ops.push(CommunityOp::ResolveReport(id, r.id));
                    }
                    if theme::action_button(ui, "Remove them").clicked() {
                        cs.ops.push(CommunityOp::Remove {
                            id,
                            who: r.reported.clone(),
                            topic: None,
                            block: false,
                        });
                        cs.ops.push(CommunityOp::ResolveReport(id, r.id));
                    }
                    if theme::action_button(ui, "Dismiss").clicked() {
                        cs.ops.push(CommunityOp::ResolveReport(id, r.id));
                    }
                });
            });
            ui.add_space(4.0);
        }
    }
    theme::section(ui, "Topics");
    for t in &c.topics {
        let key = channel_key(&t.group);
        let sub = if t.archived {
            "Archived: its history stays, nothing new"
        } else if t.joined {
            "Open"
        } else {
            "Being added…"
        };
        let r = card_row(
            ui,
            ("topic", &key),
            &format!("# {}", t.name),
            ChatKind::Topic,
            sub,
            theme::TEXT_DIM,
            |_| {},
        );
        if r.clicked() && t.joined {
            out.open = Some(key);
        }
    }
    theme::note(
        ui,
        "Everyone here is in every topic. Somebody added later reads from the moment they joined.",
    );
    ui.add_space(10.0);
    if role == CommunityRole::Owner {
        theme::note(
            ui,
            "You own this community. Hand it over on People before leaving.",
        );
    } else if theme::confirm_button(
        ui,
        egui::Id::new(("communities_leave", id)),
        "Leave the community",
        "Leave: tap again",
    ) {
        cs.ops.push(CommunityOp::Leave(id));
        cs.screen = None;
    }
    let _ = me;
}

fn edit_screen(ui: &mut egui::Ui, cs: &mut Communities, id: Uuid) {
    if title_row(ui, "Edit community") {
        cs.show(id);
        return;
    }
    let Some(c) = cs.get(id).cloned() else {
        cs.screen = None;
        return;
    };
    settings_form(ui, &mut cs.draft);
    if !cs.draft.topic_edits.is_empty() {
        theme::section(ui, "Topics");
        for (i, t) in cs.draft.topic_edits.iter_mut().enumerate() {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("#").color(theme::TEXT_DIM));
                crate::systems::text_touch::field(
                    ui,
                    egui::TextEdit::singleline(&mut t.name)
                        .id(egui::Id::new(("communities_topic_edit", i)))
                        .desired_width(ui.available_width() - 110.0),
                );
                ui.checkbox(&mut t.archived, "Archived");
            });
        }
    }
    ui.add_space(10.0);
    if theme::primary_button_sized(ui, "Save", ui.available_width()).clicked() {
        cs.ops.push(CommunityOp::Edit(id, cs.draft.settings()));
        for (i, t) in cs.draft.topic_edits.iter().enumerate() {
            let before = c.topics.iter().find(|b| b.group == t.group);
            if before.is_some_and(|b| b.name != t.name || b.archived != t.archived) {
                cs.ops.push(CommunityOp::EditTopic {
                    id,
                    group: t.group.clone(),
                    name: t.name.clone(),
                    position: i as u32,
                    archived: t.archived,
                });
            }
        }
        cs.show(id);
    }
    if c.role == Some(CommunityRole::Owner) {
        theme::section(ui, "Danger");
        if theme::confirm_button(
            ui,
            egui::Id::new(("communities_delete", id)),
            "Delete the community",
            "Delete it and every topic: tap again",
        ) {
            cs.ops.push(CommunityOp::Delete(id));
            cs.screen = None;
        }
    }
}

fn people_screen(ui: &mut egui::Ui, cs: &mut Communities, id: Uuid, me: &str, out: &mut Out) {
    if title_row(ui, "People") {
        cs.show(id);
        return;
    }
    let Some(c) = cs.get(id).cloned() else {
        cs.screen = None;
        return;
    };
    let mine = c.role.unwrap_or(CommunityRole::Member);
    let mut section = |ui: &mut egui::Ui, title: &str, running: bool| {
        theme::section(ui, title);
        for m in c.members.iter().filter(|m| m.role.moderates() == running) {
            let who = shown(&m.address, &c.home);
            let is_me = m.address.eq_ignore_ascii_case(me);
            let row = card_row(
                ui,
                ("person", &m.address),
                &who,
                ChatKind::Private,
                if is_me { "You" } else { role_word(m.role) },
                theme::TEXT_DIM,
                |ui| {
                    if !is_me {
                        ui.label(egui::RichText::new(icons::MORE).color(theme::TEXT_DIM));
                    }
                },
            );
            if is_me {
                continue;
            }
            row.context_menu(|ui| {
                ui.label(egui::RichText::new(&who).strong().color(theme::CREAM));
                ui.separator();
                if ui.button("Message").clicked() {
                    out.message = Some(m.address.clone());
                    ui.close();
                }
                if mine == CommunityRole::Owner {
                    if m.role == CommunityRole::Member && ui.button("Make moderator").clicked() {
                        cs.ops.push(CommunityOp::SetRole(
                            id,
                            m.address.clone(),
                            CommunityRole::Moderator,
                        ));
                        ui.close();
                    }
                    if m.role == CommunityRole::Moderator
                        && ui.button("Stop being a moderator").clicked()
                    {
                        cs.ops.push(CommunityOp::SetRole(
                            id,
                            m.address.clone(),
                            CommunityRole::Member,
                        ));
                        ui.close();
                    }
                    if ui.button("Hand the community over").clicked() {
                        cs.ops.push(CommunityOp::SetRole(
                            id,
                            m.address.clone(),
                            CommunityRole::Owner,
                        ));
                        ui.close();
                    }
                }
                let may_remove = match m.role {
                    CommunityRole::Owner => false,
                    CommunityRole::Moderator => mine == CommunityRole::Owner,
                    CommunityRole::Member => mine.moderates(),
                };
                if may_remove {
                    ui.separator();
                    for t in c.topics.iter().filter(|t| !t.archived) {
                        if ui
                            .button(
                                egui::RichText::new(format!("Remove from # {}", t.name))
                                    .color(theme::ERROR),
                            )
                            .clicked()
                        {
                            cs.ops.push(CommunityOp::Remove {
                                id,
                                who: m.address.clone(),
                                topic: Some(t.group.clone()),
                                block: false,
                            });
                            ui.close();
                        }
                    }
                    if ui
                        .button(
                            egui::RichText::new("Remove from the community").color(theme::ERROR),
                        )
                        .clicked()
                    {
                        cs.ops.push(CommunityOp::Remove {
                            id,
                            who: m.address.clone(),
                            topic: None,
                            block: false,
                        });
                        ui.close();
                    }
                    if ui
                        .button(
                            egui::RichText::new("Remove and block from rejoining")
                                .color(theme::ERROR),
                        )
                        .clicked()
                    {
                        cs.ops.push(CommunityOp::Remove {
                            id,
                            who: m.address.clone(),
                            topic: None,
                            block: true,
                        });
                        ui.close();
                    }
                }
            });
        }
    };
    section(ui, "Running it", true);
    section(ui, "Members", false);
    theme::note(
        ui,
        "Tap and hold a person for what you can do. Only the owner appoints moderators.",
    );
}

/// What a topic's chat header says under its name, and the community it opens.
#[must_use]
pub fn topic_header(communities: &Communities, key: &str) -> Option<(String, String, Uuid)> {
    let (c, t) = communities.topic(key)?;
    Some((
        format!("# {}", t.name),
        format!(
            "{} · {} people{}",
            c.settings.name,
            c.members.len(),
            if t.archived { " · archived" } else { "" }
        ),
        c.id,
    ))
}

/// "Delete this account?" for a server admin, over everything: deleting an
/// account cannot be undone, so it is never one tap in a context menu.
pub fn confirm_account_delete(ctx: &egui::Context, cs: &mut Communities) {
    let Some(who) = cs.confirm_delete.clone() else {
        return;
    };
    let mut yes = false;
    let mut no = false;
    egui::Window::new("Delete account")
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .show(ctx, |ui| {
            ui.label(
                egui::RichText::new(format!(
                    "Delete {who}'s account? Their devices, memberships and contacts go, \
                     and it cannot be undone."
                ))
                .color(theme::CREAM),
            );
            ui.horizontal(|ui| {
                yes = theme::action_button(
                    ui,
                    egui::RichText::new("Delete the account").color(theme::ERROR),
                )
                .clicked();
                no = theme::action_button(ui, "Cancel").clicked();
            });
        });
    if yes {
        cs.admin_act(who, true);
    }
    if yes || no {
        cs.confirm_delete = None;
    }
}

/// Communities for a scripted capture (`--ui communities` / `community` /
/// `findcommunity` / `newchat`): every scripted run is offline, and these
/// screens need a server to have anything on them. `screen` is the harness's
/// name for which one is wanted.
pub fn seed_demo(
    cs: &mut Communities,
    chat: &mut ChatState,
    view: &mut crate::systems::chats::ChatsView,
    screen: &str,
) {
    use cartopolis_core::{CommunityMemberDto, CommunityReportDto, JoinRequestDto, ReportReason};
    let home = "cartopolis.org".to_string();
    let who = |n: &str| format!("{n}@cartopolis.org");
    let member = |n: &str, role| CommunityMemberDto {
        address: who(n),
        role,
    };
    let topic = |n: u8, name: &str, joined: bool| TopicDto {
        group: cartopolis_core::address::group_id("cartopolis.org", [n; 16]),
        name: name.into(),
        position: u32::from(n),
        archived: false,
        joined,
    };
    let garage = Uuid::from_bytes([1; 16]);
    let fietsers = Uuid::from_bytes([2; 16]);
    let buurt = Uuid::from_bytes([3; 16]);
    let general = topic(1, "general", true);
    let map_data = topic(2, "map-data", true);
    let meetups = topic(3, "meet-ups", true);
    let rides = topic(4, "sunday-rides", true);
    cs.list = vec![
        CommunityDto {
            id: garage,
            home: home.clone(),
            settings: CommunitySettings {
                name: "Garage 44".into(),
                about: "Building cartopolis together".into(),
                join: JoinRule::Ask,
                listed: true,
                members_start_topics: true,
            },
            role: Some(CommunityRole::Owner),
            members: vec![
                member("you", CommunityRole::Owner),
                member("karim", CommunityRole::Moderator),
                member("anna", CommunityRole::Member),
                member("bas", CommunityRole::Member),
                member("pieter", CommunityRole::Member),
            ],
            topics: vec![general.clone(), map_data.clone(), meetups.clone()],
            requests: vec![JoinRequestDto {
                address: who("sanne"),
                message: "I live on the Verlengde Hereweg".into(),
                requested_ms: 0,
            }],
            reports: vec![CommunityReportDto {
                id: 1,
                reporter: who("anna"),
                reported: who("pieter"),
                group: general.group.clone(),
                seq: 7,
                reason: ReportReason::Spam,
                note: String::new(),
                line: "Cheap phones, DM me, crypto only".into(),
                created_ms: 0,
            }],
        },
        CommunityDto {
            id: fietsers,
            home: home.clone(),
            settings: CommunitySettings {
                name: "Fietsers Groningen".into(),
                about: "Routes, repairs and Sunday rides".into(),
                join: JoinRule::Open,
                listed: true,
                members_start_topics: false,
            },
            role: Some(CommunityRole::Member),
            members: vec![
                member("joris", CommunityRole::Owner),
                member("you", CommunityRole::Member),
            ],
            topics: vec![rides.clone()],
            requests: Vec::new(),
            reports: Vec::new(),
        },
        CommunityDto {
            id: buurt,
            home,
            settings: CommunitySettings {
                name: "Buurt Oosterpoort".into(),
                about: "News, lost cats, the street party".into(),
                join: JoinRule::Ask,
                listed: true,
                members_start_topics: false,
            },
            role: None,
            members: Vec::new(),
            topics: Vec::new(),
            requests: Vec::new(),
            reports: Vec::new(),
        },
    ];
    for (t, lines) in [
        (
            &general,
            vec![
                ("Bas", "Did the phone fix land?"),
                ("Anna", "My phone shows everything from today on"),
            ],
        ),
        (&map_data, vec![("Bas", "BGT run for Zwolle is done")]),
        (&meetups, vec![("Karim", "Grote Markt, Saturday at 7?")]),
        (&rides, vec![("Joris", "Leaving from the Vismarkt at 10")]),
    ] {
        let key = channel_key(&t.group);
        if chat.channels.iter().any(|c| c.key == key) {
            continue;
        }
        chat.open_channel(&key, &format!("#{}", t.name));
        for (from, body) in lines {
            chat.receive(&key, 0, from.into(), body.into(), None);
        }
    }
    cs.directory = vec![
        CommunityListing {
            id: fietsers,
            name: "Fietsers Groningen".into(),
            about: "Routes, repairs and Sunday rides around the city".into(),
            join: JoinRule::Open,
            members: 48,
            topics: vec!["sunday-rides".into()],
            moderators: vec![member("joris", CommunityRole::Owner)],
            mine: ListingState::Member,
            created_ms: 0,
        },
        CommunityListing {
            id: buurt,
            name: "Buurt Oosterpoort".into(),
            about: "For people living in the Oosterpoort".into(),
            join: JoinRule::Ask,
            members: 131,
            topics: vec!["general".into(), "lost-and-found".into()],
            moderators: vec![member("marieke", CommunityRole::Owner)],
            mine: ListingState::Asked,
            created_ms: 0,
        },
        CommunityListing {
            id: Uuid::from_bytes([4; 16]),
            name: "Map makers NL".into(),
            about: "Buildings, paving and aerial imagery: what's wrong where".into(),
            join: JoinRule::Open,
            members: 27,
            topics: vec!["general".into()],
            moderators: vec![member("bas", CommunityRole::Owner)],
            mine: ListingState::Outside,
            created_ms: 0,
        },
    ];
    cs.demo = true;
    cs.screen = match screen {
        "community" => Some(Screen::Community(garage)),
        "find" => Some(Screen::Find),
        _ => None,
    };
    view.new_chat = screen == "new";
    view.new_kind = crate::systems::chats::NewKind::Choose;
}

/// Undo [`seed_demo`] for a capture that did not ask for it.
pub fn clear_demo(cs: &mut Communities) {
    if cs.demo {
        *cs = Communities::default();
    }
}
