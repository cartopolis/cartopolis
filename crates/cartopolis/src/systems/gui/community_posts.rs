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
    suggest_handle, EditPost, NewPost, NewReply, NewReport, NoticeKind, PostDto, PostNotices,
    PostPlace, PostReport, PostThread, PublicInfo, PublicSettings, ReadNotices, ReportAction,
    SourceKind, Toggle, WhoPosts, MAX_PICTURE_ALT, MAX_POST_TEXT, MAX_WARNING,
};
use cartopolis_core::{CommunityDto, CommunityRole};
use std::collections::{HashMap, HashSet};
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
    /// A content warning, shown in place of the post until opened; `None`
    /// when off.
    pub warning: Option<String>,
    /// What the picture shows, in words.
    pub alt: String,
    /// Blur the picture until a reader asks to see it.
    pub sensitive: bool,
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
    /// A like or a boost changed a post: the post as it is now.
    Toggled(Result<PostDto, String>),
    /// A report went to the moderators.
    Reported(Result<(), String>),
    /// What happened to this account in public.
    Notices(Result<PostNotices, String>),
}

/// Everything the Posts half of the panel holds.
#[derive(Default)]
pub struct Posts {
    pub tab: Tab,
    pub(crate) info: HashMap<Uuid, PublicInfo>,
    pub(crate) feeds: HashMap<Uuid, Vec<PostDto>>,
    /// The open thread, once it has arrived, and which post it is.
    pub(crate) thread: Option<PostThread>,
    pub(crate) thread_open: Option<Uuid>,
    pub draft: Option<Draft>,
    /// Whether the draft is open as the community's page (`compose_page`).
    /// Off, a draft with something in it waits behind "Continue your post",
    /// so looking round the map for a better view does not cost the text.
    pub composing: bool,
    pub(crate) reply: String,
    /// The reply being answered, when it is one rather than the post: its id
    /// and its author's name.
    pub(crate) reply_to: Option<(Uuid, String)>,
    /// The post being reported: what the reporter says, and whether it goes
    /// to its own server too.
    pub(crate) reporting: Option<(Uuid, String, bool)>,
    /// Posts behind a content warning or with a sensitive picture that the
    /// reader opened.
    pub(crate) shown: HashSet<Uuid>,
    /// Replies, mentions, likes, boosts and followers, once read.
    pub notices: Option<PostNotices>,
    /// When they were last read.
    notices_at: Option<bevy::platform::time::Instant>,
    pub(crate) handle: String,
    pub(crate) follow: String,
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
    pub sending: bool,
    pub(crate) settings_open: bool,
    /// The post whose actions are open.
    pub(crate) menu_for: Option<Uuid>,
    /// The post or reply being edited, and its words so far.
    pub(crate) editing: Option<(Uuid, String)>,
    /// Reports from other servers, per community, while there are any.
    pub(crate) reports: HashMap<Uuid, Vec<PostReport>>,
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
    /// The library draws the filter chip (`lib_filter_chip`):
    /// [`render_filter_chip`] still decides whether it shows, and publishes
    /// what it says in [`chip_view`](Self::chip_view), but draws nothing.
    pub chip_lib: bool,
    /// The chip as egui's system last worked it out; `None` while hidden.
    pub chip_view: Option<ChipView>,
    /// The library draws the composer over the map (`lib_post_composer`):
    /// [`render_floating_composer`] still decides whether it shows, and
    /// publishes [`composer_view`](Self::composer_view), but draws nothing.
    pub composer_lib: bool,
    /// The floating composer as egui's system last worked it out; `None`
    /// while hidden.
    pub composer_view: Option<ComposerView>,
}

/// What the chip over the map says, for the library's edition.
#[derive(Debug, Clone, PartialEq)]
pub struct ChipView {
    pub community: Uuid,
    pub name: String,
    /// How many of its posts are pinned on the map.
    pub pinned: usize,
}

impl ChipView {
    /// "Only Garage44 · 3 posts".
    #[must_use]
    #[cfg_attr(not(feature = "ui-lib"), allow(dead_code))]
    pub fn words(&self) -> String {
        let posts = if self.pinned == 1 { "post" } else { "posts" };
        format!("Only {} · {} {posts}", self.name, self.pinned)
    }
}

/// A tap on the filter chip: one set, for both editions, done by
/// [`chip_act`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChipAction {
    /// The name: the community's page.
    Open,
    /// ✕: every community's posts again.
    Clear,
}

/// Do a tap on the filter chip, for egui's and the library's.
pub fn chip_act(
    cs: &mut Communities,
    right_panel: &mut crate::resources::RightPanel,
    action: ChipAction,
) {
    let Some(id) = cs.posts.only else {
        return;
    };
    match action {
        ChipAction::Clear => cs.posts.only = None,
        ChipAction::Open => {
            cs.show(id);
            *right_panel = crate::resources::RightPanel::Chat;
        }
    }
}

/// What the floating composer shows that is not the draft's own, for the
/// library's edition: where it stands, and who the post reaches.
#[derive(Debug, Clone, PartialEq)]
pub struct ComposerView {
    pub community: Uuid,
    /// Held sideways: beside the map, not along its bottom.
    pub sideways: bool,
    /// The followers on other servers the post is sent to.
    pub followers: u32,
    /// The community's posts go to other servers.
    pub federating: bool,
}

/// The line under the composer saying who a post reaches, without its icon.
#[must_use]
pub fn reach_words(federating: bool, followers: u32) -> String {
    if federating {
        format!(
            "Public: on the community's page and map, and sent to {followers} follower{} on other servers, under your own address.",
            if followers == 1 { "" } else { "s" }
        )
    } else {
        "Public: on the community's page and map, for anybody on this server.".to_string()
    }
}

/// A tap in the composer that is not typing: one set, for both editions,
/// done by [`composer_act`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComposerAction {
    /// ✕: the draft goes.
    Discard,
    Post,
    /// Use view: the camera's view is the post's place again.
    UseView,
    /// The camera, for a picture.
    Camera,
    /// A picture from this device.
    Upload,
    RemoveSnapshot,
    RemovePicture,
}

/// Do a tap in the composer, for egui's and the library's.
pub fn composer_act(cs: &mut Communities, action: ComposerAction) {
    let busy = !cs.picture_for_post_idle() || cs.posts.snapshot_busy;
    match action {
        ComposerAction::Discard => {
            cs.posts.draft = None;
            cs.picture_for_post = false;
        }
        ComposerAction::Post => {
            if ready_to_post(cs) {
                send_post(cs);
            }
        }
        ComposerAction::UseView => cs.posts.want_view = true,
        ComposerAction::Camera => {
            if !busy {
                cs.posts.want_camera = true;
            }
        }
        ComposerAction::Upload => {
            if !busy {
                cs.picture_for_post = true;
                cs.pick_picture = true;
            }
        }
        ComposerAction::RemoveSnapshot => {
            if let Some(d) = cs.posts.draft.as_mut() {
                d.snapshot = None;
            }
        }
        ComposerAction::RemovePicture => {
            if let Some(d) = cs.posts.draft.as_mut() {
                d.picture = None;
            }
        }
    }
}

/// How often the map's posts are read again while the layer is on: a post
/// written elsewhere shows up within this, and a quiet community costs one
/// small request a minute.
const SWEEP_SECS: u64 = 60;

