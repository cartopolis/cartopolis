//! Communities in the Chats panel (`docs/notes/communities.md`, the design is
//! `docs/mockups/communities/` v1–v3).
//!
//! A community is a named set of people with a few topics, and being in it is
//! being in every topic. The topics themselves are ordinary private chats —
//! the library shows them as it shows any other — so this module holds only
//! what is around them: the communities in the list, the screens for finding,
//! starting, running and editing one, and the people in it with their roles.
//! The component library draws those screens (`lib_communities`); this is
//! their state and what their taps do ([`tap`]).
//!
//! Like the rest of the panel it never touches the engine. Everything it wants
//! done goes on [`Communities::ops`], which `private_chat::drive_private_chats`
//! drains into `cartopolis_chat::Engine`, and everything it shows is the
//! latest [`Communities::list`] the server pushed.

use bevy::prelude::*;
use bevy::tasks::IoTaskPool;
use bevy::tasks::Task;
use cartopolis_core::{
    CommunityDto, CommunityListing, CommunityPlace, CommunityRole, CommunitySettings,
    DirectoryOrder, JoinRule, ListingState, TopicDto,
};
use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

use crate::systems::chat::ChatState;
use crate::systems::private_chat::{channel_key, group_of};

/// Something for the engine to do, queued by the panel.
#[derive(Debug, Clone)]
pub enum CommunityOp {
    Create {
        settings: CommunitySettings,
        topics: Vec<String>,
        people: Vec<String>,
    },
    Edit(Uuid, CommunitySettings),
    /// Search the directory; the point is where the person is, for Near you.
    Find(String, DirectoryOrder, Option<(f64, f64)>),
    CreateInvite(Uuid),
    LookupInvite(String),
    JoinInvite(String),
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
    /// An invite link was opened: the community it names, and Join.
    Invite(String),
    /// "Post this view" from the map: which community to post it in.
    PostTo,
    /// Replies, likes and followers on this account's public posts.
    Activity,
}

/// A form being filled in: starting or editing a community.
#[derive(Debug, Clone)]
pub struct Draft {
    pub name: String,
    pub about: String,
    pub join: JoinRule,
    pub listed: bool,
    pub members_start_topics: bool,
    /// The picture's hash in the picture store.
    pub picture: Option<String>,
    pub place: Option<CommunityPlace>,
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
            picture: None,
            place: None,
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
            picture: c.settings.picture.clone(),
            place: c.settings.place.clone(),
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
            picture: self.picture.clone(),
            place: self.place.clone(),
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
    pub(crate) draft: Draft,
    pub(crate) query: String,
    pub(crate) order: Option<DirectoryOrder>,
    /// What is typed into the community screen's fields.
    pub(crate) join_message: String,
    pub(crate) new_topic: String,
    pub(crate) add_people: String,
    /// Which of the community screen's two forms is open.
    pub(crate) adding: bool,
    pub(crate) starting_topic: bool,
    /// A moderator has opened the join requests and reports, which the
    /// page otherwise folds into one row above the posts.
    pub(crate) reviewing: bool,
    admin: AdminInbox,
    /// Where the person is looking, for Near you and for pinning a place.
    pub here: Option<(f64, f64)>,
    /// Topics muted on this device, by channel key: no count, no badge. Kept
    /// in `user.json` (`sync_device`), because muting is a choice about this
    /// screen and nobody else is told.
    pub muted: BTreeSet<String>,
    muted_loaded: bool,
    /// The last invite link a moderator made here, for the community it is to.
    pub invite: Option<(Uuid, String)>,
    /// A code or link typed into Find.
    pub(crate) invite_input: String,
    /// An invite this launch was opened with, until the session is up.
    pub pending_invite: Option<String>,
    /// The invite being looked up was refused: expired, or never was.
    pub invite_refused: bool,
    /// The Edit screen asked for a picture to be picked (`chats` starts it).
    pub pick_picture: bool,
    pub capture: Option<crate::platform::photo::Capture>,
    pub(crate) upload: Option<Task<Result<String, String>>>,
    /// Something went wrong with the picture, in words.
    pub picture_note: Option<String>,
    /// Seeded for a scripted capture (`seed_demo`), to be cleared after.
    demo: bool,
    /// The public half of each community (`community_posts`).
    pub posts: crate::systems::community_posts::Posts,
    /// The picture being picked is for a post, not the community's face: kept
    /// full size, and handed to the post being written.
    pub picture_for_post: bool,
    /// Pictures from the picture store as the library's screens draw them:
    /// a community's in its disc, a post's photo and polaroid
    /// ([`Communities::picture`]).
    #[cfg(feature = "ui-lib")]
    thumbs: crate::systems::thumbs::Thumbs,
}

