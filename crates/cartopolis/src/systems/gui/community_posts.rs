//! A community's public posts in the Chats panel (`docs/notes/fediverse.md`,
//! the design is `docs/mockups/communities/v4-public-posts.html`).
//!
//! A community page has two halves: **Posts**, public and on the fediverse, and
//! **Topics**, encrypted and members-only, as before. The two are told apart in
//! words and in colour everywhere ([`crate::systems::theme::PUBLIC`] against the topics'
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
    suggest_handle, EditPost, NewPost, NewReply, NewReport, PostDto, PostNotices, PostPlace,
    PostReport, PostThread, PublicInfo, PublicSettings, ReadNotices, ReportAction, SourceKind,
    Toggle, WhoPosts,
};
use cartopolis_core::CommunityDto;
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

use crate::systems::communities::{Communities, Out};
use crate::systems::model_assets::{api_delete, api_get, api_post};

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
    /// chip under the switch clears it (`drive_filter_chip`).
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
    /// The draft is written over the map (`drive_floating_composer`), not in
    /// the panel: it started from the map (Post here, the camera), and the
    /// place it is about should stay in sight.
    pub over_map: bool,
    /// The filter chip (`lib_filter_chip`) as [`drive_filter_chip`] last
    /// worked it out; `None` while hidden.
    pub chip_view: Option<ChipView>,
    /// The composer over the map (`lib_post_composer`) as
    /// [`drive_floating_composer`] last worked it out; `None` while hidden.
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

/// Do a tap on the filter chip (`lib_filter_chip`).
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

/// Do a tap in the composer (`lib_post_composer`, `lib_communities`).
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
        // A stable order, so a pin keeps its key from one frame to the next
        // whatever the map's iteration order.
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
/// Activity, Post to) that is not typing, from the component library
/// (`lib_communities`), done by [`post_tap`].
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

/// Do a tap in a community's public half (`lib_communities`).
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
///
/// This works out whether the chip shows and what it says, for the library's
/// chip (`lib_filter_chip`), which does its two taps itself through
/// [`chip_act`].
pub fn drive_filter_chip(
    mut cs: ResMut<Communities>,
    right_panel: Res<crate::resources::RightPanel>,
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
    if cs.posts.chip_view != shown {
        cs.posts.chip_view = shown;
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
///
/// This decides whether it shows and publishes where it stands and who the
/// post reaches, for the library's composer (`lib_post_composer`), which
/// edits the draft itself and does its taps through [`composer_act`].
pub fn drive_floating_composer(
    metrics: Res<crate::systems::ui_input::UiMetrics>,
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
    let viewport = metrics.viewport_pts();
    let sideways = viewport.x > viewport.y;
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
/// What the way into Activity says, and whether anything in it is new;
/// `None` while there is nothing to show. Reads the notices again when they
/// are due, so call it only while the list shows (`lib_chats` does).
pub fn activity_label(cs: &mut Communities) -> Option<(String, bool)> {
    cs.posts.refresh_notices(false);
    let n = cs.posts.notices.as_ref().filter(|n| !n.items.is_empty())?;
    Some(if n.unread > 0 {
        (format!("Activity · {} new", n.unread), true)
    } else {
        ("Activity on your posts".to_string(), false)
    })
}