/// A failed request in words. The client's HTTP layer keeps only the status,
/// so the status is what is explained. Also how an agent's post tools word one
/// (`mcp::agent`).
pub(crate) fn said(e: &str) -> String {
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
        self.reply_to = None;
        self.spawn(async move { Done::Thread(json(api_get(&format!("/api/posts/{post}")).await)) });
    }

    /// Start writing a post in `community`, about the camera's view (or the
    /// place "Post this view" brought).
    pub fn compose(&mut self, community: Uuid) {
        let place = self.from_view.take();
        self.want_view = place.is_none();
        self.tab = Tab::Posts;
        self.composing = true;
        self.draft = Some(Draft {
            community,
            text: String::new(),
            place,
            picture: None,
            snapshot: None,
            warning: None,
            alt: String::new(),
            sensitive: false,
        });
    }

    /// A post changed (a like, a boost): put it wherever it is shown.
    fn replace(&mut self, post: PostDto) {
        let lists = self
            .feeds
            .values_mut()
            .chain(self.thread.as_mut().map(|t| &mut t.replies).into_iter());
        for list in lists {
            for p in list.iter_mut().filter(|p| p.id == post.id) {
                *p = post.clone();
            }
        }
        if let Some(t) = self.thread.as_mut().filter(|t| t.post.id == post.id) {
            t.post = post;
        }
    }

    /// Like a post or pass it on (`what` is `like` or `boost`), or take that
    /// back.
    fn toggle(&mut self, post: Uuid, what: &'static str, on: bool) {
        let body = serde_json::to_string(&Toggle { on }).unwrap_or_default();
        self.spawn(async move {
            Done::Toggled(json(
                api_post(&format!("/api/posts/{post}/{what}"), Some(body)).await,
            ))
        });
    }

    /// Read this account's notices again, at most once a minute.
    pub fn refresh_notices(&mut self, force: bool) {
        let due = self.notices_at.is_none_or(|t| t.elapsed().as_secs() >= 60);
        if !force && !due {
            return;
        }
        self.notices_at = Some(bevy::platform::time::Instant::now());
        self.spawn(async move { Done::Notices(json(api_get("/api/fediverse/notices").await)) });
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
                    self.reply_to = None;
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
                Done::Toggled(Ok(post)) => self.replace(post),
                Done::Reported(Ok(())) => {
                    self.reporting = None;
                    self.note = Some("Reported. The moderators will look at it.".into());
                }
                Done::Notices(Ok(n)) => self.notices = Some(n),
                // Signed out, or a server older than notices: nothing to show.
                Done::Notices(Err(_)) => {}
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
                | Done::Toggled(Err(e))
                | Done::Reported(Err(e))
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

pub(crate) fn first_line(text: &str, max: usize) -> String {
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
pub(crate) fn ago(at_ms: i64, now_ms: i64) -> String {
    let s = (now_ms - at_ms).max(0) / 1000;
    match s {
        0..=59 => "now".into(),
        60..=3599 => format!("{} min", s / 60),
        3600..=86_399 => format!("{} h", s / 3600),
        86_400..=604_799 => format!("{} d", s / 86_400),
        _ => crate::systems::chats::clock_time(u64::try_from(at_ms).unwrap_or(0)),
    }
}

pub(crate) fn now_ms() -> i64 {
    jiff::Timestamp::now().as_millisecond()
}

/// A community page shows: read its public half the first time it does, and
/// forget the last community's thread and settings. Both editions call it
/// while the page is up.
pub fn load(cs: &mut Communities, id: Uuid) {
    if cs.posts.loaded != Some(id) {
        cs.posts.loaded = Some(id);
        cs.posts.thread_open = None;
        cs.posts.thread = None;
        cs.posts.settings_open = false;
        cs.posts.handle.clear();
        cs.posts.refresh(id);
    }
}

/// The fediverse address a moderator is offered before public posts are on
/// for the first time: the community's name made into one, until they type
/// their own.
pub fn suggest_first_handle(cs: &mut Communities, c: &CommunityDto) {
    let first = cs
        .posts
        .info
        .get(&c.id)
        .is_some_and(|i| i.settings.handle.is_none());
    if first && cs.posts.handle.is_empty() {
        cs.posts.handle = suggest_handle(&c.settings.name);
    }
}

/// Read this account's notices when due, and mark what they hold read:
/// Activity shows them (both editions).
pub fn read_notices(cs: &mut Communities) {
    cs.posts.refresh_notices(false);
    let Some(list) = cs.posts.notices.as_mut() else {
        return;
    };
    if list.unread > 0 {
        let up_to = list.items.iter().map(|n| n.id).max();
        list.unread = 0;
        let body = serde_json::to_string(&ReadNotices { up_to }).unwrap_or_default();
        cs.posts.spawn(async move {
            Done::Changed(
                Uuid::nil(),
                api_post("/api/fediverse/notices/read", Some(body))
                    .await
                    .map(|_| ()),
            )
        });
    }
}

/// A tap in a community's public half (its posts, a thread, the settings,
/// Activity, Post to) that is not typing: one set for egui's drawing and
/// the component library's (`lib_communities`), done by [`post_tap`].
#[derive(Debug, Clone)]
pub enum PostTap {
    /// Posts or Topics.
    Tab(Tab),
    /// The note of what went wrong.
    DismissNote,
    /// Write a post in this community.
    Write(Uuid),
    /// Pick a draft left by Back up again, or let it go.
    ContinueDraft,
    DiscardDraft,
    /// Pin this community's posts on the map, or stop.
    TogglePins(Uuid),
    /// Read the community's posts again.
    Refresh(Uuid),
    /// Open or fold a moderator's settings for the public half.
    ToggleSettings,
    /// A moderator's answer to a report from another server.
    Resolve {
        community: Uuid,
        report: Uuid,
        action: ReportAction,
    },
    OpenThread(Uuid),
    CloseThread,
    /// Like a post or pass it on (`true`), or take that back.
    Like(Uuid, bool),
    Boost(Uuid, bool),
    /// Answer this reply rather than the post.
    ReplyTo {
        post: Uuid,
        name: String,
        address: String,
        local: bool,
    },
    /// Answer the post again.
    ReplyToPost,
    /// Send the reply typed under the thread whose post this is.
    SendReply(Uuid),
    /// Open (or close again) a post behind a warning or a sensitive picture.
    ToggleShown(Uuid),
    /// Open a post's place on the map: (community, where).
    ShowInWorld(Uuid, Point),
    /// A post's actions: open or fold.
    ToggleMenu(Uuid),
    /// Edit a post of yours, from these words.
    Edit(Uuid, String),
    /// Save the edit: (community, post).
    SaveEdit(Uuid, Uuid),
    CancelEdit,
    /// Delete a post of yours, hide somebody else's from the community, or
    /// block its author from the community: (community, post or address).
    Delete(Uuid, Uuid),
    Hide(Uuid, Uuid),
    Block(Uuid, String),
    /// Report a post to the moderators: start, send, or not.
    Report(Uuid),
    SendReport(Uuid),
    CancelReport,
    /// A moderator's settings: switch posts on or off, or who may post.
    Public {
        community: Uuid,
        enabled: Option<bool>,
        who: Option<WhoPosts>,
    },
    /// Stop following an address, or follow the one typed.
    Unfollow(Uuid, String),
    Follow(Uuid),
    /// A notice in Activity: open its post on its community's page.
    Notice(Uuid, Uuid),
    /// Post to: write the view's post in this community, or not at all.
    PostIn(Uuid),
    CancelPostTo,
}

/// Whether the reply typed under a thread can go.
#[must_use]
pub fn reply_ready(cs: &Communities) -> bool {
    !cs.posts.reply.trim().is_empty() && !cs.posts.sending
}

/// Whether the follow typed into a moderator's settings is an address.
#[must_use]
pub fn follow_ready(cs: &Communities) -> bool {
    let typed = cs.posts.follow.trim();
    typed.contains('@') || typed.starts_with("https://")
}

/// Do a tap in a community's public half, for egui's and the library's.
#[allow(clippy::too_many_lines)]
pub fn post_tap(cs: &mut Communities, t: PostTap, out: &mut Out) {
    match t {
        PostTap::Tab(tab) => cs.posts.tab = tab,
        PostTap::DismissNote => cs.posts.note = None,
        PostTap::Write(id) => cs.posts.compose(id),
        PostTap::ContinueDraft => cs.posts.composing = true,
        PostTap::DiscardDraft => composer_act(cs, ComposerAction::Discard),
        PostTap::TogglePins(id) => {
            cs.posts.on_map = if cs.posts.on_map == Some(id) {
                None
            } else {
                Some(id)
            };
        }
        PostTap::Refresh(id) => cs.posts.refresh(id),
        PostTap::ToggleSettings => cs.posts.settings_open = !cs.posts.settings_open,
        PostTap::Resolve {
            community,
            report,
            action,
        } => {
            let body = serde_json::json!({ "action": action }).to_string();
            cs.posts.spawn(async move {
                let r = api_post(
                    &format!("/api/communities/{community}/reports/{report}"),
                    Some(body),
                )
                .await
                .map(|_| ());
                Done::Changed(community, r)
            });
        }
        PostTap::OpenThread(post) => cs.posts.open_thread(post),
        PostTap::CloseThread => {
            cs.posts.thread_open = None;
            cs.posts.thread = None;
        }
        PostTap::Like(post, on) => cs.posts.toggle(post, "like", on),
        PostTap::Boost(post, on) => cs.posts.toggle(post, "boost", on),
        PostTap::ReplyTo {
            post,
            name,
            address,
            local,
        } => {
            cs.posts.reply_to = Some((post, name));
            if cs.posts.reply.trim().is_empty() && !local {
                cs.posts.reply = format!("@{address} ");
            }
        }
        PostTap::ReplyToPost => cs.posts.reply_to = None,
        PostTap::SendReply(root) => {
            if !reply_ready(cs) {
                return;
            }
            cs.posts.sending = true;
            let body = serde_json::to_string(&NewReply {
                text: cs.posts.reply.trim().to_string(),
            })
            .unwrap_or_default();
            let to = cs.posts.reply_to.as_ref().map_or(root, |(id, _)| *id);
            cs.posts.spawn(async move {
                Done::Replied(
                    root,
                    json(api_post(&format!("/api/posts/{to}/replies"), Some(body)).await),
                )
            });
        }
        PostTap::ToggleShown(post) => {
            if !cs.posts.shown.remove(&post) {
                cs.posts.shown.insert(post);
            }
        }
        PostTap::ShowInWorld(community, point) => {
            out.go_to = Some(point);
            cs.posts.on_map = Some(community);
        }
        PostTap::ToggleMenu(post) => {
            cs.posts.menu_for = if cs.posts.menu_for == Some(post) {
                None
            } else {
                Some(post)
            };
        }
        PostTap::Edit(post, text) => {
            cs.posts.editing = Some((post, text));
            cs.posts.menu_for = None;
        }
        PostTap::SaveEdit(community, post) => {
            let Some((_, text)) = cs.posts.editing.take() else {
                return;
            };
            if text.trim().is_empty() {
                cs.posts.editing = Some((post, text));
                return;
            }
            let body = serde_json::to_string(&EditPost {
                text: text.trim().to_string(),
            })
            .unwrap_or_default();
            // POST, the edit's alias: the app's HTTP layer has no PUT.
            cs.posts.spawn(async move {
                let r = api_post(&format!("/api/posts/{post}"), Some(body))
                    .await
                    .map(|_| ());
                Done::Changed(community, r)
            });
        }
        PostTap::CancelEdit => cs.posts.editing = None,
        PostTap::Delete(community, post) => {
            cs.posts.menu_for = None;
            cs.posts.spawn(async move {
                let r = api_delete(&format!("/api/posts/{post}")).await;
                Done::Changed(community, r)
            });
            if cs.posts.thread_open == Some(post) {
                cs.posts.thread_open = None;
            }
        }
        PostTap::Hide(community, post) => {
            cs.posts.menu_for = None;
            cs.posts.spawn(async move {
                let r = api_post(&format!("/api/posts/{post}/hide"), None)
                    .await
                    .map(|_| ());
                Done::Changed(community, r)
            });
        }
        PostTap::Block(community, address) => {
            let body = serde_json::json!({ "address": address }).to_string();
            cs.posts.menu_for = None;
            cs.posts.spawn(async move {
                let r = api_post(&format!("/api/communities/{community}/blocks"), Some(body))
                    .await
                    .map(|_| ());
                Done::Changed(community, r)
            });
        }
        PostTap::Report(post) => {
            cs.posts.reporting = Some((post, String::new(), false));
            cs.posts.menu_for = None;
        }
        PostTap::SendReport(post) => {
            let Some((_, reason, forward)) = cs.posts.reporting.clone().filter(|r| r.0 == post)
            else {
                return;
            };
            let body = serde_json::to_string(&NewReport {
                reason: reason.trim().to_string(),
                forward,
            })
            .unwrap_or_default();
            cs.posts.spawn(async move {
                Done::Reported(
                    api_post(&format!("/api/posts/{post}/report"), Some(body))
                        .await
                        .map(|_| ()),
                )
            });
        }
        PostTap::CancelReport => cs.posts.reporting = None,
        PostTap::Public {
            community,
            enabled,
            who,
        } => {
            let Some(info) = cs.posts.info.get(&community) else {
                return;
            };
            let first = info.settings.handle.is_none();
            let settings = PublicSettings {
                enabled: enabled.unwrap_or(info.settings.enabled),
                handle: first.then(|| cs.posts.handle.trim().to_string()),
                who_posts: who.unwrap_or(info.settings.who_posts),
            };
            let body = serde_json::to_string(&settings).unwrap_or_default();
            cs.posts.spawn(async move {
                let r = api_post(&format!("/api/communities/{community}/public"), Some(body))
                    .await
                    .map(|_| ());
                Done::Changed(community, r)
            });
        }
        PostTap::Unfollow(community, address) => {
            cs.posts.spawn(async move {
                let r =
                    api_delete(&format!("/api/communities/{community}/follows/{address}")).await;
                Done::Changed(community, r)
            });
        }
        PostTap::Follow(community) => {
            if !follow_ready(cs) {
                return;
            }
            let body = serde_json::json!({ "address": cs.posts.follow.trim() }).to_string();
            cs.posts.follow.clear();
            cs.posts.spawn(async move {
                let r = api_post(&format!("/api/communities/{community}/follows"), Some(body))
                    .await
                    .map(|_| ());
                Done::Changed(community, r)
            });
        }
        PostTap::Notice(community, post) => {
            cs.posts.focus(community, post);
            cs.screen = Some(crate::systems::communities::Screen::Community(community));
        }
        PostTap::PostIn(id) => {
            cs.posts.compose(id);
            cs.show(id);
        }
        PostTap::CancelPostTo => {
            cs.posts.from_view = None;
            cs.posts.pending_picture = None;
            cs.screen = None;
        }
    }
}

/// The two halves of a community page, side by side. Answers which is showing.
pub fn tabs(ui: &mut egui::Ui, cs: &mut Communities, id: Uuid, topics: usize) -> Tab {
    load(cs, id);
    let mut tab = cs.posts.tab;
    let posts = format!("{}  Posts · public", icons::SHARE);
    let topics = format!("{}  Topics · {topics}", icons::LOCK);
    if theme::segmented(
        ui,
        &mut tab,
        &[(Tab::Posts, &posts), (Tab::Topics, &topics)],
    ) {
        post_tap(cs, PostTap::Tab(tab), &mut Out::default());
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
                    .color(theme::p().error)
                    .size(theme::SIZE_SUB),
            );
            if theme::icon_button_small(ui, icons::CLEAR, "Dismiss").clicked() {
                post_tap(cs, PostTap::DismissNote, &mut Out::default());
            }
        });
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
                .color(theme::p().text_dim)
                .size(theme::SIZE_SUB),
        );
    }
    ui.add_space(6.0);
    if info.may_post {
        let waiting = cs.posts.draft.as_ref().is_some_and(|d| d.community == id);
        if waiting {
            // A draft left by Back: pick it up, or let it go.
            ui.horizontal(|ui| {
                let side = theme::PRIMARY_BUTTON_HEIGHT.max(theme::row_height());
                let gap = ui.spacing().item_spacing.x;
                let wide = (ui.available_width() - side - gap).floor();
                if theme::primary_button_sized(
                    ui,
                    &format!("{}  Continue your post", icons::RENAME),
                    wide,
                )
                .clicked()
                {
                    post_tap(cs, PostTap::ContinueDraft, &mut Out::default());
                }
                let drop = theme::secondary_button(ui, icons::CLEAR, "", side);
                if theme::named(ui, drop, "Discard the draft")
                    .on_hover_text("Discard the draft")
                    .clicked()
                {
                    post_tap(cs, PostTap::DiscardDraft, &mut Out::default());
                }
            });
        } else {
            let width = ui.available_width();
            if theme::primary_button_sized(ui, &format!("{}  Write a post", icons::RENAME), width)
                .clicked()
            {
                post_tap(cs, PostTap::Write(id), out);
            }
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
            post_tap(cs, PostTap::TogglePins(id), &mut Out::default());
        }
        let again = theme::secondary_button(ui, icons::REFRESH, "", side);
        if theme::named(ui, again, "Read again")
            .on_hover_text("Read again")
            .clicked()
        {
            post_tap(cs, PostTap::Refresh(id), &mut Out::default());
        }
        if role.moderates() {
            let open = cs.posts.settings_open;
            let settings = theme::secondary_toggle(ui, icons::SETTINGS, "", side, open);
            settings.widget_info(|| {
                egui::WidgetInfo::selected(
                    egui::WidgetType::Button,
                    ui.is_enabled(),
                    open,
                    "Settings",
                )
            });
            if settings.on_hover_text("Settings").clicked() {
                post_tap(cs, PostTap::ToggleSettings, &mut Out::default());
            }
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
            post_tap(cs, PostTap::OpenThread(p.id), out);
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
                ui.add(
                    egui::Label::new(egui::RichText::new(&r.reason).color(theme::p().cream)).wrap(),
                );
            }
            ui.label(
                egui::RichText::new(format!(
                    "{}: {}",
                    r.post.author.address,
                    first_line(&r.post.text, 120)
                ))
                .size(theme::SIZE_SUB)
                .color(theme::p().text_dim),
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
                        post_tap(
                            cs,
                            PostTap::Resolve {
                                community: id,
                                report: r.id,
                                action,
                            },
                            &mut Out::default(),
                        );
                    }
                }
            });
        });
        ui.add_space(4.0);
    }
}