impl Communities {
    /// A picture from the picture store (a community's, a post's photo or
    /// polaroid) as a thumbnail cut to `fit`, fetching it the first time it
    /// is asked for: `None` while it is on its way, or when it did not come.
    /// Not encrypted, unlike a chat's: these pictures are public.
    #[cfg(feature = "ui-lib")]
    pub fn picture(
        &mut self,
        hash: Option<&str>,
        fit: crate::systems::thumbs::Fit,
    ) -> Option<crate::systems::thumbs::Thumb> {
        use crate::systems::thumbs::Start;
        let hash = hash?;
        self.thumbs.get(hash, fit, || {
            // A scripted capture's picture is drawn here, never fetched.
            if let Some(demo) = crate::systems::chat_pictures::demo_jpeg_for(hash) {
                return match demo {
                    Ok(jpeg) => Start::Bytes(jpeg),
                    Err(why) => Start::Fetch(IoTaskPool::get().spawn(async move { Err(why) })),
                };
            }
            let h = hash.to_string();
            Start::Fetch(
                IoTaskPool::get()
                    .spawn(async move { crate::systems::model_assets::fetch_picture(&h).await }),
            )
        })
    }

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

    /// Seeded for a scripted capture, with no session behind it.
    #[must_use]
    pub fn is_demo(&self) -> bool {
        self.demo
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
        self.ops
            .push(CommunityOp::Find(self.query.clone(), order, self.here));
    }

    /// Open the form for starting one.
    pub fn start(&mut self) {
        self.draft = Draft::default();
        self.screen = Some(Screen::Create);
    }

    /// Whether a topic is muted on this device.
    #[must_use]
    pub fn is_muted(&self, key: &str) -> bool {
        self.muted.contains(key)
    }

    pub fn set_muted(&mut self, key: &str, muted: bool) {
        if muted {
            self.muted.insert(key.to_string());
        } else {
            self.muted.remove(key);
        }
    }

    /// Open the community an invite code or link names.
    pub fn open_invite(&mut self, code: String) {
        self.directory.clear();
        self.invite_refused = false;
        self.ops.push(CommunityOp::LookupInvite(code.clone()));
        self.screen = Some(Screen::Invite(code));
    }

    /// A picked picture: shrink it to a square thumbnail and store it. Not
    /// encrypted, unlike a chat's pictures: a community's picture is its face
    /// in the directory, shown to anybody who finds it.
    pub fn upload_picture(&mut self, bytes: Vec<u8>) {
        self.picture_note = Some("Uploading the picture…".into());
        // A post's picture is shown at the width of the feed, so it keeps its
        // full size; a community's face is a thumbnail.
        let full = self.picture_for_post;
        self.upload = Some(IoTaskPool::get().spawn(async move {
            let prepared = crate::utils::photo::prepare(&bytes)?;
            let jpeg = if full { prepared.full } else { prepared.thumb };
            crate::systems::model_assets::api_upload_picture(jpeg).await
        }));
    }

    /// Whether no picture is on its way up.
    #[must_use]
    pub fn picture_for_post_idle(&self) -> bool {
        self.upload.is_none()
    }

