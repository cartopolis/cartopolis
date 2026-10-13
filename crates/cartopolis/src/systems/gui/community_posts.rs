//! A community's public posts in the Chats panel (`docs/notes/fediverse.md`,
//! the design is `docs/mockups/communities/v4-public-posts.html`).
//!
//! A community page has two halves: **Posts**, public and on the fediverse, and
//! **Topics**, encrypted and members-only, as before. The two are told apart in
//! words and in colour everywhere ([`theme::PUBLIC`] against the topics'
//! padlock), because writing in the wrong one is the mistake that matters.
//!
//! Posts are HTTP, not the wire (`cartopolis_core::posts` says why), so nothing
//! here goes through the chat engine: requests are tasks on the I/O pool, their
//! answers are folded in by [`Posts::poll`] every frame, and the panel draws
//! whatever has arrived. A post written here always has a place — the camera's
//! view when the composer opened, answered by [`answer_post_view`] — and a
//! post's place opens on the map the way a point in a chat does.

use bevy::prelude::*;
use bevy::tasks::{IoTaskPool, Task};
use cartopolis_core::point::{Point, PointView};
use cartopolis_core::posts::{
    suggest_handle, NewPost, NewReply, PostDto, PostPlace, PostThread, PublicInfo, PublicSettings,
    SourceKind, WhoPosts, MAX_POST_TEXT,
};
use cartopolis_core::{CommunityDto, CommunityRole};
use std::collections::HashMap;
use uuid::Uuid;

use crate::systems::chats::{avatar, raised_frame, ChatKind};
use crate::systems::communities::{Communities, Out};
use crate::systems::model_assets::{api_delete, api_get, api_post};
use crate::systems::{icons, theme};

/// Which half of a community page is showing.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    #[default]
    Posts,
    Topics,
}

/// A post being written.
#[derive(Debug, Clone)]
pub struct Draft {
    pub community: Uuid,
    pub text: String,
    /// Where it is about: the camera's view when the composer opened, until
    /// somebody asks for the view again.
    pub place: Option<PostPlace>,
    /// A plain JPEG in the picture store.
    pub picture: Option<String>,
}

enum Done {
    Info(Uuid, Result<PublicInfo, String>),
    Feed(Uuid, Result<Vec<PostDto>, String>),
    Thread(Result<PostThread, String>),
    Posted(Uuid, Result<PostDto, String>),
    Replied(Uuid, Result<PostDto, String>),
    /// Something changed in a community: read it again.
    Changed(Uuid, Result<(), String>),
}

/// Everything the Posts half of the panel holds.
#[derive(Default)]
pub struct Posts {
    pub tab: Tab,
    info: HashMap<Uuid, PublicInfo>,
    feeds: HashMap<Uuid, Vec<PostDto>>,
    /// The open thread, once it has arrived, and which post it is.
    thread: Option<PostThread>,
    thread_open: Option<Uuid>,
    pub draft: Option<Draft>,
    reply: String,
    handle: String,
    follow: String,
    /// Something went wrong, in words, until the next thing works.
    pub note: Option<String>,
    tasks: Vec<Task<Done>>,
    /// The composer wants the camera's view (`answer_post_view`).
    pub want_view: bool,
    /// "Post this view" from the map, waiting for a community to be chosen.
    pub from_view: Option<PostPlace>,
    /// The community whose posts are pinned on the map.
    pub on_map: Option<Uuid>,
    /// The community whose page last asked for its posts.
    loaded: Option<Uuid>,
    sending: bool,
    settings_open: bool,
    /// The post whose actions are open.
    menu_for: Option<Uuid>,
}

/// A failed request in words. The client's HTTP layer keeps only the status,
/// so the status is what is explained.
fn said(e: &str) -> String {
    let code = e.rsplit(' ').next().and_then(|c| c.parse::<u16>().ok());
    match code {
        Some(401) => "Sign in again: the session has expired.".into(),
        Some(403) => "You may not do that here.".into(),
        Some(404) => "That is no longer there.".into(),
        Some(409) => "That address is taken, or can't be changed.".into(),
        Some(400) => "The server would not take that. Check what you wrote.".into(),
        Some(c) if c >= 500 => "The server could not do that right now.".into(),
        _ if e.contains("not signed in") => "Sign in to a server first.".into(),
        _ => format!("Not done: {e}"),
    }
}

fn json<T: serde::de::DeserializeOwned>(body: Result<String, String>) -> Result<T, String> {
    let body = body?;
    serde_json::from_str(&body).map_err(|e| format!("unreadable answer: {e}"))
}

impl Posts {
    /// `ConditionalSend`, not `Send`: on the web the I/O pool is the page's one
    /// thread and the fetch futures are not `Send`.
    fn spawn(
        &mut self,
        f: impl std::future::Future<Output = Done> + bevy::tasks::ConditionalSend + 'static,
    ) {
        self.tasks.push(IoTaskPool::get().spawn(f));
    }