/// Below a draft's pictures: a content warning, and for a photo what it shows
/// in words and whether to blur it.
fn draft_extras(ui: &mut egui::Ui, cs: &mut Communities, community: Uuid, has_picture: bool) {
    let Some(d) = cs.posts.draft.as_mut() else {
        return;
    };
    let mut warn = d.warning.is_some();
    if theme::checkbox(ui, &mut warn, "Content warning")
        .on_hover_text("Readers see only the warning until they open the post")
        .changed()
    {
        d.warning = warn.then(String::new);
    }
    if let Some(w) = d.warning.as_mut() {
        crate::systems::theme::text_field(
            ui,
            egui::TextEdit::singleline(w)
                .id(egui::Id::new(("post_warning", community)))
                .desired_width(ui.available_width())
                .char_limit(MAX_WARNING)
                .hint_text("What readers see before they open it"),
        );
    }
    if has_picture {
        crate::systems::theme::text_field(
            ui,
            egui::TextEdit::singleline(&mut d.alt)
                .id(egui::Id::new(("post_alt", community)))
                .desired_width(ui.available_width())
                .char_limit(MAX_PICTURE_ALT)
                .hint_text("What the photo shows, in words"),
        );
        theme::checkbox(
            ui,
            &mut d.sensitive,
            "Sensitive: blur the photo until opened",
        );
    }
}

