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
    suggest_handle, EditPost, NewPost, NewReply, PostDto, PostPlace, PostReport, PostThread,
    PublicInfo, PublicSettings, ReportAction, SourceKind, WhoPosts, MAX_POST_TEXT,
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
    /// The snapshot of the world, also in the picture store
    /// (`utils::world_polaroid`).
    pub snapshot: Option<String>,
}

enum Done {
    /// A polaroid was framed and stored: its hash.
    Snapshot(Result<String, String>),
    Info(Uuid, Result<PublicInfo, String>),
    Feed(Uuid, Result<Vec<PostDto>, String>),
    Thread(Result<PostThread, String>),
    Posted(Uuid, Result<PostDto, String>),
    Replied(Uuid, Result<PostDto, String>),
    /// Something changed in a community: read it again.
    Changed(Uuid, Result<(), String>),
    /// A community's reports from other servers, for its moderators.
    Reports(Uuid, Result<Vec<PostReport>, String>),
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
    /// Only this community's posts are on the map, whatever the layer says:
    /// picking a community in the list goes to the World with this set, and the
    /// chip under the switch clears it (`render_filter_chip`).
    pub only: Option<Uuid>,
    /// The community whose page last asked for its posts.
    loaded: Option<Uuid>,
    sending: bool,
    settings_open: bool,
    /// The post whose actions are open.
    menu_for: Option<Uuid>,
    /// The post or reply being edited, and its words so far.
    editing: Option<(Uuid, String)>,
    /// Reports from other servers, per community, while there are any.
    reports: HashMap<Uuid, Vec<PostReport>>,
    /// Layers ▸ Community posts: every community's posts on the map, not only
    /// the one whose page asked. Copied from `DataLayers` each frame.
    pub layer: bool,
    /// When every community's posts were last read for the map.
    swept: Option<bevy::platform::time::Instant>,
    /// How many communities that read covered, so joining one reads again.
    swept_count: usize,
    /// The composer asked for a snapshot of the view: the next frame is
    /// captured without the interface (`ui::drive_screenshots`).
    pub want_snapshot: bool,
    /// That capture, as RGBA and its size, until it is framed as a polaroid.
    pub snapshot_in: Option<(Vec<u8>, [u32; 2])>,
    /// A polaroid is being captured, framed or stored.
    pub snapshot_busy: bool,
    /// The composer asked for the camera (`camera_screen::hand_picture_to_post`
    /// opens it, and hands the picture back).
    pub want_camera: bool,
    /// A picture from the camera waiting for a draft to hold it, then uploaded
    /// as the post's picture.
    pub pending_picture: Option<Vec<u8>>,
    /// The draft is written over the map (`render_floating_composer`), not in
    /// the panel: it started from the map (Post here, the camera), and the
    /// place it is about should stay in sight.
    pub over_map: bool,
}