    /// Read a community's public half and its newest posts.
    pub fn refresh(&mut self, id: Uuid) {
        self.spawn(async move {
            Done::Info(
                id,
                json(api_get(&format!("/api/communities/{id}/public")).await),
            )
        });
        self.spawn(async move {
            Done::Feed(
                id,
                json(api_get(&format!("/api/communities/{id}/posts")).await),
            )
        });
    }

    fn open_thread(&mut self, post: Uuid) {
        self.thread_open = Some(post);
        self.thread = None;
        self.reply.clear();
        self.spawn(async move { Done::Thread(json(api_get(&format!("/api/posts/{post}")).await)) });
    }

    /// Start writing a post in `community`, about the camera's view (or the
    /// place "Post this view" brought).
    pub fn compose(&mut self, community: Uuid) {
        let place = self.from_view.take();
        self.want_view = place.is_none();
        self.tab = Tab::Posts;
        self.draft = Some(Draft {
            community,
            text: String::new(),
            place,
            picture: None,
        });
    }

    /// Whether anything is still on its way.
    #[must_use]
    pub fn busy(&self) -> bool {
        !self.tasks.is_empty()
    }

    /// The posts with a place in the community pinned on the map, for
    /// `place_pins`.
    #[must_use]
    pub fn map_posts(&self) -> Vec<(Uuid, Point, String)> {
        let Some(c) = self.on_map else {
            return Vec::new();
        };
        self.feeds
            .get(&c)
            .into_iter()
            .flatten()
            .filter_map(|p| {
                let place = p.place.as_ref()?;
                Some((p.id, point_of(place, &p.text), first_line(&p.text, 28)))
            })
            .collect()
    }

    /// Fold in whatever has arrived.
    pub fn poll(&mut self) {
        let mut done = Vec::new();
        self.tasks
            .retain_mut(|t| match crate::platform::http::poll_task(t) {
                Some(d) => {
                    done.push(d);
                    false
                }
                None => true,
            });
        for d in done {
            match d {
                Done::Info(id, Ok(info)) => {
                    if self.handle.is_empty() {
                        if let Some(h) = &info.settings.handle {
                            self.handle.clone_from(h);
                        }
                    }
                    self.info.insert(id, info);
                }
                Done::Feed(id, Ok(feed)) => {
                    self.feeds.insert(id, feed);
                }
                Done::Thread(Ok(t)) => {
                    if self.thread_open == Some(t.post.id)
                        || t.replies.iter().any(|r| Some(r.id) == self.thread_open)
                    {
                        self.thread_open = Some(t.post.id);
                        self.thread = Some(t);
                    }
                }
                Done::Posted(id, Ok(post)) => {
                    self.sending = false;
                    self.draft = None;
                    self.note = None;
                    self.feeds.entry(id).or_default().insert(0, post);
                }
                Done::Replied(root, Ok(reply)) => {
                    self.sending = false;
                    self.reply.clear();
                    if let Some(t) = self.thread.as_mut().filter(|t| t.post.id == root) {
                        t.replies.push(reply);
                        t.post.replies += 1;
                    }
                }
                Done::Changed(id, Ok(())) => self.refresh(id),
                Done::Info(_, Err(e))
                | Done::Feed(_, Err(e))
                | Done::Thread(Err(e))
                | Done::Posted(_, Err(e))
                | Done::Replied(_, Err(e))
                | Done::Changed(_, Err(e)) => {
                    self.sending = false;
                    self.note = Some(said(&e));
                }
            }
        }
    }
}

/// A post's place as a point, so the map can open it the way it opens one sent
/// in a chat.
#[must_use]
pub fn point_of(place: &PostPlace, text: &str) -> Point {
    Point {
        latitude: place.latitude,
        longitude: place.longitude,
        name: place.name.clone().or_else(|| Some(first_line(text, 40))),
        text: None,
        view: place.view,
        meetup: None,
    }
}

fn first_line(text: &str, max: usize) -> String {
    let line = text
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim();
    if line.chars().count() > max {
        let cut: String = line.chars().take(max.saturating_sub(1)).collect();
        format!("{cut}…")
    } else {
        line.to_string()
    }
}

/// "now", "5 min", "3 h", "2 d", then a date.
fn ago(at_ms: i64, now_ms: i64) -> String {
    let s = (now_ms - at_ms).max(0) / 1000;
    match s {
        0..=59 => "now".into(),
        60..=3599 => format!("{} min", s / 60),
        3600..=86_399 => format!("{} h", s / 3600),
        86_400..=604_799 => format!("{} d", s / 86_400),
        _ => crate::systems::chats::clock_time(u64::try_from(at_ms).unwrap_or(0)),
    }
}

fn now_ms() -> i64 {
    jiff::Timestamp::now().as_millisecond()
}