/// Like, pass on, and answer a post or reply. Counts include what other
/// servers sent: Mastodon's favourites and boosts, Lemmy's votes.
fn actions_row(ui: &mut egui::Ui, cs: &mut Communities, p: &PostDto) {
    let likes = if p.dislikes > 0 {
        format!("{}  {} · {} down", icons::STAR, p.likes, p.dislikes)
    } else {
        format!("{}  {}", icons::STAR, p.likes)
    };
    if theme::chip(ui, &likes, p.liked)
        .on_hover_text(if p.liked {
            "Take your like back"
        } else {
            "Like"
        })
        .clicked()
    {
        post_tap(cs, PostTap::Like(p.id, !p.liked), &mut Out::default());
    }
    if p.parent.is_none() {
        let boosts = format!("{}  {}", icons::SHARE, p.boosts);
        if p.mine {
            if p.boosts > 0 {
                ui.label(
                    egui::RichText::new(boosts)
                        .size(theme::SIZE_SUB)
                        .color(theme::p().text_dim),
                );
            }
        } else if theme::chip(ui, &boosts, p.boosted)
            .on_hover_text(if p.boosted {
                "Stop passing it on"
            } else {
                "Pass it on to the people who follow you"
            })
            .clicked()
        {
            post_tap(cs, PostTap::Boost(p.id, !p.boosted), &mut Out::default());
        }
    } else if theme::chip(ui, &format!("{}  Reply", icons::CHAT), false)
        .on_hover_text(format!("Answer {}", p.author.name))
        .clicked()
    {
        post_tap(
            cs,
            PostTap::ReplyTo {
                post: p.id,
                name: p.author.name.clone(),
                address: p.author.address.clone(),
                local: p.author.local,
            },
            &mut Out::default(),
        );
    }
}

/// A post behind a content warning: the warning, and Show or Hide. Answers
/// whether the post's words and pictures are hidden.
fn warning_gate(ui: &mut egui::Ui, cs: &mut Communities, p: &PostDto) -> bool {
    let Some(w) = &p.warning else {
        return false;
    };
    let hidden = !p.mine && !cs.posts.shown.contains(&p.id);
    ui.horizontal_wrapped(|ui| {
        ui.label(egui::RichText::new(format!("{}  {w}", icons::WARNING)).color(theme::p().warn));
        if !p.mine && theme::chip(ui, if hidden { "Show" } else { "Hide" }, false).clicked() {
            post_tap(cs, PostTap::ToggleShown(p.id), &mut Out::default());
        }
    });
    hidden
}