    /// Fold in a finished upload.
    pub fn poll_pictures(&mut self) {
        if let Some(task) = self.upload.as_mut() {
            if let Some(result) = crate::platform::http::poll_task(task) {
                self.upload = None;
                match result {
                    Ok(hash) if self.picture_for_post => {
                        self.picture_for_post = false;
                        self.picture_note = None;
                        if let Some(d) = self.posts.draft.as_mut() {
                            d.picture = Some(hash);
                        }
                    }
                    Ok(hash) => {
                        self.draft.picture = Some(hash);
                        self.picture_note = None;
                    }
                    Err(why) => self.picture_note = Some(why),
                }
            }
        }
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

/// Keep the panel's view of this device current: where the person is looking
/// (for Near you and for pinning a place), the muted topics in `user.json`,
/// and an invite link the app was launched with.
pub fn sync_device(
    mut cs: ResMut<Communities>,
    mut store: ResMut<crate::systems::user_store::UserStore>,
    map_anchor: Res<crate::resources::MapAnchor>,
    globe: Res<crate::systems::globe::GlobeState>,
    cameras: crate::grid::AnchoredCamera,
    online: Option<Res<crate::resources::OnlineSession>>,
    mut new_link: ResMut<crate::systems::place_link::NewLink>,
    mut launched: Local<bool>,
) {
    if !*launched {
        *launched = true;
        cs.pending_invite = launch_invite();
    }
    // A link tapped while the app already runs (`place_link::take_new_link`):
    // shown over whatever screen was open, because the person just asked for it.
    if let Some(code) = new_link.invite.take() {
        cs.screen = None;
        cs.pending_invite = Some(code);
    }
    if let (Some(anchor), Some((cell, tf))) = (map_anchor.geo, cameras.get()) {
        cs.here = Some(crate::systems::geo_nav::camera_geo(
            anchor, &globe, cell, tf,
        ));
    }
    if !cs.muted_loaded {
        cs.muted_loaded = true;
        cs.muted = store.data.muted_topics.iter().cloned().collect();
    } else if !cs.demo {
        let now: Vec<String> = cs.muted.iter().cloned().collect();
        if store.data.muted_topics != now {
            store.data.muted_topics = now;
            store.dirty = true;
        }
    }
    if online.is_some() && cs.screen.is_none() {
        if let Some(code) = cs.pending_invite.take() {
            cs.open_invite(code);
        }
    }
    if cs.upload.is_some() {
        cs.poll_pictures();
    }
}

/// The invite a launch was opened with, if any: the Android intent, or
/// `CARTO_COMMUNITY` (`?community=` on the web).
fn launch_invite() -> Option<String> {
    #[cfg(target_os = "android")]
    if let Some(url) = crate::platform::android::launch_url() {
        if let Some(code) = crate::systems::place_link::invite_from_link(&url) {
            return Some(code);
        }
    }
    crate::systems::place_link::invite_from_link(&crate::env_value("CARTO_COMMUNITY")?)
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
    /// Open this place on the map: a public post's view.
    pub go_to: Option<cartopolis_core::point::Point>,
    /// Close the panel and go back to the map, as World does.
    pub close: bool,
}

/// A tap on a community screen that is not typing, from the component
/// library's screens (`lib_communities`), done by [`tap`].
#[derive(Debug, Clone)]
pub enum Tap {
    /// Back: one screen up, as each screen's back goes ([`tap`] says where).
    Back,
    /// The ✕: back to the map, as World does.
    #[cfg_attr(not(feature = "ui-lib"), allow(dead_code))]
    Close,
    /// Ask the directory again, in this order.
    Find(DirectoryOrder),
    /// A listing in Find: its page when this account is in it, else its
    /// preview.
    Listing(Uuid, ListingState),
    /// Open the code or link typed under "Have an invite?".
    OpenTypedInvite,
    /// A community's page.
    Show(Uuid),
    /// Join through the invite being looked at.
    JoinInvite(String, Uuid),
    /// Join an open community.
    Join(Uuid),
    /// Ask to join, with the line typed for the moderators.
    AskToJoin(Uuid),
    /// Take a request to join back; `true` goes back to Find.
    Withdraw(Uuid, bool),
    /// Message this person.
    Message(String),
    /// The form's picture: pick one (`chats` starts the picker), or drop it.
    ChoosePicture,
    RemovePicture,
    /// The form's place: where the map is, or none.
    PinPlace,
    RemovePlace,
    /// Start the community the form describes, once it has a name.
    Create,
    /// The community page's two forms: Invite and New topic.
    ToggleAdding,
    ToggleTopic,
    People(Uuid),
    /// The Edit form, filled from the community as it is.
    Edit(Uuid),
    /// Add the people typed into Invite.
    AddPeople(Uuid),
    /// Put an invite link on the clipboard.
    CopyInvite(String),
    /// Start the topic typed into New topic.
    StartTopic(Uuid),
    /// Open or fold a moderator's queue.
    ToggleReview,
    /// Open this topic's chat.
    Open(String),
    /// Mute (`true`) or unmute a topic on this device.
    Mute(String, bool),
    /// Save the Edit form.
    Save(Uuid),
    /// Leave the community, or (`Delete`) delete it with every topic.
    Leave(Uuid),
    Delete(Uuid),
    /// Work for the engine as it stands: a decision, a role, a removal, a
    /// report closed, a new invite link.
    Ops(Vec<CommunityOp>),
}

/// Whether the community form's picture is on its way (picked or uploading):
/// Choose and Remove wait.
#[must_use]
pub fn picture_busy(cs: &Communities) -> bool {
    cs.upload.is_some() || cs.capture.is_some()
}

/// Whether the form for a new community can be sent: it has a name.
#[must_use]
pub fn create_ready(cs: &Communities) -> bool {
    !cs.draft.name.trim().is_empty()
}

/// Do a tap on a community screen.
#[allow(clippy::too_many_lines)]
pub fn tap(cs: &mut Communities, t: Tap, out: &mut Out) {
    match t {
        Tap::Back => match cs.screen.clone() {
            Some(Screen::Preview(_)) => cs.screen = Some(Screen::Find),
            Some(Screen::Community(id)) if crate::systems::community_posts::composing(cs, id) => {
                crate::systems::community_posts::leave_compose_page(cs);
            }
            Some(Screen::Edit(id) | Screen::People(id)) => {
                if cs.get(id).is_some() {
                    cs.show(id);
                } else {
                    cs.screen = None;
                }
            }
            Some(Screen::PostTo) => {
                cs.posts.from_view = None;
                cs.posts.pending_picture = None;
                cs.screen = None;
            }
            _ => cs.screen = None,
        },
        Tap::Close => out.close = true,
        Tap::Find(order) => {
            cs.order = Some(order);
            cs.ops
                .push(CommunityOp::Find(cs.query.clone(), order, cs.here));
        }
        Tap::Listing(id, mine) => {
            cs.join_message.clear();
            cs.screen = Some(match mine {
                ListingState::Member => Screen::Community(id),
                _ => Screen::Preview(id),
            });
        }
        Tap::OpenTypedInvite => {
            if let Some(code) = crate::systems::place_link::invite_from_link(&cs.invite_input) {
                cs.invite_input.clear();
                cs.open_invite(code);
            }
        }
        Tap::Show(id) => cs.show(id),
        Tap::JoinInvite(code, id) => {
            cs.ops.push(CommunityOp::JoinInvite(code));
            cs.show(id);
        }
        Tap::Join(id) => {
            cs.ops.push(CommunityOp::Join(id, String::new()));
            cs.show(id);
        }
        Tap::AskToJoin(id) => {
            cs.ops
                .push(CommunityOp::Join(id, cs.join_message.trim().to_string()));
            cs.join_message.clear();
            cs.screen = None;
        }
        Tap::Withdraw(id, to_find) => {
            cs.ops.push(CommunityOp::Leave(id));
            cs.screen = to_find.then_some(Screen::Find);
        }
        Tap::Message(who) => out.message = Some(who),
        Tap::ChoosePicture => {
            if crate::platform::photo::is_available() && !picture_busy(cs) {
                cs.picture_note = None;
                cs.picture_for_post = false;
                cs.pick_picture = true;
            }
        }
        Tap::RemovePicture => {
            if !picture_busy(cs) {
                cs.draft.picture = None;
            }
        }
        Tap::PinPlace => {
            if let Some((latitude, longitude)) = cs.here {
                cs.draft.place = Some(CommunityPlace {
                    latitude,
                    longitude,
                    name: String::new(),
                });
            }
        }
        Tap::RemovePlace => cs.draft.place = None,
        Tap::Create => {
            if !create_ready(cs) {
                return;
            }
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
        Tap::ToggleAdding => {
            cs.adding = !cs.adding;
            cs.starting_topic = false;
        }
        Tap::ToggleTopic => {
            cs.starting_topic = !cs.starting_topic;
            cs.adding = false;
        }
        Tap::People(id) => cs.screen = Some(Screen::People(id)),
        Tap::Edit(id) => {
            if let Some(c) = cs.get(id).cloned() {
                cs.draft = Draft::of(&c);
                cs.screen = Some(Screen::Edit(id));
            }
        }
        Tap::AddPeople(id) => {
            for p in people(&cs.add_people) {
                cs.ops.push(CommunityOp::Add(id, p));
            }
            cs.add_people.clear();
            cs.adding = false;
        }
        Tap::CopyInvite(link) => crate::utils::clipboard::set_text(&link),
        Tap::StartTopic(id) => {
            if !cs.new_topic.trim().is_empty() {
                cs.ops
                    .push(CommunityOp::StartTopic(id, cs.new_topic.trim().to_string()));
                cs.new_topic.clear();
                cs.starting_topic = false;
            }
        }
        Tap::ToggleReview => cs.reviewing = !cs.reviewing,
        Tap::Open(key) => out.open = Some(key),
        Tap::Mute(key, muted) => cs.set_muted(&key, muted),
        Tap::Save(id) => {
            let Some(c) = cs.get(id).cloned() else {
                cs.screen = None;
                return;
            };
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
        Tap::Leave(id) => {
            cs.ops.push(CommunityOp::Leave(id));
            cs.screen = None;
        }
        Tap::Delete(id) => {
            cs.ops.push(CommunityOp::Delete(id));
            cs.screen = None;
        }
        Tap::Ops(ops) => cs.ops.extend(ops),
    }
}

pub(crate) fn role_word(r: CommunityRole) -> &'static str {
    match r {
        CommunityRole::Owner => "Owner",
        CommunityRole::Moderator => "Moderator",
        CommunityRole::Member => "Member",
    }
}

pub(crate) fn rule_word(r: JoinRule) -> &'static str {
    match r {
        JoinRule::Open => "Open",
        JoinRule::Ask => "Asks first",
        JoinRule::Invite => "Invite only",
    }
}

/// `name@server` as a person on this server reads it.
pub(crate) fn shown(address: &str, home: &str) -> String {
    cartopolis_core::address::display(address, home).to_string()
}

fn people(s: &str) -> Vec<String> {
    s.split([',', ' ', '\n'])
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(str::to_string)
        .collect()
}

// ── The communities in the Community list ─────────────────────────────────

/// One community as a row of the Community list: one row per community,
/// whose last line names the topic that spoke last
/// (`docs/mockups/home-and-profile/v1`, A). It was a row per community with a
/// row per topic under it, which put the whole membership of every community
/// on the list.
pub struct CommunityRow {
    pub id: Uuid,
    pub name: String,
    /// "# bankjes · Anna: de nieuwe bank staat er", or what the community is
    /// waiting for.
    pub preview: String,
    /// When the line in `preview` was said, for the list's newest-first order.
    pub at_ms: u64,
    /// Unread lines in its topics, plus requests to join for a moderator.
    pub unread: usize,
}

/// Every community this account is in or asked to join, as list rows.
/// `unread` counts a chat's unread lines.
pub fn list_rows(
    communities: &Communities,
    chat: &ChatState,
    unread: &dyn Fn(&str) -> usize,
) -> Vec<CommunityRow> {
    let mut out = Vec::new();
    for c in &communities.list {
        let joined: Vec<_> = c
            .topics
            .iter()
            .filter(|t| t.joined && !t.archived)
            .collect();
        let waiting: usize = joined
            .iter()
            .map(|t| channel_key(&t.group))
            .filter(|k| !communities.is_muted(k))
            .map(|k| unread(&k))
            .sum::<usize>()
            + c.requests.len();
        // The topic that spoke last, and what it said.
        let last = joined
            .iter()
            .filter_map(|t| {
                let key = channel_key(&t.group);
                let line = chat
                    .channels
                    .iter()
                    .find(|ch| ch.key == key)?
                    .lines
                    .iter()
                    .rev()
                    .find(|l| !chat.is_blocked(&l.sender))?
                    .clone();
                Some((t.name.clone(), line))
            })
            .max_by_key(|(_, l)| l.at_ms);
        let (preview, at_ms) = match (&c.role, last) {
            (None, _) => ("Waiting for a moderator".to_string(), 0),
            (Some(_), Some((topic, l))) => {
                let said = if l.removed {
                    "Message removed by a moderator".to_string()
                } else {
                    format!(
                        "{}: {}",
                        l.sender,
                        crate::systems::private_chat::readable(&l.body)
                    )
                };
                (format!("# {topic} · {said}"), l.at_ms)
            }
            (Some(_), None) => (
                format!(
                    "{} {} · {} {}",
                    c.members.len(),
                    if c.members.len() == 1 {
                        "person"
                    } else {
                        "people"
                    },
                    joined.len(),
                    if joined.len() == 1 { "topic" } else { "topics" },
                ),
                0,
            ),
        };
        out.push(CommunityRow {
            id: c.id,
            name: c.settings.name.clone(),
            preview,
            at_ms,
            unread: waiting,
        });
    }
    out
}

/// "350 m away", "4.2 km away".
pub(crate) fn distance_words(m: f64) -> String {
    if m < 1000.0 {
        format!("{} m away", (m / 10.0).round() * 10.0)
    } else if m < 10_000.0 {
        format!("{:.1} km away", m / 1000.0)
    } else {
        format!("{:.0} km away", m / 1000.0)
    }
}

/// "Grote Markt, Groningen" or the coordinates, when it was given no name.
pub(crate) fn place_words(p: &CommunityPlace) -> String {
    if p.name.trim().is_empty() {
        format!("{:.4}, {:.4}", p.latitude, p.longitude)
    } else {
        p.name.clone()
    }
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
                picture: None,
                place: Some(CommunityPlace {
                    latitude: 53.2194,
                    longitude: 6.5665,
                    name: "Grote Markt, Groningen".into(),
                }),
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
                picture: None,
                place: None,
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
                picture: None,
                place: None,
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
            picture: None,
            place: None,
            distance_m: None,
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
            picture: None,
            place: None,
            distance_m: Some(850.0),
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
            picture: None,
            place: None,
            distance_m: None,
        },
    ];
    cs.demo = true;
    // The public half (`community_posts`): the feed on the community page, a
    // post being written, or the posts pinned on the map with the panel shut.
    crate::systems::community_posts::seed_demo(&mut cs.posts, garage, screen);
    cs.screen = match screen {
        "community" | "communitytopics" | "communitypost" => Some(Screen::Community(garage)),
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Every community screen replaces the whole panel, the Community | World
    /// switch with it, so its ✕ is the only way back to the map. A page
    /// opened from a post's pin had a back chevron and nothing else. The ✕
    /// closes without going back; back goes one screen up.
    #[test]
    fn close_leaves_the_screen_and_back_goes_up() {
        let mut cs = Communities {
            screen: Some(Screen::Preview(Uuid::nil())),
            ..Default::default()
        };
        let mut out = Out::default();
        tap(&mut cs, Tap::Close, &mut out);
        assert!(out.close, "the ✕ did not ask the panel to close");
        assert_eq!(
            cs.screen,
            Some(Screen::Preview(Uuid::nil())),
            "the ✕ also went back"
        );
        tap(&mut cs, Tap::Back, &mut out);
        assert_eq!(cs.screen, Some(Screen::Find), "back from a preview is Find");
    }
}