/// The two halves of a community page, side by side. Answers which is showing.
pub fn tabs(ui: &mut egui::Ui, cs: &mut Communities, id: Uuid, topics: usize) -> Tab {
    if cs.posts.loaded != Some(id) {
        cs.posts.loaded = Some(id);
        cs.posts.thread_open = None;
        cs.posts.thread = None;
        cs.posts.settings_open = false;
        cs.posts.handle.clear();
        cs.posts.refresh(id);
    }
    let mut tab = cs.posts.tab;
    let posts = format!("{}  Posts · public", icons::SHARE);
    let topics = format!("{}  Topics · {topics}", icons::LOCK);
    if theme::segmented(
        ui,
        &mut tab,
        &[(Tab::Posts, &posts), (Tab::Topics, &topics)],
    ) {
        cs.posts.tab = tab;
    }
    tab
}

/// The Posts half of a community page.
pub fn posts_tab(ui: &mut egui::Ui, cs: &mut Communities, c: &CommunityDto, out: &mut Out) {
    let id = c.id;
    let role = c.role.unwrap_or(CommunityRole::Member);
    if let Some(note) = cs.posts.note.clone() {
        ui.horizontal_wrapped(|ui| {
            ui.label(
                egui::RichText::new(note)
                    .color(theme::ERROR)
                    .size(theme::SIZE_SUB),
            );
            if ui.small_button(icons::CLEAR).clicked() {
                cs.posts.note = None;
            }
        });
    }
    if cs.posts.draft.as_ref().is_some_and(|d| d.community == id) {
        composer(ui, cs, c);
        return;
    }
    if let Some(post) = cs.posts.thread_open {
        thread_view(ui, cs, c, post, out);
        return;
    }
    let Some(info) = cs.posts.info.get(&id).cloned() else {
        theme::busy(ui, "Reading the posts…");
        return;
    };
    if !info.settings.enabled {
        theme::note(
            ui,
            "This community has no public posts. With them on, anybody can read what members post here, on the map and on the fediverse (Mastodon, Lemmy and others), and reply.",
        );
        if role.moderates() {
            settings_form(ui, cs, c, &info);
        }
        return;
    }
    ui.label(
        egui::RichText::new(format!(
            "{}  Anyone can read these, on any server · {} follower{}",
            icons::SHARE,
            info.followers,
            if info.followers == 1 { "" } else { "s" }
        ))
        .color(theme::PUBLIC)
        .size(theme::SIZE_SUB),
    );
    if let Some(address) = &info.address {
        ui.label(
            egui::RichText::new(format!("@{address}"))
                .color(theme::TEXT_DIM)
                .size(theme::SIZE_SUB),
        );
    }
    ui.add_space(4.0);
    ui.horizontal_wrapped(|ui| {
        if info.may_post
            && theme::action_button(ui, format!("{} Write a post", icons::RENAME)).clicked()
        {
            cs.posts.compose(id);
        }
        let pins = cs.posts.on_map == Some(id);
        if theme::action_button(
            ui,
            format!(
                "{} {}",
                icons::PIN,
                if pins { "Hide pins" } else { "On the map" }
            ),
        )
        .on_hover_text("Pin this community's posts where they were written")
        .clicked()
        {
            cs.posts.on_map = if pins { None } else { Some(id) };
        }
        if theme::action_button(ui, icons::REFRESH)
            .on_hover_text("Read again")
            .clicked()
        {
            cs.posts.refresh(id);
        }
        if role.moderates()
            && theme::action_button(ui, format!("{} Settings", icons::SETTINGS)).clicked()
        {
            cs.posts.settings_open = !cs.posts.settings_open;
        }
    });
    if cs.posts.settings_open && role.moderates() {
        settings_form(ui, cs, c, &info);
    }
    let feed = cs.posts.feeds.get(&id).cloned().unwrap_or_default();
    if feed.is_empty() {
        theme::empty_state(
            ui,
            "No posts yet",
            if info.may_post {
                "Write the first: it is pinned where you are looking."
            } else {
                "Posts from members and from the fediverse appear here."
            },
        );
    }
    let now = now_ms();
    for p in &feed {
        if post_card(ui, cs, c, p, now, out, true) {
            cs.posts.open_thread(p.id);
        }
        ui.add_space(6.0);
    }
}