/// A sensitive picture stays a button until it is asked for. Answers whether
/// the picture may be drawn.
fn sensitive_gate(ui: &mut egui::Ui, cs: &mut Communities, p: &PostDto) -> bool {
    if !p.sensitive || p.mine || cs.posts.shown.contains(&p.id) {
        return true;
    }
    if theme::chip(ui, "Sensitive photo: tap to show", false).clicked() {
        post_tap(cs, PostTap::ToggleShown(p.id), &mut Out::default());
    }
    false
}

/// What somebody reporting a post says, and whether it goes to its own server.
fn report_box(ui: &mut egui::Ui, cs: &mut Communities, p: &PostDto) {
    let Some((_, reason, forward)) = cs.posts.reporting.as_mut().filter(|r| r.0 == p.id) else {
        return;
    };
    ui.separator();
    crate::systems::theme::text_field(
        ui,
        egui::TextEdit::multiline(reason)
            .id(egui::Id::new(("post_report", p.id)))
            .desired_rows(2)
            .desired_width(ui.available_width())
            .char_limit(1000)
            .hint_text("What is wrong with it? (optional)"),
    );
    if !p.author.local {
        let server = p.author.address.split('@').nth(1).unwrap_or("its server");
        theme::checkbox(
            ui,
            forward,
            format!("Also tell {server}, without your name"),
        );
    }
    ui.horizontal(|ui| {
        if theme::action_button(ui, format!("{} Report", icons::FLAG)).clicked() {
            post_tap(cs, PostTap::SendReport(p.id), &mut Out::default());
        }
        if theme::action_button(ui, "Cancel").clicked() {
            post_tap(cs, PostTap::CancelReport, &mut Out::default());
        }
    });
}

/// Replies, mentions, likes, boosts and new followers on this account's public
/// posts, here and from other servers, newest first. Drawing it marks them
/// read; a notice about a post answers that post and its community.
pub fn notices_list(ui: &mut egui::Ui, cs: &mut Communities) -> Option<(Uuid, Uuid)> {
    read_notices(cs);
    let Some(list) = cs.posts.notices.clone() else {
        theme::busy(ui, "Reading what happened…");
        return None;
    };
    if list.items.is_empty() {
        theme::note(
            ui,
            "Nothing yet. Replies, likes and followers on your public posts show here.",
        );
    }
    let now = now_ms();
    let mut open = None;
    for n in &list.items {
        let (icon, words) = match n.kind {
            NoticeKind::Reply => (icons::CHAT, "replied"),
            NoticeKind::Mention => (icons::LABEL, "mentioned you"),
            NoticeKind::Like => (icons::STAR, "likes your post"),
            NoticeKind::Boost => (icons::SHARE, "passed on your post"),
            NoticeKind::Follow => (icons::FOLLOW, "follows you"),
        };
        let card = raised_frame().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                avatar(ui, &n.who.name, ChatKind::Private, 28.0);
                ui.vertical(|ui| {
                    ui.label(
                        egui::RichText::new(format!("{icon}  {} {words}", n.who.name)).color(
                            if n.read {
                                theme::p().text
                            } else {
                                theme::p().cream
                            },
                        ),
                    );
                    let mut line = ago(n.created_ms, now);
                    if !n.who.local {
                        line = format!("{} · {line}", n.who.address);
                    }
                    ui.label(egui::RichText::new(line).size(theme::SIZE_SUB).color(
                        if n.who.local {
                            theme::p().text_dim
                        } else {
                            theme::PUBLIC
                        },
                    ));
                    if let Some(e) = &n.excerpt {
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(e)
                                    .size(theme::SIZE_SUB)
                                    .color(theme::p().text_dim),
                            )
                            .truncate(),
                        );
                    }
                });
            });
        });
        if let (Some(post), Some(community)) = (n.post, n.community) {
            let r = card.response.interact(egui::Sense::click());
            if theme::named(ui, r, &n.who.name)
                .on_hover_cursor(egui::CursorIcon::PointingHand)
                .clicked()
            {
                open = Some((community, post));
            }
        }
        ui.add_space(4.0);
    }
    open
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
                ui.label(egui::RichText::new(&p.author.name).color(theme::p().cream));
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
                        .color(if p.author.local { theme::p().text_dim } else { theme::PUBLIC }),
                );
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // Anybody may report what is not theirs.
                if theme::kebab_button(ui).clicked()
                {
                    post_tap(cs, PostTap::ToggleMenu(p.id), &mut Out::default());
                }
            });
        });
        if let Some(to) = &p.answers {
            ui.label(
                egui::RichText::new(format!("{}  to {to}", icons::CHAT))
                    .size(theme::SIZE_SUB)
                    .color(theme::p().text_dim),
            );
        }
        let hidden = warning_gate(ui, cs, p);
        if hidden {
            // Behind its warning: the words and pictures wait.
        } else if cs.posts.editing.as_ref().is_some_and(|(e, _)| *e == p.id) {
            edit_box(ui, cs, c.id, p.id);
        } else {
            ui.add(egui::Label::new(egui::RichText::new(&p.text).color(theme::p().text)).wrap());
        }
        let pictures_shown = !hidden && sensitive_gate(ui, cs, p);
        // The polaroid keeps its own proportions: its frame is
        // part of the picture, and a crop would cut them off.
        if let Some(t) = snapshot.as_ref().filter(|_| !hidden) {
            let w = ui.available_width().min(420.0);
            let size = t.size_vec2();
            ui.add(
                egui::Image::new(t).fit_to_exact_size(egui::vec2(w, w * size.y / size.x.max(1.0))),
            );
        }
        let more = p
            .more_pictures
            .iter()
            .filter_map(|x| cs.picture(ui.ctx(), Some(&x.hash)).map(|t| (t, x.alt.clone())));
        let pictures: Vec<_> = picture
            .into_iter()
            .map(|t| (t, p.picture_alt.clone()))
            .chain(more)
            .collect();
        for (t, alt) in pictures.iter().filter(|_| pictures_shown) {
            let w = ui.available_width();
            let size = t.size_vec2();
            let h = (w * size.y / size.x.max(1.0)).min(220.0);
            let shown = ui.add(
                egui::Image::new(t)
                    .fit_to_exact_size(egui::vec2(w, h))
                    .corner_radius(10.0),
            );
            if let Some(alt) = alt {
                shown.on_hover_text(alt);
            }
        }
        ui.horizontal_wrapped(|ui| {
            match &p.place {
                Some(place) => {
                    let label = place.name.clone().unwrap_or_else(|| "On the map".into());
                    ui.label(
                        egui::RichText::new(format!("{}  {label}", icons::PIN))
                            .size(theme::SIZE_SUB)
                            .color(theme::p().text_dim),
                    );
                    // Show in world (`bridges`): the map, at where it was
                    // written, and a way back to this post.
                    if theme::chip(ui, &format!("{}  Show in world", icons::CAM_EARTH), false)
                        .on_hover_text("Open where it was written, on the map")
                        .clicked()
                    {
                        post_tap(cs, PostTap::ShowInWorld(c.id, point_of(place, &p.text)), out);
                    }
                }
                None => {
                    ui.label(
                        egui::RichText::new("No place: listed, not pinned")
                            .size(theme::SIZE_SUB)
                            .color(theme::p().text_dim),
                    );
                }
            }
            actions_row(ui, cs, p);
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
                    post_tap(cs, PostTap::Edit(p.id, p.text.clone()), &mut Out::default());
                }
                if p.mine
                    && theme::confirm_button(
                        ui,
                        egui::Id::new(("post_delete", p.id)),
                        &format!("{} Delete", icons::DELETE),
                        "Delete: tap again",
                    )
                {
                    post_tap(cs, PostTap::Delete(c.id, p.id), &mut Out::default());
                }
                if moderates && !p.mine {
                    if theme::action_button(ui, format!("{} Hide from this community", icons::DELETE))
                        .clicked()
                    {
                        post_tap(cs, PostTap::Hide(c.id, p.id), &mut Out::default());
                    }
                    if !p.author.local
                        && theme::action_button(
                            ui,
                            format!("{} Block {}", icons::FLAG, p.author.address),
                        )
                        .on_hover_text("They can't post here again, and what they wrote here is hidden")
                        .clicked()
                    {
                        post_tap(cs, PostTap::Block(c.id, p.author.address.clone()), &mut Out::default());
                    }
                }
            });
            if !p.mine
                && theme::action_button(ui, format!("{} Report", icons::FLAG))
                    .on_hover_text("To this community's moderators")
                    .clicked()
            {
                post_tap(cs, PostTap::Report(p.id), &mut Out::default());
            }
            if moderates && !p.author.local {
                theme::note(
                    ui,
                    "A server that sends nothing but trouble is the server admin's to block, for every community at once.",
                );
            }
        }
        report_box(ui, cs, p);
    });
    if tappable {
        let card = frame.response.interact(egui::Sense::click());
        if theme::named(ui, card, &p.author.name)
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .clicked()
        {
            tapped = true;
        }
    }
    tapped
}