/// How often the map's posts are read again while the layer is on: a post
/// written elsewhere shows up within this, and a quiet community costs one
/// small request a minute.
const SWEEP_SECS: u64 = 60;

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
            snapshot: None,
        });
    }

    /// Whether anything is still on its way.
    #[must_use]
    pub fn busy(&self) -> bool {
        !self.tasks.is_empty()
    }

    /// The posts with a place to pin, for `place_pins`: every community's while
    /// the layer is on, else the one community whose page asked. Each is
    /// (community, post, where, label).
    #[must_use]
    pub fn map_posts(&self) -> Vec<(Uuid, Uuid, Point, String)> {
        let mut out = Vec::new();
        for (c, feed) in &self.feeds {
            let shown = match self.only {
                Some(only) => only == *c,
                None => self.layer || self.on_map == Some(*c),
            };
            if !shown {
                continue;
            }
            for p in feed {
                if let Some(place) = &p.place {
                    out.push((*c, p.id, point_of(place, &p.text), first_line(&p.text, 28)));
                }
            }
        }
        // A stable order, so a pin keeps its id (and egui its area) from one
        // frame to the next whatever the map's iteration order.
        out.sort_by_key(|(c, p, _, _)| (*c, *p));
        out
    }

    /// The community a post goes to without asking: the one whose posts
    /// alone are on the map, else the one whose page last put them there, if
    /// this account is in it.
    #[must_use]
    pub fn community_to_post_in(&self, list: &[CommunityDto]) -> Option<Uuid> {
        self.only
            .or(self.on_map)
            .filter(|id| list.iter().any(|c| c.id == *id && c.role.is_some()))
    }

    /// Show only `community`'s posts on the map, and read them.
    pub fn show_only(&mut self, community: Uuid) {
        self.only = Some(community);
        self.on_map = Some(community);
        self.refresh(community);
    }

    /// How many of `community`'s posts have a place to pin.
    #[must_use]
    pub fn pinned(&self, community: Uuid) -> usize {
        self.feeds
            .get(&community)
            .map_or(0, |f| f.iter().filter(|p| p.place.is_some()).count())
    }

    /// A pin was tapped: have its post open on the community's page, for when
    /// the panel is next opened.
    pub fn focus(&mut self, community: Uuid, post: Uuid) {
        self.tab = Tab::Posts;
        self.loaded = Some(community);
        self.open_thread(post);
    }

    /// A capture of the view arrived: frame it as a polaroid of the draft's
    /// place and store it, off the frame.
    fn make_snapshot(&mut self, rgba: Vec<u8>, size: [u32; 2], community_name: &str) {
        let place = self
            .draft
            .as_ref()
            .and_then(|d| d.place.as_ref())
            .and_then(|p| p.name.clone())
            .unwrap_or_else(|| community_name.to_string());
        let date = jiff::Zoned::now().strftime("%-d %B %Y").to_string();
        self.spawn(async move {
            let made =
                crate::utils::world_polaroid::compose(&rgba, size[0], size[1], &place, &date);
            let stored = match made {
                Ok(jpeg) => crate::systems::model_assets::api_upload_picture(jpeg).await,
                Err(e) => Err(e),
            };
            Done::Snapshot(stored)
        });
    }

    /// Read every community's posts again when the layer is on and it is time.
    fn sweep(&mut self, communities: &[Uuid]) {
        let due = self
            .swept
            .is_none_or(|t| t.elapsed().as_secs() >= SWEEP_SECS)
            || self.swept_count != communities.len();
        if !self.layer || !due {
            return;
        }
        self.swept = Some(bevy::platform::time::Instant::now());
        self.swept_count = communities.len();
        for &id in communities {
            self.spawn(async move {
                Done::Feed(
                    id,
                    json(api_get(&format!("/api/communities/{id}/posts")).await),
                )
            });
        }
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
                    if info.open_reports > 0 {
                        self.spawn(async move {
                            Done::Reports(
                                id,
                                json(api_get(&format!("/api/communities/{id}/reports")).await),
                            )
                        });
                    } else {
                        self.reports.remove(&id);
                    }
                    self.info.insert(id, info);
                }
                Done::Reports(id, Ok(reports)) => {
                    self.reports.insert(id, reports);
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
                Done::Changed(id, Ok(())) => {
                    self.refresh(id);
                    // A thread open on what changed shows it changed.
                    if let Some(post) = self.thread_open {
                        self.open_thread(post);
                    }
                }
                Done::Snapshot(Ok(hash)) => {
                    self.snapshot_busy = false;
                    if let Some(d) = self.draft.as_mut() {
                        d.snapshot = Some(hash);
                    }
                }
                Done::Snapshot(Err(e)) => {
                    self.snapshot_busy = false;
                    self.note = Some(format!("The polaroid did not work: {e}"));
                }
                Done::Info(_, Err(e))
                | Done::Feed(_, Err(e))
                | Done::Thread(Err(e))
                | Done::Posted(_, Err(e))
                | Done::Replied(_, Err(e))
                | Done::Reports(_, Err(e))
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
    ui.add_space(6.0);
    if info.may_post {
        let width = ui.available_width();
        if theme::primary_button_sized(ui, &format!("{}  Write a post", icons::RENAME), width)
            .clicked()
        {
            cs.posts.compose(id);
        }
        ui.add_space(4.0);
    }
    ui.horizontal(|ui| {
        let side = theme::PRIMARY_BUTTON_HEIGHT.max(theme::row_height());
        let squares = if role.moderates() { 2.0 } else { 1.0 };
        let gap = ui.spacing().item_spacing.x;
        let wide = (ui.available_width() - squares * (side + gap)).floor();
        let pins = cs.posts.on_map == Some(id);
        if theme::secondary_toggle(
            ui,
            icons::PIN,
            if pins {
                "Pinned on the map"
            } else {
                "Show on the map"
            },
            wide,
            pins,
        )
        .on_hover_text("Pin this community's posts where they were written")
        .clicked()
        {
            cs.posts.on_map = if pins { None } else { Some(id) };
        }
        if theme::secondary_button(ui, icons::REFRESH, "", side)
            .on_hover_text("Read again")
            .clicked()
        {
            cs.posts.refresh(id);
        }
        if role.moderates()
            && theme::secondary_toggle(ui, icons::SETTINGS, "", side, cs.posts.settings_open)
                .on_hover_text("Settings")
                .clicked()
        {
            cs.posts.settings_open = !cs.posts.settings_open;
        }
    });
    ui.add_space(4.0);
    if cs.posts.settings_open && role.moderates() {
        settings_form(ui, cs, c, &info);
    }
    if role.moderates() {
        reports_list(ui, cs, id);
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

/// Reports from other servers waiting for a moderator: each post with who
/// reported it and why, to hide or to leave.
fn reports_list(ui: &mut egui::Ui, cs: &mut Communities, id: Uuid) {
    let Some(reports) = cs.posts.reports.get(&id).cloned().filter(|r| !r.is_empty()) else {
        return;
    };
    theme::section(
        ui,
        &format!(
            "{}  {} report{} from other servers",
            icons::FLAG,
            reports.len(),
            if reports.len() == 1 { "" } else { "s" }
        ),
    );
    for r in &reports {
        raised_frame().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(
                egui::RichText::new(format!("{} · {}", r.reporter, ago(r.created_ms, now_ms())))
                    .size(theme::SIZE_SUB)
                    .color(theme::PUBLIC),
            );
            if !r.reason.is_empty() {
                ui.add(egui::Label::new(egui::RichText::new(&r.reason).color(theme::CREAM)).wrap());
            }
            ui.label(
                egui::RichText::new(format!(
                    "{}: {}",
                    r.post.author.address,
                    first_line(&r.post.text, 120)
                ))
                .size(theme::SIZE_SUB)
                .color(theme::TEXT_DIM),
            );
            ui.horizontal_wrapped(|ui| {
                for (action, label) in [
                    (
                        ReportAction::Hide,
                        format!("{} Hide the post", icons::DELETE),
                    ),
                    (ReportAction::Dismiss, "Leave it".to_string()),
                ] {
                    if theme::action_button(ui, label).clicked() {
                        let report = r.id;
                        let body = serde_json::json!({ "action": action }).to_string();
                        cs.posts.spawn(async move {
                            let r = api_post(
                                &format!("/api/communities/{id}/reports/{report}"),
                                Some(body),
                            )
                            .await
                            .map(|_| ());
                            Done::Changed(id, r)
                        });
                    }
                }
            });
        });
        ui.add_space(4.0);
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
    let snapshot = p
        .snapshot
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
                if p.edited_ms.is_some() {
                    line.push_str(" · edited");
                }
                ui.label(
                    egui::RichText::new(line)
                        .size(theme::SIZE_SUB)
                        .color(if p.author.local { theme::TEXT_DIM } else { theme::PUBLIC }),
                );
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let can_act = p.mine || moderates;
                if can_act && theme::kebab_button(ui).clicked()
                {
                    cs.posts.menu_for = if cs.posts.menu_for == Some(p.id) {
                        None
                    } else {
                        Some(p.id)
                    };
                }
            });
        });
        if cs.posts.editing.as_ref().is_some_and(|(e, _)| *e == p.id) {
            edit_box(ui, cs, c.id, p.id);
        } else {
            ui.add(egui::Label::new(egui::RichText::new(&p.text).color(theme::TEXT)).wrap());
        }
        // The polaroid keeps its own proportions: its frame is
        // part of the picture, and a crop would cut them off.
        if let Some(t) = &snapshot {
            let w = ui.available_width().min(420.0);
            let size = t.size_vec2();
            ui.add(
                egui::Image::new(t).fit_to_exact_size(egui::vec2(w, w * size.y / size.x.max(1.0))),
            );
        }
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
                    if theme::chip(ui, &format!("{}  {label}", icons::PIN), false)
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
            // Votes from other servers: Mastodon's favourites, Lemmy's votes.
            let votes = match (p.likes, p.dislikes) {
                (0, 0) => None,
                (l, 0) => Some(format!("{l} like{}", if l == 1 { "" } else { "s" })),
                (l, d) => Some(format!("{l} up · {d} down")),
            };
            if let Some(v) = votes {
                ui.label(egui::RichText::new(v).size(theme::SIZE_SUB).color(theme::TEXT_DIM));
            }
            if p.parent.is_none() && tappable {
                let n = p.replies;
                if theme::chip(
                    ui,
                    &format!("{}  {n} {}", icons::CHAT, if n == 1 { "reply" } else { "replies" }),
                    false,
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
                if p.mine && theme::action_button(ui, format!("{} Edit", icons::RENAME)).clicked() {
                    cs.posts.editing = Some((p.id, p.text.clone()));
                    cs.posts.menu_for = None;
                }
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

/// The words of a post being edited, with Save and Cancel. A change reaches
/// other servers as an edit, as Mastodon's own do.
fn edit_box(ui: &mut egui::Ui, cs: &mut Communities, community: Uuid, post: Uuid) {
    let Some((_, text)) = cs.posts.editing.as_mut() else {
        return;
    };
    crate::systems::text_touch::field(
        ui,
        egui::TextEdit::multiline(text)
            .id(egui::Id::new(("post_edit", post)))
            .desired_rows(3)
            .desired_width(ui.available_width())
            .char_limit(MAX_POST_TEXT),
    );
    let ready = !text.trim().is_empty();
    let body = serde_json::to_string(&EditPost {
        text: text.trim().to_string(),
    })
    .unwrap_or_default();
    ui.horizontal(|ui| {
        if ui
            .add_enabled_ui(ready, |ui| {
                theme::action_button(ui, format!("{} Save", icons::RENAME))
            })
            .inner
            .clicked()
        {
            cs.posts.editing = None;
            // POST, the edit's alias: the app's HTTP layer has no PUT.
            cs.posts.spawn(async move {
                let r = api_post(&format!("/api/posts/{post}"), Some(body))
                    .await
                    .map(|_| ());
                Done::Changed(community, r)
            });
        }
        if theme::action_button(ui, "Cancel").clicked() {
            cs.posts.editing = None;
        }
    });
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
    let width = ui.available_width();
    if ui
        .add_enabled_ui(ready, |ui| {
            theme::primary_button_sized(ui, &format!("{}  Reply", icons::SEND), width)
        })
        .inner
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
        ui.label(
            egui::RichText::new("New post")
                .size(theme::SIZE_TITLE)
                .color(theme::CREAM),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if theme::close_button(ui).on_hover_text("Discard").clicked() {
                cs.posts.draft = None;
                cs.picture_for_post = false;
            }
        });
    });
    let Some(draft) = cs.posts.draft.as_mut() else {
        return;
    };
    ui.add_space(4.0);
    crate::systems::text_touch::field(
        ui,
        egui::TextEdit::multiline(&mut draft.text)
            .id(egui::Id::new(("post_text", c.id)))
            .desired_rows(4)
            .desired_width(ui.available_width())
            .char_limit(MAX_POST_TEXT)
            .hint_text("What's happening here?"),
    );
    let used = draft.text.chars().count();
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
        ui.label(
            egui::RichText::new(format!("{used} / {MAX_POST_TEXT}"))
                .size(theme::SIZE_TINY)
                .color(if used + 20 >= MAX_POST_TEXT {
                    theme::WARN
                } else {
                    theme::TEXT_DIM
                }),
        );
    });
    let picture = draft.picture.clone();
    let snapshot = draft.snapshot.clone();
    let place = draft.place.clone();

    // Where: one line, and the one thing to do about it.
    raised_frame().show(ui, |ui| {
        ui.set_width(ui.available_width());
        row(
            ui,
            40.0,
            egui::Layout::right_to_left(egui::Align::Center),
            |ui| {
                if theme::secondary_button(ui, icons::HERE, "Use view", 0.0)
                    .on_hover_text("Close the panel, look somewhere else, come back and press this")
                    .clicked()
                {
                    cs.posts.want_view = true;
                }
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    ui.label(
                        egui::RichText::new(icons::PIN)
                            .size(20.0)
                            .color(theme::ACCENT),
                    );
                    ui.vertical(|ui| match &place {
                        Some(p) => {
                            let name = p.name.clone().unwrap_or_else(|| {
                                format!("{:.5}, {:.5}", p.latitude, p.longitude)
                            });
                            ui.add(
                                egui::Label::new(egui::RichText::new(name).color(theme::CREAM))
                                    .truncate(),
                            );
                            ui.label(
                                egui::RichText::new("Pinned where you are looking")
                                    .size(theme::SIZE_SUB)
                                    .color(theme::TEXT_DIM),
                            );
                        }
                        None => theme::busy(ui, "Taking the view…"),
                    });
                });
            },
        );
    });

    // What goes with it: each attachment as it will look, with its own remove.
    // The snapshot first, as it goes out first.
    if let Some(t) = snapshot
        .as_deref()
        .and_then(|h| cs.picture(ui.ctx(), Some(h)))
    {
        let w = ui.available_width().min(300.0);
        let size = t.size_vec2();
        let shown = ui.add(
            egui::Image::new(&t).fit_to_exact_size(egui::vec2(w, w * size.y / size.x.max(1.0))),
        );
        if remove_badge(ui, shown.rect, "Remove the polaroid") {
            if let Some(d) = cs.posts.draft.as_mut() {
                d.snapshot = None;
            }
        }
    }
    if let Some(t) = picture
        .as_deref()
        .and_then(|h| cs.picture(ui.ctx(), Some(h)))
    {
        let w = ui.available_width();
        let size = t.size_vec2();
        let h = (w * size.y / size.x.max(1.0)).min(180.0);
        let shown = ui.add(
            egui::Image::new(&t)
                .fit_to_exact_size(egui::vec2(w, h))
                .corner_radius(10.0),
        );
        if remove_badge(ui, shown.rect, "Remove the picture") {
            if let Some(d) = cs.posts.draft.as_mut() {
                d.picture = None;
            }
        }
    }
    ui.add_space(6.0);
    // Camera and Upload, the same two round buttons wherever a picture can be
    // added (`docs/mockups/social-camera/`). The camera's In world mode is
    // the polaroid this row used to have a button of its own for.
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 18.0;
        let busy = !cs.picture_for_post_idle() || cs.posts.snapshot_busy;
        for (icon, label, hint) in [
            (icons::CAMERA, "Camera", "Take a picture for this post"),
            (icons::UPLOAD, "Upload", "A picture from this device"),
        ] {
            ui.vertical_centered(|ui| {
                ui.set_width(theme::ROUND_BUTTON_SIZE + 24.0);
                if theme::round_button(ui, icon, hint, false).clicked() && !busy {
                    if label == "Camera" {
                        cs.posts.want_camera = true;
                    } else {
                        cs.picture_for_post = true;
                        cs.pick_picture = true;
                    }
                }
                ui.label(egui::RichText::new(label).size(12.0).color(theme::TEXT));
            });
        }
        if picture.is_some() {
            ui.label(
                egui::RichText::new("Another replaces the picture.")
                    .size(theme::SIZE_SUB)
                    .color(theme::TEXT_DIM),
            );
        }
    });
    if let Some(note) = &cs.picture_note {
        theme::note(ui, note);
    }
    ui.add_space(6.0);
    let reach = if federating {
        format!(
            "{}  Public: on the community's page and map, and sent to {followers} follower{} on other servers, under your own address.",
            icons::SHARE,
            if followers == 1 { "" } else { "s" }
        )
    } else {
        format!(
            "{}  Public: on the community's page and map, for anybody on this server.",
            icons::SHARE
        )
    };
    ui.add(
        egui::Label::new(
            egui::RichText::new(reach)
                .size(theme::SIZE_SUB)
                .color(theme::PUBLIC),
        )
        .wrap(),
    );
    let ready = cs
        .posts
        .draft
        .as_ref()
        .is_some_and(|d| !d.text.trim().is_empty() && d.place.is_some())
        && !cs.posts.sending
        && cs.picture_for_post_idle()
        && !cs.posts.snapshot_busy;
    ui.add_space(6.0);
    let label = if cs.posts.sending {
        "Posting…".to_string()
    } else {
        format!("{}  Post", icons::SEND)
    };
    let width = ui.available_width();
    if ui
        .add_enabled_ui(ready, |ui| theme::primary_button_sized(ui, &label, width))
        .inner
        .clicked()
    {
        if let Some(d) = cs.posts.draft.clone() {
            if let Some(place) = d.place {
                cs.posts.sending = true;
                let body = serde_json::to_string(&NewPost {
                    text: d.text.trim().to_string(),
                    picture: d.picture,
                    snapshot: d.snapshot,
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
    if cs
        .posts
        .draft
        .as_ref()
        .is_some_and(|d| d.text.trim().is_empty())
    {
        ui.vertical_centered(|ui| {
            ui.label(
                egui::RichText::new("Write something to post")
                    .size(theme::SIZE_TINY)
                    .color(theme::TEXT_DIM),
            );
        });
    }
}

/// A row of a stated height, its contents centred on it. `ui.horizontal`
/// centres on the row's *starting* height, which is shorter than a button, so
/// a word beside one sat level with its top.
fn row<R>(
    ui: &mut egui::Ui,
    height: f32,
    layout: egui::Layout,
    add: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    ui.allocate_ui_with_layout(egui::vec2(ui.available_width(), height), layout, add)
        .inner
}

/// A small round ✕ over the top-right corner of an attachment's preview.
/// Answers whether it was pressed.
fn remove_badge(ui: &mut egui::Ui, over: egui::Rect, tooltip: &str) -> bool {
    let side = theme::ROUND_BUTTON_SIZE;
    let rect = egui::Rect::from_min_size(
        over.right_top() + egui::vec2(-side - 6.0, 6.0),
        egui::Vec2::splat(side),
    );
    ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
        theme::round_button(ui, icons::CLEAR, tooltip, false).clicked()
    })
    .inner
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
                .hint_text("groningen@feddit.nl, someone@mastodon.social, or a relay's address"),
        );
        let typed = cs.posts.follow.trim();
        if theme::action_button(ui, format!("{} Follow", icons::ADD)).clicked()
            && (typed.contains('@') || typed.starts_with("https://"))
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
        cs.posts.pending_picture = None;
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
    layers: Res<crate::systems::data_layers::DataLayers>,
    online: Option<Res<crate::resources::OnlineSession>>,
    mut shot: ResMut<crate::resources::ScreenshotState>,
) {
    if cs.posts.busy() {
        cs.posts.poll();
    }
    // "Polaroid of this view": capture the next frame without the interface,
    // the way a chat's "Send a snapshot" does, then draw it off the frame.
    if std::mem::take(&mut cs.posts.want_snapshot) {
        cs.posts.snapshot_busy = true;
        shot.post_snapshot = true;
    }
    if let Some((rgba, size)) = cs.posts.snapshot_in.take() {
        let name = cs
            .posts
            .draft
            .as_ref()
            .and_then(|d| cs.get(d.community))
            .map(|c| c.settings.name.clone())
            .unwrap_or_default();
        cs.posts.make_snapshot(rgba, size, &name);
    }
    // Layers ▸ Community posts: keep every community's posts current for the
    // map. Nothing to read without a server, and a scripted capture's demo
    // feeds are not to be replaced.
    cs.posts.layer = layers.community_posts_enabled;
    if online.is_some() {
        let mine: Vec<Uuid> = cs
            .list
            .iter()
            .filter(|c| c.role.is_some())
            .map(|c| c.id)
            .collect();
        cs.posts.sweep(&mine);
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

/// The chip under the Community | World switch while only one community's
/// posts are on the map (`Posts::only`): whose they are and how many, the
/// community's page a tap on its name, and ✕ to show every community's again.
///
/// Nothing of the community's chat is on the map with it (2026-10-18, the
/// user: "we don't need the chat prominent on the screen by default"). The
/// dock's chat disc and this chip's name are the two ways in.
#[allow(clippy::too_many_arguments)]
pub fn render_filter_chip(
    mut egui: crate::systems::egui_manager::EguiMut,
    mut cs: ResMut<Communities>,
    mut right_panel: ResMut<crate::resources::RightPanel>,
    chat: Res<crate::systems::chat::ChatState>,
    driver: Res<crate::systems::driver::DriverState>,
    camera: Res<crate::systems::camera_screen::CameraScreen>,
    online: Option<Res<crate::resources::OnlineSession>>,
) {
    let Some(id) = cs.posts.only else {
        return;
    };
    if online.is_none() && !cs.is_demo() {
        cs.posts.only = None;
        return;
    }
    if driver.active
        || camera.is_open()
        || chat.world.is_some()
        || !right_panel.is(crate::resources::RightPanel::None)
    {
        return;
    }
    let Some(name) = cs.get(id).map(|c| c.settings.name.clone()) else {
        cs.posts.only = None;
        return;
    };
    let pinned = cs.posts.pinned(id);
    let top = crate::systems::chats::top_offset(&chat) + 52.0;
    let ctx = egui.ctx_mut().clone();
    let (mut open, mut clear) = (false, false);
    egui::Area::new(egui::Id::new("community_filter_chip"))
        .order(egui::Order::Middle)
        .anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, top))
        .show(&ctx, |ui| {
            crate::systems::chats::glass_frame()
                .corner_radius(egui::CornerRadius::same(16))
                .inner_margin(egui::Margin::symmetric(10, 4))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;
                        let (dot, _) =
                            ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
                        ui.painter().circle_filled(dot.center(), 5.0, theme::PUBLIC);
                        let posts = if pinned == 1 { "post" } else { "posts" };
                        if ui
                            .add(
                                egui::Label::new(
                                    egui::RichText::new(format!("Only {name} · {pinned} {posts}"))
                                        .size(theme::SIZE_SUB)
                                        .color(theme::CREAM),
                                )
                                .sense(egui::Sense::click()),
                            )
                            .on_hover_text("Open the community's page")
                            .on_hover_cursor(egui::CursorIcon::PointingHand)
                            .clicked()
                        {
                            open = true;
                        }
                        if crate::systems::chats::circle_button(
                            ui,
                            icons::CLEAR,
                            false,
                            true,
                            "Show every community's posts",
                        )
                        .clicked()
                        {
                            clear = true;
                        }
                    });
                });
        });
    if clear {
        cs.posts.only = None;
    }
    if open {
        cs.show(id);
        *right_panel = crate::resources::RightPanel::Chat;
    }
}