/// One post, as a card. Answers whether it was tapped (to open its thread).
fn post_card(
    ui: &mut egui::Ui,
    cs: &mut Communities,
    c: &CommunityDto,
    p: &PostDto,
    now: i64,
    out: &mut Out,
    tappable: bool,
) -> bool {
    let moderates = c.role.is_some_and(CommunityRole::moderates);
    let picture = p
        .picture
        .as_deref()
        .and_then(|h| cs.picture(ui.ctx(), Some(h)));
    let mut tapped = false;
    let frame = raised_frame().show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            avatar(ui, &p.author.name, ChatKind::Private, 32.0);
            ui.vertical(|ui| {
                ui.label(egui::RichText::new(&p.author.name).color(theme::CREAM));
                let mut line = if p.author.local {
                    "member".to_string()
                } else {
                    p.author.address.clone()
                };
                if let Some(via) = &p.via {
                    line.push_str(&format!(" · via {via}"));
                }
                line.push_str(&format!(" · {}", ago(p.created_ms, now)));
                ui.label(
                    egui::RichText::new(line)
                        .size(theme::SIZE_SUB)
                        .color(if p.author.local { theme::TEXT_DIM } else { theme::PUBLIC }),
                );
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let can_act = p.mine || moderates;
                if can_act
                    && ui
                        .add(egui::Button::new(icons::MORE).frame(false))
                        .on_hover_text("Actions")
                        .clicked()
                {
                    cs.posts.menu_for = if cs.posts.menu_for == Some(p.id) {
                        None
                    } else {
                        Some(p.id)
                    };
                }
            });
        });
        ui.add(egui::Label::new(egui::RichText::new(&p.text).color(theme::TEXT)).wrap());
        if let Some(t) = &picture {
            let w = ui.available_width();
            let size = t.size_vec2();
            let h = (w * size.y / size.x.max(1.0)).min(220.0);
            ui.add(
                egui::Image::new(t)
                    .fit_to_exact_size(egui::vec2(w, h))
                    .corner_radius(10.0),
            );
        }
        ui.horizontal_wrapped(|ui| {
            match &p.place {
                Some(place) => {
                    let label = place.name.clone().unwrap_or_else(|| "On the map".into());
                    if theme::action_button(ui, format!("{} {label}", icons::PIN))
                        .on_hover_text("Open where it was written")
                        .clicked()
                    {
                        out.go_to = Some(point_of(place, &p.text));
                        cs.posts.on_map = Some(c.id);
                    }
                }
                None => {
                    ui.label(
                        egui::RichText::new("No place: listed, not pinned")
                            .size(theme::SIZE_SUB)
                            .color(theme::TEXT_DIM),
                    );
                }
            }
            if p.parent.is_none() && tappable {
                let n = p.replies;
                if theme::action_button(
                    ui,
                    format!("{} {n} {}", icons::CHAT, if n == 1 { "reply" } else { "replies" }),
                )
                .clicked()
                {
                    tapped = true;
                }
            }
        });
        if cs.posts.menu_for == Some(p.id) {
            ui.separator();
            ui.horizontal_wrapped(|ui| {
                if p.mine
                    && theme::confirm_button(
                        ui,
                        egui::Id::new(("post_delete", p.id)),
                        &format!("{} Delete", icons::DELETE),
                        "Delete: tap again",
                    )
                {
                    let (post, community) = (p.id, c.id);
                    cs.posts.menu_for = None;
                    cs.posts.spawn(async move {
                        let r = api_delete(&format!("/api/posts/{post}")).await;
                        Done::Changed(community, r)
                    });
                    if cs.posts.thread_open == Some(post) {
                        cs.posts.thread_open = None;
                    }
                }
                if moderates && !p.mine {
                    if theme::action_button(ui, format!("{} Hide from this community", icons::DELETE))
                        .clicked()
                    {
                        let (post, community) = (p.id, c.id);
                        cs.posts.menu_for = None;
                        cs.posts.spawn(async move {
                            let r = api_post(&format!("/api/posts/{post}/hide"), None)
                                .await
                                .map(|_| ());
                            Done::Changed(community, r)
                        });
                    }
                    if !p.author.local
                        && theme::action_button(
                            ui,
                            format!("{} Block {}", icons::FLAG, p.author.address),
                        )
                        .on_hover_text("They can't post here again, and what they wrote here is hidden")
                        .clicked()
                    {
                        let community = c.id;
                        let body = serde_json::json!({ "address": p.author.address }).to_string();
                        cs.posts.menu_for = None;
                        cs.posts.spawn(async move {
                            let r = api_post(&format!("/api/communities/{community}/blocks"), Some(body))
                                .await
                                .map(|_| ());
                            Done::Changed(community, r)
                        });
                    }
                }
            });
            if moderates && !p.author.local {
                theme::note(
                    ui,
                    "A server that sends nothing but trouble is the server admin's to block, for every community at once.",
                );
            }
        }
    });
    if tappable
        && frame
            .response
            .interact(egui::Sense::click())
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .clicked()
    {
        tapped = true;
    }
    tapped
}