/// The words of a post being edited, with Save and Cancel. A change reaches
/// other servers as an edit, as Mastodon's own do.
fn edit_box(ui: &mut egui::Ui, cs: &mut Communities, community: Uuid, post: Uuid) {
    let Some((_, text)) = cs.posts.editing.as_mut() else {
        return;
    };
    crate::systems::theme::text_field(
        ui,
        egui::TextEdit::multiline(text)
            .id(egui::Id::new(("post_edit", post)))
            .desired_rows(3)
            .desired_width(ui.available_width())
            .char_limit(MAX_POST_TEXT),
    );
    let ready = !text.trim().is_empty();
    ui.horizontal(|ui| {
        if ui
            .add_enabled_ui(ready, |ui| {
                theme::action_button(ui, format!("{} Save", icons::RENAME))
            })
            .inner
            .clicked()
        {
            post_tap(cs, PostTap::SaveEdit(community, post), &mut Out::default());
        }
        if theme::action_button(ui, "Cancel").clicked() {
            post_tap(cs, PostTap::CancelEdit, &mut Out::default());
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
    let clicked = theme::ghost_button(ui, icons::CHEVRON_LEFT, "Posts").clicked();
    if theme::page_back(ui.ctx(), clicked) {
        post_tap(cs, PostTap::CloseThread, out);
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
    if let Some((_, name)) = cs.posts.reply_to.clone() {
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(format!("{}  To {name}", icons::CHAT))
                    .size(theme::SIZE_SUB)
                    .color(theme::p().text_dim),
            );
            if theme::close_button(ui)
                .on_hover_text("Answer the post instead")
                .clicked()
            {
                post_tap(cs, PostTap::ReplyToPost, &mut Out::default());
            }
        });
    }
    crate::systems::theme::text_field(
        ui,
        egui::TextEdit::multiline(&mut cs.posts.reply)
            .id(egui::Id::new(("post_reply", post)))
            .desired_rows(2)
            .desired_width(ui.available_width())
            .char_limit(MAX_POST_TEXT)
            .hint_text("Reply in public"),
    );
    let ready = reply_ready(cs);
    let width = ui.available_width();
    if ui
        .add_enabled_ui(ready, |ui| {
            theme::primary_button_sized(ui, &format!("{}  Reply", icons::SEND), width)
        })
        .inner
        .clicked()
    {
        post_tap(cs, PostTap::SendReply(t.post.id), out);
    }
    theme::note(
        ui,
        "Replies are public, like the post, and reach its author wherever they are.",
    );
}

/// The composer over the map (`render_floating_composer`), with its own title
/// row — Post, and a ✕ that discards: over the map there is no page to go
/// back to.
fn composer(ui: &mut egui::Ui, cs: &mut Communities, c: &CommunityDto) {
    let ready = ready_to_post(cs);
    let label = if cs.posts.sending {
        "Posting…"
    } else {
        "Post"
    };
    let mut send = false;
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new("New post")
                .size(theme::SIZE_TITLE)
                .color(theme::p().cream),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if theme::close_button(ui).on_hover_text("Discard").clicked() {
                composer_act(cs, ComposerAction::Discard);
            }
            send = ui
                .add_enabled_ui(ready, |ui| theme::header_action(ui, label))
                .inner
                .clicked();
        });
    });
    if send {
        composer_act(cs, ComposerAction::Post);
    }
    ui.add_space(4.0);
    compose_page(ui, cs, c);
}

/// Whether the draft can go: text and a place, nothing still uploading.
#[must_use]
pub fn ready_to_post(cs: &Communities) -> bool {
    cs.posts
        .draft
        .as_ref()
        .is_some_and(|d| !d.text.trim().is_empty() && d.place.is_some())
        && !cs.posts.sending
        && cs.picture_for_post_idle()
        && !cs.posts.snapshot_busy
}

/// Send the draft (`Done::Posted` takes it off the page when the server has
/// it). Does nothing without a place.
pub fn send_post(cs: &mut Communities) {
    let Some(d) = cs.posts.draft.clone() else {
        return;
    };
    let Some(place) = d.place else {
        return;
    };
    cs.posts.sending = true;
    let body = serde_json::to_string(&NewPost {
        text: d.text.trim().to_string(),
        sensitive: d.sensitive && d.picture.is_some(),
        picture_alt: Some(d.alt.trim().to_string())
            .filter(|a| !a.is_empty() && d.picture.is_some()),
        warning: d
            .warning
            .map(|w| w.trim().to_string())
            .filter(|w| !w.is_empty()),
        picture: d.picture,
        snapshot: d.snapshot,
        place,
    })
    .unwrap_or_default();
    let community = d.community;
    cs.posts.spawn(async move {
        Done::Posted(
            community,
            json(api_post(&format!("/api/communities/{community}/posts"), Some(body)).await),
        )
    });
}

/// Whether the community's page is the composer: a draft for it, open.
#[must_use]
pub fn composing(cs: &Communities, community: Uuid) -> bool {
    cs.posts.composing
        && cs
            .posts
            .draft
            .as_ref()
            .is_some_and(|d| d.community == community)
}

/// Back from the composer page: an empty draft goes, one with something in it
/// waits on the community's page behind "Continue your post".
pub fn leave_compose_page(cs: &mut Communities) {
    cs.posts.composing = false;
    let empty =
        cs.posts.draft.as_ref().is_none_or(|d| {
            d.text.trim().is_empty() && d.picture.is_none() && d.snapshot.is_none()
        });
    if empty {
        cs.posts.draft = None;
        cs.picture_for_post = false;
    }
}