/// The long-press ring's Post here (`ui::render_context_menu`): the composer,
/// with that spot as the post's place, in the community on the map; else the
/// community picker first.
pub fn post_here(
    mut menu: ResMut<crate::resources::ContextMenuState>,
    mut cs: ResMut<Communities>,
    mut right_panel: ResMut<crate::resources::RightPanel>,
) {
    let Some((lat, lng)) = menu.post_here.take() else {
        return;
    };
    cs.posts.from_view = Some(PostPlace {
        latitude: lat,
        longitude: lng,
        name: None,
        view: None,
    });
    match cs.posts.community_to_post_in(&cs.list) {
        Some(id) => {
            cs.posts.compose(id);
            cs.posts.over_map = true;
            *right_panel = crate::resources::RightPanel::None;
        }
        None => {
            cs.screen = Some(crate::systems::communities::Screen::PostTo);
            *right_panel = crate::resources::RightPanel::Chat;
        }
    }
}

/// A post started from the map, written over the map: beside it held
/// sideways, along the bottom held upright, with the place still in sight
/// (`docs/mockups/social-camera/`, "Post from the long press"). The same
/// composer the community page has.
pub fn render_floating_composer(
    mut egui: crate::systems::egui_manager::EguiMut,
    mut cs: ResMut<Communities>,
    right_panel: Res<crate::resources::RightPanel>,
    camera: Res<crate::systems::camera_screen::CameraScreen>,
) {
    if !cs.posts.over_map {
        return;
    }
    let Some(community) = cs
        .posts
        .draft
        .as_ref()
        .and_then(|d| cs.get(d.community))
        .cloned()
    else {
        cs.posts.over_map = false;
        return;
    };
    if !right_panel.is(crate::resources::RightPanel::None) || camera.is_open() {
        return;
    }
    let viewport = egui.viewport_pts();
    let sideways = viewport.x > viewport.y;
    let (anchor, offset, width, height) = if sideways {
        (
            egui::Align2::RIGHT_TOP,
            egui::vec2(-theme::OSD_MARGIN, theme::OSD_MARGIN),
            380.0_f32.min(viewport.x * 0.5),
            viewport.y - 2.0 * theme::OSD_MARGIN,
        )
    } else {
        (
            egui::Align2::CENTER_BOTTOM,
            egui::vec2(0.0, -theme::OSD_MARGIN),
            viewport.x - 2.0 * theme::OSD_MARGIN,
            viewport.y * 0.55,
        )
    };
    let ctx = egui.ctx_mut().clone();
    egui::Area::new(egui::Id::new("floating_post_composer"))
        .order(egui::Order::Foreground)
        .anchor(anchor, offset)
        .show(&ctx, |ui| {
            crate::systems::chats::glass_frame().show(ui, |ui| {
                ui.set_width(width - 24.0);
                egui::ScrollArea::vertical()
                    .max_height(height - 24.0)
                    .show(ui, |ui| composer(ui, &mut cs, &community));
            });
        });
    if cs.posts.draft.is_none() {
        cs.posts.over_map = false;
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
            community: None,
            open_reports: 0,
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
                snapshot: None,
                place,
                url: None,
                created_ms: now - ago_min * 60_000,
                edited_ms: None,
                replies,
                mine: false,
                likes: replies * 2,
                dislikes: 0,
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
        // As picking it in the list does, without the read: offline.
        posts.only = Some(community);
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
            snapshot: None,
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
            snapshot: None,
            place,
            url: None,
            created_ms: 0,
            edited_ms: None,
            replies: 0,
            mine: false,
            likes: 0,
            dislikes: 0,
        };
        posts
            .feeds
            .insert(c, vec![post(Some(place.clone())), post(None)]);
        let other = Uuid::new_v4();
        let mut elsewhere = post(Some(place));
        elsewhere.community = other;
        posts.feeds.insert(other, vec![elsewhere]);
        assert!(posts.map_posts().is_empty());
        // One community's page asked: only its placed post.
        posts.on_map = Some(c);
        let pins = posts.map_posts();
        assert_eq!(pins.len(), 1);
        assert_eq!(pins[0].0, c);
        assert_eq!(pins[0].2.name.as_deref(), Some("Plant swap"));
        // Layers ▸ Community posts: every community's placed posts.
        posts.on_map = None;
        posts.layer = true;
        assert_eq!(posts.map_posts().len(), 2);
    }
}