fn thread_view(
    ui: &mut egui::Ui,
    cs: &mut Communities,
    c: &CommunityDto,
    post: Uuid,
    out: &mut Out,
) {
    if ui
        .add(egui::Button::new(format!("{}  Posts", icons::CHEVRON_LEFT)).frame(false))
        .clicked()
    {
        cs.posts.thread_open = None;
        cs.posts.thread = None;
        return;
    }
    let Some(t) = cs.posts.thread.clone().filter(|t| t.post.id == post) else {
        theme::busy(ui, "Reading the thread…");
        return;
    };
    let now = now_ms();
    post_card(ui, cs, c, &t.post, now, out, false);
    theme::section(
        ui,
        &format!(
            "{} {}",
            t.replies.len(),
            if t.replies.len() == 1 {
                "reply"
            } else {
                "replies"
            }
        ),
    );
    for r in &t.replies {
        post_card(ui, cs, c, r, now, out, false);
        ui.add_space(4.0);
    }
    ui.add_space(6.0);
    crate::systems::text_touch::field(
        ui,
        egui::TextEdit::multiline(&mut cs.posts.reply)
            .id(egui::Id::new(("post_reply", post)))
            .desired_rows(2)
            .desired_width(ui.available_width())
            .char_limit(MAX_POST_TEXT)
            .hint_text("Reply in public"),
    );
    let ready = !cs.posts.reply.trim().is_empty() && !cs.posts.sending;
    if ui
        .add_enabled(ready, egui::Button::new(format!("{} Reply", icons::SEND)))
        .clicked()
    {
        cs.posts.sending = true;
        let body = serde_json::to_string(&NewReply {
            text: cs.posts.reply.trim().to_string(),
        })
        .unwrap_or_default();
        let root = t.post.id;
        cs.posts.spawn(async move {
            Done::Replied(
                root,
                json(api_post(&format!("/api/posts/{root}/replies"), Some(body)).await),
            )
        });
    }
    theme::note(
        ui,
        "Replies are public, like the post, and reach its author wherever they are.",
    );
}

fn composer(ui: &mut egui::Ui, cs: &mut Communities, c: &CommunityDto) {
    let followers = cs.posts.info.get(&c.id).map_or(0, |i| i.followers);
    let federating = cs.posts.info.get(&c.id).is_some_and(|i| i.federating);
    ui.horizontal(|ui| {
        if ui
            .add(egui::Button::new(icons::CLEAR).frame(false))
            .on_hover_text("Discard")
            .clicked()
        {
            cs.posts.draft = None;
            cs.picture_for_post = false;
        }
        ui.label(
            egui::RichText::new("New post")
                .size(theme::SIZE_TITLE)
                .color(theme::CREAM),
        );
    });
    let Some(draft) = cs.posts.draft.as_mut() else {
        return;
    };
    crate::systems::text_touch::field(
        ui,
        egui::TextEdit::multiline(&mut draft.text)
            .id(egui::Id::new(("post_text", c.id)))
            .desired_rows(4)
            .desired_width(ui.available_width())
            .char_limit(MAX_POST_TEXT)
            .hint_text("What's happening here?"),
    );
    ui.label(
        egui::RichText::new(format!("{} / {MAX_POST_TEXT}", draft.text.chars().count()))
            .size(theme::SIZE_TINY)
            .color(theme::TEXT_DIM),
    );
    let picture = draft.picture.clone();
    let place = draft.place.clone();
    if let Some(t) = picture
        .as_deref()
        .and_then(|h| cs.picture(ui.ctx(), Some(h)))
    {
        let w = ui.available_width();
        let size = t.size_vec2();
        let h = (w * size.y / size.x.max(1.0)).min(180.0);
        ui.add(
            egui::Image::new(&t)
                .fit_to_exact_size(egui::vec2(w, h))
                .corner_radius(10.0),
        );
    }
    raised_frame().show(ui, |ui| {
        ui.set_width(ui.available_width());
        match &place {
            Some(p) => {
                ui.label(
                    egui::RichText::new(format!(
                        "{}  {}",
                        icons::PIN,
                        p.name
                            .clone()
                            .unwrap_or_else(|| format!("{:.5}, {:.5}", p.latitude, p.longitude))
                    ))
                    .color(theme::CREAM),
                );
                theme::note(ui, "The spot you were looking at, and your view of it.");
            }
            None => theme::busy(ui, "Taking the view…"),
        }
        if theme::action_button(ui, format!("{} Use the view now", icons::HERE))
            .on_hover_text("Close the panel, look somewhere else, then come back and press this")
            .clicked()
        {
            cs.posts.want_view = true;
        }
    });
    ui.horizontal_wrapped(|ui| {
        let label = if picture.is_some() {
            "Another picture"
        } else {
            "Add a picture"
        };
        if theme::action_button(ui, format!("{} {label}", icons::CAMERA)).clicked() {
            cs.picture_for_post = true;
            cs.pick_picture = true;
        }
        if picture.is_some() && theme::action_button(ui, "Remove the picture").clicked() {
            if let Some(d) = cs.posts.draft.as_mut() {
                d.picture = None;
            }
        }
    });
    if let Some(note) = &cs.picture_note {
        theme::note(ui, note);
    }
    egui::Frame::NONE
        .fill(theme::PUBLIC.gamma_multiply(0.15))
        .stroke(egui::Stroke::new(1.0, theme::PUBLIC.gamma_multiply(0.5)))
        .corner_radius(10.0)
        .inner_margin(10.0)
        .show(ui, |ui| {
            let reach = if federating {
                format!(
                    "Public. On the community's page and map, and sent to its {followers} follower{} on other servers. Posted under your own address. Deleting it later asks other servers to delete their copy; they may not.",
                    if followers == 1 { "" } else { "s" }
                )
            } else {
                "Public. On the community's page and map, for anybody on this server.".to_string()
            };
            ui.add(egui::Label::new(egui::RichText::new(reach).size(theme::SIZE_SUB).color(theme::TEXT)).wrap());
        });
    let ready = cs
        .posts
        .draft
        .as_ref()
        .is_some_and(|d| !d.text.trim().is_empty() && d.place.is_some())
        && !cs.posts.sending
        && cs.picture_for_post_idle();
    ui.add_space(4.0);
    if ui
        .add_enabled(
            ready,
            egui::Button::new(format!("{} Post", icons::SEND))
                .min_size(egui::vec2(120.0, theme::row_height())),
        )
        .clicked()
    {
        if let Some(d) = cs.posts.draft.clone() {
            if let Some(place) = d.place {
                cs.posts.sending = true;
                let body = serde_json::to_string(&NewPost {
                    text: d.text.trim().to_string(),
                    picture: d.picture,
                    place,
                })
                .unwrap_or_default();
                let community = d.community;
                cs.posts.spawn(async move {
                    Done::Posted(
                        community,
                        json(
                            api_post(&format!("/api/communities/{community}/posts"), Some(body))
                                .await,
                        ),
                    )
                });
            }
        }
    }
}