/// The composer as the community's own page, under the page's title row
/// (`communities::community_screen`). It used to sit in the Posts tab, under
/// the community's picture, name, action strip and tabs: on a phone that is a
/// whole screen before the text field, and Post was two screens down.
pub fn compose_page(ui: &mut egui::Ui, cs: &mut Communities, c: &CommunityDto) {
    let followers = cs.posts.info.get(&c.id).map_or(0, |i| i.followers);
    let federating = cs.posts.info.get(&c.id).is_some_and(|i| i.federating);
    let Some(draft) = cs.posts.draft.as_mut() else {
        return;
    };
    crate::systems::theme::text_field(
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
                    theme::p().warn
                } else {
                    theme::p().text_dim
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
                // On a phone the words leave the place one line of three
                // letters; the icon alone, square, as the review row does.
                let side = theme::PRIMARY_BUTTON_HEIGHT.max(theme::row_height());
                let (word, w) = if ui.available_width() < 300.0 {
                    ("", side)
                } else {
                    ("Use view", 0.0)
                };
                let again = theme::secondary_button(ui, icons::HERE, word, w);
                if theme::named(ui, again, "Use view")
                    .on_hover_text("Close the panel, look somewhere else, come back and press this")
                    .clicked()
                {
                    composer_act(cs, ComposerAction::UseView);
                }
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    ui.label(
                        egui::RichText::new(icons::PIN)
                            .size(20.0)
                            .color(theme::p().accent),
                    );
                    ui.vertical(|ui| match &place {
                        Some(p) => {
                            let name = p.name.clone().unwrap_or_else(|| {
                                format!("{:.5}, {:.5}", p.latitude, p.longitude)
                            });
                            ui.add(
                                egui::Label::new(egui::RichText::new(name).color(theme::p().cream))
                                    .truncate(),
                            );
                            ui.label(
                                egui::RichText::new("Pinned where you are looking")
                                    .size(theme::SIZE_SUB)
                                    .color(theme::p().text_dim),
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
            composer_act(cs, ComposerAction::RemoveSnapshot);
        }
    }
    if let Some(t) = picture
        .as_deref()
        .and_then(|h| cs.picture(ui.ctx(), Some(h)))
    {
        // As wide as the text, or as tall as the text field for an upright
        // picture from a phone, which at the width would take the screen.
        let w = ui.available_width();
        let shown = ui.add(
            egui::Image::new(&t)
                .fit_to_exact_size(egui::vec2(w, 200.0))
                .corner_radius(10.0),
        );
        if remove_badge(ui, shown.rect, "Remove the picture") {
            composer_act(cs, ComposerAction::RemovePicture);
        }
    }
    draft_extras(ui, cs, c.id, picture.is_some());
    ui.add_space(6.0);
    // Camera and Upload, the same two round buttons wherever a picture can be
    // added (`docs/mockups/social-camera/`). The camera's In world mode is
    // the polaroid this row used to have a button of its own for.
    // With a picture on the post, the same two buttons replace it. The words
    // under them say so; a sentence beside them ran off a phone's edge and,
    // being unwrapped, widened everything under it past the screen too.
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 18.0;
        let busy = !cs.picture_for_post_idle() || cs.posts.snapshot_busy;
        for (icon, label, hint) in [
            (
                icons::CAMERA,
                if picture.is_some() {
                    "Retake"
                } else {
                    "Camera"
                },
                "Take a picture for this post",
            ),
            (
                icons::UPLOAD,
                if picture.is_some() {
                    "Replace"
                } else {
                    "Upload"
                },
                "A picture from this device",
            ),
        ] {
            // An exact cell at the cursor: `vertical_centered` with a
            // `set_width` inside centres the narrowed cell in the rest of the
            // row, which put Camera mid-screen and Upload at the right edge.
            let cell = egui::vec2(
                theme::ROUND_BUTTON_SIZE + 24.0,
                theme::ROUND_BUTTON_SIZE + 22.0,
            );
            ui.allocate_ui_with_layout(cell, egui::Layout::top_down(egui::Align::Center), |ui| {
                if theme::round_button(ui, icon, hint, false).clicked() && !busy {
                    composer_act(
                        cs,
                        if icon == icons::CAMERA {
                            ComposerAction::Camera
                        } else {
                            ComposerAction::Upload
                        },
                    );
                }
                ui.label(egui::RichText::new(label).size(12.0).color(theme::p().text));
            });
        }
    });
    if let Some(note) = &cs.picture_note {
        theme::note(ui, note);
    }
    ui.add_space(6.0);
    let reach = format!("{}  {}", icons::SHARE, reach_words(federating, followers));
    ui.add(
        egui::Label::new(
            egui::RichText::new(reach)
                .size(theme::SIZE_SUB)
                .color(theme::PUBLIC),
        )
        .wrap(),
    );
    if cs
        .posts
        .draft
        .as_ref()
        .is_some_and(|d| d.text.trim().is_empty())
    {
        theme::note(ui, "Write something, and Post above goes live.");
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
///
/// Drawn in a child `Ui` that allocates nothing in the parent: `scope_builder`
/// hands the badge's rect back to the parent as a widget of its own, and a
/// top-down layout then puts its cursor under *that* — 50 pt below the top of
/// the picture — so the warning box, the alt field and the sensitive tick were
/// written over the picture (seen on the phone, 2026-11-22).
fn remove_badge(ui: &mut egui::Ui, over: egui::Rect, tooltip: &str) -> bool {
    let side = theme::ROUND_BUTTON_SIZE;
    let rect = egui::Rect::from_min_size(
        over.right_top() + egui::vec2(-side - 6.0, 6.0),
        egui::Vec2::splat(side),
    );
    let mut badge = ui.new_child(egui::UiBuilder::new().max_rect(rect));
    theme::round_button(&mut badge, icons::CLEAR, tooltip, false).clicked()
}

fn settings_form(ui: &mut egui::Ui, cs: &mut Communities, c: &CommunityDto, info: &PublicInfo) {
    raised_frame().show(ui, |ui| {
        ui.set_width(ui.available_width());
        let id = c.id;
        let first = info.settings.handle.is_none();
        if first {
            suggest_first_handle(cs, c);
            ui.label(egui::RichText::new("The community's fediverse address").color(theme::p().cream));
            crate::systems::theme::text_field(
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
            ui.label(egui::RichText::new(format!("@{address}")).color(theme::p().cream));
        }
        let mut who = info.settings.who_posts;
        ui.label(egui::RichText::new("Who can post").size(theme::SIZE_SUB).color(theme::p().text_dim));
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
            post_tap(
                cs,
                PostTap::Public {
                    community: id,
                    enabled: switch,
                    who: Some(who),
                },
                &mut Out::default(),
            );
        }
        if !info.settings.enabled {
            return;
        }
        ui.separator();
        ui.label(egui::RichText::new("This community follows").size(theme::SIZE_SUB).color(theme::p().text_dim));
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
                ui.label(egui::RichText::new(format!("{kind}{state}")).size(theme::SIZE_SUB).color(theme::p().text_dim));
                if theme::icon_button_small(ui, icons::CLEAR, "Stop following").clicked() {
                    post_tap(cs, PostTap::Unfollow(id, f.address.clone()), &mut Out::default());
                }
            });
        }
        crate::systems::theme::text_field(
            ui,
            egui::TextEdit::singleline(&mut cs.posts.follow)
                .id(egui::Id::new(("post_follow", id)))
                .desired_width(ui.available_width())
                .hint_text("groningen@feddit.nl, someone@mastodon.social, or a relay's address"),
        );
        if theme::action_button(ui, format!("{} Follow", icons::ADD)).clicked() {
            post_tap(cs, PostTap::Follow(id), &mut Out::default());
        }
    });
}