fn settings_form(ui: &mut egui::Ui, cs: &mut Communities, c: &CommunityDto, info: &PublicInfo) {
    raised_frame().show(ui, |ui| {
        ui.set_width(ui.available_width());
        let id = c.id;
        let first = info.settings.handle.is_none();
        if first {
            if cs.posts.handle.is_empty() {
                cs.posts.handle = suggest_handle(&c.settings.name);
            }
            ui.label(egui::RichText::new("The community's fediverse address").color(theme::CREAM));
            crate::systems::text_touch::field(
                ui,
                egui::TextEdit::singleline(&mut cs.posts.handle)
                    .id(egui::Id::new(("post_handle", id)))
                    .desired_width(ui.available_width())
                    .char_limit(cartopolis_core::posts::MAX_HANDLE),
            );
            theme::note(
                ui,
                &format!(
                    "People elsewhere find it as @{}@{}. It can't be changed later: their servers know the community by it.",
                    cs.posts.handle.trim(),
                    c.home
                ),
            );
        } else if let Some(address) = &info.address {
            ui.label(egui::RichText::new(format!("@{address}")).color(theme::CREAM));
        }
        let mut who = info.settings.who_posts;
        ui.label(egui::RichText::new("Who can post").size(theme::SIZE_SUB).color(theme::TEXT_DIM));
        let changed = theme::segmented(
            ui,
            &mut who,
            &[(WhoPosts::Members, "Every member"), (WhoPosts::Moderators, "Moderators only")],
        );
        let switch = if info.settings.enabled {
            theme::action_button(ui, "Switch public posts off").clicked().then_some(false)
        } else {
            theme::action_button(ui, format!("{} Switch public posts on", icons::SHARE))
                .clicked()
                .then_some(true)
        };
        if changed || switch.is_some() {
            let settings = PublicSettings {
                enabled: switch.unwrap_or(info.settings.enabled),
                handle: if first {
                    Some(cs.posts.handle.trim().to_string())
                } else {
                    None
                },
                who_posts: who,
            };
            let body = serde_json::to_string(&settings).unwrap_or_default();
            cs.posts.spawn(async move {
                let r = api_post(&format!("/api/communities/{id}/public"), Some(body))
                    .await
                    .map(|_| ());
                Done::Changed(id, r)
            });
        }
        if !info.settings.enabled {
            return;
        }
        ui.separator();
        ui.label(egui::RichText::new("This community follows").size(theme::SIZE_SUB).color(theme::TEXT_DIM));
        if !info.federating {
            theme::note(ui, "This server does not talk to other servers, so it follows nothing.");
            return;
        }
        if info.follows.is_empty() {
            theme::note(ui, "Nothing yet. Their posts would appear here, marked with where they came from.");
        }
        for f in &info.follows {
            ui.horizontal(|ui| {
                let kind = match f.kind {
                    SourceKind::Community => "community",
                    SourceKind::Account => "account",
                };
                let state = if f.accepted { "" } else { " · waiting for their server" };
                ui.label(egui::RichText::new(format!("{}  {}", icons::SHARE, f.address)).color(theme::PUBLIC));
                ui.label(egui::RichText::new(format!("{kind}{state}")).size(theme::SIZE_SUB).color(theme::TEXT_DIM));
                if ui.small_button(icons::CLEAR).on_hover_text("Stop following").clicked() {
                    let address = f.address.clone();
                    cs.posts.spawn(async move {
                        let r = api_delete(&format!("/api/communities/{id}/follows/{address}")).await;
                        Done::Changed(id, r)
                    });
                }
            });
        }
        crate::systems::text_touch::field(
            ui,
            egui::TextEdit::singleline(&mut cs.posts.follow)
                .id(egui::Id::new(("post_follow", id)))
                .desired_width(ui.available_width())
                .hint_text("groningen@feddit.nl or someone@mastodon.social"),
        );
        if theme::action_button(ui, format!("{} Follow", icons::ADD)).clicked()
            && cs.posts.follow.contains('@')
        {
            let body = serde_json::json!({ "address": cs.posts.follow.trim() }).to_string();
            cs.posts.follow.clear();
            cs.posts.spawn(async move {
                let r = api_post(&format!("/api/communities/{id}/follows"), Some(body))
                    .await
                    .map(|_| ());
                Done::Changed(id, r)
            });
        }
    });
}

/// "Post this view": the communities this account is in, to choose one.
pub fn pick_community(ui: &mut egui::Ui, cs: &mut Communities) {
    ui.label(
        egui::RichText::new("Post this view publicly, to")
            .size(theme::SIZE_TITLE)
            .color(theme::CREAM),
    );
    if let Some(p) = &cs.posts.from_view {
        ui.label(
            egui::RichText::new(format!(
                "{}  {}",
                icons::PIN,
                p.name
                    .clone()
                    .unwrap_or_else(|| format!("{:.5}, {:.5}", p.latitude, p.longitude))
            ))
            .color(theme::TEXT_DIM),
        );
    }
    let mine: Vec<(Uuid, String)> = cs
        .list
        .iter()
        .filter(|c| c.role.is_some())
        .map(|c| (c.id, c.settings.name.clone()))
        .collect();
    if mine.is_empty() {
        theme::note(
            ui,
            "You're not in a community yet. Find one, or start one, first.",
        );
    }
    for (id, name) in mine {
        if theme::action_button(ui, format!("{}  {name}", icons::GROUP)).clicked() {
            cs.posts.compose(id);
            cs.show(id);
        }
    }
    if theme::action_button(ui, "Cancel").clicked() {
        cs.posts.from_view = None;
        cs.screen = None;
    }
}

/// Answer the composer's "where am I looking" with the camera's view, as
/// `geo_nav::answer_view_requests` does for a point sent in a chat.
pub fn answer_post_view(
    mut cs: ResMut<Communities>,
    nav: Res<crate::systems::geo_nav::GeoNavState>,
    globe: Res<crate::systems::globe::GlobeState>,
    camera: crate::grid::AnchoredCamera,
    map_anchor: Res<crate::resources::MapAnchor>,
) {
    if cs.posts.busy() {
        cs.posts.poll();
    }
    if !cs.posts.want_view {
        return;
    }
    let (Some((cell, tf)), Some(anchor)) = (camera.get(), map_anchor.geo) else {
        return;
    };
    cs.posts.want_view = false;
    let place = view_place(&nav, &globe, anchor, cell, tf);
    if let Some(d) = cs.posts.draft.as_mut() {
        d.place = Some(place);
    }
}

/// The camera's view as a post's place.
pub fn view_place(
    nav: &crate::systems::geo_nav::GeoNavState,
    globe: &crate::systems::globe::GlobeState,
    anchor: (f64, f64, u32),
    cell: &big_space::prelude::GridCell,
    tf: &Transform,
) -> PostPlace {
    let (lat, lng) = crate::systems::geo_nav::camera_geo(anchor, globe, cell, tf);
    let (yaw, pitch, _) = tf.rotation.to_euler(EulerRot::YXZ);
    PostPlace {
        latitude: lat,
        longitude: lng,
        name: nav
            .history
            .get(nav.history_pos)
            .and_then(|e| e.label.clone()),
        view: Some(PointView {
            altitude: globe.altitude_m(cell, tf),
            pitch: pitch.to_degrees(),
            yaw: yaw.to_degrees(),
        }),
    }
}