/// "Post this view": the communities this account is in, to choose one.
pub fn pick_community(ui: &mut egui::Ui, cs: &mut Communities) {
    ui.label(
        egui::RichText::new("Post this view publicly, to")
            .size(theme::SIZE_TITLE)
            .color(theme::p().cream),
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
            .color(theme::p().text_dim),
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
            post_tap(cs, PostTap::PostIn(id), &mut Out::default());
        }
    }
    if theme::action_button(ui, "Cancel").clicked() {
        post_tap(cs, PostTap::CancelPostTo, &mut Out::default());
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
    layers: Res<crate::systems::layer_state::DataLayers>,
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
    let (shown, forget) =
        filter_chip_shown(&cs, &right_panel, &chat, &driver, &camera, online.is_some());
    if forget {
        cs.posts.only = None;
    }
    // The library's edition is showing (`lib_filter_chip`): hand it what the
    // chip says, draw nothing. Its taps it does itself, through `chip_act`.
    if cs.posts.chip_lib {
        if cs.posts.chip_view != shown {
            cs.posts.chip_view = shown;
        }
        return;
    }
    let Some(ChipView { name, pinned, .. }) = shown else {
        return;
    };
    // Under the search bar, between the two columns of round buttons that
    // come down the edges under it (`dock`), and truncated to fit there.
    let top = crate::systems::dock::reserved_top();
    let between = egui.viewport_pts().x
        - 2.0 * (theme::OSD_MARGIN + theme::ROUND_BUTTON_SIZE + theme::OSD_GAP);
    let ctx = egui.ctx_mut().clone();
    let mut asked: Option<ChipAction> = None;
    egui::Area::new(egui::Id::new("community_filter_chip"))
        .order(egui::Order::Middle)
        .anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, top))
        .show(&ctx, |ui| {
            crate::systems::theme::glass_frame()
                .corner_radius(egui::CornerRadius::same(16))
                .inner_margin(egui::Margin::symmetric(10, 4))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;
                        let (dot, _) =
                            ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
                        ui.painter().circle_filled(dot.center(), 5.0, theme::PUBLIC);
                        let posts = if pinned == 1 { "post" } else { "posts" };
                        // Room for the dot, the ✕ and the gaps beside it.
                        let words = (between - 20.0 - 10.0 - 38.0 - 2.0 * 8.0 - 20.0).max(40.0);
                        if ui
                            .scope(|ui| {
                                ui.set_max_width(words);
                                ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(format!(
                                            "Only {name} · {pinned} {posts}"
                                        ))
                                        .size(theme::SIZE_SUB)
                                        .color(theme::p().cream),
                                    )
                                    .truncate()
                                    .sense(egui::Sense::click()),
                                )
                            })
                            .inner
                            .on_hover_text("Open the community's page")
                            .on_hover_cursor(egui::CursorIcon::PointingHand)
                            .clicked()
                        {
                            asked = Some(ChipAction::Open);
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
                            asked = Some(ChipAction::Clear);
                        }
                    });
                });
        });
    if let Some(a) = asked {
        chip_act(&mut cs, &mut right_panel, a);
    }
}

/// Whether the filter chip shows, and what it says: only while one
/// community's posts are on the map, nothing else owns the screen, and the
/// community is still known; and whether the filter is to be forgotten,
/// because it cannot hold (signed out, the community gone).
fn filter_chip_shown(
    cs: &Communities,
    right_panel: &crate::resources::RightPanel,
    chat: &crate::systems::chat::ChatState,
    driver: &crate::systems::driver::DriverState,
    camera: &crate::systems::camera_screen::CameraScreen,
    online: bool,
) -> (Option<ChipView>, bool) {
    let Some(id) = cs.posts.only else {
        return (None, false);
    };
    if !online && !cs.is_demo() {
        return (None, true);
    }
    if driver.active
        || camera.is_open()
        || chat.world.is_some()
        || !right_panel.is(crate::resources::RightPanel::None)
    {
        return (None, false);
    }
    let Some(name) = cs.get(id).map(|c| c.settings.name.clone()) else {
        return (None, true);
    };
    let pinned = cs.posts.pinned(id);
    (
        Some(ChipView {
            community: id,
            name,
            pinned,
        }),
        false,
    )
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
        if cs.posts.composer_view.is_some() {
            cs.posts.composer_view = None;
        }
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
        cs.posts.composer_view = None;
        return;
    };
    let hidden = !right_panel.is(crate::resources::RightPanel::None) || camera.is_open();
    let viewport = egui.viewport_pts();
    let sideways = viewport.x > viewport.y;
    // The library's edition is showing (`lib_post_composer`): hand it where
    // the composer stands and who the post reaches, draw nothing. The draft
    // it edits itself, its taps go through `composer_act`.
    if cs.posts.composer_lib {
        let info = cs.posts.info.get(&community.id);
        let view = (!hidden).then(|| ComposerView {
            community: community.id,
            sideways,
            followers: info.map_or(0, |i| i.followers),
            federating: info.is_some_and(|i| i.federating),
        });
        if cs.posts.composer_view != view {
            cs.posts.composer_view = view;
        }
        return;
    }
    if hidden {
        return;
    }
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
            crate::systems::theme::glass_frame().show(ui, |ui| {
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
                boosts: 0,
                liked: false,
                boosted: false,
                answers: None,
                warning: None,
                sensitive: false,
                picture_alt: None,
                more_pictures: Vec::new(),
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
        posts.composing = true;
        posts.draft = Some(Draft {
            community,
            text: "Street party on the Grote Markt tonight, from 8. Bring a chair!".into(),
            place: Some(PostPlace {
                latitude: 53.2194,
                longitude: 6.5665,
                name: Some("Grote Markt, Groningen".into()),
                view: view(-30.0, 0.0),
            }),
            // A picture, as the camera leaves one: upright, from a phone.
            picture: Some(crate::systems::chat_pictures::DEMO_HASH_PORTRAIT.into()),
            snapshot: None,
            warning: None,
            alt: String::new(),
            sensitive: false,
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
            boosts: 0,
            liked: false,
            boosted: false,
            answers: None,
            warning: None,
            sensitive: false,
            picture_alt: None,
            more_pictures: Vec::new(),
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
/// Activity: what happened to this account's public posts, as a screen of its
/// own over the chat list. A notice about a post opens that post's thread on
/// its community's page.
pub fn activity_screen(ui: &mut egui::Ui, cs: &mut Communities) {
    let clicked = theme::ghost_button(ui, icons::CHEVRON_LEFT, "Chats").clicked();
    if theme::page_back(ui.ctx(), clicked) {
        cs.screen = None;
        return;
    }
    ui.label(
        egui::RichText::new("Activity")
            .size(theme::SIZE_TITLE)
            .color(theme::p().cream),
    );
    theme::note(
        ui,
        "Replies, mentions, likes and followers on your public posts, here and on other servers.",
    );
    ui.add_space(6.0);
    if let Some((community, post)) = notices_list(ui, cs) {
        post_tap(cs, PostTap::Notice(community, post), &mut Out::default());
    }
}

/// The way into Activity from the chat list, while there is any. Answers
/// whether it was tapped.
pub fn activity_entry(ui: &mut egui::Ui, cs: &mut Communities) -> bool {
    let Some((label, new)) = activity_label(cs) else {
        return false;
    };
    theme::chip(ui, &format!("{}  {label}", icons::STAR), new).clicked()
}

/// What the way into Activity says, and whether anything in it is new;
/// `None` while there is nothing to show. Reads the notices again when they
/// are due, so call it only while the list shows (both editions do).
pub fn activity_label(cs: &mut Communities) -> Option<(String, bool)> {
    cs.posts.refresh_notices(false);
    let n = cs.posts.notices.as_ref().filter(|n| !n.items.is_empty())?;
    Some(if n.unread > 0 {
        (format!("Activity · {} new", n.unread), true)
    } else {
        ("Activity on your posts".to_string(), false)
    })
}