/// Seed the public half for a scripted capture (`--ui community`,
/// `communitypost`, `communitymap`, `communitytopics`): every scripted run is
/// offline, and these screens need a server to draw anything at all.
pub fn seed_demo(posts: &mut Posts, community: Uuid, screen: &str) {
    use cartopolis_core::posts::{FollowDto, PostAuthor};
    *posts = Posts::default();
    posts.loaded = Some(community);
    posts.tab = if screen == "communitytopics" {
        Tab::Topics
    } else {
        Tab::Posts
    };
    posts.info.insert(
        community,
        PublicInfo {
            settings: PublicSettings {
                enabled: true,
                handle: Some("garage44".into()),
                who_posts: WhoPosts::Members,
            },
            address: Some("garage44@cartopolis.org".into()),
            followers: 312,
            follows: vec![FollowDto {
                address: "groningen@feddit.nl".into(),
                name: "Groningen".into(),
                kind: SourceKind::Community,
                accepted: true,
            }],
            may_post: true,
            may_moderate: true,
            federating: true,
        },
    );
    let now = now_ms();
    let author = |name: &str, address: &str, local: bool| PostAuthor {
        name: name.into(),
        address: address.into(),
        local,
    };
    let view = |pitch: f32, yaw: f32| {
        Some(PointView {
            altitude: 180.0,
            pitch,
            yaw,
        })
    };
    let post =
        |text: &str, who: PostAuthor, place: Option<PostPlace>, ago_min: i64, replies: u32| {
            PostDto {
                id: Uuid::new_v4(),
                community,
                parent: None,
                author: who,
                via: None,
                text: text.into(),
                picture: None,
                place,
                url: None,
                created_ms: now - ago_min * 60_000,
                edited_ms: None,
                replies,
                mine: false,
            }
        };
    let mut feed = vec![
        post(
            "Plant swap on the corner this Saturday, 10 to 1. Bring cuttings, take cuttings 🌱",
            author("sanne", "sanne@cartopolis.org", true),
            Some(PostPlace {
                latitude: 53.2112,
                longitude: 6.5735,
                name: Some("Meeuwerderweg".into()),
                view: view(-35.0, 20.0),
            }),
            25,
            6,
        ),
        post(
            "Heron back on the Oosterhaven quay this morning @garage44@cartopolis.org",
            author("Anna", "anna@mastodon.nl", false),
            Some(PostPlace {
                latitude: 53.2163,
                longitude: 6.5788,
                name: Some("Oosterhaven".into()),
                view: view(-30.0, 120.0),
            }),
            60,
            2,
        ),
        post(
            "Gemeente plans bike parking at the Oosterpoort",
            author("dirk", "dirk@feddit.nl", false),
            None,
            180,
            11,
        ),
    ];
    feed[2].via = Some("groningen@feddit.nl".into());
    posts.feeds.insert(community, feed);
    if screen == "communitymap" {
        posts.on_map = Some(community);
    }
    if screen == "communitypost" {
        posts.draft = Some(Draft {
            community,
            text: "Street party on the Grote Markt tonight, from 8. Bring a chair!".into(),
            place: Some(PostPlace {
                latitude: 53.2194,
                longitude: 6.5665,
                name: Some("Grote Markt, Groningen".into()),
                view: view(-30.0, 0.0),
            }),
            picture: None,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failure_reads_as_a_sentence() {
        assert_eq!(
            said("https://x/api/posts: status code 403"),
            "You may not do that here."
        );
        assert!(said("not signed in").starts_with("Sign in"));
    }

    #[test]
    fn times_read_like_a_feed() {
        assert_eq!(ago(1_000, 30_000), "now");
        assert_eq!(ago(0, 5 * 60_000), "5 min");
        assert_eq!(ago(0, 3 * 3_600_000), "3 h");
        assert_eq!(ago(0, 2 * 86_400_000), "2 d");
    }

    #[test]
    fn a_pin_label_is_the_first_line_cut_short() {
        assert_eq!(first_line("\nPlant swap\nmore", 28), "Plant swap");
        assert_eq!(first_line("abcdef", 4), "abc…");
    }

    #[test]
    fn only_the_pinned_communitys_placed_posts_are_on_the_map() {
        let c = Uuid::new_v4();
        let mut posts = Posts::default();
        let place = PostPlace {
            latitude: 53.2,
            longitude: 6.5,
            name: None,
            view: None,
        };
        let post = |place: Option<PostPlace>| PostDto {
            id: Uuid::new_v4(),
            community: c,
            parent: None,
            author: cartopolis_core::posts::PostAuthor {
                name: "sanne".into(),
                address: "sanne@x".into(),
                local: true,
            },
            via: None,
            text: "Plant swap".into(),
            picture: None,
            place,
            url: None,
            created_ms: 0,
            edited_ms: None,
            replies: 0,
            mine: false,
        };
        posts.feeds.insert(c, vec![post(Some(place)), post(None)]);
        assert!(posts.map_posts().is_empty());
        posts.on_map = Some(c);
        let pins = posts.map_posts();
        assert_eq!(pins.len(), 1);
        assert_eq!(pins[0].1.name.as_deref(), Some("Plant swap"));
    }
}
